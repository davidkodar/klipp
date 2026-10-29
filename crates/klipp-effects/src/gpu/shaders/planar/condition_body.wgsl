//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> partial: array<f32>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
//#include planar_eigen
//#include planar_similarity
@compute @workgroup_size(1)
fn main() {
    var s: array<f32, NORMAL>;
    for (var j = 0u; j < NORMAL; j++) { s[j] = 0.0; }
    for (var g = 0u; g < p.wgs; g++) {
        for (var j = 0u; j < NORMAL; j++) { s[j] += partial[g * NORMAL + j]; }
    }
    var a: array<array<f32, 8>, 8>;
    var k = 0u;
    for (var r = 0u; r < 8u; r++) {
        for (var c = r; c < 8u; c++) { a[r][c] = s[k]; a[c][r] = s[k]; k++; }
    }
    if state[21] > 0.5 {
        var b: array<f32, 8>;
        state[18] = conditioning(to_similarity(a, b).a, 4u);
    } else {
        state[18] = conditioning(a, 8u);
    }
}
