//! M3 step 18 parity: the keyframe curve editor (d9's family), on every
//! keyframable effect parameter's track, as it lists them today (tracks
//! that have keys). Each case drives the editor, and the keyframe menu the
//! timeline draws for it, with the generated inspector on, and requires
//! the keys the edit names, one undo step, and an exact undo and redo.
//! Every place a case clicks is a share of the track's drawn range, so one
//! set of cases drives every parameter.
//!
//! A grade node's track is listed by its path through the grade group
//! (#891), one lane whatever its node: driven on the representatives
//! (`rules::GRADE_CURVE_REPRESENTATIVE`).

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, KeyInterpolation, Manifest, Report, Surface};
use crate::panels::curve_editor::{CurveEditorPanel, CurveParam};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::test_clip;

struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// The key times the cases start from, and the shares of the track's
/// drawn range their values sit at.
const KEYS: [(f64, f32); 2] = [(0.5, 0.2), (1.5, 0.8)];

/// One effect parameter's track: its effect's tag, its key, and the range
/// the curve editor draws it over (`curve_editor::node_param`'s).
#[derive(Clone, Copy)]
struct Track {
    tag: &'static str,
    key: &'static str,
    lo: f32,
    hi: f32,
    /// The clip's Video track this row is reached as in the curve editor,
    /// when it is one (the anchor Opacity's value is the clip's opacity).
    video: Option<klipp_core::VideoParam>,
    /// A grade node's kind, for a grade track (#891).
    grade: Option<klipp_nodes::NodeKind>,
}

impl Track {
    /// The value at `share` of the drawn range.
    fn at(&self, share: f32) -> f32 {
        self.lo + (self.hi - self.lo) * share
    }

    /// How far apart two values may be and still be one: two hundredths of
    /// the range, as a click lands within a pixel or two.
    fn near(&self, a: f32, b: f32) -> bool {
        (a - b).abs() <= (self.hi - self.lo).abs() * 0.04
    }
}

/// The track of a keyframable effect parameter's catalog entry, or of a
/// grade representative's (#891): `None` for another grade entry or an
/// entry with no parameter.
fn track(entry: &super::parity_manifest::CatalogEntry) -> Option<Track> {
    let CatalogValue::Parameter { desc, .. } = entry.value else { return None };
    if !desc.keyframable {
        return None;
    }
    if entry.grade {
        if !super::parity_manifest::grade_curve_representative(entry.type_id, &entry.parameter) {
            return None;
        }
        let ty = klipp_nodes::registry::node_types().find(|t| t.desc.id == entry.type_id)?;
        let (lo, hi) = desc.soft.or(desc.range).unwrap_or((0.0, 1.0));
        return Some(Track { tag: ty.desc.id, key: desc.key, lo, hi, video: None, grade: Some(ty.kind) });
    }
    let ty = klipp_effects::registry::effect_types().find(|t| t.desc.id == entry.type_id)?;
    // Opacity's value is the clip's own opacity, its Video track (0 to 1).
    if ty.tag == "opacity" && desc.key == "value" {
        return Some(Track { tag: ty.tag, key: desc.key, lo: 0.0, hi: 1.0, video: Some(klipp_core::VideoParam::Opacity), grade: None });
    }
    let (lo, hi) = desc.soft.or(desc.range).unwrap_or((0.0, 1.0));
    Some(Track { tag: ty.tag, key: desc.key, lo, hi, video: None, grade: None })
}

/// One clip, selected, its blur's radius keyed at `KEYS`, drawn in the
/// curve editor on that track.
struct Curves {
    track: Track,
    state: AppState,
    clip: ClipId,
    panel: CurveEditorPanel,
    ctx: egui::Context,
    clock: f64,
    unapplied: Vec<String>,
}

