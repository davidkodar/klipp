//#include pixel_storage

// `mode`: 0 averages all `n` samples of `input`; 1 and 3 place sample 0
// of `input` and add it, premultiplied, to `output` (1 starting it afresh);
// 2 gives `input`, such a sum of `n` samples, back averaged and straight.
struct Params { width: u32, height: u32, n: u32, mode: u32 }
// A sample's placement: anchor and position; rotation and inverse scale;
// tilt; focal length. Numbers, not pixels.
struct Sample { place: vec4<f32>, turn: vec4<f32>, tilt: vec4<f32>, lens: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> samples: array<Sample>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

fn texel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    return unpack(input[u32(y) * params.width + u32(x)]);
}

// The picture as sample `k` places it, at pixel (x, y): the Transform
// shader's inverse map and bilinear read, line for line.
fn placed(k: u32, x: f32, y: f32) -> vec4<f32> {
    let a = samples[k].place;
    let r = samples[k].turn;
    let t = samples[k].tilt;
    let focal = samples[k].lens.x;
    let dx = x - a.x - a.z;
    let dy = y - a.y - a.w;
    let rx = dx * r.x + dy * r.y;
    let ry = -dx * r.y + dy * r.x;
    var sx = rx * r.z;
    var sy = ry * r.w;
    let cos_rx = t.x;
    let sin_rx = t.y;
    let cos_ry = t.z;
    let sin_ry = t.w;
    if cos_rx != 1.0 || cos_ry != 1.0 {
        var u = 0.0;
        var v = 0.0;
        var seen = true;
        if focal > 0.0 {
            let a11 = sx * sin_ry - focal * cos_ry;
            let a12 = -sx * sin_rx * cos_ry - focal * sin_rx * sin_ry;
            let a21 = sy * sin_ry;
            let a22 = -sy * sin_rx * cos_ry - focal * cos_rx;
            let det = a11 * a22 - a12 * a21;
            if abs(det) < 1e-9 {
                seen = false;
            } else {
                let b1 = -sx * focal;
                let b2 = -sy * focal;
                u = (b1 * a22 - a12 * b2) / det;
                v = (a11 * b2 - b1 * a21) / det;
                seen = focal + (u * sin_ry - v * sin_rx * cos_ry) > 0.0;
            }
        } else if abs(cos_rx) < 1e-6 || abs(cos_ry) < 1e-6 {
            seen = false;
        } else {
            v = sy / cos_rx;
            u = (sx - v * sin_rx * sin_ry) / cos_ry;
        }
        if seen {
            sx = u;
            sy = v;
        } else {
            sx = -1.0e9;
            sy = -1.0e9;
        }
    }
    sx = sx + a.x;
    sy = sy + a.y;
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    let top = mix(texel(x0, y0), texel(x0 + 1, y0), fx);
    let bot = mix(texel(x0, y0 + 1), texel(x0 + 1, y0 + 1), fx);
    return mix(top, bot, fy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    if params.mode == 1u || params.mode == 3u {
        let p = placed(0u, x, y);
        var sum = vec4<f32>(0.0);
        if params.mode == 3u { sum = unpack(output[idx]); }
        output[idx] = pack(sum + vec4<f32>(p.xyz * p.w, p.w));
        return;
    }
    var rgb = vec3<f32>(0.0);
    var covered = 0.0;
    if params.mode == 2u {
        let sum = unpack(input[idx]);
        rgb = sum.xyz;
        covered = sum.w;
    } else {
        for (var k = 0u; k < params.n; k = k + 1u) {
            let p = placed(k, x, y);
            rgb = rgb + p.xyz * p.w;
            covered = covered + p.w;
        }
    }
    var colour = vec3<f32>(0.0);
    if covered > 