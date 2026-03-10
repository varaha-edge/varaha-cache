use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// A pooled connection entry holding a real TcpStream.
pub struct PooledConnection {
    /// The live TCP stream that can be reused.
    pub stream: TcpStream,
    /// The remote address this stream is connected to.
    pub addr: SocketAddr,
    /// When this connection was originally established.
    pub created: Instant,
    /// When this connection was last used (returned to the pool).
    pub last_used: Instant,
}

impl std::fmt::Debug for PooledConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConnection")
            .field("addr", &self.addr)
            .field("created", &self.created)
            .field("last_used", &self.last_used)
            .finish()
    }
}

/// Connection pool for backend connections.
/// Based on cache_conn_pool.c -- pools real TcpStream instances
/// keyed by remote SocketAddr for connection reuse on keep-alive
/// backends.
///
/// Uses per-address sharding via DashMap for concurrent access
/// without a single global lock.
pub struct ConnectionPool {
    max_connections: usize,
    idle_timeout: Duration,
    connections: DashMap<SocketAddr, VecDeque<PooledConnection>>,
    total: AtomicUsize,
}

impl ConnectionPool {
    pub fn new(max_connections: usize, idle_timeout: Duration) -> Self {
        Self {
            max_connections,
            idle_timeout,
            connections: DashMap::new(),
            total: AtomicUsize::new(0),
        }
    }

    /// Try to get a pooled TcpStream to the given address.
    ///
    /// Expired connections (idle longer than `idle_timeout`) are pruned
    /// before the lookup, so callers always receive a stream that was
    /// active within the timeout window.  Returns `None` when no
    /// reusable connection exists.
    pub fn get(&self, addr: SocketAddr) -> Option<TcpStream> {
        let mut entry = self.connections.get_mut(&addr)?;
        let queue = entry.value_mut();
        let now = Instant::now();

        // Remove expired connections from the front (oldest first)
        let mut expired_count = 0usize;
        while let Some(front) = queue.front() {
            if now.duration_since(front.last_used) >= self.idle_timeout {
                queue.pop_front();
                expired_count += 1;
            } else {
                break;
            }
        }

        if expired_count > 0 {
            self.total.fetch_sub(expired_count, Ordering::Relaxed);
        }

        // Take the most recently used connection (back of the deque)
        // to maximize the chance the TCP connection is still alive.
        if let Some(pc) = queue.pop_back() {
            self.total.fetch_sub(1, Ordering::Relaxed);
            let stream = pc.stream;

            // Clean up empty queues - drop the mutable ref first
            if queue.is_empty() {
                drop(entry);
                self.connections.remove(&addr);
            }

            return Some(stream);
        }

        // Queue was empty after pruning expired entries
        drop(entry);
        self.connections.remove(&addr);

        None
    }

