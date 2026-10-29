//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> points: array<f32>;
@group(0) @binding(2) var<storage, read> seen: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> trust: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read_write> partial: array<f32>;
@group(0) @binding(5) var<storage, read> state: array<f32>;
var<workgroup> acc: array<f32, (THREADS * LIGHT)>;
@compute @workgroup_size(THREADS)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    // Converged: the rest of the level's steps do nothing.
    if state[20] > 0.5 { return; }
    var s: array<f32, LIGHT>;
    for (var j = 0u; j < LIGHT; j++) { s[j] = 0.0; }
    var i = wg.x * THREADS + li;
    loop {
        if i >= p.n { break; }
        let v = seen[i];
        if v.z > 0.5 {
            let o = i * POINT;
            let u = vec2<f32>(points[o], points[o + 1u]);
            let t = trust[i];
            // Brightness: rows [T, 1, u, v].
            var r0 = array<f32, 4>(points[o + 2u], 1.0, u.x, u.y);
            var k = 0u;
            for (var a = 0u; a < 4u; a++) {
                for (var b = a; b < 4u; b++) { s[k] += t.x * r0[a] * r0[b]; k++; }
            }
            for (var a = 0u; a < 4u; a++) { s[10u + a] += t.x * r0[a] * v.x; }
            // Edges: rows [T, 1].
            let e = points[o + 3u];
            s[14] += t.y * e * e;
            s[15] += t.y * e;
            s[16] += t.y;
            s[17] += t.y * e * v.y;
            s[18] += t.y * v.y;
            s[19] += t.x;
        }
        i += GROUPS * THREADS;
    }
    for (var j = 0u; j < LIGHT; j++) { acc[li * LIGHT + j] = s[j]; }
    workgroupBarrier();
    var stride = THREADS / 2u;
    loop {
        if stride == 0u { break; }
        if li < stride {
            for (var j = 0u; j < LIGHT; j++) { acc[li * LIGHT + j] += acc[(li + stride) * LIGHT + j]; }
        }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if li == 0u {
        for (var j = 0u; j < LIGHT; j++) { partial[wg.x * LIGHT + j] = acc[j]; }
    }
}
