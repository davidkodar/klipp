//! MLT XML export (Kdenlive `.kdenlive` project format) — cuts-only v1.
//!
//! Writes a minimal MLT document that Kdenlive (and `melt`) will open. Only
//! clip positions, source ranges, track structure, and media references are
//! serialized. Effects, transitions, keyframes, color grades, nested
//! sequences, multicam, and markers are explicitly DEFERRED — see issue #68.
//!
//! Structure mirrors what Kdenlive writes itself:
//!   - top-level `<profile>`
//!   - one `<producer>` per unique media file (id = `producerN`)
//!   - one `<playlist>` per source track with `<entry>` + `<blank>` children
//!   - one per-track-pair `<tractor>` wrapping each playlist (with a secondary
//!     empty playlist, matching Kdenlive's dual-playlist shape)
//!   - a final root `<tractor>` that lists all per-track-pair tractors. MUST
//!     be the last element in the document — Kdenlive's parser relies on this.
//!
//! Off-by-one: MLT `out` is INCLUSIVE (last valid frame). Klipp's
//! `timeline_out` / `source_out` are EXCLUSIVE. Always subtract 1 at emission.

use klipp_core::Fps;
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind};
use std::path::Path;

/// Export a sequence as MLT XML (`.kdenlive`) to the given path.
///
/// Returns `Err(String)` on IO failure, else what the project does not
/// carry, one line each (#2520), as `fcpxml::export_fcpxml` does.
pub fn export_kdenlive(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<Vec<String>, String> {
    let xml = generate_mlt_xml(seq, media_pool);
    std::fs::write(output_path, xml)
        .map_err(|e| format!("Failed to write Kdenlive project: {e}"))?;
    Ok(kdenlive_not_carried(seq, media_pool))
}

/// What this cuts-only Kdenlive project leaves out of `seq`: every track
/// comes across, clip by clip, and nothing else of them.
pub fn kdenlive_not_carried(seq: &Sequence, media_pool: &MediaPool) -> Vec<String> {
    crate::not_carried::not_carried(seq, media_pool, &crate::not_carried::Format { name: "the Kdenlive project", transitions: false, video_tracks: None, audio_tracks: None, drops_disabled: false })
}

/// Generate the MLT XML document as a String. Never panics.
pub fn generate_mlt_xml(seq: &Sequence, media_pool: &MediaPool) -> String {
    let mut xml = String::with_capacity(8192);
    let fps = &seq.fps;

    // ── Collect unique producers, in insertion order ──────────────────────
    // Each unique MediaId maps to `producer{N}`. Repeated uses of the same
    // source in different clips share a producer.
    let mut producer_order: Vec<klipp_core::MediaId> = Vec::new();
    for track in &seq.tracks {
        for clip in &track.clips {
            if let ClipKind::Media { media_id } = &clip.kind {
                if !producer_order.iter().any(|m| m == media_id) {
                    producer_order.push(*media_id);
                }
            }
        }
    }

    let producer_id_for = |mid: &klipp_core::MediaId| -> Option<String> {
        producer_order
            .iter()
            .position(|m| m == mid)
            .map(|i| format!("producer{i}"))
    };

    // Sequence duration in frames at the sequence rate. Used as `out` on the
    // root tractor and the per-track-pair tractors.
    let seq_frames = sequence_duration_frames(seq, fps);
    // MLT `out` is inclusive; duration 0 → no clips → just emit 0.
    let seq_out_inclusive = if seq_frames == 0 { 0 } else { seq_frames - 1 };

    // ── Header ────────────────────────────────────────────────────────────
    // NOTE: intentionally omit `<!DOCTYPE mlt SYSTEM ...>` — roxmltree (used
    // by the importer) rejects DOCTYPE with "XML with DTD detected". Kdenlive
    // and melt both accept MLT XML without the DOCTYPE declaration; Kdenlive
    // itself does not require it. Keeping the document DOCTYPE-free lets
    // Klipp round-trip its own output.
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str(
        "<mlt title=\"Klipp Export\" version=\"7.0.0\" LC_NUMERIC=\"C\" producer=\"main_bin\">\n",
    );

    // ── Profile ───────────────────────────────────────────────────────────
    let profile_name = profile_name_for(seq.resolution.width, seq.resolution.height, fps);
    xml.push_str(&format!(
        "  <profile description=\"{}\" width=\"{}\" height=\"{}\" progressive=\"1\" \
         sample_aspect_num=\"1\" sample_aspect_den=\"1\" display_aspect_num=\"{}\" \
         display_aspect_den=\"{}\" frame_rate_num=\"{}\" frame_rate_den=\"{}\" colorspace=\"709\"/>\n",
        escape_xml(&profile_name),
        seq.resolution.width,
        seq.resolution.height,
        seq.resolution.width,
        seq.resolution.height,
        fps.num,
        fps.den,
    ));

    // ── Black producer (reserved id, emitted once) ────────────────────────
    xml.push_str("  <producer id=\"black\" in=\"0\" out=\"999999\">\n");
    xml.push_str("    <property name=\"resource\">black</property>\n");
    xml.push_str("    <property name=\"mlt_service\">colour</property>\n");
    xml.push_str("    <property name=\"colour\">0x000000ff</property>\n");
    xml.push_str("    <property name=\"aspect_ratio\">1</property>\n");
    xml.push_str("    <property name=\"length\">999999</property>\n");
    xml.push_str("  </producer>\n");

    // ── Media producers ───────────────────────────────────────────────────
    for (i, mid) in producer_order.iter().enumerate() {
        let pid = format!("producer{i}");
        let (path_str, name_str, max_out) = match media_pool.get(mid) {
            Some(item) => {
                let p = item.path.to_string_lossy().to_string();
                let n = item
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| item.filename.clone());
                // Use media duration if populated; fall back to a safe large
                // number so source ranges are never out-of-bounds.
                let max_frames = item.duration.rescale_to(fps.num).value;
                let max_out = if max_frames > 0 { max_frames - 1 } else { 999_999 };
                (p, n, max_out)
            }
            None => (format!("missing_{i}"), format!("missing_{i}"), 999_999_i64),
        };

        xml.push_str(&format!(
            "  <producer id=\"{}\" in=\"0\" out=\"{}\">\n",
            pid, max_out
        ));
        xml.push_str(&format!(
            "    <property name=\"resource\">{}</property>\n",
            escape_xml(&path_str)
        ));
        xml.push_str("    <property name=\"mlt_service\">avformat</property>\n");
        xml.push_str(&format!(
            "    <property name=\"kdenlive:clipname\">{}</property>\n",
            escape_xml(&name_str)
        ));
        xml.push_str(&format!(
            "    <property name=\"kdenlive:id\">{}</property>\n",
            pid
        ));
        xml.push_str("  </producer>\n");
    }

    // ── Per-track playlists (primary with content) + per-track-pair tractors
    // One tractor per Klipp track. Each tractor wraps two playlists:
    //   - `playlist{N}a`: primary, carries <entry>/<blank>
    //   - `playlist{N}b`: secondary, always empty (matches Kdenlive's dual-
    //     playlist shape the importer expects for transition overlaps)
    let mut track_tractor_ids: Vec<(String, TrackKind)> = Vec::new();

    for (ti, track) in seq.tracks.iter().enumerate() {
        // Only serialize Video + Audio tracks for v1. Text/Subtitle deferred.
        if !matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
            continue;
        }

        let primary_id = format!("playlist{ti}a");
        let secondary_id = format!("playlist{ti}b");
        let tractor_id = format!("tractor{ti}");

        // Emit primary playlist with entries/blanks.
        xml.push_str(&format!("  <playlist id=\"{}\">\n", primary_id));

        // Sort clips by timeline_in so we can emit sequential blanks/entries.
        let mut clips: Vec<&klipp_timeline::Clip> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.timeline_in.rescale_to(fps.num).value);

        let mut cursor_frames: i64 = 0;
        for clip in &clips {
            let in_f = clip.timeline_in.rescale_to(fps.num).value;
            let out_f = clip.timeline_out.rescale_to(fps.num).value;
            if out_f <= in_f {
                continue; // defensive: zero-length clip
            }

            // Leading blank / gap-fill blank.
            if in_f > cursor_frames {
                let gap = in_f - cursor_frames;
                xml.push_str(&format!("    <blank length=\"{}\"/>\n", gap));
            }

            // Clip → entry or blank stub.
            let clip_len_frames = out_f - in_f;
            match &clip.kind {
                ClipKind::Media { media_id } => {
                    let src_in = clip.source_in.rescale_to(fps.num).value.max(0);
                    // MLT out is INCLUSIVE → source_out (exclusive) - 1.
                    let src_out_incl = {
                        let so = clip.source_out.rescale_to(fps.num).value;
                        (so - 1).max(src_in)
                    };
                    match producer_id_for(media_id) {
                        Some(pid) => {
                            xml.push_str(&format!(
                                "    <entry producer=\"{}\" in=\"{}\" out=\"{}\"/>\n",
                                pid, src_in, src_out_incl
                            ));
                        }
                        None => {
                            // Media referenced but absent from pool — fill the
                            // clip's slot with a blank so downstream clips stay
                            // aligned.
                            xml.push_str(&format!(
                                "    <blank length=\"{}\"/>\n",
                                clip_len_frames
                            ));
                        }
                    }
                }
                // TODO: #68-follow-up: serialize title clips, generators, and
                // adjustment layers. For v1 they collapse to blanks of the
                // same duration to keep downstream timing intact.
                ClipKind::Title(_)
                | ClipKind::Generator(_)
                | ClipKind::AdjustmentLayer
                | ClipKind::Sequence { .. }
                | ClipKind::Precomposition { .. }
                | ClipKind::TemplatePrecomposition { .. }
                | ClipKind::Gap => {
                    xml.push_str(&format!(
                        "    <blank length=\"{}\"/>\n",
                        clip_len_frames
                    ));
                }
            }

            cursor_frames = out_f;
        }

        xml.push_str("  </playlist>\n");

        // Secondary empty playlist (Kdenlive's dual-playlist per-track shape).
        xml.push_str(&format!("  <playlist id=\"{}\"/>\n", secondary_id));

        // Per-track-pair tractor. `hide=` goes on the <track> children here,
        // not on the root tractor.
        let hide_attr = match track.kind {
            TrackKind::Video => " hide=\"audio\"",
            TrackKind::Audio => " hide=\"video\"",
            _ => "",
        };
        xml.push_str(&format!(
            "  <tractor id=\"{}\" in=\"0\" out=\"{}\">\n",
            tractor_id, seq_out_inclusive
        ));
        xml.push_str(&format!(
            "    <track producer=\"{}\"{}/>\n",
            primary_id, hide_attr
        ));
        xml.push_str(&format!(
            "    <track producer=\"{}\"{}/>\n",
            secondary_id, hide_attr
        ));
        xml.push_str("  </tractor>\n");

        track_tractor_ids.push((tractor_id, track.kind));
    }

    // ── Root tractor (MUST be last) ───────────────────────────────────────
    xml.push_str(&format!(
        "  <tractor id=\"main_tractor\" in=\"0\" out=\"{}\">\n",
        seq_out_inclusive
    ));
    xml.push_str("    <property name=\"kdenlive:docproperties.version\">1.1</property>\n");
    xml.push_str(&format!(
        "    <property name=\"kdenlive:docproperties.profile\">{}</property>\n",
        escape_xml(&profile_name)
    ));
    // Root tractor lists each per-track-pair tractor as a <track> child,
    // no hide attribute (hide lives on the inner per-track-pair's tracks).
    for (tid, _kind) in &track_tractor_ids {
        xml.push_str(&format!("    <track producer=\"{}\"/>\n", tid));
    }
    xml.push_str("  </tractor>\n");

    xml.push_str("</mlt>\n");
    xml
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Sequence duration in sequence-rate frames: the max timeline_out across
/// all tracks, or 0 for an empty sequence.
fn sequence_duration_frames(seq: &Sequence, fps: &Fps) -> i64 {
    seq.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.timeline_out.rescale_to(fps.num).value)
        .max()
        .unwrap_or(0)
        .max(0)
}

