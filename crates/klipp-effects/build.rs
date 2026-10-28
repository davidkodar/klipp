//! Assembles the GPU shaders under `src/gpu/shaders/` into `OUT_DIR`
//! (#2890), where `gpu_shader!` takes them in at compile time. The rules
//! are in `build/shader_includes.rs`. Files are read in sorted order and
//! written only when their text changed, so the output is the same on
//! every build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[path = "build/shader_includes.rs"]
mod shader_includes;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/gpu/shaders");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("shaders");
    println!("cargo:rerun-if-changed=build/shader_includes.rs");

    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();

    let mut includes = BTreeMap::new();
    let mut shaders = Vec::new();
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        if path.extension().is_none_or(|e| e != "wgsl") {
            continue;
        }
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{rel}: {e}"));
        match rel.strip_prefix("include/") {
            Some(name) if !name.contains('/') => {
                if let Err(e) = shader_includes::check_include(&rel, &text) {
                    panic!("{e}");
                }
                includes.insert(name.trim_end_matches(".wgsl").to_string(), text);
            }
            Some(_) => panic!("{rel}: shared pieces sit directly in include/"),
            None => shaders.push((rel, text)),
        }
    }

    let mut index = String::new();
    for (rel, text) in &shaders {
        let assembled = shader_includes::assemble(rel, text, &includes).unwrap_or_else(|e| panic!("{e}"));
        write_if_changed(&out.join(rel), &assembled);
        index.push_str(rel);
        index.push('\n');
    }
    write_if_changed(&out.join("INDEX"), &index);
}

/// Every file and folder under `dir`; each folder is watched too, so a
/// shader added or removed reruns the build.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn write_if_changed(path: &Path, text: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}
