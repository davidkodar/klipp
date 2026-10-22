//! Temporal noise reduction on the executor's GPU chain (#1988): a lone
//! temporal-denoise node draws on the chain, against the same source
//! neighbours the CPU route reads, and the picture is the CPU route's
//! within a level of 8-bit sRGB: alone, after an input transform, and
//! after another node (its current frame the picture coming in, its
//! neighbours the source's). Its declines are the CPU route's. A mixed
//! one, and one a parked frame is to hold, keep the CPU route. A missing
//! GPU leaves this unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_core::{ClipId, MediaId};
use klipp_effects::BuiltinEffect as E;
use klipp_nodes::frame_context::{BoundedSources, EncodedFrame, FrameContext, LinearFrame, Neighbour, NoAnalysis, PluginInstances, SourceProvider};
use klipp_nodes::graph::{Edge, Node, NodeGraph, NodeId};
use klipp_nodes::kinds::NodeKind;
use klipp_nodes::params::{EffectBlend, NodeParams};
use klipp_nodes::{Decline, GraphExecutor};

const W: u32 = 24;
const H: u32 = 10;
const FPS: f64 = 25.0;

/// The frame drawn: a ramp with a little structure.
fn frame() -> Vec<f32> {
    (0..W * H).flat_map(|i| {
        let x = (i % W) as f32 / W as f32;
        [0.05 + 0.6 * x, 0.3 + 0.1 * ((i / W) % 2) as f32, 0.5 - 0.3 * x, 1.0]
    }).collect()
}

/// A media of `end` seconds whose frames each differ a little from the
/// drawn one, by their time: near enough to be averaged, far enough to
/// show.
struct Media {
    end: f64,
}

impl SourceProvider for Media {
    fn bounds(&self) -> (f64, f64) {
        (0.0, self.end)
    }
    fn frame_secs(&self) -> Option<f64> {
        Some(1.0 / FPS)
    }
    fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
        Neighbour::Frame(EncodedFrame { rgba8: vec![0; (W * H * 4) as usize], width: W, height: H, source_secs })
    }
    fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
        let lift = (source_secs * 3.0).fract() as f32 * 0.08;
        let rgba = frame().chunks_exact(4).enumerate().flat_map(|(i, p)| [p[0] + lift, p[1] + lift * (i % 3) as f32 * 0.5, (p[2] - lift).max(0.0), 1.0]).collect();
        Neighbour::Frame(LinearFrame { rgba: Arc::new(rgba), width: W, height: H, source_secs, pts: None })
    }
}

fn node(kind: NodeKind, params: NodeParams) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
}

fn denoise() -> Node {
    node(NodeKind::Effect, NodeParams { effects: vec![E::TemporalDenoise { strength: 1.0, threshold: 0.2 }], ..Default::default() })
}

