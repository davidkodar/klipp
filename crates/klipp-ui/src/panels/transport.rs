use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, RationalTime, ViewerMode};
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

#[derive(Default)]
pub struct TransportBar {
    /// J or L pressed with K held (#921): a frame stepped, and after a
    /// moment held, slow play until either key is let go.
    slow: Option<SlowPlay>,
}

/// One J or L pressed while K is held.
struct SlowPlay {
    forward: bool,
    since: std::time::Instant,
    playing: bool,
}

/// How long J or L is held with K before a frame step becomes slow play.
const SLOW_PLAY_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

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
                .add_enabled(enabled, egui::Button::image(Icon::SkipBack.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
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
                .add_enabled(enabled, egui::Button::image(Icon::Rewind.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("-10s")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative(-(fps_val * 10.0) as i64), state));
            }

            // Step back (one frame)
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::CaretLeft.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
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
                    egui::Button::image(play_icon.image(18.0).tint(icon_tint)).min_size(egui::vec2(36.0, 32.0)),
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
                .add_enabled(enabled, egui::Button::image(Icon::CaretRight.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
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
                    .size(16.0);
                let resp = ui
                    .add(
                        egui::Button::new(glyph)
                            .min_size(egui::vec2(32.0, 32.0)),
                    )
                    .on_hover_text(if state.is_recording {
                        "Stop recording"
                    } else {
                        "Record to picture on the armed audio track: playback rolls from the pre-roll before the In (or the playhead), the take lands at the In, and an Out stops it. Your voice is not played back while recording, so it cannot feed back into the take; listen through your interface's direct monitoring."
                    });
                if resp.clicked() {
                    if state.is_recording {
                        cmds.push(AppCommand::StopRecording);
                    } else {
                        cmds.push(AppCommand::StartRecording);
                    }
                }
                // The input's level while a track is armed (#114): seen
                // before rolling, so a take is not recorded clipped.
                let armed = state.sequence().tracks.iter().any(|t| t.record_armed);
                if armed || state.is_recording {
                    let db = 20.0 * state.input_level.max(1e-6).log10();
                    let (rect, meter) = ui.allocate_exact_size(egui::vec2(6.0, 28.0), egui::Sense::hover());
                    let fill = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let colour = if db > -1.0 { theme.marker_error } else if db > -12.0 { theme.marker_warning } else { theme.accent };
                    ui.painter().rect_filled(rect, 2.0, theme.bg_raised);
                    let lit = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - rect.height() * fill), rect.max);
                    ui.painter().rect_filled(lit, 2.0, colour);
                    meter.on_hover_text(format!("Input {db:.0} dBFS{}", if db > -1.0 { " \u{2014} clipping: turn the input down" } else { "" }));
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
                .add_enabled(enabled, egui::Button::image(Icon::FastForward.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("+10s")
                .clicked()
            {
                cmds.push(route_for_viewer(AppCommand::SeekRelative((fps_val * 10.0) as i64), state));
            }

            // Go to end
            if ui
                .add_enabled(enabled, egui::Button::image(Icon::SkipForward.image(16.0).tint(icon_tint)).min_size(egui::vec2(32.0, 32.0)))
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
                    (theme.accent_subtle, theme.accent)
                } else if resp.hovered() {
                    (theme.bg_hover, theme.text_secondary)
                } else {
                    (egui::Color32::TRANSPARENT, theme.text_dim)
                };
                ui.painter().rect_filled(rect, crate::theme::radius::CONTROL, bg);
                // Loop icon: 270° arc + arrowhead
                draw_loop_icon(ui.painter(), rect.center(), 8.0, fg);
                if resp.clicked() {
                    cmds.push(AppCommand::ToggleLoopPlayback);
                }
                resp.on_hover_text("Loop playback (Ctrl+L)");
            }

            // Fullscreen viewer. The shortcut is F11, which a Mac
            // keyboard need not have, so the viewer carries the button
            // too rather than making fullscreen keyboard-only.
            {
                let full = state.fullscreen_viewer;
                let tint = if full { theme.accent } else { icon_tint };
                let icon = if full { Icon::FrameCorners } else { Icon::ArrowsOut };
                if ui
                    .add(egui::Button::image(icon.image(16.0).tint(tint)).min_size(egui::vec2(32.0, 32.0)))
                    .on_hover_text(if full { "Leave fullscreen (Esc)" } else { "Fullscreen viewer" })
                    .clicked()
                {
                    cmds.push(AppCommand::ToggleFullscreen);
                }
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
                        if let Some(tc) = parse_timecode_input(&state.timecode_input, state.timecode_display()) {
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
                            .size(16.0)
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
            let duration_str = state.timecode_display().duration(duration);
            ui.label(
                egui::RichText::new(format!(" / {}", duration_str))
                    .monospace()
                    .size(13.0)
                    .color(theme.text_dim),
            );

            // Range display (only when in or out is set)
            let has_range = state.sequence().in_point.is_some() || state.sequence().out_point.is_some();
            if has_range {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                let amber = theme.text_accent;
                let dim = theme.text_dim;

                if let Some(in_pt) = state.sequence().in_point {
                    ui.label(egui::RichText::new("In ").size(10.0).color(dim));
                    ui.label(egui::RichText::new(
                        crate::timeline::range_visuals::format_handle_timecode(in_pt.to_secs_f64(), state.timecode_display()),
                    ).monospace().size(11.0).color(amber));
                }

                ui.label(egui::RichText::new(" \u{2192} ").size(10.0).color(dim));

                if let Some(out_pt) = state.sequence().out_point {
                    ui.label(egui::RichText::new(
                        crate::timeline::range_visuals::format_handle_timecode(out_pt.to_secs_f64(), state.timecode_display()),
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
                        .size(10.0)
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
                if ui.add(egui::Button::image(Icon::XCircle.image(14.0).tint(theme.marker_error))
                    .min_size(egui::vec2(18.0, 18.0))).on_hover_text("Cancel render").clicked() {
                    cmds.push(AppCommand::CancelExport);
                }
            }
        });

        cmds
    }

    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        state: &AppState,
        kb: &crate::keybindings::KeyBindings,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        let has_clips = state.sequences.get(state.active_sequence_idx)
            .map(|seq| seq.tracks.iter().any(|t| !t.clips.is_empty()))
            .unwrap_or(false);
        if state.playback == PlaybackState::Idle && !has_clips {
            return cmds;
        }

        // Skip transport shortcuts when any text field has focus, a
        // modal overlay is open (Quick Look, preferences, dialogs) or no
        // project is (the welcome screen, #2551).
        if state.project_path.is_none()
            || state.editing_timecode
            || ctx.wants_keyboard_input()
            || state.has_modal_open()
        {
            return cmds;
        }

        let text_editing = ctx.wants_keyboard_input() || state.editing_timecode || state.viewer_text_edit;
        // What Shift+Left and Shift+Right move, in frames (what
        // SeekRelative counts): the preference, or one second. Not
        // `fps.rate()`, the numerator: 30000 at 29.97 (#921).
        let shift_step = crate::navigation::shift_step_frames(state.shift_step_frames, state.fps.as_f64());
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
            if !text_editing && !state.files_panel_focused && kb.is_action_pressed("play_pause", i) {
                if focus_playback == PlaybackState::Playing {
                    cmds.push(route_for_viewer(AppCommand::Pause, state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                }
            }
            if !text_editing && !i.modifiers.alt
                && (kb.is_action_pressed("step_backward", i) || (i.modifiers.shift && i.key_pressed(egui::Key::ArrowLeft))) {
                if i.modifiers.shift {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(-shift_step), state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(-1), state));
                }
            }
            if !text_editing && !i.modifiers.alt
                && (kb.is_action_pressed("step_forward", i) || (i.modifiers.shift && i.key_pressed(egui::Key::ArrowRight))) {
                if i.modifiers.shift {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(shift_step), state));
                } else {
                    cmds.push(route_for_viewer(AppCommand::SeekRelative(1), state));
                }
            }
            if !text_editing && kb.is_action_pressed("go_to_start", i) {
                cmds.push(route_for_viewer(
                    AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate())),
                    state,
                ));
            }
            if !text_editing && kb.is_action_pressed("go_to_end", i) {
                let dur = state.sequence_duration();
                cmds.push(route_for_viewer(AppCommand::SeekTo(dur), state));
            }
            // J, K and L are a node canvas's while it has the keys: L lays
            // its nodes out (#1742). Space and the arrows still play.
            let shuttle_keys = !text_editing && !state.node_canvas_focused();
            // K held: J and L step a frame, and held on, play slowly (#921).
            let k_held = shuttle_keys && kb.is_action_down("shuttle_pause", i);
            // Dynamic trimming (#914): J or L with an edit point selected,
            // on the program, plays from its cut; K then commits the trim.
            let arms_trim = state.viewer_mode != ViewerMode::Source
                && !state.selected_edits.is_empty()
                && state.dynamic_trim.is_none()
                && focus_playback != PlaybackState::Playing;
            let jl = kb.is_action_pressed("shuttle_reverse", i) || kb.is_action_pressed("shuttle_forward", i);
            if shuttle_keys && jl && arms_trim {
                cmds.push(AppCommand::BeginDynamicTrim);
            }
            // J — reverse, normal speed first, then faster
            if shuttle_keys && kb.is_action_pressed("shuttle_reverse", i) {
                if k_held {
                    self.start_slow(false, &mut cmds, state);
                } else {
                    let playing = focus_playback == PlaybackState::Playing;
                    let new_speed = crate::navigation::shuttle_speed(state.playback_speed, playing, false, state.jkl_speed_step);
                    cmds.push(route_for_viewer(AppCommand::SetSpeed(new_speed), state));
                    if !playing {
                        cmds.push(route_for_viewer(AppCommand::Play, state));
                    }
                }
            }
            // K — pause, reset speed
            if shuttle_keys && kb.is_action_pressed("shuttle_pause", i) {
                cmds.push(route_for_viewer(AppCommand::SetSpeed(1.0), state));
                cmds.push(route_for_viewer(AppCommand::Pause, state));
                if state.dynamic_trim.is_some() {
                    cmds.push(AppCommand::CommitDynamicTrim);
                }
            }
            // L — forward, normal speed first, then faster
            if shuttle_keys && kb.is_action_pressed("shuttle_forward", i) {
                if k_held {
                    self.start_slow(true, &mut cmds, state);
                } else {
                    let playing = focus_playback == PlaybackState::Playing;
                    let new_speed = crate::navigation::shuttle_speed(state.playback_speed, playing, true, state.jkl_speed_step);
                    cmds.push(route_for_viewer(AppCommand::SetSpeed(new_speed), state));
                    if !playing {
                        cmds.push(route_for_viewer(AppCommand::Play, state));
                    }
                }
            }
            if let Some(slow) = &mut self.slow {
                let key = if slow.forward { "shuttle_forward" } else { "shuttle_reverse" };
                if !k_held || !kb.is_action_down(key, i) {
                    if slow.playing {
                        cmds.push(route_for_viewer(AppCommand::Pause, state));
                        cmds.push(route_for_viewer(AppCommand::SetSpeed(1.0), state));
                    }
                    self.slow = None;
                } else if !slow.playing && slow.since.elapsed() >= SLOW_PLAY_AFTER {
                    let speed = if slow.forward { crate::navigation::SLOW_SPEED } else { -crate::navigation::SLOW_SPEED };
                    cmds.push(route_for_viewer(AppCommand::SetSpeed(speed), state));
                    cmds.push(route_for_viewer(AppCommand::Play, state));
                    slow.playing = true;
                }
            }
            // Markers, and Play In to Out (#921)
            if !text_editing && kb.is_action_pressed("next_marker", i) {
                if let Some(at) = crate::navigation::marker_from(state.sequence(), state.playhead, true) {
                    cmds.push(AppCommand::SeekTo(at));
                }
            }
            if !text_editing && kb.is_action_pressed("prev_marker", i) {
                if let Some(at) = crate::navigation::marker_from(state.sequence(), state.playhead, false) {
                    cmds.push(AppCommand::SeekTo(at));
                }
            }
            if !text_editing && kb.is_action_pressed("play_in_to_out", i) {
                cmds.push(AppCommand::PlayInToOut);
            }
        });
        // Held keys are read each frame: keep frames coming while one is.
        if self.slow.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }

        cmds
    }
}

