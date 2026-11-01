
@group(0) @binding(0) var<storage, read> y_plane: array<u32>;
@group(0) @binding(1) var<storage, read> uv_plane: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;
// Packed RGB10 HDR code values (source primaries, still PQ/HLG-encoded).
// Written only for HDR transfers; bound to a dummy on SDR frames.
@group(0) @binding(4) var<storage, read_write> hdr_codes: array<u32>;
// Linear working-space f32, for a caller that is grading or rendering
// rather than showing eight bits. Written only when `write_linear` is
// set; bound to a dummy otherwise.
@group(0) @binding(5) var<storage, read_write> linear: array<vec4<f32>>;

struct Params {
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
    y_stride: u32,
    uv_stride: u32,
    rotation: u32,
    color_range: u32,
    matrix_id: u32,
    transfer_id: u32,
    primaries_id: u32,
    write_linear: u32,
    // Where the file's chroma sits: what each axis takes off s / 2, 0
    // co-sited or 0.25 between (`klipp_color::hdr::ChromaSiting`, #3266).
    chroma_off_x: f32,
    chroma_off_y: f32,
    _pad0: u32,
    _pad1: u32,
    primaries_row0: vec4<f32>,
    primaries_row1: vec4<f32>,
    primaries_row2: vec4<f32>,
}

// 16-bit LE sample at byte offset `byte_idx` from u32-packed arrays.
fn load_u16_le_y(byte_idx: u32) -> u32 {
    let word_idx = byte_idx / 4u;
    let byte_off = byte_idx % 4u;
    let w = y_plane[word_idx];
    if (byte_off <= 2u) {
        return (w >> (byte_off * 8u)) & 0xFFFFu;
    } else {
        let lo = (w >> 24u) & 0xFFu;
        let hi = y_plane[word_idx + 1u] & 0xFFu;
        return lo | (hi << 8u);
    }
}

fn load_u16_le_uv(byte_idx: u32) -> u32 {
    let word_idx = byte_idx / 4u;
    let byte_off = byte_idx % 4u;
    let w = uv_plane[word_idx];
    if (byte_off <= 2u) {
        return (w >> (byte_off * 8u)) & 0xFFFFu;
    } else {
        let lo = (w >> 24u) & 0xFFu;
        let hi = uv_plane[word_idx + 1u] & 0xFFu;
        return lo | (hi << 8u);
    }
}

