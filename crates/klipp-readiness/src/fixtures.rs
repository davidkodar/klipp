//! The native plug-in fixtures the families' cases render (#932's OFX
//! bundle, step 2's frei0r plug-ins), built from their C sources with the
//! flags `scripts/plugin-tests.py` uses, into the build's scratch folder.
//! A missing C compiler fails here, naming what to install: a case never
//! skips for want of one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Where fixtures are built: the target folder, never the source tree or
/// /tmp.
pub fn scratch() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| crates().join("../target"));
    let dir = target.join("readiness-fixtures");
    std::fs::create_dir_all(&dir).expect("the fixture folder");
    dir
}

/// Compile `source` with `defines` into `out`. Cases build in parallel in
/// one process, so each builds under a name of its own and renames it into
/// place: the same bytes, so the last rename wins.
fn compile(source: &Path, defines: &[String], out: &Path) {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    let partial = out.with_extension(format!("{}-{}.part", std::process::id(), BUILDS.fetch_add(1, Ordering::Relaxed)));
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let status = std::process::Command::new(&cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC"])
        .args(defines)
        .arg(source)
        .arg("-o")
        .arg(&partial)
        .status()
        .unwrap_or_else(|e| panic!("C compiler '{cc}' not found ({e}): the plug-in fixtures are C sources; install gcc (base-devel on Arch) or set CC"));
    assert!(status.success(), "{} didn't build", source.display());
    std::fs::rename(&partial, out).expect("the fixture in place");
}

/// The frei0r fixture of `kind` (fixture.c's `KLIPP_KIND`), as
/// `{stem}.so`, alone in a folder of its own. The stem is what the history
/// list keys a plug-in by (`klipp_frei0r::temporal::key`), so a case can
/// build the history fixture as a listed plug-in (`delay0r`) to reach the
/// refusal above 2048.
pub fn frei0r(kind: u32, stem: &str) -> PathBuf {
    let dir = scratch().join(format!("frei0r-{stem}"));
    std::fs::create_dir_all(&dir).expect("the plug-in folder");
    let so = dir.join(format!("{stem}.so"));
    compile(&crates().join("klipp-frei0r/tests/fixtures/frei0r/fixture.c"), &[format!("-DKLIPP_KIND={kind}")], &so);
    so
}

/// The OFX fixture bundle (its four plug-ins, `org.klipp.fixture.*`), in a
/// folder that holds nothing else: what `Scene::plugin_dirs` names.
pub fn ofx() -> PathBuf {
    ofx_at("ofx", &[])
}

/// The OFX fixture bundle built as version 2 (one parameter more): the
/// same plug-ins under another schema, in a folder of its own.
pub fn ofx_v2() -> PathBuf {
    ofx_at("ofx-v2", &["-DKLIPP_FIXTURE_VERSION=2".to_owned()])
}

/// An empty plug-in folder: a host searching it finds none.
pub fn no_plugins() -> PathBuf {
    let dir = scratch().join("no-plugins");
    std::fs::create_dir_all(&dir).expect("the empty folder");
    dir
}

fn ofx_at(name: &str, defines: &[String]) -> PathBuf {
    let dir = scratch().join(name);
    let binary = dir.join("KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx");
    std::fs::create_dir_all(binary.parent().expect("the bundle's folder")).expect("the bundle");
    compile(&crates().join("klipp-plugin/tests/fixtures/ofx/fixture.c"), defines, &binary);
    dir
}
