//! Rust source → node graph, using `syn`.
//!
//! Everything the graph can represent becomes nodes and wires. Anything it can't
//! (match, closures, if-let, labelled loops, ...) is kept as a raw-code node, so no
//! code is ever lost.

use crate::graph::*;
use quote::ToTokens;
use std::collections::HashMap;
use syn::punctuated::Punctuated;
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
    let mut b = Builder { g: FnGraph::new(name, sig_text, params.clone()), scopes: vec![HashMap::new()] };
    let entry = b.g.entry;
    for (i, p) in params.iter().enumerate() {
        b.scopes[0].insert(p.clone(), (entry, i));
    }
    b.block(&block.stmts, Some((entry, 0)), true);
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

struct Builder {
    g: FnGraph,
    scopes: Vec<HashMap<String, (NodeId, usize)>>,
}

impl Builder {
    fn lookup(&self, name: &str) -> Option<(NodeId, usize)> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn exec_node(&mut self, kind: NodeKind, port: Port) -> NodeId {
        let id = self.g.add(kind, [0.0, 0.0]);
        if let Some((n, p)) = port {
            self.g.connect_exec(n, p, id);
        }
        id
    }

    fn wire(&mut self, e: &Expr, to: NodeId, pin: usize) {
        let (n, p) = self.expr(e);
        self.g.connect_data(n, p, to, pin);
    }

    fn raw_stmt(&mut self, s: &Stmt, port: Port) -> Port {
        let id = self.exec_node(NodeKind::RawStmt { code: pretty_stmts(vec![s.clone()]) }, port);
        Some((id, 0))
    }

    fn block(&mut self, stmts: &[Stmt], port: Port, tail_return: bool) -> Port {
        self.scopes.push(HashMap::new());
        let mut port = port;
        for (i, s) in stmts.iter().enumerate() {
            let last = i + 1 == stmts.len();
            port = self.stmt(s, port, tail_return && last);
        }
        self.scopes.pop();
        port
    }

    fn stmt(&mut self, s: &Stmt, port: Port, tail_ctx: bool) -> Port {
        match s {
            Stmt::Local(local) => {
                if local.init.as_ref().is_some_and(|i| i.diverge.is_some()) {
                    return self.raw_stmt(s, port); // let-else
                }
                let (pat, ty) = match &local.pat {
                    Pat::Type(pt) => (&*pt.pat, pretty_type(&pt.ty)),
                    p => (p, String::new()),
                };
                let Pat::Ident(pi) = pat else { return self.raw_stmt(s, port) };
                if pi.by_ref.is_some() || pi.subpat.is_some() {
                    return self.raw_stmt(s, port);
                }
                let name = pi.ident.to_string();
                let id = self.exec_node(NodeKind::Let { name: name.clone(), mutable: pi.mutability.is_some(), ty }, port);
                if let Some(init) = &local.init {
                    self.wire(&init.expr, id, 0); // before registering the name: `let x = x + 1` shadows
                }
                self.scopes.last_mut().unwrap().insert(name, (id, 0));
                Some((id, 0))
            }
            Stmt::Item(_) => self.raw_stmt(s, port),
            Stmt::Macro(m) => {
                let id = self.exec_node(NodeKind::Eval, port);
                let (n, p) = self.macro_node(&m.mac);
                self.g.connect_data(n, p, id, 0);
                Some((id, 0))
            }
            Stmt::Expr(e, semi) => self.expr_stmt(s, e, port, tail_ctx && semi.is_none(), tail_ctx),
        }
    }

    /// `value_tail`: this expression is the function's return value.
    /// `tail_ctx`: we're in tail position of the function (branch tails are return values).
    fn expr_stmt(&mut self, s: &Stmt, e: &Expr, port: Port, value_tail: bool, tail_ctx: bool) -> Port {
        match e {
            Expr::If(ei) => {
                if matches!(&*ei.cond, Expr::Let(_)) {
                    return self.raw_stmt(s, port);
                }
                let id = self.exec_node(NodeKind::If, port);
                self.wire(&ei.cond, id, 0);
                self.block(&ei.then_branch.stmts, Some((id, 0)), value_tail);
                if let Some((_, els)) = &ei.else_branch {
                    match &**els {
                        Expr::Block(b) if b.label.is_none() => {
                            self.block(&b.block.stmts, Some((id, 1)), value_tail);
                        }
                        Expr::If(_) => {
                            let inner = Stmt::Expr((**els).clone(), None);
                            self.expr_stmt(&inner, els, Some((id, 1)), value_tail, tail_ctx);
                        }
                        _ => {
                            let inner = Stmt::Expr((**els).clone(), None);
                            self.raw_stmt(&inner, Some((id, 1)));
                        }
                    }
                }
                Some((id, 2))
            }
            Expr::While(w) if w.label.is_none() && !matches!(&*w.cond, Expr::Let(_)) => {
                let id = self.exec_node(NodeKind::While, port);
                self.wire(&w.cond, id, 0);
                self.block(&w.body.stmts, Some((id, 0)), false);
                Some((id, 1))
            }
            Expr::Loop(l) if l.label.is_none() && !value_tail => {
                let id = self.exec_node(NodeKind::Loop, port);
                self.block(&l.body.stmts, Some((id, 0)), false);
                Some((id, 1))
            }
            Expr::ForLoop(f) if f.label.is_none() => {
                let Pat::Ident(pi) = &*f.pat else { return self.raw_stmt(s, port) };
                if pi.by_ref.is_some() || pi.subpat.is_some() || pi.mutability.is_some() {
                    return self.raw_stmt(s, port);
                }
                let var = pi.ident.to_string();
                let id = self.exec_node(NodeKind::For { var: var.clone() }, port);
                self.wire(&f.expr, id, 0);
                self.scopes.push(HashMap::from([(var, (id, 0))]));
                self.block(&f.body.stmts, Some((id, 0)), false);
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
            Expr::Assign(a) if !value_tail => {
                let id = self.exec_node(NodeKind::Assign { op: "=".into() }, port);
                self.wire(&a.left, id, 0);
                self.wire(&a.right, id, 1);
                Some((id, 0))
            }
            Expr::Binary(b) if is_assign_op(&b.op) && !value_tail => {
                let id = self.exec_node(NodeKind::Assign { op: b.op.to_token_stream().to_string() }, port);
                self.wire(&b.left, id, 0);
                self.wire(&b.right, id, 1);
                Some((id, 0))
            }
            Expr::Block(_) | Expr::Unsafe(_) | Expr::Match(_) | Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_)
            | Expr::Break(_) | Expr::Const(_) | Expr::TryBlock(_) => self.raw_stmt(s, port),
            _ if value_tail => {
                let id = self.exec_node(NodeKind::Return, port);
                self.wire(e, id, 0);
                None
            }
            _ => {
                let id = self.exec_node(NodeKind::Eval, port);
                self.wire(e, id, 0);
                Some((id, 0))
            }
        }
    }

    fn pure(&mut self, kind: NodeKind, inputs: &[&Expr]) -> (NodeId, usize) {
        let id = self.g.add(kind, [0.0, 0.0]);
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
                    _ => return self.raw_expr(e),
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
            _ => self.raw_expr(e),
        }
    }

    fn raw_expr(&mut self, e: &Expr) -> (NodeId, usize) {
        self.pure(NodeKind::RawExpr { code: pretty_expr(e) }, &[])
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
        self.raw_expr(&e)
    }
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
