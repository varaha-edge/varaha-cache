use thiserror::Error;

/// Errors that can occur within VMOD execution.
#[derive(Debug, Error)]
pub enum VmodError {
    /// An argument to a VMOD function was invalid.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A type mismatch occurred during value conversion.
    #[error("type mismatch: {0}")]
    TypeMismatch(String),

    /// A requested VMOD, function, or resource was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An internal error within the VMOD subsystem.
    #[error("internal error: {0}")]
    InternalError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = VmodError::InvalidArgument("bad value".to_string());
        assert_eq!(err.to_string(), "invalid argument: bad value");

        let err = VmodError::TypeMismatch("expected int, got string".to_string());
        assert_eq!(err.to_string(), "type mismatch: expected int, got string");

        let err = VmodError::NotFound("std.nonexistent".to_string());
        assert_eq!(err.to_string(), "not found: std.nonexistent");

        let err = VmodError::InternalError("something broke".to_string());
        assert_eq!(err.to_string(), "internal error: something broke");
    }
}
