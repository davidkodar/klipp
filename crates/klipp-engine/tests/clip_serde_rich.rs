//! A clip with every picture field set *and filled* (modifiers inside
//! their stacks, node parameters, every lane, a modifier on an effect
//! parameter) serialises and hashes for the render cache as pinned, and
//! the lifted picture it may hold changes neither (#1089, M1b 2b; the
//! #1162 review). The modifier on an effect parameter is a struct map
//! key JSON can't write, so it is added after reading and is in the
//! canonical bytes only.

use klipp_core::{ModifierStack, ParamId};
use klipp_engine::canonical_hash::canonical_bytes;
use klipp_timeline::Clip;

const JSON: &str = include_str!("fixtures/populated_clip_rich.json");
const CANONICAL: &[u8] = include_bytes!("fixtures/populated_clip_rich.canonical");

fn oscillate(amplitude: f32) -> klipp_core::Modifier {
    klipp_core::Modifier::Oscillate { amplitude, frequency: 2.0, phase: 0.25, waveform: Default::default(), blend: Default::default(), enabled: true }
}

/// The fixture as read, with the modifier JSON can't hold.
fn read() -> Clip {
    let mut clip: Clip = serde_json::from_str(JSON).unwrap();
    clip.edit_param_modifiers(|m| {
        m.insert(ParamId::Effect { effect_index: 1, param_name: "mix".into() }, ModifierStack { modifiers: vec![oscillate(0.2)] })
    });
    clip
}

#[test]
fn a_filled_clip_serialises_and_hashes_as_pinned_lifted_or_not() {
    let clip = read();
    let expected: serde_json::Value = serde_json::from_str(JSON).unwrap();
    let mut without = clip.clone();
    without.edit_param_modifiers(|m| m.retain(|k, _| !matches!(k, ParamId::Effect { .. })));
    let json = || serde_json::from_str::<serde_json::Value>(&serde_json::to_string(&without).unwrap()).unwrap();
    assert_eq!(json(), expected, "the clip's JSON");
    assert!(canonical_bytes(&clip).unwrap() == CANONICAL, "the clip's canonical bytes");
    clip.picture_v2().unwrap();
    without.picture_v2().unwrap();
    assert_eq!(json(), expected, "the same once lifted");
    assert!(canonical_bytes(&clip).unwrap() == CANONICAL, "the same canonical bytes once lifted");
}

/// Writes the fixture: `KLIPP_WRITE_CLIP_FIXTURE=1 cargo test -p klipp-engine
/// --test clip_serde_rich -- --ignored`.
#[test]
#[ignore]
fn write_the_fixture() {
    use klipp_core::{Interpolation, Keyframe, MediaId, RationalTime};
    use klipp_timeline::GradeAccess;
    if std::env::var_os("KLIPP_WRITE_CLIP_FIXTURE").is_none() {
        return;
    }
    let t = |n| RationalTime::from_frames(n, 24);
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: Interpolation::Bezier, tangent_out: (0.1, 0.2), tangent_in: (-0.1, -0.2), handles: Default::default() };
    let mut clip = Clip::media(MediaId::new(), t(0), t(48), t(0), t(48));
    clip.edit_grade(|g| {
        g.nodes[1].params.exposure = Some(0.75);
        g.nodes[1].params.saturation = Some(1.2);
    });
    clip.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.1 });
    clip.push_effect(klipp_effects::BuiltinEffect::Invert { mix: 0.7 });
    let second = clip.effect_id_at(1).unwrap();
    clip.set_effect_enabled(second, false);
    clip.edit_opacity_keyframes(|v| v.extend([key(0.0, 1.0), key(1.0, 0.5)]));
    clip.edit_transform_pos_x_keyframes(|v| v.push(key(0.5, 10.0)));
    clip.edit_transform_pos_y_keyframes(|v| v.push(key(0.5, -20.0)));
    clip.edit_transform_scale_keyframes(|v| v.push(key(0.75, 1.25)));
    clip.edit_transform_rotation_keyframes(|v| v.push(key(1.5, 45.0)));
    clip.edit_transform_tilt_x_keyframes(|v| v.push(key(0.2, 5.0)));
    clip.edit_transform_tilt_y_keyframes(|v| v.push(key(0.25, 15.0)));
    clip.edit_effect_keyframes(|m| m.insert("0:radius".into(), vec![key(0.0, 0.3), key(2.0, 4.0)]));
    clip.edit_param_modifiers(|m| {
        m.insert(ParamId::Opacity, ModifierStack { modifiers: vec![oscillate(0.4)] });
        m.insert(ParamId::Rotation, ModifierStack { modifiers: vec![oscillate(10.0), oscillate(3.0)] });
    });
    let warm = klipp_timeline::GradeVersion::new("Warm", clip.grade().clone());
    clip.edit_grade_versions(|vs| vs.push(warm));
    clip.set_active_grade_version(Some(0));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::write(dir.join("populated_clip_rich.json"), serde_json::to_string_pretty(&clip).unwrap() + "\n").unwrap();
    // As the test reads it: through text, with the effect-parameter modifier.
    std::fs::write(dir.join("populated_clip_rich.canonical"), canonical_bytes(&read_from(&dir)).unwrap()).unwrap();
}

fn read_from(dir: &std::path::Path) -> Clip {
    let mut clip: Clip = serde_json::from_str(&std::fs::read_to_string(dir.join("populated_clip_rich.json")).unwrap()).unwrap();
    clip.edit_param_modifiers(|m| {
        m.insert(ParamId::Effect { effect_index: 1, param_name: "mix".into() }, ModifierStack { modifiers: vec![oscillate(0.2)] })
    });
    clip
}
