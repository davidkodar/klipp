//! Snap a quad to a flat surface around a point (#684 phase 2): a click
//! inside a phone screen, a monitor or a sign gives its four corners.
//!
//! Such a surface is a four-sided patch set off from what is round it by
//! a straight edge on every side. Candidate quads come two ways: from the
//! straight edges in the picture (a Hough vote by gradient direction),
//! four lines round the click, two either side of it each way; and from
//! regions grown from the click by colour, whose convex hull fills the
//! holes what a screen shows leaves. Each side is moved onto the nearest
//! strong edge and refitted. A quad counts only when an edge runs along
//! most of every side, and of those the largest is kept: the surface's
//! outer edge, a screen's bezel included, which a pin's inset allows for.
//! Nothing is found where no candidate has an edge all round, and the
//! caller falls back to corners placed by hand.

/// The long side the picture is searched at, in pixels.
const SIDE: usize = 480;

/// Four corners, top left, top right, bottom right, bottom left, as
/// fractions of the picture.
pub type Quad = [[f32; 2]; 4];

struct Image {
    w: usize,
    h: usize,
    /// Blurred RGB, 0..255.
    rgb: Vec<[f32; 3]>,
    /// Gradient magnitude of the blurred luma, and its direction.
    grad: Vec<f32>,
    gdir: Vec<[f32; 2]>,
}

