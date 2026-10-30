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
    let mut items = vec![];
    for attr in &file.attrs {
        // inner attributes like #![allow(...)]
        items.push(Item::Raw(attr.to_token_stream().to_string()));
    }
    for item in &file.items {
        items.push(convert_item(item));
    }
    Ok(Program { items })
}

fn convert_item(item: &syn::Item) -> Item {
    match item {
        syn::Item::Fn(f) => {
            let name = f.sig.ident.to_string();
            Item::Func(build_fn(name, &f.attrs, &f.vis, &f.sig, &f.block))
        }
        syn::Item::Impl(imp) => {
            let mut empty = imp.clone();
            empty.items.clear();
            let header = pretty_item(&syn::Item::Impl(empty));
            let header = header.trim_end().trim_end_matches("{}").trim_end().to_string();
            let self_ty = pretty_type(&imp.self_ty);
            let items = imp
                .items
                .iter()
                .map(|ii| match ii {
                    syn::ImplItem::Fn(f) => {
                        let name = format!("{self_ty}::{}", f.sig.ident);
                        Item::Func(build_fn(name, &f.attrs, &f.vis, &f.sig, &f.block))
                    }
                    other => Item::Raw(pretty_impl_item(other)),
                })
                .collect();
            Item::Impl { header, items }
        }
        other => Item::Raw(pretty_item(other)),
    }
}

