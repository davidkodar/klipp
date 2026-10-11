//! The names the lifted picture and its readers share (#997 step 12e-0):
//! an effect's node type by its tag, its keyframe names, a clip's aspect.
//! The conversion uses them and so does everything that reads or edits a
//! lifted picture, so they live outside `lift` and `to_v2`, which only the
//! conversion needs.

use klipp_graph_format::types;

/// Effect keyframe names that animate a parameter of another name
/// (`render.rs`: ChromaKey's `spill` drives `soft_edge`). Kept in step with
/// the registry's keyframe aliases by a test.
pub const KEYFRAME_ALIASES: [(&str, &str, &str); 1] = [("chroma_key", "spill", "soft_edge")];

/// The keyframe name an effect asks for (`BuiltinEffect::animate`) of the
/// parameter its node animates under `key`: the inverse of
/// [`KEYFRAME_ALIASES`], so a ChromaKey node's `soft_edge` is read as
/// `spill`. `tag` is the effect's saved tag (`"chroma_key"`).
pub fn keyframe_name<'a>(tag: &str, key: &'a str) -> &'a str {
    KEYFRAME_ALIASES.iter().find(|(t, _, to)| *t == tag && *to == key).map_or(key, |(_, from, _)| from)
}

/// The key a node's animation is stored under for the keyframe name `key`
/// (`tag` the effect's saved tag): [`KEYFRAME_ALIASES`]' own, so a
/// ChromaKey's `spill` is its `soft_edge`. Every other name is itself. An
/// edit by path naming either reaches the one animation.
pub fn anim_key<'a>(tag: &str, key: &'a str) -> &'a str {
    KEYFRAME_ALIASES.iter().find(|(t, from, _)| *t == tag && *from == key).map_or(key, |(_, _, to)| to)
}

/// A clip's aspect from its two scales (`PlacementProps::aspect`): `None`
/// where they agree, or the width is 0 or absent.
pub fn aspect(scale_x: Option<f32>, scale_y: Option<f32>) -> Option<f32> {
    let (x, y) = (scale_x.unwrap_or(1.0), scale_y.or(scale_x).unwrap_or(1.0));
    (x != 0.0 && y != x).then(|| y / x).filter(|a| a.is_finite())
}

/// The node type a built-in effect with saved tag `tag` becomes: its own
/// for the four the clip places itself (opacity, transform, speed ramp,
/// temporal denoise), `klipp.legacy.effect` for every other. The one rule
/// the conversion and a clip's graph-op writers share.
pub fn effect_node_type(tag: &str) -> &'static str {
    match tag {
        "opacity" => types::LEGACY_OPACITY,
        "transform" => types::LEGACY_TRANSFORM,
        "speed_ramp" => types::LEGACY_SPEED_RAMP,
        "temporal_denoise" => types::LEGACY_TEMPORAL_DENOISE,
        _ => types::LEGACY_EFFECT,
    }
}
