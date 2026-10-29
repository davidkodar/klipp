
// One sample into the chroma plane: Cb across, Cr up, in the measured
// space's own Y′CbCr — `Cb = (B′ − Y′) / 2(1 − Kb)`, `Cr = (R′ − Y′) /
// 2(1 − Kr)` — split by distance between the four cells around it.
fn bin_vector(e: vec3<f32>) {
    let y = dot(p.luma.xyz, e);
    if (y < p.lo || y > p.hi) { return; }
    let cb = (e.b - y) * 0.5 / (1.0 - p.luma.z);
    let cr = (e.r - y) * 0.5 / (1.0 - p.luma.x);
    let n = f32(p.cols - 1u);
    let x = clamp((cb + 0.5) * n, 0.0, n);
    let yy = clamp((0.5 - cr) * n, 0.0, n);
    let x0 = u32(floor(x)); let y0 = u32(floor(yy));
    let fx = x - f32(x0); let fy = yy - f32(y0);
    let x1 = min(x0 + 1u, p.cols - 1u); let y1 = min(y0 + 1u, p.cols - 1u);
    atomicAdd(&bins[y0 * p.cols + x0], u32((1.0 - fx) * (1.0 - fy) * 256.0 + 0.5));
    atomicAdd(&bins[y0 * p.cols + x1], u32(fx * (1.0 - fy) * 256.0 + 0.5));
    atomicAdd(&bins[y1 * p.cols + x0], u32((1.0 - fx) * fy * 256.0 + 0.5));
    atomicAdd(&bins[y1 * p.cols + x1], u32(fx * fy * 256.0 + 0.5));
}
