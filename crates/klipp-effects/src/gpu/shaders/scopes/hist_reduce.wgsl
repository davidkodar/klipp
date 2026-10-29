
struct HistParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> raw: array<f32>;
@group(0) @binding(2) var<uniform> p: HistParams;
@compute @workgroup_size(64, 4)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let level = gid.x; let ch = gid.y;
    if (level >= p.rows || ch >= 4u) { return; }
    let base = ch * p.cols * p.rows;
    var sum = 0.0;
    for (var c = 0u; c < p.cols; c = c + 1u) {
        sum = sum + f32(bins[base + c * p.rows + level]);
    }
    raw[ch * p.rows + level] = sum;
}
