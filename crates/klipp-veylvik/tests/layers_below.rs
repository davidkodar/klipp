//! A composition that reads the layers below its clip is given them, the
//! tracks under it composited, in Klipp's renders on the GPU stack and on
//! the CPU's alike; with nothing below, a clear picture.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};

const W: u32 = 64;
const H: u32 = 36;

const TOP: MediaId = MediaId(uuid::Uuid::from_u128(7));
const UNDER: MediaId = MediaId(uuid::Uuid::from_u128(8));

/// Each media a flat colour of its own.
struct Flat;
impl FrameSource for Flat {
    fn frame(&mut self, c: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let px = if c.media_id() == Some(TOP) { [0.6, 0.1, 0.1, 1.0] } else { [0.1, 0.5, 0.2, 1.0] };
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new((0..w * h).flat_map(|_| px).collect())), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
}

#[test]
fn a_composition_reading_the_layers_below_is_given_them_on_the_gpu_stack_and_the_cpus() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    // The composition lays what is below its clip over the clip's own
    // footage: given it, the track under shows; not given it, the clip.
    let mut c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: 24, rate: 1 }, 24).unwrap();
    {
        use klipp_veylvik::veylvik_document::{nodes, Node, Operation};
        let below = Composition::from_host_footage(klipp_veylvik::BELOW_KEY, "Layers below", None, [W, H], Time { ticks: 24, rate: 1 }, 24).unwrap();
        let below = below.nodes[&below.output].clone();
        let merge = Node::new("Below over the clip", Operation::Merge(nodes::Merge {}), &[("background", c.output), ("foreground", below.id)]);
        c.output = merge.id;
        c.nodes.insert(below.id, below);
        c.nodes.insert(merge.id, merge);
        c.validate().unwrap();
    }
    let bytes: Arc<[u8]> = serde_json::to_vec(&c).unwrap().into();
    assert!(klipp_veylvik::bytes_read_below(&bytes));
    klipp_veylvik::set_project_compositions([(id, bytes)].into());
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut top = Clip::media(TOP, at(0.0), at(1.0), at(0.0), at(1.0));
    top.composition = Some(id);
    let under = Clip::media(UNDER, at(0.0), at(1.0), at(0.0), at(1.0));
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let (mut v2, mut v1) = (Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video));
    v2.clips = vec![top];
    v1.clips = vec![under];
    // The first track is the top one.
    seq.tracks = vec![v2, v1];
    let middle = ((H / 2 * W + W / 2) * 4) as usize;
    // On the GPU stack, as the viewer and the export draw a frame where it
    // takes it (and it must here), or on the CPU's.
    let draw = |seq: &Sequence, gpu: bool| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        if gpu {
            r.render_linear_on_gpu(seq, at(0.5), W, H, &mut Flat, RenderOptions::default()).expect("the GPU stack takes the frame")
        } else {
            r.render(seq, at(0.5), W, H, &mut Flat, RenderOptions::default())
        }
    };
    for gpu in [true, false] {
        let frame = draw(&seq, gpu);
        let px = &frame[middle..middle + 4];
        assert!((px[0] - 0.1).abs() < 2e-3 && (px[1] - 0.5).abs() < 2e-3 && (px[2] - 0.2).abs() < 2e-3, "the track under it, {}: {px:?}", if gpu { "GPU" } else { "CPU" });
    }
    // Alone, nothing is below it: a clear picture over the clip's own.
    seq.tracks.pop();
    for gpu in [true, false] {
        let frame = draw(&seq, gpu);
        let px = &frame[middle..middle + 4];
        assert!((px[0] - 0.6).abs() < 2e-3 && (px[1] - 0.1).abs() < 2e-3, "its own footage under a clear below, {}: {px:?}", if gpu { "GPU" } else { "CPU" });
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
}
