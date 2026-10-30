//! The application window: toolbar, function list, canvas, inspector,
//! live generated-code view and cargo output.

use crate::codegen::{generate, Generated};
use crate::editor::Editor;
use crate::graph::*;
use crate::parse::parse_signature;
use crate::codegen;
use crate::extract;
use crate::fileview;
use crate::highlight;
use crate::project;
use eframe::egui;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const STARTER: &str = r#"fn main() {
    let name = "world";
    println!("Hello, {}!", name);
}
"#;

pub fn run(file: Option<PathBuf>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 860.0]).with_title("rust-nodes"),
        ..Default::default()
    };
    eframe::run_native("rust-nodes", options, Box::new(move |_cc| Ok(Box::new(App::new(file)))))
}

struct App {
    path_text: String,
    path: Option<PathBuf>,
    program: Program,
    current: Vec<usize>,
    editors: HashMap<Vec<usize>, Editor>,
    code: Generated,
    unsaved: bool,
    status: String,
    sig_text: String,
    sig_error: Option<String>,
    output: Arc<Mutex<String>>,
    running: Arc<AtomicBool>,
    /// false until a .rs file has been opened or created: the window starts blank
    has_file: bool,
    /// "Extract function" dialog
    extract: Option<ExtractDialog>,
    /// the File overview (every item as a card) is showing instead of a function graph
    show_file: bool,
    file_view: fileview::FileView,
    /// file view / item ranges need rebuilding (program or code changed)
    views_dirty: bool,
    item_ranges: std::collections::HashMap<usize, std::ops::Range<usize>>,
    /// what the code view last highlighted (to scroll only when the selection changes)
    last_marked: Option<(Vec<usize>, std::collections::BTreeSet<NodeId>, usize)>,
    pending_ask: bool,
    /// the "add a Rust source file?" prompt
    show_prompt: bool,
    prompt_error: Option<String>,
}

impl App {
    fn new(file: Option<PathBuf>) -> Self {
        let program = Program { items: vec![] };
        let code = generate(&program);
        let mut app = App {
            path_text: String::new(),
            path: None,
            program,
            current: vec![],
            editors: HashMap::new(),
            code,
            unsaved: false,
            status: "No file open.".into(),
            sig_text: String::new(),
            sig_error: None,
            output: Arc::new(Mutex::new(String::new())),
            running: Arc::new(AtomicBool::new(false)),
            has_file: false,
            extract: None,
            show_file: false,
            file_view: fileview::FileView::default(),
            views_dirty: true,
            item_ranges: Default::default(),
            last_marked: None,
            pending_ask: false,
            show_prompt: true,
            prompt_error: None,
        };
        if let Some(f) = file {
            app.path_text = f.display().to_string();
            if f.exists() {
                app.open();
            } else {
                app.prompt_error = Some(format!("{} doesn't exist yet. Press \"Create new\" to make it.", f.display()));
            }
        }
        app.show_prompt = !app.has_file;
        app.select_first();
        app
    }

