//! The recipe harness (#2758): every built-in recipe placed on a test
//! shot through the route the recipe op takes (`recipes::on_clip`, the
//! graph batch one undo step applies), then drawn on the GPU display
//! stack as the viewer draws it, with nothing falling back, within a
//! level of the CPU, and changed by the recipe. Each recipe's own checks
//! are data in its file (`graph.checks`), read here through the
//! viewer's channel view: a new recipe is a new file, never new code.

use std::collections::BTreeMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, TemplateValue, ViewChannel, WorkingColorspace};
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::composition_template::{builtin_recipes, SlotKind};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 160;
const H: u32 = 90;
const PICTURE: &str = "2f6a9c13-5e8b-4d07-a1c4-8b3e6f9d2a50";
const GREY: f32 = 0.3;
const GREEN: [f32; 3] = [0.02, 0.75, 0.02];

/// The test shot: grey, a green patch over its middle; a media slot's
/// picture is red.
struct Shot;

impl FrameSource for Shot {
    fn frame(&mut self, _c: &Clip, _secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let v = (0..w * h)
            .flat_map(|i| {
                let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                if (0.3..0.7).contains(&fx) && (0.3..0.7).contains(&fy) { [GREEN[0], GREEN[1], GREEN[2], 1.0] } else { [GREY, GREY, GREY, 1.0] }
            })
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(v)), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, _secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        if media != PICTURE {
            return Err(SideMissing::File(format!("{media}.mov")));
        }
        Ok((0..w * h).flat_map(|_| [0.9, 0.03, 0.03, 1.0]).collect())
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn cpu(seq: &Sequence, channel: ViewChannel) -> Vec<u8> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (pixels, _) = r.render_inspected(seq, tc(1.0), W, H, &mut Shot, RenderOptions { view_channel: channel, ..Default::default() }, Inspect::default());
    klipp_color::display::display_transform_ws_to(&pixels, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

/// `seq` on the GPU display stack in `channel`, nothing fallen back;
/// `None` when the stack declined and no GPU is required.
fn stack(seq: &Sequence, channel: ViewChannel, name: &str) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Shot, RenderOptions { view_channel: channel, ..Default::default() });
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "{name}: the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: {name}: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "{name}: nothing falls back: {:?}", fell.events);
    Some(bytes)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// What the harness fills a slot of `kind` with.
fn filled(kind: SlotKind) -> TemplateValue {
    match kind {
        SlotKind::Media => TemplateValue::Text(PICTURE.into()),
        SlotKind::Colour => {
            let srgb = |v: f32| klipp_color::transfer::linear_to_srgb(v);
            TemplateValue::Color([srgb(GREEN[0]), srgb(GREEN[1]), srgb(GREEN[2]), 1.0])
        }
        other => panic!("the harness has no {other:?} to fill a slot with yet"),
    }
}

#[test]
fn every_built_in_recipe_draws_on_the_gpu_and_passes_its_checks() {
    let recipes = builtin_recipes();
    assert!(!recipes.is_empty());
    for recipe in &recipes {
        let name = recipe.name.as_str();
        let checks = &recipe.graph.as_ref().unwrap().checks;
        assert!(!checks.is_empty(), "{name} says what it does: give it checks");
        let plain = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
        let before = sequence(plain.clone());
        let values: BTreeMap<String, TemplateValue> = recipe.slots.iter().map(|(n, s)| (n.clone(), filled(s.kind))).collect();
        let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut placed = plain.clone();
        placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{name}: {e}"));
        let seq = sequence(placed);

        let want = cpu(&seq, ViewChannel::Picture);
        assert!(worst(&want, &cpu(&before, ViewChannel::Picture)) > 30, "{name} changes the picture");
        let Some(shown) = stack(&seq, ViewChannel::Picture, name) else { continue };
        assert!(worst(&want, &shown) <= 1, "{name}: the stack draws {} levels from the CPU", worst(&want, &shown));

        for check in checks {
            let Some(frame) = stack(&seq, check.channel, name) else { continue };
            let (x, y) = ((check.at[0] * W as f32) as u32, (check.at[1] * H as f32) as u32);
            let v = frame[((y * W + x) * 4) as usize] as f32 / 255.0;
            assert!((check.min..=check.max).contains(&v), "{name}: {} ({:?} at {:?} reads {v:.3}, not {}..{})", check.says, check.channel, check.at, check.min, check.max);
        }
    }
}
