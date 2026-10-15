use klipp_effects::gpu::GpuEffectsPipeline;

#[test]
fn masks_and_chain_downloads_are_counted_but_resident_work_is_not() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(
            std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
            "required but no GPU"
        );
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let (w, h) = (32, 16);
    let n = (w * h) as usize;
    let mask = vec![128; n];
    assert_eq!(gpu.blur_alpha_gpu(&mask, w, h, 4.0), mask);
    let s = gpu.readback_stats();
    assert_eq!((s.count, s.bytes, s.failures), (1, n as u64 * 4, 0));
    assert!(s.total_us > 0);
    assert_eq!(gpu.dilate_alpha_gpu(&mask, w, h, 2.0), mask);
    assert_eq!(gpu.readback_stats().count, 2);

    let px = vec![0.4; n * 4];
    gpu.ensure_chain_buffers(n as u64 * 16);
    gpu.upload_chain_input_linear(&px);
    let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("resident diagnostics test"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    gpu.begin_chain();
    assert!(matches!(
        gpu.encode_chain_to_display(true, w, h, None, false, None, Some(&target), false),
        Some(None)
    ));
    assert_eq!(
        gpu.readback_stats().count,
        2,
        "a resident chain does not download"
    );
    assert_eq!(gpu.flush_chain_and_download_linear(true, n), px);
    let s = gpu.readback_stats();
    assert_eq!((s.count, s.bytes, s.failures), (3, n as u64 * 24, 0));
    assert!(gpu.encode_chain_to_display_u8(true, w, h, None).is_some());
    let s = gpu.readback_stats();
    assert_eq!((s.count, s.bytes, s.failures), (4, n as u64 * 28, 0));
    assert!(gpu.linear_rgba_to_u8_gpu(&px, w, h).is_some());
    let s = gpu.readback_stats();
    assert_eq!((s.count, s.bytes, s.failures), (5, n as u64 * 32, 0));
}
