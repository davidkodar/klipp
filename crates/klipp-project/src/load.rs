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
            duration: RationalTime { value: item_json.duration_frames, rate: item_json.duration_rate },
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
            proxy_path: item_json.proxy_path.as_ref().map(PathBuf::from),
            proxy_ready: item_json.proxy_ready,
            default_source_in: item_json.default_source_in.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
            default_source_out: item_json.default_source_out.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
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
            let kind = match t.kind.as_str() {
                "Audio" => TrackKind::Audio,
                _ => TrackKind::Video,
            };
            let mut track = Track::new(&t.name, kind);
            track.id = TrackId(parse_uuid(&t.id).unwrap_or_else(|_| Uuid::new_v4()));
            track.opacity = t.opacity;
            track.volume = t.volume;
            track.muted = t.muted;
            track.solo = t.solo;
            track.visible = t.visible;
            track.clips = t.clips.iter().map(|c| {
                Clip {
                    id: ClipId(parse_uuid(&c.id).unwrap_or_else(|_| Uuid::new_v4())),
                    media_id: MediaId(parse_uuid(&c.media_id).unwrap_or_else(|_| Uuid::new_v4())),
                    timeline_in: RationalTime { value: c.timeline_in, rate: c.rate },
                    timeline_out: RationalTime { value: c.timeline_out, rate: c.rate },
                    source_in: RationalTime { value: c.source_in, rate: c.rate },
                    source_out: RationalTime { value: c.source_out, rate: c.rate },
                }
            }).collect();
            track
        }).collect(),
        in_point: seq_json.in_point.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
        out_point: seq_json.out_point.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
    };

    info!("Project loaded from {:?}", dir);
    Ok((sequence, media_pool))
}

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}
