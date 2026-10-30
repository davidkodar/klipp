//! Kdenlive MLT XML project import.
//!
//! Parses `.kdenlive` files (MLT XML format) and produces a Klipp `Sequence`
//! plus a populated `MediaPool`. Unsupported features (filters, keyframes,
//! non-cross-dissolve transitions, audio routing) are dropped with warnings
//! that the caller is expected to write alongside the imported project.
//!
//! Scope is deliberately MVP-only: timeline structure, media references,
//! gaps, solid-color generators, basic title text, and cross-dissolve
//! transitions are preserved. Everything else is dropped and logged.
//!
//! The parser is read-only (uses `roxmltree`). Zero panic paths — all
//! errors are returned as `MltImportError`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use klipp_core::{Fps, Interpolation, Keyframe, RationalTime, Resolution};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// Result of a Kdenlive import operation.
#[derive(Debug)]
pub struct ImportResult {
    pub sequence: Sequence,
    pub media_pool: MediaPool,
    /// Human-readable warnings for features that were dropped or approximated.
    /// The caller typically writes these to `<name>.import_warnings.txt`.
    pub warnings: Vec<String>,
}

/// Error type for MLT import failures.
#[derive(Debug, thiserror::Error)]
pub enum MltImportError {
    #[error("Failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse MLT XML: {0}")]
    Xml(String),
    #[error("Missing required MLT element: {0}")]
    MissingElement(&'static str),
    #[error("No timeline found in MLT file")]
    NoTimeline,
}

/// Profile values extracted from `<profile>` — FPS and canvas dimensions.
#[derive(Debug, Clone, Copy)]
struct MltProfile {
    fps: Fps,
    width: u32,
    height: u32,
}

impl Default for MltProfile {
    fn default() -> Self {
        Self {
            fps: Fps::film_24(),
            width: 1920,
            height: 1080,
        }
    }
}

/// Parsed MLT document with producers, playlists, and tractors indexed by id.
/// Borrows from a `roxmltree::Document` that outlives the parser context.
struct MltDoc<'a> {
    profile: MltProfile,
    /// The parent directory of the .kdenlive file — used to resolve relative
    /// `resource` paths in avformat producers.
    #[allow(dead_code)] // used in later commits
    project_dir: PathBuf,
    producers: HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    playlists: HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    tractors: HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    /// ID of the top-level tractor (the main timeline). In MLT this is the
    /// *last* `<tractor>` in document order — inner tractors representing
    /// transitions come first.
    root_tractor_id: Option<&'a str>,
}

impl<'a> MltDoc<'a> {
    /// Walk the document, index all producers/playlists/tractors by `id`,
    /// and extract the profile. Identifies the root tractor.
    fn from_document(
        doc: &'a roxmltree::Document<'a>,
        project_dir: PathBuf,
    ) -> Result<Self, MltImportError> {
        let root = doc.root_element();
        if root.tag_name().name() != "mlt" {
            return Err(MltImportError::Xml(format!(
                "Root element is <{}>, expected <mlt>",
                root.tag_name().name()
            )));
        }

        let mut profile = MltProfile::default();
        let mut producers = HashMap::new();
        let mut playlists = HashMap::new();
        let mut tractors = HashMap::new();
        let mut root_tractor_id = None;

        for child in root.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "profile" => {
                    profile = parse_profile(child);
                }
                // Kdenlive 22+ uses <chain> for media sources (MLT 7's chain
                // concept wraps a producer + filters). Structurally identical
                // to <producer> for our purposes — index them in the same map.
                "producer" | "chain" => {
                    if let Some(id) = child.attribute("id") {
                        producers.insert(id, child);
                    }
                }
                "playlist" => {
                    if let Some(id) = child.attribute("id") {
                        playlists.insert(id, child);
                    }
                }
                "tractor" => {
                    if let Some(id) = child.attribute("id") {
                        tractors.insert(id, child);
                        // Last tractor in doc order = root timeline
                        root_tractor_id = Some(id);
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            profile,
            project_dir,
            producers,
            playlists,
            tractors,
            root_tractor_id,
        })
    }
}

/// Extract `<profile frame_rate_num frame_rate_den width height>`.
fn parse_profile(node: roxmltree::Node) -> MltProfile {
    let num = node
        .attribute("frame_rate_num")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(24);
    let den = node
        .attribute("frame_rate_den")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let width = node
        .attribute("width")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1920);
    let height = node
        .attribute("height")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1080);
    let sanitised_fps = crate::path_util::sanitize_fps(num, den);
    let sanitised_res = crate::path_util::sanitize_resolution(width, height);
    MltProfile {
        fps: sanitised_fps,
        width: sanitised_res.width,
        height: sanitised_res.height,
    }
}

/// Read a `<property name="X">value</property>` child by name.
/// Returns the first match or None.
#[allow(dead_code)] // used in later commits
fn get_property<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .filter(|n| n.is_element() && n.tag_name().name() == "property")
        .find(|n| n.attribute("name") == Some(name))
        .and_then(|n| n.text())
}

/// Parse an MLT time string, which may be either:
/// - An integer frame count (most common in melt output): `"24"` → 24 frames
/// - HH:MM:SS.mmm format: `"00:00:01.500"` → 1.5 seconds → floor(1.5 * fps) frames
///
/// Returns frames at the project FPS. None if the string can't be parsed.
fn parse_mlt_time(s: &str, fps: &Fps) -> Option<i64> {
    let s = s.trim();
    // Integer frame count — fast path
    if let Ok(frames) = s.parse::<i64>() {
        return Some(frames);
    }
    // HH:MM:SS.mmm or MM:SS.mmm
    let parts: Vec<&str> = s.split(':').collect();
    let secs: f64 = match parts.len() {
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let s: f64 = parts[2].parse().ok()?;
            h * 3600.0 + m * 60.0 + s
        }
        2 => {
            let m: f64 = parts[0].parse().ok()?;
            let s: f64 = parts[1].parse().ok()?;
            m * 60.0 + s
        }
        1 => parts[0].parse().ok()?,
        _ => return None,
    };
    Some((secs * fps.as_f64()).round() as i64)
}

/// Convert an MLT frame count to a `RationalTime` at the project FPS rate.
fn frames_to_rt(frames: i64, fps: &Fps) -> RationalTime {
    RationalTime::from_frames(frames, fps.rate())
}

/// Parse an MLT scalar keyframe string into a `Vec<Keyframe>`.
///
/// Format: `"0=0.0;50=0.5;100=1.0"` — semicolon-separated stops, each stop
/// `time_key=value`. The time key may be a frame number or `HH:MM:SS.mmm`
/// (delegated to `parse_mlt_time`). Times are returned as **source-relative
/// seconds** (matches `Keyframe::time_secs` contract and `EvalContext.clip_time_secs`).
///
/// All imported keyframes use `Interpolation::Linear` with zero tangents
/// (the MVP does not attempt to reconstruct Kdenlive's smooth curves).
///
/// An empty input returns `Ok(vec![])`. A malformed stop returns `Err`.
fn parse_scalar_keyframes(raw: &str, fps: &Fps) -> Result<Vec<Keyframe>, String> {
    let mut out = Vec::new();
    for stop in raw.split(';') {
        let stop = stop.trim();
        if stop.is_empty() {
            continue;
        }
        let (key, val) = stop
            .split_once('=')
            .ok_or_else(|| format!("malformed keyframe stop '{stop}' (expected 'time=value')"))?;
        let frames = parse_mlt_time(key.trim(), fps)
            .ok_or_else(|| format!("invalid keyframe time '{key}'"))?;
        let value: f32 = val
            .trim()
            .parse()
            .map_err(|_| format!("invalid keyframe value '{val}'"))?;
        let time_secs = frames as f64 / fps.as_f64();
        out.push(Keyframe {
            time_secs,
            value,
            interpolation: Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
            handles: Default::default(),
        });
    }
    Ok(out)
}

/// Four keyframe tracks decomposed from an MLT geometry string, plus any
/// per-stop warnings collected during parsing.
#[derive(Debug, Default)]
struct GeometryKeyframes {
    pos_x: Vec<Keyframe>,
    pos_y: Vec<Keyframe>,
    scale: Vec<Keyframe>,
    opacity: Vec<Keyframe>,
    /// One entry per stop where the w/h ratio doesn't match the sequence's
    /// aspect — the width-axis scale is used and the user is warned.
    scale_warnings: Vec<String>,
}

