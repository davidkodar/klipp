//! The control socket's side of the app: commands taken from its inbox, the
//! document ops and edit plans it asks for, and what the app publishes back
//! (transport, viewer comparison, window captures, analysis markers) (#826).

use super::*;

impl KlippApp {
    /// Route queued control-socket commands through `send_commands`, the
    /// same entry point the UI's own widgets use.
    ///
    /// A remote client cannot see or answer a modal, so control commands
    /// never open one: the frame-rate mismatch prompt is suppressed and the
    /// append goes ahead without changing the project frame rate — the
    /// prompt's non-destructive choice. (Before this, an MCP append of a
    /// clip at another frame rate was reported as accepted, then sat behind
    /// a dialog on the user's screen and never happened.) One command at a
    /// time, because the append handler consumes the suppression, and the
    /// flag is cleared afterwards so it cannot leak into the user's next
    /// drag-and-drop.
    pub(super) fn drain_control_inbox(&mut self) {
        let Some(inbox) = self.control_inbox.as_ref() else { return };
        let envelopes: Vec<klipp_document::control::ControlEnvelope> = inbox.try_iter().collect();
        for env in envelopes {
            let mut refused = None;
            let mut refused_op = None;
            let mut renamed = Vec::new();
            let mut ungated = Vec::new();
            // A graph-ops batch travels alone (node-graph M2 step 15d): one
            // batch and nothing else, so nothing of the envelope lands
            // before the batch is refused. It is checked against the
            // project and the revision it was checked against, as a
            // resolved plan is, and applied here rather than queued, so
            // the reply carries a refusal and the op it stopped at.
            let mut env = env;
            match graph_batch(&mut env) {
                Err(why) => refused = Some(why),
                Ok(None) => {}
                Ok(Some((ops, label))) => {
                    let project_identity = self.state.control_shared.project.lock().ok().map(|p| p.identity);
                    let now = self.state.control_shared.revision();
                    if let Some(want) = env.expected_project_identity.filter(|want| project_identity != Some(*want)) {
                        refused = Some(format!("the project changed since these ops were checked (expected identity {want}); check them again"));
                    } else if let Some(r) = env.expected_revision.filter(|r| *r != now) {
                        refused = Some(format!("the document is at revision {now}, the ops were checked at {r}; check them again"));
                    } else {
                        match self.apply_graph_ops(&ops, &label) {
                            Ok(outcome) => { renamed = outcome.renamed; ungated = outcome.ungated; },
                            Err((at, why)) => {
                                refused = Some(why);
                                refused_op = at;
                            }
                        }
                    }
                }
            }
            // Whether the project was already unsaved: the reply names
            // "edits" as the cause only when this envelope made it so. A
            // play or a seek over a project unsaved for another reason
            // (analysis markers arriving) used to relabel it as edits.
            let was_dirty = self.state.dirty;
            // A resolved plan: its commands forward, in order, as one
            // history entry — unless the document moved since it was
            // resolved, in which case it was made for a document that no
            // longer exists and nothing of it is applied.
            if !env.edits.is_empty() {
                let now = self.state.control_shared.revision();
                let project_identity = self.state.control_shared.project.lock().ok().map(|p| p.identity);
                if let Some(want) = env.expected_project_identity {
                    if project_identity != Some(want) {
                        refused = Some(format!("the project changed since this plan was made (expected identity {want}, current identity {}); plan again", project_identity.map_or_else(|| "unavailable".into(), |id| id.to_string())));
                    }
                }
                if refused.is_none() {
                    if let Some(r) = env.expected_revision {
                        if r != now {
                            refused = Some(format!("the document is at revision {now}, the plan was made at {r}; plan again"));
                        }
                    }
                }
                // A sequence the edits copied from, changed since it was
                // read: they would paste what it no longer holds (#2385).
                if refused.is_none() {
                    refused = changed_source(&self.state.sequences, &env.expected_sources);
                }
                if refused.is_none() {
                    self.apply_edits_as_one(&env.edits, env.label.as_deref().unwrap_or("Socket Edits"));
                    // A plan proposed for the person's approval opens
                    // where they can read it.
                    if env.edits.iter().any(|e| matches!(e, EditCommand::SetEditPlan { new: Some(p), .. } if !p.approved)) {
                        self.state.show_edit_plan = true;
                        self.state.edit_plan_note = None;
                    }
                }
            }
            if refused.is_none() {
                let active = &self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = param_write_outside(active, &env.commands) {
                    refused = Some(format!("clip {} is inside a compound, not on this sequence's own tracks: open its sequence to grade it", clip.0));
                }
            }
            let mut sought = None;
            if refused.is_none() && !env.commands.is_empty() {
                sought = last_seek(&env.commands);
                self.suppress_fps_prompt = true;
                let touches_params = env.commands.iter().any(|c| matches!(c, AppCommand::SetNodeParam { .. } | AppCommand::AddKeyframe { .. } | AppCommand::RemoveKeyframe { .. }));
                let import_request = env.commands.iter().find_map(|c| match c {
                    AppCommand::ImportTimeline { request_id, .. } | AppCommand::OpenFromAnotherEditor { request_id, .. } => Some(*request_id), _ => None,
                });
                let project_switch = env.commands.iter().any(|c| matches!(c, AppCommand::OpenProjectNow { .. } | AppCommand::CloseProjectNow { .. }));
                let project_before = project_switch.then(|| self.state.control_shared.project.lock().unwrap_or_else(|e|e.into_inner()).generation);
                let cmds = expand_control_commands(env.commands, env.batch);
                self.send_commands(cmds);
                self.suppress_fps_prompt = false;
                if let Some(before) = project_before {
                    // The handler has returned, including any save, load and
                    // post-save work. Capture its final error in THIS ack,
                    // before another envelope can change the shared status.
                    let project = self.state.control_shared.project.lock().unwrap_or_else(|e|e.into_inner());
                    refused = if project.generation == before {
                        Some("project switch did not complete".into())
                    } else { project.last_error.clone() };
                }
                if let Some(request_id) = import_request {
                    let outcome = self.state.control_shared.import_outcome.lock().unwrap_or_else(|e| e.into_inner());
                    match outcome.as_ref().filter(|o| o["request_id"] == serde_json::json!(request_id)) {
                        Some(o) => { refused = o["error"].as_str().map(str::to_owned); }
                        None => { refused = Some("The import was not applied; check whether the project is read-only.".into()); }
                    }
                }
                // A parameter write reaches the engine through a queue it
                // drains on its next frame; the socket's reads look at the
                // engine's sequence. Push the mirror now, so what the client
                // reads after the acknowledgement is what it wrote.
                if touches_params {
                    self.sync_sequence_to_engine();
                }
            }
            if let Some(ack) = env.ack {
                // The project's state as of this write, for a status read
                // straight after it: the per-frame mirror had not run yet,
                // and the project read as saved a moment after an edit.
                if let Ok(mut project) = self.state.control_shared.project.lock() {
                    project.recent_projects = self.prefs.general.recent_projects.iter().map(crate::prefs::RecentProject::control_summary).collect();
                    project.path = self.state.project_path.clone();
                    project.dirty = self.state.dirty;
                    project.undo = self.state.undo_label();
                    project.redo = self.state.redo_label();
                    project.undo_depth = self.state.undo_depth();
                    project.redo_depth = self.state.redo_depth();
                    let (undo_scope, redo_scope) = self.state.undo_scopes();
                    (project.undo_scope, project.redo_scope) = (undo_scope.map(str::to_owned), redo_scope.map(str::to_owned));
                    (project.sequences, project.active_sequence) = self.state.sequence_summaries();
                    if self.state.dirty && !was_dirty && refused.is_none() {
                        project.changed_by = Some("edits".into());
                    }
                }
                // The playhead too. A seek goes on to the engine, and the
                // UI's playhead follows only when the engine reports back,
                // so a frame read straight after an acknowledged seek held
                // the position from before it and put the playhead back
                // there. What this envelope sought is where the user is.
                self.publish_transport(sought);
                let _ = ack.send(klipp_document::control::ControlAck { revision: self.state.control_shared.revision(), refused, refused_op, renamed, ungated });
            }
        }
    }

