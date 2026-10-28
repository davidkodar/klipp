
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    radius_sq: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var max_val: u32 = 0u;
    let r = i32(params.radius);
    let r_sq = f32(params.radius_sq);
    for (var dy = -r; dy <= r; dy++) {
        let ny = i32(y) + dy;
        if ny < 0 || ny >= i32(params.height) { continue; }
        for (var dx = -r; dx <= r; dx++) {
            let nx = i32(x) + dx;
            if nx < 0 || nx >= i32(params.width) { continue; }
            let dist_sq = f32(dx * dx + dy * dy);
            if dist_sq <= r_sq {
                let val = input[u32(ny) * params.width + u32(nx)];
                if val > max_val { max_val = val; }
            }
        }
    }
    output[idx] = max_val;
}
