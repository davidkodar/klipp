use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // Initialize logging — set RUST_LOG=klipp=debug to see verbose output
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("klipp=info,wgpu=warn")),
        )
        .init();

    info!("Klipp starting up");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Klipp")
            .with_inner_size([1600.0, 900.0])
            .with_min_inner_size([1024.0, 600.0])
            .with_icon(load_icon()),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Klipp",
        options,
        Box::new(|cc| Ok(Box::new(klipp_ui::KlippApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

fn load_icon() -> egui::IconData {
    // TODO: embed actual icon from assets/icons/klipp.png at build time
    egui::IconData::default()
}
