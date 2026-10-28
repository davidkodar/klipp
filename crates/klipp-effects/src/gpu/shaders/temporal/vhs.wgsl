//#include pixel_storage

struct Params {
    width: u32, height: u32, tracking: f32, bleed: f32,
    noise: f32, scanlines: f32, seed: u32, luma_yr: f32,
    luma_yg: f32, luma_yb: f32, _pad: u32, _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

fn sample_clamped(x: f32, yi: i32) -> vec4<f32> {
    let xi = clamp(i32(round(x)), 0, i32(params.width) - 1);
    let yc = clamp(yi, 0, i32(params.height) - 1);
    return unpack(input[u32(yc) * params.width + u32(xi)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let hf = f32(params.height);
    let band_y = hash(params.seed * 2654435761u) * hf;
    let band_h = hf * 0.06;
    let rh = hash(y * 7919u + params.seed * 15485863u);
    let in_band = abs(f32(y) - band_y) < band_h;
    var dx = (rh - 0.5) * params.tracking * 3.0;
    if in_band {
        dx = dx + (rh - 0.5) * params.tracking * 40.0;
    }
    let o = sample_clamped(f32(x) + dx, i32(y));
    let sh = sample_clamped(f32(x) + dx - params.bleed * 8.0, i32(y));
    let yw = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
    let lo = dot(o.xyz, yw);
    let ls = dot(sh.xyz, yw);
    var r = lo + (sh.x - ls);
    var g = lo + (sh.y - ls);
    var b = lo + (sh.z - ls);
    var n = (hash((y * params.width + x) * 7919u + params.seed) - 0.5) * params.noise * 0.3;
    if in_band {
        n = n * 3.0;
    }
    r = r + n;
    g = g + n;
    b = b + n;
    if (y % 2u) == 1u {
        let k = 1.0 - params.scanlines * 0.25;
        r = r * k;
        g = g * k;
        b = b * k;
    }
    output[idx] = pack(vec4<f32>(r, g, b, o.w));
}
