//! Source edits beyond insert and overwrite (#919): fit to fill, place on
//! top and replace, and the three-point placement every source edit
//! shares. Each is one undo step, recorded by `tracks_undo`.

use super::*;
use crate::source_edit::{Marks, Placement};
use klipp_core::RationalTime;

impl KlippApp {
    /// The source and sequence marks a source edit of `item` reads.
    fn source_marks(&self, item: &klipp_media::MediaItem) -> Marks {
        source_marks(&self.state, item)
    }

    fn placement_timecodes(&self, item: &klipp_media::MediaItem, p: &Placement) -> (Timecode, Timecode, Timecode, Timecode) {
        placement_timecodes(&self.state, item, p)
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
        if !self.state.sequences[seq_idx].tracks.iter().any(|t| t.kind == kind) {
            self.state.toast.show(if audio_only { "No audio track in sequence" } else { "No video track in sequence" });
            return;
        }
        self.adopt_first_clip_format(&item);
        let (t_in, t_out, s_in, s_out) = self.placement_timecodes(&item, &p);
        let Some(target) = self.source_edit_track(seq_idx, kind, t_in, t_out) else { return };
        if self.state.sequences[seq_idx].tracks[target].locked {
            self.state.toast.show("Target track is locked");
            return;
        }
        let mut clip = klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out);
        clip.speed = p.speed;
        super::media::starting_role_on(&mut clip, &item);
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
        let id = place_on_top(&mut self.state, mid, &item);
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
        self.finish_source_edit_in_place(clip_id);
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
        super::media::starting_role_on(&mut new, &item);
        if !audio_only {
            new.source_rotation = item.source_rotation;
        }
        let id = new.id;
        let sound = (item.has_audio && !audio_only).then(|| klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out));
        match crate::source_edit::ripple_replace(&mut self.state.sequences[seq_idx], target, old, new, sound) {
            Ok(()) => self.finish_source_edit_in_place(id),
            Err(why) => self.state.toast.show(format!("Ripple Overwrite unchanged: {why}")),
        }
    }

    /// The track a source edit of `kind` lays its clip on from `from` to
    /// `to`: the patched one, else the first of the kind for a picture; a
    /// sound-only source goes where `Sequence::sound_source_track` says,
    /// or on a new audio track when every one is taken, so it never writes
    /// over sound it was not sent to (#2940). `None` with no track of the
    /// kind.
    pub(super) fn source_edit_track(&mut self, seq_idx: usize, kind: klipp_timeline::TrackKind, from: Timecode, to: Timecode) -> Option<usize> {
        let seq = &mut self.state.sequences[seq_idx];
        if kind != klipp_timeline::TrackKind::Audio {
            return seq.source_track(kind);
        }
        Some(seq.sound_source_track_or_new(from, to))
    }

    fn finish_source_edit(&mut self, clip_id: ClipId) {
        advance_past(&mut self.state, clip_id);
        self.finish_source_edit_in_place(clip_id);
    }

    /// [`Self::finish_source_edit`] with the playhead left where it is:
    /// an edit of the clip already under it.
    fn finish_source_edit_in_place(&mut self, clip_id: ClipId) {
        self.state.selected_clip = Some(clip_id);
        self.state.dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
    }
}

/// The source and sequence marks a source edit of `item` reads.
fn source_marks(state: &AppState, item: &klipp_media::MediaItem) -> Marks {
    let seq = state.sequence();
    let secs = |t: Option<Timecode>| t.map(|t| t.to_secs_f64());
    Marks {
        source_in: secs(item.default_source_in),
        source_out: secs(item.default_source_out),
        media_len: item.duration.to_secs_f64(),
        seq_in: secs(seq.in_point),
        seq_out: secs(seq.out_point),
        playhead: state.playhead.to_secs_f64(),
    }
}

/// `p` as timecode: the timeline in at the sequence's rate, the source
/// range at the media's, the marks themselves where they are used
/// unchanged so no rounding creeps in.
fn placement_timecodes(state: &AppState, item: &klipp_media::MediaItem, p: &Placement) -> (Timecode, Timecode, Timecode, Timecode) {
    let rate = state.sequence().fps.rate();
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
    let seq = state.sequence();
    let timeline_marks = [seq.in_point, seq.out_point, Some(state.playhead)];
    let source_marks = [item.default_source_in, item.default_source_out, zero, Some(item.duration)];
    (
        exact(p.timeline_in, &timeline_marks, rate),
        exact(p.timeline_out, &timeline_marks, rate),
        exact(p.source_in, &source_marks, src_rate),
        exact(p.source_out, &source_marks, src_rate),
    )
}

/// The source's picture laid on top (#919), by the three-point rules, on
/// the track [`place_on_top_track`] picks or a new one on top; the
/// playhead moved to its end (#2982). The new clip.
fn place_on_top(state: &mut AppState, mid: MediaId, item: &klipp_media::MediaItem) -> ClipId {
    let p = crate::source_edit::three_point(&source_marks(state, item));
    let (t_in, t_out, s_in, s_out) = placement_timecodes(state, item, &p);
    let seq_idx = state.active_sequence_idx;
    let seq = &state.sequences[seq_idx];
    let dest = match place_on_top_track(seq, t_in, t_out) {
        Some(i) => i,
        None => {
            let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
            state.sequences[seq_idx].add_video_track(format!("Video {}", n + 1));
            0
        }
    };
    let mut clip = klipp_timeline::Clip::media(mid, t_in, t_out, s_in, s_out);
    clip.source_rotation = item.source_rotation;
    let id = clip.id;
    KlippApp::overwrite_insert_clip_on_track(&mut state.sequences[seq_idx].tracks[dest], clip);
    advance_past(state, id);
    id
}

