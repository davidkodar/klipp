//! The temporal OFX family's cases (M4 step 4), on #932's fixture bundle
//! (`org.klipp.fixture.temporal`) through the level-1 routes.
//!
//! The fixture reads the frames either side of its own, in OFX time units
//! (frames, the spec's): red from the frame before, green its own, blue
//! from the frame after; a frame the host doesn't give is 0. "Either side"
//! is in playback order: the clip's frames one before and one after, each
//! at the source time the clip shows it at (`Clip::source_secs_at`), so on
//! a 2× clip they're two source frames away and on a reversed clip the
//! frame before is the later one. A host that hands the wrong frame, one
//! as if the clip played at speed 1, or one in seconds where frames are
//! meant, shows in the red and blue.

use std::collections::HashMap;
use std::sync::Arc;

use klipp_core::{MediaId, OfxValue, RationalTime};
use klipp_timeline::{Clip, ClipKind, OfxEdit, Sequence, Track, TrackKind};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, Key, Obligation, Report, Route, TimeCase};

/// The subject (`families`' name for the temporal fixture).
pub const SUBJECT: &str = "ofx.fixture.time_request";
/// The fixture's plug-in identifier.
const TEMPORAL: &str = "org.klipp.fixture.temporal";

const W: u32 = 32;
const H: u32 = 24;
const FPS: u32 = 25;
/// The media's length in frames.
const MEDIA: i64 = 100;
const ROUTES: [Route; 2] = [Route::Viewer, Route::Export];

fn key(route: Route, obligation: Obligation) -> Key {
    keyed(Entry::Clip, Some(route), obligation)
}

fn keyed(entry: Entry, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::OfxTemporal, subject: SUBJECT.into(), entry, route, obligation }
}

fn tc(frame: i64) -> RationalTime {
    RationalTime { value: frame, rate: FPS }
}

/// Source frame `n`: every channel names it (`n` in red, `n + 100` in
/// green, `n + 50` in blue, wrapped), so which frame reached which channel
/// is read straight off the output. Nothing outside the media.
struct Frames;

fn frame_bytes(n: i64) -> [u8; 4] {
    [n.rem_euclid(256) as u8, (n + 100).rem_euclid(256) as u8, (n + 50).rem_euclid(256) as u8, 255]
}

impl Source for Frames {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = (source_secs * FPS as f64 + 1e-9).floor() as i64;
        (0..MEDIA).contains(&n).then(|| {
            let srgb = frame_bytes(n).repeat((W * H) as usize);
            KnownFrame { linear: klipp_effects::srgb_u8_to_linear_f32_rgba(&srgb), width: W, height: H, pts: Some(n as f64 / FPS as f64) }
        })
    }

    fn range(&self, _media: MediaId) -> Option<(f64, f64)> {
        Some((0.0, MEDIA as f64 / FPS as f64))
    }
}

/// A clip of `len` timeline frames from frame 0, showing the source from
/// `source_in` at `speed` (negative: reversed), with the temporal fixture
/// (`mode` 5: forty frames either side, past any budget).
fn scene(source_in: i64, len: i64, wide: bool) -> Arc<Scene> {
    scene_at(source_in, len, 1.0, wide)
}

fn scene_at(source_in: i64, len: i64, speed: f64, wide: bool) -> Arc<Scene> {
    let mode = if wide { 5 } else { 0 };
    built(source_in, len, speed, &[fixture(TEMPORAL, true, mode)], None)
}

/// The fixture's entry: `plugin`, switched on or off, its `mode` (0: none
/// set, as it renders normally).
fn fixture(plugin: &str, enabled: bool, mode: i32) -> klipp_plugin::AppliedEffect {
    let mut params = HashMap::new();
    if mode != 0 {
        params.insert("mode".to_string(), OfxValue::Int(mode));
    }
    klipp_plugin::AppliedEffect { plugin_name: plugin.into(), enabled, param_values: params, ..Default::default() }
}

/// `clip` with `entries`, written through its picture.
fn with_plugins(mut clip: Clip, entries: &[klipp_plugin::AppliedEffect]) -> Clip {
    let edits: Vec<OfxEdit> = entries.iter().map(OfxEdit::Add).collect();
    if !edits.is_empty() {
        clip.apply_ofx_edits(&edits).unwrap_or_else(|e| panic!("the plug-ins didn't land on the clip: {e}"));
    }
    clip
}

