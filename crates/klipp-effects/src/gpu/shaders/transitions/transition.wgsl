//#include pixel_storage
//#include warp_functions

struct Params {
    width: u32,
    height: u32,
    progress: f32,
    kind_id: u32,
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
}

@group(0) @binding(0) var<storage, read> from_buf: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> to_buf: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

// Storage is native linear-f32 vec4, but the transition body below
// was written against 0-255-range floats (linear or otherwise). To
// keep the large body unchanged, the local `unpack` scales linear
// [0,1] → 0-255, and `pack` divides by 255 back to linear. Alpha
// stays in 0-255 internally the same way (max alpha = 255.0 ⇒ 1.0).
// 0-1 <-> 0-255, this shader's own convention. Named away from
// pack/unpack, which mean how a pixel is *stored*.
fn to_bytes(v: vec4<f32>) -> vec4<f32> { return v * 255.0; }
fn from_bytes(v: vec4<f32>) -> vec4<f32> { return v / 255.0; }

fn lerp_pixel(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    return a * (1.0 - t) + b * t;
}

// Noise for the grain, static and glitch kinds: an integer hash of
// integer inputs (pixel or block coordinates, progress's bits), so the
// pattern is the same on every GPU and on the CPU. A `fract(sin(…))`
// hash depended on each GPU's approximation of `sin` at large arguments.
// `hash_u` and `rand3` are the shared warps' (`crate::warp`).

// Smooth value noise for the film burn's edge: `rand3` at the corners of
// a unit grid, blended with a smoothstep. Integer-hashed, so the same on
// every GPU and on the CPU.
fn burn_noise(px: f32, py: f32) -> f32 {
    let ix = floor(px);
    let iy = floor(py);
    let tx = px - ix;
    let ty = py - iy;
    let ux = tx * tx * (3.0 - 2.0 * tx);
    let uy = ty * ty * (3.0 - 2.0 * ty);
    let x0 = u32(ix);
    let y0 = u32(iy);
    let a = rand3(x0, y0, 7u);
    let b = rand3(x0 + 1u, y0, 7u);
    let c = rand3(x0, y0 + 1u, 7u);
    let d = rand3(x0 + 1u, y0 + 1u, 7u);
    return mix(mix(a, b, ux), mix(c, d, ux), uy);
}

