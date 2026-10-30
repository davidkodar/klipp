//! A blurred title whose layers only translate by their keys (#3160):
//! each layer is drawn once, at the frame's time, and placed at each of the
//! shutter's instants by its own offset, sampled bilinearly, the layers
//! then laid in order. Drawing every layer again at every instant cost
//! some 16 ms an instant at 4K; this costs a shift and a composite.
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

/// Whether `td`'s drawing changes over time only by its layers' keyed
/// positions: some layer's centre keys move it, and nothing else about
/// any layer is timed. A layer that also scales, turns, fades, loops,
/// enters or changes its type or styles over time is drawn at each
/// instant instead, as are titles whose layers mix, mask or fill the
/// frame, and those that follow a path or fit a shape to a text.
pub(crate) fn translates_only(td: &TitleData) -> bool {
    use klipp_timeline::TitleBackground;
    if matches!(td.background, TitleBackground::FullFrame { .. }) || td.mixes_layers() || td.objects.len() > MAX_LAYERS {
        return false;
    }
    let still = |o: &TitleObject| {
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
                s.fit.is_none()
                    && !matches!(&s.kind, klipp_timeline::ShapeKind::Path(p) if !p.keyframes.is_empty() || p.trim.as_ref().is_some_and(|t| !t.keyframes.is_empty()))
            }
            _ => false,
        };
        content
            && [&t.size_x_keyframes, &t.size_y_keyframes, &t.rotation_keyframes, &t.anchor_x_keyframes, &t.anchor_y_keyframes].into_iter().all(|k| !animated(k))
            && !animated(&o.opacity_keyframes)
            && fx.motion.enter.is_none()
            && fx.motion.exit.is_none()
            && fx.loops.is_empty()
            && fx.animators.iter().all(|a| !a.enabled)
            && !fx.styles_keyed()
    };
    let moves = |o: &TitleObject| animated(&o.transform.center_x_keyframes) || animated(&o.transform.center_y_keyframes);
    let shown: Vec<&TitleObject> = td.objects.iter().filter(|o| o.visible).collect();
    shown.iter().any(|o| moves(o)) && shown.iter().all(|o| still(o))
}

/// The shown layers of `td`, by their index among them, in runs laid as
/// one: each layer its keys move alone, and each run of still ones
/// together, laid once a frame rather than at every instant.
pub(crate) fn groups(td: &TitleData) -> Vec<(Vec<usize>, bool)> {
    let mut out: Vec<(Vec<usize>, bool)> = Vec::new();
    for (i, o) in td.objects.iter().filter(|o| o.visible).enumerate() {
        let moves = animated(&o.transform.center_x_keyframes) || animated(&o.transform.center_y_keyframes);
        match out.last_mut() {
            Some((run, false)) if !moves => run.push(i),
            _ => out.push((vec![i], moves)),
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

/// How far each shown layer of `td` has moved at `at` from where it is at
/// `from`, in frame pixels, in the order they are laid.
pub(crate) fn offsets(td: &TitleData, from: f64, at: f64, width: u32, height: u32) -> Vec<[f32; 2]> {
    td.objects
        .iter()
        .filter(|o| o.visible)
        .map(|o| {
            let (a, b) = (o.transform.evaluated_at(from), o.transform.evaluated_at(at));
            [(b.center.x - a.center.x) * width as f32, (b.center.y - a.center.y) * height as f32]
        })
        .collect()
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
/// encoded): the title style shader's op 10 for a normal layer.
pub(crate) fn over_encoded(dst: &mut [f32], src: &[f32]) {
    use rayon::prelude::*;
    dst.par_chunks_exact_mut(4).zip(src.par_chunks_exact(4)).for_each(|(d, s)| {
        let sa = s[3];
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
        over_encoded(&mut dst, &[0.5, 0.5, 0.5, 1.0]);
        assert!((dst[0] - encoded(0.5)).abs() < 1e-6 && dst[3] == 1.0);
        over_encoded(&mut dst, &[1.0, 0.0, 0.0, 0.5]);
        assert!((dst[0] - (0.5 + encoded(0.5) * 0.5)).abs() < 1e-6 && (dst[1] - encoded(0.5) * 0.5).abs() < 1e-6, "{dst:?}");
    }

    /// Keys that move a layer, and nothing else timed: taken. A turn, a
    /// fade or a loop as well: drawn at each instant.
    #[test]
    fn only_titles_that_translate_are_taken() {
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut td = TitleData::single_text(klipp_timeline::TextBox { text: "Go".into(), ..Default::default() });
        assert!(!translates_only(&td), "still");
        td.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.2), key(1.0, 0.8)];
        assert!(translates_only(&td));
        let moved = offsets(&td, 0.5, 0.75, 100, 50)[0];
        assert!((moved[0] - 15.0).abs() < 1e-4 && moved[1] == 0.0, "{moved:?}");
        let mut turned = td.clone();
        turned.objects[0].transform.rotation_keyframes = vec![key(0.0, 0.0), key(1.0, 30.0)];
        assert!(!translates_only(&turned));
        let mut looped = td.clone();
        looped.objects[0].fx_mut().loops = vec![klipp_timeline::TitleLoop::Blink { rate_hz: 2.0, duty: 0.5 }];
        assert!(!translates_only(&looped));
    }
}
