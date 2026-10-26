//! FCPXML (Final Cut Pro XML) project import — version 1.9.
//!
//! Parses `.fcpxml` files and produces a Klipp `Sequence` plus a populated
//! `MediaPool`. Mirrors the `mlt_import.rs` structure and reuses the shared
//! `ImportResult` contract from `mlt_import.rs`.
//!
//! Scope (MVP): assets (resources), formats, a single spine, `<asset-clip>`
//! and legacy `<clip ref="...">`, `<gap>`, and `<marker>`. Deferred: effects,
//! transitions, compound clips, multicam, roles, keywords, connected clips
//! (nested `<video>`/`<audio>` for secondary storylines), custom LUT refs.
//!
//! Parser is read-only (uses `roxmltree`). Zero panic paths — all errors are
//! returned as `FcpxmlImportError`. Unsupported features are logged to
//! `ImportResult::warnings`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use klipp_core::{Fps, MarkerColor, MarkerId, MediaId, RationalTime, Resolution, SequenceId};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{Clip, ClipKind, ClipMarker, Marker, Sequence, Track, TrackKind};

pub use crate::mlt_import::ImportResult;

/// Error type for FCPXML import failures.
#[derive(Debug)]
pub enum FcpxmlImportError {
    Io(std::io::Error),
    Parse(String),
    NoSequence,
    InvalidTime(String),
    MissingAsset(String),
    MissingFormat(String),
}

impl std::fmt::Display for FcpxmlImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Failed to read FCPXML file: {e}"),
            Self::Parse(s) => write!(f, "Failed to parse FCPXML: {s}"),
            Self::NoSequence => write!(f, "No <sequence> found in FCPXML"),
            Self::InvalidTime(s) => write!(f, "Invalid FCPXML time value: {s}"),
            Self::MissingAsset(id) => write!(f, "Referenced asset '{id}' not found in <resources>"),
            Self::MissingFormat(id) => write!(f, "Referenced format '{id}' not found in <resources>"),
        }
    }
}

impl std::error::Error for FcpxmlImportError {}

impl From<std::io::Error> for FcpxmlImportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<roxmltree::Error> for FcpxmlImportError {
    fn from(e: roxmltree::Error) -> Self {
        Self::Parse(e.to_string())
    }
}

/// A resource in the `<resources>` block that references a media file.
#[derive(Debug, Clone)]
struct AssetInfo {
    path: PathBuf,
    /// Duration of the source media, as a rational (num, den) in seconds.
    /// `None` if the asset didn't declare a duration.
    duration_rational: Option<(i64, i64)>,
    /// The format id this asset was declared against — reserved for future
    /// per-asset fps resolution (e.g. mixed-rate sequences).
    #[allow(dead_code)]
    format_ref: Option<String>,
    name: String,
}

/// A `<format>` resource — FPS + canvas dimensions.
#[derive(Debug, Clone, Copy)]
struct FormatInfo {
    /// `frameDuration` as (num, den) seconds. `1/25s` → (1, 25) — i.e. each
    /// frame lasts 1/25 of a second, so the sequence runs at 25 fps.
    frame_duration: (i64, i64),
    width: u32,
    height: u32,
}

impl FormatInfo {
    /// Convert `frameDuration` into a Klipp `Fps`. `1/25s` → 25/1 fps,
    /// `1001/24000s` → 24000/1001 fps.
    fn to_fps(self) -> Fps {
        // fps = 1 / frame_duration_secs = den / num (seconds)
        let (fd_num, fd_den) = self.frame_duration;
        if fd_num <= 0 || fd_den <= 0 {
            return Fps::film_24();
        }
        let num_u32 = u32::try_from(fd_den).unwrap_or(24);
        let den_u32 = u32::try_from(fd_num).unwrap_or(1);
        crate::path_util::sanitize_fps(num_u32, den_u32)
    }
}

/// Parse an FCPXML rational time like `"100/25s"`, `"5s"`, or `"1001/24000s"`.
/// Returns the time as a `(num, den)` rational in seconds.
///
/// Grammar:
/// - `"N/Ds"`: rational N/D seconds
/// - `"Ns"`: integer N seconds (denominator = 1)
/// - trailing `"s"` is mandatory; whitespace trimmed
/// - `"0s"` is valid
fn parse_fcpxml_rational(s: &str) -> Result<(i64, i64), FcpxmlImportError> {
    let trimmed = s.trim();
    let stripped = trimmed
        .strip_suffix('s')
        .ok_or_else(|| FcpxmlImportError::InvalidTime(s.to_string()))?;
    if let Some((num_str, den_str)) = stripped.split_once('/') {
        let num: i64 = num_str
            .trim()
            .parse()
            .map_err(|_| FcpxmlImportError::InvalidTime(s.to_string()))?;
        let den: i64 = den_str
            .trim()
            .parse()
            .map_err(|_| FcpxmlImportError::InvalidTime(s.to_string()))?;
        if den == 0 {
            return Err(FcpxmlImportError::InvalidTime(s.to_string()));
        }
        Ok((num, den))
    } else {
        let num: i64 = stripped
            .trim()
            .parse()
            .map_err(|_| FcpxmlImportError::InvalidTime(s.to_string()))?;
        Ok((num, 1))
    }
}

/// Convert an FCPXML rational-time string to a Klipp `RationalTime` at the
/// given sequence fps. The returned `RationalTime` has `rate = fps.rate()`
/// (which equals `fps.num`), so frames map 1:1 onto `value`.
///
/// Rounding: `frames = round(num * fps.num / (den * fps.den))`, performed in
/// `i128` to avoid overflow on long durations.
fn parse_fcpxml_time(s: &str, seq_fps: Fps) -> Result<RationalTime, FcpxmlImportError> {
    let (num, den) = parse_fcpxml_rational(s)?;
    let rate = seq_fps.rate();
    // frames = (num * fps.num) / (den * fps.den), rounded to nearest.
    // All arithmetic in i128 to avoid overflow.
    let numerator = (num as i128) * (seq_fps.num as i128);
    let denominator = (den as i128) * (seq_fps.den as i128);
    if denominator == 0 {
        return Err(FcpxmlImportError::InvalidTime(s.to_string()));
    }
    // Round-half-away-from-zero.
    let frames_i128 = if (numerator ^ denominator) >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        (numerator - denominator / 2) / denominator
    };
    let frames = i64::try_from(frames_i128)
        .map_err(|_| FcpxmlImportError::InvalidTime(s.to_string()))?;
    Ok(RationalTime::from_frames(frames, rate))
}

