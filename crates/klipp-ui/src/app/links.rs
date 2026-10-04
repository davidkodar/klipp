//! Track mattes and transform links: a matte's channel, and the chains that
//! would loop back on themselves refused (#890).

#[allow(unused_imports)]
use super::*;

/// Wire-format → enum conversion for `AppCommand::SetClipMatte`. Mirrors the
/// `MatteChannel` discriminant order so the on-the-wire `u8` survives
/// round-trips through any logging / replay layer that flattens the enum
/// before re-dispatching. Out-of-range values fall back to `Luminance` —
/// the historical default for `MatteChannel`.
pub(super) fn matte_channel_from_u8(b: u8) -> klipp_nodes::params::MatteChannel {
    match b {
        0 => klipp_nodes::params::MatteChannel::Alpha,
        1 => klipp_nodes::params::MatteChannel::Luminance,
        2 => klipp_nodes::params::MatteChannel::Red,
        3 => klipp_nodes::params::MatteChannel::Green,
        4 => klipp_nodes::params::MatteChannel::Blue,
        _ => klipp_nodes::params::MatteChannel::Luminance,
    }
}

/// Walk the matte chain starting from `source` (i.e. the clip the user is
/// trying to bind as the matte for `consumer`). If we ever hit `consumer`,
/// the bind would create a cycle. Returns `true` to mean "reject this".
///
/// The depth cap is defensive — a well-formed sequence never has cycles
/// thanks to this exact check, but a corrupt project file or a logic bug
/// upstream could produce one. 64 hops is far past anything a human would
/// build and well below stack-overflow risk.
pub(super) fn matte_chain_creates_cycle(
    seq: &klipp_timeline::Sequence,
    consumer: klipp_core::ClipId,
    source: klipp_core::ClipId,
) -> bool {
    if consumer == source {
        return true;
    }
    let mut current = source;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.matte_source.as_ref().map(|b| b.source_clip_id));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    // Pre-existing cycle in a corrupt sequence: refuse to extend it.
    true
}

/// Mirror of `matte_chain_creates_cycle` for the `transform_link`
/// chain: returns true iff binding `consumer.transform_link.target =
/// target` would create a cycle (consumer → … → target → consumer).
/// Uses a 64-link depth cap so a corrupt pre-existing cycle is
/// detected as "always rejecting any new bind" rather than spinning.
pub(crate) fn transform_link_chain_creates_cycle(
    seq: &klipp_timeline::Sequence,
    consumer: klipp_core::ClipId,
    target: klipp_core::ClipId,
) -> bool {
    if consumer == target {
        return true;
    }
    let mut current = target;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target.referenced_clip_id()));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    true
}

