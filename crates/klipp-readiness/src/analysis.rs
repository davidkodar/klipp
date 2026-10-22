//! The analysis families' cases (M4 steps 8 and 9): Deflicker,
//! stabilisation and rolling-shutter correction, each drawn by its media's
//! analysis, held to what that analysis works out to.
//!
//! The source is a picture of gradients that changes a little each frame,
//! on a frame grid (media frame n shown from n / fps). Each media file is
//! a real file on disk, so its analysis is keyed as the app keys it, and
//! its asset is kept in the session's store. A frame's expectation is the
//! source frame it shows, through the effect's own kernel with the
//! analysis derived with the effect's settings and read at the frame's
//! source time: never a render read back. Without an analysis for the
//! frame the effect passes the frame through and its node declines (step
//! 10): a neighbour missing inside the media, which an export refuses.

use std::path::PathBuf;
use std::sync::Arc;

use klipp_analysis::assets::{self, Asset, AssetKind, AssetStore, MemoryStore};
use klipp_core::{MediaId, RationalTime};
use klipp_effects::homography::Homography;
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{AnalysisCase, Entry, Family, Key, Obligation, Outcome, Report, Route, TimeCase};

const FPS: u32 = 25;
const W: u32 = 32;
const H: u32 = 16;
/// The media: ten seconds.
const MEDIA_FRAMES: i64 = 250;
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// One analysis family: its subject, its effect at two settings, its tag,
/// and its analysis.
#[derive(Clone, Copy)]
struct Kind {
    family: Family,
    subject: &'static str,
    /// The effect's saved tag, which a fault names.
    tag: &'static str,
    asset: AssetKind,
    /// The effect as the cases place it.
    effect: fn() -> BuiltinEffect,
    /// The effect with a setting changed that its analysis is derived by.
    edited: fn() -> BuiltinEffect,
    /// The file's analysis over `frames` media frames, varied by `seed` so
    /// two files' analyses differ.
    data: fn(frames: usize, seed: f64) -> serde_json::Value,
}

const KINDS: [Kind; 3] = [
    Kind {
        family: Family::Deflicker,
        subject: "klipp.fx.deflicker",
        tag: "deflicker",
        asset: AssetKind::Deflicker,
        effect: || BuiltinEffect::Deflicker { strength: 1.0, window_secs: 0.4, gain: 1.0 },
        edited: || BuiltinEffect::Deflicker { strength: 1.0, window_secs: 1.2, gain: 1.0 },
        data: |frames, seed| {
            let luma: Vec<f32> = (0..frames).map(|i| if i % 2 == 0 { 0.35 } else { 0.55 } + 0.002 * i as f32 + 0.05 * seed as f32).collect();
            serde_json::to_value(klipp_effects::deflicker::DeflickerData { luma, fps: FPS as f64 }).expect("serialises")
        },
    },
    Kind {
        family: Family::Stabilisation,
        subject: "klipp.fx.video_stabilization",
        tag: "video_stabilization",
        asset: AssetKind::Stabilisation,
        effect: || BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.0, zoom: false },
        edited: || BuiltinEffect::VideoStabilization { smoothing: 0.9, crop_ratio: 0.0, zoom: false },
        data: |frames, seed| {
            use klipp_effects::stabilization::{MotionParams, StabilizationData};
            // Frame i sits at the inverse of a shift that changes frame to
            // frame.
            let shift = |i: usize| Homography { h: [[1.0, 0.0, 1.0 + (i % 4) as f64 + seed], [0.0, 1.0, 0.5 * (i % 3) as f64], [0.0, 0.0, 1.0]] };
            let data = StabilizationData {
                frame_transforms: vec![Homography::identity(); frames.saturating_sub(1)],
                accumulated: (0..frames).map(|i| shift(i).inverse().expect("a shift can be undone")).collect(),
                // A shaking path, so the smoothing's window shapes the
                // corrections (an edit of it changes them).
                trajectory: (0..frames).map(|i| MotionParams { tx: 3.0 * (i % 4) as f64, ty: 0.0, rotation: 0.0, scale: 1.0 }).collect(),
                fps: FPS as f64,
                frame_count: frames,
            };
            serde_json::to_value(data).expect("serialises")
        },
    },
    Kind {
        family: Family::RollingShutter,
        subject: "klipp.fx.rolling_shutter_correction",
        tag: "rolling_shutter_correction",
        asset: AssetKind::RollingShutter,
        effect: || BuiltinEffect::RollingShutterCorrection { strength: 1.0, readout_ratio: 0.9 },
        edited: || BuiltinEffect::RollingShutterCorrection { strength: 1.0, readout_ratio: 0.5 },
        data: |frames, seed| {
            // The picture moving right and down, by an amount that changes
            // frame to frame.
            let step = |dx: f64, dy: f64| Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] };
            let motion = (0..frames).map(|i| {
                let d = 2.0 + (i % 3) as f64 + seed;
                (step(-d, -1.0), step(d, 1.0))
            });
            serde_json::to_value(klipp_effects::rolling_shutter::RollingShutterData { motion: motion.collect(), fps: FPS as f64 }).expect("serialises")
        },
    },
];

