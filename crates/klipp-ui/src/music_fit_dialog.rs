//! The Fit to Length dialog (#2210): a music clip fitted to a length by
//! whole bars, as `klipp_analysis::music_structure::fit` cuts or repeats
//! them. Plan reads the track off the UI thread
//! ([`AppCommand::PlanMusicFit`]); what it found is shown before Apply
//! replaces the clip with the fitted stretches, their joins crossfaded,
//! as one undo step ([`AppCommand::ApplyMusicFit`]).
use klipp_core::{AppCommand, ClipId};

/// What length the clip is fitted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitTarget {
    /// From the clip's start to the end of the sequence's picture.
    SequenceEnd,
    /// The sequence's marked in–out range.
    MarkedRange,
    /// A length typed in.
    Length,
}

/// A fit read off the track, waiting for Apply.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedFit {
    pub target_secs: f64,
    pub segments: Vec<(f64, f64)>,
    pub length_secs: f64,
    pub join_secs: Option<f64>,
    pub bars_changed: i64,
}

/// The open dialog.
#[derive(Debug, Clone)]
pub struct MusicFitDialog {
    pub clip_id: ClipId,
    pub target: FitTarget,
    pub length_secs: f64,
    pub skip_intro: bool,
    /// The lengths the first two targets mean, from the sequence.
    pub to_end_secs: f64,
    pub marked_secs: Option<f64>,
    pub planning: bool,
    pub planned: Option<PlannedFit>,
    pub refused: Option<String>,
}

impl MusicFitDialog {
    /// The dialog for `clip_id` in `seq`, its target the marked range when
    /// there is one, else the end of the picture.
    pub fn new(seq: &klipp_timeline::Sequence, clip_id: ClipId) -> Option<Self> {
        let clip = seq.all_clips().find(|c| c.id == clip_id)?;
        let start = clip.timeline_in.to_secs_f64();
        let end = seq
            .tracks
            .iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .map(|c| c.timeline_out.to_secs_f64())
            .fold(0.0f64, f64::max);
        let end = if end > start { end } else { seq.total_duration().to_secs_f64() };
        let marked_secs = match (seq.in_point, seq.out_point) {
            (Some(i), Some(o)) if o > i => Some(o.to_secs_f64() - i.to_secs_f64()),
            _ => None,
        };
        let to_end_secs = (end - start).max(0.0);
        let current = (clip.timeline_out - clip.timeline_in).to_secs_f64();
        Some(Self {
            clip_id,
            target: if marked_secs.is_some() { FitTarget::MarkedRange } else { FitTarget::SequenceEnd },
            length_secs: current,
            skip_intro: false,
            to_end_secs,
            marked_secs,
            planning: false,
            planned: None,
            refused: None,
        })
    }

    /// The length the chosen target asks for, when it has one.
    pub fn target_secs(&self) -> Option<f64> {
        let secs = match self.target {
            FitTarget::SequenceEnd => self.to_end_secs,
            FitTarget::MarkedRange => self.marked_secs?,
            FitTarget::Length => self.length_secs,
        };
        (secs.is_finite() && secs > 0.0).then_some(secs)
    }
}

/// A plan's description: how long, what changed, where it joins.
pub fn describe(p: &PlannedFit) -> String {
    let how = match p.bars_changed {
        0 => "already that long".to_string(),
        n if n < 0 => format!("{} bar{} taken out", -n, if n == -1 { "" } else { "s" }),
        n => format!("{n} bar{} repeated", if n == 1 { "" } else { "s" }),
    };
    let join = p.join_secs.map(|j| format!(", joined at {j:.2} s")).unwrap_or_default();
    format!("{:.2} s for {:.2} s asked: {how}{join}.", p.length_secs, p.target_secs)
}

/// Draw the open dialog, if any; the commands it sends.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let Some(mut dialog) = state.music_fit_dialog.take() else { return Vec::new() };
    let mut open = true;
    let (mut plan, mut apply, mut cancel) = (false, false, false);
    egui::Window::new("Fit Music to Length")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(340.0);
            ui.label(egui::RichText::new("Cuts or repeats whole bars between the first and last phrase, so the track still starts and ends as it does. The media is not changed.").small().weak());
            ui.add_space(4.0);
            let before = (dialog.target, dialog.length_secs, dialog.skip_intro);
            ui.radio_value(&mut dialog.target, FitTarget::SequenceEnd, format!("To the end of the picture ({:.2} s)", dialog.to_end_secs));
            ui.add_enabled_ui(dialog.marked_secs.is_some(), |ui| {
                let text = dialog.marked_secs.map_or("To the marked range (none marked)".to_string(), |m| format!("To the marked range ({m:.2} s)"));
                ui.radio_value(&mut dialog.target, FitTarget::MarkedRange, text);
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut dialog.target, FitTarget::Length, "To a length");
                ui.add_enabled(dialog.target == FitTarget::Length, egui::DragValue::new(&mut dialog.length_secs).range(1.0..=3600.0).speed(0.1).fixed_decimals(2).suffix(" s"));
            });
            ui.checkbox(&mut dialog.skip_intro, "Start on the music, leaving out a quiet intro");
            if (dialog.target, dialog.length_secs, dialog.skip_intro) != before {
                // A changed ask is a new plan.
                dialog.planned = None;
                dialog.refused = None;
            }
            ui.add_space(6.0);
            if dialog.planning {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the track's bars\u{2026}");
                });
            } else if let Some(p) = &dialog.planned {
                ui.label(describe(p));
                ui.label(egui::RichText::new(format!("Plays {}.", p.segments.iter().map(|(a, b)| format!("{a:.2}–{b:.2} s")).collect::<Vec<_>>().join(", then "))).small().weak());
            } else if let Some(why) = &dialog.refused {
                ui.colored_label(ui.visuals().warn_fg_color, why);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                cancel = ui.button("Cancel").clicked();
                let can_plan = !dialog.planning && dialog.target_secs().is_some();
                if dialog.planned.is_some() {
                    apply = ui.button("Apply").clicked();
                } else {
                    plan = ui.add_enabled(can_plan, egui::Button::new("Plan")).clicked();
                }
            });
        });
    let mut out = Vec::new();
    if plan {
        if let Some(target_secs) = dialog.target_secs() {
            dialog.planning = true;
            dialog.refused = None;
            out.push(AppCommand::PlanMusicFit { clip_id: dialog.clip_id, target_secs, skip_intro: dialog.skip_intro });
        }
    }
    if apply {
        if let Some(p) = dialog.planned.take() {
            return vec![AppCommand::ApplyMusicFit { clip_id: dialog.clip_id, segments: p.segments }];
        }
    }
    if open && !cancel {
        state.music_fit_dialog = Some(dialog);
    }
    out
}

