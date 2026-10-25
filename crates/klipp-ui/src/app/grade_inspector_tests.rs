//! The generated grade inspector (M3 step 11), drawn headless on a clip
//! whose grade holds one node of each kind, its commands applied as the
//! app applies them (`AppState::path_command`).

use crate::inspector::generated::{shown, NodeRows};
use crate::inspector::keyframe_selection::FrameSelection;
use crate::state::AppState;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::ParamDesc;
use klipp_nodes::{Node, NodeGraph, NodeId, NodeKind};
use klipp_timeline::GradeAccess;

const WIDTH: f32 = 420.0;

/// A state holding one clip whose grade is a Corrector, with a `kind`
/// node after it (the Corrector itself for `Corrector`); the clip and the
/// node, or `None` when the grade won't take one.
pub(super) fn graded(kind: NodeKind) -> Option<(AppState, ClipId, NodeId)> {
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    clip.set_grade(g);
    let node = if kind == NodeKind::Corrector {
        corrector
    } else {
        let n = Node { id: NodeId::new_v4(), kind, label: kind.display_name().to_string(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        let id = n.id;
        clip.insert_grade_node(Some(corrector), n).then_some(id)?
    };
    clip.picture_v2().ok()?;
    let mut state = AppState::default();
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let id = clip.id;
    state.sequences[0].tracks[v1].clips.push(clip);
    Some((state, id, node))
}

fn clip(state: &AppState, id: ClipId) -> &klipp_timeline::Clip {
    state.sequences[0].all_clips().find(|c| c.id == id).unwrap()
}

/// The inspector drawn on one node, frame by frame, as the app draws it.
struct Run {
    ctx: egui::Context,
    state: AppState,
    clip: ClipId,
    node: NodeId,
    time: f64,
    clock: f64,
    /// The per-node layout's panel, for the specialist editors; `None`
    /// draws those rows read-only.
    panel: Option<crate::panels::node_inspector::NodeInspectorPanel>,
    /// The graph a stand-in is put on with while its root has no grade,
    /// kept as the app keeps it (`timeline_grade_fresh`).
    fresh: Option<(ClipId, NodeGraph)>,
}

impl Run {
    fn new(kind: NodeKind) -> Option<Self> {
        let (state, clip, node) = graded(kind)?;
        // The icons load as in the app: a failed one is drawn as a warning
        // filling the panel, which pushed a long page's rows off it.
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        Some(Self { ctx, state, clip, node, time: 0.5, clock: 0.0, panel: None, fresh: None })
    }

    /// One frame with `events`: the commands it pushed, the rows it drew
    /// (`None`: it drew nothing) and the texts it painted, by their centres.
    fn frame(&mut self, events: Vec<egui::Event>) -> (Vec<AppCommand>, bool, Vec<(String, egui::Pos2)>) {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 2400.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = crate::theme::KlippTheme::default();
        let mut cmds = Vec::new();
        let mut drawn = false;
        let clip = clip(&self.state, self.clip).clone();
        let frame = FrameSelection { time_secs: self.time, tolerance: 0.02, clip_start: clip.timeline_in };
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                drawn = crate::inspector::grade_view::show(ui, &theme, &clip, &[], self.node, &mut cmds, frame, self.panel.as_mut());
            });
        });
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (cmds, drawn, texts)
    }

    /// A click at `at`: what the press and the release pushed.
    fn click(&mut self, at: egui::Pos2) -> Vec<AppCommand> {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = self.frame(vec![egui::Event::PointerMoved(at)]).0;
        cmds.extend(self.frame(vec![egui::Event::PointerMoved(at), button(true)]).0);
        cmds.extend(self.frame(vec![egui::Event::PointerMoved(at), button(false)]).0);
        cmds.extend(self.frame(vec![egui::Event::PointerGone]).0);
        cmds
    }

    /// Where the `nth` of the `of` rows labelled `label` is drawn (its
    /// label's centre): counted from the bottom, since the node's header
    /// may name the node as its one row does.
    fn row(&mut self, label: &str, nth: usize, of: usize) -> Option<egui::Pos2> {
        self.frame(Vec::new());
        let mut at: Vec<egui::Pos2> = self.frame(Vec::new()).2.into_iter().filter(|(t, _)| t == label).map(|(_, p)| p).collect();
        at.sort_by(|a, b| a.y.total_cmp(&b.y));
        let first = at.len().checked_sub(of)?;
        at.get(first + nth).copied()
    }

    fn texts(&mut self) -> Vec<String> {
        self.frame(Vec::new()).2.into_iter().map(|(t, _)| t).collect()
    }

    /// The row's diamond clicked: the first thing a click finds at its
    /// height, sweeping in from the right (where the diamond is when the
    /// parameter has no keys).
    fn click_diamond(&mut self, label: &str, nth: usize, of: usize) -> Vec<AppCommand> {
        let Some(at) = self.row(label, nth, of) else { return Vec::new() };
        let mut x = WIDTH - 2.0;
        while x > WIDTH - 80.0 {
            let cmds = self.click(egui::pos2(x, at.y));
            if !cmds.is_empty() {
                return cmds;
            }
            x -= 2.0;
        }
        Vec::new()
    }

    /// Apply `cmds` as the app's dispatch does: each key's time resolved
    /// to the stored key it names within the frame, then the path edit.
    fn apply(&mut self, cmds: &[AppCommand]) {
        for cmd in cmds {
            let mut cmd = cmd.clone();
            crate::inspector::keyframe_selection::resolve_command(&self.state.sequences[0], &mut cmd);
            assert_eq!(self.state.path_command(&cmd), Some(true), "{cmd:?} applied: {:?}", self.state.toast.message_for_test());
        }
    }

    /// `apply`, a refusal handed back (the toast's words) rather than
    /// failed on.
    fn try_apply(&mut self, cmds: &[AppCommand]) -> Result<(), String> {
        for cmd in cmds {
            let mut cmd = cmd.clone();
            crate::inspector::keyframe_selection::resolve_command(&self.state.sequences[0], &mut cmd);
            if self.state.path_command(&cmd) != Some(true) {
                return Err(self.state.toast.message_for_test().unwrap_or_default().to_string());
            }
        }
        Ok(())
    }

    fn v1(&self) -> &Node {
        clip(&self.state, self.clip).grade().nodes.iter().find(|n| n.id == self.node).unwrap()
    }
}

/// The grade kinds the catalog describes.
fn kinds() -> Vec<(NodeKind, &'static klipp_nodes::registry::NodeType)> {
    klipp_nodes::registry::node_types().map(|t| (t.kind, t)).collect()
}

/// The parameters a node of type `ty` shows at its defaults.
fn shown_at_default(ty: &klipp_nodes::registry::NodeType) -> Vec<(&'static ParamDesc, klipp_effects::registry::presentation::Presentation)> {
    // An HDR Zones page shows its picked zone's rows, Global's until one
    // is picked (#240); each zone's own are driven picked, by the grade
    // families.
    let presentation = |p: &ParamDesc| {
        let mut shown = ty.presentation(p);
        if ty.kind == NodeKind::HdrZones && !crate::inspector::grade_view::hdr_zone_shows(p.key, 0) {
            shown.widget = klipp_effects::registry::presentation::Widget::Hidden;
        }
        shown
    };
    let stored = |_: &ParamDesc| serde_json::Value::Null;
    let keys = |_: &ParamDesc| (None, Default::default());
    let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    let params: &'static [ParamDesc] = ty.desc.params;
    shown(&rows).into_iter().filter_map(|(d, p)| params.iter().find(|x| x.key == d.key).map(|x| (x, p))).collect()
}

/// Every keyframable grade parameter a node shows has a diamond, and a
/// click on it keys the node by path (`AddKeyAt` on `[group, node]`); a
/// second click takes the key away (`RemoveKeyAt`). The key lands on the
/// grade node's own track.
#[test]
fn every_keyframable_grade_param_shows_a_diamond() {
    let mut misses = Vec::new();
    let mut keyed = 0;
    let mut stepped = Vec::new();
    let mut unwritable = Vec::new();
    for (kind, ty) in kinds() {
        let Some(mut run) = Run::new(kind) else { continue };
        let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
        let rows = shown_at_default(ty);
        let label_of = |d: &ParamDesc, p: &klipp_effects::registry::presentation::Presentation| p.label.unwrap_or(d.label);
        for (i, (desc, pres)) in rows.iter().enumerate() {
            let (desc, pres) = (*desc, pres);
            if !desc.keyframable || matches!(pres.widget, Widget::Custom(_)) {
                continue;
            }
            let is_stepped = matches!(pres.widget, Widget::Toggle | Widget::Combo);
            if is_stepped {
                stepped.push(format!("{kind:?} {}", desc.key));
            }
            let label = label_of(desc, pres);
            let same: Vec<usize> = rows.iter().enumerate().filter(|(_, (d, p))| label_of(d, p) == label).map(|(j, _)| j).collect();
            let (nth, of) = (same.iter().position(|j| *j == i).unwrap(), same.len());
            if run.row(label, nth, of).is_none() {
                let texts = run.texts();
                misses.push(format!("{kind:?} {}: no row {label:?} among {texts:?}", desc.key));
                continue;
            }
            let add = run.click_diamond(label, nth, of);
            // A switch's or a choice's key holds until the next (#1318).
            let step = |k: &serde_json::Value| serde_json::from_value::<klipp_core::Keyframe>(k.clone()).is_ok_and(|k| k.interpolation == klipp_core::Interpolation::Step);
            match add.as_slice() {
                [AppCommand::AddKeyAt { addr: a, key, keyframe }] if *a == addr && key.as_str() == desc.key && (!is_stepped || step(keyframe)) => {}
                other => {
                    misses.push(format!("{kind:?} {}: the diamond gave {other:?}", desc.key));
                    continue;
                }
            }
            // A matte node this fixture chains into the picture leaves a
            // graph that takes no edit at all, a value's no more than a
            // key's: the diamond is right, the fixture can't hold the key.
            if let Err(why) = run.try_apply(&add) {
                if why.contains("PortTypeMismatch") {
                    unwritable.push(format!("{kind:?} {}", desc.key));
                } else {
                    misses.push(format!("{kind:?} {}: the key was refused: {why}", desc.key));
                }
                continue;
            }
            if !run.v1().keyframes.has_keyframes(desc.key) {
                misses.push(format!("{kind:?} {}: keyed by path, but the node has no track", desc.key));
                continue;
            }
            let remove = run.click_diamond(label, nth, of);
            if !matches!(remove.as_slice(), [AppCommand::RemoveKeyAt { addr: a, key, .. }] if *a == addr && key.as_str() == desc.key) {
                misses.push(format!("{kind:?} {}: the second click gave {remove:?}", desc.key));
                continue;
            }
            run.apply(&remove);
            keyed += 1;
        }
    }
    assert!(misses.is_empty(), "{} of {} missed:\n{}", misses.len(), misses.len() + keyed, misses.join("\n"));
    assert!(keyed > 20, "only {keyed} diamonds were tried");
    assert_eq!(stepped, STEPPED, "the keyframable switches and choices, each keyed as a step");
    // The Qualifier's rows shown at its defaults: its switch, its mode and
    // the HSL mode's ranges (#895).
    let qualifier = ["enabled", "mode", "hue_center", "hue_width", "hue_soft", "sat_low", "sat_high", "sat_soft", "lum_low", "lum_high", "lum_soft"].map(|k| format!("Qualifier qual_{k}"));
    // The Key Mixer's output rows (#898), its fixture a matte chained the same way.
    let key_mixer = ["KeyMixer key_mixer_output_gain", "KeyMixer key_mixer_output_offset"].map(String::from);
    let want: Vec<String> = qualifier.into_iter().chain(["LumaKey luma_key_enabled".to_string()]).chain(key_mixer).collect();
    assert_eq!(unwritable, want, "the rows whose fixture takes no edit");
}

/// The keyframable switches and choices: their diamonds key a step
/// (#1318).
const STEPPED: &[&str] = &[
    "Corrector wheels_log_mode",
    "Corrector curves_preserve_luma",
    "ColorWheels wheels_log_mode",
    "RGBCurves curves_preserve_luma",
    "Qualifier qual_enabled",
    "Qualifier qual_mode",
    "LumaKey luma_key_enabled",
    "Lut3D lut_input_space",
    "Lut3D lut_output_space",
    "PrimariesMatrix primaries_from",
    "PrimariesMatrix primaries_to",
];

/// A switch keyed through its diamond switches at the key and doesn't
/// blend: a Corrector's log wheels keyed off at 0 s and on at 1 s are off
/// until 1 s, then on (#1318).
#[test]
fn a_keyed_switch_holds_until_its_next_key() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    for (t, v) in [(0.0, 0.0f32), (1.0, 1.0)] {
        let key = klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Step, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        run.apply(&[AppCommand::AddKeyAt { addr: addr.clone(), key: "wheels_log_mode".into(), keyframe: serde_json::to_value(key).unwrap() }]);
    }
    let at = |t: f64| run.v1().keyframes.evaluate_all(t).into_iter().find(|(k, _)| k == "wheels_log_mode").map(|(_, v)| v);
    assert_eq!(at(0.5), Some(0.0), "held off until the next key");
    assert_eq!(at(0.999), Some(0.0), "still off just before it");
    assert_eq!(at(1.0), Some(1.0), "on from the key");
}

/// A Corrector's exposure keyed through its row animates: keys at 0 s and
/// 1 s give the halfway value at 0.5 s on the grade node the engine reads.
#[test]
fn a_keyed_corrector_exposure_animates() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    for (t, v) in [(0.0, 0.0f32), (1.0, 1.0)] {
        let key = klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        run.apply(&[AppCommand::AddKeyAt { addr: addr.clone(), key: "exposure".into(), keyframe: serde_json::to_value(key).unwrap() }]);
    }
    let at = run.v1().keyframes.evaluate_all(0.5);
    let exposure = at.iter().find(|(k, _)| k == "exposure").map(|(_, v)| *v);
    assert!(exposure.is_some_and(|v| (v - 0.5).abs() < 1e-4), "{at:?}");
    run.time = 0.5;
    let (cmds, drawn, texts) = run.frame(Vec::new());
    assert!(drawn && cmds.is_empty(), "drawn {drawn}, wrote {cmds:?}");
    assert!(texts.iter().any(|(t, _)| t.starts_with("0.50")), "the row shows the animated value: {texts:?}");
}

