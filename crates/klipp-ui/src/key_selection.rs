//! Several keys selected and edited together (#1958): on one clip, across
//! its tracks, named by time (an index shifts when keys reorder). Shift-
//! click adds a key or takes it out, a box in the curve editor selects
//! what it holds, and a selected key dragged moves every selected key by
//! the same time (and, on the curve shown, value), one undo step; Delete
//! and the key menu act on every selected key.

use crate::state::{AppState, KeyframeContextMenu, KeyframeParamKind, SelectedKeyframe};
use klipp_core::{AppCommand, ClipId, Keyframe};
use klipp_timeline::Clip;

/// A selected key: its track, and its time on the clip.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyRef {
    pub param_kind: KeyframeParamKind,
    pub time_secs: f64,
}

/// The keys selected together, on one clip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeySelection {
    pub clip_id: Option<ClipId>,
    pub keys: Vec<KeyRef>,
    /// A key was clicked this frame: the clip under it leaves its own
    /// selection be (a Shift-click on a key is the key's, not the clip's).
    /// The timeline clears it as each frame starts.
    pub clicked_now: bool,
}

impl KeySelection {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Whether the key of `kind` at `time` on `clip` is selected.
    pub fn holds(&self, clip: ClipId, kind: &KeyframeParamKind, time: f64) -> bool {
        self.clip_id == Some(clip) && self.keys.iter().any(|k| k.param_kind == *kind && same_time(k.time_secs, time))
    }

    /// Whether several keys are selected, among them that one: what makes
    /// a drag or a menu act on all of them.
    pub fn several_with(&self, clip: ClipId, kind: &KeyframeParamKind, time: f64) -> bool {
        self.len() > 1 && self.holds(clip, kind, time)
    }
}

fn same_time(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// The keys of track `kind` on `clip`, as each surface reads them.
pub fn track_keys(clip: &Clip, kind: &KeyframeParamKind) -> Vec<Keyframe> {
    match kind {
        KeyframeParamKind::Title { object_id, param } => param.keyframes(clip, *object_id).to_vec(),
        KeyframeParamKind::Audio(p) => match p {
            klipp_core::AudioParam::Volume => clip.volume_keyframes.clone(),
            klipp_core::AudioParam::Pan => clip.pan_keyframes.clone(),
            klipp_core::AudioParam::Gain => clip.gain_keyframes.clone(),
        },
        KeyframeParamKind::Video(p) => crate::inspector::placement_by_path::param_keys(clip, *p),
        KeyframeParamKind::AudioEffect { effect_index, param_name } => clip.audio_effect_keyframes.get(&format!("{effect_index}:{param_name}")).cloned().unwrap_or_default(),
        KeyframeParamKind::Node { path, key } => clip.node_key_state(path, key).map(|s| s.keys).unwrap_or_default(),
        KeyframeParamKind::Mask(track) => crate::mask_tracks::keys(clip, track),
    }
}

/// A click on the key of `kind` at `time` on `clip`: with `shift` added
/// to the selection, or taken out of it; without, the one key selected.
/// The primary key (`selected_keyframe`) follows: the key clicked, or,
/// when it was taken out, the selection's last.
pub fn click(state: &mut AppState, clip: &Clip, kind: &KeyframeParamKind, index: usize, shift: bool) {
    let Some(time) = track_keys(clip, kind).get(index).map(|k| k.time_secs) else { return };
    let sel = &mut state.key_selection;
    sel.clicked_now = true;
    if !shift || sel.clip_id != Some(clip.id) {
        // Shift on another clip starts afresh there.
        sel.clip_id = Some(clip.id);
        sel.keys = vec![KeyRef { param_kind: kind.clone(), time_secs: time }];
        state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: kind.clone(), keyframe_index: index });
        return;
    }
    // A plain click that started this selection may already hold the
    // key the primary names; Shift on it takes it out.
    if let Some(at) = sel.keys.iter().position(|k| k.param_kind == *kind && same_time(k.time_secs, time)) {
        sel.keys.remove(at);
        state.selected_keyframe = sel.keys.last().and_then(|k| primary(clip, k));
    } else {
        sel.keys.push(KeyRef { param_kind: kind.clone(), time_secs: time });
        state.selected_keyframe = Some(SelectedKeyframe { clip_id: clip.id, param_kind: kind.clone(), keyframe_index: index });
    }
}

