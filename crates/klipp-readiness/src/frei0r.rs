//! The frei0r family's cases (M4 step 5), on step 2's native fixtures
//! through the level-1 routes (`route`).
//!
//! Each case renders a scene twice on the same route: without the effect
//! (the picture the node is given) and with it. The expectation is the
//! fixture's rule (the same one `scripts/plugin-tests.py` holds the plug-in
//! to) applied to the first, so a case says what the node must draw, never
//! what an older route drew.
//!
//! Earned here: each subject's pixels, CPU/GPU agreement, disabled
//! identity and viewer = export (level 2, [`crate::route2`]) on every entry
//! and route; its instance cases (the stateful fixture's history, and the
//! stateless ones held to their rule frame by frame, whatever the order);
//! step 10's outcomes, each visible on the frame; and each subject's
//! entries kept by a save, the chain's GPU veto gone, and its frame time
//! within its budget.

use std::sync::Arc;

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, ClipKind, EffectEdit, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

/// A title's full-frame background: its solid pixels besides the text.
const TITLE_BG: [u8; 3] = [64, 128, 192];

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, InstanceCase, Key, Obligation, Report, Route};

/// The subjects (`families`' names for step 2's fixtures).
pub const COLOUR: &str = "frei0r.fixture.colour";
pub const STATEFUL: &str = "frei0r.fixture.stateful";
pub const TILED: &str = "frei0r.fixture.tiled";

const W: u32 = 64;
const H: u32 = 48;
const FPS: u32 = 25;
const ROUTES: [Route; 2] = [Route::Viewer, Route::Export];

fn key(subject: &str, entry: Entry, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::Frei0r, subject: subject.into(), entry, route, obligation }
}

fn tc(frame: i64) -> RationalTime {
    RationalTime { value: frame, rate: FPS }
}

/// A source whose every frame differs, and whose every pixel is a code
/// that survives sRGB8 → linear → sRGB8: what the node is handed is
/// exactly what the fixture's rule is applied to.
struct Pattern {
    width: u32,
    height: u32,
}

impl Pattern {
    fn srgb8(width: u32, height: u32, frame: i64) -> Vec<u8> {
        let mut v = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as i64 {
            for x in 0..width as i64 {
                let r = ((x * 255) / (width as i64 - 1).max(1) + frame * 7).rem_euclid(256) as u8;
                let g = ((y * 255) / (height as i64 - 1).max(1)) as u8;
                let b = ((x * 31 + y * 17 + frame * 13).rem_euclid(256)) as u8;
                v.extend_from_slice(&[r, g, b, 255]);
            }
        }
        v
    }
}

impl Source for Pattern {
    fn frame(&self, _media: MediaId, source_secs: f64) -> Option<KnownFrame> {
        let frame = (source_secs * FPS as f64 + 1e-9).floor() as i64;
        let srgb = Self::srgb8(self.width, self.height, frame);
        Some(KnownFrame { linear: klipp_effects::srgb_u8_to_linear_f32_rgba(&srgb), width: self.width, height: self.height, pts: Some(frame as f64 / FPS as f64) })
    }
}

/// The fixtures, built and registered as the only frei0r plug-ins: their
/// type ids by stem.
struct Plugins {
    colour: String,
    history: String,
    neighbour: String,
    /// Makes no instance (a render failure).
    refuses: String,
    /// The history fixture as `delay0r.so`: a listed history plug-in, for
    /// the refusal above 2048.
    delay: String,
}

fn plugins() -> Result<Plugins, String> {
    let mut infos = Vec::new();
    let mut id = |kind: u32, stem: &str| -> Result<String, String> {
        let so = crate::fixtures::frei0r(kind, stem);
        let info = klipp_frei0r::host::scan_library(&so).into_iter().next().ok_or_else(|| format!("{stem} isn't a frei0r plug-in"))?;
        let id = info.type_id();
        infos.push(info);
        Ok(id)
    };
    let p = Plugins {
        colour: id(0, "klipp-fixture-colour")?,
        history: id(3, "klipp-fixture-history")?,
        neighbour: id(6, "klipp-fixture-neighbour")?,
        refuses: id(7, "klipp-fixture-refuses")?,
        delay: id(3, "delay0r")?,
    };
    klipp_effects::frei0r_effect::set_plugin_registry(infos);
    Ok(p)
}

