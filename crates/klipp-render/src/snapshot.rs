use crate::error::RenderError;
use klipp_core::{ClipId, ExportSettings, Keyframe, RationalTime, TransitionId};
use klipp_effects::BuiltinEffect;
use klipp_effects::stabilization::{SmoothedTrajectory, StabilizationData};
use klipp_media::MediaPool;
use klipp_nodes::NodeGraph;
use klipp_plugin::AppliedEffect;
use klipp_timeline::{
    ClipKind, Sequence, TitleData, Track, TrackKind, Transition, TransitionKind,
};
use std::collections::HashMap;
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
    /// Subtitle tracks (text content + timing, independent of burn-in).
    pub subtitle_tracks: Vec<SubtitleTrackSnapshot>,
    /// Frozen chapter entries (millisecond time base). Populated from
    /// `seq.markers` only when `ExportSettings.embed_chapters` is true;
    /// already shifted into export-range-relative coordinates when a
    /// zone export is active, and markers outside the range are dropped.
    pub chapters: Vec<klipp_core::ChapterSnapshot>,
    /// Project working colorspace at the moment of snapshot. Read
    /// alongside `ExportSettings.odt` by the encoder's display
    /// transform to dispatch the right linear → display path.
    /// Defaults to [`Rec709Linear`] for back-compat with snapshot
    /// constructors that pre-date the field.
    #[doc(hidden)]
    pub working_colorspace: klipp_core::WorkingColorspace,
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
    /// Per-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    pub effect_keyframes: HashMap<String, Vec<Keyframe>>,
    /// Per-audio-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    pub audio_effect_keyframes: HashMap<String, Vec<Keyframe>>,
    /// Clip-level opacity keyframes.
    pub opacity_keyframes: Vec<Keyframe>,
    /// Transform position X keyframes.
    pub transform_pos_x_keyframes: Vec<Keyframe>,
    /// Transform position Y keyframes.
    pub transform_pos_y_keyframes: Vec<Keyframe>,
    /// Transform scale keyframes.
    pub transform_scale_keyframes: Vec<Keyframe>,
    /// Transform rotation keyframes.
    pub transform_rotation_keyframes: Vec<Keyframe>,
    /// Per-clip blend mode for the visible composite.
    pub blend_mode: klipp_core::BlendMode,
    /// Track-matte binding: when set, this clip's alpha is masked by the
    /// rendered RGBA of `source_clip_id` at the same timeline time. Phase 2
    /// (render path) reads this; Phase 1 (data model) only carries it.
    pub matte_source: Option<klipp_timeline::MatteBinding>,
    /// Phase 4B — when set, this clip's pixels mask every clip on a
    /// lower track at the same time-range. Forwarded as-is from the
    /// authoring `Clip`. Implicitly hides the source from the visible
    /// composite.
    pub matte_stencil: Option<klipp_timeline::StencilSettings>,
    /// True when this clip is being used as a matte source by another clip
    /// AND should be hidden from the visible composite. Default false.
    pub consumed_as_matte: bool,
    /// Optional transform inheritance from another clip in the same
    /// sequence. Forwarded as-is from the authoring `Clip` so the
    /// renderer/export resolves it via the same per-frame cache the
    /// playback compositor uses.
    pub transform_link: Option<klipp_timeline::TransformLink>,
}

#[derive(Clone)]
pub enum ClipKindSnapshot {
    Media { path: PathBuf, has_audio: bool },
    Title(TitleData),
    Generator(klipp_core::GeneratorData),
    /// Adjustment layer — applies its clip's effects to the accumulated composite below.
    AdjustmentLayer,
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
    /// Per-audio-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    pub audio_effect_keyframes: HashMap<String, Vec<Keyframe>>,
}

#[derive(Debug, Clone)]
pub struct SubtitleClipSnapshot {
    pub timeline_start_frame: u64,
    pub timeline_end_frame: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SubtitleTrackSnapshot {
    pub clips: Vec<SubtitleClipSnapshot>,
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
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
    ) -> Result<Self, RenderError> {
        Self::from_sequence_with_all(seq, &[], media_pool, settings, global_lut_path, use_marked_range)
    }

