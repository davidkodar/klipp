//! A composition that reads its footage at other times (a retime, motion
//! blur, a node that wants recent frames) gets the clip's frames there from
//! Klipp, as Klipp's own temporal nodes read them. It got the frame
//! rendered only, and failed for every other: the clip showed its media.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};
use veylvik_document::{nodes, Node, Operation};

/// The project's compositions are the process's: one test at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 64;
const H: u32 = 36;
const FPS: f64 = 24000.0 / 1001.0;

fn numbered(index: i64, w: u32, h: u32) -> Vec<f32> {
    (0..w * h).flat_map(|_| [index as f32 / 64.0, 0.5, 0.3, 1.0]).collect()
}

/// A picture that says which frame of the media it is, and its other
/// frames read as a file is, counted.
struct Numbered {
    reads: Arc<AtomicUsize>,
}
impl FrameSource for Numbered {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let pixels = numbered((secs * FPS + 1e-6).floor() as i64, w, h);
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(FPS)
    }
    fn media_range(&mut self, _: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 10.0))
    }
    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, _: WorkingColorspace, w: u32, h: u32) -> Option<klipp_compose::LinearFrameProviderFn> {
        let source_in = clip.source_in.to_secs_f64();
        let reads = Arc::clone(&self.reads);
        Some(Box::new(move |t: f64| {
            reads.fetch_add(1, Ordering::Relaxed);
            let index = ((source_in + t) * FPS + 1e-6).floor() as i64;
            Some((Arc::new(numbered(index, w, h)), w, h))
        }))
    }
}

fn sequence(composition: Option<CompositionId>, source_in: f64) -> Sequence {
    let at = |s: f64| RationalTime::from_secs_f64(s, 24000);
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(4.0), at(source_in), at(source_in + 4.0));
    clip.composition = composition;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

/// The clip's footage, two and a half frames later.
fn ahead() -> Composition {
    let mut c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: 24000, rate: 1001 }, 240).unwrap();
    let remap = Node::new("Later", Operation::TimeRemap(nodes::TimeRemap { rate: 1.0.into(), source_time: (2.5 * 1001.0 / 24000.0).into() }), &[("image", c.output)]);
    c.output = remap.id;
    c.nodes.insert(remap.id, remap);
    c
}

/// The red of the picture's middle: which frame it shows, ×64.
fn shown(picture: &[f32]) -> f32 {
    let at = ((H / 2 * W + W / 2) * 4) as usize;
    picture[at] * 64.0
}

#[test]
fn a_retimed_composition_shows_the_clips_frames_it_asks_for() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(3));
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&ahead()).unwrap()))].into());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let reads = Arc::new(AtomicUsize::new(0));
    // A clip starting a second into its media: clip time and media time apart.
    let source_in = 24.0 * 1001.0 / 24000.0;
    let seq = sequence(Some(id), source_in);
    for i in 0..12i64 {
        let at = RationalTime::from_secs_f64(i as f64 * 1001.0 / 24000.0, 24000);
        let picture = r.render(&seq, at, W, H, &mut Numbered { reads: Arc::clone(&reads) }, RenderOptions::default());
        let media = 24 + i;
        assert!((shown(&picture) - (media + 2) as f32).abs() < 0.02, "clip frame {i}: media frame {} shown, not {}", media + 2, shown(&picture));
    }
    assert!(reads.load(Ordering::Relaxed) > 0, "read from Klipp's readers");
    klipp_veylvik::set_project_compositions(Compositions::new());
}

/// A composition that reads only the frame rendered never asks Klipp for
/// others: no file opened, no frame decoded for it.
#[test]
fn a_composition_of_the_frame_alone_reads_no_other() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(4));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: 24000, rate: 1001 }, 240).unwrap();
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let reads = Arc::new(AtomicUsize::new(0));
    let seq = sequence(Some(id), 0.0);
    for i in 0..6i64 {
        let at = RationalTime::from_secs_f64(i as f64 * 1001.0 / 24000.0, 24000);
        let picture = r.render(&seq, at, W, H, &mut Numbered { reads: Arc::clone(&reads) }, RenderOptions::default());
        assert!((shown(&picture) - i as f32).abs() < 0.02, "frame {i}");
    }
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    klipp_veylvik::set_project_compositions(Compositions::new());
}