/// Strip `file://` prefix from an asset src URL and minimally decode percent-
/// escapes for space (`%20`). We don't pull in a new dep for full percent-
/// decoding; a warning is pushed if we detect other escapes so the user knows
/// the path may be wrong.
fn decode_file_url(src: &str, warnings: &mut Vec<String>) -> PathBuf {
    let path_part = src
        .strip_prefix("file://")
        .map(|p| p.to_string())
        .unwrap_or_else(|| src.to_string());
    // Minimal percent-decode for the common spaces-in-paths case.
    let decoded = path_part.replace("%20", " ");
    if decoded.contains('%') {
        warnings.push(format!(
            "Asset src '{src}' contains percent-encoded characters that were not decoded — path may be wrong"
        ));
    }
    // Normalize to collapse any ".." components. FCPXML paths are typically
    // absolute file:// URLs so there's no project-dir escape to guard
    // against, but normalising keeps the stored path clean and prevents
    // unexpected traversals if a crafted FCPXML happens to use relative refs.
    crate::path_util::normalize_path(&PathBuf::from(decoded))
}

/// Index `<resources>` children into format + asset tables.
fn index_resources<'a>(
    resources: roxmltree::Node<'a, 'a>,
    warnings: &mut Vec<String>,
) -> (HashMap<String, FormatInfo>, HashMap<String, AssetInfo>) {
    let mut formats: HashMap<String, FormatInfo> = HashMap::new();
    let mut assets: HashMap<String, AssetInfo> = HashMap::new();

    for child in resources.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "format" => {
                let id = match child.attribute("id") {
                    Some(v) => v.to_string(),
                    None => continue,
                };
                // `frameDuration` is optional in FCPXML for still-image formats;
                // fall back to 24fps so we don't reject the document outright.
                let frame_duration = match child
                    .attribute("frameDuration")
                    .map(parse_fcpxml_rational)
                    .transpose()
                {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        // No frameDuration — common for image-only formats.
                        (1, 24)
                    }
                    Err(e) => {
                        warnings.push(format!(
                            "Format '{id}' has invalid frameDuration: {e}; defaulting to 1/24s"
                        ));
                        (1, 24)
                    }
                };
                let width = child
                    .attribute("width")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1920);
                let height = child
                    .attribute("height")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1080);
                formats.insert(
                    id,
                    FormatInfo {
                        frame_duration,
                        width,
                        height,
                    },
                );
            }
            "asset" => {
                let id = match child.attribute("id") {
                    Some(v) => v.to_string(),
                    None => continue,
                };
                let src = child.attribute("src").unwrap_or("");
                let path = decode_file_url(src, warnings);
                let name = child
                    .attribute("name")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string()
                    });
                let duration_rational = match child
                    .attribute("duration")
                    .map(parse_fcpxml_rational)
                    .transpose()
                {
                    Ok(v) => v,
                    Err(e) => {
                        warnings.push(format!(
                            "Asset '{id}' has invalid duration: {e}; treating as unknown length"
                        ));
                        None
                    }
                };
                let format_ref = child.attribute("format").map(|s| s.to_string());
                assets.insert(
                    id,
                    AssetInfo {
                        path,
                        duration_rational,
                        format_ref,
                        name,
                    },
                );
            }
            "media" | "effect" | "title-effect" | "transition-effect" => {
                warnings.push(format!(
                    "<{}> resource is not supported — referenced clips may be incomplete",
                    child.tag_name().name()
                ));
            }
            _ => {}
        }
    }
    (formats, assets)
}

/// Find the first `<sequence>` node under `<library>/<event>/<project>`.
/// Returns None if the document doesn't contain one.
fn find_first_sequence<'a>(root: roxmltree::Node<'a, 'a>) -> Option<roxmltree::Node<'a, 'a>> {
    root.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "sequence")
}

/// Register (or look up) a MediaItem for an asset. Dedupes on `asset_id`
/// within this import pass; the caller reuses the returned `MediaId` across
/// clips that reference the same asset.
fn register_asset_media(
    media_pool: &mut MediaPool,
    asset_id: &str,
    asset: &AssetInfo,
    seq_fps: Fps,
    cache: &mut HashMap<String, MediaId>,
) -> MediaId {
    if let Some(mid) = cache.get(asset_id) {
        return *mid;
    }

    // Try to dedupe on path — if the pool already has a MediaItem for this
    // file, reuse its id so the project stays consistent across imports.
    if let Some(existing) = media_pool.find_by_path(&asset.path) {
        let id = existing.id;
        cache.insert(asset_id.to_string(), id);
        return id;
    }

    let duration = match asset.duration_rational {
        Some((num, den)) => {
            let numerator = (num as i128) * (seq_fps.num as i128);
            let denominator = (den as i128) * (seq_fps.den as i128);
            let frames = if denominator != 0 {
                i64::try_from(numerator / denominator).unwrap_or(0)
            } else {
                0
            };
            RationalTime::from_frames(frames, seq_fps.rate())
        }
        None => RationalTime::ZERO,
    };

    let offline = !asset.path.exists();
    let item = MediaItem {
        id: MediaId::new(),
        path: asset.path.clone(),
        kind: MediaItemKind::File,
        filename: asset.name.clone(),
        duration,
        fps: seq_fps,
        resolution: Resolution { width: 0, height: 0 },
        video_codec: String::new(),
        audio_codec: String::new(),
        has_audio: false,
        file_size_bytes: 0,
        sample_rate: None,
        audio_channels: None,
        offline,
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
        needs_reprobe: !offline,
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
    };
    let id = media_pool.add(item);
    cache.insert(asset_id.to_string(), id);
    id
}

