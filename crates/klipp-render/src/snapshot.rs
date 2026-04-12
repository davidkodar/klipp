use crate::error::RenderError;
use klipp_core::{ClipId, ExportSettings, RationalTime, TransitionId};
use klipp_effects::BuiltinEffect;
use klipp_effects::stabilization::{SmoothedTrajectory, StabilizationData};
use klipp_media::MediaPool;
use klipp_nodes::NodeGraph;
use klipp_plugin::AppliedEffect;
use klipp_timeline::{
    ClipKind, Sequence, TitleData, Track, TrackKind, Transition, TransitionKind,
};
use std::path::PathBuf;

// ═══════════════════════════════════════════════════════════════════
// Snapshot types — frozen copies of sequence data at export time
// ═══════════════════════════════════════════════════════════════════

/// Complete frozen copy of the sequence at export time.
/// The export thread reads only this — it never touches the live Sequence.
#[derive(Clone)]
pub struct SequenceSnapshot {
    pub fps: f64,
    /// Exact frame rate numerator (e.g. 24000 for 23.976).
    pub fps_num: u32,
    /// Exact frame rate denominator (e.g. 1001 for 23.976).
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub duration_frames: u64,
    /// If set, only export this frame range (start inclusive, end exclusive).
    pub export_range: Option<(u64, u64)>,
    /// Video tracks in render order (bottom to top = index 0 to N).
    pub video_tracks: Vec<VideoTrackSnapshot>,
    /// Audio tracks.
    pub audio_tracks: Vec<AudioTrackSnapshot>,
    /// Global 3D LUT path if loaded.
    pub global_lut_path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct VideoTrackSnapshot {
    pub opacity: f32,
    pub visible: bool,
    pub clips: Vec<ClipSnapshot>,
    pub transitions: Vec<TransitionSnapshot>,
}

#[derive(Clone)]
pub struct ClipSnapshot {
    pub id: ClipId,
    pub kind: ClipKindSnapshot,
    /// Start frame on the sequence timeline (inclusive).
    pub timeline_start_frame: u64,
    /// End frame on the sequence timeline (exclusive).
    pub timeline_end_frame: u64,
    /// Where to seek in the source file (seconds).
    pub source_start_secs: f64,
    /// Where the clip ends in the source file (seconds).
    pub source_end_secs: f64,
    /// Playback speed multiplier (1.0 = normal).
    pub speed: f64,
    /// Node-based color processing graph.
    pub node_graph: NodeGraph,
    /// OFX effects chain.
    pub effects: Vec<AppliedEffect>,
    /// Built-in effects chain (GPU/CPU).
    pub builtin_effects: Vec<BuiltinEffect>,
    /// Source rotation in degrees (0, 90, 180, 270) — from container metadata.
    pub source_rotation: i32,
    /// Pre-computed stabilization analysis data (frame transforms + trajectory).
    pub stabilization_data: Option<StabilizationData>,
    /// Smoothed trajectory with per-frame correction homographies.
    pub smoothed_trajectory: Option<SmoothedTrajectory>,
}

#[derive(Clone)]
pub enum ClipKindSnapshot {
    Media { path: PathBuf, has_audio: bool },
    Title(TitleData),
    Generator(klipp_core::GeneratorData),
    Gap,
}

#[derive(Clone)]
pub struct AudioTrackSnapshot {
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub clips: Vec<AudioClipSnapshot>,
}

#[derive(Clone)]
pub struct AudioClipSnapshot {
    pub timeline_start_frame: u64,
    pub timeline_end_frame: u64,
    pub source_start_secs: f64,
    pub path: PathBuf,
    /// Per-clip audio DSP effects (EQ, compressor, reverb, etc.)
    pub audio_effects: Vec<klipp_core::AudioEffectInstance>,
}

#[derive(Clone)]
pub struct TransitionSnapshot {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub duration_frames: u64,
    pub clip_a_id: ClipId,
    pub clip_b_id: ClipId,
    pub user_params: klipp_core::TransitionUserParams,
    pub alignment: klipp_core::TransitionAlignment,
}

// ═══════════════════════════════════════════════════════════════════
// Construction
// ═══════════════════════════════════════════════════════════════════

impl SequenceSnapshot {
    /// Freeze the entire sequence at this moment. All MediaIds are resolved
    /// to file paths via `media_pool`. RationalTime values are converted to
    /// frame numbers and f64 seconds. The `global_lut_path` is passed from
    /// the engine's ColorPipeline.
    /// `use_marked_range`: if true AND the sequence has in/out points set,
    /// only export that range. If false, export the entire sequence.
    pub fn from_sequence(
        seq: &Sequence,
        media_pool: &MediaPool,
        _settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
    ) -> Result<Self, RenderError> {
        Self::from_sequence_with_all(seq, &[], media_pool, _settings, global_lut_path, use_marked_range)
    }