    /// Freeze the sequence, resolving compound clips (nested sequences) by
    /// flattening their inner clips into the parent timeline. `all_sequences`
    /// must contain every sequence in the project so that
    /// `ClipKind::Sequence { sequence_id }` can be resolved.
    pub fn from_sequence_with_all(
        seq: &Sequence,
        all_sequences: &[Sequence],
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
    ) -> Result<Self, RenderError> {
        let fps = seq.fps.as_f64();
        let burn_in = settings.burn_in_subtitles;

        // With both flags on, subtitles are composited into the video AND
        // emitted as a separate soft-subtitle stream — the viewer sees them
        // twice. The default ExportSettings in older code paths has both
        // true, so flag it loudly so the user (or the caller building the
        // settings) can turn one off.
        if burn_in && settings.embed_subtitles {
            tracing::warn!(
                "Export settings have burn_in_subtitles and embed_subtitles \
                 both enabled — subtitles will be rendered into the video \
                 AND emitted as a separate soft-subtitle stream (double \
                 render). Disable one of the two in the export settings."
            );
        }

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
                TrackKind::Video => {
                    video_tracks.push(snapshot_video_track(track, all_sequences, media_pool, fps, 0)?);
                }
                // Subtitle and Text tracks are composited as video overlays.
                // When burn_in_subtitles is false, skip them so they don't render
                // into the video (the user can still export sidecar SRT/VTT).
                TrackKind::Text | TrackKind::Subtitle if burn_in => {
                    video_tracks.push(snapshot_video_track(track, all_sequences, media_pool, fps, 0)?);
                }
                TrackKind::Text | TrackKind::Subtitle => {} // skip — no burn-in
                TrackKind::Audio => {
                    audio_tracks.push(snapshot_audio_track(track, all_sequences, media_pool, fps, 0)?);
                }
            }
        }

        // Collect subtitle tracks regardless of burn_in setting.
        // Subtitle text + timing is always available for soft embedding.
        let mut subtitle_tracks = Vec::new();
        for track in seq.tracks.iter() {
            if !matches!(track.kind, TrackKind::Subtitle | TrackKind::Text) {
                continue;
            }
            let mut clips = Vec::new();
            for clip in &track.clips {
                if let ClipKind::Title(data) = &clip.kind {
                    clips.push(SubtitleClipSnapshot {
                        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
                        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
                        text: data.text(),
                    });
                }
            }
            if !clips.is_empty() {
                subtitle_tracks.push(SubtitleTrackSnapshot { clips });
            }
        }

        // Chapter snapshot — sequence markers frozen as (start_ms, end_ms, title)
        // pairs. Chapter N ends at chapter N+1's start; the last chapter ends at
        // the export duration. When a zone export is active, timestamps are
        // shifted relative to the zone start and markers outside the zone are
        // skipped.
        let chapters = if settings.embed_chapters {
            let total_duration_secs = total_duration.to_secs_f64();
            let total_duration_ms = (total_duration_secs * 1000.0) as u64;
            let (export_start_ms, export_end_ms) = if let Some((start_f, end_f)) = export_range {
                let start_ms = (start_f as f64 * 1000.0 / fps) as u64;
                let end_ms = (end_f as f64 * 1000.0 / fps) as u64;
                (start_ms, end_ms)
            } else {
                (0u64, total_duration_ms)
            };
            let export_duration_ms = export_end_ms.saturating_sub(export_start_ms);

            // Step 1: collect in-range markers as (shifted_ms, title).
            let in_range: Vec<(u64, String)> = seq
                .markers
                .iter()
                .filter_map(|m| {
                    let ms = (m.position.to_secs_f64() * 1000.0) as u64;
                    if ms < export_start_ms || ms >= export_end_ms {
                        return None;
                    }
                    Some((ms - export_start_ms, m.label.clone()))
                })
                .collect();

            // Step 2: pair each marker with the next marker's start (or export
            // end for the last). Iter is already sorted by position because
            // sequence markers are inserted in order.
            let mut out: Vec<klipp_core::ChapterSnapshot> = Vec::with_capacity(in_range.len());
            for i in 0..in_range.len() {
                let (start, ref title) = in_range[i];
                let end = if i + 1 < in_range.len() {
                    in_range[i + 1].0
                } else {
                    export_duration_ms
                };
                if end <= start {
                    continue;
                }
                out.push(klipp_core::ChapterSnapshot {
                    start_ms: start,
                    end_ms: end,
                    title: title.clone(),
                });
            }
            out
        } else {
            Vec::new()
        };

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
            subtitle_tracks,
            chapters,
            // Default; the caller layer (klipp-app's export-job
            // dispatch) overrides this to match the project's active
            // working colorspace before kicking the job. Doing it
            // there keeps the snapshot construction free of any
            // dependency on the engine's atomic.
            working_colorspace: klipp_core::WorkingColorspace::default(),
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
            effect_keyframes: angle_clip.effect_keyframes.clone(),
            audio_effect_keyframes: angle_clip.audio_effect_keyframes.clone(),
            opacity_keyframes: angle_clip.opacity_keyframes.clone(),
            transform_pos_x_keyframes: angle_clip.transform_pos_x_keyframes.clone(),
            transform_pos_y_keyframes: angle_clip.transform_pos_y_keyframes.clone(),
            transform_scale_keyframes: angle_clip.transform_scale_keyframes.clone(),
            transform_rotation_keyframes: angle_clip.transform_rotation_keyframes.clone(),
            blend_mode: angle_clip.blend_mode,
            matte_source: angle_clip.matte_source.clone(),
            matte_stencil: angle_clip.matte_stencil.clone(),
            consumed_as_matte: angle_clip.consumed_as_matte,
            transform_link: None,
        });
    }

    Ok(())
}

