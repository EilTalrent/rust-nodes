//! The "File overview": every item in the file as a card, in file order.
//! `use`, `const`, `mod`, structs, enums, functions, impl blocks, comments...
//! Wires show which items use which (by name), and the inspector edits them.

use crate::graph::*;
use crate::parse::pretty_item;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use quote::ToTokens;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardKind {
    Docs,
    Comment,
    Use,
    Const,
    Static,
    Mod,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Macro,
    Func,
    Impl,
    Other,
}

impl CardKind {
    fn color(self) -> Color32 {
        match self {
            CardKind::Docs => Color32::from_rgb(60, 100, 60),
            CardKind::Comment => Color32::from_rgb(70, 75, 70),
            CardKind::Use => Color32::from_rgb(70, 95, 140),
            CardKind::Const | CardKind::Static => Color32::from_rgb(150, 110, 30),
            CardKind::Mod => Color32::from_rgb(30, 120, 130),
            CardKind::Struct | CardKind::Enum | CardKind::TypeAlias => Color32::from_rgb(40, 130, 90),
            CardKind::Trait => Color32::from_rgb(90, 130, 40),
            CardKind::Macro => Color32::from_rgb(120, 60, 150),
            CardKind::Func => Color32::from_rgb(40, 90, 170),
            CardKind::Impl => Color32::from_rgb(110, 60, 130),
            CardKind::Other => Color32::from_rgb(80, 80, 80),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CardKind::Docs => "file docs",
            CardKind::Comment => "comment",
            CardKind::Use => "use",
            CardKind::Const => "const",
            CardKind::Static => "static",
            CardKind::Mod => "mod",
            CardKind::Struct => "struct",
            CardKind::Enum => "enum",
            CardKind::Trait => "trait",
            CardKind::TypeAlias => "type",
            CardKind::Macro => "macro",
            CardKind::Func => "fn",
            CardKind::Impl => "impl",
            CardKind::Other => "item",
        }
    }
}

/// Something a card row can open.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    /// a function graph, by its path in `Program::functions()`
    Function(Vec<usize>),
    /// another source file (from `mod name;`)
    File(String),
}

#[derive(Clone, Debug)]
pub struct Card {
    /// index in `program.items`
    pub idx: usize,
    pub kind: CardKind,
    pub title: String,
    pub lines: Vec<String>,
    /// names this item defines (for "used by" wires)
    pub names: Vec<String>,
    /// identifiers this item mentions
    pub mentions: HashSet<String>,
    pub opens: Vec<(String, Open)>,
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

const W: f32 = 270.0;
const HEADER: f32 = 24.0;
const LINE: f32 = 15.0;
const ROW: f32 = 20.0;
const MAX_LINES: usize = 10;
const COLUMN_HEIGHT: f32 = 900.0;

pub struct FileView {
    pub cards: Vec<Card>,
    pub selected: Option<usize>,
    pan: Vec2,
    zoom: f32,
    pub needs_fit: bool,
    /// text being edited in the code box: (item index, text, error)
    draft: Option<(usize, String, Option<String>)>,
    form: Option<(usize, Form)>,
    add_menu: Option<(Pos2, bool)>,
    /// set when a card asks to open something; the app handles it
    pub open_request: Option<Open>,
}

impl Default for FileView {
    fn default() -> Self {
        FileView {
            cards: vec![],
            selected: None,
            pan: Vec2::new(30.0, 30.0),
            zoom: 1.0,
            needs_fit: true,
            draft: None,
            form: None,
            add_menu: None,
            open_request: None,
        }
    }
}

// ---------------------------------------------------------------------------
// building cards

fn ident_set(text: &str) -> HashSet<String> {
    crate::highlight::tokenize(text)
        .into_iter()
        .filter(|(_, t)| matches!(t, crate::highlight::Tok::Plain | crate::highlight::Tok::Type | crate::highlight::Tok::Function | crate::highlight::Tok::Macro))
        .map(|(r, _)| text[r].trim_end_matches('!').to_string())
        .filter(|w| w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
        .collect()
}

fn use_leaves(tree: &syn::UseTree, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => use_leaves(&p.tree, out),
        syn::UseTree::Name(n) if n.ident != "self" => out.push(n.ident.to_string()),
        syn::UseTree::Name(_) => {}
        syn::UseTree::Rename(r) => out.push(r.rename.to_string()),
        syn::UseTree::Glob(_) => {}
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| use_leaves(t, out)),
    }
}

fn tokens(t: impl ToTokens) -> String {
    tidy(&t.to_token_stream().to_string())
}

