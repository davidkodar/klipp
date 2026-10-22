//! OpenTimelineIO (OTIO) project import.
//!
//! Parses `.otio` JSON files (plain, not bundled) and produces a Klipp
//! `Sequence` plus a populated `MediaPool`. Unsupported features (effects,
//! transitions, nested stacks) are dropped with warnings that the caller
//! is expected to write alongside the imported project.
//!
//! Scope is deliberately MVP: tracks, clips, gaps, media references
//! (external + missing), markers (timeline and clip level), and rate-
//! accurate times. Plain `.otio` only — `.otiod` directory bundles and
//! `.otioz` zip archives are out of scope.
//!
//! Parser is serde_json-driven: children of Stacks and Tracks are mixed-
//! schema, so they're parsed as `serde_json::Value` first, then dispatched
//! on the `OTIO_SCHEMA` string. Never panics — all errors are returned as
//! `OtioImportError`.
//!
//! The `opentimelineio` crate on crates.io is a placeholder and is not used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use klipp_core::{Fps, MarkerColor, MarkerId, MediaId, RationalTime, Resolution, SequenceId};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{Clip, ClipKind, ClipMarker, Marker, Sequence, Track, TrackKind};
use serde::Deserialize;

pub use crate::mlt_import::ImportResult;

/// Error type for OTIO import failures.
#[derive(Debug, thiserror::Error)]
pub enum OtioImportError {
    #[error("Failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse OTIO JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OTIO schema error: {0}")]
    Schema(String),
    #[error("No timeline found in OTIO file")]
    NoTimeline,
}

/// `RationalTime.1` payload — both fields are `f64` in OTIO.
///
/// Kept internal; callers should always go through `rt_from_otio` or
/// `fps_from_otio_rate` to reach Klipp's RationalTime/Fps conventions.
#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct RawRationalTime {
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub value: f64,
}

/// `TimeRange.1` payload — `start_time` + `duration` (NOT start + end).
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct RawTimeRange {
    pub start_time: RawRationalTime,
    pub duration: RawRationalTime,
}

/// Top-level OTIO file — always `OTIO_SCHEMA == "Timeline.?"`.
#[derive(Debug, Deserialize)]
pub(crate) struct RawTimeline {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub global_start_time: Option<RawRationalTime>,
    pub tracks: RawStack,
}

/// `Stack.1` — the container below Timeline that holds tracks. Stack-level
/// markers are treated as timeline markers in the MVP.
#[derive(Debug, Deserialize)]
pub(crate) struct RawStack {
    #[serde(rename = "OTIO_SCHEMA")]
    pub schema: String,
    /// Children are mixed-schema (Track, and — rarely — nested Stack, Clip).
    /// Parsed as `Value` and dispatched on `OTIO_SCHEMA`.
    #[serde(default)]
    pub children: Vec<serde_json::Value>,
    /// Stack-level markers become timeline (Sequence) markers.
    #[serde(default)]
    pub markers: Vec<serde_json::Value>,
}

/// `Track.1` — a single timeline track (Video/Audio).
#[derive(Debug, Deserialize)]
pub(crate) struct RawTrack {
    // Schema string is validated at dispatch time via child_schema() — carried
    // here for completeness only.
    #[serde(rename = "OTIO_SCHEMA")]
    #[allow(dead_code)]
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `"Video"` or `"Audio"`. Unknown values default to Video with warning.
    pub kind: String,
    /// Mixed schemas: `Clip.?`, `Gap.?`, `Transition.?`.
    #[serde(default)]
    pub children: Vec<serde_json::Value>,
    /// Track-level markers are dropped with a warning — Klipp has no such concept.
    #[serde(default)]
    pub markers: Vec<serde_json::Value>,
}

/// `Clip.?` — both schema versions share the same shape for our purposes,
/// except for the media-reference field name (singular vs map).
#[derive(Debug, Deserialize)]
pub(crate) struct RawClip {
    // Schema string is validated at dispatch time via child_schema() — carried
    // here for completeness only.
    #[serde(rename = "OTIO_SCHEMA")]
    #[allow(dead_code)]
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub source_range: Option<RawTimeRange>,
    /// Old schema (`Clip.1`) — singular reference, directly inlined.
    #[serde(default)]
    pub media_reference: Option<serde_json::Value>,
    /// New schema (`Clip.2+`) — map of named references with an active key.
    #[serde(default)]
    pub media_references: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub active_media_reference_key: Option<String>,
    /// Clip markers are placed in source-relative coordinates.
    #[serde(default)]
    pub markers: Vec<serde_json::Value>,
    /// OTIO `enabled` defaults to `true` — `false` means the editor
    /// disabled the clip.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Effects (LinearTimeWarp / FreezeFrame / colour ops). Klipp drops
    /// these but emits a warning so the user knows.
    #[serde(default)]
    pub effects: Vec<serde_json::Value>,
    /// Editor's in/out points within the source — when present, narrows
    /// `source_range` further.
    #[serde(default)]
    pub marked_range: Option<RawTimeRange>,
}

fn default_true() -> bool { true }

/// `Gap.?` — placeholder entry whose `source_range.duration` is the only
/// field we care about.
#[derive(Debug, Deserialize)]
pub(crate) struct RawGap {
    #[serde(rename = "OTIO_SCHEMA")]
    #[allow(dead_code)] // read via child_schema() at dispatch time
    pub schema: String,
    pub source_range: RawTimeRange,
}

/// `Marker.?` — a named point (or range) with a color. Valid on Clip,
/// Track, and Stack children; positional semantics differ by parent.
#[derive(Debug, Deserialize)]
pub(crate) struct RawMarker {
    #[serde(rename = "OTIO_SCHEMA")]
    #[allow(dead_code)] // dispatched via child_schema() before deserialisation
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    pub marked_range: RawTimeRange,
}

