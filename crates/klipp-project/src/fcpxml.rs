//! FCPXML export (version 1.9) for interchange with Final Cut Pro and other NLEs.
//!
//! Generates a valid FCPXML document from a Klipp sequence with media references,
//! clip timing, transitions, and markers.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use std::path::Path;

/// Export a sequence as FCPXML 1.9 to the given path. Returns what the
/// file does not carry, one line each (see [`generate_fcpxml_reporting`]).
pub fn export_fcpxml(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<Vec<String>, String> {
    let (xml, dropped) = generate_fcpxml_reporting(seq, media_pool);
    std::fs::write(output_path, xml)
        .map_err(|e| format!("Failed to write FCPXML: {e}"))?;
    Ok(dropped)
}

/// Generate FCPXML 1.9 string from a sequence.
pub fn generate_fcpxml(seq: &Sequence, media_pool: &MediaPool) -> String {
    generate_fcpxml_reporting(seq, media_pool).0
}

/// One element of the spine: where it sits on the timeline, the local time
/// its `start` names, and the clips connected to it.
struct SpineElement {
    at: RationalTime,
    local: RationalTime,
    tag: &'static str,
    attrs: String,
    body: String,
    connected: String,
}

/// The FCPXML for `seq` and what it leaves out (#2471). The lowest picture
/// track is the spine, with a gap for every hole in it; every other picture
/// track is a connected lane above it and every sound track one under it.
/// A file is one asset whatever number of clips show it. A constant speed
/// is a `timeMap`. What the format is not given here (grades and effects,
/// transitions, captions, sound levels other than a constant volume, and
/// generators, adjustment layers and compound clips, which keep their
/// length as gaps) is named in the list, so nothing is lost unsaid.
pub fn generate_fcpxml_reporting(seq: &Sequence, media_pool: &MediaPool) -> (String, Vec<String>) {
    let mut xml = String::with_capacity(8192);
    let mut dropped = Vec::new();
    let fps = &seq.fps;
    let frame_dur = rational_time_str(1, fps.num, fps.den);

    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<!DOCTYPE fcpxml>\n");
    xml.push_str("<fcpxml version=\"1.9\">\n");
    xml.push_str("  <resources>\n");
    let format_id = "r0";
    xml.push_str(&format!(
        "    <format id=\"{format_id}\" name=\"FFVideoFormat{}p{}\" frameDuration=\"{}\" width=\"{}\" height=\"{}\"/>\n",
        seq.resolution.height, fps_display(fps), frame_dur, seq.resolution.width, seq.resolution.height
    ));

    // One asset per file, in the order the timeline first shows them.
    let mut assets: Vec<(MediaId, String)> = Vec::new();
    for clip in seq.tracks.iter().filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio)).flat_map(|t| &t.clips) {
        let ClipKind::Media { media_id } = &clip.kind else { continue };
        let Some(item) = media_pool.get(media_id) else { continue };
        if assets.iter().any(|(id, _)| id == media_id) {
            continue;
        }
        let asset_id = format!("r{}", assets.len() + 2);
        let has_video = item.is_still || !item.video_codec.is_empty();
        xml.push_str(&format!(
            "    <asset id=\"{asset_id}\" name=\"{}\" src=\"file://{}\" start=\"0s\" duration=\"{}\" hasVideo=\"{}\" hasAudio=\"{}\" format=\"{format_id}\"/>\n",
            escape_xml(&item.path.file_name().unwrap_or_default().to_string_lossy()),
            escape_xml(&item.path.to_string_lossy()),
            tc_str(item.duration, fps),
            u8::from(has_video),
            u8::from(item.has_audio),
        ));
        assets.push((*media_id, asset_id));
    }
    let title_effect = "r1";
    if seq.tracks.iter().flat_map(|t| &t.clips).any(|c| matches!(c.kind, ClipKind::Title(_))) {
        xml.push_str(&format!(
            "    <effect id=\"{title_effect}\" name=\"Basic Title\" uid=\".../Titles.localized/Bumper:Opener.localized/Basic Title.localized/Basic Title.moti\"/>\n"
        ));
    }
    xml.push_str("  </resources>\n");

    xml.push_str("  <library>\n");
    xml.push_str("    <event name=\"Klipp Export\">\n");
    xml.push_str(&format!("      <project name=\"{}\">\n", escape_xml(&seq.name)));
    xml.push_str(&format!(
        "        <sequence format=\"{}\" duration=\"{}\" tcStart=\"0s\">\n",
        format_id,
        sequence_duration(seq, fps)
    ));

    let ctx = ClipXml { seq, pool: media_pool, assets: &assets, title_effect };
    let seq_end = seq.tracks.iter().flat_map(|t| &t.clips).map(|c| c.timeline_out).max().unwrap_or(RationalTime::ZERO);
    let video: Vec<usize> = (0..seq.tracks.len()).filter(|&i| seq.tracks[i].kind == TrackKind::Video).collect();
    let storyline = video.last().copied();

    // The spine, its holes filled, and a gap after it to the end so a
    // clip connected past its last clip has something to hang from.
    let mut spine: Vec<SpineElement> = Vec::new();
    let mut cursor = RationalTime::ZERO;
    let gap = |at: RationalTime, dur: RationalTime| SpineElement {
        at,
        local: RationalTime::ZERO,
        tag: "gap",
        attrs: format!("offset=\"{}\" duration=\"{}\"", tc_str(at, fps), tc_str(dur, fps)),
        body: String::new(),
        connected: String::new(),
    };
    if let Some(ti) = storyline {
        let track = &seq.tracks[ti];
        let mut clips: Vec<&Clip> = track.clips.iter().filter(|c| c.timeline_out > c.timeline_in).collect();
        clips.sort_by_key(|c| c.timeline_in);
        for clip in clips {
            if clip.timeline_in < cursor {
                dropped.push(format!("Track '{}': {} at {:.3} s lies over the clip before it, which the primary storyline cannot hold — left out", track.name, describe(clip, media_pool), clip.timeline_in.to_secs_f64()));
                continue;
            }
            if clip.timeline_in > cursor {
                spine.push(gap(cursor, clip.timeline_in - cursor));
            }
            let offset = tc_str(clip.timeline_in, fps);
            let (tag, attrs, body) = ctx.element(track, clip, None, &offset, &mut dropped);
            spine.push(SpineElement { at: clip.timeline_in, local: if tag == "gap" { RationalTime::ZERO } else { clip.source_in }, tag, attrs, body, connected: String::new() });
            cursor = clip.timeline_out;
        }
    }
    if cursor < seq_end {
        spine.push(gap(cursor, seq_end - cursor));
    }

    // Lanes: picture tracks above the storyline count up from it, sound
    // tracks count down from it in their order.
    let mut lanes: Vec<(usize, i64)> = Vec::new();
    if let Some(s) = storyline {
        lanes.extend(video.iter().rev().filter(|&&i| i != s).enumerate().map(|(n, &i)| (i, n as i64 + 1)));
    }
    let sound = (0..seq.tracks.len()).filter(|&i| seq.tracks[i].kind == TrackKind::Audio);
    lanes.extend(sound.enumerate().map(|(n, i)| (i, -(n as i64) - 1)));
    for &(ti, lane) in &lanes {
        let track = &seq.tracks[ti];
        for clip in track.clips.iter().filter(|c| c.timeline_out > c.timeline_in) {
            if matches!(clip.kind, ClipKind::Gap) {
                continue;
            }
            let Some(parent) = spine.iter_mut().rev().find(|e| e.at <= clip.timeline_in) else { continue };
            let offset = tc_str(clip.timeline_in - parent.at + parent.local, fps);
            let (tag, attrs, body) = ctx.element(track, clip, Some(lane), &offset, &mut dropped);
            if tag == "gap" {
                continue;
            }
            parent.connected.push_str(&render(tag, &attrs, &body, "", "              "));
        }
    }
    for track in &seq.tracks {
        if !matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
            let n = track.clips.iter().filter(|c| !c.kind.is_gap()).count();
            if n > 0 {
                dropped.push(format!("Track '{}': {n} {:?} clip(s) — FCPXML export does not write captions; use subtitle export for them", track.name, track.kind));
            }
            continue;
        }
        for t in &track.transitions {
            dropped.push(format!("Track '{}': a {:?} transition of {:.3} s — the file has a cut there", track.name, t.kind, t.duration.to_secs_f64()));
        }
        if track.muted && track.kind == TrackKind::Audio {
            dropped.push(format!("Track '{}' is muted — its clips are written as playing", track.name));
        }
        if !track.visible && track.kind == TrackKind::Video {
            dropped.push(format!("Track '{}' is hidden — its clips are written as showing", track.name));
        }
    }

    xml.push_str("          <spine>\n");
    for e in &spine {
        xml.push_str(&render(e.tag, &e.attrs, &e.body, &e.connected, "            "));
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

    (xml, dropped)
}

