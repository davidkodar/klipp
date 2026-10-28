//#include pixel_storage

struct Params {
    width: u32, height: u32, step: u32, map_width: u32,
    to_2020_row0: vec4<f32>,
    to_2020_row1: vec4<f32>,
    to_2020_row2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> nits: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

/// `HDR_REFERENCE_WHITE_NITS`: what display light 1.0 means.
const REF_WHITE_NITS: f32 = 203.0;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let map_height = (params.height + params.step - 1u) / params.step;
    let idx = gid.x;
    if idx >= params.map_width * map_height { return; }
    let mx = idx % params.map_width;
    let my = idx / params.map_width;
    // The same sample the twin takes: the top-left pixel of each cell,
    // clamped to the frame, not an average of it.
    let x = min(mx * params.step, params.width - 1u);
    let y = min(my * params.step, params.height - 1u);
    let px = unpack(input[y * params.width + x]).xyz;
    let c = vec3<f32>(
        dot(params.to_2020_row0.xyz, px),
        dot(params.to_2020_row1.xyz, px),
        dot(params.to_2020_row2.xyz, px),
    );
    nits[idx] = max(c.x, max(c.y, c.z)) * REF_WHITE_NITS;
}
