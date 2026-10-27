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
    /// The primaries of the LUT's input domain. Primaries only: the
    /// lookup is always indexed on the sRGB encoding of those values,
    /// as a `.cube` look is built and as the Inspector's
    /// "Rec.709 / sRGB" says; the `Linear` in the variant names the
    /// working space the matrices convert from, not the index. Defaults
    /// to [`WorkingColorspace::Rec709Linear`] for an untagged `.cube`.
    pub input_space: WorkingColorspace,
    /// The primaries of the LUT's output, which is sRGB-encoded and
    /// decoded after the lookup. Same default.
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
    /// The file is larger than any real LUT ([`MAX_LUT_BYTES`]).
    TooLarge { limit: u64 },
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
            Self::TooLarge { limit } => {
                write!(f, "larger than any LUT ({} MiB at most)", limit / (1024 * 1024))
            }
        }
    }
}

impl From<std::io::Error> for LutError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

/// The largest LUT file read (#824). A 129-point cube written at full
/// float precision is about 90 MB; real LUTs are 17 to 65 points, a few
/// megabytes. Anything larger is refused before it is read into memory.
pub const MAX_LUT_BYTES: u64 = 128 * 1024 * 1024;

/// A LUT file's text, read only up to [`MAX_LUT_BYTES`]: the length is
/// checked while reading, not only from the file's size, which a file
/// still being written (or a pipe) does not tell truly.
fn read_lut_text(path: &Path) -> Result<String, LutError> {
    read_text_capped(std::fs::File::open(path)?, MAX_LUT_BYTES)
}

fn read_text_capped(file: impl std::io::Read, limit: u64) -> Result<String, LutError> {
    use std::io::Read;
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(LutError::TooLarge { limit });
    }
    Ok(text)
}

/// Parse an Adobe .cube 3D LUT file.
/// The size a 1D table or a shaper is baked into (#901): the 3D grid a
/// curve becomes, so every LUT reaches the pipeline as one 3D table.
/// 65 keeps a log shaper's shoulder within a code value of the curve.
const BAKED_SIZE: usize = 65;

/// One channel's curve, piecewise linear between `inputs` (ascending) and
/// the `outputs` there; held flat outside them.
#[derive(Debug, Clone)]
struct Curve {
    inputs: Vec<f32>,
    outputs: Vec<f32>,
}

impl Curve {
    /// `values` spread evenly over `min..=max`, as a `.cube` 1D table is.
    fn uniform(values: Vec<f32>, min: f32, max: f32) -> Self {
        let n = values.len().max(2) - 1;
        let inputs = (0..values.len()).map(|i| min + (max - min) * i as f32 / n as f32).collect();
        Self { inputs, outputs: values }
    }

    fn is_identity(&self) -> bool {
        self.inputs.iter().zip(&self.outputs).all(|(i, o)| (i - o).abs() < 1e-6) && self.inputs.first() == Some(&0.0) && self.inputs.last() == Some(&1.0)
    }

    fn at(&self, x: f32) -> f32 {
        let (ins, outs) = (&self.inputs, &self.outputs);
        if ins.is_empty() {
            return x;
        }
        if x <= ins[0] {
            return outs[0];
        }
        let last = ins.len() - 1;
        if x >= ins[last] {
            return outs[last];
        }
        let k = ins.partition_point(|&v| v <= x).clamp(1, last);
        let (x0, x1) = (ins[k - 1], ins[k]);
        let t = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
        outs[k - 1] + (outs[k] - outs[k - 1]) * t
    }
}

