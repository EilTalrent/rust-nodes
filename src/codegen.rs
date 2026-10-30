//! Node graph → Rust source.
//!
//! Exec wires become statement order; data wires become nested expressions.
//! The result is run through `prettyplease` so it reads like hand-written Rust.

use crate::graph::*;
use std::collections::{HashMap, HashSet};

pub struct Generated {
    pub code: String,
    /// Set when the generated text isn't valid Rust syntax (e.g. a broken raw-code node).
    pub error: Option<String>,
    /// Which lines each statement node became: (function index, node) -> first..=last line (0-based).
    /// Function index = position in `Program::functions()`.
    pub spans: HashMap<(usize, NodeId), (usize, usize)>,
}

/// Parse a span marker "@s3:17" / "@e3:17" / "@f3:0" into (kind, function index, node).
fn span_marker(text: &str) -> Option<(char, usize, NodeId)> {
    let rest = text.strip_prefix('@')?;
    let kind = rest.chars().next()?;
    let (fi, id) = rest[1..].split_once(':')?;
    Some((kind, fi.parse().ok()?, id.parse().ok()?))
}

const MARK: &str = "__rn_comment";
/// Macros the graph can't split are kept exactly as written: they travel through
/// prettyplease as `__rn_raw!("text")` (which it won't respace) and are put back after.
const RAW: &str = "__rn_raw";

fn is_macro_call(code: &str) -> bool {
    matches!(syn::parse_str::<syn::Expr>(code), Ok(syn::Expr::Macro(_)))
}