fn frei0r(type_id: &str) -> BuiltinEffect {
    BuiltinEffect::Frei0r { type_id: type_id.into(), params: Vec::new() }
}

/// `clip` with `fx`, each switched on or off, written through its
/// picture.
fn with_effects(mut clip: Clip, fx: &[BuiltinEffect], enabled: bool) -> Clip {
    let mut edits: Vec<EffectEdit> = fx.iter().map(EffectEdit::Add).collect();
    if !enabled {
        edits.extend((0..fx.len()).map(|i| EffectEdit::SetEnabled(i, false)));
    }
    if !edits.is_empty() {
        match clip.apply_effect_edits(&edits) {
            Some(Ok(_)) => {}
            other => panic!("the effects didn't land on the clip: {:?}", other.map(|r| r.err())),
        }
    }
    clip
}

fn track(clips: Vec<Clip>) -> Track {
    let mut t = Track::new("V", TrackKind::Video);
    t.clips = clips;
    t
}

/// A scene of `entry` with `fx` (none: the picture the node is given),
/// `width × height`, 100 frames.
fn scene(entry: Entry, fx: &[BuiltinEffect], enabled: bool, width: u32, height: u32) -> Arc<Scene> {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/pattern.mov".into(), None);
    item.fps = klipp_core::Fps::new(FPS, 1);
    item.duration = tc(400);
    item.resolution = klipp_core::Resolution { width, height };
    let media = item.id;
    pool.add(item);
    let footage = || Clip::media(media, tc(0), tc(100), tc(0), tc(100));
    let tracks = match entry {
        Entry::Clip => vec![track(vec![with_effects(footage(), fx, enabled)])],
        // Top first: the layer over the footage it adjusts.
        Entry::Adjustment => {
            let adj = Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0), tc(100)) };
            vec![track(vec![with_effects(adj, fx, enabled)]), track(vec![footage()])]
        }
        Entry::Title => {
            // Points on a 1080-line frame (the renderer scales by the short side
            // over 1080): 60% of the frame's height.
            let text = TextBox { text: "MMMM".into(), font_size: 0.6 * 1080.0, color: [255, 255, 255, 255], ..Default::default() };
            // On a full-frame background, so the layer is opaque: what the
            // node is given is the title's picture itself.
            let mut data = TitleData::single_text(text);
            data.background = TitleBackground::FullFrame { color: [TITLE_BG[0], TITLE_BG[1], TITLE_BG[2], 255] };
            let title = Clip { kind: ClipKind::Title(data), ..Clip::gap(tc(0), tc(100)) };
            vec![track(vec![with_effects(title, fx, enabled)])]
        }
        Entry::Conversion => unreachable!("not a frei0r entry"),
    };
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    sequence.resolution = klipp_core::Resolution { width, height };
    sequence.tracks = tracks;
    Arc::new(Scene { sequence, pool, width, height, plugin_dirs: Vec::new() })
}

/// The pattern at `scene`'s size.
fn pattern(scene: &Scene) -> Arc<dyn Source> {
    Arc::new(Pattern { width: scene.width, height: scene.height })
}

