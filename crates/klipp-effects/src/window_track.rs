//! The window tracker (#238): follows what is under a grade window from
//! one frame to the next and says how it has moved, grown and turned, so
//! the window can follow a face that comes closer or a sign the camera
//! circles, not only one that crosses the frame.
//!
//! The window's whole region is aligned frame to frame by the planar
//! tracker ([`crate::planar`], #2754), its map held to a move, size and
//! turn: a plain region is held by its outline, and what passes in front
//! is weighed down.
//!
//! For a window on a flat surface seen from an angle — a screen, a sign,
//! a wall the camera walks past — [`track_window_perspective`] gives the
//! map itself, so the window's four corners each go their own way as the
//! surface turns away.
//!
//! Everything here is in the pixels of the frames given, grey, one byte
//! a pixel; the caller decodes them and maps the result back to the
//! window.

use crate::gpu::planar::PlanarGpu;
use crate::homography::Homography;
use crate::planar::{Model, Options, State};

/// A grey frame, one byte a pixel, row by row.
#[derive(Debug, Clone)]
pub struct GrayFrame {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// How the window has moved since the reference frame: its pivot moved
/// by `dx`, `dy` pixels, and it scaled by `scale` and turned `rotation`
/// radians (clockwise on a picture whose y runs down) about the pivot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFit {
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub rotation: f64,
}

impl WindowFit {
    pub const STILL: WindowFit = WindowFit { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0 };
}

/// What to solve.
#[derive(Debug, Clone, Copy)]
pub struct TrackOptions {
    /// Solve for size: a subject that comes closer or goes away.
    pub scale: bool,
    /// Solve for a turn.
    pub rotation: bool,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self { scale: true, rotation: true }
    }
}

/// p' = [a -b; b a] p + (tx, ty): a similarity in the frame's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sim {
    a: f64,
    b: f64,
    tx: f64,
    ty: f64,
}

impl Sim {
    fn map(&self, p: (f64, f64)) -> (f64, f64) {
        (self.a * p.0 - self.b * p.1 + self.tx, self.b * p.0 + self.a * p.1 + self.ty)
    }

    fn about(&self, pivot: (f64, f64)) -> WindowFit {
        let moved = self.map(pivot);
        WindowFit { dx: moved.0 - pivot.0, dy: moved.1 - pivot.1, scale: self.a.hypot(self.b), rotation: self.b.atan2(self.a) }
    }
}

/// The similarity that best takes `src` to `dst` in the least-squares
/// sense, held to no scale or no turn as `opts` asks.
fn fit(src: &[(f64, f64)], dst: &[(f64, f64)], opts: &TrackOptions) -> Option<Sim> {
    let n = src.len();
    if n == 0 || n != dst.len() {
        return None;
    }
    let nf = n as f64;
    let ms = (src.iter().map(|p| p.0).sum::<f64>() / nf, src.iter().map(|p| p.1).sum::<f64>() / nf);
    let md = (dst.iter().map(|p| p.0).sum::<f64>() / nf, dst.iter().map(|p| p.1).sum::<f64>() / nf);
    let (mut dot, mut cross, mut var) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy) = (s.0 - ms.0, s.1 - ms.1);
        let (dx, dy) = (d.0 - md.0, d.1 - md.1);
        dot += sx * dx + sy * dy;
        cross += sx * dy - sy * dx;
        var += sx * sx + sy * sy;
    }
    let (a, b) = match (opts.scale && n >= 2 && var > 1e-9, opts.rotation && n >= 2) {
        (true, true) => (dot / var, cross / var),
        (true, false) => (dot / var, 0.0),
        (false, true) => {
            let th = cross.atan2(dot);
            (th.cos(), th.sin())
        }
        (false, false) => (1.0, 0.0),
    };
    if !(a.is_finite() && b.is_finite()) || a.hypot(b) < 0.2 || a.hypot(b) > 5.0 {
        return None;
    }
    Some(Sim { a, b, tx: md.0 - (a * ms.0 - b * ms.1), ty: md.1 - (b * ms.0 + a * ms.1) })
}

