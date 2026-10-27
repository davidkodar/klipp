//! Colour: the grade's node graph and its nodes, grade groups and the timeline
//! grade, sampling the picture (balance, qualifier, hue curves), tracked
//! windows, grade stills and LUT versions (#826).
use crate::widgets::menu::SubmenuRows;

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    /// Keep [`Self::graph_before_input`] for this frame: the selected
    /// clip's graph taken on a press, key or text that could begin a
    /// grade gesture, held while the pointer is down, dropped after.
    pub(super) fn keep_graph_before_input(&mut self, ctx: &egui::Context) {
        let (began, held) = ctx.input(|i| {
            let began = i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::PointerButton { pressed: true, .. }
                        | egui::Event::Key { pressed: true, .. }
                        | egui::Event::Text(_)
                        | egui::Event::Paste(_)
                )
            });
            // Released counts as held: a click lands its value on the
            // frame it is let go.
            (began, i.pointer.any_down() || i.pointer.any_released())
        });
        match keep_before(began, held, self.node_param_edit_snapshot.is_some()) {
            Keep::Take => {
                self.graph_before_input = self.state.selected_clip.and_then(|id| {
                    self.state.sequences[self.state.active_sequence_idx]
                        .tracks
                        .iter()
                        .find_map(|t| t.find_clip(id))
                        .map(|clip| (id, clip.grade().clone()))
                });
            }
            Keep::Hold => {}
            Keep::Drop => self.graph_before_input = None,
        }
    }

    /// Printer lights (#241): the offset of the grade the Colour workspace
    /// shows moved `points` printer points on one channel, or all three
    /// (`channel` 3), at the level drawn, inside a compound too (#1527).
    /// The target is read through the stand-in for a sequence's own grade,
    /// as the page reads it; the edit is made after it stands down.
    fn printer_light(&mut self, channel: u8, points: i32, needs_refresh: &mut bool) {
        if self.state.selected_clip.is_none() && !self.state.grade_timeline && self.state.grade_group_stage.is_none() && self.state.grade_track.is_none() {
            self.state.toast.show("Select a clip to give it printer lights");
            return;
        }
        let stood_in = self.stand_in_timeline_grade();
        let target = printer_light_target(&mut self.state);
        self.stand_down_timeline_grade(stood_in);
        match target.ok_or_else(|| "this grade has nothing printer lights can move".to_string()).and_then(|t| apply_printer_light(&mut self.state, t, channel, points, self.prefs.color.printer_point)) {
            Ok([r, g, b]) => {
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                *needs_refresh = true;
                self.state.toast.show(format!("Printer lights R{r:+} G{g:+} B{b:+}"));
            }
            Err(why) => self.state.toast.show(format!("Printer lights: {why}")),
        }
    }

    /// The Colour toolbar's Track menu (#1408): see [`grade_track_menu`].
    pub(super) fn grade_track_controls(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) {
        grade_track_menu(ui, &mut self.state, theme);
    }

    pub(super) fn handle_color_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // Grade groups themselves are edits to the sequence, not to a
        // graph: never through a stand-in.
        if self.handle_grade_group_commands(cmd, needs_refresh) {
            return true;
        }
        if let AppCommand::PrinterLight { channel, points } = cmd {
            self.printer_light(*channel, *points, needs_refresh);
            return true;
        }
        // Whether the graded group's grades reach its members yet: the
        // engine's copy has a group's grades baked in only while it grades
        // anything, so a step that starts it grading needs a full sync.
        let group_grades = |app: &Self| {
            app.state.grade_group_stage.and_then(|(g, _)| app.state.sequences[app.state.active_sequence_idx].grade_group(g)).is_some_and(|g| g.grades())
        };
        let group_graded_before = group_grades(self);
        let stood_in = self.stand_in_timeline_grade();
        let handled = self.handle_color_commands_inner(cmd, needs_refresh);
        // A slider step reaches the engine through the param queue, which
        // it applies to the timeline grade as it does to a clip's; a full
        // sync per step would flatten the sequence at drag rate and throw
        // the queued steps away. Except the step that begins the grade:
        // the engine has to be given the graph once before the queue can
        // carry steps to it.
        match self.stand_down_timeline_grade(stood_in) {
            TimelineGradeSettled::Unchanged => {}
            TimelineGradeSettled::Changed if matches!(cmd, AppCommand::SetNodeParam { .. }) => {}
            TimelineGradeSettled::Changed | TimelineGradeSettled::Begun => *needs_refresh = true,
        }
        if group_grades(self) != group_graded_before {
            *needs_refresh = true;
        }
        handled
    }

    /// The Colour toolbar's grade-group controls (#237): the selected
    /// clip's group, its pre-clip and post-clip grades to grade, and a
    /// menu to make, fill, rename and remove groups.
    pub(super) fn grade_group_controls(&mut self, ui: &mut egui::Ui, all_cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
        use klipp_timeline::GradeStage;
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let selection: Vec<klipp_core::ClipId> = if self.state.selected_clips.is_empty() {
            self.state.selected_clip.into_iter().collect()
        } else {
            self.state.selected_clips.clone()
        };
        let clip_group = self.state.selected_clip.and_then(|id| seq.all_clips().find(|c| c.id == id)).and_then(|c| c.grade_group);
        // Grading a group's stage and selecting a clip of another group
        // grades that group's same stage, as the selection says.
        if let (Some((g, stage)), Some(cg)) = (self.state.grade_group_stage, clip_group) {
            if g != cg {
                self.state.grade_group_stage = Some((cg, stage));
            }
        }
        // A group deleted, by undo too, is not graded.
        if self.state.grade_group_stage.is_some_and(|(g, _)| seq.grade_group(g).is_none()) {
            self.state.grade_group_stage = None;
        }
        // The group shown: the one being graded, else the selected clip's.
        let shown = self.state.grade_group_stage.map(|(g, _)| g).or(clip_group).and_then(|g| seq.grade_group(g)).cloned();
        let mut cmds: Vec<AppCommand> = Vec::new();
        let mut stage_pick: Option<Option<(klipp_core::GradeGroupId, GradeStage)>> = None;
        let stage_name = |s: GradeStage| if s == GradeStage::Pre { "Pre-clip" } else { "Post-clip" };
        // One menu for the group, however it is used, so the row stays
        // Clip, group, Timeline: what is graded is in its label.
        let graded = shown.as_ref().is_some_and(|g| g.grades());
        let menu_label = match (&shown, self.state.grade_group_stage) {
            (Some(g), Some((_, stage))) => egui::RichText::new(format!("{} \u{00b7} {} \u{25BE}", g.name, stage_name(stage))).color(theme.text_primary).background_color(theme.bg_raised),
            (Some(g), None) if graded => egui::RichText::new(format!("{} \u{25CF} \u{25BE}", g.name)).color(theme.accent),
            (Some(g), None) => egui::RichText::new(format!("{} \u{25BE}", g.name)),
            (None, _) => egui::RichText::new("Group \u{25BE}"),
        };
        ui.menu_rows(menu_label, |ui| {
            if let Some(g) = &shown {
                for (stage, tip) in [
                    (GradeStage::Pre, "Before each member's own grade, on the footage as decoded: a camera's or a scene's shared balance."),
                    (GradeStage::Post, "After each member's own grade: the scene's look. Every member follows a change."),
                ] {
                    let on = self.state.grade_group_stage.is_some_and(|(_, s)| s == stage);
                    let has = g.stage(stage).is_some_and(|x| !x.is_identity());
                    let text = format!("Grade {}{}", stage_name(stage).to_lowercase(), if has { " \u{25CF}" } else { "" });
                    if ui.selectable_label(on, text).on_hover_text(tip).clicked() {
                        stage_pick = Some(Some((g.id, stage)));
                        ui.close_menu();
                    }
                }
                ui.separator();
            }
            let n = selection.len();
            let what = if n == 1 { "the selected clip".to_string() } else { format!("the {n} selected clips") };
            if ui.add_enabled(n > 0, egui::Button::new(format!("New group from {what}"))).clicked() {
                // The first "Group N" no group has: ops name groups by
                // name, so two of one name would be one of them to an op.
                let taken = |n: &str| seq.grade_groups.iter().any(|g| g.name.eq_ignore_ascii_case(n));
                let name = (1..).map(|k| format!("Group {k}")).find(|n| !taken(n)).expect("some number is free");
                cmds.push(AppCommand::CreateGradeGroup { name, clip_ids: selection.clone() });
                ui.close_menu();
            }
            if !seq.grade_groups.is_empty() {
                ui.separator();
                for g in &seq.grade_groups {
                    let members = seq.all_clips().filter(|c| c.grade_group == Some(g.id)).count();
                    if ui.add_enabled(n > 0, egui::Button::new(format!("Add {what} to \u{201c}{}\u{201d} ({members})", g.name))).clicked() {
                        cmds.push(AppCommand::SetClipsGradeGroup { clip_ids: selection.clone(), group: Some(g.id) });
                        ui.close_menu();
                    }
                }
            }
            if let Some(g) = &shown {
                ui.separator();
                if clip_group == Some(g.id) && ui.button(format!("Take {what} out of \u{201c}{}\u{201d}", g.name)).clicked() {
                    cmds.push(AppCommand::SetClipsGradeGroup { clip_ids: selection.clone(), group: None });
                    ui.close_menu();
                }
                ui.horizontal(|ui| {
                    ui.label("Name");
                    let id = ui.id().with(("grade_group_name", g.id));
                    let mut name: String = ui.memory(|m| m.data.get_temp(id)).unwrap_or_else(|| g.name.clone());
                    let resp = ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0));
                    if resp.lost_focus() {
                        if name.trim() != g.name {
                            cmds.push(AppCommand::RenameGradeGroup { group: g.id, name: name.clone() });
                        }
                        ui.memory_mut(|m| m.data.remove::<String>(id));
                    } else {
                        ui.memory_mut(|m| m.data.insert_temp(id, name));
                    }
                });
                if ui.button(format!("Delete \u{201c}{}\u{201d}", g.name)).on_hover_text("Its grades go; its clips keep their own.").clicked() {
                    cmds.push(AppCommand::DeleteGradeGroup { group: g.id });
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text("Grade groups: clips that share a pre-clip and a post-clip grade");
        if let Some(pick) = stage_pick {
            self.state.grade_group_stage = pick;
            self.state.grade_timeline = false;
            self.state.grade_track = None;
            self.state.graph_descent_path.clear();
            self.state.selected_node_id = None;
            self.state.selected_node_ids.clear();
        }
        all_cmds.extend(cmds);
    }

    /// Make, fill, rename and remove grade groups (#237), each one undo
    /// step. Only picture clips of the active sequence join a group.
    fn handle_grade_group_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        use klipp_timeline::GradeGroup;
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let pictures = |ids: &[klipp_core::ClipId]| -> Vec<klipp_core::ClipId> {
            seq.tracks
                .iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .flat_map(|t| t.clips.iter())
                .filter(|c| ids.contains(&c.id) && matches!(c.kind, klipp_timeline::ClipKind::Media { .. }))
                .map(|c| c.id)
                .collect()
        };
        let membership = |ids: &[klipp_core::ClipId], group: Option<klipp_core::GradeGroupId>| -> Option<EditCommand> {
            let changes: Vec<_> = seq
                .all_clips()
                .filter(|c| ids.contains(&c.id) && c.grade_group != group)
                .map(|c| (c.id, c.grade_group, group))
                .collect();
            (!changes.is_empty()).then_some(EditCommand::SetClipGradeGroups { changes })
        };
        let (edits, label): (Vec<EditCommand>, &str) = match cmd {
            AppCommand::CreateGradeGroup { name, clip_ids } => {
                let group = GradeGroup::new(name.trim());
                let mut groups = seq.grade_groups.clone();
                groups.push(group.clone());
                let mut edits = vec![EditCommand::SetGradeGroups { old: seq.grade_groups.clone(), new: groups }];
                edits.extend(membership(&pictures(clip_ids), Some(group.id)));
                (edits, "New Grade Group")
            }
            AppCommand::SetClipsGradeGroup { clip_ids, group } => {
                let ids = if group.is_some() { pictures(clip_ids) } else { clip_ids.clone() };
                (membership(&ids, *group).into_iter().collect(), if group.is_some() { "Add to Grade Group" } else { "Remove from Grade Group" })
            }
            AppCommand::RenameGradeGroup { group, name } => {
                let mut groups = seq.grade_groups.clone();
                let name = name.trim();
                // Ops find a group by name: a second of one name would
                // be unreachable.
                if groups.iter().any(|g| g.id != *group && g.name.eq_ignore_ascii_case(name)) {
                    self.state.toast.show(format!("There is already a grade group called \u{201c}{name}\u{201d}"));
                    return true;
                }
                match groups.iter_mut().find(|g| g.id == *group) {
                    Some(g) if g.name != name && !name.is_empty() => g.name = name.to_string(),
                    _ => return true,
                }
                (vec![EditCommand::SetGradeGroups { old: seq.grade_groups.clone(), new: groups }], "Rename Grade Group")
            }
            AppCommand::DeleteGradeGroup { group } => {
                let members: Vec<_> = seq.all_clips().filter(|c| c.grade_group == Some(*group)).map(|c| c.id).collect();
                let groups = seq.grade_groups.iter().filter(|g| g.id != *group).cloned().collect();
                let mut edits: Vec<EditCommand> = membership(&members, None).into_iter().collect();
                edits.push(EditCommand::SetGradeGroups { old: seq.grade_groups.clone(), new: groups });
                if self.state.grade_group_stage.is_some_and(|(g, _)| g == *group) {
                    self.state.grade_group_stage = None;
                }
                (edits, "Delete Grade Group")
            }
            _ => return false,
        };
        self.apply_edits_as_one(&edits, label);
        *needs_refresh = true;
        true
    }

    /// While the Colour workspace grades the timeline grade, put it on
    /// the sequence as the clip it renders as, on a hidden track, and
    /// select it: every node command, panel and history entry that works
    /// on "the selected clip's graph" then works on it unchanged. Returns
    /// what was selected before, for [`Self::stand_down_timeline_grade`];
    /// `None` when not grading the timeline (nothing is done). Only the
    /// Colour code runs in between — the stand-in is never saved, and
    /// [`Self::sync_sequence_to_engine`] never sends it.
    pub(super) fn stand_in_timeline_grade(&mut self) -> Option<Option<klipp_core::ClipId>> {
        if self.state.active_workspace != crate::state::Workspace::Color {
            return None;
        }
        // The timeline grade, or a grade group's stage (#237).
        let which = if self.state.grade_timeline {
            GradeStandIn::Timeline
        } else if let Some(t) = self.state.grade_track {
            GradeStandIn::Track(t)
        } else {
            let (g, s) = self.state.grade_group_stage?;
            GradeStandIn::Group(g, s)
        };
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let id = stand_in_grade_on(seq, which, &mut self.timeline_grade_fresh)?;
        Some(self.state.selected_clip.replace(id))
    }

    /// Take the stand-in off again, keeping its graph as the timeline
    /// grade if it was edited, and restoring the selection.
    pub(super) fn stand_down_timeline_grade(&mut self, stood_in: Option<Option<klipp_core::ClipId>>) -> TimelineGradeSettled {
        let Some(selected) = stood_in else { return TimelineGradeSettled::Unchanged };
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let settled = stand_down_timeline_grade_on(seq, &mut self.timeline_grade_fresh);
        self.state.selected_clip = selected;
        if settled != TimelineGradeSettled::Unchanged {
            self.state.dirty = true;
        }
        settled
    }

    fn handle_color_commands_inner(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        if self.handle_color_sampling(cmd, needs_refresh) { return true; }
        if self.handle_color_node_ops(cmd, needs_refresh) { return true; }
        if self.handle_color_node_config(cmd, needs_refresh) { return true; }
        if self.handle_color_keyframes(cmd, needs_refresh) { return true; }
        if self.handle_color_grade(cmd, needs_refresh) { return true; }
        if self.handle_color_stills(cmd, needs_refresh) { return true; }
        false
    }

    /// Collapse a multi-node selection into a single Compound node.
    /// See `AppCommand::GroupIntoCompound` for the user-visible
    /// contract; this method is the in-place mutation that captures
    /// the selected nodes + their internal edges into a sub-graph,
    /// drops them from the parent graph, and inserts a fresh Compound
    /// with the sub-graph stashed in `params.compound_subgraph`.
    /// Walk a descent path from a clip's root `NodeGraph` down into
    /// nested compounds' `compound_subgraph` fields and return a
    /// mutable reference to the graph at the leaf. Empty path
    /// returns the clip's root graph. Errors (missing node, wrong
    /// kind, empty sub-graph) bubble up so the caller can toast +
    /// reset the descent path instead of silently editing the wrong
    /// graph. This is a free function (vs `&mut self` method) so
    /// callers can hold the returned borrow without locking self.
    /// Decode a representative RGBA frame (midpoint of the clip's
    /// source range) for colour-match workflows. Uses
    /// `generate_thumbnail_at_size` at 480-wide so the caller can feed
    /// it to `match_colors` without waiting for a full-resolution
    /// decode. Returns the decoded bytes + dimensions, or a
    /// human-readable error message.
    fn decode_clip_representative_frame(
        state: &AppState,
        clip_id: klipp_core::ClipId,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let clip = state.sequences[state.active_sequence_idx]
            .tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
            .ok_or_else(|| "clip not found".to_string())?;
        let media_id = clip.media_id()
            .ok_or_else(|| "clip has no media".to_string())?;
        let item = state.media_pool.get(&media_id)
            .ok_or_else(|| "media not in pool".to_string())?;
        let path = item.path.clone();
        let src_in = clip.source_in.to_secs_f64();
        let src_out = clip.source_out.to_secs_f64();
        let mid = (src_in + src_out) * 0.5;
        klipp_engine::generate_thumbnail_at_size(&path, mid, 480)
            .map_err(|e| format!("decode: {e}"))
    }

    /// Follow a window through its clip on a worker (#238): the window
    /// tracker run from the reference frame forward and back, within the
    /// clip's part of the file, its move, size and turn sent back in
    /// `TrackingComplete`.
    pub(super) fn start_window_track(&mut self, job: WindowTrackJob) {
        let WindowTrackJob { clip_id, point_index, path, media_fps, frames: (first, last), reference, quad, pivot, perspective } = job;
        self.state.tracking_in_progress.insert(clip_id);
        self.state.tracking_progress.insert(clip_id, 0.0);
        let event_tx = self.proxy_event_tx.clone();
        std::thread::spawn(move || {
            use klipp_effects::window_track::{track_window, track_window_perspective, GrayFrame, TrackOptions};
            // At most two minutes of frames about the reference: they are
            // held in memory while the track runs.
            const MOST: usize = 3600;
            let (first, last) = if last + 1 - first > MOST {
                let lo = reference.saturating_sub(MOST / 2).max(first);
                (lo, (lo + MOST - 1).min(last))
            } else {
                (first, last)
            };
            let loading = 0.6;
            let frames = match grey_frames(&path, media_fps, first, last, &|p| {
                event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: p * loading }).is_ok()
            }) {
                Ok(f) => f,
                Err(e) => {
                    let _ = event_tx.send(klipp_core::AppEvent::TrackingFailed { clip_id, error: e });
                    return;
                }
            };
            let decode = |f: usize| -> Option<GrayFrame> { f.checked_sub(first).and_then(|i| frames.get(i)).cloned() };
            let Some(reference_frame) = decode(reference) else {
                let _ = event_tx.send(klipp_core::AppEvent::TrackingFailed { clip_id, error: "the frame under the playhead could not be read".into() });
                return;
            };
            let last = first + frames.len().saturating_sub(1);
            let (gw, gh) = (reference_frame.width as f64, reference_frame.height as f64);
            let px = |p: (f32, f32)| (p.0 as f64 * gw, p.1 as f64 * gh);
            let quad_px = [px(quad[0]), px(quad[1]), px(quad[2]), px(quad[3])];
            let pivot_px = px(pivot);
            let opts = TrackOptions::default();
            // On the GPU when there is one (its own passes on the shared
            // device), else the CPU, which follows the same.
            let mut gpu = klipp_effects::gpu::planar::PlanarGpu::new();
            let forward: Vec<usize> = (reference + 1..=last).collect();
            let backward: Vec<usize> = (first..reference).rev().collect();
            let share = (forward.len() as f32 / (forward.len() + backward.len()).max(1) as f32).max(0.01);
            let report = |p: f32| event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: loading + p * (1.0 - loading) }).is_ok();
            let nothing = || {
                let _ = event_tx.send(klipp_core::AppEvent::TrackingFailed {
                    clip_id,
                    error: "the window could not be followed from this frame: draw it over something with detail or a clear outline".into(),
                });
            };
            let tried = !(forward.is_empty() && backward.is_empty());
            if perspective {
                // Each frame's map from the reference, carrying the
                // window's corners and pivot, back in frame fractions.
                let ahead = track_window_perspective(gpu.as_mut(), &reference_frame, quad_px, &forward, &mut |f| decode(f), &|p| report(p * share));
                let behind = track_window_perspective(gpu.as_mut(), &reference_frame, quad_px, &backward, &mut |f| decode(f), &|p| report(share + p * (1.0 - share)));
                if ahead.is_empty() && behind.is_empty() && tried {
                    nothing();
                    return;
                }
                let still = klipp_effects::homography::Homography::identity();
                let all: Vec<(usize, klipp_effects::homography::Homography)> =
                    behind.into_iter().rev().chain(std::iter::once((reference, still))).chain(ahead).collect();
                let start_frame = all.first().map_or(reference, |a| a.0);
                let frac = |p: (f64, f64)| (p.0 / gw, p.1 / gh);
                let positions = all.iter().map(|(_, h)| frac(h.transform(pivot_px.0, pivot_px.1))).collect();
                let corners = all.iter().map(|(_, h)| quad_px.map(|c| frac(h.transform(c.0, c.1)))).collect();
                let _ = event_tx.send(klipp_core::AppEvent::TrackingComplete { clip_id, point_index, positions, start_frame, motion: Vec::new(), corners });
                return;
            }
            let ahead = track_window(gpu.as_mut(), &reference_frame, quad_px, pivot_px, &forward, &mut |f| decode(f), &opts, &|p| report(p * share));
            let behind = track_window(gpu.as_mut(), &reference_frame, quad_px, pivot_px, &backward, &mut |f| decode(f), &opts, &|p| report(share + p * (1.0 - share)));
            if ahead.is_empty() && behind.is_empty() && tried {
                nothing();
                return;
            }
            // The frames followed, earliest first, the reference among them.
            let still = klipp_effects::window_track::WindowFit::STILL;
            let all: Vec<(usize, klipp_effects::window_track::WindowFit)> =
                behind.into_iter().rev().chain(std::iter::once((reference, still))).chain(ahead).collect();
            let start_frame = all.first().map_or(reference, |a| a.0);
            let positions = all.iter().map(|(_, m)| ((pivot_px.0 + m.dx) / gw, (pivot_px.1 + m.dy) / gh)).collect();
            let motion = all.iter().map(|(_, m)| (m.scale, m.rotation.to_degrees())).collect();
            let _ = event_tx.send(klipp_core::AppEvent::TrackingComplete { clip_id, point_index, positions, start_frame, motion, corners: Vec::new() });
        });
    }

    pub(super) fn resolve_graph_at_path_mut<'a>(
        root: &'a mut klipp_nodes::NodeGraph,
        path: &[klipp_nodes::NodeId],
    ) -> Result<&'a mut klipp_nodes::NodeGraph, String> {
        let mut current = root;
        for &node_id in path {
            let node = current
                .nodes
                .iter_mut()
                .find(|n| n.id == node_id)
                .ok_or_else(|| format!("node {node_id} not on descent path"))?;
            if node.kind != klipp_nodes::NodeKind::Compound {
                return Err(format!(
                    "node {node_id} is {:?}, not Compound", node.kind
                ));
            }
            current = node
                .params
                .compound_subgraph
                .as_mut()
                .ok_or_else(|| format!(
                    "compound {node_id} has no subgraph"
                ))?;
        }
        Ok(current)
    }

    /// Run `mutate` against the currently-edited `NodeGraph` — root
    /// or some descent depth, whichever `state.graph_descent_path`
    /// points at. Snapshots the TOP-LEVEL clip graph before + after
    /// so the history entry captures the whole clip state (same
    /// shape undo has used since day one; no path-aware variant
    /// needed). Refuses gracefully with a toast if the stored
    /// descent path is invalid — the caller's refresh still runs so
    /// the UI self-heals on the next render.
    fn with_current_clip_graph_mut<F>(
        &mut self,
        clip_id: klipp_core::ClipId,
        needs_refresh: &mut bool,
        mutate: F,
    ) where
        F: FnOnce(&mut klipp_nodes::NodeGraph),
    {
        let path = self.state.graph_descent_path.clone();
        self.with_clip_graph_mut_at(clip_id, &path, needs_refresh, mutate);
    }

    /// The graph a command naming `nodes` on `clip_id` edits; see
    /// [`graph_path_for`].
    fn graph_holding(&self, clip_id: klipp_core::ClipId, nodes: &[klipp_nodes::NodeId]) -> Option<Vec<klipp_nodes::NodeId>> {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let root = seq.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == clip_id))?.grade();
        graph_path_for(root, nodes, &self.state.graph_descent_path)
    }

    /// [`Self::with_current_clip_graph_mut`] at `path` instead of the
    /// editor's descent: an empty path is the clip's top-level graph.
    fn with_clip_graph_mut_at<F>(
        &mut self,
        clip_id: klipp_core::ClipId,
        path: &[klipp_nodes::NodeId],
        needs_refresh: &mut bool,
        mutate: F,
    ) where
        F: FnOnce(&mut klipp_nodes::NodeGraph),
    {
        // Skip the per-call history step while a node-param edit gesture
        // is open (BeginNodeParamEdit took the snapshot, EndNodeParamEdit
        // commits the whole gesture as ONE entry). Without this, dragging a
        // mask handle on the viewer pushed one full SetNodeGraph entry per
        // frame: hundreds of graph clones a second, which flooded the undo
        // stack and set off engine resyncs that desynced the master clock
        // ("playback walks off on its own").
        let record = self.node_param_edit_snapshot.is_none();
        match edit_clip_graph(&mut self.state, clip_id, path, record, mutate) {
            Some(Ok(())) => *needs_refresh = true,
            Some(Err(())) => {
                // Descent path went stale (upstream remove, clip switch
                // we missed). Reset to root so future commands hit the
                // normal path; the user loses nothing except their
                // current descent position.
                self.state.graph_descent_path.clear();
                self.state.selected_node_ids.clear();
                self.state.selected_node_id = None;
            }
            None => {}
        }
    }

    /// The frame on screen, as bytes, when it is a whole frame of
    /// `clip_id` that a white balance can be read from; otherwise why not.
    ///
    /// The newest frame the engine made, not the one last swapped in:
    /// while paused the UI repaints rarely, and `data` can be several
    /// frames behind. And only when those bytes are the picture on
    /// screen, of this clip: while the GPU stack presents zero-copy
    /// (playing), no bytes are published and the buffer still holds the
    /// last parked frame — another time, maybe another clip — whose
    /// correction was applied here (#404).
    fn shown_frame_for_balance(&self, clip_id: klipp_core::ClipId) -> Result<(Vec<u8>, u32, u32), &'static str> {
        let clip_span = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .map(|c| (c.timeline_in, c.timeline_out));
        let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(why) = auto_wb_frame_refusal(
            fb.gpu_output_live,
            self.state.hdr_monitor.renders_hdr(),
            fb.latest_complete(),
            fb.latest_presented(),
            clip_span,
        ) {
            return Err(why);
        }
        let (bytes, fw, fh, _) = fb.latest_frame();
        if fw == 0 || fh == 0 || !bytes.chunks_exact(4).any(|p| p[3] != 0) {
            return Err("the viewer has no picture to read yet");
        }
        Ok((bytes.to_vec(), fw, fh))
    }

    /// Add a white balance correction to `clip_id`'s balance, on its
    /// Corrector (or a legacy Colour Balance node), as one undo step. The
    /// frame it was read from is the graded one, so the correction goes
    /// on top of the balance the clip already has, not instead of it.
    fn apply_balance_step(
        &mut self,
        clip_id: klipp_core::ClipId,
        temp_adj: f32,
        tint_adj: f32,
        who: &str,
        needs_refresh: &mut bool,
    ) {
        match self.state.apply_white_balance(clip_id, temp_adj, tint_adj) {
            None => self.state.toast.show(format!("{who}: add a Colour Balance node first")),
            // The sliders stop at ±50; a strong cast, or a clip already near
            // the end, gets part of what it needs.
            Some(true) => self.state.toast.show(format!(
                "{who}: the correction reached the end of the temperature or tint range, so the \
                 frame is only partly balanced — fix the cast at the source or with another balance node"
            )),
            Some(false) => {}
        }
        *needs_refresh = true;
    }

    /// The hue eyedropper's answer: three points on `curve` of the node,
    /// round the hue picked, as one change (#612).
    pub(super) fn range_hue_curve_on_sample(
        &mut self,
        clip_id: klipp_core::ClipId,
        node_id: uuid::Uuid,
        curve: u8,
        nx: f32,
        ny: f32,
        sample: Result<[f32; 3], klipp_core::NodeSampleRefusal>,
    ) {
        let rgb = match sample {
            Ok(rgb) => rgb,
            Err(klipp_core::NodeSampleRefusal::Why(why)) => {
                self.state.toast.show(format!("Hue picker: {why}"));
                return;
            }
            Err(klipp_core::NodeSampleRefusal::Placed) => match self.shown_frame_for_balance(clip_id).ok().and_then(|(bytes, fw, fh)| picked_linear(&bytes, fw, fh, nx, ny)) {
                Some(rgb) => rgb,
                None => {
                    self.state.toast.show("Hue picker: nothing to read there");
                    return;
                }
            },
        };
        let hsl = klipp_effects::cpu_grade::picture_hsl(rgb);
        if hsl.s < 0.05 {
            self.state.toast.show("Hue picker: that is almost grey, so it has no hue to change; pick a coloured patch");
            return;
        }
        let mut huesat = picked_node(self.state.sequence(), clip_id, node_id)
            .and_then(|n| n.params.huesat.clone())
            .unwrap_or_default();
        let points = match curve {
            0 => &mut huesat.hue_vs_hue,
            1 => &mut huesat.hue_vs_sat,
            _ => &mut huesat.hue_vs_lum,
        };
        *points = hue_range_points(points, hsl.h);
        self.state.toast.show(format!("Three points round {:.0}°: drag the middle one", hsl.h * 360.0));
        self.send_commands(vec![AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat }]);
    }

    /// One of a 3D stroke's samples back (#895): kept; a clip moved in
    /// the frame read on screen instead. Once the last is back, the node's
    /// volume takes them all, one undo step. `false` when no stroke of this
    /// node was waiting.
    pub(super) fn stroke_sample(&mut self, clip_id: klipp_core::ClipId, node_id: uuid::Uuid, nx: f32, ny: f32, sample: Result<[f32; 3], klipp_core::NodeSampleRefusal>) -> bool {
        if !self.state.qualifier_stroke.as_ref().is_some_and(|s| (s.clip_id, s.node_id) == (clip_id, node_id)) {
            return false;
        }
        let read = match sample {
            Ok(rgb) => Some(rgb),
            Err(klipp_core::NodeSampleRefusal::Placed) => self.shown_frame_for_balance(clip_id).ok().and_then(|(bytes, fw, fh)| picked_linear(&bytes, fw, fh, nx, ny)),
            Err(klipp_core::NodeSampleRefusal::Why(_)) => None,
        };
        let Some(stroke) = self.state.qualifier_stroke.as_mut() else { return false };
        if let Some(rgb) = read {
            stroke.samples.push(klipp_effects::cpu_grade::qualifier_rgb(rgb));
        }
        stroke.pending = stroke.pending.saturating_sub(1);
        if stroke.pending > 0 {
            return true;
        }
        let Some(done) = self.state.qualifier_stroke.take() else { return true };
        if done.samples.is_empty() {
            self.state.toast.show("Qualifier stroke: nothing to read there");
            return true;
        }
        let base = picked_node(self.state.sequence(), clip_id, node_id).and_then(|n| n.params.qualifier.clone()).unwrap_or_default();
        let qualifier = qualifier_with_stroke(base, &done.samples, done.remove);
        self.send_commands(vec![AppCommand::SetNodeQualifier { clip_id, node_id, qualifier }]);
        true
    }

    /// Key a Qualifier node on the eyedropper's sample: the colour it
    /// receives there, or, for a clip moved in the frame, the colour on
    /// screen, since the point clicked is not the same point of its
    /// picture. The node's softness and matte finesse stay.
    pub(super) fn key_qualifier_on_sample(
        &mut self,
        clip_id: klipp_core::ClipId,
        node_id: uuid::Uuid,
        nx: f32,
        ny: f32,
        sample: Result<[f32; 3], klipp_core::NodeSampleRefusal>,
    ) {
        let base = picked_node(self.state.sequence(), clip_id, node_id)
            .and_then(|n| n.params.qualifier.clone())
            .unwrap_or_default();
        let qualifier = match sample {
            Ok(rgb) => qualifier_keyed_on(rgb, base),
            Err(klipp_core::NodeSampleRefusal::Why(why)) => {
                self.state.toast.show(format!("Qualifier picker: {why}"));
                return;
            }
            Err(klipp_core::NodeSampleRefusal::Placed) => match self.shown_frame_for_balance(clip_id) {
                Err(why) => {
                    self.state.toast.show(format!("Qualifier picker: {why}"));
                    return;
                }
                Ok((bytes, fw, fh)) => match qualifier_from_pick(&bytes, fw, fh, nx, ny, base) {
                    None => {
                        self.state.toast.show("Qualifier picker: nothing to read there");
                        return;
                    }
                    Some(q) => {
                        self.state.toast.show("Qualifier picker: the clip is moved in the frame, so this read the colour on screen");
                        q
                    }
                },
            },
        };
        self.send_commands(vec![AppCommand::SetNodeQualifier { clip_id, node_id, qualifier }]);
    }

    fn handle_color_sampling(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::RequestWhiteBalanceSample { nx, ny } => {
                // The picked point's grey is the reference: what the user
                // knows is neutral, where Auto WB has to guess (#428).
                let Some(clip_id) = self.state.wb_pick_clip.take() else { return true };
                match self.shown_frame_for_balance(clip_id) {
                    Err(why) => self.state.toast.show(format!("White balance picker: {why}")),
                    Ok((bytes, fw, fh)) => match klipp_color::measure::picked_grey(&bytes, fw, fh, *nx, *ny) {
                        Err(why) => self.state.toast.show(format!("White balance picker: {why}")),
                        Ok(reference) => {
                            let (temp_adj, tint_adj) = klipp_color::white_balance::white_balance_to_neutralise(
                                reference,
                                klipp_core::WorkingColorspace::Rec709Linear,
                            );
                            self.apply_balance_step(clip_id, temp_adj, tint_adj, "White balance picker", _needs_refresh);
                        }
                    },
                }
                true
            }
            AppCommand::RequestHueCurveSample { nx, ny } => {
                // Read, as the qualifier is, from the picture the node
                // receives: the curve acts on that hue, not the graded one
                // on screen (#612). Answered by `NodeInputSampled`.
                let Some((clip_id, node_id, curve)) = self.state.hue_curve_pick.take() else { return true };
                self.state.hue_curve_awaiting = Some((clip_id, node_id, curve));
                let at = sample_path(self.state.sequence(), clip_id, node_id);
                let _ = self.cmd_tx.send(AppCommand::SampleNodeInput { clip_id, node_id, at, nx: *nx, ny: *ny });
                true
            }
            AppCommand::QualifierStroke { points } => {
                // Each point read on the picture the node receives, as the
                // eyedropper's is; the volume takes them once all are back.
                let Some((clip_id, node_id, remove)) = self.state.qualifier_stroke_pick.take() else { return true };
                let points = stroke_points(points, STROKE_SAMPLES);
                if points.is_empty() {
                    return true;
                }
                self.state.qualifier_stroke = Some(crate::state::StrokeSampling { clip_id, node_id, remove, samples: Vec::new(), pending: points.len() });
                let at = sample_path(self.state.sequence(), clip_id, node_id);
                for [nx, ny] in points {
                    let _ = self.cmd_tx.send(AppCommand::SampleNodeInput { clip_id, node_id, at: at.clone(), nx, ny });
                }
                true
            }
            AppCommand::RequestQualifierSample { nx, ny } => {
                // Keyed on the picture the node receives, not the one on
                // screen, which is the grade's result and, after a first
                // pick, the key's own correction (#587). The engine reads
                // it and answers with `NodeInputSampled`.
                let Some((clip_id, node_id)) = self.state.qualifier_pick.take() else { return true };
                let at = sample_path(self.state.sequence(), clip_id, node_id);
                let _ = self.cmd_tx.send(AppCommand::SampleNodeInput { clip_id, node_id, at, nx: *nx, ny: *ny });
                true
            }
            _ => false,
        }
    }

    fn handle_color_node_ops(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // Duplicate, copy, paste and replace, from either canvas (#1745).
        if let Some(changed) = self.state.node_edit(cmd) {
            *needs_refresh |= changed;
            return true;
        }
        match cmd {
            AppCommand::AddNode { .. } | AppCommand::RemoveNode { .. } | AppCommand::GroupIntoCompound { .. } | AppCommand::UngroupCompound { .. } => {
                // By path, one step (#1476).
                if self.state.canvas_structure_edit(cmd) == Some(true) {
                    *needs_refresh = true;
                }
                true
            }

            AppCommand::CreateNodeFrame { clip_id, node_ids, label, color_rgba } => {
                let label = label.clone();
                let color_rgba = *color_rgba;
                let ids = node_ids.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    let valid_ids: std::collections::BTreeSet<klipp_nodes::NodeId> = ids
                        .into_iter()
                        .filter(|id| graph.nodes.iter().any(|n| n.id == *id))
                        .collect();
                    if valid_ids.is_empty() { return; }
                    graph.frames.push(klipp_nodes::NodeFrame {
                        id: klipp_nodes::NodeFrameId::new_v4(),
                        label,
                        description: String::new(),
                        color_rgba,
                        contained_node_ids: valid_ids,
                    });
                });
                true
            }
            AppCommand::RenameNodeFrame { clip_id, frame_id, label } => {
                let frame_id = *frame_id;
                let label = label.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(f) = graph.frames.iter_mut().find(|f| f.id == frame_id) {
                        f.label = label;
                    }
                });
                true
            }
            AppCommand::SetNodeFrameDescription { clip_id, frame_id, description } => {
                let frame_id = *frame_id;
                let description = description.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(f) = graph.frames.iter_mut().find(|f| f.id == frame_id) {
                        f.description = description;
                    }
                });
                true
            }
            AppCommand::CreateStickyNote { clip_id, x, y } => {
                let x = *x;
                let y = *y;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    graph.sticky_notes.push(klipp_nodes::NodeStickyNote {
                        id: klipp_nodes::NodeStickyNoteId::new_v4(),
                        pos: klipp_nodes::NodePos { x, y },
                        width: 200.0,
                        text: String::new(),
                        color_rgba: STICKY_DEFAULT_COLOR_RGBA,
                    });
                });
                true
            }
            AppCommand::SetStickyNoteText { clip_id, sticky_id, text } => {
                let sticky_id = *sticky_id;
                let text = text.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(s) = graph.sticky_notes.iter_mut().find(|s| s.id == sticky_id) {
                        s.text = text;
                    }
                });
                true
            }
            AppCommand::MoveStickyNote { clip_id, sticky_id, delta_x, delta_y } => {
                // Inside the canvas's layout gesture the drag is one step,
                // taken when it ends (#1745).
                let record = !self.state.in_layout_gesture(*clip_id);
                let path = self.state.graph_descent_path.clone();
                if move_sticky_note(&mut self.state, *clip_id, &path, *sticky_id, (*delta_x, *delta_y), record) {
                    *needs_refresh = true;
                }
                true
            }
            AppCommand::DeleteStickyNote { clip_id, sticky_id } => {
                let sticky_id = *sticky_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    graph.sticky_notes.retain(|s| s.id != sticky_id);
                });
                true
            }
            AppCommand::RecolorNodeFrame { clip_id, frame_id, color_rgba } => {
                let frame_id = *frame_id;
                let color_rgba = *color_rgba;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(f) = graph.frames.iter_mut().find(|f| f.id == frame_id) {
                        f.color_rgba = color_rgba;
                    }
                });
                true
            }
            AppCommand::DeleteNodeFrame { clip_id, frame_id } => {
                let frame_id = *frame_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    graph.frames.retain(|f| f.id != frame_id);
                });
                true
            }
            AppCommand::AddToNodeFrame { clip_id, frame_id, node_id } => {
                let frame_id = *frame_id;
                let node_id = *node_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if !graph.nodes.iter().any(|n| n.id == node_id) { return; }
                    if let Some(f) = graph.frames.iter_mut().find(|f| f.id == frame_id) {
                        f.contained_node_ids.insert(node_id);
                    }
                });
                true
            }
            AppCommand::RemoveFromNodeFrame { clip_id, frame_id, node_id } => {
                let frame_id = *frame_id;
                let node_id = *node_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(f) = graph.frames.iter_mut().find(|f| f.id == frame_id) {
                        f.contained_node_ids.remove(&node_id);
                    }
                    graph.frames.retain(|f| !f.contained_node_ids.is_empty());
                });
                true
            }

            AppCommand::AutoLayoutNodeGraph { .. } => {
                // By path, the whole layout one step (#1528).
                if self.state.canvas_auto_layout(cmd) == Some(true) {
                    *needs_refresh = true;
                }
                true
            }

            AppCommand::UpdateActiveGradeVersion { clip_id } => {
                let clip_id = *clip_id;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(clip_id) {
                        if let Some(idx) = clip.active_grade_version() {
                            let grade = clip.grade().clone();
                            if clip.edit_grade_versions(|vs| vs.get_mut(idx).map(|v| v.set_grade(grade))).is_some() {
                                self.state.dirty = true;
                            }
                        }
                        break;
                    }
                }
                true
            }
            AppCommand::DescendIntoCompound { clip_id, node_id } => {
                // Resolve the CURRENT graph at the existing descent
                // path, check that `node_id` is a Compound with a
                // sub-graph, then append to the path. Clears
                // selection so the inspector doesn't linger on an
                // outer-level node the user can no longer see.
                let path = self.state.graph_descent_path.clone();
                let seq_idx = self.state.active_sequence_idx;
                let mut ok = false;
                for track in &mut self.state.sequences[seq_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|root| {
                            if let Ok(graph) = Self::resolve_graph_at_path_mut(root, &path) {
                                let is_compound = graph.nodes.iter().any(|n| {
                                    n.id == *node_id
                                        && n.kind == klipp_nodes::NodeKind::Compound
                                        && n.params.compound_subgraph.is_some()
                                });
                                if is_compound { ok = true; }
                            }
                        });
                        break;
                    }
                }
                if ok {
                    self.state.graph_descent_path.push(*node_id);
                    self.state.selected_node_ids.clear();
                    self.state.selected_node_id = None;
                }
                *needs_refresh = true;
                true
            }
            AppCommand::ConnectNodes { clip_id, from, from_port, to, to_port } => {
                let (from_node, from_port, to_node, to_port) = (*from, *from_port, *to, *to_port);
                let Some(path) = self.graph_holding(*clip_id, &[from_node, to_node]) else { return true };
                self.with_clip_graph_mut_at(*clip_id, &path, needs_refresh, |graph| {
                    let pre_connect = graph.clone();
                    // Remove existing edge to this input port — one
                    // edge per (node, port) pair.
                    graph.edges.retain(|e| !(e.to_node == to_node && e.to_port == to_port));
                    graph.edges.push(klipp_nodes::Edge {
                        from_node, from_port, to_node, to_port,
                    });
                    // A mixer grows to a wire dropped on a port past its
                    // count, so the executor finds a setting there.
                    if let Some(target) = graph.nodes.iter_mut().find(|n| n.id == to_node) {
                        target.grow_to_port(to_port);
                    }
                    // Roll back inside the closure if the new edge
                    // would introduce a cycle. The outer helper then
                    // records a no-op history entry (old == new) —
                    // undo stays consistent.
                    if graph.topo_sort().is_err() {
                        *graph = pre_connect;
                    }
                });
                true
            }
            AppCommand::DisconnectEdge { clip_id, to, to_port } => {
                let (to_node, to_port) = (*to, *to_port);
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    graph.edges.retain(|e| !(e.to_node == to_node && e.to_port == to_port));
                });
                true
            }
            AppCommand::SetNodeOutputLink { clip_id, node_id, label } => {
                let node_id = *node_id;
                let label = label.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    let before = graph.clone();
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.output_link = label.as_ref().and_then(|s| {
                            let t = s.trim();
                            if t.is_empty() { None } else { Some(t.to_string()) }
                        });
                    }
                    // A wireless link closes a loop as an edge does, and
                    // a looping grade can't be saved (#1135): roll back,
                    // as ConnectNodes does.
                    if graph.topo_sort().is_err() {
                        *graph = before;
                    }
                });
                true
            }
            AppCommand::SetNodeInputLink { clip_id, node_id, port, label } => {
                let node_id = *node_id;
                let port = *port as usize;
                let label = label.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    let before = graph.clone();
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        if node.input_links.len() <= port {
                            node.input_links.resize(port + 1, None);
                        }
                        node.input_links[port] = label.as_ref().and_then(|s| {
                            let t = s.trim();
                            if t.is_empty() { None } else { Some(t.to_string()) }
                        });
                        while node.input_links.last().is_some_and(|l| l.is_none()) {
                            node.input_links.pop();
                        }
                    }
                    // As for an output link: a loop through a wireless
                    // link is refused, rolled back (#1135).
                    if graph.topo_sort().is_err() {
                        *graph = before;
                    }
                });
                true
            }
            AppCommand::MoveNode { .. } => {
                // By path at the level shown, inside the canvas's layout
                // gesture: one step per drag (#1528).
                if self.state.canvas_move(cmd) == Some(true) {
                    *needs_refresh = true;
                }
                true
            }
            AppCommand::AddOutsideNode { clip_id, node_id } => {
                let node_id = *node_id;
                let Some(path) = self.graph_holding(*clip_id, &[node_id]) else { return true };
                let mut added = None;
                self.with_clip_graph_mut_at(*clip_id, &path, needs_refresh, |graph| {
                    added = graph.add_outside_node(node_id);
                });
                if added.is_none() {
                    self.state.toast.show("An outside node needs a correction gated by a key on the grade's chain".to_string());
                }
                true
            }
            AppCommand::ToggleNodeEnabled { clip_id, node_id } | AppCommand::ResetNode { clip_id, node_id } => {
                let node_id = *node_id;
                let Some(path) = self.graph_holding(*clip_id, &[node_id]) else { return true };
                self.with_clip_graph_mut_at(*clip_id, &path, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::SetNodeSolo { at } => {
                // Mirror into AppState for UI feedback (halo / badge /
                // toolbar indicator) AND let the dispatcher propagate
                // it to the engine via the normal forward path so the
                // executor's setter gets called on the next render.
                // needs_refresh = true so the paused viewer re-renders
                // the new soloed output immediately rather than waiting
                // for the next frame to arrive on its own.
                // The canvas marks the soloed node of the grade it shows.
                self.state.solo_node = at.as_ref().and_then(|a| a.path.node());
                self.state.solo_at = at.clone();
                self.state.viewed_node_name = at.as_ref().map(|a| {
                    let clip = match a.path.root { klipp_graph_format::path::Root::Clip { clip } => self.state.sequence().all_clips().find(|c| c.id.0 == clip), _ => None };
                    clip.and_then(|c| crate::panels::node_graph_v2::node_name(c, &a.path)).filter(|n| !n.is_empty()).unwrap_or_else(|| "a node".into())
                });
                *needs_refresh = true;
                false
            }
            AppCommand::SetViewChannel { channel } => {
                // The viewer's alone; the engine draws it.
                self.state.view_channel = *channel;
                *needs_refresh = true;
                false
            }
            AppCommand::SetNodeViewBookmark { slot } => {
                self.node_graph_v2_panel.set_bookmark(*slot as usize);
                true
            }
            AppCommand::JumpNodeViewBookmark { slot } => {
                self.node_graph_v2_panel.jump_bookmark(*slot as usize);
                true
            }
            AppCommand::CopyColorNodes { clip_id } => {
                // Build the selection set: prefer the multi-select if
                // populated, otherwise fall back to the primary selected
                // node id (single-click case).
                use std::collections::HashSet;
                let selection: HashSet<klipp_nodes::NodeId> = if !self.state.selected_node_ids.is_empty() {
                    self.state.selected_node_ids.iter().copied().collect()
                } else if let Some(id) = self.state.selected_node_id {
                    [id].into_iter().collect()
                } else {
                    HashSet::new()
                };
                if selection.is_empty() {
                    self.state.toast.show("Copy: no colour node selected");
                    return true;
                }
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                // The nodes would be copied without their mixes (M3 step 17a).
                if seq.all_clips().any(|c| c.id == *clip_id && c.holds_grade_mix()) {
                    self.state.toast.show(GRADE_MIX_NOT_COPIED);
                    return true;
                }
                let clipboard = seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id)
                    .map(|c| crate::state::GradeNodeClipboard::from_selection(c.grade(), &selection))
                    .unwrap_or(crate::state::GradeNodeClipboard {
                        nodes: Vec::new(), edges: Vec::new(),
                        node_positions: std::collections::HashMap::new(),
                    });
                let node_count = clipboard.nodes.len();
                let edge_count = clipboard.edges.len();
                if node_count == 0 {
                    self.state.toast.show("Copy: selected nodes not in clip's graph");
                } else {
                    self.state.grade_node_clipboard = Some(clipboard);
                    if edge_count > 0 {
                        self.state.toast.show(format!(
                            "Copied {node_count} colour node(s) + {edge_count} internal edge(s)"
                        ));
                    } else {
                        self.state.toast.show(format!("Copied {node_count} colour node(s)"));
                    }
                }
                true
            }
            AppCommand::PasteColorNodes { clip_id } => {
                let Some(template) = self.state.grade_node_clipboard.clone() else {
                    self.state.toast.show("Paste: nothing in the colour clipboard");
                    return true;
                };
                if template.nodes.is_empty() {
                    self.state.toast.show("Paste: clipboard is empty");
                    return true;
                }
                let edge_count = template.edges.len();
                // Layout offset so pasted nodes don't stack exactly on
                // top of existing nodes. Small enough to keep them
                // visually grouped; big enough to be visible.
                const PASTE_OFFSET: (f32, f32) = (40.0, 40.0);
                let mut node_count = 0usize;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    node_count = template.merge_into(graph, PASTE_OFFSET);
                });
                if edge_count > 0 {
                    self.state.toast.show(format!(
                        "Pasted {node_count} colour node(s) + {edge_count} edge(s)"
                    ));
                } else {
                    self.state.toast.show(format!("Pasted {node_count} colour node(s)"));
                }
                true
            }
            _ => false,
        }
    }


    fn handle_color_node_config(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetNodeLut { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::RenameNode { clip_id, node_id, ref label } => {
                let node_id = *node_id;
                let label = label.clone();
                let Some(path) = self.graph_holding(*clip_id, &[node_id]) else { return true };
                self.with_clip_graph_mut_at(*clip_id, &path, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.label = label;
                    }
                });
                true
            }
            AppCommand::SetNodeParam { clip_id, node_id, param, value } => {
                // An editor drawn before the level had a Corrector names
                // the one it reserved: made now, in the same write.
                let target = self.state.reserved_target(*clip_id, *node_id);
                let node_id = &target.0;
                // Update the UI-local sequence mirror — required so the UI's
                // own scope-preview re-grade (runs on this thread) sees the
                // new value. This mutates `self.state.sequences[...]` only,
                // not the shared `Arc<klipp_timeline::SharedSequence>` the engine reads.
                let path = self.state.graph_descent_path.clone();
                // A zones node's first edit places its zones for the
                // clip's source first, here and in the engine alike
                // (#2767).
                let seed = zones_seed(&self.state, *clip_id, *node_id, param);
                for (p, v) in &seed {
                    write_node_param(&mut self.state, *clip_id, &path, target, p, *v);
                    let _ = self.param_change_queue.push(ParamChange { clip_id: *clip_id, node_id: *node_id, param: p.clone(), value: *v });
                }
                write_node_param(&mut self.state, *clip_id, &path, target, param, *value);
                // Push the change onto the engine's lock-free queue. The
                // engine drains it at the start of each frame with a SINGLE
                // write-lock acquisition for the whole batch, avoiding the
                // per-tick contention with the engine's long sequence read
                // lock that the old `sync_sequence_to_engine` path caused.
                //
                // Overflow (256-slot bound) drops the OLDEST queued change.
                // That's fine: the user is scrubbing, so any subsequent tick
                // carries the current absolute value of the same parameter.
                let mut change = Some(ParamChange {
                    clip_id: *clip_id,
                    node_id: *node_id,
                    param: param.clone(),
                    value: *value,
                });
                while let Some(pending) = change.take() {
                    match self.param_change_queue.push(pending) {
                        Ok(()) => break,
                        Err(rejected) => {
                            // Queue full — drop one old entry and retry.
                            let _ = self.param_change_queue.pop();
                            change = Some(rejected);
                        }
                    }
                }
                // Do NOT set *needs_refresh = true — that would call
                // `sync_and_refresh_frame()` which takes the sequence write
                // lock. The dispatcher in `send_commands` detects any
                // SetNodeParam in the batch and calls the fast scope-preview
                // refresh path instead.
                //
                // Return true so the dispatcher does NOT forward this
                // command via cmd_tx — the engine learns of it through the
                // param_change_queue drain, not the command channel.
                true
            }
            AppCommand::BeginNodeParamEdit { clip_id } => {
                // Snapshot the current node graph so EndNodeParamEdit can commit
                // the whole gesture as one history entry. Idempotent — if a
                // gesture is already in progress for this clip we keep the
                // original snapshot.
                if self.node_param_edit_snapshot.is_none() {
                    // As the graph was before the input that began the
                    // gesture: the panel has already written its first
                    // value into the live one.
                    let before = self.graph_before_input.take().filter(|(id, _)| id == clip_id);
                    self.node_param_edit_snapshot = before.or_else(|| {
                        self.state.sequences[self.state.active_sequence_idx]
                            .tracks
                            .iter()
                            .find_map(|t| t.find_clip(*clip_id))
                            .map(|clip| (*clip_id, clip.grade().clone()))
                    });
                }
                // Auto-capture a "Before" reference still the first time the
                // user touches grade params on a saved project. Cheap gate
                // (is_empty) avoids re-capturing on subsequent edits.
                if self.state.grade_stills.is_empty() && self.state.project_path.is_some() {
                    if let Some(still) = self.capture_grade_still("Before".to_string()) {
                        self.state.grade_stills.push(still);
                        self.persist_grade_stills();
                        tracing::info!("Auto-captured 'Before' grade still for first edit");
                    }
                }
                // Return false so the dispatcher forwards the command to the
                // engine as well — the engine needs this signal to toggle
                // `in_node_param_edit`, which gates render-cache skip,
                // drag-downsample, and per-frame perf logging. Previously
                // returning true swallowed the command and left every
                // `in_node_param_edit` code path on the engine side as dead
                // code.
                false
            }
            AppCommand::EndNodeParamEdit { clip_id } => {
                if let Some((snap_clip_id, old_graph)) = self.node_param_edit_snapshot.take() {
                    if snap_clip_id == *clip_id {
                        close_gesture(&mut self.state, snap_clip_id, old_graph);
                    }
                }
                // Forward to engine so it clears `in_node_param_edit` and
                // triggers the one-shot full-res recovery regrade. See the
                // matching comment on BeginNodeParamEdit.
                false
            }
            AppCommand::AbandonNodeParamEdit { clip_id } => {
                // The gesture taken back: the graph as Begin found it, and
                // no history entry.
                if let Some((snap_clip_id, old_graph)) = self.node_param_edit_snapshot.take() {
                    if snap_clip_id == *clip_id && abandon_gesture(&mut self.state, snap_clip_id, old_graph) {
                        *needs_refresh = true;
                    }
                }
                // Forwarded to the engine, as EndNodeParamEdit is, to end
                // its drag state.
                false
            }
            AppCommand::SetNodeCurves { clip_id, node_id, curves } => {
                let target = self.state.reserved_target(*clip_id, *node_id);
                let cmd = AppCommand::SetNodeCurves { clip_id: *clip_id, node_id: target.0, curves: curves.clone() };
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    make_reserved(graph, target);
                    write_node_settings(graph, &cmd);
                });
                true
            }
            AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat } => {
                let target = self.state.reserved_target(*clip_id, *node_id);
                let cmd = AppCommand::SetNodeHueSatCurves { clip_id: *clip_id, node_id: target.0, huesat: huesat.clone() };
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    make_reserved(graph, target);
                    write_node_settings(graph, &cmd);
                });
                true
            }
            AppCommand::SetNodeQualifier { clip_id, .. }
            | AppCommand::SetNodeLumaKey { clip_id, .. }
            | AppCommand::SetNodeDifferenceKey { clip_id, .. }
            | AppCommand::SetNodeSoftClip { clip_id, .. }
            | AppCommand::SetNodeFilmGrain { clip_id, .. }
            | AppCommand::SetExternalMatteChannel { clip_id, .. }
            | AppCommand::SetMatteCombineOp { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                if matches!(cmd, AppCommand::SetExternalMatteChannel { .. } | AppCommand::SetMatteCombineOp { .. }) {
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetNodeDifferenceKeyPlate { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::SetNodeMasks { clip_id, node_id, masks } => {
                let node_id = *node_id;
                let clip_id_val = *clip_id;
                let new_len = masks.masks.len();
                self.with_current_clip_graph_mut(clip_id_val, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                // Defensive tracker cleanup: any TrackPoint bound to
                // (node_id, mask_index) where mask_index now points
                // past the end of the new mask list is orphaned and
                // would update a non-existent mask every frame. Drops
                // those + leaves bindings into the still-valid range
                // alone. Mirrors the cleanup on `RemoveMaskShape`
                // and covers every callsite of `SetNodeMasks` that
                // shrinks the list (whether today's UI ever does so
                // or not — defending against future regressions).
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id_val) {
                    clip.track_points.retain(|tp| match tp.linked_mask {
                        Some((nid, mi)) if nid == node_id => mi < new_len,
                        _ => true,
                    });
                }
                true
            }
            AppCommand::SetNodeColorMatch { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::SetExternalMattePath { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                self.state.dirty = true;
                true
            }
            AppCommand::SetNodeLayerMixerInput { clip_id, .. } => {
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::CaptureColorMatchReference => {
                let captured = {
                    let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
                        None
                    } else {
                        Some((fb.data.clone(), fb.width, fb.height))
                    }
                };
                if let Some((bytes, w, h)) = captured {
                    self.state.color_match_reference = Some((bytes, w, h));
                    self.state.toast.show(format!("Captured colour reference ({w}×{h})"));
                } else {
                    self.state.toast.show("No frame to capture — start playback or scrub first");
                }
                true
            }
            AppCommand::AddMaskShape { clip_id, .. } => {
                // The shape onto the grade's Mask node, made with its
                // cutout when there's none (`ensure_mask_cutout` composites
                // source × matte → Output), so a mask added from the Color
                // tab clips the picture at once.
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            AppCommand::FocusMaskInspector => {
                // Edit-workspace user just clicked a mask on the
                // viewer. Flip their inspector to the Color tab so
                // the Masks section is visible; also open the color
                // panel so Color-workspace users see the node /
                // MASKS section inline too.
                self.layout.active_inspector_tab = crate::layout_state::InspectorTab::Color;
                true
            }
            AppCommand::MoveMaskShape { clip_id, node_id, from, to } => {
                let (clip_id, node_id, from, to) = (*clip_id, *node_id, *from, *to);
                let mut moved = false;
                self.with_current_clip_graph_mut(clip_id, needs_refresh, |graph| {
                    moved = move_mask(graph, node_id, from, to);
                });
                if moved {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                        for tp in &mut clip.track_points {
                            if let Some((tn, mi)) = tp.linked_mask.filter(|(tn, _)| *tn == node_id) {
                                tp.linked_mask = Some((tn, moved_index(mi, from, to)));
                            }
                        }
                    }
                    // The tool's selection named the old places.
                    self.state.mask_edit = Default::default();
                }
                true
            }
            AppCommand::RemoveMaskShape { clip_id, mask_index } => {
                let clip_id = *clip_id;
                let mask_index = *mask_index;
                // Capture the Mask node id BEFORE the graph mutation
                // so we can clean up any TrackPoints bound to this
                // mask after the removal lands. with_current_clip_graph_mut
                // wraps the mutation in history, so the tracker
                // cleanup runs as a separate (non-history) follow-up
                // — acceptable because trackers don't go through the
                // node-graph snapshot machinery anyway.
                let mask_node_id: Option<uuid::Uuid> = self
                    .state.sequences[self.state.active_sequence_idx]
                    .tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .and_then(|c| c.grade().nodes.iter()
                        .find(|n| n.kind == klipp_nodes::NodeKind::Mask)
                        .map(|n| n.id));
                self.with_current_clip_graph_mut(clip_id, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                if let Some(nid) = mask_node_id {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                        // Drop trackers that were bound to the removed
                        // mask. Indices on remaining trackers can shift,
                        // and bindings on OTHER trackers pointing to
                        // higher mask_index need to slide down by one
                        // so they keep matching their actual mask after
                        // the removal.
                        clip.track_points.retain(|tp| tp.linked_mask != Some((nid, mask_index)));
                        for tp in &mut clip.track_points {
                            if let Some((tn, mi)) = tp.linked_mask {
                                if tn == nid && mi > mask_index {
                                    tp.linked_mask = Some((tn, mi - 1));
                                }
                            }
                        }
                    }
                }
                true
            }
            AppCommand::RemoveMaskTracking { clip_id, node_id, mask_index } => {
                let clip_id = *clip_id;
                let node_id = *node_id;
                let mask_index = *mask_index;
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                    clip.track_points.retain(|tp| tp.linked_mask != Some((node_id, mask_index)));
                    clip.edit_grade(|graph| {
                        if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                            if let Some(masks) = &mut node.params.masks {
                                if let Some(mask) = masks.masks.get_mut(mask_index) {
                                    mask.keyframes.center_x.clear();
                                    mask.keyframes.center_y.clear();
                                    mask.keyframes.clear_track();
                                }
                            }
                        }
                    });
                }
                self.state.dirty = true;
                *needs_refresh = true;
                self.state.toast.show("Mask tracking cleared");
                true
            }
            AppCommand::UseClipAsColorMatchReference { clip_id } => {
                let clip_id = *clip_id;
                match Self::decode_clip_representative_frame(&self.state, clip_id) {
                    Ok((bytes, w, h)) => {
                        self.state.color_match_reference = Some((bytes, w, h));
                        self.state.color_match_reference_clip = Some(clip_id);
                        self.state.toast.show(format!(
                            "Colour reference captured from clip ({w}×{h})"
                        ));
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Couldn't capture reference: {e}"));
                    }
                }
                true
            }
            AppCommand::MatchClipToColorReference { clip_id } => {
                let clip_id = *clip_id;
                let Some((ref_bytes, _rw, _rh)) = self.state.color_match_reference.clone() else {
                    self.state.toast.show(
                        "Mark a reference clip first (right-click → Use as Colour Reference)"
                    );
                    return true;
                };
                let source_bytes = match Self::decode_clip_representative_frame(&self.state, clip_id) {
                    Ok((b, _, _)) => b,
                    Err(e) => {
                        self.state.toast.show(format!("Couldn't read target clip: {e}"));
                        return true;
                    }
                };
                let result = klipp_color::color_match::match_colors(&source_bytes, &ref_bytes);
                let settings = klipp_core::ColorMatchSettings {
                    exposure: result.exposure_adjust,
                    temperature: result.temperature_adjust,
                    tint: result.tint_adjust,
                    saturation: result.saturation_adjust,
                };
                let mut applied = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(clip_id) {
                        let node_id = clip.edit_grade(|g| g.ensure_color_match());
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                                node.params.color_match = Some(settings);
                                node.enabled = true;
                                applied = true;
                            }
                        });
                        break;
                    }
                }
                if applied {
                    *needs_refresh = true;
                    self.state.toast.show(format!(
                        "Matched clip to reference: exp {:+.2}, temp {:+.0}, tint {:+.0}, sat {:.2}×",
                        settings.exposure, settings.temperature, settings.tint, settings.saturation
                    ));
                } else {
                    self.state.toast.show("Target clip not found in active sequence");
                }
                true
            }
            AppCommand::ApplyColorMatch { clip_id, node_id } => {
                let Some((ref ref_bytes, _rw, _rh)) = self.state.color_match_reference else {
                    self.state.toast.show("Capture a reference frame first");
                    return true;
                };
                // Source = the clip's pre-grade frame at the playhead.
                // The engine publishes that as `fb.pregrade` whenever
                // `in_node_param_edit` is set OR a compare mode is on.
                // For a quick first integration we use the displayed
                // post-grade frame (`fb.data`) as the source — it's
                // always present and matches what the user sees the
                // node currently producing. The match adjustments
                // are applied additively, so matching what's already
                // showing converges to the reference look anyway.
                let source = {
                    let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if fb.data.is_empty() { None } else { Some(fb.data.clone()) }
                };
                let Some(source_bytes) = source else {
                    self.state.toast.show("No source frame available — try scrubbing the playhead");
                    return true;
                };
                let result = klipp_color::color_match::match_colors(&source_bytes, ref_bytes);
                let settings = klipp_core::ColorMatchSettings {
                    exposure: result.exposure_adjust,
                    temperature: result.temperature_adjust,
                    tint: result.tint_adjust,
                    saturation: result.saturation_adjust,
                };
                // Through SetNodeColorMatch, as the qualifier picker sends
                // its key: one undo step. Written here in place, it was
                // recorded in none, and undo could not take it back.
                self.send_commands(vec![AppCommand::SetNodeColorMatch { clip_id: *clip_id, node_id: *node_id, settings }]);
                *needs_refresh = true;
                self.state.toast.show(format!(
                    "Match: exp {:+.2}, temp {:+.0}, tint {:+.0}, sat {:.2}×",
                    settings.exposure, settings.temperature, settings.tint, settings.saturation
                ));
                true
            }
            _ => false,
        }
    }

    fn handle_color_keyframes(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::ShowMaskKeysOnTimeline { clip_id, node_id, mask_index } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let first = seq.all_clips().find(|c| c.id == *clip_id).and_then(|clip| {
                    crate::mask_tracks::keyed(clip).into_iter().map(|(t, _)| t).find(|t| t.node == *node_id && t.mask == *mask_index)
                });
                if let Some(track) = first {
                    self.state.timeline_mask_lane = Some((*clip_id, track));
                }
                true
            }
            AppCommand::AddMaskKeyframe { clip_id, node_id, .. }
            | AppCommand::RemoveMaskKeyframe { clip_id, node_id, .. }
            | AppCommand::ClearMaskKeyframes { clip_id, node_id, .. }
            | AppCommand::MoveMaskKeyframe { clip_id, node_id, .. }
            | AppCommand::SetMaskKeyframeInterpolation { clip_id, node_id, .. } => {
                let _ = (clip_id, node_id);
                // One step, or none inside a slider's or a key drag's
                // gesture (whose close records it).
                let record = self.node_param_edit_snapshot.is_none();
                if let Some(Ok(())) = mask_key_edit(&mut self.state, cmd, record) {
                    *needs_refresh = true;
                }
                true
            }
            AppCommand::AddKeyframe { clip_id, node_id, .. }
            | AppCommand::RemoveKeyframe { clip_id, node_id, .. }
            | AppCommand::ClearKeyframes { clip_id, node_id, .. }
            | AppCommand::SetKeyframeInterpolation { clip_id, node_id, .. }
            | AppCommand::SetKeyframeEase { clip_id, node_id, .. }
            | AppCommand::SetKeyframeTangents { clip_id, node_id, .. } => {
                // A node's key track (its bypass's, from the bypass switch
                // and diamond, a colour wheel's) edited on the graph holding the node, as its
                // bypass is switched: one step, or none inside a slider's
                // gesture (whose close records it). Written straight into
                // the grade, these recorded no step, so an undo skipped them.
                let Some(path) = self.graph_holding(*clip_id, &[*node_id]) else { return true };
                self.with_clip_graph_mut_at(*clip_id, &path, needs_refresh, |graph| {
                    write_node_settings(graph, cmd);
                });
                true
            }
            _ => false,
        }
    }

    fn handle_color_grade(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::RefreshGrade => {
                *needs_refresh = true;
                self.state.dirty = true;
                true
            }
            AppCommand::SetSourceRotation { clip_id, degrees } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for track in &mut seq.tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_rotation = *degrees;
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::SetClipLogFormat { clip_id, log_format } => {
                if let Some(mid) = self.state.set_clip_log_format(*clip_id, *log_format) {
                    // Sequence first: the media sync makes the engine
                    // re-decode, and that frame must be graded — and
                    // cached — under the clips' new graphs, not the old.
                    self.sync_sequence_to_engine();
                    self.sync_media_pool_to_engine();
                    // Thumbnail and filmstrip were converted under the old
                    // format; their cache key includes it, so this finds or
                    // makes the pictures for the new one.
                    self.refresh_media_pictures(mid);
                }
                *needs_refresh = true;
                true
            }
            AppCommand::AutoWhiteBalance { clip_id } => {
                let clip_id = *clip_id;
                match self.shown_frame_for_balance(clip_id) {
                    Err(why) => self.state.toast.show(format!("Auto White Balance: {why}")),
                    Ok((bytes, fw, fh)) => {
                        // The framing analysis's faces, when there is one: a
                        // face is never the surface a cast is judged against,
                        // so a close-up under a cool light balances on the shirt.
                        let faces = crate::panels::colour_check::whole_faces_at_playhead(&self.state);
                        match klipp_color::auto_white_balance(&bytes, fw, fh, &faces) {
                            Ok((temp_adj, tint_adj)) => {
                                self.apply_balance_step(clip_id, temp_adj, tint_adj, "Auto White Balance", needs_refresh);
                            }
                            Err(klipp_color::measure::NoReference::NothingGrey) => self.state.toast.show(
                                "Auto White Balance: nothing in this frame is surely grey (skin, sand \
                                 and a strong cast are not trusted) — the balance is left as it was. \
                                 Pick a grey with the eyedropper beside the button instead",
                            ),
                            Err(klipp_color::measure::NoReference::LeansAgainstTheFrame) => self.state.toast.show(
                                "Auto White Balance: the greys here lean against the frame's main colour — \
                                 a strong cast, or a coloured wall beside a grey that is not quite, and \
                                 colour alone cannot tell which. The balance is left as it was: pick the \
                                 grey with the eyedropper beside the button, or run Analyze so faces are known",
                            ),
                        }
                    }
                }
                true
            }
            AppCommand::PickHueCurve { clip_id, node_id, curve } => {
                self.state.eyedropper_mode = crate::state::EyedropperMode::HueCurve;
                self.state.hue_curve_pick = Some((*clip_id, *node_id, *curve));
                self.state.toast.show("Click the colour to change in the viewer (Esc cancels)");
                true
            }
            AppCommand::PickQualifierStroke { clip_id, node_id, remove } => {
                self.state.eyedropper_mode = crate::state::EyedropperMode::QualifierStroke;
                self.state.qualifier_stroke_pick = Some((*clip_id, *node_id, *remove));
                self.state.toast.show(if *remove { "Drag across the colours to take out of the key (Esc cancels)" } else { "Drag across the colours to keep (Esc cancels)" });
                true
            }
            AppCommand::PickQualifierColour { clip_id, node_id } => {
                self.state.eyedropper_mode = crate::state::EyedropperMode::Qualifier;
                self.state.qualifier_pick = Some((*clip_id, *node_id));
                self.state.toast.show("Click the colour to key in the viewer (Esc cancels)");
                true
            }
            AppCommand::PickWhiteBalance { clip_id } => {
                self.state.eyedropper_mode = crate::state::EyedropperMode::WhiteBalance;
                self.state.wb_pick_clip = Some(*clip_id);
                self.state.toast.show("Click something grey, white or black in the viewer (Esc cancels)");
                true
            }
            AppCommand::ToggleColorPanel => {
                // Move the workspace and nothing else: the panel
                // visibility and the browser borrow are settled by
                // `reconcile_workspace`, so this route cannot drift
                // from the toolbar tabs the way it had.
                use crate::state::Workspace;
                self.state.active_workspace = if self.state.active_workspace == Workspace::Color {
                    Workspace::Edit
                } else {
                    Workspace::Color
                };
                true
            }
            AppCommand::ShowEffectsBrowser => {
                self.state.show_browser_panel = true;
                self.layout.active_browser_tab = crate::layout_state::BrowserTab::Library;
                true
            }
            AppCommand::ToggleBrowserPanel => {
                let now = !self.state.show_browser_panel;
                self.set_browser_visible(now);
                true
            }
            AppCommand::ToggleInspectorPanel => {
                let now = !self.state.show_inspector_panel;
                self.set_inspector_visible(now);
                true
            }
            AppCommand::ToggleBeforeAfter => {
                self.state.before_after_active = !self.state.before_after_active;
                if self.state.before_after_active {
                    // Mutually exclusive with other compare modes
                    if self.state.stabilization_compare_active {
                        self.state.stabilization_compare_active = false;
                        let _ = self.cmd_tx.send(AppCommand::ToggleStabilizationCompare);
                    }
                    self.state.grade_still_compare = false;
                }
                // Fall through to forward to engine (sets bypass_color_grade)
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::ToggleStabilizationCompare => {
                self.state.stabilization_compare_active = !self.state.stabilization_compare_active;
                if self.state.stabilization_compare_active {
                    // Mutually exclusive with other compare modes
                    if self.state.before_after_active {
                        self.state.before_after_active = false;
                        let _ = self.cmd_tx.send(AppCommand::ToggleBeforeAfter);
                    }
                    self.state.grade_still_compare = false;
                    if self.state.split_compare_node.is_some() {
                        self.state.split_compare_node = None;
                        let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id: None, path: None });
                    }
                }
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            _ => false,
        }
    }

    fn handle_color_stills(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // Delegates LUT and grade-version handling to a follow-up pass.
        if self.handle_color_stills_inner(cmd, needs_refresh) { return true; }
        if self.handle_color_lut_versions(cmd, needs_refresh) { return true; }
        false
    }

    fn handle_color_stills_inner(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            // Grade stills (reference frame comparison)
            AppCommand::CaptureGradeStill { ref name } => {
                if let Some(still) = self.capture_grade_still(name.clone()) {
                    // A grade holding a correction's mix isn't kept with
                    // the still (M3 step 17a), so it can't be applied from it.
                    let mixed = still_source_clip(self.state.sequence(), self.state.playhead, self.state.selected_clip).is_some_and(|c| c.holds_grade_mix());
                    self.state.grade_stills.push(still);
                    if mixed {
                        self.state.toast.show(format!("Grade still '{name}' captured without its grade, which can't be applied from it: a correction's mix or mask channel can't be copied yet"));
                    } else {
                        self.state.toast.show(format!("Grade still '{}' captured", name));
                    }
                    self.persist_grade_stills();
                }
                true
            }
            AppCommand::DeleteGradeStill { index } => {
                if *index < self.state.grade_stills.len() {
                    let removed_filename = self.state.grade_stills[*index].filename.clone();
                    // Defer the texture drop for two frames — the
                    // compare thumbnail may still be bound in the
                    // paint command set that's being submitted this
                    // tick, and dropping the `TextureHandle` now
                    // would destroy the underlying wgpu texture
                    // before the submit lands, triggering:
                    //   Texture with 'egui_texid_Managed(N)' has
                    //   been destroyed
                    // The same pattern is used everywhere else a
                    // managed texture gets replaced mid-frame.
                    if let Some(tex) = self.state.grade_stills[*index].texture.take() {
                        self.defer_drop_texture(tex);
                    }
                    self.state.grade_stills.remove(*index);
                    if self.state.active_grade_still == Some(*index) {
                        self.state.active_grade_still = None;
                        self.state.grade_still_compare = false;
                    } else if let Some(active) = self.state.active_grade_still {
                        if active > *index {
                            self.state.active_grade_still = Some(active - 1);
                        }
                    }
                    // Best-effort PNG cleanup. Re-write the index so the load
                    // path doesn't try to read a now-missing file.
                    if let Some(project_dir) = self.writable_project_dir() {
                        if !removed_filename.is_empty() {
                            klipp_project::delete_grade_still_file(&project_dir, &removed_filename);
                        }
                        self.persist_grade_stills();
                    }
                }
                true
            }
            AppCommand::SetActiveGradeStill { index } => {
                self.state.active_grade_still = *index;
                true
            }
            AppCommand::ApplyGradeFromStill { index, clip_ids, append } => {
                let Some(still) = self.state.grade_stills.get(*index) else { return true };
                let Some(grade) = still.grade.clone() else {
                    self.state.toast.show("That still was grabbed before stills kept their grade; grab it again to use its grade");
                    return true;
                };
                let name = still.name.clone();
                let labels = still.grade_labels.clone();
                let GradeApplied { edits, refused, labels } = apply_grade(&self.state.sequences[self.state.active_sequence_idx], &grade, &labels, clip_ids, *append);
                if edits.is_empty() {
                    if refused.is_empty() {
                        self.state.toast.show("Select the clips to grade first");
                    } else {
                        self.state.toast.show(format!("Could not add the grade of \"{name}\": {}", refusals_line(&refused)));
                    }
                    return true;
                }
                let n = edits.len();
                // The grade's node labels in the same step (#1866).
                let edits: Vec<_> = edits.into_iter().chain(labels).collect();
                self.apply_edits_as_one(&edits, "Apply Grade from Still");
                let mut told = format!("Applied the grade of \"{name}\" to {n} clip{}", if n == 1 { "" } else { "s" });
                if !refused.is_empty() {
                    told = format!("{told}; not to {}: {}", clip_ids.len() - n, refusals_line(&refused));
                }
                self.state.toast.show(told);
                true
            }
            AppCommand::SaveGradeToLibrary { clip_id, name } => {
                let Some(dir) = crate::grade_library::dir() else {
                    self.state.toast.show("No config folder to keep the grade library in");
                    return true;
                };
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let Some(clip) = seq.all_clips().find(|c| c.id == *clip_id) else { return true };
                // Its thumbnail, as the strip shows it: the clip graded, at its middle.
                let thumbnail = klipp_render::still::clip_still(clip, &self.state.media_pool, clip.duration().to_secs_f64() / 2.0, crate::graded_thumbs::THUMB_WIDTH, klipp_engine::video_decoder::working_colorspace()).ok();
                // Its nodes' colour labels in its file (#1866).
                let node_labels = klipp_timeline::grade_node_labels(&seq.node_labels, clip.id);
                match crate::grade_library::save_labelled_in(&dir, name, clip.grade(), &node_labels, thumbnail.as_ref()) {
                    Ok(()) => {
                        self.state.grade_library = crate::grade_library::load_in(&dir);
                        self.state.toast.show(format!("Kept \"{}\" in the grade library", name.trim()));
                    }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::DeleteGradeFromLibrary { name } => {
                let Some(dir) = crate::grade_library::dir() else { return true };
                match crate::grade_library::delete_in(&dir, name) {
                    Ok(()) => self.state.grade_library = crate::grade_library::load_in(&dir),
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::SetGradeLibraryLabels { name, camera, profile, notes } => {
                let Some(dir) = crate::grade_library::dir() else { return true };
                let labels = crate::grade_library::GradeLabels { camera: camera.trim().into(), profile: profile.trim().into(), notes: notes.trim().into() };
                match crate::grade_library::set_labels_in(&dir, name, &labels) {
                    Ok(()) => {
                        if let Some(g) = self.state.grade_library.iter_mut().find(|g| &g.name == name) {
                            g.labels = labels;
                        }
                    }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::ApplyGradeFromLibrary { name, clip_ids, append } => {
                let Some((grade, labels)) = self.state.grade_library.iter().find(|g| &g.name == name).map(|g| (g.graph.clone(), g.node_labels.clone())) else {
                    self.state.toast.show(format!("No grade \"{name}\" in the library"));
                    return true;
                };
                let GradeApplied { edits, refused, labels } = apply_grade(&self.state.sequences[self.state.active_sequence_idx], &grade, &labels, clip_ids, *append);
                if edits.is_empty() {
                    if refused.is_empty() {
                        self.state.toast.show("Select the clips to grade first");
                    } else {
                        self.state.toast.show(format!("Could not add \"{name}\": {}", refusals_line(&refused)));
                    }
                    return true;
                }
                let n = edits.len();
                // The grade's node labels in the same step (#1866).
                let edits: Vec<_> = edits.into_iter().chain(labels).collect();
                self.apply_edits_as_one(&edits, "Apply Grade from Library");
                let mut told = format!("Applied \"{name}\" to {n} clip{}", if n == 1 { "" } else { "s" });
                if !refused.is_empty() {
                    told = format!("{told}; not to {}: {}", clip_ids.len() - n, refusals_line(&refused));
                }
                self.state.toast.show(told);
                true
            }
            AppCommand::ToggleGradeStillCompare => {
                self.state.grade_still_compare = !self.state.grade_still_compare;
                // One wipe at a time.
                if self.state.grade_still_compare {
                    self.state.wipe_clip = None;
                }
                true
            }
            AppCommand::WipeAgainstClip { target } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let wiped = target.and_then(|t| crate::graded_thumbs::wipe_target(seq, self.state.selected_clip, self.state.playhead, t));
                if target.is_some() && wiped.is_none() {
                    self.state.toast.show("No clip that way to wipe against");
                }
                self.state.wipe_clip = wiped;
                if wiped.is_some() {
                    self.state.grade_still_compare = false;
                }
                true
            }
            _ => false,
        }
    }

    /// Snapshot the current viewer frame into a `GradeStill` value.
    /// Returns `None` if the frame buffer is empty. Pure capture — does
    /// not mutate `self.state`; caller is responsible for pushing the
    /// result and persisting.
    fn capture_grade_still(&self, name: String) -> Option<crate::state::GradeStill> {
        let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.data.is_empty() || fb.width == 0 {
            return None;
        }
        let src_w = fb.width;
        let src_h = fb.height;
        // Downscale to 480 px wide for memory efficiency (matches the
        // pre-existing CaptureGradeStill behaviour).
        let scale = (480.0 / src_w as f32).min(1.0);
        let tw = (src_w as f32 * scale) as u32;
        let th = (src_h as f32 * scale) as u32;
        let mut rgba = vec![0u8; (tw * th * 4) as usize];
        for y in 0..th {
            for x in 0..tw {
                let sx = (x as f32 / scale) as u32;
                let sy = (y as f32 / scale) as u32;
                let si = ((sy * src_w + sx) * 4) as usize;
                let di = ((y * tw + x) * 4) as usize;
                if si + 3 < fb.data.len() && di + 3 < rgba.len() {
                    rgba[di..di + 4].copy_from_slice(&fb.data[si..si + 4]);
                }
            }
        }
        // Filename is stable for the lifetime of the still — derived from
        // the OS time so reopening the project finds the right PNG.
        let filename = format!(
            "still_{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let seq = self.state.sequences.get(self.state.active_sequence_idx)?;
        let clip = still_source_clip(seq, self.state.playhead, self.state.selected_clip);
        let source = clip.map(|c| klipp_project::StillSource {
            clip_id: Some(c.id),
            media_id: c.media_id(),
            record_secs: self.state.playhead.to_secs_f64(),
            clip_name: c
                .media_id()
                .and_then(|m| self.state.media_pool.get(&m))
                .map(|m| m.filename.clone())
                .unwrap_or_default(),
        });
        Some(crate::state::GradeStill {
            name,
            filename,
            rgba,
            width: tw,
            height: th,
            source,
            // A grade with a correction's mix (M3 step 17a) would be
            // applied from the still without it: the still keeps no grade.
            grade: clip.filter(|c| !c.holds_grade_mix()).map(|c| c.grade().clone()),
            // Its nodes' colour labels with it (#1866).
            grade_labels: clip.filter(|c| !c.holds_grade_mix()).map(|c| klipp_timeline::grade_node_labels(&seq.node_labels, c.id)).unwrap_or_default(),
            texture: None,
        })
    }

    /// Write all current grade stills (PNG sidecars + index) to the
    /// project directory. No-op if the project hasn't been saved yet.
    pub(super) fn persist_grade_stills(&mut self) {
        let Some(project_dir) = self.writable_project_dir() else {
            return;
        };
        let stills: Vec<klipp_project::GradeStillData> = self
            .state
            .grade_stills
            .iter()
            .map(|s| klipp_project::GradeStillData {
                name: s.name.clone(),
                filename: s.filename.clone(),
                rgba: s.rgba.clone(),
                width: s.width,
                height: s.height,
                source: s.source.clone(),
                grade: s.grade.clone(),
                grade_labels: s.grade_labels.clone(),
            })
            .collect();
        if let Err(e) = klipp_project::save_grade_stills(&project_dir, &stills) {
            tracing::warn!("Failed to persist grade stills: {e}");
            // Said, not only logged: a newer Klipp's stills are kept by
            // not saving these over them (#993).
            self.state.toast.show(e);
        }
    }

    /// Tell the control socket what the viewer's frame goes through after
    /// the grade, when it has changed (#1976): read each frame from the
    /// state, so every writer of those settings is covered.
    pub(super) fn publish_picture_transform(&self) {
        let now = picture_transform_text(self.state.global_lut_path.as_deref(), self.state.global_lut_enabled, self.state.global_lut_opacity, self.prefs.color.sdr_display_transfer, self.state.hdr_monitor);
        if let Ok(mut project) = self.state.control_shared.project.lock() {
            if project.picture_transform != now {
                project.picture_transform = now;
            }
            project.viewer_lut = self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled).map(|p| (p, self.state.global_lut_opacity));
        }
    }

    /// Tell the control socket which global LUT the project names, for
    /// `check`.
    /// The project's colour set-up changed to `colour` as one undo step
    /// named for what changed (#1875); with `convert`, a new working space
    /// converts every clip's grade so it keeps its look, and a toast says
    /// how many. Nothing happens when nothing changes.
    pub(super) fn set_project_colour(&mut self, colour: klipp_core::ProjectColour, convert: bool) {
        let Some((edit, label, migrated)) = crate::panels::colour_management::colour_edit(&self.state, colour, convert) else { return };
        let to = crate::panels::colour_management::working_colorspace_label(match &edit {
            EditCommand::SetProjectColour { new, .. } => new.working_colorspace,
            _ => self.state.project_working_colorspace,
        });
        self.apply_edits_as_one(std::slice::from_ref(&edit), &label);
        if convert && migrated > 0 {
            self.state.toast.show(format!("Migrated {migrated} clip{} to {to}", if migrated == 1 { "" } else { "s" }));
        }
    }

    pub(super) fn publish_global_lut(&self) {
        if let Ok(mut project) = self.state.control_shared.project.lock() {
            project.global_lut = self.state.global_lut_path.clone();
        }
    }

    fn handle_color_lut_versions(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            // Global LUT — update UI state and forward to engine
            // The output LUT is part of the project's colour set-up, so a
            // change of it is an undo step (#1875).
            AppCommand::LoadGlobalLut { path } => {
                if self.state.global_lut_path.as_ref() == Some(path) && self.state.global_lut_enabled {
                    // The same file again: read afresh, nothing to undo.
                    let _ = self.cmd_tx.send(cmd.clone());
                } else {
                    let mut colour = crate::panels::colour_management::project_colour(&self.state);
                    colour.output_lut = Some(path.clone());
                    colour.output_lut_enabled = true;
                    self.set_project_colour(colour, false);
                }
                true
            }
            AppCommand::ClearGlobalLut => {
                let mut colour = crate::panels::colour_management::project_colour(&self.state);
                colour.output_lut = None;
                self.set_project_colour(colour, false);
                true
            }
            AppCommand::SetProjectColour { colour, convert_grades } => {
                self.set_project_colour((**colour).clone(), *convert_grades);
                true
            }
            AppCommand::SetGlobalLutOpacity(opacity) => {
                self.state.global_lut_opacity = *opacity;
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::SetGlobalLutEnabled(enabled) => {
                let mut colour = crate::panels::colour_management::project_colour(&self.state);
                colour.output_lut_enabled = *enabled;
                self.set_project_colour(colour, false);
                true
            }

            // Bake the current clip's grade chain to a 3D LUT (.cube file).
            // Runs CPU-only on a fresh executor against a clone of the graph
            // so live state and the engine's GPU pipeline are untouched.
            // Spatial nodes (Qualifier / Mask / matte ops) are auto-disabled
            // by `bake_graph_to_lut` since they can't be captured per-pixel.
            AppCommand::BakeGradeToLut { clip_id, output_path, lut_size } => {
                let clip_id = *clip_id;
                let output_path = output_path.clone();
                let lut_size = *lut_size;
                let working_cs = self.state.project_working_colorspace;

                let graph = {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    // As it draws, its corrections' mixes in (M3 step 17a).
                    seq.all_clips().find(|c| c.id == clip_id).map(|c| c.grade_to_run().into_owned())
                };
                let Some(graph) = graph else {
                    self.state.toast.show("Bake failed — clip not found");
                    return true;
                };

                let title = output_path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Klipp Grade")
                    .to_string();

                let mut executor = klipp_nodes::GraphExecutor::new_cpu_only();
                executor.set_working_colorspace(working_cs);
                let lut = klipp_nodes::bake_graph_to_lut(&graph, &mut executor, lut_size);

                match klipp_color::write_cube(&output_path, &lut, lut_size as u32, &title) {
                    Ok(()) => {
                        self.state.toast.show(format!(
                            "Baked grade → {} ({}³)",
                            output_path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("LUT"),
                            lut_size,
                        ));
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Bake failed — {e}"));
                    }
                }
                true
            }
            AppCommand::SetBakeLutSize(size) => {
                self.state.bake_lut_size = *size;
                true
            }

            // Grade versions
            AppCommand::SaveGradeVersion { .. } => {
                self.state.grade_version_command(cmd);
                true
            }
            AppCommand::SwitchGradeVersion { .. } => {
                // sync_and_refresh_frame for an immediate visual update,
                // even when paused.
                if self.state.grade_version_command(cmd) {
                    *needs_refresh = true;
                }
                true
            }
            AppCommand::DeleteGradeVersion { clip_id, version_index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    if *version_index < clip.grade_versions().len() {
                        clip.edit_grade_versions(|vs| vs.remove(*version_index));
                        match clip.active_grade_version() {
                            Some(idx) if idx == *version_index => clip.set_active_grade_version(None),
                            Some(idx) if idx > *version_index => clip.set_active_grade_version(Some(idx - 1)),
                            _ => {}
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::RenameGradeVersion { clip_id, version_index, name } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    clip.edit_grade_versions(|vs| {
                        if let Some(version) = vs.get_mut(*version_index) {
                            version.name = name.clone();
                        }
                    });
                }
                self.state.dirty = true;
                true
            }

            AppCommand::SaveLook { name } => {
                let Some(clip_id) = self.state.selected_clip else {
                    self.state.toast.show("Select a clip first to save a Look");
                    return true;
                };
                let snapshot = {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if seq.all_clips().any(|c| c.id == clip_id && c.holds_grade_mix()) {
                        self.state.toast.show(GRADE_MIX_NOT_COPIED);
                        return true;
                    }
                    seq.all_clips().find(|c| c.id == clip_id).map(|c| c.grade().clone())
                };
                let Some(node_graph) = snapshot else {
                    self.state.toast.show("Selected clip not found");
                    return true;
                };
                let look = klipp_timeline::Look::new(uuid::Uuid::new_v4(), name.trim().to_string(), node_graph);
                let label = look.name.clone();
                self.state.looks.push(look);
                self.state.dirty = true;
                self.state.toast.show(format!("Saved Look \"{label}\""));
                true
            }
            AppCommand::ApplyLook { look_id, clip_ids, append } => {
                let Some(look) = self.state.looks.iter().find(|l| l.id == *look_id).cloned() else {
                    self.state.toast.show("Look not found");
                    return true;
                };
                let mut applied = 0_usize;
                let mut refused: Vec<klipp_nodes::CannotAppend> = Vec::new();
                // Collect old/new graphs first (mutable borrow on seq); push history
                // entries afterwards (mutable borrow on state.command_histories).
                let mut history_pushes: Vec<(klipp_core::ClipId, klipp_nodes::NodeGraph, klipp_nodes::NodeGraph)> = Vec::new();
                {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    for clip_id in clip_ids {
                        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                            let old_graph = clip.grade().clone();
                            // Fresh node ids per clip, so two clips with the
                            // same Look don't collide, and the clip's own
                            // Input Transform kept: the Look's, if it has
                            // one, is the shot it was saved on.
                            if *append {
                                if let Err(why) = clip.edit_grade(|g| g.append(look.grade())) {
                                    if !refused.contains(&why) {
                                        refused.push(why);
                                    }
                                    continue;
                                }
                            } else {
                                clip.set_grade(clip.grade().with_grade_of(look.grade()));
                            }
                            applied += 1;
                            history_pushes.push((*clip_id, old_graph, clip.grade().clone()));
                        }
                    }
                }
                for (clip_id, old_graph, new_graph) in history_pushes {
                    self.state.push_history(crate::history::EditCommand::SetNodeGraph {
                        clip_id,
                        old_graph,
                        new_graph,
                    });
                }
                if applied > 0 {
                    self.state.dirty = true;
                    let mut told = format!("Applied \"{}\" to {applied} clip{}", look.name, if applied == 1 { "" } else { "s" });
                    if !refused.is_empty() {
                        told = format!("{told}; not to {}: {}", clip_ids.len() - applied, refusals_line(&refused));
                    }
                    self.state.toast.show(told);
                    *needs_refresh = true;
                } else if refused.is_empty() {
                    self.state.toast.show("No matching clips selected");
                } else {
                    self.state.toast.show(format!("Could not add \"{}\": {}", look.name, refusals_line(&refused)));
                }
                true
            }
            AppCommand::DeleteLook { look_id } => {
                let before = self.state.looks.len();
                self.state.looks.retain(|l| l.id != *look_id);
                if self.state.looks.len() != before {
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::RenameLook { look_id, new_name } => {
                if let Some(look) = self.state.looks.iter_mut().find(|l| l.id == *look_id) {
                    look.name = new_name.trim().to_string();
                    self.state.dirty = true;
                }
                true
            }

            _ => false,
        }
    }
}

impl KlippApp {
    /// A list of recorded edits applied forward as one history entry —
    /// one undo step — with the document marked changed and the engine
    /// told. What the control socket's plans go through; the Edit Plan
    /// window's buttons too.
    /// Send a published colour chart through the same renderer the
    /// viewer and the export use, and read it back.
    ///
    /// Everything the Colour Check window says is a reading of pixels
    /// that came out of that renderer. If the path itself moves colour,
    /// those readings are partly a measurement of Klipp, and a person
    /// chasing them will be grading the pipeline. It runs on both the
    /// CPU and the GPU path, because the viewer shows one and an export
    /// may take the other.
    pub(super) fn run_colour_self_check(&mut self) {
        let working = klipp_engine::video_decoder::working_colorspace();
        let both = klipp_compose::selfcheck::run_both(480, 320, working, klipp_compose::RenderOptions::default());
        let mut said = Vec::new();
        let mut ok = true;
        for (gpu, check) in &both {
            let path = if *gpu { "GPU" } else { "CPU" };
            for (severity, what) in check.findings() {
                if severity != "ok" {
                    ok = false;
                }
                said.push(format!("{path}: {what}"));
            }
        }
        let worst = both[0]
            .1
            .patches
            .iter()
            .zip(both[1].1.patches.iter())
            .map(|(a, b)| klipp_color::measure::delta_e2000(a.back, b.back))
            .fold(0.0f32, f32::max);
        if worst >= 1.0 {
            ok = false;
            said.push(format!("The CPU and the GPU disagree about the chart by {worst:.1} ΔE — a grade approved in the viewer is not the grade that exports."));
        }
        self.colour_check.selfcheck = Some((ok, said.join("\n")));
    }

    /// The runner the Colour Check window's tools use (#897).
    pub fn set_local_control(&mut self, run: crate::app::LocalControl) {
        self.local_control = Some(run);
        self.colour_check.tools_available = true;
    }

    /// Start a measured colour tool on a worker thread: it renders and
    /// measures frames through this UI, so it must not run on it.
    pub(super) fn run_colour_tool(&mut self, action: crate::panels::colour_check::ColourAction) {
        let Some(run) = self.local_control.clone() else { return };
        if self.colour_tool_reply.is_some() {
            return;
        }
        let selected_media = (self.state.selected_media_ids.len() == 1).then(|| self.state.selected_media_ids.iter().next().copied()).flatten();
        let Some(request) = colour_tool_request(action, self.state.selected_clip, &self.state.selected_clips, selected_media) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("klipp-colour-tool".into()).spawn(move || {
            let _ = tx.send(run(request));
        });
        if spawned.is_ok() {
            self.colour_tool_reply = Some(rx);
            self.colour_check.tool_running = Some(action);
            self.colour_check.tool_result = None;
        }
    }

    /// The running colour tool's answer, put in the window when it comes.
    pub(super) fn take_colour_tool_reply(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.colour_tool_reply else { return };
        let reply = match rx.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Asleep, egui would not look again until the mouse moved.
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the tool stopped without an answer".into()),
        };
        let name = self.colour_check.tool_running.take().map(|a| a.name()).unwrap_or("The tool");
        self.colour_tool_reply = None;
        self.colour_check.tool_result = Some(match reply {
            Ok(v) => (true, v["text"].as_str().map(str::to_string).unwrap_or_else(|| format!("{name}: done."))),
            Err(why) => (false, format!("{name}: {why}")),
        });
    }
}

/// The control request a Colour Check tool sends (#897), or `None` when
/// what it acts on is not selected. Match to Hero takes the selected
/// shots when there are two or more, else the whole picture track.
fn colour_tool_request(
    action: crate::panels::colour_check::ColourAction,
    clip: Option<klipp_core::ClipId>,
    clips: &[klipp_core::ClipId],
    media: Option<klipp_core::MediaId>,
) -> Option<serde_json::Value> {
    use crate::panels::colour_check::ColourAction as A;
    use serde_json::json;
    Some(match action {
        A::MatchToHero => {
            let mut v = json!({ "method": "match_scenes", "apply": true });
            if clips.len() >= 2 {
                v["clip_ids"] = json!(clips.iter().map(|c| c.0.to_string()).collect::<Vec<_>>());
            }
            v
        }
        A::BalanceSkin => json!({ "method": "skin_balance", "clip_id": clip?.0.to_string(), "apply": true }),
        A::GradeToLook => json!({ "method": "grade_to_target", "clip_id": clip?.0.to_string(), "apply": true }),
        A::AdoptLookFromTimeline => json!({ "method": "adopt_look", "from_timeline": true, "apply": true }),
        A::AdoptLookFromMedia => json!({ "method": "adopt_look", "media_id": media?.0.to_string(), "apply": true }),
        A::None | A::SelfCheck => return None,
    })
}

#[cfg(test)]
mod colour_tool_tests {
    use super::colour_tool_request;
    use crate::panels::colour_check::ColourAction;
    use klipp_core::{ClipId, MediaId};

    /// Each button asks for what an assistant would, applied; a tool
    /// with nothing selected to act on asks for nothing.
    #[test]
    fn each_tool_sends_its_request() {
        let (a, b) = (ClipId::new(), ClipId::new());
        let req = |action, clip, clips: &[ClipId], media| colour_tool_request(action, clip, clips, media);
        let one = req(ColourAction::MatchToHero, Some(a), &[a], None).unwrap();
        assert_eq!(one["method"], "match_scenes");
        assert!(one.get("clip_ids").is_none(), "one shot selected: the whole track");
        let two = req(ColourAction::MatchToHero, Some(a), &[a, b], None).unwrap();
        assert_eq!(two["clip_ids"].as_array().unwrap().len(), 2);
        let skin = req(ColourAction::BalanceSkin, Some(a), &[a], None).unwrap();
        assert_eq!((skin["method"].as_str(), skin["clip_id"].as_str(), skin["apply"].as_bool()), (Some("skin_balance"), Some(a.0.to_string().as_str()), Some(true)));
        assert_eq!(req(ColourAction::GradeToLook, Some(a), &[], None).unwrap()["method"], "grade_to_target");
        assert!(req(ColourAction::BalanceSkin, None, &[], None).is_none(), "no clip, no request");
        let m = MediaId::new();
        assert_eq!(req(ColourAction::AdoptLookFromMedia, None, &[], Some(m)).unwrap()["media_id"], m.0.to_string());
        assert!(req(ColourAction::AdoptLookFromMedia, None, &[], None).is_none());
        assert_eq!(req(ColourAction::AdoptLookFromTimeline, None, &[], None).unwrap()["from_timeline"], true);
        assert!(req(ColourAction::SelfCheck, None, &[], None).is_none());
    }
}

/// `value + adjustment` held to the temperature and tint sliders' ±50,
/// and whether it had to be (#405: the clamp used to be silent).
pub(super) fn add_on_rail(value: f32, adjustment: f32) -> (f32, bool) {
    let want = value + adjustment;
    let held = want.clamp(-50.0, 50.0);
    (held, (held - want).abs() > 0.01)
}

/// A qualifier keyed on the colour picked at `(nx, ny)` (0..=1 over the
/// frame) of an sRGB frame: `base` with its hue centred on the colour and
/// its saturation range around it. Read as the qualifier reads a pixel
/// (`picture_hsl`), from the mean of a 5×5 patch, so one noisy pixel does
/// not set the key. `None` when the frame is empty.
pub(super) fn qualifier_from_pick(
    rgba: &[u8],
    width: u32,
    height: u32,
    nx: f32,
    ny: f32,
    base: klipp_core::QualifierSettings,
) -> Option<klipp_core::QualifierSettings> {
    Some(qualifier_keyed_on(picked_linear(rgba, width, height, nx, ny)?, base))
}

/// The colour at `(nx, ny)` (0..=1 over the frame) of an sRGB frame, in
/// linear light: the mean of a 5×5 patch, so one noisy pixel does not
/// decide it. `None` when the frame is empty.
pub(super) fn picked_linear(rgba: &[u8], width: u32, height: u32, nx: f32, ny: f32) -> Option<[f32; 3]> {
    if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
        return None;
    }
    let (cx, cy) = ((nx.clamp(0.0, 1.0) * (width - 1) as f32).round() as i64, (ny.clamp(0.0, 1.0) * (height - 1) as f32).round() as i64);
    let mut patch = Vec::with_capacity(25 * 4);
    for y in (cy - 2).max(0)..=(cy + 2).min(height as i64 - 1) {
        for x in (cx - 2).max(0)..=(cx + 2).min(width as i64 - 1) {
            let i = (y as usize * width as usize + x as usize) * 4;
            patch.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&patch);
    let n = (linear.len() / 4) as f32;
    Some([0, 1, 2].map(|c| linear.chunks_exact(4).map(|p| p[c]).sum::<f32>() / n))
}

/// How far either side of a picked hue its range reaches: 30°.
pub(super) const HUE_PICK_HALF_WIDTH: f32 = 1.0 / 12.0;

/// `points` with a range round `hue` (0..1): a point at the hue and one
/// either side of it, all neutral, so nothing changes until the middle
/// one is dragged; the points already inside that range give way to it,
/// the rest stay. Near red the range stops at the curve's end rather
/// than wrapping round to the other (#612).
pub(super) fn hue_range_points(points: &[[f32; 2]], hue: f32) -> Vec<[f32; 2]> {
    let hue = hue.rem_euclid(1.0);
    let (lo, hi) = ((hue - HUE_PICK_HALF_WIDTH).max(0.0), (hue + HUE_PICK_HALF_WIDTH).min(1.0));
    let mut out: Vec<[f32; 2]> = points.iter().copied().filter(|p| p[0] < lo || p[0] > hi).collect();
    for x in [lo, hue, hi] {
        if !out.iter().any(|p| (p[0] - x).abs() < 1e-4) {
            out.push([x, 0.5]);
        }
    }
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    out
}

/// `base` keyed on a colour in linear light, read as the qualifier reads
/// a pixel (`picture_hsl`): its hue centred there, its saturation range
/// around it.
/// How many points of a drawn stroke are read (#895).
pub(super) const STROKE_SAMPLES: usize = 16;

/// A drawn stroke's points to read (#895): at most `most`, spread evenly
/// along it by its length, its ends among them; a click, one point.
pub(super) fn stroke_points(points: &[[f32; 2]], most: usize) -> Vec<[f32; 2]> {
    if points.len() <= 1 || most <= 1 {
        return points.iter().take(1).copied().collect();
    }
    let seg: Vec<f32> = points.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).collect();
    let total: f32 = seg.iter().sum();
    if total <= 1e-6 {
        return vec![points[0]];
    }
    (0..most)
        .map(|k| {
            let mut want = total * k as f32 / (most - 1) as f32;
            for (i, len) in seg.iter().enumerate() {
                if want <= *len || i == seg.len() - 1 {
                    let t = if *len > 0.0 { (want / len).clamp(0.0, 1.0) } else { 0.0 };
                    let (a, b) = (points[i], points[i + 1]);
                    return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                }
                want -= len;
            }
            points[points.len() - 1]
        })
        .collect()
}

/// `base` with a stroke's `samples` (display-encoded) added to what it
/// keeps, or with `remove` to what it takes out (#895): the key in 3D mode
/// and on, everything else as it was.
pub(super) fn qualifier_with_stroke(base: klipp_core::QualifierSettings, samples: &[[f32; 3]], remove: bool) -> klipp_core::QualifierSettings {
    let mut q = klipp_core::QualifierSettings { enabled: true, mode: klipp_core::QualifierMode::ThreeD, ..base };
    if remove {
        q.remove_samples.extend_from_slice(samples);
    } else {
        q.keep_samples.extend_from_slice(samples);
    }
    q
}

#[cfg(test)]
mod stroke_tests {
    /// A stroke is read at most 16 points along it, evenly by length, its
    /// ends included; a click at its one point.
    #[test]
    fn a_stroke_is_read_evenly_along_it() {
        let pts = super::stroke_points(&[[0.0, 0.5], [0.1, 0.5], [0.2, 0.5], [0.8, 0.5]], 4);
        assert_eq!(pts.len(), 4);
        for (p, x) in pts.iter().zip([0.0f32, 0.8 / 3.0, 1.6 / 3.0, 0.8]) {
            assert!((p[0] - x).abs() < 1e-5 && (p[1] - 0.5).abs() < 1e-6, "{pts:?}");
        }
        assert_eq!(super::stroke_points(&[[0.3, 0.4]], 16), vec![[0.3, 0.4]]);
        assert!(super::stroke_points(&[], 16).is_empty());
    }

    /// A keep stroke's samples join what the key keeps, a remove stroke's
    /// what it takes out; either turns the key on in 3D mode and leaves the
    /// rest of it.
    #[test]
    fn a_stroke_adds_to_its_side_of_the_volume() {
        let base = klipp_core::QualifierSettings { invert: true, keep_samples: vec![[0.1, 0.2, 0.3]], ..Default::default() };
        let kept = super::qualifier_with_stroke(base.clone(), &[[0.5, 0.5, 0.5]], false);
        assert!(kept.enabled && kept.mode == klipp_core::QualifierMode::ThreeD && kept.invert);
        assert_eq!(kept.keep_samples, vec![[0.1, 0.2, 0.3], [0.5, 0.5, 0.5]]);
        let out = super::qualifier_with_stroke(base, &[[0.9, 0.1, 0.1]], true);
        assert_eq!((out.keep_samples.len(), out.remove_samples.clone()), (1, vec![[0.9, 0.1, 0.1]]));
    }
}

pub(super) fn qualifier_keyed_on(rgb: [f32; 3], base: klipp_core::QualifierSettings) -> klipp_core::QualifierSettings {
    // In RGB mode (#895), each channel within 0.1 of the picked colour's.
    if base.mode == klipp_core::QualifierMode::Rgb {
        let e = klipp_effects::cpu_grade::qualifier_rgb(rgb);
        let (lo, hi) = (|v: f32| (v - 0.1).max(0.0), |v: f32| (v + 0.1).min(1.0));
        return klipp_core::QualifierSettings {
            enabled: true,
            red_low: lo(e[0]),
            red_high: hi(e[0]),
            green_low: lo(e[1]),
            green_high: hi(e[1]),
            blue_low: lo(e[2]),
            blue_high: hi(e[2]),
            ..base
        };
    }
    let hsl = klipp_effects::cpu_grade::picture_hsl(rgb);
    klipp_core::QualifierSettings {
        enabled: true,
        hue_center: hsl.h * 360.0,
        hue_width: 30.0,
        sat_low: (hsl.s - 0.2).max(0.0),
        sat_high: (hsl.s + 0.2).min(1.0),
        ..base
    }
}

#[cfg(test)]
mod qualifier_mode_pick_tests {
    /// The eyedropper keys by the qualifier's mode (#895): in RGB mode each
    /// channel within 0.1 of the picked colour's, the HSL window left as it
    /// was; in HSL mode the hue and saturation, the RGB ranges left.
    #[test]
    fn a_pick_keys_by_the_qualifiers_mode() {
        let picked = [0.6f32, 0.2, 0.05];
        let e = klipp_effects::cpu_grade::qualifier_rgb(picked);
        let rgb = super::qualifier_keyed_on(picked, klipp_core::QualifierSettings { mode: klipp_core::QualifierMode::Rgb, ..Default::default() });
        assert!(rgb.enabled);
        assert!((rgb.red_low - (e[0] - 0.1)).abs() < 1e-6 && (rgb.red_high - (e[0] + 0.1).min(1.0)).abs() < 1e-6);
        assert!((rgb.green_low - (e[1] - 0.1).max(0.0)).abs() < 1e-6 && (rgb.blue_low - (e[2] - 0.1).max(0.0)).abs() < 1e-6);
        assert_eq!(rgb.hue_center, klipp_core::QualifierSettings::default().hue_center, "the HSL window is left");
        let hsl = super::qualifier_keyed_on(picked, Default::default());
        assert!(hsl.hue_center > 0.0 && hsl.red_low == 0.0 && hsl.red_high == 1.0, "HSL mode keys the hue, the RGB ranges left");
    }
}

/// Why Auto White Balance cannot read the frame buffer's bytes for a
/// clip spanning `clip_span`, or `None` when it can: the bytes must be the
/// picture on screen (not while the GPU stack presents without them), the
/// SDR picture (an HDR monitor mode publishes PQ preview or false colour,
/// whose "neutrals" mean nothing), and a frame of the clip being balanced.
pub(super) fn auto_wb_frame_refusal(
    gpu_output_live: bool,
    hdr_monitor: bool,
    complete: bool,
    bytes_at: Option<klipp_core::RationalTime>,
    clip_span: Option<(klipp_core::RationalTime, klipp_core::RationalTime)>,
) -> Option<&'static str> {
    if gpu_output_live {
        return Some("pause playback first — it reads the frame on screen, and a playing frame is not held to read");
    }
    if hdr_monitor {
        return Some("turn the HDR monitor off first — the viewer shows an HDR preview or false colour, not the picture it balances");
    }
    // A progress frame, or one drawn while a stream lagged, is published
    // under the playhead without every clip's picture in it.
    if !complete {
        return Some("the frame is still being drawn — press again in a moment");
    }
    match (bytes_at, clip_span) {
        (Some(at), Some((a, b))) if at >= a && at < b => None,
        (_, None) => Some("the clip is no longer on the timeline"),
        _ => Some("the frame on screen is not of this clip — park the playhead on it first"),
    }
}

/// Where the playhead goes when a clip is selected on the Colour page:
/// nowhere if it is already in the clip, else back to the frame the
/// clip was last looked at there, else its first frame.
pub(super) fn colour_seek_on_select(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::RationalTime,
    remembered: Option<klipp_core::RationalTime>,
) -> Option<klipp_core::RationalTime> {
    let inside = |t: klipp_core::RationalTime| t >= clip.timeline_in && t < clip.timeline_out;
    if inside(playhead) {
        return None;
    }
    Some(remembered.filter(|&t| inside(t)).unwrap_or(clip.timeline_in))
}

/// What to do with the graph kept from before the input this frame.
#[derive(Debug, PartialEq)]
enum Keep {
    /// Take it afresh: the input may begin a gesture.
    Take,
    /// Keep the one taken at the press, the pointer still held.
    Hold,
    Drop,
}

/// A new press, key or text takes the graph, unless a gesture is under
/// way and its snapshot already taken; a pointer held, or let go this
/// frame, keeps it, for a drag that begins frames after its press and a
/// click that lands on its release; otherwise it goes.
fn keep_before(began: bool, held: bool, in_gesture: bool) -> Keep {
    if began && !in_gesture {
        Keep::Take
    } else if held {
        Keep::Hold
    } else {
        Keep::Drop
    }
}

#[cfg(test)]
mod keep_before_tests {
    use super::{keep_before, Keep};

    /// A press takes the graph from before it; a drag holds it; a let go
    /// drops it; a gesture under way keeps its own snapshot.
    #[test]
    fn the_graph_before_a_press_is_kept_through_the_drag() {
        assert_eq!(keep_before(true, true, false), Keep::Take, "a press");
        assert_eq!(keep_before(false, true, false), Keep::Hold, "dragging");
        assert_eq!(keep_before(false, false, false), Keep::Drop, "let go");
        assert_eq!(keep_before(true, false, false), Keep::Take, "a key or typed text");
        assert_eq!(keep_before(true, true, true), Keep::Hold, "a press inside a gesture keeps the gesture's");
    }
}

#[cfg(test)]
mod node_param_undo_tests {
    //! Tests for the BeginNodeParamEdit / EndNodeParamEdit round-trip
    //! invariant: a gesture that snapshots the graph, mutates it, and commits
    //! a `SetNodeGraph` history entry must preserve the pre-gesture graph so
    //! the existing undo replay can restore it.
    //!
    //! Full dispatch testing requires a `KlippApp` instance (event channels,
    //! engine handle, egui context), which is not practical in a unit test.
    //! These tests validate the *data contract* my handlers depend on: the
    //! `EditCommand::SetNodeGraph` entry round-trips the old/new graphs and
    //! is structurally equivalent to what the existing `ToggleNodeEnabled`
    //! handler (known-working) produces.

    use crate::history::{EditCommand, HistoryEntry};
    use klipp_core::ClipId;

    #[test]
    fn set_node_graph_history_entry_preserves_snapshot() {
        // Simulate BeginNodeParamEdit: snapshot the current graph.
        let original = klipp_nodes::NodeGraph::default();
        let old_graph = original.clone();
        let original_node_count = original.nodes.len();

        // Simulate several SetNodeParam commands mutating the working graph.
        let mut working = original.clone();
        if let Some(first_node) = working.nodes.first_mut() {
            // Mutate something observable — a new label marks the working
            // graph as distinct from the snapshot.
            first_node.label = "mutated".to_string();
        }

        // Simulate EndNodeParamEdit: push a SetNodeGraph history entry with
        // the snapshot and the post-gesture graph.
        let entry = HistoryEntry {
            command: EditCommand::SetNodeGraph {
                clip_id: ClipId::new(),
                old_graph,
                new_graph: working.clone(),
            },
        };

        // The history entry must still hold an unmodified snapshot of the
        // pre-gesture graph — that's what undo replays.
        match &entry.command {
            EditCommand::SetNodeGraph { old_graph, new_graph, .. } => {
                assert_eq!(old_graph.nodes.len(), original_node_count);
                // old_graph must not have picked up the mutation we made to
                // the working copy.
                if let Some(snap_first) = old_graph.nodes.first() {
                    assert_ne!(snap_first.label, "mutated",
                        "snapshot must be independent of the working graph");
                }
                // And the new_graph must reflect the mutation.
                if let Some(new_first) = new_graph.nodes.first() {
                    assert_eq!(new_first.label, "mutated");
                }
            }
            other => panic!("expected SetNodeGraph, got {:?}", other),
        }
    }

    /// An atomic click-to-reset (Begin + SetNodeParam + End in the same
    /// frame) must produce exactly one `SetNodeGraph` history entry whose
    /// `old_graph` holds the pre-click state. This models the behavior of
    /// `push_atomic_node_edit` in node_inspector.rs.
    #[test]
    fn atomic_edit_creates_single_history_entry() {
        use klipp_core::AppCommand;
        use klipp_nodes::NodeId;

        // Simulated command stream produced by a single "reset" click:
        let clip_id = ClipId::new();
        let node_id = NodeId::new_v4();
        let cmds: Vec<AppCommand> = vec![
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam {
                clip_id,
                node_id,
                param: "exposure".into(),
                value: 0.0,
            },
            AppCommand::EndNodeParamEdit { clip_id },
        ];

        // The stream must contain exactly one Begin and one End bracketing
        // the SetNodeParam. That's the contract a handler needs to produce a
        // single history entry.
        let begin_count = cmds.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count();
        let end_count = cmds.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count();
        let set_count = cmds.iter().filter(|c| matches!(c, AppCommand::SetNodeParam { .. })).count();
        assert_eq!(begin_count, 1, "atomic edit must emit exactly one Begin");
        assert_eq!(end_count, 1, "atomic edit must emit exactly one End");
        assert_eq!(set_count, 1, "atomic edit here carries one SetNodeParam");

        // Order: Begin must precede the SetNodeParam, End must follow.
        let begin_idx = cmds.iter().position(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).unwrap();
        let set_idx = cmds.iter().position(|c| matches!(c, AppCommand::SetNodeParam { .. })).unwrap();
        let end_idx = cmds.iter().position(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).unwrap();
        assert!(begin_idx < set_idx && set_idx < end_idx,
            "expected Begin < SetNodeParam < End, got {begin_idx}/{set_idx}/{end_idx}");
    }

    /// Two consecutive atomic edits must produce two separate Begin/End
    /// brackets so they become two distinct history entries, not one
    /// coalesced undo step.
    #[test]
    fn multiple_atomic_edits_create_separate_brackets() {
        use klipp_core::AppCommand;
        use klipp_nodes::NodeId;

        let clip_id = ClipId::new();
        let node_id = NodeId::new_v4();
        let cmds: Vec<AppCommand> = vec![
            // First atomic edit
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam { clip_id, node_id, param: "exposure".into(), value: 0.5 },
            AppCommand::EndNodeParamEdit { clip_id },
            // Second atomic edit
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam { clip_id, node_id, param: "contrast".into(), value: 0.3 },
            AppCommand::EndNodeParamEdit { clip_id },
        ];

        let begin_count = cmds.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count();
        let end_count = cmds.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count();
        assert_eq!(begin_count, 2, "two edits must emit two Begins");
        assert_eq!(end_count, 2, "two edits must emit two Ends");

        // The two brackets must be disjoint: the first End must precede the
        // second Begin. If they were interleaved, the handler would coalesce
        // them into a single history entry.
        let positions: Vec<usize> = cmds
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                AppCommand::BeginNodeParamEdit { .. } | AppCommand::EndNodeParamEdit { .. } => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 4);
        assert!(positions[0] < positions[1] && positions[1] < positions[2] && positions[2] < positions[3],
            "brackets must not interleave");
    }

    #[test]
    fn begin_end_cycle_with_no_mutation_still_produces_valid_entry() {
        // Edge case: user clicks a slider but releases without dragging.
        // Begin snapshots, End commits an identical graph. The entry should
        // still be well-formed (undoing a no-op restores an identical graph).
        let graph = klipp_nodes::NodeGraph::default();
        let entry = HistoryEntry {
            command: EditCommand::SetNodeGraph {
                clip_id: ClipId::new(),
                old_graph: graph.clone(),
                new_graph: graph.clone(),
            },
        };
        match &entry.command {
            EditCommand::SetNodeGraph { old_graph, new_graph, .. } => {
                assert_eq!(old_graph.nodes.len(), new_graph.nodes.len());
            }
            _ => panic!(),
        }
    }
}

#[cfg(test)]
mod add_on_rail_tests {
    use super::add_on_rail;

    #[test]
    fn within_the_range_is_kept_and_past_it_is_held_and_said() {
        assert_eq!(add_on_rail(10.0, -12.0), (-2.0, false));
        assert_eq!(add_on_rail(40.0, 20.0), (50.0, true));
        assert_eq!(add_on_rail(-45.0, -30.0), (-50.0, true));
    }
}

#[cfg(test)]
mod qualifier_pick_tests {
    use super::{hue_range_points, picked_linear, qualifier_from_pick};

    /// #612: a pick puts three neutral points round the hue, the middle
    /// one on it; points inside the range give way, the rest stay; near
    /// red the range stops at the curve's end.
    #[test]
    fn a_hue_pick_puts_a_neutral_range_round_the_hue() {
        let picked = hue_range_points(&[], 0.5);
        assert_eq!(picked.len(), 3);
        assert!((picked[1][0] - 0.5).abs() < 1e-6 && picked.iter().all(|p| p[1] == 0.5), "{picked:?}");
        assert!((picked[2][0] - picked[0][0] - 2.0 / 12.0).abs() < 1e-6, "30° either side");
        let kept = hue_range_points(&[[0.1, 0.7], [0.52, 0.9], [0.9, 0.3]], 0.5);
        assert!(kept.contains(&[0.1, 0.7]) && kept.contains(&[0.9, 0.3]), "outside the range they stay: {kept:?}");
        assert!(!kept.iter().any(|p| p[1] == 0.9), "inside it they give way: {kept:?}");
        assert!(kept.windows(2).all(|w| w[0][0] < w[1][0]), "in order");
        let red = hue_range_points(&[], 0.01);
        assert_eq!(red[0], [0.0, 0.5], "stops at the curve's start: {red:?}");
        // And the hue the pick reads is the one the curve acts on: an
        // orange patch reads as orange.
        let frame: Vec<u8> = (0..16 * 8).flat_map(|_| [230u8, 120, 30, 255]).collect();
        let hue = klipp_effects::cpu_grade::picture_hsl(picked_linear(&frame, 16, 8, 0.5, 0.5).unwrap()).h * 360.0;
        assert!((hue - 28.0).abs() < 2.0, "orange: {hue}");
    }

    /// A frame of `left` on its left half and `right` on its right.
    fn halves(left: [u8; 3], right: [u8; 3]) -> Vec<u8> {
        (0..16 * 8).flat_map(|i| { let c = if i % 16 < 8 { left } else { right }; [c[0], c[1], c[2], 255] }).collect()
    }

    #[test]
    fn the_pick_keys_the_colour_under_it_and_the_qualifier_passes_it() {
        let (orange, blue) = ([230, 120, 30], [40, 80, 200]);
        let frame = halves(orange, blue);
        let base = klipp_core::QualifierSettings { hue_soft: 7.0, ..Default::default() };
        let q = qualifier_from_pick(&frame, 16, 8, 0.1, 0.5, base.clone()).unwrap();
        assert!(q.enabled);
        assert!((q.hue_center - 28.0).abs() < 2.0, "orange: {}", q.hue_center);
        assert_eq!(q.hue_soft, 7.0, "the node's softness is kept");
        // The qualifier's own matte: the picked colour in, the other out.
        let key = |c: [u8; 3]| {
            let mut px = klipp_effects::srgb_u8_to_linear_f32_rgba(&[c[0], c[1], c[2], 255]);
            klipp_effects::cpu_grade::qualifier_matte(
                &mut px, q.hue_center / 360.0, q.hue_width / 360.0, q.hue_soft / 360.0,
                q.sat_low, q.sat_high, q.sat_soft, q.lum_low, q.lum_high, q.lum_soft, q.invert,
            );
            px[0]
        };
        assert!(key(orange) > 0.99, "picked colour keyed: {}", key(orange));
        assert!(key(blue) < 0.01, "other colour not: {}", key(blue));
        assert!(qualifier_from_pick(&[], 0, 0, 0.5, 0.5, base).is_none());
    }
}

#[cfg(test)]
mod auto_wb_frame_tests {
    use super::auto_wb_frame_refusal;
    use klipp_core::RationalTime;

    fn t(f: i64) -> RationalTime { RationalTime { value: f, rate: 30 } }

    #[test]
    fn a_parked_frame_of_the_clip_is_read() {
        assert_eq!(auto_wb_frame_refusal(false, false, true, Some(t(15)), Some((t(10), t(40)))), None);
    }

    /// #404: while the GPU stack presents without bytes, the buffer holds
    /// an older parked frame.
    #[test]
    fn a_zero_copy_frame_on_screen_is_refused() {
        assert!(auto_wb_frame_refusal(true, false, true, Some(t(15)), Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn bytes_of_another_clip_are_refused() {
        assert!(auto_wb_frame_refusal(false, false, true, Some(t(45)), Some((t(10), t(40)))).is_some());
        assert!(auto_wb_frame_refusal(false, false, true, None, Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn an_hdr_monitor_frame_is_not_read() {
        assert!(auto_wb_frame_refusal(false, true, true, Some(t(15)), Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn a_frame_still_being_drawn_is_not_read() {
        assert!(auto_wb_frame_refusal(false, false, false, Some(t(15)), Some((t(10), t(40)))).is_some());
    }
}

#[cfg(test)]
mod colour_seek_tests {
    use super::colour_seek_on_select;
    use klipp_core::RationalTime;
    use klipp_timeline::Clip;

    fn t(secs: i64) -> RationalTime { RationalTime { value: secs * 30, rate: 30 } }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_playhead_already_in_the_clip_stays_put() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(15), None), None);
    }

    #[test]
    fn a_playhead_elsewhere_goes_to_the_first_frame() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), None), Some(t(10)));
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(20), None), Some(t(10)));
    }

    #[test]
    fn returning_to_a_clip_goes_back_to_where_it_was_graded() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), Some(t(17))), Some(t(17)));
    }

    /// A remembered frame the clip no longer covers (it was trimmed)
    /// is ignored rather than followed out of the clip.
    #[test]
    fn a_remembered_frame_outside_a_trimmed_clip_is_ignored() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), Some(t(25))), Some(t(10)));
    }
}

