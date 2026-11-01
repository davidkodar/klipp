//! Keeping the engine in step: the sequence, the media pool and log formats
//! sent to it, frames and scope previews refreshed, and commands sent (#890).

use super::*;

impl KlippApp {
    pub(super) fn sync_sequence_to_engine(&self) {
        sync_sequence(&self.state, &self.sequence, &self.param_change_queue);
    }

    /// Fast UI-thread re-grade at reduced resolution for instant scope
    /// feedback, WITHOUT taking the shared sequence write lock or running
    /// `flatten_sequence`. Used on the slider-scrub path: the engine has
    /// already been given the change via `param_change_queue` and the UI's
    /// local `self.state.sequences[…]` mirror has been mutated, so all we
    /// need is to refresh the scope preview buffer + bump the frame-buffer
    /// generation.
    fn refresh_scope_preview_fast(&mut self) {
        // Throttle: the UI-thread re-grade is ~10 ms on 1080p/4 for a
        // typical colour-balance graph. At slider-drag rate (60–120
        // SetNodeParam per second) this would saturate the UI thread.
        // 33 Hz is plenty for scope preview — the user can't
        // distinguish 30 fps from 120 fps on a scope trace anyway.
        if self.last_scope_preview_refresh.elapsed()
            < std::time::Duration::from_millis(30)
        {
            return;
        }
        self.last_scope_preview_refresh = std::time::Instant::now();
        let t_start = std::time::Instant::now();
        self.refresh_scope_preview_inner();
        let total_ms = t_start.elapsed().as_secs_f64() * 1000.0;
        // Correlate with ENGINE_TICK_SLOW / UI_TICK_SLOW by timestamp.
        // Threshold at 5 ms so normal HD-sized runs stay quiet; 4K
        // downscale + grade will always fire.
        if total_ms > 5.0 {
            tracing::debug!(
                "SCOPE_PREVIEW_FAST {:.1}ms queue_depth={}",
                total_ms,
                self.param_change_queue.len(),
            );
        }
        // Don't send SequenceUpdated — would trigger the engine's audio
        // restart + grade-cache invalidation path, which is already handled
        // by drain_param_changes() on the engine side.
    }

