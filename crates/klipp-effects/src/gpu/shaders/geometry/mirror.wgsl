
struct Params {
    width: u32,
    height: u32,
    horizontal: u32,
    vertical: u32,
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
    var sx = x;
    var sy = y;
    if params.horizontal != 0u {
        sx = params.width - 1u - x;
    }
    if params.vertical != 0u {
        sy = params.height - 1u - y;
    }
    output[idx] = input[sy * params.width + sx];
}
