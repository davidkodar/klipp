
struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    luma: vec4<f32>,
    gain_sat: array<vec4<f32>, 7>,
    edge: array<vec4<f32>, 7>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

fn zone_weight(i: u32, s: f32, b: f32, f: f32) -> f32 {
    if i == 0u { return 1.0; }
    let half = max(f, 0.0) * 0.5;
    let lo = b - half;
    let hi = b + half;
    var up: f32;
    if hi <= lo {
        up = select(0.0, 1.0, s >= lo);
    } else {
        let t = clamp((s - lo) / (hi - lo), 0.0, 1.0);
        up = t * t * (3.0 - 2.0 * t);
    }
    if i <= 3u { return 1.0 - up; }
    return up;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let orig = unpack(input[idx]);
    let luma = params.luma.xyz;
    let s = log2(max(dot(luma, orig.xyz), 0.18 / 1048576.0) / 0.18);
    var sum = vec3<f32>(0.0);
    var sat = 1.0;
    for (var i = 0u; i < 7u; i = i + 1u) {
        let w = zone_weight(i, s, params.edge[i].x, params.edge[i].y);
        sum = sum + w * params.gain_sat[i].xyz;
        sat = sat * (1.0 + w * (params.gain_sat[i].w - 1.0));
    }
    var c = select(orig.xyz * exp2(sum), orig.xyz, sum == vec3<f32>(0.0));
    if sat != 1.0 {
        let y = dot(luma, c);
        c = vec3<f32>(y) + (c - vec3<f32>(y)) * sat;
    }
    output[idx] = pack(vec4<f32>(c, orig.w));
}
