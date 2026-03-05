//! Log reader for consuming and filtering log records.
//!
//! LogReader wraps a shared reference to the ring buffer and maintains a
//! read cursor so callers can incrementally consume new records. It also
//! provides filtering helpers to narrow results by tag or transaction ID.

use std::sync::Arc;

use rv_types::vsl::Vxid;
use rv_types::LogTag;

use crate::record::LogRecord;
use crate::ringbuf::RingBuffer;

/// A reader handle for consuming log records from a shared ring buffer.
///
/// Each reader independently tracks its own read position, so multiple
/// readers can consume the same buffer at their own pace.
pub struct LogReader {
    buffer: Arc<RingBuffer>,
    /// The logical position of the next record this reader expects to consume.
    position: u64,
}

impl LogReader {
    /// Create a new LogReader starting at the current end of the buffer.
    ///
    /// This means the reader will only see records written after its creation.
    /// To start from the beginning, use `LogReader::from_start`.
    pub fn new(buffer: Arc<RingBuffer>) -> Self {
        let position = buffer.total_written();
        Self { buffer, position }
    }

    /// Create a new LogReader starting from the oldest available record.
    pub fn from_start(buffer: Arc<RingBuffer>) -> Self {
        Self {
            buffer,
            position: 0,
        }
    }

    /// Read all new records since the last call to `read_new`.
    ///
    /// Advances the internal read position so the next call will only
    /// return records written after this one completes.
    pub fn read_new(&mut self) -> Vec<LogRecord> {
        let (records, new_pos) = self.buffer.read_from(self.position);
        self.position = new_pos;
        records
    }

    /// Read all currently stored records in the buffer, regardless of
    /// the reader's current position.
    ///
    /// This does NOT advance the read position.
    pub fn read_all(&self) -> Vec<LogRecord> {
        let (records, _) = self.buffer.read_from(0);
        records
    }

    /// Return the most recent `count` records from the buffer.
    ///
    /// If fewer than `count` records exist, all available records are returned.
    /// This does NOT advance the read position.
    pub fn tail(&self, count: usize) -> Vec<LogRecord> {
        let total = self.buffer.total_written();
        let capacity = self.buffer.capacity() as u64;
        let stored = std::cmp::min(total, capacity);
        let skip = if stored > count as u64 {
            total - count as u64
        } else {
            total - stored
        };
        let (records, _) = self.buffer.read_from(skip);
        records
    }

    /// Filter the given records, returning only those matching the specified tag.
    pub fn filter_by_tag(records: &[LogRecord], tag: LogTag) -> Vec<LogRecord> {
        records
            .iter()
            .filter(|r| r.tag == tag)
            .cloned()
            .collect()
    }

    /// Filter the given records, returning only those matching the specified
    /// transaction ID.
    pub fn filter_by_vxid(records: &[LogRecord], vxid: Vxid) -> Vec<LogRecord> {
        records
            .iter()
            .filter(|r| r.vxid == vxid)
            .cloned()
            .collect()
    }

    /// Return the reader's current logical position.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Reset the reader's position to the current end of the buffer,
    /// effectively skipping all unread records.
    pub fn skip_to_end(&mut self) {
        self.position = self.buffer.total_written();
    }

