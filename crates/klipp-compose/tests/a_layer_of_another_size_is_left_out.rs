//! A layer of another size than the frame is left out of it, and said
//! so, never laid. Under a reflection it was moved in as the composite,
//! the reflection's split went past its end ("mid > len"), and in the
//! app the engine thread died on a paused regrade; laid otherwise, it
//! zipped short and drew part of the frame wrong.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 20;

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

/// The clip `short` hands back a quarter of the picture it says it is;
/// every other clip a whole grey one.
struct Source {
    short: ClipId,
}

impl FrameSource for Source {
    fn frame(&mut self, clip: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let n = if clip.id == self.short { (width * height / 4) as usize } else { (width * height) as usize };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([0.4f32, 0.4, 0.4, 1.0].repeat(n))), width, height, pts: None })
    }
}

fn media() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(4.0)) }
}

fn reflection() -> BuiltinEffect {
    BuiltinEffect::Reflection {
        waterline: 0.5, gap: 0.0, ripple_amount: 0.0, ripple_size: 60.0, ripple_speed: 0.0, breakup: 0.0, smear: 0.0, fade: 0.0, opacity: 1.0, darken: 0.0, contact: 0.0,
        blend: 0, tint: [0, 0, 0], tint_amount: 0.0, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 0.0, ripple_from: 0, seed: 0, time: 0.0,
    }
}

#[test]
fn a_short_layer_under_a_reflection_is_left_out_not_split() {
    let top = klipp_timeline::test_clip::TestClip::of(media()).effects([reflection()]).build();
    let below = media();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = [("V2", &top), ("V1", &below)]
        .map(|(name, clip)| {
            let mut t = Track::new(name, TrackKind::Video);
            t.clips = vec![clip.clone()];
            t
        })
        .into();
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (img, found) = r.render_inspected(&seq, tc(1.0), W, H, &mut Source { short: below.id }, RenderOptions::default(), Inspect::default());
    assert_eq!(img.len(), (W * H * 4) as usize, "the whole frame");
    assert_eq!(found.wrong_size_layers, 1, "the short layer is left out, and counted");
    // The reflected clip is laid on its own: its grey above the line.
    let i = (((3 * W) + W / 2) * 4) as usize;
    assert!((img[i] - 0.4).abs() < 0.05, "the reflected clip drawn: {:?}", &img[i..i + 4]);
}
