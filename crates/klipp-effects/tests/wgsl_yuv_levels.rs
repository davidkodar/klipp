//! `yuv_normalise` against the levels BT.709 / BT.2100 define: limited
//! range places black, white and zero chroma at 16/235/128 x 2^(n-8).
//! The shader used to scale by (2^n - 1)/255 instead, which is right at 8
//! bits and wrong above — 10-bit chroma zero landed at 513.5.

use klipp_effects::color_wgsl::{WGSL_HELPERS, WGSL_PRELUDE};

const BODY: &str = "
struct P { range: u32, max: f32, n: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> inp: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> out_v: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> p: P;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) g: vec3<u32>) {
    if g.x >= p.n { return; }
    let s = inp[g.x];
    out_v[g.x] = vec4<f32>(yuv_normalise(s.x, s.y, s.z, p.range, p.max), 0.0);
}
";

fn run(ctx: &klipp_effects::gpu_context::GpuContext, range: u32, max: f32, samples: &[[f32; 3]]) -> Vec<[f32; 3]> {
    use wgpu::util::DeviceExt;
    let dev = &ctx.device;
    let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(format!("{WGSL_PRELUDE}{WGSL_HELPERS}{BODY}").into()),
    });
    let pipeline = dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None, layout: None, module: &module, entry_point: "main",
        compilation_options: Default::default(), cache: None,
    });
    let input: Vec<f32> = samples.iter().flat_map(|s| [s[0], s[1], s[2], 0.0]).collect();
    let n = samples.len() as u64;
    let inp = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(&input), usage: wgpu::BufferUsages::STORAGE,
    });
    let out = dev.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: n * 16, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false,
    });
    let staging = dev.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: n * 16, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
    });
    let params: [u32; 4] = [range, max.to_bits(), n as u32, 0];
    let pbuf = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(&params), usage: wgpu::BufferUsages::UNIFORM,
    });
    let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: inp.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: pbuf.as_entire_binding() },
        ],
    });
    let mut enc = dev.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    enc.copy_buffer_to_buffer(&out, 0, &staging, 0, n * 16);
    ctx.queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    klipp_effects::gpu::poll_until_mapped(dev, &rx).unwrap().unwrap();
    let v = bytemuck::cast_slice::<u8, f32>(&slice.get_mapped_range()).to_vec();
    staging.unmap();
    v.chunks_exact(4).map(|c| [c[0], c[1], c[2]]).collect()
}

#[test]
fn limited_range_anchors_land_where_the_standards_put_them() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        eprintln!("skipping: no GPU");
        return;
    };
    // (bit depth max, black Y, white Y, zero chroma)
    for (max, black, white, zero) in [(255.0_f32, 16.0, 235.0, 128.0), (1023.0, 64.0, 940.0, 512.0)] {
        let out = run(ctx, klipp_core::color_ids::COLOR_RANGE_LIMITED, max, &[
            [black, zero, zero],
            [white, zero, zero],
        ]);
        for (label, got, want_y) in [("black", out[0], 0.0_f32), ("white", out[1], 1.0)] {
            assert!((got[0] - want_y).abs() < 1e-6, "{max}-max {label}: Y -> {} (want {want_y})", got[0]);
            assert!(got[1].abs() < 1e-6 && got[2].abs() < 1e-6,
                "{max}-max {label}: zero chroma must be exactly 0, got {:?} — a cast on every neutral", &got[1..]);
        }
    }
}