/// `scene` reading a real file of the pattern (level 2): the same
/// pictures, decoded by the viewer's and export's own readers.
fn on_file(scene: &Arc<Scene>) -> Result<Arc<Scene>, String> {
    let (w, h) = (scene.width, scene.height);
    let path = crate::route2::known_media(&format!("frei0r-pattern-{w}x{h}"), w, h, FPS, 120, &|i| klipp_effects::srgb_u8_to_linear_f32_rgba(&Pattern::srgb8(w, h, i)))?;
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

/// Frames `frames` of `scene` on `route`, by one render owner, as sRGB8.
fn render(route: Route, gpu: bool, scene: &Arc<Scene>, frames: &[i64]) -> Vec<Vec<u8>> {
    let mut s = Session::new(route, gpu, Arc::clone(scene), pattern(scene));
    frames.iter().map(|&f| s.frame(f).srgb8).collect()
}

/// The fixtures' rules, on a picture's sRGB8 (`klipp-frei0r/tests/fixtures/
/// frei0r/fixture.c`; the runner holds the plug-ins to the same).
fn colour(px: &[u8], clip_time: f64) -> Vec<u8> {
    px.chunks_exact(4).flat_map(|p| [((p[2] as i64 + (clip_time * 16.0) as i64).rem_euclid(256)) as u8, 255 - p[0], p[1], p[3]]).collect()
}

fn neighbour(px: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as i64, h as i64);
    let at = |x: i64, y: i64, c: usize| px[((y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) * 4) as usize + c] as u32;
    let mut out = Vec::with_capacity(px.len());
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let sum: u32 = (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))).map(|(dx, dy)| at(x + dx, y + dy, c)).sum();
                out.push(((sum + 4) / 9) as u8);
            }
        }
    }
    out
}

/// `got` against `want`, every pixel. A title must have drawn something
/// besides its background in the picture the node was given, so a title
/// drawing nothing can't pass.
fn compare(entry: Entry, given: &[u8], got: &[u8], want: &[u8]) -> Result<(), String> {
    if got.len() != want.len() {
        return Err(format!("{} bytes, expected {}", got.len(), want.len()));
    }
    if entry == Entry::Title && given.chunks_exact(4).all(|p| p[..3] == TITLE_BG) {
        return Err("the title drew nothing but its background".into());
    }
    let mut off = 0;
    for (i, (g, w)) in got.chunks_exact(4).zip(want.chunks_exact(4)).enumerate() {
        if g[..3] != w[..3] {
            off += 1;
            if off == 1 {
                eprintln!("frei0r case: pixel {i}: got {:?}, expected {:?}", &g[..3], &w[..3]);
            }
        }
    }
    if off == 0 { Ok(()) } else { Err(format!("{off} of {} pixels differ", got.len() / 4)) }
}

/// The largest difference between two frames, in sRGB codes.
fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(u8::MAX)
}

/// A subject's per-entry keys: its pixels, CPU/GPU agreement and disabled
/// identity, on each route. `expect(given frames, i)` is frame `i`'s
/// expectation from the pictures the node was given.
fn entry_cases(report: &mut Report, subject: &str, type_id: &str, entry: Entry, size: (u32, u32), frames: &[i64], expect: &dyn Fn(&[Vec<u8>], usize) -> Vec<u8>) {
    let fx = [frei0r(type_id)];
    let (with, without, off) = (scene(entry, &fx, true, size.0, size.1), scene(entry, &[], true, size.0, size.1), scene(entry, &fx, false, size.0, size.1));
    for route in ROUTES {
        let given = render(route, true, &without, frames);
        let got = render(route, true, &with, frames);
        report.case(&format!("{subject} {entry:?} {route:?} pixels"), [key(subject, entry, Some(route), Obligation::ExpectedPixels)], || {
            for (i, f) in got.iter().enumerate() {
                compare(entry, &given[i], f, &expect(&given, i)).map_err(|e| format!("frame {}: {e}", frames[i]))?;
            }
            Ok(())
        });
        report.case(&format!("{subject} {entry:?} {route:?} cpu=gpu"), [key(subject, entry, Some(route), Obligation::CpuGpuAgree)], || {
            let cpu = render(route, false, &with, frames);
            let w = cpu.iter().zip(&got).map(|(a, b)| worst(a, b)).max().unwrap_or(0);
            if w > 1 { Err(format!("CPU and GPU differ by {w} codes")) } else { Ok(()) }
        });
        report.case(&format!("{subject} {entry:?} {route:?} disabled"), [key(subject, entry, Some(route), Obligation::DisabledIsIdentity)], || {
            let disabled = render(route, true, &off, frames);
            if disabled == given { Ok(()) } else { Err("switched off, the picture isn't the one it was given".into()) }
        });
    }
    // Level 2: the engine's viewer shows export's frame, from one file.
    // One frame, so a stateful plug-in starts fresh on both (the viewer
    // parks, a seek; export would carry history from the frame before).
    let vee = ROUTES.map(|r| key(subject, entry, Some(r), Obligation::ViewerEqualsExport));
    report.case(&format!("{subject} {entry:?} viewer = export"), vee, || crate::route2::viewer_equals_export(on_file(&with)?, &frames[..1]));
}

