
struct Params { width: u32, height: u32, vertical: u32, band: f32 }
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
    var u = (f32(idx % params.width) + 0.5) / f32(params.width);
    if params.vertical != 0u { u = (f32(idx / params.width) + 0.5) / f32(params.height); }
    let d = abs(u - 0.5) * 2.0;
    var m = clamp((d - params.band) / max(1.0 - params.band, 0.001), 0.0, 1.0);
    m = m * m * (3.0 - 2.0 * m);
    // Mixed premultiplied, as the blurred copy is, then back to straight.
    let o = unpack(original[idx]);
    let op = vec4<f32>(o.xyz * o.w, o.w);
    let mixed = op + (unpack(blurred[idx]) - op) * m;
    var rgb = vec3<f32>(0.0);
    if mixed.w > 1e-6 { rgb = mixed.xyz / mixed.w; }
    output[idx] = pack(vec4<f32>(rgb, mixed.w));
}
