//! The Normalize dialog (#1843): bring the selected clips to a loudness
//! (LUFS) or peak (dBFS) target, each on its own or the selection as one
//! group, under a true-peak ceiling. Its last values are the
//! preferences' (`AudioPrefs::normalize`); Normalize sends
//! [`AppCommand::NormalizeClips`], which measures off the UI thread.

use klipp_core::normalize::{NormalizeMode, NormalizeSettings, LOUDNESS_PRESETS};
use klipp_core::{AppCommand, ClipId};

/// The peak target a switch to peak mode starts from.
const DEFAULT_PEAK_DBFS: f64 = -1.0;

/// The open Normalize dialog.
#[derive(Debug, Clone)]
pub struct NormalizeDialog {
    pub settings: NormalizeSettings,
    pub clip_ids: Vec<ClipId>,
    /// The loudness targets with a name, the standards then the person's
    /// own, read once as the dialog opens (#2887).
    pub targets: Vec<(String, f64)>,
}

impl NormalizeDialog {
    /// The dialog for `clip_ids`, starting from the last settings used.
    pub fn new(clip_ids: Vec<ClipId>, last: NormalizeSettings) -> Self {
        let mut settings = last;
        if settings.check().is_err() {
            settings = NormalizeSettings::default();
        }
        Self { settings, clip_ids, targets: klipp_core::normalize::loudness_targets() }
    }

    /// The loudness preset the target is, if it is one.
    pub fn preset(&self) -> Option<usize> {
        self.targets.iter().position(|(_, v)| (*v - self.settings.target).abs() < 1e-6)
    }

    /// Switch the mode; the target moves to that mode's default when it
    /// would make no sense in the new one.
    pub fn set_mode(&mut self, mode: NormalizeMode) {
        if self.settings.mode == mode {
            return;
        }
        self.settings.mode = mode;
        self.settings.target = match mode {
            NormalizeMode::Loudness => LOUDNESS_PRESETS[0].1,
            NormalizeMode::Peak => DEFAULT_PEAK_DBFS,
        };
    }
}

