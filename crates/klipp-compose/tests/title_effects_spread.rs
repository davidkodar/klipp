//! A title's picture effects reach past its glyphs (#2703): a blur
//! softens out of the rows the text was drawn in, a directional blur
//! trails along its angle only, a glow lights the dark around the text,
//! and the GPU stack draws what the CPU path draws for each.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

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

/// White "Klipp" over black with `fx`, on the GPU stack and on the CPU
/// path; `None` without a GPU stack.
fn render(fx: Vec<E>) -> Option<Vec<u8>> {
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Klipp".into(), font_size: 240.0, ..Default::default() });
    let title = TestClip::of(Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }).effects(fx.clone()).build();
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![title];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (stack, stats) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default())?;
    assert_eq!(stats.effects_on_layers, u32::from(!fx.is_empty()), "{fx:?}: the title's effects ran over its slot");
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()));
    let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 3, "{fx:?}: the stack differs from the CPU path by {worst} levels");
    Some(stack)
}

/// The box around every pixel brighter than `over`: x0, y0, x1, y1.
fn lit(px: &[u8], over: u8) -> (u32, u32, u32, u32) {
    let mut b = (W, H, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if px[((y * W + x) * 4) as usize] > over {
                b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    assert!(b.0 <= b.2, "something is lit");
    b
}

/// The light outside `b`: the red channel summed.
fn outside(px: &[u8], b: (u32, u32, u32, u32)) -> u64 {
    let mut sum = 0;
    for y in 0..H {
        for x in 0..W {
            if x < b.0 || x > b.2 || y < b.1 || y > b.3 {
                sum += u64::from(px[((y * W + x) * 4) as usize]);
            }
        }
    }
    sum
}

#[test]
fn a_titles_effects_reach_past_its_glyphs() {
    let Some(plain) = render(Vec::new()) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let text = lit(&plain, 40);

    let blur = render(vec![E::GaussianBlur { radius: 8.0 }]).expect("the stack drew the plain title");
    let soft = lit(&blur, 8);
    assert!(soft.1 + 3 <= text.1 && soft.3 >= text.3 + 3, "a blur softens above and below the text: {soft:?} vs {text:?}");
    assert!(soft.0 + 3 <= text.0 && soft.2 >= text.2 + 3, "and to its sides: {soft:?} vs {text:?}");

    let smear = render(vec![E::DirectionalBlur { angle: 0.0, distance: 30.0, samples: 24 }]).expect("the stack drew the plain title");
    let along = lit(&smear, 8);
    // A trail behind the move, so to one side.
    assert!(along.0 + 8 <= text.0 || along.2 >= text.2 + 8, "a level smear reaches out sideways: {along:?} vs {text:?}");
    assert!(along.1 + 2 >= text.1 && along.3 <= text.3 + 2, "and not up or down: {along:?} vs {text:?}");

    let glow = render(vec![E::Glow { radius: 20.0, intensity: 1.0, threshold: 0.0 }]).expect("the stack drew the plain title");
    assert!(outside(&glow, text) > outside(&plain, text) + 2_000, "a glow lights the dark around the text: {} vs {}", outside(&glow, text), outside(&plain, text));
}
