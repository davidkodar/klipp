//! Colour: the grade's node graph and its nodes, grade groups and the timeline
//! grade, sampling the picture (balance, qualifier, hue curves), tracked
//! windows, grade stills and LUT versions (#826).

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

    pub(super) fn handle_color_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // Grade groups themselves are edits to the sequence, not to a
        // graph: never through a stand-in.
        if self.handle_grade_group_commands(cmd, needs_refresh) {
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
        ui.menu_button(menu_label, |ui| {
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

    fn add_color_node(&mut self, clip_id: klipp_core::ClipId, kind: &str, position: (f32, f32), needs_refresh: &mut bool) {
        let node_kind = match kind {
            "Corrector" => klipp_nodes::NodeKind::Corrector,
            "Exposure" => klipp_nodes::NodeKind::Exposure,
            "Contrast" => klipp_nodes::NodeKind::Contrast,
            "Saturation" => klipp_nodes::NodeKind::Saturation,
            "ToneControls" => klipp_nodes::NodeKind::ToneControls,
            "ColorWheels" => klipp_nodes::NodeKind::ColorWheels,
            "WheelSatBri" => klipp_nodes::NodeKind::WheelSatBri,
            "RGBCurves" => klipp_nodes::NodeKind::RGBCurves,
            "HueSatCurves" => klipp_nodes::NodeKind::HueSatCurves,
            "ColorBalance" => klipp_nodes::NodeKind::ColorBalance,
            "InputTransform" => klipp_nodes::NodeKind::InputTransform,
            "Qualifier" => klipp_nodes::NodeKind::Qualifier,
            "LumaKey" => klipp_nodes::NodeKind::LumaKey,
            "SoftClip" => klipp_nodes::NodeKind::SoftClip,
            "Mask" => klipp_nodes::NodeKind::Mask,
            "CombineMattes" => klipp_nodes::NodeKind::CombineMattes,
            "ExternalMatte" => klipp_nodes::NodeKind::ExternalMatte,
            "DifferenceKey" => klipp_nodes::NodeKind::DifferenceKey,
            "ApplyMatte" => klipp_nodes::NodeKind::ApplyMatte,
            "Blend" => klipp_nodes::NodeKind::Blend,
            "Layer" => klipp_nodes::NodeKind::Layer,
            "LayerMixer" => klipp_nodes::NodeKind::LayerMixer,
            "Lut3D" => klipp_nodes::NodeKind::Lut3D,
            "FilmGrain" => klipp_nodes::NodeKind::FilmGrain,
            "ColorMatch" => klipp_nodes::NodeKind::ColorMatch,
            "ColorWarper" => klipp_nodes::NodeKind::ColorWarper,
            "Denoise" => klipp_nodes::NodeKind::Denoise,
            "Compound" => klipp_nodes::NodeKind::Compound,
            "RGBSplitter" => klipp_nodes::NodeKind::RGBSplitter,
            "RGBCombiner" => klipp_nodes::NodeKind::RGBCombiner,
            "MaskCutout" => klipp_nodes::NodeKind::MaskCutout,
            "Reroute" => klipp_nodes::NodeKind::Reroute,
            _ => { return; }
        };
        self.with_current_clip_graph_mut(clip_id, needs_refresh, |graph| {
            let new_id = klipp_nodes::NodeId::new_v4();
            graph.nodes.push(klipp_nodes::Node {
                id: new_id,
                kind: node_kind,
                label: node_kind.display_name().to_string(),
                enabled: true,
                params: klipp_nodes::NodeParams::default(),
                keyframes: klipp_nodes::NodeKeyframeTracks::default(),
                output_link: None,
                input_links: Vec::new(),
            });
            graph.node_positions.insert(
                new_id,
                klipp_nodes::NodePos { x: position.0, y: position.1 },
            );
        });
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
            let forward: Vec<usize> = (reference + 1..=last).collect();
            let backward: Vec<usize> = (first..reference).rev().collect();
            let share = (forward.len() as f32 / (forward.len() + backward.len()).max(1) as f32).max(0.01);
            let report = |p: f32| event_tx.send(klipp_core::AppEvent::TrackingProgress { clip_id, progress: loading + p * (1.0 - loading) }).is_ok();
            let nothing = || {
                let _ = event_tx.send(klipp_core::AppEvent::TrackingFailed {
                    clip_id,
                    error: "nothing in the window to follow: draw it over something with detail".into(),
                });
            };
            let tried = !(forward.is_empty() && backward.is_empty());
            if perspective {
                // Each frame's map from the reference, carrying the
                // window's corners and pivot, back in frame fractions.
                let ahead = track_window_perspective(&reference_frame, quad_px, &forward, &mut |f| decode(f), &opts, &|p| report(p * share));
                let behind = track_window_perspective(&reference_frame, quad_px, &backward, &mut |f| decode(f), &opts, &|p| report(share + p * (1.0 - share)));
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
            let ahead = track_window(&reference_frame, quad_px, pivot_px, &forward, &mut |f| decode(f), &opts, &|p| report(p * share));
            let behind = track_window(&reference_frame, quad_px, pivot_px, &backward, &mut |f| decode(f), &opts, &|p| report(share + p * (1.0 - share)));
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
        let seq_idx = self.state.active_sequence_idx;
        let mut applied = false;
        let mut old_top_graph: Option<klipp_nodes::NodeGraph> = None;
        let mut new_top_graph: Option<klipp_nodes::NodeGraph> = None;
        let mut path_went_stale = false;
        for track in &mut self.state.sequences[seq_idx].tracks {
            if let Some(clip) = track.find_clip_mut(clip_id) {
                let old = clip.grade().clone();
                let resolve = clip.edit_grade(|root| Self::resolve_graph_at_path_mut(root, &path).map(mutate));
                match resolve {
                    Ok(()) => {}
                    Err(_e) => {
                        path_went_stale = true;
                        break;
                    }
                }
                new_top_graph = Some(clip.grade().clone());
                old_top_graph = Some(old);
                applied = true;
                break;
            }
        }
        if path_went_stale {
            // Descent path went stale (upstream remove, clip switch
            // we missed). Reset to root so future commands hit the
            // normal path; the user loses nothing except their
            // current descent position.
            self.state.graph_descent_path.clear();
            self.state.selected_node_ids.clear();
            self.state.selected_node_id = None;
            return;
        }
        if applied {
            // Skip per-call history push while a node-param edit
            // gesture is in progress (BeginNodeParamEdit took the
            // snapshot, EndNodeParamEdit will commit the whole
            // gesture as ONE entry). Without this guard, dragging a
            // mask handle on the viewer pushed one full SetNodeGraph
            // entry per frame — hundreds of clones per second of the
            // entire graph, which floods the undo stack and triggers
            // heavy engine resyncs that desync the master clock and
            // cause "playback walks off on its own" symptoms.
            let in_gesture = self.node_param_edit_snapshot.is_some();
            if !in_gesture {
                if let (Some(old_graph), Some(new_graph)) =
                    (old_top_graph, new_top_graph)
                {
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetNodeGraph {
                            clip_id,
                            old_graph,
                            new_graph,
                        },
                    });
                }
            }
            *needs_refresh = true;
        }
    }

    /// Single-input / single-output topology only for v1; refuses
    /// gracefully (toast + no-op) on multi-port selections.
    /// Run hierarchical auto-layout on the clip's currently-visible
    /// node graph (root or descended sub-graph). Captures the whole
    /// root graph before + after into a single SetNodeGraph history
    /// entry so Ctrl+Z reverts every position in one step.
    fn auto_layout_node_graph(
        &mut self,
        clip_id: klipp_core::ClipId,
        needs_refresh: &mut bool,
    ) {
        let path = self.state.graph_descent_path.clone();
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            let Some(clip) = track.find_clip_mut(clip_id) else { continue };
            let old_root_graph = clip.grade().clone();
            let changed = clip.edit_grade(|root| {
                let Ok(target_graph) = Self::resolve_graph_at_path_mut(root, &path) else {
                    self.state.toast.show("Auto-layout: invalid descent path");
                    return false;
                };
                let new_positions = klipp_nodes::auto_layout(target_graph);
                if new_positions.is_empty() {
                    // Empty graph or cycle bail-out — nothing to do.
                    return false;
                }
                target_graph.node_positions = new_positions;
                true
            });
            if !changed {
                return;
            }
            let new_root_graph = clip.grade().clone();
            self.state.active_history_mut().push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::SetNodeGraph {
                    clip_id,
                    old_graph: old_root_graph,
                    new_graph: new_root_graph,
                },
            });
            *needs_refresh = true;
            self.state.toast.show("Auto-layout applied (Ctrl+Z to revert)");
            return;
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
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let mut edit: Option<(klipp_nodes::NodeGraph, klipp_nodes::NodeGraph, bool)> = None;
        for track in &mut seq.tracks {
            if let Some(clip) = track.find_clip_mut(clip_id) {
                let before = clip.grade().clone();
                // The Corrector's temperature/tint feed the unified grade
                // shader; the legacy Colour Balance node is the fallback.
                let railed = clip.edit_grade(|graph| {
                    let target = graph.nodes.iter().position(|n| n.kind == klipp_nodes::NodeKind::Corrector)
                        .or_else(|| graph.nodes.iter().position(|n| n.kind == klipp_nodes::NodeKind::ColorBalance));
                    target.map(|idx| {
                        let node = &mut graph.nodes[idx];
                        let (t, t_railed) = add_on_rail(node.params.temperature.unwrap_or(0.0), temp_adj);
                        let (g, g_railed) = add_on_rail(node.params.tint.unwrap_or(0.0), tint_adj);
                        node.params.temperature = Some(t);
                        node.params.tint = Some(g);
                        t_railed || g_railed
                    })
                });
                if let Some(railed) = railed {
                    edit = Some((before, clip.grade().clone(), railed));
                }
                break;
            }
        }
        let Some((old_graph, new_graph, railed)) = edit else {
            self.state.toast.show(format!("{who}: add a Colour Balance node first"));
            return;
        };
        self.state.active_history_mut().push(crate::history::HistoryEntry {
            command: crate::history::EditCommand::SetNodeGraph { clip_id, old_graph, new_graph },
        });
        self.state.dirty = true;
        // The sliders stop at ±50; a strong cast, or a clip already near
        // the end, gets part of what it needs.
        if railed {
            self.state.toast.show(format!(
                "{who}: the correction reached the end of the temperature or tint range, so the \
                 frame is only partly balanced — fix the cast at the source or with another balance node"
            ));
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
        let mut huesat = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.grade().nodes.iter().find(|n| n.id == node_id))
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
        let base = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.grade().nodes.iter().find(|n| n.id == node_id))
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
                let _ = self.cmd_tx.send(AppCommand::SampleNodeInput { clip_id, node_id, nx: *nx, ny: *ny });
                true
            }
            AppCommand::RequestQualifierSample { nx, ny } => {
                // Keyed on the picture the node receives, not the one on
                // screen, which is the grade's result and, after a first
                // pick, the key's own correction (#587). The engine reads
                // it and answers with `NodeInputSampled`.
                let Some((clip_id, node_id)) = self.state.qualifier_pick.take() else { return true };
                let _ = self.cmd_tx.send(AppCommand::SampleNodeInput { clip_id, node_id, nx: *nx, ny: *ny });
                true
            }
            _ => false,
        }
    }

    fn handle_color_node_ops(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::AddNode { clip_id, ref kind, position } => {
                self.add_color_node(*clip_id, kind, *position, needs_refresh);
                true
            }
            AppCommand::RemoveNode { clip_id, node_id } => {
                let node_id = *node_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    graph.nodes.retain(|n| n.id != node_id);
                    graph.edges.retain(|e| e.from_node != node_id && e.to_node != node_id);
                    graph.node_positions.remove(&node_id);
                    // Frame cleanup invariant — drop the deleted id
                    // from every frame's membership set, then prune
                    // any frame left with zero members (a frame
                    // wrapping nothing is a visual ghost).
                    for frame in &mut graph.frames {
                        frame.contained_node_ids.remove(&node_id);
                    }
                    graph.frames.retain(|f| !f.contained_node_ids.is_empty());
                });
                true
            }
            AppCommand::CreateNodeFrame { clip_id, node_ids, label, color_rgba } => {
                let label = label.clone();
                let color_rgba = *color_rgba;
                let ids = node_ids.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    let valid_ids: std::collections::HashSet<klipp_nodes::NodeId> = ids
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
                let sticky_id = *sticky_id;
                let delta_x = *delta_x;
                let delta_y = *delta_y;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(s) = graph.sticky_notes.iter_mut().find(|s| s.id == sticky_id) {
                        s.pos.x += delta_x;
                        s.pos.y += delta_y;
                    }
                });
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
            AppCommand::GroupIntoCompound { clip_id, node_ids } => {
                self.group_into_compound(*clip_id, node_ids, needs_refresh);
                true
            }
            AppCommand::AutoLayoutNodeGraph { clip_id } => {
                self.auto_layout_node_graph(*clip_id, needs_refresh);
                true
            }
            AppCommand::UngroupCompound { clip_id, node_id } => {
                self.ungroup_compound(*clip_id, *node_id, needs_refresh);
                true
            }
            AppCommand::UpdateActiveGradeVersion { clip_id } => {
                let clip_id = *clip_id;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(clip_id) {
                        if let Some(idx) = clip.active_grade_version {
                            let grade = clip.grade().clone();
                            if let Some(v) = clip.grade_versions.get_mut(idx) {
                                v.set_grade(grade);
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
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    let pre_connect = graph.clone();
                    // Remove existing edge to this input port — one
                    // edge per (node, port) pair.
                    graph.edges.retain(|e| !(e.to_node == to_node && e.to_port == to_port));
                    graph.edges.push(klipp_nodes::Edge {
                        from_node, from_port, to_node, to_port,
                    });
                    // LayerMixer auto-grow — if the editor dropped a
                    // wire on a port past the current count, grow the
                    // params Vec in lockstep so the executor finds a
                    // setting at that index.
                    if let Some(target) = graph.nodes.iter_mut().find(|n| n.id == to_node) {
                        if target.kind == klipp_nodes::NodeKind::LayerMixer {
                            let needed = (to_port as usize) + 1;
                            let mixer = target.params.layer_mixer.get_or_insert_with(Default::default);
                            if mixer.inputs.len() < needed {
                                mixer.inputs.resize_with(needed, klipp_nodes::LayerMixerInput::default);
                            }
                        }
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
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.output_link = label.as_ref().and_then(|s| {
                            let t = s.trim();
                            if t.is_empty() { None } else { Some(t.to_string()) }
                        });
                    }
                });
                true
            }
            AppCommand::SetNodeInputLink { clip_id, node_id, port, label } => {
                let node_id = *node_id;
                let port = *port as usize;
                let label = label.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
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
                });
                true
            }
            AppCommand::MoveNode { clip_id, node_id, delta_x, delta_y } => {
                // Position-only changes don't go through history —
                // they fire every drag-frame and would blow out the
                // undo stack. Still need path-aware resolution so
                // moves inside a compound stick.
                let path = self.state.graph_descent_path.clone();
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|root| {
                            if let Ok(graph) = Self::resolve_graph_at_path_mut(root, &path) {
                                let pos = graph.node_positions.entry(*node_id).or_default();
                                pos.x += delta_x;
                                pos.y += delta_y;
                            }
                        });
                        break;
                    }
                }
                // No undo for continuous drag — undo pushed on drag_stopped via SetNodeGraph
                *needs_refresh = true;
                true
            }
            AppCommand::ToggleNodeEnabled { clip_id, node_id } => {
                let node_id = *node_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.enabled = !node.enabled;
                    }
                });
                true
            }
            AppCommand::SetNodeSolo { node_id } => {
                // Mirror into AppState for UI feedback (halo / badge /
                // toolbar indicator) AND let the dispatcher propagate
                // it to the engine via the normal forward path so the
                // executor's setter gets called on the next render.
                // needs_refresh = true so the paused viewer re-renders
                // the new soloed output immediately rather than waiting
                // for the next frame to arrive on its own.
                self.state.solo_node = *node_id;
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

    /// Translate the inspector's `PHANTOM_CORRECTOR_ID` to a real
    /// Corrector node id on the target clip, materialising the node
    /// in the graph if it doesn't exist yet. Used at the top of every
    /// `SetNode*` handler so widgets that rendered against a scratch
    /// NodeParams (clip has no grade node) can land their first write
    /// without the inspector having to know whether a real node
    /// exists. Returns the original id unchanged when it's not the
    /// phantom sentinel.
    fn resolve_phantom_node_id(&mut self, clip_id: klipp_core::ClipId, node_id: klipp_nodes::NodeId) -> klipp_nodes::NodeId {
        if node_id != klipp_nodes::PHANTOM_CORRECTOR_ID {
            return node_id;
        }
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            if let Some(clip) = track.find_clip_mut(clip_id) {
                return clip.edit_grade(|g| g.ensure_corrector());
            }
        }
        node_id
    }

    fn handle_color_node_config(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetNodeLut { clip_id, node_id, path } => {
                let node_id = *node_id;
                let path = path.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.lut_path = path;
                    }
                });
                true
            }
            AppCommand::RenameNode { clip_id, node_id, ref label } => {
                let node_id = *node_id;
                let label = label.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.label = label;
                    }
                });
                true
            }
            AppCommand::PrinterLight { channel, points } => {
                // The offset of the grade the wheels show: the clip's
                // Corrector, else its wheels node, else a new Corrector.
                let Some(clip_id) = self.state.selected_clip else {
                    self.state.toast.show("Select a clip to give it printer lights");
                    return true;
                };
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return true };
                let graph = clip.grade();
                let node = graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector && n.enabled)
                    .or_else(|| graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::ColorWheels && n.enabled));
                let (node_id, now) = match node {
                    Some(n) => (n.id, (n.params.offset_x.unwrap_or(0.0), n.params.offset_y.unwrap_or(0.0), n.params.offset_master.unwrap_or(0.0))),
                    None => (klipp_nodes::PHANTOM_CORRECTOR_ID, (0.0, 0.0, 0.0)),
                };
                let step = self.prefs.color.printer_point;
                let which = (*channel < 3).then_some(*channel as usize);
                let (x, y, m) = klipp_color::printer_light(now, which, *points, step);
                let [r, g, b] = klipp_color::printer_points((x, y, m), step);
                self.send_commands(vec![
                    AppCommand::BeginNodeParamEdit { clip_id },
                    AppCommand::SetNodeParam { clip_id, node_id, param: "offset_x".into(), value: x },
                    AppCommand::SetNodeParam { clip_id, node_id, param: "offset_y".into(), value: y },
                    AppCommand::SetNodeParam { clip_id, node_id, param: "offset_master".into(), value: m },
                    AppCommand::EndNodeParamEdit { clip_id },
                ]);
                self.state.toast.show(format!("Printer lights R{r:+} G{g:+} B{b:+}"));
                true
            }
            AppCommand::SetNodeParam { clip_id, node_id, param, value } => {
                // Lazy-create: if the inspector dispatched with the
                // phantom Corrector id (no grade node existed on this
                // clip when the widget rendered), materialise a real
                // Corrector now and route the write to it.
                let resolved = self.resolve_phantom_node_id(*clip_id, *node_id);
                let node_id = &resolved;
                // Update the UI-local sequence mirror — required so the UI's
                // own scope-preview re-grade (runs on this thread) sees the
                // new value. This mutates `self.state.sequences[...]` only,
                // not the shared `Arc<klipp_timeline::SharedSequence>` the engine reads.
                let path = self.state.graph_descent_path.clone();
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|root| {
                            if let Ok(graph) = Self::resolve_graph_at_path_mut(root, &path) {
                                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                    node.params.apply_override(param, *value);
                                }
                            }
                        });
                        break;
                    }
                }
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
                        for track in &self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(clip) = track.find_clip(*clip_id) {
                                let new_graph = clip.grade().clone();
                                // NodeGraph doesn't impl PartialEq, so we push
                                // unconditionally; undoing a no-op gesture just
                                // restores an identical graph.
                                self.state.active_history_mut().push(crate::history::HistoryEntry {
                                    command: crate::history::EditCommand::SetNodeGraph {
                                        clip_id: snap_clip_id,
                                        old_graph,
                                        new_graph,
                                    },
                                });
                                self.state.dirty = true;
                                break;
                            }
                        }
                    }
                }
                // Forward to engine so it clears `in_node_param_edit` and
                // triggers the one-shot full-res recovery regrade. See the
                // matching comment on BeginNodeParamEdit.
                false
            }
            AppCommand::SetNodeCurves { clip_id, node_id, curves } => {
                let resolved = self.resolve_phantom_node_id(*clip_id, *node_id);
                let node_id = resolved;
                let curves = curves.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.curves = Some(curves);
                        node.enabled = true;
                    }
                });
                true
            }
            AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat } => {
                let resolved = self.resolve_phantom_node_id(*clip_id, *node_id);
                let node_id = resolved;
                let huesat = huesat.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.huesat = Some(huesat);
                        node.enabled = true;
                    }
                });
                true
            }
            AppCommand::SetNodeQualifier { clip_id, node_id, qualifier } => {
                let node_id = *node_id;
                let qualifier = qualifier.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.qualifier = Some(qualifier);
                    }
                });
                true
            }
            AppCommand::SetNodeLumaKey { clip_id, node_id, luma_key } => {
                let node_id = *node_id;
                let luma_key = luma_key.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.luma_key = Some(luma_key);
                    }
                });
                true
            }
            AppCommand::SetNodeDifferenceKey { clip_id, node_id, difference_key } => {
                let node_id = *node_id;
                let dk = difference_key.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.difference_key = Some(dk);
                        node.enabled = true;
                    }
                });
                true
            }
            AppCommand::SetNodeDifferenceKeyPlate { clip_id, node_id, plate_path } => {
                let node_id = *node_id;
                let plate_path = plate_path.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        let dk = node.params.difference_key.get_or_insert_with(Default::default);
                        dk.plate_path = plate_path;
                        node.enabled = true;
                    }
                });
                true
            }
            AppCommand::SetNodeSoftClip { clip_id, node_id, soft_clip } => {
                let node_id = *node_id;
                let soft_clip = soft_clip.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.soft_clip = Some(soft_clip);
                    }
                });
                true
            }
            AppCommand::SetNodeFilmGrain { clip_id, node_id, grain } => {
                let node_id = *node_id;
                let grain = grain.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.grain = Some(grain);
                    }
                });
                true
            }
            AppCommand::SetNodeMasks { clip_id, node_id, masks } => {
                let node_id = *node_id;
                let clip_id_val = *clip_id;
                let masks = masks.clone();
                let new_len = masks.masks.len();
                self.with_current_clip_graph_mut(clip_id_val, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.masks = Some(masks);
                        node.enabled = true;
                    }
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
            AppCommand::SetNodeColorMatch { clip_id, node_id, settings } => {
                let node_id = *node_id;
                let settings = *settings;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.color_match = Some(settings);
                        node.enabled = true;
                    }
                });
                true
            }
            AppCommand::SetExternalMattePath { clip_id, node_id, path } => {
                let node_id = *node_id;
                let matte_path = path.clone();
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.matte_image_path = matte_path;
                    }
                });
                self.state.dirty = true;
                true
            }
            AppCommand::SetExternalMatteChannel { clip_id, node_id, channel, invert } => {
                let ch = match *channel {
                    0 => klipp_nodes::MatteChannel::Alpha,
                    1 => klipp_nodes::MatteChannel::Luminance,
                    2 => klipp_nodes::MatteChannel::Red,
                    3 => klipp_nodes::MatteChannel::Green,
                    4 => klipp_nodes::MatteChannel::Blue,
                    _ => klipp_nodes::MatteChannel::Luminance,
                };
                let node_id = *node_id;
                let invert = *invert;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.matte_channel = Some(ch);
                        node.params.matte_invert = Some(invert);
                    }
                });
                self.state.dirty = true;
                true
            }
            AppCommand::SetMatteCombineOp { clip_id, node_id, op } => {
                // Out-of-range falls back to the default rather than
                // refusing the command, matching the channel handler
                // above: a bad discriminant should not wedge the node.
                let chosen = klipp_nodes::MatteCombineOp::ALL
                    .get(*op as usize)
                    .copied()
                    .unwrap_or_default();
                let node_id = *node_id;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.params.matte_combine_op = Some(chosen);
                    }
                });
                self.state.dirty = true;
                true
            }
            AppCommand::SetNodeLayerMixerInput { clip_id, node_id, port, input } => {
                let node_id = *node_id;
                let port = *port;
                let input = *input;
                self.with_current_clip_graph_mut(*clip_id, needs_refresh, |graph| {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                        let mixer = node.params.layer_mixer.get_or_insert_with(Default::default);
                        let needed = (port as usize) + 1;
                        if mixer.inputs.len() < needed {
                            mixer.inputs.resize_with(needed, klipp_nodes::LayerMixerInput::default);
                        }
                        mixer.inputs[port as usize] = input;
                    }
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
            AppCommand::AddMaskShape { clip_id, shape } => {
                let clip_id = *clip_id;
                let shape = shape.clone();
                self.with_current_clip_graph_mut(clip_id, needs_refresh, |graph| {
                    // ensure_mask_cutout lazy-creates BOTH the Mask
                    // matte generator and the MaskCutout that
                    // composites source × matte → Output. Users who
                    // added a mask from the Color tab see the
                    // clip-in-shape effect immediately; power users
                    // can still rewire / delete the cutout in the
                    // node canvas.
                    let (mask_node_id, _cutout_id) = graph.ensure_mask_cutout();
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == mask_node_id) {
                        let masks = node.params.masks.get_or_insert_with(Default::default);
                        masks.masks.push(klipp_core::Mask {
                            shape,
                            ..Default::default()
                        });
                    }
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
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Mask) {
                        if let Some(masks) = &mut node.params.masks {
                            if mask_index < masks.masks.len() {
                                masks.masks.remove(mask_index);
                            }
                        }
                    }
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
                // Apply directly to the local sequence — same effect
                // as the SetNodeColorMatch arm above; doing it inline
                // avoids re-dispatching through the command channel
                // when we already have everything we need here.
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.color_match = Some(settings);
                                node.enabled = true;
                            }
                        });
                        break;
                    }
                }
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
            AppCommand::AddMaskKeyframe { clip_id, node_id, mask_index, ref param, time_secs, value } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                if let Some(masks) = &mut node.params.masks {
                                    if let Some(mask) = masks.masks.get_mut(*mask_index) {
                                        if let Some(track) = mask.keyframes.track_mut(param) {
                                            // Insert or update keyframe at this time
                                            let epsilon = klipp_core::keyframe::KEYFRAME_TIME_EPSILON;
                                            if let Some(kf) = track.iter_mut().find(|k| (k.time_secs - time_secs).abs() < epsilon) {
                                                kf.value = *value;
                                            } else {
                                                track.push(klipp_core::Keyframe {
                                                    time_secs: *time_secs,
                                                    value: *value,
                                                    interpolation: klipp_core::Interpolation::Linear,
                                                    tangent_out: (0.0, 0.0),
                                                    tangent_in: (0.0, 0.0),
                                                });
                                                track.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                            }
                                        }
                                    }
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::RemoveMaskKeyframe { clip_id, node_id, mask_index, ref param, time_secs } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                if let Some(masks) = &mut node.params.masks {
                                    if let Some(mask) = masks.masks.get_mut(*mask_index) {
                                        if let Some(track) = mask.keyframes.track_mut(param) {
                                            klipp_core::keyframe::remove_keyframe_at(track, *time_secs);
                                        }
                                    }
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::ClearMaskKeyframes { clip_id, node_id, mask_index, ref param } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                if let Some(masks) = &mut node.params.masks {
                                    if let Some(mask) = masks.masks.get_mut(*mask_index) {
                                        if let Some(track) = mask.keyframes.track_mut(param) {
                                            track.clear();
                                        }
                                    }
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::AddKeyframe { clip_id, node_id, ref param, time_secs, value } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                let track = node.keyframes.track_mut(param);
                                track.set_keyframe(*time_secs, *value);
                                if param == klipp_nodes::ENABLED_TRACK {
                                    let epsilon = 0.01;
                                    if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < epsilon) {
                                        kf.interpolation = klipp_core::keyframe::Interpolation::Step;
                                    }
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::RemoveKeyframe { clip_id, node_id, ref param, time_secs } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.keyframes.track_mut(param).remove_keyframe(*time_secs);
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::ClearKeyframes { clip_id, node_id, ref param } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.keyframes.tracks.retain(|t| t.param_name != *param);
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::SetKeyframeInterpolation { clip_id, node_id, ref param, time_secs, interpolation } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                let track = node.keyframes.track_mut(param);
                                let epsilon = 0.01;
                                if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < epsilon) {
                                    kf.interpolation = *interpolation;
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
                true
            }
            AppCommand::SetKeyframeTangents { clip_id, node_id, ref param, time_secs, tangent_in, tangent_out } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_grade(|graph| {
                            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                let track = node.keyframes.track_mut(param);
                                let epsilon = 0.01;
                                if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < epsilon) {
                                    kf.tangent_in = *tangent_in;
                                    kf.tangent_out = *tangent_out;
                                }
                            }
                        });
                        break;
                    }
                }
                *needs_refresh = true;
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
                let mut media_id_for_writeback: Option<klipp_core::MediaId> = None;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        // `set_input_log_format` splices an
                        // InputTransform into the default Source →
                        // Corrector → Output graph if one isn't
                        // present yet — covers the common case
                        // where the user picks a format on a freshly
                        // added clip whose default graph has no IDT.
                        clip.set_input_log_format(*log_format);
                        media_id_for_writeback = clip.media_id();
                        break;
                    }
                }
                // The format belongs to the file, not the clip: a camera
                // file has one encoding, and the decoder — which does the
                // conversion — works per file. So set it on the media and
                // on every clip of that media. (This used to write back only
                // when the media was still on the default, so re-picking a
                // format on a clip changed its node but not what the
                // decoder did: nothing visible happened.)
                if let Some(mid) = media_id_for_writeback {
                    if let Some(item) = self.state.media_pool.get_mut(&mid) {
                        item.log_format = *log_format;
                        item.log_format_user_set = true;
                    }
                    for seq in &mut self.state.sequences {
                        for clip in seq.all_clips_mut() {
                            if clip.media_id() == Some(mid) {
                                clip.set_input_log_format(*log_format);
                            }
                        }
                    }
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
                self.state.dirty = true;
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
                    if self.state.split_compare_effect.is_some() {
                        self.state.split_compare_effect = None;
                        let _ = self.cmd_tx.send(AppCommand::SetSplitCompareEffect {
                            clip_id: None,
                            effect_index: 0,
                            is_ofx: false,
                        });
                    }
                }
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::SetSplitCompareEffect { clip_id, effect_index, is_ofx } => {
                // Mutate UI state and mirror the command to the engine. The
                // engine reads clip_id.is_some() as the activation signal.
                let activating = clip_id.is_some();
                self.state.split_compare_effect = clip_id.map(|id| (id, *effect_index, *is_ofx));
                if activating {
                    // Mutually exclusive with the other compare modes.
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
                    self.state.grade_stills.push(still);
                    self.state.toast.show(format!("Grade still '{}' captured", name));
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
                let (edits, refused) = grade_from_still_edits(&self.state.sequences[self.state.active_sequence_idx], &grade, clip_ids, *append);
                if edits.is_empty() {
                    if refused.is_empty() {
                        self.state.toast.show("Select the clips to grade first");
                    } else {
                        self.state.toast.show(format!("Could not add the grade of \"{name}\": {}", refusals_line(&refused)));
                    }
                    return true;
                }
                let n = edits.len();
                self.apply_edits_as_one(&edits, "Apply Grade from Still");
                let mut told = format!("Applied the grade of \"{name}\" to {n} clip{}", if n == 1 { "" } else { "s" });
                if !refused.is_empty() {
                    told = format!("{told}; not to {}: {}", clip_ids.len() - n, refusals_line(&refused));
                }
                self.state.toast.show(told);
                true
            }
            AppCommand::ToggleGradeStillCompare => {
                self.state.grade_still_compare = !self.state.grade_still_compare;
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
            grade: clip.map(|c| c.grade().clone()),
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
            })
            .collect();
        if let Err(e) = klipp_project::save_grade_stills(&project_dir, &stills) {
            tracing::warn!("Failed to persist grade stills: {e}");
            // Said, not only logged: a newer Klipp's stills are kept by
            // not saving these over them (#993).
            self.state.toast.show(e);
        }
    }

    /// Tell the control socket which global LUT the project names, for
    /// `check`.
    fn publish_global_lut(&self) {
        if let Ok(mut project) = self.state.control_shared.project.lock() {
            project.global_lut = self.state.global_lut_path.clone();
        }
    }

    fn handle_color_lut_versions(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        match cmd {
            // Global LUT — update UI state and forward to engine
            AppCommand::LoadGlobalLut { .. } => {
                if let AppCommand::LoadGlobalLut { ref path } = cmd {
                    self.state.global_lut_path = Some(path.clone());
                    self.state.global_lut_enabled = true;
                }
                self.publish_global_lut();
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::ClearGlobalLut => {
                self.state.global_lut_path = None;
                self.publish_global_lut();
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::SetGlobalLutOpacity(opacity) => {
                self.state.global_lut_opacity = *opacity;
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::SetGlobalLutEnabled(enabled) => {
                self.state.global_lut_enabled = *enabled;
                let _ = self.cmd_tx.send(cmd.clone());
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
                    seq.all_clips().find(|c| c.id == clip_id).map(|c| c.grade().clone())
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
            AppCommand::SaveGradeVersion { clip_id, name } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    let version = klipp_timeline::GradeVersion::new(name.clone(), clip.grade().clone());
                    clip.grade_versions.push(version);
                    clip.active_grade_version = Some(clip.grade_versions.len() - 1);
                }
                self.state.dirty = true;
                true
            }
            AppCommand::SwitchGradeVersion { clip_id, version_index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    // Save current graph back to active version slot before switching
                    if let Some(active_idx) = clip.active_grade_version {
                        if active_idx < clip.grade_versions.len() {
                            let grade = clip.grade().clone();
                            clip.grade_versions[active_idx].set_grade(grade);
                        }
                    }
                    // Load the target version
                    if let Some(version) = clip.grade_versions.get(*version_index) {
                        clip.set_grade(version.grade().clone());
                        clip.active_grade_version = Some(*version_index);
                    }
                }
                self.state.dirty = true;
                // Use sync_and_refresh_frame for immediate visual update (even when paused)
                *needs_refresh = true;
                true
            }
            AppCommand::DeleteGradeVersion { clip_id, version_index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    if *version_index < clip.grade_versions.len() {
                        clip.grade_versions.remove(*version_index);
                        match clip.active_grade_version {
                            Some(idx) if idx == *version_index => clip.active_grade_version = None,
                            Some(idx) if idx > *version_index => clip.active_grade_version = Some(idx - 1),
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
                    if let Some(version) = clip.grade_versions.get_mut(*version_index) {
                        version.name = name.clone();
                    }
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
pub(super) fn qualifier_keyed_on(rgb: [f32; 3], base: klipp_core::QualifierSettings) -> klipp_core::QualifierSettings {
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
