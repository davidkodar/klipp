
struct Params {
    width: u32, height: u32,
    center_x: f32, center_y: f32,
    threshold: f32, samples: u32,
    decay: f32, intensity: f32,
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
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = params.center_x * f32(params.width);
    let cy = params.center_y * f32(params.height);
    let dx = x - cx;
    let dy = y - cy;
    let step = params.decay / f32(params.samples);

    var ray = vec3<f32>(0.0);
    var weight: f32 = 1.0;
    let decay_per_sample = 1.0 - step;

    for (var s: u32 = 0u; s < params.samples; s = s + 1u) {
        let t = f32(s) * step;
        let sx = clamp(u32(x - dx * t), 0u, params.width - 1u);
        let sy = clamp(u32(y - dy * t), 0u, params.height - 1u);
        let sample = unpack(input[sy * params.width + sx]);
        let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), sample.xyz);
        if luma > params.threshold {
            ray = ray + sample.xyz * weight;
        }
        weight *= decay_per_sample;
    }

    let inv_samples = 1.0 / f32(params.samples);
    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(orig.xyz + ray * inv_samples * params.intensity, orig.w));
}
