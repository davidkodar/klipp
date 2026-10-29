//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> bytes: array<u32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.w || gid.y >= p.h { return; }
    let i = gid.y * p.w + gid.x;
    out[i] = f32((bytes[i / 4u] >> (8u * (i % 4u))) & 255u) / 255.0;
}
