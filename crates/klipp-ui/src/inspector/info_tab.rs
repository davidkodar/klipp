use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

/// State 1: Nothing selected — PROJECT, MEDIA, SEQUENCE, STORAGE
pub fn show_no_selection(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
) {
    let pad = 14.0;
    let row_h = 30.0;
    let seq = state.sequence();

    // ── PROJECT ──
    section_card(ui, "PROJECT", None, theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();

        let project_name = state.project_path.as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into());
        info_row_custom(ui, theme, pad, row_h, w, "Name", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                &project_name, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        // Created date
        let created_text = state.created_at.as_deref()
            .map(|iso| {
                let clean = iso.trim_end_matches('Z');
                chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%dT%H:%M:%S%.f")
                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%dT%H:%M:%S"))
                    .map(|dt| dt.format("%d %b %Y, %H:%M").to_string())
                    .unwrap_or_else(|_| iso.to_string())
            })
            .unwrap_or_else(|| "Unknown".into());
        info_row_custom(ui, theme, pad, row_h, w, "Created", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                &created_text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        // Last modified — use relative time from the most recent save
        let last_modified = state.project_path.as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let elapsed = t.elapsed().unwrap_or_default();
                let secs = elapsed.as_secs();
                if secs < 60 { "just now".into() }
                else if secs < 3600 { format!("{} mins ago", secs / 60) }
                else if secs < 86400 { format!("{} hours ago", secs / 3600) }
                else { format!("{} days ago", secs / 86400) }
            })
            .unwrap_or_else(|| "Unknown".into());
        info_row_custom(ui, theme, pad, row_h, w, "Last modified", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                &last_modified, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Format version", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "klipp-v2", egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Klipp version", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                "0.9.1", egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        // Project path (small, dim, padded)
        if let Some(ref path) = state.project_path {
            ui.add_space(4.0);
            egui::Frame::default()
                .outer_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .fill(theme.bg_raised)
                .rounding(4.0)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(path.to_string_lossy().as_ref())
                            .size(10.0)
                            .color(theme.text_dim),
                    );
                });
            ui.add_space(4.0);
        }
    });

    ui.add_space(8.0);

    // ── MEDIA ──
    section_card(ui, "MEDIA", None, theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();

        // Count media items
        let all_media: Vec<_> = state.media_pool.all().collect();
        let total_bin = all_media.len();
        let video_count = all_media.iter().filter(|m| !m.video_codec.is_empty()).count();
        let audio_count = all_media.iter().filter(|m| m.video_codec.is_empty() && m.has_audio).count();
        let total_duration_secs: f64 = all_media.iter().map(|m| m.duration.to_secs_f64()).sum();
        let dur_h = (total_duration_secs / 3600.0) as u32;
        let dur_m = ((total_duration_secs % 3600.0) / 60.0) as u32;
        let dur_s = (total_duration_secs % 60.0) as u32;
        let dur_text = format!("{dur_h}:{dur_m:02}:{dur_s:02}");
        let missing_count = state.offline_media_ids.len();
        let offline_count = all_media.iter().filter(|m| m.offline).count();

        // 2x2 stat grid
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                let card_w = (w - pad * 2.0 - 8.0) / 2.0;
                let card_h = 60.0;

                // Row 1
                ui.horizontal(|ui| {
                    // CLIPS IN BIN
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(card_w, card_h), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme.bg_raised);
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 14.0), egui::Align2::LEFT_CENTER,
                        "CLIPS IN BIN", egui::FontId::proportional(9.0), theme.text_dim,
                    );
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 32.0), egui::Align2::LEFT_CENTER,
                        format!("{total_bin}"), egui::FontId::proportional(18.0), theme.text_primary,
                    );
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 50.0), egui::Align2::LEFT_CENTER,
                        format!("{video_count} video \u{00B7} {audio_count} audio"), egui::FontId::proportional(9.0), theme.text_dim,
                    );

                    ui.add_space(8.0);

                    // TOTAL BIN DURATION
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(card_w, card_h), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme.bg_raised);
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 14.0), egui::Align2::LEFT_CENTER,
                        "TOTAL BIN DURATION", egui::FontId::proportional(9.0), theme.text_dim,
                    );
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 32.0), egui::Align2::LEFT_CENTER,
                        &dur_text, egui::FontId::proportional(18.0), theme.text_primary,
                    );
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 50.0), egui::Align2::LEFT_CENTER,
                        "hh:mm:ss", egui::FontId::proportional(9.0), theme.text_dim,
                    );
                });

                ui.add_space(6.0);

                // Row 2
                ui.horizontal(|ui| {
                    // MISSING MEDIA
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(card_w, card_h - 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme.bg_raised);
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 12.0), egui::Align2::LEFT_CENTER,
                        "MISSING MEDIA", egui::FontId::proportional(9.0), theme.text_dim,
                    );
                    let mc = if missing_count == 0 { theme.marker_success } else { theme.marker_error };
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 30.0), egui::Align2::LEFT_CENTER,
                        format!("{missing_count}"), egui::FontId::proportional(18.0), mc,
                    );

                    ui.add_space(8.0);

                    // OFFLINE CLIPS
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(card_w, card_h - 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 4.0, theme.bg_raised);
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 12.0), egui::Align2::LEFT_CENTER,
                        "OFFLINE CLIPS", egui::FontId::proportional(9.0), theme.text_dim,
                    );
                    let oc = if offline_count == 0 { theme.marker_success } else { theme.marker_error };
                    ui.painter().text(
                        egui::pos2(rect.min.x + 10.0, rect.min.y + 30.0), egui::Align2::LEFT_CENTER,
                        format!("{offline_count}"), egui::FontId::proportional(18.0), oc,
                    );
                });
            });
        ui.add_space(6.0);
    });

    ui.add_space(8.0);

    // ── SEQUENCE ──
    section_card(ui, "SEQUENCE", None, theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();

        info_row_custom(ui, theme, pad, row_h, w, "Resolution", |ui, rx, cy| {
            let text = format!("{} \u{00D7} {}", seq.resolution.width, seq.resolution.height);
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Frame rate", |ui, rx, cy| {
            let fps = seq.fps.num as f64 / seq.fps.den.max(1) as f64;
            let text = if (fps - fps.round()).abs() < 0.01 {
                format!("{}  fps", fps.round() as u32)
            } else {
                format!("{:.3}  fps", fps)
            };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Duration", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                state.sequence_duration().to_timecode_string(),
                egui::FontId::monospace(13.0), theme.accent,
            );
        });

        let video_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
        let audio_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
        let total_clips: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();

        info_row_custom(ui, theme, pad, row_h, w, "Video tracks", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format!("{video_tracks}"), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Audio tracks", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format!("{audio_tracks}"), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Total clips", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format!("{total_clips}"), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });
    });

    ui.add_space(8.0);

    // ── STORAGE ──
    section_card(ui, "STORAGE", None, theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();
        let project_bytes: u64 = state.media_pool.all().map(|m| m.file_size_bytes).sum();

        let cache_bytes: u64 = dirs::cache_dir()
            .map(|d| d.join("klipp"))
            .and_then(|p| dir_size_bytes(&p))
            .unwrap_or(0);

        let proxy_bytes: u64 = dirs::cache_dir()
            .map(|d| d.join("klipp").join("proxies"))
            .and_then(|p| dir_size_bytes(&p))
            .unwrap_or(0);

        let home = dirs::home_dir();
        let free_bytes: u64 = state.project_path.as_deref()
            .or(home.as_deref())
            .and_then(disk_free_bytes)
            .unwrap_or(0);

        info_row_custom(ui, theme, pad, row_h, w, "Project file", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_size(project_bytes), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Cache", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_size(cache_bytes), egui::FontId::proportional(13.0), theme.text_primary,
            );
        });

        info_row_custom(ui, theme, pad, row_h, w, "Proxies", |ui, rx, cy| {
            let text = if proxy_bytes > 0 { format_size(proxy_bytes) } else { "None generated".into() };
            let color = if proxy_bytes > 0 { theme.text_primary } else { theme.text_dim };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), color,
            );
        });

        let free_color = if free_bytes < 1_000_000_000 {
            theme.marker_error
        } else if free_bytes < 10_000_000_000 {
            theme.accent
        } else {
            theme.marker_success
        };
        info_row_custom(ui, theme, pad, row_h, w, "Free disk space", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format_size(free_bytes), egui::FontId::proportional(13.0), free_color,
            );
        });
    });

    ui.add_space(16.0);
}

