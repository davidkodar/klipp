use crate::error::RenderError;
use klipp_core::{
    ClipId, ExportSettings, FilmGrainSettings, HueSatCurves, KeyframeTracks, LogFormat,
    MaskStack, QualifierSettings, RationalTime, TransitionId,
};
use klipp_effects::BuiltinEffect;
use klipp_media::MediaPool;
use klipp_plugin::AppliedEffect;
use klipp_timeline::{
    ClipColorGrade, ClipKind, Sequence, TitleData, Track, TrackKind, Transition, TransitionKind,
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
    /// Full clone of all color grade fields.
    pub color_grade: ClipColorGradeSnapshot,
    /// OFX effects chain.
    pub effects: Vec<AppliedEffect>,
    /// Built-in effects chain (GPU/CPU).
    pub builtin_effects: Vec<BuiltinEffect>,
}

#[derive(Clone)]
pub enum ClipKindSnapshot {
    Media { path: PathBuf, has_audio: bool },
    Title(TitleData),
    Gap,
}

#[derive(Clone)]
pub struct ClipColorGradeSnapshot {
    pub enabled: bool,
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub lift_x: f32,
    pub lift_y: f32,
    pub lift_master: f32,
    pub gamma_x: f32,
    pub gamma_y: f32,
    pub gamma_master: f32,
    pub gain_x: f32,
    pub gain_y: f32,
    pub gain_master: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub offset_master: f32,
    pub lift_sat: f32,
    pub lift_bri: f32,
    pub gamma_sat: f32,
    pub gamma_bri: f32,
    pub gain_sat: f32,
    pub gain_bri: f32,
    pub offset_sat: f32,
    pub offset_bri: f32,
    pub hue: f32,
    pub global_mix: f32,
    pub curves: klipp_core::CurveSet,
    pub curves_preserve_luma: bool,
    pub curves_mix: f32,
    pub huesat: HueSatCurves,
    pub huesat_mix: f32,
    pub qualifier: QualifierSettings,
    pub masks: MaskStack,
    pub log_format: LogFormat,
    pub grain: FilmGrainSettings,
    pub keyframe_tracks: KeyframeTracks,
}

impl ClipColorGradeSnapshot {
    /// Default identity grade (no changes applied).
    pub fn default_identity() -> Self {
        Self::from(&ClipColorGrade::default())
    }
}

impl From<&ClipColorGrade> for ClipColorGradeSnapshot {
    fn from(g: &ClipColorGrade) -> Self {
        Self {
            enabled: g.enabled,
            exposure: g.exposure,
            contrast: g.contrast,
            saturation: g.saturation,
            temperature: g.temperature,
            tint: g.tint,
            highlights: g.highlights,
            shadows: g.shadows,
            whites: g.whites,
            blacks: g.blacks,
            lift_x: g.lift_x,
            lift_y: g.lift_y,
            lift_master: g.lift_master,
            gamma_x: g.gamma_x,
            gamma_y: g.gamma_y,
            gamma_master: g.gamma_master,
            gain_x: g.gain_x,
            gain_y: g.gain_y,
            gain_master: g.gain_master,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            offset_master: g.offset_master,
            lift_sat: g.lift_sat,
            lift_bri: g.lift_bri,
            gamma_sat: g.gamma_sat,
            gamma_bri: g.gamma_bri,
            gain_sat: g.gain_sat,
            gain_bri: g.gain_bri,
            offset_sat: g.offset_sat,
            offset_bri: g.offset_bri,
            hue: g.hue,
            global_mix: g.global_mix,
            curves: g.curves.clone(),
            curves_preserve_luma: g.curves_preserve_luma,
            curves_mix: g.curves_mix,
            huesat: g.huesat.clone(),
            huesat_mix: g.huesat_mix,
            qualifier: g.qualifier.clone(),
            masks: g.masks.clone(),
            log_format: g.log_format,
            grain: g.grain.clone(),
            keyframe_tracks: g.keyframe_tracks.clone(),
        }
    }
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
}

#[derive(Clone)]
pub struct TransitionSnapshot {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub duration_frames: u64,
    pub clip_a_id: ClipId,
    pub clip_b_id: ClipId,
}

// ═══════════════════════════════════════════════════════════════════
// Construction
// ═══════════════════════════════════════════════════════════════════

