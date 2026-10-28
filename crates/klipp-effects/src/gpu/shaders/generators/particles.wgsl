//#include pixel_storage

struct Params {
    width: u32, height: u32, seed: u32, additive: u32,
    cell: f32, dir_x: f32, dir_y: f32, speed: f32,
    size: f32, spread: f32, lifetime: f32, opacity: f32,
    r: f32, g: f32, b: f32, time: f32,
    sway: f32, sway_rate: f32, gravity: f32, streak: f32,
    twinkle: f32, cooling: f32, inv_cell: f32, _pad1: f32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

const PERIOD: f32 = 64.0;

fn hash_u(v: u32) -> u32 {
    var x = v;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    return (x >> 16u) ^ x;
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    return f32(hash_u(a ^ hash_u(b ^ hash_u(c)))) / 4294967295.0;
}

fn wave(t: f32) -> f32 {
    let x = t - floor(t);
    let tri = 1.0 - 4.0 * abs(x - 0.5);
    return tri * (1.5 - 0.5 * tri * tri);
}

fn sstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = params;
    var c = unpack(input[idx]);
    let fx = f32(idx % p.width) + 0.5;
    let fy = f32(idx / p.width) + 0.5;
    for (var layer: u32 = 0u; layer < 3u; layer = layer + 1u) {
        let z = 0.55 + 0.225 * f32(layer);
        let s = p.cell;
        let span = s * PERIOD;
        var shift_x = p.dir_x * p.speed * z * p.time;
        var shift_y = p.dir_y * p.speed * z * p.time;
        shift_x = shift_x - floor(shift_x / span + 0.5) * span;
        shift_y = shift_y - floor(shift_y / span + 0.5) * span;
        let ux = fx - shift_x + s * 0.37 * f32(layer);
        let uy = fy - shift_y + s * 0.61 * f32(layer);
        let cx0 = floor(ux * p.inv_cell);
        let cy0 = floor(uy * p.inv_cell);
        let key = p.seed * 131u + layer * 7919u;
        for (var j: u32 = 0u; j < 9u; j = j + 1u) {
            let cxf = cx0 + f32(j % 3u) - 1.0;
            let cyf = cy0 + f32(j / 3u) - 1.0;
            let ci = u32(cxf - floor(cxf / PERIOD) * PERIOD);
            let cj = u32(cyf - floor(cyf / PERIOD) * PERIOD);
            let h1 = rand3(ci, cj, key);
            let h2 = rand3(ci, cj, key + 1u);
            let h3 = rand3(ci, cj, key + 2u);
            let h4 = rand3(ci, cj, key + 3u);
            let h5 = rand3(ci, cj, key + 4u);
            let life = p.lifetime * (0.6 + 0.8 * h1);
            let q = p.time / life + h2;
            let cycle = floor(q);
            let age = q - cycle;
            let born = hash_u(u32(i32(cycle)));
            let ax = (cxf + rand3(ci ^ born, cj, key + 5u)) * s;
            let ay = (cyf + rand3(ci, cj ^ born, key + 6u)) * s;
            let sw = wave(p.time * p.sway_rate + h3) * p.sway * s;
            let sx = wave(h4);
            let sy = wave(h4 + 0.25);
            var dx = -p.dir_y * sw + sx * p.spread * s * age;
            var dy = p.dir_x * sw + sy * p.spread * s * age + p.gravity * s * age * age;
            let len = sqrt(dx * dx + dy * dy);
            let reach = 0.45 * s;
            if len > reach {
                dx = dx * (reach / len);
                dy = dy * (reach / len);
            }
            let px_ = ax + dx;
            let py_ = ay + dy;
            var ex = ux - px_;
            var ey = uy - py_;
            let tail = p.speed * z * p.streak;
            if tail > 0.0 {
                let along = clamp(ex * -p.dir_x + ey * -p.dir_y, 0.0, tail);
                ex = ex + p.dir_x * along;
                ey = ey + p.dir_y * along;
            }
            let dist = sqrt(ex * ex + ey * ey);
            let true_radius = min(p.size * z * (0.6 + 0.8 * h5), 0.25 * s);
            let radius = min(max(true_radius, 1.0), 0.25 * s);
            if dist >= radius {
                continue;
            }
            let f = 1.0 - dist / radius;
            var a = (true_radius / radius) * (true_radius / radius) * p.opacity * (0.45 + 0.55 * z) * sstep(0.0, 0.12, age) * (1.0 - sstep(0.65, 1.0, age)) * f * f * (3.0 - 2.0 * f);
            if p.twinkle > 0.0 {
                let tw = wave(p.time * 2.3 + h3 * 7.0);
                a = a * (1.0 - p.twinkle * 0.5 * (1.0 + tw));
            }
            let warm = 1.0 - p.cooling * age;
            if p.additive == 1u {
                c.x = c.x + p.r * warm * a * 1.5;
                c.y = c.y + p.g * warm * warm * a * 1.5;
                c.z = c.z + p.b * warm * warm * warm * a * 1.5;
                c.w = c.w + a * (1.0 - c.w);
            } else {
                c.x = c.x + (p.r - c.x) * a;
                c.y = c.y + (p.g - c.y) * a;
                c.z = c.z + (p.b - c.z) * a;
                c.w = c.w + a * (1.0 - c.w);
            }
        }
    }
    output[idx] = pack(c);
}
