//! A blurred title whose layers only move by their keys, or only fade, at
//! the frame's shutter instants (#3160, #3170): each layer is drawn once,
//! at the frame's time, with its fades taken out, and laid at each instant
//! at its offset, sampled bilinearly, and its alpha there; a run of layers
//! that does neither is laid once, as one. Drawing every layer again at
//! every instant cost some 16 ms an instant at 4K; this costs a shift and
//! a composite. A layer doing anything else at those instants (typing on,
//! sliding in, a keyed scale or style) has the title drawn at each.
//!
//! The CPU and the GPU stack run the same arithmetic: `shift` is the
//! title style shader's op 64, `over_encoded` its op 10, laid in the
//! authored space with its colours encoded, as the title's own layers are.

use klipp_timeline::{TitleData, TitleObject, TitleObjectContent};

/// At most this many layers are kept apart for the shutter; a title with
/// more is drawn at each instant.
pub(crate) const MAX_LAYERS: usize = 12;

fn animated(k: &[klipp_core::Keyframe]) -> bool {
    k.windows(2).any(|w| (w[0].value - w[1].value).abs() > 1e-6)
}

/// One shown layer of a title, as the shutter lays it: `single`, the layer
/// alone with its fades taken out, drawn once; its offset in frame pixels
/// and its alpha at each instant.
pub(crate) struct Layer {
    pub single: TitleData,
    pub offsets: Vec<[f32; 2]>,
    pub alphas: Vec<f32>,
}

impl Layer {
    /// Whether it is laid the same at every instant.
    pub fn holds(&self) -> bool {
        self.offsets.iter().all(|o| o.iter().all(|v| v.abs() < 1e-4)) && self.alphas.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-6)
    }
}

/// The loops that only fade a layer: its opacity scaled, nothing redrawn.
fn fades(l: &klipp_timeline::TitleLoop) -> bool {
    use klipp_timeline::TitleLoop as L;
    matches!(l, L::Blink { .. } | L::Pulse { .. } | L::Flicker { .. })
}

/// How long a motion takes on `o`, its last unit's included: as
/// klipp-title times an entrance or exit as live.
fn span(o: &TitleObject, m: &klipp_timeline::Motion) -> f32 {
    use klipp_timeline::StaggerUnit as S;
    let text = match &o.content { TitleObjectContent::TextBox(tb) => tb.text.as_str(), _ => "" };
    let units = match m.stagger {
        S::None => 0.0,
        S::Line => text.lines().count().max(1) as f32,
        S::Word => text.split_whitespace().count().max(1) as f32,
        S::Char => text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f32,
    };
    m.delay_secs + m.duration_secs + units * m.stagger_secs + 0.05
}

/// How `td` is laid at the shutter's instants `times`, around the frame's
/// time `at` in a clip `window` seconds long, or `None` when some layer
/// changes otherwise at them and the title is drawn at each. Only titles
/// whose layers don't mix, mask or fill the frame, and that don't follow
/// a path or fit a shape to a text.
pub(crate) fn plan(td: &TitleData, at: f64, times: &[f64], window: f32, width: u32, height: u32) -> Option<Vec<Layer>> {
    use klipp_timeline::{MotionKind, StaggerUnit, TitleBackground};
    if matches!(td.background, TitleBackground::FullFrame { .. }) || td.mixes_layers() || td.objects.len() > MAX_LAYERS || times.is_empty() {
        return None;
    }
    let mut layers = Vec::new();
    for o in td.objects.iter().filter(|o| o.visible) {
        let t = &o.transform;
        let fx = o.fx();
        let content = match &o.content {
            TitleObjectContent::TextBox(tb) => {
                !tb.has_keyframes()
                    && tb.char_animation.as_ref().is_none_or(|c| !c.enabled)
                    && tb.glitch_intensity <= 0.0
                    && tb.text_path.is_none()
                    && tb.three_d.as_ref().is_none_or(|d| !d.enabled)
            }
            TitleObjectContent::Shape(s) => {
                s.fit.is_none() && !matches!(&s.kind, klipp_timeline::ShapeKind::Path(p) if !p.keyframes.is_empty() || p.trim.as_ref().is_some_and(|t| !t.keyframes.is_empty()))
            }
            _ => false,
        };
        let still_otherwise = content
            && [&t.size_x_keyframes, &t.size_y_keyframes, &t.rotation_keyframes, &t.anchor_x_keyframes, &t.anchor_y_keyframes].into_iter().all(|k| !animated(k))
            && fx.loops.iter().all(fades)
            && fx.animators.iter().all(|a| !a.enabled)
            && !fx.styles_keyed();
        if !still_otherwise {
            return None;
        }
        // An entrance or exit: a whole-layer fade is an alpha; any other is
        // laid only where it is at rest at every instant.
        let whole_fade = |m: &klipp_timeline::Motion| m.kind == MotionKind::Fade && m.stagger == StaggerUnit::None;
        for &tk in times {
            let tk = tk as f32;
            let enter_live = fx.motion.enter.as_ref().is_some_and(|m| !whole_fade(m) && tk < span(o, m));
            let exit_live = fx.motion.exit.as_ref().is_some_and(|m| !whole_fade(m) && window - tk < span(o, m));
            if enter_live || exit_live {
                return None;
            }
        }
        let fading: Vec<klipp_timeline::TitleLoop> = fx.loops.clone();
        let alphas = times
            .iter()
            .map(|&tk| {
                let (pe, px) = fx.motion.progress(tk as f32, Some(window), 0);
                let mut a = o.evaluated_opacity(tk).clamp(0.0, 1.0);
                if let Some(m) = fx.motion.enter.as_ref().filter(|m| whole_fade(m)) {
                    a *= m.pose(pe, 1.0).0;
                }
                if let Some(m) = fx.motion.exit.as_ref().filter(|m| whole_fade(m)) {
                    a *= m.pose(px, 1.0).0;
                }
                if !fading.is_empty() {
                    let geo = klipp_timeline::LoopGeometry { frame_w: width as f32, frame_h: height as f32, text_w: 1.0, text_h: 1.0, font_px: 1.0 };
                    a *= klipp_core::title_motion::eval_loops(&fading, tk as f32, Some(window), geo, [255, 255, 255, 255]).opacity;
                }
                a.clamp(0.0, 1.0)
            })
            .collect();
        let from = t.evaluated_at(at);
        let offsets = times
            .iter()
            .map(|&tk| {
                let b = t.evaluated_at(tk);
                [(b.center.x - from.center.x) * width as f32, (b.center.y - from.center.y) * height as f32]
            })
            .collect();
        // The layer alone, its fades taken out: its entrance and exit (a
        // fade, or at rest throughout), its loops and its opacity are laid.
        let mut single = td.clone();
        let mut lone = o.clone();
        lone.opacity = 1.0;
        lone.opacity_keyframes.clear();
        lone.fx_mut().loops.clear();
        lone.fx_mut().motion = Default::default();
        single.objects = vec![lone];
        layers.push(Layer { single, offsets, alphas });
    }
    // Worth it only when something moves or fades: a title held still is
    // drawn once and placed by the shutter as before.
    layers.iter().any(|l| !l.holds()).then_some(layers)
}

