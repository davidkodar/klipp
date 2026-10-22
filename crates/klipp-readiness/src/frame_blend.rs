//! The FrameBlend family's cases (M4 step 7): a slowed clip's frame
//! blending, held to the mix its source frames give.
//!
//! The source is a ramp of flat greys on a frame grid, media frame n at
//! 0.3 + 0.01·n with its presentation time n / fps: asked for a time, it
//! gives the frame shown then. A clip blends when its Speed Ramp is in
//! blend mode 1, it isn't reversed, and it runs below real time there: the
//! frame at the mapped source time is mixed toward the next by how far the
//! time is between their presentation times. The mapped time comes from
//! the clip's own speed curve (`Clip::source_secs_at`), which source
//! evaluation keeps; the cases keep the Speed Ramp's speed equal to the
//! curve, so the retime's authored value (#1409) doesn't change them.
//!
//! Blend mode 2 follows the motion between the two frames by optical
//! flow (#2215). A flat grey has no motion to follow: its frames warp to
//! themselves, so the in-between frame is the same cross-fade, and the
//! cases hold mode 2 to it. Where the two frames are unlike (the ramp's
//! step), the motion can't be followed and the whole frame cross-fades,
//! declined as a fallback. How well it follows real motion is held by
//! `klipp_effects::flow`'s own tests.

use std::sync::Arc;

use klipp_core::{Keyframe, MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, Key, Obligation, Outcome, Report, Route, TimeCase};

const SUBJECT: &str = klipp_graph_format::lifted::types::FRAME_BLEND;

const FPS: u32 = 25;
const W: u32 = 16;
const H: u32 = 8;
const MEDIA_FRAMES: i64 = 250;

fn key(route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::FrameBlend, subject: SUBJECT.into(), entry: Entry::Clip, route, obligation }
}

fn grey(frame: i64) -> f32 {
    0.3 + 0.01 * frame as f32
}

fn flat(v: f32) -> Vec<f32> {
    [v, v, v, 1.0].repeat((W * H) as usize)
}

/// The media frame shown at `source_secs`.
fn shown(source_secs: f64) -> i64 {
    (source_secs * FPS as f64 + 1e-9).floor() as i64
}

/// The ramp on its frame grid, with the media frames it has none of, and
/// those it gives at twice the size; from media frame `step` on, a jump of
/// 0.3, so a blend between the wrong two frames shows.
struct Ramp {
    missing: Vec<i64>,
    large: Vec<i64>,
    step: Option<i64>,
}

impl Ramp {
    /// Media frame `n`'s grey.
    fn level(&self, n: i64) -> f32 {
        grey(n) + if self.step.is_some_and(|s| n >= s) { 0.3 } else { 0.0 }
    }
}

impl Source for Ramp {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = shown(source_secs);
        if !(0..MEDIA_FRAMES).contains(&n) || self.missing.contains(&n) {
            return None;
        }
        let (w, h) = if self.large.contains(&n) { (W * 2, H * 2) } else { (W, H) };
        let v = self.level(n);
        Some(KnownFrame { linear: [v, v, v, 1.0].repeat((w * h) as usize), width: w, height: h, pts: Some(n as f64 / FPS as f64) })
    }
}

/// How the clip runs.
#[derive(Clone, Copy)]
enum Run {
    /// At a constant `speed`.
    At(f64),
    /// From `from` to `to` across the clip, by its speed curve.
    Ramped(f64, f64),
}

/// A scene of one media clip of the ramp, its source from media frame
/// `from`, `frames` long on the timeline, running `run`, with a Speed Ramp
/// in `blend_mode`.
fn scene(from: i64, frames: i64, run: Run, blend_mode: u32) -> (Arc<Scene>, Clip) {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/ramp.mov".into(), None);
    item.duration = RationalTime { value: MEDIA_FRAMES, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    let media = item.id;
    pool.add(item);
    let tc = |f: i64| RationalTime { value: f, rate: FPS };
    let (speed, mean) = match run {
        Run::At(s) => (s, s.abs()),
        Run::Ramped(a, b) => (a, (a + b) / 2.0),
    };
    // The source the clip's speed covers, from its in point.
    let covered = (frames as f64 * mean).round() as i64;
    let mut clip = Clip::media(media, tc(0), tc(frames), tc(from), tc(from + covered));
    clip.speed = speed;
    if let Run::Ramped(a, b) = run {
        let key = |time_secs: f64, value: f32| Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
        clip.speed_keyframes = vec![key(0.0, a as f32), key(frames as f64 / FPS as f64, b as f32)];
    }
    clip.set_effect_list(
        vec![BuiltinEffect::SpeedRamp { speed: speed.abs() as f32, blend_mode }],
        vec![true],
    );
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v1.clips = vec![clip.clone()];
    sequence.tracks = vec![v1];
    (Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: Vec::new() }), clip)
}

