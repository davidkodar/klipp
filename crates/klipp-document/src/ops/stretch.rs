//! Rate stretch (#3055): a clip's edge moved so the same content (its
//! source In to Out) plays faster or slower to fill the new length.
//!
//! The clips stretched are the ones named and their linked sound or
//! picture. Clips that touch on a track stretch together, about the
//! run's fixed edge, so their relative timing is kept: every time `t` in
//! a run goes to `a + (t - a) * f`, `a` the run's fixed edge and `f` the
//! stretch, each edge then put on its track's grid. A clip that plays at
//! a speed takes `speed / f` and has everything timed from its start
//! scaled with it ([`Clip::scale_content_time`]); a title, still,
//! generator or freeze frame changes length only, as a trim does. Each
//! clip is replaced whole, so one undo puts it back exactly.

use super::*;
use klipp_timeline::{SPEED_MAX, SPEED_MIN};

/// What a stretch is to: the first clip's new length, its dragged edge's
/// new place, or its new speed (in either case the others stretched by
/// as much).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StretchTo {
    Length(f64),
    EdgeAt(f64),
    Speed(f64),
}

/// Whether `clip` plays at a speed a stretch changes: a file's clip with
/// a frame rate, not a still or a freeze frame.
pub fn plays_at_speed(clip: &Clip, pool: &MediaPool) -> bool {
    matches!(clip.kind, ClipKind::Media { .. }) && clip.hold.is_none() && clip.media_id().and_then(|m| pool.get(&m)).is_some_and(|m| !m.is_still)
}

/// The stretches `clips` can take with every speed (each key of a ramp)
/// kept within 10% to 1600%: `f` from the first to the second.
pub fn stretch_range<'a>(clips: impl IntoIterator<Item = &'a Clip>, pool: &MediaPool) -> (f64, f64) {
    let (mut lo, mut hi) = (0.0_f64, f64::INFINITY);
    for c in clips.into_iter().filter(|c| plays_at_speed(c, pool)) {
        let speeds: Vec<f64> = if c.speed_keyframes.is_empty() { vec![c.speed.abs()] } else { c.speed_keyframes.iter().map(|k| f64::from(k.value).abs()).collect() };
        for s in speeds.into_iter().filter(|s| *s > 0.0) {
            lo = lo.max(s / SPEED_MAX);
            hi = hi.min(s / SPEED_MIN);
        }
    }
    (lo, hi)
}

/// The plan for stretching `clip_ids` from `edge` to `to`: what it does,
/// as one undo step, and the clips it changed. `ripple` moves what
/// follows by the change (the stretch keeping its start, from either
/// edge), else a stretch that would run into another clip is refused.
/// `keep_pitch` is how the stretched sound plays.
#[allow(clippy::too_many_arguments)]
pub fn stretch_at(seq: &Sequence, pool: &MediaPool, clip_ids: &[ClipId], edge: Edge, to: StretchTo, ripple: bool, keep_pitch: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    stretch(seq, pool, 0, clip_ids, edge, to, ripple, keep_pitch)
}

