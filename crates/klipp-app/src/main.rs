use anyhow::Result;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use std::sync::{Arc, Mutex};
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // 1. Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("klipp=debug,wgpu=warn,naga=warn")),
        )
        .init();

    info!("Klipp starting up");

    // 2. Create channels
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();

    // 3. Create shared frame buffer
    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));

    // 4. Create and spawn engine
    let engine = PlaybackEngine::new(
        cmd_rx,
        event_tx,
        frame_buffer.clone(),
        Box::new(|| {}),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create playback engine: {e}"))?;

    let engine_handle = std::thread::Builder::new()
        .name("klipp-engine".into())
        .spawn(move || engine.run())?;

    info!("Engine thread spawned");

    // 5. Run eframe
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klipp")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let cmd_tx_clone = cmd_tx.clone();
    let fb_clone = frame_buffer.clone();

    eframe::run_native(
        "Klipp",
        options,
        Box::new(move |cc| {
            Ok(Box::new(klipp_ui::KlippApp::new(
                cc,
                cmd_tx_clone,
                event_rx,
                fb_clone,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    // 6. Clean shutdown
    info!("Klipp shutting down");
    let _ = cmd_tx.send(klipp_core::AppCommand::Quit);
    let _ = engine_handle.join();

    Ok(())
}
