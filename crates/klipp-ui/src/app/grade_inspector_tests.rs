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
        Some(Self { ctx: egui::Context::default(), state, clip, node, time: 0.5, clock: 0.0, panel: None, fresh: None })
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
            assert_eq!(self.state.path_command(&cmd), Some(true), "{cmd:?} applied");
        }
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
    let presentation = |p: &ParamDesc| ty.presentation(p);
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
            if matches!(pres.widget, Widget::Toggle | Widget::Combo) {
                stepped.push(format!("{kind:?} {}", desc.key));
                continue;
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
            match add.as_slice() {
                [AppCommand::AddKeyAt { addr: a, key, .. }] if *a == addr && key.as_str() == desc.key => {}
                other => {
                    misses.push(format!("{kind:?} {}: the diamond gave {other:?}", desc.key));
                    continue;
                }
            }
            run.apply(&add);
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
    assert_eq!(stepped, STEPPED, "the keyframable switches and choices, drawn without a diamond");
}

/// The keyframable switches and choices: their rows draw no diamond yet
/// (a switch and a choice row have none); whether they get one is IC1's
/// open question for stepped parameters.
const STEPPED: &[&str] = &[
    "Corrector wheels_log_mode",
    "Corrector curves_preserve_luma",
    "ColorWheels wheels_log_mode",
    "RGBCurves curves_preserve_luma",
    "Qualifier qual_enabled",
    "LumaKey luma_key_enabled",
    "Lut3D lut_input_space",
    "Lut3D lut_output_space",
    "PrimariesMatrix primaries_from",
    "PrimariesMatrix primaries_to",
];

/// A Corrector's exposure keyed through its row animates: keys at 0 s and
/// 1 s give the halfway value at 0.5 s on the grade node the engine reads.
#[test]
fn a_keyed_corrector_exposure_animates() {
    let mut run = Run::new(NodeKind::Corrector).unwrap();
    let (addr, _) = crate::inspector::grade_view::grade_node(clip(&run.state, run.clip), run.node).unwrap();
    for (t, v) in [(0.0, 0.0f32), (1.0, 1.0)] {
        let key = klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
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
    let key = klipp_core::Keyframe { time_secs: 0.49, value: 0.3, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
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
    let target = run.state.phantom_target(run.clip, named);
    assert_eq!(saved(&run.state, run.clip), before, "deciding the node changes nothing");
    let written = super::colour::edit_clip_graph(&mut run.state, run.clip, &[], true, |g| {
        super::colour::make_phantom(g, target);
        if let Some(n) = g.nodes.iter_mut().find(|n| n.id == target.0) {
            write(n);
        }
    });
    assert_eq!(written, Some(Ok(())));
    target.0
}

/// An editor's own command on the reserved Corrector makes it under that
/// id too (beside a disabled one: IC16), so the rows and the editors
/// write one node; the old page's phantom still writes the first
/// Corrector it finds, a disabled one included.
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

        // The old page's phantom: the first Corrector, whatever its state.
        let mut old = without_corrector(disabled);
        let id = editor_write(&mut old, klipp_nodes::PHANTOM_CORRECTOR_ID, |_| {});
        let n = clip(&old.state, old.clip).grade().nodes.iter().find(|n| n.id == id).cloned().unwrap();
        assert_eq!((n.kind, n.enabled), (NodeKind::Corrector, !disabled), "old: into the disabled one when there is one");
    }
}

/// A bare editor command (no Begin/End around it, as the Hue-Sat
/// eyedropper sends `SetNodeHueSatCurves`) on a Corrector that isn't made
/// yet is one step with the node it makes: undoing it leaves the clip
/// saved as it was, for the reserved id and the old phantom alike.
#[test]
fn undoing_a_bare_editor_command_takes_back_the_corrector_it_made() {
    for (disabled, reserve) in [(false, true), (true, true), (false, false)] {
        let mut run = without_corrector(disabled);
        tabbed_on(&mut run, false, crate::state::ColorTab::HueSat, None, Vec::new());
        let named = if reserve { run.state.pending_corrector.unwrap().1 } else { klipp_nodes::PHANTOM_CORRECTOR_ID };
        let before = saved(&run.state, run.clip);
        let made = editor_write(&mut run, named, |n| { n.params.huesat = Some(Default::default()); n.enabled = true; });
        assert!(clip(&run.state, run.clip).grade().nodes.iter().any(|n| n.id == made), "made");
        assert_eq!(run.state.active_history().undo_count(), 1);
        let entry = run.state.active_history_mut().undo().unwrap();
        let crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, .. } = entry.command else { panic!("a graph step") };
        run.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap().set_grade(old_graph);
        assert_eq!(saved(&run.state, run.clip), before, "disabled {disabled}, reserved {reserve}: as it was");
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
    (run, vec![GradeStandIn::Timeline, GradeStandIn::Group(g, klipp_timeline::GradeStage::Pre), GradeStandIn::Group(g, klipp_timeline::GradeStage::Post)])
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
/// and undo takes them off (#1321).
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
        assert_eq!(run.state.path_command(&AppCommand::SetMixAt { addr: addr.clone(), mix: Some(0.5) }), Some(true), "{w:?}: {:?}", run.state.toast.message_for_test());
        assert_eq!(run.state.path_command(&AppCommand::SetMaskChannelAt { addr, channel: Some("alpha".into()) }), Some(true), "{w:?}");
        assert_eq!(blend(&run), Some(klipp_nodes::params::EffectBlend { mix: 0.5, channel: MaskChannel::Alpha }), "{w:?}");
        undo_root(&mut run.state);
        assert_eq!(blend(&run), Some(klipp_nodes::params::EffectBlend { mix: 0.5, channel: MaskChannel::Red }), "{w:?}: undo takes the channel off");
        undo_root(&mut run.state);
        assert_eq!(blend(&run), None, "{w:?}: and then the mix");
    }
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
