//! OFX string constants — match the C `#define` values exactly.
//!
//! Using constants prevents typos in string literals (e.g. "OfxImageFieldNone"
//! vs the correct "OfxFieldNone") which cause silent plugin failures.

#![allow(non_upper_case_globals)]

// Field order
pub const kOfxFieldNone: &str = "OfxFieldNone";
pub const kOfxFieldBoth: &str = "OfxFieldBoth";
pub const kOfxFieldLower: &str = "OfxFieldLower";
pub const kOfxFieldUpper: &str = "OfxFieldUpper";

// Pixel depth
pub const kOfxBitDepthByte: &str = "OfxBitDepthByte";
pub const kOfxBitDepthShort: &str = "OfxBitDepthShort";
pub const kOfxBitDepthFloat: &str = "OfxBitDepthFloat";

// Pixel components
pub const kOfxImageComponentRGBA: &str = "OfxImageComponentRGBA";
pub const kOfxImageComponentRGB: &str = "OfxImageComponentRGB";
pub const kOfxImageComponentAlpha: &str = "OfxImageComponentAlpha";

// Premultiplication (standard OFX)
pub const kOfxImageUnPreMultiplied: &str = "OfxImageUnPreMultiplied";
pub const kOfxImagePreMultiplied: &str = "OfxImagePreMultiplied";

// Premultiplication (Natron variant — used by openfx-misc/CImg from Natron builds)
pub const kOfxImageAlphaUnPremultiplied: &str = "OfxImageAlphaUnPremultiplied";
pub const kOfxImageAlphaPremultiplied: &str = "OfxImageAlphaPremultiplied";
pub const kOfxImageOpaque: &str = "OfxImageOpaque";

// Context
pub const kOfxImageEffectContextFilter: &str = "OfxImageEffectContextFilter";
