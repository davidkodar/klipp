pub mod blur;
mod chroma_key;
mod crop;
pub mod gpu;
mod light;
pub mod nv12_gpu;
mod opacity;
mod sharpen;
pub mod transform;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuiltinEffect {
    Crop {
        /// Fraction of width to crop from the left (0.0–1.0)
        left: f32,
        /// Fraction of width to crop from the right (0.0–1.0)
        right: f32,
        /// Fraction of height to crop from the top (0.0–1.0)
        top: f32,
        /// Fraction of height to crop from the bottom (0.0–1.0)
        bottom: f32,
        /// Feather in pixels (0.0 = hard edge)
        feather: f32,
    },
    Transform {
        /// Horizontal offset in pixels from center
        position_x: f32,
        /// Vertical offset in pixels from center
        position_y: f32,
        /// Horizontal scale (1.0 = 100%)
        scale_x: f32,
        /// Vertical scale (1.0 = 100%)
        scale_y: f32,
        /// Rotation in degrees
        rotation: f32,
        /// Anchor X (0.0–1.0, default 0.5 = center)
        anchor_x: f32,
        /// Anchor Y (0.0–1.0, default 0.5 = center)
        anchor_y: f32,
    },
    Opacity {
        /// Opacity value (0.0–1.0)
        value: f32,
    },
    GaussianBlur {
        /// Blur radius in pixels
        radius: f32,
    },
    Sharpen {
        /// Sharpen amount (0.0–2.0)
        amount: f32,
    },
    ChromaKey {
        /// RGB of the key colour
        key_color: [u8; 3],
        /// Tolerance (0.0–1.0)
        tolerance: f32,
        /// Spill suppression (0.0–1.0)
        spill_suppression: f32,
    },
    /// Bloom: extract bright areas, blur, and add back for a cinematic glow.
    Bloom {
        /// Luminance threshold for bright areas (0.0–1.0, default 0.7)
        threshold: f32,
        /// Blur radius for the bloom (pixels, default 20.0)
        radius: f32,
        /// Intensity of the bloom overlay (0.0–2.0, default 0.5)
        intensity: f32,
    },
    /// Soft glow: diffuse light wrap around bright areas.
    Glow {
        /// Blur radius for the glow (pixels, default 30.0)
        radius: f32,
        /// Glow intensity (0.0–2.0, default 0.4)
        intensity: f32,
    },
    /// God rays: volumetric light rays emanating from a point.
    LightRays {
        /// X position of the light source (0.0–1.0, default 0.5 = center)
        center_x: f32,
        /// Y position of the light source (0.0–1.0, default 0.3 = upper third)
        center_y: f32,
        /// Luminance threshold for ray sources (0.0–1.0, default 0.6)
        threshold: f32,
        /// Number of radial blur samples (4–128, default 64)
        samples: u32,
        /// How far rays extend (0.0–1.0, default 0.3)
        decay: f32,
        /// Ray intensity (0.0–2.0, default 0.5)
        intensity: f32,
    },
    /// Vignette: darken or lighten edges/corners of the frame.
    Vignette {
        /// How much to darken/lighten (-1.0=full black, 0.0=none, 1.0=full white, default -0.4)
        amount: f32,
        /// Size of the vignette center (0.0–2.0, default 0.8). Larger = more visible center.
        size: f32,
        /// Softness of the vignette falloff (0.0–2.0, default 0.5)
        softness: f32,
        /// Roundness: 0.0=rectangular, 1.0=circular (default 0.5)
        roundness: f32,
    },
    /// Diffusion / Pro-Mist: softens highlights for a dreamy cinematic look.
    Diffusion {
        /// How much bright areas bleed into surroundings (0.0–1.0, default 0.3)
        amount: f32,
        /// Blur radius for the diffusion (pixels, default 25.0)
        radius: f32,
        /// Luminance threshold — only soften above this (0.0–1.0, default 0.5)
        threshold: f32,
        /// Saturation reduction in the diffused areas (0.0–1.0, default 0.2)
        desaturation: f32,
    },
    /// Lens Flare: simulated lens flare with configurable position and style.
    LensFlare {
        /// X position of the flare source (0.0–1.0, default 0.7)
        position_x: f32,
        /// Y position of the flare source (0.0–1.0, default 0.3)
        position_y: f32,
        /// Overall brightness (0.0–2.0, default 0.6)
        brightness: f32,
        /// Size of the main flare disc (0.0–1.0, default 0.15)
        flare_size: f32,
        /// Number of ghost elements (0–8, default 4)
        ghost_count: u32,
        /// Intensity of ghost elements (0.0–1.0, default 0.3)
        ghost_intensity: f32,
        /// Chromatic fringing on ghosts (0.0–1.0, default 0.4)
        chromatic_aberration: f32,
    },
    /// Light Wrap: bleed background light onto foreground edges (compositing essential).
    LightWrap {
        /// Width of the light wrap in pixels (0.0–100.0, default 15.0)
        width: f32,
        /// Intensity of the wrap (0.0–1.0, default 0.5)
        intensity: f32,
        /// Softness of the wrap falloff (0.0–2.0, default 1.0)
        softness: f32,
    },
    /// Anamorphic Streak: horizontal lens flare streak on bright areas.
    AnamorphicStreak {
        /// Luminance threshold for streak sources (0.0–1.0, default 0.7)
        threshold: f32,
        /// Horizontal length of the streak in pixels (0.0–200.0, default 80.0)
        length: f32,
        /// Intensity of the streak (0.0–2.0, default 0.5)
        intensity: f32,
        /// Tint color: 0.0=warm amber, 0.5=neutral, 1.0=cool blue (default 0.3)
        tint: f32,
    },
    /// Film Halation: vintage film look with red/orange bleed around bright highlights.
    FilmHalation {
        /// Luminance threshold for halation sources (0.0–1.0, default 0.75)
        threshold: f32,
        /// Blur radius for the halation (pixels, default 15.0)
        radius: f32,
        /// Intensity of the halation (0.0–1.0, default 0.3)
        intensity: f32,
        /// Hue of the halation: 0.0=deep red, 0.5=orange, 1.0=warm yellow (default 0.2)
        hue: f32,
    },
    /// Speed Ramp: GPU-accelerated retiming with frame blending.
    /// Changes playback speed with smooth interpolation between frames.
    SpeedRamp {
        /// Playback speed multiplier (0.1=10% slow-mo, 1.0=normal, 4.0=4x fast)
        speed: f32,
        /// Blend mode: 0=nearest (no blend), 1=linear (smooth), 2=optical flow (best quality)
        blend_mode: u32,
        /// Ramp curve: 0=constant, 1=ease-in, 2=ease-out, 3=ease-in-out, 4=s-curve
        ramp_curve: u32,
        /// Ramp start position (0.0–1.0 of clip duration, only used with ramp curves)
        ramp_start: f32,
        /// Ramp end position (0.0–1.0 of clip duration)
        ramp_end: f32,
        /// Reverse playback
        reverse: bool,
        /// Freeze frame at this position (0.0–1.0, only active when freeze=true)
        freeze_at: f32,
        /// Enable freeze frame mode
        freeze: bool,
    },
}