/// Parse an MLT geometry keyframe string into four decomposed tracks.
///
/// Format: `"time=x y w h a;time=x y w h a"` — semicolon-separated stops,
/// delimiter `=` (Kdenlive) or `~=`/`|=` (MLT interpolation hints, which
/// we normalise to Linear). Time may be a frame number or `HH:MM:SS.mmm`.
/// Each stop carries `x y w h alpha` where `x, y` are absolute pixels
/// (origin top-left), `w, h` are absolute pixel dimensions, and `a` is
/// alpha in `0.0..=1.0`. Scale is derived as `w / seq_width`; if
/// `w/seq_width` and `h/seq_height` disagree by more than `0.01`, a
/// warning is recorded and the width-axis value is used.
///
/// All emitted keyframes are `Linear` with zero tangents.
fn parse_geometry_keyframes(
    raw: &str,
    fps: &Fps,
    seq_width: u32,
    seq_height: u32,
) -> Result<GeometryKeyframes, String> {
    let mut gk = GeometryKeyframes::default();
    if seq_width == 0 || seq_height == 0 {
        return Err(format!(
            "invalid sequence dimensions {seq_width}x{seq_height}"
        ));
    }
    let seq_w = seq_width as f64;
    let seq_h = seq_height as f64;

    for stop in raw.split(';') {
        let stop = stop.trim();
        if stop.is_empty() {
            continue;
        }
        let (key, val) = stop
            .split_once('=')
            .ok_or_else(|| format!("malformed geometry stop '{stop}' (expected 'time=x y w h a')"))?;
        // Strip MLT interpolation-hint suffixes on the time key (e.g. "0~",
        // "0|", "0!"). We map all to Linear — the hint is informational.
        let key = key.trim().trim_end_matches(['~', '|', '!']);
        let frames = parse_mlt_time(key, fps)
            .ok_or_else(|| format!("invalid geometry time '{key}'"))?;
        let time_secs = frames as f64 / fps.as_f64();

        let parts: Vec<&str> = val.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(format!(
                "geometry stop '{stop}' has {} values (expected 5: x y w h a)",
                parts.len()
            ));
        }
        let x: f64 = parts[0]
            .parse()
            .map_err(|_| format!("invalid x '{}'", parts[0]))?;
        let y: f64 = parts[1]
            .parse()
            .map_err(|_| format!("invalid y '{}'", parts[1]))?;
        let w: f64 = parts[2]
            .parse()
            .map_err(|_| format!("invalid w '{}'", parts[2]))?;
        let h: f64 = parts[3]
            .parse()
            .map_err(|_| format!("invalid h '{}'", parts[3]))?;
        let a: f64 = parts[4]
            .parse()
            .map_err(|_| format!("invalid alpha '{}'", parts[4]))?;

        let scale_x = w / seq_w;
        let scale_y = h / seq_h;
        if (scale_x - scale_y).abs() > 0.01 {
            gk.scale_warnings.push(format!(
                "frame {frames}: non-uniform scale (x={scale_x:.3}, y={scale_y:.3}), using width-axis",
            ));
        }

        let kf = |value: f32| Keyframe {
            time_secs,
            value,
            interpolation: Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
            handles: Default::default(),
        };
        gk.pos_x.push(kf(x as f32));
        gk.pos_y.push(kf(y as f32));
        gk.scale.push(kf(scale_x as f32));
        gk.opacity.push(kf(a as f32));
    }

    Ok(gk)
}

/// Classification of an MLT `<producer>` element, used by the timeline walker
/// to decide how to build the corresponding Klipp clip.
#[derive(Debug, Clone, PartialEq)]
enum ProducerKind {
    /// A media file (video/audio) — `mlt_service="avformat"`.
    AvFormat { path: PathBuf },
    /// Blank gap — `mlt_service="blank"` or `resource="blank"`.
    Blank,
    /// Solid color producer — `mlt_service="color"`, `resource="0xRRGGBBAA"`.
    Color { rgba: [u8; 4] },
    /// Kdenlive title clip — `mlt_service="kdenlivetitle"`.
    KdenliveTitled {
        text: String,
        font: String,
        font_size: f32,
        color: [u8; 4],
    },
    /// A `<producer>` whose service we don't specifically handle. Upstream
    /// callers can decide to drop with a warning or fall back to Gap.
    Unknown { service: String },
}

/// Classify a `<producer>` element by its `mlt_service` property plus
/// any service-specific properties needed to fully identify it.
fn classify_producer(node: roxmltree::Node<'_, '_>, project_dir: &Path) -> ProducerKind {
    let service = get_property(node, "mlt_service").unwrap_or("");
    match service {
        // Kdenlive emits "avformat-novalidate" for chains (skips file probing
        // on open); semantically identical to "avformat" for import purposes.
        "avformat" | "avformat-novalidate" => {
            let resource = get_property(node, "resource").unwrap_or("");
            match resolve_resource_path(resource, project_dir) {
                Some(path) => ProducerKind::AvFormat { path },
                None => ProducerKind::Unknown { service: format!("blocked-path:{resource}") },
            }
        }
        "blank" => ProducerKind::Blank,
        "color" => {
            let resource = get_property(node, "resource").unwrap_or("0x000000ff");
            let rgba = parse_mlt_color(resource).unwrap_or([0, 0, 0, 255]);
            ProducerKind::Color { rgba }
        }
        "kdenlivetitle" => {
            // Kdenlive nests its own XML inside the xmldata property.
            let xmldata = get_property(node, "xmldata").unwrap_or("");
            let (text, font, font_size, color) = parse_kdenlivetitle_xmldata(xmldata);
            ProducerKind::KdenliveTitled { text, font, font_size, color }
        }
        other => ProducerKind::Unknown { service: other.to_string() },
    }
}

/// Resolve an MLT `resource` attribute to an absolute path, guarded against
/// path-traversal attacks. Returns None for relative paths with `..` that
/// escape the project directory; the caller marks the media offline.
fn resolve_resource_path(resource: &str, project_dir: &Path) -> Option<PathBuf> {
    crate::path_util::safe_resolve(project_dir, Path::new(resource))
}

/// Parse an MLT color string of form `0xRRGGBBAA` (hex RGBA) into `[u8; 4]`.
/// Accepts both with and without the `0x` prefix; returns None on parse failure.
fn parse_mlt_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
    Some([r, g, b, a])
}

/// Parse the nested `xmldata` XML from a kdenlivetitle producer.
/// Extracts text, font, font_size, and color. All fields fall back to
/// sensible defaults on parse failure.
///
/// Minimal recognized schema:
/// ```xml
/// <kdenlivetitle>
///   <item type="QGraphicsTextItem">
///     <content font="DejaVu Sans" font-size="72" color="255,255,255,255">Hello</content>
///   </item>
/// </kdenlivetitle>
/// ```
fn parse_kdenlivetitle_xmldata(xmldata: &str) -> (String, String, f32, [u8; 4]) {
    let default_font = "sans-serif".to_string();
    let default_size = 72.0_f32;
    let default_color = [255, 255, 255, 255];

    if xmldata.trim().is_empty() {
        return (String::new(), default_font, default_size, default_color);
    }

    let doc = match crate::path_util::parse_xml_safe(xmldata) {
        Ok(d) => d,
        Err(_) => return (String::new(), default_font, default_size, default_color),
    };

    // Walk for the first <content> element anywhere in the tree.
    let content = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "content");

    match content {
        Some(node) => {
            let text = node.text().unwrap_or("").to_string();
            let font = node
                .attribute("font")
                .map(str::to_string)
                .unwrap_or(default_font);
            let font_size = node
                .attribute("font-size")
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(default_size);
            let color = node
                .attribute("color")
                .and_then(parse_kdenlive_comma_color)
                .unwrap_or(default_color);
            (text, font, font_size, color)
        }
        None => (String::new(), default_font, default_size, default_color),
    }
}

/// Parse Kdenlive's `R,G,B,A` color format (each 0-255, comma-separated).
fn parse_kdenlive_comma_color(s: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return None;
    }
    Some([
        parts[0].parse::<u8>().ok()?,
        parts[1].parse::<u8>().ok()?,
        parts[2].parse::<u8>().ok()?,
        parts[3].parse::<u8>().ok()?,
    ])
}

/// Register a media file in the pool, or reuse an existing entry if the path
/// already exists. Returns the `MediaId`. Returns `None` if the file doesn't
/// exist on disk — caller should add a warning and skip the clip.
fn register_media(
    media_pool: &mut MediaPool,
    path: &Path,
    fps: &Fps,
    length_frames: Option<i64>,
) -> Option<klipp_core::MediaId> {
    if !path.exists() {
        return None;
    }
    if let Some(existing) = media_pool.find_by_path(path) {
        return Some(existing.id);
    }

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let duration = match length_frames {
        Some(n) => frames_to_rt(n, fps),
        None => RationalTime::ZERO,
    };

    let item = klipp_media::MediaItem {
        id: klipp_core::MediaId::new(),
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        filename,
        duration,
        fps: *fps,
        resolution: Resolution { width: 0, height: 0 },
        video_codec: String::new(),
        audio_codec: String::new(),
        has_audio: false, // will be probed later if needed
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
        dolby_vision_profile: None,
        log: Default::default(),
        synced_audio: None,
        markers: Vec::new(),
        screen_text: String::new(),
        audio_role: None,
        span: None,
    };
    Some(media_pool.add(item))
}

/// Walk a `<playlist>` and produce a Klipp `Track`. Accumulates a frame
/// cursor as it encounters `<entry>` and `<blank>` children so that each
/// clip's `timeline_in` / `timeline_out` is correctly positioned.
///
/// Entries that reference an inner tractor (a transition container) are
/// handled via `resolve_transition_entry` in a later commit; for now they
/// are flattened into a Gap placeholder so cursor math stays consistent.
fn resolve_playlist(
    playlist: roxmltree::Node<'_, '_>,
    doc: &MltDoc<'_>,
    media_pool: &mut MediaPool,
    warnings: &mut Vec<String>,
) -> Track {
    let profile = &doc.profile;
    let fps = &profile.fps;
    let track_kind = infer_track_kind(playlist, doc);
    let name = playlist
        .attribute("id")
        .unwrap_or("Imported Track")
        .to_string();
    let mut track = Track::new(&name, track_kind);

    let mut cursor: i64 = 0;

    for child in playlist.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "blank" => {
                let length = child
                    .attribute("length")
                    .and_then(|s| parse_mlt_time(s, fps))
                    .unwrap_or(0);
                if length > 0 {
                    let ti = frames_to_rt(cursor, fps);
                    let to = frames_to_rt(cursor + length, fps);
                    track.clips.push(Clip::gap(ti, to));
                }
                cursor += length;
            }
            "entry" => {
                let producer_id = match child.attribute("producer") {
                    Some(id) => id,
                    None => continue,
                };

                // MLT `out` is inclusive (last valid frame index). Klipp's
                // timeline_out is exclusive. So duration = out - in + 1.
                let src_in = child
                    .attribute("in")
                    .and_then(|s| parse_mlt_time(s, fps))
                    .unwrap_or(0);
                let src_out = child
                    .attribute("out")
                    .and_then(|s| parse_mlt_time(s, fps))
                    .unwrap_or(src_in);
                let duration = (src_out - src_in + 1).max(0);

                // If the producer ID points to an inner tractor, this entry
                // is a transition — walk the inner tractor, resolve both
                // underlying clips, and record a Klipp Transition if the
                // mlt_service maps to a known TransitionKind.
                if let Some(&inner) = doc.tractors.get(producer_id) {
                    resolve_transition_entry(
                        inner,
                        cursor,
                        duration,
                        profile,
                        doc,
                        media_pool,
                        &mut track,
                        warnings,
                    );
                    cursor += duration;
                    continue;
                }

                let producer_node = match doc.producers.get(producer_id) {
                    Some(n) => *n,
                    None => {
                        warnings.push(format!(
                            "Entry references unknown producer '{producer_id}' — skipped",
                        ));
                        continue;
                    }
                };

                if let Some(clip) = build_clip_from_entry(
                    producer_node,
                    src_in,
                    src_out,
                    cursor,
                    profile,
                    &doc.project_dir,
                    media_pool,
                    warnings,
                ) {
                    track.clips.push(clip);
                }
                cursor += duration;
            }
            _ => {
                // Filters on playlists: warning handled separately in commit 5.
            }
        }
    }

    track
}

