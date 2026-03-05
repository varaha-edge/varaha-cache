//! DNS-based director.
//!
//! `DnsDirector` resolves a hostname to one or more IP addresses, creates a
//! [`SimpleBackend`] for each resolved address, and delegates backend selection
//! to an inner [`Director`] (typically round-robin or random). A background
//! tokio task periodically re-resolves the hostname so that DNS changes (e.g.
//! from service discovery or cloud load balancers) are picked up automatically.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tracing::{debug, warn};

use crate::director::RoundRobinDirector;
use crate::simple::SimpleBackend;
use crate::traits::{AdminHealth, Backend, Director, DirectorListEntry};

/// A director that resolves a hostname via DNS and maintains a set of backends
/// corresponding to the resolved addresses.
///
/// On each resolution cycle the director replaces its backend list with fresh
/// [`SimpleBackend`] instances built from the DNS results and rebuilds the
/// inner sub-director.
pub struct DnsDirector {
    name: String,
    hostname: String,
    port: u16,
    backends: RwLock<Vec<Arc<SimpleBackend>>>,
    resolve_interval: Duration,
    /// The sub-director that performs the actual backend selection once the
    /// backend list is populated. Wrapped in an `RwLock` so that resolution
    /// can swap it atomically.
    sub_director: RwLock<Arc<dyn Director>>,
}

impl DnsDirector {
    /// Creates a new `DnsDirector`.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for this director.
    /// * `hostname` - The hostname to resolve (e.g. `"backend.example.com"`).
    /// * `port` - The port number to use for each resolved backend.
    /// * `resolve_interval` - How often to re-resolve the hostname.
    pub fn new(
        name: impl Into<String>,
        hostname: impl Into<String>,
        port: u16,
        resolve_interval: Duration,
    ) -> Self {
        let name = name.into();
        let sub = Arc::new(RoundRobinDirector::new(format!("{name}.rr")));
        Self {
            name,
            hostname: hostname.into(),
            port,
            backends: RwLock::new(Vec::new()),
            resolve_interval,
            sub_director: RwLock::new(sub),
        }
    }

    /// Performs a single DNS resolution of the configured hostname and updates
    /// the backend list and sub-director accordingly.
    ///
    /// Resolution uses [`tokio::net::lookup_host`] which performs an async
    /// getaddrinfo(3) call under the hood.
    pub async fn resolve(&self) {
        let lookup = format!("{}:{}", self.hostname, self.port);
        match tokio::net::lookup_host(&lookup).await {
            Ok(addrs) => {
                let addrs: Vec<SocketAddr> = addrs.collect();
                if addrs.is_empty() {
                    warn!(
                        director = %self.name,
                        hostname = %self.hostname,
                        "DNS resolution returned zero addresses"
                    );
                    return;
                }

                let new_rr = Arc::new(RoundRobinDirector::new(format!("{}.rr", self.name)));
                let mut new_backends = Vec::with_capacity(addrs.len());

                for (i, addr) in addrs.iter().enumerate() {
                    let be = Arc::new(SimpleBackend::new(format!("{}.{i}", self.name), *addr));
                    new_rr.add_backend(Arc::clone(&be) as Arc<dyn Backend>);
                    new_backends.push(be);
                }

                debug!(
                    director = %self.name,
                    hostname = %self.hostname,
                    count = new_backends.len(),
                    "DNS resolved backends"
                );

                *self.backends.write() = new_backends;
                *self.sub_director.write() = new_rr;
            }
            Err(e) => {
                warn!(
                    director = %self.name,
                    hostname = %self.hostname,
                    error = %e,
                    "DNS resolution failed, keeping existing backends"
                );
            }
        }
    }

    /// Spawns a background tokio task that periodically re-resolves the
    /// hostname at the configured interval. Also performs an initial
    /// resolution before entering the loop.
    ///
    /// Returns a `tokio::task::JoinHandle` that the caller can use to
    /// cancel or await the background task.
    pub fn start_resolver(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            // Perform an initial resolution immediately.
            this.resolve().await;

            let mut interval = tokio::time::interval(this.resolve_interval);
            // The first tick completes immediately; skip it since we already
            // resolved above.
            interval.tick().await;

            loop {
                interval.tick().await;
                this.resolve().await;
            }
        })
    }

    /// Returns the currently resolved backends.
    pub fn backends(&self) -> Vec<Arc<SimpleBackend>> {
        self.backends.read().clone()
    }

    /// Returns the configured hostname.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Returns the configured port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns the configured resolve interval.
    pub fn resolve_interval(&self) -> Duration {
        self.resolve_interval
    }
}

impl Director for DnsDirector {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        self.sub_director.read().resolve()
    }

    fn healthy(&self) -> bool {
        self.sub_director.read().healthy()
    }

    fn admin_health(&self) -> AdminHealth {
        AdminHealth::Probe
    }

    fn list(&self) -> Vec<DirectorListEntry> {
        self.sub_director.read().list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dns_director_new() {
        let dir = DnsDirector::new("dns-test", "localhost", 8080, Duration::from_secs(30));
        assert_eq!(dir.name(), "dns-test");
        assert_eq!(dir.hostname(), "localhost");
        assert_eq!(dir.port(), 8080);
        assert_eq!(dir.resolve_interval(), Duration::from_secs(30));
    }

    #[test]
    fn test_dns_director_empty_initially() {
        let dir = DnsDirector::new("dns-test", "localhost", 8080, Duration::from_secs(30));
        assert!(dir.backends().is_empty());
        // With no backends, resolve returns None.
        assert!(Director::resolve(&dir).is_none());
        assert!(!dir.healthy());
    }

    #[tokio::test]
    async fn test_dns_director_resolve_localhost() {
        let dir = DnsDirector::new("dns-local", "localhost", 9999, Duration::from_secs(60));
        dir.resolve().await;

        let backends = dir.backends();
        // localhost should resolve to at least one address (127.0.0.1 or ::1).
        assert!(
            !backends.is_empty(),
            "localhost should resolve to at least one address"
        );

        // All backends should be on port 9999.
        for be in &backends {
            assert_eq!(be.addr().port(), 9999);
        }

        // The director should now report healthy (SimpleBackend defaults to
        // healthy when no probe is configured).
        assert!(Director::resolve(&dir).is_some());
        assert!(dir.healthy());
    }

    #[tokio::test]
    async fn test_dns_director_resolve_bad_host() {
        let dir = DnsDirector::new(
            "dns-bad",
            "this.host.definitely.does.not.exist.invalid",
            80,
            Duration::from_secs(60),
        );
        dir.resolve().await;

        // Resolution should fail gracefully -- no backends added.
        assert!(dir.backends().is_empty());
        assert!(Director::resolve(&dir).is_none());
    }

    #[tokio::test]
    async fn test_dns_director_list() {
        let dir = DnsDirector::new("dns-list", "localhost", 7777, Duration::from_secs(60));
        dir.resolve().await;

        let list = dir.list();
        assert!(!list.is_empty());
        for entry in &list {
            assert!(entry.is_healthy);
        }
    }
}
