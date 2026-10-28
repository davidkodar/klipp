//#include pixel_storage

struct Params {
    width: u32,
    height: u32,
    shadow_r: f32,
    shadow_g: f32,
    shadow_b: f32,
    mid_r: f32,
    mid_g: f32,
    mid_b: f32,
    highlight_r: f32,
    highlight_g: f32,
    highlight_b: f32,
    mix: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn gm_encode(c: f32) -> f32 {
    if c <= 0.0031308 { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn gm_decode(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    let e = vec3<f32>(gm_encode(max(px.r, 0.0)), gm_encode(max(px.g, 0.0)), gm_encode(max(px.b, 0.0)));
    let l = clamp(e.x * params.luma_yr + e.y * params.luma_yg + e.z * params.luma_yb, 0.0, 1.0);
    let shadow = vec3<f32>(params.shadow_r, params.shadow_g, params.shadow_b);
    let mid = vec3<f32>(params.mid_r, params.mid_g, params.mid_b);
    let highlight = vec3<f32>(params.highlight_r, params.highlight_g, params.highlight_b);
    var a = mid;
    var b = highlight;
    var t = (l - 0.5) * 2.0;
    if l < 0.5 {
        a = shadow;
        b = mid;
        t = l * 2.0;
    }
    let m = clamp(params.mix, 0.0, 1.0);
    let s = a + (b - a) * t;
    let ramp = vec3<f32>(gm_decode(s.x), gm_decode(s.y), gm_decode(s.z));
    output[idx] = pack(vec4<f32>(px.rgb + (ramp - px.rgb) * m, px.a));
}
