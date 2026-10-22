//! A grade row's keyframe controls (M3, flip readiness ruling 10-09): its
//! diamond, its carets, a value typed at a key and its interpolation menu,
//! driven on the generated grade rows and held to the key edit made on the
//! node's track directly. The controls are the generated rows' one code
//! path whatever the type, so they're earned on the central rules'
//! representative rows (`rules::GRADE_KEY_REPRESENTATIVE`): a per-node
//! type's on the per-node layout, the Corrector's on the tabbed page and
//! the bottom tools, on the clip's grade and, run through `on_roots`, each
//! of the sequence's grades. That every keyframable grade row takes a key
//! at all is the grade numeric family's per-type case ([`add_key_case`]).

use super::parity_controls::{placed, Grade, GradePicture};
use super::parity_grade_numeric::{nth, row_at, shown_on, type_into};
use super::parity_manifest::{CoverageKey, Interaction, KeyInterpolation, Report, Surface};
use crate::inspector::generated::row;
use klipp_core::{Interpolation, Keyframe};
use klipp_effects::registry::ParamDesc;
use klipp_nodes::registry::NodeType;
use klipp_nodes::NodeKind;
use klipp_timeline::GradeAccess;

/// The time the grade pages draw their rows' keys at (the harness's frame
/// selection).
const AT: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
enum Action {
    Add,
    Remove,
    Edit,
    Previous,
    Next,
    Interpolate(Interpolation),
}

const ACTIONS: [Action; 8] = [
    Action::Add,
    Action::Remove,
    Action::Edit,
    Action::Previous,
    Action::Next,
    Action::Interpolate(Interpolation::Linear),
    Action::Interpolate(Interpolation::Step),
    Action::Interpolate(Interpolation::Bezier),
];

impl Action {
    fn interaction(self) -> Interaction {
        match self {
            Self::Add => Interaction::AddKey,
            Self::Remove => Interaction::RemoveKey,
            Self::Edit => Interaction::EditKey,
            Self::Previous => Interaction::PreviousKey,
            Self::Next => Interaction::NextKey,
            Self::Interpolate(i) => Interaction::Interpolation(match i {
                Interpolation::Linear => KeyInterpolation::Linear,
                Interpolation::Step => KeyInterpolation::Step,
                // The parity runs pick no eased curve; it is shaped as a Bezier is.
                Interpolation::Bezier | Interpolation::Eased(_) => KeyInterpolation::Bezier,
            }),
        }
    }
}

/// A row's parameter, as its row draws it.
struct Row {
    desc: &'static ParamDesc,
    label: &'static str,
    nth: usize,
    /// A stored value at `share` of the row's shown range, and its text.
    lo: f32,
    hi: f32,
    decimals: usize,
    to_stored: Option<klipp_effects::registry::presentation::DisplayMap>,
}

impl Row {
    fn new(ty: &NodeType, desc: &'static ParamDesc) -> Self {
        let pres = ty.presentation(desc);
        let encoding = klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8());
        let (lo, hi) = row::shown_range(desc, &pres, encoding);
        Row { desc, label: pres.label.unwrap_or(desc.label), nth: nth(desc), lo, hi, decimals: usize::from(pres.decimals), to_stored: pres.display }
    }

    fn value(&self, share: f32) -> (String, f32) {
        let encoding = klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8());
        let text = format!("{:.*}", self.decimals, self.lo + (self.hi - self.lo) * share);
        let shown: f32 = text.parse().unwrap();
        (text, self.to_stored.map_or(shown, |m| (m.to_stored)(shown, encoding)))
    }
}

/// `start` with its node's track for `key` edited by `edit`, written
/// straight into the document (through a sequence grade's stand-in), and a
/// fresh history.
fn with_track(start: &Grade, key: &str, edit: impl FnOnce(&mut Vec<Keyframe>)) -> Result<Grade, String> {
    let mut g = start.twin();
    let node = g.node;
    g.with_stand_in(|g| -> Result<(), String> {
        let clip_id = g.clip;
        let clip = g.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).ok_or("no clip for the grade")?;
        clip.edit_grade(|graph| -> Result<(), String> {
            let node = graph.nodes.iter_mut().find(|n| n.id == node).ok_or("no node")?;
            edit(&mut node.keyframes.track_mut(key).keyframes);
            Ok(())
        })
    })?;
    Ok(g.twin())
}

