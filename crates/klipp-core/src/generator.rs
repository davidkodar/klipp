//! Generator frame rendering — produces RGBA8 pixel buffers for each GeneratorData variant.

use crate::{FourPointGradient, GeneratorData};

/// Render a generator frame at the given resolution.
/// `elapsed_secs` and `total_secs` are only used by Countdown.
pub fn render_generator(
    data: &GeneratorData,
    width: u32,
    height: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u8> {
    match data {
        GeneratorData::SolidColor { color } => render_solid_color(*color, width, height),
        GeneratorData::Gradient { top, bottom, angle_deg, four_point } => {
            if let Some(corners) = four_point {
                render_four_point_gradient(*corners, width, height)
            } else {
                render_gradient(*top, *bottom, *angle_deg, width, height)
            }
        }
        GeneratorData::ColorBars => render_color_bars(width, height),
        GeneratorData::Countdown => render_countdown(width, height, elapsed_secs, total_secs),
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
    let n = sides.max(3) as usize;
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let radius = (width.min(height) as f32) * 0.40;
    let verts = polygon_vertices(n, cx, cy, radius);
    rasterise_polygon(&verts, color, outline_width_px, fill, width, height)
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
            buf[i] = lerp_u8(top[0], bottom[0], t);
            buf[i + 1] = lerp_u8(top[1], bottom[1], t);
            buf[i + 2] = lerp_u8(top[2], bottom[2], t);
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

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Render SMPTE 75% color bars (seven vertical bars of equal width).
pub fn render_color_bars(width: u32, height: u32) -> Vec<u8> {
    // SMPTE 75% bar colors in RGBA8 order: white, yellow, cyan, green, magenta, red, blue.
    const BARS: [[u8; 4]; 7] = [
        [191, 191, 191, 255], // white
        [191, 191, 0, 255],   // yellow
        [0, 191, 191, 255],   // cyan
        [0, 191, 0, 255],     // green
        [191, 0, 191, 255],   // magenta
        [191, 0, 0, 255],     // red
        [0, 0, 191, 255],     // blue
    ];
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = Vec::with_capacity(pixel_count * 4);
    for _y in 0..height {
        for x in 0..width {
            let bar_idx =
                ((x as u64 * BARS.len() as u64) / width.max(1) as u64) as usize;
            let color = BARS[bar_idx.min(BARS.len() - 1)];
            buf.extend_from_slice(&color);
        }
    }
    buf
}

/// Render a countdown frame: white number on black background.
/// Shows seconds remaining, counting down from total_secs.
pub fn render_countdown(
    width: u32,
    height: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u8> {
    let seconds_remaining = ((total_secs - elapsed_secs).ceil() as i64).max(1);
    let digit = format!("{}", seconds_remaining);

    // Start with black background
    let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];

    // Render digit as a simple bitmap font — each digit is drawn as filled rectangles.
    // This avoids requiring a font library for a single generator variant.
    let digit_h = (height / 3).max(10);
    let digit_w = digit_h * 3 / 5;
    let total_w = digit.len() as u32 * (digit_w + digit_w / 4).max(1);
    let start_x = (width.saturating_sub(total_w)) / 2;
    let start_y = (height.saturating_sub(digit_h)) / 2;

    for (char_idx, ch) in digit.chars().enumerate() {
        let cx = start_x + char_idx as u32 * (digit_w + digit_w / 4);
        let segments = digit_segments(ch);
        draw_seven_segment(&mut buf, width, cx, start_y, digit_w, digit_h, segments);
    }
    buf
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

/// Draw a seven-segment digit into an RGBA buffer.
fn draw_seven_segment(
    buf: &mut [u8],
    buf_width: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    segments: [bool; 7],
) {
    let thickness = (h / 8).max(2);
    let half_h = h / 2;
    let white = [255u8, 255, 255, 255];

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
        if !segments[i] {
            continue;
        }
        for py in ry..(ry + rh).min(buf_width / 4 * 4) {
            // Guard against out-of-bounds
            let row_start = (py as usize) * (buf_width as usize) * 4;
            for px in rx..(rx + rw) {
                if px < buf_width {
                    let offset = row_start + (px as usize) * 4;
                    if offset + 3 < buf.len() {
                        buf[offset..offset + 4].copy_from_slice(&white);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(render_countdown(640, 480, 2.0, 10.0).len(), expected);
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
}
