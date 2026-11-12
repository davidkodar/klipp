// A linear frame of another size fitted into the chain's frame, as the
// CPU compositor's `fit` fits a source and `u8_fit_to_linear` fits 8-bit
// codes: one scale factor, centred, the rest transparent; an area average
// when shrinking by two or more on both axes, bilinear otherwise. Straight
// alpha, as `fit` keeps it: a sample never reaches outside the source, so
// the bars around it are never averaged into its edge.
//#include pixel_storage

struct Params {
    src_w: u32, src_h: u32, out_w: u32, out_h: u32,
    dst_w: u32, dst_h: u32, x_off: u32, y_off: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn tap(x: u32, y: u32) -> vec4<f32> {
    return unpack(input[y * params.src_w + x]);
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
        // The windows in whole numbers: as floats, a bound landing on a
        // whole pixel (228 × 800/384 = 475) rounded one way here and the
        // other on the CPU, and took a column more or less.
        let x0 = min(x * params.src_w / params.dst_w, params.src_w - 1u);
        let x1 = clamp((x + 1u) * params.src_w / params.dst_w, x0 + 1u, params.src_w);
        let y0 = min(y * params.src_h / params.dst_h, params.src_h - 1u);
        let y1 = clamp((y + 1u) * params.src_h / params.dst_h, y0 + 1u, params.src_h);
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
