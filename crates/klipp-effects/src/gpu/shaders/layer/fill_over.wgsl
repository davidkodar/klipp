//#include pixel_storage

struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
fn bilinear_second(x_in: f32, y_in: f32, w: u32, lo: vec2<f32>, hi: vec2<f32>) -> vec4<f32> {
    let x = clamp(x_in, lo.x, hi.x);
    let y = clamp(y_in, lo.y, hi.y);
    let ix = floor(x);
    let iy = floor(y);
    let tx = x - ix;
    let ty = y - iy;
    let x0 = u32(ix); let y0 = u32(iy);
    let x1 = u32(min(ix + 1.0, hi.x)); let y1 = u32(min(iy + 1.0, hi.y));
    let a = unpack(second[y0 * w + x0]); let b = unpack(second[y0 * w + x1]);
    let c = unpack(second[y1 * w + x0]); let d = unpack(second[y1 * w + x1]);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    return top + (bottom - top) * ty;
}

@compute @workgroup_size(256)
fn fill_over(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let rx = f32(params.small_w) / f32(params.width);
    let ry = f32(params.small_h) / f32(params.height);
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let hi = vec2<f32>(f32(params.small_w - 1u), f32(params.small_h - 1u));
    let bg = bilinear_second((x + 0.5) * rx - 0.5, (y + 0.5) * ry - 0.5, params.small_w, vec2<f32>(0.0), hi);
    if bg.a < 0.5 {
        output[idx] = input[idx];
        return;
    }
    let px = unpack(input[idx]);
    let keep = 1.0 - params.darken;
    output[idx] = pack(vec4<f32>(px.rgb * px.a + bg.rgb * keep * (1.0 - px.a), 1.0));
}
