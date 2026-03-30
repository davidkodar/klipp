use crate::icons::Icon;
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
    let pad = 14.0;
    let row_h = 30.0;
    let value_color = theme.text_primary;
    let accent = theme.accent;
    let dim = theme.text_dim;
    let section_bg = theme.bg_deep;
    let card_bg = theme.bg_panel;
    let border = theme.border;

    // ════════════════════════════════════════
    // CLIP INFO
    // ════════════════════════════════════════
    section_card(ui, "CLIP INFO", None, section_bg, card_bg, border, theme, |ui| {
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
                ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0, pill_border));
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
                ui.painter().rect(pill_rect, 4.0, pill_bg, egui::Stroke::new(1.0, pill_border));
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
                ColourSpace::Sdr => ("SDR", egui::Color32::from_rgb(60, 120, 200), "BT.709"),
                ColourSpace::Hdr10 => ("HDR10", egui::Color32::from_rgb(220, 140, 30), "BT.2020"),
                ColourSpace::Hlg => ("HLG", egui::Color32::from_rgb(200, 100, 40), "BT.2020"),
                ColourSpace::Unknown => ("SDR", egui::Color32::from_rgb(60, 120, 200), "BT.709"),
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

        // Proxy
        info_row_custom(ui, theme, pad, row_h, w, "Proxy", |ui, rx, cy| {
            let is_generating = state.proxy_generating.contains(&media.id);
            if media.proxy_ready {
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    "Ready", egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(100, 200, 120),
                );
            } else if is_generating {
                let progress = state.proxy_progress.get(&media.id).copied().unwrap_or(0.0);
                ui.painter().text(
                    egui::pos2(rx, cy), egui::Align2::RIGHT_CENTER,
                    format!("{:.0}%", progress * 100.0),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(220, 180, 60),
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

    // ════════════════════════════════════════
    // TAGS
    // ════════════════════════════════════════
    let tag_count = media.tags.len();
    section_card(ui, "TAGS", Some(format!("{tag_count}")), section_bg, card_bg, border, theme, |ui| {
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
    section_card(ui, "TIMELINE", None, section_bg, card_bg, border, theme, |ui| {
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
                egui::Color32::from_rgb(80, 160, 220),
            );
        });

        // Speed
        info_row_custom(ui, theme, pad, row_h, w, "Speed", |ui, rx, cy| {
            let speed = clip.speed;
            let color = if (speed - 1.0).abs() < 0.001 {
                egui::Color32::from_rgb(100, 200, 120) // green
            } else if speed > 1.0 {
                egui::Color32::from_rgb(220, 180, 60) // amber
            } else {
                egui::Color32::from_rgb(200, 80, 80) // red
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
fn section_card(
    ui: &mut egui::Ui,
    title: &str,
    badge: Option<String>,
    header_bg: egui::Color32,
    card_bg: egui::Color32,
    border: egui::Color32,
    theme: &KlippTheme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let w = ui.available_width();
    let id = ui.id().with(title);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(), id, true,
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

    // Left accent bar
    let accent_rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 6.0, se: 0.0 }
    };
    ui.painter().rect_filled(
        egui::Rect::from_min_size(header_rect.min, egui::vec2(3.0, header_h)),
        accent_rounding, border,
    );

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
    }

    // Body
    if state.is_open() {
        let body_start = ui.cursor().min;
        let frame = egui::Frame::default()
            .fill(card_bg)
            .inner_margin(egui::Margin { left: 0.0, right: 0.0, top: 4.0, bottom: 0.0 })
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE);

        frame.show(ui, |ui| {
            add_contents(ui);
        });

        let body_end_y = ui.cursor().min.y;

        // Left accent on body
        ui.painter().rect_filled(
            egui::Rect::from_min_size(body_start, egui::vec2(3.0, body_end_y - body_start.y)),
            0.0, border,
        );

        // Card outline
        let full_rect = egui::Rect::from_min_max(
            header_rect.min,
            egui::pos2(header_rect.max.x, body_end_y),
        );
        ui.painter().rect_stroke(full_rect, 6.0, egui::Stroke::new(0.5, border));
    } else {
        ui.painter().rect_stroke(header_rect, 6.0, egui::Stroke::new(0.5, border));
    }

    state.store(ui.ctx());
}

/// Draw a single key-value row with custom right-side rendering.
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
        egui::Stroke::new(0.5, theme.border),
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