/// The media clip's scene with `entries` on it; or, with `entry` Title or
/// Adjustment, the entries on that layer (an adjustment layer over the
/// clip; a title alone).
fn built(source_in: i64, len: i64, speed: f64, entries: &[klipp_plugin::AppliedEffect], entry: Option<Entry>) -> Arc<Scene> {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/frames.mov".into(), None);
    item.fps = klipp_core::Fps::new(FPS, 1);
    item.duration = tc(MEDIA);
    item.resolution = klipp_core::Resolution { width: W, height: H };
    let media = item.id;
    pool.add(item);
    let covered = (len as f64 * speed.abs()).round() as i64;
    let mut clip = Clip::media(media, tc(0), tc(len), tc(source_in), tc(source_in + covered));
    clip.speed = speed;
    let track = |clips| {
        let mut t = Track::new("V", TrackKind::Video);
        t.clips = clips;
        t
    };
    // Top first.
    let tracks = match entry {
        None | Some(Entry::Clip) => vec![track(vec![with_plugins(clip, entries)])],
        Some(Entry::Adjustment) => {
            let adj = Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0), tc(len)) };
            vec![track(vec![with_plugins(adj, entries)]), track(vec![clip])]
        }
        Some(Entry::Title) => {
            let mut data = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "M".into(), font_size: 0.6 * 1080.0, color: [255, 255, 255, 255], ..Default::default() });
            data.background = klipp_timeline::TitleBackground::FullFrame { color: [64, 128, 192, 255] };
            vec![track(vec![with_plugins(Clip { kind: ClipKind::Title(data), ..Clip::gap(tc(0), tc(len)) }, entries)])]
        }
        Some(Entry::Conversion) => unreachable!("not an OFX entry"),
    };
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    sequence.resolution = klipp_core::Resolution { width: W, height: H };
    sequence.tracks = tracks;
    Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: vec![crate::fixtures::ofx()] })
}

/// The source frame the clip shows at its frame `f` (the app's own
/// mapping: speed, reverse), or none outside the media.
fn source_frame(clip: &Clip, f: i64) -> Option<i64> {
    let n = (clip.source_secs_at(f as f64 / FPS as f64, FPS as f64) * FPS as f64 + 1e-6).floor() as i64;
    (0..MEDIA).contains(&n).then_some(n)
}

/// What timeline frame `f` of `scene`'s clip shows: red from the clip's
/// frame `f - reach`, green its own, blue from `f + reach`, each at the
/// source frame the clip shows there, and 0 when the media hasn't it or it
/// is past the budget the host allows.
fn expected(scene: &Scene, f: i64, reach: i64, budget: i64) -> [u8; 3] {
    let clip = &scene.sequence.tracks[0].clips[0];
    let at = |g: i64, c: usize| match source_frame(clip, g) {
        Some(n) if reach <= budget => frame_bytes(n)[c],
        _ => 0,
    };
    [at(f - reach, 0), source_frame(clip, f).map_or(0, |n| frame_bytes(n)[1]), at(f + reach, 2)]
}

/// Frames `frames` on `route`, by one render owner: each one's first pixel.
fn shows(route: Route, scene: &Arc<Scene>, frames: &[i64]) -> Vec<[u8; 3]> {
    let mut s = Session::new(route, true, Arc::clone(scene), Arc::new(Frames));
    frames.iter().map(|&f| {
        let px = s.frame(f).srgb8;
        [px[0], px[1], px[2]]
    }).collect()
}

fn check(got: Vec<[u8; 3]>, want: Vec<[u8; 3]>) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("showed {got:?}, expected {want:?}")) }
}