fn refuse_as(i: usize, code: RefusalCode, why: impl Into<String>, hint: impl Into<String>) -> Refusal {
    Refusal { code: Some(code), ..refuse(i, why, hint) }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stretch(seq: &Sequence, pool: &MediaPool, i: usize, clip_ids: &[ClipId], edge: Edge, to: StretchTo, ripple: bool, keep_pitch: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let fps = seq.fps;
    let Some(&first) = clip_ids.first() else {
        return Err(refuse(i, "no clip to stretch", "name a clip on the timeline"));
    };
    // The clips named, then their linked partners, each once.
    let mut members: Vec<(usize, &Clip)> = Vec::new();
    for id in clip_ids {
        let Some((ti, _, c)) = find_clip(seq, *id) else {
            return Err(refuse(i, format!("no clip {}", id.0), "name a clip on the timeline"));
        };
        if c.kind.is_gap() {
            return Err(refuse(i, "a gap has nothing to stretch", "name a clip"));
        }
        if !members.iter().any(|(_, m)| m.id == c.id) {
            members.push((ti, c));
        }
    }
    // A multicam group stretches as one: its other angles go with it, as
    // a ripple trim takes them, or the group would play out of step.
    for k in 0..members.len() {
        for id in multicam_followers(seq, members[k].1) {
            if let Some((ti, _, c)) = find_clip(seq, id).filter(|(_, _, c)| !members.iter().any(|(_, m)| m.id == c.id)) {
                members.push((ti, c));
            }
        }
    }
    let named = members.len();
    for k in 0..named {
        let partner = members[k].1.linked_clip_id.and_then(|lid| find_clip(seq, lid));
        if let Some((pti, _, p)) = partner {
            if !members.iter().any(|(_, m)| m.id == p.id) {
                members.push((pti, p));
            }
        }
    }
    if let Some((ti, _)) = members.iter().find(|(ti, _)| seq.tracks[*ti].locked) {
        return Err(refuse_as(i, RefusalCode::TrackLocked, format!("track {ti} is locked"), "unlock it first"));
    }
    let (_, primary) = members[0];
    debug_assert_eq!(primary.id, first);

    // Runs: clips that touch on a track, the named ones and their partners.
    let half_frame = secs(one_frame(fps)) / 2.0;
    let mut by_track: std::collections::BTreeMap<usize, Vec<&Clip>> = std::collections::BTreeMap::new();
    for &(ti, c) in &members {
        by_track.entry(ti).or_default().push(c);
    }
    let mut runs: Vec<(usize, Vec<&Clip>)> = Vec::new();
    for (ti, mut clips) in by_track {
        clips.sort_by_key(|c| c.timeline_in);
        let mut run: Vec<&Clip> = Vec::new();
        for c in clips {
            if run.last().is_some_and(|l| (secs(c.timeline_in) - secs(l.timeline_out)).abs() > half_frame) {
                runs.push((ti, std::mem::take(&mut run)));
            }
            run.push(c);
        }
        runs.push((ti, run));
    }
    // A run's fixed edge: its start for a stretch from the end, or with
    // ripple, which keeps the start; its end for one from the start.
    let keeps_start = matches!(edge, Edge::Out) || ripple;
    let anchor = |run: &[&Clip]| -> f64 { if keeps_start { secs(run[0].timeline_in) } else { secs(run[run.len() - 1].timeline_out) } };
    let run_of = |id: ClipId| runs.iter().find(|(_, r)| r.iter().any(|c| c.id == id)).map(|(_, r)| r.as_slice()).unwrap_or(&[]);

    // The stretch, from the first clip.
    let len = secs(primary.duration());
    if len <= 0.0 {
        return Err(refuse(i, "the clip has no length", "name a clip with a length"));
    }
    let f = match to {
        StretchTo::Length(l) => l / len,
        StretchTo::Speed(s) => {
            if !primary.speed_keyframes.is_empty() {
                return Err(refuse(i, format!("{} has a speed ramp, so it has no one speed", clip_name(seq, pool, primary)), "give the length to stretch it to"));
            }
            if !plays_at_speed(primary, pool) {
                return Err(refuse(i, format!("{} has no speed to change", clip_name(seq, pool, primary)), "give a length instead"));
            }
            if s == 0.0 || !s.is_finite() || (s < 0.0) != primary.is_reversed() {
                return Err(refuse(i, format!("a speed of {s}"), "give a speed of the same sign as the clip's: a stretch keeps its direction"));
            }
            primary.speed.abs() / s.abs()
        }
        StretchTo::EdgeAt(t) => {
            let a = anchor(run_of(primary.id));
            match edge {
                // The end to `t`, the run's start held.
                Edge::Out => (t - a) / (secs(primary.timeline_out) - a),
                // The start to `t` with ripple: as a ripple trim of the
                // head, the clip keeps its start and is as much longer
                // or shorter as the head moved.
                Edge::In if ripple => (secs(primary.timeline_out) - t) / len,
                // The start to `t`, the run's end held.
                Edge::In => (a - t) / (a - secs(primary.timeline_in)),
            }
        }
    };
    if !f.is_finite() || f <= 0.0 {
        return Err(refuse(i, "the clip would have no length", "stretch it to a length after its start"));
    }
    let (lo, hi) = stretch_range(members.iter().map(|(_, c)| *c), pool);
    if f < lo || f > hi {
        let needs = if plays_at_speed(primary, pool) && primary.speed_keyframes.is_empty() { format!(": this length needs {:.0}%", primary.speed.abs() / f * 100.0) } else { String::new() };
        return Err(refuse_as(
            i,
            RefusalCode::SpeedOutOfRange,
            format!("out of the speed range ({:.0}%–{:.0}%){needs}", SPEED_MIN * 100.0, SPEED_MAX * 100.0),
            format!("stretch to between {:.2} and {:.2} s", len * lo, len * hi),
        ));
    }

    // Each clip mapped through its run's stretch, on its track's grid.
    let mut replaced: Vec<(usize, Clip, Clip)> = Vec::new();
    for (ti, run) in &runs {
        // A sound linked to a picture keeps to the picture's frames
        // (#3230): on the samples its edges fell between them.
        let linked = run.iter().any(|c| c.linked_clip_id.is_some());
        let grid = if linked { fps } else { placement_grid(&seq.tracks[*ti], fps) };
        let a = anchor(run);
        let map = |t: Timecode| snap(a + (secs(t) - a) * f, grid);
        for c in run {
            let (new_in, new_out) = (map(c.timeline_in), map(c.timeline_out));
            if new_out <= new_in {
                return Err(refuse(i, format!("{} would be shorter than a frame", clip_name(seq, pool, c)), "stretch less"));
            }
            let mut n = (*c).clone();
            n.timeline_in = new_in;
            n.timeline_out = new_out;
            let fi = secs(new_out - new_in) / secs(c.duration());
            if plays_at_speed(c, pool) {
                n.speed = c.speed / fi;
                n.scale_content_time(fi).map_err(|why| refuse(i, format!("{}: its keys could not be moved: {why}", clip_name(seq, pool, c)), "stretch it without its keys, or take them off first"))?;
                n.keep_pitch = keep_pitch;
            } else {
                // As a trim of a title or still: as long as its place.
                n.source_out = n.source_in + (new_out - new_in);
            }
            let l = secs(new_out - new_in) as f32;
            n.fade_in_secs = n.fade_in_secs.min(l);
            n.fade_out_secs = n.fade_out_secs.min(l - n.fade_in_secs);
            replaced.push((*ti, (*c).clone(), n));
        }
    }

    // Without ripple, a stretch takes the room of the gap clips it grows
    // into (the primary track's holes) and runs into nothing else.
    let ids: Vec<ClipId> = replaced.iter().map(|(_, o, _)| o.id).collect();
    let mut taken: Vec<EditCommand> = Vec::new();
    if !ripple {
        for (ti, _, n) in &replaced {
            let skip = |c: &Clip| ids.contains(&c.id) || taken.iter().any(|e| matches!(e, EditCommand::LegacyRemoveClip { clip, .. } if clip.id == c.id));
            match super::room_from_gaps(seq, *ti, n.timeline_in, n.timeline_out, &skip) {
                Ok(gaps) => taken.extend(gaps),
                Err(c) => {
                    return Err(refuse_as(
                        i,
                        RefusalCode::WouldOverlap,
                        format!("{} would run into {} at {:.2} s", clip_name(seq, pool, n), clip_name(seq, pool, c), secs(c.timeline_in)),
                        "stretch less, make room first, or stretch with ripple (Alt)",
                    ));
                }
            }
        }
    }
    // The gaps out of the way first, then the clips stretched over them.
    let mut edits: Vec<EditCommand> = taken;
    edits.extend(replaced.iter().map(|(_, o, n)| EditCommand::ReplaceClip { description: "Rate Stretch", old: Box::new(o.clone()), new: Box::new(n.clone()) }));
    let mut moved_ids = Vec::new();
    let mut said_ripple = String::new();
    if ripple {
        // What follows each run moves by the change in its end; then the
        // sync-locked tracks by the first run's.
        let ends: Vec<(usize, &Clip, Timecode)> = runs.iter().map(|(ti, run)| {
            let last = run[run.len() - 1];
            let new_out = replaced.iter().find(|(_, o, _)| o.id == last.id).map(|(_, _, n)| n.timeline_out).unwrap_or(last.timeline_out);
            (*ti, last, new_out)
        }).collect();
        let Rippled { moves, ids: m, later, delta, partners: _ } = ripple_after(seq, pool, fps, i, &ends, true, "stretch")?;
        let skip: Vec<usize> = runs.iter().map(|(ti, _)| *ti).collect();
        let shifts = sync_lock_shifts(seq, i, &skip, ends[0].1.timeline_out, delta)?;
        if later > 0 || !shifts.is_empty() {
            said_ripple = format!("; {later} later clip(s) moved {:+.2} s", secs(delta));
        }
        edits.extend(moves);
        edits.extend(shifts);
        moved_ids = m;
    }
    let p = &replaced.iter().find(|(_, o, _)| o.id == primary.id).expect("the first clip is stretched").2;
    let what = format!(
        "stretch {} to {:.2} s ({:.2}-{:.2} s){}{}{}",
        clip_name(seq, pool, primary),
        secs(p.duration()),
        secs(p.timeline_in),
        secs(p.timeline_out),
        if plays_at_speed(primary, pool) && primary.speed_keyframes.is_empty() { format!(", now {:.1}%", p.speed.abs() * 100.0) } else if plays_at_speed(primary, pool) { ", its ramp scaled".into() } else { ", its length only".into() },
        if replaced.len() > 1 { format!(", {} other clip(s) with it", replaced.len() - 1) } else { String::new() },
        said_ripple,
    );
    let ids: Vec<ClipId> = ids.into_iter().chain(moved_ids).collect();
    let edit = if edits.len() == 1 { edits.remove(0) } else { EditCommand::Batch(edits) };
    Ok((what, edit, ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, Keyframe};
    use klipp_timeline::Track;

    fn tc(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 25)
    }

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// A 10 s file; on V1 a 4 s clip of its first 4 s and one after it
    /// from 6 s, on A1 the first clip's linked sound.
    fn fixture() -> (Sequence, MediaPool, ClipId, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/stretch.mov".into(), None);
        item.duration = tc(10.0);
        item.has_audio = true;
        let media = pool.add(item);
        let mut v = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let next = Clip::media(media, tc(6.0), tc(9.0), tc(5.0), tc(8.0));
        let mut a = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        v.linked_clip_id = Some(a.id);
        a.linked_clip_id = Some(v.id);
        let ids = (v.id, a.id, next.id);
        let mut vt = Track::new("V1", TrackKind::Video);
        vt.clips = vec![v, next];
        let mut at = Track::new("A1", TrackKind::Audio);
        at.clips = vec![a];
        seq.tracks = vec![vt, at];
        (seq, pool, ids.0, ids.1, ids.2)
    }

    fn applied(seq: &Sequence, pool: &MediaPool, edit: &EditCommand) -> Sequence {
        let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
        crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(edit);
        seqs.remove(0)
    }

    fn undone(seq: &Sequence, pool: &MediaPool, edit: &EditCommand) -> Sequence {
        let (mut seqs, mut pool, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
        crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(edit);
        seqs.remove(0)
    }

    fn lane_times(c: &Clip, lane: &str) -> Vec<f64> {
        let lifted = c.picture_v2().expect("a picture");
        lifted.picture.placement_anim.get(&klipp_graph_format::ParamKey::from(lane)).map(|a| a.keys.iter().filter_map(|k| k["time_secs"].as_f64()).collect()).unwrap_or_default()
    }

    fn clip(seq: &Sequence, id: ClipId) -> &Clip {
        seq.all_clips().find(|c| c.id == id).expect("the clip")
    }

    /// The done-when case at the document: a 4 s clip's end to 8 s plays
    /// the same frames at 50%, its sound with it at its pitch; the next
    /// clip, from 6 s, is run into without ripple, moved by 4 s with it;
    /// one undo puts every clip back as it was.
    #[test]
    fn a_four_second_clip_stretched_to_eight_plays_its_frames_at_half_speed() {
        let (mut seq, pool, v, a, next) = fixture();
        // Room to stretch: the next clip moved out of the way first.
        let refused = stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::EdgeAt(8.0), false, true).unwrap_err();
        assert_eq!(refused.code, Some(RefusalCode::WouldOverlap), "{}", refused.why);
        let (_, edit, _) = stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::EdgeAt(8.0), true, true).unwrap();
        let after = applied(&seq, &pool, &edit);
        let (cv, ca) = (clip(&after, v), clip(&after, a));
        assert_eq!((cv.timeline_in, cv.timeline_out, cv.source_in, cv.source_out), (tc(0.0), tc(8.0), tc(0.0), tc(4.0)));
        assert!((cv.speed - 0.5).abs() < 1e-12 && (ca.speed - 0.5).abs() < 1e-12);
        assert_eq!((secs(ca.timeline_in), secs(ca.timeline_out)), (0.0, 8.0), "its sound lasts 8 s too");
        assert!(ca.keep_pitch && cv.keep_pitch);
        // The same frames: at 0, 4 and 7.96 s the file at 0, 2 and 3.98 s.
        for (t, s) in [(0.0, 0.0), (4.0, 2.0), (7.96, 3.98)] {
            assert!((cv.source_secs_at(t, 25.0) - s).abs() < 1e-9, "{t} s shows {}", cv.source_secs_at(t, 25.0));
        }
        assert_eq!(clip(&after, next).timeline_in, tc(10.0), "the clip after moved by the change");
        let back = undone(&after, &pool, &edit);
        for id in [v, a, next] {
            assert_eq!(format!("{:?}", clip(&back, id)), format!("{:?}", clip(&seq, id)), "one undo restores it exactly");
        }
        // With room, no ripple needed.
        seq.tracks[0].clips.retain(|c| c.id != next);
        assert!(stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::Length(8.0), false, true).is_ok());
    }

    /// From its start: the end stays, the clip starts earlier and plays
    /// slower; a reversed clip stays reversed.
    #[test]
    fn from_the_start_the_end_stays_and_reverse_is_kept() {
        let (mut seq, pool, v, a, next) = fixture();
        seq.tracks[0].clips.retain(|c| c.id != next);
        for c in seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            c.timeline_in = tc(2.0);
            c.timeline_out = tc(6.0);
            c.speed = -1.0;
        }
        let (_, edit, _) = stretch_at(&seq, &pool, &[v], Edge::In, StretchTo::EdgeAt(0.0), false, true).unwrap();
        let after = applied(&seq, &pool, &edit);
        let cv = clip(&after, v);
        assert_eq!((cv.timeline_in, cv.timeline_out), (tc(0.0), tc(6.0)));
        assert!((cv.speed + 4.0 / 6.0).abs() < 1e-12, "reversed, slower: {}", cv.speed);
        assert_eq!(secs(clip(&after, a).timeline_in), 0.0);
    }

    /// A ramp and a keyed transform, stretched to twice the length: the
    /// ramp's shape kept, each key at the same frame of the file.
    #[test]
    fn a_ramp_and_its_keys_stretch_with_the_content() {
        let (mut seq, pool, v, _, next) = fixture();
        seq.tracks[0].clips.retain(|c| c.id != next);
        let ci = seq.tracks[0].clips.iter().position(|c| c.id == v).unwrap();
        let c = &mut seq.tracks[0].clips[ci];
        c.speed_keyframes = vec![key(0.0, 2.0), key(4.0, 0.5)];
        c.timeline_out = c.timeline_in + tc(klipp_timeline::effective_timeline_duration_for_keyframes(&c.speed_keyframes, 4.0, 25.0, 1.0));
        *c = klipp_timeline::test_clip::TestClip::of(c.clone()).lane("pos_x", 1.0, 0.0).lane("pos_x", 2.0, 100.0).build();
        let before = c.clone();
        let file_at_keys: Vec<f64> = lane_times(&before, "pos_x").iter().map(|t| before.source_secs_at(*t, 25.0)).collect();
        assert_eq!(file_at_keys.len(), 2);
        let len = secs(before.duration());
        let (_, edit, _) = stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::Length(len * 2.0), false, true).unwrap();
        let after = applied(&seq, &pool, &edit);
        let cv = clip(&after, v);
        let f = secs(cv.duration()) / len;
        assert!((cv.speed_keyframes[1].time_secs - 4.0 * f).abs() < 1e-9 && (f64::from(cv.speed_keyframes[0].value) - 2.0 / f).abs() < 1e-6);
        for (t, s) in lane_times(cv, "pos_x").iter().zip(&file_at_keys) {
            assert!((cv.source_secs_at(*t, 25.0) - s).abs() < 0.02, "the key at {t} s shows {} s of the file, was {s}", cv.source_secs_at(*t, 25.0));
        }
    }

    /// Two clips that touch, stretched from their shared end: both
    /// twice as long, the second still starting where the first ends.
    #[test]
    fn clips_that_touch_keep_their_timing() {
        let (mut seq, pool, v, _, next) = fixture();
        let ci = seq.tracks[0].clips.iter().position(|c| c.id == next).unwrap();
        seq.tracks[0].clips[ci].timeline_in = tc(4.0);
        seq.tracks[0].clips[ci].timeline_out = tc(7.0);
        let (_, edit, _) = stretch_at(&seq, &pool, &[next, v], Edge::Out, StretchTo::EdgeAt(14.0), false, true).unwrap();
        let after = applied(&seq, &pool, &edit);
        assert_eq!((clip(&after, v).timeline_in, clip(&after, v).timeline_out), (tc(0.0), tc(8.0)));
        assert_eq!((clip(&after, next).timeline_in, clip(&after, next).timeline_out), (tc(8.0), tc(14.0)));
    }

    /// A multicam group's other angle stretches with the angle named, in
    /// step.
    #[test]
    fn a_multicam_groups_angles_stretch_together() {
        let (mut seq, pool, v, _, next) = fixture();
        seq.tracks[0].clips.retain(|c| c.id != next);
        let group = klipp_core::GroupId::new();
        let ci = seq.tracks[0].clips.iter().position(|c| c.id == v).unwrap();
        seq.tracks[0].clips[ci].multicam_group = Some(group);
        let mut angle = seq.tracks[0].clips[ci].clone();
        angle.id = ClipId::new();
        angle.linked_clip_id = None;
        let angle_id = angle.id;
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![angle];
        seq.tracks.push(v2);
        let (_, edit, _) = stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::Length(8.0), false, true).unwrap();
        let after = applied(&seq, &pool, &edit);
        for id in [v, angle_id] {
            let c = clip(&after, id);
            assert_eq!((secs(c.timeline_out), c.speed), (8.0, 0.5), "every angle in step");
        }
    }

    /// A title changes length only; the limits are said; a locked track
    /// refuses.
    #[test]
    fn a_title_changes_length_only_and_the_limits_and_locks_refuse() {
        let (mut seq, pool, v, _, next) = fixture();
        let title = title_clip(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox::default()), tc(20.0), tc(23.0), seq.fps);
        let title = klipp_timeline::test_clip::TestClip::of(title).lane("pos_x", 1.0, 1.0).build();
        let tid = title.id;
        let mut tt = Track::new("T1", TrackKind::Video);
        tt.clips = vec![title];
        seq.tracks.push(tt);
        let (_, edit, _) = stretch_at(&seq, &pool, &[tid], Edge::Out, StretchTo::Length(6.0), false, true).unwrap();
        let t = clip(&applied(&seq, &pool, &edit), tid).clone();
        assert_eq!((t.timeline_out, t.speed, lane_times(&t, "pos_x")), (tc(26.0), 1.0, vec![1.0]), "length only");
        let r = stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::Length(0.1), true, true).unwrap_err();
        assert_eq!(r.code, Some(RefusalCode::SpeedOutOfRange), "{}", r.why);
        assert!(r.why.contains("4000%"), "says what it needs: {}", r.why);
        let (lo, hi) = stretch_range([clip(&seq, v)], &pool);
        assert!((lo - 1.0 / 16.0).abs() < 1e-12 && (hi - 10.0).abs() < 1e-9);
        seq.tracks[0].clips.retain(|c| c.id != next);
        seq.tracks[1].locked = true;
        assert_eq!(stretch_at(&seq, &pool, &[v], Edge::Out, StretchTo::Length(5.0), false, true).unwrap_err().code, Some(RefusalCode::TrackLocked));
    }
}