impl Image {
    fn new(rgba: &[u8], width: usize, height: usize) -> Option<Image> {
        if width < 8 || height < 8 || rgba.len() < width * height * 4 {
            return None;
        }
        let scale = (SIDE as f32 / width.max(height) as f32).min(1.0);
        let (w, h) = (((width as f32 * scale).round() as usize).max(8), ((height as f32 * scale).round() as usize).max(8));
        // An area average down to the search size.
        let mut rgb = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            let (y0, y1) = (y * height / h, ((y + 1) * height / h).max(y * height / h + 1));
            for x in 0..w {
                let (x0, x1) = (x * width / w, ((x + 1) * width / w).max(x * width / w + 1));
                let mut acc = [0.0f32; 3];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let i = (sy * width + sx) * 4;
                        for c in 0..3 {
                            acc[c] += rgba[i + c] as f32;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as f32;
                rgb[y * w + x] = acc.map(|v| v / n);
            }
        }
        // Two passes of a 3x3 box: noise and the screen's own pixel grid go.
        for _ in 0..2 {
            let src = rgb.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut acc = [0.0f32; 3];
                    let mut n = 0.0;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                            if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                                let p = src[sy as usize * w + sx as usize];
                                for c in 0..3 {
                                    acc[c] += p[c];
                                }
                                n += 1.0;
                            }
                        }
                    }
                    rgb[y * w + x] = acc.map(|v| v / n);
                }
            }
        }
        let luma: Vec<f32> = rgb.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
        let mut grad = vec![0.0f32; w * h];
        let mut gdir = vec![[0.0f32; 2]; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let l = |dx: i32, dy: i32| luma[(y as i32 + dy) as usize * w + (x as i32 + dx) as usize];
                let gx = l(1, -1) + 2.0 * l(1, 0) + l(1, 1) - l(-1, -1) - 2.0 * l(-1, 0) - l(-1, 1);
                let gy = l(-1, 1) + 2.0 * l(0, 1) + l(1, 1) - l(-1, -1) - 2.0 * l(0, -1) - l(1, -1);
                let g = gx.hypot(gy);
                grad[y * w + x] = g;
                if g > 0.0 {
                    gdir[y * w + x] = [gx / g, gy / g];
                }
            }
        }
        Some(Image { w, h, rgb, grad, gdir })
    }

    /// The gradient at a point across a line of `normal`: its strength
    /// where it runs that way, nothing where it runs along the line.
    fn across(&self, x: f32, y: f32, normal: [f32; 2]) -> f32 {
        let (xi, yi) = (x.round() as i32, y.round() as i32);
        if xi < 1 || yi < 1 || xi as usize + 1 >= self.w || yi as usize + 1 >= self.h {
            return 0.0;
        }
        let i = yi as usize * self.w + xi as usize;
        let d = self.gdir[i];
        if (d[0] * normal[0] + d[1] * normal[1]).abs() < 0.85 {
            return 0.0;
        }
        self.grad[i]
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The pixels reached from `seed`, four-connected, for which `joins`
/// says yes; `None` when the region runs to the picture's edge, since a
/// surface inside the frame does not.
fn grow(img: &Image, seed: (usize, usize), joins: impl Fn(usize, usize) -> bool) -> Option<Vec<bool>> {
    let mut inside = vec![false; img.w * img.h];
    let mut stack = vec![seed];
    inside[seed.1 * img.w + seed.0] = true;
    while let Some((x, y)) = stack.pop() {
        if x == 0 || y == 0 || x + 1 == img.w || y + 1 == img.h {
            return None;
        }
        let here = y * img.w + x;
        for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            let there = ny * img.w + nx;
            if !inside[there] && joins(here, there) {
                inside[there] = true;
                stack.push((nx, ny));
            }
        }
    }
    Some(inside)
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// The convex hull, counter-clockwise in screen terms (y down).
fn hull(mut pts: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut lower: Vec<[f32; 2]> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<[f32; 2]> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn area(poly: &[[f32; 2]]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| cross([0.0, 0.0], poly[i], poly[(i + 1) % n])).sum::<f32>().abs() * 0.5
}

/// The largest quadrilateral on the hull's vertices: for a hull that is
/// nearly a quad, its corners.
fn largest_quad(hull: &[[f32; 2]]) -> Option<[[f32; 2]; 4]> {
    // At most forty vertices, evenly taken, keeps the search small.
    let step = hull.len().div_ceil(40).max(1);
    let v: Vec<[f32; 2]> = hull.iter().step_by(step).copied().collect();
    let n = v.len();
    if n < 4 {
        return None;
    }
    let mut best = (0.0f32, [0usize; 4]);
    for a in 0..n {
        for b in a + 1..n {
            for c in b + 1..n {
                for d in c + 1..n {
                    let q = [v[a], v[b], v[c], v[d]];
                    let s = area(&q);
                    if s > best.0 {
                        best = (s, [a, b, c, d]);
                    }
                }
            }
        }
    }
    (best.0 > 0.0).then(|| best.1.map(|i| v[i]))
}

/// A line through points, by total least squares: a point on it and its
/// direction.
fn fit_line(pts: &[[f32; 2]]) -> Option<([f32; 2], [f32; 2])> {
    if pts.len() < 4 {
        return None;
    }
    let n = pts.len() as f32;
    let (mx, my) = (pts.iter().map(|p| p[0]).sum::<f32>() / n, pts.iter().map(|p| p[1]).sum::<f32>() / n);
    let (mut sxx, mut sxy, mut syy) = (0.0f32, 0.0f32, 0.0f32);
    for p in pts {
        let (dx, dy) = (p[0] - mx, p[1] - my);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    Some(([mx, my], [theta.cos(), theta.sin()]))
}

fn intersect(a: ([f32; 2], [f32; 2]), b: ([f32; 2], [f32; 2])) -> Option<[f32; 2]> {
    let (p, r) = a;
    let (q, s) = b;
    let denom = r[0] * s[1] - r[1] * s[0];
    if denom.abs() < 1e-4 {
        return None;
    }
    let t = ((q[0] - p[0]) * s[1] - (q[1] - p[1]) * s[0]) / denom;
    Some([p[0] + t * r[0], p[1] + t * r[1]])
}

/// How much of the side from `a` to `b` an edge runs along: the share of
/// points down its middle with a strong gradient across it close by.
fn support(img: &Image, a: [f32; 2], b: [f32; 2], strong: f32) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    if len < 4.0 {
        return 0.0;
    }
    let normal = [-dy / len, dx / len];
    let samples = (len / 3.0).clamp(8.0, 40.0) as usize;
    let mut hits = 0;
    for k in 0..samples {
        let t = 0.08 + 0.84 * (k as f32 + 0.5) / samples as f32;
        let p = [a[0] + dx * t, a[1] + dy * t];
        if [-1.5f32, -0.75, 0.0, 0.75, 1.5].iter().any(|o| img.across(p[0] + normal[0] * o, p[1] + normal[1] * o, normal) > strong) {
            hits += 1;
        }
    }
    hits as f32 / samples as f32
}

/// The weakest side's support.
fn least_support(img: &Image, q: &[[f32; 2]; 4], strong: f32) -> f32 {
    (0..4).map(|i| support(img, q[i], q[(i + 1) % 4], strong)).fold(f32::MAX, f32::min)
}

/// Each side of `quad` moved onto an edge across it within `reach` and
/// refitted, and the four lines met again. Along each side every crest of
/// gradient across it is found; the straight lines most of the side's
/// points agree on are kept, and of those the outermost, since a surface
/// is wanted to its outer edge (a screen's bezel, which a pin's inset
/// allows for), rather than a blend of its two edges.
fn refine(img: &Image, quad: [[f32; 2]; 4], reach: f32, strong: f32) -> Option<[[f32; 2]; 4]> {
    let centre = [quad.iter().map(|p| p[0]).sum::<f32>() / 4.0, quad.iter().map(|p| p[1]).sum::<f32>() / 4.0];
    let mut lines = Vec::with_capacity(4);
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if len < 4.0 {
            return None;
        }
        let mut normal = [-dy / len, dx / len];
        // Pointing out of the quad.
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        if (mid[0] - centre[0]) * normal[0] + (mid[1] - centre[1]) * normal[1] < 0.0 {
            normal = [-normal[0], -normal[1]];
        }
        let samples = 24;
        let steps = (reach * 2.0) as i32;
        // Inward no farther than part way to the opposite side: a thin
        // surface (a canopy seen edge on) has its other edge within reach.
        let (c, d) = (quad[(i + 2) % 4], quad[(i + 3) % 4]);
        let (ex, ey) = (d[0] - c[0], d[1] - c[1]);
        let across = ((mid[0] - c[0]) * ey - (mid[1] - c[1]) * ex).abs() / ex.hypot(ey).max(1e-6);
        let inward = steps.min((across * 0.4 * 2.0) as i32).max(1);
        // For each point down the middle of the side: the crests across it,
        // as (sample, offset outward, point).
        let mut crests: Vec<(usize, f32, [f32; 2])> = Vec::new();
        for k in 0..samples {
            let t = 0.12 + 0.76 * (k as f32 + 0.5) / samples as f32;
            let base = [a[0] + dx * t, a[1] + dy * t];
            let at = |off: f32| [base[0] + normal[0] * off, base[1] + normal[1] * off];
            let g = |off: f32| { let p = at(off); img.across(p[0], p[1], normal) };
            let peak = (-inward..=steps).map(|s| g(s as f32 * 0.5)).fold(0.0f32, f32::max);
            // Texture beside a real edge is passed over; a bezel's fainter
            // outer edge beside its bright screen is not.
            let enough = strong.max(peak * 0.3);
            for s in -inward + 1..steps {
                let off = s as f32 * 0.5;
                let here = g(off);
                if here >= enough && here >= g(off - 0.5) && here > g(off + 0.5) {
                    crests.push((k, off, at(off)));
                }
            }
        }
        // Lines through two crests of different samples, with the samples
        // that have a crest within a pixel of them.
        let inliers = |o: [f32; 2], n: [f32; 2], within: f32| -> Vec<[f32; 2]> {
            let mut by_sample: Vec<Option<(f32, [f32; 2])>> = vec![None; samples];
            for c in &crests {
                let d = ((c.2[0] - o[0]) * n[0] + (c.2[1] - o[1]) * n[1]).abs();
                if d < within && by_sample[c.0].is_none_or(|(e, _)| d < e) {
                    by_sample[c.0] = Some((d, c.2));
                }
            }
            by_sample.iter().flatten().map(|(_, p)| *p).collect()
        };
        let mut found: Vec<(f32, Vec<[f32; 2]>)> = Vec::new();
        for (u, p) in crests.iter().enumerate() {
            for q in crests.iter().skip(u + 1) {
                if p.0 == q.0 {
                    continue;
                }
                let (lx, ly) = (q.2[0] - p.2[0], q.2[1] - p.2[1]);
                let ll = lx.hypot(ly);
                // Along the side, within about 15°: two crests of a short
                // side can line up any way.
                if ll < 1.0 || (lx * dy - ly * dx).abs() > 0.26 * ll * len {
                    continue;
                }
                let on = inliers(p.2, [-ly / ll, lx / ll], 1.0);
                if on.len() * 10 < samples * 6 {
                    continue;
                }
                let outward = on.iter().map(|p| (p[0] - mid[0]) * normal[0] + (p[1] - mid[1]) * normal[1]).sum::<f32>() / on.len() as f32;
                found.push((outward, on));
            }
        }
        // The outermost edge: of the lines within two pixels of the
        // farthest out, the one most points agree on, refitted on them and
        // then on the points near that fit.
        let farthest = found.iter().map(|f| f.0).fold(f32::MIN, f32::max);
        let chosen = found.into_iter().filter(|f| f.0 >= farthest - 2.0).max_by_key(|f| f.1.len());
        let mut line = chosen.and_then(|(_, on)| fit_line(&on));
        for _ in 0..2 {
            if let Some((o, d)) = line {
                let on = inliers(o, [-d[1], d[0]], 0.75);
                if on.len() * 2 >= samples {
                    line = fit_line(&on);
                }
            }
        }
        let line = line.filter(|(_, d)| (d[0] * dy - d[1] * dx).abs() <= 0.26 * len).unwrap_or((a, [dx / len, dy / len]));
        lines.push(line);
    }
    let mut corners = [[0.0f32; 2]; 4];
    for i in 0..4 {
        corners[(i + 1) % 4] = intersect(lines[i], lines[(i + 1) % 4])?;
    }
    Some(corners)
}