/// A Colour Wheels node draws no saturation or brightness rows: its type
/// describes none, and its stage applies none (#1295).
#[test]
fn colour_wheels_draws_no_saturation_or_brightness() {
    let ty = klipp_nodes::registry::node_type(NodeKind::ColorWheels).unwrap();
    let keys: Vec<&str> = shown_at_default(ty).iter().map(|(d, _)| d.key).collect();
    assert!(keys.iter().any(|k| k.starts_with("lift_")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.ends_with("_sat") || k.ends_with("_bri")), "{keys:?}");
    let mut run = Run::new(NodeKind::ColorWheels).unwrap();
    let (cmds, drawn, texts) = run.frame(Vec::new());
    assert!(drawn && cmds.is_empty(), "drawn {drawn}, wrote {cmds:?}");
    assert!(!texts.iter().any(|(t, _)| t == "S" || t == "B"), "{texts:?}");
}

/// Drawing a node writes nothing, for every kind: no command on open, nor
/// on the frames after.
#[test]
fn opening_a_grade_node_writes_nothing() {
    let mut drawn = 0;
    for (kind, _) in kinds() {
        let Some(mut run) = Run::new(kind) else { continue };
        for _ in 0..3 {
            let (cmds, shown, _) = run.frame(Vec::new());
            assert!(shown, "{kind:?} drawn");
            assert!(cmds.is_empty(), "{kind:?} wrote on open: {cmds:?}");
        }
        drawn += 1;
    }
    assert!(drawn > 25, "only {drawn} kinds drawn");
}

/// A key off the frame grid, shown at the playhead's frame, is the key
/// the row's diamond takes away, and the one an edit there changes: the
/// commands resolve to its stored time, as an effect's do.
#[test]
fn an_off_grid_grade_key_is_the_one_its_row_edits() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    let key = klipp_core::Keyframe { time_secs: 0.49, value: 0.3, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    run.apply(&[AppCommand::AddKeyAt { addr: addr.clone(), key: "exposure".into(), keyframe: serde_json::to_value(key.clone()).unwrap() }]);
    // At 0.50 s (25 fps), the key at 0.49 s is in the frame.
    run.time = 0.5;
    let times = |run: &Run| run.v1().keyframes.tracks.iter().find(|t| t.param_name == "exposure").map(|t| t.keyframes.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>()).unwrap_or_default();
    // An edit at the playhead changes that key, not a second one.
    let edit = AppCommand::AddKeyAt { addr: addr.clone(), key: "exposure".into(), keyframe: serde_json::to_value(klipp_core::Keyframe { time_secs: 0.5, value: 0.6, ..key }).unwrap() };
    run.apply(&[edit]);
    assert_eq!(times(&run), vec![(0.49, 0.6)], "the key in the frame is changed");
    // Its filled diamond takes it away.
    let remove = run.click_diamond("Exposure", 0, 1);
    assert!(matches!(remove.as_slice(), [AppCommand::RemoveKeyAt { .. }]), "{remove:?}");
    run.apply(&remove);
    assert_eq!(times(&run), Vec::<(f64, f32)>::new(), "the key at 0.49 s is gone");
}

impl Run {
    /// As `new`, with the per-node layout's panel, so the specialist
    /// editors are drawn.
    fn with_panel(kind: NodeKind) -> Option<Self> {
        let mut run = Self::new(kind)?;
        run.panel = Some(Default::default());
        Some(run)
    }

    /// One frame's commands and drawn controls, as the row recorder saw
    /// them.
    fn recorded(&mut self) -> (Vec<AppCommand>, Vec<crate::widgets::row_recorder::Row>) {
        let ((cmds, _, _), rows) = crate::widgets::row_recorder::capture(|| self.frame(Vec::new()));
        (cmds, rows)
    }
}

/// With the panel, every specialist editor a grade row names is drawn by
/// the per-node layout's widget: no row is left read-only, and opening
/// writes nothing.
#[test]
fn every_grade_editor_is_drawn_with_the_panel() {
    let mut drawn = 0;
    for (kind, _) in kinds() {
        let Some(mut run) = Run::with_panel(kind) else { continue };
        run.frame(Vec::new());
        let (cmds, rows) = run.recorded();
        assert!(cmds.is_empty(), "{kind:?} wrote on open: {cmds:?}");
        let read_only: Vec<_> = rows.iter().filter(|r| r.widget == "read_only").map(|r| r.parameter.clone()).collect();
        assert!(read_only.is_empty(), "{kind:?} left {read_only:?} read-only");
        drawn += 1;
    }
    assert!(drawn > 25, "only {drawn} kinds drawn");
}

/// The wheels are one block: four wheels, each with its saturation and
/// brightness where the type has them (a Corrector), and without where
/// it hasn't (a Colour Wheels node, whose stage reads none: #1295).
#[test]
fn wheels_carry_sat_and_bri_only_where_the_type_has_them() {
    for (kind, sliders) in [(NodeKind::Corrector, 8), (NodeKind::ColorWheels, 0)] {
        let mut run = Run::with_panel(kind).unwrap();
        run.frame(Vec::new());
        let (_, rows) = run.recorded();
        let wheels = rows.iter().filter(|r| r.widget == "colour_wheel").count();
        let sat_bri = rows.iter().filter(|r| r.widget == "wheel_slider").count();
        assert_eq!((wheels, sat_bri), (4, sliders), "{kind:?}");
    }
}

/// A node whose own widget draws all its parameters is drawn by that
/// widget in place of rows.
#[test]
fn whole_node_widgets_stand_in_for_rows() {
    // Each widget's own control (a difference key shows its plate once
    // it's on, so its switch).
    for (kind, widget, parameter) in [(NodeKind::ExternalMatte, "image_picker", "matte_image_path"), (NodeKind::DifferenceKey, "checkbox", "difference_key.enabled"), (NodeKind::LayerMixer, "layer_mixer", "layer_mixer")] {
        let Some(mut run) = Run::with_panel(kind) else { panic!("{kind:?} isn't drawn") };
        run.frame(Vec::new());
        let (_, rows) = run.recorded();
        let drawn: Vec<_> = rows.iter().map(|r| (r.widget.as_str(), r.parameter.as_deref())).collect();
        assert!(drawn.contains(&(widget, Some(parameter))), "{kind:?}: {drawn:?}");
    }
}

/// The parameters the tabbed page shares with the per-node layout.
const SHARED: &[&str] = &["exposure", "contrast", "contrast_pivot", "clarity", "saturation", "vibrance", "highlights", "shadows", "whites", "blacks", "temperature", "tint", "hue", "global_mix"];

/// The tone rows, shared on a Tone Controls node as on the Corrector (#1420).
const TONE: &[&str] = &["highlights", "shadows", "whites", "blacks"];

/// The tabbed page's Adjustments tab, one frame with `events`, its
/// generated rows' snapshot set as the app sets it, and `selected` the
/// canvas selection: its commands, what the recorder saw
/// and its texts.
fn tabbed(run: &mut Run, selected: Option<NodeId>, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<crate::widgets::row_recorder::Row>, Vec<(String, egui::Pos2)>) {
    tabbed_in(run, false, selected, events)
}

/// As [`tabbed`], in the right inspector's layout or (`wide`) the Colour
/// page's bottom tools'.
fn tabbed_in(run: &mut Run, wide: bool, selected: Option<NodeId>, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<crate::widgets::row_recorder::Row>, Vec<(String, egui::Pos2)>) {
    tabbed_on(run, wide, crate::state::ColorTab::Adjustments, selected, events)
}

/// As [`tabbed_in`], on the tab `on`.
fn tabbed_on(run: &mut Run, wide: bool, on: crate::state::ColorTab, selected: Option<NodeId>, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<crate::widgets::row_recorder::Row>, Vec<(String, egui::Pos2)>) {
    let c = clip(&run.state, run.clip).clone();
    let mut panel = run.panel.take().unwrap_or_default();
    panel.generated = crate::inspector::grade_view::snapshot_for(&c, &[], false, &mut run.state.pending_corrector);
    let mut graph = c.grade().clone();
    let theme = crate::theme::KlippTheme::default();
    let frame = FrameSelection { time_secs: run.time, tolerance: 0.02, clip_start: c.timeline_in };
    run.clock += 0.05;
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 2400.0))), time: Some(run.clock), events, ..Default::default() };
    let mut cmds = Vec::new();
    let mut texts = Vec::new();
    let ((), rows) = crate::widgets::row_recorder::capture(|| {
        let out = run.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut sel = selected;
                let mut tab = on;
                cmds = if wide {
                    panel.show_simple_horizontal(ui, &mut graph, c.id, &mut sel, &mut tab, &theme, frame)
                } else {
                    panel.show_simple(ui, &mut graph, c.id, &mut sel, &mut tab, &theme, frame)
                };
            });
        });
        texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
    });
    // Each caller drops the snapshot after the draw, so none is reused.
    panel.generated = None;
    run.panel = Some(panel);
    (cmds, rows, texts)
}

/// A recorded row as its control is shown: what must be the same on both
/// pages for one parameter.
fn shown_as(r: &crate::widgets::row_recorder::Row) -> String {
    format!("{} {} {:?} {:?} {:?} {:?} {} diamond={}", r.widget, r.label, r.range, r.reset, r.step, r.decimals, r.suffix, r.keyframe_state.is_some())
}

/// The parameters the tabbed page shares with the per-node layout are one
/// row on both: the same widget, label, range, reset, step, decimals,
/// suffix and diamond, from the same descriptor (M3 decision §3).
#[test]
fn the_shared_controls_are_one_row_on_both_pages() {
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    let (cmds, tab_rows, _) = tabbed(&mut run, None, Vec::new());
    assert!(cmds.is_empty(), "the tabbed page wrote on open: {cmds:?}");
    run.frame(Vec::new());
    let (_, node_rows) = run.recorded();
    for key in SHARED {
        let find = |rows: &[crate::widgets::row_recorder::Row]| rows.iter().find(|r| r.parameter.as_deref() == Some(key)).map(shown_as);
        let (on_tab, on_node) = (find(&tab_rows), find(&node_rows));
        assert!(on_tab.is_some(), "{key} isn't on the tabbed page");
        assert_eq!(on_tab, on_node, "{key}: the tabbed page's row isn't the per-node layout's");
    }
}

/// A Tone Controls node's four rows are the per-node layout's on the
/// tabbed page and the bottom tools too, not the old rows (#1420).
#[test]
fn a_tone_controls_node_s_rows_are_one_row_on_both_pages() {
    for wide in [false, true] {
        let mut run = Run::with_panel(NodeKind::ToneControls).unwrap();
        let node = run.node;
        let (cmds, tab_rows, _) = tabbed_in(&mut run, wide, Some(node), Vec::new());
        assert!(cmds.is_empty(), "wide {wide}: the page wrote on open: {cmds:?}");
        run.frame(Vec::new());
        let (_, node_rows) = run.recorded();
        for key in TONE {
            let find = |rows: &[crate::widgets::row_recorder::Row]| rows.iter().find(|r| r.parameter.as_deref() == Some(key)).map(shown_as);
            let (on_tab, on_node) = (find(&tab_rows), find(&node_rows));
            assert!(on_tab.is_some(), "wide {wide}: {key} isn't on the page");
            assert_eq!(on_tab, on_node, "wide {wide}: {key}: the page's row isn't the per-node layout's");
        }
    }
}

/// The tabbed page writes each shared parameter to the node it always
/// targeted: the selected node of the row's kind, else the Corrector.
/// Only the row drawing it changed: its reset (a click on its label) sets
/// `exposure` by path on that node.
#[test]
fn tabbed_targeting_is_unchanged() {
    let mut run = Run::with_panel(NodeKind::Exposure).unwrap();
    let exposure_node = run.node;
    let corrector = clip(&run.state, run.clip).grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    // Both away from the default, so the row's label resets them.
    for id in [exposure_node, corrector] {
        let c = run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap();
        c.edit_grade(|g| g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(0.7));
    }
    for (selected, want) in [(Some(exposure_node), exposure_node), (None, corrector)] {
        tabbed(&mut run, selected, Vec::new());
        let (_, _, texts) = tabbed(&mut run, selected, Vec::new());
        let at = texts.iter().filter(|(t, _)| t == "Exposure").map(|(_, p)| *p).next_back().expect("the Exposure row");
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = tabbed(&mut run, selected, vec![egui::Event::PointerMoved(at)]).0;
        cmds.extend(tabbed(&mut run, selected, vec![egui::Event::PointerMoved(at), press(true)]).0);
        cmds.extend(tabbed(&mut run, selected, vec![egui::Event::PointerMoved(at), press(false)]).0);
        let set: Vec<_> = cmds.iter().filter_map(|c| match c {
            AppCommand::SetParamAt { addr, key, .. } if key.as_str() == "exposure" => addr.node(),
            _ => None,
        }).collect();
        assert_eq!(set, vec![want], "selected {selected:?}: {cmds:?}");
    }
}

/// A clip's snapshot is taken of its own grade at any depth: inside a
/// compound its nodes have paths through it (#1391), and a descent that
/// names no compound has none. A stand-in's (the timeline grade, a group's
/// stage) is taken at its own root instead (`snapshot_for_root`, M3 step 19).
#[test]
fn the_shared_rows_are_generated_at_any_depth_and_not_for_a_stand_in() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let id = run.clip;
    let c = run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
    let mut compound = klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind: NodeKind::Compound, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
    compound.params.compound_subgraph = Some(klipp_timeline::GradeAccess::grade(&*c).clone());
    let at = compound.id;
    let anchor = klipp_timeline::GradeAccess::grade(&*c).nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| n.id);
    assert!(klipp_timeline::GradeAccess::insert_grade_node(c, anchor, compound));
    let c = clip(&run.state, id);
    use crate::inspector::grade_view::snapshot_for;
    assert!(snapshot_for(c, &[], false, &mut None).is_some(), "the clip's own grade");
    assert!(snapshot_for(c, &[at], false, &mut None).is_some(), "inside its compound");
    assert!(snapshot_for(c, &[klipp_nodes::NodeId::new_v4()], false, &mut None).is_none(), "a descent that names no compound");
    assert!(snapshot_for(c, &[], true, &mut None).is_none(), "a stand-in's grade");
}

