//! Generator frame rendering — produces RGBA8 pixel buffers for each GeneratorData variant.

use crate::{FourPointGradient, GeneratorData, TimerFormat, TimerMode};

/// Render a generator frame at the given resolution.
/// `elapsed_secs` is the clip-relative time, negative before the clip
/// starts (a transition's overlap, #1918; Countdown and Timer are never
/// drawn there, see `GeneratorData::shows_own_time`);
/// `total_secs` is the clip's full duration (used by Countdown).
/// A generator's frame as sRGB-encoded half floats, for the compositor.
///
/// The continuous-tone generators, the two-point, four-point and living
/// gradients, draw straight into float (#190): a gradient is the one
/// thing here that eight bits genuinely cannot hold, because it crosses
/// few code values over a whole frame and the steps between them land
/// as bands. Everything else is flat colour, bars, hard edges or noise
/// that eight bits hold exactly, so it is drawn as bytes and widened
/// exactly the way the decode pass would have widened it. (A slate's
/// text is drawn in float by `klipp_compose::synth`.)
pub fn render_generator_f16(
    data: &GeneratorData,
    width: u32,
    height: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u32> {
    match data {
        GeneratorData::Gradient { top, bottom, angle_deg, four_point: None, grain } => {
            render_gradient_f16(*top, *bottom, *angle_deg, *grain, width, height)
        }
        GeneratorData::Gradient { four_point: Some(corners), grain, .. } => render_four_point_gradient_f16(*corners, *grain, width, height),
        GeneratorData::LivingGradient { a, b, angle_deg, drift_deg_per_sec, glow, grain } => {
            render_living_gradient_f16(*a, *b, *angle_deg, *drift_deg_per_sec, *glow, *grain, width, height, elapsed_secs)
        }
        _ => srgb_u8_to_f16(&render_generator(data, width, height, elapsed_secs, total_secs)),
    }
}

pub fn render_generator(
    data: &GeneratorData,
    width: u32,
    height: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u8> {
    match data {
        GeneratorData::SolidColor { color } => render_solid_color(*color, width, height),
        GeneratorData::LivingGradient { a, b, angle_deg, drift_deg_per_sec, glow, .. } => {
            render_living_gradient(*a, *b, *angle_deg, *drift_deg_per_sec, *glow, width, height, elapsed_secs)
        }
        GeneratorData::Grain { color, amount, size_px, speed } => {
            render_grain(*color, *amount, *size_px, *speed, width, height, elapsed_secs)
        }
        GeneratorData::Placeholder { color, .. } => render_placeholder_backdrop(*color, width, height),
        GeneratorData::Gradient { top, bottom, angle_deg, four_point, .. } => {
            if let Some(corners) = four_point {
                render_four_point_gradient(*corners, width, height)
            } else {
                render_gradient(*top, *bottom, *angle_deg, width, height)
            }
        }
        GeneratorData::ColorBars => render_color_bars(width, height),
        GeneratorData::ColorBars100 => render_color_bars_100(width, height),
        GeneratorData::ColorBarsSmpte => render_color_bars_smpte(width, height),
        GeneratorData::Countdown => render_countdown(width, height, elapsed_secs, total_secs),
        GeneratorData::Timer { mode, initial_seconds, format, text_color, background_color } => {
            render_timer(width, height, elapsed_secs, *mode, *initial_seconds, *format, *text_color, *background_color)
        }
        GeneratorData::Slate { background_color, .. } => {
            // klipp-core can't render text. The engine intercepts Slate
            // before this dispatch and routes through klipp-title's
            // TitleRenderer; this fallback is only hit when render_generator
            // is called directly (tests / preview thumbnails) and yields a
            // plain background-coloured frame instead of an error magenta.
            render_solid_color(*background_color, width, height)
        }
        GeneratorData::StillFrame { png_path, .. } => render_still_frame(png_path, width, height),
        GeneratorData::Polygon { sides, color, outline_width_px, fill } => {
            render_polygon(*sides, *color, *outline_width_px, *fill, width, height)
        }
        GeneratorData::Star { points, inner_radius_pct, color, outline_width_px, fill } => {
            render_star(*points, *inner_radius_pct, *color, *outline_width_px, *fill, width, height)
        }
    }
}

/// Build the vertex list for a regular n-gon centred in the frame,
/// inscribed in a circle of radius `radius_px`. Vertices are ordered
/// clockwise starting at the top (12 o'clock). Pulled out so the
/// star renderer can reuse it on a 2N-vertex alternating path.
fn polygon_vertices(
    n: usize,
    cx: f32,
    cy: f32,
    radius_px: f32,
) -> Vec<(f32, f32)> {
    let mut verts = Vec::with_capacity(n);
    for i in 0..n {
        // -π/2 puts vertex 0 at the top; positive angle goes clockwise
        // because our y-axis is inverted (image coordinates).
        let theta = -std::f32::consts::FRAC_PI_2
            + (i as f32) * std::f32::consts::TAU / (n as f32);
        verts.push((cx + radius_px * theta.cos(), cy + radius_px * theta.sin()));
    }
    verts
}

/// Same shape but alternating between an outer and an inner radius —
/// produces a 2N-vertex star polygon.
fn star_vertices(
    points: usize,
    cx: f32,
    cy: f32,
    outer: f32,
    inner: f32,
) -> Vec<(f32, f32)> {
    let total = points * 2;
    let mut verts = Vec::with_capacity(total);
    for i in 0..total {
        let r = if i % 2 == 0 { outer } else { inner };
        let theta = -std::f32::consts::FRAC_PI_2
            + (i as f32) * std::f32::consts::TAU / (total as f32);
        verts.push((cx + r * theta.cos(), cy + r * theta.sin()));
    }
    verts
}

/// Even-odd point-in-polygon test. Robust against vertex order and
/// concavity (which is what makes it work for stars). Edge cases
/// (point exactly on an edge) round consistently — egui's pixel
/// grid centres each pixel at integer coordinates so we don't see
/// half-pixel ambiguity in practice.
fn point_in_polygon(verts: &[(f32, f32)], px: f32, py: f32) -> bool {
    let mut inside = false;
    let n = verts.len();
    if n < 3 { return false; }
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = verts[i];
        let (xj, yj) = verts[j];
        let crosses = (yi > py) != (yj > py);
        if crosses {
            // Slope-test x-coordinate at this y.
            let x_at = (xj - xi) * (py - yi) / (yj - yi) + xi;
            if px < x_at {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Squared distance from a point to a line segment, in pixels.
/// Used for outline-only rendering: a pixel is "on" the stroke
/// when its minimum distance to any polygon edge is ≤ half the
/// stroke width.
fn point_to_segment_dist2(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-6 {
        let dx0 = px - ax;
        let dy0 = py - ay;
        return dx0 * dx0 + dy0 * dy0;
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    let qx = ax + t * dx;
    let qy = ay + t * dy;
    let ex = px - qx;
    let ey = py - qy;
    ex * ex + ey * ey
}

/// Generic polygon rasteriser used by both the regular-polygon and
/// star renderers. Background is transparent (alpha 0); pixels
/// inside the polygon get `color` (or the outline if `fill` is
/// false); pixels within `outline_width_px / 2` of any edge are
/// also drawn in `color` regardless of fill mode.
fn rasterise_polygon(
    verts: &[(f32, f32)],
    color: [u8; 4],
    outline_width_px: f32,
    fill: bool,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = vec![0u8; pixel_count * 4];
    let n = verts.len();
    if n < 3 { return buf; }
    let stroke_half = outline_width_px.max(0.0) * 0.5;
    let stroke_thresh2 = stroke_half * stroke_half;
    let needs_stroke = stroke_half > 0.0;

    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut hit = false;
            if fill && point_in_polygon(verts, px, py) {
                hit = true;
            }
            if !hit && needs_stroke {
                // Distance to any edge ≤ stroke_half → on the line.
                let mut min_d2 = f32::INFINITY;
                let mut j = n - 1;
                for i in 0..n {
                    let d2 = point_to_segment_dist2(
                        px, py,
                        verts[j].0, verts[j].1,
                        verts[i].0, verts[i].1,
                    );
                    if d2 < min_d2 { min_d2 = d2; }
                    j = i;
                }
                if min_d2 <= stroke_thresh2 {
                    hit = true;
                }
            }
            if hit {
                let i = ((y as usize) * (width as usize) + (x as usize)) * 4;
                buf[i] = color[0];
                buf[i + 1] = color[1];
                buf[i + 2] = color[2];
                buf[i + 3] = color[3];
            }
        }
    }
    buf
}

/// Regular polygon centred in the frame. `sides < 3` falls back to 3
/// (triangle) — a straight-line "polygon" makes no visible sense
/// and we'd rather show *something* than a blank frame. Inscribed
/// in a circle of radius 40 % of the frame's shorter side so the
/// shape stays comfortably inside the visible area at any aspect.
pub fn render_polygon(
    sides: u32,
    color: [u8; 4],
    outline_width_px: f32,
    fill: bool,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let radius = (width.min(height) as f32) * 0.40;
    // From CIRCLE_SIDES up, a true circle, its edge smoothed over a
    // pixel: a 24-gon read as facets at full frame (#2775).
    if sides >= CIRCLE_SIDES {
        return render_circle(cx, cy, radius, color, outline_width_px, fill, width, height);
    }
    let n = sides.max(3) as usize;
    let verts = polygon_vertices(n, cx, cy, radius);
    rasterise_polygon(&verts, color, outline_width_px, fill, width, height)
}

/// The side count from which a polygon is drawn as a true circle.
pub const CIRCLE_SIDES: u32 = 64;

/// A circle centred at `cx, cy`, filled or as a ring `outline_width_px`
/// wide (both when filled with an outline), its edges anti-aliased by
/// the pixel's distance to them: smooth at any size.
#[allow(clippy::too_many_arguments)]
fn render_circle(cx: f32, cy: f32, radius: f32, color: [u8; 4], outline_width_px: f32, fill: bool, width: u32, height: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
    let half = outline_width_px.max(0.0) * 0.5;
    for y in 0..height {
        for x in 0..width {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            // Coverage: inside the disc, and within the stroke of its edge.
            let disc = if fill { (radius + 0.5 - d).clamp(0.0, 1.0) } else { 0.0 };
            let ring = if half > 0.0 { (half + 0.5 - (d - radius).abs()).clamp(0.0, 1.0) } else { 0.0 };
            let cover = disc.max(ring);
            if cover > 0.0 {
                let i = ((y * width + x) * 4) as usize;
                buf[i..i + 3].copy_from_slice(&color[..3]);
                buf[i + 3] = (color[3] as f32 * cover).round() as u8;
            }
        }
    }
    buf
}

/// N-pointed star centred in the frame. `points < 3` falls back to 3
/// (a degenerate triangle-with-pinch — visually unusual but still
/// a star). `inner_radius_pct` is clamped to (0.05, 0.95) so we
/// never emit a degenerate zero-width or all-but-collapsed shape.
pub fn render_star(
    points: u32,
    inner_radius_pct: f32,
    color: [u8; 4],
    outline_width_px: f32,
    fill: bool,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let n = points.max(3) as usize;
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let outer = (width.min(height) as f32) * 0.40;
    let inner = outer * inner_radius_pct.clamp(0.05, 0.95);
    let verts = star_vertices(n, cx, cy, outer, inner);
    rasterise_polygon(&verts, color, outline_width_px, fill, width, height)
}

/// Decode the sidecar PNG at `png_path` and resize to `(width, height)`.
/// Missing/corrupt file → solid magenta so the error is visibly obvious
/// rather than silently substituting black.
pub fn render_still_frame(png_path: &std::path::Path, width: u32, height: u32) -> Vec<u8> {
    match image::open(png_path) {
        Ok(img) => img
            .resize_exact(width, height, image::imageops::FilterType::Lanczos3)
            .to_rgba8()
            .into_raw(),
        Err(_) => {
            let pixel_count = (width as usize) * (height as usize);
            let mut buf = Vec::with_capacity(pixel_count * 4);
            for _ in 0..pixel_count {
                buf.extend_from_slice(&[255u8, 0, 255, 255]);
            }
            buf
        }
    }
}

/// Render a solid color frame.
/// A small integer hash to a value in -1..1, stable per (x, y, frame).
fn grain_at(x: u32, y: u32, frame: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ frame.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// The gradient with its angle turned by `drift` degrees per second,
/// and a soft light of strength `glow` drifting across it on a slow
/// ellipse, so a background behind a title is never a still.
#[allow(clippy::too_many_arguments)]
/// The sRGB EOTF, the same curve the `sprite_rows` shader decodes the
/// eight-bit path with: IEC 61966-2-1, so a generator that goes the
/// float way lands on the values the u8 way would have decoded to.
#[inline]
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Eight-bit sRGB RGBA widened to linear-light f32 RGBA — what the
/// decode pass does, for the generators that have no float path of
/// their own. Alpha is linear already and only scales.
pub fn srgb_u8_to_linear_f32(rgba: &[u8]) -> Vec<f32> {
    rgba.chunks_exact(4)
        .flat_map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0,
            ]
        })
        .collect()
}

/// Two RGBA halves to a u32 pair: what the GPU reads with
/// `unpack2x16float`.
#[inline]
fn pack_f16(px: [f32; 4]) -> [u32; 2] {
    let h = |v: f32| half::f16::from_f32(v).to_bits() as u32;
    [h(px[0]) | (h(px[1]) << 16), h(px[2]) | (h(px[3]) << 16)]
}

/// [`pack_f16`]'s inverse.
#[inline]
fn unpack_f16(p: [u32; 2]) -> [f32; 4] {
    let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32();
    [un(p[0] & 0xFFFF), un(p[0] >> 16), un(p[1] & 0xFFFF), un(p[1] >> 16)]
}

/// `px` packed, with the generator grain (#2073) of `grain` code values
/// at pixel `index` (`y * width + x`) when there is any: rounded to half
/// floats first, grained, and rounded again, as the GPU's living
/// gradient pass does. At 0 it is [`pack_f16`] and nothing else, so a
/// gradient with its grain off is the one drawn before there was any.
#[inline]
fn pack_grained(px: [f32; 4], index: u32, grain: f32) -> [u32; 2] {
    let packed = pack_f16(px);
    if grain <= 0.0 {
        return packed;
    }
    grained(packed, index, grain)
}

/// Packed half floats with the generator grain added and rounded again.
#[inline]
fn grained(packed: [u32; 2], index: u32, grain: f32) -> [u32; 2] {
    use klipp_effects::generator_grain::{grain_channel, grain_noise};
    let c = unpack_f16(packed);
    let n = grain_noise(index);
    pack_f16([grain_channel(c[0], n, grain), grain_channel(c[1], n, grain), grain_channel(c[2], n, grain), c[3]])
}

/// A two-point gradient, sRGB-encoded, in half floats.
///
/// Encoded rather than linear, and packed rather than whole, because of
/// where the work lands. sRGB is a power curve; converting on the CPU is
/// a `powf` a channel, 6.2 million of them across a 1080p frame, and it
/// cost more than drawing the gradient did. The GPU already decodes the
/// eight-bit path on its way into the layer and will decode this one
/// the same way, for nothing. Half floats carry eleven bits of mantissa
/// where sRGB asks for eight, at half the bytes of f32.
///
/// The ramp itself is the one the byte path draws — this is a change of
/// precision, not of look. Interpolating in linear light would be a
/// different, brighter gradient, and that is a thing to choose.
///
/// `grain` is the generator grain, in code values (#2073; 0 for none).
pub fn render_gradient_f16(
    top: [u8; 4],
    bottom: [u8; 4],
    angle_deg: f32,
    grain: f32,
    width: u32,
    height: u32,
) -> Vec<u32> {
    let mut buf = vec![0u32; (width as usize) * (height as usize) * 2];
    let (w, h) = (width as f32, height as f32);
    let theta = angle_deg.to_radians();
    let (dir_x, dir_y) = (theta.sin(), theta.cos());
    let span_w = (w - 1.0).max(0.0);
    let span_h = (h - 1.0).max(0.0);
    let (cx, cy) = (span_w * 0.5, span_h * 0.5);
    let half_extent = (dir_x.abs() * span_w * 0.5) + (dir_y.abs() * span_h * 0.5);
    let extent = (half_extent * 2.0).max(1.0);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let t = (((dx * dir_x + dy * dir_y) + half_extent) / extent).clamp(0.0, 1.0);
            let mut px = [0f32; 4];
            for c in 0..4 {
                px[c] = (top[c] as f32 + (bottom[c] as f32 - top[c] as f32) * t) / 255.0;
            }
            let index = y * width + x;
            let i = index as usize * 2;
            let packed = pack_grained(px, index, grain);
            buf[i] = packed[0];
            buf[i + 1] = packed[1];
        }
    }
    buf
}