impl TransportBar {
    /// J or L pressed with K held: one frame that way now, and slow play
    /// if it is still held after `SLOW_PLAY_AFTER`.
    fn start_slow(&mut self, forward: bool, cmds: &mut Vec<AppCommand>, state: &AppState) {
        cmds.push(route_for_viewer(AppCommand::SeekRelative(if forward { 1 } else { -1 }), state));
        self.slow = Some(SlowPlay { forward, since: std::time::Instant::now(), playing: false });
    }
}

pub fn parse_timecode_input_pub(input: &str, tc: klipp_core::TimecodeDisplay) -> Option<RationalTime> {
    parse_timecode_input(input, tc)
}

fn parse_timecode_input(input: &str, tc: klipp_core::TimecodeDisplay) -> Option<RationalTime> {
    let input = input.trim();
    // A timecode, counted the way this sequence counts — and a
    // semicolon is honoured whatever the sequence is set to, so a
    // timecode copied off a camera report can be pasted as written.
    if let Some(t) = tc.parse(input) {
        return Some(t);
    }
    let fps = tc.fps;
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

#[cfg(test)]
mod jkl_step_tests {
    use super::*;

    /// L pressed once, playing at `speed` with JKL steps `step`: the
    /// speed it asks for.
    fn press_l(speed: f64, step: f64) -> Option<f64> {
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let (zero, end) = (RationalTime { value: 0, rate: 24 }, RationalTime { value: 240, rate: 24 });
        state.sequences[0].tracks[v1].clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), zero, end, zero, end));
        state.project_path = Some(crate::project_paths::UNTITLED.into());
        state.playback = PlaybackState::Playing;
        state.playback_speed = speed;
        state.jkl_speed_step = step;
        let kb = crate::keybindings::KeyBindings::default();
        let mut panel = TransportBar::default();
        let ctx = egui::Context::default();
        let mut cmds = Vec::new();
        let key = |pressed| egui::Event::Key { key: egui::Key::L, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        let _ = ctx.run(egui::RawInput { events: vec![key(true)], ..Default::default() }, |ctx| {
            cmds = panel.handle_keyboard(ctx, &state, &kb);
        });
        cmds.iter().find_map(|c| match c {
            AppCommand::SetSpeed(s) => Some(*s),
            _ => None,
        })
    }

    /// #1816: L while playing multiplies the speed by the JKL step the
    /// preferences hold: 3 takes 3x to 9x, 2 takes 2x to 4x.
    #[test]
    fn l_steps_by_the_preference() {
        assert_eq!(press_l(3.0, 3.0), Some(9.0));
        assert_eq!(press_l(2.0, 2.0), Some(4.0));
    }
}
