
struct ExtParams { cols: u32, rows: u32, _p0: u32, _p1: u32 };
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> ext: array<u32>;
@group(0) @binding(2) var<uniform> p: ExtParams;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = gid.x; let ch = gid.y;
    if (col >= p.cols || ch >= 4u) { return; }
    let base = ch * p.cols * p.rows + col * p.rows;
    var lo = 0xffffffffu; var hi = 0u; var any = false;
    for (var r = 0u; r < p.rows; r = r + 1u) {
        if (bins[base + r] != 0u) {
            if (!any) { lo = r; any = true; }
            hi = r;
        }
    }
    ext[(ch * p.cols + col) * 2u] = lo;
    ext[(ch * p.cols + col) * 2u + 1u] = select(0xffffffffu, hi, any);
}
