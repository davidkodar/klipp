//! The Ofx family's cases (M4 step 3): OFX plug-ins as nodes, held to the
//! native fixtures' own rules (#932).
//!
//! The fixture bundle (`klipp-plugin/tests/fixtures/ofx/fixture.c`) is
//! built by [`crate::fixtures::ofx`] into a folder of its own, the only
//! one the session's host searches. Each fixture is a byte rule on sRGB RGBA8, so a case's
//! expectation is the rule applied to the clip's picture encoded as the
//! node encodes it (working linear → authored BT.709 → sRGB8; a Rec. 709
//! working space is the authored one), and decoded back:
//!
//! - `identity`: unchanged.
//! - `channels`: (B, 255 − R, G, A).
//! - `alpha`: (R, G, B, ⌊A / 2⌋).
//!
//! Each OFX node converts to 8-bit sRGB and back around its own plug-in;
//! two in series apply their rules in order. A disabled
//! node passes its input on with no conversion, so values above 1 survive
//! exactly. A plug-in that's missing, can't run in the Filter context, or
//! fails its render passes its input on.

use std::sync::Arc;

use klipp_core::{MediaId, RationalTime};
use klipp_plugin::{AppliedEffect, OfxCategory};
use klipp_timeline::{Clip, Sequence};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, Key, Obligation, Outcome, Report, Route};

const FPS: u32 = 25;
const W: u32 = 16;
const H: u32 = 8;

const IDENTITY: &str = "org.klipp.fixture.identity";
const CHANNELS: &str = "org.klipp.fixture.channels";
const ALPHA: &str = "org.klipp.fixture.alpha";

fn key(subject: &str, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::Ofx, subject: subject.into(), entry: Entry::Clip, route, obligation }
}

/// The clip's picture: a ramp across it in red, the other way in green,
/// half blue, opaque; with `hdr`, the red ramp reaches 2.
fn picture(hdr: bool) -> Vec<f32> {
    let n = (W * H) as usize;
    (0..n)
        .flat_map(|i| {
            let r = i as f32 / (n - 1) as f32 * if hdr { 2.0 } else { 1.0 };
            [r, 1.0 - i as f32 / (n - 1) as f32, 0.5, 1.0]
        })
        .collect()
}

struct Still {
    hdr: bool,
}

impl Source for Still {
    fn frame(&self, _media: MediaId, _source_secs: f64) -> Option<KnownFrame> {
        Some(KnownFrame { linear: picture(self.hdr), width: W, height: H, pts: None })
    }
}

fn entry(plugin: &str, enabled: bool, context: OfxCategory) -> AppliedEffect {
    AppliedEffect { plugin_name: plugin.into(), enabled, param_values: Default::default(), context, transition_input: None, transition_amount: 0.0, id: None }
}

/// `clip` carrying `entries`, through its picture.
fn carrying(mut clip: Clip, entries: &[AppliedEffect]) -> Result<Clip, String> {
    let edits: Vec<klipp_timeline::OfxEdit> = entries.iter().map(klipp_timeline::OfxEdit::Add).collect();
    if !edits.is_empty() {
        clip.apply_ofx_edits(&edits)?;
    }
    Ok(clip)
}

fn tc(f: i64) -> RationalTime {
    RationalTime { value: f, rate: FPS }
}

/// A scene of `tracks` (top first) over the known still's media, its
/// host searching only `dir`, `width × height`.
fn scene_of(tracks: Vec<klipp_timeline::Track>, pool: klipp_media::MediaPool, dir: std::path::PathBuf, width: u32, height: u32) -> Arc<Scene> {
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    sequence.tracks = tracks;
    Arc::new(Scene { sequence, pool, width, height, plugin_dirs: vec![dir] })
}

