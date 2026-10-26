//! The real-project pixel check (#1089): a fixed set of frames of every
//! project, rendered offline as the export renders them, on the CPU and on
//! the GPU, so a later build's frames of the converted projects can be held
//! to them.
//!
//! ```text
//! cargo run --release -p klipp-render --example pixel_check -- \
//!     record  <projects> <out dir>
//! cargo run --release -p klipp-render --example pixel_check -- \
//!     compare <projects> <recorded dir> <out dir>
//! ```
//!
//! `<projects>` is a `klipp-convert scan` report (JSON with
//! `projects[].path`) or a list of project folders, one per line. Each
//! project is read from a temporary copy (its JSON copied, everything else
//! linked), never opened in place, and nothing launches the app or touches
//! a control socket. The frames are the user's footage: they go to `<out
//! dir>` (outside the repository), and only counts are printed.
//!
//! Per sequence: five times across it (start, a third, a half, two thirds,
//! the last frame), at 320 pixels wide, through every path a frame is drawn
//! by, named `<project #>/<sequence #>/<frame>-<path>.png`:
//! - `cpu`: the composite with the GPU off;
//! - `gpu`: the composite with the GPU on for grades and effects;
//! - `stack`: the viewer's GPU stack, display bytes as it draws them while
//!   playing (a frame it declines is counted, and has no file);
//! - `export` or `export-yuv`: the export's own frame, as `ExportJob`
//!   makes it, the encoder's planes when the frame is converted on the GPU
//!   (`-export-yuv`, the planes one after another as grey), else the
//!   picture through the encoder's display transform (`-export`).
//!
//! `index.json` names the projects and counts each path's frames.
//! `declines.txt` lists, sorted, every node that didn't draw what it shows
//! in a `cpu` or `gpu` frame (`<project #>/<sequence #>/<frame>-<path> clip
//! <id> node <id>: <plug-in>: <reason> (<outcome>)`); compare fails when the
//! set differs from the recording's, as it fails on a picture.
//!
//! The copy resolves stored paths as the original does: a relative media,
//! proxy, LUT or texture path is made absolute against the original
//! folder, by the loader's own rule, so the frames don't depend on where
//! the copy is (`TMPDIR`).
//!
//! A project from before revision 2 is converted on its temporary copy
//! next, as `klipp-convert run` converts it (the lift, and the LUT paths
//! made portable once), because a build from step A on (#1089) reads
//! revision 2 only. So the same list records at a build before the switch
//! and compares at one after it.
//!
//! **Coverage.** A frame of one flat colour shows nothing, and a project
//! with media offline renders placeholders, so a check of those proves
//! nothing; a missing LUT, title image or texture is drawn without, just as
//! silently. Each project's offline media, missing files and flat frames are
//! counted in `index.json`. A project is **covered** when all its media and
//! files are there and at least one frame is a picture. The summary says how many projects and
//! frames are covered, and lists the ones that aren't: they are never
//! counted as passed, and a project covered in the recording must be
//! covered in the compare.
//!
//! Each frame ends as the export's does, so a decoder that failed fails its
//! project, and `-gpu` frames drawn without a GPU fail it too. `record`
//! and `compare` exit non-zero if any project failed (drop those from the
//! list, or mend them, and record again), `compare` refuses a recording
//! with failures or one made from a different list, and both refuse an
//! output folder that isn't new and empty or overlaps the recorded one.

use std::path::{Path, PathBuf};

use klipp_core::Timecode;
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::pixel_check::{default_renderer, render_sequence, settings, Options, Rendered, differ, differ_route};
#[cfg(test)]
use klipp_render::pixel_check::decline_line;
use serde_json::{json, Value};

fn projects(list: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(list).map_err(|e| format!("reading the project list: {e}"))?;
    Ok(match serde_json::from_str::<Value>(&text) {
        Ok(v) => v["projects"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).map(PathBuf::from).collect(),
        Err(_) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect(),
    })
}

/// A temporary copy of a project: its JSON files copied, everything else
/// linked, so nothing of the original can be written. A JSON file, or the
/// `media` or `looks` folder, that is itself a link is copied as what it
/// points to: the copy owns every file and folder the check writes.
fn temp_copy(dir: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let (from, dest) = (e.path(), to.join(e.file_name()));
        // Through a link: what it points to.
        let target = std::fs::metadata(&from);
        let is_dir = target.as_ref().is_ok_and(|m| m.is_dir());
        let is_file = target.as_ref().is_ok_and(|m| m.is_file());
        if is_dir && (e.file_name() == "media" || e.file_name() == "looks") {
            temp_copy(&from, &dest)?;
        } else if from.extension().is_some_and(|x| x == "json") && is_file {
            std::fs::copy(&from, &dest)?;
        } else {
            std::os::unix::fs::symlink(&from, &dest)?;
        }
    }
    Ok(())
}

/// Write a file of the copy, refusing a link: what the check writes must
/// never reach the original.
fn write_owned(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is a link into the original: not written", path.display()));
    }
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Keys whose string value the loader resolves against the project folder:
/// a media item's `path` and `proxy_path`, a sequence's `global_lut_path`,
/// a title's legacy `texture_fill_path`, a texture's `File` source, and an
/// image layer's `path` (klipp-project's `load.rs`).
const PATH_KEYS: [&str; 5] = ["path", "proxy_path", "global_lut_path", "texture_fill_path", "File"];

