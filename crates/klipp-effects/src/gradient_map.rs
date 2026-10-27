//! Gradient Map (#2758): the picture's brightness laid along a ramp of
//! three colours, its shadows taking the first, its middle the second and
//! its highlights the third, mixed over the picture by `mix`. Brightness
//! is read as it looks, by the working space's luminance weights on the
//! display encoding, and the ramp runs between the colours as they are
//! picked (sRGB), so an even grey step lands an even step along it.
//!
//! The GPU runs [`shader_wgsl`]; the CPU runs [`mapped`], the same
//! computation in Rust, so the two agree.

/// sRGB encode, as the shaders' `linear_to_srgb_component`.
fn encode(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB decode, as the shaders' `srgb_to_linear_component`.
fn decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A picked colour as the ramp takes it: sRGB, 0 to 1.
pub fn colour(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| f32::from(v) / 255.0)
}

/// Pixel `px` (linear, straight alpha) mapped: its brightness by `yw`
/// on the display encoding, along `shadow` → `mid` → `highlight` (sRGB,
/// 0 to 1), decoded and mixed over it by `mix`. Alpha kept.
pub fn mapped(px: [f32; 4], shadow: [f32; 3], mid: [f32; 3], highlight: [f32; 3], mix: f32, yw: [f32; 3]) -> [f32; 4] {
    let e = [encode(px[0].max(0.0)), encode(px[1].max(0.0)), encode(px[2].max(0.0))];
    let l = (e[0] * yw[0] + e[1] * yw[1] + e[2] * yw[2]).clamp(0.0, 1.0);
    let (a, b, t) = if l < 0.5 { (shadow, mid, l * 2.0) } else { (mid, highlight, (l - 0.5) * 2.0) };
    let m = mix.clamp(0.0, 1.0);
    let mut out = px;
    for c in 0..3 {
        let ramp = decode(a[c] + (b[c] - a[c]) * t);
        out[c] = px[c] + (ramp - px[c]) * m;
    }
    out
}

/// The CPU's Gradient Map over a frame.
pub fn cpu(mut pixels: Vec<f32>, shadow: [f32; 3], mid: [f32; 3], highlight: [f32; 3], mix: f32, yw: [f32; 3]) -> Vec<f32> {
    use rayon::prelude::*;
    pixels.par_chunks_exact_mut(4).for_each(|p| {
        let out = mapped([p[0], p[1], p[2], p[3]], shadow, mid, highlight, mix, yw);
        p.copy_from_slice(&out);
    });
    pixels
}

/// The GPU's Gradient Map: one pass, [`mapped`] per pixel. After the
/// pixel storage.
macro_rules! shader_wgsl {
    () => {
        "
struct Params {
    width: u32,
    height: u32,
    shadow_r: f32,
    shadow_g: f32,
    shadow_b: f32,
    mid_r: f32,
    mid_g: f32,
    mid_b: f32,
    highlight_r: f32,
    highlight_g: f32,
    highlight_b: f32,
    mix: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn gm_encode(c: f32) -> f32 {
    if c <= 0.0031308 { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn gm_decode(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    let e = vec3<f32>(gm_encode(max(px.r, 0.0)), gm_encode(max(px.g, 0.0)), gm_encode(max(px.b, 0.0)));
    let l = clamp(e.x * params.luma_yr + e.y * params.luma_yg + e.z * params.luma_yb, 0.0, 1.0);
    let shadow = vec3<f32>(params.shadow_r, params.shadow_g, params.shadow_b);
    let mid = vec3<f32>(params.mid_r, params.mid_g, params.mid_b);
    let highlight = vec3<f32>(params.highlight_r, params.highlight_g, params.highlight_b);
    var a = mid;
    var b = highlight;
    var t = (l - 0.5) * 2.0;
    if l < 0.5 {
        a = shadow;
        b = mid;
        t = l * 2.0;
    }
    let m = clamp(params.mix, 0.0, 1.0);
    let s = a + (b - a) * t;
    let ramp = vec3<f32>(gm_decode(s.x), gm_decode(s.y), gm_decode(s.z));
    output[idx] = pack(vec4<f32>(px.rgb + (ramp - px.rgb) * m, px.a));
}
"
    };
}
pub(crate) use shader_wgsl;

#[cfg(test)]
mod tests {
    use super::*;

    const YW: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    /// Black takes the shadow colour, white the highlight, a grey that
    /// looks halfway the middle one; alpha is kept, no mix is the picture
    /// and half a mix is halfway in linear light.
    #[test]
    fn the_ramp_runs_shadow_to_mid_to_highlight() {
        let (s, m, h) = (colour([20, 24, 60]), colour([190, 90, 70]), colour([255, 236, 190]));
        let lin = |c: [f32; 3], a: f32| [decode(c[0]), decode(c[1]), decode(c[2]), a];
        assert!(close(mapped([0.0, 0.0, 0.0, 0.5], s, m, h, 1.0, YW), lin(s, 0.5)), "black, the shadows");
        assert!(close(mapped([1.0, 1.0, 1.0, 1.0], s, m, h, 1.0, YW), lin(h, 1.0)), "white, the highlights");
        let grey = decode(0.5);
        assert!(close(mapped([grey, grey, grey, 1.0], s, m, h, 1.0, YW), lin(m, 1.0)), "a grey that looks halfway, the midtones");
        let px = [0.3, 0.1, 0.6, 1.0];
        assert!(close(mapped(px, s, m, h, 0.0, YW), px), "no mix, the picture");
        let full = mapped(px, s, m, h, 1.0, YW);
        let half = mapped(px, s, m, h, 0.5, YW);
        assert!((0..3).all(|c| (half[c] - (px[c] + full[c]) * 0.5).abs() < 1e-5), "half a mix, halfway");
    }
}
