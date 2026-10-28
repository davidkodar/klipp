//#include srgb_components
//#include pixel_storage
//#include linear_helpers
//#include bt2020_gamut
//#include display_dither

struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    // The CPU's order: to BT.709 and compressed, negatives clamped,
    // (no tone map), sRGB encode, clamp, quantise.
    let mapped = max(bt2020_to_bt709_display(px.xyz), vec3<f32>(0.0));
    let e = vec3<f32>(
        linear_to_srgb_component(mapped.x),
        linear_to_srgb_component(mapped.y),
        linear_to_srgb_component(mapped.z),
    );
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let a = u32(clamp(px.w, 0.0, 1.0) * 255.0 + 0.5);
    output[idx] = quantise_u8(e.x, d) | (quantise_u8(e.y, d) << 8u) | (quantise_u8(e.z, d) << 16u) | (a << 24u);
}
