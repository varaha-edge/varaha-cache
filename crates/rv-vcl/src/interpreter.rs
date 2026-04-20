use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use dashmap::DashMap;
use regex::Regex;

use rv_http::header::HeaderMap;
use rv_http::message::HttpMessage;
use rv_types::{HttpStatus, VclValue, VtimReal};

use crate::ast::{BinOp, Expr, SetOp, Statement, UnaryOp, VclProgram};

/// VCL return actions that the interpreter can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum VclAction {
    Deliver,
    Pass,
    Pipe,
    Hash,
    Lookup,
    Fetch,
    Synth,
    Purge,
    Restart,
    Retry,
    Abandon,
    Fail,
    Error,
    Ok,
}

impl VclAction {
    /// Convert to the rv_types VclAction used by the FSM.
    pub fn to_types_action(&self) -> rv_types::VclAction {
        match self {
            VclAction::Deliver => rv_types::VclAction::Deliver,
            VclAction::Pass => rv_types::VclAction::Pass,
            VclAction::Pipe => rv_types::VclAction::Pipe,
            VclAction::Hash => rv_types::VclAction::Hash,
            VclAction::Lookup => rv_types::VclAction::Lookup,
            VclAction::Fetch => rv_types::VclAction::Fetch,
            VclAction::Synth => rv_types::VclAction::Synth,
            VclAction::Purge => rv_types::VclAction::Purge,
            VclAction::Restart => rv_types::VclAction::Restart,
            VclAction::Retry => rv_types::VclAction::Retry,
            VclAction::Abandon => rv_types::VclAction::Abandon,
            VclAction::Fail => rv_types::VclAction::Fail,
            VclAction::Error => rv_types::VclAction::Error,
            VclAction::Ok => rv_types::VclAction::Ok,
        }
    }

    fn from_name(name: &str) -> Option<VclAction> {
        match name {
            "deliver" => Some(VclAction::Deliver),
            "pass" => Some(VclAction::Pass),
            "pipe" => Some(VclAction::Pipe),
            "hash" => Some(VclAction::Hash),
            "lookup" => Some(VclAction::Lookup),
            "fetch" => Some(VclAction::Fetch),
            "synth" => Some(VclAction::Synth),
            "purge" => Some(VclAction::Purge),
            "restart" => Some(VclAction::Restart),
            "retry" => Some(VclAction::Retry),
            "abandon" => Some(VclAction::Abandon),
            "fail" => Some(VclAction::Fail),
            "error" => Some(VclAction::Error),
            "ok" => Some(VclAction::Ok),
            _ => None,
        }
    }
}

/// Result of executing a VCL subroutine.
#[derive(Debug)]
pub enum VclExecResult {
    /// VCL returned an action (e.g. return(pass)).
    Action(VclAction),
    /// VCL fell through without explicit return.
    Fallthrough,
    /// An error occurred during execution.
    Error(std::string::String),
}

/// Internal control flow during block execution.
enum ControlFlow {
    Continue,
    Return(VclAction),
    Error(std::string::String),
}

/// Mutable execution context for a single VCL subroutine invocation.
pub struct VclContext<'a> {
    /// The client request.
    pub req: &'a mut HttpMessage,
    /// The response being built for the client.
    pub resp: &'a mut HttpMessage,
    /// The backend request (may be uninitialized in recv).
    pub bereq: &'a mut HttpMessage,
    /// The backend response.
    pub beresp: &'a mut HttpMessage,
    /// Connection information.
    pub client_ip: IpAddr,
    pub server_ip: IpAddr,
    pub is_ssl: bool,
    /// Restart counter.
    pub restarts: u32,
    /// Object hit count (for vcl_hit).
    pub obj_hits: i64,
    /// Object TTL/grace/keep for obj.* access.
    pub obj_ttl: f64,
    pub obj_grace: f64,
    pub obj_keep: f64,
    /// Synthetic response body.
    pub synth_body: Option<std::string::String>,
    /// Synth status code for return(synth(NNN)).
    pub synth_status: Option<u16>,
    /// Local variables set during execution.
    pub local_vars: HashMap<std::string::String, VclValue>,
    /// Whether beresp.do_gzip has been set.
    pub do_gzip: bool,
    /// Whether beresp.do_gunzip has been set.
    pub do_gunzip: bool,
    /// beresp.uncacheable flag.
    pub uncacheable: bool,
    /// Optional reference to request body bytes (read-only).
    pub req_body: Option<&'a [u8]>,
    /// Optional mutable reference to response body (for modification).
    pub resp_body: Option<&'a mut Vec<u8>>,
}