/// `data`, a `size`³ table (red fastest), read at `rgb` in 0..1, trilinearly.
fn sample_3d(data: &[f32], size: usize, rgb: [f32; 3]) -> [f32; 3] {
    let n = (size - 1) as f32;
    let axis = |v: f32| {
        let p = v.clamp(0.0, 1.0) * n;
        let i = (p.floor() as usize).min(size - 2);
        (i, p - i as f32)
    };
    let ((r0, fr), (g0, fg), (b0, fb)) = (axis(rgb[0]), axis(rgb[1]), axis(rgb[2]));
    let at = |r: usize, g: usize, b: usize, c: usize| data[((b * size + g) * size + r) * 3 + c];
    let mut out = [0.0f32; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(at(r0, g0, b0, c), at(r0 + 1, g0, b0, c), fr);
        let c10 = lerp(at(r0, g0 + 1, b0, c), at(r0 + 1, g0 + 1, b0, c), fr);
        let c01 = lerp(at(r0, g0, b0 + 1, c), at(r0 + 1, g0, b0 + 1, c), fr);
        let c11 = lerp(at(r0, g0 + 1, b0 + 1, c), at(r0 + 1, g0 + 1, b0 + 1, c), fr);
        *o = lerp(lerp(c00, c10, fg), lerp(c01, c11, fg), fb);
    }
    out
}

/// A `BAKED_SIZE`³ table of `f` over the unit cube (red fastest).
fn bake(f: impl Fn([f32; 3]) -> [f32; 3]) -> Vec<f32> {
    let size = BAKED_SIZE;
    let n = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                data.extend_from_slice(&f([r as f32 / n, g as f32 / n, b as f32 / n]));
            }
        }
    }
    data
}

/// A LUT from a 3D table, defaulting both sides to BT.709 / sRGB as an
/// untagged file is taken to be authored.
fn lut_data(size: usize, data: Vec<f32>) -> LutData {
    LutData { size, data, input_space: WorkingColorspace::Rec709Linear, output_space: WorkingColorspace::Rec709Linear }
}

pub fn load_cube(path: &Path) -> Result<LutData, LutError> {
    let text = read_lut_text(path)?;
    parse_cube(&text)
}