/// The graph an added node goes into: the clip's top-level one when the
/// sender names the node (the control socket, whose reply says the id
/// `get_grade` will show), else the one the editor has descended into.
pub(super) fn add_node_path(node_id: Option<uuid::Uuid>, descent: &[klipp_nodes::NodeId]) -> Vec<klipp_nodes::NodeId> {
    if node_id.is_some() { Vec::new() } else { descent.to_vec() }
}

/// A new, unwired node of `kind` with id `id` at `position`, as the v1
/// canvas added one before adds went by path (#1476).
#[cfg(test)]
fn push_node(graph: &mut klipp_nodes::NodeGraph, kind: klipp_nodes::NodeKind, id: klipp_nodes::NodeId, position: (f32, f32)) {
    graph.nodes.push(klipp_nodes::Node {
        id,
        kind,
        label: kind.display_name().to_string(),
        enabled: true,
        params: klipp_nodes::NodeParams::default(),
        keyframes: klipp_nodes::NodeKeyframeTracks::default(),
        output_link: None,
        input_links: Vec::new(),
    });
    graph.node_positions.insert(id, klipp_nodes::NodePos { x: position.0, y: position.1 });
}

#[cfg(test)]
mod add_node_tests {
    use super::{add_node_path, push_node, KlippApp};
    use klipp_nodes::{Node, NodeGraph, NodeId, NodeKind};

