//! The node-graph model.
//!
//! A [`Program`] mirrors a Rust source file: a list of items. Free functions and
//! methods inside `impl` blocks become editable [`FnGraph`]s; everything else
//! (structs, enums, `use`, traits, ...) is kept as raw Rust text.
//!
//! Inside a function there are two kinds of wires, just like Unreal Blueprints:
//! * **exec wires** decide the order statements run in (white, triangle pins)
//! * **data wires** carry values from one node to another (coloured, round pins)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type NodeId = u32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Item {
    /// Any item we don't turn into a graph, kept verbatim (pretty-printed).
    Raw(String),
    Func(FnGraph),
    /// `impl ... { ... }` block. `header` is everything before the `{`.
    Impl { header: String, items: Vec<Item> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FnGraph {
    /// Display name, e.g. `main` or `Counter::step`.
    pub name: String,
    /// Full signature including attributes / doc comments, without the body,
    /// e.g. `pub fn add(a: i32, b: i32) -> i32`.
    pub sig: String,
    /// Parameter names, used as output pins on the Entry node.
    pub params: Vec<String>,
    pub nodes: BTreeMap<NodeId, Node>,
    pub next_id: NodeId,
    pub exec: Vec<ExecEdge>,
    pub data: Vec<DataEdge>,
    pub entry: NodeId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    pub pos: [f32; 2],
}

/// Exec wire: from an exec output pin to a node's (single) exec input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecEdge {
    pub from: NodeId,
    pub from_pin: usize,
    pub to: NodeId,
}

/// Data wire: from a data output pin to a data input pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataEdge {
    pub from: NodeId,
    pub from_pin: usize,
    pub to: NodeId,
    pub to_pin: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    // ---- exec ("impure") nodes: they have exec pins and run in order ----
    /// Function start. Data outputs = parameters.
    Entry,
    /// `let [mut] name[: ty] = value;`
    Let { name: String, mutable: bool, ty: String },
    /// `target op value;` where op is `=`, `+=`, `-=`, ...
    Assign { op: String },
    /// Expression statement: `value;`
    Eval,
    If,
    While,
    Loop,
    /// `for var in iter { body }`
    For { var: String },
    Break,
    Continue,
    /// `return value;` (the last return of a function is written as a tail expression)
    Return,
    /// Escape hatch: any statement as raw Rust.
    RawStmt { code: String },

    // ---- pure nodes: only data pins ----
    Literal { code: String },
    /// A name that isn't wired to a `let`/parameter (constants, paths, raw-code locals...).
    Var { name: String },
    Binary { op: String },
    /// `!x`, `-x`, `*x`, `&x`, `&mut x`
    Unary { op: String },
    /// `path(args...)`
    Call { path: String, args: usize },
    /// `receiver.name(args...)`
    Method { name: String, args: usize },
    /// `name!(args...)`; `bracket` = use `[...]` (e.g. `vec![...]`)
    Macro { name: String, args: usize, bracket: bool },
    /// `value.name`
    Field { name: String },
    /// `value[index]`
    Index,
    /// Escape hatch: any expression as raw Rust.
    RawExpr { code: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Entry,
    Flow,
    Statement,
    Value,
    Op,
    Call,
    Raw,
}

impl NodeKind {
    pub fn is_exec(&self) -> bool {
        use NodeKind::*;
        matches!(
            self,
            Entry | Let { .. } | Assign { .. } | Eval | If | While | Loop | For { .. } | Break | Continue | Return | RawStmt { .. }
        )
    }

    pub fn has_exec_in(&self) -> bool {
        self.is_exec() && !matches!(self, NodeKind::Entry)
    }

