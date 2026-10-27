//! A picture pinned onto a surface tracked in the clip, inside the clip's
//! graph: a Surface Pin beside the chain, merged over the clip, is the
//! corner pin of the surface's corners as the frame shows them, fitted
//! as the clip's picture is fitted, inset as asked. On the GPU display
//! stack with nothing falling back, it is the frame a corner pin set by
//! hand to those corners makes; the CPU route draws the same. A surface
//! the clip lacks draws nothing: the clip alone.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::surface::{pin_target, source_quad_in_frame, Quad};
use klipp_timeline::{Clip, ClipKind, PinFit, Sequence, Surface, SurfaceKey, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const ITEM: &str = "7c1e4a90-3b2d-4f6e-9a8c-5d0b1e2f3a47";

/// A screen leaning in the shot.
const SCREEN: Quad = [[0.3, 0.25], [0.7, 0.3], [0.68, 0.75], [0.32, 0.7]];

/// The clip's chart, and the item: red all over.
struct Sources;

impl FrameSource for Sources {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.3 * fx, 0.2 + 0.5 * fy, 0.6 * (1.0 - fx), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, _secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        if media != ITEM {
            return Err(SideMissing::File(format!("{media}.mov")));
        }
        Ok([0.85, 0.05, 0.05, 1.0].repeat((w * h) as usize))
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
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

/// The clip, carrying the screen tracked still over its file seconds 0
/// to 10, on a picture `source` in size.
fn clip(source: [u32; 2]) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    c.surfaces.push(Surface {
        id: klipp_core::ClipId::new().0,
        name: "Screen".into(),
        corners: SCREEN,
        reference_secs: 0.0,
        keys: vec![SurfaceKey::at_corners(0.0, &SCREEN, &SCREEN).unwrap(), SurfaceKey::at_corners(10.0, &SCREEN, &SCREEN).unwrap()],
        source_size: source,
        media_fps: 0.0,
        track_area: None,
        exclude: Vec::new(),
    });
    c
}

/// `c` with the item laid over it through `pin` beside its chain.
fn pinned(mut c: Clip, pin: E) -> Clip {
    let picture = c.picture_v2().unwrap().picture.clone();
    let id = c.id.0;
    let at = |n| NodePath::new(Root::Clip { clip: id }, [n]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let merge = effect_node(&E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 });
    let item = effect_node(&E::MediaSource { media: ITEM.into(), offset: 0.0 });
    let pin = effect_node(&pin);
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(item.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(item.id), to_port: "in".into() },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(pin.clone()) },
        GraphOp::Connect { from: at(item.id), from_port: "out".into(), to: at(pin.id), to_port: "in".into() },
        GraphOp::Connect { from: at(pin.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
    c
}

fn surface_pin(surface: &str, inset: f32) -> E {
    E::SurfacePin { surface: surface.into(), inset, on_source: [[0.0; 2]; 4], source: [0; 2] }
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn stack(seq: &Sequence) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Sources, RenderOptions::default());
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "nothing falls back: {:?}", fell.events);
    Some(bytes)
}

fn on_cpu(seq: &Sequence) -> Vec<u8> {
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let (linear, _) = cpu.render_inspected(seq, tc(1.0), W, H, &mut Sources, RenderOptions::default(), Default::default());
    klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn by_hand(q: Quad) -> E {
    klipp_effects::corner_pin::with_corners(q)
}

#[test]
fn a_surface_pin_is_the_corner_pin_on_the_surface() {
    let got = sequence(pinned(clip([W, H]), surface_pin("Screen", 0.0)));
    let Some(shown) = stack(&got) else { return };
    let Some(want) = stack(&sequence(pinned(clip([W, H]), by_hand(SCREEN)))) else { return };
    let Some(alone) = stack(&sequence(clip([W, H]))) else { return };
    assert!(worst(&shown, &alone) > 100, "the item lies on the screen");
    assert!(worst(&shown, &want) <= 1, "the corner pin on the screen's corners: {} levels off", worst(&shown, &want));
    // By its id as by its name.
    let c = clip([W, H]);
    let id = c.surfaces[0].id.to_string();
    let Some(by_id) = stack(&sequence(pinned(c, surface_pin(&id, 0.0)))) else { return };
    assert!(worst(&by_id, &want) <= 1);
    // The CPU route pins it the same.
    assert!(worst(&on_cpu(&got), &shown) <= 3, "the CPU route: {} levels off", worst(&on_cpu(&got), &shown));
}

#[test]
fn a_picture_of_another_shape_is_fitted_and_inset() {
    // Twice the frame's width: fitted, it is a band across the middle.
    let source = [W * 2, H];
    let inset = 0.1;
    let on_frame = source_quad_in_frame(&pin_target(&SCREEN, None, None, PinFit::Stretch, inset).unwrap(), source, [W, H], None);
    let Some(shown) = stack(&sequence(pinned(clip(source), surface_pin("Screen", inset)))) else { return };
    let Some(want) = stack(&sequence(pinned(clip(source), by_hand(on_frame)))) else { return };
    let Some(unfitted) = stack(&sequence(pinned(clip(source), by_hand(SCREEN)))) else { return };
    assert!(worst(&shown, &unfitted) > 100, "not the corners as they are on the picture");
    assert!(worst(&shown, &want) <= 1, "fitted as the clip's picture is, inset: {} levels off", worst(&shown, &want));
}

#[test]
fn a_surface_the_clip_lacks_draws_nothing() {
    let Some(shown) = stack(&sequence(pinned(clip([W, H]), surface_pin("Nowhere", 0.0)))) else { return };
    let Some(alone) = stack(&sequence(clip([W, H]))) else { return };
    assert!(worst(&shown, &alone) <= 1, "the clip alone: {} levels off", worst(&shown, &alone));
}
