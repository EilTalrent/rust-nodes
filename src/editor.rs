//! The node canvas: pan/zoom, drawing nodes and wires, dragging wires between pins,
//! the right-click "add node" menu, and the inspector for the selected node.

use crate::graph::*;
use crate::layout::*;
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
    pub selected: Option<NodeId>,
    dragging_wire: Option<Pin>,
    add_menu: Option<AddMenu>,
    pub needs_fit: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Editor { pan: Vec2::new(40.0, 60.0), zoom: 1.0, selected: None, dragging_wire: None, add_menu: None, needs_fit: true }
    }
}

const EXEC_COLOR: Color32 = Color32::from_rgb(235, 235, 235);
const DATA_COLOR: Color32 = Color32::from_rgb(90, 200, 160);

fn header_color(c: Category) -> Color32 {
    match c {
        Category::Entry => Color32::from_rgb(170, 40, 40),
        Category::Flow => Color32::from_rgb(110, 110, 120),
        Category::Statement => Color32::from_rgb(40, 90, 170),
        Category::Value => Color32::from_rgb(150, 110, 30),
        Category::Op => Color32::from_rgb(40, 130, 90),
        Category::Call => Color32::from_rgb(120, 60, 150),
        Category::Raw => Color32::from_rgb(70, 70, 70),
    }
}