/// Pick a Kdenlive-style profile name. `atsc_1080p_{fps}` for 1920x1080,
/// otherwise `custom_{w}x{h}_{fps}`.
fn profile_name_for(width: u32, height: u32, fps: &Fps) -> String {
    let fps_int = (fps.as_f64().round()) as u32;
    if width == 1920 && height == 1080 {
        format!("atsc_1080p_{fps_int}")
    } else {
        format!("custom_{width}x{height}_{fps_int}")
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, Fps, MediaId, RationalTime, Resolution, SequenceId};
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
    use std::path::PathBuf;

    // ── Fixture builders (mirror fcpxml.rs tests for consistency) ────────

    fn empty_sequence(fps: Fps) -> Sequence {
        Sequence {
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            buses: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: "Test Sequence".into(),
            fps,
            resolution: Resolution::hd_1080(),
            first_clip_autoset: Default::default(),
            tracks: vec![Track::new("V1", TrackKind::Video)],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        }
    }

    fn make_media_item(id: MediaId, filename: &str, frames: i64, rate: u32) -> MediaItem {
        MediaItem {
            id,
            path: PathBuf::from(format!("/media/{filename}")),
            kind: MediaItemKind::File,
            filename: filename.into(),
            duration: RationalTime { value: frames, rate },
            fps: Fps { num: rate, den: 1 },
            resolution: Resolution::hd_1080(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1000,
            sample_rate: Some(48000),
            audio_channels: Some(2),
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
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
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
        timeline_in: RationalTime,
        timeline_out: RationalTime,
        source_in: RationalTime,
        source_out: RationalTime,
    ) -> Clip {
        Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in,
            timeline_out,
            source_in,
            source_out,
            picture: Default::default(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            grade_group: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: Default::default(),
            fade_out_curve: Default::default(),
            channel_config: Default::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: Default::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            hold: None,
            audio_effects: Vec::new(),
            audio_effect_keyframes: Default::default(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: klipp_timeline::GradeFlag::None,
            precomp: None,
            baked_own: None,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn generated_xml_is_valid_utf8() {
        let seq = empty_sequence(Fps::new(24, 1));
        let pool = MediaPool::new();
        let xml = generate_mlt_xml(&seq, &pool);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<mlt "));
        assert!(xml.contains("</mlt>"));
        // String is UTF-8 by construction; explicit roundtrip just in case.
        assert_eq!(std::str::from_utf8(xml.as_bytes()).unwrap(), xml);
    }

    #[test]
    fn root_tractor_is_last_element() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        // Add a second track so there are multiple per-track-pair tractors.
        seq.tracks.push(Track::new("A1", TrackKind::Audio));

        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "clip.mov", 48, 24));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        let root_pos = xml
            .rfind("<tractor id=\"main_tractor\"")
            .expect("main_tractor must exist");

        // Nothing past the root tractor's opening should contain another
        // playlist or non-main tractor.
        let tail = &xml[root_pos..];
        assert!(
            !tail.contains("<playlist "),
            "<playlist> after root tractor: {tail}"
        );
        // Only the main tractor should match — per-track tractors are `tractor0`, `tractor1`, …
        assert!(
            !tail.contains("<tractor id=\"tractor"),
            "per-track tractor after root tractor: {tail}"
        );
        // Producers must also come before.
        assert!(
            !tail.contains("<producer "),
            "<producer> after root tractor: {tail}"
        );
    }

    #[test]
    fn producer_ids_are_unique() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        // Same media used in two clips → one producer only.
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "a.mov", 48, 24));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        ));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);

        // Collect every `<producer id="..."` occurrence via simple scanning.
        let mut ids: Vec<&str> = Vec::new();
        for frag in xml.split("<producer id=\"").skip(1) {
            if let Some(end) = frag.find('"') {
                ids.push(&frag[..end]);
            }
        }
        // Must include at least `black` and `producer0`.
        assert!(ids.contains(&"black"));
        assert!(ids.contains(&"producer0"));
        // No duplicates.
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            ids.len(),
            sorted.len(),
            "duplicate producer ids: {ids:?}"
        );
    }

    #[test]
    fn mlt_out_is_inclusive() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "clip.mov", 100, 24));

        // source_in = 0, source_out = 100 (exclusive) → MLT out = 99.
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 100, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 100, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        assert!(
            xml.contains("<entry producer=\"producer0\" in=\"0\" out=\"99\"/>"),
            "expected inclusive out=99 in: {xml}"
        );
    }

    #[test]
    fn blank_fills_gap_between_clips() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "clip.mov", 100, 24));

        // Clip 1: 0..24  (24 frames)
        // Clip 2: 36..48 (leaves 12-frame gap between them)
        // Leading blank of 0 → not emitted (cursor == 0 == in_f).
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        ));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 36, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 12, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        assert!(
            xml.contains("<blank length=\"12\"/>"),
            "expected gap-fill blank of length 12: {xml}"
        );
    }

    #[test]
    fn leading_blank_when_first_clip_starts_late() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "clip.mov", 100, 24));

        // Clip starts at frame 10 → leading blank of 10.
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 10, rate: 24 },
            RationalTime { value: 30, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 20, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        assert!(
            xml.contains("<blank length=\"10\"/>"),
            "expected leading blank of 10: {xml}"
        );
    }

    #[test]
    fn xml_escapes_special_chars_in_media_paths() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        // Filename contains `&` and `<` — must be escaped in both `resource`
        // and `kdenlive:clipname` property values.
        let mut item = make_media_item(mid, "tricky & <name>.mov", 48, 24);
        item.path = PathBuf::from("/media/path & <special>.mov");
        pool.add(item);

        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        assert!(xml.contains("&amp;"), "& must be escaped: {xml}");
        assert!(xml.contains("&lt;"), "< must be escaped: {xml}");
        assert!(xml.contains("&gt;"), "> must be escaped: {xml}");
        // Raw unescaped characters must not appear in the resource line.
        assert!(
            !xml.contains("path & <special>"),
            "raw unescaped chars leaked: {xml}"
        );
    }

    #[test]
    fn video_track_gets_hide_audio_on_inner_track() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        seq.tracks.push(Track::new("A1", TrackKind::Audio));

        let pool = MediaPool::new();
        let xml = generate_mlt_xml(&seq, &pool);

        // Video track → hide="audio" on its inner <track> elements.
        assert!(xml.contains("<track producer=\"playlist0a\" hide=\"audio\"/>"));
        assert!(xml.contains("<track producer=\"playlist0b\" hide=\"audio\"/>"));
        // Audio track → hide="video".
        assert!(xml.contains("<track producer=\"playlist1a\" hide=\"video\"/>"));
        assert!(xml.contains("<track producer=\"playlist1b\" hide=\"video\"/>"));
    }

    #[test]
    fn round_trip_importer_accepts_our_output() {
        // The strongest integration check: generate XML, write to temp, and
        // ask load_kdenlive to read it back. Verifies structural soundness
        // against the canonical schema.
        use super::super::mlt_import::load_kdenlive;

        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        seq.tracks.push(Track::new("A1", TrackKind::Audio));

        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "rt.mov", 100, 24));

        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        ));

        let xml = generate_mlt_xml(&seq, &pool);
        let tmp = std::env::temp_dir().join(format!(
            "klipp_mlt_export_roundtrip_{}.kdenlive",
            std::process::id()
        ));
        std::fs::write(&tmp, &xml).unwrap();
        let imported = load_kdenlive(&tmp).expect("importer must accept our output");
        // We wrote 2 tracks (V1 + A1). Importer should see at least one.
        assert!(
            !imported.sequence.tracks.is_empty(),
            "importer saw no tracks; xml=\n{xml}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn title_and_gap_clips_become_blanks() {
        let fps = Fps::new(24, 1);
        let mut seq = empty_sequence(fps);
        let pool = MediaPool::new();

        // Gap clip on the timeline — serialized as a blank of equal length.
        let mut gap = make_media_clip(
            MediaId::new(),
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        );
        gap.kind = ClipKind::Gap;
        seq.tracks[0].clips.push(gap);

        let xml = generate_mlt_xml(&seq, &pool);
        // 24-frame gap clip → <blank length="24"/>.
        assert!(
            xml.contains("<blank length=\"24\"/>"),
            "Gap clip must emit blank: {xml}"
        );
    }
}