/// A plan or refusal for the open dialog, when it is still the one asked
/// for: the same clip and the same length.
pub fn take_result(state: &mut crate::state::AppState, clip_id: ClipId, result: Result<PlannedFit, String>) {
    let Some(dialog) = state.music_fit_dialog.as_mut().filter(|d| d.clip_id == clip_id) else { return };
    let asked = dialog.target_secs();
    dialog.planning = false;
    match result {
        Ok(p) if asked.is_some_and(|t| (t - p.target_secs).abs() < 1e-6) => dialog.planned = Some(p),
        Ok(_) => {}
        Err(why) => dialog.refused = Some(why),
    }
}

/// The edit that replaces `clip_id` with `segments` of its media, placed
/// back to back from where it starts on its track, joins crossfaded.
pub fn fitted_edit(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, clip_id: ClipId, segments: &[(f64, f64)]) -> Result<Vec<klipp_document::history::EditCommand>, String> {
    let (ti, ci) = seq.find_clip(clip_id).ok_or("the clip is no longer in the sequence")?;
    let clip = &seq.tracks[ti].clips[ci];
    let media_id = clip.media_id().ok_or("the clip has no media to fit")?;
    let at = clip.timeline_in.to_secs_f64();
    let ops = klipp_document::ops::fitted_music_ops(media_id, klipp_document::ops::TrackRef::Index(ti), at, segments, Some(clip_id));
    let plan = klipp_document::ops::resolve(seq, pool, &ops).map_err(|r| r.why)?;
    Ok(plan.steps.into_iter().map(|s| s.edit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(secs: f64) -> klipp_core::RationalTime {
        klipp_core::RationalTime::from_secs_f64(secs, 24)
    }

    #[test]
    fn the_target_is_the_marked_range_when_there_is_one() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut pool = klipp_media::MediaPool::new();
        let media = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/fit-music.wav".into(), None));
        let music = klipp_timeline::Clip::media(media, tc(2.0), tc(40.0), tc(0.0), tc(38.0));
        let id = music.id;
        let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[audio].clips.push(music);
        let video = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        seq.tracks[video].clips.push(klipp_timeline::Clip::media(media, tc(0.0), tc(30.0), tc(0.0), tc(30.0)));
        let d = MusicFitDialog::new(&seq, id).unwrap();
        assert_eq!(d.target, FitTarget::SequenceEnd);
        assert!((d.target_secs().unwrap() - 28.0).abs() < 1e-6, "from the clip's start to the picture's end");
        seq.in_point = Some(tc(4.0));
        seq.out_point = Some(tc(24.0));
        let d = MusicFitDialog::new(&seq, id).unwrap();
        assert_eq!(d.target, FitTarget::MarkedRange);
        assert!((d.target_secs().unwrap() - 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_fit_replaces_the_clip_with_its_stretches_and_crossfades_the_join() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/fit-music.wav".into(), None);
        item.duration = tc(60.0);
        item.has_audio = true;
        let media = pool.add(item);
        let music = klipp_timeline::Clip::media(media, tc(2.0), tc(62.0), tc(0.0), tc(60.0));
        let id = music.id;
        let audio = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[audio].clips.push(music);
        let edits = fitted_edit(&seq, &pool, id, &[(0.0, 20.0), (36.0, 60.0)]).expect("a planned fit");
        let mut seqs = vec![seq];
        let mut fps = klipp_core::Fps::new(24, 1);
        let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&klipp_document::history::EditCommand::Batch(edits));
        let clips: Vec<_> = seqs[0].tracks[audio].clips.iter().collect();
        assert!(clips.iter().all(|c| c.id != id), "the clip is replaced");
        assert_eq!(clips.len(), 2, "one clip a stretch");
        assert!((clips[0].timeline_in.to_secs_f64() - 2.0).abs() < 0.05, "from where the clip started");
        let length: f64 = clips.iter().map(|c| (c.timeline_out - c.timeline_in).to_secs_f64()).sum();
        assert!((length - 44.0).abs() < 0.1, "the stretches back to back: {length}");
        assert!(!seqs[0].tracks[audio].transitions.is_empty(), "the join is crossfaded");
    }
}
