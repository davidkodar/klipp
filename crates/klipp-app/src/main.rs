use anyhow::Result;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::sync::{Arc, Mutex, RwLock};
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // VAAPI on NVIDIA requires this before any ffmpeg calls
    unsafe { std::env::set_var("LIBVA_DRIVER_NAME", "nvidia") };

    ffmpeg_next::init().expect("Failed to initialize ffmpeg");

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

    info!("Klipp starting up");

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();

    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
    let sequence = Arc::new(RwLock::new(Sequence::new_default()));
    let media_pool = Arc::new(RwLock::new(MediaPool::new()));

    // Skim audio consumer — shared between engine (cpal callback) and app (UI thread)
    let skim_consumer: klipp_engine::audio_output::SkimConsumer =
        Arc::new(Mutex::new(None));

    let engine = PlaybackEngine::new(
        cmd_rx,
        event_tx,
        frame_buffer.clone(),
        sequence.clone(),
        media_pool.clone(),
        Box::new(|| {}),
        skim_consumer.clone(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create playback engine: {e}"))?;

    let engine_handle = std::thread::Builder::new()
        .name("klipp-engine".into())
        .spawn(move || engine.run())?;

    info!("Engine thread spawned");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klipp")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let cmd_tx_clone = cmd_tx.clone();
    let fb_clone = frame_buffer.clone();
    let seq_clone = sequence.clone();
    let pool_clone = media_pool.clone();
    let skim_clone = skim_consumer;

    eframe::run_native(
        "Klipp",
        options,
        Box::new(move |cc| {
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
