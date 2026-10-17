//! A stream's frames converted on the GPU are written into buffers kept
//! and handed out again once nothing holds them, not a fresh 33 MB buffer
//! made and freed for every 4K frame (#2107); and a buffer still held,
//! by a frame in a queue or on screen, is never written again. For 8-bit
//! (NV12) and 10-bit (P010) streams alike.

use klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED;
use klipp_effects::nv12_gpu::GpuNv12Converter;

const W: u32 = 64;
const H: u32 = 32;

fn convert(c: &mut GpuNv12Converter, luma: u8) -> std::sync::Arc<wgpu::Buffer> {
    let y = vec![luma; (W * H) as usize];
    let uv = vec![128u8; (W * H / 2) as usize];
    c.convert_nv12_on_gpu(&y, &uv, W, H, W, W, W, H, 0, [0, 0, 0, 0], IDENTITY_PRIMARIES_MATRIX_PACKED).rgba
}

#[test]
fn frames_let_go_are_written_into_kept_buffers() {
    let Some(mut c) = GpuNv12Converter::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("unverified: no GPU");
        return;
    };
    // Each frame let go as soon as it is converted, as a frame shown and
    // replaced is: a few buffers serve them all.
    for n in 0..60u8 {
        drop(convert(&mut c, n));
    }
    let kept = c.resident_buffers_kept();
    assert!(kept <= 6, "{kept} buffers for 60 frames let go one by one");
    // Held, as frames waiting in a queue are: each gets its own, and none
    // is handed out while it is held.
    let held: Vec<_> = (0..10u8).map(|n| convert(&mut c, n)).collect();
    for n in 0..20u8 {
        let next = convert(&mut c, n);
        assert!(held.iter().all(|h| !std::sync::Arc::ptr_eq(h, &next)), "frame {n} written into a buffer still held");
    }
}

/// The same for a 10-bit stream's frames (P010), two bytes a sample.
#[test]
fn p010_frames_let_go_are_written_into_kept_buffers() {
    use klipp_effects::p010_gpu::GpuP010Converter;
    let Some(mut c) = GpuP010Converter::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("unverified: no GPU");
        return;
    };
    let convert = |c: &mut GpuP010Converter, luma: u8| {
        let y = vec![luma; (W * H * 2) as usize];
        let uv = vec![128u8; (W * H) as usize];
        c.convert_p010_on_gpu(&y, &uv, W, H, W * 2, W * 2, W, H, 0, [0, 0, 0, 0], IDENTITY_PRIMARIES_MATRIX_PACKED).rgba
    };
    for n in 0..60u8 {
        drop(convert(&mut c, n));
    }
    let kept = c.resident_buffers_kept();
    assert!(kept <= 6, "{kept} buffers for 60 frames let go one by one");
    let held: Vec<_> = (0..10u8).map(|n| convert(&mut c, n)).collect();
    for n in 0..20u8 {
        let next = convert(&mut c, n);
        assert!(held.iter().all(|h| !std::sync::Arc::ptr_eq(h, &next)), "frame {n} written into a buffer still held");
    }
}
