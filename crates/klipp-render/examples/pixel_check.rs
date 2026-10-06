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

use klipp_core::{ExportSettings, Timecode, WorkingColorspace};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::ExportFrameSource;
use serde_json::{json, Value};

const WIDTH: u32 = 320;

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

/// Convert the copy's `project.json` to revision 2 when it is older, as
/// `klipp-convert run` converts a real project (#1089).
fn to_revision_2(copy: &Path) -> Result<(), String> {
    let path = copy.join("project.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading: {e}"))?;
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| format!("parsing: {e}"))?;
    if raw.get("format_revision").and_then(Value::as_u64).unwrap_or(0) >= u64::from(klipp_project::FORMAT_REVISION) {
        return Ok(());
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

/// Whether an RGBA frame is one colour throughout (alpha aside).
fn is_flat(px: &[u8]) -> bool {
    let first = &px[..3.min(px.len())];
    px.chunks_exact(4).all(|p| &p[..3] == first)
}

/// What a project rendered: frames written and how many were flat, its
/// media offline and other files missing (a LUT, a title image or texture),
/// whether a grade ran on the GPU for its `-gpu` frames, and which way each
/// of its stack and export frames went.
#[derive(Default)]
struct Rendered {
    frames: usize,
    flat: usize,
    offline: usize,
    missing: usize,
    grade_on_gpu: bool,
    /// Frames the GPU stack drew (`-stack`), and those it declined.
    stack: usize,
    stack_declined: usize,
    /// Export frames converted on the GPU (`-export-yuv`), and those
    /// rendered and converted on the CPU (`-export`), as the export job
    /// chooses between them.
    export_gpu: usize,
    export_cpu: usize,
}

impl Rendered {
    fn covered(&self) -> bool {
        self.offline == 0 && self.missing == 0 && self.flat < self.frames
    }
}

fn settings() -> ExportSettings {
    ExportSettings { output_path: PathBuf::from("/dev/null"), ..ExportSettings::default() }
}

fn display(linear: &[f32], ws: WorkingColorspace) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb)
}

