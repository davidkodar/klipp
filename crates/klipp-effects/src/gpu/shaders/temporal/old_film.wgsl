//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    flicker: f32,
    scratches: f32,
    dust: f32,
    weave: f32,
    sepia: f32,
    seed: u32,
    luma_r: f32,
    luma_g: f32,
    luma_b: f32,
    amount: f32,
    age: f32,
    mono: f32,
    stains: f32,
    frame_lines: f32,
    leaks: f32,
    burn: f32,
    _pad0: u32,
    _pad1: u32,
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

fn sstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn vnoise(x: f32, y: f32, s: u32) -> f32 {
    let fx0 = floor(x);
    let fy0 = floor(y);
    let fx = x - fx0;
    let fy = y - fy0;
    let ix = bitcast<u32>(i32(fx0));
    let iy = bitcast<u32>(i32(fy0));
    let a = hash(((ix) * 73856093u) ^ ((iy) * 19349663u) ^ s);
    let b = hash(((ix + 1u) * 73856093u) ^ ((iy) * 19349663u) ^ s);
    let c = hash(((ix) * 73856093u) ^ ((iy + 1u) * 19349663u) ^ s);
    let d = hash(((ix + 1u) * 73856093u) ^ ((iy + 1u) * 19349663u) ^ s);
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    return mix(mix(a, b, sx), mix(c, d, sx), sy);
}

fn at(x: i32, y: i32) -> vec4<f32> {
    let xi = clamp(x, 0, i32(params.width) - 1);
    let yi = clamp(y, 0, i32(params.height) - 1);
    return unpack(input[u32(yi) * params.width + u32(xi)]);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(params.luma_r, params.luma_g, params.luma_b));
}