    /// Resolve and apply the document ops panels asked for, each request
    /// one undo step. A refusal says why in a toast; a request of
    /// independent ops applies the ones that resolve and names the rest.
    pub(super) fn run_pending_doc_ops(&mut self) {
        use klipp_document::ops::{self, Anchor, Op, Refusal, RefusalCode};
        for crate::state::DocOpsRequest { label, ops, independent } in std::mem::take(&mut self.state.pending_doc_ops) {
            self.transcript_source.sync(self.state.analysis_generation);
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            let resolved = if independent {
                Ok(ops::resolve_each_with(seq, &self.state.media_pool, &ops, &self.transcript_source))
            } else {
                ops::resolve_with(seq, &self.state.media_pool, &ops, &self.transcript_source).map(|plan| (plan, Vec::new()))
            };
            // A refusal, as the person at the keyboard reads it: where, and
            // why. The document's hints are written for whoever asked, the
            // assistant included; the one a person meets from the captions
            // form is said in the form's own words: tick Replace when it is
            // not ticked, and when it is, the captions in the way lie
            // outside the range, so widen it.
            let said = |r: &Refusal| {
                let hint = if r.hint.is_empty() { String::new() } else { format!(" \u{2014} {}", r.hint) };
                match ops.get(r.step) {
                    Some(Op::Cut { from: Anchor::Secs(a), to: Anchor::Secs(b) }) => format!("{a:.2}-{b:.2} s: {}{hint}", r.why),
                    Some(Op::Captions { replace: false, .. }) if r.code == Some(RefusalCode::CaptionsOverlap) => "Captions already exist in that range; tick Replace to overwrite them".to_string(),
                    Some(Op::Captions { replace: true, .. }) if r.code == Some(RefusalCode::CaptionsOverlap) => format!("{}; widen the range to take those in, or choose another track", r.why),
                    _ => format!("{}{hint}", r.why),
                }
            };
            // A sweep's label counts what was done, not what was asked.
            let label_for = |n: usize| label.replace("{n}", &n.to_string());
            match resolved {
                Ok((plan, refused)) if !plan.steps.is_empty() => {
                    let label = label_for(plan.steps.len());
                    let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                    self.apply_edits_as_one(&edits, &label);
                    let what: Vec<&str> = plan.steps.iter().map(|s| s.what.as_str()).collect();
                    let mut toast = format!("{label}: {}", what.join("; "));
                    if !refused.is_empty() {
                        let why: Vec<String> = refused.iter().map(said).collect();
                        toast.push_str(&format!("; {} not done: {}", refused.len(), why.join("; ")));
                    }
                    self.state.toast.show(toast);
                }
                Ok((_, refused)) => {
                    let why: Vec<String> = refused.iter().map(said).collect();
                    self.state.toast.show(format!("{}: not done, {}", label_for(ops.len()), why.join("; ")));
                }
                Err(r) => self.state.toast.show(format!("{}: not done, {}", label_for(ops.len()), said(&r))),
            }
        }
    }