/// Make token-printed text readable: "Vec < String >" -> "Vec<String>".
fn tidy(s: &str) -> String {
    s.replace(" :: ", "::")
        .replace(":: ", "::")
        .replace(" < ", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(" ,", ",")
        .replace("& ", "&")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace("[ ", "[")
        .replace(" ]", "]")
        .replace(" ;", ";")
        .replace("# [", "#[")
        .replace(" !", "!")
}

/// Summarise one raw item: kind, title, body lines, defined names.
fn summarize(text: &str) -> (CardKind, String, Vec<String>, Vec<String>) {
    let Ok(item) = syn::parse_str::<syn::Item>(text) else {
        return (CardKind::Other, "item".into(), text.lines().map(String::from).collect(), vec![]);
    };
    match &item {
        syn::Item::Use(u) => {
            let mut names = vec![];
            use_leaves(&u.tree, &mut names);
            let path = tokens(&u.tree);
            let lines = if names.len() > 1 { names.clone() } else { vec![] };
            (CardKind::Use, format!("use {}", short_path(&path)), lines, names)
        }
        syn::Item::Const(c) => {
            let value = crate::parse::pretty_expr(&c.expr);
            (CardKind::Const, format!("const {}", c.ident), vec![format!("{}: {}", c.ident, tokens(&c.ty)), format!("= {value}")], vec![c.ident.to_string()])
        }
        syn::Item::Static(s) => {
            let m = if matches!(s.mutability, syn::StaticMutability::Mut(_)) { "mut " } else { "" };
            let value = crate::parse::pretty_expr(&s.expr);
            (CardKind::Static, format!("static {m}{}", s.ident), vec![format!("{}: {}", s.ident, tokens(&s.ty)), format!("= {value}")], vec![s.ident.to_string()])
        }
        syn::Item::Mod(m) => {
            let lines = if m.content.is_some() { vec!["(inline module)".to_string()] } else { vec![] };
            (CardKind::Mod, format!("mod {}", m.ident), lines, vec![m.ident.to_string()])
        }
        syn::Item::Struct(s) => {
            let lines = match &s.fields {
                syn::Fields::Named(n) => n.named.iter().map(|f| format!("{}: {}", f.ident.as_ref().unwrap(), tokens(&f.ty))).collect(),
                syn::Fields::Unnamed(u) => u.unnamed.iter().enumerate().map(|(i, f)| format!("{i}: {}", tokens(&f.ty))).collect(),
                syn::Fields::Unit => vec![],
            };
            (CardKind::Struct, format!("struct {}{}", s.ident, tokens(&s.generics)), lines, vec![s.ident.to_string()])
        }
        syn::Item::Enum(e) => {
            let lines = e.variants.iter().map(|v| tokens(v)).collect();
            (CardKind::Enum, format!("enum {}{}", e.ident, tokens(&e.generics)), lines, vec![e.ident.to_string()])
        }
        syn::Item::Trait(t) => {
            let lines = t
                .items
                .iter()
                .map(|ti| match ti {
                    syn::TraitItem::Fn(f) => format!("fn {}", f.sig.ident),
                    other => tidy(&other.to_token_stream().to_string()),
                })
                .collect();
            (CardKind::Trait, format!("trait {}", t.ident), lines, vec![t.ident.to_string()])
        }
        syn::Item::Type(t) => (CardKind::TypeAlias, format!("type {}", t.ident), vec![format!("= {}", tokens(&t.ty))], vec![t.ident.to_string()]),
        syn::Item::Macro(m) => {
            let name = m.ident.as_ref().map(|i| i.to_string()).unwrap_or_else(|| tokens(&m.mac.path));
            (CardKind::Macro, format!("macro {name}!"), vec![], vec![name])
        }
        syn::Item::ExternCrate(e) => (CardKind::Use, format!("extern crate {}", e.ident), vec![], vec![e.ident.to_string()]),
        _ => (CardKind::Other, text.lines().next().unwrap_or("item").chars().take(40).collect(), text.lines().skip(1).map(String::from).collect(), vec![]),
    }
}

fn short_path(p: &str) -> String {
    if p.chars().count() > 34 { format!("{}…", p.chars().take(33).collect::<String>()) } else { p.to_string() }
}

impl FileView {
    /// Rebuild the cards from the program. `fn_paths` = `Program::functions()` paths, in order.
    pub fn refresh(&mut self, program: &Program, file_dir: Option<&std::path::Path>) {
        let fn_paths: Vec<Vec<usize>> = program.functions().into_iter().map(|(p, _)| p).collect();
        let mut cards = vec![];
        for (idx, it) in program.items.iter().enumerate() {
            let (kind, title, lines, names, mentions, opens): (CardKind, String, Vec<String>, Vec<String>, HashSet<String>, Vec<(String, Open)>) = match it {
                Item::Header(t) => (CardKind::Docs, "file docs".into(), t.lines().map(|l| l.trim_start_matches("//!").trim().to_string()).collect(), vec![], HashSet::new(), vec![]),
                Item::Comment(t) => (
                    CardKind::Comment,
                    "comment".into(),
                    t.lines().filter(|l| !l.is_empty()).map(|l| l.trim_start_matches('~').to_string()).collect(),
                    vec![],
                    HashSet::new(),
                    vec![],
                ),
                Item::Raw(t) => {
                    let (kind, title, lines, names) = summarize(t);
                    let mut opens = vec![];
                    if kind == CardKind::Mod && !t.contains('{') {
                        if let (Some(dir), Some(name)) = (file_dir, names.first()) {
                            for candidate in [dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs")] {
                                if candidate.exists() {
                                    let label = candidate.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                                    let shown = if label == "mod.rs" { format!("{name}/mod.rs") } else { label };
                                    opens.push((format!("▶ open {shown}"), Open::File(candidate.display().to_string())));
                                    break;
                                }
                            }
                        }
                    }
                    let mentions = if kind == CardKind::Use { HashSet::new() } else { ident_set(t) };
                    (kind, title, lines, names, mentions, opens)
                }
                Item::Func(f) => {
                    let path = vec![idx];
                    let code = crate::codegen::gen_fn(f);
                    (
                        CardKind::Func,
                        f.name.clone(),
                        vec![f.sig.lines().last().unwrap_or("").trim().to_string()],
                        vec![f.name.clone()],
                        ident_set(&code),
                        vec![("▶ open graph".into(), Open::Function(path))],
                    )
                }
                Item::Impl { header, items } => {
                    let mut lines = vec![];
                    let mut opens = vec![];
                    let mut mentions = ident_set(header);
                    for (j, inner) in items.iter().enumerate() {
                        match inner {
                            Item::Func(f) => {
                                let short = f.name.rsplit("::").next().unwrap_or(&f.name).to_string();
                                opens.push((format!("▶ {short}()"), Open::Function(vec![idx, j])));
                                mentions.extend(ident_set(&crate::codegen::gen_fn(f)));
                            }
                            Item::Raw(t) => {
                                lines.push(t.lines().next().unwrap_or("").trim().to_string());
                                mentions.extend(ident_set(t));
                            }
                            _ => {}
                        }
                    }
                    let title = header.lines().last().unwrap_or("impl").trim().to_string();
                    (CardKind::Impl, title, lines, vec![], mentions, opens)
                }
            };
            let _ = &fn_paths;
            // blank-line markers between items aren't worth a card
            if kind == CardKind::Comment && lines.iter().all(|l| l.trim().is_empty()) {
                continue;
            }
            cards.push(Card { idx, kind, title, lines, names, mentions, opens, pos: [0.0; 2], size: [W, 0.0] });
        }
        // a card doesn't "use" the names it defines itself
        for c in &mut cards {
            for n in c.names.clone() {
                c.mentions.remove(&n);
            }
        }
        // layout: columns in file order
        let (mut x, mut y) = (0.0, 0.0);
        for c in &mut cards {
            let shown = c.lines.len().min(MAX_LINES) as f32;
            let h = HEADER + shown * LINE + c.opens.len() as f32 * ROW + 12.0;
            if y > 0.0 && y + h > COLUMN_HEIGHT {
                x += W + 60.0;
                y = 0.0;
            }
            c.pos = [x, y];
            c.size = [W, h];
            y += h + 18.0;
        }
        if let Some(s) = self.selected {
            if s >= cards.len() {
                self.selected = None;
            }
        }
        self.cards = cards;
    }

    /// Pairs (definer card, user card) for "used by" wires.
    fn uses(&self) -> Vec<(usize, usize)> {
        let mut out = vec![];
        for (a, def) in self.cards.iter().enumerate() {
            for name in &def.names {
                for (b, user) in self.cards.iter().enumerate() {
                    if a != b && user.mentions.contains(name) {
                        out.push((a, b));
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn to_screen(&self, rect: Rect, p: [f32; 2]) -> Pos2 {
        rect.min + (Vec2::new(p[0], p[1]) + self.pan) * self.zoom
    }

    // -----------------------------------------------------------------------
    // canvas

    /// Returns an item to add (kind template) if the user picked one from the menu.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<&'static str> {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        let id = ui.scope_id().with("file-overview");
        if self.needs_fit && !self.cards.is_empty() {
            let w = self.cards.iter().map(|c| c.pos[0] + c.size[0]).fold(0.0, f32::max) + 60.0;
            let h = self.cards.iter().map(|c| c.pos[1] + c.size[1]).fold(0.0, f32::max) + 60.0;
            self.zoom = (rect.width() / w).min(rect.height() / h).clamp(0.55, 1.0);
            self.pan = Vec2::new(30.0, 30.0);
            self.needs_fit = false;
        }
        if resp.dragged_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Middle) || resp.dragged_by(egui::PointerButton::Secondary) {
            self.pan += resp.drag_delta() / self.zoom;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta().y);
            let factor = (scroll * 0.0015).exp() * ui.input(|i| i.zoom_delta());
            if (factor - 1.0).abs() > 1e-4 {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                    let before = (p - rect.min) / self.zoom - self.pan;
                    self.zoom = (self.zoom * factor).clamp(0.2, 2.5);
                    let after = (p - rect.min) / self.zoom - self.pan;
                    self.pan += after - before;
                }
            }
        }
        if resp.clicked() {
            self.selected = None;
            self.add_menu = None;
        }
        if resp.secondary_clicked() {
            self.add_menu = resp.interact_pointer_pos().map(|p| (p, true));
        }
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgb(28, 28, 32));
        let painter = painter.with_clip_rect(rect);
        let z = self.zoom;

        // "used by" wires
        let uses = self.uses();
        for (a, b) in &uses {
            let (ca, cb) = (&self.cards[*a], &self.cards[*b]);
            let involved = self.selected.is_some_and(|s| s == *a || s == *b);
            if self.selected.is_some() && !involved {
                continue;
            }
            let p0 = self.to_screen(rect, [ca.pos[0] + ca.size[0], ca.pos[1] + HEADER * 0.5]);
            let p1 = self.to_screen(rect, [cb.pos[0], cb.pos[1] + HEADER * 0.5]);
            let color = ca.kind.color();
            let color = if involved { color } else { Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 70) };
            let dx = ((p1.x - p0.x).abs() * 0.5).max(40.0 * z);
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p0, p0 + Vec2::new(dx, 0.0), p1 - Vec2::new(dx, 0.0), p1],
                false,
                Color32::TRANSPARENT,
                Stroke::new(if involved { 2.5 } else { 1.2 } * z.max(0.5), color),
            ));
        }

        // cards
        let mut open = None;
        for (i, c) in self.cards.iter().enumerate() {
            let r = Rect::from_min_size(self.to_screen(rect, c.pos), Vec2::new(c.size[0], c.size[1]) * z);
            let cr = ui.interact(r, id.with(("card", i)), Sense::click());
            if cr.clicked() {
                self.selected = Some(i);
                self.add_menu = None;
            }
            if cr.double_clicked() {
                if let Some((_, o)) = c.opens.first() {
                    open = Some(o.clone());
                }
            }
            let round = CornerRadius::same((6.0 * z) as u8);
            painter.rect_filled(r.translate(Vec2::splat(3.0 * z)), round, Color32::from_black_alpha(90));
            painter.rect_filled(r, round, Color32::from_rgb(46, 46, 52));
            let header = Rect::from_min_size(r.min, Vec2::new(r.width(), HEADER * z));
            painter.rect_filled(header, CornerRadius { nw: round.nw, ne: round.ne, sw: 0, se: 0 }, c.kind.color());
            let sel = self.selected == Some(i);
            painter.rect_stroke(r, round, if sel { Stroke::new(2.0, Color32::from_rgb(255, 200, 60)) } else { Stroke::new(1.0, Color32::from_gray(20)) }, StrokeKind::Outside);
            painter.text(header.left_center() + Vec2::new(8.0 * z, 0.0), Align2::LEFT_CENTER, short(&c.title.replace('\n', " ")), FontId::proportional(13.0 * z), Color32::WHITE);
            painter.text(header.right_center() - Vec2::new(8.0 * z, 0.0), Align2::RIGHT_CENTER, c.kind.label(), FontId::proportional(10.5 * z), Color32::from_white_alpha(150));
            let mut y = r.top() + (HEADER + 4.0) * z;
            let text_color = if matches!(c.kind, CardKind::Comment | CardKind::Docs) { Color32::from_rgb(130, 170, 120) } else { Color32::from_rgb(215, 215, 200) };
            for (li, line) in c.lines.iter().take(MAX_LINES).enumerate() {
                let line = if li + 1 == MAX_LINES && c.lines.len() > MAX_LINES { format!("… {} more", c.lines.len() - MAX_LINES + 1) } else { line.clone() };
                let line: String = line.chars().take(44).collect();
                painter.text(Pos2::new(r.left() + 8.0 * z, y), Align2::LEFT_TOP, line, FontId::monospace(11.0 * z), text_color);
                y += LINE * z;
            }
            for (k, (label, target)) in c.opens.iter().enumerate() {
                let row = Rect::from_min_size(Pos2::new(r.left() + 4.0 * z, y + 2.0 * z), Vec2::new(r.width() - 8.0 * z, (ROW - 3.0) * z));
                let rr = ui.interact(row, id.with(("open", i, k)), Sense::click());
                let fill = if rr.hovered() { Color32::from_rgb(60, 80, 120) } else { Color32::from_rgb(40, 55, 85) };
                painter.rect_filled(row, CornerRadius::same((4.0 * z) as u8), fill);
                painter.text(row.left_center() + Vec2::new(6.0 * z, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.5 * z), Color32::WHITE);
                if rr.clicked() {
                    open = Some(target.clone());
                }
                y += ROW * z;
            }
        }
        if open.is_some() {
            self.open_request = open;
        }

        // add menu
        let mut add = None;
        if let Some((p, first)) = self.add_menu {
            let area = egui::Area::new(id.with("add")).fixed_pos(p).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label("Add to the file");
                    for (label, template) in ITEM_TEMPLATES {
                        if ui.selectable_label(false, *label).clicked() {
                            add = Some(*template);
                        }
                    }
                });
            });
            if add.is_some() || (!first && area.response.clicked_elsewhere()) {
                self.add_menu = None;
            } else if first {
                self.add_menu = Some((p, false));
            }
        }

        painter.text(
            rect.left_bottom() + Vec2::new(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            "Every item in the file, in order · lines show what uses what · click to select & edit · double-click / ▶ to open · right-click: add",
            FontId::proportional(12.0),
            Color32::from_gray(130),
        );
        add
    }
}