impl<'a> VclContext<'a> {
    pub fn new(
        req: &'a mut HttpMessage,
        resp: &'a mut HttpMessage,
        bereq: &'a mut HttpMessage,
        beresp: &'a mut HttpMessage,
    ) -> Self {
        Self {
            req,
            resp,
            bereq,
            beresp,
            client_ip: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            server_ip: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            is_ssl: false,
            restarts: 0,
            obj_hits: 0,
            obj_ttl: 0.0,
            obj_grace: 0.0,
            obj_keep: 0.0,
            synth_body: None,
            synth_status: None,
            local_vars: HashMap::new(),
            do_gzip: false,
            do_gunzip: false,
            uncacheable: false,
            req_body: None,
            resp_body: None,
        }
    }
}

/// The VCL interpreter executes parsed VCL programs.
pub struct VclInterpreter {
    program: Arc<VclProgram>,
    /// Index of subroutines by name for fast lookup.
    sub_index: HashMap<std::string::String, usize>,
    /// Compiled regex cache (thread-safe).
    regex_cache: DashMap<std::string::String, Regex>,
    /// Optional function resolver for VMOD calls (e.g., "std.log").
    resolver: Option<Arc<dyn crate::resolver::FunctionResolver>>,
}

impl VclInterpreter {
    /// Create a new interpreter for the given VCL program.
    pub fn new(program: Arc<VclProgram>) -> Self {
        let mut sub_index = HashMap::new();
        for (i, sub) in program.subs.iter().enumerate() {
            sub_index.insert(sub.name.clone(), i);
        }
        Self {
            program,
            sub_index,
            regex_cache: DashMap::new(),
            resolver: None,
        }
    }

    /// Create a new interpreter with a function resolver for VMOD dispatch.
    pub fn with_resolver(
        program: Arc<VclProgram>,
        resolver: Arc<dyn crate::resolver::FunctionResolver>,
    ) -> Self {
        let mut inst = Self::new(program);
        inst.resolver = Some(resolver);
        inst
    }

    /// Get a reference to the VCL program.
    pub fn program(&self) -> &VclProgram {
        &self.program
    }

    /// Execute a named subroutine.
    pub fn exec_subroutine(&self, name: &str, ctx: &mut VclContext) -> VclExecResult {
        let idx = match self.sub_index.get(name) {
            Some(idx) => *idx,
            None => return VclExecResult::Fallthrough,
        };
        let sub = &self.program.subs[idx];
        match self.exec_block(&sub.body, ctx) {
            ControlFlow::Continue => VclExecResult::Fallthrough,
            ControlFlow::Return(action) => VclExecResult::Action(action),
            ControlFlow::Error(e) => VclExecResult::Error(e),
        }
    }

    /// Check if a subroutine exists in the program.
    pub fn has_subroutine(&self, name: &str) -> bool {
        self.sub_index.contains_key(name)
    }

    fn exec_block(&self, stmts: &[Statement], ctx: &mut VclContext) -> ControlFlow {
        for stmt in stmts {
            match self.exec_statement(stmt, ctx) {
                ControlFlow::Continue => {}
                other => return other,
            }
        }
        ControlFlow::Continue
    }