    /// What the Edit Plan window asked for: approval recorded, the plan
    /// built after what is down, or the plan discarded — each one undo step.
    pub(super) fn act_on_edit_plan(&mut self, action: crate::panels::edit_plan::PlanAction) {
        use crate::panels::edit_plan::PlanAction;
        let Some(plan) = self.state.sequence().edit_plan.clone() else { return };
        match action {
            PlanAction::None => {}
            PlanAction::Approve => {
                let mut approved = plan.clone();
                approved.approved = true;
                self.apply_edits_as_one(&[EditCommand::SetEditPlan { old: Some(Box::new(plan)), new: Some(Box::new(approved)) }], "Approve Edit Plan");
                self.state.edit_plan_note = Some("Approved. Build places it after whatever is on the timeline.".into());
            }
            PlanAction::Discard => {
                self.apply_edits_as_one(&[EditCommand::SetEditPlan { old: Some(Box::new(plan)), new: None }], "Discard Edit Plan");
                self.state.edit_plan_note = None;
            }
            PlanAction::Build => {
                if !plan.approved {
                    self.state.edit_plan_note = Some("Approve the plan first.".into());
                    return;
                }
                let (edits, summary) = {
                    let seq = self.state.sequence();
                    let pool = &self.state.media_pool;
                    if let Err(e) = klipp_document::edit_plan::validate(&plan, pool, seq.brief.as_ref()) {
                        self.state.edit_plan_note = Some(format!("The plan no longer builds: {e}"));
                        return;
                    }
                    let ops = klipp_document::edit_plan::to_ops(&plan, seq, pool, seq.brief.as_ref());
                    match klipp_document::ops::resolve_with(seq, pool, &ops, &klipp_document::ops::NoTranscripts) {
                        Ok(resolved) => {
                            let edits: Vec<EditCommand> = resolved.steps.iter().map(|s| s.edit.clone()).collect();
                            (edits, format!("Built: {} steps, the timeline now runs {:.1} s.", resolved.steps.len(), resolved.duration_secs))
                        }
                        Err(r) => {
                            self.state.edit_plan_note = Some(format!("Step {} could not be built: {}. {}", r.step + 1, r.why, r.hint));
                            return;
                        }
                    }
                };
                self.apply_edits_as_one(&edits, "Build Edit Plan");
                self.state.edit_plan_note = Some(summary);
            }
        }
    }

