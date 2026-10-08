//! M3 step 18 parity: the analysis effects' workflow (d9's family).
//! Deflicker and Video Stabilization measure the clip before they do
//! anything, and both views draw the same status widget for it
//! (`effects_tab::deflicker_status`, `stabilization_status`). Each case
//! drives that widget, or the row its measurement is smoothed by, through
//! the old Effects tab and the generated stack alike, and requires the
//! same picture, the same undo depth and the same analysis commands.
//!
//! A measurement lands from a worker while the user may be dragging: it
//! is applied as the app's drain applies it (the open drag closed first,
//! then `proxies::apply_analysis_result`), so the case drives the code
//! the app runs.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_core::{AppEvent, ClipId};
use klipp_effects::registry::{effect_types, EffectType};

/// The fixture: the type's sample on a two-second clip.
const VARIANT: &str = "sample";

/// An analysis effect: its type, the workflow it shows, the buttons its
/// status draws, the row its measurement is smoothed by, and a
/// measurement of the fixture's clip.
struct Analysis {
    tag: &'static str,
    workflow: &'static str,
    analyse: &'static str,
    reanalyse: &'static str,
    row_key: &'static str,
    measured: fn(&mut klipp_timeline::Clip),
    in_progress: fn(&mut crate::state::AppState, ClipId),
    complete: fn(ClipId) -> AppEvent,
    /// The undo steps a drag held past a result leaves: old view, new.
    result_drag_steps: (usize, usize),
}

const ANALYSES: &[Analysis] = &[
    Analysis {
        tag: "deflicker",
        workflow: "deflicker",
        analyse: "Analyse",
        reanalyse: "Re-analyse",
        row_key: "window_secs",
        measured: |clip| {
            let data = klipp_effects::deflicker::DeflickerData { luma: (0..48).map(|i| 0.5 + 0.02 * ((i % 3) as f32)).collect(), fps: 24.0 };
            clip.deflicker_gains = Some(klipp_effects::deflicker::DeflickerGains::refresh(None, &data, 0.5));
            clip.deflicker_data = Some(data);
        },
        in_progress: |state, clip| {
            state.deflicker_in_progress.insert(clip);
        },
        complete: |clip_id| AppEvent::DeflickerComplete { clip_id },
        result_drag_steps: (1, 2),
    },
    Analysis {
        tag: "video_stabilization",
        workflow: "video_stabilization",
        analyse: "Analyze Clip",
        reanalyse: "Re-analyze",
        row_key: "smoothing",
        measured: |clip| {
            use klipp_effects::rolling_shutter::Homography;
            use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, smooth_trajectory, StabilizationData};
            let step = |i: usize| Homography { h: [[1.0, 0.0, if i % 2 == 0 { 2.0 } else { -1.5 }], [0.0, 1.0, 0.5], [0.0, 0.0, 1.0]] };
            let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
            let accumulated = accumulate_trajectory(&frame_transforms);
            let trajectory = accumulated.iter().map(decompose_homography).collect();
            let data = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
            clip.smoothed_trajectory = Some(smooth_trajectory(&data, 1.0));
            clip.stabilization_data = Some(data);
        },
        in_progress: |state, clip| {
            state.stabilization_in_progress.insert(clip);
        },
        complete: |clip_id| AppEvent::StabilizationComplete { clip_id },
        result_drag_steps: (0, 1),
    },
];

fn effect_type(tag: &str) -> &'static EffectType {
    effect_types().find(|t| t.tag == tag).unwrap()
}

/// Every analysis effect's workflow entry, required on the effects stack.
pub(super) fn require(manifest: &mut Manifest) {
    for entry in catalog() {
        if let CatalogValue::Workflow(id) = entry.value {
            if ANALYSES.iter().any(|a| a.workflow == id) {
                manifest.require_fixture(&entry, Surface::EffectsStack, VARIANT);
            }
        }
    }
}

/// The old Effects tab and the generated stack over one clone of `clip`,
/// `prepare` run on each state.
fn both(clip: &klipp_timeline::Clip, prepare: impl Fn(&mut Run)) -> (Run, Run) {
    let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip.clone(), true));
    prepare(&mut old);
    prepare(&mut new);
    (old, new)
}

