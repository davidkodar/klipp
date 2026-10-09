//! The TemporalDenoise family's cases (M4 step 6): a media clip's temporal
//! noise reduction, as its source-boundary node, held to the result its
//! source frames give.
//!
//! The source is a ramp of flat greys, frame n at 0.3 + 0.01·n, so the
//! denoiser's neighbours (a frame either side of the frame's source time)
//! are within the clip's threshold and averaged: the expectation is
//! `denoise_frame` over the frames the source has there. A neighbour is
//! read across the clip's in and out points (the media's handles), and is
//! left out only past the media itself, or where the source has none.

use std::sync::Arc;

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, Key, Obligation, Outcome, Report, Route, TimeCase};

/// The family's subject: the lifted node type.
const SUBJECT: &str = klipp_graph_format::lifted::types::TEMPORAL_DENOISE;

const FPS: u32 = 25;
const W: u32 = 16;
const H: u32 = 8;
/// The media: ten seconds.
const MEDIA_FRAMES: i64 = 250;
const STRENGTH: f32 = 1.0;
const THRESHOLD: f32 = 0.1;

fn key(route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::TemporalDenoise, subject: SUBJECT.into(), entry: Entry::Clip, route, obligation }
}

/// The grey of the media's frame at `source_secs`.
fn grey(source_secs: f64) -> f32 {
    0.3 + 0.01 * (source_secs * FPS as f64).round() as f32
}

fn flat(v: f32) -> Vec<f32> {
    [v, v, v, 1.0].repeat((W * H) as usize)
}

/// The ramp, with the source times it has no frame at, and those it gives
/// at another size.
struct Ramp {
    missing: Vec<f64>,
    wrong_size: Vec<f64>,
}

impl Ramp {
    fn whole() -> Self {
        Self { missing: vec![], wrong_size: vec![] }
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.5 / FPS as f64
}

impl Source for Ramp {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        if !(0.0..MEDIA_FRAMES as f64 / FPS as f64).contains(&source_secs) || self.missing.iter().any(|t| near(*t, source_secs)) {
            return None;
        }
        let (w, h) = if self.wrong_size.iter().any(|t| near(*t, source_secs)) { (W * 2, H * 2) } else { (W, H) };
        let v = grey(source_secs);
        Some(KnownFrame { linear: [v, v, v, 1.0].repeat((w * h) as usize), width: w, height: h, pts: None })
    }
}

/// A scene of one media clip of the ramp, its source from `from` to `to`
/// (media frames), at `speed`, with a denoiser of `strength`.
fn scene(from: i64, to: i64, speed: f64, strength: f32) -> (Arc<Scene>, Clip) {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/ramp.mov".into(), None);
    item.duration = RationalTime { value: MEDIA_FRAMES, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    let media = item.id;
    pool.add(item);
    let tc = |f: i64| RationalTime { value: f, rate: FPS };
    let length = ((to - from) as f64 / speed.abs()).round() as i64;
    let mut clip = Clip::media(media, tc(0), tc(length), tc(from), tc(to));
    clip.speed = speed;
    clip.set_effect_list(vec![BuiltinEffect::TemporalDenoise { strength, threshold: THRESHOLD }], vec![true]);
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v1.clips = vec![clip.clone()];
    sequence.tracks = vec![v1];
    (Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: Vec::new() }), clip)
}

/// What timeline frame `frame` of `clip` must be: its source frame,
/// denoised against the neighbours `source` has a frame either side of it.
fn expected(clip: &Clip, source: &Ramp, frame: i64, strength: f32) -> Vec<f32> {
    let at = clip.source_secs_at(frame as f64 / FPS as f64, FPS as f64);
    let dt = 1.0 / FPS as f64;
    let neighbours: Vec<Vec<f32>> = [at - dt, at + dt]
        .into_iter()
        .filter_map(|t| source.frame(MediaId::nil(), t))
        .filter(|f| (f.width, f.height) == (W, H))
        .map(|f| f.linear)
        .collect();
    let refs: Vec<&[f32]> = neighbours.iter().map(Vec::as_slice).collect();
    klipp_effects::temporal_denoise::denoise_frame(&flat(grey(at)), &refs, THRESHOLD, strength)
}

