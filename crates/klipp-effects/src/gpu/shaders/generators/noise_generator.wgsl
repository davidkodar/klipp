//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    amount: f32,
    monochrome: u32,
    seed: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
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
    let v = unpack(input[idx]);
    var r = v.x;
    var g = v.y;
    var b = v.z;

    let noise_seed = idx * 7919u + params.seed;
    if params.monochrome != 0u {
        let n = (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        r = r + n;
        g = g + n;
        b = b + n;
    } else {
        r = r + (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        g = g + (hash(noise_seed + 1u) - 0.5) * 2.0 * params.amount;
        b = b + (hash(noise_seed + 2u) - 0.5) * 2.0 * params.amount;
    }

    output[idx] = pack(vec4<f32>(r, g, b, v.w));
}
