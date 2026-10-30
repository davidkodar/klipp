//! An export's temporal noise reduction draws on the GPU chain (#1988),
//! within a level of the CPU route's frames, from a software-decoded
//! source (FFV1: its neighbours uploaded) and a hardware-decoded one
//! (H.264: its neighbours the decoder's own GPU frames, bound as they
//! are, nothing uploaded). Needs the ffmpeg CLI and a GPU; without the
//! hardware decoder the H.264 half says it is unverified.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};

const W: u32 = 320;
const H: u32 = 180;

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-tnr-on-the-chain", name)
}

/// A second of a moving pattern with grain on it, in `codec`.
fn source(name: &str, codec: &[&str]) -> Option<PathBuf> {
    let path = tmp(name);
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:r=25:d=1,noise=alls=10:allf=t"])
        .args(["-vf", "format=yuv420p"])
        .args(codec)
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

fn snapshot(src: &Path) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let clip = klipp_timeline::test_clip::TestClip::of(klipp_timeline::Clip::media(id, tc(0), tc(20), tc(2), tc(22)))
        .effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.3 })
        .build();
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![clip];
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(1.0), fps: Some((25, 1)), ..Default::default() });
    SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: 20, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

fn srgb8(v: f32) -> i32 {
    (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as i32
}

/// Frames 3 to 9 on one route, and per frame the denoise nodes on the
/// chain and the neighbours uploaded.
fn frames(snap: &SequenceSnapshot, gpu: bool) -> Vec<(Vec<f32>, u32, u32)> {
    let mut r = klipp_render::pixel_check::default_renderer(snap, gpu).unwrap();
    let mut src = ExportFrameSource::new(&snap.video.media, snap.fps, snap.working_colorspace);
    (3..10)
        .map(|f| {
            let img = r.render(&snap.video.sequence, RationalTime::from_frames(f, 25), W, H, &mut src, klipp_compose::RenderOptions::default());
            let ex = r.executor();
            (img, ex.last_denoise_on_chain(), ex.last_neighbour_uploads())
        })
        .collect()
}

#[test]
fn an_exports_temporal_denoise_draws_on_the_chain_as_the_cpu_draws_it() {
    let mut probe = klipp_nodes::GraphExecutor::new();
    if probe.gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    for (name, codec, hardware) in [("ffv1.mkv", &["-c:v", "ffv1"][..], false), ("h264.mp4", &["-c:v", "libx264", "-crf", "16", "-g", "5"][..], true)] {
        let Some(src) = source(name, codec) else {
            eprintln!("skipping {name}: ffmpeg could not write it");
            continue;
        };
        let snap = snapshot(&src);
        let (cpu, gpu) = (frames(&snap, false), frames(&snap, true));
        // The hardware decoder's frames are on the GPU: none of them is
        // uploaded. Without it every neighbour is.
        let decoded_on_gpu = RenderDecoder::open(&src, 0.0, 1.0, W, H, 25.0, 0).is_ok_and(|d| d.hw_accel_name() != "Software");
        if hardware && !decoded_on_gpu {
            eprintln!("UNVERIFIED {name}: not hardware-decoded here");
        }
        let mut worst = 0;
        for (f, ((a, ..), (b, on_chain, uploads))) in cpu.iter().zip(&gpu).enumerate() {
            assert_eq!(*on_chain, 1, "{name} frame {f}: the denoise drew on the chain");
            match (hardware && decoded_on_gpu, f) {
                // The first frame's neighbour before was read before the
                // stream kept the frame before it shows (#1988 phase 1).
                (true, 0) => assert!(*uploads <= 1, "{name} frame 0: {uploads} uploaded"),
                (true, _) => assert_eq!(*uploads, 0, "{name} frame {f}: the decoder's GPU frames bound as they are"),
                (false, _) => assert_eq!(*uploads, 2, "{name} frame {f}: a software frame's neighbours uploaded"),
            }
            assert_eq!(a.len(), b.len());
            let off = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(p, q)| (0..3).map(|c| (srgb8(p[c]) - srgb8(q[c])).abs()).max().unwrap()).max().unwrap();
            worst = worst.max(off);
            assert!(off <= 1, "{name} frame {f}: the chain's frame is {off} levels off the CPU route's");
        }
        eprintln!("{name}: chain against CPU, worst {worst} levels");
    }
}
