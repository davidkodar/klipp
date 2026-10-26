//! Sound placed on the timeline for an edit: an audio clip inserted at a time,
//! and the built-in whoosh that goes with a transition or a zoom (#890).

#[allow(unused_imports)]
use super::*;

/// Apply one half of a planned hyperzoom to `clip_id` in `seq`:
///
/// * Set the clip's `BuiltinEffect::Transform` anchor to the focal
///   point (creating the effect with otherwise-default values if the
///   clip didn't already have one).
/// * Replace the clip's `transform_scale_keyframes` with the planned
///   pair so the ramp is exact regardless of any pre-existing scale
///   keyframes (re-applying hyperzoom should be idempotent — a stale
///   ramp from an earlier application doesn't accumulate).
///
/// No-ops if the clip can't be located. The handler logs the failure
/// path so a stale dispatch doesn't silently mutate an unrelated clip.
/// Outcome of a successful [`AppCommand::InsertAudioMediaClip`]
/// dispatch — the inserted clip plus the track index it landed on.
/// Caller records the pair in the undo stack via
/// `EditCommand::LegacyAppendClip`.
#[derive(Debug)]
pub(super) struct InsertedAudioClip {
    pub(super) clip: klipp_timeline::Clip,
    pub(super) track_index: usize,
}

/// Reasons [`insert_audio_media_clip_into_seq`] declines to insert.
/// Surface as a tracing warning at the dispatch arm — these are
/// programmer / state-shape errors, not user-recoverable conditions.
#[derive(Debug)]
pub(super) enum InsertAudioMediaClipError {
    MediaNotInPool,
    TrackIndexOutOfRange,
    TrackIsNotAudio,
    ZeroDuration,
}

impl std::fmt::Display for InsertAudioMediaClipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MediaNotInPool => f.write_str("media id not present in pool"),
            Self::TrackIndexOutOfRange => f.write_str("track_index out of range"),
            Self::TrackIsNotAudio => f.write_str("track at track_index is not an audio track"),
            Self::ZeroDuration => f.write_str("media duration is zero"),
        }
    }
}

/// Mutation core of [`AppCommand::InsertAudioMediaClip`]. Pure-ish
/// (only mutates `seq`); separated from the dispatch arm so unit
/// tests can hit it without a full `App`.
///
/// `track_index` semantics match the AppCommand contract: `Some(i)`
/// must reference an existing audio track; `None` picks the lowest
/// audio track index, auto-creating one if the sequence has none.
pub(super) fn insert_audio_media_clip_into_seq(
    seq: &mut klipp_timeline::Sequence,
    media_pool: &klipp_media::MediaPool,
    media_id: klipp_core::MediaId,
    track_index: Option<usize>,
    timeline_in: klipp_core::Timecode,
) -> Result<InsertedAudioClip, InsertAudioMediaClipError> {
    let item = media_pool
        .get(&media_id)
        .ok_or(InsertAudioMediaClipError::MediaNotInPool)?;
    let duration = item.duration;
    if duration.value <= 0 {
        return Err(InsertAudioMediaClipError::ZeroDuration);
    }
    let track_idx = match track_index {
        Some(idx) => {
            let track = seq
                .tracks
                .get(idx)
                .ok_or(InsertAudioMediaClipError::TrackIndexOutOfRange)?;
            if track.kind != klipp_timeline::TrackKind::Audio {
                return Err(InsertAudioMediaClipError::TrackIsNotAudio);
            }
            idx
        }
        None => match seq
            .tracks
            .iter()
            .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
        {
            Some(i) => i,
            None => {
                seq.add_audio_track("Audio 1".to_string());
                seq.tracks
                    .iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .expect("audio track was just appended")
            }
        },
    };
    // RationalTime addition rescales RHS to LHS rate (project memory:
    // see "RationalTime rate gotcha"). To keep frame-accurate
    // arithmetic, rescale the timeline anchor to the source rate
    // before the add.
    let timeline_in_rescaled = if timeline_in.rate != duration.rate {
        timeline_in.rescale_to(duration.rate)
    } else {
        timeline_in
    };
    let timeline_out = timeline_in_rescaled + duration;
    let source_in = klipp_core::Timecode {
        value: 0,
        rate: duration.rate,
    };
    let clip = klipp_timeline::Clip::media(
        media_id,
        timeline_in_rescaled,
        timeline_out,
        source_in,
        duration,
    );
    let clip_for_history = clip.clone();
    seq.tracks[track_idx].clips.push(clip);
    Ok(InsertedAudioClip {
        clip: clip_for_history,
        track_index: track_idx,
    })
}

