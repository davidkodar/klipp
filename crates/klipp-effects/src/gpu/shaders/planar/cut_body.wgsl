//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read_write> state: array<f32>;
@compute @workgroup_size(1)
fn main() {
    // The fewest residuals a spread is read from (the parameters read,
    // as every pass's are bound).
    let fewest = 16u + min(p.n, 0u);
    if state[20] > 0.5 { return; }
    var total = 0u;
    for (var b = 0u; b < BINS; b++) { total += atomicLoad(&hist[b]); }
    if total < fewest { state[19] = 1.0; }
    var seen = 0u;
    var at = 0u;
    for (var b = 0u; b < BINS; b++) {
        seen += atomicLoad(&hist[b]);
        if seen * 2u > total { at = b; break; }
    }
    let mid = (f32(at) + 0.5) / 1024.0;
    let median = mid * mid * 2.0;
    state[17] = 4.685 * max(median * 1.4826, 0.004);
    for (var b = 0u; b < BINS; b++) { atomicStore(&hist[b], 0u); }
}
