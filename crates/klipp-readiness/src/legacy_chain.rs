//! The legacy chain's cases (M4 plan v2): the effects that still read the
//! whole frame at once, held to their own rule. Its subject is equalize:
//! a histogram of the frame's luma (sRGB-encoded, BT.709's weights in a
//! Rec. 709 working space), its cumulative curve from the first level the
//! frame has, and each pixel's encoded colour scaled toward the curve's
//! value for its luma by the strength; light above white passes through.
//! The expectation is that rule computed here, on the picture the node is
//! given, which the same scene without it draws (opaque, so the frame's).

use std::sync::Arc;

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, Sequence};

use crate::route::{KnownFrame, Scene, Session, Source};
use crate::{Entry, Family, Key, Obligation, Outcome, Report, Route};

const FPS: u32 = 25;
const W: u32 = 16;
const H: u32 = 8;
const SUBJECT: &str = "klipp.fx.equalize";
/// A linear frame within this of its expectation: the GPU's histogram and
/// curve in f32 against the rule's here.
const LINEAR_TOLERANCE: f32 = 1e-4;

fn key(at: Entry, route: Option<Route>, obligation: Obligation) -> Key {
    Key { family: Family::LegacyChain, subject: SUBJECT.into(), entry: at, route, obligation }
}

fn tc(f: i64) -> RationalTime {
    RationalTime { value: f, rate: FPS }
}

/// The clip's picture: a ramp of grey across it, a little colour, and a
/// highlight above white in its last column, opaque.
fn picture() -> Vec<f32> {
    let n = (W * H) as usize;
    (0..n)
        .flat_map(|i| {
            let v = (i % W as usize) as f32 / (W - 1) as f32 * 0.6 + 0.05;
            let over = if i % W as usize == W as usize - 1 { 1.5 } else { 0.0 };
            [v + over, v * 0.9, v * 0.7 + 0.1, 1.0]
        })
        .collect()
}

struct Still;

impl Source for Still {
    fn frame(&self, _media: MediaId, _source_secs: f64) -> Option<KnownFrame> {
        Some(KnownFrame { linear: picture(), width: W, height: H, pts: None })
    }
}

