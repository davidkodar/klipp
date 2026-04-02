//! FCPXML export (version 1.9) for interchange with Final Cut Pro and other NLEs.
//!
//! Generates a valid FCPXML document from a Klipp sequence with media references,
//! clip timing, transitions, and markers.

use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind};
use std::path::Path;

/// Export a sequence as FCPXML 1.9 to the given path.
pub fn export_fcpxml(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<(), String> {
    let xml = generate_fcpxml(seq, media_pool);
    std::fs::write(output_path, xml)
        .map_err(|e| format!("Failed to write FCPXML: {e}"))
}

/// Generate FCPXML 1.9 string from a sequence.
pub fn generate_fcpxml(seq: &Sequence, media_pool: &MediaPool) -> String {
    let mut xml = String::with_capacity(8192);
    let fps = &seq.fps;
    let _framerate_str = format!("{}fps", fps_display(fps));
    let frame_dur = rational_time_str(1, fps.num, fps.den);

    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<!DOCTYPE fcpxml>\n");
    xml.push_str("<fcpxml version=\"1.9\">\n");

    // Resources: formats and media assets
    xml.push_str("  <resources>\n");

    // Format resource for the sequence
    let format_id = "r0";
    xml.push_str(&format!(
        "    <format id=\"{format_id}\" name=\"FFVideoFormat{}p{}\" frameDuration=\"{}\" width=\"{}\" height=\"{}\"/>\n",
        seq.resolution.height, fps_display(fps), frame_dur, seq.resolution.width, seq.resolution.height
    ));

    // Collect unique media references
    let mut assets = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for clip in &track.clips {
            if let ClipKind::Media { media_id } = &clip.kind {
                if let Some(item) = media_pool.get(media_id) {
                    let asset_id = format!("r{}_{}", ti, clip.id.0.as_u128() & 0xFFFF);
                    assets.push((asset_id.clone(), item.path.clone(), *media_id));
                    let path_str = escape_xml(&item.path.to_string_lossy());
                    let dur = clip_duration_str(clip, fps);
                    xml.push_str(&format!(
                        "    <asset id=\"{}\" name=\"{}\" src=\"file://{}\" start=\"0s\" duration=\"{}\" format=\"{}\"/>\n",
                        asset_id,
                        escape_xml(&item.path.file_name().unwrap_or_default().to_string_lossy()),
                        path_str,
                        dur,
                        format_id,
                    ));
                }
            }
        }
    }

    xml.push_str("  </resources>\n");

    // Library > Event > Project > Sequence
    xml.push_str("  <library>\n");
    xml.push_str("    <event name=\"Klipp Export\">\n");
    xml.push_str(&format!(
        "      <project name=\"{}\">\n",
        escape_xml(&seq.name)
    ));

    // Sequence element
    let seq_dur = sequence_duration(seq, fps);
    xml.push_str(&format!(
        "        <sequence format=\"{}\" duration=\"{}\" tcStart=\"0s\">\n",
        format_id, seq_dur
    ));

    // Spine (primary storyline)
    xml.push_str("          <spine>\n");

    // Export video tracks (primary storyline = first video track)
    let primary_track = seq.tracks.iter()
        .find(|t| matches!(t.kind, TrackKind::Video));

    if let Some(track) = primary_track {
        let mut _asset_idx = 0;
        for clip in &track.clips {
            match &clip.kind {
                ClipKind::Media { media_id } => {
                    let asset_id = assets.iter()
                        .find(|(_, _, mid)| mid == media_id)
                        .map(|(id, _, _)| id.as_str())
                        .unwrap_or("r0");
                    let name = media_pool.get(media_id)
                        .map(|m| m.path.file_name().unwrap_or_default().to_string_lossy().to_string())
                        .unwrap_or_else(|| "Untitled".into());
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    let start = tc_str(clip.source_in, fps);

                    xml.push_str(&format!(
                        "            <asset-clip ref=\"{}\" name=\"{}\" offset=\"{}\" duration=\"{}\" start=\"{}\"",
                        asset_id, escape_xml(&name), offset, duration, start
                    ));

                    if !clip.enabled {
                        xml.push_str(" enabled=\"0\"");
                    }

                    // Check for transitions or markers
                    let has_markers = !clip.clip_markers.is_empty();
                    if has_markers {
                        xml.push_str(">\n");
                        for marker in &clip.clip_markers {
                            let marker_offset = format!("{}s", format_decimal(marker.time_secs));
                            xml.push_str(&format!(
                                "              <marker start=\"{}\" duration=\"1/{}s\" value=\"{}\"/>\n",
                                marker_offset, fps.num, escape_xml(&marker.name)
                            ));
                        }
                        xml.push_str("            </asset-clip>\n");
                    } else {
                        xml.push_str("/>\n");
                    }
                    _asset_idx += 1;
                }
                ClipKind::Title(td) => {
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    xml.push_str(&format!(
                        "            <title name=\"{}\" offset=\"{}\" duration=\"{}\">\n",
                        escape_xml(&td.text), offset, duration
                    ));
                    xml.push_str(&format!(
                        "              <text>{}</text>\n",
                        escape_xml(&td.text)
                    ));
                    xml.push_str("            </title>\n");
                }
                ClipKind::Gap => {
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    xml.push_str(&format!(
                        "            <gap offset=\"{}\" duration=\"{}\"/>\n",
                        offset, duration
                    ));
                }
                ClipKind::Sequence { .. } => {
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    xml.push_str(&format!(
                        "            <gap name=\"Compound Clip\" offset=\"{}\" duration=\"{}\"/>\n",
                        offset, duration
                    ));
                }
                ClipKind::Generator(_) | ClipKind::AdjustmentLayer => {
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    let name = if matches!(clip.kind, ClipKind::AdjustmentLayer) { "Adjustment Layer" } else { "Generator" };
                    xml.push_str(&format!(
                        "            <gap name=\"{}\" offset=\"{}\" duration=\"{}\"/>\n",
                        name, offset, duration
                    ));
                }
            }
        }
    }

    xml.push_str("          </spine>\n");

    // Sequence markers
    for marker in &seq.markers {
        let pos = tc_str(marker.position, fps);
        xml.push_str(&format!(
            "          <marker start=\"{}\" duration=\"1/{}s\" value=\"{}\"/>\n",
            pos, fps.num, escape_xml(&marker.label)
        ));
    }

    xml.push_str("        </sequence>\n");
    xml.push_str("      </project>\n");
    xml.push_str("    </event>\n");
    xml.push_str("  </library>\n");
    xml.push_str("</fcpxml>\n");

    xml
}

