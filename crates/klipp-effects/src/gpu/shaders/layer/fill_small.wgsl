//#include pixel_storage

struct FillParams {
    width: u32, height: u32, small_w: u32, small_h: u32,
    zoom: f32, darken: f32, pad0: u32, pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> second: array<vec4<f32>>; // not-pixels: reduced opaque bounds
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: FillParams;
fn bilinear_input(x_in: f32, y_in: f32, w: u32, lo: vec2<f32>, hi: vec2<f32>) -> vec4<f32> {
    let x = clamp(x_in, lo.x, hi.x);
    let y = clamp(y_in, lo.y, hi.y);
    let ix = floor(x);
    let iy = floor(y);
    let tx = x - ix;
    let ty = y - iy;
    let x0 = u32(ix); let y0 = u32(iy);
    let x1 = u32(min(ix + 1.0, hi.x)); let y1 = u32(min(iy + 1.0, hi.y));
    let a = unpack(input[y0 * w + x0]); let b = unpack(input[y0 * w + x1]);
    let c = unpack(input[y1 * w + x0]); let d = unpack(input[y1 * w + x1]);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    return top + (bottom - top) * ty;
}

@compute @workgroup_size(256)
fn fill_small(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.small_w * params.small_h { return; }
    let box = second[0];
    let wf = f32(params.width);
    let hf = f32(params.height);
    // Nothing opaque, or a picture that fills the frame: no bars, and an
    // empty fill says so to fill_over.
    if !(box.z > box.x) || (box.x <= 0.0 && box.y <= 0.0 && box.z >= wf && box.w >= hf) {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let scale = max(wf / (box.z - box.x), hf / (box.w - box.y)) * max(params.zoom, 1.0);
    let cx = (box.x + box.z) * 0.5;
    let cy = (box.y + box.w) * 0.5;
    let fx = (f32(idx % params.small_w) + 0.5) * wf / f32(params.small_w);
    let fy = (f32(idx / params.small_w) + 0.5) * hf / f32(params.small_h);
    let c = bilinear_input(cx + (fx - wf * 0.5) / scale - 0.5, cy + (fy - hf * 0.5) / scale - 0.5, params.width, box.xy, box.zw - vec2<f32>(1.0));
    output[idx] = pack(vec4<f32>(c.rgb, 1.0));
}