/// What timeline frame `frame` of `clip` must be: the media frame shown at
/// its source time, mixed toward the next one the source has when the clip
/// blends there, by where the time falls between them.
fn expected(clip: &Clip, source: &Ramp, frame: i64, blend_mode: u32) -> Vec<f32> {
    let elapsed = frame as f64 / FPS as f64;
    let at = clip.source_secs_at(elapsed, FPS as f64);
    let n = shown(at);
    let current = source.level(n);
    let blends = matches!(blend_mode, 1 | 2) && !clip.is_reversed() && clip.speed_at(elapsed).abs() < 1.0 - 1e-6;
    let next = source.frame(MediaId::nil(), (n + 1) as f64 / FPS as f64);
    let weight = (at * FPS as f64 - n as f64).clamp(0.0, 1.0) as f32;
    match next {
        Some(_) if blends && weight > 1e-4 => flat(current + (source.level(n + 1) - current) * weight),
        _ => flat(current),
    }
}

/// `frames` of the scene on `route`, each held to its expectation, in the
/// order given.
fn holds(route: Route, gpu: bool, scene: (Arc<Scene>, Clip), source: Ramp, frames: &[i64], blend_mode: u32) -> Result<(), String> {
    let (scene, clip) = scene;
    let source = Arc::new(source);
    let mut s = Session::new(route, gpu, scene, Arc::clone(&source) as Arc<dyn Source>);
    for &f in frames {
        let got = s.frame(f);
        // The viewer's GPU stack draws display bytes only: within a byte.
        let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-4 };
        let off = got.off(&expected(&clip, &source, f, blend_mode));
        if off > tolerance {
            return Err(format!("frame {f}: {off} from the expected blend"));
        }
    }
    Ok(())
}

/// Frame `frame` of the scene on `route` held to what it declines (M4
/// step 10): nothing, or one decline with the outcome labelled `label`.
fn declines(route: Route, scene: (Arc<Scene>, Clip), source: Ramp, frame: i64, label: Option<&str>) -> Result<(), String> {
    let mut s = Session::new(route, true, scene.0, Arc::new(source) as Arc<dyn Source>);
    s.frame(frame).declined_as(label)
}