/// Build a single Klipp `Clip` from an `<entry>` referencing a classified
/// producer. Returns `None` (and adds a warning) if the producer is a media
/// file whose path doesn't exist on disk.
fn build_clip_from_entry(
    producer: roxmltree::Node<'_, '_>,
    src_in: i64,
    src_out: i64,
    cursor: i64,
    profile: &MltProfile,
    project_dir: &Path,
    media_pool: &mut MediaPool,
    warnings: &mut Vec<String>,
) -> Option<Clip> {
    let fps = &profile.fps;
    let duration = (src_out - src_in + 1).max(0);
    let ti = frames_to_rt(cursor, fps);
    let to = frames_to_rt(cursor + duration, fps);
    let si = frames_to_rt(src_in, fps);
    let so = frames_to_rt(src_out + 1, fps); // +1: MLT out inclusive, Klipp exclusive

    // Start from a gap skeleton so we inherit all the default clip fields
    // (node_graph, keyframes, channel_config, etc.) without repeating them.
    let mut clip = Clip::gap(ti, to);
    clip.source_in = si;
    clip.source_out = so;

    // Constant speed property (speed ramps / timewarp keyframes are not
    // supported in MVP — user is warned separately).
    if let Some(speed_str) = get_property(producer, "speed") {
        if let Ok(speed) = speed_str.parse::<f64>() {
            if (speed - 1.0).abs() > f64::EPSILON {
                clip.speed = speed;
            }
        }
    }

    match classify_producer(producer, project_dir) {
        ProducerKind::AvFormat { path } => {
            // Try to glean length from producer's <property name="length">
            let length = get_property(producer, "length").and_then(|s| s.parse::<i64>().ok());
            let media_id = match register_media(media_pool, &path, fps, length) {
                Some(id) => id,
                None => {
                    warnings.push(format!(
                        "Media file not found: {} — clip skipped",
                        path.display(),
                    ));
                    return None;
                }
            };
            clip.kind = ClipKind::Media { media_id };
        }
        ProducerKind::Blank => {
            // Already a gap from Clip::gap above — nothing else to do.
        }
        ProducerKind::Color { rgba } => {
            clip.kind = ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: rgba });
        }
        ProducerKind::KdenliveTitled { text, font, font_size, color } => {
            let mut td = klipp_timeline::TitleData::default();
            if let Some(tb) = td.primary_text_mut() {
                tb.text = text;
                tb.font_name = font;
                tb.font_size = font_size;
                tb.color = color;
            }
            clip.kind = ClipKind::Title(td);
        }
        ProducerKind::Unknown { service } => {
            warnings.push(format!(
                "Producer service '{service}' not supported — imported as gap",
            ));
            // kind stays as Gap
        }
    }

    // Producer-level filters: parse the handful of services Klipp can map
    // onto keyframe tracks, warn on the rest. (Previously dropped silently.)
    apply_producer_filters(producer, profile, &mut clip, warnings);

    Some(clip)
}

/// Iterate `<filter>` children of a producer and dispatch the ones Klipp
/// can model onto keyframe tracks on `clip`. Unsupported filters are
/// recorded as warnings (one per filter instance — see `collect_filter_warnings`
/// for the collapsed tractor/playlist-level variant).
fn apply_producer_filters(
    producer: roxmltree::Node<'_, '_>,
    profile: &MltProfile,
    clip: &mut Clip,
    warnings: &mut Vec<String>,
) {
    let fps = &profile.fps;
    for filter in producer
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "filter")
    {
        // Skip filters Kdenlive flags as disabled — applying them would
        // override the clip's default transform with inactive data.
        if get_property(filter, "disable").map(|v| v.trim() == "1").unwrap_or(false) {
            tracing::debug!(
                service = get_property(filter, "mlt_service").unwrap_or("?"),
                "skipping disabled filter"
            );
            continue;
        }

        let service = get_property(filter, "mlt_service").unwrap_or("unknown");
        match service {
            "qtblend" => {
                apply_geometry_filter(filter, "rect", service, profile, clip, warnings);
                // Kdenlive carries rotation as a separate scalar-keyframe
                // property alongside `rect`. Degrees, same time-key format.
                if let Some(raw) = get_property(filter, "rotation") {
                    match parse_scalar_keyframes(raw, fps) {
                        Ok(kfs) => clip.edit_transform_rotation_keyframes(|v| *v = kfs),
                        Err(e) => warnings.push(format!(
                            "Filter 'qtblend' rotation parse failed: {e} — rotation dropped",
                        )),
                    }
                }
            }
            "affine" => {
                apply_geometry_filter(filter, "geometry", service, profile, clip, warnings);
            }
            "volume" => {
                if let Some(raw) = get_property(filter, "level") {
                    match parse_scalar_keyframes(raw, fps) {
                        Ok(kfs) => clip.volume_keyframes = kfs,
                        Err(e) => warnings.push(format!(
                            "Filter 'volume' level parse failed: {e} — filter dropped",
                        )),
                    }
                }
            }
            other => {
                warnings.push(format!(
                    "Producer filter '{other}' is not supported and was not imported. \
                     Re-apply it manually in Klipp.",
                ));
            }
        }
    }
}

/// Shared body for `qtblend` / `affine` — both carry a geometry string on a
/// known property name and map to the same four keyframe tracks.
fn apply_geometry_filter(
    filter: roxmltree::Node<'_, '_>,
    property: &str,
    service: &str,
    profile: &MltProfile,
    clip: &mut Clip,
    warnings: &mut Vec<String>,
) {
    let raw = match get_property(filter, property) {
        Some(r) => r,
        None => return,
    };
    match parse_geometry_keyframes(raw, &profile.fps, profile.width, profile.height) {
        Ok(gk) => {
            tracing::info!(
                service,
                property,
                pos_x = gk.pos_x.len(),
                scale = gk.scale.len(),
                opacity = gk.opacity.len(),
                "geometry keyframes imported"
            );
            clip.edit_transform_pos_x_keyframes(|v| *v = gk.pos_x);
            clip.edit_transform_pos_y_keyframes(|v| *v = gk.pos_y);
            clip.edit_transform_scale_keyframes(|v| *v = gk.scale);
            clip.edit_opacity_keyframes(|v| *v = gk.opacity);
            for msg in gk.scale_warnings {
                warnings.push(format!("Filter '{service}': {msg}"));
            }
        }
        Err(e) => warnings.push(format!(
            "Filter '{service}' {property} parse failed: {e} — filter dropped",
        )),
    }
}

/// Collect `<filter>` children from a node and emit one warning per
/// distinct `mlt_service`, with occurrence counts if repeated.
/// Klipp has no direct equivalent for most Kdenlive filters (brightness,
/// contrast, curves, etc.) — they are dropped with guidance for the user.
fn collect_filter_warnings(
    node: roxmltree::Node<'_, '_>,
    warnings: &mut Vec<String>,
) {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for child in node.children().filter(|n| n.is_element() && n.tag_name().name() == "filter") {
        let service = get_property(child, "mlt_service")
            .unwrap_or("unknown")
            .to_string();
        *counts.entry(service).or_insert(0) += 1;
    }
    for (service, count) in counts {
        if count == 1 {
            warnings.push(format!(
                "Effect '{service}' is not supported and was not imported. \
                 Re-apply it manually in Klipp."
            ));
        } else {
            warnings.push(format!(
                "Effect '{service}' was not imported ({count} occurrences). \
                 Re-apply manually in Klipp."
            ));
        }
    }
}

/// Map an MLT transition `mlt_service` to a Klipp `TransitionKind`.
/// Returns `None` for services Klipp doesn't model — caller should warn
/// and fall back to a straight cut.
fn mlt_service_to_transition_kind(service: &str) -> Option<klipp_core::TransitionKind> {
    match service {
        "luma" | "mix" | "composite" | "frei0r.cairoblend" => {
            Some(klipp_core::TransitionKind::CrossDissolve)
        }
        _ => None,
    }
}

