//! A difference key's clean plate picked from the clip's own frames
//! (#2758): the shot's frame at the picked source time, read once by the
//! media reader and handed to the run, keys the subject out of the frame
//! now as the same frame saved as a plate file does. On the GPU display
//! stack with nothing falling back, and the CPU route the same. A picked
//! time outside the media, or no time at all, is refused before anything
//! is decoded: the key has no plate, as with none chosen.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{DifferenceKeySettings, MediaId, RationalTime, WorkingColorspace};
use klipp_effects::{BlendMode, BuiltinEffect as E};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::ports::{input_key, output_key};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::side_picture::SideMissing;
use klipp_nodes::NodeParams;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const SHOT: &str = "5b2e8d41-7c9a-4f13-b6e0-2a4d8c1f9e37";
const BLUE: &str = "c4a7e2f9-1d3b-4e85-9f60-7b2c5a8d1e94";

/// Whether (x, y), fractions of the frame, is on the subject.
fn on_subject(x: f32, y: f32) -> bool {
    (0.3..0.6).contains(&x) && (0.3..0.7).contains(&y)
}

/// The shot at `secs`: a ramp, and from half a second on a red subject.
fn shot(secs: f64, w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            if secs >= 0.5 && on_subject(fx, fy) { [0.85, 0.04, 0.04, 1.0] } else { [0.15 + 0.3 * fx, 0.3, 0.2 + 0.3 * fy, 1.0] }
        })
        .collect()
}

/// The shot as the clip's media and as an item of the pool, which the
/// media reader reads (counted); a blue item for the new background.
#[derive(Default)]
struct Sources {
    shot_reads: usize,
}

impl FrameSource for Sources {
    fn frame(&mut self, _c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(shot(secs, w, h))), width: w, height: h, pts: None })
    }

    fn media_range(&mut self, _c: &Clip) -> Option<(f64, f64)> {
        Some((0.0, 10.0))
    }

    fn media_frame(&mut self, media: &str, secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        match media {
            SHOT => {
                self.shot_reads += 1;
                Ok(shot(secs, w, h))
            }
            BLUE => Ok([0.03, 0.08, 0.8, 1.0].repeat((w * h) as usize)),
            other => Err(SideMissing::File(other.into())),
        }
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The shot's clean frame (0 s) as a plate file.
fn plate_file() -> std::path::PathBuf {
    static FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let bytes = klipp_effects::linear_f32_rgba_to_srgb_u8(&shot(0.0, W, H));
        let path = std::env::temp_dir().join(format!("klipp-clean-frame-plate-{}.png", std::process::id()));
        image::RgbaImage::from_raw(W, H, bytes).unwrap().save(&path).unwrap();
        path
    })
    .clone()
}

fn key(plate_frame: Option<f64>, plate_path: Option<std::path::PathBuf>) -> DifferenceKeySettings {
    DifferenceKeySettings { enabled: true, plate_frame, plate_path, tolerance: 0.1, soft: 0.05, invert: true, ..DifferenceKeySettings::default() }
}

fn shot_clip() -> Clip {
    Clip { kind: ClipKind::Media { media_id: MediaId(SHOT.parse().unwrap()) }, ..Clip::gap(tc(0.0), tc(2.0)) }
}

/// The shot with the blue item merged over it where it matches the plate
/// `dk` keys by: the background replaced, the subject kept.
fn replaced(dk: DifferenceKeySettings) -> Clip {
    let mut c = shot_clip();
    let graph = c.picture_v2().unwrap().picture.graph.clone();
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    let source = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let display = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let effect = |e: &E| -> klipp_graph_format::Node {
        let payload = serde_json::to_value(e).unwrap();
        serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
            "legacy": { "payload": payload },
        }))
        .unwrap()
    };
    let merge = effect(&E::Merge { blend_mode: BlendMode::Normal, opacity: 1.0 });
    let blue = effect(&E::MediaSource { media: BLUE.into(), offset: 0.0 });
    let params = NodeParams { difference_key: Some(dk), ..NodeParams::default() };
    let keyer: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": "klipp.legacy.node",
        "legacy": { "payload": { "kind": "DifferenceKey", "label": "Clean plate", "params": params } },
    }))
    .unwrap();
    c.apply_picture_ops(&[
        GraphOp::AddNode { after: at(display), node: Box::new(merge.clone()) },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(blue.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(blue.id), to_port: "in".into() },
        GraphOp::Connect { from: at(blue.id), from_port: "out".into(), to: at(merge.id), to_port: "fg".into() },
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(keyer.clone()) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(keyer.id), to_port: input_key("DifferenceKey", 0) },
        GraphOp::Connect { from: at(keyer.id), from_port: output_key("DifferenceKey", 0), to: at(merge.id), to_port: "mask".into() },
    ])
    .unwrap_or_else(|e| panic!("{e}"));
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

