//! Fonts that ship inside the binary, so a title looks the same on
//! every machine: a grotesk for broadcast and social work, a condensed
//! display face for impact, a serif for cinematic titles, and a mono
//! for slates and data. All four are under the SIL Open Font License;
//! the licence text sits beside each family in `assets/fonts`.

/// One face of a bundled family.
pub struct BundledFont {
    pub family: &'static str,
    /// "Regular", "Bold", "Italic" or "Bold Italic".
    pub style: &'static str,
    pub bytes: &'static [u8],
}

pub const BUNDLED: &[BundledFont] = &[
    BundledFont { family: "Inter", style: "Regular", bytes: include_bytes!("../../../assets/fonts/Inter/Inter-Regular.ttf") },
    BundledFont { family: "Inter", style: "Bold", bytes: include_bytes!("../../../assets/fonts/Inter/Inter-Bold.ttf") },
    BundledFont { family: "Inter", style: "Italic", bytes: include_bytes!("../../../assets/fonts/Inter/Inter-Italic.ttf") },
    BundledFont { family: "Inter", style: "Bold Italic", bytes: include_bytes!("../../../assets/fonts/Inter/Inter-BoldItalic.ttf") },
    BundledFont { family: "Bebas Neue", style: "Regular", bytes: include_bytes!("../../../assets/fonts/BebasNeue/BebasNeue-Regular.ttf") },
    BundledFont { family: "Libre Baskerville", style: "Regular", bytes: include_bytes!("../../../assets/fonts/LibreBaskerville/LibreBaskerville-Regular.ttf") },
    BundledFont { family: "Libre Baskerville", style: "Bold", bytes: include_bytes!("../../../assets/fonts/LibreBaskerville/LibreBaskerville-Bold.ttf") },
    BundledFont { family: "Libre Baskerville", style: "Italic", bytes: include_bytes!("../../../assets/fonts/LibreBaskerville/LibreBaskerville-Italic.ttf") },
    BundledFont { family: "Libre Baskerville", style: "Bold Italic", bytes: include_bytes!("../../../assets/fonts/LibreBaskerville/LibreBaskerville-BoldItalic.ttf") },
    BundledFont { family: "JetBrains Mono", style: "Regular", bytes: include_bytes!("../../../assets/fonts/JetBrainsMono/JetBrainsMono-Regular.ttf") },
    BundledFont { family: "JetBrains Mono", style: "Bold", bytes: include_bytes!("../../../assets/fonts/JetBrainsMono/JetBrainsMono-Bold.ttf") },
    BundledFont { family: "JetBrains Mono", style: "Italic", bytes: include_bytes!("../../../assets/fonts/JetBrainsMono/JetBrainsMono-Italic.ttf") },
    BundledFont { family: "JetBrains Mono", style: "Bold Italic", bytes: include_bytes!("../../../assets/fonts/JetBrainsMono/JetBrainsMono-BoldItalic.ttf") },
];

/// The family every new title starts with.
pub const DEFAULT_FAMILY: &str = "Inter";

/// The bytes for `family` in `style`, falling back to the family's
/// Regular face, then to its first face. `None` for a family that does
/// not ship with the app.
pub fn lookup(family: &str, style: &str) -> Option<&'static [u8]> {
    let faces = BUNDLED.iter().filter(|f| f.family == family);
    faces
        .clone()
        .find(|f| f.style == style)
        .or_else(|| faces.clone().find(|f| f.style == "Regular"))
        .or_else(|| faces.clone().next())
        .map(|f| f.bytes)
}

/// Every bundled family with the styles it ships, for the font picker.
pub fn families() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut out: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for f in BUNDLED {
        match out.iter_mut().find(|(fam, _)| *fam == f.family) {
            Some((_, styles)) => styles.push(f.style),
            None => out.push((f.family, vec![f.style])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_face_parses() {
        for f in BUNDLED {
            assert!(
                fontdue::Font::from_bytes(f.bytes, fontdue::FontSettings::default()).is_ok(),
                "{} {} does not parse",
                f.family,
                f.style
            );
        }
    }

    #[test]
    fn lookup_falls_back_to_regular() {
        assert!(lookup("Bebas Neue", "Bold").is_some());
        assert!(lookup("Nope", "Regular").is_none());
        assert_eq!(families().len(), 4);
    }
}
