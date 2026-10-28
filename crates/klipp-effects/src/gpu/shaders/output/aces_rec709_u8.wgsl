//#include pixel_storage
//#include display_dither

struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;

//#include aces_rec709
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    let e = acescg_to_rec709_sdr(px.xyz);
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let r = quantise_u8(e.x, d);
    let g = quantise_u8(e.y, d);
    let b = quantise_u8(e.z, d);
    let a = u32(clamp(px.w * 255.0 + 0.5, 0.0, 255.0));
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
