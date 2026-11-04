use crate::theme::KlippTheme;
use klipp_export::{RenderJobStatus, RenderQueue};

/// Actions that the render queue panel needs the app to perform.
pub enum RenderQueueAction {
    CancelJob(uuid::Uuid),
    RemoveJob(uuid::Uuid),
    ClearCompleted,
    Close,
    /// Open the finished file in the system's player (#717).
    Play(std::path::PathBuf),
    /// Show the file in the file manager.
    ShowInFolder(std::path::PathBuf),
    /// Queue the job again, same settings and frames, from the sequence
    /// as it is now: a fix after a render, or a retry after a failure.
    RenderAgain(uuid::Uuid),
}

/// Render queue panel — shows all queued/active/completed export jobs.
pub struct RenderQueuePanel;

impl RenderQueuePanel {
    // ── Design constants ──
    const BAR_HEIGHT: f32 = 20.0;
    const JOB_SPACING: f32 = 6.0;

    /// The job's state as a word and its colour: a label, not a button.
    fn status(status: &RenderJobStatus, theme: &KlippTheme) -> (&'static str, egui::Color32) {
        match status {
            RenderJobStatus::Queued => ("Queued", theme.text_secondary),
            RenderJobStatus::Rendering => ("Rendering", theme.accent),
            RenderJobStatus::Complete => ("Done", theme.marker_success),
            RenderJobStatus::Failed => ("Failed", theme.marker_error),
            RenderJobStatus::Cancelled => ("Cancelled", theme.text_dim),
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

        crate::widgets::window("Render Queue")
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
                        egui::RichText::new(if total == 1 { "1 job".to_string() } else { format!("{total} jobs") })
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
        let small = |text: String, color: egui::Color32| egui::RichText::new(text).color(color).size(11.0);
        let button = |ui: &mut egui::Ui, text: &str, color: egui::Color32, hover: &str| {
            ui.add(egui::Button::new(egui::RichText::new(text).size(11.0).color(color)).fill(theme.bg_raised))
                .on_hover_text(hover)
                .clicked()
        };

        // ── Row 1: state, file, and what can be done with it ──
        ui.horizontal(|ui| {
            let (word, color) = Self::status(&job.status, theme);
            ui.label(egui::RichText::new(format!("\u{25CF} {word}")).color(color).size(11.0));
            ui.label(egui::RichText::new(&job.label).color(theme.text_primary).strong().size(13.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match job.status {
                    RenderJobStatus::Queued | RenderJobStatus::Rendering => {
                        if button(ui, "Cancel", theme.marker_error, "Stop this render; the partial file is removed") {
                            actions.push(RenderQueueAction::CancelJob(job.id));
                        }
                    }
                    RenderJobStatus::Complete | RenderJobStatus::Failed | RenderJobStatus::Cancelled => {
                        if button(ui, "Remove", theme.text_dim, "Take the job off the list; the file stays") {
                            actions.push(RenderQueueAction::RemoveJob(job.id));
                        }
                        let again = if job.status == RenderJobStatus::Failed { "Retry" } else { "Render Again" };
                        if button(ui, again, theme.text_secondary, "The same settings and frames, from the sequence as it is now; replaces the file") {
                            actions.push(RenderQueueAction::RenderAgain(job.id));
                        }
                        if job.status == RenderJobStatus::Complete {
                            if button(ui, "Show in Folder", theme.text_secondary, "Open the folder the file is in") {
                                actions.push(RenderQueueAction::ShowInFolder(job.output_path.clone()));
                            }
                            if button(ui, "Play", theme.accent, "Open the file in the system's player") {
                                actions.push(RenderQueueAction::Play(job.output_path.clone()));
                            }
                        }
                    }
                }
            });
        });

        // ── Row 2: what is being made, at a glance ──
        ui.label(small(Self::identity(job), theme.text_secondary));
        ui.add_space(4.0);

        // ── Row 3: progress ──
        let bar_text = Self::format_bar_text(job, elapsed_secs);
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), Self::BAR_HEIGHT), egui::Sense::hover());
        let painter = ui.painter_at(bar_rect);
        painter.rect_filled(bar_rect, 2.0, theme.bg_deep);
        let fill_width = bar_rect.width() * job.progress.clamp(0.0, 1.0);
        if fill_width > 0.5 {
            let fill_color = match job.status {
                RenderJobStatus::Rendering => theme.accent_dim,
                // Darkened, so the white text over them reads.
                RenderJobStatus::Complete => theme.marker_success.gamma_multiply(0.45),
                RenderJobStatus::Failed => theme.marker_error.gamma_multiply(0.5),
                _ => theme.bg_hover,
            };
            painter.rect_filled(egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_width, Self::BAR_HEIGHT)), 2.0, fill_color);
        }
        let font = egui::FontId::proportional(11.0);
        painter.text(bar_rect.center() + egui::vec2(1.0, 1.0), egui::Align2::CENTER_CENTER, &bar_text, font.clone(), egui::Color32::BLACK);
        painter.text(bar_rect.center(), egui::Align2::CENTER_CENTER, &bar_text, font, egui::Color32::WHITE);
        ui.add_space(2.0);

        // ── Row 4: where it goes ──
        // The button first, from the right, and the path in what is left,
        // shortened at its front: a long path laid out whole widened every
        // card past the panel, and Cancel and Remove went out of reach.
        // In a row of its own height: laid out straight in the scrolling
        // list, the row centred itself in all the height left, and the
        // first card filled the window with the others below it.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if job.status != RenderJobStatus::Complete
                    && ui.add(egui::Button::new(small("Show folder".into(), theme.text_secondary)).frame(false)).clicked()
                {
                    actions.push(RenderQueueAction::ShowInFolder(job.output_path.clone()));
                }
                let room = (ui.available_width() / 6.0).floor().max(12.0) as usize;
                ui.add(egui::Label::new(small(format!("\u{2192} {}", elide_front(&Self::shown_path(&job.output_path), room)), theme.text_dim)).truncate())
                    .on_hover_text(job.output_path.display().to_string());
            });
        });

        // ── Row 5: how it is going (rendering) ──
        if job.status == RenderJobStatus::Rendering {
            let mut parts = Vec::new();
            if elapsed_secs > 0 && job.frames_done > 0 {
                let render_fps = job.frames_done as f64 / elapsed_secs as f64;
                if job.timeline_fps > 0.0 {
                    parts.push(format!("{:.1}\u{00D7} real-time", render_fps / job.timeline_fps));
                }
                parts.push(format!("{render_fps:.0} fps"));
            }
            if elapsed_secs > 0 {
                parts.push(format!("{} elapsed", Self::duration(elapsed_secs)));
            }
            // The size it will be, measured from what is written (#19).
            if let Some(bytes) = job.projected_bytes() {
                parts.push(format!("\u{2248} {} when done", Self::size(bytes)));
            }
            if !job.hw_info.is_empty() {
                parts.push(job.hw_info.replace("Decode: ", "").replace("Encode: ", "").replace(" | ", " \u{2022} "));
            }
            // The stage, when it says more than the codec already shown:
            // muxing, the loudness pass, finishing the sound.
            if !job.render_stage.is_empty() && !job.render_stage.starts_with("Encoding video") {
                parts.push(job.render_stage.clone());
            }
            if !parts.is_empty() {
                ui.label(small(parts.join(" \u{2022} "), theme.text_dim));
            }
        }

        // ── Where the time goes, and where frames were composited
        //    (#1924): while it renders, and kept once it ends ──
        if let Some(line) = job.timings.as_ref().and_then(Self::timings_line) {
            ui.label(small(line, theme.text_dim));
        }

        // ── Why it failed ──
        if let Some(err) = &job.error {
            ui.label(small(err.clone(), theme.marker_error));
        }

        // ── Details: the rest of the settings ──
        let details_id = egui::Id::new("rq_details").with(job.id);
        let mut show_details = ui.data_mut(|d| *d.get_temp_mut_or_default::<bool>(details_id));
        let details_label = if show_details { "\u{25BE} Details" } else { "\u{25B8} Details" };
        if ui.add(egui::Button::new(small(details_label.into(), theme.text_dim)).frame(false)).clicked() {
            show_details = !show_details;
        }
        ui.data_mut(|d| d.insert_temp(details_id, show_details));
        if show_details {
            ui.indent("rq_detail_indent", |ui| {
                let dim = theme.text_dim;
                let s = &job.settings;
                let quality = if s.video_codec.uses_crf() {
                    format!("CRF {}", s.crf)
                } else if let Some(kbps) = s.video_bitrate_kbps {
                    format!("{kbps} kbps")
                } else {
                    "codec default".into()
                };
                // Sound alone has no picture to describe (P16).
                if !s.format.is_audio_only() {
                    ui.label(small(format!("Video: {} \u{2022} {quality} \u{2022} {}-bit \u{2022} encoder preset {}", s.video_codec.label(), s.bit_depth, s.preset.ffmpeg_value()), dim));
                }
                let audio = s.audio_bitrate_kbps.map(|b| format!(" {b} kbps")).unwrap_or_default();
                ui.label(small(format!("Audio: {}{audio}", s.audio_codec.label()), dim));
                if let Some(lufs) = s.target_lufs {
                    ui.label(small(format!("Loudness target: {lufs:.0} LUFS"), dim));
                }
                if let Some(ref lr) = job.loudness_result {
                    ui.label(small(lr.clone(), dim));
                }
                ui.add(egui::Label::new(small(format!("Full path: {}", job.output_path.display()), dim)).wrap());
            });
        }
    }

    /// A render's timings in a line: per frame the render (its decode
    /// inside it), the encode and the sound, where frames were composited,
    /// and the loudness fit's trials before it. `None` before a frame.
    fn timings_line(t: &klipp_core::ExportTimings) -> Option<String> {
        if t.frames == 0 {
            return None;
        }
        let mut line = if t.gpu_composited + t.cpu_composited == 0 {
            // Sound alone.
            format!("Sound {:.1} ms per frame", t.audio_ms)
        } else {
            format!("Render {:.1} ms \u{00B7} decode {:.1} \u{00B7} encode {:.1} \u{00B7} audio {:.1} per frame", t.render_ms, t.decode_ms, t.encode_ms, t.audio_ms)
        };
        if let Some(composited) = t.compositing() {
            line.push_str(&format!(" \u{2014} composited on the {composited}"));
        }
        if t.loudness_trials > 0 {
            line.push_str(&format!(" \u{2022} loudness fit {:.1} s, {} trial{}", t.loudness_verify_secs, t.loudness_trials, if t.loudness_trials == 1 { "" } else { "s" }));
        }
        Some(line)
    }

    /// The job at a glance: picture, rate, codec and quality, sound, and
    /// how long, with the stretch when it is not the whole sequence.
    fn identity(job: &klipp_export::RenderJob) -> String {
        let s = &job.settings;
        let mut parts = picture_parts(s, job.width, job.height, job.timeline_fps);
        parts.push(match s.audio_bitrate_kbps {
            Some(b) => format!("{} {b} kbps", s.audio_codec.label()),
            None => s.audio_codec.label().to_string(),
        });
        if job.total_frames > 0 && job.timeline_fps > 0.0 {
            let secs = (job.total_frames as f64 / job.timeline_fps).round() as u64;
            let mut length = format!("{} ({} frames)", Self::clock(secs), Self::thousands(job.total_frames));
            if let Some((a, b)) = job.export_range {
                let _ = std::fmt::Write::write_fmt(&mut length, format_args!(", frames {}\u{2013}{}", Self::thousands(a), Self::thousands(b.saturating_sub(1))));
            }
            parts.push(length);
        }
        parts.join(" \u{2022} ")
    }

    /// The path as a person reads it: the home directory as `~`.
    fn shown_path(path: &std::path::Path) -> String {
        let full = path.display().to_string();
        match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && full.starts_with(&format!("{home}/")) => format!("~{}", &full[home.len()..]),
            _ => full,
        }
    }

    fn thousands(n: u64) -> String {
        let digits = n.to_string();
        let mut out = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
        }
        out
    }

    /// `m:ss`, or `h:mm:ss` past the hour.
    fn clock(secs: u64) -> String {
        if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        }
    }

    /// `1.2 GB`, or `340 MB` under a gigabyte.
    fn size(bytes: u64) -> String {
        if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1e9) } else { format!("{:.0} MB", bytes as f64 / 1e6) }
    }

    /// `1m 05s`, or `42s` under a minute.
    fn duration(secs: u64) -> String {
        if secs >= 60 { format!("{}m {:02}s", secs / 60, secs % 60) } else { format!("{secs}s") }
    }

    fn format_bar_text(job: &klipp_export::RenderJob, elapsed_secs: u64) -> String {
        let frames = || format!("frame {} of {}", Self::thousands(job.frames_done.min(job.total_frames)), Self::thousands(job.total_frames));
        match job.status {
            RenderJobStatus::Rendering => {
                let pct = (job.progress * 100.0) as u32;
                let when = if job.progress > 0.01 && elapsed_secs > 2 {
                    let remaining = (elapsed_secs as f64 / job.progress as f64 - elapsed_secs as f64).max(0.0) as u64;
                    // When it will be done, as a time of day: what someone
                    // stepping away wants to know.
                    let done = chrono::Local::now() + chrono::Duration::seconds(remaining as i64);
                    format!("{} left, done \u{2248} {}", Self::duration(remaining), done.format("%H:%M"))
                } else {
                    "calculating\u{2026}".to_string()
                };
                if job.total_frames > 0 {
                    format!("{pct}%  \u{2022}  {}  \u{2022}  {when}", frames())
                } else {
                    format!("{pct}%  \u{2022}  {when}")
                }
            }
            RenderJobStatus::Complete => {
                let mut text = match (job.started_at, job.completed_at) {
                    (Some(start), Some(end)) => {
                        let secs = end.duration_since(start).as_secs();
                        let speed = if secs > 0 && job.total_frames > 0 && job.timeline_fps > 0.0 {
                            format!(" ({:.1}\u{00D7} real-time)", job.total_frames as f64 / secs as f64 / job.timeline_fps)
                        } else {
                            String::new()
                        };
                        format!("Done in {}{speed}", Self::duration(secs))
                    }
                    _ => "Done".to_string(),
                };
                if let Ok(m) = std::fs::metadata(&job.output_path) {
                    let _ = std::fmt::Write::write_fmt(&mut text, format_args!("  \u{2022}  {:.1} MB", m.len() as f64 / 1_000_000.0));
                }
                text
            }
            RenderJobStatus::Failed if job.total_frames > 0 && job.frames_done > 0 => format!("Failed at {}", frames()),
            RenderJobStatus::Failed => "Failed".to_string(),
            RenderJobStatus::Cancelled if job.total_frames > 0 && job.frames_done > 0 => format!("Cancelled at {}", frames()),
            RenderJobStatus::Cancelled => "Cancelled".to_string(),
            RenderJobStatus::Queued => "Waiting in queue\u{2026}".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_times_read_as_people_write_them() {
        assert_eq!(RenderQueuePanel::thousands(3484), "3,484");
        assert_eq!(RenderQueuePanel::thousands(999), "999");
        assert_eq!(RenderQueuePanel::thousands(1_234_567), "1,234,567");
        assert_eq!(RenderQueuePanel::clock(116), "1:56");
        assert_eq!(RenderQueuePanel::clock(3725), "1:02:05");
        assert_eq!(RenderQueuePanel::duration(71), "1m 11s");
        assert_eq!(RenderQueuePanel::duration(42), "42s");
    }
}

/// `text` in at most `chars` characters, its end kept and its front cut
/// to an ellipsis: a path's file name and nearest folders are what tell
/// one render from another.
fn elide_front(text: &str, chars: usize) -> String {
    let n = text.chars().count();
    if n <= chars {
        return text.to_string();
    }
    let keep = chars.saturating_sub(1);
    let tail: String = text.chars().skip(n - keep).collect();
    format!("\u{2026}{tail}")
}

#[cfg(test)]
mod elide_tests {
    /// A long path keeps its end, the file name, and fits its room.
    #[test]
    fn a_long_path_keeps_its_file_name_and_fits() {
        let long = format!("~/{}/final_cut_v3.mp4", "very_long_folder_name/".repeat(12));
        let shown = super::elide_front(&long, 40);
        assert_eq!(shown.chars().count(), 40);
        assert!(shown.starts_with('\u{2026}') && shown.ends_with("final_cut_v3.mp4"), "{shown}");
        assert_eq!(super::elide_front("~/a.mp4", 40), "~/a.mp4");
    }
}

/// A job's picture at a glance: its size, rate and codec with its quality.
/// Nothing for sound alone (a WAV, MP3 or M4A): it said "H.264 CRF 23" for
/// a .wav (P16).
fn picture_parts(s: &klipp_core::ExportSettings, width: u32, height: u32, fps: f64) -> Vec<String> {
    if s.format.is_audio_only() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    if width > 0 && height > 0 {
        parts.push(format!("{width}\u{00D7}{height}"));
    }
    if fps > 0.0 {
        parts.push(if (fps - fps.round()).abs() < 1e-3 { format!("{fps:.0} fps") } else { format!("{fps:.3} fps") });
    }
    parts.push(if s.video_codec.uses_crf() { format!("{} CRF {}", s.video_codec.label(), s.crf) } else { s.video_codec.label().to_string() });
    parts
}

#[cfg(test)]
mod picture_parts_tests {
    /// P16: an audio-only export's card names no picture.
    #[test]
    fn sound_alone_names_no_picture() {
        let mut s = klipp_core::ExportSettings::default();
        assert!(super::picture_parts(&s, 1920, 1080, 25.0).iter().any(|p| p.contains("CRF") || p.contains('\u{00D7}')));
        s.format = klipp_core::ExportFormat::Wav;
        assert!(super::picture_parts(&s, 1920, 1080, 25.0).is_empty());
    }
}


#[cfg(test)]
mod card_height_tests {
    use klipp_export::{RenderJob, RenderJobStatus, RenderQueue};

    fn done(name: &str) -> RenderJob {
        RenderJob {
            id: uuid::Uuid::new_v4(),
            label: name.into(),
            output_path: std::path::PathBuf::from(format!("/tmp/out/{name}")),
            settings: klipp_core::ExportSettings::default(),
            status: RenderJobStatus::Complete,
            progress: 1.0,
            created_at: std::time::Instant::now(),
            started_at: None,
            completed_at: None,
            error: None,
            total_frames: 550,
            frames_done: 550,
            hw_info: String::new(),
            timeline_fps: 25.0,
            render_stage: String::new(),
            loudness_result: None,
            timings: None,
            width: 640,
            height: 360,
            export_range: None,
            sequence_id: klipp_core::SequenceId(uuid::Uuid::nil()),
            heard_media: Vec::new(),
        }
    }

    /// EX6: in the window, each card is as tall as what it holds, so a
    /// queue of several shows them one under another. The path row took
    /// all the height left, and only the first card was seen.
    #[test]
    fn cards_stack_in_the_window() {
        let mut queue = RenderQueue::new();
        queue.jobs = ["j1.mp4", "j2.mp4", "j3.mp4"].into_iter().map(done).collect();
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let theme = crate::theme::KlippTheme::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, 420.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                super::RenderQueuePanel::show_contents(ui, &queue, &theme, false, &mut Vec::new());
            });
        });
        let y = |name: &str| {
            out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == name => Some(t.pos.y),
                _ => None,
            }).unwrap_or_else(|| panic!("{name} not drawn"))
        };
        let (first, second, third) = (y("j1.mp4"), y("j2.mp4"), y("j3.mp4"));
        assert!(second - first < 160.0 && third - second < 160.0, "cards at {first}, {second}, {third}");
        assert!(third < 420.0, "the third card is out of sight at {third}");
    }
}
