
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    small_w: u32, small_h: u32, radius: u32, mode: u32,
    luma_opacity: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
//#include pixel_storage

const TARGET: f32 = 4.5;
const LIGHT_TEXT: f32 = 0.179;
const HALO_FULL: f32 = 0.04;

fn luma(c: vec3<f32>) -> f32 {
    return clamp(dot(c, p.luma_opacity.xyz), 0.0, 1.0);
}

fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[u32(cy) * p.small_w + u32(cx)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    if p.op == 4u {
        if i >= p.width * p.height { return; }
        let x = i % p.width; let y = i / p.width;
        let u = (f32(x) + 0.5) / f32(p.scale) - 0.5;
        let v = (f32(y) + 0.5) / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let m = mix(mix(small_at(x0, y0), small_at(x0 + 1, y0), fx), mix(small_at(x0, y0 + 1), small_at(x0 + 1, y0 + 1), fx), fy);
        let a = m.z;
        if a <= 1e-3 { return; }
        let ht = clamp(a / HALO_FULL, 0.0, 1.0);
        let halo = ht * ht * (3.0 - 2.0 * ht);
        let busy = sqrt(max(m.y - m.x * m.x, 0.0));
        let lt = clamp(m.w / a, 0.0, 1.0);
        var px = unpack(output[i]);
        if lt >= LIGHT_TEXT {
            let need = (lt + 0.05) / TARGET - 0.05;
            let cur = min(m.x + busy, 1.0);
            var k = 1.0;
            if cur > need { k = clamp(max(need, 0.0) / cur, 0.12, 1.0); }
            if p.mode == 2u { k = min(k, 0.55); }
            px = vec4<f32>(px.xyz * (1.0 - halo * (1.0 - k)), px.w);
        } else {
            let need = TARGET * (lt + 0.05) - 0.05;
            let cur = max(m.x - busy, 0.0);
            var t = clamp((need - cur) / max(1.0 - cur, 1e-4), 0.0, 0.88);
            if p.mode == 2u { t = max(t, 0.45); }
            px = vec4<f32>(px.xyz + (vec3<f32>(1.0) - px.xyz) * halo * t, px.w);
        }
        output[i] = pack(px);
        return;
    }
    if i >= p.small_w * p.small_h { return; }
    let sx = i % p.small_w; let sy = i / p.small_w;
    if p.op <= 1u {
        // 0: the picture's luminance and its square; 1: the layer's
        // coverage and luminance times coverage, beside them.
        var sum = vec2<f32>(0.0);
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                if p.op == 0u {
                    let lb = luma(c.xyz) * clamp(c.w, 0.0, 1.0);
                    sum += vec2<f32>(lb, lb * lb);
                } else {
                    let a = clamp(c.w * p.luma_opacity.w, 0.0, 1.0);
                    sum += vec2<f32>(a, luma(c.xyz) * a);
                }
                n += 1.0;
            }
        }
        let mean = sum / max(n, 1.0);
        if p.op == 0u {
            output[i] = pack(vec4<f32>(mean, 0.0, 0.0));
        } else {
            let was = unpack(output[i]);
            output[i] = pack(vec4<f32>(was.xy, mean));
        }
    } else if p.op == 2u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx) + d, i32(sy)); }
        output[i] = pack(sum / f32(2 * r + 1));
    } else if p.op == 3u {
        var sum = vec4<f32>(0.0);
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) { sum += small_at(i32(sx), i32(sy) + d); }
        output[i] = pack(sum / f32(2 * r + 1));
    }
}