fn time_cases(report: &mut Report, budget: i64) {
    for route in ROUTES {
        // A clip trimmed at its start: its first frame reads the handle
        // before it (source 49), which the media has.
        let trimmed = scene(50, 30, false);
        report.case(&format!("temporal {route:?} trimmed start"), [key(route, Obligation::Time(TimeCase::TrimmedStart))], || {
            check(shows(route, &trimmed, &[0, 1]), vec![expected(&trimmed, 0, 1, budget), expected(&trimmed, 1, 1, budget)])
        });
        // Its last frame reads the handle after it (source 80).
        report.case(&format!("temporal {route:?} trimmed end"), [key(route, Obligation::Time(TimeCase::TrimmedEnd))], || {
            check(shows(route, &trimmed, &[29]), vec![expected(&trimmed, 29, 1, budget)])
        });
        // A clip ending at the media's end: the frame after isn't there.
        let to_the_end = scene(MEDIA - 10, 10, false);
        report.case(&format!("temporal {route:?} past the media"), [key(route, Obligation::Time(TimeCase::OutOfRange))], || {
            let got = shows(route, &to_the_end, &[9]);
            if got[0][2] != 0 {
                return Err(format!("the frame after the media's last was handed in: {got:?}"));
            }
            check(got, vec![expected(&to_the_end, 9, 1, budget)])
        });
        // Played on, then sent back: each frame reads its own neighbours.
        report.case(&format!("temporal {route:?} seek back"), [key(route, Obligation::Time(TimeCase::BackwardSeek))], || {
            check(shows(route, &trimmed, &[10, 11, 3]), [10, 11, 3].iter().map(|&f| expected(&trimmed, f, 1, budget)).collect())
        });
        // At twice the speed: the frames either side are two source frames
        // away.
        let fast = scene_at(20, 20, 2.0, false);
        report.case(&format!("temporal {route:?} at 2x"), [key(route, Obligation::Time(TimeCase::SpeedRamped))], || {
            check(shows(route, &fast, &[3, 4]), vec![expected(&fast, 3, 1, budget), expected(&fast, 4, 1, budget)])
        });
        // Reversed: the frame before, in playback, is the later source frame.
        let back = scene_at(30, 20, -1.0, false);
        report.case(&format!("temporal {route:?} reversed"), [key(route, Obligation::Time(TimeCase::Reversed))], || {
            let clip = &back.sequence.tracks[0].clips[0];
            if source_frame(clip, 4) <= source_frame(clip, 6) {
                return Err("the clip isn't reversed: its frame before shows an earlier source frame".into());
            }
            check(shows(route, &back, &[5, 6]), vec![expected(&back, 5, 1, budget), expected(&back, 6, 1, budget)])
        });
        // Asking forty either side, past the budget: those frames aren't
        // handed in (0), never the current frame instead.
        let wide = scene(50, 30, true);
        report.case(&format!("temporal {route:?} past the budget"), [], || {
            let got = shows(route, &wide, &[5]);
            check(got, vec![expected(&wide, 5, 40, budget)])
        });
    }
}

/// Every pixel of frame `f` of `scene` on `route` (one owner, the GPU or
/// not), as sRGB8.
fn frame_of(route: Route, gpu: bool, scene: &Arc<Scene>, frames: &[i64]) -> Vec<crate::route::Frame> {
    let mut s = Session::new(route, gpu, Arc::clone(scene), Arc::new(Frames));
    frames.iter().map(|&f| s.frame(f)).collect()
}

/// `scene` reading a real file of the same frames (level 2).
fn on_file(scene: &Arc<Scene>) -> Result<Arc<Scene>, String> {
    let path = crate::route2::known_media("ofx-temporal-frames", W, H, FPS, MEDIA, &|n| klipp_effects::srgb_u8_to_linear_f32_rgba(&frame_bytes(n).repeat((W * H) as usize)))?;
    let mut s = (**scene).clone();
    let ids: Vec<MediaId> = s.pool.all().map(|m| m.id).collect();
    for id in ids {
        if let Some(m) = s.pool.get_mut(&id) {
            m.path = path.clone();
            m.kind = klipp_media::MediaItemKind::File;
        }
    }
    Ok(Arc::new(s))
}

