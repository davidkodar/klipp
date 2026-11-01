//! An export's frames are the shared decision's (#1156): what `ExportJob`
//! encodes is what `ExportFrameMaker` makes, so anything that renders
//! through the maker (the real-project pixel check) renders exactly what an
//! export would. Needs the ffmpeg CLI to decode; skips without it.

use klipp_core::*;
use klipp_render::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use klipp_render::*;
use klipp_timeline::GradeAccess;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: u64 = 5;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-export-frames-shared", name)
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A W×H, 25 fps timeline of one graded, moving generator clip: a frame
/// the GPU converts to the encoder's planes when the encoder takes them.
fn snapshot() -> SequenceSnapshot {
    let tc = |f| RationalTime::from_frames(f, 25);
    let gradient = GeneratorData::LivingGradient { a: [200, 40, 30, 255], b: [20, 60, 220, 255], angle_deg: 30.0, drift_deg_per_sec: 90.0, glow: 0.4, grain: klipp_core::DEFAULT_GENERATOR_GRAIN };
    let mut clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(gradient),
        source_in: tc(0),
        source_out: tc(FRAMES as i64),
        ..klipp_timeline::Clip::gap(tc(0), tc(FRAMES as i64))
    };
    clip.edit_grade(|g| {
        let c = g.ensure_corrector();
        g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.exposure = Some(0.3);
    });
    let mut v = VideoTimeline::default();
    v.sequence.fps = Fps::new(25, 1);
    v.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    v.sequence.tracks.push(t);
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: FRAMES, export_range: None, video: v,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

/// 10-bit H.264: always the software encoder (deterministic, on every
/// machine), and near-lossless.
fn settings(out: &Path) -> ExportSettings {
    ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 10, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        alpha: false,
        mastering_peak_nits: DEFAULT_MASTERING_PEAK_NITS,
    }
}

/// The ten-bit samples of a little-endian planar 4:2:0 buffer.
fn samples(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect()
}

#[test]
fn an_exports_frames_are_the_shared_decisions_frames() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let out = tmp("shared.mp4");
    let s = settings(&out);
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.clone(), settings: s.clone(), snapshot: snapshot(),
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: FRAMES,
    }
    .run()
    .unwrap();
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    let size = (W * H * 3 / 2) as usize * 2;
    assert_eq!(decoded.stdout.len(), size * FRAMES as usize, "five 10-bit 4:2:0 frames");

    // The same frames through the shared decision, set up as the job sets
    // it up: its renderer, its encoder's answer, its frame source.
    let snap = snapshot();
    let probe = tmp("probe.mp4");
    let encoder = Encoder::open(&probe, &s, &snap).unwrap();
    assert_eq!(encoder.video_format(), ffmpeg_next::format::Pixel::YUV420P10LE, "the software encoder's planar 10-bit");
    let maker = ExportFrameMaker::new(&s, &snap, &encoder, "test");
    drop(encoder);
    let _ = std::fs::remove_file(&probe);
    // Klipp requires a GPU, and the frames this holds the job to are the
    // ones the GPU converts: without one there is nothing to check.
    assert!(
        maker.converts_on_gpu(),
        "this test needs a GPU (Klipp requires one): the export of a one-clip project converts its frames on it, and none was found"
    );

    // The maker's planes at `times`, as the job makes them.
    let made = |times: std::ops::Range<u64>| -> Vec<Vec<u16>> {
        let mut maker = ExportFrameMaker::new(&s, &snap, &Encoder::open(&probe, &s, &snap).unwrap(), "test");
        let _ = std::fs::remove_file(&probe);
        let mut renderer = export_renderer(&snap, "test");
        let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
        times
            .map(|f| {
                let time = RationalTime { value: f as i64, rate: 25 };
                let ExportFrame::Planes { planes, .. } = maker.frame(&mut renderer, &snap.video.sequence, time, &mut source).unwrap() else {
                    panic!("frame {f} came back on the CPU");
                };
                assert_eq!(planes.len(), size, "frame {f}: the encoder's planes");
                samples(&planes)
            })
            .collect()
    };
    let job = |f: usize| samples(&decoded.stdout[f * size..(f + 1) * size]);
    let worst = |pairs: Vec<(Vec<u16>, Vec<u16>)>| {
        pairs.iter().flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())).max().unwrap()
    };
    // CRF 1 at ten bits: a couple of levels of codec error (2, measured).
    let same = worst(made(0..FRAMES).into_iter().enumerate().map(|(f, m)| (job(f), m)).collect());
    assert!(same <= 6, "the job's frames differ from the shared decision's by up to {same} ten-bit levels");
    // The control: the job's frames against the maker's one step later,
    // a different picture on this drifting, graded gradient (15 levels
    // off, measured), must fail the same bound, or the bound proves
    // nothing.
    let shifted = worst(made(1..FRAMES).into_iter().enumerate().map(|(f, m)| (job(f), m)).collect());
    assert!(shifted > 6, "a frame one step off in time is only {shifted} levels off: the bound wouldn't tell a different picture apart");
}

