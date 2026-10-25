//! Source edits beyond insert and overwrite (#919): fit to fill, place on
//! top and replace, and the three-point placement every source edit
//! shares. Each is one undo step, recorded by `tracks_undo`.

use super::*;
use crate::source_edit::{Marks, Placement};
use klipp_core::RationalTime;

impl KlippApp {
    /// The source and sequence marks a source edit of `item` reads.
    fn source_marks(&self, item: &klipp_media::MediaItem) -> Marks {
        let seq = self.state.sequence();
        let secs = |t: Option<Timecode>| t.map(|t| t.to_secs_f64());
        Marks {
            source_in: secs(item.default_source_in),
            source_out: secs(item.default_source_out),
            media_len: item.duration.to_secs_f64(),
            seq_in: secs(seq.in_point),
            seq_out: secs(seq.out_point),
            playhead: self.state.playhead.to_secs_f64(),
        }
    }

    /// `p` as timecode: the timeline in at the sequence's rate, the source
    /// range at the media's, the marks themselves where they are used
    /// unchanged so no rounding creeps in.
    fn placement_timecodes(&self, item: &klipp_media::MediaItem, p: &Placement) -> (Timecode, Timecode, Timecode, Timecode) {
        let rate = self.state.sequence().fps.rate();
        let src_rate = item.duration.rate;
        let exact = |secs: f64, marks: &[Option<Timecode>], rate: u32| {
            marks
                .iter()
                .flatten()
                .copied()
                .find(|t| (t.to_secs_f64() - secs).abs() < 1e-9)
                .unwrap_or_else(|| RationalTime::from_secs_f64(secs, rate))
        };
        let zero = Some(Timecode { value: 0, rate: src_rate });
        let seq = self.state.sequence();
        let timeline_marks = [seq.in_point, seq.out_point, Some(self.state.playhead)];
        let source_marks = [item.default_source_in, item.default_source_out, zero, Some(item.duration)];
        (
            exact(p.timeline_in, &timeline_marks, rate),
            exact(p.timeline_out, &timeline_marks, rate),
            exact(p.source_in, &source_marks, src_rate),
            exact(p.source_out, &source_marks, src_rate),
        )
    }

    /// Where an insert or overwrite of `item` lands, by the three-point
    /// rules: (timeline in, source in, source out).
    pub(super) fn source_placement(&self, item: &klipp_media::MediaItem) -> (Timecode, Timecode, Timecode) {
        let p = crate::source_edit::three_point(&self.source_marks(item));
        let (timeline_in, _, source_in, source_out) = self.placement_timecodes(item, &p);
        (timeline_in, source_in, source_out)
    }

    /// The loaded source, when it can be edited in: its id and item.
    fn editable_source(&mut self) -> Option<(MediaId, klipp_media::MediaItem)> {
        let Some(mid) = self.state.source_media_id else {
            self.state.toast.show("Load a clip in the source viewer first");
            return None;
        };
        let Some(item) = self.state.media_pool.get(&mid).cloned() else {
            self.state.toast.show("Source clip is no longer in the media pool");
            return None;
        };
        if item.offline {
            self.state.toast.show(format!("Cannot edit '{}' — media is offline", item.filename));
            return None;
        }
        Some((mid, item))
    }

