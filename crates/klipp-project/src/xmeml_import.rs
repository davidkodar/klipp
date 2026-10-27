//! Final Cut Pro 7 XML (`<xmeml>`) timelines (#2084), the interchange XML
//! many editors still export.
//!
//! Read: the first sequence, its rate (NTSC and drop frame) and frame
//! size; its video and audio tracks, their clip items with their files
//! (`pathurl`, percent-encoded, `file://localhost/` or not), enabled or
//! not, linked picture and sound; gaps between them; constant speed and
//! reverse; opacity and audio level with their keyframes; cross dissolves
//! and dips between two clips; and markers on the sequence and on clips.
//! Anything else (generators such as titles, other filters, nested
//! sequences) is named in the import report as not imported.
//!
//! Frames are counted at the sequence's rate on the timeline (`start`,
//! `end`) and at the clip item's own rate in its file (`in`, `out`, a
//! keyframe's `when`). A clip next to a transition has `start` or `end`
//! of -1: its edge is found from its length in the file and its speed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use klipp_core::{ClipId, Fps, Interpolation, Keyframe, MarkerColor, MarkerId, MediaId, RationalTime, Resolution, SequenceId};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipMarker, Marker, Sequence, Track, TrackKind};
use roxmltree::Node;

use crate::mlt_import::ImportResult;

/// Read the xmeml file at `path`.
pub fn load_xmeml(path: &Path) -> Result<ImportResult, String> {
    let bytes = crate::path_util::read_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES).map_err(|e| e.to_string())?;
    parse_xmeml(&String::from_utf8_lossy(&bytes))
}

/// The child element of `node` named `name`.
fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.is_element() && n.tag_name().name() == name)
}

/// The child elements of `node` named `name`.
fn children<'a, 'i: 'a>(node: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children().filter(move |n| n.is_element() && n.tag_name().name() == name)
}

/// The text of `node`'s child `name`, trimmed.
fn text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(node, name).and_then(|n| n.text()).map(str::trim)
}

fn int(node: Node<'_, '_>, name: &str) -> Option<i64> {
    text(node, name).and_then(|t| t.parse::<f64>().ok()).map(|v| v.round() as i64)
}

fn truthy(node: Node<'_, '_>, name: &str) -> Option<bool> {
    text(node, name).map(|t| t.eq_ignore_ascii_case("true"))
}

/// The rate `node` gives in its `<rate>`: a timebase, NTSC for its
/// thousand-and-first slower rate (24 is 23.976).
fn rate_of(node: Node<'_, '_>) -> Option<Fps> {
    let rate = child(node, "rate")?;
    // A timebase a frame rate can be, so neither cast nor product wraps;
    // the rate then goes through the importers' one check.
    let timebase = int(rate, "timebase").filter(|t| (1..=1000).contains(t))? as u32;
    let (num, den) = if truthy(rate, "ntsc").unwrap_or(false) { (timebase * 1000, 1001) } else { (timebase, 1) };
    Some(crate::path_util::sanitize_fps(num, den))
}

fn frames(n: i64, fps: Fps) -> RationalTime {
    RationalTime::from_frames(n, fps.rate())
}

/// A `pathurl` as a path: the `file://` scheme (with `localhost` or not)
/// taken off and every percent-escape decoded, UTF-8 included.
pub(crate) fn path_from_url(url: &str) -> PathBuf {
    let rest = url.strip_prefix("file://localhost").or_else(|| url.strip_prefix("file://")).unwrap_or(url);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8_lossy(&out).into_owned();
    // A Windows drive after the scheme's slash: /C:/... is C:/...
    let decoded = match decoded.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => decoded[1..].to_string(),
        _ => decoded,
    };
    crate::path_util::normalize_path(&PathBuf::from(decoded))
}