/// Make the relative paths in a copied JSON file absolute against the
/// original folder, as the loader would resolve them there; the number
/// made. A value with a scheme (`bundled:`) or already absolute is left.
fn absolutize(value: &mut Value, original: &Path) -> usize {
    let mut n = 0;
    match value {
        Value::Object(o) => {
            for (k, v) in o.iter_mut() {
                match v {
                    Value::String(s) if PATH_KEYS.contains(&k.as_str()) && is_relative(s) => {
                        *s = klipp_project::path_util::resolve_media_path(original, Path::new(s.as_str())).to_string_lossy().into_owned();
                        n += 1;
                    }
                    _ => n += absolutize(v, original),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| n += absolutize(v, original)),
        _ => {}
    }
    n
}

fn is_relative(s: &str) -> bool {
    let scheme = s.split_once(':').is_some_and(|(head, _)| !head.contains('/'));
    !s.is_empty() && !Path::new(s).is_absolute() && !scheme
}

/// Resolve the copy's stored paths against the original folder.
fn resolve_against(copy: &Path, original: &Path) -> Result<(), String> {
    for file in [copy.join("project.json"), copy.join("media/manifest.json")] {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let mut v: Value = serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", file.display()))?;
        if absolutize(&mut v, original) > 0 {
            write_owned(&file, &serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?)?;
        }
    }
    Ok(())
}

/// Convert the copy's `project.json` to the current revision when it is older, as
/// `klipp-convert run` converts a real project (#1089).
fn to_revision_2(copy: &Path) -> Result<(), String> {
    let path = copy.join("project.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading: {e}"))?;
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| format!("parsing: {e}"))?;
    match raw.get("format_revision").and_then(Value::as_u64).unwrap_or(0) {
        r if r >= u64::from(klipp_project::FORMAT_REVISION) => return Ok(()),
        // Revision 2 takes the step to 3, revision 3 the step to 4 (M4
        // step F), and revision 4 the step to 5 (#2754), as the converter's
        // runs step a real project.
        r @ (2..=4) => {
            let at3 = if r == 2 { klipp_graph_lift::revision3::project(&raw).map_err(|e| format!("converting to 3: {e}"))? } else { raw };
            let at4 = if r <= 3 { klipp_graph_lift::revision4::project(&at3, &klipp_timeline::RegistryCatalog).map_err(|e| format!("converting to 4: {e}"))? } else { at3 };
            let converted = klipp_graph_lift::revision5::project(&at4).map_err(|e| format!("converting to 5: {e}"))?;
            return write_owned(&path, &serde_json::to_string_pretty(&converted).map_err(|e| e.to_string())?);
        }
        _ => {}
    }
    let library = klipp_core::assets::bundled_luts_dir();
    let rule = |s: &str| klipp_core::assets::portable_lut_path(&klipp_core::assets::resolve_lut_path(s, library), library);
    klipp_graph_lift::load_rewrites::portable_lut_paths(&mut raw, &rule);
    let (converted, _notes) = klipp_graph_lift::revision2::project(&raw).map_err(|e| format!("converting: {e}"))?;
    write_owned(&path, &serde_json::to_string_pretty(&converted).map_err(|e| e.to_string())?)
}

/// Files the copy's `project.json` names that aren't there: a LUT, a title
/// image or texture, a grade LUT. The renderer draws without them and says
/// nothing, so a project missing one isn't covered. Paths are absolute by
/// now (`resolve_against`); a grade LUT resolves through the library.
fn missing_assets(copy: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(copy.join("project.json")).map_err(|e| format!("reading: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("parsing: {e}"))?;
    let library = klipp_core::assets::bundled_luts_dir();
    fn walk(v: &Value, library: Option<&Path>, n: &mut usize) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    match x {
                        Value::String(s) if !s.is_empty() && PATH_KEYS.contains(&k.as_str()) => {
                            *n += usize::from(!Path::new(s).exists());
                        }
                        Value::String(s) if !s.is_empty() && k == "lut_path" => {
                            *n += usize::from(!klipp_core::assets::resolve_lut_path(s, library).exists());
                        }
                        _ => walk(x, library, n),
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, library, n)),
            _ => {}
        }
    }
    let mut n = 0;
    walk(&v, library, &mut n);
    Ok(n)
}

/// The lines only the recording has, and those only this run has.
fn declines_differ(recorded: &str, now: &str) -> (Vec<String>, Vec<String>) {
    let set = |t: &str| t.lines().filter(|l| !l.is_empty()).map(str::to_string).collect::<std::collections::BTreeSet<_>>();
    let (r, n) = (set(recorded), set(now));
    (r.difference(&n).cloned().collect(), n.difference(&r).cloned().collect())
}

/// A snapshot with the saved colour settings, independent of process defaults.
fn saved_snapshot(seq: &klipp_timeline::Sequence, sequences: &[klipp_timeline::Sequence], pool: &klipp_media::MediaPool, colour: &klipp_project::ColorSettings) -> Result<SequenceSnapshot, klipp_render::RenderError> {
    let lut = colour.global_lut_path.clone().filter(|_| colour.global_lut_enabled);
    let mut snapshot = SequenceSnapshot::from_sequence_with_all(seq, sequences, pool, &settings(), lut, false, colour.working_colorspace)?;
    snapshot.grade_encoding = colour.grade_encoding;
    snapshot.video.global_lut_opacity = colour.global_lut_opacity;
    Ok(snapshot)
}