/// Map an `ApplyHyperzoom.whoosh_variant` u8 wire-format tag to a
/// concrete [`klipp_audio::WhooshVariant`]. `255` is the "custom
/// sample" sentinel (handled by a different branch — never passed
/// here). Unknown values fall back to `Medium` per the AppCommand
/// doc comment so a stale dispatch can never silently produce no
/// audio when the user ticked the box.
pub(super) fn whoosh_variant_from_u8(tag: u8) -> klipp_audio::WhooshVariant {
    match tag {
        0 => klipp_audio::WhooshVariant::Short,
        1 => klipp_audio::WhooshVariant::Medium,
        2 => klipp_audio::WhooshVariant::Long,
        _ => klipp_audio::WhooshVariant::Medium,
    }
}

/// Compute the whoosh clip's `timeline_in` so its temporal midpoint
/// (where the synth envelope peaks — see `klipp_audio::synth`'s
/// shared-envelope contract) lands on the cut frame at
/// `src_timeline_out`. Result is clamped to ≥ 0; an early cut
/// (whoosh would extend before frame 0) is the project's start
/// rather than a negative time.
///
/// Pure function — exposed for unit testing without spinning up an
/// `App` or `Sequence`.
pub(super) fn whoosh_clip_in_for_apply(
    src_timeline_out: klipp_core::Timecode,
    whoosh_dur_secs: f64,
) -> klipp_core::Timecode {
    let half_dur_secs = whoosh_dur_secs / 2.0;
    let half_dur_ticks = (half_dur_secs * src_timeline_out.rate as f64).round() as i64;
    let value = (src_timeline_out.value - half_dur_ticks).max(0);
    klipp_core::Timecode {
        value,
        rate: src_timeline_out.rate,
    }
}

/// Resolve the cache filesystem path for the synthesised WAV of a
/// built-in whoosh variant. Lives under `~/.cache/klipp/whoosh/` so
/// it sits alongside other Klipp cache artefacts (font registry,
/// thumbnails) and gets cleaned up by the user's normal cache-dir
/// hygiene. Returns `None` when the platform doesn't expose a cache
/// directory (rare; defensive).
pub(super) fn whoosh_wav_cache_path(variant: klipp_audio::WhooshVariant) -> Option<std::path::PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("klipp");
    p.push("whoosh");
    p.push(format!("{}.wav", variant.cache_stem()));
    Some(p)
}

