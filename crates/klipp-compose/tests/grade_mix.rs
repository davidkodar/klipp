//! A grade correction's mix (M3 step 17a) reaches the picture: a
//! picture-truth clip's corrector mixed at a half draws between the clip
//! with it switched off and with it whole, on the CPU and on the GPU.

use std::sync::Arc;
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 64;
const H: u32 = 36;
fn tc(s: f64) -> RationalTime { RationalTime::from_secs_f64(s, 24) }

struct Chart;
impl FrameSource for Chart {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut pixels = Vec::new();
        for y in 0..h { for x in 0..w {
            pixels.extend_from_slice(&[x as f32 / w as f32, 0.2 + 0.5 * y as f32 / h as f32, 0.6, 1.0]);
        }}
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: None })
    }
}

/// A clip whose corrector takes the colour out, given `mix` (`None` for
/// none set), or switched off.
fn sequence(mix: Option<f32>, on: bool) -> Sequence {
    let mut c = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let mut g = klipp_nodes::NodeGraph::default();
    let corrector = g.ensure_corrector();
    for n in &mut g.nodes {
        if n.id == corrector {
            n.params.saturation = Some(0.0);
            n.enabled = on;
        }
    }
    c.set_grade(g);
    if mix.is_some() {
        let group = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).unwrap().id;
        c.apply_picture_ops(&[GraphOp::SetMix { path: NodePath::new(Root::Clip { clip: c.id.0 }, [group, corrector]), mix }]).unwrap();
        assert!(c.is_picture_truth());
    }
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, gpu: bool) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default()).0
}

#[test]
fn a_corrections_half_mix_draws_between_off_and_whole() {
    let gpu = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(gpu || std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required GPU unavailable");
    for use_gpu in [false, true].into_iter().filter(|v| !*v || gpu) {
        let off = render(&sequence(None, false), use_gpu);
        let whole = render(&sequence(None, true), use_gpu);
        let half = render(&sequence(Some(0.5), true), use_gpu);
        assert_eq!(render(&sequence(Some(1.0), true), use_gpu), whole, "gpu={use_gpu}: a whole mix is the correction");
        let mut between = 0;
        for ((o, w), h) in off.iter().zip(&whole).zip(&half) {
            let (lo, hi) = (o.min(*w), o.max(*w));
            assert!(*h >= lo - 1e-3 && *h <= hi + 1e-3, "gpu={use_gpu}: {h} outside {lo}..{hi}");
            if hi - lo > 0.02 && *h > lo + 0.005 && *h < hi - 0.005 {
                between += 1;
            }
        }
        assert!(between > (W * H) as usize, "gpu={use_gpu}: the half mix lands between, not on either end ({between})");
    }
}
