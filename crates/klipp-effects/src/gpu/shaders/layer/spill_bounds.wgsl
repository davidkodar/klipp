//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
var<workgroup> wx0: atomic<i32>;
var<workgroup> wy0: atomic<i32>;
var<workgroup> wx1: atomic<i32>;
var<workgroup> wy1: atomic<i32>;
@compute @workgroup_size(256)
fn spill_bounds(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    if li == 0u {
        atomicStore(&wx0, i32(params.width));
        atomicStore(&wy0, i32(params.height));
        atomicStore(&wx1, 0);
        atomicStore(&wy1, 0);
    }
    workgroupBarrier();
    let idx = gid.x;
    if idx < params.width * params.height && unpack(input[idx]).a > 0.0 {
        let x = i32(idx % params.width);
        let y = i32(idx / params.width);
        atomicMin(&wx0, x);
        atomicMin(&wy0, y);
        atomicMax(&wx1, x + 1);
        atomicMax(&wy1, y + 1);
    }
    workgroupBarrier();
    if li == 0u && atomicLoad(&wx1) > atomicLoad(&wx0) {
        atomicMin(&region.x0, atomicLoad(&wx0));
        atomicMin(&region.y0, atomicLoad(&wy0));
        atomicMax(&region.x1, atomicLoad(&wx1));
        atomicMax(&region.y1, atomicLoad(&wy1));
    }
}
