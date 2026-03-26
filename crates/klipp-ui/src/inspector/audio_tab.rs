use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::{scrubby_param::scrubby_param_row, waveform};
use klipp_core::AppCommand;
use klipp_timeline::{Clip, FadeCurve};

/// Find peak levels for the clip's audio. If the clip is on a video track
/// with a linked audio clip, use the audio track's peaks.
pub fn find_clip_peak_levels(clip: &Clip, track_idx: usize, state: &AppState) -> (f32, f32) {
    // Check if the clip has a linked audio partner on an audio track
    let audio_track_idx = if let Some(linked_id) = clip.linked_clip_id {
        state.sequence().tracks.iter().enumerate()
            .find(|(_, t)| {
                t.kind == klipp_timeline::TrackKind::Audio
                    && t.clips.iter().any(|c| c.id == linked_id)
            })
            .map(|(i, _)| i)
            .unwrap_or(track_idx)
    } else {
        track_idx
    };

    state.track_peak_levels
        .get(audio_track_idx)
        .copied()
        .unwrap_or((0.0, 0.0))
}

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    cmds: &mut Vec<AppCommand>,
    peak: (f32, f32),
    waveform_peaks: Option<&[f32]>,
) {
    let clip_id = clip.id;

    // Read current values into local mutable copies
    let mut volume = clip.volume;
    let mut pan = clip.pan;
    let mut fade_in = clip.fade_in_secs;
    let mut fade_out = clip.fade_out_secs;
    let mut fade_in_curve = clip.fade_in_curve;
    let mut fade_out_curve = clip.fade_out_curve;

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
                fade_in_curve = FadeCurve::default();
                fade_out_curve = FadeCurve::default();
                cmds.push(AppCommand::UpdateClipAudio {
                    clip_id,
                    volume,
                    pan,
                    fade_in_secs: fade_in,
                    fade_out_secs: fade_out,
                    fade_in_curve,
                    fade_out_curve,
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
            fade_in_curve,
            fade_out_curve,
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
            fade_in_curve,
            fade_out_curve,
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
            fade_in_curve,
            fade_out_curve,
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

    // ── Fade In Curve ──
    {
        let prev = fade_in_curve;
        egui::ComboBox::from_id_salt("fade_in_curve")
            .selected_text(fade_curve_label(fade_in_curve))
            .width(100.0)
            .show_ui(ui, |ui| {
                for variant in FADE_CURVE_VARIANTS {
                    ui.selectable_value(&mut fade_in_curve, *variant, fade_curve_label(*variant));
                }
            });
        if fade_in_curve != prev {
            cmds.push(AppCommand::UpdateClipAudio {
                clip_id,
                volume,
                pan,
                fade_in_secs: fade_in,
                fade_out_secs: fade_out,
                fade_in_curve,
                fade_out_curve,
            });
        }
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
            fade_in_curve,
            fade_out_curve,
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

    // ── Fade Out Curve ──
    {
        let prev = fade_out_curve;
        egui::ComboBox::from_id_salt("fade_out_curve")
            .selected_text(fade_curve_label(fade_out_curve))
            .width(100.0)
            .show_ui(ui, |ui| {
                for variant in FADE_CURVE_VARIANTS {
                    ui.selectable_value(&mut fade_out_curve, *variant, fade_curve_label(*variant));
                }
            });
        if fade_out_curve != prev {
            cmds.push(AppCommand::UpdateClipAudio {
                clip_id,
                volume,
                pan,
                fade_in_secs: fade_in,
                fade_out_secs: fade_out,
                fade_in_curve,
                fade_out_curve,
            });
        }
    }

    ui.add_space(14.0);

    // ── Level Meter ──
    ui.label(
        egui::RichText::new("LEVEL")
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(4.0);
    draw_stereo_meter(ui, theme, peak.0, peak.1);
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
            // Slice peaks to the clip's source range
            let clip_peaks = waveform_peaks.map(|peaks| {
                if peaks.is_empty() { return &peaks[..]; }
                let total_dur = peaks.len() as f64; // total peaks count
                let src_in = clip.source_in.to_secs_f64();
                let src_out = clip.source_out.to_secs_f64();
                let clip_dur = (src_out - src_in).max(0.0);
                // Compute the media's total duration from the peaks
                // (peaks are generated at some fixed rate, we use the ratio)
                let media_dur = clip.source_out.to_secs_f64().max(clip_dur + src_in);
                if media_dur <= 0.0 { return &peaks[..]; }
                let start = ((src_in / media_dur) * total_dur) as usize;
                let end = (((src_in + clip_dur) / media_dur) * total_dur) as usize;
                let start = start.min(peaks.len());
                let end = end.min(peaks.len());
                &peaks[start..end]
            });
            waveform::draw_role_waveform(ui.painter(), rect, theme.role_dialogue, clip_peaks);
        });
}

const FADE_CURVE_VARIANTS: &[FadeCurve] = &[
    FadeCurve::Linear,
    FadeCurve::Exponential,
    FadeCurve::Logarithmic,
    FadeCurve::SCurve,
];

fn fade_curve_label(curve: FadeCurve) -> &'static str {
    match curve {
        FadeCurve::Linear => "Linear",
        FadeCurve::Exponential => "Exponential",
        FadeCurve::Logarithmic => "Logarithmic",
        FadeCurve::SCurve => "S-Curve",
    }
}

/// Persistent state for the level meter (smoothed values + peak hold).
#[derive(Clone, Default)]
struct MeterState {
    smooth_l: f32,
    smooth_r: f32,
    peak_hold_l: f32,
    peak_hold_r: f32,
    peak_hold_age_l: f32, // seconds since peak was set
    peak_hold_age_r: f32,
}

/// Draw a stereo level meter with L/R bars, dB scale, and peak hold indicators.
fn draw_stereo_meter(ui: &mut egui::Ui, theme: &KlippTheme, peak_l: f32, peak_r: f32) {
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let meter_id = ui.id().with("audio_meter_state");

    // Load persistent meter state
    let mut ms: MeterState = ui.ctx().data_mut(|d| d.get_temp(meter_id).unwrap_or_default());

    // Smooth: fast attack (rise instantly), slow release (decay ~300ms to half)
    let release_coeff = (-dt / 0.15).exp(); // ~150ms time constant
    ms.smooth_l = if peak_l > ms.smooth_l { peak_l } else { ms.smooth_l * release_coeff };
    ms.smooth_r = if peak_r > ms.smooth_r { peak_r } else { ms.smooth_r * release_coeff };

    // Peak hold: capture highest peak, hold for 2 seconds, then decay slowly
    let hold_time = 2.0; // seconds to hold before decay
    let hold_decay = 0.3; // decay rate after hold expires (units/sec)

    if peak_l >= ms.peak_hold_l {
        ms.peak_hold_l = peak_l;
        ms.peak_hold_age_l = 0.0;
    } else {
        ms.peak_hold_age_l += dt;
        if ms.peak_hold_age_l > hold_time {
            ms.peak_hold_l = (ms.peak_hold_l - hold_decay * dt).max(0.0);
        }
    }
    if peak_r >= ms.peak_hold_r {
        ms.peak_hold_r = peak_r;
        ms.peak_hold_age_r = 0.0;
    } else {
        ms.peak_hold_age_r += dt;
        if ms.peak_hold_age_r > hold_time {
            ms.peak_hold_r = (ms.peak_hold_r - hold_decay * dt).max(0.0);
        }
    }

    // Store back
    ui.ctx().data_mut(|d| d.insert_temp(meter_id, ms.clone()));

    let meter_w = ui.available_width() - 20.0;
    let bar_h = 6.0;
    let gap = 2.0;
    let label_w = 14.0;
    let total_h = bar_h * 2.0 + gap;

    let (meter_rect, _) = ui.allocate_exact_size(egui::vec2(meter_w, total_h), egui::Sense::hover());
    let painter = ui.painter();

    let bar_w = meter_w - label_w - 4.0;
    let bar_left = meter_rect.left() + label_w + 4.0;
    let max_level = 1.5f32; // meter range in linear (covers ~+3.5dB)

    let smoothed = [ms.smooth_l, ms.smooth_r];
    let peak_holds = [ms.peak_hold_l, ms.peak_hold_r];

    for (ch, label) in ["L", "R"].iter().enumerate() {
        let y = meter_rect.top() + ch as f32 * (bar_h + gap);
        let level = smoothed[ch];
        let hold = peak_holds[ch];

        // Channel label
        painter.text(
            egui::pos2(meter_rect.left() + label_w / 2.0, y + bar_h / 2.0),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::monospace(9.0),
            theme.text_dim,
        );

        // Background bar
        let bar = egui::Rect::from_min_size(egui::pos2(bar_left, y), egui::vec2(bar_w, bar_h));
        painter.rect_filled(bar, 2.0, egui::Color32::from_rgb(20, 20, 25));

        // Fill bar with gradient colors
        let fill = (level.min(max_level) / max_level * bar_w).max(0.0);
        if fill > 0.0 {
            let green_end = bar_w * 0.25 / max_level * max_level;
            let yellow_end = bar_w * 0.7 / max_level * max_level;

            let green_w = fill.min(green_end);
            if green_w > 0.0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(bar_left, y), egui::vec2(green_w, bar_h)),
                    2.0, egui::Color32::from_rgb(40, 170, 60),
                );
            }
            if fill > green_end {
                let yellow_w = (fill - green_end).min(yellow_end - green_end);
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(bar_left + green_end, y), egui::vec2(yellow_w, bar_h)),
                    0.0, egui::Color32::from_rgb(210, 180, 40),
                );
            }
            if fill > yellow_end {
                let red_w = fill - yellow_end;
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(bar_left + yellow_end, y), egui::vec2(red_w, bar_h)),
                    0.0, egui::Color32::from_rgb(210, 45, 45),
                );
            }
        }

        // Peak hold indicator — thin bright line
        if hold > 0.005 {
            let hold_x = bar_left + (hold.min(max_level) / max_level * bar_w);
            let hold_color = if hold > 0.9 {
                egui::Color32::from_rgb(255, 60, 60)
            } else if hold > 0.7 {
                egui::Color32::from_rgb(255, 210, 50)
            } else {
                egui::Color32::from_rgb(80, 220, 100)
            };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(hold_x - 1.0, y), egui::vec2(2.0, bar_h)),
                0.0, hold_color,
            );
        }
    }

    // dB tick marks
    ui.add_space(2.0);
    let (tick_rect, _) = ui.allocate_exact_size(egui::vec2(meter_w, 10.0), egui::Sense::hover());
    let tick_left = tick_rect.left() + label_w + 4.0;
    let tick_w = meter_w - label_w - 4.0;

    let tick_painter = ui.painter();
    let db_marks: &[(f32, &str)] = &[
        (0.004, "-48"), (0.06, "-24"), (0.25, "-12"),
        (0.5, "-6"), (0.7, "-3"), (1.0, "0"),
    ];
    for &(level, label) in db_marks {
        let x = tick_left + (level / max_level) * tick_w;
        if x <= tick_left + tick_w {
            tick_painter.text(
                egui::pos2(x, tick_rect.center().y),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(7.0),
                theme.text_dim,
            );
        }
    }

    // Request repaint during playback so meter animates smoothly
    if ms.smooth_l > 0.001 || ms.smooth_r > 0.001 || ms.peak_hold_l > 0.005 || ms.peak_hold_r > 0.005 {
        ui.ctx().request_repaint();
    }
}