    fn exec_statement(&self, stmt: &Statement, ctx: &mut VclContext) -> ControlFlow {
        match stmt {
            Statement::Set {
                target,
                operator,
                value,
            } => {
                let val = match self.eval_expr(value, ctx) {
                    Ok(v) => v,
                    Err(e) => return ControlFlow::Error(e),
                };
                self.set_variable(target, val, *operator, ctx);
                ControlFlow::Continue
            }
            Statement::Unset { target } => {
                self.unset_variable(target, ctx);
                ControlFlow::Continue
            }
            Statement::Call { subroutine } => {
                let idx = match self.sub_index.get(subroutine.as_str()) {
                    Some(idx) => *idx,
                    None => {
                        return ControlFlow::Error(format!("subroutine not found: {subroutine}"));
                    }
                };
                let sub = &self.program.subs[idx];
                self.exec_block(&sub.body, ctx)
            }
            Statement::Return { action } => self.handle_return(action, ctx),
            Statement::If {
                condition,
                body,
                else_ifs,
                else_body,
            } => {
                let cond = match self.eval_expr(condition, ctx) {
                    Ok(v) => v,
                    Err(e) => return ControlFlow::Error(e),
                };
                if cond.to_bool() {
                    return self.exec_block(body, ctx);
                }
                for (elif_cond, elif_body) in else_ifs {
                    let cond = match self.eval_expr(elif_cond, ctx) {
                        Ok(v) => v,
                        Err(e) => return ControlFlow::Error(e),
                    };
                    if cond.to_bool() {
                        return self.exec_block(elif_body, ctx);
                    }
                }
                if let Some(else_body) = else_body {
                    return self.exec_block(else_body, ctx);
                }
                ControlFlow::Continue
            }
            Statement::Synthetic { expr } => {
                let val = match self.eval_expr(expr, ctx) {
                    Ok(v) => v,
                    Err(e) => return ControlFlow::Error(e),
                };
                ctx.synth_body = Some(val.to_string_value());
                ControlFlow::Continue
            }
            Statement::Ban { expr } => {
                let _val = match self.eval_expr(expr, ctx) {
                    Ok(v) => v,
                    Err(e) => return ControlFlow::Error(e),
                };
                // Ban processing is handled by the cache engine
                ControlFlow::Continue
            }
            Statement::New { .. } => {
                // Object instantiation -- handled at a higher level
                ControlFlow::Continue
            }
            Statement::ExprStatement(expr) => match self.eval_expr(expr, ctx) {
                Ok(_) => ControlFlow::Continue,
                Err(e) => ControlFlow::Error(e),
            },
        }
    }

    fn handle_return(&self, action: &Expr, ctx: &mut VclContext) -> ControlFlow {
        match action {
            // return(pass), return(deliver), etc.
            Expr::Ident(name) | Expr::Variable(name) => match VclAction::from_name(name) {
                Some(a) => ControlFlow::Return(a),
                None => ControlFlow::Error(format!("unknown return action: {name}")),
            },
            // return(synth(404, "Not Found")) -- function-call style
            Expr::FunctionCall { name, args } => match name.as_str() {
                "synth" => {
                    if let Some(status_expr) = args.first() {
                        if let Ok(val) = self.eval_expr(status_expr, ctx) {
                            ctx.synth_status = Some(val.to_int() as u16);
                        }
                    }
                    if let Some(reason_expr) = args.get(1) {
                        if let Ok(val) = self.eval_expr(reason_expr, ctx) {
                            ctx.synth_body = Some(val.to_string_value());
                        }
                    }
                    ControlFlow::Return(VclAction::Synth)
                }
                _ => match VclAction::from_name(name) {
                    Some(a) => ControlFlow::Return(a),
                    None => ControlFlow::Error(format!("unknown return action: {name}")),
                },
            },
            _ => ControlFlow::Error("invalid return expression".to_string()),
        }
    }

    /// Evaluate an expression and return a VclValue.
    pub fn eval_expr(
        &self,
        expr: &Expr,
        ctx: &mut VclContext,
    ) -> Result<VclValue, std::string::String> {
        match expr {
            Expr::StringLit(s) => Ok(VclValue::String(s.clone())),
            Expr::IntLit(i) => Ok(VclValue::Int(*i)),
            Expr::RealLit(r) => Ok(VclValue::Real(*r)),
            Expr::DurationLit(d) => Ok(VclValue::Duration(*d)),
            Expr::BoolLit(b) => Ok(VclValue::Bool(*b)),

            Expr::Ident(name) => {
                // Check if it's a known action/keyword or a local variable
                if let Some(val) = ctx.local_vars.get(name) {
                    return Ok(val.clone());
                }
                // Could be a bare identifier like a backend name
                Ok(VclValue::String(name.clone()))
            }

            Expr::Variable(name) => Ok(self.resolve_variable(name, ctx)),

            Expr::HeaderRef(obj, header_name) => {
                let msg = match obj.as_str() {
                    "req" => &*ctx.req,
                    "resp" => &*ctx.resp,
                    "bereq" => &*ctx.bereq,
                    "beresp" => &*ctx.beresp,
                    _ => return Err(format!("unknown object: {obj}")),
                };
                let val = msg.get_header(header_name).unwrap_or("").to_string();
                Ok(VclValue::String(val))
            }

            Expr::BinaryOp { left, op, right } => {
                let lval = self.eval_expr(left, ctx)?;
                let rval = self.eval_expr(right, ctx)?;
                self.eval_binary_op(&lval, op, &rval)
            }

            Expr::UnaryOp { op, operand } => {
                let val = self.eval_expr(operand, ctx)?;
                match op {
                    UnaryOp::Not => Ok(VclValue::Bool(!val.to_bool())),
                    UnaryOp::Negate => Ok(VclValue::Int(-val.to_int())),
                }
            }

            Expr::FunctionCall { name, args } => {
                let mut evaluated_args = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated_args.push(self.eval_expr(arg, ctx)?);
                }
                self.call_builtin(name, &evaluated_args, ctx)
            }

            Expr::MethodCall {
                object,
                method,
                args,
            } => {
                let obj_val = self.eval_expr(object, ctx)?;
                let mut evaluated_args = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated_args.push(self.eval_expr(arg, ctx)?);
                }
                self.call_method(&obj_val, method, &evaluated_args)
            }