/// The clip the cases edit: the track's effect, its sample, with that
/// parameter keyed at `KEYS`, and no history. Built once per case, so both
/// views start from the same clip.
fn fixture(track: &Track) -> Result<klipp_timeline::Clip, String> {
    // A grade node keyed by its path through the grade group (#891).
    if let Some(kind) = track.grade {
        let (mut state, id, node) = super::grade_inspector_tests::graded(kind).ok_or("the grade won't take one")?;
        let addr = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().grade_nodes().into_iter().map(|(p, ..)| p).find(|p| p.node() == Some(node)).ok_or("no grade node by path")?;
        for (t, share) in KEYS {
            let keyframe = serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: track.at(share), interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
            state.path_command(&AppCommand::AddKeyAt { addr: addr.clone(), key: track.key.into(), keyframe }).ok_or("no key added")?;
        }
        let clip = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().clone();
        if clip.animated_grade_keys().is_empty() {
            return Err("the keyed grade parameter isn't listed as a track".into());
        }
        return Ok(clip);
    }
    let ty = klipp_effects::registry::effect_type(track.tag).ok_or("no such effect")?;
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.push_effect((ty.sample)());
    if let Some(param) = track.video {
        clip.edit_video_param_keyframes(param, |keys| {
            for (t, share) in KEYS {
                klipp_core::keyframe::upsert_keyframe(keys, t, track.at(share));
            }
        });
        return Ok(clip);
    }
    // A placed kind (a Temporal Denoise) is keyed through the clip's effect
    // list, as its card keys it; the path handler refuses its node (#1419).
    if crate::inspector::stack_view::is_placed_kind(track.tag) {
        let effect = test_clip::effect_id(&clip, 0);
        clip.edit_effect_keyframes_of(effect, track.key, |keys| {
            for (t, share) in KEYS {
                klipp_core::keyframe::upsert_keyframe(keys, t, track.at(share));
            }
        });
        if clip.animated_effect_keys().is_empty() {
            return Err("the keyed parameter isn't listed as a track".into());
        }
        return Ok(clip);
    }
    let id = clip.id;
    let mut state = AppState::default();
    state.sequences[0].tracks[0].clips.push(clip);
    let node = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().picture_v2()?.picture.graph.nodes.iter()
        .find(|n| klipp_graph_lift::effect_params::tag(n) == Some(track.tag)).map(|n| n.id).ok_or("no effect node")?;
    let addr = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: id.0 }, [node]);
    for (t, share) in KEYS {
        let keyframe = serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: track.at(share), interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
        state.path_command(&AppCommand::AddKeyAt { addr: addr.clone(), key: track.key.into(), keyframe }).ok_or("no key added")?;
    }
    let clip = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().clone();
    if clip.animated_effect_keys().is_empty() {
        return Err("the keyed parameter isn't listed as a track".into());
    }
    Ok(clip)
}

impl Curves {
    /// `clip` selected in a fresh state, drawn in the curve editor on its
    /// track.
    fn over(track: Track, clip: klipp_timeline::Clip) -> Result<Self, String> {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.selected_clip = Some(id);
        let mut panel = CurveEditorPanel::new();
        let param = match track.video {
            Some(p) => CurveParam::Video(p),
            None => {
                let clip = state.sequences[0].all_clips().find(|c| c.id == id).unwrap();
                let (path, key, tag) = clip.animated_effect_keys().into_iter().chain(clip.animated_grade_keys()).next().ok_or("no animated track")?;
                CurveParam::Node { path, key, tag }
            }
        };
        panel.show_track(param);
        let mut run = Curves { track, state, clip: id, panel, ctx: egui::Context::default(), clock: 0.0, unapplied: Vec::new() };
        run.frame(Vec::new());
        run.frame(Vec::new());
        Ok(run)
    }

    fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    /// A fresh history over the clip as it is: where a case starts when
    /// its fixture needed an edit first.
    fn restart(&mut self) {
        let clip = self.clip().clone();
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.selected_clip = Some(self.clip);
        self.state = state;
        self.frame(Vec::new());
    }

    fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    /// The track's keys, as (time, value, interpolation).
    fn keys(&self) -> Vec<(f64, f32, klipp_core::Interpolation)> {
        if let Some(p) = self.track.video {
            return self.clip().video_param_keyframes(p).iter().map(|k| (k.time_secs, k.value, k.interpolation)).collect();
        }
        let CurveParam::Node { path, key, .. } = self.panel_param() else { return Vec::new() };
        self.clip().node_key_state(&path, &key).map(|s| s.keys.iter().map(|k| (k.time_secs, k.value, k.interpolation)).collect()).unwrap_or_default()
    }

    fn panel_param(&self) -> CurveParam {
        if let Some(p) = self.track.video {
            return CurveParam::Video(p);
        }
        let (path, key, tag) = self.clip().animated_effect_keys().into_iter().chain(self.clip().animated_grade_keys()).next().expect("the track");
        CurveParam::Node { path, key, tag }
    }

    /// One frame with `events`: the curve editor and the keyframe menu, as
    /// the app draws them, and their commands applied as it applies them.
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let Curves { state, panel, ctx, .. } = self;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                cmds.extend(panel.show(ui, state, &theme));
                crate::panels::timeline::TimelinePanel::show_keyframe_context_menu(ui, state, &mut cmds);
            });
        });
        for cmd in cmds {
            self.apply(cmd);
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    fn apply(&mut self, cmd: AppCommand) {
        self.state.before_dispatch(&cmd);
        self.state.toast = Default::default();
        if self.state.path_command(&cmd).is_some() {
            if let Some(why) = self.state.toast.message_for_test() {
                self.unapplied.push(format!("{cmd:?} refused: {why}"));
            }
            return;
        }
        // The playhead and the view: nothing the clip records.
        if matches!(cmd, AppCommand::SeekTo(_)) {
            return;
        }
        // The keys' handlers, as the app runs them: an effect's by its
        // place in the list (a placed kind's track, #1419), a Video track's.
        let mut host = InspectorEditHost { state: &mut self.state, side: &Sink };
        if !host.apply_effect_keyframes(&cmd) && !host.apply_video_keyframes(&cmd) && !host.apply_keyframe_drag(&cmd) && !host.apply(&cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    /// Where time `t` and `share` of the track's range are drawn.
    fn at(&self, t: f64, share: f32) -> egui::Pos2 {
        self.panel.screen_at(t, self.track.at(share)).expect("the graph is drawn")
    }

    fn button(&mut self, at: egui::Pos2, button: egui::PointerButton, pressed: bool) {
        self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }]);
    }

    fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) {
        self.button(at, button, true);
        self.button(at, button, false);
        self.frame(vec![egui::Event::PointerGone]);
    }

    fn step(&mut self, forward: bool) -> Result<(), String> {
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
        // The step the app takes: the sequence's or the project's (#1986).
        let entry = if forward { self.state.take_redo() } else { self.state.take_undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }
}

/// Every keyframable effect parameter's entry and its track: what the
/// curve editor owes.
fn entries() -> Vec<(super::parity_manifest::CatalogEntry, Track)> {
    catalog().into_iter().filter_map(|e| track(&e).map(|t| (e, t))).collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    // A placed kind's track too: a Temporal Denoise's keys are listed and
    // edited through the clip's effect list (#1419).
    for (e, _) in entries() {
        manifest.require_fixture(&e, Surface::CurveEditor, "keyed");
    }
}

/// One edit through the editor: `want` on its keys, one step, undone and
/// redone exactly.
fn case(track: Track, want: impl Fn(&[(f64, f32, klipp_core::Interpolation)]) -> Result<(), String>, drive: impl Fn(&mut Curves) -> Result<(), String>) -> Result<(), String> {
    case_from(track, |_| Ok(()), want, drive)
}