    /// A top-level graph holding a Compound, and the Compound's id.
    fn with_compound() -> (NodeGraph, NodeId) {
        let mut root = NodeGraph::default();
        let compound = NodeId::new_v4();
        let params = klipp_nodes::NodeParams { compound_subgraph: Some(NodeGraph::default()), ..Default::default() };
        root.nodes.push(Node {
            id: compound, kind: NodeKind::Compound, label: "Compound".into(), enabled: true, params,
            keyframes: Default::default(), output_link: None, input_links: vec![],
        });
        (root, compound)
    }

    fn ids(g: &NodeGraph) -> Vec<NodeId> {
        g.nodes.iter().map(|n| n.id).collect()
    }

    fn inside(root: &NodeGraph, compound: NodeId) -> &NodeGraph {
        root.nodes.iter().find(|n| n.id == compound).and_then(|n| n.params.compound_subgraph.as_ref()).expect("the compound's graph")
    }

    /// The socket's id is the node's, in the top-level graph `get_grade`
    /// reports, even while the editor is inside a Compound (#1168).
    #[test]
    fn a_named_node_goes_to_the_top_level_graph_with_its_id() {
        let (mut root, compound) = with_compound();
        let id = NodeId::new_v4();
        let path = add_node_path(Some(id), &[compound]);
        push_node(KlippApp::resolve_graph_at_path_mut(&mut root, &path).unwrap(), NodeKind::LumaKey, id, (10.0, 0.0));
        let added = root.nodes.iter().find(|n| n.id == id).expect("in the top-level graph");
        assert_eq!(added.kind, NodeKind::LumaKey);
        assert!(!ids(inside(&root, compound)).contains(&id));
    }

