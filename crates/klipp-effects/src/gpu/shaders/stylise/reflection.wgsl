//#include pixel_storage
//#include displace_noise

struct Params {
    width: u32,
    height: u32,
    waterline: f32,
    ripple_amount: f32,
    ripple_size: f32,
    ripple_speed: f32,
    breakup: f32,
    smear: f32,
    fade: f32,
    opacity: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    tint_amount: f32,
    submerge: f32,
    seed: u32,
    time: f32,
    darken: f32,
    contact: f32,
    gap: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn at(x: i32, y: i32) -> vec4<f32> {
    let w = i32(params.width);
    let h = i32(params.height);
    if x < 0 || y < 0 || x >= w || y >= h { return vec4<f32>(0.0); }
    return unpack(input[u32(y * w + x)]);
}

fn sample(sx: f32, sy: f32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    var sum = vec4<f32>(0.0);
    let c00 = at(x0, y0);
    let c10 = at(x0 + 1, y0);
    let c01 = at(x0, y0 + 1);
    let c11 = at(x0 + 1, y0 + 1);
    sum += vec4<f32>(c00.rgb * c00.a, c00.a) * ((1.0 - fx) * (1.0 - fy));
    sum += vec4<f32>(c10.rgb * c10.a, c10.a) * (fx * (1.0 - fy));
    sum += vec4<f32>(c01.rgb * c01.a, c01.a) * ((1.0 - fx) * fy);
    sum += vec4<f32>(c11.rgb * c11.a, c11.a) * (fx * fy);
    return sum;
}

fn reflected(x: f32, y: f32) -> vec4<f32> {
    let own = at(i32(x), i32(y));
    let d = y + 0.5 - params.waterline - max(params.gap, 0.0);
    if d <= 0.0 { return own; }
    let h = f32(max(params.height, 1u));
    let size = max(params.ripple_size, 1.0);
    let t = params.time * params.ripple_speed;
    let q = vec3<f32>(x / size, d / (size * 0.2), t);
    let grow = 0.35 + 0.65 * clamp(d / (size * 2.0), 0.0, 1.0);
    let fine = vec3<f32>(x / (size * 0.4), d / (size * 0.07), t * 1.3);
    let dx = params.ripple_amount * grow * (dn_fbm(q, params.seed) + 0.35 * dn_fbm(fine, params.seed + 3u));
    let dy = params.ripple_amount * 0.12 * dn_fbm(q + vec3<f32>(41.3, 7.9, 0.0), params.seed + 1u);
    let bend = 1.2 * dn_fbm(vec3<f32>(x / (size * 1.7), d / (size * 0.5), t * 0.4), params.seed + 4u);
    let b = dn_fbm(vec3<f32>(x / (size * 3.0), d / (size * 0.12) + bend, t * 0.6), params.seed + 2u);
    let thr = -0.7 + 1.5 * params.breakup * clamp(d / (size * 1.5), 0.0, 1.0);
    let glint = dn_fbm(vec3<f32>(x / (size * 0.3), d / (size * 0.05), t * 1.7), params.seed + 5u);
    let near = clamp(d / max(f32(params.height) * 0.15, 1.0), 0.0, 1.0);
    let keep = mix(1.0, smoothstep(thr - 0.16, thr + 0.16, b) * clamp(1.0 + glint * 0.8 * params.breakup, 0.0, 1.0), near);
    let stretch = 1.0 + params.smear * 2.5 * clamp(d / (size * 2.0), 0.0, 1.0);
    let r = params.smear * d * 0.5;
    let sx = x + dx;
    let sy = params.waterline - d / stretch + dy - 0.5;
    var sum = vec4<f32>(0.0);
    for (var k = 0; k < 7; k++) {
        sum += sample(sx, sy + r * (f32(k) - 3.0) / 3.0) / 7.0;
    }
    let reach = mix(1.0, 0.05, clamp(params.fade, 0.0, 1.0));
    let f = 1.0 - clamp(d / h / reach, 0.0, 1.0);
    var out = vec4<f32>(0.0, 0.0, 0.0, sum.a * f * f * keep * params.opacity);
    if sum.a > 1e-6 {
        let lw = vec3<f32>(0.2126, 0.7152, 0.0722);
        let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
        let darken = clamp(params.darken, 0.0, 1.0);
        let c = sum.rgb / sum.a;
        let l = dot(c, lw);
        let dark = max(vec3<f32>(l) + (c - vec3<f32>(l)) * (1.0 + 0.6 * darken), vec3<f32>(0.0)) * (1.0 - darken);
        let k = dot(dark, lw) / max(dot(tint, lw), 1e-4);
        let wobble = (params.ripple_amount * 0.25 + 1.0) * dn_fbm(vec3<f32>(x / (size * 0.6), 0.0, params.time * params.ripple_speed * 0.8), params.seed + 6u);
        let band = max(size * 0.12, 2.0);
        let resting = 1.0 - smoothstep(0.0, band, params.gap);
        let shade = 1.0 - 0.6 * clamp(params.contact, 0.0, 1.0) * resting * (1.0 - smoothstep(0.0, band, d + wobble));
        out = vec4<f32>(mix(dark, tint * k, params.tint_amount) * shade, out.a);
    }
    let sink = clamp(params.submerge, 0.0, 1.0);
    let s = sample(x + dx * 0.5 * sink, y);
    let own_a = s.a * (1.0 - sink);
    if own_a <= 0.0 { return out; }
    let lit = s.rgb / s.a;
    let lw = vec3<f32>(0.2126, 0.7152, 0.0722);
    let tint = vec3<f32>(params.tint_r, params.tint_g, params.tint_b);
    let darken = clamp(params.darken, 0.0, 1.0);
    let l = dot(lit, lw);
    let dark = max(vec3<f32>(l) + (lit - vec3<f32>(l)) * (1.0 + 0.6 * darken), vec3<f32>(0.0)) * (1.0 - darken);
    let wet = mix(dark, tint * (dot(dark, lw) / max(dot(tint, lw), 1e-4)), params.tint_amount);
    let alpha = own_a + out.a * (1.0 - own_a);
    if alpha <= 1e-6 { return vec4<f32>(0.0); }
    return vec4<f32>((mix(lit, wet, sink) * own_a + out.rgb * out.a * (1.0 - own_a)) / alpha, alpha);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    if idx >= w * params.height { return; }
    output[idx] = pack(reflected(f32(idx % w), f32(idx / w)));
}
