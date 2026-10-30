//! Composite scenarios: whole frames through `klipp_compose::Renderer`,
//! the CPU stack against the GPU stack, on the shapes that cost the
//! most: a UHD clip with a credits roll, plus a grade, two video
//! layers with a blend mode, a matte, an adjustment layer with a LUT,
//! five layers at 1080p.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_effects::gpu_context::GpuContext;
use klipp_nodes::executor::GraphExecutor;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

use crate::stats::{summarise, ScenarioResult};

const FRAMES: usize = 60;
const WARMUP: usize = 10;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 30)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

/// A decoded frame the way the engine holds one: sRGB bytes on the GPU,
/// seeded into the chain without a readback; read back only when a
/// stage asks for pixels.
struct OnGpu {
    buffer: Arc<wgpu::Buffer>,
    bytes: Arc<Vec<u8>>,
}

impl klipp_compose::GpuPixels for OnGpu {
    fn seed(&self, exec: &mut GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_gpu_u8_buffer(&self.buffer, width, height)
    }
    fn read(&self) -> Arc<Vec<f32>> {
        Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.bytes))
    }
}

pub(crate) type Make = Box<dyn Fn() -> SourcePixels>;

#[derive(Default)]
struct Sources(HashMap<ClipId, Make>);

impl FrameSource for Sources {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|f| SourceFrame { pixels: f(), width, height, pts: None })
    }
}

/// The decoded picture: a gradient with a diagonal, in sRGB bytes.
pub(crate) fn picture(width: u32, height: u32) -> Arc<Vec<u8>> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            out[i] = (x * 255 / width.max(1)) as u8;
            out[i + 1] = (y * 255 / height.max(1)) as u8;
            out[i + 2] = ((x + y) % 256) as u8;
            out[i + 3] = 255;
        }
    }
    Arc::new(out)
}

fn credits() -> ClipKind {
    let tb = klipp_timeline::TextBox {
        text: "Director\n[Name]\n\nProducer\n[Name]\n\nEditor\n[Name]".into(),
        font_size: 36.0,
        ..Default::default()
    };
    let fx = klipp_timeline::LayerFx { loops: vec![klipp_timeline::TitleLoop::Scroll { direction: klipp_timeline::ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }], ..Default::default() };
    ClipKind::Title(klipp_timeline::TitleData::single_text_fx(tb, fx))
}

fn lower_third(text: &str) -> ClipKind {
    let tb = klipp_timeline::TextBox { text: text.into(), font_size: 48.0, ..Default::default() };
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() {
        o.transform.center.y = 0.85;
    }
    ClipKind::Title(td)
}

struct Build {
    seq: Sequence,
    src: Sources,
}

/// `bytes`, a decoded sRGB frame, as the engine holds one on the GPU: a
/// maker of the frame's pixels, each call sharing the one buffer.
pub(crate) fn decoded_on_gpu(ctx: &GpuContext, bytes: Arc<Vec<u8>>) -> Make {
    let buffer = Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("klipp-perf decoded frame"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    ctx.queue.write_buffer(&buffer, 0, &bytes);
    ctx.device.poll(wgpu::Maintain::Poll);
    let on: Arc<dyn klipp_compose::GpuPixels> = Arc::new(OnGpu { buffer, bytes });
    Box::new(move || SourcePixels::Gpu(Arc::clone(&on)))
}

/// A media clip on `track`, its frame on the GPU when `ctx` is given.
fn add_media(b: &mut Build, track: usize, from: f64, to: f64, width: u32, height: u32, ctx: Option<&GpuContext>, graph: Option<klipp_nodes::NodeGraph>) -> ClipId {
    let mut c = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, from, to);
    if let Some(g) = graph {
        c.set_grade(g);
    }
    let id = c.id;
    let bytes = picture(width, height);
    match ctx {
        Some(ctx) => {
            b.src.0.insert(id, decoded_on_gpu(ctx, bytes));
        }
        None => {
            let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
            b.src.0.insert(id, Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
        }
    }
    b.seq.tracks[track].clips.push(c);
    id
}

fn sequence(width: u32, height: u32, tracks: &[TrackKind]) -> Build {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width, height };
    seq.tracks = tracks.iter().enumerate().map(|(i, k)| Track::new(&format!("T{i}"), *k)).collect();
    Build { seq, src: Sources::default() }
}

