
struct HistParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> raw: array<f32>;
@group(0) @binding(1) var<storage, read_write> hist: array<f32>;
@group(0) @binding(2) var<uniform> p: HistParams;
const EDGE: u32 = 10u;
@compute @workgroup_size(1)
fn main() {
    let n = p.rows;
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        var inner = 0.0; var whole = 0.0;
        for (var l = 0u; l < n; l = l + 1u) {
            var s = raw[ch * n + l];
            for (var d = 1u; d < 4u; d = d + 1u) {
                let w = 1.0 - f32(d) * 0.25;
                if (l >= d) { s = s + raw[ch * n + l - d] * w; }
                if (l + d < n) { s = s + raw[ch * n + l + d] * w; }
            }
            hist[ch * n + l] = s;
            whole = max(whole, s);
            if (l >= EDGE && l + EDGE < n) { inner = max(inner, s); }
        }
        hist[4u * n + ch] = inner;
        hist[4u * n + 4u + ch] = whole;
    }
}
