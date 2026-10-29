
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.w || id.y >= p.h) { return; }
    let x = i32(id.x) * 2;
    let y = i32(id.y) * 2;
    lum[p.off + id.y * p.w + id.x] = (up_at(x, y) + up_at(x + 1, y) + up_at(x, y + 1) + up_at(x + 1, y + 1)) * 0.25;
}
