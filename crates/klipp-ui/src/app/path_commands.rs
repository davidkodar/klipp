//! Path-addressed edits (M3 step 2): a node named by its root and the ids
//! down to it (`NodePath`), a parameter by its key. Each edit is a batch of
//! graph ops on that node's picture, applied as `GraphOps` is: one undo step
//! or nothing. They sit beside the index and bare-id commands, which stay
//! until the inspector switches over (step 19).
//!
//! A drag (`BeginParamEditAt` .. `EndParamEditAt`) of one `(addr, key)` is
//! one undo step. Any other edit while it is open closes it first, as one
//! step, and then records its own: an open drag never merges another edit.

use super::*;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{Node, ParamKey};
use klipp_graph_lift::ops::GraphOp;

/// What every path drag's history key starts with.
const PATH_DRAG: &str = "at:";

/// The history key a path drag is held under.
fn drag_key(addr: &NodePath, key: &ParamKey) -> String {
    let ids: Vec<String> = addr.nodes.iter().map(|n| n.to_string()).collect();
    format!("{PATH_DRAG}{:?}:{}:{}", addr.root, ids.join("/"), key)
}

/// The graph ops a path-addressed edit is, and its undo label; `None` for
/// the commands that aren't one batch of ops (the drags, the dock and the
/// split view's state, and every other command).
pub(crate) fn path_ops(cmd: &AppCommand) -> Option<(Vec<GraphOp>, &'static str)> {
    Some(match cmd {
        AppCommand::AddNodeAt { after, node } => (vec![GraphOp::AddNode { after: after.clone(), node: node.clone() }], "Add Node"),
        AppCommand::RemoveNodeAt { addr } => (vec![GraphOp::RemoveNode { path: addr.clone() }], "Remove Node"),
        AppCommand::MoveNodeAt { addr, after } => (vec![GraphOp::MoveAfter { path: addr.clone(), after: *after }], "Move Node"),
        AppCommand::SetNodeEnabledAt { addr, enabled } => (vec![GraphOp::SetEnabled { path: addr.clone(), enabled: *enabled }], "Toggle Node"),
        AppCommand::SetParamAt { addr, key, value } => (vec![GraphOp::SetParam { path: addr.clone(), key: key.clone(), value: value.clone() }], "Edit Parameter"),
        AppCommand::SetPayloadAt { addr, payload } => (vec![GraphOp::SetPayload { path: addr.clone(), payload: payload.clone() }], "Edit Effect"),
        AppCommand::SetMixAt { addr, mix } => (vec![GraphOp::SetMix { path: addr.clone(), mix: *mix }], "Set Mix"),
        AppCommand::ConnectMaskAt { addr, from, from_port } => (
            vec![GraphOp::Connect { from: from.clone(), from_port: from_port.clone(), to: addr.clone(), to_port: "mask".into() }],
            "Connect Mask",
        ),
        AppCommand::SetMaskChannelAt { addr, channel } => (vec![GraphOp::SetMaskChannel { path: addr.clone(), channel: channel.clone() }], "Set Mask Channel"),
        AppCommand::AddKeyAt { addr, key, keyframe } => (vec![GraphOp::AddKeyAt { path: addr.clone(), key: key.clone(), keyframe: keyframe.clone() }], "Add Keyframe"),
        AppCommand::RemoveKeyAt { addr, key, time_secs } => (vec![GraphOp::RemoveKeyAt { path: addr.clone(), key: key.clone(), time_secs: *time_secs }], "Remove Keyframe"),
        _ => return None,
    })
}

/// The node `path` names in `nodes`: each id but the last a group whose
/// body holds the next. A path that skips a group isn't found: nothing is
/// searched for.
fn walk<'a>(mut nodes: &'a [Node], ids: &[klipp_graph_format::NodeId]) -> Option<&'a Node> {
    let (last, through) = ids.split_last()?;
    for step in through {
        nodes = &nodes.iter().find(|n| n.id == *step)?.group.as_ref()?.graph.nodes;
    }
    nodes.iter().find(|n| n.id == *last)
}

impl KlippApp {
    pub(super) fn handle_path_commands(&mut self, cmd: &AppCommand) -> bool {
        match self.state.path_command(cmd) {
            None => false,
            Some(changed) => {
                if changed {
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                if matches!(cmd, AppCommand::SetSplitCompareNode { .. }) {
                    self.split_compare_to_engine();
                }
                true
            }
        }
    }

    /// The split compare as the state holds it, to the engine (M3 step 4),
    /// which renders the compared node bypassed: a refused command leaves
    /// it as it was. Comparing an effect ends the other compare modes.
    fn split_compare_to_engine(&mut self) {
        let compared = self.state.split_compare_node.clone();
        if compared.is_some() {
            if self.state.before_after_active {
                self.state.before_after_active = false;
                let _ = self.cmd_tx.send(AppCommand::ToggleBeforeAfter);
            }
            if self.state.stabilization_compare_active {
                self.state.stabilization_compare_active = false;
                let _ = self.cmd_tx.send(AppCommand::ToggleStabilizationCompare);
            }
            self.state.grade_still_compare = false;
        }
        let (clip_id, path) = compared.map_or((None, None), |(c, p)| (Some(c), Some(p)));
        let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id, path });
    }
}