/// Templates for the "add" menu: (label, code). "fn" adds a new function graph.
pub const ITEM_TEMPLATES: &[(&str, &str)] = &[
    ("use …", "use std::collections::HashMap;"),
    ("const …", "const NAME: i32 = 0;"),
    ("static …", "static NAME: &str = \"\";"),
    ("struct …", "struct NewStruct {\n    field: i32,\n}"),
    ("enum …", "enum NewEnum {\n    A,\n    B,\n}"),
    ("type alias …", "type Alias = Vec<i32>;"),
    ("mod … (other file)", "mod other;"),
    ("// comment", "// a note"),
    ("fn (new function graph)", "fn"),
];

// ---------------------------------------------------------------------------
// inspector

/// Structured form for common items. Rendered back to code on every change.
#[derive(Clone, Debug)]
enum Form {
    Use { prefix: String, tree: String },
    Const { prefix: String, kw: String, name: String, ty: String, value: String },
    Mod { prefix: String, name: String },
    Struct { prefix: String, name: String, generics: String, fields: Vec<(String, String, String, String)> },
    Enum { prefix: String, name: String, generics: String, variants: Vec<String> },
    TypeAlias { prefix: String, name: String, ty: String },
}

/// Attributes, doc comments and visibility in front of an item, as printed.
fn prefix_of(pretty: &str, keyword: &str) -> String {
    let mut out = String::new();
    for line in pretty.lines() {
        let t = line.trim_start();
        if t.starts_with("#[") || t.starts_with("///") || t.starts_with("//") {
            out.push_str(line);
            out.push('\n');
        } else {
            // visibility etc. on the item line, before the keyword
            if let Some(pos) = line.find(keyword) {
                out.push_str(&line[..pos]);
            }
            break;
        }
    }
    out
}

