// The Veylvik viewer's picture (scene-linear Rec.709, premultiplied) as
// Klipp's display encode takes a frame: straight colour in the project's
// working space. `KlippDisplay::straight_working` is the twin.

struct Params {
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
    count: u32, _p0: u32, _p1: u32, _p2: u32,
}
@group(0) @binding(0) var<storage, read> picture: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> straight: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = gid.x + gid.y * groups.x * 256u;
    if (i >= params.count) { return; }
    let p = picture[i];
    var c = vec3<f32>(0.0);
    if (p.w > 1e-6) { c = p.xyz / p.w; }
    let w = vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
    straight[i] = vec4<f32>(w, p.w);
}