/// State 2: Bin media selected — CLIP INFO, TAGS, TIMELINE USAGE
pub fn show_bin_media(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    media: &klipp_media::MediaItem,
    tag_input: &mut String,
    cmds: &mut Vec<AppCommand>,
) {
    let pad = 14.0;
    let row_h = 30.0;
    let accent = theme.accent;
    let dim = theme.text_dim;

    // ── CLIP INFO ──
    section_card(ui, "CLIP INFO", None, theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();

        info_row_custom(ui, theme, pad, row_h, w, "Resolution", |ui, rx, cy| {
            let text = format!("{} \u{00D7} {}", media.resolution.width, media.resolution.height);
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(13.0), theme.text_primary);
        });

        if !media.is_still {
            info_row_custom(ui, theme, pad, row_h, w, "Frame rate", |ui, rx, cy| {
                let fps = media.fps.as_f64();
                let text = if (fps - fps.round()).abs() < 0.01 { format!("{}  fps", fps.round() as u32) } else { format!("{:.3}  fps", fps) };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(13.0), theme.text_primary);
            });
        }

        // Codec pills
        info_row_custom(ui, theme, pad, row_h, w, "Codec", |ui, rx, cy| {
            let pill_h = 20.0;
            let pill_bg = theme.bg_raised;
            let pill_border = theme.border_light;
            let mut x = rx;
            if !media.audio_codec.is_empty() {
                let codec = media.audio_codec.to_uppercase();
                let galley = ui.painter().layout_no_wrap(codec.clone(), egui::FontId::monospace(11.0), theme.text_primary);
                let pw = galley.size().x + 12.0;
                let pr = egui::Rect::from_min_size(egui::pos2(x - pw, cy - pill_h / 2.0), egui::vec2(pw, pill_h));
                ui.painter().rect(pr, 4.0, pill_bg, egui::Stroke::new(1.0_f32, pill_border));
                ui.painter().text(pr.center(), egui::Align2::CENTER_CENTER, codec, egui::FontId::monospace(11.0), theme.text_primary);
                x -= pw + 6.0;
            }
            if !media.video_codec.is_empty() {
                let codec = media.video_codec.to_uppercase();
                let galley = ui.painter().layout_no_wrap(codec.clone(), egui::FontId::monospace(11.0), theme.text_primary);
                let pw = galley.size().x + 12.0;
                let pr = egui::Rect::from_min_size(egui::pos2(x - pw, cy - pill_h / 2.0), egui::vec2(pw, pill_h));
                ui.painter().rect(pr, 4.0, pill_bg, egui::Stroke::new(1.0_f32, pill_border));
                ui.painter().text(pr.center(), egui::Align2::CENTER_CENTER, codec, egui::FontId::monospace(11.0), theme.text_primary);
            }
        });

        // Audio
        if media.has_audio {
            info_row_custom(ui, theme, pad, row_h, w, "Audio", |ui, rx, cy| {
                let channels = media.audio_channels.unwrap_or(2);
                let sample_rate = media.sample_rate.unwrap_or(48000);
                let ch_label = match channels { 1 => "Mono", 2 => "Stereo", 6 => "5.1", 8 => "7.1", _ => "" };
                let sr_khz = sample_rate / 1000;
                let label = format!("{ch_label} \u{00B7} {sr_khz} kHz");
                let label_galley = ui.painter().layout_no_wrap(label.clone(), egui::FontId::proportional(12.0), dim);
                let label_w = label_galley.size().x;
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, &label, egui::FontId::proportional(12.0), dim);
                // Channel bars
                let bar_heights: &[f32] = match channels { 1 => &[0.8], 2 => &[0.9, 0.7], 6 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5], 8 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5, 0.7, 0.6], _ => &[0.8, 0.7] };
                let bar_w = 4.0; let bar_gap = 2.0; let max_bar_h = 14.0;
                let total_bars_w = bar_heights.len() as f32 * (bar_w + bar_gap) - bar_gap;
                let bx = rx - label_w - 10.0 - total_bars_w;
                for (i, &h_frac) in bar_heights.iter().enumerate() {
                    let bh = max_bar_h * h_frac;
                    ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(bx + i as f32 * (bar_w + bar_gap), cy - bh / 2.0), egui::vec2(bar_w, bh)), 1.0, accent);
                }
            });
        }

        info_row_custom(ui, theme, pad, row_h, w, "Duration", |ui, rx, cy| {
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, media.duration.to_timecode_string(), egui::FontId::proportional(13.0), theme.text_primary);
        });

        info_row_custom(ui, theme, pad, row_h, w, "File size", |ui, rx, cy| {
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, format_size(media.file_size_bytes), egui::FontId::proportional(13.0), theme.text_primary);
        });

        // Colour
        info_row_custom(ui, theme, pad, row_h, w, "Colour", |ui, rx, cy| {
            use klipp_media::ColourSpace;
            let (badge_text, badge_color, sub_text) = match media.colour_space {
                ColourSpace::Sdr => ("SDR", theme.marker_info, "BT.709"),
                ColourSpace::Hdr10 => ("HDR10", theme.marker_warning, "BT.2020"),
                ColourSpace::Hlg => ("HLG", theme.accent, "BT.2020"),
                ColourSpace::Unknown => ("SDR", theme.marker_info, "BT.709"),
            };
            let sub_galley = ui.painter().layout_no_wrap(sub_text.to_string(), egui::FontId::proportional(12.0), dim);
            let sub_w = sub_galley.size().x;
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, sub_text, egui::FontId::proportional(12.0), dim);
            let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), egui::FontId::proportional(11.0), egui::Color32::WHITE);
            let bw = badge_galley.size().x + 10.0;
            let bh = 18.0;
            let badge_rect = egui::Rect::from_min_size(egui::pos2(rx - sub_w - 8.0 - bw, cy - bh / 2.0), egui::vec2(bw, bh));
            ui.painter().rect_filled(badge_rect, 3.0, badge_color);
            ui.painter().text(badge_rect.center(), egui::Align2::CENTER_CENTER, badge_text, egui::FontId::proportional(11.0), egui::Color32::WHITE);
        });

        // Shoot date
        let shoot_date = media.media_created_at.or(media.file_modified_at);
        if let Some(epoch_secs) = shoot_date.filter(|&t| t > 0) {
            info_row_custom(ui, theme, pad, row_h, w, "Shoot date", |ui, rx, cy| {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, format_epoch_date(epoch_secs), egui::FontId::proportional(13.0), theme.text_primary);
            });
        }

        // Camera info (if available from metadata)
        if media.camera_make.is_some() || media.camera_model.is_some() {
            info_row_custom(ui, theme, pad, row_h, w, "Camera", |ui, rx, cy| {
                let make = media.camera_make.as_deref().unwrap_or("");
                let model = media.camera_model.as_deref().unwrap_or("");
                let text = if !make.is_empty() && !model.is_empty() {
                    format!("{make} {model}")
                } else {
                    format!("{make}{model}")
                };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(13.0), theme.text_primary);
            });
        }

        truncated_row(ui, theme, pad, row_h, w, media);
        synced_audio_row(ui, theme, pad, row_h, w, state, media);
        shot_rows(ui, theme, pad, row_h, w, dim, &media.shot);

        // Encoder
        if let Some(ref enc) = media.encoder {
            info_row_custom(ui, theme, pad, row_h, w, "Encoder", |ui, rx, cy| {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, enc, egui::FontId::proportional(12.0), dim);
            });
        }

        // Bit depth + pixel format
        if media.bit_depth.is_some() || media.pixel_format.is_some() {
            info_row_custom(ui, theme, pad, row_h, w, "Format", |ui, rx, cy| {
                let mut parts = Vec::new();
                if let Some(bd) = media.bit_depth { parts.push(format!("{bd}-bit")); }
                if let Some(ref pf) = media.pixel_format { parts.push(pf.clone()); }
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, parts.join(" \u{00B7} "), egui::FontId::proportional(12.0), dim);
            });
        }

        // Video bitrate
        if let Some(vbr) = media.video_bitrate_kbps {
            info_row_custom(ui, theme, pad, row_h, w, "Bitrate", |ui, rx, cy| {
                let text = if vbr >= 1000 { format!("{:.1} Mbps", vbr as f64 / 1000.0) } else { format!("{vbr} kbps") };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), dim);
            });
        }

        // Color transfer / primaries
        if media.color_transfer.is_some() || media.color_primaries.is_some() {
            info_row_custom(ui, theme, pad, row_h, w, "Color", |ui, rx, cy| {
                let transfer = media.color_transfer.as_deref().unwrap_or("Unspecified");
                let primaries = media.color_primaries.as_deref().unwrap_or("");
                let text = if !primaries.is_empty() && primaries != "Unspecified" && primaries != transfer {
                    format!("{transfer} / {primaries}")
                } else {
                    transfer.to_string()
                };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), dim);
            });
        }

        tag_list(ui, theme, &media.shot);

        // Proxy
        info_row_custom(ui, theme, pad, row_h, w, "Proxy", |ui, rx, cy| {
            if media.proxy_ready {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, "Ready", egui::FontId::proportional(13.0), theme.marker_success);
            } else {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, "None", egui::FontId::proportional(13.0), dim);
            }
        });

        // File path
        ui.add_space(4.0);
        egui::Frame::default()
            .outer_margin(egui::Margin { left: pad, right: pad, top: 0.0, bottom: 0.0 })
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .fill(theme.bg_raised)
            .rounding(4.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(media.path.to_string_lossy().as_ref()).size(10.0).color(dim));
            });
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    logging_section(ui, theme, theme.bg_deep, theme.bg_panel, media, cmds);
    ui.add_space(8.0);

    // ── TAGS ──
    let tag_count = media.tags.len();
    section_card(ui, "TAGS", Some(format!("{tag_count}")), theme.bg_deep, theme.bg_panel, theme, |ui| {
        if media.tags.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(egui::RichText::new("No tags added").italics().size(12.0).color(dim));
            });
            ui.add_space(6.0);
        } else {
            ui.add_space(4.0);
            let mut tag_to_remove: Option<usize> = None;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(pad);
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for (i, tag) in media.tags.iter().enumerate() {
                    egui::Frame::default()
                        .fill(theme.bg_raised)
                        .rounding(10.0)
                        .inner_margin(egui::Margin { left: 8.0, right: 4.0, top: 3.0, bottom: 3.0 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                ui.label(egui::RichText::new(tag.as_str()).size(11.0).color(theme.text_primary));
                                if ui.add(egui::Button::new(egui::RichText::new("\u{00D7}").size(12.0).color(dim)).frame(false)).clicked() {
                                    tag_to_remove = Some(i);
                                }
                            });
                        });
                }
            });
            if let Some(idx) = tag_to_remove {
                let mut new_tags = media.tags.clone();
                new_tags.remove(idx);
                cmds.push(AppCommand::SetMediaTags { media_id: media.id, tags: new_tags });
            }
            ui.add_space(6.0);
        }

        // Add tag input
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let w = ui.available_width() - pad;
            let resp = ui.add(
                egui::TextEdit::singleline(tag_input)
                    .hint_text("+  Add tag\u{2026}")
                    .desired_width(w)
                    .font(egui::TextStyle::Small),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !tag_input.trim().is_empty() {
                let new_tag = tag_input.trim().to_lowercase();
                if !media.tags.contains(&new_tag) {
                    let mut new_tags = media.tags.clone();
                    new_tags.push(new_tag);
                    cmds.push(AppCommand::SetMediaTags { media_id: media.id, tags: new_tags });
                }
                tag_input.clear();
            }
        });
        ui.add_space(8.0);
    });

    ui.add_space(8.0);

    // ── TIMELINE USAGE ──
    // Find all clips on the timeline that reference this media
    let mut usages: Vec<(String, String)> = Vec::new();
    let mut total_used_secs = 0.0_f64;
    for track in &state.sequence().tracks {
        for clip in &track.clips {
            if clip.media_id() == Some(media.id) {
                let in_tc = clip.timeline_in.to_timecode_string();
                let out_tc = clip.timeline_out.to_timecode_string();
                let track_name = if track.name.is_empty() {
                    format!("{:?} {}", track.kind, track.clips.iter().position(|c| c.id == clip.id).unwrap_or(0) + 1)
                } else {
                    track.name.clone()
                };
                usages.push((track_name, format!("{in_tc} \u{2013} {out_tc}")));
                total_used_secs += clip.duration().to_secs_f64();
            }
        }
    }
    let usage_count = usages.len();
    let total_used_tc = klipp_core::Timecode::from_secs_f64(total_used_secs, state.sequence().fps.num);

    section_card(ui, "TIMELINE USAGE", Some(format!("{usage_count}")), theme.bg_deep, theme.bg_panel, theme, |ui| {
        let w = ui.available_width();

        info_row_custom(ui, theme, pad, row_h, w, "Used in", |ui, rx, cy| {
            let text = if usage_count == 0 { "Not used".into() } else { format!("{usage_count} place{}", if usage_count == 1 { "" } else { "s" }) };
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(13.0), theme.text_primary);
        });

        if usage_count > 0 {
            info_row_custom(ui, theme, pad, row_h, w, "Total used", |ui, rx, cy| {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, total_used_tc.to_timecode_string(), egui::FontId::monospace(13.0), theme.text_primary);
            });

            // Usage list
            ui.add_space(4.0);
            for (track_name, range) in &usages {
                let (usage_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h), egui::Sense::hover());
                ui.painter().hline(usage_rect.min.x + pad..=usage_rect.max.x - pad, usage_rect.max.y, egui::Stroke::new(0.5_f32, theme.border));
                // Colored dot
                ui.painter().circle_filled(egui::pos2(usage_rect.min.x + pad + 4.0, usage_rect.center().y), 3.5, accent);
                // Track name + range
                ui.painter().text(
                    egui::pos2(usage_rect.min.x + pad + 14.0, usage_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{track_name} \u{00B7} {range}"),
                    egui::FontId::proportional(11.0), dim,
                );
            }
        }
        ui.add_space(4.0);
    });

    ui.add_space(16.0);
}