fn grade3() -> klipp_nodes::NodeGraph {
    use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId, NodePos}, kinds::NodeKind, params::NodeParams};
    let node = |kind: NodeKind, params: NodeParams| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.3), ..Default::default() }),
        node(NodeKind::Saturation, NodeParams { saturation: Some(1.2), ..Default::default() }),
        node(NodeKind::Contrast, NodeParams { contrast: Some(1.1), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..nodes.len() - 1).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[4].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Run one composite scenario: warm up, then time `FRAMES` frames of the
/// full viewer picture (composite plus display encode). On the GPU run,
/// a frame the stack declines falls back to the CPU path, and the
/// result says so.
fn run(name: &str, cpu_only: bool, budget_ms: f64, build: impl Fn(Option<&GpuContext>) -> Build) -> ScenarioResult {
    let ctx = if cpu_only { None } else { GpuContext::shared() };
    let Build { seq, mut src } = build(ctx);
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(!cpu_only);
    let mut fell_back = 0usize;
    // `KLIPP_PERF_ZERO_COPY=1`: the frame into a viewer texture, nothing
    // read back — the app's playing path on a shared device.
    let zero_copy = std::env::var("KLIPP_PERF_ZERO_COPY").ok().as_deref() == Some("1");
    let texture = ctx.filter(|_| zero_copy).map(|ctx| {
        ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("perf_viewer"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        })
    });
    // `KLIPP_PERF_GPU_WAIT=1`: wait for the GPU after each zero-copy
    // frame, so the time is the frame's and not only the engine's.
    let gpu_wait = zero_copy && std::env::var("KLIPP_PERF_GPU_WAIT").ok().as_deref() == Some("1");
    // `KLIPP_PERF_YUV=1`: the export's frame instead — the composite
    // encoded to 8-bit BT.709 Y'CbCr planes on the GPU and read back
    // (12.4 MB at UHD), as `ExportJob` takes it.
    let yuv = !cpu_only && std::env::var("KLIPP_PERF_YUV").ok().as_deref() == Some("1");
    let mut frame = |r: &mut Renderer, i: usize, count: bool, fell_back: &mut usize| {
        let t = tc(1.0 + i as f64 / 30.0);
        if yuv {
            let target = klipp_effects::gpu::Yuv420Target {
                tone_map_op: 0,
                odt_tag: 0,
                scene: None,
                gamut_op: 0,
                hdr_op: 0,
                peak_nits: 0.0,
                luma_coefs: [0.2126, 0.7152, 0.0722],
                semi_planar: false,
                bits: 8,
                sample_shift: 0,
            };
            if let Some((planes, _)) = r.render_yuv420_on_gpu(&seq, t, w, h, &mut src, RenderOptions::default(), target) {
                std::hint::black_box(planes.len());
                return;
            }
            if count {
                *fell_back += 1;
            }
        } else if !cpu_only {
            let out = klipp_compose::DisplayDelivery { texture: texture.as_ref(), bytes: texture.is_none() };
            if let Some((bytes, _)) = r.render_display_on_gpu(&seq, t, w, h, &mut src, RenderOptions::default(), out) {
                std::hint::black_box(bytes.map(|b| b.len()));
                if gpu_wait {
                    if let Some(ctx) = ctx {
                        ctx.device().poll(wgpu::Maintain::Wait);
                    }
                }
                return;
            }
            if count {
                *fell_back += 1;
            }
        }
        let linear = r.render(&seq, t, w, h, &mut src, RenderOptions::default());
        let bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&linear);
        std::hint::black_box(bytes.len());
    };
    for i in 0..WARMUP {
        frame(&mut r, i, false, &mut fell_back);
    }
    let submits = |r: &mut Renderer| r.executor().gpu_pipeline_mut().map_or(0, |g| g.submit_count());
    let submits_before = submits(&mut r);
    let mut samples = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let t0 = Instant::now();
        frame(&mut r, WARMUP + i, true, &mut fell_back);
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let submits_per_frame = (submits(&mut r) - submits_before) as f64 / FRAMES as f64;
    eprintln!("{name}: {submits_per_frame:.1} queue submits per frame{}", if gpu_wait { ", GPU waited on" } else { "" });
    let mut label = if fell_back > 0 { format!("{name} (CPU fallback {fell_back}/{FRAMES})") } else { name.to_string() };
    if texture.is_some() {
        label.push_str(" (zero-copy)");
    }
    if yuv {
        label.push_str(" (Y'CbCr planes)");
    }
    let mut result = summarise(&label, w, h, &samples, 0, budget_ms);
    result.warmup_frames = WARMUP;
    result
}

const UHD_BUDGET: f64 = 33.3;
const HD_BUDGET: f64 = 16.7;