fn encode(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Equalize's rule at `strength` on `linear` (straight RGBA).
fn equalized(linear: &[f32], strength: f32) -> Vec<f32> {
    let yw = [0.2126f32, 0.7152, 0.0722];
    let mut hist = [0u32; 256];
    for c in linear.chunks_exact(4) {
        let l = (yw[0] * encode(c[0]) + yw[1] * encode(c[1]) + yw[2] * encode(c[2])).clamp(0.0, 1.0);
        hist[(l * 255.0) as usize] += 1;
    }
    let total = (linear.len() / 4) as f32;
    let first = hist.iter().position(|&h| h > 0).unwrap_or(0);
    let min_cum: u32 = hist[..=first].iter().sum();
    let mut cdf = [0.0f32; 256];
    let mut cum = 0u32;
    for (i, h) in hist.iter().enumerate() {
        cum += h;
        cdf[i] = if total > min_cum as f32 { (cum.saturating_sub(min_cum) as f32 / (total - min_cum as f32)).clamp(0.0, 1.0) } else { i as f32 / 255.0 };
    }
    let mut out = linear.to_vec();
    for px in out.chunks_exact_mut(4) {
        let s = [encode(px[0]), encode(px[1]), encode(px[2])];
        let d = s.map(|v| v.min(1.0));
        let l = yw[0] * d[0] + yw[1] * d[1] + yw[2] * d[2];
        let eq = cdf[(l * 255.0).clamp(0.0, 255.0) as usize];
        let k = if l < 0.001 { 1.0 } else { eq / l };
        for i in 0..3 {
            px[i] = decode(d[i] + (d[i] * k - d[i]) * strength + (s[i] - 1.0).max(0.0));
        }
    }
    out
}

/// `clip` with equalize at `strength`, switched on or off, through its
/// picture.
fn equalizing(mut clip: Clip, effect: Option<(f32, bool)>) -> Result<Clip, String> {
    if let Some((strength, on)) = effect {
        let eq = BuiltinEffect::Equalize { strength };
        let mut edits = vec![klipp_timeline::EffectEdit::Add(&eq)];
        if !on {
            edits.push(klipp_timeline::EffectEdit::SetEnabled(0, false));
        }
        match clip.apply_effect_edits(&edits) {
            Some(Ok(_)) => {}
            Some(Err(why)) => return Err(why),
            None => return Err("equalize couldn't be added through the picture".into()),
        }
    }
    Ok(clip)
}

/// Equalize (`effect`: its strength and whether it's on, or none) where
/// `at` names: on a media clip; on a title over a full frame of its own
/// colour; or on an adjustment layer over a media clip.
fn placed(at: Entry, effect: Option<(f32, bool)>) -> Result<Arc<Scene>, String> {
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/known/still.mov".into(), None);
    item.duration = RationalTime { value: 250, rate: FPS };
    item.fps = klipp_core::Fps::new(FPS, 1);
    let media = item.id;
    pool.add(item);
    let shot = || Clip::media(media, tc(0), tc(50), tc(0), tc(50));
    let video = |clips: Vec<Clip>| {
        let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
        t.clips = clips;
        t
    };
    let tracks = match at {
        Entry::Clip => vec![video(vec![equalizing(shot(), effect)?])],
        Entry::Title => {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "EQ".into(), font_size: 6.0, ..Default::default() });
            td.background = klipp_timeline::TitleBackground::FullFrame { color: [40, 160, 90, 255] };
            let title = Clip { kind: klipp_timeline::ClipKind::Title(td), ..Clip::gap(tc(0), tc(50)) };
            let mut text = klipp_timeline::Track::new("T", klipp_timeline::TrackKind::Text);
            text.clips = vec![equalizing(title, effect)?];
            vec![text]
        }
        Entry::Adjustment => {
            let adjustment = Clip { kind: klipp_timeline::ClipKind::AdjustmentLayer, ..Clip::gap(tc(0), tc(50)) };
            vec![video(vec![equalizing(adjustment, effect)?]), video(vec![shot()])]
        }
        Entry::Conversion => return Err("no effect is placed in a conversion".into()),
    };
    let mut sequence = Sequence::new_default();
    sequence.fps = klipp_core::Fps::new(FPS, 1);
    sequence.tracks = tracks;
    Ok(Arc::new(Scene { sequence, pool, width: W, height: H, plugin_dirs: vec![] }))
}

/// Frame 10 of `scene` on `route`: its linear frame, or its display bytes
/// decoded, and whether it was display bytes only; declining nothing.
fn frame(route: Route, gpu: bool, scene: Arc<Scene>) -> Result<(Vec<f32>, bool), String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still));
    let f = s.frame(10);
    f.declined_as(None)?;
    let display_only = f.display_only();
    Ok((f.linear.clone().unwrap_or_else(|| klipp_effects::srgb_u8_to_linear_f32_rgba(&f.srgb8)), display_only))
}

/// Frame 10 of `scene` is `want`: within a byte of display bytes, within
/// [`LINEAR_TOLERANCE`] of a linear frame.
fn draws(route: Route, gpu: bool, scene: Arc<Scene>, want: &[f32]) -> Result<(), String> {
    let mut s = Session::new(route, gpu, scene, Arc::new(Still));
    let got = s.frame(10);
    got.declined_as(None)?;
    let tolerance = if got.display_only() { 1.0 / 255.0 } else { LINEAR_TOLERANCE };
    let off = got.off(want);
    if off > tolerance {
        return Err(format!("{off} from the expected picture"));
    }
    Ok(())
}

/// The picture the node is given where `at` names, on `route`: on a clip,
/// and on an adjustment layer over the clip, the clip's own picture as the
/// source gives it (light above white too, which the histogram counts and
/// display bytes would clip); on a title, the title as the same scene
/// without the node draws it (it holds nothing above white).
fn given(route: Route, gpu: bool, at: Entry) -> Result<Vec<f32>, String> {
    match at {
        Entry::Title => Ok(frame(route, gpu, placed(at, None)?)?.0),
        _ => Ok(picture()),
    }
}