impl crate::state::AppState {
    /// Run a path-addressed command on the state: `None` when `cmd` isn't
    /// one, else whether a picture changed (the engine wants the
    /// sequence). A refusal is said in a toast and changes nothing.
    pub(crate) fn path_command(&mut self, cmd: &AppCommand) -> Option<bool> {
        let result = match cmd {
            AppCommand::BeginParamEditAt { addr, key } => self.begin_param_edit_at(addr, key).map(|()| false),
            AppCommand::EndParamEditAt { addr, key } => {
                self.end_param_edit_at(addr, key);
                Ok(false)
            }
            AppCommand::RevealGraph { path, select } => self.reveal_graph(path, *select).map(|()| false),
            AppCommand::SetGraphDockRoot { root } => self.set_graph_dock_root(*root).map(|()| false),
            AppCommand::SetSplitCompareNode { clip_id, path } => self.set_split_compare_node(*clip_id, path.as_ref()).map(|()| false),
            AppCommand::MoveKeyAt { addr, key, time_secs, new_time_secs, value } => self
                .key_edit(addr, key, *time_secs, |k| {
                    k["time_secs"] = serde_json::json!(new_time_secs);
                    k["value"] = serde_json::json!(value);
                })
                .and_then(|ops| self.apply_path_ops(&ops, "Move Keyframe"))
                .map(|()| true),
            AppCommand::SetInterpKeyAt { addr, key, time_secs, interpolation } => self
                .interpolation_edit(addr, key, *time_secs, *interpolation)
                .and_then(|ops| self.apply_path_ops(&ops, "Keyframe Interpolation"))
                .map(|()| true),
            _ => {
                let (ops, label) = path_ops(cmd)?;
                self.apply_path_ops(&ops, label).map(|()| true)
            }
        };
        match result {
            Ok(changed) => Some(changed),
            Err(why) => {
                tracing::warn!(%why, "path edit refused");
                self.toast.show(why);
                Some(false)
            }
        }
    }

    /// Whether `cmd`, arriving while a path drag is open, closes it first:
    /// every command but the drag's own (`BeginParamEditAt`, `SetParamAt`
    /// and `EndParamEditAt` of its `(addr, key)`) and those that change no
    /// picture (transport, a refresh, the dock and compare state). An edit
    /// by any route, old or new, is then its own step, never folded into
    /// the drag's snapshot nor taken as one of its live steps. A command
    /// missing from the list only splits a drag into more steps; it can't
    /// mix two edits.
    pub(crate) fn closes_path_drag(&self, cmd: &AppCommand) -> bool {
        let Some(open) = self.active_history().pending_drag_key().filter(|k| k.starts_with(PATH_DRAG)) else { return false };
        match cmd {
            AppCommand::BeginParamEditAt { addr, key } | AppCommand::SetParamAt { addr, key, .. } | AppCommand::EndParamEditAt { addr, key } => drag_key(addr, key) != open,
            AppCommand::Play
            | AppCommand::Pause
            | AppCommand::Stop
            | AppCommand::SeekTo(_)
            | AppCommand::SeekRelative(_)
            | AppCommand::SequenceUpdated
            | AppCommand::RevealGraph { .. }
            | AppCommand::SetGraphDockRoot { .. }
            | AppCommand::SetSplitCompareNode { .. } => false,
            _ => true,
        }
    }

    /// Close an open path drag before an edit applied outside the dispatch
    /// boundary (`apply_edits_as_one`: the control socket's edits and graph
    /// ops, edit plans, and the app's own whole edits). Such an edit is
    /// never part of a path drag, whose steps are written live, so any
    /// open one is closed, as its own step, before the edit is applied.
    pub(crate) fn close_path_drag_before_edit(&mut self) {
        if self.active_history().pending_drag_key().is_some_and(|k| k.starts_with(PATH_DRAG)) {
            self.close_open_drag();
        }
    }

    /// The dispatch boundary every command passes (M3 step 2): an open path
    /// drag that `cmd` doesn't belong to is closed as its own step before
    /// any handler runs.
    pub(crate) fn before_dispatch(&mut self, cmd: &AppCommand) {
        if self.closes_path_drag(cmd) {
            self.close_open_drag();
        }
    }

