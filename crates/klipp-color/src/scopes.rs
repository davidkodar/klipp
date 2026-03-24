/// Waveform data: per-column luma histogram with 256 bins.
pub struct WaveformData {
    /// One [u32; 256] histogram per display column.
    pub bins: Vec<[u32; 256]>,
    pub width: u32,
}

/// Vectorscope data: list of (Cb, Cr) points in -0.5..0.5 range.
pub struct VectorscopeData {
    pub points: Vec<(f32, f32)>,
}

/// RGB + Luma histogram data: 256 bins per channel.
pub struct HistogramData {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    pub luma: [u32; 256],
}

/// Compute a luma waveform from RGBA data.
/// `display_columns` controls horizontal resolution of the output.
/// Each column accumulates luma values from the corresponding source columns.
pub fn compute_waveform(rgba: &[u8], width: u32, height: u32, display_columns: u32) -> WaveformData {
    let display_columns = display_columns.max(1).min(width);
    let mut bins = vec![[0u32; 256]; display_columns as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
            let bin = (luma * 255.0) as usize;
            let col = (x as u64 * display_columns as u64 / width as u64) as usize;
            bins[col][bin] += 1;
        }
    }

    WaveformData { bins, width: display_columns }
}

/// Compute a vectorscope from RGBA data using BT.709 YCbCr.
/// Samples every 4th pixel for performance.
/// Returns (Cb, Cr) points centered at 0 (range -0.5..0.5).
pub fn compute_vectorscope(rgba: &[u8], width: u32, height: u32) -> VectorscopeData {
    let sample_step = 4usize;
    let estimated = (width as usize / sample_step) * (height as usize / sample_step);
    let mut points = Vec::with_capacity(estimated);

    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            // BT.709 YCbCr (Cb/Cr centered at 0, not 0.5)
            let cb = -0.1146 * r - 0.3854 * g + 0.5000 * b;
            let cr =  0.5000 * r - 0.4542 * g - 0.0458 * b;
            points.push((cb, cr));
        }
    }

    VectorscopeData { points }
}

/// Compute RGB and luma histograms from RGBA data.
/// Samples every 2nd pixel for performance.
pub fn compute_histogram(rgba: &[u8], width: u32, height: u32) -> HistogramData {
    let mut data = HistogramData {
        r: [0u32; 256],
        g: [0u32; 256],
        b: [0u32; 256],
        luma: [0u32; 256],
    };

    let sample_step = 2usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let rv = rgba[idx];
            let gv = rgba[idx + 1];
            let bv = rgba[idx + 2];
            data.r[rv as usize] += 1;
            data.g[gv as usize] += 1;
            data.b[bv as usize] += 1;
            let luma = (0.2126 * rv as f32 + 0.7152 * gv as f32 + 0.0722 * bv as f32)
                .clamp(0.0, 255.0) as usize;
            data.luma[luma] += 1;
        }
    }

    data
}

/// RGB Parade data: per-column R/G/B histograms (256 bins each).
pub struct ParadeData {
    pub red:   Vec<[u32; 256]>,
    pub green: Vec<[u32; 256]>,
    pub blue:  Vec<[u32; 256]>,
    pub width: u32,
}

/// Compute an RGB parade from RGBA data.
/// Each channel gets its own per-column histogram.
pub fn compute_parade(rgba: &[u8], width: u32, height: u32, display_columns: u32) -> ParadeData {
    let display_columns = display_columns.max(1).min(width);
    let mut red   = vec![[0u32; 256]; display_columns as usize];
    let mut green = vec![[0u32; 256]; display_columns as usize];
    let mut blue  = vec![[0u32; 256]; display_columns as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let col = (x as u64 * display_columns as u64 / width as u64) as usize;
            red[col][rgba[idx] as usize] += 1;
            green[col][rgba[idx + 1] as usize] += 1;
            blue[col][rgba[idx + 2] as usize] += 1;
        }
    }

    ParadeData { red, green, blue, width: display_columns }
}

