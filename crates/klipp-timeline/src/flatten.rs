//! Compound clip flattening — shared by viewer (`sync_sequence_to_engine`)
//! and exporter (`snapshot.rs`).
//!
//! [`flatten_sequence`] produces a flat copy of a `Sequence` with every
//! `ClipKind::Sequence { sequence_id }` clip replaced by the inner clips
//! it references, with timeline coordinates remapped onto the parent.
//! The viewer engine has no native compound-clip awareness — it consumes
//! a flat sequence; the exporter does its own per-clip mapping on top of
//! the same flat output.
//!
//! # Known limitations
//!
//! **Linked clip pairs:** If a compound clip's inner sequence contains
//! clips linked via `linked_clip_id` (audio/video pairs), the flattened
//! output preserves the original `ClipId` values. If the same inner clip
//! appears in two separate compound clip instances, both flattened copies
//! share the same `ClipId`. The engine's linked-clip resolution
//! (`iter().find(|c| c.id == lid)`) returns the first match, which may
//! not be the correct instance.
//!
//! **Workaround:** Compound clips that contain linked audio/video pairs
//! will have their audio/video sync preserved if the compound clip is the
//! only user of that inner sequence. When the same inner sequence is used
//! in multiple compound clip instances simultaneously, audio/video sync
//! within those instances may be incorrect. Re-IDing flattened clips is
//! deferred to a follow-up PRD.

use crate::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_core::{RationalTime, SequenceId};

/// Maximum nesting depth for compound clips. Beyond this depth the
/// recursion stops and no further clips are produced.
pub const MAX_COMPOUND_DEPTH: u8 = 8;

/// Produce a flat copy of `seq` with all `ClipKind::Sequence` clips
/// recursively replaced by the inner clips they reference.
/// `all_sequences` must contain every sequence in the project.
/// The returned `Sequence` shares `seq`'s id, name, fps, resolution,
/// markers, in_point, out_point, and last_playhead — only `tracks` differ.
pub fn flatten_sequence(seq: &Sequence, all_sequences: &[Sequence]) -> Sequence {
    let mut flat = seq.clone();
    flat.tracks = seq
        .tracks
        .iter()
        .map(|track| flatten_track(track, all_sequences, 0))
        .collect();
    flat
}

/// Returns true if a compound clip on a `parent_kind` track should pull in
/// inner clips from a track of `inner_kind`. Mirrors the per-track-type
/// split in the export path: video/text/subtitle compose together; audio
/// is its own lane.
fn track_kinds_compatible(parent_kind: TrackKind, inner_kind: TrackKind) -> bool {
    match parent_kind {
        TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => matches!(
            inner_kind,
            TrackKind::Video | TrackKind::Text | TrackKind::Subtitle
        ),
        TrackKind::Audio => inner_kind == TrackKind::Audio,
    }
}

fn flatten_track(track: &Track, all_sequences: &[Sequence], depth: u8) -> Track {
    let mut flat = track.clone();
    let track_kind = track.kind;
    flat.clips = track
        .clips
        .iter()
        .flat_map(|clip| {
            if let ClipKind::Sequence { sequence_id } = &clip.kind {
                if depth < MAX_COMPOUND_DEPTH {
                    return flatten_clip_for_track(
                        clip,
                        *sequence_id,
                        all_sequences,
                        track_kind,
                        depth,
                    );
                }
            }
            vec![clip.clone()]
        })
        .collect();
    flat
}

/// Expand one compound clip into the inner clips it contains, with time
/// coordinates remapped onto the parent timeline. Inner clips are pulled
/// from every inner track regardless of kind. Returns an empty `Vec` if
/// `sequence_id` does not exist in `all_sequences` (defensive — matches
/// the export path's behavior when the project graph is inconsistent).
///
/// Most callers want [`flatten_clip_for_track`] instead, which respects
/// the parent track's kind (video/audio split).
pub fn flatten_clip(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    depth: u8,
) -> Vec<Clip> {
    flatten_clip_inner(compound, sequence_id, all_sequences, None, depth)
}

/// Expand one compound clip, restricting inner clips to tracks whose kind
/// is compatible with `parent_kind` (see `track_kinds_compatible`).
pub fn flatten_clip_for_track(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    parent_kind: TrackKind,
    depth: u8,
) -> Vec<Clip> {
    flatten_clip_inner(compound, sequence_id, all_sequences, Some(parent_kind), depth)
}

