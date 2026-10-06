//! Open and save each project given, through a copy, and say whether the
//! saved `project.json` is byte for byte the one on disk (#1089, M1b's
//! first-save proof). Only the per-save stamps (`generation`, `saved_at`,
//! `klipp_version`, `platform`) may differ. The projects themselves are
//! never written.
//!
//! cargo run --release -p klipp-project --example resave_identity -- <project.klipp>...

use std::path::{Path, PathBuf};

static ADOPTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The top-level lines a save stamps afresh every time.
const STAMPS: [&str; 4] = ["  \"generation\":", "  \"saved_at\":", "  \"klipp_version\":", "  \"platform\":"];

fn without_stamps(text: &str) -> String {
    text.lines().filter(|l| !STAMPS.iter().any(|s| l.starts_with(s))).collect::<Vec<_>>().join("\n")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The first differing line of `a` and `b`, with its number.
fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}: disk {:?} / saved {:?}", i + 1, x.trim(), y.trim());
        }
    }
    format!("lengths differ: disk {} lines, saved {} lines", a.lines().count(), b.lines().count())
}

fn resave(project: &Path, scratch: &Path) -> Result<Option<String>, String> {
    let name = project.file_name().ok_or("no file name")?;
    let copy = scratch.join(name);
    copy_dir(project, &copy).map_err(|e| format!("copy: {e}"))?;
    let (sequences, media_pool, color, active_id, created_at, notes, zoom, scroll_x, track_scale, title_assets) =
        klipp_project::load_project(&copy)?;
    // Load keeps a clip's stored picture only if lifting the clip as
    // loaded gives exactly that picture: every clip holding one is the
    // lift(view) == stored proof for that clip.
    let clips: Vec<_> = sequences.iter().flat_map(|s| s.tracks.iter().flat_map(|t| t.clips.iter())).collect();
    let held = clips.iter().filter(|c| c.has_lifted_picture()).count();
    if held != clips.len() {
        return Err(format!("{held} of {} clips kept their stored picture", clips.len()));
    }
    ADOPTED.fetch_add(held, std::sync::atomic::Ordering::Relaxed);
    klipp_project::save_project_full(
        &copy,
        &sequences,
        &media_pool,
        &color,
        active_id.as_deref(),
        created_at.as_deref(),
        &notes,
        0,
        zoom,
        scroll_x,
        track_scale,
        &title_assets,
    )?;
    let disk = std::fs::read_to_string(project.join("project.json")).map_err(|e| e.to_string())?;
    let saved = std::fs::read_to_string(copy.join("project.json")).map_err(|e| e.to_string())?;
    let (disk, saved) = (without_stamps(&disk), without_stamps(&saved));
    Ok((disk != saved).then(|| first_difference(&disk, &saved)))
}

fn main() {
    let scratch: PathBuf = std::env::var_os("KLIPP_RESAVE_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("klipp-resave-{}", std::process::id())));
    let mut failed = 0;
    let projects: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    for project in &projects {
        match resave(project, &scratch) {
            Ok(None) => println!("SAME  {}", project.display()),
            Ok(Some(diff)) => {
                failed += 1;
                println!("DIFF  {}  {diff}", project.display());
            }
            Err(e) => {
                failed += 1;
                println!("ERROR {}  {e}", project.display());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!("{} of {} projects saved byte-identical", projects.len() - failed, projects.len());
    println!("{} clips kept their stored picture (lift(view) == stored)", ADOPTED.load(std::sync::atomic::Ordering::Relaxed));
    std::process::exit(i32::from(failed > 0));
}