/// The known still as a media item, and its pool.
fn media() -> (MediaId, klipp_media::MediaPool) {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/still.mov".into(), None);
    item.duration = RationalTime { value: 250, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    let media = item.id;
    pool.add(item);
    (media, pool)
}

fn video(clips: Vec<Clip>) -> klipp_timeline::Track {
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = clips;
    t
}

/// A scene of one media clip with `entries`, its host searching only the
/// fixtures.
fn scene(entries: Vec<AppliedEffect>) -> Result<Arc<Scene>, String> {
    placed(Entry::Clip, entries)
}

/// The plug-ins `entries` put where `at` names, the host searching only
/// the fixtures: on a media clip; on a title over a full frame of its own
/// colour (opaque, so the title's picture is the frame's); or on an
/// adjustment layer over a media clip.
fn placed(at: Entry, entries: Vec<AppliedEffect>) -> Result<Arc<Scene>, String> {
    placed_in(at, entries, crate::fixtures::ofx(), W, H)
}

fn placed_in(at: Entry, entries: Vec<AppliedEffect>, dir: std::path::PathBuf, width: u32, height: u32) -> Result<Arc<Scene>, String> {
    let (media, pool) = media();
    let shot = || Clip::media(media, tc(0), tc(50), tc(0), tc(50));
    let tracks = match at {
        Entry::Clip => vec![video(vec![carrying(shot(), &entries)?])],
        Entry::Title => {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "OFX".into(), font_size: 6.0, ..Default::default() });
            td.background = klipp_timeline::TitleBackground::FullFrame { color: [40, 160, 90, 255] };
            let title = Clip { kind: klipp_timeline::ClipKind::Title(td), ..Clip::gap(tc(0), tc(50)) };
            let mut text = klipp_timeline::Track::new("T", klipp_timeline::TrackKind::Text);
            text.clips = vec![carrying(title, &entries)?];
            vec![text]
        }
        Entry::Adjustment => {
            let adjustment = Clip { kind: klipp_timeline::ClipKind::AdjustmentLayer, ..Clip::gap(tc(0), tc(50)) };
            vec![video(vec![carrying(adjustment, &entries)?]), video(vec![shot()])]
        }
        Entry::Conversion => return Err("no plug-in is placed in a conversion".into()),
    };
    Ok(scene_of(tracks, pool, dir, width, height))
}

/// `rules` applied in turn to the picture as the node run encodes it, and
/// decoded back: one conversion at each end of the run.
fn through(hdr: bool, rules: &[fn([u8; 4]) -> [u8; 4]]) -> Vec<f32> {
    let mut bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&picture(hdr));
    for px in bytes.chunks_exact_mut(4) {
        let mut p = [px[0], px[1], px[2], px[3]];
        for rule in rules {
            p = rule(p);
        }
        px.copy_from_slice(&p);
    }
    klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes)
}

/// `picture` as the frame shows it: flattened over the opaque black the
/// timeline starts from (straight colour times its alpha, then opaque). A
/// plug-in that takes alpha away lets the black show through.
fn flattened(mut picture: Vec<f32>) -> Vec<f32> {
    for px in picture.chunks_exact_mut(4) {
        let a = px[3];
        px[0] *= a;
        px[1] *= a;
        px[2] *= a;
        px[3] = 1.0;
    }
    picture
}

fn identity(p: [u8; 4]) -> [u8; 4] {
    p
}

fn channels(p: [u8; 4]) -> [u8; 4] {
    [p[2], 255 - p[0], p[1], p[3]]
}

fn alpha(p: [u8; 4]) -> [u8; 4] {
    [p[0], p[1], p[2], p[3] / 2]
}

/// Frame 10 of `scene` on `route` is `want`, declining nothing a frame
/// fails on: a node switched off passes its input on and may name itself
/// a bypass, which no route refuses (the viewer doesn't show it).
fn passes_on(route: Route, gpu: bool, scene: Arc<Scene>, hdr: bool, want: &[f32]) -> Result<(), String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still { hdr }));
    let got = s.frame(10);
    if let Some(d) = got.declines().into_iter().find(|d| !d.contains("(bypassed;")) {
        return Err(format!("frame 10 declined {d}"));
    }
    let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-6 };
    let off = got.off(want);
    if off > tolerance {
        return Err(format!("{off} from the expected picture"));
    }
    Ok(())
}

/// Frame 10 of the scene on `route` is `want`, declining nothing.
fn draws(route: Route, gpu: bool, scene: Arc<Scene>, hdr: bool, want: &[f32]) -> Result<(), String> {
    draws_declining(route, gpu, scene, hdr, want, None)
}

