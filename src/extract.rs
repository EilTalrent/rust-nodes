//! "Extract function": turn a group of nodes into a real Rust helper function.
//!
//! * wires coming into the group become **parameters** (by value, `&` or `&mut`)
//! * wires going out of the group become **return values** (a tuple if several)
//! * the group is replaced by a call: `let x = helper(a, &mut b);`
//!
//! Things that would change meaning when moved into another function (`return`,
//! `?`, `break`/`continue` of a loop outside the group, ...) are refused with a reason.

use crate::graph::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Value,
    Ref,
    Mut,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Value => "value",
            Mode::Ref => "&",
            Mode::Mut => "&mut",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub mode: Mode,
    /// Base type (without & / &mut). Empty = unknown, the user must fill it in.
    pub ty: String,
    /// Where the value comes from, outside the group.
    pub src: (NodeId, usize),
    /// Inputs inside the group that use it.
    pub uses: Vec<(NodeId, usize)>,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    pub ty: String,
    /// The value inside the group.
    pub src: (NodeId, usize),
    /// Inputs outside the group that use it.
    pub users: Vec<(NodeId, usize)>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub name: String,
    pub group: NodeId,
    pub params: Vec<Param>,
    pub outputs: Vec<Output>,
    /// every node that moves into the helper (members, nested groups, container insides)
    pub moved: BTreeSet<NodeId>,
    pub first: NodeId,
    pub last: NodeId,
    /// exec wires coming into `first` from outside
    pub exec_in: Vec<(NodeId, usize)>,
    /// where execution continues after the group
    pub exec_out: Option<NodeId>,
}

impl Plan {
    pub fn signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                let ty = if p.ty.trim().is_empty() { "_" } else { p.ty.trim() };
                match p.mode {
                    Mode::Value => format!("{}: {ty}", p.name),
                    Mode::Ref => format!("{}: &{ty}", p.name),
                    Mode::Mut => format!("{}: &mut {ty}", p.name),
                }
            })
            .collect();
        let ty = |o: &Output| if o.ty.trim().is_empty() { "_".to_string() } else { o.ty.trim().to_string() };
        let ret = match self.outputs.len() {
            0 => String::new(),
            1 => format!(" -> {}", ty(&self.outputs[0])),
            _ => format!(" -> ({})", self.outputs.iter().map(ty).collect::<Vec<_>>().join(", ")),
        };
        format!("fn {}({}){ret}", self.name, params.join(", "))
    }

    /// Problems that stop the user from pressing Extract.
    pub fn problems(&self, existing_fns: &[String]) -> Vec<String> {
        let mut v = vec![];
        if syn::parse_str::<syn::Ident>(&self.name).is_err() {
            v.push(format!("`{}` isn't a valid function name", self.name));
        } else if existing_fns.iter().any(|n| n == &self.name) {
            v.push(format!("a function called `{}` already exists", self.name));
        }
        for p in &self.params {
            if p.ty.trim().is_empty() {
                v.push(format!("type of parameter `{}` is needed", p.name));
            }
        }
        for o in &self.outputs {
            if o.ty.trim().is_empty() {
                v.push(format!("type of returned `{}` is needed", o.name));
            }
        }
        if v.is_empty() && syn::parse_str::<syn::ItemFn>(&format!("{} {{}}", self.signature())).is_err() {
            v.push("the signature isn't valid Rust (check the types)".into());
        }
        v
    }
}

/// Every node inside group `g`, however deeply (nested groups and template containers).
pub fn descendants(f: &FnGraph, g: NodeId) -> BTreeSet<NodeId> {
    let mut set = BTreeSet::new();
    let mut stack = vec![g];
    while let Some(n) = stack.pop() {
        for (id, node) in &f.nodes {
            if node.scope.map(|s| s.0) == Some(n) && set.insert(*id) {
                stack.push(*id);
            }
        }
    }
    set
}