/// Parse .cube format from a string (testable without filesystem).
///
/// A 3D table (`LUT_3D_SIZE`), a 1D table (`LUT_1D_SIZE`), or a 1D shaper
/// followed by a 3D table (both sizes, the 1D rows first). A 1D table, and
/// a shaper with the table it feeds, are baked into one 3D table (#901);
/// they were skipped before, and a 1D file failed to load. The 1D input
/// range is `DOMAIN_MIN`/`DOMAIN_MAX` or `LUT_1D_INPUT_RANGE`, 0..1 if
/// neither is given.
pub fn parse_cube(text: &str) -> Result<LutData, LutError> {
    let mut size_3d: Option<usize> = None;
    let mut size_1d: Option<usize> = None;
    let (mut dmin, mut dmax) = ([0.0f32; 3], [1.0f32; 3]);
    let mut data = Vec::new();
    let floats = |rest: &str, line_num: usize| -> Result<Vec<f32>, LutError> {
        rest.split_whitespace().map(|t| t.parse::<f32>().map_err(|_| LutError::ParseFloat { line_num, text: t.to_string() })).collect()
    };
    let size_of = |rest: &str, line_num: usize, max: usize| -> Result<usize, LutError> {
        let n: usize = rest.trim().parse().map_err(|_| LutError::ParseFloat { line_num, text: rest.trim().to_string() })?;
        if !(2..=max).contains(&n) {
            return Err(LutError::InvalidSize(n));
        }
        Ok(n)
    };

    for (line_idx, line) in text.lines().enumerate() {
        let line = line.trim();
        let line_num = line_idx + 1;
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
            size_3d = Some(size_of(rest, line_num, 256)?);
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_1D_SIZE") {
            size_1d = Some(size_of(rest, line_num, 65536)?);
            continue;
        }
        if let Some(rest) = line.strip_prefix("DOMAIN_MIN") {
            if let [r, g, b] = floats(rest, line_num)?[..] {
                dmin = [r, g, b];
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("DOMAIN_MAX") {
            if let [r, g, b] = floats(rest, line_num)?[..] {
                dmax = [r, g, b];
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_1D_INPUT_RANGE") {
            if let [lo, hi] = floats(rest, line_num)?[..] {
                (dmin, dmax) = ([lo; 3], [hi; 3]);
            }
            continue;
        }
        // Skip any other keyword lines (LUT_3D_INPUT_RANGE, etc.)
        if line.contains('_') && line.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            continue;
        }

        // Data line: three floats
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            for (i, part) in parts.iter().take(3).enumerate() {
                let v: f32 = part.parse().map_err(|_| LutError::ParseFloat {
                    line_num,
                    text: parts[i].to_string(),
                })?;
                data.push(v);
            }
        }
    }

    let n1 = size_1d.unwrap_or(0);
    let n3 = size_3d.map(|n| n * n * n).unwrap_or(0);
    if n1 == 0 && n3 == 0 {
        return Err(LutError::MissingSize);
    }
    let got = data.len() / 3;
    if got < n1 + n3 {
        return Err(LutError::NotEnoughEntries { expected: n1 + n3, got });
    }
    let curves = (n1 > 0).then(|| {
        let channel = |c: usize| Curve::uniform((0..n1).map(|i| data[i * 3 + c]).collect(), dmin[c], dmax[c]);
        [channel(0), channel(1), channel(2)]
    });
    match (curves, size_3d) {
        // A 3D table alone, as before. Extra rows are ignored.
        (None, Some(size)) => Ok(lut_data(size, data[..n3 * 3].to_vec())),
        // A 1D table alone: each channel through its curve.
        (Some(c), None) => Ok(lut_data(BAKED_SIZE, bake(|v| [c[0].at(v[0]), c[1].at(v[1]), c[2].at(v[2])]))),
        // A shaper, then the 3D table it indexes.
        (Some(c), Some(size)) => {
            let table = &data[n1 * 3..(n1 + n3) * 3];
            Ok(lut_data(BAKED_SIZE, bake(|v| sample_3d(table, size, [c[0].at(v[0]), c[1].at(v[1]), c[2].at(v[2])]))))
        }
        (None, None) => Err(LutError::MissingSize),
    }
}

