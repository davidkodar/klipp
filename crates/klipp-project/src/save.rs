use crate::format::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Sequence, TrackKind};
use std::path::Path;
use tracing::info;

pub fn save_project(
    dir: &Path,
    sequence: &Sequence,
    media_pool: &MediaPool,
) -> Result<(), String> {
    // Create directory structure
    std::fs::create_dir_all(dir.join("media"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;

    // Build project.json
    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
        sequence: sequence_to_json(sequence),
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

fn sequence_to_json(seq: &Sequence) -> SequenceJson {
    SequenceJson {
        id: seq.id.to_string(),
        name: seq.name.clone(),
        fps: FpsJson { num: seq.fps.num, den: seq.fps.den },
        resolution: ResolutionJson { width: seq.resolution.width, height: seq.resolution.height },
        tracks: seq.tracks.iter().map(|t| {
            TrackJson {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: match t.kind {
                    TrackKind::Video => "Video".to_string(),
                    TrackKind::Audio => "Audio".to_string(),
                },
                clips: t.clips.iter().map(|c| {
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id.to_string(),
                        timeline_in: c.timeline_in.value,
                        timeline_out: c.timeline_out.value,
                        source_in: c.source_in.value,
                        source_out: c.source_out.value,
                        rate: c.timeline_in.rate,
                    }
                }).collect(),
            }
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
    }
}
