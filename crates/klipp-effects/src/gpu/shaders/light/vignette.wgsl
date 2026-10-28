
struct Params { width: u32, height: u32, amount: f32, size: f32, softness: f32, roundness: f32, _pad: u32, _pad2: u32 }
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
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let nx = (x - cx) / cx;
    let ny = (y - cy) / cy;
    let rect_dist = max(abs(nx), abs(ny));
    let circ_dist = sqrt(nx * nx + ny * ny);
    let dist = rect_dist * (1.0 - params.roundness) + circ_dist * params.roundness;
    let falloff = clamp((dist - params.size) / max(params.softness, 0.01), 0.0, 1.0);
    let factor = 1.0 + params.amount * falloff;
    let px = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(px.xyz * factor, px.w));
}