/// State 3: Timeline clip selected — full clip info
pub fn show_clip(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    track_idx: usize,
    clip: &Clip,
    media: &klipp_media::MediaItem,
    tag_input: &mut String,
    cmds: &mut Vec<AppCommand>,
) {
    let pad = 14.0;
    let row_h = 30.0;
    let value_color = theme.text_primary;
    let accent = theme.accent;
    let dim = theme.text_dim;
    let section_bg = theme.bg_deep;
    let card_bg = theme.bg_panel;

    // ════════════════════════════════════════
    // CLIP INFO
    // ════════════════════════════════════════
    section_card(ui, "CLIP INFO", None, section_bg, card_bg, theme, |ui| {
        let w = ui.available_width();

        // Resolution
        info_row_custom(ui, theme, pad, row_h, w, "Resolution", |ui, rx, cy| {
            let text = format!("{} \u{00D7} {}", media.resolution.width, media.resolution.height);
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                text, egui::FontId::proportional(13.0), value_color,
            );
        });

        // Frame rate
        if !media.is_still {
            info_row_custom(ui, theme, pad, row_h, w, "Frame rate", |ui, rx, cy| {
                let fps_val = media.fps.as_f64();
                let text = if (fps_val - fps_val.round()).abs() < 0.01 {
                    format!("{}  fps", fps_val.round() as u32)
                } else {
                    format!("{:.3}  fps", fps_val)
                };
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    text, egui::FontId::proportional(13.0), value_color,
                );
            });
        } else {
            info_row_custom(ui, theme, pad, row_h, w, "Type", |ui, rx, cy| {
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    "Still image", egui::FontId::proportional(13.0), value_color,
                );
            });
        }

        // Codec (pill badges)
        info_row_custom(ui, theme, pad, row_h, w, "Codec", |ui, rx, cy| {
            let pill_h = 20.0;
            let pill_bg = theme.bg_raised;
            let pill_border = theme.border_light;
            let mut x = rx;

            // Audio codec pill
            if !media.audio_codec.is_empty() {
                let codec = media.audio_codec.to_uppercase();
                let galley = ui.painter().layout_no_wrap(
                    codec.clone(), egui::FontId::monospace(11.0), value_color,
                );
                let pw = galley.size().x + 12.0;
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(x - pw, cy - pill_h / 2.0),
                    egui::vec2(pw, pill_h),
                );
                ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0_f32, pill_border));
                ui.painter().text(
                    pill_rect.center(), egui::Align2::CENTER_CENTER,
                    codec, egui::FontId::monospace(11.0), value_color,
                );
                x -= pw + 6.0;
            }

            // Video codec pill
            if !media.video_codec.is_empty() {
                let codec = media.video_codec.to_uppercase();
                let galley = ui.painter().layout_no_wrap(
                    codec.clone(), egui::FontId::monospace(11.0), value_color,
                );
                let pw = galley.size().x + 12.0;
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(x - pw, cy - pill_h / 2.0),
                    egui::vec2(pw, pill_h),
                );
                ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0_f32, pill_border));
                ui.painter().text(
                    pill_rect.center(), egui::Align2::CENTER_CENTER,
                    codec, egui::FontId::monospace(11.0), value_color,
                );
            }
        });

        // Audio (channel bars + label)
        if media.has_audio {
            info_row_custom(ui, theme, pad, row_h, w, "Audio", |ui, rx, cy| {
                let channels = media.audio_channels.unwrap_or(2);
                let sample_rate = media.sample_rate.unwrap_or(48000);
                let ch_label = match channels {
                    1 => "Mono",
                    2 => "Stereo",
                    6 => "5.1",
                    8 => "7.1",
                    _ => "",
                };
                let sr_khz = sample_rate as f64 / 1000.0;
                let sr_text = if sr_khz == sr_khz.floor() {
                    format!("{} kHz", sr_khz as u32)
                } else {
                    format!("{:.1} kHz", sr_khz)
                };
                let label = format!("{ch_label} \u{00B7} {sr_text}");

                let label_galley = ui.painter().layout_no_wrap(
                    label.clone(), egui::FontId::proportional(12.0), dim,
                );
                let label_w = label_galley.size().x;

                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    &label, egui::FontId::proportional(12.0), dim,
                );

                // Channel bars
                let bars_x = rx - label_w - 10.0;
                let bar_w = 4.0;
                let bar_gap = 2.0;
                let max_bar_h = 14.0;
                let bar_heights: &[f32] = match channels {
                    1 => &[0.8],
                    2 => &[0.9, 0.7],
                    6 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5],
                    8 => &[0.7, 0.9, 1.0, 0.8, 0.6, 0.5, 0.7, 0.6],
                    _ => &[0.8, 0.7],
                };
                let total_bars_w = bar_heights.len() as f32 * (bar_w + bar_gap) - bar_gap;
                let bx = bars_x - total_bars_w;
                for (i, &h_frac) in bar_heights.iter().enumerate() {
                    let bh = max_bar_h * h_frac;
                    let bar_rect = egui::Rect::from_min_size(
                        egui::pos2(bx + i as f32 * (bar_w + bar_gap), cy - bh / 2.0),
                        egui::vec2(bar_w, bh),
                    );
                    ui.painter().rect_filled(bar_rect, 1.0, accent);
                }
            });
        }

        // Duration
        info_row_custom(ui, theme, pad, row_h, w, "Duration", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                media.duration.to_timecode_string(),
                egui::FontId::proportional(13.0), value_color,
            );
        });

        // File size
        info_row_custom(ui, theme, pad, row_h, w, "File size", |ui, rx, cy| {
            let size_str = if media.file_size_bytes > 1_000_000_000 {
                format!("{:.1} GB", media.file_size_bytes as f64 / 1_000_000_000.0)
            } else {
                format!("{:.1} MB", media.file_size_bytes as f64 / 1_000_000.0)
            };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                size_str, egui::FontId::proportional(13.0), value_color,
            );
        });

        // Colour space
        info_row_custom(ui, theme, pad, row_h, w, "Colour", |ui, rx, cy| {
            use klipp_media::ColourSpace;
            let (badge_text, badge_color, sub_text) = match media.colour_space {
                ColourSpace::Sdr => ("SDR", theme.marker_info, "BT.709"),
                ColourSpace::Hdr10 => ("HDR10", theme.marker_warning, "BT.2020"),
                ColourSpace::Hlg => ("HLG", theme.accent, "BT.2020"),
                ColourSpace::Unknown => ("SDR", theme.marker_info, "BT.709"),
            };

            // Sub-text
            let sub_galley = ui.painter().layout_no_wrap(
                sub_text.to_string(), egui::FontId::proportional(12.0), dim,
            );
            let sub_w = sub_galley.size().x;
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                sub_text, egui::FontId::proportional(12.0), dim,
            );

            // Badge
            let badge_galley = ui.painter().layout_no_wrap(
                badge_text.to_string(), egui::FontId::proportional(11.0), egui::Color32::WHITE,
            );
            let bw = badge_galley.size().x + 10.0;
            let bh = 18.0;
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(rx - sub_w - 8.0 - bw, cy - bh / 2.0),
                egui::vec2(bw, bh),
            );
            ui.painter().rect_filled(badge_rect, 3.0, badge_color);
            ui.painter().text(
                badge_rect.center(), egui::Align2::CENTER_CENTER,
                badge_text, egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
        });

        // Shoot date
        let shoot_date = media.media_created_at.or(media.file_modified_at);
        if let Some(epoch_secs) = shoot_date.filter(|&t| t > 0) {
            info_row_custom(ui, theme, pad, row_h, w, "Shoot date", |ui, rx, cy| {
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    format_epoch_date(epoch_secs),
                    egui::FontId::proportional(13.0), value_color,
                );
            });
        }

        // Camera info (if available from metadata)
        if media.camera_make.is_some() || media.camera_model.is_some() {
            info_row_custom(ui, theme, pad, row_h, w, "Camera", |ui, rx, cy| {
                let make = media.camera_make.as_deref().unwrap_or("");
                let model = media.camera_model.as_deref().unwrap_or("");
                let text = if !make.is_empty() && !model.is_empty() {
                    format!("{make} {model}")
                } else {
                    format!("{make}{model}")
                };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(13.0), theme.text_primary);
            });
        }
        truncated_row(ui, theme, pad, row_h, w, media);
        synced_audio_row(ui, theme, pad, row_h, w, state, media);
        shot_rows(ui, theme, pad, row_h, w, dim, &media.shot);

        if let Some(ref enc) = media.encoder {
            info_row_custom(ui, theme, pad, row_h, w, "Encoder", |ui, rx, cy| {
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, enc, egui::FontId::proportional(12.0), dim);
            });
        }
        if media.bit_depth.is_some() || media.pixel_format.is_some() {
            info_row_custom(ui, theme, pad, row_h, w, "Format", |ui, rx, cy| {
                let mut parts = Vec::new();
                if let Some(bd) = media.bit_depth { parts.push(format!("{bd}-bit")); }
                if let Some(ref pf) = media.pixel_format { parts.push(pf.clone()); }
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, parts.join(" \u{00B7} "), egui::FontId::proportional(12.0), dim);
            });
        }
        if let Some(vbr) = media.video_bitrate_kbps {
            info_row_custom(ui, theme, pad, row_h, w, "Bitrate", |ui, rx, cy| {
                let text = if vbr >= 1000 { format!("{:.1} Mbps", vbr as f64 / 1000.0) } else { format!("{vbr} kbps") };
                ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), dim);
            });
        }

        tag_list(ui, theme, &media.shot);

        // Proxy
        info_row_custom(ui, theme, pad, row_h, w, "Proxy", |ui, rx, cy| {
            let is_generating = state.proxy_generating.contains(&media.id);
            if media.proxy_ready {
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    "Ready", egui::FontId::proportional(13.0),
                    theme.marker_success,
                );
            } else if is_generating {
                let progress = state.proxy_progress.get(&media.id).copied().unwrap_or(0.0);
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    format!("{:.0}%", progress * 100.0),
                    egui::FontId::proportional(13.0),
                    theme.marker_warning,
                );
                ui.ctx().request_repaint();
            } else {
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    "None", egui::FontId::proportional(13.0), dim,
                );
            }
        });
    });

    ui.add_space(8.0);

    logging_section(ui, theme, section_bg, card_bg, media, cmds);
    ui.add_space(8.0);

    // ════════════════════════════════════════
    // TAGS
    // ════════════════════════════════════════
    let tag_count = media.tags.len();
    section_card(ui, "TAGS", Some(format!("{tag_count}")), section_bg, card_bg, theme, |ui| {
        if media.tags.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(
                    egui::RichText::new("No tags added")
                        .italics()
                        .size(12.0)
                        .color(dim),
                );
            });
            ui.add_space(6.0);
        } else {
            ui.add_space(4.0);
            ui.horizontal(|ui| { ui.add_space(pad); });
            let mut tag_to_remove: Option<usize> = None;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(pad);
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for (i, tag) in media.tags.iter().enumerate() {
                    egui::Frame::none()
                        .fill(theme.bg_raised)
                        .rounding(10.0)
                        .inner_margin(egui::Margin { left: 8.0, right: 4.0, top: 3.0, bottom: 3.0 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                ui.label(
                                    egui::RichText::new(tag.as_str())
                                        .size(11.0)
                                        .color(theme.text_primary),
                                );
                                let x_btn = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new("\u{00D7}")
                                            .size(12.0)
                                            .color(theme.text_dim)
                                    )
                                    .frame(false)
                                );
                                if x_btn.clicked() {
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
            ui.add_space(6.0);
        }

        // Add tag button / input
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let w = ui.available_width() - pad;
            let resp = ui.add(
                egui::TextEdit::singleline(tag_input)
                    .hint_text("+  Add tag\u{2026}")
                    .desired_width(w)
                    .font(egui::TextStyle::Small),
            );
            if resp.lost_focus()
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
        });
        ui.add_space(8.0);
    });

    ui.add_space(8.0);

    // ════════════════════════════════════════
    // TIMELINE
    // ════════════════════════════════════════
    section_card(ui, "TIMELINE", None, section_bg, card_bg, theme, |ui| {
        let w = ui.available_width();

        // Progress bar — clip position in sequence
        let seq_dur = state.sequence_duration().to_secs_f64();
        let clip_in = clip.timeline_in.to_secs_f64();
        let clip_out = clip.timeline_out.to_secs_f64();
        if seq_dur > 0.0 {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                let bar_w = w - pad * 2.0;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(bar_w, 6.0), egui::Sense::hover(),
                );
                // Background
                ui.painter().rect_filled(rect, 2.0, theme.bg_raised);
                // Clip range highlight
                let start_frac = (clip_in / seq_dur).clamp(0.0, 1.0) as f32;
                let end_frac = (clip_out / seq_dur).clamp(0.0, 1.0) as f32;
                let highlight = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x + rect.width() * start_frac, rect.min.y),
                    egui::pos2(rect.min.x + rect.width() * end_frac, rect.max.y),
                );
                ui.painter().rect_filled(highlight, 2.0, accent);
            });

            // Timecode range labels
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.colored_label(dim,
                    egui::RichText::new(clip.timeline_in.to_timecode_string()).monospace().size(10.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(pad);
                    ui.colored_label(dim,
                        egui::RichText::new(clip.timeline_out.to_timecode_string()).monospace().size(10.0));
                });
            });
        }
        ui.add_space(4.0);

        // In
        info_row_custom(ui, theme, pad, row_h, w, "In", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                clip.timeline_in.to_timecode_string(),
                egui::FontId::monospace(13.0), value_color,
            );
        });

        // Out
        info_row_custom(ui, theme, pad, row_h, w, "Out", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                clip.timeline_out.to_timecode_string(),
                egui::FontId::monospace(13.0), value_color,
            );
        });

        // Duration (accent)
        info_row_custom(ui, theme, pad, row_h, w, "Duration", |ui, rx, cy| {
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                clip.duration().to_timecode_string(),
                egui::FontId::monospace(13.0), accent,
            );
        });

        // Track
        info_row_custom(ui, theme, pad, row_h, w, "Track", |ui, rx, cy| {
            let track_name = state.sequence().tracks.get(track_idx)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                track_name,
                egui::FontId::proportional(13.0),
                theme.marker_info,
            );
        });

        // Speed
        info_row_custom(ui, theme, pad, row_h, w, "Speed", |ui, rx, cy| {
            let speed = clip.speed;
            let color = if (speed - 1.0).abs() < 0.001 {
                theme.marker_success // green
            } else if speed > 1.0 {
                theme.marker_warning // amber
            } else {
                theme.marker_error // red
            };
            ui.painter().text(
                egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                format!("{:.0}%", speed * 100.0),
                egui::FontId::proportional(13.0), color,
            );
        });
    });
}

