//! A wide-gamut scene-referred clip exported to SDR on the GPU is the
//! renderer's picture (#1280).
//!
//! A source in wider primaries than the project's (BT.2020 in a Rec.709
//! project) has colours outside the working space, whose converted
//! components are negative. The GPU export's tone map for a scene-referred
//! clip took them through the filmic curve unclamped, which maps a small
//! negative to a positive: a colour cast the renderer, which clamps at 0
//! first, never shows. Tests with matching primaries couldn't see it.
//!
//! A missing GPU leaves it unverified.

use std::sync::Arc;

use klipp_color::hdr::HdrTransfer;
use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 32;
const H: u32 = 16;
const LUMA_709: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// BT.2020 primaries into BT.709.
const BT2020_TO_709: [[f32; 3]; 3] = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// A clip with a half-stop grade, so the graded path (the one the export
/// has always taken for a scene-referred clip) runs.
fn graded() -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(1.0)) };
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

/// Saturated codes across the frame: strong reds, greens and blues, so
/// many fall outside BT.709 once converted from BT.2020.
fn codes() -> Vec<u32> {
    (0..H)
        .flat_map(|y| {
            (0..W).map(move |x| {
                let a = x * 1023 / W;
                let b = y * 1023 / H;
                match (x + y) % 3 {
                    0 => klipp_color::hdr::pack_rgb10(a.max(400), b / 6, 40),
                    1 => klipp_color::hdr::pack_rgb10(40, a.max(400), b / 6),
                    _ => klipp_color::hdr::pack_rgb10(b / 6, 40, a.max(400)),
                }
            })
        })
        .collect()
}

struct Scene {
    codes: Arc<klipp_color::hdr::SceneCodes>,
    transfer: HdrTransfer,
}

impl FrameSource for Scene {
    fn frame(&mut self, _clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame {
            pixels: SourcePixels::SceneReferred { codes: Arc::clone(&self.codes), transfer: self.transfer, source_to_working: BT2020_TO_709 },
            width,
            height,
            pts: None,
        })
    }
}

fn renderer() -> Renderer {
    Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new())
}

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

/// A linear frame encoded as the CPU export encodes it for this target:
/// the display transform, then BT.709 limited-range Y'CbCr, planar 4:2:0
/// with each chroma sample the mean of its 2x2 block.
fn yuv420(px: &[f32]) -> Vec<u8> {
    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::OdtChoice::MatchViewer);
    let [kr, kg, kb] = LUMA_709;
    let ycc: Vec<[f32; 3]> = px
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = kr * e[0] + kg * e[1] + kb * e[2];
            [y, (e[2] - y) / (2.0 * (1.0 - kb)), (e[0] - y) / (2.0 * (1.0 - kr))]
        })
        .collect();
    let q = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
    let mut out: Vec<u8> = ycc.iter().map(|c| q(16.0 + 219.0 * c[0])).collect();
    for k in [1, 2] {
        for j in 0..(H / 2) as usize {
            for i in 0..(W / 2) as usize {
                let at = |dx: usize, dy: usize| ycc[(2 * j + dy) * W as usize + 2 * i + dx][k];
                out.push(q(128.0 + 224.0 * (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4.0));
            }
        }
    }
    out
}

#[test]
fn a_wide_gamut_scene_referred_clip_exports_to_sdr_as_the_renderer_draws_it() {
    if klipp_effects::gpu::GpuEffectsPipeline::new().is_none() {
        eprintln!("UNVERIFIED: no usable GPU");
        return;
    }
    let codes = Arc::new(klipp_color::hdr::SceneCodes::Packed10(codes()));
    // The fixture reaches outside BT.709, or this would pass unclamped.
    let linear = codes.to_scene_linear_rgba(HdrTransfer::Pq, BT2020_TO_709);
    assert!(linear.chunks_exact(4).any(|p| p[..3].iter().any(|c| *c < -0.01)), "the fixture has no colour outside BT.709");

    let c = graded();
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![c];
    seq.tracks = vec![t];
    for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg, HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)] {
        let mut src = Scene { codes: Arc::clone(&codes), transfer };
        let mut r = renderer();
        let (planes, _) = r
            .render_yuv420_on_gpu(&seq, tc(0.5), W, H, &mut src, RenderOptions::default(), target())
            .unwrap_or_else(|| panic!("{transfer:?}: the GPU export refused: {:?}", r.last_decline()));
        // The reference is the CPU's own render, with the GPU off.
        let mut cpu = renderer();
        cpu.set_gpu_enabled(false);
        let px = cpu.render(&seq, tc(0.5), W, H, &mut src, RenderOptions::default());
        let want = yuv420(&px);
        let (y_len, c_len) = ((W * H) as usize, (W * H / 4) as usize);
        for (name, at, len, bar) in [("Y", 0, y_len, 1), ("Cb", y_len, c_len, 2), ("Cr", y_len + c_len, c_len, 2)] {
            let (got, want) = (&planes[at..at + len], &want[at..at + len]);
            let worst = got.iter().zip(want).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap_or(0);
            eprintln!("{transfer:?} in BT.2020 -> SDR: {name}, GPU export vs the CPU render, worst {worst} levels");
            assert!(worst <= bar, "{transfer:?}: {name} differs from the CPU render by {worst} levels");
        }
    }
}
