
fn sample_rgba(from_b: bool, xi: f32, yi: f32) -> vec4<f32> {
    let x = clamp(xi, 0.0, f32(p.src_w - 1u));
    let y = clamp(yi, 0.0, f32(p.src_h - 1u));
    let x0f = floor(x);
    let y0f = floor(y);
    let fx = x - x0f;
    let fy = y - y0f;
    let x0 = u32(x0f);
    let y0 = u32(y0f);
    let x1 = min(x0 + 1u, p.src_w - 1u);
    let y1 = min(y0 + 1u, p.src_h - 1u);
    var c00: vec4<f32>; var c10: vec4<f32>; var c01: vec4<f32>; var c11: vec4<f32>;
    if (from_b) {
        c00 = fb[y0 * p.src_w + x0]; c10 = fb[y0 * p.src_w + x1]; c01 = fb[y1 * p.src_w + x0]; c11 = fb[y1 * p.src_w + x1];
    } else {
        c00 = fa[y0 * p.src_w + x0]; c10 = fa[y0 * p.src_w + x1]; c01 = fa[y1 * p.src_w + x0]; c11 = fa[y1 * p.src_w + x1];
    }
    let top = c00 + (c10 - c00) * fx;
    let bottom = c01 + (c11 - c01) * fx;
    return top + (bottom - top) * fy;
}
fn inside(px: f32, py: f32) -> bool {
    return px >= -0.5 && py >= -0.5 && px <= f32(p.src_w) - 0.5 && py <= f32(p.src_h) - 0.5;
}
// `disagreement` with `forward` the forward field when `from_a`.
fn disagreement(from_a: bool, x: f32, y: f32) -> f32 {
    var f: vec2<f32>;
    var b: vec2<f32>;
    if (from_a) {
        f = fwd_motion(x, y);
        b = bwd_motion(x + f.x, y + f.y);
    } else {
        f = bwd_motion(x, y);
        b = fwd_motion(x + f.x, y + f.y);
    }
    return sqrt((f.x + b.x) * (f.x + b.x) + (f.y + b.y) * (f.y + b.y));
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.src_w || id.y >= p.src_h) { return; }
    let i = id.y * p.src_w + id.x;
    let t = clamp(p.t, 0.0, 1.0);
    let a0 = fa[i];
    let b0 = fb[i];
    let blend = a0 + (b0 - a0) * t;
    if (f32(count[0]) / f32(p.total) > p.frac) {
        out[i] = blend;
        return;
    }
    let kx = f32(p.w) / f32(p.src_w);
    let ky = f32(p.h) / f32(p.src_h);
    let x = f32(id.x);
    let y = f32(id.y);
    let qx = (x + 0.5) * kx - 0.5;
    let qy = (y + 0.5) * ky - 0.5;
    let f01 = fwd_motion(qx, qy);
    let f10 = bwd_motion(qx, qy);
    let s = 1.0 - t;
    let ax = x + (-s * t * f01.x + t * t * f10.x) / kx;
    let ay = y + (-s * t * f01.y + t * t * f10.y) / ky;
    let bx = x + (s * s * f01.x - t * s * f10.x) / kx;
    let by = y + (s * s * f01.y - t * s * f10.y) / ky;
    var ra = 0.0;
    if (inside(ax, ay)) {
        let e = disagreement(true, (ax + 0.5) * kx - 0.5, (ay + 0.5) * ky - 0.5) / CONSISTENCY_SCALE;
        ra = exp(-e * e);
    }
    var rb = 0.0;
    if (inside(bx, by)) {
        let e = disagreement(false, (bx + 0.5) * kx - 0.5, (by + 0.5) * ky - 0.5) / CONSISTENCY_SCALE;
        rb = exp(-e * e);
    }
    let ca = sample_rgba(false, ax, ay);
    let cb = sample_rgba(true, bx, by);
    let wa = s * ra;
    let wb = t * rb;
    if (wa + wb <= 1e-3) {
        out[i] = blend;
        return;
    }
    let d = abs(encode(ca) - encode(cb)) / PHOTOMETRIC_SCALE;
    let alike = exp(-d * d);
    let confidence = max(ra, rb) * (1.0 + (alike - 1.0) * min(ra, rb));
    let warped = (wa * ca + wb * cb) / (wa + wb);
    out[i] = blend + (warped - blend) * confidence;
}
