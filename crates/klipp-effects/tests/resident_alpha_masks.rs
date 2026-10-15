//! Resident masks match the existing download API without mapping between passes.
use klipp_effects::gpu::{poll_until_mapped, GpuEffectsPipeline};

#[test]
fn resident_blur_and_dilation_match_and_do_not_read_back() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(
            std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
            "GPU required"
        );
        return;
    };
    let (w, h) = (37, 19);
    let n = (w * h) as usize;
    let bytes = (n * 4) as u64;
    let device = gpu.device();
    let queue = gpu.queue();
    let buffer = || {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident alpha test"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let (src, dst, scratch) = (buffer(), buffer(), buffer());
    let mask: Vec<u8> = (0..n)
        .map(|i| {
            if i % 11 == 0 {
                255
            } else {
                (i * 37 % 256) as u8
            }
        })
        .collect();
    let values: Vec<u32> = mask.iter().map(|a| u32::from(*a)).collect();
    queue.write_buffer(&src, 0, bytemuck::cast_slice(&values));
    let read = |from: &wgpu::Buffer| {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test verification only"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(from, 0, &staging, 0, bytes);
        queue.submit(Some(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        poll_until_mapped(&device, &rx).unwrap().unwrap();
        let mapped = staging.slice(..).get_mapped_range();
        let out: Vec<u8> = bytemuck::cast_slice::<u8, u32>(&mapped)
            .iter()
            .map(|a| *a as u8)
            .collect();
        drop(mapped);
        staging.unmap();
        out
    };
    for radius in [0.0, 0.8, 2.5, 7.0] {
        assert!(gpu.stack_begin(w, h, 1));
        let before = gpu.readback_stats();
        assert!(gpu.record_alpha_blur(&src, &dst, &scratch, w, h, radius));
        gpu.stack_end();
        assert_eq!(gpu.readback_stats(), before);
        assert_eq!(read(&dst), gpu.blur_alpha_gpu(&mask, w, h, radius));

        assert!(gpu.stack_begin(w, h, 1));
        let before = gpu.readback_stats();
        assert!(gpu.record_alpha_dilate(&src, &dst, w, h, radius));
        gpu.stack_end();
        assert_eq!(gpu.readback_stats(), before);
        assert_eq!(read(&dst), gpu.dilate_alpha_gpu(&mask, w, h, radius));
    }
    assert!(!gpu.record_alpha_blur(&src, &dst, &src, w, h, 2.0));
    assert!(!gpu.record_alpha_blur(&src, &dst, &scratch, w, h, f32::NAN));
    assert!(!gpu.record_alpha_blur(&src, &dst, &scratch, w + 1, h, 2.0));
    assert!(!gpu.record_alpha_dilate(&src, &src, w, h, 2.0));
    assert!(!gpu.record_alpha_dilate(&src, &dst, 0, h, 2.0));
    assert!(
        !gpu.is_device_lost(),
        "invalid calls must not poison the device"
    );
}
