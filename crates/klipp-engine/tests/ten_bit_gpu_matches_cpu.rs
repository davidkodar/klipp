//! #3300: a kept frame's ten-bit words made into display bytes on the
//! GPU are the CPU table's bytes.

/// #3300: the GPU's encode of ten-bit words is the CPU table's, byte
/// for byte, dither included: every display setting the table covers,
/// each of the 1024 codes in each channel at many pixels, at 1080p
/// and dispatched in rows too (a small workgroup cap).
#[test]
fn the_gpus_ten_bit_encode_is_the_cpu_tables() {
    use klipp_core::{SdrTransfer, ToneMapOp, WorkingColorspace};
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        return;
    };
    let mut gpu = klipp_effects::gpu::TenBitDisplay::new(ctx).expect("the encode's pipeline");
    let (w, h) = (1920u32, 1080u32);
    // Each pixel's channels walk the codes at different strides, so
    // every code meets many dither values in each channel.
    let words: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (r, g, b) = (i % 1024, (i * 7 + 3) % 1024, (i.wrapping_mul(2_654_435_761) >> 22) % 1024);
            (r | (g << 10) | (b << 20)).to_le_bytes()
        })
        .collect();
    let mut settings = 0;
    for tone in [ToneMapOp::None, ToneMapOp::Reinhard, ToneMapOp::Hable, ToneMapOp::Aces] {
        for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear] {
            for transfer in [SdrTransfer::Srgb, SdrTransfer::Bt1886] {
                let Some(display) = klipp_color::display::per_channel_display(tone, working, transfer) else { continue };
                settings += 1;
                let want = klipp_engine::cache_store::ten_bit_display(&words, display);
                let got = klipp_engine::cache_store::ten_bit_display_on_gpu(&mut gpu, &words, (w, h), display).expect("encoded on the GPU");
                let wrong = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                assert_eq!(wrong, 0, "{tone:?} {working:?} {transfer:?}: {wrong} bytes differ");
            }
        }
    }
    assert!(settings >= 2, "the table covers {settings} settings");
    // Rows of workgroups, as a frame past the per-dimension limit is.
    let _cap = klipp_effects::gpu::WorkgroupCapForTest::new(64);
    let display = klipp_color::display::per_channel_display(ToneMapOp::None, WorkingColorspace::Rec709Linear, SdrTransfer::Srgb).unwrap();
    assert!(klipp_engine::cache_store::ten_bit_display_on_gpu(&mut gpu, &words, (w, h), display) == Some(klipp_engine::cache_store::ten_bit_display(&words, display)), "dispatched in rows");
    // A frame of another size after a larger one: the buffers kept.
    let small = &words[..64 * 32 * 4];
    assert_eq!(klipp_engine::cache_store::ten_bit_display_on_gpu(&mut gpu, small, (64, 32), display), Some(klipp_engine::cache_store::ten_bit_display(small, display)));
    // Words that aren't the size said: refused, for the CPU's encode.
    assert_eq!(klipp_engine::cache_store::ten_bit_display_on_gpu(&mut gpu, small, (64, 33), display), None);
}
