
struct Params {
    width: u32,
    height: u32,
    block_size: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let bx = u32(floor(f32(x) / params.block_size) * params.block_size);
    let by = u32(floor(f32(y) / params.block_size) * params.block_size);
    let cbx = min(bx, params.width - 1u);
    let cby = min(by, params.height - 1u);
    let src_idx = cby * params.width + cbx;
    output[idx] = input[src_idx];
}
