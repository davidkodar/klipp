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
//! the last frame), at 320 pixels wide, as display-referred RGBA PNGs named
//! `<project #>/<sequence #>/<frame>-{cpu,gpu}.png`, with `index.json`
//! naming the projects.

use std::path::{Path, PathBuf};

use klipp_core::{ExportSettings, Timecode, WorkingColorspace};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::ExportFrameSource;
use serde_json::{json, Value};

const WIDTH: u32 = 320;

fn projects(list: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(list).expect("the project list");
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => v["projects"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).map(PathBuf::from).collect(),
        Err(_) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect(),
    }
}

/// A temporary copy of a project: its JSON files copied, everything else
/// linked, so nothing of the original can be written.
fn temp_copy(dir: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let (from, dest) = (e.path(), to.join(e.file_name()));
        let kind = e.file_type()?;
        if kind.is_dir() && !kind.is_symlink() && (e.file_name() == "media" || e.file_name() == "looks") {
            temp_copy(&from, &dest)?;
        } else if from.extension().is_some_and(|x| x == "json") && kind.is_file() {
            std::fs::copy(&from, &dest)?;
        } else {
            std::os::unix::fs::symlink(&from, &dest)?;
        }
    }
    Ok(())
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

/// Render project `dir`'s frames into `out`; the frames written.
fn render_project(dir: &Path, out: &Path) -> Result<usize, String> {
    let tmp = std::env::temp_dir().join(format!("klipp-pixel-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    temp_copy(dir, &tmp).map_err(|e| format!("copying: {e}"))?;
    let loaded = klipp_project::load_project(&tmp);
    let result = loaded.map_err(|e| format!("loading: {e}")).and_then(|(sequences, pool, colour, ..)| {
        let mut written = 0;
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
                    let px = display(&linear, snapshot.working_colorspace);
                    image::save_buffer(seq_dir.join(format!("{f}-{name}.png")), &px, WIDTH, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
                    written += 1;
                }
            }
        }
        Ok(written)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, list, out, recorded) = match args.as_slice() {
        [m, l, o] if m == "record" => ("record", l, PathBuf::from(o), None),
        [m, l, r, o] if m == "compare" => ("compare", l, PathBuf::from(o), Some(PathBuf::from(r))),
        _ => {
            eprintln!("usage: pixel_check record <projects> <out dir> | compare <projects> <recorded dir> <out dir>");
            std::process::exit(2);
        }
    };
    let started = std::time::Instant::now();
    let list = projects(Path::new(list));
    let (mut rendered, mut failed, mut frames) = (0, Vec::new(), 0);
    let mut index = Vec::new();
    for (i, dir) in list.iter().enumerate() {
        match render_project(dir, &out.join(format!("{i}"))) {
            Ok(n) => {
                rendered += 1;
                frames += n;
            }
            Err(e) => failed.push(json!({ "project": i, "why": e })),
        }
        index.push(json!({ "project": i, "path": dir }));
    }
    let _ = std::fs::write(out.join("index.json"), serde_json::to_string_pretty(&json!({ "projects": index, "failed": failed })).unwrap_or_default());
    println!("{mode}: {rendered} projects, {frames} frames, {} failed, in {:.0} s", failed.len(), started.elapsed().as_secs_f64());
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
            std::process::exit(1);
        }
    }
}
