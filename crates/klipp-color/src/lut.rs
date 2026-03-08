use std::path::Path;

/// 3D LUT data — trilinear interpolation lookup table.
#[derive(Debug, Clone)]
pub struct LutData {
    pub size: usize,
    pub data: Vec<f32>, // r,g,b interleaved, size^3 * 3 floats
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

    Ok(LutData { size, data })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_lut(size: usize) -> LutData {
        let mut data = Vec::with_capacity(size * size * size * 3);
        let n1 = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(r as f32 / n1);
                    data.push(g as f32 / n1);
                    data.push(b as f32 / n1);
                }
            }
        }
        LutData { size, data }
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
        let lut = LutData { size: 2, data };
        let mut px = vec![128, 128, 128, 255];
        let orig = px.clone();
        apply_3d_lut(&mut px, &lut, 0.0);
        assert_eq!(px, orig);
    }

    #[test]
    fn lut_full_opacity_applies() {
        // All-black LUT should make everything black
        let data = vec![0.0f32; 2 * 2 * 2 * 3];
        let lut = LutData { size: 2, data };
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
        let lut = LutData { size: 2, data };
        let mut px = vec![200, 200, 200, 255];
        apply_3d_lut(&mut px, &lut, 0.5);
        assert_eq!(px[0], 100);
        assert_eq!(px[1], 100);
        assert_eq!(px[2], 100);
    }

    #[test]
    fn lut_alpha_unchanged() {
        let data = vec![0.0f32; 2 * 2 * 2 * 3];
        let lut = LutData { size: 2, data };
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
}
