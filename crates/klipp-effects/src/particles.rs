//! Particles (#1141): floating dust, falling snow, rain, sparks and
//! embers drawn over the picture, the same at every frame for a seed and
//! a time, and the same on the CPU and the GPU.
//!
//! Nothing is simulated from frame to frame. Each of three depth layers
//! is a lattice of cells, one particle a cell, sliding with the layer's
//! wind: a pixel looks back into that sliding lattice, at its own cell
//! and the eight around it, and for each particle works out where it is
//! now from its cell, its seed and the time, by formula. A particle lives
//! a lifetime, fades in and out, and is born again somewhere else in its
//! cell; it sways, strays and (sparks) falls under gravity, never more
//! than the cell's reach, so the nine cells hold every particle that can
//! touch the pixel. The far layers are smaller, slower and fainter.
//!
//! [`params`] works out the numbers both renderers use, trigonometry and
//! colour included, so the GPU only multiplies and adds what the CPU
//! does; `SHADER_PARTICLES` in `gpu.rs` and `apply` here are the same
//! per-pixel code.

use crate::BuiltinEffect;

/// The kinds, as `Particles::style` names them.
pub const KINDS: [&str; 5] = ["dust", "snow", "rain", "sparks", "embers"];

/// What the shader is given: the frame, the lattice and each kind's
/// motion, worked out on the CPU.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleParams {
    pub width: u32,
    pub height: u32,
    pub seed: u32,
    /// 1: light added (sparks, embers); 0: laid over (dust, snow, rain).
    pub additive: u32,
    /// A cell's side, in pixels.
    pub cell: f32,
    /// The wind's direction (unit) and speed, pixels a second.
    pub dir_x: f32,
    pub dir_y: f32,
    pub speed: f32,
    /// A particle's radius at the near layer, pixels.
    pub size: f32,
    /// How far a particle strays from the wind, a share of its cell.
    pub spread: f32,
    pub lifetime: f32,
    pub opacity: f32,
    /// The colour, linear.
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub time: f32,
    /// The kind's sway (share of a cell), sway rate (a second), gravity
    /// (share of a cell over a life; negative rises), streak (seconds of
    /// travel drawn as a line), twinkle (0..1) and cooling (embers dim
    /// as they age).
    pub sway: f32,
    pub sway_rate: f32,
    pub gravity: f32,
    pub streak: f32,
    pub twinkle: f32,
    pub cooling: f32,
    /// One over `cell`: the cell a pixel is in is found by multiplying,
    /// which the GPU rounds as the CPU does (its divide is approximate).
    pub inv_cell: f32,
    pub _pad1: f32,
}

/// The lattice repeats every this many cells, and the wind's slide is
/// kept within half of that either way: the coordinates stay small
/// enough that a float holds them to a thousandth of a pixel, so the
/// GPU's rounding can't move a particle visibly from the CPU's. (A
/// longer period put a slide that had just gone negative a whole period
/// away, where the two rounded a particle's edge differently.)
pub const PERIOD: f32 = 64.0;