/// Handle an `<entry producer="tractorN">` where `tractorN` is an inner
/// tractor representing a transition. The inner tractor holds two
/// `<track>` references to the outgoing and incoming producers, plus a
/// `<transition>` element describing the effect.
///
/// On success, pushes the two underlying clips to `track` and records a
/// `Transition` in `track.transitions` referencing them by ClipId.
///
/// On unsupported transition service, falls back to a straight cut (both
/// clips pushed, no transition recorded) and adds a warning.
fn resolve_transition_entry(
    inner_tractor: roxmltree::Node<'_, '_>,
    cursor: i64,
    duration: i64,
    profile: &MltProfile,
    doc: &MltDoc<'_>,
    media_pool: &mut MediaPool,
    track: &mut Track,
    warnings: &mut Vec<String>,
) {
    let fps = &profile.fps;
    // Find the two <track> child elements — these reference the A (outgoing)
    // and B (incoming) producers.
    let tracks: Vec<roxmltree::Node> = inner_tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
        .collect();
    if tracks.len() != 2 {
        warnings.push(format!(
            "Inner tractor has {} tracks (expected 2) — transition skipped",
            tracks.len()
        ));
        // Fall through as a gap so cursor stays aligned
        let ti = frames_to_rt(cursor, fps);
        let to = frames_to_rt(cursor + duration, fps);
        track.clips.push(Clip::gap(ti, to));
        return;
    }

    // Find the <transition> element, read its service
    let transition_el = inner_tractor
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "transition");
    let service = transition_el
        .and_then(|t| get_property(t, "mlt_service"))
        .unwrap_or("unknown");

    let kind = match mlt_service_to_transition_kind(service) {
        Some(k) => k,
        None => {
            warnings.push(format!(
                "Transition '{service}' not supported — replaced with cut",
            ));
            // Build both clips back-to-back, no Transition record
            build_transition_clips(&tracks, cursor, duration, profile, doc, media_pool, track, warnings, None);
            return;
        }
    };

    build_transition_clips(&tracks, cursor, duration, profile, doc, media_pool, track, warnings, Some(kind));
}

/// Shared helper: build the two clips referenced by an inner tractor's
/// `<track>` elements and optionally record a `Transition` linking them.
///
/// For a CrossDissolve: both clips span the full overlap window and the
/// transition duration equals the overlap. For a plain cut (kind=None):
/// clips are placed sequentially with no overlap — the total consumed
/// window is still `duration` so cursor math stays consistent.
#[allow(clippy::too_many_arguments)]
fn build_transition_clips(
    track_refs: &[roxmltree::Node<'_, '_>],
    cursor: i64,
    duration: i64,
    profile: &MltProfile,
    doc: &MltDoc<'_>,
    media_pool: &mut MediaPool,
    track: &mut Track,
    warnings: &mut Vec<String>,
    kind: Option<klipp_core::TransitionKind>,
) {
    let fps = &profile.fps;
    // Resolve both <track> references to their producer entries.
    let mut built_clip_ids: [Option<klipp_core::ClipId>; 2] = [None, None];

    for (idx, tr) in track_refs.iter().take(2).enumerate() {
        let pid = match tr.attribute("producer") {
            Some(p) => p,
            None => continue,
        };
        let producer = match doc.producers.get(pid) {
            Some(p) => *p,
            None => {
                warnings.push(format!(
                    "Transition track references unknown producer '{pid}' — skipped",
                ));
                continue;
            }
        };
        let src_in = tr
            .attribute("in")
            .and_then(|s| parse_mlt_time(s, fps))
            .unwrap_or(0);
        let src_out = tr
            .attribute("out")
            .and_then(|s| parse_mlt_time(s, fps))
            .unwrap_or(src_in);

        // For CrossDissolve both clips occupy the same timeline window.
        // For a fallback cut, place them back-to-back within the window.
        let (clip_start, clip_end) = match kind {
            Some(_) => (cursor, cursor + duration),
            None => {
                let half = duration / 2;
                if idx == 0 {
                    (cursor, cursor + half)
                } else {
                    (cursor + half, cursor + duration)
                }
            }
        };

        if let Some(clip) = build_clip_from_entry(
            producer,
            src_in,
            src_out,
            clip_start,
            profile,
            &doc.project_dir,
            media_pool,
            warnings,
        ) {
            let clip_id = clip.id;
            // Override timeline_out if this is a cross-dissolve (entry duration
            // may differ from the tractor's overlap window).
            let mut clip = clip;
            clip.timeline_in = frames_to_rt(clip_start, fps);
            clip.timeline_out = frames_to_rt(clip_end, fps);
            track.clips.push(clip);
            built_clip_ids[idx] = Some(clip_id);
        }
    }

    // Record the Transition if we have both clip IDs and a recognized kind.
    if let (Some(kind), Some(a_id), Some(b_id)) =
        (kind, built_clip_ids[0], built_clip_ids[1])
    {
        track.transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind,
            duration: frames_to_rt(duration, fps),
            clip_a_id: a_id,
            clip_b_id: b_id,
            user_params: klipp_core::TransitionUserParams::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
    }
}

/// Infer the track kind from a playlist's content. Checks an explicit
/// `kdenlive:track_type` property first, else walks the first few entries.
fn infer_track_kind(playlist: roxmltree::Node<'_, '_>, doc: &MltDoc<'_>) -> TrackKind {
    // Newer Kdenlive versions set an explicit track type property.
    if let Some(t) = get_property(playlist, "kdenlive:track_type") {
        return match t {
            "audio" => TrackKind::Audio,
            "subtitle" => TrackKind::Subtitle,
            _ => TrackKind::Video,
        };
    }

    // Otherwise, look at the first non-blank entry's producer kind.
    for entry in playlist.children().filter(|n| n.is_element() && n.tag_name().name() == "entry") {
        let pid = match entry.attribute("producer") {
            Some(p) => p,
            None => continue,
        };
        if let Some(prod) = doc.producers.get(pid) {
            match classify_producer(*prod, &doc.project_dir) {
                ProducerKind::KdenliveTitled { .. } => return TrackKind::Text,
                ProducerKind::Blank => continue,
                _ => return TrackKind::Video,
            }
        }
    }

    TrackKind::Video
}

/// Cap on tractor nesting depth when recursing through Kdenlive's main-
/// timeline container and its per-track-pair tractors. Real projects use
/// depth 2 (outer root -> UUID container -> track-pair tractors -> playlists).
const MAX_TRACTOR_DEPTH: u32 = 3;

/// Does this tractor hold a `<transition>` child? If so, it's a transition
/// wrapper (expected to live inside a playlist `<entry>`, not as a direct
/// track) — NOT a container we should recurse into.
fn tractor_has_transition(tractor: roxmltree::Node<'_, '_>) -> bool {
    tractor
        .children()
        .any(|n| n.is_element() && n.tag_name().name() == "transition")
}

/// A Kdenlive per-track-pair tractor holds exactly the two playlists that
/// make up one timeline track (main + transition-overlap). Heuristic: every
/// `<track>` child's `producer` attribute resolves to a known playlist.
fn is_track_pair_tractor(tractor: roxmltree::Node<'_, '_>, doc: &MltDoc<'_>) -> bool {
    let mut seen_any = false;
    for t in tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
    {
        seen_any = true;
        match t.attribute("producer") {
            Some(pid) if doc.playlists.contains_key(pid) => {}
            _ => return false,
        }
    }
    seen_any
}

/// Read the `hide` attribute from the `<track>` elements INSIDE a per-track-
/// pair tractor (Kdenlive puts it there, not on the referenced `<playlist>`).
/// `hide="video"` → audio is active (Audio track).
/// `hide="audio"` → video is active (Video track).
fn infer_track_kind_from_hide(tractor: roxmltree::Node<'_, '_>) -> Option<TrackKind> {
    let hide = tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
        .find_map(|t| t.attribute("hide"))?;
    match hide {
        "video" => Some(TrackKind::Audio),
        "audio" => Some(TrackKind::Video),
        _ => None,
    }
}

/// Resolve one Kdenlive per-track-pair tractor into a single Klipp `Track`.
///
/// Kdenlive wraps each timeline track in a tractor holding two playlists:
/// the primary carries the clips; the secondary is a "blank pair" used for
/// transition overlap windows. MVP: use the first playlist with `<entry>`
/// content as the clip source; warn if the secondary also has content (we
/// don't yet reconstruct Kdenlive's dual-playlist transition trick).
fn resolve_track_pair_tractor(
    tractor: roxmltree::Node<'_, '_>,
    doc: &MltDoc<'_>,
    media_pool: &mut MediaPool,
    warnings: &mut Vec<String>,
) -> Option<Track> {
    let playlist_ids: Vec<&str> = tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
        .filter_map(|t| t.attribute("producer"))
        .collect();
    if playlist_ids.is_empty() {
        return None;
    }

    let has_entries = |pid: &str| -> bool {
        doc.playlists
            .get(pid)
            .map(|pl| {
                pl.children()
                    .any(|n| n.is_element() && n.tag_name().name() == "entry")
            })
            .unwrap_or(false)
    };

    // Pick the first playlist with `<entry>` content; if none, fall back to
    // the first playlist (empty track).
    let primary_id = playlist_ids
        .iter()
        .copied()
        .find(|pid| has_entries(pid))
        .unwrap_or(playlist_ids[0]);
    let primary = doc.playlists.get(primary_id).copied()?;

    let mut track = resolve_playlist(primary, doc, media_pool, warnings);
    if let Some(kind) = infer_track_kind_from_hide(tractor) {
        track.kind = kind;
    }

    // Warn if any NON-primary playlist also has content.
    for pid in playlist_ids.iter().copied().filter(|p| *p != primary_id) {
        if has_entries(pid) {
            warnings.push(format!(
                "Track-pair tractor: secondary playlist '{pid}' has content \
                 (transition overlap not fully reconstructed)",
            ));
        }
    }

    Some(track)
}

