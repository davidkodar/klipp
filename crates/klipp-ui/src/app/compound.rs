//! Compound clips: made from a selection, taken apart again, and grouped
//! from or ungrouped into a compound (#826).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    pub(super) fn create_compound_clip_impl(&mut self, name: String) {
        let mut target_clips: Vec<klipp_core::ClipId> = self.state.selected_clips.clone();
        if let Some(primary) = self.state.selected_clip {
            if !target_clips.contains(&primary) {
                target_clips.push(primary);
            }
        }
        if target_clips.is_empty() {
            self.state.toast.show("Select clips first".to_string());
            return;
        }

        let active_idx = self.state.active_sequence_idx;
        let fps = self.state.sequences[active_idx].fps;
        let res = self.state.sequences[active_idx].resolution;

        // Collect the clips and find the time range
        let seq = &self.state.sequences[active_idx];
        let mut collected: Vec<(usize, usize, klipp_timeline::Clip)> = Vec::new();
        for (ti, track) in seq.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                if target_clips.contains(&clip.id) {
                    collected.push((ti, ci, clip.clone()));
                }
            }
        }
        if collected.is_empty() { return; }

        let range_start = collected.iter().map(|(_, _, c)| c.timeline_in).min().unwrap();
        let range_end = collected.iter().map(|(_, _, c)| c.timeline_out).max().unwrap();

        // Create new sequence with same fps/resolution
        let mut nested_seq = klipp_timeline::Sequence {
            // A new sequence starts without a camera; a nest that goes
            // back into its parent is moved by the parent's.
            camera: klipp_timeline::Camera::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            buses: Vec::new(),
            drop_frame: klipp_timeline::Sequence::default_drop_frame(fps),
            id: klipp_core::SequenceId::new(),
            name: name.clone(),
            fps,
            resolution: res,
            first_clip_autoset: klipp_timeline::FirstClipAutoSet { resolution: false, fps: false, rate_answered: false },
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        };

        // Reproduce track structure and move clips (offset to start at 0)
        let parent_seq = &self.state.sequences[active_idx];
        for (ti, track) in parent_seq.tracks.iter().enumerate() {
            let mut new_track = klipp_timeline::Track::new(&track.name, track.kind);
            for (_, _, clip) in collected.iter().filter(|(t, _, _)| *t == ti) {
                let mut c = clip.clone();
                c.timeline_in -= range_start;
                c.timeline_out -= range_start;
                new_track.clips.push(c);
            }
            if !new_track.clips.is_empty() {
                new_track.sort_clips();
                nested_seq.tracks.push(new_track);
            }
        }

        let nested_seq_id = nested_seq.id;
        let parent_seq_id = self.state.sequences[active_idx].id;
        self.state.sequences.push(nested_seq);

        // Refuse to create a circular reference: if the new nested sequence
        // (which contains the moved clips, possibly including other compound
        // clips) transitively references the parent, abort and roll back.
        if would_create_cycle(nested_seq_id, parent_seq_id, &self.state.sequences) {
            self.state.sequences.pop();
            self.state
                .toast
                .show("Cannot create compound clip: this would create a circular reference.");
            return;
        }

        // Remove original clips from parent and insert compound clip
        let parent_seq = &mut self.state.sequences[active_idx];
        // Find the first track that had selected clips
        let first_track_idx = collected.first().map(|(ti, _, _)| *ti).unwrap_or(0);
        for (ti, _, clip) in &collected {
            if let Some(track) = parent_seq.tracks.get_mut(*ti) {
                track.clips.retain(|c| c.id != clip.id);
            }
        }
        // Insert compound clip on the first track at the range start
        let compound_clip = klipp_timeline::Clip::sequence(nested_seq_id, range_start, range_end);
        let compound_id = compound_clip.id;
        if let Some(track) = parent_seq.tracks.get_mut(first_track_idx) {
            track.clips.push(compound_clip);
            track.sort_clips();
        }

        self.state.selected_clip = Some(compound_id);
        self.state.selected_clips.clear();
        self.state.toast.show(format!("Created compound clip \"{}\"", name));
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Replace a compound clip on the active sequence with its inner clips at
    /// their original positions, then delete the now-orphaned inner sequence.
    /// Inverse of `create_compound_clip_impl`.
    ///
    /// Honours the compound's source-window trim, speed, and per-clip effects:
    /// - inner clips outside the trim are dropped
    /// - inner clips at the trim boundaries are sliced (via
    ///   `klipp_timeline::flatten::slice_inner_clip_to_compound`)
    /// - `compound.speed` is multiplied into each restored leaf's `speed`,
    ///   and the parent-timeline interval is scaled accordingly
    /// - any non-default `node_graph`, `effects`, `builtin_effects`,
    ///   `*_keyframes`, or `param_modifiers` on the compound are carried over
    ///   to a new `ClipKind::AdjustmentLayer` clip placed on a video track
    ///   above the restored inner clips
    ///
    /// KNOWN LIMITATIONS:
    /// 1. `compound.speed_keyframes` (variable speed ramp) are dropped — baking
    ///    a time-varying envelope into each inner clip is out of scope. A toast
    ///    warns the user when this applies.
    /// 2. Audio effects on the compound (`audio_effects`, non-default `volume`
    ///    `gain`, `pan`, fades) are dropped — adjustment layers don't apply to
    ///    audio in Klipp's data model. Toast warns.
    /// 3. `compound.source_rotation` is dropped — minor visual difference.
    /// 4. `linked_clip_id` on inner clips: if two compound instances share the
    ///    same inner sequence, decomposing both may produce duplicate ClipIds
    ///    with `linked_clip_id` pointing at the wrong partner. Rare edge case.
    pub(super) fn decompose_compound_clip_impl(&mut self, clip_id: klipp_core::ClipId) {
        let active_idx = self.state.active_sequence_idx;

        // 1. Find the compound clip and snapshot every field we'll need before
        //    mutating either the parent or the inner sequence.
        struct CompoundSnapshot {
            parent_track_idx: usize,
            parent_clip_idx: usize,
            timeline_in: klipp_core::Timecode,
            source_in: klipp_core::Timecode,
            source_out: klipp_core::Timecode,
            speed: f64,
            inner_seq_id: klipp_core::SequenceId,
            // Effect-side fields carried into the adjustment layer if any
            node_graph: klipp_nodes::NodeGraph,
            builtin_effects: Vec<klipp_effects::BuiltinEffect>,
            builtin_effects_enabled: Vec<bool>,
            effects: Vec<klipp_plugin::AppliedEffect>,
            effect_keyframes:
                std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
            opacity_keyframes: Vec<klipp_core::Keyframe>,
            transform_pos_x_keyframes: Vec<klipp_core::Keyframe>,
            transform_pos_y_keyframes: Vec<klipp_core::Keyframe>,
            transform_scale_keyframes: Vec<klipp_core::Keyframe>,
            transform_scale_y_keyframes: Vec<klipp_core::Keyframe>,
            transform_rotation_keyframes: Vec<klipp_core::Keyframe>,
            param_modifiers:
                std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
            // Limitation-warning fields
            has_speed_keyframes: bool,
            audio_effects_count: usize,
            volume: f32,
            gain: f32,
            pan: f32,
            fade_in_secs: f32,
            fade_out_secs: f32,
            // A picture-truth compound clip, whose picture (masks, mixes)
            // the adjustment layer takes.
            truth_source: Option<klipp_timeline::Clip>,
        }
        let snap: CompoundSnapshot = {
            let parent = &self.state.sequences[active_idx];
            let mut found: Option<CompoundSnapshot> = None;
            'outer: for (ti, track) in parent.tracks.iter().enumerate() {
                for (ci, c) in track.clips.iter().enumerate() {
                    if c.id == clip_id {
                        if let klipp_timeline::ClipKind::Sequence { sequence_id, .. } = &c.kind {
                            found = Some(CompoundSnapshot {
                                parent_track_idx: ti,
                                parent_clip_idx: ci,
                                timeline_in: c.timeline_in,
                                source_in: c.source_in,
                                source_out: c.source_out,
                                speed: c.speed,
                                inner_seq_id: *sequence_id,
                                node_graph: c.grade().clone(),
                                builtin_effects: c.builtin_effects().to_vec(),
                                builtin_effects_enabled: c.builtin_effects_enabled().to_vec(),
                                effects: c.ofx_effects().to_vec(),
                                effect_keyframes: c.effect_keyframes().clone(),
                                opacity_keyframes: c.opacity_keyframes().to_vec(),
                                transform_pos_x_keyframes: c.transform_pos_x_keyframes().to_vec(),
                                transform_pos_y_keyframes: c.transform_pos_y_keyframes().to_vec(),
                                transform_scale_keyframes: c.transform_scale_keyframes().to_vec(),
                                transform_scale_y_keyframes: c.transform_scale_y_keyframes().to_vec(),
                                transform_rotation_keyframes: c
                                    .transform_rotation_keyframes()
                                    .to_vec(),
                                param_modifiers: c.param_modifiers().clone(),
                                has_speed_keyframes: !c.speed_keyframes.is_empty(),
                                audio_effects_count: c.audio_effects.len(),
                                volume: c.volume,
                                gain: c.gain,
                                pan: c.pan,
                                fade_in_secs: c.fade_in_secs,
                                fade_out_secs: c.fade_out_secs,
                                truth_source: c.is_picture_truth().then(|| c.clone()),
                            });
                        }
                        break 'outer;
                    }
                }
            }
            match found {
                Some(v) => v,
                None => return,
            }
        };

        // 2. Snapshot the inner sequence's clips before any mutation.
        let inner_idx = match self
            .state
            .sequences
            .iter()
            .position(|s| s.id == snap.inner_seq_id)
        {
            Some(i) => i,
            None => {
                self.remove_clip_impl(clip_id);
                self.state.toast.show(
                    "Compound clip removed (inner sequence was missing).",
                );
                return;
            }
        };
        let inner_track_blueprint: Vec<(
            klipp_timeline::TrackKind,
            String,
            Vec<klipp_timeline::Clip>,
        )> = self.state.sequences[inner_idx]
            .tracks
            .iter()
            .map(|t| (t.kind, t.name.clone(), t.clips.clone()))
            .collect();

        // 3. Remove the compound clip from the parent in place (no ripple).
        {
            let parent = &mut self.state.sequences[active_idx];
            if let Some(track) = parent.tracks.get_mut(snap.parent_track_idx) {
                if snap.parent_clip_idx < track.clips.len()
                    && track.clips[snap.parent_clip_idx].id == clip_id
                {
                    track.clips.remove(snap.parent_clip_idx);
                }
            }
        }

        // 4. Slice each inner clip to the compound's source window and place
        //    it on a parent track of matching kind. The slice helper handles
        //    trim, speed propagation (parent interval + leaf .speed), and
        //    drops clips that lie entirely outside the trim window.
        let compound_in_secs = snap.timeline_in.to_secs_f64();
        let compound_source_in_s = snap.source_in.to_secs_f64();
        let compound_source_out_s = snap.source_out.to_secs_f64();
        let rate = snap.timeline_in.rate;

        for (kind, src_name, inner_clips) in inner_track_blueprint {
            let sliced: Vec<klipp_timeline::Clip> = inner_clips
                .iter()
                .filter_map(|c| {
                    klipp_timeline::flatten::slice_inner_clip_to_compound(
                        c,
                        compound_in_secs,
                        compound_source_in_s,
                        compound_source_out_s,
                        snap.speed,
                        rate,
                    )
                })
                .filter(|c| !matches!(c.kind, klipp_timeline::ClipKind::Gap))
                .collect();

            if sliced.is_empty() {
                continue;
            }

            let block_start = sliced
                .iter()
                .map(|c| c.timeline_in.to_secs_f64())
                .fold(f64::INFINITY, f64::min);
            let block_end = sliced
                .iter()
                .map(|c| c.timeline_out.to_secs_f64())
                .fold(f64::NEG_INFINITY, f64::max);

            // Find an existing parent track of matching kind with room (no
            // clip overlapping the sliced block); otherwise create a new one.
            let parent = &self.state.sequences[active_idx];
            let target_track_idx = parent
                .tracks
                .iter()
                .enumerate()
                .find(|(_, t)| {
                    t.kind == kind
                        && t.clips.iter().all(|c| {
                            let a = c.timeline_in.to_secs_f64();
                            let b = c.timeline_out.to_secs_f64();
                            b <= block_start || a >= block_end
                        })
                })
                .map(|(i, _)| i);

            let target_track_idx = match target_track_idx {
                Some(i) => i,
                None => {
                    let parent = &mut self.state.sequences[active_idx];
                    let new_track = klipp_timeline::Track::new(&src_name, kind);
                    match kind {
                        klipp_timeline::TrackKind::Video => {
                            parent.tracks.insert(0, new_track);
                        }
                        klipp_timeline::TrackKind::Audio
                        | klipp_timeline::TrackKind::Text
                        | klipp_timeline::TrackKind::Subtitle => {
                            parent.tracks.push(new_track);
                        }
                    };
                    parent.renumber_tracks();
                    parent
                        .tracks
                        .iter()
                        .position(|t| t.kind == kind && t.clips.is_empty())
                        .unwrap_or(0)
                }
            };

            let parent = &mut self.state.sequences[active_idx];
            if let Some(track) = parent.tracks.get_mut(target_track_idx) {
                for clip in sliced {
                    track.clips.push(clip);
                }
                track.sort_clips();
            }
        }

        // 5. Compound-level effects → adjustment layer above the restored
        //    clips. Skip when the compound had no effects to carry.
        let needs_adj_layer = !snap.node_graph.is_identity()
            || !snap.builtin_effects.is_empty()
            || !snap.effects.is_empty()
            || !snap.opacity_keyframes.is_empty()
            || !snap.transform_pos_x_keyframes.is_empty()
            || !snap.transform_pos_y_keyframes.is_empty()
            || !snap.transform_scale_keyframes.is_empty()
            || !snap.transform_scale_y_keyframes.is_empty()
            || !snap.transform_rotation_keyframes.is_empty()
            || !snap.effect_keyframes.is_empty()
            || !snap.param_modifiers.is_empty();

        if needs_adj_layer {
            // Adjustment layer spans the compound's sliced parent-timeline
            // range (start = compound_in; end = compound_in + window/speed).
            let speed = if snap.speed.abs() < 1e-9 { 1.0 } else { snap.speed };
            let adj_in_secs = compound_in_secs;
            let adj_out_secs =
                adj_in_secs + (compound_source_out_s - compound_source_in_s) / speed;
            let adj_in = klipp_core::RationalTime::from_secs_f64(adj_in_secs, rate);
            let adj_out = klipp_core::RationalTime::from_secs_f64(adj_out_secs, rate);

            let mut adj_clip = klipp_timeline::Clip {
                id: klipp_core::ClipId::new(),
                kind: klipp_timeline::ClipKind::AdjustmentLayer,
                timeline_in: adj_in,
                timeline_out: adj_out,
                source_in: klipp_core::RationalTime::from_secs_f64(0.0, rate),
                source_out: klipp_core::RationalTime::from_secs_f64(
                    adj_out_secs - adj_in_secs,
                    rate,
                ),
                picture: klipp_timeline::ClipPictureParts {
                    builtin_effects: snap.builtin_effects,
                    builtin_effects_enabled: snap.builtin_effects_enabled,
                    ofx_effects: snap.effects,
                    effect_keyframes: snap.effect_keyframes,
                    opacity_keyframes: snap.opacity_keyframes,
                    transform_pos_x_keyframes: snap.transform_pos_x_keyframes,
                    transform_pos_y_keyframes: snap.transform_pos_y_keyframes,
                    transform_scale_keyframes: snap.transform_scale_keyframes,
                    transform_scale_y_keyframes: snap.transform_scale_y_keyframes,
                    transform_rotation_keyframes: snap.transform_rotation_keyframes,
                    param_modifiers: snap.param_modifiers,
                    ..Default::default()
                }
                .into(),
                ..klipp_timeline::Clip::gap(adj_in, adj_out)
            };

            adj_clip.set_grade(snap.node_graph);

            // Place on a video track at index 0 so the compositor processes
            // it AFTER (above) the restored inner clips. If the existing top
            // video track has room at the adjustment range, reuse it;
            // otherwise insert a new "Decomposed Adjustments" track at idx 0.
            let parent = &mut self.state.sequences[active_idx];
            let topmost_has_room = parent
                .tracks
                .first()
                .is_some_and(|t| {
                    t.kind == klipp_timeline::TrackKind::Video
                        && t.clips.iter().all(|c| {
                            c.timeline_out.to_secs_f64() <= adj_in_secs
                                || c.timeline_in.to_secs_f64() >= adj_out_secs
                        })
                });
            let adj_track_idx = if topmost_has_room {
                0
            } else {
                let new_track = klipp_timeline::Track::new(
                    "Decomposed Adjustments",
                    klipp_timeline::TrackKind::Video,
                );
                parent.tracks.insert(0, new_track);
                parent.renumber_tracks();
                0
            };
            if let Some(source) = &snap.truth_source {
                adj_clip.take_picture_truth_of(source);
            }
            parent.tracks[adj_track_idx].clips.push(adj_clip);
            parent.tracks[adj_track_idx].sort_clips();
        }

        // 6. Surface limitations the user might care about as a toast.
        let has_audio_effects = snap.audio_effects_count > 0
            || (snap.volume - 1.0).abs() > 1e-6
            || (snap.gain - 1.0).abs() > 1e-6
            || snap.pan.abs() > 1e-6
            || snap.fade_in_secs > 0.0
            || snap.fade_out_secs > 0.0;
        if snap.has_speed_keyframes {
            self.state.toast.show(
                "Note: variable speed (speed ramp) on the compound clip was not restored. Re-apply manually.",
            );
        } else if has_audio_effects {
            self.state.toast.show(
                "Note: audio effects on the compound clip were not restored. Re-apply on the audio tracks.",
            );
        } else if needs_adj_layer {
            self.state.toast.show(
                "Compound clip decomposed — inner clips restored. Effects carried to an adjustment layer above.",
            );
        } else {
            self.state
                .toast
                .show("Compound clip decomposed — inner clips restored.");
        }

        // 7. Delete the now-orphaned inner sequence (no other compound
        //    references it). Skip if it's the project's last sequence.
        let still_referenced = self.state.sequences.iter().enumerate().any(|(i, seq)| {
            i != inner_idx
                && seq.tracks.iter().any(|t| {
                    t.clips.iter().any(|c| {
                        matches!(c.kind, klipp_timeline::ClipKind::Sequence {
                            sequence_id, ..
                        } if sequence_id == snap.inner_seq_id)
                    })
                })
        });
        if !still_referenced && self.state.sequences.len() > 1 {
            if let Some(inner_idx) = self
                .state
                .sequences
                .iter()
                .position(|s| s.id == snap.inner_seq_id)
            {
                self.state.sequences.remove(inner_idx);
                self.state
                    .compound_clip_stack
                    .retain(|(idx, _)| *idx != inner_idx);
                for entry in self.state.compound_clip_stack.iter_mut() {
                    if entry.0 > inner_idx {
                        entry.0 -= 1;
                    }
                }
                if self.state.active_sequence_idx >= self.state.sequences.len() {
                    self.state.active_sequence_idx = self.state.sequences.len() - 1;
                } else if self.state.active_sequence_idx > inner_idx {
                    self.state.active_sequence_idx -= 1;
                }
                self.state.fps = self.state.sequences[self.state.active_sequence_idx].fps;
            }
        }

        self.state.selected_clip = None;
        self.state.selected_clips.clear();
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }
}