/// Map an OTIO `rate` (plain `f64`) to a Klipp `Fps` using the SMPTE
/// num/den convention already used elsewhere in the project.
///
/// Recognised fractional rates collapse to the canonical constructors so
/// timeline maths stays frame-accurate. Anything unrecognised is rounded
/// to the nearest integer rate — good enough for MVP; real NTSC content
/// should hit the explicit branches.
pub(crate) fn fps_from_otio_rate(rate: f64) -> Fps {
    const EPS: f64 = 0.01;
    if (rate - 23.976).abs() < EPS || (rate - 23.98).abs() < EPS {
        Fps::ntsc_23976()
    } else if (rate - 29.97).abs() < EPS {
        Fps::ntsc_29970()
    } else if (rate - 59.94).abs() < EPS {
        Fps::new(60000, 1001)
    } else {
        // Integer or unrecognised: round up to a sane minimum of 1.
        Fps::new(rate.round().max(1.0) as u32, 1)
    }
}

/// Convert an OTIO `RationalTime` to a Klipp `RationalTime` at a caller-
/// specified target rate (normally the sequence fps's `rate()`).
///
/// The conversion goes via seconds to sidestep the difference between
/// OTIO's frames-at-rate convention and Klipp's num/den-derived rate.
/// For example OTIO `{rate: 23.976, value: 24.0}` (= 1.001 s) at target
/// rate 24000 produces `{value: 24024, rate: 24000}` — i.e. 24 NTSC
/// frames × 1001 ticks/frame, matching `Fps::ntsc_23976()`.
pub(crate) fn rt_from_otio(rt: &RawRationalTime, target_rate: u32) -> RationalTime {
    let secs = if rt.rate > 0.0 { rt.value / rt.rate } else { 0.0 };
    RationalTime::from_secs_f64(secs, target_rate)
}

/// Map an OTIO marker color string (UPPERCASE, e.g. `"RED"`, `"CYAN"`)
/// to the nearest Klipp `MarkerColor`. Unknown or missing colors default
/// to `Blue` — the same default Klipp uses elsewhere.
pub(crate) fn otio_color_to_klipp(color: Option<&str>) -> MarkerColor {
    match color.unwrap_or("") {
        "RED" => MarkerColor::Red,
        "ORANGE" => MarkerColor::Orange,
        "YELLOW" => MarkerColor::Yellow,
        "GREEN" => MarkerColor::Green,
        "BLUE" | "CYAN" => MarkerColor::Blue,
        "PURPLE" | "MAGENTA" | "PINK" => MarkerColor::Purple,
        _ => MarkerColor::Blue,
    }
}

/// Read a child's `OTIO_SCHEMA` string, or empty if absent. Used to dispatch
/// mixed-schema arrays (track children, stack children) without eagerly
/// deserialising into a fixed type.
fn child_schema(v: &serde_json::Value) -> &str {
    v.get("OTIO_SCHEMA").and_then(|s| s.as_str()).unwrap_or("")
}

/// Resolve the active media reference for a clip, handling both schema
/// versions.
///
/// - `Clip.1` inlines a singular `media_reference` field.
/// - `Clip.2+` stores a `media_references` map plus an
///   `active_media_reference_key` (defaults to `"DEFAULT_MEDIA"`).
///
/// Returns the JSON value of the active reference, or `None` if no reference
/// is present at all. Keys that don't resolve fall back to the first map entry
/// so a malformed `active_media_reference_key` doesn't silently drop media.
fn active_media_ref(clip: &RawClip) -> Option<&serde_json::Value> {
    if let Some(refs) = &clip.media_references {
        let key = clip.active_media_reference_key.as_deref().unwrap_or("DEFAULT_MEDIA");
        return refs.get(key).or_else(|| refs.values().next());
    }
    clip.media_reference.as_ref()
}

/// Convert an OTIO `target_url` into a concrete filesystem path. Accepts:
///   - `file:///absolute/path` (three slashes — what Python OTIO emits)
///   - `file://host/path` (rare; dropped to `/path` since we're local-only)
///   - plain absolute path `/foo/bar.mp4`
///   - plain relative path `media/foo.mp4` (resolved against `project_dir`)
fn resolve_target_url(url: &str, project_dir: &Path) -> Option<PathBuf> {
    let url = url.trim();
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let raw = raw.strip_prefix('/').map(|s| format!("/{s}")).unwrap_or_else(|| raw.to_string());
    crate::path_util::safe_resolve(project_dir, &PathBuf::from(raw))
}

/// Build a fully-defaulted `MediaItem` for OTIO imports. Fields beyond the
/// bare essentials are zeroed or defaulted — Klipp will re-probe on demand
/// (see `needs_reprobe` in the MediaItem). Marked offline when we can't
/// verify the file exists.
fn make_otio_media_item(path: PathBuf, filename: String, fps: Fps, offline: bool) -> MediaItem {
    MediaItem {
        id: MediaId::new(),
        path,
        kind: MediaItemKind::File,
        filename,
        duration: RationalTime::ZERO,
        fps,
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
    }
}

/// Register a media reference into the pool and return its `MediaId`.
///
/// - `ExternalReference` whose file exists on disk: dedupe via `find_by_path`,
///   register online.
/// - `ExternalReference` whose file is missing: register with `offline: true`
///   so the user can relink it later (mirrors MLT importer behaviour).
/// - `MissingReference`: register with `offline: true` under the clip's
///   display name. Each clip gets its own `MediaId` — no dedupe.
fn register_otio_media_ref(
    media_pool: &mut MediaPool,
    media_ref: &serde_json::Value,
    clip_name: Option<&str>,
    project_dir: &Path,
    fps: Fps,
    warnings: &mut Vec<String>,
) -> MediaId {
    let schema = child_schema(media_ref);
    if schema.starts_with("ExternalReference.") {
        let target_url = media_ref.get("target_url").and_then(|v| v.as_str()).unwrap_or("");
        if target_url.is_empty() {
            warnings.push(format!(
                "Clip '{}' has ExternalReference without target_url — treated as offline",
                clip_name.unwrap_or("<unnamed>"),
            ));
            let name = clip_name.unwrap_or("<missing>").to_string();
            return media_pool.add(make_otio_media_item(PathBuf::new(), name, fps, true));
        }
        let Some(path) = resolve_target_url(target_url, project_dir) else {
            warnings.push(format!(
                "Path traversal blocked for media: {} — imported as offline",
                target_url,
            ));
            let name = clip_name.unwrap_or("<missing>").to_string();
            return media_pool.add(make_otio_media_item(PathBuf::new(), name, fps, true));
        };
        if path.exists() {
            if let Some(existing) = media_pool.find_by_path(&path) {
                return existing.id;
            }
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            media_pool.add(make_otio_media_item(path, filename, fps, false))
        } else {
            warnings.push(format!(
                "Media file not found: {} — imported as offline",
                path.display(),
            ));
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(clip_name.unwrap_or("<missing>"))
                .to_string();
            media_pool.add(make_otio_media_item(path, filename, fps, true))
        }
    } else if schema.starts_with("MissingReference.") {
        let name = clip_name.unwrap_or("<missing>").to_string();
        media_pool.add(make_otio_media_item(PathBuf::new(), name, fps, true))
    } else {
        warnings.push(format!(
            "Media reference schema '{schema}' not supported — clip '{}' treated as offline",
            clip_name.unwrap_or("<unnamed>"),
        ));
        let name = clip_name.unwrap_or("<missing>").to_string();
        media_pool.add(make_otio_media_item(PathBuf::new(), name, fps, true))
    }
}

