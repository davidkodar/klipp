//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
//#include planar_solve
//#include planar_similarity
var<workgroup> sums: array<f32, NORMAL>;
@compute @workgroup_size(THREADS)
fn main(@builtin(local_invocation_index) li: u32) {
    if state[19] > 0.5 || state[20] > 0.5 { return; }
    // Each sum over the groups by a thread of its own, then one solves.
    if li < NORMAL {
        var t = 0.0;
        for (var g = 0u; g < p.wgs; g++) { t += partial[g * NORMAL + li]; }
        sums[li] = t;
    }
    workgroupBarrier();
    if li != 0u { return; }
    var s: array<f32, NORMAL>;
    for (var j = 0u; j < NORMAL; j++) { s[j] = sums[j]; }
    var a: array<array<f32, 8>, 8>;
    var b: array<f32, 8>;
    var k = 0u;
    for (var r = 0u; r < 8u; r++) {
        for (var c = r; c < 8u; c++) { a[r][c] = s[k]; a[c][r] = s[k]; k++; }
        b[r] = s[36u + r];
    }
    var d: array<f32, 8>;
    if state[21] > 0.5 {
        let r = to_similarity(a, b);
        let x = solve(r.a, r.b, 4u);
        if !x.ok { state[19] = 1.0; return; }
        d = from_similarity(x.v);
    } else {
        let x = solve(a, b, 8u);
        if !x.ok { state[19] = 1.0; return; }
        d = x.v;
    }
    // A step this small is the CPU's stop: the level is done.
    var moved = 0.0;
    for (var j = 0u; j < 8u; j++) { moved += d[j] * d[j]; }
    if moved < 1.0e-10 { state[20] = 1.0; }
    // The step as a map, inverted by its adjugate.
    let m = mat3x3<f32>(vec3<f32>(1.0 + d[0], d[3], d[6]), vec3<f32>(d[1], 1.0 + d[4], d[7]), vec3<f32>(d[2], d[5], 1.0));
    let det = determinant(m);
    if abs(det) < 1.0e-12 { state[19] = 1.0; return; }
    let inv = adjugate(m) * (1.0 / det);
    let g = mat3x3<f32>(vec3<f32>(state[0], state[3], state[6]), vec3<f32>(state[1], state[4], state[7]), vec3<f32>(state[2], state[5], state[8]));
    let n = g * inv;
    state[0] = n[0][0]; state[1] = n[1][0]; state[2] = n[2][0];
    state[3] = n[0][1]; state[4] = n[1][1]; state[5] = n[2][1];
    state[6] = n[0][2]; state[7] = n[1][2]; state[8] = n[2][2];
}