    /// Open the file in `path_text`. If it doesn't exist and `create` is set, make it with a hello-world starter.
    fn open_or_create(&mut self, create: bool) -> Result<(), String> {
        let path = PathBuf::from(self.path_text.trim());
        if path.as_os_str().is_empty() {
            return Err("Type a path to a .rs file first.".into());
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            return Err("That isn't a .rs file.".into());
        }
        if !path.exists() {
            if !create {
                return Err(format!("{} doesn't exist. Use \"Create new\" to make it.", path.display()));
            }
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, STARTER).map_err(|e| e.to_string())?;
        }
        self.open();
        if self.has_file { Ok(()) } else { Err(self.status.clone()) }
    }

    /// Native "open file" dialog.
    fn browse_open(&mut self) -> Result<(), String> {
        match rfd::FileDialog::new().add_filter("Rust source", &["rs"]).set_title("Open a Rust source file").pick_file() {
            Some(p) => {
                self.path_text = p.display().to_string();
                self.open_or_create(false)
            }
            None => Err("No file chosen. (If no dialog appeared, type a path instead.)".into()),
        }
    }

    /// Native "save as" dialog, then create the file with a starter program.
    fn browse_new(&mut self) -> Result<(), String> {
        match rfd::FileDialog::new()
            .add_filter("Rust source", &["rs"])
            .set_file_name("main.rs")
            .set_title("Create a new Rust source file")
            .save_file()
        {
            Some(p) => {
                self.path_text = p.display().to_string();
                self.open_or_create(true)
            }
            None => Err("No file chosen. (If no dialog appeared, type a path instead.)".into()),
        }
    }

    fn prompt(&mut self, ctx: &egui::Context) {
        let mut close = false;
        egui::Modal::new(egui::Id::unique("add-rs-file")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Welcome to rust-nodes");
            ui.add_space(4.0);
            ui.label("Do you want to add a Rust (.rs) source file?");
            ui.add_space(10.0);
            let mut result = None;
            ui.horizontal(|ui| {
                if ui.button("Open existing .rs…").clicked() {
                    result = Some(self.browse_open());
                }
                if ui.button("✚ Create new .rs…").clicked() {
                    result = Some(self.browse_new());
                }
            });
            ui.add_space(10.0);
            ui.label("…or type a path:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.path_text).code_editor().desired_width(260.0).hint_text("my_app/src/main.rs"));
                if ui.button("Open").clicked() {
                    result = Some(self.open_or_create(false));
                }
                if ui.button("Create new").clicked() {
                    result = Some(self.open_or_create(true));
                }
            });
            ui.weak("Tip: put the file in a cargo project (like my_app/src/main.rs) so ▶ Run works.");
            match result {
                Some(Ok(())) => close = true,
                Some(Err(e)) => self.prompt_error = Some(e),
                None => {}
            }
            if let Some(e) = &self.prompt_error {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 100), e);
            }
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Not now").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.show_prompt = false;
            self.prompt_error = None;
        }
    }

    fn select_first(&mut self) {
        let fns = self.program.functions();
        let main = fns.iter().find(|(_, f)| f.name == "main").or(fns.first());
        self.current = main.map(|(p, _)| p.clone()).unwrap_or_default();
        self.load_sig();
    }

    fn load_sig(&mut self) {
        self.sig_text = self.program.function_mut(&self.current.clone()).map(|f| f.sig.clone()).unwrap_or_default();
        self.sig_error = None;
    }

    fn open(&mut self) {
        let path = PathBuf::from(self.path_text.trim());
        match project::open(&path) {
            Ok(loaded) => {
                self.program = loaded.program;
                self.status = loaded.note;
                self.has_file = true;
                self.path = Some(path);
                self.editors.clear();
                self.unsaved = false;
                self.code = generate(&self.program);
                self.views_dirty = true;
                self.file_view = fileview::FileView::default();
                self.select_first();
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
    }

    fn save(&mut self) -> bool {
        let path = PathBuf::from(self.path_text.trim());
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        match project::save(&path, &self.program) {
            Ok(g) => {
                self.status = match &g.error {
                    Some(e) => format!("Saved, but the code has a syntax error: {e}"),
                    None => format!("Saved {}", path.display()),
                };
                self.code = g;
                self.path = Some(path);
                self.unsaved = false;
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    fn cargo(&mut self, sub: &'static str, ctx: &egui::Context) {
        if self.running.load(Ordering::SeqCst) || !self.save() {
            return;
        }
        let path = self.path.clone().unwrap();
        let Some(root) = project::cargo_root(&path) else {
            self.status = "No Cargo.toml found above this file. Put it in a cargo project (e.g. my_app/src/main.rs).".into();
            return;
        };
        let out = self.output.clone();
        let running = self.running.clone();
        let ctx = ctx.clone();
        *out.lock().unwrap() = format!("$ cargo {sub}   (in {})\n", root.display());
        running.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let child = Command::new("cargo")
                .arg(sub)
                .arg("--color=never")
                .current_dir(&root)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match child {
                Ok(mut child) => {
                    let stderr = child.stderr.take().unwrap();
                    let stdout = child.stdout.take().unwrap();
                    let (o2, c2) = (out.clone(), ctx.clone());
                    let t = std::thread::spawn(move || {
                        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                            o2.lock().unwrap().push_str(&(line + "\n"));
                            c2.request_repaint();
                        }
                    });
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        out.lock().unwrap().push_str(&(line + "\n"));
                        ctx.request_repaint();
                    }
                    let _ = t.join();
                    let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
                    out.lock().unwrap().push_str(&format!("\n[finished: {status}]\n"));
                }
                Err(e) => out.lock().unwrap().push_str(&format!("could not start cargo: {e}\n")),
            }
            running.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn add_function(&mut self) {
        let n = self.program.functions().len();
        let name = format!("new_function_{n}");
        let sig = format!("fn {name}()");
        self.program.items.push(Item::Func(FnGraph::new(name, sig, vec![])));
        self.current = vec![self.program.items.len() - 1];
        self.load_sig();
        self.mark_changed();
    }

    fn delete_function(&mut self) {
        let path = self.current.clone();
        let mut items = &mut self.program.items;
        for (d, &i) in path.iter().enumerate() {
            if d + 1 == path.len() {
                if i < items.len() {
                    items.remove(i);
                }
                break;
            }
            match items.get_mut(i) {
                Some(Item::Impl { items: inner, .. }) => items = inner,
                _ => return,
            }
        }
        self.editors.clear();
        self.select_first();
        self.mark_changed();
    }

    /// Rebuild the File overview cards and item code ranges if the program changed.
    fn refresh_views(&mut self) {
        if !self.views_dirty {
            return;
        }
        self.views_dirty = false;
        let dir = self.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        self.file_view.refresh(&self.program, dir.as_deref());
        self.item_ranges = fileview::item_ranges(&self.program, &self.code.code);
    }

    /// After items were added/moved/removed: function paths may have shifted, so
    /// find the current function again by name and drop per-function view state.
    fn items_reshaped(&mut self, current_name: Option<String>) {
        self.editors.clear();
        self.file_view.reset_editing();
        if let Some(name) = current_name {
            if let Some((p, _)) = self.program.functions().into_iter().find(|(_, f)| f.name == name) {
                self.current = p;
            }
        }
        if self.program.function_mut(&self.current.clone()).is_none() {
            self.select_first();
        }
        self.load_sig();
        self.mark_changed();
    }

    fn current_name(&self) -> Option<String> {
        self.program.functions().into_iter().find(|(p, _)| *p == self.current).map(|(_, f)| f.name.clone())
    }

    /// Handle something a File overview card asked to open.
    fn handle_open(&mut self) {
        let Some(req) = self.file_view.open_request.take() else { return };
        match req {
            fileview::Open::Function(path) => {
                if self.program.function_mut(&path).is_some() {
                    self.current = path;
                    self.show_file = false;
                    self.load_sig();
                }
            }
            fileview::Open::File(file) => {
                if self.unsaved && !self.save() {
                    return;
                }
                self.path_text = file;
                if let Err(e) = self.open_or_create(false) {
                    self.status = e;
                }
            }
        }
    }

    /// Insert a new item from the File overview's add menu (after the selected card).
    fn add_item(&mut self, template: &str) {
        let name = self.current_name();
        let at = self.file_view.selected.and_then(|s| self.file_view.cards.get(s)).map(|c| c.idx + 1).unwrap_or(self.program.items.len());
        let item = if template == "fn" {
            let n = self.program.functions().len();
            let fname = format!("new_function_{n}");
            Item::Func(FnGraph::new(fname.clone(), format!("fn {fname}()"), vec![]))
        } else if template.starts_with("//") {
            Item::Comment(format!("{template}\n"))
        } else {
            Item::Raw(template.to_string())
        };
        self.program.items.insert(at.min(self.program.items.len()), item);
        self.items_reshaped(name);
        self.refresh_views();
        self.file_view.selected = self.file_view.cards.iter().position(|c| c.idx == at);
    }

    fn take_extract_request(&mut self, path: &[usize]) {
        let Some(g) = self.editors.get_mut(path).and_then(|e| e.extract_request.take()) else { return };
        let Some(f) = self.program.function_mut(path) else { return };
        let plan = extract::plan(f, g);
        let unknown = plan.as_ref().is_ok_and(|p| p.params.iter().any(|x| x.ty.is_empty()) || p.outputs.iter().any(|x| x.ty.is_empty()));
        self.extract = Some(ExtractDialog { fn_path: path.to_vec(), plan, asking: None, notes: vec![] });
        // some types couldn't be guessed: ask the compiler straight away
        if unknown {
            self.pending_ask = true;
        }
    }

    fn mark_changed(&mut self) {
        self.views_dirty = true;
        self.unsaved = true;
        self.code = generate(&self.program);
    }
}

type ProbeAnswer = Result<Vec<(extract::ProbeTarget, Result<String, String>)>, String>;

struct ExtractDialog {
    fn_path: Vec<usize>,
    plan: Result<extract::Plan, String>,
    /// answer from a background compiler run, when one is in progress
    asking: Option<Arc<Mutex<Option<ProbeAnswer>>>>,
    /// what the compiler run told us (shown under the fields)
    notes: Vec<String>,
}

impl App {
    /// Start a background compiler run that fills in the dialog's types.
    fn ask_compiler(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else { return };
        let Some(dlg) = &mut self.extract else { return };
        let Ok(plan) = &dlg.plan else { return };
        match extract::probe_code(&self.program, &dlg.fn_path, plan) {
            Err(e) => dlg.notes = vec![e],
            Ok((code, targets)) => {
                let slot = Arc::new(Mutex::new(None));
                dlg.asking = Some(slot.clone());
                dlg.notes.clear();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let answer = crate::compiler::probe_types(&path, &code, targets.len())
                        .map(|types| targets.into_iter().zip(types).collect());
                    *slot.lock().unwrap() = Some(answer);
                    ctx.request_repaint();
                });
            }
        }
    }

    fn extract_dialog(&mut self, ctx: &egui::Context) {
        // collect a finished compiler answer
        if let Some(dlg) = &mut self.extract {
            let done = dlg.asking.as_ref().and_then(|slot| slot.lock().unwrap().take());
            if let Some(answer) = done {
                dlg.asking = None;
                match (answer, &mut dlg.plan) {
                    (Err(e), _) => dlg.notes = vec![e],
                    (Ok(list), Ok(plan)) => {
                        let mut filled = 0;
                        for (t, ty) in list {
                            let (name, slot) = match t {
                                extract::ProbeTarget::Param(k) => (format!("parameter `{}`", plan.params[k].name), &mut plan.params[k].ty),
                                extract::ProbeTarget::Output(k) => (format!("returned `{}`", plan.outputs[k].name), &mut plan.outputs[k].ty),
                            };
                            match ty {
                                Ok(ty) => {
                                    *slot = ty;
                                    filled += 1;
                                }
                                Err(why) => dlg.notes.push(format!("{name}: {why}")),
                            }
                        }
                        dlg.notes.insert(0, format!("✔ The compiler filled in {filled} type{}.", if filled == 1 { "" } else { "s" }));
                    }
                    _ => {}
                }
            }
        }
        let mut ask = false;
        let Some(dlg) = &mut self.extract else { return };
        let existing = extract::function_names(&self.program);
        let mut close = false;
        let mut run = false;
        egui::Modal::new(egui::Id::unique("extract-fn")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("ƒ Extract function");
            ui.add_space(4.0);
            match &mut dlg.plan {
                Err(e) => {
                    ui.label("This group can't be turned into a function:");
                    ui.colored_label(egui::Color32::from_rgb(230, 120, 100), e.as_str());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                }
                Ok(plan) => {
                    ui.weak("Values flowing into the group become parameters; values used after it are returned.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut plan.name).code_editor().desired_width(240.0));
                    });
                    ui.add_space(6.0);
                    ui.strong(if plan.params.is_empty() { "No parameters" } else { "Parameters" });
                    if !plan.params.is_empty() {
                        egui::Grid::new("params").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
                            for (i, p) in plan.params.iter_mut().enumerate() {
                                ui.add_sized([130.0, 20.0], egui::TextEdit::singleline(&mut p.name).code_editor());
                                egui::ComboBox::from_id_salt(("mode", i)).selected_text(p.mode.label()).width(70.0).show_ui(ui, |ui| {
                                    for m in [extract::Mode::Value, extract::Mode::Ref, extract::Mode::Mut] {
                                        ui.selectable_value(&mut p.mode, m, m.label());
                                    }
                                });
                                ui.add(egui::TextEdit::singleline(&mut p.ty).code_editor().desired_width(220.0).hint_text("type, e.g. i32"));
                                ui.end_row();
                            }
                        });
                    }
                    ui.add_space(6.0);
                    ui.strong(if plan.outputs.is_empty() { "Returns nothing" } else { "Returns" });
                    if !plan.outputs.is_empty() {
                        egui::Grid::new("outs").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                            for o in plan.outputs.iter_mut() {
                                ui.add_sized([130.0, 20.0], egui::TextEdit::singleline(&mut o.name).code_editor());
                                ui.add(egui::TextEdit::singleline(&mut o.ty).code_editor().desired_width(300.0).hint_text("type, e.g. String"));
                                ui.end_row();
                            }
                        });
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let busy = dlg.asking.is_some();
                        if ui
                            .add_enabled(!busy, egui::Button::new("Ask the compiler for types"))
                            .on_hover_text("Runs cargo check on a scratch copy of your project (your files aren't touched) and reads the real types from its errors.")
                            .clicked()
                        {
                            ask = true;
                        }
                        if busy {
                            ui.spinner();
                            ui.weak("running cargo check…");
                        }
                    });
                    for n in &dlg.notes {
                        let color = if n.starts_with('✔') { egui::Color32::from_rgb(120, 200, 140) } else { egui::Color32::from_rgb(230, 160, 90) };
                        ui.colored_label(color, n);
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(plan.signature()).monospace().strong());
                    let problems = plan.problems(&existing);
                    for p in &problems {
                        ui.colored_label(egui::Color32::from_rgb(230, 160, 90), format!("• {p}"));
                    }
                    ui.weak("Types are guessed where possible; the compiler gives the real ones.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(problems.is_empty(), egui::Button::new("Extract")).clicked() {
                            run = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if ask {
            self.ask_compiler(ctx);
        }
        if run {
            let dlg = self.extract.take().unwrap();
            if let Ok(plan) = dlg.plan {
                match extract::apply(&mut self.program, &dlg.fn_path, &plan) {
                    Ok(_) => {
                        self.status = format!("Extracted `{}`", plan.name);
                        self.editors.clear();
                        self.mark_changed();
                    }
                    Err(e) => self.status = format!("Extract failed: {e}"),
                }
            }
        } else if close {
            self.extract = None;
        }
    }
}