/// The picture each entry hands the node, without it, and with it.
fn entry_cases(report: &mut Report) {
    for entry in [Entry::Clip, Entry::Title, Entry::Adjustment] {
        let with = built(50, 30, 1.0, &[fixture(TEMPORAL, true, 0)], Some(entry));
        let without = built(50, 30, 1.0, &[], Some(entry));
        let off = built(50, 30, 1.0, &[fixture(TEMPORAL, false, 0)], Some(entry));
        let frames = [5, 6];
        for route in ROUTES {
            let k = |o| keyed(entry, Some(route), o);
            // The node's picture: its own frame in green; red and blue the
            // clip's frames either side. A title or an adjustment layer has
            // no source (`frame_context::NoSources`): the node runs with no
            // neighbours (0), declined as a fallback, never as a failure.
            let sourced = entry == Entry::Clip;
            report.case(&format!("temporal {entry:?} {route:?} pixels"), [k(Obligation::ExpectedPixels)], || {
                let given = frame_of(route, true, &without, &[4, 5, 6, 7]);
                for (i, got) in frame_of(route, true, &with, &frames).into_iter().enumerate() {
                    let (before, own, after) = (&given[i].srgb8, &given[i + 1].srgb8, &given[i + 2].srgb8);
                    let side = |px: &Vec<u8>, at: usize| if sourced { px[at] } else { 0 };
                    let want: Vec<u8> = (0..own.len() / 4).flat_map(|p| [side(before, p * 4), own[p * 4 + 1], side(after, p * 4 + 2), own[p * 4 + 3]]).collect();
                    got.declined_as(if sourced { None } else { Some("fallback") })?;
                    let worst = got.srgb8.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                    if worst > 1 {
                        return Err(format!("frame {}: {worst} levels from its neighbours' channels (first pixel {:?}, expected {:?}; declined {:?})", frames[i], &got.srgb8[..4], &want[..4], got.declines()));
                    }
                }
                Ok(())
            });
            report.case(&format!("temporal {entry:?} {route:?} cpu=gpu"), [k(Obligation::CpuGpuAgree)], || {
                let (cpu, gpu) = (frame_of(route, false, &with, &frames), frame_of(route, true, &with, &frames));
                let worst = cpu.iter().zip(&gpu).flat_map(|(a, b)| a.srgb8.iter().zip(&b.srgb8).map(|(x, y)| x.abs_diff(*y))).max().unwrap_or(0);
                if worst > 1 { Err(format!("CPU and GPU differ by {worst} levels")) } else { Ok(()) }
            });
            report.case(&format!("temporal {entry:?} {route:?} switched off"), [k(Obligation::DisabledIsIdentity)], || {
                let (a, b) = (frame_of(route, true, &off, &frames), frame_of(route, true, &without, &frames));
                if a.iter().zip(&b).all(|(x, y)| x.srgb8 == y.srgb8) { Ok(()) } else { Err("switched off, the picture isn't the one it was given".into()) }
            });
        }
        let vee = ROUTES.map(|r| keyed(entry, Some(r), Obligation::ViewerEqualsExport));
        report.case(&format!("temporal {entry:?} viewer = export"), vee, || crate::route2::viewer_equals_export(on_file(&with)?, &[5]));
    }
}

/// Step 10's outcomes, on the clip, each visible on the frame: the input
/// passed on, never a picture made up.
fn outcome_cases(report: &mut Report) {
    use crate::Outcome;
    let given = |route| frame_of(route, true, &built(50, 30, 1.0, &[], None), &[5]).remove(0).srgb8;
    for route in ROUTES {
        let holds = |scene: Arc<Scene>, label: &str| -> Result<(), String> {
            let f = frame_of(route, true, &scene, &[5]).remove(0);
            f.declined_as(Some(label))?;
            if f.srgb8 != given(route) { Err(format!("declined {label}, but the picture isn't its input")) } else { Ok(()) }
        };
        report.case(&format!("temporal {route:?}: switched off"), [key(route, Obligation::Outcome(Outcome::Bypass))], || holds(built(50, 30, 1.0, &[fixture(TEMPORAL, false, 0)], None), "bypassed"));
        report.case(&format!("temporal {route:?}: not installed"), [key(route, Obligation::Outcome(Outcome::PluginMissing))], || holds(built(50, 30, 1.0, &[fixture("org.klipp.fixture.absent_temporal", true, 0)], None), "plug-in missing"));
        report.case(&format!("temporal {route:?}: its render fails"), [key(route, Obligation::Outcome(Outcome::RenderFailed))], || holds(built(50, 30, 1.0, &[fixture(TEMPORAL, true, 2)], None), "render failed"));
        report.case(&format!("temporal {route:?}: a context it hasn't"), [key(route, Obligation::Outcome(Outcome::Unsupported))], || {
            let mut e = fixture(TEMPORAL, true, 0);
            e.context = klipp_plugin::OfxCategory::Transition;
            holds(built(50, 30, 1.0, &[e], None), "unsupported")
        });
        report.case(&format!("temporal {route:?}: a neighbour inside the media unread"), [key(route, Obligation::Outcome(Outcome::NeighbourMissing))], || {
            // Source frame 54 (the frame before 5) can't be read.
            let mut s = Session::new(route, true, built(50, 30, 1.0, &[fixture(TEMPORAL, true, 0)], None), Arc::new(Holed(54)));
            let f = s.frame(5);
            f.declined_as(Some("neighbour frame missing"))
        });
    }
}

