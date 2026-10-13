//! A project written before layers carried their own stacks renders
//! exactly as it did. The fixture under `klipp-project/tests/fixtures/
//! pre_layer_fx` was saved by the build before this change — a text
//! layer with a staggered rise, a fade exit, a pulse, glow, shadow,
//! outline, inner shadow and motion blur, behind it a rounded backing
//! and over it a picture — and the hashes below are its frames from
//! that build's renderer. The loader hoists the old text-box fields
//! onto the object; the renderer reads the object; the picture is the
//! same to the byte.
//!
//! The fixture stays in that build's format, as the converter's tests
//! read it; like any real project it goes through the one-time
//! conversion to revision 2 (#1089) before the app opens it, here on a
//! temp copy.

use klipp_core::title_motion::{MotionKind, StaggerUnit, TitleLoop};
use klipp_project::load_project;
use klipp_timeline::{ClipKind, TitleObjectContent};

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// A temp copy of the fixture, converted to revision 2 as `klipp-convert
/// run` converts a real project.
fn converted_fixture() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-project/tests/fixtures/pre_layer_fx");
    let dir = std::env::temp_dir().join(format!("klipp-layer-fx-goldens-{}", std::process::id())).join("pre_layer_fx");
    std::fs::create_dir_all(dir.join("media")).unwrap();
    std::fs::copy(src.join("media/manifest.json"), dir.join("media/manifest.json")).unwrap();
    let old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(src.join("project.json")).unwrap()).unwrap();
    let (converted, _notes) = klipp_graph_lift::revision2::project(&old).unwrap();
    std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&converted).unwrap()).unwrap();
    dir
}

#[test]
fn a_project_from_before_the_stacks_moved_renders_the_same() {
    let dir = converted_fixture();
    let (sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
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
    // Re-pinned for OpenType shaping (#1501): the title's letters now
    // kern, which moves them; nothing else in the fixture changed.
    // Re-pinned when a posed or faded title stopped being rounded to whole
    // levels before the frame's own rounding (#190): against the frames
    // before, at most 1 level premultiplied, on 131 to 234 of 129,600
    // pixels.
    let goldens = [(0.1f32, 0xc4a78c449354acbau64), (0.5, 0xf255e760b78eeba6), (2.0, 0x0ea2e8e8569d4ee0)];
    for (t, want) in goldens {
        let px = r.render_with_window(td, 480, 270, None, t, Some(4.0));
        assert_eq!(fnv(&px), want, "frame at {t} s differs from the build before the move");
    }
}
