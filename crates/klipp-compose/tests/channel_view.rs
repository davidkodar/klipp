//! The viewer's channel view (#2758): a clip keyed over a known green
//! patch, shown as its alpha, reads black on the patch and white round
//! it, on the CPU and on the GPU display stack alike, with nothing
//! falling back; red, green, blue and luma show the picture's own
//! channel in grey.

use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, ViewChannel, WorkingColorspace};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 96;
const H: u32 = 54;
const GREY: f32 = 0.3;
const GREEN: [f32; 3] = [0.02, 0.75, 0.02];

/// Whether (x, y) is on the green patch.
fn on_patch(x: u32, y: u32) -> bool {
    (24..72).contains(&x) && (12..42).contains(&y)
}

/// A grey frame with a green patch in the middle.
struct Patch;

impl FrameSource for Patch {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if on_patch(x, y) { [GREEN[0], GREEN[1], GREEN[2], 1.0] } else { [GREY, GREY, GREY, 1.0] }
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The clip with a chroma key on the patch's green after its display
/// step.
fn keyed() -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let picture = c.picture_v2().unwrap().picture.clone();
    let clip = c.id.0;
    let at = |id| NodePath::new(Root::Clip { clip }, [id]);
    let display = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).unwrap().id;
    let key = E::ChromaKey { key_color: [5, 190, 5], tolerance: 0.25, soft_edge: 0.05, spill_method: Default::default(), spill_strength: 0.0 };
    let payload = serde_json::to_value(&key).unwrap();
    let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
        "id": klipp_graph_format::NodeId::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
        "legacy": { "payload": payload },
    }))
    .unwrap();
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(display), node: Box::new(node.clone()) }]).unwrap_or_else(|e| panic!("{e}"));
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

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn view(channel: ViewChannel) -> RenderOptions {
    RenderOptions { view_channel: channel, ..Default::default() }
}

fn cpu(seq: &Sequence, opts: RenderOptions, inspect: Inspect) -> Vec<u8> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (pixels, found) = r.render_inspected(seq, tc(1.0), W, H, &mut Patch, opts, inspect);
    assert!(found.unavailable.is_empty(), "{:?}", found.unavailable);
    display(&pixels)
}

fn rgb(frame: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [frame[i], frame[i + 1], frame[i + 2]]
}

/// Every pixel grey, black on the patch (well inside it) and white well
/// outside it; the key's soft edge is left out.
fn reads_the_key(frame: &[u8], what: &str) {
    for y in 0..H {
        for x in 0..W {
            let p = rgb(frame, x, y);
            assert!(p[0] == p[1] && p[1] == p[2], "{what}: grey at ({x}, {y}): {p:?}");
        }
    }
    let inside = rgb(frame, 48, 27);
    let outside = [rgb(frame, 4, 4), rgb(frame, 90, 50), rgb(frame, 48, 4)];
    assert_eq!(inside, [0; 3], "{what}: the keyed patch is clear");
    for o in outside {
        assert_eq!(o, [255; 3], "{what}: round the patch is opaque");
    }
}

#[test]
fn the_alpha_view_of_a_key_reads_clear_on_the_patch_and_opaque_round_it() {
    let clip = keyed();
    let seq = sequence(clip);
    let want = cpu(&seq, view(ViewChannel::Alpha), Inspect::default());
    reads_the_key(&want, "the CPU");
    // The picture itself is not grey: the view is what made it so.
    let picture = cpu(&seq, view(ViewChannel::Picture), Inspect::default());
    assert_ne!(rgb(&picture, 4, 4), [255; 3]);

    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Patch, view(ViewChannel::Alpha));
    drop(frame);
    let Some((stack, _)) = drawn else {
        assert!(!required(), "the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: the stack declined ({:?})", gpu.last_decline());
        return;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "nothing falls back: {:?}", fell.events);
    reads_the_key(&stack, "the GPU stack");
    let off = want.iter().zip(&stack).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(off <= 1, "the stack draws {off} levels from the CPU");
}

/// Red, green, blue and luma are the picture's own, in grey: outside the
/// patch the grey source shows the same in all four; where a channel is
/// zero on the keyed patch, black.
#[test]
fn each_colour_channel_shows_the_picture_in_grey() {
    let clip = keyed();
    let seq = sequence(clip);
    let picture = cpu(&seq, view(ViewChannel::Picture), Inspect::default());
    let grey = rgb(&picture, 4, 4);
    for channel in [ViewChannel::Red, ViewChannel::Green, ViewChannel::Blue, ViewChannel::Luma] {
        let f = cpu(&seq, view(channel), Inspect::default());
        let o = rgb(&f, 4, 4);
        assert!(o[0] == o[1] && o[1] == o[2] && o[0].abs_diff(grey[0]) <= 1, "{channel:?} round the patch: {o:?}, the picture {grey:?}");
        assert_eq!(rgb(&f, 48, 27), [0; 3], "{channel:?} on the keyed patch");
    }
}