/// Parse a heterogeneous markers array, collecting only valid `Marker.?`
/// entries. Malformed entries (missing `marked_range`, unexpected schema)
/// are recorded in `warnings` and skipped — the rest of the import still
/// lands.
fn parse_markers(
    raw: &[serde_json::Value],
    scope_label: &str,
    warnings: &mut Vec<String>,
) -> Vec<RawMarker> {
    let mut out = Vec::new();
    for v in raw {
        let schema = child_schema(v);
        if !schema.starts_with("Marker.") {
            warnings.push(format!(
                "{scope_label}: unexpected marker schema '{schema}' — skipped",
            ));
            continue;
        }
        match serde_json::from_value::<RawMarker>(v.clone()) {
            Ok(m) => out.push(m),
            Err(e) => warnings.push(format!(
                "{scope_label}: marker parse failed: {e} — skipped",
            )),
        }
    }
    out
}

/// Choose a sequence fps. Priority: `global_start_time.rate`, then the rate
/// of the first clip's `source_range.start_time`, else 24.
fn sequence_fps(doc: &RawTimeline) -> Fps {
    if let Some(gst) = doc.global_start_time {
        if gst.rate > 0.0 {
            return fps_from_otio_rate(gst.rate);
        }
    }
    for child_val in &doc.tracks.children {
        if !child_schema(child_val).starts_with("Track.") {
            continue;
        }
        let Ok(track) = serde_json::from_value::<RawTrack>(child_val.clone()) else {
            continue;
        };
        for cv in &track.children {
            if !child_schema(cv).starts_with("Clip.") {
                continue;
            }
            let Ok(clip) = serde_json::from_value::<RawClip>(cv.clone()) else {
                continue;
            };
            if let Some(sr) = clip.source_range {
                if sr.start_time.rate > 0.0 {
                    return fps_from_otio_rate(sr.start_time.rate);
                }
            }
        }
    }
    Fps::film_24()
}

/// Build one Klipp `Track` from a parsed `RawTrack`. Iterates mixed-schema
/// children, dispatches on `OTIO_SCHEMA`, and builds `Clip`s / gap clips.
/// Transitions and unknown schemas are dropped with a warning (full
/// transition handling is a later commit).
fn build_track(
    raw: &RawTrack,
    fps_rate: u32,
    project_dir: &Path,
    media_pool: &mut MediaPool,
    warnings: &mut Vec<String>,
) -> Track {
    let kind = match raw.kind.as_str() {
        "Video" => TrackKind::Video,
        "Audio" => TrackKind::Audio,
        other => {
            warnings.push(format!(
                "Unknown track kind '{other}' — defaulting to Video",
            ));
            TrackKind::Video
        }
    };
    let name = raw.name.clone().unwrap_or_else(|| "Imported".to_string());
    tracing::debug!(name = %name, kind = ?kind, "OTIO track");
    let mut track = Track::new(&name, kind);

    // Cursor tracks where on the timeline the next clip/gap starts. Rooted
    // at 0 in the sequence's rate units so every clip timeline_in sits on a
    // frame boundary.
    let mut cursor = RationalTime {
        value: 0,
        rate: fps_rate,
    };

    for child_val in &raw.children {
        let schema = child_schema(child_val);
        if schema.starts_with("Clip.") {
            match serde_json::from_value::<RawClip>(child_val.clone()) {
                Ok(raw_clip) => {
                    match build_clip(&raw_clip, cursor, fps_rate, project_dir, media_pool, warnings) {
                        Some((clip, consumed)) => {
                            track.clips.push(clip);
                            cursor += consumed;
                        }
                        None => {
                            // Clip lacked a source_range — skip it entirely
                            // (cursor doesn't advance since we don't know its
                            // intended duration).
                            warnings.push(format!(
                                "Clip '{}' has no source_range — skipped",
                                raw_clip.name.as_deref().unwrap_or("<unnamed>"),
                            ));
                        }
                    }
                }
                Err(e) => warnings.push(format!("Clip parse failed: {e} — skipped")),
            }
        } else if schema.starts_with("Gap.") {
            match serde_json::from_value::<RawGap>(child_val.clone()) {
                Ok(raw_gap) => {
                    let dur = rt_from_otio(&raw_gap.source_range.duration, fps_rate);
                    if dur.value > 0 {
                        track.clips.push(Clip::gap(cursor, cursor + dur));
                    }
                    cursor += dur;
                }
                Err(e) => warnings.push(format!("Gap parse failed: {e} — skipped")),
            }
        } else if schema.starts_with("Transition.") {
            // OTIO transitions overlap the adjacent clips (in_offset + out_offset)
            // and carry a transition_type enum. Klipp's transition model doesn't
            // fit the OTIO overlap semantics directly — drop with a specific
            // warning. Crucially the cursor does NOT advance: transitions are
            // conceptually zero-duration at the track timeline level.
            let kind = child_val
                .get("transition_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            warnings.push(format!(
                "Transition '{schema}' (type={kind}) not supported — skipped",
            ));
        } else {
            warnings.push(format!(
                "Track child schema '{schema}' not supported — skipped",
            ));
        }
    }

    // Track-level markers are parsed but dropped — Klipp's data model only
    // supports timeline (Sequence) and clip-scope markers.
    let track_markers = parse_markers(
        &raw.markers,
        &format!("Track '{}'", track.name),
        warnings,
    );
    if !track_markers.is_empty() {
        warnings.push(format!(
            "Track '{}': {} track-level marker(s) dropped — Klipp has no track markers",
            track.name,
            track_markers.len(),
        ));
    }

    track
}

