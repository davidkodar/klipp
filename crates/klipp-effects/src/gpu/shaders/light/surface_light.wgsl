
struct Params {
    width: u32, height: u32, op: u32, scale: u32,
    small_w: u32, small_h: u32, radius: u32, shade: f32,
    texture: f32, _p0: u32, _p1: u32, _p2: u32,
    luma: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
//#include pixel_storage

const MAX_RATIO: f32 = 4.0;
const EPS: f32 = 1e-3;

fn luma(c: vec3<f32>) -> f32 {
    return max(dot(c, p.luma.xyz), 0.0);
}

fn small_at(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(p.small_w) - 1);
    let cy = clamp(y, 0, i32(p.small_h) - 1);
    return unpack(input[u32(cy) * p.small_w + u32(cx)]);
}

fn ratio(a: f32, b: f32) -> f32 {
    return clamp((a + EPS) / (b + EPS), 1.0 / MAX_RATIO, MAX_RATIO);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let i = gid.x;
    let cells = p.small_w * p.small_h;
    let pixels = p.width * p.height;
    if p.op <= 1u {
        // 0: each cell's picture luminance; 1: how much the layer covers
        // it, beside it.
        if i >= cells { return; }
        let sx = i % p.small_w; let sy = i / p.small_w;
        var sum = 0.0;
        var n = 0.0;
        for (var dy = 0u; dy < p.scale; dy++) {
            let y = sy * p.scale + dy;
            if y >= p.height { break; }
            for (var dx = 0u; dx < p.scale; dx++) {
                let x = sx * p.scale + dx;
                if x >= p.width { break; }
                let c = unpack(input[y * p.width + x]);
                if p.op == 0u {
                    sum += luma(c.xyz) * clamp(c.w, 0.0, 1.0);
                } else {
                    sum += clamp(c.w, 0.0, 1.0);
                }
                n += 1.0;
            }
        }
        let mean = sum / max(n, 1.0);
        if p.op == 0u {
            output[i] = pack(vec4<f32>(mean, 0.0, 0.0, 0.0));
        } else {
            let was = unpack(output[i]);
            output[i] = pack(vec4<f32>(was.x, 0.0, mean, 0.0));
        }
    } else if p.op <= 3u {
        // The luminance blurred across (2) and down (3), the coverage kept.
        if i >= cells { return; }
        let sx = i32(i % p.small_w); let sy = i32(i / p.small_w);
        var sum = 0.0;
        let r = i32(p.radius);
        for (var d = -r; d <= r; d++) {
            if p.op == 2u { sum += small_at(sx + d, sy).x; } else { sum += small_at(sx, sy + d).x; }
        }
        output[i] = pack(vec4<f32>(sum / f32(2 * r + 1), 0.0, small_at(sx, sy).z, 0.0));
    } else if p.op == 4u {
        // Each row's local light weighted by coverage, and its coverage.
        if i >= p.small_h { return; }
        var sum = vec2<f32>(0.0);
        for (var x = 0u; x < p.small_w; x++) {
            let c = unpack(input[i * p.small_w + x]);
            sum += vec2<f32>(c.x * c.z, c.z);
        }
        output[i] = pack(vec4<f32>(sum, 0.0, 0.0));
    } else if p.op == 5u {
        // The rows summed.
        if i != 0u { return; }
        var sum = vec2<f32>(0.0);
        for (var y = 0u; y < p.small_h; y++) { sum += unpack(input[y]).xy; }
        output[0] = pack(vec4<f32>(sum, 0.0, 0.0));
    } else if p.op == 6u {
        // The surface's light into every cell; -1 where nothing is covered.
        if i >= cells { return; }
        let t = unpack(input[0]).xy;
        let was = unpack(output[i]);
        var mean = -1.0;
        if t.y > EPS { mean = t.x / t.y; }
        output[i] = pack(vec4<f32>(was.xyz, mean));
    } else if p.op == 7u {
        // The picture's own luminance, at each pixel.
        if i >= pixels { return; }
        let c = unpack(input[i]);
        output[i] = pack(vec4<f32>(luma(c.xyz) * clamp(c.w, 0.0, 1.0), 0.0, 0.0, 0.0));
    } else if p.op == 8u {
        // The factor at each pixel: its local light against the surface's,
        // and its luminance against its local light.
        if i >= pixels { return; }
        let mean = unpack(input[0]).w;
        let was = unpack(output[i]);
        if mean < 0.0 {
            output[i] = pack(vec4<f32>(was.x, 1.0, 0.0, 0.0));
            return;
        }
        let x = i % p.width; let y = i / p.width;
        let u = (f32(x) + 0.5) / f32(p.scale) - 0.5;
        let v = (f32(y) + 0.5) / f32(p.scale) - 0.5;
        let x0 = i32(floor(u)); let y0 = i32(floor(v));
        let fx = u - floor(u); let fy = v - floor(v);
        let top = small_at(x0, y0).x + (small_at(x0 + 1, y0).x - small_at(x0, y0).x) * fx;
        let bottom = small_at(x0, y0 + 1).x + (small_at(x0 + 1, y0 + 1).x - small_at(x0, y0 + 1).x) * fx;
        let local = top + (bottom - top) * fy;
        let f = clamp(pow(ratio(local, mean), p.shade) * pow(ratio(was.x, local), p.texture), 0.1, 4.0);
        output[i] = pack(vec4<f32>(was.x, f, 0.0, 0.0));
    } else if p.op == 9u {
        // The layer multiplied by it, its alpha kept.
        if i >= pixels { return; }
        let px = unpack(output[i]);
        if px.w <= 0.0 { return; }
        output[i] = pack(vec4<f32>(px.xyz * unpack(input[i]).y, px.w));
    }
}