/// The straight edges in the picture, strongest first: a vote over
/// angle and distance by each strong pixel, cast only for the angles its
/// gradient allows, then the peaks.
fn lines(img: &Image, strong: f32) -> Vec<([f32; 2], [f32; 2])> {
    const ANGLES: usize = 180;
    let diag = ((img.w * img.w + img.h * img.h) as f32).sqrt().ceil() as usize;
    let rhos = 2 * diag + 1;
    let mut votes = vec![0.0f32; ANGLES * rhos];
    let trig: Vec<(f32, f32)> = (0..ANGLES).map(|a| (a as f32).to_radians().sin_cos()).collect();
    for y in 1..img.h - 1 {
        for x in 1..img.w - 1 {
            let i = y * img.w + x;
            let g = img.grad[i];
            if g < strong {
                continue;
            }
            let d = img.gdir[i];
            let a0 = d[1].atan2(d[0]).to_degrees().rem_euclid(180.0).round() as i32;
            for da in -6..=6 {
                let a = (a0 + da).rem_euclid(ANGLES as i32) as usize;
                let (sin, cos) = trig[a];
                let rho = x as f32 * cos + y as f32 * sin;
                let r = (rho.round() as i32 + diag as i32) as usize;
                votes[a * rhos + r] += g;
            }
        }
    }
    let mut peaks: Vec<(f32, usize, usize)> = Vec::new();
    let least = strong * (img.w.min(img.h) as f32 * 0.06);
    for a in 0..ANGLES {
        for r in 0..rhos {
            let v = votes[a * rhos + r];
            if v < least {
                continue;
            }
            let mut top = true;
            'near: for da in -3i32..=3 {
                for dr in -4i32..=4 {
                    if da == 0 && dr == 0 {
                        continue;
                    }
                    let aa = (a as i32 + da).rem_euclid(ANGLES as i32) as usize;
                    let rr = r as i32 + dr;
                    if rr < 0 || rr as usize >= rhos {
                        continue;
                    }
                    if votes[aa * rhos + rr as usize] > v {
                        top = false;
                        break 'near;
                    }
                }
            }
            if top {
                peaks.push((v, a, r));
            }
        }
    }
    peaks.sort_by(|p, q| q.0.total_cmp(&p.0));
    peaks.truncate(28);
    peaks
        .into_iter()
        .map(|(_, a, r)| {
            let (sin, cos) = trig[a];
            let rho = r as f32 - diag as f32;
            ([cos * rho, sin * rho], [-sin, cos])
        })
        .collect()
}