/// Parse `<marker>` children of a clip element, returning ClipMarkers with
/// `time_secs` in clip-local time — i.e. offset from the clip's
/// `source_in`, matching Klipp's `ClipMarker.time_secs` convention
/// ("Time relative to clip's source_in"). FCPXML's `<marker start="...">`
/// is absolute source time within the asset, so we subtract the clip's
/// `source_in` here at import time. Before this, markers landed at
/// absolute positions and consumers had to remember to subtract
/// `source_in` downstream (they typically didn't — F-170).
fn parse_clip_markers(
    clip_node: roxmltree::Node<'_, '_>,
    seq_fps: Fps,
    source_in_secs: f64,
) -> Vec<ClipMarker> {
    let mut out = Vec::new();
    for m in clip_node
        .children()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "marker" | "chapter-marker"))
    {
        let start_attr = match m.attribute("start") {
            Some(v) => v,
            None => continue,
        };
        let time = match parse_fcpxml_time(start_attr, seq_fps) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Rebase: FCPXML absolute → Klipp clip-local.
        let clip_local_secs = time.to_secs_f64() - source_in_secs;
        let name = m
            .attribute("value")
            .map(|s| s.to_string())
            .unwrap_or_default();
        out.push(ClipMarker {
            time_secs: clip_local_secs,
            name,
            color: marker_colour(m),
            comment: marker_note(m),
        });
    }
    out
}

/// A marker's note: its `note` attribute, as Final Cut Pro writes it, or
/// a `<note>` child.
fn marker_note(m: roxmltree::Node<'_, '_>) -> String {
    m.attribute("note")
        .or_else(|| m.children().find(|c| c.is_element() && c.tag_name().name() == "note").and_then(|c| c.text()))
        .unwrap_or("")
        .to_string()
}

/// A marker's colour as Final Cut Pro shows its kind (#2085): a to-do red,
/// a completed one green, a chapter purple, any other blue.
fn marker_colour(m: roxmltree::Node<'_, '_>) -> MarkerColor {
    match (m.tag_name().name(), m.attribute("completed")) {
        ("chapter-marker", _) => MarkerColor::Purple,
        (_, Some("0")) => MarkerColor::Red,
        (_, Some("1")) => MarkerColor::Green,
        _ => MarkerColor::Blue,
    }
}

/// Where an element's `offset` is measured from: a spine element's on the
/// timeline, a connected clip's in its parent's own time, which starts at
/// the parent's `start` where the parent is on the timeline (#2085).
#[derive(Clone, Copy)]
struct TimeBase {
    timeline: RationalTime,
    local: RationalTime,
}

impl TimeBase {
    const SPINE: TimeBase = TimeBase { timeline: RationalTime::ZERO, local: RationalTime::ZERO };

    // The offset first: a sum takes its left side's rate, and the spine's
    // base is zero at rate 1.
    fn place(self, offset: RationalTime) -> RationalTime {
        offset - self.local + self.timeline
    }
}

/// An element's `offset`, `duration` and `start`, at the sequence's rate.
fn times(node: roxmltree::Node<'_, '_>, seq_fps: Fps) -> Result<(RationalTime, RationalTime, RationalTime), FcpxmlImportError> {
    let at = |name: &str| parse_fcpxml_time(node.attribute(name).unwrap_or("0s"), seq_fps);
    Ok((at("offset")?, at("duration")?, at("start")?))
}

/// A `<param name="amount">`'s keyframes under `adjust`, as (seconds from
/// the clip's start, value).
fn amount_keys(adjust: roxmltree::Node<'_, '_>, seq_fps: Fps, start: RationalTime, value: impl Fn(&str) -> Option<f64>) -> Vec<(f64, f64)> {
    adjust
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "keyframe")
        .filter_map(|k| {
            let t = parse_fcpxml_time(k.attribute("time")?, seq_fps).ok()?;
            Some(((t - start).to_secs_f64().max(0.0), value(k.attribute("value")?)?))
        })
        .collect()
}

/// A level such as `-6dB` as a gain (0 dB is 1).
fn decibels(v: &str) -> Option<f64> {
    let db: f64 = v.trim().trim_end_matches("dB").trim().parse().ok()?;
    Some(10f64.powf(db / 20.0))
}

fn keys(points: &[(f64, f64)]) -> Vec<klipp_core::Keyframe> {
    points
        .iter()
        .map(|(t, v)| klipp_core::Keyframe { time_secs: *t, value: *v as f32, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() })
        .collect()
}

/// A clip's constant speed from its `<timeMap>`: how much of its source a
/// second of it plays. `None` for no map; a ramp (points off one line) is
/// named in `warnings` and played at its overall speed.
fn time_map_speed(node: roxmltree::Node<'_, '_>, seq_fps: Fps, name: &str, warnings: &mut Vec<String>) -> Option<f64> {
    let map = node.children().find(|n| n.is_element() && n.tag_name().name() == "timeMap")?;
    let points: Vec<(f64, f64)> = map
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "timept")
        .filter_map(|p| Some((parse_fcpxml_time(p.attribute("time")?, seq_fps).ok()?.to_secs_f64(), parse_fcpxml_time(p.attribute("value")?, seq_fps).ok()?.to_secs_f64())))
        .collect();
    let (first, last) = (points.first()?, points.last()?);
    if last.0 <= first.0 {
        return None;
    }
    let speed = (last.1 - first.1) / (last.0 - first.0);
    let off_line = points.iter().any(|(t, v)| (first.1 + (t - first.0) * speed - v).abs() > 1e-3);
    if off_line {
        warnings.push(format!("Clip '{name}': a speed ramp is played at its overall speed ({:.0}%)", speed * 100.0));
    }
    (speed.abs() > 1e-9).then_some(speed)
}