/// Build one media `Clip` from a parsed `RawClip`. Returns `(clip, consumed)`
/// where `consumed` is the timeline duration to advance the cursor by. Returns
/// `None` when the clip has no `source_range` (malformed input).
fn build_clip(
    raw: &RawClip,
    cursor_timeline: RationalTime,
    fps_rate: u32,
    project_dir: &Path,
    media_pool: &mut MediaPool,
    warnings: &mut Vec<String>,
) -> Option<(Clip, RationalTime)> {
    let source_range = raw.source_range.as_ref()?;
    let source_in = rt_from_otio(&source_range.start_time, fps_rate);
    let duration = rt_from_otio(&source_range.duration, fps_rate);
    let source_out = source_in + duration;
    let timeline_in = cursor_timeline;
    let timeline_out = timeline_in + duration;

    // Resolve the active media reference and register it.
    let media_id = if let Some(mref) = active_media_ref(raw) {
        register_otio_media_ref(
            media_pool,
            mref,
            raw.name.as_deref(),
            project_dir,
            Fps::new(fps_rate.max(1), 1),
            warnings,
        )
    } else {
        warnings.push(format!(
            "Clip '{}' has no media reference — treated as offline",
            raw.name.as_deref().unwrap_or("<unnamed>"),
        ));
        let name = raw.name.clone().unwrap_or_else(|| "<missing>".to_string());
        media_pool.add(make_otio_media_item(
            PathBuf::new(),
            name,
            Fps::new(fps_rate.max(1), 1),
            true,
        ))
    };

    let mut clip = Clip::gap(timeline_in, timeline_out);
    clip.source_in = source_in;
    clip.source_out = source_out;
    clip.kind = ClipKind::Media { media_id };
    clip.enabled = raw.enabled;

    let clip_name = raw.name.as_deref().unwrap_or("<unnamed>");

    // marked_range narrows source_in/source_out to the editor's in/out
    // points within the source. Clamped to the existing source_range so a
    // malformed marked_range (extending past the loaded portion) can't
    // produce a garbage clip.
    if let Some(mr) = raw.marked_range.as_ref() {
        let new_in = rt_from_otio(&mr.start_time, fps_rate);
        let mr_dur = rt_from_otio(&mr.duration, fps_rate);
        let new_out = new_in + mr_dur;
        let in_secs = new_in.to_secs_f64();
        let out_secs = new_out.to_secs_f64();
        let lo_secs = clip.source_in.to_secs_f64();
        let hi_secs = clip.source_out.to_secs_f64();
        let clamped_in = if in_secs < lo_secs { clip.source_in } else { new_in };
        let clamped_out = if out_secs > hi_secs { clip.source_out } else { new_out };
        if clamped_out > clamped_in {
            clip.source_in = clamped_in;
            clip.source_out = clamped_out;
        }
    }

    // Effects (LinearTimeWarp, FreezeFrame, colour ops) are dropped. Emit
    // a specific warning for time warps since they change clip duration
    // and a generic one for everything else.
    if !raw.effects.is_empty() {
        let mut has_time_warp = false;
        for fx in &raw.effects {
            let schema = fx
                .get("OTIO_SCHEMA")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if schema.starts_with("LinearTimeWarp") || schema.starts_with("FreezeFrame") {
                has_time_warp = true;
                break;
            }
        }
        if has_time_warp {
            warnings.push(format!(
                "Clip '{clip_name}': speed ramp / freeze-frame effect not imported (clip will play at normal speed)"
            ));
        } else {
            warnings.push(format!(
                "Clip '{}': {} effect(s) not imported (OTIO effects are not supported — LinearTimeWarp, colour corrections etc. are dropped)",
                clip_name,
                raw.effects.len(),
            ));
        }
    }

    // OTIO clip markers sit in the source's coordinate system (same basis as
    // source_range). Klipp's `ClipMarker.time_secs` is expressed relative to
    // `source_in`, so subtract before converting to seconds.
    let source_in_secs = clip.source_in.to_secs_f64();
    for rm in parse_markers(&raw.markers, &format!("Clip '{clip_name}'"), warnings) {
        let absolute = rt_from_otio(&rm.marked_range.start_time, fps_rate);
        let time_secs = absolute.to_secs_f64() - source_in_secs;
        clip.clip_markers.push(ClipMarker {
            time_secs,
            name: rm.name.unwrap_or_default(),
            color: otio_color_to_klipp(rm.color.as_deref()),
            // OTIO carries marker comments in metadata extensions
            // (variable per writer); skip for now and let users edit
            // post-import.
            comment: String::new(),
        });
    }

    Some((clip, duration))
}