// ── Helpers ──────────────────────────────────────────────

/// Collapsible section card using egui's collapsing header state.
/// The editor's log of the clip — scene, shot, take, reel, keywords,
/// description, comments, good take — edited in place. Each change goes
/// out as one `SetMediaLog`; the keywords line is committed when it is
/// left, so a comma being typed is not swept away mid-edit.
fn logging_section(ui: &mut egui::Ui, theme: &KlippTheme, header_bg: egui::Color32, card_bg: egui::Color32, media: &klipp_media::MediaItem, cmds: &mut Vec<AppCommand>) {
    let pad = 12.0;
    let dim = theme.text_dim;
    let badge = media.log.slate_line().or_else(|| (!media.log.is_empty()).then(|| "logged".to_string()));
    section_card(ui, "LOGGING", badge, header_bg, card_bg, theme, |ui| {
        let mut log = media.log.clone();
        let mut changed = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let total = ui.available_width() - pad;
            egui::Grid::new(("clip_log_grid", media.id)).num_columns(2).spacing([8.0, 4.0]).min_col_width(64.0).show(ui, |ui| {
                let edit_w = (total - 80.0).max(60.0);
                let mut line = |ui: &mut egui::Ui, label: &str, value: &mut String| {
                    ui.label(egui::RichText::new(label).size(11.0).color(dim));
                    if ui.add(egui::TextEdit::singleline(value).desired_width(edit_w).font(egui::TextStyle::Small)).changed() {
                        changed = true;
                    }
                    ui.end_row();
                };
                line(ui, "Scene", &mut log.scene);
                line(ui, "Shot", &mut log.shot);
                line(ui, "Take", &mut log.take);
                line(ui, "Reel", &mut log.reel);

                // Keywords: edited as one line in a buffer of their own,
                // normalised when the field is left.
                ui.label(egui::RichText::new("Keywords").size(11.0).color(dim));
                let buf_id = egui::Id::new(("clip_log_keywords", media.id));
                let mut buf: String = ui.ctx().data_mut(|d| d.get_temp::<String>(buf_id)).unwrap_or_else(|| log.keywords_line());
                let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(edit_w).font(egui::TextStyle::Small).hint_text("interview, wide, b-roll"));
                if resp.has_focus() || resp.changed() {
                    ui.ctx().data_mut(|d| d.insert_temp(buf_id, buf.clone()));
                }
                if resp.lost_focus() {
                    ui.ctx().data_mut(|d| d.remove::<String>(buf_id));
                    let mut edited = log.clone();
                    edited.set_keywords_line(&buf);
                    if edited.keywords != log.keywords {
                        log.keywords = edited.keywords;
                        changed = true;
                    }
                }
                ui.end_row();

                ui.label(egui::RichText::new("Description").size(11.0).color(dim));
                if ui.add(egui::TextEdit::multiline(&mut log.description).desired_width(edit_w).desired_rows(2).font(egui::TextStyle::Small)).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label(egui::RichText::new("Comments").size(11.0).color(dim));
                if ui.add(egui::TextEdit::multiline(&mut log.comments).desired_width(edit_w).desired_rows(2).font(egui::TextStyle::Small)).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("");
                if ui.checkbox(&mut log.good_take, egui::RichText::new("Good take").size(11.0)).changed() {
                    changed = true;
                }
                ui.end_row();
            });
        });
        if let Some(reel) = media.shot.reel_name.as_deref().filter(|_| log.reel.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(egui::RichText::new(format!("Reel from the file: {reel}")).size(10.0).color(dim));
            });
        }
        ui.add_space(8.0);
        if changed {
            cmds.push(AppCommand::SetMediaLog { media_id: media.id, log });
        }
    });
}