/// The history fixture on a clip, by one owner: which source frame each
/// rendered frame shows (its output is the frame before's input).
fn shows(route: Route, scene: &Arc<Scene>, frames: &[i64]) -> Vec<i64> {
    let picture = |n: i64| Pattern::srgb8(scene.width, scene.height, n);
    render(route, true, scene, frames)
        .into_iter()
        .map(|got| (0..200).find(|&n| picture(n) == got).unwrap_or(-1))
        .collect()
}

/// A subject's instance cases. The stateful fixture shows the frame
/// before its input: the history that must start again, or stay apart.
/// A stateless one has no history to carry: each frame is held to its
/// rule on that frame's input (`rule(picture, frame, w, h)`), in whatever
/// order the case asks, so an instance carried over shows as a wrong frame.
fn instance_cases(report: &mut Report, subject: &str, type_id: &str, rule: Option<&dyn Fn(&[u8], i64, u32, u32) -> Vec<u8>>) {
    let instance = |i| key(subject, Entry::Clip, None, Obligation::Instances(i));
    let clip = scene(Entry::Clip, &[frei0r(type_id)], true, W, H);
    // `want`: the source frames the stateful fixture shows; a stateless
    // one is held to its rule on the frames asked.
    let check = |scene: &Arc<Scene>, route: Route, frames: &[i64], want: &[i64]| -> Result<(), String> {
        match rule {
            None => {
                let got = shows(route, scene, frames);
                if got == want { Ok(()) } else { Err(format!("showed frames {got:?}, expected {want:?}")) }
            }
            Some(rule) => {
                for (f, got) in frames.iter().zip(render(route, true, scene, frames)) {
                    // Its input is source frame `f` (each clip's in-point is
                    // its timeline start); its time is the clip's own.
                    let start = scene.sequence.tracks[0].clips.iter().filter(|c| c.timeline_in.value <= *f).map(|c| c.timeline_in.value).max().unwrap_or(0);
                    let want = rule(&Pattern::srgb8(scene.width, scene.height, *f), f - start, scene.width, scene.height);
                    if got != want {
                        return Err(format!("frame {f} isn't the rule on its own input"));
                    }
                }
                Ok(())
            }
        }
    };
    report.case(&format!("{subject}: a seek starts again"), [instance(InstanceCase::ResetOnSeek)], || {
        // Play 10, 11, 12; jump to 40 (a seek); play 41.
        check(&clip, Route::Export, &[10, 11, 12, 40, 41], &[10, 10, 11, 40, 40])
    });
    report.case(&format!("{subject}: a step back starts again"), [instance(InstanceCase::ResetOnReverse)], || check(&clip, Route::Viewer, &[20, 21, 19, 18], &[20, 20, 19, 18]));
    report.case(&format!("{subject}: viewer and export each their own"), [instance(InstanceCase::ViewerAndExportTogether)], || {
        let mut viewer = Session::new(Route::Viewer, true, Arc::clone(&clip), pattern(&clip));
        let mut export = Session::new(Route::Export, true, Arc::clone(&clip), pattern(&clip));
        let at = |n| Pattern::srgb8(W, H, n);
        // Interleaved: each owner's frames are its own.
        for (i, (v, e)) in [(0, 30), (1, 31), (2, 32)].into_iter().enumerate() {
            let (vf, ef) = (viewer.frame(v).srgb8, export.frame(e).srgb8);
            let (wv, we) = match rule {
                None if i == 0 => (at(v), at(e)),
                None => (at(v - 1), at(e - 1)),
                Some(rule) => (rule(&at(v), v, W, H), rule(&at(e), e, W, H)),
            };
            if vf != wv || ef != we {
                return Err(format!("at step {i}: an owner showed the other's frame"));
            }
        }
        Ok(())
    });
    report.case(&format!("{subject}: two clips each their own"), [instance(InstanceCase::TwoClips)], || {
        // Two clips of the plug-in on one timeline, one after the other:
        // the second starts its own.
        let mut s = (*clip).clone();
        let first = s.sequence.tracks[0].clips[0].clone();
        let mut second = first.clone();
        second.id = klipp_core::ClipId::new();
        second.timeline_in = tc(100);
        second.timeline_out = tc(200);
        second.source_in = tc(100);
        second.source_out = tc(200);
        s.sequence.tracks[0].clips.push(second);
        check(&Arc::new(s), Route::Export, &[98, 99, 100, 101], &[98, 98, 100, 100])
    });
    report.case(&format!("{subject}: two nodes of one plug-in"), [instance(InstanceCase::TwoNodesOfOnePlugin)], || {
        let twice = scene(Entry::Clip, &[frei0r(type_id), frei0r(type_id)], true, W, H);
        match rule {
            // Two in a row: each a frame behind, so the picture is two behind.
            None => check(&twice, Route::Export, &[50, 51, 52, 53], &[50, 50, 50, 51]),
            // The rule twice, each node on the one before's picture.
            Some(rule) => {
                for (f, got) in [50, 51, 52].iter().zip(render(Route::Export, true, &twice, &[50, 51, 52])) {
                    let once = rule(&Pattern::srgb8(W, H, *f), *f, W, H);
                    if got != rule(&once, *f, W, H) {
                        return Err(format!("frame {f} isn't the rule applied twice"));
                    }
                }
                Ok(())
            }
        }
    });
    report.case(&format!("{subject}: a new size starts again"), [instance(InstanceCase::ResetOnResolution)], || {
        let small = scene(Entry::Clip, &[frei0r(type_id)], true, W / 2, H / 2);
        let mut s = Session::new(Route::Export, true, Arc::clone(&clip), pattern(&clip));
        s.frame(60);
        s.frame(61);
        // The same owner at another size (a render-scale change).
        let mut s2 = s.with_scene(Arc::clone(&small), pattern(&small));
        let got = s2.frame(62).srgb8;
        let input = Pattern::srgb8(W / 2, H / 2, 62);
        let want = rule.map_or_else(|| input.clone(), |r| r(&input, 62, W / 2, H / 2));
        if got == want { Ok(()) } else { Err("the instance from the other size carried".into()) }
    });
    report.case(&format!("{subject}: a changed schema starts again"), [instance(InstanceCase::ResetOnSchema)], || {
        // The same owner, the node's parameters changed between frames (an
        // edit that rebuilds the instance): the next frame is its own.
        let mut s = Session::new(Route::Export, true, Arc::clone(&clip), pattern(&clip));
        s.frame(70);
        s.frame(71);
        let edited = scene(Entry::Clip, &[frei0r(type_id), frei0r(type_id)], true, W, H);
        let mut s2 = s.with_scene(Arc::clone(&edited), pattern(&edited));
        let got = s2.frame(72).srgb8;
        let input = Pattern::srgb8(W, H, 72);
        // Two fresh nodes: the stateful pair passes its input (each starts
        // fresh), a stateless pair is its rule twice.
        let want = rule.map_or_else(|| input.clone(), |r| r(&r(&input, 72, W, H), 72, W, H));
        if got == want { Ok(()) } else { Err("an instance from before the edit carried".into()) }
    });
}

