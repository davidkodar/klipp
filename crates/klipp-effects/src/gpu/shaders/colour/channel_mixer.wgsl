
struct Params {
    width: u32,
    height: u32,
    rr: f32,
    rg: f32,
    rb: f32,
    gr: f32,
    gg: f32,
    gb: f32,
    br: f32,
    bg: f32,
    bb: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let out = vec3<f32>(
        params.rr * p.x + params.rg * p.y + params.rb * p.z,
        params.gr * p.x + params.gg * p.y + params.gb * p.z,
        params.br * p.x + params.bg * p.y + params.bb * p.z,
    );
    output[idx] = pack(vec4<f32>(out, p.w));
}
