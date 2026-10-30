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
        let old_paths = node_paths(of);
        let old_by_path: HashMap<String, [f32; 2]> =
            old_paths.iter().map(|(id, path)| (path.clone(), of.nodes[id].pos)).collect();
        let new_paths = node_paths(f);
        for (id, path) in &new_paths {
            if let Some(pos) = old_by_path.get(path) {
                f.nodes.get_mut(id).unwrap().pos = *pos;
            }
        }
        let new_by_path: HashMap<String, NodeId> = new_paths.into_iter().map(|(id, p)| (p, id)).collect();
        carry_groups(of, f, &old_paths, &new_by_path);
    }
}

/// Groups exist only in the layout file, so re-create them on the re-read graph,
/// matching their members by structural path.
pub fn carry_groups(of: &FnGraph, f: &mut FnGraph, old_paths: &HashMap<NodeId, String>, new_by_path: &HashMap<String, NodeId>) {
    let old_groups: Vec<NodeId> = of.nodes.keys().copied().filter(|id| of.is_group(*id)).collect();
    if old_groups.is_empty() {
        return;
    }
    let mut map: HashMap<NodeId, NodeId> = HashMap::new();
    for g in &old_groups {
        let n = &of.nodes[g];
        let new = f.add_in(n.kind.clone(), n.pos, None);
        map.insert(*g, new);
    }
    // the old node a scope refers to, translated into the new graph
    let map_scope = |s: Scope| -> Option<Scope> {
        match s {
            None => Some(None),
            Some((owner, h)) => {
                if let Some(g) = map.get(&owner) {
                    Some(Some((*g, 0)))
                } else {
                    let id = new_by_path.get(old_paths.get(&owner)?)?;
                    Some(Some((*id, h)))
                }
            }
        }
    };
    for g in &old_groups {
        let scope = map_scope(of.nodes[g].scope).unwrap_or(None);
        f.nodes.get_mut(&map[g]).unwrap().scope = scope;
    }
    // the real (non-group) container a scope sits in
    let structural = |f: &FnGraph, mut s: Scope| -> Scope {
        for _ in 0..64 {
            match s {
                Some((owner, _)) if f.is_group(owner) => s = f.nodes[&owner].scope,
                _ => break,
            }
        }
        s
    };
    for (old_id, path) in old_paths {
        let Some(Some((owner, _))) = of.nodes.get(old_id).map(|n| n.scope) else { continue };
        let Some(new_g) = map.get(&owner).copied() else { continue };
        let Some(new_id) = new_by_path.get(path).copied() else { continue };
        let target = Some((new_g, 0));
        // only move it if it's in the same real container the group is in
        if f.nodes[&new_id].scope == structural(f, target) {
            f.nodes.get_mut(&new_id).unwrap().scope = target;
        }
    }
    // drop groups that ended up empty (innermost first)
    loop {
        let empty: Vec<NodeId> = map.values().copied().filter(|g| f.nodes.contains_key(g) && f.members(*g).is_empty()).collect();
        if empty.is_empty() {
            break;
        }
        for g in empty {
            f.nodes.remove(&g);
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