/// The real container a node is in, looking through visual groups.
fn structural(f: &FnGraph, mut s: Scope) -> Scope {
    for _ in 0..64 {
        match s {
            Some((owner, _)) if f.is_group(owner) => s = f.nodes[&owner].scope,
            _ => break,
        }
    }
    s
}

/// Is this node inside a closure container (where `return` / `?` stay local)?
fn inside_closure(f: &FnGraph, id: NodeId, stop: &BTreeSet<NodeId>) -> bool {
    let mut s = f.nodes[&id].scope;
    for _ in 0..64 {
        let Some((owner, _)) = s else { return false };
        if !stop.contains(&owner) && !f.is_group(owner) {
            return false;
        }
        if let NodeKind::Template { code, .. } = &f.nodes[&owner].kind {
            let t = code.trim_start();
            if t.starts_with('|') || t.starts_with("move") {
                return true;
            }
        }
        s = f.nodes[&owner].scope;
    }
    false
}

pub fn plan(f: &FnGraph, g: NodeId) -> Result<Plan, String> {
    let NodeKind::Group { name } = &f.nodes.get(&g).ok_or("no such group")?.kind else {
        return Err("only a group can be extracted: select nodes and collapse them first (Ctrl+G)".into());
    };
    let moved = descendants(f, g);
    let level = structural(f, f.nodes[&g].scope);
    // statements at the group's own level (not inside containers)
    let stmts: BTreeSet<NodeId> = moved
        .iter()
        .copied()
        .filter(|id| f.nodes[id].kind.is_exec() && structural(f, f.nodes[id].scope) == level)
        .collect();
    if stmts.is_empty() {
        return Err("the group has no statements (white exec wires) to run".into());
    }

    // ---- control flow in and out ----
    let exec_in_edges: Vec<&ExecEdge> = f.exec.iter().filter(|e| moved.contains(&e.to) && !moved.contains(&e.from)).collect();
    let firsts: BTreeSet<NodeId> = exec_in_edges.iter().map(|e| e.to).collect();
    if firsts.len() != 1 {
        return Err(if firsts.is_empty() {
            "nothing runs the group: connect a white exec wire into it".into()
        } else {
            "execution enters the group at more than one place: it must be one straight run of statements".into()
        });
    }
    let first = *firsts.iter().next().unwrap();
    let exits: Vec<&ExecEdge> = f.exec.iter().filter(|e| moved.contains(&e.from) && !moved.contains(&e.to)).collect();
    let exit_targets: BTreeSet<NodeId> = exits.iter().map(|e| e.to).collect();
    if exit_targets.len() > 1 {
        return Err("execution leaves the group at more than one place".into());
    }
    // walk the main chain
    let mut chain = vec![];
    let mut cur = Some(first);
    while let Some(id) = cur {
        if !moved.contains(&id) || chain.contains(&id) {
            break;
        }
        chain.push(id);
        cur = f.nodes[&id].kind.next_pin().and_then(|p| f.exec_target(id, p));
    }
    let last = *chain.last().unwrap();
    for e in &exits {
        if e.from != last || f.nodes[&last].kind.next_pin() != Some(e.from_pin) {
            return Err("a branch inside the group jumps to a statement outside it".into());
        }
    }
    // every statement must be reachable from `first` without leaving the group
    let mut reach = BTreeSet::new();
    let mut stack = vec![first];
    while let Some(id) = stack.pop() {
        if !moved.contains(&id) || !reach.insert(id) {
            continue;
        }
        for pin in 0..f.nodes[&id].kind.exec_outs().len() {
            if let Some(t) = f.exec_target(id, pin) {
                stack.push(t);
            }
        }
    }
    if let Some(stray) = stmts.iter().find(|id| !reach.contains(id)) {
        return Err(format!("`{}` in the group isn't connected to the rest of the group's statements", f.nodes[stray].kind.title()));
    }

    // ---- things that mean something different inside another function ----
    // loop bodies inside the group (break/continue there are fine)
    let mut in_loop = BTreeSet::new();
    for id in &moved {
        if matches!(f.nodes[id].kind, NodeKind::While | NodeKind::Loop | NodeKind::For { .. }) {
            let mut st: Vec<NodeId> = f.exec_target(*id, 0).into_iter().collect();
            while let Some(n) = st.pop() {
                if in_loop.insert(n) {
                    for pin in 0..f.nodes[&n].kind.exec_outs().len() {
                        if let Some(t) = f.exec_target(n, pin) {
                            st.push(t);
                        }
                    }
                }
            }
        }
    }
    for id in &moved {
        let k = &f.nodes[id].kind;
        let closure = inside_closure(f, *id, &moved);
        match k {
            NodeKind::Return if !closure => return Err("the group contains `return`, which would only leave the new function".into()),
            NodeKind::BlockValue if structural(f, f.nodes[id].scope) == level => {
                return Err("the group contains the block's Result node".into())
            }
            NodeKind::Break | NodeKind::Continue if !in_loop.contains(id) && !closure => {
                return Err(format!("the group has `{}` for a loop outside the group", k.title().to_lowercase()))
            }
            _ => {}
        }
        let code = match k {
            NodeKind::RawStmt { code } | NodeKind::RawExpr { code } | NodeKind::Template { code, .. } | NodeKind::TemplateStmt { code, .. } => code.as_str(),
            _ => "",
        };
        if !closure && (code.contains("⟧?") || code.trim_end().ends_with('?') || code.contains("return") || code.contains("break '") || code.contains("continue '")) {
            return Err("the group contains `?`, `return` or a labelled break, which would behave differently in a new function".into());
        }
    }

    // ---- parameters: values flowing in ----
    let mut params: Vec<Param> = vec![];
    for e in f.data.iter().filter(|e| moved.contains(&e.to) && !moved.contains(&e.from)) {
        let src = (e.from, e.from_pin);
        match params.iter_mut().find(|p| p.src == src) {
            Some(p) => p.uses.push((e.to, e.to_pin)),
            None => params.push(Param { name: String::new(), mode: Mode::Value, ty: String::new(), src, uses: vec![(e.to, e.to_pin)] }),
        }
    }
    let mut taken: Vec<String> = vec![];
    for (k, p) in params.iter_mut().enumerate() {
        p.name = unique(var_name(f, p.src).unwrap_or_else(|| format!("arg{k}")), &mut taken);
        p.ty = guess_type(f, p.src, 0);
        let src_mut = matches!(&f.nodes[&p.src.0].kind, NodeKind::Let { mutable: true, .. });
        let mutated = p.uses.iter().any(|(n, pin)| match &f.nodes[n].kind {
            NodeKind::Assign { .. } => *pin == 0,
            NodeKind::Unary { op } => op.trim() == "&mut",
            NodeKind::Method { .. } => *pin == 0 && src_mut,
            _ => false,
        });
        if mutated {
            p.mode = Mode::Mut;
        }
    }

    // ---- outputs: values flowing out ----
    let mut outputs: Vec<Output> = vec![];
    for e in f.data.iter().filter(|e| moved.contains(&e.from) && !moved.contains(&e.to)) {
        let src = (e.from, e.from_pin);
        match outputs.iter_mut().find(|o| o.src == src) {
            Some(o) => o.users.push((e.to, e.to_pin)),
            None => outputs.push(Output { name: String::new(), ty: String::new(), src, users: vec![(e.to, e.to_pin)] }),
        }
    }
    let mut taken_out: Vec<String> = vec![];
    for (k, o) in outputs.iter_mut().enumerate() {
        o.name = unique(var_name(f, o.src).unwrap_or_else(|| format!("out{k}")), &mut taken_out);
        o.ty = guess_type(f, o.src, 0);
    }

    Ok(Plan {
        name: snake(name),
        group: g,
        params,
        outputs,
        moved,
        first,
        last,
        exec_in: exec_in_edges.iter().map(|e| (e.from, e.from_pin)).collect(),
        exec_out: exit_targets.into_iter().next(),
    })
}

