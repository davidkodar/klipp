
struct Params {
    width: u32, height: u32, threshold: f32, half_len: u32,
    intensity: f32, tint: f32, _pad: u32, _pad2: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
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
    let x = idx % params.width;
    let y = idx / params.width;
    let tint_r = 1.0 + (1.0 - params.tint) * 0.3;
    let tint_b = 1.0 + params.tint * 0.5;
    var streak = vec3<f32>(0.0);
    let x_start = max(i32(x) - i32(params.half_len), 0);
    let x_end = min(x + params.half_len + 1u, params.width);
    let count = f32(x_end - u32(x_start));
    for (var sx: u32 = u32(x_start); sx < x_end; sx = sx + 1u) {
        let px = unpack(input[y * params.width + sx]);
        let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), px.xyz);
        if luma > params.threshold {
            let factor = clamp((luma - params.threshold) / (1.0 - params.threshold + 1e-6), 0.0, 1.0);
            streak = streak + px.xyz * factor;
        }
    }
    streak = streak / count;
    let orig = unpack(input[idx]);
    let out = vec3<f32>(
        orig.x + streak.x * tint_r * params.intensity,
        orig.y + streak.y * params.intensity,
        orig.z + streak.z * tint_b * params.intensity,
    );
    output[idx] = pack(vec4<f32>(out, orig.w));
}
