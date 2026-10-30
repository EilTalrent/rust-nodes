//! Rust source → node graph, using `syn`.
//!
//! Everything the graph can represent becomes nodes and wires. Anything it can't
//! (match, closures, if-let, labelled loops, ...) is kept as a raw-code node, so no
//! code is ever lost.

use crate::graph::*;
use quote::ToTokens;
use std::collections::HashMap;
use syn::punctuated::Punctuated;
use syn::visit_mut::{self, VisitMut};
use syn::{BinOp, Expr, FnArg, Pat, Stmt, UnOp};

pub fn parse_program(src: &str) -> Result<Program, String> {
    let file = syn::parse_file(src).map_err(|e| {
        let start = e.span().start();
        format!("line {}:{}: {e}", start.line, start.column + 1)
    })?;
    use syn::spanned::Spanned;
    let lines: Vec<&str> = src.lines().collect();
    let mut items = vec![];
    let mut prev = proc_macro2::LineColumn { line: 1, column: 0 };
    if !file.attrs.is_empty() {
        // inner attributes and `//!` doc comments, printed the normal way
        let attrs_only = syn::File { shebang: None, attrs: file.attrs.clone(), items: vec![] };
        items.push(Item::Header(prettyplease::unparse(&attrs_only).trim_end().to_string()));
        prev = file.attrs.last().unwrap().span().end();
    }
    for item in &file.items {
        let span = item.span();
        if let Some(c) = gap_text(&lines, prev, span.start()) {
            items.push(Item::Comment(c));
        }
        items.push(convert_item(item, &lines));
        prev = span.end();
    }
    let end = proc_macro2::LineColumn { line: lines.len() + 1, column: 0 };
    if let Some(c) = gap_text(&lines, prev, end) {
        items.push(Item::Comment(c));
    }
    Ok(Program { items })
}

type LC = proc_macro2::LineColumn;

/// The ordinary `//` and `/* */` comments (and blank lines) in the source between two positions,
/// one entry per line: "" = blank line, "~// x" = a comment at the end of the line `from` is on.
/// Doc comments are attributes and travel with their item, so they're skipped.
fn gap_text(lines: &[&str], from: LC, to: LC) -> Option<String> {
    if to.line < from.line || from.line == 0 || from.line > lines.len() {
        return None;
    }
    let mut gl: Vec<String> = vec![];
    for l in from.line..=to.line.min(lines.len()) {
        let line: Vec<char> = lines[l - 1].chars().collect();
        let a = if l == from.line { from.column.min(line.len()) } else { 0 };
        let b = if l == to.line { to.column.min(line.len()) } else { line.len() };
        gl.push(if a < b { line[a..b].iter().collect() } else { String::new() });
    }
    let n = gl.len();
    let mut out: Vec<String> = vec![];
    let mut in_block: Option<String> = None;
    for (i, l) in gl.iter().enumerate() {
        let t = l.trim();
        if let Some(mut acc) = in_block.take() {
            acc.push('\n');
            acc.push_str(l.trim_end());
            if t.contains("*/") {
                out.push(acc);
            } else {
                in_block = Some(acc);
            }
            continue;
        }
        if t.is_empty() {
            if i > 0 && i + 1 < n && out.last().map(String::as_str) != Some("") {
                out.push(String::new());
            }
            continue;
        }
        let Some(start) = t.find("//").into_iter().chain(t.find("/*")).min() else { continue };
        let c = &t[start..];
        let doc = (c.starts_with("///") && !c.starts_with("////")) || c.starts_with("//!") || (c.starts_with("/**") && !c.starts_with("/**/")) || c.starts_with("/*!");
        if doc {
            continue;
        }
        let entry = if i == 0 { format!("~{c}") } else { c.to_string() };
        if c.starts_with("/*") && !c.contains("*/") {
            in_block = Some(entry);
        } else {
            out.push(entry);
        }
    }
    if let Some(acc) = in_block {
        out.push(acc);
    }
    // every entry ends in '\n', so a lone blank line is "\n" (never empty)
    (!out.is_empty()).then(|| out.iter().map(|e| format!("{e}\n")).collect())
}

/// The exact source text between two positions.
fn source_text(lines: &[&str], from: LC, to: LC) -> Option<String> {
    if from.line == 0 || to.line > lines.len() || to.line < from.line {
        return None;
    }
    let mut out = String::new();
    for l in from.line..=to.line {
        let line: Vec<char> = lines[l - 1].chars().collect();
        let a = if l == from.line { from.column.min(line.len()) } else { 0 };
        let b = if l == to.line { to.column.min(line.len()) } else { line.len() };
        if l > from.line {
            out.push('\n');
        }
        out.extend(&line[a.min(b)..b]);
    }
    Some(out)
}