/// The two views agree: the same whole picture, the same analysis data on
/// the clip, the same undo depth (`undo` in each), the same side commands
/// (`side`, in order), and nothing either sent left unapplied.
fn agree(old: &Run, new: &Run, undo: usize, side: &[String]) -> Result<(), String> {
    if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
        return Err(format!("unapplied: old {:?}, new {:?}", old.unapplied, new.unapplied));
    }
    if old.clip().picture_v2()? != new.clip().picture_v2()? {
        return Err(format!("the pictures differ: old {:?}, new {:?}", old.clip().builtin_effects(), new.clip().builtin_effects()));
    }
    let analysis = |r: &Run| format!("{:?}{:?}{:?}{:?}", r.clip().deflicker_data, r.clip().deflicker_gains, r.clip().stabilization_data, r.clip().smoothed_trajectory);
    if analysis(old) != analysis(new) {
        return Err("the clips' measurements differ".into());
    }
    let (uo, un) = (old.state.active_history().undo_count(), new.state.active_history().undo_count());
    if (uo, un) != (undo, undo) {
        return Err(format!("undo depth old {uo}, new {un}, want {undo}"));
    }
    if old.side != side || new.side != side {
        return Err(format!("side commands old {:?}, new {:?}, want {side:?}", old.side, new.side));
    }
    Ok(())
}