            Expr::RegexLit(pattern) => Ok(VclValue::String(pattern.clone())),

            Expr::Concat(parts) => {
                let mut result = std::string::String::new();
                for part in parts {
                    let val = self.eval_expr(part, ctx)?;
                    result.push_str(&val.to_string_value());
                }
                Ok(VclValue::String(result))
            }
        }
    }

    fn eval_binary_op(
        &self,
        left: &VclValue,
        op: &BinOp,
        right: &VclValue,
    ) -> Result<VclValue, std::string::String> {
        match op {
            BinOp::Eq => {
                let eq = left.to_string_value() == right.to_string_value();
                Ok(VclValue::Bool(eq))
            }
            BinOp::Neq => {
                let neq = left.to_string_value() != right.to_string_value();
                Ok(VclValue::Bool(neq))
            }
            BinOp::Gt => Ok(VclValue::Bool(left.to_real() > right.to_real())),
            BinOp::Lt => Ok(VclValue::Bool(left.to_real() < right.to_real())),
            BinOp::Gte => Ok(VclValue::Bool(left.to_real() >= right.to_real())),
            BinOp::Lte => Ok(VclValue::Bool(left.to_real() <= right.to_real())),
            BinOp::And => Ok(VclValue::Bool(left.to_bool() && right.to_bool())),
            BinOp::Or => Ok(VclValue::Bool(left.to_bool() || right.to_bool())),
            BinOp::Add => match (left, right) {
                (VclValue::Int(a), VclValue::Int(b)) => Ok(VclValue::Int(a + b)),
                (VclValue::Duration(a), VclValue::Duration(b)) => Ok(VclValue::Duration(a + b)),
                (VclValue::String(a), _) => Ok(VclValue::String(format!(
                    "{}{}",
                    a,
                    right.to_string_value()
                ))),
                _ => Ok(VclValue::Real(left.to_real() + right.to_real())),
            },
            BinOp::Sub => match (left, right) {
                (VclValue::Int(a), VclValue::Int(b)) => Ok(VclValue::Int(a - b)),
                (VclValue::Duration(a), VclValue::Duration(b)) => Ok(VclValue::Duration(a - b)),
                _ => Ok(VclValue::Real(left.to_real() - right.to_real())),
            },
            BinOp::Mul => Ok(VclValue::Real(left.to_real() * right.to_real())),
            BinOp::Div => {
                let divisor = right.to_real();
                if divisor == 0.0 {
                    Err("division by zero".to_string())
                } else {
                    Ok(VclValue::Real(left.to_real() / divisor))
                }
            }
            BinOp::Match => {
                let text = left.to_string_value();
                let pattern = right.to_string_value();
                let re = self.get_or_compile_regex(&pattern)?;
                Ok(VclValue::Bool(re.is_match(&text)))
            }
            BinOp::NotMatch => {
                let text = left.to_string_value();
                let pattern = right.to_string_value();
                let re = self.get_or_compile_regex(&pattern)?;
                Ok(VclValue::Bool(!re.is_match(&text)))
            }
        }
    }

    fn get_or_compile_regex(&self, pattern: &str) -> Result<Regex, std::string::String> {
        if let Some(cached) = self.regex_cache.get(pattern) {
            return Ok(cached.value().clone());
        }
        let re = Regex::new(pattern).map_err(|e| format!("regex error: {e}"))?;
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
    }

    /// Resolve a dotted variable name to a value.
    pub fn resolve_variable(&self, name: &str, ctx: &VclContext) -> VclValue {
        match name {
            // req.* variables
            "req.url" => VclValue::String(ctx.req.url.clone()),
            "req.method" => VclValue::String(ctx.req.method.to_string()),
            "req.proto" => VclValue::String(ctx.req.protocol.as_str().to_string()),
            "req.restarts" => VclValue::Int(ctx.restarts as i64),
            "req.is_ssl" | "req.is_tls" => VclValue::Bool(ctx.is_ssl),

            // bereq.* variables
            "bereq.url" => VclValue::String(ctx.bereq.url.clone()),
            "bereq.method" => VclValue::String(ctx.bereq.method.to_string()),
            "bereq.proto" => VclValue::String(ctx.bereq.protocol.as_str().to_string()),

            // beresp.* variables
            "beresp.status" => VclValue::Int(ctx.beresp.status.code() as i64),
            "beresp.reason" => VclValue::String(ctx.beresp.reason.clone()),
            "beresp.ttl" => VclValue::Duration(120.0), // default
            "beresp.grace" => VclValue::Duration(10.0),
            "beresp.keep" => VclValue::Duration(0.0),
            "beresp.uncacheable" => VclValue::Bool(ctx.uncacheable),
            "beresp.do_gzip" => VclValue::Bool(ctx.do_gzip),
            "beresp.do_gunzip" => VclValue::Bool(ctx.do_gunzip),

            // resp.* variables
            "resp.status" => VclValue::Int(ctx.resp.status.code() as i64),
            "resp.reason" => VclValue::String(ctx.resp.reason.clone()),
            "resp.proto" => VclValue::String(ctx.resp.protocol.as_str().to_string()),

            // obj.* variables (available in vcl_hit)
            "obj.hits" => VclValue::Int(ctx.obj_hits),
            "obj.ttl" => VclValue::Duration(ctx.obj_ttl),
            "obj.grace" => VclValue::Duration(ctx.obj_grace),
            "obj.keep" => VclValue::Duration(ctx.obj_keep),

            // client/server
            "client.ip" => VclValue::Ip(ctx.client_ip),
            "server.ip" => VclValue::Ip(ctx.server_ip),

            // now
            "now" => VclValue::Real(VtimReal::now().0),

            // Check local variables
            _ => {
                if let Some(val) = ctx.local_vars.get(name) {
                    val.clone()
                } else {
                    // Try as header reference: req.http.X-Foo
                    if let Some(header) = name.strip_prefix("req.http.") {
                        let val = ctx.req.get_header(header).unwrap_or("").to_string();
                        VclValue::String(val)
                    } else if let Some(header) = name.strip_prefix("resp.http.") {
                        let val = ctx.resp.get_header(header).unwrap_or("").to_string();
                        VclValue::String(val)
                    } else if let Some(header) = name.strip_prefix("bereq.http.") {
                        let val = ctx.bereq.get_header(header).unwrap_or("").to_string();
                        VclValue::String(val)
                    } else if let Some(header) = name.strip_prefix("beresp.http.") {
                        let val = ctx.beresp.get_header(header).unwrap_or("").to_string();
                        VclValue::String(val)
                    } else {
                        VclValue::Void
                    }
                }
            }
        }
    }

    /// Set a variable to a value, applying the specified operator.
    fn set_variable(&self, name: &str, val: VclValue, op: SetOp, ctx: &mut VclContext) {
        match name {
            // req.* writeable variables
            "req.url" => match op {
                SetOp::Assign => ctx.req.url = val.to_string_value(),
                SetOp::Add => ctx.req.url.push_str(&val.to_string_value()),
                SetOp::Subtract => {}
            },
            "req.method" => {
                if op == SetOp::Assign {
                    ctx.req.method = rv_types::HttpMethod::parse_method(&val.to_string_value());
                }
            }

            // bereq.* writeable variables
            "bereq.url" => match op {
                SetOp::Assign => ctx.bereq.url = val.to_string_value(),
                SetOp::Add => ctx.bereq.url.push_str(&val.to_string_value()),
                SetOp::Subtract => {}
            },
            "bereq.method" => {
                if op == SetOp::Assign {
                    ctx.bereq.method = rv_types::HttpMethod::parse_method(&val.to_string_value());
                }
            }

            // beresp.* writeable variables
            "beresp.status" => {
                if op == SetOp::Assign {
                    ctx.beresp.status = HttpStatus::new(val.to_int() as u16);
                }
            }
            "beresp.reason" => {
                if op == SetOp::Assign {
                    ctx.beresp.reason = val.to_string_value();
                }
            }
            "beresp.ttl" => {
                ctx.local_vars.insert(
                    "beresp.ttl".to_string(),
                    VclValue::Duration(apply_duration_op(
                        ctx.local_vars
                            .get("beresp.ttl")
                            .map_or(120.0, |v| v.to_duration_secs()),
                        val.to_duration_secs(),
                        op,
                    )),
                );
            }
            "beresp.grace" => {
                ctx.local_vars.insert(
                    "beresp.grace".to_string(),
                    VclValue::Duration(apply_duration_op(
                        ctx.local_vars
                            .get("beresp.grace")
                            .map_or(10.0, |v| v.to_duration_secs()),
                        val.to_duration_secs(),
                        op,
                    )),
                );
            }
            "beresp.keep" => {
                ctx.local_vars.insert(
                    "beresp.keep".to_string(),
                    VclValue::Duration(apply_duration_op(
                        ctx.local_vars
                            .get("beresp.keep")
                            .map_or(0.0, |v| v.to_duration_secs()),
                        val.to_duration_secs(),
                        op,
                    )),
                );
            }
            "beresp.uncacheable" => {
                if op == SetOp::Assign {
                    ctx.uncacheable = val.to_bool();
                }
            }
            "beresp.do_gzip" => {
                if op == SetOp::Assign {
                    ctx.do_gzip = val.to_bool();
                }
            }
            "beresp.do_gunzip" => {
                if op == SetOp::Assign {
                    ctx.do_gunzip = val.to_bool();
                }
            }

            // resp.* writeable variables
            "resp.status" => {
                if op == SetOp::Assign {
                    ctx.resp.status = HttpStatus::new(val.to_int() as u16);
                }
            }
            "resp.reason" => {
                if op == SetOp::Assign {
                    ctx.resp.reason = val.to_string_value();
                }
            }

            _ => {
                // Handle header references
                if let Some(header) = name.strip_prefix("req.http.") {
                    apply_header_op(&mut ctx.req.headers, header, &val, op);
                } else if let Some(header) = name.strip_prefix("resp.http.") {
                    apply_header_op(&mut ctx.resp.headers, header, &val, op);
                } else if let Some(header) = name.strip_prefix("bereq.http.") {
                    apply_header_op(&mut ctx.bereq.headers, header, &val, op);
                } else if let Some(header) = name.strip_prefix("beresp.http.") {
                    apply_header_op(&mut ctx.beresp.headers, header, &val, op);
                } else {
                    // Store as local variable
                    ctx.local_vars.insert(name.to_string(), val);
                }
            }
        }
    }

    fn unset_variable(&self, name: &str, ctx: &mut VclContext) {
        if let Some(header) = name.strip_prefix("req.http.") {
            ctx.req.unset_header(header);
        } else if let Some(header) = name.strip_prefix("resp.http.") {
            ctx.resp.unset_header(header);
        } else if let Some(header) = name.strip_prefix("bereq.http.") {
            ctx.bereq.unset_header(header);
        } else if let Some(header) = name.strip_prefix("beresp.http.") {
            ctx.beresp.unset_header(header);
        } else {
            ctx.local_vars.remove(name);
        }
    }

    fn call_builtin(
        &self,
        name: &str,
        args: &[VclValue],
        _ctx: &mut VclContext,
    ) -> Result<VclValue, std::string::String> {
        match name {
            "regsub" => {
                if args.len() < 3 {
                    return Err("regsub requires 3 arguments".to_string());
                }
                let text = args[0].to_string_value();
                let pattern = args[1].to_string_value();
                let replacement = args[2].to_string_value();
                let re = self.get_or_compile_regex(&pattern)?;
                Ok(VclValue::String(
                    re.replace(&text, replacement.as_str()).to_string(),
                ))
            }
            "regsuball" => {
                if args.len() < 3 {
                    return Err("regsuball requires 3 arguments".to_string());
                }
                let text = args[0].to_string_value();
                let pattern = args[1].to_string_value();
                let replacement = args[2].to_string_value();
                let re = self.get_or_compile_regex(&pattern)?;
                Ok(VclValue::String(
                    re.replace_all(&text, replacement.as_str()).to_string(),
                ))
            }
            "hash_data" => {
                // Accumulate hash data - noop in interpreter, handled by caller
                Ok(VclValue::Void)
            }
            "ban" => {
                // Ban expression - handled by cache engine
                Ok(VclValue::Void)
            }
            "return" => {
                // This shouldn't be called as a function normally
                Ok(VclValue::Void)
            }
            _ => {
                // Try function resolver for dotted names (e.g., "std.log")
                if let Some(pos) = name.find('.') {
                    if let Some(ref resolver) = self.resolver {
                        let module = &name[..pos];
                        let function = &name[pos + 1..];
                        return resolver.call(module, function, args);
                    }
                }
                // Unknown function - return void
                Ok(VclValue::Void)
            }
        }
    }

    fn call_method(
        &self,
        _object: &VclValue,
        _method: &str,
        _args: &[VclValue],
    ) -> Result<VclValue, std::string::String> {
        // Method calls are typically for VMOD objects
        Ok(VclValue::Void)
    }
}