pub fn run(report: &mut Report) {
    // Step 11's deleted loop stays gone.
    report.case("the legacy loop stays gone", [key(Entry::Clip, None, Obligation::OldStageGone)], || crate::old_stage::gone(Family::LegacyChain));
    // Saved and opened: its strength and switch kept.
    report.case("equalize saved and opened", [key(Entry::Clip, None, Obligation::Saved)], || {
        let s = placed(Entry::Clip, Some((0.7, false)))?;
        let back = crate::saved::round_trip(&s.sequence, &s.pool)?;
        let clip = back.all_clips().next().ok_or("no clip came back")?;
        let got = clip.picture_effects()?;
        match got.as_slice() {
            [e] if e.effect == BuiltinEffect::Equalize { strength: 0.7 } && !e.enabled => Ok(()),
            other => Err(format!("came back {other:?}")),
        }
    });
    for route in [Route::Viewer, Route::Export] {
        for at in [Entry::Clip, Entry::Title, Entry::Adjustment] {
            report.case(&format!("equalize {route:?} on {at:?}"), [key(at, Some(route), Obligation::ExpectedPixels)], || {
                let want = equalized(&given(route, true, at)?, 1.0);
                draws(route, true, placed(at, Some((1.0, true)))?, &want)
            });
            report.case(&format!("equalize {route:?} on {at:?}, the CPU and the GPU agree"), [key(at, Some(route), Obligation::CpuGpuAgree)], || {
                for gpu in [false, true] {
                    let want = equalized(&given(route, gpu, at)?, 1.0);
                    draws(route, gpu, placed(at, Some((1.0, true)))?, &want)?;
                }
                Ok(())
            });
            report.case(&format!("equalize {route:?} on {at:?} switched off"), [key(at, Some(route), Obligation::DisabledIsIdentity)], || {
                let want = given(route, true, at)?;
                let mut s = Session::new(route, true, placed(at, Some((1.0, false)))?, Arc::new(Still));
                let got = s.frame(10);
                // Passed over as asked, and said so: a bypass, never shown.
                got.declined_as(Some("bypassed"))?;
                let tolerance = if got.display_only() { 1.0 / 255.0 } else { LINEAR_TOLERANCE };
                let off = got.off(&want);
                if off > tolerance {
                    return Err(format!("{off} from the picture it was given"));
                }
                Ok(())
            });
        }
        // Its render failing: the frame as it was, declined by name, which
        // an export refuses and the viewer shows.
        report.case(&format!("equalize {route:?} failing its run"), [key(Entry::Clip, Some(route), Obligation::Outcome(Outcome::RenderFailed))], || {
            let _fault = klipp_nodes::fault::fail("equalize");
            let mut s = Session::new(route, true, placed(Entry::Clip, Some((1.0, true)))?, Arc::new(Still));
            let got = s.frame(10);
            got.declined_as(Some("render failed"))?;
            let tolerance = if got.display_only() { 1.0 / 255.0 } else { LINEAR_TOLERANCE };
            let off = got.off(&picture());
            if off > tolerance {
                return Err(format!("{off} from the picture it was given"));
            }
            Ok(())
        });
        // At strength 0 it passes its input on (light above white too).
        report.case(&format!("equalize {route:?} at strength 0"), [key(Entry::Clip, Some(route), Obligation::Outcome(Outcome::Bypass))], || {
            draws(route, true, placed(Entry::Clip, Some((0.0, true)))?, &picture())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule spreads a narrow ramp across the range, and does nothing
    /// at strength 0: a control that it changes the picture at all.
    #[test]
    fn the_rule_stretches_a_ramp_and_strength_0_is_nothing() {
        let p = picture();
        let eq = equalized(&p, 1.0);
        let spread = |v: &[f32]| v.chunks_exact(4).map(|c| c[1]).fold(f32::MIN, f32::max) - v.chunks_exact(4).map(|c| c[1]).fold(f32::MAX, f32::min);
        assert!(spread(&eq) > spread(&p) + 0.05, "equalized: {} against {}", spread(&eq), spread(&p));
        let off = equalized(&p, 0.0).iter().zip(&p).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(off < 1e-5, "strength 0 changed the picture by {off}");
    }

    #[test]
    #[ignore = "renders the family's scenes; run on request"]
    fn the_legacy_chain_cases_pass() {
        let mut report = crate::Report::default();
        run(&mut report);
        assert!(report.failures.is_empty(), "{}", report.failures.join("\n"));
        eprintln!("earned {}", report.passed.len());
    }
}

