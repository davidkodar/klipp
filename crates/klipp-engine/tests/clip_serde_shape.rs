//! A clip with every picture field populated serialises, and hashes for the
//! render cache, exactly as it did before the picture fields were gathered
//! into `Clip::picture` (#1089, M1b 2a). The fixture was written by the
//! code before that move. Its JSON reads back and writes again as the same
//! value (a JSON object's key order, and a grade's `node_positions` map,
//! carry no meaning), and its canonical bytes, the render key's input,
//! which sort every map, are pinned byte for byte.

use klipp_engine::canonical_hash::canonical_bytes;
use klipp_timeline::Clip;

const JSON: &str = include_str!("fixtures/populated_clip.json");
const CANONICAL: &[u8] = include_bytes!("fixtures/populated_clip.canonical");

#[test]
fn a_populated_clip_serialises_and_hashes_as_before() {
    let clip: Clip = serde_json::from_str(JSON).unwrap();
    let expected: serde_json::Value = serde_json::from_str(JSON).unwrap();
    // Through text, as the socket sends it: `to_value` would widen an f32.
    let written: serde_json::Value = serde_json::from_str(&serde_json::to_string(&clip).unwrap()).unwrap();
    assert_eq!(written, expected, "the clip's JSON");
    assert!(canonical_bytes(&clip).unwrap() == CANONICAL, "the clip's canonical bytes");
}

/// The lifted picture a clip may hold is no part of what it is: its JSON
/// and its render key are the same whether or not it has been lifted.
#[test]
fn the_lifted_picture_changes_neither_json_nor_render_key() {
    let clip: Clip = serde_json::from_str(JSON).unwrap();
    let (json, key) = (serde_json::to_string(&clip).unwrap(), canonical_bytes(&clip).unwrap());
    assert!(!clip.has_lifted_picture());
    clip.picture_v2().unwrap();
    assert!(clip.has_lifted_picture());
    assert_eq!(serde_json::to_string(&clip).unwrap(), json);
    assert!(canonical_bytes(&clip).unwrap() == key);
    let copy = clip.clone();
    assert!(canonical_bytes(&copy).unwrap() == key, "nor does a clone sharing it");
}

/// Writes the fixture from today's code: `KLIPP_WRITE_CLIP_FIXTURE=1 cargo
/// test -p klipp-engine --test clip_serde_shape -- --ignored`.
#[test]
#[ignore]
fn write_the_fixture() {
    use klipp_core::{Interpolation, Keyframe, MediaId, ModifierStack, ParamId, RationalTime};
    use klipp_timeline::GradeAccess;
    if std::env::var_os("KLIPP_WRITE_CLIP_FIXTURE").is_none() {
        return;
    }
    let t = |n| RationalTime::from_frames(n, 24);
    let key = |time_secs, value| Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    let mut clip = Clip::media(MediaId::new(), t(0), t(48), t(0), t(48));
    clip.edit_grade(|g| g.nodes.truncate(3));
    clip.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.1 });
    clip.push_effect(klipp_effects::BuiltinEffect::Invert { mix: 0.7 });
    let second = clip.effect_id_at(1).unwrap();
    clip.set_effect_enabled(second, false);
    clip.set_ofx_effects(vec![klipp_plugin::AppliedEffect {
        plugin_name: "org.example.blur".into(),
        enabled: true,
        param_values: [("radius".to_string(), klipp_plugin::OfxValue::Double(2.5))].into(),
        ..Default::default()
    }]);
    clip.edit_opacity_keyframes(|v| v.extend([key(0.0, 1.0), key(1.0, 0.5)]));
    clip.edit_transform_pos_x_keyframes(|v| v.push(key(0.5, 10.0)));
    clip.edit_transform_tilt_y_keyframes(|v| v.push(key(0.25, 15.0)));
    clip.edit_effect_keyframes(|m| m.insert("0:amount".into(), vec![key(0.0, 0.3)]));
    clip.edit_param_modifiers(|m| {
        m.insert(ParamId::Opacity, ModifierStack::default());
    });
    let warm = klipp_timeline::GradeVersion::new("Warm", clip.grade().clone());
    clip.edit_grade_versions(|vs| vs.push(warm));
    clip.set_active_grade_version(Some(0));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    // Through text and back, so the fixture is the form that reads in.
    let json = serde_json::to_string_pretty(&clip).unwrap();
    let clip: Clip = serde_json::from_str(&json).unwrap();
    std::fs::write(dir.join("populated_clip.json"), serde_json::to_string_pretty(&clip).unwrap() + "\n").unwrap();
    std::fs::write(dir.join("populated_clip.canonical"), canonical_bytes(&clip).unwrap()).unwrap();
}

