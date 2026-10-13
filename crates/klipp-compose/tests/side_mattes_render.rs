//! Each matte the Picture view puts beside a clip's chain (#1746) masks an
//! effect the same on the GPU and the CPU: a window, a rectangle, a
//! gradient, a roto shape, a luma key and a difference key, each feeding a
//! blur's mask, read by the renderer as a side matte. Each limits the
//! blur: the picture is neither the plain blur nor the clip unblurred,
//! and the GPU's is the CPU's within a level.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{DifferenceKeySettings, LumaKeySettings, Mask, MaskShape, MaskStack, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::ports::{input_key, output_key};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::NodeParams;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;

/// Stripes, so a blur shows, over a ramp, so a luma key splits it.
fn chart(w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let stripe = if (x / 4) % 2 == 0 { 1.0 } else { 0.15 };
            v.extend_from_slice(&[stripe * fx, (0.2 + 0.7 * fy) * stripe, stripe * (1.0 - fx) * 0.8, 1.0]);
        }
    }
    v
}

struct Chart;

impl FrameSource for Chart {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(chart(w, h))), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The chart as a clean plate on disk, with its middle third painted out:
/// the difference key selects that third.
fn plate() -> std::path::PathBuf {
    static FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let mut bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&chart(W, H));
        for y in 0..H {
            for x in W / 3..2 * W / 3 {
                let i = ((y * W + x) * 4) as usize;
                bytes[i..i + 3].copy_from_slice(&[128, 128, 128]);
            }
        }
        let path = std::env::temp_dir().join(format!("klipp-side-matte-plate-{}.png", std::process::id()));
        image::RgbaImage::from_raw(W, H, bytes).unwrap().save(&path).unwrap();
        path
    })
    .clone()
}

fn window(shape: MaskShape) -> NodeParams {
    NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, feather: 0.05, ..Mask::default() }] }), ..NodeParams::default() }
}

/// Each matte: its name, its grade kind as stored, and its settings.
fn mattes() -> Vec<(&'static str, &'static str, NodeParams)> {
    vec![
        ("window", "Mask", window(MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 })),
        ("rectangle", "Mask", window(MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.5, height: 0.4, rotation: 0.0 })),
        ("gradient", "Mask", window(MaskShape::LinearGradient { start_x: 0.2, start_y: 0.5, end_x: 0.8, end_y: 0.5 })),
        ("roto", "Mask", window(MaskShape::polygon(vec![(0.5, 0.2), (0.8, 0.5), (0.5, 0.8), (0.2, 0.5)]))),
        ("luma key", "LumaKey", NodeParams { luma_key: Some(LumaKeySettings { enabled: true, low: 0.5, high: 1.0, soft: 0.1, invert: false }), ..NodeParams::default() }),
        (
            "difference key",
            "DifferenceKey",
            NodeParams { difference_key: Some(DifferenceKeySettings { enabled: true, plate_path: Some(plate()), tolerance: 0.1, soft: 0.05, invert: false, ..DifferenceKeySettings::default() }), ..NodeParams::default() },
        ),
    ]
}

fn clip() -> Clip {
    let c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    TestClip::of(c).effect(E::GaussianBlur { radius: 4.0 }).build()
}

/// The clip with a matte of `kind` and `params` beside its chain, reading
/// its source, feeding its blur's mask: as the Picture view's Mattes menu
/// adds one.
fn masked(kind: &str, params: &NodeParams) -> Clip {
    let mut c = clip();
    let graph = c.picture_v2().unwrap().picture.graph.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let source = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
    let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": "klipp.legacy.node",
        "legacy": { "payload": { "kind": kind, "label": kind, "params": params } },
    }))
    .unwrap();
    let id = node.id;
    let ops = vec![
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: input_key(kind, 0) },
        GraphOp::Connect { from: at(id), from_port: output_key(kind, 0), to: at(blur), to_port: "mask".into() },
    ];
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{kind}: {e}"));
    c
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn render(gpu: bool, c: &Clip) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.render_inspected(&sequence(c.clone()), tc(1.0), W, H, &mut Chart, RenderOptions::default(), Inspect::default())
}

/// The worst difference in 8-bit sRGB levels, colour channels.
fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

#[test]
fn each_matte_masks_its_effect_the_same_on_the_gpu_and_the_cpu() {
    let (blurred, _) = render(false, &clip());
    let unblurred = {
        let c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        render(false, &c).0
    };
    for (name, kind, params) in mattes() {
        let c = masked(kind, &params);
        let (cpu, found) = render(false, &c);
        assert_eq!(found.stats.effects_mask_unavailable, 0, "{name}: the matte is given");
        assert!(found.declines.is_empty(), "{name}: {:?}", found.declines);
        assert!(worst_levels(&cpu, &blurred) > 2, "{name}: the matte limits the blur");
        assert!(worst_levels(&cpu, &unblurred) > 2, "{name}: the blur shows where the matte is");
        if !gpu() {
            eprintln!("UNVERIFIED: no GPU");
            continue;
        }
        let (on_gpu, found) = render(true, &c);
        assert_eq!(found.stats.effects_mask_unavailable, 0, "{name}: the GPU gives the matte");
        let worst = worst_levels(&on_gpu, &cpu);
        assert_eq!((found.stats.effects_followed_grade, found.stats.effects_on_cpu), (1, 0), "{name}: masked on the GPU, in the grade's plan");
        eprintln!("{name}: GPU against CPU: worst {worst} levels");
        assert!(worst <= 1, "{name}: the GPU draws {worst} levels from the CPU");
    }
}
