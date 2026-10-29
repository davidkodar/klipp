//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> state: array<f32>;
@group(0) @binding(4) var<storage, read_write> res: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read> strong: array<f32>;
// A residual's bin, on a square-root scale over 0 to 2.
fn residual_bin(r: f32) -> u32 {
    return u32(clamp(sqrt(abs(r) / 2.0) * 1024.0, 0.0, 1023.0));
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n { return; }
    let v = seen[i];
    if v.z < 0.5 || state[20] > 0.5 { return; }
    let o = i * POINT;
    let u = vec2<f32>(points[o], points[o + 1u]);
    let r0 = (v.x - state[10] - state[11] * u.x - state[12] * u.y) / state[9] - points[o + 2u];
    let r1 = (v.y - state[14]) / state[13] - points[o + 3u];
    res[i] = vec2<f32>(r0, r1);
    if points[o + 20u] >= strong[0] { atomicAdd(&hist[residual_bin(r0)], 1u); }
    if points[o + 21u] >= strong[1] { atomicAdd(&hist[residual_bin(r1)], 1u); }
}
