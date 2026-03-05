//! rv-log: Structured ring-buffer based logging for varaha-cache.
//!
//! This crate provides a VSL (Varnish Shared Log) inspired logging system
//! built around a fixed-size, thread-safe ring buffer. Log records are tagged
//! with VSL-style tags (from `rv_types::LogTag`), associated with transaction
//! IDs (`rv_types::vsl::Vxid`), and timestamped with wall-clock time.
//!
//! The design mirrors the original Varnish shared memory log: a circular
//! buffer that overwrites the oldest entries when capacity is exhausted,
//! with readers that can independently tail or query the log.
//!
//! ## Architecture
//!
//! - **LogRecord** - A single structured log entry.
//! - **RingBuffer** - The fixed-capacity circular storage, thread-safe via
//!   `parking_lot::RwLock`.
//! - **LogWriter** - A write handle that produces records and emits them to
//!   both the ring buffer and the `tracing` infrastructure.
//! - **LogReader** - A read handle with cursor tracking for incremental
//!   consumption and filtering by tag or transaction ID.
//!
//! ## Example
//!
//! ```rust
//! use std::sync::Arc;
//! use rv_types::vsl::Vxid;
//! use rv_types::LogTag;
//! use rv_log::{RingBuffer, LogWriter, LogReader};
//!
//! let buf = Arc::new(RingBuffer::new(1024));
//! let writer = LogWriter::new(Arc::clone(&buf));
//! let mut reader = LogReader::from_start(Arc::clone(&buf));
//!
//! writer.log(LogTag::ReqStart, Vxid::new(1), "GET /index.html");
//! writer.debug(Vxid::new(1), "cache lookup starting");
//!
//! let new_records = reader.read_new();
//! assert_eq!(new_records.len(), 2);
//! ```

pub mod reader;
pub mod record;
pub mod ringbuf;
pub mod writer;

// Re-export the primary public types at crate root for convenience.
pub use reader::LogReader;
pub use record::LogRecord;
pub use ringbuf::RingBuffer;
pub use writer::LogWriter;