fn section_card(
    ui: &mut egui::Ui,
    title: &str,
    badge: Option<String>,
    header_bg: egui::Color32,
    card_bg: egui::Color32,
    theme: &KlippTheme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let w = ui.available_width();

    // Use global collapsed sections set for persistence
    let collapsed_key = egui::Id::new("klipp_collapsed_sections");
    let is_collapsed: bool = ui.ctx().data_mut(|d| {
        let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
        set.contains(title)
    });

    let id = ui.id().with(title);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(), id, !is_collapsed,
    );

    // Header
    let header_h = 30.0;
    let (header_rect, header_resp) = ui.allocate_exact_size(
        egui::vec2(w, header_h), egui::Sense::click(),
    );

    let is_open = state.is_open();
    let rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding::same(6.0)
    };
    ui.painter().rect_filled(header_rect, rounding, header_bg);

    // Title
    ui.painter().text(
        egui::pos2(header_rect.min.x + 14.0 + 4.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(11.0),
        theme.text_dim,
    );

    // Badge
    if let Some(ref badge_text) = badge {
        ui.painter().text(
            egui::pos2(header_rect.max.x - 14.0 - 16.0, header_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            badge_text,
            egui::FontId::proportional(11.0),
            theme.text_dim,
        );
    }

    // Collapse/expand icon
    let icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
    let icon_size = 12.0;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(header_rect.max.x - 14.0 - icon_size / 2.0, header_rect.center().y),
        egui::vec2(icon_size, icon_size),
    );
    ui.put(icon_rect, icon.image(icon_size).tint(theme.text_dim));

    if header_resp.clicked() {
        state.toggle(ui);
        let now_open = state.is_open();
        ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
            if now_open {
                set.remove(title);
            } else {
                set.insert(title.to_string());
            }
        });
    }

    // Body
    if state.is_open() {
        let frame = egui::Frame::none()
            .fill(card_bg)
            .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 4.0, bottom: 0.0 });

        frame.show(ui, |ui| {
            add_contents(ui);
        });
    }

    state.store(ui.ctx());
}

