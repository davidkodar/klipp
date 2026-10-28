
struct Params {
    width: u32, height: u32, threshold: f32, premultiply: u32,
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
    let px = unpack(input[idx]);
    if params.premultiply == 1u {
        // A glow's light: the picture seen through its alpha, and above a
        // threshold its highlights alone, eased in (#2763).
        var weight = 1.0;
        if params.threshold > 0.0 {
            let l = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), px.xyz);
            weight = select(0.0, clamp((l - params.threshold) / (1.0 - params.threshold + 1e-6), 0.0, 1.0), l > params.threshold);
        }
        output[idx] = pack(vec4<f32>(px.xyz * px.w * weight, px.w));
        return;
    }
    let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), px.xyz);
    if luma > params.threshold {
        // A weight, not a ramp: normalising by 1 - threshold handed a 12x
        // highlight a factor of 34, so the bloom grew the light it spread.
        let factor = clamp((luma - params.threshold) / (1.0 - params.threshold + 1e-6), 0.0, 1.0);
        output[idx] = pack(vec4<f32>(px.xyz * factor, px.w));
    } else {
        output[idx] = pack(vec4<f32>(0.0, 0.0, 0.0, 0.0));
    }
}
