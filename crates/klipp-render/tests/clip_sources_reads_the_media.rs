//! A non-pure node's source frames (M4 step 1, `ClipSources`) over the
//! export's real readers: asked in source seconds, a trimmed clip's are
//! the media's frames at those times, its handles included; out of range
//! is only past the media's end, and past a media of unknown length the
//! reader says it has none rather than give another frame.
//!
//! The source is a grey ramp, one level per frame, so each frame read
//! says which media frame it is. Generated with the ffmpeg CLI; the test
//! skips without it.

use std::collections::HashMap;
use std::path::PathBuf;

use klipp_compose::render::ClipSources;
use klipp_core::*;
use klipp_nodes::frame_context::{BoundedSources, Missing, Neighbour, SourceProvider};
use klipp_render::*;

const W: u32 = 16;
const H: u32 = 16;
const FPS: u32 = 24;
/// Frames in the media: two seconds.
const N: i64 = 48;

/// 48 frames at 24 fps, frame n a flat grey of 20 + 4n, intra-only.
///
/// Written once per process (#1700): the tests run at the same time in
/// one scratch directory, and `ffmpeg -y` truncating the file while
/// another test decodes it gave that test `Unavailable` frames.
fn ramp() -> Option<PathBuf> {
    static RAMP: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        let path = klipp_testing::scratch::scratch_file("klipp-clip-sources", "ramp.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s={W}x{H}:r={FPS}:d=2,geq=lum='20+4*N':cb=128:cr=128"))
            .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(path)
    })
    .clone()
}

fn tc(frames: i64) -> RationalTime {
    RationalTime::from_frames(frames, FPS)
}

/// A media clip of the ramp, its source from frame `from` to `to`.
fn clip(media_id: MediaId, from: i64, to: i64) -> klipp_timeline::Clip {
    klipp_timeline::Clip::media(media_id, tc(0), tc(to - from), tc(from), tc(to))
}

/// The ramp's media table, its length known or not.
fn media(path: &std::path::Path, known: bool) -> (MediaId, HashMap<MediaId, MediaFile>) {
    let id = MediaId::new();
    let file = MediaFile { path: path.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: known.then_some(N as f64 / FPS as f64), ..Default::default() };
    (id, HashMap::from([(id, file)]))
}

fn secs(frame: i64) -> f64 {
    frame as f64 / FPS as f64
}

/// The frame a read gave, by its grey: the media frame it is.
fn which(read: Neighbour<klipp_nodes::frame_context::EncodedFrame>) -> Result<i64, Missing> {
    match read {
        Neighbour::Frame(f) => Ok(f.rgba8.iter().step_by(4).map(|&v| v as i64).sum::<i64>() / (W * H) as i64),
        Neighbour::Missing(m) => Err(m),
    }
}

/// What each media frame reads as, from an untrimmed clip, by the same
/// reader: the scale `which` is read against.
fn greys(path: &std::path::Path) -> Vec<i64> {
    let (id, media) = media(path, true);
    let c = clip(id, 0, N);
    let mut src = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let mut s = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
    (0..N).map(|f| which(s.encoded(secs(f))).unwrap_or_else(|m| panic!("frame {f}: {m:?}"))).collect()
}

/// A trimmed clip's frame at its in point is the media's frame there, in
/// both contracts: the readers are asked from the media's start, not the
/// clip's.
#[test]
fn a_trimmed_clips_frames_are_the_medias_at_their_source_times() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let greys = greys(&path);
    assert!(greys.windows(2).all(|w| w[1] > w[0]), "every frame its own grey: {greys:?}");
    let (id, media) = media(&path, true);
    let c = clip(id, 24, 36);
    let mut src = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let mut s = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
    assert_eq!(which(s.encoded(secs(24))), Ok(greys[24]), "the in point is media frame 24");
    assert_eq!(which(s.encoded(secs(30))), Ok(greys[30]));
    match s.linear(secs(24)) {
        Neighbour::Frame(f) => assert_eq!((f.width, f.height, f.source_secs), (W, H, secs(24))),
        other => panic!("{other:?}"),
    }
}

/// The last frame's next neighbour is the media frame after the out point,
/// a handle frame: read, not out of range. Past the media's own end it is
/// out of range.
#[test]
fn a_handle_is_read_and_only_the_medias_end_is_out_of_range() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let greys = greys(&path);
    let (id, media) = media(&path, true);
    let c = clip(id, 24, 36);
    let mut src = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let mut clip_sources = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
    let mut s = BoundedSources::new(&mut clip_sources, W, H);
    s.for_node(klipp_nodes::NodeId::new_v4(), 8);
    assert_eq!(which(s.encoded(secs(36))), Ok(greys[36]), "the frame after the out point, a handle");
    assert_eq!(which(s.encoded(secs(23))), Ok(greys[23]), "the frame before the in point, a handle");
    assert_eq!(which(s.encoded(secs(N))), Err(Missing::OutOfRange), "past the media's end");
}

/// A media of unknown length: past its end the reader says it has no frame,
/// never the last one instead.
#[test]
fn past_a_media_of_unknown_length_there_is_no_frame() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let (id, media) = media(&path, false);
    let c = clip(id, 24, 36);
    let mut src = ExportFrameSource::new(&media, FPS as f64, WorkingColorspace::Rec709Linear);
    let mut s = ClipSources::new(&mut src, &c, WorkingColorspace::Rec709Linear);
    assert_eq!(s.bounds(), (0.0, f64::INFINITY));
    assert_eq!(which(s.encoded(secs(N + 12))), Err(Missing::Unavailable));
    assert!(matches!(s.linear(secs(N + 12)), Neighbour::Missing(Missing::Unavailable)));
}