    /// The editor's own add goes where the editor is.
    #[test]
    fn an_editor_add_goes_into_the_compound_it_shows() {
        let (mut root, compound) = with_compound();
        let id = NodeId::new_v4();
        let path = add_node_path(None, &[compound]);
        push_node(KlippApp::resolve_graph_at_path_mut(&mut root, &path).unwrap(), NodeKind::Exposure, id, (0.0, 0.0));
        assert!(!ids(&root).contains(&id));
        assert!(ids(inside(&root, compound)).contains(&id));
    }
}

impl crate::state::AppState {
    /// A white balance's correction added to the clip's balance, as Auto
    /// White Balance and its picker land theirs: the Corrector's
    /// temperature and tint (a legacy Colour Balance node's when there is
    /// no Corrector), each held to its range, as one undo step. Whether
    /// either reached the end of its range, or `None` when the grade has
    /// no balance to add to. One place, so the dispatch and the parity
    /// land a computed balance alike.
    pub(crate) fn apply_white_balance(&mut self, clip_id: klipp_core::ClipId, temp_adj: f32, tint_adj: f32) -> Option<bool> {
        let seq = &mut self.sequences[self.active_sequence_idx];
        let clip = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id))?;
        let old_graph = clip.grade().clone();
        let railed = clip.edit_grade(|graph| {
            let target = graph.nodes.iter().position(|n| n.kind == klipp_nodes::NodeKind::Corrector)
                .or_else(|| graph.nodes.iter().position(|n| n.kind == klipp_nodes::NodeKind::ColorBalance))?;
            let node = &mut graph.nodes[target];
            let (t, t_railed) = add_on_rail(node.params.temperature.unwrap_or(0.0), temp_adj);
            let (g, g_railed) = add_on_rail(node.params.tint.unwrap_or(0.0), tint_adj);
            node.params.temperature = Some(t);
            node.params.tint = Some(g);
            Some(t_railed || g_railed)
        })?;
        let new_graph = clip.grade().clone();
        self.active_history_mut().push(crate::history::HistoryEntry {
            command: crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph },
        });
        self.dirty = true;
        Some(railed)
    }
}