fn unique(base: String, taken: &mut Vec<String>) -> String {
    let mut name = base.clone();
    let mut i = 2;
    while taken.contains(&name) {
        name = format!("{base}{i}");
        i += 1;
    }
    taken.push(name.clone());
    name
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) { format!("helper_{out}") } else { out }
}

/// A variable name for a value, if it has one (let, parameter, loop variable, binder).
fn var_name(f: &FnGraph, (id, pin): (NodeId, usize)) -> Option<String> {
    let n = f.nodes.get(&id)?;
    let name = match &n.kind {
        NodeKind::Let { name, .. } => name.clone(),
        NodeKind::For { var } => var.clone(),
        NodeKind::Entry => f.params.get(pin)?.clone(),
        NodeKind::BlockEntry { binders } => binders.get(pin)?.clone(),
        NodeKind::Var { name } if syn::parse_str::<syn::Ident>(name).is_ok() => name.clone(),
        _ => return None,
    };
    (syn::parse_str::<syn::Ident>(&name).is_ok() && name != "self").then_some(name)
}

/// Best-effort type guess. Empty string = don't know.
pub fn guess_type(f: &FnGraph, (id, pin): (NodeId, usize), depth: usize) -> String {
    if depth > 16 {
        return String::new();
    }
    let Some(n) = f.nodes.get(&id) else { return String::new() };
    let input = |p: usize| f.data_source(id, p).map(|s| guess_type(f, s, depth + 1)).unwrap_or_default();
    match &n.kind {
        NodeKind::Let { ty, .. } if !ty.is_empty() => ty.clone(),
        NodeKind::Let { .. } => input(0),
        NodeKind::Entry => param_type(&f.sig, pin),
        NodeKind::Literal { code } => literal_type(code),
        NodeKind::Binary { op } => match op.as_str() {
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => "bool".into(),
            _ => {
                let a = input(0);
                if a.is_empty() { input(1) } else { a }
            }
        },
        NodeKind::Unary { op } => match op.trim() {
            "!" | "-" => input(0),
            "&" => prefix("&", input(0)),
            "&mut" => prefix("&mut ", input(0)),
            "*" => {
                let t = input(0);
                t.strip_prefix("&mut ").or_else(|| t.strip_prefix('&')).unwrap_or("").to_string()
            }
            _ => String::new(),
        },
        NodeKind::Method { name, .. } => match name.as_str() {
            "len" | "count" => "usize".into(),
            "to_string" | "to_uppercase" | "to_lowercase" | "to_owned" if input(0).contains("str") || input(0) == "String" => "String".into(),
            "to_string" => "String".into(),
            "clone" | "abs" | "min" | "max" | "pow" => input(0),
            "is_empty" | "contains" | "starts_with" | "ends_with" | "is_some" | "is_none" => "bool".into(),
            _ => String::new(),
        },
        NodeKind::Macro { name, .. } if name == "format" => "String".into(),
        NodeKind::Macro { name, args, .. } if name == "vec" && *args > 0 => {
            let t = input(0);
            if t.is_empty() { String::new() } else { format!("Vec<{t}>") }
        }
        _ => String::new(),
    }
}

