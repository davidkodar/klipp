
struct ExtParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read_write> sat: array<u32>;
@group(0) @binding(1) var<uniform> p: ExtParams;
@compute @workgroup_size(64, 4)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let r = gid.x; let ch = gid.y;
    if (r >= p.rows || ch >= 4u) { return; }
    let base = ch * p.cols * p.rows;
    for (var c = 1u; c < p.cols; c = c + 1u) {
        sat[base + c * p.rows + r] = sat[base + c * p.rows + r] + sat[base + (c - 1u) * p.rows + r];
    }
}
