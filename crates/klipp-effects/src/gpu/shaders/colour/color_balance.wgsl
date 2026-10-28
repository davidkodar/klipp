
struct Params {
    width: u32,
    height: u32,
    wb0: f32, wb1: f32, wb2: f32,
    wb3: f32, wb4: f32, wb5: f32,
    wb6: f32, wb7: f32, wb8: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    // Temperature / tint: the white-balance matrix.
    var r = params.wb0 * orig.x + params.wb1 * orig.y + params.wb2 * orig.z;
    var g = params.wb3 * orig.x + params.wb4 * orig.y + params.wb5 * orig.z;
    var b = params.wb6 * orig.x + params.wb7 * orig.y + params.wb8 * orig.z;

    // Hue rotation, in HSL (the Corrector's hue, the same maths).
    if abs(params.hue) >= 0.01 {
        let rot = hue_rotate_hsl(vec3<f32>(r, g, b), params.hue);
        r = rot.x;
        g = rot.y;
        b = rot.z;
    }

    // Global mix
    r = orig.x + (r - orig.x) * params.global_mix;
    g = orig.y + (g - orig.y) * params.global_mix;
    b = orig.z + (b - orig.z) * params.global_mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