/// `frames` of the scene on `route`, the GPU `gpu`, each held to its
/// expectation, in the order given.
fn holds(route: Route, gpu: bool, scene: (Arc<Scene>, Clip), source: Ramp, frames: &[i64], strength: f32) -> Result<(), String> {
    let (scene, clip) = scene;
    let source = Arc::new(source);
    let mut s = Session::new(route, gpu, scene, Arc::clone(&source) as Arc<dyn Source>);
    for &f in frames {
        let got = s.frame(f);
        // The viewer's GPU stack draws display bytes only: within a byte.
        let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-4 };
        let off = got.off(&expected(&clip, &source, f, strength));
        if off > tolerance {
            return Err(format!("frame {f}: {off} from the expected denoise"));
        }
    }
    Ok(())
}

pub fn run(report: &mut Report) {
    for route in [Route::Viewer, Route::Export] {
        let at = |o| [key(Some(route), o)];
        report.case(&format!("denoise {route:?}: a frame mid-clip"), at(Obligation::ExpectedPixels), || {
            holds(route, true, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[40], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: the CPU and the GPU agree"), at(Obligation::CpuGpuAgree), || {
            holds(route, false, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[40], STRENGTH)?;
            holds(route, true, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[40], STRENGTH)
        });
        // The first frame of a trimmed clip reads the handle before its in
        // point; the last the one after its out point.
        report.case(&format!("denoise {route:?}: a trimmed clip's first frame"), at(Obligation::Time(TimeCase::TrimmedStart)), || {
            holds(route, true, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[0], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: a trimmed clip's last frame"), at(Obligation::Time(TimeCase::TrimmedEnd)), || {
            holds(route, true, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[99], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: a reversed clip"), at(Obligation::Time(TimeCase::Reversed)), || {
            holds(route, true, scene(50, 150, -1.0, STRENGTH), Ramp::whole(), &[0, 40, 99], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: a clip at twice the speed"), at(Obligation::Time(TimeCase::SpeedRamped)), || {
            holds(route, true, scene(50, 150, 2.0, STRENGTH), Ramp::whole(), &[0, 20, 49], STRENGTH)
        });
        // At the media's first frame the one before it is past the media:
        // the frame denoises against the one after only.
        report.case(&format!("denoise {route:?}: the media's first frame"), at(Obligation::Time(TimeCase::OutOfRange)), || {
            holds(route, true, scene(0, 100, 1.0, STRENGTH), Ramp::whole(), &[0], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: a seek back"), at(Obligation::Time(TimeCase::BackwardSeek)), || {
            holds(route, true, scene(50, 150, 1.0, STRENGTH), Ramp::whole(), &[60, 61, 20, 21], STRENGTH)
        });
        report.case(&format!("denoise {route:?}: at strength 0"), at(Obligation::Outcome(Outcome::Bypass)), || {
            holds(route, true, scene(50, 150, 1.0, 0.0), Ramp::whole(), &[40], 0.0)
        });
        // A neighbour the source hasn't, or has at another size, is left
        // out: one side, or the frame as it is.
        report.case(&format!("denoise {route:?}: missing neighbours"), at(Obligation::Outcome(Outcome::NeighbourMissing)), || {
            let (scene_, clip) = scene(50, 150, 1.0, STRENGTH);
            let at = clip.source_secs_at(40.0 / FPS as f64, FPS as f64);
            let dt = 1.0 / FPS as f64;
            holds(route, true, (Arc::clone(&scene_), clip.clone()), Ramp { missing: vec![at - dt], wrong_size: vec![] }, &[40], STRENGTH)?;
            holds(route, true, (Arc::clone(&scene_), clip.clone()), Ramp { missing: vec![at - dt], wrong_size: vec![at + dt] }, &[40], STRENGTH)?;
            holds(route, true, (scene_, clip), Ramp { missing: vec![at - dt, at + dt], wrong_size: vec![] }, &[40], STRENGTH)
        });
    }
}
