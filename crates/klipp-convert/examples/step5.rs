//! The step to revision 5 (`klipp_convert::step5`, #2754): every
//! revision-4 project on this machine to revision 5, each project file
//! backed up, a tracked surface's keys as maps from its reference corners.
//! Its dry run lists, per project, the surfaces it converts and every
//! named refusal (a stop).
//!
//!     cargo run --release -p klipp-convert --example step5 -- list
//!     cargo run --release -p klipp-convert --example step5 -- dry-run --out <folder> [--render <program>] [--exclude <dir>]...
//!     cargo run --release -p klipp-convert --example step5 -- run --approved <manifest hash> --confirm-real-run [--exclude <dir>]...
//!
//! The dry run reads only and writes its manifest and report under
//! `--out`, outside every project. `--render <program>` is run as
//! `<program> <project folder> <converted copy>` for each project to
//! convert: exit 0 with a line saying what matched is a pass, anything else
//! a failure. Without it the report says "render NOT RUN", and it is not
//! ready for the go. The real run takes the manifest hash the dry run
//! reported and refuses if anything changed since.
//!
//! It loads each written project with the app's loader, which only the
//! tests and examples link (klipp-project).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use klipp_convert::run::{no_klipp_running, real_preconditions, real_settings};
use klipp_convert::{scan, step3, step5};

/// The app's loader on a copy, strict: a clip whose stored picture its
/// fields don't give again fails the project in a release build too
/// (`load_project_strict`), reported with the rest, not the end of the run.
fn reload(dir: &Path) -> Result<(), String> {
    let dir = dir.to_path_buf();
    match std::panic::catch_unwind(move || klipp_project::load_project_strict(&dir).map(|_| ())) {
        Ok(r) => r,
        Err(e) => Err(format!("the loader stopped: {}", e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default())),
    }
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("klipp-convert step5: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_default();
    let (mut out, mut render, mut approved, mut confirmed, mut excluded) = (None, None, None, false, BTreeSet::new());
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut value = |what: &str| it.next().cloned().unwrap_or_else(|| fail(format!("{a} needs {what}")));
        match a.as_str() {
            "--out" => out = Some(PathBuf::from(value("a folder"))),
            "--render" => render = Some(value("a program")),
            "--approved" => approved = Some(value("the dry run's manifest hash")),
            "--exclude" => {
                excluded.insert(PathBuf::from(value("a project folder")));
            }
            "--confirm-real-run" => confirmed = true,
            other => fail(format!("unknown argument {other:?}")),
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| fail("HOME isn't set"));
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("klipp");
    let mut settings = real_settings(&home, &config, excluded).unwrap_or_else(|e| fail(format!("refused: {e}")));
    settings.backup_root = scan::backup_root_v5(&home);
    // The projects only: colour presets are side stores, which stay at 2.
    settings.config_dir = None;
    let build = format!("klipp-convert {} ({})", env!("CARGO_PKG_VERSION"), option_env!("KLIPP_GIT_SHA").unwrap_or("sha not recorded"));
    match mode.as_str() {
        "dry-run" => {
            let out = out.unwrap_or_else(|| fail("dry-run needs --out <folder>"));
            let program = render.clone();
            let run_render = move |project: &Path, copy: &Path| -> Result<String, String> {
                let program = program.as_deref().ok_or("no render program")?;
                let o = std::process::Command::new(program).arg(project).arg(copy).output().map_err(|e| format!("running {program}: {e}"))?;
                let said = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if o.status.success() { Ok(said) } else { Err(format!("{}: {said} {}", o.status, String::from_utf8_lossy(&o.stderr).trim())) }
            };
            let r = step5::dry_run(&settings, &out, &build, &reload, render.as_ref().map(|_| &run_render as &dyn Fn(&Path, &Path) -> Result<String, String>)).unwrap_or_else(|e| fail(e));
            print!("{}", r.text());
            println!("report in {}", out.join("report.txt").display());
            if !r.go() {
                std::process::exit(1);
            }
        }
        "run" => {
            if !confirmed {
                fail("this converts every revision-4 Klipp project on this machine, once; it runs only with --confirm-real-run");
            }
            let approved = approved.unwrap_or_else(|| fail("run needs --approved <the dry run's manifest hash>"));
            let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            real_preconditions(&repo).unwrap_or_else(|e| fail(format!("refused: {e}")));
            let hooks = step3::Hooks { crash: &|_| false, fail_write: &|_| false, reload: &reload };
            match step5::run(&settings, &approved, &no_klipp_running, &hooks) {
                Ok(r) => println!("converted {}, already at 5 {}, excluded {}; backups in {}", r.converted.len(), r.already.len(), r.excluded.len(), settings.backup_root.join(&settings.date).display()),
                Err(e) => fail(format!("{e:?}")),
            }
        }
        "list" => {
            // Read only: the projects the run's own discovery finds, each
            // with its revision and the decision preflight makes, as tab-
            // separated lines; what the dry run's corpus copy is taken from.
            let m = step5::preflight(&settings);
            for c in &m.candidates {
                println!("{}\t{:?}\t{:?}", c.dir.display(), c.revision, c.decision);
            }
            for s in &m.stops {
                println!("STOP\t{s}");
            }
        }
        _ => fail("usage: step5 list | dry-run --out <folder> [--render <program>] | run --approved <hash> --confirm-real-run"),
    }
}
