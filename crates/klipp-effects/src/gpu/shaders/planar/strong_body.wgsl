//#include planar_params
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<storage, read_write> strong: array<f32>;
@compute @workgroup_size(1)
fn main() {
    // The channels' count (the parameters read, as every pass's are
    // bound).
    let channels = 2u + min(p.n, 0u);
    for (var c = 0u; c < channels; c++) {
        var total = 0u;
        for (var b = 0u; b < BINS; b++) { total += atomicLoad(&hist[c * BINS + b]); }
        var seen = 0u;
        var at = 0u;
        for (var b = 0u; b < BINS; b++) {
            seen += atomicLoad(&hist[c * BINS + b]);
            if seen * 2u > total { at = b; break; }
        }
        // The bin's floor, so the median point itself counts as strong.
        strong[c] = select(exp2(f32(at) / 16.0 - 24.0), 0.0, at == 0u);
        for (var b = 0u; b < BINS; b++) { atomicStore(&hist[c * BINS + b], 0u); }
    }
}
