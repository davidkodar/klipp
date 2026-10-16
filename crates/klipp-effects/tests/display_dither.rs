//! The GPU's 8-bit display encodes dither as the export does (#2072).
//!
//! A gradient that crosses few code values across a 4K frame rounds to
//! wide plateaus with hard one-level edges, and the viewer showed them as
//! bands while the export, which dithers, did not. The encodes now add
//! `klipp_effects::dither`'s pattern before they round: the same pattern
//! as the export, so the two agree to the pixel bar float rounding, and
//! never on alpha. The viewer's CPU route (`display_transform_ws_dithered_to`)
//! rounds the same way, so the viewer shows one picture whichever route
//! drew it. A value already on a code value is not dithered, so a picture
//! that was 8-bit to begin with comes back byte for byte on every route.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails.

use klipp_color::display::{ExportDisplay, OdtChoice, ToneMapOp};
use klipp_core::WorkingColorspace;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;
use wgpu::util::DeviceExt;

const W: u32 = 3840;
const H: u32 = 8;
/// The display code values the low-contrast gradient starts and ends on:
/// twelve levels across 3840 pixels, about 320 a level when rounded.
const LO: f32 = 40.0;
const HI: f32 = 52.0;

fn pipeline() -> Option<GpuEffectsPipeline> {
    let gpu = GpuContext::shared().and_then(GpuEffectsPipeline::with_context);
    if gpu.is_none() {
        assert!(
            std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
            "KLIPP_TEST_REQUIRE_GPU=1, but there is no usable GPU",
        );
        eprintln!("unverified: no usable GPU");
    }
    gpu
}

/// The display code value the gradient asks for at column `x`, unrounded.
fn wanted_code(x: u32) -> f32 {
    LO + (HI - LO) * x as f32 / (W - 1) as f32
}

/// Linear light that encodes (sRGB) to `code / 255`.
fn linear_of_code(code: f32) -> f32 {
    klipp_color::transfer::srgb_to_linear(code / 255.0)
}

/// The low-contrast gradient as linear BT.709 RGBA, grey, opaque.
fn gradient() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..H {
        for x in 0..W {
            let l = linear_of_code(wanted_code(x));
            v.extend_from_slice(&[l, l, l, 1.0]);
        }
    }
    v
}

/// `linear` into the chain's input slot, as a stack's result lands there.
fn load(gpu: &mut GpuEffectsPipeline, linear: &[f32]) {
    let buf = gpu.device().create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("display_dither_input"),
        contents: bytemuck::cast_slice(linear),
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    assert!(gpu.copy_linear_into_chain(&buf, W, H));
}

/// The viewer's encode of a Rec.709 project, read back.
fn display_bytes(gpu: &mut GpuEffectsPipeline, linear: &[f32]) -> Vec<u8> {
    load(gpu, linear);
    gpu.encode_chain_to_display(true, W, H, None, false, None, None, true)
        .expect("the encode ran")
        .expect("bytes were asked for")
}

/// The longest run of one value along a row, over every row and channel.
fn longest_flat_run(bytes: &[u8]) -> usize {
    let mut worst = 0;
    for row in bytes.chunks_exact((W * 4) as usize) {
        for c in 0..3 {
            let mut run = 1;
            for x in 1..W as usize {
                if row[x * 4 + c] == row[(x - 1) * 4 + c] {
                    run += 1;
                    worst = worst.max(run);
                } else {
                    run = 1;
                }
            }
        }
    }
    worst
}

fn distinct_levels(bytes: &[u8]) -> usize {
    let mut seen = [false; 256];
    for px in bytes.chunks_exact(4) {
        seen[px[0] as usize] = true;
    }
    seen.iter().filter(|s| **s).count()
}

/// The 12-level gradient through the viewer's encode breaks up: no flat
/// run longer than a few dozen pixels where rounding alone leaves ~320,
/// and every 64-pixel region still averages to what was asked for.
#[test]
fn a_low_contrast_gradient_does_not_band_on_the_gpu() {
    let Some(mut gpu) = pipeline() else { return };
    let bytes = display_bytes(&mut gpu, &gradient());
    let run = longest_flat_run(&bytes);
    eprintln!("12-level gradient over {W} px: longest flat run {run} px, {} distinct levels", distinct_levels(&bytes));
    assert!(run <= 48, "a flat run of {run} px: the encode bands");

    const REGION: u32 = 64;
    for r in 0..W / REGION {
        let (mut got, mut want) = (0.0f64, 0.0f64);
        for y in 0..H {
            for x in r * REGION..(r + 1) * REGION {
                got += bytes[((y * W + x) * 4) as usize] as f64;
                want += wanted_code(x) as f64;
            }
        }
        let n = (REGION * H) as f64;
        let off = (got - want) / n;
        assert!(off.abs() <= 0.5, "region {r}: mean off the asked-for value by {off:.3} codes");
    }
}

