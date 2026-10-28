
fn hash_u(v: u32) -> u32 {
    var x = v;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    return (x >> 16u) ^ x;
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    return f32(hash_u(a ^ hash_u(b ^ hash_u(c)))) / 4294967295.0;
}

fn warp_wave(x: f32, y: f32, w: f32, h: f32, envelope: f32, frequency: f32, dir: u32, shift: f32) -> vec3<f32> {
    let ay = (y / h * frequency + shift) * 6.28318530;
    let ax = (x / w * frequency + shift) * 6.28318530;
    var off_x: f32 = 0.0;
    var off_y: f32 = 0.0;
    var crest: f32 = 0.0;
    if dir == 0u {
        off_x = sin(ay) * envelope;
        crest = cos(ay);
    } else if dir == 1u {
        off_y = sin(ax) * envelope;
        crest = cos(ax);
    } else {
        off_x = sin(ay) * envelope * 0.7;
        off_y = sin(ax) * envelope * 0.7;
        crest = (cos(ay) + cos(ax)) * 0.5;
    }
    let sx = clamp(x + off_x, 0.0, w - 1.0);
    let sy = clamp(y + off_y, 0.0, h - 1.0);
    return vec3<f32>(sx, sy, crest);
}

fn warp_twirl(x: f32, y: f32, w: f32, h: f32, cx: f32, cy: f32, max_r: f32, angle: f32, falloff: f32) -> vec2<f32> {
    let dx = x - cx;
    let dy = y - cy;
    let dist = length(vec2<f32>(dx, dy));
    let norm_dist = dist / max_r;
    let swirl_angle = angle * exp(-norm_dist * falloff);
    let cos_a = cos(swirl_angle);
    let sin_a = sin(swirl_angle);
    let sx = clamp(cos_a * dx - sin_a * dy + cx, 0.0, w - 1.0);
    let sy = clamp(sin_a * dx + cos_a * dy + cy, 0.0, h - 1.0);
    return vec2<f32>(sx, sy);
}

fn warp_ripple(x: f32, y: f32, w: f32, h: f32, cx: f32, cy: f32, rings: f32, shift: f32, wave: f32, amp: f32) -> vec3<f32> {
    let dx = x - cx;
    let dy = y - cy;
    let dist = length(vec2<f32>(dx, dy));
    let phase = dist / h * rings * 6.28318530 - shift;
    let ripple = sin(phase) * wave * amp * h / 1080.0;
    var nx: f32 = 0.0;
    var ny: f32 = 0.0;
    if dist > 0.1 {
        nx = dx / dist;
        ny = dy / dist;
    }
    let sx = clamp(x + nx * ripple, 0.0, w - 1.0);
    let sy = clamp(y + ny * ripple, 0.0, h - 1.0);
    return vec3<f32>(sx, sy, phase);
}

fn warp_bulge(x: f32, y: f32, cx: f32, cy: f32, radius: f32, strength: f32) -> vec2<f32> {
    let dx = x - cx;
    let dy = y - cy;
    let r = length(vec2<f32>(dx, dy)) / max(radius, 1.0);
    if r >= 1.0 || r < 0.000001 {
        return vec2<f32>(x, y);
    }
    let k = pow(r, strength);
    return vec2<f32>(cx + dx * k, cy + dy * k);
}

fn warp_band_shift(y: f32, band_h: f32, step: u32, envelope: f32, w: f32) -> f32 {
    let band = u32(y / band_h);
    let on = select(0.0, 1.0, rand3(band, step, 6u) > 0.35);
    return (0.3 + 0.7 * rand3(band, step, 7u)) * envelope * 0.04 * w * on;
}
