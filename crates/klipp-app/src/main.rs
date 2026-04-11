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

    let engine = {
        let _t = LogTimer::new("PlaybackEngine::new (TOTAL)");
        PlaybackEngine::new(
            cmd_rx,
            event_tx,
            frame_buffer.clone(),
            sequence.clone(),
            media_pool.clone(),
            Box::new(|| {}),
            skim_consumer.clone(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create playback engine: {e}"))?
    };

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
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    info!("Klipp shutting down");
    let _ = cmd_tx.send(klipp_core::AppCommand::Quit);
    let _ = engine_handle.join();

    Ok(())
}
