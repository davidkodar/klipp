//#include pixel_storage
//#include layer_spill_head

@group(0) @binding(5) var<storage, read_write> args: array<u32, 12>; // not-pixels: the region's indirect dispatches
// `n` workgroups over x and, past the per-dimension limit, y, as
// `split_workgroups` splits a direct dispatch (#2341): the passes rebuild
// their index from both.
fn split(at: u32, n: u32) {
    var x = n;
    var y = 1u;
    if n > 65535u {
        y = (n + 65534u) / 65535u;
        x = (n + y - 1u) / y;
    }
    args[at] = x;
    args[at + 1u] = y;
    args[at + 2u] = 1u;
}
@compute @workgroup_size(1)
fn spill_region() {
    let b = rect();
    var r = vec4<i32>(0);
    if b.z > b.x && b.w > b.y {
        r = vec4<i32>(
            max(b.x - max(params.reach.x, 0), 0),
            max(b.y - max(params.reach.z, 0), 0),
            min(b.z + max(params.reach.y, 0), i32(params.width)),
            min(b.w + max(params.reach.w, 0), i32(params.height)),
        );
    }
    atomicStore(&region.x0, r.x);
    atomicStore(&region.y0, r.y);
    atomicStore(&region.x1, r.z);
    atomicStore(&region.y1, r.w);
    let n = u32((r.z - r.x) * (r.w - r.y));
    // A thread a pixel; a thread a row; a thread a column.
    split(0u, (n + 255u) / 256u);
    // A thread a 32-pixel stretch of a row, or of a column.
    let rw = u32(r.z - r.x);
    let rh = u32(r.w - r.y);
    split(3u, (rh * ((rw + 31u) / 32u) + 63u) / 64u);
    split(6u, (rw * ((rh + 31u) / 32u) + 63u) / 64u);
}
