//! Unified effect registry — merges built-in effects and OFX plugins into one browseable list.

use crate::browser::effects_tab::EffectCategory;

/// Whether an effect is built-in or from an OFX plugin.
#[derive(Debug, Clone)]
pub enum EffectSource {
    Builtin(klipp_effects::BuiltinEffect),
    Ofx(String),   // plugin identifier
    Audio(String), // audio effect type_id (e.g. "eq", "compressor")
    /// External LADSPA plugin. Holds the full discovered metadata so the
    /// inspector can render parameter sliders without a second lookup.
    Ladspa(klipp_audio::ladspa::LadspaPluginInfo),
}

/// A single entry in the effects browser.
#[derive(Debug, Clone)]
pub struct EffectEntry {
    pub name: String,
    pub category: EffectCategory,
    pub source: EffectSource,
    pub source_label: &'static str,
    pub grouping: String,
    /// Human-readable description (from OFX Describe, empty for built-ins).
    pub description: String,
}

/// A downscaled reference frame used as source for effect thumbnails.
#[derive(Clone)]
pub struct ReferenceFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct EffectRegistry {
    pub effects: Vec<EffectEntry>,
    /// The current reference frame for thumbnail generation.
    pub reference_frame: Option<ReferenceFrame>,
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectRegistry {
    pub fn new() -> Self {
        let effects = vec![
            EffectEntry {
                name: "Opacity".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Opacity { value: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Gaussian Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Sharpen".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Crop".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Crop {
                    left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Transform".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Transform {
                    position_x: 0.0, position_y: 0.0,
                    scale_x: 1.0, scale_y: 1.0,
                    rotation: 0.0,
                    anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Chroma Key".into(),
                category: EffectCategory::Keying,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChromaKey {
                    key_color: [0, 255, 0],
                    tolerance: 0.2,
                    spill_suppression: 0.1,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Keying".into(),
            },
            // Light effects
            EffectEntry {
                name: "Bloom".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Bloom {
                    threshold: 0.7, radius: 20.0, intensity: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Glow".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Glow {
                    radius: 30.0, intensity: 0.4,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Light Rays".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LightRays {
                    center_x: 0.5, center_y: 0.3,
                    threshold: 0.6, samples: 64,
                    decay: 0.3, intensity: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Vignette".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Vignette {
                    amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Diffusion".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Diffusion {
                    amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Lens Flare".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LensFlare {
                    position_x: 0.7, position_y: 0.3, brightness: 0.6,
                    flare_size: 0.15, ghost_count: 4, ghost_intensity: 0.3,
                    chromatic_aberration: 0.4,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Light Wrap".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LightWrap {
                    width: 15.0, intensity: 0.5, softness: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Anamorphic Streak".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::AnamorphicStreak {
                    threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Film Halation".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::FilmHalation {
                    threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Invert".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Invert { mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Posterize".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Posterize { levels: 8.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Mosaic".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Mosaic { block_size: 10.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Edge Detect".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::EdgeDetect { strength: 1.0, mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Emboss".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Emboss { angle: 135.0, strength: 1.0, mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Channel Mixer".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChannelMixer {
                    red_red: 1.0, red_green: 0.0, red_blue: 0.0,
                    green_red: 0.0, green_green: 1.0, green_blue: 0.0,
                    blue_red: 0.0, blue_green: 0.0, blue_blue: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Directional Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::DirectionalBlur {
                    angle: 0.0, distance: 20.0, samples: 8,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Mirror".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Mirror {
                    horizontal: 1, vertical: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Transform".into(),
            },
            EffectEntry {
                name: "Lens Distortion".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LensDistortion {
                    distortion: 0.0, scale: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Rolling Shutter Correction".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::RollingShutterCorrection {
                    strength: 0.5, readout_ratio: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Video Stabilization".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::VideoStabilization {
                    smoothing: 0.5, crop_ratio: 0.05, zoom: true,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Dilate".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Dilate { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Erode".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Erode { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Denoise".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Denoise {
                    spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Gamma".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Gamma { gamma: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "HSV Adjust".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::HSVAdjust {
                    hue_shift: 0.0, saturation: 1.0, value: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Noise Generator".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::NoiseGenerator {
                    amount: 0.1, monochrome: 0, seed: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Median Filter".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::MedianFilter { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Radial Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::RadialBlur {
                    center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Levels".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Levels {
                    in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Equalize".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Equalize { strength: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Letterbox".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Letterbox {
                    aspect_ratio: 2.39, bar_color: 0, opacity: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Chromatic Aberration".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChromaticAberration {
                    red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Speed Ramp".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::SpeedRamp {
                    speed: 1.0, blend_mode: 1, ramp_curve: 0,
                    ramp_start: 0.0, ramp_end: 1.0, reverse: false,
                    freeze_at: 0.0, freeze: false,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            // ── Audio effects ──
            EffectEntry {
                name: "Parametric EQ".into(),
                category: EffectCategory::AudioEQ,
                source: EffectSource::Audio("eq".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "EQ".into(),
            },
            EffectEntry {
                name: "Pitch Shift".into(),
                category: EffectCategory::AudioEQ,
                source: EffectSource::Audio("pitch_shift".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "EQ".into(),
            },
            EffectEntry {
                name: "Compressor".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("compressor".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Limiter".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("limiter".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Noise Gate".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("gate".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "De-Esser".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("deesser".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Normalize".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("normalize".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                // CODE_AUDIT C-3: this effect is a multi-band gate, not
                // spectral-subtraction NR. The internal type_id stays
                // "noise_reduction" so saved .klipp projects keep
                // loading; the user-facing name advertises the honest
                // behaviour.
                name: "Multi-band Gate".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("noise_reduction".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "De-click".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("declick".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Reverb".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("reverb".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Reverb".into(),
            },
            EffectEntry {
                name: "Delay".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("delay".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Reverb".into(),
            },
            EffectEntry {
                name: "Chorus".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("chorus".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Reverb".into(),
            },
        ];

        Self { effects, reference_frame: None }
    }

    /// Merge discovered LADSPA plugins into the registry. One `EffectEntry`
    /// per plugin; grouping is the plugin maker so the browser buckets them
    /// sensibly. Discovery already de-dupes by unique_id.
    pub fn merge_ladspa_plugins(
        &mut self,
        plugins: &[klipp_audio::ladspa::LadspaPluginInfo],
    ) {
        for plugin in plugins {
            let name = if plugin.name.is_empty() {
                plugin.label.clone()
            } else {
                plugin.name.clone()
            };
            let grouping = if plugin.maker.is_empty() {
                "LADSPA".to_string()
            } else {
                plugin.maker.clone()
            };
            let description = if plugin.maker.is_empty() {
                format!("LADSPA plugin: {}", name)
            } else {
                format!("LADSPA plugin: {} by {}", name, plugin.maker)
            };
            self.effects.push(EffectEntry {
                category: ladspa_category_from_name(&name, &plugin.label),
                name,
                source: EffectSource::Ladspa(plugin.clone()),
                source_label: "LADSPA",
                description,
                grouping,
            });
        }
    }

    /// Merge discovered OFX plugins into the registry, skipping duplicates.
    /// OFX bundles often ship multiple versions of the same plugin — keep only the latest.
    pub fn merge_ofx_plugins(&mut self, plugins: &[klipp_plugin::OfxPluginInfo]) {
        let mut seen = std::collections::HashSet::new();
        for plugin in plugins {
            // Deduplicate by plugin identifier — keep the first (highest version)
            if !seen.insert(plugin.name.clone()) {
                continue;
            }
            let display_name = format_ofx_name(&plugin.name);
            let category = category_from_grouping(&plugin.grouping);

            self.effects.push(EffectEntry {
                name: display_name,
                category,
                source: EffectSource::Ofx(plugin.name.clone()),
                source_label: "OFX",
                description: plugin.description.clone(),
                grouping: plugin.grouping.clone(),
            });
        }
    }

    /// Set the reference frame from a full-resolution RGBA buffer.
    /// Downscales to thumbnail size (192x108) using nearest-neighbor.
    pub fn set_reference_frame(&mut self, rgba: &[u8], src_w: u32, src_h: u32) {
        const THUMB_W: u32 = 192;
        const THUMB_H: u32 = 108;

        if rgba.len() != (src_w * src_h * 4) as usize || src_w == 0 || src_h == 0 {
            return;
        }

        let mut thumb = vec![0u8; (THUMB_W * THUMB_H * 4) as usize];
        for ty in 0..THUMB_H {
            for tx in 0..THUMB_W {
                let sx = (tx as f32 / THUMB_W as f32 * src_w as f32) as u32;
                let sy = (ty as f32 / THUMB_H as f32 * src_h as f32) as u32;
                let si = ((sy * src_w + sx) * 4) as usize;
                let di = ((ty * THUMB_W + tx) * 4) as usize;
                thumb[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
            }
        }

        self.reference_frame = Some(ReferenceFrame {
            rgba: thumb,
            width: THUMB_W,
            height: THUMB_H,
        });
    }

    /// Filter effects by category and search query, sorted alphabetically.
    /// `favorite_names` backs the `EffectCategory::Favourites` pseudo-category
    /// — when the caller doesn't need it (sidebar count queries, most tests)
    /// an empty slice is fine.
    pub fn filtered(&self, category: EffectCategory, search: &str) -> Vec<&EffectEntry> {
        self.filtered_with_favorites(category, search, &[])
    }

    pub fn filtered_with_favorites(
        &self,
        category: EffectCategory,
        search: &str,
        favorite_names: &[String],
    ) -> Vec<&EffectEntry> {
        let search_lower = search.to_lowercase();
        let _is_audio_cat = matches!(
            category,
            EffectCategory::AudioAll
                | EffectCategory::AudioEQ
                | EffectCategory::AudioDynamics
                | EffectCategory::AudioReverb
        );
        let mut result: Vec<&EffectEntry> = self
            .effects
            .iter()
            .filter(|e| {
                let e_is_audio = matches!(
                    e.category,
                    EffectCategory::AudioEQ
                        | EffectCategory::AudioDynamics
                        | EffectCategory::AudioReverb
                ) || matches!(e.source, EffectSource::Ladspa(_));
                let cat_match = match category {
                    // Favourites: show every effect the user has starred,
                    // video or audio alike. Empty favorites list → empty
                    // grid (handled naturally by the contains() check).
                    EffectCategory::Favourites => favorite_names.contains(&e.name),
                    EffectCategory::All => !e_is_audio, // All = all video effects
                    EffectCategory::AudioAll => e_is_audio, // AudioAll = all audio effects
                    _ => e.category == category,
                };
                cat_match
                    && (search_lower.is_empty()
                        || e.name.to_lowercase().contains(&search_lower)
                        || e.grouping.to_lowercase().contains(&search_lower)
                        || e.description.to_lowercase().contains(&search_lower))
            })
            .collect();
        result.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        result
    }
}

/// Convert an OFX plugin identifier to a human-readable name.
/// "net.sf.openfx.GaussianBlur" → "Gaussian Blur"
/// "net.sf.cimg.CImgBloom" → "CImg Bloom"
fn format_ofx_name(identifier: &str) -> String {
    let last = identifier.rsplit_once('.').map(|(_, n)| n).unwrap_or(identifier);
    // Insert spaces before uppercase letters (CamelCase → Camel Case)
    let mut result = String::with_capacity(last.len() + 4);
    for (i, c) in last.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            // Don't insert space if previous char was also uppercase (e.g. "CImg")
            let prev = last.as_bytes()[i - 1] as char;
            if prev.is_lowercase() {
                result.push(' ');
            }
        }
        result.push(c);
    }
    result
}

/// Map an OFX grouping string to an EffectCategory.
fn category_from_grouping(grouping: &str) -> EffectCategory {
    let g = grouping.to_lowercase();
    if g.contains("blur") || g.contains("sharpen") || g.contains("soften") || g.contains("denoise") {
        EffectCategory::Blur
    } else if g.contains("color") || g.contains("colour") || g.contains("grade") || g.contains("correct")
        || g.contains("saturation") || g.contains("hue")
    {
        EffectCategory::Color
    } else if g.contains("key") {
        EffectCategory::Keying
    } else if g.contains("distort") || g.contains("warp") || g.contains("transform") || g.contains("lens") {
        EffectCategory::Distort
    } else if g.contains("light") || g.contains("glow") || g.contains("flare") || g.contains("bloom") {
        EffectCategory::Light
    } else {
        EffectCategory::Stylize
    }
}

/// Heuristic mapping from a LADSPA plugin's display name + label to an
/// audio category for the effects-tab category sidebar. LADSPA itself
/// carries no category metadata in the C ABI, so this is a substring
/// match against common plugin name conventions.
///
/// Order matters — checks are top-to-bottom, first match wins. The order
/// reflects how distinctive each substring is: "reverb" rarely appears in
/// non-reverb plugin names, but "filter" can appear in EQ-style or
/// dynamics-style plugins, so we check the more distinctive groups first.
///
/// Falls back to `AudioEQ` for unknown plugins. AudioEQ is a forgiving
/// default: it isn't claiming the plugin IS an EQ, it's saying "no clear
/// category match — show in the EQ sub-tab" (the AudioAll sub-tab still
/// shows everything).
fn ladspa_category_from_name(name: &str, label: &str) -> EffectCategory {
    let s = format!("{} {}", name.to_lowercase(), label.to_lowercase());

    // Reverb / spatial — very distinctive substrings, checked first.
    if s.contains("reverb")
        || s.contains("hall")
        || s.contains("plate")
        || s.contains("room")
        || s.contains("freeverb")
        || s.contains("gverb")
        || s.contains("plateverb")
    {
        return EffectCategory::AudioReverb;
    }

    // Delay / echo — sit under Reverb in the audio category sidebar
    // (the sub-tab is "Reverb" but covers reverb+delay collectively).
    if s.contains("delay")
        || s.contains("echo")
        || s.contains("tap_")
        || s.contains("flanger") // delay-based modulation
        || s.contains("chorus") // delay-based modulation
        || s.contains("phaser") // sometimes delay-based
        || s.contains("comb")
    {
        return EffectCategory::AudioReverb;
    }

    // Dynamics — compressor/limiter/gate/expander/maximiser families.
    if s.contains("compressor")
        || s.contains("compress")
        || s.contains("limiter")
        || s.contains("gate")
        || s.contains("expander")
        || s.contains("maximizer")
        || s.contains("maximiser")
        || s.contains("ducker")
        || s.contains("sidechain")
        || s.contains("dynamics")
        || s.contains("leveler")
        || s.contains("leveller")
    {
        return EffectCategory::AudioDynamics;
    }

    // EQ / filter family — equalisers, filters, shelves, parametric/graphic.
    if s.contains("eq ")
        || s.ends_with("eq")
        || s.starts_with("eq ")
        || s.contains("equali")
        || s.contains("equaliser")
        || s.contains("equalizer")
        || s.contains("filter")
        || s.contains("lowpass")
        || s.contains("highpass")
        || s.contains("bandpass")
        || s.contains("notch")
        || s.contains("shelf")
        || s.contains("low_shelf")
        || s.contains("high_shelf")
        || s.contains("allpass")
        || s.contains("biquad")
        || s.contains("formant")
        || s.contains("vocoder")
        || s.contains("crossover")
    {
        return EffectCategory::AudioEQ;
    }

    // Default: route unmatched plugins to AudioEQ. They still appear in
    // AudioAll; the only consequence is which sub-tab in the category
    // sidebar surfaces them. AudioEQ is the most forgiving default
    // because users browsing for a generic effect tend to start there.
    EffectCategory::AudioEQ
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal registry with three distinct entries so tests don't
    /// depend on the real (large, evolving) registry content.
    fn test_registry() -> EffectRegistry {
        EffectRegistry {
            effects: vec![
                EffectEntry {
                    name: "Blur".into(),
                    category: EffectCategory::Blur,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Blur".into(),
                },
                EffectEntry {
                    name: "Sharpen".into(),
                    category: EffectCategory::Blur,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Blur".into(),
                },
                EffectEntry {
                    name: "Opacity".into(),
                    category: EffectCategory::Color,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::Opacity { value: 1.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Color".into(),
                },
            ],
            reference_frame: None,
        }
    }

    #[test]
    fn favourites_filter_returns_only_starred() {
        let reg = test_registry();
        let favs = vec!["Blur".to_string()];
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "", &favs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Blur");
    }

    #[test]
    fn favourites_filter_with_search_narrows_further() {
        let reg = test_registry();
        let favs = vec!["Blur".to_string(), "Opacity".to_string()];
        // Both starred — search "opac" uniquely hits Opacity by name, so the
        // starred Blur entry is filtered out even though it's a favourite.
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "opac", &favs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Opacity");
    }

    #[test]
    fn favourites_filter_empty_when_no_stars() {
        let reg = test_registry();
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "", &[]);
        assert!(out.is_empty(), "no stars → empty grid");
    }

    #[test]
    fn star_toggle_adds_and_removes() {
        // Mirrors the `pending_fav_toggle` apply logic in effects_tab.rs.
        let mut favs: Vec<String> = Vec::new();
        let name = "Blur".to_string();

        // First toggle → add.
        if let Some(i) = favs.iter().position(|n| n == &name) {
            favs.remove(i);
        } else {
            favs.push(name.clone());
        }
        assert_eq!(favs, vec!["Blur".to_string()]);

        // Second toggle → remove.
        if let Some(i) = favs.iter().position(|n| n == &name) {
            favs.remove(i);
        } else {
            favs.push(name.clone());
        }
        assert!(favs.is_empty());
    }

    #[test]
    fn filtered_backwards_compat_no_favorites() {
        // The old `filtered()` helper (no favorites slice) must still return
        // non-favorite categories identically to the new variant.
        let reg = test_registry();
        let old = reg.filtered(EffectCategory::Blur, "");
        let new = reg.filtered_with_favorites(EffectCategory::Blur, "", &[]);
        let old_names: Vec<_> = old.iter().map(|e| e.name.as_str()).collect();
        let new_names: Vec<_> = new.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(old_names, new_names);
    }

    // ─── ladspa_category_from_name heuristic ─────────────────────────
    //
    // Locks down the substring matches that route LADSPA plugins into the
    // audio category sidebar. The plugin names below are real LADSPA
    // plugins from common bundles (CALF, SWH, TAP, LSP, ...). Any future
    // change to the heuristic that would re-route one of these wrongly
    // will fail loudly here.

    #[test]
    fn ladspa_category_routes_reverbs() {
        for name in [
            "Calf Reverb",
            "Plate Reverb",
            "Plate 2x2",
            "Plateverb",
            "Freeverb",
            "GVerb",
            "Hall Reverb",
            "Room Reverb",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioReverb,
                "expected {name:?} to be AudioReverb",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_delays_and_modulation_to_reverb_bucket() {
        // Delay + delay-based modulation effects sit under the Reverb sub-tab
        // because that's where Klipp's category sidebar groups time-domain FX.
        for name in [
            "Stereo Delay",
            "Tape Echo",
            "Tap Delay",
            "Calf Flanger",
            "Calf Chorus",
            "Phaser",
            "Comb Filter Delay",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioReverb,
                "expected {name:?} to be AudioReverb",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_dynamics() {
        for name in [
            "Calf Compressor",
            "Calf Sidechain Compressor",
            "Calf Limiter",
            "Calf Multiband Limiter",
            "Calf Sidechain Gate",
            "Expander",
            "Maximizer",
            "Loudness Maximiser",
            "Auto Leveler",
            "Stereo Ducker",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioDynamics,
                "expected {name:?} to be AudioDynamics",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_eqs_and_filters() {
        for name in [
            "Calf Equalizer 8 Band",
            "Calf Equaliser 12 Band",
            "Parametric EQ",
            "DJ EQ",
            "TAP Equalizer",
            "4-pole Lowpass Filter",
            "Highpass Filter",
            "Bandpass Filter",
            "Allpass Filter",
            "Notch Filter",
            "Low Shelf",
            "High Shelf",
            "Biquad",
            "Crossover",
            "Vocoder",
            "Formant Filter",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioEQ,
                "expected {name:?} to be AudioEQ",
            );
        }
    }

    #[test]
    fn ladspa_category_falls_back_for_unknown_names() {
        // No matching substring → AudioEQ default. AudioAll always shows
        // these too; the default just controls which sub-tab surfaces them.
        for name in ["Mystery Plugin", "Foo Bar", "Saturator", "Bitcrusher"] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioEQ,
                "expected {name:?} to fall back to AudioEQ",
            );
        }
    }

    #[test]
    fn ladspa_category_uses_label_when_name_is_empty() {
        // Some plugins don't fill the human-readable name field; the
        // heuristic should still match against the technical label.
        assert_eq!(
            ladspa_category_from_name("", "calf_reverb"),
            EffectCategory::AudioReverb,
        );
        assert_eq!(
            ladspa_category_from_name("", "plate_reverb_lr"),
            EffectCategory::AudioReverb,
        );
        assert_eq!(
            ladspa_category_from_name("", "compressor_st"),
            EffectCategory::AudioDynamics,
        );
    }
}