    /// Freeze the sequence, resolving compound clips (nested sequences) by
    /// flattening their inner clips into the parent timeline. `all_sequences`
    /// must contain every sequence in the project so that
    /// `ClipKind::Sequence { sequence_id }` can be resolved.
    pub fn from_sequence_with_all(
        seq: &Sequence,
        all_sequences: &[Sequence],
        media_pool: &MediaPool,
        _settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
    ) -> Result<Self, RenderError> {
        let fps = seq.fps.as_f64();

        let export_range = if use_marked_range {
            match (&seq.in_point, &seq.out_point) {
                (Some(in_pt), Some(out_pt)) => {
                    let start = time_to_frame(in_pt, fps);
                    let end = time_to_frame(out_pt, fps);
                    Some((start, end))
                }
                _ => None,
            }
        } else {
            None
        };

        let total_duration = seq.total_duration();
        let duration_frames = time_to_frame(&total_duration, fps);

        let mut video_tracks = Vec::new();
        let mut audio_tracks = Vec::new();

        // Process tracks in order. Video tracks are stored bottom-to-top
        // (V1 = index 0, composited first).
        for track in seq.tracks.iter().rev() {
            match track.kind {
                TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => {
                    video_tracks.push(snapshot_video_track(track, all_sequences, media_pool, fps, 0)?);
                }
                TrackKind::Audio => {
                    audio_tracks.push(snapshot_audio_track(track, all_sequences, media_pool, fps, 0)?);
                }
            }
        }

        Ok(Self {
            fps,
            fps_num: seq.fps.num,
            fps_den: seq.fps.den,
            width: seq.resolution.width,
            height: seq.resolution.height,
            sample_rate: 48000, // Standard NLE audio rate
            duration_frames,
            export_range,
            video_tracks,
            audio_tracks,
            global_lut_path,
        })
    }

    /// Total frames to export, respecting export_range.
    pub fn frames_to_export(&self) -> u64 {
        match self.export_range {
            Some((start, end)) => end.saturating_sub(start),
            None => self.duration_frames,
        }
    }

    /// Start frame of the export range, or 0.
    pub fn start_frame(&self) -> u64 {
        self.export_range.map(|(s, _)| s).unwrap_or(0)
    }