/// The Colour page's bottom tools draw the shared rows from this frame's
/// snapshot, as the right inspector does: a value changed between draws
/// (by path, as an edit there sends it) shows at once, never a snapshot
/// left by an earlier draw.
#[test]
fn the_bottom_tools_draw_the_current_values() {
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    let exposure = |rows: &[crate::widgets::row_recorder::Row]| rows.iter().find(|r| r.parameter.as_deref() == Some("exposure")).map(|r| (r.keyframe_state.is_some(), r.value.clone()));
    for v in [0.3f32, 0.6] {
        run.apply(&[AppCommand::SetParamAt { addr: addr.clone(), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(v) }]);
        let (_, right, _) = tabbed_in(&mut run, false, None, Vec::new());
        let (cmds, bottom, _) = tabbed_in(&mut run, true, None, Vec::new());
        assert!(cmds.is_empty(), "the bottom tools wrote on open: {cmds:?}");
        assert_eq!(exposure(&bottom), exposure(&right), "the bottom tools' exposure row is the right inspector's");
        assert_eq!(exposure(&bottom).map(|(_, value)| value), Some(serde_json::json!(v)), "the current value, {v}");
    }
}

/// A clip whose grade has no enabled Corrector: none at all (its Source
/// straight to its Output), or (`disabled`) one turned off.
fn without_corrector(disabled: bool) -> Run {
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    let corrector = run.node;
    let c = run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap();
    c.edit_grade(|g| {
        if disabled {
            g.nodes.iter_mut().find(|n| n.id == corrector).unwrap().enabled = false;
            return;
        }
        let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let output = g.output_node_id;
        g.nodes.retain(|n| n.id != corrector);
        g.edges.retain(|e| e.from_node != corrector && e.to_node != corrector);
        g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: output, to_port: 0 });
        g.node_positions.remove(&corrector);
    });
    run
}

/// The clip as it is saved: its fields and its stored picture.
fn saved(state: &AppState, id: ClipId) -> String {
    let c = clip(state, id);
    format!("{}{}", serde_json::to_string(c).unwrap(), serde_json::to_string(&c.picture_v2().unwrap().picture).unwrap())
}

/// The commands, each through the dispatch boundary and then applied as a
/// path edit, as the app applies the tabbed page's.
fn dispatch(run: &mut Run, cmds: &[AppCommand]) {
    for cmd in cmds {
        run.state.before_dispatch(cmd);
        assert!(run.state.path_command(cmd).is_some(), "{cmd:?} is a path edit");
    }
}

/// A drag of the row labelled `label` on tab `on`: pressed on its value,
/// moved `dx` in three steps, then released, or (`escape`) Escape before
/// the release. The commands each frame pushed, applied as they come.
fn drag_row(run: &mut Run, on: crate::state::ColorTab, label: &str, dx: f32, escape: bool) -> Vec<AppCommand> {
    drag_with(run, &mut |run, events| tabbed_on(run, false, on, None, events), label, dx, escape)
}

/// What one drawn frame gave: its commands, the controls the recorder saw
/// and the texts painted.
type Drawn = (Vec<AppCommand>, Vec<crate::widgets::row_recorder::Row>, Vec<(String, egui::Pos2)>);

/// [`drag_row`] on what `draw` draws each frame.
fn drag_with(run: &mut Run, draw: &mut dyn FnMut(&mut Run, Vec<egui::Event>) -> Drawn, label: &str, dx: f32, escape: bool) -> Vec<AppCommand> {
    draw(run, Vec::new());
    let texts = draw(run, Vec::new()).2;
    let label_at = texts.iter().filter(|(t, _)| t == label).map(|(_, p)| *p).next_back().unwrap_or_else(|| panic!("no {label} row: {texts:?}"));
    let value_at = texts.iter()
        .filter(|(t, p)| (p.y - label_at.y).abs() < 4.0 && p.x > label_at.x && t.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit()))
        .map(|(_, p)| *p).min_by(|a, b| a.x.total_cmp(&b.x)).unwrap_or_else(|| panic!("no value on the {label} row"));
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut all = Vec::new();
    let mut step = |run: &mut Run, events| {
        let cmds = draw(run, events).0;
        dispatch(run, &cmds);
        all.extend(cmds);
    };
    step(run, vec![egui::Event::PointerMoved(value_at)]);
    step(run, vec![button(value_at, true)]);
    for i in 1..=3 {
        step(run, vec![egui::Event::PointerMoved(value_at + egui::vec2(dx * i as f32 / 3.0, 0.0))]);
    }
    let end = value_at + egui::vec2(dx, 0.0);
    if escape {
        step(run, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    }
    step(run, vec![button(end, false)]);
    step(run, vec![egui::Event::PointerGone]);
    all
}

fn undo_once(state: &mut AppState) {
    let entry = state.active_history_mut().undo().expect("a step to undo");
    let mut command = entry.command;
    if let klipp_document::history::EditCommand::Named { edit, .. } = command { command = *edit; }
    let klipp_document::history::EditCommand::PictureEdit { clip_id, edit, .. } = command else { panic!("a picture edit") };
    state.set_picture(clip_id, &edit.before);
}

fn redo_once(state: &mut AppState) {
    let entry = state.active_history_mut().redo().expect("a step to redo");
    let mut command = entry.command;
    if let klipp_document::history::EditCommand::Named { edit, .. } = command { command = *edit; }
    let klipp_document::history::EditCommand::PictureEdit { clip_id, edit, .. } = command else { panic!("a picture edit") };
    state.set_picture(clip_id, &edit.after);
}

/// The enabled Correctors of the clip's grade.
fn correctors(run: &Run) -> Vec<Node> {
    clip(&run.state, run.clip).grade().nodes.iter().filter(|n| n.kind == NodeKind::Corrector && n.enabled).cloned().collect()
}

/// A clip with no enabled Corrector opens every tab that makes one
/// generated, drawn from the Corrector's defaults under one reserved id,
/// and writes nothing: no command, the clip saved as it was, and no node
/// added (M3 step 19).
#[test]
fn a_clip_without_a_corrector_opens_its_tabs_writing_nothing() {
    use crate::state::ColorTab;
    for disabled in [false, true] {
        let mut run = without_corrector(disabled);
        let before = saved(&run.state, run.clip);
        for tab in [ColorTab::Wheels, ColorTab::Curves, ColorTab::HueSat, ColorTab::Warper] {
            let (first, _, _) = tabbed_on(&mut run, false, tab, None, Vec::new());
            let reserved = run.state.pending_corrector;
            let (cmds, rows, _) = tabbed_on(&mut run, false, tab, None, Vec::new());
            assert!(first.is_empty() && cmds.is_empty(), "{tab:?} wrote on open: {first:?} {cmds:?}");
            assert!(matches!(reserved, Some((c, _)) if c == run.clip), "{tab:?}: a Corrector reserved for the clip");
            assert_eq!(run.state.pending_corrector, reserved, "{tab:?}: the same id every frame");
            assert!(!rows.is_empty() && rows.iter().all(|r| r.widget != "read_only"), "{tab:?} drew {:?}", rows.iter().map(|r| &r.widget).collect::<Vec<_>>());
        }
        assert_eq!(saved(&run.state, run.clip), before, "disabled {disabled}: nothing written");
        assert!(correctors(&run).is_empty());
    }
}

/// The first edit of a clip with no enabled Corrector makes it, under the
/// id the page reserved, as one step with the edit; undo leaves the clip
/// saved as it was, and redo brings the Corrector back under that id. A
/// disabled Corrector is left as it was (IC16).
#[test]
fn the_first_edit_makes_the_corrector_in_the_same_step() {
    use crate::state::ColorTab;
    for disabled in [false, true] {
        let mut run = without_corrector(disabled);
        let before = saved(&run.state, run.clip);
        let off = clip(&run.state, run.clip).grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).cloned();
        tabbed_on(&mut run, false, ColorTab::Curves, None, Vec::new());
        let (_, reserved) = run.state.pending_corrector.expect("a Corrector reserved");
        let cmds = drag_row(&mut run, ColorTab::Curves, "Mix", -60.0, false);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SetParamAt { addr, .. } if addr.node() == Some(reserved))), "the row edits the reserved Corrector: {cmds:?}");
        let made = correctors(&run);
        assert_eq!(made.len(), 1, "disabled {disabled}: one enabled Corrector made");
        assert_eq!(made[0].id, reserved, "under the reserved id");
        assert!(made[0].params.curves_mix.is_some_and(|m| m < 1.0), "the drag's value: {:?}", made[0].params.curves_mix);
        let now = clip(&run.state, run.clip).grade().nodes.iter().find(|n| Some(n.id) == off.as_ref().map(|o| o.id)).cloned();
        assert_eq!(now, off, "a disabled Corrector is left as it was");
        assert_eq!(run.state.active_history().undo_count(), 1, "one step");
        let after = saved(&run.state, run.clip);
        undo_once(&mut run.state);
        assert_eq!(saved(&run.state, run.clip), before, "undo: the clip as it was");
        redo_once(&mut run.state);
        assert_eq!(saved(&run.state, run.clip), after, "redo: the same Corrector back");
        // Made, the reservation is spent: the tab now draws the Corrector.
        tabbed_on(&mut run, false, ColorTab::Curves, None, Vec::new());
        assert_eq!(run.state.pending_corrector, None);
    }
}

/// A grade with no Corrector: its Source straight to its Output.
pub(super) fn bare_grade() -> NodeGraph {
    let mut g = NodeGraph::default();
    let corrector = g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let output = g.output_node_id;
    g.nodes.retain(|n| n.id != corrector);
    g.edges.retain(|e| e.from_node != corrector && e.to_node != corrector);
    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: output, to_port: 0 });
    g.node_positions.remove(&corrector);
    g
}

/// `inner` as a Compound in `g`, just before its Output: the compound's id.
pub(super) fn nest(g: &mut NodeGraph, inner: NodeGraph) -> NodeId {
    let mut compound = Node { id: NodeId::new_v4(), kind: NodeKind::Compound, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
    compound.params.compound_subgraph = Some(inner);
    let (id, output) = (compound.id, g.output_node_id);
    let at = g.nodes.iter().position(|n| n.id == output).unwrap();
    g.nodes.insert(at, compound);
    for e in g.edges.iter_mut().filter(|e| e.to_node == output && e.to_port == 0) {
        e.to_node = id;
    }
    g.edges.push(klipp_nodes::Edge { from_node: id, from_port: 0, to_node: output, to_port: 0 });
    id
}

/// A grade with its Corrector at the top and a compound with none, or
/// (`deep`) a compound with none inside another with none: the grade, and
/// the descent to the innermost.
fn compounded(deep: bool) -> (NodeGraph, Vec<NodeId>) {
    let mut top = NodeGraph::default();
    if !deep {
        let at = nest(&mut top, bare_grade());
        return (top, vec![at]);
    }
    let mut outer = bare_grade();
    let inner = nest(&mut outer, bare_grade());
    let at = nest(&mut top, outer);
    (top, vec![at, inner])
}

/// The enabled Correctors of `grade` at `descent`.
fn correctors_at(grade: &NodeGraph, descent: &[NodeId]) -> Vec<Node> {
    crate::inspector::grade_view::v1_level(grade, descent).expect("the level").nodes.iter().filter(|n| n.kind == NodeKind::Corrector && n.enabled).cloned().collect()
}

/// Inside a clip's grade compound with no Corrector, one level down or
/// two, the page reserves one, and the first edit by path makes it in that
/// compound under the reserved id, as one step with the edit; undo leaves
/// the clip saved as it was (#1483).
#[test]
fn the_first_edit_inside_a_compound_makes_its_corrector_in_the_same_step() {
    use klipp_graph_format::path::{NodePath, Root};
    for deep in [false, true] {
        let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
        let (grade, descent) = compounded(deep);
        let top = grade.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap().edit_grade(|g| *g = grade);
        let (before, steps) = (saved(&run.state, run.clip), run.state.active_history().undo_count());
        let c = clip(&run.state, run.clip).clone();
        let snapshot = crate::inspector::grade_view::snapshot_for(&c, &descent, false, &mut run.state.pending_corrector).expect("drawn inside the compound");
        let reserved = snapshot.pending().expect("deep {deep}: a Corrector reserved inside it");
        assert_eq!(run.state.pending_corrector, Some((run.clip, reserved)));
        let group = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), top).unwrap().0.nodes[0];
        let addr = NodePath::new(Root::Clip { clip: run.clip.0 }, std::iter::once(group).chain(descent.iter().copied()).chain([reserved]));
        let edit = AppCommand::SetParamAt { addr, key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.4) };
        assert_eq!(run.state.path_command(&edit), Some(true), "deep {deep}: {:?}", run.state.toast.message_for_test());
        let made = correctors_at(clip(&run.state, run.clip).grade(), &descent);
        assert_eq!(made.iter().map(|n| (n.id, n.params.exposure)).collect::<Vec<_>>(), vec![(reserved, Some(0.4))], "deep {deep}: made in the compound, with the edit");
        assert_eq!(correctors_at(clip(&run.state, run.clip).grade(), &[]).iter().map(|n| n.id).collect::<Vec<_>>(), vec![top], "deep {deep}: none made at the top");
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "deep {deep}: one step");
        undo_once(&mut run.state);
        assert_eq!(saved(&run.state, run.clip), before, "deep {deep}: undo, the clip as it was");
    }
}

