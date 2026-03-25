use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    track_idx: usize,
    clip: &Clip,
    media: &klipp_media::MediaItem,
    tag_input: &mut String,
    cmds: &mut Vec<AppCommand>,
) {
    section_header(ui, theme, "Clip Info");

    info_row(ui, theme, "Resolution", &format!(
        "{}\u{00d7}{}",
        media.resolution.width, media.resolution.height
    ));
    if media.is_still {
        info_row(ui, theme, "Type", "Still image");
    } else {
        info_row(ui, theme, "Frame rate", &format!("{} fps", media.fps.as_f64() as u32));
    }

    let codec_str = if media.audio_codec.is_empty() {
        media.video_codec.clone()
    } else {
        format!("{} / {}", media.video_codec, media.audio_codec)
    };
    info_row(ui, theme, "Codec", &codec_str);

    if media.has_audio {
        let channels_str = match media.audio_channels {
            Some(1) => "Mono".to_string(),
            Some(2) => "Stereo".to_string(),
            Some(6) => "5.1 Surround".to_string(),
            Some(8) => "7.1 Surround".to_string(),
            Some(n) => format!("{n} ch"),
            None => "Unknown".to_string(),
        };
        let rate_str = match media.sample_rate {
            Some(r) => format!("{:.0} kHz", r as f64 / 1000.0),
            None => String::new(),
        };
        let audio_str = if rate_str.is_empty() {
            format!("{} {}", media.audio_codec, channels_str)
        } else {
            format!("{} {} {}", media.audio_codec, channels_str, rate_str)
        };
        info_row(ui, theme, "Audio", &audio_str);
    } else {
        info_row(ui, theme, "Audio", "None");
    }

    info_row(ui, theme, "Duration", &clip.duration().to_timecode_string());

    // File size
    let file_size = media.file_size_bytes;
    let size_str = if file_size > 1_000_000_000 {
        format!("{:.1} GB", file_size as f64 / 1_000_000_000.0)
    } else {
        format!("{:.1} MB", file_size as f64 / 1_000_000.0)
    };
    info_row(ui, theme, "File size", &size_str);

    // Shoot date (from container metadata or file mtime)
    let shoot_date = media.media_created_at.or(media.file_modified_at);
    if let Some(epoch_secs) = shoot_date {
        info_row(ui, theme, "Shoot date", &format_epoch_date(epoch_secs));
    }

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

    // Tags
    section_header(ui, theme, "Tags");
    if media.tags.is_empty() && tag_input.is_empty() {
        ui.label(
            egui::RichText::new("No tags")
                .size(10.0)
                .color(theme.text_dim),
        );
    }
    // Tag chips — horizontal wrap
    let mut tag_to_remove: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (i, tag) in media.tags.iter().enumerate() {
            egui::Frame::none()
                .fill(theme.bg_raised)
                .rounding(4.0)
                .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.label(
                            egui::RichText::new(tag.as_str())
                                .size(10.0)
                                .color(theme.text_primary),
                        );
                        if ui.small_button("\u{00d7}").clicked() {
                            tag_to_remove = Some(i);
                        }
                    });
                });
        }
    });
    if let Some(idx) = tag_to_remove {
        let mut new_tags = media.tags.clone();
        new_tags.remove(idx);
        cmds.push(AppCommand::SetMediaTags {
            media_id: media.id,
            tags: new_tags,
        });
    }

    // Tag input
    ui.add_space(4.0);
    let response = ui.add(
        egui::TextEdit::singleline(tag_input)
            .hint_text("Add tag...")
            .desired_width(ui.available_width())
            .font(egui::TextStyle::Small),
    );
    if response.lost_focus()
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
        && !tag_input.trim().is_empty()
    {
        let new_tag = tag_input.trim().to_lowercase();
        if !media.tags.contains(&new_tag) {
            let mut new_tags = media.tags.clone();
            new_tags.push(new_tag);
            cmds.push(AppCommand::SetMediaTags {
                media_id: media.id,
                tags: new_tags,
            });
        }
        tag_input.clear();
    }

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

/// Format Unix epoch seconds as "DD Mon YYYY, HH:MM".
fn format_epoch_date(epoch_secs: u64) -> String {
    let secs = epoch_secs;
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m) = (rem / 3600, (rem % 3600) / 60);
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let mut mo = 0usize;
    for md in &mdays {
        if d < *md { break; }
        d -= md;
        mo += 1;
    }
    format!("{} {} {y}, {h:02}:{m:02}", d + 1, months[mo.min(11)])
}