/// Every file the document describes, by its id, into the pool: a file
/// is described in full once and named by its id after.
fn index_files(root: Node<'_, '_>, pool: &mut MediaPool, fallback: Fps, warnings: &mut Vec<String>) -> HashMap<String, MediaId> {
    let mut files = HashMap::new();
    for file in root.descendants().filter(|n| n.is_element() && n.tag_name().name() == "file") {
        let Some(id) = file.attribute("id") else { continue };
        if files.contains_key(id) {
            continue;
        }
        let Some(url) = text(file, "pathurl").filter(|u| !u.is_empty()) else { continue };
        let path = path_from_url(url);
        let media_id = match pool.find_by_path(&path) {
            Some(existing) => existing.id,
            None => {
                let fps = rate_of(file).unwrap_or(fallback);
                let name = text(file, "name").map(str::to_string).unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                let duration = int(file, "duration").map_or(RationalTime::ZERO, |d| frames(d.max(0), fps));
                let item = crate::import_report::interchange_media_item(path.clone(), name, duration, fps);
                if item.offline {
                    warnings.push(format!("Media not found: {} — relink it", path.display()));
                }
                pool.add(item)
            }
        };
        files.insert(id.to_string(), media_id);
    }
    files
}

/// A clip item's filter `effectid` (or `name`) and the parameter `param`
/// of it: its value, and its keyframes as (frame, value).
struct Param {
    value: Option<f64>,
    keys: Vec<(i64, f64)>,
}

fn effects<'a, 'i: 'a>(item: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    children(item, "filter").filter_map(|f| child(f, "effect")).filter(|e| !text(*e, "enabled").is_some_and(|t| t.eq_ignore_ascii_case("false")))
}

fn effect_id(effect: Node<'_, '_>) -> String {
    text(effect, "effectid").or_else(|| text(effect, "name")).unwrap_or("").to_ascii_lowercase()
}

fn param(effect: Node<'_, '_>, id: &str) -> Option<Param> {
    let p = children(effect, "parameter").find(|p| text(*p, "parameterid").is_some_and(|t| t.eq_ignore_ascii_case(id)))?;
    let value = text(p, "value").and_then(|v| v.parse::<f64>().ok());
    let keys = children(p, "keyframe").filter_map(|k| Some((int(k, "when")?, text(k, "value")?.parse::<f64>().ok()?))).collect();
    Some(Param { value, keys })
}

fn keyframes(keys: &[(i64, f64)], from: i64, fps: Fps, scale: f64) -> Vec<Keyframe> {
    keys.iter()
        .map(|(when, v)| Keyframe { time_secs: (when - from).max(0) as f64 / fps.as_f64(), value: (v * scale) as f32, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() })
        .collect()
}

/// A transition on a track, in timeline frames, kept until its two clips
/// are known.
struct PendingTransition {
    start: i64,
    end: i64,
    alignment: String,
    name: String,
}