/// `key` as the primary selection: its index on its track now.
fn primary(clip: &Clip, key: &KeyRef) -> Option<SelectedKeyframe> {
    let index = track_keys(clip, &key.param_kind).iter().position(|k| same_time(k.time_secs, key.time_secs))?;
    Some(SelectedKeyframe { clip_id: clip.id, param_kind: key.param_kind.clone(), keyframe_index: index })
}

/// Nothing selected: an empty click.
pub fn clear(state: &mut AppState) {
    state.key_selection = KeySelection::default();
    state.selected_keyframe = None;
}

/// The keys of `kind` on `clip` inside the box from `(t0, v0)` to
/// `(t1, v1)` selected: added to the selection with `add`, else in its
/// place.
pub fn select_box(state: &mut AppState, clip: &Clip, kind: &KeyframeParamKind, (t0, v0): (f64, f32), (t1, v1): (f64, f32), add: bool) {
    let (ta, tb) = (t0.min(t1), t0.max(t1));
    let (va, vb) = (v0.min(v1), v0.max(v1));
    let inside: Vec<KeyRef> = track_keys(clip, kind)
        .into_iter()
        .filter(|k| k.time_secs >= ta && k.time_secs <= tb && k.value >= va && k.value <= vb)
        .map(|k| KeyRef { param_kind: kind.clone(), time_secs: k.time_secs })
        .collect();
    let sel = &mut state.key_selection;
    if !add || sel.clip_id != Some(clip.id) {
        sel.clip_id = Some(clip.id);
        sel.keys.clear();
    }
    for k in inside {
        if !sel.keys.contains(&k) {
            sel.keys.push(k);
        }
    }
    state.selected_keyframe = sel.keys.last().and_then(|k| primary(clip, k));
}

/// A drag of the selected keys: where each was when it began, and how
/// far the drag has moved them so far.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiKeyDrag {
    pub clip_id: ClipId,
    /// Each key's track, and its time and value when the drag began.
    pub keys: Vec<(KeyframeParamKind, f64, f32)>,
    /// The track whose keys move in value too (the curve editor's shown
    /// one); none moves in value on the timeline.
    pub value_track: Option<KeyframeParamKind>,
    /// The time and value moves the keys stand at.
    pub dt: f64,
    pub dv: f32,
    /// On the timeline, where the press was across, and the clip's pixels
    /// a second: the drag's time from the pointer.
    pub anchor_x: Option<f32>,
    pub px_per_sec: f32,
    /// The primary key's track and time as the drag began: its index
    /// named again where it sorts once let go.
    pub primary: Option<(KeyframeParamKind, f64)>,
}

/// The drag of the selection begun: the press's batch opened, so the whole
/// drag is one undo step.
pub fn begin_drag(state: &mut AppState, clip: &Clip, value_track: Option<KeyframeParamKind>) -> Vec<AppCommand> {
    let keys = state
        .key_selection
        .keys
        .iter()
        .filter_map(|k| track_keys(clip, &k.param_kind).into_iter().find(|kf| same_time(kf.time_secs, k.time_secs)).map(|kf| (k.param_kind.clone(), kf.time_secs, kf.value)))
        .collect();
    let primary = state
        .selected_keyframe
        .as_ref()
        .filter(|sk| sk.clip_id == clip.id)
        .and_then(|sk| track_keys(clip, &sk.param_kind).get(sk.keyframe_index).map(|k| (sk.param_kind.clone(), k.time_secs)));
    state.multi_key_drag = Some(MultiKeyDrag { clip_id: clip.id, keys, value_track, dt: 0.0, dv: 0.0, anchor_x: None, px_per_sec: 1.0, primary });
    vec![AppCommand::BeginHistoryBatch]
}

