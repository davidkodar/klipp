use crate::error::RenderError;
use klipp_core::{ExportSettings, Keyframe, RationalTime};
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, Track, TrackKind};
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
    /// Everything the video render reads: see [`VideoTimeline`].
    pub video: VideoTimeline,
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

/// The video half of an export: the timeline exactly as the viewer's
/// engine receives it — flattened by the same
/// `klipp_timeline::flatten::flatten_sequence` — rendered by the same
/// `klipp_compose::Renderer`. Keeping the timeline as-is, rather than a
/// converted copy, is what lets export and viewer agree by construction.
#[derive(Clone)]
pub struct VideoTimeline {
    /// The flattened sequence. Text and subtitle tracks are hidden when
    /// subtitles are not burned in.
    pub sequence: Sequence,
    /// The file behind each media clip, resolved at snapshot time.
    pub media: HashMap<klipp_core::MediaId, MediaFile>,
    /// Strength of the global LUT (`SequenceSnapshot::global_lut_path`).
    pub global_lut_opacity: f32,
}

impl Default for VideoTimeline {
    /// No video: an empty timeline.
    fn default() -> Self {
        let mut sequence = Sequence::new_default();
        sequence.tracks.clear();
        Self { sequence, media: HashMap::new(), global_lut_opacity: 1.0 }
    }
}

/// A media file as export decodes it.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaFile {
    pub path: PathBuf,
    /// The media's camera log format, when it is one (S-Log3, V-Log, ...).
    /// File tags never say so; the viewer takes it from the media pool,
    /// and export must decode the file the same way.
    pub log_format: Option<klipp_core::LogFormat>,
}