/// The layers in the runs they are laid in: each that changes across the
/// instants alone, and each run of ones that don't, together, by index.
pub(crate) fn runs(layers: &[Layer]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, l) in layers.iter().enumerate() {
        match out.last_mut() {
            Some(run) if l.holds() && layers[run[0]].holds() => run.push(i),
            _ => out.push(vec![i]),
        }
    }
    out
}

/// A picture laid by `over_encoded`, back in linear light, straight: a run
/// of still layers, laid once, to lay again as one layer.
pub(crate) fn decoded(laid: &[f32]) -> Vec<f32> {
    let linear = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    laid.chunks_exact(4).flat_map(|p| [linear(p[0]), linear(p[1]), linear(p[2]), p[3]]).collect()
}

/// `src` (straight RGBA, linear light) moved by `by` pixels, sampled
/// bilinearly in premultiplied light, nothing read from outside the frame:
/// the title style shader's op 64.
pub(crate) fn shift(src: &[f32], width: u32, height: u32, by: [f32; 2]) -> Vec<f32> {
    use rayon::prelude::*;
    let (w, h) = (width as i32, height as i32);
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_exact_mut(4).enumerate().for_each(|(i, px)| {
        let (x, y) = ((i as i32 % w) as f32, (i as i32 / w) as f32);
        let (sx, sy) = (x - by[0], y - by[1]);
        let (bx, by) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - bx, sy - by);
        let mut acc = [0.0f32; 4];
        for j in 0..2 {
            for k in 0..2 {
                let (xi, yi) = (bx as i32 + k, by as i32 + j);
                if xi < 0 || yi < 0 || xi >= w || yi >= h {
                    continue;
                }
                let weight = if k == 1 { fx } else { 1.0 - fx } * if j == 1 { fy } else { 1.0 - fy };
                let v = &src[((yi * w + xi) * 4) as usize..][..4];
                for c in 0..3 {
                    acc[c] += v[c] * v[3] * weight;
                }
                acc[3] += v[3] * weight;
            }
        }
        if acc[3] > 0.0 {
            px.copy_from_slice(&[acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]);
        }
    });
    out
}

fn encoded(x: f32) -> f32 {
    if x <= 0.0031308 { 12.92 * x } else { 1.055 * x.max(0.0).powf(1.0 / 2.4) - 0.055 }
}

