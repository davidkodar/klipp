
struct MaskRecord {
    shape_type: u32,
    flags: u32,
    bezier_offset: u32,
    bezier_count: u32,
    cx: f32, cy: f32, p1: f32, p2: f32,
    p3: f32, feather: f32, feather_inner: f32, _pad1: u32,
    w0: vec4<f32>, w1: vec4<f32>, w2: vec4<f32>,
}


// As klipp_core::soft_edge: covered inside `inner` past the line, falling
// off to `outer` outside it.
fn soft_edge_sdf(dist: f32, inner_in: f32, outer_in: f32) -> f32 {
    let inner = max(inner_in, 0.0);
    let outer = max(outer_in, 0.0);
    let span = inner + outer;
    if dist <= -inner { return 1.0; }
    if span > 0.0001 && dist < outer {
        let t = (dist + inner) / span;
        return 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return 0.0;
}

// Every record joined in order, as klipp_core::MaskCombine::apply: the
// matte starts empty, or whole when the first record doesn't add (#74).
fn window_matte(nx: f32, ny: f32, aspect: f32, count: u32) -> f32 {
    var matte: f32 = 0.0;
    if count > 0u && ((records[0].flags >> 2u) & 3u) != 0u { matte = 1.0; }
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let m = eval_record(records[i], nx, ny, aspect);
        let c = (records[i].flags >> 2u) & 3u;
        if c == 1u {
            matte = min(matte, 1.0 - m);
        } else if c == 2u {
            matte = min(matte, m);
        } else {
            matte = max(matte, m);
        }
    }
    return matte;
}

// Signed-distance to a closed polygon using the same winding-
// number + min edge distance algorithm as the CPU rasteriser at
// klipp-color/src/masks.rs:80-115. Returns negative inside,
// positive outside, in normalised width-relative units.
fn bezier_sdf(nx: f32, ny: f32, aspect: f32, offset: u32, count: u32) -> f32 {
    if count < 3u { return 1.0; }
    let px = nx;
    let py = ny * aspect;
    var min_dist_sq: f32 = 1.0e30;
    var winding: i32 = 0;
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let a_raw = points[offset + i];
        let b_raw = points[offset + ((i + 1u) % count)];
        let ax = a_raw.x;
        let ay = a_raw.y * aspect;
        let bx = b_raw.x;
        let by = b_raw.y * aspect;
        let ex = bx - ax;
        let ey = by - ay;
        let dx = px - ax;
        let dy = py - ay;
        let denom = ex * ex + ey * ey;
        var t: f32 = 0.0;
        if denom > 0.0 {
            t = clamp((dx * ex + dy * ey) / denom, 0.0, 1.0);
        }
        let cx = ax + t * ex - px;
        let cy = ay + t * ey - py;
        let d2 = cx * cx + cy * cy;
        if d2 < min_dist_sq { min_dist_sq = d2; }
        // Winding-number contribution. CPU uses the classic
        // ray-casting algorithm — match the branches exactly so
        // the parity test stays clean.
        if ay <= py {
            if by > py {
                let cross = ex * (py - ay) - ey * (px - ax);
                if cross > 0.0 { winding = winding + 1; }
            }
        } else {
            if by <= py {
                let cross = ex * (py - ay) - ey * (px - ax);
                if cross < 0.0 { winding = winding - 1; }
            }
        }
    }
    let dist = sqrt(min_dist_sq);
    if winding != 0 { return -dist; }
    return dist;
}

fn eval_record(rec: MaskRecord, px: f32, py: f32, aspect: f32) -> f32 {
    // A window tracked in perspective is measured where the pixel was
    // on the frame it was drawn on; past the horizon is outside. As the
    // CPU rasteriser's PlaneMap::apply.
    var nx = px;
    var ny = py;
    var behind = false;
    if (rec.flags & 2u) != 0u {
        let h = vec3<f32>(px, py, 1.0);
        let w = dot(rec.w2.xyz, h);
        if !(w > 1.0e-9) {
            behind = true;
        } else {
            nx = dot(rec.w0.xyz, h) / w;
            ny = dot(rec.w1.xyz, h) / w;
        }
    }
    var dist: f32;
    if behind { dist = 3.0e38; } else {
    switch rec.shape_type {
        case 0u: {
            // circle
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) * aspect;
            dist = sqrt(dx * dx + dy * dy) - rec.p1;
        }
        case 1u: {
            // rectangle
            let rad = rec.p3 * 3.14159265 / 180.0;
            let cos_r = cos(rad);
            let sin_r = sin(rad);
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) * aspect;
            let lx = abs(dx * cos_r + dy * sin_r);
            let ly = abs(-dx * sin_r + dy * cos_r);
            let hw = rec.p1 / 2.0;
            let hh = rec.p2 / 2.0;
            let ox = lx - hw;
            let oy = ly - hh;
            let outside = sqrt(max(ox, 0.0) * max(ox, 0.0) + max(oy, 0.0) * max(oy, 0.0));
            let inside = min(max(ox, oy), 0.0);
            dist = outside + inside;
        }
        case 2u: {
            // linear gradient
            let gx = rec.p1 - rec.cx;
            let gy = (rec.p2 - rec.cy) * aspect;
            let glen2 = gx * gx + gy * gy;
            if glen2 < 1.0e-8 {
                dist = 0.0;
            } else {
                let dx = nx - rec.cx;
                let dy = (ny - rec.cy) * aspect;
                let t = (dx * gx + dy * gy) / glen2;
                dist = t - 0.5;
            }
        }
        case 3u: {
            // bezier polygon
            dist = bezier_sdf(nx, ny, aspect, rec.bezier_offset, rec.bezier_count);
        }
        case 4u: {
            // radial gradient: in gradient widths from the ring midway
            // between inner (p1) and outer (p2)
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) * aspect;
            let r = sqrt(dx * dx + dy * dy);
            let span = rec.p2 - rec.p1;
            let mid = (rec.p1 + rec.p2) * 0.5;
            if abs(span) < 0.0001 { dist = r - mid; } else { dist = (r - mid) / span; }
        }
        default: { dist = 1.0; }
    }
    }
    var m = soft_edge_sdf(dist, rec.feather_inner, rec.feather);
    if (rec.flags & 1u) != 0u { m = 1.0 - m; }
    let opacity = f32((rec.flags >> 8u) & 0xFFu) / 255.0;
    return m * opacity;
}

