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
            drop_frame: klipp_timeline::Sequence::default_drop_frame(fps),
            id: klipp_core::SequenceId::new(),
            name: name.clone(),
            fps,
            resolution: res,
            first_clip_autoset: klipp_timeline::FirstClipAutoSet { resolution: false, fps: false },
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
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
                                effect_keyframes: c.effect_keyframes.clone(),
                                opacity_keyframes: c.opacity_keyframes.clone(),
                                transform_pos_x_keyframes: c.transform_pos_x_keyframes.clone(),
                                transform_pos_y_keyframes: c.transform_pos_y_keyframes.clone(),
                                transform_scale_keyframes: c.transform_scale_keyframes.clone(),
                                transform_rotation_keyframes: c
                                    .transform_rotation_keyframes
                                    .clone(),
                                param_modifiers: c.param_modifiers.clone(),
                                has_speed_keyframes: !c.speed_keyframes.is_empty(),
                                audio_effects_count: c.audio_effects.len(),
                                volume: c.volume,
                                gain: c.gain,
                                pan: c.pan,
                                fade_in_secs: c.fade_in_secs,
                                fade_out_secs: c.fade_out_secs,
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
                effect_list: klipp_timeline::EffectList::new(snap.builtin_effects, snap.builtin_effects_enabled),
                effects: snap.effects.into(),
                effect_keyframes: snap.effect_keyframes,
                opacity_keyframes: snap.opacity_keyframes,
                transform_pos_x_keyframes: snap.transform_pos_x_keyframes,
                transform_pos_y_keyframes: snap.transform_pos_y_keyframes,
                transform_scale_keyframes: snap.transform_scale_keyframes,
                transform_rotation_keyframes: snap.transform_rotation_keyframes,
                param_modifiers: snap.param_modifiers,
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

    pub(super) fn group_into_compound(
        &mut self,
        clip_id: klipp_core::ClipId,
        node_ids: &[uuid::Uuid],
        needs_refresh: &mut bool,
    ) {
        use std::collections::HashSet;
        use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeKeyframeTracks, NodeParams, NodePos};

        let inside: HashSet<NodeId> = node_ids.iter().copied().collect();
        if inside.len() < 2 {
            return;
        }

        let path = self.state.graph_descent_path.clone();
        // Compute the edit against the currently-visible graph (root
        // or a descended sub-graph). `target_old_graph` is a snapshot
        // of that graph for validation + sub-graph construction;
        // `old_root_graph` is the whole clip graph for undo.
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            let Some(clip) = track.find_clip_mut(clip_id) else { continue };
            let old_root_graph = clip.grade().clone();
            let Some(compound_id) = clip.edit_grade(|root| {
                let Ok(target_graph) = Self::resolve_graph_at_path_mut(root, &path) else {
                    self.state.toast.show("Group: invalid descent path");
                    return None;
                };
                let old_graph = target_graph.clone();

                // Source / Output are structural for the parent graph —
                // moving them into a sub-graph would orphan the parent.
                // Reject the whole gesture if the user selected one.
                if old_graph.nodes.iter().any(|n| {
                    inside.contains(&n.id)
                        && matches!(n.kind, NodeKind::Source | NodeKind::Output)
                }) {
                    self.state.toast.show(
                        "Group: Source / Output can't be packed into a Compound"
                    );
                    return None;
                }

                // Partition every parent edge by which side(s) of the cut
                // it sits on. `external_kept` are edges that don't touch
                // the selection at all and survive the rewire untouched.
                let mut internal_edges: Vec<Edge> = Vec::new();
                let mut inbound_edges: Vec<Edge> = Vec::new();
                let mut outbound_edges: Vec<Edge> = Vec::new();
                let mut external_kept: Vec<Edge> = Vec::new();
                for e in &old_graph.edges {
                    let from_in = inside.contains(&e.from_node);
                    let to_in = inside.contains(&e.to_node);
                    match (from_in, to_in) {
                        (true, true) => internal_edges.push(e.clone()),
                        (false, true) => inbound_edges.push(e.clone()),
                        (true, false) => outbound_edges.push(e.clone()),
                        (false, false) => external_kept.push(e.clone()),
                    }
                }

                // v1: single-input / single-output compounds only. Multi-
                // port compounds need port-count plumbing on the
                // Compound NodeKind itself (stage 6 polish if anyone
                // actually wants it).
                if inbound_edges.len() > 1 {
                    self.state.toast.show(
                        "Group: only one input edge supported in v1"
                    );
                    return None;
                }
                if outbound_edges.len() > 1 {
                    self.state.toast.show(
                        "Group: only one output edge supported in v1"
                    );
                    return None;
                }

                // Build the sub-graph: synthetic Source + Output bracket
                // the selected nodes so the executor recognises it as a
                // standalone runnable graph (matches the convention every
                // NodeGraph follows). Selected NodeIds + params clone
                // through unchanged so keyframes + executor caches keep
                // working without remap.
                let sub_source_id = NodeId::new_v4();
                let sub_output_id = NodeId::new_v4();
                let mut subgraph = NodeGraph {
                    nodes: Vec::with_capacity(inside.len() + 2),
                    edges: Vec::with_capacity(internal_edges.len() + 2),
                    output_node_id: sub_output_id,
                    node_positions: std::collections::HashMap::new(),
                    working_space_override: None,
                    frames: Vec::new(),
                    sticky_notes: Vec::new(),
                };
                subgraph.nodes.push(Node {
                    id: sub_source_id,
                    kind: NodeKind::Source,
                    label: "Source".to_string(),
                    enabled: true,
                    params: NodeParams::default(),
                    keyframes: NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: Vec::new(),
                });
                subgraph.nodes.push(Node {
                    id: sub_output_id,
                    kind: NodeKind::Output,
                    label: "Output".to_string(),
                    enabled: true,
                    params: NodeParams::default(),
                    keyframes: NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: Vec::new(),
                });
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
                subgraph.edges.extend(internal_edges.iter().cloned());

                // Wire the synthetic Source to whichever internal node
                // was previously the parent's inbound target, and the
                // synthetic Output from whichever internal node fed the
                // parent's outbound consumer. Empty selections (no
                // external edges either way) still build a valid sub-
                // graph — Source / Output sit unconnected, executor sees
                // an identity passthrough.
                if let Some(in_edge) = inbound_edges.first() {
                    subgraph.edges.push(Edge {
                        from_node: sub_source_id,
                        from_port: 0,
                        to_node: in_edge.to_node,
                        to_port: in_edge.to_port,
                    });
                }
                if let Some(out_edge) = outbound_edges.first() {
                    subgraph.edges.push(Edge {
                        from_node: out_edge.from_node,
                        from_port: out_edge.from_port,
                        to_node: sub_output_id,
                        to_port: 0,
                    });
                }

                // Cycle check on the sub-graph — cheap insurance against
                // future bugs that might construct a malformed selection.
                // Carving a sub-DAG out of a DAG can't introduce a cycle
                // today, but the validate gives us a clean abort if it
                // ever can.
                if subgraph.topo_sort().is_err() {
                    self.state.toast.show("Group: subgraph would cycle — aborted");
                    return None;
                }

                // Centroid placement so the new Compound visually
                // replaces the cluster the user just collapsed.
                let mut cx = 0.0_f32;
                let mut cy = 0.0_f32;
                let mut count = 0u32;
                for id in &inside {
                    if let Some(pos) = old_graph.node_positions.get(id) {
                        cx += pos.x;
                        cy += pos.y;
                        count += 1;
                    }
                }
                let centroid = if count > 0 {
                    NodePos { x: cx / count as f32, y: cy / count as f32 }
                } else {
                    NodePos { x: 0.0, y: 0.0 }
                };

                // Build the new parent graph. Drop selected nodes; insert
                // the Compound; rewire the surviving external edges
                // through it.
                let compound_id = NodeId::new_v4();
                let mut new_nodes: Vec<Node> = old_graph
                    .nodes
                    .iter()
                    .filter(|n| !inside.contains(&n.id))
                    .cloned()
                    .collect();
                new_nodes.push(Node {
                    id: compound_id,
                    kind: NodeKind::Compound,
                    label: "Compound".to_string(),
                    enabled: true,
                    params: NodeParams {
                        compound_subgraph: Some(subgraph),
                        ..NodeParams::default()
                    },
                    keyframes: NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: Vec::new(),
                });

                let mut new_edges: Vec<Edge> = external_kept;
                if let Some(in_edge) = inbound_edges.first() {
                    new_edges.push(Edge {
                        from_node: in_edge.from_node,
                        from_port: in_edge.from_port,
                        to_node: compound_id,
                        to_port: 0,
                    });
                }
                if let Some(out_edge) = outbound_edges.first() {
                    new_edges.push(Edge {
                        from_node: compound_id,
                        from_port: 0,
                        to_node: out_edge.to_node,
                        to_port: out_edge.to_port,
                    });
                }

                let mut new_positions = old_graph.node_positions.clone();
                for id in &inside {
                    new_positions.remove(id);
                }
                new_positions.insert(compound_id, centroid);

                // Carry frames over from the old parent graph, dropping
                // any member id that's no longer in the parent (those
                // nodes are now packed into the new Compound). Frames
                // left with zero members get pruned.
                let surviving_ids: std::collections::HashSet<klipp_nodes::NodeId> =
                    new_nodes.iter().map(|n| n.id).collect();
                let new_frames: Vec<klipp_nodes::NodeFrame> = old_graph.frames.iter()
                    .map(|f| {
                        let mut f2 = f.clone();
                        f2.contained_node_ids.retain(|id| surviving_ids.contains(id));
                        f2
                    })
                    .filter(|f| !f.contained_node_ids.is_empty())
                    .collect();
                let new_graph = NodeGraph {
                    nodes: new_nodes,
                    edges: new_edges,
                    output_node_id: old_graph.output_node_id,
                    node_positions: new_positions,
                    working_space_override: old_graph.working_space_override,
                    frames: new_frames,
                    // Sticky notes are free-floating annotations — they
                    // don't reference node ids, so they pass through
                    // any structural mutation untouched.
                    sticky_notes: old_graph.sticky_notes.clone(),
                };

                if new_graph.topo_sort().is_err() {
                    self.state.toast.show("Group: parent would cycle — aborted");
                    return None;
                }

                // Write the new graph back into whichever level we came
                // from (root or a nested sub-graph). `target_graph` is
                // still the resolved `&mut NodeGraph` borrow from up top.
                *target_graph = new_graph;
                Some(compound_id)
            }) else { return };
            // Keep the whole-clip snapshot for undo after the grade edit.
            let new_root_graph = clip.grade().clone();

            // Replace the multi-selection with the new Compound so the
            // user's next gesture (drag, delete, descend) targets the
            // collapsed unit instead of the now-vanished originals.
            self.state.selected_node_ids.clear();
            self.state.selected_node_ids.insert(compound_id);
            self.state.selected_node_id = Some(compound_id);

            self.state.active_history_mut().push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::SetNodeGraph {
                    clip_id,
                    old_graph: old_root_graph,
                    new_graph: new_root_graph,
                },
            });
            *needs_refresh = true;
            return;
        }
    }

    /// Explode a Compound node back into its constituent nodes —
    /// the inverse of `group_into_compound`. Mirrors Group's edge-
    /// partitioning logic on the sub-graph (inbound from synthetic
    /// Source, outbound to synthetic Output, internal edges between
    /// the kept nodes) and rewires the Compound's external edges
    /// straight to the previously-internal endpoints. Single-port
    /// compounds only for v1 (same shape Group accepts). Refuses
    /// cleanly with a toast if something's off.
    pub(super) fn ungroup_compound(
        &mut self,
        clip_id: klipp_core::ClipId,
        compound_id: klipp_nodes::NodeId,
        needs_refresh: &mut bool,
    ) {
        use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind};

        let path = self.state.graph_descent_path.clone();
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            let Some(clip) = track.find_clip_mut(clip_id) else { continue };
            let old_root_graph = clip.grade().clone();
            let Some(unpacked_ids) = clip.edit_grade(|root| {
                let Ok(parent) = Self::resolve_graph_at_path_mut(root, &path) else {
                    self.state.toast.show("Ungroup: invalid descent path");
                    return None;
                };

                // Locate the target Compound + extract its sub-graph.
                let Some(compound_idx) = parent.nodes.iter().position(|n| n.id == compound_id) else {
                    self.state.toast.show("Ungroup: compound not found");
                    return None;
                };
                if parent.nodes[compound_idx].kind != NodeKind::Compound {
                    self.state.toast.show("Ungroup: selection is not a Compound");
                    return None;
                }
                let Some(subgraph) = parent.nodes[compound_idx]
                    .params
                    .compound_subgraph
                    .clone()
                else {
                    self.state.toast.show("Ungroup: compound has no sub-graph");
                    return None;
                };

                // Discover which internal nodes the synthetic Source
                // and Output touched so we know what to rewire the
                // parent's external edges to. The sub-graph shape Group
                // builds has exactly one Source and exactly one Output;
                // any deviation (loaded old project, manual edit)
                // degrades to "no reconnection" for that side, which is
                // the least-surprising behaviour.
                let sub_source_id = subgraph
                    .nodes
                    .iter()
                    .find(|n| n.kind == NodeKind::Source)
                    .map(|n| n.id);
                let sub_output_id = subgraph.output_node_id;
                let inner_entry = sub_source_id.and_then(|sid| {
                    subgraph
                        .edges
                        .iter()
                        .find(|e| e.from_node == sid)
                        .map(|e| (e.to_node, e.to_port))
                });
                let inner_exit = subgraph
                    .edges
                    .iter()
                    .find(|e| e.to_node == sub_output_id)
                    .map(|e| (e.from_node, e.from_port));

                // Compound's external in / out (same single-port v1
                // restriction Group enforces).
                let compound_in = parent
                    .edges
                    .iter()
                    .find(|e| e.to_node == compound_id)
                    .cloned();
                let compound_out = parent
                    .edges
                    .iter()
                    .find(|e| e.from_node == compound_id)
                    .cloned();

                // Compute centroid placement around the compound so the
                // unpacked nodes spread sensibly instead of piling up at
                // (0, 0) if the sub-graph never got layout. We use the
                // compound's own position as an anchor + a small offset
                // per node along the x axis.
                let anchor = parent
                    .node_positions
                    .get(&compound_id)
                    .copied()
                    .unwrap_or_default();

                // Build the new parent: drop the Compound node, strip
                // edges touching it, then splice in the sub-graph's
                // kept nodes + internal edges.
                let mut new_nodes: Vec<Node> = parent
                    .nodes
                    .iter()
                    .filter(|n| n.id != compound_id)
                    .cloned()
                    .collect();
                let mut new_edges: Vec<Edge> = parent
                    .edges
                    .iter()
                    .filter(|e| e.from_node != compound_id && e.to_node != compound_id)
                    .cloned()
                    .collect();
                let mut new_positions = parent.node_positions.clone();
                new_positions.remove(&compound_id);

                let mut placed = 0i32;
                for node in &subgraph.nodes {
                    // Skip the synthetic Source / Output wrappers —
                    // they only existed to make the sub-graph runnable.
                    if Some(node.id) == sub_source_id || node.id == sub_output_id {
                        continue;
                    }
                    new_nodes.push(node.clone());
                    let pos = subgraph.node_positions.get(&node.id).copied();
                    let pos = pos.unwrap_or(klipp_nodes::NodePos {
                        x: anchor.x + 30.0 * placed as f32,
                        y: anchor.y + 30.0 * placed as f32,
                    });
                    new_positions.insert(node.id, pos);
                    placed += 1;
                }
                for edge in &subgraph.edges {
                    // Skip edges that touch the synthetic Source /
                    // Output — they become external connections.
                    if Some(edge.from_node) == sub_source_id || edge.to_node == sub_output_id {
                        continue;
                    }
                    new_edges.push(edge.clone());
                }

                // Wire the parent's external inbound to whatever the
                // sub-graph's Source fed internally.
                if let (Some(in_edge), Some((entry_node, entry_port))) =
                    (compound_in.as_ref(), inner_entry)
                {
                    new_edges.push(Edge {
                        from_node: in_edge.from_node,
                        from_port: in_edge.from_port,
                        to_node: entry_node,
                        to_port: entry_port,
                    });
                }
                // Wire whatever fed the sub-graph's Output to the
                // parent's external outbound.
                if let (Some(out_edge), Some((exit_node, exit_port))) =
                    (compound_out.as_ref(), inner_exit)
                {
                    new_edges.push(Edge {
                        from_node: exit_node,
                        from_port: exit_port,
                        to_node: out_edge.to_node,
                        to_port: out_edge.to_port,
                    });
                }

                let new_parent = NodeGraph {
                    nodes: new_nodes,
                    edges: new_edges,
                    output_node_id: parent.output_node_id,
                    node_positions: new_positions,
                    working_space_override: parent.working_space_override,
                    // Ungroup just unpacks a Compound — the parent's
                    // frames cover existing parent nodes, none of which
                    // were removed, so frames pass through unchanged.
                    // Inner-graph frames are dropped on the floor (the
                    // sub-graph is gone). Sticky notes are detached
                    // annotations so they pass through too.
                    frames: parent.frames.clone(),
                    sticky_notes: parent.sticky_notes.clone(),
                };
                if new_parent.topo_sort().is_err() {
                    self.state.toast.show("Ungroup: parent would cycle — aborted");
                    return None;
                }

                // Replace the selection with the freshly-unpacked set
                // so the user can immediately continue editing / group-
                // again without re-selecting.
                let unpacked_ids: Vec<NodeId> = subgraph
                    .nodes
                    .iter()
                    .filter(|n| Some(n.id) != sub_source_id && n.id != sub_output_id)
                    .map(|n| n.id)
                    .collect();

                *parent = new_parent;
                Some(unpacked_ids)
            }) else { return };
            let new_root_graph = clip.grade().clone();

            self.state.selected_node_ids.clear();
            for id in &unpacked_ids {
                self.state.selected_node_ids.insert(*id);
            }
            self.state.selected_node_id = unpacked_ids.first().copied();

            self.state.active_history_mut().push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::SetNodeGraph {
                    clip_id,
                    old_graph: old_root_graph,
                    new_graph: new_root_graph,
                },
            });
            *needs_refresh = true;
            return;
        }
    }
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
