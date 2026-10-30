//! The application window: toolbar, function list, canvas, inspector,
//! live generated-code view and cargo output.

use crate::codegen::{generate, Generated};
use crate::editor::Editor;
use crate::graph::*;
use crate::parse::parse_signature;
use crate::extract;
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
            ui.heading("Functions");
            let fns: Vec<(Vec<usize>, String)> = self.program.functions().into_iter().map(|(p, f)| (p, f.name.clone())).collect();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for (p, name) in fns {
                    if ui.selectable_label(p == self.current, egui::RichText::new(name).monospace()).clicked() {
                        self.current = p;
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
            let others = self.program.items.iter().filter(|i| matches!(i, Item::Raw(_) | Item::Comment(_))).count();
            ui.weak(format!("{others} other item(s) (structs, enums, use, ...) are kept as code."));
        });

        egui::Panel::right("side").resizable(true).default_size(380.0).show(ui, |ui| {
            ui.heading("Inspector");
            let path = self.current.clone();
            let mut changed = false;
            if let Some(f) = self.program.function_mut(&path) {
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
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&self.code.code).monospace()).wrap_mode(egui::TextWrapMode::Extend));
            });
        });

        egui::CentralPanel::no_frame().show(ui, |ui| {
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