/// An element at `indent`, self-closed when it holds nothing: its own
/// children a step in, its connected clips rendered at that depth already.
fn render(tag: &str, attrs: &str, body: &str, connected: &str, indent: &str) -> String {
    if body.is_empty() && connected.is_empty() {
        return format!("{indent}<{tag} {attrs}/>\n");
    }
    let inner: String = body.lines().map(|l| format!("{indent}  {l}\n")).collect();
    format!("{indent}<{tag} {attrs}>\n{inner}{connected}{indent}</{tag}>\n")
}

/// What a clip's element is written from.
struct ClipXml<'a> {
    seq: &'a Sequence,
    pool: &'a MediaPool,
    assets: &'a [(MediaId, String)],
    title_effect: &'a str,
}

impl ClipXml<'_> {
    /// A clip's element: its tag, attributes and children (unindented), on
    /// `lane` when connected; what of it the element leaves out goes in
    /// `dropped`.
    fn element(&self, track: &Track, clip: &Clip, lane: Option<i64>, offset: &str, dropped: &mut Vec<String>) -> (&'static str, String, String) {
        let fps = &self.seq.fps;
        let lane_attr = lane.map(|l| format!(" lane=\"{l}\"")).unwrap_or_default();
        let duration = tc_str(clip.duration(), fps);
        let at = clip.timeline_in.to_secs_f64();
        let what = describe(clip, self.pool);
        let enabled = if clip.enabled { "" } else { " enabled=\"0\"" };
        let gap = |name: Option<&str>| {
            let name = name.map(|n| format!("name=\"{n}\" ")).unwrap_or_default();
            ("gap", format!("{name}offset=\"{offset}\" duration=\"{duration}\""), String::new())
        };
        match &clip.kind {
            ClipKind::Gap => gap(None),
            ClipKind::Title(td) => {
                dropped.push(format!("Track '{}': {what} at {at:.3} s — its text and length only, not its look", track.name));
                let text = escape_xml(&td.text());
                (
                    "title",
                    format!("name=\"{text}\" ref=\"{}\"{lane_attr} offset=\"{offset}\" duration=\"{duration}\"{enabled}", self.title_effect),
                    format!("<text>{text}</text>\n"),
                )
            }
            ClipKind::Media { media_id } => {
                let Some((_, asset)) = self.assets.iter().find(|(id, _)| id == media_id) else {
                    dropped.push(format!("Track '{}': {what} at {at:.3} s — its media is not in the pool, left out", track.name));
                    return gap(None);
                };
                let sound_track = track.kind == TrackKind::Audio;
                // The picture of a clip whose sound is a linked clip of its
                // own plays no sound of its own, as on Klipp's timeline.
                let linked_sound = clip.linked_clip_id.is_some_and(|id| self.seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == id)));
                let src = if sound_track { " srcEnable=\"audio\"" } else if linked_sound { " srcEnable=\"video\"" } else { "" };
                let attrs = format!(
                    "ref=\"{asset}\" name=\"{}\"{lane_attr} offset=\"{offset}\" duration=\"{duration}\" start=\"{}\"{src}{enabled}",
                    escape_xml(&what_name(clip, self.pool)),
                    tc_str(clip.source_in, fps),
                );
                let mut body = String::new();
                // A constant speed: local time runs from the clip's start
                // at the source frame it plays first, so `start` stays
                // the source in and the clip ends at its source out.
                let ramped = !clip.speed_keyframes.is_empty();
                if ramped || (clip.speed - 1.0).abs() > 1e-9 {
                    let (first, last) = if clip.is_reversed() { (clip.source_out, clip.source_in) } else { (clip.source_in, clip.source_out) };
                    let sampling = match clip.frame_blend_mode() {
                        Some(1) => " frameSampling=\"frame-blending\"",
                        Some(_) => " frameSampling=\"optical-flow-classic\"",
                        None => "",
                    };
                    let pitch = if clip.keep_pitch { "" } else { " preservesPitch=\"0\"" };
                    body.push_str(&format!("<timeMap{sampling}{pitch}>\n"));
                    body.push_str(&format!("  <timept time=\"{}\" value=\"{}\" interp=\"linear\"/>\n", tc_str(clip.source_in, fps), tc_str(first, fps)));
                    body.push_str(&format!("  <timept time=\"{}\" value=\"{}\" interp=\"linear\"/>\n", tc_str(clip.source_in + clip.duration(), fps), tc_str(last, fps)));
                    body.push_str("</timeMap>\n");
                    if ramped {
                        dropped.push(format!("Track '{}': {what} at {at:.3} s — its speed ramp, written as one even speed over the same frames", track.name));
                    }
                }
                if (sound_track || !linked_sound) && clip.volume_keyframes.is_empty() && (clip.volume - 1.0).abs() > 1e-6 && clip.volume > 0.0 {
                    body.push_str(&format!("<adjust-volume amount=\"{:.2}dB\"/>\n", 20.0 * f64::from(clip.volume).log10()));
                }
                for marker in &clip.clip_markers {
                    body.push_str(&format!(
                        "<marker start=\"{}s\" duration=\"1/{}s\" value=\"{}\"/>\n",
                        format_decimal(clip.source_in.to_secs_f64() + marker.time_secs), fps.num, escape_xml(&marker.name)
                    ));
                }
                let mut left = Vec::new();
                if !sound_track && (!clip.grade_is_identity() || clip.has_plugin_entries() || clip.effect_node_at(false, 1).is_some() || (clip.effect_node_at(false, 0).is_some() && !clip.has_speed_ramp())) {
                    left.push("its grade and picture effects");
                }
                if (sound_track || !linked_sound) && (!clip.volume_keyframes.is_empty() || clip.volume <= 0.0) {
                    left.push("its volume changes");
                }
                if (sound_track || !linked_sound) && (clip.pan.abs() > 1e-6 || !clip.pan_keyframes.is_empty()) {
                    left.push("its pan");
                }
                if clip.fade_in_secs > 0.0 || clip.fade_out_secs > 0.0 {
                    left.push("its fades");
                }
                if !clip.audio_effects.is_empty() {
                    left.push("its sound effects");
                }
                if !left.is_empty() {
                    dropped.push(format!("Track '{}': {what} at {at:.3} s — {}", track.name, left.join(", ")));
                }
                ("asset-clip", attrs, body)
            }
            ClipKind::Generator(_) | ClipKind::AdjustmentLayer | ClipKind::Sequence { .. } | ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. } => {
                dropped.push(format!("Track '{}': {what} at {at:.3} s — FCPXML export does not write it{}", track.name, if lane.is_none() { "; a gap keeps its length" } else { "" }));
                gap(Some(match clip.kind {
                    ClipKind::AdjustmentLayer => "Adjustment Layer",
                    ClipKind::Generator(_) => "Generator",
                    _ => "Compound Clip",
                }))
            }
        }
    }
}

