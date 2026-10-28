
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;

var<workgroup> local: array<atomic<u32>, 256>;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    atomicStore(&local[li], 0u);
    workgroupBarrier();
    let idx = gid.x;
    if idx < params.width * params.height {
        let c = unpack(input[idx]);
        let l = params.luma.x * linear_to_srgb_component(c.x)
            + params.luma.y * linear_to_srgb_component(c.y)
            + params.luma.z * linear_to_srgb_component(c.z);
        // A NaN counts in bin 0, as the CPU's cast puts it.
        var bin = 0u;
        if l == l {
            bin = min(u32(clamp(l, 0.0, 1.0) * 255.0), 255u);
        }
        atomicAdd(&local[bin], 1u);
    }
    workgroupBarrier();
    let n = atomicLoad(&local[li]);
    if n > 0u {
        atomicAdd(&hist[li], n);
    }
}
