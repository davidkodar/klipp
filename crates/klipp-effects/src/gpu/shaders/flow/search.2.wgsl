
fn cost(x: i32, y: i32, pred: vec2<f32>, dx: i32, dy: i32) -> f32 {
    let mx = pred.x + f32(dx);
    let my = pred.y + f32(dy);
    var sum = 0.0;
    for (var oy = -PATCH_RADIUS; oy <= PATCH_RADIUS; oy++) {
        for (var ox = -PATCH_RADIUS; ox <= PATCH_RADIUS; ox++) {
            sum += abs(from_at(x + ox, y + oy) - to_sample(f32(x + ox) + mx, f32(y + oy) + my));
        }
    }
    let n = (2 * PATCH_RADIUS + 1) * (2 * PATCH_RADIUS + 1);
    return sum / f32(n) + MOTION_PENALTY * f32(abs(dx) + abs(dy));
}
fn parabola(minus: f32, centre: f32, plus: f32) -> f32 {
    let denom = minus - 2.0 * centre + plus;
    if (denom > 1e-6) { return clamp((minus - plus) / (2.0 * denom), -0.5, 0.5); }
    return 0.0;
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.w || id.y >= p.h) { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    var pred = vec2<f32>(0.0, 0.0);
    if (p.cw > 0u) {
        let m = coarse_motion((f32(x) + 0.5) * 0.5 - 0.5, (f32(y) + 0.5) * 0.5 - 0.5);
        pred = vec2<f32>(m.x * 2.0, m.y * 2.0);
    }
    var bx = 0;
    var by = 0;
    var bc = 1e30;
    for (var dy = -SEARCH_RADIUS; dy <= SEARCH_RADIUS; dy++) {
        for (var dx = -SEARCH_RADIUS; dx <= SEARCH_RADIUS; dx++) {
            let c = cost(x, y, pred, dx, dy);
            if (c < bc) { bx = dx; by = dy; bc = c; }
        }
    }
    let sx = parabola(cost(x, y, pred, bx - 1, by), bc, cost(x, y, pred, bx + 1, by));
    let sy = parabola(cost(x, y, pred, bx, by - 1), bc, cost(x, y, pred, bx, by + 1));
    raw[id.y * p.w + id.x] = vec4<f32>(pred.x + f32(bx) + sx, pred.y + f32(by) + sy, bc, 0.0);
}
