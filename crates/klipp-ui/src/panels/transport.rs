use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Fps, RationalTime, ViewerMode};
use klipp_engine::PlaybackState;

/// Re-route a timeline-targeted transport command to the source
/// viewer when `mode == Source`. Lets transport callers stay source-
/// agnostic: push the timeline command, wrap through this function,
/// and the right variant lands in the command queue. Commands with
/// no source equivalent (multi-clip edits, timeline-specific ops)
/// pass through unchanged — the source viewer just ignores them.
pub fn route_command_for_viewer(cmd: AppCommand, mode: ViewerMode) -> AppCommand {
    if mode == ViewerMode::Program {
        return cmd;
    }
    match cmd {
        AppCommand::Play => AppCommand::PlaySource,
        AppCommand::Pause => AppCommand::PauseSource,
        AppCommand::SeekRelative(frames) => AppCommand::StepSource(frames),
        AppCommand::SeekTo(tc) => AppCommand::SeekSource(tc),
        // SetSpeed has no source equivalent (source plays at 1× only).
        // Drop it by returning a no-op — we use PauseSource as the
        // harmless noop since the engine ignores it when already
        // paused. Cleaner than adding a NoOp variant.
        AppCommand::SetSpeed(_) => AppCommand::PauseSource,
        other => other,
    }
}

/// Convenience wrapper: route based on the user's currently-focused
/// viewer (`state.viewer_mode`). Used by the global transport bar
/// and keyboard handlers that don't already know which viewer they
/// belong to. Per-viewer playbars should use `route_command_for_viewer`
/// directly with their own `viewer_kind`.
fn route_for_viewer(cmd: AppCommand, state: &AppState) -> AppCommand {
    route_command_for_viewer(cmd, state.viewer_mode)
}

pub struct TransportBar;

impl Default for TransportBar {
    fn default() -> Self {
        Self
    }
}

impl TransportBar {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // Transport is enabled if there's something to play: either the engine
        // has a file loaded (Stopped/Paused/Playing) or the timeline has clips
        // (the sequence has at least one non-empty track — handles generator-only timelines
        // where the engine never leaves Idle because no media file was loaded).
        let has_clips = state.sequences.get(state.active_sequence_idx)
            .map(|seq| seq.tracks.iter().any(|t| !t.clips.is_empty()))
            .unwrap_or(false);
        let enabled = state.playback != PlaybackState::Idle || has_clips;
        let fps_val = state.fps.as_f64();

        ui.horizontal_centered(|ui| {
            ui.add_space(16.0);

            let icon_tint = theme.text_primary;

            // Go to start
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::SkipBack.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to start")
                .clicked()
            {
                cmds.push(route_for_viewer(
                    AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate())),
                    state,
                ));
            }

