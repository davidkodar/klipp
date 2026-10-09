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

/// The frame size the oracle draws at, unless a case asks for another:
/// large enough that every Video Transform typed value keeps the moved
/// clip in frame (positions are in the sequence's pixels).
pub(super) const SMALL: (u32, u32) = (320, 180);

/// The delivery size, for effects whose strength is scaled to the frame
/// (a gate weave of a few pixels at 1920 wide is under one at 320).
pub(super) const FULL: (u32, u32) = (1920, 1080);

/// The time every frame is drawn at: inside the oracle's clips, which
/// start at zero and run two seconds, early enough that a clip sped up
/// to twice its speed still shows.
fn at() -> Timecode {
    RationalTime::from_secs_f64(0.5, 24)
}

/// A frame of the source at `secs`: hue and brightness ramps across and
/// down, a checker, a patch brighter than white, and a fine grain that
/// changes every frame, the whole picture panning two pixels a frame. So
/// any move, scale, tilt, colour change, highlight effect or retime
/// shows, a rolling-shutter correction has motion to measure, and a
/// temporal denoise has noise to take away.
fn source(secs: f64, (w, h): (u32, u32)) -> Arc<Vec<f32>> {
    let frame = (secs * 24.0).round() as u32;
    let pan = frame * 2;
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let xs = x + pan;
            let (fx, fy) = ((xs % w) as f32 / w as f32, y as f32 / h as f32);
            let check = if (xs / 8 + y / 8).is_multiple_of(2) { 0.15 } else { 0.0 };
            let bright = (0.6..0.75).contains(&fx) && (0.2..0.4).contains(&fy);
            let lift = if bright { 1.5 } else { 0.0 };
            let hash = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ frame.wrapping_mul(83492791)) % 1000;
            let grain = (hash as f32 / 1000.0 - 0.5) * 0.06;
            v.extend_from_slice(&[fx * 0.8 + check + lift + grain, fy * 0.7 + lift + grain, (1.0 - fx) * 0.6 + check + lift + grain, 1.0]);
        }
    }
    Arc::new(v)
}

/// The synthetic source, counting what it was asked for: a frame drawn
/// without reading it isn't evidence of the clip.
struct Synthetic {
    size: (u32, u32),
    reads: u32,
}

impl FrameSource for Synthetic {
    fn frame(&mut self, _clip: &Clip, secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        self.reads += 1;
        Some(SourceFrame { pixels: SourcePixels::Linear(source(secs, self.size)), width: self.size.0, height: self.size.1, pts: None })
    }

    /// Frames at other clip times, for a temporal denoise's neighbours:
    /// the same source, linear, as the working space takes it.
    fn temporal_frames_linear(&mut self, _clip: &Clip, _working: klipp_core::WorkingColorspace) -> Option<klipp_compose::LinearFrameProviderFn> {
        let size = self.size;
        Some(Box::new(move |secs| Some(((*source(secs, size)).clone(), size.0, size.1))))
    }

    /// A frame for motion estimation: random 8x8 blocks panning six
    /// pixels a frame (as the compose tests' moving source), which the
    /// corner matcher can follow where a repeating checker is ambiguous,
    /// so a rolling-shutter correction has motion to measure.
    fn motion_frame(&mut self, _clip: &Clip, secs: f64, max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let (w, h) = (256u32, 144u32);
        if w > max_width {
            return None;
        }
        let n = (secs * 24.0).floor() as i64;
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let (bx, by) = ((x - 6 * n).div_euclid(8), y.div_euclid(8));
                let g = ((bx * 7919 + by * 104_729).rem_euclid(251)) as u8;
                v.extend_from_slice(&[g, g, g, 255]);
            }
        }
        Some((v, w, h))
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
fn sequence(clip: &Clip, (width, height): (u32, u32)) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width, height };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    seq
}

/// `clip` drawn on the CPU executor: linear RGBA floats, full, finite,
/// not flat, and read from the clip's source.
fn render_cpu(seq: &Sequence, size: (u32, u32)) -> Result<Vec<f32>, String> {
    let (w, h) = size;
    let mut src = Synthetic { size, reads: 0 };
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let (frame, _) = cpu.render_inspected(seq, at(), w, h, &mut src, RenderOptions::default(), Inspect::default());
    if src.reads == 0 {
        return Err("the CPU frame never read the clip's source".into());
    }
    if frame.len() != (w * h * 4) as usize || frame.iter().any(|v| !v.is_finite()) {
        return Err(format!("the CPU frame isn't a full, finite {w}x{h} frame ({} values)", frame.len()));
    }
    if frame.chunks_exact(4).all(|p| p == &frame[..4]) {
        return Err("the CPU frame is flat".into());
    }
    Ok(frame)
}