    /// Reset the reader's position to the beginning, so the next `read_new`
    /// will return all available records.
    pub fn reset(&mut self) {
        self.position = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ringbuf::RingBuffer;

    fn make_buffer_with_records() -> Arc<RingBuffer> {
        let buf = Arc::new(RingBuffer::new(32));
        let records = [
            (LogTag::ReqStart, Vxid::new(1), "start req 1"),
            (LogTag::ReqURL, Vxid::new(1), "/index.html"),
            (LogTag::Debug, Vxid::new(1), "debug info"),
            (LogTag::ReqStart, Vxid::new(2), "start req 2"),
            (LogTag::ReqURL, Vxid::new(2), "/api/data"),
            (LogTag::Error, Vxid::new(2), "backend timeout"),
        ];
        for (tag, vxid, data) in &records {
            buf.write(LogRecord::new(*tag, *vxid, data.to_string()));
        }
        buf
    }

    #[test]
    fn read_new_incremental() {
        let buf = Arc::new(RingBuffer::new(16));
        let mut reader = LogReader::from_start(Arc::clone(&buf));

        buf.write(LogRecord::new(
            LogTag::Debug,
            Vxid::new(1),
            "first".into(),
        ));
        let batch1 = reader.read_new();
        assert_eq!(batch1.len(), 1);
        assert_eq!(batch1[0].data, "first");

        buf.write(LogRecord::new(
            LogTag::Debug,
            Vxid::new(2),
            "second".into(),
        ));
        buf.write(LogRecord::new(
            LogTag::Debug,
            Vxid::new(3),
            "third".into(),
        ));
        let batch2 = reader.read_new();
        assert_eq!(batch2.len(), 2);
        assert_eq!(batch2[0].data, "second");
        assert_eq!(batch2[1].data, "third");
    }

    #[test]
    fn read_new_empty_when_caught_up() {
        let buf = Arc::new(RingBuffer::new(8));
        buf.write(LogRecord::new(LogTag::Debug, Vxid::new(1), "msg".into()));
        // Reader starts at the end by default.
        let mut reader = LogReader::new(Arc::clone(&buf));
        let records = reader.read_new();
        assert!(records.is_empty());
    }

    #[test]
    fn read_all_returns_everything() {
        let buf = make_buffer_with_records();
        let reader = LogReader::new(Arc::clone(&buf));
        let all = reader.read_all();
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn tail_returns_last_n() {
        let buf = make_buffer_with_records();
        let reader = LogReader::new(Arc::clone(&buf));
        let last3 = reader.tail(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].data, "start req 2");
        assert_eq!(last3[1].data, "/api/data");
        assert_eq!(last3[2].data, "backend timeout");
    }

    #[test]
    fn tail_more_than_available() {
        let buf = make_buffer_with_records();
        let reader = LogReader::new(Arc::clone(&buf));
        let all = reader.tail(100);
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn filter_by_tag() {
        let buf = make_buffer_with_records();
        let reader = LogReader::new(Arc::clone(&buf));
        let all = reader.read_all();

        let starts = LogReader::filter_by_tag(&all, LogTag::ReqStart);
        assert_eq!(starts.len(), 2);
        assert!(starts.iter().all(|r| r.tag == LogTag::ReqStart));
    }

    #[test]
    fn filter_by_vxid() {
        let buf = make_buffer_with_records();
        let reader = LogReader::new(Arc::clone(&buf));
        let all = reader.read_all();

        let vxid1 = LogReader::filter_by_vxid(&all, Vxid::new(1));
        assert_eq!(vxid1.len(), 3);
        assert!(vxid1.iter().all(|r| r.vxid == Vxid::new(1)));

        let vxid2 = LogReader::filter_by_vxid(&all, Vxid::new(2));
        assert_eq!(vxid2.len(), 3);
    }

    #[test]
    fn skip_to_end_and_reset() {
        let buf = make_buffer_with_records();
        let mut reader = LogReader::from_start(Arc::clone(&buf));

        // Read some.
        let _ = reader.read_new();
        assert_eq!(reader.position(), 6);

        // Write more.
        buf.write(LogRecord::new(
            LogTag::Debug,
            Vxid::new(99),
            "new".into(),
        ));

        // Skip past the new record.
        reader.skip_to_end();
        let records = reader.read_new();
        assert!(records.is_empty());

        // Reset to beginning.
        reader.reset();
        let all = reader.read_new();
        assert_eq!(all.len(), 7);
    }

    #[test]
    fn tail_on_wrapped_buffer() {
        let buf = Arc::new(RingBuffer::new(4));
        // Write 6 records into a capacity-4 buffer; first 2 are overwritten.
        for i in 0..6u64 {
            buf.write(LogRecord::new(
                LogTag::Debug,
                Vxid::new(i),
                format!("msg-{}", i),
            ));
        }
        let reader = LogReader::new(Arc::clone(&buf));
        let last2 = reader.tail(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].data, "msg-4");
        assert_eq!(last2[1].data, "msg-5");
    }
}
