
struct Solved { v: array<f32, 8>, ok: bool }

fn solve(a_in: array<array<f32, 8>, 8>, b_in: array<f32, 8>, n: u32) -> Solved {
    var a = a_in;
    var b = b_in;
    for (var col = 0u; col < n; col++) {
        var best = col;
        var big = abs(a[col][col]);
        for (var r = col + 1u; r < n; r++) {
            if abs(a[r][col]) > big { big = abs(a[r][col]); best = r; }
        }
        if big < 1.0e-12 { return Solved(b, false); }
        if best != col {
            let row = a[col]; a[col] = a[best]; a[best] = row;
            let t = b[col]; b[col] = b[best]; b[best] = t;
        }
        for (var r = col + 1u; r < n; r++) {
            let f = a[r][col] / a[col][col];
            for (var c = col; c < n; c++) { a[r][c] -= f * a[col][c]; }
            b[r] -= f * b[col];
        }
    }
    var x: array<f32, 8>;
    for (var j = 0u; j < 8u; j++) { x[j] = 0.0; }
    for (var k = 0u; k < n; k++) {
        let r = n - 1u - k;
        var sum = b[r];
        for (var c = r + 1u; c < n; c++) { sum -= a[r][c] * x[c]; }
        x[r] = sum / a[r][r];
    }
    return Solved(x, true);
}

fn adjugate(m: mat3x3<f32>) -> mat3x3<f32> {
    let a = m[0]; let b = m[1]; let c = m[2];
    return transpose(mat3x3<f32>(cross(b, c), cross(c, a), cross(a, b)));
}
