//! The Veylvik page's viewer, given Klipp's display, shows a picture as
//! Klipp's viewer does: the texture it gets back holds Klipp's encode of
//! the picture, byte for byte. And what that costs at a viewer's size.

use std::sync::Arc;
use std::time::Instant;

use klipp_core::{SdrTransfer, WorkingColorspace};
use klipp_veylvik::display::KlippDisplay;
use veylvik_preview::screen::HostEncode;

fn texture_bytes(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
    let pitch = (w * 4).div_ceil(256) * 256;
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(pitch * h), usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut e = device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &staging, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([e.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    klipp_effects::gpu::poll_until_mapped(device, &rx).unwrap().unwrap();
    let padded = staging.slice(..).get_mapped_range().to_vec();
    padded.chunks(pitch as usize).flat_map(|row| row[..(w * 4) as usize].to_vec()).collect()
}

#[test]
fn the_page_gets_klipps_view_of_its_picture() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let (device, queue) = (ctx.device(), ctx.queue());
    for (w, h) in [(64u32, 36u32), (1920, 1080)] {
        let picture: Vec<f32> = (0..w * h).flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            [2.0 * x, y, 0.5 * (1.0 - x), 1.0]
        }).collect();
        let buffer = wgpu::util::DeviceExt::create_buffer_init(device.as_ref(), &wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&picture), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC });
        for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG] {
            let display = Arc::new(KlippDisplay::new(working, SdrTransfer::Srgb, None));
            let t = Instant::now();
            let texture = display.encode(&device, &queue, &buffer, w, h).expect("encoded");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("{w}x{h} {working:?}: encode {ms:.1} ms");
            assert_eq!((texture.width(), texture.height()), (w, h));
            assert_eq!(texture_bytes(&device, &queue, &texture, w, h), display.encode_pixels(&picture), "{w}x{h} {working:?}");
        }
    }
}