/// Which side of a line a point is on, and how far.
fn side(line: &([f32; 2], [f32; 2]), p: [f32; 2]) -> f32 {
    let (o, d) = line;
    (p[0] - o[0]) * -d[1] + (p[1] - o[1]) * d[0]
}

/// Quads of four lines round the click: two on either side of it one
/// way and two the other, the pairs well apart in angle.
fn line_quads(lines: &[([f32; 2], [f32; 2])], click: [f32; 2]) -> Vec<[[f32; 2]; 4]> {
    let angle = |l: &([f32; 2], [f32; 2])| l.1[1].atan2(l.1[0]).to_degrees().rem_euclid(180.0);
    let apart = |a: f32, b: f32| {
        let d = (a - b).abs();
        d.min(180.0 - d)
    };
    let mut pairs = Vec::new();
    for i in 0..lines.len() {
        for j in i + 1..lines.len() {
            let (a, b) = (angle(&lines[i]), angle(&lines[j]));
            if apart(a, b) < 30.0 && side(&lines[i], click) * side(&lines[j], click) < 0.0 && (side(&lines[i], click).abs() > 3.0 && side(&lines[j], click).abs() > 3.0) {
                pairs.push((i, j, (a + b) * 0.5));
            }
        }
    }
    let mut quads = Vec::new();
    for p in 0..pairs.len() {
        for q in p + 1..pairs.len() {
            let (a, b, ang1) = pairs[p];
            let (c, d, ang2) = pairs[q];
            if apart(ang1, ang2) < 40.0 || [a, b].contains(&c) || [a, b].contains(&d) {
                continue;
            }
            let corners = [intersect(lines[a], lines[c]), intersect(lines[c], lines[b]), intersect(lines[b], lines[d]), intersect(lines[d], lines[a])];
            if let [Some(w), Some(x), Some(y), Some(z)] = corners {
                quads.push([w, x, y, z]);
            }
        }
    }
    quads
}

fn contains(q: &[[f32; 2]; 4], p: [f32; 2]) -> bool {
    let s: Vec<f32> = (0..4).map(|i| cross(q[i], q[(i + 1) % 4], p)).collect();
    s.iter().all(|v| *v > 0.0) || s.iter().all(|v| *v < 0.0)
}

fn convex(q: &[[f32; 2]; 4]) -> bool {
    let signs: Vec<bool> = (0..4).map(|i| cross(q[i], q[(i + 1) % 4], q[(i + 2) % 4]) > 0.0).collect();
    signs.iter().all(|s| *s) || signs.iter().all(|s| !*s)
}

/// Put a quad's corners in the order top left, top right, bottom right,
/// bottom left, clockwise on screen.
fn ordered(q: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    let c = [q.iter().map(|p| p[0]).sum::<f32>() / 4.0, q.iter().map(|p| p[1]).sum::<f32>() / 4.0];
    let mut v = q.to_vec();
    v.sort_by(|a, b| (a[1] - c[1]).atan2(a[0] - c[0]).total_cmp(&(b[1] - c[1]).atan2(b[0] - c[0])));
    // Clockwise on screen: start at the left end of the top side, the
    // side running most nearly left to right. The corner nearest the top
    // left would do for a screen, but not for a long thin surface seen
    // edge on, whose short end can put a lower corner further left.
    let across = |i: usize| {
        let (a, b) = (v[i], v[(i + 1) % 4]);
        (b[0] - a[0]) / (b[0] - a[0]).hypot(b[1] - a[1]).max(1e-6)
    };
    let start = (0..4).max_by(|&i, &j| across(i).total_cmp(&across(j))).unwrap_or(0);
    [v[start], v[(start + 1) % 4], v[(start + 2) % 4], v[(start + 3) % 4]]
}

/// The largest quad inside a grown region's convex hull, when the region
/// is a fair size and near four-sided.
fn region_quad(img: &Image, inside: &[bool]) -> Option<[[f32; 2]; 4]> {
    let total = (img.w * img.h) as f32;
    let count = inside.iter().filter(|v| **v).count() as f32;
    if count < total * 0.002 || count > total * 0.85 {
        return None;
    }
    let mut edge = Vec::new();
    for y in 1..img.h - 1 {
        for x in 1..img.w - 1 {
            let i = y * img.w + x;
            if inside[i] && (!inside[i - 1] || !inside[i + 1] || !inside[i - img.w] || !inside[i + img.w]) {
                edge.push([x as f32, y as f32]);
            }
        }
    }
    let shell = hull(edge);
    let rough = largest_quad(&shell)?;
    (area(&rough) >= area(&shell) * 0.8).then_some(rough)
}