fn key(kind: &Kind, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: kind.family, subject: kind.subject.into(), entry: Entry::Clip, route, obligation }
}

/// Source frame `n`: gradients across and down, its blue a step that
/// changes with the frame.
fn picture(n: i64) -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            v.extend_from_slice(&[0.1 + 0.8 * x as f32 / (W - 1) as f32, 0.1 + 0.6 * y as f32 / (H - 1) as f32, 0.2 + 0.01 * (n % 7) as f32, 1.0]);
        }
    }
    v
}

/// The media frame shown at `source_secs`.
fn shown(source_secs: f64) -> i64 {
    (source_secs * FPS as f64 + 1e-9).floor() as i64
}

/// The source: [`picture`] on its frame grid, every media alike.
struct Pictures;

impl Source for Pictures {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = shown(source_secs);
        (0..MEDIA_FRAMES).contains(&n).then(|| KnownFrame { linear: picture(n), width: W, height: H, pts: Some(n as f64 / FPS as f64) })
    }
}

/// A real file named `name`, for a media item to key its analysis by.
fn file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-readiness-analysis-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.mov"));
    if !path.exists() {
        let _ = std::fs::write(&path, name.as_bytes());
    }
    path
}

/// A media item of the file `name`.
fn media(name: &str) -> klipp_media::MediaItem {
    let mut item = klipp_media::MediaItem::synthetic_for_test(file(name), None);
    item.duration = RationalTime { value: MEDIA_FRAMES, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
    item
}

/// A clip of `item`, on the timeline from frame `at` for `frames`, its
/// source from media frame `from`, at `speed`, with `effect` (on or off).
fn clip_of(item: &klipp_media::MediaItem, at: i64, frames: i64, from: i64, speed: f64, effect: BuiltinEffect, on: bool) -> Clip {
    let tc = |f: i64| RationalTime { value: f, rate: FPS };
    let covered = (frames as f64 * speed.abs()).round() as i64;
    let mut c = Clip::media(item.id, tc(at), tc(at + frames), tc(from), tc(from + covered));
    c.speed = speed;
    // Through the clip's picture; a case that can't place it fails (the
    // cases run caught).
    let mut edits = vec![klipp_timeline::EffectEdit::Add(&effect)];
    if !on {
        edits.push(klipp_timeline::EffectEdit::SetEnabled(0, false));
    }
    match c.apply_effect_edits(&edits) {
        Some(Ok(_)) => c,
        Some(Err(why)) => panic!("{effect:?} couldn't be placed: {why}"),
        None => panic!("{effect:?} couldn't be placed through the picture"),
    }
}

/// A scene of `clips` on one track, their media in `items`.
fn scene(items: Vec<klipp_media::MediaItem>, clips: Vec<Clip>) -> Arc<Scene> {
    let mut pool = klipp_media::MediaPool::new();
    for item in items {
        pool.add(item);
    }
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v1.clips = clips;
    sequence.tracks = vec![v1];
    Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: Vec::new() })
}