/// One track's clip items, transitions and generators, in order: its
/// clips, and links by clip item id.
#[allow(clippy::too_many_arguments)]
fn read_track(
    track_node: Node<'_, '_>,
    kind: TrackKind,
    name: String,
    seq_fps: Fps,
    files: &HashMap<String, MediaId>,
    links: &mut Vec<(ClipId, Vec<String>)>,
    item_ids: &mut HashMap<String, ClipId>,
    warnings: &mut Vec<String>,
) -> Track {
    let mut track = Track::new(&name, kind);
    if truthy(track_node, "enabled") == Some(false) {
        track.muted = true;
    }
    let mut pending = Vec::new();
    let mut cursor = 0i64;
    for item in track_node.children().filter(|n| n.is_element()) {
        match item.tag_name().name() {
            "clipitem" => {
                let item_name = text(item, "name").unwrap_or("clip").to_string();
                let Some(media_id) = child(item, "file").and_then(|f| f.attribute("id")).and_then(|id| files.get(id)).copied() else {
                    if child(item, "sequence").is_some() {
                        warnings.push(format!("Nested sequence '{item_name}' is not imported"));
                    } else {
                        warnings.push(format!("Clip '{item_name}' names no media file — not imported"));
                    }
                    continue;
                };
                let clip_fps = rate_of(item).unwrap_or(seq_fps);
                let (src_in, src_out) = (int(item, "in").unwrap_or(0).max(0), int(item, "out").unwrap_or(0).max(0));
                // Speed: the time remap's percent and reverse.
                let remap = effects(item).find(|e| effect_id(*e) == "timeremap");
                let percent = remap.as_ref().and_then(|e| param(*e, "speed")).and_then(|p| p.value).filter(|v| *v > 0.0).unwrap_or(100.0);
                let reverse = remap.as_ref().and_then(|e| param(*e, "reverse")).and_then(|p| p.value).is_some_and(|v| v != 0.0)
                    || remap.as_ref().and_then(|e| children(*e, "parameter").find(|p| text(*p, "parameterid") == Some("reverse"))).and_then(|p| text(p, "value")).is_some_and(|v| v.eq_ignore_ascii_case("true"));
                let speed = percent / 100.0;
                // Its length on the timeline, in timeline frames.
                let length = (((src_out - src_in) as f64 / clip_fps.as_f64()) / speed * seq_fps.as_f64()).round() as i64;
                let (start, end) = match (int(item, "start").unwrap_or(-1), int(item, "end").unwrap_or(-1)) {
                    (s, e) if s >= 0 && e >= 0 => (s, e),
                    (s, _) if s >= 0 => (s, s + length),
                    (_, e) if e >= 0 => (e - length, e),
                    _ => (cursor, cursor + length),
                };
                if end <= start {
                    warnings.push(format!("Clip '{item_name}' has no length — not imported"));
                    continue;
                }
                let mut clip = Clip::media(media_id, frames(start, seq_fps), frames(end, seq_fps), frames(src_in, clip_fps), frames(src_out, clip_fps));
                clip.speed = if reverse { -speed } else { speed };
                clip.enabled = truthy(item, "enabled").unwrap_or(true);
                cursor = end;
                for effect in effects(item) {
                    match effect_id(effect).as_str() {
                        "timeremap" => {}
                        "opacity" if kind == TrackKind::Video => {
                            if let Some(p) = param(effect, "opacity") {
                                let keys = if p.keys.is_empty() { p.value.map(|v| vec![(src_in, v)]).unwrap_or_default() } else { p.keys };
                                // A constant 100 % is the clip as it is.
                                if keys.iter().any(|(_, v)| (*v - 100.0).abs() > 1e-6) {
                                    if let Err(e) = crate::import_report::set_opacity_keys(&mut clip, &keyframes(&keys, src_in, clip_fps, 0.01)) {
                                        warnings.push(format!("Clip '{item_name}': opacity not imported ({e})"));
                                    }
                                }
                            }
                        }
                        "audiolevels" if kind == TrackKind::Audio => {
                            if let Some(p) = param(effect, "level") {
                                if p.keys.is_empty() {
                                    clip.volume = p.value.unwrap_or(1.0) as f32;
                                } else {
                                    clip.volume_keyframes = keyframes(&p.keys, src_in, clip_fps, 1.0);
                                }
                            }
                        }
                        other => {
                            let shown = text(effect, "name").unwrap_or(other);
                            warnings.push(format!("Filter '{shown}' on '{item_name}' is not imported"));
                        }
                    }
                }
                for marker in children(item, "marker") {
                    let at = int(marker, "in").unwrap_or(src_in);
                    clip.clip_markers.push(ClipMarker {
                        time_secs: (at - src_in).max(0) as f64 / clip_fps.as_f64(),
                        name: text(marker, "name").unwrap_or("").to_string(),
                        color: MarkerColor::Blue,
                        comment: text(marker, "comment").unwrap_or("").to_string(),
                    });
                }
                if let Some(id) = item.attribute("id") {
                    item_ids.insert(id.to_string(), clip.id);
                }
                let refs: Vec<String> = children(item, "link").filter_map(|l| text(l, "linkclipref").map(str::to_string)).collect();
                if !refs.is_empty() {
                    links.push((clip.id, refs));
                }
                track.clips.push(clip);
            }
            "transitionitem" => {
                let (Some(start), Some(end)) = (int(item, "start"), int(item, "end")) else { continue };
                let effect = child(item, "effect");
                let name = effect.and_then(|e| text(e, "name").or_else(|| text(e, "effectid"))).unwrap_or("transition").to_string();
                pending.push(PendingTransition { start, end, alignment: text(item, "alignment").unwrap_or("center").to_ascii_lowercase(), name });
            }
            "generatoritem" => {
                let shown = text(item, "name").unwrap_or("generator");
                warnings.push(format!("Generator '{shown}' (a title, colour matte or the like) is not imported"));
                if let Some(end) = int(item, "end") {
                    cursor = cursor.max(end);
                }
            }
            _ => {}
        }
    }
    track.clips.sort_by_key(|c| c.timeline_in);
    for t in pending {
        add_transition(&mut track, &t, seq_fps, warnings);
    }
    track
}

