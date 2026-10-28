
struct Params { width: u32, height: u32, opacity: f32, mode: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> other: array<vec4<f32>>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers
//#include blend_modes

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let base = unpack(input[idx]);
    let top = unpack(other[idx]);
    let op = clamp(params.opacity, 0.0, 1.0) * clamp(top.w, 0.0, 1.0);
    let b = blend3(params.mode, base.xyz, top.xyz);
    output[idx] = pack(vec4<f32>(base.xyz + (b - base.xyz) * op, base.w));
}