/// As [`case`], from where `prepare` leaves the fixture, with a fresh
/// history.
fn case_from(track: Track, prepare: impl Fn(&mut Curves) -> Result<(), String>, want: impl Fn(&[(f64, f32, klipp_core::Interpolation)]) -> Result<(), String>, drive: impl Fn(&mut Curves) -> Result<(), String>) -> Result<(), String> {
    let mut c = Curves::over(track, fixture(&track)?)?;
    prepare(&mut c).map_err(|e| format!("preparing: {e}"))?;
    c.restart();
    let before = c.picture();
    drive(&mut c)?;
    if !c.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", c.unapplied));
    }
    want(&c.keys()).map_err(|e| format!("{e}: {:?}", c.keys()))?;
    if c.state.active_history().undo_count() != 1 {
        return Err(format!("{} steps, not one", c.state.active_history().undo_count()));
    }
    let after = c.picture();
    c.step(false)?;
    if c.picture() != before {
        return Err("undo doesn't give back the picture".into());
    }
    c.step(true)?;
    if c.picture() != after {
        return Err("redo doesn't give back the picture".into());
    }
    Ok(())
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

pub(super) fn run(report: &mut Report) {
    let all = entries();
    if all.is_empty() {
        report.case("curve editor", [], || Err("no keyframable effect parameter in the catalog".into()));
    }
    for (e, track) in all {
        run_track(report, &e, track);
    }
}

