//! Multicam: an angle switched, the group's sound chosen, the angles synced
//! and the synced plan applied (#890).

use super::*;

impl KlippApp {
    /// Cut the multicam group in play at `at` to `angle` (counted from
    /// 1): the keys, the angle bar, the grid and the control socket all
    /// come here.
    pub(super) fn switch_multicam_angle(&mut self, angle: usize, at: klipp_core::Timecode) {
        // Angles are counted from 1 here, as the buttons and keys 1-9
        // count them; the cut list from 0, the primary. Stored as sent,
        // "Angle 1" cut to the second camera.
        let Some(wanted) = angle.checked_sub(1).and_then(|a| u8::try_from(a).ok()) else { return };
        let seq_idx = self.state.active_sequence_idx;
        let Some((track_idx, primary_id, angles)) = self.state.sequences[seq_idx]
            .multicam_at(None, at)
            .map(|m| (m.primary_track, m.primary.id, m.angles))
        else {
            return;
        };
        if !angles.contains(&wanted) {
            self.state.toast.show(format!("This multicam group has no angle {angle}"));
            return;
        }
        let Some(clip) = self.state.sequences[seq_idx].tracks[track_idx].find_clip_mut(primary_id) else { return };
        let old = clip.clone();
        let clip_time = (at - clip.timeline_in).to_secs_f64();
        clip.multicam_cuts.retain(|(t, _)| (*t - clip_time).abs() > 0.001);
        clip.multicam_cuts.push((clip_time, wanted));
        clip.multicam_cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let new = clip.clone();
        // One undo step per cut: cutting live means mis-hits,
        // and a mis-hit could not be taken back.
        self.state.push_history(EditCommand::ReplaceClip { description: "Switch Angle", old: Box::new(old), new: Box::new(new) });
        self.state.toast.show(format!("Cut to angle {angle}"));
        self.state.dirty = true;
        // The viewer shows the cut only once the engine has
        // the sequence and redraws the frame; without this
        // the toast and the angle bar said one camera and the
        // picture another (paused, a sync alone redraws
        // nothing).
        self.sync_and_refresh_frame();
    }

