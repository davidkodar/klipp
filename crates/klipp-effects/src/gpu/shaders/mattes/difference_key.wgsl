
struct Params {
    width: u32,
    height: u32,
    tolerance: f32,
    soft: f32,
    invert: u32,
    match_space: u32,
    _pad0: u32,
    _pad1: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> plate: array<vec4<f32>>;
//#include pixel_storage


// `soft_step` from the CPU twin: 0 below `lo`, a linear ramp to 1 across
// [lo, hi], 1 above, collapsing to a hard step when the band is empty.
// The 1e-4 floor is the CPU's, not a rounding guard — without it a zero
// `soft` divides by zero instead of keying hard.
fn diff_soft_step(val: f32, lo: f32, hi: f32) -> f32 {
    if val <= lo { return 0.0; }
    if hi - lo <= 0.0001 { return 1.0; }
    if val >= hi { return 1.0; }
    return clamp((val - lo) / (hi - lo), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let live = unpack(input[idx]);
    let ref_px = unpack(plate[idx]);

    var d: f32;
    if params.match_space == 1u {
        // Weighted luminance difference — forgiving of plate chroma
        // drift. The working space's own row, since both sides are
        // linear working-space values.
        let yl = dot(live.xyz, params.luma.xyz);
        let yp = dot(ref_px.xyz, params.luma.xyz);
        d = abs(yl - yp);
    } else {
        // Chebyshev / L-infinity, per channel, so a colour-only
        // difference registers even when luminance matches.
        let e = abs(live.xyz - ref_px.xyz);
        d = max(e.x, max(e.y, e.z));
    }

    var matte = diff_soft_step(d, params.tolerance, params.tolerance + params.soft);
    if params.invert != 0u { matte = 1.0 - matte; }

    output[idx] = pack(vec4<f32>(matte, matte, matte, live.w));
}
