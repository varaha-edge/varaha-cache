/// Errors produced during VCL lexing and parsing.
#[derive(Debug, thiserror::Error)]
pub enum VclError {
    #[error("lex error at {line}:{col}: {message}")]
    LexError {
        message: String,
        line: usize,
        col: usize,
    },

    #[error("parse error at {line}:{col}: {message}")]
    ParseError {
        message: String,
        line: usize,
        col: usize,
    },
}