/// Draw a single key-value row with custom right-side rendering.
/// The separate recording paired with the clip, and its offset.
fn synced_audio_row(ui: &mut egui::Ui, theme: &KlippTheme, pad: f32, row_h: f32, w: f32, state: &AppState, media: &klipp_media::MediaItem) {
    let Some(sync) = &media.synced_audio else { return };
    let name = state.media_pool.get(&sync.audio_media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "missing recording".to_string());
    let text = format!("{name} ({})", sync.describe());
    info_row_custom(ui, theme, pad, row_h, w, "Synced audio", |ui, rx, cy| {
        ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), theme.text_accent);
    });
}

/// A file cut short: what it holds against what its header claims.
fn truncated_row(ui: &mut egui::Ui, theme: &KlippTheme, pad: f32, row_h: f32, w: f32, media: &klipp_media::MediaItem) {
    let Some(claimed) = media.truncated_from_secs else { return };
    let text = format!("{} of {} the header claims", klipp_media::describe_secs(media.duration.to_secs_f64()), klipp_media::describe_secs(claimed));
    info_row_custom(ui, theme, pad, row_h, w, "Cut short", |ui, rx, cy| {
        ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), theme.text_accent);
    });
}

/// The shot as the camera recorded it: lens, exposure, white balance,
/// gamma and gamut, timecode and reel — each row only when known.
fn shot_rows(ui: &mut egui::Ui, theme: &KlippTheme, pad: f32, row_h: f32, w: f32, dim: egui::Color32, shot: &klipp_core::ShotMetadata) {
    let rows: [(&str, Option<String>, egui::Color32); 6] = [
        ("Lens", shot.lens.clone(), theme.text_primary),
        ("Exposure", shot.exposure_summary(), theme.text_primary),
        ("White balance", shot.white_balance.clone(), dim),
        (
            "Gamma",
            match (&shot.gamma, &shot.colour_gamut) {
                (Some(g), Some(c)) => Some(format!("{g} / {c}")),
                (Some(g), None) => Some(g.clone()),
                (None, Some(c)) => Some(c.clone()),
                (None, None) => None,
            },
            dim,
        ),
        ("Timecode", shot.start_timecode.clone(), theme.text_primary),
        ("Reel", shot.reel_name.clone(), dim),
    ];
    for (label, value, colour) in rows {
        let Some(value) = value else { continue };
        info_row_custom(ui, theme, pad, row_h, w, label, |ui, rx, cy| {
            ui.painter().text(egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER, value, egui::FontId::proportional(12.0), colour);
        });
    }
}

