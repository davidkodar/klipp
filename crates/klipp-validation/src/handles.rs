//! Handle-frame availability detection for transitions.
//!
//! A transition between two clips needs material beyond the cut point on
//! both sides to have something to blend. These beyond-the-cut frames are
//! called "handles" — they exist in the source media file but are trimmed
//! off the timeline. When a transition's duration exceeds the handles
//! available on either clip, the engine today silently produces a
//! freeze-frame by accident; the purpose of this module is to detect that
//! situation precisely so the UI can warn and the export pipeline can
//! block.
//!
//! Handle computation is alignment-aware:
//! - **Center**: needs `duration/2` on A's tail AND `duration/2` on B's head
//!   (using the same integer floor the engine uses at `transition.rs:83`)
//! - **StartAtCut**: needs `duration` on B's head; A contributes nothing
//! - **EndAtCut**: needs `duration` on A's tail; B contributes nothing

use klipp_core::TransitionAlignment;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Transition};

/// How many handle frames a transition needs vs. has on each side.
///
/// All frame counts are in the transition's own frame rate (derived from
/// `transition.duration.rate`). A shortfall on either side means the
/// engine will freeze-frame at playback and the export should block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandleAvailability {
    /// Frames needed from clip A's tail (material past clip A's `source_out`).
    pub clip_a_needed: i64,
    /// Frames actually available in clip A's source file past `source_out`.
    /// `None` means unknown (e.g., clip A is missing or isn't a media clip).
    pub clip_a_available: Option<i64>,

    /// Frames needed from clip B's head (material before clip B's `source_in`).
    pub clip_b_needed: i64,
    /// Frames actually available in clip B's source file before `source_in`.
    /// `None` means unknown.
    pub clip_b_available: Option<i64>,
}

impl HandleAvailability {
    /// Frames short on clip A's side, or 0 if sufficient or unknown.
    pub fn shortfall_a(&self) -> i64 {
        match self.clip_a_available {
            Some(avail) => (self.clip_a_needed - avail).max(0),
            None => 0,
        }
    }

    /// Frames short on clip B's side, or 0 if sufficient or unknown.
    pub fn shortfall_b(&self) -> i64 {
        match self.clip_b_available {
            Some(avail) => (self.clip_b_needed - avail).max(0),
            None => 0,
        }
    }

    /// True if either side has a non-zero shortfall.
    pub fn has_shortfall(&self) -> bool {
        self.shortfall_a() > 0 || self.shortfall_b() > 0
    }

    /// True if either side's availability could not be determined.
    pub fn has_unknown(&self) -> bool {
        self.clip_a_available.is_none() || self.clip_b_available.is_none()
    }
}

/// Compute handle availability for a transition given its two clips and
/// the media pool.
///
/// The caller is responsible for resolving `transition.clip_a_id` and
/// `transition.clip_b_id` to actual `&Clip` references. This function
/// does not take a `Sequence` because it's easier to test with fake clips
/// directly.
///
/// If a clip is not a media clip (e.g., title, generator, gap), its
/// availability is reported as `None`. If a clip is a media clip but its
/// media is not in the pool (e.g., missing/unloaded), availability is
/// also `None`.
pub fn compute_handle_availability(
    transition: &Transition,
    clip_a: &Clip,
    clip_b: &Clip,
    media_pool: &MediaPool,
) -> HandleAvailability {
    let trans_rate = transition.duration.rate;

    // Compute needed frames per side, matching the engine's integer math.
    // Center uses duration.value / 2 (integer floor), exactly as the engine
    // does at klipp-engine/src/transition.rs:83.
    let (clip_a_needed, clip_b_needed) = match transition.alignment {
        TransitionAlignment::Center => {
            let half = transition.duration.value / 2;
            (half, half)
        }
        TransitionAlignment::StartAtCut => (0, transition.duration.value),
        TransitionAlignment::EndAtCut => (transition.duration.value, 0),
    };

    // Compute available handle frames for clip A's tail.
    let clip_a_available = compute_tail_handle(clip_a, media_pool, trans_rate);

    // Compute available handle frames for clip B's head.
    let clip_b_available = compute_head_handle(clip_b, media_pool, trans_rate);

    HandleAvailability {
        clip_a_needed,
        clip_a_available,
        clip_b_needed,
        clip_b_available,
    }
}