/// Flatten a compound clip's inner sequence video tracks into the parent clip list.
/// Delegates the time-mapping math to
/// `klipp_timeline::flatten::flatten_clip_for_track`, which produces real
/// `Clip` values that we then map to `ClipSnapshot`. The shared flattener is
/// also used by `sync_sequence_to_engine` so the viewer and exporter agree
/// by construction.
fn flatten_compound_video_clips(
    compound: &klipp_timeline::Clip,
    sequence_id: klipp_core::SequenceId,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
    out: &mut Vec<ClipSnapshot>,
) -> Result<(), RenderError> {
    let flat_clips = klipp_timeline::flatten::flatten_clip_for_track(
        compound,
        sequence_id,
        all_sequences,
        TrackKind::Video,
        depth,
    );
    for clip in flat_clips {
        if matches!(clip.kind, ClipKind::Gap | ClipKind::Sequence { .. }) {
            continue;
        }
        out.push(snapshot_clip(&clip, media_pool, fps)?);
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
                        audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
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
    let flat_clips = klipp_timeline::flatten::flatten_clip_for_track(
        compound,
        sequence_id,
        all_sequences,
        TrackKind::Audio,
        depth,
    );
    for clip in flat_clips {
        let Some(media_id) = clip.media_id() else { continue };
        let Some(media) = media_pool.get(&media_id) else { continue };
        if !media.has_audio {
            continue;
        }
        out.push(AudioClipSnapshot {
            timeline_start_frame: (clip.timeline_in.to_secs_f64() * fps).round() as u64,
            timeline_end_frame: (clip.timeline_out.to_secs_f64() * fps).round() as u64,
            source_start_secs: clip.source_in.to_secs_f64(),
            path: media.path.clone(),
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
        });
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
            audio_effect_keyframes: angle_clip.audio_effect_keyframes.clone(),
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
        ClipKind::AdjustmentLayer => ClipKindSnapshot::AdjustmentLayer,
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
        effect_keyframes: clip.effect_keyframes.clone(),
        audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
        opacity_keyframes: clip.opacity_keyframes.clone(),
        transform_pos_x_keyframes: clip.transform_pos_x_keyframes.clone(),
        transform_pos_y_keyframes: clip.transform_pos_y_keyframes.clone(),
        transform_scale_keyframes: clip.transform_scale_keyframes.clone(),
        transform_rotation_keyframes: clip.transform_rotation_keyframes.clone(),
        blend_mode: clip.blend_mode,
        matte_source: clip.matte_source.clone(),
        matte_stencil: clip.matte_stencil.clone(),
        consumed_as_matte: clip.consumed_as_matte,
        transform_link: None,
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
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};
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
            kind: MediaItemKind::File,
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
            parent_media_id: None,
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
            audio_bitrate_kbps: Some(192), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, embed_chapters: false,
            bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer }
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
                assert_eq!(data.text(), title.text());
            }
            _ => panic!("expected Title clip"),
        }
    }

    // ── Chapter snapshot tests (#44) ──────────────────────────────────

    fn make_marker_at_secs(secs: f64, label: &str) -> klipp_timeline::Marker {
        klipp_timeline::Marker {
            id: klipp_core::MarkerId::new(),
            position: klipp_core::Timecode {
                value: (secs * 24.0).round() as i64,
                rate: 24,
            },
            label: label.to_string(),
            color: klipp_core::MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        }
    }

    fn make_sequence_with_length_secs(secs: f64) -> Sequence {
        let mut seq = make_sequence();
        let frames = (secs * 24.0).round() as i64;
        let gap = Clip::gap(make_rational(0, 24), make_rational(frames, 24));
        seq.tracks[0].clips.push(gap);
        seq
    }

    #[test]
    fn chapter_end_times_inferred_correctly() {
        // Three markers at 0s, 10s, 30s inside a 60s sequence.
        // Expected: chapter ends at 10000, 30000, 60000 ms.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(0.0, "Intro"));
        seq.markers.push(make_marker_at_secs(10.0, "Act 1"));
        seq.markers.push(make_marker_at_secs(30.0, "Act 2"));

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(snap.chapters.len(), 3, "three markers → three chapters");
        assert_eq!(snap.chapters[0].start_ms, 0);
        assert_eq!(snap.chapters[0].end_ms, 10_000);
        assert_eq!(snap.chapters[0].title, "Intro");
        assert_eq!(snap.chapters[1].start_ms, 10_000);
        assert_eq!(snap.chapters[1].end_ms, 30_000);
        assert_eq!(snap.chapters[2].start_ms, 30_000);
        assert_eq!(snap.chapters[2].end_ms, 60_000);
    }

    #[test]
    fn chapters_shifted_for_zone_export() {
        // Markers at 5s and 15s; zone export covers [10s, 30s].
        // The 5s marker is outside the zone and must be dropped; the
        // 15s marker shifts to start_ms = 5000 (relative to zone start)
        // and its end clamps to the zone duration 20000.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(5.0, "Outside"));
        seq.markers.push(make_marker_at_secs(15.0, "Inside"));
        seq.in_point = Some(make_rational(240, 24));  // 10s
        seq.out_point = Some(make_rational(720, 24)); // 30s

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, true).unwrap();

        assert_eq!(
            snap.chapters.len(),
            1,
            "5s marker is outside the zone — only 15s marker survives"
        );
        assert_eq!(snap.chapters[0].start_ms, 5_000, "15s - 10s zone start = 5000ms");
        assert_eq!(snap.chapters[0].end_ms, 20_000, "clamped to zone duration");
        assert_eq!(snap.chapters[0].title, "Inside");
    }

    #[test]
    fn chapters_empty_when_no_markers() {
        // embed_chapters is true but sequence has no markers — should
        // produce an empty chapters Vec, not an error or panic.
        let seq = make_sequence_with_length_secs(60.0);

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert!(snap.chapters.is_empty());
    }

    #[test]
    fn chapters_empty_when_embed_disabled() {
        // Even with markers, embed_chapters=false → no snapshot entries.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(10.0, "Act 1"));
        seq.markers.push(make_marker_at_secs(30.0, "Act 2"));

        let settings = make_settings(); // embed_chapters: false by default

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert!(snap.chapters.is_empty());
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

    #[test]
    fn snapshot_carries_effect_keyframes() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let kf0 = klipp_core::Keyframe {
            time_secs: 0.0,
            value: 5.0,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        };
        let kf1 = klipp_core::Keyframe {
            time_secs: 1.0,
            value: 50.0,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        };

        let mut effect_keyframes = std::collections::HashMap::new();
        effect_keyframes.insert("0:radius".to_string(), vec![kf0.clone(), kf1.clone()]);

        let opacity_kf = klipp_core::Keyframe {
            time_secs: 0.5,
            value: 0.5,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        };

        let mut clip = Clip::gap(make_rational(0, 24), make_rational(48, 24));
        clip.kind = ClipKind::Media { media_id };
        clip.source_in = make_rational(0, 24);
        clip.source_out = make_rational(48, 24);
        clip.effect_keyframes = effect_keyframes;
        clip.opacity_keyframes = vec![opacity_kf];
        clip.transform_scale_keyframes = vec![kf0.clone(), kf1.clone()];

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let sc = &v1.clips[0];

        assert_eq!(sc.effect_keyframes.len(), 1);
        assert!(sc.effect_keyframes.contains_key("0:radius"));
        assert_eq!(sc.effect_keyframes["0:radius"].len(), 2);
        assert_eq!(sc.opacity_keyframes.len(), 1);
        assert!((sc.opacity_keyframes[0].value - 0.5).abs() < 0.001);
        assert_eq!(sc.transform_scale_keyframes.len(), 2);
        assert!(sc.transform_pos_x_keyframes.is_empty());
        assert!(sc.transform_pos_y_keyframes.is_empty());
        assert!(sc.transform_rotation_keyframes.is_empty());
    }

    #[test]
    fn snapshot_carries_audio_effect_keyframes() {
        // Mirrors snapshot_carries_effect_keyframes for the audio chain:
        // both ClipSnapshot.audio_effect_keyframes (video-track snapshot
        // path) and AudioClipSnapshot.audio_effect_keyframes (audio-track
        // snapshot path) must round-trip the source clip's data so the
        // offline mixdown path sees the same kfs as live playback.
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mk_kf = |t: f64, v: f32| klipp_core::Keyframe {
            time_secs: t,
            value: v,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        };

        let mut audio_effect_keyframes = std::collections::HashMap::new();
        audio_effect_keyframes.insert(
            "0:gain".to_string(),
            vec![mk_kf(0.0, -6.0), mk_kf(1.0, 0.0)],
        );
        audio_effect_keyframes.insert(
            "0:__bypass".to_string(),
            vec![mk_kf(0.5, 1.0)],
        );

        let mut clip = Clip::gap(make_rational(0, 24), make_rational(48, 24));
        clip.kind = ClipKind::Media { media_id };
        clip.source_in = make_rational(0, 24);
        clip.source_out = make_rational(48, 24);
        clip.audio_effect_keyframes = audio_effect_keyframes;

        let mut seq = make_sequence();
        // Mirror onto both video and audio tracks so we exercise BOTH
        // snapshot paths (ClipSnapshot via video lane, AudioClipSnapshot
        // via audio lane).
        seq.tracks[0].clips.push(clip.clone());
        seq.tracks[1].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        // Video-side ClipSnapshot must carry the field.
        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let sc = &v1.clips[0];
        assert_eq!(sc.audio_effect_keyframes.len(), 2,
            "ClipSnapshot must carry both kf tracks");
        assert!(sc.audio_effect_keyframes.contains_key("0:gain"));
        assert!(sc.audio_effect_keyframes.contains_key("0:__bypass"));
        assert_eq!(sc.audio_effect_keyframes["0:gain"].len(), 2);

        // Audio-side AudioClipSnapshot (offline mixdown path) must carry
        // the same data.
        assert_eq!(snap.audio_tracks.len(), 1);
        let ac = &snap.audio_tracks[0].clips[0];
        assert_eq!(ac.audio_effect_keyframes.len(), 2,
            "AudioClipSnapshot must carry both kf tracks for offline mixdown");
        assert!((ac.audio_effect_keyframes["0:gain"][0].value - (-6.0)).abs() < 1e-6);
        assert!((ac.audio_effect_keyframes["0:gain"][1].value - 0.0).abs() < 1e-6);
        assert!((ac.audio_effect_keyframes["0:__bypass"][0].time_secs - 0.5).abs() < 1e-9);
    }

    #[test]
    fn snapshot_collects_subtitle_tracks() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mut seq = make_sequence();
        // Add a video clip so duration is non-zero
        seq.tracks[0].clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(120, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(120, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(120, 24))
        });

        // Add a subtitle track with two clips
        let mut sub_track = klipp_timeline::Track::new("Subtitles", TrackKind::Subtitle);
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let mut tb = klipp_timeline::TextBox::default();
                tb.text = "Hello World".to_string();
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(24, 24),
            timeline_out: make_rational(72, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(24, 24), make_rational(72, 24))
        });
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let mut tb = klipp_timeline::TextBox::default();
                tb.text = "Second subtitle".to_string();
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(96, 24),
            timeline_out: make_rational(120, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            ..Clip::gap(make_rational(96, 24), make_rational(120, 24))
        });
        seq.tracks.push(sub_track);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(snap.subtitle_tracks.len(), 1);
        assert_eq!(snap.subtitle_tracks[0].clips.len(), 2);
        assert_eq!(snap.subtitle_tracks[0].clips[0].text, "Hello World");
        assert_eq!(snap.subtitle_tracks[0].clips[0].timeline_start_frame, 24);
        assert_eq!(snap.subtitle_tracks[0].clips[0].timeline_end_frame, 72);
        assert_eq!(snap.subtitle_tracks[0].clips[1].text, "Second subtitle");
        assert_eq!(snap.subtitle_tracks[0].clips[1].timeline_start_frame, 96);
        assert_eq!(snap.subtitle_tracks[0].clips[1].timeline_end_frame, 120);
    }

    #[test]
    fn snapshot_subtitle_tracks_present_even_without_burn_in() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        });

        let mut sub_track = klipp_timeline::Track::new("Subs", TrackKind::Subtitle);
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let mut tb = klipp_timeline::TextBox::default();
                tb.text = "Test".to_string();
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(24, 24))
        });
        seq.tracks.push(sub_track);

        // burn_in OFF — subtitle data should still be in subtitle_tracks
        let mut settings = make_settings();
        settings.burn_in_subtitles = false;
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(snap.subtitle_tracks.len(), 1, "subtitle_tracks should be populated even without burn-in");
        assert_eq!(snap.subtitle_tracks[0].clips[0].text, "Test");
    }

    #[test]
    fn snapshot_no_subtitle_tracks_when_none_exist() {
        let pool = MediaPool::new();
        let seq = make_sequence();
        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();
        assert!(snap.subtitle_tracks.is_empty());
    }
}