/// A grade note moved by `delta` at the level `path`; recorded as its
/// own step unless `record` is off, inside a layout gesture (#1745).
pub(super) fn move_sticky_note(state: &mut AppState, clip_id: klipp_core::ClipId, path: &[klipp_nodes::NodeId], sticky_id: klipp_nodes::NodeStickyNoteId, delta: (f32, f32), record: bool) -> bool {
    let moved = edit_clip_graph(state, clip_id, path, record, |graph| {
        if let Some(s) = graph.sticky_notes.iter_mut().find(|s| s.id == sticky_id) {
            s.pos.x += delta.0;
            s.pos.y += delta.1;
        }
    });
    matches!(moved, Some(Ok(())))
}

/// A node's settings at their defaults (#1742), keeping what the node
/// is: its effects, plug-in, LUT and matte files, and a group's graph.
/// The reset shortcut once put the whole `NodeParams` back to default,
/// which emptied a compound and took an effect node's effect away.
pub(super) fn reset_node_params(params: &mut klipp_nodes::NodeParams) {
    let kept = klipp_nodes::NodeParams {
        effects: std::mem::take(&mut params.effects),
        ofx: params.ofx.take(),
        lut_path: params.lut_path.take(),
        matte_image_path: params.matte_image_path.take(),
        matte_subject: params.matte_subject.take(),
        compound_subgraph: params.compound_subgraph.take(),
        ..Default::default()
    };
    *params = kept;
}

