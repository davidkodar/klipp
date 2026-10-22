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
                        escape_xml(&td.text()), offset, duration
                    ));
                    xml.push_str(&format!(
                        "              <text>{}</text>\n",
                        escape_xml(&td.text())
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
                ClipKind::Sequence { .. } | ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. } => {
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

    // Sequence markers. Range markers emit their real duration; point
    // markers fall back to the synthetic 1-frame value FCP expects.
    // Comments round-trip via a child <note> element when non-empty
    // (matches FCPXML's marker-comment shape). Marker kind doesn't
    // have a 1:1 FCPXML mapping so it's lost on export today.
    for marker in &seq.markers {
        let pos = tc_str(marker.position, fps);
        let dur_str = match marker.duration {
            Some(d) if d.value > 0 => format!("{}/{}s", d.value, d.rate),
            _ => format!("1/{}s", fps.num),
        };
        if marker.comment.is_empty() {
            xml.push_str(&format!(
                "          <marker start=\"{}\" duration=\"{}\" value=\"{}\"/>\n",
                pos, dur_str, escape_xml(&marker.label)
            ));
        } else {
            xml.push_str(&format!(
                "          <marker start=\"{}\" duration=\"{}\" value=\"{}\">\n",
                pos, dur_str, escape_xml(&marker.label)
            ));
            xml.push_str(&format!(
                "            <note>{}</note>\n",
                escape_xml(&marker.comment)
            ));
            xml.push_str("          </marker>\n");
        }
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
    // Drop-frame exists at 29.97 and 59.94 and nowhere else. The header
    // declares the mode and every timecode below is counted in it —
    // these two facts have to agree, or a conform silently misplaces
    // every event (it used to say DROP FRAME over non-drop numbers,
    // which is 3.6 s an hour of drift at 29.97).
    let drop = klipp_core::sync::is_drop_frame_rate(seq.fps);

    edl.push_str(&format!("TITLE: {}\n", seq.name));
    edl.push_str(&format!("FCM: {}\n\n", if drop { "DROP FRAME" } else { "NON-DROP FRAME" }));

    let mut event_num = 1u32;
    let mut reels = ReelNamer::default();
    let mut audio_channel = 0u32;

    for track in &seq.tracks {
        if !matches!(track.kind, TrackKind::Video | TrackKind::Audio) { continue; }
        // CMX 3600 names the audio channel an event belongs to, so a
        // multi-track conform can be put back together. Audio tracks are
        // numbered in the order they appear; past A4 the format has no
        // agreed spelling, so those fall back to a bare "A".
        let channel = if track.kind == TrackKind::Video {
            "V".to_string()
        } else {
            audio_channel += 1;
            if audio_channel <= 4 { format!("A{audio_channel}") } else { "A".to_string() }
        };

        for clip in &track.clips {
            if clip.kind.is_gap() || !clip.enabled { continue; }

            // A transition whose *incoming* side is this clip. CMX 3600
            // puts the transition on the incoming event — the outgoing
            // clip stays a cut and the one arriving carries the code and
            // the length — so the lookup is by `clip_b_id`.
            let incoming = track.transitions.iter().find(|t| t.clip_b_id == clip.id);
            let (code, effect_name) = match incoming {
                Some(t) => {
                    let frames = (t.duration.to_secs_f64() * seq.fps.as_f64()).round().max(1.0) as i64;
                    (edl_transition_code(t.kind, frames), Some(t.kind))
                }
                None => ("C        ".to_string(), None),
            };

            let item = clip.media_id().and_then(|mid| media_pool.get(&mid));
            let reel = reels.name_for(item);

            let src_in = tc_to_edl(clip.source_in.to_secs_f64(), seq.fps, drop);
            let src_out = tc_to_edl(clip.source_out.to_secs_f64(), seq.fps, drop);
            let rec_in = tc_to_edl(clip.timeline_in.to_secs_f64(), seq.fps, drop);
            let rec_out = tc_to_edl(clip.timeline_out.to_secs_f64(), seq.fps, drop);

            edl.push_str(&format!(
                "{:03}  {:8} {:<2}    {}{} {} {} {}\n",
                event_num, reel.name, channel, code, src_in, src_out, rec_in, rec_out
            ));
            // The kind by name, because the code cannot carry it: a
            // blur dissolve and a film dissolve are both D, and a wipe
            // Klipp has no SMPTE number for is a D that says what it
            // really was. `* EFFECT NAME` is what Resolve and Avid
            // write, so a finishing app recreating the transition has
            // the name it needs.
            if let Some(kind) = effect_name {
                edl.push_str(&format!(
                    "* EFFECT NAME: {}\n",
                    klipp_core::transition_registry::spec_for_kind(kind).display_name.to_uppercase()
                ));
            }
            if reel.disambiguated {
                // Two sources reduced to the same eight characters. The
                // conform needs them apart, so one was renamed — say so,
                // because the reel no longer matches what is on the card.
                edl.push_str(&format!("* REEL {} RENAMED FROM {}\n", reel.name, reel.wanted));
            }

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

/// The CMX 3600 transition field for `kind`, padded to its column.
///
/// The format has three codes that matter: `C` for a cut, `D` for a
/// dissolve with a length in frames, and `W` plus a SMPTE pattern
/// number for a wipe. Klipp has sixty-odd transitions and CMX has
/// numbers for a handful, so the mapping is deliberately small:
///
/// * anything that is a dissolve by nature becomes `D`;
/// * the four cardinal wipes take the two SMPTE numbers everything
///   agrees on — 001 horizontal, 002 vertical;
/// * everything else becomes `D` of the right length.
///
/// That last line is the honest compromise. A page turn is not a
/// dissolve, but a conform needs *something* of the right duration at
/// the right place, and the alternative — writing a cut — loses the
/// transition entirely and moves nothing to tell anyone. The real kind
/// goes out beside it as `* EFFECT NAME`, which is where a finishing
/// app looks anyway.
fn edl_transition_code(kind: klipp_core::TransitionKind, frames: i64) -> String {
    use klipp_core::TransitionKind as K;
    let pattern = match kind {
        K::WipeLeft | K::WipeRight | K::SplitWipeHorizontal | K::VenetianBlindHorizontal => Some("001"),
        K::WipeUp | K::WipeDown | K::SplitWipeVertical | K::VenetianBlindVertical => Some("002"),
        _ => None,
    };
    match pattern {
        Some(p) => format!("W{p} {frames:>3} "),
        None => format!("D    {frames:>3} "),
    }
}

/// The reel an event was given, and whether it is the one that was asked for.
struct Reel {
    name: String,
    /// What the source actually calls itself, before truncation and
    /// collision handling.
    wanted: String,
    disambiguated: bool,
}

/// Hands out CMX 3600 reel names, keeping them unique across the EDL.
///
/// The reel is the EDL's primary key: it is how a conform finds the
/// tape or card an event came from, so two different sources must never
/// share one. Eight characters is the format's limit, which is exactly
/// why collisions happen — `A001C001_240315` and `A001C001_240316` are
/// the same card on different days and reduce to the same eight.
#[derive(Default)]
struct ReelNamer {
    /// Reel name → the media it belongs to.
    taken: std::collections::HashMap<String, klipp_core::MediaId>,
    /// Media → the reel already given to it, so a source cut in twenty
    /// times is one reel and not twenty.
    given: std::collections::HashMap<klipp_core::MediaId, Reel>,
}

impl ReelNamer {
    fn name_for(&mut self, item: Option<&klipp_media::MediaItem>) -> Reel {
        // No media behind the event: BL is the format's own name for
        // black, and is what a gap or a generator conforms to.
        let Some(item) = item else {
            return Reel { name: "BL".into(), wanted: "BL".into(), disambiguated: false };
        };
        if let Some(prev) = self.given.get(&item.id) {
            return Reel { name: prev.name.clone(), wanted: prev.wanted.clone(), disambiguated: false };
        }

        // The editor's logged reel first, then the one the file carried
        // — Sony and Canon XML sidecars and the file's own metadata both
        // land in `ShotMetadata::reel_name`. The filename is the last
        // resort, because it is a guess about what the card was called.
        let wanted = item
            .reel()
            .map(str::to_string)
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| item.path.file_stem().unwrap_or_default().to_string_lossy().into_owned());

        let base = sanitise_reel(&wanted);
        let mut name = base.clone();
        let mut disambiguated = false;
        // Taken by a *different* source: shorten and number, so the two
        // stay distinguishable. Nine tries at seven characters, then two
        // digits; a shoot with a hundred colliding reels has a naming
        // problem the EDL cannot fix.
        if self.taken.get(&name).is_some_and(|owner| *owner != item.id) {
            for n in 2..100u32 {
                let suffix = n.to_string();
                let keep = 8usize.saturating_sub(suffix.len());
                let candidate: String = base.chars().take(keep).chain(suffix.chars()).collect();
                if !self.taken.contains_key(&candidate) {
                    name = candidate;
                    disambiguated = true;
                    break;
                }
            }
        }

        self.taken.insert(name.clone(), item.id);
        let reel = Reel { name: name.clone(), wanted: wanted.clone(), disambiguated };
        self.given.insert(item.id, Reel { name, wanted, disambiguated });
        reel
    }
}

/// A reel name as CMX 3600 wants it: up to eight characters, uppercase,
/// alphanumeric.
///
/// Truncation is by characters, never bytes. `&name[..8]` on a filename
/// like `Café_shoot_01` splits a multi-byte character and panics, which
/// is what this used to do.
///
/// Anything that is not a letter or a digit comes out, because the
/// format is column-based and a space in the reel field shifts every
/// column after it. A name with nothing left becomes AX, the
/// conventional reel for a source that cannot name itself.
pub(crate) fn sanitise_reel(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_uppercase();
    if cleaned.is_empty() { "AX".into() } else { cleaned }
}

/// A timecode for an EDL event, counted in the mode the file's `FCM`
/// line declares.
///
/// Colons throughout, including in drop-frame. CMX 3600 writes
/// `hh:mm:ss:ff` and takes the mode from `FCM`, which is authoritative;
/// a semicolon is how an application *displays* drop-frame, and a
/// strict conform parser is not obliged to accept it in the field. The
/// numbers carry the mode, not the punctuation.
fn tc_to_edl(secs: f64, fps: klipp_core::Fps, drop: bool) -> String {
    let frames = (secs * fps.as_f64()).round() as i64;
    let (h, m, s, f) = klipp_core::sync::frames_to_timecode(frames, fps, drop);
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
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{
        Clip, ClipKind, ClipMarker, Marker, Sequence, TitleData, Track, TrackKind,
    };
    use std::path::PathBuf;

    /// Build a minimal Sequence with the given fps/resolution and a single empty Video track.
    fn empty_sequence(fps: Fps) -> Sequence {
        Sequence {
            title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: "Test Sequence".into(),
            fps,
            resolution: Resolution::hd_1080(),
            first_clip_autoset: Default::default(),
            tracks: vec![Track::new("Video 1", TrackKind::Video)],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
        }
    }

    /// Create a MediaItem with the given id, filename, and duration (in frames at the given rate).
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
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
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
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
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
        if let Some(tb) = td.primary_text_mut() { tb.text = "Hello World".into(); }
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

        seq.markers.push(Marker { review: None,
            id: MarkerId::new(),
            position: RationalTime { value: 48, rate: 24 },
            label: "Scene 2".into(),
            color: MarkerColor::Green,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });
        seq.markers.push(Marker { review: None,
            id: MarkerId::new(),
            position: RationalTime { value: 120, rate: 24 },
            label: "End".into(),
            color: MarkerColor::Red,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
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
            comment: String::new(),
        });
        clip.clip_markers.push(ClipMarker {
            time_secs: 2.5,
            name: "Cut Here".into(),
            color: MarkerColor::Orange,
            comment: String::new(),
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
        if let Some(tb) = td.primary_text_mut() { tb.text = "Rock & Roll <Live> \"Show\"".into(); }
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
        // The reel is uppercased — CMX reels are conventionally upper,
        // and reel matching on conform is case-insensitive either way.
        // Columns are unchanged: the channel field is two wide now that
        // audio says A1/A2, and "V " plus four spaces is the same six
        // characters "V" plus five used to be.
        assert!(edl.contains("001  SCENE01  V     C        00:00:00:00 00:00:03:00 00:00:01:00 00:00:04:00\n"), "{edl}");
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
    fn edl_drop_frame_timecodes_agree_with_the_header() {
        // The header used to say DROP FRAME over timecodes counted
        // non-drop. A conform reads the header, applies drop-frame to
        // numbers that were never in it, and places every event 3.6 s
        // an hour early — silently, because nothing in the file is
        // malformed.
        let fps = Fps::ntsc_29970();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "hero.mov", 200_000, fps.rate()));

        // `RationalTime::value` is in ticks of the rate numerator, so one
        // video frame at 29.97 is 1001 ticks. Frame 107_892 is exactly
        // 01:00:00;00 in drop-frame — the canonical hour, and the value
        // the old arithmetic reported as 00:59:56:12.
        let frame = |n: i64| RationalTime { value: n * 1001, rate: 30_000 };
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            frame(107_892),
            frame(107_892 + 30),
            frame(0),
            frame(30),
        ));

        let edl = generate_edl(&seq, &pool);

        assert!(edl.contains("FCM: DROP FRAME"), "{edl}");
        assert!(
            edl.contains("01:00:00:00"),
            "the hour should be counted drop-frame, as the header says it is:\n{edl}"
        );
        assert!(
            !edl.contains("00:59:56:12"),
            "that is the non-drop count of the same frame — the defect:\n{edl}"
        );
    }

    #[test]
    fn edl_non_drop_rates_are_left_alone() {
        // 25 has nothing to drop, so the count is plain and the header
        // says so. Guards against the drop-frame path being applied to
        // a rate it does not belong to.
        let fps = Fps::new(25, 1);
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "pal.mov", 10_000, 25));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 25 * 60 * 10, rate: 25 }, // 10 minutes
            RationalTime { value: 25 * 60 * 10 + 25, rate: 25 },
            RationalTime { value: 0, rate: 25 },
            RationalTime { value: 25, rate: 25 },
        ));

        let edl = generate_edl(&seq, &pool);

        assert!(edl.contains("FCM: NON-DROP FRAME"), "{edl}");
        assert!(edl.contains("00:10:00:00"), "{edl}");
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

        // Eight characters, uppercase, alphanumeric: the underscores go
        // because the format is column-based and punctuation in the reel
        // field is not portable, so "very_long_clip_name" reduces to
        // "VERYLONG" rather than "VERY_LON".
        assert!(edl.contains("VERYLONG"), "{edl}");
        // Full name should NOT appear in the event line (only in FROM CLIP NAME comment)
        let event_line = edl.lines()
            .find(|l| l.starts_with("001"))
            .expect("should have event line");
        assert!(!event_line.contains("very_long_clip_name"));
        assert!(event_line.contains("VERYLONG"));
    }

    #[test]
    fn edl_reel_comes_from_the_metadata_not_the_filename() {
        // The reel is how a conform finds the card an event came from.
        // Klipp reads it at import — from the file and from Sony/Canon
        // XML sidecars — and the exporter used to write the first eight
        // characters of the filename instead, which is a guess.
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        let mut item = make_media_item(mid, "clip_0001_transcoded.mov", 24, 24);
        item.shot.reel_name = Some("A001C002".into());
        pool.add(item);

        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 24, rate: 24 },
        ));

        let edl = generate_edl(&seq, &pool);
        let event_line = edl.lines().find(|l| l.starts_with("001")).expect("event line");
        assert!(event_line.contains("A001C002"), "the card's own reel:\n{edl}");
        assert!(!event_line.contains("CLIP0001"), "not the filename:\n{edl}");
    }

    #[test]
    fn edl_two_sources_never_share_a_reel() {
        // Eight characters is the format's limit and exactly why this
        // happens: the same card on two days reduces to the same reel,
        // and a conform would then pull the wrong shot. C0001.MP4 on
        // every card of a multi-camera shoot is the common case.
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let a = MediaId::new();
        let b = MediaId::new();
        pool.add(make_media_item(a, "A001C001_240315.mov", 24, 24));
        pool.add(make_media_item(b, "A001C001_240316.mov", 24, 24));
        for (i, mid) in [a, b].into_iter().enumerate() {
            let at = i as i64 * 24;
            seq.tracks[0].clips.push(make_media_clip(
                mid,
                RationalTime { value: at, rate: 24 },
                RationalTime { value: at + 24, rate: 24 },
                RationalTime { value: 0, rate: 24 },
                RationalTime { value: 24, rate: 24 },
            ));
        }

        let edl = generate_edl(&seq, &pool);
        let reels: Vec<&str> = edl
            .lines()
            .filter(|l| l.len() > 12 && l.starts_with(|c: char| c.is_ascii_digit()))
            .map(|l| l[5..13].trim())
            .collect();
        assert_eq!(reels.len(), 2, "{edl}");
        assert_ne!(reels[0], reels[1], "two sources took the same reel:\n{edl}");
        assert!(edl.contains("* REEL"), "the rename should be recorded:\n{edl}");
    }

    #[test]
    fn edl_one_source_cut_many_times_keeps_one_reel() {
        // The flip side of the rule above: a reel identifies the source,
        // not the event, so twenty cuts of one card are twenty events on
        // one reel.
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "A001C001.mov", 240, 24));
        for i in 0..3i64 {
            seq.tracks[0].clips.push(make_media_clip(
                mid,
                RationalTime { value: i * 24, rate: 24 },
                RationalTime { value: i * 24 + 24, rate: 24 },
                RationalTime { value: 0, rate: 24 },
                RationalTime { value: 24, rate: 24 },
            ));
        }

        let edl = generate_edl(&seq, &pool);
        // Event lines only — the reel also appears in each
        // "* FROM CLIP NAME" comment, which is the filename, not the reel.
        let reels: Vec<&str> = edl
            .lines()
            .filter(|l| l.len() > 12 && l.starts_with(|c: char| c.is_ascii_digit()))
            .map(|l| l[5..13].trim())
            .collect();
        assert_eq!(reels, ["A001C001", "A001C001", "A001C001"], "{edl}");
        assert!(!edl.contains("* REEL"), "nothing was renamed:\n{edl}");
    }

    #[test]
    fn edl_reel_from_a_non_ascii_filename_does_not_panic() {
        // `&name[..8]` split a multi-byte character and panicked. The
        // stem here is 13 characters and 14 bytes, so byte 8 lands
        // inside the 'é'.
        assert_eq!(sanitise_reel("Café_shoot_01"), "CAFSHOOT");
        assert_eq!(sanitise_reel("日本語のクリップ"), "AX", "nothing usable left");
        assert_eq!(sanitise_reel(""), "AX");
        assert_eq!(sanitise_reel("a-b-c"), "ABC");
    }

    /// Two clips butted together with a transition between them.
    fn two_clips_with(kind: klipp_core::TransitionKind, secs: f64) -> (Sequence, MediaPool) {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "A001C001.mov", 480, 24));

        let a = make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        );
        let b = make_media_clip(
            mid,
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 96, rate: 24 },
            RationalTime { value: 96, rate: 24 },
            RationalTime { value: 144, rate: 24 },
        );
        let (a_id, b_id) = (a.id, b.id);
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);
        seq.tracks[0].transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind,
            duration: RationalTime::from_secs_f64(secs, 24),
            clip_a_id: a_id,
            clip_b_id: b_id,
            user_params: Default::default(),
            alignment: Default::default(),
        });
        (seq, pool)
    }

    #[test]
    fn a_dissolve_is_a_d_event_on_the_incoming_clip() {
        // Every transition used to be written as a cut, so a conform
        // got the right clips with the wrong boundaries and nothing
        // saying so. CMX puts the code on the arriving event.
        let (seq, pool) = two_clips_with(klipp_core::TransitionKind::CrossDissolve, 1.0);
        let edl = generate_edl(&seq, &pool);

        let events: Vec<&str> = edl
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .collect();
        assert_eq!(events.len(), 2, "{edl}");
        assert!(events[0].contains(" C  "), "the outgoing clip stays a cut:\n{edl}");
        // One second at 24 is 24 frames.
        assert!(events[1].contains("D     24 "), "the incoming carries D and its length:\n{edl}");
        assert!(edl.contains("* EFFECT NAME: CROSS DISSOLVE"), "{edl}");
    }

    #[test]
    fn a_cardinal_wipe_takes_its_smpte_number() {
        let (seq, pool) = two_clips_with(klipp_core::TransitionKind::WipeLeft, 0.5);
        let edl = generate_edl(&seq, &pool);
        assert!(edl.contains("W001  12 "), "horizontal wipe, 12 frames:\n{edl}");

        let (seq, pool) = two_clips_with(klipp_core::TransitionKind::WipeUp, 0.5);
        let edl = generate_edl(&seq, &pool);
        assert!(edl.contains("W002  12 "), "vertical wipe:\n{edl}");
    }

    #[test]
    fn a_transition_cmx_has_no_number_for_still_goes_out_with_its_name() {
        // A page turn is not a dissolve. Writing a cut would lose it
        // entirely and say nothing; a D of the right length keeps the
        // duration and the boundary, and the name goes beside it so a
        // finishing app can recreate the real thing.
        let (seq, pool) = two_clips_with(klipp_core::TransitionKind::PageTurn, 1.0);
        let edl = generate_edl(&seq, &pool);
        assert!(edl.contains("D     24 "), "{edl}");
        assert!(edl.contains("* EFFECT NAME: "), "the real kind is recorded:\n{edl}");
        assert!(!edl.contains("* EFFECT NAME: CROSS DISSOLVE"), "and it is not called a dissolve:\n{edl}");
    }

    #[test]
    fn a_cut_is_still_a_cut_and_says_nothing_extra() {
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "A001C001.mov", 480, 24));
        seq.tracks[0].clips.push(make_media_clip(
            mid,
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
            RationalTime { value: 0, rate: 24 },
            RationalTime { value: 48, rate: 24 },
        ));
        let edl = generate_edl(&seq, &pool);
        assert!(!edl.contains("* EFFECT NAME"), "nothing to say about a cut:\n{edl}");
        assert!(edl.contains("  C  "), "{edl}");
    }

    #[test]
    fn edl_audio_tracks_are_numbered() {
        // A multi-track conform needs to know which channel an event
        // belongs to; every audio event used to say a bare "A".
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        seq.tracks.push(Track::new("Audio 1", TrackKind::Audio));
        seq.tracks.push(Track::new("Audio 2", TrackKind::Audio));
        let mut pool = MediaPool::new();

        let mid = MediaId::new();
        pool.add(make_media_item(mid, "sound.wav", 24, 24));
        for ti in [1usize, 2] {
            seq.tracks[ti].clips.push(make_media_clip(
                mid,
                RationalTime { value: 0, rate: 24 },
                RationalTime { value: 24, rate: 24 },
                RationalTime { value: 0, rate: 24 },
                RationalTime { value: 24, rate: 24 },
            ));
        }

        let edl = generate_edl(&seq, &pool);
        assert!(edl.contains(" A1    C "), "first audio track is A1:\n{edl}");
        assert!(edl.contains(" A2    C "), "second is A2:\n{edl}");
    }

    #[test]
    fn fcpxml_marker_comment_writes_note_child() {
        // A marker with a non-empty comment should expand to the
        // open/close <marker>...<note>...</note></marker> shape so
        // round-trip readers (FCP, Klipp's own importer) can recover
        // the note. Legacy comment-less markers stay self-closing.
        let fps = Fps::film_24();
        let mut seq = empty_sequence(fps);
        let pool = MediaPool::new();

        seq.markers.push(Marker { review: None,
            id: MarkerId::new(),
            position: RationalTime { value: 24, rate: 24 },
            label: "Cut on action".into(),
            color: MarkerColor::Yellow,
            duration: None,
            comment: "Trim two frames off the head".into(),
            kind: klipp_core::MarkerKind::ToDo,
        });
        seq.markers.push(Marker { review: None,
            id: MarkerId::new(),
            position: RationalTime { value: 48, rate: 24 },
            label: "Plain".into(),
            color: MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });

        let xml = generate_fcpxml(&seq, &pool);

        // Comment-bearing marker has the open/close form + the note child.
        assert!(xml.contains("<marker start=\"24/24s\" duration=\"1/24s\" value=\"Cut on action\">"));
        assert!(xml.contains("<note>Trim two frames off the head</note>"));
        // Plain marker stays self-closing.
        assert!(xml.contains("<marker start=\"48/24s\" duration=\"1/24s\" value=\"Plain\"/>"));
    }
}