/// Parse an `<asset-clip>`, `<clip>`, `<video>` or `<audio>` element into a
/// Klipp `Clip`, its `offset` read against `base`.
#[allow(clippy::too_many_arguments)]
fn parse_clip_element(
    node: roxmltree::Node<'_, '_>,
    base: TimeBase,
    assets: &HashMap<String, AssetInfo>,
    media_pool: &mut MediaPool,
    media_cache: &mut HashMap<String, MediaId>,
    seq_fps: Fps,
    warnings: &mut Vec<String>,
) -> Result<Option<Clip>, FcpxmlImportError> {
    // A `<clip>` names its media on the `<video>` (or `<audio>`) inside it.
    let own_media = node.children().find(|n| n.is_element() && matches!(n.tag_name().name(), "video" | "audio") && n.attribute("lane").is_none());
    let asset_id = match node.attribute("ref").or_else(|| own_media.and_then(|m| m.attribute("ref"))) {
        Some(v) => v,
        None => {
            warnings.push(format!(
                "<{}> has no 'ref' attribute — skipped",
                node.tag_name().name()
            ));
            return Ok(None);
        }
    };
    let asset = match assets.get(asset_id) {
        Some(a) => a.clone(),
        None => return Err(FcpxmlImportError::MissingAsset(asset_id.to_string())),
    };
    let (offset, duration, source_in) = times(node, seq_fps)?;
    let name = node.attribute("name").unwrap_or(asset_id).to_string();

    let timeline_in = base.place(offset);
    let timeline_out = timeline_in + duration;
    let speed = time_map_speed(node, seq_fps, &name, warnings);
    let source_out = match speed {
        Some(speed) => source_in + RationalTime::from_secs_f64(duration.to_secs_f64() * speed.abs(), seq_fps.rate()),
        None => source_in + duration,
    };

    let media_id = register_asset_media(media_pool, asset_id, &asset, seq_fps, media_cache);

    let mut clip = Clip::gap(timeline_in, timeline_out);
    clip.source_in = source_in;
    clip.source_out = source_out;
    clip.kind = ClipKind::Media { media_id };
    if let Some(speed) = speed {
        clip.speed = speed;
    }
    if let Some(enabled) = node.attribute("enabled") {
        clip.enabled = enabled != "0";
    }
    clip.clip_markers = parse_clip_markers(node, seq_fps, source_in.to_secs_f64());

    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "marker" | "chapter-marker" | "timeMap" => {} // handled
            // Connected clips: their parent places them on their lanes.
            _ if child.attribute("lane").is_some() => {}
            "video" | "audio" => {} // the clip's own media
            "adjust-blend" => {
                let points = amount_keys(child, seq_fps, source_in, |v| v.parse().ok());
                let points = if points.is_empty() { child.attribute("amount").and_then(|v| v.parse().ok()).map(|v| vec![(0.0, v)]).unwrap_or_default() } else { points };
                if points.iter().any(|(_, v)| (*v - 1.0).abs() > 1e-6) {
                    if let Err(e) = crate::import_report::set_opacity_keys(&mut clip, &keys(&points)) {
                        warnings.push(format!("Clip '{name}': opacity not imported ({e})"));
                    }
                }
            }
            "adjust-volume" => {
                let points = amount_keys(child, seq_fps, source_in, decibels);
                if points.is_empty() {
                    if let Some(gain) = child.attribute("amount").and_then(decibels) {
                        clip.volume = gain as f32;
                    }
                } else {
                    clip.volume_keyframes = keys(&points);
                }
            }
            "adjust-transform" => warnings.push(format!("Clip '{name}': its position, scale and rotation (adjust-transform) are not imported")),
            "filter-video" | "filter-audio" | "adjust-color" | "adjust-levels" => warnings
                .push(format!(
                    "Effect <{}> on clip '{asset_id}' is not imported",
                    child.tag_name().name()
                )),
            other => warnings.push(format!(
                "Unknown <{other}> child on clip '{asset_id}' — ignored",
            )),
        }
    }

    Ok(Some(clip))
}

/// A `<title>` as an editable Klipp title holding its text and length,
/// its `offset` read against `base`; the look is Klipp's default, named in
/// `warnings`.
fn parse_title(node: roxmltree::Node<'_, '_>, base: TimeBase, seq_fps: Fps, warnings: &mut Vec<String>) -> Result<Clip, FcpxmlImportError> {
    let (offset, duration, _) = times(node, seq_fps)?;
    let timeline_in = base.place(offset);
    let text: String = node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "text")
        .flat_map(|t| t.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()))
        .collect::<Vec<_>>()
        .join("");
    let text = if text.trim().is_empty() { node.attribute("name").unwrap_or("Title").to_string() } else { text };
    let mut data = klipp_timeline::TitleData::default();
    if let Some(block) = data.primary_text_mut() {
        block.text = text.clone();
    }
    let mut clip = Clip::gap(timeline_in, timeline_in + duration);
    clip.kind = ClipKind::Title(data);
    warnings.push(format!("Title '{}': its text and length came across; its look is Klipp's default", text.lines().next().unwrap_or("")));
    Ok(clip)
}

/// A spine `<transition>`: where it starts and how long it is, and its name.
struct SpineTransition {
    offset: RationalTime,
    duration: RationalTime,
    name: String,
}

