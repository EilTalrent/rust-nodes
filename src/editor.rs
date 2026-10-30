//! The node canvas: pan/zoom, drawing nodes and wires, dragging wires between pins,
//! the right-click "add node" menu, and the inspector for the selected node.

use crate::graph::*;
use crate::layout::*;
use std::collections::{BTreeSet, HashMap};
use eframe::egui::{
    self, epaint::CubicBezierShape, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Vec2,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pin {
    ExecIn(NodeId),
    ExecOut(NodeId, usize),
    DataIn(NodeId, usize),
    DataOut(NodeId, usize),
}

impl Pin {
    fn node(self) -> NodeId {
        match self {
            Pin::ExecIn(n) | Pin::ExecOut(n, _) | Pin::DataIn(n, _) | Pin::DataOut(n, _) => n,
        }
    }
    fn compatible(self, other: Pin) -> bool {
        self.node() != other.node()
            && matches!(
                (self, other),
                (Pin::ExecOut(..), Pin::ExecIn(..))
                    | (Pin::ExecIn(..), Pin::ExecOut(..))
                    | (Pin::DataOut(..), Pin::DataIn(..))
                    | (Pin::DataIn(..), Pin::DataOut(..))
            )
    }
}

struct AddMenu {
    screen_pos: Pos2,
    world_pos: [f32; 2],
    search: String,
    /// When the menu was opened by dropping a wire on empty space, connect the new node to this pin.
    pending: Option<Pin>,
    just_opened: bool,
}

pub struct Editor {
    pub pan: Vec2,
    pub zoom: f32,
    /// The node shown in the inspector.
    pub selected: Option<NodeId>,
    /// Everything selected (includes `selected`).
    pub multi: BTreeSet<NodeId>,
    dragging_wire: Option<Pin>,
    add_menu: Option<AddMenu>,
    /// Box selection in progress: start point (screen).
    box_start: Option<Pos2>,
    pub needs_fit: bool,
    /// Containers / groups we've opened, outermost first. Empty = function top level.
    pub view: Vec<(NodeId, usize)>,
    /// Set when the user asks to turn a group into a function; the app shows the dialog.
    pub extract_request: Option<NodeId>,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            pan: Vec2::new(40.0, 60.0),
            zoom: 1.0,
            selected: None,
            multi: BTreeSet::new(),
            dragging_wire: None,
            add_menu: None,
            box_start: None,
            needs_fit: true,
            view: vec![],
            extract_request: None,
        }
    }
}

const EXEC_COLOR: Color32 = Color32::from_rgb(235, 235, 235);
const DATA_COLOR: Color32 = Color32::from_rgb(90, 200, 160);
const SELECT_COLOR: Color32 = Color32::from_rgb(255, 200, 60);

pub fn header_color(c: Category) -> Color32 {
    match c {
        Category::Entry => Color32::from_rgb(170, 40, 40),
        Category::Flow => Color32::from_rgb(110, 110, 120),
        Category::Statement => Color32::from_rgb(40, 90, 170),
        Category::Value => Color32::from_rgb(150, 110, 30),
        Category::Op => Color32::from_rgb(40, 130, 90),
        Category::Call => Color32::from_rgb(120, 60, 150),
        Category::Raw => Color32::from_rgb(70, 70, 70),
        Category::Template => Color32::from_rgb(30, 110, 120),
        Category::Group => Color32::from_rgb(150, 70, 120),
    }
}

/// A wire, by index into `f.exec` or `f.data`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EdgeRef {
    Exec(usize),
    Data(usize),
}

/// Wires that cross a group's border, shown as pins on the collapsed group node.
#[derive(Default)]
struct GroupPins {
    ins: Vec<EdgeRef>,
    outs: Vec<EdgeRef>,
}

const GROUP_W: f32 = 210.0;

fn group_size(gp: &GroupPins) -> [f32; 2] {
    let rows = gp.ins.len().max(gp.outs.len()).max(1);
    [GROUP_W, HEADER + rows as f32 * ROW + ROW + 8.0]
}

impl Editor {
    fn select_only(&mut self, id: NodeId) {
        self.selected = Some(id);
        self.multi = BTreeSet::from([id]);
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.multi.clear();
    }

    fn go_to_view(&mut self, v: Vec<(NodeId, usize)>) {
        self.view = v;
        self.clear_selection();
        self.add_menu = None;
        self.needs_fit = true;
    }

    /// Select a node and open whatever containers / groups it's inside, so it's visible.
    pub fn reveal(&mut self, f: &FnGraph, id: NodeId) {
        let mut chain = vec![];
        let mut s = f.nodes.get(&id).and_then(|n| n.scope);
        while let Some((owner, hole)) = s {
            chain.push((owner, hole));
            s = f.nodes.get(&owner).and_then(|n| n.scope);
            if chain.len() > 64 {
                break;
            }
        }
        chain.reverse();
        if chain != self.view {
            self.go_to_view(chain);
        }
        self.select_only(id);
    }

    /// Group the selected nodes. Returns true if a group was made.
    pub fn collapse_selection(&mut self, f: &mut FnGraph) -> bool {
        let ids: Vec<NodeId> = self.multi.iter().copied().collect();
        let n = f.nodes.values().filter(|n| matches!(n.kind, NodeKind::Group { .. })).count() + 1;
        if let Some(g) = f.collapse(&ids, &format!("Group {n}")) {
            self.select_only(g);
            true
        } else {
            false
        }
    }