/// The drag at `dt` seconds (and `dv` on its value track) from where it
/// began: each key moved from where it stands to its place, by its own
/// track's command, or nothing when any key would land on an unselected
/// key's time on its track, or outside the clip.
pub fn drag_to(state: &mut AppState, clip: &Clip, dt: f64, dv: f32) -> Vec<AppCommand> {
    let Some(drag) = state.multi_key_drag.clone() else { return Vec::new() };
    if drag.clip_id != clip.id || (same_time(dt, drag.dt) && (dv - drag.dv).abs() < 1e-7) {
        return Vec::new();
    }
    let duration = clip.duration().to_secs_f64();
    // Every key's new time inside the clip, and clear of the keys left.
    for (kind, t0, _) in &drag.keys {
        let to = t0 + dt;
        if !(0.0..=duration + 1e-9).contains(&to) {
            return Vec::new();
        }
        let moving: Vec<f64> = drag.keys.iter().filter(|(k, ..)| k == kind).map(|(_, t, _)| t + drag.dt).collect();
        let still = track_keys(clip, kind).into_iter().filter(|k| !moving.iter().any(|m| same_time(*m, k.time_secs)));
        if still.into_iter().any(|k| same_time(k.time_secs, to)) {
            return Vec::new();
        }
    }
    // Later keys first when moving later, earlier first when earlier, so
    // no key is moved onto one of its own that hasn't moved yet, and an
    // index read before a move still names its key.
    let mut order: Vec<&(KeyframeParamKind, f64, f32)> = drag.keys.iter().collect();
    order.sort_by(|a, b| if dt >= drag.dt { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) });
    // A sound key's move records no step of its own (a lane's drag holds
    // the clip's keys around it); the others' each do, into the batch.
    // So the sound keys' moves go first, held as one keyframe drag.
    let (mut held, mut rest) = (Vec::new(), Vec::new());
    for (kind, t0, v0) in order {
        let (from, to) = (t0 + drag.dt, t0 + dt);
        let value = if drag.value_track.as_ref() == Some(kind) { Some(v0 + dv) } else { None };
        let bucket = if matches!(kind, KeyframeParamKind::Audio(_) | KeyframeParamKind::AudioEffect { .. }) { &mut held } else { &mut rest };
        bucket.extend(move_command(clip, kind, from, to, value));
    }
    let mut cmds = Vec::new();
    if !held.is_empty() {
        let key = format!("kf:keys:{}", clip.id.0);
        cmds.push(AppCommand::BeginKeyframeDrag { key: key.clone(), clip_id: clip.id });
        cmds.extend(held);
        cmds.push(AppCommand::EndKeyframeDrag { key, clip_id: clip.id });
    }
    cmds.extend(rest);
    if let Some(d) = state.multi_key_drag.as_mut() {
        d.dt = dt;
        d.dv = dv;
    }
    cmds
}

/// The drag let go: its batch closed, the selection at the keys' new
/// times.
pub fn end_drag(state: &mut AppState) -> Vec<AppCommand> {
    let Some(drag) = state.multi_key_drag.take() else { return Vec::new() };
    state.key_selection.keys = drag.keys.iter().map(|(k, t, _)| KeyRef { param_kind: k.clone(), time_secs: t + drag.dt }).collect();
    if let Some((kind, t0)) = drag.primary {
        let index = state
            .sequence()
            .all_clips()
            .find(|c| c.id == drag.clip_id)
            .and_then(|c| track_keys(c, &kind).iter().position(|k| same_time(k.time_secs, t0 + drag.dt)));
        if let Some(keyframe_index) = index {
            state.selected_keyframe = Some(SelectedKeyframe { clip_id: drag.clip_id, param_kind: kind, keyframe_index });
        }
    }
    vec![AppCommand::EndHistoryBatch]
}

/// The command moving the key of `kind` at `from` on `clip` to `to`, its
/// value to `value` when given, else kept.
pub(crate) fn move_command(clip: &Clip, kind: &KeyframeParamKind, from: f64, to: f64, value: Option<f32>) -> Option<AppCommand> {
    let keys = track_keys(clip, kind);
    let index = keys.iter().position(|k| same_time(k.time_secs, from))?;
    Some(move_at(clip.id, kind, index, from, to, value.unwrap_or(keys[index].value)))
}

/// The commands moving several keys of `kind` on `clip`, each `(from,
/// to, value)` in the order given (its value kept where `None`): a key
/// named by its index is named as the moves before it left the keys, so
/// a key that passes another is still the one meant.
pub(crate) fn move_commands(clip: &Clip, kind: &KeyframeParamKind, moves: &[(f64, f64, Option<f32>)]) -> Vec<AppCommand> {
    let mut keys = track_keys(clip, kind);
    let mut out = Vec::new();
    for &(from, to, value) in moves {
        let Some(index) = keys.iter().position(|k| same_time(k.time_secs, from)) else { continue };
        let new_value = value.unwrap_or(keys[index].value);
        out.push(move_at(clip.id, kind, index, from, to, new_value));
        keys[index].time_secs = to;
        keys[index].value = new_value;
        keys.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
    }
    out
}

