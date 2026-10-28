//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    h0: f32,
    h1: f32,
    h2: f32,
    h3: f32,
    h4: f32,
    h5: f32,
    h6: f32,
    h7: f32,
    h8: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let fx0 = f32(x);
    let fy0 = f32(y);
    let w = params.h6 * fx0 + params.h7 * fy0 + params.h8;
    if w <= 1e-6 {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let sx = (params.h0 * fx0 + params.h1 * fy0 + params.h2) / w;
    let sy = (params.h3 * fx0 + params.h4 * fy0 + params.h5) / w;

    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);

    let p00 = sample_pixel(x0, y0);
    let p10 = sample_pixel(x1, y0);
    let p01 = sample_pixel(x0, y1);
    let p11 = sample_pixel(x1, y1);

    let top = mix(p00, p10, fx);
    let bot = mix(p01, p11, fx);
    output[idx] = pack(mix(top, bot, fy));
}

fn sample_pixel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    let src_idx = u32(y) * params.width + u32(x);
    return unpack(input[src_idx]);
}
