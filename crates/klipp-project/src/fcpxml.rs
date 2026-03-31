//! FCPXML export (version 1.9) for interchange with Final Cut Pro and other NLEs.
//!
//! Generates a valid FCPXML document from a Klipp sequence with media references,
//! clip timing, transitions, and markers.

use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind, TransitionKind};
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
    let framerate_str = format!("{}fps", fps_display(fps));
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
        let mut asset_idx = 0;
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
                    asset_idx += 1;
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
                ClipKind::Generator(_) => {
                    let offset = tc_str(clip.timeline_in, fps);
                    let duration = tc_str(clip.duration(), fps);
                    xml.push_str(&format!(
                        "            <gap name=\"Generator\" offset=\"{}\" duration=\"{}\"/>\n",
                        offset, duration
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
