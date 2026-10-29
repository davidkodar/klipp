
fn chan_at(c: u32, x: i32, y: i32) -> f32 {
    let xi = clamp(x, 0, i32(p.w) - 1);
    let yi = clamp(y, 0, i32(p.h) - 1);
    let i = u32(yi) * p.w + u32(xi);
    if c == 0u { return c0[i]; }
    return c1[i];
}

struct Sampled { v: f32, ok: bool }

fn sample(c: u32, x: f32, y: f32) -> Sampled {
    if !(x >= 0.0 && y >= 0.0 && x <= f32(p.w - 1u) && y <= f32(p.h - 1u)) {
        return Sampled(0.0, false);
    }
    let x0 = i32(floor(x));
    let y0 = i32(floor(y));
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let top = chan_at(c, x0, y0) * (1.0 - fx) + chan_at(c, x0 + 1, y0) * fx;
    let bottom = chan_at(c, x0, y0 + 1) * (1.0 - fx) + chan_at(c, x0 + 1, y0 + 1) * fx;
    return Sampled(top * (1.0 - fy) + bottom * fy, true);
}

// The map, row by row, from the state.
fn map_point(u: vec2<f32>) -> vec3<f32> {
    let w = state[6] * u.x + state[7] * u.y + state[8];
    if w <= 1.0e-12 { return vec3<f32>(0.0, 0.0, -1.0); }
    return vec3<f32>((state[0] * u.x + state[1] * u.y + state[2]) / w, (state[3] * u.x + state[4] * u.y + state[5]) / w, 1.0);
}
