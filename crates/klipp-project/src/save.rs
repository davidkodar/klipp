use crate::format::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Sequence, TrackKind};
use std::path::Path;
use tracing::info;

/// Global color settings to persist alongside the project.
pub struct ColorSettings {
    pub global_lut_path: Option<std::path::PathBuf>,
    pub global_lut_opacity: f32,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self { global_lut_path: None, global_lut_opacity: 1.0 }
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
    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
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
                clips: t.clips.iter().map(|c| {
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id.to_string(),
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
                        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
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
    }
}
