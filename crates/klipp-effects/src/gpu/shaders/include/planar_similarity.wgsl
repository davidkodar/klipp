
fn basis(a: u32, i: u32) -> f32 {
    if a == 0u || a == 4u { return select(0.0, 1.0, i == 0u); }
    if a == 1u { return select(0.0, -1.0, i == 1u); }
    if a == 3u { return select(0.0, 1.0, i == 1u); }
    if a == 2u { return select(0.0, 1.0, i == 2u); }
    if a == 5u { return select(0.0, 1.0, i == 3u); }
    return 0.0;
}

struct Reduced { a: array<array<f32, 8>, 8>, b: array<f32, 8> }

fn to_similarity(h_in: array<array<f32, 8>, 8>, b_in: array<f32, 8>) -> Reduced {
    var h = h_in;
    var b = b_in;
    var r: Reduced;
    for (var i = 0u; i < 4u; i++) {
        for (var a = 0u; a < 8u; a++) {
            r.b[i] += basis(a, i) * b[a];
            for (var j = 0u; j < 4u; j++) {
                for (var c = 0u; c < 8u; c++) { r.a[i][j] += basis(a, i) * h[a][c] * basis(c, j); }
            }
        }
    }
    return r;
}

fn from_similarity(q_in: array<f32, 8>) -> array<f32, 8> {
    var q = q_in;
    var d: array<f32, 8>;
    for (var a = 0u; a < 8u; a++) {
        for (var i = 0u; i < 4u; i++) { d[a] += basis(a, i) * q[i]; }
    }
    return d;
}