/// Drop blank-line entries at the start / end (e.g. right after `{` or right before `}`).
fn trim_blank(s: Option<String>, start: bool, end: bool) -> Option<String> {
    let s = s?;
    let mut v: Vec<&str> = s.lines().collect();
    while start && v.first() == Some(&"") {
        v.remove(0);
    }
    while end && v.last() == Some(&"") {
        v.pop();
    }
    (!v.is_empty()).then(|| v.iter().map(|e| format!("{e}\n")).collect())
}

fn convert_item(item: &syn::Item, lines: &[&str]) -> Item {
    match item {
        syn::Item::Fn(f) => {
            let name = f.sig.ident.to_string();
            Item::Func(build_fn(name, &f.attrs, &f.vis, &f.sig, &f.block, lines))
        }
        syn::Item::Impl(imp) => {
            let mut empty = imp.clone();
            empty.items.clear();
            let header = pretty_item(&syn::Item::Impl(empty));
            let header = header.trim_end().trim_end_matches("{}").trim_end().to_string();
            let self_ty = pretty_type(&imp.self_ty);
            use syn::spanned::Spanned;
            let mut items = vec![];
            let mut prev = imp.brace_token.span.open().end();
            for ii in &imp.items {
                let span = ii.span();
                if let Some(c) = gap_text(lines, prev, span.start()) {
                    items.push(Item::Comment(c));
                }
                items.push(match ii {
                    syn::ImplItem::Fn(f) => {
                        let name = format!("{self_ty}::{}", f.sig.ident);
                        Item::Func(build_fn(name, &f.attrs, &f.vis, &f.sig, &f.block, lines))
                    }
                    other => Item::Raw(pretty_impl_item(other)),
                });
                prev = span.end();
            }
            if let Some(c) = gap_text(lines, prev, imp.brace_token.span.close().start()) {
                items.push(Item::Comment(c));
            }
            Item::Impl { header, items }
        }
        other => Item::Raw(pretty_item(other)),
    }
}

/// Build a graph for one function body.
pub fn build_fn(name: String, attrs: &[syn::Attribute], vis: &syn::Visibility, sig: &syn::Signature, block: &syn::Block, lines: &[&str]) -> FnGraph {
    let sig_text = signature_text(attrs, vis, sig);
    let params = param_names(sig);
    let mut b = Builder {
        g: FnGraph::new(name, sig_text, params.clone()),
        scopes: vec![HashMap::new()],
        cur: None,
        lines: std::rc::Rc::new(lines.iter().map(|l| l.to_string()).collect()),
    };
    let entry = b.g.entry;
    for (i, p) in params.iter().enumerate() {
        b.scopes[0].insert(p.clone(), (entry, i));
    }
    b.block(&block.stmts, Some((entry, 0)), Tail::Return, braces(&block.brace_token));
    crate::layout::auto_layout(&mut b.g);
    b.g
}

pub fn signature_text(attrs: &[syn::Attribute], vis: &syn::Visibility, sig: &syn::Signature) -> String {
    let f = syn::ItemFn {
        attrs: attrs.to_vec(),
        vis: vis.clone(),
        sig: sig.clone(),
        block: Box::new(syn::parse_quote!({})),
    };
    let s = pretty_item(&syn::Item::Fn(f));
    s.trim_end().trim_end_matches("{}").trim_end().to_string()
}

pub fn param_names(sig: &syn::Signature) -> Vec<String> {
    sig.inputs
        .iter()
        .map(|a| match a {
            FnArg::Receiver(_) => "self".to_string(),
            FnArg::Typed(pt) => match &*pt.pat {
                Pat::Ident(pi) => pi.ident.to_string(),
                other => other.to_token_stream().to_string(),
            },
        })
        .collect()
}

/// Parse a signature typed by the user in the inspector, e.g. `fn add(a: i32, b: i32) -> i32`.
pub fn parse_signature(text: &str) -> Result<Vec<String>, String> {
    let f: syn::ItemFn = syn::parse_str(&format!("{text} {{}}")).map_err(|e| e.to_string())?;
    Ok(param_names(&f.sig))
}

type Port = Option<(NodeId, usize)>;

/// What a tail expression (last expression without `;`) means where we are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// function body: it's the return value
    Return,
    /// container block: it's the block's value
    Value,
    /// anywhere else: it's just an expression statement
    None,
}

/// Where a `{ ... }` block's inside starts and ends in the source.
type Braces = Option<(LC, LC)>;

fn braces(b: &syn::token::Brace) -> Braces {
    Some((b.span.open().end(), b.span.close().start()))
}

struct Builder {
    /// source lines, for keeping comments
    lines: std::rc::Rc<Vec<String>>,
    g: FnGraph,
    scopes: Vec<HashMap<String, (NodeId, usize)>>,
    /// container new nodes go into
    cur: Scope,
}

