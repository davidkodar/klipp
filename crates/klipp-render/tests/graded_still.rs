//! A clip's graded still (#899, `still::clip_still`): its frame at a time
//! of its own, read from its file and drawn with its grade, small. The
//! source is a grey ramp, one level per frame (written with the ffmpeg
//! CLI; the test skips without it): a later time is a brighter frame, and
//! a grade that brightens makes the same frame brighter.

use klipp_core::*;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_timeline::GradeAccess;

const FPS: u32 = 24;

/// Two seconds at 24 fps of a 64x36 grey, frame n at 20 + 4n.
fn ramp() -> Option<std::path::PathBuf> {
    static RAMP: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        let path = klipp_testing::scratch::scratch_file("klipp-graded-still", "ramp.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s=64x36:r={FPS}:d=2,geq=lum='20+4*N':cb=128:cr=128"))
            .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(path)
    })
    .clone()
}

fn mean(still: &klipp_render::still::Still) -> f64 {
    still.rgba.chunks_exact(4).map(|p| p[1] as f64).sum::<f64>() / (still.width * still.height) as f64
}

#[test]
fn a_clips_still_is_its_frame_with_its_grade() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    item.resolution = Resolution { width: 64, height: 36 };
    item.fps = Fps::new(FPS, 1);
    item.duration = RationalTime::from_frames(48, FPS);
    let id = item.id;
    let mut pool = klipp_media::MediaPool::new();
    pool.add(item);
    let tc = |f| RationalTime::from_frames(f, FPS);
    // A trimmed clip, placed late on the timeline: its own time is read.
    let plain = klipp_timeline::Clip::media(id, tc(100), tc(140), tc(4), tc(44));
    let still = |clip: &klipp_timeline::Clip, at: f64| klipp_render::still::clip_still(clip, &pool, at, 32, WorkingColorspace::Rec709Linear).expect("a still");

    let early = still(&plain, 0.0);
    assert_eq!((early.width, early.height), (32, 18), "the media's aspect, 32 wide");
    let late = still(&plain, 1.5);
    assert!(mean(&late) > mean(&early) + 20.0, "a later frame of the ramp is brighter: {} then {}", mean(&early), mean(&late));

    let mut graded = plain.clone();
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corrector && n.kind == NodeKind::Corrector).unwrap().params.exposure = Some(1.0);
    graded.set_grade(g);
    assert!(mean(&still(&graded, 0.0)) > mean(&early) + 10.0, "a stop up brightens the still: {} against {}", mean(&still(&graded, 0.0)), mean(&early));

    let title = klipp_timeline::Clip::gap(tc(0), tc(10));
    assert!(klipp_render::still::clip_still(&title, &pool, 0.0, 32, WorkingColorspace::Rec709Linear).is_err(), "only a media clip has a still");
}