/// A per-node editor's settings command written to its node in `graph`:
/// the curves, hue curves, qualifier, luma key, difference key, soft clip,
/// grain, external matte channel, matte combine, LUT file, mask stack,
/// colour match or mask key (added, removed or a track cleared) that the
/// command carries, or a mask shape added to (or removed from) the grade's
/// Mask node. One place, so the app's dispatch and the inspector parity
/// apply them alike. `false` when `cmd` is none of these.
/// The mask at `from` in node `node_id`'s stack moved to `to` (#74):
/// whether it moved.
pub(super) fn move_mask(graph: &mut klipp_nodes::NodeGraph, node_id: klipp_nodes::NodeId, from: usize, to: usize) -> bool {
    let Some(masks) = graph.nodes.iter_mut().find(|n| n.id == node_id).and_then(|n| n.params.masks.as_mut()) else { return false };
    if from == to || from >= masks.masks.len() || to >= masks.masks.len() {
        return false;
    }
    let m = masks.masks.remove(from);
    masks.masks.insert(to, m);
    true
}

/// Where the mask at `i` is after the one at `from` moved to `to`.
pub(super) fn moved_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if to < from && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

pub(super) fn write_node_settings(graph: &mut klipp_nodes::NodeGraph, cmd: &AppCommand) -> bool {
    // A mask shape added or removed names its clip, not its node: the
    // grade's Mask node takes it, made (with its cutout) when there's none.
    match cmd {
        AppCommand::AddMaskShape { shape, .. } => {
            let (mask_node_id, _cutout_id) = graph.ensure_mask_cutout();
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == mask_node_id) {
                node.params.masks.get_or_insert_with(Default::default).masks.push(klipp_core::Mask::new(shape.clone()));
            }
            return true;
        }
        AppCommand::RemoveMaskShape { mask_index, .. } => {
            if let Some(masks) = graph.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Mask).and_then(|n| n.params.masks.as_mut()) {
                if *mask_index < masks.masks.len() {
                    masks.masks.remove(*mask_index);
                }
            }
            return true;
        }
        _ => {}
    }
    // A node's bypass switched, and its key track, by its parameter. A
    // bypass key holds: the switch is on or off, never between.
    match cmd {
        AppCommand::ToggleNodeEnabled { node_id, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                node.enabled = !node.enabled;
            }
            return true;
        }
        AppCommand::ResetNode { node_id, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                reset_node_params(&mut node.params);
            }
            return true;
        }
        AppCommand::AddKeyframe { node_id, param, time_secs, value, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                let track = node.keyframes.track_mut(param);
                track.set_keyframe(*time_secs, *value);
                if param == klipp_nodes::ENABLED_TRACK {
                    if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < 0.01) {
                        kf.interpolation = klipp_core::keyframe::Interpolation::Step;
                    }
                }
            }
            return true;
        }
        AppCommand::RemoveKeyframe { node_id, param, time_secs, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                node.keyframes.track_mut(param).remove_keyframe(*time_secs);
            }
            return true;
        }
        AppCommand::ClearKeyframes { node_id, param, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                node.keyframes.tracks.retain(|t| t.param_name != *param);
            }
            return true;
        }
        // A Difference Key's plate (which turns the node on), an External
        // Matte's image, a Layer Mixer input's settings: written as one
        // edit of the node, the app's dispatch and the test driver alike.
        AppCommand::SetNodeDifferenceKeyPlate { node_id, plate_path, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                node.params.difference_key.get_or_insert_with(Default::default).plate_path = plate_path.clone();
                node.enabled = true;
            }
            return true;
        }
        AppCommand::SetExternalMattePath { node_id, path, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                node.params.matte_image_path = path.clone();
                // A file chosen, or cleared, ends a subject binding (#1881).
                node.params.matte_subject = None;
            }
            return true;
        }
        AppCommand::SetNodeLayerMixerInput { node_id, port, input, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                let mixer = node.params.layer_mixer.get_or_insert_with(Default::default);
                let needed = usize::from(*port) + 1;
                if mixer.inputs.len() < needed {
                    mixer.inputs.resize_with(needed, klipp_nodes::LayerMixerInput::default);
                }
                mixer.inputs[usize::from(*port)] = *input;
            }
            return true;
        }
        // As the keyframe editor sets it: the key and the one before it
        // (the moves into and out of it).
        AppCommand::SetKeyframeInterpolation { node_id, param, time_secs, interpolation, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                let keys = node.keyframes.tracks.iter_mut().find(|t| t.param_name == *param).map(|t| &mut t.keyframes);
                if let Some((keys, i)) = keys.and_then(|k| k.iter().position(|kf| (kf.time_secs - *time_secs).abs() < 0.01).map(|i| (k, i))) {
                    klipp_core::keyframe::set_interpolation_at(keys, i, *interpolation);
                }
            }
            return true;
        }
        AppCommand::SetKeyframeEase { node_id, param, time_secs, ease, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                let keys = node.keyframes.tracks.iter_mut().find(|t| t.param_name == *param).map(|t| &mut t.keyframes);
                if let Some((keys, i)) = keys.and_then(|k| k.iter().position(|kf| (kf.time_secs - *time_secs).abs() < 0.01).map(|i| (k, i))) {
                    klipp_core::keyframe::ease_key(keys, i, *ease);
                }
            }
            return true;
        }
        AppCommand::SetKeyframeTangents { node_id, param, time_secs, tangent_in, tangent_out, .. } => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                if let Some(kf) = node.keyframes.track_mut(param).keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < 0.01) {
                    kf.tangent_in = *tangent_in;
                    kf.tangent_out = *tangent_out;
                }
            }
            return true;
        }
        _ => {}
    }
    let node_id = match cmd {
        AppCommand::SetNodeLut { node_id, .. } | AppCommand::SetNodeMasks { node_id, .. } | AppCommand::SetNodeColorMatch { node_id, .. } => *node_id,
        AppCommand::SetNodeCurves { node_id, .. } | AppCommand::SetNodeHueSatCurves { node_id, .. } => *node_id,
        AppCommand::AddMaskKeyframe { node_id, .. }
        | AppCommand::RemoveMaskKeyframe { node_id, .. }
        | AppCommand::ClearMaskKeyframes { node_id, .. }
        | AppCommand::MoveMaskKeyframe { node_id, .. }
        | AppCommand::SetMaskKeyframeInterpolation { node_id, .. } => *node_id,
        AppCommand::SetNodeQualifier { node_id, .. }
        | AppCommand::SetNodeLumaKey { node_id, .. }
        | AppCommand::SetNodeDifferenceKey { node_id, .. }
        | AppCommand::SetNodeSoftClip { node_id, .. }
        | AppCommand::SetNodeFilmGrain { node_id, .. }
        | AppCommand::SetExternalMatteChannel { node_id, .. }
        | AppCommand::SetMatteCombineOp { node_id, .. } => *node_id,
        _ => return false,
    };
    let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) else { return true };
    let params = &mut node.params;
    // A mask, by its place in the node's stack.
    fn mask(params: &mut klipp_nodes::NodeParams, mask_index: usize) -> Option<&mut klipp_core::Mask> {
        params.masks.as_mut()?.masks.get_mut(mask_index)
    }
    match cmd {
        // A key at this time is updated, else one is put in; "shape" keys
        // a freeform mask's shape as it is then.
        AppCommand::AddMaskKeyframe { mask_index, param, time_secs, value, .. } => {
            if let Some(m) = mask(params, *mask_index) {
                m.add_key(param, *time_secs, *value);
            }
        }
        AppCommand::RemoveMaskKeyframe { mask_index, param, time_secs, .. } => {
            if let Some(m) = mask(params, *mask_index) {
                m.remove_key(param, *time_secs);
            }
        }
        AppCommand::ClearMaskKeyframes { mask_index, param, .. } => {
            if let Some(m) = mask(params, *mask_index) {
                m.clear_track(param);
            }
        }
        AppCommand::MoveMaskKeyframe { mask_index, param, time_secs, new_time_secs, value, .. } => {
            if let Some(m) = mask(params, *mask_index) {
                m.move_key(param, *time_secs, *new_time_secs, *value);
            }
        }
        AppCommand::SetMaskKeyframeInterpolation { mask_index, param, time_secs, interpolation, .. } => {
            if let Some(m) = mask(params, *mask_index) {
                m.set_key_interpolation(param, *time_secs, *interpolation);
            }
        }
        AppCommand::SetNodeCurves { curves, .. } => {
            params.curves = Some(curves.clone());
            node.enabled = true;
        }
        AppCommand::SetNodeHueSatCurves { huesat, .. } => {
            params.huesat = Some(huesat.clone());
            node.enabled = true;
        }
        AppCommand::SetNodeQualifier { qualifier, .. } => params.qualifier = Some(qualifier.clone()),
        AppCommand::SetNodeLumaKey { luma_key, .. } => params.luma_key = Some(luma_key.clone()),
        AppCommand::SetNodeDifferenceKey { difference_key, .. } => {
            params.difference_key = Some(difference_key.clone());
            node.enabled = true;
        }
        AppCommand::SetNodeLut { path, .. } => params.lut_path = path.clone(),
        AppCommand::SetNodeColorMatch { settings, .. } => {
            params.color_match = Some(*settings);
            node.enabled = true;
        }
        AppCommand::SetNodeMasks { masks, .. } => {
            params.masks = Some(masks.clone());
            node.enabled = true;
        }
        AppCommand::SetNodeSoftClip { soft_clip, .. } => params.soft_clip = Some(soft_clip.clone()),
        AppCommand::SetNodeFilmGrain { grain, .. } => params.grain = Some(grain.clone()),
        AppCommand::SetExternalMatteChannel { channel, invert, .. } => {
            params.matte_channel = Some(match *channel {
                0 => klipp_nodes::MatteChannel::Alpha,
                1 => klipp_nodes::MatteChannel::Luminance,
                2 => klipp_nodes::MatteChannel::Red,
                3 => klipp_nodes::MatteChannel::Green,
                4 => klipp_nodes::MatteChannel::Blue,
                _ => klipp_nodes::MatteChannel::Luminance,
            });
            params.matte_invert = Some(*invert);
        }
        // Out-of-range falls back to the default rather than refusing the
        // command, matching the channel above: a bad discriminant should
        // not wedge the node.
        AppCommand::SetMatteCombineOp { op, .. } => {
            params.matte_combine_op = Some(klipp_nodes::MatteCombineOp::ALL.get(*op as usize).copied().unwrap_or_default());
        }
        _ => {}
    }
    true
}

