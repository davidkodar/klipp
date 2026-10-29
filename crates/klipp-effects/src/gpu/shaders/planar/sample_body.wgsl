//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> seen: array<vec4<f32>>;
//#include planar_sample
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let id = vec3<u32>(id_flat.x + id_flat.y * nwg.x * 64u, 0u, id_flat.z);
    let i = id.x;
    if i >= p.n || state[20] > 0.5 { return; }
    let o = i * POINT;
    let m = map_point(vec2<f32>(points[o], points[o + 1u]));
    if m.z < 0.0 { seen[i] = vec4<f32>(0.0); return; }
    let a = sample(0u, m.x, m.y);
    let b = sample(1u, m.x, m.y);
    seen[i] = vec4<f32>(a.v, b.v, select(0.0, 1.0, a.ok && b.ok), 0.0);
}
