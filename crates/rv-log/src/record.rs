//! Log record definition for the structured ring-buffer log.
//!
//! Each LogRecord represents a single VSL-style log entry, tagged with a LogTag,
//! associated with a transaction via Vxid, and timestamped with wall-clock time.

use rv_types::vsl::Vxid;
use rv_types::{LogTag, VtimReal};

/// A single structured log record, modeled after a VSL log entry.
///
/// Records are immutable once created and stored in the ring buffer.
/// They carry a tag for categorization, a transaction ID for correlation,
/// a wall-clock timestamp, and a freeform data payload.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// The VSL tag categorizing this log entry (e.g., ReqStart, RespStatus, Debug).
    pub tag: LogTag,
    /// The transaction ID this record belongs to. Zero indicates no association.
    pub vxid: Vxid,
    /// Wall-clock timestamp when this record was created.
    pub timestamp: VtimReal,
    /// The freeform log data payload.
    pub data: String,
}

impl LogRecord {
    /// Create a new log record with the current wall-clock time.
    pub fn new(tag: LogTag, vxid: Vxid, data: String) -> Self {
        Self {
            tag,
            vxid,
            timestamp: VtimReal::now(),
            data,
        }
    }

    /// Create a new log record with an explicit timestamp.
    pub fn with_timestamp(tag: LogTag, vxid: Vxid, timestamp: VtimReal, data: String) -> Self {
        Self {
            tag,
            vxid,
            timestamp,
            data,
        }
    }
}

impl std::fmt::Display for LogRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:<12} {:>8} {:.6} {}",
            self.tag.name(),
            self.vxid,
            self.timestamp.as_secs(),
            self.data,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_record() {
        let rec = LogRecord::new(LogTag::Debug, Vxid::new(42), "test message".to_string());
        assert_eq!(rec.tag, LogTag::Debug);
        assert_eq!(rec.vxid, Vxid::new(42));
        assert!(!rec.data.is_empty());
    }

    #[test]
    fn create_record_with_timestamp() {
        let ts = VtimReal::from_secs(1_700_000_000.0);
        let rec =
            LogRecord::with_timestamp(LogTag::ReqStart, Vxid::new(1), ts, "/index.html".into());
        assert_eq!(rec.timestamp, ts);
        assert_eq!(rec.data, "/index.html");
    }

    #[test]
    fn display_format() {
        let ts = VtimReal::from_secs(1_700_000_000.123456);
        let rec = LogRecord::with_timestamp(LogTag::ReqURL, Vxid::new(100), ts, "/foo".into());
        let formatted = format!("{}", rec);
        assert!(formatted.contains("ReqURL"));
        assert!(formatted.contains("100"));
        assert!(formatted.contains("/foo"));
    }
}