/// A graph edit as the colour dispatch lands it: `mutate` run on the graph
/// at `path` in `clip_id`'s grade (an empty path is its top-level graph),
/// and, when `record`, one `SetNodeGraph` step of the whole top-level
/// graph. `record` is false while a node-param gesture is open, whose close
/// records the step. `None` when the active sequence has no such clip,
/// `Err` when `path` no longer leads to a graph (nothing is written).
pub(super) fn edit_clip_graph(
    state: &mut AppState,
    clip_id: klipp_core::ClipId,
    path: &[klipp_nodes::NodeId],
    record: bool,
    mutate: impl FnOnce(&mut klipp_nodes::NodeGraph),
) -> Option<Result<(), ()>> {
    let seq = state.active_sequence_idx;
    let clip = state.sequences[seq].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id))?;
    let old_graph = clip.grade().clone();
    if clip.edit_grade(|root| KlippApp::resolve_graph_at_path_mut(root, path).map(mutate)).is_err() {
        return Some(Err(()));
    }
    let new_graph = clip.grade().clone();
    if record {
        state.active_history_mut().push(crate::history::HistoryEntry {
            command: crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph },
        });
    }
    Some(Ok(()))
}

/// A mask key command (#1947) on the clip it names, in the graph of that
/// clip's grade that holds the Mask node, a group's or a compound's too.
/// A node id is only unique within one clip's grade (a duplicate's can
/// match another clip's), so the search never leaves the named clip; a
/// clip without the node is left alone and records nothing.
pub(super) fn mask_key_edit(state: &mut AppState, cmd: &AppCommand, record: bool) -> Option<Result<(), ()>> {
    let (clip_id, node_id) = match cmd {
        AppCommand::AddMaskKeyframe { clip_id, node_id, .. }
        | AppCommand::RemoveMaskKeyframe { clip_id, node_id, .. }
        | AppCommand::ClearMaskKeyframes { clip_id, node_id, .. }
        | AppCommand::MoveMaskKeyframe { clip_id, node_id, .. }
        | AppCommand::SetMaskKeyframeInterpolation { clip_id, node_id, .. } => (*clip_id, *node_id),
        _ => return None,
    };
    let seq = &state.sequences[state.active_sequence_idx];
    let root = seq.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == clip_id))?.grade();
    let path = root.path_to_node(node_id)?;
    edit_clip_graph(state, clip_id, &path, record, |graph| {
        write_node_settings(graph, cmd);
    })
}

/// A grade number as the colour dispatch writes it: `param` set to `value`
/// on the node `target` names in the graph at `path` in `clip_id`'s grade, in place and
/// with no step (a drag's gesture records one when it closes).
///
/// `target` is the node as [`AppState::reserved_target`] decided it: made
/// here first when it must be, in the same write. A bare `SetNodeParam`
/// records no step of its own, so that node is undone with the gesture
/// around it, as the value is.
pub(super) fn write_node_param(state: &mut AppState, clip_id: klipp_core::ClipId, path: &[klipp_nodes::NodeId], target: (klipp_nodes::NodeId, bool), param: &str, value: f32) {
    let seq = state.active_sequence_idx;
    let node_id = target.0;
    if let Some(clip) = state.sequences[seq].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)) {
        clip.edit_grade(|root| {
            if let Ok(graph) = KlippApp::resolve_graph_at_path_mut(root, path) {
                make_reserved(graph, target);
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                    node.params.apply_override(param, value);
                }
            }
        });
    }
}