/// [`render_living_gradient`] as sRGB-encoded half floats. The drifting
/// angle and the light that crosses it are the same; the glow lifts in
/// the encoded space it has always lifted in, so it keeps its shape.
/// The grain goes on last, over the light too (#2073).
#[allow(clippy::too_many_arguments)]
pub fn render_living_gradient_f16(
    a: [u8; 4],
    b: [u8; 4],
    angle_deg: f32,
    drift_deg_per_sec: f32,
    glow: f32,
    grain: f32,
    width: u32,
    height: u32,
    elapsed_secs: f64,
) -> Vec<u32> {
    let t = elapsed_secs as f32;
    let mut buf = render_gradient_f16(a, b, angle_deg + drift_deg_per_sec * t, 0.0, width, height);
    if glow <= 0.0 && grain <= 0.0 {
        return buf;
    }
    let (w, h) = (width as f32, height as f32);
    let cx = w * (0.5 + 0.28 * (t * 0.33).cos());
    let cy = h * (0.5 + 0.22 * (t * 0.21).sin());
    let radius = w.max(h) * 0.45;
    let inv = 1.0 / (2.0 * radius * radius);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let f = if glow > 0.0 { (-(dx * dx + dy * dy) * inv).exp() * glow } else { 0.0 };
            let lit = f >= 0.002;
            if !lit && grain <= 0.0 {
                continue;
            }
            let index = y * width + x;
            let i = index as usize * 2;
            let mut packed = [buf[i], buf[i + 1]];
            if lit {
                let mut px = unpack_f16(packed);
                for v in px.iter_mut().take(3) {
                    *v = (*v + (1.0 - *v) * f * 0.6).min(1.0);
                }
                packed = pack_f16(px);
            }
            if grain > 0.0 {
                packed = grained(packed, index, grain);
            }
            buf[i] = packed[0];
            buf[i + 1] = packed[1];
        }
    }
    buf
}