impl Builder {
    fn lookup(&self, name: &str) -> Option<(NodeId, usize)> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn add(&mut self, kind: NodeKind) -> NodeId {
        self.g.add_in(kind, [0.0, 0.0], self.cur)
    }

    fn exec_node(&mut self, kind: NodeKind, port: Port) -> NodeId {
        let id = self.add(kind);
        if let Some((n, p)) = port {
            self.g.connect_exec(n, p, id);
        }
        id
    }

    fn wire(&mut self, e: &Expr, to: NodeId, pin: usize) {
        let (n, p) = self.expr(e);
        self.g.connect_data(n, p, to, pin);
    }

    /// Statements we have no dedicated node for become template nodes with holes.
    fn template_stmt(&mut self, s: &Stmt, port: Port) -> Port {
        let mut col = HoleCollector { lines: self.lines.clone(), ..Default::default() };
        let mut s2 = s.clone();
        match &mut s2 {
            Stmt::Expr(e, _) => col.visit_expr_mut(e),
            Stmt::Local(l) => {
                col.in_root = true;
                col.visit_local_mut(l);
            }
            _ => col.failed = true,
        }
        if col.failed || col.payloads.is_empty() {
            let id = self.exec_node(NodeKind::RawStmt { code: pretty_stmts(vec![s.clone()]) }, port);
            return Some((id, 0));
        }
        let holes = col.holes();
        let code = fill_markers(&pretty_stmts(vec![s2]), &holes);
        let id = self.exec_node(NodeKind::TemplateStmt { code, holes }, port);
        self.fill_holes(id, col.payloads);
        Some((id, 0))
    }

    fn template_expr(&mut self, e: &Expr) -> (NodeId, usize) {
        let mut col = HoleCollector { lines: self.lines.clone(), ..Default::default() };
        let mut e2 = e.clone();
        col.visit_expr_mut(&mut e2);
        if col.failed || col.payloads.is_empty() {
            return self.pure(NodeKind::RawExpr { code: pretty_expr(e) }, &[]);
        }
        let holes = col.holes();
        let code = fill_markers(&pretty_expr(&e2), &holes);
        let id = self.add(NodeKind::Template { code, holes });
        self.fill_holes(id, col.payloads);
        (id, 0)
    }

    /// Wire expression holes and build a sub-graph for each block hole.
    fn fill_holes(&mut self, id: NodeId, payloads: Vec<Payload>) {
        let mut pin = 0;
        for (i, p) in payloads.into_iter().enumerate() {
            match p {
                Payload::Expr(e, _) => {
                    self.wire(&e, id, pin);
                    pin += 1;
                }
                Payload::Block { stmts, binders, braces, .. } => {
                    let saved = self.cur;
                    self.cur = Some((id, i));
                    let be = self.add(NodeKind::BlockEntry { binders: binders.clone() });
                    self.scopes.push(binders.iter().enumerate().map(|(j, b)| (b.clone(), (be, j))).collect());
                    self.block(&stmts, Some((be, 0)), Tail::Value, braces);
                    self.scopes.pop();
                    self.cur = saved;
                }
            }
        }
    }