/// The nodes in a line between a Source and an Output.
fn chain(middle: Vec<Node>) -> NodeGraph {
    let mut nodes = vec![node(NodeKind::Source, NodeParams::default())];
    nodes.extend(middle);
    nodes.push(node(NodeKind::Output, NodeParams::default()));
    let edges = nodes.windows(2).map(|w| Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
    NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

struct Ran {
    pixels: Vec<f32>,
    declines: Vec<Decline>,
    on_chain: u32,
}

fn run(ex: &mut GraphExecutor, g: &NodeGraph, at: f64, end: f64) -> Ran {
    let mut media = Media { end };
    let mut bounded = BoundedSources::new(&mut media, W, H);
    let mut instances = PluginInstances::default();
    let mut ctx = FrameContext {
        clip: ClipId::new(),
        frame: 10,
        source: (MediaId::nil(), 0.0),
        clip_time_secs: at,
        source_secs: at,
        source_pts: Some(at),
        frame_number: 10,
        fps: FPS,
        sources: &mut bounded,
        analysis: &NoAnalysis,
        instances: &mut instances,
        ofx: None,
        source_at: &|t| t,
    };
    let pixels = ex.execute_linear_with_context(&frame(), g, W, H, at, 10, None, None, None, &mut ctx).expect("the run").0;
    Ran { pixels, declines: ex.last_run_declines().to_vec(), on_chain: ex.last_denoise_on_chain() }
}

fn gpu() -> Option<GraphExecutor> {
    let mut ex = GraphExecutor::new();
    let ok = ex.gpu_pipeline_or_init().is_some();
    assert!(ok || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
    if !ok {
        eprintln!("UNVERIFIED: no GPU");
    }
    ok.then_some(ex)
}

fn srgb8(v: f32) -> i32 {
    (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as i32
}

/// The worst difference between two pictures in 8-bit sRGB levels.
fn levels(a: &[f32], b: &[f32]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(p, q)| (0..3).map(|c| (srgb8(p[c]) - srgb8(q[c])).abs()).max().unwrap()).max().unwrap()
}

#[test]
fn a_temporal_denoise_draws_on_the_chain_as_the_cpu_draws_it() {
    let Some(mut on_gpu) = gpu() else { return };
    let mut on_cpu = GraphExecutor::new_cpu_only();
    let cases = [
        ("alone", chain(vec![denoise()])),
        ("after an input transform", chain(vec![node(NodeKind::InputTransform, NodeParams::default()), denoise()])),
        ("after another node, a grade after it", chain(vec![node(NodeKind::Exposure, NodeParams { exposure: Some(0.7), ..Default::default() }), denoise(), node(NodeKind::Exposure, NodeParams { exposure: Some(-0.3), ..Default::default() })])),
    ];
    for (what, g) in cases {
        let (cpu, gpu) = (run(&mut on_cpu, &g, 0.4, 10.0), run(&mut on_gpu, &g, 0.4, 10.0));
        assert_eq!(gpu.on_chain, 1, "{what}: drawn on the chain");
        assert_eq!(cpu.on_chain, 0, "{what}: the CPU executor has no chain");
        let off = levels(&gpu.pixels, &cpu.pixels);
        assert!(off <= 1, "{what}: {off} levels off the CPU route");
        assert!(levels(&cpu.pixels, &run(&mut on_cpu, &chain(vec![]), 0.4, 10.0).pixels) > 1 || what != "alone", "{what}: the denoise changed the picture");
        assert_eq!(gpu.declines, cpu.declines, "{what}: the same declines");
    }
}

/// The viewer's neighbours (#1988): its streams hold their frames on the
/// GPU as sRGB bytes, which the chain decodes to linear there. Against a
/// source that reads the same bytes on the CPU, the chain's picture is
/// the CPU route's within a level, with nothing uploaded.
#[test]
fn a_temporal_denoise_binds_neighbours_the_gpu_holds_as_srgb_bytes() {
    use klipp_nodes::frame_context::{GpuFramePixels, GpuLinearFrame};
    struct Bytes {
        buf: wgpu::Buffer,
        linear: Arc<Vec<f32>>,
    }
    impl GpuFramePixels for Bytes {
        fn buffer(&self) -> &wgpu::Buffer {
            &self.buf
        }
        fn read(&self) -> Arc<Vec<f32>> {
            Arc::clone(&self.linear)
        }
        fn srgb8(&self) -> bool {
            true
        }
    }
    /// [`Media`]'s frames as 8-bit sRGB: on the CPU decoded, on the GPU
    /// as the bytes.
    struct Srgb8Media {
        inner: Media,
        gpu: Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>,
    }
    impl Srgb8Media {
        fn bytes(&mut self, source_secs: f64) -> Option<Vec<u8>> {
            let Neighbour::Frame(f) = self.inner.linear(source_secs) else { return None };
            Some(klipp_effects::linear_f32_rgba_to_srgb_u8(&f.rgba))
        }
    }
    impl SourceProvider for Srgb8Media {
        fn bounds(&self) -> (f64, f64) {
            self.inner.bounds()
        }
        fn frame_secs(&self) -> Option<f64> {
            self.inner.frame_secs()
        }
        fn encoded(&mut self, source_secs: f64) -> Neighbour<EncodedFrame> {
            self.inner.encoded(source_secs)
        }
        fn linear(&mut self, source_secs: f64) -> Neighbour<LinearFrame> {
            let Some(bytes) = self.bytes(source_secs) else { return self.inner.linear(source_secs) };
            Neighbour::Frame(LinearFrame { rgba: Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes)), width: W, height: H, source_secs, pts: None })
        }
        fn linear_on_gpu(&mut self, source_secs: f64) -> Neighbour<GpuLinearFrame> {
            let (Some((device, queue)), Some(bytes)) = (self.gpu.clone(), self.bytes(source_secs)) else {
                return Neighbour::Missing(klipp_nodes::frame_context::Missing::Unavailable);
            };
            let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes.len() as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&buf, 0, &bytes);
            let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
            Neighbour::Frame(GpuLinearFrame { pixels: Arc::new(Bytes { buf, linear }), width: W, height: H, source_secs })
        }
    }
    let Some(mut on_gpu) = gpu() else { return };
    let device = on_gpu.gpu_pipeline_or_init().map(|g| (g.device(), g.queue()));
    let mut on_cpu = GraphExecutor::new_cpu_only();
    let g = chain(vec![denoise()]);
    let ran = |ex: &mut GraphExecutor, gpu: Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>| {
        let mut media = Srgb8Media { inner: Media { end: 10.0 }, gpu };
        let mut bounded = BoundedSources::new(&mut media, W, H);
        let mut instances = PluginInstances::default();
        let mut ctx = FrameContext { clip: ClipId::new(), frame: 10, source: (MediaId::nil(), 0.0), clip_time_secs: 0.4, source_secs: 0.4, source_pts: Some(0.4), frame_number: 10, fps: FPS, sources: &mut bounded, analysis: &NoAnalysis, instances: &mut instances, ofx: None, source_at: &|t| t };
        ex.execute_linear_with_context(&frame(), &g, W, H, 0.4, 10, None, None, None, &mut ctx).expect("the run").0
    };
    let cpu = ran(&mut on_cpu, None);
    let gpu = ran(&mut on_gpu, device);
    assert_eq!(on_gpu.last_denoise_on_chain(), 1, "drawn on the chain");
    assert_eq!(on_gpu.last_neighbour_uploads(), 0, "its neighbours bound from the GPU, none uploaded");
    let off = levels(&gpu, &cpu);
    assert!(off <= 1, "{off} levels off the CPU route");
}

