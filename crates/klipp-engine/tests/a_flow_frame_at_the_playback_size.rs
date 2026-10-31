//! Optical flow between source frames at the playback size (#3205): a
//! 4K timeline is drawn at the playback size while playing, and a frame
//! retimed by flow makes its in-between frame there, at a quarter of the
//! pixels, its motion searched at that size. Drawn at 1080p it is the 4K
//! frame shrunk, within a bound measured on it, and nearer that than a
//! cross-fade of the same frames is.
//!
//! Through the still renderer, whose blend is drawn with its frames in
//! hand: a parked viewer frame takes its neighbour as the stream gives
//! it, and is not always blended when it settles.

mod common;


use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 25;

/// Three seconds of 4K panning across still, blurred noise, every frame
/// a keyframe: motion optical flow follows everywhere, where a test
/// pattern's changing shapes are cross-faded as motion it can't follow.
/// `None` without ffmpeg.
fn pan() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-uhd-noise-pan-intra-3s.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "nullsrc=s=4800x2700:d=1,geq=lum='random(1)*255':cb=128:cr=128,gblur=sigma=3"])
        .args(["-vf", &format!("loop=-1:1:0,fps={FPS},crop={W}:{H}:x='40+t*90':y='30+t*40',trim=duration=3")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "1", "-pix_fmt", "yuv420p"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// `full` (RGBA, `w` by `h`) shrunk by two each way, a box of four.
fn halve(full: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((hw * hh * 4) as usize);
    for y in 0..hh {
        for x in 0..hw {
            for c in 0..4 {
                let at = |dx: u32, dy: u32| u32::from(full[(((2 * y + dy) * w + 2 * x + dx) * 4 + c) as usize]);
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    out
}

/// Mean difference over red, green and blue, in levels.
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (sum, n) = a.chunks_exact(4).zip(b.chunks_exact(4)).fold((0u64, 0u64), |(s, n), (p, q)| (s + (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>(), n + 3));
    sum as f64 / n.max(1) as f64
}

/// A 4K clip slowed to 0.3, forward and reversed, between two of its
/// frames and retimed by optical flow, drawn at 1080p and at 4K.
#[test]
fn a_flow_frame_at_the_playback_size_is_the_full_one_shrunk() {
    let Some(path) = pan() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(FPS, 1);
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: path.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps,
        ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(3.0), fps)
    });
    // At 0.3 over ten seconds: at 1 s the source is halfway between two
    // of its frames, at 0.3 s forward, at 2.7 s reversed.
    let timeline = |speed: f64, blend_mode: u32| {
        let mut slow = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(10.0)) };
        slow.speed = speed;
        let slow = TestClip::of(slow).effect(BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode }).build();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![slow];
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: W, height: H };
        seq.tracks = vec![v1];
        seq
    };
    // blend_mode 2 is optical flow, 1 a cross-fade.
    let still = |seq: &Sequence, w: u32, h: u32| {
        let snapshot = klipp_render::SequenceSnapshot::from_sequence_with_all(seq, std::slice::from_ref(seq), &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).expect("snapshot");
        klipp_render::still::timeline_still(&snapshot, 1.0, w, h).expect("a frame").rgba
    };
    // Reversed, flow at 1080p keeps less of its lead over a cross-fade
    // (measured 1.03 levels off against 1.66) than forward (0.34 against
    // 2.95): each is held to its own.
    for (speed, share) in [(0.3, 0.3), (-0.3, 0.75)] {
        let (flow_4k, faded_4k, shown) = (still(&timeline(speed, 2), W, H), still(&timeline(speed, 1), W, H), still(&timeline(speed, 2), 1920, 1080));
        let (flow_full, faded_full) = (halve(&flow_4k, W, H), halve(&faded_4k, W, H));
        let off = mean_diff(&shown, &flow_full);
        let fade_off = mean_diff(&faded_full, &flow_full);
        eprintln!("at {speed}: flow at 1080p is {off:.2} levels from flow at 4K shrunk; a cross-fade at 4K is {fade_off:.2}");
        assert!(fade_off > 1.0, "at {speed}, flow and a cross-fade of these frames differ by only {fade_off:.2} levels: the frame tests nothing");
        assert!(off < 1.5 && off < share * fade_off, "at {speed}, flow at 1080p is {off:.2} levels off, a cross-fade {fade_off:.2}");
    }
}