// Slot k's speck coverage at (x, y), and whether it is white.
fn debris_cover(k: u32, x: f32, y: f32) -> vec2<f32> {
    var life = 1u;
    if hash(k * 101u + 7u) > 0.55 { life = 2u; }
    let epoch = (params.seed + k * 3u) / life;
    let key = epoch * 7919u + k * 104729u;
    if hash(key + 2u) >= params.dust * params.amount * 0.18 { return vec2<f32>(0.0); }
    let s = f32(params.height) / 1080.0;
    let u = hash(key + 5u);
    let radius = (1.0 + 2.5 * u + 30.0 * pow(u, 32.0)) * s;
    var white = 0.0;
    if hash(key + 6u) < 0.75 { white = 1.0; }
    let hair = hash(key + 7u) < 0.15;
    let sx = hash(key + 3u) * f32(params.width);
    let sy = hash(key + 4u) * f32(params.height);
    let bend = (hash(key + 8u) - 0.5) * 2.0;
    let angle = hash(key + 9u) * 6.283185;
    let dx = x - sx;
    let dy = y - sy;
    if hair {
        let len = (30.0 + 60.0 * min(radius / s / 31.0, 1.0) + 30.0 * abs(bend)) * s;
        let c = cos(angle);
        let n = sin(angle);
        let t = dx * c + dy * n;
        if t < 0.0 || t > len { return vec2<f32>(0.0, white); }
        let off = -dx * n + dy * c - bend * len * 0.15 * sin(t / len * 4.712389);
        return vec2<f32>(1.0 - sstep(0.4 * s, 1.2 * s, abs(off)), white);
    }
    if abs(dx) > radius * 1.6 || abs(dy) > radius * 1.6 { return vec2<f32>(0.0, white); }
    let r = max(radius, 1e-3);
    let d = sqrt(dx * dx + dy * dy) / r;
    let ragged = d * (0.75 + 0.5 * vnoise(dx / r * 1.7 + 17.0 * f32(k), dy / r * 1.7, params.seed + k));
    return vec2<f32>(1.0 - sstep(0.6, 1.0, ragged), white);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let xu = idx % params.width;
    let yu = idx / params.width;
    let x = f32(xu);
    let y = f32(yu);
    let wf = f32(params.width);
    let hf = f32(params.height);
    let s = hf / 1080.0;
    let fseed = params.seed * 2654435761u;
    let damage = max(params.amount, 0.0);
    let wx = (hash(fseed + 11u) - 0.5) * params.weave * 6.0 * wf / 1920.0;
    let wy = (hash(fseed + 23u) - 0.5) * params.weave * 6.0 * hf / 1080.0;
    let o = at(i32(round(x + wx)), i32(round(y + wy)));
    let a = clamp(params.age, 0.0, 1.0);
    var e = pow(max(o.xyz, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    let le = luma(e);
    let warm = vec3<f32>(1.0 + 0.07 * a, 1.0 + 0.02 * a, 1.0 - 0.13 * a);
    let sat = mix(vec3<f32>(le), e, 1.0 - 0.55 * a);
    let flat = vec3<f32>(0.5) + (sat - vec3<f32>(0.5)) * (1.0 - 0.35 * a);
    e = (flat * (1.0 - 0.10 * a) + vec3<f32>(0.07 * a)) * warm;
    var c = pow(max(e, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = mix(c, vec3<f32>(luma(c)), clamp(params.mono, 0.0, 1.0));
    let l = luma(c);
    c = c + (l * vec3<f32>(1.07, 0.93, 0.72) - c) * params.sepia;
    let t = f32(params.seed) * 0.7;
    let ti = u32(floor(t));
    let slow = mix(hash(ti + 401u), hash(ti + 402u), sstep(0.0, 1.0, fract(t)));
    let tilt = hash(fseed + 5u) * 6.283185;
    let nx = (x - wf * 0.5) / (wf * 0.5);
    let ny = (y - hf * 0.5) / (hf * 0.5);
    let flick = 1.0 + params.flicker * damage * (0.18 * ((slow - 0.5) * 1.4 + (hash(fseed) - 0.5) * 0.6) + 0.05 * (nx * cos(tilt) + ny * sin(tilt)));
    c = c * flick;
    if params.stains > 0.0 {
        for (var k = 0u; k < 3u; k++) {
            let drift = f32(params.seed) * 0.004 + f32(k) * 2.1;
            let cx = (hash(k * 13u + 1u) + 0.04 * sin(drift)) * wf;
            let cy = (hash(k * 13u + 2u) + 0.03 * cos(drift * 0.7)) * hf;
            let r = (0.06 + 0.12 * hash(k * 13u + 3u)) * hf;
            let d = sqrt((x - cx) * (x - cx) + (y - cy) * (y - cy)) / r + 0.25 * (vnoise(x / (r * 0.5), y / (r * 0.5), k + 900u) - 0.5);
            let m = (1.0 - sstep(0.7, 1.0, d)) * params.stains * damage * 0.5;
            let rim = sstep(0.85, 0.95, d) * (1.0 - sstep(0.95, 1.05, d)) * params.stains * damage * 0.25;
            c = c * mix(vec3<f32>(1.0), vec3<f32>(0.85, 0.72, 0.5), m) * (1.0 - rim);
        }
    }
    let n_scr = u32(round(params.scratches * damage * 4.0));
    let life = params.seed / 6u;
    for (var k = 0u; k < n_scr; k++) {
        let sx = hash((life + k * 97u) * 7919u) * wf;
        let jitter = (hash(params.seed + k * 31u) - 0.5) * 2.0;
        if abs(x - (sx + jitter)) < 0.8 {
            c = c + (vec3<f32>(1.0) - c) * 0.35;
        }
    }
    for (var k = 0u; k < 8u; k++) {
        let cover = debris_cover(k, x, y);
        if cover.x > 0.0 {
            if cover.y > 0.5 {
                c = mix(c, vec3<f32>(0.92), cover.x * 0.85);
            } else {
                c = mix(c, vec3<f32>(0.02), cover.x * 0.9);
            }
        }
    }
    if params.frame_lines > 0.0 {
        let edge = min(y + wy, hf - y - wy);
        let m = params.frame_lines * (1.0 - sstep(0.0, 0.035 * hf, edge));
        c = c * (1.0 - 0.85 * m);
    }
    if params.leaks > 0.0 {
        var side = 0.0;
        if hash(params.seed / 48u + 77u) > 0.5 { side = wf; }
        let ly = hf * (0.3 + 0.4 * abs(sin(f32(params.seed) * 0.013)));
        let d = sqrt((x - side) * (x - side) + (y - ly) * (y - ly)) / (0.3 * wf);
        let g = 0.5 * params.leaks * damage * exp(-d * d) * (0.6 + 0.4 * sin(f32(params.seed) * 0.05));
        c = c + g * vec3<f32>(1.0, 0.45, 0.15);
    }
    if params.burn > 0.0 {
        let edge = min(min(x, wf - x), min(y, hf - y)) / (0.12 * min(wf, hf));
        let ee = edge + 0.3 * (vnoise(x / (40.0 * s), y / (40.0 * s), 555u) - 0.5);
        let b = params.burn * damage * (1.0 - sstep(0.0, 1.0, ee));
        c = mix(c, c * vec3<f32>(1.0, 0.55, 0.25) * 0.6, b) * (1.0 - 0.8 * b * b);
    }
    let vig = 1.0 - 0.25 * (nx * nx + ny * ny);
    output[idx] = pack(vec4<f32>(c * vig, o.w));
}
