use crate::format::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{ClipKind, Sequence, TrackKind};
use std::path::Path;
use tracing::info;

/// Global color settings to persist alongside the project.
pub struct ColorSettings {
    pub global_lut_path: Option<std::path::PathBuf>,
    pub global_lut_opacity: f32,
    pub track_header_width: f32,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self { global_lut_path: None, global_lut_opacity: 1.0, track_header_width: 160.0 }
    }
}

pub fn save_project(
    dir: &Path,
    sequence: &Sequence,
    media_pool: &MediaPool,
) -> Result<(), String> {
    save_project_with_color(dir, sequence, media_pool, &ColorSettings::default())
}

pub fn save_project_with_color(
    dir: &Path,
    sequence: &Sequence,
    media_pool: &MediaPool,
    color: &ColorSettings,
) -> Result<(), String> {
    // Create directory structure
    std::fs::create_dir_all(dir.join("media"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;

    // Build project.json
    let now = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Manual ISO 8601 without pulling in chrono
        let (days, rem) = (secs / 86400, secs % 86400);
        let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // Days since 1970-01-01 → year/month/day (simplified)
        let mut y = 1970i64;
        let mut d = days as i64;
        loop {
            let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
            if d < dy { break; }
            d -= dy;
            y += 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let mut mo = 0usize;
        for md in &mdays {
            if d < *md { break; }
            d -= md;
            mo += 1;
        }
        format!("{y:04}-{:02}-{:02}T{h:02}:{m:02}:{s:02}Z", mo + 1, d + 1)
    };

    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
        klipp_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        saved_at: Some(now),
        platform: Some(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
        sequence: sequence_to_json(sequence, color),
    };

    let project_json = serde_json::to_string_pretty(&project)
        .map_err(|e| format!("Failed to serialize project: {e}"))?;
    std::fs::write(dir.join("project.json"), project_json)
        .map_err(|e| format!("Failed to write project.json: {e}"))?;

    // Build media/manifest.json
    let manifest = MediaManifestJson {
        version: "1.0.0".to_string(),
        items: media_pool.all().map(media_item_to_json).collect(),
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize media manifest: {e}"))?;
    std::fs::write(dir.join("media").join("manifest.json"), manifest_json)
        .map_err(|e| format!("Failed to write manifest.json: {e}"))?;

    info!("Project saved to {:?}", dir);
    Ok(())
}

fn timecode_to_json(tc: &klipp_core::Timecode) -> TimecodeJson {
    TimecodeJson { value: tc.value, rate: tc.rate }
}

fn sequence_to_json(seq: &Sequence, color: &ColorSettings) -> SequenceJson {
    SequenceJson {
        id: seq.id.to_string(),
        name: seq.name.clone(),
        fps: FpsJson { num: seq.fps.num, den: seq.fps.den },
        resolution: ResolutionJson { width: seq.resolution.width, height: seq.resolution.height },
        in_point: seq.in_point.as_ref().map(timecode_to_json),
        out_point: seq.out_point.as_ref().map(timecode_to_json),
        global_lut_path: color.global_lut_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        global_lut_opacity: color.global_lut_opacity,
        track_header_width: color.track_header_width,
        markers: seq.markers.iter().map(|m| {
            MarkerJson {
                id: m.id.to_string(),
                position: timecode_to_json(&m.position),
                label: m.label.clone(),
                color: format!("{:?}", m.color),
            }
        }).collect(),
        tracks: seq.tracks.iter().map(|t| {
            TrackJson {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: match t.kind {
                    TrackKind::Video => "Video".to_string(),
                    TrackKind::Audio => "Audio".to_string(),
                },
                opacity: t.opacity,
                volume: t.volume,
                muted: t.muted,
                solo: t.solo,
                visible: t.visible,
                pan: t.pan,
                locked: t.locked,
                transitions: t.transitions.iter().map(|tr| {
                    TransitionJson {
                        id: tr.id.to_string(),
                        kind: format!("{:?}", tr.kind),
                        duration: timecode_to_json(&tr.duration),
                        clip_a_id: tr.clip_a_id.to_string(),
                        clip_b_id: tr.clip_b_id.to_string(),
                    }
                }).collect(),
                clips: t.clips.iter().map(|c| {
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id().map(|mid| mid.to_string()),
                        kind: Some(clip_kind_to_json(&c.kind)),
                        timeline_in: c.timeline_in.value,
                        timeline_out: c.timeline_out.value,
                        source_in: c.source_in.value,
                        source_out: c.source_out.value,
                        rate: c.timeline_in.rate,
                        color_grade: if c.color_grade.enabled
                            || c.color_grade.exposure != 0.0
                            || c.color_grade.contrast != 0.0
                            || c.color_grade.saturation != 1.0
                            || c.color_grade.temperature != 0.0
                            || c.color_grade.tint != 0.0
                        {
                            Some(color_grade_to_json(&c.color_grade))
                        } else {
                            None
                        },
                        effects: c.effects.iter().map(effect_to_json).collect(),
                        builtin_effects: if c.builtin_effects.is_empty() {
                            None
                        } else {
                            serde_json::to_value(&c.builtin_effects).ok()
                        },
                        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
                        group_id: c.group_id.map(|id| id.to_string()),
                    }
                }).collect(),
            }
        }).collect(),
    }
}

fn color_grade_to_json(cg: &klipp_timeline::ClipColorGrade) -> ColorGradeJson {
    ColorGradeJson {
        enabled: cg.enabled,
        exposure: cg.exposure,
        contrast: cg.contrast,
        saturation: cg.saturation,
        temperature: cg.temperature,
        tint: cg.tint,
    }
}

fn effect_to_json(effect: &klipp_plugin::AppliedEffect) -> EffectJson {
    EffectJson {
        plugin_name: effect.plugin_name.clone(),
        enabled: effect.enabled,
        param_values: effect.param_values.iter().map(|(k, v): (&String, &klipp_plugin::OfxValue)| {
            let json_val = match v {
                klipp_plugin::OfxValue::Double(d) => OfxValueJson::Double(*d),
                klipp_plugin::OfxValue::Int(i) => OfxValueJson::Double(*i as f64),
                klipp_plugin::OfxValue::Bool(b) => OfxValueJson::Bool(*b),
                klipp_plugin::OfxValue::String(s) => OfxValueJson::String(s.clone()),
            };
            (k.clone(), json_val)
        }).collect(),
    }
}

#[allow(clippy::needless_borrows_for_generic_args)]
fn clip_kind_to_json(kind: &ClipKind) -> ClipKindJson {
    match kind {
        ClipKind::Media { media_id } => ClipKindJson::Media {
            media_id: media_id.to_string(),
        },
        ClipKind::Title(data) => ClipKindJson::Title {
            text: data.text.clone(),
            font_size: data.font_size,
            font_name: data.font_name.clone(),
            color: data.color,
            alignment: format!("{:?}", data.alignment),
            position_x: data.position.x,
            position_y: data.position.y,
            background: serde_json::to_value(&data.background).unwrap_or_default(),
            shadow: serde_json::to_value(&data.shadow).unwrap_or_default(),
        },
        ClipKind::Gap => ClipKindJson::Gap,
    }
}

fn media_item_to_json(item: &MediaItem) -> MediaItemJson {
    MediaItemJson {
        id: item.id.to_string(),
        path: item.path.to_string_lossy().to_string(),
        filename: item.filename.clone(),
        duration_frames: item.duration.value,
        duration_rate: item.duration.rate,
        fps: FpsJson { num: item.fps.num, den: item.fps.den },
        resolution: ResolutionJson { width: item.resolution.width, height: item.resolution.height },
        video_codec: item.video_codec.clone(),
        audio_codec: item.audio_codec.clone(),
        has_audio: item.has_audio,
        file_size_bytes: item.file_size_bytes,
        proxy_path: item.proxy_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        proxy_ready: item.proxy_ready,
        default_source_in: item.default_source_in.as_ref().map(timecode_to_json),
        default_source_out: item.default_source_out.as_ref().map(timecode_to_json),
        rating: item.rating,
        tags: item.tags.clone(),
        user_notes: item.user_notes.clone(),
        bin: item.bin.clone(),
    }
}
