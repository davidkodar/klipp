//#include srgb_components
//#include pixel_storage
//#include linear_helpers
//#include aces_rec709
//#include bt2020_gamut

fn h_working_luma() -> vec3<f32> {
    return vec3<f32>(tables[10u], tables[11u], tables[12u]);
}
//#include hdr_display_light
//#include hdr_encode

struct Params {
    width: u32,
    height: u32,
    // 0 none, 1 Reinhard, 2 Hable, and 3 means the ACES Rec.709
    // output transform instead of a tone map and an sRGB encode —
    // see `display_encode`. Anything else is refused by the host.
    tone_map_op: u32,
    // Word offset of the chroma. Planar: the Cb plane, with Cr one plane
    // further on. Semi-planar: the single interleaved UV plane.
    chroma_word_offset: u32,
    kr: f32,
    kg: f32,
    kb: f32,
    // 1 for NV12/P010's interleaved UV, 0 for separate Cb and Cr planes.
    semi_planar: u32,
    // 8 or 10. At 10 every sample is a u16 rather than a byte, so half
    // as many fit in a word.
    bits: u32,
    // Left shift on each stored sample: 6 for P010, which carries its
    // ten bits at the top of the u16, 0 everywhere else.
    sample_shift: u32,
    // 1 compresses BT.2020 into BT.709 before the tone map, reading the
    // reach tables at binding 3; 0 leaves the basis alone. Never 1 at
    // the same time as `tone_map_op == 3` — an ACES ODT maps its own
    // gamut, and the two would want the same binding.
    gamut_op: u32,
    // 0 an SDR delivery (the tone map and sRGB encode above), 1 PQ,
    // 2 HLG. An HDR delivery replaces the whole tail: the frame is
    // display light already, and `hdr_encode` takes it to the
    // delivery's signal. Reads binding 3, so likewise exclusive with
    // the other two.
    hdr_op: u32,
    // The master's peak, cd/m². PQ only; HLG's is the nominal 1000.
    peak_nits: f32,
    // A scene-referred source's own last step, before any of the above:
    // 0 none (the frame is already display-referred), 1 the SDR display
    // tone map, 2 HDR display light. `scene_kind` is
    // `HdrTransfer::display_light_kind` and `scene_scale` its
    // `scene_scale`; `scene_exposure` is `display_exposure`, for the
    // tone map.
    scene_op: u32,
    scene_kind: u32,
    scene_scale: f32,
    scene_exposure: f32,
    _pad2: u32,
    _pad3: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
// Whichever tables this delivery's display transform needs: ACES' curves
// and matrices for `tone_map_op == 3`, BT.2020's gamut reach and cusps
// for `gamut_op == 1`. A small dummy when it needs neither.
@group(0) @binding(3) var<storage, read> tables: array<f32>;
// One `(max, sum)` pair of cd/m² per workgroup, for a PQ delivery's
// MaxCLL and MaxFALL. The host reduces the pairs; a small dummy is bound
// when the delivery measures no light.
@group(0) @binding(4) var<storage, read_write> light: array<f32>;

/// Narkowicz ACES fit; constants shared with `klipp_color::hdr::filmic`.
/// The SDR display tone map a scene-referred clip ends with.
fn filmic(x: f32) -> f32 {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

fn hable_curve(x: f32) -> f32 {
    let a = 0.15; let b = 0.50; let c = 0.10;
    let d = 0.20; let e = 0.02; let f = 0.30;
    return ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f)) - e / f;
}

fn tone_map_c(v: f32) -> f32 {
    if params.tone_map_op == 1u { return v / (1.0 + v); }
    if params.tone_map_op == 2u {
        // hable_normalised: the exposure bias and the white point are
        // part of the curve, not the caller's business.
        return hable_curve(v * 2.0) / hable_curve(11.2);
    }
    return v;
}

