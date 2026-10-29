//! Loading and saving. The `.rs` file is always the source of truth.
//! Next to it we keep `<file>.nodes.json` holding node positions (and any
//! unconnected nodes still being worked on), plus a hash of the `.rs` text it matches.

use crate::codegen::{generate, Generated};
use crate::graph::*;
use crate::layout::node_paths;
use crate::parse::parse_program;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct Sidecar {
    source_hash: String,
    program: Program,
}

pub fn sidecar_path(rs: &Path) -> PathBuf {
    let mut s = rs.as_os_str().to_owned();
    s.push(".nodes.json");
    PathBuf::from(s)
}

/// FNV-1a, so the hash is stable across Rust versions and platforms.
pub fn hash_text(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub struct Loaded {
    pub program: Program,
    pub note: String,
}

pub fn open(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let side: Option<Sidecar> =
        std::fs::read_to_string(sidecar_path(path)).ok().and_then(|s| serde_json::from_str(&s).ok());

    if let Some(side) = &side {
        if side.source_hash == hash_text(&text) {
            return Ok(Loaded { program: side.program.clone(), note: "Opened (layout restored)".into() });
        }
    }
    let mut program = parse_program(&text)?;
    let note = if let Some(side) = side {
        carry_positions(&side.program, &mut program);
        "Opened. The .rs file changed outside the editor, so it was re-read; saved positions were kept where possible".into()
    } else {
        "Opened (auto layout)".into()
    };
    Ok(Loaded { program, note })
}

/// Write the generated Rust and the sidecar. Returns the generated code.
pub fn save(path: &Path, program: &Program) -> Result<Generated, String> {
    let generated = generate(program);
    std::fs::write(path, &generated.code).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    let side = Sidecar { source_hash: hash_text(&generated.code), program: program.clone() };
    let json = serde_json::to_string_pretty(&side).map_err(|e| e.to_string())?;
    std::fs::write(sidecar_path(path), json).map_err(|e| e.to_string())?;
    Ok(generated)
}

fn carry_positions(old: &Program, new: &mut Program) {
    let old_fns: HashMap<String, &FnGraph> = old.functions().into_iter().map(|(_, f)| (f.name.clone(), f)).collect();
    let paths: Vec<Vec<usize>> = new.functions().into_iter().map(|(p, _)| p).collect();
    for p in paths {
        let f = new.function_mut(&p).unwrap();
        let Some(of) = old_fns.get(&f.name) else { continue };
        let old_by_path: HashMap<String, [f32; 2]> =
            node_paths(of).into_iter().map(|(id, path)| (path, of.nodes[&id].pos)).collect();
        for (id, path) in node_paths(f) {
            if let Some(pos) = old_by_path.get(&path) {
                f.nodes.get_mut(&id).unwrap().pos = *pos;
            }
        }
    }
}

/// Find the Cargo project folder containing `file` (walks up to the nearest Cargo.toml).
pub fn cargo_root(file: &Path) -> Option<PathBuf> {
    let abs = std::fs::canonicalize(file).ok()?;
    let mut dir = abs.parent();
    while let Some(d) = dir {
        if d.join("Cargo.toml").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}