impl App {
    /// Index of the current function in `Program::functions()` (what span markers use).
    fn current_fi(&self) -> Option<usize> {
        self.program.functions().iter().position(|(p, _)| *p == self.current)
    }

    /// Code ranges to paint for the current selection, each in its node's colour.
    fn selection_marks(&self) -> Vec<(std::ops::Range<usize>, egui::Color32)> {
        let mut marks = vec![];
        if self.show_file {
            if let Some(card) = self.file_view.selected.and_then(|s| self.file_view.cards.get(s)) {
                if let Some(r) = self.item_ranges.get(&card.idx) {
                    let c = card_color(card.kind);
                    marks.push((r.clone(), egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 150)));
                }
            }
            return marks;
        }
        let (Some(fi), Some(ed)) = (self.current_fi(), self.editors.get(&self.current)) else { return marks };
        let Some((_, f)) = self.program.functions().into_iter().find(|(p, _)| *p == self.current) else { return marks };
        let code = &self.code.code;
        let span_range = |id: NodeId| self.code.spans.get(&(fi, id)).map(|&(a, b)| highlight::line_range(code, a, b));
        let tint = |c: egui::Color32| egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 150);
        let mut ids: Vec<NodeId> = ed.multi.iter().copied().collect();
        if ids.is_empty() {
            ids.extend(ed.selected);
        }
        for id in ids {
            let Some(node) = f.nodes.get(&id) else { continue };
            let color = tint(crate::editor::header_color(node.kind.category()));
            if f.is_group(id) {
                for m in extract::descendants(f, id) {
                    if let Some(r) = span_range(m) {
                        marks.push((r, color));
                    }
                }
            } else if let Some(r) = span_range(id) {
                marks.push((r, color));
            } else if !node.kind.is_exec() {
                // a value node: find its text inside the statement that uses it
                if let Some(stmt) = owning_statement(f, id) {
                    if let Some(within) = span_range(stmt) {
                        let text = codegen::expr_text(f, id, 0);
                        let r = highlight::find_loose(code, within.clone(), &text).unwrap_or(within);
                        marks.push((r, color));
                    }
                }
            }
        }
        marks
    }

    /// The generated code: syntax highlighted, the selection painted in its node's colour,
    /// scrolled into view when the selection changes; click a line to select its node.
    fn code_view(&mut self, ui: &mut egui::Ui) {
        let marks = self.selection_marks();
        let mut sel = self.editors.get(&self.current).map(|e| e.multi.clone()).unwrap_or_default();
        if self.show_file {
            sel = self.file_view.selected.map(|s| std::collections::BTreeSet::from([u32::MAX - s as u32])).unwrap_or_default();
        }
        let key = (self.current.clone(), sel, self.code.code.len());
        let scroll_to_selection = self.last_marked.as_ref() != Some(&key) && !marks.is_empty();
        self.last_marked = Some(key);
        let code = self.code.code.clone();
        let job = highlight::layout(&code, egui::FontId::monospace(12.5), &marks);
        let mut clicked_line = None;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let galley = ui.painter().layout_job(job);
            let (rect, resp) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
            ui.painter().galley(rect.min, galley.clone(), egui::Color32::WHITE);
            if scroll_to_selection {
                if let Some((r, _)) = marks.first() {
                    let chars = code[..r.start].chars().count();
                    let at = galley.pos_from_cursor(egui::text::CCursor::new(chars)).translate(rect.min.to_vec2());
                    ui.scroll_to_rect(at.expand(30.0), Some(egui::Align::Center));
                }
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let c = galley.cursor_from_pos(pos - rect.min);
                    let byte = code.char_indices().nth(c.index.0).map(|(b, _)| b).unwrap_or(code.len());
                    clicked_line = Some(code[..byte].matches('\n').count());
                }
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click a line to select its node");
        });
        if let Some(line) = clicked_line {
            self.select_from_code(line);
        }
    }

    /// Select the node whose code contains `line` (the innermost one), switching function if needed.
    fn select_from_code(&mut self, line: usize) {
        let best = self
            .code
            .spans
            .iter()
            .filter(|(_, ab)| ab.0 <= line && line <= ab.1)
            .min_by_key(|(_, ab)| ab.1 - ab.0)
            .map(|(&(fi, id), _)| (fi, id));
        let Some((fi, id)) = best else { return };
        let fns = self.program.functions();
        let Some((path, f)) = fns.get(fi) else { return };
        let path = path.clone();
        let f = (*f).clone();
        if path != self.current {
            self.current = path.clone();
            self.load_sig();
        }
        self.editors.entry(path).or_default().reveal(&f, id);
    }
}

