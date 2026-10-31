//#include pixel_storage

// A frozen track's layer (#3207): RGBA half floats, two to a word, linear
// in the working space with straight alpha, widened into a stack layer as
// they are. The params are the sprite rows' layout; the matrices are not
// read: a frozen layer is in the working space already.
struct Params { width: u32, height: u32, y0: u32, y1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let rg = unpack2x16float(input[idx * 2u]);
    let ba = unpack2x16float(input[idx * 2u + 1u]);
    output[idx] = pack(vec4<f32>(rg.x, rg.y, ba.x, ba.y));
}