/// `t` between the clip on `track` that ends at its middle and the one
/// that starts there, as the nearest Klipp transition; one with no clip
/// on a side, or of a kind Klipp has no like of, is named in `warnings`.
fn add_spine_transition(track: &mut Track, t: &SpineTransition, seq_fps: Fps, warnings: &mut Vec<String>) {
    use klipp_core::TransitionKind;
    let cut = t.offset + RationalTime::from_secs_f64(t.duration.to_secs_f64() / 2.0, seq_fps.rate());
    let near = |a: RationalTime| (a.to_secs_f64() - cut.to_secs_f64()).abs() < seq_fps.frame_duration_secs() * 0.75;
    let a = track.clips.iter().find(|c| near(c.timeline_out)).map(|c| c.id);
    let b = track.clips.iter().find(|c| near(c.timeline_in)).map(|c| c.id);
    let (Some(clip_a_id), Some(clip_b_id)) = (a, b) else {
        warnings.push(format!("Transition '{}' has no clip on one side — not imported", t.name));
        return;
    };
    let lower = t.name.to_lowercase();
    let kind = if lower.contains("cross dissolve") || lower == "dissolve" {
        TransitionKind::CrossDissolve
    } else if lower.contains("fade to color") || lower.contains("dip to") {
        TransitionKind::DipToBlack
    } else if lower.contains("wipe") {
        TransitionKind::WipeLeft
    } else if lower.contains("push") {
        TransitionKind::PushLeft
    } else {
        warnings.push(format!("Transition '{}' imported as a cross dissolve", t.name));
        TransitionKind::CrossDissolve
    };
    track.transitions.push(klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        kind,
        duration: t.duration,
        clip_a_id,
        clip_b_id,
        user_params: klipp_core::TransitionUserParams::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    });
}

/// Parse an FCPXML 1.9 document from a string.
pub fn parse_fcpxml(xml: &str) -> Result<ImportResult, FcpxmlImportError> {
    let xml = crate::path_util::without_bare_doctype(xml);
    let doc = crate::path_util::parse_xml_safe(&xml)?;
    let root = doc.root_element();
    if root.tag_name().name() != "fcpxml" {
        return Err(FcpxmlImportError::Parse(format!(
            "Root element is <{}>, expected <fcpxml>",
            root.tag_name().name()
        )));
    }

    let mut warnings: Vec<String> = Vec::new();

    // Index <resources>.
    let resources_node = root
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "resources");
    let (formats, assets) = match resources_node {
        Some(r) => index_resources(r, &mut warnings),
        None => {
            warnings.push("<resources> block missing — clips may fail to resolve".to_string());
            (HashMap::new(), HashMap::new())
        }
    };

    // Find the first <sequence>.
    let seq_node = find_first_sequence(root).ok_or(FcpxmlImportError::NoSequence)?;

    // Resolve sequence format → fps + resolution.
    let seq_format_ref = seq_node.attribute("format").map(|s| s.to_string());
    let (seq_fps, resolution) = if let Some(ref fref) = seq_format_ref {
        let fmt = formats
            .get(fref)
            .ok_or_else(|| FcpxmlImportError::MissingFormat(fref.clone()))?;
        (
            fmt.to_fps(),
            crate::path_util::sanitize_resolution(fmt.width, fmt.height),
        )
    } else {
        warnings.push(
            "<sequence> has no format attribute — defaulting to 24fps 1920x1080".to_string(),
        );
        (Fps::film_24(), Resolution { width: 1920, height: 1080 })
    };

    // The spine is the primary storyline.
    let spine = match seq_node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "spine")
    {
        Some(s) => s,
        None => {
            warnings.push("<sequence> has no <spine> — timeline will be empty".to_string());
            // Build an empty sequence and return — not an error.
            let sequence = Sequence {
                // No interchange format carries a camera.
                camera: klipp_timeline::Camera::default(),
                timeline_graph: None,
                grade_groups: Vec::new(),
                // An imported timeline takes its rate's convention.
                title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
                node_labels: Vec::new(),
                drop_frame: Sequence::default_drop_frame(seq_fps),
                id: SequenceId::new(),
                name: project_name_from_tree(root),
                fps: seq_fps,
                resolution,
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
            };
            return Ok(ImportResult {
                sequence,
                media_pool: MediaPool::new(),
                warnings,
            });
        }
    };

    let mut media_pool = MediaPool::new();
    let mut media_cache: HashMap<String, MediaId> = HashMap::new();
    let mut track = Track::new("V1", TrackKind::Video);
    // Connected clips by lane: above the storyline positive, under it
    // (sound) negative (#2085).
    let mut lanes: std::collections::BTreeMap<i64, Track> = std::collections::BTreeMap::new();
    let mut transitions: Vec<SpineTransition> = Vec::new();

    for child in spine.children().filter(|n| n.is_element()) {
        let mut parent: Option<TimeBase> = None;
        match child.tag_name().name() {
            "asset-clip" | "clip" | "video" | "audio" => {
                if let Some(clip) = parse_clip_element(
                    child,
                    TimeBase::SPINE,
                    &assets,
                    &mut media_pool,
                    &mut media_cache,
                    seq_fps,
                    &mut warnings,
                )? {
                    parent = Some(TimeBase { timeline: clip.timeline_in, local: clip.source_in });
                    track.clips.push(clip)
                }
            }
            "title" => {
                let clip = parse_title(child, TimeBase::SPINE, seq_fps, &mut warnings)?;
                let (_, _, start) = times(child, seq_fps)?;
                parent = Some(TimeBase { timeline: clip.timeline_in, local: start });
                track.clips.push(clip);
            }
            "gap" => {
                // Gaps in FCPXML are explicit on the spine. Klipp's timeline
                // represents empty space as absence of clips — skipping the
                // gap preserves the intended offsets on subsequent clips,
                // because each clip carries its own `offset` attribute
                // (absolute timeline position). Clips connected to it are
                // placed all the same.
                let (offset, _, start) = times(child, seq_fps)?;
                parent = Some(TimeBase { timeline: offset, local: start });
            }
            "transition" => {
                let (offset, duration, _) = times(child, seq_fps)?;
                transitions.push(SpineTransition { offset, duration, name: child.attribute("name").unwrap_or("transition").to_string() });
            }
            "ref-clip" => warnings.push(
                "<ref-clip> (compound clip) is not imported — deferred feature".to_string(),
            ),
            "mc-clip" => warnings.push(
                "<mc-clip> (multicam) is not imported — deferred feature".to_string(),
            ),
            "sync-clip" => warnings.push(
                "<sync-clip> is not imported — deferred feature".to_string(),
            ),
            "audition" => warnings.push(
                "<audition> is not imported — the active pick will also be skipped"
                    .to_string(),
            ),
            other => warnings.push(format!(
                "Unknown spine element <{other}> — ignored",
            )),
        }
        // Its connected clips, each on its lane's track.
        let Some(base) = parent else { continue };
        for connected in child.children().filter(|n| n.is_element()) {
            let Some(lane) = connected.attribute("lane").and_then(|l| l.parse::<i64>().ok()).filter(|l| *l != 0) else { continue };
            let clip = match connected.tag_name().name() {
                "asset-clip" | "clip" | "video" | "audio" => parse_clip_element(connected, base, &assets, &mut media_pool, &mut media_cache, seq_fps, &mut warnings)?,
                "title" => Some(parse_title(connected, base, seq_fps, &mut warnings)?),
                other => {
                    warnings.push(format!("Connected <{other}> on lane {lane} is not imported"));
                    None
                }
            };
            if let Some(clip) = clip {
                let kind = if lane > 0 { TrackKind::Video } else { TrackKind::Audio };
                let name = if lane > 0 { format!("V{}", lane + 1) } else { format!("A{}", -lane) };
                lanes.entry(lane).or_insert_with(|| Track::new(&name, kind)).clips.push(clip);
            }
        }
    }
    for t in &transitions {
        add_spine_transition(&mut track, t, seq_fps, &mut warnings);
    }
    // Top lane first, the storyline, then the sound lanes from the
    // storyline down; a lane's clips that overlap are kept as they are.
    let mut tracks: Vec<Track> = lanes.range(1..).rev().map(|(_, t)| t.clone()).collect();
    tracks.push(track);
    tracks.extend(lanes.range(..0).rev().map(|(_, t)| t.clone()));
    for t in &mut tracks {
        t.clips.sort_by_key(|c| c.timeline_in);
    }

    let sequence = Sequence {
        // No interchange format carries a camera.
        camera: klipp_timeline::Camera::default(),
        timeline_graph: None,
        grade_groups: Vec::new(),
        // An imported timeline takes its rate's convention.
        title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
        node_labels: Vec::new(),
        drop_frame: Sequence::default_drop_frame(seq_fps),
        id: SequenceId::new(),
        name: project_name_from_tree(root),
        fps: seq_fps,
        resolution,
        first_clip_autoset: Default::default(),
        tracks,
        in_point: None,
        out_point: None,
        markers: collect_sequence_markers(seq_node, seq_fps),
        last_playhead: None,
        brief: None,
        edit_plan: None,
        reviews: Vec::new(),
        awaiting_refit: false,
    };

    // Touch MarkerId so it isn't a dead import if no sequence markers are
    // produced — future work will promote clip markers or spine markers.
    let _ = MarkerId::new;

    Ok(ImportResult {
        sequence,
        media_pool,
        warnings,
    })
}