/// The command moving the key of `kind` at `index` (its time `from`) to
/// `to` with `new_value`.
fn move_at(clip_id: klipp_core::ClipId, kind: &KeyframeParamKind, index: usize, from: f64, to: f64, new_value: f32) -> AppCommand {
    match kind {
        KeyframeParamKind::Video(param) => AppCommand::MoveVideoKeyframe { clip_id, param: *param, index, new_time_secs: to, new_value },
        KeyframeParamKind::Audio(param) => AppCommand::MoveAudioKeyframe { clip_id, param: *param, index, new_time_secs: to, new_value },
        KeyframeParamKind::AudioEffect { effect_index, param_name } => AppCommand::MoveAudioEffectKeyframe { clip_id, effect_index: *effect_index, param_name: param_name.clone(), keyframe_index: index, new_time_secs: to, new_value },
        KeyframeParamKind::Title { object_id, param } => param.move_keyframe(clip_id, *object_id, index, to, new_value),
        KeyframeParamKind::Node { path, key } => AppCommand::MoveKeyAt { addr: path.clone(), key: key.clone(), time_secs: from, new_time_secs: to, value: new_value },
        KeyframeParamKind::Mask(track) => AppCommand::MoveMaskKeyframe { clip_id, node_id: track.node, mask_index: track.mask, param: track.param.clone(), time_secs: from, new_time_secs: to, value: (!track.is_shape()).then_some(new_value) },
    }
}

/// The keys a key menu opened on `menu`'s key acts on: every selected key
/// when several are and it is one of them, else that key alone. Each as
/// the menu names a key: by its index on its track now.
pub fn menu_targets(state: &AppState, menu: &KeyframeContextMenu) -> Vec<KeyframeContextMenu> {
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == menu.clip_id) else { return vec![menu.clone()] };
    let time = track_keys(clip, &menu.param_kind).get(menu.keyframe_index).map(|k| k.time_secs);
    match time {
        Some(t) if several(state) && state.key_selection.several_with(clip.id, &menu.param_kind, t) => state
            .key_selection
            .keys
            .iter()
            .filter_map(|k| {
                let index = track_keys(clip, &k.param_kind).iter().position(|kf| same_time(kf.time_secs, k.time_secs))?;
                Some(KeyframeContextMenu { clip_id: clip.id, param_kind: k.param_kind.clone(), keyframe_index: index, screen_pos: menu.screen_pos })
            })
            .collect(),
        _ => vec![menu.clone()],
    }
}

/// Whether several keys are selected and the primary key is one of them.
/// A surface that set the primary alone (a click there that didn't go
/// through [`click`]) leaves a selection that isn't the person's any
/// more; it is then not acted on.
pub fn several(state: &AppState) -> bool {
    let sel = &state.key_selection;
    let Some(sk) = &state.selected_keyframe else { return false };
    if sel.len() < 2 || sel.clip_id != Some(sk.clip_id) {
        return false;
    }
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == sk.clip_id) else { return false };
    track_keys(clip, &sk.param_kind).get(sk.keyframe_index).is_some_and(|k| sel.holds(clip.id, &sk.param_kind, k.time_secs))
}

/// The keys an ease shortcut acts on (#2307), each as a key menu names
/// it: every selected key when several are, else the one selected (on the
/// timeline, in the Curve Editor or the Dope Sheet), else the key at the
/// playhead of the parameter the Inspector has in focus.
pub fn ease_targets(state: &AppState) -> Vec<KeyframeContextMenu> {
    let at = |sk: &SelectedKeyframe| KeyframeContextMenu { clip_id: sk.clip_id, param_kind: sk.param_kind.clone(), keyframe_index: sk.keyframe_index, screen_pos: egui::Pos2::ZERO };
    if let Some(sk) = &state.selected_keyframe {
        return menu_targets(state, &at(sk));
    }
    let Some(focus) = &state.param_focus else { return Vec::new() };
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == focus.clip_id) else { return Vec::new() };
    let t = state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
    let keys = track_keys(clip, &focus.kind);
    let tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    crate::inspector::keyframe_selection::nearest(&keys, t, tolerance)
        .map(|keyframe_index| vec![at(&SelectedKeyframe { clip_id: clip.id, param_kind: focus.kind.clone(), keyframe_index })])
        .unwrap_or_default()
}

