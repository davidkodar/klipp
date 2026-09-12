//! The viewer's two playback fast paths give the shared renderer's picture.
//!
//! Everything the viewer shows goes through `klipp_compose::Renderer`,
//! except frames that take a fast path: the decoder's bytes shown as they
//! are (direct present), or the clip's grade run from the decoder's GPU
//! buffer straight to display bytes (`klipp_engine::fast_path`). The engine
//! takes those only when the renderer would render the one clip and
//! nothing else; these tests pin that, then, the pictures agree.
//!
//! The graded clip starts two seconds into the timeline and ten seconds
//! into its media, with an exposure keyframed across it: a fast path that
//! evaluated the graph at source time — as the viewer's once did — would
//! read the ramp's end, a stop brighter.

use std::sync::Arc;

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::display::{display_transform_ws, ToneMapOp};
use klipp_color::hdr::HdrTransfer;
use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{color_ids, ColorPrimaries, LogFormat, RationalTime, Timecode, WorkingColorspace};
use klipp_effects::gpu_context::GpuContext;
use klipp_effects::p010_gpu::GpuP010Converter;
use klipp_engine::fast_path::{grade_to_display_on_gpu, GpuSeed, GradeAt};
use klipp_nodes::{
    graph::{Edge, Node, NodeGraph, NodeId, NodePos},
    kinds::NodeKind,
    params::{NodeKeyframeTrack, NodeKeyframeTracks, NodeParams},
};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 64;
const H: u32 = 32;
const WORKING: WorkingColorspace = WorkingColorspace::Rec709Linear;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn graph(nodes: Vec<(NodeKind, NodeParams, NodeKeyframeTracks)>) -> NodeGraph {
    let mut all = vec![(NodeKind::Source, NodeParams::default(), NodeKeyframeTracks::default())];
    all.extend(nodes);
    all.push((NodeKind::Output, NodeParams::default(), NodeKeyframeTracks::default()));
    let nodes: Vec<Node> = all
        .into_iter()
        .map(|(kind, params, keyframes)| Node {
            id: NodeId::new_v4(),
            kind,
            label: format!("{kind:?}"),
            enabled: true,
            params,
            keyframes,
            output_link: None,
            input_links: vec![],
        })
        .collect();
    let edges = (0..nodes.len() - 1)
        .map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 })
        .collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph {
        output_node_id: nodes.last().unwrap().id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    }
}

/// Exposure ramping 0 → 2 stops over the clip's first four seconds, then
/// a warmed white balance.
fn keyframed_grade(input_transform: Option<LogFormat>) -> NodeGraph {
    let mut keys = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
    klipp_core::keyframe::upsert_keyframe(&mut keys, 4.0, 2.0);
    let mut kf = NodeKeyframeTracks::default();
    kf.tracks.push(NodeKeyframeTrack { param_name: "exposure".into(), keyframes: keys });
    let mut exposure = NodeParams::default();
    exposure.exposure = Some(0.0);
    let mut balance = NodeParams::default();
    balance.temperature = Some(0.08);
    balance.global_mix = Some(1.0);
    let mut nodes = Vec::new();
    if let Some(format) = input_transform {
        let mut it = NodeParams::default();
        it.log_format = Some(format);
        nodes.push((NodeKind::InputTransform, it, NodeKeyframeTracks::default()));
    }
    nodes.push((NodeKind::Exposure, exposure, kf));
    nodes.push((NodeKind::ColorBalance, balance, NodeKeyframeTracks::default()));
    graph(nodes)
}

/// One media clip, two seconds into the timeline and ten into its media.
fn one_clip(node_graph: NodeGraph) -> (Sequence, Clip) {
    let mut clip = Clip {
        kind: ClipKind::Media { media_id: klipp_core::MediaId::new() },
        ..Clip::gap(tc(2.0), tc(8.0))
    };
    clip.source_in = tc(10.0);
    clip.source_out = tc(16.0);
    clip.node_graph = node_graph;
    let mut seq = Sequence::new_default();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![clip.clone()];
    seq.tracks = vec![track];
    (seq, clip)
}

