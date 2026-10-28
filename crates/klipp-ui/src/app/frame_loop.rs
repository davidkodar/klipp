//! The frame loop: `update` runs these phases in order, every frame (#826).

use super::*;

impl KlippApp {
    /// Housekeeping at the top of a frame: a workspace switch settled, the
    /// clipboard, the deferred texture drops, the GPU viewer texture, AI
    /// tools, interaction and the heartbeat, system audio, dropped files and
    /// the popup state.
    pub(super) fn begin_frame(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Settle any workspace switch before the layout is computed, so
        // the panel changes it implies land in the same frame the new
        // workspace does rather than a frame behind it.
        let colour_visible = if self.state.active_workspace != self.last_workspace {
            self.state.active_workspace == crate::state::Workspace::Color
        } else {
            self.state.show_color_panel
        };
        let side_nodes = (self.state.graph_dock_open || (colour_visible && self.state.show_node_strip))
            && self.layout.node_graph_placement == crate::layout_state::NodeGraphPlacement::Right;
        // Release the nested node borrow before the workspace restores its
        // browser, and take it only after the workspace has settled.
        self.node_browser_borrow.leave_if_inactive(side_nodes, &mut self.state.show_browser_panel);
        self.reconcile_workspace();
        self.state.repair_graph_dock();
        self.refresh_node_thumbnails();
        self.node_browser_borrow.enter_if_active(side_nodes, &mut self.state.show_browser_panel);
        if let Some(text) = self.state.system_clipboard_text.take() {
            ctx.copy_text(text);
        }
        if std::mem::take(&mut self.preferences_panel.show_updates) {
            self.state.show_preferences = false;
            self.updates.check_in_about(&mut self.state.show_about_dialog);
        }
        let project_open = self.state.project_path.is_some();
        self.updates.show(ctx, &mut self.prefs.general, &mut self.prefs_dirty, project_open, &mut self.state.show_about_dialog);
        self.state.update_notice_open = self.updates.holds_keys();
        self.tick_unsaved(ctx);
        self.state.seed_recorded_tracks();

        // Rotate the deferred-drop texture ring. Advance the slot and
        // clear the one we've just moved into — it holds handles
        // stashed ~2 frames ago. Two full frames of deferral covers
        // every submit + present cycle a texture could still be
        // referenced by. See the field's docs in state.rs.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::TEXTURE_DROP_RING);
        let next_slot = (self.state.deferred_texture_drops_slot + 1) % 3;
        self.state.deferred_texture_drops[next_slot].clear();
        self.state.deferred_texture_drops_slot = next_slot;

        // Register the engine's `FrameBuffer.gpu_output` texture
        // with eframe-wgpu's renderer when it changes. No-op when
        // the engine hasn't produced a GPU output (env gate off, no
        // shared device, glow fallback) — the legacy CPU path keeps
        // working. The viewer render code picks up the new
        // `viewer.gpu_texture_id` when present.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::REGISTER_GPU_TEXTURE);
        self.refresh_gpu_viewer_texture(frame);

        // AI Tools dispatch tick — drains pending_run / pending_cancel
        // and polls the worker channel.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::AI_TOOLS_TICK);
        self.tick_ai_tools(ctx);
        self.tick_track_subject(ctx);

        // Bump the foreground-activity signal whenever the user
        // *interacts* — clicks, keys, scrolls, drags. Plain pointer
        // motion intentionally does NOT count: a user reading the UI
        // with the mouse drifting around shouldn't keep the skim-
        // transcode / filmstrip / audio-conform jobs paused forever.
        let had_interaction = ctx.input(|i| {
            // Held button = drag in flight → user is busy.
            if i.pointer.any_down() { return true; }
            // Discrete events that count as "interaction". Explicitly
            // exclude PointerMoved + MouseMoved + WindowFocused —
            // those fire constantly during normal use without
            // signalling intent.
            i.events.iter().any(|e| {
                use egui::Event as E;
                !matches!(
                    e,
                    E::PointerMoved(_)
                        | E::MouseMoved(_)
                        | E::WindowFocused(_)
                        | E::PointerGone
                )
            })
        });
        if had_interaction {
            self.foreground_activity.touch_input();
        }

        // Main-thread heartbeat. Logs every 2 s with UI fps + state so we
        // can see the UI thread redraw rate in real time (separate from
        // the engine tick rate). An idle paused UI should be near 0 fps.
        self.frame_count = self.frame_count.wrapping_add(1);
        let events_this_frame = ctx.input(|i| i.events.len());
        self.hb_input_events += events_this_frame as u64;
        self.hb_max_batch = self.hb_max_batch.max(events_this_frame);
        let since_last_frame = self.last_frame_at.elapsed();
        self.last_frame_at = std::time::Instant::now();
        if events_this_frame >= 24 && since_last_frame >= std::time::Duration::from_secs(1) {
            // Input piled up while no frame was painted: the user was
            // acting and the UI was not answering.
            tracing::warn!(
                "INPUT_BURST {events_this_frame} input events in one frame after {:.1}s without a frame",
                since_last_frame.as_secs_f64(),
            );
        }
        if self.last_heartbeat.elapsed() >= std::time::Duration::from_secs(2) {
            let elapsed = self.last_heartbeat.elapsed().as_secs_f64();
            let frames_in_interval =
                self.frame_count.saturating_sub(self.last_heartbeat_frame);
            let ui_fps = frames_in_interval as f64 / elapsed.max(0.001);
            tracing::info!(
                "UI_HEARTBEAT fps={:.1} frames={} events={} max_batch={} max_gap={:.0}ms max_update={:.0}ms focused={} state={:?} viewer_mode={:?} elapsed={:.2}s show_scopes={} show_color={}",
                ui_fps,
                frames_in_interval,
                self.hb_input_events,
                self.hb_max_batch,
                self.hb_max_gap.as_secs_f64() * 1000.0,
                self.hb_max_update.as_secs_f64() * 1000.0,
                ctx.input(|i| i.focused),
                self.state.playback,
                self.state.viewer_mode,
                elapsed,
                self.state.show_scopes,
                self.state.show_color_panel,
            );
            self.last_heartbeat = std::time::Instant::now();
            self.last_heartbeat_frame = self.frame_count;
            self.hb_max_gap = std::time::Duration::ZERO;
            self.hb_max_update = std::time::Duration::ZERO;
            self.hb_input_events = 0;
            self.hb_max_batch = 0;
        }

        // Consume any menu / toolbar request to toggle the perf
        // recording. The direct Ctrl+Alt+P keybinding calls
        // `toggle_perf_recording` inline during shortcut handling
        // for snappier UX; this pending-flag path covers everything
        // else that doesn't have easy access to `&mut self`.
        if std::mem::take(&mut self.state.pending_perf_recording_toggle) {
            self.toggle_perf_recording();
        }

        // CONTRAST RULE: Ensure selection bg stays dark each frame.
        // egui popup windows can reset visuals; re-apply selection colors to be safe.
        // Only mutate when the value drifted — avoids bumping the style dirty flag
        // on the 99% of frames where no popup reset happened.
        {
            let target = egui::Color32::from_rgb(0x38, 0x2c, 0x14);
            let needs_fix = ctx.style().visuals.selection.bg_fill != target;
            if needs_fix {
                ctx.style_mut(|style| {
                    style.visuals.selection.bg_fill = target;
                });
            }
        }

        // ── Poll system audio mute state (Linux/PipeWire) ──
        // Check every ~2 seconds via `wpctl` so we can warn the user if
        // their system audio is muted. Lightweight: wpctl completes in
        // <1ms and we only run it every 120 frames at 60fps.
        if self.state.system_audio_last_check.elapsed() > std::time::Duration::from_secs(2) {
            self.state.system_audio_last_check = std::time::Instant::now();
            let was = self.state.system_audio_muted;
            self.state.system_audio_muted = check_system_audio_muted();

            if self.state.system_audio_muted {
                let interval = self.state.mute_reminder_interval_secs;
                let since_last = self.state.system_audio_last_reminder.elapsed();
                // Show toast on first detection OR every interval seconds.
                // interval == 0 means "show only once" (on state change).
                let should_toast = if !was {
                    true // first detection
                } else if interval > 0 {
                    since_last >= std::time::Duration::from_secs(interval as u64)
                } else {
                    false
                };
                if should_toast {
                    self.state.system_audio_last_reminder = std::time::Instant::now();
                    self.state.toast.show(
                        "\u{26a0} Audio muted by system \u{2014} click the speaker icon in your taskbar to unmute"
                            .to_string(),
                    );
                }
            }
        }

        // ── Handle OS-level file drag-and-drop ──
        let dropped_files: Vec<std::path::PathBuf> = ctx.input(|i| {
            if i.raw.dropped_files.is_empty() {
                return Vec::new();
            }
            i.raw.dropped_files.iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped_files.is_empty() {
            let media_exts = klipp_core::media_extensions::all_media_extensions();
            for path in dropped_files {
                let ext = path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if media_exts.contains(&ext.as_str()) {
                    let media_id = klipp_core::MediaId::new();
                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path: path.clone(), media_id });
                    tracing::info!("Dropped file: {:?}", path);
                } else if crate::cli::is_project(&path) {
                    // Opened through the UI, as a click would (#395): sent
                    // to the engine, OpenProject was dropped.
                    self.startup_open = Some(crate::cli::StartupOpen::project(path));
                } else if let Some(why) = klipp_core::media_extensions::camera_raw_video_refusal(&ext) {
                    // Camera RAW video is a non-goal (#85): say so and what
                    // to do, rather than ignore the drop.
                    self.state.toast.show(why);
                } else if klipp_project::import_report::detect(&path).is_ok() {
                    // A timeline from another editor (#2083): opened as a
                    // new project, as from the File menu.
                    self.send_commands(vec![AppCommand::OpenFromAnotherEditor { path, request_id: uuid::Uuid::nil() }]);
                }
            }
        }

        // Also show visual feedback when files are being hovered over the window
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            // Paint a subtle overlay to indicate drop target
            let screen = ctx.screen_rect();
            egui::Area::new(egui::Id::new("drop_overlay"))
                .fixed_pos(screen.min)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, self.theme.accent_subtle);
                    ui.painter().rect_stroke(rect.shrink(4.0), 4.0, egui::Stroke::new(2.0_f32, self.theme.accent_glow));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Drop media files to import",
                        egui::FontId::proportional(16.0),
                        self.theme.text_accent,
                    );
                });
        }

        // Check if any popup/menu/combobox/foreground layer is active (blocks background interaction)
        let popup_open = ctx.memory(|m| m.any_popup_open());
        // Also check if pointer is over a foreground layer (menus, context menus)
        let pointer_over_foreground = ctx.input(|i| i.pointer.hover_pos())
            .and_then(|pos| ctx.layer_id_at(pos))
            .is_some_and(|layer| layer.order == egui::Order::Foreground || layer.order == egui::Order::Tooltip);
        self.state.modal_or_popup_open = popup_open || pointer_over_foreground
            || self.scope_window_runtime.pointer_over;
    }

    /// What arrived since the last frame: the playhead moved on, engine
    /// events drained, the scopes synced, thumbnails, proxies and renders
    /// taken in, and the frames uploaded to egui.
    pub(super) fn take_in_results(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // A write through a picture-truth clip's fields refused (it would
        // have taken away a mask or a mix only the clip's picture keeps):
        // the clip was left as it was, and this says why.
        if let Some(why) = klipp_timeline::take_picture_refusal() {
            self.state.toast.show(why);
        }
        // Smooth playhead interpolation BEFORE draining events
        if self.state.playback == PlaybackState::Playing {
            let wall_elapsed = self.state.playhead_anchor_wall.elapsed().as_secs_f64();
            let anchor_secs = self.state.playhead_anchor.to_secs_f64();
            // Account for clip speed in smooth interpolation — the master clock
            // runs at clip_speed, so the playhead on the timeline advances at
            // (clip_speed / clip_speed) = 1.0 timeline-seconds per wall-second,
            // BUT the playhead moves through timeline space, not source space.
            // The clip occupies (source_duration / clip_speed) timeline seconds,
            // so the playhead advances at playback_speed in timeline space.
            let speed = self.state.playback_speed;
            let interpolated_secs = anchor_secs + wall_elapsed * speed;
            let dur_secs = self.state.sequence_duration().to_secs_f64();
            // When looping, clamp to out_point to prevent one-frame overshoot
            let max_secs = if self.state.loop_playback_enabled {
                self.state.sequence().out_point
                    .map(|t| t.to_secs_f64())
                    .unwrap_or(dur_secs)
            } else {
                dur_secs
            };
            let clamped = interpolated_secs.clamp(0.0, max_secs);

            self.state.playhead_smooth_secs = clamped;
            self.state.playhead = Timecode::from_secs_f64(clamped, self.state.fps.rate());
        }

        // 1. Drain events from engine
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::DRAIN_EVENTS);
        self.drain_events(ctx);
        self.stop_at_out();
        self.tick_recording();
        self.tick_automation();

        // Sync scope mode to engine — only when it actually changed.
        // Previously this sent unconditionally every UI frame (~120
        // commands/sec), contributing to engine command-channel load
        // for no benefit.
        {
            let mode_byte = match self.state.scope_mode {
                crate::state::ScopeMode::Waveform => 0u8,
                crate::state::ScopeMode::Parade => 1,
                crate::state::ScopeMode::Vectorscope => 2,
                crate::state::ScopeMode::Histogram => 3,
            };
            if self.last_sent_scope_mode != Some(mode_byte) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: mode_byte });
                self.last_sent_scope_mode = Some(mode_byte);
            }

            // Publish the union of all video scope modes the UI needs
            // rendered per tick: dock mode + every floating-window pane
            // whose `ScopeType` maps to a video scope. Audio scopes don't
            // go through the engine at all. CIE chromaticity is drawn in
            // the UI thread but still asks the engine for the linear
            // samples it measures, under its own byte. Re-sent only on
            // change so the engine command channel doesn't churn.
            let mut active: std::collections::BTreeSet<u8> =
                std::collections::BTreeSet::new();
            active.insert(mode_byte);
            // Every pane that is actually on screen asks for its mode:
            // the floating window's when it is open, and the dock's when
            // it is showing a grid. A pane whose mode is missing from
            // this set has no texture to lift and draws blank.
            let want_pane_modes = |panes: &[crate::state::PaneConfig],
                                       active: &mut std::collections::BTreeSet<u8>| {
                for pane in panes {
                    if let Some(sm) = pane.scope_type.to_scope_mode() {
                        let pane_byte = match sm {
                            crate::state::ScopeMode::Waveform => 0u8,
                            crate::state::ScopeMode::Parade => 1,
                            crate::state::ScopeMode::Vectorscope => 2,
                            crate::state::ScopeMode::Histogram => 3,
                        };
                        active.insert(pane_byte);
                    } else if pane.scope_type == crate::state::ScopeType::CIEChromaticity {
                        // Drawn in the UI, but it still needs the engine to
                        // publish the linear working-space samples it
                        // measures — the picture, not the view of it.
                        active.insert(klipp_engine::scopes::CIE_SAMPLES_MODE);
                    }
                }
            };
            if self.state.scope_window.open {
                want_pane_modes(&self.state.scope_window.panes, &mut active);
            }
            if self.state.scopes_docked()
                && self.state.scope_dock.layout != crate::state::ScopeLayout::OnePane
            {
                want_pane_modes(&self.state.scope_dock.panes, &mut active);
            }
            if self.last_sent_active_scope_modes.as_ref() != Some(&active) {
                let _ = self.cmd_tx.send(AppCommand::SetActiveScopeModes {
                    modes: active.clone(),
                });
                self.last_sent_active_scope_modes = Some(active);
            }

            // The physical size each scope mode was drawn at last frame —
            // by the dock and by every floating pane, the larger side of
            // each where two show the same mode — so the engine draws it
            // at that size. Taken, so this frame's drawing notes it afresh
            // and a closed pane stops asking.
            let mut sizes: std::collections::BTreeMap<u8, (u32, u32)> = std::collections::BTreeMap::new();
            let shown = std::iter::once(self.state.scope_state.shown_size.take())
                .chain(self.scope_window_runtime.shown_sizes.iter_mut().map(Option::take))
                .chain(self.scope_dock_runtime.shown_sizes.iter_mut().map(Option::take));
            for (mode, (w, h)) in shown.flatten() {
                let size = sizes.entry(mode).or_insert((0, 0));
                *size = (size.0.max(w), size.1.max(h));
            }
            if self.last_sent_scope_sizes.as_ref() != Some(&sizes) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeSizes { sizes: sizes.clone() });
                self.last_sent_scope_sizes = Some(sizes);
            }

            // Publish FFmpeg-scope settings. Built from the existing
            // global AppState toggles (legacy popover) until Phase 5
            // adds dedicated per-mode UI. Same filter / channel / intensity
            // applies to BOTH waveform and parade in this back-compat
            // shape.
            //
            // FFmpeg waveform filter is read from AppState. Lowpass is the
            // 10-bit-clean default; other filter modes fall back to 8-bit
            // before the waveform filter to dodge a libavfilter 9.x SIGSEGV
            // on gbrp10le input (see SCOPE-NEXT-LIBAV-10BIT). The popover
            // dropdown lets the user pick per visual style.
            let legacy_channels = klipp_core::ChannelMask {
                r: self.state.scope_show_r,
                g: self.state.scope_show_g,
                b: self.state.scope_show_b,
            };
            let wp = klipp_core::WaveformParadeSettings {
                intensity: self.state.scope_options.waveform_intensity,
                channels: legacy_channels,
                envelope: self.state.scope_options.waveform_envelope,
            };
            let scope_settings = klipp_core::ScopeSettings {
                waveform: wp,
                parade: wp,
                vectorscope: klipp_core::VectorscopeSettings {
                    colour: self.state.scope_options.vectorscope_colour,
                    luma_lo: self.state.scope_options.vectorscope_luma_lo,
                    luma_hi: self.state.scope_options.vectorscope_luma_hi,
                    zoom: self.state.scope_options.vectorscope_zoom,
                },
                histogram: klipp_core::HistogramSettings {
                    display_mode: self.state.scope_options.histogram_display,
                    levels: self.state.scope_options.histogram_levels,
                    fill: self.state.scope_options.histogram_fill,
                },
                quality: self.state.scope_quality,
                luma_only: self.state.scope_options.waveform_luma_only,
            };
            if self.last_sent_scope_settings != Some(scope_settings) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeSettings {
                    settings: scope_settings,
                });
                self.last_sent_scope_settings = Some(scope_settings);
            }

            // Bloom toggle — separate post-pass concern (not an FFmpeg
            // spec knob), so a separate command + sentinel pair.
            let bloom_enabled = self.state.scope_bloom;
            if self.last_sent_scope_bloom != Some(bloom_enabled) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeBloom {
                    enabled: bloom_enabled,
                });
                self.last_sent_scope_bloom = Some(bloom_enabled);
            }
            // After the project's viewing LUT, or before it (#466).
            let after = self.state.project_viewing_lut.clone().filter(|_| self.state.project_scopes_after_viewing_lut);
            if self.last_sent_scope_lut.as_ref().map(|(p, _)| p) != Some(&after) {
                let key = self.last_sent_scope_lut.as_ref().map_or(1, |(_, k)| k + 1);
                let lut = after.as_deref().and_then(|path| match klipp_color::lut::load_lut(path) {
                    Ok(l) => Some(klipp_core::ScopeLut { key, size: l.size as u32, table: std::sync::Arc::new(l.data) }),
                    Err(e) => {
                        tracing::warn!("viewing LUT for the scopes {}: {e}", path.display());
                        None
                    }
                });
                let _ = self.cmd_tx.send(AppCommand::SetScopeLut { lut });
                self.last_sent_scope_lut = Some((after, key));
            }
            let low_pass = self.state.scope_low_pass;
            if self.last_sent_scope_low_pass != Some(low_pass) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeLowPass { enabled: low_pass });
                self.last_sent_scope_low_pass = Some(low_pass);
            }
        }

        // Sync scope-panel visibility to the engine — same change-
        // tracking pattern as the mode sync above. Catches every
        // mutation site (keyboard toggle, button click, layout
        // restore) without per-site plumbing because we observe the
        // current state value once per UI frame.
        {
            let visible = self.state.scopes_shown();
            if self.last_sent_scopes_visible != Some(visible) {
                let _ = self.cmd_tx.send(AppCommand::SetScopesVisible(visible));
                self.last_sent_scopes_visible = Some(visible);
            }
        }

        self.apply_ui_scale(ctx);
        self.serve_window_capture(ctx);
        self.drain_thumbnails();
        self.drain_source_frame(ctx);
        // Keep repainting while filmstrip is generating so timeline updates progressively
        if self.state.filmstrip_generating || !self.filmstrip_cache_loads.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // And while scenes are being found: their progress moves in the
        // menu, and the result is taken in, and a clip split, on the frame
        // after it arrives, which with nothing else moving never came
        // (#616).
        if !self.state.scene_detect_in_progress.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Likewise a track, a window's or a surface's: with the window
        // idle its result sat unapplied until the pointer moved.
        if !self.state.tracking_in_progress.is_empty() || self.state.surface_snaps_pending > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.drain_audio_conforms();
        self.drain_proxy_events();
        self.measure_stale_normalizes();
        self.measure_unmeasured_water();
        // If a skim decode just finished and there's a queued request,
        // dispatch it immediately. After `drain_proxy_events`, because that
        // is where `BrowserSkimFrameReady` clears `skim_decode_in_flight`:
        // run before it and the queued request could only ever be seen as
        // still blocked, so every one of them went out a UI frame late and
        // the picture lagged the cursor by that frame for the whole drag.
        if !self.state.skim_decode_in_flight {
            if let Some((mid, time, target)) = self.state.skim_queued_request.take() {
                self.request_skim_frame(mid, time, target);
            }
        }
        self.drain_proxy_progress();
        self.poll_render_queue();
        self.ensure_analyses(ctx);
        // Show filmstrip thumbnail preview only while waiting for a seek result
        if self.state.awaiting_seek_frame && !self.state.showing_thumbnail_preview {
            // A skim frame that has landed but not yet been uploaded sets
            // `pending_ready`, not `dirty` — `dirty` is only set by the
            // `swap_pending()` inside `upload_frame_to_egui`, which runs
            // after this. Checking `dirty` alone let a thumbnail paint over
            // a good frame that was already waiting.
            let waiting = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.dirty || fb.pending_ready
            };
            if !waiting {
                self.show_filmstrip_preview(ctx);
            }
        }
        self.upload_frame_to_egui(ctx);
        self.refresh_viewer_lut(frame);
        self.upload_source_frame_to_egui(ctx);
        self.upload_title_overlay(ctx);
        self.recompute_char_positions_if_needed();
        self.poll_effect_thumbnails(ctx);
    }

    /// State that follows from the frame so far: media usage, the multicam
    /// viewer and its grid, the window title, preferences saved and the
    /// auto-save.
    pub(super) fn sync_derived_state(&mut self, ctx: &egui::Context) {
        // Rebuild media usage cache if dirty (O(clips) once per edit, then O(1) per render)
        if self.state.media_usage_cache_dirty {
            self.state.media_usage_cache.clear();
            for seq in &self.state.sequences {
                for track in &seq.tracks {
                    for clip in &track.clips {
                        if let Some(mid) = clip.media_id() {
                            *self.state.media_usage_cache.entry(mid).or_insert(0) += 1;
                        }
                    }
                }
            }
            self.state.media_usage_cache_dirty = false;
        }

        // Multicam mode (#1844) shows the group under the playhead, else
        // the opened one, and only while the mode is on: the playhead
        // crossing a multicam clip never turns it on. The program
        // viewer's angle bar follows the playhead whatever the mode.
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let at_playhead = crate::multicam_grid::view_at(seq, self.state.multicam_opened, self.state.playhead);
        let view = crate::multicam_grid::mode_view(seq, self.state.multicam_mode, self.state.multicam_opened, self.state.playhead);
        self.state.multicam_at_playhead = at_playhead.is_some();
        self.state.multicam_viewer_active = view.is_some();
        if let Some((angles, active)) = view.or(at_playhead) {
            self.state.multicam_angles = angles;
            self.state.multicam_active_angle = active;
        }
        // The angle grid, in the Source viewer's place: decoded while it
        // is shown, its tiles let go when it is not.
        // Shown where the Source viewer is: beside Program, or in its
        // place in a single-viewer layout switched to Source.
        // As the layout decides it (`layout::compute_layout`): the Colour
        // and Export workspaces' single viewer is Program whatever the
        // mode says, and decoding every angle for no pixels on screen
        // was waste.
        let source_on_screen = self.state.viewers_side_by_side()
            || (!self.state.single_viewer_is_program() && self.state.viewer_mode == klipp_core::ViewerMode::Source);
        let grid_shown = self.state.multicam_viewer_active && source_on_screen;
        let mut old_tiles = Vec::new();
        if grid_shown {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            if let Some(group) = crate::multicam_grid::group_at(seq, self.state.multicam_opened, self.state.playhead) {
                let runtime = self.multicam_grid.get_or_insert_with(|| crate::multicam_grid::GridRuntime::new(ctx));
                runtime.request(seq, &self.state.media_pool, group, self.state.playhead, self.state.playback == PlaybackState::Playing);
                runtime.drain(ctx, |t| old_tiles.push(t));
                old_tiles.extend(runtime.take_retired());
            }
        } else if let Some(runtime) = self.multicam_grid.as_mut() {
            runtime.clear(|t| old_tiles.push(t));
        }
        for t in old_tiles {
            self.defer_drop_texture(t);
        }

        // Window title with project name and dirty indicator — only
        // send the viewport command when the title actually changed to
        // avoid a per-frame String allocation + IPC round-trip.
        {
            let title = match &self.state.project_path {
                Some(p) => {
                    let name = p.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Untitled".into());
                    let read_only = if self.read_only_project.as_ref() == Some(p) { " (read-only)" } else { "" };
                    if self.state.unsaved {
                        format!("Klipp \u{2014} {name}{read_only} \u{2022}")
                    } else {
                        format!("Klipp \u{2014} {name}{read_only}")
                    }
                }
                None => "Klipp".to_string(),
            };
            if title != self.last_window_title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.last_window_title = title;
            }
        }

        // Apply pending false color palette switch from toolbar quick-switcher
        if let Some(palette) = self.state.pending_false_color_palette.take() {
            self.state.false_color_lut = palette.build_lut();
            self.prefs.viewer.false_color_palette = palette;
            self.prefs_dirty = true;
        }

        // Collapsing or expanding a section is a preference change made
        // by a widget with no access to `self`, so nothing marks the
        // prefs dirty and the mirror below — which only runs when they
        // already are — never sees it. It went unnoticed while every
        // section started open and most are never collapsed; with GRADE
        // starting closed, a colourist who wants it open would have had
        // to re-open it on every launch.
        if self.state.remember_inspector_layout {
            let key = egui::Id::new("klipp_collapsed_sections");
            let differs = ctx.data_mut(|d| {
                let set: &std::collections::HashSet<String> =
                    d.get_temp_mut_or_default(key);
                set.len() != self.prefs.general.collapsed_sections.len()
                    || self
                        .prefs
                        .general
                        .collapsed_sections
                        .iter()
                        .any(|name| !set.contains(name))
            });
            if differs {
                self.prefs_dirty = true;
            }
        }

        // The ease shortcuts as the key menus name them (#2307): once, and
        // again while the preferences, where they are rebound, are open.
        if self.state.show_preferences || !crate::widgets::scrubby_param::has_ease_hints(ctx) {
            crate::widgets::scrubby_param::set_ease_hints(ctx, &self.key_bindings);
        }
        // Debounced preferences save (at most once per second)
        if self.prefs_dirty && self.prefs_last_save.elapsed().as_secs() >= 1 {
            // Sync prefs to state
            self.state.set_history_max_size(self.prefs.editing.undo_limit);
            self.state.audio_fade_overlay_mode = self.prefs.audio.fade_overlay_mode;
            self.state.audio_show_rubber_band = self.prefs.audio.show_rubber_band;
            self.state.audio_show_waveform = self.prefs.audio.show_waveform;
            self.state.audio_anti_click_fade = self.prefs.audio.anti_click_fade;
            self.state.default_fade_secs = self.prefs.audio.default_fade_duration_ms as f32 / 1000.0;
            self.state.default_fade_curve = self.prefs.audio.default_fade_curve;
            self.state.jkl_speed_step = f64::from(self.prefs.playback.jkl_speed_step);
            self.state.track_search_radius = self.prefs.editing.track_search_radius;
            self.state.multicam_default_sync = self.prefs.editing.multicam_sync_method.sync();
            // The timeline's own copy, so a change shows without a restart.
            self.state.timeline_prefs = self.prefs.timeline.clone();
            self.state.audio_show_crossfade_indicators = self.prefs.audio.show_crossfade_indicators;
            self.state.lufs_target = match self.prefs.audio.loudness_standard {
                klipp_core::LoudnessStandard::EbuR128 => -23.0,
                klipp_core::LoudnessStandard::AtscA85 => -24.0,
                klipp_core::LoudnessStandard::Off => -23.0,
            };
            self.state.peak_hold_time_secs = self.prefs.audio.peak_hold_time_secs;
            self.state.tp_oversample = self.prefs.audio.tp_oversample;
            let _ = self.cmd_tx.send(AppCommand::SetAntiClickFade(self.state.audio_anti_click_fade));
            // Sync editing prefs
            if !self.prefs.editing.playhead_follows_playback {
                self.state.playhead_follow = crate::state::PlayheadFollow::Off;
            }
            // T-PREFS-LIVE-SYNC (2026-05-07): the AppCommand handlers
            // for SetMagneticTimeline + SetMulticamShowAngles already
            // mirror these fields immediately on every prefs-panel
            // change, so these two lines are now redundant for
            // in-app edits. Kept as a safety net for any future
            // out-of-band prefs.json edit (e.g. the user hand-edits
            // the file while Klipp is running and triggers a save
            // through some other path). If that case is ever
            // formally unsupported, this whole block can drop both
            // lines without behavioural change.
            self.state.magnetic_timeline_enabled = self.prefs.editing.magnetic_timeline;
            self.state.multicam_show_angles = self.prefs.editing.multicam_show_angles;
            // Sync engine statics
            klipp_engine::video_decoder::set_hardware_decode(self.prefs.system.hardware_decode);
            klipp_engine::video_decoder::set_optimised_playback_mode(
                self.prefs.playback.optimised_playback.to_engine_mode(),
            );
            klipp_engine::video_decoder::set_playback_quality_divisor(crate::prefs::playback_quality_divisor(
                self.prefs.playback.playback_quality,
            ));
            let _ = self.cmd_tx.send(AppCommand::SetLutTetrahedral(
                self.prefs.color.lut_interpolation == crate::prefs::LutInterpolation::Tetrahedral,
            ));
            let _ = self.cmd_tx.send(AppCommand::SetSdrTransfer(self.prefs.color.sdr_display_transfer));
            let _ = self.cmd_tx.send(AppCommand::SetRenderCacheLimitGb(self.prefs.media_cache.max_cache_size_gb));
            // Push the background-performance pref into the live
            // scheduler so user changes take effect immediately
            // (without waiting for the next app launch).
            self.job_scheduler.set_policy(
                self.prefs.playback.background_performance.to_scheduler_policy(),
            );
            // Rebuild false color LUT if palette changed
            self.state.false_color_lut = self.prefs.viewer.false_color_palette.build_lut();
            // Sync collapsed inspector sections to prefs
            if self.state.remember_inspector_layout {
                let collapsed_key = egui::Id::new("klipp_collapsed_sections");
                let sections: Vec<String> = ctx.data_mut(|d| {
                    let set: &std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                    set.iter().cloned().collect()
                });
                self.prefs.general.collapsed_sections = sections;
                // Mirror Title-inspector expanded groups back to prefs.
                let title_key = egui::Id::new(crate::inspector::TITLE_EXPANDED_GROUPS_KEY);
                let title_groups: std::collections::HashSet<String> = ctx.data_mut(|d| {
                    let set: &std::collections::HashSet<String> = d.get_temp_mut_or_default(title_key);
                    set.iter().cloned().collect()
                });
                self.prefs.title.inspector_expanded_groups = title_groups;
            } else {
                self.prefs.general.collapsed_sections.clear();
                self.prefs.title.inspector_expanded_groups.clear();
            }
            if let Err(e) = self.prefs.save() {
                tracing::warn!("Failed to save preferences: {e}");
            }
            // Live theme reload — picks up Theme dropdown change,
            // accent colour change, role colour edits without
            // requiring a restart. Also re-applies egui visuals so
            // built-in widgets (popups, scrollbars, selection
            // highlights) match the new palette polarity.
            self.theme = KlippTheme::from_prefs(&self.prefs.appearance, &self.theme_registry);
            self.theme.apply_to_egui(ctx);
            self.prefs_dirty = false;
            self.prefs_last_save = std::time::Instant::now();
        }

        // Auto-save. The write itself runs on a worker thread — see the
        // spawn below for why — so first reap a finished one.
        if let Some(rx) = &self.autosave_rx {
            match rx.try_recv() {
                Ok(Ok(path)) => {
                    tracing::info!("Auto-saved to {:?}", path);
                    self.state.toast.show("Auto-saved");
                    self.autosave_rx = None;
                }
                Ok(Err(e)) => {
                    tracing::warn!("Auto-save failed: {e}");
                    self.autosave_rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // Writer thread died without sending — treat as done
                    // so a panicking save can't wedge auto-save forever.
                    tracing::warn!("Auto-save worker vanished without reporting a result");
                    self.autosave_rx = None;
                }
            }
        }

        self.poll_project_copy();
        self.poll_fetched_fonts();
        self.write_untitled_on_first_edit();
        self.sync_project_lock();

        self.publish_viewer_compare();

        // Auto-save: trigger on time interval OR op-count threshold (whichever hits first).
        let interval = self.prefs.general.auto_save_interval_secs;
        let op_threshold = self.prefs.general.auto_save_op_threshold;
        let time_trigger = interval > 0
            && self.last_autosave.elapsed().as_secs() >= interval as u64;
        let op_trigger = op_threshold > 0
            && self.state.history_ops_since_save >= op_threshold as u64;
        // Not for a project open read-only: its auto-save folder sits
        // beside the project, where the editor who holds it would find
        // this session's edits offered as their own recovery.
        if (time_trigger || op_trigger)
            && self.is_unsaved()
            && self.state.project_path.is_some()
            && !crate::project_paths::is_untitled(self.state.project_path.as_deref())
            && self.read_only_project != self.state.project_path
            && !self.state.any_drag_in_progress()
            && self.autosave_rx.is_none()
        {
            if let Some(ref project_path) = self.state.project_path.clone() {
                let autosave_path = autosave_path_for(project_path);
                let color = klipp_project::ColorSettings {
                    global_lut_path: self.state.global_lut_path.clone(),
                    global_lut_opacity: self.state.global_lut_opacity,
                    global_lut_enabled: self.state.global_lut_enabled,
                    track_header_width: self.state.track_header_width,
                    auto_generate_filmstrips: self.state.project_auto_filmstrips,
                    auto_generate_skim: self.state.project_auto_skim,
                    working_colorspace: self.state.project_working_colorspace,
                    grade_encoding: self.state.project_grade_encoding,
                    default_odt: self.state.project_default_odt,
                    default_input_format: self.state.project_default_input_format,
                    default_input_lut: self.state.project_default_input_lut.clone(),
                    viewing_lut: self.state.project_viewing_lut.clone(),
                    scopes_after_viewing_lut: self.state.project_scopes_after_viewing_lut,
                };
                let active_seq_id = self.state.sequences[self.state.active_sequence_idx].id.to_string();

                // Snapshot everything the writer needs. The clone is a
                // handful of allocations; the serialise-plus-disk-write it
                // moves off this thread took seconds on a large project and
                // stalled the whole editor mid-edit, on a timer, with no way
                // for the user to tell what had happened.
                let sequences = self.state.sequences.clone();
                let media_pool = self.state.media_pool.clone();
                let created_at = self.state.created_at.clone();
                let notes = self.state.project_notes.clone();
                let import_report = self.state.last_import_report.clone();
                let title_assets = self.state.title_assets.clone();
                let looks = self.state.looks.clone();
                let backup_count = self.prefs.editing.backup_count;
                let zoom = self.state.timeline_zoom;
                let scroll_x = self.state.timeline_scroll_x;
                let track_scale = self.state.timeline_track_scale;

                let (tx, rx) = crossbeam_channel::bounded(1);
                let repaint_ctx = ctx.clone();
                let spawned = std::thread::Builder::new()
                    .name("klipp-autosave".into())
                    .spawn(move || {
                        let result = klipp_project::save_project_full(
                            &autosave_path,
                            &sequences,
                            &media_pool,
                            &color,
                            Some(&active_seq_id),
                            created_at.as_deref(),
                            &notes,
                            backup_count,
                            Some(zoom),
                            Some(scroll_x),
                            Some(track_scale),
                            &title_assets,
                        )
                        .and_then(|()| {
                            klipp_project::import_report::save(&autosave_path, import_report.as_ref())?;
                            klipp_project::save_looks(&autosave_path, &looks)
                                .map_err(|e| format!("failed to persist Looks: {e}"))
                        })
                        .map(|()| autosave_path);
                        let _ = tx.send(result);
                        // Wake the UI so the toast appears promptly even if
                        // egui is idle (reactive) with nothing else pending.
                        repaint_ctx.request_repaint();
                    });

                match spawned {
                    Ok(_) => {
                        self.autosave_rx = Some(rx);
                        // The snapshot above *is* the saved state, so every
                        // op up to this point is accounted for. Ops made
                        // while the writer runs are not in it and correctly
                        // start the counter climbing again from zero.
                        //
                        // Consequence worth knowing: on a write that then
                        // fails, those ops are no longer counted toward the
                        // op-threshold trigger, so recovery falls to the
                        // time trigger. Preferable to double-counting and
                        // re-saving the same state every frame.
                        self.state.reset_ops_since_save();
                    }
                    Err(e) => tracing::warn!("Auto-save: failed to spawn writer thread: {e}"),
                }
            }
            self.last_autosave = std::time::Instant::now();
        }
    }

    /// The repaint clock, the meters' ballistics, thumbnail textures, and
    /// skim frames routed to the viewer that asked for them.
    pub(super) fn tick_meters_and_skims(&mut self, ctx: &egui::Context) {
        // The loudness playback is heard at, sent when it changes (#1565).
        let brief_lufs = self.state.sequence().brief.as_ref().and_then(|b| b.loudness_lufs);
        let enabled = self.prefs.playback.hear_delivery_loudness;
        let target = super::export::delivery_target(&self.export_dialog, brief_lufs, enabled);
        self.state.delivery_loudness.available = super::export::delivery_target(&self.export_dialog, brief_lufs, true).is_some();
        self.state.delivery_loudness.enabled = enabled;
        if self.state.delivery_loudness.sent != Some(target) {
            self.state.delivery_loudness.sent = Some(target);
            let _ = self.cmd_tx.send(AppCommand::SetDeliveryTarget(target));
        }
        // Frame-rate-aware repaint during playback (must be AFTER drain_events so Playing state is set).
        // 16 ms = 60 Hz cap. Engine grade time at 1080p typically exceeds
        // 8 ms budget under load, so asking the UI for 125 Hz just burns
        // CPU waiting for frames that aren't ready. 60 Hz matches the
        // engine's target and halves UI thread work under grade load.
        // A scrub wants frames too: a skim frame lands on the worker thread
        // and is written into the frame buffer, but nothing wakes the UI to
        // upload it, so without a clock it sits there until the next pointer
        // event. The filmstrip preview used to provide that clock by asking
        // for a repaint on every drag frame; #483 stopped arming it over
        // skimmable media and this replaces the clock without the thumbnail.
        let wants_continuous_frames = self.state.playback == PlaybackState::Playing
            || self.state.source_playback == PlaybackState::Playing
            || self.state.is_scrubbing;
        if wants_continuous_frames {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Tell the watchdog whether a missing `update()` call is a hang
        // or just egui being reactive with nothing to draw. See
        // `klipp_engine::UI_WANTS_FRAMES`.
        klipp_engine::UI_WANTS_FRAMES
            .store(wants_continuous_frames, std::sync::atomic::Ordering::Relaxed);

        // The meters' ballistics are tuned per frame at 60 fps, and taken
        // per this frame's real length: the UI draws at whatever rate the
        // display and the present mode give it, 120 fps and more, and a
        // meter counted in frames fell twice as fast there.
        let ballistics = MeterBallistics::for_frame(ctx.input(|i| i.unstable_dt));

        // Peak meter ballistics: exponential release (~1.7s to decay from 0dB to -60dB).
        // Factor 0.96/frame at 60fps → ~0.96^60 ≈ 0.085 after 1s → -21dB/s.
        // This gives smooth, professional-looking decay without flicker.
        let peak_release = ballistics.peak_release;
        for (l, r) in &mut self.state.track_peak_levels {
            *l *= peak_release;
            *r *= peak_release;
        }
        self.state.master_peak_left *= peak_release;
        self.state.master_peak_right *= peak_release;

        // LUFS display smoothing: EMA filter on momentary and short-term.
        // Integrated is already slow-moving (gated average), no smoothing needed.
        // Alpha ~0.12 → ~8 frames to settle → ~130ms at 60fps. Smooth but responsive.
        fn ema_lufs(smooth: &mut f32, raw: f32, alpha: f32, drift: f32) {
            if !raw.is_finite() {
                // Decay toward -inf when signal stops
                if smooth.is_finite() {
                    *smooth -= drift; // drift down ~90 LUFS/sec
                    if *smooth < -70.0 { *smooth = f32::NEG_INFINITY; }
                }
            } else if !smooth.is_finite() {
                *smooth = raw; // first valid reading: snap
            } else {
                *smooth = *smooth * (1.0 - alpha) + raw * alpha;
            }
        }
        ema_lufs(&mut self.state.lufs_momentary_smooth, self.state.lufs_momentary, ballistics.momentary_alpha, ballistics.drift);
        ema_lufs(&mut self.state.lufs_short_term_smooth, self.state.lufs_short_term, ballistics.short_term_alpha, ballistics.drift);

        // Update performance stats
        if self.state.show_perf_overlay {
            self.update_perf_stats(ctx);
        }

        // Update thumbnails: convert ready thumbnails to textures
        self.update_thumbnail_textures(ctx);

        // Reset skim flag for this frame (set by browser or timeline hover).
        // A playhead drag holds it for the whole gesture: `scrub_to` only
        // runs on frames egui reports the drag on, and the skim must not be
        // declared over on the frames in between.
        self.state.browser_skim_active_this_frame = self.state.is_scrubbing;

        // Process pending skim transcodes (queued during drain_events)
        let pending_skims: Vec<MediaId> = std::mem::take(&mut self.state.pending_skim_transcodes);
        for mid in pending_skims {
            // Re-use the command processing path
            self.handle_skim_transcode(mid);
        }

        // Route the pending skim frame based on the explicit target the
        // dispatch site stamped on the request:
        //
        //   * `ViewerMode::Program` — timeline playhead scrub or
        //     timeline filmstrip hover. The frame represents what the
        //     timeline output is or would be. Write to the engine's
        //     frame buffer so the Program Viewer paints it and scopes
        //     refresh.
        //
        //   * `ViewerMode::Source`  — media-browser bin hover. The frame
        //     is a transient overlay for the Source Viewer and must not
        //     touch the engine frame buffer or it would contaminate the
        //     Program Viewer.
        //
        // A timeline-side skim can never leak into the Source Viewer and
        // a bin-side skim can never leak into the Program Viewer.
        if let Some((media_id, rgba, width, height, target)) = self.state.browser_skim_pending.take() {
            match target {
                klipp_core::ViewerMode::Program => {
                    {
                        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                        fb.write_skim_frame(rgba.clone(), width, height);
                    }
                    // Mark the Program viewer as currently showing a
                    // skim frame so the end-of-frame cleanup knows to
                    // refresh back to the committed playhead when the
                    // mouse leaves the timeline / filmstrip.
                    self.state.program_skim_active = true;
                    // Only when a scope is there to show it: the engine
                    // draws every active scope for each skim frame.
                    if self.state.scopes_shown() {
                        let _ = self.cmd_tx.send(AppCommand::RenderScopesForFrame {
                            rgba,
                            width,
                            height,
                        });
                    }
                }
                klipp_core::ViewerMode::Source => {
                    let Some(image) = crate::ui_util::checked_image([width as usize, height as usize], &rgba, "skim frame") else {
                        return;
                    };
                    let new_size = egui::vec2(width as f32, height as f32);
                    // Reuse the existing handle when dimensions match —
                    // avoids an allocation + lifetime crossing on every
                    // skim tick. Different dimensions (media changed)
                    // still need a fresh handle; defer the old drop.
                    // Never into a poster: until the first frame lands the
                    // skim shows the pool thumbnail's own handle, and
                    // writing through it painted skim pixels into the
                    // pool tile.
                    let is_poster = self.state.browser_skim_texture.as_ref()
                        .is_some_and(|t| self.state.thumbnail_textures.values().any(|p| p.id() == t.id()));
                    match self.state.browser_skim_texture {
                        Some(ref mut tex) if !is_poster && tex.size_vec2() == new_size => {
                            tex.set(image, egui::TextureOptions::LINEAR);
                        }
                        _ => {
                            if let Some(old) = self.state.browser_skim_texture.take() {
                                self.defer_drop_texture(old);
                            }
                            self.state.browser_skim_texture = Some(
                                alloc_texture_logged(ctx, "browser_skim", image, egui::TextureOptions::LINEAR),
                            );
                        }
                    }
                    self.state.browser_skim_media_id = Some(media_id);
                }
            }
        }
    }

    /// The window drawn: file pickers, shortcuts, dialogs, the layout and
    /// every panel. Returns the commands they asked for.
    pub(super) fn draw_window(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        self.keep_graph_before_input(ctx);
        self.state.forget_stale_path_drag(ctx.input(|i| i.pointer.any_down()));
        // Pickers run off the UI thread (see `file_dialog`); what the
        // ones that closed since the last frame chose starts this
        // frame's commands. Their scope is the open project and the
        // active sequence: a pick made for another is dropped.
        let picker_scope = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.state.project_epoch.hash(&mut h);
            self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.id.to_string()).hash(&mut h);
            h.finish()
        };
        crate::file_dialog::install(ctx, picker_scope);
        let picked = crate::file_dialog::take_ready();
        // Everything the pickers had to say this frame, in one toast: a
        // refusal must not hide a worker's "Frame saved".
        let mut said: Vec<String> = Vec::new();
        if picked.stale > 0 {
            said.push("The project or sequence changed while the file dialog was open, so that choice was not applied".into());
        }
        if picked.refused > 0 {
            said.push("A file dialog is already open".into());
        }
        if picked.expired > 0 {
            said.push("The file dialog answered after its panel had closed, so that choice was not applied".into());
        }
        said.extend(picked.notices);
        if !said.is_empty() {
            self.state.toast.show(said.join(" \u{2022} "));
        }
        let mut all_cmds = picked.commands;

        // 2. Handle global keyboard shortcuts
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::SHORTCUTS);
        let kb_cmds = self.handle_global_shortcuts(ctx);
        all_cmds.extend(kb_cmds);

        // 2b. In the Colour workspace the grade follows the playhead.
        self.follow_playhead_for_grading();

        // 2c. The curves draw the viewer frame's histogram behind them.
        // Gated on the frame buffer's generation inside, so a paused
        // viewer costs one pass rather than one per repaint.
        if self.state.active_workspace == crate::state::Workspace::Color {
            self.node_inspector_panel.refresh_curve_histogram(&self.frame_buffer);
        }

        // 3. Handle transport keyboard shortcuts
        let transport_cmds = self.transport.handle_keyboard(ctx, &self.state, &self.key_bindings);
        all_cmds.extend(transport_cmds);

        self.scope_window_runtime.pointer_over = false;

        // 4. Unsaved changes dialog
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::RENDER_DIALOGS);
        let dialog_cmds = self.show_unsaved_dialog(ctx);
        all_cmds.extend(dialog_cmds);

        // The keys in effect, for the tooltips that name them (#2933).
        crate::keybindings::publish(ctx, &self.key_bindings);
        self.show_modal_dialogs(ctx, &mut all_cmds);

        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::RENDER_PANELS);
        let typing_before = ctx.memory(|m| m.focused().is_some());
        self.render_main_layout(ctx, &mut all_cmds);
        // A field left by Enter holds the single keys (#2943).
        let typing_now = ctx.memory(|m| m.focused().is_some());
        let held = self.state.field_keys_held;
        self.state.field_keys_held = ctx.input(|i| super::shortcuts::field_keys_held(held, typing_before, typing_now, i));
        // The Welcome window and the tour (#2933), over the layout just drawn.
        self.show_first_time_help(ctx);

        // A scrub that lost its release. egui does not promise
        // `drag_stopped` when the window loses focus or a modal opens over a
        // drag, and since the scrub flag reached the engine (#483) a stuck
        // one is not cosmetic: it suppresses the parked refresh for that
        // position (#489), holds the foreground signal so background jobs
        // yield indefinitely, and keeps the scrub mute engaged. If nothing
        // is being dragged any more, end the gesture exactly as a release
        // would, so the final seek still lands.
        if self.state.is_scrubbing && ctx.dragged_id().is_none() {
            tracing::debug!("scrub ended without a release; closing it");
            self.timeline.scrub_end(&mut self.state, &mut all_cmds);
        }

        // Dim UI on play: darken everything outside the viewer rect during
        // playback for cinematic focus. Click-through (no allocate_rect),
        // Order::Middle so modals/command palette/markers/toasts paint above
        // it. Fades in/out via egui's animator.
        if self.prefs.playback.dim_ui_on_play {
            let target_alpha = if self.state.playback == PlaybackState::Playing {
                60.0_f32
            } else {
                0.0_f32
            };
            let alpha = ctx.animate_value_with_time(
                egui::Id::new("dim_on_play_alpha"),
                target_alpha,
                0.15, // 150ms fade
            );
            if alpha > 0.5 {
                // Read the actual viewer panel rect that was painted this frame
                // by either render_main_layout (windowed) or render_fullscreen_viewer
                // (fullscreen). Avoids recomputing layout, which would be wrong in
                // fullscreen mode where compute_layout is never called.
                let viewer = self.state.viewer_panel_rect;
                let screen = ctx.screen_rect();
                let dim = egui::Color32::from_black_alpha(alpha as u8);

                ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new("dim_on_play_layer"),
                ))
                .extend([
                    // top strip — full width above the viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, viewer.min.y)),
                        0.0,
                        dim,
                    ),
                    // bottom strip — full width below the viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(egui::pos2(screen.min.x, viewer.max.y), screen.max),
                        0.0,
                        dim,
                    ),
                    // left strip — between top and bottom strips, left of viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(screen.min.x, viewer.min.y),
                            egui::pos2(viewer.min.x, viewer.max.y),
                        ),
                        0.0,
                        dim,
                    ),
                    // right strip — between top and bottom strips, right of viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(viewer.max.x, viewer.min.y),
                            egui::pos2(screen.max.x, viewer.max.y),
                        ),
                        0.0,
                        dim,
                    ),
                ]);
            }
        }

        // Markers panel
        if self.state.show_markers_panel {
            let cmds = self.markers.show(ctx, &mut self.state);
            all_cmds.extend(cmds);
        }

        // Edit list (#923)
        if self.state.show_edit_list {
            let cmds = self.edit_list.show(ctx, &mut self.state);
            all_cmds.extend(cmds);
        }

        // Subtitle editor panel
        if self.state.show_subtitle_editor {
            let cmds = crate::panels::subtitle_editor::show(ctx, &mut self.state);
            all_cmds.extend(cmds);
        }

        // Transcript panel
        if self.state.show_transcript_panel {
            let cmds = self.transcript_panel.show(ctx, &mut self.state, &self.transcript_source, &self.theme);
            all_cmds.extend(cmds);
        } else {
            self.state.transcript_panel_hovered = false;
        }
        self.run_pending_doc_ops();

        // Project Notes panel
        if self.state.show_project_notes {
            // Borrow split: take the open flag out of state so the panel
            // can mutate state.project_notes + state.dirty without the
            // borrow checker tripping over the &mut bool.
            let mut open = self.state.show_project_notes;
            crate::panels::project_notes::show_project_notes(
                ctx,
                &mut self.state,
                &self.theme,
                &mut open,
            );
            self.state.show_project_notes = open;
        }

        // Colour Check window: the frame on screen read the way a
        // colourist reads it, with the fault named and the knob to turn.
        if self.state.show_colour_check {
            let mut open = self.state.show_colour_check;
            let action = {
                let fb = self.frame_buffer.clone();
                self.colour_check.show(ctx, &self.state, &fb, &self.theme, &mut open)
            };
            self.state.show_colour_check = open;
            match action {
                crate::panels::colour_check::ColourAction::None => {}
                crate::panels::colour_check::ColourAction::SelfCheck => self.run_colour_self_check(),
                tool => self.run_colour_tool(tool),
            }
        }
        // A colour tool keeps running with the window closed; its answer
        // is there when it is opened again.
        if self.colour_tool_reply.is_some() {
            self.take_colour_tool_reply(ctx);
        }

        self.publish_picture_transform();

        // Delivery Check window (#1978): the check's findings, reviewed
        // where they are read. Its request keeps running with the window
        // closed; the answer is there when it opens again.
        if self.state.show_delivery_check {
            let mut open = self.state.show_delivery_check;
            let action = self.delivery_check.show(ctx, &self.theme, self.local_control.is_some(), &mut open);
            self.state.show_delivery_check = open;
            self.act_on_delivery_check(action);
        }
        if self.delivery_reply.is_some() {
            self.take_delivery_reply(ctx);
        }
        if self.translation.is_some() {
            self.take_translation(ctx);
        }

        // Sound Over Time window: a whole file's sound at once.
        if self.state.show_sound_over_time {
            let mut open = self.state.show_sound_over_time;
            self.sound_over_time.show(ctx, &self.state, &self.theme, &mut open);
            self.state.show_sound_over_time = open;
        }

        // Narrate window: a scratch voice from text.
        if self.state.show_narrate {
            let mut open = self.state.show_narrate;
            let cmds = self.narrate.show(ctx, &mut self.state, &mut open);
            self.state.show_narrate = open;
            self.send_commands(cmds);
        }

        // Edit Plan window: the paper edit awaiting the person's word.
        if self.state.show_edit_plan {
            let mut open = self.state.show_edit_plan;
            let action = crate::panels::edit_plan::show_edit_plan(ctx, &self.state, &self.theme, &mut open);
            self.state.show_edit_plan = open;
            self.act_on_edit_plan(action);
        }

        // Command palette (Ctrl+K). Entry list comes from
        // `crate::command_palette::build` so adding a new entry is a
        // single edit (and the `palette_anchors_present` test there
        // catches accidental removals).
        if self.state.show_command_palette {
            use crate::command_palette::{build as build_palette, dispatch_dialog, PaletteAction};

            // Modal scrim — match the AI Models dialog so the
            // palette has the same "this layer is the active one"
            // visual treatment. Captures click + drag so timeline
            // / browser interactions behind don't fire while the
            // user is typing a command.
            let screen_rect = ctx.screen_rect();
            egui::Area::new(egui::Id::new("command_palette_scrim"))
                .order(egui::Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
                    ui.painter().rect_filled(
                        screen_rect,
                        0.0,
                        egui::Color32::from_black_alpha(140),
                    );
                    if resp.clicked() {
                        // Click-outside dismisses, matching the
                        // AI Models dialog convention.
                        self.state.show_command_palette = false;
                    }
                });

            let entries = build_palette(&self.state);
            let query = self.state.command_palette_query.to_lowercase();
            let filtered_idx: Vec<usize> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    query.is_empty()
                        || e.label.to_lowercase().contains(&query)
                        || e.shortcut.to_lowercase().contains(&query)
                })
                .map(|(i, _)| i)
                .collect();

            let max_visible = 12;
            let sel = &mut self.state.command_palette_selected;
            if *sel >= filtered_idx.len() { *sel = 0; }

            let mut close = false;
            let mut activate_idx: Option<usize> = None;

            egui::Area::new(egui::Id::new("command_palette"))
                .fixed_pos(egui::pos2(
                    ctx.screen_rect().center().x - 200.0,
                    ctx.screen_rect().top() + 80.0,
                ))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::window(ui.style())
                        .fill(self.theme.bg_panel)
                        .stroke(egui::Stroke::new(1.0_f32, self.theme.border))
                        .rounding(4.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.set_min_width(400.0);

                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.state.command_palette_query)
                                    .hint_text("Type a command...")
                                    .desired_width(384.0)
                                    .font(egui::FontId::proportional(13.0))
                            );
                            resp.request_focus();

                            let input = ui.input(|i| (
                                i.key_pressed(egui::Key::ArrowDown),
                                i.key_pressed(egui::Key::ArrowUp),
                                i.key_pressed(egui::Key::Enter),
                                i.key_pressed(egui::Key::Escape),
                            ));
                            if input.0 && *sel + 1 < filtered_idx.len() { *sel += 1; }
                            if input.1 && *sel > 0 { *sel -= 1; }
                            if input.3 { close = true; }
                            if input.2 {
                                if let Some(&entry_idx) = filtered_idx.get(*sel) {
                                    activate_idx = Some(entry_idx);
                                }
                                close = true;
                            }

                            ui.add_space(4.0);

                            for (vis_i, &entry_idx) in filtered_idx.iter().take(max_visible).enumerate() {
                                let entry = &entries[entry_idx];
                                let is_selected = vis_i == *sel;
                                let bg = if is_selected {
                                    self.theme.bg_hover
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let text_color = if entry.enabled {
                                    self.theme.text_primary
                                } else {
                                    self.theme.text_dim
                                };
                                let (r, _) = ui.allocate_exact_size(egui::vec2(400.0, 22.0), egui::Sense::click());
                                ui.painter().rect_filled(r, 4.0, bg);
                                ui.painter().text(
                                    egui::pos2(r.left() + 8.0, r.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    entry.label.as_ref(),
                                    egui::FontId::proportional(12.0),
                                    text_color,
                                );
                                if !entry.shortcut.is_empty() {
                                    ui.painter().text(
                                        egui::pos2(r.right() - 8.0, r.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        entry.shortcut.as_ref(),
                                        egui::FontId::monospace(10.0),
                                        self.theme.text_dim,
                                    );
                                }
                                if ui.rect_contains_pointer(r) && ui.input(|i| i.pointer.primary_clicked()) {
                                    activate_idx = Some(entry_idx);
                                    close = true;
                                }
                            }

                            if filtered_idx.is_empty() {
                                ui.colored_label(
                                    self.theme.text_dim,
                                    "No matching commands",
                                );
                            }
                        });
                });

            if close {
                self.state.show_command_palette = false;
            }
            if let Some(idx) = activate_idx {
                let entry = &entries[idx];
                if entry.enabled {
                    match &entry.action {
                        PaletteAction::Cmd(cmd) => all_cmds.push(cmd.clone()),
                        PaletteAction::ToggleState(f) | PaletteAction::SetState(f) => {
                            f(&mut self.state);
                        }
                        PaletteAction::OpenDialog(kind) => {
                            let kind = *kind;
                            crate::file_dialog::spawn(move || dispatch_dialog(kind));
                        }
                        PaletteAction::Custom(f) => {
                            f(&mut self.state, &mut all_cmds);
                        }
                    }
                }
            }
        }

        // Model-download dialogs (consent + progress + error). Drives
        // the F-97 SAM2 download flow. The state machine lives on
        // AppState.model_download; transitions are handled here per
        // frame.
        self.render_model_download_dialogs(ctx, &mut all_cmds);
        crate::aaf_install::show(ctx, &mut self.state.aaf_install, &mut all_cmds);
        self.poll_aaf_reading(ctx);
        self.keep_brief_with_transcription();
        self.follow_transcriptions();

        // History panel
        if self.state.show_history_panel {
            let mut open = true;
            egui::Window::new("History")
                .open(&mut open)
                .resizable(true)
                .default_size([220.0, 400.0])
                .show(ctx, |ui| {
                    // This sequence's steps and the project's, in the
                    // order ctrl+z and redo take them (#1986).
                    let (undo, redo) = self.state.history_view();
                    ui.label(format!("{} undo / {} redo", undo.len(), redo.len()));
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                        // Redo stack (future states, shown dimmed at top)
                        for (i, label) in redo.iter().enumerate().rev() {
                            let resp = ui.selectable_label(false,
                                egui::RichText::new(format!("  {label}"))
                                    .size(11.0)
                                    .color(self.theme.text_dim)
                            );
                            if resp.clicked() {
                                // Redo (redo_count - i) steps
                                for _ in 0..=(redo.len() - 1 - i) {
                                    all_cmds.push(AppCommand::Redo);
                                }
                            }
                        }
                        // Current state marker
                        ui.colored_label(self.theme.accent,
                            egui::RichText::new("\u{25B6} Current State").size(11.0));
                        // Undo stack (past states, most recent first)
                        for (i, label) in undo.iter().enumerate().rev() {
                            let resp = ui.selectable_label(false,
                                egui::RichText::new(format!("  {label}")).size(11.0));
                            if resp.clicked() {
                                // Undo (undo_count - i) steps
                                for _ in 0..=(undo.len() - 1 - i) {
                                    all_cmds.push(AppCommand::Undo);
                                }
                            }
                        }
                    });
                });
            if !open {
                self.state.show_history_panel = false;
            }
        }

        // About dialog
        if self.state.show_about_dialog {
            match crate::panels::about::show(ctx, &self.theme, &self.updates.status()) {
                crate::panels::about::AboutAction::Close => self.state.show_about_dialog = false,
                crate::panels::about::AboutAction::CheckForUpdates => self.updates.check_now(),
                crate::panels::about::AboutAction::Install => self.updates.install(),
                crate::panels::about::AboutAction::None => {}
            }
        }

        // Relink dialog
        if self.state.show_relink_dialog {
            let mut open = true;
            egui::Window::new("Relink Offline Media")
                .collapsible(false)
                .resizable(true)
                .default_width(600.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    let relinks = self.relink_panel.show(ui, &self.state, &self.theme);
                    all_cmds.extend(relinks);
                    ui.add_space(4.0);
                    if ui.button("Close").clicked() {
                        self.state.show_relink_dialog = false;
                    }
                });
            if !open {
                self.state.show_relink_dialog = false;
            }
        }

        // (Removed: unconditional request_repaint during playback. That
        // call meant "next tick ASAP" — combined with the 16 ms
        // request_repaint_after above it forced UI redraws to run as
        // fast as the event loop can go, burning CPU. 60 Hz via the
        // request_repaint_after is sufficient and matches the engine.)

        // 9b. Floating scope window — runs AFTER CentralPanel so viewer_image_rect is current-frame
        if !self.state.fullscreen_viewer {
            let locked_data = if self.state.scope_cursor_locked {
                self.state.scope_cursor_locked_data
            } else {
                None
            };
            // Collect audio-scope inputs. Both are snapshotted under
            // `try_lock` so the UI never stalls behind the mix thread —
            // a missed frame just means the scope holds its previous
            // texture, which naturally reads as "frozen on pause."
            let correlation_samples: Vec<f32> = if let Ok(ring) =
                self.state.spectrum_ring.try_lock()
            {
                // Last 4096 interleaved stereo samples = 2048 stereo pairs
                // ≈ 43 ms at 48 kHz. Enough trace density for a stable
                // Lissajous figure, small enough to fit on the stack
                // of the render path.
                const N: usize = 4096;
                let len = ring.len();
                let start = len.saturating_sub(N);
                ring.iter().copied().skip(start).collect()
            } else {
                Vec::new()
            };
            let lufs_history: Vec<f32> =
                self.state.lufs_short_term_history.iter().copied().collect();
            let lufs_target = self.state.lufs_target;
            let spectrum_history: Vec<Vec<f32>> =
                self.state.spectrum_history.iter().cloned().collect();
            // The engine keeps one low-pass filter and one bloom for every
            // scope: the floating window's checkboxes show and set those.
            crate::widgets::scope_window::show_shared_toggles(
                &mut self.state.scope_window,
                self.state.scope_low_pass,
                self.state.scope_bloom,
            );
            self.scope_window_runtime.hdr_levels = self.state.hdr_monitor.renders_hdr();
            // The look's skin level, for the false-colour pane's green
            // band, so it agrees with the skin findings rather than
            // being a second opinion from a fixed IRE window.
            self.state.scope_window.look_skin_l = self
                .state
                .sequence()
                .brief
                .as_ref()
                .and_then(|b| b.look.as_ref())
                .and_then(|l| l.skin_l);
            crate::widgets::scope_window::show_scope_window(
                ctx,
                &mut self.state.scope_window,
                &mut self.state.scope_options,
                &mut self.scope_window_runtime,
                &self.frame_buffer,
                self.prefs.color.vectorscope_skin_angle,
                self.state.playback == PlaybackState::Playing,
                self.state.viewer_image_rect,
                self.state.viewer_pointer_pos,
                locked_data,
                &self.state.spectrum_bins,
                &correlation_samples,
                &lufs_history,
                lufs_target,
                &spectrum_history,
                &self.theme,
                klipp_color::scopes::ScopeBasis::for_working_space(
                    self.state.project_working_colorspace,
                ),
            );
            crate::widgets::scope_window::take_shared_toggles(
                &self.state.scope_window,
                &mut self.state.scope_low_pass,
                &mut self.state.scope_bloom,
            );
        }
        all_cmds
    }

    /// The frame's commands sent, after a quit is held back from an engine
    /// that still has unsaved work to wait for. Returns whether a quit was
    /// asked for.
    pub(super) fn dispatch_commands(&mut self, ctx: &egui::Context, mut all_cmds: Vec<AppCommand>) -> bool {
        // 10. Process quit
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::PROCESS_QUIT);
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // When unsaved changes are about to raise the Save dialog, the
        // Quit command must NOT reach the engine — the engine treats it
        // as "shut down now" and would kill itself behind whatever is
        // queued while the user is still deciding. Cancel would then
        // leave a dead engine, and the engine thread's teardown while
        // the app keeps running races eframe's wgpu state (shutdown
        // SIGSEGV). The dialog's Don't Save / Save buttons hard-exit
        // the process themselves, so the engine never needs this Quit.
        if has_quit && self.is_unsaved() {
            all_cmds.retain(|c| !matches!(c, AppCommand::Quit));
        }

        // Save layout state on quit
        if has_quit {
            self.layout.save();
        }

        // 11. Send all commands
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::SEND_COMMANDS);
        self.send_commands(all_cmds);
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::POST_COMMANDS);

        // 11.5. Upload title overlay texture if a title property was changed this frame
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::UPLOAD_TITLE);
        self.upload_title_overlay(ctx);
        has_quit
    }

    /// The end of a frame: skim state let go, the toasts and drag ghost
    /// drawn, the proxy notices, a window close, and the repaint asked for.
    pub(super) fn finish_frame(&mut self, ctx: &egui::Context, has_quit: bool) {
        // Clear title overlay during playback (engine handles compositing then)
        if self.state.playback == PlaybackState::Playing {
            if let Some(old) = self.viewer.title_overlay_texture.take() {
                self.defer_drop_texture(old);
            }
        }

        // A timeline skim the engine never answered (#2945): let the next go.
        if self.state.timeline_skim_in_flight.is_some_and(|(t, _)| t.elapsed().as_secs_f32() > 2.0) {
            tracing::warn!("timeline skim unanswered: sending the next");
            self.state.timeline_skim_in_flight = None;
            self.send_queued_timeline_skim();
        }

        // Watchdog: reset stuck skim_decode_in_flight after 2s with no completion.
        // Belt-and-braces backup to the always-send-completion fix in the decode threads.
        if self.state.skim_decode_in_flight {
            if let Some(t) = self.state.skim_decode_started_at {
                if t.elapsed().as_secs_f32() > 2.0 {
                    tracing::warn!("skim decode timed out — resetting in-flight flag");
                    self.state.skim_decode_in_flight = false;
                    self.state.skim_decode_started_at = None;
                }
            }
        }

        // 11a. Skim cleanup: if no panel set the skim flag this frame, tear
        // down whatever skim state is still alive. Three independent skim
        // surfaces:
        //   * `browser_skim_media_id` — Source-viewer media-pool hover
        //     (texture overlay).
        //   * `skim_audio_session`    — audio skim sidecar (cpal).
        //   * `program_skim_active`   — Program-viewer timeline /
        //     filmstrip hover (frame written into the engine's
        //     frame_buffer; refreshing it requires a re-seek to the
        //     committed playhead because the skim path overwrote what
        //     the engine last decoded for playback).
        // All three need to wind down together so pro-NLE "mouse-leave
        // = back to the committed playhead" semantics hold uniformly.
        let lost_focus = !ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let any_skim_state_alive = self.state.browser_skim_media_id.is_some()
            || self.state.skim_audio_session.is_some()
            || self.state.program_skim_active;
        if (!self.state.browser_skim_active_this_frame && any_skim_state_alive) || lost_focus {
            // The skim is over: coming back to the same frame asks for it again.
            self.state.skim_last_requested = None;
            if self.state.browser_skim_media_id.is_some() {
                // Route the cleared handle through the deferred-drop ring —
                // the current frame's submit may still reference its ID,
                // and dropping inline here trips egui-wgpu's
                // "Texture has been destroyed" validation.
                if let Some(old) = self.state.browser_skim_texture.take() {
                    self.defer_drop_texture(old);
                }
                self.state.browser_skim_media_id = None;
                // `viewer_mode` is no longer reset here. It's now the
                // user-controlled focus indicator (set by clicking a
                // viewer or by LoadSourceMedia); resetting it on skim
                // end would silently steal focus back to Program every
                // time a hover finished.
            }
            // End audio skim session when mouse leaves or app loses focus
            if self.state.skim_audio_session.is_some() {
                self.send_commands(vec![AppCommand::BrowserSkimAudioEnd]);
            }
            // Program-viewer skim ended: reset the flag, and when not
            // actively playing back, re-decode at the committed playhead
            // so the viewer reverts from the last-skimmed frame to the
            // actual current frame.
            //
            // CRITICAL: `state.playhead` is only authoritative when not
            // playing. During playback `PlayheadMoved` updates the
            // *anchor* and leaves `state.playhead` pinned at the
            // pre-play position (see the PlayheadMoved handler). If we
            // emitted `SeekTo(state.playhead)` mid-playback we'd snap
            // back to wherever playback started — exactly the bug we
            // saw. Skip the seek when playing; the engine overwrites
            // `frame_buffer` with its next real frame within ~one frame
            // anyway, so the skim leftover disappears on its own.
            if self.state.program_skim_active {
                self.state.program_skim_active = false;
                if self.state.playback != PlaybackState::Playing {
                    // Debounced: arm a deadline instead of seeking now.
                    // Continuous skimming flaps this flag for a frame at
                    // clip boundaries; an immediate revert per flap
                    // flickers the viewer and queues a seek per flap
                    // (the engine pays a full seek for each — a hover
                    // session once stalled it 8+ s draining them). A
                    // new skim frame before the deadline disarms it.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(150);
                    self.state.program_skim_revert_deadline = Some(deadline);
                    ctx.request_repaint_after(std::time::Duration::from_millis(160));
                }
            }
        }

        // Fire the debounced skim-end revert once the skim has stayed
        // quiet past the deadline: one SeekTo(committed playhead) to
        // repaint the Program viewer over the last skim frame.
        if let Some(deadline) = self.state.program_skim_revert_deadline {
            if self.state.program_skim_active
                || self.state.playback == PlaybackState::Playing
                || self.state.is_scrubbing
            {
                // Skim resumed (flag flap), playback took over, or a drag
                // began — the engine repaints on its own and `scrub_end`
                // commits the drop position, so no revert is needed. A
                // deadline armed by the hover that ended as the drag began
                // would otherwise still fire in the middle of it.
                self.state.program_skim_revert_deadline = None;
            } else if std::time::Instant::now() >= deadline {
                self.state.program_skim_revert_deadline = None;
                let cur = self.state.playhead;
                let _ = self.cmd_tx.send(AppCommand::SeekTo(cur));
            } else {
                // Not due yet — make sure a repaint arrives to fire it
                // even if the mouse goes still.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        // Fullscreen viewer is an in-window "cinema mode": the main
        // layout is replaced with a viewer-only panel filling
        // `ctx.screen_rect()`. We deliberately do NOT send
        // `ViewportCommand::Fullscreen` — on Wayland+NVIDIA the
        // resulting swap-chain reconfigure leaves the surface in a
        // `VK_SUBOPTIMAL_KHR` state that presents a stale frame
        // forever. Users who want OS-level fullscreen can use their
        // window manager's own fullscreen hotkey (Super+F on most
        // compositors) on the Klipp window instead.
        if self.state.fullscreen_changed {
            self.state.fullscreen_changed = false;
            ctx.request_repaint();
        }

        // 11c. Toast notifications + drag-drop cleanup
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::TOAST_AND_GHOST);
        // Phase 5 — fire any one-shot first-time tips that the
        // current sequence state has just unlocked. Cheap per-frame
        // check; the dismissal set is consulted up front so each tip
        // ever fires at most once, persisted to prefs.
        self.maybe_fire_first_time_tips();
        self.state.toast.render(ctx, &self.theme);
        self.state.toast.cleanup();

        // 11c-bis. Floating drag ghost for FilePaths drags from the file
        // browser. Other DragPayload types rely on target-side highlighting
        // alone, but the file browser → timeline path crosses a lot of
        // empty screen space, so we render a small label following the
        // cursor for the entire duration of the drag.
        if let Some(crate::drag_drop::DragPayload::FilePaths { ref paths }) = self.state.drag_drop.payload {
            if let Some(pos) = self.state.drag_drop.cursor_pos {
                let label = if paths.len() == 1 {
                    paths[0].file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| paths[0].to_string_lossy().to_string())
                } else {
                    format!("{} files", paths.len())
                };
                let theme = &self.theme;
                egui::Area::new(egui::Id::new("file_drag_ghost"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(pos + egui::vec2(12.0, 12.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(theme.bg_raised)
                            .stroke(egui::Stroke::new(1.0_f32, theme.accent))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(label)
                                        .size(11.0)
                                        .color(theme.text_primary),
                                );
                            });
                    });
            }
        }

        // If the user releases the mouse anywhere outside a drop target while
        // a FilePaths drag is in progress, clear the payload (otherwise it
        // would persist into the next click).
        if matches!(self.state.drag_drop.payload, Some(crate::drag_drop::DragPayload::FilePaths { .. }))
            && !ctx.input(|i| i.pointer.any_down())
        {
            self.state.drag_drop.end();
        }

        self.state.drag_drop.frame_start();

        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::FRAME_END);

        // 12. Filmstrip generation — shown in status bar now (no popup)

        // 12b. Proxy generation toast
        if !self.state.proxy_toast_dismissed {
            if let Some((ref msg, when)) = self.state.proxy_status_message {
                let age = when.elapsed().as_secs_f64();
                if age < 5.0 {
                    let y_offset = if self.state.filmstrip_generating && !self.state.filmstrip_toast_dismissed {
                        -120.0
                    } else {
                        -80.0
                    };
                    // Compute aggregate progress across all generating proxies
                    let avg_progress = if !self.state.proxy_generating.is_empty() {
                        let total: f32 = self.state.proxy_generating.iter()
                            .map(|id| self.state.proxy_progress.get(id).copied().unwrap_or(0.0))
                            .sum();
                        total / self.state.proxy_generating.len() as f32
                    } else {
                        0.0
                    };
                    let is_generating = !self.state.proxy_generating.is_empty();
                    // Compute ETA from earliest start time
                    let eta_str = if is_generating && avg_progress > 0.01 {
                        let earliest_start = self.state.proxy_start_times.values().min().copied();
                        if let Some(start) = earliest_start {
                            let elapsed = start.elapsed().as_secs_f64();
                            let remaining = (elapsed / avg_progress as f64) - elapsed;
                            let rem_secs = remaining as u64;
                            if rem_secs >= 60 {
                                format!(" — ~{}m {:02}s left", rem_secs / 60, rem_secs % 60)
                            } else {
                                format!(" — ~{}s left", rem_secs)
                            }
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let msg_clone = msg.clone();
                    egui::Area::new(egui::Id::new("proxy_toast"))
                        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, y_offset))
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style())
                                .fill(self.theme.bg_raised)
                                .stroke(egui::Stroke::new(1.0_f32, self.theme.border))
                                .rounding(4.0)
                                .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if is_generating {
                                            ui.spinner();
                                            ui.add_space(8.0);
                                        }
                                        if is_generating {
                                            ui.label(
                                                egui::RichText::new(format!("{} — {:.0}%{}", msg_clone, avg_progress * 100.0, eta_str))
                                                    .color(self.theme.text_primary)
                                                    .size(12.0),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new(&msg_clone)
                                                    .color(self.theme.text_primary)
                                                    .size(12.0),
                                            );
                                        }
                                        ui.add_space(8.0);
                                        if ui.add(egui::Button::image(Icon::X.image(12.0).tint(self.theme.text_primary))).on_hover_text("Dismiss").clicked() {
                                            self.state.proxy_toast_dismissed = true;
                                        }
                                    });
                                    if is_generating {
                                        ui.add_space(4.0);
                                        let bar = egui::ProgressBar::new(avg_progress)
                                            .desired_width(250.0);
                                        ui.add(bar);
                                    }
                                });
                        });
                    ctx.request_repaint();
                } else if self.state.proxy_generating.is_empty() {
                    self.state.proxy_status_message = None;
                }
            }
        }
        // Keep toast alive while generating; un-dismiss when new proxy starts
        if !self.state.proxy_generating.is_empty() {
            if let Some((_, ref mut when)) = self.state.proxy_status_message {
                *when = std::time::Instant::now();
            }
        }

        // Intercept window X button close — treat same as File → Quit.
        // Gate on `!show_unsaved_dialog` so CancelClose isn't fired
        // every frame while the dialog is up (egui keeps reporting
        // close_requested = true until either Close or CancelClose is
        // actually processed by the OS event loop, which can be a
        // few frames late). The repeated CancelClose stomps on
        // egui's viewport state machine and can wedge the input
        // handler — symptoms include the dialog rendering but
        // ignoring clicks.
        let window_close_requested = ctx.input(|i| i.viewport().close_requested());
        if window_close_requested {
            let unsaved = self.is_unsaved();
            tracing::info!(
                "viewport close_requested received (unsaved={}, dialog_already_open={})",
                unsaved,
                self.state.show_unsaved_dialog,
            );
            // A dirty project is asked about whether it has been saved
            // before or not. Gating on a path made a never-saved project —
            // the state every first session is in — close in silence with
            // everything in it.
            if unsaved && !self.state.show_unsaved_dialog {
                tracing::info!("→ raising unsaved-changes dialog, sending CancelClose");
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else if !unsaved {
                eprintln!("Klipp: clean close → exit(0)");
                // Read-only, the auto-save beside the project is the
                // holder's crash recovery, not ours to clear.
                if let Some(path) = self.writable_project_dir() {
                    let autosave = autosave_path_for(&path);
                    if autosave.exists() {
                        std::thread::Builder::new()
                            .name("klipp-autosave-rm".into())
                            .spawn(move || {
                                let _ = std::fs::remove_dir_all(&autosave);
                            })
                            .ok();
                    }
                }
                self.kill_children_for_exit();
                self.layout.save();
                self.exit_after_engine_shutdown();
            } else {
                // Dialog is already up: dialog buttons drive the
                // outcome. Send CancelClose once to reject this
                // window-manager close attempt; egui clears
                // close_requested next frame on its own, so we
                // don't loop.
                tracing::info!("→ dialog already open, rejecting this close request");
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }

        if has_quit {
            if self.is_unsaved() {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
                eprintln!("Klipp: has_quit branch → exit(0)");
                // Read-only, the auto-save beside the project is the
                // holder's crash recovery, not ours to clear.
                if let Some(path) = self.writable_project_dir() {
                    let autosave = autosave_path_for(&path);
                    if autosave.exists() {
                        std::thread::Builder::new()
                            .name("klipp-autosave-rm".into())
                            .spawn(move || {
                                let _ = std::fs::remove_dir_all(&autosave);
                            })
                            .ok();
                    }
                }
                self.kill_children_for_exit();
                self.layout.save();
                self.exit_after_engine_shutdown();
            }
        }

        if self.state.playback == PlaybackState::Playing || self.state.render_queue.has_active_job() {
            ctx.request_repaint();
        }

        // Watchdog: clear any unconsumed Ctrl+F focus request. If the
        // active panel had a search field, its show() already
        // request_focus'd and reset the field. If not (Inspector /
        // Viewer / Timeline / etc.), this clears the stale request so
        // the next Ctrl+F starts from a clean slate instead of
        // fighting a leftover one.
        self.state.focus_search_request = None;
    }
}