/// Tail handle: frames available in the source file past `source_out`,
/// expressed in `target_rate`.
fn compute_tail_handle(clip: &Clip, pool: &MediaPool, target_rate: u32) -> Option<i64> {
    let media_id = clip.media_id()?;
    let item = pool.get(&media_id)?;
    // Compute the difference in the source's native rate first, then rescale.
    // This avoids compound rounding from rescaling each operand separately.
    let tail = item.duration - clip.source_out;
    let tail_at_rate = tail.rescale_to(target_rate);
    Some(tail_at_rate.value.max(0))
}

/// Convenience: resolve clips from a sequence and compute handle availability.
///
/// Returns `None` if either clip can't be found (data bug in the sequence).
pub fn compute_for_transition(
    transition: &Transition,
    sequence: &klipp_timeline::Sequence,
    media_pool: &MediaPool,
) -> Option<HandleAvailability> {
    let (ti_a, ci_a) = sequence.find_clip(transition.clip_a_id)?;
    let (ti_b, ci_b) = sequence.find_clip(transition.clip_b_id)?;
    let clip_a = &sequence.tracks[ti_a].clips[ci_a];
    let clip_b = &sequence.tracks[ti_b].clips[ci_b];
    Some(compute_handle_availability(transition, clip_a, clip_b, media_pool))
}

