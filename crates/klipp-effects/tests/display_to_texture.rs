//! The display encode into a texture is the display encode to bytes:
//! `encode_chain_to_display` stores the same texels the readback returns,
//! at a width whose byte pitch no `copy_buffer_to_texture` would take.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;

// 2160 × 4 = 8640 bytes a row: not a multiple of 256, so the old
// row copy refused it. A portrait UHD frame is this wide.
const W: u32 = 2160;
const H: u32 = 48;

fn pipeline() -> Option<GpuEffectsPipeline> {
    let ctx = GpuContext::shared()?;
    GpuEffectsPipeline::with_context(ctx)
}

fn picture() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[fx * 0.9, fy, (fx * fy).sqrt() * 0.5, 1.0]);
        }
    }
    v
}

/// The texture's texels, rows unpadded.
fn read_texture(gpu: &GpuEffectsPipeline, tex: &wgpu::Texture) -> Vec<u8> {
    let device = gpu.device();
    let queue = gpu.queue();
    let pitch = (W * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read_texture"),
        size: (pitch * H) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(H) },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    queue.submit(std::iter::once(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let padded = slice.get_mapped_range().to_vec();
    staging.unmap();
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H as usize {
        let row = &padded[y * pitch as usize..][..(W * 4) as usize];
        out.extend_from_slice(row);
    }
    out
}

#[test]
fn the_texture_holds_the_bytes_the_readback_returns() {
    let Some(mut gpu) = pipeline() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let tex = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("viewer"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_linear(0, &picture()));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(false));
    let bytes = gpu
        .encode_chain_to_display(true, W, H, None, false, None, Some(&tex), true)
        .expect("the encode ran")
        .expect("bytes were asked for");
    let expected = klipp_effects::linear_f32_rgba_to_srgb_u8(&picture());
    let worst = bytes.iter().zip(&expected).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 1, "the readback is off the CPU encode by {worst}");
    let texels = read_texture(&gpu, &tex);
    assert_eq!(texels, bytes, "the texture differs from the readback");

    // No bytes asked for: nothing comes back, the texture still fills.
    assert!(gpu.stack_begin(W, H, 1));
    let flat: Vec<f32> = [0.5, 0.5, 0.5, 1.0].repeat((W * H) as usize);
    assert!(gpu.stack_layer_from_linear(0, &flat));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(false));
    let none = gpu.encode_chain_to_display(true, W, H, None, false, None, Some(&tex), false).expect("the encode ran");
    assert!(none.is_none());
    let texels = read_texture(&gpu, &tex);
    let grey = klipp_effects::linear_f32_rgba_to_srgb_u8(&[0.5, 0.5, 0.5, 1.0]);
    assert!(texels.chunks(4).all(|p| p.iter().zip(&grey).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 1)));
}

#[test]
fn a_texture_without_a_storage_binding_is_refused() {
    let Some(mut gpu) = pipeline() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let tex = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("sampled_only"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_linear(0, &picture()));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(false));
    assert!(gpu.encode_chain_to_display(true, W, H, None, false, None, Some(&tex), true).is_none());
}
