//! Playback: play, pause, seek and the transport's commands (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_playback_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::Undo => {
                self.apply_undo();
                true
            }
            AppCommand::Redo => {
                self.apply_redo();
                true
            }
            AppCommand::SquashUndo { count, label, only, expected_revision } => {
                // Only the steps it names, and only if nothing has moved
                // the document since: never an edit of the person's.
                if self.state.control_shared.revision() == *expected_revision {
                    self.state.active_history_mut().squash_last(*count, label, only);
                }
                true
            }
            AppCommand::BeginHistoryBatch => {
                self.state.active_history_mut().begin_batch();
                true
            }
            AppCommand::EndHistoryBatch => {
                self.state.active_history_mut().end_batch();
                true
            }
            AppCommand::SetRubberBandParam(param) => {
                self.state.rubber_band_param = crate::state::RubberBandParam::ClipParam(*param);
                true
            }
            AppCommand::SetVideoRubberBandParam(param) => {
                self.state.video_rubber_band_param = *param;
                true
            }
            AppCommand::SequenceUpdated => {
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::SetSpeed(speed) => {
                // Re-anchor interpolation at current position with new speed
                self.state.playhead_anchor = self.state.playhead;
                self.state.playhead_anchor_wall = std::time::Instant::now();
                self.state.playback_speed = *speed;
                // Fall through to forward to engine
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::BeginScrub => {
                // Forward first, so the engine is in scrub mode for the
                // proxy switch below as well as for the rest of the drag.
                // Handling a command here returns `true`, which is what
                // stops `send_commands` forwarding it — without this the
                // engine's `is_scrubbing` was never set by anything.
                let _ = self.cmd_tx.send(cmd.clone());
                // Auto-switch to proxy during scrub if preference enabled
                if self.prefs.proxy.auto_switch_during_scrub {
                    self.state.pre_scrub_use_proxies = self.state.use_proxies;
                    self.state.use_proxies = true;
                    let _ = self.cmd_tx.send(AppCommand::SetUseProxies(true));
                }
                true
            }
            AppCommand::EndScrub => {
                // Forward first: the proxy restore, the full-res re-seek
                // and the `SetPlayhead` that `scrub_end` pushes next all
                // travel the same channel, so clearing `is_scrubbing`
                // ahead of them leaves the release seek settling exactly
                // as it did before.
                let _ = self.cmd_tx.send(cmd.clone());
                // Restore proxy state after scrub
                if self.prefs.proxy.auto_switch_during_scrub {
                    self.state.use_proxies = self.state.pre_scrub_use_proxies;
                    let _ = self.cmd_tx.send(AppCommand::SetUseProxies(self.state.pre_scrub_use_proxies));
                    // If proxies were off before scrub, re-seek at full-res so the
                    // viewer shows the final position in full quality
                    if !self.state.pre_scrub_use_proxies {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                }
                true
            }
            AppCommand::SetSnapMaster(enabled) => {
                self.state.snap.master = *enabled;
                self.layout.snap = self.state.snap;
                self.layout_dirty = true;
                true
            }
            AppCommand::SetSnapTarget { target, enabled } => {
                use klipp_core::SnapTargetKind;
                match target {
                    SnapTargetKind::ClipEdge => self.state.snap.clips = *enabled,
                    SnapTargetKind::Playhead => self.state.snap.playhead = *enabled,
                    SnapTargetKind::Marker => self.state.snap.markers = *enabled,
                    // SequenceStart / InPoint / OutPoint share one
                    // user-facing toggle. The kind variants stay
                    // distinct for the on-snap legend.
                    SnapTargetKind::SequenceStart | SnapTargetKind::InPoint | SnapTargetKind::OutPoint => {
                        self.state.snap.in_out = *enabled;
                    }
                    SnapTargetKind::Grid => self.state.snap.grid = *enabled,
                }
                self.layout.snap = self.state.snap;
                self.layout_dirty = true;
                true
            }
            AppCommand::SetGridInterval(interval) => {
                // Mirror to both prefs (persisted) and state
                // (consulted live by the snap-resolution layer).
                self.prefs.editing.grid_snap_interval = *interval;
                self.state.grid_snap_interval = *interval;
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetSnapThresholdPx(val) => {
                apply_snap_threshold_px(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetZoomStep(val) => {
                apply_zoom_step(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetMagneticTimeline(val) => {
                apply_magnetic_timeline(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetDefaultSequenceResolutionIdx(val) => {
                apply_default_sequence_resolution_idx(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetDefaultTransitionFrames(val) => {
                apply_default_transition_frames(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetShiftStepFrames(val) => {
                apply_shift_step_frames(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            // Play once from the In to the Out (#921). The frame loop
            // pauses at the Out: `stop_at_out`.
            AppCommand::PlayInToOut => {
                let seq = self.state.sequence();
                let from = seq.in_point.unwrap_or(klipp_core::Timecode { value: 0, rate: self.state.fps.rate() });
                let to = seq.out_point.unwrap_or_else(|| self.state.sequence_duration());
                if to > from {
                    self.state.play_to_out = Some(crate::state::PlayToOut { from, out: to, asked: std::time::Instant::now(), started: false, looping: false });
                    // The playhead is at the In from now: the first frames
                    // after Play must not read the old position, past the
                    // Out, and stop at once (#940 review).
                    self.state.playhead = from;
                    self.send_commands(vec![AppCommand::SetSpeed(1.0), AppCommand::SeekTo(from), AppCommand::Play]);
                }
                true
            }
            AppCommand::SetMulticamShowAngles(val) => {
                apply_multicam_show_angles(&mut self.prefs, &mut self.state, *val);
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetMasterMute(muted) => {
                self.state.master_muted = *muted;
                let effective = if self.state.master_muted { 0.0 } else { self.state.master_volume };
                let _ = self.cmd_tx.send(AppCommand::SetMasterVolume(effective));
                true
            }
            AppCommand::SetHearDeliveryLoudness(on) => {
                self.prefs.playback.hear_delivery_loudness = *on;
                self.prefs_dirty = true;
                true
            }
            AppCommand::SetMasterVolume(v) => {
                self.state.master_volume = *v;
                let effective = if self.state.master_muted { 0.0 } else { *v };
                let _ = self.cmd_tx.send(AppCommand::SetMasterVolume(effective));
                true
            }
            AppCommand::SetPlayheadFromTimecode { input } => {
                if let Some(tc) = crate::panels::transport::parse_timecode_input_pub(input, self.state.timecode_display()) {
                    self.state.awaiting_seek_frame = true;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(tc));
                }
                true
            }
            AppCommand::SetRippleEnabled(enabled) => {
                self.state.ripple_enabled = *enabled;
                true
            }
            AppCommand::SetAppendBringsAudio(enabled) => {
                self.state.append_brings_audio = *enabled;
                self.state.toast.show(if *enabled {
                    "Appending brings audio"
                } else {
                    "Appending video only"
                });
                true
            }
            AppCommand::SetViewerMode(mode) => {
                self.state.viewer_mode = *mode;
                true
            }
            AppCommand::SetActiveTool(tool) => {
                self.state.active_tool = *tool;
                // Sync layout tool
                self.layout.active_tool = match tool {
                    Tool::Select => crate::layout_state::EditTool::Select,
                    Tool::Blade => crate::layout_state::EditTool::Blade,
                    Tool::Range => crate::layout_state::EditTool::Range,
                    Tool::Trim => crate::layout_state::EditTool::Trim,
                    Tool::Text => crate::layout_state::EditTool::Text,
                    Tool::Mask => crate::layout_state::EditTool::Mask,
                };
                // Clear range drag state on tool change
                self.state.range_drag = None;
                self.state.ruler_press_origin = None;
                true
            }
            AppCommand::ToggleLoopPlayback => {
                self.state.loop_playback_enabled = !self.state.loop_playback_enabled;
                self.prefs.playback.loop_playback = self.state.loop_playback_enabled;
                self.prefs.save().ok();
                let msg = if self.state.loop_playback_enabled { "Loop On" } else { "Loop Off" };
                self.state.toast.show(msg);
                // Forward to engine as SetLoopPlayback
                let _ = self.cmd_tx.send(AppCommand::SetLoopPlayback(self.state.loop_playback_enabled));
                true
            }
            AppCommand::ToggleAudioSkim => {
                self.state.audio_skimming_enabled = !self.state.audio_skimming_enabled;
                self.prefs.playback.skim_audio = self.state.audio_skimming_enabled;
                self.prefs_dirty = true;
                // Tear down any in-flight skim audio session when
                // disabling so the user gets immediate silence; flushing
                // the cpal consumer drops queued samples mid-buffer.
                if !self.state.audio_skimming_enabled {
                    if let Some(ref session) = self.state.skim_audio_session {
                        session.stop();
                    }
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.skim_audio_session = None;
                    self.state.skim_speed_px_per_sec = 0.0;
                }
                let msg = if self.state.audio_skimming_enabled {
                    "Audio Skimming On"
                } else {
                    "Audio Skimming Off (video only)"
                };
                self.state.toast.show(msg);
                true
            }
            AppCommand::SetLoopPlayback(enabled) => {
                self.state.loop_playback_enabled = *enabled;
                // Fall through to forward to engine
                let _ = self.cmd_tx.send(cmd.clone());
                true
            }
            AppCommand::ToggleFullscreen => {
                self.state.fullscreen_viewer = !self.state.fullscreen_viewer;
                self.state.fullscreen_changed = true;
                true
            }
            // ── Audio recording (#60) ──
            AppCommand::ArmTrack { track_id, armed } => {
                if let Some(seq) = self.state.sequences.get_mut(self.state.active_sequence_idx) {
                    if *armed {
                        // Exclusive arm — MVP only records one track at a time.
                        // Multi-track arming is a tier-3 extension.
                        for t in seq.tracks.iter_mut() {
                            t.record_armed = t.id == *track_id;
                        }
                    } else if let Some(t) = seq.tracks.iter_mut().find(|t| t.id == *track_id) {
                        t.record_armed = false;
                    }
                }
                true
            }
            AppCommand::SetAudioAutomationLaneParam { track_idx, param, track } => {
                if let Some(seq) = self.state.sequences.get_mut(self.state.active_sequence_idx) {
                    if let Some(t) = seq.tracks.get_mut(*track_idx) {
                        // Only writes when the lane is already open —
                        // the picker UI is only reachable from inside
                        // the lane, so a write while collapsed would
                        // be a logic bug rather than a no-op fix.
                        if t.automation_lane_visible.is_some() {
                            t.automation_lane_visible = Some(*param);
                            t.automation_lane_track = *track && *param != klipp_core::AudioParam::Gain;
                            self.state.dirty = true;
                        }
                    }
                }
                true
            }
            AppCommand::ToggleAudioAutomationLane { track_idx } => {
                let active_param = match self.state.rubber_band_param {
                    crate::state::RubberBandParam::ClipParam(p) => p,
                    crate::state::RubberBandParam::EffectParam { .. } => {
                        // FX-param rubber band on the pill picker doesn't
                        // map onto a clip-level lane. Fall back to Volume so
                        // the toggle still does something useful.
                        klipp_core::AudioParam::Volume
                    }
                };
                if let Some(seq) = self.state.sequences.get_mut(self.state.active_sequence_idx) {
                    if let Some(t) = seq.tracks.get_mut(*track_idx) {
                        // Opened again, the lane shows the clips' keys of
                        // the active param, as it did before track keys
                        // (#2306).
                        t.automation_lane_track = false;
                        t.automation_lane_visible = match t.automation_lane_visible {
                            None => Some(active_param),
                            Some(_) => None,
                        };
                        self.state.dirty = true;
                    }
                }
                true
            }
            AppCommand::StartRecording => {
                self.start_recording();
                true
            }
            AppCommand::StopRecording => {
                self.stop_recording();
                true
            }
            AppCommand::OpenAiModelsDialog => {
                self.state.show_ai_models_dialog = true;
                true
            }
            AppCommand::OpenAudioPluginsDialog => {
                self.state.show_audio_plugins_dialog = true;
                // Reset the in-process "last scanned" stamp so the panel
                // re-stamps itself on first frame (the discovery state
                // shown is whatever's already in state.ladspa_plugins).
                self.state.audio_plugins_dialog.last_scanned_secs = None;
                true
            }
            AppCommand::RescanAudioPlugins => {
                // Drop the on-disk cache so discover_ladspa_plugins_cached
                // is forced to do a fresh scan — without this the function
                // would happily return the cached result and our "Rescan"
                // button would do nothing visible.
                let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
                if let Some(ref base) = cache_dir {
                    let cache_file = base.join("ladspa_registry.json");
                    if cache_file.exists() {
                        if let Err(e) = std::fs::remove_file(&cache_file) {
                            tracing::warn!(
                                "LADSPA rescan: failed to delete cache file {}: {e}",
                                cache_file.display()
                            );
                        }
                    }
                }
                let t0 = std::time::Instant::now();
                self.state.ladspa_plugins =
                    klipp_audio::ladspa::discover_ladspa_plugins_cached(cache_dir.as_deref());
                // Replace the LADSPA section of the effect registry with
                // the fresh result. The simplest correct way: remove all
                // existing LADSPA entries, then re-merge.
                self.state.effect_registry.effects.retain(|e| {
                    !matches!(
                        e.source,
                        crate::browser::effect_registry::EffectSource::Ladspa(_)
                    )
                });
                let enabled: Vec<klipp_audio::ladspa::LadspaPluginInfo> = self
                    .state
                    .ladspa_plugins
                    .iter()
                    .filter(|p| !self.state.disabled_audio_plugins.contains(&p.unique_id))
                    .cloned()
                    .collect();
                self.state.effect_registry.merge_ladspa_plugins(&enabled);
                self.state.audio_plugins_dialog.last_scanned_secs = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                );
                let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                tracing::info!(
                    "LADSPA rescan: {} plugin(s) in {:.1} ms",
                    self.state.ladspa_plugins.len(),
                    elapsed_ms
                );
                self.state.toast.show(format!(
                    "Rescanned: {} LADSPA plugin(s) in {:.0} ms",
                    self.state.ladspa_plugins.len(),
                    elapsed_ms
                ));
                true
            }
            AppCommand::OpenSystemProbe => {
                self.state.show_system_probe = true;
                true
            }
            AppCommand::OpenProjectDoctor => {
                self.state.show_project_doctor = true;
                true
            }
            AppCommand::ClearAllAudioPluginsDisabled => {
                let cleared = self.state.disabled_audio_plugins.len();
                if cleared == 0 {
                    return true;
                }
                self.state.disabled_audio_plugins.clear();
                self.state.effect_registry.effects.retain(|e| {
                    !matches!(
                        e.source,
                        crate::browser::effect_registry::EffectSource::Ladspa(_)
                    )
                });
                self.state
                    .effect_registry
                    .merge_ladspa_plugins(&self.state.ladspa_plugins);
                crate::audio_plugins_disabled::save(&self.state.disabled_audio_plugins);
                self.state.toast.show(format!(
                    "Re-enabled {cleared} plugin{}",
                    if cleared == 1 { "" } else { "s" }
                ));
                true
            }
            AppCommand::ToggleAudioPluginDisabled { unique_id } => {
                let now_disabled = if self.state.disabled_audio_plugins.contains(unique_id) {
                    self.state.disabled_audio_plugins.remove(unique_id);
                    false
                } else {
                    self.state.disabled_audio_plugins.insert(*unique_id);
                    true
                };
                // Rebuild the LADSPA section of the effect registry so
                // the audio FX browser, inspector picker, and any other
                // EffectSource::Ladspa consumer see the new filter
                // applied immediately. No rescan needed; we're filtering
                // the in-memory `state.ladspa_plugins` list which
                // already has every discovered plugin.
                self.state.effect_registry.effects.retain(|e| {
                    !matches!(
                        e.source,
                        crate::browser::effect_registry::EffectSource::Ladspa(_)
                    )
                });
                let enabled: Vec<klipp_audio::ladspa::LadspaPluginInfo> = self
                    .state
                    .ladspa_plugins
                    .iter()
                    .filter(|p| !self.state.disabled_audio_plugins.contains(&p.unique_id))
                    .cloned()
                    .collect();
                self.state.effect_registry.merge_ladspa_plugins(&enabled);
                crate::audio_plugins_disabled::save(&self.state.disabled_audio_plugins);

                let label = self
                    .state
                    .ladspa_plugins
                    .iter()
                    .find(|p| p.unique_id == *unique_id)
                    .map(|p| {
                        if p.name.is_empty() {
                            p.label.clone()
                        } else {
                            p.name.clone()
                        }
                    })
                    .unwrap_or_else(|| format!("plugin {unique_id}"));
                self.state.toast.show(format!(
                    "{} {}",
                    if now_disabled { "Disabled" } else { "Enabled" },
                    label
                ));
                true
            }
            AppCommand::ShowAiToolsPanel { task } => {
                self.state.ai_tools_state.panel_visible = true;
                self.state.ai_tools_state.active_task = Some(ai_tools_task_from_tag(*task));
                let task_local = ai_tools_task_from_tag(*task);
                self.state.ai_tools_state.selected_model_id = self
                    .state
                    .ai_tools_registry
                    .models_for(task_local)
                    .first()
                    .map(|m| m.id.clone());
                true
            }
            AppCommand::SetAiToolsTask(tag) => {
                let task_local = ai_tools_task_from_tag(*tag);
                self.state.ai_tools_state.active_task = Some(task_local);
                self.state.ai_tools_state.selected_model_id = self
                    .state
                    .ai_tools_registry
                    .models_for(task_local)
                    .first()
                    .map(|m| m.id.clone());
                true
            }
            AppCommand::SelectAiToolsModel { model_id } => {
                if self
                    .state
                    .ai_tools_registry
                    .info(model_id)
                    .is_some()
                {
                    self.state.ai_tools_state.selected_model_id = Some(model_id.clone());
                }
                true
            }
            AppCommand::RunAiToolsInference => {
                // Frame-grab + worker dispatch land in a follow-up; for
                // now we only mark the panel intent so the dispatcher
                // sees consistent state.
                self.state.ai_tools_state.pending_run = true;
                true
            }
            AppCommand::CancelAiToolsInference => {
                self.state.ai_tools_state.inference.cancel();
                self.state.ai_tools_state.pending_cancel = false;
                true
            }
            _ => false,
        }
    }
}

impl KlippApp {
    /// Play In to Out stops at the Out (#921): checked every frame, it
    /// pauses on the frame the playhead reaches it and puts the playhead
    /// on it. Pausing, stopping or seeking ends it, as does playback that
    /// never started.
    pub(super) fn stop_at_out(&mut self) {
        let Some(run) = self.state.play_to_out else { return };
        let playing = self.state.playback == PlaybackState::Playing;
        let at = self.state.playhead;
        let from_the_in = at >= run.from && at < run.out;
        match crate::navigation::play_to_out_step(run.started, playing, run.asked.elapsed(), from_the_in, at >= run.out) {
            crate::navigation::PlayToOutStep::Wait => {}
            crate::navigation::PlayToOutStep::Started => {
                self.state.play_to_out = Some(crate::state::PlayToOut { started: true, ..run });
            }
            crate::navigation::PlayToOutStep::Done => self.state.play_to_out = None,
            crate::navigation::PlayToOutStep::StopAtOut if run.looping => {
                // Round again from the start of the loop.
                self.state.play_to_out = Some(crate::state::PlayToOut { asked: std::time::Instant::now(), started: false, ..run });
                self.state.playhead = run.from;
                self.send_commands(vec![AppCommand::SeekTo(run.from)]);
            }
            crate::navigation::PlayToOutStep::StopAtOut => {
                self.state.play_to_out = None;
                self.send_commands(vec![AppCommand::Pause, AppCommand::SeekTo(run.out)]);
            }
        }
    }
}

