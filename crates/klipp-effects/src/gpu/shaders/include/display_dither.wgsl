
fn dither_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

/// `klipp_effects::dither::dither_at`: triangular, in code values, in
/// [-1, 1], from the pixel's index `y * width + x`.
fn dither_at(index: u32) -> f32 {
    let a = f32(dither_hash(index)) / 4294967296.0;
    let b = f32(dither_hash(index ^ 0x9e3779b9u)) / 4294967296.0;
    return a + b - 1.0;
}

/// `klipp_effects::dither::quantise` at 8 bits: a value on 0 or 1, or
/// already on a code value (within `ON_CODE_TOLERANCE`), is not dithered.
fn quantise_u8(v: f32, d: f32) -> u32 {
    let c = clamp(v, 0.0, 1.0);
    let x = c * 255.0;
    var n = d;
    if (c <= 0.0 || c >= 1.0 || abs(x - round(x)) < 1e-3) { n = 0.0; }
    return u32(clamp(x + 0.5 + n, 0.0, 255.0));
}
