use crate::state::AppState;
use crate::theme::KlippTheme;

/// Show the Project Info window. Returns true if still open.
pub fn show_project_info(
    ctx: &egui::Context,
    state: &AppState,
    theme: &KlippTheme,
    open: &mut bool,
) {
    egui::Window::new("Project Info")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);

            let project_name = state.project_path.as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into());

            // Project name
            info_row(ui, theme, "Project", &project_name);

            // Location
            if let Some(ref path) = state.project_path {
                info_row(ui, theme, "Location", &path.to_string_lossy());
            }

            ui.separator();

            // Created
            if let Some(ref created) = state.created_at {
                info_row(ui, theme, "Created", format_date(created));
            }

            // Sequences
            info_row(ui, theme, "Sequences", &state.sequences.len().to_string());

            // Active sequence info
            let seq = state.sequence();
            info_row(ui, theme, "Active Sequence", &seq.name);

            // Duration
            let dur = state.sequence_duration().to_secs_f64();
            let mins = (dur / 60.0).floor() as u32;
            let secs = (dur % 60.0).floor() as u32;
            let frames = ((dur % 1.0) * seq.fps.rate() as f64).floor() as u32;
            info_row(ui, theme, "Duration", &format!("{mins}:{secs:02}.{frames:02}"));

            // Resolution
            info_row(
                ui, theme, "Resolution",
                &format!("{}x{}", seq.resolution.width, seq.resolution.height),
            );

            // Frame rate
            let fps_str = if seq.fps.den == 1 {
                format!("{} fps", seq.fps.num)
            } else {
                format!("{:.3} fps", seq.fps.num as f64 / seq.fps.den as f64)
            };
            info_row(ui, theme, "Frame Rate", &fps_str);

            ui.separator();

            // Media
            let media_count = state.media_pool.len();
            let total_size: u64 = state.media_pool.all()
                .map(|m| m.file_size_bytes)
                .sum();
            info_row(ui, theme, "Media Items", &media_count.to_string());
            info_row(ui, theme, "Total Media Size", &format_size(total_size));

            // Track count
            let video_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
            let audio_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            info_row(ui, theme, "Tracks", &format!("{video_tracks}V + {audio_tracks}A"));

            // Clip count
            let clip_count: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
            info_row(ui, theme, "Clips", &clip_count.to_string());
        });
}

fn info_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label}:"))
                .size(12.0)
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(value)
                .size(12.0)
                .color(theme.text_primary),
        );
    });
}

fn format_date(iso: &str) -> &str {
    // Show just the date portion (YYYY-MM-DD) from ISO 8601
    if iso.len() >= 10 { &iso[..10] } else { iso }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
