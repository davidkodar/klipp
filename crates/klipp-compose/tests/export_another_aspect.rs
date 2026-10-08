//! A source of another aspect than the sequence's is exported on the GPU
//! (#1277): the single-clip path hands it to the stack, which fits it as
//! the CPU fits it (bars, no stretch, no crop), instead of refusing it and
//! sending every frame of, say, a vertical clip on a 16:9 sequence down the
//! CPU composite.
//!
//! The picture varies across the frame and is asymmetric (a marked corner),
//! so a wrong fit, a mirrored or shifted mapping, or a stretch shows. Each
//! case is held two ways:
//! - its luma to the ordinary render's (the CPU fit and encode);
//! - all three planes, chroma too, to the same picture fitted first and
//!   exported at the sequence's size, which takes the single-clip path.
//!
//! The source counts its fetches: the single-clip path fetches a clip once,
//! and handing it to the stack fetches it again, which is how the test tells
//! which path took the frame.
//!
//! The stack takes an SDR delivery in a Rec.709 project; those are what this
//! covers. A missing GPU leaves it unverified.

use std::cell::Cell;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn target() -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: LUMA_709,
        semi_planar: false,
        bits: 8,
        sample_shift: 0,
    }
}

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn media() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(1.0)) }
}

/// The clip with a half-stop grade, so the graded arm of `grade` runs.
fn graded(mut c: Clip) -> Clip {
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(NodeKind::Exposure, NodeParams { exposure: Some(0.5), ..Default::default() }),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    c.set_grade(NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] });
    c
}

fn sequence(c: &Clip, w: u32, h: u32) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![c.clone()];
    seq.tracks = vec![t];
    seq
}

/// A gradient across and down, its top-left corner marked bright red, at
/// `w` by `h`: sRGB bytes.
fn picture(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| {
            (0..w).flat_map(move |x| {
                if x < w.div_ceil(5) && y < h.div_ceil(5) {
                    [235, 30, 30, 255]
                } else {
                    [(40 + x * 180 / w.max(1)) as u8, (40 + y * 180 / h.max(1)) as u8, (200 - (x + y) * 120 / (w + h)) as u8, 255]
                }
            })
        })
        .collect()
}

/// A frame the decoder left on the GPU: sRGB bytes at the source's own
/// size, seeding at that size only, as export's frames do.
struct Seeded {
    srgb: Arc<Vec<u8>>,
    w: u32,
    h: u32,
}

impl klipp_compose::GpuPixels for Seeded {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        (width, height) == (self.w, self.h) && exec.prime_chain_from_srgb8(&self.srgb, width, height)
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::new(klipp_effects::srgb_u8_to_linear_f32_rgba(&self.srgb))
    }
}

/// One clip's frame, `w` by `h`, from the GPU; counts its fetches.
struct Source {
    srgb: Arc<Vec<u8>>,
    w: u32,
    h: u32,
    fetches: Cell<u32>,
}

impl Source {
    fn new(srgb: Vec<u8>, w: u32, h: u32) -> Self {
        Source { srgb: Arc::new(srgb), w, h, fetches: Cell::new(0) }
    }
}

impl FrameSource for Source {
    fn frame(&mut self, _clip: &Clip, _secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        self.fetches.set(self.fetches.get() + 1);
        Some(SourceFrame { pixels: SourcePixels::Gpu(Arc::new(Seeded { srgb: Arc::clone(&self.srgb), w: self.w, h: self.h })), width: self.w, height: self.h, pts: None })
    }
}

fn renderer() -> Renderer {
    Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new())
}

/// The GPU path's planes, or `None` when it refused.
fn planes(seq: &Sequence, w: u32, h: u32, src: &mut Source) -> Option<Vec<u8>> {
    let mut r = renderer();
    let out = r.render_yuv420_on_gpu(seq, tc(0.5), w, h, src, RenderOptions::default(), target()).map(|(b, _)| b);
    assert!(out.is_none() || r.last_decline().is_none(), "planes with a decline: {:?}", r.last_decline());
    out
}