/// Frame `frame` of `scene` on `route`.
fn framed(route: Route, scene: &Arc<Scene>, frame: i64) -> crate::route::Frame {
    let mut s = Session::new(route, true, Arc::clone(scene), pattern(scene));
    s.frame(frame)
}

/// Step 10's outcomes for each subject's node on a clip, on both routes:
/// distinct, visible (the frame names the node's outcome), and the input
/// passed on, never a picture made up. The bypass and the plug-in missing
/// are each subject's own; a render failure and an unsupported frame are
/// the frei0r node's (a fixture that makes no instance; a history plug-in
/// above 2048), earned for every subject the node runs.
fn outcome_cases(report: &mut Report, subjects: &[(&str, &str, &str)], refuses_id: &str, delay_id: &str) {
    use crate::Outcome;
    for route in ROUTES {
        let given = |w, h| render(route, true, &scene(Entry::Clip, &[], true, w, h), &[0]).remove(0);
        let holds = |frame: crate::route::Frame, label: &str, w, h| -> Result<(), String> {
            frame.declined_as(Some(label))?;
            if frame.srgb8 != given(w, h) { Err(format!("declined {label}, but the picture isn't its input")) } else { Ok(()) }
        };
        for &(subject, type_id, stem) in subjects {
            let k = |o| key(subject, Entry::Clip, Some(route), Obligation::Outcome(o));
            report.case(&format!("{subject} {route:?}: switched off"), [k(Outcome::Bypass)], || holds(framed(route, &scene(Entry::Clip, &[frei0r(type_id)], false, W, H), 0), "bypassed", W, H));
            report.case(&format!("{subject} {route:?}: not installed"), [k(Outcome::PluginMissing)], || {
                let gone = format!("frei0r:/nowhere/{stem}.so:{stem}");
                holds(framed(route, &scene(Entry::Clip, &[frei0r(&gone)], true, W, H), 0), "plug-in missing", W, H)
            });
        }
        let all = |o| subjects.iter().map(move |(s, ..)| key(s, Entry::Clip, Some(route), Obligation::Outcome(o))).collect::<Vec<_>>();
        report.case(&format!("frei0r {route:?}: an instance the plug-in won't make"), all(Outcome::RenderFailed), || holds(framed(route, &scene(Entry::Clip, &[frei0r(refuses_id)], true, W, H), 0), "render failed", W, H));
        report.case(&format!("frei0r {route:?}: history above 2048"), all(Outcome::Unsupported), || holds(framed(route, &scene(Entry::Clip, &[frei0r(delay_id)], true, 2064, 16), 0), "unsupported", 2064, 16));
    }
}

