//! Thread-safe ring buffer for storing log records.
//!
//! The ring buffer provides a fixed-capacity circular buffer backed by a
//! `Vec<Option<LogRecord>>`. When the buffer is full, the oldest entries are
//! overwritten. Thread safety is achieved via `parking_lot::RwLock`, which
//! provides better performance than `std::sync::RwLock` under contention due
//! to its smaller footprint and lack of poisoning.

use parking_lot::RwLock;

use crate::record::LogRecord;

/// Inner state of the ring buffer, protected by the RwLock.
struct RingBufInner {
    /// The backing storage. Slots start as `None` and are filled as records arrive.
    slots: Vec<Option<LogRecord>>,
    /// The next write position (wraps around at capacity).
    write_pos: usize,
    /// Total number of records ever written. Used by readers to detect how far
    /// behind they are and whether records have been overwritten.
    total_written: u64,
}

/// A fixed-size, thread-safe ring buffer for log records.
///
/// The buffer uses a circular write strategy: once the capacity is reached,
/// new records overwrite the oldest entries. Readers can request records from
/// a given logical position, read all currently stored records, or tail the
/// most recent N entries.
pub struct RingBuffer {
    inner: RwLock<RingBufInner>,
    capacity: usize,
}

impl RingBuffer {
    /// Create a new ring buffer with the specified capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be greater than zero");
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || None);
        Self {
            inner: RwLock::new(RingBufInner {
                slots,
                write_pos: 0,
                total_written: 0,
            }),
            capacity,
        }
    }

    /// Write a record into the ring buffer, overwriting the oldest entry if full.
    pub fn write(&self, record: LogRecord) {
        let mut inner = self.inner.write();
        let pos = inner.write_pos;
        inner.slots[pos] = Some(record);
        inner.write_pos = (pos + 1) % self.capacity;
        inner.total_written += 1;
    }

    /// Read all records starting from a logical position.
    ///
    /// The `position` parameter is a logical counter representing how many
    /// records the caller has already consumed. Returns all records written
    /// since that position, along with the new position the caller should
    /// track for the next call.
    ///
    /// If the caller has fallen too far behind and records have been overwritten,
    /// only the currently available records are returned.
    pub fn read_from(&self, position: u64) -> (Vec<LogRecord>, u64) {
        let inner = self.inner.read();
        let total = inner.total_written;

        if position >= total {
            // Caller is already caught up.
            return (Vec::new(), total);
        }

        // Determine how many records are available.
        let stored = std::cmp::min(total, self.capacity as u64);
        let oldest_available = total - stored;

        // If the caller's position is behind what we still have, clamp to oldest available.
        let effective_start = std::cmp::max(position, oldest_available);
        let count = (total - effective_start) as usize;

        let mut records = Vec::with_capacity(count);

        for i in 0..count {
            let logical_pos = effective_start + i as u64;
            let slot_index = (logical_pos % self.capacity as u64) as usize;
            if let Some(ref rec) = inner.slots[slot_index] {
                records.push(rec.clone());
            }
        }

        (records, total)
    }

    /// Return the number of records currently stored in the buffer.
    ///
    /// This is at most `capacity`. Before the buffer has wrapped, it equals
    /// the number of records written so far.
    pub fn len(&self) -> usize {
        let inner = self.inner.read();
        std::cmp::min(inner.total_written as usize, self.capacity)
    }

    /// Return true if the buffer contains no records.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return true if the buffer has wrapped at least once, meaning the oldest
    /// entries have been overwritten.
    pub fn is_full(&self) -> bool {
        let inner = self.inner.read();
        inner.total_written >= self.capacity as u64
    }

    /// Return the fixed capacity of this ring buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Return the total number of records ever written to this buffer.
    ///
    /// This value grows monotonically and is used by readers to track their
    /// read position.
    pub fn total_written(&self) -> u64 {
        self.inner.read().total_written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_types::vsl::Vxid;
    use rv_types::LogTag;

    fn make_record(id: u64, msg: &str) -> LogRecord {
        LogRecord::new(LogTag::Debug, Vxid::new(id), msg.to_string())
    }

    #[test]
    fn new_buffer_is_empty() {
        let buf = RingBuffer::new(10);
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(!buf.is_full());
        assert_eq!(buf.capacity(), 10);
    }

    #[test]
    #[should_panic(expected = "capacity must be greater than zero")]
    fn zero_capacity_panics() {
        let _ = RingBuffer::new(0);
    }

    #[test]
    fn write_and_read() {
        let buf = RingBuffer::new(4);
        buf.write(make_record(1, "first"));
        buf.write(make_record(2, "second"));

        assert_eq!(buf.len(), 2);
        assert!(!buf.is_full());

        let (records, pos) = buf.read_from(0);
        assert_eq!(records.len(), 2);
        assert_eq!(pos, 2);
        assert_eq!(records[0].data, "first");
        assert_eq!(records[1].data, "second");
    }

    #[test]
    fn wrapping_overwrites_oldest() {
        let buf = RingBuffer::new(3);
        buf.write(make_record(1, "a"));
        buf.write(make_record(2, "b"));
        buf.write(make_record(3, "c"));
        assert!(buf.is_full());

        // This overwrites "a".
        buf.write(make_record(4, "d"));
        assert_eq!(buf.len(), 3);

        let (records, pos) = buf.read_from(0);
        // We asked from 0, but "a" (position 0) is gone. We get b, c, d.
        assert_eq!(records.len(), 3);
        assert_eq!(pos, 4);
        assert_eq!(records[0].data, "b");
        assert_eq!(records[1].data, "c");
        assert_eq!(records[2].data, "d");
    }

    #[test]
    fn read_from_caught_up() {
        let buf = RingBuffer::new(4);
        buf.write(make_record(1, "x"));
        let (records, pos) = buf.read_from(1);
        assert!(records.is_empty());
        assert_eq!(pos, 1);
    }

    #[test]
    fn read_from_partial() {
        let buf = RingBuffer::new(10);
        for i in 0..5 {
            buf.write(make_record(i, &format!("msg-{}", i)));
        }
        // Read from position 3 -> should get records 3 and 4.
        let (records, pos) = buf.read_from(3);
        assert_eq!(records.len(), 2);
        assert_eq!(pos, 5);
        assert_eq!(records[0].data, "msg-3");
        assert_eq!(records[1].data, "msg-4");
    }

    #[test]
    fn total_written_tracks_all_writes() {
        let buf = RingBuffer::new(2);
        buf.write(make_record(1, "a"));
        buf.write(make_record(2, "b"));
        buf.write(make_record(3, "c")); // overwrites "a"
        assert_eq!(buf.total_written(), 3);
        assert_eq!(buf.len(), 2);
    }
}
