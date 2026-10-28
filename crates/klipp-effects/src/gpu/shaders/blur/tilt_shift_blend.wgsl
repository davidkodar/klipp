
struct Params {
    width: u32, height: u32, center: f32, band: f32,
    softness: f32, _pad: u32, _pad2: u32, _pad3: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> blurred: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let o = unpack(original[idx]);
    let bl = unpack(blurred[idx]);
    let ny = (f32(y) + 0.5) / f32(params.height);
    let d = abs(ny - params.center) - params.band * 0.5;
    var m = clamp(d / max(params.softness, 0.001), 0.0, 1.0);
    m = m * m * (3.0 - 2.0 * m);
    output[idx] = pack(vec4<f32>(o.xyz + (bl.xyz - o.xyz) * m, o.w));
}
