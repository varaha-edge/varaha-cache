//! Log writer for producing structured log entries.
//!
//! LogWriter wraps a shared reference to the ring buffer and provides
//! convenient methods for writing tagged log records. It also integrates
//! with the `tracing` crate so that log entries are emitted both to the
//! ring buffer and to any configured tracing subscriber.

use std::sync::Arc;

use rv_types::LogTag;
use rv_types::vsl::Vxid;

use crate::record::LogRecord;
use crate::ringbuf::RingBuffer;

/// A writer handle for producing log entries into a shared ring buffer.
///
/// Cloning a LogWriter is cheap (it holds an `Arc` to the underlying buffer)
/// and each clone can be used independently from any thread.
#[derive(Clone)]
pub struct LogWriter {
    buffer: Arc<RingBuffer>,
}

impl LogWriter {
    /// Create a new LogWriter backed by the given ring buffer.
    pub fn new(buffer: Arc<RingBuffer>) -> Self {
        Self { buffer }
    }

    /// Write a log record with the given tag, transaction ID, and data payload.
    pub fn log(&self, tag: LogTag, vxid: Vxid, data: impl Into<String>) {
        let data_string = data.into();

        // Emit to the tracing infrastructure as well, so that operators using
        // tracing-subscriber see VSL-style entries in their configured sinks.
        tracing::trace!(tag = tag.name(), vxid = vxid.0, "{}", data_string,);

        let record = LogRecord::new(tag, vxid, data_string);
        self.buffer.write(record);
    }

    /// Write a log record using format arguments, avoiding an intermediate
    /// String allocation when the caller uses the `format_args!` macro.
    pub fn log_fmt(&self, tag: LogTag, vxid: Vxid, args: std::fmt::Arguments<'_>) {
        let data_string = args.to_string();

        tracing::trace!(tag = tag.name(), vxid = vxid.0, "{}", data_string,);

        let record = LogRecord::new(tag, vxid, data_string);
        self.buffer.write(record);
    }

    /// Convenience: write a Debug-tagged log record.
    pub fn debug(&self, vxid: Vxid, msg: impl Into<String>) {
        self.log(LogTag::Debug, vxid, msg);
    }

    /// Convenience: write an Error-tagged log record.
    pub fn error(&self, vxid: Vxid, msg: impl Into<String>) {
        let msg_string: String = msg.into();

        tracing::error!(tag = "Error", vxid = vxid.0, "{}", msg_string,);

        let record = LogRecord::new(LogTag::Error, vxid, msg_string);
        self.buffer.write(record);
    }

    /// Return a reference to the underlying ring buffer.
    pub fn buffer(&self) -> &Arc<RingBuffer> {
        &self.buffer
    }
}

/// Convenience macro for writing formatted log entries without allocating
/// an intermediate String at the call site.
///
/// Usage:
/// ```ignore
/// rv_log!(writer, LogTag::Debug, vxid, "request {} took {}ms", url, elapsed);
/// ```
#[macro_export]
macro_rules! rv_log {
    ($writer:expr, $tag:expr, $vxid:expr, $($arg:tt)*) => {
        $writer.log_fmt($tag, $vxid, format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<RingBuffer>, LogWriter) {
        let buf = Arc::new(RingBuffer::new(64));
        let writer = LogWriter::new(Arc::clone(&buf));
        (buf, writer)
    }

    #[test]
    fn log_writes_to_buffer() {
        let (buf, writer) = setup();
        writer.log(LogTag::ReqStart, Vxid::new(1), "GET /index.html");
        assert_eq!(buf.len(), 1);

        let (records, _) = buf.read_from(0);
        assert_eq!(records[0].tag, LogTag::ReqStart);
        assert_eq!(records[0].data, "GET /index.html");
    }

    #[test]
    fn debug_and_error_convenience() {
        let (buf, writer) = setup();
        writer.debug(Vxid::new(10), "debug info");
        writer.error(Vxid::new(10), "something failed");

        assert_eq!(buf.len(), 2);
        let (records, _) = buf.read_from(0);
        assert_eq!(records[0].tag, LogTag::Debug);
        assert_eq!(records[1].tag, LogTag::Error);
    }

    #[test]
    fn log_fmt_writes_formatted_string() {
        let (buf, writer) = setup();
        let url = "/api/v1/data";
        let status = 200;
        rv_log!(
            writer,
            LogTag::RespStatus,
            Vxid::new(5),
            "{} {}",
            url,
            status
        );

        assert_eq!(buf.len(), 1);
        let (records, _) = buf.read_from(0);
        assert_eq!(records[0].data, "/api/v1/data 200");
    }

    #[test]
    fn writer_is_clone() {
        let (_buf, writer) = setup();
        let writer2 = writer.clone();
        writer.log(LogTag::Debug, Vxid::new(1), "from original");
        writer2.log(LogTag::Debug, Vxid::new(2), "from clone");
    }
}