impl Editor {
    /// Draw the canvas for one function. Returns true if the graph changed.
    pub fn show(&mut self, ui: &mut egui::Ui, f: &mut FnGraph, salt: &str) -> bool {
        let mut changed = false;
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        let base_id = ui.scope_id().with(("rust-nodes-canvas", salt));

        if self.needs_fit {
            self.fit(f, rect);
            self.needs_fit = false;
        }

        // ---- pan & zoom ----
        if resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Primary) {
            self.pan += resp.drag_delta() / self.zoom;
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
            self.selected = None;
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

        // ---- wires ----
        for e in &f.data {
            if let (Some(a), Some(b)) = (f.nodes.get(&e.from), f.nodes.get(&e.to)) {
                let p0 = self.to_screen(rect, data_out_pos(a, e.from_pin, &f.params));
                let p1 = self.to_screen(rect, data_in_pos(b, e.to_pin));
                self.wire(&painter, p0, p1, DATA_COLOR, 2.0);
            }
        }
        for e in &f.exec {
            if let (Some(a), Some(b)) = (f.nodes.get(&e.from), f.nodes.get(&e.to)) {
                let p0 = self.to_screen(rect, exec_out_pos(a, e.from_pin, &f.params));
                let p1 = self.to_screen(rect, exec_in_pos(b));
                self.wire(&painter, p0, p1, EXEC_COLOR, 3.0);
            }
        }

        // ---- nodes ----
        let z = self.zoom;
        let ids: Vec<NodeId> = f.nodes.keys().copied().collect();
        let mut to_delete = None;
        let mut to_duplicate = None;
        let mut pins: Vec<(Pin, Pos2)> = vec![];
        for id in ids {
            let node = &f.nodes[&id];
            let [w, h] = node_size(&node.kind, &f.params);
            let r = Rect::from_min_size(self.to_screen(rect, node.pos), Vec2::new(w, h) * z);
            if !r.intersects(rect.expand(400.0)) {
                // still register pins so wires can be dropped near the edge
            }
            let nresp = ui.interact(r, base_id.with(("node", id)), Sense::click_and_drag());
            if nresp.dragged_by(egui::PointerButton::Primary) {
                let d = nresp.drag_delta() / z;
                let n = f.nodes.get_mut(&id).unwrap();
                n.pos[0] += d.x;
                n.pos[1] += d.y;
                changed = true;
            }
            if nresp.clicked() || nresp.drag_started() {
                self.selected = Some(id);
            }
            nresp.context_menu(|ui| {
                if id != f.entry && ui.button("Delete").clicked() {
                    to_delete = Some(id);
                    ui.close();
                }
                if id != f.entry && ui.button("Duplicate").clicked() {
                    to_duplicate = Some(id);
                    ui.close();
                }
            });

            let node = &f.nodes[&id];
            let kind = &node.kind;
            let sel = self.selected == Some(id);
            let cr = CornerRadius::same((6.0 * z).round().clamp(0.0, 255.0) as u8);
            painter.rect_filled(r.translate(Vec2::splat(3.0 * z)), cr, Color32::from_black_alpha(90));
            painter.rect_filled(r, cr, Color32::from_rgb(48, 48, 54));
            let header = Rect::from_min_size(r.min, Vec2::new(r.width(), HEADER * z));
            painter.rect_filled(
                header,
                CornerRadius { nw: cr.nw, ne: cr.ne, sw: 0, se: 0 },
                header_color(kind.category()),
            );
            let border = if sel { Stroke::new(2.0, Color32::from_rgb(255, 200, 60)) } else { Stroke::new(1.0, Color32::from_gray(20)) };
            painter.rect_stroke(r, cr, border, StrokeKind::Outside);
            painter.text(
                header.left_center() + Vec2::new(8.0 * z, 0.0),
                Align2::LEFT_CENTER,
                kind.title(),
                FontId::proportional(13.0 * z),
                Color32::WHITE,
            );

            let label_font = FontId::proportional(11.5 * z);
            let label_color = Color32::from_gray(200);
            // left pins
            if kind.has_exec_in() {
                let p = self.to_screen(rect, exec_in_pos(node));
                self.exec_pin(&painter, p, f.exec.iter().any(|e| e.to == id));
                pins.push((Pin::ExecIn(id), p));
            }
            for (i, name) in kind.data_ins().iter().enumerate() {
                let p = self.to_screen(rect, data_in_pos(node, i));
                let connected = f.data_source(id, i).is_some();
                self.data_pin(&painter, p, connected);
                painter.text(p + Vec2::new(10.0 * z, 0.0), Align2::LEFT_CENTER, name, label_font.clone(), label_color);
                pins.push((Pin::DataIn(id, i), p));
            }
            // right pins
            for (i, name) in kind.exec_outs().iter().enumerate() {
                let p = self.to_screen(rect, exec_out_pos(node, i, &f.params));
                self.exec_pin(&painter, p, f.exec_target(id, i).is_some());
                painter.text(p - Vec2::new(10.0 * z, 0.0), Align2::RIGHT_CENTER, *name, label_font.clone(), label_color);
                pins.push((Pin::ExecOut(id, i), p));
            }
            for (i, name) in kind.data_outs(&f.params).iter().enumerate() {
                let p = self.to_screen(rect, data_out_pos(node, i, &f.params));
                let connected = f.data.iter().any(|e| e.from == id && e.from_pin == i);
                self.data_pin(&painter, p, connected);
                painter.text(p - Vec2::new(10.0 * z, 0.0), Align2::RIGHT_CENTER, name, label_font.clone(), label_color);
                pins.push((Pin::DataOut(id, i), p));
            }
            // raw code body
            if let Some(code) = kind.body_text() {
                let rows = left_rows(kind).max(right_rows(kind, &f.params)).max(1);
                let mut y = r.top() + (HEADER + rows as f32 * ROW + 4.0) * z;
                for line in code.lines().take(12) {
                    painter.text(
                        Pos2::new(r.left() + 8.0 * z, y),
                        Align2::LEFT_TOP,
                        line,
                        FontId::monospace(11.5 * z),
                        Color32::from_rgb(220, 220, 170),
                    );
                    y += LINE * z;
                }
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

        // ---- keyboard ----
        if !ui.ctx().egui_wants_keyboard_input() && resp.ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            if let Some(sel) = self.selected {
                to_delete = Some(sel);
            }
        }
        if let Some(id) = to_delete {
            if id != f.entry {
                f.remove(id);
                self.selected = None;
                changed = true;
            }
        }
        if let Some(id) = to_duplicate {
            let mut n = f.nodes[&id].clone();
            n.pos[0] += 30.0;
            n.pos[1] += 30.0;
            self.selected = Some(f.add(n.kind, n.pos));
            changed = true;
        }

        changed |= self.show_add_menu(ui, f, rect);

        // help text
        painter.text(
            rect.left_bottom() + Vec2::new(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            "Right-click: add node   Drag pin→pin: connect   Drag wire to empty space: add & connect   Del: delete   Scroll: zoom   Drag background: pan",
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
        if area.response.clicked_elsewhere() && chosen.is_none() {
            close = true;
        }
        let mut changed = false;
        if let Some(kind) = chosen {
            let pending = menu.pending;
            let pos = menu.world_pos;
            let id = f.add(kind, pos);
            if let Some(p) = pending {
                if let Some(to) = first_compatible_pin(f, id, p) {
                    connect(f, p, to);
                }
            }
            self.selected = Some(id);
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
    fn fit(&mut self, f: &FnGraph, rect: Rect) {
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for n in f.nodes.values() {
            let [w, h] = node_size(&n.kind, &f.params);
            min = [min[0].min(n.pos[0]), min[1].min(n.pos[1])];
            max = [max[0].max(n.pos[0] + w), max[1].max(n.pos[1] + h)];
        }
        if min[0] > max[0] {
            return;
        }
        let size = Vec2::new(max[0] - min[0] + 80.0, max[1] - min[1] + 80.0);
        self.zoom = (rect.width() / size.x).min(rect.height() / size.y).clamp(0.6, 1.0);
        self.pan = Vec2::new(-min[0] + 40.0, -min[1] + 40.0);
    }

    /// Property editor for the selected node. Returns true if something changed.
    pub fn inspector(&mut self, ui: &mut egui::Ui, f: &mut FnGraph) -> bool {
        let Some(id) = self.selected else {
            ui.weak("Select a node to edit it.");
            return false;
        };
        let Some(node) = f.nodes.get_mut(&id) else {
            self.selected = None;
            return false;
        };
        ui.strong(node.kind.title());
        let mut c = false;
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
        match &mut node.kind {
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
            RawExpr { code } | RawStmt { code } => {
                ui.label("Rust code:");
                c |= ui.add(egui::TextEdit::multiline(code).code_editor().desired_rows(6).desired_width(f32::INFINITY)).changed();
            }
            Eval | If | While | Loop | Break | Continue | Return | Index => {
                ui.weak("No properties.");
            }
        }
        if c {
            f.prune_edges();
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
