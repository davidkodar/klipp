
/// `HDR_REFERENCE_WHITE_NITS`: what display light 1.0 means.
const H_REF_WHITE_NITS: f32 = 203.0;
/// `HLG_NOMINAL_PEAK_NITS` and `HLG_GAMMA`.
const H_HLG_PEAK_NITS: f32 = 1000.0;
const H_HLG_GAMMA: f32 = 1.2;

/// `NITS_PER_LINEAR`: what scene-linear 1.0 means for a PQ source.
const H_NITS_PER_LINEAR: f32 = 100.0;
/// `LOG_HDR_EXPOSURE` and `LOG_HDR_KNEE`.
const H_LOG_HDR_EXPOSURE: f32 = 0.03102416;
const H_LOG_HDR_KNEE: f32 = 0.5;

/// `pq_reference_ootf`: BT.2100's reference OOTF — scene light (1.0 =
/// its 10 000-nit top) to display light, cd/m². The BT.709 OETF followed
/// by the BT.1886 EOTF.
fn h_pq_reference_ootf(e: f32) -> f32 {
    let x = 59.5208 * max(e, 0.0);
    var ep = 0.0;
    if x <= 0.018 {
        ep = 4.5 * x;
    } else {
        ep = 1.099 * pow(x, 0.45) - 0.099;
    }
    return 100.0 * pow(ep, 2.4);
}

/// The OOTF applied odd-symmetrically, so a BT.2020 colour outside the
/// working gamut arrives negative and survives the trip.
fn h_signed_ootf(c: f32) -> f32 {
    let m = h_pq_reference_ootf(abs(c) * H_LOG_HDR_EXPOSURE);
    return select(-m, m, c >= 0.0);
}

/// `roll_off`: compress above `knee` so the curve approaches `peak`
/// without reaching it, with slope 1 at the knee so there is no corner.
fn h_roll_off(x: f32, knee: f32, peak: f32) -> f32 {
    if x <= knee {
        return x;
    }
    let t = (x - knee) / (peak - knee);
    return knee + (peak - knee) * t / (1.0 + t);
}

/// `hdr_display_light`: a graded scene-linear pixel to HDR display light
/// (1.0 = 203 cd/m²). `kind` is `HdrTransfer::display_light_kind` — 0 PQ,
/// 1 HLG, 2 camera log.
///
/// A scene-referred clip's last step of its own, which used to happen on
/// the CPU frame after the grade and is why a real HDR export — one whose
/// *source* is PQ, HLG or log — could not take this path at all.
fn hdr_display_light(rgb: vec3<f32>, kind: u32, scene_scale: f32, peak: f32) -> vec3<f32> {
    var nits = vec3<f32>(0.0);
    if kind == 0u {
        nits = rgb * H_NITS_PER_LINEAR;
    } else if kind == 1u {
        // Undo the decoder's PQ-sized scale to get HLG scene light, then
        // the HLG OOTF. Channels stay signed for the same reason as below.
        let e = rgb / scene_scale;
        let ys = dot(h_working_luma(), e);
        var gain = 0.0;
        if ys > 0.0 {
            gain = H_HLG_PEAK_NITS * pow(ys, H_HLG_GAMMA - 1.0);
        }
        nits = e * gain;
    } else {
        let n = vec3<f32>(h_signed_ootf(rgb.x), h_signed_ootf(rgb.y), h_signed_ootf(rgb.z));
        // Roll off the brightest channel and scale the others with it, so
        // a highlight keeps its hue as it compresses.
        let m = max(n.x, max(n.y, n.z));
        let rolled = h_roll_off(m, H_LOG_HDR_KNEE * peak, peak);
        var k = 1.0;
        if m > 0.0 {
            k = rolled / m;
        }
        nits = n * k;
    }
    // PQ and HLG sources are display light already and are not rolled
    // off: the encoder holds the peak.
    return nits / H_REF_WHITE_NITS;
}
