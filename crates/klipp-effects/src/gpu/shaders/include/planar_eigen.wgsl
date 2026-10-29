
fn conditioning(a_in: array<array<f32, 8>, 8>, n: u32) -> f32 {
    var a = a_in;
    for (var sweep = 0; sweep < 30; sweep++) {
        var off = 0.0;
        for (var i = 0u; i < n; i++) { for (var j = 0u; j < n; j++) { if i != j { off += a[i][j] * a[i][j]; } } }
        if off < 1.0e-20 { break; }
        for (var pp = 0u; pp < n; pp++) {
            for (var q = pp + 1u; q < n; q++) {
                if abs(a[pp][q]) < 1.0e-30 { continue; }
                let theta = (a[q][q] - a[pp][pp]) / (2.0 * a[pp][q]);
                var t = sign(theta) / (abs(theta) + sqrt(theta * theta + 1.0));
                if theta == 0.0 { t = 1.0; }
                let c = 1.0 / sqrt(t * t + 1.0);
                let s = t * c;
                for (var k = 0u; k < n; k++) {
                    let akp = a[k][pp]; let akq = a[k][q];
                    a[k][pp] = c * akp - s * akq; a[k][q] = s * akp + c * akq;
                }
                for (var k = 0u; k < n; k++) {
                    let apk = a[pp][k]; let aqk = a[q][k];
                    a[pp][k] = c * apk - s * aqk; a[q][k] = s * apk + c * aqk;
                }
            }
        }
    }
    var lo = 3.0e38;
    var hi = 0.0;
    for (var i = 0u; i < n; i++) { lo = min(lo, a[i][i]); hi = max(hi, a[i][i]); }
    if hi <= 0.0 { return 0.0; }
    return max(lo / hi, 0.0);
}
