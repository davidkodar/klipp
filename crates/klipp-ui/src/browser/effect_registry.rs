//! Unified effect registry — merges built-in effects and OFX plugins into one browseable list.

use crate::browser::effects_tab::EffectCategory;

/// Whether an effect is built-in or from an OFX plugin.
#[derive(Debug, Clone)]
pub enum EffectSource {
    Builtin(klipp_effects::BuiltinEffect),
    Ofx(String), // plugin identifier
    Audio(String), // audio effect type_id (e.g. "eq", "compressor")
}

/// A single entry in the effects browser.
#[derive(Debug, Clone)]
pub struct EffectEntry {
    pub name: String,
    pub category: EffectCategory,
    pub source: EffectSource,
    pub source_label: &'static str,
    pub grouping: String,
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
                grouping: "Color".into(),
            },
            EffectEntry {
                name: "Gaussian Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 }),
                source_label: "Built-in",
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Sharpen".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 }),
                source_label: "Built-in",
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Crop".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Crop {
                    left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
                }),
                source_label: "Built-in",
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Transform".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Transform {
                    position_x: 0.0, position_y: 0.0,
                    scale_x: 1.0, scale_y: 1.0,
                    rotation: 0.0,
                    anchor_x: 0.5, anchor_y: 0.5,
                }),
                source_label: "Built-in",
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
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Glow".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Glow {
                    radius: 30.0, intensity: 0.4,
                }),
                source_label: "Built-in",
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
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Vignette".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Vignette {
                    amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5,
                }),
                source_label: "Built-in",
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Diffusion".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Diffusion {
                    amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2,
                }),
                source_label: "Built-in",
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
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Light Wrap".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LightWrap {
                    width: 15.0, intensity: 0.5, softness: 1.0,
                }),
                source_label: "Built-in",
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Anamorphic Streak".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::AnamorphicStreak {
                    threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3,
                }),
                source_label: "Built-in",
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Film Halation".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::FilmHalation {
                    threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2,
                }),
                source_label: "Built-in",
                grouping: "Light".into(),
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
                grouping: "Stylize".into(),
            },
            // ── Audio effects ──
            EffectEntry {
                name: "Parametric EQ".into(),
                category: EffectCategory::AudioEQ,
                source: EffectSource::Audio("eq".into()),
                source_label: "Built-in",
                grouping: "EQ".into(),
            },
            EffectEntry {
                name: "Pitch Shift".into(),
                category: EffectCategory::AudioEQ,
                source: EffectSource::Audio("pitch_shift".into()),
                source_label: "Built-in",
                grouping: "EQ".into(),
            },
            EffectEntry {
                name: "Compressor".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("compressor".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Limiter".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("limiter".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Noise Gate".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("gate".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "De-Esser".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("deesser".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Normalize".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("normalize".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Noise Reduction".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("noise_reduction".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "De-click".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("declick".into()),
                source_label: "Built-in",
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Reverb".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("reverb".into()),
                source_label: "Built-in",
                grouping: "Reverb".into(),
            },
            EffectEntry {
                name: "Delay".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("delay".into()),
                source_label: "Built-in",
                grouping: "Reverb".into(),
            },
            EffectEntry {
                name: "Chorus".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("chorus".into()),
                source_label: "Built-in",
                grouping: "Reverb".into(),
            },
        ];

        Self { effects, reference_frame: None }
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
    pub fn filtered(&self, category: EffectCategory, search: &str) -> Vec<&EffectEntry> {
        let search_lower = search.to_lowercase();
        let _is_audio_cat = matches!(category, EffectCategory::AudioAll | EffectCategory::AudioEQ | EffectCategory::AudioDynamics | EffectCategory::AudioReverb);
        let mut result: Vec<&EffectEntry> = self.effects
            .iter()
            .filter(|e| {
                let e_is_audio = matches!(e.category, EffectCategory::AudioEQ | EffectCategory::AudioDynamics | EffectCategory::AudioReverb);
                let cat_match = match category {
                    EffectCategory::All => !e_is_audio, // All = all video effects
                    EffectCategory::AudioAll => e_is_audio, // AudioAll = all audio effects
                    _ => e.category == category,
                };
                cat_match
                    && (search_lower.is_empty()
                        || e.name.to_lowercase().contains(&search_lower)
                        || e.grouping.to_lowercase().contains(&search_lower))
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