/// The same inside a compound of a sequence grade: the timeline grade's
/// and a group's stages (#1483).
#[test]
fn the_first_edit_inside_a_sequence_grade_s_compound_makes_its_corrector() {
    use klipp_graph_format::path::NodePath;
    for deep in [false, true] {
        let (mut run, which) = sequence_grades(true);
        for w in which {
            let (grade, descent) = compounded(deep);
            let top = grade.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
            let root = w.root(&run.state.sequences[0]);
            run.state.sequences[0].set_root_grade(root, Some(grade)).unwrap();
            let (before, steps) = (stored(&run, w), run.state.active_history().undo_count());
            let id = super::timeline_grade::stand_in_grade_on(&mut run.state.sequences[0], w, &mut run.fresh).expect("stood in");
            let snapshot = crate::inspector::grade_view::snapshot_for_root(&run.state.sequences[0], id, root, &descent, &mut run.state.pending_corrector, &mut run.state.root_grade_start);
            super::timeline_grade::stand_down_timeline_grade_on(&mut run.state.sequences[0], &mut run.fresh);
            let reserved = snapshot.expect("drawn inside the compound").pending().expect("a Corrector reserved inside it");
            let addr = NodePath::new(root, descent.iter().copied().chain([reserved]));
            let edit = AppCommand::SetParamAt { addr, key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.4) };
            assert_eq!(run.state.path_command(&edit), Some(true), "{w:?} deep {deep}: {:?}", run.state.toast.message_for_test());
            let grade = run.state.sequences[0].root_grade(root).unwrap().unwrap().clone();
            let made = correctors_at(&grade, &descent);
            assert_eq!(made.iter().map(|n| (n.id, n.params.exposure)).collect::<Vec<_>>(), vec![(reserved, Some(0.4))], "{w:?} deep {deep}");
            assert_eq!(correctors_at(&grade, &[]).iter().map(|n| n.id).collect::<Vec<_>>(), vec![top], "{w:?} deep {deep}: none made at the top");
            assert_eq!(run.state.active_history().undo_count(), steps + 1, "{w:?} deep {deep}: one step");
            undo_root(&mut run.state);
            assert_eq!(stored(&run, w), before, "{w:?} deep {deep}: undo, the grade as it was");
        }
    }
}

/// A grade of `kinds` chained between its Source and its Output, and
/// their ids in order.
pub(super) fn chain_of(kinds: &[NodeKind]) -> (NodeGraph, Vec<NodeId>) {
    let mut g = bare_grade();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let output = g.output_node_id;
    g.edges.clear();
    let (mut prev, mut ids) = (source, Vec::new());
    for kind in kinds {
        let n = Node { id: NodeId::new_v4(), kind: *kind, label: format!("{kind:?}"), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
        g.edges.push(klipp_nodes::Edge { from_node: prev, from_port: 0, to_node: n.id, to_port: 0 });
        prev = n.id;
        ids.push(n.id);
        let at = g.nodes.iter().position(|x| x.id == output).unwrap();
        g.nodes.insert(at, n);
    }
    g.edges.push(klipp_nodes::Edge { from_node: prev, from_port: 0, to_node: output, to_port: 0 });
    (g, ids)
}

/// The level `descent` names in `grade`, as stored.
pub(super) fn level_of(grade: &NodeGraph, descent: &[NodeId]) -> Option<NodeGraph> {
    crate::inspector::grade_view::v1_level(grade, descent).cloned()
}

/// A grade whose level `depth` compounds down holds an Exposure, a
/// Saturation and a Contrast in a chain: the grade, the descent to that
/// level, and those three.
fn at_depth(depth: usize) -> (NodeGraph, Vec<NodeId>, Vec<NodeId>) {
    let (mut grade, ids) = chain_of(&[NodeKind::Exposure, NodeKind::Saturation, NodeKind::Contrast]);
    let mut descent = Vec::new();
    for _ in 0..depth {
        let mut outer = NodeGraph::default();
        descent.insert(0, nest(&mut outer, grade));
        grade = outer;
    }
    (grade, descent, ids)
}

/// Each structure edit, as the grade canvas makes it (#1476), on `grade`
/// at `descent`, through the editing clip `clip` (a clip, or a sequence
/// grade's stand-in): each one step, which undo takes back.
fn check_structure_edits(run: &mut Run, clip: ClipId, descent: &[NodeId], ids: &[NodeId], level: &dyn Fn(&Run) -> NodeGraph, saved: &dyn Fn(&Run) -> String, undo: &dyn Fn(&mut AppState), what: &str) {
    use super::path_commands::GradeStructure as S;
    let kinds = |g: &NodeGraph| g.nodes.iter().map(|n| (n.id, n.kind)).collect::<Vec<_>>();
    let (added, group) = (NodeId::new_v4(), NodeId::new_v4());
    let edits = [
        ("add", S::Add { kind: NodeKind::Saturation, id: added, position: (40.0, 60.0) }),
        ("remove", S::Remove { node: ids[1] }),
        ("reorder", S::Reorder { node: ids[2], after: ids[0] }),
        ("group", S::Group { nodes: ids[0..2].to_vec(), group }),
    ];
    for (name, edit) in edits {
        let (before, steps, shape) = (saved(run), run.state.active_history().undo_count(), kinds(&level(run)));
        assert!(run.state.grade_structure_edit(clip, descent, edit), "{what} {name}: {:?}", run.state.toast.message_for_test());
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "{what} {name}: one step");
        let after = level(run);
        match name {
            "add" => assert!(after.nodes.iter().any(|n| n.id == added && n.kind == NodeKind::Saturation), "{what}: added at the level"),
            "remove" => assert!(!after.nodes.iter().any(|n| n.id == ids[1]), "{what}: removed from the level"),
            "reorder" => assert_ne!(kinds(&after), shape, "{what}: reordered"),
            _ => {
                let g = after.nodes.iter().find(|n| n.id == group).expect("the compound at the level");
                let inside = g.params.compound_subgraph.as_ref().unwrap();
                assert!(ids[0..2].iter().all(|id| inside.nodes.iter().any(|n| n.id == *id)), "{what}: grouped inside it");
                assert!(!after.nodes.iter().any(|n| ids[0..2].contains(&n.id)), "{what}: out of the level");
            }
        }
        undo(&mut run.state);
        assert_eq!(saved(run), before, "{what} {name}: undo, the grade as it was");
    }
    // Group, then Ungroup that compound: one step each.
    assert!(run.state.grade_structure_edit(clip, descent, S::Group { nodes: ids[0..2].to_vec(), group }));
    let (grouped, steps) = (saved(run), run.state.active_history().undo_count());
    assert!(run.state.grade_structure_edit(clip, descent, S::Ungroup { node: group }), "{what} ungroup: {:?}", run.state.toast.message_for_test());
    assert_eq!(run.state.active_history().undo_count(), steps + 1, "{what} ungroup: one step");
    let after = level(run);
    assert!(!after.nodes.iter().any(|n| n.id == group) && ids[0..2].iter().all(|id| after.nodes.iter().any(|n| n.id == *id)), "{what}: ungrouped back into the level");
    undo(&mut run.state);
    assert_eq!(saved(run), grouped, "{what} ungroup: undo, the compound back");
}

/// Add, remove, reorder, Group and Ungroup on a clip's grade, at its top,
/// one compound down and two, each by path in one step (#1476).
#[test]
fn a_clip_grade_s_structure_edits_are_one_step_by_path_at_any_depth() {
    for depth in 0..3 {
        let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
        let (grade, descent, ids) = at_depth(depth);
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap().edit_grade(|g| *g = grade);
        let clip_id = run.clip;
        let d = descent.clone();
        check_structure_edits(
            &mut run,
            clip_id,
            &descent,
            &ids,
            &|run| crate::inspector::grade_view::v1_level(clip(&run.state, run.clip).grade(), &d).expect("the level").clone(),
            &|run| saved(&run.state, run.clip),
            &undo_once,
            &format!("clip, depth {depth}"),
        );
    }
}

/// The same on the timeline grade and a group's stages, through their
/// stand-ins' ids (#1476).
#[test]
fn a_sequence_grade_s_structure_edits_are_one_step_by_path_at_any_depth() {
    for depth in 0..3 {
        let (mut run, which) = sequence_grades(true);
        for w in which {
            let (grade, descent, ids) = at_depth(depth);
            let seq = &mut run.state.sequences[0];
            let root = w.root(seq);
            seq.set_root_grade(root, Some(grade)).unwrap();
            let stand_in = w.id(seq).unwrap();
            let d = descent.clone();
            check_structure_edits(
                &mut run,
                stand_in,
                &descent,
                &ids,
                &|run| {
                    let seq = &run.state.sequences[0];
                    crate::inspector::grade_view::v1_level(seq.root_grade(root).unwrap().unwrap(), &d).expect("the level").clone()
                },
                &|run| stored(run, w),
                &undo_root,
                &format!("{w:?}, depth {depth}"),
            );
        }
    }
}

/// Escape during the first drag takes the drag and the Corrector it made
/// back: the clip saved as it was, no step. A press and release that move
/// nothing leave nothing either.
#[test]
fn escape_or_a_still_click_on_the_first_edit_leaves_no_corrector() {
    use crate::state::ColorTab;
    for (dx, escape) in [(-60.0, true), (0.0, false)] {
        let mut run = without_corrector(false);
        let before = saved(&run.state, run.clip);
        let cmds = drag_row(&mut run, ColorTab::Curves, "Mix", dx, escape);
        // The gesture reached the row: a drag edited it, then Escape took it back.
        let set = cmds.iter().any(|c| matches!(c, AppCommand::SetParamAt { .. }));
        let abandoned = cmds.iter().any(|c| matches!(c, AppCommand::AbandonParamEditAt { .. }));
        assert_eq!((set, abandoned), (escape, escape), "dx {dx}, escape {escape}: {cmds:?}");
        assert_eq!(saved(&run.state, run.clip), before, "dx {dx}, escape {escape}: as it was");
        assert_eq!(run.state.active_history().undo_count(), 0, "no step");
        assert!(!run.state.active_history().is_any_drag_in_progress());
    }
}

/// An editor's `SetNode*` command as the colour dispatch writes it: the
/// node it names decided first, changing nothing, then made (when it must
/// be) in the same recorded graph edit as the write, one `SetNodeGraph`
/// step from the grade as it was. The node it wrote.
fn editor_write(run: &mut Run, named: NodeId, write: impl FnOnce(&mut Node)) -> NodeId {
    let before = saved(&run.state, run.clip);
    let target = run.state.reserved_target(run.clip, named);
    assert_eq!(saved(&run.state, run.clip), before, "deciding the node changes nothing");
    let written = super::colour::edit_clip_graph(&mut run.state, run.clip, &[], true, |g| {
        super::colour::make_reserved(g, target);
        if let Some(n) = g.nodes.iter_mut().find(|n| n.id == target.0) {
            write(n);
        }
    });
    assert_eq!(written, Some(Ok(())));
    target.0
}

/// An editor's own command on the reserved Corrector makes it under that
/// id too (beside a disabled one: IC16), so the rows and the editors
/// write one node.
#[test]
fn an_editor_command_on_the_reserved_corrector_makes_that_node() {
    for disabled in [false, true] {
        let mut run = without_corrector(disabled);
        tabbed_on(&mut run, false, crate::state::ColorTab::Wheels, None, Vec::new());
        let (_, reserved) = run.state.pending_corrector.unwrap();
        assert_eq!(editor_write(&mut run, reserved, |_| {}), reserved);
        assert_eq!(correctors(&run).iter().map(|n| n.id).collect::<Vec<_>>(), vec![reserved], "disabled {disabled}");
        assert_eq!(editor_write(&mut run, reserved, |_| {}), reserved, "a second write, the same node");
        assert_eq!(correctors(&run).len(), 1);
    }
}

/// A bare editor command (no Begin/End around it, as the Hue-Sat
/// eyedropper sends `SetNodeHueSatCurves`) on a Corrector that isn't made
/// yet is one step with the node it makes: undoing it leaves the clip
/// saved as it was.
#[test]
fn undoing_a_bare_editor_command_takes_back_the_corrector_it_made() {
    for disabled in [false, true] {
        let mut run = without_corrector(disabled);
        tabbed_on(&mut run, false, crate::state::ColorTab::HueSat, None, Vec::new());
        let named = run.state.pending_corrector.unwrap().1;
        let before = saved(&run.state, run.clip);
        let made = editor_write(&mut run, named, |n| { n.params.huesat = Some(Default::default()); n.enabled = true; });
        assert!(clip(&run.state, run.clip).grade().nodes.iter().any(|n| n.id == made), "made");
        assert_eq!(run.state.active_history().undo_count(), 1);
        let entry = run.state.active_history_mut().undo().unwrap();
        let crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, .. } = entry.command else { panic!("a graph step") };
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().set_grade(old_graph);
        assert_eq!(saved(&run.state, run.clip), before, "disabled {disabled}: as it was");
    }
}

/// A reservation is spent once its Corrector is made: if that node goes,
/// the next one the page reserves is a new id, never one a node had.
#[test]
fn a_spent_reservation_is_never_reused() {
    let mut run = without_corrector(false);
    let tab = crate::state::ColorTab::Wheels;
    tabbed_on(&mut run, false, tab, None, Vec::new());
    let (_, first) = run.state.pending_corrector.unwrap();
    editor_write(&mut run, first, |_| {});
    tabbed_on(&mut run, false, tab, None, Vec::new());
    assert_eq!(run.state.pending_corrector, None);
    let c = run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap();
    c.edit_grade(|g| g.nodes.iter_mut().find(|n| n.id == first).unwrap().enabled = false);
    tabbed_on(&mut run, false, tab, None, Vec::new());
    let (_, second) = run.state.pending_corrector.unwrap();
    assert_ne!(second, first);
}

/// On a clip with a Corrector, each tab is that Corrector's generated rows
/// in the tab's band, its editors registered: the same parameters the old
/// page drew there, written by path to the Corrector.
#[test]
fn a_corrector_s_tabs_are_its_generated_rows() {
    use crate::state::ColorTab;
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    for (tab, want) in [
        (ColorTab::Wheels, &["wheels_log_mode", "luma_mix"][..]),
        (ColorTab::Curves, &["curves_preserve_luma", "curves_mix"][..]),
        (ColorTab::HueSat, &["huesat_mix"][..]),
    ] {
        let (cmds, rows, _) = tabbed_on(&mut run, false, tab, None, Vec::new());
        assert!(cmds.is_empty(), "{tab:?} wrote on open");
        let drawn: Vec<_> = rows.iter().filter_map(|r| r.parameter.as_deref()).collect();
        for key in want {
            assert!(drawn.contains(key), "{tab:?} has no {key}: {drawn:?}");
        }
        assert!(!drawn.iter().any(|k| ["exposure", "temperature", "global_mix"].contains(k)), "{tab:?} drew another tab's rows: {drawn:?}");
    }
    let cmds = drag_row(&mut run, ColorTab::HueSat, "Mix", -60.0, false);
    assert!(cmds.iter().any(|c| matches!(c, AppCommand::SetParamAt { addr, key, .. } if addr.node() == Some(run.node) && key.as_str() == "huesat_mix")), "{cmds:?}");
    assert!(run.v1().params.huesat_mix.is_some_and(|m| m < 1.0));
    assert_eq!(run.state.active_history().undo_count(), 1);
}