pub fn run(report: &mut Report) {
    let whole = || Ramp { missing: vec![], large: vec![], step: None };
    // The renderer's own frame blend is gone (step 7).
    report.case("blend: the old stage is gone", [key(None, Obligation::OldStageGone)], || crate::old_stage::gone(Family::FrameBlend));
    // A save and a strict reload keep the clip's blend: its Speed Ramp in
    // blend mode, its speed, its speed curve.
    report.case("blend: saved and reloaded", [key(None, Obligation::Saved)], || {
        for mode in [1, 2] {
            let (scene, clip) = scene(50, 100, Run::Ramped(0.25, 0.75), mode);
            let back = crate::saved::round_trip(&scene.sequence, &scene.pool)?;
            let got = back.tracks.first().and_then(|t| t.clips.first()).ok_or("no clip came back")?;
            let ramp = |c: &Clip| c.picture_effects().map(|fx| fx.into_iter().map(|e| (e.effect, e.enabled)).collect::<Vec<_>>());
            if ramp(got)? != ramp(&clip)? || got.speed != clip.speed || got.speed_keyframes != clip.speed_keyframes {
                return Err(format!("the blend in mode {mode} came back as {:?} at {} ({:?})", ramp(got)?, got.speed, got.speed_keyframes));
            }
        }
        Ok(())
    });
    // Level 2: the viewer shows export's frame, both read from one real
    // file of the ramp, cross-faded and followed by optical flow (#2215).
    report.case("blend: the viewer shows export's frame", [key(Some(Route::Viewer), Obligation::ViewerEqualsExport), key(Some(Route::Export), Obligation::ViewerEqualsExport)], || {
        for mode in [1, 2] {
            let (s, _) = scene(50, 100, Run::At(0.5), mode);
            let s = crate::on_file::on_file_frames(&s, "frame-blend-ramp", FPS, MEDIA_FRAMES, &|n| flat(grey(n)))?;
            crate::route2::viewer_equals_export(s, &[40, 41]).map_err(|e| format!("blend mode {mode}: {e}"))?;
        }
        Ok(())
    });
    for route in [Route::Viewer, Route::Export] {
        let at = |o| [key(Some(route), o)];
        // At half speed, every other frame falls halfway between two media
        // frames.
        report.case(&format!("blend {route:?}: half speed"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[1, 2, 41], 1)?;
            // The next frame read: nothing declined, between frames or on one.
            declines(route, scene(50, 100, Run::At(0.5), 1), whole(), 41, None)?;
            declines(route, scene(50, 100, Run::At(0.5), 1), whole(), 2, None)
        });
        report.case(&format!("blend {route:?}: the CPU and the GPU agree"), at(Obligation::CpuGpuAgree), || {
            holds(route, false, scene(50, 100, Run::At(0.5), 1), whole(), &[41], 1)?;
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[41], 1)
        });
        // Optical flow (#2215): a flat grey warps to itself, so the frames
        // are the cross-fade's, on the GPU and, without one, cross-faded.
        report.case(&format!("blend {route:?}: optical flow at half speed"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 2), whole(), &[1, 2, 41], 2)?;
            holds(route, true, scene(50, 100, Run::Ramped(0.25, 0.75), 2), whole(), &[3, 37, 70, 98], 2)?;
            declines(route, scene(50, 100, Run::At(0.5), 2), whole(), 41, None)
        });
        report.case(&format!("blend {route:?}: optical flow on the CPU and the GPU"), at(Obligation::CpuGpuAgree), || {
            holds(route, false, scene(50, 100, Run::At(0.5), 2), whole(), &[41], 2)?;
            holds(route, true, scene(50, 100, Run::At(0.5), 2), whole(), &[41], 2)
        });
        // Two frames unlike each other, the ramp's step: the motion can't
        // be followed, the frame cross-fades whole and says so, as a
        // fallback an export still writes.
        report.case(&format!("blend {route:?}: optical flow it can't follow"), at(Obligation::ExpectedPixels), || {
            let step = || Ramp { missing: vec![], large: vec![], step: Some(71) };
            holds(route, true, scene(50, 100, Run::At(0.5), 2), step(), &[39, 40, 41, 42], 2)?;
            declines(route, scene(50, 100, Run::At(0.5), 2), step(), 41, Some("fallback"))?;
            declines(route, scene(50, 100, Run::At(0.5), 2), step(), 40, None)
        });
        report.case(&format!("blend {route:?}: a trimmed clip's first frame"), at(Obligation::Time(TimeCase::TrimmedStart)), || {
            holds(route, true, scene(50, 100, Run::At(0.4), 1), whole(), &[0, 1], 1)
        });
        // The last frame blends toward the media frame past the out point.
        report.case(&format!("blend {route:?}: a trimmed clip's last frame"), at(Obligation::Time(TimeCase::TrimmedEnd)), || {
            holds(route, true, scene(50, 100, Run::At(0.4), 1), whole(), &[99], 1)
        });
        // A reversed clip has no following frame to blend toward.
        report.case(&format!("blend {route:?}: a reversed clip"), at(Obligation::Time(TimeCase::Reversed)), || {
            holds(route, true, scene(50, 100, Run::At(-0.5), 1), whole(), &[0, 41, 99], 1)
        });
        report.case(&format!("blend {route:?}: a ramping speed"), at(Obligation::Time(TimeCase::SpeedRamped)), || {
            holds(route, true, scene(50, 100, Run::Ramped(0.25, 0.75), 1), whole(), &[3, 37, 70, 98], 1)
        });
        // At the media's last frame there is no next one: the frame as it is.
        report.case(&format!("blend {route:?}: the media's last frame"), at(Obligation::Time(TimeCase::OutOfRange)), || {
            holds(route, true, scene(MEDIA_FRAMES - 10, 20, Run::At(0.5), 1), whole(), &[19], 1)?;
            // No frame past the media to blend toward: a fallback, shown,
            // never refused by an export.
            declines(route, scene(MEDIA_FRAMES - 10, 20, Run::At(0.5), 1), whole(), 19, Some("fallback"))
        });
        report.case(&format!("blend {route:?}: a seek back"), at(Obligation::Time(TimeCase::BackwardSeek)), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[61, 62, 21, 22], 1)
        });
        report.case(&format!("blend {route:?}: blend mode 0"), at(Obligation::Outcome(Outcome::Bypass)), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 0), whole(), &[1, 41], 0)
        });
        // A blend whose run fails shows the frame it read, unblended, and
        // says so; an export refuses it.
        report.case(&format!("blend {route:?}: its render failed"), at(Obligation::Outcome(Outcome::RenderFailed)), || {
            let _fault = klipp_nodes::fault::fail("speed_ramp");
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[41], 0)?;
            declines(route, scene(50, 100, Run::At(0.5), 1), whole(), 41, Some("render failed"))
        });
        report.case(&format!("blend {route:?}: no next frame"), at(Obligation::Outcome(Outcome::NeighbourMissing)), || {
            // Frame 41 at half speed shows media frame 70, between 70 and 71.
            holds(route, true, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![71], large: vec![], step: None }, &[41], 1)?;
            // Declined as a frame missing, which an export refuses; on a
            // media frame's own time the next isn't needed, so nothing.
            declines(route, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![71], large: vec![], step: None }, 41, Some("neighbour frame missing"))?;
            declines(route, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![71], large: vec![], step: None }, 40, None)
        });
        // A time on a media frame's own shows that frame whole, a time
        // between two mixes those two: with a step in the source after
        // frame 70, a blend between the wrong pair shows (the frame before
        // mixed toward the frame after looks right on an even ramp alone).
        report.case(&format!("blend {route:?}: the frames either side of the time"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![], large: vec![], step: Some(71) }, &[39, 40, 41, 42], 1)
        });
        // A next frame of another size is fitted to the render size, as the
        // current one is, and blended: a source whose frames change size
        // doesn't stutter (ruling 10-09). A flat grey fits to itself.
        report.case(&format!("blend {route:?}: a next frame of another size"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![], large: vec![71], step: None }, &[41], 1)?;
            // Fitted and blended: nothing declined.
            declines(route, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![], large: vec![71], step: None }, 41, None)
        });
    }
}