/// The renderer's frame source: the one frame, whatever is asked.
struct Frame(Box<dyn Fn() -> SourcePixels>);

impl FrameSource for Frame {
    fn frame(&mut self, _clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: (self.0)(), width, height, pts: None })
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_working_colorspace(WORKING);
    r
}

/// The renderer's frame at `t`, through the viewer's display encode.
fn rendered(r: &mut Renderer, seq: &Sequence, t: f64, src: &mut Frame) -> Vec<u8> {
    let linear = r.render(seq, tc(t), W, H, src, RenderOptions::default());
    display_transform_ws(&linear, ToneMapOp::None, WORKING)
}

/// The graded fast path's frame for `clip` at `t`, through the engine's
/// function, on the renderer's executor as the engine drives it.
fn fast(r: &mut Renderer, clip: &Clip, t: f64, seed: GpuSeed<'_>) -> Vec<u8> {
    let (clip_time, frame_number) = klipp_compose::render::grade_time(clip, tc(t), 24.0);
    let at = GradeAt {
        graph: &clip.node_graph,
        clip_time,
        frame_number,
        track_points: &clip.track_points,
        working: WORKING,
        lut_tetrahedral: true,
        solo: None,
    };
    let display = |l: &[f32]| display_transform_ws(l, ToneMapOp::None, WORKING);
    grade_to_display_on_gpu(r.executor(), seed, W, H, &at, &display).expect("the fast path takes this frame")
}

fn worst_lsb(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .enumerate()
        .filter(|(i, _)| i % 4 != 3)
        .map(|(_, (x, y))| (*x as i32 - *y as i32).abs())
        .max()
        .unwrap()
}

fn sdr_bytes() -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H {
        for col in 0..W {
            let i = row * W + col;
            v.extend_from_slice(&[(i % 256) as u8, ((i * 7 + 40) % 256) as u8, ((i * 13 + 90) % 256) as u8, 255]);
        }
    }
    v
}

fn gpu_buffer(ctx: &GpuContext, bytes: &[u8]) -> Arc<wgpu::Buffer> {
    let buf = Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fast_paths_match_renderer_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    ctx.queue.write_buffer(&buf, 0, bytes);
    ctx.device.poll(wgpu::Maintain::Poll);
    buf
}

/// Direct present shows the decoder's bytes. For a clip alone with no
/// grade, the renderer's frame is those bytes, exactly.
#[test]
fn direct_present_is_the_renderers_frame() {
    let bytes = sdr_bytes();
    let (seq, _) = one_clip(NodeGraph::default());
    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    for gpu in [false, true] {
        let mut r = renderer();
        r.set_gpu_enabled(gpu);
        let got = rendered(&mut r, &seq, 4.0, &mut src);
        assert_eq!(got, bytes, "renderer (gpu={gpu}) changed an ungraded frame");
    }
}

/// An SDR clip graded on the GPU from the decoder's buffer matches the
/// renderer's frame to one level.
#[test]
fn the_graded_fast_path_is_the_renderers_frame() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let bytes = sdr_bytes();
    let buffer = gpu_buffer(ctx, &bytes);
    let (seq, clip) = one_clip(keyframed_grade(None));
    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    let mut r = renderer();
    let want = rendered(&mut r, &seq, 4.0, &mut src);
    let got = fast(&mut r, &clip, 4.0, GpuSeed::Srgb8(&buffer));
    let worst = worst_lsb(&got, &want);
    eprintln!("SDR graded fast path vs renderer: worst {worst} LSB");
    assert!(worst <= 1, "the graded fast path differs from the renderer by {worst} levels");

    // And the time is the clip's own: at source time the exposure would be
    // a stop brighter.
    let (_, frame_number) = klipp_compose::render::grade_time(&clip, tc(4.0), 24.0);
    let at_source_time = GradeAt {
        graph: &clip.node_graph,
        clip_time: 12.0,
        frame_number,
        track_points: &[],
        working: WORKING,
        lut_tetrahedral: true,
        solo: None,
    };
    let display = |l: &[f32]| display_transform_ws(l, ToneMapOp::None, WORKING);
    let wrong = grade_to_display_on_gpu(r.executor(), GpuSeed::Srgb8(&buffer), W, H, &at_source_time, &display).unwrap();
    assert!(worst_lsb(&wrong, &want) > 20, "the keyframes do not tell clip time from source time");
}