fn apply_header_op(headers: &mut HeaderMap, name: &str, val: &VclValue, op: SetOp) {
    match op {
        SetOp::Assign => {
            headers.set(name, val.to_string_value());
        }
        SetOp::Add => {
            let existing = headers.get(name).unwrap_or("").to_string();
            headers.set(name, format!("{}{}", existing, val.to_string_value()));
        }
        SetOp::Subtract => {
            // Subtract doesn't make sense for headers, treat as noop
        }
    }
}

fn apply_duration_op(current: f64, new_val: f64, op: SetOp) -> f64 {
    match op {
        SetOp::Assign => new_val,
        SetOp::Add => current + new_val,
        SetOp::Subtract => current - new_val,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lexer, Parser};
    use rv_http::message::HttpVersion;
    use rv_types::HttpMethod;

    fn parse_vcl(source: &str) -> VclProgram {
        let tokens = Lexer::tokenize(source).unwrap();
        Parser::parse(&tokens).unwrap()
    }

    fn make_context<'a>(
        req: &'a mut HttpMessage,
        resp: &'a mut HttpMessage,
        bereq: &'a mut HttpMessage,
        beresp: &'a mut HttpMessage,
    ) -> VclContext<'a> {
        VclContext::new(req, resp, bereq, beresp)
    }

    #[test]
    fn test_eval_string_lit() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let val = interp
            .eval_expr(&Expr::StringLit("hello".to_string()), &mut ctx)
            .unwrap();
        assert_eq!(val.to_string_value(), "hello");
    }

    #[test]
    fn test_eval_int_lit() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let val = interp.eval_expr(&Expr::IntLit(42), &mut ctx).unwrap();
        assert_eq!(val.to_int(), 42);
    }

    #[test]
    fn test_eval_binary_eq() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let expr = Expr::BinaryOp {
            left: Box::new(Expr::StringLit("hello".to_string())),
            op: BinOp::Eq,
            right: Box::new(Expr::StringLit("hello".to_string())),
        };
        let val = interp.eval_expr(&expr, &mut ctx).unwrap();
        assert!(val.to_bool());
    }

    #[test]
    fn test_eval_regex_match() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let expr = Expr::BinaryOp {
            left: Box::new(Expr::StringLit("/images/foo.png".to_string())),
            op: BinOp::Match,
            right: Box::new(Expr::RegexLit("^/images/".to_string())),
        };
        let val = interp.eval_expr(&expr, &mut ctx).unwrap();
        assert!(val.to_bool());
    }

    #[test]
    fn test_resolve_req_url() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let val = interp.resolve_variable("req.url", &ctx);
        assert_eq!(val.to_string_value(), "/test");
    }

    #[test]
    fn test_resolve_req_method() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Post, "/api", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let val = interp.resolve_variable("req.method", &ctx);
        assert_eq!(val.to_string_value(), "POST");
    }

    #[test]
    fn test_resolve_header_ref() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        req.set_header("X-Custom", "test-value");
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let val = interp.resolve_variable("req.http.X-Custom", &ctx);
        assert_eq!(val.to_string_value(), "test-value");
    }

    #[test]
    fn test_exec_subroutine_pass() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv {
    return(pass);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_recv", &mut ctx) {
            VclExecResult::Action(action) => assert_eq!(action, VclAction::Pass),
            other => panic!("expected Action(Pass), got {other:?}"),
        }
    }

    #[test]
    fn test_exec_subroutine_if_condition() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv {
    if (req.url == "/health") {
        return(synth);
    }
    return(hash);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);

        // Test matching path
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/health", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_recv", &mut ctx) {
            VclExecResult::Action(action) => assert_eq!(action, VclAction::Synth),
            other => panic!("expected Action(Synth), got {other:?}"),
        }

        // Test non-matching path
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/api", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_recv", &mut ctx) {
            VclExecResult::Action(action) => assert_eq!(action, VclAction::Hash),
            other => panic!("expected Action(Hash), got {other:?}"),
        }
    }

    #[test]
    fn test_exec_set_header() {
        let vcl = r#"
vcl 4.0;
sub vcl_deliver {
    set resp.http.X-Cache = "HIT";
    return(deliver);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        interp.exec_subroutine("vcl_deliver", &mut ctx);
        assert_eq!(resp.get_header("X-Cache"), Some("HIT"));
    }

    #[test]
    fn test_exec_unset_header() {
        let vcl = r#"
vcl 4.0;
sub vcl_deliver {
    unset resp.http.X-Powered-By;
    return(deliver);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        resp.set_header("X-Powered-By", "PHP/7.4");
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        interp.exec_subroutine("vcl_deliver", &mut ctx);
        assert!(resp.get_header("X-Powered-By").is_none());
    }

    #[test]
    fn test_exec_regex_match_in_if() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv {
    if (req.url ~ "^/api/") {
        return(pass);
    }
    return(hash);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);

        let mut req = HttpMessage::new_request(HttpMethod::Get, "/api/users", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_recv", &mut ctx) {
            VclExecResult::Action(action) => assert_eq!(action, VclAction::Pass),
            other => panic!("expected Action(Pass), got {other:?}"),
        }
    }

    #[test]
    fn test_exec_nonexistent_sub() {
        let vcl = "vcl 4.0;\nsub vcl_recv { return(hash); }";
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_miss", &mut ctx) {
            VclExecResult::Fallthrough => {}
            other => panic!("expected Fallthrough, got {other:?}"),
        }
    }

    #[test]
    fn test_regex_cache() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);

        let re1 = interp.get_or_compile_regex("^/api/").unwrap();
        let re2 = interp.get_or_compile_regex("^/api/").unwrap();
        // Both should work (cached)
        assert!(re1.is_match("/api/test"));
        assert!(re2.is_match("/api/test"));
        // Only one entry in cache
        assert_eq!(interp.regex_cache.len(), 1);
    }

    #[test]
    fn test_eval_concat() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let expr = Expr::Concat(vec![
            Expr::StringLit("hello ".to_string()),
            Expr::StringLit("world".to_string()),
        ]);
        let val = interp.eval_expr(&expr, &mut ctx).unwrap();
        assert_eq!(val.to_string_value(), "hello world");
    }

    #[test]
    fn test_return_synth_with_status() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv {
    return(synth(403, "Forbidden"));
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        match interp.exec_subroutine("vcl_recv", &mut ctx) {
            VclExecResult::Action(action) => {
                assert_eq!(action, VclAction::Synth);
                assert_eq!(ctx.synth_status, Some(403));
                assert_eq!(ctx.synth_body.as_deref(), Some("Forbidden"));
            }
            other => panic!("expected Action(Synth), got {other:?}"),
        }
    }

    #[test]
    fn test_set_req_url() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv {
    set req.url = "/rewritten";
    return(hash);
}
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/original", HttpVersion::Http11);
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        interp.exec_subroutine("vcl_recv", &mut ctx);
        assert_eq!(req.url, "/rewritten");
    }

    #[test]
    fn test_regsub() {
        let program = Arc::new(parse_vcl("vcl 4.0;\nsub vcl_recv { }"));
        let interp = VclInterpreter::new(program);
        let mut req = HttpMessage::default();
        let mut resp = HttpMessage::default();
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut ctx = make_context(&mut req, &mut resp, &mut bereq, &mut beresp);

        let expr = Expr::FunctionCall {
            name: "regsub".to_string(),
            args: vec![
                Expr::StringLit("/api/v1/users".to_string()),
                Expr::StringLit("^/api/".to_string()),
                Expr::StringLit("/".to_string()),
            ],
        };
        let val = interp.eval_expr(&expr, &mut ctx).unwrap();
        assert_eq!(val.to_string_value(), "/v1/users");
    }

    #[test]
    fn test_has_subroutine() {
        let vcl = r#"
vcl 4.0;
sub vcl_recv { return(hash); }
sub vcl_deliver { return(deliver); }
"#;
        let program = Arc::new(parse_vcl(vcl));
        let interp = VclInterpreter::new(program);
        assert!(interp.has_subroutine("vcl_recv"));
        assert!(interp.has_subroutine("vcl_deliver"));
        assert!(!interp.has_subroutine("vcl_miss"));
    }
}
