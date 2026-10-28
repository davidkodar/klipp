
fn grain_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

/// `klipp_effects::generator_grain::grain_noise`.
fn grain_noise(index: u32) -> f32 {
    let a = f32(grain_hash(index ^ 0x85ebca6bu) >> 8u) / 16777216.0;
    let b = f32(grain_hash(index ^ 0xc2b2ae35u) >> 8u) / 16777216.0;
    let c = f32(grain_hash(index ^ 0x27d4eb2fu) >> 8u) / 16777216.0;
    return (a + b + c - 1.5) * 2.0;
}

/// `klipp_effects::generator_grain::grain_channel`.
fn grain_channel(v: f32, n: f32, s: f32) -> f32 {
    let taper = clamp(min(v, 1.0 - v) * 63.75, 0.0, 1.0);
    return clamp(v + n * s * taper / 255.0, 0.0, 1.0);
}
