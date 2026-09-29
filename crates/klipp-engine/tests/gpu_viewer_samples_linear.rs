//! The zero-copy viewer texture holds display-encoded bytes, and the
//! viewer must sample it through a view that decodes them: egui's shader
//! takes what it samples as linear light and encodes it for the screen.
//! Sampled through the plain view, mid-grey came back as 0.5 "linear",
//! was encoded again, and every playing picture showed a whole sRGB curve
//! too bright beside the paused frame and the export (#455). Skips
//! without a GPU.

use klipp_engine::texture::{FrameBuffer, GPU_VIEWER_SAMPLE_FORMAT};

/// Read texel (0,0) of `view` in a compute pass, as the fragment shader
/// would sample it.
fn load(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) -> [f32; 4] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var t: texture_2d<f32>;
             @group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;
             @compute @workgroup_size(1) fn main() { out[0] = textureLoad(t, vec2<i32>(0, 0), 0); }"
                .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    });
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
        ],
    });
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    enc.copy_buffer_to_buffer(&out, 0, &read, 0, 16);
    queue.submit([enc.finish()]);
    read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let v: Vec<f32> = bytemuck::cast_slice(&read.slice(..).get_mapped_range()).to_vec();
    [v[0], v[1], v[2], v[3]]
}

#[test]
fn the_viewer_texture_samples_as_the_linear_light_it_encodes() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let (device, queue) = (ctx.device(), ctx.queue());
    let mut fb = FrameBuffer::new(4, 4);
    let tex = fb.ensure_gpu_output(&device, &queue, 4, 4).expect("a viewer texture");
    // Display-encoded mid-grey, as the stack's display encode leaves it.
    queue.write_texture(
        tex.as_image_copy(),
        &[128u8, 128, 128, 255].repeat(16),
        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(16), rows_per_image: Some(4) },
        wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor { format: Some(GPU_VIEWER_SAMPLE_FORMAT), ..Default::default() });
    let got = load(&device, &queue, &view);
    let want = klipp_color::transfer::srgb_to_linear(128.0 / 255.0);
    for c in 0..3 {
        assert!((got[c] - want).abs() < 0.01, "channel {c}: sampled {}, the linear light of code 128 is {want}", got[c]);
    }
    assert!((got[3] - 1.0).abs() < 0.01);
}

/// #544: while playing, the frame on screen is only in the viewer texture,
/// and a screenshot reads it back from there. Every byte comes back where
/// it was, at a width whose rows are not a copy's 256-byte multiple; and
/// once a frame of bytes follows, the texture is no longer the one to read.
#[test]
fn the_frame_on_screen_reads_back_from_the_viewer_texture() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let (device, queue) = (ctx.device(), ctx.queue());
    let (w, h) = (37u32, 5u32);
    let mut fb = FrameBuffer::new(w, h);
    let tex = fb.ensure_gpu_output(&device, &queue, w, h).expect("a viewer texture");
    assert!(fb.gpu_output_snapshot().is_none(), "no frame has been written there yet");
    let bytes: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
    queue.write_texture(
        tex.as_image_copy(),
        &bytes,
        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    fb.note_gpu_frame();
    let at = klipp_core::RationalTime { value: 489, rate: 30 };
    fb.set_presented(at, true);
    let snapshot = fb.gpu_output_snapshot().expect("the newest frame is in the texture");
    assert_eq!((snapshot.width, snapshot.height), (w, h));
    assert_eq!(snapshot.read().expect("read back"), bytes);
    // It says which moment it is, and a newer frame counts past it, which
    // is how a screenshot knows its copy may have read that newer frame.
    assert_eq!(snapshot.presented, Some(at));
    fb.note_gpu_frame();
    assert_ne!(fb.gpu_output_snapshot().unwrap().frame, snapshot.frame);

    fb.write_frame(vec![0; (w * h * 4) as usize], w, h);
    assert!(fb.gpu_output_snapshot().is_none(), "a frame of bytes is newer than the texture");
}