use super::timeline_grade::GradeStandIn;

/// A run whose sequence has a grade group (`Night`), and the stand-ins of
/// its own grades: the timeline grade and that group's two stages. Each
/// with a stored grade (`stored`), or none yet.
fn sequence_grades(stored: bool) -> (Run, Vec<GradeStandIn>) {
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    let seq = &mut run.state.sequences[0];
    let mut group = klipp_timeline::GradeGroup::new("Night");
    if stored {
        group.stage_slot(klipp_timeline::GradeStage::Pre).set(Some(NodeGraph::default()));
        group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(NodeGraph::default()));
        seq.timeline_grade_slot().set(Some(NodeGraph::default()));
    }
    let g = group.id;
    seq.grade_groups.push(group);
    // A video track's own grade too (#1408).
    let track = seq.tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    if stored {
        track.effects.grade_slot().set(Some(NodeGraph::default()));
    }
    let t = track.id;
    (run, vec![GradeStandIn::Timeline, GradeStandIn::Group(g, klipp_timeline::GradeStage::Pre), GradeStandIn::Group(g, klipp_timeline::GradeStage::Post), GradeStandIn::Track(t)])
}

/// One frame of a sequence grade's tab `on`, as the app draws it (M3 step
/// 19): its stand-in put on, the snapshot taken at its root, the page
/// drawn over the stand-in's grade on the right or (`wide`) in the bottom
/// tools, then the stand-in taken down. Its commands are not applied.
fn stand_in_tab(run: &mut Run, which: GradeStandIn, wide: bool, on: crate::state::ColorTab, events: Vec<egui::Event>) -> Drawn {
    let id = super::timeline_grade::stand_in_grade_on(&mut run.state.sequences[0], which, &mut run.fresh).expect("stood in");
    let root = which.root(&run.state.sequences[0]);
    let mut panel = run.panel.take().unwrap_or_default();
    panel.generated = crate::inspector::grade_view::snapshot_for_root(&run.state.sequences[0], id, root, &[], &mut run.state.pending_corrector, &mut run.state.root_grade_start);
    let mut graph = clip(&run.state, id).grade().clone();
    let theme = crate::theme::KlippTheme::default();
    let frame = FrameSelection { time_secs: run.time, tolerance: 0.02, clip_start: clip(&run.state, id).timeline_in };
    run.clock += 0.05;
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 2400.0))), time: Some(run.clock), events, ..Default::default() };
    let mut cmds = Vec::new();
    let mut texts = Vec::new();
    let ((), rows) = crate::widgets::row_recorder::capture(|| {
        let out = run.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (mut sel, mut tab) = (None, on);
                cmds = if wide {
                    panel.show_simple_horizontal(ui, &mut graph, id, &mut sel, &mut tab, &theme, frame)
                } else {
                    panel.show_simple(ui, &mut graph, id, &mut sel, &mut tab, &theme, frame)
                };
            });
        });
        texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
    });
    panel.generated = None;
    run.panel = Some(panel);
    super::timeline_grade::stand_down_timeline_grade_on(&mut run.state.sequences[0], &mut run.fresh);
    (cmds, rows, texts)
}

/// The grade a stand-in stands for, as stored: its saved form, `None`
/// included.
fn stored(run: &Run, which: GradeStandIn) -> String {
    let seq = &run.state.sequences[0];
    serde_json::to_string(&seq.root_grade(which.root(seq)).unwrap()).unwrap()
}

/// The value a recorded row shows for `key`.
fn row_value(rows: &[crate::widgets::row_recorder::Row], key: &str) -> Option<f64> {
    rows.iter().find(|r| r.parameter.as_deref() == Some(key)).and_then(|r| r.value.as_f64())
}

/// Undo once, as the app does for a sequence grade's path edit.
fn undo_root(state: &mut AppState) {
    let entry = state.active_history_mut().undo().expect("a step to undo");
    let mut command = entry.command;
    if let klipp_document::history::EditCommand::Named { edit, .. } = command { command = *edit; }
    let klipp_document::history::EditCommand::SetRootGrade { root, old, .. } = command else { panic!("a sequence grade step: {command:?}") };
    state.sequences[0].set_root_grade(root, old.map(|g| *g)).unwrap();
}

/// The timeline grade and a group's pre and post stages draw their tabs
/// generated (M3 step 19): opening writes nothing, and a row's edit names
/// the node at the grade's own root, never the stand-in clip.
#[test]
fn a_sequence_grade_s_tabs_are_generated_at_its_root() {
    use crate::state::ColorTab;
    let (mut run, which) = sequence_grades(true);
    for w in which {
        let root = w.root(&run.state.sequences[0]);
        let before = stored(&run, w);
        for wide in [false, true] {
            let (cmds, rows, _) = stand_in_tab(&mut run, w, wide, ColorTab::Curves, Vec::new());
            assert!(cmds.is_empty(), "{w:?} wide {wide}: wrote on open: {cmds:?}");
            let mix = rows.iter().find(|r| r.parameter.as_deref() == Some("curves_mix")).unwrap_or_else(|| panic!("{w:?}: no Mix row"));
            assert!(mix.keyframe_state.is_some(), "{w:?} wide {wide}: Mix is the generated row");
        }
        assert_eq!(stored(&run, w), before, "{w:?}: opening wrote nothing");
        let cmds = drag_with(&mut run, &mut |run, events| stand_in_tab(run, w, false, ColorTab::Curves, events), "Mix", -60.0, false);
        let at: Vec<_> = cmds.iter().filter_map(|c| match c { AppCommand::SetParamAt { addr, .. } => Some(addr.root), _ => None }).collect();
        assert!(!at.is_empty() && at.iter().all(|r| *r == root), "{w:?}: edits at {root:?}: {at:?}");
    }
}

/// A path edit of a sequence grade, made outside the stand-in as the app
/// dispatches it, shows on the next draw of both the right inspector's
/// tabbed page and the bottom tools, as one step; undoing it puts the
/// stored grade back byte-exact and both surfaces show the value before.
#[test]
fn a_sequence_grade_s_edit_shows_on_both_surfaces_and_undoes_exactly() {
    use crate::state::ColorTab;
    let (mut run, which) = sequence_grades(true);
    for w in which {
        let steps = run.state.active_history().undo_count();
        let before = stored(&run, w);
        let mix = |run: &mut Run, wide| row_value(&stand_in_tab(run, w, wide, ColorTab::Curves, Vec::new()).1, "curves_mix");
        let was = (mix(&mut run, false), mix(&mut run, true));
        drag_with(&mut run, &mut |run, events| stand_in_tab(run, w, false, ColorTab::Curves, events), "Mix", -60.0, false);
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "{w:?}: one step");
        let seq = &run.state.sequences[0];
        let now = seq.root_grade(w.root(seq)).unwrap().unwrap().nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().params.curves_mix;
        assert!(now.is_some_and(|m| m < 1.0), "{w:?}: stored {now:?}");
        let now = now.map(f64::from);
        assert_eq!((mix(&mut run, false), mix(&mut run, true)), (now, now), "{w:?}: both surfaces show the edit");
        undo_root(&mut run.state);
        assert_eq!(stored(&run, w), before, "{w:?}: undo, byte-exact");
        assert_eq!((mix(&mut run, false), mix(&mut run, true)), was, "{w:?}: both surfaces show the value before");
    }
}

/// A sequence with no timeline grade, and a group with no stages, draw the
/// graph their stand-in is put on with; the first edit begins the grade
/// from it, with the ids drawn, and undoing it leaves no grade at all.
#[test]
fn a_first_edit_begins_a_sequence_grade_and_undo_takes_it_off() {
    use crate::state::ColorTab;
    let (mut run, which) = sequence_grades(false);
    for w in which {
        assert_eq!(stored(&run, w), "null", "{w:?}: no grade yet");
        let (cmds, _, _) = stand_in_tab(&mut run, w, false, ColorTab::Curves, Vec::new());
        assert!(cmds.is_empty(), "{w:?}: wrote on open");
        let drawn = run.state.root_grade_start.clone().expect("the graph drawn kept").1;
        drag_with(&mut run, &mut |run, events| stand_in_tab(run, w, false, ColorTab::Curves, events), "Mix", -60.0, false);
        let seq = &run.state.sequences[0];
        let made = seq.root_grade(w.root(seq)).unwrap().cloned().expect("the grade begun");
        assert_eq!(made.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), drawn.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), "{w:?}: the ids drawn");
        undo_root(&mut run.state);
        assert_eq!(stored(&run, w), "null", "{w:?}: undo leaves no grade");
    }
}

/// Escape during a drag on a sequence grade takes it back: the grade as
/// stored before, no step.
#[test]
fn escape_on_a_sequence_grade_s_drag_takes_it_back() {
    use crate::state::ColorTab;
    let (mut run, which) = sequence_grades(true);
    for w in which {
        let (before, steps) = (stored(&run, w), run.state.active_history().undo_count());
        let cmds = drag_with(&mut run, &mut |run, events| stand_in_tab(run, w, false, ColorTab::Curves, events), "Mix", -60.0, true);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::AbandonParamEditAt { .. })), "{w:?}: escaped");
        assert_eq!((stored(&run, w), run.state.active_history().undo_count()), (before, steps), "{w:?}");
    }
}

/// A correction's mix and mask channel on the timeline grade and on a
/// group's stages land on the node as its run setting, one step each,
/// and undo takes them off (#1321); a track's grade refuses them (#1408).
#[test]
fn a_mix_and_channel_edit_on_a_sequence_grade_lands() {
    use klipp_effects::mask_mix::MaskChannel;
    let (mut run, which) = sequence_grades(true);
    for w in which {
        let seq = &run.state.sequences[0];
        let root = w.root(seq);
        let node = seq.root_grade(root).unwrap().unwrap().nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
        let blend = |run: &Run| run.state.sequences[0].root_grade(root).unwrap().unwrap().nodes.iter().find(|n| n.id == node).unwrap().params.correction_blend;
        let addr = klipp_graph_format::path::NodePath::new(root, [node]);
        // A track's grade has no stored form for them: refused, nothing
        // changed, no step (#1408).
        if matches!(w, GradeStandIn::Track(_)) {
            let steps = run.state.active_history().undo_count();
            assert_eq!(run.state.path_command(&AppCommand::SetMixAt { addr: addr.clone(), mix: Some(0.5) }), Some(false), "{w:?}");
            assert_eq!(run.state.path_command(&AppCommand::SetMaskChannelAt { addr, channel: Some("alpha".into()) }), Some(false), "{w:?}");
            assert_eq!((blend(&run), run.state.active_history().undo_count()), (None, steps), "{w:?}: nothing changed");
            continue;
        }
        assert_eq!(run.state.path_command(&AppCommand::SetMixAt { addr: addr.clone(), mix: Some(0.5) }), Some(true), "{w:?}: {:?}", run.state.toast.message_for_test());
        assert_eq!(run.state.path_command(&AppCommand::SetMaskChannelAt { addr, channel: Some("alpha".into()) }), Some(true), "{w:?}");
        assert_eq!(blend(&run), Some(klipp_nodes::params::EffectBlend { mix: 0.5, channel: MaskChannel::Alpha }), "{w:?}");
        undo_root(&mut run.state);
        assert_eq!(blend(&run), Some(klipp_nodes::params::EffectBlend { mix: 0.5, channel: MaskChannel::Red }), "{w:?}: undo takes the channel off");
        undo_root(&mut run.state);
        assert_eq!(blend(&run), None, "{w:?}: and then the mix");
    }
}

/// A grade node's own mix on its page (#1348): its "Node mix" row,
/// dragged, is the node's mix by path, one step, and undo takes it back.
#[test]
fn a_grade_node_s_mix_drags_by_path_in_one_step() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let before = saved(&run.state, run.clip);
    let cmds = drag_with(&mut run, &mut |run, events| { let (cmds, _, texts) = run.frame(events); (cmds, Vec::new(), texts) }, "Node mix", -60.0, false);
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    assert!(cmds.iter().any(|c| matches!(c, AppCommand::SetMixAt { addr: a, mix: Some(m) } if *a == addr && *m < 1.0)), "{cmds:?}");
    let mix = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap().1.mix;
    assert!(mix.is_some_and(|m| m < 1.0), "the node keeps less of its change: {mix:?}");
    assert_eq!(run.state.active_history().undo_count(), 1, "one step");
    undo_once(&mut run.state);
    assert_eq!(saved(&run.state, run.clip), before, "undo takes it back");
}

/// Redo once, as the app does for a sequence grade's path edit.
fn redo_root(state: &mut AppState) {
    let entry = state.active_history_mut().redo().expect("a step to redo");
    let mut command = entry.command;
    if let klipp_document::history::EditCommand::Named { edit, .. } = command { command = *edit; }
    let klipp_document::history::EditCommand::SetRootGrade { root, new, .. } = command else { panic!("a sequence grade step: {command:?}") };
    state.sequences[0].set_root_grade(root, new.map(|g| *g)).unwrap();
}

/// A drag that closes (not Escape) on a sequence grade that has none yet
/// is one step: undo takes the grade off again, and redo brings back the
/// grade with the dragged value.
#[test]
fn a_closed_drag_on_a_sequence_grade_without_one_is_one_step() {
    use crate::state::ColorTab;
    let (mut run, which) = sequence_grades(false);
    for w in which {
        let steps = run.state.active_history().undo_count();
        let cmds = drag_with(&mut run, &mut |run, events| stand_in_tab(run, w, false, ColorTab::Curves, events), "Mix", -60.0, false);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::EndParamEditAt { .. })), "{w:?}: the drag closed");
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "{w:?}: one step");
        let dragged = stored(&run, w);
        let seq = &run.state.sequences[0];
        let mix = seq.root_grade(w.root(seq)).unwrap().unwrap().nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().params.curves_mix;
        assert!(mix.is_some_and(|m| m < 1.0), "{w:?}: the dragged value, {mix:?}");
        undo_root(&mut run.state);
        assert_eq!(stored(&run, w), "null", "{w:?}: undo takes the grade off");
        redo_root(&mut run.state);
        assert_eq!(stored(&run, w), dragged, "{w:?}: redo brings back the dragged grade");
    }
}