/// Parse an OpenTimelineIO `.otio` JSON file into a Klipp `Sequence` plus a
/// populated `MediaPool`.
///
/// Never panics — all errors are returned as `OtioImportError`. Unsupported
/// features (transitions, effects, nested stacks) are dropped and recorded in
/// `ImportResult::warnings` for the caller to surface.
pub fn load_otio(path: &Path) -> Result<ImportResult, OtioImportError> {
    tracing::info!(path = %path.display(), "OTIO import: starting");
    let bytes = crate::path_util::read_capped(path, crate::path_util::MAX_INPUT_FILE_BYTES)?;
    let doc: RawTimeline = serde_json::from_slice(&bytes)?;
    if !doc.schema.starts_with("Timeline.") {
        return Err(OtioImportError::Schema(format!(
            "root schema '{}' is not a Timeline",
            doc.schema
        )));
    }
    if !doc.tracks.schema.starts_with("Stack.") {
        return Err(OtioImportError::Schema(format!(
            "tracks container schema '{}' is not a Stack",
            doc.tracks.schema
        )));
    }

    let fps = sequence_fps(&doc);
    let fps_rate = fps.rate();
    tracing::info!(
        fps = %fps.as_f64(),
        fps_rate,
        stack_children = doc.tracks.children.len(),
        "OTIO import: document parsed"
    );

    if doc.tracks.children.is_empty() {
        return Err(OtioImportError::NoTimeline);
    }

    let project_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut media_pool = MediaPool::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut tracks: Vec<Track> = Vec::new();
    for child_val in &doc.tracks.children {
        let schema = child_schema(child_val);
        if schema.starts_with("Track.") {
            match serde_json::from_value::<RawTrack>(child_val.clone()) {
                Ok(raw_track) => {
                    let track = build_track(
                        &raw_track,
                        fps_rate,
                        &project_dir,
                        &mut media_pool,
                        &mut warnings,
                    );
                    tracks.push(track);
                }
                Err(e) => warnings.push(format!("Track parse failed: {e} — skipped")),
            }
        } else if schema.starts_with("Stack.") {
            warnings.push(format!(
                "Nested Stack '{schema}' in timeline — not supported, skipped",
            ));
        } else if !schema.is_empty() {
            warnings.push(format!(
                "Stack child schema '{schema}' not a Track — skipped",
            ));
        }
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Imported OTIO".to_string());

    // Stack-level markers become timeline markers on the Sequence.
    let mut sequence_markers: Vec<Marker> = Vec::new();
    for rm in parse_markers(&doc.tracks.markers, "Timeline", &mut warnings) {
        let position = rt_from_otio(&rm.marked_range.start_time, fps_rate);
        // OTIO markers always carry a duration. Import anything > 0 frames
        // as a range marker; zero-duration stays a point marker.
        let duration = rt_from_otio(&rm.marked_range.duration, fps_rate);
        let duration = if duration.value > 0 { Some(duration) } else { None };
        sequence_markers.push(Marker { review: None,
            id: MarkerId::new(),
            position,
            label: rm.name.unwrap_or_default(),
            color: otio_color_to_klipp(rm.color.as_deref()),
            duration,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });
    }

    let sequence = Sequence {
        // No interchange format carries a camera.
        camera: klipp_timeline::Camera::default(),
        timeline_graph: None,
        grade_groups: Vec::new(),
        // An imported timeline takes its rate's convention.
        title_nits: klipp_timeline::DEFAULT_TITLE_NITS,
        node_labels: Vec::new(),
        drop_frame: Sequence::default_drop_frame(fps),
        id: SequenceId::new(),
        name: doc.name.unwrap_or(name),
        fps,
        resolution: Resolution {
            width: 1920,
            height: 1080,
        },
        first_clip_autoset: Default::default(),
        tracks,
        in_point: None,
        out_point: None,
        markers: sequence_markers,
        last_playhead: None,
        brief: None,
        edit_plan: None,
        reviews: Vec::new(),
        awaiting_refit: false,
    };

    tracing::info!(
        tracks = sequence.tracks.len(),
        total_clips = sequence.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
        sequence_markers = sequence.markers.len(),
        clip_markers = sequence
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.clip_markers.len())
            .sum::<usize>(),
        warnings = warnings.len(),
        "OTIO import: done"
    );

    Ok(ImportResult {
        sequence,
        media_pool,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_MULTITRACK: &str =
        include_str!("../tests/fixtures/otio_multitrack.otio");

    #[test]
    fn fps_integer_rate() {
        assert_eq!(fps_from_otio_rate(24.0), Fps::film_24());
        assert_eq!(fps_from_otio_rate(25.0), Fps::pal_25());
        assert_eq!(fps_from_otio_rate(30.0), Fps::new(30, 1));
        assert_eq!(fps_from_otio_rate(60.0), Fps::new(60, 1));
    }

    #[test]
    fn fps_ntsc_fractional_rates() {
        assert_eq!(fps_from_otio_rate(23.976), Fps::ntsc_23976());
        assert_eq!(fps_from_otio_rate(23.98), Fps::ntsc_23976());
        assert_eq!(fps_from_otio_rate(29.97), Fps::ntsc_29970());
        assert_eq!(fps_from_otio_rate(59.94), Fps::new(60000, 1001));
    }

    #[test]
    fn fps_unrecognised_rate_rounds_to_integer() {
        // Exotic rate: defaults to rounded integer rather than scaling.
        assert_eq!(fps_from_otio_rate(48.0), Fps::new(48, 1));
        // Degenerate input doesn't panic.
        assert_eq!(fps_from_otio_rate(0.0), Fps::new(1, 1));
        assert_eq!(fps_from_otio_rate(-5.0), Fps::new(1, 1));
    }

    #[test]
    fn rt_integer_rate_round_trip() {
        // 12 frames at 24 fps → 0.5 s → 12 frames at target rate 24.
        let r = RawRationalTime { rate: 24.0, value: 12.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 12, rate: 24 });
    }

    #[test]
    fn rt_ntsc_fractional_preserves_frame_count() {
        // 24 frames at 23.976 fps = 1.001 s. Target rate 24000 (ntsc_23976
        // canonical) should map to value 24024 = 24 × 1001 ticks/frame.
        let r = RawRationalTime { rate: 23.976, value: 24.0 };
        let converted = rt_from_otio(&r, 24000);
        // Allow ±1 tick tolerance for f64 rounding (24.0/23.976 is not
        // exactly 1.001).
        assert_eq!(converted.rate, 24000);
        let diff = (converted.value - 24024).abs();
        assert!(diff <= 1, "expected ~24024 ticks, got {}", converted.value);
    }

    #[test]
    fn rt_zero_rate_is_safe() {
        // Malformed input must not divide by zero.
        let r = RawRationalTime { rate: 0.0, value: 100.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 0, rate: 24 });
    }

    #[test]
    fn rt_audio_rate_normalises_to_video_sequence_rate() {
        // Audio at 48 kHz sampled at value=48000 (= 1 second) should produce
        // 24 frames at a 24 fps sequence rate — mixed-rate content converges.
        let r = RawRationalTime { rate: 48000.0, value: 48000.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 24, rate: 24 });
    }

    #[test]
    fn color_known_mappings() {
        assert_eq!(otio_color_to_klipp(Some("RED")), MarkerColor::Red);
        assert_eq!(otio_color_to_klipp(Some("ORANGE")), MarkerColor::Orange);
        assert_eq!(otio_color_to_klipp(Some("YELLOW")), MarkerColor::Yellow);
        assert_eq!(otio_color_to_klipp(Some("GREEN")), MarkerColor::Green);
        assert_eq!(otio_color_to_klipp(Some("BLUE")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("CYAN")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("PURPLE")), MarkerColor::Purple);
        assert_eq!(otio_color_to_klipp(Some("MAGENTA")), MarkerColor::Purple);
        assert_eq!(otio_color_to_klipp(Some("PINK")), MarkerColor::Purple);
    }

    #[test]
    fn color_unknown_and_missing_default_to_blue() {
        assert_eq!(otio_color_to_klipp(Some("BLACK")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("WHITE")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("chartreuse")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(None), MarkerColor::Blue);
    }

    #[test]
    fn raw_rational_time_deserialises() {
        let raw: RawRationalTime = serde_json::from_str(
            r#"{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":48.0}"#,
        )
        .unwrap();
        assert_eq!(raw, RawRationalTime { rate: 24.0, value: 48.0 });
    }

    #[test]
    fn raw_rational_time_missing_fields_default_to_zero() {
        let raw: RawRationalTime = serde_json::from_str("{}").unwrap();
        assert_eq!(raw, RawRationalTime { rate: 0.0, value: 0.0 });
    }

    // ── active_media_ref — dual schema ───────────────────────────────

    fn parse_clip(json_str: &str) -> RawClip {
        serde_json::from_str(json_str).expect("valid RawClip JSON")
    }

    #[test]
    fn active_media_ref_old_schema() {
        // Clip.1 (OTIO < ~0.14) — singular media_reference directly on the clip.
        let clip = parse_clip(
            r#"{
                "OTIO_SCHEMA": "Clip.1",
                "name": "old_clip",
                "media_reference": {
                    "OTIO_SCHEMA": "MissingReference.1",
                    "available_range": null
                }
            }"#,
        );
        let m = active_media_ref(&clip).expect("singular media_reference must resolve");
        assert_eq!(child_schema(m), "MissingReference.1");
    }

    #[test]
    fn active_media_ref_new_schema() {
        // Clip.2+ — media_references map with an explicit active key.
        let clip = parse_clip(
            r#"{
                "OTIO_SCHEMA": "Clip.2",
                "media_references": {
                    "ORIG": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": "/a.mp4" },
                    "PROXY": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": "/b.mp4" }
                },
                "active_media_reference_key": "PROXY"
            }"#,
        );
        let m = active_media_ref(&clip).expect("must resolve via active_media_reference_key");
        assert_eq!(
            m.get("target_url").and_then(|v| v.as_str()),
            Some("/b.mp4"),
            "active key PROXY must select /b.mp4, not the first map entry",
        );
    }

    #[test]
    fn active_media_ref_new_schema_default_key() {
        // No active_media_reference_key — the implicit default is DEFAULT_MEDIA.
        let clip = parse_clip(
            r#"{
                "OTIO_SCHEMA": "Clip.2",
                "media_references": {
                    "DEFAULT_MEDIA": {
                        "OTIO_SCHEMA": "ExternalReference.1",
                        "target_url": "/default.mp4"
                    }
                }
            }"#,
        );
        let m = active_media_ref(&clip).expect("DEFAULT_MEDIA must be the implicit key");
        assert_eq!(
            m.get("target_url").and_then(|v| v.as_str()),
            Some("/default.mp4"),
        );
    }

    #[test]
    fn active_media_ref_missing_key_falls_back_to_first() {
        // active_media_reference_key names a missing key — fall back to the
        // first map entry so malformed input still produces a valid clip.
        let clip = parse_clip(
            r#"{
                "OTIO_SCHEMA": "Clip.2",
                "media_references": {
                    "FIRST": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": "/first.mp4" }
                },
                "active_media_reference_key": "NONEXISTENT"
            }"#,
        );
        let m = active_media_ref(&clip).expect("fallback to first entry");
        assert_eq!(m.get("target_url").and_then(|v| v.as_str()), Some("/first.mp4"));
    }

    #[test]
    fn active_media_ref_none_when_no_fields() {
        let clip = parse_clip(r#"{ "OTIO_SCHEMA": "Clip.2" }"#);
        assert!(active_media_ref(&clip).is_none());
    }

    // ── markers ───────────────────────────────────────────────────

    #[test]
    fn parse_markers_valid_and_malformed_mix() {
        // Two valid markers + one with bad schema + one with missing
        // marked_range. Valid ones come through; bad ones become warnings.
        let raw = vec![
            serde_json::from_str(
                r#"{"OTIO_SCHEMA":"Marker.1","name":"a","color":"RED",
                    "marked_range":{"OTIO_SCHEMA":"TimeRange.1",
                        "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":5.0},
                        "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":0.0}}}"#,
            ).unwrap(),
            serde_json::from_str(r#"{"OTIO_SCHEMA":"Effect.1","name":"x"}"#).unwrap(),
            serde_json::from_str(r#"{"OTIO_SCHEMA":"Marker.1","name":"broken"}"#).unwrap(),
            serde_json::from_str(
                r#"{"OTIO_SCHEMA":"Marker.2","name":"b","color":"GREEN",
                    "marked_range":{"OTIO_SCHEMA":"TimeRange.1",
                        "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":10.0},
                        "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":0.0}}}"#,
            ).unwrap(),
        ];
        let mut warnings = Vec::new();
        let out = parse_markers(&raw, "TEST", &mut warnings);

        assert_eq!(out.len(), 2, "two valid markers");
        assert_eq!(out[0].name.as_deref(), Some("a"));
        assert_eq!(out[1].name.as_deref(), Some("b"));
        assert_eq!(warnings.len(), 2, "one unexpected schema + one malformed");
        assert!(warnings.iter().any(|w| w.contains("Effect.1")));
        assert!(warnings.iter().any(|w| w.contains("parse failed")));
    }

    #[test]
    fn resolve_target_url_absolute_and_relative() {
        let dir = Path::new("/proj");
        assert_eq!(
            resolve_target_url("/abs/clip.mp4", dir),
            Some(PathBuf::from("/abs/clip.mp4"))
        );
        assert_eq!(
            resolve_target_url("media/clip.mp4", dir),
            Some(PathBuf::from("/proj/media/clip.mp4"))
        );
        assert_eq!(
            resolve_target_url("file:///abs/clip.mp4", dir),
            Some(PathBuf::from("/abs/clip.mp4")),
            "file:// prefix must be stripped"
        );
    }

    #[test]
    fn resolve_target_url_traversal_blocked() {
        assert_eq!(
            resolve_target_url("../../etc/passwd", Path::new("/proj")),
            None,
        );
    }

    // ── End-to-end against the real multitrack.otio fixture ─────────

    #[test]
    fn load_otio_multitrack_fixture() {
        // The fixture is the canonical OpenTimelineIO `multitrack.otio` —
        // 3 Video tracks, 5 Clip.1 entries (all MissingReference), 7 Gaps,
        // rate 24 throughout, no transitions, no markers.
        let tmp = std::env::temp_dir().join("klipp_otio_multitrack.otio");
        std::fs::write(&tmp, FIXTURE_MULTITRACK).unwrap();
        let result = load_otio(&tmp).expect("import should succeed");
        let _ = std::fs::remove_file(&tmp);

        // Three tracks, all Video.
        assert_eq!(result.sequence.tracks.len(), 3);
        for t in &result.sequence.tracks {
            assert_eq!(t.kind, TrackKind::Video, "track '{}' must be Video", t.name);
        }

        // Clip / gap counts per track — match the audit of the file.
        let counts: Vec<(usize, usize)> = result
            .sequence
            .tracks
            .iter()
            .map(|t| {
                let clips = t.clips.iter().filter(|c| matches!(c.kind, ClipKind::Media { .. })).count();
                let gaps = t.clips.iter().filter(|c| matches!(c.kind, ClipKind::Gap)).count();
                (clips, gaps)
            })
            .collect();
        assert_eq!(counts, vec![(3, 3), (1, 2), (1, 2)], "per-track clip/gap layout");

        // 5 Clip.1 entries → 5 media items in the pool, all offline
        // (the fixture has only MissingReference).
        let all: Vec<_> = result.media_pool.all().collect();
        assert_eq!(all.len(), 5, "one MediaItem per Clip, no dedupe across missing refs");
        for item in &all {
            assert!(item.offline, "MissingReference → offline MediaItem");
        }

        // Sequence fps = 24 (from the fixture's RationalTimes).
        assert_eq!(result.sequence.fps, Fps::film_24());

        // Fixture has no markers or transitions — warnings should be empty.
        assert!(
            result.warnings.is_empty(),
            "no unexpected warnings for a clean fixture, got: {:?}",
            result.warnings,
        );

        // Clip timing spot-check: track 0 clip 0 is a 720-frame media clip
        // starting at timeline position 0.
        let t0_c0 = &result.sequence.tracks[0].clips[0];
        assert!(matches!(t0_c0.kind, ClipKind::Media { .. }));
        assert_eq!(t0_c0.timeline_in.value, 0);
        assert_eq!(t0_c0.timeline_in.rate, 24);
        // source_range had start=0, duration=720 at rate 24.
        assert_eq!(t0_c0.source_in.value, 0);
        assert_eq!(
            (t0_c0.source_out.value - t0_c0.source_in.value),
            720,
            "clip duration should be 720 frames",
        );
    }

    // ── Warning-text + drop-case coverage ────────────────────────────

    /// Build a minimal valid Timeline JSON wrapping one Video track whose
    /// `children` array is the supplied JSON value. Keeps the boilerplate
    /// out of every drop-case test.
    fn timeline_with_track_children(children_json: serde_json::Value, markers_json: serde_json::Value) -> String {
        serde_json::json!({
            "OTIO_SCHEMA": "Timeline.1",
            "metadata": {},
            "name": "test",
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "metadata": {},
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "metadata": {},
                    "name": "V1",
                    "kind": "Video",
                    "children": children_json,
                    "markers": markers_json,
                }]
            }
        }).to_string()
    }

    /// Write a JSON string to a temp file and load via `load_otio`, so a
    /// test can avoid wiring up tempfile boilerplate inline.
    fn load_inline(json: &str, slug: &str) -> ImportResult {
        let path = std::env::temp_dir().join(format!("klipp_otio_test_{slug}.otio"));
        std::fs::write(&path, json).unwrap();
        let result = load_otio(&path).expect("import should succeed");
        let _ = std::fs::remove_file(&path);
        result
    }

    fn warnings_contain_substring(warnings: &[String], needle: &str) -> bool {
        let lc = needle.to_lowercase();
        warnings.iter().any(|w| w.to_lowercase().contains(&lc))
    }

    #[test]
    fn transition_drop_produces_warning() {
        let json = timeline_with_track_children(
            serde_json::json!([
                {
                    "OTIO_SCHEMA": "Clip.1",
                    "name": "A",
                    "source_range": {
                        "OTIO_SCHEMA": "TimeRange.1",
                        "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                        "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                    },
                    "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
                },
                {
                    "OTIO_SCHEMA": "Transition.1",
                    "name": "fade",
                    "transition_type": "SMPTE_Dissolve",
                    "in_offset":  { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 6.0 },
                    "out_offset": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 6.0 }
                },
                {
                    "OTIO_SCHEMA": "Clip.1",
                    "name": "B",
                    "source_range": {
                        "OTIO_SCHEMA": "TimeRange.1",
                        "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                        "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                    },
                    "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
                },
            ]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "transition");
        assert!(
            warnings_contain_substring(&result.warnings, "transition"),
            "warnings should mention 'transition', got: {:?}", result.warnings,
        );
    }

    #[test]
    fn track_marker_drop_produces_warning() {
        let json = timeline_with_track_children(
            serde_json::json!([
                {
                    "OTIO_SCHEMA": "Clip.1",
                    "name": "A",
                    "source_range": {
                        "OTIO_SCHEMA": "TimeRange.1",
                        "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                        "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                    },
                    "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
                }
            ]),
            serde_json::json!([{
                "OTIO_SCHEMA": "Marker.1",
                "name": "track-mark",
                "color": "RED",
                "marked_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 12.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                }
            }]),
        );
        let result = load_inline(&json, "track_marker");
        assert!(
            warnings_contain_substring(&result.warnings, "marker"),
            "warnings should mention 'marker', got: {:?}", result.warnings,
        );
    }

    #[test]
    fn effect_on_clip_produces_warning() {
        let json = timeline_with_track_children(
            serde_json::json!([{
                "OTIO_SCHEMA": "Clip.1",
                "name": "fx_clip",
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} },
                "effects": [{
                    "OTIO_SCHEMA": "Effect.1",
                    "name": "SomeEffect",
                    "effect_name": "blur",
                    "metadata": {}
                }]
            }]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "effect");
        assert!(
            warnings_contain_substring(&result.warnings, "effect"),
            "warnings should mention 'effect', got: {:?}", result.warnings,
        );
    }

    #[test]
    fn linear_time_warp_produces_specific_warning() {
        let json = timeline_with_track_children(
            serde_json::json!([{
                "OTIO_SCHEMA": "Clip.1",
                "name": "warp_clip",
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} },
                "effects": [{
                    "OTIO_SCHEMA": "LinearTimeWarp.1",
                    "name": "slowmo",
                    "time_scalar": 0.5,
                    "metadata": {}
                }]
            }]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "time_warp");
        let mentions_speed_or_freeze = result.warnings.iter().any(|w| {
            let lw = w.to_lowercase();
            lw.contains("speed ramp") || lw.contains("freeze-frame")
        });
        assert!(
            mentions_speed_or_freeze,
            "warnings should mention 'speed ramp' or 'freeze-frame', got: {:?}",
            result.warnings,
        );
    }

    #[test]
    fn disabled_clip_imports_as_disabled() {
        let json = timeline_with_track_children(
            serde_json::json!([{
                "OTIO_SCHEMA": "Clip.1",
                "name": "off",
                "enabled": false,
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
            }]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "disabled");
        let clip = &result.sequence.tracks[0].clips[0];
        assert!(!clip.enabled, "disabled OTIO clip should land as Clip.enabled = false");
    }

    #[test]
    fn enabled_defaults_true_when_field_absent() {
        let json = timeline_with_track_children(
            serde_json::json!([{
                "OTIO_SCHEMA": "Clip.1",
                "name": "default",
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
            }]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "enabled_default");
        let clip = &result.sequence.tracks[0].clips[0];
        assert!(clip.enabled, "absent enabled field must default to true");
    }

    #[test]
    fn marked_range_narrows_source_in_out() {
        // source_range is 0..24 frames; marked_range narrows to 6..18.
        let json = timeline_with_track_children(
            serde_json::json!([{
                "OTIO_SCHEMA": "Clip.1",
                "name": "marked",
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                },
                "marked_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 6.0 },
                    "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 12.0 },
                },
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
            }]),
            serde_json::json!([]),
        );
        let result = load_inline(&json, "marked_range");
        let clip = &result.sequence.tracks[0].clips[0];
        assert_eq!(clip.source_in.value, 6, "source_in should narrow to 6");
        assert_eq!(
            clip.source_out.value - clip.source_in.value,
            12,
            "marked_range duration should win",
        );
    }

    #[test]
    fn unicode_clip_and_track_names_preserved() {
        // Track + clip names with non-ASCII characters and emoji must
        // survive parsing intact — Klipp uses String everywhere so this
        // mostly tests serde and our own field plumbing.
        let json = serde_json::json!({
            "OTIO_SCHEMA": "Timeline.1",
            "metadata": {},
            "name": "ünîcødé project",
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "metadata": {},
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "metadata": {},
                    "name": "Spår 1 — Björn 🎬",
                    "kind": "Video",
                    "children": [{
                        "OTIO_SCHEMA": "Clip.1",
                        "name": "Scène 42 – été",
                        "source_range": {
                            "OTIO_SCHEMA": "TimeRange.1",
                            "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                            "duration":   { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 },
                        },
                        "media_reference": { "OTIO_SCHEMA": "MissingReference.1", "metadata": {} }
                    }],
                    "markers": []
                }]
            }
        }).to_string();
        let result = load_inline(&json, "unicode");
        let track = &result.sequence.tracks[0];
        assert_eq!(track.name, "Spår 1 — Björn 🎬");
        // Clip name lands on the MediaItem.filename via register_otio_media_ref
        // when the reference is Missing (no URL to derive from).
        let item = result.media_pool.all().next().expect("one media item");
        assert!(
            item.filename.contains("Scène 42")
                || item.filename.contains("été"),
            "unicode clip name should reach the MediaItem, got '{}'",
            item.filename,
        );
    }

    // ── End-to-end against a hand-crafted fixture exercising drops ──

    #[test]
    fn load_otio_with_transitions_and_markers() {
        // Hand-crafted small fixture: one Video track with two clips,
        // a transition between them, a track marker, a clip marker on
        // the first clip, and a stack-level (sequence) marker.
        let fixture = include_str!(
            "../tests/fixtures/otio_with_transitions_and_markers.otio"
        );
        let path = std::env::temp_dir().join("klipp_otio_with_t_and_m.otio");
        std::fs::write(&path, fixture).unwrap();
        let result = load_otio(&path).expect("import should succeed");
        let _ = std::fs::remove_file(&path);

        // One track, two clips (transition isn't materialised as a clip).
        assert_eq!(result.sequence.tracks.len(), 1);
        let track = &result.sequence.tracks[0];
        let clip_count = track
            .clips
            .iter()
            .filter(|c| matches!(c.kind, ClipKind::Media { .. }))
            .count();
        assert_eq!(clip_count, 2, "expected 2 media clips");

        // Sequence-level marker promoted from the Stack.
        assert_eq!(result.sequence.markers.len(), 1, "one sequence marker");

        // Clip marker on the first clip.
        let first_clip_markers = track.clips[0].clip_markers.len();
        assert_eq!(first_clip_markers, 1, "one marker on first clip");

        // Warnings: transition + track marker drops.
        assert!(!result.warnings.is_empty(), "warnings expected");
        assert!(
            warnings_contain_substring(&result.warnings, "transition"),
            "warnings should mention 'transition', got: {:?}", result.warnings,
        );
        assert!(
            warnings_contain_substring(&result.warnings, "marker"),
            "warnings should mention 'marker', got: {:?}", result.warnings,
        );
    }
}