/// [`draws`], declining one node with the outcome `declined` labels, or
/// nothing (M4 step 10).
fn draws_declining(route: Route, gpu: bool, scene: Arc<Scene>, hdr: bool, want: &[f32], declined: Option<&str>) -> Result<(), String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still { hdr }));
    let got = s.frame(10);
    got.declined_as(declined)?;
    // The viewer's GPU stack draws display bytes only: within a byte.
    let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-6 };
    let off = got.off(want);
    if off > tolerance {
        return Err(format!("{off} from the expected picture"));
    }
    Ok(())
}

/// Frame 10 of `scene` on `route`, as the test's ground truth for a scene
/// without the node: its linear frame, or its display bytes decoded.
fn bare(route: Route, gpu: bool, scene: Arc<Scene>) -> Result<Vec<f32>, String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still { hdr: false }));
    let f = s.frame(10);
    f.declined_as(None)?;
    Ok(f.linear.clone().unwrap_or_else(|| klipp_effects::srgb_u8_to_linear_f32_rgba(&f.srgb8)))
}

/// `rules` applied in turn to `bare_scene`'s frame as the node applies
/// them (its bytes, sRGB8), decoded and flattened over the timeline's
/// black: what the node draws on an opaque picture.
///
/// The frame is the renderer's linear one on either route. A plug-in
/// declines the viewer's GPU stack, so the node is drawn there by the
/// renderer too, and that is its ground truth; the stack's display bytes
/// for the bare scene can sit a byte apart (an adjustment layer over a
/// clip does), which the node's own frame never sees.
fn ruled(gpu: bool, bare_scene: Arc<Scene>, rules: &[fn([u8; 4]) -> [u8; 4]]) -> Result<Vec<f32>, String> {
    let mut bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&bare(Route::Export, gpu, bare_scene)?);
    for px in bytes.chunks_exact_mut(4) {
        let mut p = [px[0], px[1], px[2], px[3]];
        for rule in rules {
            p = rule(p);
        }
        px.copy_from_slice(&p);
    }
    Ok(flattened(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes)))
}

fn key_at(subject: &str, at: Entry, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::Ofx, subject: subject.into(), entry: at, route, obligation }
}

const COUNT: &str = "org.klipp.fixture.count";

/// The count fixture's rule for its `n`th render: red is `n`.
fn counted(n: u8) -> impl Fn([u8; 4]) -> [u8; 4] {
    move |p| [n, p[1], p[2], p[3]]
}

/// `rules` then the count fixture's `n`th render, applied to the picture
/// as one run encodes it, flattened: what `[subject, count]` draws.
fn through_counted(rules: &[fn([u8; 4]) -> [u8; 4]], n: u8) -> Vec<f32> {
    through_counted_on(&picture(false), rules, n)
}

/// [`through_counted`] on `input`.
fn through_counted_on(input: &[f32], rules: &[fn([u8; 4]) -> [u8; 4]], n: u8) -> Vec<f32> {
    let count = counted(n);
    let mut bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(input);
    for px in bytes.chunks_exact_mut(4) {
        let mut p = [px[0], px[1], px[2], px[3]];
        for rule in rules {
            p = rule(p);
        }
        px.copy_from_slice(&count(p));
    }
    flattened(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes))
}

/// Frame `frame` of `s` is `want`, declining nothing.
fn frame_is(s: &mut Session, frame: i64, want: &[f32], what: &str) -> Result<(), String> {
    let got = s.frame(frame);
    got.declined_as(None)?;
    let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-6 };
    let off = got.off(want);
    if off > tolerance {
        return Err(format!("{what}: frame {frame} {off} from the expected picture"));
    }
    Ok(())
}