/// `src` (straight RGBA, linear authored light) laid over `dst` (straight,
/// encoded) at `opacity`: the title style shader's op 10 for a normal
/// layer, its alpha rounded to a byte when faded.
pub(crate) fn over_encoded(dst: &mut [f32], src: &[f32], opacity: f32) {
    use rayon::prelude::*;
    let faded = (opacity - 1.0).abs() >= 0.001;
    dst.par_chunks_exact_mut(4).zip(src.par_chunks_exact(4)).for_each(|(d, s)| {
        let sa = if faded { (s[3] * opacity * 255.0 + 0.5).floor() / 255.0 } else { s[3] };
        let a = sa + d[3] * (1.0 - sa);
        if a <= 0.0 {
            d.copy_from_slice(&[0.0; 4]);
            return;
        }
        for c in 0..3 {
            d[c] = (encoded(s[c]) * sa + d[c] * d[3] * (1.0 - sa)) / a;
        }
        d[3] = a;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole-pixel shift moves the picture exactly; a half-pixel one
    /// spreads an opaque pixel over two at half their alpha, its colour
    /// kept; nothing comes in from outside the frame.
    #[test]
    fn a_layer_is_shifted_bilinearly_in_premultiplied_light() {
        let (w, h) = (4u32, 1u32);
        let mut src = vec![0.0f32; 16];
        src[4..8].copy_from_slice(&[0.2, 0.4, 0.6, 1.0]);
        let moved = shift(&src, w, h, [1.0, 0.0]);
        assert_eq!(&moved[8..12], &[0.2, 0.4, 0.6, 1.0]);
        assert_eq!(moved[7], 0.0);
        let half = shift(&src, w, h, [0.5, 0.0]);
        for px in [&half[4..8], &half[8..12]] {
            assert!((px[3] - 0.5).abs() < 1e-6 && (px[0] - 0.2).abs() < 1e-6, "{px:?}");
        }
        assert_eq!(shift(&src, w, h, [-3.0, 0.0]).iter().sum::<f32>(), 0.0, "moved off the frame");
    }

    /// A layer laid over nothing is itself, encoded; a translucent one over
    /// an opaque one mixes in the encoded colours, as the title stack does.
    #[test]
    fn layers_are_laid_over_each_other_in_encoded_colour() {
        let mut dst = vec![0.0f32; 4];
        over_encoded(&mut dst, &[0.5, 0.5, 0.5, 1.0], 1.0);
        assert!((dst[0] - encoded(0.5)).abs() < 1e-6 && dst[3] == 1.0);
        over_encoded(&mut dst, &[1.0, 0.0, 0.0, 0.5], 1.0);
        assert!((dst[0] - (0.5 + encoded(0.5) * 0.5)).abs() < 1e-6 && (dst[1] - encoded(0.5) * 0.5).abs() < 1e-6, "{dst:?}");
    }

    /// A layer its keys move and one that blinks are laid at each instant
    /// at their offset and alpha; a keyed turn, a loop that moves, or an
    /// entrance that slides while the shutter is open has the title drawn
    /// at each instead; one at rest throughout is laid.
    #[test]
    fn a_title_is_planned_only_when_its_layers_move_or_fade() {
        use klipp_timeline::{Motion, MotionKind, TitleLoop, TitleMotion};
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut td = TitleData::single_text(klipp_timeline::TextBox { text: "Go".into(), ..Default::default() });
        let times = [0.49, 0.5, 0.51];
        assert!(plan(&td, 0.5, &times, 3.0, 100, 50).is_none(), "still");
        td.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.2), key(1.0, 0.8)];
        let p = plan(&td, 0.5, &times, 3.0, 100, 50).expect("moves");
        assert!((p[0].offsets[2][0] - 0.6).abs() < 1e-3 && p[0].alphas == vec![1.0; 3]);
        let mut blink = TitleData::single_text(klipp_timeline::TextBox { text: "Go".into(), ..Default::default() });
        blink.objects[0].fx_mut().loops = vec![TitleLoop::Blink { rate_hz: 2.0, duty: 0.5 }];
        let p = plan(&blink, 0.25, &[0.2, 0.25, 0.3], 3.0, 100, 50).expect("fades");
        assert_eq!(p[0].alphas, vec![1.0, 0.0, 0.0], "on, then off at a quarter second");
        assert!(p[0].single.objects[0].fx().loops.is_empty(), "drawn without it");
        let mut turned = td.clone();
        turned.objects[0].transform.rotation_keyframes = vec![key(0.0, 0.0), key(1.0, 30.0)];
        assert!(plan(&turned, 0.5, &times, 3.0, 100, 50).is_none());
        let mut shaken = td.clone();
        shaken.objects[0].fx_mut().loops = vec![TitleLoop::Shake { amplitude: 0.1, rate_hz: 3.0 }];
        assert!(plan(&shaken, 0.5, &times, 3.0, 100, 50).is_none());
        let mut slid = td.clone();
        slid.objects[0].fx_mut().motion = TitleMotion::enter(Motion::new(MotionKind::SlideRight).duration(0.3));
        assert!(plan(&slid, 0.2, &[0.19, 0.2, 0.21], 3.0, 100, 50).is_none(), "sliding in");
        assert!(plan(&slid, 0.5, &times, 3.0, 100, 50).is_some(), "at rest");
        let mut faded = td.clone();
        faded.objects[0].fx_mut().motion = TitleMotion::enter(Motion::new(MotionKind::Fade).duration(1.0));
        let p = plan(&faded, 0.5, &times, 3.0, 100, 50).expect("a fade is an alpha");
        assert!(p[0].alphas[0] < p[0].alphas[2], "{:?}", p[0].alphas);
    }
}