/// The ordinary render's luma, encoded as the CPU export encodes it.
fn cpu_luma(seq: &Sequence, w: u32, h: u32, src: &mut Source) -> Vec<u8> {
    let px = renderer().render(seq, tc(0.5), w, h, src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::OdtChoice::MatchViewer);
    px.chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = LUMA_709[0] * e[0] + LUMA_709[1] * e[1] + LUMA_709[2] * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// The picture fitted into `w` by `h` by the CPU's own rule (the
/// renderer's, read back through a CPU render of the source), as sRGB bytes
/// at the sequence's size: the reference the stack's fit is held to.
fn fitted_reference(srgb: &[u8], sw: u32, sh: u32, w: u32, h: u32) -> Vec<u8> {
    let c = media();
    let seq = sequence(&c, w, h);
    let mut src = Source::new(srgb.to_vec(), sw, sh);
    let px = renderer().render(&seq, tc(0.5), w, h, &mut src, RenderOptions::default());
    px.chunks_exact(4)
        .flat_map(|p| {
            let e = |v: f32| (klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
            [e(p[0]), e(p[1]), e(p[2]), 255]
        })
        .collect()
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// Whether this machine's GPU takes the path at all: a frame of the
/// sequence's own size. Asked once, so a refusal below is a failure, not a
/// skip.
fn gpu_path_available() -> bool {
    let c = media();
    planes(&sequence(&c, 32, 16), 32, 16, &mut Source::new(picture(32, 16), 32, 16)).is_some()
}

#[test]
fn a_source_of_another_aspect_is_exported_on_the_gpu() {
    if !gpu_path_available() {
        eprintln!("UNVERIFIED: no usable GPU");
        return;
    }
    // (sequence, source): a portrait source on a landscape sequence
    // (pillarboxed), a landscape one on a portrait sequence (letterboxed),
    // and odd source dimensions.
    let cases = [((32u32, 16u32), (9u32, 16u32)), ((16, 32), (32, 18)), ((32, 16), (11, 13))];
    for ((w, h), (sw, sh)) in cases {
        let srgb = picture(sw, sh);
        for grade in [false, true] {
            let what = format!("{sw}x{sh} on {w}x{h}, graded {grade}");
            let c = if grade { graded(media()) } else { media() };
            let seq = sequence(&c, w, h);
            let mut src = Source::new(srgb.clone(), sw, sh);
            let got = planes(&seq, w, h, &mut src).unwrap_or_else(|| panic!("{what}: the GPU path refused it"));
            assert_eq!(src.fetches.get(), 2, "{what}: fetched by the single-clip path, then by the stack");

            // Luma: the ordinary render's, bars and all.
            let want = cpu_luma(&seq, w, h, &mut Source::new(srgb.clone(), sw, sh));
            let d = worst(&got[..want.len()], &want);
            assert!(d <= 1, "{what}: luma differs from the render by {d} levels");
            // The bars are black and the picture is in the middle, not
            // stretched over the bars.
            let (row, mid) = ((h / 2) as usize * w as usize, (h / 2 * w + w / 2) as usize);
            let edge = if sw * h < w * sh { row } else { (w / 2) as usize };
            assert!(want[edge] <= 17 && got[edge] <= 17, "{what}: a bar where the frame has none ({})", got[edge]);
            assert!(got[mid] > 40, "{what}: the picture is missing from the middle");

            // All three planes, chroma too: the same picture fitted first
            // and exported at the sequence's size, through the single-clip
            // path, is the same frame.
            let mut fitted = Source::new(fitted_reference(&srgb, sw, sh, w, h), w, h);
            let reference = planes(&seq, w, h, &mut fitted).expect("the single-clip path takes a frame of the sequence's size");
            assert_eq!(fitted.fetches.get(), 1, "{what}: a frame of the sequence's size takes the single-clip path");
            let d = worst(&got, &reference);
            assert!(d <= 2, "{what}: the planes differ from the fitted picture's by {d} levels");
        }
    }
}

/// A scene-referred source of another size is still refused: the stack
/// can't place one yet (#1276 keeps that), so the frame takes the CPU
/// composite, said in the decline.
#[test]
fn a_scene_referred_source_of_another_size_is_still_refused() {
    struct Scene;
    impl FrameSource for Scene {
        fn frame(&mut self, _clip: &Clip, _secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
            let (w, h) = (9, 16);
            Some(SourceFrame {
                pixels: SourcePixels::SceneReferred {
                    codes: Arc::new(vec![klipp_color::hdr::pack_rgb10(512, 512, 512); (w * h) as usize]),
                    transfer: klipp_color::hdr::HdrTransfer::Pq,
                    source_to_working: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                },
                width: w,
                height: h,
                pts: None,
            })
        }
    }
    let c = media();
    let seq = sequence(&c, 32, 16);
    let mut r = renderer();
    assert!(r.render_yuv420_on_gpu(&seq, tc(0.5), 32, 16, &mut Scene, RenderOptions::default(), target()).is_none());
    if r.last_decline() != Some("the GPU pipeline was unavailable") {
        assert_eq!(r.last_decline(), Some("a scene-referred source of another size"));
    }
}
