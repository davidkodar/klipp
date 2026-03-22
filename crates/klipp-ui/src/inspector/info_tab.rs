use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    track_idx: usize,
    clip: &Clip,
    media: &klipp_media::MediaItem,
) {
    section_header(ui, theme, "Clip Info");

    info_row(ui, theme, "Resolution", &format!(
        "{}\u{00d7}{}",
        media.resolution.width, media.resolution.height
    ));
    info_row(ui, theme, "Frame rate", &format!("{} fps", media.fps.as_f64() as u32));

    let codec_str = if media.audio_codec.is_empty() {
        media.video_codec.clone()
    } else {
        format!("{} / {}", media.video_codec, media.audio_codec)
    };
    info_row(ui, theme, "Codec", &codec_str);

    let audio_str = if media.has_audio {
        format!("{} Stereo", media.audio_codec)
    } else {
        "None".into()
    };
    info_row(ui, theme, "Audio", &audio_str);

    info_row(ui, theme, "Duration", &clip.duration().to_timecode_string());

    // File size
    let file_size = media.file_size_bytes;
    let size_str = if file_size > 1_000_000_000 {
        format!("{:.1} GB", file_size as f64 / 1_000_000_000.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1_000_000.0)
    };
    info_row(ui, theme, "File size", &size_str);

    // Proxy
    let proxy_str = if media.proxy_ready {
        "Active"
    } else {
        "None"
    };
    let proxy_color = if media.proxy_ready {
        theme.accent
    } else {
        theme.text_primary
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new("Proxy")
                .size(11.0)
                .color(theme.text_secondary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(proxy_str)
                    .size(11.0)
                    .monospace()
                    .color(proxy_color),
            );
        });
    });
    ui.add_space(5.0);

    ui.add_space(14.0);

    // Timeline info
    section_header(ui, theme, "Timeline");
    info_row(ui, theme, "In", &clip.timeline_in.to_timecode_string());
    info_row(ui, theme, "Out", &clip.timeline_out.to_timecode_string());
    if let Some(track) = state.sequence().tracks.get(track_idx) {
        info_row(ui, theme, "Track", &track.name);
    }
}

fn section_header(ui: &mut egui::Ui, theme: &KlippTheme, label: &str) {
    ui.label(
        egui::RichText::new(label.to_uppercase())
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(7.0);
}

fn info_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new(label)
                .size(11.0)
                .color(theme.text_secondary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(11.0)
                    .monospace()
                    .color(theme.text_primary),
            );
        });
    });
    ui.add_space(5.0);
}
