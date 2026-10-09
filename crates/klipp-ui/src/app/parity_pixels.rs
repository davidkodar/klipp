//! The picture oracle for the parity families' Pixels obligations (M3
//! step 18): a clip as the real renderer draws it, on the CPU and on the
//! native GPU stack, from a synthetic, non-flat source.
//!
//! A case earns Pixels only when its edit is seen: the edited clip's
//! frame differs from the unedited one on each route (an edit that draws
//! nothing proves nothing), and the old view's clip and the generated
//! view's draw the same frame on each route, to the level. The routes are
//! never compared with each other here; their own bar is the CPU/GPU
//! parity suites'.
//!
//! A GPU that is missing or declines the frame, or a frame that leaves
//! the native GPU route (a layer drawn on the CPU and uploaded, an effect
//! on the CPU kernels, a fallback list, a mask the run couldn't give), is
//! BLOCKED: an error, so the case fails and earns no key. It is never a
//! skip that passes.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

/// Large enough that every control's typed value keeps the moved clip in
/// frame: positions are in the sequence's pixels.
const W: u32 = 320;
const H: u32 = 180;

/// The time every frame is drawn at: inside the oracle's clips, which
/// start at zero and run two seconds.
fn at() -> Timecode {
    RationalTime::from_secs_f64(1.0, 24)
}

/// A frame of the source: hue and brightness ramps across and down, so
/// any move, scale, tilt or colour change shows.
fn source() -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let check = if (x / 8 + y / 8) % 2 == 0 { 0.15 } else { 0.0 };
            v.extend_from_slice(&[fx * 0.8 + check, fy * 0.7, (1.0 - fx) * 0.6 + check, 1.0]);
        }
    }
    Arc::new(v)
}

/// The synthetic source, counting what it was asked for: a frame drawn
/// without reading it isn't evidence of the clip.
struct Synthetic {
    pixels: Arc<Vec<f32>>,
    reads: u32,
}

impl FrameSource for Synthetic {
    fn frame(&mut self, _clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.reads += 1;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.pixels)), width: W.min(width), height: H.min(height), pts: None })
    }
}

/// One clip's frame on each route.
#[derive(Clone, PartialEq)]
pub(super) struct Frames {
    /// The CPU executor's frame: linear RGBA floats.
    pub cpu: Vec<f32>,
    /// The native GPU stack's display frame: RGBA bytes.
    pub gpu: Vec<u8>,
}

/// `clip` alone on a sequence of the oracle's size.
fn sequence(clip: &Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    seq
}

/// `clip` drawn on the CPU and on the native GPU stack, or why it can't
/// be (an error beginning `BLOCKED` when the GPU route isn't available or
/// wasn't kept to).
pub(super) fn render(clip: &Clip) -> Result<Frames, String> {
    let seq = sequence(clip);
    let mut src = Synthetic { pixels: source(), reads: 0 };

    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let (frame, _) = cpu.render_inspected(&seq, at(), W, H, &mut src, RenderOptions::default(), Inspect::default());
    if src.reads == 0 {
        return Err("the CPU frame never read the clip's source".into());
    }
    if frame.len() != (W * H * 4) as usize || frame.iter().any(|v| !v.is_finite()) {
        return Err(format!("the CPU frame isn't a full, finite {W}x{H} frame ({} values)", frame.len()));
    }
    if frame.chunks_exact(4).all(|p| p == &frame[..4]) {
        return Err("the CPU frame is flat".into());
    }

    src.reads = 0;
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let Some((bytes, stats)) = gpu.render_display_u8_on_gpu(&seq, at(), W, H, &mut src, RenderOptions::default()) else {
        return Err(format!("BLOCKED: the GPU stack declined the frame: {}", gpu.last_decline().unwrap_or("no reason given")));
    };
    let off_route = [
        ("layers drawn on the CPU", stats.cpu_layers),
        ("effect runs on the CPU kernels", stats.effects_on_cpu_kernels),
        ("failed plan runs", stats.effects_on_cpu),
        ("fallback effect lists", stats.effects_fallback_lists),
        ("masks unavailable", stats.effects_mask_unavailable),
    ];
    if let Some((what, n)) = off_route.iter().find(|(_, n)| *n > 0) {
        return Err(format!("BLOCKED: the GPU frame left the native route: {n} {what}"));
    }
    if src.reads == 0 {
        return Err("the GPU frame never read the clip's source".into());
    }
    if bytes.len() != (W * H * 4) as usize {
        return Err(format!("the GPU frame isn't a full {W}x{H} frame ({} bytes)", bytes.len()));
    }
    if bytes.chunks_exact(4).all(|p| p == &bytes[..4]) {
        return Err("the GPU frame is flat".into());
    }
    Ok(Frames { cpu: frame, gpu: bytes })
}

/// The Pixels bar for one edit: `old` and `new` (the clip after the edit
/// in the old view and in the generated one) draw `before`'s clip changed,
/// and the same frame as each other, on each route.
pub(super) fn same_edit(before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    let (before, old, new) = (render(before)?, render(old)?, render(new)?);
    if new.cpu == before.cpu {
        return Err("the edit doesn't change the CPU frame".into());
    }
    if new.gpu == before.gpu {
        return Err("the edit doesn't change the GPU frame".into());
    }
    if let Some(i) = (0..new.cpu.len()).find(|&i| new.cpu[i].to_bits() != old.cpu[i].to_bits()) {
        return Err(format!("old and generated CPU frames differ, first at value {i}: {} vs {}", old.cpu[i], new.cpu[i]));
    }
    if let Some(i) = (0..new.gpu.len()).find(|&i| new.gpu[i] != old.gpu[i]) {
        return Err(format!("old and generated GPU frames differ, first at byte {i}: {} vs {}", old.gpu[i], new.gpu[i]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect as E;

    fn clip(effects: &[E]) -> Clip {
        let tc = |s| RationalTime::from_secs_f64(s, 24);
        let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        for e in effects {
            c.push_effect(e.clone());
        }
        c
    }

    /// The oracle's own controls: an edit that draws nothing is refused,
    /// an edit drawn differently by the two views is refused, and the same
    /// edit passes.
    #[test]
    fn the_oracle_refuses_an_unseen_edit_and_a_different_one() {
        let before = clip(&[E::Gamma { gamma: 1.0 }]);
        let seen = clip(&[E::Gamma { gamma: 1.6 }]);
        let other = clip(&[E::Gamma { gamma: 1.3 }]);
        match same_edit(&before, &seen, &seen) {
            Err(e) if e.starts_with("BLOCKED") => panic!("{e}"),
            r => r.expect("the same seen edit passes"),
        }
        let unseen = same_edit(&before, &before, &before).unwrap_err();
        assert!(unseen.contains("doesn't change"), "{unseen}");
        let differ = same_edit(&before, &other, &seen).unwrap_err();
        assert!(differ.contains("differ"), "{differ}");
    }
}