/// A clip's file name, or what it is.
fn what_name(clip: &Clip, pool: &MediaPool) -> String {
    match &clip.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "Untitled".into()),
        _ => describe(clip, pool),
    }
}

/// A clip as the dropped list names it.
pub(crate) fn describe(clip: &Clip, pool: &MediaPool) -> String {
    match &clip.kind {
        ClipKind::Media { .. } => format!("'{}'", what_name(clip, pool)),
        ClipKind::Title(td) => {
            let text = td.text();
            let text = text.trim();
            if text.is_empty() { "a title".to_string() } else { format!("the title \"{}\"", text.chars().take(40).collect::<String>()) }
        }
        ClipKind::Generator(_) => "a generator".to_string(),
        ClipKind::AdjustmentLayer => "an adjustment layer".to_string(),
        ClipKind::Sequence { .. } => "a compound clip".to_string(),
        ClipKind::Precomposition { .. } => "a precomposition".to_string(),
        ClipKind::TemplatePrecomposition { .. } => "a template precomposition".to_string(),
        ClipKind::Gap => "a gap".to_string(),
    }
}

/// Also generate a simple EDL (CMX 3600 format) for broader compatibility.
/// Returns what the EDL does not carry, one line each (#2520).
pub fn export_edl(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<Vec<String>, String> {
    let edl = generate_edl(seq, media_pool);
    std::fs::write(output_path, edl)
        .map_err(|e| format!("Failed to write EDL: {e}"))?;
    Ok(edl_not_carried(seq, media_pool))
}

/// What a CMX 3600 EDL leaves out of `seq`: one picture track and four
/// sound channels, cuts and transitions, nothing of a clip but its source
/// range; a clip switched off is not written.
pub fn edl_not_carried(seq: &Sequence, media_pool: &MediaPool) -> Vec<String> {
    crate::not_carried::not_carried(seq, media_pool, &crate::not_carried::Format { name: "an EDL", transitions: true, video_tracks: Some(1), audio_tracks: Some(4), drops_disabled: true })
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
            awaiting_refit: false,
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
            matched_to: None,
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

    // ── #2471: every track, speed, and what is left out ──

    /// A picture track over the storyline, a sound track under it, one
    /// file shown by three clips: the file is one asset, the clips are on
    /// their lanes at their places, and the sound comes from the sound clip.
    fn three_tracks() -> (Sequence, MediaPool, [ClipId; 3]) {
        let fps = Fps { num: 25, den: 1 };
        let mut seq = empty_sequence(fps);
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "take.mov", 250, 25));
        let t = |s: i64| RationalTime { value: s * 25, rate: 25 };
        // Storyline: 0-4 s from 2 s of the file; a hole; then 6-8 s.
        let mut a = make_media_clip(mid, t(0), t(4), t(2), t(6));
        let b = make_media_clip(mid, t(6), t(8), t(0), t(2));
        let over = make_media_clip(mid, t(1), t(3), t(5), t(7));
        let mut sound = make_media_clip(mid, t(0), t(4), t(2), t(6));
        a.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(a.id);
        let ids = [a.id, over.id, sound.id];
        seq.tracks[0].name = "V1".into();
        seq.tracks[0].clips = vec![a, b];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips.push(over);
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips.push(sound);
        seq.tracks = vec![v2, seq.tracks.remove(0), a1];
        (seq, pool, ids)
    }

    #[test]
    fn every_track_goes_out_on_its_lane() {
        let (seq, pool, _) = three_tracks();
        let (xml, dropped) = generate_fcpxml_reporting(&seq, &pool);
        roxmltree::Document::parse_with_options(&xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).expect("well-formed");
        assert_eq!(xml.matches("<asset id=").count(), 1, "one asset for one file:\n{xml}");
        assert!(xml.contains("duration=\"250/25s\" hasVideo=\"1\" hasAudio=\"1\""), "the file's own length:\n{xml}");
        // The hole on the storyline is a gap, so 6 s stays 6 s.
        assert!(xml.contains("<gap offset=\"100/25s\" duration=\"50/25s\"/>"), "{xml}");
        // Connected to the first clip, in its local time: 1 s in, from 2 s.
        assert!(xml.contains("lane=\"1\" offset=\"75/25s\" duration=\"50/25s\" start=\"125/25s\""), "{xml}");
        assert!(xml.contains("lane=\"-1\" offset=\"50/25s\" duration=\"100/25s\" start=\"50/25s\" srcEnable=\"audio\""), "{xml}");
        assert!(xml.contains("offset=\"0/25s\" duration=\"100/25s\" start=\"50/25s\" srcEnable=\"video\""), "the picture leaves its sound to the sound clip:\n{xml}");
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn what_goes_out_comes_back_in_on_the_same_tracks() {
        let (seq, pool, _) = three_tracks();
        let back = crate::fcpxml_import::parse_fcpxml(&generate_fcpxml(&seq, &pool)).unwrap();
        let places = |s: &Sequence| -> Vec<(String, Vec<(i64, i64, i64)>)> {
            s.tracks.iter().map(|t| (format!("{:?}", t.kind), t.clips.iter().filter(|c| !c.kind.is_gap()).map(|c| (c.timeline_in.rescale_to(25).value, c.timeline_out.rescale_to(25).value, c.source_in.rescale_to(25).value)).collect())).collect()
        };
        assert_eq!(places(&back.sequence), places(&seq), "{:?}", back.warnings);
    }

    #[test]
    fn a_speed_change_is_a_time_map_inside_its_media() {
        let (mut seq, pool, _) = three_tracks();
        // 2x: 4 s of the file in 2 s, then reversed.
        let clip = &mut seq.tracks[1].clips[1];
        clip.source_in = RationalTime { value: 100, rate: 25 };
        clip.source_out = RationalTime { value: 200, rate: 25 };
        clip.speed = 2.0;
        let xml = generate_fcpxml(&seq, &pool);
        assert!(xml.contains("<timept time=\"100/25s\" value=\"100/25s\" interp=\"linear\"/>"), "{xml}");
        assert!(xml.contains("<timept time=\"150/25s\" value=\"200/25s\" interp=\"linear\"/>"), "ends at its source out, not 2 s past it:\n{xml}");
        let back = crate::fcpxml_import::parse_fcpxml(&xml).unwrap();
        let c = back.sequence.tracks[1].clips.iter().find(|c| c.timeline_in.rescale_to(25).value == 150).unwrap();
        assert_eq!((c.speed, c.source_out.rescale_to(25).value), (2.0, 200));

        seq.tracks[1].clips[1].speed = -2.0;
        let xml = generate_fcpxml(&seq, &pool);
        assert!(xml.contains("<timept time=\"100/25s\" value=\"200/25s\" interp=\"linear\"/>"), "reversed: from its source out:\n{xml}");
        assert!(xml.contains("<timept time=\"150/25s\" value=\"100/25s\" interp=\"linear\"/>"), "{xml}");
    }

    #[test]
    fn what_the_file_does_not_carry_is_listed() {
        let (mut seq, pool, [a, _, _]) = three_tracks();
        let b = seq.tracks[1].clips[1].id;
        seq.tracks[1].transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: RationalTime { value: 10, rate: 25 },
            clip_a_id: a,
            clip_b_id: b,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        let mut td = TitleData::default();
        if let Some(tb) = td.primary_text_mut() { tb.text = "Name".into(); }
        seq.tracks[0].clips.push(make_clip_with_kind(ClipKind::Title(td), RationalTime { value: 150, rate: 25 }, RationalTime { value: 175, rate: 25 }));
        seq.tracks[0].clips.push(make_clip_with_kind(ClipKind::AdjustmentLayer, RationalTime { value: 200, rate: 25 }, RationalTime { value: 225, rate: 25 }));
        let mut subs = Track::new("Captions", TrackKind::Subtitle);
        subs.clips.push(make_clip_with_kind(ClipKind::Title(TitleData::default()), RationalTime { value: 0, rate: 25 }, RationalTime { value: 25, rate: 25 }));
        seq.tracks.push(subs);
        seq.tracks[2].clips[0].fade_in_secs = 0.5;
        seq.tracks[2].clips[0].volume_keyframes.push(klipp_core::Keyframe { time_secs: 0.0, value: 0.5, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() });
        let (xml, dropped) = generate_fcpxml_reporting(&seq, &pool);
        roxmltree::Document::parse_with_options(&xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).expect("well-formed");
        let said = |what: &str| assert!(dropped.iter().any(|d| d.contains(what)), "{what}: {dropped:#?}");
        said("CrossDissolve transition");
        said("the title \"Name\" at 6.000 s — its text and length only");
        said("an adjustment layer at 8.000 s");
        said("Track 'Captions': 1 Subtitle clip(s)");
        said("Track 'A1': 'take.mov' at 0.000 s — its volume changes, its fades");
        assert!(xml.contains("ref=\"r1\"") && xml.contains("name=\"Basic Title\""), "a title names its effect:\n{xml}");
    }

    // ── #2520: what an EDL and a Kdenlive project leave out ──

    #[test]
    fn plain_cuts_leave_nothing_out() {
        let mut seq = empty_sequence(Fps { num: 25, den: 1 });
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(make_media_item(mid, "take.mov", 250, 25));
        let t = |s: i64| RationalTime { value: s * 25, rate: 25 };
        seq.tracks[0].clips = vec![make_media_clip(mid, t(0), t(4), t(0), t(4)), make_media_clip(mid, t(4), t(8), t(4), t(8))];
        assert_eq!(edl_not_carried(&seq, &pool), Vec::<String>::new());
        assert_eq!(crate::mlt_export::kdenlive_not_carried(&seq, &pool), Vec::<String>::new());
    }

    #[test]
    fn what_an_edl_and_a_kdenlive_project_leave_out_is_listed() {
        let (mut seq, pool, [a, _, _]) = three_tracks();
        let b = seq.tracks[1].clips[1].id;
        seq.tracks[1].transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: RationalTime { value: 10, rate: 25 },
            clip_a_id: a,
            clip_b_id: b,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        seq.tracks[1].clips[1].speed = 2.0;
        seq.tracks[1].clips[1].enabled = false;
        let mut td = TitleData::default();
        if let Some(tb) = td.primary_text_mut() { tb.text = "Name".into(); }
        seq.tracks[0].clips.push(make_clip_with_kind(ClipKind::Title(td), RationalTime { value: 150, rate: 25 }, RationalTime { value: 175, rate: 25 }));
        seq.tracks[2].clips[0].fade_in_secs = 0.5;
        let mut subs = Track::new("Captions", TrackKind::Subtitle);
        subs.clips.push(make_clip_with_kind(ClipKind::Title(TitleData::default()), RationalTime { value: 0, rate: 25 }, RationalTime { value: 25, rate: 25 }));
        seq.tracks.push(subs);
        seq.markers.push(Marker { review: None, id: MarkerId::new(), position: RationalTime { value: 25, rate: 25 }, label: "Intro".into(), color: MarkerColor::Blue, duration: None, comment: String::new(), kind: Default::default() });

        let edl = edl_not_carried(&seq, &pool);
        let said = |list: &[String], what: &str| assert!(list.iter().any(|d| d.contains(what)), "{what}: {list:#?}");
        said(&edl, "Track 'V1': an EDL has one picture track");
        said(&edl, "the title \"Name\" at 6.000 s — an EDL cannot carry it");
        said(&edl, "is switched off — left out");
        said(&edl, "its speed change");
        said(&edl, "Track 'A1': 'take.mov' at 0.000 s — its fades");
        said(&edl, "Track 'Captions': 1 Subtitle clip(s)");
        said(&edl, "1 sequence marker(s)");
        assert!(!edl.iter().any(|d| d.contains("transition")), "an EDL carries the dissolve: {edl:#?}");

        let mlt = crate::mlt_export::kdenlive_not_carried(&seq, &pool);
        said(&mlt, "CrossDissolve transition");
        said(&mlt, "is switched off — written as on");
        assert!(!mlt.iter().any(|d| d.contains("one picture track")), "every track comes across: {mlt:#?}");
    }
}