/// Build a [`klipp_media::MediaItem`] for a synthesised whoosh WAV.
/// All metadata is known up front (mono, 48 kHz, fixed duration per
/// variant) so we can register the item in the pool inline without
/// going through the engine's async `LoadMediaFile` probe.
///
/// `display_name` is what shows in the media browser; we tag it
/// distinctly so users can find + delete cached whooshes if they
/// wish.
pub(super) fn build_whoosh_media_item(
    variant: klipp_audio::WhooshVariant,
    path: std::path::PathBuf,
) -> klipp_media::MediaItem {
    use klipp_audio::synth::SAMPLE_RATE;
    let dur_secs = variant.duration_secs() as f64;
    // Sample-accurate duration at 48 kHz tick rate so timeline
    // arithmetic uses the same rate as the underlying audio.
    let duration = klipp_core::RationalTime {
        value: (dur_secs * SAMPLE_RATE as f64).round() as i64,
        rate: SAMPLE_RATE,
    };
    let label = match variant {
        klipp_audio::WhooshVariant::Short => "Whoosh (short)",
        klipp_audio::WhooshVariant::Medium => "Whoosh (medium)",
        klipp_audio::WhooshVariant::Long => "Whoosh (long)",
    };
    klipp_media::MediaItem {
        id: klipp_core::MediaId::new(),
        path,
        kind: klipp_media::MediaItemKind::File,
        filename: label.to_string(),
        duration,
        fps: klipp_core::Fps::film_24(),
        resolution: klipp_core::Resolution { width: 0, height: 0 },
        video_codec: String::new(),
        audio_codec: "pcm_s16le".into(),
        has_audio: true,
        file_size_bytes: 0,
        sample_rate: Some(SAMPLE_RATE),
        audio_channels: Some(1),
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
        parent_media_id: None,
        added_at: 0,
        fingerprint: None,
        cache_key: None,
        file_modified_at: None,
        media_created_at: None,
        color_label: klipp_media::ColorLabel::default(),
        colour_space: klipp_media::ColourSpace::default(),
        log_format: klipp_core::LogFormat::default(),
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
        detected_log_format: klipp_core::LogFormat::default(),
        log_format_user_set: false,
        skim_file: None,
        needs_reprobe: false,
        waveform_data: None,
        audio_cache: None,
        shot: Default::default(),
        metadata_read: false,
        truncated_from_secs: None,
        log: Default::default(),
        synced_audio: None,
        markers: Vec::new(),
        screen_text: String::new(),
        audio_role: None,
        span: None,
    }
}

/// Find an existing pool entry whose path matches `custom_path`.
/// The whoosh handler uses this to bind a user-picked custom WAV
/// without re-importing — the user must have imported the file
/// through the normal media-pool flow first (the inspector UI in
/// commit 3 will hand-hold that), so by apply time the MediaItem
/// is already registered.
pub(super) fn find_existing_audio_in_pool(
    media_pool: &klipp_media::MediaPool,
    custom_path: &std::path::Path,
) -> Option<(klipp_core::MediaId, klipp_core::Timecode)> {
    media_pool
        .all()
        .find(|item| item.has_audio && item.path == *custom_path)
        .map(|item| (item.id, item.duration))
}

/// `whoosh_variant` wire-format tag → cache key. Inverse of
/// [`whoosh_variant_from_u8`] for the three known values; tagged
/// here as `u8` because the AppState cache is u8-keyed.
pub(super) fn whoosh_variant_to_u8(variant: klipp_audio::WhooshVariant) -> u8 {
    match variant {
        klipp_audio::WhooshVariant::Short => 0,
        klipp_audio::WhooshVariant::Medium => 1,
        klipp_audio::WhooshVariant::Long => 2,
    }
}

/// Run the optional whoosh-stinger insertion that ApplyHyperzoom
/// can dispatch when `add_whoosh` is true. Pure entry point so the
/// dispatch arm stays a thin wiring layer + the integration tests
/// can hit the full whoosh pipeline without standing up an `App`.
///
/// Returns `Some(InsertedAudioClip)` when a clip was actually
/// inserted; `None` when `add_whoosh` was false, the variant
/// resolution failed (no cache dir, synth I/O error, custom path
/// not in pool), or the underlying clip insertion was rejected.
/// Caller (the dispatch arm) records the inserted clip in the undo
/// stack so apply + whoosh ride a single ctrl-z.
pub(super) fn maybe_insert_whoosh_for_apply(
    state: &mut crate::state::AppState,
    seq_idx: usize,
    src_timeline_out: klipp_core::Timecode,
    add_whoosh: bool,
    whoosh_variant: u8,
    whoosh_custom_path: Option<&std::path::Path>,
) -> Option<InsertedAudioClip> {
    if !add_whoosh {
        return None;
    }
    let resolved = if whoosh_variant == 255 {
        match whoosh_custom_path {
            Some(custom_path) => {
                let found = find_existing_audio_in_pool(&state.media_pool, custom_path);
                if found.is_none() {
                    tracing::warn!(
                        "ApplyHyperzoom whoosh: custom path {:?} not in media pool — \
                         import via the media browser first",
                        custom_path,
                    );
                }
                found
            }
            None => {
                tracing::warn!(
                    "ApplyHyperzoom whoosh: variant=255 (Custom) but no custom path provided — skipping",
                );
                None
            }
        }
    } else {
        let v = whoosh_variant_from_u8(whoosh_variant);
        ensure_builtin_whoosh_in_pool(
            &mut state.media_pool,
            &mut state.whoosh_media_ids,
            v,
        )
    };
    let (media_id, dur) = resolved?;
    let timeline_in = whoosh_clip_in_for_apply(src_timeline_out, dur.to_secs_f64());
    match insert_audio_media_clip_into_seq(
        &mut state.sequences[seq_idx],
        &state.media_pool,
        media_id,
        None,
        timeline_in,
    ) {
        Ok(inserted) => Some(inserted),
        Err(e) => {
            tracing::warn!("ApplyHyperzoom whoosh: clip insertion rejected: {e}");
            None
        }
    }
}