/// `clip` drawn on the native GPU stack: display RGBA bytes, or an error
/// beginning `BLOCKED` when the stack declines the frame or the frame
/// leaves the native route.
fn render_gpu(seq: &Sequence, size: (u32, u32)) -> Result<Vec<u8>, String> {
    let (w, h) = size;
    let mut src = Synthetic { size, reads: 0 };
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let Some((bytes, stats)) = gpu.render_display_u8_on_gpu(seq, at(), w, h, &mut src, RenderOptions::default()) else {
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
    if bytes.len() != (w * h * 4) as usize {
        return Err(format!("the GPU frame isn't a full {w}x{h} frame ({} bytes)", bytes.len()));
    }
    if bytes.chunks_exact(4).all(|p| p == &bytes[..4]) {
        return Err("the GPU frame is flat".into());
    }
    Ok(bytes)
}

/// `clip` drawn on the CPU and on the native GPU stack, or why it can't
/// be (an error beginning `BLOCKED` when the GPU route isn't available or
/// wasn't kept to).
pub(super) fn render(clip: &Clip, size: (u32, u32)) -> Result<Frames, String> {
    let seq = sequence(clip, size);
    Ok(Frames { cpu: render_cpu(&seq, size)?, gpu: render_gpu(&seq, size)? })
}

/// Which routes an edit is drawn on: both, or the CPU alone for a family
/// whose production route is the CPU by design (its GPU half is declared
/// not applicable, citing the step that moves it, never BLOCKED).
#[derive(Clone, Copy, Debug)]
pub(super) enum Routes {
    Both,
    CpuByDesign,
}

/// The CPU half of [`same_edit`]: the edit changes the CPU frame, and the
/// two views' clips draw it bit for bit alike.
fn same_on_cpu(before: &Clip, old: &Clip, new: &Clip, size: (u32, u32)) -> Result<(), String> {
    let cpu = |c: &Clip| render_cpu(&sequence(c, size), size);
    let (before, old, new) = (cpu(before)?, cpu(old)?, cpu(new)?);
    if new == before {
        return Err("the edit doesn't change the CPU frame".into());
    }
    if let Some(i) = (0..new.len()).find(|&i| new[i].to_bits() != old[i].to_bits()) {
        return Err(format!("old and generated CPU frames differ, first at value {i}: {} vs {}", old[i], new[i]));
    }
    Ok(())
}

/// Whether `a` and `b` draw different CPU frames: a fixture's positive
/// control, that the frame can show the change a case looks for at all.
pub(super) fn cpu_frames_differ(a: &Clip, b: &Clip, size: (u32, u32)) -> Result<bool, String> {
    Ok(render_cpu(&sequence(a, size), size)? != render_cpu(&sequence(b, size), size)?)
}

/// [`same_edit`] on `routes`, drawn at `size`.
pub(super) fn same_edit_on(routes: Routes, size: (u32, u32), before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    match routes {
        Routes::Both => same_edit_at(size, before, old, new),
        Routes::CpuByDesign => same_on_cpu(before, old, new, size),
    }
}

/// The Pixels bar for one edit: `old` and `new` (the clip after the edit
/// in the old view and in the generated one) draw `before`'s clip changed,
/// and the same frame as each other, on each route.
pub(super) fn same_edit(before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    same_edit_at(SMALL, before, old, new)
}

/// [`same_edit`], drawn at `size`.
fn same_edit_at(size: (u32, u32), before: &Clip, old: &Clip, new: &Clip) -> Result<(), String> {
    let (before, old, new) = (render(before, size)?, render(old, size)?, render(new, size)?);
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

/// How long the GPU stack takes over `clip`'s frame at `size`, as a median
/// of `runs`, and how many layers it drew on the CPU: a cost probe, not a
/// parity check.
pub(super) fn gpu_frame_cost(clip: &Clip, size: (u32, u32), runs: usize) -> Option<(f64, u32)> {
    let seq = sequence(clip, size);
    let mut src = Synthetic { size, reads: 0 };
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    gpu.render_display_u8_on_gpu(&seq, at(), size.0, size.1, &mut src, RenderOptions::default())?;
    let mut times = Vec::with_capacity(runs);
    let mut cpu_layers = 0;
    for _ in 0..runs {
        let t0 = std::time::Instant::now();
        let (_, stats) = gpu.render_display_u8_on_gpu(&seq, at(), size.0, size.1, &mut src, RenderOptions::default())?;
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        cpu_layers = stats.cpu_layers;
    }
    times.sort_by(f64::total_cmp);
    Some((times[times.len() / 2], cpu_layers))
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

    /// The cost of the stack's temporal gate for M4 step 9: a stabilised
    /// clip's frame on the GPU stack, without analysis and with it, against
    /// the same clip with no effect. Run by hand (`--ignored --nocapture`),
    /// in a release build for numbers that mean anything.
    #[test]
    #[ignore = "a timing probe, run by hand; not a gate"]
    fn stabilised_layer_cost() {
        use klipp_effects::rolling_shutter::Homography;
        use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, smooth_trajectory, StabilizationData};
        let plain = clip(&[]);
        let unanalysed = clip(&[E::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]);
        let mut analysed = unanalysed.clone();
        let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 6.0 } else { -4.5 }], [0.0, 1.0, 1.5], [0.0, 0.0, 1.0]] };
        let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
        let accumulated = accumulate_trajectory(&frame_transforms);
        let trajectory = accumulated.iter().map(decompose_homography).collect();
        let data = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
        analysed.smoothed_trajectory = Some(smooth_trajectory(&data, 1.0));
        analysed.stabilization_data = Some(data);
        for (name, c) in [("no effect", &plain), ("stabiliser, unanalysed", &unanalysed), ("stabiliser, analysed", &analysed)] {
            match gpu_frame_cost(c, FULL, 15) {
                Some((ms, cpu_layers)) => eprintln!("{name}: {ms:.2} ms median at 1920x1080, cpu_layers {cpu_layers}"),
                None => eprintln!("{name}: the GPU stack declined"),
            }
        }
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
