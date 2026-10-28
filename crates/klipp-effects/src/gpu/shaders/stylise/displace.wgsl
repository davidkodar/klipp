//#include pixel_storage
//#include displace_noise

struct Params {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    speed: f32,
    direction: u32,
    seed: u32,
    time: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    if idx >= w * params.height { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);
    let d = dn_offset(x, y, params.amount, params.size, params.speed, params.direction, params.seed, params.time);
    output[idx] = pack(sample_premultiplied(x + d.x, y + d.y));
}
//#include displace_sample