/// Each side of `quad` (pixels of the full picture) onto the edge across
/// it nearest the line, within `reach`, to a fraction of a pixel: the
/// luma difference across the side sampled a half pixel at a time, its
/// crest found and refined by a parabola, and the line refitted.
fn sharpen(rgba: &[u8], width: usize, height: usize, quad: [[f32; 2]; 4], reach: f32) -> Option<[[f32; 2]; 4]> {
    let luma = |x: f32, y: f32| -> f32 {
        let (x, y) = (x.clamp(0.0, width as f32 - 1.001), y.clamp(0.0, height as f32 - 1.001));
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let px = |x: usize, y: usize| {
            let i = (y * width + x) * 4;
            0.2126 * rgba[i] as f32 + 0.7152 * rgba[i + 1] as f32 + 0.0722 * rgba[i + 2] as f32
        };
        let top = px(x0, y0) * (1.0 - fx) + px(x0 + 1, y0) * fx;
        let bottom = px(x0, y0 + 1) * (1.0 - fx) + px(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    };
    let mut lines = Vec::with_capacity(4);
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if len < 8.0 {
            return None;
        }
        let normal = [-dy / len, dx / len];
        let along = [dx / len, dy / len];
        let samples = (len / 4.0).clamp(12.0, 80.0) as usize;
        let steps = (reach * 2.0).ceil() as i32;
        let mut on_edge = Vec::new();
        for k in 0..samples {
            let t = 0.1 + 0.8 * (k as f32 + 0.5) / samples as f32;
            let base = [a[0] + dx * t, a[1] + dy * t];
            // The difference across a pixel, averaged over three along the
            // side, at half-pixel steps out from the line.
            let diff = |off: f32| -> f32 {
                let mut d = 0.0;
                for w in [-1.0f32, 0.0, 1.0] {
                    let p = [base[0] + normal[0] * off + along[0] * w, base[1] + normal[1] * off + along[1] * w];
                    d += luma(p[0] + normal[0] * 0.5, p[1] + normal[1] * 0.5) - luma(p[0] - normal[0] * 0.5, p[1] - normal[1] * 0.5);
                }
                (d / 3.0).abs()
            };
            let g: Vec<f32> = (-steps..=steps).map(|s| diff(s as f32 * 0.5)).collect();
            let peak = g.iter().copied().fold(0.0f32, f32::max);
            if peak < 8.0 {
                continue;
            }
            // The crest nearest the line, among those not small beside the
            // strongest.
            let mut best: Option<(f32, f32)> = None;
            for j in 1..g.len() - 1 {
                if g[j] >= peak * 0.4 && g[j] >= g[j - 1] && g[j] > g[j + 1] {
                    let denom = g[j - 1] - 2.0 * g[j] + g[j + 1];
                    let shift = if denom.abs() > 1e-6 { 0.5 * (g[j - 1] - g[j + 1]) / denom } else { 0.0 };
                    let off = (j as f32 - steps as f32 + shift.clamp(-0.5, 0.5)) * 0.5;
                    if best.is_none_or(|(o, _)| off.abs() < o.abs()) {
                        best = Some((off, g[j]));
                    }
                }
            }
            if let Some((off, _)) = best {
                on_edge.push([base[0] + normal[0] * off, base[1] + normal[1] * off]);
            }
        }
        if on_edge.len() * 2 < samples {
            return None;
        }
        // Fitted, then refitted on the points near that fit.
        let mut line = fit_line(&on_edge)?;
        let near: Vec<[f32; 2]> = on_edge.iter().copied().filter(|p| ((p[0] - line.0[0]) * -line.1[1] + (p[1] - line.0[1]) * line.1[0]).abs() < 1.0).collect();
        if near.len() * 2 >= on_edge.len() {
            line = fit_line(&near)?;
        }
        lines.push(line);
    }
    let mut corners = [[0.0f32; 2]; 4];
    for i in 0..4 {
        corners[(i + 1) % 4] = intersect(lines[i], lines[(i + 1) % 4])?;
    }
    // A sharpening that moved a corner far from where it was found is not
    // to be trusted.
    corners.iter().zip(quad.iter()).all(|(c, q)| (c[0] - q[0]).hypot(c[1] - q[1]) < reach * 2.0).then_some(corners)
}

/// The corners of the flat surface round `click` (fractions of the
/// picture) in the `width` by `height` RGBA picture, as fractions of it;
/// `None` when nothing four-sided with an edge all round is found there.
pub fn snap_quad(rgba: &[u8], width: usize, height: usize, click: (f32, f32)) -> Option<Quad> {
    candidates(rgba, width, height, click).into_iter().next()
}

/// Why no surface was found round a click (#2754).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    /// Nothing four-sided round the click has a straight edge along most
    /// of every side.
    NoClosedRegion,
    /// Something did, but in the frames after it doesn't move as one flat
    /// surface: a gap between things, a shadow, part of something bent.
    NotOnePlane,
}