fn flatten_clip_inner(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    parent_kind: Option<TrackKind>,
    depth: u8,
) -> Vec<Clip> {
    let inner_seq = match all_sequences.iter().find(|s| s.id == sequence_id) {
        Some(s) => s,
        None => return Vec::new(),
    };

    let compound_timeline_in = compound.timeline_in.to_secs_f64();
    let compound_source_in = compound.source_in.to_secs_f64();
    let compound_source_out = compound.source_out.to_secs_f64();
    let compound_speed = compound.speed;

    let rate = compound.timeline_in.rate;
    let mut out = Vec::new();

    for inner_track in &inner_seq.tracks {
        if let Some(pk) = parent_kind {
            if !track_kinds_compatible(pk, inner_track.kind) {
                continue;
            }
        }
        for inner_clip in &inner_track.clips {
            if !inner_clip.enabled {
                continue;
            }
            if matches!(inner_clip.kind, ClipKind::Gap) {
                continue;
            }

            let sliced = match slice_inner_clip_to_compound(
                inner_clip,
                compound_timeline_in,
                compound_source_in,
                compound_source_out,
                compound_speed,
                rate,
            ) {
                Some(c) => c,
                None => continue,
            };

            // Recurse into nested compound clips — feed the sliced synthetic
            // back through flatten_clip_inner so the inner-of-inner clips get
            // remapped against the new compound bounds.
            if let ClipKind::Sequence {
                sequence_id: nested_id,
            } = &sliced.kind
            {
                if depth + 1 < MAX_COMPOUND_DEPTH {
                    out.extend(flatten_clip_inner(
                        &sliced,
                        *nested_id,
                        all_sequences,
                        parent_kind,
                        depth + 1,
                    ));
                }
                continue;
            }

            out.push(sliced);
        }
    }
    out
}

