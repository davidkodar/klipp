
struct AccParams {
    width: u32, height: u32, step: u32, cols: u32,
    rows: u32, transfer: u32, scale: f32, _pad: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>, luma: vec4<f32>,
    lo: f32, hi: f32, _p2: u32, _p3: u32,
};
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> p: AccParams;

fn srgb_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}
fn pq_encode(v: f32) -> f32 {
    let y = clamp(v, 0.0, 1.0);
    let n = pow(y, 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * n) / (1.0 + 18.6875 * n), 78.84375);
}
fn encode(v: f32) -> f32 {
    switch p.transfer {
        case 1u: { return pow(clamp(v, 0.0, 1.0), 1.0 / 2.4); }
        case 2u: { return pq_encode(v * p.scale); }
        default: { return srgb_encode(v); }
    }
}
// `wgt` units of channel `ch` at level `v` (0..1) into bin column `c`,
// split by distance between the two levels around it.
fn splat(ch: u32, c: u32, v: f32, wgt: f32) {
    let r = clamp(v, 0.0, 1.0) * f32(p.rows - 1u);
    let r0 = u32(floor(r));
    let fr = r - f32(r0);
    let r1 = min(r0 + 1u, p.rows - 1u);
    let base = ch * p.cols * p.rows + c * p.rows;
    atomicAdd(&bins[base + r0], u32((1.0 - fr) * wgt + 0.5));
    atomicAdd(&bins[base + r1], u32(fr * wgt + 0.5));
}
// One sample at source column `sx`. A sampled column stands for the
// `step` columns from it, and its 256 units go to the bin columns that
// span covers in proportion to the overlap. Exact coverage is what
// keeps a picture whose columns do not divide evenly into the bins from
// striping; it also means a sparse sampling leaves no bin column empty
// and a narrow picture fills the width.
fn bin_sample(sx: u32, e: vec3<f32>) {
    let y = dot(p.luma.xyz, e);
    let scale = f32(p.cols) / f32(p.width);
    let lo = f32(sx) * scale;
    let hi = min(f32(sx + p.step) * scale, f32(p.cols));
    let c_from = u32(floor(lo));
    let c_to = min(u32(ceil(hi)), p.cols);
    for (var c = c_from; c < c_to; c = c + 1u) {
        let overlap = min(f32(c + 1u), hi) - max(f32(c), lo);
        let wgt = 256.0 * overlap / (hi - lo);
        if (wgt < 0.5) { continue; }
        splat(0u, c, e.r, wgt);
        splat(1u, c, e.g, wgt);
        splat(2u, c, e.b, wgt);
        splat(3u, c, y, wgt);
    }
}