/// The ACES and BT.2020 projects' viewer encodes dither too.
#[test]
fn the_wide_working_space_encodes_dither_too() {
    let Some(mut gpu) = pipeline() else { return };
    // In each working space, linear light that its display transform
    // brings to the same few levels: the BT.709 gradient's own colour,
    // moved into the space's basis, for BT.2020; for ACES the gradient
    // is narrowed to what crosses about twelve levels after the RRT.
    let to_2020 = klipp_color::primaries::working_space_conversion_matrix(
        WorkingColorspace::Rec709Linear,
        WorkingColorspace::Bt2020Linear,
    )
    .expect("a basis change");
    let mut bt2020 = gradient();
    for px in bt2020.chunks_exact_mut(4) {
        let rgb = klipp_color::primaries::matrix3_mul_vec(to_2020, [px[0], px[1], px[2]]);
        px[..3].copy_from_slice(&rgb);
    }
    load(&mut gpu, &bt2020);
    let bytes = gpu
        .encode_chain_to_bt2020_display_u8(true, W, H, &klipp_color::gamut::bt2020_to_bt709_gpu_data())
        .expect("the BT.2020 encode ran");
    let run = longest_flat_run(&bytes);
    eprintln!("BT.2020 encode: longest flat run {run} px, {} distinct levels", distinct_levels(&bytes));
    assert!(run <= 48, "BT.2020 encode: a flat run of {run} px");

    let aces: Vec<f32> = (0..H)
        .flat_map(|_| (0..W).flat_map(|x| {
            let l = 0.05 + 0.012 * x as f32 / (W - 1) as f32;
            [l, l, l, 1.0]
        }))
        .collect();
    load(&mut gpu, &aces);
    let bytes = gpu
        .encode_chain_to_aces_rec709_u8(true, W, H, &klipp_color::aces::aces_rec709_gpu_data(1024))
        .expect("the ACES encode ran");
    let levels = distinct_levels(&bytes);
    let run = longest_flat_run(&bytes);
    eprintln!("ACES encode: longest flat run {run} px, {levels} distinct levels");
    assert!((4..=40).contains(&levels), "the ACES gradient should cross a few levels, crossed {levels}");
    assert!(run <= 48, "ACES encode: a flat run of {run} px");
}

/// A picture with colour everywhere, alpha falling across it.
fn varied() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[0.02 + 0.6 * fx, 0.3 * fy + 0.05 * fx, 0.15 + 0.1 * (fx * 9.0).sin().abs(), 1.0 - fx]);
        }
    }
    v
}

/// The viewer's encode and the export's CPU transform, same frame: within
/// a level everywhere, and the same value nearly everywhere — which only
/// holds when the two add the same dither to the same pixel.
fn agrees_with_export(gpu_bytes: &[u8], linear: &[f32], ws: WorkingColorspace, what: &str) {
    let export = ExportDisplay::new(ToneMapOp::None, ws, OdtChoice::MatchViewer)
        .frame(linear, |v, n| klipp_color::display::quantise(v, n, 255.0) as u8);
    // The viewer's CPU route rounds as the export does, so the viewer's two
    // routes and the export are one picture.
    let cpu_viewer = klipp_color::display::display_transform_ws_dithered_to(linear, ToneMapOp::None, ws, klipp_core::SdrTransfer::Srgb);
    assert!(cpu_viewer == export, "{what}: the CPU viewer encode is not the export's");
    let mut worst = 0;
    let mut differ = 0usize;
    for (g, c) in gpu_bytes.iter().zip(&export) {
        let d = (*g as i32 - *c as i32).abs();
        worst = worst.max(d);
        differ += usize::from(d != 0);
    }
    let share = differ as f64 / export.len() as f64;
    eprintln!("{what}: GPU vs export worst {worst}, {:.3}% of samples differ", share * 100.0);
    assert!(worst <= 1, "{what}: the viewer is {worst} levels off the export");
    assert!(share < 0.01, "{what}: {:.2}% of samples differ — not the export's dither", share * 100.0);
}

#[test]
fn the_gpu_display_encode_matches_the_export_dither_included() {
    let Some(mut gpu) = pipeline() else { return };
    for (what, picture) in [("gradient", gradient()), ("varied", varied())] {
        let bytes = display_bytes(&mut gpu, &picture);
        agrees_with_export(&bytes, &picture, WorkingColorspace::Rec709Linear, what);
    }
    // An AcesCG project's viewer encode is the export's ACES Rec.709.
    let aces: Vec<f32> = varied().chunks_exact(4).flat_map(|p| [p[0] * 0.8, p[1] * 0.8, p[2] * 0.8, p[3]]).collect();
    load(&mut gpu, &aces);
    let bytes = gpu
        .encode_chain_to_aces_rec709_u8(true, W, H, &klipp_color::aces::aces_rec709_gpu_data(1024))
        .expect("the ACES encode ran");
    agrees_with_export(&bytes, &aces, WorkingColorspace::AcesCG, "ACES");
}

