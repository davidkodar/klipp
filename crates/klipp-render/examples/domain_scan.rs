//! Render each project's sequences on the CPU at a few times and list
//! every colour-domain refusal (#1867): which node, in which clip, and why.
//! Each project is read from a scratch copy (its JSON copied, everything
//! else linked, stored paths made absolute against the original), never
//! opened in place.
//!
//! ```text
//! cargo run --release -p klipp-render --example domain_scan -- <scratch dir> <project>...
//! ```

use std::path::Path;

use klipp_core::Timecode;
use klipp_render::snapshot::SequenceSnapshot;
use serde_json::Value;

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


fn main() {
    let args: Vec<String> = std::env::args().collect();
    let scratch = Path::new(&args[1]);
    let (mut projects, mut refusals, mut offline_total) = (0, 0, 0);
    for (i, dir) in args[2..].iter().enumerate() {
        let dir = Path::new(dir);
        let tmp = scratch.join(format!("domain-scan-{i}"));
        let _ = std::fs::remove_dir_all(&tmp);
        let loaded = temp_copy(dir, &tmp)
            .map_err(|e| format!("copying: {e}"))
            .and_then(|()| resolve_against(&tmp, dir))
            .and_then(|()| klipp_project::load_project(&tmp).map_err(|e| format!("loading: {e}")));
        let (sequences, pool, colour, ..) = match loaded {
            Ok(l) => l,
            Err(e) => {
                println!("{}: not read ({e})", dir.display());
                let _ = std::fs::remove_dir_all(&tmp);
                continue;
            }
        };
        projects += 1;
        let offline = pool.all().filter(|m| m.offline).count();
        offline_total += offline;
        let mut found = Vec::new();
        let mut frames = 0;
        for seq in &sequences {
            let lut = colour.global_lut_path.clone().filter(|_| colour.global_lut_enabled);
            let Ok(snapshot) = SequenceSnapshot::from_sequence_with_all(seq, &sequences, &pool, &klipp_render::pixel_check::settings(), lut, false, colour.working_colorspace) else { continue };
            let s = &snapshot.video.sequence;
            let end = s.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_out.to_secs_f64()).fold(0.0, f64::max);
            if end <= 0.0 {
                continue;
            }
            let mut r = klipp_render::export_frames::export_renderer(&snapshot, "domain scan");
            r.set_gpu_enabled(false);
            let mut src = klipp_render::ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
            let w = 320u32;
            let h = ((w as u64 * s.resolution.height as u64 / s.resolution.width.max(1) as u64) as u32).max(2) & !1;
            for k in 0..5 {
                let t = end * f64::from(k) / 5.0 + 0.01;
                let time = Timecode::from_secs_f64(t, s.fps.rate());
                let (_, inspection) = r.render_inspected(s, time, w, h, &mut src, klipp_compose::RenderOptions::default(), klipp_compose::Inspect::default());
                let _ = src.end_frame();
                frames += 1;
                for d in inspection.declines.iter().filter(|d| d.reason.contains("colour domains")) {
                    found.push(format!("  {} at {t:.2} s: clip {:?} node {} ({}): {}", seq.name, d.clip.map(|c| c.0), d.node, d.name, d.why));
                }
            }
        }
        refusals += found.len();
        println!("{}: {} sequence(s), {frames} frames, {offline} offline media, {} colour-domain refusal(s)", dir.display(), sequences.len(), found.len());
        for f in &found {
            println!("{f}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    println!("{projects} projects read, {refusals} colour-domain refusals, {offline_total} offline media in all");
}
