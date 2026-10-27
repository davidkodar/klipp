//! A title drawn as a picture inside a clip's graph (#2758): a Title
//! Source beside the chain, merged over the clip, is the frame the same
//! title makes as a title clip on the track above. On the GPU stack it is
//! drawn by the title route into a layer of its own and handed to the
//! clip's run, with nothing falling back; the CPU route rasters it, the
//! same picture within the title's CPU/GPU bound.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::*;

const W: u32 = 320;
const H: u32 = 180;

/// A chart for the clip; no other media.
struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.5 * fx, 0.15 + 0.4 * fy, 0.5 * (1.0 - fx), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn title() -> TitleData {
    TitleData::single_text(TextBox { text: "Behind".into(), font_size: 140.0, color: [235, 210, 120, 255], ..Default::default() })
}

fn effect_node(e: &E) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

fn chart_clip() -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) }
}

/// The chart clip with the title merged over it from a Title Source
/// beside its chain.
fn with_title_source() -> Clip {
    let mut c = chart_clip();
    let clip = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip }, [id]);
    let picture = c.picture_v2().unwrap().picture.clone();
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let merge = effect_node(&E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 });
    let text = effect_node(&E::TitleSource { title: serde_json::to_string(&title()).unwrap() });
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(text.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(text.id), to_port: "in".into() },
        GraphOp::Connect { from: at(text.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    c
}

fn sequence(tracks: Vec<(TrackKind, Clip)>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    // Tracks run top first.
    seq.tracks = tracks.into_iter().map(|(kind, c)| {
        let mut t = Track::new("t", kind);
        t.clips = vec![c];
        t
    }).collect();
    seq
}

fn stack(seq: &Sequence) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Chart, RenderOptions::default());
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let events = klipp_effects::residency::last_frame().events;
    assert!(!events.iter().any(|e| e.work == klipp_effects::residency::Work::Fallback), "nothing falls back: {events:?}");
    Some(bytes)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn a_title_source_merged_is_the_title_clip_above() {
    let inside = sequence(vec![(TrackKind::Video, with_title_source())]);
    let title_clip = Clip { kind: ClipKind::Title(title()), ..Clip::gap(tc(0.0), tc(2.0)) };
    let above = sequence(vec![(TrackKind::Video, title_clip), (TrackKind::Video, chart_clip())]);
    let plain = sequence(vec![(TrackKind::Video, chart_clip())]);
    let Some(got) = stack(&inside) else { return };
    let Some(want) = stack(&above) else { return };
    let Some(bare) = stack(&plain) else { return };
    assert!(worst(&got, &bare) > 40, "the title shows");
    assert!(worst(&got, &want) <= 3, "the title inside the clip is the title clip above: {} levels apart", worst(&got, &want));

    // The CPU route rasters the same title.
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let (linear, _) = cpu.render_inspected(&inside, tc(1.0), W, H, &mut Chart, RenderOptions::default(), Default::default());
    let on_cpu = klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
    assert!(worst(&got, &on_cpu) <= 3, "the CPU route draws it within the title's bound: {} levels apart", worst(&got, &on_cpu));
}
