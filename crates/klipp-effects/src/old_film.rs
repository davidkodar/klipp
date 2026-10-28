//! Old Film (#2744): a picture aged and damaged as film is. Each part has
//! its amount, and `amount` scales the damage together:
//!
//! * the colour aged (`age`): faded, yellowed, its blacks lifted, less
//!   saturated and flatter; black and white (`mono`) or sepia toned;
//! * flicker, uneven over the frame and drifting from frame to frame;
//! * gate weave, a frame line at the gate's edge (`frame_lines`);
//! * scratches running down the frame;
//! * debris (`dust`): a few specks a frame, often none, each lasting one
//!   or two frames in one place, mostly tiny with the odd blotch, soft
//!   and ragged, with now and then a hair; mostly white (dirt on the
//!   negative), some black (dirt on the print);
//! * stains: soft discoloured patches with a water mark's rim, drifting
//!   slowly;
//! * light leaks and burned edges.
//!
//! The pattern is the timeline frame's (`seed`), so a frame draws the
//! same damage in the viewer and the export. The GPU runs
//! `src/gpu/shaders/temporal/old_film.wgsl`, the CPU [`cpu`], the same
//! computation.

/// The effect's numbers as both computations take them.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    pub flicker: f32,
    pub scratches: f32,
    pub dust: f32,
    pub weave: f32,
    pub sepia: f32,
    pub seed: u32,
    pub luma: [f32; 3],
    /// Scales the damage: flicker, scratches, debris, stains, leaks and
    /// burn.
    pub amount: f32,
    pub age: f32,
    pub mono: f32,
    pub stains: f32,
    pub frame_lines: f32,
    pub leaks: f32,
    pub burn: f32,
    pub _pad: [u32; 2],
}
// 20 fields x 4 bytes = 80 bytes

/// The debris slots a frame has: each holds a speck or nothing.
pub const DEBRIS_SLOTS: u32 = 8;

