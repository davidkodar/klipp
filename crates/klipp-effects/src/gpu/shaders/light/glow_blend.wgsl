
struct Params {
    width: u32, height: u32, intensity: f32, desat: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, lift_alpha: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> blurred: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let o = unpack(original[idx]);
    let bl = unpack(blurred[idx]).xyz;
    let l = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), bl.xyz);
    let b = (bl + (vec3<f32>(l) - bl) * params.desat) * params.intensity;
    if params.lift_alpha == 1u {
        // The light (premultiplied) screened over the layer as seen, and
        // seen itself where the layer is clear: a glow past a title's
        // edges (#2703). An opaque pixel comes out as below.
        let lit = screen3_at_ceiling(o.xyz * o.w, b);
        let a = o.w + clamp(max(b.x, max(b.y, b.z)), 0.0, 1.0) * (1.0 - o.w);
        output[idx] = pack(select(vec4<f32>(0.0), vec4<f32>(lit / a, a), a > 0.0));
        return;
    }
    output[idx] = pack(vec4<f32>(screen3_at_ceiling(o.xyz, b), o.w));
}
