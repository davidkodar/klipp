//! Klipp desktop binary.
//!
//! Wires the engine, media pool, and UI into a single process. The
//! real work lives in [`klipp_engine::PlaybackEngine`] and
//! [`klipp_ui::app::KlippApp`]; this binary is a thin orchestrator:
//!
//! 1. Install the tracing subscriber and start the startup timer.
//! 2. Pre-warm expensive probes (audio devices, OFX plugins, font
//!    registry) on background threads so the first frame doesn't
//!    block on them.
//! 3. Construct shared `Arc<RwLock<Sequence>>` and
//!    `Arc<RwLock<MediaPool>>` state.
//! 4. Spawn the playback engine on its own thread (crossbeam channel
//!    for commands, another for events).
//! 5. Launch the eframe/egui event loop with `KlippApp` as the
//!    `eframe::App` trait object.
//!
//! Nothing else. Commands funnel through `AppCommand`, events through
//! `AppEvent`, both defined in `klipp-core`.

use anyhow::Result;
use klipp_core::startup_timer::{mark, mark_process_start, LogTimer};
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::sync::{Arc, Mutex, RwLock};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Append a single message to klipp-debug.log (best-effort,
/// async-signal-safe-ish: open + write + close, no allocator
/// surprises). Used by both the panic hook and the native
/// signal handler so a hard crash still leaves a trace.
fn write_crash_marker(msg: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("klipp-debug.log")?;
    writeln!(f, "[CRASH-MARKER] {msg}")?;
    f.flush()?;
    Ok(())
}

/// Install handlers for SIGSEGV / SIGABRT / SIGBUS / SIGILL.
/// On a native crash we write a minimal marker (signal name +
/// best-effort backtrace from the `backtrace` crate is *not* used
/// here — allocator may be wedged) into klipp-debug.log, then
/// re-raise so the OS coredump path still runs. Without this a
/// segfault leaves zero trace.
fn install_native_crash_handler() {
    // Note: writing inside a signal handler is technically
    // restricted to async-signal-safe calls. `write(2)` and
    // `open(2)` are both signal-safe; `std::fs::OpenOptions`
    // boils down to those, but its error path can allocate.
    // For our purposes (debug build aiding diagnosis), the
    // best-effort write is fine even if it occasionally
    // double-faults; the raised default handler still runs.
    extern "C" fn handler(signum: libc::c_int) {
        let name = match signum {
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS  => "SIGBUS",
            libc::SIGILL  => "SIGILL",
            libc::SIGFPE  => "SIGFPE",
            _ => "SIG?",
        };
        // Async-signal-safe: write(2) directly to a path the
        // process can open in the handler. Truncated info but
        // enough to know "the app hit a native crash."
        let msg = format!(
            "[CRASH-MARKER] NATIVE_CRASH signal={name} ({signum}) — process aborting\n"
        );
        unsafe {
            let path = b"klipp-debug.log\0";
            let fd = libc::open(
                path.as_ptr() as *const _,
                libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT,
                0o644,
            );
            if fd >= 0 {
                let _ = libc::write(fd, msg.as_ptr() as *const _, msg.len());
                let _ = libc::close(fd);
            }
            // Restore default disposition and re-raise so the OS
            // generates a core dump and the parent shell sees the
            // expected exit status.
            libc::signal(signum, libc::SIG_DFL);
            libc::raise(signum);
        }
    }
    unsafe {
        let h = handler as *const () as libc::sighandler_t;
        libc::signal(libc::SIGSEGV, h);
        libc::signal(libc::SIGABRT, h);
        libc::signal(libc::SIGBUS,  h);
        libc::signal(libc::SIGILL,  h);
        libc::signal(libc::SIGFPE,  h);
    }
}

fn main() -> Result<()> {
    // Anchor T0 for STARTUP_PERF measurements as the very first thing in main.
    mark_process_start();

    // Kick off system font discovery on a background thread immediately, so the
    // ~5s scan runs in parallel with ffmpeg / engine / window init instead of
    // blocking the foreground startup path. The first call from any thread
    // (TitleRenderer::new in engine init, or the UI font picker) will reuse
    // this scan's result via the OnceLock inside klipp_title::font_discovery.
    klipp_title::font_discovery::discover_system_fonts_async();

    // VAAPI on NVIDIA requires this before any ffmpeg calls
    unsafe { std::env::set_var("LIBVA_DRIVER_NAME", "nvidia") };

    {
        let _t = LogTimer::new("ffmpeg::init");
        ffmpeg_next::init().expect("Failed to initialize ffmpeg");
    }
    // Route ffmpeg's own log channel through our tracing subscriber
    // so cuvid / hwaccel / decoder error messages land in
    // `klipp-debug.log` alongside the rest. Without this, ffmpeg's
    // diagnostics go to stderr (invisible when launched from a
    // GUI) — silently swallowing the actual cause of "decoder error:
    // Invalid argument" and similar rabbit holes. The default level
    // is WARNING so the log isn't spammed with INFO-level chatter
    // (codec-init details, container probe noise) while real
    // problems still come through.
    unsafe {
        ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_WARNING as i32);
        ffmpeg_next::ffi::av_log_set_callback(Some(klipp_engine::ffmpeg_log_to_tracing));
    }

    // Log to klipp-debug.log, truncated on each launch
    let log_file = std::fs::File::create("klipp-debug.log")
        .expect("Failed to create klipp-debug.log");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Crate names (underscore, not colon) so EnvFilter matches
                // `klipp_engine::...`, `klipp_ui::...`, etc. The bare
                // `klipp=debug` directive only matched `klipp::*`, which
                // silently dropped every PERF / ENGINE_TICK_SLOW /
                // UI_TICK_SLOW diagnostic in release builds. Explicitly
                // whitelist each crate we care about.
                EnvFilter::new(
                    "klipp_engine=debug,klipp_ui=debug,klipp_app=debug,\
                     klipp_core=debug,klipp_effects=debug,klipp_media=debug,\
                     klipp_color=debug,\
                     klipp_timeline=debug,klipp_jobs=debug,klipp_title=debug,\
                     klipp_nodes=debug,\
                     wgpu=warn,naga=warn",
                )
            }),
        )
        .with_writer(log_file)
        .with_ansi(false)
        .init();

    // Force RUST_BACKTRACE on so the panic hook below carries a real
    // stack — without this, release builds emit "Backtrace::capture
    // is disabled" and the log line above is useless. Set BEFORE the
    // panic hook installs so the first capture sees it.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        // SAFETY: main thread, before any threads spawn.
        unsafe { std::env::set_var("RUST_BACKTRACE", "full"); }
    }

    // Route Rust panics (from any thread) into klipp-debug.log via tracing.
    // Without this, `panic = "abort"` + stripped release symbols makes
    // background-thread panics invisible — stderr goes nowhere when the app
    // isn't launched from a terminal. Call `next` (the default hook) after
    // logging so panic output still reaches stderr for users who do run
    // from a terminal.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let message = panic_info.to_string();
        // RUST_BACKTRACE=1 (or "full") to include a symbolicated backtrace.
        let backtrace = std::backtrace::Backtrace::capture();
        tracing::error!(
            thread = thread_name,
            location = %location,
            "PANIC: {message}\nbacktrace:\n{backtrace}"
        );
        // Belt-and-braces: also write directly to klipp-debug.log in
        // case tracing's writer is wedged (panic mid-log-flush etc.).
        let _ = write_crash_marker(&format!(
            "PANIC thread={thread_name} at {location}: {message}\n{backtrace}\n"
        ));
        default_hook(panic_info);
    }));

    // Native crash handler — catches SIGSEGV / SIGABRT / SIGBUS /
    // SIGILL that hit C-side code (FFmpeg, NVDEC, wgpu drivers).
    // The Rust panic hook above does not see those because they
    // come in as POSIX signals before any unwinding. Without this,
    // a hard FFmpeg crash leaves zero trace in klipp-debug.log and
    // looks like the app "just disappeared."
    install_native_crash_handler();

    mark("main entered (logging up)");
    info!("Klipp starting up");

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();

    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
    // Source viewer gets its own frame buffer so its decoded frames
    // never collide with the program viewer's. Same struct, second
    // instance — UI's existing texture-upload code handles both.
    let source_frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
    let sequence = Arc::new(RwLock::new(Sequence::new_default()));
    let media_pool = Arc::new(RwLock::new(MediaPool::new()));

    // Skim audio consumer — shared between engine (cpal callback) and app (UI thread)
    let skim_consumer: klipp_engine::audio_output::SkimConsumer =
        Arc::new(Mutex::new(None));

    // Shared performance ring — engine writes one sample per
    // post-processed frame, UI reads for the overlay + session dump.
    let perf_ring = klipp_engine::perf::shared();

    // Shared foreground-activity signal — engine writes is_playing /
    // is_scrubbing on state transitions, UI bumps last_input_at_us
    // on every input event, the background JobScheduler reads to
    // pause/resume filmstrip + skim transcode + audio conform jobs
    // whenever the user is busy. Built here in main so engine and UI
    // share the same Arc.
    let foreground_activity = klipp_jobs::ForegroundActivity::new();

    // Cross-thread repaint handle. The engine calls `(repaint_cb)()`
    // on every state transition / FrameReady / seek-complete, but
    // there's no egui::Context at engine-creation time (eframe hasn't
    // booted yet). Stash it in an `Arc<OnceLock<Context>>` — empty
    // while the engine is warming up, filled by the eframe creator
    // callback below. Once installed, every engine wake-up cross-
    // thread calls `ctx.request_repaint()` so the UI doesn't sleep
    // through a PlaybackStarted or FrameReady event. Without this the
    // UI only redraws on input, so until the user moves the mouse
    // the viewer stays blank after Play.
    let repaint_ctx: std::sync::Arc<std::sync::OnceLock<egui::Context>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let repaint_ctx_for_engine = std::sync::Arc::clone(&repaint_ctx);
    let mut engine = {
        let _t = LogTimer::new("PlaybackEngine::new (TOTAL)");
        PlaybackEngine::new(
            cmd_rx,
            event_tx,
            frame_buffer.clone(),
            source_frame_buffer.clone(),
            sequence.clone(),
            media_pool.clone(),
            Box::new(move || {
                if let Some(ctx) = repaint_ctx_for_engine.get() {
                    ctx.request_repaint();
                }
            }),
            skim_consumer.clone(),
            perf_ring.clone(),
        )
    };
    engine.set_foreground_activity(Arc::clone(&foreground_activity));
    // Cloned again here so the move-into-closure for eframe below
    // gets its own handle.
    let foreground_for_ui = Arc::clone(&foreground_activity);

    // Grab the spectrum PCM handle BEFORE `engine` is moved into the engine
    // thread. The mix thread is already running inside the engine struct at
    // this point, so this is safe.
    let spectrum_buf = engine.spectrum_buf();
    let spectrum_sample_rate = engine.spectrum_sample_rate();
    let param_change_queue = engine.param_change_queue();

    let engine_handle = std::thread::Builder::new()
        .name("klipp-engine".into())
        .spawn(move || engine.run())?;

    mark("engine thread spawned");
    info!("Engine thread spawned");

    // Hang watchdog. Reads `ENGINE_TICK` and `UI_TICK` every 500 ms.
    // If either counter hasn't advanced in 2 s the owning thread is
    // stuck. Logs ERROR lines with the last-seen tick and (for the
    // engine) the last-entered `EngineStage` so we can correlate with
    // nearby PERF lines. No recovery action — this is strictly a
    // diagnostic signal for the "app froze, had to force-quit" class
    // of bug.
    //
    // The two counters are independent: a hard freeze can hit the
    // engine thread, the UI thread, or both. A soak log where only
    // `UI_WATCHDOG` fires tells a very different story than one where
    // only `ENGINE_WATCHDOG` fires, and both firing together points
    // at a shared-resource deadlock.
    std::thread::Builder::new()
        .name("klipp-watchdog".into())
        .spawn(|| {
            use std::sync::atomic::Ordering;
            let mut last_engine = 0u64;
            let mut last_engine_change = std::time::Instant::now();
            let mut engine_alerted = false;
            let mut last_ui = 0u64;
            let mut last_ui_change = std::time::Instant::now();
            let mut ui_alerted = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));

                let current_engine = klipp_engine::ENGINE_TICK.load(Ordering::Relaxed);
                if current_engine != last_engine {
                    if engine_alerted {
                        tracing::error!(
                            "ENGINE_WATCHDOG RECOVERED after {}ms — tick advanced {} -> {}",
                            last_engine_change.elapsed().as_millis(),
                            last_engine, current_engine,
                        );
                        engine_alerted = false;
                    }
                    last_engine = current_engine;
                    last_engine_change = std::time::Instant::now();
                } else if last_engine_change.elapsed() >= std::time::Duration::from_millis(2000) {
                    let stage_id = klipp_engine::ENGINE_STAGE.load(Ordering::Relaxed);
                    let gpu_stage_id = klipp_effects::gpu::GPU_STAGE.load(Ordering::Acquire);
                    tracing::error!(
                        "ENGINE_WATCHDOG STALLED {}ms at tick={} stage={} gpu_stage={} — engine thread is not iterating",
                        last_engine_change.elapsed().as_millis(),
                        last_engine,
                        klipp_engine::engine_stage_name(stage_id),
                        klipp_effects::gpu::gpu_stage_name(gpu_stage_id),
                    );
                    engine_alerted = true;
                }

                let current_ui = klipp_engine::UI_TICK.load(Ordering::Relaxed);
                if current_ui != last_ui {
                    if ui_alerted {
                        tracing::error!(
                            "UI_WATCHDOG RECOVERED after {}ms — tick advanced {} -> {}",
                            last_ui_change.elapsed().as_millis(),
                            last_ui, current_ui,
                        );
                        ui_alerted = false;
                    }
                    last_ui = current_ui;
                    last_ui_change = std::time::Instant::now();
                } else if last_ui_change.elapsed() >= std::time::Duration::from_millis(2000) {
                    let ui_stage_id = klipp_engine::UI_STAGE.load(Ordering::Acquire);
                    tracing::error!(
                        "UI_WATCHDOG STALLED {}ms at tick={} stage={} — UI thread (update) is not running",
                        last_ui_change.elapsed().as_millis(),
                        last_ui,
                        klipp_engine::ui_stage::name(ui_stage_id),
                    );
                    ui_alerted = true;
                }
            }
        })?;

    // Load window icon from embedded PNG
    let icon_data = {
        let _t = LogTimer::new("decode app icon PNG");
        let png_bytes = include_bytes!("../../../assets/icons/klipp_app_icons/hicolor/256x256/apps/klipp.png");
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes.as_slice()));
        let mut reader = decoder.read_info().expect("Failed to decode app icon PNG");
        // png 0.18: output_buffer_size returns Option<usize>. The
        // app icon is a known-good baked asset; if dimensions
        // overflow the host's usize we have bigger problems.
        let buf_size = reader.output_buffer_size()
            .expect("App icon PNG has dimensions that overflow usize");
        let mut buf = vec![0u8; buf_size];
        let info = reader.next_frame(&mut buf).expect("Failed to read app icon frame");
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
            png::ColorType::Rgb => buf[..info.buffer_size()]
                .chunks_exact(3)
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect(),
            _ => panic!("Unsupported icon PNG color type"),
        };
        egui::IconData {
            rgba,
            width: info.width,
            height: info.height,
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klipp")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(std::sync::Arc::new(icon_data))
            .with_app_id("klipp")
            .with_drag_and_drop(true),
        // Phase 1 of the GPU-residency plan: move the UI off the GL
        // backend onto wgpu so the engine's wgpu device (klipp-effects)
        // and the UI's renderer live on the same API. Later phases will
        // share a single device between them — for now they stay
        // independent, but the backend is unified.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let cmd_tx_clone = cmd_tx.clone();
    let fb_clone = frame_buffer.clone();
    let source_fb_clone = source_frame_buffer.clone();
    let seq_clone = sequence.clone();
    let pool_clone = media_pool.clone();
    let skim_clone = skim_consumer;
    let perf_ring_clone = perf_ring.clone();

    mark("calling eframe::run_native");
    eframe::run_native(
        "Klipp",
        options,
        Box::new(move |cc| {
            mark("eframe creator callback entered");
            let _t = LogTimer::new("KlippApp::new (TOTAL)");

            // Install the egui Context into the shared OnceLock so
            // the engine's `repaint_cb` can wake the UI from its own
            // thread. Without this, PlaybackStarted / FrameReady /
            // seek-complete events from the engine land in the
            // channel but the UI stays asleep until the user moves
            // the mouse — classic egui-idle symptom.
            let _ = repaint_ctx.set(cc.egui_ctx.clone());

            // Optionally install eframe-wgpu's device as the shared
            // `GpuContext` so the engine + decoder submit onto the
            // same wgpu device the UI is rendering on. This enables
            // the zero-copy viewer handoff, but on wgpu-22 Vulkan +
            // NVIDIA it deadlocks: engine readbacks call
            // `device.poll(...)`, and the shared queue is kept non-
            // empty forever by the UI thread's continuous egui
            // submits — our poll never returns, the engine thread
            // hangs at PostProcessReencode, and the app is unusable.
            //
            // Gated behind `KLIPP_SHARED_GPU_DEVICE=1` so the default
            // run keeps the separate-device behaviour. When wgpu /
            // egui-wgpu lifetime semantics get stronger and the poll
            // loop can make progress independent of UI pressure we
            // flip the default.
            let want_shared = std::env::var("KLIPP_SHARED_GPU_DEVICE").ok().as_deref() == Some("1");
            if want_shared {
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    let ctx = klipp_effects::gpu_context::GpuContext::from_existing(
                        std::sync::Arc::clone(&rs.device),
                        std::sync::Arc::clone(&rs.queue),
                    );
                    match klipp_effects::gpu_context::GpuContext::install_shared(ctx) {
                        Ok(()) => info!("GpuContext: installed eframe-wgpu device as shared"),
                        Err(_) => warn!(
                            "GpuContext: shared slot was already populated — \
                             engine will use its own device, viewer will use eframe's"
                        ),
                    }
                } else {
                    warn!("CreationContext has no wgpu_render_state — running without device unification");
                }
            }

            Ok(Box::new(klipp_ui::KlippApp::new(
                cc,
                cmd_tx_clone,
                event_rx,
                fb_clone,
                source_fb_clone,
                seq_clone,
                pool_clone,
                skim_clone,
                spectrum_buf,
                spectrum_sample_rate,
                perf_ring_clone,
                param_change_queue,
                foreground_for_ui,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    info!("Klipp shutting down");
    let _ = cmd_tx.send(klipp_core::AppCommand::Quit);
    let _ = engine_handle.join();

    Ok(())
}
