use std::cell::RefCell;

const MAX_POOL_SIZE: usize = 32;
const DEFAULT_BUF_CAPACITY: usize = 512;

thread_local! {
    static BUF_POOL: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// Get a buffer from the thread-local pool, or create a new one.
pub fn get_buf() -> Vec<u8> {
    BUF_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        match pool.pop() {
            Some(mut buf) => {
                buf.clear();
                buf
            }
            None => Vec::with_capacity(DEFAULT_BUF_CAPACITY),
        }
    })
}

/// Return a buffer to the thread-local pool for reuse.
pub fn put_buf(buf: Vec<u8>) {
    BUF_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < MAX_POOL_SIZE {
            pool.push(buf);
        }
        // else: drop the buffer (pool full)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_buf_returns_empty_buffer() {
        let buf = get_buf();
        assert!(buf.is_empty());
        assert!(buf.capacity() >= DEFAULT_BUF_CAPACITY);
    }

    #[test]
    fn put_and_get_reuses_buffer() {
        let mut buf = get_buf();
        buf.extend_from_slice(b"hello world");
        let cap = buf.capacity();
        put_buf(buf);

        let buf2 = get_buf();
        assert!(buf2.is_empty());
        assert_eq!(buf2.capacity(), cap);
    }

    #[test]
    fn pool_respects_max_size() {
        // Drain any buffers left by previous tests
        BUF_POOL.with(|pool| pool.borrow_mut().clear());

        // Fill the pool beyond max
        for _ in 0..(MAX_POOL_SIZE + 10) {
            put_buf(Vec::with_capacity(DEFAULT_BUF_CAPACITY));
        }

        // Pool should have exactly MAX_POOL_SIZE entries
        let count = BUF_POOL.with(|pool| pool.borrow().len());
        assert_eq!(count, MAX_POOL_SIZE);
    }
}
