//! A Media Source beside a picture's chain draws another media item's
//! frame (#1746), at the clip's time plus its offset, read from the frame
//! source: wired into a Merge's `fg`, it is laid over the clip's picture,
//! the same on the GPU (the viewer's route) and the CPU. An item the
//! frame source can't find is a clear picture, declined as a missing file
//! that names it.

use std::sync::Arc;

use klipp_color::hdr::{HdrTransfer, SceneToDisplay};
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const ITEM: &str = "6f1d1c34-5b0e-4c55-9a39-0d3b3c7b2a10";

/// The clip's chart, and the item's picture: stripes whose blue is the
/// time they were read at, so the offset shows.
struct Sources;

fn item_frame(secs: f64, w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..h {
        for x in 0..w {
            let on = (x / 8) % 2 == 0;
            v.extend_from_slice(&[if on { 0.8 } else { 0.05 }, 0.3, secs as f32 / 4.0, 1.0]);
        }
    }
    v
}

impl FrameSource for Sources {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                v.extend_from_slice(&[0.1 + 0.8 * fx, 0.1 + 0.6 * fy, 0.6 * (1.0 - fx), 1.0]);
            }
        }
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        match media {
            ITEM | HDR_ITEM | LOG_ITEM => Ok(item_frame(secs, w, h)),
            other => Err(SideMissing::File(format!("{other}.mov"))),
        }
    }

    fn media_transfer(&mut self, media: &str) -> Option<HdrTransfer> {
        match media {
            HDR_ITEM => Some(HdrTransfer::Hlg),
            LOG_ITEM => Some(HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)),
            _ => None,
        }
    }
}

/// An item whose frames are HLG, which the source reads as `ITEM`'s.
const HDR_ITEM: &str = "9d2e4b71-0c3a-4f58-b6e1-27a4c8d5f390";
/// An item whose frames are S-Log3, read the same.
const LOG_ITEM: &str = "3c8a1f20-7d4b-4e19-a5c6-81b0e2f4d937";

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// An effect node for `e`, as the Picture view makes one.
fn effect_node(e: &E) -> klipp_graph_format::Node {
    let payload = serde_json::to_value(e).unwrap();
    serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap()
}

/// The chart clip with `merge` after its display step and, when `fg`, a
/// node beside the chain reading its source, wired into the merge's `fg`:
/// as the Picture view's Sources menu makes it.
fn clip(merge: Option<E>, fg: Option<E>) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let Some(merge) = merge else { return c };
    let picture = c.picture_v2().unwrap().picture.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let source = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let m = effect_node(&merge);
    let mut ops = vec![GraphOp::AddNode { after: at(display), node: Box::new(m.clone()) }];
    if let Some(fg) = fg {
        let g = effect_node(&fg);
        ops.extend([
            GraphOp::AddNodeBeside { at: at(source), node: Box::new(g.clone()) },
            GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(g.id), to_port: "in".into() },
            GraphOp::Connect { from: at(g.id), from_port: "out".into(), to: at(m.id), to_port: "fg".into() },
        ]);
    }
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
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
    render_for(gpu, c, None)
}

/// [`render`] for a delivery of `peak` nits (`None`: SDR).
fn render_for(gpu: bool, c: &Clip, peak: Option<f32>) -> (Vec<f32>, klipp_compose::Inspection) {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    let opts = RenderOptions { hdr_peak_nits: peak, ..RenderOptions::default() };
    r.render_inspected(&sequence(c.clone()), tc(1.0), W, H, &mut Sources, opts, Inspect::default())
}

fn worst_levels(a: &[f32], b: &[f32]) -> u8 {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(a), klipp_effects::linear_f32_rgba_to_srgb_u8(b));
    a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

fn gpu() -> bool {
    let ok = klipp_nodes::GraphExecutor::new().gpu_pipeline_or_init().is_some();
    assert!(ok || !required(), "required, but there is no GPU");
    ok
}

fn media(item: &str, offset: f32) -> E {
    E::MediaSource { media: item.into(), offset }
}

/// The item's frame at the clip's time (1 s) plus `offset`, laid over the
/// plain picture at `opacity`, as the merge's formula gives.
fn oracle(offset: f32, opacity: f32) -> Vec<f32> {
    let (plain, _) = render(false, &clip(None, None));
    let fg = item_frame(1.0 + f64::from(offset), W, H);
    plain.chunks_exact(4).zip(fg.chunks_exact(4)).flat_map(|(b, t)| [0, 1, 2].map(|i| b[i] + (t[i] - b[i]) * opacity).into_iter().chain([b[3]])).collect()
}

