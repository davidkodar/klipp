
@group(0) @binding(1) var<storage, read> frame: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.w || id.y >= p.h) { return; }
    let y0 = id.y * p.src_h / p.h;
    let y1 = max((id.y + 1u) * p.src_h / p.h, y0 + 1u);
    let x0 = id.x * p.src_w / p.w;
    let x1 = max((id.x + 1u) * p.src_w / p.w, x0 + 1u);
    var sum = 0.0;
    for (var sy = y0; sy < y1; sy++) {
        for (var sx = x0; sx < x1; sx++) {
            sum += encode(frame[sy * p.src_w + sx]);
        }
    }
    out[id.y * p.w + id.x] = sum / f32((x1 - x0) * (y1 - y0));
}