            // -10s
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::Rewind.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("-10s")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative(-(fps_val * 10.0) as i64), state));
            }

            // Step back (one frame)
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::CaretLeft.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step back 1 frame")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative(-1), state));
            }

            // Play / Pause — icon reflects whichever viewer currently
            // has focus so the button accurately advertises what
            // clicking it will do.
            let focus_playback_for_icon = if state.viewer_mode == ViewerMode::Source {
                state.source_playback
            } else {
                state.playback
            };
            let play_icon = if focus_playback_for_icon == PlaybackState::Playing {
                Icon::Pause
            } else {
                Icon::Play
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::image(play_icon.bold(20.0).tint(icon_tint)).min_size(egui::vec2(40.0, 40.0)),
                )
                .on_hover_text("Play/Pause")
                .clicked()
            {
                let focus_playback = if state.viewer_mode == ViewerMode::Source {
                    state.source_playback
                } else {
                    state.playback
                };
                if focus_playback == PlaybackState::Playing {
                    cmds.push(route_for_viewer(AppCommand::Pause, state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                }
            }

            // Step forward (one frame)
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::CaretRight.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step forward 1 frame")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative(1), state));
            }

            // Record button (#60).
            // Dim red when idle (needs an armed track to do anything),
            // bright red when actively recording. Clicking toggles —
            // StartRecording checks for an armed track and shows a toast
            // if none exists, so the button is always enabled.
            {
                let rec_color = if state.is_recording {
                    theme.marker_error
                } else {
                    theme.accent_dim
                };
                // U+25CF BLACK CIRCLE — renders in egui's default font set.
                // U+23FA (the proper "record" glyph) is in Misc Technical and
                // isn't covered by the default fallback chain, so use the
                // filled circle that's already used elsewhere in the UI.
                let glyph = egui::RichText::new("\u{25CF}")
                    .color(rec_color)
                    .size(18.0);
                let resp = ui
                    .add(
                        egui::Button::new(glyph)
                            .min_size(egui::vec2(32.0, 32.0)),
                    )
                    .on_hover_text(if state.is_recording {
                        "Stop recording"
                    } else {
                        "Start recording (arm an audio track first)"
                    });
                if resp.clicked() {
                    if state.is_recording {
                        cmds.push(AppCommand::StopRecording);
                    } else {
                        cmds.push(AppCommand::StartRecording);
                    }
                }
            }

            // Jog wheel — drag horizontally to scrub frame by frame.
            // A software substitute for hardware JogShuttle devices;
            // emits JogFrames which routes through the existing
            // SeekRelative plumbing. Sized to fit the 44px transport
            // bar with a small margin.
            ui.add_space(6.0);
            let jog_frames = crate::widgets::jog_wheel::jog_wheel(
                ui,
                &mut state.jog_wheel_state,
                theme,
                16.0,
            );
            if jog_frames != 0 {
                cmds.push(AppCommand::JogFrames { frames: jog_frames });
            }
            ui.add_space(6.0);

            // +10s
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::FastForward.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("+10s")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative((fps_val * 10.0) as i64), state));
            }

            // Go to end
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::SkipForward.bold(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to end")
                .clicked()
            {
                let dur = state.sequence_duration();
                cmds.push(route_for_viewer(AppCommand::SeekTo(dur), state));
            }

            // Loop button
            ui.add_space(4.0);
            {
                let loop_active = state.loop_playback_enabled;
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(32.0, 32.0),
                    egui::Sense::click(),
                );
                let (bg, fg) = if loop_active {
                    (theme.accent, theme.text_on_accent)
                } else if resp.hovered() {
                    (theme.bg_raised, theme.text_secondary)
                } else {
                    (egui::Color32::TRANSPARENT, theme.text_dim)
                };
                ui.painter().rect_filled(rect, rect.height() / 2.0, bg);
                // Loop icon: 270° arc + arrowhead
                draw_loop_icon(ui.painter(), rect.center(), 8.0, fg);
                if resp.clicked() {
                    cmds.push(AppCommand::ToggleLoopPlayback);
                }
                resp.on_hover_text("Loop playback (Ctrl+L)");
            }

            // Timecode display — clickable for input
            ui.add_space(24.0);

            if state.editing_timecode {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.timecode_input)
                        .desired_width(120.0)
                        .font(egui::TextStyle::Monospace),
                );
                // Request focus only on the first frame (when not yet focused)
                if !response.has_focus() {
                    response.request_focus();
                }
                // Enter confirms and seeks, Escape cancels
                if response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    let confirmed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if confirmed {
                        if let Some(tc) = parse_timecode_input(&state.timecode_input, state.fps) {
                            cmds.push(AppCommand::SeekTo(tc));
                        }
                    }
                    state.editing_timecode = false;
                }
            } else {
                // Use smooth float for display — shows HH:MM:SS.mmm
                let playhead_str = {
                    let s = state.playhead_smooth_secs;
                    let sign = if s < 0.0 { "-" } else { "" };
                    let s = s.abs();
                    let hh = (s / 3600.0) as u32;
                    let mm = ((s % 3600.0) / 60.0) as u32;
                    let ss = (s % 60.0) as u32;
                    let ms = ((s % 1.0) * 1000.0) as u32;
                    format!("{sign}{hh:02}:{mm:02}:{ss:02}.{ms:03}")
                };
                let tc_response = ui.add(
                    egui::Label::new(
                        egui::RichText::new(&playhead_str)
                            .monospace()
                            .size(18.0)
                            .color(theme.text_primary),
                    )
                    .sense(egui::Sense::click()),
                );
                if tc_response.clicked() {
                    state.editing_timecode = true;
                    state.timecode_input = playhead_str;
                }
                tc_response.on_hover_text("Click to type timecode");
            }

            let duration = state.sequence_duration();
            let duration_str = duration.to_timecode_string();
            ui.label(
                egui::RichText::new(format!(" / {}", duration_str))
                    .monospace()
                    .size(14.0)
                    .color(theme.text_dim),
            );

            // Range display (only when in or out is set)
            let has_range = state.sequence().in_point.is_some() || state.sequence().out_point.is_some();
            if has_range {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                let amber = theme.accent;
                let dim = theme.text_dim;

                if let Some(in_pt) = state.sequence().in_point {
                    ui.label(egui::RichText::new("In ").size(10.0).color(dim));
                    ui.label(egui::RichText::new(
                        crate::timeline::range_visuals::format_handle_timecode(in_pt.to_secs_f64(), in_pt.rate),
                    ).monospace().size(11.0).color(amber));
                }

                ui.label(egui::RichText::new(" \u{2192} ").size(10.0).color(dim));

                if let Some(out_pt) = state.sequence().out_point {
                    ui.label(egui::RichText::new(
                        crate::timeline::range_visuals::format_handle_timecode(out_pt.to_secs_f64(), out_pt.rate),
                    ).monospace().size(11.0).color(amber));
                    ui.label(egui::RichText::new(" Out").size(10.0).color(dim));
                }

                if let (Some(in_pt), Some(out_pt)) = (state.sequence().in_point, state.sequence().out_point) {
                    let dur_secs = (out_pt - in_pt).to_secs_f64();
                    ui.label(egui::RichText::new(
                        format!(" = {}", crate::timeline::range_visuals::format_range_duration(dur_secs)),
                    ).monospace().size(11.0).color(amber));
                }

                // Clear button
                let clear_resp = ui.add(
                    egui::Button::image(Icon::X.image(12.0).tint(amber))
                        .frame(false)
                );
                if clear_resp.clicked() {
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                }
                clear_resp.on_hover_text("Clear range (Alt+X)");
            }

            // Spacer to push master controls right
            ui.add_space(24.0);

            // Master volume slider + mute
            ui.label(egui::RichText::new("Master").size(10.0).color(theme.text_dim));
            let mute_color = if state.master_muted {
                theme.marker_error
            } else {
                theme.text_dim
            };
            let mute_icon = if state.master_muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
            if ui.add(egui::Button::image(mute_icon.image(12.0).tint(mute_color))
                .min_size(egui::vec2(18.0, 16.0))).clicked() {
                state.master_muted = !state.master_muted;
                let effective = if state.master_muted { 0.0 } else { state.master_volume };
                cmds.push(AppCommand::SetMasterVolume(effective));
            }
            let mut vol = state.master_volume;
            let slider = egui::Slider::new(&mut vol, 0.0..=1.3)
                .show_value(false);
            if ui.add(slider).changed() {
                state.master_volume = vol;
                let effective = if state.master_muted { 0.0 } else { vol };
                cmds.push(AppCommand::SetMasterVolume(effective));
            }
            let vol_pct = (state.master_volume * 100.0) as u32;
            ui.label(egui::RichText::new(format!("{}%", vol_pct)).size(10.0).color(theme.text_dim));

            // ── System audio mute warning ──
            // When the OS-level app stream is muted (e.g. user clicked
            // the speaker icon on the taskbar), tint the mute button red
            // and change the master label to warn them.
            if state.system_audio_muted {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("\u{26a0} MUTED BY SYSTEM")
                        .size(9.0)
                        .strong()
                        .color(theme.marker_error)
                        .background_color(egui::Color32::from_rgba_unmultiplied(theme.marker_error.r(), theme.marker_error.g(), theme.marker_error.b(), 30)),
                )
                .on_hover_text(
                    "Klipp's audio is muted at the OS level.\n\
                     Click the speaker icon in your system taskbar to unmute.",
                );
            }

            // Master VU bars (compact)
            let vu_w = 60.0;
            let vu_h = 4.0;
            let (vu_rect, _) = ui.allocate_exact_size(egui::vec2(vu_w, vu_h * 2.0 + 2.0), egui::Sense::hover());
            let painter = ui.painter();
            for (ch, peak) in [(0, state.master_peak_left), (1, state.master_peak_right)] {
                let bar = egui::Rect::from_min_size(
                    egui::pos2(vu_rect.left(), vu_rect.top() + ch as f32 * (vu_h + 1.0)),
                    egui::vec2(vu_w, vu_h),
                );
                painter.rect_filled(bar, 1.0, theme.bg_deep);
                let fill = (peak.min(1.0) * vu_w).max(0.0);
                if fill > 0.0 {
                    let color = if peak > 0.9 {
                        theme.marker_error
                    } else if peak > 0.7 {
                        theme.marker_warning
                    } else {
                        theme.marker_success
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_size(bar.left_top(), egui::vec2(fill, vu_h)),
                        1.0, color,
                    );
                }
            }

            // Render progress indicator
            if let Some((label, progress)) = state.render_queue.active_job_progress() {
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);

                let pct = (progress * 100.0) as u32;
                ui.label(
                    egui::RichText::new(format!("Rendering: {} {}%", label, pct))
                        .size(11.0)
                        .color(theme.accent),
                );

                // Mini progress bar
                let bar_w = 80.0;
                let bar_h = 8.0;
                let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(bar_rect, 2.0, theme.bg_panel_alt);
                let fill_w = (progress * bar_w).max(0.0).min(bar_w);
                if fill_w > 0.0 {
                    painter.rect_filled(
                        egui::Rect::from_min_size(bar_rect.left_top(), egui::vec2(fill_w, bar_h)),
                        2.0,
                        theme.accent,
                    );
                }

                // Cancel button
                if ui.add(egui::Button::image(Icon::XCircle.bold(14.0).tint(theme.marker_error))
                    .min_size(egui::vec2(18.0, 18.0))).on_hover_text("Cancel render").clicked() {
                    cmds.push(AppCommand::CancelExport);
                }
            }
        });

        cmds
    }

    pub fn handle_keyboard(
        &self,
        ctx: &egui::Context,
        state: &AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        let has_clips = state.sequences.get(state.active_sequence_idx)
            .map(|seq| seq.tracks.iter().any(|t| !t.clips.is_empty()))
            .unwrap_or(false);
        if state.playback == PlaybackState::Idle && !has_clips {
            return cmds;
        }

        // Skip transport shortcuts when any text field has focus or a
        // modal overlay is open (Quick Look, preferences, dialogs).
        if state.editing_timecode
            || ctx.wants_keyboard_input()
            || state.has_modal_open()
        {
            return cmds;
        }

        let text_editing = ctx.wants_keyboard_input() || state.editing_timecode || state.viewer_text_edit;
        // One second, in frames (what SeekRelative counts). `fps.rate()`
        // is the numerator — 30000 at 29.97 — not frames a second.
        let fps_rate = state.fps.as_f64().round() as i64;
        ctx.input(|i| {
            // Spacebar: play/pause — UNLESS the file browser has keyboard
            // focus, in which case Spacebar belongs to Quick Look. Only
            // Spacebar is gated on this flag; J/K/L, arrows, and other
            // transport keys work regardless of browser focus so the user
            // doesn't lose audio during file browsing.
            // Which transport state the user's eyes are on — the
            // focused viewer. Space / JKL decisions (toggle play, set
            // speed) read from here so the source viewer doesn't
            // react to the timeline's state and vice versa.
            let focus_playback = if state.viewer_mode == ViewerMode::Source {
                state.source_playback
            } else {
                state.playback
            };
            if !text_editing && !state.files_panel_focused && i.key_pressed(egui::Key::Space) {
                if focus_playback == PlaybackState::Playing {
                    cmds.push(route_for_viewer(AppCommand::Pause, state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                }
            }
            if !text_editing && i.key_pressed(egui::Key::ArrowLeft) && !i.modifiers.alt {
                if i.modifiers.shift {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(-fps_rate), state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(-1), state));
                }
            }
            if !text_editing && i.key_pressed(egui::Key::ArrowRight) && !i.modifiers.alt {
                if i.modifiers.shift {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(fps_rate), state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(1), state));
                }
            }
            if !text_editing && i.key_pressed(egui::Key::Home) {
                cmds.push(route_for_viewer(
                    AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate())),
                    state,
                ));
            }
            if !text_editing && i.key_pressed(egui::Key::End) {
                let dur = state.sequence_duration();
                cmds.push(route_for_viewer(AppCommand::SeekTo(dur), state));
            }
            // J — reverse/faster reverse
            if !text_editing && i.key_pressed(egui::Key::J) {
                let s = state.playback_speed;
                let new_speed = if s > 0.0 { -2.0 }
                    else if s == -2.0 { -4.0 }
                    else if s == -4.0 { -8.0 }
                    else { -2.0 };
                cmds.push(route_for_viewer(AppCommand::SetSpeed(new_speed), state));
                if focus_playback != PlaybackState::Playing {
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                }
            }
            // K — pause, reset speed
            if !text_editing && i.key_pressed(egui::Key::K) {
                cmds.push(route_for_viewer(AppCommand::SetSpeed(1.0), state));
                cmds.push(route_for_viewer(AppCommand::Pause, state));
            }
            // L — forward/faster forward
            if !text_editing && i.key_pressed(egui::Key::L) {
                let s = state.playback_speed;
                let new_speed = if s < 0.0 { 2.0 }
                    else if s == 2.0 { 4.0 }
                    else if s == 4.0 { 8.0 }
                    else { 2.0 };
                cmds.push(route_for_viewer(AppCommand::SetSpeed(new_speed), state));
                if focus_playback != PlaybackState::Playing {
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                }
            }
        });

        cmds
    }
}