/// [`Frames`] at half the size, as a render at half scale reads them.
struct Half;

impl Source for Half {
    fn frame(&self, media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = (source_secs * FPS as f64 + 1e-9).floor() as i64;
        let f = Frames.frame(media, source_secs)?;
        let srgb = frame_bytes(n).repeat(((W / 2) * (H / 2)) as usize);
        Some(KnownFrame { linear: klipp_effects::srgb_u8_to_linear_f32_rgba(&srgb), width: W / 2, height: H / 2, pts: f.pts })
    }

    fn range(&self, media: MediaId) -> Option<(f64, f64)> {
        Frames.range(media)
    }
}

/// [`Frames`] with one frame inside the media that can't be read.
struct Holed(i64);

impl Source for Holed {
    fn frame(&self, media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let n = (source_secs * FPS as f64 + 1e-9).floor() as i64;
        if n == self.0 { None } else { Frames.frame(media, source_secs) }
    }

    fn range(&self, media: MediaId) -> Option<(f64, f64)> {
        Frames.range(media)
    }
}

/// The node's instances: each frame reads its own neighbours, whatever the
/// order asked, the owner, the clip, the size or an edit between.
fn instance_cases(report: &mut Report, budget: i64) {
    use crate::InstanceCase as I;
    let k = |i| keyed(Entry::Clip, None, Obligation::Instances(i));
    let clip = scene(50, 30, false);
    let each = |route, scene: &Arc<Scene>, frames: &[i64]| check(shows(route, scene, frames), frames.iter().map(|&f| expected(scene, f, 1, budget)).collect());
    report.case("temporal: a seek reads the new frame's neighbours", [k(I::ResetOnSeek)], || each(Route::Export, &clip, &[10, 11, 12, 25, 26]));
    report.case("temporal: a step back reads its own", [k(I::ResetOnReverse)], || each(Route::Viewer, &clip, &[20, 21, 19, 18]));
    report.case("temporal: viewer and export each their own", [k(I::ViewerAndExportTogether)], || {
        let mut v = Session::new(Route::Viewer, true, Arc::clone(&clip), Arc::new(Frames));
        let mut e = Session::new(Route::Export, true, Arc::clone(&clip), Arc::new(Frames));
        for (a, b) in [(1, 20), (2, 21), (3, 22)] {
            let (pv, pe) = (v.frame(a).srgb8, e.frame(b).srgb8);
            if [pv[0], pv[1], pv[2]] != expected(&clip, a, 1, budget) || [pe[0], pe[1], pe[2]] != expected(&clip, b, 1, budget) {
                return Err(format!("at {a} and {b}: an owner read the other's frames"));
            }
        }
        Ok(())
    });
    report.case("temporal: two clips each their own", [k(I::TwoClips)], || {
        // A second clip of other source frames after the first: its first
        // frame reads its own handle.
        let mut s = (*clip).clone();
        let mut second = s.sequence.tracks[0].clips[0].clone();
        second.id = klipp_core::ClipId::new();
        (second.timeline_in, second.timeline_out, second.source_in, second.source_out) = (tc(30), tc(40), tc(10), tc(20));
        s.sequence.tracks[0].clips.push(second);
        let got = shows(Route::Export, &Arc::new(s), &[29, 30]);
        // Frame 30 is the second clip's first: source 10, between 9 and 11.
        let want = vec![expected(&clip, 29, 1, budget), [9, 110, 61]];
        check(got, want)
    });
    report.case("temporal: two nodes of one plug-in", [k(I::TwoNodesOfOnePlugin)], || {
        // Each reads the clip's own frames either side; the second's own
        // frame is the first's picture: the same channels in the end.
        let twice = built(50, 30, 1.0, &[fixture(TEMPORAL, true, 0), fixture(TEMPORAL, true, 0)], None);
        check(shows(Route::Export, &twice, &[5, 6]), vec![expected(&clip, 5, 1, budget), expected(&clip, 6, 1, budget)])
    });
    report.case("temporal: a new size reads its own", [k(I::ResetOnResolution)], || {
        let mut s = Session::new(Route::Export, true, Arc::clone(&clip), Arc::new(Frames));
        s.frame(8);
        // A render-scale change: the frames come at the new size too.
        let mut small = (*clip).clone();
        (small.width, small.height) = (W / 2, H / 2);
        let small = Arc::new(small);
        let mut s2 = s.with_scene(Arc::clone(&small), Arc::new(Half));
        let px = s2.frame(9).srgb8;
        check(vec![[px[0], px[1], px[2]]], vec![expected(&clip, 9, 1, budget)])
    });
    report.case("temporal: an edit between frames reads its own", [k(I::ResetOnSchema)], || {
        let mut s = Session::new(Route::Export, true, Arc::clone(&clip), Arc::new(Frames));
        s.frame(8);
        // The node's parameter set (an edit that rebuilds the instance).
        let edited = built(50, 30, 1.0, &[fixture(TEMPORAL, true, 1)], None);
        let mut s2 = s.with_scene(Arc::clone(&edited), Arc::new(Frames));
        let px = s2.frame(9).srgb8;
        // Mode 1 passes its input through.
        let own = frame_bytes(59);
        check(vec![[px[0], px[1], px[2]]], vec![[own[0], own[1], own[2]]])
    });
}

