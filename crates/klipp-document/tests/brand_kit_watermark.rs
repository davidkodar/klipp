//! #2722: a brand kit's watermark, measured where it lands in a frame the
//! GPU draws. The kit's ops put the logo (a solid red 2:1 picture) on a
//! track of its own over the picture, a fifth of the frame's width wide,
//! 3% of the width in from the bottom right (and a tall one top left);
//! the red in the frame is where the pose says, to within two pixels, in
//! its own shape.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_document::apply::DocumentView;
use klipp_document::brand_kit_ops::Watermark;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

/// Grey for the picture, solid red for the logo.
struct Source {
    logo: MediaId,
    size: (u32, u32),
}

impl FrameSource for Source {
    fn frame(&mut self, clip: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let (w, h, c) = if clip.media_id() == Some(self.logo) { (self.size.0, self.size.1, [1.0, 0.0, 0.0, 1.0]) } else { (width, height, [0.2, 0.2, 0.2, 1.0]) };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(c.repeat((w * h) as usize))), width: w, height: h, pts: None })
    }
}

#[test]
fn a_watermark_lands_in_its_corner_on_the_gpu() {
    // A wide logo bottom right, a tall one top left.
    measure(Watermark { left: false, top: false, margin: 0.03, scale: 0.2, opacity: 1.0 }, (200, 100));
    measure(Watermark { left: true, top: true, margin: 0.05, scale: 0.08, opacity: 1.0 }, (100, 200));
}

fn measure(mark: Watermark, logo_size: (u32, u32)) {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let mut pool = MediaPool::new();
    let mut base = klipp_media::MediaItem::synthetic_for_test("/tmp/brand-base.mov".into(), None);
    base.duration = tc(4.0);
    let base = pool.add(base);
    let mut logo = klipp_media::MediaItem::synthetic_for_test("/tmp/brand-logo.png".into(), None);
    logo.is_still = true;
    logo.has_audio = false;
    logo.resolution = klipp_core::Resolution { width: logo_size.0, height: logo_size.1 };
    let logo = pool.add(logo);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::media(base, tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    seq.tracks = vec![v];

    let ops = mark.ops(logo, logo_size, (W, H), 4.0).unwrap();
    let plan = klipp_document::ops::resolve(&seq, &pool, &ops).map_err(|r| r.why).expect("the ops resolve");
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    let seq = sequences.remove(0);
    assert_eq!(seq.tracks.len(), 2, "a track of its own");
    assert_eq!(seq.tracks[0].clips[0].media_id(), Some(logo), "on top");
    assert!((seq.tracks[0].clips[0].timeline_out.to_secs_f64() - 4.0).abs() < 0.05, "for the whole piece");

    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((px, _)) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Source { logo, size: logo_size }, RenderOptions::default()) else {
        assert!(!required, "required, but the GPU stack declined the frame");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let red = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        px[i] > 200 && px[i + 1] < 60 && px[i + 2] < 60
    };
    let (mut x0, mut y0, mut x1, mut y1) = (W, H, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if red(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    assert!(x1 > x0 && y1 > y0, "the logo is drawn");
    let (cx, cy, w, h) = mark.pose(logo_size, (W, H));
    let want = ((cx - w / 2.0) * W as f32, (cy - h / 2.0) * H as f32, (cx + w / 2.0) * W as f32, (cy + h / 2.0) * H as f32);
    eprintln!("red {x0},{y0} to {x1},{y1}; the pose {want:?}");
    for (got, want) in [(x0 as f32, want.0), (y0 as f32, want.1), (x1 as f32 + 1.0, want.2), (y1 as f32 + 1.0, want.3)] {
        assert!((got - want).abs() <= 2.0, "edge at {got}, the pose puts it at {want}");
    }
    let margin = W as f32 * mark.margin;
    let (gap_x, gap_y) = if mark.left { (x0 as f32, y0 as f32) } else { ((W - 1 - x1) as f32, (H - 1 - y1) as f32) };
    assert!((gap_x - margin).abs() <= 2.0 && (gap_y - margin).abs() <= 2.0, "the margin in from both edges of its corner: {gap_x}, {gap_y}");
    assert!((((x1 - x0 + 1) as f32) / ((y1 - y0 + 1) as f32) - logo_size.0 as f32 / logo_size.1 as f32).abs() < 0.06, "its own shape");
}