    /// Choose whose sound the multicam group at `at` plays, on each of
    /// its primaries (a bladed group has several), as one undo step.
    pub(super) fn set_multicam_sound(&mut self, sound: klipp_core::MulticamSound, at: klipp_core::Timecode) {
        let seq_idx = self.state.active_sequence_idx;
        let Some((group, angles)) = self.state.sequences[seq_idx].multicam_at(None, at).map(|m| (m.group, m.angles)) else {
            self.state.toast.show("There is no multicam group here");
            return;
        };
        if let klipp_core::MulticamSound::Angle(a) = sound {
            if !angles.contains(&a) {
                self.state.toast.show(format!("This multicam group has no angle {}", a as usize + 1));
                return;
            }
        }
        let mut steps = Vec::new();
        for clip in self.state.sequences[seq_idx].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if clip.multicam_group == Some(group) && clip.multicam_angle == 0 && clip.multicam_sound != sound {
                let old = clip.clone();
                clip.multicam_sound = sound;
                steps.push(EditCommand::ReplaceClip { description: "Multicam Sound", old: Box::new(old), new: Box::new(clip.clone()) });
            }
        }
        let command = match steps.len() {
            0 => return,
            1 => steps.remove(0),
            _ => EditCommand::Batch(steps),
        };
        self.state.push_history(command);
        self.state.toast.show(format!("Multicam sound: {}", sound.label()));
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Sync `media_ids` onto one clock, off the UI thread when sound is
    /// involved; the plan comes back as `AppEvent::MulticamSyncReady`.
    pub(super) fn spawn_multicam_sync(&mut self, media_ids: Vec<klipp_core::MediaId>, method: klipp_core::MulticamSync) {
        let mut items: Vec<klipp_media::MediaItem> = media_ids
            .iter()
            .filter_map(|id| self.state.media_pool.get(id))
            // A sound-only recording joins as the others' reference and
            // goes on an audio track; it is not an angle.
            .filter(|m| !m.offline && !m.is_still && (!m.video_codec.is_empty() || m.has_audio))
            .cloned()
            .collect();
        if items.iter().filter(|m| !m.video_codec.is_empty()).count() < 2 {
            self.state.toast.show("A multicam needs two or more online video clips");
            return;
        }
        // Angle numbers follow the names, which is how cameras label cards.
        items.sort_by(|a, b| a.filename.cmp(&b.filename));
        let angles: Vec<klipp_core::MulticamAngle> = items
            .iter()
            .map(|m| klipp_core::MulticamAngle {
                media_id: m.id,
                name: m.filename.clone(),
                start_timecode: m.shot.start_timecode.clone(),
                fps: m.fps,
                duration_secs: m.duration.to_secs_f64(),
                has_audio: m.has_audio,
            })
            .collect();
        let paths: Vec<std::path::PathBuf> = items.iter().map(|m| m.path.clone()).collect();
        let sequence_id = self.state.sequences[self.state.active_sequence_idx].id;
        let at_secs = self.state.playhead.to_secs_f64();
        let tx = self.proxy_event_tx.clone();
        if method != klipp_core::MulticamSync::Timecode {
            self.state.toast.show(format!("Syncing {} angles\u{2026}", angles.len()));
        }
        let spawned = std::thread::Builder::new().name("klipp-multicam-sync".into()).spawn(move || {
            use klipp_analysis::audio_sync::{coarse_offset, decode_mono_pcm, refine_offset, SoundPrint};
            use klipp_core::AudioMatch;
            const SR: u32 = 8_000;
            let ffmpeg = std::path::Path::new("ffmpeg");
            // Each file's sound is decoded once and kept only as its
            // print; the refinement decodes two seconds of each again.
            // Twelve one-hour angles would otherwise hold 1.4 GB.
            let mut prints: std::collections::HashMap<usize, Result<std::sync::Arc<SoundPrint>, String>> =
                std::collections::HashMap::new();
            let mut print = |i: usize| {
                prints
                    .entry(i)
                    .or_insert_with(|| decode_mono_pcm(ffmpeg, &paths[i], SR, 0.0, 0.0).map(|pcm| std::sync::Arc::new(SoundPrint::of(&pcm, SR))))
                    .clone()
            };
            let plan = klipp_core::plan_multicam(&angles, method, |r, i| {
                let (a, b) = match (print(r), print(i)) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return AudioMatch::Unavailable(e),
                };
                let Some(coarse) = coarse_offset(&a, &b) else { return AudioMatch::TooLittleOverlap };
                let (ref_from, clip_from, len) = coarse.refine_windows();
                let slack = klipp_analysis::audio_sync::REFINE_SLACK_SECS;
                let offset_secs = match (
                    decode_mono_pcm(ffmpeg, &paths[r], SR, ref_from.max(0.0), len),
                    decode_mono_pcm(ffmpeg, &paths[i], SR, clip_from.max(0.0), len + 2.0 * slack),
                ) {
                    (Ok(rw), Ok(cw)) if clip_from >= 0.0 => refine_offset(&rw, &cw, SR, &coarse),
                    _ => coarse.offset_secs,
                };
                AudioMatch::Matched { offset_secs, confidence: coarse.confidence }
            });
            let _ = tx.send(AppEvent::MulticamSyncReady { plan, sequence_id, at_secs });
        });
        if let Err(e) = spawned {
            self.state.toast.show(format!("Could not start the multicam sync: {e}"));
        }
    }

    /// Lay out a synced multicam: each angle on its own tracks at its
    /// place after `at_secs`, then grouped, as one undo step.
    pub(super) fn apply_multicam_plan(&mut self, plan: klipp_core::MulticamPlan, sequence_id: klipp_core::SequenceId, at_secs: f64) {
        let seq_idx = self.state.active_sequence_idx;
        if self.state.sequences[seq_idx].id != sequence_id {
            self.state.toast.show("The sequence changed while the angles were syncing, so the multicam was not placed");
            return;
        }
        if plan.synced() < 2 {
            let why = if plan.notes.is_empty() { String::new() } else { format!(": {}", plan.notes.join("; ")) };
            self.state.toast.show(format!("Could not put two angles on one clock{why}"));
            return;
        }
        // On a frame of the sequence: a sound match is finer than a frame,
        // and a clip between frames is a clip nobody can cut. The grid is
        // the one the sequence will have: an empty sequence takes its rate
        // from the first clip with picture placed in it, and that must
        // hold for that first clip's own position too.
        let fps = {
            let seq = &self.state.sequences[seq_idx];
            let has_video = seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Video && !t.clips.is_empty());
            let first_picture = plan
                .placements
                .iter()
                .filter_map(|p| self.state.media_pool.get(&p.media_id))
                .find(|m| !m.video_codec.is_empty() && m.resolution.width > 0 && m.resolution.height > 0);
            match first_picture {
                Some(m) if !has_video => m.fps,
                _ => seq.fps,
            }
        };
        let mut entries = Vec::new();
        let mut video_clips = Vec::new();
        for p in &plan.placements {
            let frames = ((at_secs + p.start_secs) * fps.as_f64()).round() as i64;
            let start = klipp_core::RationalTime { value: frames * i64::from(fps.den), rate: fps.num };
            if let Some((video, entry)) = self.place_media_on_new_tracks(p.media_id, start) {
                entries.push(entry);
                if let Some(v) = video {
                    video_clips.push((v, p.is_reference));
                }
            }
        }
        // The reference angle first: angle 0 carries the cut list, and
        // it is the sound the others were matched to.
        video_clips.sort_by_key(|(_, is_ref)| !is_ref);
        let group_id = klipp_core::GroupId::new();
        let seq = &mut self.state.sequences[seq_idx];
        for (angle, (clip_id, _)) in video_clips.iter().enumerate() {
            for track in &mut seq.tracks {
                if let Some(clip) = track.find_clip_mut(*clip_id) {
                    clip.multicam_group = Some(group_id);
                    clip.multicam_angle = angle as u8;
                    break;
                }
            }
        }
        // Snapshots taken now, grouped: redo puts back what they hold.
        for entry in &mut entries {
            self.refresh_placement_snapshot(seq_idx, entry);
        }
        if !entries.is_empty() {
            self.state.active_history_mut().push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::Batch(entries),
            });
        }
        let by = |m| plan.placements.iter().filter(|p| p.method == Some(m)).count();
        let (tc, snd) = (by(klipp_core::SyncMethod::Timecode), by(klipp_core::SyncMethod::Waveform));
        let mut summary = format!("Multicam of {} angles", video_clips.len());
        if tc > 0 {
            summary.push_str(&format!(", {tc} by timecode"));
        }
        if snd > 0 {
            summary.push_str(&format!(", {snd} by sound"));
        }
        for n in &plan.notes {
            summary.push_str(". ");
            summary.push_str(n);
        }
        tracing::info!("{summary}");
        self.state.toast.show(summary);
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }
}
