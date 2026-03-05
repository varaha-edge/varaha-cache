//! rv-vmod: Built-in VMOD implementations for Rust Varnish Cache.
//!
//! This crate provides the VMOD (Varnish Module) subsystem, including
//! a registry for managing modules and built-in implementations of
//! the standard VMODs:
//!
//! - **std** -- Type conversions, string operations, logging, time
//! - **blob** -- Binary data encoding/decoding (BASE64, HEX, URL)
//! - **cookie** -- HTTP cookie parsing and manipulation
//! - **purge** -- Cache purge operations (hard and soft)
//! - **directors** -- Backend director type wrappers

pub mod blob;
pub mod cookie;
pub mod directors;
pub mod error;
pub mod purge;
pub mod registry;
pub mod std_vmod;
pub mod types;

pub use error::VmodError;
pub use registry::{VmodFunction, VmodRegistry};
pub use types::{VclValue, VclValueExt};
