

fn enc3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_component(v.x), linear_to_srgb_component(v.y), linear_to_srgb_component(v.z));
}
fn dec3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_component(v.x), srgb_to_linear_component(v.y), srgb_to_linear_component(v.z));
}

// Screen blend — light added on top — at the picture's own ceiling.
// 1 - (1-o)(1-v) is only a blend while o and v stay under 1: on a
// highlight of 4.0 both factors go negative and their product came back
// as a negative pixel, a black halo where the glow should be. Written as
// o + v - o*v/W with W = max(1, o, v) it is the same formula under 1 and
// keeps adding light above it.
fn screen_at_ceiling(o: f32, v: f32) -> f32 {
    let w = max(max(o, v), 1.0);
    return o + v - o * v / w;
}

fn screen3_at_ceiling(o: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(screen_at_ceiling(o.x, v.x), screen_at_ceiling(o.y, v.y), screen_at_ceiling(o.z, v.z));
}

// sRGB encode / decode that keep the sign, so a negative channel — a
// colour outside the working space's gamut — is not lost to a NaN or a
// clamp.
fn enc_signed(c: f32) -> f32 {
    let e = linear_to_srgb_component(abs(c));
    return select(e, -e, c < 0.0);
}
fn dec_signed(c: f32) -> f32 {
    let d = srgb_to_linear_component(abs(c));
    return select(d, -d, c < 0.0);
}

// The HSL the hue/sat tools work in: the picture as displayed (sRGB
// encoded, where their axes live), and scaled into [0, 1] when the pixel
// is brighter than white, because HSL's saturation is only defined
// there — feeding it a highlight of 4.0 gave a negative saturation that
// clamped to zero, which turned every bright colour grey. `magnitude`
// carries the scale back out.
struct PictureHsl {
    h: f32,
    /// Saturation as a fraction of the brightest channel,
    /// `(max - min) / max` — the HSV/HSB definition, and what the
    /// Hue/Sat curves, the Colour Warper and the Qualifier all read.
    ///
    /// It used to be the HSL one, `(max - min) / (1 - |2l - 1|)`, which
    /// is wrong for these three tools in two ways. A near-white pastel
    /// came back fully saturated — `(1.0, 0.95, 0.95)`, almost no colour
    /// in it, read 1.000, the same as a vivid red highlight — so the
    /// qualifier could not tell rich from pale up there and a
    /// saturation boost had nowhere to go. And it was wrong in range
    /// too: light pastel pink read 0.497 against a mid saturated red's
    /// 0.457, which no colourist would agree with. User's choice,
    /// 2026-09-12.
    s: f32,
    /// Lightness, `(max + min) / 2`. Kept for `picture_lum_axis` — the
    /// luminance the lum-driven lookups and the qualifier's luminance
    /// window use — and *not* what a saturation change holds fixed.
    l: f32,
    /// The brightest channel, normalised. This is what a saturation
    /// change holds fixed, so making a colour more saturated deepens it
    /// rather than brightening it, which is what a saturation control is
    /// expected to do.
    v: f32,
    magnitude: f32,
}

fn picture_hsl(c: vec3<f32>) -> PictureHsl {
    let e = vec3<f32>(enc_signed(c.x), enc_signed(c.y), enc_signed(c.z));
    let magnitude = max(max(max(e.x, e.y), e.z), 1.0);
    let n = e / magnitude;
    let mx = max(max(n.x, n.y), n.z);
    let mn = min(min(n.x, n.y), n.z);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    var s = 0.0;
    var h = 0.0;
    if delta > 0.0001 {
        s = clamp(delta / max(mx, 0.0001), 0.0, 1.0);
        if mx == n.x {
            h = ((n.y - n.z) / delta) % 6.0;
            if h < 0.0 { h = h + 6.0; }
            h = h / 6.0;
        } else if mx == n.y {
            h = ((n.z - n.x) / delta + 2.0) / 6.0;
        } else {
            h = ((n.x - n.y) / delta + 4.0) / 6.0;
        }
    }
    return PictureHsl(h, s, l, mx, magnitude);
}

// Hue, max-normalised saturation and value back to a normalised encoded
// RGB. The partner of `picture_hsl`'s `s`: chroma is `s * v`, so the
// brightest channel stays where it was and only the others move.
fn hsv_rgb_norm(h: f32, s: f32, v: f32) -> vec3<f32> {
    if s < 0.0001 { return vec3<f32>(v, v, v); }
    let c = s * v;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = v - c;
    var rgb: vec3<f32>;
    switch u32(h * 6.0) {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

// The inverse of `picture_hsl`: hue, saturation and value back to
// linear light. `magnitude` puts a pixel that was brighter than white
// back where it was.
fn picture_rgb(h: f32, s: f32, v: f32, magnitude: f32) -> vec3<f32> {
    let e = hsv_rgb_norm(h, s, v) * magnitude;
    return vec3<f32>(dec_signed(e.x), dec_signed(e.y), dec_signed(e.z));
}

// The luminance axis those tools look up on: the encoded lightness, with
// everything above white at the top of the axis.
fn picture_lum_axis(p: PictureHsl) -> f32 {
    return clamp(p.l * p.magnitude, 0.0, 1.0);
}

// Hue rotation by `degrees` in HSL, the hue slider's meaning: a colour's
// HSL hue moves by exactly that much. HSL needs bounded input, so a pixel
// brighter than 1 is scaled into range and back. Greys are left alone.
fn hue_rotate_hsl(c: vec3<f32>, degrees: f32) -> vec3<f32> {
    let magnitude = max(max(max(c.x, c.y), c.z), 1.0);
    let r = c.x / magnitude;
    let g = c.y / magnitude;
    let b = c.z / magnitude;
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    if delta < 0.0001 { return c; }
    let s = delta / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r {
        h = ((g - b) / delta) % 6.0;
        if h < 0.0 { h = h + 6.0; }
        h = h / 6.0;
    } else if mx == g {
        h = ((b - r) / delta + 2.0) / 6.0;
    } else {
        h = ((r - g) / delta + 4.0) / 6.0;
    }
    h = (h + degrees / 360.0) % 1.0;
    if h < 0.0 { h = h + 1.0; }
    let cc = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = cc * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - cc * 0.5;
    var o: vec3<f32>;
    switch u32(h * 6.0) {
        case 0u: { o = vec3<f32>(cc, x, 0.0); }
        case 1u: { o = vec3<f32>(x, cc, 0.0); }
        case 2u: { o = vec3<f32>(0.0, cc, x); }
        case 3u: { o = vec3<f32>(0.0, x, cc); }
        case 4u: { o = vec3<f32>(x, 0.0, cc); }
        default: { o = vec3<f32>(cc, 0.0, x); }
    }
    return (o + vec3<f32>(m, m, m)) * magnitude;
}
