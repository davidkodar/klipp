//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> partial: array<f32>;
//#include planar_sample
var<workgroup> acc: array<f32, (THREADS * 6)>;
@compute @workgroup_size(THREADS)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    var s = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    var i = wg.x * THREADS + li;
    loop {
        if i >= p.n { break; }
        let o = i * POINT;
        let m = map_point(vec2<f32>(points[o], points[o + 1u]));
        if m.z > 0.0 {
            let b = sample(0u, m.x, m.y);
            if b.ok {
                let a = points[o + 2u];
                s[0] += 1.0; s[1] += a; s[2] += b.v; s[3] += a * a; s[4] += b.v * b.v; s[5] += a * b.v;
            }
        }
        i += GROUPS * THREADS;
    }
    for (var j = 0u; j < 6u; j++) { acc[li * 6u + j] = s[j]; }
    workgroupBarrier();
    var stride = THREADS / 2u;
    loop {
        if stride == 0u { break; }
        if li < stride { for (var j = 0u; j < 6u; j++) { acc[li * 6u + j] += acc[(li + stride) * 6u + j]; } }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if li == 0u { for (var j = 0u; j < 6u; j++) { partial[wg.x * 6u + j] = acc[j]; } }
}