    /// Names of the exec output pins. The pin called "next" (or the only pin) continues the chain.
    pub fn exec_outs(&self) -> &'static [&'static str] {
        use NodeKind::*;
        match self {
            Entry | Let { .. } | Assign { .. } | Eval | RawStmt { .. } => &[""],
            If => &["then", "else", "next"],
            While | Loop | For { .. } => &["body", "next"],
            Break | Continue | Return => &[],
            _ => &[],
        }
    }

    /// Index of the exec pin that continues straight on after this node, if any.
    pub fn next_pin(&self) -> Option<usize> {
        let outs = self.exec_outs();
        match outs.len() {
            0 => None,
            1 => Some(0),
            n => Some(n - 1),
        }
    }

    pub fn data_ins(&self) -> Vec<String> {
        use NodeKind::*;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        match self {
            Let { .. } | Eval | Return => s(&["value"]),
            Assign { .. } => s(&["target", "value"]),
            If | While => s(&["cond"]),
            For { .. } => s(&["iter"]),
            Binary { .. } => s(&["a", "b"]),
            Unary { .. } | Field { .. } => s(&["value"]),
            Index => s(&["value", "index"]),
            Call { args, .. } => (0..*args).map(|i| format!("arg{i}")).collect(),
            Method { args, .. } => {
                let mut v = vec!["self".to_string()];
                v.extend((0..*args).map(|i| format!("arg{i}")));
                v
            }
            Macro { args, .. } => (0..*args).map(|i| format!("arg{i}")).collect(),
            _ => vec![],
        }
    }

    pub fn data_outs(&self, params: &[String]) -> Vec<String> {
        use NodeKind::*;
        match self {
            Entry => params.to_vec(),
            Let { name, .. } => vec![name.clone()],
            For { var } => vec![var.clone()],
            Literal { .. } | Var { .. } | Binary { .. } | Unary { .. } | Call { .. } | Method { .. } | Macro { .. }
            | Field { .. } | Index | RawExpr { .. } => vec![String::new()],
            _ => vec![],
        }
    }

    pub fn title(&self) -> String {
        use NodeKind::*;
        match self {
            Entry => "Entry".into(),
            Let { name, mutable, ty } => {
                let m = if *mutable { "mut " } else { "" };
                if ty.is_empty() { format!("let {m}{name}") } else { format!("let {m}{name}: {ty}") }
            }
            Assign { op } => format!("Assign {op}"),
            Eval => "Do".into(),
            If => "If".into(),
            While => "While".into(),
            Loop => "Loop".into(),
            For { var } => format!("For {var} in"),
            Break => "Break".into(),
            Continue => "Continue".into(),
            Return => "Return".into(),
            RawStmt { .. } => "Rust statement".into(),
            Literal { code } => code.clone(),
            Var { name } => name.clone(),
            Binary { op } => op.clone(),
            Unary { op } => op.trim().to_string(),
            Call { path, .. } => format!("{path}()"),
            Method { name, .. } => format!(".{name}()"),
            Macro { name, .. } => format!("{name}!"),
            Field { name } => format!(".{name}"),
            Index => "[ ]".into(),
            RawExpr { .. } => "Rust expr".into(),
        }
    }

    pub fn category(&self) -> Category {
        use NodeKind::*;
        match self {
            Entry => Category::Entry,
            If | While | Loop | For { .. } | Break | Continue | Return => Category::Flow,
            Let { .. } | Assign { .. } | Eval => Category::Statement,
            Literal { .. } | Var { .. } => Category::Value,
            Binary { .. } | Unary { .. } | Field { .. } | Index => Category::Op,
            Call { .. } | Method { .. } | Macro { .. } => Category::Call,
            RawStmt { .. } | RawExpr { .. } => Category::Raw,
        }
    }

    /// Raw code shown inside the node body (for escape-hatch nodes).
    pub fn body_text(&self) -> Option<&str> {
        match self {
            NodeKind::RawStmt { code } | NodeKind::RawExpr { code } => Some(code),
            _ => None,
        }
    }

    /// Templates offered in the "add node" menu.
    pub fn palette() -> Vec<(&'static str, NodeKind)> {
        use NodeKind::*;
        vec![
            ("let (variable)", Let { name: "x".into(), mutable: false, ty: String::new() }),
            ("let mut (variable)", Let { name: "x".into(), mutable: true, ty: String::new() }),
            ("Assign =", Assign { op: "=".into() }),
            ("Assign +=", Assign { op: "+=".into() }),
            ("Do (expression statement)", Eval),
            ("If", If),
            ("While", While),
            ("Loop", Loop),
            ("For in", For { var: "i".into() }),
            ("Break", Break),
            ("Continue", Continue),
            ("Return", Return),
            ("Literal", Literal { code: "0".into() }),
            ("String literal", Literal { code: "\"text\"".into() }),
            ("Variable / path", Var { name: "x".into() }),
            ("Add +", Binary { op: "+".into() }),
            ("Subtract -", Binary { op: "-".into() }),
            ("Multiply *", Binary { op: "*".into() }),
            ("Divide /", Binary { op: "/".into() }),
            ("Remainder %", Binary { op: "%".into() }),
            ("Equal ==", Binary { op: "==".into() }),
            ("Not equal !=", Binary { op: "!=".into() }),
            ("Less <", Binary { op: "<".into() }),
            ("Less or equal <=", Binary { op: "<=".into() }),
            ("Greater >", Binary { op: ">".into() }),
            ("Greater or equal >=", Binary { op: ">=".into() }),
            ("And &&", Binary { op: "&&".into() }),
            ("Or ||", Binary { op: "||".into() }),
            ("Range ..", RawExpr { code: "0..10".into() }),
            ("Not !", Unary { op: "!".into() }),
            ("Negate -", Unary { op: "-".into() }),
            ("Borrow &", Unary { op: "&".into() }),
            ("Borrow mut &mut", Unary { op: "&mut ".into() }),
            ("Deref *", Unary { op: "*".into() }),
            ("Call function", Call { path: "foo".into(), args: 1 }),
            ("Call method", Method { name: "len".into(), args: 0 }),
            ("println!", Macro { name: "println".into(), args: 2, bracket: false }),
            ("format!", Macro { name: "format".into(), args: 2, bracket: false }),
            ("vec!", Macro { name: "vec".into(), args: 3, bracket: true }),
            ("Field .name", Field { name: "field".into() }),
            ("Index [i]", Index),
            ("Rust expression (raw)", RawExpr { code: "todo!()".into() }),
            ("Rust statement (raw)", RawStmt { code: "// any Rust here".into() }),
        ]
    }
}

