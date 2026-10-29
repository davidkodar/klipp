
struct Params {
    width: u32, height: u32, radius: i32, mode: u32,
    offset_x: i32, offset_y: i32, opacity: f32, pad: u32,
    reach: vec4<i32>,
    color: vec4<f32>,
}
struct Region { x0: atomic<i32>, y0: atomic<i32>, x1: atomic<i32>, y1: atomic<i32> }
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var<storage, read_write> region: Region; // not-pixels: the region spills work in
fn rect() -> vec4<i32> {
    return vec4<i32>(atomicLoad(&region.x0), atomicLoad(&region.y0), atomicLoad(&region.x1), atomicLoad(&region.y1));
}
// The region `r`'s `i`th pixel, or (-1, -1) past its end. The region is
// read once a pass's invocation, into `r`: atomics are not cached.
fn region_pixel(r: vec4<i32>, i: u32) -> vec2<i32> {
    let w = r.z - r.x;
    if w <= 0 || i32(i) >= w * (r.w - r.y) { return vec2<i32>(-1, -1); }
    return vec2<i32>(r.x + i32(i) % w, r.y + i32(i) / w);
}
fn inside(r: vec4<i32>, x: i32, y: i32) -> bool {
    return x >= r.x && x < r.z && y >= r.y && y < r.w;
}
// The fixed-point step of a box blur's window: as fine as a window of
// `2 * radius + 1` alphas of at most 1 can hold in a u32 without overflow,
// with room for the float step's own rounding (2^24 to spare).
fn fixed_scale() -> f32 {
    return f32(0xFF000000u / u32(2 * params.radius + 1));
}
