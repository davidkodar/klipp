
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.w || id.y >= p.h) { return; }
    let x = i32(id.x);
    let y = i32(id.y);
    var sx = 0.0;
    var sy = 0.0;
    var sw = 0.0;
    for (var oy = -1; oy <= 1; oy++) {
        for (var ox = -1; ox <= 1; ox++) {
            let v = raw_at(x + ox, y + oy);
            let weight = 1.0 / (v.z + 0.01);
            sx += v.x * weight;
            sy += v.y * weight;
            sw += weight;
        }
    }
    field[p.off + id.y * p.w + id.x] = vec4<f32>(sx / sw, sy / sw, raw_at(x, y).z, 0.0);
}
