
struct Params {
    width: u32,
    height: u32,
    hue_center: f32,
    hue_width: f32,
    hue_soft: f32,
    sat_low: f32,
    sat_high: f32,
    sat_soft: f32,
    lum_low: f32,
    lum_high: f32,
    lum_soft: f32,
    invert: u32,
    sec_exposure: f32,
    sec_saturation: f32,
    sec_hue_shift: f32,
    sec_temperature: f32,
    matte_preview: u32,
    mode: u32,
    _pad1: u32,
    _pad2: u32,
    red_low: f32,
    red_high: f32,
    green_low: f32,
    green_high: f32,
    blue_low: f32,
    blue_high: f32,
    rgb_soft: f32,
    _pad3: f32,
    keep_mean: vec4<f32>,
    keep_inv0: vec4<f32>,
    keep_inv1: vec4<f32>,
    keep_inv2: vec4<f32>,
    remove_mean: vec4<f32>,
    remove_inv0: vec4<f32>,
    remove_inv1: vec4<f32>,
    remove_inv2: vec4<f32>,
    reach: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers


fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn soft_range_centered(dist: f32, width: f32, soft: f32) -> f32 {
    let half_w = width * 0.5;
    if dist <= half_w { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    let t = (dist - half_w) / soft;
    return clamp(1.0 - t, 0.0, 1.0);
}

// The 3D key's space and distance (#895), as `cpu_grade::volume_matte`.
fn qualifier_ycc(e: vec3<f32>) -> vec3<f32> {
    let y = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
    return vec3<f32>(y, (e.z - y) / 1.8556, (e.x - y) / 1.5748);
}

fn volume_distance(c: vec3<f32>, mean: vec4<f32>, i0: vec4<f32>, i1: vec4<f32>, i2: vec4<f32>) -> f32 {
    let d = c - mean.xyz;
    let q = d.x * dot(i0.xyz, d) + d.y * dot(i1.xyz, d) + d.z * dot(i2.xyz, d);
    return sqrt(max(q, 0.0));
}

fn volume_reach(d: f32, radius: f32, soft: f32) -> f32 {
    if d <= radius { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    return clamp(1.0 - (d - radius) / soft, 0.0, 1.0);
}

fn soft_range(val: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if val >= lo && val <= hi { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    if val < lo {
        return clamp(1.0 - (lo - val) / soft, 0.0, 1.0);
    }
    return clamp(1.0 - (val - hi) / soft, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let r = orig.x;
    let g = orig.y;
    let b = orig.z;

    // The picture's hue, saturation and lightness — display-encoded, and
    // defined for a pixel brighter than white, which used to read as
    // fully desaturated and so never qualified.
    let hsl = picture_hsl(orig.xyz);
    let h = hsl.h;
    let s = hsl.s;
    let l = picture_lum_axis(hsl);

    // Qualifier matte: by hue, saturation and luminance, or (RGB mode,
    // #895) by each display-encoded channel on its own.
    var matte = 0.0;
    if params.mode == 2u {
        if params.keep_mean.w > 0.5 {
            let e = clamp(vec3<f32>(enc_signed(r), enc_signed(g), enc_signed(b)), vec3<f32>(0.0), vec3<f32>(1.0));
            let c = qualifier_ycc(e);
            let dk = volume_distance(c, params.keep_mean, params.keep_inv0, params.keep_inv1, params.keep_inv2);
            matte = volume_reach(dk, params.reach.x, params.reach.y);
            if params.remove_mean.w > 0.5 {
                let dr = volume_distance(c, params.remove_mean, params.remove_inv0, params.remove_inv1, params.remove_inv2);
                if dr < dk {
                    matte = matte * (1.0 - volume_reach(dr, params.reach.x, params.reach.y));
                }
            }
        }
    } else if params.mode == 1u {
        let e = clamp(vec3<f32>(enc_signed(r), enc_signed(g), enc_signed(b)), vec3<f32>(0.0), vec3<f32>(1.0));
        matte = soft_range(e.x, params.red_low, params.red_high, params.rgb_soft)
            * soft_range(e.y, params.green_low, params.green_high, params.rgb_soft)
            * soft_range(e.z, params.blue_low, params.blue_high, params.rgb_soft);
    } else {
        let hue_dist = circular_distance(h, params.hue_center);
        let hue_m = soft_range_centered(hue_dist, params.hue_width, params.hue_soft);
        let sat_m = soft_range(s, params.sat_low, params.sat_high, params.sat_soft);
        let lum_m = soft_range(l, params.lum_low, params.lum_high, params.lum_soft);
        matte = hue_m * sat_m * lum_m;
    }
    if params.invert != 0u { matte = 1.0 - matte; }

    // Matte preview mode: output greyscale matte
    if params.matte_preview != 0u {
        output[idx] = pack(vec4<f32>(matte, matte, matte, orig.w));
        return;
    }

    // Skip untouched pixels
    if matte < 0.001 {
        output[idx] = input[idx];
        return;
    }

    // Secondary adjustments
    var gr = r;
    var gg = g;
    var gb = b;

    // Exposure
    if abs(params.sec_exposure) > 0.001 {
        let factor = pow(2.0, params.sec_exposure);
        gr = gr * factor;
        gg = gg * factor;
        gb = gb * factor;
    }

    // Temperature (R += t, B -= t)
    if abs(params.sec_temperature) > 0.0001 {
        gr = gr + params.sec_temperature;
        gb = gb - params.sec_temperature;
    }

    // Saturation
    if abs(params.sec_saturation - 1.0) > 0.001 {
        let luma = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb;
        gr = luma + (gr - luma) * params.sec_saturation;
        gg = luma + (gg - luma) * params.sec_saturation;
        gb = luma + (gb - luma) * params.sec_saturation;
    }

    // Hue shift (rotation matrix — coefficients are from the classic
    // AE luma-rotation matrix, tuned for linear-ish RGB)
    if abs(params.sec_hue_shift) > 0.001 {
        let cos_h = cos(params.sec_hue_shift);
        let sin_h = sin(params.sec_hue_shift);
        let ir = gr; let ig = gg; let ib = gb;
        gr = ir * (0.213 + cos_h * 0.787 - sin_h * 0.213)
           + ig * (0.715 - cos_h * 0.715 - sin_h * 0.715)
           + ib * (0.072 - cos_h * 0.072 + sin_h * 0.928);
        gg = ir * (0.213 - cos_h * 0.213 + sin_h * 0.143)
           + ig * (0.715 + cos_h * 0.285 + sin_h * 0.140)
           + ib * (0.072 - cos_h * 0.072 - sin_h * 0.283);
        gb = ir * (0.213 - cos_h * 0.213 - sin_h * 0.787)
           + ig * (0.715 - cos_h * 0.715 + sin_h * 0.715)
           + ib * (0.072 + cos_h * 0.928 + sin_h * 0.072);
    }

    // Blend through matte
    let out = vec3<f32>(
        r * (1.0 - matte) + gr * matte,
        g * (1.0 - matte) + gg * matte,
        b * (1.0 - matte) + gb * matte,
    );
    output[idx] = pack(vec4<f32>(out, orig.w));
}