    fn block(&mut self, stmts: &[Stmt], port: Port, tail: Tail, braces: Braces) -> Port {
        use syn::spanned::Spanned;
        self.scopes.push(HashMap::new());
        let mut port = port;
        let lines = self.lines.clone();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut prev: Option<LC> = braces.map(|b| b.0);
        let mut last_node: Option<NodeId> = None;
        for (i, s) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            let span = s.span();
            let comment = prev.and_then(|p| trim_blank(gap_text(&refs, p, span.start()), i == 0, false));
            let first_new = self.g.next_id;
            port = self.stmt(s, port, if last { tail } else { Tail::None });
            // the statement's own node: the first exec node it created at this level
            let cur = self.cur;
            let node = self.g.nodes.iter().find(|(id, n)| **id >= first_new && n.scope == cur && n.kind.is_exec()).map(|(id, _)| *id);
            if let (Some(c), Some(n)) = (comment, node) {
                self.g.nodes.get_mut(&n).unwrap().comment = c;
            }
            last_node = node.or(last_node);
            prev = (span.start().line > 0).then(|| span.end());
        }
        if let (Some(p), Some(b), Some(n)) = (prev, braces, last_node) {
            if let Some(c) = trim_blank(gap_text(&refs, p, b.1), false, true) {
                self.g.nodes.get_mut(&n).unwrap().comment_after = c;
            }
        }
        self.scopes.pop();
        port
    }

    fn stmt(&mut self, s: &Stmt, port: Port, tail: Tail) -> Port {
        match s {
            Stmt::Local(local) => {
                if local.init.as_ref().is_some_and(|i| i.diverge.is_some()) {
                    return self.template_stmt(s, port); // let-else
                }
                let (pat, ty) = match &local.pat {
                    Pat::Type(pt) => (&*pt.pat, pretty_type(&pt.ty)),
                    p => (p, String::new()),
                };
                let Pat::Ident(pi) = pat else { return self.template_stmt(s, port) };
                if pi.by_ref.is_some() || pi.subpat.is_some() {
                    return self.template_stmt(s, port);
                }
                let name = pi.ident.to_string();
                let id = self.exec_node(NodeKind::Let { name: name.clone(), mutable: pi.mutability.is_some(), ty }, port);
                if let Some(init) = &local.init {
                    self.wire(&init.expr, id, 0); // before registering the name: `let x = x + 1` shadows
                }
                self.scopes.last_mut().unwrap().insert(name, (id, 0));
                Some((id, 0))
            }
            Stmt::Item(_) => {
                let id = self.exec_node(NodeKind::RawStmt { code: pretty_stmts(vec![s.clone()]) }, port);
                Some((id, 0))
            }
            Stmt::Macro(m) => {
                let id = self.exec_node(NodeKind::Eval, port);
                let (n, p) = self.macro_node(&m.mac);
                self.g.connect_data(n, p, id, 0);
                Some((id, 0))
            }
            Stmt::Expr(e, semi) => {
                let value_tail = if semi.is_none() { tail } else { Tail::None };
                self.expr_stmt(s, e, port, value_tail)
            }
        }
    }

    /// `value_tail`: what this expression's value means (Return/Value), or None if it's discarded.
    fn expr_stmt(&mut self, s: &Stmt, e: &Expr, port: Port, value_tail: Tail) -> Port {
        let is_tail = value_tail != Tail::None;
        match e {
            Expr::If(ei) if !matches!(&*ei.cond, Expr::Let(_)) && !contains_let(&ei.cond) => {
                let id = self.exec_node(NodeKind::If, port);
                self.wire(&ei.cond, id, 0);
                self.block(&ei.then_branch.stmts, Some((id, 0)), value_tail, braces(&ei.then_branch.brace_token));
                if let Some((_, els)) = &ei.else_branch {
                    let inner = Stmt::Expr((**els).clone(), None);
                    match &**els {
                        Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => {
                            self.block(&b.block.stmts, Some((id, 1)), value_tail, braces(&b.block.brace_token));
                        }
                        _ => {
                            self.expr_stmt(&inner, els, Some((id, 1)), value_tail);
                        }
                    }
                }
                Some((id, 2))
            }
            Expr::While(w) if w.label.is_none() && !contains_let(&w.cond) => {
                let id = self.exec_node(NodeKind::While, port);
                self.wire(&w.cond, id, 0);
                self.block(&w.body.stmts, Some((id, 0)), Tail::None, braces(&w.body.brace_token));
                Some((id, 1))
            }
            Expr::Loop(l) if l.label.is_none() && !is_tail => {
                let id = self.exec_node(NodeKind::Loop, port);
                self.block(&l.body.stmts, Some((id, 0)), Tail::None, braces(&l.body.brace_token));
                Some((id, 1))
            }
            Expr::ForLoop(f) if f.label.is_none() && simple_ident(&f.pat).is_some() => {
                let var = simple_ident(&f.pat).unwrap();
                let id = self.exec_node(NodeKind::For { var: var.clone() }, port);
                self.wire(&f.expr, id, 0);
                self.scopes.push(HashMap::from([(var, (id, 0))]));
                self.block(&f.body.stmts, Some((id, 0)), Tail::None, braces(&f.body.brace_token));
                self.scopes.pop();
                Some((id, 1))
            }
            Expr::Return(r) => {
                let id = self.exec_node(NodeKind::Return, port);
                if let Some(v) = &r.expr {
                    self.wire(v, id, 0);
                }
                None
            }
            Expr::Break(b) if b.label.is_none() && b.expr.is_none() => {
                self.exec_node(NodeKind::Break, port);
                None
            }
            Expr::Continue(c) if c.label.is_none() => {
                self.exec_node(NodeKind::Continue, port);
                None
            }
            Expr::Assign(a) if !is_tail => {
                let id = self.exec_node(NodeKind::Assign { op: "=".into() }, port);
                self.wire(&a.left, id, 0);
                self.wire(&a.right, id, 1);
                Some((id, 0))
            }
            Expr::Binary(b) if is_assign_op(&b.op) && !is_tail => {
                let id = self.exec_node(NodeKind::Assign { op: b.op.to_token_stream().to_string() }, port);
                self.wire(&b.left, id, 0);
                self.wire(&b.right, id, 1);
                Some((id, 0))
            }
            _ if value_tail == Tail::Return => {
                let id = self.exec_node(NodeKind::Return, port);
                self.wire(e, id, 0);
                None
            }
            _ if value_tail == Tail::Value => {
                let id = self.exec_node(NodeKind::BlockValue, port);
                self.wire(e, id, 0);
                None
            }
            // statement-like constructs without a dedicated node
            Expr::If(_) | Expr::While(_) | Expr::Loop(_) | Expr::ForLoop(_) | Expr::Block(_) | Expr::Unsafe(_)
            | Expr::Match(_) | Expr::Break(_) | Expr::Const(_) | Expr::TryBlock(_) => self.template_stmt(s, port),
            _ => {
                let id = self.exec_node(NodeKind::Eval, port);
                self.wire(e, id, 0);
                Some((id, 0))
            }
        }
    }

    fn pure(&mut self, kind: NodeKind, inputs: &[&Expr]) -> (NodeId, usize) {
        let id = self.add(kind);
        for (i, e) in inputs.iter().enumerate() {
            self.wire(e, id, i);
        }
        (id, 0)
    }

    fn expr(&mut self, e: &Expr) -> (NodeId, usize) {
        match e {
            Expr::Path(p) if p.qself.is_none() && p.attrs.is_empty() => {
                if let Some(ident) = p.path.get_ident() {
                    if let Some(port) = self.lookup(&ident.to_string()) {
                        return port;
                    }
                }
                self.pure(NodeKind::Var { name: pretty_expr(e) }, &[])
            }
            Expr::Lit(l) if l.attrs.is_empty() => self.pure(NodeKind::Literal { code: l.lit.to_token_stream().to_string() }, &[]),
            Expr::Binary(b) if !is_assign_op(&b.op) => {
                self.pure(NodeKind::Binary { op: b.op.to_token_stream().to_string() }, &[&b.left, &b.right])
            }
            Expr::Unary(u) => {
                let op = match u.op {
                    UnOp::Deref(_) => "*",
                    UnOp::Not(_) => "!",
                    UnOp::Neg(_) => "-",
                    _ => return self.template_expr(e),
                };
                self.pure(NodeKind::Unary { op: op.into() }, &[&u.expr])
            }
            Expr::Reference(r) => {
                let op = if r.mutability.is_some() { "&mut " } else { "&" };
                self.pure(NodeKind::Unary { op: op.into() }, &[&r.expr])
            }
            Expr::Call(c) if matches!(&*c.func, Expr::Path(_)) => {
                let args: Vec<&Expr> = c.args.iter().collect();
                self.pure(NodeKind::Call { path: pretty_expr(&c.func), args: args.len() }, &args)
            }
            Expr::MethodCall(m) if m.turbofish.is_none() => {
                let mut ins: Vec<&Expr> = vec![&m.receiver];
                ins.extend(m.args.iter());
                self.pure(NodeKind::Method { name: m.method.to_string(), args: m.args.len() }, &ins)
            }
            Expr::Macro(m) => self.macro_node(&m.mac),
            Expr::Field(f) => {
                let name = match &f.member {
                    syn::Member::Named(i) => i.to_string(),
                    syn::Member::Unnamed(i) => i.index.to_string(),
                };
                self.pure(NodeKind::Field { name }, &[&f.base])
            }
            Expr::Index(i) => self.pure(NodeKind::Index, &[&i.expr, &i.index]),
            Expr::Paren(p) => self.expr(&p.expr),
            Expr::Group(g) => self.expr(&g.expr),
            _ => self.template_expr(e),
        }
    }

    fn macro_node(&mut self, mac: &syn::Macro) -> (NodeId, usize) {
        let bracket = match mac.delimiter {
            syn::MacroDelimiter::Paren(_) => false,
            syn::MacroDelimiter::Bracket(_) => true,
            syn::MacroDelimiter::Brace(_) => return self.raw_macro(mac),
        };
        let name = mac.path.to_token_stream().to_string().replace(' ', "");
        // only macros whose arguments really are expressions become nodes; others
        // (matches!, json!, custom DSLs...) may take patterns or other syntax: keep them verbatim
        const EXPR_MACROS: &[&str] = &[
            "println", "print", "eprintln", "eprint", "format", "format_args", "write", "writeln", "panic", "todo",
            "unimplemented", "unreachable", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq",
            "debug_assert_ne", "vec", "dbg",
        ];
        let base = name.rsplit("::").next().unwrap_or(&name);
        if !EXPR_MACROS.contains(&base) || (base == "vec" && mac.tokens.to_string().contains(';')) {
            return self.raw_macro(mac);
        }
        match mac.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated) {
            // Keep the format string etc. as normal argument nodes.
            Ok(args) => {
                let args: Vec<&Expr> = args.iter().collect();
                self.pure(NodeKind::Macro { name, args: args.len(), bracket }, &args)
            }
            Err(_) => self.raw_macro(mac),
        }
    }

    fn raw_macro(&mut self, mac: &syn::Macro) -> (NodeId, usize) {
        use syn::spanned::Spanned;
        // keep the macro exactly as written (the printer would respace its tokens)
        let span = mac.span();
        let refs: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        let original = (span.start().line > 0).then(|| source_text(&refs, span.start(), span.end())).flatten();
        let code = match original {
            Some(t) if syn::parse_str::<Expr>(&t).is_ok() => t,
            _ => pretty_expr(&Expr::Macro(syn::ExprMacro { attrs: vec![], mac: mac.clone() })),
        };
        self.pure(NodeKind::RawExpr { code }, &[])
    }
}