/// Alpha is quantised exactly: dither is for what is looked at.
#[test]
fn alpha_is_not_dithered() {
    let Some(mut gpu) = pipeline() else { return };
    let picture = varied();
    let check = |bytes: &[u8], what: &str| {
        for (px, src) in bytes.chunks_exact(4).zip(picture.chunks_exact(4)) {
            let exact = (src[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            assert_eq!(px[3], exact, "{what}: alpha {} came back {}", src[3], px[3]);
        }
    };
    let bytes = display_bytes(&mut gpu, &picture);
    check(&bytes, "Rec.709");
    load(&mut gpu, &picture);
    let bytes = gpu
        .encode_chain_to_aces_rec709_u8(true, W, H, &klipp_color::aces::aces_rec709_gpu_data(1024))
        .expect("the ACES encode ran");
    check(&bytes, "ACES");
    load(&mut gpu, &picture);
    let bytes = gpu
        .encode_chain_to_bt2020_display_u8(true, W, H, &klipp_color::gamut::bt2020_to_bt709_gpu_data())
        .expect("the BT.2020 encode ran");
    check(&bytes, "BT.2020");
}

/// The same frame encoded twice is the same bytes: the pattern is fixed
/// in space, so a still picture does not crawl.
#[test]
fn the_dither_is_the_same_every_frame() {
    let Some(mut gpu) = pipeline() else { return };
    let a = display_bytes(&mut gpu, &gradient());
    let b = display_bytes(&mut gpu, &gradient());
    assert!(a == b, "two encodes of one frame differ");
}

/// Every 8-bit code, in every channel, at every position of the dither's
/// pattern that a 3840-wide frame reaches: the frame a decoder hands over,
/// expanded to linear light as the engine expands it.
fn every_code() -> (Vec<u8>, Vec<f32>) {
    let mut bytes = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            bytes.extend_from_slice(&[(i % 256) as u8, ((i / 3 + 85) % 256) as u8, ((i * 7 + 170) % 256) as u8, ((i / 5) % 256) as u8]);
        }
    }
    let linear = bytes
        .chunks_exact(4)
        .flat_map(|p| {
            let c = |v: u8| klipp_color::transfer::srgb_to_linear(v as f32 / 255.0);
            [c(p[0]), c(p[1]), c(p[2]), p[3] as f32 / 255.0]
        })
        .collect();
    (bytes, linear)
}

/// How far, in code values, the CPU's encode of an 8-bit picture's linear
/// light lands from the codes it came from: the float error the dither's
/// on-code tolerance has to absorb.
fn round_trip_error(linear: &[f32]) -> f32 {
    linear
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .map(|v| {
            let x = klipp_color::transfer::linear_to_srgb(v) * 255.0;
            (x - x.round()).abs()
        })
        .fold(0.0, f32::max)
}

/// A picture that was 8-bit to begin with is shown byte for byte: the
/// export and the viewer's CPU route leave a value already on a code value
/// alone, as they leave black and white.
#[test]
fn an_8bit_picture_comes_back_exact_on_the_cpu() {
    let (bytes, linear) = every_code();
    let err = round_trip_error(&linear);
    eprintln!("8-bit round trip on the CPU: within {err:e} of a code (tolerance {:e})", klipp_effects::dither::ON_CODE_TOLERANCE);
    assert!(err * 10.0 <= klipp_effects::dither::ON_CODE_TOLERANCE, "the round trip's error {err} leaves the tolerance no margin");
    let export = ExportDisplay::new(ToneMapOp::None, WorkingColorspace::Rec709Linear, OdtChoice::MatchViewer)
        .frame(&linear, |v, n| klipp_color::display::quantise(v, n, 255.0) as u8);
    assert!(export == bytes, "the export added noise to an 8-bit picture");
    let viewer = klipp_color::display::display_transform_ws_dithered_to(&linear, ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
    assert!(viewer == bytes, "the viewer's CPU route added noise to an 8-bit picture");
}

/// [`an_8bit_picture_comes_back_exact_on_the_cpu`] on the GPU's display
/// encode: every code at 30,720 positions of the pattern, byte for byte.
/// The dither moves a dithered value by a code a quarter of the time, so
/// a GPU whose round trip fell outside the tolerance would fail here.
#[test]
fn an_8bit_picture_comes_back_exact_on_the_gpu() {
    let Some(mut gpu) = pipeline() else { return };
    let (bytes, linear) = every_code();
    let shown = display_bytes(&mut gpu, &linear);
    let differ = shown.iter().zip(&bytes).filter(|(a, b)| a != b).count();
    assert_eq!(differ, 0, "the GPU's display encode changed {differ} samples of an 8-bit picture");
}

/// The export still dithers what is between codes: the 12-level gradient
/// through the export's own transform breaks up, and it is the GPU
/// viewer's picture.
#[test]
fn the_export_still_dithers_a_gradient() {
    let linear = gradient();
    let export = ExportDisplay::new(ToneMapOp::None, WorkingColorspace::Rec709Linear, OdtChoice::MatchViewer)
        .frame(&linear, |v, n| klipp_color::display::quantise(v, n, 255.0) as u8);
    let run = longest_flat_run(&export);
    eprintln!("export of the 12-level gradient: longest flat run {run} px, {} distinct levels", distinct_levels(&export));
    assert!(run <= 48, "the export bands: a flat run of {run} px");
}
