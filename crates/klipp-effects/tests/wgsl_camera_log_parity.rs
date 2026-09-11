//! The decoder shaders' camera log curves (WGSL, `color_wgsl`) against the
//! same independent references the CPU curves are checked against
//! (klipp-color/tests/fixtures: OCIO built-ins, colour-science).
//!
//! This is the third copy of every curve — the GPU one, reached through
//! `display_to_linear` for transfer ids >= 100. Log footage decoded on the
//! GPU goes through it, so it has to agree with the references too.

use klipp_core::color_ids as ids;
use klipp_effects::color_wgsl::{WGSL_HELPERS, WGSL_PRELUDE};

const BODY: &str = "
struct P { transfer: u32, n: u32, _a: u32, _b: u32 }
@group(0) @binding(0) var<storage, read> codes: array<f32>;
@group(0) @binding(1) var<storage, read_write> out_v: array<f32>;
@group(0) @binding(2) var<uniform> p: P;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) g: vec3<u32>) {
    if g.x >= p.n { return; }
    let c = codes[g.x];
    out_v[g.x] = display_to_linear(vec3<f32>(c, c, c), p.transfer).x;
}
";

const FORMATS: &[(&str, u32)] = &[
    ("ocio_camlog_SLog3", ids::TRANSFER_SLOG3),
    ("ocio_camlog_LogC3", ids::TRANSFER_LOGC3),
    ("ocio_camlog_LogC4", ids::TRANSFER_LOGC4),
    ("ocio_camlog_VLog", ids::TRANSFER_VLOG),
    ("ocio_camlog_CanonLog3", ids::TRANSFER_CANON_LOG3),
    ("ocio_camlog_RedLog3G10", ids::TRANSFER_RED_LOG3G10),
    ("ocio_camlog_AppleLog", ids::TRANSFER_APPLE_LOG),
    ("colour_camlog_SLog2", ids::TRANSFER_SLOG2),
    ("colour_camlog_CanonLog", ids::TRANSFER_CANON_LOG),
    ("colour_camlog_DJIDLog", ids::TRANSFER_DJI_DLOG),
    ("colour_camlog_FLog", ids::TRANSFER_FLOG),
    ("colour_camlog_NLog", ids::TRANSFER_NLOG),
    ("colour_camlog_GoProFlat", ids::TRANSFER_GOPRO_FLAT),
];

fn fixture(name: &str) -> Vec<(f32, f32)> {
    let path = format!("{}/../klipp-color/tests/fixtures/{name}.csv", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("code"))
        .map(|l| {
            let mut it = l.split(',');
            (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
        })
        .collect()
}

/// Evaluate the shader curve for `transfer` at every code on the GPU.
fn gpu_curve(ctx: &klipp_effects::gpu_context::GpuContext, transfer: u32, codes: &[f32]) -> Vec<f32> {
    use wgpu::util::DeviceExt;
    let dev = &ctx.device;
    let src = format!("{WGSL_PRELUDE}{WGSL_HELPERS}{BODY}");
    let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("wgsl_camera_log_parity"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let pipeline = dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None, layout: None, module: &module, entry_point: "main",
        compilation_options: Default::default(), cache: None,
    });
    let n = codes.len() as u64;
    let input = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(codes), usage: wgpu::BufferUsages::STORAGE,
    });
    let output = dev.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: n * 4, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = dev.create_buffer(&wgpu::BufferDescriptor {
        label: None, size: n * 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let params = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None, contents: bytemuck::cast_slice(&[transfer, n as u32, 0, 0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
        ],
    });
    let mut enc = dev.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups((n as u32).div_ceil(64), 1, 1);
    }
    enc.copy_buffer_to_buffer(&output, 0, &staging, 0, n * 4);
    ctx.queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
    klipp_effects::gpu::poll_until_mapped(dev, &rx).expect("map").expect("map ok");
    let v = bytemuck::cast_slice::<u8, f32>(&slice.get_mapped_range()).to_vec();
    staging.unmap();
    v
}

#[test]
fn shader_camera_log_curves_match_their_references() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let mut failures = Vec::new();
    for (name, transfer) in FORMATS {
        let rows = fixture(name);
        let codes: Vec<f32> = rows.iter().map(|r| r.0).collect();
        let got = gpu_curve(ctx, *transfer, &codes);
        let (mut stops, mut abs) = (0.0_f32, 0.0_f32);
        for (&(_, reference), &g) in rows.iter().zip(&got) {
            // The shader clamps sub-black values to 0, as the CPU decoder
            // does, so compare against the reference clamped the same way.
            let reference = reference.max(0.0);
            if reference > 0.01 && g > 0.0 {
                stops = stops.max((g / reference).log2().abs());
            } else {
                abs = abs.max((g - reference).abs());
            }
        }
        eprintln!("{name:<26} shader: {stops:.2e} stops, {abs:.2e} abs near black");
        if stops > 2e-3 || abs > 1e-3 {
            failures.push(format!("{name}: {stops:.2e} stops, {abs:.2e} abs"));
        }
    }
    assert!(failures.is_empty(), "shader curves off their reference:\n  {}", failures.join("\n  "));
}