pub fn parse_timecode_input_pub(input: &str, fps: Fps) -> Option<RationalTime> {
    parse_timecode_input(input, fps)
}

fn parse_timecode_input(input: &str, fps: Fps) -> Option<RationalTime> {
    let input = input.trim();
    // Try HH:MM:SS:FF format
    if let Some(tc) = parse_hmsf(input, fps.rate()) {
        return Some(tc);
    }
    // Try plain frame number
    if let Ok(frames) = input.parse::<i64>() {
        return Some(RationalTime::from_frames(frames, fps.rate()));
    }
    // Try seconds (with decimal)
    if let Ok(secs) = input.parse::<f64>() {
        return Some(RationalTime::from_secs_f64(secs, fps.rate()));
    }
    None
}

fn parse_hmsf(input: &str, rate: u32) -> Option<RationalTime> {
    let parts: Vec<&str> = input.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    let hh: i64 = parts[0].parse().ok()?;
    let mm: i64 = parts[1].parse().ok()?;
    let ss: i64 = parts[2].parse().ok()?;
    let ff: i64 = parts[3].parse().ok()?;
    let total_frames = ((hh * 3600 + mm * 60 + ss) * rate as i64) + ff;
    Some(RationalTime::from_frames(total_frames, rate))
}

fn draw_loop_icon(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: egui::Color32) {
    // 270° arc (three-quarters circle, clockwise from top)
    let steps = 24;
    let start_angle = -std::f32::consts::FRAC_PI_2; // top
    let sweep = std::f32::consts::PI * 1.5; // 270°
    let pts: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let angle = start_angle + sweep * (i as f32 / steps as f32);
            egui::pos2(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect();

    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5_f32, color)));

    // Arrowhead at the end of the arc (pointing clockwise)
    let end_angle = start_angle + sweep;
    let tip = egui::pos2(
        center.x + radius * end_angle.cos(),
        center.y + radius * end_angle.sin(),
    );
    let arrow_dir = egui::vec2(-end_angle.sin(), end_angle.cos());
    let perp = egui::vec2(arrow_dir.y, -arrow_dir.x);
    let arrow_pts = vec![
        tip,
        tip - arrow_dir * 4.0 + perp * 3.0,
        tip - arrow_dir * 4.0 - perp * 3.0,
    ];
    painter.add(egui::Shape::convex_polygon(arrow_pts, color, egui::Stroke::NONE));
}