/// One pixel through the display transform: the CPU twin's
/// `ExportDisplay::encode`, on whichever path the host selected.
///
/// `w` is the pixel's light in cd/m², which only a PQ delivery measures
/// (HDR10's MaxCLL and MaxFALL are made of it); 0 on every other path.
fn display_encode(px_in: vec3<f32>) -> vec4<f32> {
    // A scene-referred clip's own last step, ahead of the delivery's
    // transform: `Renderer::display_render`, which for an HDR delivery is
    // the display light and for an SDR one the filmic tone map.
    var px = px_in;
    if params.scene_op == 2u {
        px = hdr_display_light(px, params.scene_kind, params.scene_scale, params.peak_nits);
    } else if params.scene_op == 1u {
        // Negative first goes to 0, as the CPU's `hdr_display_tonemap`
        // and the viewer's tone map take it: a wide-gamut source's colours
        // outside the working space arrive negative, and this curve maps a
        // small negative to a positive (-0.1 to 0.21), a cast the CPU
        // never shows (#1280).
        let e = max(px * params.scene_exposure, vec3<f32>(0.0));
        px = vec3<f32>(filmic(e.x), filmic(e.y), filmic(e.z));
    }
    // An HDR delivery is a different tail entirely — the frame is display
    // light, not scene light, and there is no tone map or sRGB encode.
    if params.hdr_op != 0u {
        let e = hdr_encode(px, params.peak_nits, params.hdr_op);
        return vec4<f32>(clamp(e.xyz, vec3<f32>(0.0), vec3<f32>(1.0)), e.w);
    }
    // The ACES path is a whole output transform and ends
    // display-encoded already, so it returns before the sRGB encode.
    if params.tone_map_op == 3u {
        return vec4<f32>(acescg_to_rec709_sdr(px), 0.0);
    }
    // A wide-gamut basis comes into BT.709 first, gamut-compressing what
    // BT.709 cannot hold — `ToBt709::apply` in the twin, which likewise
    // runs before the negatives are clamped.
    var v = px;
    if params.gamut_op == 1u {
        v = bt2020_to_bt709_display(v);
    }
    // Negatives clamped before the tone map, as the twin does.
    let lin = max(v, vec3<f32>(0.0));
    let m = vec3<f32>(tone_map_c(lin.x), tone_map_c(lin.y), tone_map_c(lin.z));
    let e = vec3<f32>(
        linear_to_srgb_component(m.x),
        linear_to_srgb_component(m.y),
        linear_to_srgb_component(m.z),
    );
    return vec4<f32>(clamp(e, vec3<f32>(0.0), vec3<f32>(1.0)), 0.0);
}

/// `quant` from the CPU twin: offset and range are given in 8-bit
/// units and scaled to the output depth, rounded half up, clamped.
/// Then shifted into place — P010 puts its ten bits at the top of the
/// u16, which is what `sample_shift` carries.
fn quant(v: f32, off: f32, range: f32) -> u32 {
    let scale = f32(1u << (params.bits - 8u));
    let top = f32((1u << params.bits) - 1u);
    let q = u32(clamp((off + v * range) * scale + 0.5, 0.0, top));
    return q << params.sample_shift;
}

fn luma(e: vec3<f32>) -> f32 {
    return params.kr * e.x + params.kg * e.y + params.kb * e.z;
}

// A workgroup's partial `(max, sum)` of pixel light, one slot per
// invocation, reduced below.
var<workgroup> wg_max: array<f32, 64>;
var<workgroup> wg_sum: array<f32, 64>;

