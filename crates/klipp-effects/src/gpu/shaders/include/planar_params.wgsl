
struct P {
    w: u32,
    h: u32,
    n: u32,
    wgs: u32,
    // The region: its centre and half-size in the finest pixels, the
    // level's scale, and which level.
    cx: f32,
    cy: f32,
    s: f32,
    k: f32,
    // The coarse search's radius and the point step it reads.
    radius: i32,
    step: u32,
    shift_x: f32,
    shift_y: f32,
}