/// The v1 canvas's Group into Compound on `old_graph`, kept to hold the
/// path op's result to the lift of its result (#1476): `inside` packed into
/// a new Compound `compound_id`, its subgraph's Source and Output `ends`.
/// Single input and single output only; Source / Output stay out.
#[cfg(test)]
pub(super) fn group_v1(
    old_graph: &klipp_nodes::NodeGraph,
    inside: &std::collections::HashSet<klipp_nodes::NodeId>,
    compound_id: klipp_nodes::NodeId,
    ends: (klipp_nodes::NodeId, klipp_nodes::NodeId),
) -> Result<klipp_nodes::NodeGraph, String> {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKeyframeTracks, NodeKind, NodeParams, NodePos};
    if old_graph.nodes.iter().any(|n| inside.contains(&n.id) && matches!(n.kind, NodeKind::Source | NodeKind::Output)) {
        return Err("Group: Source / Output can't be packed into a Compound".into());
    }
    let (mut internal_edges, mut inbound_edges, mut outbound_edges, mut external_kept) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for e in &old_graph.edges {
        match (inside.contains(&e.from_node), inside.contains(&e.to_node)) {
            (true, true) => internal_edges.push(e.clone()),
            (false, true) => inbound_edges.push(e.clone()),
            (true, false) => outbound_edges.push(e.clone()),
            (false, false) => external_kept.push(e.clone()),
        }
    }
    if inbound_edges.len() > 1 {
        return Err("Group: only one input edge supported in v1".into());
    }
    if outbound_edges.len() > 1 {
        return Err("Group: only one output edge supported in v1".into());
    }
    let (sub_source_id, sub_output_id) = ends;
    let end = |id: NodeId, kind: NodeKind, label: &str| Node {
        id,
        kind,
        label: label.to_string(),
        enabled: true,
        params: NodeParams::default(),
        keyframes: NodeKeyframeTracks::default(),
        output_link: None,
        input_links: Vec::new(),
    };
    let mut subgraph = NodeGraph {
        nodes: vec![end(sub_source_id, NodeKind::Source, "Source"), end(sub_output_id, NodeKind::Output, "Output")],
        edges: internal_edges,
        output_node_id: sub_output_id,
        node_positions: std::collections::HashMap::new(),
        working_space_override: None,
        frames: Vec::new(),
        sticky_notes: Vec::new(),
    };
    subgraph.node_positions.insert(sub_source_id, NodePos { x: -200.0, y: 0.0 });
    subgraph.node_positions.insert(sub_output_id, NodePos { x: 800.0, y: 0.0 });
    for node in &old_graph.nodes {
        if inside.contains(&node.id) {
            subgraph.nodes.push(node.clone());
            if let Some(pos) = old_graph.node_positions.get(&node.id) {
                subgraph.node_positions.insert(node.id, *pos);
            }
        }
    }
    if let Some(in_edge) = inbound_edges.first() {
        subgraph.edges.push(Edge { from_node: sub_source_id, from_port: 0, to_node: in_edge.to_node, to_port: in_edge.to_port });
    }
    if let Some(out_edge) = outbound_edges.first() {
        subgraph.edges.push(Edge { from_node: out_edge.from_node, from_port: out_edge.from_port, to_node: sub_output_id, to_port: 0 });
    }
    if subgraph.topo_sort().is_err() {
        return Err("Group: subgraph would cycle — aborted".into());
    }
    let (mut cx, mut cy, mut count) = (0.0_f32, 0.0_f32, 0u32);
    let mut ordered: Vec<&NodeId> = inside.iter().collect();
    ordered.sort();
    for id in ordered {
        if let Some(pos) = old_graph.node_positions.get(id) {
            cx += pos.x;
            cy += pos.y;
            count += 1;
        }
    }
    let centroid = if count > 0 { NodePos { x: cx / count as f32, y: cy / count as f32 } } else { NodePos { x: 0.0, y: 0.0 } };
    let mut new_nodes: Vec<Node> = old_graph.nodes.iter().filter(|n| !inside.contains(&n.id)).cloned().collect();
    let mut compound = end(compound_id, NodeKind::Compound, "Compound");
    compound.params.compound_subgraph = Some(subgraph);
    new_nodes.push(compound);
    let mut new_edges: Vec<Edge> = external_kept;
    if let Some(in_edge) = inbound_edges.first() {
        new_edges.push(Edge { from_node: in_edge.from_node, from_port: in_edge.from_port, to_node: compound_id, to_port: 0 });
    }
    if let Some(out_edge) = outbound_edges.first() {
        new_edges.push(Edge { from_node: compound_id, from_port: 0, to_node: out_edge.to_node, to_port: out_edge.to_port });
    }
    let mut new_positions = old_graph.node_positions.clone();
    for id in inside {
        new_positions.remove(id);
    }
    new_positions.insert(compound_id, centroid);
    // Frames keep the members still in the parent; one left empty goes.
    let surviving: std::collections::HashSet<NodeId> = new_nodes.iter().map(|n| n.id).collect();
    let frames = old_graph
        .frames
        .iter()
        .map(|f| {
            let mut f = f.clone();
            f.contained_node_ids.retain(|id| surviving.contains(id));
            f
        })
        .filter(|f| !f.contained_node_ids.is_empty())
        .collect();
    let new_graph = NodeGraph {
        nodes: new_nodes,
        edges: new_edges,
        output_node_id: old_graph.output_node_id,
        node_positions: new_positions,
        working_space_override: old_graph.working_space_override,
        frames,
        sticky_notes: old_graph.sticky_notes.clone(),
    };
    if new_graph.topo_sort().is_err() {
        return Err("Group: parent would cycle — aborted".into());
    }
    Ok(new_graph)
}

