//! Running the Rust compiler on a scratch copy of the user's code, to learn types.
//!
//! The user's files are never touched: a cargo project is copied to a temp folder
//! (without `target/` and `.git/`) and checked there, reusing the project's own
//! `target/` folder so dependencies don't rebuild. A file that isn't in a cargo
//! project is checked with plain `rustc`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// One "mismatched types" error: the line it points at and its label,
/// e.g. (12, "expected `()`, found `String`").
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub line: usize,
    pub label: String,
    /// error code, e.g. "E0308"; other errors on a probe line explain why it has no type
    pub code: String,
}

/// Check `code` as if it were the contents of `file`, and return all E0308 errors.
pub fn check_mismatches(file: &Path, code: &str) -> Result<Vec<Mismatch>, String> {
    let scratch = std::env::temp_dir().join(format!("rust-nodes-probe-{}-{}", std::process::id(), unique()));
    let result = match crate::project::cargo_root(file) {
        Some(root) => check_with_cargo(&root, file, code, &scratch),
        None => check_with_rustc(code, &scratch),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn unique() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn check_with_cargo(root: &Path, file: &Path, code: &str, scratch: &Path) -> Result<Vec<Mismatch>, String> {
    let file = std::fs::canonicalize(file).map_err(|e| e.to_string())?;
    let rel = file.strip_prefix(root).map_err(|_| "file isn't inside its cargo project".to_string())?;
    copy_dir(root, scratch).map_err(|e| format!("couldn't copy the project: {e}"))?;
    std::fs::write(scratch.join(rel), code).map_err(|e| e.to_string())?;
    let out = Command::new("cargo")
        .args(["check", "--message-format=json", "--quiet"])
        .current_dir(scratch)
        // reuse already-built dependencies
        .env("CARGO_TARGET_DIR", root.join("target").join("rust-nodes-probe"))
        .output()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut found = vec![];
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] == "compiler-message" {
            collect(&v["message"], rel, &mut found);
        }
    }
    if !found.iter().any(|m| m.code == "E0308") && !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().filter(|l| !l.trim().is_empty()).take(4).collect::<Vec<_>>().join(" ");
        return Err(format!("cargo check failed before reaching the code: {first}"));
    }
    Ok(found)
}

fn check_with_rustc(code: &str, scratch: &Path) -> Result<Vec<Mismatch>, String> {
    std::fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    let src = scratch.join("main.rs");
    std::fs::write(&src, code).map_err(|e| e.to_string())?;
    let out = Command::new("rustc")
        .args(["--edition=2021", "--error-format=json", "--emit=metadata", "--crate-type=bin", "-o"])
        .arg(scratch.join("out.rmeta"))
        .arg(&src)
        .output()
        .map_err(|e| format!("couldn't run rustc: {e}"))?;
    let mut found = vec![];
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            collect(&v, Path::new("main.rs"), &mut found);
        }
    }
    Ok(found)
}

/// Pull the primary spans of errors in `file` out of one diagnostic.
fn collect(msg: &serde_json::Value, file: &Path, out: &mut Vec<Mismatch>) {
    if msg["level"] != "error" {
        return;
    }
    let code = msg["code"]["code"].as_str().unwrap_or("").to_string();
    let Some(spans) = msg["spans"].as_array() else { return };
    for sp in spans {
        let same_file = sp["file_name"].as_str().is_some_and(|n| Path::new(n).ends_with(file) || file.ends_with(n));
        if sp["is_primary"] == true && same_file {
            if let Some(line) = sp["line_start"].as_u64() {
                let label = if code == "E0308" {
                    sp["label"].as_str().unwrap_or("").to_string()
                } else {
                    msg["message"].as_str().unwrap_or("").to_string()
                };
                out.push(Mismatch { line: line as usize, label, code: code.clone() });
            }
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = entry.path();
        let dest: PathBuf = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &dest)?;
        } else if !name.to_string_lossy().ends_with(".nodes.json") {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

/// For each probe variable, the type the compiler found (or why it couldn't).
pub fn probe_types(file: &Path, code: &str, probes: usize) -> Result<Vec<Result<String, String>>, String> {
    let mismatches = check_mismatches(file, code)?;
    if probes > 0 && !mismatches.iter().any(|m| m.code == "E0308") {
        let other = mismatches.first().map(|m| format!(" First error: {}", m.label)).unwrap_or_default();
        return Err(format!("the compiler stopped before checking types; the program probably has another error (press Check).{other}"));
    }
    let lines: Vec<&str> = code.lines().collect();
    Ok((0..probes)
        .map(|i| {
            let var = format!("{}:", crate::extract::probe_var(i));
            let Some(line) = lines.iter().position(|l| l.contains(&var)).map(|n| n + 1) else {
                return Err("probe missing".into());
            };
            if let Some(m) = mismatches.iter().find(|m| m.line == line && m.code != "E0308") {
                return Err(format!("compiler: {}", m.label));
            }
            match mismatches.iter().find(|m| m.line == line && m.code == "E0308") {
                Some(m) => match m.label.split_once("found ") {
                    Some((_, found)) => crate::extract::type_from_found(found),
                    None => Err(format!("compiler said: {}", m.label)),
                },
                // no complaint means the value really is `()`
                None => Ok("()".into()),
            }
        })
        .collect())
}