/// Recurse into a container tractor (a tractor referenced directly as a
/// `<track>` child of another tractor, with no `<transition>` element).
///
/// Each child `<track>` either:
/// - references `main_bin` → skipped (Kdenlive's media-bin sentinel)
/// - references a plain `<producer>` → silently skipped (typically the
///   black-BG slot at the top of Kdenlive's UUID container)
/// - references another tractor that is a per-track-pair → built into a
///   single `Track` via `resolve_track_pair_tractor`
/// - references another container tractor → recurse
/// - references a playlist → handled via `resolve_playlist`
fn walk_container_tractor(
    tractor: roxmltree::Node<'_, '_>,
    doc: &MltDoc<'_>,
    media_pool: &mut MediaPool,
    sequence: &mut Sequence,
    warnings: &mut Vec<String>,
    visited: &mut HashSet<String>,
    depth: u32,
) {
    let tractor_id = tractor.attribute("id").unwrap_or("<no-id>");
    let track_count = tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
        .count();
    tracing::info!(
        tractor_id = %tractor_id,
        depth,
        track_count,
        "walk_container_tractor: enter"
    );

    if depth > MAX_TRACTOR_DEPTH {
        warnings.push(format!(
            "Tractor nesting exceeds depth {MAX_TRACTOR_DEPTH} — stopping recursion",
        ));
        return;
    }

    for track_ref in tractor
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "track")
    {
        let pid = match track_ref.attribute("producer") {
            Some(p) => p,
            None => continue,
        };
        tracing::debug!(pid = %pid, depth, "container <track>");

        if pid == "main_bin" {
            tracing::debug!(pid = %pid, "skipping main_bin");
            continue;
        }

        if doc.producers.contains_key(pid) {
            // Plain producer at this position is almost always Kdenlive's
            // black-BG slot. Silently skip.
            tracing::debug!(pid = %pid, "skipping plain producer (BG slot)");
            continue;
        }

        if let Some(playlist) = doc.playlists.get(pid).copied() {
            tracing::info!(pid = %pid, "container track: direct playlist → resolve_playlist");
            collect_filter_warnings(playlist, warnings);
            let track = resolve_playlist(playlist, doc, media_pool, warnings);
            sequence.tracks.push(track);
            continue;
        }

        let inner = match doc.tractors.get(pid) {
            Some(&t) => t,
            None => {
                tracing::warn!(pid = %pid, "container track: unknown producer");
                warnings.push(format!(
                    "Track references unknown producer '{pid}' — skipped",
                ));
                continue;
            }
        };

        if !visited.insert(pid.to_string()) {
            warnings.push(format!("Tractor cycle detected at '{pid}' — skipped"));
            continue;
        }

        // A <transition> child on a tractor reached via a <track> ref is
        // Kdenlive's track-level compositor — recurse anyway. Clip-to-clip
        // transition tractors reach the importer via playlist <entry>s.
        if tractor_has_transition(inner) {
            tracing::debug!(
                pid = %pid,
                "tractor has <transition> child (track-level compositor) — recursing anyway"
            );
        }

        let is_pair = is_track_pair_tractor(inner, doc);
        tracing::info!(
            pid = %pid,
            is_pair,
            depth,
            "container track: tractor dispatch"
        );
        if is_pair {
            if let Some(track) = resolve_track_pair_tractor(inner, doc, media_pool, warnings) {
                tracing::info!(
                    pid = %pid,
                    kind = ?track.kind,
                    clips = track.clips.len(),
                    "track-pair: resolved"
                );
                sequence.tracks.push(track);
            } else {
                tracing::warn!(pid = %pid, "track-pair: resolve returned None");
            }
        } else {
            walk_container_tractor(inner, doc, media_pool, sequence, warnings, visited, depth + 1);
        }
    }
}

