//! Level 2: the viewer against export, for `ViewerEqualsExport`.
//!
//! Level 1 ([`crate::route`]) feeds both routes known frames, so it can't
//! see where the viewer and export differ most: their frame sources
//! (decode, bit depth, the sRGB and linear hand-off). Here both read one
//! real file of known frames ([`known_media`], lossless ffv1):
//!
//! - **Viewer**: the real `PlaybackEngine`, headless, parked on the frame;
//!   what its frame buffer shows, and the declines stamped to that frame.
//! - **Export**: the export's own renderer and frame source
//!   (`export_frames::export_renderer`, `ExportFrameSource`), the frame it
//!   hands the encoder, before the encoder: every export codec is lossy,
//!   and a byte bar can't hold through one.
//!
//! The two are held to two levels of sRGB8 (the viewer may draw on the GPU
//! stack, export on the CPU) and to the same declines. A case earns both
//! routes' `ViewerEqualsExport` keys with one call.
//!
//! The scene's OFX folders reach both through the process's plug-in
//! search (`OfxHost::add_plugin_dirs`), as `OFX_PLUGIN_PATH` would.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::{AppCommand, AppEvent, ExportSettings, RationalTime, Resolution};
use klipp_engine::{FrameBuffer, PlaybackEngine};

use crate::route::Scene;

/// The most a viewer byte may differ from export's.
pub const BAR: u8 = 2;

/// A lossless file of `n` known frames at `fps`, `w × h`: frame `i` is
/// `frame(i)` (display-referred linear RGBA, encoded to sRGB8 as a decoder
/// gives it back). Written once per name under the build's scratch folder.
pub fn known_media(name: &str, w: u32, h: u32, fps: u32, n: i64, frame: &dyn Fn(i64) -> Vec<f32>) -> Result<PathBuf, String> {
    let dir = crate::fixtures::scratch().join("known-media");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("{name}-{w}x{h}x{n}.mkv"));
    if out.exists() {
        return Ok(out);
    }
    let mut raw = Vec::new();
    for i in 0..n {
        let px = frame(i);
        if px.len() != (w * h * 4) as usize {
            return Err(format!("{name}: frame {i} is {} values, not {w}×{h}×4", px.len()));
        }
        raw.extend(klipp_effects::linear_f32_rgba_to_srgb_u8(&px));
    }
    let partial = out.with_extension(format!("{}.part.mkv", std::process::id()));
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-r", &fps.to_string(), "-i", "-"])
        .args(["-c:v", "ffv1", "-pix_fmt", "rgba"])
        .arg(&partial)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg not found ({e}): level 2 writes its media with it"))?;
    {
        use std::io::Write;
        child.stdin.take().ok_or("ffmpeg's input")?.write_all(&raw).map_err(|e| e.to_string())?;
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err(format!("ffmpeg didn't write {}", out.display()));
    }
    std::fs::rename(&partial, &out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// One route's frame: sRGB8 RGBA bytes and its declines, each as
/// "node: outcome".
struct Shown {
    bytes: Vec<u8>,
    declines: BTreeSet<String>,
}

fn declines<'a>(d: impl IntoIterator<Item = &'a klipp_nodes::Decline>) -> BTreeSet<String> {
    d.into_iter().map(|d| format!("{}: {}", d.node, d.outcome.label())).collect()
}

/// The viewer's frames: the engine, headless, parked on each in turn.
fn viewer(scene: &Scene, frames: &[i64]) -> Result<Vec<Shown>, String> {
    let (w, h) = (scene.width, scene.height);
    let fps = scene.sequence.fps;
    let mut seq = scene.sequence.clone();
    seq.resolution = Resolution { width: w, height: h };
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(w, h)));
    let engine = PlaybackEngine::new(
        cmd_rx,
        ev_tx,
        Arc::clone(&fb),
        Arc::new(Mutex::new(FrameBuffer::new(w, h))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)),
        Arc::new(RwLock::new(scene.pool.clone())),
        Box::new(|| {}),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    let generation = |fb: &Mutex<FrameBuffer>| fb.lock().map_or(0, |f| f.latest_generation());
    let mut out = Vec::new();
    for &f in frames {
        let g = generation(&fb);
        let _ = cmd.send(AppCommand::SetPlayhead(RationalTime { value: f * fps.den as i64, rate: fps.num }));
        // A frame newer than the one before, then quiet on a complete
        // frame: a paused frame is drawn again as its streams arrive, and
        // the last is the one shown. A minute at the most each: a loaded
        // machine is slower, not wrong.
        let by = Instant::now() + Duration::from_secs(60);
        while generation(&fb) == g && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(2));
        }
        let (mut last, mut since) = (generation(&fb), Instant::now());
        while Instant::now() < by {
            std::thread::sleep(Duration::from_millis(10));
            let now = generation(&fb);
            if now != last {
                (last, since) = (now, Instant::now());
            } else if since.elapsed() > Duration::from_millis(300) && fb.lock().is_ok_and(|f| f.latest_complete()) {
                break;
            }
        }
        let fb = fb.lock().map_err(|_| "the frame buffer")?;
        let at = fb.latest_presented().map(|t| (t.to_secs_f64() * fps.as_f64()).round() as i64);
        if at != Some(f) {
            return Err(format!("the viewer shows frame {at:?}, parked on {f}"));
        }
        let (bytes, fw, fh, _) = fb.latest_frame();
        if (fw, fh) != (w, h) {
            return Err(format!("the viewer's frame {f} is {fw}×{fh}, not {w}×{h}"));
        }
        out.push(Shown { bytes: bytes.to_vec(), declines: declines(fb.declines()) });
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    // A decode thread still in ffmpeg's teardown at exit crashes the
    // process.
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(60));
    Ok(out)
}