/// The v1 canvas's Ungroup of the Compound `compound_id` in `parent`: the
/// parent with its nodes in the compound's place, and their ids; kept as
/// [`group_v1`] is.
#[cfg(test)]
pub(super) fn ungroup_v1(parent: &klipp_nodes::NodeGraph, compound_id: klipp_nodes::NodeId) -> Result<(klipp_nodes::NodeGraph, Vec<klipp_nodes::NodeId>), String> {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind};
    let compound = parent.nodes.iter().find(|n| n.id == compound_id).ok_or("Ungroup: compound not found")?;
    if compound.kind != NodeKind::Compound {
        return Err("Ungroup: selection is not a Compound".into());
    }
    let subgraph = compound.params.compound_subgraph.clone().ok_or("Ungroup: compound has no sub-graph")?;
    let sub_source_id = subgraph.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id);
    let sub_output_id = subgraph.output_node_id;
    let inner_entry = sub_source_id.and_then(|sid| subgraph.edges.iter().find(|e| e.from_node == sid).map(|e| (e.to_node, e.to_port)));
    let inner_exit = subgraph.edges.iter().find(|e| e.to_node == sub_output_id).map(|e| (e.from_node, e.from_port));
    let compound_in = parent.edges.iter().find(|e| e.to_node == compound_id).cloned();
    let compound_out = parent.edges.iter().find(|e| e.from_node == compound_id).cloned();
    let anchor = parent.node_positions.get(&compound_id).copied().unwrap_or_default();
    let mut new_nodes: Vec<Node> = parent.nodes.iter().filter(|n| n.id != compound_id).cloned().collect();
    let mut new_edges: Vec<Edge> = parent.edges.iter().filter(|e| e.from_node != compound_id && e.to_node != compound_id).cloned().collect();
    let mut new_positions = parent.node_positions.clone();
    new_positions.remove(&compound_id);
    let mut placed = 0i32;
    for node in &subgraph.nodes {
        if Some(node.id) == sub_source_id || node.id == sub_output_id {
            continue;
        }
        new_nodes.push(node.clone());
        let pos = subgraph.node_positions.get(&node.id).copied().unwrap_or(klipp_nodes::NodePos { x: anchor.x + 30.0 * placed as f32, y: anchor.y + 30.0 * placed as f32 });
        new_positions.insert(node.id, pos);
        placed += 1;
    }
    for edge in &subgraph.edges {
        if Some(edge.from_node) == sub_source_id || edge.to_node == sub_output_id {
            continue;
        }
        new_edges.push(edge.clone());
    }
    if let (Some(in_edge), Some((entry_node, entry_port))) = (compound_in.as_ref(), inner_entry) {
        new_edges.push(Edge { from_node: in_edge.from_node, from_port: in_edge.from_port, to_node: entry_node, to_port: entry_port });
    }
    if let (Some(out_edge), Some((exit_node, exit_port))) = (compound_out.as_ref(), inner_exit) {
        new_edges.push(Edge { from_node: exit_node, from_port: exit_port, to_node: out_edge.to_node, to_port: out_edge.to_port });
    }
    let new_parent = NodeGraph {
        nodes: new_nodes,
        edges: new_edges,
        output_node_id: parent.output_node_id,
        node_positions: new_positions,
        working_space_override: parent.working_space_override,
        frames: parent.frames.clone(),
        sticky_notes: parent.sticky_notes.clone(),
    };
    if new_parent.topo_sort().is_err() {
        return Err("Ungroup: parent would cycle — aborted".into());
    }
    let unpacked: Vec<NodeId> = subgraph.nodes.iter().filter(|n| Some(n.id) != sub_source_id && n.id != sub_output_id).map(|n| n.id).collect();
    Ok((new_parent, unpacked))
}

