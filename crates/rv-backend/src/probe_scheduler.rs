use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tracing::{debug, error, info};

use crate::health::{HealthProbe, ProbeResult, ProbeStatus};

/// Manages background health probe tasks for all configured probes.
pub struct ProbeScheduler {
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl ProbeScheduler {
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Start a probe task for the given health probe.
    /// Returns a join handle for the spawned task.
    pub fn start_probe(&self, probe: Arc<HealthProbe>) -> tokio::task::JoinHandle<()> {
        let mut shutdown_rx = self.shutdown_rx.clone();
        let interval = probe.interval;

        tokio::spawn(async move {
            info!(
                target = %probe.target(),
                url = %probe.url,
                interval = ?interval,
                "health probe started"
            );

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        let result = execute_probe(&probe).await;
                        debug!(
                            target = %probe.target(),
                            status = ?result.status,
                            response_time = ?result.response_time,
                            "probe result"
                        );
                        probe.record_result(&result);
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!(target = %probe.target(), "health probe stopping");
                            return;
                        }
                    }
                }
            }
        })
    }

    /// Start probe tasks for multiple probes.
    pub fn start_probes(&self, probes: Vec<Arc<HealthProbe>>) -> Vec<tokio::task::JoinHandle<()>> {
        probes
            .into_iter()
            .map(|probe| self.start_probe(probe))
            .collect()
    }

    /// Signal all probe tasks to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl Default for ProbeScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a single health probe against the backend.
async fn execute_probe(probe: &HealthProbe) -> ProbeResult {
    let start = std::time::Instant::now();
    let addr = probe.target();
    let timeout = probe.timeout;

    match tokio::time::timeout(timeout, probe_backend(addr, &probe.url)).await {
        Ok(Ok(status_code)) => {
            let response_time = start.elapsed();
            let is_healthy = (200..400).contains(&status_code);
            ProbeResult {
                status: if is_healthy {
                    ProbeStatus::Healthy
                } else {
                    ProbeStatus::Sick
                },
                response_time,
                http_status: Some(status_code),
            }
        }
        Ok(Err(e)) => {
            error!(target = %addr, error = %e, "probe connection error");
            ProbeResult {
                status: ProbeStatus::Sick,
                response_time: start.elapsed(),
                http_status: None,
            }
        }
        Err(_) => {
            error!(target = %addr, "probe timed out");
            ProbeResult {
                status: ProbeStatus::Sick,
                response_time: start.elapsed(),
                http_status: None,
            }
        }
    }
}

/// Send a minimal HTTP/1.1 GET request and read the status code.
async fn probe_backend(addr: std::net::SocketAddr, url: &str) -> Result<u16, std::io::Error> {
    let mut stream = TcpStream::connect(addr).await?;

    let request = format!("GET {url} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    // Read just the status line
    let mut buf = vec![0u8; 256];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "empty response",
        ));
    }

    let response = String::from_utf8_lossy(&buf[..n]);
    // Parse status line: "HTTP/1.1 200 OK"
    let status_line = response.lines().next().unwrap_or("");
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() >= 2 {
        parts[1]
            .parse::<u16>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid status line",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_probe(port: u16) -> Arc<HealthProbe> {
        Arc::new(HealthProbe::new(
            format!("127.0.0.1:{port}").parse().unwrap(),
            "/health",
            Duration::from_millis(100),
            Duration::from_millis(50),
            2,
            3,
        ))
    }

    #[test]
    fn test_scheduler_creation() {
        let scheduler = ProbeScheduler::new();
        // Verify it can be created and shutdown without panicking
        scheduler.shutdown();
    }

    #[tokio::test]
    async fn test_probe_timeout() {
        // Use a port where nothing is listening
        let probe = make_probe(19999);
        let result = execute_probe(&probe).await;
        assert_eq!(result.status, ProbeStatus::Sick);
        assert!(result.http_status.is_none());
    }

    #[tokio::test]
    async fn test_scheduler_shutdown() {
        let scheduler = ProbeScheduler::new();
        let probe = make_probe(19998);

        let handle = scheduler.start_probe(probe);

        // Give it a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Shutdown should cause the task to finish
        scheduler.shutdown();
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "probe task should have stopped");
    }
}