/// Render project `dir`'s frames into `out`, from a copy under `scratch`.
fn render_project(dir: &Path, out: &Path, scratch: &Path) -> Result<Rendered, String> {
    let tmp = scratch.join(format!("klipp-pixel-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    if !dir.is_dir() {
        return Err("not there".into());
    }
    temp_copy(dir, &tmp).map_err(|e| format!("copying: {e}"))?;
    let missing = resolve_against(&tmp, dir).and_then(|()| to_revision_2(&tmp)).and_then(|()| missing_assets(&tmp));
    let loaded = missing.and_then(|missing| klipp_project::load_project(&tmp).map(|l| (missing, l)).map_err(|e| format!("loading: {e}")));
    let result = loaded.and_then(|(missing, (sequences, pool, colour, ..))| {
        let mut done = Rendered { offline: pool.all().filter(|m| m.offline).count(), missing, ..Rendered::default() };
        for (si, seq) in sequences.iter().enumerate() {
            let snapshot = saved_snapshot(seq, &sequences, &pool, &colour).map_err(|e| format!("snapshot: {e:?}"))?;
            let seq_dir = out.join(format!("{si}"));
            done.merge(render_sequence(&snapshot, &seq_dir, &si.to_string(), &tmp.with_extension("probe.mp4"), &Options::default(), &default_renderer)?);
        }
        Ok(done)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The worst difference and the count of samples over one level, between
/// two PNGs; alpha is ignored.
/// The frame a run made for `f` when the export took the other route:
/// `-export.png` for `-export-yuv.png`, or the other way round.
fn other_route(f: &Path) -> Option<PathBuf> {
    let name = f.file_name()?.to_str()?;
    let other = match name.strip_suffix("-export.png") {
        Some(n) => format!("{n}-export-yuv.png"),
        None => format!("{}-export.png", name.strip_suffix("-export-yuv.png")?),
    };
    Some(f.with_file_name(other))
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "png") {
                v.push(p.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    v.sort();
    v
}

/// Refuse output folders that would mix with what is already on disk: an
/// output folder that isn't new and empty could hold an earlier run's
/// frames, which a project failing now would leave to be compared; and one
/// that overlaps the recorded folder would write over the reference.
/// Checked before anything is written.
fn check_folders(out: &Path, recorded: Option<&Path>) -> Result<(), String> {
    if out.exists() && !out.is_dir() {
        return Err(format!("{} isn't a folder", out.display()));
    }
    if std::fs::read_dir(out).is_ok_and(|mut es| es.next().is_some()) {
        return Err(format!("the output folder {} isn't empty: give a new one", out.display()));
    }
    if let Some(rec) = recorded {
        let (o, r) = (resolve(out), resolve(rec));
        if o.starts_with(&r) || r.starts_with(&o) {
            return Err("the output folder and the recorded folder overlap: the output would write over the reference".into());
        }
    }
    Ok(())
}

/// `path` with its links and `..` resolved as the system would resolve
/// it, also where it doesn't exist yet: one part at a time, each prefix that
/// exists with its links followed.
fn resolve(path: &Path) -> PathBuf {
    let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut at = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                at.pop();
            }
            std::path::Component::CurDir => {}
            other => {
                at.push(other);
                if let Ok(real) = std::fs::canonicalize(&at) {
                    at = real;
                }
            }
        }
    }
    at
}

/// The check, by its arguments; the exit code. Copies go under `TMPDIR`.
fn run(args: &[String]) -> i32 {
    run_in(args, &std::env::temp_dir())
}

/// [`run`], with the projects' copies under `scratch`.
fn run_in(args: &[String], scratch: &Path) -> i32 {
    let (mode, list, out, recorded) = match args {
        [m, l, o] if m == "record" => ("record", l, PathBuf::from(o), None),
        [m, l, r, o] if m == "compare" => ("compare", l, PathBuf::from(o), Some(PathBuf::from(r))),
        _ => {
            eprintln!("usage: pixel_check record <projects> <out dir> | compare <projects> <recorded dir> <out dir>");
            return 2;
        }
    };
    if let Err(e) = check_folders(&out, recorded.as_deref()) {
        eprintln!("pixel_check: {e}");
        return 2;
    }
    let started = std::time::Instant::now();
    let list = match projects(Path::new(list)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pixel_check: {e}");
            return 2;
        }
    };
    // Frames are filed by place in the list, so compare must be given the
    // list record was, and a project record could not render has no frames
    // for compare to miss: the reference is only made clean.
    if let Some(rec) = &recorded {
        let Some(index) = std::fs::read_to_string(rec.join("index.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            eprintln!("pixel_check: no readable index.json in the recorded folder");
            return 2;
        };
        let paths: Vec<PathBuf> = index["projects"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).map(PathBuf::from).collect();
        if paths != list {
            eprintln!("pixel_check: the project list is not the one recorded ({} recorded, {} given)", paths.len(), list.len());
            return 2;
        }
        if index["failed"].as_array().is_some_and(|f| !f.is_empty()) {
            eprintln!("pixel_check: the recording has failed projects, so it is no reference");
            return 2;
        }
        if index["projects"].as_array().into_iter().flatten().any(|p| p.get("covered").is_none()) {
            eprintln!("pixel_check: the recording has no coverage: record it again with this build");
            return 2;
        }
    }
    let recorded_covered: Vec<bool> = recorded
        .as_ref()
        .and_then(|rec| std::fs::read_to_string(rec.join("index.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|v| v["projects"].as_array().into_iter().flatten().map(|p| p["covered"].as_bool() == Some(true)).collect())
        .unwrap_or_default();
    let (mut rendered, mut failed, mut frames, mut graded_on_gpu) = (0, Vec::new(), 0, 0);
    let (mut covered, mut real_frames, mut not_covered, mut lost) = (0, 0, Vec::new(), Vec::new());
    let (mut stack_drawn, mut stack_declined, mut export_gpu, mut export_cpu, mut export_letterboxed) = (0, 0, 0, 0, 0);
    let (mut plan_clips, mut plan_fallbacks, mut gpu_cpu_kernels) = (0u32, 0u32, 0u32);
    let mut index = Vec::new();
    let mut declines = Vec::new();
    for (i, dir) in list.iter().enumerate() {
        let r = match render_project(dir, &out.join(format!("{i}")), scratch) {
            Ok(r) => {
                rendered += 1;
                frames += r.frames;
                graded_on_gpu += usize::from(r.grade_on_gpu);
                if r.covered() {
                    covered += 1;
                    real_frames += r.frames - r.flat;
                } else {
                    not_covered.push(i);
                }
                r
            }
            Err(e) => {
                failed.push(json!({ "project": i, "why": e }));
                Rendered::default()
            }
        };
        // Covered when recorded, not now: its pictures are gone.
        if recorded_covered.get(i) == Some(&true) && !r.covered() {
            lost.push(i);
        }
        stack_drawn += r.stack;
        stack_declined += r.stack_declined;
        plan_clips += r.plan_clips;
        plan_fallbacks += r.plan_fallbacks;
        gpu_cpu_kernels += r.gpu_cpu_kernels;
        export_gpu += r.export_gpu;
        export_cpu += r.export_cpu;
        export_letterboxed += r.export_letterboxed;
        declines.extend(r.declines.iter().map(|l| format!("{i}/{l}")));
        index.push(json!({
            "project": i, "path": dir, "grade_on_gpu": r.grade_on_gpu, "frames": r.frames, "flat": r.flat, "offline": r.offline, "missing": r.missing, "covered": r.covered(),
            "stack": r.stack, "stack_declined": r.stack_declined, "export_gpu": r.export_gpu, "export_cpu": r.export_cpu, "export_letterboxed": r.export_letterboxed,
        }));
    }
    let text = serde_json::to_string_pretty(&json!({ "projects": index, "failed": failed })).unwrap_or_default();
    if let Err(e) = std::fs::create_dir_all(&out).and_then(|()| std::fs::write(out.join("index.json"), text)) {
        eprintln!("pixel_check: writing index.json: {e}");
        return 1;
    }
    // Sorted, one line each: the same frames with the same declines read
    // back the same, whatever order the renders ran in.
    declines.sort();
    let declines_text: String = declines.iter().map(|l| format!("{l}\n")).collect();
    if let Err(e) = std::fs::write(out.join("declines.txt"), &declines_text) {
        eprintln!("pixel_check: writing declines.txt: {e}");
        return 1;
    }
    println!(
        "{mode}: {rendered} projects, {frames} frames ({graded_on_gpu} projects with a grade run on the GPU for their -gpu frames), {} failed, in {:.0} s",
        failed.len(),
        started.elapsed().as_secs_f64()
    );
    println!("paths: GPU stack drew {stack_drawn} frames and declined {stack_declined}; export converted {export_gpu} on the GPU and {export_cpu} on the CPU ({export_letterboxed} letterboxed)");
    println!("plan: {plan_clips} clip renders took their effects from the picture, {plan_fallbacks} fell back to the list; the CPU kernels drew {gpu_cpu_kernels} effect runs in the GPU renders");
    let list_of = |v: &[usize]| v.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    println!(
        "coverage: {covered} of {rendered} projects show pictures, {real_frames} of {frames} frames; not covered (media offline, a file missing, or every frame flat; never counted as passed): {}",
        not_covered.len()
    );
    if !not_covered.is_empty() {
        println!("  not covered: {}", list_of(&not_covered));
    }
    if !failed.is_empty() {
        // The reasons can name the user's files, so they stay in index.json.
        let which: Vec<String> = failed.iter().map(|f| f["project"].to_string()).collect();
        println!("  failed projects (why in index.json): {}", which.join(", "));
    }
    if !lost.is_empty() {
        println!("  covered when recorded, not now: {}", list_of(&lost));
    }
    // A project that fails now, or lost the pictures it was recorded with,
    // fails the check.
    let mut code = i32::from(!failed.is_empty() || !lost.is_empty());
    if let Some(rec) = recorded {
        let (mut same, mut different, mut missing) = (0, 0, 0);
        // An export frame recorded on one route and made on the other: its
        // picture held to the planes (`differ_route`), never left missing.
        // Its bar is a level wider: the picture it is held to was rounded
        // to 8-bit RGB before it was converted, where the planes were
        // converted from the frame's floats.
        let (mut rerouted, mut rerouted_worst, mut rerouted_over, mut rerouted_samples) = (Vec::new(), 0, 0, 0);
        for f in files(&rec) {
            let mut bar = 1;
            let compared = match differ(&rec.join(&f), &out.join(&f)) {
                Err(_) if !out.join(&f).exists() => match other_route(&f).filter(|o| out.join(o).exists()) {
                    Some(o) => {
                        bar = 2;
                        let gpu_now = o.to_string_lossy().ends_with("-export-yuv.png");
                        rerouted.push(format!("{} ({})", f.display(), if gpu_now { "CPU→GPU" } else { "GPU→CPU" }));
                        let (rgb, yuv) = if gpu_now { (rec.join(&f), out.join(&o)) } else { (out.join(&o), rec.join(&f)) };
                        let r = differ_route(&rgb, &yuv);
                        if let Ok((worst, over)) = r {
                            rerouted_worst = rerouted_worst.max(worst);
                            rerouted_over += over;
                            rerouted_samples += image::image_dimensions(&rgb).map_or(0, |(w, h)| (w * h) as usize * 3 / 2);
                        }
                        r
                    }
                    None => Err("missing".into()),
                },
                r => r,
            };
            match compared {
                Ok((worst, _)) if worst <= bar => same += 1,
                Ok(_) => different += 1,
                Err(_) => missing += 1,
            }
        }
        println!("compared: {same} frames alike, {different} differ by more than a level, {missing} missing or unreadable");
        if !rerouted.is_empty() {
            println!(
                "  route changed: {} export frames held to the other route's planes (bar 2 levels), worst {rerouted_worst}, {rerouted_over} of {rerouted_samples} samples over a level",
                rerouted.len()
            );
            for r in &rerouted {
                println!("    {r}");
            }
        }
        if different + missing > 0 {
            code = 1;
        }
        // The same pictures drawn with other declines are not the same
        // frames: a node that stopped drawing, or started, fails the check.
        match std::fs::read_to_string(rec.join("declines.txt")) {
            Ok(recorded) => {
                let (gone, new) = declines_differ(&recorded, &declines_text);
                println!("declines: {} recorded, {} now; {} only recorded, {} only now", recorded.lines().count(), declines.len(), gone.len(), new.len());
                for l in gone.iter().take(10) {
                    println!("  only recorded: {l}");
                }
                for l in new.iter().take(10) {
                    println!("  only now: {l}");
                }
                if !gone.is_empty() || !new.is_empty() {
                    code = 1;
                }
            }
            Err(_) => {
                println!("declines: the recording has no declines.txt: record it again with this build");
                code = 1;
            }
        }
    }
    code
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|m| m == "resolve") {
        std::process::exit(resolve_sweep(&args[1..], &std::env::temp_dir()));
    }
    std::process::exit(run(&args));
}

/// `resolve <projects>`: every clip of every project, at five times
/// across it, its effects read from its lifted picture against the
/// renderer's list (#1089, M2 step 10c-1). CPU only, nothing rendered.
fn resolve_sweep(args: &[String], scratch: &Path) -> i32 {
    let [list] = args else {
        eprintln!("usage: pixel_check resolve <projects>");
        return 2;
    };
    let list = match projects(Path::new(list)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pixel_check: {e}");
            return 2;
        }
    };
    let (mut clips, mut times, mut mismatches, mut failed, mut empty) = (0, 0, 0, Vec::new(), Vec::new());
    for (i, dir) in list.iter().enumerate() {
        let tmp = scratch.join(format!("klipp-resolve-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let loaded = temp_copy(dir, &tmp)
            .map_err(|e| format!("copying: {e}"))
            .and_then(|()| resolve_against(&tmp, dir))
            .and_then(|()| to_revision_2(&tmp))
            .and_then(|()| klipp_project::load_project(&tmp).map_err(|e| format!("loading: {e}")));
        let (sequences, pool, colour) = match loaded {
            Ok((s, p, c, ..)) => (s, p, c),
            Err(e) => {
                failed.push(format!("{i}: {e}"));
                let _ = std::fs::remove_dir_all(&tmp);
                continue;
            }
        };
        let before = clips;
        for seq in &sequences {
            let Ok(snapshot) = saved_snapshot(seq, &sequences, &pool, &colour) else {
                failed.push(format!("{i}: snapshot"));
                continue;
            };
            let fps = snapshot.fps;
            for track in &snapshot.video.sequence.tracks {
                for clip in &track.clips {
                    if matches!(clip.kind, klipp_timeline::ClipKind::Gap) {
                        continue;
                    }
                    clips += 1;
                    let picture = match clip.picture_v2() {
                        Ok(p) => p,
                        Err(e) => {
                            mismatches += 1;
                            println!("  {i} clip {}: no lift: {e}", clip.id.0);
                            continue;
                        }
                    };
                    let (a, b) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
                    for k in 0..5 {
                        let t = a + (b - a) * (k as f64 / 5.0);
                        let time = Timecode::from_secs_f64(t, snapshot.fps_num);
                        times += 1;
                        let want = klipp_compose::render::list_effects(clip, time, fps, None, None).0;
                        let got = klipp_compose::resolve::resolve_effects(clip, &picture.picture, time, fps, None, None);
                        match got {
                            Ok(got) if format!("{got:?}") == format!("{want:?}") => {}
                            Ok(got) => {
                                mismatches += 1;
                                println!("  {i} clip {} at {t:.3}: picture {got:?}\n    renderer {want:?}", clip.id.0);
                            }
                            Err(e) => {
                                mismatches += 1;
                                println!("  {i} clip {} at {t:.3}: {e}", clip.id.0);
                            }
                        }
                    }
                }
            }
        }
        if clips == before {
            empty.push(i);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    println!("resolve: {} projects, {clips} clips at {times} times, {mismatches} mismatches, {} failed", list.len(), failed.len());
    println!("  no clips: {empty:?}");
    for f in &failed {
        println!("  failed {f}");
    }
    i32::from(mismatches > 0 || !failed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder with a recorded reference: a clean `index.json`
    /// naming one project, which is not there, and one frame.
    fn reference(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rec = root.join("rec");
        std::fs::create_dir_all(rec.join("0/0")).unwrap();
        let missing = root.join("Gone.klipp");
        std::fs::write(rec.join("index.json"), json!({ "projects": [{ "project": 0, "path": missing, "covered": true }], "failed": [] }).to_string()).unwrap();
        image::save_buffer(rec.join("0/0/0-cpu.png"), &[9u8; 16], 2, 2, image::ExtendedColorType::Rgba8).unwrap();
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", missing.display())).unwrap();
        (root, rec, list)
    }

    fn args(v: &[&Path]) -> Vec<String> {
        v.iter().map(|p| p.display().to_string()).collect()
    }

    /// A declined frame's line names its frame, clip, node, plug-in,
    /// reason and outcome, never the detail; and a set that changed either
    /// way is a difference.
    #[test]
    fn a_decline_is_a_line_and_a_changed_set_differs() {
        let clip = klipp_core::ClipId::new();
        let node = uuid::Uuid::new_v4();
        let d = klipp_nodes::Decline {
            clip: Some(clip),
            node,
            kind: klipp_nodes::NodeKind::Frei0r,
            outcome: klipp_nodes::DeclineOutcome::Unsupported,
            name: "Delay0r".into(),
            reason: "needs a frame size under 2048".into(),
            why: "/tmp/klipp-pixel-check-1/x".into(),
        };
        let line = decline_line("0/12-gpu", &d);
        assert_eq!(line, format!("0/12-gpu clip {} node {node}: frei0r Delay0r: needs a frame size under 2048 (unsupported)", clip.0));
        assert!(!line.contains("/tmp"), "the detail stays out: {line}");
        let a = format!("{line}\n");
        assert_eq!(declines_differ(&a, &a), (vec![], vec![]));
        assert_eq!(declines_differ(&a, ""), (vec![line.clone()], vec![]), "a decline gone");
        assert_eq!(declines_differ("", &a), (vec![], vec![line]), "a decline new");
    }

    /// record writes declines.txt beside index.json, and compare fails on
    /// a recording whose declines differ from the frames drawn now.
    #[test]
    fn compare_fails_on_a_changed_decline_set() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-declines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(here) = relative_project(&root, "here", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", here.display())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root), 0);
        let recorded = std::fs::read_to_string(root.join("rec/declines.txt")).expect("record writes declines.txt");
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec"), &root.join("cmp")]), &root), 0, "the same declines pass");
        std::fs::write(root.join("rec/declines.txt"), format!("{recorded}0/0/0-gpu clip - node {}: Gone: plug-in missing (plug-in missing)\n", uuid::Uuid::nil())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec"), &root.join("cmp2")]), &root), 1, "a changed set fails");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project that fails in compare fails the check, even with the
    /// frames it would have drawn already beside it from an earlier run:
    /// such an output folder is refused before anything is written.
    #[test]
    fn a_project_failing_now_fails_the_check() {
        let (root, rec, list) = reference("failing");
        let out = root.join("out");
        assert_eq!(run(&args(&[Path::new("compare"), &list, &rec, &out])), 1, "a missing project fails compare");
        // The same compare over an earlier run's output, which holds the
        // very frames of the reference: refused, not passed.
        let stale = root.join("stale");
        std::fs::create_dir_all(stale.join("0/0")).unwrap();
        std::fs::copy(rec.join("0/0/0-cpu.png"), stale.join("0/0/0-cpu.png")).unwrap();
        assert_eq!(run(&args(&[Path::new("compare"), &list, &rec, &stale])), 2);
        assert!(!stale.join("index.json").exists(), "nothing written");
        assert_eq!(run(&args(&[Path::new("record"), &list, &root.join("rec2")])), 1, "and record");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project in `root/projects/P.klipp` whose one clip is a clip at
    /// `root/src/clip.avi`, stored relative to the project
    /// (`../../src/clip.avi`); with `gone`, the clip isn't there. None
    /// without ffmpeg.
    fn relative_project(root: &Path, name: &str, gone: bool) -> Option<PathBuf> {
        sized_project(root, name, gone, (64, 36))
    }

    /// [`relative_project`] with a clip and sequence of `size`.
    fn sized_project(root: &Path, name: &str, gone: bool, (w, h): (u32, u32)) -> Option<PathBuf> {
        use klipp_core::{Fps, MediaId, Resolution};
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let clip = src.join(format!("{name}.avi"));
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc=size={w}x{h}:rate=25:duration=1"), "-c:v", "ffv1"])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            return None;
        }
        if gone {
            std::fs::remove_file(&clip).unwrap();
        }
        let fps = Fps::new(25, 1);
        let id = MediaId::new();
        let mut pool = klipp_media::MediaPool::default();
        pool.add(klipp_media::MediaItem {
            path: clip.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: w, height: h },
            ..klipp_media::MediaItem::new_synthetic_title(id, format!("{name}.avi"), Timecode::from_frames(25, 25), fps)
        });
        let mut seq = klipp_timeline::Sequence::new_default();
        (seq.fps, seq.resolution) = (fps, Resolution { width: w, height: h });
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![klipp_timeline::Clip {
            kind: klipp_timeline::ClipKind::Media { media_id: id },
            source_in: Timecode::from_frames(0, 25),
            source_out: Timecode::from_frames(25, 25),
            ..klipp_timeline::Clip::gap(Timecode::from_frames(0, 25), Timecode::from_frames(25, 25))
        }];
        seq.tracks = vec![v1];
        let dir = root.join("projects").join(format!("{name}.klipp"));
        klipp_project::save_project(&dir, &[seq], &pool).unwrap();
        // Stored relative, as a project made beside its media is.
        let manifest = dir.join("media/manifest.json");
        let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        m["items"][0]["path"] = json!(format!("../../src/{name}.avi"));
        std::fs::write(&manifest, m.to_string()).unwrap();
        Some(dir)
    }

    #[test]
    fn saved_colour_settings_change_the_picture_the_checkpoint_checks() {
        use klipp_timeline::GradeAccess;
        let root = std::env::temp_dir().join(format!("klipp-pixel-colour-{}", uuid::Uuid::new_v4()));
        let project = relative_project(&root, "colour", false).expect("FFmpeg is required");
        let (mut sequences, pool, ..) = klipp_project::load_project_strict(&project).unwrap();
        sequences[0].tracks[0].clips[0].edit_grade(|g| {
            let id = g.ensure_corrector();
            let params = &mut g.nodes.iter_mut().find(|n| n.id == id).unwrap().params;
            params.gain_master = Some(0.15);
            params.gamma_x = Some(0.07);
        });
        let lut = root.join("invert-red.cube");
        std::fs::write(&lut, "LUT_3D_SIZE 2\n1 0 0\n0 0 0\n1 1 0\n0 1 0\n1 0 1\n0 0 1\n1 1 1\n0 1 1\n").unwrap();
        let base_colour = klipp_project::ColorSettings {
            working_colorspace: klipp_core::WorkingColorspace::AcesCG,
            grade_encoding: klipp_core::GradeEncoding::Bt1886,
            ..Default::default()
        };
        let draw = |name: &str, colour: klipp_project::ColorSettings| {
            let dir = root.join(format!("{name}.klipp"));
            klipp_project::save_project_with_color(&dir, &sequences, &pool, &colour, None, None, "").unwrap();
            let out = root.join(format!("{name}-frames"));
            render_project(&dir, &out, &root.join("scratch")).unwrap();
            out.join("0/0-cpu.png")
        };
        let base = draw("base", base_colour.clone());
        let disabled = draw("disabled", klipp_project::ColorSettings {
            global_lut_path: Some(lut.clone()), global_lut_enabled: false, global_lut_opacity: 1.0,
            ..base_colour.clone()
        });
        assert_eq!(differ(&base, &disabled).unwrap().0, 0, "a disabled saved LUT must not change the picture");
        let zero = draw("zero", klipp_project::ColorSettings {
            global_lut_path: Some(lut.clone()), global_lut_enabled: true, global_lut_opacity: 0.0,
            ..base_colour.clone()
        });
        assert_eq!(differ(&base, &zero).unwrap().0, 0, "a saved LUT at zero strength must not change the picture");
        let full = draw("full", klipp_project::ColorSettings {
            global_lut_path: Some(lut), global_lut_enabled: true, global_lut_opacity: 1.0,
            ..base_colour.clone()
        });
        assert!(differ(&base, &full).unwrap().0 > 1, "the LUT control must visibly change the picture");
        let encoded = draw("encoded", klipp_project::ColorSettings {
            grade_encoding: klipp_core::GradeEncoding::AcesCct, ..base_colour
        });
        assert!(differ(&base, &encoded).unwrap().0 > 1, "saved grade encoding must reach the renderer in ACEScg");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn index_of(out: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(out.join("index.json")).unwrap()).unwrap()
    }

    /// A project's relative media resolves against its own folder, not the
    /// copy's: recorded from two scratch folders at different depths, the
    /// frames are the same pictures, and the project is covered. Before
    /// #1089's fix the copy resolved `../../src` from the scratch folder,
    /// found nothing, and recorded placeholders that passed.
    #[test]
    fn relative_media_renders_the_same_from_any_scratch_folder() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-relative-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(project) = relative_project(&root, "shot", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", project.display())).unwrap();
        let (shallow, deep) = (root.join("s"), root.join("d/e/e/p"));
        for (scratch, out) in [(&shallow, root.join("rec-a")), (&deep, root.join("rec-b"))] {
            std::fs::create_dir_all(scratch).unwrap();
            assert_eq!(run_in(&args(&[Path::new("record"), &list, &out]), scratch), 0, "{}", scratch.display());
            let p = &index_of(&out)["projects"][0];
            assert_eq!((p["offline"].as_u64(), p["covered"].as_bool()), (Some(0), Some(true)), "{p}");
            assert_eq!(p["flat"], 0, "every frame a picture: {p}");
        }
        let frames = files(&root.join("rec-a"));
        assert!(!frames.is_empty());
        assert_eq!(frames, files(&root.join("rec-b")));
        for f in &frames {
            assert_eq!(differ(&root.join("rec-a").join(f), &root.join("rec-b").join(f)).unwrap().0, 0, "{}", f.display());
        }
        // And it compares clean from a third depth.
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec-a"), &root.join("cmp")]), &root), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project whose media is offline renders placeholders: it is
    /// reported, isn't covered and isn't counted as passed; and a project
    /// covered when recorded but not now fails the compare.
    #[test]
    fn media_offline_is_not_covered() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-offline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (Some(gone), Some(here)) = (relative_project(&root, "gone", true), relative_project(&root, "here", false)) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n{}\n", gone.display(), here.display())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root), 0);
        let ps = &index_of(&root.join("rec"))["projects"];
        assert_eq!((ps[0]["offline"].as_u64(), ps[0]["covered"].as_bool()), (Some(1), Some(false)), "{}", ps[0]);
        assert_eq!(ps[1]["covered"], true, "{}", ps[1]);
        // The covered project loses its media: the compare fails.
        std::fs::remove_file(root.join("src/here.avi")).unwrap();
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec"), &root.join("cmp")]), &root), 1);
        assert_eq!(index_of(&root.join("cmp"))["projects"][1]["covered"], false);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review of #1127: a project whose `manifest.json`, `project.json` or
    /// `media` folder is a link is checked without writing through the
    /// link. The originals are byte for byte what they were, and the
    /// project is still covered (its relative media still resolves).
    #[test]
    fn a_linked_json_or_media_folder_is_never_written_through() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(project) = relative_project(&root, "linked", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        // Three projects: a linked manifest, a linked project.json, a linked media folder.
        let mut cases = Vec::new();
        for (n, what) in ["manifest", "project", "media"].into_iter().enumerate() {
            let dir = root.join("projects").join(format!("{what}.klipp"));
            std::fs::create_dir_all(dir.join("media")).unwrap();
            std::fs::copy(project.join("project.json"), dir.join("project.json")).unwrap();
            std::fs::copy(project.join("media/manifest.json"), dir.join("media/manifest.json")).unwrap();
            let (inside, real) = match what {
                "manifest" => (dir.join("media/manifest.json"), elsewhere.join(format!("{n}-manifest.json"))),
                "project" => (dir.join("project.json"), elsewhere.join(format!("{n}-project.json"))),
                _ => (dir.join("media"), elsewhere.join(format!("{n}-media"))),
            };
            std::fs::rename(&inside, &real).unwrap();
            std::os::unix::fs::symlink(&real, &inside).unwrap();
            let file = if what == "media" { real.join("manifest.json") } else { real.clone() };
            cases.push((dir, file.clone(), std::fs::read(&file).unwrap()));
        }
        let list = root.join("list.txt");
        std::fs::write(&list, cases.iter().map(|(d, ..)| format!("{}\n", d.display())).collect::<String>()).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root.join("projects")), 0);
        for (i, (dir, file, before)) in cases.iter().enumerate() {
            assert_eq!(&std::fs::read(file).unwrap(), before, "{}: the original is untouched", dir.display());
            assert_eq!(index_of(&root.join("rec"))["projects"][i]["covered"], true, "{}", dir.display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review of #1127: a file the project names that isn't there (a LUT,
    /// a title image) is drawn without, silently, so the project isn't
    /// covered even though its media is.
    #[test]
    fn a_missing_lut_or_title_image_is_not_covered() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(project) = relative_project(&root, "lut", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let path = project.join("project.json");
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["sequences"][0]["global_lut_path"] = json!("luts/not-there.cube");
        std::fs::write(&path, v.to_string()).unwrap();
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", project.display())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root), 0);
        let p = &index_of(&root.join("rec"))["projects"][0];
        assert_eq!((p["offline"].as_u64(), p["missing"].as_u64(), p["covered"].as_bool()), (Some(0), Some(1), Some(false)), "{p}");

        // A title image layer's file, the same way.
        let copy = root.join("copy.klipp");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::write(copy.join("project.json"), json!({ "o": [{ "content": { "Image": { "path": "/not/there.png" } } }] }).to_string()).unwrap();
        assert_eq!(missing_assets(&copy).unwrap(), 1);
        std::fs::write(copy.join("project.json"), json!({ "o": [{ "content": { "Image": { "path": path } } }] }).to_string()).unwrap();
        assert_eq!(missing_assets(&copy).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each frame is drawn by the path it is named for. A one-clip project
    /// is one the GPU stack draws and the export converts on the GPU, as
    /// `ExportJob` would: the counts say every frame went that way, each
    /// path left one file per time, the export's planes have I420's shape,
    /// and the recording compares clean against itself.
    #[test]
    fn every_path_draws_its_own_frames() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(project) = relative_project(&root, "paths", false) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", project.display())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root), 0);
        let p = &index_of(&root.join("rec"))["projects"][0];
        // One sequence, five times, four paths.
        assert_eq!((p["stack"].as_u64(), p["stack_declined"].as_u64()), (Some(5), Some(0)), "the GPU stack draws every frame: {p}");
        assert_eq!((p["export_gpu"].as_u64(), p["export_cpu"].as_u64()), (Some(5), Some(0)), "the export converts every frame on the GPU: {p}");
        assert_eq!((p["frames"].as_u64(), p["covered"].as_bool()), (Some(20), Some(true)), "{p}");
        let names = files(&root.join("rec"));
        for path in ["cpu", "gpu", "stack", "export-yuv"] {
            let n = names.iter().filter(|f| f.to_string_lossy().ends_with(&format!("-{path}.png"))).count();
            assert_eq!(n, 5, "{path}: {names:?}");
        }
        // 64x36 at 320 wide is 180 high: Y, then Cb and Cr at a quarter each.
        let yuv = names.iter().find(|f| f.to_string_lossy().ends_with("-export-yuv.png")).unwrap();
        let img = image::open(root.join("rec").join(yuv)).unwrap();
        assert_eq!((img.width(), img.height()), (320, 180 * 3 / 2));
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec"), &root.join("cmp")]), &root), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A sequence of an odd aspect (64x27, about 2.37:1) at the check's
    /// 320 wide would come out 320x134, which the export letterboxes and
    /// renders on the CPU. The export's size is chosen so the picture fills
    /// it, so the export converts on the GPU as a full-size export of it
    /// would, and nothing is letterboxed (#1160 review).
    #[test]
    fn an_odd_aspect_exports_the_way_a_full_size_export_does() {
        let root = std::env::temp_dir().join(format!("klipp-pixel-check-test-aspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let Some(project) = sized_project(&root, "wide", false, (64, 27)) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let list = root.join("list.txt");
        std::fs::write(&list, format!("{}\n", project.display())).unwrap();
        assert_eq!(run_in(&args(&[Path::new("record"), &list, &root.join("rec")]), &root), 0);
        let p = &index_of(&root.join("rec"))["projects"][0];
        assert_eq!((p["export_gpu"].as_u64(), p["export_cpu"].as_u64(), p["export_letterboxed"].as_u64()), (Some(5), Some(0), Some(0)), "{p}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A stored path is made absolute against the original folder only
    /// where it is relative: absolute paths and names with a scheme stay.
    #[test]
    fn only_relative_paths_are_resolved() {
        let mut v = json!({
            "items": [{ "path": "../src/a.mov", "proxy_path": "proxies/a.mov", "filename": "a/b.mov" }],
            "sequences": [{ "global_lut_path": "luts/l.cube", "name": "x/y" }],
            "o": { "source": { "File": "tex/t.png" }, "path": "/abs/b.png", "lut_path": "bundled:fuji/a.cube" }
        });
        assert_eq!(absolutize(&mut v, Path::new("/p/Film.klipp")), 4);
        assert_eq!(v["items"][0]["path"], "/p/src/a.mov");
        assert_eq!(v["items"][0]["proxy_path"], "/p/Film.klipp/proxies/a.mov");
        assert_eq!(v["items"][0]["filename"], "a/b.mov", "not a path the loader resolves");
        assert_eq!(v["sequences"][0]["global_lut_path"], "/p/Film.klipp/luts/l.cube");
        assert_eq!(v["sequences"][0]["name"], "x/y");
        assert_eq!(v["o"]["source"]["File"], "/p/Film.klipp/tex/t.png");
        assert_eq!(v["o"]["path"], "/abs/b.png");
        assert_eq!(v["o"]["lut_path"], "bundled:fuji/a.cube");
    }

    /// A recording made before coverage was counted is refused: whether
    /// its frames were pictures isn't known.
    #[test]
    fn a_recording_without_coverage_is_refused() {
        let (root, rec, list) = reference("uncounted");
        let index = json!({ "projects": [{ "project": 0, "path": std::fs::read_to_string(&list).unwrap().trim() }], "failed": [] });
        std::fs::write(rec.join("index.json"), index.to_string()).unwrap();
        assert_eq!(run(&args(&[Path::new("compare"), &list, &rec, &root.join("out")])), 2);
        assert!(!root.join("out").exists(), "nothing written");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An output folder that is the recorded one, inside it, holds it, or
    /// names it through a link or `..`, is refused before anything is
    /// written: the reference is never written over.
    #[test]
    fn an_output_overlapping_the_reference_is_refused() {
        let (root, rec, list) = reference("overlap");
        let index = std::fs::read(rec.join("index.json")).unwrap();
        std::os::unix::fs::symlink(&rec, root.join("link")).unwrap();
        for out in [rec.clone(), rec.join("new"), root.clone(), root.join("link"), root.join("link/new"), root.join("x/../rec")] {
            assert_eq!(run(&args(&[Path::new("compare"), &list, &rec, &out])), 2, "{}", out.display());
            assert_eq!(std::fs::read(rec.join("index.json")).unwrap(), index, "{}: the reference index untouched", out.display());
        }
        assert!(!rec.join("new").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