/// The workflow cases for one analysis effect.
fn run_analysis(report: &mut Report, a: &Analysis) {
    let ty = effect_type(a.tag);
    let id = ty.desc.id;
    let parameter = format!("@workflow/{}", a.workflow);
    let key = |interaction| CoverageKey::new(id, &parameter, Surface::EffectsStack, VARIANT, interaction);
    let clip = fixture(&(ty.sample)());
    let clip_id = clip.id;
    let mut measured = clip.clone();
    (a.measured)(&mut measured);
    let desc = ty.desc.params.iter().find(|p| p.key == a.row_key).unwrap();
    let row = ty.presentation(desc).label.unwrap_or(desc.label);

    report.case(&format!("{id} opened"), [key(Interaction::OpenWithoutWrite)], || {
        let (mut old, mut new) = both(&clip, |_| {});
        let before = old.clip().picture_v2()?;
        old.frame(Vec::new(), egui::Modifiers::NONE);
        new.frame(Vec::new(), egui::Modifiers::NONE);
        agree(&old, &new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("opening it wrote the picture".into());
        }
        Ok(())
    });

    // Measured or not, "Analyse" asks for a measurement and writes nothing.
    for (from, button) in [(&clip, a.analyse), (&measured, a.reanalyse)] {
        report.case(&format!("{id} {button}"), [key(Interaction::StartAnalysis)], || {
            let (mut old, mut new) = both(from, |_| {});
            if !(old.click_text("", button) && new.click_text("", button)) {
                return Err(format!("no {button:?} in a view"));
            }
            let want = format!("{:?}", if a.tag == "deflicker" { klipp_core::AppCommand::AnalyzeDeflicker { clip_id } } else { klipp_core::AppCommand::AnalyzeStabilization { clip_id } });
            agree(&old, &new, 0, &[want])
        });
    }

    report.case(&format!("{id} cancelled"), [key(Interaction::CancelAnalysis)], || {
        let (mut old, mut new) = both(&clip, |r| (a.in_progress)(&mut r.state, clip_id));
        if !(old.click_text("", "Cancel") && new.click_text("", "Cancel")) {
            return Err("no Cancel in a view while it measures".into());
        }
        let want = format!("{:?}", if a.tag == "deflicker" { klipp_core::AppCommand::CancelDeflicker { clip_id } } else { klipp_core::AppCommand::CancelStabilization { clip_id } });
        agree(&old, &new, 0, &[want])
    });

    // On a measured clip, a drag of that row asks for the smoothing to be
    // redone as it goes (`RegainDeflicker`, `ResmoothStabilization`): those
    // write no document, so they join the drag's one step (#1328), unlike
    // a measurement landing mid-drag below (#1338).
    report.case(&format!("{id} drag over a measurement"), [key(Interaction::UndoRedo)], || {
        let (mut old, mut new) = both(&measured, |_| {});
        for run in [&mut old, &mut new] {
            let (_, at) = run.row(row).ok_or_else(|| format!("no row {row:?}"))?;
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            run.button(at + egui::vec2(18.0, 0.0), egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
        }
        if new.side.is_empty() {
            return Err("the drag asked for no smoothing".into());
        }
        let side = new.side.clone();
        agree(&old, &new, 1, &side)
    });

    // A measurement arriving mid-drag on the row it is smoothed by: the
    // drag is its own step, closed before the result lands; the result is
    // kept; and an undo takes back the drag alone.
    report.case(&format!("{id} result during a drag"), [key(Interaction::ResultDuringDrag), key(Interaction::UndoRedo)], || {
        let (mut old, mut new) = both(&clip, |r| (a.in_progress)(&mut r.state, clip_id));
        let before = new.clip().picture_v2()?;
        for run in [&mut old, &mut new] {
            let (_, at) = run.row(row).ok_or_else(|| format!("no row {row:?}"))?;
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            // The worker's result, as the drain applies it.
            let event = (a.complete)(clip_id);
            if super::event_writes::event_writes(&event) {
                run.state.close_path_drag_before_edit();
            }
            let mut flat = run.state.sequences[0].clone();
            let target = flat.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
            (a.measured)(target);
            if !super::proxies::apply_analysis_result(&mut run.state, Some(&flat), &event) {
                return Err("the result wasn't applied".into());
            }
            for dx in [18.0, 24.0, 30.0, 36.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            run.button(at + egui::vec2(36.0, 0.0), egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
        }
        if new.clip().picture_v2()? == before {
            return Err("the drag changed nothing".into());
        }
        if new.clip().deflicker_data.is_none() && new.clip().stabilization_data.is_none() {
            return Err("the result wasn't kept".into());
        }
        if new.state.deflicker_in_progress.contains(&clip_id) || new.state.stabilization_in_progress.contains(&clip_id) {
            return Err("still measuring after the result".into());
        }
        // The steps the gesture leaves, which differ by design (#1338):
        // the generated view closes its path drag as its own step before
        // the result and goes on as one more; the old index drag is never
        // closed by a result. A stabilization result draws its trajectory
        // above the rows, which moves egui's ids under the held slider and
        // ends its drag in both views: the generated view's half is one
        // step; the old view's drag is left open, no step at all.
        let (want_old, want_new) = a.result_drag_steps;
        let (uo, un) = (old.state.active_history().undo_count(), new.state.active_history().undo_count());
        if (uo, un) != (want_old, want_new) {
            return Err(format!("undo depth old {uo}, new {un}, pinned {want_old}, {want_new}"));
        }
        let depth = un;
        if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
            return Err(format!("unapplied: old {:?}, new {:?}", old.unapplied, new.unapplied));
        }
        if old.clip().picture_v2()? != new.clip().picture_v2()? {
            return Err(format!("the pictures differ: old {:?}, new {:?}", old.clip().builtin_effects(), new.clip().builtin_effects()));
        }
        if old.side != new.side {
            return Err(format!("side commands old {:?}, new {:?}", old.side, new.side));
        }
        // Undo all the way back: the picture before the drag, and the
        // measurement still on the clip; redo: the picture after.
        let after = new.clip().picture_v2()?;
        for _ in 0..depth {
            if !new.step(false) {
                return Err("an undo step is missing".into());
            }
        }
        if new.clip().picture_v2()? != before {
            return Err("undo doesn't give back the picture before the drag".into());
        }
        if new.clip().deflicker_data.is_none() && new.clip().stabilization_data.is_none() {
            return Err("undoing the drag took the measurement".into());
        }
        for _ in 0..depth {
            if !new.step(true) {
                return Err("a redo step is missing".into());
            }
        }
        if new.clip().picture_v2()? != after {
            return Err("redo doesn't give back the picture after the drag".into());
        }
        Ok(())
    });
}

pub(super) fn run(report: &mut Report) {
    for a in ANALYSES {
        report.family(a.tag, |report| run_analysis(report, a));
    }
}

#[test]
fn analysis_workflows_are_the_same_through_both_views() {
    let mut report = Report::default();
    report.family("analysis workflows", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let wanted: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels)).collect();
    let missing: Vec<_> = wanted.iter().filter(|k| !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}
