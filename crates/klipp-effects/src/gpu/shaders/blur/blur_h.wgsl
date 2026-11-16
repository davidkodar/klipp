
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
//#include srgb_components
//#include pixel_storage
//#include linear_helpers

// The widest box summed from the workgroup's tile, which holds
// 256 + 2 × TILE_R pixels; past it the window is slid.
const TILE_R: i32 = 64;
// The workgroup's 256 pixels and TILE_R either side, in the frame's
// flat order.
var<workgroup> tile: array<vec4<f32>, 384>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let w_i32 = i32(params.width);

    if params.radius <= TILE_R {
        // Each pixel was read from the frame at every tap, 2r + 1 reads a
        // pixel, which on the Apple GPU (Mesa) made the op's default blur
        // 31 ms at 1080p. The workgroup reads its stretch of the frame
        // and r either side once, then sums from that. A workgroup may
        // run over the end of a row; a tap clamped to its own row still
        // lies within r of its pixel, so inside the tile.
        let r = params.radius;
        let n = i32(params.width * params.height);
        let start = i32(idx - lid) - r;
        for (var i = i32(lid); i < 256 + 2 * r; i += 256) {
            tile[i] = unpack(input[clamp(start + i, 0, n - 1)]);
        }
        workgroupBarrier();
        let y = idx / params.width;
        if y >= params.height { return; }
        let x = i32(idx % params.width);
        let row = i32(y * params.width) - start;
        for (var dx = -r; dx <= r; dx++) {
            sum += tile[row + clamp(x + dx, 0, w_i32 - 1)];
        }
        output[idx] = pack(sum * inv);
        return;
    }
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }
    // A wider window is slid along a stretch of the row at least as long
    // as itself, by the invocation at the stretch's start; the rest
    // return (#3176). Summed afresh at every pixel, a light wrap's widest
    // box (a sigma of 3200) ran past the GPU's job time limit.
    // `cpu::slide_box` is the same sum.
    let r = params.radius;
    let len = min(w_i32, max(32, diam));
    let x0 = i32(x);
    if x0 % len != 0 { return; }
    let last = w_i32 - 1;
    sum = f32(max(0, r - x0)) * at(y, 0) + f32(max(0, x0 + r - last)) * at(y, last);
    for (var k = max(x0 - r, 0); k <= min(x0 + r, last); k++) {
        sum += at(y, k);
    }
    for (var sx = x0; sx < min(x0 + len, w_i32); sx++) {
        output[y * params.width + u32(sx)] = pack(sum * inv);
        sum += at(y, min(sx + r + 1, last)) - at(y, max(sx - r, 0));
    }
}

// The row's pixel `x`, which the caller keeps on the row.
fn at(y: u32, x: i32) -> vec4<f32> {
    return unpack(input[y * params.width + u32(x)]);
}