fn form_of(text: &str) -> Option<Form> {
    let item = syn::parse_str::<syn::Item>(text).ok()?;
    let pretty = pretty_item(&item);
    Some(match &item {
        syn::Item::Use(u) => Form::Use { prefix: prefix_of(&pretty, "use "), tree: tokens(&u.tree) },
        syn::Item::Const(c) => Form::Const {
            prefix: prefix_of(&pretty, "const "),
            kw: "const".into(),
            name: c.ident.to_string(),
            ty: tokens(&c.ty),
            value: crate::parse::pretty_expr(&c.expr),
        },
        syn::Item::Static(s) => Form::Const {
            prefix: prefix_of(&pretty, "static "),
            kw: if matches!(s.mutability, syn::StaticMutability::Mut(_)) { "static mut".into() } else { "static".into() },
            name: s.ident.to_string(),
            ty: tokens(&s.ty),
            value: crate::parse::pretty_expr(&s.expr),
        },
        syn::Item::Mod(m) if m.content.is_none() => Form::Mod { prefix: prefix_of(&pretty, "mod "), name: m.ident.to_string() },
        syn::Item::Struct(s) if matches!(s.fields, syn::Fields::Named(_)) => {
            let syn::Fields::Named(named) = &s.fields else { unreachable!() };
            let fields = named
                .named
                .iter()
                .map(|f| {
                    // a field's docs / attributes, printed via a one-field struct
                    let one: syn::ItemStruct = syn::parse_quote!(struct X { #f });
                    let p = pretty_item(&syn::Item::Struct(one));
                    let attrs: String = p
                        .lines()
                        .map(str::trim)
                        .filter(|l| l.starts_with("#[") || l.starts_with("///"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let vis = tokens(&f.vis);
                    (attrs, if vis.is_empty() { String::new() } else { format!("{vis} ") }, f.ident.as_ref().unwrap().to_string(), tokens(&f.ty))
                })
                .collect();
            Form::Struct { prefix: prefix_of(&pretty, "struct "), name: s.ident.to_string(), generics: tokens(&s.generics), fields }
        }
        syn::Item::Enum(e) => Form::Enum {
            prefix: prefix_of(&pretty, "enum "),
            name: e.ident.to_string(),
            generics: tokens(&e.generics),
            variants: e.variants.iter().map(|v| tidy(&v.to_token_stream().to_string())).collect(),
        },
        syn::Item::Type(t) => Form::TypeAlias { prefix: prefix_of(&pretty, "type "), name: t.ident.to_string(), ty: tokens(&t.ty) },
        _ => return None,
    })
}

fn render(form: &Form) -> String {
    match form {
        Form::Use { prefix, tree } => format!("{prefix}use {tree};"),
        Form::Const { prefix, kw, name, ty, value } => format!("{prefix}{kw} {name}: {ty} = {value};"),
        Form::Mod { prefix, name } => format!("{prefix}mod {name};"),
        Form::Struct { prefix, name, generics, fields } => {
            let body: Vec<String> = fields
                .iter()
                .map(|(attrs, vis, n, t)| {
                    let a = if attrs.is_empty() { String::new() } else { format!("{attrs}\n") };
                    format!("{a}{vis}{n}: {t},")
                })
                .collect();
            format!("{prefix}struct {name}{generics} {{\n{}\n}}", body.join("\n"))
        }
        Form::Enum { prefix, name, generics, variants } => format!("{prefix}enum {name}{generics} {{\n{}\n}}", variants.iter().map(|v| format!("{v},")).collect::<Vec<_>>().join("\n")),
        Form::TypeAlias { prefix, name, ty } => format!("{prefix}type {name} = {ty};"),
    }
}

/// What the inspector asks the app to do.
pub enum ItemAction {
    None,
    /// item text changed (already applied)
    Changed,
    MoveUp,
    MoveDown,
    Delete,
}

impl FileView {
    pub fn inspector(&mut self, ui: &mut egui::Ui, program: &mut Program) -> ItemAction {
        let Some(sel) = self.selected else {
            ui.weak("Select a card to edit it. Right-click the canvas to add use / const / struct / mod ...");
            return ItemAction::None;
        };
        let Some(card) = self.cards.get(sel).cloned() else { return ItemAction::None };
        let idx = card.idx;
        ui.strong(format!("{} · {}", card.kind.label(), short(&card.title)));
        let mut action = ItemAction::None;
        ui.horizontal(|ui| {
            if ui.add_enabled(idx > 0, egui::Button::new("▲ Move up")).clicked() {
                action = ItemAction::MoveUp;
            }
            if ui.add_enabled(idx + 1 < program.items.len(), egui::Button::new("▼ Move down")).clicked() {
                action = ItemAction::MoveDown;
            }
            if ui.button("Delete").clicked() {
                action = ItemAction::Delete;
            }
        });
        if !matches!(action, ItemAction::None) {
            self.draft = None;
            self.form = None;
            return action;
        }
        ui.add_space(4.0);
        match &program.items[idx] {
            Item::Func(_) | Item::Impl { .. } => {
                ui.weak("Functions are edited as graphs:");
                for (label, target) in &card.opens {
                    if ui.button(label).clicked() {
                        self.open_request = Some(target.clone());
                    }
                }
                return ItemAction::None;
            }
            Item::Comment(text) | Item::Header(text) => {
                let is_header = matches!(program.items[idx], Item::Header(_));
                let mut t = text.clone();
                ui.label(if is_header { "File docs (//! lines)" } else { "Comment" });
                if ui.add(egui::TextEdit::multiline(&mut t).code_editor().desired_rows(4).desired_width(f32::INFINITY)).changed() {
                    program.items[idx] = if is_header { Item::Header(t) } else { Item::Comment(t) };
                    return ItemAction::Changed;
                }
                return ItemAction::None;
            }
            Item::Raw(_) => {}
        }
        let Item::Raw(text) = &program.items[idx] else { return ItemAction::None };
        let text = text.clone();

        // structured form, if this kind has one
        if self.form.as_ref().is_none_or(|(i, _)| *i != idx) {
            self.form = form_of(&text).map(|f| (idx, f));
            self.draft = None;
        }
        let mut new_text: Option<String> = None;
        if let Some((_, form)) = &mut self.form {
            let mut changed = false;
            let field = |ui: &mut egui::Ui, label: &str, s: &mut String| -> bool {
                ui.horizontal(|ui| {
                    ui.label(label);
                    ui.add(egui::TextEdit::singleline(s).code_editor().desired_width(f32::INFINITY)).changed()
                })
                .inner
            };
            match form {
                Form::Use { tree, .. } => {
                    changed |= field(ui, "use", tree);
                    ui.weak("e.g. std::collections::{HashMap, HashSet}");
                }
                Form::Const { kw, name, ty, value, .. } => {
                    ui.horizontal(|ui| {
                        for k in ["const", "static", "static mut"] {
                            changed |= ui.selectable_value(kw, k.to_string(), k).changed();
                        }
                    });
                    changed |= field(ui, "name", name);
                    changed |= field(ui, "type", ty);
                    ui.label("value");
                    changed |= ui.add(egui::TextEdit::multiline(value).code_editor().desired_rows(2).desired_width(f32::INFINITY)).changed();
                }
                Form::Mod { name, .. } => {
                    changed |= field(ui, "mod", name);
                    ui.weak("A module in another file (name.rs or name/mod.rs next to this one).");
                }
                Form::Struct { name, generics, fields, .. } => {
                    changed |= field(ui, "struct", name);
                    changed |= field(ui, "generics", generics);
                    ui.label("Fields");
                    let mut remove = None;
                    egui::Grid::new("fields").num_columns(3).spacing([6.0, 3.0]).show(ui, |ui| {
                        for (i, (_, _, n, t)) in fields.iter_mut().enumerate() {
                            changed |= ui.add_sized([110.0, 20.0], egui::TextEdit::singleline(n).code_editor()).changed();
                            changed |= ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(t).code_editor()).changed();
                            if ui.small_button("✕").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                    if let Some(i) = remove {
                        fields.remove(i);
                        changed = true;
                    }
                    if ui.small_button("+ field").clicked() {
                        fields.push((String::new(), String::new(), format!("field{}", fields.len()), "i32".into()));
                        changed = true;
                    }
                }
                Form::Enum { name, generics, variants, .. } => {
                    changed |= field(ui, "enum", name);
                    changed |= field(ui, "generics", generics);
                    ui.label("Variants");
                    let mut remove = None;
                    for (i, v) in variants.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            changed |= ui.add(egui::TextEdit::singleline(v).code_editor().desired_width(220.0)).changed();
                            if ui.small_button("✕").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        variants.remove(i);
                        changed = true;
                    }
                    if ui.small_button("+ variant").clicked() {
                        variants.push(format!("Variant{}", variants.len()));
                        changed = true;
                    }
                }
                Form::TypeAlias { name, ty, .. } => {
                    changed |= field(ui, "type", name);
                    changed |= field(ui, "=", ty);
                }
            }
            if changed {
                new_text = Some(render(form));
            }
            ui.add_space(6.0);
            ui.collapsing("Rust code", |ui| {
                code_box(ui, &mut self.draft, idx, &text, &mut new_text);
            });
        } else {
            ui.label("Rust code");
            code_box(ui, &mut self.draft, idx, &text, &mut new_text);
        }

        // apply only valid Rust
        if let Some(t) = new_text {
            match syn::parse_str::<syn::Item>(&t) {
                Ok(_) => {
                    program.items[idx] = Item::Raw(t);
                    if let Some((_, msg)) = self.draft.as_mut().map(|d| (d.0, &mut d.2)) {
                        *msg = None;
                    }
                    self.form_error_clear();
                    return ItemAction::Changed;
                }
                Err(e) => {
                    let d = self.draft.get_or_insert((idx, t.clone(), None));
                    d.2 = Some(e.to_string());
                }
            }
        }
        if let Some((i, _, Some(err))) = &self.draft {
            if *i == idx {
                ui.colored_label(Color32::from_rgb(230, 140, 100), format!("Not applied yet: {err}"));
            }
        }
        action
    }

    fn form_error_clear(&mut self) {
        if let Some(d) = &mut self.draft {
            d.2 = None;
        }
    }

    /// Forget editing state (after the program changed shape).
    pub fn reset_editing(&mut self) {
        self.draft = None;
        self.form = None;
    }
}

/// Free-form code editor for an item; edits go into `draft` and are proposed via `new_text`.
fn code_box(ui: &mut egui::Ui, draft: &mut Option<(usize, String, Option<String>)>, idx: usize, text: &str, new_text: &mut Option<String>) {
    if draft.as_ref().is_none_or(|d| d.0 != idx) {
        *draft = Some((idx, text.to_string(), None));
    }
    let d = draft.as_mut().unwrap();
    if d.2.is_none() && d.1 != text && new_text.is_none() {
        d.1 = text.to_string(); // follow outside changes
    }
    if ui.add(egui::TextEdit::multiline(&mut d.1).code_editor().desired_rows(6).desired_width(f32::INFINITY)).changed() {
        *new_text = Some(d.1.clone());
    }
}

/// Byte ranges in the generated code for each top-level item index (for highlighting a card's code).
pub fn item_ranges(program: &Program, code: &str) -> HashMap<usize, std::ops::Range<usize>> {
    use syn::spanned::Spanned;
    let mut out = HashMap::new();
    let Ok(file) = syn::parse_file(code) else { return out };
    // output items correspond to program items that aren't comments / file docs
    let idxs: Vec<usize> = program.items.iter().enumerate().filter(|(_, it)| !matches!(it, Item::Comment(_) | Item::Header(_))).map(|(i, _)| i).collect();
    for (k, item) in file.items.iter().enumerate() {
        if let Some(&idx) = idxs.get(k) {
            let s = item.span();
            out.insert(idx, crate::highlight::line_range(code, s.start().line - 1, s.end().line - 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_for_every_item() {
        let src = "//! docs\n\n// note\nuse std::collections::{HashMap, HashSet};\nmod tree;\nconst LIMIT: usize = 10;\n#[derive(Debug)]\npub struct Pane {\n    /// rows\n    rows: u16,\n    name: String,\n}\nenum Dir { Left, Right(u8) }\nimpl Pane { fn rows(&self) -> u16 { self.rows } }\nfn main() { let m: HashMap<u8, u8> = HashMap::new(); println!(\"{} {}\", LIMIT, m.len()); }\n";
        let p = crate::parse::parse_program(src).unwrap();
        let mut fv = FileView::default();
        fv.refresh(&p, None);
        let kinds: Vec<CardKind> = fv.cards.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            vec![CardKind::Docs, CardKind::Comment, CardKind::Use, CardKind::Mod, CardKind::Const, CardKind::Struct, CardKind::Enum, CardKind::Impl, CardKind::Func]
        );
        let s = &fv.cards[5];
        assert_eq!(s.title, "struct Pane");
        assert_eq!(s.lines, vec!["rows: u16", "name: String"]);
        // HashMap (from the use) and LIMIT (the const) are wired to main
        let uses = fv.uses();
        assert!(uses.contains(&(2, 8)), "use -> main: {uses:?}");
        assert!(uses.contains(&(4, 8)), "const -> main: {uses:?}");
        assert!(uses.contains(&(5, 7)), "struct -> impl: {uses:?}");
        // functions and impls open their graphs
        assert_eq!(fv.cards[8].opens[0].1, Open::Function(vec![8]));
        assert_eq!(fv.cards[7].opens[0].1, Open::Function(vec![7, 0]));
        // code ranges per item
        let g = crate::codegen::generate(&p);
        let ranges = item_ranges(&p, &g.code);
        assert_eq!(&g.code[ranges[&4].clone()], "const LIMIT: usize = 10;");
        assert!(g.code[ranges[&5].clone()].starts_with("#[derive(Debug)]\npub struct Pane {"));
    }

    #[test]
    fn forms_rebuild_valid_rust() {
        for src in [
            "pub use std::collections::{HashMap, HashSet};",
            "const LIMIT: usize = 10 * 2;",
            "static mut COUNT: u32 = 0;",
            "pub mod tree;",
            "#[derive(Debug, Clone)]\n/// A pane\npub struct Pane<T> {\n    /// How many rows\n    pub rows: u16,\n    data: Vec<T>,\n}",
            "enum Dir { Left, Right(u8), Up { by: i32 } }",
            "type Grid = Vec<Vec<char>>;",
        ] {
            let form = form_of(src).unwrap_or_else(|| panic!("no form for {src}"));
            let out = render(&form);
            let a = syn::parse_str::<syn::Item>(src).unwrap();
            let b = syn::parse_str::<syn::Item>(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
            assert_eq!(pretty_item(&a), pretty_item(&b), "{src}\n---\n{out}");
        }
    }
}
