//! A composition template placed as one clip (#2757) draws on the GPU
//! stack what its composition draws with the same value set by hand: the
//! template's parameter put on the copied composition. Its scene is still
//! composed on the CPU (the precomposition gap on main, at most two
//! fallbacks), until the stack composes precompositions itself. A
//! missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

struct Nothing;

impl FrameSource for Nothing {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

/// A card: a headline on a coloured backing.
fn card(text: &str) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 120.0, ..Default::default() });
    td.background = TitleBackground::Color { color: [30, 90, 200, 255], padding: 24.0 };
    let mut t = Track::new("T1", TrackKind::Text);
    t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(3.0)) }];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![t];
    seq
}

/// `clip` laid in a sequence of its own, flattened with `composition`.
fn outer(clip: Clip, composition: &Sequence) -> Sequence {
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![v];
    klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), composition.clone()])
}

fn on_gpu(seq: &Sequence) -> Option<Vec<u8>> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()).map(|(px, _)| px)
}

#[test]
fn a_placed_template_draws_its_composition_with_the_value_set() {
    // The template: the card, its headline a parameter.
    let made = card("Launch");
    let instance = klipp_core::TemplateInstance {
        template_id: klipp_core::SequenceId::new().0,
        bindings: [("text".to_string(), "object:0.text".to_string())].into_iter().collect(),
        overrides: [("text".to_string(), klipp_core::TemplateValue::Text("Ship it".into()))].into_iter().collect(),
        media_slots: Default::default(),
    };
    let mut placed = Clip::precomposition(made.id, tc(0.0), tc(3.0));
    placed.kind = ClipKind::TemplatePrecomposition { sequence_id: made.id, template_instance: instance };
    let Some(placed_px) = on_gpu(&outer(placed, &made)) else {
        assert!(!required(), "required, but the GPU stack declined the placed template");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let fallbacks = klipp_effects::residency::last_frame().fallback_count();
    // The same card with the headline typed in, laid in as it is.
    let typed = card("Ship it");
    let reference = on_gpu(&outer(Clip::precomposition(typed.id, tc(0.0), tc(3.0)), &typed)).expect("the stack drew the card");
    let worst = placed_px.iter().zip(&reference).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 2, "the placed template differs from its composition by {worst} levels");
    let blue = placed_px.chunks(4).filter(|p| p[2] > 150 && p[0] < 100).count();
    assert!(blue > 1000, "the card is drawn: {blue} pixels of its backing");
    // The stack has no precomposition path yet: any precomposition's
    // scene is composed on the CPU and uploaded (composition.layer and
    // title.cpu_composition), templates or not. The next part of #2757
    // composes it on the GPU and takes this to 0.
    assert!(fallbacks <= 2, "only the known precomposition fallbacks: {fallbacks}");
}
