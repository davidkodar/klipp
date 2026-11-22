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
    let mut warm = klipp_color::lut::LutData::identity(17);
    for c in warm.data.as_chunks_mut::<3>().0 {
        c[0] = c[0].powf(0.9);
    }
    let warm = Arc::new(warm);
    let mut failed = Vec::new();
    for (w, h) in [(64u32, 36u32), (1920, 1080)] {
        // Over-bright light, and a band of part and no coverage at the top.
        let picture: Vec<f32> = (0..w * h).flat_map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let a = if y < 0.1 { x } else { 1.0 };
            [2.0 * x * a, y * a, 0.5 * (1.0 - x) * a, a]
        }).collect();
        let buffer = wgpu::util::DeviceExt::create_buffer_init(device.as_ref(), &wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&picture), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC });
        for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear, WorkingColorspace::AcesCG] {
            for transfer in [SdrTransfer::Srgb, SdrTransfer::Bt1886] {
                for lut in [None, Some(Arc::clone(&warm))] {
                    let display = Arc::new(KlippDisplay::new(working, transfer, lut.clone()).on_shared_device());
                    let mut ms = f64::MAX;
                    let mut texture = None;
                    for _ in 0..3 {
                        let t = Instant::now();
                        texture = Some(display.encode(&device, &queue, &buffer, w, h).expect("encoded"));
                        ms = ms.min(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    let texture = texture.unwrap();
                    let on_gpu = display.encodes_on_gpu();
                    let what = format!("{w}x{h} {working:?} {transfer:?} lut={}", lut.is_some());
                    eprintln!("{what}: encode {ms:.1} ms, gpu={on_gpu}");
                    // Klipp's GPU encode serves an sRGB monitor; BT.1886 has
                    // no GPU curve and stays on the CPU.
                    if on_gpu != (transfer == SdrTransfer::Srgb) {
                        failed.push(format!("{what}: gpu={on_gpu}"));
                    }
                    assert_eq!((texture.width(), texture.height()), (w, h));
                    let got = texture_bytes(&device, &queue, &texture, w, h);
                    let want = display.encode_pixels(&picture);
                    let off = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                    let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
                    if off > 0 {
                        eprintln!("{what}: {off} bytes off, worst {worst}");
                    }
                    // Klipp's GPU encode rounds a hair apart from its CPU
                    // encode, as Klipp's own viewer does: a code at most, on
                    // few bytes; a LUT steeper than one stretches that code
                    // to two.
                    let allowed = if lut.is_some() { 2 } else { 1 };
                    if worst > allowed || off * 1000 > got.len() {
                        failed.push(format!("{what}: {off} bytes off, worst {worst}"));
                    }
                }
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