/// The same clip in a BT.2020 project. The GPU's display encode is an
/// sRGB encode alone — no basis change, no gamut map — so the fast path
/// has to finish a wide-gamut frame through the viewer's own transform
/// instead. Encoding BT.2020 values as though they were BT.709, which is
/// what keeping that frame on the GPU does, over-saturates every colour
/// in the picture; the last assertion measures that difference, so this
/// test fails if the fast path ever starts taking the shortcut again.
#[test]
fn the_graded_fast_path_is_the_renderers_frame_in_a_bt2020_project() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    const WS: WorkingColorspace = WorkingColorspace::Bt2020Linear;
    let bytes = sdr_bytes();
    let buffer = gpu_buffer(ctx, &bytes);
    let (seq, clip) = one_clip(keyframed_grade(None));
    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    let mut r = renderer();
    r.set_working_colorspace(WS);

    let frame = r.render(&seq, tc(4.0), W, H, &mut src, RenderOptions::default());
    let want = display_transform_ws(&frame, ToneMapOp::None, WS);

    let (clip_time, frame_number) = klipp_compose::render::grade_time(&clip, tc(4.0), 24.0);
    let at = GradeAt {
        graph: &clip.node_graph,
        clip_time,
        frame_number,
        track_points: &clip.track_points,
        working: WS,
        lut_tetrahedral: true,
        solo: None,
    };
    let display = |l: &[f32]| display_transform_ws(l, ToneMapOp::None, WS);
    let got = grade_to_display_on_gpu(r.executor(), GpuSeed::Srgb8(&buffer), W, H, &at, &display)
        .expect("the fast path takes this frame");
    let worst = worst_lsb(&got, &want);
    eprintln!("BT.2020 graded fast path vs renderer: worst {worst} LSB");
    assert!(worst <= 1, "the graded fast path differs from the renderer by {worst} levels");

    let as_if_709 = display_transform_ws(&frame, ToneMapOp::None, WorkingColorspace::Rec709Linear);
    let shortcut = worst_lsb(&as_if_709, &want);
    eprintln!("the GPU's sRGB-only encode would differ by {shortcut} LSB");
    assert!(shortcut > 8, "this picture cannot tell the two encodes apart; pick a more saturated source");
}

/// Direct present must never take an AcesCG project, and this measures
/// the size of the mistake if it ever does.
///
/// That path hands the decoder's own sRGB bytes straight to the viewer,
/// on the argument that grading nothing and re-encoding lands back on
/// the bytes it started from. True in BT.709. Emphatically not true
/// under ACES, where the output transform changes every pixel of an
/// *ungraded* frame — so the blocker list has to name something for an
/// AcesCG project, and `fast_path_blockers` pushes "ACES working space"
/// for exactly that reason while the graded gates tolerate it.
///
/// `fast_path_blockers` is private, so this asserts the property the
/// blocker exists to protect rather than the blocker itself: if the two
/// ever became interchangeable, this would be the test that stopped
/// agreeing.
#[test]
fn an_ungraded_frame_is_not_its_own_bytes_in_an_aces_project() {
    let bytes = sdr_bytes();
    let (seq, _) = one_clip(NodeGraph::default());
    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    let mut r = renderer();
    r.set_working_colorspace(WorkingColorspace::AcesCG);

    let frame = r.render(&seq, tc(4.0), W, H, &mut src, RenderOptions::default());
    let aces = display_transform_ws(&frame, ToneMapOp::None, WorkingColorspace::AcesCG);
    let worst = worst_lsb(&aces, &bytes);
    eprintln!("ungraded ACES render vs the decoder's own bytes: worst {worst} LSB");
    assert!(
        worst > 8,
        "an ungraded ACES render came back within {worst} levels of its input, so direct \
         present would look almost right and this test would not notice it being wrong"
    );
}

