//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
//#include planar_solve
var<workgroup> sums: array<f32, LIGHT>;
@compute @workgroup_size(THREADS)
fn main(@builtin(local_invocation_index) li: u32) {
    if state[20] > 0.5 { return; }
    // Each sum over the groups by a thread of its own, then one solves.
    if li < LIGHT {
        var t = 0.0;
        for (var g = 0u; g < p.wgs; g++) { t += partial[g * LIGHT + li]; }
        sums[li] = t;
    }
    workgroupBarrier();
    if li != 0u { return; }
    var s: array<f32, LIGHT>;
    for (var j = 0u; j < LIGHT; j++) { s[j] = sums[j]; }
    // Brightness.
    var a: array<array<f32, 8>, 8>;
    var b: array<f32, 8>;
    for (var r = 0u; r < 8u; r++) { for (var c = 0u; c < 8u; c++) { a[r][c] = select(0.0, 1.0, r == c); } b[r] = 0.0; }
    var k = 0u;
    for (var r = 0u; r < 4u; r++) {
        for (var c = r; c < 4u; c++) { a[r][c] = s[k]; a[c][r] = s[k]; k++; }
        b[r] = s[10u + r];
    }
    let x = solve(a, b, 4u);
    if s[19] >= 16.0 && x.ok && x.v[0] >= 0.3 && x.v[0] <= 3.0 {
        state[9] = x.v[0]; state[10] = x.v[1]; state[11] = x.v[2]; state[12] = x.v[3];
    } else {
        state[9] = 1.0; state[10] = 0.0; state[11] = 0.0; state[12] = 0.0;
    }
    // Edges.
    for (var r = 0u; r < 8u; r++) { for (var c = 0u; c < 8u; c++) { a[r][c] = select(0.0, 1.0, r == c); } b[r] = 0.0; }
    a[0][0] = s[14]; a[0][1] = s[15]; a[1][0] = s[15]; a[1][1] = s[16];
    b[0] = s[17]; b[1] = s[18];
    let y = solve(a, b, 2u);
    if s[16] >= 16.0 && y.ok && y.v[0] >= 0.3 && y.v[0] <= 3.0 {
        state[13] = y.v[0]; state[14] = y.v[1];
    } else {
        state[13] = 1.0; state[14] = 0.0;
    }
}
