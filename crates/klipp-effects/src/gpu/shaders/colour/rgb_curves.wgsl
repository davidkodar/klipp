
struct Params {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 4 channels x 256 entries = 1024 floats
// 0..255   = master
// 256..511 = red
// 512..767 = green
// 768..1023 = blue
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// The curve inside its [0, 1] domain.
fn lut_lookup(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    let idx_f = clamp(val, 0.0, 1.0) * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

// The curve over the whole real line: straight past either end, at the
// slope the curve ends with there.
fn curve(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    if val > 1.0 {
        let slope = (lut[base + 255u] - lut[base + 254u]) * 255.0;
        return lut[base + 255u] + (val - 1.0) * slope;
    }
    if val < 0.0 {
        let slope = (lut[base + 1u] - lut[base]) * 255.0;
        return lut[base] + val * slope;
    }
    return lut_lookup(channel, val);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);

    // Per-channel curves, then master, on the display-encoded picture.
    var r = curve(1u, enc_signed(orig.x));
    var g = curve(2u, enc_signed(orig.y));
    var b = curve(3u, enc_signed(orig.z));
    r = dec_signed(curve(0u, r));
    g = dec_signed(curve(0u, g));
    b = dec_signed(curve(0u, b));

    // Preserve luma: scale output to match input luma
    if params.preserve_luma != 0u {
        let orig_luma = params.luma_yr * orig.x + params.luma_yg * orig.y + params.luma_yb * orig.z;
        let new_luma = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
        if new_luma > 0.0001 {
            let scale = orig_luma / new_luma;
            r = r * scale;
            g = g * scale;
            b = b * scale;
        }
    }

    // Mix with original
    r = orig.x + (r - orig.x) * params.mix;
    g = orig.y + (g - orig.y) * params.mix;
    b = orig.z + (b - orig.z) * params.mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