/// A frei0r node's frame time: its local CPU segment (step 5) on a frame
/// the rest of which stays on the GPU, so no layer drawn on the CPU.
const BUDGET: crate::budget::Budget = crate::budget::Budget { ratio: 3.0, slack_ms: 2.0, cpu_layers: 0 };

/// What a save keeps, what step 5 deleted, and what each subject's node
/// costs on the viewer: its entries (two of the plug-in, the second
/// switched off) through a real save and a strict reload; the chain's GPU
/// veto gone; the frame time with the node against the same scene
/// without it.
fn kept_cases(report: &mut Report, subjects: &[(&str, &str, (u32, u32), &[i64])]) {
    for &(subject, type_id, (w, h), frames) in subjects {
        report.case(&format!("{subject}: saved and opened"), [key(subject, Entry::Clip, None, Obligation::Saved)], || {
            let mut s = (*scene(Entry::Clip, &[frei0r(type_id), frei0r(type_id)], true, W, H)).clone();
            let clip = s.sequence.tracks[0].clips.first_mut().ok_or("no clip")?;
            match clip.apply_effect_edits(&[EffectEdit::SetEnabled(1, false)]) {
                Some(Ok(_)) => {}
                other => return Err(format!("the switch didn't land: {:?}", other.map(|r| r.err()))),
            }
            let authored = s.sequence.tracks[0].clips[0].picture_effects()?;
            let back = crate::saved::round_trip(&s.sequence, &s.pool)?;
            let got = back.all_clips().next().ok_or("no clip came back")?.picture_effects()?;
            let kept = |fx: &[klipp_timeline::PictureEffect]| fx.iter().map(|e| (e.effect.clone(), e.enabled)).collect::<Vec<_>>();
            if kept(&got) != kept(&authored) {
                return Err(format!("came back {:?}, authored {:?}", kept(&got), kept(&authored)));
            }
            Ok(())
        });
        report.case(&format!("{subject}: the old stage is gone"), [key(subject, Entry::Clip, None, Obligation::OldStageGone)], || crate::old_stage::gone(Family::Frei0r));
        report.case(&format!("{subject} Viewer: within its frame budget"), [key(subject, Entry::Clip, Some(Route::Viewer), Obligation::FrameBudget)], || {
            let with = scene(Entry::Clip, &[frei0r(type_id)], true, w, h);
            let without = scene(Entry::Clip, &[], true, w, h);
            let source = pattern(&with);
            crate::budget::within(with, without, source, frames, BUDGET).map(|_| ())
        });
    }
}