/// Every tag the file carried, as written, folded away under a header.
fn tag_list(ui: &mut egui::Ui, theme: &KlippTheme, shot: &klipp_core::ShotMetadata) {
    if shot.tags.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(egui::RichText::new(format!("All metadata ({})", shot.tags.len())).size(11.0).color(theme.text_dim))
        .id_salt("shot_tag_list")
        .show(ui, |ui| {
            egui::Grid::new("shot_tag_grid").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                for (k, v) in &shot.tags {
                    ui.label(egui::RichText::new(k).monospace().size(10.0).color(theme.text_dim));
                    ui.add(egui::Label::new(egui::RichText::new(v).size(10.0).color(theme.text_primary)).wrap());
                    ui.end_row();
                }
            });
        });
}

fn info_row_custom(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    pad: f32,
    row_h: f32,
    total_w: f32,
    label: &str,
    draw_value: impl FnOnce(&mut egui::Ui, f32, f32),
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, row_h), egui::Sense::hover(),
    );

    // Separator
    ui.painter().hline(
        rect.min.x + pad..=rect.max.x - pad,
        rect.max.y,
        egui::Stroke::new(0.5_f32, theme.border),
    );

    // Label
    ui.painter().text(
        egui::pos2(rect.min.x + pad, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        theme.text_secondary,
    );

    // Value
    draw_value(ui, rect.max.x - pad, rect.center().y);
}

/// Format Unix epoch seconds as "DD Mon YYYY, HH:MM".
fn format_epoch_date(epoch_secs: u64) -> String {
    let secs = epoch_secs;
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m) = (rem / 3600, (rem % 3600) / 60);
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let dy = if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
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

fn format_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn dir_size_bytes(path: &std::path::Path) -> Option<u64> {
    if !path.is_dir() { return None; }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size_bytes(&entry.path()).unwrap_or(0);
            }
        }
    }
    Some(total)
}

fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            Some(stat.f_bavail * stat.f_frsize)
        } else {
            None
        }
    }
}