fn hash_u(v: u32) -> u32 {
    let mut x = v;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    (x >> 16) ^ x
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    hash_u(a ^ hash_u(b ^ hash_u(c))) as f32 / 4294967295.0
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

impl BuiltinEffect {
    /// These particles at `time` seconds into their clip; anything else
    /// as it is.
    pub fn with_time(mut self, at: f32) -> Self {
        if let BuiltinEffect::Particles { time, .. } = &mut self {
            *time = at;
        }
        self
    }

    /// These particles `k` times as many and as opaque as they can be;
    /// anything else as it is.
    pub fn denser(mut self, k: f32) -> Self {
        if let BuiltinEffect::Particles { amount, opacity, .. } = &mut self {
            *amount *= k;
            *opacity = (*opacity * k).min(1.0);
        }
        self
    }
}

/// Particles of `kind` with that kind's own look: its preset, and what
/// a reset or the assistant's `snow` gives. The particle presets that go
/// by a kind's name ([`KINDS`], `crate::presets`); an unknown kind is
/// dust.
pub fn preset(kind: u32) -> BuiltinEffect {
    let name = KINDS.get(kind as usize).copied().unwrap_or(KINDS[0]);
    crate::presets::keyed("particles", name)
        .or_else(|| crate::presets::keyed("particles", KINDS[0]))
        .map_or_else(|| (crate::registry::effect_type("particles").expect("particles are described").sample)(), |p| p.effect.clone())
}

/// The numbers a Particles effect draws with on a `width` × `height`
/// frame, or `None` for any other effect or one that draws nothing.
pub fn params(effect: &BuiltinEffect, width: u32, height: u32) -> Option<ParticleParams> {
    let BuiltinEffect::Particles { style: kind, amount, size, speed, direction, spread, lifetime, opacity, color, seed, time } = *effect else { return None };
    if amount < 1.0 || opacity <= 0.0 || size <= 0.0 {
        return None;
    }
    // Sizes and speeds are pixels of a 1080-line frame.
    let k = height as f32 / 1080.0;
    // sway, sway rate, gravity, streak, twinkle, additive, cooling
    let (sway, sway_rate, gravity, streak, twinkle, additive, cooling) = match kind {
        1 => (0.3, 0.45, 0.0, 0.0, 0.0, 0, 0.0),
        2 => (0.0, 0.0, 0.0, 0.03, 0.0, 0, 0.0),
        3 => (0.04, 3.0, 0.45, 0.02, 0.35, 1, 0.0),
        4 => (0.25, 0.7, -0.15, 0.0, 0.6, 1, 0.7),
        _ => (0.25, 0.25, 0.0, 0.0, 0.5, 0, 0.0),
    };
    let size = size * k;
    let speed = speed * k;
    // One particle a cell over three layers; a cell never so small that
    // a particle reaches past the cells round it: its stray (at most 0.45
    // of a cell), its radius and its streak (a quarter each) stay short
    // of a whole cell, so the nine cells round a pixel hold every
    // particle that can touch it, however the pixel's cell is rounded.
    let per_layer = (amount / 3.0).max(1.0);
    let mut cell = ((width as f32 * height as f32) / per_layer).sqrt();
    cell = cell.max(size * 1.4 / 0.25).max(speed * streak / 0.25).max(4.0);
    let ang = direction.to_radians();
    Some(ParticleParams {
        width,
        height,
        seed,
        additive,
        cell,
        dir_x: ang.cos(),
        dir_y: ang.sin(),
        speed,
        size,
        spread: spread.clamp(0.0, 1.0) * 0.4,
        lifetime: lifetime.max(0.05),
        opacity: opacity.clamp(0.0, 1.0),
        r: srgb_to_linear(color[0]),
        g: srgb_to_linear(color[1]),
        b: srgb_to_linear(color[2]),
        time,
        sway,
        sway_rate,
        gravity,
        streak,
        twinkle,
        cooling,
        inv_cell: 1.0 / cell,
        _pad1: 0.0,
    })
}

/// A smooth wave through -1..1 once per unit of `t`: a rounded triangle,
/// made of sums and products only, so the GPU's is the CPU's to the bit
/// (a GPU's `sin` is only roughly the CPU's).
fn wave(t: f32) -> f32 {
    let x = t - t.floor();
    let tri = 1.0 - 4.0 * (x - 0.5).abs();
    tri * (1.5 - 0.5 * tri * tri)
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Pixel `(x, y)` with `px` under it, particles drawn: the shader's `main`.
fn pixel(p: &ParticleParams, x: u32, y: u32, px: [f32; 4]) -> [f32; 4] {
    let mut c = px;
    let fx = x as f32 + 0.5;
    let fy = y as f32 + 0.5;
    for layer in 0..3u32 {
        // Far (0) to near (2).
        let z = 0.55 + 0.225 * layer as f32;
        let s = p.cell;
        let span = s * PERIOD;
        let mut shift_x = p.dir_x * p.speed * z * p.time;
        let mut shift_y = p.dir_y * p.speed * z * p.time;
        shift_x = shift_x - (shift_x / span + 0.5).floor() * span;
        shift_y = shift_y - (shift_y / span + 0.5).floor() * span;
        // Each layer's lattice is offset, so the layers don't line up.
        let ux = fx - shift_x + s * 0.37 * layer as f32;
        let uy = fy - shift_y + s * 0.61 * layer as f32;
        let cx0 = (ux * p.inv_cell).floor();
        let cy0 = (uy * p.inv_cell).floor();
        let key = p.seed.wrapping_mul(131).wrapping_add(layer * 7919);
        for j in 0..9u32 {
            let cxf = cx0 + (j % 3) as f32 - 1.0;
            let cyf = cy0 + (j / 3) as f32 - 1.0;
            let ci = (cxf - (cxf / PERIOD).floor() * PERIOD) as u32;
            let cj = (cyf - (cyf / PERIOD).floor() * PERIOD) as u32;
            let h1 = rand3(ci, cj, key);
            let h2 = rand3(ci, cj, key + 1);
            let h3 = rand3(ci, cj, key + 2);
            let h4 = rand3(ci, cj, key + 3);
            let h5 = rand3(ci, cj, key + 4);
            let life = p.lifetime * (0.6 + 0.8 * h1);
            let q = p.time / life + h2;
            let cycle = q.floor();
            let age = q - cycle;
            // Born again somewhere else in its cell each life.
            let born = hash_u(cycle as i32 as u32);
            let ax = (cxf + rand3(ci ^ born, cj, key + 5)) * s;
            let ay = (cyf + rand3(ci, cj ^ born, key + 6)) * s;
            // Sway across the wind, stray, gravity; held to the reach.
            let sw = wave(p.time * p.sway_rate + h3) * p.sway * s;
            let (sx, sy) = (wave(h4), wave(h4 + 0.25));
            let mut dx = -p.dir_y * sw + sx * p.spread * s * age;
            let mut dy = p.dir_x * sw + sy * p.spread * s * age + p.gravity * s * age * age;
            let len = (dx * dx + dy * dy).sqrt();
            let reach = 0.45 * s;
            if len > reach {
                dx *= reach / len;
                dy *= reach / len;
            }
            let px_ = ax + dx;
            let py_ = ay + dy;
            // Distance to the particle, or to its streak behind it.
            let mut ex = ux - px_;
            let mut ey = uy - py_;
            let tail = p.speed * z * p.streak;
            if tail > 0.0 {
                let along = (ex * -p.dir_x + ey * -p.dir_y).clamp(0.0, tail);
                ex += p.dir_x * along;
                ey += p.dir_y * along;
            }
            let dist = (ex * ex + ey * ey).sqrt();
            // A speck smaller than a pixel is drawn a pixel across, as
            // faint as its size: a sub-pixel disc only flickers.
            let true_radius = (p.size * z * (0.6 + 0.8 * h5)).min(0.25 * s);
            let radius = true_radius.max(1.0).min(0.25 * s);
            if dist >= radius {
                continue;
            }
            let f = 1.0 - dist / radius;
            let mut a = (true_radius / radius) * (true_radius / radius) * p.opacity * (0.45 + 0.55 * z) * smoothstep(0.0, 0.12, age) * (1.0 - smoothstep(0.65, 1.0, age)) * f * f * (3.0 - 2.0 * f);
            if p.twinkle > 0.0 {
                let tw = wave(p.time * 2.3 + h3 * 7.0);
                a *= 1.0 - p.twinkle * 0.5 * (1.0 + tw);
            }
            let warm = 1.0 - p.cooling * age;
            if p.additive == 1 {
                c[0] += p.r * warm * a * 1.5;
                c[1] += p.g * warm * warm * a * 1.5;
                c[2] += p.b * warm * warm * warm * a * 1.5;
                c[3] = c[3] + a * (1.0 - c[3]);
            } else {
                c[0] = c[0] + (p.r - c[0]) * a;
                c[1] = c[1] + (p.g - c[1]) * a;
                c[2] = c[2] + (p.b - c[2]) * a;
                c[3] = c[3] + a * (1.0 - c[3]);
            }
        }
    }
    c
}

/// `p`, a `w` × `h` linear frame, with `effect`'s particles drawn: the
/// picture `SHADER_PARTICLES` draws on the GPU.
pub(crate) fn apply(effect: &BuiltinEffect, p: Vec<f32>, w: u32, h: u32) -> Vec<f32> {
    let Some(pp) = params(effect, w, h) else { return p };
    crate::cpu::each_pixel(&p, w, h, |x, y, f| pixel(&pp, x as u32, y as u32, f.at(x, y)))
}