/// `t` between the clip that ends at its cut and the one that starts
/// there; a transition at a clip's edge alone (a fade from black) or of a
/// kind Klipp has no like of is named as not imported.
fn add_transition(track: &mut Track, t: &PendingTransition, fps: Fps, warnings: &mut Vec<String>) {
    use klipp_core::{TransitionAlignment, TransitionKind};
    let (cut, alignment) = match t.alignment.as_str() {
        "start" => (t.start, TransitionAlignment::StartAtCut),
        "end" => (t.end, TransitionAlignment::EndAtCut),
        "center" => ((t.start + t.end) / 2, TransitionAlignment::Center),
        _ => {
            warnings.push(format!("Transition '{}' at a clip's edge alone is not imported", t.name));
            return;
        }
    };
    let at = frames(cut, fps);
    let a = track.clips.iter().find(|c| c.timeline_out == at).map(|c| c.id);
    let b = track.clips.iter().find(|c| c.timeline_in == at).map(|c| c.id);
    let (Some(clip_a_id), Some(clip_b_id)) = (a, b) else {
        warnings.push(format!("Transition '{}' has no clip on one side — not imported", t.name));
        return;
    };
    let lower = t.name.to_ascii_lowercase();
    let kind = if lower.contains("cross dissolve") || lower == "dissolve" {
        TransitionKind::CrossDissolve
    } else if lower.contains("dip to color") || lower.contains("dip to black") {
        TransitionKind::DipToBlack
    } else if lower.contains("dip to white") {
        TransitionKind::DipToWhite
    } else {
        warnings.push(format!("Transition '{}' imported as a cross dissolve", t.name));
        TransitionKind::CrossDissolve
    };
    track.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        kind,
        duration: frames((t.end - t.start).max(1), fps),
        clip_a_id,
        clip_b_id,
        user_params: klipp_core::TransitionUserParams::default(),
        alignment,
    });
}



/// Parse an xmeml document.
pub fn parse_xmeml(xml: &str) -> Result<ImportResult, String> {
    let xml = crate::path_util::without_bare_doctype(xml);
    let doc = crate::path_util::parse_xml_safe(&xml).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    if root.tag_name().name() != "xmeml" {
        return Err(format!("the root element is <{}>, not <xmeml>", root.tag_name().name()));
    }
    let seq_node = root.descendants().find(|n| n.is_element() && n.tag_name().name() == "sequence").ok_or("no sequence in the file")?;
    let mut warnings = Vec::new();
    let seq_fps = rate_of(seq_node).unwrap_or_else(|| {
        warnings.push("The sequence gives no rate — 24 fps taken".to_string());
        Fps::film_24()
    });
    let drop_frame = child(seq_node, "timecode").and_then(|t| text(t, "displayformat")).map(|d| d.eq_ignore_ascii_case("DF"));
    let media = child(seq_node, "media");
    let video = media.and_then(|m| child(m, "video"));
    let audio = media.and_then(|m| child(m, "audio"));
    let size = video.and_then(|v| child(v, "format")).and_then(|f| child(f, "samplecharacteristics")).and_then(|s| Some((int(s, "width")?, int(s, "height")?)));
    let resolution = match size {
        Some((w, h)) => crate::path_util::sanitize_resolution(w.max(0) as u32, h.max(0) as u32),
        None => Resolution { width: 1920, height: 1080 },
    };

    let mut pool = MediaPool::new();
    let files = index_files(root, &mut pool, seq_fps, &mut warnings);
    let (mut links, mut item_ids) = (Vec::new(), HashMap::new());
    // Video tracks bottom first in the file, top first here; sound after.
    let mut tracks: Vec<Track> = Vec::new();
    let video_tracks: Vec<_> = video.map(|v| children(v, "track").collect()).unwrap_or_default();
    for (i, t) in video_tracks.iter().enumerate().rev() {
        tracks.push(read_track(*t, TrackKind::Video, format!("V{}", i + 1), seq_fps, &files, &mut links, &mut item_ids, &mut warnings));
    }
    for (i, t) in audio.map(|a| children(a, "track").collect::<Vec<_>>()).unwrap_or_default().into_iter().enumerate() {
        tracks.push(read_track(t, TrackKind::Audio, format!("A{}", i + 1), seq_fps, &files, &mut links, &mut item_ids, &mut warnings));
    }
    if tracks.iter().all(|t| t.kind != TrackKind::Video) {
        tracks.insert(0, Track::new("V1", TrackKind::Video));
    }
    // Picture and sound linked: each clip with the first other clip it
    // names that is of the other kind.
    let kind_of: HashMap<ClipId, TrackKind> = tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (c.id, t.kind))).collect();
    for (clip, refs) in links {
        let partner = refs.iter().filter_map(|r| item_ids.get(r)).find(|p| **p != clip && kind_of.get(*p) != kind_of.get(&clip));
        if let Some(partner) = partner.copied() {
            if let Some(c) = tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == clip) {
                c.linked_clip_id = Some(partner);
            }
        }
    }

    let markers = children(seq_node, "marker")
        .filter_map(|m| {
            let at = int(m, "in")?;
            let out = int(m, "out").filter(|o| *o > at);
            Some(Marker {
                review: None,
                id: MarkerId::new(),
                position: frames(at.max(0), seq_fps),
                label: text(m, "name").unwrap_or("").to_string(),
                color: MarkerColor::Blue,
                duration: out.map(|o| frames(o - at, seq_fps)),
                comment: text(m, "comment").unwrap_or("").to_string(),
                kind: klipp_core::MarkerKind::None,
            })
        })
        .collect();

    let sequence = Sequence {
        // No interchange format carries a camera.
        camera: klipp_timeline::Camera::default(),
        timeline_graph: None,
        grade_groups: Vec::new(),
        title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
        node_labels: Vec::new(),
        drop_frame: drop_frame.unwrap_or_else(|| Sequence::default_drop_frame(seq_fps)),
        id: SequenceId::new(),
        name: text(seq_node, "name").filter(|n| !n.is_empty()).unwrap_or("Imported").to_string(),
        fps: seq_fps,
        resolution,
        first_clip_autoset: Default::default(),
        tracks,
        in_point: None,
        out_point: None,
        markers,
        last_playhead: None,
        brief: None,
        edit_plan: None,
        reviews: Vec::new(),
        awaiting_refit: false,
        version_of: None,
    };
    Ok(ImportResult { sequence, media_pool: pool, warnings })
}

