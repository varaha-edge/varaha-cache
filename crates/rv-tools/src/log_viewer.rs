use std::sync::Arc;

use rv_log::{LogReader, LogRecord, RingBuffer};

/// Log viewer tool - streams VSL records from the ring buffer.
/// Equivalent to varnishlog in the C codebase.
///
/// An in-process log viewer that reads from a shared ring buffer.
///
/// The `LogViewer` wraps a `LogReader` and provides convenience methods
/// for retrieving and formatting log entries. In production, the ring
/// buffer would be backed by shared memory; for now it reads from an
/// in-process `RingBuffer` reference.
pub struct LogViewer {
    reader: LogReader,
}

impl LogViewer {
    /// Create a new `LogViewer` that reads from the given ring buffer.
    ///
    /// The viewer starts from the oldest available record so that `tail`
    /// calls can access historical entries.
    pub fn new(ringbuf: Arc<RingBuffer>) -> Self {
        let reader = LogReader::from_start(ringbuf);
        Self { reader }
    }

    /// Return the last `count` log entries as formatted strings.
    ///
    /// If fewer than `count` records exist in the buffer, all available
    /// records are returned. Each entry is formatted using `format_entry`.
    pub fn tail(&self, count: usize) -> Vec<String> {
        let records = self.reader.tail(count);
        records.iter().map(Self::format_entry).collect()
    }

    /// Format a single log record as a human-readable string.
    ///
    /// The output format is:
    /// ```text
    /// <tag>  <vxid>  <timestamp>  <data>
    /// ```
    pub fn format_entry(entry: &LogRecord) -> String {
        format!(
            "{:<12} {:>8} {:.6} {}",
            entry.tag.name(),
            entry.vxid,
            entry.timestamp.as_secs(),
            entry.data,
        )
    }

    /// Read all new records since the last read and return them as
    /// formatted strings.
    pub fn read_new(&mut self) -> Vec<String> {
        let records = self.reader.read_new();
        records.iter().map(Self::format_entry).collect()
    }

    /// Read all records currently stored in the ring buffer as
    /// formatted strings.
    pub fn read_all(&self) -> Vec<String> {
        let records = self.reader.read_all();
        records.iter().map(Self::format_entry).collect()
    }

    /// Return a reference to the underlying `LogReader` for advanced
    /// filtering or direct record access.
    pub fn reader(&self) -> &LogReader {
        &self.reader
    }

    /// Return a mutable reference to the underlying `LogReader`.
    pub fn reader_mut(&mut self) -> &mut LogReader {
        &mut self.reader
    }
}

