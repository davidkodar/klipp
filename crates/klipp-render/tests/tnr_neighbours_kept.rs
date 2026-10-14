//! An export keeps the neighbour reader temporal noise reduction reads
//! (#1980): one per file for the whole export, not one a frame, and every
//! frame is bit for bit the frame a reader opened per frame makes,
//! across a cut, a dissolve between two clips of the file and a reversed
//! clip, whose neighbours are behind the reader. Needs the ffmpeg CLI;
//! skips without it.

use klipp_core::*;
use klipp_render::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use klipp_render::*;
use std::path::{Path, PathBuf};

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: i64 = 30;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-tnr-neighbours-kept", name)
}

/// Two seconds of a moving pattern: every frame differs, so a neighbour
/// of the wrong time is another picture.
fn source() -> Option<PathBuf> {
    let path = tmp("pattern.mkv");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:r=25:d=2"])
        .args(["-vf", "format=yuv420p", "-c:v", "ffv1"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

fn denoised(clip: klipp_timeline::Clip) -> klipp_timeline::Clip {
    klipp_timeline::test_clip::TestClip::of(clip).effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.3 }).build()
}

/// Frames 0-10 and 10-20 two clips of the file with a four-frame
/// dissolve over the cut, then 20-30 a third played backward, all with
/// temporal noise reduction.
fn snapshot(src: &Path) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let a = denoised(klipp_timeline::Clip::media(id, tc(0), tc(10), tc(5), tc(15)));
    let b = denoised(klipp_timeline::Clip::media(id, tc(10), tc(20), tc(25), tc(35)));
    let mut c = denoised(klipp_timeline::Clip::media(id, tc(20), tc(30), tc(10), tc(20)));
    c.speed = -1.0;
    let dissolve = klipp_timeline::Transition {
        id: TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(4),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    };
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![a, b, c];
    t.transitions.push(dissolve);
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(2.0), fps: Some((25, 1)), ..Default::default() });
    SequenceSnapshot {
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

/// Every frame of the timeline as the export makes it, and the readers
/// its source opened, with the neighbour readers kept or not.
fn frames(snap: &SequenceSnapshot, kept: bool) -> (Vec<Vec<u8>>, usize) {
    let probe = tmp(if kept { "kept.mp4" } else { "per-frame.mp4" });
    let settings = ExportSettings { output_path: probe.clone(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264, audio_codec: ExportAudioCodec::Copy, ..Default::default() };
    let encoder = Encoder::open(&probe, &settings, snap).unwrap();
    let mut maker = ExportFrameMaker::new(&settings, snap, &encoder, "test");
    drop(encoder);
    let _ = std::fs::remove_file(&probe);
    let mut renderer = export_renderer(snap, "test");
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    source.set_neighbour_readers_kept(kept);
    let made = (0..FRAMES)
        .map(|f| match maker.frame(&mut renderer, &snap.video.sequence, RationalTime { value: f, rate: 25 }, &mut source).unwrap() {
            ExportFrame::Planes { planes, .. } => planes,
            ExportFrame::Linear(px) => px.iter().flat_map(|v| v.to_le_bytes()).collect(),
        })
        .collect();
    (made, source.readers_opened())
}

#[test]
fn kept_neighbour_readers_make_the_same_frames() {
    let Some(src) = source() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let snap = snapshot(&src);
    let (kept, kept_opened) = frames(&snap, true);
    let (per_frame, per_frame_opened) = frames(&snap, false);
    for (f, (a, b)) in kept.iter().zip(&per_frame).enumerate() {
        assert!(a == b, "frame {f}: {} bytes differ from a reader opened per frame", a.iter().zip(b).filter(|(x, y)| x != y).count());
    }
    eprintln!("readers opened: {kept_opened} kept, {per_frame_opened} per frame");
    // The clips' streams and one neighbour reader for the file, against
    // one neighbour reader a frame (two in the dissolve) before.
    assert!(kept_opened <= 4, "{kept_opened} readers opened with the neighbour reader kept");
    assert!(per_frame_opened >= FRAMES as usize, "{per_frame_opened}: the old way opens one a frame");
}
