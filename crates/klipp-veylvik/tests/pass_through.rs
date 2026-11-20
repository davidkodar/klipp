//! A clip through a composition that only reads its footage comes back as
//! it went in: Klipp's frame, on Klipp's device, handed to Veylvik and
//! written back, in each of Klipp's working spaces. Half-transparent
//! pixels, light above 1 and colours outside Rec.709 included.

use std::sync::Arc;

use klipp_core::WorkingColorspace;
use klipp_nodes::frame_context::GpuFramePixels;
use klipp_veylvik::{working_pixels, ClipFootage, ClipSource, Composition, HostSource, KlippHost, Renderer, SourceInfo, Time};
use wgpu::util::DeviceExt;

const W: u32 = 32;
const H: u32 = 18;

/// A frame Klipp would hand over: linear, straight alpha, in the working space.
fn pixels() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as f32;
            let a = match x % 4 {
                0 => 1.0,
                1 => 0.5,
                2 => 0.25,
                _ => 1.0,
            };
            // Up to 4.0 of light, some channels near zero.
            v.extend_from_slice(&[(i % 17.0) / 4.0, (x as f32) / (W as f32), (y as f32) / (H as f32) * 0.3, a]);
        }
    }
    v
}

struct Held(wgpu::Buffer);
impl GpuFramePixels for Held {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.0
    }
    fn read(&self) -> Arc<Vec<f32>> {
        unreachable!("never read back")
    }
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer) -> Vec<f32> {
    let size = buffer.size();
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("read back"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit([enc.finish()]);
    staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let out = bytemuck::cast_slice::<u8, f32>(&staging.slice(..).get_mapped_range()).to_vec();
    out
}

#[test]
fn a_pass_through_composition_gives_klipps_frame_back_unchanged() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let info = ctx.adapter_info().cloned().expect("the shared context knows its adapter");
    let (device, queue) = (ctx.device(), ctx.queue());
    let renderer = Arc::new(Renderer::from_device((*device).clone(), (*queue).clone(), info).expect("Veylvik's renderer on Klipp's device"));

    let input = pixels();
    let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("klipp frame"), contents: bytemuck::cast_slice(&input), usage: storage });
    let fps = Time { ticks: 25, rate: 1 };

    for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear] {
        let host = Arc::new(KlippHost::new());
        let held = source.clone();
        let footage = ClipFootage::new(
            SourceInfo { width: W, height: H, duration: None, frame_rate: Some(fps), frames: Some(50), description: "test clip".into() },
            25.0,
            [W, H],
            working_pixels(working),
            1,
            Box::new(move |_| Some(Arc::new(Held(held.clone())) as Arc<dyn GpuFramePixels>)),
        );
        host.insert("clip", Arc::new(HostSource::new(footage, 64 << 20)));
        let composition = Composition::from_host_footage("clip", "Clip", None, [W, H], fps, 50).expect("a composition over the clip");
        let assets = std::env::temp_dir().join(format!("klipp-veylvik-pass-{}", std::process::id()));
        let clip = ClipSource::new(renderer.clone(), composition, assets, Some(host as Arc<dyn klipp_veylvik::Host>)).expect("a clip source");

        let target = device.create_buffer(&wgpu::BufferDescriptor { label: Some("out"), size: source.size(), usage: storage, mapped_at_creation: false });
        clip.render(Time { ticks: 1, rate: 25 }, [W, H], &target, &working_pixels(working)).expect("rendered");
        let out = read_back(&device, &queue, &target);

        let worst = input.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        // Through Rec.709 and back in f32: a few ulps where the matrix turns.
        let allowed = if working == WorkingColorspace::Rec709Linear { 0.0 } else { 2e-6 * 4.0 };
        assert!(worst <= allowed, "{working:?}: largest difference {worst}");
    }
}