/// Load any supported LUT format by file extension (.cube, .3dl, .csp).
pub fn load_lut(path: &Path) -> Result<LutData, LutError> {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let text = read_lut_text(path)?;
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
///
/// `CSPLUTV100`, then `1D` or `3D`, an optional metadata block, three
/// per-channel curves (a count, that many inputs, that many outputs), and
/// for a 3D file its size and table. The curves of a 1D file, and a 3D
/// file's pre-LUTs with the table they index, are baked into one 3D table
/// (#901); the pre-LUTs were skipped before, and read a line per entry,
/// so a file with a long pre-LUT was misread. Identity pre-LUTs keep the
/// table as it is.
pub fn parse_csp(text: &str) -> Result<LutData, LutError> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).peekable();
    if lines.next() != Some("CSPLUTV100") {
        return Err(LutError::MissingSize);
    }
    let is_3d = match lines.next() {
        Some("3D") => true,
        Some("1D") => false,
        _ => return Err(LutError::MissingSize),
    };
    if lines.peek() == Some(&"BEGIN METADATA") {
        for line in lines.by_ref() {
            if line == "END METADATA" {
                break;
            }
        }
    }
    // Numbers as they come, across line breaks.
    let mut tokens = lines.flat_map(str::split_whitespace).enumerate();
    let mut next_f32 = || -> Result<f32, LutError> {
        let (n, t) = tokens.next().ok_or(LutError::MissingSize)?;
        t.parse::<f32>().map_err(|_| LutError::ParseFloat { line_num: n, text: t.to_string() })
    };
    let curve = |next_f32: &mut dyn FnMut() -> Result<f32, LutError>| -> Result<Curve, LutError> {
        let count = next_f32()? as usize;
        if !(2..=65536).contains(&count) {
            return Err(LutError::InvalidSize(count));
        }
        let inputs = (0..count).map(|_| next_f32()).collect::<Result<Vec<_>, _>>()?;
        let outputs = (0..count).map(|_| next_f32()).collect::<Result<Vec<_>, _>>()?;
        Ok(Curve { inputs, outputs })
    };
    let c = [curve(&mut next_f32)?, curve(&mut next_f32)?, curve(&mut next_f32)?];
    if !is_3d {
        return Ok(lut_data(BAKED_SIZE, bake(|v| [c[0].at(v[0]), c[1].at(v[1]), c[2].at(v[2])])));
    }
    // "N N N", or one N.
    let size = next_f32()? as usize;
    if !(2..=256).contains(&size) {
        return Err(LutError::InvalidSize(size));
    }
    let expected = size * size * size;
    // Room for what the text can hold (a value is two bytes at least),
    // not for what its header claims.
    let mut data = Vec::with_capacity((expected * 3).min(text.len() / 2 + 3));
    // The size may be written three times.
    let mut first = vec![next_f32()?, next_f32()?];
    if first.iter().all(|&v| v as usize == size && v.fract() == 0.0) {
        first.clear();
    }
    data.extend(first);
    while data.len() < expected * 3 {
        match next_f32() {
            Ok(v) => data.push(v),
            Err(LutError::MissingSize) => break,
            Err(e) => return Err(e),
        }
    }
    let got = data.len() / 3;
    if got < expected {
        return Err(LutError::NotEnoughEntries { expected, got });
    }
    data.truncate(expected * 3);
    if c.iter().all(Curve::is_identity) {
        return Ok(lut_data(size, data));
    }
    Ok(lut_data(BAKED_SIZE, bake(|v| sample_3d(&data, size, [c[0].at(v[0]), c[1].at(v[1]), c[2].at(v[2])]))))
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

/// The value `lut` gives `rgb` (each 0..1), trilinearly, as
/// [`apply_3d_lut`] reads it.
pub fn lookup_3d(lut: &LutData, rgb: [f32; 3]) -> [f32; 3] {
    sample_3d(&lut.data, lut.size.max(2), rgb)
}

/// One LUT that does `first` and then `then`: sampled on a grid as fine
/// as the finer of the two. For a viewer that runs a look and then a
/// monitor's calibration in one pass.
pub fn compose(first: &LutData, then: &LutData) -> LutData {
    let size = first.size.max(then.size).clamp(2, 65);
    let n1 = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let mid = lookup_3d(first, [r as f32 / n1, g as f32 / n1, b as f32 / n1]);
                let out = lookup_3d(then, [mid[0].clamp(0.0, 1.0), mid[1].clamp(0.0, 1.0), mid[2].clamp(0.0, 1.0)]);
                data.extend_from_slice(&out);
            }
        }
    }
    LutData { size, data, input_space: first.input_space, output_space: then.output_space }
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
///   2. Encode to sRGB, interpolate the LUT (tetrahedral or trilinear),
///      decode the result from sRGB — the GPU LUT shader's order, and
///      the encoding a `.cube` look is built for. Input and output are
///      clamped to `[0, 1]` around the lookup, as on the GPU.
///   3. If `working_space != lut.output_space`, convert the LUT
///      output back into the working space.
///   4. Mix with the original input by `opacity`, in the working
///      space.
///
/// `rgba_linear` is `&mut [f32]` RGBA in linear-light values, with
/// alpha left untouched.
///
/// For untagged LUTs (the common case — both `input_space` and
/// `output_space` default to `Rec709Linear` after parsing) and a
/// `Rec709Linear` working space, the primaries matrices are skipped.
/// The bracketing kicks in only when one of the three colorspaces
/// differs. The GPU LUT pass brackets with the same matrices
/// ([`lut_bracket`]), so a tag means the same on both (#422).
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
pub fn apply_3d_lut_linear_ws_with_overrides(
    rgba_linear: &mut [f32],
    lut: &LutData,
    opacity: f32,
    working_space: WorkingColorspace,
    input_space: WorkingColorspace,
    output_space: WorkingColorspace,
    tetrahedral: bool,
) {
    let (pre, post) = lut_bracket(working_space, input_space, output_space);
    klipp_effects::apply_3d_lut_bracketed(rgba_linear, &lut.data, lut.size, opacity, pre, post, tetrahedral);
}