/// What Delete takes when several keys are selected: every one, in one
/// batch, one step. `None` when fewer than two are.
pub fn delete_all(state: &AppState) -> Option<Vec<AppCommand>> {
    let sel = &state.key_selection;
    if !several(state) {
        return None;
    }
    let clip = state.sequence().all_clips().find(|c| Some(c.id) == sel.clip_id)?;
    let mut cmds = vec![AppCommand::BeginHistoryBatch];
    for k in &sel.keys {
        let Some(index) = track_keys(clip, &k.param_kind).iter().position(|kf| same_time(kf.time_secs, k.time_secs)) else { continue };
        let sk = SelectedKeyframe { clip_id: clip.id, param_kind: k.param_kind.clone(), keyframe_index: index };
        cmds.extend(crate::app::remove_selected_keyframe_for(state, &sk).unwrap_or_default());
    }
    cmds.push(AppCommand::EndHistoryBatch);
    Some(cmds)
}

/// A timeline lane's click on its key `index` of `kind` on clip `clip_id`
/// (#1958): Shift adds it or takes it out, else it alone is selected.
pub fn lane_click(state: &mut AppState, clip_id: ClipId, kind: KeyframeParamKind, index: usize, shift: bool) {
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else { return };
    click(state, &clip, &kind, index, shift);
}

/// A timeline lane's press on its key `index` of `kind`, as a drag starts:
/// when several keys are selected with it, their drag, in time only, at
/// `px_per_sec` from `anchor_x`, its batch opened. `None` for a drag of
/// the one key, which the lane's own gesture takes.
pub fn lane_drag_begin(state: &mut AppState, clip_id: ClipId, kind: &KeyframeParamKind, index: usize, anchor_x: f32, px_per_sec: f32) -> Option<Vec<AppCommand>> {
    let clip = state.sequence().all_clips().find(|c| c.id == clip_id).cloned()?;
    let time = track_keys(&clip, kind).get(index)?.time_secs;
    if !several(state) || !state.key_selection.holds(clip_id, kind, time) {
        return None;
    }
    let cmds = begin_drag(state, &clip, None);
    if let Some(d) = state.multi_key_drag.as_mut() {
        d.anchor_x = Some(anchor_x);
        d.px_per_sec = px_per_sec.max(1e-3);
    }
    Some(cmds)
}

/// A timeline frame of the selection's drag on clip `clip_id`: the keys
/// at the pointer's time while it is down, the batch closed when let go.
pub fn lane_drag_frame(state: &mut AppState, clip_id: ClipId, pointer_x: Option<f32>, down: bool) -> Vec<AppCommand> {
    let Some(drag) = state.multi_key_drag.clone().filter(|d| d.clip_id == clip_id) else { return Vec::new() };
    let Some(anchor) = drag.anchor_x else { return Vec::new() };
    if !down {
        return end_drag(state);
    }
    let Some(x) = pointer_x else { return Vec::new() };
    let Some(clip) = state.sequence().all_clips().find(|c| c.id == clip_id).cloned() else { return Vec::new() };
    drag_to(state, &clip, ((x - anchor) / drag.px_per_sec) as f64, 0.0)
}

/// Whether the key of `kind` at `time` on `clip_id` is one of several
/// selected: drawn marked, as the primary is.
pub fn marked(state: &AppState, clip_id: ClipId, kind: &KeyframeParamKind, time: f64) -> bool {
    state.key_selection.len() > 1 && state.key_selection.holds(clip_id, kind, time)
}

/// Which of `keys`, a track of `kind` on `clip_id`, are among several
/// selected.
pub fn marked_indices(state: &AppState, clip_id: ClipId, kind: &KeyframeParamKind, keys: &[Keyframe]) -> Vec<usize> {
    keys.iter().enumerate().filter(|(_, k)| marked(state, clip_id, kind, k.time_secs)).map(|(i, _)| i).collect()
}
