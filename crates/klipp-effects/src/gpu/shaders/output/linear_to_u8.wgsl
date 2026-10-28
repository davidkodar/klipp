//#include pixel_storage

struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn linear_to_srgb_c(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.0031308) { return x * 12.92; }
    return 1.055 * pow(x, 1.0/2.4) - 0.055;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = unpack(input[idx]);
    // Encode RGB through sRGB transfer, alpha is linear (no gamma).
    let r = u32(clamp(linear_to_srgb_c(pixel.x) * 255.0 + 0.5, 0.0, 255.0));
    let g = u32(clamp(linear_to_srgb_c(pixel.y) * 255.0 + 0.5, 0.0, 255.0));
    let b = u32(clamp(linear_to_srgb_c(pixel.z) * 255.0 + 0.5, 0.0, 255.0));
    let a = u32(clamp(pixel.w * 255.0 + 0.5, 0.0, 255.0));
    // Pack RGBA into a single u32, little-endian (R in byte 0).
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
