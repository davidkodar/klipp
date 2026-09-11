use std::path::Path;

use klipp_core::WorkingColorspace;

/// 3D LUT data — trilinear interpolation lookup table.
///
/// `input_space` and `output_space` describe what colorspace the
/// LUT's domain (input) and range (output) live in. Most legacy
/// `.cube` files were authored in BT.709 / sRGB and don't carry
/// metadata; they're loaded as `Rec709Linear` on both sides by
/// default. ACES-AP1-tagged LUTs that arrive without metadata can
/// be re-tagged via the Inspector dropdown. When the active project
/// working space differs from the LUT's input or output space, the
/// applicator brackets the LUT with the appropriate primaries-
/// conversion matrices (see [`apply_3d_lut_linear_ws`]).
#[derive(Debug, Clone)]
pub struct LutData {
    pub size: usize,
    pub data: Vec<f32>, // r,g,b interleaved, size^3 * 3 floats
    /// Colorspace of the LUT's input domain (the values you index in
    /// with). Defaults to [`WorkingColorspace::Rec709Linear`] for the
    /// existing "untagged .cube" convention.
    pub input_space: WorkingColorspace,
    /// Colorspace of the LUT's output range. Same default.
    pub output_space: WorkingColorspace,
}

impl LutData {
    /// Identity LUT of the given size (each grid point maps to its own
    /// position in the unit cube). Used as the passthrough fallback when
    /// `load_lut` fails — see `klipp-nodes/src/executor.rs` Lut3D paths.
    /// `size = 2` is the minimum (the 8 corners of the unit cube); since
    /// identity is linear, trilinear interpolation between the corners
    /// reproduces the identity exactly.
    ///
    /// Identity LUTs are colorspace-neutral (they don't change the
    /// signal), so they default to `Rec709Linear` on both sides — same
    /// as the parser-loaded default.
    pub fn identity(size: usize) -> Self {
        let mut data = Vec::with_capacity(size * size * size * 3);
        let n1 = (size.saturating_sub(1).max(1)) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(r as f32 / n1);
                    data.push(g as f32 / n1);
                    data.push(b as f32 / n1);
                }
            }
        }
        LutData {
            size,
            data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        }
    }
}

#[derive(Debug)]
pub enum LutError {
    IoError(std::io::Error),
    MissingSize,
    InvalidSize(usize),
    NotEnoughEntries { expected: usize, got: usize },
    ParseFloat { line_num: usize, text: String },
}

impl std::fmt::Display for LutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "IO error: {e}"),
            Self::MissingSize => write!(f, "missing LUT_3D_SIZE"),
            Self::InvalidSize(s) => write!(f, "invalid LUT size: {s}"),
            Self::NotEnoughEntries { expected, got } => {
                write!(f, "expected {expected} entries, got {got}")
            }
            Self::ParseFloat { line_num, text } => {
                write!(f, "line {line_num}: cannot parse float: {text}")
            }
        }
    }
}

impl From<std::io::Error> for LutError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

/// Parse an Adobe .cube 3D LUT file.
pub fn load_cube(path: &Path) -> Result<LutData, LutError> {
    let text = std::fs::read_to_string(path)?;
    parse_cube(&text)
}

/// Parse .cube format from a string (testable without filesystem).
pub fn parse_cube(text: &str) -> Result<LutData, LutError> {
    let mut size: Option<usize> = None;
    let mut data = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("TITLE") || line.starts_with("DOMAIN_MIN") || line.starts_with("DOMAIN_MAX") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
            let n: usize = rest.trim().parse().map_err(|_| LutError::ParseFloat {
                line_num: line_idx + 1,
                text: rest.trim().to_string(),
            })?;
            if !(2..=256).contains(&n) {
                return Err(LutError::InvalidSize(n));
            }
            size = Some(n);
            continue;
        }
        // Skip any other keyword lines (LUT_1D_SIZE, etc.)
        if line.contains('_') && line.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            continue;
        }

        // Data line: three floats
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            for (i, part) in parts.iter().take(3).enumerate() {
                let v: f32 = part.parse().map_err(|_| LutError::ParseFloat {
                    line_num: line_idx + 1,
                    text: parts[i].to_string(),
                })?;
                data.push(v);
            }
        }
    }

    let size = size.ok_or(LutError::MissingSize)?;
    let expected = size * size * size;
    let got = data.len() / 3;
    if got < expected {
        return Err(LutError::NotEnoughEntries { expected, got });
    }
    // Truncate to exact size (ignore extra)
    data.truncate(expected * 3);

    Ok(LutData {
        size,
        data,
        // Untagged .cube/.csp/.3dl files are conventionally authored
        // in BT.709 / sRGB. Users override via the Inspector when
        // they know the LUT was authored in a different space.
        input_space: WorkingColorspace::Rec709Linear,
        output_space: WorkingColorspace::Rec709Linear,
    })
}

/// Load any supported LUT format by file extension (.cube, .3dl, .csp).
pub fn load_lut(path: &Path) -> Result<LutData, LutError> {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let text = std::fs::read_to_string(path)?;
    match ext.as_str() {
        "cube" => parse_cube(&text),
        "3dl" => parse_3dl(&text),
        "csp" => parse_csp(&text),
        _ => parse_cube(&text), // fallback to .cube
    }
}

/// Parse Autodesk/Kodak .3dl 3D LUT format.
/// Lines contain 3 integers (10-bit 0-1023 or 12-bit 0-4095). Size inferred from entry count.
pub fn parse_3dl(text: &str) -> Result<LutData, LutError> {
    let mut data = Vec::new();
    let mut max_val: u32 = 0;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Skip header lines (often the first line is a mesh definition like "0 64 128 ...")
        // Data lines have exactly 3 integer values
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 3 {
            let mut vals = [0u32; 3];
            let mut ok = true;
            for (i, part) in parts.iter().enumerate() {
                match part.parse::<u32>() {
                    Ok(v) => {
                        vals[i] = v;
                        if v > max_val { max_val = v; }
                    }
                    Err(_) => {
                        // Try as float (some .3dl files use floats)
                        match part.parse::<f32>() {
                            Ok(v) => {
                                data.push(v.clamp(0.0, 1.0));
                                ok = false; // handled as float
                            }
                            Err(_) => { ok = false; break; }
                        }
                    }
                }
            }
            if ok {
                // Store as raw integers, normalize later
                for v in &vals {
                    data.push(*v as f32);
                }
            }
        } else if parts.len() > 3 {
            // Skip mesh definition lines (e.g. "0 64 128 192 256 ...")
            continue;
        }
    }

    if data.is_empty() {
        return Err(LutError::NotEnoughEntries { expected: 8, got: 0 });
    }

    // Normalize integer values to 0-1 range
    if max_val > 1 {
        let scale = 1.0 / max_val as f32;
        for v in &mut data {
            *v *= scale;
        }
    }

    // Infer size from entry count: entries = size^3, data.len() = size^3 * 3
    let entries = data.len() / 3;
    let size = (entries as f64).cbrt().round() as usize;
    if size * size * size != entries {
        return Err(LutError::InvalidSize(size));
    }
    if !(2..=256).contains(&size) {
        return Err(LutError::InvalidSize(size));
    }

    data.truncate(size * size * size * 3);
    Ok(LutData {
        size,
        data,
        // Untagged .cube/.csp/.3dl files are conventionally authored
        // in BT.709 / sRGB. Users override via the Inspector when
        // they know the LUT was authored in a different space.
        input_space: WorkingColorspace::Rec709Linear,
        output_space: WorkingColorspace::Rec709Linear,
    })
}

