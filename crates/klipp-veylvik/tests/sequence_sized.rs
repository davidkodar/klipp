//! A composition made at the sequence's size has its clip's footage fitted
//! inside, as Klipp fits media: a clip shown through one that only reads
//! its footage looks as it does without one, for media smaller than the
//! frame and for a portrait clip in a landscape sequence, its sides left
//! clear. It is drawn at the size Klipp renders at, the whole frame.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Compositor, Time};

const W: u32 = 96;
const H: u32 = 54;
const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

/// Media of `size`, handed over fitted into the frame at its own shape, as
/// Klipp's sources hand media: a smooth chart, so filters' small
/// differences stay small and a misplaced picture shows.
struct Media {
    size: [u32; 2],
}
impl FrameSource for Media {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let scale = (f64::from(w) / f64::from(self.size[0])).min(f64::from(h) / f64::from(self.size[1]));
        let (fw, fh) = (((f64::from(self.size[0]) * scale).round() as u32).max(1), ((f64::from(self.size[1]) * scale).round() as u32).max(1));
        let shade = ((secs * f64::from(FPS)).floor() as f32 / 48.0).fract();
        let mut px = Vec::with_capacity((fw * fh * 4) as usize);
        for y in 0..fh {
            for x in 0..fw {
                px.extend_from_slice(&[0.2 + 0.6 * x as f32 / fw as f32, 0.2 + 0.6 * y as f32 / fh as f32, shade, 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: fw, height: fh, pts: Some(secs) })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(f64::from(FPS))
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 20.0))
    }
}

fn sequence(clip: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, src: &mut dyn FrameSource, gpu: bool, at: f64, (w, h): (u32, u32)) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(WorkingColorspace::Rec709Linear);
    r.render_inspected(seq, tc(at), w, h, src, RenderOptions::default(), Inspect::default()).0
}

/// The largest difference in what is seen (each colour times its alpha:
/// a clear pixel's colour shows nowhere), and the largest in alpha alone.
fn worst(a: &[f32], b: &[f32]) -> (f32, f32) {
    assert_eq!(a.len(), b.len());
    let seen = |p: &[f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3]];
    let (pa, pb) = (a.as_chunks::<4>().0, b.as_chunks::<4>().0);
    let all = pa.iter().zip(pb).flat_map(|(x, y)| {
        let (x, y) = (seen(x), seen(y));
        (0..3).map(move |i| (x[i] - y[i]).abs())
    }).fold(0.0, f32::max);
    let alpha = pa.iter().zip(pb).map(|(x, y)| (x[3] - y[3]).abs()).fold(0.0, f32::max);
    (all, alpha)
}

/// The columns in which what is seen differs by more than two scalers'
/// rounding.
fn columns_off(a: &[f32], b: &[f32], width: usize) -> usize {
    let off = |i: usize| (0..3).any(|c| (a[i * 4 + c] * a[i * 4 + 3] - b[i * 4 + c] * b[i * 4 + 3]).abs() > 0.01);
    (0..width).filter(|x| (0..a.len() / 4 / width).any(|y| off(y * width + x))).count()
}

#[test]
fn a_sequence_sized_composition_fits_its_footage_as_klipp_fits_media() {
    let Some(compositor) = Compositor::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let pass = CompositionId(uuid::Uuid::from_u128(11));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: i64::from(FPS), rate: 1 }, 480).unwrap();
    compositor.set_compositions([(pass, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let clip = Clip::media(MediaId(uuid::Uuid::from_u128(8)), tc(0.0), tc(4.0), tc(1.0), tc(5.0));
    let mut failed = Vec::new();
    for (name, size) in [("the frame's size", [W, H]), ("half the frame's size", [W / 2, H / 2]), ("portrait", [H, W])] {
        for (label, render_size) in [("full size", (W, H)), ("half-size preview", (W / 2, H / 2))] {
            for gpu in [true, false] {
                let plain = render(&sequence(clip.clone()), &mut Media { size }, gpu, 1.5, render_size);
                let mut composed = clip.clone();
                composed.composition = Some(pass);
                let mut media = Media { size };
                let before = compositor.composed();
                let through = render(&sequence(composed), &mut compositor.source(&mut media, WorkingColorspace::Rec709Linear), gpu, 1.5, render_size);
                if compositor.composed() == before {
                    failed.push(format!("{name}, {label}, gpu={gpu}: the media was shown, not the composition"));
                    continue;
                }
                let (all, alpha) = worst(&plain, &through);
                let edges = columns_off(&plain, &through, render_size.0 as usize);
                eprintln!("{name}, {label}, gpu={gpu}: worst {all:.4}, alpha {alpha:.4}, columns off {edges}");
                // Fitted on a whole pixel either way, so the same picture in the
                // same place: only two scalers' rounding of a smaller preview.
                if alpha > 1e-6 || edges > 0 || all > 0.01 {
                    failed.push(format!("{name}, {label}, gpu={gpu}: worst {all}, alpha {alpha}, columns off {edges}"));
                }
            }
        }
    }
    compositor.set_compositions(Compositions::new());
    assert!(failed.is_empty(), "{failed:#?}");
}
