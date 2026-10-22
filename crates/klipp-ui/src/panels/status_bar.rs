use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_engine::PlaybackState;

/// Render the 26px status bar at the bottom of the window.
pub fn show(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme) {
    let bar = ui.max_rect();
    // The right group first, what is running and the audio device; the
    // left fields take the room it leaves, so the two never overlap
    // (#1355).
    let right_start = ui
        .allocate_new_ui(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.add_space(10.0);

            // Proxy status
            let proxy_label = if state.use_proxies { "Active" } else { "None" };
            status_label_value(ui, theme, "Proxy:", proxy_label);

            // Audio backend status
            let (audio_dot_rect, _) = ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
            if state.audio_backend_ok {
                ui.painter().circle_filled(audio_dot_rect.center(), 2.5, theme.marker_success);
            } else {
                ui.painter().circle_filled(audio_dot_rect.center(), 2.5, theme.marker_error);
            }
            let sr_khz = state.audio_sample_rate as f32 / 1000.0;
            let sr_label = if sr_khz == sr_khz.floor() {
                format!("{}kHz", sr_khz as u32)
            } else {
                format!("{:.1}kHz", sr_khz)
            };
            let audio_label = if state.audio_backend_ok {
                format!("PipeWire \u{00b7} {sr_label}")
            } else {
                "Audio \u{00b7} No Device".to_string()
            };
            status_item(ui, theme, &audio_label);

            // A GPU driver refusing work (#2394): Klipp goes on in
            // software, and says why it is slower.
            if let Some(warning) = klipp_engine::gpu_health::Refusals::now().warning() {
                ui.colored_label(theme.marker_error, egui::RichText::new("GPU refusing work").size(11.0)).on_hover_text(warning);
            }

            // Filmstrip generation progress
            if state.filmstrip_generating {
                let elapsed = state.filmstrip_generate_start.elapsed().as_secs_f64();
                // Per-job N/expected: prefer the live counter we
                // increment per FilmstripResult::Frame, fall back to
                // the thumbnail-cache count if the counter wasn't
                // seeded (older queued jobs).
                let active_id = state.filmstrip_in_progress.iter().next().copied();
                let (current_frames, expected_frames) = active_id
                    .and_then(|id| state.filmstrip_progress.get(&id).copied())
                    .map(|(d, t)| (d as usize, t as usize))
                    .unwrap_or_else(|| {
                        let d = active_id
                            .and_then(|id| state.thumbnail_cache.filmstrip(&id))
                            .map(|f| f.len())
                            .unwrap_or(0);
                        (d, 0)
                    });
                let done = state.filmstrip_completed;
                let total = state.filmstrip_total_queued;
                let frames_label = if expected_frames > 0 {
                    format!("{}/{} frames", current_frames, expected_frames)
                } else {
                    format!("{} frames", current_frames)
                };
                let progress_text = if total > 1 {
                    format!("Filmstrip {}/{} — {} ({:.0}s)", done + 1, total, frames_label, elapsed)
                } else {
                    format!("Filmstrip — {} ({:.0}s)", frames_label, elapsed)
                };
                ui.label(
                    egui::RichText::new(progress_text)
                        .size(10.0)
                        .monospace()
                        .color(theme.text_dim),
                );
                ui.spinner();
            }

            // Skim pre-render progress (active or recently completed)
            let skim_done_fade = state.skim_transcode_done_at.map(|t| {
                let elapsed = t.elapsed().as_secs_f32();
                (1.0 - elapsed / 3.0).clamp(0.0, 1.0) // fade over 3 seconds
            });
            let show_skim_active = state.skim_transcode_status.is_some();
            let show_skim_done = skim_done_fade.is_some_and(|f| f > 0.0);

            if show_skim_active || show_skim_done {
                let (label_text, progress, alpha) = if show_skim_active {
                    let (ref filename, prog) = state.skim_transcode_status.as_ref().unwrap();
                    let short = crate::ui_util::elide(filename, 20);
                    (format!("Pre-rendering: {short}"), *prog, 1.0_f32)
                } else {
                    let fade = skim_done_fade.unwrap();
                    ("Pre-render complete".to_string(), 1.0, fade)
                };

                let pct = (progress * 100.0) as u32;
                let text_alpha = (alpha * 255.0) as u8;
                let bar_color = if show_skim_active {
                    theme.accent_dim
                } else {
                    theme.marker_success // green for done
                };

                ui.label(
                    egui::RichText::new(format!("{pct}%"))
                        .size(10.0)
                        .monospace()
                        .color(theme.text_secondary.linear_multiply(alpha)),
                );

                // Progress bar
                let bar_w = 60.0;
                let bar_h = 3.0;
                let (bar_rect, _) =
                    ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                ui.painter().rect_filled(bar_rect, 2.0, theme.bg_raised.linear_multiply(alpha));
                let fill_w = (progress * bar_w).clamp(0.0, bar_w);
                if fill_w > 0.0 {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            bar_rect.left_top(),
                            egui::vec2(fill_w, bar_h),
                        ),
                        2.0,
                        bar_color.linear_multiply(alpha),
                    );
                }

                ui.label(
                    egui::RichText::new(&label_text)
                        .size(10.0)
                        .monospace()
                        .color(egui::Color32::from_rgba_premultiplied(
                            theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), text_alpha,
                        )),
                );

                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }

            // A subject being tracked (#239): a line each pass, how far
            // and about how long is left.
            for line in &state.status_tracking {
                ui.label(egui::RichText::new(line).size(10.0).color(theme.text_secondary));
            }
            // Captions being translated (#2213).
            if let Some(line) = &state.status_translation {
                ui.label(egui::RichText::new(line).size(10.0).color(theme.text_secondary));
            }

            // Render progress (if active)
            if let Some((label, progress)) = state.render_queue.active_job_progress() {
                let pct = (progress * 100.0) as u32;
                ui.label(
                    egui::RichText::new(format!("{pct}%"))
                        .size(10.0)
                        .monospace()
                        .color(theme.text_secondary),
                );

                // Progress bar
                let bar_w = 70.0;
                let bar_h = 3.0;
                let (bar_rect, _) =
                    ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                ui.painter()
                    .rect_filled(bar_rect, 2.0, theme.bg_raised);
                let fill_w = (progress * bar_w).clamp(0.0, bar_w);
                if fill_w > 0.0 {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            bar_rect.left_top(),
                            egui::vec2(fill_w, bar_h),
                        ),
                        2.0,
                        theme.accent,
                    );
                }

                status_label_value(ui, theme, "Rendering:", label);
            }
            ui.min_rect().left()
        })
        .inner;
    let left = egui::Rect::from_min_max(bar.min, egui::pos2((right_start - 14.0).max(bar.min.x), bar.max.y));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.set_clip_rect(left.intersect(ui.clip_rect()));
        left_fields(ui, state, theme);
    });
}

