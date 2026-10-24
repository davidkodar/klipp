//! AI tool runs: the tick that dispatches and polls them, inference, a mask
//! applied to a clip, its overlay, and model downloads (#890).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    /// Open the consent dialog for a model identified by its stable
    /// id in the manifest. No network call yet; the user confirms in
    /// the dialog before any download starts. Surfaces a toast if
    /// the manifest doesn't contain the id (typo / version drift).
    pub fn request_model_download(&mut self, model_id: &str) {
        // Re-check cache first — if the model is already verified
        // locally there's nothing to download. Surfaces a toast so
        // the user gets feedback rather than silence.
        let manifest = match klipp_ml::load_effective() {
            Ok(m) => m,
            Err(e) => {
                self.state.toast.show(format!("Manifest error: {e}"));
                return;
            }
        };
        let Some(entry) = manifest.get(model_id) else {
            self.state.toast.show(format!("Unknown model id: {model_id}"));
            return;
        };
        if let Ok(cache) = klipp_ml::ModelCache::open() {
            // Downloaded only when every file that comes with it is too.
            if manifest.parts(entry).iter().all(|e| matches!(cache.is_cached(e), Ok(klipp_ml::CachedStatus::Verified))) {
                self.state.toast.show(
                    format!("'{}' is already downloaded.", entry.display_name),
                );
                return;
            }
        }
        self.state.model_download = crate::model_download::ModelDownloadState::Consenting {
            entry: entry.clone(),
        };
    }

    /// Drive the AI Tools dispatch state machine each frame.
    ///
    /// Drains `pending_run` / `pending_cancel`, polls the worker
    /// channel, and triggers a repaint when the inference state
    /// changes so the panel + viewer overlay refresh promptly.
    pub(super) fn tick_ai_tools(&mut self, ctx: &egui::Context) {
        if self.state.ai_tools_state.pending_cancel {
            self.state.ai_tools_state.inference.cancel();
            self.state.ai_tools_state.pending_cancel = false;
        }

        if self.state.ai_tools_state.pending_run {
            self.state.ai_tools_state.pending_run = false;
            self.dispatch_ai_inference(ctx);
        }

        if self.state.ai_tools_state.inference.poll() {
            // Inference left Running — clear the start-time stamp
            // so the panel's progress bar stops ticking. Done's
            // own duration comes from response.inference_ms.
            self.state.ai_tools_state.inference_start_time = None;
            // Record telemetry on the Idle → Done transition. poll()
            // returns true on every state change (Done / Failed /
            // Disconnected) but we only want successful runs.
            if let crate::ai_tools::InferenceState::Done { model_id, response } =
                &self.state.ai_tools_state.inference
            {
                let id = model_id.clone();
                let ms = response.inference_ms;
                let candidates = response.candidates.len();
                let model_display = self
                    .state
                    .ai_tools_registry
                    .info(&id)
                    .map(|i| i.display_name)
                    .unwrap_or_else(|| id.clone());
                self.state
                    .ai_tools_state
                    .model_telemetry
                    .entry(id.clone())
                    .or_default()
                    .record_run(ms);
                // Persist updated telemetry. The file is small
                // (well under 1 KB even with full telemetry across
                // every model) and inference itself takes hundreds
                // of ms, so the per-write cost is invisible. Save
                // failures are logged + swallowed.
                crate::ai_tools::persistence::save(
                    &crate::ai_tools::persistence::AiToolsConfig::from_state(
                        &self.state.ai_tools_state,
                    ),
                );
                // Push a Recent entry. Thumbnail texture is built
                // here while we still hold a reference to the
                // mask buffer; pushed at the front so the panel's
                // .iter().take(3) shows the freshest first.
                if let Some(task) = self.state.ai_tools_state.active_task {
                    let thumbnail = build_recent_thumbnail(
                        ctx,
                        &self.state.ai_tools_state.inference,
                    );
                    let entry = crate::ai_tools::RecentEntry {
                        task,
                        model_id: id,
                        model_display_name: model_display,
                        timestamp: std::time::SystemTime::now(),
                        duration_ms: ms,
                        candidates_count: candidates,
                        thumbnail,
                        prompts: self.state.ai_tools_state.prompts.clone(),
                    };
                    self.state.ai_tools_state.recent_inferences.push_front(entry);
                    while self.state.ai_tools_state.recent_inferences.len() > 20 {
                        self.state.ai_tools_state.recent_inferences.pop_back();
                    }
                }
            }
            self.refresh_ai_tools_overlay_texture(ctx);
            ctx.request_repaint();
        }

        // Drain the panel's Re-run button. Resets inference back
        // to Idle while keeping the captured prompts so the user
        // can tweak + re-fire. Defers the overlay texture drop
        // through the wgpu-safe path.
        if self.state.ai_tools_state.pending_rerun {
            self.state.ai_tools_state.pending_rerun = false;
            self.state.ai_tools_state.inference =
                crate::ai_tools::InferenceState::Idle;
            self.state.ai_tools_state.inference_start_time = None;
            self.refresh_ai_tools_overlay_texture(ctx);
            ctx.request_repaint();
        }

        // Drain the panel's Apply-to-clip button. Writes the mask
        // PNG to the cache + inserts an ExternalMatte node into
        // the chosen clip's node graph.
        if self.state.ai_tools_state.pending_apply_to_clip {
            self.state.ai_tools_state.pending_apply_to_clip = false;
            self.execute_apply_mask_to_clip();
        }

        // Drive the custom-URL paste state machine. Worker thread
        // delivers progress + completion events; transitioning to
        // Confirming repaints so the modal flips into the SHA/size
        // confirmation view.
        if self.state.custom_model_add.poll() {
            ctx.request_repaint();
        }
    }

    /// Apply the most recent successful mask to a clip's node
    /// graph. Target clip selection follows the
    /// selected-then-playhead-fallback rule:
    ///   1. `state.selected_clip` if set and resolvable.
    ///   2. Otherwise the topmost video track's clip at the
    ///      playhead (egui's `find_map` order matches our
    ///      bottom-up track numbering — first match is highest).
    ///   3. Otherwise toast "No clip selected".
    ///
    /// On success: writes the mask to a metadata-rich filename in
    /// `~/.cache/klipp/ai_masks/`, inserts an `ExternalMatte`
    /// node into the clip's graph wired into the existing
    /// Corrector's matte input (port 1) when one exists, and
    /// toasts "Mask applied to {track} · {clip_short}".
    fn execute_apply_mask_to_clip(&mut self) {
        // Cut Out (#73): the mask is the alpha the clip's grade leaves
        // with, rather than the gate of its correction.
        let cut_out = std::mem::take(&mut self.state.ai_tools_state.pending_cut_out);
        // Snapshot what we need from inference state without
        // holding the borrow into mutating ops.
        let (mask, model_id) = match &self.state.ai_tools_state.inference {
            crate::ai_tools::InferenceState::Done { model_id, response }
                if !response.candidates.is_empty() =>
            {
                (response.candidates[0].mask.clone(), model_id.clone())
            }
            _ => {
                self.state.toast.show("No mask to apply");
                return;
            }
        };

        // Resolve target clip. Selected first, playhead fallback.
        let playhead = self.state.playhead;
        let target = self.state.selected_clip_loc().map(|(t, c)| (t, c.id, c.clone()));
        let target = target.or_else(|| {
            self.state
                .sequence()
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|(i, t)| {
                    t.clip_at_timecode(playhead).map(|(_, c)| (i, c.id, c.clone()))
                })
        });
        let Some((track_idx, clip_id, clip_snapshot)) = target else {
            self.state.toast.show(
                "No clip selected. Select a clip or position the playhead over one.",
            );
            return;
        };

        // Resolve the cache directory + a metadata-rich filename.
        // Schema: <clip_id>_<frame>_<model_id_hash>_<timestamp>.png
        // — gives a future cleanup pass enough info to identify
        // orphans (clip_id parsed against current project) and
        // dupes (clip+frame+model triple) without re-hashing.
        let cache_dir = match dirs::cache_dir() {
            Some(d) => d.join("klipp").join("ai_masks"),
            None => {
                self.state.toast.show("Apply failed: no cache dir");
                return;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            self.state.toast.show(format!("Apply failed: mkdir cache ({e})"));
            return;
        }
        let frame = playhead.value;
        let model_hash: u64 = {
            // 64-bit FNV-1a — stable, no extra dep, short enough
            // to keep the filename readable.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in model_id.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!(
            "{}_{frame}_{:016x}_{timestamp}.png",
            clip_id.to_string().split('-').next().unwrap_or("clip"),
            model_hash,
        );
        let final_path = cache_dir.join(&filename);
        let tmp_path = cache_dir.join(format!("{filename}.tmp"));

        // Encode mask as a single-channel grayscale PNG via the
        // image crate. Atomic write: encode to .tmp then rename
        // so a crash mid-write doesn't leave a half-written file
        // referenced by the node.
        let img = image::GrayImage::from_raw(
            mask.width,
            mask.height,
            mask.alpha.clone(),
        );
        let img = match img {
            Some(i) => i,
            None => {
                self.state.toast.show("Apply failed: mask buffer dimensions mismatch");
                return;
            }
        };
        if let Err(e) = img.save_with_format(&tmp_path, image::ImageFormat::Png) {
            self.state.toast.show(format!("Apply failed: PNG write ({e})"));
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            self.state.toast.show(format!("Apply failed: rename ({e})"));
            return;
        }
        // TODO(cleanup): orphan detection — the schema in this
        // filename gives a future cleanup pass enough info to
        // prune AI mask cache entries whose clip_id is no longer
        // referenced. v1 ships a 2GB LRU sweep on app start
        // (klipp-app::main) as the disk-pressure safety net.

        // Insert the ExternalMatte node into the clip's graph and
        // wire it into the existing Corrector's matte input
        // (port 1) when one exists: the executor runs a correction
        // gated there through an ApplyMatte, so the grade reaches only
        // the mask (#440). If not, the matte hangs in the graph and the
        // user can wire it manually from the V2 editor — still better
        // than no apply.
        let track_name = self
            .state
            .sequence()
            .tracks
            .get(track_idx)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| format!("Track {track_idx}"));
        let _ = clip_snapshot; // we mutate via sequence_mut below

        let path_for_node = final_path.clone();
        let mut wired = false;
        let mut taken: Option<String> = None;
        if let Some(track) = self.state.sequence_mut().tracks.get_mut(track_idx) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                let matte_id = klipp_nodes::NodeId::new_v4();
                let mut params = klipp_nodes::params::NodeParams::default();
                params.matte_image_path = Some(path_for_node);
                params.matte_channel =
                    Some(klipp_nodes::params::MatteChannel::Luminance);
                clip.edit_grade(|graph| {
                    graph.nodes.push(klipp_nodes::graph::Node {
                        id: matte_id,
                        kind: klipp_nodes::kinds::NodeKind::ExternalMatte,
                        label: "AI mask".into(),
                        enabled: true,
                        params,
                        keyframes: klipp_nodes::params::NodeKeyframeTracks::default(),
                        output_link: None,
                        input_links: Vec::new(),
                    });
                    // Cut out: into the Output's alpha (port 1), a matte
                    // already there replaced (#73, #898).
                    if cut_out {
                        let out = graph.output_node_id;
                        // Another matte already the alpha: refused below,
                        // by name, with the mask left unwired (#73).
                        if let Some(other) = graph.input_of(out, 1) {
                            taken = graph.nodes.iter().find(|n| n.id == other).map(|n| if n.label.is_empty() { n.kind.display_name().to_string() } else { n.label.clone() });
                            return;
                        }
                        graph.edges.retain(|e| !(e.to_node == out && e.to_port == 1));
                        graph.edges.push(klipp_nodes::graph::Edge { from_node: matte_id, from_port: 0, to_node: out, to_port: 1 });
                        wired = true;
                        return;
                    }
                    // Find the first Corrector node — that's where the
                    // default-graph's grade-with-matte expects the
                    // matte input to feed (port 1).
                    let corrector_id = graph
                        .nodes
                        .iter()
                        .find(|n| {
                            matches!(n.kind, klipp_nodes::kinds::NodeKind::Corrector)
                        })
                        .map(|n| n.id);
                    if let Some(corrector_id) = corrector_id {
                        // Drop any existing matte edge into the
                        // corrector's port 1 so the new mask wins.
                        graph
                            .edges
                            .retain(|e| !(e.to_node == corrector_id && e.to_port == 1));
                        graph.edges.push(klipp_nodes::graph::Edge {
                            from_node: matte_id,
                            from_port: 0,
                            to_node: corrector_id,
                            to_port: 1,
                        });
                        wired = true;
                    }
                });
            }
        }

        let clip_short = clip_id
            .to_string()
            .split('-')
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "clip".into());
        if let Some(name) = taken {
            self.state.toast.show(format!("The grade's alpha already takes \u{201c}{name}\u{201d}: the mask was added unwired; unwire that one from the Output to cut out by it"));
        } else if wired && cut_out {
            self.state.toast.show(format!("{track_name} · {clip_short} cut out by the mask"));
        } else if wired {
            self.state.toast.show(format!(
                "Mask applied to {track_name} · {clip_short}",
            ));
        } else {
            self.state.toast.show(format!(
                "Mask added to {track_name} · {clip_short} (no Corrector found — \
                 wire the matte from the V2 editor)",
            ));
        }
    }

    /// Rebuild the cached mask-overlay texture to match the current
    /// inference state. Routes the previous handle through the
    /// deferred-drop ring so wgpu doesn't see a destroyed texture
    /// mid-frame.
    fn refresh_ai_tools_overlay_texture(&mut self, ctx: &egui::Context) {
        // Use the multi-detection compositor so RF-DETR-Seg's many
        // candidates render in distinct palette colors. Falls back
        // to the single-color path automatically when there's only
        // one candidate (BiRefNet, MobileSAM single-click).
        let candidates = self.state.ai_tools_state.inference.candidates();
        let new_tex = if candidates.is_empty() {
            None
        } else {
            crate::ai_tools::build_multi_detection_texture(
                ctx,
                candidates,
                self.state.ai_tools_state.mask_overlay_color,
                "ai_tools_mask_overlay",
            )
        };
        let old = std::mem::replace(
            &mut self.state.ai_tools_state.mask_overlay_texture,
            new_tex,
        );
        if let Some(h) = old {
            self.defer_drop_texture(h);
        }
    }

    /// Build an `OwnedMaskRequest` from the current viewer frame +
    /// captured prompts and hand it to a worker thread.
    ///
    /// Holds the `frame_buffer` mutex only long enough to copy the
    /// raw sRGB u8 RGBA bytes out (cheap `memcpy`) — sRGB→linear
    /// f32 conversion runs on the worker thread so a 4K frame
    /// (~30 M `powf` calls) doesn't freeze the UI for several
    /// seconds. Watchdog hit this as `stage=AiToolsTick` 30 s+
    /// stalls when both linearisation AND ML inference were on the
    /// UI thread under one lock.
    fn dispatch_ai_inference(&mut self, ctx: &egui::Context) {
        let Some(model_id) = self.state.ai_tools_state.selected_model_id.clone() else {
            self.state.toast.show("AI Tools: pick a model first");
            return;
        };
        if self.state.ai_tools_state.inference.is_running() {
            return;
        }

        // Snapshot raw u8 RGBA — fast memcpy under the lock.
        let (w, h, srgb_bytes) = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
                self.state.toast.show("AI Tools: no frame loaded yet");
                return;
            }
            let expected = (fb.width as usize) * (fb.height as usize) * 4;
            if fb.data.len() < expected {
                self.state.toast.show("AI Tools: frame buffer not ready");
                return;
            }
            (fb.width, fb.height, fb.data[..expected].to_vec())
        };

        let prompts = self.state.ai_tools_state.prompts.build();
        // Per-model overrides (RF-DETR's score / NMS thresholds when
        // the user has nudged the panel sliders). Empty default for
        // models that don't expose any knobs — generators ignore
        // unrecognised fields.
        let tunables = self
            .state
            .ai_tools_state
            .model_tunables
            .get(&model_id)
            .cloned()
            .unwrap_or_default();
        let model_id_for_state = model_id.clone();
        let registry = self.state.ml_registry.clone();
        let ctx_for_worker = ctx.clone();
        let pixel_count = (w as usize) * (h as usize);

        // Spawn a "preparer" thread that does the heavy
        // linearisation off the UI thread, then hands the
        // OwnedMaskRequest to spawn_inference. This adds one extra
        // thread hop but keeps the UI tick under a millisecond.
        let (handle, _join) = {
            let (tx, rx) = crossbeam_channel::unbounded::<crate::ai_tools::InferenceEvent>();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancel_for_worker = cancel.clone();
            let join = std::thread::Builder::new()
                .name(format!("klipp-ml-prep-{model_id}"))
                .spawn(move || {
                    let mut linear = Vec::with_capacity(pixel_count * 4);
                    for chunk in srgb_bytes.chunks_exact(4) {
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[0] as f32 / 255.0));
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[1] as f32 / 255.0));
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[2] as f32 / 255.0));
                        linear.push(chunk[3] as f32 / 255.0);
                    }
                    if cancel_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = tx.send(crate::ai_tools::InferenceEvent::Failed("cancelled".into()));
                        ctx_for_worker.request_repaint();
                        return;
                    }
                    let req = crate::ai_tools::OwnedMaskRequest {
                        frame_pixels: std::sync::Arc::new(linear),
                        frame_width: w,
                        frame_height: h,
                        prompts,
                        temporal: None,
                        tunables,
                    };
                    // Run inference inline on this same prep thread —
                    // we already paid for the thread spawn, so a
                    // second hop would be wasteful. Channel + cancel
                    // mirror what spawn_inference does internally.
                    let _ = tx.send(crate::ai_tools::InferenceEvent::Started);
                    ctx_for_worker.request_repaint();
                    let event = crate::ai_tools::run_inference_blocking(&model_id, &req, &registry);
                    if cancel_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = tx.send(crate::ai_tools::InferenceEvent::Failed("cancelled".into()));
                    } else {
                        let _ = tx.send(event);
                    }
                    ctx_for_worker.request_repaint();
                })
                .expect("spawn klipp-ml prep thread");
            (
                crate::ai_tools::InferenceHandle { recv: rx, cancel },
                join,
            )
        };

        self.state.ai_tools_state.inference = crate::ai_tools::InferenceState::Running {
            model_id: model_id_for_state,
            frame_width: w,
            frame_height: h,
            handle,
        };
        // Stamp the start time so the panel's progress bar can
        // derive a fake percentage from elapsed-vs-telemetry-avg.
        // Cleared in the poll path when inference resolves.
        self.state.ai_tools_state.inference_start_time =
            Some(std::time::Instant::now());
    }
}

/// The AI tools task an engine event names, as the AI tools panel's own
/// task: the event carries a plain tag so klipp-core need not know the
/// panel's types.
pub(super) fn ai_tools_task_from_tag(tag: klipp_core::events::AiToolsTaskTag) -> crate::ai_tools::AiTask {
    use klipp_core::events::AiToolsTaskTag;
    match tag {
        AiToolsTaskTag::RemoveBackground => crate::ai_tools::AiTask::RemoveBackground,
        AiToolsTaskTag::IsolateObject => crate::ai_tools::AiTask::IsolateObject,
        AiToolsTaskTag::DetectObjects => crate::ai_tools::AiTask::DetectObjects,
        AiToolsTaskTag::TrackObject => crate::ai_tools::AiTask::TrackObject,
    }
}