/// A hash of `p` to 0..1, the same on both computations.
pub fn hash(p: u32) -> f32 {
    let mut x = p;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = (x >> 16) ^ x;
    x as f32 / 4294967295.0
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Smooth value noise at (`x`, `y`), 0..1, its pattern `s`'s.
fn vnoise(x: f32, y: f32, s: u32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (ix, iy) = (ix as i32 as u32, iy as i32 as u32);
    let at = |dx: u32, dy: u32| hash(ix.wrapping_add(dx).wrapping_mul(73_856_093) ^ iy.wrapping_add(dy).wrapping_mul(19_349_663) ^ s);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    mix(mix(at(0, 0), at(1, 0), sx), mix(at(0, 1), at(1, 1), sx), sy)
}

/// One debris slot on frame `seed`: where its speck is, how big, whether
/// white, whether a hair, its bend and direction; `None` when the slot
/// is empty that frame. A speck stays one or two frames in one place.
pub fn speck(p: &Params, k: u32) -> Option<[f32; 7]> {
    let life = if hash(k.wrapping_mul(101).wrapping_add(7)) > 0.55 { 2 } else { 1 };
    let epoch = p.seed.wrapping_add(k.wrapping_mul(3)) / life;
    let key = epoch.wrapping_mul(7919).wrapping_add(k.wrapping_mul(104_729));
    if hash(key.wrapping_add(2)) >= p.dust * p.amount * 0.18 {
        return None;
    }
    let s = p.height as f32 / 1080.0;
    let u = hash(key.wrapping_add(5));
    let radius = (1.0 + 2.5 * u + 30.0 * u.powi(32)) * s;
    let white = if hash(key.wrapping_add(6)) < 0.75 { 1.0 } else { 0.0 };
    let hair = if hash(key.wrapping_add(7)) < 0.15 { 1.0 } else { 0.0 };
    Some([
        hash(key.wrapping_add(3)) * p.width as f32,
        hash(key.wrapping_add(4)) * p.height as f32,
        radius,
        white,
        hair,
        (hash(key.wrapping_add(8)) - 0.5) * 2.0,
        hash(key.wrapping_add(9)) * std::f32::consts::TAU,
    ])
}

/// How much of slot `k`'s speck covers (`x`, `y`): 0 to 1.
fn debris_cover(p: &Params, k: u32, x: f32, y: f32) -> (f32, f32) {
    let Some([sx, sy, radius, white, hair, bend, angle]) = speck(p, k) else { return (0.0, 0.0) };
    let s = p.height as f32 / 1080.0;
    let (dx, dy) = (x - sx, y - sy);
    if hair > 0.5 {
        // A fibre: a thin curve 30 to 90 px long, bent along its length.
        let len = (30.0 + 60.0 * (radius / s / 31.0).min(1.0) + 30.0 * bend.abs()) * s;
        let (c, n) = (angle.cos(), angle.sin());
        let t = dx * c + dy * n;
        if !(0.0..=len).contains(&t) {
            return (0.0, white);
        }
        let off = -dx * n + dy * c - bend * len * 0.15 * (t / len * 4.712_389).sin();
        return (1.0 - smoothstep(0.4 * s, 1.2 * s, off.abs()), white);
    }
    if dx.abs() > radius * 1.6 || dy.abs() > radius * 1.6 {
        return (0.0, white);
    }
    // Ragged: the edge pushed in and out by a noise at the speck's scale.
    let d = (dx * dx + dy * dy).sqrt() / radius.max(1e-3);
    let ragged = d * (0.75 + 0.5 * vnoise(dx / radius.max(1e-3) * 1.7 + 17.0 * k as f32, dy / radius.max(1e-3) * 1.7, p.seed.wrapping_add(k)));
    (1.0 - smoothstep(0.6, 1.0, ragged), white)
}

/// The picture at (`x`, `y`) of the frame `at` reads, straight RGBA.
pub fn pixel(p: &Params, at: &dyn Fn(i32, i32) -> [f32; 4], x: u32, y: u32) -> [f32; 4] {
    let (wf, hf) = (p.width as f32, p.height as f32);
    let s = hf / 1080.0;
    let fseed = p.seed.wrapping_mul(2_654_435_761);
    let damage = p.amount.max(0.0);
    // Weave: the gate's shift this frame.
    let wx = (hash(fseed.wrapping_add(11)) - 0.5) * p.weave * 6.0 * wf / 1920.0;
    let wy = (hash(fseed.wrapping_add(23)) - 0.5) * p.weave * 6.0 * hf / 1080.0;
    let o = at((x as f32 + wx).round() as i32, (y as f32 + wy).round() as i32);
    let mut c = [o[0], o[1], o[2]];
    let luma = |c: [f32; 3]| p.luma[0] * c[0] + p.luma[1] * c[1] + p.luma[2] * c[2];
    // Aged, in an encoding near the eye's: flatter, faded, yellowed.
    let a = p.age.clamp(0.0, 1.0);
    let mut e = c.map(|v| v.max(0.0).powf(1.0 / 2.2));
    let le = luma(e);
    let warm = [1.0 + 0.07 * a, 1.0 + 0.02 * a, 1.0 - 0.13 * a];
    for i in 0..3 {
        let sat = mix(le, e[i], 1.0 - 0.55 * a);
        let flat = 0.5 + (sat - 0.5) * (1.0 - 0.35 * a);
        e[i] = (flat * (1.0 - 0.10 * a) + 0.07 * a) * warm[i];
    }
    c = e.map(|v| v.max(0.0).powf(2.2));
    let l = luma(c);
    c = c.map(|v| mix(v, l, p.mono.clamp(0.0, 1.0)));
    let l = luma(c);
    let tone = [1.07, 0.93, 0.72];
    for i in 0..3 {
        c[i] += (l * tone[i] - c[i]) * p.sepia;
    }
    // Flicker: drifting from frame to frame, a little uneven over it.
    let t = p.seed as f32 * 0.7;
    let slow = mix(hash((t.floor() as u32).wrapping_add(401)), hash((t.floor() as u32).wrapping_add(402)), smoothstep(0.0, 1.0, t.fract()));
    let tilt = hash(fseed.wrapping_add(5)) * std::f32::consts::TAU;
    let (nx, ny) = ((x as f32 - wf * 0.5) / (wf * 0.5), (y as f32 - hf * 0.5) / (hf * 0.5));
    let flick = 1.0 + p.flicker * damage * (0.18 * ((slow - 0.5) * 1.4 + (hash(fseed) - 0.5) * 0.6) + 0.05 * (nx * tilt.cos() + ny * tilt.sin()));
    c = c.map(|v| v * flick);
    // Stains: three soft patches with a darker rim, drifting slowly.
    if p.stains > 0.0 {
        for k in 0..3u32 {
            let drift = p.seed as f32 * 0.004 + k as f32 * 2.1;
            let (cx, cy) = ((hash(k * 13 + 1) + 0.04 * drift.sin()) * wf, (hash(k * 13 + 2) + 0.03 * (drift * 0.7).cos()) * hf);
            let r = (0.06 + 0.12 * hash(k * 13 + 3)) * hf;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / r + 0.25 * (vnoise(x as f32 / (r * 0.5), y as f32 / (r * 0.5), k + 900) - 0.5);
            let m = (1.0 - smoothstep(0.7, 1.0, d)) * p.stains * damage * 0.5;
            let rim = smoothstep(0.85, 0.95, d) * (1.0 - smoothstep(0.95, 1.05, d)) * p.stains * damage * 0.25;
            let tint = [0.85, 0.72, 0.5];
            for i in 0..3 {
                c[i] *= mix(1.0, tint[i], m) * (1.0 - rim);
            }
        }
    }
    // Scratches: lines down the frame, wandering a pixel.
    let n_scr = (p.scratches * damage * 4.0).round() as u32;
    let life = p.seed / 6;
    for k in 0..n_scr {
        let sx = hash(life.wrapping_add(k.wrapping_mul(97)).wrapping_mul(7919)) * wf;
        let jitter = (hash(p.seed.wrapping_add(k.wrapping_mul(31))) - 0.5) * 2.0;
        if (x as f32 - (sx + jitter)).abs() < 0.8 {
            c = c.map(|v| v + (1.0 - v) * 0.35);
        }
    }
    // Debris.
    for k in 0..DEBRIS_SLOTS {
        let (m, white) = debris_cover(p, k, x as f32, y as f32);
        if m > 0.0 {
            let (to, strength) = if white > 0.5 { (0.92, 0.85) } else { (0.02, 0.9) };
            c = c.map(|v| mix(v, to, m * strength));
        }
    }
    // The frame line at the gate's top and bottom, moving with the weave.
    if p.frame_lines > 0.0 {
        let edge = (y as f32 + wy).min(hf - y as f32 - wy);
        let m = p.frame_lines * (1.0 - smoothstep(0.0, 0.035 * hf, edge));
        c = c.map(|v| v * (1.0 - 0.85 * m));
    }
    // A light leak from one side, wandering.
    if p.leaks > 0.0 {
        let side = if hash((p.seed / 48).wrapping_add(77)) > 0.5 { wf } else { 0.0 };
        let ly = hf * (0.3 + 0.4 * (p.seed as f32 * 0.013).sin().abs());
        let d = ((x as f32 - side).powi(2) + (y as f32 - ly).powi(2)).sqrt() / (0.3 * wf);
        let g = 0.5 * p.leaks * damage * (-d * d).exp() * (0.6 + 0.4 * (p.seed as f32 * 0.05).sin());
        let glow = [1.0, 0.45, 0.15];
        for i in 0..3 {
            c[i] += g * glow[i];
        }
    }
    // Burned edges: darker and orange toward the frame's border.
    if p.burn > 0.0 {
        let edge = (x as f32).min(wf - x as f32).min(y as f32).min(hf - y as f32) / (0.12 * wf.min(hf));
        let e = edge + 0.3 * (vnoise(x as f32 / (40.0 * s), y as f32 / (40.0 * s), 555) - 0.5);
        let b = p.burn * damage * (1.0 - smoothstep(0.0, 1.0, e));
        let scorch = [1.0, 0.55, 0.25];
        for i in 0..3 {
            c[i] = mix(c[i], c[i] * scorch[i] * 0.6, b) * (1.0 - 0.8 * b * b);
        }
    }
    let vig = 1.0 - 0.25 * (nx * nx + ny * ny);
    [c[0] * vig, c[1] * vig, c[2] * vig, o[3]]
}

/// The CPU's Old Film of `pixels`, straight RGBA `p.width` × `p.height`.
pub fn cpu(pixels: &[f32], p: &Params) -> Vec<f32> {
    let (w, h) = (p.width as i32, p.height as i32);
    let at = |x: i32, y: i32| {
        let (x, y) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let mut out = vec![0.0; pixels.len()];
    for y in 0..p.height {
        for x in 0..p.width {
            let i = ((y * p.width + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&pixel(p, &at, x, y));
        }
    }
    out
}

/// The templates: a look in one click, then Age and Damage (`age` and
/// `amount`) to move it. Each name, its label, and its settings.
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    ("super8", "Super 8", "A home movie: warm, grainy, lively flicker, the gate's edge showing"),
    ("film_16mm", "16 mm", "A documentary print: lightly faded, a few scratches and specks"),
    ("silent_film", "Silent Film", "The 1920s in black and white: heavy flicker, scratches, burned edges"),
    ("faded_70s", "1970s Faded", "Colour gone warm and flat with age, the odd leak of light"),
];

/// The template `name`'s effect (see [`TEMPLATES`]).
pub fn template(name: &str) -> Option<crate::BuiltinEffect> {
    let e = |flicker, scratches, dust, weave, sepia, age, mono, stains, frame_lines, leaks, burn| crate::BuiltinEffect::OldFilm {
        flicker, scratches, dust, weave, sepia, seed: 0, amount: 1.0, age, mono, stains, frame_lines, leaks, burn,
    };
    Some(match name {
        "super8" => e(0.6, 0.3, 0.6, 0.8, 0.1, 0.55, 0.0, 0.1, 0.6, 0.35, 0.2),
        "film_16mm" => e(0.3, 0.4, 0.4, 0.4, 0.0, 0.35, 0.0, 0.05, 0.0, 0.0, 0.0),
        "silent_film" => e(0.9, 0.8, 0.8, 0.7, 0.15, 0.6, 1.0, 0.25, 0.4, 0.0, 0.45),
        "faded_70s" => e(0.2, 0.15, 0.3, 0.2, 0.0, 0.9, 0.0, 0.1, 0.0, 0.25, 0.0),
        _ => return None,
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    fn params(seed: u32, dust: f32) -> Params {
        Params {
            width: 1920,
            height: 1080,
            flicker: 0.0,
            scratches: 0.0,
            dust,
            weave: 0.0,
            sepia: 0.0,
            seed,
            luma: [0.2126, 0.7152, 0.0722],
            amount: 1.0,
            age: 0.0,
            mono: 0.0,
            stains: 0.0,
            frame_lines: 0.0,
            leaks: 0.0,
            burn: 0.0,
            _pad: [0; 2],
        }
    }

    /// #2744: debris is sparse, a few specks a frame and often none,
    /// mostly tiny with the odd blotch, mostly white; each speck stays
    /// one or two frames in one place, never longer.
    #[test]
    fn debris_is_sparse_and_short_lived() {
        let frames = 400u32;
        let specks: Vec<Vec<[f32; 7]>> = (0..frames).map(|f| (0..DEBRIS_SLOTS).filter_map(|k| speck(&params(f, 1.0), k)).collect()).collect();
        let counts: Vec<usize> = specks.iter().map(Vec::len).collect();
        let empty = counts.iter().filter(|&&n| n == 0).count();
        let mean = counts.iter().sum::<usize>() as f32 / frames as f32;
        assert!((0.8..2.5).contains(&mean), "{mean} specks a frame");
        assert!(empty * 10 >= frames as usize, "often none: {empty} empty frames of {frames}");
        assert!(counts.iter().all(|&n| n <= 6), "never many: {:?}", counts.iter().max());
        let all: Vec<&[f32; 7]> = specks.iter().flatten().collect();
        let tiny = all.iter().filter(|s| s[2] < 4.0).count();
        let blotch = all.iter().filter(|s| s[2] > 12.0).count();
        let white = all.iter().filter(|s| s[3] > 0.5).count();
        assert!(tiny * 10 >= all.len() * 7 && blotch >= 1 && blotch * 20 < all.len(), "{tiny} tiny, {blotch} blotches of {}", all.len());
        assert!(white * 10 >= all.len() * 6 && white < all.len(), "{white} white of {}", all.len());
        // Lifetime: a slot's speck at one place for one or two frames.
        for k in 0..DEBRIS_SLOTS {
            let mut run = 0;
            let mut last: Option<[f32; 2]> = None;
            for f in 0..frames {
                let now = speck(&params(f, 1.0), k).map(|s| [s[0], s[1]]);
                run = if now.is_some() && now == last { run + 1 } else { usize::from(now.is_some()) };
                assert!(run <= 2, "slot {k}: one speck {run} frames at frame {f}");
                last = now;
            }
        }
        assert!(speck(&params(10, 0.0), 0).is_none() && (0..frames).all(|f| (0..DEBRIS_SLOTS).all(|k| speck(&params(f, 0.0), k).is_none())), "no dust, no debris");
    }

    /// Age fades the colour: less saturated, flatter, warmer, its blacks
    /// lifted.
    #[test]
    fn age_fades_warms_and_lifts() {
        let picture = |c: [f32; 4]| move |_: i32, _: i32| c;
        let aged = |c: [f32; 4], age: f32| pixel(&Params { age, ..params(0, 0.0) }, &picture(c), 960, 540);
        let red = aged([0.6, 0.05, 0.05, 1.0], 1.0);
        let sat = |c: [f32; 4]| (c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) / c[0].max(c[1]).max(c[2]);
        assert!(sat(red) < sat([0.6, 0.05, 0.05, 1.0]) * 0.75, "less saturated: {red:?}");
        let grey = aged([0.18, 0.18, 0.18, 1.0], 1.0);
        assert!(grey[0] > grey[2] * 1.15, "warmer grey: {grey:?}");
        let black = aged([0.0, 0.0, 0.0, 1.0], 1.0);
        let white = aged([1.0, 1.0, 1.0, 1.0], 1.0);
        assert!(black[1] > 0.002 && white[1] < 0.95, "blacks lifted, whites dimmed: {black:?} {white:?}");
        assert!(aged([0.3, 0.2, 0.1, 1.0], 0.0)[..3].iter().zip([0.3f32, 0.2, 0.1]).all(|(a, b)| (a - b).abs() < 1e-4), "no age, no change");
    }

    /// Every template is an Old Film; a name not among them is none.
    #[test]
    fn the_templates_are_old_films() {
        for (name, _, _) in TEMPLATES {
            assert!(matches!(template(name), Some(crate::BuiltinEffect::OldFilm { .. })), "{name}");
        }
        assert!(template("vhs").is_none());
    }
}
