//! A metal surface for a title's text (#2706): gold, silver, steel,
//! copper, bronze, chrome or brushed metal. Metal reads as metal by what
//! it reflects, so the face mirrors an environment through a bevel, with
//! a fresnel edge, highlights from a key and a rim light, and glints.
//! Every field has a value that looks right with the preset alone.

use serde::{Deserialize, Serialize};

/// The metals a title can be made of. Each sets every other field of a
/// [`TitleMaterial`] ([`TitleMaterial::preset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetalPreset {
    #[default]
    Gold,
    Silver,
    Steel,
    Copper,
    RoseGold,
    Bronze,
    Chrome,
    BrushedMetal,
}

impl MetalPreset {
    pub const ALL: [MetalPreset; 8] = [Self::Gold, Self::Silver, Self::Steel, Self::Copper, Self::RoseGold, Self::Bronze, Self::Chrome, Self::BrushedMetal];

    /// The name the ops and the app give it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gold => "gold",
            Self::Silver => "silver",
            Self::Steel => "steel",
            Self::Copper => "copper",
            Self::RoseGold => "rose_gold",
            Self::Bronze => "bronze",
            Self::Chrome => "chrome",
            Self::BrushedMetal => "brushed_metal",
        }
    }

    /// The label the app shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gold => "Gold",
            Self::Silver => "Silver",
            Self::Steel => "Steel",
            Self::Copper => "Copper",
            Self::RoseGold => "Rose gold",
            Self::Bronze => "Bronze",
            Self::Chrome => "Chrome",
            Self::BrushedMetal => "Brushed metal",
        }
    }

    /// The preset by its name; `iron` is steel, `brushed` brushed metal.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        match name.as_str() {
            "iron" => Some(Self::Steel),
            "brushed" => Some(Self::BrushedMetal),
            _ => Self::ALL.into_iter().find(|p| p.name() == name),
        }
    }
}

/// A light on the metal: from `azimuth` degrees around (0 from the right,
/// 90 from below, as a 3D title's light) and `elevation` degrees towards
/// the viewer, at `strength`, in `color` (sRGB).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetalLight {
    pub azimuth: f32,
    pub elevation: f32,
    pub strength: f32,
    pub color: [u8; 3],
}

impl MetalLight {
    /// The unit vector towards the light, in the frame's axes (x right,
    /// y down, z into the frame), as a 3D title's light.
    pub fn direction(&self) -> [f32; 3] {
        let (az, el) = (self.azimuth.to_radians(), self.elevation.clamp(-89.0, 89.0).to_radians());
        [el.cos() * az.cos(), el.cos() * az.sin(), -el.sin()]
    }
}

/// What the metal reflects, as a mix: a studio of soft boxes over a dark
/// floor, a sky from a neutral-to-warm horizon to a pale zenith, its
/// colour shifted by `sky_tint` (sRGB; white leaves it as it is), and
/// the shot behind the title, blurred by `shot_blur` (a fraction of the
/// frame's height), so the metal takes the scene's colours and light.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetalEnvironment {
    pub studio: f32,
    pub sky: f32,
    pub sky_tint: [u8; 3],
    pub shot: f32,
    pub shot_blur: f32,
}

impl Default for MetalEnvironment {
    fn default() -> Self {
        Self { studio: 0.65, sky: 0.35, sky_tint: [255, 255, 255], shot: 0.5, shot_blur: 0.04 }
    }
}

/// A title's metal surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleMaterial {
    pub preset: MetalPreset,
    /// The metal's reflectance facing the viewer, linear RGB 0..1: its
    /// colour.
    pub tint: [f32; 3],
    /// 0 a mirror, 1 a satin blur of what it reflects.
    pub roughness: f32,
    /// 0..1: streaks along `anisotropy_angle` (degrees, 0 across), as on
    /// brushed metal.
    pub anisotropy: f32,
    pub anisotropy_angle: f32,
    /// 0..1: how much brighter and whiter the surface turns at grazing
    /// angles (fresnel).
    pub edge: f32,
    /// 0..2: the lights' highlights.
    pub highlights: f32,
    /// 0..1: small star sparkles on the brightest points.
    pub glints: f32,
    /// The bevel's width, a fraction of the font size (0..0.3), and how
    /// steep it is (0..1): flat text gets its shape from it.
    pub bevel: f32,
    pub depth: f32,
    /// 0..1: a dark line round the letters' outer edge, which keeps a
    /// light metal's shape on a bright picture.
    pub contour: f32,
    pub environment: MetalEnvironment,
    pub key_light: MetalLight,
    pub rim_light: Option<MetalLight>,
}

impl Default for TitleMaterial {
    fn default() -> Self {
        Self::preset(MetalPreset::Gold)
    }
}

