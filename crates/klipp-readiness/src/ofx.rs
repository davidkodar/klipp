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
//! An adjacent run of OFX nodes converts once, at its ends. A disabled
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
    AppliedEffect { plugin_name: plugin.into(), enabled, param_values: Default::default(), context, transition_input: None, transition_amount: 0.0 }
}

/// A scene of one media clip with `entries`, its host searching only the
/// fixtures.
fn scene(entries: Vec<AppliedEffect>) -> Result<Arc<Scene>, String> {
    let dir = crate::fixtures::ofx();
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/still.mov".into(), None);
    item.duration = RationalTime { value: 250, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    let media = item.id;
    pool.add(item);
    let tc = |f: i64| RationalTime { value: f, rate: FPS };
    let mut clip = Clip::media(media, tc(0), tc(50), tc(0), tc(50));
    clip.set_ofx_effects(entries);
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v1.clips = vec![clip];
    sequence.tracks = vec![v1];
    Ok(Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: vec![dir] }))
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

fn identity(p: [u8; 4]) -> [u8; 4] {
    p
}

fn channels(p: [u8; 4]) -> [u8; 4] {
    [p[2], 255 - p[0], p[1], p[3]]
}

fn alpha(p: [u8; 4]) -> [u8; 4] {
    [p[0], p[1], p[2], p[3] / 2]
}

/// Frame 10 of the scene on `route` is `want`.
fn draws(route: Route, gpu: bool, scene: Arc<Scene>, hdr: bool, want: &[f32]) -> Result<(), String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still { hdr }));
    let got = s.frame(10);
    // The viewer's GPU stack draws display bytes only: within a byte.
    let tolerance = if got.display_only() { 1.0 / 255.0 } else { 1e-6 };
    let off = got.off(want);
    if off > tolerance {
        return Err(format!("{off} from the expected picture"));
    }
    Ok(())
}

pub fn run(report: &mut Report) {
    let filter = OfxCategory::Filter;
    for route in [Route::Viewer, Route::Export] {
        for (subject, rule) in [(IDENTITY, identity as fn([u8; 4]) -> [u8; 4]), (CHANNELS, channels), (ALPHA, alpha)] {
            report.case(&format!("ofx {route:?}: {subject}"), [key(subject, Some(route), Obligation::ExpectedPixels)], || {
                draws(route, true, scene(vec![entry(subject, true, filter)])?, false, &through(false, &[rule]))
            });
            report.case(&format!("ofx {route:?}: {subject}, the CPU and the GPU agree"), [key(subject, Some(route), Obligation::CpuGpuAgree)], || {
                let want = through(false, &[rule]);
                draws(route, false, scene(vec![entry(subject, true, filter)])?, false, &want)?;
                draws(route, true, scene(vec![entry(subject, true, filter)])?, false, &want)
            });
            // Switched off, the input as it was: above 1 too, no conversion.
            report.case(&format!("ofx {route:?}: {subject} switched off"), [key(subject, Some(route), Obligation::DisabledIsIdentity)], || {
                draws(route, true, scene(vec![entry(subject, false, filter)])?, true, &picture(true))
            });
        }
        // Two in series convert once, at the run's ends.
        report.case(&format!("ofx {route:?}: channels then alpha, one run"), [key(CHANNELS, Some(route), Obligation::ExpectedPixels)], || {
            draws(route, true, scene(vec![entry(CHANNELS, true, filter), entry(ALPHA, true, filter)])?, false, &through(false, &[channels, alpha]))
        });
        report.case(&format!("ofx {route:?}: a plug-in that isn't there"), [key(CHANNELS, Some(route), Obligation::Outcome(Outcome::PluginMissing))], || {
            draws(route, true, scene(vec![entry("org.klipp.fixture.absent", true, filter)])?, false, &picture(false))
        });
        report.case(&format!("ofx {route:?}: a context the plug-in hasn't"), [key(CHANNELS, Some(route), Obligation::Outcome(Outcome::Unsupported))], || {
            draws(route, true, scene(vec![entry(CHANNELS, true, OfxCategory::Transition)])?, false, &picture(false))
        });
    }
}
