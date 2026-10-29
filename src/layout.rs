//! Node geometry (shared by the editor) and automatic layout for graphs that
//! came from plain text and have no saved positions.

use crate::graph::*;
use std::collections::{HashMap, HashSet};

pub const HEADER: f32 = 26.0;
pub const ROW: f32 = 22.0;
pub const LINE: f32 = 15.0;
const COL_GAP: f32 = 40.0;
const ROW_GAP: f32 = 40.0;

pub fn node_width(kind: &NodeKind) -> f32 {
    let base: f32 = if kind.is_exec() { 190.0 } else { 150.0 };
    let title = kind.title().chars().count() as f32 * 7.5 + 40.0;
    let body = kind.body_text().map(|t| t.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * 7.2 + 24.0).unwrap_or(0.0);
    base.max(title).max(body).min(520.0)
}

pub fn left_rows(kind: &NodeKind) -> usize {
    kind.has_exec_in() as usize + kind.data_ins().len()
}

pub fn right_rows(kind: &NodeKind, params: &[String]) -> usize {
    kind.exec_outs().len() + kind.data_outs(params).len()
}

pub fn node_size(kind: &NodeKind, params: &[String]) -> [f32; 2] {
    let rows = left_rows(kind).max(right_rows(kind, params)).max(1);
    let body = kind.body_text().map(|t| t.lines().count().min(12) as f32 * LINE + 8.0).unwrap_or(0.0);
    [node_width(kind), HEADER + rows as f32 * ROW + 6.0 + body]
}

fn row_y(pos: [f32; 2], row: usize) -> f32 {
    pos[1] + HEADER + ROW * 0.5 + row as f32 * ROW
}

pub fn exec_in_pos(n: &Node) -> [f32; 2] {
    [n.pos[0], row_y(n.pos, 0)]
}
pub fn data_in_pos(n: &Node, i: usize) -> [f32; 2] {
    [n.pos[0], row_y(n.pos, n.kind.has_exec_in() as usize + i)]
}
pub fn exec_out_pos(n: &Node, i: usize, params: &[String]) -> [f32; 2] {
    [n.pos[0] + node_size(&n.kind, params)[0], row_y(n.pos, i)]
}
pub fn data_out_pos(n: &Node, i: usize, params: &[String]) -> [f32; 2] {
    [n.pos[0] + node_size(&n.kind, params)[0], row_y(n.pos, n.kind.exec_outs().len() + i)]
}

// ---------------------------------------------------------------------------

/// Lay out a whole function: exec chains run left→right, branches stack below,
/// and each statement's expression tree sits to its left.
pub fn auto_layout(g: &mut FnGraph) {
    let mut placed = HashSet::new();
    let entry = g.entry;
    set_pos(g, entry, [0.0, 0.0]);
    placed.insert(entry);
    let w = node_size(&g.nodes[&entry].kind, &g.params)[0];
    let start = g.exec_target(entry, 0);
    let (_, h) = place_chain(g, start, w + COL_GAP, 0.0, &mut placed);

    // Anything not reachable from Entry goes in a row underneath.
    let mut x = 0.0;
    let y = h.max(120.0) + 120.0;
    let rest: Vec<NodeId> = g.nodes.keys().copied().filter(|id| !placed.contains(id)).collect();
    for id in rest {
        if placed.contains(&id) {
            continue;
        }
        if g.nodes[&id].kind.is_exec() {
            let (xe, _) = place_chain(g, Some(id), x, y, &mut placed);
            x = xe + COL_GAP;
        } else {
            set_pos(g, id, [x, y]);
            placed.insert(id);
            x += node_size(&g.nodes[&id].kind, &g.params)[0] + COL_GAP;
        }
    }
}

fn set_pos(g: &mut FnGraph, id: NodeId, p: [f32; 2]) {
    if let Some(n) = g.nodes.get_mut(&id) {
        n.pos = p;
    }
}

