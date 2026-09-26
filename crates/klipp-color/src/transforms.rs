//! Auto white balance, and the camera-log decode the input-device
//! transform is built from.
//!
//! The grade itself lives in [`crate::linear`] (CPU, linear f32) and
//! `klipp_effects::gpu`'s shaders; the camera-log curves live in
//! [`crate::camera_log`].

/// Auto white balance: the temperature and tint that make the frame's
/// neutral surfaces neutral, or why it has none to act on.
///
/// `rgba` is a displayed frame (sRGB bytes). The reference is
/// [`crate::measure::balance_reference`] — the walls, paper and road
/// under the scene's light, never skin or sand — and the slider values come from
/// [`crate::white_balance::white_balance_to_neutralise`], solved against
/// the balance the grade actually applies.
///
/// It used to average the whole frame (grey world). On a street of pink
/// brick under a blue sky that "neutralised" the brick and the sky, and
/// pushed the balance a long way toward magenta: the neutrals came out
/// further from grey than they went in.
///
/// `faces` are the framing analysis's face regions, if any (see
/// [`crate::measure::balance_reference`]); the error says why nothing
/// was balanced.
pub fn auto_white_balance(
    rgba: &[u8],
    width: u32,
    height: u32,
    faces: &[[f32; 4]],
) -> Result<(f32, f32), crate::measure::NoReference> {
    let reference = crate::measure::balance_reference(rgba, width, height, faces)?;
    Ok(crate::white_balance::white_balance_to_neutralise(reference, klipp_core::WorkingColorspace::Rec709Linear))
}