/// Build a graph for one function body.
pub fn build_fn(name: String, attrs: &[syn::Attribute], vis: &syn::Visibility, sig: &syn::Signature, block: &syn::Block) -> FnGraph {
    let sig_text = signature_text(attrs, vis, sig);
    let params = param_names(sig);
    let mut b = Builder { g: FnGraph::new(name, sig_text, params.clone()), scopes: vec![HashMap::new()], cur: None };
    let entry = b.g.entry;
    for (i, p) in params.iter().enumerate() {
        b.scopes[0].insert(p.clone(), (entry, i));
    }
    b.block(&block.stmts, Some((entry, 0)), Tail::Return);
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

struct Builder {
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
        let mut col = HoleCollector::default();
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
        let code = fill_markers(&pretty_stmts(vec![s2]));
        let holes = col.holes();
        let id = self.exec_node(NodeKind::TemplateStmt { code, holes }, port);
        self.fill_holes(id, col.payloads);
        Some((id, 0))
    }

    fn template_expr(&mut self, e: &Expr) -> (NodeId, usize) {
        let mut col = HoleCollector::default();
        let mut e2 = e.clone();
        col.visit_expr_mut(&mut e2);
        if col.failed || col.payloads.is_empty() {
            return self.pure(NodeKind::RawExpr { code: pretty_expr(e) }, &[]);
        }
        let code = fill_markers(&pretty_expr(&e2));
        let holes = col.holes();
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
                Payload::Block { stmts, binders, .. } => {
                    let saved = self.cur;
                    self.cur = Some((id, i));
                    let be = self.add(NodeKind::BlockEntry { binders: binders.clone() });
                    self.scopes.push(binders.iter().enumerate().map(|(j, b)| (b.clone(), (be, j))).collect());
                    self.block(&stmts, Some((be, 0)), Tail::Value);
                    self.scopes.pop();
                    self.cur = saved;
                }
            }
        }
    }

    fn block(&mut self, stmts: &[Stmt], port: Port, tail: Tail) -> Port {
        self.scopes.push(HashMap::new());
        let mut port = port;
        for (i, s) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            port = self.stmt(s, port, if last { tail } else { Tail::None });
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
                self.block(&ei.then_branch.stmts, Some((id, 0)), value_tail);
                if let Some((_, els)) = &ei.else_branch {
                    let inner = Stmt::Expr((**els).clone(), None);
                    match &**els {
                        Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => {
                            self.block(&b.block.stmts, Some((id, 1)), value_tail);
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
                self.block(&w.body.stmts, Some((id, 0)), Tail::None);
                Some((id, 1))
            }
            Expr::Loop(l) if l.label.is_none() && !is_tail => {
                let id = self.exec_node(NodeKind::Loop, port);
                self.block(&l.body.stmts, Some((id, 0)), Tail::None);
                Some((id, 1))
            }
            Expr::ForLoop(f) if f.label.is_none() && simple_ident(&f.pat).is_some() => {
                let var = simple_ident(&f.pat).unwrap();
                let id = self.exec_node(NodeKind::For { var: var.clone() }, port);
                self.wire(&f.expr, id, 0);
                self.scopes.push(HashMap::from([(var, (id, 0))]));
                self.block(&f.body.stmts, Some((id, 0)), Tail::None);
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
        let e = Expr::Macro(syn::ExprMacro { attrs: vec![], mac: mac.clone() });
        self.pure(NodeKind::RawExpr { code: pretty_expr(&e) }, &[])
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

const HOLE: &str = "__rust_nodes_hole_";

enum Payload {
    Expr(Expr, u8),
    Block { stmts: Vec<Stmt>, binders: Vec<String>, bare_ok: bool },
}

/// Walks one construct and replaces its direct sub-expressions with hole placeholders.
/// Blocks become container holes; variables bound by patterns (match arms, closures,
/// `if let`, `while let`, `for`) become inputs of that container.
#[derive(Default)]
struct HoleCollector {
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
        let stmts = match old {
            Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => b.block.stmts,
            other => vec![Stmt::Expr(other, None)],
        };
        self.payloads.push(Payload::Block { stmts, binders, bare_ok: true });
    }

    /// Turn a `{ ... }` block into a container.
    fn block_hole(&mut self, b: &mut syn::Block, binders: Vec<String>) {
        if block_is_placeholder(b) {
            return;
        }
        let i = self.payloads.len();
        let stmts = std::mem::replace(&mut b.stmts, vec![Stmt::Expr(placeholder(i), None)]);
        self.payloads.push(Payload::Block { stmts, binders, bare_ok: false });
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
                for arm in &mut m.arms {
                    let binders = pat_binders(&arm.pat);
                    if let Some((_, g)) = &mut arm.guard {
                        self.expr_hole(g);
                    }
                    self.body_hole(&mut arm.body, binders);
                }
            }
            Expr::If(i) => {
                if let Expr::Let(l) = &mut *i.cond {
                    let binders = pat_binders(&l.pat);
                    self.expr_hole(&mut l.expr);
                    self.block_hole(&mut i.then_branch, binders);
                } else if contains_let(&i.cond) {
                    self.failed = true; // let-chains: keep as raw code
                }
            }
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
            // `if let` scrutinee already handled
            Expr::Let(l) if is_placeholder(&l.expr) => {}
            Expr::Let(_) => self.failed = true,
            Expr::Block(b) if b.label.is_none() && b.attrs.is_empty() => {
                let i = self.payloads.len();
                let old = std::mem::replace(e, placeholder(i));
                let Expr::Block(b) = old else { unreachable!() };
                self.payloads.push(Payload::Block { stmts: b.block.stmts, binders: vec![], bare_ok: false });
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
        _ => P_ATOM,
    }
}

/// Replace hole placeholders in printed code with ⟦n⟧ markers.
/// `{ hole }` (a block that is only a placeholder) becomes just the marker.
fn fill_markers(code: &str) -> String {
    let mut out = String::new();
    let mut rest = code;
    while let Some(pos) = rest.find(HOLE) {
        let (before, after) = rest.split_at(pos);
        let digits: String = after[HOLE.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let mut tail = &after[HOLE.len() + digits.len()..];
        let mut before = before.to_string();
        // collapse "{ <hole> }"
        let trimmed = before.trim_end();
        if trimmed.ends_with('{') && tail.trim_start().starts_with('}') {
            before.truncate(trimmed.len() - 1);
            tail = &tail.trim_start()[1..];
        }
        out.push_str(&before);
        out.push_str(&hole_marker(digits.parse().unwrap_or(0)));
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
    pretty_stmts(vec![Stmt::Expr(e.clone(), None)])
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
