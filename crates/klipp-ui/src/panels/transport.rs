use crate::state::AppState;
use klipp_core::{AppCommand, Fps, RationalTime};
use klipp_engine::PlaybackState;

pub struct TransportBar;

impl Default for TransportBar {
    fn default() -> Self {
        Self
    }
}

impl TransportBar {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let enabled = state.playback != PlaybackState::Idle;
        let fps_val = state.fps.as_f64();

        ui.horizontal_centered(|ui| {
            ui.add_space(16.0);

            // Go to start
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23EE}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to start")
                .clicked()
            {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::zero()));
            }

            // -10s
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23EA}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("-10s")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(-(fps_val * 10.0) as i64));
            }

            // Step back (one frame)
            if ui
                .add_enabled(enabled, egui::Button::new("|<").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step back 1 frame")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(-1));
            }

            // Play / Pause
            let play_label = if state.playback == PlaybackState::Playing {
                "\u{23F8}"
            } else {
                "\u{25B6}"
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(play_label).min_size(egui::vec2(40.0, 40.0)),
                )
                .on_hover_text("Play/Pause")
                .clicked()
            {
                if state.playback == PlaybackState::Playing {
                    cmds.push(AppCommand::Pause);
                } else {
                    cmds.push(AppCommand::Play);
                }
            }

            // Step forward (one frame)
            if ui
                .add_enabled(enabled, egui::Button::new(">|").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step forward 1 frame")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(1));
            }

            // +10s
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23E9}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("+10s")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative((fps_val * 10.0) as i64));
            }

            // Go to end
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23ED}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to end")
                .clicked()
            {
                let dur = state.sequence_duration();
                cmds.push(AppCommand::SeekTo(dur));
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
                            .color(egui::Color32::WHITE),
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
                    .color(egui::Color32::from_rgb(100, 100, 110)),
            );

            // Spacer to push master controls right
            ui.add_space(24.0);

            // Master volume slider + mute
            ui.label(egui::RichText::new("Master").size(10.0).color(egui::Color32::from_rgb(120, 120, 130)));
            let mute_color = if state.master_muted {
                egui::Color32::from_rgb(255, 80, 80)
            } else {
                egui::Color32::from_rgb(100, 100, 110)
            };
            if ui.add(egui::Button::new(
                egui::RichText::new("M").size(10.0).color(mute_color),
            ).min_size(egui::vec2(18.0, 16.0))).clicked() {
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
            ui.label(egui::RichText::new(format!("{}%", vol_pct)).size(10.0).color(egui::Color32::from_rgb(140, 140, 150)));

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
                painter.rect_filled(bar, 1.0, egui::Color32::from_rgb(20, 20, 25));
                let fill = (peak.min(1.0) * vu_w).max(0.0);
                if fill > 0.0 {
                    let color = if peak > 0.9 {
                        egui::Color32::from_rgb(220, 50, 50)
                    } else if peak > 0.7 {
                        egui::Color32::from_rgb(220, 180, 40)
                    } else {
                        egui::Color32::from_rgb(50, 180, 70)
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_size(bar.left_top(), egui::vec2(fill, vu_h)),
                        1.0, color,
                    );
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

        if state.playback == PlaybackState::Idle {
            return cmds;
        }

        // Only skip transport shortcuts when actively editing timecode
        if state.editing_timecode {
            return cmds;
        }

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                if state.playback == PlaybackState::Playing {
                    cmds.push(AppCommand::Pause);
                } else {
                    cmds.push(AppCommand::Play);
                }
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                cmds.push(AppCommand::SeekRelative(-1));
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                cmds.push(AppCommand::SeekRelative(1));
            }
            if i.key_pressed(egui::Key::Home) {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::zero()));
            }
            if i.key_pressed(egui::Key::End) {
                let dur = state.sequence_duration();
                cmds.push(AppCommand::SeekTo(dur));
            }
            // J — reverse/faster reverse
            if i.key_pressed(egui::Key::J) {
                let s = state.playback_speed;
                let new_speed = if s > 0.0 { -2.0 }
                    else if s == -2.0 { -4.0 }
                    else if s == -4.0 { -8.0 }
                    else { -2.0 };
                cmds.push(AppCommand::SetSpeed(new_speed));
                if state.playback != PlaybackState::Playing {
                    cmds.push(AppCommand::Play);
                }
            }
            // K — pause, reset speed
            if i.key_pressed(egui::Key::K) {
                cmds.push(AppCommand::SetSpeed(1.0));
                cmds.push(AppCommand::Pause);
            }
            // L — forward/faster forward
            if i.key_pressed(egui::Key::L) {
                let s = state.playback_speed;
                let new_speed = if s < 0.0 { 2.0 }
                    else if s == 2.0 { 4.0 }
                    else if s == 4.0 { 8.0 }
                    else { 2.0 };
                cmds.push(AppCommand::SetSpeed(new_speed));
                if state.playback != PlaybackState::Playing {
                    cmds.push(AppCommand::Play);
                }
            }
        });

        cmds
    }
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
