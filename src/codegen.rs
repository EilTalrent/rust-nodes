//! Node graph → Rust source.
//!
//! Exec wires become statement order; data wires become nested expressions.
//! The result is run through `prettyplease` so it reads like hand-written Rust.

use crate::graph::*;
use std::collections::HashSet;

pub struct Generated {
    pub code: String,
    /// Set when the generated text isn't valid Rust syntax (e.g. a broken raw-code node).
    pub error: Option<String>,
}

pub fn generate(p: &Program) -> Generated {
    let mut out = String::new();
    gen_items(&p.items, &mut out);
    match syn::parse_file(&out) {
        Ok(file) => Generated { code: space_items(&prettyplease::unparse(&file)), error: None },
        Err(e) => {
            let start = e.span().start();
            Generated { code: out, error: Some(format!("line {}:{}: {e}", start.line, start.column + 1)) }
        }
    }
}

fn gen_items(items: &[Item], out: &mut String) {
    for it in items {
        match it {
            Item::Raw(s) => {
                out.push_str(s);
                out.push_str("\n\n");
            }
            Item::Func(f) => {
                out.push_str(&gen_fn(f));
                out.push_str("\n\n");
            }
            Item::Impl { header, items } => {
                out.push_str(header);
                out.push_str(" {\n");
                gen_items(items, out);
                out.push_str("}\n\n");
            }
        }
    }
}

pub fn gen_fn(f: &FnGraph) -> String {
    let mut g = Gen { f, out: String::new(), visited: HashSet::new(), container: false };
    let start = f.exec_target(f.entry, 0);
    g.chain(start, true);
    format!("{} {{\n{}}}", f.sig, g.out)
}

struct Gen<'a> {
    f: &'a FnGraph,
    out: String,
    visited: HashSet<NodeId>,
    /// generating the inside of a container block (not the function body)
    container: bool,
}

/// Short text for whatever feeds a data pin, used to label wires that come from
/// outside the container being viewed.
pub fn expr_preview(f: &FnGraph, id: NodeId, pin: usize) -> String {
    let g = Gen { f, out: String::new(), visited: HashSet::new(), container: false };
    let s = g.expr(id, pin, 0).0.replace('\n', " ");
    if s.chars().count() > 24 { format!("{}…", s.chars().take(23).collect::<String>()) } else { s }
}

// Operator precedence, higher binds tighter.
pub const P_LOWEST: u8 = 0;
pub const P_UNARY: u8 = 20;
pub const P_ATOM: u8 = 30;

pub fn bin_prec(op: &str) -> u8 {
    match op {
        "||" => 3,
        "&&" => 4,
        "==" | "!=" | "<" | ">" | "<=" | ">=" => 5,
        "|" => 6,
        "^" => 7,
        "&" => 8,
        "<<" | ">>" => 9,
        "+" | "-" => 10,
        "*" | "/" | "%" => 11,
        _ => 1,
    }
}

fn paren(s: (String, u8), min: u8) -> String {
    if s.1 < min { format!("({})", s.0) } else { s.0 }
}

impl<'a> Gen<'a> {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Emit a chain of statements following exec wires.
    fn chain(&mut self, start: Option<NodeId>, top: bool) {
        let mut cur = start;
        while let Some(id) = cur {
            if !self.visited.insert(id) {
                self.line("// (exec loop in graph: node already emitted)");
                return;
            }
            let Some(node) = self.f.nodes.get(&id) else { return };
            let next = node.kind.next_pin().and_then(|p| self.f.exec_target(id, p));
            self.stmt(id, &node.kind, top && next.is_none());
            cur = next;
        }
    }

    fn stmt(&mut self, id: NodeId, kind: &NodeKind, last_top: bool) {
        use NodeKind::*;
        match kind {
            Let { name, mutable, ty } => {
                let m = if *mutable { "mut " } else { "" };
                let t = if ty.is_empty() { String::new() } else { format!(": {ty}") };
                match self.input_opt(id, 0) {
                    Some(v) => self.line(&format!("let {m}{name}{t} = {v};")),
                    None => self.line(&format!("let {m}{name}{t};")),
                }
            }
            Assign { op } => {
                let t = self.input(id, 0).0;
                let v = self.input(id, 1).0;
                self.line(&format!("{t} {op} {v};"));
            }
            Eval => {
                let v = self.input(id, 0).0;
                self.line(&format!("{v};"));
            }
            If => {
                let tail = last_top && self.ends_with_return(Some(id), 0);
                self.gen_if(id, "", tail)
            }
            While => {
                let c = self.input(id, 0).0;
                self.line(&format!("while {c} {{"));
                self.chain(self.f.exec_target(id, 0), false);
                self.line("}");
            }
            Loop => {
                self.line("loop {");
                self.chain(self.f.exec_target(id, 0), false);
                self.line("}");
            }
            For { var } => {
                let it = self.input(id, 0).0;
                self.line(&format!("for {var} in {it} {{"));
                self.chain(self.f.exec_target(id, 0), false);
                self.line("}");
            }
            Break => self.line("break;"),
            Continue => self.line("continue;"),
            Return => match (self.input_opt(id, 0), last_top && !self.container) {
                (Some(v), true) => self.line(&v), // tail expression
                (None, true) => {}
                (Some(v), false) => self.line(&format!("return {v};")),
                (None, false) => self.line("return;"),
            },
            BlockValue => match (self.input_opt(id, 0), last_top) {
                (Some(v), true) => self.line(&v), // the block's value
                (Some(v), false) => self.line(&format!("{v};")),
                (None, _) => {}
            },
            RawStmt { code } => self.line(code),
            TemplateStmt { code, holes } => {
                let t = self.template_text(id, code, holes, 0);
                self.line(&t);
            }
            Entry | BlockEntry { .. } => {}
            _ => {} // pure nodes never sit on an exec chain
        }
    }

