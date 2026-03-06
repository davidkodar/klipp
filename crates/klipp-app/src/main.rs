use anyhow::Result;
use klipp_engine::{FrameTexture, PlaybackEngine};
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

    // 3. Initialize wgpu
    let (device, queue) = pollster::block_on(init_wgpu())?;
    let device = Arc::new(device);
    let queue = Arc::new(queue);

    info!("wgpu device created");

    // 4. Create shared frame texture (1920x1080 initial)
    let frame_texture = Arc::new(Mutex::new(FrameTexture::new(&device, 1920, 1080)));

    // 5. Create and spawn engine
    let engine = PlaybackEngine::new(
        cmd_rx,
        event_tx,
        device.clone(),
        queue.clone(),
        frame_texture.clone(),
        Box::new(|| {}), // repaint callback set later
    )
    .map_err(|e| anyhow::anyhow!("Failed to create playback engine: {e}"))?;

    let engine_handle = std::thread::Builder::new()
        .name("klipp-engine".into())
        .spawn(move || engine.run())?;

    info!("Engine thread spawned");

    // 6. Run eframe
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klipp")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let cmd_tx_clone = cmd_tx.clone();
    let ft_clone = frame_texture.clone();

    eframe::run_native(
        "Klipp",
        options,
        Box::new(move |cc| {
            Ok(Box::new(klipp_ui::KlippApp::new(
                cc,
                cmd_tx_clone,
                event_rx,
                ft_clone,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    // 7. Clean shutdown
    info!("Klipp shutting down");
    let _ = cmd_tx.send(klipp_core::AppCommand::Quit);
    let _ = engine_handle.join();

    Ok(())
}

async fn init_wgpu() -> Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await;

    let adapter = match adapter {
        Some(a) => {
            info!("wgpu adapter: {:?}", a.get_info());
            a
        }
        None => {
            tracing::warn!("Vulkan adapter not found, trying GL fallback");
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..Default::default()
            });
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .ok_or_else(|| anyhow::anyhow!("No suitable GPU adapter found"))?
        }
    };

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("klipp-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create GPU device: {e}"))?;

    Ok((device, queue))
}