impl Miss {
    /// What to tell the person who clicked.
    pub fn reason(self) -> &'static str {
        match self {
            Miss::NoClosedRegion => "no closed region with straight edges round the click",
            Miss::NotOnePlane => "the region found round the click does not move as one flat surface in the frames after it",
        }
    }
}

/// The candidates [`find`] checks against the frames after: at most this
/// many, the largest first.
const CHECKED: usize = 4;

/// The flat surface round `click` in the RGBA picture (#2754): of the
/// quads with an edge all round it, largest first, the first that the
/// planar tracker follows through `after` (the frames that follow, grey,
/// the picture's size) as one plane, confidently, each frame. With no
/// frames after, the largest quad. Corners as fractions of the picture.
pub fn find(rgba: &[u8], width: usize, height: usize, click: (f32, f32), after: &[crate::window_track::GrayFrame]) -> Result<Quad, Miss> {
    let found = candidates(rgba, width, height, click);
    if found.is_empty() {
        return Err(Miss::NoClosedRegion);
    }
    if after.is_empty() {
        return Ok(found[0]);
    }
    let reference = crate::window_track::GrayFrame {
        data: rgba.chunks_exact(4).map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8).collect(),
        width,
        height,
    };
    let frames: Vec<usize> = (0..after.len()).collect();
    let opts = crate::planar::Options { reacquire_for: 0, ..Default::default() };
    found
        .into_iter()
        .take(CHECKED)
        .find(|q| {
            let px = q.map(|c| (c[0] as f64 * width as f64, c[1] as f64 * height as f64));
            let run = crate::planar::track(&reference, px, &[], &frames, &mut |i| after.get(i).cloned(), &opts, &|_| true);
            let mean = run.iter().map(|t| t.confidence).sum::<f32>() / run.len().max(1) as f32;
            run.len() == after.len() && run.iter().all(|t| t.state != crate::planar::State::Lost) && mean >= 0.6
        })
        .ok_or(Miss::NotOnePlane)
}

