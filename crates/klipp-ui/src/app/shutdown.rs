//! Shutting down: the GPU cleaned up before the window closes, child
//! processes killed, and the process left once the engine has stopped (#890).

use super::*;

impl KlippApp {
    /// Register (or re-register) `FrameBuffer.gpu_output` with
    /// eframe-wgpu's renderer and update `viewer.gpu_texture_id`.
    /// Called at the top of every `update()`; the `gpu_output_generation`
    /// check means we only actually touch the renderer when the
    /// engine produced a newly-sized texture.
    ///
    /// No-op when the engine hasn't published a gpu_output yet, or
    /// when eframe is running without a wgpu render state (glow
    /// fallback). Legacy CPU viewer path is untouched.
    /// Free GPU resources we registered with eframe-wgpu's renderer
    /// while we still have `frame: &mut eframe::Frame` access. Called
    /// from `update()` the moment we know we're going to send
    /// `ViewportCommand::Close`. Without this, `gpu_texture_id` lives
    /// in the renderer's BindGroup map until the renderer drops; the
    /// matching `gpu_texture_view` also lives until KlippApp drops;
    /// when the wgpu::Device tears down the order ends up wrong and
    /// the process SIGSEGVs in egui-wgpu's queue.submit.
    ///
    /// Idempotent — safe to call multiple times. After this runs,
    /// the viewer paints fall back to the CPU upload path until the
    /// app actually exits.
    /// Stop the engine, wait for it to finish releasing its GPU
    /// resources, then exit the process.
    ///
    /// Every close path used to call `process::exit(0)` directly. That
    /// runs libc's atexit handlers and terminates *without waiting for
    /// other threads*, and the NVIDIA GL driver registers teardown in
    /// those handlers — so exiting while the engine thread was still
    /// dropping its wgpu pipelines tore the device out from under it
    /// mid-call. A core dump caught it exactly there: engine thread in
    /// `destroy_pipeline` under `drop_glue<Option<GpuEffectsPipeline>>`,
    /// main thread down inside `libGLX_nvidia`'s exit path. The comment
    /// on the Don't-Save branch had already named this race; this is the
    /// handshake it was missing.
    ///
    /// `process::exit` itself stays. It is a deliberate mitigation for
    /// the separate eframe/wgpu drop-order SEGV (`BUG-SHUTDOWN-SEGV`)
    /// that survived eight rounds of leak-plugging, and removing it here
    /// would trade one crash for another.
    ///
    /// The wait is bounded: a wedged engine must not stop the user
    /// closing the window, and exiting anyway is no worse than the
    /// behaviour this replaces.
    pub(super) fn exit_after_engine_shutdown(&mut self) -> ! {
        // An install of the AAF converter stopped, its step killed, rather
        // than left running after Klipp (#2086).
        if let Some(install) = &self.state.aaf_install {
            install.stop(std::time::Duration::from_secs(1));
        }
        if let Some(reading) = &self.state.aaf_reading {
            reading.stop(std::time::Duration::from_secs(1));
        }
        // Before anything that can hang: a lock left behind makes the
        // next open ask a question nobody needs to answer.
        if let Some(keeper) = self.lock_keeper.take() {
            keeper.release_now();
        }
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(2000);
        let t0 = std::time::Instant::now();

        // Harmless if the engine is already shutting down or gone: a
        // closed channel just errors, and the flag is already set.
        let _ = self.cmd_tx.send(klipp_core::AppCommand::Quit);

        let mut confirmed = false;
        while t0.elapsed() < BUDGET {
            if klipp_engine::ENGINE_SHUTDOWN_COMPLETE
                .load(std::sync::atomic::Ordering::Acquire)
            {
                confirmed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if confirmed {
            tracing::info!("engine confirmed teardown in {ms:.1}ms — exiting");
        } else {
            tracing::warn!(
                "engine did not confirm teardown within {}ms — exiting anyway",
                BUDGET.as_millis(),
            );
        }

        // The engine being down does not mean its decode threads are.
        // `VideoDecoder::stop` waits half a second and then detaches, and
        // `stop_detached` does not wait at all — fine while the process
        // lives, fatal here: `exit` tears down libc under threads that
        // keep running, and a decode thread still inside ffmpeg's
        // teardown then crashes in it. Also bounded, for the same reason
        // the engine wait is.
        klipp_engine::video_decoder::wait_for_decode_threads(
            std::time::Duration::from_millis(1000),
        );
        // An export the quit cancelled (`kill_children_for_exit`) drops
        // its decoders and GPU converters on its own thread as it sees
        // the flag: waited for the same way, and as bounded.
        let t1 = std::time::Instant::now();
        while klipp_render::live_render_decoders() > 0 && t1.elapsed() < std::time::Duration::from_millis(1000) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if klipp_render::live_render_decoders() > 0 {
            tracing::warn!("{} render decoder(s) still alive after 1000ms — exiting anyway", klipp_render::live_render_decoders());
        }
        std::process::exit(0);
    }

    /// Kill child processes (proxy ffmpeg, render queue) before
    /// `process::exit(0)` skips Drop. Idempotent — safe to call from
    /// any of the close paths.
    pub(super) fn kill_children_for_exit(&mut self) {
        for pid_ref in &self.proxy_children {
            let pid = pid_ref.load(std::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        self.state.render_queue.kill_active();
    }

    /// Free GPU resources we registered with eframe-wgpu's renderer
    /// while we still have `frame: &mut eframe::Frame` access. Kept
    /// around — currently unused because the SHUTDOWN-SEGV
    /// workaround (`std::process::exit(0)`) bypasses the eframe
    /// close path entirely. When `BUG-SHUTDOWN-SEGV` gets a real
    /// fix this becomes the canonical pre-close hook.
    #[allow(dead_code)]
    fn pre_close_gpu_cleanup(&mut self, frame: &mut eframe::Frame) {
        tracing::info!("pre_close_gpu_cleanup: freeing GPU-side resources");
        let t0 = std::time::Instant::now();

        // 1) Free the eframe-wgpu native texture id + its view.
        if let Some(render_state) = frame.wgpu_render_state() {
            let mut renderer = render_state.renderer.write();
            if let Some(old_id) = self.viewer.gpu_texture_id.take() {
                renderer.free_texture(&old_id);
            }
        } else {
            self.viewer.gpu_texture_id = None;
        }
        self.viewer.gpu_texture_view = None;
        self.viewer.gpu_texture_gen = 0;

        // 2) Drop the engine's GPU output texture. The
        //    `frame_buffer.gpu_output: Option<Arc<wgpu::Texture>>` is
        //    shared with the engine; engine has shut down by the time
        //    we reach the second close pass, so it's safe to take.
        //    Dropping the Texture HERE (with wgpu device still
        //    alive) routes the deallocate through a healthy queue;
        //    leaving it for AppState drop later races teardown.
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.gpu_output = None;
            fb.gpu_output_width = 0;
            fb.gpu_output_height = 0;
            fb.gpu_output_generation = 0;
        }
        {
            let mut sfb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            sfb.gpu_output = None;
            sfb.gpu_output_width = 0;
            sfb.gpu_output_height = 0;
            sfb.gpu_output_generation = 0;
        }

        // 3) Drop every egui::TextureHandle we know about now (same
        //    set on_exit clears, but earlier so there's more
        //    headroom before wgpu starts tearing down).
        self.state.ai_tools_state.mask_overlay_texture = None;
        self.state.source_live_texture = None;
        self.state.browser_skim_texture = None;
        self.state.quick_look_texture = None;
        self.state.file_info_thumb = None;
        self.state.thumbnail_textures.clear();
        self.state.filmstrip_textures.clear();
        self.viewer.video_texture = None;
        self.viewer.title_overlay_texture = None;
        self.viewer.logo_texture = None;
        self.browser.clear_textures_for_shutdown();
        self.new_inspector.clear_textures_for_shutdown();
        for bucket in &mut self.state.deferred_texture_drops {
            bucket.clear();
        }

        tracing::info!(
            "pre_close_gpu_cleanup done in {:.1}ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
}

impl Drop for KlippApp {
    fn drop(&mut self) {
        // Kill proxy ffmpeg child processes
        for pid_ref in &self.proxy_children {
            let pid = pid_ref.load(std::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        // Kill render queue child processes
        self.state.render_queue.kill_active();
    }
}