/// The node's keys for `key`, as stored.
fn keys_of(g: &Grade, key: &str) -> Vec<Keyframe> {
    g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == key)).map(|t| t.keyframes.clone()).unwrap_or_default()
}

/// The row's diamond: the drawn diamond on the row's line, right of its
/// label.
fn diamond(g: &mut Grade, r: &Row) -> Result<egui::Pos2, String> {
    let (label, _) = row_at(g, r.label, r.nth)?;
    g.diamonds.iter().filter(|d| (d.center().y - label.y).abs() < 6.0 && d.center().x > label.x).map(|d| d.center()).min_by(|a, b| a.x.total_cmp(&b.x)).ok_or_else(|| format!("no diamond on the {:?} row", r.label))
}

/// The row's caret toward the previous or the next key: the small icon
/// beside its diamond, on that side.
fn caret(g: &mut Grade, r: &Row, next: bool) -> Result<egui::Pos2, String> {
    let at = diamond(g, r)?;
    g.icons.iter().filter(|i| (i.center().y - at.y).abs() < 6.0 && (i.center().x > at.x) == next && (i.center().x - at.x).abs() < 40.0 && i.width() < 12.0).map(|i| i.center()).min_by(|a, b| (a.x - at.x).abs().total_cmp(&(b.x - at.x).abs())).ok_or_else(|| format!("no {} caret on the {:?} row", if next { "next" } else { "previous" }, r.label))
}

/// The interpolation menu's item for `want`, picked from the menu the
/// diamond's right click opens.
fn interpolate(g: &mut Grade, r: &Row, want: Interpolation) -> Result<(), String> {
    let at = diamond(g, r)?;
    g.right_click_at(at);
    let name = match want {
        Interpolation::Linear => "Linear",
        Interpolation::Step => "Step (Hold)",
        Interpolation::Bezier => "Bezier (Custom)",
        Interpolation::Eased(_) => return Err("an eased curve is picked from its family's submenu".into()),
    };
    // The menu is drawn last.
    let item = g.frame(Vec::new()).into_iter().rev().find(|(t, _)| t.trim().trim_start_matches('*').trim() == name).map(|(_, p)| p).ok_or_else(|| format!("no interpolation menu item {name}"))?;
    g.click_at(item);
    Ok(())
}

