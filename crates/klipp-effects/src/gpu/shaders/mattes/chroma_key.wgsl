//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    soft_edge: f32,
    spill_method: u32,
    spill_strength: f32,
    spill_key_idx: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn linear_to_srgb_component(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return c * 12.92;
    } else {
        return 1.055 * pow(c, 1.0/2.4) - 0.055;
    }
}

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

fn rgb_to_cbcr(r: f32, g: f32, b: f32) -> vec2<f32> {
    let cb = 128.0 + (-0.168736 * r - 0.331264 * g + 0.5 * b);
    let cr = 128.0 + (0.5 * r - 0.418688 * g - 0.081312 * b);
    return vec2<f32>(cb, cr);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    var pixel = unpack(input[idx]);
    // Re-encode to sRGB 0-255 for YCbCr math (matches CPU path + user input).
    let srgb_r = clamp(linear_to_srgb_component(pixel.x), 0.0, 1.0) * 255.0;
    let srgb_g = clamp(linear_to_srgb_component(pixel.y), 0.0, 1.0) * 255.0;
    let srgb_b = clamp(linear_to_srgb_component(pixel.z), 0.0, 1.0) * 255.0;
    let cbcr = rgb_to_cbcr(srgb_r, srgb_g, srgb_b);

    let dcb = (cbcr.x - params.key_cb) / 255.0;
    let dcr = (cbcr.y - params.key_cr) / 255.0;
    let dist = sqrt(dcb * dcb + dcr * dcr);

    if dist < params.tolerance {
        // Fully keyed out — no spill stage needed.
        output[idx] = pack(vec4<f32>(0.0));
        return;
    } else if dist < params.tolerance + params.soft_edge {
        let factor = (dist - params.tolerance) / params.soft_edge;
        pixel.w = pixel.w * factor;
    }

    // Spill suppression on whatever's still visible. Mirrors the CPU
    // `apply_spill` shape: pick a target value the key channel drops
    // toward when it exceeds that target, blend by spill_strength.
    if params.spill_method != 0u && params.spill_strength > 0.0 && pixel.w > 0.0 {
        // On the pixel's own values, unclamped, and only the key channel
        // changes, as on the CPU: above white and below black survive
        // (#1020).
        let srgb = vec3<f32>(linear_to_srgb_component(pixel.x), linear_to_srgb_component(pixel.y), linear_to_srgb_component(pixel.z)) * 255.0;
        let key = srgb[params.spill_key_idx];
        let other_a_idx = (params.spill_key_idx + 1u) % 3u;
        let other_b_idx = (params.spill_key_idx + 2u) % 3u;
        let other_a = srgb[other_a_idx];
        let other_b = srgb[other_b_idx];
        var spill_target: f32 = key;
        if params.spill_method == 1u {
            spill_target = max(other_a, other_b);
        } else if params.spill_method == 2u {
            spill_target = 0.5 * (other_a + other_b);
        }
        if key > spill_target {
            let new_key = key + params.spill_strength * (spill_target - key);
            pixel[params.spill_key_idx] = srgb_to_linear_component(new_key / 255.0);
        }
    }

    output[idx] = pack(pixel);
}
