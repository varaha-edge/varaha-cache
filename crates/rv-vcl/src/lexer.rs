use crate::error::VclError;
use crate::token::{Token, TokenKind};

/// VCL lexer/tokenizer.
///
/// Converts raw VCL source text into a sequence of tokens, handling:
/// - C-style comments (`//` line comments and `/* */` block comments)
/// - VCL long strings `{"..."}`
/// - Regular double-quoted strings `"..."`
/// - Duration literals (`120s`, `5m`, `1h`, `2d`, `500ms`)
/// - Integer and real number literals
/// - All VCL keywords, operators, and delimiters
/// - Accurate line and column tracking for error reporting
pub struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Tokenize the entire source, returning a vector of tokens ending with `Eof`.
    pub fn tokenize(source: &str) -> Result<Vec<Token>, VclError> {
        let mut lexer = Lexer::new(source);
        let mut tokens = Vec::new();

        loop {
            let tok = lexer.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }

        Ok(tokens)
    }

    fn peek(&self) -> Option<u8> {
        if self.pos < self.source.len() {
            Some(self.source[self.pos])
        } else {
            None
        }
    }

    fn peek_ahead(&self, offset: usize) -> Option<u8> {
        let idx = self.pos + offset;
        if idx < self.source.len() {
            Some(self.source[idx])
        } else {
            None
        }
    }

    fn advance(&mut self) -> Option<u8> {
        if self.pos < self.source.len() {
            let ch = self.source[self.pos];
            self.pos += 1;
            if ch == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(ch)
        } else {
            None
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == b' ' || ch == b'\t' || ch == b'\n' || ch == b'\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.advance() {
            if ch == b'\n' {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), VclError> {
        let start_line = self.line;
        let start_col = self.col;
        loop {
            match self.advance() {
                Some(b'*') => {
                    if self.peek() == Some(b'/') {
                        self.advance();
                        return Ok(());
                    }
                }
                None => {
                    return Err(VclError::LexError {
                        message: "unterminated block comment".to_string(),
                        line: start_line,
                        col: start_col,
                    });
                }
                _ => {}
            }
        }
    }

    fn skip_comments_and_whitespace(&mut self) -> Result<(), VclError> {
        loop {
            self.skip_whitespace();
            if self.peek() == Some(b'/') {
                if self.peek_ahead(1) == Some(b'/') {
                    // C++ style line comment
                    self.advance();
                    self.advance();
                    self.skip_line_comment();
                } else if self.peek_ahead(1) == Some(b'*') {
                    // C style block comment
                    self.advance();
                    self.advance();
                    self.skip_block_comment()?;
                } else {
                    // Not a comment, it is the `/` operator
                    break;
                }
            } else if self.peek() == Some(b'#') {
                // Shell-style line comment (also valid in VCL)
                self.advance();
                self.skip_line_comment();
            } else {
                break;
            }
        }
        Ok(())
    }

    fn read_string(&mut self) -> Result<String, VclError> {
        let start_line = self.line;
        let start_col = self.col;
        // Opening quote already consumed by caller
        let mut s = String::new();
        loop {
            match self.advance() {
                Some(b'\\') => {
                    // Escape sequences
                    match self.advance() {
                        Some(b'n') => s.push('\n'),
                        Some(b't') => s.push('\t'),
                        Some(b'r') => s.push('\r'),
                        Some(b'\\') => s.push('\\'),
                        Some(b'"') => s.push('"'),
                        Some(ch) => {
                            s.push('\\');
                            s.push(ch as char);
                        }
                        None => {
                            return Err(VclError::LexError {
                                message: "unterminated string literal".to_string(),
                                line: start_line,
                                col: start_col,
                            });
                        }
                    }
                }
                Some(b'"') => return Ok(s),
                Some(ch) => s.push(ch as char),
                None => {
                    return Err(VclError::LexError {
                        message: "unterminated string literal".to_string(),
                        line: start_line,
                        col: start_col,
                    });
                }
            }
        }
    }

    fn read_long_string(&mut self) -> Result<String, VclError> {
        let start_line = self.line;
        let start_col = self.col;
        // {"  — the `{` and `"` have already been consumed by the caller
        let mut s = String::new();
        loop {
            match self.advance() {
                Some(b'"') => {
                    if self.peek() == Some(b'}') {
                        self.advance(); // consume the closing `}`
                        return Ok(s);
                    }
                    s.push('"');
                }
                Some(ch) => s.push(ch as char),
                None => {
                    return Err(VclError::LexError {
                        message: "unterminated long string literal".to_string(),
                        line: start_line,
                        col: start_col,
                    });
                }
            }
        }
    }

    fn read_number(&mut self, first: u8) -> Result<TokenKind, VclError> {
        let mut num_str = String::new();
        num_str.push(first as char);
        let mut is_real = false;

        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                num_str.push(ch as char);
                self.advance();
            } else if ch == b'.' && !is_real {
                // Check if next char after dot is a digit (to distinguish from e.g. `1.method`)
                if let Some(next) = self.peek_ahead(1) {
                    if next.is_ascii_digit() {
                        is_real = true;
                        num_str.push('.');
                        self.advance();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // Check for duration suffix
        if let Some(ch) = self.peek() {
            if ch == b's' || ch == b'm' || ch == b'h' || ch == b'd' {
                let suffix_start = self.pos;
                let suffix_char = ch;
                self.advance();

                // Check for "ms" (milliseconds)
                if suffix_char == b'm' && self.peek() == Some(b's') {
                    self.advance();
                    let val: f64 = num_str.parse().map_err(|_| VclError::LexError {
                        message: format!("invalid number: {num_str}"),
                        line: self.line,
                        col: self.col,
                    })?;
                    return Ok(TokenKind::DurationLit(val / 1000.0));
                }

                // Check that the char after the suffix is not alphanumeric (i.e. it is truly
                // a duration suffix and not part of an identifier like `set`)
                if let Some(next) = self.peek() {
                    if next.is_ascii_alphanumeric() || next == b'_' {
                        // Not a duration suffix; rewind
                        self.pos = suffix_start;
                        self.col -= 1; // rough unwind; col is not perfectly tracked on rewind
                    } else {
                        let val: f64 = num_str.parse().map_err(|_| VclError::LexError {
                            message: format!("invalid number: {num_str}"),
                            line: self.line,
                            col: self.col,
                        })?;
                        let seconds = match suffix_char {
                            b's' => val,
                            b'm' => val * 60.0,
                            b'h' => val * 3600.0,
                            b'd' => val * 86400.0,
                            _ => unreachable!(),
                        };
                        return Ok(TokenKind::DurationLit(seconds));
                    }
                } else {
                    // End of input after suffix: it is a duration
                    let val: f64 = num_str.parse().map_err(|_| VclError::LexError {
                        message: format!("invalid number: {num_str}"),
                        line: self.line,
                        col: self.col,
                    })?;
                    let seconds = match suffix_char {
                        b's' => val,
                        b'm' => val * 60.0,
                        b'h' => val * 3600.0,
                        b'd' => val * 86400.0,
                        _ => unreachable!(),
                    };
                    return Ok(TokenKind::DurationLit(seconds));
                }
            }
        }

        if is_real {
            let val: f64 = num_str.parse().map_err(|_| VclError::LexError {
                message: format!("invalid real number: {num_str}"),
                line: self.line,
                col: self.col,
            })?;
            Ok(TokenKind::RealLit(val))
        } else {
            let val: i64 = num_str.parse().map_err(|_| VclError::LexError {
                message: format!("invalid integer: {num_str}"),
                line: self.line,
                col: self.col,
            })?;
            Ok(TokenKind::IntLit(val))
        }
    }

    fn read_ident(&mut self, first: u8) -> TokenKind {
        let mut ident = String::new();
        ident.push(first as char);

        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-' {
                ident.push(ch as char);
                self.advance();
            } else {
                break;
            }
        }

        // Map keywords
        match ident.as_str() {
            "vcl" => TokenKind::Vcl,
            "backend" => TokenKind::Backend,
            "probe" => TokenKind::Probe,
            "acl" => TokenKind::Acl,
            "sub" => TokenKind::Sub,
            "import" => TokenKind::Import,
            "if" => TokenKind::If,
            "elsif" | "elif" | "elseif" => TokenKind::ElseIf,
            "else" => TokenKind::Else,
            "return" => TokenKind::Return,
            "set" => TokenKind::Set,
            "unset" => TokenKind::Unset,
            "call" => TokenKind::Call,
            "new" => TokenKind::New,
            "include" => TokenKind::Include,
            "synthetic" => TokenKind::Synthetic,
            "ban" => TokenKind::Ban,
            "true" => TokenKind::BoolLit(true),
            "false" => TokenKind::BoolLit(false),
            _ => TokenKind::Ident(ident),
        }
    }

    fn next_token(&mut self) -> Result<Token, VclError> {
        self.skip_comments_and_whitespace()?;

        let line = self.line;
        let col = self.col;

        let ch = match self.advance() {
            Some(ch) => ch,
            None => return Ok(Token::new(TokenKind::Eof, line, col)),
        };

        let kind = match ch {
            b'{' => {
                // Check for long string `{"`
                if self.peek() == Some(b'"') {
                    self.advance();
                    let s = self.read_long_string()?;
                    TokenKind::StringLit(s)
                } else {
                    TokenKind::LBrace
                }
            }
            b'}' => TokenKind::RBrace,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b';' => TokenKind::Semicolon,
            b',' => TokenKind::Comma,
            b'.' => TokenKind::Dot,
            b'~' => TokenKind::Match,
            b'*' => TokenKind::Star,

            b'+' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::PlusAssign
                } else {
                    TokenKind::Plus
                }
            }
            b'-' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::MinusAssign
                } else {
                    TokenKind::Minus
                }
            }
            b'/' => {
                // The skip_comments_and_whitespace already handles // and /* so if
                // we reach here, it is the division operator.
                TokenKind::Slash
            }

            b'=' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::Eq
                } else {
                    TokenKind::Assign
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::Neq
                } else if self.peek() == Some(b'~') {
                    self.advance();
                    TokenKind::NotMatch
                } else {
                    TokenKind::Not
                }
            }
            b'&' => {
                if self.peek() == Some(b'&') {
                    self.advance();
                    TokenKind::And
                } else {
                    return Err(VclError::LexError {
                        message: "expected `&&`, found single `&`".to_string(),
                        line,
                        col,
                    });
                }
            }
            b'|' => {
                if self.peek() == Some(b'|') {
                    self.advance();
                    TokenKind::Or
                } else {
                    return Err(VclError::LexError {
                        message: "expected `||`, found single `|`".to_string(),
                        line,
                        col,
                    });
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::Gte
                } else {
                    TokenKind::Gt
                }
            }
            b'<' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    TokenKind::Lte
                } else {
                    TokenKind::Lt
                }
            }

            b'"' => {
                let s = self.read_string()?;
                TokenKind::StringLit(s)
            }

            ch if ch.is_ascii_digit() => self.read_number(ch)?,

            ch if ch.is_ascii_alphabetic() || ch == b'_' => self.read_ident(ch),

            other => {
                return Err(VclError::LexError {
                    message: format!("unexpected character: {:?}", other as char),
                    line,
                    col,
                });
            }
        };

        Ok(Token::new(kind, line, col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic_keywords() {
        let tokens = Lexer::tokenize("vcl backend sub if else return").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Vcl);
        assert_eq!(tokens[1].kind, TokenKind::Backend);
        assert_eq!(tokens[2].kind, TokenKind::Sub);
        assert_eq!(tokens[3].kind, TokenKind::If);
        assert_eq!(tokens[4].kind, TokenKind::Else);
        assert_eq!(tokens[5].kind, TokenKind::Return);
        assert_eq!(tokens[6].kind, TokenKind::Eof);
    }

    #[test]
    fn test_tokenize_operators() {
        let tokens = Lexer::tokenize("== != >= <= && || !~ += -=").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Eq);
        assert_eq!(tokens[1].kind, TokenKind::Neq);
        assert_eq!(tokens[2].kind, TokenKind::Gte);
        assert_eq!(tokens[3].kind, TokenKind::Lte);
        assert_eq!(tokens[4].kind, TokenKind::And);
        assert_eq!(tokens[5].kind, TokenKind::Or);
        assert_eq!(tokens[6].kind, TokenKind::NotMatch);
        assert_eq!(tokens[7].kind, TokenKind::PlusAssign);
        assert_eq!(tokens[8].kind, TokenKind::MinusAssign);
    }

    #[test]
    fn test_tokenize_string_literal() {
        let tokens = Lexer::tokenize(r#""hello world""#).unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLit("hello world".to_string()));
    }

    #[test]
    fn test_tokenize_long_string() {
        let tokens = Lexer::tokenize(r#"{"multi
line
string"}"#).unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::StringLit("multi\nline\nstring".to_string())
        );
    }

    #[test]
    fn test_tokenize_integers_and_reals() {
        let tokens = Lexer::tokenize("42 3.14").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLit(42));
        assert_eq!(tokens[1].kind, TokenKind::RealLit(3.14));
    }

    #[test]
    fn test_tokenize_duration_literals() {
        let tokens = Lexer::tokenize("120s 5m 1h 2d 500ms").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::DurationLit(120.0));
        assert_eq!(tokens[1].kind, TokenKind::DurationLit(300.0));
        assert_eq!(tokens[2].kind, TokenKind::DurationLit(3600.0));
        assert_eq!(tokens[3].kind, TokenKind::DurationLit(172800.0));
        assert_eq!(tokens[4].kind, TokenKind::DurationLit(0.5));
    }

    #[test]
    fn test_tokenize_booleans() {
        let tokens = Lexer::tokenize("true false").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::BoolLit(true));
        assert_eq!(tokens[1].kind, TokenKind::BoolLit(false));
    }

    #[test]
    fn test_skip_line_comment() {
        let tokens = Lexer::tokenize("vcl // this is a comment\nbackend").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Vcl);
        assert_eq!(tokens[1].kind, TokenKind::Backend);
    }

    #[test]
    fn test_skip_block_comment() {
        let tokens = Lexer::tokenize("vcl /* block comment */ backend").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Vcl);
        assert_eq!(tokens[1].kind, TokenKind::Backend);
    }

    #[test]
    fn test_skip_hash_comment() {
        let tokens = Lexer::tokenize("vcl # hash comment\nbackend").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Vcl);
        assert_eq!(tokens[1].kind, TokenKind::Backend);
    }

    #[test]
    fn test_line_and_column_tracking() {
        let tokens = Lexer::tokenize("vcl\nbackend").unwrap();
        assert_eq!(tokens[0].line, 1);
        assert_eq!(tokens[0].col, 1);
        assert_eq!(tokens[1].line, 2);
        assert_eq!(tokens[1].col, 1);
    }

    #[test]
    fn test_tokenize_vcl_snippet() {
        let src = r#"
vcl 4.0;

backend default {
    .host = "127.0.0.1";
    .port = "8080";
}

sub vcl_recv {
    if (req.url ~ "^/api") {
        return(pass);
    }
    set req.http.X-Custom = "value";
}
"#;
        let tokens = Lexer::tokenize(src).unwrap();
        // Should not fail and should end with Eof
        assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);

        // Verify a few key tokens
        assert_eq!(tokens[0].kind, TokenKind::Vcl);
        // "4.0" is parsed as RealLit
        assert_eq!(tokens[1].kind, TokenKind::RealLit(4.0));
        assert_eq!(tokens[2].kind, TokenKind::Semicolon);
        assert_eq!(tokens[3].kind, TokenKind::Backend);
    }

    #[test]
    fn test_unterminated_string_error() {
        let result = Lexer::tokenize(r#""unterminated"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_unterminated_block_comment_error() {
        let result = Lexer::tokenize("/* unterminated");
        assert!(result.is_err());
    }
}