impl SequenceSnapshot {
    /// Freeze the entire sequence at this moment. All MediaIds are resolved
    /// to file paths via `media_pool`. RationalTime values are converted to
    /// frame numbers and f64 seconds. The `global_lut_path` is passed from
    /// the engine's ColorPipeline.
    pub fn from_sequence(
        seq: &Sequence,
        media_pool: &MediaPool,
        _settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
    ) -> Result<Self, RenderError> {
        let fps = seq.fps.as_f64();

        let export_range = match (&seq.in_point, &seq.out_point) {
            (Some(in_pt), Some(out_pt)) => {
                let start = time_to_frame(in_pt, fps);
                let end = time_to_frame(out_pt, fps);
                Some((start, end))
            }
            _ => None,
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
                    video_tracks.push(snapshot_video_track(track, media_pool, fps)?);
                }
                TrackKind::Audio => {
                    audio_tracks.push(snapshot_audio_track(track, media_pool, fps)?);
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

fn snapshot_video_track(
    track: &Track,
    media_pool: &MediaPool,
    fps: f64,
) -> Result<VideoTrackSnapshot, RenderError> {
    let mut clips = Vec::with_capacity(track.clips.len());
    for clip in &track.clips {
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

fn snapshot_audio_track(
    track: &Track,
    media_pool: &MediaPool,
    fps: f64,
) -> Result<AudioTrackSnapshot, RenderError> {
    let mut clips = Vec::new();
    for clip in &track.clips {
        if let Some(media_id) = clip.media_id() {
            if let Some(media) = media_pool.get(&media_id) {
                if media.has_audio {
                    clips.push(AudioClipSnapshot {
                        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
                        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
                        source_start_secs: clip.source_in.to_secs_f64(),
                        path: media.path.clone(),
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
    };

    Ok(ClipSnapshot {
        id: clip.id,
        kind,
        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
        source_start_secs: clip.source_in.to_secs_f64(),
        source_end_secs: clip.source_out.to_secs_f64(),
        color_grade: ClipColorGradeSnapshot::from(&clip.color_grade),
        effects: clip.effects.clone(),
        builtin_effects: clip.builtin_effects.clone(),
    })
}

fn snapshot_transition(t: &Transition, fps: f64) -> TransitionSnapshot {
    TransitionSnapshot {
        id: t.id,
        kind: t.kind,
        duration_frames: time_to_frame(&t.duration, fps).max(1),
        clip_a_id: t.clip_a_id,
        clip_b_id: t.clip_b_id,
    }
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
            offline: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: vec![],
            user_notes: String::new(),
            bin: None,
            added_at: 0,
            log_format: klipp_core::LogFormat::default(),
            skim_file: None,
            waveform_data: None,
            audio_cache: None,
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
            audio_bitrate_kbps: Some(192),
        }
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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip.clone()); // Video 1 is tracks[1] (V2, V1, A1)

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip);
        seq.in_point = Some(make_rational(24, 24));  // 1 second in
        seq.out_point = Some(make_rational(120, 24)); // 5 seconds in

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

        assert_eq!(snap.export_range, Some((24, 120)));
        assert_eq!(snap.frames_to_export(), 96);
        assert_eq!(snap.start_frame(), 24);
        assert_eq!(snap.end_frame(), 120);
    }

    #[test]
    fn snapshot_captures_color_grade() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mut grade = ClipColorGrade::default();
        grade.enabled = true;
        grade.exposure = 1.5;
        grade.contrast = 0.3;
        grade.saturation = 1.2;
        grade.temperature = -20.0;

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            color_grade: grade,
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

        let v1 = snap.video_tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
        let sc = &v1.clips[0];
        assert!(sc.color_grade.enabled);
        assert_eq!(sc.color_grade.exposure, 1.5);
        assert_eq!(sc.color_grade.contrast, 0.3);
        assert_eq!(sc.color_grade.saturation, 1.2);
        assert_eq!(sc.color_grade.temperature, -20.0);
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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let pool = MediaPool::new();
        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

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
        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Gap,
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let pool = MediaPool::new();
        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let clip_b = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id: media_id_b },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let mut seq = make_sequence();
        // Default tracks: V2(0), V1(1), A1(2)
        seq.tracks[0].clips.push(clip_b); // V2
        seq.tracks[1].clips.push(clip_a); // V1

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

        // Tracks are reversed to bottom-to-top: A1 last in seq, but we skip audio in video_tracks
        // Video tracks should have V1 first (bottom), V2 second (top) after reversal
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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };
        let clip_b = Clip {
            id: clip_b_id,
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(36, 24),
            timeline_out: make_rational(96, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(60, 24),
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let transition = Transition {
            id: TransitionId::new(),
            kind: TransitionKind::CrossDissolve,
            duration: make_rational(12, 24), // 0.5 sec
            clip_a_id,
            clip_b_id,
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip_a);
        seq.tracks[1].clips.push(clip_b);
        seq.tracks[1].transitions.push(transition);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

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
            color_grade: ClipColorGrade::default(),
            effects: vec![],
            builtin_effects: vec![],
            linked_clip_id: None,
            group_id: None,
            magnetic_anchor: None,
        };

        let mut seq = make_sequence();
        seq.tracks[2].clips.push(clip); // A1
        seq.tracks[2].volume = 0.8;
        seq.tracks[2].pan = -0.5;

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None).unwrap();

        assert_eq!(snap.audio_tracks.len(), 1);
        let at = &snap.audio_tracks[0];
        assert_eq!(at.volume, 0.8);
        assert_eq!(at.pan, -0.5);
        assert_eq!(at.clips.len(), 1);
        assert_eq!(at.clips[0].source_start_secs, 1.0);
    }
}