fn simple_ident(p: &Pat) -> Option<String> {
    match p {
        Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() && pi.mutability.is_none() => Some(pi.ident.to_string()),
        _ => None,
    }
}

fn contains_let(e: &Expr) -> bool {
    match e {
        Expr::Let(_) => true,
        Expr::Binary(b) => contains_let(&b.left) || contains_let(&b.right),
        Expr::Paren(p) => contains_let(&p.expr),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Turning any construct into a template with holes.

const HOLE: &str = "__rnh";

enum Payload {
    Expr(Expr, u8),
    Block { stmts: Vec<Stmt>, binders: Vec<String>, bare_ok: bool, braces: Braces },
}

/// Walks one construct and replaces its direct sub-expressions with hole placeholders.
/// Blocks become container holes; variables bound by patterns (match arms, closures,
/// `if let`, `while let`, `for`) become inputs of that container.
#[derive(Default)]
struct HoleCollector {
    /// source lines, to keep comments between match arms
    lines: std::rc::Rc<Vec<String>>,
    in_root: bool,
    failed: bool,
    payloads: Vec<Payload>,
}

fn placeholder(i: usize) -> Expr {
    let ident = syn::Ident::new(&format!("{HOLE}{i}"), proc_macro2::Span::call_site());
    syn::parse_quote!(#ident)
}

fn is_placeholder(e: &Expr) -> bool {
    matches!(e, Expr::Path(p) if p.path.get_ident().is_some_and(|i| i.to_string().starts_with(HOLE)))
}

fn block_is_placeholder(b: &syn::Block) -> bool {
    matches!(b.stmts.as_slice(), [Stmt::Expr(e, None)] if is_placeholder(e))
}

/// Variable names bound by a pattern.
fn pat_binders(p: &Pat) -> Vec<String> {
    struct V(Vec<String>);
    impl<'a> syn::visit::Visit<'a> for V {
        fn visit_pat_ident(&mut self, i: &'a syn::PatIdent) {
            let name = i.ident.to_string();
            // Uppercase idents in patterns are usually constants/unit variants, not bindings.
            if !name.starts_with(|c: char| c.is_uppercase()) && !self.0.contains(&name) {
                self.0.push(name);
            }
            syn::visit::visit_pat_ident(self, i);
        }
    }
    let mut v = V(vec![]);
    syn::visit::Visit::visit_pat(&mut v, p);
    v.0
}

impl HoleCollector {
    fn holes(&self) -> Vec<Hole> {
        self.payloads
            .iter()
            .map(|p| match p {
                Payload::Expr(_, prec) => Hole { block: false, binders: vec![], bare_ok: false, prec: *prec },
                Payload::Block { binders, bare_ok, .. } => {
                    Hole { block: true, binders: binders.clone(), bare_ok: *bare_ok, prec: 0 }
                }
            })
            .collect()
    }

    fn expr_hole(&mut self, e: &mut Expr) {
        let i = self.payloads.len();
        let old = std::mem::replace(e, placeholder(i));
        let prec = expr_prec(&old);
        self.payloads.push(Payload::Expr(old, prec));
    }

    /// Turn an expression-position body (closure body, match arm) into a container.
    fn body_hole(&mut self, e: &mut Expr, binders: Vec<String>) {
        let i = self.payloads.len();
        let old = std::mem::replace(e, placeholder(i));
        let (stmts, br) = match old {
            Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => {
                let br = braces(&b.block.brace_token);
                (b.block.stmts, br)
            }
            other => (vec![Stmt::Expr(other, None)], None),
        };
        self.payloads.push(Payload::Block { stmts, binders, bare_ok: true, braces: br });
    }

    /// Turn a `{ ... }` block into a container.
    fn block_hole(&mut self, b: &mut syn::Block, binders: Vec<String>) {
        if block_is_placeholder(b) {
            return;
        }
        let i = self.payloads.len();
        let br = braces(&b.brace_token);
        let stmts = std::mem::replace(&mut b.stmts, vec![Stmt::Expr(placeholder(i), None)]);
        self.payloads.push(Payload::Block { stmts, binders, bare_ok: false, braces: br });
    }

    /// `if [let] .. {} else if [let] .. {} else {}`: one template, a container per branch.
    fn if_chain(&mut self, i: &mut syn::ExprIf) {
        if let Expr::Let(l) = &mut *i.cond {
            let binders = pat_binders(&l.pat);
            self.expr_hole(&mut l.expr);
            self.block_hole(&mut i.then_branch, binders);
        } else if contains_let(&i.cond) {
            self.failed = true; // let-chains: keep as raw code
            return;
        } else {
            self.expr_hole(&mut i.cond);
            self.block_hole(&mut i.then_branch, vec![]);
        }
        if let Some((_, els)) = &mut i.else_branch {
            match &mut **els {
                Expr::If(inner) => self.if_chain(inner),
                Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => self.block_hole(&mut b.block, vec![]),
                other => self.expr_hole(other),
            }
        }
    }

    /// Special handling for the construct itself: constructs that bind variables.
    fn root(&mut self, e: &mut Expr) {
        match e {
            Expr::Closure(c) => {
                let binders = c.inputs.iter().flat_map(pat_binders).collect();
                self.body_hole(&mut c.body, binders);
            }
            Expr::Match(m) => {
                self.expr_hole(&mut m.expr);
                // comments between arms ride along as `#[__rn_comment("...")]` attributes
                {
                    use syn::spanned::Spanned;
                    let refs: Vec<&str> = self.lines.iter().map(String::as_str).collect();
                    let mut prev = m.brace_token.span.open().end();
                    for (i, arm) in m.arms.iter_mut().enumerate() {
                        let span = arm.span();
                        if span.start().line == 0 || refs.is_empty() {
                            break;
                        }
                        if let Some(c) = trim_blank(gap_text(&refs, prev, span.start()), i == 0, false) {
                            let mut attrs: Vec<syn::Attribute> = c
                                .lines()
                                .map(|l| {
                                    let lit = syn::LitStr::new(l, proc_macro2::Span::call_site());
                                    syn::parse_quote!(#[__rn_comment(#lit)])
                                })
                                .collect();
                            attrs.append(&mut arm.attrs);
                            arm.attrs = attrs;
                        }
                        prev = span.end();
                    }
                }
                for arm in &mut m.arms {
                    let binders = pat_binders(&arm.pat);
                    if let Some((_, g)) = &mut arm.guard {
                        self.expr_hole(g);
                    }
                    self.body_hole(&mut arm.body, binders);
                }
            }
            Expr::If(i) => self.if_chain(i),
            Expr::While(w) => {
                if let Expr::Let(l) = &mut *w.cond {
                    let binders = pat_binders(&l.pat);
                    self.expr_hole(&mut l.expr);
                    self.block_hole(&mut w.body, binders);
                } else if contains_let(&w.cond) {
                    self.failed = true;
                }
            }
            Expr::ForLoop(f) => {
                let binders = pat_binders(&f.pat);
                self.expr_hole(&mut f.expr);
                self.block_hole(&mut f.body, binders);
            }
            _ => {}
        }
    }
}

impl VisitMut for HoleCollector {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        if self.failed || is_placeholder(e) {
            return;
        }
        if !self.in_root {
            self.in_root = true;
            self.root(e);
            visit_mut::visit_expr_mut(self, e); // remaining direct children become holes
            return;
        }
        match e {
            // a block that's already a hole
            Expr::Block(b) if block_is_placeholder(&b.block) => {}
            // part of an if/else-if chain that was already flattened
            Expr::If(i) if block_is_placeholder(&i.then_branch) => visit_mut::visit_expr_if_mut(self, i),
            // `if let` scrutinee already handled
            Expr::Let(l) if is_placeholder(&l.expr) => {}
            Expr::Let(_) => self.failed = true,
            Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => {
                let i = self.payloads.len();
                let old = std::mem::replace(e, placeholder(i));
                let Expr::Block(b) = old else { unreachable!() };
                let br = braces(&b.block.brace_token);
                self.payloads.push(Payload::Block { stmts: b.block.stmts, binders: vec![], bare_ok: false, braces: br });
            }
            _ => self.expr_hole(e),
        }
    }
    fn visit_block_mut(&mut self, b: &mut syn::Block) {
        self.block_hole(b, vec![]);
    }
    fn visit_field_value_mut(&mut self, f: &mut syn::FieldValue) {
        // `Foo { x }` shorthand must become `Foo { x: <hole> }`
        if f.colon_token.is_none() {
            f.colon_token = Some(Default::default());
        }
        visit_mut::visit_field_value_mut(self, f);
    }
    // Patterns, types, attributes and macro tokens stay as written.
    fn visit_pat_mut(&mut self, _: &mut Pat) {}
    fn visit_type_mut(&mut self, _: &mut syn::Type) {}
    fn visit_attribute_mut(&mut self, _: &mut syn::Attribute) {}
    fn visit_macro_mut(&mut self, _: &mut syn::Macro) {}
}

/// Precedence of an expression, matching the table in codegen.
fn expr_prec(e: &Expr) -> u8 {
    use crate::codegen::{bin_prec, P_ATOM, P_LOWEST, P_UNARY};
    match e {
        Expr::Binary(b) => bin_prec(&b.op.to_token_stream().to_string()),
        Expr::Unary(_) | Expr::Reference(_) => P_UNARY,
        Expr::Cast(_) => 12,
        Expr::Range(_) | Expr::Assign(_) | Expr::Closure(_) | Expr::Return(_) | Expr::Break(_) | Expr::Let(_) => P_LOWEST,
        // block-like expressions sat here without parentheses, so anything fits
        Expr::If(_) | Expr::Match(_) | Expr::Block(_) | Expr::Unsafe(_) | Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_) => P_LOWEST,
        _ => P_ATOM,
    }
}

/// Replace hole placeholders in printed code with ⟦n⟧ markers.
/// For block holes in block position, `{ hole }` becomes just the marker (codegen adds the braces).
/// Other holes keep any braces the printer put around them (e.g. a long match arm wrapped in `{ }`).
fn fill_markers(code: &str, holes: &[Hole]) -> String {
    let mut out = String::new();
    let mut rest = code;
    while let Some(pos) = rest.find(HOLE) {
        let (before, after) = rest.split_at(pos);
        let digits: String = after[HOLE.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let mut tail = &after[HOLE.len() + digits.len()..];
        let mut before = before.to_string();
        let n: usize = digits.parse().unwrap_or(0);
        let block_pos = holes.get(n).is_some_and(|h| h.block && !h.bare_ok);
        // collapse "{ <hole> }"
        let trimmed = before.trim_end();
        if block_pos && trimmed.ends_with('{') && tail.trim_start().starts_with('}') {
            before.truncate(trimmed.len() - 1);
            tail = &tail.trim_start()[1..];
        }
        out.push_str(&before);
        out.push_str(&hole_marker(n));
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn is_assign_op(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
            | BinOp::BitXorAssign(_)
            | BinOp::BitAndAssign(_)
            | BinOp::BitOrAssign(_)
            | BinOp::ShlAssign(_)
            | BinOp::ShrAssign(_)
    )
}

// ---------- pretty printing helpers (via prettyplease) ----------

pub fn pretty_item(item: &syn::Item) -> String {
    prettyplease::unparse(&syn::File { shebang: None, attrs: vec![], items: vec![item.clone()] }).trim_end().to_string()
}

fn pretty_impl_item(ii: &syn::ImplItem) -> String {
    let imp: syn::ItemImpl = syn::parse_quote!(impl X { #ii });
    let s = pretty_item(&syn::Item::Impl(imp));
    inner_lines(&s)
}

fn pretty_type(t: &syn::Type) -> String {
    let e: syn::ItemType = syn::parse_quote!(type X = #t;);
    let s = pretty_item(&syn::Item::Type(e));
    s.trim_start_matches("type X = ").trim_end_matches(';').to_string()
}

pub fn pretty_stmts(stmts: Vec<Stmt>) -> String {
    let f = syn::ItemFn {
        attrs: vec![],
        vis: syn::Visibility::Inherited,
        sig: syn::parse_quote!(fn __f()),
        block: Box::new(syn::Block { brace_token: Default::default(), stmts }),
    };
    inner_lines(&pretty_item(&syn::Item::Fn(f)))
}

pub fn pretty_expr(e: &Expr) -> String {
    let s = pretty_stmts(vec![Stmt::Expr(e.clone(), None)]);
    // prettyplease prints some tail expressions (like `a = b`) with a `;`; an
    // expression never ends in `;` itself, so drop it
    match s.strip_suffix(';') {
        Some(t) => t.to_string(),
        None => s,
    }
}

/// Strip the first and last line of `header {\n ... \n}` and dedent by 4.
fn inner_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() < 3 {
        // `fn __f() { x }` printed on one line
        let open = s.find('{').map(|i| i + 1).unwrap_or(0);
        let close = s.rfind('}').unwrap_or(s.len());
        return s[open..close].trim().to_string();
    }
    lines[1..lines.len() - 1]
        .iter()
        .map(|l| l.strip_prefix("    ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}