    /// Sync sequence and re-grade the cached frame directly on the UI thread.
    /// Zero latency — scopes see the result on the same frame as the slider change.
    pub(super) fn sync_and_refresh_frame(&mut self) {
        self.sync_sequence_to_engine();
        self.refresh_scope_preview_inner();
        // Tell engine to re-grade at full resolution + render scope texture
        let scope_mode = match self.state.scope_mode {
            crate::state::ScopeMode::Waveform => 0u8,
            crate::state::ScopeMode::Parade => 1,
            crate::state::ScopeMode::Vectorscope => 2,
            crate::state::ScopeMode::Histogram => 3,
        };
        let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: scope_mode });
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
    }

    /// Shared scope-preview refresh used by both the slow path
    /// (`sync_and_refresh_frame`) and the slider-scrub fast path
    /// (`refresh_scope_preview_fast`). Does NOT touch the shared Sequence
    /// RwLock — reads only from the UI's own `self.state.sequences` mirror.
    fn refresh_scope_preview_inner(&mut self) {
        // Fast UI-thread re-grade at reduced resolution for instant scope feedback.
        let fb_snapshot = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            match (&fb.pregrade, fb.width) {
                // The frame, not its picture — `display()` below makes that,
                // outside this lock and only if a preview is really wanted.
                (Some(pregrade), w) if w > 0 => {
                    Some((std::sync::Arc::clone(pregrade), fb.width, fb.height))
                }
                _ => None,
            }
        };

        // Try to produce a graded scope_preview; fall back to pregrade on any failure
        let mut preview: Option<(Vec<u8>, u32, u32)> = None;

        if let Some((ref pregrade, full_w, full_h)) = fb_snapshot {
            if let Some(cid) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                // Its grade as the render copy runs it: inside its grade
                // group's (#237), then its track's (#1060).
                let seq = &self.state.sequences[seq_idx];
                if let (Some(clip), Some(graph)) = (seq.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == cid)), seq.render_grade(cid)) {
                    let graph = graph.as_ref();
                    if !graph.is_identity() {
                        // Here, and not before: an identity graph gets no
                        // preview, and for an HDR source this encode is tens
                        // of milliseconds at 4K. Made once per frame and kept,
                        // so a drag pays it on its first step only.
                        let pregrade_data = pregrade.display();
                        let scale = if full_w > 960 { 4u32 } else if full_w > 480 { 2 } else { 1 };
                        let small_w = full_w / scale;
                        let small_h = full_h / scale;
                        let mut small = vec![0u8; (small_w * small_h * 4) as usize];
                        for y in 0..small_h {
                            for x in 0..small_w {
                                let src = ((y * scale * full_w + x * scale) * 4) as usize;
                                let dst = ((y * small_w + x) * 4) as usize;
                                if src + 3 < pregrade_data.len() && dst + 3 < small.len() {
                                    small[dst..dst + 4].copy_from_slice(&pregrade_data[src..src + 4]);
                                }
                            }
                        }

                        let clip_time = (self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
                        // A shrunk preview, graded with its pixel-sized
                        // settings shrunk with it, so it looks like the
                        // full frame (#1012).
                        let seq_h = self.state.sequence().resolution.height.max(1) as f32;
                        self.scope_executor.set_render_scale(small_h as f32 / seq_h);
                        match self.scope_executor.execute(
                            &small, graph, small_w, small_h,
                            clip_time, 0, None, None,
                        ) {
                            Ok((graded, _)) => { preview = Some((graded, small_w, small_h)); }
                            Err(_) => { preview = Some((small, small_w, small_h)); }
                        }
                    }
                }
            }
        }

        // ALWAYS bump generation so scopes detect the change
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((data, w, h)) = preview {
                fb.scope_preview = data;
                fb.scope_preview_w = w;
                fb.scope_preview_h = h;
            } else {
                // Clear scope_preview so scopes fall back to fb.data
                fb.scope_preview.clear();
                fb.scope_preview_w = 0;
            }
            fb.generation = fb.generation.wrapping_add(1);
        }
    }

    pub(super) fn sync_media_pool_to_engine(&self) {
        let mut pool = self.media_pool_shared.write().unwrap_or_else(|e| e.into_inner());
        *pool = self.state.media_pool.clone();
        drop(pool);
        self.sync_log_formats_to_decoders();
    }

    /// Tell the decoders each file's camera log format. The decoder is what
    /// turns log footage into scene-linear (curve + camera gamut), and
    /// container metadata never says "S-Log3" — the media's format (the
    /// user's choice, else what was detected) is the only source of truth.
    fn sync_log_formats_to_decoders(&self) {
        // Proxies and skim files are transcodes that keep the source's
        // encoding (8-bit, still log), and timeline playback or the browser
        // may read them instead of the original, so they carry the media's
        // format too. (PQ/HLG skims are tone-mapped to SDR by ffmpeg, but
        // PQ/HLG never enter this registry: their files say what they are.)
        let changed = klipp_engine::video_decoder::set_media_log_formats(
            self.state.media_pool.all().flat_map(|m| {
                let f = m.effective_log_format();
                std::iter::once(m.path.clone())
                    .chain(m.proxy_path.clone())
                    .chain(m.skim_file.clone())
                    .map(move |p| (p, f))
            }),
        );
        if changed {
            let _ = self.cmd_tx.send(AppCommand::MediaDecodeChanged);
        }
    }

    pub(super) fn send_commands(&mut self, cmds: Vec<AppCommand>) {
        if !cmds.is_empty() {
            self.state.media_usage_cache_dirty = true;
        }
        let mut needs_refresh = false;
        // Track whether any SetNodeParam arrived this batch. If so, take the
        // fast refresh path (scope preview only) instead of the full
        // sync_and_refresh_frame — the engine picked up the changes via the
        // lock-free param_change_queue, so we don't need to flatten the
        // sequence or take the shared RwLock's write lock on the slider path.
        let mut node_param_scrub = false;
        for cmd in cmds {
            // A path drag the command doesn't belong to is closed as its own
            // step before any handler runs (M3 step 2).
            self.state.before_dispatch(&cmd);
            if self.handle_node_thumbnail_request(&cmd) { continue; }
            // Several clips selected and the Inspector's clip edited: the
            // others take the change too (#843).
            let multi = self.multi_edit_pre(&cmd);
            // Edits whose handlers change the tracks in place without
            // recording it: the tracks either side, one undo step (#907).
            let tracks_before = crate::tracks_undo::before(&self.state, &cmd);
            'dispatch: {
                if matches!(&cmd, AppCommand::SetNodeParam { .. }) {
                    node_param_scrub = true;
                }
                // Timeline markers are edited outside the document's own
                // ops, so their handlers record nothing: a marker added,
                // changed or removed (by hand or by an agent) is one undo step
                // here, the whole list either side.
                let markers_before = self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.markers.clone());
                if self.handle_marker_commands(&cmd, &mut needs_refresh) {
                    if let Some(old) = markers_before {
                        let new = self.state.sequences[self.state.active_sequence_idx].markers.clone();
                        if old != new {
                            self.state.push_history(EditCommand::SetMarkers { old, new });
                        }
                    }
                    break 'dispatch;
                }
                if self.handle_storyline_commands(&cmd) { break 'dispatch; }
                if self.handle_frame_glide_commands(&cmd) { break 'dispatch; }
                if self.handle_track_subject_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_transition_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_preview_commands(&cmd) { break 'dispatch; }
                if self.handle_playback_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_transition_key_commands(&cmd) { break 'dispatch; }
                if self.handle_edit_point_commands(&cmd) { break 'dispatch; }
                if self.handle_clip_move_commands(&cmd) { break 'dispatch; }
                if self.handle_transcription_commands(&cmd) { break 'dispatch; }
                if self.handle_audition_commands(&cmd) { break 'dispatch; }
                if self.handle_export_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_browser_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_stabilization_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_keyframe_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_clip_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_source_edit_commands(&cmd) { break 'dispatch; }
                if self.handle_edit_helper_commands(&cmd) { break 'dispatch; }
                if self.handle_timeline_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_effects_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_graph_ops_commands(&cmd) { break 'dispatch; }
                if self.handle_path_commands(&cmd) { break 'dispatch; }
                if self.handle_color_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_title_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_project_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_media_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_visual_search_commands(&cmd) { break 'dispatch; }
                // Clip operations — handled by handle_clip_commands
                // Timeline operations — handled by handle_timeline_commands
                // Track seek commands for filmstrip preview
                if matches!(&cmd, AppCommand::SeekTo(_) | AppCommand::SeekRelative(_)) {
                    self.state.awaiting_seek_frame = true;
                }
                let _ = self.cmd_tx.send(cmd);
            }
            if let Some(before) = tracks_before {
                crate::tracks_undo::record(&mut self.state, before);
            }
            if let Some(post) = multi {
                self.multi_edit_post(post);
            }
            // A split compare on a node the command took away ends (M3 step
            // 4): the compare is by node, so nothing else moves it.
            if self.state.split_compare_gone() {
                let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id: None, path: None });
            }
        }
        // Prefer the full-sync path when ANY non-slider command needed refresh
        // (structural changes, keyframe edits, etc.). The slider fast path is
        // only taken when SetNodeParam was the sole trigger for refresh.
        // While a node-param-edit gesture is in flight (mask drag etc.),
        // throttle the heavy sync to ~30 Hz — without this the per-frame
        // SetNodeMasks during a mask drag fires sync_sequence_to_engine +
        // SequenceUpdated at 60+ Hz which floods the engine command channel
        // and visibly drifts the playhead.
        if needs_refresh {
            let in_gesture = self.node_param_edit_snapshot.is_some();
            let throttle_hit = in_gesture
                && self.last_gesture_sync.elapsed() < std::time::Duration::from_millis(33);
            if !throttle_hit {
                if in_gesture {
                    self.last_gesture_sync = std::time::Instant::now();
                }
                self.sync_and_refresh_frame();
            } else {
                self.refresh_scope_preview_fast();
            }
        } else if node_param_scrub {
            // Slider scrub only — engine drains param_change_queue itself, so
            // we just need to refresh the scope preview on the UI thread.
            self.refresh_scope_preview_fast();
        }
    }
}

/// The same engine mirror write used by the live inspector edit host.
pub(super) fn sync_sequence(state: &AppState, sequence: &klipp_timeline::SharedSequence, param_change_queue: &ArrayQueue<ParamChange>) {
    // Parameter changes still queued for the engine are already in
    // the mirror this pushes; left in the queue they would land
    // *after* it and overwrite it — an undo of a grade change lost
    // to the change itself, seen over the control socket and
    // possible for a fast ctrl+z too. The queue is multi-consumer.
    while param_change_queue.pop().is_some() {}
    // The sequence the engine renders from is built here, with nothing
    // locked, and put in place whole (#690). It used to be written in
    // place under a read-write lock the engine read for every frame,
    // and a busy frame kept the UI waiting; once, after a project
    // opened, for over a minute.
    let active = &state.sequences[state.active_sequence_idx];
    // Flatten compound clips so the engine receives a plain sequence
    // with no ClipKind::Sequence clips — the engine has no compound-clip
    // awareness. This mirrors the export path exactly: viewer and export
    // are consistent by construction.
    // Each template's and precomposition's scene from the cache when
    // nothing it is made from changed (#3117).
    let mut flat = klipp_timeline::flatten::flatten_sequence_cached(active, &state.sequences, &mut state.flatten_cache.borrow_mut());
    // A command in the Colour workspace can sync while the timeline
    // grade stands in as a clip: the engine gets the grade, never the
    // stand-in.
    settle_timeline_grade_proxy(&mut flat, None);
    // The clip being framed for its glide, without its move (#611).
    super::frame_glide::show_unmoved(&mut flat, state.frame_glide_clip);
    sequence.replace(flat);
}

#[cfg(test)]
mod engine_send_tests {
    /// #437: every command the UI sends straight to the playback engine is
    /// one the engine acts on. Read from the source, so a send on a path no
    /// test runs fails here rather than doing nothing in the app.
    #[test]
    fn every_command_sent_to_the_engine_is_one_it_acts_on() {
        fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push((path.clone(), std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut sent = 0;
        let mut astray = Vec::new();
        for (path, text) in &files {
            for marker in ["cmd_tx.send(AppCommand::", "cmd_tx.send(klipp_core::AppCommand::", "side.send(AppCommand::"] {
                for (at, _) in text.match_indices(marker) {
                    let name: String = text[at + marker.len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    // This test's own markers name nothing.
                    if name.is_empty() {
                        continue;
                    }
                    sent += 1;
                    if !klipp_engine::commands::NAMES.contains(&name.as_str()) {
                        let line = text[..at].lines().count();
                        astray.push(format!("{}:{line} sends {name}", path.display()));
                    }
                }
            }
        }
        assert!(sent > 100, "the scan found only {sent} sends: has the field been renamed?");
        assert!(astray.is_empty(), "sent to the engine, which does nothing with them — send them through send_commands:\n{}", astray.join("\n"));
    }
}
