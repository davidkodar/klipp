
struct Params {
    width: u32,
    height: u32,
    mix: f32,
    // Bit c set: curve c was edited. The others are left out: an
    // unedited curve's LUT is filler, and Sat vs Sat's would set every
    // saturation to 0.5.
    curves_on: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// LUT layout: 7 curves × 256 entries = indices 0..1791
// Curve 0: hue_vs_hue   (0..255)
// Curve 1: hue_vs_sat   (256..511)
// Curve 2: hue_vs_lum   (512..767)
// Curve 3: lum_vs_sat   (768..1023)
// Curve 4: sat_vs_sat   (1024..1279)
// Curve 5: orange_vs_sat (1280..1535)
// Curve 6: sat_vs_lum   (1536..1791)

fn lut_lookup(curve: u32, t: f32) -> f32 {
    let base = curve * 256u;
    let idx_f = t * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let hsl = picture_hsl(orig.xyz);
    let lum_axis = picture_lum_axis(hsl);
    var h = hsl.h;
    var s = hsl.s;
    // The brightness the curves move is the value, not the lightness:
    // saturation is now a fraction of the brightest channel, so the two
    // have to be adjusted in the same currency or a lightness curve is
    // computed and then discarded. For a grey the two are identical; for
    // a saturated colour a shift here moves the peak channel.
    var v = hsl.v;

    // Curve 0: Hue vs Hue (0.5 = no change)
    if (params.curves_on & 1u) != 0u {
        let hvh = lut_lookup(0u, h);
        h = (h + (hvh - 0.5) * 2.0) % 1.0;
        if h < 0.0 { h = h + 1.0; }
    }

    // Curve 1: Hue vs Sat (0.5 = identity)
    if (params.curves_on & 2u) != 0u {
        let hvs = lut_lookup(1u, hsl.h);
        s = clamp(s * hvs * 2.0, 0.0, 1.0);
    }

    // Curve 2: Hue vs Lum (0.5 = no change)
    if (params.curves_on & 4u) != 0u {
        let hvl = lut_lookup(2u, hsl.h);
        v = clamp(v + (hvl - 0.5), 0.0, 1.0);
    }

    // Curve 3: Lum vs Sat (0.5 = identity)
    if (params.curves_on & 8u) != 0u {
        let lvs = lut_lookup(3u, lum_axis);
        s = clamp(s * lvs * 2.0, 0.0, 1.0);
    }

    // Curve 4: Sat vs Sat, a mapping of the input saturation
    if (params.curves_on & 16u) != 0u {
        let svs = lut_lookup(4u, hsl.s);
        s = clamp(svs, 0.0, 1.0);
    }

    // Curve 5: Orange vs Sat (targeted at ~30°/360° ≈ 0.083)
    if (params.curves_on & 32u) != 0u {
        let ovs = lut_lookup(5u, hsl.s);
        let orange_dist = min(abs(hsl.h - 0.083), min(abs(hsl.h - 0.083 + 1.0), abs(hsl.h - 0.083 - 1.0)));
        let weight = clamp(1.0 - orange_dist * 8.0, 0.0, 1.0);
        s = s + (ovs - s) * weight;
    }

    // Curve 6: Sat vs Lum (0.5 = no change). Indexed by the incoming
    // saturation, like every other s-indexed curve, so an earlier
    // saturation curve does not move this one's input.
    if (params.curves_on & 64u) != 0u {
        let svl = lut_lookup(6u, hsl.s);
        v = clamp(v + (svl - 0.5), 0.0, 1.0);
    }

    let rgb = picture_rgb(h, s, v, hsl.magnitude);

    let mixed = orig.xyz + (rgb - orig.xyz) * params.mix;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
