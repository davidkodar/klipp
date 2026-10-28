
struct Params {
    width: u32,
    height: u32,
    hue_shift: f32,
    saturation: f32,
    value_mult: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
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
    let p = unpack(input[idx]);
    let e = enc3(p.xyz);
    let r = e.x;
    let g = e.y;
    let b = e.z;

    // RGB to HSV
    let cmax = max(r, max(g, b));
    let cmin = min(r, min(g, b));
    let delta = cmax - cmin;

    var h = 0.0;
    if delta > 0.001 {
        if abs(cmax - r) < 0.001 {
            h = ((g - b) / delta) % 6.0;
        } else if abs(cmax - g) < 0.001 {
            h = (b - r) / delta + 2.0;
        } else {
            h = (r - g) / delta + 4.0;
        }
        h = h * 60.0;
        if h < 0.0 { h += 360.0; }
    }
    let s = select(0.0, delta / cmax, cmax > 0.001);
    let val = cmax;

    // Apply adjustments
    h = (h + params.hue_shift + 360.0) % 360.0;
    let new_s = clamp(s * params.saturation, 0.0, 1.0);
    let new_v = val * params.value_mult;

    // HSV to RGB
    let c = new_v * new_s;
    let x = c * (1.0 - abs((h / 60.0) % 2.0 - 1.0));
    let m = new_v - c;

    var rr = 0.0;
    var gg = 0.0;
    var bb = 0.0;
    if h < 60.0 { rr = c; gg = x; bb = 0.0; }
    else if h < 120.0 { rr = x; gg = c; bb = 0.0; }
    else if h < 180.0 { rr = 0.0; gg = c; bb = x; }
    else if h < 240.0 { rr = 0.0; gg = x; bb = c; }
    else if h < 300.0 { rr = x; gg = 0.0; bb = c; }
    else { rr = c; gg = 0.0; bb = x; }

    output[idx] = pack(vec4<f32>(dec3(vec3<f32>(rr + m, gg + m, bb + m)), p.w));
}