    /// Share the document revision and the render queue's state with the
    /// control socket.
    /// Tell control-socket clients which compare the viewer is showing:
    /// the frame they read is the whole graded picture, and the person is
    /// looking at a split of it.
    pub(super) fn publish_viewer_compare(&self) {
        let s = &self.state;
        let still = s.grade_still_compare.then(|| s.active_grade_still.and_then(|i| s.grade_stills.get(i))).flatten();
        let pct = (s.split_position * 100.0).round();
        let compare = if let Some(still) = still {
            Some(format!("grade still split: left {pct}% shows the still \"{}\", right shows the graded frame", still.name))
        } else if s.before_after_active {
            Some(format!("before/after split: left {pct}% shows the frame ungraded, right graded"))
        } else if s.stabilization_compare_active {
            Some(format!("stabilisation split: left {pct}% unstabilised"))
        } else if s.split_compare_node.is_some() {
            Some(format!("effect split: left {pct}% with the effect toggled"))
        } else {
            None
        };
        if let Ok(mut shown) = s.control_shared.viewer_compare.lock() {
            if *shown != compare {
                *shown = compare;
            }
        }
    }

    /// Where the project's other sequences are published for the control
    /// socket (#2385).
    pub fn set_control_others(&mut self, others: klipp_document::ops::SharedSelects) {
        self.state.control_others = others;
        self.state.control_others_for = None;
    }

    /// Publish the project's other sequences for the control socket when
    /// which they are, or which is open, has changed (#2385): the only
    /// times one of them can have changed, as edits go to the open one. A
    /// publish behind its sequence is refused at apply, never pasted.
    pub(crate) fn publish_control_others(&mut self) {
        let key = (self.state.sequences.iter().map(|s| s.id).collect::<Vec<_>>(), self.state.active_sequence_idx);
        if self.state.control_others_for.as_ref() == Some(&key) {
            return;
        }
        let others = self
            .state
            .sequences
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.state.active_sequence_idx)
            .map(|(_, s)| klipp_document::ops::Selects { content: klipp_document::clipboard::content_revision(s), sequence: std::sync::Arc::new(s.clone()) })
            .collect();
        if let Ok(mut published) = self.state.control_others.write() {
            *published = others;
            self.state.control_others_for = Some(key);
        }
    }

    pub fn set_control_shared(&mut self, shared: std::sync::Arc<klipp_core::ControlShared>) {
        self.state.control_shared = std::sync::Arc::clone(&shared);
        // Histories made before this arrived point at the default counter.
        for h in self.state.command_histories.values_mut() {
            h.set_revision_shared(std::sync::Arc::clone(&shared));
        }
        self.state.project_history.set_revision_shared(std::sync::Arc::clone(&shared));
    }

    /// Analysis that finished since the last frame, put on its media
    /// items as markers: a cut between shots, a stretch of silence.
    pub(super) fn apply_analysis_markers(&mut self) {
        let done = klipp_analysis::service::take_completed();
        if done.is_empty() {
            return;
        }
        let mut changed = false;
        for id in done {
            let Some(path) = self.state.media_pool.get(&id).map(|i| i.path.clone()) else { continue };
            let report = klipp_analysis::report::MediaAnalysis::load(&path);
            changed |= self.state.apply_analysis(id, &report);
        }
        if changed {
            self.sync_media_pool_to_engine();
        }
    }

    /// A whole-window capture: ask egui for the next frame's picture,
    /// then write it where it was asked for, scaled down to `max_width`.
    /// The image is the window as drawn: the viewer through its monitor
    /// LUT and overlays, so not a frame to measure colour from.
    pub(super) fn serve_window_capture(&mut self, ctx: &egui::Context) {
        let Some((path, max_width, asked)) = self.state.window_capture.clone() else { return };
        if !asked {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            ctx.request_repaint();
            self.state.window_capture = Some((path, max_width, true));
            return;
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = shot else {
            ctx.request_repaint();
            return;
        };
        self.state.window_capture = None;
        let [w, h] = image.size;
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
        let Some(mut img) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) else { return };
        if let Some(mw) = max_width.filter(|mw| *mw > 0 && (*mw as usize) < w) {
            let nh = ((h as f64) * mw as f64 / w as f64).round().max(1.0) as u32;
            img = image::imageops::resize(&img, mw, nh, image::imageops::FilterType::Triangle);
        }
        if let Err(e) = img.save(&path) {
            tracing::warn!("window capture to {}: {e}", path.display());
        }
    }

    /// Where the user's playhead is, for the control socket: a frame
    /// read puts the playhead back here when it is done.
    pub(super) fn publish_transport(&self, sought: Option<klipp_core::Timecode>) {
        if let Ok(mut transport) = self.state.control_shared.transport.lock() {
            *transport = klipp_core::TransportStatus {
                playhead: Some(sought.unwrap_or(self.state.playhead)),
                playing: self.state.playback == PlaybackState::Playing,
            };
        }
    }
}

