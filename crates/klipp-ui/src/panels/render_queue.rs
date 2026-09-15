use crate::theme::KlippTheme;
use klipp_export::{RenderJobStatus, RenderQueue};

/// Actions that the render queue panel needs the app to perform.
pub enum RenderQueueAction {
    CancelJob(uuid::Uuid),
    RemoveJob(uuid::Uuid),
    ClearCompleted,
    Close,
}

/// Render queue panel — shows all queued/active/completed export jobs.
pub struct RenderQueuePanel;

impl RenderQueuePanel {
    // ── Design constants ──
    const STATUS_BADGE_W: f32 = 72.0;
    const BAR_HEIGHT: f32 = 20.0;
    const JOB_SPACING: f32 = 6.0;

    fn status_badge_colors(status: &RenderJobStatus, theme: &KlippTheme) -> (egui::Color32, egui::Color32) {
        match status {
            RenderJobStatus::Queued => (
                theme.bg_raised,
                theme.text_secondary,
            ),
            RenderJobStatus::Rendering => (
                theme.accent_dim,
                theme.accent,
            ),
            RenderJobStatus::Complete => (
                theme.bg_raised,
                theme.marker_success,
            ),
            RenderJobStatus::Failed => (
                theme.bg_raised,
                theme.marker_error,
            ),
            RenderJobStatus::Cancelled => (
                theme.bg_panel_alt,
                theme.text_dim,
            ),
        }
    }