/// Packed sRGB half floats unpacked and decoded to linear f32 — what
/// the GPU does on its way into a layer, for the CPU path that has no
/// GPU under it.
pub fn srgb_f16_to_linear_f32(packed: &[u32]) -> Vec<f32> {
    let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32();
    packed
        .chunks_exact(2)
        .flat_map(|p| {
            [
                srgb_to_linear(un(p[0] & 0xFFFF)),
                srgb_to_linear(un(p[0] >> 16)),
                srgb_to_linear(un(p[1] & 0xFFFF)),
                un(p[1] >> 16),
            ]
        })
        .collect()
}

/// Eight-bit sRGB RGBA repacked as sRGB half floats, for the
/// generators with no float path of their own. No precision is gained
/// and none is lost; it just takes the same road as the rest.
pub fn srgb_u8_to_f16(rgba: &[u8]) -> Vec<u32> {
    rgba.chunks_exact(4)
        .flat_map(|p| pack_f16([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]))
        .collect()
}

pub fn render_living_gradient(
    a: [u8; 4],
    b: [u8; 4],
    angle_deg: f32,
    drift_deg_per_sec: f32,
    glow: f32,
    width: u32,
    height: u32,
    elapsed_secs: f64,
) -> Vec<u8> {
    let t = elapsed_secs as f32;
    let mut buf = render_gradient(a, b, angle_deg + drift_deg_per_sec * t, width, height);
    if glow <= 0.0 {
        return buf;
    }
    let (w, h) = (width as f32, height as f32);
    let cx = w * (0.5 + 0.28 * (t * 0.33).cos());
    let cy = h * (0.5 + 0.22 * (t * 0.21).sin());
    let radius = w.max(h) * 0.45;
    let inv = 1.0 / (2.0 * radius * radius);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let f = (-(dx * dx + dy * dy) * inv).exp() * glow;
            if f < 0.002 {
                continue;
            }
            let i = ((y * width + x) * 4) as usize;
            let d = dither_at(x, y);
            for c in 0..3 {
                let v = buf[i + c] as f32;
                buf[i + c] = (v + (255.0 - v) * f * 0.6 + d).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    buf
}

/// Film grain over a colour: monochrome noise of `size_px` grains (at
/// 1080p), `amount` deep, refreshed `speed` times a second.
pub fn render_grain(color: [u8; 4], amount: f32, size_px: f32, speed: f32, width: u32, height: u32, elapsed_secs: f64) -> Vec<u8> {
    let mut buf = render_solid_color(color, width, height);
    let scale = (height as f32 / 1080.0).max(0.1);
    let cell = (size_px * scale).max(1.0);
    // The grain frame, before the clip's start too (a transition's
    // overlap, #1918): as a whole number first, so a negative one wraps to
    // a frame of its own instead of every one saturating to frame 0.
    let frame = (elapsed_secs as f32 * speed.max(0.0)).floor() as i64 as u32;
    let depth = amount.clamp(0.0, 1.0) * 110.0;
    for y in 0..height {
        let gy = (y as f32 / cell) as u32;
        for x in 0..width {
            let gx = (x as f32 / cell) as u32;
            let n = grain_at(gx, gy, frame) * depth;
            let i = ((y * width + x) * 4) as usize;
            for c in 0..3 {
                buf[i + c] = (buf[i + c] as f32 + n).clamp(0.0, 255.0) as u8;
            }
        }
    }
    buf
}

/// The stripes behind a placeholder's label: the colour and a darker
/// diagonal hatch, so the card reads as "not the shot" at a glance.
pub fn render_placeholder_backdrop(color: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let mut buf = render_solid_color(color, width, height);
    let period = (height as f32 / 1080.0 * 64.0).max(8.0);
    for y in 0..height {
        for x in 0..width {
            let d = ((x as f32 + y as f32) / period).fract();
            if d < 0.5 {
                let i = ((y * width + x) * 4) as usize;
                for c in 0..3 {
                    buf[i + c] = (buf[i + c] as f32 * 0.78) as u8;
                }
            }
        }
    }
    buf
}

pub fn render_solid_color(color: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = Vec::with_capacity(pixel_count * 4);
    for _ in 0..pixel_count {
        buf.extend_from_slice(&color);
    }
    buf
}

/// Render a 2-point linear gradient between two colours along
/// `angle_deg`. 0 = top→bottom (the historical default — `top` at
/// y=0, `bottom` at y=height-1), 90 = left→right, 180 = bottom→top,
/// 270 = right→left. Any angle is supported; the gradient axis is
/// projected through the frame's centre and the band of constant
/// colour runs perpendicular to it.
pub fn render_gradient(
    top: [u8; 4],
    bottom: [u8; 4],
    angle_deg: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = vec![0u8; pixel_count * 4];

    let w = width as f32;
    let h = height as f32;
    // Direction unit vector. 0° = down (+Y), 90° = right (+X). Sin/cos
    // chosen so the legacy "top→bottom" mapping (angle 0) keeps top
    // at y=0. Using image coordinates where +Y is downward.
    let theta = angle_deg.to_radians();
    let dir_x = theta.sin();
    let dir_y = theta.cos();
    // Pixel-grid extents use `(dim - 1)` so the corner pixels
    // (indices 0 and dim-1) project exactly onto t=0 and t=1.
    // Using `dim` would shift the start/end colours inward by half a
    // pixel — a barely-visible 1/255 quantization error on small
    // frames, but enough to break "endpoint-correct" tests and
    // visually nudge the start of a credits-style scroll.
    let span_w = (w - 1.0).max(0.0);
    let span_h = (h - 1.0).max(0.0);
    let cx = span_w * 0.5;
    let cy = span_h * 0.5;
    let half_extent = (dir_x.abs() * span_w * 0.5) + (dir_y.abs() * span_h * 0.5);
    let extent = (half_extent * 2.0).max(1.0);

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let proj = dx * dir_x + dy * dir_y;
            let t = ((proj + half_extent) / extent).clamp(0.0, 1.0);
            let i = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let d = dither_at(x, y);
            buf[i] = lerp_u8_dithered(top[0], bottom[0], t, d);
            buf[i + 1] = lerp_u8_dithered(top[1], bottom[1], t, d);
            buf[i + 2] = lerp_u8_dithered(top[2], bottom[2], t, d);
            buf[i + 3] = lerp_u8(top[3], bottom[3], t);
        }
    }
    buf
}

/// Render a 4-point bilinear gradient. Each frame corner gets one of
/// the four colours; the body interpolates linearly along both axes
/// (horizontal first, then vertical between the resulting top/bottom
/// rows), which is the standard cinematic 4-corner colour wash.
pub fn render_four_point_gradient(
    corners: FourPointGradient,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = vec![0u8; pixel_count * 4];
    let w_div = (width.saturating_sub(1)).max(1) as f32;
    let h_div = (height.saturating_sub(1)).max(1) as f32;

    for y in 0..height {
        let ty = y as f32 / h_div;
        // Top row blend: TL → TR
        let row_top = [
            lerp_u8(corners.top_left[0], corners.top_right[0], 0.0),
            lerp_u8(corners.top_left[1], corners.top_right[1], 0.0),
            lerp_u8(corners.top_left[2], corners.top_right[2], 0.0),
            lerp_u8(corners.top_left[3], corners.top_right[3], 0.0),
        ];
        let _ = row_top; // kept for clarity; per-pixel blend below
        for x in 0..width {
            let tx = x as f32 / w_div;
            // Bilinear: blend horizontally on top + bottom rows, then
            // blend the two results vertically.
            let r_top_h = lerp_u8(corners.top_left[0], corners.top_right[0], tx);
            let g_top_h = lerp_u8(corners.top_left[1], corners.top_right[1], tx);
            let b_top_h = lerp_u8(corners.top_left[2], corners.top_right[2], tx);
            let a_top_h = lerp_u8(corners.top_left[3], corners.top_right[3], tx);
            let r_bot_h = lerp_u8(corners.bottom_left[0], corners.bottom_right[0], tx);
            let g_bot_h = lerp_u8(corners.bottom_left[1], corners.bottom_right[1], tx);
            let b_bot_h = lerp_u8(corners.bottom_left[2], corners.bottom_right[2], tx);
            let a_bot_h = lerp_u8(corners.bottom_left[3], corners.bottom_right[3], tx);
            let i = ((y as usize) * (width as usize) + (x as usize)) * 4;
            buf[i] = lerp_u8(r_top_h, r_bot_h, ty);
            buf[i + 1] = lerp_u8(g_top_h, g_bot_h, ty);
            buf[i + 2] = lerp_u8(b_top_h, b_bot_h, ty);
            buf[i + 3] = lerp_u8(a_top_h, a_bot_h, ty);
        }
    }
    buf
}