/// An AcesCG project's graded fast path is the renderer's frame.
///
/// The companion to the BT.2020 case above, and its opposite
/// conclusion: BT.2020 has no GPU display transform yet and must fall
/// back, while ACES has its whole output transform in a shader
/// (`SHADER_ACES_REC709_U8`, matched to `acescg_to_rec709_sdr` within a
/// code value) and so keeps the frame on the GPU. That is worth 88 ms a
/// 4K frame — the reason an AcesCG project graded and exported six
/// times slower than a Rec.709 one.
///
/// The last assertion is the one that matters: it measures what
/// encoding AcesCG values as though they were BT.709 would look like.
/// If the fast path ever loses the ACES branch and takes the plain sRGB
/// encode instead, the picture goes wrong rather than slow, and this is
/// what notices.
#[test]
fn the_graded_fast_path_is_the_renderers_frame_in_an_aces_project() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    const WS: WorkingColorspace = WorkingColorspace::AcesCG;
    let bytes = sdr_bytes();
    let buffer = gpu_buffer(ctx, &bytes);
    let (seq, clip) = one_clip(keyframed_grade(None));
    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    let mut r = renderer();
    r.set_working_colorspace(WS);

    let frame = r.render(&seq, tc(4.0), W, H, &mut src, RenderOptions::default());
    let want = display_transform_ws(&frame, ToneMapOp::None, WS);

    let (clip_time, frame_number) = klipp_compose::render::grade_time(&clip, tc(4.0), 24.0);
    let at = GradeAt {
        graph: &clip.node_graph,
        clip_time,
        frame_number,
        track_points: &clip.track_points,
        working: WS,
        lut_tetrahedral: true,
        solo: None,
    };
    // Deliberately a tone map the viewer would apply elsewhere: the ACES
    // path ignores it, on both the CPU and the GPU, and if one of them
    // ever stopped ignoring it the two would part company here.
    let display = |l: &[f32]| display_transform_ws(l, ToneMapOp::Hable, WS);
    let got = grade_to_display_on_gpu(r.executor(), GpuSeed::Srgb8(&buffer), W, H, &at, &display)
        .expect("the fast path takes this frame");
    let worst = worst_lsb(&got, &want);
    eprintln!("ACES graded fast path vs renderer: worst {worst} LSB");
    assert!(worst <= 1, "the graded fast path differs from the renderer by {worst} levels");

    let as_if_709 = display_transform_ws(&frame, ToneMapOp::None, WorkingColorspace::Rec709Linear);
    let shortcut = worst_lsb(&as_if_709, &want);
    eprintln!("the GPU's sRGB-only encode would differ by {shortcut} LSB");
    assert!(
        shortcut > 8,
        "this picture cannot tell the ACES transform from a plain sRGB encode; \
         pick a source that can"
    );
}

