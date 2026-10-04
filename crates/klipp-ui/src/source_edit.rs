//! Where a source edit lands (#919): three-point editing from the source
//! In and Out and the sequence In and Out, as editors expect it, and fit
//! to fill for the four-point case.
//!
//! Three of the four points decide the edit; a missing one follows:
//! - the sequence In, or the playhead without one, is where the clip
//!   starts;
//! - a sequence Out alone places the clip by its end (backtiming);
//! - with both sequence points, their span is the length, taken from the
//!   source In, or back from a source Out set without an In.
//!
//! Times are seconds here; the caller turns them into timecode at the
//! source's and the sequence's rates.

/// A source edit's placement: the timeline span and the source range it
/// plays, at `speed` (1 except for fit to fill).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub timeline_in: f64,
    pub timeline_out: f64,
    pub source_in: f64,
    pub source_out: f64,
    pub speed: f64,
}

/// The marks a source edit reads, in seconds: the source's own (`None`
/// when unset), the media's length, the sequence's, and the playhead.
#[derive(Debug, Clone, Copy)]
pub struct Marks {
    pub source_in: Option<f64>,
    pub source_out: Option<f64>,
    pub media_len: f64,
    pub seq_in: Option<f64>,
    pub seq_out: Option<f64>,
    pub playhead: f64,
}

impl Marks {
    /// The source range the marks name; the whole file when they name
    /// nothing, or nothing usable.
    fn source_range(&self) -> (f64, f64) {
        let s_in = self.source_in.unwrap_or(0.0).clamp(0.0, self.media_len);
        let s_out = self.source_out.unwrap_or(self.media_len).clamp(0.0, self.media_len);
        if s_out > s_in { (s_in, s_out) } else { (0.0, self.media_len) }
    }

    /// The sequence span both sequence marks make, when they make one.
    fn seq_span(&self) -> Option<(f64, f64)> {
        match (self.seq_in, self.seq_out) {
            (Some(i), Some(o)) if o > i => Some((i, o)),
            _ => None,
        }
    }
}

/// Three-point placement at speed 1.
pub fn three_point(m: &Marks) -> Placement {
    let (s_in, s_out) = m.source_range();
    let at_speed = |t_in: f64, s_in: f64, s_out: f64| Placement { timeline_in: t_in, timeline_out: t_in + (s_out - s_in), source_in: s_in, source_out: s_out, speed: 1.0 };
    if let Some((t_in, t_out)) = m.seq_span() {
        let len = t_out - t_in;
        // A source Out set without an In: the span is taken back from it.
        if m.source_out.is_some() && m.source_in.is_none() {
            let s_in = (s_out - len).max(0.0);
            return at_speed(t_out - (s_out - s_in), s_in, s_out);
        }
        let s_out = (s_in + len).min(m.media_len);
        return at_speed(t_in, s_in, s_out);
    }
    match (m.seq_in, m.seq_out) {
        // Backtiming: the clip ends at the sequence Out.
        (None, Some(t_out)) => {
            let len = s_out - s_in;
            // Before the sequence's start, the head is what gives way.
            let start = t_out - len;
            if start < 0.0 {
                at_speed(0.0, s_in - start, s_out)
            } else {
                at_speed(start, s_in, s_out)
            }
        }
        (Some(t_in), _) => at_speed(t_in, s_in, s_out),
        (None, None) => at_speed(m.playhead, s_in, s_out),
    }
}

/// Fit to fill: the source range into the sequence In to Out, its speed
/// changed so it fills that span exactly. `None` without both sequence
/// marks.
pub fn fit_to_fill(m: &Marks) -> Option<Placement> {
    let (t_in, t_out) = m.seq_span()?;
    let (s_in, s_out) = m.source_range();
    Some(Placement { timeline_in: t_in, timeline_out: t_out, source_in: s_in, source_out: s_out, speed: (s_out - s_in) / (t_out - t_in) })
}

