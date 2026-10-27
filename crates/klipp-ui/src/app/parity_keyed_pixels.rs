//! A key lane's picture: every keyframable numeric effect row (the
//! effect-key family's rows) earns `KeyedPixels` when its keys animate the
//! drawn frame, and a key edited through the inspector draws the frame the
//! lane keyed to that value directly draws, on each route, the CPU's as the
//! GPU's. The lanes render through the
//! effect node's animation, a path apart from the row's static value, so
//! the row's Pixels key doesn't vouch for them.
//!
//! On the numeric family's pixel fixture, with two keys (the fixture's
//! value at `T1`, a visibly different one at `T2`):
//! - the active control: the frame at `T2` differs from the same clip's
//!   with a flat lane (both keys the fixture's value) on each route, or
//!   the row fails, named; two times of one clip always differ, since the
//!   source moves;
//! - the edit: the key at `T2` typed to a third visible value at the
//!   playhead, which must change the frame at `T2` and draw the expected
//!   lane's frame at `T2` (a key time) and at `MID` (strictly between the
//!   keys) on each route, the CPU's within a level of the GPU's.
//!
//! Each interpolation the row's lane takes is checked: Step always, and
//! Linear where the parameter interpolates, so an interpolation mismatch
//! can't hide behind a key-time sample. Routes and known defects are the
//! numeric family's.

use super::parity_effect_keys::{eligible, variant};
use super::parity_harness::Run;
use super::parity_manifest::{catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use super::parity_numeric::{known_defect, number, routes_for, size_for, stored, type_value, RowLabel};
use super::parity_pixels::{frames_at, Routes};
use crate::inspector::generated::row;
use klipp_core::{Interpolation, Keyframe, Timecode};
use klipp_effects::registry::{effect_types, ParamKind};
use klipp_timeline::test_clip;

/// The first key's time, the second's, and a time strictly between them.
const T1: f64 = 0.25;
const T2: f64 = 1.25;
const MID: f64 = 0.75;

fn key(entry: &CatalogEntry) -> CoverageKey {
    CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, variant(entry), Interaction::KeyedPixels)
}

/// Why a row's keyed lane is excluded, if it is: a known defect of the row.
/// (Posterize's keyed lane, which drew apart at a level boundary, finds
/// its bands against one set of edges on both routes now, #1443.)
fn excluded(entry: &CatalogEntry) -> Option<&'static str> {
    known_defect(entry, Surface::EffectsStack, variant(entry))
}

/// The rows that owe `KeyedPixels`: the effect-key family's, less the
/// known defects, whose keys move nothing or draw apart on the routes.
pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog().into_iter().filter(eligible) {
        let k = key(&entry);
        manifest.require(k.clone());
        if let Some(reason) = excluded(&entry) {
            manifest.exclude_key(k, reason);
        }
    }
}

/// Whether two clips draw different frames at `secs` on any of `routes`.
fn differ(routes: Routes, size: (u32, u32), a: &klipp_timeline::Clip, b: &klipp_timeline::Clip, secs: f64) -> Result<bool, String> {
    Ok(frames_at(routes, size, a, secs)? != frames_at(routes, size, b, secs)?)
}

/// One row's lane under `interpolation`.
/// The values a row's case keys and types, chosen once per row: a key's
/// frame depends on its value, not on the lane's interpolation, so Step's
/// choice serves Linear too.
type Picks = Option<(f32, String)>;