// Bilinear samples for the kinds that move pixels by fractional amounts
// (spin, wave, zoom): smooth motion, where nearest-pixel sampling
// stair-stepped — and picked a different pixel on each GPU wherever a
// coordinate fell on a boundary. Clamped to the frame; unpacked.
fn bilinear(buf_is_to: bool, sx: f32, sy: f32) -> vec4<f32> {
    let fx = clamp(sx, 0.0, f32(params.width) - 1.0);
    let fy = clamp(sy, 0.0, f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    var c00: vec4<f32>;
    var c10: vec4<f32>;
    var c01: vec4<f32>;
    var c11: vec4<f32>;
    if buf_is_to {
        c00 = unpack(to_buf[y0 * params.width + x0]);
        c10 = unpack(to_buf[y0 * params.width + x1]);
        c01 = unpack(to_buf[y1 * params.width + x0]);
        c11 = unpack(to_buf[y1 * params.width + x1]);
    } else {
        c00 = unpack(from_buf[y0 * params.width + x0]);
        c10 = unpack(from_buf[y0 * params.width + x1]);
        c01 = unpack(from_buf[y1 * params.width + x0]);
        c11 = unpack(from_buf[y1 * params.width + x1]);
    }
    return to_bytes(mix(mix(c00, c10, tx), mix(c01, c11, tx), ty));
}

// Piece transitions (#985): the settings `PieceParams` packs into param3.
// Bits 0-2 the order, 3-4 the easing, 5 assemble, 6 alternate, 7-11 the
// gap in pixels.
fn piece_bits() -> u32 {
    return u32(max(params.param3, 0.0) + 0.5);
}

// A piece's place in the order, 0 moving first and 1 last: `along` is
// its place along the move (0 nearest the way out), `centre` its
// distance from the middle (0 to 1), `id` what shuffles it.
fn piece_key(id: u32, along: f32, centre: f32) -> f32 {
    let order = piece_bits() & 7u;
    if order == 1u {
        return 1.0 - along;
    } else if order == 2u {
        return centre;
    } else if order == 3u {
        return 1.0 - centre;
    } else if order == 4u {
        return rand3(id, 985u, 3u);
    } else if order == 5u {
        return 0.0;
    }
    return along;
}

// How far a piece with key `k` has gone, eased: 0 at rest, 1 gone. Each
// piece's start is put off by the stagger times its key.
fn piece_travel(k: f32) -> f32 {
    let s = clamp(params.param2, 0.0, 0.9);
    let t = clamp((params.progress - s * k) / (1.0 - s), 0.0, 1.0);
    let ease = (piece_bits() >> 3u) & 3u;
    if ease == 1u {
        return t;
    } else if ease == 2u {
        return t * t;
    } else if ease == 3u {
        let u = t - 1.0;
        return 1.0 + 2.70158 * u * u * u + 1.70158 * u * u;
    }
    return t * t * (3.0 - 2.0 * t);
}

// Half the gap open around a piece that has gone `e`: none at rest or
// gone, all of it on the way.
fn piece_gap(e: f32) -> f32 {
    return f32((piece_bits() >> 7u) & 31u) * 0.5 * clamp(8.0 * e * (1.0 - e), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let y = idx / params.width;
    let x = idx % params.width;
    let p = params.progress;

    let from_px = to_bytes(unpack(from_buf[idx]));
    let to_px = to_bytes(unpack(to_buf[idx]));

    var result: vec4<f32>;

    if params.kind_id == 0u {
        // Cross Dissolve
        result = lerp_pixel(from_px, to_px, p);
    } else if params.kind_id == 1u {
        // Wipe Left (reveals to-frame from left to right)
        // param0 = feather radius in pixels (0 = hard edge)
        // threshold expanded by ±feather so endpoints fully clear/cover the frame
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.width) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 2u {
        // Wipe Right (reveals to-frame from right to left)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.width) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 3u {
        // Wipe Up (reveals to-frame from top to bottom)
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.height) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 4u {
        // Wipe Down (reveals to-frame from bottom to top)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.height) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 5u || params.kind_id == 6u {
        // Dip to colour. param0 = hold duration (0..0.45) at the midpoint.
        // colour = dip colour (default black for kind 5, white for kind 6).
        // `params.color_*` are already in 0-1 sRGB — decode to linear
        // so the dip crossfade works in physical light. Scale to the
        // 0-255 local convention this shader body uses.
        let dip_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let hold = clamp(params.param0, 0.0, 0.45);
        let mid_start = 0.5 - hold * 0.5;
        let mid_end = 0.5 + hold * 0.5;
        if p <= mid_start {
            let t = p / max(mid_start, 0.001);
            result = lerp_pixel(from_px, dip_color, t);
        } else if p >= mid_end {
            let t = (p - mid_end) / max(1.0 - mid_end, 0.001);
            result = lerp_pixel(dip_color, to_px, t);
        } else {
            result = dip_color;
        }
    } else if params.kind_id == 7u {
        // Push: outgoing slides off, incoming slides on. Hard slide, no blend.
        // param0 = direction (0=Left, 1=Right, 2=Up, 3=Down)
        let dir = u32(params.param0 + 0.5);
        if dir == 1u {
            // Push Right: outgoing slides right, incoming enters from left
            let offset = i32(p * f32(params.width));
            let src_x = i32(x) - offset;
            if src_x >= 0 {
                result = to_bytes(unpack(from_buf[y * params.width + u32(src_x)]));
            } else {
                let in_x = u32(src_x + i32(params.width));
                result = to_bytes(unpack(to_buf[y * params.width + in_x]));
            }
        } else if dir == 2u {
            // Push Up: outgoing slides up, incoming enters from bottom
            let offset = u32(p * f32(params.height));
            let src_y = y + offset;
            if src_y < params.height {
                result = to_bytes(unpack(from_buf[src_y * params.width + x]));
            } else {
                let in_y = src_y - params.height;
                result = to_bytes(unpack(to_buf[in_y * params.width + x]));
            }
        } else if dir == 3u {
            // Push Down: outgoing slides down, incoming enters from top
            let offset = i32(p * f32(params.height));
            let src_y = i32(y) - offset;
            if src_y >= 0 {
                result = to_bytes(unpack(from_buf[u32(src_y) * params.width + x]));
            } else {
                let in_y = u32(src_y + i32(params.height));
                result = to_bytes(unpack(to_buf[in_y * params.width + x]));
            }
        } else {
            // Push Left (default): outgoing slides left, incoming enters from right
            let offset = u32(p * f32(params.width));
            let src_x = x + offset;
            if src_x < params.width {
                result = to_bytes(unpack(from_buf[y * params.width + src_x]));
            } else {
                let in_x = src_x - params.width;
                result = to_bytes(unpack(to_buf[y * params.width + in_x]));
            }
        }
    } else if params.kind_id == 8u {
        // Film Dissolve: square-root blending that emulates the
        // density-stacking behaviour of film emulsions. In the
        // linear pipeline `from_px` / `to_px` are already linear,
        // so we drop the /255 and *255 scaling — the sqrt curve
        // still produces the characteristic over-bright midpoint
        // that distinguishes film dissolve from cross dissolve.
        let blended = sqrt(from_px * from_px * (1.0 - p) + to_px * to_px * p);
        result = blended;
        result.w = from_px.w * (1.0 - p) + to_px.w * p;
    } else if params.kind_id == 9u {
        // Blur Dissolve: dissolve + horizontal box blur peaking at midpoint
        // param0 = max blur radius in pixels
        let max_radius = max(params.param0, 1.0);
        let blur_r = i32(p * (1.0 - p) * 4.0 * max_radius);
        var acc = vec4<f32>(0.0);
        var count = 0.0;
        let ix = i32(x);
        let step_sz = max(blur_r / 4, 1);
        for (var dx = -blur_r; dx <= blur_r; dx += step_sz) {
            let sx = clamp(ix + dx, 0, i32(params.width) - 1);
            let si = y * params.width + u32(sx);
            let fpx = to_bytes(unpack(from_buf[si]));
            let tpx = to_bytes(unpack(to_buf[si]));
            acc += lerp_pixel(fpx, tpx, p);
            count += 1.0;
        }
        if count > 0.0 {
            result = acc / count;
        } else {
            result = lerp_pixel(from_px, to_px, p);
        }
    } else if params.kind_id == 10u {
        // Flash: bright colour flash with parameterized peak.
        // param0 = intensity, param1 = peak position, color = flash colour.
        // Decode user-supplied sRGB colour to linear, scaled to the
        // 0-255 local convention this shader body uses.
        let flash_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let intensity = clamp(params.param0, 0.0, 2.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = clamp(flash_t * intensity, 0.0, 1.0);
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp((p - dissolve_start) / max(1.0 - dissolve_start, 0.001), 0.0, 1.0);
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        // No 255 clamp — HDR flash may legitimately exceed 1.0 linear;
        // the display boundary clamps at the u8 encode step.
        result = vec4<f32>(
            base.x * (1.0 - flash_t) + flash_color.x * flash_t,
            base.y * (1.0 - flash_t) + flash_color.y * flash_t,
            base.z * (1.0 - flash_t) + flash_color.z * flash_t,
            base.w,
        );
    } else if params.kind_id == 11u {
        // Spin: rotation dissolve around center
        // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount
        let rotations = max(params.param0, 0.05);
        let zoom_amount = clamp(params.param1, 0.0, 1.0);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let angle = p * rotations * 6.283185; // 2*PI
        let scale = 1.0 + p * (1.0 - p) * 4.0 * zoom_amount;
        let inv_s = 1.0 / scale;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let rx = (dx * cos(angle) + dy * sin(angle)) * inv_s + cx;
        let ry = (-dx * sin(angle) + dy * cos(angle)) * inv_s + cy;
        let rotated_from = bilinear(false, rx, ry);
        result = lerp_pixel(rotated_from, to_px, p);
    } else if params.kind_id == 12u {
        // Light Leak: additive coloured glow peaking at midpoint.
        // param0 = intensity, color = leak colour (default warm amber).
        let leak_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let intensity = clamp(params.param0, 0.0, 2.0);
        let leak_t = pow(p * (1.0 - p) * 4.0, 0.7) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        // A soft patch of light drifting across the frame as the shots
        // change, brightest at its middle: added over the whole frame it
        // was a wash (#2765). Additive, so a leak of any colour shows.
        let lx = (-0.2 + 1.4 * p) * f32(params.width);
        let ly = 0.35 * f32(params.height);
        let ex = (f32(x) - lx) / (0.45 * f32(params.width));
        let ey = (f32(y) - ly) / (0.6 * f32(params.height));
        let falloff = exp(-2.5 * (ex * ex + ey * ey));
        let add = leak_color * leak_t * falloff * 0.8;
        result = vec4<f32>(
            min(base.x + add.x, 255.0),
            min(base.y + add.y, 255.0),
            min(base.z + add.z, 255.0),
            base.w,
        );
    } else if params.kind_id == 13u {
        // Luminance Dissolve: bright pixels of `from` dissolve first, dark pixels last.
        // param0 = softness (default 1.0).
        let from_norm = from_px.rgb / 255.0;
        let luma = dot(from_norm, vec3<f32>(0.2126, 0.7152, 0.0722));
        let softness = max(params.param0 * 0.15, 0.01);
        // Each pixel turns over when the transition reaches 1 - its luma,
        // the progress stretched by the softness so the first starts at 0
        // and the last ends at 1: against the luma itself it began and
        // ended part way, a pop at both cuts (#2765).
        let start = 1.0 - luma;
        let at = p * (1.0 + 2.0 * softness) - softness;
        let t = smoothstep(start - softness, start + softness, at);
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 14u || params.kind_id == 15u {
        // Circle Wipe In (14) expands from center; Out (15) contracts to center.
        // param0 = softness, param1 = cx (0..1), param2 = cy (0..1)
        let cx = params.param1 * f32(params.width);
        let cy = params.param2 * f32(params.height);
        let mx = max(cx, f32(params.width) - cx);
        let my = max(cy, f32(params.height) - cy);
        let max_r = max(length(vec2<f32>(mx, my)), 1.0);
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy)) / max_r;
        let softness = max(params.param0 * 0.04, 0.001);
        var radius: f32;
        if params.kind_id == 14u {
            radius = p * (1.0 + 2.0 * softness) - softness;
        } else {
            radius = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
        }
        let s_val = smoothstep(radius - softness, radius + softness, dist);
        var t: f32;
        if params.kind_id == 14u {
            t = 1.0 - s_val;
        } else {
            t = s_val;
        }
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 16u {
        // Clock Wipe: angular sweep around frame center.
        // param0=softness, param1=start angle deg, param2=direction (0=CW, 1=CCW)
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let start_rad = params.param1 * 3.14159265 / 180.0;
        let two_pi = 6.28318530;
        var angle = atan2(dx, -dy) - start_rad;
        angle = ((angle - floor(angle / two_pi) * two_pi) + two_pi);
        angle = angle - floor(angle / two_pi) * two_pi;
        if params.param2 > 0.5 {
            angle = two_pi - angle;
        }
        let norm_angle = angle / two_pi;
        let softness = max(params.param0 * 0.02, 0.001);
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_val = smoothstep(threshold - softness, threshold + softness, norm_angle);
        result = lerp_pixel(from_px, to_px, 1.0 - s_val);
    } else if params.kind_id == 17u || params.kind_id == 18u {
        // Barn Door Open (17) splits from center outward; Close (18) closes inward.
        // param0 = softness, param1 = orientation (0=horizontal, 1=vertical)
        let softness = max(params.param0 * 0.03, 0.001);
        var dist: f32;
        if params.param1 > 0.5 {
            let cy = f32(params.height) * 0.5;
            dist = abs(f32(y) - cy) / max(cy, 1.0);
        } else {
            let cx = f32(params.width) * 0.5;
            dist = abs(f32(x) - cx) / max(cx, 1.0);
        }
        var t_bd: f32;
        if params.kind_id == 17u {
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            t_bd = 1.0 - smoothstep(threshold - softness, threshold + softness, dist);
        } else {
            let threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
            t_bd = smoothstep(threshold - softness, threshold + softness, dist);
        }
        result = lerp_pixel(from_px, to_px, t_bd);
    } else if params.kind_id == 19u {
        // Pixelate: block size peaks at midpoint.
        // param0 = max block size, param1 = mode (0 = pixelate+dissolve, 1 = cross)
        let max_block = max(params.param0, 2.0);
        let triangle = 1.0 - abs(p * 2.0 - 1.0);
        let block_f = max(max_block * triangle, 1.0);
        let block = max(u32(block_f), 1u);
        let bx = min((x / block) * block + block / 2u, params.width - 1u);
        let by = min((y / block) * block + block / 2u, params.height - 1u);
        let block_idx = by * params.width + bx;
        let from_block = to_bytes(unpack(from_buf[block_idx]));
        if params.param1 > 0.5 {
            let to_block = to_bytes(unpack(to_buf[block_idx]));
            result = lerp_pixel(from_block, to_block, p);
        } else if p < 0.5 {
            result = from_block;
        } else {
            let t_px = (p - 0.5) * 2.0;
            result = lerp_pixel(from_block, to_px, t_px);
        }
    } else if params.kind_id == 20u {
        // Ripple: rings spreading from the centre through both shots, the
        // surface lit by their slope, strongest at the cut. Sized by the
        // frame: param0 the amplitude in pixels of a 1080-line frame,
        // param1 the rings across its height, param2 their speed. Sized by
        // the pixel instead, the rings were finer than the picture's detail
        // and moved one shot only: a plain dissolve to see (#2764).
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let wave = sin(p * 3.14159265);
        // The rings are the shared warp's (`crate::warp`).
        let r = warp_ripple(f32(x), f32(y), f32(params.width), f32(params.height), cx, cy, params.param1, p * params.param2 * 10.0, wave, params.param0);
        let phase = r.z;
        let base = lerp_pixel(bilinear(false, r.x, r.y), bilinear(true, r.x, r.y), p);
        let light = 1.0 + cos(phase) * wave * 0.15;
        result = vec4<f32>(clamp(base.x * light, 0.0, 255.0), clamp(base.y * light, 0.0, 255.0), clamp(base.z * light, 0.0, 255.0), base.w);
    } else if params.kind_id == 21u || params.kind_id == 22u {
        // Page Turn (21) / Page Turn Reverse (22): the page turns over from
        // one side, a shadow at the fold and its curled back beside it.
        // param0 = shadow intensity, param1 = direction (0=RTL, 1=LTR) for PageTurn.
        let shadow_intensity = params.param0;
        var fx: f32 = f32(x);
        if params.kind_id == 22u || params.param1 > 0.5 {
            fx = f32(params.width) - 1.0 - fx;
        }
        let norm_x = fx / f32(params.width);
        let fold_x = 1.0 - p;
        let denom = max(p * 0.3, 0.01);
        if norm_x > fold_x {
            let shadow_dist = (norm_x - fold_x) / denom;
            let shadow = shadow_intensity * exp(-shadow_dist * 3.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4<f32>(to_px.x * t, to_px.y * t, to_px.z * t, 255.0);
        } else if norm_x > fold_x - (1.0 - fold_x) * 0.6 {
            // The turned page curled over beside the fold: its back, the
            // part past the fold mirrored, paler as paper's back is, lit
            // along the curl. Without it the page slid off with no turn
            // to see (#2764).
            let fw = max((1.0 - fold_x) * 0.6, 0.001);
            let d = (fold_x - norm_x) / fw;
            var src = (fold_x + d * (1.0 - fold_x)) * f32(params.width);
            if params.kind_id == 22u || params.param1 > 0.5 {
                src = f32(params.width) - 1.0 - src;
            }
            let back = bilinear(false, clamp(src, 0.0, f32(params.width) - 1.0), f32(y));
            let curl = 0.75 + 0.25 * sin(d * 3.14159265);
            let paper = (back.xyz * 0.45 + vec3<f32>(0.55 * 255.0, 0.53 * 255.0, 0.5 * 255.0)) * curl;
            result = vec4<f32>(paper.x, paper.y, paper.z, 255.0);
        } else {
            let shadow_dist = (fold_x - norm_x) / denom;
            let shadow = shadow_intensity * 0.5 * exp(-shadow_dist * 2.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4<f32>(from_px.x * t, from_px.y * t, from_px.z * t, 255.0);
        }
    } else if params.kind_id == 23u {
        // Chromatic Aberration: R/B channels sampled with offset, envelope peaks at midpoint.
        // param0 = max offset px, param1 = direction (0=H, 1=V, 2=diag)
        let max_offset = params.param0;
        let dir = u32(params.param1 + 0.5);
        let envelope = sin(p * 3.14159265) * max_offset;
        var r_ox: f32 = 0.0;
        var r_oy: f32 = 0.0;
        var b_ox: f32 = 0.0;
        var b_oy: f32 = 0.0;
        if dir == 1u {
            r_oy = -envelope;
            b_oy = envelope;
        } else if dir == 2u {
            r_ox = -envelope * 0.707;
            r_oy = -envelope * 0.707;
            b_ox = envelope * 0.707;
            b_oy = envelope * 0.707;
        } else {
            r_ox = -envelope;
            b_ox = envelope;
        }
        let base_g = lerp_pixel(from_px, to_px, p).y;
        let rx = clamp(f32(x) + r_ox, 0.0, f32(params.width) - 1.0);
        let ry = clamp(f32(y) + r_oy, 0.0, f32(params.height) - 1.0);
        let r_idx = u32(ry) * params.width + u32(rx);
        let r_val = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p).x;
        let bx = clamp(f32(x) + b_ox, 0.0, f32(params.width) - 1.0);
        let by = clamp(f32(y) + b_oy, 0.0, f32(params.height) - 1.0);
        let b_idx = u32(by) * params.width + u32(bx);
        let b_val = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p).z;
        result = vec4<f32>(r_val, base_g, b_val, 255.0);
    } else if params.kind_id == 24u {
        // Film Burn: a hot spot that blooms in from the right edge and
        // sweeps across, warm at its rim and near white at its core, laid
        // over the pictures so both show through, the cut hidden under it.
        // Its edge is ragged with low-frequency noise. Each pixel drew its
        // own random threshold before, which made a flat grain field with
        // no shape at all (#511).
        // param0 = temperature (0 white, 1 amber), param1 = sweep speed,
        // param2 = grain
        let temp = clamp(params.param0, 0.0, 1.0);
        let speed = max(params.param1, 0.1);
        let grain = params.param2;
        let pc = clamp(p, 0.0, 1.0);
        let env = max(sin(pc * 3.14159265), 0.0);
        let w = f32(params.width);
        let h = f32(params.height);
        let short_side = min(w, h);
        let travel = clamp(pc * speed, 0.0, 1.0);
        let cx = w * (1.1 - 0.7 * travel);
        let cy = h * (0.3 + 0.1 * travel);
        let radius = short_side * (0.3 + 0.85 * env);
        let fx = f32(x);
        let fy = f32(y);
        let n = burn_noise(fx / (short_side * 0.3), fy / (short_side * 0.3));
        let d = length(vec2<f32>(fx - cx, fy - cy)) / radius + (n - 0.5) * 0.3;
        let heat = pow(clamp(1.0 - d, 0.0, 1.0), 1.4) * env;
        let warm = mix(vec3<f32>(1.0, 0.9, 0.75), vec3<f32>(1.0, 0.3, 0.05), temp);
        let col = mix(warm, vec3<f32>(1.0, 0.95, 0.85), smoothstep(0.7, 1.0, heat));
        let cut_t = smoothstep(0.35, 0.65, pc);
        // The picture under the burn is warmed as well as lit.
        let base = mix(from_px.xyz, to_px.xyz, cut_t) / 255.0 * mix(vec3<f32>(1.0, 1.0, 1.0), warm, min(heat * 1.5, 1.0));
        let g = (rand3(x, y, u32(pc * 1000.0)) - 0.5) * grain * 0.08 * heat;
        let burned = clamp(base + col * (heat * 1.2) + vec3<f32>(g, g, g), vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0));
        result = vec4<f32>(burned.x * 255.0, burned.y * 255.0, burned.z * 255.0, 255.0);
    } else if params.kind_id == 25u {
        // Star Wipe: N-pointed star expanding from center.
        // param0=softness, param1=point count, param2=inner radius ratio
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let points = max(params.param1, 3.0);
        let inner_ratio = clamp(params.param2, 0.2, 0.8);
        let softness = max(params.param0 * 0.03, 0.001);
        let dist = length(vec2<f32>(dx, dy));
        let angle = atan2(dy, dx);
        let sector = 3.14159265 / points;
        let two_sector = 2.0 * sector;
        // WGSL has no float %, use floor-based modulo. Apply twice to normalize negatives.
        var sector_angle = angle - floor(angle / two_sector) * two_sector;
        sector_angle = sector_angle - floor(sector_angle / two_sector) * two_sector;
        let star_r_norm = mix(
            inner_ratio,
            1.0,
            smoothstep(0.0, sector, abs(sector_angle - sector)),
        );
        let max_r = max(length(vec2<f32>(cx, cy)), 1.0);
        let norm_dist = dist / (max_r * max(star_r_norm, 0.01));
        let max_norm = 1.0 / inner_ratio;
        let threshold = p * (max_norm + 2.0 * softness) - softness;
        let s_val = smoothstep(threshold - softness, threshold + softness, norm_dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_val);
    } else if params.kind_id == 26u || params.kind_id == 27u {
        // Split Wipe H (26) / V (27): reveal `to` expanding from the center line outward.
        let softness = max(params.param0 * 0.03, 0.001);
        var dist: f32;
        if params.kind_id == 26u {
            let cx = f32(params.width) * 0.5;
            dist = abs(f32(x) - cx) / max(cx, 1.0);
        } else {
            let cy = f32(params.height) * 0.5;
            dist = abs(f32(y) - cy) / max(cy, 1.0);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_sp = smoothstep(threshold - softness, threshold + softness, dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_sp);
    } else if params.kind_id == 28u || params.kind_id == 29u {
        // Venetian Blind H (28) / V (29): N parallel strips wipe simultaneously.
        let softness = max(params.param0 * 0.04, 0.001);
        let blinds = max(params.param1, 1.0);
        var strip_pos: f32;
        if params.kind_id == 28u {
            strip_pos = fract(f32(y) / f32(params.height) * blinds);
        } else {
            strip_pos = fract(f32(x) / f32(params.width) * blinds);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_vb = smoothstep(threshold - softness, threshold + softness, strip_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_vb);
    } else if params.kind_id == 30u {
        // Checkerboard Wipe: alternating squares wipe LTR / RTL.
        // param0 = softness, param1 = grid size (squares per axis)
        let softness = max(params.param0 * 0.04, 0.001);
        let grid = max(params.param1, 1.0);
        let fx = f32(x) / f32(params.width) * grid;
        let fy = f32(y) / f32(params.height) * grid;
        let col = u32(fx);
        let row = u32(fy);
        let local_x = fract(fx);
        var local_pos: f32;
        if (col + row) % 2u == 0u {
            local_pos = local_x;
        } else {
            local_pos = 1.0 - local_x;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_cb = smoothstep(threshold - softness, threshold + softness, local_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_cb);
    } else if params.kind_id == 31u || params.kind_id == 32u {
        // Diagonal wipe TL-BR (31) / TR-BL (32).
        let softness = max(params.param0 * 0.03, 0.001);
        let nx = f32(x) / f32(params.width);
        let ny = f32(y) / f32(params.height);
        var diag: f32;
        if params.kind_id == 31u {
            diag = (nx + ny) * 0.5;
        } else {
            diag = ((1.0 - nx) + ny) * 0.5;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_diag = smoothstep(threshold - softness, threshold + softness, diag);
        result = lerp_pixel(from_px, to_px, 1.0 - s_diag);
    } else if params.kind_id == 33u || params.kind_id == 34u {
        // Box Wipe In (33) / Out (34): rectangle expanding from / contracting to center.
        // param0 = softness, param1 = square blend (0 = frame aspect, 1 = square)
        let softness = max(params.param0 * 0.03, 0.001);
        let square = clamp(params.param1, 0.0, 1.0);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = abs(f32(x) - cx) / max(cx, 1.0);
        let dy = abs(f32(y) - cy) / max(cy, 1.0);
        let box_dist = max(dx, dy);
        let sq_dist = max(dx * cx / max(cy, 1.0), dy);
        let dist = box_dist + (sq_dist - box_dist) * square;
        let aspect = max(cx / max(cy, 1.0), 1.0);
        let max_norm = 1.0 + (aspect - 1.0) * square;
        var threshold: f32;
        if params.kind_id == 33u {
            threshold = p * (max_norm + 2.0 * softness) - softness;
        } else {
            threshold = (1.0 - p) * (max_norm + 2.0 * softness) - softness;
        }
        let s_box = smoothstep(threshold - softness, threshold + softness, dist);
        var t_box: f32;
        if params.kind_id == 33u {
            t_box = 1.0 - s_box;
        } else {
            t_box = s_box;
        }
        result = lerp_pixel(from_px, to_px, t_box);
    } else if params.kind_id >= 35u && params.kind_id <= 44u {
        // Slide / Cover / Uncover / Squeeze family — pixel-offset sampling.
        let wf = f32(params.width);
        let hf = f32(params.height);
        let wi = i32(params.width);
        let hi = i32(params.height);
        let offset_w = i32(p * wf);
        let offset_h = i32(p * hf);
        var sx: i32 = i32(x);
        var sy: i32 = i32(y);
        var use_to: bool = false;
        if params.kind_id == 35u {
            let to_x = i32(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            } else {
                sx = clamp(i32(x) - offset_w, 0, wi - 1);
            }
        } else if params.kind_id == 36u {
            let to_x = i32(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            } else {
                sx = min(i32(x) + offset_w, wi - 1);
            }
        } else if params.kind_id == 37u {
            let to_y = i32(y) + hi - offset_h;
            if to_y < hi {
                use_to = true;
                sy = max(to_y, 0);
            } else {
                sy = clamp(i32(y) - offset_h, 0, hi - 1);
            }
        } else if params.kind_id == 38u {
            let to_y = i32(y) - hi + offset_h;
            if to_y >= 0 {
                use_to = true;
                sy = min(to_y, hi - 1);
            } else {
                sy = min(i32(y) + offset_h, hi - 1);
            }
        } else if params.kind_id == 39u {
            let to_x = i32(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            }
        } else if params.kind_id == 40u {
            let to_x = i32(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            }
        } else if params.kind_id == 41u {
            let from_x = i32(x) + offset_w;
            if from_x < wi {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 42u {
            let from_x = i32(x) - offset_w;
            if from_x >= 0 {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 43u {
            // Squeeze H: proper collapse/expand with black outside.
            let half = wf * 0.5;
            let one_px = 1.0 / wf;
            var out_color: vec4<f32>;
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32(x) < vis_left || f32(x) >= vis_right {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = to_bytes(unpack(from_buf[u32(y) * params.width + u32(src_x)]));
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32(x) < vis_left || f32(x) >= vis_right {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = to_bytes(unpack(to_buf[u32(y) * params.width + u32(src_x)]));
                    }
                }
            }
            result = out_color;
        } else if params.kind_id == 44u {
            // Squeeze V: proper collapse/expand with black outside.
            let half = hf * 0.5;
            let one_px = 1.0 / hf;
            var out_color: vec4<f32>;
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32(y) < vis_top || f32(y) >= vis_bottom {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = to_bytes(unpack(from_buf[u32(src_y) * params.width + u32(x)]));
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32(y) < vis_top || f32(y) >= vis_bottom {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = to_bytes(unpack(to_buf[u32(src_y) * params.width + u32(x)]));
                    }
                }
            }
            result = out_color;
        }
        // Shared lookup for slide/cover/uncover (35–42).
        // Squeeze (43–44) already set result directly above.
        if params.kind_id <= 42u {
            let sidx = u32(sy) * params.width + u32(sx);
            if use_to {
                result = to_bytes(unpack(to_buf[sidx]));
            } else {
                result = to_bytes(unpack(from_buf[sidx]));
            }
        }
    } else if params.kind_id == 66u {
        // Water Drop: a ring spreads from the centre; the incoming
        // picture is revealed inside it, refracted at the crest, with
        // ripples trailing behind the front.
        // param0 = ring width (fraction of the half diagonal)
        // param1 = refraction at the crest, px
        // param2 = trailing ripples
        // param3 = crest highlight
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy));
        let half_diag = length(vec2<f32>(cx, cy));
        let band = max(params.param0, 0.02) * half_diag;
        let front = -band * 0.5 + p * (half_diag * 1.05 + band);
        let s = dist - front;
        let env = sin(p * 3.14159265);
        let crest = exp(-(s * s) / (band * band * 0.18));
        var trail: f32 = 0.0;
        if s < 0.0 {
            trail = sin(s / band * 6.2831853 * max(params.param2, 0.0)) * exp(s / band);
        }
        let disp = (crest + trail * 0.5) * params.param1 * env;
        var nx: f32 = 0.0;
        var ny: f32 = 0.0;
        if dist > 0.1 {
            nx = dx / dist;
            ny = dy / dist;
        }
        let sx = clamp(f32(x) + nx * disp, 0.0, f32(params.width) - 1.0);
        let sy = clamp(f32(y) + ny * disp, 0.0, f32(params.height) - 1.0);
        let from_s = bilinear(false, sx, sy);
        let to_s = bilinear(true, sx, sy);
        let reveal = 1.0 - smoothstep(-band * 0.5, band * 0.5, s);
        let col = lerp_pixel(from_s, to_s, reveal);
        let glint = crest * params.param3 * env * 255.0;
        result = vec4<f32>(min(col.x + glint, 255.0), min(col.y + glint, 255.0), min(col.z + glint, 255.0), col.w);
    } else if params.kind_id == 67u {
        // Whip Pan: the outgoing picture is flung off and the incoming one
        // follows, fastest at the cut, smeared along the move by a blur
        // that follows the speed (#512). The two pictures lie side by side
        // on a strip, and the frame slides along it.
        // param0 = direction (0 left, 1 right, 2 up, 3 down), param1 = blur
        let dir = u32(params.param0 + 0.5);
        let horizontal = dir < 2u;
        var len_axis: f32 = f32(params.height);
        var a: f32 = f32(y);
        if horizontal {
            len_axis = f32(params.width);
            a = f32(x);
        }
        let s = p * p * (3.0 - 2.0 * p);
        let speed = 6.0 * p * (1.0 - p);
        let blur = max(params.param1, 0.0) * speed * len_axis * 0.12;
        var sgn: f32 = 1.0;
        if dir == 1u || dir == 3u {
            sgn = -1.0;
        }
        // 24 taps, each pixel's set shifted by its own hash, so the
        // spacing of the taps shows as fine noise and not as ghost copies.
        let jitter = rand3(x, y, 11u);
        var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        for (var k: u32 = 0u; k < 24u; k = k + 1u) {
            let off = ((f32(k) + jitter) / 24.0 - 0.5) * blur;
            let u = a + sgn * s * len_axis + off;
            var to_side = false;
            var v: f32 = u;
            if sgn > 0.0 {
                if u >= len_axis {
                    to_side = true;
                    v = u - len_axis;
                }
            } else {
                if u < 0.0 {
                    to_side = true;
                    v = u + len_axis;
                }
            }
            v = clamp(v, 0.0, len_axis - 1.0);
            var sx: f32 = f32(x);
            var sy: f32 = f32(y);
            if horizontal {
                sx = v;
            } else {
                sy = v;
            }
            acc = acc + bilinear(to_side, sx, sy);
        }
        result = acc / 24.0;
    } else if params.kind_id == 68u {
        // Slide Strips (#985): strips across the move slide away along
        // their length, each in its turn, uncovering the incoming picture
        // (or the incoming strips slide in over the outgoing). Each output
        // pixel finds its strip and where that strip's picture came from.
        // param0 = strips, param1 = direction in degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let n = clamp(round(params.param0), 1.0, 64.0);
        let ang = params.param1 * 0.017453292;
        let mx = cos(ang);
        let my = sin(ang);
        let qx = f32(x) + 0.5 - w * 0.5;
        let qy = f32(y) + 0.5 - h * 0.5;
        let u = qx * mx + qy * my;
        let v = qy * mx - qx * my;
        // Which strip, and the gap, are judged a hair off the pixel's
        // centre: a centre exactly on an edge would fall either way with
        // the last bit of rounding, and differently on each GPU.
        let uc = (qx + 0.001953125) * mx + (qy + 0.0029296875) * my;
        let vc = (qy + 0.0029296875) * mx - (qx + 0.001953125) * my;
        let ln = abs(w * my) + abs(h * mx);
        let lm = abs(w * mx) + abs(h * my);
        let bw = ln / n;
        let fi = clamp(floor((vc + ln * 0.5) / bw), 0.0, n - 1.0);
        var sgn: f32 = 1.0;
        if (bits & 64u) != 0u && (u32(fi) & 1u) == 1u {
            sgn = -1.0;
        }
        let mid = (n - 1.0) * 0.5;
        let e = piece_travel(piece_key(u32(fi), fi / max(n - 1.0, 1.0), abs(fi - mid) / max(mid, 0.5)));
        let assemble = (bits & 32u) != 0u;
        var t: f32 = e;
        if assemble {
            t = 1.0 - e;
        }
        let su = u - sgn * t * lm;
        let gh = piece_gap(e);
        let fv = vc + ln * 0.5 - fi * bw;
        let sx = su * mx - v * my + w * 0.5;
        let sy = su * my + v * mx + h * 0.5;
        let suc = uc - sgn * t * lm;
        let sxc = suc * mx - vc * my + w * 0.5;
        let syc = suc * my + vc * mx + h * 0.5;
        // The gap is only tested once open: at rest, a pixel's own strip
        // always holds it, whatever the rounding at a strip's edge.
        let in_strip = gh <= 0.0 || (fv >= gh && fv <= bw - gh);
        if in_strip && sxc >= 0.0 && sxc < w && syc >= 0.0 && syc < h {
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 69u {
        // Slide Panels (#985): a grid of square panels slides away along
        // the nearest of right, down, left and up. A pixel's row (across
        // the move) is fixed, so it looks along the row for the panel
        // over it now, the one that has moved furthest on top.
        // param0 = rows, param1 = direction in degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let rows = clamp(round(params.param0), 1.0, 16.0);
        let cols = clamp(round(rows * w / h), 1.0, 32.0);
        let q4 = round(params.param1 / 90.0);
        let quarter = u32(q4 - 4.0 * floor(q4 / 4.0));
        let vertical = quarter == 1u || quarter == 3u;
        var dir: f32 = 1.0;
        if quarter >= 2u {
            dir = -1.0;
        }
        var a: f32 = f32(x) + 0.5;
        var b: f32 = f32(y) + 0.5;
        var la: f32 = w;
        var lb: f32 = h;
        var na: f32 = cols;
        var nb: f32 = rows;
        if vertical {
            a = f32(y) + 0.5;
            b = f32(x) + 0.5;
            la = h;
            lb = w;
            na = rows;
            nb = cols;
        }
        let pa = la / na;
        let pb = lb / nb;
        // Judged a hair off the pixel's centre, as the strips are.
        let ac = a + 0.001953125;
        let bc = b + 0.0029296875;
        let fr = clamp(floor(bc / pb), 0.0, nb - 1.0);
        let fb = bc - fr * pb;
        var sgn: f32 = dir;
        if (bits & 64u) != 0u && (u32(fr) & 1u) == 1u {
            sgn = -dir;
        }
        let assemble = (bits & 32u) != 0u;
        let across = fr / max(nb - 1.0, 1.0);
        var best: f32 = -1.0;
        var best_a: f32 = 0.0;
        for (var j: u32 = 0u; j < 32u; j = j + 1u) {
            let fj = f32(j);
            if fj >= na {
                break;
            }
            var along = fj / max(na - 1.0, 1.0);
            if sgn > 0.0 {
                along = 1.0 - along;
            }
            let ca = (fj + 0.5) / na - 0.5;
            let cb = (fr + 0.5) / nb - 0.5;
            let centre = clamp(sqrt(ca * ca + cb * cb) / 0.7072, 0.0, 1.0);
            let e = piece_travel(piece_key(u32(fr) * 32u + j, (along + across) * 0.5, centre));
            var t: f32 = e;
            if assemble {
                t = 1.0 - e;
            }
            let d = sgn * t * la;
            let gh = piece_gap(e);
            let sa = a - d;
            let sac = ac - d;
            let lo = fj * pa;
            // Its own panel by index, as the pixel's row was found, so at
            // rest every pixel is in exactly one; the gap once open.
            let in_panel = floor(sac / pa) == fj && (gh <= 0.0 || (sac >= lo + gh && sac < lo + pa - gh && fb >= gh && fb < pb - gh));
            if in_panel && abs(d) >= best {
                best = abs(d);
                best_a = sa;
            }
        }
        if best >= 0.0 {
            var sx: f32 = best_a;
            var sy: f32 = b;
            if vertical {
                sx = b;
                sy = best_a;
            }
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 70u {
        // Part Halves (#985): the picture splits down a line through the
        // middle, and the halves part across it (or slide along it, the
        // ways apart). A half only ever covers its own side of the line.
        // param0 = 0 part / 1 slide along, param1 = the line's angle in
        // degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let ang = params.param1 * 0.017453292;
        let lx = cos(ang);
        let ly = sin(ang);
        let qx = f32(x) + 0.5 - w * 0.5;
        let qy = f32(y) + 0.5 - h * 0.5;
        // The side, judged a hair off the pixel's centre, as the strips are.
        let cx = qx + 0.001953125;
        let cy = qy + 0.0029296875;
        let v = cy * lx - cx * ly;
        var side: f32 = 1.0;
        var i: u32 = 1u;
        if v < 0.0 {
            side = -1.0;
            i = 0u;
        }
        var dx: f32 = -ly * side;
        var dy: f32 = lx * side;
        var dist: f32 = (abs(w * ly) + abs(h * lx)) * 0.5 + 1.0;
        if params.param0 > 0.5 {
            dx = lx * side;
            dy = ly * side;
            dist = abs(w * lx) + abs(h * ly);
        }
        let e = piece_travel(piece_key(i, f32(i), 0.0));
        let assemble = (bits & 32u) != 0u;
        var t: f32 = e;
        if assemble {
            t = 1.0 - e;
        }
        let spx = qx - dx * t * dist;
        let spy = qy - dy * t * dist;
        let sv = ((cy - dy * t * dist) * lx - (cx - dx * t * dist) * ly) * side;
        let sx = spx + w * 0.5;
        let sy = spy + h * 0.5;
        let sxc = cx - dx * t * dist + w * 0.5;
        let syc = cy - dy * t * dist + h * 0.5;
        if sv >= piece_gap(e) && sxc >= 0.0 && sxc < w && syc >= 0.0 && syc < h {
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 63u || params.kind_id == 64u || params.kind_id == 65u {
        // Zoom family: scale from/to around frame center. In (63) the
        // outgoing shot pushes in, out (64) the incoming one settles from
        // close up, cross (65) both; whichever moves is blurred along the
        // zoom, most at the cut, as a fast zoom is. Without the blur the
        // zooms read as a plain dissolve on footage (#2764).
        let zoom_amount = max(params.param0, 1.001);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        var ep: f32 = p;
        if params.kind_id == 65u && params.param1 > 0.5 {
            ep = p * p * (3.0 - 2.0 * p);
        }
        let from_zooms = params.kind_id != 64u;
        let to_zooms = params.kind_id != 63u;
        // Each moving shot travels twice its zoom over the transition, so
        // it is at the full zoom amount at the cut, where it is half of the
        // picture; past that it is all but gone.
        let travel = 2.0 * (zoom_amount - 1.0);
        let scale_a = select(1.0, 1.0 + travel * ep, from_zooms);
        let scale_b = select(1.0, 1.0 + travel * (1.0 - ep), to_zooms);
        let blur = sin(p * 3.14159265) * 0.08;
        var from_sum = vec4<f32>(0.0);
        var to_sum = vec4<f32>(0.0);
        for (var k = 0u; k < 6u; k = k + 1u) {
            let spread = 1.0 + blur * f32(k) / 5.0;
            let sa = scale_a * select(1.0, spread, from_zooms);
            let sb = scale_b * select(1.0, spread, to_zooms);
            from_sum = from_sum + bilinear(false, clamp(dx / sa + cx, 0.0, f32(params.width) - 1.0), clamp(dy / sa + cy, 0.0, f32(params.height) - 1.0));
            to_sum = to_sum + bilinear(true, clamp(dx / sb + cx, 0.0, f32(params.width) - 1.0), clamp(dy / sb + cy, 0.0, f32(params.height) - 1.0));
        }
        result = lerp_pixel(from_sum / 6.0, to_sum / 6.0, ep);
    } else if params.kind_id == 62u {
        // Bokeh Dissolve: screen-blend bloom + chromatic fringe.
        let bokeh_size = params.param0;
        let brightness_boost = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom_strength = luma * bokeh_size * envelope * brightness_boost * 2.0;
        let bloom_val = bloom_strength * 255.0;
        let screen_r = 255.0 - (255.0 - base.x) * (255.0 - bloom_val) / 255.0;
        let screen_g = 255.0 - (255.0 - base.y) * (255.0 - bloom_val) / 255.0;
        let screen_b = 255.0 - (255.0 - base.z) * (255.0 - bloom_val) / 255.0;
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = (f32(x) - cx) / max(cx, 1.0);
        let dy = (f32(y) - cy) / max(cy, 1.0);
        let radial = length(vec2<f32>(dx, dy));
        let fringe = radial * bokeh_size * envelope * 8.0;
        result = vec4<f32>(
            clamp(screen_r + fringe, 0.0, 255.0),
            clamp(screen_g, 0.0, 255.0),
            clamp(screen_b - fringe, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 60u {
        // Anamorphic Flare: horizontal streak across frame center.
        let intensity = params.param0;
        let streak_width = params.param1;
        let flare_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        let ny = abs(f32(y) / f32(params.height) - 0.5) * 2.0;
        let streak_t = max(1.0 - ny / max(streak_width, 0.001), 0.0);
        let streak_t2 = streak_t * streak_t;
        let shimmer = 0.7 + 0.3 * sin(f32(x) / f32(params.width) * 3.14159265 * 8.0);
        let flare_strength = streak_t2 * envelope * shimmer;
        let flare = flare_color * flare_strength;
        result = vec4<f32>(
            clamp(base.x + flare.x, 0.0, 255.0),
            clamp(base.y + flare.y, 0.0, 255.0),
            clamp(base.z + flare.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 61u {
        // Dream Sequence: bloom + warm glow + overexposure.
        let bloom_intensity = params.param0;
        let warmth = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom = luma * bloom_intensity * envelope * 60.0;
        let warm_r = warmth * envelope * 30.0;
        let warm_g = warmth * envelope * 15.0;
        let warm_b = -warmth * envelope * 10.0;
        let lift = envelope * bloom_intensity * 15.0;
        result = vec4<f32>(
            clamp(base.x + bloom + warm_r + lift, 0.0, 255.0),
            clamp(base.y + bloom + warm_g + lift, 0.0, 255.0),
            clamp(base.z + bloom + warm_b + lift, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 58u {
        // Projector Flicker: brightness flicker + gate weave + frame jump.
        let flicker_intensity = params.param0;
        let weave_amount = params.param1;
        let envelope = sin(p * 3.14159265) * flicker_intensity;
        let frame_hash = rand3(bitcast<u32>(p), 0u, 3u);
        let frame_hash2 = rand3(bitcast<u32>(p), 0u, 4u);
        let frame_hash3 = rand3(bitcast<u32>(p), 0u, 5u);
        let brightness = 1.0 + (frame_hash - 0.5) * envelope * 0.6;
        let weave = (frame_hash2 - 0.5) * weave_amount * envelope;
        let sx = clamp(f32(x) + weave, 0.0, f32(params.width) - 1.0);
        var jump: f32 = 0.0;
        if frame_hash3 < envelope * 0.1 {
            jump = (frame_hash3 - 0.5) * 8.0;
        }
        let sy = clamp(f32(y) + jump, 0.0, f32(params.height) - 1.0);
        let sample_idx = u32(sy) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        result = vec4<f32>(
            clamp(base.x * brightness, 0.0, 255.0),
            clamp(base.y * brightness, 0.0, 255.0),
            clamp(base.z * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 59u {
        // Halation: per-pixel brightness-threshold additive glow. O(1) per pixel.
        let threshold = params.param0;
        let spread = params.param1;
        let halo_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let excess = max(luma - threshold, 0.0) / max(1.0 - threshold, 0.001);
        let halo_strength = excess * spread * envelope * 3.0;
        let halo = halo_color * halo_strength;
        result = vec4<f32>(
            clamp(base.x + halo.x, 0.0, 255.0),
            clamp(base.y + halo.y, 0.0, 255.0),
            clamp(base.z + halo.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 57u {
        // Glitch RGB Split: bands of the picture with their red pushed one
        // way and their blue the other, the bands and their pushes jumping a
        // dozen times through the transition, strongest at the cut. Per
        // 8-pixel block, with blocks blacked out, it was black specks with
        // no split to see (#2765). param1 the band height's unit in pixels.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        // The torn rows are the shared warp's (`crate::warp`).
        let shift = warp_band_shift(f32(y), block_size * 3.0, u32(p * 12.0), envelope, f32(params.width));
        let rx = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let bx = clamp(f32(x) - shift, 0.0, f32(params.width) - 1.0);
        let r_idx = u32(y) * params.width + u32(rx);
        let b_idx = u32(y) * params.width + u32(bx);
        let r_val = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p).x;
        let g_val = lerp_pixel(from_px, to_px, p).y;
        let b_val = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p).z;
        result = vec4<f32>(r_val, g_val, b_val, 255.0);
    } else if params.kind_id == 55u {
        // Static TV: noise + scan lines + horizontal band shift.
        let intensity = params.param0;
        let line_h = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let hash = rand3(x + y * 65536u, bitcast<u32>(p), 9u);
        let hash2 = rand3(x + y * 65536u, bitcast<u32>(p), 10u);
        var scan_line: f32 = 1.0;
        if (u32(f32(y) / line_h) % 2u) == 0u {
            scan_line = 0.75;
        }
        let static_val = hash * 255.0 * envelope * scan_line;
        let band = u32(f32(y) / line_h);
        let band_hash = rand3(band, bitcast<u32>(p), 11u);
        let shift = (band_hash - 0.5) * envelope * 20.0;
        let sx = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let sample_idx = u32(y) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        let static_mix = envelope * hash2 * 0.8;
        result = vec4<f32>(
            clamp(base.x + (static_val - base.x) * static_mix, 0.0, 255.0),
            clamp(base.y + (static_val - base.y) * static_mix, 0.0, 255.0),
            clamp(base.z + (static_val - base.z) * static_mix, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 56u {
        // VHS Tracking: horizontal band shifts + chroma bleed.
        let intensity = params.param0;
        let band_count = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let band = u32(f32(y) / f32(params.height) * band_count);
        let band_hash = rand3(band, bitcast<u32>(p), 11u);
        let band_hash2 = rand3(band, bitcast<u32>(p), 12u);
        let shift = (band_hash - 0.5) * envelope * 40.0;
        let sx = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let sample_idx = u32(y) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let chroma_shift = (band_hash2 - 0.5) * envelope * 15.0;
        let csx = clamp(f32(x) + chroma_shift, 0.0, f32(params.width) - 1.0);
        let c_idx = u32(y) * params.width + u32(csx);
        let from_c = to_bytes(unpack(from_buf[c_idx]));
        let to_c = to_bytes(unpack(to_buf[c_idx]));
        let chroma_base = lerp_pixel(from_c, to_c, p);
        let vhs_r = mix(base.x, chroma_base.x, envelope * 0.5);
        let vhs_g = mix(base.y, luma, envelope * 0.5);
        let vhs_b = mix(base.z, chroma_base.z, envelope * 0.5);
        let brightness = 1.0 - band_hash * envelope * 0.4;
        result = vec4<f32>(
            clamp(vhs_r * brightness, 0.0, 255.0),
            clamp(vhs_g * brightness, 0.0, 255.0),
            clamp(vhs_b * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 53u {
        // Swirl: rotational distortion spiraling from center.
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let max_r = max(length(vec2<f32>(cx, cy)), 1.0);
        let max_angle = params.param0 * 3.14159265 / 180.0;
        let envelope = sin(p * 3.14159265);
        // The turn is the shared warp's (`crate::warp`).
        let s = warp_twirl(f32(x), f32(y), f32(params.width), f32(params.height), cx, cy, max_r, max_angle * envelope, params.param1);
        let sample_idx = u32(s.y) * params.width + u32(s.x);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        result = lerp_pixel(from_s, to_s, p);
    } else if params.kind_id == 54u {
        // Wave Distortion: both shots bent by a sine wave, its crests lit,
        // strongest at the cut; param0 the amplitude in pixels of a
        // 1080-line frame. Unlit, its bend read as a plain dissolve on
        // footage (#2764).
        let wave = sin(p * 3.14159265);
        let envelope = wave * params.param0 * f32(params.height) / 1080.0;
        // The bend is the shared warp's (`crate::warp`).
        let b = warp_wave(f32(x), f32(y), f32(params.width), f32(params.height), envelope, params.param1, u32(params.param2 + 0.5), 0.0);
        let base = lerp_pixel(bilinear(false, b.x, b.y), bilinear(true, b.x, b.y), p);
        let light = 1.0 + b.z * wave * 0.15;
        result = vec4<f32>(clamp(base.x * light, 0.0, 255.0), clamp(base.y * light, 0.0, 255.0), clamp(base.z * light, 0.0, 255.0), base.w);
    } else if params.kind_id == 51u {
        // Colorize Flash: user-defined color washes over at the cut.
        let flash_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = flash_t * intensity;
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp(
            (p - dissolve_start) / max(1.0 - dissolve_start, 0.001),
            0.0,
            1.0,
        );
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        // The picture in the flash's colour, its light kept and lifted: a
        // flat colour over it hid the shot for much of the transition
        // (#2765).
        let coefs = vec3<f32>(0.2126, 0.7152, 0.0722);
        let l = dot(base.xyz / 255.0, coefs);
        let cl = max(dot(flash_color / 255.0, coefs), 0.05);
        let tinted = min(flash_color * (l / cl) * 1.4, vec3<f32>(255.0));
        result = vec4<f32>(
            mix(base.x, tinted.x, flash_t),
            mix(base.y, tinted.y, flash_t),
            mix(base.z, tinted.z, flash_t),
            255.0,
        );
    } else if params.kind_id == 52u {
        // Hue Rotate: hue spins through the color wheel at midpoint.
        let rotations = params.param0;
        let envelope = sin(p * 3.14159265) * rotations;
        let hue_shift = envelope * 360.0;
        let base = lerp_pixel(from_px, to_px, p);
        let r = base.x / 255.0;
        let g = base.y / 255.0;
        let b = base.z / 255.0;
        let cmax = max(r, max(g, b));
        let cmin = min(r, min(g, b));
        let delta = cmax - cmin;
        // RGB → HSV (hue in degrees)
        var h: f32 = 0.0;
        if delta > 0.001 {
            if abs(cmax - r) < 0.001 {
                let tmp = (g - b) / delta;
                h = 60.0 * (tmp - floor(tmp / 6.0) * 6.0);
            } else if abs(cmax - g) < 0.001 {
                h = 60.0 * ((b - r) / delta + 2.0);
            } else {
                h = 60.0 * ((r - g) / delta + 4.0);
            }
        }
        let s = select(0.0, delta / cmax, cmax > 0.001);
        let v = cmax;
        // Shift hue, normalize to [0, 360)
        var h2 = h + hue_shift;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        // HSV → RGB
        let c = v * s;
        let h_sector = h2 / 60.0;
        let h_mod2 = h_sector - floor(h_sector / 2.0) * 2.0;
        let x2 = c * (1.0 - abs(h_mod2 - 1.0));
        let m = v - c;
        let hi = u32(h_sector) % 6u;
        var rgb: vec3<f32>;
        if hi == 0u { rgb = vec3<f32>(c, x2, 0.0); }
        else if hi == 1u { rgb = vec3<f32>(x2, c, 0.0); }
        else if hi == 2u { rgb = vec3<f32>(0.0, c, x2); }
        else if hi == 3u { rgb = vec3<f32>(0.0, x2, c); }
        else if hi == 4u { rgb = vec3<f32>(x2, 0.0, c); }
        else { rgb = vec3<f32>(c, 0.0, x2); }
        result = vec4<f32>((rgb + vec3<f32>(m)) * 255.0, 255.0);
    } else if params.kind_id == 49u {
        // Negative Flash: brief inversion at the cut point.
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = flash_t * intensity;
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp(
            (p - dissolve_start) / max(1.0 - dissolve_start, 0.001),
            0.0,
            1.0,
        );
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        let negative = vec4<f32>(255.0 - base.x, 255.0 - base.y, 255.0 - base.z, 255.0);
        result = lerp_pixel(base, negative, flash_t);
    } else if params.kind_id == 50u {
        // Sepia Transition: warm sepia tone through the cut.
        let strength = clamp(params.param0, 0.0, 1.0);
        let warmth = clamp(params.param1, 0.5, 2.0);
        let envelope = sin(p * 3.14159265) * strength;
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let sepia_r = clamp(luma * warmth * 1.2, 0.0, 255.0);
        let sepia_g = clamp(luma * warmth * 1.0, 0.0, 255.0);
        let sepia_b = clamp(luma * warmth * 0.75, 0.0, 255.0);
        let sepia = vec4<f32>(sepia_r, sepia_g, sepia_b, 255.0);
        result = lerp_pixel(base, sepia, envelope);
    } else if params.kind_id == 47u {
        // Glitch Dissolve: block shifts + RGB split + noise.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let block_row = u32(f32(y) / block_size);
        let hash = rand3(block_row, bitcast<u32>(p), 13u);
        let hash2 = rand3(block_row, bitcast<u32>(p), 14u);
        let shift = (hash - 0.5) * envelope * 40.0;
        let sx_g = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let r_shift = shift * 0.5;
        let b_shift = -shift * 0.5;
        let rx = clamp(f32(x) + r_shift, 0.0, f32(params.width) - 1.0);
        let bx = clamp(f32(x) + b_shift, 0.0, f32(params.width) - 1.0);
        let g_idx = u32(y) * params.width + u32(sx_g);
        let r_idx = u32(y) * params.width + u32(rx);
        let b_idx = u32(y) * params.width + u32(bx);
        let base_g = lerp_pixel(to_bytes(unpack(from_buf[g_idx])), to_bytes(unpack(to_buf[g_idx])), p);
        let r_base = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p);
        let b_base = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p);
        let noise = (hash2 - 0.5) * envelope * 60.0;
        result = vec4<f32>(
            clamp(r_base.x + noise, 0.0, 255.0),
            clamp(base_g.y + noise * 0.5, 0.0, 255.0),
            clamp(b_base.z + noise, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 48u {
        // Desaturate Transition: drain to grey at midpoint.
        let depth = clamp(params.param0, 0.0, 1.0);
        let envelope = sin(p * 3.14159265) * depth;
        let base = lerp_pixel(from_px, to_px, p);
        // Luma in 0-255 space.
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let grey = vec4<f32>(luma, luma, luma, 255.0);
        result = mix(base, grey, envelope);
    } else if params.kind_id == 45u {
        // Additive Dissolve: the incoming shot rises to full while the
        // outgoing one is still full, then that one falls; the two
        // screened together, so the middle is the brighter of both lifted
        // rather than half of white added over it, a wash (#2765). param0
        // the share of that over a plain dissolve.
        let intensity = clamp(params.param0, 0.0, 1.0);
        let wa = min(1.0, 2.0 * (1.0 - p));
        let wb = min(1.0, 2.0 * p);
        let a = clamp(from_px.xyz / 255.0, vec3<f32>(0.0), vec3<f32>(1.0)) * wa;
        let b = clamp(to_px.xyz / 255.0, vec3<f32>(0.0), vec3<f32>(1.0)) * wb;
        let screen = (vec3<f32>(1.0) - (vec3<f32>(1.0) - a) * (vec3<f32>(1.0) - b)) * 255.0;
        let base = lerp_pixel(from_px, to_px, p);
        result = vec4<f32>(mix(base.xyz, screen, intensity), 255.0);
    } else if params.kind_id == 46u {
        // Fade to Color: A → solid color → B. User-supplied sRGB → linear.
        let fade_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let hold = clamp(params.param0, 0.0, 0.45);
        let mid_start = 0.5 - hold * 0.5;
        let mid_end = 0.5 + hold * 0.5;
        if p <= mid_start {
            let t = p / max(mid_start, 0.001);
            result = lerp_pixel(from_px, fade_color, t);
        } else if p >= mid_end {
            let t = (p - mid_end) / max(1.0 - mid_end, 0.001);
            result = lerp_pixel(fade_color, to_px, t);
        } else {
            result = fade_color;
        }
    } else {
        result = lerp_pixel(from_px, to_px, p);
    }

    output[idx] = pack(from_bytes(result));
}
