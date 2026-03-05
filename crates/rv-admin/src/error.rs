//! Error types for the admin subsystem.

use thiserror::Error;

/// Errors that can occur during admin CLI operations.
#[derive(Debug, Error)]
pub enum AdminError {
    /// A command was parsed correctly but failed during execution.
    #[error("command failed: {0}")]
    CommandFailed(String),

    /// The input line could not be parsed into a valid command.
    #[error("invalid command: {0}")]
    InvalidCommand(String),

    /// Authentication failed (e.g. bad secret, unauthorized client).
    #[error("authentication error: {0}")]
    AuthError(String),

    /// An underlying I/O error occurred (connection drop, read failure, etc.).
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// An internal error that does not fit the other categories.
    #[error("internal error: {0}")]
    InternalError(String),
}
