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
fn graded(kind: NodeKind) -> Option<(AppState, ClipId, NodeId)> {
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
}

impl Run {
    fn new(kind: NodeKind) -> Option<Self> {
        let (state, clip, node) = graded(kind)?;
        Some(Self { ctx: egui::Context::default(), state, clip, node, time: 0.5, clock: 0.0 })
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
        let frame = FrameSelection { time_secs: self.time, tolerance: 0.02 };
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                drawn = crate::inspector::grade_view::show(ui, &theme, &clip, self.node, &mut cmds, frame);
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

    fn apply(&mut self, cmds: &[AppCommand]) {
        for cmd in cmds {
            assert_eq!(self.state.path_command(cmd), Some(true), "{cmd:?} applied");
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
