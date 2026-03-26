use crate::icons::Icon;
use crate::theme::KlippTheme;
use crate::widgets::{scrubby_param::scrubby_param_row, waveform};
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;

    // Read current values into local mutable copies
    let mut volume = clip.volume;
    let mut pan = clip.pan;
    let mut fade_in = clip.fade_in_secs;
    let mut fade_out = clip.fade_out_secs;

    // ── Audio section header with global reset ──
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("AUDIO")
                .size(10.0)
                .strong()
                .color(theme.text_dim),
        );

        // Global reset button — only shown when any value differs from default
        let is_default = (volume - 1.0).abs() < f32::EPSILON
            && pan.abs() < f32::EPSILON
            && fade_in.abs() < f32::EPSILON
            && fade_out.abs() < f32::EPSILON;

        if !is_default {
            let (reset_rect, reset_resp) = ui.allocate_exact_size(
                egui::vec2(16.0, 16.0),
                egui::Sense::click(),
            );
            let tint = if reset_resp.hovered() {
                egui::Color32::from_rgb(220, 220, 225)
            } else {
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
            };
            Icon::ArrowCounterClockwise.image(12.0).tint(tint).paint_at(ui, reset_rect);

            if reset_resp.clicked() {
                volume = 1.0;
                pan = 0.0;
                fade_in = 0.0;
                fade_out = 0.0;
                cmds.push(AppCommand::UpdateClipAudio {
                    clip_id,
                    volume,
                    pan,
                    fade_in_secs: fade_in,
                    fade_out_secs: fade_out,
                });
            }
        }
    });
    ui.add_space(7.0);

    // ── Volume ──
    let drag_key = format!("{:?}:audio", clip_id);
    let vol_resp = scrubby_param_row(
        ui, "Volume", &mut volume, 0.0..=2.0, 1.0, 0.005, 2, "",
    );
    if vol_resp.drag_started {
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.clone(),
            clip_id,
            old_volume: clip.volume,
            old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs,
            old_fade_out: clip.fade_out_secs,
        });
    }
    if vol_resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id,
            volume,
            pan,
            fade_in_secs: fade_in,
            fade_out_secs: fade_out,
        });
    }
    if vol_resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.clone(),
            clip_id,
            new_volume: volume,
            new_pan: pan,
            new_fade_in: fade_in,
            new_fade_out: fade_out,
        });
    }

    // ── Pan ──
    let pan_resp = scrubby_param_row(
        ui, "Pan", &mut pan, -1.0..=1.0, 0.0, 0.005, 2, "",
    );
    if pan_resp.drag_started {
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.clone(),
            clip_id,
            old_volume: clip.volume,
            old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs,
            old_fade_out: clip.fade_out_secs,
        });
    }
    if pan_resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id,
            volume,
            pan,
            fade_in_secs: fade_in,
            fade_out_secs: fade_out,
        });
    }
    if pan_resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.clone(),
            clip_id,
            new_volume: volume,
            new_pan: pan,
            new_fade_in: fade_in,
            new_fade_out: fade_out,
        });
    }

    // ── Fade In ──
    let fi_resp = scrubby_param_row(
        ui, "Fade In", &mut fade_in, 0.0..=10.0, 0.0, 0.02, 2, " s",
    );
    if fi_resp.drag_started {
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.clone(),
            clip_id,
            old_volume: clip.volume,
            old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs,
            old_fade_out: clip.fade_out_secs,
        });
    }
    if fi_resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id,
            volume,
            pan,
            fade_in_secs: fade_in,
            fade_out_secs: fade_out,
        });
    }
    if fi_resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.clone(),
            clip_id,
            new_volume: volume,
            new_pan: pan,
            new_fade_in: fade_in,
            new_fade_out: fade_out,
        });
    }

    // ── Fade Out ──
    let fo_resp = scrubby_param_row(
        ui, "Fade Out", &mut fade_out, 0.0..=10.0, 0.0, 0.02, 2, " s",
    );
    if fo_resp.drag_started {
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.clone(),
            clip_id,
            old_volume: clip.volume,
            old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs,
            old_fade_out: clip.fade_out_secs,
        });
    }
    if fo_resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id,
            volume,
            pan,
            fade_in_secs: fade_in,
            fade_out_secs: fade_out,
        });
    }
    if fo_resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.clone(),
            clip_id,
            new_volume: volume,
            new_pan: pan,
            new_fade_in: fade_in,
            new_fade_out: fade_out,
        });
    }

    ui.add_space(14.0);

    // ── Waveform section ──
    ui.label(
        egui::RichText::new("WAVEFORM")
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(7.0);
    let wf_rect_size = egui::vec2(ui.available_width() - 20.0, 48.0);
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(4.0, 4.0))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(wf_rect_size, egui::Sense::hover());
            waveform::draw_role_waveform(ui.painter(), rect, theme.role_dialogue, None);
        });
}