/// The selected clips, the primary one first.
pub fn selected(state: &crate::state::AppState) -> Vec<ClipId> {
    let mut ids: Vec<ClipId> = state.selected_clip.into_iter().collect();
    for id in &state.selected_clips {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    ids
}

/// Set each clip's gain, and its linked partner's, as one undo step: an
/// audio edit for every clip that changes, partners too, since undoing one
/// puts back only the clip it names. Whether anything changed.
pub fn apply_gains(state: &mut crate::state::AppState, gains: &[(ClipId, f32)]) -> bool {
    let mut edits = Vec::new();
    let seq_idx = state.active_sequence_idx;
    for &(clip_id, gain) in gains {
        let partner = state.sequences[seq_idx].all_clips().find(|c| c.id == clip_id).and_then(|c| c.linked_clip_id);
        for id in std::iter::once(clip_id).chain(partner) {
            let Some(clip) = state.sequences[seq_idx].all_clips_mut().find(|c| c.id == id) else { continue };
            if clip.gain == gain {
                continue;
            }
            let (old_gain, volume, pan, fade_in, fade_out, channels) = (clip.gain, clip.volume, clip.pan, clip.fade_in_secs, clip.fade_out_secs, clip.channel_config);
            clip.gain = gain;
            edits.push(klipp_document::history::EditCommand::EditClipAudio {
                clip_id: id,
                old_gain,
                old_volume: volume,
                old_pan: pan,
                old_fade_in: fade_in,
                old_fade_out: fade_out,
                old_channel_config: channels,
                new_gain: gain,
                new_volume: volume,
                new_pan: pan,
                new_fade_in: fade_in,
                new_fade_out: fade_out,
                new_channel_config: channels,
            });
        }
    }
    match edits.len() {
        0 => false,
        1 => {
            state.push_history(edits.remove(0));
            true
        }
        _ => {
            state.push_history(klipp_document::history::EditCommand::Batch(edits));
            true
        }
    }
}

/// Draw the open dialog, if any; the commands its Normalize sends.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let Some(mut dialog) = state.normalize_dialog.take() else { return Vec::new() };
    let mut open = true;
    let (mut apply, mut cancel) = (false, false);
    egui::Window::new("Normalize Audio")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            let n = dialog.clip_ids.len();
            ui.label(egui::RichText::new(format!("{n} clip{} selected. Sets each clip's gain; the media is not changed.", if n == 1 { "" } else { "s" })).small().weak());
            ui.add_space(4.0);
            egui::Grid::new("normalize_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Mode");
                ui.horizontal(|ui| {
                    let mut mode = dialog.settings.mode;
                    ui.radio_value(&mut mode, NormalizeMode::Loudness, "Loudness (LUFS)");
                    ui.radio_value(&mut mode, NormalizeMode::Peak, "Peak (dBFS)");
                    dialog.set_mode(mode);
                });
                ui.end_row();

                ui.label("Target");
                ui.horizontal(|ui| match dialog.settings.mode {
                    NormalizeMode::Loudness => {
                        let shown = dialog.preset().map_or("Custom".to_string(), |i| format!("{} ({} LUFS)", dialog.targets[i].0, dialog.targets[i].1));
                        egui::ComboBox::from_id_salt("normalize_preset").selected_text(shown).show_ui(ui, |ui| {
                            for i in 0..dialog.targets.len() {
                                let (name, value) = dialog.targets[i].clone();
                                if ui.selectable_label(dialog.preset() == Some(i), format!("{name} ({value} LUFS)")).clicked() {
                                    dialog.settings.target = value;
                                }
                            }
                        });
                        ui.add(egui::DragValue::new(&mut dialog.settings.target).range(-70.0..=0.0).speed(0.1).fixed_decimals(1).suffix(" LUFS"));
                    }
                    NormalizeMode::Peak => {
                        ui.add(egui::DragValue::new(&mut dialog.settings.target).range(-60.0..=0.0).speed(0.1).fixed_decimals(1).suffix(" dBFS"));
                    }
                });
                ui.end_row();

                ui.label("True-peak ceiling");
                ui.add(egui::DragValue::new(&mut dialog.settings.ceiling_dbtp).range(-20.0..=0.0).speed(0.1).fixed_decimals(1).suffix(" dBTP"));
                ui.end_row();

                ui.label("Clips");
                ui.vertical(|ui| {
                    ui.radio_value(&mut dialog.settings.group, false, "Each on its own");
                    ui.add_enabled_ui(n > 1, |ui| {
                        ui.radio_value(&mut dialog.settings.group, true, "As one group, keeping their balance");
                    });
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                apply = ui.add_enabled(n > 0, egui::Button::new("Normalize")).clicked();
            });
        });
    if apply {
        let settings = NormalizeSettings { group: dialog.settings.group && dialog.clip_ids.len() > 1, ..dialog.settings };
        return vec![AppCommand::NormalizeClips { clip_ids: dialog.clip_ids, settings }];
    }
    if open && !cancel {
        state.normalize_dialog = Some(dialog);
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    fn frame(ctx: &egui::Context, state: &mut AppState, events: Vec<egui::Event>) -> (egui::FullOutput, Vec<AppCommand>) {
        let mut commands = Vec::new();
        let output = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), events, ..Default::default() },
            |ctx| commands.extend(show_dialog(ctx, state)),
        );
        (output, commands)
    }

    /// Where the text `label` is drawn.
    fn at(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos + t.galley.size() / 2.0),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{label:?} is drawn"))
    }

    fn click(ctx: &egui::Context, state: &mut AppState, output: &egui::FullOutput, label: &str) -> Vec<AppCommand> {
        let pos = at(output, label);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(ctx, state, vec![egui::Event::PointerMoved(pos), button(true)]);
        frame(ctx, state, vec![button(false)]).1
    }

    /// The dialog opens on the last settings, peak mode and the group can
    /// be chosen, and Normalize sends what was chosen for the clips.
    #[test]
    fn the_dialog_sends_what_was_chosen() {
        let clips = vec![ClipId::new(), ClipId::new()];
        let last = NormalizeSettings { mode: NormalizeMode::Loudness, target: -23.0, ceiling_dbtp: -2.0, group: false };
        let mut state = AppState { normalize_dialog: Some(NormalizeDialog::new(clips.clone(), last)), ..Default::default() };
        let ctx = egui::Context::default();
        // A window is measured on its first frame and drawn from the next.
        frame(&ctx, &mut state, Vec::new());
        let (out, _) = frame(&ctx, &mut state, Vec::new());
        // The preset shows as the last target.
        at(&out, "EBU R128 broadcast (-23 LUFS)");
        click(&ctx, &mut state, &out, "As one group, keeping their balance");
        let (out, _) = frame(&ctx, &mut state, Vec::new());
        click(&ctx, &mut state, &out, "Peak (dBFS)");
        let (out, _) = frame(&ctx, &mut state, Vec::new());
        let sent = click(&ctx, &mut state, &out, "Normalize");
        match sent.as_slice() {
            [AppCommand::NormalizeClips { clip_ids, settings }] => {
                assert_eq!(clip_ids, &clips);
                assert_eq!(*settings, NormalizeSettings { mode: NormalizeMode::Peak, target: DEFAULT_PEAK_DBFS, ceiling_dbtp: -2.0, group: true });
            }
            other => panic!("Normalize sent {other:?}"),
        }
        assert!(state.normalize_dialog.is_none(), "the dialog closes");
    }

    /// One clip can't be a group, whatever was last used.
    #[test]
    fn one_clip_is_normalized_on_its_own() {
        let last = NormalizeSettings { group: true, ..Default::default() };
        let mut state = AppState { normalize_dialog: Some(NormalizeDialog::new(vec![ClipId::new()], last)), ..Default::default() };
        let ctx = egui::Context::default();
        // A window is measured on its first frame and drawn from the next.
        frame(&ctx, &mut state, Vec::new());
        let (out, _) = frame(&ctx, &mut state, Vec::new());
        let sent = click(&ctx, &mut state, &out, "Normalize");
        assert!(matches!(sent.as_slice(), [AppCommand::NormalizeClips { settings, .. }] if !settings.group), "{sent:?}");
    }

    /// Two clips, one with a linked partner, take their gains as one undo
    /// step, the partner its clip's; one undo puts all three back.
    #[test]
    fn gains_are_one_undo_step_with_the_partners() {
        let mut state = AppState::default();
        let t = |f| klipp_core::RationalTime { value: f, rate: 24 };
        let media = state.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/normalize.wav".into(), None));
        let (mut a, b) = (klipp_timeline::Clip::media(media, t(0), t(48), t(0), t(48)), klipp_timeline::Clip::media(media, t(48), t(96), t(0), t(48)));
        let mut partner = klipp_timeline::Clip::media(media, t(0), t(48), t(0), t(48));
        a.linked_clip_id = Some(partner.id);
        partner.linked_clip_id = Some(a.id);
        let (ia, ib, ip) = (a.id, b.id, partner.id);
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let a1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        state.sequences[0].tracks[v1].clips.extend([a, b]);
        state.sequences[0].tracks[a1].clips.push(partner);
        let before = state.active_history().undo_count();
        assert!(apply_gains(&mut state, &[(ia, 2.0), (ib, 0.5)]));
        let gain = |s: &AppState, id| s.sequences[0].all_clips().find(|c| c.id == id).unwrap().gain;
        assert_eq!((gain(&state, ia), gain(&state, ib), gain(&state, ip)), (2.0, 0.5, 2.0));
        assert_eq!(state.active_history().undo_count(), before + 1, "one undo step");
        let entry = state.active_history_mut().undo().unwrap();
        assert_eq!(entry.command.label(), "Edit Audio");
        let (mut fps, idx) = (state.fps, state.active_sequence_idx);
        klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: idx, media_pool: &mut state.media_pool, fps: &mut fps }.apply_inverse(&entry.command);
        assert_eq!((gain(&state, ia), gain(&state, ib), gain(&state, ip)), (1.0, 1.0, 1.0), "undone, the partner too");
    }

    /// Settings that no longer make sense open as the defaults.
    #[test]
    fn bad_last_settings_open_as_the_defaults() {
        let d = NormalizeDialog::new(vec![], NormalizeSettings { target: 12.0, ..Default::default() });
        assert_eq!(d.settings, NormalizeSettings::default());
        assert_eq!(d.preset(), Some(0));
    }
}
