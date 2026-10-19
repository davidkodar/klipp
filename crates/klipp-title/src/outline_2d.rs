//! Retained 2D font edges. Preserve the font's winding; the 3D mesh's
//! nesting/hole normalization is not appropriate for signed coverage.
use ttf_parser::OutlineBuilder;

struct Edges {
    at: [f32; 2],
    start: [f32; 2],
    max_area: f32,
    lines: Vec<[f32; 4]>,
}
impl Edges {
    fn line(&mut self, a: [f32; 2], b: [f32; 2]) {
        if a[1] != b[1] {
            self.lines.push([a[0], a[1], b[0], b[1]]);
        }
    }
    fn curve(&mut self, end: [f32; 2], point: impl Fn(f32) -> [f32; 2]) {
        let mut stack = vec![(0.0, self.at, 1.0, end)];
        while let Some((ta, a, tb, b)) = stack.pop() {
            let tm = (ta + tb) * 0.5;
            let m = point(tm);
            let area = (m[0] - a[0]) * (b[1] - a[1]) - (b[0] - a[0]) * (m[1] - a[1]);
            if area.abs() > self.max_area && tb - ta > 1.0 / 65536.0 {
                stack.push((ta, a, tm, m));
                stack.push((tm, m, tb, b));
            } else {
                self.line(a, b);
            }
        }
        self.at = end;
    }
}
impl OutlineBuilder for Edges {
    fn move_to(&mut self, x: f32, y: f32) {
        self.at = [x, y];
        self.start = self.at;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let next = [x, y];
        self.line(self.at, next);
        self.at = next;
    }
    fn quad_to(&mut self, x: f32, y: f32, ex: f32, ey: f32) {
        let a = self.at;
        let b = [x, y];
        let c = [ex, ey];
        self.curve(c, |t| {
            let u = 1.0 - t;
            [
                u * u * a[0] + 2.0 * u * t * b[0] + t * t * c[0],
                u * u * a[1] + 2.0 * u * t * b[1] + t * t * c[1],
            ]
        });
    }
    fn curve_to(&mut self, x: f32, y: f32, x2: f32, y2: f32, ex: f32, ey: f32) {
        let a = self.at;
        let b = [x, y];
        let c = [x2, y2];
        let d = [ex, ey];
        self.curve(d, |t| {
            let u = 1.0 - t;
            std::array::from_fn(|i| {
                u * u * u * a[i]
                    + 3.0 * u * u * t * b[i]
                    + 3.0 * u * t * t * c[i]
                    + t * t * t * d[i]
            })
        });
    }
    fn close(&mut self) {
        self.line(self.at, self.start);
        self.at = self.start;
    }
}

pub(crate) fn edges(face: &ttf_parser::Face, glyph: u16) -> Option<Vec<[f32; 4]>> {
    let units = face.units_per_em().max(1) as f32;
    // Match fontdue's default 40px geometry optimization, including its
    // midpoint triangle-area criterion. Reusing more finely flattened 3D
    // outlines changes the established font coverage at curved edges.
    let mut builder = Edges {
        at: [0.0; 2],
        start: [0.0; 2],
        max_area: 6.0 * units / 40.0,
        lines: Vec::new(),
    };
    face.outline_glyph(ttf_parser::GlyphId(glyph), &mut builder)?;
    let scale = 2048.0 / units;
    for e in &mut builder.lines {
        e[0] *= scale;
        e[1] *= -scale;
        e[2] *= scale;
        e[3] *= -scale;
    }
    Some(builder.lines)
}

/// Row bins avoid scanning a glyph's complete outline at every pixel. A pixel
/// spanning bins counts each edge only in the first intersecting bin.
pub(crate) fn row_bins(edges: Vec<[f32; 4]>) -> Option<Vec<[f32; 4]>> {
    const BINS: usize = 64;
    if edges.is_empty() || edges.len() > 8192 {
        return None;
    }
    let min = edges
        .iter()
        .map(|e| e[1].min(e[3]))
        .fold(f32::INFINITY, f32::min);
    let max = edges
        .iter()
        .map(|e| e[1].max(e[3]))
        .fold(f32::NEG_INFINITY, f32::max);
    let span = (max - min).max(0.000001);
    let bin = |y: f32| (((y - min) / span * BINS as f32).floor() as usize).min(BINS - 1);
    let mut buckets: Vec<Vec<[f32; 4]>> = vec![Vec::new(); BINS];
    for edge in edges {
        for bucket in &mut buckets[bin(edge[1].min(edge[3]))..=bin(edge[1].max(edge[3]))] {
            bucket.push(edge);
        }
    }
    let mut out = vec![[0.0; 4]; BINS + 1];
    out[0] = [min, span, BINS as f32, 0.0];
    for (i, bucket) in buckets.into_iter().enumerate() {
        out[i + 1] = [out.len() as f32, bucket.len() as f32, 0.0, 0.0];
        out.extend(bucket);
    }
    (out.len() <= 65536).then_some(out)
}