    /// Does this chain always finish with `return value` (so it can be written as a tail expression)?
    fn ends_with_return(&self, start: Option<NodeId>, depth: usize) -> bool {
        let mut cur = start;
        let mut last = None;
        let mut n = 0;
        while let Some(id) = cur {
            n += 1;
            if n > 10_000 || depth > 64 {
                return false;
            }
            last = Some(id);
            let kind = &self.f.nodes.get(&id).map(|n| &n.kind);
            cur = kind.and_then(|k| k.next_pin()).and_then(|p| self.f.exec_target(id, p));
        }
        let Some(id) = last else { return false };
        match self.f.nodes.get(&id).map(|n| &n.kind) {
            Some(NodeKind::Return) if !self.container => self.f.data_source(id, 0).is_some(),
            Some(NodeKind::BlockValue) if self.container => self.f.data_source(id, 0).is_some(),
            Some(NodeKind::If) => {
                self.f.exec_target(id, 1).is_some()
                    && self.ends_with_return(self.f.exec_target(id, 0), depth + 1)
                    && self.ends_with_return(self.f.exec_target(id, 1), depth + 1)
            }
            _ => false,
        }
    }

    fn gen_if(&mut self, id: NodeId, prefix: &str, tail: bool) {
        let c = self.input(id, 0).0;
        self.line(&format!("{prefix}if {c} {{"));
        self.chain(self.f.exec_target(id, 0), tail);
        let els = self.f.exec_target(id, 1);
        match els {
            None => self.line("}"),
            Some(e) => {
                // `else if` when the else branch is just a single If with nothing after it
                let is_else_if = matches!(self.f.nodes.get(&e).map(|n| &n.kind), Some(NodeKind::If))
                    && self.f.exec_target(e, 2).is_none()
                    && !self.visited.contains(&e);
                if is_else_if {
                    self.visited.insert(e);
                    self.gen_if(e, "} else ", tail);
                } else {
                    self.line("} else {");
                    self.chain(Some(e), tail);
                    self.line("}");
                }
            }
        }
    }

    fn input_opt(&self, id: NodeId, pin: usize) -> Option<String> {
        self.f.data_source(id, pin).map(|(n, p)| self.expr(n, p, 0).0)
    }

    fn input(&self, id: NodeId, pin: usize) -> (String, u8) {
        self.input_d(id, pin, 0)
    }

    fn input_d(&self, id: NodeId, pin: usize, depth: usize) -> (String, u8) {
        match self.f.data_source(id, pin) {
            Some((n, p)) => self.expr(n, p, depth + 1),
            None => ("todo!(\"unconnected pin\")".into(), P_ATOM),
        }
    }

