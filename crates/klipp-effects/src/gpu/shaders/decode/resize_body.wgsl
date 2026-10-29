
// Lanczos-3 at distance x: sinc(x) sinc(x / 3) inside three pixels.
fn lanczos3(x: f32) -> f32 {
    let ax = abs(x);
    if (ax < 0.00001) { return 1.0; }
    if (ax >= 3.0) { return 0.0; }
    let px = 3.14159265358979 * ax;
    return 3.0 * sin(px) * sin(px / 3.0) / (px * px);
}

// Output pixel `fp` (a footprint under one source pixel on some side, an
// upscale) as linear working-space RGB: Lanczos-3 over the 6 x 6 source
// pixels round its centre, normalised, on their sRGB-encoded values (see
// `RESIZE_WGSL`'s doc), the ringing held to the encoded range.
fn resize_lanczos(fp: vec4<f32>) -> vec3<f32> {
    let cx = fp.x + fp.y * 0.5 - 0.5;
    let cy = fp.z + fp.w * 0.5 - 0.5;
    let ix = i32(floor(cx));
    let iy = i32(floor(cy));
    let max_x = i32(params.src_width) - 1;
    let max_y = i32(params.src_height) - 1;
    var acc = vec3<f32>(0.0);
    var total = 0.0;
    for (var j = -2; j <= 3; j = j + 1) {
        let wy = lanczos3(cy - f32(iy + j));
        let py = u32(clamp(iy + j, 0, max_y));
        for (var i = -2; i <= 3; i = i + 1) {
            let w = lanczos3(cx - f32(ix + i)) * wy;
            acc = acc + linear_to_srgb_vec(max(display_working_at(u32(clamp(ix + i, 0, max_x)), py), vec3<f32>(0.0))) * w;
            total = total + w;
        }
    }
    let e = clamp(acc / total, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(pow((e + 0.055) / 1.055, vec3<f32>(2.4)), e / 12.92, e <= vec3<f32>(0.04045));
}

// The source footprint of output pixel (dx, dy): x0, width, y0, height,
// in source pixels, through the rotation the nearest lookup uses.
fn resize_footprint(dx: u32, dy: u32) -> vec4<f32> {
    var a: u32; var a_n: u32; var b: u32; var b_n: u32;
    if (params.rotation == 0u) {
        a = dx; a_n = params.dst_width; b = dy; b_n = params.dst_height;
    } else if (params.rotation == 90u) {
        a = dy; a_n = params.dst_height; b = params.dst_width - 1u - dx; b_n = params.dst_width;
    } else if (params.rotation == 180u) {
        a = params.dst_width - 1u - dx; a_n = params.dst_width; b = params.dst_height - 1u - dy; b_n = params.dst_height;
    } else {
        a = params.dst_height - 1u - dy; a_n = params.dst_height; b = dx; b_n = params.dst_width;
    }
    let sw = f32(params.src_width) / f32(a_n);
    let sh = f32(params.src_height) / f32(b_n);
    return vec4<f32>(f32(a) * sw, sw, f32(b) * sh, sh);
}

// Samples to take across a footprint `span` source pixels wide.
fn resize_samples(span: f32) -> u32 {
    return clamp(u32(ceil(span - 0.001)), 1u, 8u);
}

// The source pixel under sample `i` of `n` across [x0, x0 + span).
fn resize_tap(x0: f32, span: f32, i: u32, n: u32, extent: u32) -> u32 {
    return min(u32(x0 + (f32(i) + 0.5) / f32(n) * span), extent - 1u);
}