/// Returns (x after the chain, height used). Long chains wrap onto a new row.
fn place_chain(g: &mut FnGraph, start: Option<NodeId>, x0: f32, y0: f32, placed: &mut HashSet<NodeId>) -> (f32, f32) {
    const WRAP: f32 = 1500.0;
    let mut cur = start;
    let mut x = x0;
    let mut y = y0;
    let mut row_h: f32 = 0.0;
    let mut max_x = x0;
    while let Some(id) = cur {
        if placed.contains(&id) {
            break;
        }
        if x > x0 + WRAP {
            y += row_h + ROW_GAP * 2.0;
            x = x0;
            row_h = 0.0;
        }
        placed.insert(id);
        let kind = g.nodes[&id].kind.clone();
        // expression trees feeding this node, laid out in columns to its left
        let cols = tree_columns(g, id, placed, 0);
        let node_x = x + cols as f32 * 170.0;
        // start value nodes below the exec wire so they don't hide it
        let mut cursor = y + 52.0;
        for pin in 0..kind.data_ins().len() {
            place_tree(g, id, pin, node_x, &mut cursor, placed);
        }
        set_pos(g, id, [node_x, y]);
        let [w, h] = node_size(&kind, &g.params);
        let mut chain_h = h.max(cursor - y);
        let after = node_x + w + COL_GAP;
        let mut x_end = after;
        let outs = kind.exec_outs().len();
        if outs > 1 {
            // branch bodies stack underneath the node
            let mut by = y + chain_h + ROW_GAP;
            for pin in 0..outs {
                if Some(pin) == kind.next_pin() {
                    continue;
                }
                let Some(t) = g.exec_target(id, pin) else { continue };
                let (xe, bh) = place_chain(g, Some(t), node_x + 60.0, by, placed);
                x_end = x_end.max(xe);
                by += bh + ROW_GAP;
            }
            chain_h = chain_h.max(by - y - ROW_GAP);
            // keep the rest of the chain from running under wide branches
            x_end = x_end.min(after + 200.0).max(after);
        }
        row_h = row_h.max(chain_h);
        x = x_end;
        max_x = max_x.max(x);
        cur = kind.next_pin().and_then(|p| g.exec_target(id, p));
    }
    (max_x, y - y0 + row_h)
}

/// How many columns of pure nodes feed into `id` (not counting already placed ones).
fn tree_columns(g: &FnGraph, id: NodeId, placed: &HashSet<NodeId>, depth: usize) -> usize {
    if depth > 64 {
        return 0;
    }
    let n = g.nodes[&id].kind.data_ins().len();
    let mut best = 0;
    for pin in 0..n {
        if let Some((src, _)) = g.data_source(id, pin) {
            let k = &g.nodes[&src].kind;
            if !k.is_exec() && !placed.contains(&src) {
                best = best.max(1 + tree_columns(g, src, placed, depth + 1));
            }
        }
    }
    best
}

fn place_tree(g: &mut FnGraph, consumer: NodeId, pin: usize, right_x: f32, cursor: &mut f32, placed: &mut HashSet<NodeId>) {
    let Some((src, _)) = g.data_source(consumer, pin) else { return };
    if placed.contains(&src) || g.nodes[&src].kind.is_exec() {
        return;
    }
    placed.insert(src);
    let kind = g.nodes[&src].kind.clone();
    let [w, h] = node_size(&kind, &g.params);
    let x = right_x - w - 20.0;
    let y0 = *cursor;
    let mut child_cursor = y0;
    for p in 0..kind.data_ins().len() {
        place_tree(g, src, p, right_x - 170.0, &mut child_cursor, placed);
    }
    set_pos(g, src, [x, y0]);
    *cursor = (y0 + h + 12.0).max(child_cursor);
}

// ---------------------------------------------------------------------------

/// A structural "address" for each node (e.g. `E/0/2.1`) that stays the same
/// when a graph is saved to Rust and parsed back. Used to carry node positions
/// over when the .rs file was edited outside the editor.
pub fn node_paths(g: &FnGraph) -> HashMap<NodeId, String> {
    let mut out = HashMap::new();
    fn exec(g: &FnGraph, id: NodeId, path: String, out: &mut HashMap<NodeId, String>) {
        if out.contains_key(&id) {
            return;
        }
        out.insert(id, path.clone());
        let kind = &g.nodes[&id].kind;
        for pin in 0..kind.data_ins().len() {
            data(g, id, pin, &path, out);
        }
        for pin in 0..kind.exec_outs().len() {
            if let Some(t) = g.exec_target(id, pin) {
                exec(g, t, format!("{path}/{pin}"), out);
            }
        }
    }
    fn data(g: &FnGraph, id: NodeId, pin: usize, path: &str, out: &mut HashMap<NodeId, String>) {
        if let Some((src, _)) = g.data_source(id, pin) {
            if !g.nodes[&src].kind.is_exec() && !out.contains_key(&src) {
                let p = format!("{path}.{pin}");
                out.insert(src, p.clone());
                for i in 0..g.nodes[&src].kind.data_ins().len() {
                    data(g, src, i, &p, out);
                }
            }
        }
    }
    exec(g, g.entry, "E".into(), &mut out);
    out
}