/// Returns true if inserting `candidate_id` as a compound clip into a sequence
/// identified by `host_id` would create a circular reference. Performs a DFS
/// from `candidate_id` through every transitively-referenced compound clip;
/// if `host_id` appears anywhere in that subtree (including `candidate_id`
/// itself), it is a cycle.
pub(super) fn would_create_cycle(
    candidate_id: klipp_core::SequenceId,
    host_id: klipp_core::SequenceId,
    all_sequences: &[klipp_timeline::Sequence],
) -> bool {
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![candidate_id];
    while let Some(sid) = stack.pop() {
        if sid == host_id {
            return true;
        }
        if !visited.insert(sid) {
            continue;
        }
        if let Some(seq) = all_sequences.iter().find(|s| s.id == sid) {
            for track in &seq.tracks {
                for clip in &track.clips {
                    if let klipp_timeline::ClipKind::Sequence { sequence_id, .. } = &clip.kind {
                        stack.push(*sequence_id);
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod compound_clip_cycle_tests {
    //! Tests for the cycle-detection guard around compound clip creation.
    use super::would_create_cycle;
    use klipp_core::{Fps, RationalTime, Resolution, SequenceId};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn t(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 24)
    }

    fn make_seq(name: &str) -> Sequence {
        Sequence {
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            buses: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: name.to_string(),
            fps: Fps::new(24, 1),
            resolution: Resolution { width: 1920, height: 1080 },
            first_clip_autoset: Default::default(),
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        }
    }

    fn add_compound(seq: &mut Sequence, target: SequenceId) {
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(Clip::sequence(target, t(0), t(24)));
        seq.tracks.push(tr);
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let a = make_seq("a");
        let id = a.id;
        assert!(would_create_cycle(id, id, &[a]));
    }

    #[test]
    fn direct_cycle_a_contains_b_then_b_into_a_detected() {
        let mut a = make_seq("a");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        // a contains b
        add_compound(&mut a, b_id);
        // would inserting a as compound clip into b create a cycle?
        // candidate=a (contains b), host=b → DFS from a finds b, NOT host b ... wait
        // We want: candidate must transitively reach host. a → b. host=b. Yes cycle.
        let _ = b; // suppress unused
        assert!(would_create_cycle(a_id, b_id, &[a.clone(), make_seq("b_dup")]),
            "candidate=a (a contains b), host=b → DFS from a hits b");
    }

    #[test]
    fn transitive_cycle_a_to_c_to_b_detected() {
        let mut a = make_seq("a");
        let mut c = make_seq("c");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        let c_id = c.id;
        add_compound(&mut a, c_id); // a contains c
        add_compound(&mut c, b_id); // c contains b
        // candidate=a, host=b → DFS a → c → b. Cycle.
        assert!(would_create_cycle(a_id, b_id, &[a, b, c]));
    }

    #[test]
    fn independent_sequences_no_cycle() {
        let a = make_seq("a");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        assert!(!would_create_cycle(a_id, b_id, &[a, b]));
    }

    #[test]
    fn empty_all_sequences_no_panic() {
        let a_id = SequenceId::new();
        let b_id = SequenceId::new();
        assert!(!would_create_cycle(a_id, b_id, &[]));
    }
}

/// Group and Ungroup by path (#1476) are made on the lifted grade itself;
/// held here to the shape the lift gives the v1 canvas's result of the
/// same gesture, case by case, without the ops depending on it.
#[cfg(test)]
mod structure_tests {
    use super::{group_v1, ungroup_v1};
    use klipp_graph_format::path::Root;
    use klipp_graph_format::Graph;
    use klipp_graph_lift::structure;
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodePos};
    use klipp_timeline::Sequence;
    use std::collections::HashSet;

    fn node(kind: NodeKind) -> Node {
        Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    fn edge(from: NodeId, to: NodeId, port: u8) -> Edge {
        Edge { from_node: from, from_port: 0, to_node: to, to_port: port }
    }

    /// A grade of `middle` between a Source and an Output, wired in a
    /// chain, each placed on the canvas: the grade and the ids in order.
    fn chain(middle: &[NodeKind]) -> (NodeGraph, Vec<NodeId>) {
        let mut nodes = vec![node(NodeKind::Source)];
        nodes.extend(middle.iter().map(|k| node(*k)));
        nodes.push(node(NodeKind::Output));
        let ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
        let edges = ids.windows(2).map(|w| edge(w[0], w[1], 0)).collect();
        let node_positions = ids.iter().enumerate().map(|(i, id)| (*id, NodePos { x: 100.0 * i as f32, y: 20.0 })).collect();
        let g = NodeGraph { nodes, edges, output_node_id: *ids.last().unwrap(), node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] };
        (g, ids)
    }

    /// `inner` as a Compound standing for the node at `at` in `g`.
    fn nest(g: &mut NodeGraph, at: NodeId, inner: NodeGraph) {
        let n = g.nodes.iter_mut().find(|n| n.id == at).unwrap();
        n.kind = NodeKind::Compound;
        n.params.compound_subgraph = Some(inner);
    }

    fn v1_level<'a>(g: &'a mut NodeGraph, descent: &[NodeId]) -> &'a mut NodeGraph {
        descent.iter().fold(g, |g, id| g.nodes.iter_mut().find(|n| n.id == *id).unwrap().params.compound_subgraph.as_mut().unwrap())
    }

    fn lifted_level<'a>(g: &'a mut Graph, descent: &[NodeId]) -> &'a mut Graph {
        descent.iter().fold(g, |g, id| &mut g.nodes.iter_mut().find(|n| n.id == *id).unwrap().group.as_mut().unwrap().graph)
    }

    /// The grade as the timeline grade lifts it.
    fn lift(seq: &Sequence, g: &NodeGraph) -> serde_json::Value {
        serde_json::to_value(seq.lift_root_grade(Root::TimelineGrade { sequence: seq.id.0 }, g).unwrap()).unwrap()
    }

    /// Group `selection` at `descent` both ways, and then Ungroup it both
    /// ways: each the lift of the v1 rules' result, or both refused.
    fn check(name: &str, grade: &NodeGraph, descent: &[NodeId], selection: &[NodeId]) {
        let seq = Sequence::new_default();
        let mut native: Graph = serde_json::from_value(lift(&seq, grade)).unwrap();
        let group = NodeId::new_v4();
        let inside: HashSet<NodeId> = selection.iter().copied().collect();
        let mut v1 = grade.clone();
        let by_v1 = group_v1(v1_level(&mut v1, descent), &inside, group, structure::ends(group));
        let by_path = structure::group(lifted_level(&mut native, descent), selection, group);
        match (by_v1, by_path) {
            (Ok(level), Ok(())) => *v1_level(&mut v1, descent) = level,
            (Err(_), Err(_)) => return,
            (a, b) => panic!("{name}: v1 {:?}, by path {:?}", a.map(|_| ()), b),
        }
        assert_eq!(serde_json::to_value(&native).unwrap(), lift(&seq, &v1), "{name}: grouped");
        let (level, _) = ungroup_v1(v1_level(&mut v1, descent), group).unwrap();
        *v1_level(&mut v1, descent) = level;
        structure::ungroup(lifted_level(&mut native, descent), group).unwrap();
        assert_eq!(serde_json::to_value(&native).unwrap(), lift(&seq, &v1), "{name}: ungrouped");
    }

    #[test]
    fn group_and_ungroup_by_path_are_the_lift_of_the_v1_rules() {
        use NodeKind::*;
        // A chain at the top: one edge in, one out.
        let (g, ids) = chain(&[Exposure, Saturation, Contrast]);
        check("chain", &g, &[], &ids[1..3]);
        // The last two before the Output.
        check("to the output", &g, &[], &ids[2..4]);
        // Unwired: each reads the frame, inside and out.
        let (mut g, ids) = chain(&[Exposure, Saturation, Contrast]);
        g.edges.retain(|e| e.to_node != ids[1]);
        check("an unwired primary", &g, &[], &ids[1..3]);
        // A Blend whose B reads its A (a filled port).
        let (g, ids) = chain(&[Exposure, Blend, Contrast]);
        check("a filled port", &g, &[], &ids[1..3]);
        // A frame round a grouped node and one left out.
        let (mut g, ids) = chain(&[Exposure, Saturation, Contrast]);
        g.frames.push(klipp_nodes::NodeFrame { id: uuid::Uuid::new_v4(), label: "F".into(), description: String::new(), color_rgba: 0, contained_node_ids: [ids[1], ids[3]].into_iter().collect(), recipe: None });
        g.frames.push(klipp_nodes::NodeFrame { id: uuid::Uuid::new_v4(), label: "G".into(), description: String::new(), color_rgba: 0, contained_node_ids: [ids[2]].into_iter().collect(), recipe: None });
        check("frames", &g, &[], &ids[1..3]);
        // A compound among the nodes grouped.
        let (mut g, ids) = chain(&[Exposure, Compound, Contrast]);
        let (inner, _) = chain(&[Saturation]);
        nest(&mut g, ids[2], inner);
        check("a compound grouped", &g, &[], &ids[1..3]);
        // Inside a compound, and two deep.
        let (mut g, ids) = chain(&[Compound]);
        let (mut mid, mid_ids) = chain(&[Exposure, Compound, Saturation]);
        let (deep, deep_ids) = chain(&[Contrast, Saturation, Exposure]);
        nest(&mut mid, mid_ids[2], deep);
        nest(&mut g, ids[1], mid);
        check("one deep", &g, &[ids[1]], &mid_ids[1..3]);
        check("two deep", &g, &[ids[1], mid_ids[2]], &deep_ids[1..3]);
        // Refused both ways: an end, two edges out, one node.
        let (mut g, ids) = chain(&[Exposure, Saturation, Contrast]);
        check("the Source", &g, &[], &ids[0..2]);
        g.edges.push(edge(ids[1], ids[3], 1));
        check("two out", &g, &[], &ids[1..3]);
        check("one node", &g, &[], &ids[1..2]);
    }
}