    /// End frame of the export range (exclusive), or duration_frames.
    pub fn end_frame(&self) -> u64 {
        self.export_range.map(|(_, e)| e).unwrap_or(self.duration_frames)
    }
}

/// Maximum nesting depth for compound clips to prevent infinite recursion.
const MAX_COMPOUND_DEPTH: u8 = 8;

fn snapshot_video_track(
    track: &Track,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
) -> Result<VideoTrackSnapshot, RenderError> {
    let mut clips = Vec::with_capacity(track.clips.len());
    for clip in &track.clips {
        if !clip.enabled { continue; }
        if let ClipKind::Sequence { sequence_id } = &clip.kind {
            if depth < MAX_COMPOUND_DEPTH {
                flatten_compound_video_clips(clip, *sequence_id, all_sequences, media_pool, fps, depth, &mut clips)?;
                continue;
            }
            // Exceeded depth limit — skip as gap
        }

        // Multicam flattening: if this is the primary clip (angle 0) with
        // a cut list, expand each cut segment into a snapshot sourcing from
        // the appropriate angle clip.
        if clip.multicam_group.is_some()
            && clip.multicam_angle == 0
            && !clip.multicam_cuts.is_empty()
        {
            flatten_multicam_video_clips(clip, track, media_pool, fps, &mut clips)?;
            continue;
        }

        // Skip non-primary multicam angles — they are referenced via the
        // primary clip's cut list and rendered through flattening above.
        if clip.multicam_group.is_some() && clip.multicam_angle != 0 {
            continue;
        }

        clips.push(snapshot_clip(clip, media_pool, fps)?);
    }

    let transitions = track
        .transitions
        .iter()
        .map(|t| snapshot_transition(t, fps))
        .collect();

    Ok(VideoTrackSnapshot {
        opacity: track.opacity,
        visible: track.visible,
        clips,
        transitions,
    })
}

/// Flatten a primary multicam clip's cut list into individual clip snapshots.
/// Each cut segment (from time A to time B, showing angle N) is resolved to the
/// corresponding angle clip in the same track, producing a snapshot that sources
/// from that angle's media for the relevant time range.
fn flatten_multicam_video_clips(
    primary: &klipp_timeline::Clip,
    track: &Track,
    media_pool: &MediaPool,
    fps: f64,
    out: &mut Vec<ClipSnapshot>,
) -> Result<(), RenderError> {
    let group_id = primary.multicam_group.expect("caller checked");
    let clip_in_secs = primary.timeline_in.to_secs_f64();
    let clip_out_secs = primary.timeline_out.to_secs_f64();
    let clip_duration_secs = clip_out_secs - clip_in_secs;

    // Build cut segments: (start_secs, end_secs, angle_index)
    // Each entry in multicam_cuts is (time_from_clip_start, angle_index).
    // We fill in the gaps: from the start of the clip to the first cut, between
    // cuts, and from the last cut to the end.
    let mut segments: Vec<(f64, f64, u8)> = Vec::new();
    let cuts = &primary.multicam_cuts;
    for (i, &(cut_time, angle)) in cuts.iter().enumerate() {
        let seg_start = cut_time;
        let seg_end = if i + 1 < cuts.len() {
            cuts[i + 1].0
        } else {
            clip_duration_secs
        };
        if seg_end > seg_start {
            segments.push((seg_start, seg_end, angle));
        }
    }
    // If the first cut doesn't start at 0, insert a segment for angle 0
    // from clip start to the first cut.
    if segments.is_empty() || segments[0].0 > 0.001 {
        let first_end = if segments.is_empty() { clip_duration_secs } else { segments[0].0 };
        if first_end > 0.001 {
            segments.insert(0, (0.0, first_end, 0));
        }
    }

    // Collect angle clips in this group from the same track
    let angle_clips: Vec<&klipp_timeline::Clip> = track.clips.iter()
        .filter(|c| c.multicam_group == Some(group_id) && c.enabled)
        .collect();

    for (seg_start, seg_end, angle) in &segments {
        // Find the clip for this angle. If not found, fall back to the primary (angle 0).
        let angle_clip = angle_clips.iter()
            .find(|c| c.multicam_angle == *angle)
            .or_else(|| angle_clips.iter().find(|c| c.multicam_angle == 0));

        let Some(angle_clip) = angle_clip else { continue };

        let kind = match &angle_clip.kind {
            ClipKind::Media { media_id } => {
                let media = media_pool
                    .get(media_id)
                    .ok_or_else(|| RenderError::Snapshot(format!("multicam media {media_id:?} not in pool")))?;
                ClipKindSnapshot::Media {
                    path: media.path.clone(),
                    has_audio: media.has_audio,
                }
            }
            ClipKind::Title(data) => ClipKindSnapshot::Title(data.clone()),
            _ => continue,
        };

        // Map segment times to the parent timeline
        let parent_in_secs = clip_in_secs + seg_start;
        let parent_out_secs = clip_in_secs + seg_end;

        // Map to source media time: offset into the angle clip's source
        let source_start_secs = angle_clip.source_in.to_secs_f64() + seg_start;
        let source_end_secs = angle_clip.source_in.to_secs_f64() + seg_end;

        out.push(ClipSnapshot {
            id: angle_clip.id,
            kind,
            timeline_start_frame: (parent_in_secs * fps).round() as u64,
            timeline_end_frame: (parent_out_secs * fps).round() as u64,
            source_start_secs,
            source_end_secs,
            speed: angle_clip.speed,
            node_graph: angle_clip.node_graph.clone(),
            effects: angle_clip.effects.clone(),
            builtin_effects: angle_clip.builtin_effects.clone(),
            source_rotation: angle_clip.source_rotation,
            stabilization_data: angle_clip.stabilization_data.clone(),
            smoothed_trajectory: angle_clip.smoothed_trajectory.clone(),
        });
    }

    Ok(())
}

/// Flatten a compound clip's inner sequence video tracks into the parent clip list.
/// The inner clips are time-shifted so they appear at the compound clip's position
/// on the parent timeline, and clipped to the compound clip's visible range.
fn flatten_compound_video_clips(
    compound: &klipp_timeline::Clip,
    sequence_id: klipp_core::SequenceId,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
    out: &mut Vec<ClipSnapshot>,
) -> Result<(), RenderError> {
    let inner_seq = all_sequences
        .iter()
        .find(|s| s.id == sequence_id)
        .ok_or_else(|| RenderError::Snapshot(format!("compound clip references unknown sequence {sequence_id:?}")))?;

    // Compound clip's window into the inner sequence (in seconds)
    let compound_timeline_in_secs = compound.timeline_in.to_secs_f64();
    let compound_source_in_secs = compound.source_in.to_secs_f64();
    let compound_source_out_secs = compound.source_out.to_secs_f64();

    for inner_track in &inner_seq.tracks {
        match inner_track.kind {
            TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => {}
            _ => continue,
        }
        for inner_clip in &inner_track.clips {
            if !inner_clip.enabled { continue; }

            let ic_in_secs = inner_clip.timeline_in.to_secs_f64();
            let ic_out_secs = inner_clip.timeline_out.to_secs_f64();

            // Inner clip must overlap the compound clip's source window
            if ic_out_secs <= compound_source_in_secs || ic_in_secs >= compound_source_out_secs {
                continue;
            }

            // Clamp the inner clip to the compound's source window
            let visible_in = ic_in_secs.max(compound_source_in_secs);
            let visible_out = ic_out_secs.min(compound_source_out_secs);

            // Map to the parent timeline
            let parent_in_secs = compound_timeline_in_secs + (visible_in - compound_source_in_secs);
            let parent_out_secs = compound_timeline_in_secs + (visible_out - compound_source_in_secs);

            // Adjust source_in if the inner clip was clipped at the start
            let source_in_offset = visible_in - ic_in_secs;
            let inner_source_in_secs = inner_clip.source_in.to_secs_f64() + source_in_offset;
            let inner_source_out_secs = inner_clip.source_out.to_secs_f64() - (ic_out_secs - visible_out);

            // Recursively handle nested compound clips
            if let ClipKind::Sequence { sequence_id: nested_id } = &inner_clip.kind {
                if depth + 1 < MAX_COMPOUND_DEPTH {
                    // Build a synthetic clip that represents the visible portion
                    let rate = (fps.ceil() as u32).max(1);
                    let mut synthetic = inner_clip.clone();
                    synthetic.timeline_in = RationalTime::from_secs_f64(parent_in_secs, rate);
                    synthetic.timeline_out = RationalTime::from_secs_f64(parent_out_secs, rate);
                    synthetic.source_in = RationalTime::from_secs_f64(inner_source_in_secs, rate);
                    synthetic.source_out = RationalTime::from_secs_f64(inner_source_out_secs, rate);
                    flatten_compound_video_clips(&synthetic, *nested_id, all_sequences, media_pool, fps, depth + 1, out)?;
                    continue;
                }
                // Depth exceeded — skip
                continue;
            }

            let kind = match &inner_clip.kind {
                ClipKind::Media { media_id } => {
                    let media = media_pool
                        .get(media_id)
                        .ok_or_else(|| RenderError::Snapshot(format!("media {media_id:?} not in pool")))?;
                    ClipKindSnapshot::Media {
                        path: media.path.clone(),
                        has_audio: media.has_audio,
                    }
                }
                ClipKind::Title(data) => ClipKindSnapshot::Title(data.clone()),
                ClipKind::Generator(data) => ClipKindSnapshot::Generator(data.clone()),
                // TODO: implement adjustment layer compositing — apply effects to all clips below
                ClipKind::Gap | ClipKind::AdjustmentLayer => continue,
                ClipKind::Sequence { .. } => unreachable!(), // handled above
            };

            out.push(ClipSnapshot {
                id: inner_clip.id,
                kind,
                timeline_start_frame: (parent_in_secs * fps).round() as u64,
                timeline_end_frame: (parent_out_secs * fps).round() as u64,
                source_start_secs: inner_source_in_secs,
                source_end_secs: inner_source_out_secs,
                speed: inner_clip.speed * compound.speed,
                node_graph: inner_clip.node_graph.clone(),
                effects: inner_clip.effects.clone(),
                builtin_effects: inner_clip.builtin_effects.clone(),
                source_rotation: inner_clip.source_rotation,
                stabilization_data: inner_clip.stabilization_data.clone(),
                smoothed_trajectory: inner_clip.smoothed_trajectory.clone(),
            });
        }
    }
    Ok(())
}

fn snapshot_audio_track(
    track: &Track,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
) -> Result<AudioTrackSnapshot, RenderError> {
    let mut clips = Vec::new();
    for clip in &track.clips {
        if !clip.enabled { continue; }
        if let ClipKind::Sequence { sequence_id } = &clip.kind {
            if depth < MAX_COMPOUND_DEPTH {
                flatten_compound_audio_clips(clip, *sequence_id, all_sequences, media_pool, fps, depth, &mut clips)?;
                continue;
            }
        }

        // Multicam audio flattening: primary clip expands cut list into
        // audio segments sourcing from the active angle's media.
        if clip.multicam_group.is_some()
            && clip.multicam_angle == 0
            && !clip.multicam_cuts.is_empty()
        {
            flatten_multicam_audio_clips(clip, track, media_pool, fps, &mut clips)?;
            continue;
        }
        // Skip non-primary multicam angles (handled via primary's cut list).
        if clip.multicam_group.is_some() && clip.multicam_angle != 0 {
            continue;
        }

        if let Some(media_id) = clip.media_id() {
            if let Some(media) = media_pool.get(&media_id) {
                if media.has_audio {
                    clips.push(AudioClipSnapshot {
                        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
                        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
                        source_start_secs: clip.source_in.to_secs_f64(),
                        path: media.path.clone(),
                        audio_effects: clip.audio_effects.clone(),
                    });
                }
            }
        }
    }

    Ok(AudioTrackSnapshot {
        volume: track.volume,
        pan: track.pan,
        muted: track.muted,
        solo: track.solo,
        clips,
    })
}

/// Flatten a compound clip's inner sequence audio tracks into the parent audio clip list.
fn flatten_compound_audio_clips(
    compound: &klipp_timeline::Clip,
    sequence_id: klipp_core::SequenceId,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
    out: &mut Vec<AudioClipSnapshot>,
) -> Result<(), RenderError> {
    let inner_seq = all_sequences
        .iter()
        .find(|s| s.id == sequence_id)
        .ok_or_else(|| RenderError::Snapshot(format!("compound clip references unknown sequence {sequence_id:?}")))?;

    let compound_timeline_in_secs = compound.timeline_in.to_secs_f64();
    let compound_source_in_secs = compound.source_in.to_secs_f64();
    let compound_source_out_secs = compound.source_out.to_secs_f64();

    for inner_track in &inner_seq.tracks {
        if inner_track.kind != TrackKind::Audio { continue; }
        for inner_clip in &inner_track.clips {
            if !inner_clip.enabled { continue; }

            let ic_in_secs = inner_clip.timeline_in.to_secs_f64();
            let ic_out_secs = inner_clip.timeline_out.to_secs_f64();

            if ic_out_secs <= compound_source_in_secs || ic_in_secs >= compound_source_out_secs {
                continue;
            }

            let visible_in = ic_in_secs.max(compound_source_in_secs);
            let visible_out = ic_out_secs.min(compound_source_out_secs);
            let parent_in_secs = compound_timeline_in_secs + (visible_in - compound_source_in_secs);
            let parent_out_secs = compound_timeline_in_secs + (visible_out - compound_source_in_secs);
            let source_in_offset = visible_in - ic_in_secs;

            // Recursively flatten nested compound clips
            if let ClipKind::Sequence { sequence_id: nested_id } = &inner_clip.kind {
                if depth + 1 < MAX_COMPOUND_DEPTH {
                    let rate = (fps.ceil() as u32).max(1);
                    let mut synthetic = inner_clip.clone();
                    synthetic.timeline_in = RationalTime::from_secs_f64(parent_in_secs, rate);
                    synthetic.timeline_out = RationalTime::from_secs_f64(parent_out_secs, rate);
                    let inner_source_in_secs = inner_clip.source_in.to_secs_f64() + source_in_offset;
                    let inner_source_out_secs = inner_clip.source_out.to_secs_f64() - (ic_out_secs - visible_out);
                    synthetic.source_in = RationalTime::from_secs_f64(inner_source_in_secs, rate);
                    synthetic.source_out = RationalTime::from_secs_f64(inner_source_out_secs, rate);
                    flatten_compound_audio_clips(&synthetic, *nested_id, all_sequences, media_pool, fps, depth + 1, out)?;
                }
                continue;
            }

            if let Some(media_id) = inner_clip.media_id() {
                if let Some(media) = media_pool.get(&media_id) {
                    if media.has_audio {
                        out.push(AudioClipSnapshot {
                            timeline_start_frame: (parent_in_secs * fps).round() as u64,
                            timeline_end_frame: (parent_out_secs * fps).round() as u64,
                            source_start_secs: inner_clip.source_in.to_secs_f64() + source_in_offset,
                            path: media.path.clone(),
                            audio_effects: inner_clip.audio_effects.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Flatten a primary multicam clip's cut list into audio clip snapshots.
/// Mirrors `flatten_multicam_video_clips` but produces `AudioClipSnapshot`s.
fn flatten_multicam_audio_clips(
    primary: &klipp_timeline::Clip,
    track: &Track,
    media_pool: &MediaPool,
    fps: f64,
    out: &mut Vec<AudioClipSnapshot>,
) -> Result<(), RenderError> {
    let group_id = primary.multicam_group.expect("caller checked");
    let clip_in_secs = primary.timeline_in.to_secs_f64();
    let clip_out_secs = primary.timeline_out.to_secs_f64();
    let clip_duration_secs = clip_out_secs - clip_in_secs;

    // Build cut segments: (start_secs, end_secs, angle_index)
    let mut segments: Vec<(f64, f64, u8)> = Vec::new();
    let cuts = &primary.multicam_cuts;
    for (i, &(cut_time, angle)) in cuts.iter().enumerate() {
        let seg_start = cut_time;
        let seg_end = if i + 1 < cuts.len() {
            cuts[i + 1].0
        } else {
            clip_duration_secs
        };
        if seg_end > seg_start {
            segments.push((seg_start, seg_end, angle));
        }
    }
    // Fill in segment from 0 to first cut if needed
    if segments.is_empty() || segments[0].0 > 0.001 {
        let first_end = if segments.is_empty() { clip_duration_secs } else { segments[0].0 };
        if first_end > 0.001 {
            segments.insert(0, (0.0, first_end, 0));
        }
    }

    let angle_clips: Vec<&klipp_timeline::Clip> = track.clips.iter()
        .filter(|c| c.multicam_group == Some(group_id) && c.enabled)
        .collect();

    for (seg_start, seg_end, angle) in &segments {
        let angle_clip = angle_clips.iter()
            .find(|c| c.multicam_angle == *angle)
            .or_else(|| angle_clips.iter().find(|c| c.multicam_angle == 0));
        let Some(angle_clip) = angle_clip else { continue };

        let media_id = match angle_clip.media_id() {
            Some(mid) => mid,
            None => continue,
        };
        let media = match media_pool.get(&media_id) {
            Some(m) if m.has_audio => m,
            _ => continue,
        };

        let parent_in_secs = clip_in_secs + seg_start;
        let parent_out_secs = clip_in_secs + seg_end;
        let source_start_secs = angle_clip.source_in.to_secs_f64() + seg_start;

        out.push(AudioClipSnapshot {
            timeline_start_frame: (parent_in_secs * fps).round() as u64,
            timeline_end_frame: (parent_out_secs * fps).round() as u64,
            source_start_secs,
            path: media.path.clone(),
            audio_effects: angle_clip.audio_effects.clone(),
        });
    }

    Ok(())
}

fn snapshot_clip(
    clip: &klipp_timeline::Clip,
    media_pool: &MediaPool,
    fps: f64,
) -> Result<ClipSnapshot, RenderError> {
    let kind = match &clip.kind {
        ClipKind::Media { media_id } => {
            let media = media_pool
                .get(media_id)
                .ok_or_else(|| RenderError::Snapshot(format!("media {media_id:?} not in pool")))?;
            ClipKindSnapshot::Media {
                path: media.path.clone(),
                has_audio: media.has_audio,
            }
        }
        ClipKind::Title(data) => ClipKindSnapshot::Title(data.clone()),
        ClipKind::Gap => ClipKindSnapshot::Gap,
        // Compound clips are flattened at the track level. This fallback
        // is only reached if nesting depth exceeds MAX_COMPOUND_DEPTH.
        ClipKind::Sequence { .. } => ClipKindSnapshot::Gap,
        ClipKind::Generator(data) => ClipKindSnapshot::Generator(data.clone()),
        ClipKind::AdjustmentLayer => ClipKindSnapshot::Gap,
    };

    Ok(ClipSnapshot {
        id: clip.id,
        kind,
        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
        source_start_secs: clip.source_in.to_secs_f64(),
        source_end_secs: clip.source_out.to_secs_f64(),
        speed: clip.speed,
        node_graph: clip.node_graph.clone(),
        effects: clip.effects.clone(),
        builtin_effects: clip.builtin_effects.clone(),
        source_rotation: clip.source_rotation,
        stabilization_data: clip.stabilization_data.clone(),
        smoothed_trajectory: clip.smoothed_trajectory.clone(),
    })
}

fn snapshot_transition(t: &Transition, fps: f64) -> TransitionSnapshot {
    TransitionSnapshot {
        id: t.id,
        kind: t.kind,
        duration_frames: time_to_frame(&t.duration, fps).max(1),
        clip_a_id: t.clip_a_id,
        clip_b_id: t.clip_b_id,
        user_params: t.user_params,
        alignment: t.alignment,
    }
}

/// Test-only access to `snapshot_transition`.
#[cfg(test)]
pub fn snapshot_transition_for_test(
    t: &klipp_timeline::Transition,
    fps: f64,
) -> TransitionSnapshot {
    snapshot_transition(t, fps)
}

/// Convert a RationalTime to a frame number at the given fps.
fn time_to_frame(rt: &RationalTime, fps: f64) -> u64 {
    let secs = rt.to_secs_f64();
    (secs * fps).round() as u64
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, ExportAudioCodec, ExportFormat, ExportVideoCodec, Fps, MediaId, Resolution, EncoderPreset};
    use klipp_media::{MediaItem, MediaPool};
    use klipp_nodes::NodeGraph;
    use klipp_timeline::{Clip, ClipKind, Sequence};

    fn make_fps24() -> Fps {
        Fps::new(24, 1)
    }

    fn make_sequence() -> Sequence {
        Sequence::new_default()
    }

    fn make_rational(frames: i64, rate: u32) -> RationalTime {
        RationalTime::from_frames(frames, rate)
    }

    fn make_media_item(id: MediaId, path: &str) -> MediaItem {
        MediaItem {
            id,
            path: PathBuf::from(path),
            filename: path.into(),
            duration: make_rational(240, 24), // 10 seconds
            fps: make_fps24(),
            resolution: Resolution {
                width: 1920,
                height: 1080,
            },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1_000_000,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: vec![],
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: klipp_media::ColorLabel::default(),
            colour_space: klipp_media::ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
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
        }
    }

    fn make_settings() -> ExportSettings {
        ExportSettings {
            output_path: PathBuf::from("/tmp/test.mp4"),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 18,
            preset: EncoderPreset::Medium,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf }
    }

    #[test]
    fn snapshot_captures_single_clip() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(24, 24),
            source_out: make_rational(72, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip.clone()); // Video 1 is tracks[0] (V1, A1)

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(snap.fps, 24.0);
        assert_eq!(snap.width, 1920);
        assert_eq!(snap.height, 1080);
        assert_eq!(snap.duration_frames, 48); // 2 seconds at 24fps
        assert!(snap.export_range.is_none());

        // Should have video tracks (reversed: V1 first)
        assert!(!snap.video_tracks.is_empty());
        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        assert_eq!(v1.clips.len(), 1);

        let sc = &v1.clips[0];
        assert_eq!(sc.timeline_start_frame, 0);
        assert_eq!(sc.timeline_end_frame, 48);
        assert_eq!(sc.source_start_secs, 1.0); // 24 frames at 24fps = 1s
        assert_eq!(sc.source_end_secs, 3.0); // 72 frames at 24fps = 3s

        match &sc.kind {
            ClipKindSnapshot::Media { path, has_audio } => {
                assert_eq!(path, &PathBuf::from("/media/clip1.mp4"));
                assert!(*has_audio);
            }
            _ => panic!("expected Media clip"),
        }
    }

    #[test]
    fn snapshot_captures_export_range() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(240, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(240, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(240, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);
        seq.in_point = Some(make_rational(24, 24));  // 1 second in
        seq.out_point = Some(make_rational(120, 24)); // 5 seconds in

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, true).unwrap();

        assert_eq!(snap.export_range, Some((24, 120)));
        assert_eq!(snap.frames_to_export(), 96);
        assert_eq!(snap.start_frame(), 24);
        assert_eq!(snap.end_frame(), 120);
    }

    #[test]
    fn snapshot_captures_node_graph() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let graph = NodeGraph::default();

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            node_graph: graph.clone(),
            ..Clip::gap(make_rational(0, 24), make_rational(24, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let sc = &v1.clips[0];
        // Verify node graph was cloned into snapshot
        assert_eq!(sc.node_graph.nodes.len(), graph.nodes.len());
    }

    #[test]
    fn snapshot_captures_title_clip() {
        let title = TitleData::default();
        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Title(title.clone()),
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(72, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(72, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(72, 24))
        };

        let pool = MediaPool::new();
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        match &v1.clips[0].kind {
            ClipKindSnapshot::Title(data) => {
                assert_eq!(data.text, title.text);
            }
            _ => panic!("expected Title clip"),
        }
    }

    #[test]
    fn snapshot_captures_gap_clip() {
        let clip = Clip::gap(make_rational(0, 24), make_rational(24, 24));

        let pool = MediaPool::new();
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        assert!(matches!(v1.clips[0].kind, ClipKindSnapshot::Gap));
    }

    #[test]
    fn snapshot_captures_multi_track() {
        let media_id_a = MediaId::new();
        let media_id_b = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id_a, "/media/a.mp4"));
        pool.add(make_media_item(media_id_b, "/media/b.mp4"));

        let clip_a = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id: media_id_a },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };

        let clip_b = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id: media_id_b },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(24, 24))
        };

        let mut seq = make_sequence();
        // Default: V1(0), A1(1). Add a second video track for multi-track test.
        seq.add_video_track("Video 2".into());
        // After add: V2(0), V1(1), A1(2)
        seq.tracks[0].clips.push(clip_b); // V2 (overlay)
        seq.tracks[1].clips.push(clip_a); // V1 (base)

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let tracks_with_clips: Vec<_> = snap.video_tracks.iter()
            .filter(|t| !t.clips.is_empty())
            .collect();
        assert_eq!(tracks_with_clips.len(), 2);
    }

    #[test]
    fn snapshot_captures_transitions() {
        let clip_a_id = ClipId::new();
        let clip_b_id = ClipId::new();
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip.mp4"));

        let clip_a = Clip {
            id: clip_a_id,
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };
        let clip_b = Clip {
            id: clip_b_id,
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(36, 24),
            timeline_out: make_rational(96, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(60, 24),
            ..Clip::gap(make_rational(36, 24), make_rational(96, 24))
        };

        let transition = Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: make_rational(12, 24), // 0.5 sec
            clip_a_id,
            clip_b_id,
            user_params: Default::default(),
            alignment: Default::default(),
        };

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip_a);
        seq.tracks[0].clips.push(clip_b);
        seq.tracks[0].transitions.push(transition);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        assert_eq!(v1.transitions.len(), 1);
        assert_eq!(v1.transitions[0].kind, TransitionKind::CrossDissolve);
        assert_eq!(v1.transitions[0].duration_frames, 12);
        assert_eq!(v1.transitions[0].clip_a_id, clip_a_id);
        assert_eq!(v1.transitions[0].clip_b_id, clip_b_id);
    }

    #[test]
    fn snapshot_audio_tracks() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip.mp4"));

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(24, 24),
            source_out: make_rational(72, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip); // A1
        seq.tracks[1].volume = 0.8;
        seq.tracks[1].pan = -0.5;

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(snap.audio_tracks.len(), 1);
        let at = &snap.audio_tracks[0];
        assert_eq!(at.volume, 0.8);
        assert_eq!(at.pan, -0.5);
        assert_eq!(at.clips.len(), 1);
        assert_eq!(at.clips[0].source_start_secs, 1.0);
    }
}