pub fn run(report: &mut Report) {
    let filter = OfxCategory::Filter;
    let subjects: [(&str, fn([u8; 4]) -> [u8; 4]); 3] = [(IDENTITY, identity), (CHANNELS, channels), (ALPHA, alpha)];
    for route in [Route::Viewer, Route::Export] {
        for (subject, rule) in subjects {
            report.case(&format!("ofx {route:?}: {subject}"), [key(subject, Some(route), Obligation::ExpectedPixels)], || {
                draws(route, true, scene(vec![entry(subject, true, filter)])?, false, &flattened(through(false, &[rule])))
            });
            report.case(&format!("ofx {route:?}: {subject}, the CPU and the GPU agree"), [key(subject, Some(route), Obligation::CpuGpuAgree)], || {
                let want = flattened(through(false, &[rule]));
                draws(route, false, scene(vec![entry(subject, true, filter)])?, false, &want)?;
                draws(route, true, scene(vec![entry(subject, true, filter)])?, false, &want)
            });
            // Switched off, the input as it was: above 1 too, no conversion.
            report.case(&format!("ofx {route:?}: {subject} switched off"), [key(subject, Some(route), Obligation::DisabledIsIdentity)], || {
                passes_on(route, true, scene(vec![entry(subject, false, filter)])?, true, &picture(true))
            });
            // On a title and on an adjustment layer: the rule on the
            // picture the node is given, which the same scene without it
            // draws (opaque either way).
            for at in [Entry::Title, Entry::Adjustment] {
                report.case(&format!("ofx {route:?}: {subject} on {at:?}"), [key_at(subject, at, Some(route), Obligation::ExpectedPixels)], || {
                    let want = ruled(true, placed(at, vec![])?, &[rule])?;
                    draws(route, true, placed(at, vec![entry(subject, true, filter)])?, false, &want)
                });
                report.case(&format!("ofx {route:?}: {subject} on {at:?}, the CPU and the GPU agree"), [key_at(subject, at, Some(route), Obligation::CpuGpuAgree)], || {
                    for gpu in [false, true] {
                        let want = ruled(gpu, placed(at, vec![])?, &[rule])?;
                        draws(route, gpu, placed(at, vec![entry(subject, true, filter)])?, false, &want)?;
                    }
                    Ok(())
                });
                report.case(&format!("ofx {route:?}: {subject} on {at:?} switched off"), [key_at(subject, at, Some(route), Obligation::DisabledIsIdentity)], || {
                    let want = bare(route, true, placed(at, vec![])?)?;
                    passes_on(route, true, placed(at, vec![entry(subject, false, filter)])?, false, &want)
                });
            }
            // Its outcomes: switched off, the input passed on and nothing a
            // frame fails on; each other declined by name, the input passed on.
            report.case(&format!("ofx {route:?}: {subject} switched off is a bypass"), [key(subject, Some(route), Obligation::Outcome(Outcome::Bypass))], || {
                passes_on(route, true, scene(vec![entry(subject, false, filter)])?, false, &picture(false))
            });
            report.case(&format!("ofx {route:?}: {subject} failing its render"), [key(subject, Some(route), Obligation::Outcome(Outcome::RenderFailed))], || {
                // The fixture's own failure: `mode` 2 fails its render.
                let mut failing = entry(subject, true, filter);
                failing.param_values.insert("mode".into(), klipp_core::OfxValue::Int(2));
                draws_declining(route, true, scene(vec![failing])?, false, &picture(false), Some("render failed"))
            });
            report.case(&format!("ofx {route:?}: {subject} not installed"), [key(subject, Some(route), Obligation::Outcome(Outcome::PluginMissing))], || {
                let s = placed_in(Entry::Clip, vec![entry(subject, true, filter)], crate::fixtures::no_plugins(), W, H)?;
                draws_declining(route, true, s, false, &picture(false), Some("plug-in missing"))
            });
            report.case(&format!("ofx {route:?}: {subject} in a context it hasn't"), [key(subject, Some(route), Obligation::Outcome(Outcome::Unsupported))], || {
                // Each fixture offers Filter only: a Transition entry is
                // refused, named, never drawn as one.
                draws_declining(route, true, scene(vec![entry(subject, true, OfxCategory::Transition)])?, false, &picture(false), Some("unsupported"))
            });
        }
        // Two in series, each rule in turn on the one before's picture.
        report.case(&format!("ofx {route:?}: channels then alpha, in order"), [key(CHANNELS, Some(route), Obligation::ExpectedPixels)], || {
            draws(route, true, scene(vec![entry(CHANNELS, true, filter), entry(ALPHA, true, filter)])?, false, &flattened(through(false, &[channels, alpha])))
        });
        report.case(&format!("ofx {route:?}: an entry applied in the General context"), [key(CHANNELS, Some(route), Obligation::Outcome(Outcome::Unsupported))], || {
            // The host's General is `Other`: only a Filter entry runs as a
            // filter, so it's refused, named, never drawn as one.
            draws_declining(route, true, scene(vec![entry(CHANNELS, true, OfxCategory::Other)])?, false, &picture(false), Some("unsupported"))
        });
    }
    instances(report);
    for (subject, _) in subjects {
        // Saved and opened: each entry's plug-in, switch and values kept.
        report.case(&format!("ofx: {subject} saved and opened"), [key(subject, None, Obligation::Saved)], || {
            let mut valued = entry(subject, true, filter);
            valued.param_values.insert("mode".into(), klipp_core::OfxValue::Int(1));
            let authored = vec![valued, entry(subject, false, filter)];
            let s = scene(authored.clone())?;
            let back = crate::saved::round_trip(&s.sequence, &s.pool)?;
            let clip = back.all_clips().next().ok_or("no clip came back")?;
            let got: Vec<AppliedEffect> = clip.picture_ofx()?.into_iter().map(|p| p.entry).collect();
            let same = |a: &AppliedEffect, b: &AppliedEffect| a.plugin_name == b.plugin_name && a.enabled == b.enabled && a.param_values == b.param_values && a.context == b.context;
            if got.len() != authored.len() || !got.iter().zip(&authored).all(|(a, b)| same(a, b)) {
                return Err(format!("came back {got:?}, authored {authored:?}"));
            }
            Ok(())
        });
        report.case(&format!("ofx: {subject}'s old stage gone"), [key(subject, None, Obligation::OldStageGone)], || crate::old_stage::gone(Family::Ofx));
        // Level 2: the viewer shows export's frame, each read from one
        // real file of the clip's picture (one case earns both routes).
        for at in [Entry::Clip, Entry::Title, Entry::Adjustment] {
            let keys = [key_at(subject, at, Some(Route::Viewer), Obligation::ViewerEqualsExport), key_at(subject, at, Some(Route::Export), Obligation::ViewerEqualsExport)];
            report.case(&format!("ofx: {subject} on {at:?}, the viewer shows export's frame"), keys, || {
                let s = crate::on_file::on_file(&*placed(at, vec![entry(subject, true, filter)])?, "ofx-still", FPS, 50, &|| picture(false))?;
                crate::route2::viewer_equals_export(s, &[10])
            });
        }
    }
}