/// A neighbour past the media's end is left out and declined the same
/// way on both routes, and the frame still denoises against the other.
#[test]
fn a_missing_neighbour_is_declined_as_the_cpu_route_declines_it() {
    let Some(mut on_gpu) = gpu() else { return };
    let mut on_cpu = GraphExecutor::new_cpu_only();
    let g = chain(vec![denoise()]);
    // The frame after 0.4 is past an end at 0.42.
    let (cpu, gpu) = (run(&mut on_cpu, &g, 0.4, 0.42), run(&mut on_gpu, &g, 0.4, 0.42));
    assert!(!cpu.declines.is_empty(), "the missing neighbour is declined");
    assert_eq!(gpu.declines, cpu.declines);
    assert_eq!(gpu.on_chain, 1);
    assert!(levels(&gpu.pixels, &cpu.pixels) <= 1);
}

/// A mixed temporal denoise keeps the CPU route, and draws the same.
#[test]
fn a_mixed_temporal_denoise_keeps_the_cpu_route() {
    let Some(mut on_gpu) = gpu() else { return };
    let mut mixed = denoise();
    mixed.params.effect_blend = Some(EffectBlend { mix: 0.5, channel: klipp_effects::mask_mix::MaskChannel::Red });
    let g = chain(vec![mixed]);
    let got = run(&mut on_gpu, &g, 0.4, 10.0);
    assert_eq!(got.on_chain, 0, "mixed: the CPU route");
    assert!(levels(&got.pixels, &run(&mut GraphExecutor::new_cpu_only(), &g, 0.4, 10.0).pixels) <= 1);
}
