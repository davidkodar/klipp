
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.w || id.y >= p.h) { return; }
    let x = f32(id.x);
    let y = f32(id.y);
    let v = fwd_at(i32(id.x), i32(id.y));
    let tx = x + v.x;
    let ty = y + v.y;
    let inside = tx >= 0.0 && ty >= 0.0 && tx <= f32(p.w - 1u) && ty <= f32(p.h - 1u);
    let f = fwd_motion(x, y);
    let b = bwd_motion(x + f.x, y + f.y);
    let dis = sqrt((f.x + b.x) * (f.x + b.x) + (f.y + b.y) * (f.y + b.y));
    if ((inside && dis > UNRELIABLE_DISAGREEMENT) || v.z > UNRELIABLE_COST) {
        atomicAdd(&count[0], 1u);
    }
}