/// The cases on one parameter's track, named with it.
fn run_track(report: &mut Report, e: &super::parity_manifest::CatalogEntry, track: Track) {
    let key = |interaction| CoverageKey::new(e.type_id, &e.parameter, Surface::CurveEditor, "keyed", interaction);
    let name = |what: &str| format!("{} / {}: curve editor {what}", e.type_id, e.parameter);

    // Previous and Next keyframe (Alt+←/→, `keyframe_jump`) walk the track
    // the editor shows (#1401): from between its keys, back to the first
    // and on to the second, writing nothing.
    for (forward, interaction, want) in [(false, Interaction::PreviousKey, KEYS[0].0), (true, Interaction::NextKey, KEYS[1].0)] {
        report.case(&name(if forward { "next key" } else { "previous key" }), [key(interaction)], || {
            let mut c = Curves::over(track, fixture(&track)?)?;
            c.state.show_curve_editor = true;
            c.frame(Vec::new());
            let start = c.clip().timeline_in.to_secs_f64();
            let between = (KEYS[0].0 + KEYS[1].0) / 2.0;
            c.state.playhead = klipp_core::Timecode::from_secs_f64(start + between, c.state.playhead.rate);
            let before = c.picture();
            let to = super::shortcuts::keyframe_jump(&c.state, forward).ok_or("no key that way: the jump doesn't follow the track shown")?;
            let at = to.to_secs_f64() - start;
            if !near(at, want, 1e-3) {
                return Err(format!("jumped to {at}, the key is at {want}"));
            }
            if c.picture() != before || c.state.active_history().undo_count() != 0 {
                return Err("the jump wrote".into());
            }
            Ok(())
        });
    }

    report.case(&name("opened"), [key(Interaction::OpenWithoutWrite)], || {
        let mut c = Curves::over(track, fixture(&track)?)?;
        let before = c.picture();
        c.frame(Vec::new());
        if c.picture() != before || c.state.active_history().undo_count() != 0 || !c.unapplied.is_empty() {
            return Err("opening wrote".into());
        }
        Ok(())
    });

    // A double-click where there's no key adds one there, at that value.
    report.case(&name("adds a key"), [key(Interaction::AddKey), key(Interaction::UndoRedo)], || {
        case(track, |keys| if keys.len() == 3 && keys.iter().any(|k| near(k.0, 1.0, 0.02) && track.near(k.1, track.at(0.5))) { Ok(()) } else { Err("no new key near the middle at 1.0 s".into()) }, |c| {
            let at = c.at(1.0, 0.5);
            c.button(at, egui::PointerButton::Primary, true);
            c.button(at, egui::PointerButton::Primary, false);
            c.button(at, egui::PointerButton::Primary, true);
            c.button(at, egui::PointerButton::Primary, false);
            c.frame(vec![egui::Event::PointerGone]);
            Ok(())
        })
    });

    // A lane's edit drawn, on one lane of each kind (the central rules'
    // `CURVE_PIXEL_REPRESENTATIVE`): the key at the frame's time dragged up
    // the lane changes the frame there, and the CPU draws it as the GPU.
    if super::parity_manifest::curve_pixel_representative(e.type_id, &e.parameter) {
        report.case(&name("edit drawn"), [key(Interaction::Pixels)], || {
            let before = fixture(&track)?;
            let mut c = Curves::over(track, before.clone())?;
            let (from, to) = (c.at(KEYS[0].0, KEYS[0].1), c.at(KEYS[0].0, 0.7));
            c.button(from, egui::PointerButton::Primary, true);
            c.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)]);
            for k in 1..=4 {
                c.frame(vec![egui::Event::PointerMoved(from + (to - from) * (k as f32 / 4.0))]);
            }
            c.button(to, egui::PointerButton::Primary, false);
            c.frame(vec![egui::Event::PointerGone]);
            if !c.unapplied.is_empty() {
                return Err(format!("unapplied {:?}", c.unapplied));
            }
            if !c.keys().iter().any(|k| near(k.0, KEYS[0].0, 0.02) && track.near(k.1, track.at(0.7))) {
                return Err(format!("the key at {} s wasn't dragged to 70% of the lane: {:?}", KEYS[0].0, c.keys()));
            }
            let (routes, _) = super::parity_numeric::routes_for(e.type_id);
            super::parity_pixels::clip_edit_drawn_on(routes, super::parity_numeric::size_for(track.tag), &before, c.clip())
        });
    }

    // A key dragged moves, in one step.
    report.case(&name("moves a key"), [key(Interaction::EditKey)], || {
        case(track, |keys| if keys.len() == 2 && keys.iter().any(|k| near(k.0, 1.2, 0.03) && track.near(k.1, track.at(0.6))) && !keys.iter().any(|k| near(k.0, 1.5, 0.01)) { Ok(()) } else { Err("the key isn't near 1.2 s, 60% of the range".into()) }, |c| {
            let (from, to) = (c.at(1.5, 0.8), c.at(1.2, 0.6));
            c.button(from, egui::PointerButton::Primary, true);
            // Just past egui's drag threshold first, still on the key: the
            // editor finds the dragged key where the drag starts.
            c.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)]);
            for k in 1..=4 {
                let p = from + (to - from) * (k as f32 / 4.0);
                c.frame(vec![egui::Event::PointerMoved(p)]);
            }
            c.button(to, egui::PointerButton::Primary, false);
            c.frame(vec![egui::Event::PointerGone]);
            Ok(())
        })
    });

    // A key clicked, then Delete, as the shortcut does it.
    report.case(&name("removes a key"), [key(Interaction::RemoveKey)], || {
        case(track, |keys| if keys.len() == 1 && near(keys[0].0, 0.5, 0.01) { Ok(()) } else { Err("the key at 1.5 isn't the one taken".into()) }, |c| {
            let at = c.at(1.5, 0.8);
            c.click(at, egui::PointerButton::Primary);
            let sk = c.state.selected_keyframe.take().ok_or("clicking the key didn't select it")?;
            for cmd in super::shortcuts::remove_selected_keyframe(&c.state, &sk).ok_or("Delete sends nothing")? {
                c.apply(cmd);
            }
            Ok(())
        })
    });

    // The key's interpolation from the menu its right-click opens: the ones
    // the parameter's kind owes (Step for every kind; Linear and Bezier for
    // a continuous one).
    let continuous = matches!(e.value, CatalogValue::Parameter { desc, .. } if matches!(desc.kind, klipp_effects::registry::ParamKind::Float | klipp_effects::registry::ParamKind::Colour));
    for (item, interp, want) in [
        ("  Linear", KeyInterpolation::Linear, klipp_core::Interpolation::Linear),
        ("  Bezier (Custom)", KeyInterpolation::Bezier, klipp_core::Interpolation::Bezier),
        ("  Step (Hold)", KeyInterpolation::Step, klipp_core::Interpolation::Step),
    ] {
        if !continuous && interp != KeyInterpolation::Step {
            continue;
        }
        report.case(&name(&format!("sets {}", item.trim())), [key(Interaction::Interpolation(interp))], || {
            // From another interpolation, so the pick changes it.
            let from = if want == klipp_core::Interpolation::Linear { Some("  Step (Hold)") } else { None };
            case_from(
                track,
                |c| from.map_or(Ok(()), |f| pick(c, f)),
                |keys| if keys.iter().any(|k| near(k.0, 0.5, 0.01) && k.2 == want) { Ok(()) } else { Err(format!("the key at 0.5 isn't {want:?}")) },
                |c| pick(c, item),
            )
        });
    }
}

