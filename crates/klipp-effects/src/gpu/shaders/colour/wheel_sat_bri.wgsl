
struct Params {
    width: u32,
    height: u32,
    lift_sat: f32,
    lift_bri: f32,
    gamma_sat: f32,
    gamma_bri: f32,
    gain_sat: f32,
    gain_bri: f32,
    offset_sat: f32,
    offset_bri: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    let luma = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;

    let lift_w = max(1.0 - luma, 0.0);
    let gamma_w = max(sin(clamp(luma, 0.0, 1.0) * 3.14159265), 0.0);
    let gain_w = luma;

    let bri_adj = params.lift_bri * lift_w + params.gamma_bri * gamma_w + params.gain_bri * gain_w + params.offset_bri;

    r = r + bri_adj;
    g = g + bri_adj;
    b = b + bri_adj;

    let sat_adj = params.lift_sat * lift_w + params.gamma_sat * gamma_w + params.gain_sat * gain_w + params.offset_sat;
    let sat_factor = 1.0 + sat_adj;
    let luma2 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    r = luma2 + (r - luma2) * sat_factor;
    g = luma2 + (g - luma2) * sat_factor;
    b = luma2 + (b - luma2) * sat_factor;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