/// Like compute_waveform but samples every `step`-th COLUMN (ALL rows processed).
/// This eliminates horizontal banding while staying fast.
pub fn compute_waveform_sampled(rgba: &[u8], width: u32, height: u32, display_columns: u32, step: u32) -> WaveformData {
    let x_step = step.max(1) as usize;
    let display_columns = display_columns.max(1) as usize;
    let mut bins = vec![[0u32; 256]; display_columns];
    let sampled_w = (width as usize + x_step - 1) / x_step;

    // Process ALL rows — only subsample columns
    for y in 0..height as usize {
        for x in (0..width as usize).step_by(x_step) {
            let idx = (y * width as usize + x) * 4;
            if idx + 2 >= rgba.len() { break; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
            let bin = (luma * 255.0) as usize;
            let col = ((x / x_step) as u64 * display_columns as u64 / sampled_w.max(1) as u64) as usize;
            if col < display_columns {
                bins[col][bin] += 1;
            }
        }
    }

    WaveformData { bins, width: display_columns as u32 }
}

/// Like compute_histogram but samples every `step`-th pixel.
pub fn compute_histogram_sampled(rgba: &[u8], width: u32, height: u32, step: u32) -> HistogramData {
    let mut data = HistogramData {
        r: [0u32; 256], g: [0u32; 256], b: [0u32; 256], luma: [0u32; 256],
    };
    let step = step.max(1) as usize;
    for y in (0..height as usize).step_by(step) {
        for x in (0..width as usize).step_by(step) {
            let idx = (y * width as usize + x) * 4;
            if idx + 2 >= rgba.len() { break; }
            let rv = rgba[idx];
            let gv = rgba[idx + 1];
            let bv = rgba[idx + 2];
            data.r[rv as usize] += 1;
            data.g[gv as usize] += 1;
            data.b[bv as usize] += 1;
            let luma = (0.2126 * rv as f32 + 0.7152 * gv as f32 + 0.0722 * bv as f32)
                .clamp(0.0, 255.0) as usize;
            data.luma[luma] += 1;
        }
    }
    data
}

/// Like compute_parade but samples every `step`-th COLUMN (ALL rows processed).
pub fn compute_parade_sampled(rgba: &[u8], width: u32, height: u32, display_columns: u32, step: u32) -> ParadeData {
    let x_step = step.max(1) as usize;
    let display_columns = display_columns.max(1) as usize;
    let mut red   = vec![[0u32; 256]; display_columns];
    let mut green = vec![[0u32; 256]; display_columns];
    let mut blue  = vec![[0u32; 256]; display_columns];
    let sampled_w = (width as usize + x_step - 1) / x_step;

    // Process ALL rows — only subsample columns
    for y in 0..height as usize {
        for x in (0..width as usize).step_by(x_step) {
            let idx = (y * width as usize + x) * 4;
            if idx + 2 >= rgba.len() { break; }
            let col = ((x / x_step) as u64 * display_columns as u64 / sampled_w.max(1) as u64) as usize;
            if col < display_columns {
                red[col][rgba[idx] as usize] += 1;
                green[col][rgba[idx + 1] as usize] += 1;
                blue[col][rgba[idx + 2] as usize] += 1;
            }
        }
    }

    ParadeData { red, green, blue, width: display_columns as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_color_frame(r: u8, g: u8, b: u8, w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            data.extend_from_slice(&[r, g, b, 255]);
        }
        data
    }

    #[test]
    fn waveform_single_color() {
        let frame = solid_color_frame(128, 128, 128, 16, 16);
        let wf = compute_waveform(&frame, 16, 16, 8);
        assert_eq!(wf.bins.len(), 8);
        // All pixels should land in the same bin
        let total: u32 = wf.bins.iter().map(|col| col.iter().sum::<u32>()).sum();
        assert_eq!(total, 16 * 16);
    }

    #[test]
    fn vectorscope_neutral_at_center() {
        let frame = solid_color_frame(128, 128, 128, 16, 16);
        let vs = compute_vectorscope(&frame, 16, 16);
        assert!(!vs.points.is_empty());
        for (cb, cr) in &vs.points {
            assert!(cb.abs() < 0.01, "Cb should be ~0 for grey, got {cb}");
            assert!(cr.abs() < 0.01, "Cr should be ~0 for grey, got {cr}");
        }
    }

    #[test]
    fn histogram_single_value() {
        let frame = solid_color_frame(100, 150, 200, 16, 16);
        let hist = compute_histogram(&frame, 16, 16);
        // With step_by(2), we sample 8*8 = 64 pixels
        assert_eq!(hist.r[100], 64);
        assert_eq!(hist.g[150], 64);
        assert_eq!(hist.b[200], 64);
    }
}