/// A media item's camera log format for its export decoder: the same
/// effective format (the user's choice, else detection) the viewer's
/// decoder registry holds, or `None` for anything that is not a camera log.
fn camera_log_format(media: &klipp_media::MediaItem) -> Option<klipp_core::LogFormat> {
    Some(media.effective_log_format()).filter(|f| f.camera_colourspace().is_some())
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

        let mut audio_tracks = Vec::new();
        for track in seq.tracks.iter().rev() {
            if track.kind == TrackKind::Audio {
                audio_tracks.push(snapshot_audio_track(track, all_sequences, media_pool, fps, 0)?);
            }
        }

        // Video: the viewer's flattened timeline, as it is. Text and
        // subtitle tracks render into the picture only when burning in
        // (the user can still export sidecar SRT/VTT).
        let mut timeline = klipp_timeline::flatten::flatten_sequence(seq, all_sequences);
        if !burn_in {
            for t in &mut timeline.tracks {
                if matches!(t.kind, TrackKind::Text | TrackKind::Subtitle) {
                    t.visible = false;
                }
            }
        }
        let mut media = HashMap::new();
        for clip in timeline.tracks.iter().flat_map(|t| &t.clips) {
            if let ClipKind::Media { media_id } = &clip.kind {
                let item = media_pool
                    .get(media_id)
                    .ok_or_else(|| RenderError::Snapshot(format!("media {media_id:?} not in pool")))?;
                media.insert(*media_id, MediaFile { path: item.path.clone(), log_format: camera_log_format(item) });
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

        // Chapter snapshot — only markers explicitly tagged
        // `MarkerKind::Chapter` are baked into the container. Mixing
        // workflow markers (ToDo, Comment, BestTake) into chapter
        // atoms would clutter player-side chapter menus, so we
        // require the explicit kind.
        //
        // Chapter N ends at chapter N+1's start; the last chapter
        // ends at the export duration. When a zone export is
        // active, timestamps are shifted relative to the zone start
        // and markers outside the zone are skipped.
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

            // Step 1: collect in-range Chapter-kind markers as
            // (shifted_ms, title).
            let in_range: Vec<(u64, String)> = seq
                .markers
                .iter()
                .filter(|m| m.kind == klipp_core::MarkerKind::Chapter)
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
            video: VideoTimeline { sequence: timeline, media, global_lut_opacity: 1.0 },
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
    use klipp_core::{ClipId, LogFormat, ExportAudioCodec, ExportFormat, ExportVideoCodec, Fps, MediaId, Resolution, EncoderPreset};
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

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
            log_format_user_set: false,
            parent_media_id: None,
        }
    }

    fn make_marker_at_secs(secs: f64, label: &str) -> klipp_timeline::Marker {
        // These tests cover chapter-snapshot behaviour, so the default
        // helper produces Chapter-kind markers (the only kind that
        // gets baked into chapter atoms). The non-Chapter exclusion
        // test below uses an inline construction with a different kind.
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
            kind: klipp_core::MarkerKind::Chapter,
        }
    }


    fn make_sequence_with_length_secs(secs: f64) -> Sequence {
        let mut seq = make_sequence();
        let frames = (secs * 24.0).round() as i64;
        let gap = Clip::gap(make_rational(0, 24), make_rational(frames, 24));
        seq.tracks[0].clips.push(gap);
        seq
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
    fn chapter_filter_excludes_non_chapter_kinds() {
        // Mix of marker kinds. Only Chapter-kind markers should land
        // in the chapter snapshot — the others are workflow markers
        // (ToDo / Comment / BestTake / Section / None) that don't
        // belong in player-side chapter menus.
        let mut seq = make_sequence_with_length_secs(60.0);
        // Default helper produces Chapter-kind markers.
        seq.markers.push(make_marker_at_secs(0.0, "Real chapter A"));
        seq.markers.push(make_marker_at_secs(30.0, "Real chapter B"));
        // Inline non-Chapter markers — must be filtered out.
        for (secs, label, kind) in [
            (5.0_f64, "todo note", klipp_core::MarkerKind::ToDo),
            (10.0, "comment note", klipp_core::MarkerKind::Comment),
            (15.0, "best take", klipp_core::MarkerKind::BestTake),
            (20.0, "section break", klipp_core::MarkerKind::Section),
            (25.0, "untyped", klipp_core::MarkerKind::None),
        ] {
            seq.markers.push(klipp_timeline::Marker {
                id: klipp_core::MarkerId::new(),
                position: klipp_core::Timecode {
                    value: (secs * 24.0).round() as i64,
                    rate: 24,
                },
                label: label.to_string(),
                color: klipp_core::MarkerColor::Yellow,
                duration: None,
                comment: String::new(),
                kind,
            });
        }

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();

        assert_eq!(
            snap.chapters.len(),
            2,
            "only the two Chapter-kind markers survive the filter"
        );
        assert_eq!(snap.chapters[0].title, "Real chapter A");
        assert_eq!(snap.chapters[1].title, "Real chapter B");
        // Chapter B should end at the export end (60s) since no
        // Chapter-kind marker follows it — the workflow markers
        // between A and B don't pin its end time.
        assert_eq!(snap.chapters[0].end_ms, 30_000);
        assert_eq!(snap.chapters[1].end_ms, 60_000);
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

    fn media_clip(media_id: MediaId, from: i64, to: i64) -> Clip {
        Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(from, 24),
            timeline_out: make_rational(to, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(to - from, 24),
            ..Clip::gap(make_rational(from, 24), make_rational(to, 24))
        }
    }

    /// The video half is the viewer's flattened timeline, and every media
    /// clip's file is in the media table.
    #[test]
    fn video_timeline_is_the_flattened_sequence_with_its_media() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(media_clip(media_id, 0, 48));
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false).unwrap();
        let flat = klipp_timeline::flatten::flatten_sequence(&seq, &[]);
        assert_eq!(snap.video.sequence.tracks.len(), flat.tracks.len());
        assert_eq!(snap.video.sequence.tracks[0].clips[0].id, seq.tracks[0].clips[0].id);
        assert_eq!(
            snap.video.media.get(&media_id),
            Some(&MediaFile { path: PathBuf::from("/media/clip1.mp4"), log_format: None }),
        );
    }

    /// The export decoder gets the same log format the viewer's decoder
    /// registry holds: the user's choice, else detection; and nothing for
    /// media that is not camera log, including an explicit sRGB override
    /// of a detected log.
    #[test]
    fn media_table_carries_the_effective_camera_log_format() {
        let cases: [(LogFormat, bool, LogFormat, Option<LogFormat>); 4] = [
            (LogFormat::Srgb, false, LogFormat::SLog3, Some(LogFormat::SLog3)), // detected
            (LogFormat::VLog, true, LogFormat::SLog3, Some(LogFormat::VLog)),   // user's choice wins
            (LogFormat::Srgb, true, LogFormat::SLog3, None),                    // explicit sRGB override
            (LogFormat::Srgb, false, LogFormat::PQ, None),                      // PQ: the file says so
        ];
        for (chosen, user_set, detected, want) in cases {
            let media_id = MediaId::new();
            let mut item = make_media_item(media_id, "/media/log.mp4");
            item.log_format = chosen;
            item.log_format_user_set = user_set;
            item.detected_log_format = detected;
            let mut pool = MediaPool::new();
            pool.add(item);
            let mut seq = make_sequence();
            seq.tracks[0].clips.push(media_clip(media_id, 0, 24));
            let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false).unwrap();
            assert_eq!(
                snap.video.media[&media_id].log_format, want,
                "chosen {chosen:?} (user set {user_set}), detected {detected:?}",
            );
        }
    }

    /// Text and subtitle tracks render into the picture only when burning
    /// subtitles in.
    #[test]
    fn subtitle_tracks_are_hidden_unless_burned_in() {
        let mut seq = make_sequence();
        seq.tracks.insert(0, Track::new("Subtitles", TrackKind::Subtitle));
        let pool = MediaPool::new();
        for burn in [true, false] {
            let mut settings = make_settings();
            settings.burn_in_subtitles = burn;
            let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false).unwrap();
            let sub = snap.video.sequence.tracks.iter().find(|t| t.kind == TrackKind::Subtitle).unwrap();
            assert_eq!(sub.visible, burn);
        }
    }

    /// A media clip whose file is missing from the pool cannot be exported.
    #[test]
    fn missing_media_is_an_error() {
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(media_clip(MediaId::new(), 0, 24));
        assert!(SequenceSnapshot::from_sequence(&seq, &MediaPool::new(), &make_settings(), None, false).is_err());
    }
}
