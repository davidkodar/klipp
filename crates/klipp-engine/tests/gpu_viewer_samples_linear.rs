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
    let tex = fb.ensure_gpu_output(&device, 4, 4).expect("a viewer texture");
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
