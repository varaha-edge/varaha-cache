/// Token kinds for the VCL lexer.
///
/// Covers all VCL language constructs: literals, keywords, operators, and delimiters.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Ident(String),
    StringLit(String),
    IntLit(i64),
    RealLit(f64),
    DurationLit(f64),
    BoolLit(bool),

    // Keywords
    Vcl,
    Backend,
    Probe,
    Acl,
    Sub,
    Import,
    If,
    ElseIf,
    Else,
    Return,
    Set,
    Unset,
    Call,
    New,
    Include,
    Synthetic,
    Ban,

    // Operators
    Eq,
    Neq,
    Match,
    NotMatch,
    And,
    Or,
    Not,
    Plus,
    Minus,
    Star,
    Slash,
    Assign,
    PlusAssign,
    MinusAssign,
    Gt,
    Lt,
    Gte,
    Lte,

    // Delimiters
    LBrace,
    RBrace,
    LParen,
    RParen,
    Semicolon,
    Comma,
    Dot,

    // Special
    Eof,
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenKind::Ident(s) => write!(f, "identifier `{s}`"),
            TokenKind::StringLit(s) => write!(f, "string \"{s}\""),
            TokenKind::IntLit(v) => write!(f, "integer {v}"),
            TokenKind::RealLit(v) => write!(f, "real {v}"),
            TokenKind::DurationLit(v) => write!(f, "duration {v}s"),
            TokenKind::BoolLit(v) => write!(f, "bool {v}"),
            TokenKind::Vcl => write!(f, "vcl"),
            TokenKind::Backend => write!(f, "backend"),
            TokenKind::Probe => write!(f, "probe"),
            TokenKind::Acl => write!(f, "acl"),
            TokenKind::Sub => write!(f, "sub"),
            TokenKind::Import => write!(f, "import"),
            TokenKind::If => write!(f, "if"),
            TokenKind::ElseIf => write!(f, "elseif"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::Return => write!(f, "return"),
            TokenKind::Set => write!(f, "set"),
            TokenKind::Unset => write!(f, "unset"),
            TokenKind::Call => write!(f, "call"),
            TokenKind::New => write!(f, "new"),
            TokenKind::Include => write!(f, "include"),
            TokenKind::Synthetic => write!(f, "synthetic"),
            TokenKind::Ban => write!(f, "ban"),
            TokenKind::Eq => write!(f, "=="),
            TokenKind::Neq => write!(f, "!="),
            TokenKind::Match => write!(f, "~"),
            TokenKind::NotMatch => write!(f, "!~"),
            TokenKind::And => write!(f, "&&"),
            TokenKind::Or => write!(f, "||"),
            TokenKind::Not => write!(f, "!"),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Assign => write!(f, "="),
            TokenKind::PlusAssign => write!(f, "+="),
            TokenKind::MinusAssign => write!(f, "-="),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gte => write!(f, ">="),
            TokenKind::Lte => write!(f, "<="),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Dot => write!(f, "."),
            TokenKind::Eof => write!(f, "EOF"),
        }
    }
}

/// A token produced by the VCL lexer, including source location information.
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub col: usize,
}

impl Token {
    pub fn new(kind: TokenKind, line: usize, col: usize) -> Self {
        Self { kind, line, col }
    }
}