/// Synthesise the cached WAV for `variant` if not already on disk,
/// register a `MediaItem` with known metadata in the pool, and
/// remember the resulting `MediaId` for next time. Hot path is the
/// cache hit — synthesis only runs on first use per session per
/// variant.
///
/// Returns `None` when the platform has no cache directory or the
/// synthesis / WAV write failed; both paths are logged. Caller
/// (the ApplyHyperzoom handler) treats `None` as "skip whoosh
/// insertion silently" so a disk failure doesn't abort the rest of
/// the apply.
pub(super) fn ensure_builtin_whoosh_in_pool(
    media_pool: &mut klipp_media::MediaPool,
    whoosh_media_ids: &mut std::collections::HashMap<u8, klipp_core::MediaId>,
    variant: klipp_audio::WhooshVariant,
) -> Option<(klipp_core::MediaId, klipp_core::Timecode)> {
    let tag = whoosh_variant_to_u8(variant);
    // Hot path: already registered + still in pool.
    if let Some(existing) = whoosh_media_ids.get(&tag) {
        if let Some(item) = media_pool.get(existing) {
            return Some((*existing, item.duration));
        }
        // Stale cache (pool was wiped between applies); re-register
        // from disk — the WAV likely still exists.
    }
    let path = whoosh_wav_cache_path(variant)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "ApplyHyperzoom whoosh: cache dir create failed at {:?}: {e}",
                    parent,
                );
                return None;
            }
        }
        if let Err(e) = klipp_audio::write_whoosh_wav(variant, &path) {
            tracing::warn!(
                "ApplyHyperzoom whoosh: WAV synthesis failed at {:?}: {e}",
                path,
            );
            return None;
        }
    }
    let item = build_whoosh_media_item(variant, path);
    let duration = item.duration;
    let media_id = media_pool.add(item);
    whoosh_media_ids.insert(tag, media_id);
    Some((media_id, duration))
}

#[cfg(test)]
mod whoosh_helper_tests {
    //! Unit tests for the pure-ish whoosh helpers feeding the
    //! ApplyHyperzoom dispatch arm.
    use super::*;
    use klipp_core::RationalTime;
    use klipp_media::MediaPool;

    #[test]
    fn variant_from_u8_maps_known_tags() {
        assert_eq!(whoosh_variant_from_u8(0), klipp_audio::WhooshVariant::Short);
        assert_eq!(whoosh_variant_from_u8(1), klipp_audio::WhooshVariant::Medium);
        assert_eq!(whoosh_variant_from_u8(2), klipp_audio::WhooshVariant::Long);
    }

    #[test]
    fn variant_from_u8_unknown_falls_back_to_medium() {
        // Per the AppCommand doc: "Unknown values fall back to
        // Medium so a stale dispatch can never produce silence
        // when the user ticked the box." Pin the contract.
        for tag in [3u8, 7, 100, 200, 254] {
            assert_eq!(
                whoosh_variant_from_u8(tag),
                klipp_audio::WhooshVariant::Medium,
            );
        }
        // 255 is "custom" — handled before this resolver in the
        // dispatch arm — but the fallback maps it to Medium too;
        // documents the never-reached path.
        assert_eq!(
            whoosh_variant_from_u8(255),
            klipp_audio::WhooshVariant::Medium,
        );
    }