pub fn run(report: &mut Report) {
    let plugins = match plugins() {
        Ok(p) => p,
        Err(e) => {
            report.failures.push(format!("frei0r fixtures: {e}"));
            return;
        }
    };
    let frames = [0, 25];
    let colour_rule = |px: &[u8], f: i64, _: u32, _: u32| colour(px, f as f64 / FPS as f64);
    let tiled_rule = |px: &[u8], _: i64, w: u32, h: u32| neighbour(px, w, h);
    for entry in [Entry::Clip, Entry::Title, Entry::Adjustment] {
        entry_cases(report, COLOUR, &plugins.colour, entry, (W, H), &frames, &|given, i| colour(&given[i], frames[i] as f64 / FPS as f64));
        let played = [10, 11, 12];
        entry_cases(report, STATEFUL, &plugins.history, entry, (W, H), &played, &|given, i| given[i.saturating_sub(1)].clone());
        // Wider than frei0r's 2048: the node tiles, and the seams don't show.
        entry_cases(report, TILED, &plugins.neighbour, entry, (2064, 16), &[0], &|given, i| neighbour(&given[i], 2064, 16));
    }
    instance_cases(report, COLOUR, &plugins.colour, Some(&colour_rule));
    instance_cases(report, STATEFUL, &plugins.history, None);
    instance_cases(report, TILED, &plugins.neighbour, Some(&tiled_rule));
    let subjects = [(COLOUR, plugins.colour.as_str(), "klipp-fixture-colour"), (STATEFUL, plugins.history.as_str(), "klipp-fixture-history"), (TILED, plugins.neighbour.as_str(), "klipp-fixture-neighbour")];
    outcome_cases(report, &subjects, &plugins.refuses, &plugins.delay);
    // Played in order: the history fixture builds on the frame before.
    let played: Vec<i64> = (10..30).collect();
    let kept = [(COLOUR, plugins.colour.as_str(), (W, H), &frames[..]), (STATEFUL, plugins.history.as_str(), (W, H), &played[..]), (TILED, plugins.neighbour.as_str(), (2064, 16), &[0][..])];
    kept_cases(report, &kept);
}

#[cfg(test)]
mod tests {
    /// The cases hand the node a picture made of sRGB8 codes decoded to
    /// linear, and apply the fixture's rule to those codes: every code
    /// must come back from linear as itself, or the expectation is off.
    #[test]
    fn every_srgb8_code_survives_linear_and_back() {
        let codes: Vec<u8> = (0..=255u8).flat_map(|c| [c, c, c, 255]).collect();
        let back = klipp_effects::linear_f32_rgba_to_srgb_u8(&klipp_effects::srgb_u8_to_linear_f32_rgba(&codes));
        assert_eq!(back, codes);
    }

    /// The rules match fixture.c on a known pixel.
    #[test]
    fn the_rules_are_the_fixtures() {
        assert_eq!(super::colour(&[10, 20, 30, 255], 1.0), [46, 245, 20, 255]);
        let flat = [7u8, 7, 7, 7].repeat(9);
        assert_eq!(super::neighbour(&flat, 3, 3), flat);
    }
}