/// Ripple overwrite: the clip `clip_id` on track `track` replaced by
/// `new`, laid from where the old one starts, and its linked sound by
/// `new_sound` on the sound's track when both are given. What follows the
/// old clip, on its track, its sound's track and every sync-locked
/// unlocked track, moves by the change in length. `Err` with the reason,
/// leaving `seq` as it was, when the move would run a clip into one that
/// stays.
pub fn ripple_replace(seq: &mut klipp_timeline::Sequence, track: usize, clip_id: klipp_core::ClipId, mut new: klipp_timeline::Clip, new_sound: Option<klipp_timeline::Clip>) -> Result<(), String> {
    use klipp_core::RationalTime;
    let old = seq.tracks.get(track).and_then(|t| t.find_clip(clip_id)).cloned().ok_or("the clip is gone")?;
    let partner = old.linked_clip_id.and_then(|p| seq.find_clip(p).map(|(ti, _)| (ti, p)));
    let point = old.timeline_out;
    let rate = old.timeline_in.rate;
    let start = old.timeline_in.to_secs_f64();
    let delta = (new.timeline_out.to_secs_f64() - new.timeline_in.to_secs_f64()) - (point.to_secs_f64() - start);
    let mut affected = vec![track];
    affected.extend(partner.map(|(ti, _)| ti));
    affected.extend(seq.tracks.iter().enumerate().filter(|(_, t)| t.sync_lock && !t.locked).map(|(i, _)| i));
    affected.sort_unstable();
    affected.dedup();
    let gone = [Some(clip_id), partner.map(|(_, p)| p)];
    let mut out = seq.clone();
    for &ti in &affected {
        let t = &mut out.tracks[ti];
        t.clips.retain(|c| !gone.contains(&Some(c.id)));
        t.transitions.retain(|tr| !gone.contains(&Some(tr.clip_a_id)) && !gone.contains(&Some(tr.clip_b_id)));
        let stays_until = t.clips.iter().filter(|c| c.timeline_in < point).map(|c| c.timeline_out.to_secs_f64()).fold(f64::MIN, f64::max);
        for c in t.clips.iter_mut().filter(|c| c.timeline_in >= point) {
            let r = c.timeline_in.rate;
            c.timeline_in = RationalTime::from_secs_f64(c.timeline_in.to_secs_f64() + delta, r);
            c.timeline_out = RationalTime::from_secs_f64(c.timeline_out.to_secs_f64() + delta, r);
            if c.timeline_in.to_secs_f64() < stays_until - 1e-6 {
                return Err(format!("on \"{}\" a clip across the edit is in the way of what follows", t.name));
            }
        }
    }
    let new_out = RationalTime::from_secs_f64(start + new.timeline_out.to_secs_f64() - new.timeline_in.to_secs_f64(), rate);
    let place = |c: &mut klipp_timeline::Clip| {
        c.timeline_in = old.timeline_in;
        c.timeline_out = new_out;
    };
    place(&mut new);
    let mut lay = |ti: usize, c: klipp_timeline::Clip| {
        let t = &mut out.tracks[ti];
        t.clips.push(c);
        t.clips.sort_by(|a, b| a.timeline_in.to_secs_f64().total_cmp(&b.timeline_in.to_secs_f64()));
    };
    match (partner, new_sound) {
        (Some((pt, _)), Some(mut sound)) => {
            place(&mut sound);
            sound.linked_clip_id = Some(new.id);
            new.linked_clip_id = Some(sound.id);
            lay(track, new);
            lay(pt, sound);
        }
        _ => {
            new.linked_clip_id = None;
            lay(track, new);
        }
    }
    *seq = out;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks() -> Marks {
        Marks { source_in: Some(2.0), source_out: Some(6.0), media_len: 10.0, seq_in: None, seq_out: None, playhead: 20.0 }
    }

    #[test]
    fn with_no_sequence_marks_the_clip_starts_at_the_playhead() {
        let p = three_point(&marks());
        assert_eq!((p.timeline_in, p.timeline_out, p.source_in, p.source_out), (20.0, 24.0, 2.0, 6.0));
        let whole = three_point(&Marks { source_in: None, source_out: None, ..marks() });
        assert_eq!((whole.source_in, whole.source_out), (0.0, 10.0), "no source marks: the whole file");
    }

    #[test]
    fn a_sequence_out_alone_places_the_clip_by_its_end() {
        let p = three_point(&Marks { seq_out: Some(30.0), ..marks() });
        assert_eq!((p.timeline_in, p.timeline_out), (26.0, 30.0));
        let early = three_point(&Marks { seq_out: Some(1.0), ..marks() });
        assert_eq!((early.timeline_in, early.timeline_out, early.source_in), (0.0, 1.0, 5.0), "the head gives way before zero");
    }

    #[test]
    fn both_sequence_marks_set_the_length() {
        let p = three_point(&Marks { seq_in: Some(10.0), seq_out: Some(12.0), ..marks() });
        assert_eq!((p.timeline_in, p.timeline_out, p.source_in, p.source_out), (10.0, 12.0, 2.0, 4.0));
        let back = three_point(&Marks { source_in: None, seq_in: Some(10.0), seq_out: Some(12.0), ..marks() });
        assert_eq!((back.source_in, back.source_out, back.timeline_in), (4.0, 6.0, 10.0), "from a source Out, backward");
        let short = three_point(&Marks { source_in: Some(9.0), source_out: None, seq_in: Some(10.0), seq_out: Some(15.0), ..marks() });
        assert_eq!((short.timeline_out, short.source_out), (11.0, 10.0), "no more media than there is");
    }

    #[test]
    fn a_ripple_overwrite_moves_what_follows_by_the_change_in_length() {
        use klipp_core::{MediaId, RationalTime};
        use klipp_timeline::{Clip, Sequence, Track, TrackKind};
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let m = MediaId::new();
        let mut seq = Sequence::new_default();
        let (mut a, b) = (Clip::media(m, tc(0.0), tc(4.0), tc(0.0), tc(4.0)), Clip::media(m, tc(4.0), tc(6.0), tc(4.0), tc(6.0)));
        let (mut sa, sb) = (Clip::media(m, tc(0.0), tc(4.0), tc(0.0), tc(4.0)), Clip::media(m, tc(4.0), tc(6.0), tc(4.0), tc(6.0)));
        a.linked_clip_id = Some(sa.id);
        sa.linked_clip_id = Some(a.id);
        let (a_id, b_id, sb_id) = (a.id, b.id, sb.id);
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = vec![a, b];
        let mut s1 = Track::new("A1", TrackKind::Audio);
        s1.clips = vec![sa, sb];
        let mut t = Track::new("T1", TrackKind::Text);
        t.sync_lock = true;
        t.clips = vec![Clip::media(m, tc(5.0), tc(6.0), tc(0.0), tc(1.0))];
        let free = Track::new("V2", TrackKind::Video);
        seq.tracks = vec![t, free, v, s1];
        let n = MediaId::new();
        let (new, sound) = (Clip::media(n, tc(0.0), tc(2.4), tc(1.0), tc(3.4)), Clip::media(n, tc(0.0), tc(2.4), tc(1.0), tc(3.4)));
        ripple_replace(&mut seq, 2, a_id, new, Some(sound)).unwrap();
        let at = |id| seq.all_clips().find(|c| c.id == id).map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()));
        assert_eq!(at(b_id), Some((2.4, 4.4)), "the next clip closed up by 1.6 s");
        assert_eq!(at(sb_id), Some((2.4, 4.4)), "and the next sound");
        assert_eq!(seq.tracks[0].clips[0].timeline_in.to_secs_f64(), 3.4, "the sync-locked title came along");
        let v1 = &seq.tracks[2].clips[0];
        assert_eq!((v1.timeline_in.to_secs_f64(), v1.timeline_out.to_secs_f64()), (0.0, 2.4));
        assert!(v1.linked_clip_id.is_some_and(|s| seq.tracks[3].clips[0].id == s), "picture and new sound are linked");
        assert!(seq.all_clips().all(|c| c.id != a_id), "the old clip is gone");
    }

    #[test]
    fn fit_to_fill_changes_the_speed_to_fill_the_span() {
        let p = fit_to_fill(&Marks { seq_in: Some(10.0), seq_out: Some(18.0), ..marks() }).unwrap();
        assert_eq!((p.timeline_in, p.timeline_out, p.source_in, p.source_out), (10.0, 18.0, 2.0, 6.0));
        assert!((p.speed - 0.5).abs() < 1e-9);
        assert!(fit_to_fill(&marks()).is_none());
    }
}
