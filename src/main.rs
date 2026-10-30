mod app;
mod codegen;
mod editor;
mod extract;
mod graph;
mod layout;
mod parse;
mod project;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

const HELP: &str = "rust-nodes: a node-based editor for Rust

USAGE:
    rust-nodes [FILE.rs]               open the editor (optionally on a file)
    rust-nodes --roundtrip FILE.rs     print FILE as graph -> regenerated Rust
    rust-nodes --graph FILE.rs         print the graph as JSON
    rust-nodes --help";

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") => println!("{HELP}"),
        Some("--roundtrip") => {
            let path = args.get(1).ok_or("missing FILE.rs")?;
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let program = parse::parse_program(&text)?;
            let out = codegen::generate(&program);
            print!("{}", out.code);
            if let Some(e) = out.error {
                return Err(e);
            }
        }
        Some("--graph") => {
            let path = args.get(1).ok_or("missing FILE.rs")?;
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let program = parse::parse_program(&text)?;
            println!("{}", serde_json::to_string_pretty(&program).unwrap());
        }
        other => {
            let file = other.map(PathBuf::from);
            app::run(file).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
