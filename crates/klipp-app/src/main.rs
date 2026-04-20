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
use tracing::info;
use tracing_subscriber::EnvFilter;

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
    // Suppress ffmpeg's stderr output (VAAPI init failures, thread count warnings, CUDA errors).
    // All important decode info is captured through our own tracing system.
    // AV_LOG_FATAL = only show unrecoverable crashes.
    unsafe { ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_FATAL as i32) };

    // Log to klipp-debug.log, truncated on each launch
    let log_file = std::fs::File::create("klipp-debug.log")
        .expect("Failed to create klipp-debug.log");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("klipp=debug,wgpu=warn,naga=warn")),
        )
        .with_writer(log_file)
        .with_ansi(false)
        .init();

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
        default_hook(panic_info);
    }));

    mark("main entered (logging up)");
    info!("Klipp starting up");

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();

    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
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

    let mut engine = {
        let _t = LogTimer::new("PlaybackEngine::new (TOTAL)");
        PlaybackEngine::new(
            cmd_rx,
            event_tx,
            frame_buffer.clone(),
            sequence.clone(),
            media_pool.clone(),
            Box::new(|| {}),
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

    // Load window icon from embedded PNG
    let icon_data = {
        let _t = LogTimer::new("decode app icon PNG");
        let png_bytes = include_bytes!("../../../assets/icons/klipp_app_icons/png/dark/klipp-dark-256.png");
        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes.as_slice()));
        let mut reader = decoder.read_info().expect("Failed to decode app icon PNG");
        let mut buf = vec![0u8; reader.output_buffer_size()];
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
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let cmd_tx_clone = cmd_tx.clone();
    let fb_clone = frame_buffer.clone();
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
            Ok(Box::new(klipp_ui::KlippApp::new(
                cc,
                cmd_tx_clone,
                event_rx,
                fb_clone,
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
