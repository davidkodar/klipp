//! #3055: a 4.0 s clip stretched by its end to 8.0 s plays the same
//! frames at 50%. At the start, middle and end of the stretched clip the
//! file is read at half the time, and the picture drawn is the one the
//! clip showed there before the stretch, on the CPU and on the GPU.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{Fps, RationalTime};
use klipp_document::apply::DocumentView;
use klipp_document::ops::{stretch_at, Edge, StretchTo};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

const W: u32 = 64;
const H: u32 = 36;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

/// A picture that tells its source second apart (a ramp whose level is
/// the second), recording each second asked for.
struct Timed(Vec<f64>);

impl FrameSource for Timed {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        self.0.push(secs);
        let level = (secs / 4.0) as f32;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[level, x as f32 / w as f32, y as f32 / h as f32, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn sequence() -> (Sequence, MediaPool, klipp_core::ClipId) {
    let mut pool = MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/stretch-frames.mov".into(), None);
    item.duration = tc(10.0);
    let media = pool.add(item);
    let clip = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![clip];
    seq.tracks = vec![v];
    (seq, pool, id)
}

/// The frame at `at` on the CPU route, and the seconds of the file read.
fn cpu(seq: &Sequence, at: f64) -> (Vec<f32>, Vec<f64>) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let mut src = Timed(Vec::new());
    let (px, _) = r.render_inspected(seq, tc(at), W, H, &mut src, RenderOptions::default(), Inspect::default());
    (px, src.0)
}

/// The frame at `at` drawn by the GPU stack, and the seconds read; `None`
/// with no GPU stack.
fn gpu(seq: &Sequence, at: f64) -> Option<(Vec<u8>, Vec<f64>)> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let mut src = Timed(Vec::new());
    let (px, _) = r.render_display_u8_on_gpu(seq, tc(at), W, H, &mut src, RenderOptions::default())?;
    Some((px, src.0))
}

#[test]
fn a_clip_stretched_to_twice_its_length_shows_the_same_frames_at_half_speed() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let (before, mut pool, id) = sequence();
    let (_, edit, _) = stretch_at(&before, &pool, &[id], Edge::Out, StretchTo::EdgeAt(8.0), false, true).expect("the stretch");
    let mut fps = before.fps;
    let mut seqs = vec![before.clone()];
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&edit);
    let after = seqs.remove(0);
    assert!((after.tracks[0].clips[0].timeline_out.to_secs_f64() - 8.0).abs() < 1e-9, "8 s long");

    // The very last frame reads halfway through the file's last one.
    let (_, read) = cpu(&after, 7.96);
    assert!(read.iter().all(|s| (s - 3.98).abs() < 1e-6), "the last frame read {read:?}");
    // Start, middle and end (the frame showing the clip's last frame):
    // the file at half the time, the picture the clip showed there before.
    for (at, file) in [(0.0, 0.0), (4.0, 2.0), (7.92, 3.96)] {
        let (stretched, read) = cpu(&after, at);
        assert!(!read.is_empty() && read.iter().all(|s| (s - file).abs() < 1e-6), "CPU at {at} s read {read:?}, want {file} s");
        let (original, _) = cpu(&before, file);
        let diff = stretched.iter().zip(&original).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(diff < 1e-5, "CPU at {at} s differs from the clip at {file} s by {diff}");
        match (gpu(&after, at), gpu(&before, file)) {
            (Some((stretched, read)), Some((original, _))) => {
                assert!(!read.is_empty() && read.iter().all(|s| (s - file).abs() < 1e-6), "GPU at {at} s read {read:?}, want {file} s");
                let diff = stretched.iter().zip(&original).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                assert!(diff <= 1, "GPU at {at} s differs from the clip at {file} s by {diff}");
            }
            _ => {
                assert!(!required, "required, but the GPU stack declined the frame");
                eprintln!("UNVERIFIED: no GPU stack");
            }
        }
    }
}
