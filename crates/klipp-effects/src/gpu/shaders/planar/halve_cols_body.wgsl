//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// p.w × p.h is the source (already halved down); the out is ceil(p.w / 2) × p.h.
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w2 = (p.w + 1u) / 2u;
    if gid.x >= w2 || gid.y >= p.h { return; }
    var kk = array<f32, 5>(1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0);
    var sum = 0.0;
    for (var k = 0; k < 5; k++) {
        let x = clamp(2 * i32(gid.x) + k - 2, 0, i32(p.w) - 1);
        sum += kk[k] * src[gid.y * p.w + u32(x)];
    }
    out[gid.y * w2 + gid.x] = sum;
}
