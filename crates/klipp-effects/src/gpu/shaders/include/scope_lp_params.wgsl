
struct LpParams {
    width: u32, height: u32, out_w: u32, out_h: u32,
    r: u32, radius: u32, transfer: u32, _p0: u32,
    sigma: f32, scale: f32, _p1: f32, _p2: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
};
@group(0) @binding(1) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: LpParams;
fn tap_weight(d: i32) -> f32 {
    return exp(-f32(d * d) / (2.0 * p.sigma * p.sigma));
}
