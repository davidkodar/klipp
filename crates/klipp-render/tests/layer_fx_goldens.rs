//! A project written before layers carried their own stacks renders
//! exactly as it did. The fixture under `klipp-project/tests/fixtures/
//! pre_layer_fx` was saved by the build before this change — a text
//! layer with a staggered rise, a fade exit, a pulse, glow, shadow,
//! outline, inner shadow and motion blur, behind it a rounded backing
//! and over it a picture — and the hashes below are its frames from
//! that build's renderer. The loader hoists the old text-box fields
//! onto the object; the renderer reads the object; the picture is the
//! same to the byte.

use klipp_core::title_motion::{MotionKind, StaggerUnit, TitleLoop};
use klipp_project::load_project;
use klipp_timeline::{ClipKind, TitleObjectContent};

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

#[test]
fn a_project_from_before_the_stacks_moved_renders_the_same() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-project/tests/fixtures/pre_layer_fx");
    let (sequences, ..) = load_project(&dir).unwrap();
    let ClipKind::Title(td) = &sequences[0].tracks[0].clips[0].kind else { panic!("the fixture's title") };

    // The old fields came up onto the object, all of them, and are
    // gone from the text box.
    let text = td.primary_object().expect("a text layer");
    let fx = text.fx.as_ref().expect("the stack is explicit after loading");
    let enter = fx.motion.enter.expect("the rise");
    assert_eq!((enter.kind, enter.stagger, enter.stagger_secs), (MotionKind::Rise, StaggerUnit::Word, 0.2));
    assert_eq!(fx.motion.exit.map(|m| m.kind), Some(MotionKind::Fade));
    assert!(matches!(fx.loops.as_slice(), [TitleLoop::Pulse { .. }]));
    assert_eq!((fx.glows.len(), fx.shadows.len(), fx.outlines.len(), fx.inner_shadows.len()), (1, 1, 1, 1));
    assert!(fx.motion_blur.as_ref().is_some_and(|m| m.enabled && m.samples == 4));
    let TitleObjectContent::TextBox(tb) = &text.content else { unreachable!() };
    assert!(tb.legacy_fx.is_default(), "nothing is left in the old place");
    for o in &td.objects {
        assert!(o.fx.is_some(), "every object's stack is explicit: {}", o.name);
    }
    assert!(td.objects.iter().filter(|o| !matches!(o.content, TitleObjectContent::TextBox(_))).all(|o| o.fx().is_default()), "the backing and the picture carry nothing yet");

    // And the frames are the frames.
    let mut r = klipp_title::TitleRenderer::new();
    let goldens = [(0.1f32, 0xeb6f390fffb5cc81u64), (0.5, 0xa908dbad6635812c), (2.0, 0xca8a6c0a50485b39)];
    for (t, want) in goldens {
        let px = r.render_with_window(td, 480, 270, None, t, Some(4.0));
        assert_eq!(fnv(&px), want, "frame at {t} s differs from the build before the move");
    }
}