/// [`render_four_point_gradient`] as sRGB-encoded half floats (#190):
/// the same bilinear wash, interpolated in the encoded space it always
/// was, without rounding the top and bottom rows to whole codes before
/// blending between them. `grain` is the generator grain, in code values
/// (#2073; 0 for none).
pub fn render_four_point_gradient_f16(corners: FourPointGradient, grain: f32, width: u32, height: u32) -> Vec<u32> {
    let mut buf = vec![0u32; (width as usize) * (height as usize) * 2];
    let w_div = (width.saturating_sub(1)).max(1) as f32;
    let h_div = (height.saturating_sub(1)).max(1) as f32;
    let lerp = |a: u8, b: u8, t: f32| a as f32 + (b as f32 - a as f32) * t;
    for y in 0..height {
        let ty = y as f32 / h_div;
        for x in 0..width {
            let tx = x as f32 / w_div;
            let mut px = [0f32; 4];
            for c in 0..4 {
                let top = lerp(corners.top_left[c], corners.top_right[c], tx);
                let bottom = lerp(corners.bottom_left[c], corners.bottom_right[c], tx);
                px[c] = (top + (bottom - top) * ty) / 255.0;
            }
            let index = y * width + x;
            let i = index as usize * 2;
            let packed = pack_grained(px, index, grain);
            buf[i] = packed[0];
            buf[i + 1] = packed[1];
        }
    }
    buf
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// A Bayer 8x8 ordered dither, in code values, centred on zero.
///
/// For the byte gradients only, which only the browser's previews and
/// thumbnails draw now: every render and export takes the half-float
/// gradients, which need none (#190).
///
/// A gradient is a ramp of real numbers quantised to 256 steps. Where
/// the ramp is long and shallow — a dark backdrop that crosses a dozen
/// code values over a whole frame — the steps land as visible bands.
/// Nudging each pixel by up to half a code value before rounding trades
/// the bands for noise far below what anyone can see. Ordered, not
/// random, so it does not crawl from frame to frame.
#[inline]
fn dither_at(x: u32, y: u32) -> f32 {
    const BAYER: [u8; 64] = [
        0, 32, 8, 40, 2, 34, 10, 42,
        48, 16, 56, 24, 50, 18, 58, 26,
        12, 44, 4, 36, 14, 46, 6, 38,
        60, 28, 52, 20, 62, 30, 54, 22,
        3, 35, 11, 43, 1, 33, 9, 41,
        51, 19, 59, 27, 49, 17, 57, 25,
        15, 47, 7, 39, 13, 45, 5, 37,
        63, 31, 55, 23, 61, 29, 53, 21,
    ];
    (BAYER[((y % 8) * 8 + (x % 8)) as usize] as f32 + 0.5) / 64.0 - 0.5
}

/// `lerp_u8` with that nudge applied before the rounding.
#[inline]
fn lerp_u8_dithered(a: u8, b: u8, t: f32, dither: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t + dither)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Render SMPTE 75% color bars (seven vertical bars of equal width).
pub fn render_color_bars(width: u32, height: u32) -> Vec<u8> {
    render_color_bars_at_intensity(width, height, 191)
}

/// Render SMPTE 100% color bars — same seven bars at full intensity.
/// Used for hardware calibration and broadcast peak-luminance checks.
pub fn render_color_bars_100(width: u32, height: u32) -> Vec<u8> {
    render_color_bars_at_intensity(width, height, 255)
}

/// The full SMPTE colour bar pattern (as in SMPTE EG 1, in full-range
/// 8-bit RGB): the seven 75 % bars over the top two thirds; a twelfth of
/// castellations under them (blue, black, magenta, black, cyan, black,
/// white, each under one bar); and in the bottom quarter −I, 100 %
/// white, +Q and black, each a bar and a quarter wide, then PLUGE's
/// three narrow steps (below black, black and 4 % above, the first
/// held at black in full range) and black.
pub fn render_color_bars_smpte(width: u32, height: u32) -> Vec<u8> {
    const V: u8 = 191;
    let bars: [[u8; 3]; 7] = [[V, V, V], [V, V, 0], [0, V, V], [0, V, 0], [V, 0, V], [V, 0, 0], [0, 0, V]];
    let castle: [[u8; 3]; 7] = [[0, 0, V], [0, 0, 0], [V, 0, V], [0, 0, 0], [0, V, V], [0, 0, 0], [V, V, V]];
    // The bottom row in sevenths of the width: where each patch ends.
    let bottom: [(f32, [u8; 3]); 8] = [
        (1.25, [0, 33, 76]),
        (2.5, [255, 255, 255]),
        (3.75, [50, 0, 106]),
        (5.0, [0, 0, 0]),
        (5.0 + 1.0 / 3.0, [0, 0, 0]),
        (5.0 + 2.0 / 3.0, [0, 0, 0]),
        (6.0, [10, 10, 10]),
        (7.0, [0, 0, 0]),
    ];
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let mut buf = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        let fy = (y as f32 + 0.5) / h;
        for x in 0..width {
            let sevenths = (x as f32 + 0.5) / w * 7.0;
            let bar = (sevenths as usize).min(6);
            let c = if fy < 2.0 / 3.0 {
                bars[bar]
            } else if fy < 0.75 {
                castle[bar]
            } else {
                bottom.iter().find(|(end, _)| sevenths < *end).map_or([0, 0, 0], |(_, c)| *c)
            };
            buf.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    buf
}

/// Shared SMPTE-bar rasteriser. `intensity` picks between 75 % (191)
/// and 100 % (255) per the spec. Bar order from left to right is
/// white, yellow, cyan, green, magenta, red, blue.
fn render_color_bars_at_intensity(width: u32, height: u32, intensity: u8) -> Vec<u8> {
    let v = intensity;
    let bars: [[u8; 4]; 7] = [
        [v, v, v, 255], // white
        [v, v, 0, 255], // yellow
        [0, v, v, 255], // cyan
        [0, v, 0, 255], // green
        [v, 0, v, 255], // magenta
        [v, 0, 0, 255], // red
        [0, 0, v, 255], // blue
    ];
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = Vec::with_capacity(pixel_count * 4);
    for _y in 0..height {
        for x in 0..width {
            let bar_idx =
                ((x as u64 * bars.len() as u64) / width.max(1) as u64) as usize;
            let color = bars[bar_idx.min(bars.len() - 1)];
            buf.extend_from_slice(&color);
        }
    }
    buf
}

/// A frame of flat rectangles over a background (#2067): a countdown's or
/// a timer's seven-segment digits, laid out once so the CPU and the GPU
/// fill the same pixels. Each rect is `[x0, y0, x1, y1)`, already clipped
/// to the frame the way the CPU drawing always clipped it.
#[derive(Debug, Clone, PartialEq)]
pub struct Segments {
    pub background: [u8; 4],
    pub color: [u8; 4],
    pub rects: Vec<[u32; 4]>,
}

impl Segments {
    /// The frame as straight-alpha sRGB bytes.
    pub fn draw(&self, width: u32, height: u32) -> Vec<u8> {
        let mut buf: Vec<u8> = self.background.repeat((width as usize) * (height as usize));
        for &[x0, y0, x1, y1] in &self.rects {
            for y in y0..y1 {
                let row = (y as usize) * (width as usize) * 4;
                for x in x0..x1 {
                    let i = row + (x as usize) * 4;
                    buf[i..i + 4].copy_from_slice(&self.color);
                }
            }
        }
        buf
    }
}

/// Render a countdown frame: white number on black background.
/// Shows seconds remaining, counting down from total_secs.
pub fn render_countdown(
    width: u32,
    height: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u8> {
    countdown_segments(width, height, elapsed_secs, total_secs).draw(width, height)
}

/// A countdown frame's digits: the seconds remaining in white seven-
/// segment digits over transparent black.
pub fn countdown_segments(width: u32, height: u32, elapsed_secs: f64, total_secs: f64) -> Segments {
    let seconds_remaining = ((total_secs - elapsed_secs).ceil() as i64).max(1);
    let digit = format!("{}", seconds_remaining);
    let mut rects = Vec::new();

    // Render digit as a simple bitmap font — each digit is drawn as filled rectangles.
    // This avoids requiring a font library for a single generator variant.
    let digit_h = (height / 3).max(10);
    let digit_w = digit_h * 3 / 5;
    let total_w = digit.len() as u32 * (digit_w + digit_w / 4).max(1);
    let start_x = (width.saturating_sub(total_w)) / 2;
    let start_y = (height.saturating_sub(digit_h)) / 2;

    for (char_idx, ch) in digit.chars().enumerate() {
        let cx = start_x + char_idx as u32 * (digit_w + digit_w / 4);
        seven_segment_rects(&mut rects, width, height, cx, start_y, digit_w, digit_h, digit_segments(ch));
    }
    Segments { background: [0; 4], color: [255; 4], rects }
}

/// Seven-segment display encoding: [top, top-right, bottom-right, bottom, bottom-left, top-left, middle]
fn digit_segments(ch: char) -> [bool; 7] {
    match ch {
        '0' => [true, true, true, true, true, true, false],
        '1' => [false, true, true, false, false, false, false],
        '2' => [true, true, false, true, true, false, true],
        '3' => [true, true, true, true, false, false, true],
        '4' => [false, true, true, false, false, true, true],
        '5' => [true, false, true, true, false, true, true],
        '6' => [true, false, true, true, true, true, true],
        '7' => [true, true, true, false, false, false, false],
        '8' => [true, true, true, true, true, true, true],
        '9' => [true, true, true, true, false, true, true],
        _ => [false; 7],
    }
}

/// A seven-segment digit's lit segments as rects, clipped as the drawing
/// always clipped them: columns to the frame, rows to the frame and to
/// `buf_width / 4 * 4` (a bound the digits never reach in practice).
#[allow(clippy::too_many_arguments)]
fn seven_segment_rects(
    out: &mut Vec<[u32; 4]>,
    buf_width: u32,
    buf_height: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    segments: [bool; 7],
) {
    let thickness = (h / 8).max(2);
    let half_h = h / 2;

    // Segment rectangles: [top, top-right, bottom-right, bottom, bottom-left, top-left, middle]
    let rects: [(u32, u32, u32, u32); 7] = [
        (x, y, w, thickness),                                       // top
        (x + w - thickness, y, thickness, half_h),                  // top-right
        (x + w - thickness, y + half_h, thickness, h - half_h),     // bottom-right
        (x, y + h - thickness, w, thickness),                       // bottom
        (x, y + half_h, thickness, h - half_h),                     // bottom-left
        (x, y, thickness, half_h),                                  // top-left
        (x, y + half_h - thickness / 2, w, thickness),              // middle
    ];

    for (i, &(rx, ry, rw, rh)) in rects.iter().enumerate() {
        if segments[i] {
            push_rect(out, rx, ry, (rx + rw).min(buf_width), (ry + rh).min(buf_width / 4 * 4).min(buf_height));
        }
    }
}

/// `[x0, y0, x1, y1)` when it covers anything.
fn push_rect(out: &mut Vec<[u32; 4]>, x0: u32, y0: u32, x1: u32, y1: u32) {
    if x0 < x1 && y0 < y1 {
        out.push([x0, y0, x1, y1]);
    }
}

/// Format a timer value (in seconds) per the requested precision.
/// Negative values clamp to zero. The MmSs format does NOT cap at
/// 99:59 — runaway minutes print at full width (e.g. `127:30`); users
/// who need >100 min should pick HhMmSs.
pub(crate) fn format_timer_value(value: f64, format: TimerFormat) -> String {
    let value = value.max(0.0);
    let total_ms = (value * 1000.0) as u64;
    let total_secs = total_ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs / 60) % 60;
    let s = total_secs % 60;
    let ms = total_ms % 1000;
    match format {
        TimerFormat::Seconds => format!("{}", total_secs),
        TimerFormat::MmSs => format!("{:02}:{:02}", total_secs / 60, s),
        TimerFormat::HhMmSs => format!("{:02}:{:02}:{:02}", h, m, s),
        TimerFormat::HhMmSsMs => format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms),
    }
}

/// Render a timer frame: 7-segment digit string with colon separators
/// over a coloured background.
#[allow(clippy::too_many_arguments)]
pub fn render_timer(
    width: u32,
    height: u32,
    elapsed_secs: f64,
    mode: TimerMode,
    initial_seconds: f32,
    format: TimerFormat,
    text_color: [u8; 4],
    background_color: [u8; 4],
) -> Vec<u8> {
    timer_segments(width, height, elapsed_secs, mode, initial_seconds, format, text_color, background_color).draw(width, height)
}

/// A timer frame's digits: the value in seven-segment digits, with
/// stacked-dot colons and a decimal dot, in `text_color` over
/// `background_color`.
#[allow(clippy::too_many_arguments)]
pub fn timer_segments(
    width: u32,
    height: u32,
    elapsed_secs: f64,
    mode: TimerMode,
    initial_seconds: f32,
    format: TimerFormat,
    text_color: [u8; 4],
    background_color: [u8; 4],
) -> Segments {
    let value = match mode {
        TimerMode::CountUp => initial_seconds as f64 + elapsed_secs,
        TimerMode::CountDown => (initial_seconds as f64 - elapsed_secs).max(0.0),
    };
    let text = format_timer_value(value, format);
    let mut rects = Vec::new();

    // Layout: digit 7-segment glyphs + colon dots + decimal dot.
    let digit_h = (height / 3).max(10);
    let digit_w = (digit_h * 3 / 5).max(1);
    let separator_w = (digit_w / 4).max(2); // colon and dot share a width
    let spacing = (digit_w / 4).max(1);

    // Two-pass: measure total width, then lay out.
    let total_w: u32 = text
        .chars()
        .map(|c| match c {
            ':' | '.' => separator_w + spacing,
            _ => digit_w + spacing,
        })
        .sum::<u32>()
        .saturating_sub(spacing);
    let start_x = width.saturating_sub(total_w) / 2;
    let start_y = height.saturating_sub(digit_h) / 2;

    let dot_size = separator_w;
    let dot = |out: &mut Vec<[u32; 4]>, x: u32, y: u32| push_rect(out, x, y, (x + dot_size).min(width), (y + dot_size).min(height));
    let mut cx = start_x;
    for ch in text.chars() {
        match ch {
            ':' => {
                // Two square dots, vertically stacked at 1/3 and 2/3 of digit_h.
                dot(&mut rects, cx, start_y + digit_h / 3 - dot_size / 2);
                dot(&mut rects, cx, start_y + (digit_h * 2) / 3 - dot_size / 2);
                cx += separator_w + spacing;
            }
            '.' => {
                // Single bottom-aligned square dot for decimal separator.
                dot(&mut rects, cx, start_y + digit_h - dot_size);
                cx += separator_w + spacing;
            }
            d if d.is_ascii_digit() => {
                seven_segment_rects(&mut rects, width, height, cx, start_y, digit_w, digit_h, digit_segments(d));
                cx += digit_w + spacing;
            }
            _ => {
                // Unknown char (shouldn't happen with format_timer_value's output)
                cx += digit_w + spacing;
            }
        }
    }
    Segments { background: background_color, color: text_color, rects }
}


#[cfg(test)]
mod tests {
    /// The half-float gradient draws the picture the eight-bit one draws.
    ///
    /// Both are sRGB-encoded, so a code value means the same thing in
    /// each: the f16 path must land within about a code value of the
    /// byte path, which is all the dither and the rounding can account
    /// for. Anything wider would mean they are drawing different
    /// gradients, not the same one at different precisions.
    #[test]
    fn the_f16_gradient_matches_the_eight_bit_one() {
        let (w, h) = (64u32, 48u32);
        let top = [4u8, 3, 12, 255];
        let bottom = [18u8, 12, 48, 255];
        for angle in [0.0f32, 25.0, 90.0, 200.0] {
            let bytes = render_gradient(top, bottom, angle, w, h);
            let halves = render_gradient_f16(top, bottom, angle, 0.0, w, h);
            let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32();
            let mut worst = 0.0f32;
            for (i, b) in bytes.chunks_exact(4).enumerate() {
                let (rg, ba) = (halves[i * 2], halves[i * 2 + 1]);
                let got = [un(rg & 0xFFFF), un(rg >> 16), un(ba & 0xFFFF)];
                for c in 0..3 {
                    worst = worst.max((got[c] * 255.0 - b[c] as f32).abs());
                }
            }
            assert!(worst <= 1.5, "angle {angle}: f16 and byte gradients differ by {worst} code values");
        }
    }

    /// And it is smoother than the thing it replaces: a shallow ramp the
    /// byte path can only draw in a dozen steps has hundreds in f16.
    /// The half-float four-point wash is the eight-bit one's picture
    /// (#190): within a code value, all the byte path's two roundings can
    /// account for, and the routing sends a four-point gradient to it.
    #[test]
    fn the_f16_four_point_gradient_matches_the_eight_bit_one() {
        let (w, h) = (64u32, 48u32);
        let corners = crate::FourPointGradient { top_left: [4, 3, 12, 255], top_right: [60, 20, 8, 255], bottom_left: [10, 40, 90, 255], bottom_right: [200, 180, 30, 200] };
        let bytes = super::render_four_point_gradient(corners, w, h);
        let half = super::render_four_point_gradient_f16(corners, 0.0, w, h);
        let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32() * 255.0;
        let mut worst = 0.0f32;
        for (b, p) in bytes.chunks_exact(4).zip(half.chunks_exact(2)) {
            let got = [un(p[0] & 0xFFFF), un(p[0] >> 16), un(p[1] & 0xFFFF), un(p[1] >> 16)];
            for c in 0..4 {
                worst = worst.max((got[c] - b[c] as f32).abs());
            }
        }
        assert!(worst <= 1.0, "{worst} code values from the byte wash");
        let data = crate::GeneratorData::Gradient { top: [0; 4], bottom: [0; 4], angle_deg: 0.0, four_point: Some(corners), grain: 0.0 };
        assert_eq!(super::render_generator_f16(&data, w, h, 0.0, 1.0), half, "the router draws it in float");
    }

    /// A shallow four-point wash steps where bytes round, not in half
    /// floats: across a row the f16 values take more distinct levels than
    /// the bytes do.
    #[test]
    fn the_f16_four_point_gradient_has_finer_steps_than_bytes() {
        let (w, h) = (512u32, 8u32);
        let corners = crate::FourPointGradient { top_left: [10, 10, 10, 255], top_right: [18, 18, 18, 255], bottom_left: [10, 10, 10, 255], bottom_right: [18, 18, 18, 255] };
        let bytes = super::render_four_point_gradient(corners, w, h);
        let half = super::render_four_point_gradient_f16(corners, 0.0, w, h);
        let row_bytes: std::collections::BTreeSet<u8> = bytes.chunks_exact(4).take(w as usize).map(|p| p[0]).collect();
        let row_half: std::collections::BTreeSet<u32> = half.chunks_exact(2).take(w as usize).map(|p| p[0] & 0xFFFF).collect();
        assert!(row_half.len() > row_bytes.len() * 4, "{} half-float levels against {} byte ones", row_half.len(), row_bytes.len());
    }

    /// The generator grain (#2073), in sRGB code values: what grained
    /// minus ungrained is at each pixel, the green channel of each.
    fn grain_codes(grained: &[u32], plain: &[u32]) -> Vec<f32> {
        let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32() * 255.0;
        grained.chunks_exact(2).zip(plain.chunks_exact(2)).map(|(g, p)| un(g[0] >> 16) - un(p[0] >> 16)).collect()
    }

    /// With its grain at 0 every smooth generator draws the picture it drew
    /// before there was any (#2073), to the bit: these hashes were taken
    /// from the same calls before the grain existed. The control: with
    /// grain the picture is another.
    #[test]
    fn without_grain_the_gradients_are_drawn_as_before() {
        let fnv = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        let (w, h) = (96u32, 54u32);
        let corners = crate::FourPointGradient { top_left: [4, 3, 12, 255], top_right: [60, 20, 8, 255], bottom_left: [10, 40, 90, 255], bottom_right: [200, 180, 30, 200] };
        let draw = |grain: f32| [
            render_gradient_f16([40, 70, 90, 255], [120, 140, 160, 255], 30.0, grain, w, h),
            render_four_point_gradient_f16(corners, grain, w, h),
            render_living_gradient_f16([12, 23, 32, 255], [29, 60, 65, 255], 25.0, 1.5, 0.18, grain, w, h, 1.25),
        ];
        let before = [0xaaa7_0b54_13f3_fe42u64, 0xaa82_a195_1015_ed51, 0xdac1_35ea_c4c9_1802];
        for ((what, drawn), want) in ["gradient", "four-point", "living gradient"].into_iter().zip(draw(0.0)).zip(before) {
            assert_eq!(fnv(&drawn), want, "{what}: grain 0 is not the picture drawn before");
        }
        for ((what, drawn), want) in ["gradient", "four-point", "living gradient"].into_iter().zip(draw(crate::DEFAULT_GENERATOR_GRAIN)).zip(before) {
            assert_ne!(fnv(&drawn), want, "the control: {what} with grain is another picture");
        }
    }

    /// The grain is as deep as asked, a bell about the ramp, on every
    /// smooth generator (#2073).
    #[test]
    fn the_grain_is_as_deep_as_asked() {
        let (w, h) = (256u32, 128u32);
        let (a, b) = ([100u8, 100, 100, 255], [140, 140, 140, 255]);
        for depth in [crate::DEFAULT_GENERATOR_GRAIN, 4.0] {
            let corners = crate::FourPointGradient { top_left: a, top_right: b, bottom_left: b, bottom_right: a };
            for (what, grained, plain) in [
                ("gradient", render_gradient_f16(a, b, 0.0, depth, w, h), render_gradient_f16(a, b, 0.0, 0.0, w, h)),
                ("four-point", render_four_point_gradient_f16(corners, depth, w, h), render_four_point_gradient_f16(corners, 0.0, w, h)),
                ("living gradient", render_living_gradient_f16(a, b, 0.0, 3.0, 0.3, depth, w, h, 1.5), render_living_gradient_f16(a, b, 0.0, 3.0, 0.3, 0.0, w, h, 1.5)),
            ] {
                let d = grain_codes(&grained, &plain);
                let mean = d.iter().sum::<f32>() / d.len() as f32;
                let std = (d.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / d.len() as f32).sqrt();
                assert!(mean.abs() < 0.05, "{what} at {depth}: the grain is centred, mean {mean}");
                assert!((std - depth).abs() < 0.1 * depth, "{what} at {depth}: {std} code values of grain");
            }
        }
    }

    /// The grain is fixed in place (#2073): a still gradient grains alike
    /// at every time, and a living one's grain at a pixel is the same on
    /// every frame, so a still frame doesn't flicker and the viewer and an
    /// export agree. The control: the living gradient itself moves.
    #[test]
    fn the_grain_holds_still() {
        let (w, h) = (64u32, 36u32);
        let (a, b) = ([30u8, 60, 90, 255], [90, 120, 160, 255]);
        let still = crate::GeneratorData::Gradient { top: a, bottom: b, angle_deg: 10.0, four_point: None, grain: crate::DEFAULT_GENERATOR_GRAIN };
        assert_eq!(render_generator_f16(&still, w, h, 0.0, 4.0), render_generator_f16(&still, w, h, 2.5, 4.0), "a still gradient: one picture");
        let grain_at = |t: f64| grain_codes(&render_living_gradient_f16(a, b, 0.0, 0.0, 0.0, 3.0, w, h, t), &render_living_gradient_f16(a, b, 0.0, 0.0, 0.0, 0.0, w, h, t));
        let (g0, g1) = (grain_at(0.0), grain_at(1.7));
        let worst = g0.iter().zip(&g1).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.2, "the living gradient's grain moved by {worst} code values");
        assert_eq!(render_living_gradient_f16(a, b, 0.0, 30.0, 0.2, 3.0, w, h, 0.5), render_living_gradient_f16(a, b, 0.0, 30.0, 0.2, 3.0, w, h, 0.5), "one time, one picture");
        assert_ne!(render_living_gradient_f16(a, b, 0.0, 30.0, 0.2, 3.0, w, h, 0.0), render_living_gradient_f16(a, b, 0.0, 30.0, 0.2, 3.0, w, h, 1.0), "the control: it moves");
    }

    #[test]
    fn the_f16_gradient_has_no_steps() {
        let (w, h) = (16u32, 512u32);
        let halves = render_gradient_f16([4, 3, 12, 255], [18, 12, 48, 255], 0.0, 0.0, w, h);
        let mut seen = std::collections::BTreeSet::new();
        for y in 0..h as usize {
            let ba = halves[(y * w as usize) * 2 + 1];
            let blue = half::f16::from_bits((ba & 0xFFFF) as u16).to_f32();
            seen.insert((blue * 255.0 * 64.0) as i64);
        }
        assert!(seen.len() > 200, "a half-float ramp down 512 rows should be near-continuous, got {} levels", seen.len());
    }

    use super::*;

    #[test]
    fn solid_color_produces_correct_bytes() {
        let buf = render_solid_color([255, 0, 0, 255], 10, 10);
        assert_eq!(buf.len(), 10 * 10 * 4);
        for pixel in buf.chunks_exact(4) {
            assert_eq!(pixel, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn solid_color_alpha_preserved() {
        let buf = render_solid_color([100, 200, 50, 128], 4, 4);
        assert_eq!(buf.len(), 64);
        for pixel in buf.chunks_exact(4) {
            assert_eq!(pixel, &[100, 200, 50, 128]);
        }
    }

    #[test]
    fn gradient_endpoints_correct() {
        // angle 0 = top→bottom (the historical default)
        let buf = render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 0.0, 1, 100);
        assert_eq!(buf.len(), 400);
        // First pixel (y=0): black
        assert_eq!(&buf[0..4], &[0, 0, 0, 255]);
        // Last pixel (y=99): white
        assert_eq!(&buf[396..400], &[255, 255, 255, 255]);
    }

    #[test]
    fn gradient_midpoint_interpolation() {
        let buf = render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 0.0, 1, 101);
        // Middle pixel at y=50
        let mid = &buf[50 * 4..50 * 4 + 4];
        // Should be approximately 127-128 due to rounding
        assert!((mid[0] as i16 - 128).abs() <= 1, "R={}", mid[0]);
        assert!((mid[1] as i16 - 128).abs() <= 1, "G={}", mid[1]);
        assert!((mid[2] as i16 - 128).abs() <= 1, "B={}", mid[2]);
        assert_eq!(mid[3], 255);
    }

    #[test]
    fn gradient_single_row_no_panic() {
        let buf = render_gradient([10, 20, 30, 255], [200, 100, 50, 128], 0.0, 10, 1);
        assert_eq!(buf.len(), 40);
        // height=1 + angle=0 → degenerate vertical: span_h=0, extent
        // collapses to 1, every pixel evaluates to t=0 → top colour.
        // Confirm no panic and that the start colour reads cleanly.
        assert_eq!(&buf[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn gradient_horizontal_angle_90() {
        // angle 90 = left→right: first pixel is `top`, last column is `bottom`.
        let buf = render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 90.0, 100, 1);
        assert_eq!(buf.len(), 400);
        assert_eq!(&buf[0..4], &[0, 0, 0, 255]);
        assert_eq!(&buf[396..400], &[255, 255, 255, 255]);
    }

    #[test]
    fn four_point_gradient_corners_match() {
        let corners = FourPointGradient {
            top_left: [255, 0, 0, 255],
            top_right: [0, 255, 0, 255],
            bottom_left: [0, 0, 255, 255],
            bottom_right: [255, 255, 0, 255],
        };
        let buf = render_four_point_gradient(corners, 4, 4);
        assert_eq!(buf.len(), 4 * 4 * 4);
        // Pixel (0,0) — top-left
        assert_eq!(&buf[0..4], &[255, 0, 0, 255]);
        // Pixel (3,0) — top-right
        let i = 3 * 4;
        assert_eq!(&buf[i..i + 4], &[0, 255, 0, 255]);
        // Pixel (0,3) — bottom-left
        let i = (3 * 4) * 4;
        assert_eq!(&buf[i..i + 4], &[0, 0, 255, 255]);
        // Pixel (3,3) — bottom-right
        let i = (3 * 4 + 3) * 4;
        assert_eq!(&buf[i..i + 4], &[255, 255, 0, 255]);
    }

    #[test]
    fn color_bars_seven_bars() {
        let buf = render_color_bars(700, 100);
        assert_eq!(buf.len(), 700 * 100 * 4);
        // First bar (x=0..99) should be white (191,191,191,255)
        assert_eq!(&buf[0..4], &[191, 191, 191, 255]);
        // Last bar (x=600..699) should be blue (0,0,191,255)
        let last_pixel = 650 * 4; // middle of last bar, first row
        assert_eq!(&buf[last_pixel..last_pixel + 4], &[0, 0, 191, 255]);
    }

    #[test]
    fn color_bars_narrow_frame() {
        let buf = render_color_bars(7, 10);
        assert_eq!(buf.len(), 7 * 10 * 4);
        // Each pixel should be one bar color
        assert_eq!(&buf[0..4], &[191, 191, 191, 255]); // x=0: white
        assert_eq!(&buf[24..28], &[0, 0, 191, 255]);   // x=6: blue
    }

    #[test]
    fn color_bars_very_narrow_no_panic() {
        let buf = render_color_bars(3, 10);
        assert_eq!(buf.len(), 3 * 10 * 4);
    }

    #[test]
    fn countdown_seconds_calculation() {
        // At elapsed=0 of a 5-second clip, remaining=5
        let remaining = ((5.0 - 0.0_f64).ceil() as i64).max(1);
        assert_eq!(remaining, 5);
        // At elapsed=2.5, remaining=3
        let remaining = ((5.0 - 2.5_f64).ceil() as i64).max(1);
        assert_eq!(remaining, 3);
        // At elapsed=4.9, remaining=1
        let remaining = ((5.0 - 4.9_f64).ceil() as i64).max(1);
        assert_eq!(remaining, 1);
    }

    #[test]
    fn all_generators_produce_correct_byte_count() {
        let expected = 640 * 480 * 4;
        assert_eq!(render_solid_color([0, 0, 0, 255], 640, 480).len(), expected);
        assert_eq!(
            render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 0.0, 640, 480).len(),
            expected,
        );
        assert_eq!(render_color_bars(640, 480).len(), expected);
        assert_eq!(render_color_bars_100(640, 480).len(), expected);
        assert_eq!(render_countdown(640, 480, 2.0, 10.0).len(), expected);
        assert_eq!(
            render_timer(640, 480, 0.0, TimerMode::CountUp, 0.0, TimerFormat::MmSs, [255; 4], [0; 4]).len(),
            expected,
        );
        assert_eq!(render_polygon(6, [255, 0, 0, 255], 0.0, true, 640, 480).len(), expected);
        assert_eq!(render_star(5, 0.4, [255, 255, 0, 255], 0.0, true, 640, 480).len(), expected);
    }

    #[test]
    fn polygon_centre_pixel_is_filled() {
        // Triangle at the frame centre — the centre pixel must be
        // inside the polygon for any sides ≥ 3.
        for sides in [3u32, 4, 5, 6, 8, 12] {
            let buf = render_polygon(sides, [255, 255, 255, 255], 0.0, true, 64, 64);
            let centre_idx = (32 * 64 + 32) * 4;
            assert_eq!(
                &buf[centre_idx..centre_idx + 4],
                &[255, 255, 255, 255],
                "centre pixel missing for {sides}-gon",
            );
        }
    }

    #[test]
    fn polygon_corner_pixel_is_transparent() {
        // 40 % radius means the polygon never touches the frame
        // corners — they must stay alpha=0 (background).
        let buf = render_polygon(6, [255, 0, 0, 255], 0.0, true, 64, 64);
        let corner_idx = 0;
        assert_eq!(buf[corner_idx + 3], 0, "(0,0) should be transparent");
    }

    #[test]
    fn polygon_outline_only_leaves_interior_transparent() {
        // fill = false + non-zero outline → only the stroke is
        // drawn; the centre stays transparent.
        let buf = render_polygon(6, [255, 0, 0, 255], 2.0, false, 64, 64);
        let centre_idx = (32 * 64 + 32) * 4;
        assert_eq!(buf[centre_idx + 3], 0, "centre should be transparent in outline-only mode");
    }

    #[test]
    fn star_centre_pixel_is_filled() {
        let buf = render_star(5, 0.4, [255, 255, 0, 255], 0.0, true, 64, 64);
        let centre_idx = (32 * 64 + 32) * 4;
        assert_eq!(&buf[centre_idx..centre_idx + 4], &[255, 255, 0, 255]);
    }

    #[test]
    fn star_inner_radius_clamped() {
        // inner_radius_pct = 0 should clamp to 0.05 internally
        // (a degenerate zero-inner star would collapse to a circle
        // and produce visual artefacts). Test via rendered byte
        // count + non-empty fill — implementation detail of the
        // clamp, but the contract is "no crash + visible shape".
        let buf = render_star(5, 0.0, [255, 255, 255, 255], 0.0, true, 64, 64);
        let centre_idx = (32 * 64 + 32) * 4;
        assert_eq!(buf[centre_idx + 3], 255);
    }

    #[test]
    fn polygon_sides_below_3_falls_back() {
        // sides=0 / sides=1 / sides=2 should not crash; render
        // SOMETHING (treated as a triangle).
        for bad in [0u32, 1, 2] {
            let buf = render_polygon(bad, [255, 255, 255, 255], 0.0, true, 32, 32);
            assert_eq!(buf.len(), 32 * 32 * 4);
            // Centre pixel filled (means a triangle was drawn).
            let centre_idx = (16 * 32 + 16) * 4;
            assert_eq!(buf[centre_idx + 3], 255, "sides={bad} should fall back to a triangle");
        }
    }

    #[test]
    fn still_frame_missing_file_returns_magenta() {
        let result = render_generator(
            &GeneratorData::StillFrame {
                png_path: std::path::PathBuf::from("/nonexistent/freeze.png"),
                width: 4,
                height: 4,
            },
            4,
            4,
            0.0,
            1.0,
        );
        assert_eq!(result.len(), 4 * 4 * 4);
        // Every pixel should be magenta.
        for pixel in result.chunks_exact(4) {
            assert_eq!(pixel, &[255u8, 0, 255, 255]);
        }
    }

    // ── Circle and full bars (#2775) ────────────────────────────────

    /// Where the edge of a shape drawn in the middle of a frame lies along
    /// `deg` from the centre: the first pixel under half coverage.
    fn edge_at(buf: &[u8], w: u32, h: u32, deg: f32) -> f32 {
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        let (s, c) = deg.to_radians().sin_cos();
        (0..(h / 2)).map(|r| r as f32).find(|r| {
            let (x, y) = ((cx + c * r) as u32, (cy + s * r) as u32);
            buf[((y * w + x) * 4 + 3) as usize] < 128
        }).unwrap_or(0.0)
    }

    #[test]
    fn a_circle_is_round_at_full_frame() {
        let (w, h) = (1920, 1080);
        let reach = |sides: u32| {
            let buf = render_polygon(sides, [255; 4], 0.0, true, w, h);
            let r: Vec<f32> = (0..360).map(|d| edge_at(&buf, w, h, d as f32)).collect();
            r.iter().cloned().fold(f32::MIN, f32::max) - r.iter().cloned().fold(f32::MAX, f32::min)
        };
        // A 24-gon is a few pixels out of round at this size; the circle
        // within the pixel grid's own step.
        assert!(reach(24) > 2.5, "the 24-gon's facets: {} px", reach(24));
        assert!(reach(CIRCLE_SIDES) <= 1.5, "the circle: {} px out of round", reach(CIRCLE_SIDES));
        // Its edge is smoothed: partly covered pixels round it.
        let buf = render_polygon(CIRCLE_SIDES, [255; 4], 0.0, true, w, h);
        assert!(buf.chunks(4).any(|p| p[3] > 0 && p[3] < 255), "an anti-aliased edge");
        // As a ring, the inside is clear.
        let ring = render_polygon(CIRCLE_SIDES, [255; 4], 8.0, false, w, h);
        assert_eq!(ring[((540 * w + 960) * 4 + 3) as usize], 0, "a ring's middle is clear");
    }

    #[test]
    fn the_full_smpte_bars_have_their_lower_rows() {
        let (w, h) = (1400u32, 600u32);
        let buf = render_color_bars_smpte(w, h);
        let px = |fx: f32, fy: f32| {
            let i = (((fy * h as f32) as u32 * w + (fx * w as f32) as u32) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        let bar = |k: f32| (k + 0.5) / 7.0;
        // The seven 75 % bars on top.
        assert_eq!(px(bar(0.0), 0.3), [191, 191, 191]);
        assert_eq!(px(bar(6.0), 0.3), [0, 0, 191]);
        // The castellations: blue under white, black under yellow, white
        // under blue.
        assert_eq!(px(bar(0.0), 0.7), [0, 0, 191]);
        assert_eq!(px(bar(1.0), 0.7), [0, 0, 0]);
        assert_eq!(px(bar(6.0), 0.7), [191, 191, 191]);
        // The bottom row: −I, 100 % white, +Q, then PLUGE's 4 % step.
        assert_eq!(px(0.5 / 7.0, 0.9), [0, 33, 76]);
        assert_eq!(px(1.9 / 7.0, 0.9), [255, 255, 255]);
        assert_eq!(px(3.1 / 7.0, 0.9), [50, 0, 106]);
        assert_eq!(px(5.8 / 7.0, 0.9), [10, 10, 10]);
        assert_eq!(px(6.5 / 7.0, 0.9), [0, 0, 0]);
    }

    // ── ColorBars100 ────────────────────────────────────────────────

    #[test]
    fn color_bars_100_first_bar_is_full_white() {
        let buf = render_color_bars_100(700, 100);
        assert_eq!(buf.len(), 700 * 100 * 4);
        // First bar (x=0..99) should be full-intensity white.
        assert_eq!(&buf[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn color_bars_100_last_bar_is_full_blue() {
        let buf = render_color_bars_100(700, 100);
        let last_pixel = 650 * 4;
        assert_eq!(&buf[last_pixel..last_pixel + 4], &[0, 0, 255, 255]);
    }

    #[test]
    fn color_bars_75_and_100_differ_in_intensity() {
        let buf75 = render_color_bars(700, 1);
        let buf100 = render_color_bars_100(700, 1);
        // Both seven-bar layouts; first pixel is white in both, but
        // 75 % is dimmer (191) than 100 % (255).
        assert_eq!(buf75[0], 191);
        assert_eq!(buf100[0], 255);
    }

    // ── Timer ───────────────────────────────────────────────────────

    #[test]
    fn timer_format_seconds_basic() {
        assert_eq!(format_timer_value(0.0, TimerFormat::Seconds), "0");
        assert_eq!(format_timer_value(45.7, TimerFormat::Seconds), "45");
        assert_eq!(format_timer_value(3661.0, TimerFormat::Seconds), "3661");
    }

    #[test]
    fn timer_format_mm_ss_pads_to_two_digits() {
        assert_eq!(format_timer_value(0.0, TimerFormat::MmSs), "00:00");
        assert_eq!(format_timer_value(7.0, TimerFormat::MmSs), "00:07");
        assert_eq!(format_timer_value(125.0, TimerFormat::MmSs), "02:05");
        // Over 100 minutes — minutes overflow the 2-digit pad cleanly.
        assert_eq!(format_timer_value(7200.0, TimerFormat::MmSs), "120:00");
    }

    #[test]
    fn timer_format_hh_mm_ss_pads_all_groups() {
        assert_eq!(format_timer_value(0.0, TimerFormat::HhMmSs), "00:00:00");
        assert_eq!(format_timer_value(3661.0, TimerFormat::HhMmSs), "01:01:01");
        assert_eq!(format_timer_value(86399.0, TimerFormat::HhMmSs), "23:59:59");
    }

    #[test]
    fn timer_format_with_milliseconds() {
        let s = format_timer_value(1.234, TimerFormat::HhMmSsMs);
        assert_eq!(s, "00:00:01.234");
    }

    #[test]
    fn timer_clamps_negative_to_zero() {
        // Negative input (e.g. count-down past zero before the call
        // site clamps it) yields all-zeros, not a panic or weird wraparound.
        assert_eq!(format_timer_value(-100.0, TimerFormat::MmSs), "00:00");
    }

    #[test]
    fn timer_count_up_advances_with_elapsed() {
        // Count-up at 5 s elapsed with initial=10 → display = 15 s.
        let buf = render_timer(
            640, 480, 5.0,
            TimerMode::CountUp, 10.0,
            TimerFormat::Seconds,
            [255; 4], [0; 4],
        );
        assert_eq!(buf.len(), 640 * 480 * 4);
        // Just confirm it didn't panic + produced the expected size.
        // Per-pixel correctness on rendered text is fragile to test;
        // format_timer_value tests cover the value math.
    }

    #[test]
    fn timer_count_down_clamps_at_zero() {
        // Count-down with initial=10 elapsed=20 → display would be -10 s,
        // clamps to 0. Render must not panic.
        let buf = render_timer(
            640, 480, 20.0,
            TimerMode::CountDown, 10.0,
            TimerFormat::MmSs,
            [255; 4], [0; 4],
        );
        assert_eq!(buf.len(), 640 * 480 * 4);
    }

    #[test]
    fn timer_paints_background_color() {
        // A pixel far from the digit-render area should match the
        // background colour. Use a distinctive dark blue.
        let bg = [10u8, 20, 80, 255];
        let buf = render_timer(
            64, 48, 0.0,
            TimerMode::CountUp, 0.0,
            TimerFormat::MmSs,
            [255; 4], bg,
        );
        // Top-left corner — well outside the centred digit glyph area.
        assert_eq!(&buf[0..4], &bg);
    }

    #[test]
    fn timer_default_mode_is_count_up() {
        assert_eq!(TimerMode::default(), TimerMode::CountUp);
    }

    #[test]
    fn timer_default_format_is_mm_ss() {
        assert_eq!(TimerFormat::default(), TimerFormat::MmSs);
    }
}

#[cfg(test)]
mod living_generators_tests {
    use super::*;

    fn luma_mean(px: &[u8]) -> f32 {
        let n = (px.len() / 4) as f32;
        px.chunks_exact(4).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).sum::<f32>() / n
    }

    #[test]
    fn grain_is_noise_around_the_colour_and_changes_between_frames() {
        let a = render_grain([100, 100, 100, 255], 0.5, 1.0, 24.0, 64, 36, 0.0);
        let b = render_grain([100, 100, 100, 255], 0.5, 1.0, 24.0, 64, 36, 0.5);
        assert_ne!(a, b, "a new grain frame each tick");
        assert!((luma_mean(&a) - 100.0).abs() < 6.0, "grain averages to the colour");
        let flat = render_grain([100, 100, 100, 255], 0.0, 1.0, 24.0, 64, 36, 0.0);
        assert!(flat.chunks_exact(4).all(|p| p[0] == 100), "no amount, no grain");
    }

    #[test]
    fn a_living_gradient_moves_and_its_light_brightens() {
        let t0 = render_living_gradient([0, 0, 80, 255], [80, 0, 40, 255], 0.0, 10.0, 0.0, 64, 36, 0.0);
        let t1 = render_living_gradient([0, 0, 80, 255], [80, 0, 40, 255], 0.0, 10.0, 0.0, 64, 36, 2.0);
        assert_ne!(t0, t1, "the angle drifts");
        let lit = render_living_gradient([0, 0, 80, 255], [80, 0, 40, 255], 0.0, 0.0, 0.6, 64, 36, 0.0);
        assert!(luma_mean(&lit) > luma_mean(&t0), "the light adds to the picture");
    }

    #[test]
    fn a_placeholder_backdrop_is_hatched() {
        let px = render_placeholder_backdrop([100, 100, 100, 255], 64, 36);
        let distinct: std::collections::HashSet<u8> = px.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(distinct.len(), 2, "the colour and its darker stripe");
    }
}