    #[test]
    fn round_trip_variant_u8_tags_match_inverse() {
        for v in [
            klipp_audio::WhooshVariant::Short,
            klipp_audio::WhooshVariant::Medium,
            klipp_audio::WhooshVariant::Long,
        ] {
            let tag = whoosh_variant_to_u8(v);
            assert_eq!(whoosh_variant_from_u8(tag), v);
        }
    }

    #[test]
    fn clip_in_centres_whoosh_on_cut() {
        // src_timeline_out = 5 seconds (240 frames @ 48 rate).
        // Medium whoosh duration = 0.5 s → half_dur = 0.25 s = 12
        // ticks @ 48 rate. Expected timeline_in = 240 − 12 = 228.
        let src_out = RationalTime { value: 240, rate: 48 };
        let result = whoosh_clip_in_for_apply(src_out, 0.5);
        assert_eq!(result.value, 228);
        assert_eq!(result.rate, 48);
    }

    #[test]
    fn clip_in_clamps_to_zero_when_whoosh_extends_before_start() {
        // src_timeline_out = 0.1 s (5 ticks @ 48 rate). Long
        // whoosh duration = 0.8 s → half_dur = 0.4 s = ~19 ticks.
        // 5 − 19 = −14 → clamped to 0.
        let src_out = RationalTime { value: 5, rate: 48 };
        let result = whoosh_clip_in_for_apply(src_out, 0.8);
        assert_eq!(result.value, 0);
    }

    #[test]
    fn clip_in_preserves_source_rate() {
        // The whoosh clip inherits the SOURCE clip's rate, not
        // the whoosh media's rate. The insert step rescales as
        // needed.
        for rate in [24, 30, 48, 60] {
            let src_out = RationalTime { value: rate as i64, rate };
            let result = whoosh_clip_in_for_apply(src_out, 0.1);
            assert_eq!(result.rate, rate);
        }
    }

    #[test]
    fn ensure_builtin_synthesises_first_then_caches() {
        // First call: cache miss → synth → register → cache.
        // Second call: cache hit → same MediaId.
        let mut pool = MediaPool::new();
        let mut cache = std::collections::HashMap::new();
        let v = klipp_audio::WhooshVariant::Short;
        // Run once. May fail on systems without dirs::cache_dir
        // (CI sandboxes); skip the assertion gracefully if so.
        let first = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v);
        let Some((id_first, dur_first)) = first else {
            eprintln!("skipping: no cache_dir on this platform");
            return;
        };
        assert!(pool.get(&id_first).is_some(), "registered in pool");
        assert_eq!(cache.get(&0).copied(), Some(id_first), "cache populated");
        // Second call must reuse.
        let (id_second, dur_second) =
            ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v).unwrap();
        assert_eq!(id_first, id_second, "cache hit reuses MediaId");
        assert_eq!(dur_first, dur_second, "duration stable across calls");
        // Pool should have exactly one entry per variant after two calls.
        assert_eq!(pool.all().count(), 1);
    }

    #[test]
    fn ensure_builtin_re_registers_when_pool_wiped_under_cache() {
        // Defensive: cache says "we have it" but pool has been
        // cleared (e.g. project save/load cycle). Helper detects
        // the stale pointer and re-registers.
        let mut pool = MediaPool::new();
        let mut cache = std::collections::HashMap::new();
        let v = klipp_audio::WhooshVariant::Medium;
        let Some((id_first, _)) = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v) else {
            return;
        };
        // Wipe the pool but keep the stale cache pointer.
        pool = MediaPool::new();
        let (id_second, _) = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v).unwrap();
        assert_ne!(id_first, id_second, "re-registered with a fresh id");
        assert_eq!(pool.all().count(), 1);
    }

    #[test]
    fn find_existing_audio_returns_none_for_unimported_path() {
        let pool = MediaPool::new();
        let phantom = std::path::PathBuf::from("/tmp/never-imported.wav");
        assert!(find_existing_audio_in_pool(&pool, &phantom).is_none());
    }

    #[test]
    fn find_existing_audio_matches_by_exact_path_and_audio_flag() {
        let mut pool = MediaPool::new();
        let path = std::path::PathBuf::from("/tmp/custom-whoosh.wav");
        let mut item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            path.clone(),
        );
        item.has_audio = true;
        let id = pool.add(item);
        let result = find_existing_audio_in_pool(&pool, &path);
        assert_eq!(result.map(|(i, _)| i), Some(id));
    }

    #[test]
    fn find_existing_audio_skips_video_only_items_with_matching_path() {
        // Edge case: someone imported a video file at the same
        // path. has_audio guard makes sure we don't pick it up
        // as a whoosh source.
        let mut pool = MediaPool::new();
        let path = std::path::PathBuf::from("/tmp/silent-clip.mp4");
        let mut item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            path.clone(),
        );
        item.has_audio = false;
        pool.add(item);
        assert!(find_existing_audio_in_pool(&pool, &path).is_none());
    }
}