/// The frame at 1 s on the GPU display stack, nothing fallen back.
fn stack(r: &mut Renderer, seq: &Sequence, src: &mut Sources) -> Option<Vec<u8>> {
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = r.render_display_u8_on_gpu(seq, tc(1.0), W, H, src, RenderOptions::default());
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", r.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", r.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "nothing falls back: {:?}", fell.events);
    Some(bytes)
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

fn on_cpu(seq: &Sequence, src: &mut Sources) -> Vec<u8> {
    let (linear, _) = renderer(false).render_inspected(seq, tc(1.0), W, H, src, RenderOptions::default(), Inspect::default());
    klipp_color::display::display_transform_ws_dithered_to(&linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn a_frame_of_the_clip_keys_as_the_same_frame_saved_as_a_plate() {
    let picked = sequence(replaced(key(Some(0.0), None)));
    let mut src = Sources::default();
    let mut gpu = renderer(true);
    let Some(shown) = stack(&mut gpu, &picked, &mut src) else { return };
    let Some(by_file) = stack(&mut renderer(true), &sequence(replaced(key(None, Some(plate_file())))), &mut Sources::default()) else { return };
    let Some(alone) = stack(&mut renderer(true), &sequence(shot_clip()), &mut Sources::default()) else { return };
    assert!(worst(&shown, &by_file) <= 1, "the frame keys as the plate file of it: {} levels apart", worst(&shown, &by_file));

    // The background replaced, the subject kept.
    let (mut subject, mut background) = (0u8, u8::MAX);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
            let near = [(2.0, 0.0), (-2.0, 0.0), (0.0, 2.0), (0.0, -2.0)].iter().any(|(dx, dy)| on_subject(fx + dx / W as f32, fy + dy / H as f32) != on_subject(fx, fy));
            if near {
                continue;
            }
            let i = ((y * W + x) * 4) as usize;
            if on_subject(fx, fy) {
                subject = subject.max((0..3).map(|c| shown[i + c].abs_diff(alone[i + c])).max().unwrap());
            } else {
                background = background.min(shown[i + 2]);
            }
        }
    }
    assert!(subject <= 1, "the subject as the clip shows it: {subject} levels off");
    assert!(background >= 200, "the new background behind it: its least blue is {background}");

    // Read once, then kept: two more frames read no more.
    for _ in 0..2 {
        stack(&mut gpu, &picked, &mut src);
    }
    assert_eq!(src.shot_reads, 1, "the clean frame is read once");

    // The CPU route keys it the same.
    assert!(worst(&on_cpu(&picked, &mut Sources::default()), &shown) <= 3, "the CPU route: {} levels off", worst(&on_cpu(&picked, &mut Sources::default()), &shown));
}

#[test]
fn a_time_outside_the_media_is_refused_before_anything_is_read() {
    let mut none = Sources::default();
    let unchosen = on_cpu(&sequence(replaced(key(None, None))), &mut none);
    for secs in [12.0, -1.0, f64::NAN] {
        let mut src = Sources::default();
        let got = on_cpu(&sequence(replaced(key(Some(secs), None))), &mut src);
        assert_eq!(src.shot_reads, 0, "{secs}: nothing decoded");
        assert!(worst(&got, &unchosen) <= 1, "{secs}: no plate, as with none chosen: {} levels apart", worst(&got, &unchosen));
    }
    assert!(klipp_compose::resolve::clean_frame_refused(12.0, Some((0.0, 10.0))).is_some_and(|why| why.contains("outside the media")));
    assert!(klipp_compose::resolve::clean_frame_refused(f64::NAN, Some((0.0, 10.0))).is_some_and(|why| why.contains("isn't a time")));
    assert_eq!(klipp_compose::resolve::clean_frame_refused(9.5, Some((0.0, 10.0))), None);
}