/// FCPXML allows `<marker>` children directly inside `<sequence>` in some
/// projects. Harvest any we find into sequence markers.
fn collect_sequence_markers(
    seq_node: roxmltree::Node<'_, '_>,
    seq_fps: Fps,
) -> Vec<Marker> {
    let mut out = Vec::new();
    for m in seq_node
        .children()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "marker" | "chapter-marker"))
    {
        let start_attr = match m.attribute("start") {
            Some(v) => v,
            None => continue,
        };
        let position = match parse_fcpxml_time(start_attr, seq_fps) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let duration = m
            .attribute("duration")
            .and_then(|s| parse_fcpxml_time(s, seq_fps).ok())
            .filter(|d| d.value > 0);
        let label = m
            .attribute("value")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let comment = marker_note(m);
        out.push(Marker { review: None,
            id: MarkerId::new(),
            position,
            label,
            color: marker_colour(m),
            duration,
            comment,
            kind: if m.tag_name().name() == "chapter-marker" { klipp_core::MarkerKind::Chapter } else { klipp_core::MarkerKind::None },
        });
    }
    out
}

fn project_name_from_tree(root: roxmltree::Node<'_, '_>) -> String {
    root.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "project")
        .and_then(|n| n.attribute("name"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Imported FCPXML".to_string())
}

/// Parse an FCPXML file from disk.
pub fn load_fcpxml(path: &Path) -> Result<ImportResult, FcpxmlImportError> {
    tracing::info!(path = %path.display(), "FCPXML import: starting");
    let xml_text = crate::path_util::read_to_string_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES)?;
    let mut result = parse_fcpxml(&xml_text)?;

    // Inherit the filename as the sequence name if the XML didn't carry one.
    if result.sequence.name == "Imported FCPXML" {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            result.sequence.name = stem.to_string();
        }
    }

    tracing::info!(
        tracks = result.sequence.tracks.len(),
        total_clips = result
            .sequence
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum::<usize>(),
        warnings = result.warnings.len(),
        "FCPXML import: done"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_FCPXML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
    <asset id="r1" name="clip.mp4" src="file:///tmp/clip.mp4"
           duration="100/25s" format="r0"/>
  </resources>
  <library>
    <event name="Event">
      <project name="Project">
        <sequence duration="100/25s" format="r0">
          <spine>
            <asset-clip name="clip.mp4" ref="r1"
                        offset="0/25s" duration="100/25s" start="0/25s"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

    const FCPXML_WITH_GAP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
    <asset id="r1" name="a.mp4" src="file:///tmp/a.mp4"
           duration="50/25s" format="r0"/>
    <asset id="r2" name="b.mp4" src="file:///tmp/b.mp4"
           duration="50/25s" format="r0"/>
  </resources>
  <library>
    <event name="E">
      <project name="P">
        <sequence duration="200/25s" format="r0">
          <spine>
            <asset-clip ref="r1" offset="0/25s" duration="50/25s" start="0/25s"/>
            <gap offset="50/25s" duration="10/25s"/>
            <asset-clip ref="r2" offset="60/25s" duration="50/25s" start="0/25s"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

    const FCPXML_WITH_MARKER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
    <asset id="r1" name="clip.mp4" src="file:///tmp/clip.mp4"
           duration="100/25s" format="r0"/>
  </resources>
  <library>
    <event name="E">
      <project name="P">
        <sequence duration="100/25s" format="r0">
          <spine>
            <asset-clip ref="r1" offset="0/25s" duration="100/25s" start="0/25s">
              <marker start="5/25s" duration="1/25s" value="My Marker"/>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

    const FCPXML_2997: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1001/30000s" width="1920" height="1080"/>
    <asset id="r1" name="clip.mp4" src="file:///tmp/clip.mp4"
           duration="30030/30000s" format="r0"/>
  </resources>
  <library>
    <event name="E">
      <project name="P">
        <sequence duration="30030/30000s" format="r0">
          <spine>
            <asset-clip ref="r1" offset="0s" duration="30030/30000s" start="0s"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;

    #[test]
    fn parse_simple_fcpxml_produces_one_sequence() {
        let result = parse_fcpxml(SIMPLE_FCPXML).expect("parse ok");
        assert_eq!(result.sequence.tracks.len(), 1);
    }

    #[test]
    fn parse_fcpxml_clip_count() {
        let result = parse_fcpxml(SIMPLE_FCPXML).expect("parse ok");
        assert_eq!(result.sequence.tracks[0].clips.len(), 1);
    }

    #[test]
    fn parse_fcpxml_time_integer() {
        assert_eq!(parse_fcpxml_rational("100/25s").unwrap(), (100, 25));
        let tc = parse_fcpxml_time("100/25s", Fps::new(25, 1)).unwrap();
        assert_eq!(tc.value, 100);
        assert_eq!(tc.rate, 25);
    }

    #[test]
    fn parse_fcpxml_time_non_integer() {
        // 24000/1001 fps — NTSC film.
        let fps = Fps::new(24000, 1001);
        // 1001/24000s at 24000/1001fps → exactly 1 frame.
        let one = parse_fcpxml_time("1001/24000s", fps).unwrap();
        assert_eq!(one.value, 1);
        // 3003/24000s → 3 frames.
        let three = parse_fcpxml_time("3003/24000s", fps).unwrap();
        assert_eq!(three.value, 3);
    }

    #[test]
    fn parse_fcpxml_time_seconds_only() {
        assert_eq!(parse_fcpxml_rational("5s").unwrap(), (5, 1));
        assert_eq!(parse_fcpxml_rational("0s").unwrap(), (0, 1));
        // 5s at 25fps = 125 frames.
        let tc = parse_fcpxml_time("5s", Fps::new(25, 1)).unwrap();
        assert_eq!(tc.value, 125);
    }

    /// #2085: connected clips on their lanes' tracks (on a clip and on a
    /// gap), a title on the storyline and one connected, a dissolve
    /// between two clips, a clip at twice its speed with an opacity fade, a
    /// level, markers of each kind, and the transform named as not carried.
    #[test]
    fn connected_clips_transitions_speed_levels_titles_and_markers_come_across() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
    <asset id="a1" name="A" src="file:///nowhere/a.mov" duration="100s" hasVideo="1"/>
    <asset id="a2" name="B" src="file:///nowhere/b.mov" duration="100s" hasVideo="1"/>
    <asset id="m1" name="Music" src="file:///nowhere/m.wav" duration="100s" hasAudio="1"/>
    <effect id="t1" name="Basic Title"/>
  </resources>
  <library><event><project name="Lanes">
    <sequence format="r0" duration="12s">
      <spine>
        <asset-clip ref="a1" name="A" offset="0s" start="10s" duration="4s">
          <asset-clip ref="a2" name="B over A" lane="1" offset="11s" start="0s" duration="2s">
            <adjust-blend>
              <param name="amount"><keyframeAnimation><keyframe time="0s" value="0"/><keyframe time="1s" value="1"/></keyframeAnimation></param>
            </adjust-blend>
          </asset-clip>
          <audio ref="m1" lane="-1" offset="10s" start="0s" duration="4s"><adjust-volume amount="-6dB"/></audio>
          <title ref="t1" name="Name" lane="2" offset="12s" duration="1s"><text><text-style ref="ts1">Ana Silva</text-style></text></title>
          <marker start="11s" duration="1/25s" value="Fix" completed="0" note="too dark"/>
          <chapter-marker start="12s" duration="1/25s" value="Part 2"/>
          <adjust-transform position="10 0" scale="1.2 1.2"/>
        </asset-clip>
        <transition name="Cross Dissolve" offset="3s" duration="2s"/>
        <asset-clip ref="a2" name="B" offset="4s" start="20s" duration="4s">
          <timeMap><timept time="0s" value="0s"/><timept time="4s" value="8s"/></timeMap>
        </asset-clip>
        <gap offset="8s" start="0s" duration="2s">
          <asset-clip ref="a1" name="A on gap" lane="1" offset="0s" start="50s" duration="2s"/>
        </gap>
        <title ref="t1" name="End" offset="10s" duration="2s"><text><text-style>The </text-style><text-style>End</text-style></text></title>
      </spine>
      <marker start="1s" duration="1/25s" value="Done" completed="1"/>
    </sequence>
  </project></event></library>
</fcpxml>"#;
        let result = parse_fcpxml(xml).expect("parses");
        let seq = &result.sequence;
        let secs = |t: RationalTime| t.to_secs_f64();
        let names: Vec<&str> = seq.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["V3", "V2", "V1", "A1"], "lanes above the storyline first, sound under it");

        // V2: B over A at A's 11 s, 1 s in; A on the gap at 8 s.
        let v2 = &seq.tracks[1];
        assert_eq!(v2.clips.iter().map(|c| (secs(c.timeline_in), secs(c.timeline_out))).collect::<Vec<_>>(), [(1.0, 3.0), (8.0, 10.0)]);
        let (fade, _) = v2.clips[0].truth_lane(&klipp_core::ParamId::Opacity).expect("reads");
        assert_eq!(fade.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, 0.0), (1.0, 1.0)]);
        // V3: the connected title at 2 s.
        let name = &seq.tracks[0].clips[0];
        assert!(matches!(&name.kind, ClipKind::Title(d) if d.primary_text().is_some_and(|b| b.text == "Ana Silva")), "{:?}", name.kind);
        assert_eq!(secs(name.timeline_in), 2.0);

        // V1: A, B at twice its speed, the title; the dissolve between A and B.
        let v1 = &seq.tracks[2];
        let b = &v1.clips[1];
        assert_eq!((b.speed, secs(b.source_in), secs(b.source_out)), (2.0, 20.0, 28.0));
        assert!(matches!(&v1.clips[2].kind, ClipKind::Title(d) if d.primary_text().is_some_and(|b| b.text == "The End")));
        let t = &v1.transitions[0];
        assert_eq!((t.kind, secs(t.duration), t.clip_a_id, t.clip_b_id), (klipp_core::TransitionKind::CrossDissolve, 2.0, v1.clips[0].id, b.id));

        // A1: the music at A's start, at -6 dB.
        let music = &seq.tracks[3].clips[0];
        assert_eq!(secs(music.timeline_in), 0.0);
        assert!((music.volume - 0.501).abs() < 0.01, "{}", music.volume);

        // Markers: a to-do red with its note, a chapter purple, a done one green.
        let marks: Vec<(&str, MarkerColor, &str)> = v1.clips[0].clip_markers.iter().map(|m| (m.name.as_str(), m.color, m.comment.as_str())).collect();
        assert_eq!(marks, [("Fix", MarkerColor::Red, "too dark"), ("Part 2", MarkerColor::Purple, "")]);
        assert_eq!((seq.markers[0].label.as_str(), seq.markers[0].color), ("Done", MarkerColor::Green));

        let warned = |w: &str| result.warnings.iter().any(|x| x.contains(w));
        assert!(warned("adjust-transform") && warned("Title 'Ana Silva'"), "{:?}", result.warnings);
        assert!(!warned("Connected clip"), "connected clips are no longer dropped: {:?}", result.warnings);
    }

    #[test]
    fn parse_fcpxml_with_gap_skips_gap() {
        let result = parse_fcpxml(FCPXML_WITH_GAP).expect("parse ok");
        let track = &result.sequence.tracks[0];
        assert_eq!(track.clips.len(), 2, "two clips present, gap itself skipped");
        // First clip at 0, second at offset 60 (because gap pushed it forward).
        assert_eq!(track.clips[0].timeline_in.value, 0);
        assert_eq!(track.clips[1].timeline_in.value, 60);
    }

    #[test]
    fn parse_fcpxml_marker_imported() {
        let result = parse_fcpxml(FCPXML_WITH_MARKER).expect("parse ok");
        let track = &result.sequence.tracks[0];
        assert_eq!(track.clips.len(), 1);
        assert!(
            !track.clips[0].clip_markers.is_empty(),
            "clip should have at least one marker"
        );
        assert_eq!(track.clips[0].clip_markers[0].name, "My Marker");
    }

    #[test]
    fn parse_fcpxml_invalid_xml_returns_error() {
        let result = parse_fcpxml("<not-fcpxml></not-fcpxml>");
        assert!(result.is_err());
    }

    #[test]
    fn parse_fcpxml_missing_asset_ref_returns_error() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
  </resources>
  <library>
    <event name="E">
      <project name="P">
        <sequence duration="10/25s" format="r0">
          <spine>
            <asset-clip ref="r999" offset="0/25s" duration="10/25s" start="0/25s"/>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;
        let result = parse_fcpxml(xml);
        assert!(matches!(result, Err(FcpxmlImportError::MissingAsset(_))));
    }

    #[test]
    fn parse_fcpxml_2997_roundtrip() {
        // Sanity: 30000/1001 sequence with a 1-second clip resolves to
        // ~30 frames at the sequence rate (30000).
        let result = parse_fcpxml(FCPXML_2997).expect("parse ok");
        assert_eq!(result.sequence.fps, Fps::new(30000, 1001));
        let clip = &result.sequence.tracks[0].clips[0];
        // duration = 30030/30000 seconds = 30030 * 30000 / (30000 * 1001) frames
        // = 900900000 / 30030000 = 30 frames at rate=30000.
        assert_eq!(clip.timeline_out.value - clip.timeline_in.value, 30);
    }

    #[test]
    fn parse_fcpxml_marker_with_note_recovers_comment() {
        // FCPXML wraps marker comments in a child <note> element.
        // Round-trip: a <marker><note>…</note></marker> shape must
        // come back as Marker.comment.
        const FCPXML_WITH_NOTED_MARKER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.9">
  <resources>
    <format id="r0" frameDuration="1/25s" width="1920" height="1080"/>
    <asset id="r1" name="clip.mp4" src="file:///tmp/clip.mp4"
           duration="100/25s" format="r0"/>
  </resources>
  <library>
    <event name="Event">
      <project name="Project">
        <sequence duration="100/25s" format="r0">
          <spine>
            <asset-clip name="clip.mp4" ref="r1"
                        offset="0/25s" duration="100/25s" start="0/25s"/>
          </spine>
          <marker start="25/25s" duration="1/25s" value="Cut on action">
            <note>Trim two frames off the head</note>
          </marker>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>"#;
        let result = parse_fcpxml(FCPXML_WITH_NOTED_MARKER).expect("parse ok");
        assert_eq!(result.sequence.markers.len(), 1);
        let m = &result.sequence.markers[0];
        assert_eq!(m.label, "Cut on action");
        assert_eq!(m.comment, "Trim two frames off the head");
    }
}