/// Every quad round `click` with an edge along most of every side, in the
/// picture's fractions, the largest first (the surface's outer edge, a
/// screen's bezel included), at most [`CHECKED`].
fn candidates(rgba: &[u8], width: usize, height: usize, click: (f32, f32)) -> Vec<Quad> {
    let Some(img) = Image::new(rgba, width, height) else { return Vec::new() };
    let seed = (((click.0 * img.w as f32) as usize).clamp(1, img.w - 2), ((click.1 * img.h as f32) as usize).clamp(1, img.h - 2));
    // The clicked colour, from a small patch about the click.
    let mut colour = [0.0f32; 3];
    let mut n = 0.0;
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            let (x, y) = ((seed.0 as i32 + dx).clamp(0, img.w as i32 - 1) as usize, (seed.1 as i32 + dy).clamp(0, img.h as i32 - 1) as usize);
            let p = img.rgb[y * img.w + x];
            for c in 0..3 {
                colour[c] += p[c];
            }
            n += 1.0;
        }
    }
    let colour = colour.map(|v| v / n);
    let total = (img.w * img.h) as f32;
    let mean_grad = img.grad.iter().sum::<f32>() / total;

    // Strong: above the picture's own texture, judged by the mean and by
    // its strongest edges.
    let mut sorted = img.grad.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let top = sorted[(sorted.len() as f32 * 0.995) as usize % sorted.len()];
    let strong = (mean_grad * 3.0).max(top * 0.2).max(12.0);
    let reach = (img.w.max(img.h) as f32 * 0.015).max(3.0);
    let click_px = [seed.0 as f32, seed.1 as f32];

    let mut candidates: Vec<[[f32; 2]; 4]> = line_quads(&lines(&img, strong), click_px);
    for tol in [10.0f32, 16.0, 24.0, 34.0, 48.0, 66.0] {
        if let Some(r) = grow(&img, seed, |_, there| dist(img.rgb[there], colour) < tol) {
            candidates.extend(region_quad(&img, &r));
        }
    }
    for step in [3.0f32, 5.0, 8.0, 12.0] {
        if let Some(r) = grow(&img, seed, |here, there| dist(img.rgb[here], img.rgb[there]) < step) {
            candidates.extend(region_quad(&img, &r));
        }
    }

    // A quad with an edge along most of every side, round the click, of a
    // sensible size; the largest of those.
    // Only quads already near edges all round are worth refining, the
    // largest first, near-repeats once.
    let mut rough: Vec<(f32, [[f32; 2]; 4])> = candidates
        .into_iter()
        .filter(|q| convex(q) && contains(q, click_px))
        .filter(|q| area(q) > total * 0.004 && area(q) < total * 0.9)
        .filter(|q| least_support(&img, q, strong * 0.5) >= 0.4)
        .map(|q| (area(&q), q))
        .collect();
    rough.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut kept: Vec<[[f32; 2]; 4]> = Vec::new();
    for (_, q) in rough {
        let near = |k: &[[f32; 2]; 4]| ordered(*k).iter().zip(ordered(q).iter()).all(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1]) < 2.0);
        if !kept.iter().any(near) {
            kept.push(q);
        }
        if kept.len() == 24 {
            break;
        }
    }

    let mut good: Vec<(f32, [[f32; 2]; 4])> = Vec::new();
    for rough in kept {
        // Refined onto the edges only where that holds the edges better: a
        // sliver (a canopy seen edge on) can lose its short ends to it.
        let corners = match refine(&img, rough, reach, strong) {
            Some(r) if least_support(&img, &r, strong * 0.5) >= least_support(&img, &rough, strong * 0.5) => r,
            _ => rough,
        };
        let size = area(&corners);
        if !convex(&corners) || !contains(&corners, click_px) || size < total * 0.004 || size > total * 0.9 {
            continue;
        }
        if corners.iter().any(|c| c[0] < -2.0 || c[1] < -2.0 || c[0] > img.w as f32 + 1.0 || c[1] > img.h as f32 + 1.0) {
            continue;
        }
        // Closure: an edge along most of every side. Measured against half
        // the picture's strong edges, as the rough quads are: a surface's
        // own edge can be fainter than the brightest in the picture (sun
        // on water), and what only looks closed is turned away by the
        // frames after ([`find`]).
        if least_support(&img, &corners, strong * 0.5) < 0.7 {
            continue;
        }
        // Refined onto the same edges, two rough quads are one.
        if !good.iter().any(|(_, k)| ordered(*k).iter().zip(ordered(corners).iter()).all(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1]) < 2.0)) {
            good.push((size, corners));
        }
    }
    good.sort_by(|a, b| b.0.total_cmp(&a.0));
    // In the full picture's pixels (a search pixel's centre is half a
    // pixel in), then each side onto its edge there, to a fraction of a
    // pixel: the search's blur and scale leave the edge wide.
    let (sx, sy) = (width as f32 / img.w as f32, height as f32 / img.h as f32);
    good.into_iter()
        .take(CHECKED)
        .filter_map(|(_, corners)| {
            let rough = ordered(corners).map(|p| [(p[0] + 0.5) * sx - 0.5, (p[1] + 0.5) * sy - 0.5]);
            let fine = sharpen(rgba, width, height, rough, sx.max(sy) * 2.0 + 2.0).unwrap_or(rough);
            let q = fine.map(|p| [(p[0] + 0.5) / width as f32, (p[1] + 0.5) / height as f32]);
            q.iter().flatten().all(|v| (-0.05..=1.05).contains(v)).then_some(q)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 640;
    const H: usize = 360;

    /// Noise on grey, with a bright screen in perspective: a dark bezel
    /// round a picture with blocks of content on it.
    fn scene(quad: [[f32; 2]; 4], content: bool) -> Vec<u8> {
        let mut px = vec![0u8; W * H * 4];
        let mut seed = 12345u32;
        let mut rand = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        let inside = |q: &[[f32; 2]; 4], x: f32, y: f32| (0..4).all(|i| cross(q[i], q[(i + 1) % 4], [x, y]) >= 0.0);
        // The screen inside the bezel, a tenth in from each corner.
        let c = [q_centre(&quad)];
        let inner = quad.map(|p| [p[0] + (c[0][0] - p[0]) * 0.08, p[1] + (c[0][1] - p[1]) * 0.08]);
        for y in 0..H {
            for x in 0..W {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let n = rand() / 4;
                let mut rgb = [110 + n, 105 + n, 100 + n];
                if inside(&quad, fx, fy) {
                    rgb = [25, 25, 28];
                }
                if inside(&inner, fx, fy) {
                    rgb = [225, 230, 240];
                    if content && ((x / 24 + y / 18) % 3 == 0) {
                        rgb = [60, 120, 200];
                    }
                }
                let i = (y * W + x) * 4;
                px[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        px
    }

    fn q_centre(q: &[[f32; 2]; 4]) -> [f32; 2] {
        [q.iter().map(|p| p[0]).sum::<f32>() / 4.0, q.iter().map(|p| p[1]).sum::<f32>() / 4.0]
    }

    fn worst_px(found: Quad, truth: [[f32; 2]; 4]) -> f32 {
        found.iter().zip(truth.iter()).map(|(f, t)| (f[0] * W as f32 - t[0]).hypot(f[1] * H as f32 - t[1])).fold(0.0, f32::max)
    }

    /// Clockwise on screen, in pixels.
    const PHONE: [[f32; 2]; 4] = [[220.0, 70.0], [430.0, 90.0], [420.0, 300.0], [210.0, 280.0]];

    #[test]
    fn a_click_on_a_bright_screen_finds_its_corners() {
        let img = scene(PHONE, false);
        let c = q_centre(&PHONE);
        let q = snap_quad(&img, W, H, (c[0] / W as f32, c[1] / H as f32)).expect("a screen there");
        // The bezel's outer edge, which a pin's inset allows for, to well
        // under a pixel.
        assert!(worst_px(q, PHONE) < 1.5, "{q:?}");
    }

    #[test]
    fn what_the_screen_shows_does_not_stop_it() {
        let img = scene(PHONE, true);
        let c = q_centre(&PHONE);
        let q = snap_quad(&img, W, H, (c[0] / W as f32 + 0.01, c[1] / H as f32)).expect("a screen with content");
        assert!(worst_px(q, PHONE) < 1.5, "{q:?}");
    }

    #[test]
    fn the_bezel_clicked_gives_its_outer_corners() {
        let img = scene(PHONE, false);
        // On the bezel, a little in from the top left corner.
        let p = [PHONE[0][0] + 8.0, PHONE[0][1] + 10.0];
        let q = snap_quad(&img, W, H, (p[0] / W as f32, p[1] / H as f32)).expect("the bezel is a surface too");
        assert!(worst_px(q, PHONE) < 1.5, "the bezel's own corners: {q:?}");
    }

    #[test]
    fn plain_texture_has_no_surface() {
        let img = scene([[-10.0, -10.0], [-5.0, -10.0], [-5.0, -5.0], [-10.0, -5.0]], false);
        assert_eq!(snap_quad(&img, W, H, (0.5, 0.5)), None);
    }

    #[test]
    fn corners_come_in_order() {
        let q = ordered([[1.0, 1.0], [0.0, 0.0], [0.0, 1.0], [1.0, 0.0]]);
        assert_eq!(q, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    fn grey(rgba: &[u8]) -> crate::window_track::GrayFrame {
        crate::window_track::GrayFrame { data: rgba.chunks_exact(4).map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8).collect(), width: W, height: H }
    }

    /// A screen that moves with the camera over the frames after the
    /// click is found, its corners as the picture alone gives them.
    #[test]
    fn a_surface_that_moves_as_one_plane_is_found() {
        let img = scene(PHONE, true);
        let moved = |k: f32| PHONE.map(|p| [p[0] + 1.5 * k, p[1] + 0.6 * k + 0.02 * k * (p[0] - 300.0) / 10.0]);
        let after: Vec<_> = (1..=8).map(|k| grey(&scene(moved(k as f32), true))).collect();
        let c = q_centre(&PHONE);
        let q = find(&img, W, H, (c[0] / W as f32 + 0.01, c[1] / H as f32), &after).expect("a screen moving as one plane");
        assert!(worst_px(q, PHONE) < 1.5, "{q:?}");
        assert_eq!(find(&img, W, H, (c[0] / W as f32 + 0.01, c[1] / H as f32), &[]), Ok(q), "with no frames after, the picture alone");
    }

    /// Something four-sided in one frame that isn't there in the frames
    /// after (a reflection, a flash) is not taken for a surface; nothing
    /// four-sided at all is said apart from it.
    #[test]
    fn a_quad_that_does_not_move_as_a_plane_is_refused_and_says_why() {
        let img = scene(PHONE, false);
        let gone = grey(&scene([[-10.0, -10.0], [-5.0, -10.0], [-5.0, -5.0], [-10.0, -5.0]], false));
        let after = vec![gone; 8];
        let c = q_centre(&PHONE);
        assert_eq!(find(&img, W, H, (c[0] / W as f32, c[1] / H as f32), &after), Err(Miss::NotOnePlane));
        let plain = scene([[-10.0, -10.0], [-5.0, -10.0], [-5.0, -5.0], [-10.0, -5.0]], false);
        assert_eq!(find(&plain, W, H, (0.5, 0.5), &[]), Err(Miss::NoClosedRegion));
        assert!(Miss::NotOnePlane.reason().contains("one flat surface"));
    }

    /// A thin plain surface seen nearly edge on (a canopy), in a picture
    /// whose brightest edges (sun glints on water) are far stronger than
    /// its own: found, though its sides lie within the refining reach of
    /// each other. Its thick end is exact; its thinnest end, a few pixels
    /// at the search size, blurs into what is round it and comes out short
    /// of the tip, a start to drag the last corners from.
    #[test]
    fn a_thin_surface_among_bright_glints_is_found() {
        const SLIVER: [[f32; 2]; 4] = [[150.0, 170.0], [450.0, 192.0], [440.0, 210.0], [150.0, 178.0]];
        let mut px = vec![0u8; W * H * 4];
        let mut seed = 777u32;
        let mut rand = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        let inside = |x: f32, y: f32| (0..4).all(|i| cross(SLIVER[i], SLIVER[(i + 1) % 4], [x, y]) >= 0.0);
        for y in 0..H {
            for x in 0..W {
                let n = rand() / 8;
                // Water, with a glint every so often.
                let mut rgb = [40 + n, 70 + n, 90 + n];
                if (x * 7 + y * 13) % 97 == 0 && y > 230 {
                    rgb = [255, 255, 250];
                }
                if inside(x as f32 + 0.5, y as f32 + 0.5) {
                    rgb = [40, 150, 210];
                }
                let i = (y * W + x) * 4;
                px[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let q = snap_quad(&px, W, H, (300.0 / W as f32, 185.0 / H as f32)).expect("the sliver is a surface");
        let off = |k: usize| (q[k][0] * W as f32 - SLIVER[k][0]).hypot(q[k][1] * H as f32 - SLIVER[k][1]);
        assert!(off(1) < 3.0 && off(2) < 3.0, "the thick end: {q:?}");
        assert!(off(0) < 30.0 && off(3) < 30.0, "the thin end, short of the tip: {q:?}");
    }
}