/// A store with `kind`'s analysis of each of `items` over `frames` frames,
/// each with its own seed.
fn store_of(kind: &Kind, items: &[&klipp_media::MediaItem], frames: usize) -> MemoryStore {
    let mut s = MemoryStore::default();
    for (i, item) in items.iter().enumerate() {
        let key = assets::asset_key(kind.asset, item).expect("a key for a file on disk");
        assets::store(&mut s, key, (kind.data)(frames, i as f64)).expect("kept");
    }
    s
}

/// What a frame showing `source_secs` must be: its source frame, through
/// `effect`'s kernel with `raw` (the file's analysis) derived with the
/// effect's settings and read at the frame's source time; the frame as it
/// is without one, or past it.
fn expected(effect: &BuiltinEffect, raw: Option<&serde_json::Value>, source_secs: f64) -> Vec<f32> {
    let frame = picture(shown(source_secs));
    let Some(at) = raw.and_then(|r| klipp_effects::node_analysis::derive(effect, r)).map(|a| a.at(effect, source_secs, H)) else { return frame };
    let mut effect = effect.clone();
    match (&mut effect, at.gain) {
        (BuiltinEffect::Deflicker { gain, .. }, Some(g)) => *gain = g,
        (BuiltinEffect::Deflicker { .. }, None) => return frame,
        _ => {}
    }
    if matches!(effect, BuiltinEffect::VideoStabilization { .. }) && at.motion.stabilization.is_none() || matches!(effect, BuiltinEffect::RollingShutterCorrection { .. }) && at.motion.rsc_rows.is_none() {
        return frame;
    }
    klipp_effects::apply_effects_with_motion(&[effect], frame, W, H, LUMA, &at.motion)
}

/// The expectation of timeline frame `frame` of the scene: the clip under
/// it, at the source time it shows, with its own effect, from `raws` (each
/// clip's file's analysis, by media).
fn expect_at(scene: &Scene, raws: &[(MediaId, Option<serde_json::Value>)], frame: i64) -> Result<Vec<f32>, String> {
    let t = RationalTime { value: frame, rate: FPS };
    let clip = scene.sequence.tracks[0].clips.iter().find(|c| c.timeline_in <= t && t < c.timeline_out).ok_or(format!("no clip at frame {frame}"))?;
    let source_secs = clip.source_secs_at((t - clip.timeline_in).to_secs_f64(), FPS as f64);
    let effect = clip.picture_effects().map_err(|e| format!("the clip's picture: {e}"))?.into_iter().next().ok_or("the clip has no effect")?;
    let raw = raws.iter().find(|(m, _)| Some(*m) == clip.media_id()).and_then(|(_, r)| r.as_ref());
    Ok(if effect.enabled { expected(&effect.effect, raw, source_secs) } else { picture(shown(source_secs)) })
}

/// A session on `route` with the GPU `gpu` and `store` as its assets.
fn session(route: Route, gpu: bool, scene: Arc<Scene>, store: MemoryStore) -> Session {
    let mut s = Session::new(route, gpu, scene, Arc::new(Pictures) as Arc<dyn Source>);
    s.renderer_mut().set_asset_store(Box::new(store));
    s
}

