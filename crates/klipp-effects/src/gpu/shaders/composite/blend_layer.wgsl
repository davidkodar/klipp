
struct Params { width: u32, height: u32, opacity: f32, mode: u32, y0: u32, y1: u32, _p0: u32, _p1: u32 }
@group(0) @binding(0) var<storage, read> layer: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> acc: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers
//#include blend_modes

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let d = unpack(acc[idx]);
    let s = unpack(layer[idx]);
    if params.mode == 13u {
        acc[idx] = pack(d + (s - d) * params.opacity);
        return;
    }
    let sa = s.w * params.opacity;
    if sa < 0.001 {
        return;
    }
    let da = d.w;
    let b = blend3(params.mode, d.xyz, s.xyz);
    let out_a = sa + da * (1.0 - sa);
    if out_a > 0.0 {
        let c = (b * sa + d.xyz * da * (1.0 - sa)) / out_a;
        acc[idx] = pack(vec4<f32>(c, out_a));
    }
}