    /// Draw the canvas for one function. Returns true if the graph changed.
    pub fn show(&mut self, ui: &mut egui::Ui, f: &mut FnGraph, salt: &str) -> bool {
        let mut changed = false;
        // ---- breadcrumb: where we are inside nested containers / groups ----
        self.view.retain(|(n, _)| f.nodes.contains_key(n));
        self.multi.retain(|n| f.nodes.contains_key(n));
        if !self.view.is_empty() {
            let mut go_to: Option<usize> = None;
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                if ui.button(format!("⬅ {}", f.name)).on_hover_text("Back to the function (Esc goes up one level)").clicked() {
                    go_to = Some(0);
                }
                for (depth, (n, hole)) in self.view.iter().enumerate() {
                    ui.label("›");
                    let k = &f.nodes[n].kind;
                    let label = match k {
                        NodeKind::Group { name } => format!("▣ {name}"),
                        _ => format!("{} {}", container_label(k), hole_label(*hole)),
                    };
                    let last = depth + 1 == self.view.len();
                    if ui.add_enabled(!last, egui::Button::new(label)).clicked() {
                        go_to = Some(depth + 1);
                    }
                }
            });
            if let Some(d) = go_to {
                let v = self.view[..d].to_vec();
                self.go_to_view(v);
            }
        }
        let scope: Scope = self.view.last().copied();
        let mut enter: Option<(NodeId, usize)> = None;

        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        let base_id = ui.scope_id().with(("rust-nodes-canvas", salt));
        let mods = ui.input(|i| i.modifiers);
        let additive = mods.shift || mods.command || mods.ctrl;

        if self.needs_fit {
            self.fit(f, rect, scope);
            self.needs_fit = false;
        }

        // ---- pan, zoom, box select ----
        if resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Secondary) {
            self.pan += resp.drag_delta() / self.zoom;
        }
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.box_start = resp.interact_pointer_pos();
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta().y);
            let pinch = ui.input(|i| i.zoom_delta());
            let factor = (scroll * 0.0015).exp() * pinch;
            if (factor - 1.0).abs() > 1e-4 {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                    let before = self.to_world(rect, p);
                    self.zoom = (self.zoom * factor).clamp(0.2, 3.0);
                    let after = self.to_world(rect, p);
                    self.pan += Vec2::new(after[0] - before[0], after[1] - before[1]);
                }
            }
        }
        if resp.clicked() {
            if !additive {
                self.clear_selection();
            }
            self.add_menu = None;
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.open_menu(rect, p, None);
            }
        }

        // ---- background grid ----
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgb(28, 28, 32));
        let step = 32.0 * self.zoom;
        if step > 6.0 {
            let origin = self.to_screen(rect, [0.0, 0.0]);
            let grid = Stroke::new(1.0, Color32::from_rgb(38, 38, 44));
            let mut x = rect.left() + (origin.x - rect.left()).rem_euclid(step);
            while x < rect.right() {
                painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], grid);
                x += step;
            }
            let mut y = rect.top() + (origin.y - rect.top()).rem_euclid(step);
            while y < rect.bottom() {
                painter.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], grid);
                y += step;
            }
        }
        let painter = painter.with_clip_rect(rect);
        let z = self.zoom;

        // ---- what's visible, and which wires cross into collapsed groups ----
        let ids: Vec<NodeId> = f.nodes.iter().filter(|(_, n)| n.scope == scope).map(|(id, _)| *id).collect();
        let rep = |id: NodeId| f.rep(id, scope);
        let mut groups: HashMap<NodeId, GroupPins> = HashMap::new();
        for id in &ids {
            if f.is_group(*id) {
                groups.insert(*id, GroupPins::default());
            }
        }
        let edges: Vec<(EdgeRef, NodeId, NodeId)> = f
            .exec
            .iter()
            .enumerate()
            .map(|(i, e)| (EdgeRef::Exec(i), e.from, e.to))
            .chain(f.data.iter().enumerate().map(|(i, e)| (EdgeRef::Data(i), e.from, e.to)))
            .collect();
        for (er, from, to) in &edges {
            let (a, b) = (rep(*from), rep(*to));
            if a == b {
                continue;
            }
            if let Some(g) = b.filter(|g| groups.contains_key(g) && *g != *to) {
                groups.get_mut(&g).unwrap().ins.push(*er);
            }
            if let Some(g) = a.filter(|g| groups.contains_key(g) && *g != *from) {
                groups.get_mut(&g).unwrap().outs.push(*er);
            }
        }
        let group_pin = |g: NodeId, er: EdgeRef, out: bool| -> Option<[f32; 2]> {
            let gp = groups.get(&g)?;
            let list = if out { &gp.outs } else { &gp.ins };
            let i = list.iter().position(|x| *x == er)?;
            let n = &f.nodes[&g];
            let x = if out { n.pos[0] + GROUP_W } else { n.pos[0] };
            Some([x, n.pos[1] + HEADER + ROW * 0.5 + i as f32 * ROW])
        };
        let size_of = |id: NodeId| -> [f32; 2] {
            match groups.get(&id) {
                Some(gp) => group_size(gp),
                None => node_size(&f.nodes[&id].kind, &f.params),
            }
        };

        // ---- wires ----
        for (er, from, to) in &edges {
            let (Some(a), Some(b)) = (rep(*from), rep(*to)) else { continue };
            if a == b {
                continue;
            }
            let (p0, p1, color, width) = match *er {
                EdgeRef::Data(i) => {
                    let e = f.data[i];
                    let p0 = if a == e.from { Some(data_out_pos(&f.nodes[&a], e.from_pin, &f.params)) } else { group_pin(a, *er, true) };
                    let p1 = if b == e.to { Some(data_in_pos(&f.nodes[&b], e.to_pin)) } else { group_pin(b, *er, false) };
                    (p0, p1, DATA_COLOR, 2.0)
                }
                EdgeRef::Exec(i) => {
                    let e = f.exec[i];
                    let p0 = if a == e.from { Some(exec_out_pos(&f.nodes[&a], e.from_pin, &f.params)) } else { group_pin(a, *er, true) };
                    let p1 = if b == e.to { Some(exec_in_pos(&f.nodes[&b])) } else { group_pin(b, *er, false) };
                    (p0, p1, EXEC_COLOR, 3.0)
                }
            };
            if let (Some(p0), Some(p1)) = (p0, p1) {
                self.wire(&painter, self.to_screen(rect, p0), self.to_screen(rect, p1), color, width);
            }
        }

        // ---- nodes ----
        let mut to_delete: Vec<NodeId> = vec![];
        let mut to_duplicate = None;
        let mut to_ungroup = None;
        let mut collapse = false;
        let mut pins: Vec<(Pin, Pos2)> = vec![];
        // labels for wires to/from outside this view: (pin position, text, on the left side?)
        let mut pills: Vec<(Pos2, String, bool)> = vec![];
        let mut node_rects: Vec<(NodeId, Rect)> = vec![];
        let mut drag_delta = Vec2::ZERO;
        for &id in &ids {
            let node = &f.nodes[&id];
            let [w, h] = size_of(id);
            let r = Rect::from_min_size(self.to_screen(rect, node.pos), Vec2::new(w, h) * z);
            node_rects.push((id, r));
            let nresp = ui.interact(r, base_id.with(("node", id)), Sense::click_and_drag());
            if nresp.drag_started_by(egui::PointerButton::Primary) && !self.multi.contains(&id) {
                if additive {
                    self.multi.insert(id);
                    self.selected = Some(id);
                } else {
                    self.select_only(id);
                }
            }
            if nresp.dragged_by(egui::PointerButton::Primary) {
                drag_delta = nresp.drag_delta() / z; // moves every selected node
            }
            if nresp.clicked() {
                if additive {
                    if !self.multi.remove(&id) {
                        self.multi.insert(id);
                    }
                    self.selected = Some(id);
                } else {
                    self.select_only(id);
                }
            }
            if nresp.double_clicked() {
                if f.is_group(id) {
                    enter = Some((id, 0));
                } else if let Some(&hole) = f.nodes[&id].kind.block_holes().first() {
                    enter = Some((id, hole));
                }
            }
            let fixed = f.is_fixed(id);
            let is_group = f.is_group(id);
            let sel_count = if self.multi.contains(&id) { self.multi.len() } else { 1 };
            nresp.context_menu(|ui| {
                if is_group {
                    if ui.button("Open group").clicked() {
                        enter = Some((id, 0));
                        ui.close();
                    }
                    if ui.button("ƒ Extract function…").on_hover_text("Turn this group into a real Rust function").clicked() {
                        self.extract_request = Some(id);
                        ui.close();
                    }
                    if ui.button("Ungroup").clicked() {
                        to_ungroup = Some(id);
                        ui.close();
                    }
                }
                if !fixed {
                    let label = if sel_count > 1 { format!("Collapse {sel_count} nodes into a group") } else { "Collapse into a group".into() };
                    if ui.button(label).on_hover_text("Ctrl+G").clicked() {
                        if !self.multi.contains(&id) {
                            self.select_only(id);
                        }
                        collapse = true;
                        ui.close();
                    }
                    let del = if is_group { "Delete group and its nodes".to_string() } else if sel_count > 1 { format!("Delete {sel_count} nodes") } else { "Delete".into() };
                    if ui.button(del).clicked() {
                        if self.multi.contains(&id) {
                            to_delete.extend(self.multi.iter().copied());
                        } else {
                            to_delete.push(id);
                        }
                        ui.close();
                    }
                    if !is_group && ui.button("Duplicate").clicked() {
                        to_duplicate = Some(id);
                        ui.close();
                    }
                }
            });

            let node = &f.nodes[&id];
            let kind = &node.kind;
            let sel = self.multi.contains(&id);
            let cr = CornerRadius::same((6.0 * z).round().clamp(0.0, 255.0) as u8);
            if is_group {
                // stacked-card look for collapsed groups
                for k in [2.0, 1.0] {
                    let back = r.translate(Vec2::new(4.0, 4.0) * k * z);
                    painter.rect_filled(back, cr, Color32::from_rgb(40, 34, 44));
                    painter.rect_stroke(back, cr, Stroke::new(1.0, Color32::from_rgb(90, 60, 85)), StrokeKind::Outside);
                }
            } else {
                painter.rect_filled(r.translate(Vec2::splat(3.0 * z)), cr, Color32::from_black_alpha(90));
            }
            painter.rect_filled(r, cr, Color32::from_rgb(48, 48, 54));
            let header = Rect::from_min_size(r.min, Vec2::new(r.width(), HEADER * z));
            painter.rect_filled(header, CornerRadius { nw: cr.nw, ne: cr.ne, sw: 0, se: 0 }, header_color(kind.category()));
            let border = if sel { Stroke::new(2.0, SELECT_COLOR) } else { Stroke::new(1.0, Color32::from_gray(20)) };
            painter.rect_stroke(r, cr, border, StrokeKind::Outside);
            let title = if is_group { format!("▣ {}", kind.title()) } else { kind.title() };
            painter.text(header.left_center() + Vec2::new(8.0 * z, 0.0), Align2::LEFT_CENTER, title, FontId::proportional(13.0 * z), Color32::WHITE);

            let label_font = FontId::proportional(11.5 * z);
            let label_color = Color32::from_gray(200);

            // the statement's `//` comments, shown above it
            let shown: Vec<String> = comment_display(&node.comment).lines().filter(|l| !l.is_empty()).map(String::from).collect();
            if !shown.is_empty() {
                let n = shown.len().min(3);
                for (i, l) in shown.iter().rev().take(3).enumerate() {
                    let text = if i == n - 1 && shown.len() > 3 { "…".to_string() } else { format!("// {}", short(l)) };
                    painter.text(
                        r.left_top() + Vec2::new(2.0 * z, -(4.0 + i as f32 * 13.0) * z),
                        Align2::LEFT_BOTTOM,
                        text,
                        FontId::proportional(11.0 * z),
                        Color32::from_rgb(130, 150, 130),
                    );
                }
            }

            if let Some(gp) = groups.get(&id) {
                // proxy pins for wires crossing the group's border (rewire inside the group)
                for er in gp.ins.iter() {
                    let p = self.to_screen(rect, group_pin(id, *er, false).unwrap());
                    let text = match *er {
                        EdgeRef::Exec(_) => "▶ in".to_string(),
                        EdgeRef::Data(di) => {
                            let e = f.data[di];
                            crate::codegen::expr_preview(f, e.from, e.from_pin)
                        }
                    };
                    match er {
                        EdgeRef::Exec(_) => self.exec_pin(&painter, p, true),
                        EdgeRef::Data(_) => self.data_pin(&painter, p, true),
                    }
                    painter.text(p + Vec2::new(10.0 * z, 0.0), Align2::LEFT_CENTER, text, label_font.clone(), label_color);
                }
                for er in gp.outs.iter() {
                    let p = self.to_screen(rect, group_pin(id, *er, true).unwrap());
                    let text = match *er {
                        EdgeRef::Exec(_) => "out ▶".to_string(),
                        EdgeRef::Data(di) => {
                            let e = f.data[di];
                            crate::codegen::expr_preview(f, e.from, e.from_pin)
                        }
                    };
                    match er {
                        EdgeRef::Exec(_) => self.exec_pin(&painter, p, true),
                        EdgeRef::Data(_) => self.data_pin(&painter, p, true),
                    }
                    painter.text(p - Vec2::new(10.0 * z, 0.0), Align2::RIGHT_CENTER, text, label_font.clone(), label_color);
                }
                // "open" row
                let top = r.bottom() - (ROW + 4.0) * z;
                let row = Rect::from_min_size(Pos2::new(r.left() + 4.0 * z, top), Vec2::new(r.width() - 8.0 * z, (ROW - 3.0) * z));
                let rr = ui.interact(row, base_id.with(("open", id, usize::MAX)), Sense::click());
                let fill = if rr.hovered() { Color32::from_rgb(105, 55, 95) } else { Color32::from_rgb(80, 45, 75) };
                painter.rect_filled(row, CornerRadius::same((4.0 * z) as u8), fill);
                let count = f.members(id).len();
                let label = format!("▶ open group · {count} node{}", if count == 1 { "" } else { "s" });
                painter.text(row.left_center() + Vec2::new(6.0 * z, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.5 * z), Color32::WHITE);
                if rr.clicked() {
                    enter = Some((id, 0));
                }
                continue;
            }

            // left pins
            if kind.has_exec_in() {
                let p = self.to_screen(rect, exec_in_pos(node));
                let incoming: Vec<NodeId> = f.exec.iter().filter(|e| e.to == id).map(|e| e.from).collect();
                self.exec_pin(&painter, p, !incoming.is_empty());
                if incoming.iter().any(|src| rep(*src).is_none()) {
                    pills.push((p, "from outside".into(), true));
                }
                pins.push((Pin::ExecIn(id), p));
            }
            for (i, name) in kind.data_ins().iter().enumerate() {
                let p = self.to_screen(rect, data_in_pos(node, i));
                let source = f.data_source(id, i);
                self.data_pin(&painter, p, source.is_some());
                painter.text(p + Vec2::new(10.0 * z, 0.0), Align2::LEFT_CENTER, name, label_font.clone(), label_color);
                // a value coming from outside this view: label it with where it comes from
                if let Some((src, sp)) = source {
                    if rep(src).is_none() {
                        pills.push((p, crate::codegen::expr_preview(f, src, sp), true));
                    }
                }
                pins.push((Pin::DataIn(id, i), p));
            }
            // right pins
            for (i, name) in kind.exec_outs().iter().enumerate() {
                let p = self.to_screen(rect, exec_out_pos(node, i, &f.params));
                let target = f.exec_target(id, i);
                self.exec_pin(&painter, p, target.is_some());
                painter.text(p - Vec2::new(10.0 * z, 0.0), Align2::RIGHT_CENTER, *name, label_font.clone(), label_color);
                if target.is_some_and(|t| rep(t).is_none()) {
                    pills.push((p, "continues outside".into(), false));
                }
                pins.push((Pin::ExecOut(id, i), p));
            }
            for (i, name) in kind.data_outs(&f.params).iter().enumerate() {
                let p = self.to_screen(rect, data_out_pos(node, i, &f.params));
                let users: Vec<NodeId> = f.data.iter().filter(|e| e.from == id && e.from_pin == i).map(|e| e.to).collect();
                self.data_pin(&painter, p, !users.is_empty());
                painter.text(p - Vec2::new(10.0 * z, 0.0), Align2::RIGHT_CENTER, name, label_font.clone(), label_color);
                if !users.is_empty() && users.iter().all(|u| rep(*u).is_none()) && !matches!(kind, NodeKind::Entry | NodeKind::BlockEntry { .. }) {
                    pills.push((p, "used outside".into(), false));
                }
                pins.push((Pin::DataOut(id, i), p));
            }
            // code body (raw nodes and templates)
            if let Some(code) = kind.body_text() {
                let mut y = r.top() + (body_top(kind, &f.params) + 2.0) * z;
                let lines: Vec<&str> = code.lines().collect();
                for (li, line) in lines.iter().take(MAX_BODY_LINES).enumerate() {
                    let line = if li + 1 == MAX_BODY_LINES && lines.len() > MAX_BODY_LINES { "…" } else { line };
                    painter.text(Pos2::new(r.left() + 8.0 * z, y), Align2::LEFT_TOP, line, FontId::monospace(11.5 * z), Color32::from_rgb(220, 220, 170));
                    y += LINE * z;
                }
            }
            // container rows: click to open
            for (k, hole) in kind.block_holes().into_iter().enumerate() {
                let top = r.top() + block_row_top(kind, &f.params, k) * z;
                let row = Rect::from_min_size(Pos2::new(r.left() + 4.0 * z, top), Vec2::new(r.width() - 8.0 * z, (ROW - 3.0) * z));
                let rr = ui.interact(row, base_id.with(("open", id, hole)), Sense::click());
                let fill = if rr.hovered() { Color32::from_rgb(40, 95, 105) } else { Color32::from_rgb(34, 70, 78) };
                painter.rect_filled(row, CornerRadius::same((4.0 * z) as u8), fill);
                let binders = &kind.holes()[hole].binders;
                let inner = f.nodes.values().filter(|n| n.scope == Some((id, hole))).count().saturating_sub(1);
                let mut label = format!("▶ open {}", hole_label(hole));
                if !binders.is_empty() {
                    label += &format!("  ({})", binders.join(", "));
                }
                label += &format!("  · {inner} node{}", if inner == 1 { "" } else { "s" });
                painter.text(row.left_center() + Vec2::new(6.0 * z, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.5 * z), Color32::WHITE);
                if rr.clicked() {
                    enter = Some((id, hole));
                }
            }
        }

        // move the whole selection together
        if drag_delta != Vec2::ZERO {
            for id in &self.multi {
                if let Some(n) = f.nodes.get_mut(id) {
                    n.pos[0] += drag_delta.x;
                    n.pos[1] += drag_delta.y;
                }
            }
            changed = true;
        }

        // labels for wires to/from outside this view (drawn on top)
        for (p, text, left) in pills {
            let galley = painter.layout_no_wrap(if left { format!("{text} ›") } else { format!("› {text}") }, FontId::monospace(11.0 * z), Color32::from_rgb(170, 240, 210));
            let size = galley.size() + Vec2::new(10.0, 4.0) * z;
            let pill = if left {
                Rect::from_min_size(p - Vec2::new(size.x + 8.0 * z, size.y * 0.5), size)
            } else {
                Rect::from_min_size(p + Vec2::new(8.0 * z, -size.y * 0.5), size)
            };
            painter.rect_filled(pill, CornerRadius::same((4.0 * z) as u8), Color32::from_rgb(28, 62, 52));
            painter.rect_stroke(pill, CornerRadius::same((4.0 * z) as u8), Stroke::new(1.0, DATA_COLOR), StrokeKind::Inside);
            let anchor = if left { pill.right_center() } else { pill.left_center() };
            painter.line_segment([anchor, p], Stroke::new(1.5, DATA_COLOR));
            painter.galley(pill.min + Vec2::new(5.0, 2.0) * z, galley, Color32::WHITE);
        }

        // ---- box selection ----
        if let Some(start) = self.box_start {
            if let Some(now) = ui.input(|i| i.pointer.interact_pos()) {
                let sel_rect = Rect::from_two_pos(start, now);
                painter.rect_filled(sel_rect, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(255, 200, 60, 25));
                painter.rect_stroke(sel_rect, CornerRadius::ZERO, Stroke::new(1.0, SELECT_COLOR), StrokeKind::Inside);
                if ui.input(|i| i.pointer.any_released()) {
                    if !additive {
                        self.clear_selection();
                    }
                    for (id, r) in &node_rects {
                        if sel_rect.intersects(*r) {
                            self.multi.insert(*id);
                        }
                    }
                    self.selected = self.multi.iter().next().copied();
                    self.box_start = None;
                }
            } else {
                self.box_start = None;
            }
        }

        // ---- pin interaction (after nodes, so pins win over node bodies) ----
        let pin_r = 9.0 * z;
        for (pin, p) in &pins {
            let presp = ui.interact(Rect::from_center_size(*p, Vec2::splat(pin_r * 2.0)), base_id.with(("pin", pin_key(pin))), Sense::drag());
            if presp.drag_started() {
                // dragging from a connected data input picks up its wire
                match *pin {
                    Pin::DataIn(n, i) => {
                        if let Some((src, sp)) = f.data_source(n, i) {
                            f.data.retain(|e| !(e.to == n && e.to_pin == i));
                            self.dragging_wire = Some(Pin::DataOut(src, sp));
                            changed = true;
                        } else {
                            self.dragging_wire = Some(*pin);
                        }
                    }
                    _ => self.dragging_wire = Some(*pin),
                }
            }
        }

        if let Some(from) = self.dragging_wire {
            let from_pos = pins.iter().find(|(p, _)| *p == from).map(|(_, pos)| *pos);
            let pointer = ui.input(|i| i.pointer.interact_pos());
            if let (Some(a), Some(b)) = (from_pos, pointer) {
                let color = if matches!(from, Pin::ExecIn(..) | Pin::ExecOut(..)) { EXEC_COLOR } else { DATA_COLOR };
                match from {
                    Pin::ExecOut(..) | Pin::DataOut(..) => self.wire(&painter, a, b, color, 2.0),
                    _ => self.wire(&painter, b, a, color, 2.0),
                }
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.dragging_wire = None;
                if let Some(pp) = pointer {
                    let target = pins
                        .iter()
                        .filter(|(p, _)| from.compatible(*p))
                        .map(|(p, pos)| (*p, pos.distance(pp)))
                        .filter(|(_, d)| *d < pin_r * 1.6)
                        .min_by(|a, b| a.1.total_cmp(&b.1));
                    if let Some((to, _)) = target {
                        connect(f, from, to);
                        changed = true;
                    } else if rect.contains(pp) {
                        self.open_menu(rect, pp, Some(from));
                    }
                }
            }
        }

        if let Some(e) = enter {
            let mut v = self.view.clone();
            v.push(e);
            self.go_to_view(v);
        }

        // ---- keyboard ----
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing {
            if !self.view.is_empty() && self.add_menu.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                let mut v = self.view.clone();
                v.pop();
                self.go_to_view(v);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                to_delete.extend(self.multi.iter().copied());
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::G)) {
                collapse = true;
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) {
                self.multi = ids.iter().copied().collect();
                self.selected = self.multi.iter().next().copied();
            }
        }
        if collapse && self.collapse_selection(f) {
            changed = true;
        }
        if let Some(g) = to_ungroup {
            let members = f.members(g);
            f.ungroup(g);
            self.multi = members.into_iter().collect();
            self.selected = self.multi.iter().next().copied();
            changed = true;
        }
        if !to_delete.is_empty() {
            for id in to_delete {
                if !f.is_fixed(id) {
                    f.remove(id);
                }
            }
            self.clear_selection();
            changed = true;
        }
        if let Some(id) = to_duplicate {
            let mut n = f.nodes[&id].clone();
            n.pos[0] += 30.0;
            n.pos[1] += 30.0;
            let new = f.add_in(n.kind, n.pos, n.scope);
            self.select_only(new);
            changed = true;
        }

        changed |= self.show_add_menu(ui, f, rect);

        // help text
        painter.text(
            rect.left_bottom() + Vec2::new(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            "Right-click: add node · Drag pin→pin: connect · Drag on empty space: box select · Right/middle-drag: pan · Scroll: zoom · Shift/Ctrl-click: multi-select · Ctrl+G: group · Del: delete",
            FontId::proportional(12.0),
            Color32::from_gray(130),
        );
        changed
    }

    fn open_menu(&mut self, rect: Rect, p: Pos2, pending: Option<Pin>) {
        self.add_menu = Some(AddMenu { screen_pos: p, world_pos: self.to_world(rect, p), search: String::new(), pending, just_opened: true });
    }

    fn show_add_menu(&mut self, ui: &mut egui::Ui, f: &mut FnGraph, _rect: Rect) -> bool {
        let Some(menu) = &mut self.add_menu else { return false };
        let mut chosen: Option<NodeKind> = None;
        let mut close = false;
        let first_frame = menu.just_opened;
        let area = egui::Area::new(ui.scope_id().with("rust-nodes-add-menu"))
            .fixed_pos(menu.screen_pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.label("Add node");
                    let te = ui.text_edit_singleline(&mut menu.search);
                    if menu.just_opened {
                        te.request_focus();
                        menu.just_opened = false;
                    }
                    let q = menu.search.to_lowercase();
                    let items: Vec<(&str, NodeKind)> = NodeKind::palette()
                        .into_iter()
                        .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
                        .filter(|(_, k)| match menu.pending {
                            None => true,
                            Some(p) => would_accept(k, p),
                        })
                        .collect();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (i, (label, kind)) in items.iter().enumerate() {
                            if ui.selectable_label(false, *label).clicked() || (enter && i == 0) {
                                chosen = Some(kind.clone());
                            }
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            });
        // (the click that opened the menu doesn't count as clicking elsewhere)
        if !first_frame && area.response.clicked_elsewhere() && chosen.is_none() {
            close = true;
        }
        let mut changed = false;
        if let Some(kind) = chosen {
            let pending = menu.pending;
            let pos = menu.world_pos;
            let id = f.add_in(kind, pos, self.view.last().copied());
            if let Some(p) = pending {
                if let Some(to) = first_compatible_pin(f, id, p) {
                    connect(f, p, to);
                }
            }
            self.select_only(id);
            close = true;
            changed = true;
        }
        if close {
            self.add_menu = None;
        }
        changed
    }

    fn exec_pin(&self, painter: &egui::Painter, p: Pos2, filled: bool) {
        let s = 6.0 * self.zoom;
        let pts = vec![p + Vec2::new(-s * 0.7, -s), p + Vec2::new(s, 0.0), p + Vec2::new(-s * 0.7, s)];
        let fill = if filled { EXEC_COLOR } else { Color32::TRANSPARENT };
        painter.add(Shape::convex_polygon(pts, fill, Stroke::new(1.5, EXEC_COLOR)));
    }

    fn data_pin(&self, painter: &egui::Painter, p: Pos2, filled: bool) {
        let r = 5.0 * self.zoom;
        if filled {
            painter.circle_filled(p, r, DATA_COLOR);
        } else {
            painter.circle_stroke(p, r, Stroke::new(1.5, DATA_COLOR));
        }
    }

    fn wire(&self, painter: &egui::Painter, a: Pos2, b: Pos2, color: Color32, width: f32) {
        let dx = ((b.x - a.x).abs() * 0.5).max(40.0 * self.zoom);
        let shape = CubicBezierShape::from_points_stroke(
            [a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b],
            false,
            Color32::TRANSPARENT,
            Stroke::new(width * self.zoom.max(0.5), color),
        );
        painter.add(shape);
    }

    pub fn to_screen(&self, rect: Rect, p: [f32; 2]) -> Pos2 {
        rect.min + (Vec2::new(p[0], p[1]) + self.pan) * self.zoom
    }

    pub fn to_world(&self, rect: Rect, p: Pos2) -> [f32; 2] {
        let v = (p - rect.min) / self.zoom - self.pan;
        [v.x, v.y]
    }

    /// Zoom/pan so the whole graph is visible.
    fn fit(&mut self, f: &FnGraph, rect: Rect, scope: Scope) {
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for n in f.nodes.values().filter(|n| n.scope == scope) {
            let [w, h] = node_size(&n.kind, &f.params);
            min = [min[0].min(n.pos[0]), min[1].min(n.pos[1])];
            max = [max[0].max(n.pos[0] + w), max[1].max(n.pos[1] + h)];
        }
        if min[0] > max[0] {
            return;
        }
        // extra room on the sides for "from outside" / "used outside" labels
        let margin = if scope.is_some() { 130.0 } else { 40.0 };
        let size = Vec2::new(max[0] - min[0] + margin * 2.0, max[1] - min[1] + 80.0);
        self.zoom = (rect.width() / size.x).min(rect.height() / size.y).clamp(0.6, 1.0);
        self.pan = Vec2::new(-min[0] + margin, -min[1] + 40.0);
    }

    /// Property editor for the selected node. Returns true if something changed.
    pub fn inspector(&mut self, ui: &mut egui::Ui, f: &mut FnGraph) -> bool {
        if self.multi.len() > 1 {
            ui.strong(format!("{} nodes selected", self.multi.len()));
            ui.add_space(4.0);
            let mut c = false;
            ui.horizontal(|ui| {
                if ui.button("▣ Collapse into a group").on_hover_text("Ctrl+G").clicked() {
                    c |= self.collapse_selection(f);
                }
                if ui.button("Delete").clicked() {
                    for id in std::mem::take(&mut self.multi) {
                        if !f.is_fixed(id) {
                            f.remove(id);
                        }
                    }
                    self.selected = None;
                    c = true;
                }
            });
            ui.weak("Groups only tidy up the graph: the generated Rust stays exactly the same.");
            return c;
        }
        let Some(id) = self.selected else {
            ui.weak("Select a node to edit it. Drag on empty space to select several.");
            return false;
        };
        let Some(node) = f.nodes.get_mut(&id) else {
            self.selected = None;
            return false;
        };
        ui.strong(node.kind.title());
        let mut c = false;
        let mut open: Option<(NodeId, usize)> = None;
        let mono = |ui: &mut egui::Ui, label: &str, s: &mut String| -> bool {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(s).code_editor().desired_width(f32::INFINITY)).changed()
            })
            .inner
        };
        let count = |ui: &mut egui::Ui, label: &str, n: &mut usize| -> bool {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::DragValue::new(n).range(0..=16)).changed()
            })
            .inner
        };
        use NodeKind::*;
        let mut ungroup = false;
        match &mut node.kind {
            Group { name } => {
                c |= mono(ui, "name", name);
                ui.horizontal(|ui| {
                    if ui.button("▶ Open").clicked() {
                        open = Some((id, 0));
                    }
                    if ui.button("Ungroup").clicked() {
                        ungroup = true;
                    }
                });
                if ui.button("ƒ Extract function…").on_hover_text("Turn this group into a real Rust function").clicked() {
                    self.extract_request = Some(id);
                }
                ui.weak("A group only tidies up the graph: the generated Rust is the same with or without it. Extract function turns it into a real helper function.");
            }
            Entry => {
                ui.weak("Function start. Edit the signature in the left panel.");
            }
            Let { name, mutable, ty } => {
                c |= mono(ui, "name", name);
                c |= ui.checkbox(mutable, "mut").changed();
                c |= mono(ui, "type", ty);
                ui.weak("Leave type empty to let Rust infer it.");
            }
            Assign { op } => {
                c |= mono(ui, "operator", op);
                ui.weak("=, +=, -=, *=, /=, %= ...");
            }
            For { var } => c |= mono(ui, "variable", var),
            Literal { code } => c |= mono(ui, "value", code),
            Var { name } => c |= mono(ui, "name/path", name),
            Binary { op } => c |= mono(ui, "operator", op),
            Unary { op } => c |= mono(ui, "operator", op),
            Call { path, args } => {
                c |= mono(ui, "function", path);
                c |= count(ui, "arguments", args);
            }
            Method { name, args } => {
                c |= mono(ui, "method", name);
                c |= count(ui, "arguments", args);
            }
            Macro { name, args, bracket } => {
                c |= mono(ui, "macro", name);
                c |= count(ui, "arguments", args);
                c |= ui.checkbox(bracket, "use [ ] brackets").changed();
            }
            Field { name } => c |= mono(ui, "field", name),
            Template { code, holes } | TemplateStmt { code, holes } => {
                ui.label("Rust code with holes:");
                c |= ui.add(egui::TextEdit::multiline(code).code_editor().desired_rows(6).desired_width(f32::INFINITY)).changed();
                ui.weak("Keep the ⟦n⟧ markers: they are where the holes go.");
                ui.add_space(4.0);
                for (i, h) in holes.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if h.block {
                            let b = if h.binders.is_empty() { String::new() } else { format!(" ({})", h.binders.join(", ")) };
                            ui.label(format!("{} block{b}", hole_label(i)));
                            if ui.small_button("Open").clicked() {
                                open = Some((id, i));
                            }
                        } else {
                            ui.label(format!("{} value input", hole_label(i)));
                        }
                    });
                }
            }
            BlockEntry { binders } => {
                if binders.is_empty() {
                    ui.weak("Start of this block.");
                } else {
                    ui.weak(format!("Start of this block. Values the construct gives the block: {}", binders.join(", ")));
                }
            }
            BlockValue => {
                ui.weak("The value this block produces (e.g. the result of a match arm or closure).");
            }
            RawExpr { code } | RawStmt { code } => {
                ui.label("Rust code:");
                c |= ui.add(egui::TextEdit::multiline(code).code_editor().desired_rows(6).desired_width(f32::INFINITY)).changed();
            }
            Eval | If | While | Loop | Break | Continue | Return | Index => {
                ui.weak("No properties.");
            }
        }
        // comment above the statement
        if let Some(node) = f.nodes.get_mut(&id) {
            if node.kind.is_exec() && !matches!(node.kind, NodeKind::Entry | NodeKind::BlockEntry { .. }) {
                ui.add_space(6.0);
                ui.label("Comment");
                let mut text = comment_display(&node.comment);
                if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY).hint_text("written as // lines above this statement")).changed() {
                    node.comment = comment_encode(&text, node.comment.starts_with('\n'));
                    c = true;
                }
            }
        }
        if c {
            f.prune_edges();
        }
        if ungroup {
            let members = f.members(id);
            f.ungroup(id);
            self.multi = members.into_iter().collect();
            self.selected = self.multi.iter().next().copied();
            c = true;
        }
        if let Some(o) = open {
            let mut v = self.view.clone();
            v.push(o);
            self.go_to_view(v);
        }
        c
    }
}

