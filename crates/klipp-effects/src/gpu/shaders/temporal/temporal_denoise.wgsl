
struct Params {
    width: u32,
    height: u32,
    count: u32,
    _pad0: u32,
    denom: f32,
    strength: f32,
    _pad1: u32,
    _pad2: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> before: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> after: array<vec4<f32>>;
//#include pixel_storage


fn td_luma(p: vec3<f32>) -> f32 {
    return params.luma.x * p.x + params.luma.y * p.y + params.luma.z * p.z;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let cur = unpack(input[idx]);
    let cur_luma = td_luma(cur.xyz);
    var sum = cur.xyz;
    var weight = 1.0;
    if params.count > 0u {
        let n = unpack(before[idx]).xyz;
        let d = cur_luma - td_luma(n);
        let w = exp(-(d * d) / params.denom);
        sum = sum + n * w;
        weight = weight + w;
    }
    if params.count > 1u {
        let n = unpack(after[idx]).xyz;
        let d = cur_luma - td_luma(n);
        let w = exp(-(d * d) / params.denom);
        sum = sum + n * w;
        weight = weight + w;
    }
    let averaged = sum * (1.0 / weight);
    let out = cur.xyz + (averaged - cur.xyz) * params.strength;
    output[idx] = pack(vec4<f32>(out, cur.w));
}