pub fn s10_uhd_clip_credits(cpu_only: bool) -> ScenarioResult {
    run("s10_composite_uhd_clip_credits", cpu_only, UHD_BUDGET, |ctx| {
        let mut b = sequence(2160, 3840, &[TrackKind::Text, TrackKind::Video]);
        b.seq.tracks[0].clips.push(clip(credits(), 0.0, 5.0));
        add_media(&mut b, 1, 0.0, 48.0, 2160, 3840, ctx, None);
        b
    })
}

/// One clip and nothing else: the frame the direct-present fast path
/// shows, as the stack shows it (seed, blend, encode).
pub fn s15_uhd_single_clip(cpu_only: bool) -> ScenarioResult {
    run("s15_composite_uhd_single_clip", cpu_only, UHD_BUDGET, |ctx| {
        let mut b = sequence(2160, 3840, &[TrackKind::Video]);
        add_media(&mut b, 0, 0.0, 48.0, 2160, 3840, ctx, None);
        b
    })
}

pub fn s11_uhd_clip_credits_grade3(cpu_only: bool) -> ScenarioResult {
    run("s11_composite_uhd_clip_credits_grade3", cpu_only, UHD_BUDGET, |ctx| {
        let mut b = sequence(2160, 3840, &[TrackKind::Text, TrackKind::Video]);
        b.seq.tracks[0].clips.push(clip(credits(), 0.0, 5.0));
        add_media(&mut b, 1, 0.0, 48.0, 2160, 3840, ctx, Some(grade3()));
        b
    })
}

pub fn s12_uhd_two_layers_blend(cpu_only: bool) -> ScenarioResult {
    run("s12_composite_uhd_two_layers_screen", cpu_only, UHD_BUDGET, |ctx| {
        let mut b = sequence(2160, 3840, &[TrackKind::Video, TrackKind::Video]);
        add_media(&mut b, 0, 0.0, 48.0, 2160, 3840, ctx, None);
        add_media(&mut b, 1, 0.0, 48.0, 2160, 3840, ctx, None);
        b.seq.tracks[0].clips[0].blend_mode = klipp_effects::BlendMode::Screen;
        b.seq.tracks[0].clips[0].apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&klipp_effects::BuiltinEffect::Opacity { value: 0.5 })]).expect("an opacity is a placed edit").expect("the opacity lands on the picture");
        b
    })
}

pub fn s13_uhd_luma_matte(cpu_only: bool) -> ScenarioResult {
    run("s13_composite_uhd_luma_matte", cpu_only, UHD_BUDGET, |ctx| {
        let mut b = sequence(2160, 3840, &[TrackKind::Video, TrackKind::Video, TrackKind::Video]);
        let source = add_media(&mut b, 0, 0.0, 48.0, 2160, 3840, ctx, None);
        add_media(&mut b, 1, 0.0, 48.0, 2160, 3840, ctx, None);
        add_media(&mut b, 2, 0.0, 48.0, 2160, 3840, ctx, None);
        b.seq.tracks[1].clips[0].matte_source = Some(klipp_timeline::MatteBinding {
            source_clip_id: source,
            channel: klipp_nodes::params::MatteChannel::Luminance,
            invert: false,
            feather_px: 0.0,
            choke_px: 0.0,
        });
        b
    })
}

pub fn s14_hd_five_layers(cpu_only: bool) -> ScenarioResult {
    run("s14_composite_1080p_five_layers", cpu_only, HD_BUDGET, |ctx| {
        let mut b = sequence(1920, 1080, &[TrackKind::Text, TrackKind::Text, TrackKind::Video, TrackKind::Video, TrackKind::Video]);
        b.seq.tracks[0].clips.push(clip(lower_third("Five layers"), 0.0, 10.0));
        b.seq.tracks[1].clips.push(clip(credits(), 0.0, 10.0));
        add_media(&mut b, 2, 0.0, 48.0, 1920, 1080, ctx, None);
        add_media(&mut b, 3, 0.0, 48.0, 1920, 1080, ctx, Some(grade3()));
        add_media(&mut b, 4, 0.0, 48.0, 1920, 1080, ctx, None);
        b.seq.tracks[2].clips[0].blend_mode = klipp_effects::BlendMode::Overlay;
        b.seq.tracks[2].clips[0].apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&klipp_effects::BuiltinEffect::Opacity { value: 0.6 })]).expect("an opacity is a placed edit").expect("the opacity lands on the picture");
        b
    })
}
