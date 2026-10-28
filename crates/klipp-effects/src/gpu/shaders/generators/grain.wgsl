
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    shadows: f32,
    highlights: f32,
    frame_seed: u32,
    monochrome: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

fn wang_hash(seed: u32) -> f32 {
    var x = seed;
    x = (x ^ 61u) ^ (x >> 16u);
    x = x * 9u;
    x = x ^ (x >> 4u);
    x = x * 0x27d4eb2du;
    x = x ^ (x >> 15u);
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let p = unpack(input[idx]);
    let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), p.xyz);

    // Pixel coordinates (for grain size variation)
    let px = idx % params.width;
    let py = idx / params.width;
    // Quantize by grain size to create larger particles
    let qx = u32(f32(px) / params.size);
    let qy = u32(f32(py) / params.size);
    let pixel_seed = qx * 1973u + qy * 9277u + params.frame_seed * 26699u;

    // Generate grain noise
    let grain_r = (wang_hash(pixel_seed) - 0.5) * params.amount;
    var grain_g = grain_r;
    var grain_b = grain_r;
    if params.monochrome == 0u {
        grain_g = (wang_hash(pixel_seed + 1u) - 0.5) * params.amount;
        grain_b = (wang_hash(pixel_seed + 2u) - 0.5) * params.amount;
    }

    // Weight grain by tonal region
    let shadow_w = (1.0 - luma) * params.shadows;
    let highlight_w = luma * params.highlights;
    let weight = clamp(shadow_w + highlight_w, 0.0, 1.5);

    output[idx] = pack(vec4<f32>(
        p.x + grain_r * weight,
        p.y + grain_g * weight,
        p.z + grain_b * weight,
        p.w,
    ));
}