impl FnGraph {
    pub fn new(name: String, sig: String, params: Vec<String>) -> Self {
        let mut g = FnGraph {
            name,
            sig,
            params,
            nodes: BTreeMap::new(),
            next_id: 0,
            exec: vec![],
            data: vec![],
            entry: 0,
        };
        g.entry = g.add(NodeKind::Entry, [0.0, 0.0]);
        g
    }

    pub fn add(&mut self, kind: NodeKind, pos: [f32; 2]) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.insert(id, Node { kind, pos });
        id
    }

    pub fn remove(&mut self, id: NodeId) {
        if id == self.entry {
            return;
        }
        self.nodes.remove(&id);
        self.exec.retain(|e| e.from != id && e.to != id);
        self.data.retain(|e| e.from != id && e.to != id);
    }

    /// Connect an exec output. An exec output has at most one wire, so any old wire is replaced.
    pub fn connect_exec(&mut self, from: NodeId, from_pin: usize, to: NodeId) {
        if from == to {
            return;
        }
        self.exec.retain(|e| !(e.from == from && e.from_pin == from_pin));
        self.exec.push(ExecEdge { from, from_pin, to });
    }

    /// Connect a data input. A data input has at most one wire, so any old wire is replaced.
    pub fn connect_data(&mut self, from: NodeId, from_pin: usize, to: NodeId, to_pin: usize) {
        if from == to {
            return;
        }
        self.data.retain(|e| !(e.to == to && e.to_pin == to_pin));
        self.data.push(DataEdge { from, from_pin, to, to_pin });
    }

    pub fn exec_target(&self, from: NodeId, pin: usize) -> Option<NodeId> {
        self.exec.iter().find(|e| e.from == from && e.from_pin == pin).map(|e| e.to)
    }

    pub fn data_source(&self, to: NodeId, pin: usize) -> Option<(NodeId, usize)> {
        self.data.iter().find(|e| e.to == to && e.to_pin == pin).map(|e| (e.from, e.from_pin))
    }

    /// After a node's pin count changes (e.g. args edited), drop wires to pins that no longer exist.
    pub fn prune_edges(&mut self) {
        let params = self.params.clone();
        let nodes = &self.nodes;
        self.data.retain(|e| match (nodes.get(&e.from), nodes.get(&e.to)) {
            (Some(f), Some(t)) => e.from_pin < f.kind.data_outs(&params).len() && e.to_pin < t.kind.data_ins().len(),
            _ => false,
        });
        self.exec.retain(|e| match (nodes.get(&e.from), nodes.get(&e.to)) {
            (Some(f), Some(t)) => e.from_pin < f.kind.exec_outs().len() && t.kind.has_exec_in(),
            _ => false,
        });
    }
}

impl Program {
    /// Every function graph with a path of item indices to reach it.
    pub fn functions(&self) -> Vec<(Vec<usize>, &FnGraph)> {
        fn walk<'a>(items: &'a [Item], prefix: &mut Vec<usize>, out: &mut Vec<(Vec<usize>, &'a FnGraph)>) {
            for (i, it) in items.iter().enumerate() {
                prefix.push(i);
                match it {
                    Item::Func(f) => out.push((prefix.clone(), f)),
                    Item::Impl { items, .. } => walk(items, prefix, out),
                    Item::Raw(_) => {}
                }
                prefix.pop();
            }
        }
        let mut out = vec![];
        walk(&self.items, &mut vec![], &mut out);
        out
    }

    pub fn function_mut(&mut self, path: &[usize]) -> Option<&mut FnGraph> {
        let mut items = &mut self.items;
        for (depth, &i) in path.iter().enumerate() {
            let last = depth + 1 == path.len();
            match items.get_mut(i)? {
                Item::Func(f) if last => return Some(f),
                Item::Impl { items: inner, .. } if !last => items = inner,
                _ => return None,
            }
        }
        None
    }
}