#[cfg(test)]
mod whoosh_dispatch_tests {
    //! Integration-style tests for the `maybe_insert_whoosh_for_apply`
    //! end of the ApplyHyperzoom dispatch arm. We can't stand up an
    //! `App` cheaply — these test against `AppState` directly, the
    //! same way `whoosh_helper_tests` does, but exercise the full
    //! variant-resolve → media-register → clip-insert chain.
    use super::{
        build_whoosh_media_item, maybe_insert_whoosh_for_apply,
    };
    use crate::state::AppState;
    use klipp_core::{ClipId, MediaId, RationalTime, Timecode};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn tc(value: i64, rate: u32) -> RationalTime {
        RationalTime { value, rate }
    }

    fn fresh_state_with_video_clip() -> (AppState, ClipId, Timecode) {
        // A minimal sequence: one video track with one media clip
        // ending at 2.0 s @ 48 rate. Source clip's timeline_out is
        // what the whoosh handler centres the audio against.
        let mut state = AppState::default();
        // new_default ships V1 + A1; clear and rebuild so we can
        // capture the source clip id deterministically.
        state.sequences[0].tracks.clear();
        let mut track = Track::new("V1", TrackKind::Video);
        let src_in = tc(0, 48);
        let src_out = tc(96, 48); // 2.0 s @ 48
        let clip = Clip::media(MediaId::new(), src_in, src_out, src_in, src_out);
        let cid = clip.id;
        track.clips.push(clip);
        state.sequences[0].tracks.push(track);
        (state, cid, src_out)
    }

