
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
}

struct RowXform {
    a: f32, b: f32, tx: f32,
    c: f32, d: f32, ty: f32,
    _pad0: f32, _pad1: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> row_xforms: array<RowXform>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;

    let xf = row_xforms[y];
    let fx = f32(x);
    let fy = f32(y);

    let src_x = xf.a * fx + xf.b * fy + xf.tx;
    let src_y = xf.c * fx + xf.d * fy + xf.ty;

    output[idx] = pack(sample_bilinear(src_x, src_y));
}