fn prefix(p: &str, t: String) -> String {
    if t.is_empty() { t } else { format!("{p}{t}") }
}

fn param_type(sig: &str, pin: usize) -> String {
    let Ok(item) = syn::parse_str::<syn::ItemFn>(&format!("{sig} {{}}")) else { return String::new() };
    match item.sig.inputs.iter().nth(pin) {
        Some(syn::FnArg::Typed(pt)) => {
            let t = &pt.ty;
            let tokens = quote::quote!(#t).to_string();
            // tidy token spacing: "& str" -> "&str", "Vec < i32 >" -> "Vec<i32>"
            tokens.replace("& ", "&").replace(" < ", "<").replace(" >", ">").replace("< ", "<").replace(" ,", ",").replace("mut ", "mut ").replace("[ ", "[").replace(" ]", "]")
        }
        _ => String::new(),
    }
}

fn literal_type(code: &str) -> String {
    let Ok(lit) = syn::parse_str::<syn::Lit>(code) else { return String::new() };
    match lit {
        syn::Lit::Str(_) => "&str".into(),
        syn::Lit::Char(_) => "char".into(),
        syn::Lit::Bool(_) => "bool".into(),
        syn::Lit::Int(i) => if i.suffix().is_empty() { "i32".into() } else { i.suffix().into() },
        syn::Lit::Float(x) => if x.suffix().is_empty() { "f64".into() } else { x.suffix().into() },
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------

/// Move the group into a new function and put a call where it was.
/// `fn_path` is the caller's path in `program`. Returns the helper's path.
pub fn apply(program: &mut Program, fn_path: &[usize], plan: &Plan) -> Result<Vec<usize>, String> {
    let existing: Vec<String> = program.functions().iter().map(|(_, f)| f.name.clone()).collect();
    let problems = plan.problems(&existing);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let f = program.function_mut(fn_path).ok_or("function not found")?;
    let g = plan.group;
    let g_scope = f.nodes[&g].scope;
    let g_pos = f.nodes[&g].pos;

    // ---------------- build the helper ----------------
    let sig = plan.signature();
    let mut helper = FnGraph {
        name: plan.name.clone(),
        sig,
        params: plan.params.iter().map(|p| p.name.clone()).collect(),
        nodes: Default::default(),
        next_id: f.next_id,
        exec: vec![],
        data: vec![],
        entry: 0,
    };
    for id in &plan.moved {
        let mut n = f.nodes[id].clone();
        if n.scope == Some((g, 0)) {
            n.scope = None; // direct members become the helper's top level
        }
        helper.nodes.insert(*id, n);
    }
    helper.exec = f.exec.iter().copied().filter(|e| plan.moved.contains(&e.from) && plan.moved.contains(&e.to)).collect();
    helper.data = f.data.iter().copied().filter(|e| plan.moved.contains(&e.from) && plan.moved.contains(&e.to)).collect();
    let xs = || plan.moved.iter().filter(|id| helper_top(&helper, **id)).map(|id| helper.nodes[id].pos);
    let min_x = xs().map(|p| p[0]).fold(f32::MAX, f32::min);
    let max_x = xs().map(|p| p[0]).fold(f32::MIN, f32::max);
    let min_y = xs().map(|p| p[1]).fold(f32::MAX, f32::min);
    let entry = helper.add(NodeKind::Entry, [min_x - 260.0, min_y]);
    helper.entry = entry;
    helper.connect_exec(entry, 0, plan.first);
    for (k, p) in plan.params.iter().enumerate() {
        for &(n, pin) in &p.uses {
            match p.mode {
                Mode::Value => helper.connect_data(entry, k, n, pin),
                Mode::Ref | Mode::Mut => {
                    // the parameter is a reference: use `*name` where the value was used
                    let at = helper.nodes[&n].pos;
                    let scope = helper.nodes[&n].scope;
                    let d = helper.add_in(NodeKind::Unary { op: "*".into() }, [at[0] - 170.0, at[1] + 30.0], scope);
                    helper.connect_data(entry, k, d, 0);
                    helper.connect_data(d, 0, n, pin);
                }
            }
        }
    }
    if !plan.outputs.is_empty() {
        let ret = helper.add(NodeKind::Return, [max_x + 260.0, min_y]);
        let last_next = helper.nodes[&plan.last].kind.next_pin().unwrap_or(0);
        helper.connect_exec(plan.last, last_next, ret);
        if plan.outputs.len() == 1 {
            let (s, sp) = plan.outputs[0].src;
            helper.connect_data(s, sp, ret, 0);
        } else {
            let holes: Vec<Hole> = plan.outputs.iter().map(|_| Hole { block: false, binders: vec![], bare_ok: false, prec: 0 }).collect();
            let code = format!("({})", (0..holes.len()).map(hole_marker).collect::<Vec<_>>().join(", "));
            let tuple = helper.add(NodeKind::Template { code, holes }, [max_x + 60.0, min_y + 120.0]);
            for (k, o) in plan.outputs.iter().enumerate() {
                helper.connect_data(o.src.0, o.src.1, tuple, k);
            }
            helper.connect_data(tuple, 0, ret, 0);
        }
    }
    f.next_id = helper.next_id;

    // ---------------- replace the group with a call ----------------
    for id in plan.moved.iter().chain([&g]) {
        f.nodes.remove(id);
    }
    f.exec.retain(|e| !plan.moved.contains(&e.from) && !plan.moved.contains(&e.to));
    f.data.retain(|e| !plan.moved.contains(&e.from) && !plan.moved.contains(&e.to));

    let [gx, gy] = g_pos;
    let call = f.add_in(NodeKind::Call { path: plan.name.clone(), args: plan.params.len() }, [gx - 40.0, gy + 60.0], g_scope);
    for (k, p) in plan.params.iter().enumerate() {
        let (s, sp) = p.src;
        match p.mode {
            Mode::Value => f.connect_data(s, sp, call, k),
            Mode::Ref | Mode::Mut => {
                let op = if p.mode == Mode::Mut { "&mut " } else { "&" };
                let r = f.add_in(NodeKind::Unary { op: op.into() }, [gx - 220.0, gy + 60.0 + k as f32 * 50.0], g_scope);
                f.connect_data(s, sp, r, 0);
                f.connect_data(r, 0, call, k);
            }
        }
    }
    // the statement(s) that make the call
    let mut stmts: Vec<NodeId> = vec![];
    let mut out_lets: Vec<NodeId> = vec![];
    match plan.outputs.len() {
        0 => {
            let ev = f.add_in(NodeKind::Eval, [gx + 140.0, gy], g_scope);
            f.connect_data(call, 0, ev, 0);
            stmts.push(ev);
        }
        1 => {
            let o = &plan.outputs[0];
            let l = f.add_in(NodeKind::Let { name: o.name.clone(), mutable: false, ty: String::new() }, [gx + 140.0, gy], g_scope);
            f.connect_data(call, 0, l, 0);
            stmts.push(l);
            out_lets.push(l);
        }
        _ => {
            let tuple = f.add_in(NodeKind::Let { name: format!("{}_out", plan.name), mutable: false, ty: String::new() }, [gx + 140.0, gy], g_scope);
            f.connect_data(call, 0, tuple, 0);
            stmts.push(tuple);
            for (k, o) in plan.outputs.iter().enumerate() {
                let x = gx + 380.0 + k as f32 * 240.0;
                let field = f.add_in(NodeKind::Field { name: k.to_string() }, [x - 30.0, gy + 60.0], g_scope);
                f.connect_data(tuple, 0, field, 0);
                let l = f.add_in(NodeKind::Let { name: o.name.clone(), mutable: false, ty: String::new() }, [x, gy], g_scope);
                f.connect_data(field, 0, l, 0);
                stmts.push(l);
                out_lets.push(l);
            }
        }
    }
    for w in stmts.windows(2) {
        f.connect_exec(w[0], 0, w[1]);
    }
    for &(from, pin) in &plan.exec_in {
        f.connect_exec(from, pin, stmts[0]);
    }
    if let Some(t) = plan.exec_out {
        f.connect_exec(*stmts.last().unwrap(), 0, t);
    }
    for (k, o) in plan.outputs.iter().enumerate() {
        for &(n, pin) in &o.users {
            f.connect_data(out_lets[k], 0, n, pin);
        }
    }

    // ---------------- add the helper right after the caller ----------------
    let at = fn_path[0] + 1;
    program.items.insert(at, Item::Func(helper));
    Ok(vec![at])
}

fn helper_top(h: &FnGraph, id: NodeId) -> bool {
    h.nodes.get(&id).is_some_and(|n| n.scope.is_none())
}

/// Names of all functions, for the "already exists" check.
pub fn function_names(p: &Program) -> Vec<String> {
    p.functions().iter().map(|(_, f)| f.name.clone()).collect()
}

// ---------------------------------------------------------------------------
// Asking the compiler for types.

/// What a probe is asking about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeTarget {
    Param(usize),
    Output(usize),
}

pub fn probe_var(i: usize) -> String {
    format!("__rn_probe_{i}")
}

/// A copy of the program's code with `let __rn_probe_N: () = value;` lines added
/// just before the group (for parameters) and just after it (for return values).
/// The compiler rejects each one with "expected `()`, found `TYPE`".
pub fn probe_code(program: &Program, fn_path: &[usize], plan: &Plan) -> Result<(String, Vec<ProbeTarget>), String> {
    let mut program = program.clone();
    let f = program.function_mut(fn_path).ok_or("function not found")?;
    let scope = f.nodes.get(&plan.first).ok_or("group changed, reopen the dialog")?.scope;
    let mut targets = vec![];
    let make = |f: &mut FnGraph, src: (NodeId, usize), t: ProbeTarget, targets: &mut Vec<ProbeTarget>| -> NodeId {
        let i = targets.len();
        targets.push(t);
        let hole = Hole { block: false, binders: vec![], bare_ok: false, prec: 0 };
        let code = format!("let {}: () = {};", probe_var(i), hole_marker(0));
        let id = f.add_in(NodeKind::TemplateStmt { code, holes: vec![hole] }, [0.0, 0.0], scope);
        f.connect_data(src.0, src.1, id, 0);
        id
    };
    // before the group
    let before: Vec<NodeId> = plan.params.iter().enumerate().map(|(k, p)| make(f, p.src, ProbeTarget::Param(k), &mut targets)).collect();
    if let Some(&head) = before.first() {
        for w in before.windows(2) {
            f.connect_exec(w[0], 0, w[1]);
        }
        for &(from, pin) in &plan.exec_in {
            f.connect_exec(from, pin, head);
        }
        f.connect_exec(*before.last().unwrap(), 0, plan.first);
    }
    // after the group
    let after: Vec<NodeId> = plan.outputs.iter().enumerate().map(|(k, o)| make(f, o.src, ProbeTarget::Output(k), &mut targets)).collect();
    if let Some(&head) = after.first() {
        for w in after.windows(2) {
            f.connect_exec(w[0], 0, w[1]);
        }
        let pin = f.nodes[&plan.last].kind.next_pin().unwrap_or(0);
        f.connect_exec(plan.last, pin, head);
        if let Some(t) = plan.exec_out {
            f.connect_exec(*after.last().unwrap(), 0, t);
        }
    }
    let g = crate::codegen::generate(&program);
    if let Some(e) = g.error {
        return Err(format!("couldn't build the probe program: {e}"));
    }
    Ok((g.code, targets))
}

/// Turn the compiler's "found ..." text into a type to write in a signature.
/// Returns Err with a note for things that can't be named (closures, ...).
pub fn type_from_found(found: &str) -> Result<String, String> {
    let f = found.trim();
    if let Some(t) = f.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        if t.contains("{closure") || t.contains("{async") || t.contains("impl ") {
            return Err(format!("`{t}` can't be written as a type"));
        }
        return Ok(t.replace("{integer}", "i32").replace("{float}", "f64"));
    }
    match f {
        "integer" => Ok("i32".into()),
        "floating-point number" => Ok("f64".into()),
        "closure" => Err("a closure can't be written as a type (use `impl Fn(..)` by hand)".into()),
        // older/other wordings: "struct `Vec<i32>`", "enum `Option<u8>`", "reference `&str`"
        other => match other.split_once('`') {
            Some((_, rest)) => type_from_found(&format!("`{}", rest)),
            None => Err(format!("compiler said: {other}")),
        },
    }
}