/// `frames` of `s`, in order, each held to its expectation from `raws`,
/// and to declining as `label` says (`None`: nothing).
fn holds(s: &mut Session, scene: &Scene, raws: &[(MediaId, Option<serde_json::Value>)], frames: &[i64], label: Option<&str>) -> Result<(), String> {
    for &f in frames {
        let got = s.frame(f);
        // The viewer's GPU stack draws display bytes only: within a byte or
        // two (a warp's bilinear taps on another processor).
        let tolerance = if got.display_only() { 2.0 / 255.0 } else { 2e-3 };
        let off = got.off(&expect_at(scene, raws, f)?);
        if off > tolerance {
            return Err(format!("frame {f}: {off} from what its analysis works out to"));
        }
        got.declined_as(label)?;
    }
    Ok(())
}

/// The analysis as raw JSON that `kind` keeps for `frames` of the file
/// with seed `seed`.
fn raw(kind: &Kind, frames: usize, seed: f64) -> Option<serde_json::Value> {
    Some((kind.data)(frames, seed))
}

pub fn run_deflicker(report: &mut Report) {
    run_kind(report, &KINDS[0]);
}

pub fn run_stabilisation(report: &mut Report) {
    run_kind(report, &KINDS[1]);
}

pub fn run_rolling_shutter(report: &mut Report) {
    run_kind(report, &KINDS[2]);
}