    /// Expression text for a data output pin, with its precedence.
    fn expr(&self, id: NodeId, pin: usize, depth: usize) -> (String, u8) {
        use NodeKind::*;
        if depth > 256 {
            return ("todo!(\"data cycle\")".into(), P_ATOM);
        }
        let Some(node) = self.f.nodes.get(&id) else { return ("todo!()".into(), P_ATOM) };
        let inp = |i| self.input_d(id, i, depth);
        match &node.kind {
            Entry => (self.f.params.get(pin).cloned().unwrap_or_default(), P_ATOM),
            Let { name, .. } => (name.clone(), P_ATOM),
            For { var } => (var.clone(), P_ATOM),
            Literal { code } => {
                let p = if code.starts_with('-') { P_UNARY } else { P_ATOM };
                (code.clone(), p)
            }
            Var { name } => (name.clone(), P_ATOM),
            Binary { op } => {
                let p = bin_prec(op);
                // comparisons don't chain in Rust, so parenthesise equal precedence on both sides
                let lmin = if p == 5 { p + 1 } else { p };
                let a = paren(inp(0), lmin);
                let b = paren(inp(1), p + 1);
                (format!("{a} {op} {b}"), p)
            }
            Unary { op } => (format!("{op}{}", paren(inp(0), P_UNARY)), P_UNARY),
            Call { path, args } => {
                let a: Vec<String> = (0..*args).map(|i| inp(i).0).collect();
                (format!("{path}({})", a.join(", ")), P_ATOM)
            }
            Method { name, args } => {
                let recv = paren(inp(0), P_ATOM);
                let a: Vec<String> = (0..*args).map(|i| inp(i + 1).0).collect();
                (format!("{recv}.{name}({})", a.join(", ")), P_ATOM)
            }
            Macro { name, args, bracket } => {
                let a: Vec<String> = (0..*args).map(|i| inp(i).0).collect();
                let (o, c) = if *bracket { ('[', ']') } else { ('(', ')') };
                (format!("{name}!{o}{}{c}", a.join(", ")), P_ATOM)
            }
            Field { name } => (format!("{}.{name}", paren(inp(0), P_ATOM)), P_ATOM),
            Index => (format!("{}[{}]", paren(inp(0), P_ATOM), inp(1).0), P_ATOM),
            RawExpr { code } => (code.clone(), raw_prec(code)),
            BlockEntry { binders } => (binders.get(pin).cloned().unwrap_or_default(), P_ATOM),
            Template { code, holes } => {
                let t = self.template_text(id, code, holes, depth);
                let p = raw_prec(&t);
                (t, p)
            }
            _ => ("todo!(\"not a value\")".into(), P_ATOM),
        }
    }
}

impl<'a> Gen<'a> {
    /// Fill a template's holes: expression holes from data wires, block holes from their containers.
    fn template_text(&self, id: NodeId, code: &str, holes: &[Hole], depth: usize) -> String {
        let mut text = code.to_string();
        let mut pin = 0;
        for (i, h) in holes.iter().enumerate() {
            let fill = if h.block {
                self.block_text(id, i, h.bare_ok)
            } else {
                let v = self.input_d(id, pin, depth);
                pin += 1;
                paren(v, h.prec)
            };
            text = text.replace(&hole_marker(i), &fill);
        }
        text
    }

    fn block_text(&self, id: NodeId, hole: usize, bare_ok: bool) -> String {
        let Some(be) = self.f.block_entry(id, hole) else { return "{}".into() };
        let mut sub = Gen { f: self.f, out: String::new(), visited: HashSet::new(), container: true };
        let start = self.f.exec_target(be, 0);
        // a block that's only a value can be written without braces (closure body, match arm)
        if bare_ok {
            if let Some(s) = start {
                if matches!(self.f.nodes.get(&s).map(|n| &n.kind), Some(NodeKind::BlockValue)) {
                    if let Some(v) = sub.input_opt(s, 0) {
                        return v;
                    }
                }
            }
        }
        sub.chain(start, true);
        format!("{{\n{}}}", sub.out)
    }
}

/// Raw expressions that are "atomic" (calls, paths, literals...) don't need parentheses.
fn raw_prec(code: &str) -> u8 {
    match syn::parse_str::<syn::Expr>(code) {
        Ok(
            syn::Expr::Path(_)
            | syn::Expr::Lit(_)
            | syn::Expr::Call(_)
            | syn::Expr::MethodCall(_)
            | syn::Expr::Macro(_)
            | syn::Expr::Field(_)
            | syn::Expr::Index(_)
            | syn::Expr::Paren(_)
            | syn::Expr::Tuple(_)
            | syn::Expr::Array(_)
            | syn::Expr::Struct(_)
            | syn::Expr::Try(_),
        ) => P_ATOM,
        _ => P_LOWEST,
    }
}

/// prettyplease doesn't put blank lines between items; add them back for readability.
fn space_items(code: &str) -> String {
    fn kind(l: &str) -> &str {
        l.split_whitespace().next().unwrap_or("")
    }
    let is_attr = |l: &str| {
        let t = l.trim_start();
        t.starts_with("#[") || t.starts_with("///") || t.starts_with("//!") || t.starts_with("#![")
    };
    let mut out = String::new();
    let mut prev = "";
    for line in code.lines() {
        let top = !line.is_empty()
            && !line.starts_with([' ', '}', ')', ']'])
            && !prev.is_empty()
            && !is_attr(prev)
            && !(kind(prev) == kind(line) && !prev.ends_with('}') && !line.ends_with('{') && !prev.starts_with(' '));
        let inner = prev == "    }"
            && line.starts_with("    ")
            && !line.starts_with("     ")
            && ["fn", "pub", "#[", "///", "const", "type", "async", "unsafe"].iter().any(|k| line.trim_start().starts_with(k));
        if top || inner {
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        prev = line;
    }
    out
}
