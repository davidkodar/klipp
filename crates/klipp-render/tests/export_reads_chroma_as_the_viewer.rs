//! The export's software decode reads a file's colour as the viewer's
//! does (#3266): the same frame of a 4:2:0, 4:2:2 or 4:4:4 file with hard
//! colour edges, through `ExportFrameSource` and through the reader the
//! viewer's software decode converts with (`yuv_to_linear_display`), is
//! the same picture within 2 levels. swscale converting the export's
//! frames put the edges up to 255 levels off, and the frame cache and
//! frozen tracks draw through the export's frames.
//!
//! Sources are testsrc2's bars in FFV1, which NVDEC cannot take, so both
//! sides decode in software. Generated with the ffmpeg CLI; the test
//! skips without it.

use std::collections::HashMap;
use std::path::PathBuf;

use klipp_compose::{FrameSource, SourcePixels};
use klipp_core::*;
use klipp_render::*;

const W: u32 = 320;
const H: u32 = 180;
const FPS: u32 = 25;

/// testsrc2's bars, one second, in `pix_fmt`, written once per format
/// and process (#1700): the tests here run at once, and `ffmpeg -y`
/// truncating a file another is reading gives that one black frames.
fn bars(pix_fmt: &str, filter: &str) -> Option<PathBuf> {
    static WRITTEN: std::sync::Mutex<Vec<(String, Option<PathBuf>)>> = std::sync::Mutex::new(Vec::new());
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, path)) = written.iter().find(|(f, _)| f == pix_fmt) {
        return path.clone();
    }
    let path = klipp_testing::scratch::scratch_file("klipp-export-chroma", &format!("bars-{pix_fmt}.mkv"));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=s={W}x{H}:r={FPS}:d=1{filter}"))
        .args(["-c:v", "ffv1", "-pix_fmt", pix_fmt])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    let path = ok.then_some(path);
    written.push((pix_fmt.to_string(), path.clone()));
    path
}

/// Linear light to 8-bit sRGB levels, the scale the issue measured in.
fn levels(linear: &[f32]) -> Vec<i32> {
    linear
        .chunks_exact(4)
        .flat_map(|p| {
            let enc = |v: f32| {
                let v = v.clamp(0.0, 1.0);
                let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
                (e * 255.0).round() as i32
            };
            [enc(p[0]), enc(p[1]), enc(p[2]), (p[3].clamp(0.0, 1.0) * 255.0).round() as i32]
        })
        .collect()
}

/// The export's frame of the file at `secs`, at `w`×`h`.
fn exported(path: &std::path::Path, secs: f64, w: u32, h: u32) -> Vec<f32> {
    let id = MediaId::new();
    let media = HashMap::from([(id, MediaFile { path: path.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(1.0), ..Default::default() })]);
    let tc = |s: f64| RationalTime::from_secs_f64(s, FPS);
    let clip = klipp_timeline::Clip::media(id, tc(0.0), tc(1.0), tc(0.0), tc(1.0));
    let mut source = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let frame = source.frame(&clip, secs, w, h).expect("the export's frame");
    assert_eq!((frame.width, frame.height), (w, h));
    match frame.pixels {
        SourcePixels::Linear(px) => px.to_vec(),
        // A CPU frame offered to the grade on the GPU reads back as is.
        SourcePixels::Gpu(g) => g.read().to_vec(),
        SourcePixels::SceneReferred { .. } => panic!("a display-referred source"),
    }
}

/// What the viewer's software decode makes of the same frame.
fn viewed(path: &std::path::Path, secs: f64) -> Vec<f32> {
    let (px, w, h) = klipp_media::frame_reader::MediaReaders::new(1).read(path, None, WorkingColorspace::Rec709Linear, secs).expect("the viewer's frame");
    assert_eq!((w, h), (W, H));
    px
}

/// The largest difference in levels, and the share of samples over 2.
fn apart(a: &[i32], b: &[i32]) -> (i32, f64) {
    let diffs: Vec<i32> = a.iter().zip(b).map(|(x, y)| (x - y).abs()).collect();
    let over = diffs.iter().filter(|&&d| d > 2).count();
    (diffs.iter().copied().max().unwrap_or(0), over as f64 / diffs.len() as f64)
}

#[test]
fn the_exports_frame_has_the_viewers_chroma_at_hard_edges() {
    let secs = 0.4;
    for fmt in ["yuv420p", "yuv422p", "yuv444p"] {
        let Some(path) = bars(fmt, "") else {
            eprintln!("skipping: ffmpeg could not write the source");
            return;
        };
        let (viewer, export) = (levels(&viewed(&path, secs)), levels(&exported(&path, secs, W, H)));
        let (worst, share) = apart(&viewer, &export);
        eprintln!("{fmt}: worst {worst} levels, {:.2}% of samples over 2", share * 100.0);
        assert!(worst <= 2, "{fmt}: the export's frame is up to {worst} levels from the viewer's ({:.2}% of samples over 2)", share * 100.0);
    }
}

/// A file's alpha plane comes through the export's own conversion, as it
/// came through swscale's (#3283).
#[test]
fn the_exports_frame_keeps_a_files_alpha() {
    let Some(path) = bars("yuva444p", ",format=yuva444p,colorchannelmixer=aa=0.5") else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let export = levels(&exported(&path, 0.4, W, H));
    let alphas: Vec<i32> = export.iter().skip(3).step_by(4).copied().collect();
    assert!(alphas.iter().all(|&a| (a - 128).abs() <= 1), "half transparent throughout: {:?}", &alphas[..8]);
}

/// At another size the export resizes its own conversion, growing it
/// (swscale's Lanczos) or shrinking it (the decode shaders' footprint
/// average): the frame is that size, and its colour on average the same
/// frame's.
#[test]
fn a_resized_exports_frame_is_the_same_picture() {
    let Some(path) = bars("yuv420p", "") else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let mean = |px: &[i32]| -> [f64; 3] {
        let n = (px.len() / 4) as f64;
        let mut m = [0.0; 3];
        for p in px.chunks_exact(4) {
            for c in 0..3 {
                m[c] += p[c] as f64 / n;
            }
        }
        m
    };
    let at_size = mean(&levels(&exported(&path, 0.4, W, H)));
    for (w, h) in [(W * 2, H * 2), (W / 2, H / 2)] {
        let resized = mean(&levels(&exported(&path, 0.4, w, h)));
        for c in 0..3 {
            assert!((at_size[c] - resized[c]).abs() < 1.5, "channel {c}: {} at size, {} at {w}x{h}", at_size[c], resized[c]);
        }
    }
}
