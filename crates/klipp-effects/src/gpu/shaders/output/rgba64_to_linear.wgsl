//#include pixel_storage

struct Params {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 16-bit code's linear value, as the CPU decodes it, then each code
// over 65535, alpha's value (`srgb16_decode_table`): the shader's `pow`
// and division only approximate them.
@group(0) @binding(3) var<storage, read> srgb16: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    // RGBA64LE: R,G in the first u32 (low half first), B,A in the second.
    let lo = input[idx * 2u];
    let hi = input[idx * 2u + 1u];
    let a = srgb16[65536u + ((hi >> 16u) & 0xFFFFu)];
    let lin = vec3<f32>(srgb16[lo & 0xFFFFu], srgb16[(lo >> 16u) & 0xFFFFu], srgb16[hi & 0xFFFFu]);
    // Alpha stays linear, per the sRGB spec.
    output[idx] = pack(vec4<f32>(
        dot(params.m0.xyz, lin),
        dot(params.m1.xyz, lin),
        dot(params.m2.xyz, lin),
        a,
    ));
}