fn case(entry: &CatalogEntry, interpolation: Interpolation, picks: &mut Picks) -> Result<(), String> {
    let CatalogValue::Parameter { desc, presentation: pres } = &entry.value else { unreachable!() };
    let ty = effect_types().find(|t| t.desc.id == entry.type_id).unwrap();
    let (routes, _) = routes_for(entry.type_id);
    let size = size_for(ty.tag);
    let effect = super::parity_numeric::editable(ty, pres)?;
    let (mut clip, index) = super::parity_numeric::pixel_fixture_for_row(ty, &effect, Surface::EffectsStack, desc.key)?;
    let label = RowLabel { text: pres.label.unwrap_or(desc.label), group: pres.group };
    let encoding = klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8());
    let (lo, hi) = row::shown_range(desc, pres, encoding);
    let value_at = |part: f32| {
        let text = format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * part);
        let shown: f32 = text.parse().unwrap();
        let stored = pres.display.map_or(shown, |map| (map.to_stored)(shown, encoding));
        (text, number(desc, stored).as_f64().unwrap_or(f64::from(stored)) as f32)
    };
    let base = stored(&test_clip::effects(&clip)[index], desc).as_f64().ok_or("the row's value isn't a number")? as f32;
    let id = test_clip::effect_id(&clip, index);
    let keyed = |clip: &mut klipp_timeline::Clip, b: f32| {
        clip.edit_effect_keyframes_of(id, desc.key, |keys| {
            *keys = [(T1, base), (T2, b)]
                .into_iter()
                .map(|(time_secs, value)| Keyframe { time_secs, value, interpolation, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() })
                .collect();
        });
    };
    // The second key: the first value near the middle whose key makes the
    // frame at T2 differ from the same clip with a flat lane (both keys
    // the fixture's value; two times of one clip always differ, since the
    // source moves, so only this isolates the lane): the active control.
    // The edit: the next whose key changes the frame at T2 again. A value
    // whose frame can't be drawn is passed over, never counted. Each
    // frame here is drawn once.
    if picks.is_none() {
        let candidates: Vec<(String, f32)> = [0.55, 0.45, 0.65, 0.35, 0.8, 0.2, 0.95, 0.05].into_iter().map(value_at).filter(|(_, v)| *v != base).collect();
        let at_key = |v: f32| {
            let mut c = clip.clone();
            keyed(&mut c, v);
            frames_at(routes, size, &c, T2)
        };
        let flat = at_key(base)?;
        let mut drawn = Vec::new();
        let mut b = None;
        for (text, v) in &candidates {
            let Ok(frame) = at_key(*v) else { continue };
            if b.is_none() && frame != flat {
                b = Some((*v, frame.clone()));
            }
            drawn.push((text.clone(), *v, frame));
        }
        let (b, b_frame) = b.ok_or("the lane doesn't move the frame at its key at any value tried")?;
        let (typed, _, _) = drawn.into_iter().find(|(_, v, frame)| *v != b && *frame != b_frame).ok_or("no second value changes the frame at the key")?;
        *picks = Some((b, typed));
    }
    let (b, typed) = picks.clone().expect("chosen above");
    keyed(&mut clip, b);
    let start = clip.clone();
    // The view types the third value at the second key; the expected clip
    // has that key set on its lane directly.
    let t0 = std::time::Instant::now();
    let mut run = Run::new(clip.clone());
    run.state.playhead = Timecode::from_secs_f64(T2, clip.timeline_in.rate);
    type_value(&mut run, label, &typed)?;
    super::parity_pixels::timing::add(&super::parity_pixels::timing::DRIVING, t0);
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", run.unapplied));
    }
    let new = run.clip().clone();
    let shown: f32 = typed.parse().map_err(|_| format!("{typed:?} isn't a number"))?;
    let want = number(desc, pres.display.map_or(shown, |map| (map.to_stored)(shown, encoding))).as_f64().unwrap_or(f64::from(shown)) as f32;
    let mut expected = start.clone();
    keyed(&mut expected, want);
    if !differ(routes, size, &start, &new, T2)? {
        return Err("the key edit doesn't change the frame at its key".into());
    }
    for (what, secs) in [("the key time", T2), ("between the keys", MID)] {
        if differ(routes, size, &expected, &new, secs)? {
            return Err(format!("the frame differs from the lane keyed to {want} at {what} ({secs} s)"));
        }
        super::parity_pixels::routes_agree_at(routes, size, &new, secs).map_err(|e| format!("at {what} ({secs} s): {e}"))?;
    }
    Ok(())
}

/// The interpolations a row's lane is checked under: Step, and Linear
/// where the parameter interpolates.
fn interpolations(entry: &CatalogEntry) -> Vec<Interpolation> {
    match entry.value {
        CatalogValue::Parameter { desc, .. } if matches!(desc.kind, ParamKind::Float | ParamKind::Colour) => vec![Interpolation::Step, Interpolation::Linear],
        _ => vec![Interpolation::Step],
    }
}

pub(super) fn run(report: &mut Report) {
    for entry in catalog().into_iter().filter(eligible) {
        let name = format!("{} / {} / keyed pixels", entry.type_id, entry.parameter);
        let check = || {
            let mut picks = None;
            for interpolation in interpolations(&entry) {
                case(&entry, interpolation, &mut picks).map_err(|e| format!("{interpolation:?}: {e}"))?;
            }
            Ok(())
        };
        match excluded(&entry) {
            // A known CPU/GPU defect: its key excluded with the issue, the
            // lane still animates the frame.
            Some(why) if why.starts_with("known CPU/GPU") => {
                report.case(&format!("{name}, routes apart (pinned)"), [], || super::parity_pixels::with_routes_apart(check));
                continue;
            }
            Some(_) => continue,
            None => {}
        }
        match routes_for(entry.type_id) {
            (Routes::CpuByDesign, Some(reason)) => report.case_cpu_by_design(&name, [key(&entry)], reason, check),
            _ => report.case_or_blocked(&name, [key(&entry)], check),
        }
    }
}

#[test]
fn every_keyed_effect_row_animates_the_same_picture_through_both_views() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    for (key, why) in report.blocked.iter().chain(&report.cpu_by_design) {
        eprintln!("{key:?}: {why}");
    }
    eprintln!("keyed pixels timing: {}", super::parity_pixels::timing::report());
    report.family_result().unwrap();
    assert!(!report.passed.is_empty());
    assert!(report.passed.iter().all(|key| manifest.required.contains(key)));
    assert!(report.blocked.is_empty(), "a keyed row left the native route: {:?}", report.blocked);
}
