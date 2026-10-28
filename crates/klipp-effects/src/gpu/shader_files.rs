//! The shaders' files (#2890): each shader's text is the same, byte for
//! byte, as the list taken before they moved out of the Rust source, every
//! file under `shaders/` is a shader in use or a piece one takes in, and the
//! build script's include rules hold.

use std::collections::BTreeMap;

#[path = "../../build/shader_includes.rs"]
mod shader_includes;

/// The list taken before the move: each shader's name, length and SHA-256.
const SNAPSHOT: &str = include_str!("shader_snapshot.tsv");

/// The shaders' folder in the source tree.
fn shaders_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gpu/shaders")
}

/// Every shader the crate compiles, by name, as `every_shader_compiles`
/// gathers them.
fn all_sources() -> Vec<(String, String)> {
    let flow = super::flow::sources();
    let planar = super::planar_wgsl::sources();
    super::shader_sources()
        .into_iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .chain(flow.iter().map(|(n, s)| (n.to_string(), s.clone())))
        .chain(planar.iter().map(|(n, s)| (n.to_string(), s.clone())))
        .collect()
}

fn sha256(s: &str) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(s.as_bytes()))
}

/// Every shader is byte for byte what it was before the move, and none is
/// missing or new without the list saying so.
#[test]
fn every_shader_is_as_it_was_before_the_move() {
    let was: BTreeMap<&str, (usize, &str)> = SNAPSHOT
        .lines()
        .map(|l| {
            let mut f = l.split('\t');
            let (name, len, sum) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
            (name, (len.parse().unwrap(), sum))
        })
        .collect();
    let now = all_sources();
    let mut failures = Vec::new();
    for (name, source) in &now {
        match was.get(name.as_str()) {
            None => failures.push(format!("{name}: not in the snapshot")),
            Some((len, sum)) if *len != source.len() || *sum != sha256(source) => failures.push(format!("{name}: {} bytes now, {len} before, or its checksum changed", source.len())),
            Some(_) => {}
        }
    }
    for name in was.keys() {
        if !now.iter().any(|(n, _)| n == name) {
            failures.push(format!("{name}: in the snapshot, gone now"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every shader file is a shader the crate compiles, as assembled, and
/// every shared piece is taken in by some shader file or helper macro:
/// nothing under `shaders/` is left over.
#[test]
fn every_shader_file_is_used() {
    let out = std::path::Path::new(env!("OUT_DIR")).join("shaders");
    let index = std::fs::read_to_string(out.join("INDEX")).unwrap();
    let sources: Vec<String> = all_sources().into_iter().map(|(_, s)| s).collect();
    let mut taken = std::collections::BTreeSet::new();
    for rel in index.lines() {
        let assembled = std::fs::read_to_string(out.join(rel)).unwrap();
        assert!(sources.contains(&assembled), "{rel} is no shader the crate compiles");
        let raw = std::fs::read_to_string(shaders_dir().join(rel)).unwrap();
        taken.extend(raw.split('\n').filter_map(|l| l.strip_prefix(shader_includes::DIRECTIVE)).map(str::to_string));
    }
    let rust = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gpu.rs")).unwrap();
    for e in std::fs::read_dir(shaders_dir().join("include")).unwrap().flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let name = file.trim_end_matches(".wgsl");
        assert!(taken.contains(name) || rust.contains(&format!("/shaders/include/{file}")), "include/{file} is taken in by nothing");
    }
}

/// The include rules: a whole line `//#include name` is replaced, with its
/// newline, by the piece as it is; an unknown name is an error naming the
/// file and line; a piece cannot take in another; nothing else is read.
#[test]
fn the_include_rules() {
    let pieces: BTreeMap<String, String> = [("a".to_string(), "fn a() {}\n".to_string())].into();
    let assemble = |s: &str| shader_includes::assemble("x.wgsl", s, &pieces);
    assert_eq!(assemble("head\n//#include a\ntail").unwrap(), "head\nfn a() {}\ntail");
    assert_eq!(assemble("//#include a").unwrap(), "fn a() {}\n", "at the end, with no newline after");
    assert_eq!(assemble("//#include a\n//#include a\n").unwrap(), "fn a() {}\nfn a() {}\n");
    for not_a_directive in ["  //#include a\n", "// #include a\n", "x //#include a\n", "//#include  a\n"] {
        let got = assemble(not_a_directive);
        assert!(got.as_deref() == Ok(not_a_directive) || got.as_ref().is_err_and(|e| e.contains("x.wgsl:1")), "{not_a_directive:?}: {got:?}");
    }
    assert_eq!(assemble("ok\n//#include nope\n").unwrap_err(), "x.wgsl:2: no shared piece `nope` in src/gpu/shaders/include");
    assert_eq!(shader_includes::check_include("include/b.wgsl", "x\n//#include a\n").unwrap_err(), "include/b.wgsl:2: a shared piece cannot include another");
    assert!(shader_includes::check_include("include/b.wgsl", "fn b() {}\n").is_ok());
}