/// A 3×3 primaries matrix, rows first.
pub type Matrix3 = [[f32; 3]; 3];

/// The primaries matrices around a LUT lookup: working space into the
/// LUT's input tag before, its output tag back to the working space
/// after. `None` where the two already agree. The GPU LUT pass takes
/// the same pair, so a tag means the same on both.
pub fn lut_bracket(
    working_space: WorkingColorspace,
    input_space: WorkingColorspace,
    output_space: WorkingColorspace,
) -> (Option<Matrix3>, Option<Matrix3>) {
    (
        crate::primaries::working_space_conversion_matrix(working_space, input_space),
        crate::primaries::working_space_conversion_matrix(output_space, working_space),
    )
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

    /// A CSP whose header claims a 256-cube and holds a handful of values
    /// is refused without first reserving room for the whole cube.
    #[test]
    fn a_csp_claiming_more_than_it_holds_is_refused() {
        let text = "CSPLUTV100\n3D\n\n2\n0 1\n0 1\n2\n0 1\n0 1\n2\n0 1\n0 1\n\n256 256 256\n0 0 0\n";
        assert!(parse_csp(text).is_err());
    }


    /// A LUT that inverts each channel, on a 5-point grid.
    fn invert(size: usize) -> LutData {
        let mut lut = LutData::identity(size);
        for v in &mut lut.data {
            *v = 1.0 - *v;
        }
        lut
    }

    /// One LUT for a look then a calibration: the same as running the
    /// two in turn, and a LUT composed with identity is itself.
    #[test]
    fn a_composed_lut_runs_both_in_turn() {
        let look = invert(5);
        let mut calibration = LutData::identity(9);
        for v in &mut calibration.data {
            *v = (*v * 0.8 + 0.1).min(1.0);
        }
        let both = compose(&look, &calibration);
        assert_eq!(both.size, 9);
        for rgb in [[0.0, 0.0, 0.0], [0.25, 0.5, 0.75], [1.0, 0.3, 0.6], [0.9, 0.9, 0.1]] {
            let in_turn = lookup_3d(&calibration, lookup_3d(&look, rgb));
            let at_once = lookup_3d(&both, rgb);
            for ch in 0..3 {
                assert!((in_turn[ch] - at_once[ch]).abs() < 1e-3, "{rgb:?}: {in_turn:?} vs {at_once:?}");
            }
        }
        let same = compose(&look, &LutData::identity(2));
        for rgb in [[0.1, 0.2, 0.3], [0.8, 0.5, 0.0]] {
            let (a, b) = (lookup_3d(&look, rgb), lookup_3d(&same, rgb));
            assert!((0..3).all(|c| (a[c] - b[c]).abs() < 1e-4), "{a:?} vs {b:?}");
        }
    }
    use super::*;

    /// #824: a LUT is read only up to the limit, and one past it is
    /// refused rather than held whole in memory.
    #[test]
    fn a_lut_past_the_limit_is_refused() {
        let cube = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let limit = cube.len() as u64;
        let text = read_text_capped(cube.as_bytes(), limit).unwrap();
        assert_eq!(parse_cube(&text).unwrap().size, 2);
        let past = format!("{cube}# one byte more than the limit allows\n");
        assert!(matches!(read_text_capped(past.as_bytes(), limit), Err(LutError::TooLarge { .. })));
        // An endless stream stops at the limit too.
        assert!(matches!(read_text_capped(std::io::repeat(b'0'), 1024), Err(LutError::TooLarge { limit: 1024 })));
    }

    fn identity_lut(size: usize) -> LutData {
        // Existing tests below use this local helper; it now just defers
        // to the public LutData::identity so the public path stays
        // exercised by the same passthrough assertions.
        LutData::identity(size)
    }

    /// The baked table read at `rgb`.
    fn read(lut: &LutData, rgb: [f32; 3]) -> [f32; 3] {
        super::sample_3d(&lut.data, lut.size, rgb)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 2e-3)
    }

    /// #901: a 1D .cube was refused for want of a 3D size; now each
    /// channel goes through its curve.
    #[test]
    fn a_1d_cube_is_each_channel_through_its_curve() {
        // Squares the red, halves the green, keeps the blue: 5 rows over 0..1.
        let rows: String = (0..5).map(|i| { let x = i as f32 / 4.0; format!("{} {} {}\n", x * x, x / 2.0, x) }).collect();
        let lut = parse_cube(&format!("TITLE \"curve\"\nLUT_1D_SIZE 5\n{rows}")).unwrap();
        assert_eq!(lut.size, 65);
        assert!(close(read(&lut, [0.5, 0.5, 0.5]), [0.25, 0.25, 0.5]), "{:?}", read(&lut, [0.5, 0.5, 0.5]));
        assert!(close(read(&lut, [1.0, 1.0, 0.0]), [1.0, 0.5, 0.0]));
        // Between rows the curve is linear: 0.375 lies between 0.25 and 0.5.
        assert!((read(&lut, [0.375, 0.0, 0.0])[0] - (0.0625 + 0.25) / 2.0).abs() < 2e-3);
    }

    /// #901: a 1D table's range from DOMAIN_MIN/MAX.
    #[test]
    fn a_1d_cube_reads_its_domain() {
        let lut = parse_cube("LUT_1D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 0.5 0.5 0.5\n0 0 0\n1 1 1\n").unwrap();
        assert!(close(read(&lut, [0.25, 0.5, 1.0]), [0.5, 1.0, 1.0]), "0.25 is half way through 0..0.5, and past 0.5 the curve holds");
    }

    /// #901: a shaper, then the 3D table it indexes.
    #[test]
    fn a_shaper_feeds_the_3d_table() {
        // Shaper: x -> x/2. Table: identity, size 2. So the whole is x/2.
        let mut text = String::from("LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n0 0 0\n0.5 0.5 0.5\n");
        for b in 0..2 { for g in 0..2 { for r in 0..2 { text += &format!("{r} {g} {b}\n"); } } }
        let lut = parse_cube(&text).unwrap();
        assert!(close(read(&lut, [1.0, 0.5, 0.0]), [0.5, 0.25, 0.0]), "{:?}", read(&lut, [1.0, 0.5, 0.0]));
    }

    /// A 3D cube is kept as it is, at its own size.
    #[test]
    fn a_3d_cube_alone_is_kept_at_its_size() {
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 { for g in 0..2 { for r in 0..2 { text += &format!("{r} {g} {b}\n"); } } }
        assert_eq!(parse_cube(&text).unwrap().size, 2);
    }

    fn identity_csp_table(size: usize) -> String {
        let n = (size - 1) as f32;
        let mut t = format!("{size} {size} {size}\n");
        for b in 0..size { for g in 0..size { for r in 0..size { t += &format!("{} {} {}\n", r as f32 / n, g as f32 / n, b as f32 / n); } } }
        t
    }

    /// #901: identity pre-LUTs keep the table; a long pre-LUT is read by
    /// its values, not a line per entry, and applied.
    #[test]
    fn csp_pre_luts_are_read_and_applied() {
        let ident = "2\n0 1\n0 1\n";
        let plain = parse_csp(&format!("CSPLUTV100\n3D\n\n{ident}{ident}{ident}\n{}", identity_csp_table(3))).unwrap();
        assert_eq!(plain.size, 3, "identity pre-LUTs: the table as it is");
        // Red's pre-LUT halves it, in five points on one line each way.
        let half = "5\n0 0.25 0.5 0.75 1\n0 0.125 0.25 0.375 0.5\n";
        let lut = parse_csp(&format!("CSPLUTV100\n3D\nBEGIN METADATA\nmade by a test\nEND METADATA\n{half}{ident}{ident}{}", identity_csp_table(3))).unwrap();
        assert!(close(read(&lut, [1.0, 1.0, 0.5]), [0.5, 1.0, 0.5]), "{:?}", read(&lut, [1.0, 1.0, 0.5]));
    }

    /// #901: a 1D CSP was refused; its curves are the LUT.
    #[test]
    fn a_1d_csp_is_its_curves() {
        let invert = "2\n0 1\n1 0\n";
        let ident = "2\n0 1\n0 1\n";
        let lut = parse_csp(&format!("CSPLUTV100\n1D\n{invert}{ident}{ident}")).unwrap();
        assert!(close(read(&lut, [0.25, 0.25, 0.25]), [0.75, 0.25, 0.25]));
    }

    /// Every bundled LUT still loads (#901 rewrote the parsers).
    #[test]
    fn every_bundled_lut_loads() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/luts");
        let mut seen = 0;
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "cube" || e == "csp" || e == "3dl") {
                    let lut = load_lut(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                    assert_eq!(lut.data.len(), lut.size.pow(3) * 3, "{}", path.display());
                    seen += 1;
                }
            }
        }
        assert!(seen > 0, "no bundled LUTs found");
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
        // Size 2 is the smallest identity LUT and is
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

    // ─── .cube export ────────────────────────────────────────────────

    #[test]
    fn cube_export_format_valid() {
        // A ramp, not an identity LUT: a writer that dropped or
        // reordered entries would still look right on a LUT whose
        // every entry is the same shape.
        let n = 17usize;
        let mut lut_data = Vec::with_capacity(n * n * n);
        for bi in 0..n {
            for gi in 0..n {
                for ri in 0..n {
                    let d = (n - 1) as f32;
                    lut_data.push([ri as f32 / d, gi as f32 / d, bi as f32 / d]);
                }
            }
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir()
            .join(format!("klipp_test_lut_format_{}_{nanos}.cube", std::process::id()));
        write_cube(&path, &lut_data, n as u32, "Format Test").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines[0].starts_with("TITLE"), "first line should be TITLE");
        let size_line = lines.iter().find(|l| l.starts_with("LUT_3D_SIZE")).unwrap();
        assert_eq!(*size_line, "LUT_3D_SIZE 17");

        let data_lines: Vec<&&str> = lines.iter()
            .filter(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                parts.len() == 3 && parts[0].parse::<f32>().is_ok()
            })
            .collect();
        assert_eq!(data_lines.len(), n * n * n,
            "should have {} data lines, got {}", n * n * n, data_lines.len());

        for line in &data_lines {
            for val_str in line.split_whitespace() {
                let v: f32 = val_str.parse().unwrap();
                assert!((0.0..=1.0).contains(&v),
                    "LUT value out of range: {v} in line '{line}'");
            }
        }
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
    /// space: no bracketing, so the result is the plain lookup — the
    /// LUT's sRGB-encoded output decoded to linear. The conversion-
    /// matrix lookup returns `None` and the matmul is skipped entirely.
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
        // The LUT's 0.5 is an sRGB-encoded value, as a `.cube` look's
        // output is: linear 0.214, what the GPU shader gives.
        let want = crate::transfer::srgb_to_linear(0.5);
        assert!(approx_eq(pixels[0], want, 1e-3), "got {} want {want}", pixels[0]);
        assert!(approx_eq(pixels[1], want, 1e-3));
        assert!(approx_eq(pixels[2], want, 1e-3));
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
