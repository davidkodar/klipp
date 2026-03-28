use klipp_core::MediaId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipKind {
    /// A clip backed by a media file in the MediaPool.
    Media { media_id: MediaId },
    /// A synthetic clip that generates its own frames — no source file.
    Title(TitleData),
    /// A gap placeholder — no media, no rendering. Pure timing spacer.
    Gap,
}

impl ClipKind {
    pub fn is_gap(&self) -> bool {
        matches!(self, ClipKind::Gap)
    }
}

impl Default for ClipKind {
    fn default() -> Self {
        // Should never be used — ClipKind must always be set explicitly.
        // Default exists only for serde compatibility with old projects.
        ClipKind::Media {
            media_id: MediaId::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleData {
    pub text: String,
    /// Font size in points, default 72.0
    pub font_size: f32,
    /// Font family name, default "sans-serif"
    pub font_name: String,
    /// Font style (Regular, Bold, Italic, Bold Italic)
    #[serde(default)]
    pub font_style: FontStyle,
    /// Text colour RGBA
    pub color: [u8; 4],
    pub alignment: TextAlignment,
    /// Normalized 0.0–1.0 position
    pub position: TitlePosition,
    pub background: TitleBackground,
    pub shadow: Option<TitleShadow>,
    /// Outline/stroke around text (single legacy, kept for backward compat)
    #[serde(default)]
    pub outline: Option<TextOutline>,
    /// Multiple outline layers (rendered widest-first, back-to-front)
    #[serde(default)]
    pub outlines: Vec<TextOutline>,
    /// Outer glow effect
    #[serde(default)]
    pub glow: Option<TextGlow>,
    /// Gradient fill (overrides solid color when set)
    #[serde(default)]
    pub gradient: Option<TextGradient>,
    /// Inner shadow (engraved/inset look)
    #[serde(default)]
    pub inner_shadow: Option<InnerShadow>,
    /// Pattern fill (overrides solid color and gradient when set)
    #[serde(default)]
    pub pattern_fill: Option<TextPattern>,
    /// Additional pixels between characters (tracking), default 0.0
    #[serde(default)]
    pub tracking: f32,
    /// Line spacing multiplier, default 1.0 (normal)
    #[serde(default = "default_one")]
    pub line_spacing: f32,
    /// Write-on reveal: 0.0 = no text, 1.0 = all text visible
    #[serde(default = "default_one")]
    pub write_on: f32,
}

fn default_one() -> f32 { 1.0 }

impl Default for TitleData {
    fn default() -> Self {
        Self {
            text: String::from("Title"),
            font_size: 72.0,
            font_name: String::from("sans-serif"),
            font_style: FontStyle::Regular,
            color: [255, 255, 255, 255],
            alignment: TextAlignment::Center,
            position: TitlePosition { x: 0.5, y: 0.5 },
            background: TitleBackground::None,
            shadow: None,
            outline: None,
            outlines: Vec::new(),
            glow: None,
            gradient: None,
            inner_shadow: None,
            pattern_fill: None,
            tracking: 0.0,
            line_spacing: 1.0,
            write_on: 1.0,
        }
    }
}

/// Font style variant — maps to style names in font files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TitlePosition {
    /// 0.0 = left, 1.0 = right, 0.5 = center
    pub x: f32,
    /// 0.0 = top, 1.0 = bottom, 0.5 = center
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TitleBackground {
    None,
    Color {
        color: [u8; 4],
        padding: f32,
    },
    FullFrame {
        color: [u8; 4],
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TitleShadow {
    pub color: [u8; 4],
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TextOutline {
    pub color: [u8; 4],
    /// Width in pixels, typically 1-8
    pub width: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TextGlow {
    pub color: [u8; 4],
    /// Blur radius in pixels, typically 5-30
    pub radius: f32,
    /// Intensity 0.0-1.0, default 0.8
    pub intensity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextGradient {
    pub top_color: [u8; 4],
    pub bottom_color: [u8; 4],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct InnerShadow {
    pub color: [u8; 4],
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum TextPattern {
    HorizontalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
    VerticalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
    Checkerboard { color_a: [u8; 4], color_b: [u8; 4], cell_size: f32 },
    DiagonalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
}
