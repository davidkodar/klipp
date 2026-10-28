//#include pixel_storage

struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 8-bit code's linear value, as the CPU decodes it, then each code
// over 255, alpha's value, as the CPU divides it (`srgb8_to_linear`).
@group(0) @binding(3) var<storage, read> srgb8: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let packed = input[idx];
    let a = srgb8[256u + ((packed >> 24u) & 0xFFu)];
    // sRGB transfer on RGB, looked up: alpha stays linear per sRGB spec.
    output[idx] = pack(vec4<f32>(srgb8[packed & 0xFFu], srgb8[(packed >> 8u) & 0xFFu], srgb8[(packed >> 16u) & 0xFFu], a));
}
