mod blur;
mod chroma_key;
mod crop;
pub mod gpu;
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
    }
}