#[test]
fn a_media_source_lays_its_item_at_the_offset_time_the_same_on_the_gpu_and_the_cpu() {
    let (plain, _) = render(false, &clip(None, None));
    for (offset, opacity) in [(0.5, 1.0), (-0.75, 0.6)] {
        let c = clip(Some(E::Merge { blend_mode: BlendMode::Normal, opacity }), Some(media(ITEM, offset)));
        assert!(c.is_picture_truth(), "a picture wired from beside the chain is the picture's");
        let want = oracle(offset, opacity);
        assert!(worst_levels(&want, &plain) > 10, "the item changes the picture");
        // Another offset's frame differs: the time is read.
        assert!(worst_levels(&want, &oracle(offset + 1.0, opacity)) > 10);
        let (cpu, found) = render(false, &c);
        assert!(found.declines.is_empty(), "{offset}: {:?}", found.declines);
        let worst = worst_levels(&cpu, &want);
        eprintln!("offset {offset}: CPU against the formula: worst {worst} levels");
        assert!(worst <= 1, "offset {offset}: the CPU draws {worst} levels from the formula");
        if !gpu() {
            eprintln!("UNVERIFIED: no GPU");
            continue;
        }
        let (on_gpu, found) = render(true, &c);
        assert!(found.declines.is_empty(), "{offset}: {:?}", found.declines);
        let worst = worst_levels(&on_gpu, &cpu);
        eprintln!("offset {offset}: GPU against CPU: worst {worst} levels");
        assert!(worst <= 1, "offset {offset}: the GPU draws {worst} levels from the CPU");
    }
}

#[test]
fn an_item_the_source_cant_find_is_clear_and_named_as_missing() {
    let (plain, _) = render(false, &clip(None, None));
    let gone = "0b5c2f7e-1111-4e2a-8c1d-3f0a9e4b7c55";
    let c = clip(Some(E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 }), Some(media(gone, 0.0)));
    for gpu_on in [false, true] {
        if gpu_on && !gpu() {
            continue;
        }
        let (out, found) = render(gpu_on, &c);
        assert_eq!(worst_levels(&out, &plain), 0, "gpu {gpu_on}: a clear picture over the clip's");
        let d = found.declines.iter().find(|d| d.outcome == klipp_nodes::DeclineOutcome::FileMissing).unwrap_or_else(|| panic!("gpu {gpu_on}: {:?}", found.declines));
        assert!(d.why.contains(gone), "gpu {gpu_on}: names the item: {d:?}");
    }
}

#[test]
fn a_media_source_with_no_item_passes_the_picture() {
    let (plain, _) = render(false, &clip(None, None));
    let c = clip(Some(E::Merge { blend_mode: BlendMode::Screen, opacity: 1.0 }), Some(media("", 0.0)));
    let (out, _) = render(false, &c);
    assert_eq!(worst_levels(&out, &plain), 0);
}

/// An HDR or log item merged after the display step is given its own
/// display step as it is read (#1857): its transfer, the delivery's peak,
/// as a clip's source is. Drawn so on the CPU and the GPU alike, the
/// viewer's route and the export's, and declined nowhere. Controls: the
/// item's frame without the step is far from what is drawn, and an SDR
/// item is drawn as it is read.
#[test]
fn an_hdr_or_log_item_merged_after_the_display_step_is_drawn_display_mapped() {
    let normal = || E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 };
    let (sdr, found) = render(false, &clip(Some(normal()), Some(media(ITEM, 0.0))));
    assert!(found.declines.is_empty(), "{:?}", found.declines);
    assert!(worst_levels(&sdr, &item_frame(1.0, W, H)) <= 1, "an SDR item as it is read");
    for (item, transfer) in [(HDR_ITEM, HdrTransfer::Hlg), (LOG_ITEM, HdrTransfer::Log(klipp_core::ColorTransfer::SLog3))] {
        for peak in [None, Some(1000.0)] {
            let c = clip(Some(normal()), Some(media(item, 0.0)));
            let raw = item_frame(1.0, W, H);
            let mut want = raw.clone();
            SceneToDisplay { transfer, peak_nits: peak, luma: klipp_color::primaries::luminance_weights(WorkingColorspace::Rec709Linear) }.apply(&mut want);
            assert!(worst_levels(&want, &raw) > 10, "{transfer:?} {peak:?}: the step changes the frame");
            let (cpu, found) = render_for(false, &c, peak);
            assert!(found.declines.is_empty(), "{transfer:?} {peak:?}: {:?}", found.declines);
            let worst = worst_levels(&cpu, &want);
            eprintln!("{transfer:?} {peak:?}: CPU against the display-mapped item: worst {worst} levels");
            assert!(worst <= 1, "{transfer:?} {peak:?}: the CPU draws {worst} levels from the display-mapped item");
            assert!(worst_levels(&cpu, &raw) > 10, "{transfer:?} {peak:?}: not the item without its step");
            if !gpu() {
                eprintln!("UNVERIFIED: no GPU");
                continue;
            }
            let (on_gpu, found) = render_for(true, &c, peak);
            assert!(found.declines.is_empty(), "{transfer:?} {peak:?}: {:?}", found.declines);
            let worst = worst_levels(&on_gpu, &cpu);
            eprintln!("{transfer:?} {peak:?}: GPU against CPU: worst {worst} levels");
            assert!(worst <= 1, "{transfer:?} {peak:?}: the GPU draws {worst} levels from the CPU");
        }
    }
}