impl KlippApp {
    /// Attach the control socket's command inbox. See `control_inbox`.
    pub fn set_control_inbox(&mut self, inbox: Receiver<klipp_document::control::ControlEnvelope>) {
        self.control_inbox = Some(inbox);
    }
}

// Pinned: each apply_* helper writes the new value to BOTH the
// persisted prefs side AND the runtime state side. Tests construct
// fresh defaults, call the helper, and assert both sides match.
// The handler arms in the AppCommand match call these helpers and
// add `prefs_dirty = true` — the dirty flag is the App's
// responsibility, not the helper's, so it isn't tested here.
/// The commands of one control envelope as the router applies them. A
/// parameter write is bracketed the way a slider drag is, so it records
/// history (without the brackets it never did); a batch wraps everything
/// in one history batch, so an agent's action is one undo step.
pub fn expand_control_commands(commands: Vec<AppCommand>, batch: bool) -> Vec<AppCommand> {
    let mut out = Vec::with_capacity(commands.len() * 3 + 2);
    if batch {
        out.push(AppCommand::BeginHistoryBatch);
    }
    for cmd in commands {
        match &cmd {
            AppCommand::SetNodeParam { clip_id, .. } => {
                let clip_id = *clip_id;
                out.push(AppCommand::BeginNodeParamEdit { clip_id });
                out.push(cmd);
                out.push(AppCommand::EndNodeParamEdit { clip_id });
            }
            _ => out.push(cmd),
        }
    }
    if batch {
        out.push(AppCommand::EndHistoryBatch);
    }
    out
}

/// The first parameter write in `cmds` whose clip isn't one of `seq`'s own.
/// The socket checks a clip id against the engine's flattened copy, where
/// a compound's clips sit under their own ids, but a write applies only to
/// the active sequence's own clips: it would be acknowledged and change
/// nothing (#1082).
pub(super) fn param_write_outside(seq: &klipp_timeline::Sequence, cmds: &[AppCommand]) -> Option<klipp_core::ClipId> {
    cmds.iter().find_map(|c| match c {
        AppCommand::SetNodeParam { clip_id, .. } | AppCommand::AddKeyframe { clip_id, .. } | AppCommand::RemoveKeyframe { clip_id, .. } if !seq.tracks.iter().any(|t| t.clips.iter().any(|x| x.id == *clip_id)) => Some(*clip_id),
        _ => None,
    })
}

/// The last absolute seek among a control envelope's commands: where the
/// playhead is headed once they are applied.
pub(super) fn last_seek(cmds: &[AppCommand]) -> Option<klipp_core::Timecode> {
    cmds.iter().rev().find_map(|c| match c {
        AppCommand::SeekTo(t) => Some(*t),
        _ => None,
    })
}

