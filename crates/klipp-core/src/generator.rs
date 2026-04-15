//! Generator frame rendering — produces RGBA8 pixel buffers for each GeneratorData variant.

use crate::GeneratorData;

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
        GeneratorData::Gradient { top, bottom } => render_gradient(*top, *bottom, width, height),
        GeneratorData::ColorBars => render_color_bars(width, height),
        GeneratorData::Countdown => render_countdown(width, height, elapsed_secs, total_secs),
        GeneratorData::StillFrame { png_path, .. } => render_still_frame(png_path, width, height),
    }
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

/// Render a vertical gradient between two colors.
/// `top` is the color at y=0, `bottom` is the color at y=height-1.
pub fn render_gradient(top: [u8; 4], bottom: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut buf = Vec::with_capacity(pixel_count * 4);
    for y in 0..height {
        let t = if height <= 1 {
            0.0
        } else {
            y as f32 / (height - 1) as f32
        };
        let pixel = [
            lerp_u8(top[0], bottom[0], t),
            lerp_u8(top[1], bottom[1], t),
            lerp_u8(top[2], bottom[2], t),
            lerp_u8(top[3], bottom[3], t),
        ];
        for _ in 0..width {
            buf.extend_from_slice(&pixel);
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
        let buf = render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 1, 100);
        assert_eq!(buf.len(), 400);
        // First pixel (y=0): black
        assert_eq!(&buf[0..4], &[0, 0, 0, 255]);
        // Last pixel (y=99): white
        assert_eq!(&buf[396..400], &[255, 255, 255, 255]);
    }

    #[test]
    fn gradient_midpoint_interpolation() {
        let buf = render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 1, 101);
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
        let buf = render_gradient([10, 20, 30, 255], [200, 100, 50, 128], 10, 1);
        assert_eq!(buf.len(), 40);
        // height=1 → t=0.0 → should be top color
        assert_eq!(&buf[0..4], &[10, 20, 30, 255]);
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
        assert_eq!(render_gradient([0, 0, 0, 255], [255, 255, 255, 255], 640, 480).len(), expected);
        assert_eq!(render_color_bars(640, 480).len(), expected);
        assert_eq!(render_countdown(640, 480, 2.0, 10.0).len(), expected);
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