    /// Return a TcpStream to the pool for future reuse.
    ///
    /// If the pool already holds `max_connections` total streams, the
    /// oldest connection from this address's queue is evicted to make
    /// room. If the address queue is empty, a connection from any
    /// address is evicted.
    pub fn put(&self, addr: SocketAddr, stream: TcpStream) {
        let current_total = self.total.load(Ordering::Relaxed);
        if current_total >= self.max_connections {
            self.evict_one();
        }

        let now = Instant::now();
        let pc = PooledConnection {
            stream,
            addr,
            created: now,
            last_used: now,
        };

        self.connections.entry(addr).or_default().push_back(pc);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// Total number of pooled connections across all addresses.
    pub fn len(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// Returns true if there are no pooled connections.
    pub fn is_empty(&self) -> bool {
        self.total.load(Ordering::Relaxed) == 0
    }

    /// Remove all expired connections from every address.
    pub fn cleanup(&self) {
        let now = Instant::now();
        let timeout = self.idle_timeout;
        let mut removed = 0usize;
        let mut empty_keys = Vec::new();

        for mut entry in self.connections.iter_mut() {
            let queue = entry.value_mut();
            let before = queue.len();
            queue.retain(|pc| now.duration_since(pc.last_used) < timeout);
            let after = queue.len();
            removed += before - after;
            if after == 0 {
                empty_keys.push(*entry.key());
            }
        }

        // Remove empty address entries outside the iteration
        for key in empty_keys {
            // Re-check emptiness under the shard lock to avoid races
            self.connections.remove_if(&key, |_k, q| q.is_empty());
        }

        if removed > 0 {
            self.total.fetch_sub(removed, Ordering::Relaxed);
            debug!(removed, "connection pool cleanup removed expired entries");
        }
    }

    /// Evict a single connection to make room for a new one.
    /// Tries to find the oldest connection across all addresses.
    fn evict_one(&self) {
        let mut oldest_addr: Option<SocketAddr> = None;
        let mut oldest_time: Option<Instant> = None;

        // First pass: find the address with the oldest front entry
        for entry in self.connections.iter() {
            if let Some(front) = entry.value().front() {
                let dominated = match oldest_time {
                    None => true,
                    Some(t) => front.last_used < t,
                };
                if dominated {
                    oldest_addr = Some(*entry.key());
                    oldest_time = Some(front.last_used);
                }
            }
        }

        // Second pass: evict from the identified address
        if let Some(addr) = oldest_addr {
            if let Some(mut entry) = self.connections.get_mut(&addr) {
                let queue = entry.value_mut();
                if queue.pop_front().is_some() {
                    self.total.fetch_sub(1, Ordering::Relaxed);
                    if queue.is_empty() {
                        drop(entry);
                        self.connections.remove(&addr);
                    }
                }
            }
        }
    }

    /// Spawn a background task that runs `cleanup()` every 30 seconds.
    ///
    /// Returns a `CancellationToken` that can be used to stop the task.
    /// The pool must be wrapped in an `Arc` for this to work.
    pub fn spawn_cleanup_task(self: &Arc<Self>) -> CancellationToken {
        let token = CancellationToken::new();
        let pool = Arc::clone(self);
        let cancel = token.clone();

        tokio::spawn(async move {
            let interval = Duration::from_secs(30);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("connection pool cleanup task cancelled");
                        break;
                    }
                    _ = tokio::time::sleep(interval) => {
                        pool.cleanup();
                    }
                }
            }
        });

        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_put_get() {
        // Create a real TCP listener so we can establish actual connections
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = ConnectionPool::new(10, Duration::from_secs(60));

        // No connections pooled yet
        assert!(pool.get(addr).is_none());

        // Connect and pool the stream
        let stream = TcpStream::connect(addr).await.unwrap();
        let _accepted = listener.accept().await.unwrap();

        pool.put(addr, stream);
        assert_eq!(pool.len(), 1);

        // Retrieve it
        let retrieved = pool.get(addr);
        assert!(retrieved.is_some());
        assert_eq!(pool.len(), 0); // Taken from pool
    }

    #[tokio::test]
    async fn test_pool_max_connections() {
        let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr1 = listener1.local_addr().unwrap();

        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();

        let listener3 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr3 = listener3.local_addr().unwrap();

        let pool = ConnectionPool::new(2, Duration::from_secs(60));

        let s1 = TcpStream::connect(addr1).await.unwrap();
        let _a1 = listener1.accept().await.unwrap();
        pool.put(addr1, s1);

        let s2 = TcpStream::connect(addr2).await.unwrap();
        let _a2 = listener2.accept().await.unwrap();
        pool.put(addr2, s2);

        let s3 = TcpStream::connect(addr3).await.unwrap();
        let _a3 = listener3.accept().await.unwrap();
        pool.put(addr3, s3);

        assert_eq!(pool.len(), 2); // Max 2, oldest evicted
    }

    #[tokio::test]
    async fn test_pool_multiple_connections_same_addr() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let pool = ConnectionPool::new(10, Duration::from_secs(60));

        let s1 = TcpStream::connect(addr).await.unwrap();
        let _a1 = listener.accept().await.unwrap();
        pool.put(addr, s1);

        let s2 = TcpStream::connect(addr).await.unwrap();
        let _a2 = listener.accept().await.unwrap();
        pool.put(addr, s2);

        assert_eq!(pool.len(), 2);

        // Get returns the most recent (back of deque)
        assert!(pool.get(addr).is_some());
        assert_eq!(pool.len(), 1);

        assert!(pool.get(addr).is_some());
        assert_eq!(pool.len(), 0);

        assert!(pool.get(addr).is_none());
    }

    #[tokio::test]
    async fn test_pool_cleanup_removes_expired() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Use a very short idle timeout
        let pool = ConnectionPool::new(10, Duration::from_millis(1));

        let s1 = TcpStream::connect(addr).await.unwrap();
        let _a1 = listener.accept().await.unwrap();
        pool.put(addr, s1);

        assert_eq!(pool.len(), 1);

        // Wait for the connection to expire
        std::thread::sleep(Duration::from_millis(10));

        pool.cleanup();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_pool_is_empty_initially() {
        let pool = ConnectionPool::new(10, Duration::from_secs(60));
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }
}
