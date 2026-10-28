//#include pixel_storage

struct Params {
    width: u32, height: u32, amount: f32, per_px: f32,
    color: u32, shadows: f32, seed: u32, luma_yr: f32,
    luma_yg: f32, luma_yb: f32, _pad: u32, _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let v = unpack(input[idx]);
    // A product, as the CPU works it (`grain_cells_per_px`): a division
    // here is good to 2.5 ULP, and an edge then fell in the next cell on
    // this route alone (#1441).
    let gx = u32(f32(x) * params.per_px);
    let gy = u32(f32(y) * params.per_px);
    let base = gx * 7919u + gy * 104729u + params.seed * 15485863u;
    let l = clamp(dot(v.xyz, vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb)), 0.0, 1.0);
    let depth = params.amount * 0.35 * (1.0 - params.shadows * l);
    let n0 = (hash(base) - 0.5) * 2.0 * depth;
    var r = v.x + n0;
    var g = v.y + n0;
    var b = v.z + n0;
    if params.color != 0u {
        g = v.y + (hash(base + 1u) - 0.5) * 2.0 * depth;
        b = v.z + (hash(base + 2u) - 0.5) * 2.0 * depth;
    }
    output[idx] = pack(vec4<f32>(r, g, b, v.w));
}
