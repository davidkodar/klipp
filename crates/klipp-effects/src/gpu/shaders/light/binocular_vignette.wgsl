//#include pixel_storage

struct Params { width: u32, height: u32, amount: f32, spacing: f32, eye_w: f32, eye_h: f32, softness: f32, fringe: f32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_bilinear(fx: f32, fy: f32, w: u32, h: u32) -> vec4<f32> {
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
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);
    let wf = f32(w);
    let hf = f32(h);
    let rx = max(params.eye_w * hf * 0.5, 1.0);
    let ry = max(params.eye_h * hf * 0.5, 1.0);
    let cy = hf * 0.5;
    let c1 = wf * (0.5 - params.spacing * 0.5);
    let c2 = wf * (0.5 + params.spacing * 0.5);
    let d1 = sqrt(((x - c1) / rx) * ((x - c1) / rx) + ((y - cy) / ry) * ((y - cy) / ry));
    let d2 = sqrt(((x - c2) / rx) * ((x - c2) / rx) + ((y - cy) / ry) * ((y - cy) / ry));
    var d = d1;
    var cx = c1;
    if d2 < d1 { d = d2; cx = c2; }
    let soft = max(params.softness, 0.01);
    let falloff = clamp((d - (1.0 - soft)) / soft, 0.0, 1.0);
    let k = 1.0 + params.amount * falloff;
    let px = unpack(input[idx]);
    var r = px.x;
    var b = px.z;
    let vx = x - cx;
    let vy = y - cy;
    let len = sqrt(vx * vx + vy * vy);
    let reach = clamp(d, 0.0, 1.0);
    let s = params.fringe * reach * reach;
    if len > 0.0 && s != 0.0 {
        let ux = vx / len * s;
        let uy = vy / len * s;
        r = sample_bilinear(x + ux, y + uy, w, h).x;
        b = sample_bilinear(x - ux, y - uy, w, h).z;
    }
    output[idx] = pack(vec4<f32>(r * k, px.y * k, b * k, px.w));
}