fn pin_key(p: &Pin) -> (u8, NodeId, usize) {
    match *p {
        Pin::ExecIn(n) => (0, n, 0),
        Pin::ExecOut(n, i) => (1, n, i),
        Pin::DataIn(n, i) => (2, n, i),
        Pin::DataOut(n, i) => (3, n, i),
    }
}

fn connect(f: &mut FnGraph, a: Pin, b: Pin) {
    match (a, b) {
        (Pin::ExecOut(n, i), Pin::ExecIn(t)) | (Pin::ExecIn(t), Pin::ExecOut(n, i)) => f.connect_exec(n, i, t),
        (Pin::DataOut(n, i), Pin::DataIn(t, j)) | (Pin::DataIn(t, j), Pin::DataOut(n, i)) => f.connect_data(n, i, t, j),
        _ => {}
    }
}

fn would_accept(k: &NodeKind, p: Pin) -> bool {
    match p {
        Pin::ExecOut(..) => k.has_exec_in(),
        Pin::ExecIn(..) => !k.exec_outs().is_empty(),
        Pin::DataOut(..) => !k.data_ins().is_empty(),
        Pin::DataIn(..) => !k.is_exec() && !k.data_outs(&[]).is_empty(),
    }
}

fn first_compatible_pin(f: &FnGraph, id: NodeId, p: Pin) -> Option<Pin> {
    let k = &f.nodes[&id].kind;
    match p {
        Pin::ExecOut(..) if k.has_exec_in() => Some(Pin::ExecIn(id)),
        Pin::ExecIn(..) => k.next_pin().map(|i| Pin::ExecOut(id, i)),
        Pin::DataOut(..) if !k.data_ins().is_empty() => Some(Pin::DataIn(id, 0)),
        Pin::DataIn(..) if !k.data_outs(&f.params).is_empty() => Some(Pin::DataOut(id, 0)),
        _ => None,
    }
}

fn container_label(k: &NodeKind) -> String {
    let code = match k {
        NodeKind::Template { code, .. } | NodeKind::TemplateStmt { code, .. } => code.as_str(),
        _ => return k.title(),
    };
    // first word of the construct: match, if, while, |closure|, unsafe ...
    let t = code.trim_start();
    if t.starts_with('|') || t.starts_with("move") {
        return "closure".into();
    }
    t.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')).find(|w| !w.is_empty()).unwrap_or("block").to_string()
}

/// Stored comment entries → plain text for display/editing (without `//`).
fn comment_display(stored: &str) -> String {
    stored
        .lines()
        .map(|l| {
            let l = l.trim_start_matches('~').trim();
            let l = l.strip_prefix("//").map(|x| x.strip_prefix(' ').unwrap_or(x)).unwrap_or(l);
            l.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

/// Edited text → stored entries (`// line` per line), keeping a blank line above if there was one.
fn comment_encode(text: &str, blank_above: bool) -> String {
    let mut out = String::new();
    if blank_above {
        out.push('\n');
    }
    for l in text.lines() {
        if l.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("// {}\n", l.trim_end()));
        }
    }
    if out.trim().is_empty() && !blank_above { String::new() } else { out }
}
