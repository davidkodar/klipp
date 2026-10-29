//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> c0: array<f32>;
@group(0) @binding(3) var<storage, read> c1: array<f32>;
@group(0) @binding(4) var<storage, read_write> state: array<f32>;
@group(0) @binding(5) var<storage, read_write> costs: array<f32>;
//#include planar_sample
var<workgroup> acc: array<f32, (THREADS * 2)>;
@compute @workgroup_size(THREADS)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let side = u32(2 * p.radius + 1);
    let shift = vec2<f32>(f32(i32(wg.x % side) - p.radius), f32(i32(wg.x / side) - p.radius));
    var sum = 0.0;
    var n = 0.0;
    var j = li;
    loop {
        let i = j * p.step;
        if i >= p.n { break; }
        let o = i * POINT;
        let m = map_point(vec2<f32>(points[o], points[o + 1u]));
        if m.z > 0.0 {
            let v = sample(0u, m.x + shift.x, m.y + shift.y);
            if v.ok { sum += abs(v.v - points[o + 2u]); n += 1.0; }
        }
        j += THREADS;
    }
    acc[li * 2u] = sum;
    acc[li * 2u + 1u] = n;
    workgroupBarrier();
    var stride = THREADS / 2u;
    loop {
        if stride == 0u { break; }
        if li < stride { acc[li * 2u] += acc[(li + stride) * 2u]; acc[li * 2u + 1u] += acc[(li + stride) * 2u + 1u]; }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if li == 0u { costs[wg.x * 2u] = acc[0]; costs[wg.x * 2u + 1u] = acc[1]; }
}