/// The playhead moved to the end of `clip`, a clip a source edit just
/// placed (#2982): the next edit at the playhead goes after it, so shots
/// laid one by one follow each other rather than stacking at one time.
pub(super) fn advance_past(state: &mut AppState, clip: ClipId) {
    let out = state.sequence().all_clips().find(|c| c.id == clip).map(|c| c.timeline_out);
    if let Some(out) = out {
        state.playhead = out;
    }
}

/// Where Place on Top lays a picture over `t_in..t_out` (#2951): the
/// nearest video track above the base that is free over that span, or
/// `None` for a new track on top. The base is the patched video track, or
/// with none patched the lowest, V1: the first in the list, the top, was
/// taken for it, so each shot laid went above the last one's new track and
/// made another.
pub(super) fn place_on_top_track(seq: &klipp_timeline::Sequence, t_in: klipp_core::Timecode, t_out: klipp_core::Timecode) -> Option<usize> {
    use klipp_timeline::TrackKind;
    let base = seq
        .tracks
        .iter()
        .position(|t| t.kind == TrackKind::Video && t.source_patch)
        .or_else(|| seq.tracks.iter().rposition(|t| t.kind == TrackKind::Video))
        .unwrap_or(seq.tracks.len());
    let free = |t: &klipp_timeline::Track| t.kind == TrackKind::Video && !t.locked && !t.clips.iter().any(|c| !c.kind.is_gap() && c.timeline_in < t_out && t_in < c.timeline_out);
    (0..base).rev().find(|&i| free(&seq.tracks[i]))
}

#[cfg(test)]
mod place_on_top_tests {
    use super::place_on_top_track;
    use klipp_timeline::{Clip, Sequence, TrackKind};

    fn tc(s: f64) -> klipp_core::Timecode {
        klipp_core::Timecode::from_secs_f64(s, 24)
    }

    /// Lays a shot over `a..b` as Place on Top does: on the track it picks,
    /// or on a new one on top.
    fn place(seq: &mut Sequence, a: f64, b: f64) -> String {
        let dest = match place_on_top_track(seq, tc(a), tc(b)) {
            Some(i) => i,
            None => {
                let n = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count();
                seq.add_video_track(format!("Video {}", n + 1));
                0
            }
        };
        let clip = Clip::media(klipp_core::MediaId::new(), tc(a), tc(b), tc(0.0), tc(b - a));
        super::KlippApp::overwrite_insert_clip_on_track(&mut seq.tracks[dest], clip);
        seq.tracks[dest].name.clone()
    }

    /// #2951: three shots apart from each other over a V1 clip all land on
    /// V2; a fourth over one of them goes to V3; nothing patched or patched.
    #[test]
    fn b_roll_reuses_a_free_track_and_stacks_only_when_it_must() {
        let mut seq = Sequence::new_default();
        let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        seq.tracks[v1].clips.push(Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(20.0), tc(0.0), tc(20.0)));
        assert_eq!(place(&mut seq, 1.0, 3.0), "Video 2");
        assert_eq!(place(&mut seq, 5.0, 7.0), "Video 2", "the free track above is used again");
        assert_eq!(place(&mut seq, 9.0, 11.0), "Video 2");
        assert_eq!(place(&mut seq, 10.0, 12.0), "Video 3", "over a shot on V2: a track above it");
        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count(), 3);
        // A gap on V2 again: the next shot fills it, not V3.
        assert_eq!(place(&mut seq, 14.0, 16.0), "Video 2");
        // With V1 patched, the same: above V1.
        let mut seq2 = Sequence::new_default();
        let v1 = seq2.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        seq2.patch_source_to(v1);
        seq2.tracks[v1].clips.push(Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(20.0), tc(0.0), tc(20.0)));
        for (a, b) in [(1.0, 3.0), (5.0, 7.0), (9.0, 11.0)] {
            assert_eq!(place(&mut seq2, a, b), "Video 2");
        }
    }
}

#[cfg(test)]
mod advance_tests {
    use super::*;

    /// #2982: Place on Top twice in a row, from the playhead: the second
    /// shot starts where the first ends, on the same track, and the
    /// playhead ends at its end. It stayed put, and the second shot went
    /// on yet another new track at the same time.
    #[test]
    fn place_on_top_twice_lays_the_shots_one_after_the_other() {
        let mut state = AppState::default();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/place-on-top.mov".into(), None);
        item.duration = Timecode::from_secs_f64(3.0, 24);
        item.video_codec = "h264".into();
        let mid = state.media_pool.add(item.clone());
        state.playhead = Timecode::from_secs_f64(1.0, 24);
        let first = place_on_top(&mut state, mid, &item);
        let second = place_on_top(&mut state, mid, &item);
        let seq = state.sequence();
        let at = |id: ClipId| {
            let ti = seq.tracks.iter().position(|t| t.find_clip(id).is_some()).expect("placed");
            let c = seq.tracks[ti].find_clip(id).unwrap();
            (ti, c.timeline_in, c.timeline_out)
        };
        let (t1, _, out1) = at(first);
        let (t2, in2, out2) = at(second);
        assert_eq!(in2, out1, "the second starts where the first ends");
        assert_eq!(t2, t1, "on the same track");
        assert_eq!(state.playhead, out2, "the playhead at the second's end");
    }
}
