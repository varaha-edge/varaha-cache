use crate::ast::*;
use crate::error::VclError;
use crate::token::{Token, TokenKind};

/// Recursive-descent parser for VCL.
///
/// Consumes a token stream produced by the lexer and builds a typed AST.
/// Operator precedence (lowest to highest):
///   `||` < `&&` < comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`, `~`, `!~`)
///   < additive (`+`, `-`) < multiplicative (`*`, `/`) < unary (`!`, `-`)
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Parse a complete VCL program from the token stream.
    pub fn parse(tokens: &[Token]) -> Result<VclProgram, VclError> {
        let mut parser = Parser::new(tokens);
        parser.parse_program()
    }

    // -----------------------------------------------------------------------
    // Token navigation helpers
    // -----------------------------------------------------------------------

    fn current(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek(&self) -> &TokenKind {
        &self.current().kind
    }

    fn peek_ahead(&self, offset: usize) -> &TokenKind {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn advance(&mut self) -> &Token {
        let idx = self.pos;
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        &self.tokens[idx]
    }

    #[allow(dead_code)]
    fn expect(&mut self, expected: &TokenKind) -> Result<&Token, VclError> {
        let tok = self.current();
        if std::mem::discriminant(&tok.kind) == std::mem::discriminant(expected) {
            Ok(self.advance())
        } else {
            Err(self.error(format!("expected {expected}, found {}", tok.kind)))
        }
    }

    fn expect_kind(&mut self, expected: TokenKind) -> Result<(), VclError> {
        let tok = self.current();
        if tok.kind == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected {expected}, found {}", tok.kind)))
        }
    }

    #[allow(dead_code)]
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_exact(&mut self, kind: TokenKind) -> bool {
        if self.current().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&self, message: String) -> VclError {
        let tok = self.current();
        VclError::ParseError {
            message,
            line: tok.line,
            col: tok.col,
        }
    }

    fn expect_ident(&mut self) -> Result<String, VclError> {
        let tok = self.current();
        if let TokenKind::Ident(name) = &tok.kind {
            let name = name.clone();
            self.advance();
            Ok(name)
        } else {
            Err(self.error(format!("expected identifier, found {}", tok.kind)))
        }
    }

    fn expect_semicolon(&mut self) -> Result<(), VclError> {
        self.expect_kind(TokenKind::Semicolon)
    }

    // -----------------------------------------------------------------------
    // Program-level parsing
    // -----------------------------------------------------------------------

    fn parse_program(&mut self) -> Result<VclProgram, VclError> {
        let mut program = VclProgram {
            version: None,
            imports: Vec::new(),
            backends: Vec::new(),
            probes: Vec::new(),
            acls: Vec::new(),
            subs: Vec::new(),
        };

        // Optional version declaration
        if *self.peek() == TokenKind::Vcl {
            program.version = Some(self.parse_vcl_version()?);
        }

        // Parse top-level declarations
        loop {
            match self.peek() {
                TokenKind::Eof => break,
                TokenKind::Import => {
                    program.imports.push(self.parse_import()?);
                }
                TokenKind::Backend => {
                    program.backends.push(self.parse_backend()?);
                }
                TokenKind::Probe => {
                    program.probes.push(self.parse_probe_decl()?);
                }
                TokenKind::Acl => {
                    program.acls.push(self.parse_acl()?);
                }
                TokenKind::Sub => {
                    program.subs.push(self.parse_sub()?);
                }
                _ => {
                    return Err(self.error(format!(
                        "unexpected token at top level: {}",
                        self.current().kind
                    )));
                }
            }
        }

        Ok(program)
    }

    /// Parse `vcl 4.0;` or `vcl 4.1;`
    fn parse_vcl_version(&mut self) -> Result<String, VclError> {
        self.expect_kind(TokenKind::Vcl)?;
        let tok = self.current();
        let version = match &tok.kind {
            TokenKind::RealLit(v) => format!("{v}"),
            TokenKind::IntLit(v) => format!("{v}.0"),
            _ => return Err(self.error(format!("expected version number, found {}", tok.kind))),
        };
        self.advance();
        self.expect_semicolon()?;
        Ok(version)
    }

    /// Parse `import std;`
    fn parse_import(&mut self) -> Result<ImportDecl, VclError> {
        self.expect_kind(TokenKind::Import)?;
        let name = self.expect_ident()?;
        self.expect_semicolon()?;
        Ok(ImportDecl { name })
    }

    // -----------------------------------------------------------------------
    // Backend
    // -----------------------------------------------------------------------

    /// Parse a backend declaration.
    ///
    /// ```vcl
    /// backend name {
    ///     .host = "...";
    ///     .port = "...";
    ///     .probe = { ... }
    /// }
    /// ```
    fn parse_backend(&mut self) -> Result<BackendDecl, VclError> {
        self.expect_kind(TokenKind::Backend)?;
        let name = self.expect_ident()?;
        self.expect_kind(TokenKind::LBrace)?;

        let mut properties = Vec::new();
        let mut probe = None;

        while *self.peek() != TokenKind::RBrace {
            self.expect_kind(TokenKind::Dot)?;

            // `.probe` is special: `probe` is a keyword, not an identifier
            if *self.peek() == TokenKind::Probe {
                self.advance(); // consume `probe`
                self.expect_kind(TokenKind::Assign)?;
                // Inline probe: `{ ... }` or named reference
                if *self.peek() == TokenKind::LBrace {
                    probe = Some(self.parse_probe_body(None)?);
                } else {
                    // Named probe reference
                    let probe_name = self.expect_ident()?;
                    self.expect_semicolon()?;
                    probe = Some(ProbeDecl {
                        name: Some(probe_name),
                        properties: Vec::new(),
                    });
                }
            } else {
                let prop_name = self.expect_ident()?;
                self.expect_kind(TokenKind::Assign)?;
                let value = self.parse_expr()?;
                self.expect_semicolon()?;
                properties.push((prop_name, value));
            }
        }

        self.expect_kind(TokenKind::RBrace)?;
        Ok(BackendDecl {
            name,
            properties,
            probe,
        })
    }

    // -----------------------------------------------------------------------
    // Probe
    // -----------------------------------------------------------------------

    /// Parse a top-level `probe name { ... }` declaration.
    fn parse_probe_decl(&mut self) -> Result<ProbeDecl, VclError> {
        self.expect_kind(TokenKind::Probe)?;
        let name = self.expect_ident()?;
        self.parse_probe_body(Some(name))
    }

    /// Parse the probe body `{ .url = "/"; .interval = 5s; ... }`.
    fn parse_probe_body(&mut self, name: Option<String>) -> Result<ProbeDecl, VclError> {
        self.expect_kind(TokenKind::LBrace)?;

        let mut properties = Vec::new();
        while *self.peek() != TokenKind::RBrace {
            self.expect_kind(TokenKind::Dot)?;
            let prop_name = self.expect_ident()?;
            self.expect_kind(TokenKind::Assign)?;
            let value = self.parse_expr()?;
            self.expect_semicolon()?;
            properties.push((prop_name, value));
        }

        self.expect_kind(TokenKind::RBrace)?;
        Ok(ProbeDecl { name, properties })
    }

    // -----------------------------------------------------------------------
    // ACL
    // -----------------------------------------------------------------------

    /// Parse an ACL declaration.
    ///
    /// ```vcl
    /// acl local {
    ///     "192.168.0.0"/24;
    ///     !"10.0.0.1";
    /// }
    /// ```
    fn parse_acl(&mut self) -> Result<AclDecl, VclError> {
        self.expect_kind(TokenKind::Acl)?;
        let name = self.expect_ident()?;
        self.expect_kind(TokenKind::LBrace)?;

        let mut entries = Vec::new();
        while *self.peek() != TokenKind::RBrace {
            let negate = self.eat_exact(TokenKind::Not);
            let tok = self.current();
            let addr = if let TokenKind::StringLit(s) = &tok.kind {
                let s = s.clone();
                self.advance();
                s
            } else {
                return Err(self.error(format!(
                    "expected string address in ACL, found {}",
                    tok.kind
                )));
            };

            let mask = if *self.peek() == TokenKind::Slash {
                self.advance();
                let tok = self.current();
                if let TokenKind::IntLit(v) = &tok.kind {
                    let v = *v as u8;
                    self.advance();
                    Some(v)
                } else {
                    return Err(
                        self.error(format!("expected mask length, found {}", tok.kind))
                    );
                }
            } else {
                None
            };

            self.expect_semicolon()?;
            entries.push(AclEntry { negate, addr, mask });
        }

        self.expect_kind(TokenKind::RBrace)?;
        Ok(AclDecl { name, entries })
    }

    // -----------------------------------------------------------------------
    // Sub
    // -----------------------------------------------------------------------

    /// Parse a subroutine declaration.
    fn parse_sub(&mut self) -> Result<SubDecl, VclError> {
        self.expect_kind(TokenKind::Sub)?;
        let name = self.expect_ident()?;
        self.expect_kind(TokenKind::LBrace)?;

        let body = self.parse_statement_block()?;

        self.expect_kind(TokenKind::RBrace)?;
        Ok(SubDecl { name, body })
    }

    /// Parse a sequence of statements until we encounter `}`.
    fn parse_statement_block(&mut self) -> Result<Vec<Statement>, VclError> {
        let mut stmts = Vec::new();
        while *self.peek() != TokenKind::RBrace && *self.peek() != TokenKind::Eof {
            stmts.push(self.parse_statement()?);
        }
        Ok(stmts)
    }

    // -----------------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------------

    fn parse_statement(&mut self) -> Result<Statement, VclError> {
        match self.peek().clone() {
            TokenKind::Set => self.parse_set(),
            TokenKind::Unset => self.parse_unset(),
            TokenKind::Call => self.parse_call(),
            TokenKind::Return => self.parse_return(),
            TokenKind::If => self.parse_if(),
            TokenKind::Synthetic => self.parse_synthetic(),
            TokenKind::New => self.parse_new(),
            TokenKind::Ban => self.parse_ban(),
            _ => {
                // Expression statement (e.g. function call)
                let expr = self.parse_expr()?;
                self.expect_semicolon()?;
                Ok(Statement::ExprStatement(expr))
            }
        }
    }

    /// Parse `set target op value;`
    fn parse_set(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Set)?;

        let target = self.parse_dotted_name()?;

        let operator = match self.peek() {
            TokenKind::Assign => {
                self.advance();
                SetOp::Assign
            }
            TokenKind::PlusAssign => {
                self.advance();
                SetOp::Add
            }
            TokenKind::MinusAssign => {
                self.advance();
                SetOp::Subtract
            }
            _ => {
                return Err(self.error(format!(
                    "expected assignment operator, found {}",
                    self.current().kind
                )));
            }
        };

        let value = self.parse_expr()?;
        self.expect_semicolon()?;

        Ok(Statement::Set {
            target,
            operator,
            value,
        })
    }

    /// Parse `unset target;`
    fn parse_unset(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Unset)?;
        let target = self.parse_dotted_name()?;
        self.expect_semicolon()?;
        Ok(Statement::Unset { target })
    }

    /// Parse `call subroutine_name;`
    fn parse_call(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Call)?;
        let subroutine = self.expect_ident()?;
        self.expect_semicolon()?;
        Ok(Statement::Call { subroutine })
    }

    /// Parse `return(action);` or `return(action(args));`
    fn parse_return(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Return)?;
        self.expect_kind(TokenKind::LParen)?;
        let action = self.parse_expr()?;
        self.expect_kind(TokenKind::RParen)?;
        self.expect_semicolon()?;
        Ok(Statement::Return { action })
    }

    /// Parse `if (cond) { ... } elsif (cond) { ... } else { ... }`
    fn parse_if(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::If)?;
        self.expect_kind(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect_kind(TokenKind::RParen)?;

        self.expect_kind(TokenKind::LBrace)?;
        let body = self.parse_statement_block()?;
        self.expect_kind(TokenKind::RBrace)?;

        let mut else_ifs = Vec::new();
        let mut else_body = None;

        loop {
            if self.eat_exact(TokenKind::ElseIf) {
                self.expect_kind(TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect_kind(TokenKind::RParen)?;
                self.expect_kind(TokenKind::LBrace)?;
                let block = self.parse_statement_block()?;
                self.expect_kind(TokenKind::RBrace)?;
                else_ifs.push((cond, block));
            } else if *self.peek() == TokenKind::Else {
                // Could be `else if` (two tokens) or plain `else`
                if *self.peek_ahead(1) == TokenKind::If {
                    self.advance(); // consume `else`
                    self.advance(); // consume `if`
                    self.expect_kind(TokenKind::LParen)?;
                    let cond = self.parse_expr()?;
                    self.expect_kind(TokenKind::RParen)?;
                    self.expect_kind(TokenKind::LBrace)?;
                    let block = self.parse_statement_block()?;
                    self.expect_kind(TokenKind::RBrace)?;
                    else_ifs.push((cond, block));
                } else {
                    self.advance(); // consume `else`
                    self.expect_kind(TokenKind::LBrace)?;
                    let block = self.parse_statement_block()?;
                    self.expect_kind(TokenKind::RBrace)?;
                    else_body = Some(block);
                    break;
                }
            } else {
                break;
            }
        }

        Ok(Statement::If {
            condition,
            body,
            else_ifs,
            else_body,
        })
    }

    /// Parse `synthetic(expr);`
    fn parse_synthetic(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Synthetic)?;
        self.expect_kind(TokenKind::LParen)?;
        let expr = self.parse_expr()?;
        self.expect_kind(TokenKind::RParen)?;
        self.expect_semicolon()?;
        Ok(Statement::Synthetic { expr })
    }

    /// Parse `new name = type(args);`
    fn parse_new(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::New)?;
        let name = self.expect_ident()?;
        self.expect_kind(TokenKind::Assign)?;
        let type_name = self.expect_ident()?;
        self.expect_kind(TokenKind::LParen)?;

        let mut args = Vec::new();
        if *self.peek() != TokenKind::RParen {
            args.push(self.parse_expr()?);
            while self.eat_exact(TokenKind::Comma) {
                args.push(self.parse_expr()?);
            }
        }

        self.expect_kind(TokenKind::RParen)?;
        self.expect_semicolon()?;

        Ok(Statement::New {
            name,
            type_name,
            args,
        })
    }

    /// Parse `ban(expr);`
    fn parse_ban(&mut self) -> Result<Statement, VclError> {
        self.expect_kind(TokenKind::Ban)?;
        self.expect_kind(TokenKind::LParen)?;
        let expr = self.parse_expr()?;
        self.expect_kind(TokenKind::RParen)?;
        self.expect_semicolon()?;
        Ok(Statement::Ban { expr })
    }

    // -----------------------------------------------------------------------
    // Expressions (operator precedence climbing)
    // -----------------------------------------------------------------------

    /// Top-level expression entry point.
    pub fn parse_expr(&mut self) -> Result<Expr, VclError> {
        self.parse_or()
    }

    /// `||` (lowest precedence)
    fn parse_or(&mut self) -> Result<Expr, VclError> {
        let mut left = self.parse_and()?;
        while *self.peek() == TokenKind::Or {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// `&&`
    fn parse_and(&mut self) -> Result<Expr, VclError> {
        let mut left = self.parse_comparison()?;
        while *self.peek() == TokenKind::And {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// `==`, `!=`, `<`, `>`, `<=`, `>=`, `~`, `!~`
    fn parse_comparison(&mut self) -> Result<Expr, VclError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                TokenKind::Eq => BinOp::Eq,
                TokenKind::Neq => BinOp::Neq,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gte => BinOp::Gte,
                TokenKind::Lte => BinOp::Lte,
                TokenKind::Match => BinOp::Match,
                TokenKind::NotMatch => BinOp::NotMatch,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// `+`, `-`
    fn parse_additive(&mut self) -> Result<Expr, VclError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// `*`, `/`
    fn parse_multiplicative(&mut self) -> Result<Expr, VclError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// Unary `!`, `-`
    fn parse_unary(&mut self) -> Result<Expr, VclError> {
        match self.peek() {
            TokenKind::Not => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                })
            }
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Negate,
                    operand: Box::new(operand),
                })
            }
            _ => self.parse_postfix(),
        }
    }

    /// Postfix: method calls `expr.method(args)` and further dot access.
    fn parse_postfix(&mut self) -> Result<Expr, VclError> {
        let mut expr = self.parse_primary()?;

        loop {
            if *self.peek() == TokenKind::LParen {
                // Function call on an identifier or variable
                match &expr {
                    Expr::Ident(name) => {
                        let name = name.clone();
                        self.advance(); // consume `(`
                        let args = self.parse_arg_list()?;
                        self.expect_kind(TokenKind::RParen)?;
                        expr = Expr::FunctionCall { name, args };
                    }
                    Expr::Variable(name) => {
                        // e.g. `obj.method(args)` -- treat last segment as method
                        let name = name.clone();
                        self.advance(); // consume `(`
                        let args = self.parse_arg_list()?;
                        self.expect_kind(TokenKind::RParen)?;
                        // Split into object.method
                        if let Some(dot_pos) = name.rfind('.') {
                            let object_part = &name[..dot_pos];
                            let method_part = &name[dot_pos + 1..];
                            expr = Expr::MethodCall {
                                object: Box::new(Expr::Variable(object_part.to_string())),
                                method: method_part.to_string(),
                                args,
                            };
                        } else {
                            expr = Expr::FunctionCall { name, args };
                        }
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, VclError> {
        let mut args = Vec::new();
        if *self.peek() != TokenKind::RParen {
            args.push(self.parse_expr()?);
            while self.eat_exact(TokenKind::Comma) {
                args.push(self.parse_expr()?);
            }
        }
        Ok(args)
    }

    /// Primary expressions: literals, variables, identifiers, function calls,
    /// parenthesized expressions.
    fn parse_primary(&mut self) -> Result<Expr, VclError> {
        let tok = self.current();
        match &tok.kind {
            TokenKind::StringLit(s) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::StringLit(s))
            }
            TokenKind::IntLit(v) => {
                let v = *v;
                self.advance();
                Ok(Expr::IntLit(v))
            }
            TokenKind::RealLit(v) => {
                let v = *v;
                self.advance();
                Ok(Expr::RealLit(v))
            }
            TokenKind::DurationLit(v) => {
                let v = *v;
                self.advance();
                Ok(Expr::DurationLit(v))
            }
            TokenKind::BoolLit(v) => {
                let v = *v;
                self.advance();
                Ok(Expr::BoolLit(v))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect_kind(TokenKind::RParen)?;
                Ok(expr)
            }
            TokenKind::Ident(_) => {
                let name = self.expect_ident()?;
                // Check for dotted access (variables / headers)
                if *self.peek() == TokenKind::Dot {
                    self.parse_dotted_expr(name)
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            _ => Err(self.error(format!("unexpected token in expression: {}", tok.kind))),
        }
    }

    /// After reading an identifier and seeing a dot, continue parsing a dotted name
    /// and determine if it is a Variable or HeaderRef.
    fn parse_dotted_expr(&mut self, first: String) -> Result<Expr, VclError> {
        let mut parts = vec![first.clone()];

        while *self.peek() == TokenKind::Dot {
            self.advance(); // consume `.`
            let part = self.expect_ident()?;
            parts.push(part);
        }

        let full = parts.join(".");

        // Detect header references: `req.http.X-Something`, `bereq.http.Host`, etc.
        if parts.len() >= 3 && parts[1] == "http" {
            let object = parts[0].clone();
            let header_name = parts[2..].join(".");
            return Ok(Expr::HeaderRef(object, header_name));
        }

        // Everything else is a variable reference
        Ok(Expr::Variable(full))
    }

    /// Parse a dotted name (used for set/unset targets), returning the full string.
    fn parse_dotted_name(&mut self) -> Result<String, VclError> {
        let first = self.expect_ident()?;
        let mut name = first;

        while *self.peek() == TokenKind::Dot {
            self.advance();
            let part = self.expect_ident()?;
            name.push('.');
            name.push_str(&part);
        }

        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_vcl(src: &str) -> VclProgram {
        let tokens = Lexer::tokenize(src).expect("lexer failed");
        Parser::parse(&tokens).expect("parser failed")
    }

    #[test]
    fn test_parse_version() {
        let prog = parse_vcl("vcl 4.0;");
        assert_eq!(prog.version.as_deref(), Some("4"));
    }

    #[test]
    fn test_parse_import() {
        let prog = parse_vcl("vcl 4.0; import std;");
        assert_eq!(prog.imports.len(), 1);
        assert_eq!(prog.imports[0].name, "std");
    }

    #[test]
    fn test_parse_simple_backend() {
        let prog = parse_vcl(
            r#"
            vcl 4.0;
            backend default {
                .host = "127.0.0.1";
                .port = "8080";
            }
            "#,
        );
        assert_eq!(prog.backends.len(), 1);
        let be = &prog.backends[0];
        assert_eq!(be.name, "default");
        assert_eq!(be.properties.len(), 2);
        assert_eq!(be.properties[0].0, "host");
        assert_eq!(be.properties[1].0, "port");
    }

    #[test]
    fn test_parse_backend_with_probe() {
        let prog = parse_vcl(
            r#"
            backend default {
                .host = "127.0.0.1";
                .probe = {
                    .url = "/health";
                    .interval = 5s;
                    .timeout = 1s;
                }
            }
            "#,
        );
        let be = &prog.backends[0];
        assert!(be.probe.is_some());
        let probe = be.probe.as_ref().unwrap();
        assert_eq!(probe.properties.len(), 3);
    }

    #[test]
    fn test_parse_acl() {
        let prog = parse_vcl(
            r#"
            acl local {
                "192.168.0.0"/24;
                !"10.0.0.1";
                "127.0.0.1";
            }
            "#,
        );
        assert_eq!(prog.acls.len(), 1);
        let acl = &prog.acls[0];
        assert_eq!(acl.name, "local");
        assert_eq!(acl.entries.len(), 3);
        assert!(!acl.entries[0].negate);
        assert_eq!(acl.entries[0].mask, Some(24));
        assert!(acl.entries[1].negate);
        assert!(!acl.entries[2].negate);
    }

    #[test]
    fn test_parse_sub_with_if_else() {
        let prog = parse_vcl(
            r#"
            sub vcl_recv {
                if (req.url == "/health") {
                    return(synth(200));
                } else if (req.url ~ "^/api") {
                    return(pass);
                } else {
                    return(hash);
                }
            }
            "#,
        );
        assert_eq!(prog.subs.len(), 1);
        let sub = &prog.subs[0];
        assert_eq!(sub.name, "vcl_recv");
        assert_eq!(sub.body.len(), 1);
        match &sub.body[0] {
            Statement::If {
                else_ifs,
                else_body,
                ..
            } => {
                assert_eq!(else_ifs.len(), 1);
                assert!(else_body.is_some());
            }
            other => panic!("expected If statement, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_set_unset() {
        let prog = parse_vcl(
            r#"
            sub vcl_recv {
                set req.http.X-Forwarded-For = "1.2.3.4";
                unset req.http.Cookie;
            }
            "#,
        );
        let sub = &prog.subs[0];
        assert_eq!(sub.body.len(), 2);
        match &sub.body[0] {
            Statement::Set {
                target,
                operator,
                value,
            } => {
                assert_eq!(target, "req.http.X-Forwarded-For");
                assert_eq!(*operator, SetOp::Assign);
                match value {
                    Expr::StringLit(s) => assert_eq!(s, "1.2.3.4"),
                    other => panic!("expected StringLit, got {other:?}"),
                }
            }
            other => panic!("expected Set, got {other:?}"),
        }
        match &sub.body[1] {
            Statement::Unset { target } => {
                assert_eq!(target, "req.http.Cookie");
            }
            other => panic!("expected Unset, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_set_plus_assign() {
        let prog = parse_vcl(
            r#"
            sub vcl_recv {
                set req.http.X-Forwarded-For += ", " + "1.2.3.4";
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::Set { operator, .. } => {
                assert_eq!(*operator, SetOp::Add);
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_operator_precedence() {
        // `a + b * c` should parse as `a + (b * c)`
        let prog = parse_vcl(
            r#"
            sub test {
                set req.http.X-Val = 1 + 2 * 3;
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::Set { value, .. } => match value {
                Expr::BinaryOp { op, right, .. } => {
                    assert_eq!(*op, BinOp::Add);
                    match right.as_ref() {
                        Expr::BinaryOp { op, .. } => {
                            assert_eq!(*op, BinOp::Mul);
                        }
                        other => panic!("expected BinaryOp(Mul), got {other:?}"),
                    }
                }
                other => panic!("expected BinaryOp(Add), got {other:?}"),
            },
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_logical_precedence() {
        // `a || b && c` should parse as `a || (b && c)`
        let prog = parse_vcl(
            r#"
            sub test {
                if (true || false && true) {
                    return(pass);
                }
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::If { condition, .. } => match condition {
                Expr::BinaryOp { op, right, .. } => {
                    assert_eq!(*op, BinOp::Or);
                    match right.as_ref() {
                        Expr::BinaryOp { op, .. } => {
                            assert_eq!(*op, BinOp::And);
                        }
                        other => panic!("expected BinaryOp(And), got {other:?}"),
                    }
                }
                other => panic!("expected BinaryOp(Or), got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_unary_not() {
        let prog = parse_vcl(
            r#"
            sub test {
                if (!req.is-ssl) {
                    return(synth(301));
                }
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::If { condition, .. } => match condition {
                Expr::UnaryOp { op, .. } => {
                    assert_eq!(*op, UnaryOp::Not);
                }
                other => panic!("expected UnaryOp(Not), got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_function_call() {
        let prog = parse_vcl(
            r#"
            sub test {
                return(synth(200, "OK"));
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::Return { action } => match action {
                Expr::FunctionCall { name, args } => {
                    assert_eq!(name, "synth");
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected FunctionCall, got {other:?}"),
            },
            other => panic!("expected Return, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_header_ref() {
        let prog = parse_vcl(
            r#"
            sub test {
                if (req.http.X-Forwarded-For == "1.2.3.4") {
                    return(pass);
                }
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::If { condition, .. } => match condition {
                Expr::BinaryOp { left, op, .. } => {
                    assert_eq!(*op, BinOp::Eq);
                    match left.as_ref() {
                        Expr::HeaderRef(obj, hdr) => {
                            assert_eq!(obj, "req");
                            assert_eq!(hdr, "X-Forwarded-For");
                        }
                        other => panic!("expected HeaderRef, got {other:?}"),
                    }
                }
                other => panic!("expected BinaryOp, got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_elsif() {
        let prog = parse_vcl(
            r#"
            sub test {
                if (req.url == "/a") {
                    return(pass);
                } elsif (req.url == "/b") {
                    return(hash);
                }
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::If { else_ifs, .. } => {
                assert_eq!(else_ifs.len(), 1);
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_regex_match() {
        let prog = parse_vcl(
            r#"
            sub test {
                if (req.url ~ "^/api/v[0-9]+") {
                    return(pass);
                }
            }
            "#,
        );
        let sub = &prog.subs[0];
        match &sub.body[0] {
            Statement::If { condition, .. } => match condition {
                Expr::BinaryOp { op, right, .. } => {
                    assert_eq!(*op, BinOp::Match);
                    match right.as_ref() {
                        Expr::StringLit(s) => {
                            assert_eq!(s, "^/api/v[0-9]+");
                        }
                        other => panic!("expected StringLit for regex, got {other:?}"),
                    }
                }
                other => panic!("expected BinaryOp(Match), got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_full_vcl() {
        let src = r#"
vcl 4.0;

import std;

probe healthcheck {
    .url = "/health";
    .interval = 5s;
    .timeout = 1s;
    .window = 5;
    .threshold = 3;
}

backend default {
    .host = "127.0.0.1";
    .port = "8080";
    .probe = healthcheck;
}

acl purge_acl {
    "127.0.0.1";
    "192.168.0.0"/24;
}

sub vcl_recv {
    if (req.method == "PURGE") {
        if (req.http.X-Purge-Token != "secret") {
            return(synth(403, "Forbidden"));
        }
        return(purge);
    }

    if (req.url ~ "^/static/") {
        unset req.http.Cookie;
        return(hash);
    }

    if (req.http.Accept-Encoding) {
        if (req.url ~ "\.(jpg|png|gif|gz|tgz|bz2|tbz|mp3|ogg)$") {
            unset req.http.Accept-Encoding;
        }
    }
}

sub vcl_deliver {
    set resp.http.X-Cache = "HIT";
    unset resp.http.X-Varnish;
}
        "#;

        let prog = parse_vcl(src);
        assert_eq!(prog.version.as_deref(), Some("4"));
        assert_eq!(prog.imports.len(), 1);
        assert_eq!(prog.probes.len(), 1);
        assert_eq!(prog.backends.len(), 1);
        assert_eq!(prog.acls.len(), 1);
        assert_eq!(prog.subs.len(), 2);
    }
}
