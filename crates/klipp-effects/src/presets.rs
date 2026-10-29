//! The built-in effect presets (#2887): `assets/presets/effects/
//! builtin.toml`, compiled in. Each a name and a line saying what it is,
//! and the effect at its values; a few also go by a key the ops and the
//! app ask for them by (an old-film template's `super8`, a reflection
//! surface's `calm_water`, a particle kind's `snow`).

use crate::BuiltinEffect;

/// One built-in effect preset.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EffectPreset {
    /// What the ops and the app ask for it by, where they do.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    pub name: String,
    pub description: String,
    pub effect: BuiltinEffect,
}

/// The form of the presets file this Klipp reads.
pub const FORMAT_VERSION: u32 = 1;

/// Every built-in effect preset, in the file's order. A file that
/// doesn't read leaves none, said, never a crash.
pub fn builtin() -> &'static [EffectPreset] {
    static PRESETS: std::sync::OnceLock<Vec<EffectPreset>> = std::sync::OnceLock::new();
    PRESETS.get_or_init(|| match klipp_presets::read(include_str!("../../../assets/presets/effects/builtin.toml"), "effect_preset", FORMAT_VERSION, "preset") {
        Ok(presets) => presets,
        Err(why) => {
            tracing::error!("the built-in effect presets don't read: {why}");
            Vec::new()
        }
    })
}

/// The built-in preset of the effect saved as `tag` that goes by `key`.
pub fn keyed(tag: &str, key: &str) -> Option<&'static EffectPreset> {
    builtin().iter().find(|p| p.key == key && crate::registry::tag_of(&p.effect) == Some(tag))
}

/// The built-in presets of the effect saved as `tag` that go by a key,
/// in order.
pub fn keys_of(tag: &'static str) -> impl Iterator<Item = &'static EffectPreset> {
    builtin().iter().filter(move |p| !p.key.is_empty() && crate::registry::tag_of(&p.effect) == Some(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The presets file reads whole: all 151, each a described effect
    /// with a name and a line; the keyed ones are the four old-film
    /// templates, the five reflection surfaces and a preset for each
    /// particle kind, each key once for its effect.
    #[test]
    fn the_built_in_presets_read_whole_and_keyed() {
        let all = builtin();
        assert_eq!(all.len(), 151);
        for p in all {
            assert!(crate::registry::tag_of(&p.effect).is_some(), "{}: a described effect", p.name);
            assert!(!p.name.is_empty() && !p.description.is_empty(), "{:?}", p.name);
        }
        let keys = |tag: &'static str| keys_of(tag).map(|p| p.key.as_str()).collect::<Vec<_>>();
        assert_eq!(keys("old_film"), ["super8", "film_16mm", "silent_film", "faded_70s"]);
        assert_eq!(keys("reflection"), ["calm_water", "rippled_water", "glossy_floor", "wet_street", "glass"]);
        assert_eq!(keys("particles"), crate::particles::KINDS);
        for (kind, name) in crate::particles::KINDS.iter().enumerate() {
            assert!(matches!(crate::particles::preset(kind as u32), BuiltinEffect::Particles { style, .. } if style as usize == kind), "{name}");
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in all.iter().filter(|p| !p.key.is_empty()) {
            assert!(seen.insert((crate::registry::tag_of(&p.effect), p.key.as_str())), "{} twice", p.key);
        }
    }
}