fn print_usage() {
    eprintln!("Usage: rv-log-viewer [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -t TAG       Filter by log tag name");
    eprintln!("  -n COUNT     Number of records to show (tail mode)");
    eprintln!("  -f           Follow mode (stream new records)");
    eprintln!("  -h           Show this help");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut tag_filter: Option<String> = None;
    let mut tail_count: Option<usize> = None;
    let mut follow = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    tag_filter = Some(args[i].clone());
                }
            }
            "-n" => {
                i += 1;
                if i < args.len() {
                    tail_count = args[i].parse().ok();
                }
            }
            "-f" => {
                follow = true;
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // Create a ring buffer and viewer for demonstration.
    // In production, this would attach to shared memory.
    let ringbuf = Arc::new(RingBuffer::new(8192));
    let mut viewer = LogViewer::new(Arc::clone(&ringbuf));

    if let Some(count) = tail_count {
        // Tail mode: show last N records.
        let entries = viewer.tail(count);
        for entry in &entries {
            if let Some(ref tag) = tag_filter
                && !entry.contains(tag.as_str())
            {
                continue;
            }
            println!("{entry}");
        }
    } else if follow {
        // Follow mode: stream new records.
        println!("Following log output (Ctrl+C to stop)...");
        loop {
            let entries = viewer.read_new();
            for entry in &entries {
                if let Some(ref tag) = tag_filter
                    && !entry.contains(tag.as_str())
                {
                    continue;
                }
                println!("{entry}");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    } else {
        // Read all available records.
        let entries = viewer.read_all();
        if entries.is_empty() {
            println!("No log records available.");
        } else {
            for entry in &entries {
                if let Some(ref tag) = tag_filter
                    && !entry.contains(tag.as_str())
                {
                    continue;
                }
                println!("{entry}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_log::LogWriter;
    use rv_types::LogTag;
    use rv_types::vsl::Vxid;

    fn make_test_buffer() -> Arc<RingBuffer> {
        let buf = Arc::new(RingBuffer::new(64));
        let writer = LogWriter::new(Arc::clone(&buf));
        writer.log(LogTag::ReqStart, Vxid::new(1), "GET /index.html");
        writer.log(LogTag::ReqURL, Vxid::new(1), "/index.html");
        writer.log(LogTag::Debug, Vxid::new(1), "cache lookup");
        writer.log(LogTag::RespStatus, Vxid::new(1), "200");
        writer.log(LogTag::ReqStart, Vxid::new(2), "GET /api/data");
        writer.log(LogTag::Error, Vxid::new(2), "backend timeout");
        buf
    }

    #[test]
    fn log_viewer_new() {
        let buf = make_test_buffer();
        let viewer = LogViewer::new(buf);
        let all = viewer.read_all();
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn log_viewer_tail() {
        let buf = make_test_buffer();
        let viewer = LogViewer::new(buf);
        let last3 = viewer.tail(3);
        assert_eq!(last3.len(), 3);
        assert!(last3[0].contains("200"));
        assert!(last3[1].contains("GET /api/data"));
        assert!(last3[2].contains("backend timeout"));
    }

    #[test]
    fn log_viewer_tail_more_than_available() {
        let buf = make_test_buffer();
        let viewer = LogViewer::new(buf);
        let all = viewer.tail(100);
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn log_viewer_tail_zero() {
        let buf = make_test_buffer();
        let viewer = LogViewer::new(buf);
        let zero = viewer.tail(0);
        assert!(zero.is_empty());
    }

    #[test]
    fn log_viewer_format_entry() {
        let record = LogRecord::new(LogTag::Debug, Vxid::new(42), "test message".to_string());
        let formatted = LogViewer::format_entry(&record);
        assert!(formatted.contains("Debug"));
        assert!(formatted.contains("42"));
        assert!(formatted.contains("test message"));
    }

    #[test]
    fn log_viewer_read_new_incremental() {
        let buf = Arc::new(RingBuffer::new(64));
        let writer = LogWriter::new(Arc::clone(&buf));
        let mut viewer = LogViewer::new(Arc::clone(&buf));

        writer.log(LogTag::Debug, Vxid::new(1), "first");
        let batch1 = viewer.read_new();
        assert_eq!(batch1.len(), 1);
        assert!(batch1[0].contains("first"));

        writer.log(LogTag::Debug, Vxid::new(2), "second");
        writer.log(LogTag::Debug, Vxid::new(3), "third");
        let batch2 = viewer.read_new();
        assert_eq!(batch2.len(), 2);
        assert!(batch2[0].contains("second"));
        assert!(batch2[1].contains("third"));
    }

    #[test]
    fn log_viewer_empty_buffer() {
        let buf = Arc::new(RingBuffer::new(64));
        let viewer = LogViewer::new(buf);
        let entries = viewer.read_all();
        assert!(entries.is_empty());
        let tail = viewer.tail(10);
        assert!(tail.is_empty());
    }

    #[test]
    fn log_viewer_reader_access() {
        let buf = make_test_buffer();
        let viewer = LogViewer::new(buf);
        // Verify the reader is accessible for advanced filtering.
        let position = viewer.reader().position();
        assert_eq!(position, 0); // from_start initializes at 0
    }
}