/// Head handle: frames available before `source_in` in the source file,
/// expressed in `target_rate`.
fn compute_head_handle(clip: &Clip, pool: &MediaPool, target_rate: u32) -> Option<i64> {
    let media_id = clip.media_id()?;
    let _item = pool.get(&media_id)?; // verify media exists
    // source_in IS the head handle (distance from frame 0 to the in-point).
    let head_at_rate = clip.source_in.rescale_to(target_rate);
    Some(head_at_rate.value.max(0))
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, MediaId, RationalTime, TransitionId};

    // ── Test helpers ─────────────────────────────────────────────────

    fn tc(value: i64, rate: u32) -> RationalTime {
        RationalTime { value, rate }
    }

    fn make_media_item(duration_frames: i64, rate: u32) -> klipp_media::MediaItem {
        klipp_media::MediaItem {
            id: MediaId::new(),
            path: std::path::PathBuf::from("/test/media.mp4"),
            kind: klipp_media::MediaItemKind::File,
            filename: "media.mp4".into(),
            duration: tc(duration_frames, rate),
            fps: klipp_core::Fps::new(rate, 1),
            resolution: klipp_core::Resolution { width: 1920, height: 1080 },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: false,
            file_size_bytes: 0,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            added_at: 0,
            fingerprint: None,
            cache_key: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: Default::default(),
            colour_space: Default::default(),
            log_format: Default::default(),
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: Default::default(),
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            parent_media_id: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            stream_ends: Default::default(),
            dolby_vision_profile: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    fn make_media_clip(
        media_id: MediaId,
        source_in_frames: i64,
        source_out_frames: i64,
        rate: u32,
    ) -> Clip {
        Clip::media(
            media_id,
            tc(0, rate),                      // timeline_in (doesn't matter for handle math)
            tc(source_out_frames - source_in_frames, rate), // timeline_out
            tc(source_in_frames, rate),        // source_in
            tc(source_out_frames, rate),       // source_out
        )
    }

    fn make_gap_clip(rate: u32) -> Clip {
        Clip::gap(tc(0, rate), tc(24, rate))
    }

    fn make_transition(
        duration_frames: i64,
        rate: u32,
        alignment: TransitionAlignment,
    ) -> Transition {
        Transition {
            id: TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(duration_frames, rate),
            clip_a_id: ClipId::new(),
            clip_b_id: ClipId::new(),
            user_params: Default::default(),
            alignment,
        }
    }

    fn make_pool_with_id(id: MediaId, item: klipp_media::MediaItem) -> MediaPool {
        let mut pool = MediaPool::new();
        // We need to set the item's id to match what the clip references
        let mut item = item;
        item.id = id;
        pool.add(item);
        pool
    }

    // ── Center alignment tests ───────────────────────────────────────

    #[test]
    fn center_ample_handles_no_shortfall() {
        // Media: 240 frames @ 24fps. Clip uses frames 60..180 (120 frames).
        // Tail handle: 240 - 180 = 60 frames. Head handle: 60 frames.
        // 24-frame Center transition needs 12 per side. Ample.
        let media_id = MediaId::new();
        let item = make_media_item(240, 24);
        let pool = make_pool_with_id(media_id, item);
        let clip_a = make_media_clip(media_id, 60, 180, 24);
        let clip_b = make_media_clip(media_id, 60, 180, 24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 12);
        assert_eq!(result.clip_b_needed, 12);
        assert_eq!(result.clip_a_available, Some(60));
        assert_eq!(result.clip_b_available, Some(60));
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 0);
        assert!(!result.has_shortfall());
    }

    #[test]
    fn center_exact_handles_no_shortfall() {
        // Clip A: source 0..228 in 240-frame media → tail = 12.
        // Clip B: source 12..240 → head = 12.
        // 24-frame Center → needs 12 each. Exact fit.
        let mid_a = MediaId::new();
        let mid_b = MediaId::new();
        let mut item_a = make_media_item(240, 24);
        item_a.id = mid_a;
        let mut item_b = make_media_item(240, 24);
        item_b.id = mid_b;
        let mut pool = MediaPool::new();
        pool.add(item_a);
        pool.add(item_b);
        let clip_a = make_media_clip(mid_a, 0, 228, 24);
        let clip_b = make_media_clip(mid_b, 12, 240, 24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_available, Some(12));
        assert_eq!(result.clip_b_available, Some(12));
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 0);
    }

    #[test]
    fn center_one_frame_short_on_a() {
        // Clip A: tail handle = 11, needed = 12.
        let mid_a = MediaId::new();
        let mid_b = MediaId::new();
        let mut item_a = make_media_item(240, 24);
        item_a.id = mid_a;
        let mut item_b = make_media_item(240, 24);
        item_b.id = mid_b;
        let mut pool = MediaPool::new();
        pool.add(item_a);
        pool.add(item_b);
        let clip_a = make_media_clip(mid_a, 0, 229, 24); // tail = 240-229 = 11
        let clip_b = make_media_clip(mid_b, 12, 240, 24); // head = 12
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 1);
        assert_eq!(result.shortfall_b(), 0);
        assert!(result.has_shortfall());
    }

    #[test]
    fn center_one_frame_short_on_b() {
        let mid_a = MediaId::new();
        let mid_b = MediaId::new();
        let mut item_a = make_media_item(240, 24);
        item_a.id = mid_a;
        let mut item_b = make_media_item(240, 24);
        item_b.id = mid_b;
        let mut pool = MediaPool::new();
        pool.add(item_a);
        pool.add(item_b);
        let clip_a = make_media_clip(mid_a, 0, 228, 24); // tail = 12
        let clip_b = make_media_clip(mid_b, 11, 240, 24); // head = 11
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 1);
    }

    #[test]
    fn center_short_on_both_sides() {
        let mid_a = MediaId::new();
        let mid_b = MediaId::new();
        let mut item_a = make_media_item(240, 24);
        item_a.id = mid_a;
        let mut item_b = make_media_item(240, 24);
        item_b.id = mid_b;
        let mut pool = MediaPool::new();
        pool.add(item_a);
        pool.add(item_b);
        let clip_a = make_media_clip(mid_a, 0, 232, 24); // tail = 8
        let clip_b = make_media_clip(mid_b, 8, 240, 24);  // head = 8
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 4); // needs 12, has 8
        assert_eq!(result.shortfall_b(), 4);
    }

    #[test]
    fn center_severely_short() {
        // Clip A: source_out == media.duration → tail = 0.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24); // tail = 0
        let clip_b = make_media_clip(mid, 0, 240, 24); // head = 0
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 12);
        assert_eq!(result.shortfall_b(), 12);
    }

    #[test]
    fn center_odd_frame_duration_floors() {
        // 25-frame Center transition: half = 25/2 = 12 (integer floor).
        // Clip A tail = 12 → no shortfall. Clip A tail = 11 → shortfall 1.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);

        // With 12 frames of tail handle
        let clip_a_ok = make_media_clip(mid, 0, 228, 24);   // tail = 12
        let clip_b = make_media_clip(mid, 60, 240, 24);     // head = 60
        let trans = make_transition(25, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a_ok, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 12); // 25/2 = 12, NOT 13
        assert_eq!(result.shortfall_a(), 0);

        // With 11 frames of tail handle
        let clip_a_short = make_media_clip(mid, 0, 229, 24); // tail = 11
        let result2 = compute_handle_availability(&trans, &clip_a_short, &clip_b, &pool);
        assert_eq!(result2.shortfall_a(), 1);
    }

    // ── StartAtCut alignment tests ───────────────────────────────────

    #[test]
    fn start_at_cut_ample_b_handle() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        // A's tail = 0, but StartAtCut doesn't need A.
        let clip_a = make_media_clip(mid, 0, 240, 24);
        let clip_b = make_media_clip(mid, 60, 240, 24); // head = 60
        let trans = make_transition(24, 24, TransitionAlignment::StartAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 0);
        assert_eq!(result.clip_b_needed, 24);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 0);
    }

    #[test]
    fn start_at_cut_exact_b_handle() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24);
        let clip_b = make_media_clip(mid, 24, 240, 24); // head = 24
        let trans = make_transition(24, 24, TransitionAlignment::StartAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_b(), 0);
    }

    #[test]
    fn start_at_cut_b_short() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24);
        let clip_b = make_media_clip(mid, 20, 240, 24); // head = 20
        let trans = make_transition(24, 24, TransitionAlignment::StartAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 4); // needs 24, has 20
    }

    #[test]
    fn start_at_cut_a_handle_irrelevant() {
        // A has 0 tail handle — irrelevant for StartAtCut.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24); // tail = 0
        let clip_b = make_media_clip(mid, 60, 240, 24); // head = 60
        let trans = make_transition(24, 24, TransitionAlignment::StartAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 0);
        assert_eq!(result.shortfall_a(), 0); // zero needed → zero shortfall
    }

    // ── EndAtCut alignment tests ─────────────────────────────────────

    #[test]
    fn end_at_cut_ample_a_handle() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 180, 24); // tail = 60
        let clip_b = make_media_clip(mid, 0, 240, 24); // head = 0
        let trans = make_transition(24, 24, TransitionAlignment::EndAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 24);
        assert_eq!(result.clip_b_needed, 0);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 0); // zero needed → zero shortfall
    }

    #[test]
    fn end_at_cut_a_short() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 220, 24); // tail = 20
        let clip_b = make_media_clip(mid, 0, 240, 24);
        let trans = make_transition(24, 24, TransitionAlignment::EndAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 4); // needs 24, has 20
        assert_eq!(result.shortfall_b(), 0);
    }

    #[test]
    fn end_at_cut_b_handle_irrelevant() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 180, 24); // tail = 60
        let clip_b = make_media_clip(mid, 0, 240, 24); // head = 0
        let trans = make_transition(24, 24, TransitionAlignment::EndAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_b_needed, 0);
        assert_eq!(result.shortfall_b(), 0);
    }

    // ── Unknown / non-media cases ────────────────────────────────────

    #[test]
    fn gap_clip_has_unknown_availability() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_gap_clip(24); // not a media clip
        let clip_b = make_media_clip(mid, 60, 240, 24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_available, None);
        assert_eq!(result.shortfall_a(), 0); // unknown → no shortfall
        assert!(result.has_unknown());
    }

    #[test]
    fn missing_media_has_unknown_availability() {
        let mid = MediaId::new();
        let pool = MediaPool::new(); // empty pool
        let clip_a = make_media_clip(mid, 0, 180, 24);
        let clip_b = make_media_clip(mid, 60, 240, 24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_available, None);
        assert_eq!(result.clip_b_available, None);
        assert!(result.has_unknown());
        assert!(!result.has_shortfall());
    }

    #[test]
    fn both_clips_unknown() {
        let pool = MediaPool::new();
        let clip_a = make_gap_clip(24);
        let clip_b = make_gap_clip(24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert!(result.has_unknown());
        assert!(!result.has_shortfall());
    }

    #[test]
    fn one_media_one_gap_center() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 180, 24); // tail = 60
        let clip_b = make_gap_clip(24);
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.clip_b_available, None);
        assert!(result.has_unknown());
    }

    // ── Rate mismatch tests ──────────────────────────────────────────

    #[test]
    fn media_30fps_transition_24fps() {
        // Media: 60 frames @ 30fps = 2 seconds of tail handle.
        // Transition: 24 frames @ 24fps → Center → needs 12 frames @ 24fps = 0.5s.
        // 60 frames @ 30fps = 2s >> 0.5s. No shortfall.
        let mid = MediaId::new();
        let mut item = make_media_item(120, 30); // 4s media
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        // Clip uses frames 0..60 @ 30fps → tail = 120-60 = 60 frames @ 30fps
        let clip_a = make_media_clip(mid, 0, 60, 30);
        let clip_b = make_media_clip(mid, 60, 120, 30);
        // 24fps transition, 24 frames → 12 per side needed
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 12);
        // 60 frames @ 30fps → rescale to 24fps: (60 * 24 / 30).round() = 48
        assert_eq!(result.clip_a_available, Some(48));
        assert_eq!(result.shortfall_a(), 0);
    }

    #[test]
    fn media_24fps_transition_30fps_exact() {
        // Media: 12 frames @ 24fps = 0.5s tail handle.
        // Transition: 30 frames @ 30fps → Center → needs 15 frames @ 30fps = 0.5s.
        // 12 frames @ 24fps rescaled to 30fps: (12 * 30 / 24).round() = 15.
        // Exact match.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 228, 24); // tail = 12 @ 24fps
        let clip_b = make_media_clip(mid, 12, 240, 24); // head = 12 @ 24fps
        let trans = make_transition(30, 30, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 15);
        assert_eq!(result.clip_a_available, Some(15)); // 12@24 → 15@30
        assert_eq!(result.shortfall_a(), 0);
    }

    #[test]
    fn media_24fps_transition_30fps_one_short() {
        // Media: 11 frames @ 24fps tail handle.
        // Rescale to 30fps: (11 * 30 / 24).round() = 13.75.round() = 14.
        // Transition needs 15 per side. Shortfall = 15 - 14 = 1.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 229, 24); // tail = 11 @ 24fps
        let clip_b = make_media_clip(mid, 60, 240, 24);
        let trans = make_transition(30, 30, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 15);
        // 11 frames@24fps → 30fps: (11*30/24).round() = 14
        assert_eq!(result.clip_a_available, Some(14));
        assert_eq!(result.shortfall_a(), 1);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn transition_longer_than_clip_total() {
        // Clip A is 10 frames total, source_out at the end → tail = 0.
        // EndAtCut transition needs 24 frames from A's tail.
        let mid = MediaId::new();
        let mut item = make_media_item(10, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 10, 24); // tail = 0
        let clip_b = make_media_clip(mid, 0, 10, 24);
        let trans = make_transition(24, 24, TransitionAlignment::EndAtCut);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 24); // needs 24, has 0
    }

    #[test]
    fn zero_duration_transition() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24); // tail = 0
        let clip_b = make_media_clip(mid, 0, 240, 24); // head = 0
        let trans = make_transition(0, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.clip_a_needed, 0);
        assert_eq!(result.clip_b_needed, 0);
        assert!(!result.has_shortfall());
    }

    #[test]
    fn center_duration_equals_double_handle_exactly() {
        // Clip A tail = 12, transition = 24 Center → needs 12. Exact.
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 228, 24); // tail = 12
        let clip_b = make_media_clip(mid, 12, 240, 24); // head = 12
        let trans = make_transition(24, 24, TransitionAlignment::Center);

        let result = compute_handle_availability(&trans, &clip_a, &clip_b, &pool);
        assert_eq!(result.shortfall_a(), 0);
        assert_eq!(result.shortfall_b(), 0);
    }

    #[test]
    fn shortfall_methods_consistency() {
        // Verify shortfall_a/b agree with has_shortfall
        let h = HandleAvailability {
            clip_a_needed: 12,
            clip_a_available: Some(10),
            clip_b_needed: 12,
            clip_b_available: Some(15),
        };
        assert_eq!(h.shortfall_a(), 2);
        assert_eq!(h.shortfall_b(), 0);
        assert!(h.has_shortfall());
        assert!(!h.has_unknown());
    }

    #[test]
    fn unknown_available_means_no_shortfall() {
        let h = HandleAvailability {
            clip_a_needed: 100,
            clip_a_available: None,
            clip_b_needed: 100,
            clip_b_available: None,
        };
        assert_eq!(h.shortfall_a(), 0);
        assert_eq!(h.shortfall_b(), 0);
        assert!(!h.has_shortfall());
        assert!(h.has_unknown());
    }

    // ── compute_for_transition wrapper tests ──────────────────────────

    fn make_sequence_with_clips(clips: Vec<Clip>, transitions: Vec<Transition>) -> klipp_timeline::Sequence {
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = clips;
        seq.tracks[0].transitions = transitions;
        seq
    }

    #[test]
    fn for_transition_resolves_clips_and_computes() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 180, 24); // tail = 60
        let clip_b = make_media_clip(mid, 60, 240, 24); // head = 60
        let mut trans = make_transition(24, 24, TransitionAlignment::Center);
        trans.clip_a_id = clip_a.id;
        trans.clip_b_id = clip_b.id;
        let seq = make_sequence_with_clips(vec![clip_a, clip_b], vec![trans]);

        let result = compute_for_transition(&trans, &seq, &pool);
        assert!(result.is_some());
        let ha = result.unwrap();
        assert!(!ha.has_shortfall());
    }

    #[test]
    fn for_transition_with_shortfall() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_media_clip(mid, 0, 240, 24); // tail = 0
        let clip_b = make_media_clip(mid, 0, 240, 24); // head = 0
        let mut trans = make_transition(24, 24, TransitionAlignment::Center);
        trans.clip_a_id = clip_a.id;
        trans.clip_b_id = clip_b.id;
        let seq = make_sequence_with_clips(vec![clip_a, clip_b], vec![trans]);

        let ha = compute_for_transition(&trans, &seq, &pool).unwrap();
        assert!(ha.has_shortfall());
        assert_eq!(ha.shortfall_a(), 12);
        assert_eq!(ha.shortfall_b(), 12);
    }

    #[test]
    fn for_transition_missing_clip_a() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_b = make_media_clip(mid, 60, 240, 24);
        let mut trans = make_transition(24, 24, TransitionAlignment::Center);
        trans.clip_a_id = ClipId::new(); // doesn't exist in sequence
        trans.clip_b_id = clip_b.id;
        let seq = make_sequence_with_clips(vec![clip_b], vec![trans]);

        assert!(compute_for_transition(&trans, &seq, &pool).is_none());
    }

    #[test]
    fn for_transition_gap_clip_unknown() {
        let mid = MediaId::new();
        let mut item = make_media_item(240, 24);
        item.id = mid;
        let mut pool = MediaPool::new();
        pool.add(item);
        let clip_a = make_gap_clip(24);
        let clip_b = make_media_clip(mid, 60, 240, 24);
        let mut trans = make_transition(24, 24, TransitionAlignment::Center);
        trans.clip_a_id = clip_a.id;
        trans.clip_b_id = clip_b.id;
        let seq = make_sequence_with_clips(vec![clip_a, clip_b], vec![trans]);

        let ha = compute_for_transition(&trans, &seq, &pool).unwrap();
        assert!(ha.has_unknown());
        assert!(!ha.has_shortfall());
    }
}
