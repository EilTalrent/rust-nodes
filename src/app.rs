//! The application window: toolbar, function list, canvas, inspector,
//! live generated-code view and cargo output.

use crate::codegen::{generate, Generated};
use crate::editor::Editor;
use crate::graph::*;
use crate::parse::{parse_program, parse_signature};
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
}

impl App {
    fn new(file: Option<PathBuf>) -> Self {
        let program = parse_program(STARTER).expect("starter parses");
        let code = generate(&program);
        let mut app = App {
            path_text: "src/main.rs".into(),
            path: None,
            program,
            current: vec![],
            editors: HashMap::new(),
            code,
            unsaved: false,
            status: "New file. Type a path and press Open, or edit and Save.".into(),
            sig_text: String::new(),
            sig_error: None,
            output: Arc::new(Mutex::new(String::new())),
            running: Arc::new(AtomicBool::new(false)),
        };
        if let Some(f) = file {
            app.path_text = f.display().to_string();
            app.open();
        }
        app.select_first();
        app
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

    fn mark_changed(&mut self) {
        self.unsaved = true;
        self.code = generate(&self.program);
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
                ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(360.0).code_editor());
                if ui.button("Open").clicked() {
                    self.open();
                }
                if ui.button(if self.unsaved { "Save*" } else { "Save" }).clicked() {
                    self.save();
                }
                ui.separator();
                let busy = self.running.load(Ordering::SeqCst);
                if ui.add_enabled(!busy, egui::Button::new("Check")).on_hover_text("Save, then cargo check").clicked() {
                    self.cargo("check", &ctx);
                }
                if ui.add_enabled(!busy, egui::Button::new("▶ Run")).on_hover_text("Save, then cargo run").clicked() {
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
            let others = self.program.items.iter().filter(|i| matches!(i, Item::Raw(_))).count();
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
        });
    }
}