/// A frame of Klipp's on the device: linear floats, or a viewer stream's
/// sRGB bytes.
struct OnDevice {
    buffer: wgpu::Buffer,
    srgb8: bool,
}
impl klipp_nodes::frame_context::GpuFramePixels for OnDevice {
    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    fn read(&self) -> Arc<Vec<f32>> {
        unreachable!("read on the device")
    }
    fn srgb8(&self) -> bool {
        self.srgb8
    }
}

/// The viewer's way: the neighbours it holds on the GPU, as floats or as
/// its streams' bytes, and no file read for them.
struct Held {
    device: Arc<wgpu::Device>,
    srgb8: bool,
    reads: Arc<AtomicUsize>,
}
impl FrameSource for Held {
    fn frame(&mut self, clip: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Numbered { reads: Arc::new(AtomicUsize::new(0)) }.frame(clip, secs, w, h)
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(FPS)
    }
    fn temporal_frames_on_gpu(&mut self, clip: &Clip, _: WorkingColorspace, w: u32, h: u32) -> Option<klipp_compose::GpuFrameProviderFn> {
        let (device, srgb8, source_in) = (Arc::clone(&self.device), self.srgb8, clip.source_in.to_secs_f64());
        Some(Box::new(move |t: f64| {
            let index = ((source_in + t) * FPS + 1e-6).floor() as i64;
            let contents: Vec<u8> = if srgb8 {
                // Bytes whose sRGB decode is the frame's number / 64 in red.
                let red = klipp_color::transfer::linear_to_srgb(index as f32 / 64.0);
                (0..w * h).flat_map(|_| [(red * 255.0).round() as u8, 128, 80, 255]).collect()
            } else {
                bytemuck::cast_slice(&numbered(index, w, h)).to_vec()
            };
            let buffer = wgpu::util::DeviceExt::create_buffer_init(device.as_ref(), &wgpu::util::BufferInitDescriptor { label: None, contents: &contents, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC });
            Some(Arc::new(OnDevice { buffer, srgb8 }) as Arc<dyn klipp_nodes::frame_context::GpuFramePixels>)
        }))
    }
    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, working: WorkingColorspace, w: u32, h: u32) -> Option<klipp_compose::LinearFrameProviderFn> {
        // Opening the file's reader, counted as a read.
        self.reads.fetch_add(1, Ordering::Relaxed);
        Numbered { reads: Arc::clone(&self.reads) }.temporal_frames_linear_at_size(clip, working, w, h)
    }
}

#[test]
fn neighbours_the_viewer_holds_on_the_gpu_are_used_there() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(5));
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&ahead()).unwrap()))].into());
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let seq = sequence(Some(id), 0.0);
    for srgb8 in [false, true] {
        // Each way its own footage: a frame Veylvik kept the other way is
        // not used.
        klipp_veylvik::media_changed();
        let reads = Arc::new(AtomicUsize::new(0));
        for i in 0..6i64 {
            let at = RationalTime::from_secs_f64(i as f64 * 1001.0 / 24000.0, 24000);
            let mut src = Held { device: ctx.device(), srgb8, reads: Arc::clone(&reads) };
            let picture = r.render(&seq, at, W, H, &mut src, RenderOptions::default());
            // 8-bit: within a step of the byte the frame's number became.
            let tolerance = if srgb8 { 0.2 } else { 0.02 };
            assert!((shown(&picture) - (i + 2) as f32).abs() < tolerance, "srgb8={srgb8} frame {i}: {} shown", shown(&picture));
        }
        assert_eq!(reads.load(Ordering::Relaxed), 0, "srgb8={srgb8}: no file opened or read for frames held on the GPU");
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
}