fn card_color(k: fileview::CardKind) -> egui::Color32 {
    use fileview::CardKind::*;
    match k {
        Use => egui::Color32::from_rgb(70, 95, 140),
        Const | Static => egui::Color32::from_rgb(150, 110, 30),
        Mod => egui::Color32::from_rgb(30, 120, 130),
        Struct | Enum | TypeAlias | Trait => egui::Color32::from_rgb(40, 130, 90),
        Func => egui::Color32::from_rgb(40, 90, 170),
        Impl | Macro => egui::Color32::from_rgb(110, 60, 130),
        _ => egui::Color32::from_rgb(90, 90, 90),
    }
}

/// The statement (exec node) a value node ends up in, following its data wires.
fn owning_statement(f: &FnGraph, id: NodeId) -> Option<NodeId> {
    let mut seen = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([id]);
    while let Some(n) = queue.pop_front() {
        if !seen.insert(n) {
            continue;
        }
        for e in f.data.iter().filter(|e| e.from == n) {
            if f.nodes.get(&e.to).is_some_and(|x| x.kind.is_exec()) {
                return Some(e.to);
            }
            queue.push_back(e.to);
        }
    }
    None
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("rust-nodes");
                ui.separator();
                ui.label("File:");
                ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(320.0).code_editor());
                if ui.button("Open").clicked() {
                    if let Err(e) = self.open_or_create(false) {
                        self.status = e;
                    }
                }
                if ui.button("Browse…").on_hover_text("Open a .rs file").clicked() {
                    if let Err(e) = self.browse_open() {
                        self.status = e;
                    }
                }
                if ui.button("✚ New…").on_hover_text("Create a new .rs file").clicked() {
                    if let Err(e) = self.browse_new() {
                        self.status = e;
                    }
                }
                let has = self.has_file;
                if ui.add_enabled(has, egui::Button::new(if self.unsaved { "Save*" } else { "Save" })).clicked() {
                    self.save();
                }
                ui.separator();
                let busy = self.running.load(Ordering::SeqCst);
                if ui.add_enabled(has && !busy, egui::Button::new("Check")).on_hover_text("Save, then cargo check").clicked() {
                    self.cargo("check", &ctx);
                }
                if ui.add_enabled(has && !busy, egui::Button::new("▶ Run")).on_hover_text("Save, then cargo run").clicked() {
                    self.cargo("run", &ctx);
                }
                if busy {
                    ui.spinner();
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::Panel::bottom("output").resizable(true).default_size(160.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Output");
                if ui.small_button("clear").clicked() {
                    self.output.lock().unwrap().clear();
                }
            });
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                let text = self.output.lock().unwrap().clone();
                ui.add(egui::Label::new(egui::RichText::new(text).monospace()).wrap_mode(egui::TextWrapMode::Extend));
            });
        });

        if self.show_prompt {
            self.prompt(&ctx);
        }

        if std::mem::take(&mut self.pending_ask) {
            self.ask_compiler(&ctx);
        }
        self.extract_dialog(&ctx);

        if !self.has_file {
            // Blank application: nothing open yet.
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.heading("No file open");
                    ui.add_space(6.0);
                    ui.weak("Add a Rust source file to start editing it as nodes.");
                    ui.add_space(10.0);
                    if ui.button("Add a .rs file…").clicked() {
                        self.show_prompt = true;
                    }
                });
            });
            return;
        }

        egui::Panel::left("functions").resizable(true).default_size(240.0).show(ui, |ui| {
            if ui
                .selectable_label(self.show_file, egui::RichText::new("▣ File overview").strong())
                .on_hover_text("Every item in the file as a card: use, const, mod, structs, enums, functions...")
                .clicked()
            {
                self.show_file = true;
                self.file_view.needs_fit = true;
            }
            ui.add_space(4.0);
            ui.heading("Functions");
            let fns: Vec<(Vec<usize>, String)> = self.program.functions().into_iter().map(|(p, f)| (p, f.name.clone())).collect();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for (p, name) in fns {
                    if ui.selectable_label(!self.show_file && p == self.current, egui::RichText::new(name).monospace()).clicked() {
                        self.current = p;
                        self.show_file = false;
                        self.load_sig();
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("+ New function").clicked() {
                    self.add_function();
                }
                if !self.current.is_empty() && ui.button("Delete").clicked() {
                    self.delete_function();
                }
            });
            ui.separator();
            ui.strong("Signature");
            let r = ui.add(egui::TextEdit::multiline(&mut self.sig_text).code_editor().desired_rows(2).desired_width(f32::INFINITY));
            if r.changed() {
                match parse_signature(&self.sig_text) {
                    Ok(params) => {
                        let sig = self.sig_text.clone();
                        if let Some(f) = self.program.function_mut(&self.current.clone()) {
                            f.sig = sig.trim().to_string();
                            f.params = params;
                            if let Ok(item) = syn::parse_str::<syn::ItemFn>(&format!("{} {{}}", f.sig)) {
                                let base = f.name.rsplit_once("::").map(|(t, _)| format!("{t}::")).unwrap_or_default();
                                f.name = format!("{base}{}", item.sig.ident);
                            }
                            f.prune_edges();
                        }
                        self.sig_error = None;
                        self.mark_changed();
                    }
                    Err(e) => self.sig_error = Some(e),
                }
            }
            if let Some(e) = &self.sig_error {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 100), e);
            }
            ui.weak("Parameters show up as outputs on the Entry node.");
            ui.separator();
            let others = self.program.items.iter().filter(|i| matches!(i, Item::Raw(_) | Item::Comment(_) | Item::Header(_))).count();
            if ui.link(format!("{others} other item(s): use, const, mod, structs... → File overview")).clicked() {
                self.show_file = true;
                self.file_view.needs_fit = true;
            }
        });

        self.refresh_views();
        egui::Panel::right("side").resizable(true).default_size(380.0).show(ui, |ui| {
            ui.heading("Inspector");
            let path = self.current.clone();
            let mut changed = false;
            if self.show_file {
                let name = self.current_name();
                match self.file_view.inspector(ui, &mut self.program) {
                    fileview::ItemAction::None => {}
                    fileview::ItemAction::Changed => self.mark_changed(),
                    action => {
                        let sel_idx = self.file_view.selected.and_then(|s| self.file_view.cards.get(s)).map(|c| c.idx);
                        if let Some(i) = sel_idx {
                            let new_idx = match action {
                                fileview::ItemAction::MoveUp if i > 0 => {
                                    self.program.items.swap(i, i - 1);
                                    Some(i - 1)
                                }
                                fileview::ItemAction::MoveDown if i + 1 < self.program.items.len() => {
                                    self.program.items.swap(i, i + 1);
                                    Some(i + 1)
                                }
                                fileview::ItemAction::Delete => {
                                    self.program.items.remove(i);
                                    None
                                }
                                _ => Some(i),
                            };
                            self.items_reshaped(name);
                            self.refresh_views();
                            self.file_view.selected = new_idx.and_then(|n| self.file_view.cards.iter().position(|c| c.idx == n));
                        }
                    }
                }
                self.handle_open();
            } else if let Some(f) = self.program.function_mut(&path) {
                let ed = self.editors.entry(path.clone()).or_default();
                changed = ed.inspector(ui, f);
            }
            if changed {
                self.mark_changed();
            }
            self.take_extract_request(&path);
            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Generated Rust");
                if ui.small_button("copy").clicked() {
                    ui.ctx().copy_text(self.code.code.clone());
                }
            });
            if let Some(e) = &self.code.error {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 100), format!("Syntax error: {e}"));
            }
            self.code_view(ui);
        });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            if self.show_file {
                self.refresh_views();
                if let Some(t) = self.file_view.show(ui) {
                    self.add_item(t);
                }
                self.handle_open();
                return;
            }
            let path = self.current.clone();
            let mut changed = false;
            if let Some(f) = self.program.function_mut(&path) {
                let salt = format!("{path:?}");
                let ed = self.editors.entry(path.clone()).or_default();
                changed = ed.show(ui, f, &salt);
            } else {
                ui.centered_and_justified(|ui| ui.label("No function selected. Add one on the left."));
            }
            if changed {
                self.mark_changed();
            }
            self.take_extract_request(&path);
        });
    }
}