/// What a save keeps and what step 4 deleted: the plug-in's entries,
/// each with its switch and values, through a real save and a strict
/// reload; and the renderer's temporal name guess and the OFX chain's
/// temporal entry, gone.
fn kept_cases(report: &mut Report) {
    report.case("ofx temporal: saved and opened", [keyed(Entry::Clip, None, Obligation::Saved)], || {
        let authored = vec![fixture(TEMPORAL, true, 5), fixture(TEMPORAL, false, 0)];
        let s = built(10, 20, 1.0, &authored, None);
        let back = crate::saved::round_trip(&s.sequence, &s.pool)?;
        let clip = back.all_clips().next().ok_or("no clip came back")?;
        let got: Vec<klipp_plugin::AppliedEffect> = clip.picture_ofx()?.into_iter().map(|p| p.entry).collect();
        let same = |a: &klipp_plugin::AppliedEffect, b: &klipp_plugin::AppliedEffect| a.plugin_name == b.plugin_name && a.enabled == b.enabled && a.param_values == b.param_values;
        if got.len() != authored.len() || !got.iter().zip(&authored).all(|(a, b)| same(a, b)) {
            return Err(format!("came back {got:?}, authored {authored:?}"));
        }
        Ok(())
    });
    report.case("ofx temporal: the old stage is gone", [keyed(Entry::Clip, None, Obligation::OldStageGone)], || crate::old_stage::gone(Family::OfxTemporal));
}

pub fn run(report: &mut Report) {
    let budget = klipp_nodes::frame_context::NEIGHBOUR_CEILING as i64;
    klipp_plugin::OfxHost::add_plugin_dirs(&[crate::fixtures::ofx()]);
    time_cases(report, budget);
    entry_cases(report);
    outcome_cases(report);
    instance_cases(report, budget);
    kept_cases(report);
}

#[cfg(test)]
mod tests {
    /// The expectation reads the right frames: a trimmed clip's first
    /// frame takes the handle before it; nothing past the media; two
    /// source frames apart at 2×; the later frame first when reversed.
    #[test]
    fn the_expectation_is_the_fixture_rule() {
        let trimmed = super::scene(50, 30, false);
        assert_eq!(super::expected(&trimmed, 0, 1, 32), [49, 150, 101]);
        let to_the_end = super::scene(90, 10, false);
        assert_eq!(super::expected(&to_the_end, 9, 1, 32), [98, 199, 0]);
        assert_eq!(super::expected(&trimmed, 5, 40, 32), [0, 155, 0], "past the budget: none");
        let fast = super::scene_at(20, 20, 2.0, false);
        assert_eq!(super::expected(&fast, 3, 1, 32), [24, 126, 78], "at 2×: source 26 with 24 and 28 either side");
        let back = super::scene_at(30, 20, -1.0, false);
        let clip = &back.sequence.tracks[0].clips[0];
        assert_eq!((super::source_frame(clip, 4), super::source_frame(clip, 5), super::source_frame(clip, 6)), (Some(45), Some(44), Some(43)), "reversed: the frame before is the later one");
        assert_eq!(super::expected(&back, 5, 1, 32), [45, 144, 93]);
    }
}
