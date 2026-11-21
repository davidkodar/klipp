//! A decoder's frame that is on the GPU goes to Veylvik there: the viewer's
//! sRGB bytes expanded on the device through Klipp's own tables, an
//! export's linear frame copied. Through a pass-through composition the
//! picture is the plain clip's (to a float's last place, where Veylvik's
//! premultiply round trip moves it), and the frame is never read back to the
//! CPU on the way (the source counts its read-backs).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use klipp_compose::{FrameSource, GpuPixels, Inspect, OnDevice, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Compositor, Time};
use wgpu::util::DeviceExt;

const W: u32 = 96;
const H: u32 = 54;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// Every byte value in every channel somewhere, half-transparent pixels too.
fn bytes() -> Vec<u8> {
    (0..W * H).flat_map(|i| [(i % 256) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, if i % 3 == 0 { 128 } else { (255 - i % 7) as u8 }]).collect()
}

struct OnGpu {
    buffer: wgpu::Buffer,
    srgb8: bool,
    cpu: Vec<f32>,
    reads: Arc<AtomicUsize>,
}

impl GpuPixels for OnGpu {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        (width, height) == (W, H) && if self.srgb8 { exec.prime_chain_from_gpu_u8_buffer(&self.buffer, W, H) } else { exec.prime_chain_from_gpu_linear_buffer(&self.buffer, W, H) }
    }
    fn read(&self) -> Arc<Vec<f32>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Arc::new(self.cpu.clone())
    }
    fn on_device(&self) -> Option<(OnDevice<'_>, u32, u32)> {
        Some((if self.srgb8 { OnDevice::Srgb8(&self.buffer) } else { OnDevice::Linear(&self.buffer) }, W, H))
    }
}

struct Decoder(Arc<OnGpu>);
impl FrameSource for Decoder {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Gpu(Arc::clone(&self.0) as Arc<dyn GpuPixels>), width: W, height: H, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
}

fn sequence(composition: Option<CompositionId>) -> Sequence {
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(5)), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.composition = composition;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, src: &mut dyn FrameSource) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.render_inspected(seq, tc(1.0), W, H, src, RenderOptions::default(), Inspect::default()).0
}

#[test]
fn a_frame_on_the_gpu_reaches_veylvik_there_and_unchanged() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let compositor = Compositor::shared().expect("Veylvik on Klipp's device");
    let id = CompositionId(uuid::Uuid::from_u128(77));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: 24, rate: 1 }, 48).unwrap();
    compositor.set_compositions(Compositions::from([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))]));

    let raw = bytes();
    let srgb_cpu = klipp_effects::srgb_u8_to_linear_f32_rgba(&raw);
    let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let device = ctx.device();
    for srgb8 in [true, false] {
        let contents: Vec<u8> = if srgb8 { raw.clone() } else { bytemuck::cast_slice(&srgb_cpu).to_vec() };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("decoded"), contents: &contents, usage });
        let reads = Arc::new(AtomicUsize::new(0));
        let frame = Arc::new(OnGpu { buffer, srgb8, cpu: srgb_cpu.clone(), reads: Arc::clone(&reads) });

        let plain = render(&sequence(None), &mut Decoder(Arc::clone(&frame)));
        let before = reads.load(Ordering::SeqCst);
        let mut decoder = Decoder(Arc::clone(&frame));
        let through = render(&sequence(Some(id)), &mut compositor.source(&mut decoder, WorkingColorspace::Rec709Linear));
        assert_eq!(reads.load(Ordering::SeqCst), before, "srgb8={srgb8}: the frame was read back to reach Veylvik");
        let off = plain.iter().zip(&through).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        eprintln!("srgb8={srgb8}: through the composition differs by {off}");
        // Veylvik premultiplies and divides back: at an alpha like 128/255,
        // not a power of two, a float's last place can move.
        assert!(off <= 1e-6, "srgb8={srgb8}: {off}");
    }
}