/// Replace every `__rn_raw!("text")` with `text`.
fn restore_raw(code: &str) -> String {
    let open = format!("{RAW}!(");
    let mut out = String::new();
    let mut rest = code;
    while let Some(pos) = rest.find(&open) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + open.len()..];
        let body = after.trim_start();
        let skipped = after.len() - body.len();
        // find the end of the string literal
        let mut end = None;
        let mut escaped = false;
        for (i, c) in body.char_indices().skip(1) {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    end = Some(i);
                    break;
                }
                _ => escaped = false,
            }
        }
        let parsed = end.and_then(|e| syn::parse_str::<syn::LitStr>(&body[..=e]).ok().map(|l| (e, l.value())));
        match parsed {
            Some((e, text)) => {
                let tail = body[e + 1..].trim_start();
                out.push_str(&text);
                rest = tail.strip_prefix(')').unwrap_or(tail);
            }
            None => {
                out.push_str(&open);
                rest = &after[skipped..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Turn `__rn_comment!("// text");` lines back into comments (see `Gen::comment_lines`).
fn restore_comments(code: &str) -> (String, HashMap<(usize, NodeId), (usize, usize)>) {
    let mut out: Vec<String> = vec![];
    let mut spans: HashMap<(usize, NodeId), (usize, usize)> = HashMap::new();
    let mut open: HashMap<(usize, NodeId), usize> = HashMap::new();
    // span markers: record line numbers instead of printing anything
    let mut mark = |text: &str, out: &Vec<String>, spans: &mut HashMap<(usize, NodeId), (usize, usize)>| -> bool {
        let Some((kind, fi, id)) = span_marker(text) else { return false };
        match kind {
            's' => {
                open.insert((fi, id), out.len());
            }
            'e' => {
                let end = out.len().saturating_sub(1);
                let start = open.remove(&(fi, id)).unwrap_or(end).min(end);
                spans.insert((fi, id), (start, end));
            }
            // the function's signature: the line just before its body's first marker
            'f' => {
                let l = out.len().saturating_sub(1);
                spans.insert((fi, id), (l, l));
            }
            _ => {}
        }
        true
    };
    // comments flagged '^': they belong after the block's final value, i.e. just before
    // the first later line that's indented less than they are (the block's closing brace)
    let mut pending: Vec<(usize, String)> = vec![];
    let mut lines = code.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        let indent_len = line.len() - t.len();
        // release (innermost first) every pending entry whose block this line closes
        if !t.is_empty() && pending.iter().any(|(ind, _)| indent_len < *ind) {
            let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut pending).into_iter().partition(|(ind, _)| indent_len < *ind);
            pending = keep;
            for (ind, text) in due {
                if !mark(&text, &out, &mut spans) {
                    push_comment(&mut out, &" ".repeat(ind), &text);
                }
            }
        }
        // statement form `__rn_comment!("..");` or match-arm attribute form `#[__rn_comment("..")]`
        let close = if t.starts_with(&format!("{MARK}!(")) {
            ");"
        } else if t.starts_with(&format!("#[{MARK}(")) {
            ")]"
        } else {
            out.push(line.to_string());
            continue;
        };
        // the printer might wrap a long one over several lines
        let mut full = t.to_string();
        while !full.trim_end().ends_with(close) {
            match lines.next() {
                Some(l) => full.push_str(l.trim()),
                None => break,
            }
        }
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        let (Some(a), Some(b)) = (full.find('"'), full.rfind('"')) else { continue };
        let Ok(lit) = syn::parse_str::<syn::LitStr>(&full[a..=b]) else { continue };
        let text = lit.value();
        if let Some(later) = text.strip_prefix('^') {
            pending.push((indent.len(), later.to_string()));
            continue;
        }
        if !mark(&text, &out, &mut spans) {
            push_comment(&mut out, &indent, &text);
        }
    }
    for (ind, text) in pending {
        if !mark(&text, &out, &mut spans) {
            push_comment(&mut out, &" ".repeat(ind), &text);
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    (s, spans)
}

/// Add one comment entry to the output lines ("~x" = end of the previous line, "" = blank line).
fn push_comment(out: &mut Vec<String>, indent: &str, text: &str) {
    {
        if let Some(trailing) = text.strip_prefix('~') {
            // a comment at the end of the previous line
            match out.iter_mut().rev().find(|l| !l.trim().is_empty()) {
                Some(prev) => {
                    prev.push(' ');
                    prev.push_str(trailing);
                }
                None => out.push(format!("{indent}{trailing}")),
            }
        } else if text.is_empty() {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
        } else {
            for (i, l) in text.lines().enumerate() {
                out.push(if i == 0 { format!("{indent}{l}") } else { l.to_string() });
            }
        }
    }
}

/// Where a kept comment goes in the output.
#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    /// before top-level item k (k == number of items: end of file)
    Item(usize),
    /// inside top-level impl k, before its item j (j == number of items: before the closing brace)
    InImpl(usize, usize),
}

pub fn generate(p: &Program) -> Generated {
    let mut out = String::new();
    let mut notes: Vec<(Anchor, String)> = vec![];
    gen_items(&p.items, &mut out, &mut notes);
    match syn::parse_file(&out) {
        Ok(mut file) => {
            syn::visit_mut::VisitMut::visit_file_mut(&mut Shorthand, &mut file);
            // raw macros first (they may span lines), then comments and span markers
            let (code, spans) = restore_comments(&restore_raw(&finish(&prettyplease::unparse(&file), &notes)));
            Generated { code, error: None, spans }
        }
        Err(e) => {
            let start = e.span().start();
            Generated { code: out, error: Some(format!("line {}:{}: {e}", start.line, start.column + 1)), spans: HashMap::new() }
        }
    }
}

fn gen_items(items: &[Item], out: &mut String, notes: &mut Vec<(Anchor, String)>) {
    let mut k = 0;
    let mut fi = 0; // function index, same order as Program::functions()
    for it in items {
        match it {
            Item::Comment(c) => notes.push((Anchor::Item(k), c.clone())),
            Item::Header(s) => {
                out.push_str(s);
                out.push_str("\n\n");
            }
            Item::Raw(s) => {
                out.push_str(s);
                out.push_str("\n\n");
                k += 1;
            }
            Item::Func(f) => {
                out.push_str(&gen_fn_at(f, Some(fi)));
                fi += 1;
                out.push_str("\n\n");
                k += 1;
            }
            Item::Impl { header, items } => {
                out.push_str(header);
                out.push_str(" {\n");
                let mut j = 0;
                for inner in items {
                    match inner {
                        Item::Comment(c) => notes.push((Anchor::InImpl(k, j), c.clone())),
                        Item::Raw(s) => {
                            out.push_str(s);
                            out.push_str("\n\n");
                            j += 1;
                        }
                        Item::Func(f) => {
                            out.push_str(&gen_fn_at(f, Some(fi)));
                            fi += 1;
                            out.push_str("\n\n");
                            j += 1;
                        }
                        _ => {}
                    }
                }
                out.push_str("}\n\n");
                k += 1;
            }
        }
    }
}

#[allow(dead_code)]
pub fn gen_fn(f: &FnGraph) -> String {
    gen_fn_at(f, None)
}

/// `fi`: the function's index, to tag statements with span markers (None = no markers).
fn gen_fn_at(f: &FnGraph, fi: Option<usize>) -> String {
    let mut g = Gen { f, out: String::new(), visited: HashSet::new(), container: false, fi };
    if let Some(fi) = fi {
        g.line(&format!("{MARK}!(\"@f{fi}:{}\");", f.entry));
    }
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
    /// function index for span markers
    fi: Option<usize>,
}

/// The full expression text of a data output (as it appears in the generated code,
/// up to formatting).
pub fn expr_text(f: &FnGraph, id: NodeId, pin: usize) -> String {
    let g = Gen { f, out: String::new(), visited: HashSet::new(), container: false, fi: None };
    restore_raw(&g.expr(id, pin, 0).0)
}

/// Short text for whatever feeds a data pin, used to label wires that come from
/// outside the container being viewed.
pub fn expr_preview(f: &FnGraph, id: NodeId, pin: usize) -> String {
    let g = Gen { f, out: String::new(), visited: HashSet::new(), container: false, fi: None };
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

    /// Comment lines travel through prettyplease as `__rn_comment!("...");` statements
    /// and are turned back into real comments afterwards (see `restore_comments`).
    fn comment_lines(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        for line in text.lines() {
            self.line(&format!("{MARK}!({line:?});"));
        }
    }

    fn stmt(&mut self, id: NodeId, kind: &NodeKind, last_top: bool) {
        let (mut before, mut after) = match self.f.nodes.get(&id) {
            Some(n) => (n.comment.clone(), n.comment_after.clone()),
            None => (String::new(), String::new()),
        };
        // span markers around the statement (turned into line numbers, never printed)
        if let Some(fi) = self.fi {
            before.push_str(&format!("@s{fi}:{id}\n"));
            after = format!("@e{fi}:{id}\n{after}");
        }
        self.comment_lines(&before);
        // nothing may follow a block's final value, so its trailing comments go in front,
        // flagged with '^' to be moved to the end of the block afterwards
        let is_tail = last_top
            && match kind {
                NodeKind::Return => !self.container,
                NodeKind::BlockValue => true,
                _ => false,
            };
        if is_tail && !after.is_empty() {
            let flagged: String = after.lines().map(|l| format!("^{l}\n")).collect();
            self.comment_lines(&flagged);
            self.stmt_inner(id, kind, last_top);
        } else {
            self.stmt_inner(id, kind, last_top);
            self.comment_lines(&after);
        }
    }

    fn stmt_inner(&mut self, id: NodeId, kind: &NodeKind, last_top: bool) {
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
                let node = &self.f.nodes[&e];
                let no_comments = node.comment.is_empty() && node.comment_after.is_empty();
                let is_else_if = matches!(node.kind, NodeKind::If)
                    && self.f.exec_target(e, 2).is_none()
                    && !self.visited.contains(&e)
                    && no_comments;
                // `else if let ...` (a template statement starting with `if`)
                let else_template = match &node.kind {
                    NodeKind::TemplateStmt { code, .. } if code.starts_with("if ") && no_comments && self.f.exec_target(e, 0).is_none() => {
                        Some(node.kind.clone())
                    }
                    _ => None,
                };
                if is_else_if {
                    self.visited.insert(e);
                    self.gen_if(e, "} else ", tail);
                } else if let Some(NodeKind::TemplateStmt { code, holes }) = else_template {
                    self.visited.insert(e);
                    let t = self.template_text(e, &code, &holes, 0);
                    self.line(&format!("}} else {t}"));
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
            RawExpr { code } if is_macro_call(code) => (format!("{RAW}!({code:?})"), P_ATOM),
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
        let mut sub = Gen { f: self.f, out: String::new(), visited: HashSet::new(), container: true, fi: self.fi };
        let start = self.f.exec_target(be, 0);
        // a block that's only a value can be written without braces (closure body, match arm),
        // and so can a lone `break` / `continue` / `return x`
        let has_comments = start.and_then(|s| self.f.nodes.get(&s)).is_some_and(|n| !n.comment.is_empty() || !n.comment_after.is_empty());
        if bare_ok && !has_comments {
            if let Some(s) = start {
                let alone = self.f.nodes[&s].kind.next_pin().and_then(|p| self.f.exec_target(s, p)).is_none();
                match self.f.nodes.get(&s).map(|n| &n.kind) {
                    Some(NodeKind::BlockValue) => {
                        if let Some(v) = sub.input_opt(s, 0) {
                            return v;
                        }
                    }
                    Some(NodeKind::Break) if alone => return "break".into(),
                    Some(NodeKind::Continue) if alone => return "continue".into(),
                    Some(NodeKind::Return) if alone => {
                        return match sub.input_opt(s, 0) {
                            Some(v) => format!("return {v}"),
                            None => "return".into(),
                        }
                    }
                    _ => {}
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
        Ok(syn::Expr::Cast(_)) => 12,
        Ok(syn::Expr::Unary(_) | syn::Expr::Reference(_)) => P_UNARY,
        Ok(syn::Expr::Binary(b)) => {
            use quote::ToTokens;
            bin_prec(&b.op.to_token_stream().to_string())
        }
        _ => P_LOWEST,
    }
}

/// Turn `Foo { x: x }` back into `Foo { x }`.
struct Shorthand;
impl syn::visit_mut::VisitMut for Shorthand {
    fn visit_field_value_mut(&mut self, f: &mut syn::FieldValue) {
        if let (syn::Member::Named(m), syn::Expr::Path(p)) = (&f.member, &f.expr) {
            if f.attrs.is_empty() && p.attrs.is_empty() && p.qself.is_none() && p.path.get_ident() == Some(m) {
                f.colon_token = None;
            }
        }
        syn::visit_mut::visit_field_value_mut(self, f);
    }
    fn visit_field_pat_mut(&mut self, f: &mut syn::FieldPat) {
        syn::visit_mut::visit_field_pat_mut(self, f);
    }
}

/// Final touches on the formatted code: blank lines between items (prettyplease drops them)
/// and the `//` comments that sat between items. Item positions come from the parser,
/// so lines inside string literals are never touched.
fn finish(code: &str, notes: &[(Anchor, String)]) -> String {
    use syn::spanned::Spanned;
    let Ok(file) = syn::parse_file(code) else { return code.to_string() };
    // comments: line to insert before -> comment text; usize::MAX = end of file
    let mut comments: Vec<(usize, &str)> = vec![];
    for (anchor, text) in notes {
        let line = match *anchor {
            Anchor::Item(k) => file.items.get(k).map(|it| it.span().start().line).unwrap_or(usize::MAX),
            Anchor::InImpl(k, j) => match file.items.get(k) {
                Some(syn::Item::Impl(imp)) => match imp.items.get(j) {
                    Some(ii) => ii.span().start().line,
                    None => imp.brace_token.span.close().start().line,
                },
                _ => usize::MAX,
            },
        };
        comments.push((line, text));
    }
    // 1-based line numbers that should get a blank line before them
    let mut before: Vec<usize> = vec![];
    let single_line_kind = |it: &syn::Item| -> Option<&'static str> {
        match it {
            syn::Item::Use(_) => Some("use"),
            syn::Item::Mod(m) if m.content.is_none() => Some("mod"),
            syn::Item::Const(_) => Some("const"),
            syn::Item::Static(_) => Some("static"),
            syn::Item::ExternCrate(_) => Some("extern"),
            _ => None,
        }
    };
    let mut prev: Option<&syn::Item> = None;
    for it in &file.items {
        if let Some(p) = prev {
            let same_short = single_line_kind(p).is_some()
                && single_line_kind(p) == single_line_kind(it)
                && p.span().end().line + 1 == it.span().start().line;
            if !same_short {
                before.push(it.span().start().line);
            }
        } else if !file.attrs.is_empty() {
            before.push(it.span().start().line); // after the //! header
        }
        if let syn::Item::Impl(imp) = it {
            for ii in imp.items.iter().skip(1) {
                before.push(ii.span().start().line);
            }
        }
        if let syn::Item::Trait(tr) = it {
            for ti in tr.items.iter().skip(1) {
                before.push(ti.span().start().line);
            }
        }
        prev = Some(it);
    }
    let mut out = String::new();
    let emit = |out: &mut String, text: &str, indent: &str| {
        for entry in text.lines() {
            if let Some(trailing) = entry.strip_prefix('~') {
                // end-of-line comment on the previous item's last line
                while out.ends_with('\n') {
                    out.pop();
                }
                out.push(' ');
                out.push_str(trailing);
                out.push_str("\n");
            } else if entry.is_empty() {
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
            } else {
                out.push_str(indent);
                out.push_str(entry.trim_start());
                out.push('\n');
            }
        }
    };
    for (i, line) in code.lines().enumerate() {
        let notes_here: Vec<&str> = comments.iter().filter(|(l, _)| *l == i + 1).map(|(_, t)| *t).collect();
        // the source's own spacing wins; otherwise one blank line between items
        if notes_here.is_empty() && before.contains(&(i + 1)) && !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        // a closing brace's comments are indented one level deeper (they were inside the block)
        let indent = if line.trim_start().starts_with('}') { format!("{indent}    ") } else { indent };
        for text in notes_here {
            emit(&mut out, text, &indent);
        }
        out.push_str(line);
        out.push('\n');
    }
    for (_, text) in comments.iter().filter(|(l, _)| *l == usize::MAX) {
        emit(&mut out, text, "");
    }
    out
}