fn run_kind(report: &mut Report, kind: &Kind) {
    let name = kind.tag;
    let whole = MEDIA_FRAMES as usize;
    // One file, one clip: from media frame 50, 100 frames, at `speed`.
    let one = |speed: f64, on: bool| {
        let item = media(&format!("{name}-one"));
        let c = clip_of(&item, 0, 100, 50, speed, (kind.effect)(), on);
        (scene(vec![item.clone()], vec![c]), item)
    };
    let valid = |item: &klipp_media::MediaItem| store_of(kind, &[item], whole);
    let raws_of = |item: &klipp_media::MediaItem, r: Option<serde_json::Value>| vec![(item.id, r)];

    report.case(&format!("{name}: the old stage is gone"), [key(kind, None, Obligation::OldStageGone)], || crate::old_stage::gone(kind.family));
    report.case(&format!("{name}: saved and reloaded"), [key(kind, None, Obligation::Saved)], || {
        let (scene, _) = one(1.0, true);
        let back = crate::saved::round_trip(&scene.sequence, &scene.pool)?;
        let fx = |c: &Clip| c.picture_effects().map(|fx| fx.into_iter().map(|e| (e.effect, e.enabled)).collect::<Vec<_>>());
        let got = back.tracks.first().and_then(|t| t.clips.first()).ok_or("no clip came back")?;
        if fx(got)? != fx(&scene.sequence.tracks[0].clips[0])? {
            return Err(format!("came back as {:?}", fx(got)?));
        }
        Ok(())
    });

    // On the viewer, within the GPU's budget of the same clip without it,
    // its analysis in the session's store.
    report.case(&format!("{name} Viewer: within its frame budget"), [key(kind, Some(Route::Viewer), Obligation::FrameBudget)], || {
        let (with, item) = one(1.0, true);
        // The same clip with the effect off.
        let (without, _) = one(1.0, false);
        let frames: Vec<i64> = (20..20 + crate::budget::MIN_FRAMES as i64).collect();
        let setup = |s: &mut Session| s.renderer_mut().set_asset_store(Box::new(valid(&item)));
        crate::budget::within_set(with, without, Arc::new(Pictures), &frames, crate::budget::Budget::GPU, &setup).map(|_| ())
    });
    // Level 2: the viewer shows export's frame, both read from one real
    // file. Both find analysis where the app keeps it, the analysis
    // cache, so the file's is put there for the case and taken out after.
    report.case(&format!("{name}: the viewer shows export's frame"), [key(kind, Some(Route::Viewer), Obligation::ViewerEqualsExport), key(kind, Some(Route::Export), Obligation::ViewerEqualsExport)], || {
        let (s, _) = one(1.0, true);
        let s = crate::on_file::on_file_frames(&s, &format!("{name}-on-file"), FPS, MEDIA_FRAMES, &picture)?;
        let item = s.pool.all().next().ok_or("the scene has no media")?;
        let asset = assets::asset_key(kind.asset, item).ok_or("no analysis key for the file")?;
        let folder = klipp_analysis::store::analysis_dir().join(&asset.media);
        assets::store(&mut assets::CacheStore::default(), asset, (kind.data)(whole, 0.0))?;
        let shown = crate::route2::viewer_equals_export(Arc::clone(&s), &[10, 11, 50]);
        let _ = std::fs::remove_dir_all(&folder);
        shown
    });

    for route in [Route::Viewer, Route::Export] {
        let at = |o| [key(kind, Some(route), o)];
        let run_on = |gpu: bool, speed: f64, frames: &[i64]| -> Result<(), String> {
            let (scene, item) = one(speed, true);
            let raws = raws_of(&item, raw(kind, whole, 0.0));
            holds(&mut session(route, gpu, Arc::clone(&scene), valid(&item)), &scene, &raws, frames, None)
        };
        report.case(&format!("{name} {route:?}: frames by their analysis"), at(Obligation::ExpectedPixels), || run_on(true, 1.0, &[10, 11, 50]));
        report.case(&format!("{name} {route:?}: the CPU and the GPU agree"), at(Obligation::CpuGpuAgree), || {
            run_on(false, 1.0, &[50])?;
            run_on(true, 1.0, &[50])
        });
        report.case(&format!("{name} {route:?}: a trimmed clip's first frame"), at(Obligation::Time(TimeCase::TrimmedStart)), || run_on(true, 1.0, &[0]));
        report.case(&format!("{name} {route:?}: a trimmed clip's last frame"), at(Obligation::Time(TimeCase::TrimmedEnd)), || run_on(true, 1.0, &[99]));
        report.case(&format!("{name} {route:?}: a reversed clip"), at(Obligation::Time(TimeCase::Reversed)), || run_on(true, -1.0, &[0, 40, 99]));
        report.case(&format!("{name} {route:?}: a clip at twice the speed"), at(Obligation::Time(TimeCase::SpeedRamped)), || run_on(true, 2.0, &[0, 20, 49]));
        // The media's first frame: the analysis's window reaches past its
        // edge, which the derivation handles; nothing declined.
        report.case(&format!("{name} {route:?}: the media's first frame"), at(Obligation::Time(TimeCase::OutOfRange)), || {
            let item = media(&format!("{name}-edge"));
            let scene = scene(vec![item.clone()], vec![clip_of(&item, 0, 100, 0, 1.0, (kind.effect)(), true)]);
            let raws = raws_of(&item, raw(kind, whole, 0.0));
            holds(&mut session(route, true, Arc::clone(&scene), valid(&item)), &scene, &raws, &[0], None)
        });
        report.case(&format!("{name} {route:?}: a seek back"), at(Obligation::Time(TimeCase::BackwardSeek)), || run_on(true, 1.0, &[60, 61, 20, 21]));
        // Switched off: the frame as it is, noted as bypassed (never
        // failing).
        report.case(&format!("{name} {route:?}: switched off"), at(Obligation::Outcome(Outcome::Bypass)), || {
            let (scene, item) = one(1.0, false);
            holds(&mut session(route, true, Arc::clone(&scene), valid(&item)), &scene, &raws_of(&item, None), &[40], Some("bypassed"))
        });
        // An analysis shorter than the clip: past it, inside the media, the
        // frame as it is, declined as a neighbour missing; the control, a
        // frame inside it, corrected with nothing declined.
        report.case(&format!("{name} {route:?}: no analysis for the frame"), at(Obligation::Outcome(Outcome::NeighbourMissing)), || {
            let (scene, item) = one(1.0, true);
            let short = store_of(kind, &[&item], 100);
            let raws = raws_of(&item, raw(kind, 100, 0.0));
            let mut s = session(route, true, Arc::clone(&scene), short);
            holds(&mut s, &scene, &raws, &[20], None)?;
            holds(&mut s, &scene, &raws, &[70], Some("neighbour frame missing"))
        });
        report.case(&format!("{name} {route:?}: its render failed"), at(Obligation::Outcome(Outcome::RenderFailed)), || {
            let _fault = klipp_nodes::fault::fail(kind.tag);
            let (scene, item) = one(1.0, true);
            holds(&mut session(route, true, Arc::clone(&scene), valid(&item)), &scene, &raws_of(&item, None), &[40], Some("render failed"))
        });
    }

    // The analysis cases, on export's route (the viewer draws the same
    // frames by the same lookup; its routes are held above).
    let route = Route::Export;
    let case = |a| [key(kind, None, Obligation::Analysis(a))];
    // A complete asset, as an export's analysis run makes it (every frame
    // of the file, `frames_of` its duration): no frame of the clip, its
    // last included, is declined, so an export that analysed first never
    // refuses one.
    report.case(&format!("{name}: a valid analysis"), case(AnalysisCase::Valid), || {
        let (scene, item) = one(1.0, true);
        let complete = assets::frames_of(item.duration.to_secs_f64(), FPS as f64);
        let mut s = session(route, true, Arc::clone(&scene), store_of(kind, &[&item], complete));
        holds(&mut s, &scene, &raws_of(&item, raw(kind, complete, 0.0)), &(0..100).collect::<Vec<_>>(), None)
    });
    // No analysis yet (one running, or none made): on both routes, the
    // frame as it is, never a blank, and declined, which the viewer names
    // and an export refuses.
    report.case(&format!("{name}: no analysis"), case(AnalysisCase::Absent), || {
        let (scene, item) = one(1.0, true);
        for r in [Route::Viewer, Route::Export] {
            holds(&mut session(r, true, Arc::clone(&scene), MemoryStore::default()), &scene, &raws_of(&item, None), &[30], Some("neighbour frame missing"))?;
        }
        Ok(())
    });
    report.case(&format!("{name}: a stale analysis"), case(AnalysisCase::Stale), || {
        let (scene, item) = one(1.0, true);
        let mut s = MemoryStore::default();
        let key = assets::asset_key(kind.asset, &item).ok_or("a key")?;
        s.put(Asset { key, algorithm: assets::algorithm(kind.asset) + 1, data: (kind.data)(whole, 0.0) })?;
        holds(&mut session(route, true, Arc::clone(&scene), s), &scene, &raws_of(&item, None), &[30], Some("neighbour frame missing"))
    });
    // Two files' clips, frame by frame in turn: each its own analysis.
    report.case(&format!("{name}: two files in turn"), case(AnalysisCase::Alternating), || {
        let (a, b) = (media(&format!("{name}-a")), media(&format!("{name}-b")));
        let scene = scene(vec![a.clone(), b.clone()], vec![clip_of(&a, 0, 50, 50, 1.0, (kind.effect)(), true), clip_of(&b, 50, 50, 50, 1.0, (kind.effect)(), true)]);
        let raws = vec![(a.id, raw(kind, whole, 0.0)), (b.id, raw(kind, whole, 1.0))];
        holds(&mut session(route, true, Arc::clone(&scene), store_of(kind, &[&a, &b], whole)), &scene, &raws, &[10, 60, 11, 61, 10], None)
    });
    // The effect's setting its analysis is derived by, changed between two
    // frames of one render owner: the second is derived anew.
    report.case(&format!("{name}: an edit of its settings"), case(AnalysisCase::InvalidatedByEdit), || {
        let item = media(&format!("{name}-edit"));
        let before = scene(vec![item.clone()], vec![clip_of(&item, 0, 100, 50, 1.0, (kind.effect)(), true)]);
        let after = scene(vec![item.clone()], vec![clip_of(&item, 0, 100, 50, 1.0, (kind.edited)(), true)]);
        let raws = raws_of(&item, raw(kind, whole, 0.0));
        let mut s = session(route, true, Arc::clone(&before), valid(&item));
        holds(&mut s, &before, &raws, &[30], None)?;
        let mut s = s.with_scene(Arc::clone(&after), Arc::new(Pictures));
        holds(&mut s, &after, &raws, &[30], None)?;
        // The control: the edit changes the frame.
        if expect_at(&before, &raws, 30)? == expect_at(&after, &raws, 30)? {
            return Err("the edited setting changes nothing here".into());
        }
        Ok(())
    });
    // The clip moved on the timeline: the same source frames, the same
    // analysis, still read.
    report.case(&format!("{name}: an edit that keeps it"), case(AnalysisCase::KeptByEdit), || {
        let item = media(&format!("{name}-move"));
        let before = scene(vec![item.clone()], vec![clip_of(&item, 0, 100, 50, 1.0, (kind.effect)(), true)]);
        let after = scene(vec![item.clone()], vec![clip_of(&item, 10, 100, 50, 1.0, (kind.effect)(), true)]);
        let raws = raws_of(&item, raw(kind, whole, 0.0));
        let mut s = session(route, true, Arc::clone(&before), valid(&item));
        holds(&mut s, &before, &raws, &[30], None)?;
        let mut s = s.with_scene(Arc::clone(&after), Arc::new(Pictures));
        holds(&mut s, &after, &raws, &[40], None)
    });
    // A blade: each half reads the file's analysis at its own source time.
    report.case(&format!("{name}: a split clip"), case(AnalysisCase::Split), || {
        let item = media(&format!("{name}-split"));
        let scene = scene(vec![item.clone()], vec![clip_of(&item, 0, 40, 50, 1.0, (kind.effect)(), true), clip_of(&item, 40, 60, 90, 1.0, (kind.effect)(), true)]);
        holds(&mut session(route, true, Arc::clone(&scene), valid(&item)), &scene, &raws_of(&item, raw(kind, whole, 0.0)), &[39, 40, 41], None)
    });
    // A copy of the clip elsewhere reads the same analysis.
    report.case(&format!("{name}: a duplicated clip"), case(AnalysisCase::Duplicate), || {
        let item = media(&format!("{name}-dup"));
        let scene = scene(vec![item.clone()], vec![clip_of(&item, 0, 50, 50, 1.0, (kind.effect)(), true), clip_of(&item, 50, 50, 50, 1.0, (kind.effect)(), true)]);
        holds(&mut session(route, true, Arc::clone(&scene), valid(&item)), &scene, &raws_of(&item, raw(kind, whole, 0.0)), &[10, 60], None)
    });
    // Relinked to another file: the old file's analysis isn't that file's.
    report.case(&format!("{name}: a relinked file"), case(AnalysisCase::Relink), || {
        let item = media(&format!("{name}-relink"));
        let store = valid(&item);
        let mut moved = item.clone();
        moved.path = file(&format!("{name}-relinked"));
        let scene = scene(vec![moved.clone()], vec![clip_of(&moved, 0, 100, 50, 1.0, (kind.effect)(), true)]);
        holds(&mut session(route, true, Arc::clone(&scene), store), &scene, &raws_of(&moved, None), &[30], Some("neighbour frame missing"))
    });
    // A proxy is drawn by its original file's analysis.
    report.case(&format!("{name}: a proxy"), case(AnalysisCase::Proxy), || {
        let mut item = media(&format!("{name}-proxied"));
        let store = valid(&item);
        item.proxy_path = Some(file(&format!("{name}-proxy")));
        item.proxy_ready = true;
        let scene = scene(vec![item.clone()], vec![clip_of(&item, 0, 100, 50, 1.0, (kind.effect)(), true)]);
        holds(&mut session(route, true, Arc::clone(&scene), store), &scene, &raws_of(&item, raw(kind, whole, 0.0)), &[30], None)
    });
}

