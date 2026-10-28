
struct Params {
    width: u32,
    height: u32,
    threshold: f32,
    knee: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers


fn soft_clip_value(x: f32, threshold: f32, knee: f32) -> f32 {
    if x <= threshold { return x; }
    if knee <= 0.000001 { return threshold; }
    let max_remaining = 1.0 - threshold;
    if max_remaining <= 0.0 { return min(x, threshold); }
    let excess = x - threshold;
    let rolloff = max_remaining * (1.0 - exp(-excess / knee));
    return min(threshold + rolloff, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let r = soft_clip_value(orig.x, params.threshold, params.knee);
    let g = soft_clip_value(orig.y, params.threshold, params.knee);
    let b = soft_clip_value(orig.z, params.threshold, params.knee);

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
