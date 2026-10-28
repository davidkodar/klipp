
struct Params {
    width: u32,
    height: u32,
    low: f32,
    high: f32,
    soft: f32,
    invert: u32,
    into_alpha: u32,
    _pad1: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers


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
    // Luma of the *encoded* picture, which is the axis the Qualifier's
    // lum window uses (`picture_lum_axis`) and the one the CPU twin
    // measures, since it quantises to u8 first. Keying linear luma here
    // instead meant the same threshold picked a different brightness
    // depending on which path ran: 0.5 is mid-grey as seen, not a bright
    // highlight. Weighting the encoded values rather than the linear
    // ones is the BT.709 luma (Y') definition, and the same order the
    // scopes use, so a threshold here means what the waveform shows.
    //
    // The coefficients come from the working space, not from Rec.709:
    // this picture is in the project's own primaries, so an ACEScg
    // project needs AP1's row (0.272/0.674/0.054) and Rec.709's
    // under-weights green. They arrive via `set_luma_coefs`, which the
    // executor installs from the loaded project.
    // Each channel clamped to [0, 1], as the CPU keys it
    // (`luma_key_value`): unclamped, a highlight above white fell out of
    // a window up to 1.0, the whole range (#1439).
    let e = clamp(enc3(orig.xyz), vec3<f32>(0.0), vec3<f32>(1.0));
    let y = dot(e, params.luma.xyz);

    var matte = soft_range(y, params.low, params.high, params.soft);
    if params.invert != 0u { matte = 1.0 - matte; }

    // The Luma Key clip effect (#1638): the matte multiplies the
    // picture's alpha, its colour untouched, as the CPU twin does.
    if params.into_alpha != 0u {
        output[idx] = pack(vec4<f32>(orig.xyz, orig.w * matte));
        return;
    }

    // A matte is opaque, as the CPU writes it (#1037): the input's alpha
    // made the two disagree over a part-transparent picture.
    output[idx] = pack(vec4<f32>(matte, matte, matte, 1.0));
}