/// Export's frames: its renderer and frame source, before the encoder.
fn export(scene: &Scene, frames: &[i64]) -> Result<Vec<Shown>, String> {
    let (w, h) = (scene.width, scene.height);
    let settings = ExportSettings { output_path: PathBuf::from("/dev/null"), resolution: Some(Resolution { width: w, height: h }), ..ExportSettings::default() };
    let seq = &scene.sequence;
    let snapshot = klipp_render::snapshot::SequenceSnapshot::from_sequence_with_all(seq, std::slice::from_ref(seq), &scene.pool, &settings, None, false, klipp_core::WorkingColorspace::default())
        .map_err(|e| format!("export's snapshot: {e:?}"))?;
    let mut renderer = klipp_render::export_frames::export_renderer(&snapshot, "readiness level 2");
    let mut source = klipp_render::ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
    let mut out = Vec::new();
    for &f in frames {
        let time = RationalTime { value: f * seq.fps.den as i64, rate: seq.fps.num };
        let (linear, found) = renderer.render_inspected(&snapshot.video.sequence, time, w, h, &mut source, klipp_compose::RenderOptions::default(), Default::default());
        source.end_frame().map_err(|e| format!("export's frame {f}: {e}"))?;
        out.push(Shown { bytes: klipp_effects::linear_f32_rgba_to_srgb_u8(&linear), declines: declines(&found.declines) });
    }
    Ok(out)
}

/// The viewer shows export's frames: each of `frames` within [`BAR`] in
/// every channel, with the same declines.
pub fn viewer_equals_export(scene: Arc<Scene>, frames: &[i64]) -> Result<(), String> {
    klipp_plugin::OfxHost::add_plugin_dirs(&scene.plugin_dirs);
    let v = viewer(&scene, frames)?;
    let e = export(&scene, frames)?;
    alike(frames, &v, &e)
}

/// Each of `frames`' viewer frame within [`BAR`] of export's, with the
/// same declines.
fn alike(frames: &[i64], v: &[Shown], e: &[Shown]) -> Result<(), String> {
    for ((f, v), e) in frames.iter().zip(v).zip(e) {
        if v.bytes.len() != e.bytes.len() {
            return Err(format!("frame {f}: the viewer's {} bytes, export's {}", v.bytes.len(), e.bytes.len()));
        }
        let off = v.bytes.iter().zip(&e.bytes).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        if off > BAR {
            return Err(format!("frame {f}: the viewer is {off} levels from export"));
        }
        if v.declines != e.declines {
            return Err(format!("frame {f}: the viewer declines {:?}, export {:?}", v.declines, e.declines));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, Sequence};

    const FPS: u32 = 25;

    /// A clip of a file whose every frame is another grey, 32 × 16.
    fn scene() -> Arc<Scene> {
        let (w, h) = (32, 16);
        let path = known_media("level2-greys", w, h, FPS, 50, &|i| [i as f32 / 60.0, 0.5, 1.0 - i as f32 / 60.0, 1.0].repeat((w * h) as usize)).expect("the known media");
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.kind = klipp_media::MediaItemKind::File;
        item.duration = RationalTime { value: 50, rate: FPS };
        item.fps = klipp_core::Fps::new(FPS, 1);
        item.resolution = Resolution { width: w, height: h };
        let media = item.id;
        pool.add(item);
        let tc = |f: i64| RationalTime { value: f, rate: FPS };
        let mut sequence = Sequence::new_default();
        sequence.fps = klipp_core::Fps::new(FPS, 1);
        let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v1.clips = vec![Clip::media(media, tc(0), tc(50), tc(0), tc(50))];
        sequence.tracks = vec![v1];
        Arc::new(Scene { sequence, pool, width: w, height: h, plugin_dirs: Vec::new() })
    }

    /// A plain clip: the viewer shows export's frames. And the control:
    /// the viewer's frame held to another of export's fails, so the
    /// comparison can fail.
    #[test]
    fn the_viewer_shows_exports_frame_and_another_frame_fails() {
        let s = scene();
        viewer_equals_export(Arc::clone(&s), &[10, 30]).expect("a plain clip");
        let v = viewer(&s, &[10]).expect("the viewer");
        let e = export(&s, &[30]).expect("export");
        assert!(alike(&[10], &v, &e).is_err(), "frame 10 held to frame 30 passed");
    }
}