/// Parse a Kdenlive .kdenlive (MLT XML) file into a Klipp sequence.
///
/// Never panics — all errors are returned as `MltImportError`. Warnings for
/// unsupported features (filters, non-cross-dissolve transitions, keyframes)
/// are collected in `ImportResult::warnings` for the caller to report.
pub fn load_kdenlive(path: &Path) -> Result<ImportResult, MltImportError> {
    tracing::info!(path = %path.display(), "MLT import: starting");
    let xml_text = crate::path_util::read_to_string_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES)?;
    let xml_text = crate::path_util::without_bare_doctype(&xml_text);
    let doc = crate::path_util::parse_xml_safe(&xml_text)
        .map_err(|e| MltImportError::Xml(e.to_string()))?;

    let project_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mlt = MltDoc::from_document(&doc, project_dir)?;

    if mlt.root_tractor_id.is_none() {
        return Err(MltImportError::NoTimeline);
    }

    // Scaffold for commits 2-5 — populate fully in later commits.
    let project_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported Project")
        .to_string();

    let mut sequence = Sequence {
        // No interchange format carries a camera.
        camera: klipp_timeline::Camera::default(),
        timeline_graph: None,
        grade_groups: Vec::new(),
        // An imported timeline takes its rate's convention.
        title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
        node_labels: Vec::new(),
        buses: Vec::new(),
        drop_frame: Sequence::default_drop_frame(mlt.profile.fps),
        id: klipp_core::SequenceId::new(),
        name: project_name,
        fps: mlt.profile.fps,
        resolution: Resolution {
            width: mlt.profile.width,
            height: mlt.profile.height,
        },
        first_clip_autoset: Default::default(),
        tracks: Vec::new(),
        in_point: None,
        out_point: None,
        markers: Vec::new(),
        last_playhead: None,
        brief: None,
        edit_plan: None,
        reviews: Vec::new(),
        awaiting_refit: false,
        version_of: None,
    };

    let mut media_pool = MediaPool::new();
    let mut warnings: Vec<String> = Vec::new();

    tracing::info!(
        producers = mlt.producers.len(),
        playlists = mlt.playlists.len(),
        tractors = mlt.tractors.len(),
        "MLT import: document indexed",
    );

    // Walk the root tractor's <track> children. Each references a playlist
    // OR another tractor (Kdenlive's main-timeline container shape).
    let root_id = mlt.root_tractor_id.expect("checked above");
    tracing::info!(root_id = %root_id, "MLT import: root tractor selected");
    let root_tractor = mlt.tractors.get(root_id).copied();
    if let Some(root) = root_tractor {
        // Filters on the root tractor apply globally — collect warnings.
        collect_filter_warnings(root, &mut warnings);

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(root_id.to_string());

        for track_ref in root.children().filter(|n| n.is_element() && n.tag_name().name() == "track") {
            let producer_id = match track_ref.attribute("producer") {
                Some(p) => p,
                None => continue,
            };

            tracing::debug!(producer_id = %producer_id, "root <track>");

            // Kdenlive's media bin is a playlist but is never a timeline track.
            if producer_id == "main_bin" {
                tracing::debug!("skipping main_bin");
                continue;
            }

            // Direct playlist reference — existing path.
            if let Some(playlist) = mlt.playlists.get(producer_id).copied() {
                tracing::info!(producer_id = %producer_id, "root track: playlist → resolve_playlist");
                collect_filter_warnings(playlist, &mut warnings);
                let track = resolve_playlist(playlist, &mlt, &mut media_pool, &mut warnings);
                sequence.tracks.push(track);
                continue;
            }

            // Tractor reference: the Kdenlive main-timeline container.
            // NOTE: a <transition> child on a tractor referenced here is
            // Kdenlive's track-level compositor (e.g. frei0r.cairoblend),
            // NOT a clip-to-clip transition — those only appear inside
            // playlist <entry> elements via resolve_transition_entry.
            if let Some(&inner) = mlt.tractors.get(producer_id) {
                if tractor_has_transition(inner) {
                    tracing::debug!(
                        producer_id = %producer_id,
                        "tractor has <transition> child (track-level compositor) — recursing anyway"
                    );
                }
                tracing::info!(producer_id = %producer_id, "root track: tractor → recurse");
                if !visited.insert(producer_id.to_string()) {
                    warnings.push(format!(
                        "Tractor cycle detected at '{producer_id}' — skipped",
                    ));
                    continue;
                }
                walk_container_tractor(
                    inner,
                    &mlt,
                    &mut media_pool,
                    &mut sequence,
                    &mut warnings,
                    &mut visited,
                    1,
                );
                continue;
            }

            warnings.push(format!(
                "Root track references unknown producer '{producer_id}' — skipped",
            ));
        }
    }

    tracing::info!(
        tracks = sequence.tracks.len(),
        total_clips = sequence.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
        warnings = warnings.len(),
        "MLT import: done"
    );
    for (i, t) in sequence.tracks.iter().enumerate() {
        tracing::info!(
            index = i,
            name = %t.name,
            kind = ?t.kind,
            clips = t.clips.len(),
            "MLT import: track summary"
        );
    }

    Ok(ImportResult {
        sequence,
        media_pool,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_TWO_CLIPS: &str = include_str!("../tests/fixtures/melt_two_clips.mlt");
    const FIXTURE_GEOMETRY_KEYFRAMES: &str =
        include_str!("../tests/fixtures/mlt_geometry_keyframes.mlt");
    const FIXTURE_NONUNIFORM_SCALE: &str =
        include_str!("../tests/fixtures/mlt_nonuniform_scale.mlt");
    const FIXTURE_SCALAR_KEYFRAMES: &str =
        include_str!("../tests/fixtures/mlt_scalar_keyframes.mlt");
    const FIXTURE_MAIN_TIMELINE: &str =
        include_str!("../tests/fixtures/mlt_kdenlive_main_timeline.mlt");

    #[test]
    fn loads_fixture_without_error() {
        // Write fixture to a temp file so load_kdenlive can open it via path.
        let tmp = std::env::temp_dir().join("klipp_mlt_test_two_clips.mlt");
        std::fs::write(&tmp, FIXTURE_TWO_CLIPS).unwrap();
        let result = load_kdenlive(&tmp).expect("import should succeed");
        assert_eq!(result.sequence.fps, Fps::new(24, 1));
        assert_eq!(result.sequence.resolution.width, 320);
        assert_eq!(result.sequence.resolution.height, 240);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn mlt_doc_indexes_producers_playlists_tractors() {
        let doc = roxmltree::Document::parse(FIXTURE_TWO_CLIPS).unwrap();
        let mlt = MltDoc::from_document(&doc, PathBuf::from("/tmp")).unwrap();

        // The fixture has 3 producers (two media clips + one blank),
        // 1 playlist, 2 tractors (inner for transition + outer root).
        assert_eq!(mlt.producers.len(), 3);
        assert_eq!(mlt.playlists.len(), 1);
        assert_eq!(mlt.tractors.len(), 2);
        assert!(mlt.root_tractor_id.is_some());
    }

    #[test]
    fn profile_parsed_from_fixture() {
        let doc = roxmltree::Document::parse(FIXTURE_TWO_CLIPS).unwrap();
        let mlt = MltDoc::from_document(&doc, PathBuf::from("/tmp")).unwrap();
        assert_eq!(mlt.profile.fps, Fps::new(24, 1));
        assert_eq!(mlt.profile.width, 320);
        assert_eq!(mlt.profile.height, 240);
    }

    #[test]
    fn no_tractor_returns_no_timeline_error() {
        let minimal = r#"<?xml version="1.0"?><mlt version="7.0"><profile frame_rate_num="24" frame_rate_den="1"/></mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_no_tractor.mlt");
        std::fs::write(&tmp, minimal).unwrap();
        let err = load_kdenlive(&tmp).expect_err("should fail with NoTimeline");
        assert!(matches!(err, MltImportError::NoTimeline));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn malformed_xml_returns_xml_error() {
        let tmp = std::env::temp_dir().join("klipp_mlt_test_bad.mlt");
        std::fs::write(&tmp, "<mlt><unclosed").unwrap();
        let err = load_kdenlive(&tmp).expect_err("should fail with Xml");
        assert!(matches!(err, MltImportError::Xml(_)));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn parse_mlt_time_integer_frames() {
        let fps = Fps::new(24, 1);
        assert_eq!(parse_mlt_time("24", &fps), Some(24));
        assert_eq!(parse_mlt_time("  100 ", &fps), Some(100));
        assert_eq!(parse_mlt_time("0", &fps), Some(0));
    }

    #[test]
    fn parse_mlt_time_hms() {
        let fps = Fps::new(24, 1);
        // 1 second at 24fps = 24 frames
        assert_eq!(parse_mlt_time("00:00:01.000", &fps), Some(24));
        // 1 minute = 60 seconds = 1440 frames
        assert_eq!(parse_mlt_time("00:01:00.000", &fps), Some(1440));
    }

    #[test]
    fn parse_mlt_time_invalid() {
        let fps = Fps::new(24, 1);
        assert_eq!(parse_mlt_time("", &fps), None);
        assert_eq!(parse_mlt_time("garbage", &fps), None);
    }

    #[test]
    fn parse_scalar_keyframes_basic() {
        let fps = Fps::new(25, 1);
        let kfs = parse_scalar_keyframes("0=0.0;50=0.5;100=1.0", &fps)
            .expect("basic 3-stop parse");
        assert_eq!(kfs.len(), 3);

        assert!((kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((kfs[0].value - 0.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 2.0).abs() < 1e-9); // 50 frames / 25 fps
        assert!((kfs[1].value - 0.5).abs() < 1e-6);
        assert!((kfs[2].time_secs - 4.0).abs() < 1e-9); // 100 frames / 25 fps
        assert!((kfs[2].value - 1.0).abs() < 1e-6);

        // All imports are Linear with zero tangents.
        for kf in &kfs {
            assert_eq!(kf.interpolation, Interpolation::Linear);
            assert_eq!(kf.tangent_out, (0.0, 0.0));
            assert_eq!(kf.tangent_in, (0.0, 0.0));
        }
    }

    #[test]
    fn parse_scalar_keyframes_single_stop() {
        let fps = Fps::new(25, 1);
        let kfs = parse_scalar_keyframes("42=0.25", &fps)
            .expect("single-stop parse");
        assert_eq!(kfs.len(), 1);
        assert!((kfs[0].time_secs - (42.0 / 25.0)).abs() < 1e-9);
        assert!((kfs[0].value - 0.25).abs() < 1e-6);
    }

    #[test]
    fn parse_geometry_keyframes_uniform() {
        let fps = Fps::new(25, 1);
        // 1920x1080 sequence, zoom-in-then-out with uniform aspect.
        let gk = parse_geometry_keyframes(
            "0~=0 0 1920 1080 1;25~=480 270 960 540 1;49~=0 0 1920 1080 1",
            &fps,
            1920,
            1080,
        )
        .expect("uniform geometry parse");

        assert!(gk.scale_warnings.is_empty(), "unexpected warnings: {:?}", gk.scale_warnings);
        assert_eq!(gk.pos_x.len(), 3);
        assert_eq!(gk.pos_y.len(), 3);
        assert_eq!(gk.scale.len(), 3);
        assert_eq!(gk.opacity.len(), 3);

        // Middle stop: frame 25 → 1.0s, pos (480, 270), scale 0.5, alpha 1.0
        assert!((gk.pos_x[1].time_secs - 1.0).abs() < 1e-9);
        assert!((gk.pos_x[1].value - 480.0).abs() < 1e-4);
        assert!((gk.pos_y[1].value - 270.0).abs() < 1e-4);
        assert!((gk.scale[1].value - 0.5).abs() < 1e-6);
        assert!((gk.opacity[1].value - 1.0).abs() < 1e-6);

        // Endpoints full-frame.
        assert!((gk.scale[0].value - 1.0).abs() < 1e-6);
        assert!((gk.scale[2].value - 1.0).abs() < 1e-6);
        assert_eq!(gk.pos_x[0].interpolation, Interpolation::Linear);
    }

    #[test]
    fn parse_geometry_keyframes_nonuniform_scale() {
        let fps = Fps::new(25, 1);
        // w/W = 1.0 but h/H = 0.5 → non-uniform, width-axis wins.
        let gk = parse_geometry_keyframes("0~=0 0 1920 540 1", &fps, 1920, 1080)
            .expect("non-uniform parse");

        assert_eq!(gk.scale_warnings.len(), 1);
        assert!(
            gk.scale_warnings[0].contains("non-uniform"),
            "unexpected warning text: {}",
            gk.scale_warnings[0]
        );
        assert!((gk.scale[0].value - 1.0).abs() < 1e-6, "should use width-axis (1.0), got {}", gk.scale[0].value);
    }

    #[test]
    fn parse_geometry_keyframes_kdenlive_format() {
        // Real Kdenlive uses plain `=` delimiter (not `~=`) and HH:MM:SS times.
        // Sample taken directly from a testar.kdenlive file.
        let fps = Fps::new(25, 1);
        let gk = parse_geometry_keyframes(
            "00:00:00.000=0 0 1920 1080 1.000000;\
             00:00:07.560=0 0 1920 1080 1.000000;\
             00:01:05.160=-2946 -641 4263 2398 1.000000",
            &fps,
            1920,
            1080,
        )
        .expect("Kdenlive-format geometry parse");
        assert_eq!(gk.pos_x.len(), 3);
        assert!((gk.pos_x[0].time_secs - 0.0).abs() < 1e-6);
        assert!((gk.pos_x[1].time_secs - 7.56).abs() < 1e-6);
        assert!((gk.pos_x[2].value - (-2946.0)).abs() < 1e-3);
        assert!((gk.opacity[0].value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn parse_geometry_keyframes_alpha() {
        let fps = Fps::new(25, 1);
        let gk = parse_geometry_keyframes("0~=0 0 1920 1080 0.5", &fps, 1920, 1080)
            .expect("alpha parse");
        assert_eq!(gk.opacity.len(), 1);
        assert!((gk.opacity[0].value - 0.5).abs() < 1e-6);
    }

    #[test]
    fn frames_to_rt_integer_fps() {
        let fps = Fps::new(24, 1);
        let rt = frames_to_rt(24, &fps);
        assert_eq!(rt.value, 24);
        assert_eq!(rt.rate, 24);
    }

    #[test]
    fn frames_to_rt_fractional_fps() {
        // 24000/1001 → rate() returns 24000 (preserves frame-accurate timing)
        let fps = Fps::ntsc_23976();
        let rt = frames_to_rt(24000, &fps);
        assert_eq!(rt.rate, 24000);
        assert_eq!(rt.value, 24000);
    }

    // ── Producer classification ─────────────────────────────────────

    /// Helper: wrap a producer snippet in a minimal MLT envelope and parse it.
    fn wrap_and_parse(producer_inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <producer id="p1">
    {producer_inner}
  </producer>
  <tractor id="root"><track producer="p1"/></tractor>
</mlt>"#
        )
    }

    fn classify_from_snippet(producer_inner: &str) -> ProducerKind {
        let xml = wrap_and_parse(producer_inner);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let producer = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "producer")
            .unwrap();
        classify_producer(producer, Path::new("/tmp"))
    }

    #[test]
    fn classify_avformat_producer() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">avformat</property>
               <property name="resource">clip.mp4</property>"#,
        );
        match kind {
            ProducerKind::AvFormat { path } => {
                assert_eq!(path, PathBuf::from("/tmp/clip.mp4"));
            }
            other => panic!("expected AvFormat, got {other:?}"),
        }
    }

    #[test]
    fn classify_avformat_absolute_path_unchanged() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">avformat</property>
               <property name="resource">/media/clip.mp4</property>"#,
        );
        match kind {
            ProducerKind::AvFormat { path } => {
                assert_eq!(path, PathBuf::from("/media/clip.mp4"));
            }
            other => panic!("expected AvFormat, got {other:?}"),
        }
    }

    #[test]
    fn classify_blank_producer() {
        let kind = classify_from_snippet(r#"<property name="mlt_service">blank</property>"#);
        assert_eq!(kind, ProducerKind::Blank);
    }

    #[test]
    fn classify_color_producer() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">color</property>
               <property name="resource">0x0000ffff</property>"#,
        );
        assert_eq!(kind, ProducerKind::Color { rgba: [0, 0, 255, 255] });
    }

    #[test]
    fn classify_color_producer_default_on_bad_hex() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">color</property>
               <property name="resource">not-a-hex</property>"#,
        );
        assert_eq!(kind, ProducerKind::Color { rgba: [0, 0, 0, 255] });
    }

    #[test]
    fn classify_kdenlivetitle_producer() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">kdenlivetitle</property>
               <property name="xmldata"><![CDATA[<kdenlivetitle><item><content font="Arial" font-size="48" color="255,128,0,255">Hi</content></item></kdenlivetitle>]]></property>"#,
        );
        match kind {
            ProducerKind::KdenliveTitled { text, font, font_size, color } => {
                assert_eq!(text, "Hi");
                assert_eq!(font, "Arial");
                assert!((font_size - 48.0).abs() < 0.001);
                assert_eq!(color, [255, 128, 0, 255]);
            }
            other => panic!("expected KdenliveTitled, got {other:?}"),
        }
    }

    #[test]
    fn classify_kdenlivetitle_missing_xmldata_gives_defaults() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">kdenlivetitle</property>"#,
        );
        match kind {
            ProducerKind::KdenliveTitled { text, font, font_size, color } => {
                assert!(text.is_empty());
                assert_eq!(font, "sans-serif");
                assert!((font_size - 72.0).abs() < 0.001);
                assert_eq!(color, [255, 255, 255, 255]);
            }
            other => panic!("expected KdenliveTitled, got {other:?}"),
        }
    }

    #[test]
    fn classify_unknown_service() {
        let kind = classify_from_snippet(
            r#"<property name="mlt_service">weird_service_123</property>"#,
        );
        assert_eq!(
            kind,
            ProducerKind::Unknown { service: "weird_service_123".to_string() },
        );
    }

    // ── Color hex parsing ───────────────────────────────────────────

    #[test]
    fn parse_mlt_color_valid() {
        assert_eq!(parse_mlt_color("0x000000ff"), Some([0, 0, 0, 255]));
        assert_eq!(parse_mlt_color("0xff0000ff"), Some([255, 0, 0, 255]));
        assert_eq!(parse_mlt_color("0xffffffff"), Some([255, 255, 255, 255]));
        // Without 0x prefix
        assert_eq!(parse_mlt_color("808080ff"), Some([128, 128, 128, 255]));
    }

    #[test]
    fn parse_mlt_color_invalid() {
        assert_eq!(parse_mlt_color("short"), None);
        assert_eq!(parse_mlt_color("0xzzzzzzzz"), None);
        assert_eq!(parse_mlt_color(""), None);
    }

    // ── Kdenlive comma-color parsing ────────────────────────────────

    #[test]
    fn parse_kdenlive_comma_color_ok() {
        assert_eq!(
            parse_kdenlive_comma_color("255,0,128,255"),
            Some([255, 0, 128, 255]),
        );
    }

    #[test]
    fn parse_kdenlive_comma_color_too_few() {
        assert_eq!(parse_kdenlive_comma_color("255,0,128"), None);
    }

    // ── Resource path resolution ────────────────────────────────────

    #[test]
    fn relative_resource_path_resolves_against_project_dir() {
        let resolved = resolve_resource_path("clip.mp4", Path::new("/home/user/project"));
        assert_eq!(resolved, Some(PathBuf::from("/home/user/project/clip.mp4")));
    }

    #[test]
    fn absolute_resource_path_unchanged() {
        let resolved = resolve_resource_path("/media/clip.mp4", Path::new("/home/user"));
        assert_eq!(resolved, Some(PathBuf::from("/media/clip.mp4")));
    }

    #[test]
    fn traversal_resource_path_rejected() {
        let resolved = resolve_resource_path("../../etc/passwd", Path::new("/home/user/project"));
        assert_eq!(resolved, None);
    }

    // ── Playlist / track mapping ────────────────────────────────────

    fn simple_playlist_xml(entries_xml: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <producer id="media1" in="0" out="48">
    <property name="mlt_service">avformat</property>
    <property name="resource">/nonexistent/clip.mp4</property>
    <property name="length">49</property>
  </producer>
  <producer id="col1" in="0" out="23">
    <property name="mlt_service">color</property>
    <property name="resource">0xff0000ff</property>
  </producer>
  <playlist id="pl">{entries_xml}</playlist>
  <tractor id="root">
    <track producer="pl"/>
  </tractor>
</mlt>"#
        )
    }

    #[test]
    fn blank_in_playlist_creates_gap() {
        let xml = simple_playlist_xml(r#"<blank length="12"/>"#);
        let tmp = std::env::temp_dir().join("klipp_mlt_test_blank.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(result.sequence.tracks.len(), 1);
        let track = &result.sequence.tracks[0];
        assert_eq!(track.clips.len(), 1);
        assert!(matches!(track.clips[0].kind, ClipKind::Gap));
        // Gap duration: 12 frames at 24fps
        assert_eq!(track.clips[0].timeline_in.value, 0);
        assert_eq!(track.clips[0].timeline_out.value, 12);
    }

    #[test]
    fn entry_cursor_accumulation() {
        // entry(24 frames) + blank(3) + entry(10 frames) — positions: 0, 24+3=27
        // Both entries point to a color producer (exists in memory, no disk check).
        let xml = simple_playlist_xml(
            r#"<entry producer="col1" in="0" out="23"/>
               <blank length="3"/>
               <entry producer="col1" in="0" out="9"/>"#,
        );
        let tmp = std::env::temp_dir().join("klipp_mlt_test_cursor.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let track = &result.sequence.tracks[0];
        assert_eq!(track.clips.len(), 3);
        // First entry: 0..24
        assert_eq!(track.clips[0].timeline_in.value, 0);
        assert_eq!(track.clips[0].timeline_out.value, 24);
        // Gap: 24..27
        assert!(matches!(track.clips[1].kind, ClipKind::Gap));
        assert_eq!(track.clips[1].timeline_in.value, 24);
        assert_eq!(track.clips[1].timeline_out.value, 27);
        // Second entry: 27..37
        assert_eq!(track.clips[2].timeline_in.value, 27);
        assert_eq!(track.clips[2].timeline_out.value, 37);
    }

    #[test]
    fn mlt_inclusive_out_converted_to_klipp_exclusive() {
        // MLT out=23 means frame 23 is the last valid frame → duration 24,
        // so Klipp source_out must be 24.
        let xml = simple_playlist_xml(r#"<entry producer="col1" in="0" out="23"/>"#);
        let tmp = std::env::temp_dir().join("klipp_mlt_test_inclusive.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let clip = &result.sequence.tracks[0].clips[0];
        assert_eq!(clip.source_in.value, 0);
        assert_eq!(clip.source_out.value, 24);
    }

    #[test]
    fn missing_media_produces_warning_and_skips() {
        let xml = simple_playlist_xml(
            r#"<entry producer="media1" in="0" out="23"/>"#,
        );
        let tmp = std::env::temp_dir().join("klipp_mlt_test_missing.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        // The clip was skipped (no track pushes it); warning recorded.
        assert!(
            result.warnings.iter().any(|w| w.contains("not found")),
            "expected a missing-media warning, got {:?}", result.warnings
        );
    }

    #[test]
    fn color_producer_entry_creates_generator_clip() {
        let xml = simple_playlist_xml(r#"<entry producer="col1" in="0" out="23"/>"#);
        let tmp = std::env::temp_dir().join("klipp_mlt_test_color.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let clip = &result.sequence.tracks[0].clips[0];
        match &clip.kind {
            ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color }) => {
                assert_eq!(*color, [255, 0, 0, 255]);
            }
            other => panic!("expected SolidColor generator, got {other:?}"),
        }
    }

    #[test]
    fn kdenlive_track_type_property_overrides_inference() {
        let xml = r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <playlist id="pl">
    <property name="kdenlive:track_type">audio</property>
  </playlist>
  <tractor id="root"><track producer="pl"/></tractor>
</mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_track_type.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(result.sequence.tracks[0].kind, TrackKind::Audio);
    }

    // ── Transition resolution ───────────────────────────────────────

    #[test]
    fn mlt_service_to_transition_kind_map() {
        assert_eq!(
            mlt_service_to_transition_kind("luma"),
            Some(klipp_core::TransitionKind::CrossDissolve)
        );
        assert_eq!(
            mlt_service_to_transition_kind("mix"),
            Some(klipp_core::TransitionKind::CrossDissolve)
        );
        assert_eq!(
            mlt_service_to_transition_kind("composite"),
            Some(klipp_core::TransitionKind::CrossDissolve)
        );
        assert_eq!(mlt_service_to_transition_kind("matte"), None);
        assert_eq!(mlt_service_to_transition_kind("unknown_service"), None);
    }

    /// Minimal MLT with two color producers + a luma transition between them.
    /// Uses color producers (no file dependency) to exercise the clip-building
    /// path without needing real media on disk.
    fn mlt_with_luma_transition(service: &str) -> String {
        format!(r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <producer id="colA" in="0" out="47">
    <property name="mlt_service">color</property>
    <property name="resource">0xff0000ff</property>
  </producer>
  <producer id="colB" in="0" out="47">
    <property name="mlt_service">color</property>
    <property name="resource">0x00ff00ff</property>
  </producer>
  <tractor id="transition_wrap" in="0" out="9">
    <track producer="colA" in="38" out="47"/>
    <track producer="colB" in="0" out="9"/>
    <transition id="t0" out="9">
      <property name="a_track">0</property>
      <property name="b_track">1</property>
      <property name="mlt_service">{service}</property>
    </transition>
  </tractor>
  <playlist id="pl">
    <entry producer="colA" in="0" out="37"/>
    <entry producer="transition_wrap" in="0" out="9"/>
    <entry producer="colB" in="10" out="47"/>
  </playlist>
  <tractor id="root">
    <track producer="pl"/>
  </tractor>
</mlt>"#)
    }

    #[test]
    fn luma_transition_creates_crossdissolve() {
        let xml = mlt_with_luma_transition("luma");
        let tmp = std::env::temp_dir().join("klipp_mlt_test_luma.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let track = &result.sequence.tracks[0];
        // Should have: colA entry, two overlap clips (A'+B' from transition wrap), colB entry
        // i.e. 4 clips total because transition adds both A and B clips in overlap region
        assert!(track.clips.len() >= 3, "got {} clips", track.clips.len());
        // One transition recorded
        assert_eq!(track.transitions.len(), 1);
        assert_eq!(
            track.transitions[0].kind,
            klipp_core::TransitionKind::CrossDissolve
        );
        // Transition duration = 10 frames (out=9, inclusive → 10 frames)
        assert_eq!(track.transitions[0].duration.value, 10);
    }

    #[test]
    fn unsupported_transition_falls_back_to_cut_with_warning() {
        let xml = mlt_with_luma_transition("matte");
        let tmp = std::env::temp_dir().join("klipp_mlt_test_matte.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        // No transition recorded — clips still present as plain cut
        assert_eq!(result.sequence.tracks[0].transitions.len(), 0);
        assert!(
            result.warnings.iter().any(|w| w.contains("matte")),
            "expected warning containing 'matte', got {:?}", result.warnings
        );
    }

    #[test]
    fn transition_references_existing_clip_ids() {
        let xml = mlt_with_luma_transition("luma");
        let tmp = std::env::temp_dir().join("klipp_mlt_test_ids.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let track = &result.sequence.tracks[0];
        let trans = &track.transitions[0];
        let a_exists = track.clips.iter().any(|c| c.id == trans.clip_a_id);
        let b_exists = track.clips.iter().any(|c| c.id == trans.clip_b_id);
        assert!(a_exists, "clip_a_id not found in track.clips");
        assert!(b_exists, "clip_b_id not found in track.clips");
        assert_ne!(trans.clip_a_id, trans.clip_b_id);
    }

    // ── Filter warnings + constant speed ────────────────────────────

    #[test]
    fn filter_on_tractor_produces_warning() {
        let xml = r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <playlist id="pl"/>
  <tractor id="root">
    <track producer="pl"/>
    <filter id="f1">
      <property name="mlt_service">brightness</property>
      <property name="level">1.2</property>
    </filter>
  </tractor>
</mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_filter.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(
            result.warnings.iter().any(|w| w.contains("brightness")),
            "expected brightness warning, got {:?}", result.warnings
        );
    }

    #[test]
    fn repeated_filter_collapses_to_single_warning() {
        let xml = r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <playlist id="pl"/>
  <tractor id="root">
    <track producer="pl"/>
    <filter><property name="mlt_service">brightness</property></filter>
    <filter><property name="mlt_service">brightness</property></filter>
    <filter><property name="mlt_service">brightness</property></filter>
  </tractor>
</mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_repeat.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let brightness_warnings: Vec<&String> = result
            .warnings
            .iter()
            .filter(|w| w.contains("brightness"))
            .collect();
        assert_eq!(
            brightness_warnings.len(),
            1,
            "expected single collapsed warning, got {brightness_warnings:?}"
        );
        assert!(brightness_warnings[0].contains("3"), "expected count in warning");
    }

    #[test]
    fn constant_speed_property_applied_to_clip() {
        let xml = r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <producer id="col1" in="0" out="23">
    <property name="mlt_service">color</property>
    <property name="resource">0xff0000ff</property>
    <property name="speed">2.0</property>
  </producer>
  <playlist id="pl">
    <entry producer="col1" in="0" out="23"/>
  </playlist>
  <tractor id="root"><track producer="pl"/></tractor>
</mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_speed.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let clip = &result.sequence.tracks[0].clips[0];
        assert!((clip.speed - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clean_fixture_has_no_warnings() {
        // Minimal: one color clip, no filters, no transitions.
        let xml = r#"<?xml version="1.0"?>
<mlt version="7.0">
  <profile frame_rate_num="24" frame_rate_den="1" width="320" height="240"/>
  <producer id="col1" in="0" out="23">
    <property name="mlt_service">color</property>
    <property name="resource">0x00ff00ff</property>
  </producer>
  <playlist id="pl">
    <entry producer="col1" in="0" out="23"/>
  </playlist>
  <tractor id="root"><track producer="pl"/></tractor>
</mlt>"#;
        let tmp = std::env::temp_dir().join("klipp_mlt_test_clean.mlt");
        std::fs::write(&tmp, xml).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(result.warnings.is_empty(), "expected no warnings, got {:?}", result.warnings);
    }

    #[test]
    fn full_fixture_produces_at_least_one_track() {
        // Smoke test: the real melt-exported fixture parses into a sequence
        // with at least one track (media files don't exist on disk, so clips
        // for them will be skipped with warnings).
        let tmp = std::env::temp_dir().join("klipp_mlt_test_fixture_track.mlt");
        std::fs::write(&tmp, FIXTURE_TWO_CLIPS).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(!result.sequence.tracks.is_empty());
    }

    #[test]
    fn load_kdenlive_qtblend_fixture() {
        // End-to-end: qtblend filter on a color producer should populate
        // transform_pos_x/y/scale and opacity keyframes on the imported clip.
        let tmp = std::env::temp_dir().join("klipp_mlt_test_qtblend.mlt");
        std::fs::write(&tmp, FIXTURE_GEOMETRY_KEYFRAMES).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let clip = &result.sequence.tracks[0].clips[0];
        assert_eq!(clip.transform_pos_x_keyframes().len(), 3);
        assert_eq!(clip.transform_pos_y_keyframes().len(), 3);
        assert_eq!(clip.transform_scale_keyframes().len(), 3);
        assert_eq!(clip.opacity_keyframes().len(), 3);

        // Middle stop at frame 25 / 25fps = 1.0s: zoomed-in half size,
        // centered offset (480, 270).
        assert!((clip.transform_pos_x_keyframes()[1].time_secs - 1.0).abs() < 1e-9);
        assert!((clip.transform_pos_x_keyframes()[1].value - 480.0).abs() < 1e-4);
        assert!((clip.transform_pos_y_keyframes()[1].value - 270.0).abs() < 1e-4);
        assert!((clip.transform_scale_keyframes()[1].value - 0.5).abs() < 1e-6);

        // Uniform scale — no non-uniform warnings expected.
        let non_uniform: Vec<&String> = result
            .warnings
            .iter()
            .filter(|w| w.contains("non-uniform"))
            .collect();
        assert!(non_uniform.is_empty(), "unexpected warnings: {non_uniform:?}");
    }

    #[test]
    fn load_kdenlive_nonuniform_scale_fixture() {
        // End-to-end: a w/h ratio that doesn't match the sequence aspect
        // should still populate keyframes, using the width-axis scale,
        // plus emit a "non-uniform" warning scoped to the filter service.
        let tmp = std::env::temp_dir().join("klipp_mlt_test_nonuniform.mlt");
        std::fs::write(&tmp, FIXTURE_NONUNIFORM_SCALE).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let clip = &result.sequence.tracks[0].clips[0];
        assert_eq!(clip.transform_scale_keyframes().len(), 2);
        // Width-axis: 1920/1920 = 1.0 (height-axis would give 0.5).
        assert!((clip.transform_scale_keyframes()[0].value - 1.0).abs() < 1e-6);

        let warning = result
            .warnings
            .iter()
            .find(|w| w.contains("non-uniform"))
            .expect("expected a non-uniform-scale warning");
        assert!(warning.contains("qtblend"), "warning should name the filter service: {warning}");
    }

    #[test]
    fn load_kdenlive_main_timeline_fixture() {
        // End-to-end: Kdenlive's real main-timeline shape — a root tractor
        // pointing at a UUID container, which holds a BG producer + 4
        // per-track-pair tractors — must produce 2 video + 2 audio tracks
        // with clips on the right kinds.
        let tmp = std::env::temp_dir().join("klipp_mlt_test_main_timeline.mlt");
        std::fs::write(&tmp, FIXTURE_MAIN_TIMELINE).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        // 4 tracks total: V1, V2, A1, A2. BG producer silently skipped.
        assert_eq!(
            result.sequence.tracks.len(),
            4,
            "expected 4 tracks, got {:?}",
            result.sequence.tracks.iter().map(|t| (&t.name, t.kind)).collect::<Vec<_>>(),
        );

        let video_tracks: Vec<&Track> = result
            .sequence
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        let audio_tracks: Vec<&Track> = result
            .sequence
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        assert_eq!(video_tracks.len(), 2, "expected 2 Video tracks");
        assert_eq!(audio_tracks.len(), 2, "expected 2 Audio tracks");

        // Exactly one V track and one A track should have a clip.
        let v_with_clip = video_tracks.iter().filter(|t| !t.clips.is_empty()).count();
        let a_with_clip = audio_tracks.iter().filter(|t| !t.clips.is_empty()).count();
        assert_eq!(v_with_clip, 1, "expected exactly 1 Video track with a clip");
        assert_eq!(a_with_clip, 1, "expected exactly 1 Audio track with a clip");

        // No "not supported" warnings — the container must be recursed, not skipped.
        let unsupported: Vec<&String> = result
            .warnings
            .iter()
            .filter(|w| w.contains("not supported"))
            .collect();
        assert!(unsupported.is_empty(), "unexpected warnings: {unsupported:?}");
    }

    #[test]
    fn load_kdenlive_volume_fixture() {
        // End-to-end: volume filter's level property should populate
        // volume_keyframes on the imported clip.
        let tmp = std::env::temp_dir().join("klipp_mlt_test_volume.mlt");
        std::fs::write(&tmp, FIXTURE_SCALAR_KEYFRAMES).unwrap();
        let result = load_kdenlive(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let clip = &result.sequence.tracks[0].clips[0];
        assert_eq!(clip.volume_keyframes.len(), 3);
        assert!((clip.volume_keyframes[0].time_secs - 0.0).abs() < 1e-9);
        assert!((clip.volume_keyframes[0].value - 0.0).abs() < 1e-6);
        assert!((clip.volume_keyframes[1].time_secs - 1.0).abs() < 1e-9); // 25/25
        assert!((clip.volume_keyframes[1].value - 1.0).abs() < 1e-6);
        assert!((clip.volume_keyframes[2].value - 0.5).abs() < 1e-6);
    }
}
