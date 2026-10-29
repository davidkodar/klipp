//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> cells: array<vec2<i32>>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> points: array<f32>;
@group(0) @binding(5) var<storage, read_write> hist: array<atomic<u32>>;
fn chan_at(c: u32, x: i32, y: i32) -> f32 {
    let i = u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1));
    if c == 0u { return c0[i]; }
    return c1[i];
}
// A strength's bin, on a log scale from 2^-24 to 2^40, sixteen to a power of two.
fn strength_bin(v: f32) -> u32 {
    if v <= 0.0 { return 0u; }
    return u32(clamp((log2(v) + 24.0) * 16.0, 0.0, 1023.0));
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n { return; }
    let cell = cells[i];
    let x = cell.x;
    let y = cell.y;
    let a = (f32(x) / p.k - p.cx) / p.s;
    let b = (f32(y) / p.k - p.cy) / p.s;
    let per_u = p.s * p.k;
    let o = i * POINT;
    points[o] = a;
    points[o + 1u] = b;
    for (var c = 0u; c < 2u; c++) {
        points[o + 2u + c] = chan_at(c, x, y);
        let gx = (chan_at(c, x + 1, y) - chan_at(c, x - 1, y)) * 0.5 * per_u;
        let gy = (chan_at(c, x, y + 1) - chan_at(c, x, y - 1)) * 0.5 * per_u;
        var d0 = array<f32, 8>(a, b, 1.0, 0.0, 0.0, 0.0, -a * a, -a * b);
        var d1 = array<f32, 8>(0.0, 0.0, 0.0, a, b, 1.0, -a * b, -b * b);
        var strength = 0.0;
        for (var j = 0u; j < 8u; j++) {
            let sd = gx * d0[j] + gy * d1[j];
            points[o + 4u + c * 8u + j] = sd;
            strength += sd * sd;
        }
        points[o + 20u + c] = strength;
        atomicAdd(&hist[c * BINS + strength_bin(strength)], 1u);
    }
}
