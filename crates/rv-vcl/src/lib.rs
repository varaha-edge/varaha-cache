//! VCL lexer, parser, bytecode compiler, and interpreter.
//!
//! This crate provides a complete frontend for Varnish Configuration Language (VCL):
//!
//! - **Lexer** (`lexer::Lexer`): Tokenizes VCL source text into a stream of tokens,
//!   handling comments, string literals (including long strings), duration literals,
//!   and all VCL operators and delimiters.
//!
//! - **Token** (`token::Token`, `token::TokenKind`): The token types produced by the lexer.
//!
//! - **AST** (`ast`): Typed abstract syntax tree nodes representing the full VCL grammar
//!   including backends, probes, ACLs, subroutines, and all statement/expression forms.
//!
//! - **Parser** (`parser::Parser`): A recursive-descent parser that builds a typed AST
//!   from the token stream, with correct operator precedence.

pub mod ast;
pub mod error;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod resolver;
pub mod token;

pub use ast::{
    AclDecl, AclEntry, AstNode, BackendDecl, BinOp, Expr, ImportDecl, ProbeDecl, SetOp, Statement,
    SubDecl, UnaryOp, VclProgram,
};
pub use error::VclError;
pub use interpreter::{VclContext, VclExecResult, VclInterpreter};
pub use lexer::Lexer;
pub use parser::Parser;
pub use resolver::FunctionResolver;
pub use rv_types::VclValue;
pub use token::{Token, TokenKind};