impl TitleMaterial {
    /// A metal as its preset makes it. The tints are the metals' measured
    /// reflectance at normal incidence (linear).
    pub fn preset(preset: MetalPreset) -> Self {
        let key = MetalLight { azimuth: -60.0, elevation: 35.0, strength: 1.0, color: [255, 246, 232] };
        let rim = MetalLight { azimuth: 120.0, elevation: -10.0, strength: 0.6, color: [235, 242, 255] };
        let base = Self {
            preset,
            tint: [1.0, 0.766, 0.336],
            roughness: 0.12,
            anisotropy: 0.0,
            anisotropy_angle: 0.0,
            edge: 0.6,
            highlights: 1.0,
            glints: 0.25,
            bevel: 0.06,
            depth: 0.7,
            contour: 0.25,
            environment: MetalEnvironment::default(),
            key_light: key,
            rim_light: Some(rim),
        };
        let env = |studio, sky, shot| MetalEnvironment { studio, sky, shot, ..MetalEnvironment::default() };
        match preset {
            MetalPreset::Gold => Self { environment: env(0.55, 0.5, 0.5), ..base },
            // Warm white and bright, its bands soft.
            MetalPreset::Silver => Self { tint: [0.97, 0.95, 0.90], roughness: 0.28, contour: 0.35, environment: env(0.4, 0.7, 0.5), ..base },
            // Darker and bluish grey, a little rough.
            MetalPreset::Steel => Self { tint: [0.40, 0.43, 0.48], roughness: 0.34, glints: 0.1, edge: 0.7, contour: 0.4, ..base },
            // Orange-red brown: its edge kept low, so grazing light does
            // not wash it pink.
            MetalPreset::Copper => Self { tint: [0.93, 0.47, 0.27], roughness: 0.18, edge: 0.35, key_light: MetalLight { color: [255, 228, 200], ..key }, ..base },
            MetalPreset::RoseGold => Self { tint: [0.98, 0.72, 0.64], roughness: 0.12, edge: 0.5, ..base },
            MetalPreset::Bronze => Self { tint: [0.62, 0.40, 0.20], roughness: 0.28, glints: 0.1, edge: 0.45, ..base },
            // A mirror: hard bands from a studio, near-black between them.
            MetalPreset::Chrome => Self { tint: [0.62, 0.63, 0.64], roughness: 0.02, edge: 0.9, glints: 0.35, contour: 0.45, environment: env(1.3, 0.0, 0.3), ..base },
            MetalPreset::BrushedMetal => Self { tint: [0.52, 0.53, 0.56], roughness: 0.3, anisotropy: 0.8, glints: 0.0, contour: 0.45, ..base },
        }
    }

    /// The colour a title without a GPU shows instead (sRGB): the tint
    /// as the eye sees it under the default studio, a little darker than
    /// its highlights.
    pub fn fallback_color(&self) -> [u8; 4] {
        let encode = |x: f32| if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
        let c = |v: f32| (encode((v * 0.8).clamp(0.0, 1.0)) * 255.0).round() as u8;
        [c(self.tint[0]), c(self.tint[1]), c(self.tint[2]), 255]
    }

    /// The fields held in range: what a project or an op could leave out
    /// of bounds is brought back in.
    pub fn clamped(mut self) -> Self {
        let unit = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        self.tint = self.tint.map(unit);
        self.roughness = unit(self.roughness);
        self.anisotropy = unit(self.anisotropy);
        self.edge = unit(self.edge);
        self.glints = unit(self.glints);
        self.depth = unit(self.depth);
        self.contour = unit(self.contour);
        self.highlights = if self.highlights.is_finite() { self.highlights.clamp(0.0, 2.0) } else { 1.0 };
        self.bevel = if self.bevel.is_finite() { self.bevel.clamp(0.0, 0.3) } else { 0.06 };
        self.environment.studio = if self.environment.studio.is_finite() { self.environment.studio.clamp(0.0, 2.0) } else { 0.0 };
        self.environment.sky = if self.environment.sky.is_finite() { self.environment.sky.clamp(0.0, 2.0) } else { 0.0 };
        self.environment.shot = if self.environment.shot.is_finite() { self.environment.shot.clamp(0.0, 2.0) } else { 0.0 };
        self.environment.shot_blur = if self.environment.shot_blur.is_finite() { self.environment.shot_blur.clamp(0.0, 0.2) } else { 0.04 };
        fn light(l: MetalLight) -> MetalLight {
            MetalLight { strength: if l.strength.is_finite() { l.strength.clamp(0.0, 4.0) } else { 0.0 }, ..l }
        }
        self.key_light = light(self.key_light);
        self.rim_light = self.rim_light.map(light);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_named_and_found_by_its_name() {
        for p in MetalPreset::ALL {
            assert_eq!(MetalPreset::from_name(p.name()), Some(p));
            assert_eq!(TitleMaterial::preset(p).preset, p);
        }
        assert_eq!(MetalPreset::from_name("Iron"), Some(MetalPreset::Steel));
        assert_eq!(MetalPreset::from_name("brushed metal"), Some(MetalPreset::BrushedMetal));
        assert_eq!(MetalPreset::from_name("tin"), None);
    }

    #[test]
    fn a_material_keeps_its_fields_through_a_save_and_fills_missing_ones_from_gold() {
        let m = TitleMaterial { roughness: 0.4, rim_light: None, ..TitleMaterial::preset(MetalPreset::Copper) };
        let back: TitleMaterial = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        let partial: TitleMaterial = serde_json::from_str(r#"{"roughness": 0.5}"#).unwrap();
        assert_eq!(partial, TitleMaterial { roughness: 0.5, ..TitleMaterial::default() });
    }

    #[test]
    fn gold_falls_back_to_a_gold_colour_and_out_of_range_fields_come_back_in() {
        let [r, g, b, _] = TitleMaterial::default().fallback_color();
        assert!(r > g && g > b, "gold: {r} {g} {b}");
        let wild = TitleMaterial { roughness: 7.0, bevel: f32::NAN, highlights: -1.0, ..TitleMaterial::default() }.clamped();
        assert_eq!((wild.roughness, wild.bevel, wild.highlights), (1.0, 0.06, 0.0));
    }
}