/// Parse Rising Sun Research .csp LUT format.
/// Header: CSPLUTV100 / 3D, optional 1D pre-LUTs, then 3D data as float triplets.
pub fn parse_csp(text: &str) -> Result<LutData, LutError> {
    let mut lines = text.lines().peekable();
    let mut found_header = false;
    let mut is_3d = false;

    // Parse header
    for line in lines.by_ref() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "CSPLUTV100" {
            found_header = true;
            continue;
        }
        if found_header && line == "3D" {
            is_3d = true;
            break;
        }
        if found_header && line == "1D" {
            // 1D CSP not supported as 3D LUT
            return Err(LutError::MissingSize);
        }
    }

    if !found_header || !is_3d {
        return Err(LutError::MissingSize);
    }

    // Skip 1D pre-LUT sections (3 sections, each: count on one line, then count pairs)
    for _ in 0..3 {
        // Read count line
        let count_line = loop {
            match lines.next() {
                Some(l) if !l.trim().is_empty() => break l.trim().to_string(),
                Some(_) => continue,
                None => return Err(LutError::MissingSize),
            }
        };
        let count: usize = count_line.parse().unwrap_or(0);
        // Skip `count` data lines
        for _ in 0..count {
            lines.next();
        }
    }

    // Read 3D LUT size
    let size_line = loop {
        match lines.next() {
            Some(l) if !l.trim().is_empty() => break l.trim().to_string(),
            Some(_) => continue,
            None => return Err(LutError::MissingSize),
        }
    };
    let parts: Vec<&str> = size_line.split_whitespace().collect();
    let size: usize = if parts.len() >= 3 {
        // "N N N" format
        parts[0].parse().map_err(|_| LutError::ParseFloat { line_num: 0, text: size_line.clone() })?
    } else {
        size_line.parse().map_err(|_| LutError::ParseFloat { line_num: 0, text: size_line.clone() })?
    };

    if !(2..=256).contains(&size) {
        return Err(LutError::InvalidSize(size));
    }

    // Read 3D data: size^3 lines of "r g b" floats
    let expected = size * size * size;
    let mut data = Vec::with_capacity(expected * 3);
    let mut line_num = 0;

    for line in lines {
        let line = line.trim();
        if line.is_empty() { continue; }
        line_num += 1;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            for part in parts.iter().take(3) {
                let v: f32 = part.parse().map_err(|_| LutError::ParseFloat {
                    line_num,
                    text: part.to_string(),
                })?;
                data.push(v);
            }
        }
        if data.len() >= expected * 3 { break; }
    }

    let got = data.len() / 3;
    if got < expected {
        return Err(LutError::NotEnoughEntries { expected, got });
    }
    data.truncate(expected * 3);

    Ok(LutData {
        size,
        data,
        // Untagged .cube/.csp/.3dl files are conventionally authored
        // in BT.709 / sRGB. Users override via the Inspector when
        // they know the LUT was authored in a different space.
        input_space: WorkingColorspace::Rec709Linear,
        output_space: WorkingColorspace::Rec709Linear,
    })
}

/// Generate a small reference image (64x64 RGBA) with color ramps for LUT preview.
/// The image contains: top third = warm skin tones to cool sky, middle = full hue sweep, bottom = grayscale ramp.
pub fn generate_lut_reference_image() -> Vec<u8> {
    let w = 64usize;
    let h = 64usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let (r, g, b) = if fy < 0.33 {
                // Top: warm to cool gradient (skin tones -> sky)
                let r = 0.85 - fx * 0.5;
                let g = 0.65 - fx * 0.15;
                let b = 0.55 + fx * 0.4;
                (r, g, b)
            } else if fy < 0.66 {
                // Middle: full hue sweep
                let hue = fx * 360.0;
                hsv_to_rgb(hue, 0.7, 0.8)
            } else {
                // Bottom: grayscale ramp
                (fx, fx, fx)
            };
            rgba[idx] = (r.clamp(0.0, 1.0) * 255.0) as u8;
            rgba[idx + 1] = (g.clamp(0.0, 1.0) * 255.0) as u8;
            rgba[idx + 2] = (b.clamp(0.0, 1.0) * 255.0) as u8;
            rgba[idx + 3] = 255;
        }
    }
    rgba
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// Generate a LUT preview thumbnail by applying the LUT to a reference image.
/// Returns 64x64 RGBA data.
pub fn generate_lut_preview(lut: &LutData) -> Vec<u8> {
    let mut rgba = generate_lut_reference_image();
    apply_3d_lut(&mut rgba, lut, 1.0);
    rgba
}

