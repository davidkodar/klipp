//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
    tetrahedral: u32,
    bracket: u32,
    _pad0: u32,
    _pad1: u32,
    pre0: vec4<f32>,
    pre1: vec4<f32>,
    pre2: vec4<f32>,
    post0: vec4<f32>,
    post1: vec4<f32>,
    post2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;

fn lut_sample(ri: u32, gi: u32, bi: u32) -> vec3<f32> {
    let n = params.lut_size;
    let idx = (bi * n * n + gi * n + ri) * 3u;
    return vec3<f32>(lut[idx], lut[idx + 1u], lut[idx + 2u]);
}

fn linear_to_srgb_component(c: f32) -> f32 {
    if (c <= 0.0031308) { return c * 12.92; }
    else { return 1.055 * pow(c, 1.0/2.4) - 0.055; }
}

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    else { return pow((c + 0.055) / 1.055, 2.4); }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let pixel_idx = gid.x;
    if pixel_idx >= params.width * params.height { return; }

    let pixel = unpack(input[pixel_idx]);
    let orig = vec3<f32>(pixel.x, pixel.y, pixel.z);
    var lut_in = orig;
    if (params.bracket & 1u) != 0u {
        lut_in = vec3<f32>(dot(params.pre0.xyz, orig), dot(params.pre1.xyz, orig), dot(params.pre2.xyz, orig));
    }
    let r_in = clamp(linear_to_srgb_component(lut_in.x), 0.0, 1.0);
    let g_in = clamp(linear_to_srgb_component(lut_in.y), 0.0, 1.0);
    let b_in = clamp(linear_to_srgb_component(lut_in.z), 0.0, 1.0);

    let n1 = f32(params.lut_size - 1u);

    let r_idx = clamp(r_in * n1, 0.0, n1);
    let g_idx = clamp(g_in * n1, 0.0, n1);
    let b_idx = clamp(b_in * n1, 0.0, n1);

    let r0 = min(u32(r_idx), params.lut_size - 2u);
    let g0 = min(u32(g_idx), params.lut_size - 2u);
    let b0 = min(u32(b_idx), params.lut_size - 2u);
    let r1 = r0 + 1u;
    let g1 = g0 + 1u;
    let b1 = b0 + 1u;

    let rf = r_idx - f32(r0);
    let gf = g_idx - f32(g0);
    let bf = b_idx - f32(b0);

    let c000 = lut_sample(r0, g0, b0);
    let c100 = lut_sample(r1, g0, b0);
    let c010 = lut_sample(r0, g1, b0);
    let c110 = lut_sample(r1, g1, b0);
    let c001 = lut_sample(r0, g0, b1);
    let c101 = lut_sample(r1, g0, b1);
    let c011 = lut_sample(r0, g1, b1);
    let c111 = lut_sample(r1, g1, b1);

    var lut_out: vec3<f32>;
    if params.tetrahedral != 0u {
        // Tetrahedral: the unit cube split into six tetrahedra by the
        // order of the fractional coordinates, so the sample is
        // interpolated over the four corners that surround it rather
        // than all eight. It holds a LUT's straight lines straight,
        // where trilinear bows them — the wobble that shows on a skin
        // tone or in a sky gradient.
        if rf > gf {
            if gf > bf {
                lut_out = c000 + (c100 - c000) * rf + (c110 - c100) * gf + (c111 - c110) * bf;
            } else if rf > bf {
                lut_out = c000 + (c100 - c000) * rf + (c101 - c100) * bf + (c111 - c101) * gf;
            } else {
                lut_out = c000 + (c001 - c000) * bf + (c101 - c001) * rf + (c111 - c101) * gf;
            }
        } else if rf > bf {
            lut_out = c000 + (c010 - c000) * gf + (c110 - c010) * rf + (c111 - c110) * bf;
        } else if gf > bf {
            lut_out = c000 + (c010 - c000) * gf + (c011 - c010) * bf + (c111 - c011) * rf;
        } else {
            lut_out = c000 + (c001 - c000) * bf + (c011 - c001) * gf + (c111 - c011) * rf;
        }
    } else {
        let c00 = mix(c000, c100, rf);
        let c01 = mix(c001, c101, rf);
        let c10 = mix(c010, c110, rf);
        let c11 = mix(c011, c111, rf);
        let c0 = mix(c00, c10, gf);
        let c1 = mix(c01, c11, gf);
        lut_out = mix(c0, c1, bf);
    }

    // Decode LUT output (sRGB) back to linear, and into the working
    // space's primaries.
    var graded = vec3<f32>(
        srgb_to_linear_component(clamp(lut_out.x, 0.0, 1.0)),
        srgb_to_linear_component(clamp(lut_out.y, 0.0, 1.0)),
        srgb_to_linear_component(clamp(lut_out.z, 0.0, 1.0)),
    );
    if (params.bracket & 2u) != 0u {
        graded = vec3<f32>(dot(params.post0.xyz, graded), dot(params.post1.xyz, graded), dot(params.post2.xyz, graded));
    }

    // Opacity blend in linear.
    let result = mix(orig, graded, params.opacity);

    output[pixel_idx] = pack(vec4<f32>(result.x, result.y, result.z, pixel.w));
}
