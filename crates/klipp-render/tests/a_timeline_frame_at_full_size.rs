//! A timeline frame is drawn from the files at the size asked (#2491):
//! `frame_at` gave the viewer's preview, decoded at the playback size,
//! where small type and fine detail are judged at the timeline's own.
//! Needs the ffmpeg CLI to make and decode the source; skips without it.

use klipp_core::*;
use klipp_render::*;
use std::path::{Path, PathBuf};

const W: u32 = 1280;
const H: u32 = 720;

fn ffmpeg(args: &[&str], out: &Path) -> bool {
    std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-y"]).args(args).arg(out).status().is_ok_and(|s| s.success())
}

fn snapshot(media: &Path) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(25), ..klipp_timeline::Clip::gap(tc(0), tc(25)) };
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut track = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    video.sequence.tracks.push(track);
    video.media.insert(id, MediaFile { path: media.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    SequenceSnapshot {
        fps: 25.0, fps_num: 25, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: 25, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886,
    }
}

#[test]
fn a_timeline_frame_is_drawn_at_full_size() {
    let dir: PathBuf = klipp_testing::scratch::scratch_dir("klipp-timeline-still");
    let media = dir.join("detail.mkv");
    // Fine detail: a one-pixel checkerboard band over a test pattern, lossless.
    if !ffmpeg(&["-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=25:d=1"), "-vf", "scale=out_color_matrix=bt709:out_range=tv", "-c:v", "ffv1", "-pix_fmt", "yuv444p", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"], &media) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let still = still::timeline_still(&snapshot(&media), 0.4, W, H).expect("a frame");
    assert_eq!((still.width, still.height, still.rgba.len()), (W, H, (W * H * 4) as usize));
    // The file's own frame, as a player shows it.
    let raw = dir.join("frame.rgba");
    assert!(ffmpeg(&["-ss", "0.4", "-i", media.to_str().unwrap(), "-frames:v", "1", "-vf", "scale=in_color_matrix=bt709:in_range=tv", "-f", "rawvideo", "-pix_fmt", "rgba"], &raw));
    let file = std::fs::read(&raw).unwrap();
    assert_eq!(file.len(), still.rgba.len());
    let diff = |a: &[u8], b: &[u8]| a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as f64 - *y as f64).abs()).sum::<f64>() / (a.len() as f64 * 0.75);
    let d = diff(&still.rgba, &file);
    assert!(d < 4.0, "the frame drawn at full size is the file's: mean difference {d:.2} levels");
    // A frame drawn small and enlarged loses the detail the full one keeps.
    let small = still::timeline_still(&snapshot(&media), 0.4, W / 4, H / 4).expect("a small frame");
    let enlarged: Vec<u8> = (0..H).flat_map(|y| (0..W).flat_map(move |x| (0..4).map(move |c| (y, x, c)))).map(|(y, x, c)| small.rgba[(((y / 4) * (W / 4) + x / 4) * 4 + c) as usize]).collect();
    assert!(diff(&enlarged, &file) > d * 2.0, "a quarter-size frame enlarged is no full-size one");
    let _ = std::fs::remove_dir_all(&dir);
}