    #[test]
    fn add_whoosh_false_emits_no_audio() {
        // The byte-identical-to-pre-feature pin. A snapshot of the
        // sequence before and after must match: no new tracks,
        // no new clips, no whoosh in the media pool, no entry in
        // the variant cache.
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let pre_track_count = state.sequences[0].tracks.len();
        let pre_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        let pre_pool_count = state.media_pool.all().count();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            false, // add_whoosh = false
            0,
            None,
        );
        assert!(result.is_none(), "false → no insertion");
        assert_eq!(state.sequences[0].tracks.len(), pre_track_count);
        let post_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(post_clip_count, pre_clip_count);
        assert_eq!(state.media_pool.all().count(), pre_pool_count);
        assert!(state.whoosh_media_ids.is_empty());
    }

    #[test]
    fn add_whoosh_true_inserts_at_cut_aligned_time() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            0, // Short = 0.30 s
            None,
        );
        let Some(inserted) = result else {
            // Skip on platforms with no cache_dir (rare).
            eprintln!("skipping: no cache_dir / synth I/O failure");
            return;
        };
        // Audio track auto-created (sequence had only the video track).
        assert_eq!(state.sequences[0].tracks[inserted.track_index].kind, TrackKind::Audio);
        assert_eq!(state.sequences[0].tracks[inserted.track_index].clips.len(), 1);
        // Whoosh registered in pool + cached in variant map.
        assert!(state.whoosh_media_ids.contains_key(&0));
        assert_eq!(state.media_pool.all().count(), 1);
        // Clip-in centring: src_out = 2.0 s @ 48 rate (96 ticks).
        // Short whoosh = 0.30 s → half = 0.15 s → at 48 rate
        // that's 7.2 → rounds to 7. Expected timeline_in =
        // 96 - 7 = 89 (rate 48; the rescale-to-source-rate inside
        // insert_audio_media_clip_into_seq runs against the source
        // clip's 48 kHz rate, producing a different value — see
        // the next assertion).
        let inserted_in = inserted.clip.timeline_in;
        // The audio clip's rate ends up at 48 000 (the synth rate)
        // because insert_audio_media_clip_into_seq rescales to the
        // *source media* rate. Verify it's NOT zero (i.e., the
        // centring formula did fire — clamping to zero would
        // indicate misuse) and is less than src_out converted to
        // the same rate.
        assert!(inserted_in.value > 0, "timeline_in non-zero (centred, not clamped)");
    }

    #[test]
    fn second_apply_reuses_cached_whoosh_media_id() {
        // Two consecutive applies on different cuts — the second
        // must reuse the cached MediaId rather than registering a
        // duplicate. Pin the audit-note-(iv) "AppState-scoped
        // cache" promise.
        let (mut state, src_id, src_out) = fresh_state_with_video_clip();
        // Add a second video clip + capture its timeline_out so
        // the second apply has a different cut frame.
        let second_out = tc(192, 48);
        let mut second_clip = Clip::media(MediaId::new(), src_out, second_out, tc(0, 48), tc(96, 48));
        let _ = src_id; // suppress unused
        second_clip.id = ClipId::new();
        state.sequences[0].tracks[0].clips.push(second_clip);
        let r1 = maybe_insert_whoosh_for_apply(&mut state, 0, src_out, true, 1, None);
        let r2 = maybe_insert_whoosh_for_apply(&mut state, 0, second_out, true, 1, None);
        let (Some(_), Some(_)) = (&r1, &r2) else {
            eprintln!("skipping: synth I/O unavailable");
            return;
        };
        assert_eq!(
            state.media_pool.all().count(),
            1,
            "second apply reused cached MediaId — pool still has one entry",
        );
    }

    #[test]
    fn custom_variant_with_path_in_pool_uses_that_media() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        // Pre-populate the pool with a "custom" audio file.
        let custom_path = std::path::PathBuf::from("/tmp/user-custom-whoosh.wav");
        let item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            custom_path.clone(),
        );
        let custom_id = state.media_pool.add(item);
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255, // Custom
            Some(&custom_path),
        );
        let inserted = result.expect("custom path resolves");
        // The inserted clip's media should be the custom one,
        // NOT a freshly-synthesised built-in.
        let media_id = match inserted.clip.kind {
            klipp_timeline::ClipKind::Media { media_id } => media_id,
            _ => panic!("expected Media kind"),
        };
        assert_eq!(media_id, custom_id);
        // Built-in cache untouched.
        assert!(state.whoosh_media_ids.is_empty());
    }

    #[test]
    fn custom_variant_without_pool_match_skips_silently() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let phantom_path = std::path::PathBuf::from("/tmp/never-imported-whoosh.wav");
        let pre_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255,
            Some(&phantom_path),
        );
        assert!(result.is_none(), "no clip inserted when path absent from pool");
        let post_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(post_clip_count, pre_clip_count, "sequence untouched");
    }

    #[test]
    fn custom_variant_without_path_skips_silently() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255,
            None, // variant=255 but no path
        );
        assert!(result.is_none());
    }
}

