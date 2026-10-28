
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> cdf: array<f32>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let p = unpack(input[idx]);
    let e = enc3(p.xyz);

    // The histogram covers what the display shows; light above white
    // passes through, so equalising does not crush the highlights.
    let over = max(e - vec3<f32>(1.0), vec3<f32>(0.0));
    let d = min(e, vec3<f32>(1.0));

    // Compute luminance and look up equalised value from CDF
    let luma = dot(params.luma.xyz, d);
    let luma_idx = u32(clamp(luma * 255.0, 0.0, 255.0));
    let eq_luma = cdf[luma_idx];

    // Scale RGB proportionally to match new luminance
    let scale = select(eq_luma / luma, 1.0, luma < 0.001);

    // Blend with original based on strength
    let out = d + (d * scale - d) * params.strength + over;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