/// The renderer as the export sets it up, on the CPU or with the GPU.
fn renderer(snapshot: &SequenceSnapshot, gpu: bool) -> klipp_compose::Renderer {
    let mut ofx = klipp_plugin::OfxHost::new();
    if snapshot.video.sequence.tracks.iter().flat_map(|t| &t.clips).any(|c| c.has_enabled_plugins()) {
        ofx.discover_plugins();
    }
    let mut r = klipp_compose::Renderer::new(ofx, klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(snapshot.working_colorspace);
    r.set_grade_encoding(snapshot.grade_encoding);
    r.set_lut_tetrahedral(true);
    if let Some(path) = &snapshot.global_lut_path {
        if let Ok(lut) = klipp_color::lut::load_lut(path) {
            r.set_global_lut(Some(klipp_compose::GlobalLut { lut, opacity: snapshot.video.global_lut_opacity }));
        }
    }
    r
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
            let snapshot = SequenceSnapshot::from_sequence_with_all(seq, &sequences, &pool, &settings(), colour.global_lut_path.clone(), false, colour.working_colorspace).map_err(|e| format!("snapshot: {e:?}"))?;
            let (first, last) = (snapshot.start_frame(), snapshot.end_frame());
            if last <= first {
                continue;
            }
            let span = last - 1 - first;
            let frames: Vec<u64> = [0, span / 3, span / 2, span * 2 / 3, span].iter().map(|f| first + f).collect();
            let h = ((WIDTH as u64 * seq.resolution.height as u64 / seq.resolution.width.max(1) as u64) as u32).max(2) & !1;
            let seq_dir = out.join(format!("{si}"));
            std::fs::create_dir_all(&seq_dir).map_err(|e| e.to_string())?;
            for (gpu, name) in [(false, "cpu"), (true, "gpu")] {
                let mut r = renderer(&snapshot, gpu);
                let mut source = ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
                for &f in &frames {
                    let time = Timecode::from_frames(f as i64, snapshot.fps_num);
                    let linear = r.render(&snapshot.video.sequence, time, WIDTH, h, &mut source, klipp_compose::RenderOptions::default());
                    // As the export does after each frame: a decoder that
                    // failed fails the project rather than being recorded,
                    // and the streams the frame did not use are closed.
                    source.end_frame().map_err(|e| format!("frame {f} ({name}): {e}"))?;
                    let px = display(&linear, snapshot.working_colorspace);
                    image::save_buffer(seq_dir.join(format!("{f}-{name}.png")), &px, WIDTH, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
                    done.frames += 1;
                    done.flat += usize::from(is_flat(&px));
                }
                // A `-gpu` frame drawn without a GPU would make a CPU
                // reference pass for a GPU one. The effects pipeline and
                // the grade's executor open the same shared device; the
                // executor opens it only when there is a grade to run.
                if gpu && r.gpu_pipeline().is_none() {
                    return Err("no GPU for the -gpu frames".into());
                }
                done.grade_on_gpu |= gpu && r.executor().has_gpu();
            }
            stack_frames(&snapshot, &frames, h, &seq_dir, &mut done)?;
            export_frames(&snapshot, &frames, h, &seq_dir, &tmp.with_extension("probe.mp4"), &mut done)?;
        }
        Ok(done)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The viewer's GPU stack: each frame's display bytes, as the viewer draws
/// them while playing, or the stack declining the frame (`None`), which is
/// counted and leaves no `-stack` file.
fn stack_frames(snapshot: &SequenceSnapshot, frames: &[u64], h: u32, seq_dir: &Path, done: &mut Rendered) -> Result<(), String> {
    let mut r = renderer(snapshot, true);
    let mut source = ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
    for &f in frames {
        let time = Timecode::from_frames(f as i64, snapshot.fps_num);
        let stack = r.render_display_u8_on_gpu(&snapshot.video.sequence, time, WIDTH, h, &mut source, klipp_compose::RenderOptions::default());
        source.end_frame().map_err(|e| format!("frame {f} (stack): {e}"))?;
        match stack {
            Some((px, _)) => {
                image::save_buffer(seq_dir.join(format!("{f}-stack.png")), &px, WIDTH, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
                done.frames += 1;
                done.flat += usize::from(is_flat(&px));
                done.stack += 1;
            }
            None => done.stack_declined += 1,
        }
    }
    Ok(())
}

/// The export's frames, as `ExportJob` makes them: the encoder for the
/// check's size is asked whether a frame can be converted on the GPU
/// (`gpu_yuv420_params`), and a frame goes that way when the renderer takes
/// it (`render_yuv420_on_gpu`: the encoder's planes, `-export-yuv`, grey,
/// the planes one after another); otherwise it is rendered and put through
/// the encoder's display transform (`export_display`: `-export`, RGBA),
/// and a miss after the work turns the GPU way off for the rest, as the job
/// does. `probe` is where the encoder opens; it is removed again.
fn export_frames(snapshot: &SequenceSnapshot, frames: &[u64], h: u32, seq_dir: &Path, probe: &Path, done: &mut Rendered) -> Result<(), String> {
    let mut out = settings();
    out.resolution = Some(klipp_core::Resolution { width: WIDTH, height: h });
    let opened = klipp_render::Encoder::open(probe, &out, snapshot).map(|e| e.gpu_yuv420_params());
    let _ = std::fs::remove_file(probe);
    let mut target = opened.map_err(|e| format!("opening the export's encoder: {e}"))?;
    let display = klipp_render::encoder::export_display(&out, snapshot.working_colorspace);
    let mut r = renderer(snapshot, true);
    let mut source = ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
    for &f in frames {
        let time = Timecode::from_frames(f as i64, snapshot.fps_num);
        let opts = klipp_compose::RenderOptions::default();
        let planes = target.and_then(|t| r.render_yuv420_on_gpu(&snapshot.video.sequence, time, WIDTH, h, &mut source, opts, t).map(|(p, _)| (p, t)));
        if let Some((planes, t)) = planes {
            source.end_frame().map_err(|e| format!("frame {f} (export): {e}"))?;
            if !planes.len().is_multiple_of(WIDTH as usize) {
                return Err(format!("frame {f} (export): {} bytes of planes don't fill rows of {WIDTH}", planes.len()));
            }
            let rows = (planes.len() / WIDTH as usize) as u32;
            image::save_buffer(seq_dir.join(format!("{f}-export-yuv.png")), &planes, WIDTH, rows, image::ExtendedColorType::L8).map_err(|e| e.to_string())?;
            // Flat when its luma plane is one value.
            let luma = &planes[..(WIDTH * h) as usize * if t.bits > 8 { 2 } else { 1 }];
            done.frames += 1;
            done.flat += usize::from(luma.chunks_exact(luma.len() / (WIDTH * h) as usize).all(|v| v == &luma[..v.len()]));
            done.export_gpu += 1;
            continue;
        }
        if target.is_some() && r.gpu_export_declined_after_work() {
            target = None;
        }
        let linear = r.render(&snapshot.video.sequence, time, WIDTH, h, &mut source, opts);
        source.end_frame().map_err(|e| format!("frame {f} (export): {e}"))?;
        let px = display.frame(&linear, |v, n| klipp_color::display::quantise(v, n, 255.0) as u8);
        image::save_buffer(seq_dir.join(format!("{f}-export.png")), &px, WIDTH, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
        done.frames += 1;
        done.flat += usize::from(is_flat(&px));
        done.export_cpu += 1;
    }
    Ok(())
}

/// The worst difference and the count of samples over one level, between
/// two PNGs; alpha is ignored.
fn differ(a: &Path, b: &Path) -> Result<(i32, usize), String> {
    let (x, y) = (image::open(a).map_err(|e| e.to_string())?.to_rgba8(), image::open(b).map_err(|e| e.to_string())?.to_rgba8());
    if x.dimensions() != y.dimensions() {
        return Err("sizes differ".into());
    }
    let mut worst = 0;
    let mut over = 0;
    for (i, (p, q)) in x.as_raw().iter().zip(y.as_raw()).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        let d = (*p as i32 - *q as i32).abs();
        worst = worst.max(d);
        over += usize::from(d > 1);
    }
    Ok((worst, over))
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
    let (mut stack_drawn, mut stack_declined, mut export_gpu, mut export_cpu) = (0, 0, 0, 0);
    let mut index = Vec::new();
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
        export_gpu += r.export_gpu;
        export_cpu += r.export_cpu;
        index.push(json!({
            "project": i, "path": dir, "grade_on_gpu": r.grade_on_gpu, "frames": r.frames, "flat": r.flat, "offline": r.offline, "missing": r.missing, "covered": r.covered(),
            "stack": r.stack, "stack_declined": r.stack_declined, "export_gpu": r.export_gpu, "export_cpu": r.export_cpu,
        }));
    }
    let text = serde_json::to_string_pretty(&json!({ "projects": index, "failed": failed })).unwrap_or_default();
    if let Err(e) = std::fs::create_dir_all(&out).and_then(|()| std::fs::write(out.join("index.json"), text)) {
        eprintln!("pixel_check: writing index.json: {e}");
        return 1;
    }
    println!(
        "{mode}: {rendered} projects, {frames} frames ({graded_on_gpu} projects with a grade run on the GPU for their -gpu frames), {} failed, in {:.0} s",
        failed.len(),
        started.elapsed().as_secs_f64()
    );
    println!("paths: GPU stack drew {stack_drawn} frames and declined {stack_declined}; export converted {export_gpu} on the GPU and {export_cpu} on the CPU");
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
        for f in files(&rec) {
            match differ(&rec.join(&f), &out.join(&f)) {
                Ok((worst, _)) if worst <= 1 => same += 1,
                Ok(_) => different += 1,
                Err(_) => missing += 1,
            }
        }
        println!("compared: {same} frames alike, {different} differ by more than a level, {missing} missing or unreadable");
        if different + missing > 0 {
            code = 1;
        }
    }
    code
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
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
        use klipp_core::{Fps, MediaId, Resolution};
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let clip = src.join(format!("{name}.avi"));
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x36:rate=25:duration=1", "-c:v", "ffv1"])
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
            resolution: Resolution { width: 64, height: 36 },
            ..klipp_media::MediaItem::new_synthetic_title(id, format!("{name}.avi"), Timecode::from_frames(25, 25), fps)
        });
        let mut seq = klipp_timeline::Sequence::new_default();
        (seq.fps, seq.resolution) = (fps, Resolution { width: 64, height: 36 });
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
        assert_eq!((img.width(), img.height()), (WIDTH, 180 * 3 / 2));
        assert_eq!(run_in(&args(&[Path::new("compare"), &list, &root.join("rec"), &root.join("cmp")]), &root), 0);
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
