/// Top-level AST node wrapping a parsed VCL program.
#[derive(Debug, Clone)]
pub enum AstNode {
    Program(VclProgram),
}

/// A complete VCL program consisting of version declaration, imports, backends,
/// probes, ACLs, and subroutines.
#[derive(Debug, Clone)]
pub struct VclProgram {
    pub version: Option<String>,
    pub imports: Vec<ImportDecl>,
    pub backends: Vec<BackendDecl>,
    pub probes: Vec<ProbeDecl>,
    pub acls: Vec<AclDecl>,
    pub subs: Vec<SubDecl>,
}

/// An import declaration: `import std;`
#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub name: String,
}

/// A backend declaration with optional inline probe.
///
/// ```vcl
/// backend default {
///     .host = "127.0.0.1";
///     .port = "8080";
///     .probe = { ... }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct BackendDecl {
    pub name: String,
    pub properties: Vec<(String, Expr)>,
    pub probe: Option<ProbeDecl>,
}

/// A probe declaration, either named (top-level) or inline (within a backend).
///
/// ```vcl
/// probe healthcheck {
///     .url = "/health";
///     .interval = 5s;
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ProbeDecl {
    pub name: Option<String>,
    pub properties: Vec<(String, Expr)>,
}

/// An ACL declaration containing network address entries.
///
/// ```vcl
/// acl local {
///     "192.168.0.0"/24;
///     !"10.0.0.1";
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AclDecl {
    pub name: String,
    pub entries: Vec<AclEntry>,
}

/// A single entry within an ACL block.
#[derive(Debug, Clone)]
pub struct AclEntry {
    pub negate: bool,
    pub addr: String,
    pub mask: Option<u8>,
}

/// A subroutine declaration containing a sequence of statements.
///
/// ```vcl
/// sub vcl_recv {
///     if (req.url ~ "/api") {
///         return(pass);
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct SubDecl {
    pub name: String,
    pub body: Vec<Statement>,
}

/// A VCL statement within a subroutine body.
#[derive(Debug, Clone)]
pub enum Statement {
    Set {
        target: String,
        operator: SetOp,
        value: Expr,
    },
    Unset {
        target: String,
    },
    Call {
        subroutine: String,
    },
    Return {
        action: Expr,
    },
    If {
        condition: Expr,
        body: Vec<Statement>,
        else_ifs: Vec<(Expr, Vec<Statement>)>,
        else_body: Option<Vec<Statement>>,
    },
    Synthetic {
        expr: Expr,
    },
    New {
        name: String,
        type_name: String,
        args: Vec<Expr>,
    },
    ExprStatement(Expr),
    Ban {
        expr: Expr,
    },
}

/// The assignment operator in a `set` statement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetOp {
    Assign,
    Add,
    Subtract,
}

/// An expression node in the VCL AST.
#[derive(Debug, Clone)]
pub enum Expr {
    StringLit(String),
    IntLit(i64),
    RealLit(f64),
    DurationLit(f64),
    BoolLit(bool),
    Ident(String),
    /// A dotted variable reference like `req.url` or `beresp.ttl`.
    Variable(String),
    /// A header reference like `req.http.X-Forwarded-For`.
    /// First element is the object (e.g. "req"), second is the header name.
    HeaderRef(String, String),
    BinaryOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    FunctionCall {
        name: String,
        args: Vec<Expr>,
    },
    MethodCall {
        object: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    RegexLit(String),
    Concat(Vec<Expr>),
}

/// Binary operators used in VCL expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Match,
    NotMatch,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}

/// Unary operators used in VCL expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Not,
    Negate,
}
