use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::net::TcpStream;

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
pub struct ConnectionPool {
    max_connections: usize,
    idle_timeout: Duration,
    connections: Mutex<HashMap<SocketAddr, VecDeque<PooledConnection>>>,
}

impl ConnectionPool {
    pub fn new(max_connections: usize, idle_timeout: Duration) -> Self {
        Self {
            max_connections,
            idle_timeout,
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Try to get a pooled TcpStream to the given address.
    ///
    /// Expired connections (idle longer than `idle_timeout`) are pruned
    /// before the lookup, so callers always receive a stream that was
    /// active within the timeout window.  Returns `None` when no
    /// reusable connection exists.
    pub fn get(&self, addr: SocketAddr) -> Option<TcpStream> {
        let mut conns = self.connections.lock();
        let now = Instant::now();

        if let Some(queue) = conns.get_mut(&addr) {
            // Remove expired connections from the front (oldest first)
            while let Some(front) = queue.front() {
                if now.duration_since(front.last_used) >= self.idle_timeout {
                    queue.pop_front();
                } else {
                    break;
                }
            }

            // Take the most recently used connection (back of the deque)
            // to maximize the chance the TCP connection is still alive.
            if let Some(pc) = queue.pop_back() {
                // Clean up empty queues
                if queue.is_empty() {
                    conns.remove(&addr);
                }
                return Some(pc.stream);
            }

            // Queue was empty after pruning expired entries
            conns.remove(&addr);
        }

        None
    }

    /// Return a TcpStream to the pool for future reuse.
    ///
    /// If the pool already holds `max_connections` total streams, the
    /// oldest connection (across all addresses) is evicted to make
    /// room.
    pub fn put(&self, addr: SocketAddr, stream: TcpStream) {
        let mut conns = self.connections.lock();

        // Enforce global max by evicting the oldest connection
        let total: usize = conns.values().map(|q| q.len()).sum();
        if total >= self.max_connections {
            Self::evict_oldest_locked(&mut conns);
        }

        let now = Instant::now();
        let pc = PooledConnection {
            stream,
            addr,
            created: now,
            last_used: now,
        };

        conns.entry(addr).or_default().push_back(pc);
    }

    /// Total number of pooled connections across all addresses.
    pub fn len(&self) -> usize {
        self.connections.lock().values().map(|q| q.len()).sum()
    }

    /// Returns true if there are no pooled connections.
    pub fn is_empty(&self) -> bool {
        self.connections.lock().values().all(|q| q.is_empty())
    }

    /// Remove all expired connections from every address.
    pub fn cleanup(&self) {
        let mut conns = self.connections.lock();
        let now = Instant::now();
        let timeout = self.idle_timeout;

        conns.retain(|_addr, queue| {
            queue.retain(|pc| now.duration_since(pc.last_used) < timeout);
            !queue.is_empty()
        });
    }

    /// Evict the single oldest connection across all addresses.
    /// Must be called while the lock is held.
    fn evict_oldest_locked(conns: &mut HashMap<SocketAddr, VecDeque<PooledConnection>>) {
        let mut oldest_addr: Option<SocketAddr> = None;
        let mut oldest_time: Option<Instant> = None;

        for (addr, queue) in conns.iter() {
            if let Some(front) = queue.front() {
                match oldest_time {
                    None => {
                        oldest_addr = Some(*addr);
                        oldest_time = Some(front.last_used);
                    }
                    Some(t) if front.last_used < t => {
                        oldest_addr = Some(*addr);
                        oldest_time = Some(front.last_used);
                    }
                    _ => {}
                }
            }
        }

        if let Some(addr) = oldest_addr {
            if let Some(queue) = conns.get_mut(&addr) {
                queue.pop_front();
                if queue.is_empty() {
                    conns.remove(&addr);
                }
            }
        }
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