/// Each structure command the grade canvas emits inside a compound, drawn
/// and clicked on the canvas and fed through the app's dispatch
/// (`canvas_structure_edit`, as the colour page's handler feeds it), lands
/// as its path-op step (#1476): Add from the add menu, Delete from a
/// node's menu, Group 2 into Compound, and Ungroup Compound.
#[test]
fn the_canvas_s_structure_commands_land_as_path_steps() {
    use crate::panels::node_graph_v2::canvas_command_tests::Harness;
    let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
    let (grade, descent, ids) = at_depth(1);
    run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap().edit_grade(|g| *g = grade);
    run.state.graph_descent_path = descent.clone();
    let clip_id = run.clip;
    // The canvas on the compound's level as it stands, its nodes spread out.
    let canvas = |run: &Run| {
        let mut level = level_of(clip(&run.state, clip_id).grade(), &descent).expect("the level");
        for (i, n) in level.nodes.iter().enumerate() {
            level.node_positions.insert(n.id, klipp_nodes::NodePos { x: (i as f32 - 2.0) * 230.0, y: 0.0 });
        }
        let mut h = Harness::on(level, clip_id);
        h.settle();
        h
    };
    let dispatch = |run: &mut Run, cmds: Vec<AppCommand>, label: &str| {
        let steps = run.state.active_history().undo_count();
        let [cmd] = cmds.as_slice() else { panic!("{label}: the canvas sent {cmds:?}") };
        assert_eq!(run.state.canvas_structure_edit(cmd), Some(true), "{label}: {cmd:?}: {:?}", run.state.toast.message_for_test());
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "{label}: one step");
        assert_eq!(run.state.active_history().undo_label(), Some(label));
    };
    let level = |run: &Run| level_of(clip(&run.state, clip_id).grade(), &descent).expect("the level");

    // Add, from the add menu.
    let mut h = canvas(&run);
    h.open_add_menu("Saturation", egui::pos2(42.0, 64.0));
    let cmds = h.click(h.text("Saturation"), egui::PointerButton::Primary);
    dispatch(&mut run, cmds, "Add Node");
    assert_eq!(level(&run).nodes.iter().filter(|n| n.kind == NodeKind::Saturation).count(), 2, "added in the compound");

    // Delete, from the Contrast's menu.
    let mut h = canvas(&run);
    let (_, rect) = h.card(NodeKind::Contrast);
    let cmds = h.menu_item(rect.center(), "Delete");
    dispatch(&mut run, cmds, "Remove Node");
    assert!(!level(&run).nodes.iter().any(|n| n.id == ids[2]), "removed from the compound");

    // Group the Exposure and the first Saturation.
    let mut h = canvas(&run);
    h.set = [ids[0], ids[1]].into_iter().collect();
    h.selected = Some(ids[0]);
    h.settle();
    let (_, rect) = h.card(NodeKind::Exposure);
    let cmds = h.menu_item(rect.center(), "Group 2 into Compound");
    dispatch(&mut run, cmds, "Group into Compound");
    let group = run.state.selected_node_id.expect("the new compound selected");
    assert!(level(&run).nodes.iter().any(|n| n.id == group && n.kind == NodeKind::Compound), "grouped in the compound");

    // Ungroup it again.
    let mut h = canvas(&run);
    let (_, rect) = h.card(NodeKind::Compound);
    let cmds = h.menu_item(rect.center(), "Ungroup Compound");
    dispatch(&mut run, cmds, "Ungroup Compound");
    assert!(!level(&run).nodes.iter().any(|n| n.id == group) && level(&run).nodes.iter().any(|n| n.id == ids[0]), "ungrouped back");
}

/// A node dragged on the grade canvas, at the grade's top or inside a
/// compound one or two deep, on a clip's grade and each sequence grade
/// (through its stand-in's id), is placed by path: the drag's commands,
/// fed through the app's dispatch, move the stored node by the drag in one
/// undo step, and undo puts it back (#1528).
#[test]
fn a_canvas_node_drag_is_one_step_by_path_at_any_depth() {
    use crate::panels::node_graph_v2::canvas_command_tests::Harness;
    // Drag the Saturation's card by (60, 40) over three frames, feeding
    // each frame's commands on as the colour page does.
    let drag = |run: &mut Run, h: &mut Harness| {
        let (_, rect) = h.card(NodeKind::Saturation);
        let at = rect.center();
        let mut sent = h.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        for step in [egui::vec2(30.0, 20.0), egui::vec2(60.0, 40.0)] {
            sent.extend(h.frame(vec![egui::Event::PointerMoved(at + step)]));
        }
        sent.extend(h.frame(vec![egui::Event::PointerButton { pos: at + egui::vec2(60.0, 40.0), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]));
        assert!(sent.iter().any(|c| matches!(c, AppCommand::MoveNode { .. })), "the canvas sent no move: {sent:?}");
        for cmd in &sent {
            if run.state.canvas_move(cmd).is_none() {
                run.state.path_command(cmd);
            }
        }
    };
    let spread = |mut level: NodeGraph| {
        for (i, n) in level.nodes.iter().enumerate() {
            level.node_positions.insert(n.id, klipp_nodes::NodePos { x: (i as f32 - 2.0) * 230.0, y: 0.0 });
        }
        level
    };
    for depth in 0..3 {
        // A clip's grade.
        let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
        let (grade, descent, ids) = at_depth(depth);
        let clip_id = run.clip;
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| {
            *g = grade;
            let level = spread(level_of(g, &descent).unwrap());
            *super::KlippApp::resolve_graph_at_path_mut(g, &descent).unwrap() = level;
        });
        run.state.graph_descent_path = descent.clone();
        let at = |run: &Run| level_of(clip(&run.state, clip_id).grade(), &descent).unwrap().node_positions[&ids[1]];
        let (before, saved_before) = (at(&run), saved(&run.state, clip_id));
        let mut h = Harness::on(level_of(clip(&run.state, clip_id).grade(), &descent).unwrap(), clip_id);
        h.settle();
        drag(&mut run, &mut h);
        let after = at(&run);
        assert!((after.x - before.x - 60.0).abs() < 0.5 && (after.y - before.y - 40.0).abs() < 0.5, "clip depth {depth}: moved by the drag: {before:?} → {after:?}");
        assert_eq!(run.state.active_history().undo_count(), 1, "clip depth {depth}: one step");
        undo_once(&mut run.state);
        assert_eq!(saved(&run.state, clip_id), saved_before, "clip depth {depth}: undo puts it back");

        // Each sequence grade, through its stand-in's id.
        let (mut run, which) = sequence_grades(true);
        for w in which {
            let (grade, descent, ids) = at_depth(depth);
            let seq = &mut run.state.sequences[0];
            let root = w.root(seq);
            let mut grade = grade;
            let level = spread(level_of(&grade, &descent).unwrap());
            *crate::app::KlippApp::resolve_graph_at_path_mut(&mut grade, &descent).unwrap() = level;
            seq.set_root_grade(root, Some(grade)).unwrap();
            let stand_in = w.id(seq).unwrap();
            run.state.graph_descent_path = descent.clone();
            let at = |run: &Run| level_of(run.state.sequences[0].root_grade(root).unwrap().unwrap(), &descent).unwrap().node_positions[&ids[1]];
            let (before, stored_before, steps) = (at(&run), stored(&run, w), run.state.active_history().undo_count());
            let mut h = Harness::on(level_of(run.state.sequences[0].root_grade(root).unwrap().unwrap(), &descent).unwrap(), stand_in);
            h.settle();
            drag(&mut run, &mut h);
            let after = at(&run);
            assert!((after.x - before.x - 60.0).abs() < 0.5 && (after.y - before.y - 40.0).abs() < 0.5, "{w:?} depth {depth}: moved by the drag: {before:?} → {after:?}");
            assert_eq!(run.state.active_history().undo_count(), steps + 1, "{w:?} depth {depth}: one step");
            undo_root(&mut run.state);
            assert_eq!(stored(&run, w), stored_before, "{w:?} depth {depth}: undo puts it back");
        }
    }
}

/// Auto Layout on the grade canvas, at the grade's top or inside a compound
/// one or two deep, on a clip's grade and each sequence grade (through its
/// stand-in's id): fed through the app's dispatch, the level shown takes
/// the layout's positions in one step, and undo puts them back (#1528).
#[test]
fn auto_layout_is_one_step_by_path_at_any_depth() {
    let positions = |g: &NodeGraph| {
        let mut p: Vec<_> = g.node_positions.iter().map(|(id, p)| (*id, (p.x, p.y))).collect();
        p.sort_by_key(|(id, _)| *id);
        p
    };
    for depth in 0..3 {
        // A clip's grade.
        let mut run = Run::with_panel(NodeKind::Corrector).unwrap();
        let (grade, descent, _) = at_depth(depth);
        let clip_id = run.clip;
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| *g = grade);
        run.state.graph_descent_path = descent.clone();
        let level = |run: &Run| level_of(clip(&run.state, clip_id).grade(), &descent).unwrap();
        let want = { let mut l = level(&run); l.node_positions = klipp_nodes::auto_layout(&l); positions(&l) };
        let saved_before = saved(&run.state, clip_id);
        assert_eq!(run.state.canvas_auto_layout(&AppCommand::AutoLayoutNodeGraph { clip_id }), Some(true), "clip depth {depth}: {:?}", run.state.toast.message_for_test());
        assert_eq!(positions(&level(&run)), want, "clip depth {depth}: laid out");
        assert_eq!(run.state.active_history().undo_count(), 1, "clip depth {depth}: one step");
        undo_once(&mut run.state);
        assert_eq!(saved(&run.state, clip_id), saved_before, "clip depth {depth}: undo puts it back");

        // Each sequence grade.
        let (mut run, which) = sequence_grades(true);
        for w in which {
            let (grade, descent, _) = at_depth(depth);
            let seq = &mut run.state.sequences[0];
            let root = w.root(seq);
            seq.set_root_grade(root, Some(grade)).unwrap();
            let stand_in = w.id(seq).unwrap();
            run.state.graph_descent_path = descent.clone();
            let level = |run: &Run| level_of(run.state.sequences[0].root_grade(root).unwrap().unwrap(), &descent).unwrap();
            let want = { let mut l = level(&run); l.node_positions = klipp_nodes::auto_layout(&l); positions(&l) };
            let (stored_before, steps) = (stored(&run, w), run.state.active_history().undo_count());
            assert_eq!(run.state.canvas_auto_layout(&AppCommand::AutoLayoutNodeGraph { clip_id: stand_in }), Some(true), "{w:?} depth {depth}: {:?}", run.state.toast.message_for_test());
            assert_eq!(positions(&level(&run)), want, "{w:?} depth {depth}: laid out");
            assert_eq!(run.state.active_history().undo_count(), steps + 1, "{w:?} depth {depth}: one step");
            undo_root(&mut run.state);
            assert_eq!(stored(&run, w), stored_before, "{w:?} depth {depth}: undo puts it back");
        }
    }
}

/// A grade node something masks has a Mask channel row on its page, and
/// one nothing masks doesn't (#1348): the window wired into a Corrector's
/// mask port; the channel picked is the node's by path, one step, and
/// undo takes it back.
#[test]
fn a_masked_grade_node_s_page_picks_its_mask_channel() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let draws = |run: &mut Run| {
        run.frame(Vec::new());
        run.texts().iter().any(|t| t == "Mask channel")
    };
    assert!(!draws(&mut run), "no mask, no row");
    // A window, from the source, into the Corrector's mask port.
    let corrector = run.node;
    let clip_id = run.clip;
    run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| {
        let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let window = Node { id: NodeId::new_v4(), kind: NodeKind::Mask, label: "Window".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: window.id, to_port: 0 });
        g.edges.push(klipp_nodes::Edge { from_node: window.id, from_port: 0, to_node: corrector, to_port: 1 });
        g.nodes.push(window);
    });
    assert!(draws(&mut run), "a masked Corrector has the row: {:?}", run.texts());
    // The channel shown, right of the label, opened; Alpha picked.
    let texts = run.frame(Vec::new()).2;
    let label = texts.iter().find(|(t, _)| t == "Mask channel").unwrap().1;
    let shown = texts.iter().filter(|(_, p)| (p.y - label.y).abs() < 6.0 && p.x > label.x).min_by(|a, b| a.1.x.total_cmp(&b.1.x)).unwrap_or_else(|| panic!("the channel shown: {texts:?}")).1;
    run.click(shown);
    let alpha = run.frame(Vec::new()).2.into_iter().find(|(t, _)| t == "Alpha").unwrap_or_else(|| panic!("no Alpha choice")).1;
    let cmds = run.click(alpha);
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, clip_id), corrector).unwrap();
    assert!(matches!(cmds.as_slice(), [AppCommand::SetMaskChannelAt { addr: a, channel: Some(c) }] if *a == addr && c == "alpha"), "{cmds:?}");
    let before = saved(&run.state, clip_id);
    let steps = run.state.active_history().undo_count();
    run.apply(&cmds);
    let channel = |run: &Run| crate::inspector::grade_view::grade_node(clip(&run.state, clip_id), corrector).unwrap().1.params.get(&klipp_graph_format::ParamKey::from("mask.channel")).cloned();
    assert_eq!(channel(&run), Some(klipp_graph_format::ParamValue::Text("alpha".into())));
    assert_eq!(run.state.active_history().undo_count(), steps + 1, "one step");
    undo_once(&mut run.state);
    assert_eq!(saved(&run.state, clip_id), before, "undo takes it back");
}