/// Apply a chain of built-in effects to an RGBA frame.
pub fn apply_effects(effects: &[BuiltinEffect], mut rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
    for effect in effects {
        rgba = apply_one(effect, rgba, width, height);
    }
    rgba
}

fn apply_one(effect: &BuiltinEffect, rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
    match effect {
        BuiltinEffect::Opacity { value } => opacity::apply(rgba, *value),
        BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            crop::apply(rgba, width, height, *left, *right, *top, *bottom, *feather)
        }
        BuiltinEffect::GaussianBlur { radius } => blur::apply(rgba, width, height, *radius),
        BuiltinEffect::Sharpen { amount } => sharpen::apply(rgba, width, height, *amount),
        BuiltinEffect::Transform {
            position_x, position_y, scale_x, scale_y,
            rotation, anchor_x, anchor_y,
        } => {
            transform::apply(
                rgba, width, height,
                *position_x, *position_y, *scale_x, *scale_y,
                *rotation, *anchor_x, *anchor_y,
            )
        }
        BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            chroma_key::apply(rgba, *key_color, *tolerance, *spill_suppression)
        }
        BuiltinEffect::Bloom { threshold, radius, intensity } => {
            light::apply_bloom(rgba, width, height, *threshold, *radius, *intensity)
        }
        BuiltinEffect::Glow { radius, intensity } => {
            light::apply_glow(rgba, width, height, *radius, *intensity)
        }
        BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            light::apply_light_rays(rgba, width, height, *center_x, *center_y, *threshold, *samples, *decay, *intensity)
        }
        BuiltinEffect::Vignette { amount, size, softness, roundness } => {
            light::apply_vignette(rgba, width, height, *amount, *size, *softness, *roundness)
        }
        BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
            light::apply_diffusion(rgba, width, height, *amount, *radius, *threshold, *desaturation)
        }
        BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
            light::apply_lens_flare(rgba, width, height, *position_x, *position_y, *brightness, *flare_size, *ghost_count, *ghost_intensity, *chromatic_aberration)
        }
        BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
            light::apply_light_wrap(rgba, width, height, *wrap_w, *intensity, *softness)
        }
        BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
            light::apply_anamorphic_streak(rgba, width, height, *threshold, *length, *intensity, *tint)
        }
        BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
            light::apply_film_halation(rgba, width, height, *threshold, *radius, *intensity, *hue)
        }
        BuiltinEffect::SpeedRamp { .. } => {
            // SpeedRamp is handled at the engine level (frame queue + GPU blend),
            // not in the per-frame effects chain. If it reaches here, pass through.
            rgba
        }
    }
}