    /// The source-edit commands of #919: `false` for any other.
    pub(super) fn handle_source_edit_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::FitToFillEdit => self.fit_to_fill_edit(),
            AppCommand::PlaceOnTopEdit => self.place_on_top_edit(),
            AppCommand::ReplaceEdit => self.replace_edit(),
            AppCommand::RippleOverwriteEdit => self.ripple_overwrite_edit(),
            _ => return false,
        }
        true
    }

    /// The source In to Out over the sequence In to Out on the patched
    /// track, at the speed that fills it, its sound under it at the same
    /// speed.
    fn fit_to_fill_edit(&mut self) {
        let Some((mid, item)) = self.editable_source() else { return };
        let Some(p) = crate::source_edit::fit_to_fill(&self.source_marks(&item)) else {
            self.state.toast.show("Fit to Fill needs a sequence In and Out");
            return;
        };
        if !(0.1..=16.0).contains(&p.speed) {
            self.state.toast.show(format!("Fit to Fill would play the source at {:.0}%; the most is 1600%, the least 10%", p.speed * 100.0));
            return;
        }
        let audio_only = item.video_codec.is_empty();
        let kind = if audio_only { klipp_timeline::TrackKind::Audio } else { klipp_timeline::TrackKind::Video };
        let seq_idx = self.state.active_sequence_idx;
        let Some(target) = self.state.sequences[seq_idx].source_track(kind) else {
            self.state.toast.show(if audio_only { "No audio track in sequence" } else { "No video track in sequence" });
            return;
        };
        if self.state.sequences[seq_idx].tracks[target].locked {
            self.state.toast.show("Target track is locked");
            return;
        }
        self.adopt_first_clip_format(&item);
        let (t_in, t_out, s_in, s_out) = self.placement_timecodes(&item, &p);
        let mut clip = klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out);
        clip.speed = p.speed;
        if !audio_only {
            clip.source_rotation = item.source_rotation;
        }
        let id = clip.id;
        Self::overwrite_insert_clip_on_track(&mut self.state.sequences[seq_idx].tracks[target], clip);
        if item.has_audio && !audio_only {
            let external = self.external_audio_of(mid);
            if let Some(a) = attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], id, external) {
                if let Some(c) = self.state.sequences[seq_idx].tracks[a.track_index].find_clip_mut(a.clip.id) {
                    c.speed = p.speed;
                }
            }
        }
        self.state.toast.show(format!("Fit to Fill: {:.0}% speed", p.speed * 100.0));
        self.finish_source_edit(id);
    }

    /// The source's picture on the nearest video track above the patched
    /// one that is free over its span, or on a new track on top. Its sound
    /// is left out: what is under it keeps its own.
    fn place_on_top_edit(&mut self) {
        let Some((mid, item)) = self.editable_source() else { return };
        if item.video_codec.is_empty() {
            self.state.toast.show("Place on Top lays a picture; this source is sound only");
            return;
        }
        self.adopt_first_clip_format(&item);
        let p = crate::source_edit::three_point(&self.source_marks(&item));
        let (t_in, t_out, s_in, s_out) = self.placement_timecodes(&item, &p);
        let seq_idx = self.state.active_sequence_idx;
        let seq = &self.state.sequences[seq_idx];
        let base = seq.source_track(klipp_timeline::TrackKind::Video).unwrap_or(seq.tracks.len());
        let free = |t: &klipp_timeline::Track| {
            t.kind == klipp_timeline::TrackKind::Video
                && !t.locked
                && !t.clips.iter().any(|c| !c.kind.is_gap() && c.timeline_in < t_out && t_in < c.timeline_out)
        };
        let dest = match (0..base).rev().find(|&i| free(&seq.tracks[i])) {
            Some(i) => i,
            None => {
                let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
                self.state.sequences[seq_idx].add_video_track(format!("Video {}", n + 1));
                0
            }
        };
        let mut clip = klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out);
        clip.source_rotation = item.source_rotation;
        let id = clip.id;
        Self::overwrite_insert_clip_on_track(&mut self.state.sequences[seq_idx].tracks[dest], clip);
        self.finish_source_edit(id);
    }

    /// The clip under the playhead on the patched track plays the source
    /// instead, the frame at the source's playhead at the timeline's; its
    /// place, length, grade and effects stay. Its linked sound follows
    /// when the source has sound.
    fn replace_edit(&mut self) {
        let Some((mid, item)) = self.editable_source() else { return };
        let audio_only = item.video_codec.is_empty();
        let kind = if audio_only { klipp_timeline::TrackKind::Audio } else { klipp_timeline::TrackKind::Video };
        let seq_idx = self.state.active_sequence_idx;
        let playhead = self.state.playhead;
        let seq = &self.state.sequences[seq_idx];
        let Some(target) = seq.source_track(kind) else {
            self.state.toast.show("No track of the source's kind to replace on");
            return;
        };
        if seq.tracks[target].locked {
            self.state.toast.show("Target track is locked");
            return;
        }
        let Some((_, clip)) = seq.tracks[target].clip_at_timecode(playhead) else {
            self.state.toast.show("Replace: no clip under the playhead on the target track");
            return;
        };
        if !matches!(clip.kind, klipp_timeline::ClipKind::Media { .. }) {
            self.state.toast.show("Replace: only a media clip can take another source");
            return;
        }
        let (clip_id, partner) = (clip.id, clip.linked_clip_id);
        let speed = clip.speed.abs().max(0.1);
        let before = (playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()) * speed;
        let len = (clip.timeline_out.to_secs_f64() - clip.timeline_in.to_secs_f64()) * speed;
        let s_in = self.state.source_playhead.to_secs_f64() - before;
        let s_out = s_in + len;
        let media_len = item.duration.to_secs_f64();
        if s_in < -1e-6 || s_out > media_len + 1e-6 {
            let short = if s_in < 0.0 { format!("{:.2} s too little before its playhead", -s_in) } else { format!("{:.2} s too little after its playhead", s_out - media_len) };
            self.state.toast.show(format!("Replace: the source has {short} to cover the clip"));
            return;
        }
        let rate = item.duration.rate;
        let (s_in, s_out) = (RationalTime::from_secs_f64(s_in.max(0.0), rate), RationalTime::from_secs_f64(s_out.min(media_len), rate));
        let seq = &mut self.state.sequences[seq_idx];
        let retarget = |c: &mut klipp_timeline::Clip| {
            c.kind = klipp_timeline::ClipKind::Media { media_id: mid };
            c.source_in = s_in;
            c.source_out = s_out;
        };
        if let Some(c) = seq.tracks[target].find_clip_mut(clip_id) {
            retarget(c);
            if !audio_only {
                c.source_rotation = item.source_rotation;
            }
        }
        if let Some(p) = partner.filter(|_| item.has_audio && !audio_only) {
            if let Some(c) = seq.all_clips_mut().find(|c| c.id == p && matches!(c.kind, klipp_timeline::ClipKind::Media { .. })) {
                retarget(c);
            }
        }
        self.finish_source_edit(clip_id);
    }

    /// The clip under the playhead on the patched track replaced by the
    /// source's marked range from where it starts, its linked sound by the
    /// source's sound; what follows moves by the change in length.
    fn ripple_overwrite_edit(&mut self) {
        let Some((mid, item)) = self.editable_source() else { return };
        let audio_only = item.video_codec.is_empty();
        let kind = if audio_only { klipp_timeline::TrackKind::Audio } else { klipp_timeline::TrackKind::Video };
        let seq_idx = self.state.active_sequence_idx;
        let playhead = self.state.playhead;
        let seq = &self.state.sequences[seq_idx];
        let Some(target) = seq.source_track(kind).filter(|&t| !seq.tracks[t].locked) else {
            self.state.toast.show("Ripple Overwrite: no unlocked track of the source's kind");
            return;
        };
        let Some(old) = seq.tracks[target].clip_at_timecode(playhead).map(|(_, c)| c.id) else {
            self.state.toast.show("Ripple Overwrite: no clip under the playhead on the target track");
            return;
        };
        // The source's marks alone say how long it is; where it goes is the
        // old clip's start.
        let p = crate::source_edit::three_point(&Marks { seq_in: None, seq_out: None, ..self.source_marks(&item) });
        let (t_in, t_out, s_in, s_out) = self.placement_timecodes(&item, &p);
        let mut new = klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out);
        if !audio_only {
            new.source_rotation = item.source_rotation;
        }
        let id = new.id;
        let sound = (item.has_audio && !audio_only).then(|| klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out));
        match crate::source_edit::ripple_replace(&mut self.state.sequences[seq_idx], target, old, new, sound) {
            Ok(()) => self.finish_source_edit(id),
            Err(why) => self.state.toast.show(format!("Ripple Overwrite unchanged: {why}")),
        }
    }

    fn finish_source_edit(&mut self, clip_id: ClipId) {
        self.state.selected_clip = Some(clip_id);
        self.state.dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
    }
}