#[cfg(test)]
mod tests {

    /// A timebase past any frame rate gives no rate rather than a wrapped
    /// one; a real one reads as before.
    #[test]
    fn a_timebase_past_any_frame_rate_gives_no_rate() {
        let rate = |xml: &str| {
            let doc = crate::path_util::parse_xml_safe(xml).unwrap();
            rate_of(doc.root_element())
        };
        assert!(rate("<x><rate><timebase>4294967296</timebase></rate></x>").is_none());
        assert!(rate("<x><rate><timebase>5000000</timebase><ntsc>TRUE</ntsc></rate></x>").is_none());
        assert_eq!(rate("<x><rate><timebase>24</timebase><ntsc>TRUE</ntsc></rate></x>"), Some(Fps::new(24000, 1001)));
    }

    use super::*;

    fn at(seq: &Sequence, n: i64) -> RationalTime {
        frames(n, seq.fps)
    }

    /// Two picture tracks and two sound tracks at 23.976: the cut under a
    /// dissolve, a clip at half speed with an opacity fade, picture and
    /// sound linked, a level fade, a disabled clip, markers on the
    /// sequence and on a clip, and what is not read named.
    #[test]
    fn a_multitrack_fcp7_timeline_comes_across() {
        let result = parse_xmeml(include_str!("../tests/fixtures/xmeml_multitrack.xml")).unwrap();
        let seq = &result.sequence;
        assert_eq!((seq.name.as_str(), seq.fps, seq.resolution.width, seq.resolution.height), ("Edit 23.976", Fps::new(24000, 1001), 1920, 1080));
        let names: Vec<&str> = seq.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["V2", "V1", "A1", "A2"], "top picture track first, sound after");

