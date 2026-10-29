//! Font pairings for brand kits (#2722): `assets/fonts/pairings.toml`,
//! a heading family and a body family that sit well together, each
//! bundled or in the catalogue, with the mood it suits and its writing.

use serde::Deserialize;

/// One pairing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    pub heading: String,
    pub body: String,
    pub mood: Vec<String>,
    pub scripts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[allow(dead_code)]
    format: u32,
    pair: Vec<Pairing>,
}

/// Every pairing, in the file's order.
pub fn all() -> &'static [Pairing] {
    static PAIRS: std::sync::OnceLock<Vec<Pairing>> = std::sync::OnceLock::new();
    PAIRS.get_or_init(|| toml::from_str::<File>(include_str!("../../../assets/fonts/pairings.toml")).map(|f| f.pair).unwrap_or_else(|e| {
        tracing::error!("the font pairings do not read: {e}");
        Vec::new()
    }))
}

/// The pairings whose mood or writing has every word of `query`.
pub fn query(query: &str) -> Vec<&'static Pairing> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    all().iter().filter(|p| words.iter().all(|w| p.mood.iter().chain(&p.scripts).any(|t| t == w) || p.heading.to_lowercase().contains(w) || p.body.to_lowercase().contains(w))).collect()
}

/// Whether `family` is bundled with Klipp or in its catalogue.
pub fn available(family: &str) -> bool {
    crate::bundled::BUNDLED.iter().any(|b| b.family.eq_ignore_ascii_case(family)) || crate::catalogue::family(family).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pairing reads, and each family is bundled or in the catalogue
    /// and covers the writing the pairing names.
    #[test]
    fn every_pairing_is_of_families_klipp_can_draw() {
        assert!(all().len() >= 10, "{} pairings", all().len());
        for p in all() {
            for f in [&p.heading, &p.body] {
                assert!(available(f), "{f} is neither bundled nor in the catalogue");
                if let Some(c) = crate::catalogue::family(f) {
                    for s in &p.scripts {
                        assert!(c.scripts.contains(s) || (s == "latin" && crate::bundled::BUNDLED.iter().any(|b| b.family == *f)), "{f} does not cover {s}");
                    }
                }
            }
        }
        assert!(query("arabic").iter().any(|p| p.body == "Cairo"));
        assert!(query("editorial warm").iter().any(|p| p.heading == "Playfair Display"));
    }
}