// One source pixel of a display-referred frame, as linear working-space
// RGB: the same reads and stages as `main` below, for the downscale to
// average (#744).
fn display_working_at(sx: u32, sy: u32) -> vec3<f32> {
    let y_10 = f32(load_u16_le_y(sy * params.y_stride + sx * 2u) >> 6u);
    let cw = (params.src_width + 1u) / 2u;
    let ch = (params.src_height + 1u) / 2u;
    let fx = clamp(f32(sx) * 0.5 - params.chroma_off_x, 0.0, f32(cw - 1u));
    let fy = clamp(f32(sy) * 0.5 - params.chroma_off_y, 0.0, f32(ch - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, cw - 1u);
    let y1 = min(y0 + 1u, ch - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    var u_acc = 0.0;
    var v_acc = 0.0;
    for (var j = 0u; j < 2u; j = j + 1u) {
        let cy = select(y0, y1, j == 1u);
        let wy = select(1.0 - ty, ty, j == 1u);
        for (var i = 0u; i < 2u; i = i + 1u) {
            let cx = select(x0, x1, i == 1u);
            let w = select(1.0 - tx, tx, i == 1u) * wy;
            let ub = cy * params.uv_stride + cx * 4u;
            u_acc = u_acc + f32(load_u16_le_uv(ub) >> 6u) * w;
            v_acc = v_acc + f32(load_u16_le_uv(ub + 2u) >> 6u) * w;
        }
    }
    let yuv = yuv_normalise(y_10, u_acc, v_acc, params.color_range, 1023.0);
    let rgb_linear = display_to_linear(yuv_to_rgb_display(yuv, params.matrix_id), params.transfer_id);
    return vec3<f32>(
        dot(params.primaries_row0.xyz, rgb_linear),
        dot(params.primaries_row1.xyz, rgb_linear),
        dot(params.primaries_row2.xyz, rgb_linear),
    );
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = gid.y;
    if (dx >= params.dst_width || dy >= params.dst_height) { return; }

    // A downscale of a display-referred frame averages the footprint in
    // linear light (#744); see `nv12_gpu::RESIZE_WGSL`. A scene-referred
    // one keeps the nearest lookup below, which its CPU twin matches.
    let scene_referred = params.transfer_id == TRANSFER_PQ || params.transfer_id == TRANSFER_HLG || params.transfer_id >= 100u;
    if (!scene_referred) {
        let fp = resize_footprint(dx, dy);
        let nx = resize_samples(fp.y);
        let ny = resize_samples(fp.w);
        // Shrinking on a side averages; growing is Lanczos-3 (#2557).
        let upscale = nx == 1u && ny == 1u && (fp.y < 0.999 || fp.w < 0.999);
        if (nx > 1u || ny > 1u || upscale) {
            var working: vec3<f32>;
            if (upscale) {
                working = resize_lanczos(fp);
            } else {
                var acc = vec3<f32>(0.0);
                for (var j = 0u; j < ny; j = j + 1u) {
                    let py = resize_tap(fp.z, fp.w, j, ny, params.src_height);
                    for (var i = 0u; i < nx; i = i + 1u) {
                        acc = acc + display_working_at(resize_tap(fp.x, fp.y, i, nx, params.src_width), py);
                    }
                }
                working = acc / f32(nx * ny);
            }
            if params.write_linear != 0u {
                linear[dy * params.dst_width + dx] = pack(vec4<f32>(working, 1.0));
                return;
            }
            let srgb = linear_to_srgb_vec(working);
            let r = u32(clamp(srgb.x * 255.0 + 0.5, 0.0, 255.0));
            let g = u32(clamp(srgb.y * 255.0 + 0.5, 0.0, 255.0));
            let b = u32(clamp(srgb.z * 255.0 + 0.5, 0.0, 255.0));
            output[dy * params.dst_width + dx] = r | (g << 8u) | (b << 16u) | (255u << 24u);
            return;
        }
    }

    // Source coord (rotation + nearest-neighbour resize).
    var sx: u32;
    var sy: u32;
    if (params.rotation == 0u) {
        sx = dx * params.src_width / params.dst_width;
        sy = dy * params.src_height / params.dst_height;
    } else if (params.rotation == 90u) {
        sx = dy * params.src_width / params.dst_height;
        sy = (params.dst_width - 1u - dx) * params.src_height / params.dst_width;
    } else if (params.rotation == 180u) {
        sx = (params.dst_width - 1u - dx) * params.src_width / params.dst_width;
        sy = (params.dst_height - 1u - dy) * params.src_height / params.dst_height;
    } else { // 270
        sx = (params.dst_height - 1u - dy) * params.src_width / params.dst_height;
        sy = dx * params.src_height / params.dst_width;
    }
    sx = min(sx, params.src_width - 1u);
    sy = min(sy, params.src_height - 1u);

    // P010 layout: Y plane u16/pixel (10 valid bits in upper bits),
    // UV plane interleaved u16 pairs at half resolution. Strides
    // are in bytes.
    let y_byte_idx = sy * params.y_stride + sx * 2u;
    let y_word16 = load_u16_le_y(y_byte_idx);
    // Upper 10 bits hold the sample; lower 6 are padding zeros.
    let y_10 = f32(y_word16 >> 6u);

    // Chroma is reconstructed, not repeated — the same sited bilinear as
    // `YuvSamples::chroma_at` and the NV12 shader. P010 is 4:2:0 with
    // H.264 `chroma_sample_loc_type = 0` siting: co-sited horizontally,
    // centred between luma rows, hence the quarter-sample vertical
    // offset and none horizontally. `sw_decode_parity` holds this
    // identical to the CPU twin.
    let cw = (params.src_width + 1u) / 2u;
    let ch = (params.src_height + 1u) / 2u;
    let fx = clamp(f32(sx) * 0.5 - params.chroma_off_x, 0.0, f32(cw - 1u));
    let fy = clamp(f32(sy) * 0.5 - params.chroma_off_y, 0.0, f32(ch - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, cw - 1u);
    let y1 = min(y0 + 1u, ch - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    var u_acc = 0.0;
    var v_acc = 0.0;
    for (var j = 0u; j < 2u; j = j + 1u) {
        let cy = select(y0, y1, j == 1u);
        let wy = select(1.0 - ty, ty, j == 1u);
        for (var i = 0u; i < 2u; i = i + 1u) {
            let cx = select(x0, x1, i == 1u);
            let wx = select(1.0 - tx, tx, i == 1u);
            let w = wx * wy;
            // Interleaved u16 pairs: four bytes per chroma column.
            let ub = cy * params.uv_stride + cx * 4u;
            u_acc = u_acc + f32(load_u16_le_uv(ub) >> 6u) * w;
            v_acc = v_acc + f32(load_u16_le_uv(ub + 2u) >> 6u) * w;
        }
    }
    let u_10 = u_acc;
    let v_10 = v_acc;

    // Five-stage colourspace pipeline (HDR adds the tone-map step):
    //   1. Range-aware normalise (limited 64/940 vs full 0/1023).
    //   2. YUV → display-encoded RGB via the source matrix.
    //   3. Inverse EOTF → linear (in source primaries; cd/m² for PQ).
    //   4. (HDR only) ACES filmic tone-map scene-linear → SDR linear.
    //   5. Source primaries → working space via the uniform matrix.
    // Then re-encode to sRGB-u8 for the existing CPU/GPU hand-off.
    let yuv = yuv_normalise(y_10, u_10, v_10, params.color_range, 1023.0);
    let rgb_display = yuv_to_rgb_display(yuv, params.matrix_id);
    var rgb_srgb: vec3<f32>;
    // The linear working-space value the u8 hand-off below throws its
    // precision away on. For a scene-referred source this is
    // scene-linear and *not* tone-mapped: the tone map is a display step,
    // and a grade wants the scene.
    var rgb_working_linear: vec3<f32>;
    let is_log = params.transfer_id >= 100u;
    if (params.transfer_id == TRANSFER_PQ || params.transfer_id == TRANSFER_HLG || is_log) {
        // Camera log curves are published over CV / 1023, the raw 10-bit
        // code; `yuv_normalise` has mapped a limited-range file's 64..940
        // onto 0..1. Undo that for logs, or S-Log3's white reads ~0.3
        // stops hot. (PQ/HLG are defined on the legal-normalised signal,
        // so they keep it.)
        var signal = rgb_display;
        if (is_log && params.color_range == 0u) {
            signal = (vec3<f32>(64.0) + signal * 876.0) / 1023.0;
        }
        // Clamp to the code range, after the remap: camera logs record
        // real highlights above legal white (S-Log3 940 is ~18x grey, 1023
        // ~38x), so a legal-range clamp would flatten the top stop.
        // PQ/HLG signal is legal-range, so for them this is the same clamp.
        signal = clamp(signal, vec3<f32>(0.0), vec3<f32>(1.0));
        // Quantise to the 10-bit codes the grade will read, then build
        // the preview from those same codes so the two cannot diverge.
        let code = vec3<u32>(signal * 1023.0 + vec3<f32>(0.5));
        hdr_codes[dy * params.dst_width + dx] = code.x | (code.y << 10u) | (code.z << 20u);
        // Scene-linear: PQ/HLG curves return 1.0 = 10 000 nits (x100 to
        // 1.0 = 100 nits); camera logs already return reflectance.
        var scale = 100.0;
        var exposure = HDR_DISPLAY_EXPOSURE;
        if (is_log) {
            scale = 1.0;
            exposure = LOG_DISPLAY_EXPOSURE;
        }
        let scene = display_to_linear(vec3<f32>(code) / 1023.0, params.transfer_id) * scale;
        let working = vec3<f32>(
            dot(params.primaries_row0.xyz, scene),
            dot(params.primaries_row1.xyz, scene),
            dot(params.primaries_row2.xyz, scene),
        );
        rgb_working_linear = working;
        rgb_srgb = linear_to_srgb_vec(tone_map_hdr_vec(working, exposure));
    } else {
        let rgb_linear = display_to_linear(rgb_display, params.transfer_id);
        let rgb_working = vec3<f32>(
            dot(params.primaries_row0.xyz, rgb_linear),
            dot(params.primaries_row1.xyz, rgb_linear),
            dot(params.primaries_row2.xyz, rgb_linear),
        );
        rgb_working_linear = rgb_working;
        rgb_srgb = linear_to_srgb_vec(rgb_working);
    }

    // Full precision for a caller that is grading or rendering; the
    // sRGB-u8 hand-off below only for one that is showing eight bits.
    if params.write_linear != 0u {
        linear[dy * params.dst_width + dx] = pack(vec4<f32>(rgb_working_linear, 1.0));
        return;
    }

    let r_b = u32(clamp(rgb_srgb.x * 255.0 + 0.5, 0.0, 255.0));
    let g_b = u32(clamp(rgb_srgb.y * 255.0 + 0.5, 0.0, 255.0));
    let b_b = u32(clamp(rgb_srgb.z * 255.0 + 0.5, 0.0, 255.0));
    let pixel = r_b | (g_b << 8u) | (b_b << 16u) | (255u << 24u);
    output[dy * params.dst_width + dx] = pixel;
}
