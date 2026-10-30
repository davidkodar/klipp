//! An export never writes a clip without the analysis its effect reads
//! (M4 step 9). A Stabilisation clip whose media has no analysis, and an
//! export given nothing to make one with, is refused before its first
//! frame, naming the clip and the effect, and leaves no `.partial`.

use klipp_core::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, Fps, HdrMode, MediaId, OdtChoice, RateControl, RationalTime, Resolution, ToneMapOp, WorkingColorspace};
use klipp_effects::BuiltinEffect;
use klipp_render::snapshot::{MediaFile, SequenceSnapshot, VideoTimeline};
use klipp_render::{partial_path_for, ExportJob, RenderError};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const N: i64 = 50;

#[test]
fn a_clip_without_its_analysis_refuses_the_export_by_name() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-export-missing-analysis");
    // Never decoded: the refusal comes first. A file of its own, so no
    // analysis of it is kept anywhere.
    let media = dir.join(format!("beach-{}.mov", uuid::Uuid::new_v4()));
    std::fs::write(&media, b"not decoded").unwrap();
    let out = dir.join("out.mp4");
    let _ = std::fs::remove_file(&out);

    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let mut clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(N), ..klipp_timeline::Clip::gap(tc(0), tc(N)) };
    clip.name = Some("Beach".into());
    let clip = klipp_timeline::test_clip::TestClip::of(clip).effect(BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.1, zoom: true }).build();
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: 64, height: 64 };
    let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    video.sequence.tracks.push(track);
    video.media.insert(id, MediaFile { path: media.clone(), duration_secs: Some(2.0), fps: Some((25, 1)), resolution: Some((64, 64)), ..Default::default() });
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: 64, height: 64, sample_rate: 48000,
        duration_frames: N as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.clone(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 18, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    let err = ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.clone(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: N as u64,
    }
    .run()
    .expect_err("an export must not write a clip without its analysis");
    let RenderError::Analysis(why) = &err else { panic!("refused for another reason: {err}") };
    assert!(why.contains("\u{201c}Beach\u{201d}") && why.contains("Stabilisation"), "names the clip and the effect: {why}");
    assert!(!out.exists() && !partial_path_for(&out).exists(), "nothing written");
}