@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
) {
    let bx = gid.x;          // 8-pixel column block
    let by = gid.y;          // 2-pixel row block
    // Deliberately *not* an early return. The light reduction below
    // ends in `workgroupBarrier`, which every invocation of the
    // workgroup has to reach — a barrier behind a bounds check is
    // undefined in WGSL, and the last workgroup of a row is partly out
    // of range whenever `width / 8` is not a multiple of 8. So the
    // out-of-range invocations run, contribute nothing, and skip only
    // the writes.
    let in_frame = bx * 8u < params.width && by * 2u < params.height;

    let x0 = bx * 8u;
    let y0 = by * 2u;

    // The sixteen pixels of the block, display-encoded once and reused
    // for both luma and chroma.
    var e: array<vec3<f32>, 16>;
    var lmax = 0.0;
    var lsum = 0.0;
    if in_frame {
        for (var r = 0u; r < 2u; r = r + 1u) {
            for (var c = 0u; c < 8u; c = c + 1u) {
                let p = unpack(input[(y0 + r) * params.width + x0 + c]);
                let d = display_encode(p.xyz);
                e[r * 8u + c] = d.xyz;
                lmax = max(lmax, d.w);
                lsum = lsum + d.w;
            }
        }
    }

    // Reduce the block partials to one pair a workgroup. Gated on a
    // uniform value, so the barriers inside stay in uniform control
    // flow, and skipped entirely for a delivery that measures nothing.
    if params.hdr_op == 1u {
        wg_max[lidx] = lmax;
        wg_sum[lidx] = lsum;
        workgroupBarrier();
        for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
            if lidx < stride {
                wg_max[lidx] = max(wg_max[lidx], wg_max[lidx + stride]);
                wg_sum[lidx] = wg_sum[lidx] + wg_sum[lidx + stride];
            }
            workgroupBarrier();
        }
        if lidx == 0u {
            let slot = (wid.y * nwg.x + wid.x) * 2u;
            light[slot] = wg_max[0];
            light[slot + 1u] = wg_sum[0];
        }
    }

    // Past the last barrier, so leaving now is safe.
    if !in_frame { return; }

    // The block's sixteen luma samples and the four chroma sites its
    // 2x2 quads reduce to, quantised. Chroma is the mean of the
    // *display-encoded RGB* over each quad, then the difference — not
    // the mean of the chroma, which is a different number.
    var ys: array<u32, 16>;
    for (var i = 0u; i < 16u; i = i + 1u) {
        ys[i] = quant(luma(e[i]), 16.0, 219.0);
    }
    var cb: array<u32, 4>;
    var cr: array<u32, 4>;
    for (var q = 0u; q < 4u; q = q + 1u) {
        let acc = (e[q * 2u] + e[q * 2u + 1u] + e[8u + q * 2u] + e[8u + q * 2u + 1u]) * 0.25;
        let l = luma(acc);
        cb[q] = quant((acc.z - l) / (2.0 * (1.0 - params.kb)), 128.0, 224.0);
        cr[q] = quant((acc.x - l) / (2.0 * (1.0 - params.kr)), 128.0, 224.0);
    }

    // Packing, which is the only part the depth changes: a word holds
    // four 8-bit samples or two 16-bit ones. Every offset below is in
    // words and lands on a boundary because the caller guarantees the
    // width is a multiple of eight.
    if params.bits == 8u {
        for (var r = 0u; r < 2u; r = r + 1u) {
            let row_word = ((y0 + r) * params.width + x0) / 4u;
            for (var half = 0u; half < 2u; half = half + 1u) {
                var w = 0u;
                for (var k = 0u; k < 4u; k = k + 1u) {
                    w = w | (ys[r * 8u + half * 4u + k] << (k * 8u));
                }
                output[row_word + half] = w;
            }
        }
        if params.semi_planar == 1u {
            // NV12: one plane of Cb,Cr pairs, eight bytes for the four.
            let uv_word = (by * params.width + x0) / 4u;
            output[params.chroma_word_offset + uv_word] =
                cb[0] | (cr[0] << 8u) | (cb[1] << 16u) | (cr[1] << 24u);
            output[params.chroma_word_offset + uv_word + 1u] =
                cb[2] | (cr[2] << 8u) | (cb[3] << 16u) | (cr[3] << 24u);
        } else {
            // Separate planes, half as wide and half as tall: the four
            // samples are exactly one word.
            let c_word = (by * (params.width / 2u) + x0 / 2u) / 4u;
            let plane_words = (params.width / 2u) * (params.height / 2u) / 4u;
            output[params.chroma_word_offset + c_word] =
                cb[0] | (cb[1] << 8u) | (cb[2] << 16u) | (cb[3] << 24u);
            output[params.chroma_word_offset + plane_words + c_word] =
                cr[0] | (cr[1] << 8u) | (cr[2] << 16u) | (cr[3] << 24u);
        }
    } else {
        // Ten bits: u16 samples, so two to a word and twice the stride.
        for (var r = 0u; r < 2u; r = r + 1u) {
            let row_word = ((y0 + r) * params.width + x0) / 2u;
            for (var k = 0u; k < 4u; k = k + 1u) {
                let a = ys[r * 8u + k * 2u];
                let b = ys[r * 8u + k * 2u + 1u];
                output[row_word + k] = a | (b << 16u);
            }
        }
        if params.semi_planar == 1u {
            // P010: Cb,Cr pairs as u16, so a word per pair.
            let uv_word = (by * params.width + x0) / 2u;
            for (var q = 0u; q < 4u; q = q + 1u) {
                output[params.chroma_word_offset + uv_word + q] = cb[q] | (cr[q] << 16u);
            }
        } else {
            let c_word = (by * (params.width / 2u) + x0 / 2u) / 2u;
            let plane_words = (params.width / 2u) * (params.height / 2u) / 2u;
            output[params.chroma_word_offset + c_word] = cb[0] | (cb[1] << 16u);
            output[params.chroma_word_offset + c_word + 1u] = cb[2] | (cb[3] << 16u);
            output[params.chroma_word_offset + plane_words + c_word] = cr[0] | (cr[1] << 16u);
            output[params.chroma_word_offset + plane_words + c_word + 1u] = cr[2] | (cr[3] << 16u);
        }
    }
}
