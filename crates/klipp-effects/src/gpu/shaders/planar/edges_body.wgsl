//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
fn at(x: i32, y: i32) -> f32 {
    return src[u32(clamp(y, 0, i32(p.h) - 1)) * p.w + u32(clamp(x, 0, i32(p.w) - 1))];
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.w || gid.y >= p.h { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let gx = (at(x + 1, y) - at(x - 1, y)) * 0.5;
    let gy = (at(x, y + 1) - at(x, y - 1)) * 0.5;
    out[gid.y * p.w + gid.x] = sqrt(gx * gx + gy * gy);
}