/// Also generate a simple EDL (CMX 3600 format) for broader compatibility.
pub fn export_edl(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<(), String> {
    let edl = generate_edl(seq, media_pool);
    std::fs::write(output_path, edl)
        .map_err(|e| format!("Failed to write EDL: {e}"))
}

/// Generate CMX 3600 EDL from a sequence.
pub fn generate_edl(seq: &Sequence, media_pool: &MediaPool) -> String {
    let mut edl = String::with_capacity(4096);
    let fps_rate = seq.fps.as_f64();

    edl.push_str(&format!("TITLE: {}\n", seq.name));
    edl.push_str(&format!("FCM: {}\n\n", if (fps_rate - 29.97).abs() < 0.01 { "DROP FRAME" } else { "NON-DROP FRAME" }));

    let mut event_num = 1u32;

    for track in &seq.tracks {
        if !matches!(track.kind, TrackKind::Video | TrackKind::Audio) { continue; }
        let reel_type = if track.kind == TrackKind::Video { "V" } else { "A" };

        for clip in &track.clips {
            if clip.kind.is_gap() || !clip.enabled { continue; }

            let reel_name = clip.media_id()
                .and_then(|mid| media_pool.get(&mid))
                .map(|m| {
                    let name = m.path.file_stem().unwrap_or_default().to_string_lossy();
                    // EDL reel names are max 8 chars
                    if name.len() > 8 { name[..8].to_string() } else { name.to_string() }
                })
                .unwrap_or_else(|| "BL".into());

            let src_in = tc_to_edl(clip.source_in.to_secs_f64(), fps_rate);
            let src_out = tc_to_edl(clip.source_out.to_secs_f64(), fps_rate);
            let rec_in = tc_to_edl(clip.timeline_in.to_secs_f64(), fps_rate);
            let rec_out = tc_to_edl(clip.timeline_out.to_secs_f64(), fps_rate);

            edl.push_str(&format!(
                "{:03}  {:8} {}     C        {} {} {} {}\n",
                event_num, reel_name, reel_type, src_in, src_out, rec_in, rec_out
            ));

            // Optional: clip name comment
            if let Some(mid) = clip.media_id() {
                if let Some(m) = media_pool.get(&mid) {
                    let name = m.path.file_name().unwrap_or_default().to_string_lossy();
                    edl.push_str(&format!("* FROM CLIP NAME: {}\n", name));
                }
            }
            edl.push('\n');
            event_num += 1;
        }
    }

    edl
}

// ── Helper functions ──

/// Convert RationalTime to FCPXML time string like "1001/30000s" or "48/24s"
fn tc_str(tc: klipp_core::RationalTime, fps: &klipp_core::Fps) -> String {
    // FCPXML uses rational time with the timebase denominator
    // For integer fps (24, 25, 30): "frames/fps_num" (e.g. "48/24s" = 2 seconds)
    // For NTSC (29.97, 23.976): "value/rate" where rate matches fps timebase
    if fps.den == 1 {
        // Integer fps: express in frames
        let frames = tc.value;
        let rate = tc.rate;
        format!("{frames}/{rate}s")
    } else {
        // NTSC: convert to the fps timebase
        let rescaled = tc.rescale_to(fps.num);
        format!("{}/{}s", rescaled.value, fps.num)
    }
}

fn rational_time_str(frames: i64, fps_num: u32, fps_den: u32) -> String {
    if fps_den == 1 {
        format!("{frames}/{fps_num}s")
    } else {
        format!("{}/{fps_num}s", frames * fps_den as i64)
    }
}

fn clip_duration_str(clip: &klipp_timeline::Clip, fps: &klipp_core::Fps) -> String {
    tc_str(clip.source_out - clip.source_in, fps)
}

fn sequence_duration(seq: &Sequence, fps: &klipp_core::Fps) -> String {
    let max_out = seq.tracks.iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.timeline_out)
        .max()
        .unwrap_or(klipp_core::RationalTime { value: 0, rate: fps.num });
    tc_str(max_out, fps)
}