/// Apply a 3D LUT with trilinear interpolation to an RGBA buffer.
/// `opacity` controls blend: 0.0 = original, 1.0 = fully graded.
pub fn apply_3d_lut(rgba: &mut [u8], lut: &LutData, opacity: f32) {
    if opacity <= 0.0 {
        return;
    }
    let n = lut.size;
    let n1 = (n - 1) as f32;

    for pixel in rgba.chunks_exact_mut(4) {
        let r_in = pixel[0] as f32 / 255.0;
        let g_in = pixel[1] as f32 / 255.0;
        let b_in = pixel[2] as f32 / 255.0;

        let r_idx = (r_in * n1).clamp(0.0, n1);
        let g_idx = (g_in * n1).clamp(0.0, n1);
        let b_idx = (b_in * n1).clamp(0.0, n1);

        let r0 = (r_idx.floor() as usize).min(n - 2);
        let g0 = (g_idx.floor() as usize).min(n - 2);
        let b0 = (b_idx.floor() as usize).min(n - 2);
        let r1 = r0 + 1;
        let g1 = g0 + 1;
        let b1 = b0 + 1;

        let rf = r_idx - r0 as f32;
        let gf = g_idx - g0 as f32;
        let bf = b_idx - b0 as f32;

        let idx = |ri: usize, gi: usize, bi: usize| (bi * n * n + gi * n + ri) * 3;

        for (ch, px) in pixel.iter_mut().take(3).enumerate() {
            let c000 = lut.data[idx(r0, g0, b0) + ch];
            let c100 = lut.data[idx(r1, g0, b0) + ch];
            let c010 = lut.data[idx(r0, g1, b0) + ch];
            let c110 = lut.data[idx(r1, g1, b0) + ch];
            let c001 = lut.data[idx(r0, g0, b1) + ch];
            let c101 = lut.data[idx(r1, g0, b1) + ch];
            let c011 = lut.data[idx(r0, g1, b1) + ch];
            let c111 = lut.data[idx(r1, g1, b1) + ch];

            let c00 = c000 + (c100 - c000) * rf;
            let c01 = c001 + (c101 - c001) * rf;
            let c10 = c010 + (c110 - c010) * rf;
            let c11 = c011 + (c111 - c011) * rf;
            let c0 = c00 + (c10 - c00) * gf;
            let c1 = c01 + (c11 - c01) * gf;
            let lut_out = c0 + (c1 - c0) * bf;

            let original = *px as f32 / 255.0;
            let result = original + (lut_out - original) * opacity;
            *px = (result.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
}

/// Apply a 3D LUT with tetrahedral interpolation to an RGBA buffer.
/// Tetrahedral is more perceptually accurate than trilinear for steep gradients.
/// `opacity` controls blend: 0.0 = original, 1.0 = fully graded.
pub fn apply_3d_lut_tetrahedral(rgba: &mut [u8], lut: &LutData, opacity: f32) {
    if opacity <= 0.0 {
        return;
    }
    let n = lut.size;
    let n1 = (n - 1) as f32;

    let idx = |ri: usize, gi: usize, bi: usize| (bi * n * n + gi * n + ri) * 3;

    for pixel in rgba.chunks_exact_mut(4) {
        let r_in = pixel[0] as f32 / 255.0;
        let g_in = pixel[1] as f32 / 255.0;
        let b_in = pixel[2] as f32 / 255.0;

        let r_idx = (r_in * n1).clamp(0.0, n1);
        let g_idx = (g_in * n1).clamp(0.0, n1);
        let b_idx = (b_in * n1).clamp(0.0, n1);

        let r0 = (r_idx.floor() as usize).min(n - 2);
        let g0 = (g_idx.floor() as usize).min(n - 2);
        let b0 = (b_idx.floor() as usize).min(n - 2);

        let rf = r_idx - r0 as f32;
        let gf = g_idx - g0 as f32;
        let bf = b_idx - b0 as f32;

        for (ch, px) in pixel.iter_mut().take(3).enumerate() {
            let c000 = lut.data[idx(r0, g0, b0) + ch];
            let c100 = lut.data[idx(r0 + 1, g0, b0) + ch];
            let c010 = lut.data[idx(r0, g0 + 1, b0) + ch];
            let c110 = lut.data[idx(r0 + 1, g0 + 1, b0) + ch];
            let c001 = lut.data[idx(r0, g0, b0 + 1) + ch];
            let c101 = lut.data[idx(r0 + 1, g0, b0 + 1) + ch];
            let c011 = lut.data[idx(r0, g0 + 1, b0 + 1) + ch];
            let c111 = lut.data[idx(r0 + 1, g0 + 1, b0 + 1) + ch];

            // Tetrahedral: split unit cube into 6 tetrahedra based on which
            // fractional coordinate is largest, middle, smallest
            let lut_out = if rf > gf {
                if gf > bf {
                    // rf > gf > bf — tetrahedron T1
                    c000 + (c100 - c000) * rf + (c110 - c100) * gf + (c111 - c110) * bf
                } else if rf > bf {
                    // rf > bf > gf — tetrahedron T2
                    c000 + (c100 - c000) * rf + (c101 - c100) * bf + (c111 - c101) * gf
                } else {
                    // bf > rf > gf — tetrahedron T3
                    c000 + (c001 - c000) * bf + (c101 - c001) * rf + (c111 - c101) * gf
                }
            } else if rf > bf {
                // gf > rf > bf — tetrahedron T4
                c000 + (c010 - c000) * gf + (c110 - c010) * rf + (c111 - c110) * bf
            } else if gf > bf {
                // gf > bf > rf — tetrahedron T5
                c000 + (c010 - c000) * gf + (c011 - c010) * bf + (c111 - c011) * rf
            } else {
                // bf > gf > rf — tetrahedron T6
                c000 + (c001 - c000) * bf + (c011 - c001) * gf + (c111 - c011) * rf
            };

            let original = *px as f32 / 255.0;
            let result = original + (lut_out - original) * opacity;
            *px = (result.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
}

/// Apply a 3D LUT to a linear-light RGBA buffer, with working-space
/// bracketing.
///
/// **Pipeline:**
///   1. If `working_space != lut.input_space`, convert each pixel
///      from the working space to the LUT's input space via the
///      primaries-conversion matrix.
///   2. Trilinear-interpolate the LUT (input clamped to `[0, 1]` for
///      the lookup; output is the LUT's natural range).
///   3. If `working_space != lut.output_space`, convert the LUT
///      output back into the working space.
///   4. Mix with the original input by `opacity`, in the working
///      space.
///
/// `rgba_linear` is `&mut [f32]` RGBA in linear-light values, with
/// alpha left untouched. Values outside `[0, 1]` are tolerated on the
/// way in (clamped only for the LUT lookup itself); the post-LUT
/// signal can re-overshoot if the LUT is bake-graded into headroom.
///
/// For untagged LUTs (the common case — both `input_space` and
/// `output_space` default to `Rec709Linear` after parsing) and a
/// `Rec709Linear` working space, this reduces to the trilinear LUT
/// apply you already had — no matmul cost. The bracketing kicks in
/// only when one of the three colorspaces differs.
pub fn apply_3d_lut_linear_ws(
    rgba_linear: &mut [f32],
    lut: &LutData,
    opacity: f32,
    working_space: WorkingColorspace,
    tetrahedral: bool,
) {
    apply_3d_lut_linear_ws_with_overrides(
        rgba_linear, lut, opacity, working_space,
        lut.input_space, lut.output_space, tetrahedral,
    )
}

/// `apply_3d_lut_linear_ws` with explicit per-call overrides for the
/// LUT's input and output colorspace tags. Used when the user has
/// re-tagged a LUT via the Inspector dropdown — the underlying
/// `LutData` (which may be `Arc`-cached and shared across clips)
/// stays untouched, and the caller's override drives the bracketing
/// matrices.
///
/// Pass `lut.input_space` / `lut.output_space` directly to recover
/// the un-overridden behaviour.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn apply_3d_lut_linear_ws_with_overrides(
    rgba_linear: &mut [f32],
    lut: &LutData,
    opacity: f32,
    working_space: WorkingColorspace,
    input_space: WorkingColorspace,
    output_space: WorkingColorspace,
    tetrahedral: bool,
) {
    if opacity <= 0.0 {
        return;
    }
    let n = lut.size;
    if n < 2 {
        return;
    }
    let n1 = (n - 1) as f32;
    let pre = crate::primaries::working_space_conversion_matrix(working_space, input_space);
    let post = crate::primaries::working_space_conversion_matrix(output_space, working_space);

    for pixel in rgba_linear.chunks_exact_mut(4) {
        let original = [pixel[0], pixel[1], pixel[2]];

        let lut_in = if let Some(m) = pre {
            crate::primaries::matrix3_mul_vec(m, original)
        } else {
            original
        };

        let r_idx = (lut_in[0] * n1).clamp(0.0, n1);
        let g_idx = (lut_in[1] * n1).clamp(0.0, n1);
        let b_idx = (lut_in[2] * n1).clamp(0.0, n1);
        let r0 = (r_idx.floor() as usize).min(n - 2);
        let g0 = (g_idx.floor() as usize).min(n - 2);
        let b0 = (b_idx.floor() as usize).min(n - 2);
        let r1 = r0 + 1;
        let g1 = g0 + 1;
        let b1 = b0 + 1;
        let rf = r_idx - r0 as f32;
        let gf = g_idx - g0 as f32;
        let bf = b_idx - b0 as f32;
        let idx = |ri: usize, gi: usize, bi: usize| (bi * n * n + gi * n + ri) * 3;

        let mut lut_out = [0.0_f32; 3];
        for (ch, slot) in lut_out.iter_mut().enumerate() {
            let c000 = lut.data[idx(r0, g0, b0) + ch];
            let c100 = lut.data[idx(r1, g0, b0) + ch];
            let c010 = lut.data[idx(r0, g1, b0) + ch];
            let c110 = lut.data[idx(r1, g1, b0) + ch];
            let c001 = lut.data[idx(r0, g0, b1) + ch];
            let c101 = lut.data[idx(r1, g0, b1) + ch];
            let c011 = lut.data[idx(r0, g1, b1) + ch];
            let c111 = lut.data[idx(r1, g1, b1) + ch];
            *slot = if tetrahedral {
                // The unit cube split into six tetrahedra by the order of
                // the fractional coordinates: four surrounding corners
                // instead of all eight, which holds a LUT's straight
                // lines straight where trilinear bows them.
                if rf > gf {
                    if gf > bf {
                        c000 + (c100 - c000) * rf + (c110 - c100) * gf + (c111 - c110) * bf
                    } else if rf > bf {
                        c000 + (c100 - c000) * rf + (c101 - c100) * bf + (c111 - c101) * gf
                    } else {
                        c000 + (c001 - c000) * bf + (c101 - c001) * rf + (c111 - c101) * gf
                    }
                } else if rf > bf {
                    c000 + (c010 - c000) * gf + (c110 - c010) * rf + (c111 - c110) * bf
                } else if gf > bf {
                    c000 + (c010 - c000) * gf + (c011 - c010) * bf + (c111 - c011) * rf
                } else {
                    c000 + (c001 - c000) * bf + (c011 - c001) * gf + (c111 - c011) * rf
                }
            } else {
                let c00 = c000 + (c100 - c000) * rf;
                let c01 = c001 + (c101 - c001) * rf;
                let c10 = c010 + (c110 - c010) * rf;
                let c11 = c011 + (c111 - c011) * rf;
                let c0 = c00 + (c10 - c00) * gf;
                let c1 = c01 + (c11 - c01) * gf;
                c0 + (c1 - c0) * bf
            };
        }

        let post_rgb = if let Some(m) = post {
            crate::primaries::matrix3_mul_vec(m, lut_out)
        } else {
            lut_out
        };

        pixel[0] = original[0] + (post_rgb[0] - original[0]) * opacity;
        pixel[1] = original[1] + (post_rgb[1] - original[1]) * opacity;
        pixel[2] = original[2] + (post_rgb[2] - original[2]) * opacity;
    }
}

/// Bake a color grade into a 3D LUT by running every RGB triplet through the
/// primary grade pipeline. Does NOT capture qualifier, masks, or keyframe
/// animation — those are spatial/temporal and cannot be represented in a 3D LUT.
///
/// `lut_size`: grid size per axis (17=fast, 33=standard, 65=high quality).
/// Returns RGB float triples in B-major order (R varies fastest).
pub fn bake_grade_to_lut(
    params: &super::ColorGradeParams,
    pipeline: &super::ColorPipeline,
    lut_size: u32,
) -> Vec<[f32; 3]> {
    let n = lut_size as usize;
    let mut lut = Vec::with_capacity(n * n * n);

    // Build params without qualifier/masks (primary grade only)
    let mut primary_params = params.clone();
    primary_params.qualifier = Default::default();
    primary_params.masks = Default::default();

    for bi in 0..n {
        for gi in 0..n {
            for ri in 0..n {
                let r = (ri as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let g = (gi as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let b = (bi as f32 / (n - 1).max(1) as f32 * 255.0) as u8;
                let pixel = vec![r, g, b, 255];
                let result = pipeline.apply_clip_grade_full(pixel, &primary_params, 1, 1, false);
                lut.push([
                    result[0] as f32 / 255.0,
                    result[1] as f32 / 255.0,
                    result[2] as f32 / 255.0,
                ]);
            }
        }
    }
    lut
}

/// Write a 3D LUT in Adobe .cube format.
pub fn write_cube(
    path: &Path,
    lut_data: &[[f32; 3]],
    lut_size: u32,
    title: &str,
) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "TITLE \"{title}\"")?;
    writeln!(f, "LUT_3D_SIZE {lut_size}")?;
    writeln!(f, "DOMAIN_MIN 0.0 0.0 0.0")?;
    writeln!(f, "DOMAIN_MAX 1.0 1.0 1.0")?;
    writeln!(f)?;
    for [r, g, b] in lut_data {
        writeln!(f, "{r:.6} {g:.6} {b:.6}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_lut(size: usize) -> LutData {
        // Existing tests below use this local helper; it now just defers
        // to the public LutData::identity so the public path stays
        // exercised by the same passthrough assertions.
        LutData::identity(size)
    }

    #[test]
    fn parse_minimal_cube() {
        let text = "LUT_3D_SIZE 2\n\
                     0.0 0.0 0.0\n\
                     1.0 0.0 0.0\n\
                     0.0 1.0 0.0\n\
                     1.0 1.0 0.0\n\
                     0.0 0.0 1.0\n\
                     1.0 0.0 1.0\n\
                     0.0 1.0 1.0\n\
                     1.0 1.0 1.0\n";
        let lut = parse_cube(text).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 2 * 2 * 2 * 3);
    }

    #[test]
    fn parse_cube_with_comments_and_headers() {
        let text = "# Created by test\n\
                     TITLE \"Test LUT\"\n\
                     DOMAIN_MIN 0.0 0.0 0.0\n\
                     DOMAIN_MAX 1.0 1.0 1.0\n\
                     LUT_3D_SIZE 2\n\
                     \n\
                     0.0 0.0 0.0\n\
                     1.0 0.0 0.0\n\
                     0.0 1.0 0.0\n\
                     1.0 1.0 0.0\n\
                     0.0 0.0 1.0\n\
                     1.0 0.0 1.0\n\
                     0.0 1.0 1.0\n\
                     1.0 1.0 1.0\n";
        let lut = parse_cube(text).unwrap();
        assert_eq!(lut.size, 2);
    }

    #[test]
    fn parse_cube_missing_size() {
        let text = "0.0 0.0 0.0\n1.0 1.0 1.0\n";
        assert!(matches!(parse_cube(text), Err(LutError::MissingSize)));
    }

    #[test]
    fn parse_cube_not_enough_entries() {
        let text = "LUT_3D_SIZE 2\n0.0 0.0 0.0\n";
        assert!(matches!(
            parse_cube(text),
            Err(LutError::NotEnoughEntries { .. })
        ));
    }

    #[test]
    fn parse_cube_invalid_size() {
        let text = "LUT_3D_SIZE 0\n";
        assert!(matches!(parse_cube(text), Err(LutError::InvalidSize(0))));
    }

    #[test]
    fn identity_lut_size2_is_passthrough() {
        // CODE_AUDIT C-9: size=2 is the smallest identity LUT and is
        // what executor.rs uses as the load-failure fallback. Pin that
        // it really is passthrough (vs the previous all-zero-data
        // fallback that silently rendered black).
        let lut = LutData::identity(2);
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 2 * 2 * 2 * 3);
        for sample in [
            [0u8, 0, 0, 255],
            [50, 100, 150, 255],
            [128, 128, 128, 255],
            [200, 50, 25, 255],
            [255, 255, 255, 255],
        ] {
            let mut px = sample.to_vec();
            apply_3d_lut(&mut px, &lut, 1.0);
            for ch in 0..3 {
                assert!(
                    (px[ch] as i16 - sample[ch] as i16).unsigned_abs() <= 1,
                    "size=2 identity LUT must be passthrough; ch {}: got {}, expected {}",
                    ch, px[ch], sample[ch]
                );
            }
            assert_eq!(px[3], sample[3], "alpha must be untouched");
        }
    }

    #[test]
    fn identity_lut_is_passthrough() {
        let lut = identity_lut(33);
        let mut px = vec![100, 150, 200, 255];
        let orig = px.clone();
        apply_3d_lut(&mut px, &lut, 1.0);
        for i in 0..3 {
            assert!(
                (px[i] as i16 - orig[i] as i16).unsigned_abs() <= 1,
                "ch {}: {} vs {}", i, px[i], orig[i]
            );
        }
        assert_eq!(px[3], 255);
    }

    #[test]
    fn lut_zero_opacity_is_passthrough() {
        // Even a non-identity LUT at opacity 0 should not change pixels
        let data = vec![0.0f32; 2 * 2 * 2 * 3]; // all-black LUT
        let lut = LutData {
            size: 2, data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut px = vec![128, 128, 128, 255];
        let orig = px.clone();
        apply_3d_lut(&mut px, &lut, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn lut_full_opacity_applies() {
        // All-black LUT should make everything black
        let data = vec![0.0f32; 2 * 2 * 2 * 3];
        let lut = LutData {
            size: 2, data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut px = vec![200, 200, 200, 255];
        apply_3d_lut(&mut px, &lut, 1.0);
        assert_eq!(px[0], 0);
        assert_eq!(px[1], 0);
        assert_eq!(px[2], 0);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn lut_half_opacity_blends() {
        let data = vec![0.0f32; 2 * 2 * 2 * 3]; // all-black LUT
        let lut = LutData {
            size: 2, data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut px = vec![200, 200, 200, 255];
        apply_3d_lut(&mut px, &lut, 0.5);
        assert_eq!(px[0], 100);
        assert_eq!(px[1], 100);
        assert_eq!(px[2], 100);
    }

    #[test]
    fn lut_alpha_unchanged() {
        let data = vec![0.0f32; 2 * 2 * 2 * 3];
        let lut = LutData {
            size: 2, data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut px = vec![200, 200, 200, 42];
        apply_3d_lut(&mut px, &lut, 1.0);
        assert_eq!(px[3], 42);
    }

    #[test]
    fn identity_lut_multi_pixel() {
        let lut = identity_lut(33);
        let mut buf = vec![
            50, 100, 150, 255,
            200, 50, 25, 255,
            0, 255, 128, 255,
        ];
        let orig = buf.clone();
        apply_3d_lut(&mut buf, &lut, 1.0);
        for i in (0..buf.len()).step_by(4) {
            for ch in 0..3 {
                assert!(
                    (buf[i + ch] as i16 - orig[i + ch] as i16).unsigned_abs() <= 1,
                    "pixel {}, ch {}: {} vs {}",
                    i / 4, ch, buf[i + ch], orig[i + ch]
                );
            }
        }
    }

    // ─── Phase 2: LUT bake/export tests ──────────────────────────────

    fn identity_grade_params() -> crate::ColorGradeParams {
        crate::ColorGradeParams {
            exposure: 0.0, contrast: 0.0, saturation: 1.0, vibrance: 0.0,
            temperature: 0.0, tint: 0.0,
            highlights: 0.0, shadows: 0.0, whites: 0.0, blacks: 0.0,
            lift: (0.0, 0.0, 0.0), gamma: (0.0, 0.0, 0.0),
            gain: (0.0, 0.0, 0.0), offset: (0.0, 0.0, 0.0),
            lift_sat: 0.0, lift_bri: 0.0,
            gamma_sat: 0.0, gamma_bri: 0.0,
            gain_sat: 0.0, gain_bri: 0.0,
            offset_sat: 0.0, offset_bri: 0.0,
            hue: 0.0, global_mix: 1.0, clarity: 0.0,
            curve_luts: None, curves_preserve_luma: false, curves_mix: 1.0,
            huesat: Default::default(), huesat_mix: 1.0,
            qualifier: Default::default(), masks: Default::default(),
            input_log_format: Default::default(),
        }
    }

    #[test]
    fn lut_bake_identity_grade() {
        // Identity grade → LUT should be identity (output ≈ input)
        let params = identity_grade_params();
        let pipeline = crate::ColorPipeline::new();
        let lut = bake_grade_to_lut(&params, &pipeline, 17);
        assert_eq!(lut.len(), 17 * 17 * 17);
        // Spot-check: several grid points should map to themselves
        // LUT quantisation error: max 2/255 ≈ 0.008 from u8 truncation
        for bi in [0, 8, 16] {
            for gi in [0, 8, 16] {
                for ri in [0, 8, 16] {
                    let idx = bi * 17 * 17 + gi * 17 + ri;
                    let expected_r = ri as f32 / 16.0;
                    let expected_g = gi as f32 / 16.0;
                    let expected_b = bi as f32 / 16.0;
                    let [lr, lg, lb] = lut[idx];
                    assert!((lr - expected_r).abs() < 0.01,
                        "identity R at ({ri},{gi},{bi}): got {lr}, expected {expected_r}");
                    assert!((lg - expected_g).abs() < 0.01,
                        "identity G at ({ri},{gi},{bi}): got {lg}, expected {expected_g}");
                    assert!((lb - expected_b).abs() < 0.01,
                        "identity B at ({ri},{gi},{bi}): got {lb}, expected {expected_b}");
                }
            }
        }
    }

    #[test]
    fn lut_bake_exposure_only() {
        // Exposure +0.5 stops → mid-grey should brighten
        let mut params = identity_grade_params();
        params.exposure = 0.5;
        let pipeline = crate::ColorPipeline::new();
        let lut = bake_grade_to_lut(&params, &pipeline, 17);
        // Mid-grey: index (8,8,8) → input = 128/255 ≈ 0.502
        let idx = 8 * 17 * 17 + 8 * 17 + 8;
        let [lr, _, _] = lut[idx];
        // Direct grade: 128/255 * 2^0.5 ≈ 0.710
        // LUT tolerance: 3/255 ≈ 0.012 (u8 quantisation in bake + readback)
        let direct = (128.0 / 255.0 * 2.0f32.powf(0.5)).min(1.0);
        assert!((lr - direct).abs() < 0.015,
            "exposure LUT: got {lr}, direct={direct}");
    }

    #[test]
    fn lut_bake_temperature_only() {
        // Temperature +30 → R should increase, B should decrease
        let mut params = identity_grade_params();
        params.temperature = 30.0;
        let pipeline = crate::ColorPipeline::new();
        let lut = bake_grade_to_lut(&params, &pipeline, 17);
        let idx = 8 * 17 * 17 + 8 * 17 + 8; // mid-grey
        let [lr, _, lb] = lut[idx];
        let input = 8.0 / 16.0; // 0.5
        assert!(lr > input, "warm temperature should increase R: {lr} > {input}");
        assert!(lb < input, "warm temperature should decrease B: {lb} < {input}");
    }

    #[test]
    fn lut_write_and_read_roundtrip() {
        // Bake, write .cube, read back, compare
        let mut params = identity_grade_params();
        params.exposure = 0.3;
        params.saturation = 1.2;
        let pipeline = crate::ColorPipeline::new();
        let lut_data = bake_grade_to_lut(&params, &pipeline, 17);

        let dir = std::env::temp_dir();
        let path = dir.join("klipp_test_lut_roundtrip.cube");
        write_cube(&path, &lut_data, 17, "Test").unwrap();

        // Read back with existing parser
        let loaded = parse_cube(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.size, 17);
        assert_eq!(loaded.data.len(), 17 * 17 * 17 * 3);

        // Apply both original grade and loaded LUT to test pixels
        for &(r, g, b) in &[(100u8, 100, 100), (200, 50, 150), (30, 200, 80)] {
            // Direct grade
            let direct = pipeline.apply_clip_grade_full(
                vec![r, g, b, 255], &params, 1, 1, false);
            // LUT application
            let mut lut_px = vec![r, g, b, 255];
            apply_3d_lut(&mut lut_px, &loaded, 1.0);
            // Tolerance: 5/255 ≈ 0.020 for LUT quantisation + trilinear interpolation
            // in steep gradient regions (e.g. highly saturated colours near clipping)
            for ch in 0..3 {
                let err = (direct[ch] as i16 - lut_px[ch] as i16).unsigned_abs();
                assert!(err <= 5,
                    "roundtrip pixel ({r},{g},{b}) ch{ch}: direct={} lut={} err={err}",
                    direct[ch], lut_px[ch]);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lut_cube_format_valid() {
        let params = identity_grade_params();
        let pipeline = crate::ColorPipeline::new();
        let lut_data = bake_grade_to_lut(&params, &pipeline, 17);

        let dir = std::env::temp_dir();
        let path = dir.join("klipp_test_lut_format.cube");
        write_cube(&path, &lut_data, 17, "Format Test").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        // Header checks
        assert!(lines[0].starts_with("TITLE"), "first line should be TITLE");
        let size_line = lines.iter().find(|l| l.starts_with("LUT_3D_SIZE")).unwrap();
        assert_eq!(*size_line, "LUT_3D_SIZE 17");

        // Count data lines (lines with 3 floats)
        let data_lines: Vec<&&str> = lines.iter()
            .filter(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                parts.len() == 3 && parts[0].parse::<f32>().is_ok()
            })
            .collect();
        assert_eq!(data_lines.len(), 17 * 17 * 17,
            "should have 4913 data lines, got {}", data_lines.len());

        // Each data value should be in 0.0-1.0 range
        for line in &data_lines {
            for val_str in line.split_whitespace() {
                let v: f32 = val_str.parse().unwrap();
                assert!(v >= 0.0 && v <= 1.0,
                    "LUT value out of range: {v} in line '{line}'");
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    // ─── Working-space-aware LUT apply (Phase E S6) ─────────────

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// Identity LUT + matching working-space tags + zero-conversion
    /// path = pure passthrough at any opacity. Sanity check on the
    /// inner trilinear loop.
    #[test]
    fn ws_aware_apply_identity_lut_rec709_passthrough() {
        let lut = LutData::identity(33);
        let mut pixels: Vec<f32> = vec![
            0.10, 0.30, 0.70, 1.0,
            0.50, 0.50, 0.50, 1.0,
            0.99, 0.01, 0.50, 1.0,
        ];
        let original = pixels.clone();
        apply_3d_lut_linear_ws(&mut pixels, &lut, 1.0, WorkingColorspace::Rec709Linear, false);
        for i in 0..pixels.len() {
            assert!(approx_eq(pixels[i], original[i], 1e-3),
                "identity LUT at index {i}: {} → {}", original[i], pixels[i]);
        }
    }

    /// LUT tagged Rec709/Rec709, working space AcesCG. Pre-bracket
    /// converts AcesCG → Rec709, post-bracket converts Rec709 →
    /// AcesCG. Net effect on an identity LUT for IN-GAMUT inputs:
    /// AcesCG → Rec709 → (identity) → AcesCG = identity (within
    /// float + LUT-grid precision).
    ///
    /// AcesCG values that fall outside the BT.709 triangle map to
    /// negative Rec.709 channels (the wider AP1 gamut spilling
    /// beyond Rec.709). The trilinear LUT lookup clamps its input to
    /// `[0, 1]`, so those out-of-gamut channels won't survive — that's
    /// inherent to running an SDR-Rec.709 LUT against wide-gamut
    /// content, not a bracketing bug. The test fixtures here use
    /// neutral-ish AcesCG values that stay in-gamut on both sides.
    #[test]
    fn ws_aware_identity_lut_with_mismatched_working_space_round_trips() {
        let lut = LutData::identity(33);
        // All values close to neutral grey + mild deviations that
        // produce non-negative Rec.709 channels (verified by hand).
        let mut pixels: Vec<f32> = vec![
            0.50, 0.50, 0.50, 1.0,
            0.30, 0.30, 0.30, 1.0,
            0.55, 0.50, 0.45, 1.0,
            0.40, 0.45, 0.50, 1.0,
        ];
        let original = pixels.clone();
        apply_3d_lut_linear_ws(&mut pixels, &lut, 1.0, WorkingColorspace::AcesCG, false);
        // Tolerance: 33-grid trilinear adds ~3e-4, AcesCG ↔ Rec709
        // round-trip ~1e-7 in f32. 5e-3 covers both cleanly with
        // headroom for the value range we're testing (~0.3 to 0.55).
        for i in 0..pixels.len() {
            assert!(approx_eq(pixels[i], original[i], 5e-3),
                "round-trip with bracketed identity LUT at {i}: {} → {}",
                original[i], pixels[i]);
        }
    }

    /// Adversarial: an AcesCG value that maps OUTSIDE the BT.709
    /// gamut (one Rec.709 channel goes negative). The Rec.709-tagged
    /// identity LUT clamps the negative channel to zero at lookup
    /// time — this is the documented limitation, pinned here so a
    /// future "let's auto-detect and reroute" change has a known
    /// baseline to compare against.
    #[test]
    fn ws_aware_out_of_gamut_acescg_input_to_rec709_lut_clamps_at_lookup() {
        let lut = LutData::identity(33);
        // (0.1, 0.3, 0.7) AcesCG → Rec.709 has a negative R channel
        // (out of BT.709 red gamut on the wide-blue side).
        let mut pixels: Vec<f32> = vec![0.10, 0.30, 0.70, 1.0];
        apply_3d_lut_linear_ws(&mut pixels, &lut, 1.0, WorkingColorspace::AcesCG, false);
        // The LUT clamp lifts the input to a non-negative neighbourhood,
        // which the post-bracket maps back to AcesCG. Output must be
        // measurably different from the input on at least one channel
        // (the negative excursion lost information).
        let drift = (pixels[0] - 0.10).abs();
        assert!(drift > 0.01,
            "LUT clamp on out-of-gamut input should produce visible drift; got {drift}");
        // But the output must still be finite and well-bounded —
        // clamping should have prevented blow-up.
        for c in pixels.iter().take(3) {
            assert!(c.is_finite() && *c > -1.0 && *c < 2.0);
        }
    }

    /// Untagged LUT (input/output both Rec709) under Rec709 working
    /// space — should produce byte-identical output to a manual LUT
    /// apply with no bracketing. The conversion-matrix lookup
    /// returns `None` and the matmul is skipped entirely.
    #[test]
    fn ws_aware_no_op_when_all_three_spaces_match() {
        // A trivially non-identity LUT: scales each channel by 0.5
        // via the LUT entries.
        let mut data = Vec::new();
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    data.push(r as f32 * 0.5);
                    data.push(g as f32 * 0.5);
                    data.push(b as f32 * 0.5);
                }
            }
        }
        let lut = LutData {
            size: 2, data,
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut pixels: Vec<f32> = vec![1.0, 1.0, 1.0, 1.0];
        apply_3d_lut_linear_ws(&mut pixels, &lut, 1.0, WorkingColorspace::Rec709Linear, false);
        assert!(approx_eq(pixels[0], 0.5, 1e-3), "got {}", pixels[0]);
        assert!(approx_eq(pixels[1], 0.5, 1e-3));
        assert!(approx_eq(pixels[2], 0.5, 1e-3));
        assert_eq!(pixels[3], 1.0);
    }

    /// Opacity blending happens in the working space (the natural
    /// interpretation: "blend the LUT's contribution with the
    /// untouched signal"). Verify with a black LUT at half opacity:
    /// output should be half the input.
    #[test]
    fn ws_aware_opacity_blends_in_working_space() {
        let zero_lut = LutData {
            size: 2,
            data: vec![0.0; 2 * 2 * 2 * 3],
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::Rec709Linear,
        };
        let mut pixels: Vec<f32> = vec![0.8, 0.6, 0.4, 1.0];
        apply_3d_lut_linear_ws(&mut pixels, &zero_lut, 0.5, WorkingColorspace::Rec709Linear, false);
        assert!(approx_eq(pixels[0], 0.4, 1e-3));
        assert!(approx_eq(pixels[1], 0.3, 1e-3));
        assert!(approx_eq(pixels[2], 0.2, 1e-3));
    }

    /// Opacity = 0 short-circuits — input passes through completely
    /// untouched (no bracketing matmul, no LUT lookup, no mix).
    #[test]
    fn ws_aware_zero_opacity_is_passthrough() {
        let zero_lut = LutData {
            size: 2,
            data: vec![0.0; 2 * 2 * 2 * 3],
            input_space: WorkingColorspace::Rec709Linear,
            output_space: WorkingColorspace::AcesCG, // mismatched, would mangle if applied
        };
        let mut pixels: Vec<f32> = vec![0.8, 0.6, 0.4, 1.0];
        let original = pixels.clone();
        apply_3d_lut_linear_ws(&mut pixels, &zero_lut, 0.0, WorkingColorspace::AcesCG, false);
        assert_eq!(pixels, original);
    }

    /// New `LutData` instances created via parsers default to
    /// Rec709Linear on both sides — back-compat with every existing
    /// `.cube` file on disk.
    #[test]
    fn parser_defaults_lut_to_rec709_linear() {
        let text = "LUT_3D_SIZE 2\n\
                     0.0 0.0 0.0\n\
                     1.0 0.0 0.0\n\
                     0.0 1.0 0.0\n\
                     1.0 1.0 0.0\n\
                     0.0 0.0 1.0\n\
                     1.0 0.0 1.0\n\
                     0.0 1.0 1.0\n\
                     1.0 1.0 1.0\n";
        let lut = parse_cube(text).unwrap();
        assert_eq!(lut.input_space, WorkingColorspace::Rec709Linear);
        assert_eq!(lut.output_space, WorkingColorspace::Rec709Linear);
    }

    /// `LutData::identity` ditto.
    #[test]
    fn identity_lut_defaults_to_rec709_linear() {
        let lut = LutData::identity(17);
        assert_eq!(lut.input_space, WorkingColorspace::Rec709Linear);
        assert_eq!(lut.output_space, WorkingColorspace::Rec709Linear);
    }

    /// Override mode: pass `lut.input_space` and `lut.output_space`
    /// directly through the with-overrides API and verify the result
    /// matches the no-overrides API exactly. Pins the wrapper /
    /// underlying-impl equivalence.
    #[test]
    fn with_overrides_matches_default_when_passing_lut_tags() {
        let lut = LutData::identity(33);
        let mut a = vec![0.50, 0.50, 0.50, 1.0, 0.30, 0.20, 0.40, 1.0];
        let mut b = a.clone();
        apply_3d_lut_linear_ws(&mut a, &lut, 1.0, WorkingColorspace::AcesCG, true);
        apply_3d_lut_linear_ws_with_overrides(
            &mut b, &lut, 1.0,
            WorkingColorspace::AcesCG,
            lut.input_space, lut.output_space,
            true,
        );
        assert_eq!(a, b);
    }

    /// User re-tags an untagged Rec.709-default LUT as ACEScg-input
    /// while the project working space is also ACEScg. Pre/post
    /// brackets disappear (no conversion needed); output should be
    /// the LUT applied directly. With an identity LUT this is
    /// passthrough.
    #[test]
    fn override_input_to_match_working_space_skips_pre_bracket() {
        let lut = LutData::identity(33);
        let mut pixels: Vec<f32> = vec![0.50, 0.50, 0.50, 1.0, 0.30, 0.45, 0.55, 1.0];
        let original = pixels.clone();
        apply_3d_lut_linear_ws_with_overrides(&mut pixels, &lut, 1.0,
            WorkingColorspace::AcesCG,
            // User says: this LUT is ACEScg-tagged. Working space
            // is also ACEScg. → no conversion either side, identity
            // LUT → passthrough.
            WorkingColorspace::AcesCG,
            WorkingColorspace::AcesCG,
            false,
        );
        for i in 0..pixels.len() {
            assert!((pixels[i] - original[i]).abs() < 1e-3,
                "passthrough drift at {i}: {} → {}", original[i], pixels[i]);
        }
    }

    /// Override mode actually changes behaviour: tagging an identity
    /// LUT as Rec.709-input under an AcesCG working space introduces
    /// the AP1 → BT.709 → (identity) → BT.709 → AP1 round-trip.
    /// Result should differ measurably from the no-conversion case
    /// above on saturated inputs (where round-trip isn't perfectly
    /// identity due to LUT clamping at [0, 1]).
    #[test]
    fn overrides_actually_drive_bracketing_choice() {
        let lut = LutData::identity(33);
        // Saturated red — clearly out of BT.709 gamut on the AP1
        // side, so the AP1 → BT.709 conversion produces negative
        // channels that the LUT clamps. Output diverges from the
        // no-bracket path measurably.
        let pixels_in: Vec<f32> = vec![0.7, 0.05, 0.0, 1.0];

        let mut acescg_lut_path = pixels_in.clone();
        apply_3d_lut_linear_ws_with_overrides(
            &mut acescg_lut_path, &lut, 1.0,
            WorkingColorspace::AcesCG,
            WorkingColorspace::AcesCG,    // no pre/post conversion
            WorkingColorspace::AcesCG,
            false,
        );

        let mut rec709_lut_path = pixels_in.clone();
        apply_3d_lut_linear_ws_with_overrides(
            &mut rec709_lut_path, &lut, 1.0,
            WorkingColorspace::AcesCG,
            WorkingColorspace::Rec709Linear,  // brackets in
            WorkingColorspace::Rec709Linear,
            false,
        );

        // Expect divergence on at least one channel — the bracket
        // does real work even on identity LUTs.
        let diff = (acescg_lut_path[0] - rec709_lut_path[0]).abs()
            + (acescg_lut_path[1] - rec709_lut_path[1]).abs()
            + (acescg_lut_path[2] - rec709_lut_path[2]).abs();
        assert!(diff > 0.01,
            "override bracketing should produce visible divergence; got total RGB delta {diff}");
    }
}