/// Right-click the key at 0.5 and pick `item` from the menu it opens.
fn pick(c: &mut Curves, item: &str) -> Result<(), String> {
    let at = c.at(0.5, 0.2);
    c.click(at, egui::PointerButton::Secondary);
    if c.state.keyframe_context_menu.is_none() {
        return Err("the right-click opened no menu".into());
    }
    let drawn = c.frame(Vec::new());
    let item_at = drawn.into_iter().rev().find(|(t, _)| t == item).map(|(_, p)| p).ok_or_else(|| format!("{item:?} isn't in the menu"))?;
    c.click(item_at, egui::PointerButton::Primary);
    Ok(())
}

/// A clip lane's Bezier handle dragged in the curve editor is one step,
/// undone exactly, as the lane's key drag is (#1424): the
/// tangent handlers record nothing themselves.
#[test]
fn a_lane_bezier_handle_drag_is_one_step() {
    use klipp_core::VideoParam;
    let track = Track { tag: "opacity", key: "value", lo: 0.0, hi: 1.0, video: Some(VideoParam::Opacity), grade: None };
    let mut clip = fixture(&track).unwrap();
    clip.edit_video_param_keyframes(VideoParam::Opacity, |keys| {
        keys[1].interpolation = klipp_core::Interpolation::Bezier;
        keys[1].tangent_out = (0.3, 0.0);
    });
    let tangent = |c: &Curves| c.clip().video_param_keyframes(VideoParam::Opacity)[1].tangent_out;
    let mut c = Curves::over(track, clip).unwrap();
    // The key selected, so its handles are drawn and grabbed.
    let key = c.at(1.5, 0.8);
    c.click(key, egui::PointerButton::Primary);
    let (steps, before) = (c.state.active_history().undo_count(), c.picture());
    let (from, to) = (c.at(1.8, 0.8), c.at(1.8, 0.5));
    c.button(from, egui::PointerButton::Primary, true);
    // Just past egui's drag threshold first, still within the handle's
    // eight pixels: the editor finds what's dragged where the drag starts.
    c.frame(vec![egui::Event::PointerMoved(from + (to - from).normalized() * 7.0)]);
    for k in 1..=4 {
        c.frame(vec![egui::Event::PointerMoved(from + (to - from) * (k as f32 / 4.0))]);
    }
    c.button(to, egui::PointerButton::Primary, false);
    c.frame(vec![egui::Event::PointerGone]);
    assert!(c.unapplied.is_empty(), "{:?}", c.unapplied);
    assert_ne!(tangent(&c), (0.3, 0.0), "the handle moved");
    assert_eq!(c.state.active_history().undo_count(), steps + 1, "one step");
    c.step(false).unwrap();
    assert!(c.picture() == before, "undo gives back the tangent");
}

#[test]
fn effect_tracks_take_the_edits_named_in_the_curve_editor() {
    let mut report = Report::default();
    report.family("curve editor", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    // Every key the family can earn, earned: not Pixels (another family's),
    // not those excluded with a citation.
    let missing: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels) && !manifest.excluded_keys.contains_key(*k) && !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}