/// Camera log / HDR transfer → scene-linear, from a normalised 10-bit code
/// value. Delegates to `camera_log::log_format_to_linear`: this used to be
/// a second copy of every curve, wrong for most of them (S-Log3 read 0.17
/// at mid grey, Canon Log 3 went negative), and disagreeing with the
/// decoder's copy by up to 23 stops.
pub fn log_to_linear(v: f32, format: klipp_core::LogFormat) -> f32 {
    crate::camera_log::log_format_to_linear(v, format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::NoReference::{LeansAgainstTheFrame, NothingGrey};
    use crate::transfer::linear_to_srgb;

    /// Auto white balance on a frame with a blue cast asks for a warmer
    /// balance, and the values it returns really do neutralise the
    /// wall — they are solved against the balance the grade applies,
    /// not scaled from the channel differences.
    #[test]
    fn auto_white_balance_neutralises_a_cast() {
        // A grey wall under a bluish light, beside a red sofa.
        let wall = [118u8, 124, 138];
        let px: Vec<u8> = (0..400).flat_map(|i| if i < 300 { [wall[0], wall[1], wall[2], 255] } else { [170, 40, 45, 255] }).collect();
        let (temperature, tint) = auto_white_balance(&px, 20, 20, &[]).expect("a grey wall under a blue light is a white reference");
        assert!(temperature > 2.0, "a blue cast should ask for warmth, got {temperature}");
        let wall = wall.map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0));
        let m = crate::white_balance::white_balance_matrix(temperature, tint, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * wall[0] + row[1] * wall[1] + row[2] * wall[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.02, "still a cast: {out:?}");
        // A neutral frame needs no correction.
        let grey: Vec<u8> = (0..400).flat_map(|_| [128u8, 128, 128, 255]).collect();
        let (t, g) = auto_white_balance(&grey, 20, 20, &[]).expect("a grey frame is its own reference");
        assert!(t.abs() < 0.5 && g.abs() < 0.5, "a neutral frame asked for ({t}, {g})");
    }

    /// #403: a frame of one clear colour is not evidence of a cast. A
    /// blue-grey wall and a blue-grey card look the same; balancing on
    /// either would grey whatever it is.
    #[test]
    fn a_frame_of_one_colour_is_left_alone() {
        let px: Vec<u8> = (0..400).flat_map(|_| [110u8, 120, 150, 255]).collect();
        assert_eq!(auto_white_balance(&px, 20, 20, &[]), Err(NothingGrey));
    }

    /// #403: a pale face against a saturated backdrop, and a beach under
    /// a blue sky. The skin and the sand were the least-coloured quarter
    /// and one press greyed them.
    #[test]
    fn skin_and_sand_are_not_a_white_reference() {
        let portrait: Vec<u8> = (0..400).flat_map(|i| if i < 160 { [225u8, 190, 170, 255] } else { [30, 90, 170, 255] }).collect();
        assert_eq!(auto_white_balance(&portrait, 20, 20, &[]), Err(NothingGrey), "the face was greyed");
        let beach: Vec<u8> = (0..400).flat_map(|i| if i < 200 { [215u8, 196, 160, 255] } else { [70, 140, 220, 255] }).collect();
        assert_eq!(auto_white_balance(&beach, 20, 20, &[]), Err(NothingGrey), "the sand was greyed");
        // With a small true grey in the shot, that is the reference, and
        // it is already grey.
        let w = 100u32;
        let with_card: Vec<u8> = (0..w * w)
            .flat_map(|i| if i % 100 < 5 { [128u8, 128, 128, 255] } else if i % 100 < 60 { [225, 190, 170, 255] } else { [30, 90, 170, 255] })
            .collect();
        let (t, g) = auto_white_balance(&with_card, w, w, &[]).expect("the card is a white reference");
        assert!(t.abs() < 0.5 && g.abs() < 0.5, "balanced on the skin, not the card: ({t}, {g})");
    }

    /// #403: under a strong tungsten cast the grey wall is too orange
    /// to be read as grey, and a small pair of jeans lands near neutral,
    /// a little blue. Balancing on the jeans warmed an orange frame by
    /// 18; the press is refused instead, as it is with no jeans at all.
    #[test]
    fn a_strong_cast_is_not_guessed_at() {
        let w = 100u32;
        let wall = [168u8, 126, 86];
        let lamp_only: Vec<u8> = (0..w * w).flat_map(|i| if i % 100 < 70 { [wall[0], wall[1], wall[2], 255] } else { [60, 40, 30, 255] }).collect();
        assert_eq!(auto_white_balance(&lamp_only, w, w, &[]), Err(NothingGrey), "the tungsten wall was taken for grey");
        let with_jeans: Vec<u8> = (0..w * w)
            .flat_map(|i| if i % 100 < 4 { [96u8, 98, 104, 255] } else if i % 100 < 74 { [wall[0], wall[1], wall[2], 255] } else { [60, 40, 30, 255] })
            .collect();
        assert_eq!(auto_white_balance(&with_jeans, w, w, &[]), Err(LeansAgainstTheFrame), "the jeans set the balance");
        // Reporting still names what the jeans say: the reading is not
        // what refuses, acting on it is.
        assert!(crate::measure::neutral_reference(&with_jeans, w, w).is_some());
    }

    /// #318: a street of pink brick under a blue sky, with a strip of
    /// slightly warm grey wall. Grey world "neutralised" the brick and
    /// sky; reading the wall asks for a small cooling, and nothing like
    /// the magenta push the whole-frame average gave.
    #[test]
    fn auto_white_balance_reads_the_wall_not_the_sky_and_brick() {
        let (w, h) = (100u32, 100u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for _ in 0..w {
                let c: [u8; 3] = if y < 45 { [90, 150, 230] } else if y < 60 { [150, 146, 138] } else { [190, 110, 110] };
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let (temperature, tint) = auto_white_balance(&px, w, h, &[]).expect("the wall is a white reference");
        assert!(temperature < 0.0, "a warm wall asks for a cooler balance, got {temperature}");
        assert!(tint.abs() < 8.0, "no magenta push from the brick: tint {tint}");
        // And the answer does what it is for: the wall comes out grey.
        let wall = [150f32, 146.0, 138.0].map(|v| crate::transfer::srgb_to_linear(v / 255.0));
        let m = crate::white_balance::white_balance_matrix(temperature, tint, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * wall[0] + row[1] * wall[1] + row[2] * wall[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.02, "the wall is still not neutral: {out:?}");
    }

    /// A frame with nothing neutral in it — grass against sky — has no
    /// white reference, and auto white balance leaves the grade alone.
    #[test]
    fn auto_white_balance_declines_a_frame_with_no_neutrals() {
        let px: Vec<u8> = (0..400).flat_map(|i| if i < 200 { [60u8, 150, 50, 255] } else { [80, 140, 235, 255] }).collect();
        assert_eq!(auto_white_balance(&px, 20, 20, &[]), Err(NothingGrey));
    }

    /// #405: a warm grey room with one bright, cool-white door. The door
    /// is a sliver of the neutrals but, averaged in linear light, it set
    /// the balance; the median neutral is the room's.
    #[test]
    fn a_bright_sliver_does_not_set_the_balance() {
        let (w, h) = (100u32, 100u32);
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| if i % 100 < 25 { [206u8, 211, 222, 255] } else { [120, 113, 103, 255] })
            .collect();
        let (temperature, tint) = auto_white_balance(&px, w, h, &[]).expect("a white reference");
        // The room is warm, so the balance cools it; the door alone would
        // have warmed it.
        assert!(temperature < 0.0, "the door set the balance: {temperature}");
        let room = [120f32, 113.0, 103.0].map(|v| crate::transfer::srgb_to_linear(v / 255.0));
        let m = crate::white_balance::white_balance_matrix(temperature, tint, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * room[0] + row[1] * room[1] + row[2] * room[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.02, "the room is not neutral after one press: {out:?}");
    }

    /// Review of #428: a close-up face under a cool light, a white
    /// shirt, a saturated backdrop. The face is the frame's least-
    /// coloured surface and the shirt leans against it, the shape of the
    /// jeans against a tungsten wall. With the framing analysis's face
    /// region the face is not the surface and the shirt is the reference;
    /// without it colour cannot tell, and the press is refused.
    #[test]
    fn a_cool_cast_portrait_balances_on_the_shirt_once_the_face_is_known() {
        let w = 100u32;
        let shirt = [163u8, 168, 178];
        // Columns: shirt 0–5, face 6–65, backdrop the rest.
        let px: Vec<u8> = (0..w * w)
            .flat_map(|i| match i % 100 {
                0..=5 => [shirt[0], shirt[1], shirt[2], 255],
                6..=65 => [196, 165, 150, 255],
                _ => [30, 110, 60, 255],
            })
            .collect();
        assert_eq!(auto_white_balance(&px, w, w, &[]), Err(LeansAgainstTheFrame));
        let face = [0.06, 0.0, 0.60, 1.0];
        let (temperature, tint) = auto_white_balance(&px, w, w, &[face]).expect("the shirt is a white reference");
        assert!(temperature > 5.0, "a cool cast asks for warmth, got {temperature}");
        let shirt = shirt.map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0));
        let m = crate::white_balance::white_balance_matrix(temperature, tint, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * shirt[0] + row[1] * shirt[1] + row[2] * shirt[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.02, "the shirt is not neutral after one press: {out:?}");
    }

    /// Review of #428: the jeans are refused against a wall under a mild
    /// cast (chroma about 16) and under a very strong one (about 50) as
    /// they are against the chroma 30 one. A threshold on the wall's
    /// colour let the first through; a cap on how coloured a surface
    /// could be, the second.
    #[test]
    fn the_jeans_are_refused_under_a_mild_cast_and_a_very_strong_one() {
        let w = 100u32;
        for wall in [[153u8, 128, 106], [215, 135, 65]] {
            let px: Vec<u8> = (0..w * w)
                .flat_map(|i| if i % 100 < 4 { [96u8, 98, 104, 255] } else if i % 100 < 74 { [wall[0], wall[1], wall[2], 255] } else { [60, 40, 30, 255] })
                .collect();
            assert_eq!(auto_white_balance(&px, w, w, &[]), Err(LeansAgainstTheFrame), "wall {wall:?}: the jeans set the balance");
        }
    }

    /// The eyedropper: a grey card under a blue light, picked, gives the
    /// balance that makes the card grey; the picked point is a patch, so
    /// one odd pixel under the cursor does not set it.
    #[test]
    fn a_picked_grey_card_is_balanced_to_grey() {
        let (w, h) = (40u32, 40u32);
        let card = [118u8, 124, 138];
        let mut px: Vec<u8> = (0..w * h).flat_map(|i| if i % w < 20 { [card[0], card[1], card[2], 255] } else { [170, 40, 45, 255] }).collect();
        // A hot pixel under the cursor.
        let at = ((10 * w + 10) * 4) as usize;
        px[at..at + 3].copy_from_slice(&[240, 10, 10]);
        let reference = crate::measure::picked_grey(&px, w, h, 10.0 / 39.0, 10.0 / 39.0).expect("a grey card");
        let (t, g) = crate::white_balance::white_balance_to_neutralise(reference, klipp_core::WorkingColorspace::Rec709Linear);
        assert!(t > 2.0, "a blue cast asks for warmth, got {t}");
        let card = card.map(|v| crate::transfer::srgb_to_linear(v as f32 / 255.0));
        let m = crate::white_balance::white_balance_matrix(t, g, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let out = m.map(|row| row[0] * card[0] + row[1] * card[1] + row[2] * card[2]);
        let (mx, mn) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
        assert!((mx - mn) / mx < 0.03, "the card is not grey after the pick: {out:?}");
    }

    /// A grey wall under tungsten (chroma 30) and under a very strong
    /// amber light (about 50) are what Auto WB refuses and sends the user
    /// to pick: the picker takes them.
    #[test]
    fn the_picker_takes_a_wall_under_a_strong_cast() {
        let (w, h) = (16u32, 16u32);
        for c in [[168u8, 126, 86], [215, 135, 65]] {
            let px: Vec<u8> = (0..w * h).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
            let reference = crate::measure::picked_grey(&px, w, h, 0.5, 0.5).unwrap_or_else(|e| panic!("{c:?}: {e}"));
            let (t, _) = crate::white_balance::white_balance_to_neutralise(reference, klipp_core::WorkingColorspace::Rec709Linear);
            assert!(t < 0.0, "{c:?}: an amber wall asks for a cooler balance, got {t}");
        }
    }

    /// Clipped white, black and a plain colour carry no light's colour
    /// to balance on; the picker says so rather than guessing.
    #[test]
    fn the_picker_refuses_clipped_black_and_colour() {
        let (w, h) = (16u32, 16u32);
        for (c, what) in [([255u8, 255, 255], "clipped"), ([4, 4, 5], "dark"), ([30, 160, 60], "colour")] {
            let px: Vec<u8> = (0..w * h).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
            let got = crate::measure::picked_grey(&px, w, h, 0.5, 0.5);
            assert!(got.is_err(), "{what}: picked {got:?}");
        }
    }

    // ─── Film grain CPU tests ─────────────────────────────────────────

    fn wang_hash_cpu(mut x: u32) -> f32 {
        x = (x ^ 61) ^ (x >> 16);
        x = x.wrapping_mul(9);
        x = x ^ (x >> 4);
        x = x.wrapping_mul(0x27d4eb2d);
        x = x ^ (x >> 15);
        x as f32 / u32::MAX as f32
    }

    fn apply_grain_cpu(rgba: &mut [u8], width: u32, amount: f32, size: f32,
                       shadows: f32, highlights: f32, monochrome: bool, frame_seed: u32) {
        for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
            let px = (i as u32) % width;
            let py = (i as u32) / width;
            let qx = (px as f32 / size) as u32;
            let qy = (py as f32 / size) as u32;
            let seed = qx.wrapping_mul(1973).wrapping_add(qy.wrapping_mul(9277))
                .wrapping_add(frame_seed.wrapping_mul(26699));
            let noise_r = (wang_hash_cpu(seed) - 0.5) * amount;
            let noise_g = if monochrome { noise_r } else { (wang_hash_cpu(seed + 1) - 0.5) * amount };
            let noise_b = if monochrome { noise_r } else { (wang_hash_cpu(seed + 2) - 0.5) * amount };
            let luma = 0.2126 * chunk[0] as f32 / 255.0 + 0.7152 * chunk[1] as f32 / 255.0
                + 0.0722 * chunk[2] as f32 / 255.0;
            let weight = ((1.0 - luma) * shadows + luma * highlights).min(1.5);
            chunk[0] = ((chunk[0] as f32 / 255.0 + noise_r * weight).clamp(0.0, 1.0) * 255.0) as u8;
            chunk[1] = ((chunk[1] as f32 / 255.0 + noise_g * weight).clamp(0.0, 1.0) * 255.0) as u8;
            chunk[2] = ((chunk[2] as f32 / 255.0 + noise_b * weight).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }

    #[test]
    fn grain_zero_amount_is_noop() {
        let mut px = vec![100, 150, 200, 255, 50, 80, 120, 255];
        let orig = px.clone();
        apply_grain_cpu(&mut px, 2, 0.0, 1.0, 1.0, 0.5, true, 42);
        assert_eq!(px, orig, "zero grain should be exact no-op");
    }

    #[test]
    fn grain_deterministic_per_seed() {
        let mut px1 = vec![128; 400]; // 100 pixels
        let mut px2 = px1.clone();
        apply_grain_cpu(&mut px1, 10, 0.2, 1.0, 1.0, 0.5, true, 42);
        apply_grain_cpu(&mut px2, 10, 0.2, 1.0, 1.0, 0.5, true, 42);
        assert_eq!(px1, px2, "same seed should produce identical results");
    }

    #[test]
    fn grain_varies_between_frames() {
        let mut px1 = vec![128; 400];
        let mut px2 = vec![128; 400];
        apply_grain_cpu(&mut px1, 10, 0.2, 1.0, 1.0, 0.5, true, 0);
        apply_grain_cpu(&mut px2, 10, 0.2, 1.0, 1.0, 0.5, true, 1);
        assert_ne!(px1, px2, "different seeds should produce different results");
    }

    #[test]
    fn grain_monochrome_equal_channels() {
        let mut px = vec![128, 128, 128, 255];
        let orig = px.clone();
        apply_grain_cpu(&mut px, 1, 0.3, 1.0, 1.0, 1.0, true, 7);
        // Monochrome: all channels should shift by the same amount
        let dr = px[0] as i16 - orig[0] as i16;
        let dg = px[1] as i16 - orig[1] as i16;
        let db = px[2] as i16 - orig[2] as i16;
        assert_eq!(dr, dg, "mono grain R={dr} should equal G={dg}");
        assert_eq!(dg, db, "mono grain G={dg} should equal B={db}");
    }

    #[test]
    fn grain_colour_independent_channels() {
        // Colour grain: R, G, B should get independent noise
        let mut px = vec![128, 128, 128, 255];
        apply_grain_cpu(&mut px, 1, 0.3, 1.0, 1.0, 1.0, false, 7);
        // With independent hashes, at least one channel pair should differ
        // (statistically near-impossible for all 3 to be identical)
        let dr = px[0] as i16 - 128;
        let dg = px[1] as i16 - 128;
        let db = px[2] as i16 - 128;
        let all_same = dr == dg && dg == db;
        assert!(!all_same, "colour grain channels should be independent: dr={dr} dg={dg} db={db}");
    }

    #[test]
    fn grain_clamped() {
        // Max grain on near-black and near-white pixels — must not overflow
        let mut px = vec![2, 2, 2, 255, 253, 253, 253, 255];
        apply_grain_cpu(&mut px, 2, 1.0, 1.0, 1.0, 1.0, true, 99);
        // All values must be valid u8 (0..=255) — the type guarantees this,
        // but verify no panic occurred and alpha is preserved
        assert_eq!(px[3], 255);
        assert_eq!(px[7], 255);
    }

    #[test]
    fn grain_shadow_weight() {
        // shadows=1.0, highlights=0.0: dark pixels should show more grain
        let mut dark = vec![10, 10, 10, 255];
        let mut bright = vec![245, 245, 245, 255];
        apply_grain_cpu(&mut dark, 1, 0.3, 1.0, 1.0, 0.0, true, 42);
        apply_grain_cpu(&mut bright, 1, 0.3, 1.0, 1.0, 0.0, true, 42);
        let dark_change = (dark[0] as i16 - 10).unsigned_abs();
        let bright_change = (bright[0] as i16 - 245).unsigned_abs();
        // With highlights=0, bright pixels get zero weight → no grain
        // Dark pixels get full shadow weight → grain applied
        assert!(dark_change >= bright_change,
            "dark pixel change ({dark_change}) should >= bright ({bright_change}) with highlights=0");
    }

    // ─── log_to_linear ─────────────────────────────────────────────────

    #[test]
    fn log_to_linear_slog3_18_percent_gray() {
        // S-Log3 encodes 18% gray (0.18 linear) at approximately 0.4 code value.
        // The log_to_linear function should map ~0.4 → ~0.18 linear.
        let linear = log_to_linear(0.4, klipp_core::LogFormat::SLog3);
        assert!(
            (linear - 0.18).abs() < 0.03,
            "SLog3(0.4) should be near 0.18 (18% gray), got {linear}"
        );
        // Must be positive and finite
        assert!(linear > 0.0 && linear.is_finite());
    }

    #[test]
    fn log_to_linear_logc3_18_percent_gray() {
        // LogC3 (EI 800): 18% gray at ~0.391 code value
        let linear = log_to_linear(0.391, klipp_core::LogFormat::LogC3);
        assert!(
            (linear - 0.18).abs() < 0.05,
            "LogC3(0.391) should be near 0.18, got {linear}"
        );
        assert!(linear > 0.0 && linear.is_finite());
    }

    #[test]
    fn log_to_linear_full_range_no_nan_inf() {
        // For all log formats, 0.0 and 1.0 should produce finite values.
        use klipp_core::LogFormat;
        let formats = [
            LogFormat::SLog3, LogFormat::SLog2, LogFormat::LogC3,
            LogFormat::LogC4, LogFormat::CanonLog, LogFormat::CanonLog3,
            LogFormat::VLog, LogFormat::NLog, LogFormat::FLog,
            LogFormat::DJIDLog, LogFormat::AppleLog,
        ];
        for fmt in &formats {
            let at_zero = log_to_linear(0.0, *fmt);
            let at_one = log_to_linear(1.0, *fmt);
            assert!(
                at_zero.is_finite(),
                "log_to_linear(0.0, {fmt:?}) produced non-finite: {at_zero}"
            );
            assert!(
                at_one.is_finite(),
                "log_to_linear(1.0, {fmt:?}) produced non-finite: {at_one}"
            );
            // 1.0 in log space should map to a positive linear value
            assert!(
                at_one > 0.0,
                "log_to_linear(1.0, {fmt:?}) should be positive, got {at_one}"
            );
        }
    }

    #[test]
    fn log_to_linear_zero_maps_near_zero() {
        // For most log formats, 0.0 code value should map to near-zero or small negative
        // (some formats like CanonLog produce larger negative values below black).
        use klipp_core::LogFormat;
        let formats_tight = [
            LogFormat::SLog3, LogFormat::LogC3, LogFormat::VLog,
            LogFormat::CanonLog3,
        ];
        for fmt in &formats_tight {
            let val = log_to_linear(0.0, *fmt);
            assert!(
                val.abs() < 0.1,
                "log_to_linear(0.0, {fmt:?}) should be near zero, got {val}"
            );
        }
        // CanonLog produces a larger negative at 0.0 (below-black region) — just verify finite
        let canon_val = log_to_linear(0.0, LogFormat::CanonLog);
        assert!(canon_val.is_finite(), "CanonLog(0.0) should be finite, got {canon_val}");
        assert!(canon_val < 0.0, "CanonLog(0.0) should be negative (below black)");
    }

    #[test]
    fn round_trip_linear_to_srgb_and_back() {
        // Both directions of the transfer curve now live in
        // crate::transfer and have their own thorough tests. The
        // spot-check below stays to catch any accidental rewiring
        // that pulls the wrong function into transforms.rs.
        use crate::transfer::srgb_to_linear;
        assert!((linear_to_srgb(0.0) - 0.0).abs() < 0.001);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 0.001);
        let srgb_18 = linear_to_srgb(0.18);
        assert!(
            (srgb_18 - 0.4613).abs() < 0.01,
            "linear_to_srgb(0.18) should be ~0.4613, got {srgb_18}"
        );
        // Forward direction is now exposed too.
        assert!((srgb_to_linear(0.4613) - 0.18).abs() < 0.005);
    }

    // ─── Log format monotonicity ────────────────────────────────────────

    #[test]
    fn log_to_linear_monotonic() {
        // For most formats, log_to_linear should be monotonically increasing
        // (higher code value → higher linear value).
        // Note: CanonLog3 has a known piecewise discontinuity at its cutpoint
        // (~0.097) where the linear segment meets the log segment, so we
        // exclude it from strict monotonicity testing.
        use klipp_core::LogFormat;
        let formats = [
            LogFormat::SLog3, LogFormat::SLog2, LogFormat::LogC3,
            LogFormat::LogC4, LogFormat::CanonLog,
            LogFormat::VLog, LogFormat::FLog,
            LogFormat::AppleLog,
        ];
        for fmt in &formats {
            let mut prev = log_to_linear(0.0, *fmt);
            for i in 1..=100 {
                let v = i as f32 / 100.0;
                let cur = log_to_linear(v, *fmt);
                assert!(
                    cur >= prev,
                    "log_to_linear not monotonic for {fmt:?}: f({}) = {cur} < f({}) = {prev}",
                    v, (i - 1) as f32 / 100.0
                );
                prev = cur;
            }
        }
    }

    #[test]
    fn log_to_linear_canon_log3_above_cutpoint_monotonic() {
        // CanonLog3 has a piecewise discontinuity at ~0.097, but above the cut region
        // (code value > 0.1) it should be monotonic.
        use klipp_core::LogFormat;
        let mut prev = log_to_linear(0.1, LogFormat::CanonLog3);
        for i in 11..=100 {
            let v = i as f32 / 100.0;
            let cur = log_to_linear(v, LogFormat::CanonLog3);
            assert!(
                cur >= prev,
                "CanonLog3 not monotonic above cut: f({v}) = {cur} < f({}) = {prev}",
                (i - 1) as f32 / 100.0
            );
            prev = cur;
        }
    }

    #[test]
    fn log_to_linear_dji_dlog_above_cutpoint_monotonic() {
        // DJI D-Log has a piecewise discontinuity at 0.14, but above the cut region
        // it should be monotonic.
        use klipp_core::LogFormat;
        let mut prev = log_to_linear(0.15, LogFormat::DJIDLog);
        for i in 16..=100 {
            let v = i as f32 / 100.0;
            let cur = log_to_linear(v, LogFormat::DJIDLog);
            assert!(
                cur >= prev,
                "DJIDLog not monotonic above cut: f({v}) = {cur} < f({}) = {prev}",
                (i - 1) as f32 / 100.0
            );
            prev = cur;
        }
    }

    // ─── linear_to_srgb properties ──────────────────────────────────────

    #[test]
    fn linear_to_srgb_boundary_values() {
        // Known sRGB transfer function properties
        assert!((linear_to_srgb(0.0)).abs() < 1e-6, "sRGB(0) = 0");
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-4, "sRGB(1) = 1");
        // The linear segment ends at 0.0031308
        let at_cut = linear_to_srgb(0.003_130_8);
        assert!(at_cut > 0.0 && at_cut < 0.05, "sRGB at cutoff should be small positive");
    }

    #[test]
    fn linear_to_srgb_monotonic() {
        let mut prev = linear_to_srgb(0.0);
        for i in 1..=1000 {
            let v = i as f32 / 1000.0;
            let cur = linear_to_srgb(v);
            assert!(cur >= prev, "linear_to_srgb not monotonic at {v}");
            prev = cur;
        }
    }
}