/// A frame the GPU declines after rendering it keeps the GPU way off only
/// while the same clips are on screen: the stretch asks once, not at
/// every frame, and the first frame with other clips asks again and gets
/// planes (#1276). It used to turn the GPU way off for the rest of the
/// export. A stretch declined before any work (a plug-in) isn't
/// remembered at all, as before.
///
/// Declined after the work: an adjustment layer whose grade run fails
/// (`klipp_compose::fault`), after its effect was spliced into the run, so
/// the stack refuses it part-way. The timeline (frames at 25 fps):
/// generator `a` to frame 12 then `b`, with a cross dissolve over the cut;
/// the failing layers over 3..6 and over 8..16, across the dissolve; a
/// plug-in clip over 17..19.
#[test]
fn a_frame_declined_after_its_work_turns_the_gpu_off_for_its_stretch_alone() {
    let tc = |f| RationalTime::from_frames(f, 25);
    let generator = |colour: [u8; 4], from, to| klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Generator(GeneratorData::SolidColor { color: colour }),
        source_in: tc(0),
        source_out: tc(to - from),
        ..klipp_timeline::Clip::gap(tc(from), tc(to))
    };
    let adjustment = |from, to| {
        let c = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::AdjustmentLayer, ..klipp_timeline::Clip::gap(tc(from), tc(to)) };
        klipp_timeline::test_clip::TestClip::of(c).effects([klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }]).build()
    };
    let a = generator([200, 60, 40, 255], 0, 12);
    let b = generator([30, 90, 210, 255], 12, 24);
    let (adj1, adj2) = (adjustment(3, 6), adjustment(8, 16));
    let plugin = klipp_timeline::test_clip::TestClip::of(generator([90, 200, 90, 255], 17, 19))
        .plugin(klipp_plugin::AppliedEffect { plugin_name: "com.klipp.test.Invert".into(), enabled: true, ..Default::default() })
        .build();
    let _faults = (klipp_compose::fault::fail_grade(adj1.id), klipp_compose::fault::fail_grade(adj2.id));

    let mut snap = snapshot();
    let mut v1 = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    v1.transitions.push(klipp_timeline::Transition {
        id: TransitionId::new(),
        clip_a_id: a.id,
        clip_b_id: b.id,
        kind: TransitionKind::CrossDissolve,
        duration: tc(4),
        alignment: Default::default(),
        user_params: Default::default(),
    });
    v1.clips = vec![a, b];
    let mut v2 = klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video);
    v2.clips = vec![adj1, adj2];
    let mut v3 = klipp_timeline::Track::new("V3", klipp_timeline::TrackKind::Video);
    v3.clips = vec![plugin];
    snap.video.sequence.tracks = vec![v3, v2, v1];
    snap.duration_frames = 24;
    let seq = snap.video.sequence.clone();

    let probe = tmp("declined-stretch.mp4");
    let s = settings(&probe);
    let mut maker = ExportFrameMaker::new(&s, &snap, &Encoder::open(&probe, &s, &snap).unwrap(), "test");
    let _ = std::fs::remove_file(&probe);
    assert!(maker.converts_on_gpu(), "this test needs a GPU (Klipp requires one)");
    let mut host = klipp_plugin::OfxHost::new();
    let at = host.register_raw_plugin(klipp_plugin::test_plugin::create_test_plugin_raw());
    host.initialise_plugin(at).expect("the invert plug-in loads");
    let mut renderer = klipp_render::export_frames::export_renderer_with_host(&snap, "test", host);
    let mut source = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    let on_screen = |f: i64| -> Vec<klipp_core::ClipId> {
        klipp_compose::active_layers(&seq, tc(f)).iter().flat_map(|l| l.clips.iter().map(|c| c.id)).collect()
    };

    // Each frame as the export would ask for it: whether it came back as
    // planes, whether the GPU was asked, and whether it declined after
    // its work.
    let mut frame = |f: i64| {
        let before = maker.gpu_attempts();
        let planes = matches!(maker.frame(&mut renderer, &seq, tc(f), &mut source), Ok(ExportFrame::Planes { .. }));
        (planes, maker.gpu_attempts() - before, renderer.gpu_export_declined_after_work())
    };
    for f in 0..3 {
        assert_eq!(frame(f), (true, 1, false), "frame {f}: plain, on the GPU");
    }
    // Declined after its work once; the rest of its stretch isn't asked.
    assert_eq!(frame(3), (false, 1, true), "frame 3: the failing layer, declined after its work");
    for f in 4..6 {
        let (planes, attempts, _) = frame(f);
        assert_eq!((planes, attempts), (false, 0), "frame {f}: the declined stretch, not asked again");
    }
    // Leaving it: the GPU is asked again, and takes the frames.
    for f in 6..8 {
        assert_eq!(frame(f), (true, 1, false), "frame {f}: plain again, back on the GPU");
    }
    // Coming back into it asks once more, and once only.
    assert_eq!(frame(4), (false, 1, true), "frame 4 again: the stretch re-entered asks once");
    assert_eq!(frame(5).1, 0, "frame 5 again: not asked twice");

    // Across the dissolve: every change in what is on screen (a side of
    // the transition joining or leaving) asks once, and the frames
    // between ask nothing.
    let mut last = on_screen(5);
    let mut transition = false;
    for f in 8..16 {
        let now = on_screen(f);
        transition |= klipp_compose::active_layers(&seq, tc(f)).iter().any(|l| l.transition);
        let asked = u64::from(now != last);
        let (planes, attempts, _) = frame(f);
        assert!(!planes, "frame {f}: under the failing layer");
        assert_eq!(attempts, asked, "frame {f}: asked only when its clips changed ({last:?} to {now:?})");
        last = now;
    }
    assert!(transition, "the failing layer spans the dissolve");
    assert_eq!(frame(16), (true, 1, false), "frame 16: plain again, back on the GPU");

    // The control: a plug-in clip is declined before any work, never
    // remembered, so each of its frames asks, and the next plain frame
    // is on the GPU, as before.
    for f in 17..19 {
        let (planes, attempts, after_work) = frame(f);
        assert_eq!((planes, attempts, after_work), (false, 1, false), "frame {f}: the plug-in, declined at the gate");
    }
    assert_eq!(frame(19), (true, 1, false), "frame 19: plain again, on the GPU");
}