impl crate::state::AppState {
    /// `report`'s results put on media item `id` (#2104): its analysis
    /// markers replaced whole (the editor's own are not touched) and its
    /// screen text. Whether anything shown changed.
    ///
    /// Not an edit: the project is left as saved. These are made from the
    /// analysis cache, again on every open; marking the project unsaved
    /// for them had a project opened and only played ask to be saved
    /// before it could be closed.
    pub(crate) fn apply_analysis(&mut self, id: klipp_core::MediaId, report: &klipp_analysis::report::MediaAnalysis) -> bool {
        let Some(item) = self.media_pool.get_mut(&id) else { return false };
        if report.is_empty() {
            return false;
        }
        let before = item.markers.clone();
        item.markers.retain(|m| m.kind != klipp_core::MediaMarkerKind::Analysis);
        if let Some(shots) = &report.shots {
            for b in &shots.boundaries {
                item.markers.push(klipp_core::MediaMarker { secs: *b, duration_secs: 0.0, name: "cut".into(), kind: klipp_core::MediaMarkerKind::Analysis });
            }
        }
        if let Some(audio) = &report.audio {
            for sp in audio.silences.iter().filter(|s| s.end_secs - s.start_secs >= 0.8) {
                let mut m = klipp_core::MediaMarker::range(klipp_core::MediaMarkerKind::Analysis, sp.start_secs, sp.end_secs);
                m.name = "silence".into();
                item.markers.push(m);
            }
        }
        item.markers.sort_by(|a, b| a.secs.total_cmp(&b.secs));
        let mut changed = item.markers != before;
        // What the screen showed, each line once, for the pool's search,
        // its Screen text column and smart-bin rules.
        let digest = report.screentext.as_ref().map(screen_text_digest).unwrap_or_default();
        if item.screen_text != digest {
            item.screen_text = digest;
            changed = true;
        }
        // The role the sound proposes for the file's clips (#2216).
        let proposal = report.audio_role.as_ref().and_then(|r| r.proposal());
        if item.audio_role != proposal {
            item.audio_role = proposal;
            changed = true;
        }
        self.analysis_generation += 1;
        changed
    }
}