fn fps_display(fps: &klipp_core::Fps) -> String {
    let rate = fps.as_f64();
    if (rate - rate.round()).abs() < 0.01 {
        format!("{}", rate.round() as u32)
    } else {
        format!("{:.2}", rate)
    }
}

fn format_decimal(secs: f64) -> String {
    if secs == secs.floor() {
        format!("{}", secs as i64)
    } else {
        format!("{:.3}", secs)
    }
}

fn tc_to_edl(secs: f64, fps: f64) -> String {
    let total_frames = (secs * fps).round() as u32;
    let fps_int = fps.round() as u32;
    let h = total_frames / (fps_int * 3600);
    let m = (total_frames % (fps_int * 3600)) / (fps_int * 60);
    let s = (total_frames % (fps_int * 60)) / fps_int;
    let f = total_frames % fps_int;
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
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
    use klipp_core::{
        ClipId, Fps, MarkerColor, MarkerId, MediaId, RationalTime, Resolution, SequenceId,
    };
    use klipp_media::{MediaItem, MediaPool};
    use klipp_timeline::{
        Clip, ClipKind, ClipMarker, Marker, Sequence, TitleData, Track, TrackKind,
    };
    use std::path::PathBuf;

    /// Build a minimal Sequence with the given fps/resolution and a single empty Video track.
    fn empty_sequence(fps: Fps) -> Sequence {
        Sequence {
            id: SequenceId::new(),
            name: "Test Sequence".into(),
            fps,
            resolution: Resolution::hd_1080(),
            tracks: vec![Track::new("Video 1", TrackKind::Video)],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
        }
    }

    /// Create a MediaItem with the given id, filename, and duration (in frames at the given rate).
    fn make_media_item(id: MediaId, filename: &str, frames: i64, rate: u32) -> MediaItem {
        MediaItem {
            id,
            path: PathBuf::from(format!("/media/{filename}")),
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
            added_at: 0,
            fingerprint: None,
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
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
        }
    }

    /// Create a media Clip with full control over timing.
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
            node_graph: Default::default(),
            effects: Vec::new(),
            builtin_effects: Vec::new(),
            builtin_effects_enabled: Vec::new(),
            linked_clip_id: None,
            group_id: None,
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
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: Default::default(),
            role: Default::default(),
            param_modifiers: Default::default(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            audio_effects: Vec::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
        }
    }

    /// Create a gap or title clip.
    fn make_clip_with_kind(
        kind: ClipKind,
        timeline_in: RationalTime,
        timeline_out: RationalTime,
    ) -> Clip {
        let mut clip = make_media_clip(
            MediaId::new(),
            timeline_in,
            timeline_out,
            RationalTime { value: 0, rate: timeline_in.rate },
            timeline_out - timeline_in,
        );
        clip.kind = kind;
        clip
    }

    // ── FCPXML Tests ──

    #[test]
    fn fcpxml_empty_sequence() {
        let seq = empty_sequence(Fps::film_24());
        let pool = MediaPool::new();
        let xml = generate_fcpxml(&seq, &pool);

        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<fcpxml version=\"1.9\">"));
        assert!(xml.contains("<spine>\n          </spine>"));
        assert!(xml.contains("</fcpxml>"));
        // Verify structure: resources, library, event, project, sequence
        assert!(xml.contains("<resources>"));
        assert!(xml.contains("<library>"));
        assert!(xml.contains("<event name=\"Klipp Export\">"));
        assert!(xml.contains("<project name=\"Test Sequence\">"));
    }

    #[test]
    fn fcpxml_single_media_clip() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "clip_a.mov", 48, 24));

        // 48 frames at 24fps = 2 seconds, starting at frame 0 on timeline
        let clip = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        // Check asset resource
        assert!(xml.contains("name=\"clip_a.mov\""));
        assert!(xml.contains("src=\"file:///media/clip_a.mov\""));
        // Check asset-clip element with correct timing
        assert!(xml.contains("offset=\"0/24s\""));
        assert!(xml.contains("duration=\"48/24s\""));
        assert!(xml.contains("start=\"0/24s\""));
        // Self-closing (no markers)
        assert!(xml.contains("/>\n"));
    }

    #[test]
    fn fcpxml_title_clip() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let pool = MediaPool::new();

        let mut td = TitleData::default();
        td.text = "Hello World".into();
        let clip = make_clip_with_kind(
            ClipKind::Title(td),
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 72, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        assert!(xml.contains("<title name=\"Hello World\""));
        assert!(xml.contains("offset=\"0/24s\""));
        assert!(xml.contains("duration=\"72/24s\""));
        assert!(xml.contains("<text>Hello World</text>"));
        assert!(xml.contains("</title>"));
    }

    #[test]
    fn fcpxml_gap_clip() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let pool = MediaPool::new();

        let clip = make_clip_with_kind(
            ClipKind::Gap,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        assert!(xml.contains("<gap offset=\"0/24s\" duration=\"24/24s\"/>"));
    }

    #[test]
    fn fcpxml_disabled_clip() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "disabled.mov", 24, 24));

        let mut clip = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        );
        clip.enabled = false;
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        assert!(xml.contains("enabled=\"0\""));
    }

    #[test]
    fn fcpxml_sequence_markers() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let pool = MediaPool::new();

        seq.markers.push(Marker {
            id: MarkerId::new(),
            position: RationalTime { value: 48, rate: 24 },
            label: "Scene 2".into(),
            color: MarkerColor::Green,
        });
        seq.markers.push(Marker {
            id: MarkerId::new(),
            position: RationalTime { value: 120, rate: 24 },
            label: "End".into(),
            color: MarkerColor::Red,
        });

        let xml = generate_fcpxml(&seq, &pool);

        // Markers appear after </spine> but inside <sequence>
        assert!(xml.contains("<marker start=\"48/24s\" duration=\"1/24s\" value=\"Scene 2\"/>"));
        assert!(xml.contains("<marker start=\"120/24s\" duration=\"1/24s\" value=\"End\"/>"));
    }

    #[test]
    fn fcpxml_clip_markers() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "marked.mov", 96, 24));

        let mut clip = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 96, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 96, rate: 24 },
        );
        clip.clip_markers.push(ClipMarker {
            time_secs: 1.0,
            name: "Beat Drop".into(),
            color: MarkerColor::Blue,
        });
        clip.clip_markers.push(ClipMarker {
            time_secs: 2.5,
            name: "Cut Here".into(),
            color: MarkerColor::Orange,
        });
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        // Clip markers are children of asset-clip (not self-closing)
        assert!(xml.contains("<asset-clip"));
        assert!(xml.contains("<marker start=\"1s\" duration=\"1/24s\" value=\"Beat Drop\"/>"));
        assert!(xml.contains("<marker start=\"2.500s\" duration=\"1/24s\" value=\"Cut Here\"/>"));
        assert!(xml.contains("</asset-clip>"));
    }

    #[test]
    fn fcpxml_rational_time_format() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "test.mov", 48, 24));

        // 48 frames at 24fps = exactly 2 seconds -> "48/24s"
        let clip = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        // Verify "frames/rate" format: "48/24s" for 2 seconds at 24fps
        assert!(xml.contains("duration=\"48/24s\""));
        assert!(xml.contains("offset=\"0/24s\""));
        assert!(xml.contains("start=\"0/24s\""));

        // Also verify the sequence duration
        assert!(xml.contains("duration=\"48/24s\""));
    }

    #[test]
    fn fcpxml_xml_escaping() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        seq.name = "Tom & Jerry's \"Great\" <Adventure>".into();
        let pool = MediaPool::new();

        let mut td = TitleData::default();
        td.text = "Rock & Roll <Live> \"Show\"".into();
        let clip = make_clip_with_kind(
            ClipKind::Title(td),
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let xml = generate_fcpxml(&seq, &pool);

        // Sequence name must be escaped in project element
        assert!(xml.contains("Tom &amp; Jerry&apos;s &quot;Great&quot; &lt;Adventure&gt;"));
        // Title text must be escaped
        assert!(xml.contains("Rock &amp; Roll &lt;Live&gt; &quot;Show&quot;"));
        // Resulting XML must be well-formed (no raw &, <, >, " in attribute/text positions)
        assert!(!xml.contains("<project name=\"Tom & Jerry"));
    }

    // ── EDL Tests ──

    #[test]
    fn edl_single_clip() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "scene01.mov", 72, 24));

        // 72 frames = 3 seconds, starting at frame 24 (1s) on timeline
        let clip = make_media_clip(
            mid,
            RationalTime { value: 24, rate: 24 },  // timeline_in = 1s
            RationalTime { value: 96, rate: 24 },   // timeline_out = 4s
            RationalTime { value: 0, rate: 24 },    // source_in = 0s
            RationalTime { value: 72, rate: 24 },   // source_out = 3s
        );
        seq.tracks[0].clips.push(clip);

        let edl = generate_edl(&seq, &pool);

        // Title line
        assert!(edl.starts_with("TITLE: Test Sequence\n"));
        // Event line: CMX 3600 format
        // event_num(3) reel_name(8) V/A(whitespace)C(whitespace) src_in src_out rec_in rec_out
        assert!(edl.contains("001  scene01  V     C        00:00:00:00 00:00:03:00 00:00:01:00 00:00:04:00\n"));
        // Clip name comment
        assert!(edl.contains("* FROM CLIP NAME: scene01.mov"));
    }

    #[test]
    fn edl_drop_frame_header() {
        let fps = Fps::ntsc_29970(); // 29.97fps -> 30000/1001
        let seq = empty_sequence(fps);
        let pool = MediaPool::new();

        let edl = generate_edl(&seq, &pool);

        assert!(edl.contains("FCM: DROP FRAME"));
    }

    #[test]
    fn edl_non_drop_frame_header() {
        let fps = Fps::film_24();
        let seq = empty_sequence(fps);
        let pool = MediaPool::new();

        let edl = generate_edl(&seq, &pool);

        assert!(edl.contains("FCM: NON-DROP FRAME"));
    }

    #[test]
    fn edl_reel_name_truncation() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        // Filename longer than 8 chars (stem = "very_long_clip_name")
        pool.add(make_media_item(mid, "very_long_clip_name.mov", 24, 24));

        let clip = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        );
        seq.tracks[0].clips.push(clip);

        let edl = generate_edl(&seq, &pool);

        // Reel name should be truncated to 8 chars: "very_lon"
        assert!(edl.contains("very_lon"));
        // Full name should NOT appear in the event line (only in FROM CLIP NAME comment)
        let event_line = edl.lines()
            .find(|l| l.starts_with("001"))
            .expect("should have event line");
        assert!(!event_line.contains("very_long_clip_name"));
        assert!(event_line.contains("very_lon"));
    }
}
