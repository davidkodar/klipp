//! Every file that compiles a shader module has its shaders validated
//! without a GPU (#2344). A WGSL error otherwise shows only on a machine
//! with an adapter, as a module left out, and the GPU tests that take a
//! missing pipeline for no GPU skip and pass. So each file that calls
//! `create_shader_module` is listed here with the test that parses and
//! validates its shaders with naga; a new one fails until it is.

/// The files that compile shader modules, and where their WGSL is
/// validated.
const VALIDATED: &[(&str, &str)] = &[
    ("klipp-effects/src/gpu.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/gpu/mask_mix.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/gpu/layer.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/gpu/thumbs.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/gpu/flow.rs", "gpu::shader_validation_tests::every_shader_compiles (flow::sources)"),
    ("klipp-effects/src/gpu/planar.rs", "gpu::shader_validation_tests::every_shader_compiles (planar_wgsl::sources)"),
    ("klipp-effects/src/nv12_gpu.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/p010_gpu.rs", "gpu::shader_validation_tests::every_shader_compiles (shader_sources)"),
    ("klipp-effects/src/gpu_scopes.rs", "gpu_scopes::tests::every_scope_shader_validates"),
    ("klipp-compose/src/title3d_gpu.rs", "title3d_gpu::tests::the_shaders_compile"),
    ("klipp-ui/src/viewer_lut.rs", "viewer_lut::tests::the_shader_compiles"),
    ("klipp-hdr-out/src/main.rs", "shader_tests::the_shader_compiles"),
];

fn crates_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn every_file_that_compiles_shaders_validates_them() {
    let root = crates_dir();
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&root).unwrap().flatten() {
        // Sources only: a test that builds its own module checks itself.
        walk(&krate.path().join("src"), &mut files);
    }
    let mut compiling: Vec<String> = files
        .iter()
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|s| s.contains("create_shader_module(")))
        .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    compiling.sort();
    let listed: Vec<&str> = VALIDATED.iter().map(|(f, _)| *f).collect();
    let missing: Vec<_> = compiling.iter().filter(|f| !listed.contains(&f.as_str())).collect();
    let stale: Vec<_> = listed.iter().filter(|f| !compiling.iter().any(|c| c == *f)).collect();
    assert!(missing.is_empty(), "files compiling shader modules with no WGSL validation listed: {missing:?}");
    assert!(stale.is_empty(), "listed files that compile no shader module: {stale:?}");
}
