//#include pixel_storage

struct Params {
    src_w: u32, src_h: u32, out_w: u32, out_h: u32,
    dst_w: u32, dst_h: u32, x_off: u32, y_off: u32,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 8-bit code's linear value, as the CPU decodes it, then each code
// over 255, alpha's value, as the CPU divides it (`srgb8_to_linear`).
@group(0) @binding(3) var<storage, read> srgb8: array<f32>;

fn tap(x: u32, y: u32) -> vec4<f32> {
    let packed = input[y * params.src_w + x];
    let a = srgb8[256u + ((packed >> 24u) & 0xFFu)];
    return vec4<f32>(srgb8[packed & 0xFFu], srgb8[(packed >> 8u) & 0xFFu], srgb8[(packed >> 16u) & 0xFFu], a);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.out_w * params.out_h { return; }
    let ox = idx % params.out_w;
    let oy = idx / params.out_w;
    if ox < params.x_off || ox >= params.x_off + params.dst_w || oy < params.y_off || oy >= params.y_off + params.dst_h {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let x = ox - params.x_off;
    let y = oy - params.y_off;
    let sx = f32(params.src_w) / f32(params.dst_w);
    let sy = f32(params.src_h) / f32(params.dst_h);
    if sx >= 2.0 && sy >= 2.0 {
        let x0 = min(u32(f32(x) * sx), params.src_w - 1u);
        let x1 = clamp(u32(f32(x + 1u) * sx), x0 + 1u, params.src_w);
        let y0 = min(u32(f32(y) * sy), params.src_h - 1u);
        let y1 = clamp(u32(f32(y + 1u) * sy), y0 + 1u, params.src_h);
        var acc = vec4<f32>(0.0);
        for (var yy = y0; yy < y1; yy = yy + 1u) {
            for (var xx = x0; xx < x1; xx = xx + 1u) {
                acc = acc + tap(xx, yy);
            }
        }
        output[idx] = pack(acc / f32((y1 - y0) * (x1 - x0)));
        return;
    }
    let fx = clamp((f32(x) + 0.5) * sx - 0.5, 0.0, f32(params.src_w - 1u));
    let fy = clamp((f32(y) + 0.5) * sy - 0.5, 0.0, f32(params.src_h - 1u));
    let xa = u32(floor(fx));
    let ya = u32(floor(fy));
    let xb = min(xa + 1u, params.src_w - 1u);
    let yb = min(ya + 1u, params.src_h - 1u);
    let tx = fract(fx);
    let ty = fract(fy);
    let top = mix(tap(xa, ya), tap(xb, ya), tx);
    let bot = mix(tap(xa, yb), tap(xb, yb), tx);
    output[idx] = pack(mix(top, bot, ty));
}