/// Slice one inner clip to a compound clip's source window and remap its
/// time coordinates onto the parent timeline.
///
/// - Returns `None` if the inner clip lies entirely outside the compound's
///   source window (caller should drop it).
/// - Honours `compound_speed`: parent-timeline duration equals the
///   inner-timeline visible span divided by the compound speed.
/// - Multiplies the inner clip's own `speed` by `compound_speed` so the
///   restored leaf plays at the combined rate.
///
/// All `*_secs` parameters are seconds; `rate` is the `RationalTime` rate
/// the output time fields will be expressed in.
pub fn slice_inner_clip_to_compound(
    inner: &Clip,
    compound_timeline_in_secs: f64,
    compound_source_in_secs: f64,
    compound_source_out_secs: f64,
    compound_speed: f64,
    rate: u32,
) -> Option<Clip> {
    let ic_in = inner.timeline_in.to_secs_f64();
    let ic_out = inner.timeline_out.to_secs_f64();

    if ic_out <= compound_source_in_secs || ic_in >= compound_source_out_secs {
        return None;
    }

    let visible_in = ic_in.max(compound_source_in_secs);
    let visible_out = ic_out.min(compound_source_out_secs);

    // Defensive: avoid divide-by-zero if speed is somehow 0 or negative.
    let speed = if compound_speed.abs() < 1e-9 { 1.0 } else { compound_speed };

    let parent_in =
        compound_timeline_in_secs + (visible_in - compound_source_in_secs) / speed;
    let parent_out =
        compound_timeline_in_secs + (visible_out - compound_source_in_secs) / speed;

    let source_in_offset = visible_in - ic_in;
    let inner_source_in_secs = inner.source_in.to_secs_f64() + source_in_offset;
    let inner_source_out_secs = inner.source_out.to_secs_f64() - (ic_out - visible_out);

    let mut flat = inner.clone();
    flat.timeline_in = RationalTime::from_secs_f64(parent_in, rate);
    flat.timeline_out = RationalTime::from_secs_f64(parent_out, rate);
    flat.source_in = RationalTime::from_secs_f64(inner_source_in_secs, rate);
    flat.source_out = RationalTime::from_secs_f64(inner_source_out_secs, rate);
    flat.speed = inner.speed * speed;
    Some(flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Sequence, Track, TrackKind};
    use klipp_core::{Fps, MediaId, RationalTime, Resolution, SequenceId};

    fn make_seq(name: &str) -> Sequence {
        Sequence {
            id: SequenceId::new(),
            name: name.to_string(),
            fps: Fps::new(24, 1),
            resolution: Resolution { width: 1920, height: 1080 },
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
        }
    }

    fn t(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 24)
    }

    fn media_clip(in_f: i64, out_f: i64, src_in_f: i64, src_out_f: i64) -> Clip {
        Clip::media(MediaId::new(), t(in_f), t(out_f), t(src_in_f), t(src_out_f))
    }

    #[test]
    fn flatten_two_media_clips_in_compound() {
        let mut child = make_seq("child");
        let mut child_track = Track::new("V1", TrackKind::Video);
        child_track.clips.push(media_clip(0, 24, 0, 24));   // 0..1s
        child_track.clips.push(media_clip(24, 48, 0, 24));  // 1..2s
        child.tracks.push(child_track);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut parent_track = Track::new("V1", TrackKind::Video);
        // Compound clip placed at 100..148 frames (4..6.16s) on parent
        parent_track
            .clips
            .push(Clip::sequence(child_id, t(100), t(148)));
        parent.tracks.push(parent_track);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert_eq!(flat.tracks.len(), 1);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        // First leaf: parent_in = 100 + (0 - 0) = 100, parent_out = 100 + 24 = 124
        assert_eq!(clips[0].timeline_in, t(100));
        assert_eq!(clips[0].timeline_out, t(124));
        assert_eq!(clips[0].source_in, t(0));
        assert_eq!(clips[0].source_out, t(24));
        // Second leaf: parent_in = 124, parent_out = 148
        assert_eq!(clips[1].timeline_in, t(124));
        assert_eq!(clips[1].timeline_out, t(148));
    }

    #[test]
    fn compound_trimmed_clips_inner() {
        // Inner sequence has a 4-second clip [0..96 frames]
        // Compound clip's source window only exposes [24..72] (1..3s)
        // Expect: one leaf clip on parent, source range 24..72, parent timeline width 48
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 96, 0, 96));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        let mut compound = Clip::sequence(child_id, t(200), t(248));
        compound.source_in = t(24);
        compound.source_out = t(72);
        ptr.clips.push(compound);
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].timeline_in, t(200));
        assert_eq!(clips[0].timeline_out, t(248));
        assert_eq!(clips[0].source_in, t(24));
        assert_eq!(clips[0].source_out, t(72));
    }

    #[test]
    fn nested_compound_two_levels() {
        // grandchild contains a media clip
        // child contains a compound clip referring to grandchild
        // parent contains a compound clip referring to child
        let mut grandchild = make_seq("gc");
        let mut gtr = Track::new("V1", TrackKind::Video);
        gtr.clips.push(media_clip(0, 24, 0, 24));
        grandchild.tracks.push(gtr);
        let gc_id = grandchild.id;

        let mut child = make_seq("child");
        let mut ctr = Track::new("V1", TrackKind::Video);
        ctr.clips.push(Clip::sequence(gc_id, t(0), t(24)));
        child.tracks.push(ctr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(50), t(74)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(
            &parent,
            &[parent.clone(), child.clone(), grandchild.clone()],
        );
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].timeline_in, t(50));
        assert_eq!(clips[0].timeline_out, t(74));
    }

    #[test]
    fn depth_limit_does_not_panic() {
        // Build a self-referential chain of MAX_COMPOUND_DEPTH + 2 sequences.
        // Each contains one compound clip pointing to the next.
        let mut seqs: Vec<Sequence> = (0..(MAX_COMPOUND_DEPTH as usize + 2))
            .map(|i| make_seq(&format!("s{i}")))
            .collect();
        // Wire each seq[i] to contain seq[i+1] as a compound clip.
        for i in 0..seqs.len() - 1 {
            let next_id = seqs[i + 1].id;
            let mut tr = Track::new("V1", TrackKind::Video);
            tr.clips.push(Clip::sequence(next_id, t(0), t(24)));
            seqs[i].tracks.push(tr);
        }
        // Tail leaf: actual media clip
        let mut tail_tr = Track::new("V1", TrackKind::Video);
        tail_tr.clips.push(media_clip(0, 24, 0, 24));
        seqs.last_mut().unwrap().tracks.push(tail_tr);

        let parent = seqs[0].clone();
        let _flat = flatten_sequence(&parent, &seqs); // must not panic
    }

    #[test]
    fn compound_speed_propagates() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        let mut compound = Clip::sequence(child_id, t(0), t(24));
        compound.speed = 2.0;
        ptr.clips.push(compound);
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert!((clips[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_sequence_id_returns_empty() {
        let parent = make_seq("parent");
        let bogus = SequenceId::new();
        let compound = Clip::sequence(bogus, t(0), t(24));
        let result = flatten_clip(&compound, bogus, std::slice::from_ref(&parent), 0);
        assert!(result.is_empty());
    }

    #[test]
    fn gap_clips_in_inner_sequence_are_skipped() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(Clip::gap(t(0), t(24)));
        tr.clips.push(media_clip(24, 48, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(48)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1, "Gap should be skipped, only media remains");
    }

    #[test]
    fn disabled_inner_clips_are_skipped() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        let mut c = media_clip(0, 24, 0, 24);
        c.enabled = false;
        tr.clips.push(c);
        tr.clips.push(media_clip(24, 48, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(48)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
    }

    #[test]
    fn empty_inner_sequence_yields_empty() {
        let child = make_seq("child");
        let child_id = child.id;
        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(24)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert!(flat.tracks[0].clips.is_empty());
    }

    #[test]
    fn audio_track_inner_clips_carry_compound_speed() {
        // Pre-existing export bug: compound.speed was dropped on the audio
        // path. The shared flattener now propagates it for all track kinds.
        let mut child = make_seq("child");
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut patr = Track::new("A1", TrackKind::Audio);
        let mut compound = Clip::sequence(child_id, t(0), t(24));
        compound.speed = 2.0;
        patr.clips.push(compound);
        parent.tracks.push(patr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert_eq!(flat.tracks[0].clips.len(), 1);
        assert!((flat.tracks[0].clips[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn track_kind_filter_audio_excludes_video_inner_clips() {
        // An audio compound clip should pull only audio inner clips,
        // even when the inner sequence has both video and audio.
        let mut child = make_seq("child");
        let mut vtr = Track::new("V1", TrackKind::Video);
        vtr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(vtr);
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let compound = Clip::sequence(child_id, t(0), t(24));
        let leaves = flatten_clip_for_track(
            &compound,
            child_id,
            &[child.clone()],
            TrackKind::Audio,
            0,
        );
        assert_eq!(leaves.len(), 1, "audio parent track must yield only audio inner clips");
    }

    #[test]
    fn track_kind_filter_video_excludes_audio_inner_clips() {
        let mut child = make_seq("child");
        let mut vtr = Track::new("V1", TrackKind::Video);
        vtr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(vtr);
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let compound = Clip::sequence(child_id, t(0), t(24));
        let leaves = flatten_clip_for_track(
            &compound,
            child_id,
            &[child.clone()],
            TrackKind::Video,
            0,
        );
        assert_eq!(leaves.len(), 1, "video parent track must yield only video inner clips");
    }

    #[test]
    fn flatten_sequence_yields_no_compound_clips() {
        // After flattening, no clip in any track should be ClipKind::Sequence.
        // This is the core invariant the engine depends on.
        let mut grandchild = make_seq("gc");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        grandchild.tracks.push(tr);
        let gc_id = grandchild.id;

        let mut child = make_seq("child");
        let mut ctr = Track::new("V1", TrackKind::Video);
        ctr.clips.push(Clip::sequence(gc_id, t(0), t(24)));
        child.tracks.push(ctr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(24)));
        ptr.clips.push(media_clip(24, 48, 0, 24));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(
            &parent,
            &[parent.clone(), child.clone(), grandchild.clone()],
        );
        for track in &flat.tracks {
            for clip in &track.clips {
                assert!(
                    !matches!(clip.kind, ClipKind::Sequence { .. }),
                    "flattened sequence must not contain ClipKind::Sequence clips"
                );
            }
        }
    }

    #[test]
    fn no_compound_clips_is_structurally_identical() {
        let mut seq = make_seq("plain");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        tr.clips.push(media_clip(24, 48, 0, 24));
        seq.tracks.push(tr);

        let flat = flatten_sequence(&seq, &[seq.clone()]);
        assert_eq!(flat.id, seq.id);
        assert_eq!(flat.tracks.len(), seq.tracks.len());
        assert_eq!(flat.tracks[0].clips.len(), seq.tracks[0].clips.len());
        for (a, b) in flat.tracks[0].clips.iter().zip(&seq.tracks[0].clips) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.timeline_in, b.timeline_in);
            assert_eq!(a.timeline_out, b.timeline_out);
        }
    }

    // ─── slice_inner_clip_to_compound ───────────────────────────────────────

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn slice_inner_fully_inside_window() {
        // Compound: parent [100..148], source window [0..48], speed 1.0.
        // Inner clip on inner timeline at [12..36], source [0..24].
        let inner = media_clip(12, 36, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, /*parent_in*/ 100.0 / 24.0,
            /*src_in*/ 0.0, /*src_out*/ 48.0 / 24.0,
            1.0, 24,
        )
        .expect("inside window should slice");
        // visible_in=12/24, visible_out=36/24 → parent_in=100/24+12/24, parent_out=100/24+36/24
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), (100.0 + 12.0) / 24.0));
        assert!(approx_eq(sliced.timeline_out.to_secs_f64(), (100.0 + 36.0) / 24.0));
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 24.0 / 24.0));
        assert!(approx_eq(sliced.speed, 1.0));
    }

    #[test]
    fn slice_inner_outside_left_returns_none() {
        // Inner clip at inner timeline [0..24]; compound source window [48..72].
        let inner = media_clip(0, 24, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 48.0 / 24.0, 72.0 / 24.0, 1.0, 24,
        );
        assert!(sliced.is_none());
    }

    #[test]
    fn slice_inner_outside_right_returns_none() {
        // Inner clip at inner timeline [72..96]; compound source window [0..48].
        let inner = media_clip(72, 96, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 48.0 / 24.0, 1.0, 24,
        );
        assert!(sliced.is_none());
    }

    #[test]
    fn slice_inner_clipped_at_left_advances_source_in() {
        // Inner [0..48] with source [0..48]. Compound source window [12..48].
        // Visible window: [12..48]; left-clipped by 12.
        // source_in advances from 0 to 12 (in inner-timeline secs == source secs).
        let inner = media_clip(0, 48, 0, 48);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 12.0 / 24.0, 48.0 / 24.0, 1.0, 24,
        )
        .expect("partial overlap should slice");
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 12.0 / 24.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 48.0 / 24.0));
        // parent_in starts at 0 (the trim starts at the compound's left edge)
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), 0.0));
    }

    #[test]
    fn slice_inner_clipped_at_right_pulls_source_out_back() {
        // Inner [0..48] with source [0..48]. Compound source window [0..36].
        // Visible window: [0..36]; right-clipped by 12.
        let inner = media_clip(0, 48, 0, 48);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 36.0 / 24.0, 1.0, 24,
        )
        .expect("partial overlap should slice");
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 36.0 / 24.0));
    }

    #[test]
    fn slice_compound_speed_two_halves_parent_interval() {
        // Inner [0..24] with source [0..24]. Compound source window [0..24].
        // Compound speed 2.0 → parent_out = parent_in + 24/2 = parent_in + 12.
        let inner = media_clip(0, 24, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 24.0 / 24.0, 2.0, 24,
        )
        .expect("speed=2 should still slice");
        // parent timeline duration is half the inner-timeline window (24/2=12 frames=0.5s)
        assert!(approx_eq(
            sliced.timeline_out.to_secs_f64() - sliced.timeline_in.to_secs_f64(),
            12.0 / 24.0,
        ));
        // speed = inner.speed (1.0) * compound.speed (2.0)
        assert!(approx_eq(sliced.speed, 2.0));
        // source window unchanged (the inner clip's own media still spans [0..24])
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 24.0 / 24.0));
    }

    #[test]
    fn slice_compound_speed_one_matches_old_additive_math() {
        // Regression: speed=1.0 must match the old additive flatten math.
        let inner = media_clip(12, 36, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 100.0 / 24.0, 0.0, 48.0 / 24.0, 1.0, 24,
        )
        .expect("speed=1 should slice");
        let parent_in_secs = 100.0 / 24.0 + 12.0 / 24.0;
        let parent_out_secs = 100.0 / 24.0 + 36.0 / 24.0;
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), parent_in_secs));
        assert!(approx_eq(sliced.timeline_out.to_secs_f64(), parent_out_secs));
        assert!(approx_eq(sliced.speed, 1.0));
    }
}
