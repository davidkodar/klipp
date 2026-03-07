use crate::format::*;
use klipp_core::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use uuid::Uuid;

pub fn load_project(dir: &Path) -> Result<(Sequence, MediaPool), String> {
    let project_path = dir.join("project.json");
    let manifest_path = dir.join("media").join("manifest.json");

    let project_str = std::fs::read_to_string(&project_path)
        .map_err(|e| format!("Failed to read project.json: {e}"))?;
    let project: ProjectJson = serde_json::from_str(&project_str)
        .map_err(|e| format!("Failed to parse project.json: {e}"))?;

    let manifest_str = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read manifest.json: {e}"))?;
    let manifest: MediaManifestJson = serde_json::from_str(&manifest_str)
        .map_err(|e| format!("Failed to parse manifest.json: {e}"))?;

    // Reconstruct media pool
    let mut media_pool = MediaPool::new();
    for item_json in &manifest.items {
        let path = PathBuf::from(&item_json.path);
        let offline = !path.exists();
        if offline {
            warn!("Media file not found, marking offline: {:?}", path);
        }
        let media_item = MediaItem {
            id: MediaId(parse_uuid(&item_json.id)?),
            path,
            filename: item_json.filename.clone(),
            duration: Timecode(item_json.duration_frames),
            fps: Fps::new(item_json.fps.num, item_json.fps.den),
            resolution: Resolution {
                width: item_json.resolution.width,
                height: item_json.resolution.height,
            },
            video_codec: item_json.video_codec.clone(),
            audio_codec: item_json.audio_codec.clone(),
            has_audio: item_json.has_audio,
            file_size_bytes: item_json.file_size_bytes,
            offline,
        };
        media_pool.add(media_item);
    }

    // Reconstruct sequence
    let seq_json = &project.sequence;
    let sequence = Sequence {
        id: SequenceId(parse_uuid(&seq_json.id)?),
        name: seq_json.name.clone(),
        fps: Fps::new(seq_json.fps.num, seq_json.fps.den),
        resolution: Resolution {
            width: seq_json.resolution.width,
            height: seq_json.resolution.height,
        },
        tracks: seq_json.tracks.iter().map(|t| {
            Track {
                id: TrackId(parse_uuid(&t.id).unwrap_or_else(|_| Uuid::new_v4())),
                name: t.name.clone(),
                kind: match t.kind.as_str() {
                    "Audio" => TrackKind::Audio,
                    _ => TrackKind::Video,
                },
                clips: t.clips.iter().map(|c| {
                    Clip {
                        id: ClipId(parse_uuid(&c.id).unwrap_or_else(|_| Uuid::new_v4())),
                        media_id: MediaId(parse_uuid(&c.media_id).unwrap_or_else(|_| Uuid::new_v4())),
                        timeline_in: Timecode(c.timeline_in),
                        timeline_out: Timecode(c.timeline_out),
                        source_in: Timecode(c.source_in),
                        source_out: Timecode(c.source_out),
                    }
                }).collect(),
            }
        }).collect(),
    };

    info!("Project loaded from {:?}", dir);
    Ok((sequence, media_pool))
}

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}
