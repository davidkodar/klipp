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
/// those it gives at twice the size.
struct Ramp {
    missing: Vec<i64>,
    large: Vec<i64>,
}

impl Source for Ramp {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = shown(source_secs);
        if !(0..MEDIA_FRAMES).contains(&n) || self.missing.contains(&n) {
            return None;
        }
        let (w, h) = if self.large.contains(&n) { (W * 2, H * 2) } else { (W, H) };
        let v = grey(n);
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
        let key = |time_secs: f64, value: f32| Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) };
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
    let current = grey(n);
    let blends = blend_mode == 1 && !clip.is_reversed() && clip.speed_at(elapsed).abs() < 1.0 - 1e-6;
    let next = source.frame(MediaId::nil(), (n + 1) as f64 / FPS as f64);
    let weight = (at * FPS as f64 - n as f64).clamp(0.0, 1.0) as f32;
    match next {
        Some(_) if blends && weight > 1e-4 => flat(current + (grey(n + 1) - current) * weight),
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

pub fn run(report: &mut Report) {
    let whole = || Ramp { missing: vec![], large: vec![] };
    for route in [Route::Viewer, Route::Export] {
        let at = |o| [key(Some(route), o)];
        // At half speed, every other frame falls halfway between two media
        // frames.
        report.case(&format!("blend {route:?}: half speed"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[1, 2, 41], 1)
        });
        report.case(&format!("blend {route:?}: the CPU and the GPU agree"), at(Obligation::CpuGpuAgree), || {
            holds(route, false, scene(50, 100, Run::At(0.5), 1), whole(), &[41], 1)?;
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[41], 1)
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
            holds(route, true, scene(MEDIA_FRAMES - 10, 20, Run::At(0.5), 1), whole(), &[19], 1)
        });
        report.case(&format!("blend {route:?}: a seek back"), at(Obligation::Time(TimeCase::BackwardSeek)), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), whole(), &[61, 62, 21, 22], 1)
        });
        report.case(&format!("blend {route:?}: blend mode 0"), at(Obligation::Outcome(Outcome::Bypass)), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 0), whole(), &[1, 41], 0)
        });
        report.case(&format!("blend {route:?}: no next frame"), at(Obligation::Outcome(Outcome::NeighbourMissing)), || {
            // Frame 41 at half speed shows media frame 70, between 70 and 71.
            holds(route, true, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![71], large: vec![] }, &[41], 1)
        });
        // A next frame of another size is fitted to the render size, as the
        // current one is, and blended: a source whose frames change size
        // doesn't stutter (ruling 10-09). A flat grey fits to itself.
        report.case(&format!("blend {route:?}: a next frame of another size"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 100, Run::At(0.5), 1), Ramp { missing: vec![], large: vec![71] }, &[41], 1)
        });
    }
}
