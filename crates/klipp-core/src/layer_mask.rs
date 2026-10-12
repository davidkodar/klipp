//! How one title layer masks another (#1513): the track-matte model
//! inside a title. The masked layer names its source; the source's
//! alpha or its brightness is how much of the masked layer shows.

use serde::{Deserialize, Serialize};

/// What of the source layer is the mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerMaskKind {
    /// Shows where the source is drawn.
    #[default]
    Alpha,
    /// Shows where the source is not drawn.
    InvertedAlpha,
    /// Shows where the source is bright: its luminance, in linear
    /// light, over black.
    Luma,
    /// Shows where the source is dark or not drawn.
    InvertedLuma,
}

impl LayerMaskKind {
    pub const ALL: [LayerMaskKind; 4] = [LayerMaskKind::Alpha, LayerMaskKind::InvertedAlpha, LayerMaskKind::Luma, LayerMaskKind::InvertedLuma];

    pub fn display_name(self) -> &'static str {
        match self {
            LayerMaskKind::Alpha => "Alpha",
            LayerMaskKind::InvertedAlpha => "Alpha Inverted",
            LayerMaskKind::Luma => "Luma",
            LayerMaskKind::InvertedLuma => "Luma Inverted",
        }
    }

    /// The mask value, 0..1, of one source pixel: `luma` is its
    /// straight colour's luminance in linear light, `alpha` its alpha.
    pub fn value(self, luma: f32, alpha: f32) -> f32 {
        let v = match self {
            LayerMaskKind::Alpha | LayerMaskKind::InvertedAlpha => alpha,
            LayerMaskKind::Luma | LayerMaskKind::InvertedLuma => luma * alpha,
        }
        .clamp(0.0, 1.0);
        match self {
            LayerMaskKind::Alpha | LayerMaskKind::Luma => v,
            LayerMaskKind::InvertedAlpha | LayerMaskKind::InvertedLuma => 1.0 - v,
        }
    }
}