        // V1: the cut under a centred dissolve, found from the clips'
        // lengths where their edges read -1.
        let v1 = &seq.tracks[1];
        let ranges: Vec<_> = v1.clips.iter().map(|c| (c.timeline_in, c.timeline_out)).collect();
        assert_eq!(ranges, [(at(seq, 0), at(seq, 48)), (at(seq, 48), at(seq, 120))]);
        assert_eq!(v1.clips[1].source_in, at(seq, 100));
        let t = &v1.transitions[0];
        assert_eq!((t.kind, t.duration, t.clip_a_id, t.clip_b_id), (klipp_core::TransitionKind::CrossDissolve, at(seq, 24), v1.clips[0].id, v1.clips[1].id));
        assert_eq!(t.alignment, klipp_core::TransitionAlignment::Center);
        let look = &v1.clips[1].clip_markers[0];
        assert_eq!((look.name.as_str(), look.comment.as_str()), ("Look", "her eyes"));
        assert!((look.time_secs - 10.0 * 1001.0 / 24000.0).abs() < 1e-9);

        // V2: half speed, fading in over its first second.
        let slow = &seq.tracks[0].clips[0];
        assert_eq!((slow.speed, slow.timeline_in, slow.timeline_out), (0.5, at(seq, 24), at(seq, 120)));
        let (fade, _) = slow.truth_lane(&klipp_core::ParamId::Opacity).expect("the picture reads");
        assert_eq!(fade.iter().map(|k| k.value).collect::<Vec<_>>(), [0.0, 1.0]);
        assert!((fade[1].time_secs - 24.0 * 1001.0 / 24000.0).abs() < 1e-6);

        // A1 linked to its picture, with a level fade; A2 off.
        let sound = &seq.tracks[2].clips[0];
        assert_eq!(sound.linked_clip_id, Some(v1.clips[0].id));
        assert_eq!(v1.clips[0].linked_clip_id, Some(sound.id));
        assert_eq!(sound.volume_keyframes.iter().map(|k| k.value).collect::<Vec<_>>(), [1.0, 0.5]);
        assert!(!seq.tracks[3].clips[0].enabled);

        // The files, decoded and offline; one item for a file used thrice.
        let paths: Vec<_> = result.media_pool.all().map(|m| (m.path.clone(), m.offline)).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&(PathBuf::from("/nowhere/A 001é.mov"), true)), "{paths:?}");
        assert!(paths.contains(&(PathBuf::from("/nowhere/B002.mov"), true)));

        assert_eq!((seq.markers[0].label.as_str(), seq.markers[0].comment.as_str(), seq.markers[0].position), ("Beat", "hit", at(seq, 30)));
        let warned = |s: &str| result.warnings.iter().any(|w| w.contains(s));
        assert!(warned("Generator 'Text'") && warned("Gaussian Blur") && warned("Media not found"), "{:?}", result.warnings);
    }

    /// 29.97 drop frame, and a clip with neither edge given that follows
    /// the one before it.
    #[test]
    fn drop_frame_and_clips_placed_after_each_other() {
        let xml = r#"<xmeml version="4"><sequence><name>DF</name>
            <rate><timebase>30</timebase><ntsc>TRUE</ntsc></rate>
            <timecode><displayformat>DF</displayformat></timecode>
            <media><video><track>
              <clipitem id="a"><start>0</start><end>30</end><in>0</in><out>30</out><file id="f"><pathurl>file:///nowhere/x.mov</pathurl><duration>300</duration></file></clipitem>
              <clipitem id="b"><start>-1</start><end>-1</end><in>30</in><out>90</out><file id="f"/></clipitem>
            </track></video></media></sequence></xmeml>"#;
        let seq = parse_xmeml(xml).unwrap().sequence;
        assert_eq!(seq.fps, Fps::new(30000, 1001));
        assert!(seq.drop_frame);
        let b = &seq.tracks[0].clips[1];
        assert_eq!((b.timeline_in, b.timeline_out), (at(&seq, 30), at(&seq, 90)));
    }

    #[test]
    fn pathurls_decode_every_escape() {
        assert_eq!(path_from_url("file://localhost/Volumes/Shoot/A%20B/%C3%A9t%C3%A9.mov"), PathBuf::from("/Volumes/Shoot/A B/été.mov"));
        assert_eq!(path_from_url("file:///home/me/clip%2B1.mov"), PathBuf::from("/home/me/clip+1.mov"));
        assert_eq!(path_from_url("file://localhost/C:/Media/a.mov"), PathBuf::from("C:/Media/a.mov"));
        assert!(parse_xmeml("<fcpxml/>").is_err());
        assert!(parse_xmeml("<xmeml version=\"5\"/>").unwrap_err().contains("no sequence"));
    }
}
