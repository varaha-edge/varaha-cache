use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::Mutex;
use tokio::sync::Notify;

/// A streaming body buffer that supports concurrent producers and consumers.
///
/// The producer appends chunks via `append()` and signals completion with
/// `mark_complete()`. Consumers can read data that has already arrived via
/// `read_from()`, or asynchronously wait for new data via `wait_for_data()`.
///
/// This enables "hit-for-miss" streaming: while a fetch is still in progress,
/// waiting clients can begin receiving data as soon as chunks arrive, rather
/// than waiting for the entire response to be stored.
pub struct StreamingBody {
    /// Received body chunks, in order of arrival.
    chunks: Mutex<Vec<Vec<u8>>>,
    /// Whether the backend fetch is complete.
    complete: AtomicBool,
    /// Notification channel for new data arrival.
    notify: Notify,
    /// Total bytes received across all chunks.
    total_size: AtomicUsize,
}

impl StreamingBody {
    /// Create a new empty streaming body.
    pub fn new() -> Self {
        Self {
            chunks: Mutex::new(Vec::new()),
            complete: AtomicBool::new(false),
            notify: Notify::new(),
            total_size: AtomicUsize::new(0),
        }
    }

    /// Append a chunk of data and notify any waiting consumers.
    ///
    /// This is called by the fetch task as data arrives from the backend.
    pub fn append(&self, data: Vec<u8>) {
        let len = data.len();
        {
            let mut chunks = self.chunks.lock();
            chunks.push(data);
        }
        self.total_size.fetch_add(len, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Signal that all data has been received from the backend.
    ///
    /// After this call, `is_complete()` returns `true` and any consumers
    /// waiting in `wait_for_data()` will be woken.
    pub fn mark_complete(&self) {
        self.complete.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Check whether the fetch is complete.
    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }

    /// Read data starting from a byte offset.
    ///
    /// Returns `(data, is_complete)` where `data` contains all bytes from
    /// `offset` to the current end of the buffer, and `is_complete` indicates
    /// whether the fetch has finished (no more data will arrive).
    ///
    /// If `offset` is beyond the current total size, returns empty data.
    pub fn read_from(&self, offset: usize) -> (Vec<u8>, bool) {
        let chunks = self.chunks.lock();
        let complete = self.is_complete();

        let mut result = Vec::new();
        let mut current_offset = 0;

        for chunk in chunks.iter() {
            let chunk_end = current_offset + chunk.len();
            if chunk_end <= offset {
                // This entire chunk is before the requested offset
                current_offset = chunk_end;
                continue;
            }
            let start_in_chunk = if offset > current_offset {
                offset - current_offset
            } else {
                0
            };
            result.extend_from_slice(&chunk[start_in_chunk..]);
            current_offset = chunk_end;
        }

        (result, complete)
    }

    /// Asynchronously wait for new data beyond the given offset.
    ///
    /// If data is already available beyond `offset`, it returns immediately.
    /// If the stream is complete and no new data is available, it returns
    /// empty data with `is_complete = true`.
    ///
    /// Returns `(data, is_complete)` just like `read_from()`.
    pub async fn wait_for_data(&self, offset: usize) -> (Vec<u8>, bool) {
        loop {
            let (data, complete) = self.read_from(offset);
            if !data.is_empty() || complete {
                return (data, complete);
            }
            // No data available yet and stream is not complete -- wait
            self.notify.notified().await;
        }
    }

    /// Return the total number of bytes received so far.
    pub fn total_size(&self) -> usize {
        self.total_size.load(Ordering::Acquire)
    }
}

impl Default for StreamingBody {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_append_and_read_sequentially() {
        let body = StreamingBody::new();

        // Append two chunks
        body.append(b"hello ".to_vec());
        body.append(b"world".to_vec());

        // Read from the beginning
        let (data, complete) = body.read_from(0);
        assert_eq!(&data, b"hello world");
        assert!(!complete);

        // Read from an offset in the middle
        let (data, _) = body.read_from(6);
        assert_eq!(&data, b"world");

        // Read from offset at the end
        let (data, _) = body.read_from(11);
        assert!(data.is_empty());
    }

    #[test]
    fn test_read_from_offset_within_chunk() {
        let body = StreamingBody::new();
        body.append(b"abcdef".to_vec());
        body.append(b"ghijkl".to_vec());

        // Offset 3 is within the first chunk
        let (data, _) = body.read_from(3);
        assert_eq!(&data, b"defghijkl");

        // Offset 8 is within the second chunk
        let (data, _) = body.read_from(8);
        assert_eq!(&data, b"ijkl");
    }

    #[test]
    fn test_complete_signaling() {
        let body = StreamingBody::new();
        assert!(!body.is_complete());

        body.append(b"data".to_vec());
        assert!(!body.is_complete());

        body.mark_complete();
        assert!(body.is_complete());

        let (data, complete) = body.read_from(0);
        assert_eq!(&data, b"data");
        assert!(complete);
    }

    #[test]
    fn test_total_size_tracking() {
        let body = StreamingBody::new();
        assert_eq!(body.total_size(), 0);

        body.append(b"abc".to_vec());
        assert_eq!(body.total_size(), 3);

        body.append(b"de".to_vec());
        assert_eq!(body.total_size(), 5);
    }

    #[test]
    fn test_empty_body() {
        let body = StreamingBody::new();
        let (data, complete) = body.read_from(0);
        assert!(data.is_empty());
        assert!(!complete);

        body.mark_complete();
        let (data, complete) = body.read_from(0);
        assert!(data.is_empty());
        assert!(complete);
    }

    #[tokio::test]
    async fn test_wait_for_data_immediate_return() {
        let body = Arc::new(StreamingBody::new());
        body.append(b"available".to_vec());

        // Data is already available, should return immediately
        let (data, complete) = body.wait_for_data(0).await;
        assert_eq!(&data, b"available");
        assert!(!complete);
    }

    #[tokio::test]
    async fn test_wait_for_data_complete_no_data() {
        let body = Arc::new(StreamingBody::new());
        body.mark_complete();

        // Stream complete with no data -- should return immediately
        let (data, complete) = body.wait_for_data(0).await;
        assert!(data.is_empty());
        assert!(complete);
    }

    #[tokio::test]
    async fn test_concurrent_read_write() {
        let body = Arc::new(StreamingBody::new());
        let writer = Arc::clone(&body);
        let reader = Arc::clone(&body);

        // Spawn a writer task
        let writer_handle = tokio::spawn(async move {
            for i in 0..10 {
                let chunk = format!("chunk{i}");
                writer.append(chunk.into_bytes());
                tokio::task::yield_now().await;
            }
            writer.mark_complete();
        });

        // Reader waits for data incrementally
        let reader_handle = tokio::spawn(async move {
            let mut offset = 0;
            let mut all_data = Vec::new();
            loop {
                let (data, complete) = reader.wait_for_data(offset).await;
                offset += data.len();
                all_data.extend_from_slice(&data);
                if complete {
                    break;
                }
            }
            all_data
        });

        writer_handle.await.unwrap();
        let all_data = reader_handle.await.unwrap();

        // Verify all chunks arrived
        let expected = (0..10)
            .map(|i| format!("chunk{i}"))
            .collect::<String>();
        assert_eq!(
            std::str::from_utf8(&all_data).unwrap(),
            expected,
        );
    }

    #[tokio::test]
    async fn test_multiple_concurrent_readers() {
        let body = Arc::new(StreamingBody::new());

        // Spawn multiple readers
        let mut reader_handles = Vec::new();
        for _ in 0..3 {
            let reader = Arc::clone(&body);
            reader_handles.push(tokio::spawn(async move {
                let mut offset = 0;
                let mut all_data = Vec::new();
                loop {
                    let (data, complete) = reader.wait_for_data(offset).await;
                    offset += data.len();
                    all_data.extend_from_slice(&data);
                    if complete {
                        break;
                    }
                }
                all_data
            }));
        }

        // Write data
        body.append(b"part1".to_vec());
        tokio::task::yield_now().await;
        body.append(b"part2".to_vec());
        tokio::task::yield_now().await;
        body.mark_complete();

        // All readers should get the same data
        for handle in reader_handles {
            let data = handle.await.unwrap();
            assert_eq!(std::str::from_utf8(&data).unwrap(), "part1part2");
        }
    }

    #[test]
    fn test_read_from_beyond_total_size() {
        let body = StreamingBody::new();
        body.append(b"short".to_vec());

        let (data, _) = body.read_from(100);
        assert!(data.is_empty());
    }
}