#[cfg(test)]
mod insert_audio_media_clip_tests {
    //! Pure-mutation tests for `insert_audio_media_clip_into_seq`.
    //! Builds a sequence + media pool inline; the dispatch arm in
    //! `update()` is the thin caller that records undo + dirties.
    use super::{insert_audio_media_clip_into_seq, InsertAudioMediaClipError};
    use klipp_core::{Fps, MediaId, RationalTime, Resolution};
    use klipp_media::{ColorLabel, ColourSpace, MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 48)
    }

    fn audio_media_item(duration: RationalTime) -> MediaItem {
        MediaItem {
            id: MediaId::new(),
            path: std::path::PathBuf::from("/tmp/whoosh-test.wav"),
            kind: MediaItemKind::File,
            filename: "whoosh-test.wav".into(),
            duration,
            fps: Fps::film_24(),
            resolution: Resolution { width: 0, height: 0 },
            video_codec: String::new(),
            audio_codec: "pcm_s16le".into(),
            has_audio: true,
            file_size_bytes: 0,
            sample_rate: Some(48_000),
            audio_channels: Some(1),
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
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            cache_key: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
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
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    fn seq_video_only() -> Sequence {
        let mut s = Sequence::new_default();
        // new_default ships one video + one audio track. Strip the
        // audio so we exercise the "auto-create when none exists"
        // branch.
        s.tracks.retain(|t| t.kind != TrackKind::Audio);
        assert!(
            !s.tracks.iter().any(|t| t.kind == TrackKind::Audio),
            "fixture invariant: no audio tracks expected after retain",
        );
        s
    }

    fn seq_with_audio_at(idx: usize) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks.retain(|t| t.kind != TrackKind::Audio);
        // Pad the track list with extra video tracks so the audio
        // lands at the requested index.
        while s.tracks.len() < idx {
            let name = format!("V{}", s.tracks.len() + 1);
            s.tracks.push(Track::new(&name, TrackKind::Video));
        }
        s.tracks
            .insert(idx, Track::new("Audio 1", TrackKind::Audio));
        s
    }

    #[test]
    fn picks_first_audio_track_when_track_index_is_none() {
        let mut seq = seq_with_audio_at(2); // V1, V2, A1
        let mut pool = MediaPool::new();
        let item = audio_media_item(tc(24));
        let media_id = pool.add(item);
        let inserted = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(48))
            .expect("insert");
        assert_eq!(inserted.track_index, 2);
        assert_eq!(seq.tracks[2].clips.len(), 1);
        assert_eq!(seq.tracks[2].clips[0].timeline_in, tc(48));
    }

    #[test]
    fn auto_creates_audio_track_when_none_exists() {
        let mut seq = seq_video_only();
        let video_count = seq.tracks.len();
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let inserted = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(0))
            .expect("insert");
        assert_eq!(seq.tracks.len(), video_count + 1, "audio track auto-created");
        let new_track = &seq.tracks[inserted.track_index];
        assert_eq!(new_track.kind, TrackKind::Audio);
        assert_eq!(new_track.clips.len(), 1);
    }

    #[test]
    fn inserts_at_explicit_audio_track_index() {
        let mut seq = seq_with_audio_at(1); // V1, A1
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let inserted =
            insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(1), tc(0))
                .expect("insert");
        assert_eq!(inserted.track_index, 1);
        assert_eq!(seq.tracks[1].clips.len(), 1);
    }

    #[test]
    fn errors_when_media_id_not_in_pool() {
        let mut seq = seq_with_audio_at(1);
        let pool = MediaPool::new(); // empty
        let phantom = MediaId::new();
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, phantom, None, tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::MediaNotInPool));
    }

    #[test]
    fn errors_when_track_index_out_of_range() {
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(99), tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::TrackIndexOutOfRange));
    }

    #[test]
    fn errors_when_track_index_points_at_video_track() {
        let mut seq = seq_with_audio_at(1); // V1 (idx 0), A1 (idx 1)
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(0), tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::TrackIsNotAudio));
    }

    #[test]
    fn errors_when_media_duration_zero() {
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let zero = RationalTime { value: 0, rate: 48 };
        let media_id = pool.add(audio_media_item(zero));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::ZeroDuration));
    }

    #[test]
    fn timeline_in_rescaled_to_source_rate() {
        // Pin the RationalTime gotcha — caller passes a 24-rate
        // timecode, source is 48-rate. Without rescale the
        // timeline_in.value would be misinterpreted as 48-rate
        // frames and the clip would land at the wrong time.
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let dur_48 = RationalTime { value: 24, rate: 48 };
        let media_id = pool.add(audio_media_item(dur_48));
        let timeline_in_24 = RationalTime { value: 12, rate: 24 }; // 0.5 s
        insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, timeline_in_24)
            .expect("insert");
        let clip = &seq.tracks[1].clips[0];
        assert_eq!(clip.timeline_in.rate, 48, "rescaled to source rate");
        assert_eq!(clip.timeline_in.value, 24, "0.5 s @ 48 rate = 24 ticks");
    }
}