/// The window's region followed by the planar tracker, its map held to
/// `model`, on `gpu` when there is one: the map from the reference
/// frame's pixels to each frame's, up to the first frame where it is
/// lost — a track stops there rather than guessing.
fn follow(
    model: Model,
    gpu: Option<&mut PlanarGpu>,
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<(usize, Homography)> {
    let opts = Options { lost_after: 1, reacquire_for: 0, model, ..Options::default() };
    let tracked = match gpu {
        Some(g) => g.track(reference, quad, &[], frames, next, &opts, progress),
        None => crate::planar::track(reference, quad, &[], frames, next, &opts, progress),
    };
    tracked.into_iter().take_while(|t| t.state != State::Lost).map(|t| (t.frame, t.h)).collect()
}

/// Follow the window `quad` (its four corners on the reference frame, in
/// pixels) from `reference` through `frames`, in the order given — the
/// frames after it for a forward track, before it, nearest first, for a
/// backward one — on `gpu` when there is one. `next` gives each frame;
/// `progress` hears the share done and stops the track by returning
/// false.
///
/// Returns the window's motion on each frame followed, about `pivot`,
/// from the reference frame: the move, size and turn (as `opts` allows)
/// that best carry its corners where the surface under them went. It
/// stops at the first frame where the window is lost, rather than
/// guessing.
#[allow(clippy::too_many_arguments)]
pub fn track_window(
    gpu: Option<&mut PlanarGpu>,
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    pivot: (f64, f64),
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &TrackOptions,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<(usize, WindowFit)> {
    let mut out = Vec::new();
    for (f, h) in follow(Model::Similarity, gpu, reference, quad, frames, next, progress) {
        let to = quad.map(|c| h.transform(c.0, c.1));
        let Some(sim) = fit(&quad, &to, opts) else { break };
        out.push((f, sim.about(pivot)));
    }
    out
}

/// [`track_window`] in perspective: the window on a flat surface whose
/// corners each go their own way as it turns away from the camera.
///
/// Returns, for each frame followed, the map from the reference frame's
/// pixels to that frame's: where any point of the window went.
pub fn track_window_perspective(
    gpu: Option<&mut PlanarGpu>,
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<(usize, Homography)> {
    follow(Model::Homography, gpu, reference, quad, frames, next, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with texture everywhere: smoothed noise, the same every
    /// run, sampled anywhere by a smooth function so a warped frame is
    /// exact.
    fn texture(x: f64, y: f64) -> f64 {
        let mut v = 0.0;
        for (i, (fx, fy, ph)) in [(0.21, 0.13, 0.3), (0.07, 0.31, 1.7), (0.43, 0.05, 2.9), (0.11, 0.17, 4.1), (0.33, 0.29, 0.8), (0.05, 0.09, 3.3)].iter().enumerate() {
            v += ((x * fx + y * fy + ph) * (1.0 + i as f64 * 0.37)).sin();
        }
        128.0 + 20.0 * v
    }

    /// The frame where the content at `p` on the reference has gone to
    /// `motion` about `pivot`.
    fn frame(w: usize, h: usize, motion: WindowFit, pivot: (f64, f64)) -> GrayFrame {
        let (s, c) = motion.rotation.sin_cos();
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                // Back from this pixel to where it came from.
                let (ux, uy) = (x as f64 - pivot.0 - motion.dx, y as f64 - pivot.1 - motion.dy);
                let (rx, ry) = ((c * ux + s * uy) / motion.scale, (-s * ux + c * uy) / motion.scale);
                data[y * w + x] = texture(rx + pivot.0, ry + pivot.1).clamp(0.0, 255.0) as u8;
            }
        }
        GrayFrame { data, width: w, height: h }
    }

    /// #238: a window over a subject that comes closer, turns and moves is
    /// followed in all three, frame after frame, to within a fraction of
    /// a pixel and a degree, with no drift building up.
    #[test]
    fn a_window_follows_a_subject_that_grows_turns_and_moves() {
        let (w, h) = (240, 160);
        let pivot = (120.0, 80.0);
        let quad = [(90.0, 55.0), (150.0, 55.0), (150.0, 105.0), (90.0, 105.0)];
        let motion = |k: usize| WindowFit { dx: 1.5 * k as f64, dy: -0.8 * k as f64, scale: 1.0 + 0.012 * k as f64, rotation: (0.9 * k as f64).to_radians() };
        let reference = frame(w, h, WindowFit::STILL, pivot);
        // Thirty frames: the subject a third bigger by the end, far from
        // how it looked on the reference.
        let frames: Vec<usize> = (1..=30).collect();
        let got = track_window(None, &reference, quad, pivot, &frames, &mut |k| Some(frame(w, h, motion(k), pivot)), &TrackOptions::default(), &|_| true);
        assert_eq!(got.len(), 30, "every frame followed");
        for (k, fit) in &got {
            let want = motion(*k);
            assert!((fit.dx - want.dx).abs() < 0.6 && (fit.dy - want.dy).abs() < 0.6, "frame {k}: moved {:.2},{:.2}, wanted {:.2},{:.2}", fit.dx, fit.dy, want.dx, want.dy);
            assert!((fit.scale / want.scale - 1.0).abs() < 0.01, "frame {k}: scale {:.4}, wanted {:.4}", fit.scale, want.scale);
            assert!((fit.rotation - want.rotation).to_degrees().abs() < 0.6, "frame {k}: turned {:.2}°, wanted {:.2}°", fit.rotation.to_degrees(), want.rotation.to_degrees());
        }
    }

    /// Held to moving alone, a turning subject's window keeps its size
    /// and angle.
    #[test]
    fn a_window_held_to_moving_does_not_scale_or_turn() {
        let (w, h) = (200, 140);
        let pivot = (100.0, 70.0);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = frame(w, h, WindowFit::STILL, pivot);
        let opts = TrackOptions { scale: false, rotation: false };
        let got = track_window(None, &reference, quad, pivot, &[1, 2, 3], &mut |k| Some(frame(w, h, WindowFit { dx: 2.0 * k as f64, dy: 1.0 * k as f64, scale: 1.0, rotation: 0.0 }, pivot)), &opts, &|_| true);
        assert_eq!(got.len(), 3);
        let last = got[2].1;
        assert!(last.scale == 1.0 && last.rotation == 0.0);
        assert!((last.dx - 6.0).abs() < 0.5 && (last.dy - 3.0).abs() < 0.5, "{last:?}");
    }

    /// A frame where the window's content is gone stops the track there,
    /// rather than inventing a place for it.
    #[test]
    fn a_lost_window_stops_rather_than_guesses() {
        let (w, h) = (200, 140);
        let pivot = (100.0, 70.0);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = frame(w, h, WindowFit::STILL, pivot);
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let got = track_window(None, &reference, quad, pivot, &[1, 2, 3], &mut |k| Some(if k < 2 { frame(w, h, WindowFit { dx: 1.0, ..WindowFit::STILL }, pivot) } else { flat.clone() }), &TrackOptions::default(), &|_| true);
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1], "followed to the frame before it was lost");
    }

    /// #2754: a window over a plain card — nothing inside it to match
    /// a point on — is held by the card's outline, every frame, as the
    /// card moves and grows over a background that stays.
    #[test]
    fn a_window_over_a_plain_card_follows_its_outline() {
        let (w, h) = (320, 240);
        let pivot = (160.0, 120.0);
        let quad = [(110.0, 85.0), (210.0, 85.0), (210.0, 155.0), (110.0, 155.0)];
        let card = |k: usize| {
            let (cx, cy, s) = (160.0 + 1.2 * k as f64, 120.0 - 0.7 * k as f64, 1.0 + 0.01 * k as f64);
            let data = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f64, (i / w) as f64);
                    // The card's edges drawn by how much of each pixel it covers.
                    let cover = |d: f64, half: f64| (half * s - d.abs() + 0.5).clamp(0.0, 1.0);
                    let c = cover(x - cx, 50.0) * cover(y - cy, 35.0);
                    let back = 70.0 + 10.0 * ((x * 0.02).sin() + (y * 0.03).cos());
                    (back + (200.0 - back) * c) as u8
                })
                .collect();
            GrayFrame { data, width: w, height: h }
        };
        let frames: Vec<usize> = (1..=20).collect();
        let got = track_window(None, &card(0), quad, pivot, &frames, &mut |k| Some(card(k)), &TrackOptions::default(), &|_| true);
        assert_eq!(got.len(), 20, "every frame followed");
        for (k, fit) in &got {
            let (dx, dy, s) = (1.2 * *k as f64, -0.7 * *k as f64, 1.0 + 0.01 * *k as f64);
            assert!((fit.dx - dx).hypot(fit.dy - dy) < 0.6, "frame {k}: moved {:.2},{:.2}, wanted {dx:.2},{dy:.2}", fit.dx, fit.dy);
            assert!((fit.scale / s - 1.0).abs() < 0.025, "frame {k}: scale {:.4}, wanted {s:.4}", fit.scale);
        }
    }

    /// The frame where the content at a reference point `p` has gone to
    /// `h`: each pixel sampled back through its inverse.
    fn warped(w: usize, h: usize, map: &Homography) -> GrayFrame {
        let back = map.inverse().unwrap();
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (rx, ry) = back.transform(x as f64, y as f64);
                data[y * w + x] = texture(rx, ry).clamp(0.0, 255.0) as u8;
            }
        }
        GrayFrame { data, width: w, height: h }
    }

    /// #238: a window on a surface that turns away from the camera — its
    /// near side growing, its far side shrinking, the whole drifting —
    /// is followed in perspective: each corner lands where the surface
    /// took it, to a fraction of a pixel.
    #[test]
    fn a_window_in_perspective_follows_each_corner() {
        let (w, h) = (240, 160);
        let quad = [(80.0, 50.0), (160.0, 50.0), (160.0, 110.0), (80.0, 110.0)];
        // Each corner on its own course, per frame.
        let course = [(0.6, -0.5), (0.2, 0.9), (0.1, -0.3), (0.7, 0.5)];
        let motion = |k: usize| {
            let to = std::array::from_fn(|i| (quad[i].0 + course[i].0 * k as f64, quad[i].1 + course[i].1 * k as f64));
            Homography::from_4_points(&quad, &to).unwrap()
        };
        let reference = warped(w, h, &Homography::identity());
        let frames: Vec<usize> = (1..=24).collect();
        let got = track_window_perspective(None, &reference, quad, &frames, &mut |k| Some(warped(w, h, &motion(k))), &|_| true);
        assert_eq!(got.len(), 24, "every frame followed");
        for (k, map) in &got {
            let want = motion(*k);
            // The middle to a quarter of a pixel, the corners to under
            // one.
            for (c, within) in [(quad[0], 1.0), (quad[1], 1.0), (quad[2], 1.0), (quad[3], 1.0), ((120.0, 80.0), 0.25)] {
                let (g, t) = (map.map(c.0, c.1).unwrap_or_else(|| panic!("frame {k}: {c:?} past the horizon, {map:?}")), want.transform(c.0, c.1));
                assert!((g.0 - t.0).hypot(g.1 - t.1) < within, "frame {k}: {c:?} at {g:.2?}, wanted {t:.2?}");
            }
        }
        // The far side really did shrink against the near one: a
        // similarity could not have followed this.
        let last = motion(24);
        let side = |a: (f64, f64), b: (f64, f64)| {
            let (a, b) = (last.transform(a.0, a.1), last.transform(b.0, b.1));
            (a.0 - b.0).hypot(a.1 - b.1)
        };
        assert!(side(quad[1], quad[2]) / side(quad[0], quad[3]) < 0.7);
    }

    /// In perspective too, a frame where the content is gone stops the
    /// track.
    #[test]
    fn a_lost_window_in_perspective_stops() {
        let (w, h) = (200, 140);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = warped(w, h, &Homography::identity());
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let shifted = Homography { h: [[1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let got = track_window_perspective(None, &reference, quad, &[1, 2, 3], &mut |k| Some(if k < 2 { warped(w, h, &shifted) } else { flat.clone() }), &|_| true);
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1]);
    }
}