    fn status_label(status: &RenderJobStatus) -> &'static str {
        match status {
            RenderJobStatus::Queued => "Queued",
            RenderJobStatus::Rendering => "Rendering",
            RenderJobStatus::Complete => "Complete",
            RenderJobStatus::Failed => "Failed",
            RenderJobStatus::Cancelled => "Cancelled",
        }
    }

    /// Show the render queue window. Returns actions for the app to execute.
    pub fn show(
        ctx: &egui::Context,
        queue: &RenderQueue,
        theme: &KlippTheme,
    ) -> Vec<RenderQueueAction> {
        let mut actions = Vec::new();
        let mut open = true;

        egui::Window::new("Render Queue")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .default_height(420.0)
            .show(ctx, |ui| {
                Self::show_contents(ui, queue, theme, false, &mut actions);
            });

        if !open {
            actions.push(RenderQueueAction::Close);
        }

        actions
    }

    /// The queue itself: the summary row, then one card per job. In a
    /// window the jobs scroll on their own; embedded in a column the
    /// column scrolls.
    pub fn show_contents(
        ui: &mut egui::Ui,
        queue: &RenderQueue,
        theme: &KlippTheme,
        embedded: bool,
        actions: &mut Vec<RenderQueueAction>,
    ) {
                // ── Header bar ──
                ui.horizontal(|ui| {
                    let total = queue.jobs.len();
                    let active = queue.jobs.iter().filter(|j| j.status == RenderJobStatus::Rendering).count();
                    let queued = queue.queued_count();
                    let complete = queue.jobs.iter().filter(|j| j.status == RenderJobStatus::Complete).count();

                    ui.label(
                        egui::RichText::new(format!("{total} jobs"))
                            .color(theme.text_primary)
                            .size(13.0),
                    );
                    if active > 0 {
                        ui.colored_label(theme.accent, format!("{active} rendering"));
                    }
                    if queued > 0 {
                        ui.colored_label(theme.text_secondary, format!("{queued} queued"));
                    }
                    if complete > 0 {
                        ui.colored_label(
                            theme.marker_success,
                            format!("{complete} done"),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let has_finished = queue.jobs.iter().any(|j| {
                            matches!(
                                j.status,
                                RenderJobStatus::Complete
                                    | RenderJobStatus::Failed
                                    | RenderJobStatus::Cancelled
                            )
                        });
                        if has_finished
                            && ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Clear Finished")
                                            .size(12.0)
                                            .color(theme.text_secondary),
                                    )
                                    .fill(theme.bg_raised),
                                )
                                .clicked()
                        {
                            actions.push(RenderQueueAction::ClearCompleted);
                        }
                    });
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── Empty state ──
                if queue.jobs.is_empty() {
                    ui.add_space(if embedded { 8.0 } else { 40.0 });
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("No render jobs queued")
                                .color(theme.text_dim)
                                .size(13.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Export a sequence to add it to the queue")
                                .color(theme.text_dim)
                                .size(12.0),
                        );
                    });
                    return;
                }

                // ── Job list ──
                let jobs = |ui: &mut egui::Ui, actions: &mut Vec<RenderQueueAction>| {
                        for job in &queue.jobs {
                            let job_frame_bg = if job.status == RenderJobStatus::Rendering {
                                theme.bg_panel_alt
                            } else {
                                theme.bg_panel
                            };

                            egui::Frame::default()
                                .fill(job_frame_bg)
                                .rounding(4.0)
                                .inner_margin(egui::Margin::same(8.0))
                                .stroke(egui::Stroke::new(1.0_f32, theme.border))
                                .show(ui, |ui| {
                                    Self::show_job_row(ui, job, theme, actions);
                                });

                            ui.add_space(Self::JOB_SPACING);
                        }
                };
                if embedded {
                    jobs(ui, actions);
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| jobs(ui, actions));
                }
    }

    fn show_job_row(
        ui: &mut egui::Ui,
        job: &klipp_export::RenderJob,
        theme: &KlippTheme,
        actions: &mut Vec<RenderQueueAction>,
    ) {
        let elapsed_secs = job.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        // ── Row 1: Status badge + name + codec + action button ──
        ui.horizontal(|ui| {
            // Status badge
            let (badge_bg, badge_fg) = Self::status_badge_colors(&job.status, theme);
            let label = Self::status_label(&job.status);
            let badge_rect = ui.allocate_exact_size(
                egui::vec2(Self::STATUS_BADGE_W, 18.0),
                egui::Sense::hover(),
            ).0;
            let painter = ui.painter_at(badge_rect);
            painter.rect_filled(badge_rect, 2.0, badge_bg);

            // Animated dot for rendering jobs
            let badge_text = if job.status == RenderJobStatus::Rendering {
                let dots = match (ui.input(|i| i.time) * 2.0) as usize % 4 {
                    0 => "   ",
                    1 => ".  ",
                    2 => ".. ",
                    _ => "...",
                };
                format!("{label}{dots}")
            } else {
                label.to_string()
            };

            painter.text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                &badge_text,
                egui::FontId::proportional(11.0),
                badge_fg,
            );

            // Job label
            ui.label(
                egui::RichText::new(&job.label)
                    .color(theme.text_primary)
                    .strong()
                    .size(13.0),
            );

            // Codec badge
            let codec_label = job.settings.video_codec.label();
            ui.label(
                egui::RichText::new(format!("\u{2022} {codec_label}"))
                    .color(theme.text_dim)
                    .size(11.0),
            );

            // Right-aligned action buttons
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match job.status {
                    RenderJobStatus::Queued | RenderJobStatus::Rendering => {
                        let btn = egui::Button::new(
                            egui::RichText::new("Cancel")
                                .size(11.0)
                                .color(theme.marker_error),
                        )
                        .fill(theme.bg_raised);
                        if ui.add(btn).clicked() {
                            actions.push(RenderQueueAction::CancelJob(job.id));
                        }
                    }
                    RenderJobStatus::Complete
                    | RenderJobStatus::Failed
                    | RenderJobStatus::Cancelled => {
                        let btn = egui::Button::new(
                            egui::RichText::new("Remove")
                                .size(11.0)
                                .color(theme.text_dim),
                        )
                        .fill(theme.bg_raised);
                        if ui.add(btn).clicked() {
                            actions.push(RenderQueueAction::RemoveJob(job.id));
                        }
                    }
                }
            });
        });

        ui.add_space(4.0);

        // ── Row 2: Progress bar ──
        let bar_text = Self::format_bar_text(job, elapsed_secs);
        let (bar_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), Self::BAR_HEIGHT),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(bar_rect);

        // Background track
        painter.rect_filled(bar_rect, 2.0, theme.bg_deep);

        // Fill
        let fill_width = bar_rect.width() * job.progress;
        if fill_width > 0.5 {
            let fill_rect =
                egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_width, Self::BAR_HEIGHT));
            let fill_color = match job.status {
                RenderJobStatus::Rendering => theme.accent_dim,
                RenderJobStatus::Complete => theme.marker_success,
                RenderJobStatus::Failed => theme.marker_error,
                _ => theme.bg_hover,
            };
            painter.rect_filled(fill_rect, 2.0, fill_color);
        }

        // Text (shadow + foreground)
        let font = egui::FontId::proportional(11.0);
        let text_pos = bar_rect.center();
        painter.text(
            text_pos + egui::vec2(1.0, 1.0),
            egui::Align2::CENTER_CENTER,
            &bar_text,
            font.clone(),
            egui::Color32::BLACK,
        );
        painter.text(
            text_pos,
            egui::Align2::CENTER_CENTER,
            &bar_text,
            font,
            egui::Color32::WHITE,
        );

        // ── Row 3: Details line ──
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            // Output path
            let path_str = job
                .output_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| job.output_path.display().to_string());
            ui.label(
                egui::RichText::new(path_str)
                    .color(theme.text_dim)
                    .size(11.0),
            );

            // Render speed (for active jobs)
            if job.status == RenderJobStatus::Rendering
                && elapsed_secs > 0
                && job.frames_done > 0
            {
                let render_fps = job.frames_done as f64 / elapsed_secs as f64;
                let speed = if job.timeline_fps > 0.0 {
                    render_fps / job.timeline_fps
                } else {
                    0.0
                };
                ui.label(
                    egui::RichText::new(format!(
                        "\u{2022} {:.1}\u{00D7} real-time \u{2022} {:.0} fps",
                        speed, render_fps
                    ))
                    .color(theme.text_dim)
                    .size(11.0),
                );
            }

            // HW info
            if !job.hw_info.is_empty() {
                let hw_clean = job
                    .hw_info
                    .replace("Decode: ", "")
                    .replace("Encode: ", "")
                    .replace(" | ", " \u{2022} ");
                ui.label(
                    egui::RichText::new(format!("\u{2022} {hw_clean}"))
                        .color(theme.text_dim)
                        .size(11.0),
                );
            }
        });

        // ── Render stage (for active jobs) ──
        if job.status == RenderJobStatus::Rendering && !job.render_stage.is_empty() {
            ui.label(
                egui::RichText::new(&job.render_stage)
                    .color(theme.text_secondary)
                    .size(11.0)
                    .italics(),
            );
        }

        // ── Error message (for failed jobs) ──
        if let Some(err) = &job.error {
            ui.label(
                egui::RichText::new(err)
                    .color(theme.marker_error)
                    .size(11.0),
            );
        }

        // ── Collapsible details ──
        let details_id = egui::Id::new("rq_details").with(job.id);
        let mut show_details = ui.data_mut(|d| *d.get_temp_mut_or_default::<bool>(details_id));
        let details_label = if show_details {
            "\u{25BE} Details"
        } else {
            "\u{25B8} Details"
        };
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(details_label)
                        .color(theme.text_dim)
                        .size(11.0),
                )
                .frame(false),
            )
            .clicked()
        {
            show_details = !show_details;
        }
        ui.data_mut(|d| d.insert_temp(details_id, show_details));

        if show_details {
            ui.indent("rq_detail_indent", |ui| {
                let dim = theme.text_dim;
                let res = job
                    .settings
                    .resolution
                    .as_ref()
                    .map(|r| format!("{}x{}", r.width, r.height))
                    .unwrap_or_else(|| "Sequence resolution".into());
                let crf_str = if job.settings.video_codec.uses_crf() {
                    format!("CRF {}", job.settings.crf)
                } else {
                    "N/A".into()
                };
                let audio_str = job.settings.audio_codec.label();
                let bitrate_str = job
                    .settings
                    .audio_bitrate_kbps
                    .map(|b| format!("{b} kbps"))
                    .unwrap_or_else(|| "default".into());

                ui.label(egui::RichText::new(format!("Resolution: {res}")).color(dim).size(11.0));
                ui.label(egui::RichText::new(format!("Quality: {crf_str}")).color(dim).size(11.0));
                ui.label(
                    egui::RichText::new(format!("Audio: {audio_str} {bitrate_str}"))
                        .color(dim)
                        .size(11.0),
                );
                if let Some(ref lr) = job.loudness_result {
                    ui.label(
                        egui::RichText::new(lr)
                            .color(dim)
                            .size(11.0),
                    );
                }
                ui.label(
                    egui::RichText::new(format!("Frames: {}", job.total_frames))
                        .color(dim)
                        .size(11.0),
                );

                if job.total_frames > 0 && job.timeline_fps > 0.0 {
                    let dur_secs = job.total_frames as f64 / job.timeline_fps;
                    let dm = dur_secs as u64 / 60;
                    let ds = dur_secs as u64 % 60;
                    ui.label(
                        egui::RichText::new(format!("Duration: {dm}:{ds:02}"))
                            .color(dim)
                            .size(11.0),
                    );
                }

                // Completed timing
                if job.status == RenderJobStatus::Complete {
                    if let (Some(start), Some(end)) = (job.started_at, job.completed_at) {
                        let secs = end.duration_since(start).as_secs();
                        let m = secs / 60;
                        let s = secs % 60;
                        let elapsed = if m > 0 {
                            format!("{m}m {s:02}s")
                        } else {
                            format!("{s}s")
                        };
                        let speed_str =
                            if secs > 0 && job.total_frames > 0 && job.timeline_fps > 0.0 {
                                let render_fps = job.total_frames as f64 / secs as f64;
                                format!(
                                    " ({:.1}\u{00D7} real-time)",
                                    render_fps / job.timeline_fps
                                )
                            } else {
                                String::new()
                            };
                        ui.label(
                            egui::RichText::new(format!("Render time: {elapsed}{speed_str}"))
                                .color(dim)
                                .size(11.0),
                        );
                    }
                }
            });
        }
    }

    fn format_bar_text(job: &klipp_export::RenderJob, elapsed_secs: u64) -> String {
        match job.status {
            RenderJobStatus::Rendering => {
                let pct = (job.progress * 100.0) as u32;
                let remaining_str = if job.progress > 0.01 && elapsed_secs > 2 {
                    let total_est = elapsed_secs as f64 / job.progress as f64;
                    let remaining = (total_est - elapsed_secs as f64).max(0.0) as u64;
                    let m = remaining / 60;
                    let s = remaining % 60;
                    if m > 0 {
                        format!(" \u{2014} {m}m {s:02}s remaining")
                    } else {
                        format!(" \u{2014} {s}s remaining")
                    }
                } else {
                    " \u{2014} calculating\u{2026}".to_string()
                };
                format!("{pct}%{remaining_str}")
            }
            RenderJobStatus::Complete => {
                match (job.started_at, job.completed_at) {
                    (Some(start), Some(end)) => {
                        let secs = end.duration_since(start).as_secs();
                        let m = secs / 60;
                        let s = secs % 60;
                        let elapsed = if m > 0 {
                            format!("{m}m {s:02}s")
                        } else {
                            format!("{s}s")
                        };
                        format!("Completed in {elapsed}")
                    }
                    _ => "Completed".to_string(),
                }
            }
            RenderJobStatus::Failed => "Export failed".to_string(),
            RenderJobStatus::Cancelled => "Cancelled".to_string(),
            RenderJobStatus::Queued => "Waiting in queue\u{2026}".to_string(),
        }
    }
}
