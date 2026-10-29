//! Verifies `Frame::into_cpu()` on the `Gpu` variant actually reads
//! bytes back from a `wgpu::Buffer`. Uses a headless wgpu device
//! created locally; the test is a no-op on systems without a usable
//! backend (CI without GPU, etc.).

use std::sync::Arc;

use klipp_engine::{CpuFrame, Frame, GpuFrame};

fn try_wgpu() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
    // Match the env shim klipp-effects uses on CachyOS so the test
    // behaves identically to the production GPU init.
    if std::env::var("VK_DRIVER_FILES").is_err() {
        let icd = "/usr/share/vulkan/icd.d/nvidia_icd.json";
        if std::path::Path::new(icd).exists() {
            // Safe: single-threaded test startup.
            unsafe { std::env::set_var("VK_DRIVER_FILES", icd) };
        }
    }

    let instance = klipp_effects::gpu_context::instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("frame_gpu_roundtrip"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()?;
    Some((Arc::new(device), Arc::new(queue)))
}

#[test]
fn gpu_frame_into_cpu_reads_back_exact_bytes() {
    let Some((device, queue)) = try_wgpu() else {
        eprintln!("skipping: no usable wgpu backend");
        return;
    };

    let width: u32 = 4;
    let height: u32 = 2;
    // 4×2 RGBA8 — 32 bytes total. Distinct pattern per pixel so a
    // miscopy is obvious.
    let expected: Vec<u8> = (0u8..32).collect();

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("roundtrip_src"),
        size: expected.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, &expected);
    // Flush the write before we submit a dependent copy.
    device.poll(wgpu::Maintain::Poll);

    let frame = Frame::Gpu(GpuFrame::new(
        device,
        queue,
        Arc::new(buffer),
        width,
        height,
    ));

    let cpu: CpuFrame = frame.into_cpu();
    assert_eq!(cpu.width, width);
    assert_eq!(cpu.height, height);
    assert_eq!(cpu.data, expected, "readback bytes must match the original upload");
}

#[test]
fn cpu_variant_into_cpu_is_zero_copy_semantically() {
    let data = vec![9u8; 16];
    let frame = Frame::Cpu(CpuFrame::new(data.clone(), 2, 2));
    let back = frame.into_cpu();
    assert_eq!(back.data, data);
    assert_eq!(back.width, 2);
    assert_eq!(back.height, 2);
}
