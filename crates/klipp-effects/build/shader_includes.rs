//! Assembling the GPU shaders from their files (#2890), shared by the
//! build script and its tests.
//!
//! A shader is one `.wgsl` file under `src/gpu/shaders/`; the pieces
//! several shaders share are files under `src/gpu/shaders/include/`. A
//! shader takes one in with a line that is exactly `//#include <name>`:
//! that line, with its newline, is replaced by `include/<name>.wgsl` as
//! it is. Nothing else is read as a directive, and a shared piece cannot
//! take in another (one level), so there are no cycles.

use std::collections::BTreeMap;

/// The directive, at the start of a line.
pub const DIRECTIVE: &str = "//#include ";

/// `source` (the shader file `file`) with each include line replaced by
/// its piece from `includes` (name → text): the shader as it compiles.
/// An unknown name is an error naming the file and line.
pub fn assemble(file: &str, source: &str, includes: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(source.len());
    for (n, line) in source.split_inclusive('\n').enumerate() {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        match bare.strip_prefix(DIRECTIVE) {
            Some(name) => {
                let piece = includes.get(name).ok_or_else(|| format!("{file}:{}: no shared piece `{name}` in src/gpu/shaders/include", n + 1))?;
                out.push_str(piece);
            }
            None => out.push_str(line),
        }
    }
    Ok(out)
}

/// A shared piece's text, refused when it takes in another: includes are
/// one level only.
pub fn check_include(file: &str, source: &str) -> Result<(), String> {
    match source.split('\n').position(|l| l.starts_with(DIRECTIVE)) {
        Some(n) => Err(format!("{file}:{}: a shared piece cannot include another", n + 1)),
        None => Ok(()),
    }
}