/// The lines a screen recording showed, each once in the order first
/// seen, joined for a field; cut at a few thousand characters so a
/// long recording's chrome does not swamp the pool.
pub(super) fn screen_text_digest(st: &klipp_analysis::screentext::ScreenText) -> String {
    const MAX_CHARS: usize = 4000;
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for s in &st.spans {
        let key: String = s.text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if key.len() < 2 || !seen.insert(key) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(s.text.trim());
        if out.len() > MAX_CHARS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod control_envelope_tests {
    use super::*;
    use klipp_core::ClipId;

    #[test]
    fn a_parameter_write_over_the_socket_is_bracketed_like_a_drag() {
        let clip_id = ClipId::new();
        let cmds = expand_control_commands(
            vec![AppCommand::SetNodeParam { clip_id, node_id: klipp_nodes::NodeId::new_v4(), param: "exposure".into(), value: 0.5 }],
            false,
        );
        assert!(matches!(cmds.as_slice(), [AppCommand::BeginNodeParamEdit { .. }, AppCommand::SetNodeParam { .. }, AppCommand::EndNodeParamEdit { .. }]));
    }

    #[test]
    fn a_batch_is_one_history_entry() {
        let cmds = expand_control_commands(vec![AppCommand::Play, AppCommand::Pause], true);
        assert!(matches!(cmds.first(), Some(AppCommand::BeginHistoryBatch)));
        assert!(matches!(cmds.last(), Some(AppCommand::EndHistoryBatch)));
        assert_eq!(cmds.len(), 4);
        // And the history collects what a batch pushes into one entry.
        let mut h = crate::history::CommandHistory::new();
        h.begin_batch();
        h.push(crate::history::HistoryEntry { command: crate::history::EditCommand::Batch(vec![]) });
        h.push(crate::history::HistoryEntry { command: crate::history::EditCommand::Batch(vec![]) });
        h.end_batch();
        assert!(h.undo().is_some());
        assert!(h.undo().is_none(), "two pushes inside a batch are one undo step");
    }
}

#[cfg(test)]
mod param_write_outside_tests {
    use super::{param_write_outside, AppCommand};
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{flatten::flatten_sequence, Clip, ClipKind, Sequence, Track, TrackKind};

    /// The socket accepts a compound's clip, since the engine's flattened
    /// copy holds it; the write is refused here, where the document says
    /// it isn't the sequence's own (#1082).
    #[test]
    fn a_write_to_a_clip_inside_a_compound_is_named() {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let mut scene = Sequence::new_default();
        let inner = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let inner_id = inner.id;
        scene.tracks = vec![Track::new("V1", TrackKind::Video)];
        scene.tracks[0].clips = vec![inner];
        let mut active = Sequence::new_default();
        let own = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let own_id = own.id;
        let mut compound = Clip::gap(t(2.0), t(4.0));
        compound.kind = ClipKind::Sequence { sequence_id: scene.id };
        active.tracks = vec![Track::new("V1", TrackKind::Video)];
        active.tracks[0].clips = vec![own, compound];
        let flat = flatten_sequence(&active, &[active.clone(), scene.clone()]);
        assert!(flat.all_clips().any(|c| c.id == inner_id), "the engine's copy holds the compound's clip");

        let write = |clip_id| AppCommand::SetNodeParam { clip_id, node_id: klipp_nodes::NodeId::new_v4(), param: "exposure".into(), value: 0.5 };
        assert_eq!(param_write_outside(&active, &[write(own_id)]), None, "the sequence's own clip is written");
        assert_eq!(param_write_outside(&active, &[AppCommand::Play, write(inner_id)]), Some(inner_id));
        assert!(param_write_outside(&active, &[write(ClipId::new())]).is_some(), "and one that's nowhere");
    }
}

#[cfg(test)]
mod last_seek_tests {
    use super::{last_seek, AppCommand};
    use klipp_core::Timecode;

    #[test]
    fn the_last_seek_of_an_envelope_is_where_the_playhead_is_going() {
        let (a, b) = (Timecode::from_frames(30, 30), Timecode::from_frames(90, 30));
        let cmds = [AppCommand::Pause, AppCommand::SeekTo(a), AppCommand::Play, AppCommand::SeekTo(b)];
        assert_eq!(last_seek(&cmds), Some(b));
        assert_eq!(last_seek(&[AppCommand::Play]), None);
    }
}

#[cfg(test)]
mod analysis_is_not_an_edit_tests {
    use klipp_analysis::report::MediaAnalysis;
    use klipp_analysis::shots::Shots;

    /// #2104: a project opened and only played came up unsaved: the
    /// analysis cache's markers arriving on its media marked it changed.
    /// They arrive and show, and the project stays as saved; the editor's
    /// own marker, the control, is an edit.
    #[test]
    fn analysis_arriving_leaves_the_project_saved_and_a_marker_does_not() {
        let mut state = crate::state::AppState::default();
        let id = klipp_core::MediaId::new();
        let fps = klipp_core::Fps::new(30, 1);
        state.media_pool.add(klipp_media::MediaItem::new_synthetic_title(id, "shot.mp4".into(), klipp_core::Timecode::from_frames(300, 30), fps));
        let report = MediaAnalysis { shots: Some(Shots::from_boundaries(10.0, vec![2.0, 6.5])), ..Default::default() };
        assert!(state.apply_analysis(id, &report), "the markers are new: shown");
        let cuts = state.media_pool.get(&id).unwrap().markers.iter().filter(|m| m.kind == klipp_core::MediaMarkerKind::Analysis).count();
        assert_eq!(cuts, 2);
        assert!(!state.dirty, "analysis arriving is no edit");
        assert!(!state.apply_analysis(id, &report), "the same again changes nothing");
        state.add_media_marker(id, klipp_core::MediaMarker::note(4.0));
        assert!(state.dirty, "the editor's own marker is an edit");
    }
}

#[cfg(test)]
mod screen_text_digest_tests {
    use super::screen_text_digest;
    use klipp_analysis::screentext::{ScreenText, TextSpan};

    #[test]
    fn the_digest_lists_each_line_once_in_order_first_seen() {
        let span = |t: &str, a: f64| TextSpan { text: t.into(), confidence: 0.9, start_secs: a, end_secs: a + 4.0, x: 0.1, y: 0.1, w: 0.2, h: 0.02, frames: 1 };
        let st = ScreenText { spans: vec![span("SETTINGS", 0.0), span("General", 0.0), span("settings", 8.0), span("  General ", 12.0), span("x", 12.0)], ..Default::default() };
        assert_eq!(screen_text_digest(&st), "SETTINGS · General");
        assert_eq!(screen_text_digest(&ScreenText::default()), "");
    }
}

/// An envelope's graph-ops batch, taken out of it: `None` when it holds
/// none. A batch travels alone, one batch with no other edits or commands
/// beside it, so nothing of the envelope lands before the batch could be
/// refused; otherwise the whole envelope is refused, and nothing is left
/// in it to send.
fn graph_batch(env: &mut klipp_document::control::ControlEnvelope) -> Result<Option<(Vec<klipp_graph_lift::ops::GraphOp>, String)>, String> {
    let batches = env.commands.iter().filter(|c| matches!(c, AppCommand::GraphOps { .. })).count();
    if batches == 0 {
        return Ok(None);
    }
    let commands = std::mem::take(&mut env.commands);
    if batches > 1 || commands.len() > 1 || !env.edits.is_empty() {
        env.edits.clear();
        return Err("a graph-ops batch travels alone: one batch, with no other edits or commands in its envelope".into());
    }
    match commands.into_iter().next() {
        Some(AppCommand::GraphOps { ops, label }) => Ok(Some((ops, label))),
        _ => unreachable!("the one command is the batch"),
    }
}

#[cfg(test)]
mod graph_batch_tests {
    use super::*;
    use klipp_document::control::ControlEnvelope;

    fn envelope(commands: Vec<AppCommand>, edits: Vec<EditCommand>) -> ControlEnvelope {
        ControlEnvelope { commands, edits, batch: false, expected_revision: None, expected_project_identity: None, expected_sources: Vec::new(), label: None, ack: None }
    }

    fn batch() -> AppCommand {
        AppCommand::GraphOps { ops: Vec::new(), label: "Graph".into() }
    }

    /// One batch alone is taken out; beside anything else, the whole
    /// envelope is refused and nothing is left in it to apply.
    #[test]
    fn a_graph_batch_travels_alone() {
        let mut alone = envelope(vec![batch()], Vec::new());
        assert!(matches!(graph_batch(&mut alone), Ok(Some(_))));
        assert!(alone.commands.is_empty());
        for mut mixed in [
            envelope(vec![batch(), batch()], Vec::new()),
            envelope(vec![batch(), AppCommand::Pause], Vec::new()),
            envelope(vec![AppCommand::Pause, batch()], Vec::new()),
            envelope(vec![batch()], vec![EditCommand::Batch(Vec::new())]),
        ] {
            assert!(graph_batch(&mut mixed).unwrap_err().contains("travels alone"));
            assert!(mixed.commands.is_empty() && mixed.edits.is_empty(), "nothing left to apply");
        }
        let mut none = envelope(vec![AppCommand::Pause], Vec::new());
        assert!(matches!(graph_batch(&mut none), Ok(None)));
        assert_eq!(none.commands.len(), 1, "an envelope without a batch is left as it was");
    }
}

/// Why edits that copied from `sources` can't land: one of them is gone,
/// or holds other than it did when read (#2385). `None` when all stand.
fn changed_source(sequences: &[klipp_timeline::Sequence], sources: &[(klipp_core::SequenceId, u64)]) -> Option<String> {
    sources.iter().find_map(|(id, read)| match sequences.iter().find(|s| s.id == *id) {
        None => Some("a sequence the plan copied from is gone; plan again".to_string()),
        Some(s) if klipp_document::clipboard::content_revision(s) != *read => Some(format!("\"{}\" changed since the plan copied from it; plan again", s.name)),
        Some(_) => None,
    })
}

#[cfg(test)]
mod changed_source_tests {
    use super::changed_source;

    /// #2385: edits that copied from a sequence land only while it holds
    /// what was read; changed or gone, they are refused, naming it.
    #[test]
    fn a_copy_from_a_changed_or_gone_sequence_is_refused() {
        let mut seqs = vec![klipp_timeline::Sequence::new_default(), klipp_timeline::Sequence::new_default()];
        seqs[1].name = "Selects".into();
        let read = vec![(seqs[1].id, klipp_document::clipboard::content_revision(&seqs[1]))];
        assert_eq!(changed_source(&seqs, &read), None);
        seqs[1].tracks.pop();
        assert_eq!(changed_source(&seqs, &read).as_deref(), Some("\"Selects\" changed since the plan copied from it; plan again"));
        seqs.pop();
        assert!(changed_source(&seqs, &read).unwrap().contains("gone"));
    }
}