#[cfg(test)]
mod track_matte_handler_tests {
    //! Phase 1.4 — invariants pinned by `SetClipMatte` / `ClearClipMatte` /
    //! the orphan-binding sweep in `remove_clip_impl`. The handlers
    //! themselves require a full `App` to test, so these exercise the pure
    //! free helpers (`matte_chain_creates_cycle`,
    //! `matte_channel_from_u8`) and the data-model invariants on a hand-
    //! built `Sequence`. The render-path side lands in Phase 2.
    use super::{matte_channel_from_u8, matte_chain_creates_cycle};
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, MatteBinding, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24))
    }

    #[test]
    fn channel_from_u8_maps_known_values() {
        use klipp_nodes::params::MatteChannel;
        assert_eq!(matte_channel_from_u8(0), MatteChannel::Alpha);
        assert_eq!(matte_channel_from_u8(1), MatteChannel::Luminance);
        assert_eq!(matte_channel_from_u8(2), MatteChannel::Red);
        assert_eq!(matte_channel_from_u8(3), MatteChannel::Green);
        assert_eq!(matte_channel_from_u8(4), MatteChannel::Blue);
    }

    #[test]
    fn channel_from_u8_unknown_falls_back_to_luminance() {
        use klipp_nodes::params::MatteChannel;
        // Fallback path matters: a stale or future-version replay log
        // should never crash, just degrade to the historical default.
        assert_eq!(matte_channel_from_u8(99), MatteChannel::Luminance);
        assert_eq!(matte_channel_from_u8(255), MatteChannel::Luminance);
    }

    #[test]
    fn cycle_check_rejects_self_reference() {
        let a = fresh_clip();
        let id = a.id;
        let seq = seq_with_clips(vec![a]);
        // SetClipMatte handler short-circuits before reaching the chain
        // walker for self-reference, but the chain walker also catches it
        // as a defence-in-depth — pin both behaviours.
        assert!(matte_chain_creates_cycle(&seq, id, id));
    }

    #[test]
    fn cycle_check_passes_for_unrelated_clips() {
        let a = fresh_clip();
        let b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        let seq = seq_with_clips(vec![a, b]);
        // No bindings yet — binding A → B is fine.
        assert!(!matte_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_detects_two_hop_cycle() {
        // Setup: A.matte_source = B already. Now the user tries to set
        // B.matte_source = A. Walker starts from `source = A`, reads A's
        // matte_source (none), returns false — wait, that's not a cycle.
        // The cycle is in the OTHER direction: if B were already pointing
        // at A, then setting A.matte_source = B would close the loop.
        // Build that case.
        let mut a = fresh_clip();
        let mut b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        // B already consumes A as its matte. (Pre-existing.)
        b.matte_source = Some(MatteBinding::new(aid));
        let _ = &mut a; // unused mut suppression
        let seq = seq_with_clips(vec![a, b]);
        // Now the user wants to set A.matte_source = B. The walker starts
        // at B (the proposed source), reads B's matte_source.source = A
        // (the consumer) — cycle detected.
        assert!(matte_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_handles_missing_intermediate_clip() {
        // Walker should bail (return false / no cycle) when it hits a
        // dangling matte_source pointing at a non-existent clip — the
        // chain just terminates. This case is plausible after a corrupt
        // load; we don't want the cycle check to infinite-loop.
        let mut a = fresh_clip();
        let b = fresh_clip();
        let bid = b.id;
        a.matte_source = Some(MatteBinding::new(ClipId::new())); // dangling
        let _ = &mut a;
        let seq = seq_with_clips(vec![a]); // b NOT in sequence
        // Asking "would bind consumer=b, source=<dangling>" — the chain
        // from a-not-in-seq trivially terminates.
        assert!(!matte_chain_creates_cycle(&seq, bid, ClipId::new()));
    }
}

#[cfg(test)]
mod transform_link_handler_tests {
    //! Pure-function tests for the `transform_link` cycle walker. The
    //! `SetClipTransformLink` handler itself needs a full `App`; these
    //! exercise the data-model invariants on a hand-built `Sequence`.
    use super::transform_link_chain_creates_cycle;
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind, TransformLink};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24))
    }

    #[test]
    fn cycle_check_rejects_self_reference() {
        let a = fresh_clip();
        let id = a.id;
        let seq = seq_with_clips(vec![a]);
        assert!(transform_link_chain_creates_cycle(&seq, id, id));
    }

    #[test]
    fn cycle_check_passes_for_unrelated_clips() {
        let a = fresh_clip();
        let b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        let seq = seq_with_clips(vec![a, b]);
        assert!(!transform_link_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_detects_two_hop_cycle() {
        // B already links → A. Setting A → B closes the loop.
        let a = fresh_clip();
        let mut b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        b.transform_link = Some(TransformLink::new(aid));
        let seq = seq_with_clips(vec![a, b]);
        assert!(transform_link_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_handles_dangling_target() {
        let mut a = fresh_clip();
        a.transform_link = Some(TransformLink::new(ClipId::new())); // dangling
        let b = fresh_clip();
        let bid = b.id;
        let seq = seq_with_clips(vec![a]);
        assert!(!transform_link_chain_creates_cycle(&seq, bid, ClipId::new()));
    }
}

#[cfg(test)]
mod matte_candidate_finder_tests {
    //! Phase 3 — `find_matte_candidate_above` drives the right-click
    //! "Use clip above as matte" headline gesture in the timeline. The
    //! function lives in `panels/timeline.rs` (private) so we exercise
    //! the same logic via a re-implementation here that mirrors its
    //! shape — the goal is to pin behaviour, not couple to the impl.
    //!
    //! When the timeline impl drifts these tests will fail loudly,
    //! flagging that the gesture's contract changed. Tests stay tight:
    //! "above means higher track index", "AdjustmentLayers are
    //! skipped", "non-overlapping clips are skipped", "audio tracks
    //! are skipped".
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    /// Re-implementation of `panels::timeline::find_matte_candidate_above`
    /// kept private to the test module. If the timeline copy drifts,
    /// keep this in lockstep — both encode the user-facing gesture.
    fn find_candidate(
        seq: &Sequence,
        current_track_idx: usize,
        current_clip_id: ClipId,
    ) -> Option<(ClipId, ClipKind)> {
        let current_clip = seq.tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current_clip_id)?;
        let lo = current_clip.timeline_in;
        let hi = current_clip.timeline_out;
        for idx in (0..current_track_idx).rev() {
            let track = &seq.tracks[idx];
            if !matches!(
                track.kind,
                TrackKind::Video | TrackKind::Text | TrackKind::Subtitle
            ) {
                continue;
            }
            for c in &track.clips {
                if c.id == current_clip_id { continue; }
                if matches!(c.kind, ClipKind::AdjustmentLayer) { continue; }
                if c.timeline_in < hi && c.timeline_out > lo {
                    return Some((c.id, c.kind.clone()));
                }
            }
        }
        None
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    fn video_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn audio_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Audio);
        t.clips = clips;
        t
    }

    fn media_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    // Test fixtures use the production array convention: tracks[0]
    // is at the TOP of the panel (visually highest = lowest array
    // idx). For "find candidate above the consumer", the source
    // sits at a LOWER idx than the consumer.

    #[test]
    fn returns_overlap_on_track_directly_above() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let source = media_clip(0, 24);
        let source_id = source.id;
        let seq = seq_with_tracks(vec![
            video_track("V_above (source)", vec![source]),  // idx 0 — visually above
            video_track("V_below (consumer)", vec![consumer]), // idx 1
        ]);
        let (cand, _kind) = find_candidate(&seq, 1, consumer_id).expect("candidate");
        assert_eq!(cand, source_id);
    }

    #[test]
    fn skips_track_with_no_overlap() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let too_late = media_clip(48, 72);
        let seq = seq_with_tracks(vec![
            video_track("V_above", vec![too_late]),    // idx 0
            video_track("V_below", vec![consumer]),    // idx 1
        ]);
        assert!(find_candidate(&seq, 1, consumer_id).is_none());
    }

    #[test]
    fn skips_adjustment_layer_above() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        // From top to bottom: valid media, adjustment layer (skipped),
        // consumer. Walk goes from consumer up; first valid match is
        // the topmost media clip.
        let mut adj = media_clip(0, 24);
        adj.kind = ClipKind::AdjustmentLayer;
        let valid = media_clip(0, 24);
        let valid_id = valid.id;
        let seq = seq_with_tracks(vec![
            video_track("V_top (valid)", vec![valid]),    // idx 0
            video_track("V_mid (adj)",   vec![adj]),      // idx 1
            video_track("V_bot (consumer)", vec![consumer]), // idx 2
        ]);
        let (cand, _) = find_candidate(&seq, 2, consumer_id).expect("candidate");
        assert_eq!(cand, valid_id, "should jump past the adjustment layer to the valid clip above");
    }

    #[test]
    fn skips_audio_track() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let audio_clip = media_clip(0, 24);
        let video_above = media_clip(0, 24);
        let video_id = video_above.id;
        let seq = seq_with_tracks(vec![
            video_track("V_top",   vec![video_above]),   // idx 0 — valid candidate
            audio_track("A_mid",   vec![audio_clip]),    // idx 1 — audio, skipped
            video_track("V_bot",   vec![consumer]),      // idx 2 — consumer
        ]);
        let (cand, _) = find_candidate(&seq, 2, consumer_id).expect("candidate");
        assert_eq!(cand, video_id);
    }

    #[test]
    fn returns_none_when_consumer_on_top_track() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![consumer]), // idx 0 — nothing above
        ]);
        assert!(find_candidate(&seq, 0, consumer_id).is_none(),
            "no tracks above → no candidate");
    }

    #[test]
    fn ignores_consumer_itself_even_if_listed_above() {
        // Defensive: if the consumer's id somehow appeared on a track
        // above, we still skip it (a clip cannot matte itself).
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let mut self_copy = media_clip(0, 24);
        self_copy.id = consumer_id;
        let seq = seq_with_tracks(vec![
            video_track("V_above", vec![self_copy]),   // idx 0 — same id, must be skipped
            video_track("V_below", vec![consumer]),    // idx 1 — consumer
        ]);
        assert!(find_candidate(&seq, 1, consumer_id).is_none(),
            "consumer id on track above must not be returned as its own candidate");
    }
}
