//! Transitions on the CPU: the body of `SHADER_TRANSITION` in `gpu.rs`,
//! run as written through [`crate::wgsl`], so a transition gives the same
//! picture with or without a GPU. The CPU fallback used to be a separate,
//! 8-bit implementation of every kind (`klipp-transitions`), mixing
//! gamma-encoded bytes. Regenerate this body from the shader when the
//! shader changes; `tests/cpu_gpu_transition_parity.rs` holds them
//! together.

#![allow(unused_mut, unused_variables, unused_assignments, unused_parens, dead_code, clippy::all)]

use rayon::prelude::*;

use crate::wgsl::*;

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

struct Buf(Vec<V4>);

impl std::ops::Index<u32> for Buf {
    type Output = V4;
    fn index(&self, i: u32) -> &V4 {
        &self.0[i as usize]
    }
}

fn srgb_to_linear_component(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn unpack(v: V4) -> V4 {
    v * 255.0
}

fn pack(v: V4) -> V4 {
    v / 255.0
}

fn lerp_pixel(a: V4, b: V4, t: f32) -> V4 {
    a * (1.0 - t) + b * t
}

fn hash_u(v: u32) -> u32 {
    let mut x = v;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    (x >> 16) ^ x
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    hash_u(a ^ hash_u(b ^ hash_u(c))) as f32 / 4294967295.0
}

/// The shader's `burn_noise`.
fn burn_noise(px: f32, py: f32) -> f32 {
    let ix = floor(px);
    let iy = floor(py);
    let tx = px - ix;
    let ty = py - iy;
    let ux = tx * tx * (3.0 - 2.0 * tx);
    let uy = ty * ty * (3.0 - 2.0 * ty);
    let x0 = u32c(ix);
    let y0 = u32c(iy);
    let a = rand3(x0, y0, 7u32);
    let b = rand3(x0 + 1, y0, 7u32);
    let c = rand3(x0, y0 + 1, 7u32);
    let d = rand3(x0 + 1, y0 + 1, 7u32);
    mix(mix(a, b, ux), mix(c, d, ux), uy)
}

fn bilinear_in(params: &Params, buf: &Buf, sx: f32, sy: f32) -> V4 {
    let fx = clamp(sx, 0.0, f32c(params.width) - 1.0);
    let fy = clamp(sy, 0.0, f32c(params.height) - 1.0);
    let x0 = u32c(floor(fx));
    let y0 = u32c(floor(fy));
    let x1 = min(x0 + 1, params.width - 1);
    let y1 = min(y0 + 1, params.height - 1);
    let tx = fx - f32c(x0);
    let ty = fy - f32c(y0);
    let w = params.width;
    let top = mix(buf[y0 * w + x0], buf[y0 * w + x1], tx);
    let bot = mix(buf[y1 * w + x0], buf[y1 * w + x1], tx);
    unpack(mix(top, bot, ty))
}

/// The shader's `piece_bits` (#985).
fn piece_bits(params: &Params) -> u32 {
    u32c(max(params.param3, 0.0) + 0.5)
}

/// The shader's `piece_key`.
fn piece_key(params: &Params, id: u32, along: f32, centre: f32) -> f32 {
    let order = piece_bits(params) & 7u32;
    if order == 1u32 {
        return 1.0 - along;
    } else if order == 2u32 {
        return centre;
    } else if order == 3u32 {
        return 1.0 - centre;
    } else if order == 4u32 {
        return rand3(id, 985u32, 3u32);
    } else if order == 5u32 {
        return 0.0;
    }
    along
}

/// The shader's `piece_travel`.
fn piece_travel(params: &Params, k: f32) -> f32 {
    let s = clamp(params.param2, 0.0, 0.9);
    let t = clamp((params.progress - s * k) / (1.0 - s), 0.0, 1.0);
    let ease = (piece_bits(params) >> 3u32) & 3u32;
    if ease == 1u32 {
        return t;
    } else if ease == 2u32 {
        return t * t;
    } else if ease == 3u32 {
        let u = t - 1.0;
        return 1.0 + 2.70158 * u * u * u + 1.70158 * u * u;
    }
    t * t * (3.0 - 2.0 * t)
}

/// The shader's `piece_gap`.
fn piece_gap(params: &Params, e: f32) -> f32 {
    f32c((piece_bits(params) >> 7u32) & 31u32) * 0.5 * clamp(8.0 * e * (1.0 - e), 0.0, 1.0)
}

/// A transition between two linear-light RGBA frames on the CPU — the
/// same arguments, and the same picture, as
/// `GpuEffectsPipeline::render_transition_linear`.
#[allow(clippy::too_many_arguments)]
pub fn render_transition_linear(
    from: &[f32],
    to: &[f32],
    kind_id: u32,
    progress: f32,
    width: u32,
    height: u32,
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color: [f32; 4],
) -> Vec<f32> {
    // Posed pieces (#985) are drawn forward, not inverse-mapped here.
    if crate::pieces::is_posed(kind_id, param3) {
        return crate::pieces::render(from, to, &crate::pieces::plan(kind_id, progress, width, height, [param0, param1, param2, param3], 0.0, 1));
    }
    let buf = |p: &[f32]| Buf(p.chunks_exact(4).map(|c| V4 { x: c[0], y: c[1], z: c[2], w: c[3] }).collect());
    let (from_buf, to_buf) = (buf(from), buf(to));
    let params = Params {
        width, height, progress, kind_id, param0, param1, param2, param3,
        color_r: color[0], color_g: color[1], color_b: color[2], color_a: color[3],
    };
    let mut out = vec![0.0f32; (width as usize) * (height as usize) * 4];
    out.par_chunks_exact_mut(4).enumerate().for_each(|(i, o)| {
        let v = pixel(&params, &from_buf, &to_buf, i as u32);
        o.copy_from_slice(&[v.x, v.y, v.z, v.w]);
    });
    out
}

fn pixel(params: &Params, from_buf: &Buf, to_buf: &Buf, idx: u32) -> V4 {
    let bilinear = |to: bool, sx: f32, sy: f32| bilinear_in(params, if to { to_buf } else { from_buf }, sx, sy);
    let y = idx / params.width;
    let x = idx % params.width;
    let p = params.progress;

    let from_px = unpack(from_buf[idx]);
    let to_px = unpack(to_buf[idx]);

    let mut result: V4 = Default::default();

    if params.kind_id == 0u32 {
        // Cross Dissolve
        result = lerp_pixel(from_px, to_px, p);
    } else if params.kind_id == 1u32 {
        // Wipe Left (reveals to-frame from left to right)
        // param0 = feather radius in pixels (0 = hard edge)
        // threshold expanded by ±feather so endpoints fully clear/cover the frame
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32c(params.width) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32c(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 2u32 {
        // Wipe Right (reveals to-frame from right to left)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32c(params.width) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32c(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 3u32 {
        // Wipe Up (reveals to-frame from top to bottom)
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32c(params.height) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32c(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 4u32 {
        // Wipe Down (reveals to-frame from bottom to top)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32c(params.height) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32c(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 5u32 || params.kind_id == 6u32 {
        // Dip to colour. param0 = hold duration (0..0.45) at the midpoint.
        // colour = dip colour (default black for kind 5, white for kind 6).
        // `params.color_*` are already in 0-1 sRGB — decode to linear
        // so the dip crossfade works in physical light. Scale to the
        // 0-255 local convention this shader body uses.
        let dip_color = vec4!(
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
    } else if params.kind_id == 7u32 {
        // Push: outgoing slides off, incoming slides on. Hard slide, no blend.
        // param0 = direction (0=Left, 1=Right, 2=Up, 3=Down)
        let dir = u32c(params.param0 + 0.5);
        if dir == 1u32 {
            // Push Right: outgoing slides right, incoming enters from left
            let offset = i32c(p * f32c(params.width));
            let src_x = i32c(x) - offset;
            if src_x >= 0 {
                result = unpack(from_buf[y * params.width + u32c(src_x)]);
            } else {
                let in_x = u32c(src_x + i32c(params.width));
                result = unpack(to_buf[y * params.width + in_x]);
            }
        } else if dir == 2u32 {
            // Push Up: outgoing slides up, incoming enters from bottom
            let offset = u32c(p * f32c(params.height));
            let src_y = y + offset;
            if src_y < params.height {
                result = unpack(from_buf[src_y * params.width + x]);
            } else {
                let in_y = src_y - params.height;
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else if dir == 3u32 {
            // Push Down: outgoing slides down, incoming enters from top
            let offset = i32c(p * f32c(params.height));
            let src_y = i32c(y) - offset;
            if src_y >= 0 {
                result = unpack(from_buf[u32c(src_y) * params.width + x]);
            } else {
                let in_y = u32c(src_y + i32c(params.height));
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else {
            // Push Left (default): outgoing slides left, incoming enters from right
            let offset = u32c(p * f32c(params.width));
            let src_x = x + offset;
            if src_x < params.width {
                result = unpack(from_buf[y * params.width + src_x]);
            } else {
                let in_x = src_x - params.width;
                result = unpack(to_buf[y * params.width + in_x]);
            }
        }
    } else if params.kind_id == 8u32 {
        // Film Dissolve: square-root blending that emulates the
        // density-stacking behaviour of film emulsions. In the
        // linear pipeline `from_px` / `to_px` are already linear,
        // so we drop the /255 and *255 scaling — the sqrt curve
        // still produces the characteristic over-bright midpoint
        // that distinguishes film dissolve from cross dissolve.
        let blended = sqrt(from_px * from_px * (1.0 - p) + to_px * to_px * p);
        result = blended;
        result.w = from_px.w * (1.0 - p) + to_px.w * p;
    } else if params.kind_id == 9u32 {
        // Blur Dissolve: dissolve + horizontal box blur peaking at midpoint
        // param0 = max blur radius in pixels
        let max_radius = max(params.param0, 1.0);
        let blur_r = i32c(p * (1.0 - p) * 4.0 * max_radius);
        let mut acc = vec4!(0.0);
        let mut count = 0.0;
        let ix = i32c(x);
        let step_sz = max(blur_r / 4, 1);
        let mut dx = -blur_r;
        while dx <= blur_r {
            let sx = clamp(ix + dx, 0, i32c(params.width) - 1);
            let si = y * params.width + u32c(sx);
            let fpx = unpack(from_buf[si]);
            let tpx = unpack(to_buf[si]);
            acc += lerp_pixel(fpx, tpx, p);
            count += 1.0;
            dx += step_sz;
        }
        if count > 0.0 {
            result = acc / count;
        } else {
            result = lerp_pixel(from_px, to_px, p);
        }
    } else if params.kind_id == 10u32 {
        // Flash: bright colour flash with parameterized peak.
        // param0 = intensity, param1 = peak position, color = flash colour.
        // Decode user-supplied sRGB colour to linear, scaled to the
        // 0-255 local convention this shader body uses.
        let flash_color = vec4!(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let intensity = clamp(params.param0, 0.0, 2.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        let mut flash_t: f32 = Default::default();
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
        result = vec4!(
            base.x * (1.0 - flash_t) + flash_color.x * flash_t,
            base.y * (1.0 - flash_t) + flash_color.y * flash_t,
            base.z * (1.0 - flash_t) + flash_color.z * flash_t,
            base.w,
        );
    } else if params.kind_id == 11u32 {
        // Spin: rotation dissolve around center
        // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount
        let rotations = max(params.param0, 0.05);
        let zoom_amount = clamp(params.param1, 0.0, 1.0);
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let angle = p * rotations * 6.283185; // 2*PI
        let scale = 1.0 + p * (1.0 - p) * 4.0 * zoom_amount;
        let inv_s = 1.0 / scale;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let rx = (dx * cos(angle) + dy * sin(angle)) * inv_s + cx;
        let ry = (-dx * sin(angle) + dy * cos(angle)) * inv_s + cy;
        let rotated_from = bilinear(false, rx, ry);
        result = lerp_pixel(rotated_from, to_px, p);
    } else if params.kind_id == 12u32 {
        // Light Leak: additive coloured glow peaking at midpoint.
        // param0 = intensity, color = leak colour (default warm amber).
        let leak_color = vec3!(
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
        let lx = (-0.2 + 1.4 * p) * f32c(params.width);
        let ly = 0.35 * f32c(params.height);
        let ex = (f32c(x) - lx) / (0.45 * f32c(params.width));
        let ey = (f32c(y) - ly) / (0.6 * f32c(params.height));
        let falloff = exp(-2.5 * (ex * ex + ey * ey));
        let add = leak_color * leak_t * falloff * 0.8;
        result = vec4!(
            min(base.x + add.x, 255.0),
            min(base.y + add.y, 255.0),
            min(base.z + add.z, 255.0),
            base.w,
        );
    } else if params.kind_id == 13u32 {
        // Luminance Dissolve: bright pixels of `from` dissolve first, dark pixels last.
        // param0 = softness (default 1.0).
        let from_norm = from_px.rgb() / 255.0;
        let luma = dot3(from_norm, vec3!(0.2126, 0.7152, 0.0722));
        let softness = max(params.param0 * 0.15, 0.01);
        // Each pixel turns over when the transition reaches 1 - its luma,
        // the progress stretched by the softness so the first starts at 0
        // and the last ends at 1: against the luma itself it began and
        // ended part way, a pop at both cuts (#2765).
        let start = 1.0 - luma;
        let at = p * (1.0 + 2.0 * softness) - softness;
        let t = smoothstep(start - softness, start + softness, at);
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 14u32 || params.kind_id == 15u32 {
        // Circle Wipe In (14) expands from center; Out (15) contracts to center.
        // param0 = softness, param1 = cx (0..1), param2 = cy (0..1)
        let cx = params.param1 * f32c(params.width);
        let cy = params.param2 * f32c(params.height);
        let mx = max(cx, f32c(params.width) - cx);
        let my = max(cy, f32c(params.height) - cy);
        let max_r = max(length(vec2!(mx, my)), 1.0);
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let dist = length(vec2!(dx, dy)) / max_r;
        let softness = max(params.param0 * 0.04, 0.001);
        let mut radius: f32 = Default::default();
        if params.kind_id == 14u32 {
            radius = p * (1.0 + 2.0 * softness) - softness;
        } else {
            radius = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
        }
        let s_val = smoothstep(radius - softness, radius + softness, dist);
        let mut t: f32 = Default::default();
        if params.kind_id == 14u32 {
            t = 1.0 - s_val;
        } else {
            t = s_val;
        }
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 16u32 {
        // Clock Wipe: angular sweep around frame center.
        // param0=softness, param1=start angle deg, param2=direction (0=CW, 1=CCW)
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let start_rad = params.param1 * 3.14159265 / 180.0;
        let two_pi = 6.28318530;
        let mut angle = atan2(dx, -dy) - start_rad;
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
    } else if params.kind_id == 17u32 || params.kind_id == 18u32 {
        // Barn Door Open (17) splits from center outward; Close (18) closes inward.
        // param0 = softness, param1 = orientation (0=horizontal, 1=vertical)
        let softness = max(params.param0 * 0.03, 0.001);
        let mut dist: f32 = Default::default();
        if params.param1 > 0.5 {
            let cy = f32c(params.height) * 0.5;
            dist = abs(f32c(y) - cy) / max(cy, 1.0);
        } else {
            let cx = f32c(params.width) * 0.5;
            dist = abs(f32c(x) - cx) / max(cx, 1.0);
        }
        let mut t_bd: f32 = Default::default();
        if params.kind_id == 17u32 {
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            t_bd = 1.0 - smoothstep(threshold - softness, threshold + softness, dist);
        } else {
            let threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
            t_bd = smoothstep(threshold - softness, threshold + softness, dist);
        }
        result = lerp_pixel(from_px, to_px, t_bd);
    } else if params.kind_id == 19u32 {
        // Pixelate: block size peaks at midpoint.
        // param0 = max block size, param1 = mode (0 = pixelate+dissolve, 1 = cross)
        let max_block = max(params.param0, 2.0);
        let triangle = 1.0 - abs(p * 2.0 - 1.0);
        let block_f = max(max_block * triangle, 1.0);
        let block = max(u32c(block_f), 1u32);
        let bx = min((x / block) * block + block / 2u32, params.width - 1u32);
        let by = min((y / block) * block + block / 2u32, params.height - 1u32);
        let block_idx = by * params.width + bx;
        let from_block = unpack(from_buf[block_idx]);
        if params.param1 > 0.5 {
            let to_block = unpack(to_buf[block_idx]);
            result = lerp_pixel(from_block, to_block, p);
        } else if p < 0.5 {
            result = from_block;
        } else {
            let t_px = (p - 0.5) * 2.0;
            result = lerp_pixel(from_block, to_px, t_px);
        }
    } else if params.kind_id == 20u32 {
        // Ripple: radial sine displacement, envelope peaks at midpoint.
        // param0=amplitude, param1=frequency, param2=speed
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let dist = length(vec2!(dx, dy));
        let amplitude = params.param0;
        let frequency = params.param1;
        let speed = params.param2;
        let envelope = sin(p * 3.14159265) * amplitude;
        let phase = dist * frequency * 0.1 - p * speed * 10.0;
        let ripple = sin(phase) * envelope;
        let mut nx: f32 = 0.0;
        let mut ny: f32 = 0.0;
        if dist > 0.1 {
            nx = dx / dist;
            ny = dy / dist;
        }
        let sx = clamp(f32c(x) + nx * ripple, 0.0, f32c(params.width) - 1.0);
        let sy = clamp(f32c(y) + ny * ripple, 0.0, f32c(params.height) - 1.0);
        let sample_idx = u32c(sy) * params.width + u32c(sx);
        let from_distorted = unpack(from_buf[sample_idx]);
        result = lerp_pixel(from_distorted, to_px, p);
    } else if params.kind_id == 21u32 || params.kind_id == 22u32 {
        // Page Turn (21) / Page Turn Reverse (22): peel from one side with fold shadow.
        // param0 = shadow intensity, param1 = direction (0=RTL, 1=LTR) for PageTurn.
        let shadow_intensity = params.param0;
        let mut fx: f32 = f32c(x);
        if params.kind_id == 22u32 || params.param1 > 0.5 {
            fx = f32c(params.width) - 1.0 - fx;
        }
        let norm_x = fx / f32c(params.width);
        let fold_x = 1.0 - p;
        let denom = max(p * 0.3, 0.01);
        if norm_x > fold_x {
            let shadow_dist = (norm_x - fold_x) / denom;
            let shadow = shadow_intensity * exp(-shadow_dist * 3.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4!(to_px.x * t, to_px.y * t, to_px.z * t, 255.0);
        } else {
            let shadow_dist = (fold_x - norm_x) / denom;
            let shadow = shadow_intensity * 0.5 * exp(-shadow_dist * 2.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4!(from_px.x * t, from_px.y * t, from_px.z * t, 255.0);
        }
    } else if params.kind_id == 23u32 {
        // Chromatic Aberration: R/B channels sampled with offset, envelope peaks at midpoint.
        // param0 = max offset px, param1 = direction (0=H, 1=V, 2=diag)
        let max_offset = params.param0;
        let dir = u32c(params.param1 + 0.5);
        let envelope = sin(p * 3.14159265) * max_offset;
        let mut r_ox: f32 = 0.0;
        let mut r_oy: f32 = 0.0;
        let mut b_ox: f32 = 0.0;
        let mut b_oy: f32 = 0.0;
        if dir == 1u32 {
            r_oy = -envelope;
            b_oy = envelope;
        } else if dir == 2u32 {
            r_ox = -envelope * 0.707;
            r_oy = -envelope * 0.707;
            b_ox = envelope * 0.707;
            b_oy = envelope * 0.707;
        } else {
            r_ox = -envelope;
            b_ox = envelope;
        }
        let base_g = lerp_pixel(from_px, to_px, p).y;
        let rx = clamp(f32c(x) + r_ox, 0.0, f32c(params.width) - 1.0);
        let ry = clamp(f32c(y) + r_oy, 0.0, f32c(params.height) - 1.0);
        let r_idx = u32c(ry) * params.width + u32c(rx);
        let r_val = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p).x;
        let bx = clamp(f32c(x) + b_ox, 0.0, f32c(params.width) - 1.0);
        let by = clamp(f32c(y) + b_oy, 0.0, f32c(params.height) - 1.0);
        let b_idx = u32c(by) * params.width + u32c(bx);
        let b_val = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p).z;
        result = vec4!(r_val, base_g, b_val, 255.0);
    } else if params.kind_id == 24u32 {
        // Film Burn (#511): see the shader.
        let temp = clamp(params.param0, 0.0, 1.0);
        let speed = max(params.param1, 0.1);
        let grain = params.param2;
        let pc = clamp(p, 0.0, 1.0);
        let env = max(sin(pc * 3.14159265), 0.0);
        let w = f32c(params.width);
        let h = f32c(params.height);
        let short_side = min(w, h);
        let travel = clamp(pc * speed, 0.0, 1.0);
        let cx = w * (1.1 - 0.7 * travel);
        let cy = h * (0.3 + 0.1 * travel);
        let radius = short_side * (0.3 + 0.85 * env);
        let fx = f32c(x);
        let fy = f32c(y);
        let n = burn_noise(fx / (short_side * 0.3), fy / (short_side * 0.3));
        let d = ((fx - cx) * (fx - cx) + (fy - cy) * (fy - cy)).sqrt() / radius + (n - 0.5) * 0.3;
        let heat = pow(clamp(1.0 - d, 0.0, 1.0), 1.4) * env;
        let mix3 = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        let warm = mix3([1.0, 0.9, 0.75], [1.0, 0.3, 0.05], temp);
        let col = mix3(warm, [1.0, 0.95, 0.85], smoothstep(0.7, 1.0, heat));
        let cut_t = smoothstep(0.35, 0.65, pc);
        let tint = min(heat * 1.5, 1.0);
        let base = [
            (from_px.x + (to_px.x - from_px.x) * cut_t) / 255.0 * (1.0 + (warm[0] - 1.0) * tint),
            (from_px.y + (to_px.y - from_px.y) * cut_t) / 255.0 * (1.0 + (warm[1] - 1.0) * tint),
            (from_px.z + (to_px.z - from_px.z) * cut_t) / 255.0 * (1.0 + (warm[2] - 1.0) * tint),
        ];
        let g = (rand3(x, y, u32c(pc * 1000.0)) - 0.5) * grain * 0.08 * heat;
        let ch = |i: usize| clamp(base[i] + col[i] * (heat * 1.2) + g, 0.0, 1.0);
        result = vec4!(ch(0) * 255.0, ch(1) * 255.0, ch(2) * 255.0, 255.0);
    } else if params.kind_id == 25u32 {
        // Star Wipe: N-pointed star expanding from center.
        // param0=softness, param1=point count, param2=inner radius ratio
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let points = max(params.param1, 3.0);
        let inner_ratio = clamp(params.param2, 0.2, 0.8);
        let softness = max(params.param0 * 0.03, 0.001);
        let dist = length(vec2!(dx, dy));
        let angle = atan2(dy, dx);
        let sector = 3.14159265 / points;
        let two_sector = 2.0 * sector;
        // WGSL has no float %, use floor-based modulo. Apply twice to normalize negatives.
        let mut sector_angle = angle - floor(angle / two_sector) * two_sector;
        sector_angle = sector_angle - floor(sector_angle / two_sector) * two_sector;
        let star_r_norm = mix(
            inner_ratio,
            1.0,
            smoothstep(0.0, sector, abs(sector_angle - sector)),
        );
        let max_r = max(length(vec2!(cx, cy)), 1.0);
        let norm_dist = dist / (max_r * max(star_r_norm, 0.01));
        let max_norm = 1.0 / inner_ratio;
        let threshold = p * (max_norm + 2.0 * softness) - softness;
        let s_val = smoothstep(threshold - softness, threshold + softness, norm_dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_val);
    } else if params.kind_id == 26u32 || params.kind_id == 27u32 {
        // Split Wipe H (26) / V (27): reveal `to` expanding from the center line outward.
        let softness = max(params.param0 * 0.03, 0.001);
        let mut dist: f32 = Default::default();
        if params.kind_id == 26u32 {
            let cx = f32c(params.width) * 0.5;
            dist = abs(f32c(x) - cx) / max(cx, 1.0);
        } else {
            let cy = f32c(params.height) * 0.5;
            dist = abs(f32c(y) - cy) / max(cy, 1.0);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_sp = smoothstep(threshold - softness, threshold + softness, dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_sp);
    } else if params.kind_id == 28u32 || params.kind_id == 29u32 {
        // Venetian Blind H (28) / V (29): N parallel strips wipe simultaneously.
        let softness = max(params.param0 * 0.04, 0.001);
        let blinds = max(params.param1, 1.0);
        let mut strip_pos: f32 = Default::default();
        if params.kind_id == 28u32 {
            strip_pos = fract(f32c(y) / f32c(params.height) * blinds);
        } else {
            strip_pos = fract(f32c(x) / f32c(params.width) * blinds);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_vb = smoothstep(threshold - softness, threshold + softness, strip_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_vb);
    } else if params.kind_id == 30u32 {
        // Checkerboard Wipe: alternating squares wipe LTR / RTL.
        // param0 = softness, param1 = grid size (squares per axis)
        let softness = max(params.param0 * 0.04, 0.001);
        let grid = max(params.param1, 1.0);
        let fx = f32c(x) / f32c(params.width) * grid;
        let fy = f32c(y) / f32c(params.height) * grid;
        let col = u32c(fx);
        let row = u32c(fy);
        let local_x = fract(fx);
        let mut local_pos: f32 = Default::default();
        if (col + row) % 2u32 == 0u32 {
            local_pos = local_x;
        } else {
            local_pos = 1.0 - local_x;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_cb = smoothstep(threshold - softness, threshold + softness, local_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_cb);
    } else if params.kind_id == 31u32 || params.kind_id == 32u32 {
        // Diagonal wipe TL-BR (31) / TR-BL (32).
        let softness = max(params.param0 * 0.03, 0.001);
        let nx = f32c(x) / f32c(params.width);
        let ny = f32c(y) / f32c(params.height);
        let mut diag: f32 = Default::default();
        if params.kind_id == 31u32 {
            diag = (nx + ny) * 0.5;
        } else {
            diag = ((1.0 - nx) + ny) * 0.5;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_diag = smoothstep(threshold - softness, threshold + softness, diag);
        result = lerp_pixel(from_px, to_px, 1.0 - s_diag);
    } else if params.kind_id == 33u32 || params.kind_id == 34u32 {
        // Box Wipe In (33) / Out (34): rectangle expanding from / contracting to center.
        // param0 = softness, param1 = square blend (0 = frame aspect, 1 = square)
        let softness = max(params.param0 * 0.03, 0.001);
        let square = clamp(params.param1, 0.0, 1.0);
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = abs(f32c(x) - cx) / max(cx, 1.0);
        let dy = abs(f32c(y) - cy) / max(cy, 1.0);
        let box_dist = max(dx, dy);
        let sq_dist = max(dx * cx / max(cy, 1.0), dy);
        let dist = box_dist + (sq_dist - box_dist) * square;
        let aspect = max(cx / max(cy, 1.0), 1.0);
        let max_norm = 1.0 + (aspect - 1.0) * square;
        let mut threshold: f32 = Default::default();
        if params.kind_id == 33u32 {
            threshold = p * (max_norm + 2.0 * softness) - softness;
        } else {
            threshold = (1.0 - p) * (max_norm + 2.0 * softness) - softness;
        }
        let s_box = smoothstep(threshold - softness, threshold + softness, dist);
        let mut t_box: f32 = Default::default();
        if params.kind_id == 33u32 {
            t_box = 1.0 - s_box;
        } else {
            t_box = s_box;
        }
        result = lerp_pixel(from_px, to_px, t_box);
    } else if params.kind_id >= 35u32 && params.kind_id <= 44u32 {
        // Slide / Cover / Uncover / Squeeze family — pixel-offset sampling.
        let wf = f32c(params.width);
        let hf = f32c(params.height);
        let wi = i32c(params.width);
        let hi = i32c(params.height);
        let offset_w = i32c(p * wf);
        let offset_h = i32c(p * hf);
        let mut sx: i32 = i32c(x);
        let mut sy: i32 = i32c(y);
        let mut use_to: bool = false;
        if params.kind_id == 35u32 {
            let to_x = i32c(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            } else {
                sx = clamp(i32c(x) - offset_w, 0, wi - 1);
            }
        } else if params.kind_id == 36u32 {
            let to_x = i32c(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            } else {
                sx = min(i32c(x) + offset_w, wi - 1);
            }
        } else if params.kind_id == 37u32 {
            let to_y = i32c(y) + hi - offset_h;
            if to_y < hi {
                use_to = true;
                sy = max(to_y, 0);
            } else {
                sy = clamp(i32c(y) - offset_h, 0, hi - 1);
            }
        } else if params.kind_id == 38u32 {
            let to_y = i32c(y) - hi + offset_h;
            if to_y >= 0 {
                use_to = true;
                sy = min(to_y, hi - 1);
            } else {
                sy = min(i32c(y) + offset_h, hi - 1);
            }
        } else if params.kind_id == 39u32 {
            let to_x = i32c(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            }
        } else if params.kind_id == 40u32 {
            let to_x = i32c(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            }
        } else if params.kind_id == 41u32 {
            let from_x = i32c(x) + offset_w;
            if from_x < wi {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 42u32 {
            let from_x = i32c(x) - offset_w;
            if from_x >= 0 {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 43u32 {
            // Squeeze H: proper collapse/expand with black outside.
            let half = wf * 0.5;
            let one_px = 1.0 / wf;
            let mut out_color: V4 = Default::default();
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32c(x) < vis_left || f32c(x) >= vis_right {
                        out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32c(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = unpack(from_buf[u32c(y) * params.width + u32c(src_x)]);
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32c(x) < vis_left || f32c(x) >= vis_right {
                        out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32c(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = unpack(to_buf[u32c(y) * params.width + u32c(src_x)]);
                    }
                }
            }
            result = out_color;
        } else if params.kind_id == 44u32 {
            // Squeeze V: proper collapse/expand with black outside.
            let half = hf * 0.5;
            let one_px = 1.0 / hf;
            let mut out_color: V4 = Default::default();
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32c(y) < vis_top || f32c(y) >= vis_bottom {
                        out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32c(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = unpack(from_buf[u32c(src_y) * params.width + u32c(x)]);
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32c(y) < vis_top || f32c(y) >= vis_bottom {
                        out_color = vec4!(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32c(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = unpack(to_buf[u32c(src_y) * params.width + u32c(x)]);
                    }
                }
            }
            result = out_color;
        }
        // Shared lookup for slide/cover/uncover (35–42).
        // Squeeze (43–44) already set result directly above.
        if params.kind_id <= 42u32 {
            let sidx = u32c(sy) * params.width + u32c(sx);
            if use_to {
                result = unpack(to_buf[sidx]);
            } else {
                result = unpack(from_buf[sidx]);
            }
        }
    } else if params.kind_id == 66u32 {
        // Water Drop: a ring spreads from the centre; the incoming
        // picture is revealed inside it, refracted at the crest, with
        // ripples trailing behind the front.
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let dist = length(vec2!(dx, dy));
        let half_diag = length(vec2!(cx, cy));
        let band = max(params.param0, 0.02) * half_diag;
        let front = -band * 0.5 + p * (half_diag * 1.05 + band);
        let s = dist - front;
        let env = sin(p * 3.14159265);
        let crest = exp(-(s * s) / (band * band * 0.18));
        let mut trail: f32 = 0.0;
        if s < 0.0 {
            trail = sin(s / band * 6.2831853 * max(params.param2, 0.0)) * exp(s / band);
        }
        let disp = (crest + trail * 0.5) * params.param1 * env;
        let mut nx: f32 = 0.0;
        let mut ny: f32 = 0.0;
        if dist > 0.1 {
            nx = dx / dist;
            ny = dy / dist;
        }
        let sx = clamp(f32c(x) + nx * disp, 0.0, f32c(params.width) - 1.0);
        let sy = clamp(f32c(y) + ny * disp, 0.0, f32c(params.height) - 1.0);
        let from_s = bilinear(false, sx, sy);
        let to_s = bilinear(true, sx, sy);
        let reveal = 1.0 - smoothstep(-band * 0.5, band * 0.5, s);
        let col = lerp_pixel(from_s, to_s, reveal);
        let glint = crest * params.param3 * env * 255.0;
        result = vec4!(min(col.x + glint, 255.0), min(col.y + glint, 255.0), min(col.z + glint, 255.0), col.w);
    } else if params.kind_id == 67u32 {
        // Whip Pan (#512): see the shader.
        let dir = u32c(params.param0 + 0.5);
        let horizontal = dir < 2u32;
        let mut len_axis: f32 = f32c(params.height);
        let mut a: f32 = f32c(y);
        if horizontal {
            len_axis = f32c(params.width);
            a = f32c(x);
        }
        let s = p * p * (3.0 - 2.0 * p);
        let speed = 6.0 * p * (1.0 - p);
        let blur = max(params.param1, 0.0) * speed * len_axis * 0.12;
        let mut sgn: f32 = 1.0;
        if dir == 1u32 || dir == 3u32 {
            sgn = -1.0;
        }
        let jitter = rand3(x, y, 11u32);
        let mut acc = vec4!(0.0, 0.0, 0.0, 0.0);
        for k in 0..24u32 {
            let off = ((f32c(k) + jitter) / 24.0 - 0.5) * blur;
            let u = a + sgn * s * len_axis + off;
            let mut to_side = false;
            let mut v: f32 = u;
            if sgn > 0.0 {
                if u >= len_axis {
                    to_side = true;
                    v = u - len_axis;
                }
            } else if u < 0.0 {
                to_side = true;
                v = u + len_axis;
            }
            v = clamp(v, 0.0, len_axis - 1.0);
            let mut sx: f32 = f32c(x);
            let mut sy: f32 = f32c(y);
            if horizontal {
                sx = v;
            } else {
                sy = v;
            }
            acc = acc + bilinear(to_side, sx, sy);
        }
        result = acc / 24.0;
    } else if params.kind_id == 68u32 {
        // Slide Strips (#985): see the shader.
        let bits = piece_bits(params);
        let w = f32c(params.width);
        let h = f32c(params.height);
        let n = clamp(round(params.param0), 1.0, 64.0);
        let ang = params.param1 * 0.017453292;
        let mx = cos(ang);
        let my = sin(ang);
        let qx = f32c(x) + 0.5 - w * 0.5;
        let qy = f32c(y) + 0.5 - h * 0.5;
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
        let mut sgn: f32 = 1.0;
        if (bits & 64u32) != 0u32 && (u32c(fi) & 1u32) == 1u32 {
            sgn = -1.0;
        }
        let mid = (n - 1.0) * 0.5;
        let e = piece_travel(params, piece_key(params, u32c(fi), fi / max(n - 1.0, 1.0), abs(fi - mid) / max(mid, 0.5)));
        let assemble = (bits & 32u32) != 0u32;
        let mut t: f32 = e;
        if assemble {
            t = 1.0 - e;
        }
        let su = u - sgn * t * lm;
        let gh = piece_gap(params, e);
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
    } else if params.kind_id == 69u32 {
        // Slide Panels (#985): see the shader.
        let bits = piece_bits(params);
        let w = f32c(params.width);
        let h = f32c(params.height);
        let rows = clamp(round(params.param0), 1.0, 16.0);
        let cols = clamp(round(rows * w / h), 1.0, 32.0);
        let q4 = round(params.param1 / 90.0);
        let quarter = u32c(q4 - 4.0 * floor(q4 / 4.0));
        let vertical = quarter == 1u32 || quarter == 3u32;
        let mut dir: f32 = 1.0;
        if quarter >= 2u32 {
            dir = -1.0;
        }
        let mut a: f32 = f32c(x) + 0.5;
        let mut b: f32 = f32c(y) + 0.5;
        let mut la: f32 = w;
        let mut lb: f32 = h;
        let mut na: f32 = cols;
        let mut nb: f32 = rows;
        if vertical {
            a = f32c(y) + 0.5;
            b = f32c(x) + 0.5;
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
        let mut sgn: f32 = dir;
        if (bits & 64u32) != 0u32 && (u32c(fr) & 1u32) == 1u32 {
            sgn = -dir;
        }
        let assemble = (bits & 32u32) != 0u32;
        let across = fr / max(nb - 1.0, 1.0);
        let mut best: f32 = -1.0;
        let mut best_a: f32 = 0.0;
        for j in 0..32u32 {
            let fj = f32c(j);
            if fj >= na {
                break;
            }
            let mut along = fj / max(na - 1.0, 1.0);
            if sgn > 0.0 {
                along = 1.0 - along;
            }
            let ca = (fj + 0.5) / na - 0.5;
            let cb = (fr + 0.5) / nb - 0.5;
            let centre = clamp(sqrt(ca * ca + cb * cb) / 0.7072, 0.0, 1.0);
            let e = piece_travel(params, piece_key(params, u32c(fr) * 32u32 + j, (along + across) * 0.5, centre));
            let mut t: f32 = e;
            if assemble {
                t = 1.0 - e;
            }
            let d = sgn * t * la;
            let gh = piece_gap(params, e);
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
            let mut sx: f32 = best_a;
            let mut sy: f32 = b;
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
    } else if params.kind_id == 70u32 {
        // Part Halves (#985): see the shader.
        let bits = piece_bits(params);
        let w = f32c(params.width);
        let h = f32c(params.height);
        let ang = params.param1 * 0.017453292;
        let lx = cos(ang);
        let ly = sin(ang);
        let qx = f32c(x) + 0.5 - w * 0.5;
        let qy = f32c(y) + 0.5 - h * 0.5;
        // The side, judged a hair off the pixel's centre, as the strips are.
        let cx = qx + 0.001953125;
        let cy = qy + 0.0029296875;
        let v = cy * lx - cx * ly;
        let mut side: f32 = 1.0;
        let mut i: u32 = 1u32;
        if v < 0.0 {
            side = -1.0;
            i = 0u32;
        }
        let mut dx: f32 = -ly * side;
        let mut dy: f32 = lx * side;
        let mut dist: f32 = (abs(w * ly) + abs(h * lx)) * 0.5 + 1.0;
        if params.param0 > 0.5 {
            dx = lx * side;
            dy = ly * side;
            dist = abs(w * lx) + abs(h * ly);
        }
        let e = piece_travel(params, piece_key(params, i, f32c(i), 0.0));
        let assemble = (bits & 32u32) != 0u32;
        let mut t: f32 = e;
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
        if sv >= piece_gap(params, e) && sxc >= 0.0 && sxc < w && syc >= 0.0 && syc < h {
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 63u32 || params.kind_id == 64u32 || params.kind_id == 65u32 {
        // Zoom family: scale from/to around frame center.
        let zoom_amount = max(params.param0, 1.001);
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let mut ep: f32 = p;
        if params.kind_id == 65u32 && params.param1 > 0.5 {
            ep = p * p * (3.0 - 2.0 * p);
        }
        let mut from_sample: V4 = Default::default();
        let mut to_sample: V4 = Default::default();
        if params.kind_id == 63u32 {
            let scale_a = 1.0 + (zoom_amount - 1.0) * ep;
            let from_sx = clamp(dx / scale_a + cx, 0.0, f32c(params.width) - 1.0);
            let from_sy = clamp(dy / scale_a + cy, 0.0, f32c(params.height) - 1.0);
            from_sample = bilinear(false, from_sx, from_sy);
            to_sample = to_px;
        } else if params.kind_id == 64u32 {
            // The incoming shot settles from close up to its own size, the
            // outgoing one left as it is: zoomed at the start, the cut
            // jumped (#2765).
            let scale_b = zoom_amount - (zoom_amount - 1.0) * ep;
            let to_sx = clamp(dx / scale_b + cx, 0.0, f32c(params.width) - 1.0);
            let to_sy = clamp(dy / scale_b + cy, 0.0, f32c(params.height) - 1.0);
            from_sample = from_px;
            to_sample = bilinear(true, to_sx, to_sy);
        } else {
            let scale_a = 1.0 + (zoom_amount - 1.0) * ep;
            let scale_b = zoom_amount - (zoom_amount - 1.0) * ep;
            let from_sx = clamp(dx / scale_a + cx, 0.0, f32c(params.width) - 1.0);
            let from_sy = clamp(dy / scale_a + cy, 0.0, f32c(params.height) - 1.0);
            let to_sx = clamp(dx / scale_b + cx, 0.0, f32c(params.width) - 1.0);
            let to_sy = clamp(dy / scale_b + cy, 0.0, f32c(params.height) - 1.0);
            from_sample = bilinear(false, from_sx, from_sy);
            to_sample = bilinear(true, to_sx, to_sy);
        }
        result = lerp_pixel(from_sample, to_sample, ep);
    } else if params.kind_id == 62u32 {
        // Bokeh Dissolve: screen-blend bloom + chromatic fringe.
        let bokeh_size = params.param0;
        let brightness_boost = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom_strength = luma * bokeh_size * envelope * brightness_boost * 2.0;
        let bloom_val = bloom_strength * 255.0;
        let screen_r = 255.0 - (255.0 - base.x) * (255.0 - bloom_val) / 255.0;
        let screen_g = 255.0 - (255.0 - base.y) * (255.0 - bloom_val) / 255.0;
        let screen_b = 255.0 - (255.0 - base.z) * (255.0 - bloom_val) / 255.0;
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = (f32c(x) - cx) / max(cx, 1.0);
        let dy = (f32c(y) - cy) / max(cy, 1.0);
        let radial = length(vec2!(dx, dy));
        let fringe = radial * bokeh_size * envelope * 8.0;
        result = vec4!(
            clamp(screen_r + fringe, 0.0, 255.0),
            clamp(screen_g, 0.0, 255.0),
            clamp(screen_b - fringe, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 60u32 {
        // Anamorphic Flare: horizontal streak across frame center.
        let intensity = params.param0;
        let streak_width = params.param1;
        let flare_color = vec3!(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        let ny = abs(f32c(y) / f32c(params.height) - 0.5) * 2.0;
        let streak_t = max(1.0 - ny / max(streak_width, 0.001), 0.0);
        let streak_t2 = streak_t * streak_t;
        let shimmer = 0.7 + 0.3 * sin(f32c(x) / f32c(params.width) * 3.14159265 * 8.0);
        let flare_strength = streak_t2 * envelope * shimmer;
        let flare = flare_color * flare_strength;
        result = vec4!(
            clamp(base.x + flare.x, 0.0, 255.0),
            clamp(base.y + flare.y, 0.0, 255.0),
            clamp(base.z + flare.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 61u32 {
        // Dream Sequence: bloom + warm glow + overexposure.
        let bloom_intensity = params.param0;
        let warmth = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom = luma * bloom_intensity * envelope * 60.0;
        let warm_r = warmth * envelope * 30.0;
        let warm_g = warmth * envelope * 15.0;
        let warm_b = -warmth * envelope * 10.0;
        let lift = envelope * bloom_intensity * 15.0;
        result = vec4!(
            clamp(base.x + bloom + warm_r + lift, 0.0, 255.0),
            clamp(base.y + bloom + warm_g + lift, 0.0, 255.0),
            clamp(base.z + bloom + warm_b + lift, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 58u32 {
        // Projector Flicker: brightness flicker + gate weave + frame jump.
        let flicker_intensity = params.param0;
        let weave_amount = params.param1;
        let envelope = sin(p * 3.14159265) * flicker_intensity;
        let frame_hash = rand3(p.to_bits(), 0u32, 3u32);
        let frame_hash2 = rand3(p.to_bits(), 0u32, 4u32);
        let frame_hash3 = rand3(p.to_bits(), 0u32, 5u32);
        let brightness = 1.0 + (frame_hash - 0.5) * envelope * 0.6;
        let weave = (frame_hash2 - 0.5) * weave_amount * envelope;
        let sx = clamp(f32c(x) + weave, 0.0, f32c(params.width) - 1.0);
        let mut jump: f32 = 0.0;
        if frame_hash3 < envelope * 0.1 {
            jump = (frame_hash3 - 0.5) * 8.0;
        }
        let sy = clamp(f32c(y) + jump, 0.0, f32c(params.height) - 1.0);
        let sample_idx = u32c(sy) * params.width + u32c(sx);
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
        let base = lerp_pixel(from_s, to_s, p);
        result = vec4!(
            clamp(base.x * brightness, 0.0, 255.0),
            clamp(base.y * brightness, 0.0, 255.0),
            clamp(base.z * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 59u32 {
        // Halation: per-pixel brightness-threshold additive glow. O(1) per pixel.
        let threshold = params.param0;
        let spread = params.param1;
        let halo_color = vec3!(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722)) / 255.0;
        let excess = max(luma - threshold, 0.0) / max(1.0 - threshold, 0.001);
        let halo_strength = excess * spread * envelope * 3.0;
        let halo = halo_color * halo_strength;
        result = vec4!(
            clamp(base.x + halo.x, 0.0, 255.0),
            clamp(base.y + halo.y, 0.0, 255.0),
            clamp(base.z + halo.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 57u32 {
        // Glitch RGB Split: bands of the picture with their red pushed one
        // way and their blue the other, the bands and their pushes jumping a
        // dozen times through the transition, strongest at the cut. Per
        // 8-pixel block, with blocks blacked out, it was black specks with
        // no split to see (#2765). param1 the band height's unit in pixels.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let band = u32c(f32c(y) / (block_size * 3.0));
        let step = u32c(p * 12.0);
        let on = select(0.0, 1.0, rand3(band, step, 6u32) > 0.35);
        let shift = (0.3 + 0.7 * rand3(band, step, 7u32)) * envelope * 0.04 * f32c(params.width) * on;
        let rx = clamp(f32c(x) + shift, 0.0, f32c(params.width) - 1.0);
        let bx = clamp(f32c(x) - shift, 0.0, f32c(params.width) - 1.0);
        let r_idx = u32c(y) * params.width + u32c(rx);
        let b_idx = u32c(y) * params.width + u32c(bx);
        let r_val = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p).x;
        let g_val = lerp_pixel(from_px, to_px, p).y;
        let b_val = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p).z;
        result = vec4!(r_val, g_val, b_val, 255.0);
    } else if params.kind_id == 55u32 {
        // Static TV: noise + scan lines + horizontal band shift.
        let intensity = params.param0;
        let line_h = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let hash = rand3(x + y * 65536u32, p.to_bits(), 9u32);
        let hash2 = rand3(x + y * 65536u32, p.to_bits(), 10u32);
        let mut scan_line: f32 = 1.0;
        if (u32c(f32c(y) / line_h) % 2u32) == 0u32 {
            scan_line = 0.75;
        }
        let static_val = hash * 255.0 * envelope * scan_line;
        let band = u32c(f32c(y) / line_h);
        let band_hash = rand3(band, p.to_bits(), 11u32);
        let shift = (band_hash - 0.5) * envelope * 20.0;
        let sx = clamp(f32c(x) + shift, 0.0, f32c(params.width) - 1.0);
        let sample_idx = u32c(y) * params.width + u32c(sx);
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
        let base = lerp_pixel(from_s, to_s, p);
        let static_mix = envelope * hash2 * 0.8;
        result = vec4!(
            clamp(base.x + (static_val - base.x) * static_mix, 0.0, 255.0),
            clamp(base.y + (static_val - base.y) * static_mix, 0.0, 255.0),
            clamp(base.z + (static_val - base.z) * static_mix, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 56u32 {
        // VHS Tracking: horizontal band shifts + chroma bleed.
        let intensity = params.param0;
        let band_count = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let band = u32c(f32c(y) / f32c(params.height) * band_count);
        let band_hash = rand3(band, p.to_bits(), 11u32);
        let band_hash2 = rand3(band, p.to_bits(), 12u32);
        let shift = (band_hash - 0.5) * envelope * 40.0;
        let sx = clamp(f32c(x) + shift, 0.0, f32c(params.width) - 1.0);
        let sample_idx = u32c(y) * params.width + u32c(sx);
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
        let base = lerp_pixel(from_s, to_s, p);
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722));
        let chroma_shift = (band_hash2 - 0.5) * envelope * 15.0;
        let csx = clamp(f32c(x) + chroma_shift, 0.0, f32c(params.width) - 1.0);
        let c_idx = u32c(y) * params.width + u32c(csx);
        let from_c = unpack(from_buf[c_idx]);
        let to_c = unpack(to_buf[c_idx]);
        let chroma_base = lerp_pixel(from_c, to_c, p);
        let vhs_r = mix(base.x, chroma_base.x, envelope * 0.5);
        let vhs_g = mix(base.y, luma, envelope * 0.5);
        let vhs_b = mix(base.z, chroma_base.z, envelope * 0.5);
        let brightness = 1.0 - band_hash * envelope * 0.4;
        result = vec4!(
            clamp(vhs_r * brightness, 0.0, 255.0),
            clamp(vhs_g * brightness, 0.0, 255.0),
            clamp(vhs_b * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 53u32 {
        // Swirl: rotational distortion spiraling from center.
        let cx = f32c(params.width) * 0.5;
        let cy = f32c(params.height) * 0.5;
        let dx = f32c(x) - cx;
        let dy = f32c(y) - cy;
        let dist = length(vec2!(dx, dy));
        let max_r = max(length(vec2!(cx, cy)), 1.0);
        let norm_dist = dist / max_r;
        let max_angle = params.param0 * 3.14159265 / 180.0;
        let falloff = params.param1;
        let envelope = sin(p * 3.14159265);
        let swirl_angle = max_angle * envelope * exp(-norm_dist * falloff);
        let cos_a = cos(swirl_angle);
        let sin_a = sin(swirl_angle);
        let sx = clamp(cos_a * dx - sin_a * dy + cx, 0.0, f32c(params.width) - 1.0);
        let sy = clamp(sin_a * dx + cos_a * dy + cy, 0.0, f32c(params.height) - 1.0);
        let sample_idx = u32c(sy) * params.width + u32c(sx);
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
        result = lerp_pixel(from_s, to_s, p);
    } else if params.kind_id == 54u32 {
        // Wave Distortion: sinusoidal displacement.
        let amplitude = params.param0;
        let frequency = params.param1;
        let dir = u32c(params.param2 + 0.5);
        let envelope = sin(p * 3.14159265) * amplitude;
        let mut off_x: f32 = 0.0;
        let mut off_y: f32 = 0.0;
        if dir == 0u32 {
            off_x = sin(f32c(y) / f32c(params.height) * frequency * 6.28318530) * envelope;
        } else if dir == 1u32 {
            off_y = sin(f32c(x) / f32c(params.width) * frequency * 6.28318530) * envelope;
        } else {
            off_x = sin(f32c(y) / f32c(params.height) * frequency * 6.28318530) * envelope * 0.7;
            off_y = sin(f32c(x) / f32c(params.width) * frequency * 6.28318530) * envelope * 0.7;
        }
        let sx = clamp(f32c(x) + off_x, 0.0, f32c(params.width) - 1.0);
        let sy = clamp(f32c(y) + off_y, 0.0, f32c(params.height) - 1.0);
        let from_s = bilinear(false, sx, sy);
        let to_s = bilinear(true, sx, sy);
        result = lerp_pixel(from_s, to_s, p);
    } else if params.kind_id == 51u32 {
        // Colorize Flash: user-defined color washes over at the cut.
        let flash_color = vec3!(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        let mut flash_t: f32 = Default::default();
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
        let coefs = vec3!(0.2126, 0.7152, 0.0722);
        let l = dot3(base.rgb() / 255.0, coefs);
        let cl = max(dot3(flash_color / 255.0, coefs), 0.05);
        let tinted = min(flash_color * (l / cl) * 1.4, vec3!(255.0));
        result = vec4!(
            mix(base.x, tinted.x, flash_t),
            mix(base.y, tinted.y, flash_t),
            mix(base.z, tinted.z, flash_t),
            255.0,
        );
    } else if params.kind_id == 52u32 {
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
        let mut h: f32 = 0.0;
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
        let mut h2 = h + hue_shift;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        // HSV → RGB
        let c = v * s;
        let h_sector = h2 / 60.0;
        let h_mod2 = h_sector - floor(h_sector / 2.0) * 2.0;
        let x2 = c * (1.0 - abs(h_mod2 - 1.0));
        let m = v - c;
        let hi = u32c(h_sector) % 6u32;
        let mut rgb: V3 = Default::default();
        if hi == 0u32 { rgb = vec3!(c, x2, 0.0); }
        else if hi == 1u32 { rgb = vec3!(x2, c, 0.0); }
        else if hi == 2u32 { rgb = vec3!(0.0, c, x2); }
        else if hi == 3u32 { rgb = vec3!(0.0, x2, c); }
        else if hi == 4u32 { rgb = vec3!(x2, 0.0, c); }
        else { rgb = vec3!(c, 0.0, x2); }
        result = vec4!((rgb + vec3!(m)) * 255.0, 255.0);
    } else if params.kind_id == 49u32 {
        // Negative Flash: brief inversion at the cut point.
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        let mut flash_t: f32 = Default::default();
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
        let negative = vec4!(255.0 - base.x, 255.0 - base.y, 255.0 - base.z, 255.0);
        result = lerp_pixel(base, negative, flash_t);
    } else if params.kind_id == 50u32 {
        // Sepia Transition: warm sepia tone through the cut.
        let strength = clamp(params.param0, 0.0, 1.0);
        let warmth = clamp(params.param1, 0.5, 2.0);
        let envelope = sin(p * 3.14159265) * strength;
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722));
        let sepia_r = clamp(luma * warmth * 1.2, 0.0, 255.0);
        let sepia_g = clamp(luma * warmth * 1.0, 0.0, 255.0);
        let sepia_b = clamp(luma * warmth * 0.75, 0.0, 255.0);
        let sepia = vec4!(sepia_r, sepia_g, sepia_b, 255.0);
        result = lerp_pixel(base, sepia, envelope);
    } else if params.kind_id == 47u32 {
        // Glitch Dissolve: block shifts + RGB split + noise.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let block_row = u32c(f32c(y) / block_size);
        let hash = rand3(block_row, p.to_bits(), 13u32);
        let hash2 = rand3(block_row, p.to_bits(), 14u32);
        let shift = (hash - 0.5) * envelope * 40.0;
        let sx_g = clamp(f32c(x) + shift, 0.0, f32c(params.width) - 1.0);
        let r_shift = shift * 0.5;
        let b_shift = -shift * 0.5;
        let rx = clamp(f32c(x) + r_shift, 0.0, f32c(params.width) - 1.0);
        let bx = clamp(f32c(x) + b_shift, 0.0, f32c(params.width) - 1.0);
        let g_idx = u32c(y) * params.width + u32c(sx_g);
        let r_idx = u32c(y) * params.width + u32c(rx);
        let b_idx = u32c(y) * params.width + u32c(bx);
        let base_g = lerp_pixel(unpack(from_buf[g_idx]), unpack(to_buf[g_idx]), p);
        let r_base = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p);
        let b_base = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p);
        let noise = (hash2 - 0.5) * envelope * 60.0;
        result = vec4!(
            clamp(r_base.x + noise, 0.0, 255.0),
            clamp(base_g.y + noise * 0.5, 0.0, 255.0),
            clamp(b_base.z + noise, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 48u32 {
        // Desaturate Transition: drain to grey at midpoint.
        let depth = clamp(params.param0, 0.0, 1.0);
        let envelope = sin(p * 3.14159265) * depth;
        let base = lerp_pixel(from_px, to_px, p);
        // Luma in 0-255 space.
        let luma = dot3(base.xyz(), vec3!(0.2126, 0.7152, 0.0722));
        let grey = vec4!(luma, luma, luma, 255.0);
        result = mix(base, grey, envelope);
    } else if params.kind_id == 45u32 {
        // Additive Dissolve: the incoming shot rises to full while the
        // outgoing one is still full, then that one falls; the two
        // screened together, so the middle is the brighter of both lifted
        // rather than half of white added over it, a wash (#2765). param0
        // the share of that over a plain dissolve.
        let intensity = clamp(params.param0, 0.0, 1.0);
        let wa = min(1.0, 2.0 * (1.0 - p));
        let wb = min(1.0, 2.0 * p);
        let a = clamp(from_px.rgb() / 255.0, vec3!(0.0), vec3!(1.0)) * wa;
        let b = clamp(to_px.rgb() / 255.0, vec3!(0.0), vec3!(1.0)) * wb;
        let screen = (vec3!(1.0) - (vec3!(1.0) - a) * (vec3!(1.0) - b)) * 255.0;
        let base = lerp_pixel(from_px, to_px, p);
        let rgb = mix(base.rgb(), screen, intensity);
        result = vec4!(rgb.x, rgb.y, rgb.z, 255.0);
    } else if params.kind_id == 46u32 {
        // Fade to Color: A → solid color → B. User-supplied sRGB → linear.
        let fade_color = vec4!(
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

    pack(result)
}