/// The writes a zones node takes before its first edit (#2767): each
/// zone's boundary as [`klipp_core::HdrZonesSettings::for_source`] places
/// it for `clip_id`'s source. None when `param` is not a zone's, or the
/// node already has zones of its own.
pub(super) fn zones_seed(state: &AppState, clip_id: klipp_core::ClipId, node_id: klipp_nodes::NodeId, param: &str) -> Vec<(String, f32)> {
    if !param.starts_with("hdr_") {
        return Vec::new();
    }
    fn unset(graph: &klipp_nodes::NodeGraph, id: klipp_nodes::NodeId) -> Option<bool> {
        graph.nodes.iter().find_map(|n| {
            if n.id == id {
                return Some(n.kind == klipp_nodes::NodeKind::HdrZones && n.params.hdr_zones.is_none());
            }
            n.params.compound_subgraph.as_ref().and_then(|g| unset(g, id))
        })
    }
    let Some(clip) = state.sequences[state.active_sequence_idx].tracks.iter().find_map(|t| t.find_clip(clip_id)) else { return Vec::new() };
    if unset(clip.grade(), node_id) != Some(true) {
        return Vec::new();
    }
    let boundary = klipp_core::HdrZonesSettings::boundary_for(state.media_pool.scene_referred(clip.media_id()));
    klipp_nodes::params::HDR_ZONE_KEYS.iter().enumerate().skip(1).map(|(k, key)| (format!("hdr_{key}_boundary"), boundary[k])).collect()
}

/// A node-param gesture closed: one `SetNodeGraph` step from `old_graph`,
/// the grade as the gesture found it, to `clip_id`'s grade now. NodeGraph
/// has no PartialEq, so the step is pushed even for a gesture that changed
/// nothing; undoing it restores an identical graph.
pub(super) fn close_gesture(state: &mut AppState, clip_id: klipp_core::ClipId, old_graph: klipp_nodes::NodeGraph) {
    let seq = state.active_sequence_idx;
    let Some(clip) = state.sequences[seq].tracks.iter().find_map(|t| t.find_clip(clip_id)) else { return };
    let new_graph = clip.grade().clone();
    state.active_history_mut().push(crate::history::HistoryEntry {
        command: crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph },
    });
    state.dirty = true;
}

/// A node-param gesture taken back: `clip_id`'s grade restored to
/// `old_graph`, the grade as the gesture found it, with no step. `false`
/// when the active sequence has no such clip.
pub(super) fn abandon_gesture(state: &mut AppState, clip_id: klipp_core::ClipId, old_graph: klipp_nodes::NodeGraph) -> bool {
    let seq = state.active_sequence_idx;
    let Some(clip) = state.sequences[seq].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)) else { return false };
    clip.set_grade(old_graph);
    true
}

/// The graph a command naming `nodes` edits, as a path from the clip's
/// top-level graph: the one that holds them, found by id however deep in
/// Compounds, whatever graph the editor shows. The editor only names
/// nodes it shows, so for its commands that is where it is; a socket's
/// lands where the node is (#1179). Nodes in no graph at all (an id gone
/// stale) keep the editor's descent, where they change nothing. Nodes in
/// different graphs: `None`, no graph holds them all.
pub(super) fn graph_path_for(root: &klipp_nodes::NodeGraph, nodes: &[klipp_nodes::NodeId], descent: &[klipp_nodes::NodeId]) -> Option<Vec<klipp_nodes::NodeId>> {
    let mut paths = nodes.iter().filter_map(|id| root.path_to_node(*id));
    let Some(first) = paths.next() else { return Some(descent.to_vec()) };
    paths.all(|p| p == first).then_some(first)
}

impl crate::state::AppState {
    /// The node a grade command's `node_id` writes, and whether that node
    /// must be made first: an editor drawn before the clip had a Corrector
    /// names one that doesn't exist yet. Decides only, changing nothing:
    /// the handler makes the node ([`make_reserved`]) inside the edit it
    /// records, so undoing that edit takes the node back too.
    ///
    /// - The id the generated tabbed page reserved for the clip
    ///   (`pending_corrector`, M3 step 19): that node, made enabled even
    ///   beside a disabled Corrector, so the edit shows and the page's
    ///   rows and editors write one node (IC16).
    /// - Any other id: itself.
    pub(crate) fn reserved_target(&self, clip_id: klipp_core::ClipId, node_id: klipp_nodes::NodeId) -> (klipp_nodes::NodeId, bool) {
        if self.pending_corrector != Some((clip_id, node_id)) {
            return (node_id, false);
        }
        let Some(clip) = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id == clip_id) else { return (node_id, false) };
        (node_id, !clip.grade().nodes.iter().any(|n| n.id == node_id))
    }
}

/// What printer lights move (#1527): on the selected clip's grade (or the
/// stand-in standing for a sequence's own) at the level drawn, the node and
/// the offset it holds now, as [`GradeSnapshot::printer_light_target`]
/// gives them; the level's Corrector reserved when it has none.
///
/// [`GradeSnapshot::printer_light_target`]: crate::inspector::grade_view::GradeSnapshot::printer_light_target
pub(super) fn printer_light_target(state: &mut crate::state::AppState) -> Option<(klipp_graph_format::path::NodePath, (f32, f32, f32))> {
    let clip_id = state.selected_clip?;
    let descent = state.graph_descent_path.clone();
    let seq = &state.sequences[state.active_sequence_idx];
    let snapshot = match GradeStandIn::of(seq, clip_id) {
        Some(which) => crate::inspector::grade_view::snapshot_for_root(seq, clip_id, which.root(seq), &descent, &mut state.pending_corrector, &mut state.root_grade_start),
        None => seq.all_clips().find(|c| c.id == clip_id).and_then(|c| crate::inspector::grade_view::snapshot_for(c, &descent, false, &mut state.pending_corrector)),
    };
    snapshot.and_then(|s| s.printer_light_target())
}

/// `target`'s offset moved `points` printer points on `channel` (3: all
/// three), each `step` a point: the three values one path edit, the
/// reserved Corrector made in it when that is the target (#1527). The
/// points the offset stands at after.
pub(super) fn apply_printer_light(state: &mut crate::state::AppState, (addr, now): (klipp_graph_format::path::NodePath, (f32, f32, f32)), channel: u8, points: i32, step: f32) -> Result<[i32; 3], String> {
    let which = (channel < 3).then_some(channel as usize);
    let (x, y, m) = klipp_color::printer_light(now, which, points, step);
    let ops = [("offset_x", x), ("offset_y", y), ("offset_master", m)]
        .into_iter()
        .map(|(key, v)| klipp_graph_lift::ops::GraphOp::SetParam { path: addr.clone(), key: key.into(), value: klipp_graph_format::ParamValue::Float(v) })
        .collect();
    state.apply_path_ops_making_pending(ops, "Printer Lights")?;
    Ok(klipp_color::printer_points((x, y, m), step))
}

/// Make the Corrector [`crate::state::AppState::reserved_target`] said
/// must be made, in `graph`: the graph the command then writes.
pub(super) fn make_reserved(graph: &mut klipp_nodes::NodeGraph, (id, make): (klipp_nodes::NodeId, bool)) {
    if make && !graph.nodes.iter().any(|n| n.id == id) {
        graph.insert_corrector(id);
    }
}

#[cfg(test)]
mod graph_path_tests {
    use super::graph_path_for;
    use klipp_nodes::{Node, NodeGraph, NodeId, NodeKind};

    fn node(kind: NodeKind) -> Node {
        Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    /// A top-level node, a Compound holding two more, and their ids.
    fn graph() -> (NodeGraph, NodeId, NodeId, NodeId, NodeId) {
        let (top, a, b) = (node(NodeKind::Exposure), node(NodeKind::Contrast), node(NodeKind::Saturation));
        let (top_id, a_id, b_id) = (top.id, a.id, b.id);
        let mut inner = NodeGraph::default();
        inner.nodes.extend([a, b]);
        let mut compound = node(NodeKind::Compound);
        compound.params.compound_subgraph = Some(inner);
        let compound_id = compound.id;
        let mut root = NodeGraph::default();
        root.nodes.extend([top, compound]);
        (root, top_id, compound_id, a_id, b_id)
    }

    #[test]
    fn a_command_edits_the_graph_that_holds_its_nodes_whatever_the_editor_shows() {
        let (root, top, compound, a, b) = graph();
        // The editor at the top level, a node inside the Compound named.
        assert_eq!(graph_path_for(&root, &[a], &[]), Some(vec![compound]));
        // The editor inside the Compound, a top-level node named.
        assert_eq!(graph_path_for(&root, &[top], &[compound]), Some(vec![]));
        // Two nodes in the same graph: that graph.
        assert_eq!(graph_path_for(&root, &[a, b], &[]), Some(vec![compound]));
        // Two nodes in different graphs: no graph holds both.
        assert_eq!(graph_path_for(&root, &[top, a], &[]), None);
        // A stale id keeps the editor where it is, and changes nothing there.
        assert_eq!(graph_path_for(&root, &[NodeId::new_v4()], &[compound]), Some(vec![compound]));
    }
}

/// The node a pick answered for: in the grade `clip` names, a clip's
/// own or, for a stand-in, the sequence grade it edits, which no clip of
/// the timeline holds between colour dispatches.
fn picked_node(seq: &klipp_timeline::Sequence, clip: klipp_core::ClipId, node: klipp_nodes::NodeId) -> Option<&klipp_nodes::Node> {
    use klipp_timeline::GradeStage;
    let graph = if clip == seq.timeline_grade_clip_id() {
        seq.timeline_grade()
    } else if let Some((group, stage)) = seq.grade_groups.iter().find_map(|g| [GradeStage::Pre, GradeStage::Post].into_iter().find(|s| clip == g.stage_clip_id(*s)).map(|s| (g, s))) {
        group.stage(stage)
    } else {
        seq.all_clips().find(|c| c.id == clip).map(|c| c.grade())
    };
    graph?.nodes.iter().find(|n| n.id == node)
}

/// The path of the node a pick asks the engine to read (#1428): `node`
/// of the grade `clip` names, a clip's own, or, for a stand-in the Colour
/// page edits one of the sequence's own grades through, that grade (the
/// timeline grade, a grade group's stage). The engine reads a stand-in's
/// grade where it runs; it has no clip by the stand-in's id.
pub(super) fn sample_path(seq: &klipp_timeline::Sequence, clip: klipp_core::ClipId, node: klipp_nodes::NodeId) -> klipp_graph_format::path::NodePath {
    use klipp_graph_format::lifted::Stage;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_timeline::GradeStage;
    if clip == seq.timeline_grade_clip_id() {
        return NodePath::new(Root::TimelineGrade { sequence: seq.id.0 }, [node]);
    }
    for group in &seq.grade_groups {
        for (stage, at) in [(GradeStage::Pre, Stage::Pre), (GradeStage::Post, Stage::Post)] {
            if clip == group.stage_clip_id(stage) {
                return NodePath::new(Root::GroupStage { group: group.id.0, stage: at }, [node]);
            }
        }
    }
    klipp_compose::inspect::grade_node(clip, node).path
}

#[cfg(test)]
mod sample_path_tests {
    use super::{picked_node, sample_path};
    use klipp_graph_format::lifted::Stage;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_timeline::{GradeGroup, GradeStage};

    /// A pick through each stand-in names the sequence grade it edits,
    /// and its answer finds the node there (#1428); a clip's own pick is
    /// unchanged.
    #[test]
    fn a_stand_in_pick_names_the_grade_it_edits() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let graph = || {
            let mut g = klipp_nodes::NodeGraph::default();
            let id = g.ensure_corrector();
            (g, id)
        };
        let (timeline, t) = graph();
        seq.timeline_grade_slot().set(Some(timeline));
        let mut group = GradeGroup::new("Night");
        let (pre, p) = graph();
        let (post, q) = graph();
        group.stage_slot(GradeStage::Pre).set(Some(pre));
        group.stage_slot(GradeStage::Post).set(Some(post));
        let (gid, pre_id, post_id) = (group.id, group.stage_clip_id(GradeStage::Pre), group.stage_clip_id(GradeStage::Post));
        seq.grade_groups.push(group);

        let tid = seq.timeline_grade_clip_id();
        assert_eq!(sample_path(&seq, tid, t), NodePath::new(Root::TimelineGrade { sequence: seq.id.0 }, [t]));
        assert_eq!(sample_path(&seq, pre_id, p), NodePath::new(Root::GroupStage { group: gid.0, stage: Stage::Pre }, [p]));
        assert_eq!(sample_path(&seq, post_id, q), NodePath::new(Root::GroupStage { group: gid.0, stage: Stage::Post }, [q]));
        assert_eq!(picked_node(&seq, tid, t).map(|n| n.id), Some(t));
        assert_eq!(picked_node(&seq, pre_id, p).map(|n| n.id), Some(p));
        assert_eq!(picked_node(&seq, post_id, q).map(|n| n.id), Some(q));
        assert!(picked_node(&seq, pre_id, q).is_none(), "each stage holds its own nodes");

        let clip = klipp_core::ClipId::new();
        let n = klipp_nodes::NodeId::new_v4();
        assert_eq!(sample_path(&seq, clip, n), klipp_compose::inspect::grade_node(clip, n).path);
    }
}

/// The Colour toolbar's Track menu (#1408): each visible video track, whose
/// own grade, after each of its clips' grades, the workspace then grades.
/// Marked while a track's grade is the one graded; a track whose grade
/// does something is marked in the list.
pub(super) fn grade_track_menu(ui: &mut egui::Ui, state: &mut crate::state::AppState, theme: &KlippTheme) {
    let seq = &state.sequences[state.active_sequence_idx];
    let tracks: Vec<(klipp_core::TrackId, String, bool)> = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .map(|t| (t.id, t.name.clone(), t.effects.track_grade().is_some_and(|g| !g.is_identity())))
        .collect();
    let shown = state.grade_track.and_then(|id| tracks.iter().find(|t| t.0 == id)).map(|t| t.1.clone());
    let label = match &shown {
        Some(name) => egui::RichText::new(format!("Track \u{00b7} {name}")).color(theme.accent),
        None => egui::RichText::new("Track"),
    };
    let mut pick = None;
    ui.menu_rows(label, |ui| {
        for (id, name, graded) in &tracks {
            let text = if *graded { format!("{name} \u{25CF}") } else { name.clone() };
            if ui.selectable_label(state.grade_track == Some(*id), text).on_hover_text("Grade this track: after each of its clips' own grades").clicked() {
                pick = Some(*id);
                ui.close_menu();
            }
        }
    })
    .response
    .on_hover_text("A track's grade: runs on every picture of the track after its own grade");
    if let Some(id) = pick {
        state.pick_track_grade(id);
    }
}

#[cfg(test)]
mod grade_track_menu_tests {
    use super::*;

    /// The Colour toolbar's Track menu lists the video tracks and, clicked,
    /// grades the one picked, letting go of the timeline grade and a group
    /// stage (#1408); the track header's Grade in Colour also opens the
    /// Colour workspace.
    #[test]
    fn the_track_menu_picks_a_track_s_grade() {
        let mut state = crate::state::AppState { grade_timeline: true, ..Default::default() };
        let v1 = state.sequences[0].tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let (tid, name) = (v1.id, v1.name.clone());
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let mut clock = 0.0;
        let mut frame = |state: &mut crate::state::AppState, events: Vec<egui::Event>| {
            clock += 0.1;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), time: Some(clock), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| grade_track_menu(ui, state, &theme));
            });
            out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
                _ => None,
            }).collect::<Vec<_>>()
        };
        let click = |frame: &mut dyn FnMut(&mut crate::state::AppState, Vec<egui::Event>) -> Vec<(String, egui::Pos2)>, state: &mut crate::state::AppState, at: egui::Pos2| {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            frame(state, vec![egui::Event::PointerMoved(at), button(true)]);
            frame(state, vec![button(false)]);
            frame(state, Vec::new())
        };
        let drawn = frame(&mut state, Vec::new());
        let menu = drawn.iter().find(|(t, _)| t == "Track").expect("the Track menu").1;
        let drawn = click(&mut frame, &mut state, menu);
        let item = drawn.iter().find(|(t, _)| *t == name).unwrap_or_else(|| panic!("{name} listed: {drawn:?}")).1;
        let drawn = click(&mut frame, &mut state, item);
        assert_eq!(state.grade_track, Some(tid), "picked");
        assert!(!state.grade_timeline && state.grade_group_stage.is_none(), "the other targets let go");
        assert!(drawn.iter().any(|(t, _)| *t == format!("Track \u{00b7} {name}")), "the menu names it: {drawn:?}");

        let mut state = crate::state::AppState::default();
        state.grade_track_in_colour(tid);
        assert_eq!((state.active_workspace, state.grade_track), (crate::state::Workspace::Color, Some(tid)));
    }
}


/// The viewer's after-grade transform as one stable text (#1976): the
/// global LUT (its path, switch and opacity to the percent the inspector
/// shows, so float noise does not change it), the SDR viewer transfer
/// and the HDR monitor mode.
pub(crate) fn picture_transform_text(lut: Option<&std::path::Path>, enabled: bool, opacity: f32, sdr: klipp_core::SdrTransfer, hdr: klipp_core::HdrMonitor) -> String {
    let lut = match lut {
        Some(p) => format!("lut {} {} {:.0}%", p.display(), if enabled { "on" } else { "off" }, (opacity.clamp(0.0, 1.0) * 100.0).round()),
        None => "no lut".to_string(),
    };
    format!("{lut}|sdr {sdr:?}|hdr {hdr:?}")
}

#[cfg(test)]
mod picture_transform_tests {
    use super::picture_transform_text as text;

    /// #1976: each setting the viewer's frame goes through changes the
    /// text; a wobble under the inspector's percent does not.
    #[test]
    fn each_setting_after_the_grade_changes_the_text() {
        let p = std::path::Path::new("/luts/show.cube");
        let base = text(Some(p), true, 1.0, Default::default(), klipp_core::HdrMonitor::Off);
        assert_ne!(base, text(Some(p), false, 1.0, Default::default(), klipp_core::HdrMonitor::Off), "switched off");
        assert_ne!(base, text(Some(p), true, 0.5, Default::default(), klipp_core::HdrMonitor::Off), "opacity");
        assert_ne!(base, text(None, true, 1.0, Default::default(), klipp_core::HdrMonitor::Off), "no LUT");
        assert_ne!(base, text(Some(p), true, 1.0, Default::default(), klipp_core::HdrMonitor::Preview), "HDR monitor");
        assert_eq!(base, text(Some(p), true, 0.999_9, Default::default(), klipp_core::HdrMonitor::Off), "float noise");
    }
}

#[cfg(test)]
mod zones_placed_for_their_source {
    use super::*;

    /// #2767: the colour page places a zones node's zones where the op
    /// does: its first edit seeds the boundaries the `zones` op gives the
    /// same clip, SDR or HDR; a node with zones of its own, or another
    /// node's parameter, seeds nothing.
    #[test]
    fn the_ui_seeds_the_boundaries_the_op_gives() {
        for hdr in [false, true] {
            let mut state = AppState::default();
            let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/zones.mov".into(), None);
            item.duration = klipp_core::Timecode::from_secs_f64(4.0, 25);
            item.video_codec = "h264".into();
            if hdr {
                item.colour_space = klipp_media::ColourSpace::Hdr10;
            }
            let media = state.media_pool.add(item);
            let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
            let mut clip = klipp_timeline::Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
            let mut graph = klipp_nodes::defaults::default_graph();
            let zones = klipp_nodes::NodeId::new_v4();
            graph.splice_before_output(klipp_nodes::Node { id: zones, kind: klipp_nodes::NodeKind::HdrZones, label: "HDR Zones".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] });
            klipp_timeline::GradeAccess::set_grade(&mut clip, graph);
            let clip_id = clip.id;
            let seq = &mut state.sequences[state.active_sequence_idx];
            let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
            seq.tracks[v].clips.push(clip);

            let seeded: Vec<f32> = zones_seed(&state, clip_id, zones, "hdr_highlight_exposure").into_iter().map(|(_, v)| v).collect();
            let op = klipp_document::ops::Op::Zones { clip_id: Some(klipp_document::ops::ClipRef::Id(clip_id)), on: None, zone: Some("highlight".into()), exposure: Some(-0.7), hue: None, amount: None, saturation: None, boundary: None, falloff: None, preset: None, reset: false };
            let plan = klipp_document::ops::resolve(state.sequence(), &state.media_pool, &[op]).unwrap();
            let klipp_document::history::EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!() };
            let by_op = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::HdrZones).and_then(|n| n.params.hdr_zones.clone()).unwrap().boundary;
            assert_eq!(seeded, by_op[1..].to_vec(), "hdr {hdr}: the colour page and the op place the zones alike");
            assert_eq!(by_op, klipp_core::HdrZonesSettings::boundary_for(hdr));
            assert!(zones_seed(&state, clip_id, zones, "exposure").is_empty(), "not a zone's parameter");
        }
    }

    #[test]
    fn the_panel_says_where_a_zone_starts() {
        assert_eq!(crate::panels::node_inspector::zone_start_label(5, 1.5), "Highlight starts at 51% of white");
        assert_eq!(crate::panels::node_inspector::zone_start_label(3, -1.0), "Shadow reaches up to 9% of white");
        assert_eq!(crate::panels::node_inspector::zone_start_label(6, 4.5), "Specular starts at 4.1\u{00d7} white");
    }
}