/// Status, project, duration, format, media size and the selected clip's
/// link, left to right, as far as they fit in `ui`.
fn left_fields(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme) {
    ui.spacing_mut().item_spacing.x = 14.0;
    ui.add_space(10.0);

    // Status dot — uses role colors from theme
    // Status dot: lit while the engine has something loaded, dim
    // when idle. No glow, no role colour.
    let dot_color = match state.playback {
        PlaybackState::Idle => theme.text_dim,
        _ => theme.marker_success,
    };
    let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
    ui.painter().circle_filled(dot_rect.center(), 3.0, dot_color);

    let status_label = match state.playback {
        PlaybackState::Playing => "Playing",
        _ => "Ready",
    };
    status_item(ui, theme, status_label);

    // Project name — suffixed with " •" when there are unsaved changes.
    let project_name = state
        .project_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".into());
    let project_display = if state.dirty {
        format!("{project_name} •")
    } else {
        project_name
    };
    // The name shortened to the room there is, never under a few
    // letters; the fields after it are dropped, in turn, when they
    // don't fit (#1355).
    let room = ui.available_width() - text_width(ui, "Project:") - 4.0;
    let chars = (room / text_width(ui, "0")).floor().max(0.0) as usize;
    if chars < 8 {
        return;
    }
    status_label_value(ui, theme, "Project:", &crate::ui_util::elide(&project_display, chars));

    // Duration
    let dur = state.sequence_duration();
    let dur_str = dur.to_timecode_string();
    if !fits(ui, &["Duration:", &dur_str]) {
        return;
    }
    status_label_value(ui, theme, "Duration:", &dur_str);

    // Resolution + FPS
    // The active sequence's own rate, 29.97 shown as such (#2356).
    let seq = state.sequence();
    let res_str = format!("{}x{} \u{00b7} {}fps", seq.resolution.width, seq.resolution.height, fps_text(seq.fps.as_f64()));
    if !fits(ui, &[&res_str]) {
        return;
    }
    status_item(ui, theme, &res_str);

    // Project media size
    let total_bytes: u64 = state.media_pool.all().map(|m| m.file_size_bytes).sum();
    if total_bytes > 0 {
        let size = format_size(total_bytes);
        if !fits(ui, &["Media:", &size]) {
            return;
        }
        status_label_value(ui, theme, "Media:", &size);
    }

    // Link indicator for selected clip — green chain when the
    // selected clip has a linked partner (e.g. video paired
    // with audio), dim chain when it's standalone. Hover
    // tooltip names what the icon means; without it the green
    // chain reads as decorative.
    if let Some(cid) = state.selected_clip.filter(|_| ui.available_width() >= 24.0) {
        let is_linked = state.sequence().tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == cid)
            .and_then(|c| c.linked_clip_id)
            .is_some();
        let link_label = if is_linked { "\u{1F517}" } else { "\u{26D3}\u{FE0E}" };
        let link_color = if is_linked {
            theme.marker_success
        } else {
            theme.text_dim
        };
        let tooltip = if is_linked {
            "Linked clip — selected clip is paired with another track \
             (e.g. video + audio). Edits affect both."
        } else {
            "Standalone clip — no linked partner."
        };
        ui.add_space(6.0);
        ui.colored_label(link_color, egui::RichText::new(link_label).size(11.0))
            .on_hover_text(tooltip);
    }
}