/// Printer lights inside a compound with no Corrector, one or two deep,
/// make its Corrector there, by path, holding the offset, in one undo step
/// that undo takes back; none is made at the top. A second press moves
/// that Corrector's offset on (#1527).
#[test]
fn printer_lights_inside_a_compound_make_its_corrector_there() {
    for deep in [false, true] {
        let mut run = Run::new(NodeKind::Corrector).unwrap();
        let (grade, descent) = compounded(deep);
        let top = grade.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(run.clip)).unwrap().edit_grade(|g| *g = grade);
        run.state.selected_clip = Some(run.clip);
        run.state.graph_descent_path = descent.clone();
        let (before, steps) = (saved(&run.state, run.clip), run.state.active_history().undo_count());
        let step = 0.0125;
        let target = super::colour::printer_light_target(&mut run.state).expect("a target");
        let reserved = run.state.pending_corrector.expect("a Corrector reserved in the compound").1;
        assert_eq!(target.0.nodes.last(), Some(&reserved), "deep {deep}: the reserved Corrector");
        let points = super::colour::apply_printer_light(&mut run.state, target, 0, 2, step).unwrap();
        assert_eq!(points[0], 2, "deep {deep}: two red points");
        let made = correctors_at(clip(&run.state, run.clip).grade(), &descent);
        assert_eq!(made.iter().map(|n| n.id).collect::<Vec<_>>(), vec![reserved], "deep {deep}: made in the compound");
        assert!(made[0].params.offset_x.is_some_and(|x| x != 0.0), "deep {deep}: holding the offset");
        assert_eq!(correctors_at(clip(&run.state, run.clip).grade(), &[]).iter().map(|n| n.id).collect::<Vec<_>>(), vec![top], "deep {deep}: none made at the top");
        assert_eq!(run.state.active_history().undo_count(), steps + 1, "deep {deep}: one step");
        // Again: the Corrector made now takes it.
        let target = super::colour::printer_light_target(&mut run.state).expect("a target");
        assert_eq!(target.0.nodes.last(), Some(&reserved), "deep {deep}: the same node");
        assert_eq!(super::colour::apply_printer_light(&mut run.state, target, 0, 1, step).unwrap()[0], 3, "deep {deep}: moved on");
        assert_eq!(run.state.active_history().undo_count(), steps + 2);
        undo_once(&mut run.state);
        undo_once(&mut run.state);
        assert_eq!(saved(&run.state, run.clip), before, "deep {deep}: undo, the clip as it was");
    }
}

/// One key pressed with `mods` held, run through `f` inside an egui frame.
fn with_key<R>(ctx: &egui::Context, key: egui::Key, mods: egui::Modifiers, f: impl FnOnce(&egui::Context) -> R) -> R {
    let event = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods };
    let (mut f, mut out) = (Some(f), None);
    let _ = ctx.run(egui::RawInput { events: vec![event], modifiers: mods, ..Default::default() }, |ctx| out = f.take().map(|f| f(ctx)));
    out.unwrap()
}

/// With the grade canvas focused, each of its keys does only its own
/// action (#1742): the app's shortcuts run only the Ctrl combinations
/// and the grade's keys, which take none of these; the transport's J, K
/// and L stand aside; and the canvas does its thing (Delete takes the
/// node, L lays the graph out). Delete used to remove the clip too.
#[test]
fn a_focused_grade_canvas_keeps_its_keys_from_the_timeline() {
    use super::shortcuts::{grade_node_keys, key_scope, KeyScope};
    use crate::panels::node_graph_v2::canvas_command_tests::Harness;
    use crate::state::PanelKind;
    let (mut state, clip_id, node) = graded(NodeKind::Saturation).unwrap();
    state.selected_clip = Some(clip_id);
    state.playback = klipp_engine::PlaybackState::Paused;
    // A project is open: with none, only the welcome screen's keys run.
    state.project_path = Some(crate::project_paths::UNTITLED.into());

    // Which shortcuts run, by the focus.
    for (panel, editing, scope) in [
        (Some(PanelKind::NodeGraph), false, KeyScope::GradeCanvas),
        (Some(PanelKind::PictureGraph), false, KeyScope::CtrlOnly),
        (Some(PanelKind::Timeline), false, KeyScope::All),
        (None, false, KeyScope::All),
        (Some(PanelKind::NodeGraph), true, KeyScope::CtrlOnly),
    ] {
        state.active_panel = panel;
        assert_eq!(key_scope(&state, editing), scope, "{panel:?}, typing {editing}");
    }
    state.active_panel = Some(PanelKind::NodeGraph);

    let mut grade = clip(&state, clip_id).grade().clone();
    for (i, n) in grade.nodes.iter().enumerate() {
        grade.node_positions.insert(n.id, klipp_nodes::NodePos { x: (i as f32 - 1.0) * 230.0, y: 0.0 });
    }
    let mut h = Harness::on(grade, clip_id);
    h.selected = Some(node);
    h.set = [node].into_iter().collect();
    h.settle();
    let kb = crate::keybindings::KeyBindings::defaults();
    let ctx = egui::Context::default();
    let mut transport = crate::panels::transport::TransportBar::default();
    for key in [egui::Key::Delete, egui::Key::Backspace, egui::Key::L, egui::Key::M, egui::Key::F, egui::Key::Num1, egui::Key::J, egui::Key::K] {
        let event = egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        h.selected = Some(node);
        h.set = [node].into_iter().collect();
        // What the canvas did, its thumbnail requests aside.
        let canvas: Vec<AppCommand> = h.frame(vec![event]).into_iter().filter(|c| !matches!(c, AppCommand::SetVisibleNodeThumbs { .. })).collect();
        let (grade_keys, transported) = with_key(&ctx, key, egui::Modifiers::NONE, |ctx| {
            let mut cmds = Vec::new();
            ctx.input(|i| grade_node_keys(&mut state, &kb, i, &mut cmds));
            (cmds, transport.handle_keyboard(ctx, &state, &kb))
        });
        assert!(grade_keys.is_empty() && transported.is_empty(), "{key:?} reached the app: {grade_keys:?} {transported:?}");
        match key {
            egui::Key::Delete => assert!(matches!(canvas.as_slice(), [AppCommand::RemoveNode { node_id, .. }] if *node_id == node), "{canvas:?}"),
            egui::Key::L => assert!(matches!(canvas.as_slice(), [AppCommand::AutoLayoutNodeGraph { .. }]), "{canvas:?}"),
            // 1 views the selected node in the program viewer (#1763).
            egui::Key::Num1 => assert!(matches!(canvas.as_slice(), [AppCommand::SetNodeSolo { at: Some(at) }] if at.path.node() == Some(node)), "{canvas:?}"),
            _ => assert!(!canvas.iter().any(|c| matches!(c, AppCommand::RemoveClip { .. })), "{key:?}: {canvas:?}"),
        }
    }

    // Controls: unfocused, the canvas takes no Delete; and with the
    // timeline focused the transport's L plays.
    let mut h = Harness::on(clip(&state, clip_id).grade().clone(), clip_id);
    h.panel.grade_keys = false;
    h.selected = Some(node);
    h.set = [node].into_iter().collect();
    h.settle();
    let delete = egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    assert!(!h.frame(vec![delete]).iter().any(|c| matches!(c, AppCommand::RemoveNode { .. })), "an unfocused canvas leaves Delete alone");
    state.active_panel = Some(PanelKind::Timeline);
    let played = with_key(&egui::Context::default(), egui::Key::L, egui::Modifiers::NONE, |ctx| crate::panels::transport::TransportBar::default().handle_keyboard(ctx, &state, &kb));
    assert!(played.iter().any(|c| matches!(c, AppCommand::Play)), "{played:?}");
}

/// The grade's keys act on the level the canvas shows (#1742): `[` and
/// `]` step through a compound's nodes, not the grade's top; Alt+D and
/// Ctrl+Shift+R bypass and reset a node inside it, each one undo step as
/// the app applies it; and a reset keeps what a node is (a compound its
/// nodes).
#[test]
fn the_grade_keys_act_on_the_open_level() {
    use super::colour::{edit_clip_graph, graph_path_for, write_node_settings};
    use super::shortcuts::grade_node_keys;
    let (mut state, clip_id, _) = graded(NodeKind::Corrector).unwrap();
    let (grade, descent, ids) = at_depth(1);
    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| *g = grade);
    state.graph_descent_path = descent.clone();
    state.selected_clip = Some(clip_id);
    state.active_panel = Some(crate::state::PanelKind::NodeGraph);
    state.selected_node_id = Some(ids[0]);
    let kb = crate::keybindings::KeyBindings::defaults();
    let ctx = egui::Context::default();
    let press = |state: &mut AppState, key, mods| {
        with_key(&ctx, key, mods, |ctx| {
            let mut cmds = Vec::new();
            ctx.input(|i| grade_node_keys(state, &kb, i, &mut cmds));
            cmds
        })
    };
    assert!(press(&mut state, egui::Key::CloseBracket, egui::Modifiers::NONE).is_empty());
    assert_eq!(state.selected_node_id, Some(ids[1]), "] steps to the compound's next node");
    press(&mut state, egui::Key::OpenBracket, egui::Modifiers::NONE);
    press(&mut state, egui::Key::OpenBracket, egui::Modifiers::NONE);
    assert_eq!(state.selected_node_id, Some(ids[2]), "[ wraps round the compound's nodes");
    assert_eq!(state.selected_node_ids.len(), 1);

    // The app's handling of the commands: the level holding the node,
    // edited and recorded.
    let apply = |state: &mut AppState, cmd: &AppCommand| {
        let (AppCommand::ToggleNodeEnabled { node_id, .. } | AppCommand::ResetNode { node_id, .. }) = cmd else { panic!("{cmd:?}") };
        let path = graph_path_for(clip(state, clip_id).grade(), &[*node_id], &state.graph_descent_path).expect("its level");
        assert_eq!(edit_clip_graph(state, clip_id, &path, true, |g| { write_node_settings(g, cmd); }), Some(Ok(())));
    };
    let inner = |state: &AppState, id: NodeId| level_of(clip(state, clip_id).grade(), &descent).unwrap().nodes.into_iter().find(|n| n.id == id).unwrap();

    let alt = egui::Modifiers { alt: true, ..Default::default() };
    let cmds = press(&mut state, egui::Key::D, alt);
    assert!(matches!(cmds.as_slice(), [AppCommand::ToggleNodeEnabled { node_id, .. }] if *node_id == ids[2]), "{cmds:?}");
    let steps = state.active_history().undo_count();
    apply(&mut state, &cmds[0]);
    assert!(!inner(&state, ids[2]).enabled, "bypassed in the compound");
    assert!(clip(&state, clip_id).grade().nodes.iter().all(|n| n.enabled), "the top untouched");
    assert_eq!(state.active_history().undo_count(), steps + 1, "one step");

    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| {
        let level = super::KlippApp::resolve_graph_at_path_mut(g, &descent).unwrap();
        level.nodes.iter_mut().find(|n| n.id == ids[2]).unwrap().params.contrast = Some(1.4);
    });
    let reset = egui::Modifiers { ctrl: true, shift: true, command: true, ..Default::default() };
    let cmds = press(&mut state, egui::Key::R, reset);
    assert!(matches!(cmds.as_slice(), [AppCommand::ResetNode { node_id, .. }] if *node_id == ids[2]), "{cmds:?}");
    apply(&mut state, &cmds[0]);
    assert_eq!(inner(&state, ids[2]).params.contrast, None, "reset in the compound");
    assert_eq!(state.active_history().undo_count(), steps + 2, "one step");

    // A compound reset keeps its nodes.
    state.graph_descent_path.clear();
    state.selected_node_id = Some(descent[0]);
    let cmds = press(&mut state, egui::Key::R, reset);
    apply(&mut state, &cmds[0]);
    assert!(clip(&state, clip_id).grade().nodes.iter().find(|n| n.id == descent[0]).unwrap().params.compound_subgraph.is_some(), "the compound keeps its graph");
}

/// The grade canvas's commands applied as the colour page's dispatch
/// applies them (#1745): the node editor's edits, node and note moves in
/// the layout gesture, and the gesture itself.
fn dispatch_canvas(state: &mut AppState, cmds: &[AppCommand]) {
    for cmd in cmds {
        state.before_dispatch(cmd);
        if state.node_edit(cmd).is_some() || state.canvas_move(cmd).is_some() {
            continue;
        }
        if let AppCommand::MoveStickyNote { clip_id, sticky_id, delta_x, delta_y } = cmd {
            let record = !state.in_layout_gesture(*clip_id);
            let path = state.graph_descent_path.clone();
            super::colour::move_sticky_note(state, *clip_id, &path, *sticky_id, (*delta_x, *delta_y), record);
            continue;
        }
        state.path_command(cmd);
    }
}

/// A grade at depth 1 on a clip, the canvas on that level with its nodes
/// spread out (and `place` moving them), and the three nodes.
fn canvas_at_depth(place: impl Fn(usize) -> (f32, f32)) -> (AppState, ClipId, Vec<NodeId>, crate::panels::node_graph_v2::canvas_command_tests::Harness) {
    let (mut state, clip_id, _) = graded(NodeKind::Corrector).unwrap();
    let (grade, descent, ids) = at_depth(1);
    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| *g = grade);
    state.graph_descent_path = descent.clone();
    state.selected_clip = Some(clip_id);
    let mut level = level_of(clip(&state, clip_id).grade(), &descent).unwrap();
    for (i, n) in level.nodes.iter().enumerate() {
        let (x, y) = place(i);
        level.node_positions.insert(n.id, klipp_nodes::NodePos { x, y });
    }
    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| {
        *super::KlippApp::resolve_graph_at_path_mut(g, &descent).unwrap() = level.clone();
    });
    let h = crate::panels::node_graph_v2::canvas_command_tests::Harness::on(level, clip_id);
    (state, clip_id, ids, h)
}

