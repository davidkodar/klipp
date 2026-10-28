//#include pixel_storage
//#include mask_sdf

struct Params {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> records: array<MaskRecord>;
@group(0) @binding(3) var<storage, read> points: array<vec2<f32>>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);

    let m = clamp(window_matte(nx, ny, aspect, params.mask_count), 0.0, 1.0);
    output[idx] = pack(vec4<f32>(m, m, m, 1.0));
}