/// How wide `text` is in the status bar's font.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::monospace(10.0), egui::Color32::WHITE).size().x
}

/// Whether a field of `parts` (a label and its value, or one text) fits
/// in the room left on the row.
fn fits(ui: &egui::Ui, parts: &[&str]) -> bool {
    let w: f32 = parts.iter().map(|p| text_width(ui, p)).sum::<f32>() + 4.0 * (parts.len() as f32 - 1.0);
    w <= ui.available_width()
}

fn status_item(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .monospace()
            .color(theme.text_secondary),
    );
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// A dim label and its value as one text: one widget, so it is placed
/// as a whole in either direction (a row of two, inside the right
/// group's right-to-left layout, drew over its neighbour, #1355).
fn status_label_value(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    let font = egui::FontId::monospace(10.0);
    let mut job = egui::text::LayoutJob::default();
    job.append(label, 0.0, egui::TextFormat::simple(font.clone(), theme.text_dim));
    job.append(value, 4.0, egui::TextFormat::simple(font, theme.text_secondary));
    ui.label(job);
}

#[cfg(test)]
mod narrow_tests {
    use super::*;

    /// At 1018 px, and narrower and wider, with a long project name, a
    /// selected clip and a render running: no field draws over another,
    /// and the right group and the status are always there (#1355).
    #[test]
    fn no_field_draws_over_another_at_any_width() {
        for width in [1920.0, 1280.0, 1018.0, 760.0, 480.0] {
            let ctx = egui::Context::default();
            let mut state = AppState { project_path: Some("/tmp/a-rather-long-project-name-for-a-wedding-film.klipp".into()), dirty: true, ..Default::default() };
            state.audio_backend_ok = true;
            let mut texts = Vec::new();
            for i in 0..3 {
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 200.0))), time: Some(f64::from(i) * 0.1), ..Default::default() };
                let output = ctx.run(input, |ctx| {
                    egui::TopBottomPanel::bottom("status").exact_height(26.0).show(ctx, |ui| show(ui, &state, &KlippTheme::default()));
                });
                texts = output.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                    _ => None,
                }).collect::<Vec<_>>();
            }
            let clash = crate::panels::toolbar::narrow_bar_tests::overlaps(&texts);
            let off: Vec<_> = texts.iter().filter(|(_, r)| r.left() < 0.0 || r.right() > width).collect();
            assert!(off.is_empty(), "{width} px: off the bar: {off:?}");
            assert!(clash.is_empty(), "{width} px:\n{}", clash.join("\n"));
            for want in ["Ready", "Proxy:"] {
                assert!(texts.iter().any(|(t, _)| t.starts_with(want)), "{width} px: no {want:?} among {texts:?}");
            }
            if width >= 1018.0 {
                assert!(texts.iter().any(|(t, _)| t.starts_with("Project:")), "{width} px: the project is shown: {texts:?}");
            }
        }
    }
}

/// A frame rate as the status bar shows it: whole rates plain, the
/// fractional ones to two places (23.98, 29.97, 59.94).
fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.005 {
        format!("{}", fps.round() as u32)
    } else {
        format!("{fps:.2}")
    }
}

#[cfg(test)]
mod fps_text_tests {
    #[test]
    fn whole_rates_plain_fractional_to_two_places() {
        assert_eq!(super::fps_text(30.0), "30");
        assert_eq!(super::fps_text(24000.0 / 1001.0), "23.98");
        assert_eq!(super::fps_text(30000.0 / 1001.0), "29.97");
    }
}
