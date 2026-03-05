//! rv-admin: Management CLI and admin interface for varaha-cache.
//!
//! This crate provides the management CLI server that accepts TCP connections
//! from administrative tools, parses CLI commands, executes them against the
//! cache engine, and returns structured responses. The wire protocol mirrors
//! the Varnish CLI protocol with status codes and length-prefixed response bodies.
//!
//! ## Architecture
//!
//! - **protocol** - Wire-level encoding and decoding of CLI responses.
//! - **commands** - Parsing of text command lines into typed `CliCommand` variants.
//! - **handler** - Execution of commands against the cache engine and configuration.
//! - **server** - Async TCP server that accepts admin connections via Tokio.
//! - **auth** - Challenge-response authentication for admin connections.
//! - **error** - Error types for the admin subsystem.

pub mod auth;
pub mod error;
pub mod protocol;
pub mod commands;
pub mod handler;
pub mod server;
pub mod vcl_manager;

pub use error::AdminError;
pub use protocol::{CliStatus, CliResponse, encode_response, decode_response};
pub use commands::{CliCommand, parse_command};
pub use handler::{AdminContext, handle_command};
pub use server::AdminServer;
pub use vcl_manager::VclManager;
pub use auth::{generate_challenge, compute_auth_response, verify_auth};