    /// `ops` as one undo step named `label`, or live inside the open drag
    /// of the one `(addr, key)` they set. Any other edit closes an open
    /// drag first, as its own step.
    pub(crate) fn apply_path_ops(&mut self, ops: &[GraphOp], label: &str) -> Result<(), String> {
        let refused = |(at, why): (Option<usize>, String)| match at {
            Some(i) if ops.len() > 1 => format!("{label}: op {i}: {why}"),
            _ => format!("{label}: {why}"),
        };
        if let [GraphOp::SetParam { path, key, .. }] = ops {
            if self.active_history().is_dragging(&drag_key(path, key)) {
                let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], ops).map_err(refused)?;
                if let EditCommand::PictureEdit { clip_id, edit, .. } = edit {
                    self.set_picture(clip_id, &edit.after);
                }
                return Ok(());
            }
        }
        if self.active_history().is_any_drag_in_progress() {
            self.close_open_drag();
        }
        let graph_ops::GraphOpsOutcome { edit, ungated, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], ops).map_err(refused)?;
        if let EditCommand::PictureEdit { clip_id, edit, .. } = &edit {
            self.set_picture(*clip_id, &edit.after);
        }
        self.active_history_mut().begin_batch();
        self.push_history(edit);
        self.active_history_mut().end_batch_as(label);
        self.show_ungated(&ungated);
        Ok(())
    }

    /// Write `picture` to the clip, with no history entry of its own.
    fn set_picture(&mut self, clip_id: ClipId, picture: &klipp_timeline::ClipPictureFields) {
        let seq = &mut self.sequences[self.active_sequence_idx];
        if let Some(clip) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)) {
            clip.restore_picture(picture);
            self.dirty = true;
            self.media_usage_cache_dirty = true;
        }
    }

    fn begin_param_edit_at(&mut self, addr: &NodePath, key: &ParamKey) -> Result<(), String> {
        if self.active_history().is_any_drag_in_progress() {
            self.close_open_drag();
        }
        let Root::Clip { clip } = addr.root else { return Err("only a clip's graph takes edits by path so far".into()) };
        self.node_at(addr)?;
        let clip_id = ClipId(clip);
        let before = self.picture_of(clip_id).ok_or_else(|| format!("no clip {clip} on this sequence"))?;
        self.active_history_mut().begin_drag(
            drag_key(addr, key),
            HistoryEntry {
                command: EditCommand::PictureEdit { clip_id, description: "Edit Parameter", edit: Box::new(klipp_timeline::PictureEdit { before: before.clone(), after: before }) },
            },
        );
        Ok(())
    }

    fn end_param_edit_at(&mut self, addr: &NodePath, key: &ParamKey) {
        if !self.active_history().is_dragging(&drag_key(addr, key)) {
            return;
        }
        // The entry keeps the drag's own "before": closing it is closing
        // any open drag, from its start to the picture now.
        self.close_open_drag();
    }

    /// The one op that changes the key of `(addr, key)` at `time_secs` by
    /// `edit`: the parameter's whole animation, written back in time order.
    fn key_edit(&self, addr: &NodePath, key: &ParamKey, time_secs: f64, edit: impl FnOnce(&mut serde_json::Value)) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let mut anim = node.anim.get(key).cloned().ok_or_else(|| format!("{key} isn't animated"))?;
        let time = |k: &serde_json::Value| k.get("time_secs").and_then(serde_json::Value::as_f64);
        let at = anim.keys.iter().position(|k| time(k).is_some_and(|t| (t - time_secs).abs() < 1e-9)).ok_or_else(|| format!("{key} has no key at {time_secs} s"))?;
        edit(&mut anim.keys[at]);
        anim.keys.sort_by(|a, b| time(a).unwrap_or(0.0).total_cmp(&time(b).unwrap_or(0.0)));
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim: Some(anim) }])
    }

    /// The one op that sets the interpolation at the key of `(addr, key)` at
    /// `time_secs` as the keyframe editor sets it
    /// ([`klipp_core::keyframe::set_interpolation_at`]): that key and the
    /// one before it, with Bezier tangents where they have none.
    fn interpolation_edit(&self, addr: &NodePath, key: &ParamKey, time_secs: f64, interpolation: klipp_core::Interpolation) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let mut anim = node.anim.get(key).cloned().ok_or_else(|| format!("{key} isn't animated"))?;
        let mut keys: Vec<klipp_core::Keyframe> = anim.keys.iter().map(|k| serde_json::from_value(k.clone()).map_err(|e| format!("a key of {key}: {e}"))).collect::<Result<_, _>>()?;
        let at = keys.iter().position(|k| (k.time_secs - time_secs).abs() < 1e-9).ok_or_else(|| format!("{key} has no key at {time_secs} s"))?;
        klipp_core::keyframe::set_interpolation_at(&mut keys, at, interpolation);
        anim.keys = keys.iter().map(|k| serde_json::to_value(k).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim: Some(anim) }])
    }

    /// The node `path` names, on this sequence: only a clip's graph so far,
    /// as `GraphOps` takes.
    fn node_at(&self, path: &NodePath) -> Result<Node, String> {
        let Root::Clip { clip } = path.root else { return Err("only a clip's graph is shown by path so far".into()) };
        let c = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id.0 == clip).ok_or_else(|| format!("no clip {clip} on this sequence"))?;
        let lifted = c.picture_v2().map_err(|e| e.to_string())?;
        walk(&lifted.picture.graph.nodes, &path.nodes).cloned().ok_or_else(|| format!("{:?} names no node of the clip's graph", path.nodes))
    }

    fn reveal_graph(&mut self, path: &NodePath, select: bool) -> Result<(), String> {
        self.node_at(path)?;
        self.graph_dock_open = true;
        self.graph_dock_root = Some(path.root);
        self.graph_dock_path = path.nodes[..path.nodes.len() - 1].to_vec();
        if select {
            self.selected_node_addr = Some(path.clone());
        }
        Ok(())
    }

    fn set_graph_dock_root(&mut self, root: Root) -> Result<(), String> {
        let Root::Clip { clip } = root else { return Err("only a clip's graph is shown by path so far".into()) };
        if !self.sequences[self.active_sequence_idx].all_clips().any(|c| c.id.0 == clip) {
            return Err(format!("no clip {clip} on this sequence"));
        }
        self.graph_dock_root = Some(root);
        self.graph_dock_path.clear();
        Ok(())
    }

    /// Whether the split compare's node has gone (removed, or its clip):
    /// then the compare is ended here, and the caller tells the engine.
    pub(crate) fn split_compare_gone(&mut self) -> bool {
        let Some((_, path)) = self.split_compare_node.clone() else { return false };
        if self.node_at(&path).is_ok() {
            return false;
        }
        self.split_compare_node = None;
        true
    }

    fn set_split_compare_node(&mut self, clip_id: Option<ClipId>, path: Option<&NodePath>) -> Result<(), String> {
        let (Some(clip_id), Some(path)) = (clip_id, path) else {
            self.split_compare_node = None;
            return Ok(());
        };
        if path.root != (Root::Clip { clip: clip_id.0 }) {
            return Err("the compared node is in another clip's graph".into());
        }
        self.node_at(path)?;
        self.split_compare_node = Some((clip_id, path.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! M3 step 2's proofs for the effect commands (m3-step2-test-spec.md
    //! §2): each index command and its path twin give the same picture and
    //! history, a drag by path is one undo step, an edit lands only in the
    //! root and node it names, and ids are kept.

    use super::*;
    use crate::state::AppState;
    use klipp_effects::BuiltinEffect as E;
    use klipp_graph_format::{types, ParamValue};
    use klipp_timeline::{EffectEdit, GradeAccess};

    /// A clip with `effects`, ready to put in states of its own.
    fn fixture(effects: &[E]) -> klipp_timeline::Clip {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        for e in effects {
            clip.push_effect(e.clone());
        }
        clip
    }

    /// A fresh state holding `clips` on its first video track.
    fn with(clips: &[klipp_timeline::Clip]) -> AppState {
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.extend(clips.iter().cloned());
        state
    }

    fn state(effects: &[E]) -> (AppState, ClipId) {
        let c = fixture(effects);
        let id = c.id;
        (with(&[c]), id)
    }

    fn clip(state: &AppState, id: ClipId) -> &klipp_timeline::Clip {
        state.sequences[0].all_clips().find(|c| c.id == id).unwrap()
    }

    /// The whole picture, as typed: the graph and the versions.
    fn picture(state: &AppState, id: ClipId) -> klipp_timeline::LiftedPicture {
        (*clip(state, id).picture_v2().unwrap()).clone()
    }

    /// The effect nodes, in list order.
    fn effects(state: &AppState, id: ClipId) -> Vec<klipp_graph_format::NodeId> {
        clip(state, id).picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect()
    }

    fn at(id: ClipId, nodes: &[klipp_graph_format::NodeId]) -> NodePath {
        NodePath::new(Root::Clip { clip: id.0 }, nodes.iter().copied())
    }

    /// What the app's old handler does with an index command
    /// (`effect_edit_by_ops`): the clip's own ops for the edit, one picture
    /// edit on the history.
    fn old(state: &mut AppState, id: ClipId, edit: EffectEdit) {
        let seq = &mut state.sequences[0];
        let c = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
        let ops = c.effect_edit_ops(edit).expect("a plain effect edit");
        let done = c.apply_picture_ops(&ops).unwrap();
        state.push_history(EditCommand::PictureEdit { description: "Edit Effect", clip_id: id, edit: Box::new(done) });
    }

    /// Undo once, as the app does for a picture edit.
    fn undo(state: &mut AppState) {
        let entry = state.active_history_mut().undo().expect("a step to undo");
        let mut command = entry.command;
        // A path edit is recorded under its label (`Named`).
        if let EditCommand::Named { edit, .. } = command {
            command = *edit;
        }
        let EditCommand::PictureEdit { clip_id, edit, .. } = command else { panic!("a picture edit") };
        state.set_picture(clip_id, &edit.before);
    }

    fn run(state: &mut AppState, cmd: AppCommand) {
        assert_eq!(state.path_command(&cmd), Some(true), "{cmd:?} applied");
    }

    /// Every index command on a clip's effect list has a path twin: a new
    /// command naming an effect by index fails to compile here until it is
    /// listed. OFX plug-ins' twins are out of step 2 (the clip's writer
    /// refuses OFX nodes; they get a step of their own), as are track,
    /// audio and title-object effects.
    fn has_a_twin(cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::AddBuiltinEffect { .. }
            | AppCommand::RemoveBuiltinEffect { .. }
            | AppCommand::ToggleBuiltinEffectEnabled { .. }
            | AppCommand::UpdateBuiltinEffect { .. }
            | AppCommand::ReorderBuiltinEffect { .. }
            | AppCommand::AddEffectKeyframe { .. }
            | AppCommand::RemoveEffectKeyframe { .. }
            | AppCommand::MoveEffectKeyframe { .. }
            | AppCommand::SetEffectKeyframeInterpolation { .. }
            | AppCommand::BeginEffectDrag { .. }
            | AppCommand::EndEffectDrag { .. } => true,
            AppCommand::AddEffect { .. }
            | AppCommand::RemoveEffect { .. }
            | AppCommand::SetEffectEnabled { .. }
            | AppCommand::ReorderEffect { .. }
            | AppCommand::SetEffectParam { .. } => false,
            _ => unreachable!("not an index command"),
        }
    }

    /// Each built-in effect command by index, and its path twin on a copy:
    /// the same picture, one history step each, and one undo back to the
    /// picture before (proof 1, the effect list's half).
    #[test]
    fn every_index_command_has_a_path_twin() {
        let fx = [E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }, E::Gamma { gamma: 1.2 }];
        let c = fixture(&fx);
        let id = c.id;
        let base = with(std::slice::from_ref(&c));
        let ids = effects(&base, id);
        let (i0, i1, i2) = (ids[0], ids[1], ids[2]);
        let gamma = E::Gamma { gamma: 1.6 };
        let added = E::Vignette { amount: 0.3, size: 0.8, softness: 0.5, roundness: 0.7 };
        type Twin = Box<dyn Fn(&AppState) -> AppCommand>;
        let cases: Vec<(AppCommand, EffectEdit, Twin)> = vec![
            (
                AppCommand::RemoveBuiltinEffect { clip_id: id, index: 1 },
                EffectEdit::Remove(1),
                Box::new(move |_| AppCommand::RemoveNodeAt { addr: at(id, &[i1]) }),
            ),
            (
                AppCommand::ToggleBuiltinEffectEnabled { clip_id: id, index: 0 },
                EffectEdit::SetEnabled(0, false),
                Box::new(move |_| AppCommand::SetNodeEnabledAt { addr: at(id, &[i0]), enabled: false }),
            ),
            (
                AppCommand::UpdateBuiltinEffect { clip_id: id, index: 2, effect: gamma.clone() },
                EffectEdit::Update(2, &gamma),
                Box::new(move |_| AppCommand::SetPayloadAt { addr: at(id, &[i2]), payload: serde_json::to_value(E::Gamma { gamma: 1.6 }).unwrap() }),
            ),
            (
                AppCommand::ReorderBuiltinEffect { clip_id: id, from_index: 2, to_index: 1 },
                EffectEdit::Move { from: 2, to: 1 },
                Box::new(move |_| AppCommand::MoveNodeAt { addr: at(id, &[i2]), after: i0 }),
            ),
        ];
        for (index_cmd, edit, twin) in cases {
            assert!(has_a_twin(&index_cmd));
            let (mut a, mut b) = (with(std::slice::from_ref(&c)), with(std::slice::from_ref(&c)));
            old(&mut a, id, edit);
            let cmd = twin(&b);
            run(&mut b, cmd);
            assert_eq!(picture(&a, id), picture(&b, id), "{index_cmd:?}: the twin draws the same picture");
            assert_eq!(a.active_history().undo_count(), b.active_history().undo_count(), "{index_cmd:?}: one step each");
            undo(&mut b);
            assert_eq!(picture(&b, id), picture(&base, id), "{index_cmd:?}: undone");
        }
        // The add: the twin names the node the old add made, so the two
        // pictures are the same down to its id.
        let index_cmd = AppCommand::AddBuiltinEffect { clip_id: id, effect: added.clone() };
        assert!(has_a_twin(&index_cmd));
        let (mut a, mut b) = (with(std::slice::from_ref(&c)), with(std::slice::from_ref(&c)));
        old(&mut a, id, EffectEdit::Add(&added));
        let made = clip(&a, id).picture_v2().unwrap().picture.graph.nodes.iter().find(|n| !ids.contains(&n.id) && n.type_id.as_str() == types::LEGACY_EFFECT).cloned().expect("the added node");
        run(&mut b, AppCommand::AddNodeAt { after: at(id, &[ids[2]]), node: Box::new(made) });
        assert_eq!(picture(&a, id), picture(&b, id), "the add's twin draws the same picture");
        undo(&mut b);
        assert_eq!(picture(&b, id), picture(&base, id), "the add undone");
    }

    /// A drag by path is one undo step, whatever its steps: undo returns
    /// the value from before it (proof 2).
    #[test]
    fn a_drag_by_path_is_one_undo_step() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let before = picture(&s, id);
        let blur = at(id, &effects(&s, id));
        let key: ParamKey = "radius".into();
        let steps = s.active_history().undo_count();
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: key.clone() }), Some(false));
        for i in 0..30 {
            run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: key.clone(), value: ParamValue::Float(3.0 + i as f32) });
        }
        assert_eq!(s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: key.clone() }), Some(false));
        assert_eq!(s.active_history().undo_count(), steps + 1, "thirty steps, one entry");
        assert!(matches!(clip(&s, id).builtin_effects()[0], E::GaussianBlur { radius } if radius == 32.0));
        undo(&mut s);
        assert_eq!(picture(&s, id), before, "undo returns the value from before the drag");
    }

    /// An edit of another `(addr, key)` while a drag is open closes the
    /// drag as its own step and records its own: it never merges into the
    /// drag's entry. A drag left open is closed by the next edit the same way.
    #[test]
    fn an_edit_inside_a_drag_closes_it_first() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let (blur, sharpen) = (at(id, &[ids[0]]), at(id, &[ids[1]]));
        let steps = s.active_history().undo_count();
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        run(&mut s, AppCommand::SetParamAt { addr: sharpen.clone(), key: "amount".into(), value: ParamValue::Float(0.9) });
        assert!(!s.active_history().is_any_drag_in_progress(), "the other edit closed the drag");
        assert_eq!(s.active_history().undo_count(), steps + 2, "the drag, then the edit");
        // The drag's End after it was closed changes nothing.
        s.path_command(&AppCommand::EndParamEditAt { addr: blur, key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), steps + 2);
        undo(&mut s);
        assert!(matches!(clip(&s, id).builtin_effects()[1], E::Sharpen { amount } if amount == 0.5), "the edit undoes alone");
        assert!(matches!(clip(&s, id).builtin_effects()[0], E::GaussianBlur { radius } if radius == 9.0), "the drag's step stays");
    }

    /// An edit by path changes only the clip and node it names; a path
    /// that skips a group isn't searched for, it's refused, and nothing
    /// changes (proofs 3 and 4).
    #[test]
    fn an_edit_lands_only_in_the_addressed_root() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let v1 = s.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let mut copy = clip(&s, a).clone();
        copy.id = ClipId::new();
        let b = copy.id;
        s.sequences[0].tracks[v1].clips.push(copy);
        let other = picture(&s, b);
        let fx_a = effects(&s, a);
        run(&mut s, AppCommand::SetParamAt { addr: at(a, &fx_a), key: "radius".into(), value: ParamValue::Float(7.0) });
        assert!(matches!(clip(&s, a).builtin_effects()[0], E::GaussianBlur { radius } if radius == 7.0));
        assert_eq!(picture(&s, b), other, "the other clip is untouched");
        // A node inside the grade group, named without the group.
        let grade = clip(&s, a).picture_v2().unwrap().picture.graph.nodes.iter().find_map(|n| n.group.as_ref().and_then(|g| g.graph.nodes.first()).map(|inner| inner.id));
        if let Some(inner) = grade {
            let steps = s.active_history().undo_count();
            let before = picture(&s, a);
            assert_eq!(s.path_command(&AppCommand::SetNodeEnabledAt { addr: at(a, &[inner]), enabled: false }), Some(false), "refused");
            assert_eq!(picture(&s, a), before);
            assert_eq!(s.active_history().undo_count(), steps);
        }
    }

    /// M3 step 4: the split compare names a node, so moving the effects
    /// around it, or taking away one before it, leaves it on the same
    /// effect (the index it held was patched by hand on a removal, and a
    /// reorder moved it onto another effect); taking the compared effect
    /// away ends it. The engine toggles the same node.
    #[test]
    fn split_compare_survives_a_reorder() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }, E::Vibrance { amount: 0.2 }]);
        let fx = effects(&s, a);
        let compared = at(a, &[fx[2]]);
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(a), path: Some(compared.clone()) }), Some(false));
        let is_vibrance = |s: &AppState| {
            let (_, path) = s.split_compare_node.clone().expect("still compared");
            let c = clip(s, a);
            let (ofx, i) = c.effect_slot_of_node(path.node().unwrap()).expect("the clip holds it");
            !ofx && matches!(c.builtin_effects()[i], E::Vibrance { .. })
        };
        // Moved to the front, then the effect before it taken away.
        run(&mut s, AppCommand::MoveNodeAt { addr: compared.clone(), after: fx[0] });
        assert!(!s.split_compare_gone());
        assert!(is_vibrance(&s), "the compare moved with its effect");
        run(&mut s, AppCommand::RemoveNodeAt { addr: at(a, &[fx[0]]) });
        assert!(!s.split_compare_gone());
        assert!(is_vibrance(&s), "a removal before it doesn't move it");
        let mut toggled = clip(&s, a).clone();
        assert!(toggled.toggle_effect_node(compared.node().unwrap()));
        let i = toggled.effect_slot_of_node(compared.node().unwrap()).unwrap().1;
        assert!(!toggled.builtin_effects_enabled()[i], "the engine's bypass is the compared effect's");
        // The compared effect taken away: the compare ends.
        run(&mut s, AppCommand::RemoveNodeAt { addr: compared });
        assert!(s.split_compare_gone());
        assert!(s.split_compare_node.is_none());
    }

    /// State held by address belongs to the clip it names: a selection,
    /// a compared node and a dock root on one clip aren't another's, even
    /// where the local node ids agree.
    #[test]
    fn two_clips_with_the_same_local_node_id_do_not_share_state() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let v1 = s.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let mut copy = clip(&s, a).clone();
        copy.id = ClipId::new();
        let b = copy.id;
        s.sequences[0].tracks[v1].clips.push(copy);
        let node = effects(&s, a)[0];
        assert_eq!(s.path_command(&AppCommand::RevealGraph { path: at(a, &[node]), select: true }), Some(false));
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(a), path: Some(at(a, &[node])) }), Some(false));
        assert_eq!(s.selected_node_addr.as_ref().map(|p| p.root), Some(Root::Clip { clip: a.0 }));
        assert_ne!(s.selected_node_addr, Some(at(b, &[node])), "B's node of the same id isn't selected");
        assert_eq!(s.split_compare_node.as_ref().map(|(c, _)| *c), Some(a));
        // A path naming A's node in B's graph is refused.
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(b), path: Some(at(a, &[node])) }), Some(false));
        assert_eq!(s.split_compare_node.as_ref().map(|(c, _)| *c), Some(a), "unchanged");
    }

    /// Mix, mask channel and mask feed by path: each one step, the clip
    /// picture-truth after, and undo brings back the clip as it was; a
    /// type that takes no mix is refused with nothing changed.
    #[test]
    fn mix_and_masks_by_path_are_one_step_each() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }, E::Invert { mix: 1.0 }]);
        let id = c.id;
        let base = with(std::slice::from_ref(&c));
        let ids = effects(&base, id);
        for cmd in [
            AppCommand::SetMixAt { addr: at(id, &[ids[0]]), mix: Some(0.5) },
            AppCommand::SetMaskChannelAt { addr: at(id, &[ids[0]]), channel: Some("alpha".into()) },
        ] {
            let mut s = with(std::slice::from_ref(&c));
            run(&mut s, cmd.clone());
            assert_eq!(s.active_history().undo_count(), base.active_history().undo_count() + 1, "{cmd:?}");
            assert!(clip(&s, id).is_picture_truth(), "{cmd:?}: the picture holds it");
            undo(&mut s);
            assert!(!clip(&s, id).is_picture_truth(), "{cmd:?}: undone");
            assert_eq!(picture(&s, id), picture(&base, id));
        }
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, id);
        assert_eq!(s.path_command(&AppCommand::SetMixAt { addr: at(id, &[ids.get(1).copied().unwrap_or(ids[0])]), mix: Some(0.5) }), Some(false), "Invert takes no mix");
        assert_eq!(picture(&s, id), before);
    }

    /// `RevealGraph` is idempotent and never closes the dock; it selects
    /// on request; an address that names nothing is refused with no change.
    /// `SetGraphDockRoot` sets the root and goes back to its top level.
    #[test]
    fn reveal_graph_and_the_dock_root() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&s, id)[0];
        s.graph_dock_path = vec![uuid::Uuid::from_u128(7)];
        for _ in 0..2 {
            assert_eq!(s.path_command(&AppCommand::RevealGraph { path: at(id, &[node]), select: false }), Some(false));
            assert!(s.graph_dock_open);
            assert_eq!(s.graph_dock_root, Some(Root::Clip { clip: id.0 }));
            assert!(s.graph_dock_path.is_empty());
            assert_eq!(s.selected_node_addr, None);
        }
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[node]), select: true });
        assert_eq!(s.selected_node_addr, Some(at(id, &[node])));
        let held = (s.graph_dock_open, s.graph_dock_root, s.graph_dock_path.clone(), s.selected_node_addr.clone());
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[uuid::Uuid::from_u128(9)]), select: true });
        assert_eq!((s.graph_dock_open, s.graph_dock_root, s.graph_dock_path.clone(), s.selected_node_addr.clone()), held, "refused, unchanged");
        s.graph_dock_path = vec![node];
        s.path_command(&AppCommand::SetGraphDockRoot { root: Root::Clip { clip: id.0 } });
        assert!(s.graph_dock_path.is_empty());
        s.path_command(&AppCommand::SetGraphDockRoot { root: Root::Clip { clip: uuid::Uuid::from_u128(5) } });
        assert_eq!(s.graph_dock_root, Some(Root::Clip { clip: id.0 }), "a clip not here is refused");
    }

    /// Ids are kept (M3 proposal 6 is in): the node an add names is the
    /// node the clip holds, and a move renames nothing, so a selection made
    /// before the move still names the moved effect.
    #[test]
    fn ids_are_kept_by_add_and_move() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let mut node = clip(&s, id).picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.id == ids[1]).cloned().unwrap();
        node.id = uuid::Uuid::from_u128(0xadd);
        node.legacy.as_mut().unwrap().payload = serde_json::to_value(E::Gamma { gamma: 1.1 }).unwrap();
        run(&mut s, AppCommand::AddNodeAt { after: at(id, &[ids[1]]), node: Box::new(node) });
        assert_eq!(effects(&s, id), vec![ids[0], ids[1], uuid::Uuid::from_u128(0xadd)], "the clip keeps the add's id");
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[ids[1]]), select: true });
        let order_before = effects(&s, id);
        run(&mut s, AppCommand::MoveNodeAt { addr: at(id, &[ids[1]]), after: uuid::Uuid::from_u128(0xadd) });
        let mut moved = effects(&s, id);
        assert_eq!(moved, vec![ids[0], uuid::Uuid::from_u128(0xadd), ids[1]], "moved, every id the same");
        moved.sort();
        let mut was = order_before;
        was.sort();
        assert_eq!(moved, was);
        assert_eq!(s.selected_node_addr, Some(at(id, &[ids[1]])), "the selection still names it");
    }

    #[test]
    fn removing_a_mask_by_path_reports_its_reader_and_undo_restores_it() {
        let mut c = fixture(&[]);
        let mut grade = klipp_nodes::NodeGraph::default();
        let (mask, reader) = grade.ensure_masked_secondary(klipp_nodes::NodeKind::Corrector);
        grade.nodes.iter_mut().find(|n| n.id == reader).unwrap().label = "Skin correction".into();
        c.set_grade(grade);
        let p = c.picture_v2().unwrap();
        let group = p.picture.graph.nodes.iter().find(|n| n.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|n| n.id == mask))).unwrap().id;
        let addr = at(c.id, &[group, mask]);
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, c.id);
        let outcome = graph_ops::graph_ops_edit(&s.sequences[0], &[GraphOp::RemoveNode { path: addr.clone() }]).unwrap();
        assert_eq!(outcome.ungated, vec![klipp_graph_lift::edit_planner::Ungated { path: at(c.id, &[group, reader]), label: "Skin correction".into() }]);
        let count = s.active_history().undo_count();
        run(&mut s, AppCommand::RemoveNodeAt { addr });
        assert_eq!(s.active_history().undo_count(), count + 1);
        assert_eq!(s.toast.message_for_test(), Some("1 node lost their mask: Skin correction"));
        undo(&mut s);
        assert_eq!(picture(&s, c.id), before);
    }

    /// A clip with a Corrector in its grade, and the Corrector's address:
    /// inside the clip's grade group.
    fn graded() -> (klipp_timeline::Clip, NodePath) {
        let mut c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let mut g = klipp_nodes::NodeGraph::default();
        g.ensure_corrector();
        let corrector = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        c.set_grade(g);
        let p = c.picture_v2().unwrap().picture.clone();
        let group = p.graph.nodes.iter().find(|n| n.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|i| i.id == corrector))).expect("the grade group holds the Corrector").id;
        let addr = at(c.id, &[group, corrector]);
        (c, addr)
    }

    /// A grade node's scalar parameter set by its bare id (`SetNodeParam`,
    /// `apply_override` on the graph) and by path (`SetParamAt` on
    /// `[group, node]`): the same picture, and one undo back. The Corrector
    /// named without its group is refused.
    #[test]
    fn a_grade_param_by_path_is_the_param_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        for (param, value) in [("exposure", 0.7f32), ("contrast", 0.2), ("saturation", 1.4)] {
            let mut a = with(std::slice::from_ref(&c));
            let seq = &mut a.sequences[0];
            let clip_a = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
            clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().params.apply_override(param, value));
            let mut b = with(std::slice::from_ref(&c));
            run(&mut b, AppCommand::SetParamAt { addr: addr.clone(), key: param.into(), value: ParamValue::Float(value) });
            assert_eq!(picture(&a, c.id), picture(&b, c.id), "{param}: the same picture");
            undo(&mut b);
            assert_eq!(picture(&b, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "{param}: undone");
        }
        let mut a = with(std::slice::from_ref(&c));
        let seq = &mut a.sequences[0];
        let clip_a = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().enabled = false);
        let mut b = with(std::slice::from_ref(&c));
        run(&mut b, AppCommand::SetNodeEnabledAt { addr: addr.clone(), enabled: false });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "toggled by path as by id");
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, c.id);
        assert_eq!(s.path_command(&AppCommand::SetParamAt { addr: at(c.id, &[node]), key: "exposure".into(), value: ParamValue::Float(0.5) }), Some(false), "the group skipped: refused");
        assert_eq!(picture(&s, c.id), before);
    }


    /// A grade node's struct parameter set whole by its bare id
    /// (`SetNodeQualifier`) and by path (`SetPayloadAt`, the node's stored
    /// entry with that one struct changed): the same picture.
    #[test]
    fn a_grade_struct_by_path_is_the_struct_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        let q = klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 40.0, ..Default::default() };
        let mut a = with(std::slice::from_ref(&c));
        let clip_a = a.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().params.qualifier = Some(q.clone()));
        let entry = |s: &AppState| {
            let p = clip(s, c.id).picture_v2().unwrap();
            let group = p.picture.graph.nodes.iter().find(|n| n.id == addr.nodes[0]).unwrap().clone();
            group.group.unwrap().graph.nodes.iter().find(|n| n.id == node).unwrap().legacy.clone().unwrap().payload
        };
        let mut b = with(std::slice::from_ref(&c));
        let mut payload = entry(&b);
        payload["params"]["qualifier"] = serde_json::to_value(&q).unwrap();
        run(&mut b, AppCommand::SetPayloadAt { addr: addr.clone(), payload });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "the qualifier by path as by id");
        undo(&mut b);
        assert_eq!(picture(&b, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "undone");
    }


    /// An effect's keys by path and by index: adding one (`AddKeyAt`,
    /// `AddEffectKeyframe`), moving it and setting its interpolation, and
    /// removing it give the same picture each time.
    #[test]
    fn effect_keys_by_path_are_the_keys_by_index() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&with(std::slice::from_ref(&c)), c.id)[0];
        let addr = at(c.id, &[node]);
        let key = |t: f64, v: f32, i: klipp_core::Interpolation| klipp_core::Keyframe { time_secs: t, value: v, interpolation: i, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
        let lin = klipp_core::Interpolation::Linear;
        // By index: the clip's own keyframe list for the effect.
        let by_index = |keys: Vec<klipp_core::Keyframe>| {
            let mut s = with(std::slice::from_ref(&c));
            let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
            let id = clip.effect_id_at(0).unwrap();
            clip.edit_effect_keyframes_of(id, "radius", |v| *v = keys);
            s
        };
        let mut s = with(std::slice::from_ref(&c));
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "radius".into(), keyframe: serde_json::to_value(key(0.5, 6.0, lin)).unwrap() });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(0.5, 6.0, lin)]), c.id), "added");
        run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 0.5, new_time_secs: 1.0, value: 8.0 });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(1.0, 8.0, lin)]), c.id), "moved");
        let hold = klipp_core::Interpolation::Step;
        run(&mut s, AppCommand::SetInterpKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 1.0, interpolation: hold });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(1.0, 8.0, hold)]), c.id), "interpolation");
        run(&mut s, AppCommand::RemoveKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 1.0 });
        assert_eq!(picture(&s, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "removed");
        assert_eq!(s.active_history().undo_count(), 4, "one step each");
        assert_eq!(s.path_command(&AppCommand::MoveKeyAt { addr, key: "radius".into(), time_secs: 3.0, new_time_secs: 1.0, value: 1.0 }), Some(false), "no key there: refused");
    }


    /// Setting a key's interpolation by path is the keyframe editor's rule,
    /// as by index: the key and the one before it change, and Bezier gives
    /// both tangents. Two and three keys, Step and Bezier, on the middle and
    /// the last key.
    #[test]
    fn interpolation_by_path_is_the_editors_rule() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&with(std::slice::from_ref(&c)), c.id)[0];
        let addr = at(c.id, &[node]);
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) };
        for keys in [vec![key(0.0, 2.0), key(1.0, 9.0)], vec![key(0.0, 2.0), key(1.0, 9.0), key(1.5, 4.0)]] {
            for interpolation in [klipp_core::Interpolation::Step, klipp_core::Interpolation::Bezier] {
                for index in 1..keys.len() {
                    let seeded = |keys: Vec<klipp_core::Keyframe>| {
                        let mut s = with(std::slice::from_ref(&c));
                        let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
                        let id = clip.effect_id_at(0).unwrap();
                        clip.edit_effect_keyframes_of(id, "radius", |v| *v = keys);
                        s
                    };
                    let mut want = keys.clone();
                    klipp_core::keyframe::set_interpolation_at(&mut want, index, interpolation);
                    let mut s = seeded(keys.clone());
                    run(&mut s, AppCommand::SetInterpKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: keys[index].time_secs, interpolation });
                    assert_eq!(picture(&s, c.id), picture(&seeded(want.clone()), c.id), "{} keys, {interpolation:?} at {index}", keys.len());
                    // The rule itself: the key before changes too, and Bezier
                    // gives both tangents.
                    assert_eq!(want[index - 1].interpolation, interpolation);
                    if interpolation == klipp_core::Interpolation::Bezier {
                        assert_ne!(want[index].tangent_in, (0.0, 0.0));
                        assert_ne!(want[index - 1].tangent_out, (0.0, 0.0));
                    }
                }
            }
        }
    }


    /// A path drag left open while a command of any other route arrives:
    /// the dispatch boundary closes the drag first, so the edit is its own
    /// step, neither taken as one of the drag's live steps nor folded into
    /// its snapshot. Every old effect command, a keyframe edit and a
    /// `GraphOps` batch close it; the drag's own commands and transport
    /// don't. Then, end to end: a drag, an old toggle, the drag's end are
    /// two steps, each undoing its own change.
    #[test]
    fn any_other_command_closes_an_open_path_drag() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&with(std::slice::from_ref(&c)), c.id);
        let blur = at(c.id, &[ids[0]]);
        let mut s = with(std::slice::from_ref(&c));
        assert!(!s.closes_path_drag(&AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 }), "no drag open, nothing to close");
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        let others = [
            AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 },
            AppCommand::RemoveBuiltinEffect { clip_id: c.id, index: 1 },
            AppCommand::ReorderBuiltinEffect { clip_id: c.id, from_index: 1, to_index: 0 },
            AppCommand::UpdateBuiltinEffect { clip_id: c.id, index: 1, effect: E::Sharpen { amount: 0.9 } },
            AppCommand::AddEffectKeyframe { clip_id: c.id, effect_index: 1, param_name: "amount".into(), time_secs: 0.5, value: 0.7 },
            AppCommand::GraphOps { ops: vec![GraphOp::SetEnabled { path: at(c.id, &[ids[1]]), enabled: false }], label: "x".into() },
            AppCommand::SetParamAt { addr: at(c.id, &[ids[1]]), key: "amount".into(), value: ParamValue::Float(0.7) },
            AppCommand::SetParamAt { addr: blur.clone(), key: "other".into(), value: ParamValue::Float(0.7) },
        ];
        for cmd in &others {
            assert!(s.closes_path_drag(cmd), "{cmd:?} closes the drag");
        }
        for cmd in [
            AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(5.0) },
            AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() },
            AppCommand::SeekTo(klipp_core::RationalTime::from_secs_f64(0.0, 25)),
            AppCommand::Play,
        ] {
            assert!(!s.closes_path_drag(&cmd), "{cmd:?} leaves the drag open");
        }
        // End to end: the drag's step, then an old toggle at the boundary.
        let original = picture(&s, c.id);
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        let dragged = picture(&s, c.id);
        let toggle = AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 };
        s.before_dispatch(&toggle);
        assert!(!s.active_history().is_any_drag_in_progress(), "closed before the toggle's handler runs");
        old(&mut s, c.id, EffectEdit::SetEnabled(1, false));
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), 2, "the drag and the toggle, two steps");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), dragged, "undo takes back the toggle alone");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), original, "then the drag");
        // The socket's route: a whole edit applied without passing the
        // dispatch boundary (`apply_edits_as_one` closes the drag first).
        let mut s = with(std::slice::from_ref(&c));
        let original = picture(&s, c.id);
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        let dragged = picture(&s, c.id);
        s.close_path_drag_before_edit();
        let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&s.sequences[0], &[GraphOp::SetEnabled { path: at(c.id, &[ids[1]]), enabled: false }]).unwrap();
        if let EditCommand::PictureEdit { clip_id, edit: e, .. } = &edit {
            s.set_picture(*clip_id, &e.after);
        }
        s.push_history(edit);
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), 2, "socket route: two steps");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), dragged, "socket route: the edit undoes alone");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), original, "socket route: then the drag");
    }

}