/// One key control on `start`'s row, held to the key edit made on the
/// node's track: the picture that edit leaves, one undo step, and undo and
/// redo exact; a caret moves the playhead to the key, writing nothing.
fn key_case(start: &Grade, r: &Row, action: Action) -> Result<(), String> {
    let (_, a) = r.value(0.2);
    let (_, b) = r.value(0.8);
    let (typed, target) = r.value(0.6);
    // A key at the frame's time for the edits of a key, around it for an
    // add and the carets.
    let first = if matches!(action, Action::Add | Action::Previous | Action::Next) { 0.25 } else { AT };
    let initial = if matches!(action, Action::Interpolate(Interpolation::Linear)) { Interpolation::Step } else { Interpolation::Linear };
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: initial, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
    let start = with_track(start, r.desc.key, |keys| *keys = vec![key(first, a), key(1.25, b)])?;
    let expected: GradePicture = with_track(&start, r.desc.key, |keys| match action {
        Action::Add => {
            let at = klipp_core::keyframe::evaluate(keys, AT, a);
            klipp_core::keyframe::upsert_keyframe(keys, AT, at);
        }
        Action::Remove => klipp_core::keyframe::remove_keyframe_at(keys, AT),
        Action::Edit => klipp_core::keyframe::upsert_keyframe(keys, AT, target),
        Action::Interpolate(i) => klipp_core::keyframe::set_interpolation_at(keys, 0, i),
        Action::Previous | Action::Next => {}
    })?.picture();
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    match action {
        Action::Add | Action::Remove => {
            let at = diamond(&mut g, r)?;
            g.click_at(at);
        }
        Action::Edit => type_into(&mut g, r.label, r.nth, &typed)?,
        Action::Previous | Action::Next => {
            let at = caret(&mut g, r, matches!(action, Action::Next))?;
            g.click_at(at);
        }
        Action::Interpolate(i) => interpolate(&mut g, r, i)?,
    }
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let after = g.picture();
    if after != expected {
        return Err(format!("{action:?} left another grade than the key edit made on the track: keys {:?}", keys_of(&g, r.desc.key).iter().map(|k| (k.time_secs, k.value, k.interpolation)).collect::<Vec<_>>()));
    }
    if let Action::Previous | Action::Next = action {
        let want = if matches!(action, Action::Previous) { 0.25 } else { 1.25 };
        // The seek is in the timeline's time: the key's clip time from
        // where the clip starts.
        let lands = |s: f64| g.state.sequences[0].all_clips().any(|c| (s - c.timeline_in.to_secs_f64() - want).abs() < 0.02);
        if g.seeks.len() != 1 || !lands(g.seeks[0].to_secs_f64()) || g.state.active_history().undo_count() != 0 {
            return Err(format!("navigation: seeks {:?}, undo {}", g.seeks, g.state.active_history().undo_count()));
        }
        return Ok(());
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    g.step(true)?;
    if g.picture() != after {
        return Err("redo doesn't give back the keyed grade".into());
    }
    Ok(())
}

/// The per-type case of every keyframable grade row, on its own page (the
/// grade numeric family's `row_cases`): its diamond keys the node at the
/// frame's time, with the value it shows, as one undo step undone exactly.
pub(super) fn add_key_case(start: &Grade, ty: &NodeType, desc: &'static ParamDesc) -> Result<(), String> {
    let r = Row::new(ty, desc);
    let mut g = start.twin();
    g.drawn();
    let before = g.picture();
    let shown = keys_of(&g, desc.key);
    let value = g.value(desc).as_f64().map(|v| v as f32);
    let at = diamond(&mut g, &r)?;
    g.click_at(at);
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    let keys = keys_of(&g, desc.key);
    if keys.len() != shown.len() + 1 || !keys.iter().any(|k| (k.time_secs - AT).abs() < 0.02 && value.is_none_or(|v| (k.value - v).abs() < 1e-4)) {
        return Err(format!("the diamond didn't key {} at {AT} s with its value {value:?}: {:?}", desc.key, keys.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>()));
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    Ok(())
}

/// A representative's row drawn, with its page and variant's start.
pub(super) fn representative(page: Surface, type_id: &str, parameter: &str) -> Result<(Grade, &'static NodeType, &'static ParamDesc), String> {
    let ty = klipp_nodes::registry::node_types().find(|t| t.desc.id == type_id).ok_or("no such grade type")?;
    let desc = ty.desc.params.iter().find(|d| d.key == parameter).ok_or("no such parameter")?;
    let start = match page {
        Surface::GradePerNode => {
            let (state, clip, node) = placed(ty.kind).ok_or("the grade won't take one")?;
            Grade::new(state, clip, node)
        }
        Surface::GradeTabbedRight | Surface::GradeBottomTools => {
            let (state, clip, node) = super::grade_inspector_tests::graded(NodeKind::Corrector).filter(|_| ty.kind == NodeKind::Corrector).ok_or("the tabbed representative is the Corrector's")?;
            shown_on(&Grade::new(state, clip, node), ty, desc, page == Surface::GradeBottomTools)?.0
        }
        other => return Err(format!("{other:?} isn't a grade page")),
    };
    Ok((start, ty, desc))
}

pub(super) fn run(report: &mut Report) {
    for (page, type_id, parameter) in super::parity_manifest::grade_key_representatives() {
        let key = |action: Action| CoverageKey::new(type_id, parameter, page, super::parity_manifest::grade_key_variant(), action.interaction());
        for action in ACTIONS {
            report.case(&format!("{type_id} {parameter} on {page:?}: {action:?}"), [key(action)], || {
                let (start, ty, desc) = representative(page, type_id, parameter)?;
                key_case(&start, &Row::new(ty, desc), action)
            });
        }
    }
}

#[test]
fn grade_key_controls_store_the_key_edit() {
    let mut report = Report::default();
    report.family("grade keys", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no grade key control was driven");
}

#[test]
fn grade_key_controls_store_the_key_edit_on_the_sequences_grades() {
    let mut report = Report::default();
    super::parity_controls::on_roots(&mut report, run);
    report.family_result().unwrap();
    super::parity_controls::assert_on_every_root(&report);
}