/// A PQ clip graded on the GPU from the decoder's codes matches the
/// renderer's frame — scene-linear grade, display tone map — to one level.
#[test]
fn the_graded_fast_path_is_the_renderers_frame_for_pq() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let mut y = Vec::new();
    for row in 0..H {
        for col in 0..W {
            let v: u32 = 64 + ((col * 37 + row * 11) % 877);
            y.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    let mut uv = Vec::new();
    for row in 0..(H / 2) {
        for col in 0..(W / 2) {
            let u = 512 + (((col as i32 - row as i32) * 23).rem_euclid(300)) as u32 - 150;
            let v = 512 + (((col as i32 + row as i32) * 29).rem_euclid(300)) as u32 - 150;
            uv.extend_from_slice(&(((u & 0x3FF) << 6) as u16).to_le_bytes());
            uv.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WORKING);
    let matrix4 = klipp_effects::color_wgsl::pack_primaries_matrix(m);
    let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT2020_NCL, color_ids::TRANSFER_PQ, color_ids::PRIMARIES_BT2020];
    let codes = conv.convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, matrix4).expect("CPU codes");
    let gpu_codes = conv.convert_p010_on_gpu(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, matrix4).hdr_codes.expect("GPU codes");

    // The graph a PQ clip is seeded with: its input transform bypasses,
    // the decoder having delivered codes.
    let (seq, clip) = one_clip(keyframed_grade(Some(LogFormat::PQ)));
    let codes = Arc::new(codes);
    let mut src = Frame(Box::new(move || SourcePixels::SceneReferred {
        codes: Arc::clone(&codes),
        transfer: HdrTransfer::Pq,
        source_to_working: m,
    }));
    let mut r = renderer();
    let want = rendered(&mut r, &seq, 4.0, &mut src);
    let seed = GpuSeed::SceneReferred { buffer: &gpu_codes, transfer: HdrTransfer::Pq, source_to_working: m };
    let got = fast(&mut r, &clip, 4.0, seed);
    let worst = worst_lsb(&got, &want);
    eprintln!("PQ graded fast path vs renderer: worst {worst} LSB");
    assert!(worst <= 1, "the graded fast path differs from the renderer by {worst} levels");
}

/// A decoder frame left on the GPU, as the viewer hands it to the
/// renderer: the grade seeds from it; `read` counts readbacks.
struct OnGpu {
    buffer: Arc<wgpu::Buffer>,
    bytes: Vec<u8>,
    reads: std::sync::atomic::AtomicUsize,
}

impl klipp_compose::GpuPixels for OnGpu {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_gpu_u8_buffer(&self.buffer, width, height)
    }
    fn read(&self) -> Arc<Vec<f32>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.bytes))
    }
}