/// Ctrl+D, Ctrl+C and Ctrl+V on the grade canvas, applied as the app
/// applies them (#1745): inside a compound, a duplicate and a paste land
/// in the compound, never at the grade's top, each one undo step, the
/// new nodes selected; the paste's first node where it was asked for.
#[test]
fn grade_canvas_duplicates_copies_and_pastes_at_the_open_level() {
    let (mut state, clip_id, ids, mut h) = canvas_at_depth(|i| ((i as f32 - 2.0) * 230.0, 0.0));
    let level = |state: &AppState| level_of(clip(state, clip_id).grade(), &state.graph_descent_path.clone()).unwrap();
    let top = clip(&state, clip_id).grade().nodes.len();
    let before = level(&state).nodes.len();
    h.set = [ids[0], ids[1]].into_iter().collect();
    h.selected = Some(ids[1]);
    h.settle();
    h.mods = egui::Modifiers::COMMAND;
    let sent = h.key(egui::Key::D);
    let steps = state.active_history().undo_count();
    dispatch_canvas(&mut state, &sent);
    assert_eq!(level(&state).nodes.len(), before + 2, "two copies in the compound: {sent:?}");
    assert_eq!(clip(&state, clip_id).grade().nodes.len(), top, "the top untouched");
    assert_eq!(state.active_history().undo_count(), steps + 1, "one step");
    assert_eq!(state.selected_node_ids.len(), 2);
    assert!(state.selected_node_ids.iter().all(|id| !ids.contains(id)), "the copies are selected");

    let sent = h.key(egui::Key::C);
    dispatch_canvas(&mut state, &sent);
    assert_eq!(state.grade_node_clipboard.as_ref().map(|c| c.nodes.len()), Some(2), "copied from the compound: {sent:?}");
    let sent = h.key(egui::Key::V);
    let Some(AppCommand::PasteNodes { position, .. }) = sent.iter().find(|c| matches!(c, AppCommand::PasteNodes { .. })).cloned() else { panic!("{sent:?}") };
    dispatch_canvas(&mut state, &sent);
    let now = level(&state);
    assert_eq!(now.nodes.len(), before + 4);
    assert_eq!(state.active_history().undo_count(), steps + 2, "the paste one more step");
    let pasted: Vec<_> = state.selected_node_ids.iter().map(|id| now.node_positions[id]).collect();
    let min = pasted.iter().fold((f32::INFINITY, f32::INFINITY), |m, p| (m.0.min(p.x), m.1.min(p.y)));
    assert_eq!(min, position, "the paste lands where it was asked");
}

/// "Replace with…" swaps a node for one of another kind in its place:
/// its id, wires and place kept, one undo step.
#[test]
fn replace_keeps_the_node_its_wires_and_its_place() {
    let (mut state, clip_id, ids, mut h) = canvas_at_depth(|i| ((i as f32 - 2.0) * 230.0, 0.0));
    let level = |state: &AppState| level_of(clip(state, clip_id).grade(), &state.graph_descent_path.clone()).unwrap();
    let before = level(&state);
    let (_, rect) = h.card(NodeKind::Saturation);
    h.menu_item(rect.center(), "Replace with\u{2026}");
    h.settle();
    h.frame(vec![egui::Event::Text("Exposure".into())]);
    let sent = h.key(egui::Key::Enter);
    let steps = state.active_history().undo_count();
    dispatch_canvas(&mut state, &sent);
    let after = level(&state);
    let node = after.nodes.iter().find(|n| n.id == ids[1]).expect("the id kept");
    assert_eq!(node.kind, NodeKind::Exposure, "{sent:?}");
    assert_eq!(after.edges, before.edges, "its wires kept");
    assert_eq!(after.node_positions[&ids[1]], before.node_positions[&ids[1]], "its place kept");
    assert_eq!(state.active_history().undo_count(), steps + 1);
}

/// The node menu's Align lines a selection's top edges up, one undo
/// step for all of them.
#[test]
fn align_lines_a_selection_up_in_one_step() {
    let (mut state, clip_id, ids, mut h) = canvas_at_depth(|i| ((i as f32 - 2.0) * 230.0, i as f32 * 40.0));
    let level = |state: &AppState| level_of(clip(state, clip_id).grade(), &state.graph_descent_path.clone()).unwrap();
    h.set = ids.iter().copied().collect();
    h.selected = Some(ids[0]);
    h.settle();
    let (_, rect) = h.card(NodeKind::Contrast);
    h.click(rect.center(), egui::PointerButton::Secondary);
    h.settle();
    h.click(h.text("Align \u{25B8}"), egui::PointerButton::Primary);
    h.settle();
    let sent = h.click(h.text("Top edges"), egui::PointerButton::Primary);
    let steps = state.active_history().undo_count();
    dispatch_canvas(&mut state, &sent);
    let after = level(&state);
    let tops: Vec<f32> = ids.iter().map(|id| after.node_positions[id].y).collect();
    assert!(tops.iter().all(|y| *y == tops[0]), "{tops:?}: {sent:?}");
    assert_eq!(state.active_history().undo_count(), steps + 1, "one step: {sent:?}");
}

/// A note dragged on the grade canvas is one undo step for the drag, as
/// a node's drag is (#1745); it used to be a step a frame.
#[test]
fn a_grade_note_drag_is_one_step() {
    let (mut state, clip_id, _, _) = canvas_at_depth(|i| ((i as f32 - 2.0) * 230.0, 0.0));
    let note = klipp_nodes::NodeStickyNote { id: klipp_nodes::NodeStickyNoteId::new_v4(), pos: klipp_nodes::NodePos { x: -100.0, y: 300.0 }, width: 200.0, text: String::new(), color_rgba: u32::MAX };
    let descent = state.graph_descent_path.clone();
    state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().edit_grade(|g| {
        super::KlippApp::resolve_graph_at_path_mut(g, &descent).unwrap().sticky_notes.push(note.clone());
    });
    let mut h = crate::panels::node_graph_v2::canvas_command_tests::Harness::on(level_of(clip(&state, clip_id).grade(), &descent).unwrap(), clip_id);
    let at = h.text("Click to edit\u{2026}");
    let press = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let steps = state.active_history().undo_count();
    let mut sent = h.frame(vec![egui::Event::PointerMoved(at), press(at, true)]);
    for step in [20.0, 40.0, 60.0] {
        sent.extend(h.frame(vec![egui::Event::PointerMoved(at + egui::vec2(step, step))]));
    }
    sent.extend(h.frame(vec![press(at + egui::vec2(60.0, 60.0), false)]));
    dispatch_canvas(&mut state, &sent);
    let moved = level_of(clip(&state, clip_id).grade(), &descent).unwrap().sticky_notes[0].pos;
    assert!(moved.x > note.pos.x + 10.0 && moved.y > note.pos.y + 10.0, "the note moved: {moved:?}: {sent:?}");
    assert_eq!(state.active_history().undo_count(), steps + 1, "one step for the drag: {sent:?}");
}

/// A grade compound's chain Source → Exposure → Saturation → Contrast →
/// Output on the canvas, spread in a row (#1744).
fn wired() -> (AppState, ClipId, Vec<NodeId>, NodeId, crate::panels::node_graph_v2::canvas_command_tests::Harness) {
    let (state, clip_id, ids, h) = canvas_at_depth(|i| ((i as f32 - 2.0) * 230.0, 0.0));
    let source = h.graph.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    (state, clip_id, ids, source, h)
}

fn wire_level(state: &AppState, clip_id: ClipId) -> NodeGraph {
    level_of(clip(state, clip_id).grade(), &state.graph_descent_path.clone()).unwrap()
}

fn has_wire(g: &NodeGraph, from: NodeId, to: NodeId, to_port: u8) -> bool {
    g.edges.iter().any(|e| e.from_node == from && e.to_node == to && e.to_port == to_port)
}

/// A wire's menu (#1744): Delete lifts it; Add reroute and Insert node…
/// (the add menu, its pick) put a node into it, fed by its source and
/// feeding its end, one undo step each.
#[test]
fn a_wire_menu_deletes_inserts_and_adds_a_reroute() {
    let (mut state, clip_id, ids, _, mut h) = wired();
    let mid = h.wire_mid(ids[0], ids[1], 0);
    h.click(mid, egui::PointerButton::Secondary);
    h.settle();
    assert!(h.has("Insert node\u{2026}") && h.has("Add reroute") && h.has("Delete"), "the wire menu");
    let sent = h.click(h.text("Delete"), egui::PointerButton::Primary);
    assert!(sent.iter().any(|c| matches!(c, AppCommand::DisconnectEdge { to, to_port: 0, .. } if *to == ids[1])), "{sent:?}");

    h.settle();
    h.click(mid, egui::PointerButton::Secondary);
    h.settle();
    let sent = h.click(h.text("Add reroute"), egui::PointerButton::Primary);
    let steps = state.active_history().undo_count();
    dispatch_canvas(&mut state, &sent);
    let g = wire_level(&state, clip_id);
    let reroute = g.nodes.iter().find(|n| n.kind == NodeKind::Reroute).expect("a reroute added").id;
    assert!(has_wire(&g, ids[0], reroute, 0) && has_wire(&g, reroute, ids[1], 0) && !has_wire(&g, ids[0], ids[1], 0), "into the wire: {sent:?}");
    assert_eq!(state.active_history().undo_count(), steps + 1);

    let mid = h.wire_mid(ids[1], ids[2], 0);
    h.settle();
    h.click(mid, egui::PointerButton::Secondary);
    h.settle();
    h.click(h.text("Insert node\u{2026}"), egui::PointerButton::Primary);
    h.settle();
    h.frame(vec![egui::Event::Text("Exposure".into())]);
    let sent = h.key(egui::Key::Enter);
    dispatch_canvas(&mut state, &sent);
    let g = wire_level(&state, clip_id);
    let added = g.nodes.iter().find(|n| n.kind == NodeKind::Exposure && n.id != ids[0]).expect("an Exposure added").id;
    assert!(has_wire(&g, ids[1], added, 0) && has_wire(&g, added, ids[2], 0), "into the wire: {sent:?}");
    assert_eq!(state.active_history().undo_count(), steps + 2);
}

/// A wire's input end dragged to another input moves the wire there,
/// one step; let go in empty canvas, the wire goes (#1744).
#[test]
fn a_wire_end_dragged_off_reconnects_or_disconnects() {
    let (mut state, clip_id, ids, _, mut h) = wired();
    let steps = state.active_history().undo_count();
    let sent = h.drag(h.in_port(ids[1], 0), h.in_port(ids[2], 0));
    assert!(sent.iter().any(|c| matches!(c, AppCommand::ReconnectEdge { to, to_port: 0, new_to, new_to_port: 0, .. } if *to == ids[1] && *new_to == ids[2])), "{sent:?}");
    dispatch_canvas(&mut state, &sent);
    let g = wire_level(&state, clip_id);
    assert!(has_wire(&g, ids[0], ids[2], 0), "Exposure now feeds Contrast");
    assert!(!g.edges.iter().any(|e| e.to_node == ids[1]), "Saturation's input is free");
    assert!(!has_wire(&g, ids[1], ids[2], 0), "the wire it replaced is gone");
    assert_eq!(state.active_history().undo_count(), steps + 1);

    let below = h.card_of(ids[2]).center() + egui::vec2(0.0, 220.0);
    let sent = h.drag(h.in_port(ids[2], 0), below);
    assert!(sent.iter().any(|c| matches!(c, AppCommand::DisconnectEdge { to, to_port: 0, .. } if *to == ids[2])), "let go in empty canvas: {sent:?}");
}

/// A wire dragged from an output into empty canvas opens the add menu
/// there, and the node picked comes fed by it, one step (#1744).
#[test]
fn a_wire_let_go_in_empty_canvas_adds_a_node_fed_by_it() {
    let (mut state, clip_id, ids, _, mut h) = wired();
    let below = h.card_of(ids[2]).center() + egui::vec2(0.0, 220.0);
    let sent = h.drag(h.out_port(ids[2]), below);
    assert!(!sent.iter().any(|c| matches!(c, AppCommand::ConnectNodes { .. })), "{sent:?}");
    h.settle();
    h.frame(vec![egui::Event::Text("Exposure".into())]);
    let sent = h.key(egui::Key::Enter);
    assert!(sent.iter().any(|c| matches!(c, AppCommand::AddNodeWired { from, from_port: 0, .. } if *from == ids[2])), "{sent:?}");
    let steps = state.active_history().undo_count();
    dispatch_canvas(&mut state, &sent);
    let g = wire_level(&state, clip_id);
    let added = g.nodes.iter().find(|n| n.kind == NodeKind::Exposure && n.id != ids[0]).expect("added").id;
    assert!(has_wire(&g, ids[2], added, 0), "fed by the wire");
    assert_eq!(state.active_history().undo_count(), steps + 1);
}

/// A node dragged onto a wire goes into it: taken out of its place (what
/// fed it feeds what it fed) and put between the wire's ends, the drag
/// and the drop one undo step (#1744).
#[test]
fn a_node_dropped_on_a_wire_goes_into_it_in_one_step() {
    let (mut state, clip_id, ids, source, mut h) = wired();
    let output = h.graph.nodes.iter().find(|n| n.kind == NodeKind::Output).unwrap().id;
    let steps = state.active_history().undo_count();
    let target = h.wire_mid(source, ids[0], 0);
    let sent = h.drag(h.card_of(ids[2]).center(), target);
    assert!(sent.iter().any(|c| matches!(c, AppCommand::SpliceNodeIntoWire { node_id, from, to, .. } if *node_id == ids[2] && *from == source && *to == ids[0])), "{sent:?}");
    dispatch_canvas(&mut state, &sent);
    let g = wire_level(&state, clip_id);
    assert!(has_wire(&g, source, ids[2], 0) && has_wire(&g, ids[2], ids[0], 0), "Contrast between Source and Exposure");
    assert!(has_wire(&g, ids[1], output, 0), "Saturation now feeds the Output");
    assert_eq!(state.active_history().undo_count(), steps + 1, "the drag and the drop one step: {sent:?}");
}

/// A wire that would close a loop is refused where it is dropped, with
/// the reason on the canvas; one that would not is made (the control).
#[test]
fn a_wire_that_would_loop_says_why() {
    let (_, _, ids, _, mut h) = wired();
    let sent = h.drag(h.out_port(ids[2]), h.in_port(ids[0], 0));
    assert!(!sent.iter().any(|c| matches!(c, AppCommand::ConnectNodes { .. })), "{sent:?}");
    assert!(h.toast().is_some_and(|t| t.contains("loop")), "{:?}", h.toast());
    let sent = h.drag(h.out_port(ids[0]), h.in_port(ids[2], 0));
    assert!(sent.iter().any(|c| matches!(c, AppCommand::ConnectNodes { from, to, .. } if *from == ids[0] && *to == ids[2])), "{sent:?}");
}