/// Each subject's instance cases (export's route unless a case is about
/// the viewer): the subject's rule holds throughout, and a count fixture
/// beside it shows which instance drew each frame (red = its renders so
/// far), so a shared instance or a missed reset is seen, not passed.
fn instances(report: &mut Report) {
    use crate::InstanceCase::*;
    let filter = OfxCategory::Filter;
    let subjects: [(&str, fn([u8; 4]) -> [u8; 4]); 3] = [(IDENTITY, identity), (CHANNELS, channels), (ALPHA, alpha)];
    let still = || Arc::new(Still { hdr: false }) as Arc<dyn Source>;
    for (subject, rule) in subjects {
        let k = |i| [key(subject, None, Obligation::Instances(i))];
        let pair = move || vec![entry(subject, true, filter), entry(COUNT, true, filter)];
        report.case(&format!("ofx: two {subject} nodes and two counts, each its own instance"), k(TwoNodesOfOnePlugin), || {
            let entries = vec![entry(subject, true, filter), entry(subject, true, filter), entry(COUNT, true, filter), entry(COUNT, true, filter)];
            let mut s = Session::new(Route::Export, true, scene(entries)?, still());
            // The second count's own first render: one shared would count 2.
            frame_is(&mut s, 10, &through_counted(&[rule, rule], 1), "two nodes")?;
            frame_is(&mut s, 11, &through_counted(&[rule, rule], 2), "two nodes, played on")
        });
        report.case(&format!("ofx: {subject} on two clips, each its own instance"), k(TwoClips), || {
            let (media, pool) = media();
            let a = carrying(Clip::media(media, tc(0), tc(50), tc(0), tc(50)), &pair())?;
            let b = carrying(Clip::media(media, tc(50), tc(100), tc(0), tc(50)), &pair())?;
            let mut s = Session::new(Route::Export, true, scene_of(vec![video(vec![a, b])], pool, crate::fixtures::ofx(), W, H), still());
            frame_is(&mut s, 10, &through_counted(&[rule], 1), "the first clip")?;
            frame_is(&mut s, 11, &through_counted(&[rule], 2), "the first clip, on")?;
            // The second clip's own instance: shared, it would count 3.
            frame_is(&mut s, 51, &through_counted(&[rule], 1), "the second clip")
        });
        report.case(&format!("ofx: {subject} on the viewer and export at once, each its own"), k(ViewerAndExportTogether), || {
            let mut viewer = Session::new(Route::Viewer, true, scene(pair())?, still());
            let mut export = Session::new(Route::Export, true, scene(pair())?, still());
            for (n, f) in [(1, 10), (2, 11), (3, 12)] {
                frame_is(&mut viewer, f, &through_counted(&[rule], n), "the viewer")?;
                frame_is(&mut export, f, &through_counted(&[rule], n), "export")?;
            }
            Ok(())
        });
        report.case(&format!("ofx: {subject} starts fresh after a seek"), k(ResetOnSeek), || {
            let mut s = Session::new(Route::Export, true, scene(pair())?, still());
            frame_is(&mut s, 10, &through_counted(&[rule], 1), "played")?;
            frame_is(&mut s, 11, &through_counted(&[rule], 2), "played on")?;
            frame_is(&mut s, 30, &through_counted(&[rule], 1), "a seek ahead")?;
            frame_is(&mut s, 5, &through_counted(&[rule], 1), "a seek back")
        });
        report.case(&format!("ofx: {subject} starts fresh played backward"), k(ResetOnReverse), || {
            let mut s = Session::new(Route::Export, true, scene(pair())?, still());
            frame_is(&mut s, 12, &through_counted(&[rule], 1), "the first frame")?;
            // A frame back is never play: each one starts it fresh.
            frame_is(&mut s, 11, &through_counted(&[rule], 1), "one back")?;
            frame_is(&mut s, 10, &through_counted(&[rule], 1), "another back")
        });
        report.case(&format!("ofx: {subject} starts fresh at a new size"), k(ResetOnResolution), || {
            let mut s = Session::new(Route::Export, true, scene(pair())?, still());
            frame_is(&mut s, 10, &through_counted(&[rule], 1), "the first size")?;
            frame_is(&mut s, 11, &through_counted(&[rule], 2), "played on")?;
            // The same picture drawn at twice the size: a new instance.
            let big = placed_in(Entry::Clip, pair(), crate::fixtures::ofx(), W * 2, H * 2)?;
            let mut s = s.with_scene(big, Arc::new(Big));
            frame_is(&mut s, 12, &through_counted_on(&Big::picture(), &[rule], 1), "the new size")
        });
        report.case(&format!("ofx: {subject} starts fresh under another schema"), k(ResetOnSchema), || {
            let mut s = Session::new(Route::Export, true, scene(pair())?, still());
            frame_is(&mut s, 10, &through_counted(&[rule], 1), "version 1")?;
            frame_is(&mut s, 11, &through_counted(&[rule], 2), "version 1, on")?;
            // The same plug-ins reloaded as version 2 (a parameter more).
            let mut host = klipp_plugin::OfxHost::new();
            host.discover_plugins_in(&[crate::fixtures::ofx_v2()]);
            *s.renderer_mut().ofx_host() = host;
            frame_is(&mut s, 12, &through_counted(&[rule], 1), "version 2")
        });
    }
}

/// The known still at twice the size.
struct Big;

impl Big {
    fn picture() -> Vec<f32> {
        let small = picture(false);
        let mut linear = Vec::with_capacity(small.len() * 4);
        for y in 0..H * 2 {
            for x in 0..W * 2 {
                let at = (((y / 2) * W + x / 2) * 4) as usize;
                linear.extend_from_slice(&small[at..at + 4]);
            }
        }
        linear
    }
}

impl Source for Big {
    fn frame(&self, _media: MediaId, _source_secs: f64) -> Option<KnownFrame> {
        Some(KnownFrame { linear: Big::picture(), width: W * 2, height: H * 2, pts: None })
    }
}

#[cfg(test)]
mod family {
    /// The family's cases alone, for working on them (the gate runs them
    /// with every other family).
    #[test]
    #[ignore = "renders through the fixture bundle; run on request"]
    fn the_ofx_cases_pass() {
        let mut report = crate::Report::default();
        super::run(&mut report);
        assert!(report.failures.is_empty(), "{}", report.failures.join("\n"));
        eprintln!("earned {}", report.passed.len());
    }
}