/// Over another layer, a graded clip whose frame is still on the GPU
/// renders as its read-back pixels do — the grade starting from the GPU
/// buffer, never reading the frame back.
#[test]
fn a_gpu_resident_frame_is_graded_where_it_is() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let bytes = sdr_bytes();
    let on_gpu = Arc::new(OnGpu {
        buffer: gpu_buffer(ctx, &bytes),
        bytes: bytes.clone(),
        reads: Default::default(),
    });
    let (mut seq, _) = one_clip(keyframed_grade(None));
    // A half-transparent title above: the frame is a composite, not a
    // fast-path frame.
    let mut title = Clip {
        kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 255, 255, 255] }),
        ..Clip::gap(tc(0.0), tc(10.0))
    };
    title.builtin_effects = vec![klipp_effects::BuiltinEffect::Opacity { value: 0.25 }];
    let mut top = Track::new("V2", TrackKind::Video);
    top.clips = vec![title];
    seq.tracks.insert(0, top);

    let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
    let mut on_cpu = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
    let g = Arc::clone(&on_gpu);
    let mut resident = Frame(Box::new(move || SourcePixels::Gpu(Arc::clone(&g) as Arc<dyn klipp_compose::GpuPixels>)));

    let mut r = renderer();
    let want = rendered(&mut r, &seq, 4.0, &mut on_cpu);
    let got = rendered(&mut r, &seq, 4.0, &mut resident);
    let worst = worst_lsb(&got, &want);
    eprintln!("GPU-resident graded source vs CPU source: worst {worst} LSB");
    assert!(worst <= 1, "a GPU-resident source renders {worst} levels off");
    assert_eq!(on_gpu.reads.load(std::sync::atomic::Ordering::Relaxed), 0, "the frame was read back");

    // Ungraded, there is no grade to start: it is read back, once.
    let (mut plain, _) = one_clip(NodeGraph::default());
    plain.tracks.insert(0, seq.tracks[0].clone());
    let want = rendered(&mut r, &plain, 4.0, &mut on_cpu);
    let got = rendered(&mut r, &plain, 4.0, &mut resident);
    assert!(worst_lsb(&got, &want) <= 1);
    assert_eq!(on_gpu.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// Packed PQ codes across the range, varied per pixel and channel.
fn pq_codes(shift: u32) -> Vec<u32> {
    (0..W * H)
        .map(|i| {
            let c = |k: u32| 64 + ((i * k + shift) % 877);
            klipp_color::hdr::pack_rgb10(c(37), c(11), c(23))
        })
        .collect()
}

/// A parked frame — pixels in memory, not a decoder's GPU buffer — graded
/// by the fast path from a seed the executor uploads and keeps: SDR bytes
/// and PQ codes both give the renderer's frame, the reference decoded on
/// the CPU. Seeding the same frame again reuses the upload; another frame
/// replaces it.
#[test]
fn a_parked_frame_seeded_from_memory_is_the_renderers_frame() {
    if GpuContext::shared().is_none() {
        eprintln!("skipping: no usable GpuContext");
        return;
    }
    let (seq, clip) = one_clip(keyframed_grade(None));
    let mut r = renderer();
    let mut cpu = renderer();
    cpu.set_gpu_enabled(false);

    // SDR bytes, and then another frame of them.
    let mut last = Vec::new();
    let inverted = sdr_bytes().iter().enumerate().map(|(i, b)| if i % 4 == 3 { *b } else { 255 - b }).collect();
    for bytes in [sdr_bytes(), inverted] {
        let bytes = Arc::new(bytes);
        let linear = Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&bytes));
        let mut src = Frame(Box::new(move || SourcePixels::Linear(Arc::clone(&linear))));
        let want = rendered(&mut cpu, &seq, 4.0, &mut src);
        let got = fast(&mut r, &clip, 4.0, GpuSeed::Srgb8Bytes(&bytes));
        let again = fast(&mut r, &clip, 4.0, GpuSeed::Srgb8Bytes(&bytes));
        let worst = worst_lsb(&got, &want);
        eprintln!("SDR parked seed vs renderer: worst {worst} LSB");
        assert!(worst <= 1, "an SDR parked seed differs from the renderer by {worst} levels");
        assert_eq!(got, again, "grading the kept seed again changed the frame");
        assert_ne!(got, last, "a new frame did not replace the kept seed");
        last = got;
    }

    // PQ codes, twice over.
    let (seq, clip) = one_clip(keyframed_grade(Some(LogFormat::PQ)));
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WORKING);
    for shift in [0, 400] {
        let codes = Arc::new(pq_codes(shift));
        let c = Arc::clone(&codes);
        let mut src = Frame(Box::new(move || SourcePixels::SceneReferred {
            codes: Arc::clone(&c),
            transfer: HdrTransfer::Pq,
            source_to_working: m,
        }));
        let want = rendered(&mut cpu, &seq, 4.0, &mut src);
        let seed = || GpuSeed::SceneCodes { codes: &codes, transfer: HdrTransfer::Pq, source_to_working: m };
        let got = fast(&mut r, &clip, 4.0, seed());
        let again = fast(&mut r, &clip, 4.0, seed());
        let worst = worst_lsb(&got, &want);
        eprintln!("PQ parked seed vs renderer: worst {worst} LSB");
        assert!(worst <= 1, "a PQ parked seed differs from the renderer by {worst} levels");
        assert_eq!(got, again);
        // The renderer seeds scene-referred codes on the GPU itself, too.
        let seeded = rendered(&mut r, &seq, 4.0, &mut src);
        assert!(worst_lsb(&seeded, &want) <= 1, "the renderer's own GPU seed is off");
    }
}
