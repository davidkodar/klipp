use klipp_core::MediaId;
use serde::{Deserialize, Serialize};

/// A style run — a contiguous range of characters sharing the same visual style.
/// Fields are all `Option<T>`: `None` means "inherit from the base TitleData style".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleRun {
    /// Byte offset start in the text string (inclusive).
    pub start: usize,
    /// Byte offset end in the text string (exclusive).
    pub end: usize,
    /// Font family name override (if None, inherits from TitleData.font_name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_name: Option<String>,
    /// Font size override (if None, inherits from TitleData.font_size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// Color override RGBA (if None, inherits from TitleData.color).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 4]>,
    /// Font style override (if None, inherits from TitleData.font_style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_style: Option<FontStyle>,
    /// Extra tracking (letter spacing) in pixels (if None, inherits from TitleData.tracking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<f32>,
    /// Vertical baseline offset in pixels — positive = up (if None, defaults to 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_offset: Option<f32>,
}

impl StyleRun {
    /// Create a default (all-inherit) style run for a byte range.
    pub fn default_for_range(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            font_name: None,
            font_size: None,
            color: None,
            font_style: None,
            tracking: None,
            baseline_offset: None,
        }
    }

    /// Returns true if all fields are None (fully inheriting from base style).
    pub fn is_all_inherited(&self) -> bool {
        self.font_name.is_none()
            && self.font_size.is_none()
            && self.color.is_none()
            && self.font_style.is_none()
            && self.tracking.is_none()
            && self.baseline_offset.is_none()
    }

    /// Returns true if two runs have identical style fields (ignoring range).
    pub fn same_style(&self, other: &StyleRun) -> bool {
        self.font_name == other.font_name
            && self.font_size == other.font_size
            && self.color == other.color
            && self.font_style == other.font_style
            && self.tracking == other.tracking
            && self.baseline_offset == other.baseline_offset
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipKind {
    /// A clip backed by a media file in the MediaPool.
    Media { media_id: MediaId },
    /// A synthetic clip that generates its own frames — no source file.
    Title(TitleData),
    /// A gap placeholder — no media, no rendering. Pure timing spacer.
    Gap,
    /// A nested sequence (compound clip) — references another sequence by ID.
    Sequence { sequence_id: klipp_core::SequenceId },
    /// A procedurally-generated frame source (solid color, gradient, bars, countdown).
    Generator(GeneratorData),
    /// Adjustment layer — transparent clip that applies its effects to all clips below.
    AdjustmentLayer,
}

// GeneratorData is defined in klipp-core and re-exported here for convenience.
pub use klipp_core::GeneratorData;

impl ClipKind {
    pub fn is_gap(&self) -> bool {
        matches!(self, ClipKind::Gap)
    }

    /// Returns true if the playback engine should produce frames for this clip kind.
    /// Non-playable kinds are structural: Gap produces no frames, AdjustmentLayer
    /// is a layer effect applied to other clips.
    pub fn is_playable(&self) -> bool {
        match self {
            ClipKind::Media { .. }
            | ClipKind::Title(_)
            | ClipKind::Generator(_)
            | ClipKind::Sequence { .. } => true,
            ClipKind::Gap | ClipKind::AdjustmentLayer => false,
        }
    }

    /// Returns true if this clip kind needs a decoder to produce frames.
    /// Media clips and nested sequences go through decoders. Synthetic clips
    /// (Title, Generator) produce frames directly without one.
    pub fn needs_decoder(&self) -> bool {
        match self {
            ClipKind::Media { .. } | ClipKind::Sequence { .. } => true,
            ClipKind::Title(_) | ClipKind::Generator(_) => false,
            ClipKind::Gap | ClipKind::AdjustmentLayer => false,
        }
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

/// A title saved as a reusable project asset. Lives in the project's
/// `title_assets: HashMap<MediaId, TitleAsset>` store, keyed by the
/// `MediaId` of its synthetic `MediaItem`. Editing an asset
/// propagates to every linked clip on every sequence at render time.
///
/// `display_name` is the user-facing label shown in the Media tab and
/// in the Inspector's "Linked to" banner — initially derived from the
/// title's text content but renameable so users can group brand
/// assets ("Brand Lower Third (Red)" vs the title's actual visible
/// "Jane Doe — CEO" text).
///
/// `data` is the canonical `TitleData` source-of-truth; linked clips
/// fall back to their inline `ClipKind::Title(TitleData)` only when
/// the asset can't be resolved (loading a project where the asset
/// was deleted out from under live instances — the inline copy is
/// the safety net).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleAsset {
    /// User-editable name. Renaming doesn't break links — the
    /// `MediaId` is the stable identifier.
    pub display_name: String,
    /// Canonical title content, styling, and animation parameters.
    pub data: TitleData,
    /// Animation token string from the original Library template
    /// (e.g. `"fade_in_0.5+slide_up_0.3"`). Stored on the asset so
    /// every linked instance gets the same intro animation; clips
    /// can override per-instance via Inspector if Detach hasn't been
    /// triggered.
    #[serde(default)]
    pub animation: Option<String>,
}

/// Stable identifier for a `TitleObject` within its parent `TitleData`.
/// Used by selection state (which object is active for editing) and
/// command dispatch (which object to move / resize / restyle). Plain
/// `u32` keyed via a per-`TitleData` counter rather than a UUID — the
/// scope is tiny (typically 1-5 objects per title), and integer keys
/// are cheap to compare and serialise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TitleObjectId(pub u32);

/// Position + size + rotation + anchor for a `TitleObject` on the
/// title canvas. All in normalised (0.0..1.0) coordinates, so a title
/// renders identically at 720p, 1080p, 4K, etc. The renderer maps
/// these to absolute pixels at the target resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectTransform {
    /// Normalised centre of the object's bounding box.
    pub center: TitlePosition,
    /// Per-axis position keyframes. When non-empty, the renderer
    /// evaluates these against the clip's local time and overrides
    /// `center.x` / `center.y` for that frame; otherwise the static
    /// `center` value is used. Empty by default — every existing
    /// project file deserialises unchanged via `#[serde(default)]`.
    /// Soft-disable behaviour: a layer toggled off keeps these vecs
    /// intact so re-enabling restores the animation.
    #[serde(default)]
    pub center_x_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub center_y_keyframes: Vec<klipp_core::Keyframe>,
    /// Normalised width / height of the bounding box (1.0 = full
    /// frame extent). `width = 1.0, height = 0.2` is a full-frame-
    /// width × 20%-frame-height bar — a typical lower-third bar.
    pub size: TitlePosition,
    /// Rotation in degrees, positive = clockwise.
    #[serde(default)]
    pub rotation: f32,
    /// Anchor point inside the bounding box (normalised 0..1 within
    /// the box). 0.5,0.5 = centre (default). Drives rotation pivot
    /// and resize behaviour.
    #[serde(default = "default_anchor_norm")]
    pub anchor_norm: TitlePosition,
}

impl ObjectTransform {
    /// Returns the effective `center` at `time_secs` — for each axis,
    /// the keyframe-evaluated value when the corresponding `*_keyframes`
    /// vec is non-empty, otherwise the static `center` field. Empty
    /// vecs short-circuit to the static value, so titles without
    /// position keyframes pay no per-frame sampling cost.
    pub fn evaluated_center(&self, time_secs: f64) -> TitlePosition {
        let x = if self.center_x_keyframes.is_empty() {
            self.center.x
        } else {
            klipp_core::keyframe::evaluate(
                &self.center_x_keyframes,
                time_secs,
                self.center.x,
            )
        };
        let y = if self.center_y_keyframes.is_empty() {
            self.center.y
        } else {
            klipp_core::keyframe::evaluate(
                &self.center_y_keyframes,
                time_secs,
                self.center.y,
            )
        };
        TitlePosition { x, y }
    }

    /// Mutable accessor for the `*_keyframes` vec of one axis. Routes
    /// through the enum so handlers stay axis-agnostic.
    pub fn position_keyframes_mut(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
    ) -> &mut Vec<klipp_core::Keyframe> {
        match axis {
            klipp_core::TitlePositionAxis::X => &mut self.center_x_keyframes,
            klipp_core::TitlePositionAxis::Y => &mut self.center_y_keyframes,
        }
    }

    /// Insert (or replace, within ±20ms) a keyframe at `time_secs`.
    /// Keeps the vec sorted by `time_secs` so the renderer's
    /// `evaluate(...)` walk stays correct.
    pub fn upsert_position_keyframe(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        time_secs: f64,
        value: f32,
    ) {
        let kfs = self.position_keyframes_mut(axis);
        kfs.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
        kfs.push(klipp_core::Keyframe {
            time_secs,
            value,
            interpolation: klipp_core::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        });
        kfs.sort_by(|a, b| {
            a.time_secs
                .partial_cmp(&b.time_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Remove keyframe(s) within ±20ms of `time_secs`. No-op if none
    /// match; matches the NLE-standard "click diamond again to delete"
    /// affordance.
    pub fn remove_position_keyframe_at(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        time_secs: f64,
    ) {
        self.position_keyframes_mut(axis)
            .retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
    }

    /// Move the keyframe at `index` to a new (time, value). Re-sorts
    /// after the move so the vec stays in time order. Returns true if
    /// the index was valid.
    pub fn move_position_keyframe(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        index: usize,
        new_time_secs: f64,
        new_value: f32,
    ) -> bool {
        let kfs = self.position_keyframes_mut(axis);
        let Some(kf) = kfs.get_mut(index) else {
            return false;
        };
        kf.time_secs = new_time_secs;
        kf.value = new_value;
        kfs.sort_by(|a, b| {
            a.time_secs
                .partial_cmp(&b.time_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        true
    }

    /// Set the interpolation mode of the keyframe at `index`. Returns
    /// true if the index was valid.
    pub fn set_position_keyframe_interpolation(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        index: usize,
        interpolation: klipp_core::Interpolation,
    ) -> bool {
        let kfs = self.position_keyframes_mut(axis);
        if let Some(kf) = kfs.get_mut(index) {
            kf.interpolation = interpolation;
            true
        } else {
            false
        }
    }
}

fn default_anchor_norm() -> TitlePosition {
    TitlePosition { x: 0.5, y: 0.5 }
}

/// One layer in a `TitleData`. Each object is independently selectable,
/// movable, resizable, and rotatable on the title canvas, with its own
/// style and content. Objects render in order — the first object is at
/// the back, the last at the front (Photoshop-style stacking).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleObject {
    pub id: TitleObjectId,
    /// Display name for the object list panel. Empty string falls
    /// back to the variant kind in the UI.
    #[serde(default)]
    pub name: String,
    pub transform: ObjectTransform,
    /// Hide the object in the renderer. Selection state still
    /// tracks it; toggling visibility via the eye icon doesn't
    /// disturb editing focus.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Lock the object — prevents on-canvas dragging/handle-resize
    /// without affecting Inspector property edits.
    #[serde(default)]
    pub locked: bool,
    /// Per-object opacity 0..1. Multiplies into the alpha channel
    /// of every pixel this object contributes during composition.
    /// Default 1.0 (fully opaque). Independent from clip-level
    /// `opacity_keyframes` — each layer in a multi-layer title can
    /// be individually faded.
    #[serde(default = "default_one_f32")]
    pub opacity: f32,
    pub content: TitleObjectContent,
}

fn default_one_f32() -> f32 { 1.0 }
fn default_one_f32_tb() -> f32 { 1.0 }

fn default_true() -> bool { true }

/// What kind of layer an object is. `TextBox` for typography,
/// `Shape` for primitives like the bar in a lower-third. New layer
/// kinds (image, mask, sub-title, etc.) extend this enum without
/// disturbing existing instances.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TitleObjectContent {
    TextBox(TextBox),
    Shape(Shape),
}

/// A text layer's content + style. All the rich-text styling fields
/// (fonts, colour, gradients, outlines, glow, extrusion, path,
/// per-character animation, motion blur, style runs) live here so
/// each text layer is independently stylable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextBox {
    pub text: String,
    /// Font size in points, default 72.0. The renderer scales by
    /// `frame_h / 1080` so this stays resolution-stable.
    pub font_size: f32,
    pub font_name: String,
    #[serde(default)]
    pub font_style: FontStyle,
    pub color: [u8; 4],
    pub alignment: TextAlignment,
    #[serde(default)]
    pub shadow: Option<TitleShadow>,
    /// Outline/stroke layers, rendered widest-first, back-to-front.
    #[serde(default)]
    pub outlines: Vec<TextOutline>,
    #[serde(default)]
    pub glow: Option<TextGlow>,
    /// Gradient fill (overrides solid colour when set).
    #[serde(default)]
    pub gradient: Option<TextGradient>,
    #[serde(default)]
    pub inner_shadow: Option<InnerShadow>,
    /// Pattern fill (overrides solid colour and gradient when set).
    #[serde(default)]
    pub pattern_fill: Option<TextPattern>,
    /// Soft-disable for `pattern_fill`. When `false` and `pattern_fill`
    /// is `Some(...)`, the renderer skips the pattern but the parameters
    /// are preserved. `pattern_fill` is an enum without a place to add
    /// a struct-level `enabled` flag, so the bit lives here on TextBox.
    #[serde(default = "default_true")]
    pub pattern_fill_enabled: bool,
    /// Extra pixels between characters (tracking).
    #[serde(default)]
    pub tracking: f32,
    /// Line spacing multiplier, default 1.0.
    #[serde(default = "default_one")]
    pub line_spacing: f32,
    /// Write-on reveal: 0.0 = no text, 1.0 = all visible. Animation
    /// overrides this per frame for Typewriter; otherwise 1.0.
    #[serde(default = "default_one")]
    pub write_on: f32,
    /// 3D extrusion (pseudo-depth).
    #[serde(default)]
    pub extrusion: Option<TextExtrusion>,
    /// Text follows a geometric path (arc / circle / wave).
    #[serde(default)]
    pub text_path: Option<TextPath>,
    /// Per-character entrance animation.
    #[serde(default)]
    pub char_animation: Option<CharAnimation>,
    /// Texture fill — file path or procedural preset, plus the
    /// transform / blend params (scale, offset, rotation, opacity,
    /// tint, tile mode). `None` = no texture fill configured.
    /// Soft-disable lives inside `TextureFill::enabled`.
    #[serde(default)]
    pub texture_fill: Option<TextureFill>,
    /// Motion blur settings (None = disabled).
    #[serde(default)]
    pub motion_blur: Option<MotionBlur>,
    /// Per-character style runs. Empty = uniform style.
    #[serde(default)]
    pub style_runs: Vec<StyleRun>,
    /// ASS/SSA style name for round-tripping imported .ass files.
    #[serde(default)]
    pub ass_style_name: Option<String>,
    /// Underline the entire text body (one full-width line below the
    /// baseline; thickness scales with the resolved font size).
    #[serde(default)]
    pub underline: bool,
    /// Strikethrough the entire text body (one full-width line at the
    /// x-height midpoint).
    #[serde(default)]
    pub strikethrough: bool,
    /// Case transform applied to `text` BEFORE layout — the resting
    /// `text` field is preserved verbatim so the user can flip the
    /// case treatment without re-typing.
    #[serde(default)]
    pub case_transform: TextCase,
    /// Vertical baseline shift in pixels at the 1080p reference
    /// resolution; positive shifts text DOWN (away from cap-height),
    /// negative shifts UP. Useful for fine-tuning lower-thirds against
    /// a bar background.
    #[serde(default)]
    pub baseline_shift: f32,
    /// Outline draw-on progress 0..1 — clips the rendered outline
    /// horizontally so it appears to "draw on" from the text's left
    /// edge. `1.0` (default) = fully drawn; `0.0` = no outline visible.
    /// Independent of `write_on` (which clips the whole text body).
    /// Driven by the OutlineDrawOn TitleAnimationType variant.
    #[serde(default = "default_one_f32_tb")]
    pub outline_write_on: f32,
    /// Chromatic aberration in 1080p reference pixels. `0.0` (default)
    /// = no effect. Positive values split the rendered fill into
    /// R/G/B colour channels and offset them: red shifted left by
    /// this amount, blue shifted right, green left in the middle.
    /// The classic VHS / glitch / synthwave fringing look.
    #[serde(default)]
    pub chromatic_aberration_px: f32,
    /// Glitch intensity 0..1 — slices the rendered text into
    /// horizontal stripes, each shifted by a random amount per
    /// frame. `0.0` (default) = no effect. Pattern is seeded by
    /// the current clip time so playback is reproducible.
    #[serde(default)]
    pub glitch_intensity: f32,
    /// Drop-cap scale factor for the first character of `text`.
    /// `0.0` (default) = no drop cap. `>= 1.0` = enlarge the first
    /// character by this factor; e.g. 3.0 = three-line drop cap (the
    /// classic graphic-design "drop initial"). Implementation is a
    /// per-character font-size override on the first glyph, layered
    /// on top of any user-authored `style_runs`. Body text wrap is
    /// NOT yet aware of the drop cap's bounds — the body flows on
    /// the same baseline; future work will lay out the body around
    /// the drop cap's box for true wrap-under behaviour.
    #[serde(default)]
    pub drop_cap_scale: f32,
    /// Number of columns to flow body text into. `1` (default) =
    /// single-column layout — current behaviour. `2+` = split the
    /// body into N approximately-equal chunks (on word boundaries)
    /// and lay them out side-by-side with a gap between columns.
    /// Style runs and drop caps are not yet column-aware; they
    /// silently fall back to single-column when columns ≥ 2.
    #[serde(default = "default_one_u8")]
    pub columns: u8,
    /// Stack characters vertically (CJK-style top-to-bottom) instead
    /// of the default horizontal flow. The character glyphs keep
    /// their natural horizontal orientation — they're just stacked
    /// one per line. `false` (default) = horizontal layout. True
    /// vertical typography (rotated CJK glyphs, kana orientation)
    /// is a follow-up that needs OpenType `vrt2` feature support
    /// in the rasteriser.
    #[serde(default)]
    pub vertical_layout: bool,
    /// Text direction. `Ltr` (default) = standard left-to-right.
    /// `Rtl` = right-to-left for Hebrew. Limited Arabic support —
    /// proper Arabic shaping (joining forms, lam-alif ligatures, …)
    /// requires harfbuzz integration tracked for a follow-up. RTL
    /// reverses the visual character order and right-aligns the
    /// text by default.
    #[serde(default)]
    pub text_direction: TextDirection,
}

impl TextBox {
    /// Mutable accessor for the effect-keyframe vec named by
    /// `param_key`. Routes by prefix: `"glow.*"` lands on the
    /// `TextGlow` keyframe vecs, returning `None` if no glow is
    /// configured. Add a new arm for each effect type as it gains
    /// keyframable params.
    pub fn effect_keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        // Note: check `inner_shadow.` before `shadow.` — the latter
        // would otherwise prefix-match the former.
        if param_key.starts_with("glow.") {
            self.glow.as_mut().and_then(|g| g.keyframes_mut_for(param_key))
        } else if param_key.starts_with("inner_shadow.") {
            self.inner_shadow.as_mut().and_then(|s| s.keyframes_mut_for(param_key))
        } else if param_key.starts_with("shadow.") {
            self.shadow.as_mut().and_then(|s| s.keyframes_mut_for(param_key))
        } else if param_key.starts_with("extrusion.") {
            self.extrusion.as_mut().and_then(|e| e.keyframes_mut_for(param_key))
        } else if param_key.starts_with("motion_blur.") {
            self.motion_blur.as_mut().and_then(|m| m.keyframes_mut_for(param_key))
        } else if param_key.starts_with("char_anim.") {
            self.char_animation.as_mut().and_then(|c| c.keyframes_mut_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_outline_param_key(param_key) {
            // Per-stack-element outline keyframes. Each index is its
            // own slot — out-of-range indices return None.
            match field {
                "width" => self.outlines.get_mut(idx).map(|o| &mut o.width_keyframes),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Read-only mirror of `effect_keyframes_mut_for`. Used by the
    /// inspector to build a `KeyframeState`.
    pub fn effect_keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        if param_key.starts_with("glow.") {
            self.glow.as_ref().and_then(|g| g.keyframes_for(param_key))
        } else if param_key.starts_with("inner_shadow.") {
            self.inner_shadow.as_ref().and_then(|s| s.keyframes_for(param_key))
        } else if param_key.starts_with("shadow.") {
            self.shadow.as_ref().and_then(|s| s.keyframes_for(param_key))
        } else if param_key.starts_with("extrusion.") {
            self.extrusion.as_ref().and_then(|e| e.keyframes_for(param_key))
        } else if param_key.starts_with("motion_blur.") {
            self.motion_blur.as_ref().and_then(|m| m.keyframes_for(param_key))
        } else if param_key.starts_with("char_anim.") {
            self.char_animation.as_ref().and_then(|c| c.keyframes_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_outline_param_key(param_key) {
            match field {
                "width" => self.outlines.get(idx).map(|o| o.width_keyframes.as_slice()),
                _ => None,
            }
        } else {
            None
        }
    }
}

fn default_one_u8() -> u8 { 1 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}

/// Pre-layout case transform applied to `TextBox.text` before glyphs
/// are produced. `SmallCaps` uppercases lower-case input but renders
/// the upper-cased glyphs at 80 % of the configured `font_size` so
/// they read like genuine smallcaps even on fonts that don't ship
/// dedicated smallcaps glyph variants.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum TextCase {
    #[default]
    None,
    Uppercase,
    Lowercase,
    SmallCaps,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            text: String::from("Title"),
            font_size: 72.0,
            font_name: String::from("sans-serif"),
            font_style: FontStyle::Regular,
            color: [255, 255, 255, 255],
            alignment: TextAlignment::Center,
            shadow: None,
            outlines: Vec::new(),
            glow: None,
            gradient: None,
            inner_shadow: None,
            pattern_fill: None,
            pattern_fill_enabled: true,
            tracking: 0.0,
            line_spacing: 1.0,
            write_on: 1.0,
            extrusion: None,
            text_path: None,
            char_animation: None,
            texture_fill: None,
            motion_blur: None,
            style_runs: Vec::new(),
            ass_style_name: None,
            underline: false,
            strikethrough: false,
            case_transform: TextCase::None,
            baseline_shift: 0.0,
            drop_cap_scale: 0.0,
            outline_write_on: 1.0,
            chromatic_aberration_px: 0.0,
            glitch_intensity: 0.0,
            columns: 1,
            vertical_layout: false,
            text_direction: TextDirection::Ltr,
        }
    }
}

/// Vector primitive layer. The simplest first cut — rectangles with
/// fill / stroke / corner radius — covers lower-third bars and
/// generic backdrop bars. New variants (ellipse, polygon, path) are
/// future additions to `ShapeKind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shape {
    pub kind: ShapeKind,
    pub fill: Option<[u8; 4]>,
    pub stroke: Option<ShapeStroke>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShapeKind {
    Rectangle {
        /// Corner radius in pixels at 1080p reference resolution.
        /// 0.0 = sharp corners.
        #[serde(default)]
        corner_radius: f32,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ShapeStroke {
    pub color: [u8; 4],
    /// Stroke width in pixels at 1080p reference resolution.
    pub width: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleData {
    /// Stacking-order list of layers (back → front). The first
    /// object renders at the back; the last on top. A new title
    /// starts with one default `TextBox` object.
    pub objects: Vec<TitleObject>,
    /// Monotonic counter for assigning unique `TitleObjectId`s as
    /// new objects are added. Never decreases — deletion leaves
    /// gaps in the id space, which is harmless.
    #[serde(default)]
    pub next_object_id: u32,
    /// Title-level motion animation (scroll/crawl/typewriter)
    /// applied to every object as a unit. Per-object animation
    /// is a future extension.
    #[serde(default)]
    pub animation_type: TitleAnimationType,
    /// Optional title-level background bar. Lower-third templates
    /// use this to paint a coloured strip behind the text. This
    /// stays as a clip-level field for commit 1; a future commit
    /// migrates lower-third templates to a `Shape` object instead,
    /// at which point this field can be retired.
    #[serde(default)]
    pub background: TitleBackground,
}

impl TitleData {
    /// Create a TitleData with one default-styled TextBox at frame
    /// centre — the canonical "fresh title" shape used by `Default`
    /// and most templates.
    pub fn single_text(text_box: TextBox) -> Self {
        let mut td = Self {
            objects: Vec::new(),
            next_object_id: 0,
            animation_type: TitleAnimationType::default(),
            background: TitleBackground::None,
        };
        td.add_object(TitleObjectContent::TextBox(text_box), default_text_transform());
        td
    }

    /// Push a new object onto the stack with a freshly-allocated id.
    /// Returns the assigned id so callers can immediately reference
    /// the new object (e.g. for selection).
    pub fn add_object(
        &mut self,
        content: TitleObjectContent,
        transform: ObjectTransform,
    ) -> TitleObjectId {
        let id = TitleObjectId(self.next_object_id);
        self.next_object_id = self.next_object_id.saturating_add(1);
        let name = match &content {
            TitleObjectContent::TextBox(_) => format!("Text {}", id.0 + 1),
            TitleObjectContent::Shape(_) => format!("Shape {}", id.0 + 1),
        };
        self.objects.push(TitleObject {
            id,
            name,
            transform,
            visible: true,
            locked: false,
            opacity: 1.0,
            content,
        });
        id
    }

    /// Look up an object by id. Returns `None` if the id was deleted
    /// or never existed in this title.
    pub fn object(&self, id: TitleObjectId) -> Option<&TitleObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn object_mut(&mut self, id: TitleObjectId) -> Option<&mut TitleObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// First TextBox object on the stack, or `None` if the title is
    /// shape-only. The "primary text" is the single-text-clip
    /// abstraction most legacy code reads through; new code that
    /// needs to operate on every text object should iterate
    /// `self.objects` directly.
    pub fn primary_text(&self) -> Option<&TextBox> {
        self.objects.iter().find_map(|o| match &o.content {
            TitleObjectContent::TextBox(tb) => Some(tb),
            _ => None,
        })
    }

    pub fn primary_text_mut(&mut self) -> Option<&mut TextBox> {
        self.objects.iter_mut().find_map(|o| match &mut o.content {
            TitleObjectContent::TextBox(tb) => Some(tb),
            _ => None,
        })
    }

    /// Convenience for tests / templates / single-object cases:
    /// returns a default TextBox if no text object exists. Avoids
    /// `unwrap()` everywhere reading typography from a title that's
    /// expected to have one.
    pub fn primary_text_or_default(&self) -> TextBox {
        self.primary_text().cloned().unwrap_or_default()
    }

    // ── Single-text-object convenience accessors ──
    //
    // The old flat `TitleData` shape had typography fields directly
    // on the struct; lots of call sites (renderer, inspector, viewer
    // text edit, etc.) read `td.text` / `td.font_size` / `td.color`
    // etc. These accessors keep those reads working as method calls
    // (`td.text()`) so the migration to the object model is a
    // mechanical add-`()` change at each call site. Mutation goes
    // through `primary_text_mut()` (or, for full multi-object
    // editing, direct iteration over `self.objects`).
    pub fn text(&self) -> String {
        self.primary_text().map(|tb| tb.text.clone()).unwrap_or_default()
    }
    pub fn font_size(&self) -> f32 {
        self.primary_text().map_or(72.0, |tb| tb.font_size)
    }
    pub fn font_name(&self) -> String {
        self.primary_text()
            .map(|tb| tb.font_name.clone())
            .unwrap_or_else(|| "sans-serif".into())
    }
    pub fn font_style(&self) -> FontStyle {
        self.primary_text().map_or(FontStyle::Regular, |tb| tb.font_style)
    }
    pub fn color(&self) -> [u8; 4] {
        self.primary_text().map_or([255, 255, 255, 255], |tb| tb.color)
    }
    pub fn alignment(&self) -> TextAlignment {
        self.primary_text().map_or(TextAlignment::Center, |tb| tb.alignment)
    }
    /// Position of the primary text object on the title canvas.
    /// Sourced from the object's transform.center, falling back to
    /// frame centre.
    pub fn position(&self) -> TitlePosition {
        self.objects
            .iter()
            .find_map(|o| match o.content {
                TitleObjectContent::TextBox(_) => Some(o.transform.center),
                _ => None,
            })
            .unwrap_or(TitlePosition { x: 0.5, y: 0.5 })
    }
    pub fn shadow(&self) -> Option<TitleShadow> {
        self.primary_text().and_then(|tb| tb.shadow.clone())
    }
    pub fn outlines(&self) -> Vec<TextOutline> {
        self.primary_text().map(|tb| tb.outlines.clone()).unwrap_or_default()
    }
    pub fn glow(&self) -> Option<TextGlow> {
        self.primary_text().and_then(|tb| tb.glow.clone())
    }
    pub fn gradient(&self) -> Option<TextGradient> {
        self.primary_text().and_then(|tb| tb.gradient.clone())
    }
    pub fn inner_shadow(&self) -> Option<InnerShadow> {
        self.primary_text().and_then(|tb| tb.inner_shadow.clone())
    }
    pub fn pattern_fill(&self) -> Option<TextPattern> {
        self.primary_text().and_then(|tb| tb.pattern_fill)
    }
    pub fn tracking(&self) -> f32 {
        self.primary_text().map_or(0.0, |tb| tb.tracking)
    }
    pub fn line_spacing(&self) -> f32 {
        self.primary_text().map_or(1.0, |tb| tb.line_spacing)
    }
    pub fn write_on(&self) -> f32 {
        self.primary_text().map_or(1.0, |tb| tb.write_on)
    }
    pub fn extrusion(&self) -> Option<TextExtrusion> {
        self.primary_text().and_then(|tb| tb.extrusion.clone())
    }
    pub fn text_path(&self) -> Option<TextPath> {
        self.primary_text().and_then(|tb| tb.text_path.clone())
    }
    pub fn char_animation(&self) -> Option<CharAnimation> {
        self.primary_text().and_then(|tb| tb.char_animation.clone())
    }
    pub fn texture_fill_path(&self) -> Option<String> {
        self.primary_text()
            .and_then(|tb| tb.texture_fill.as_ref())
            .and_then(|t| t.file_path().map(|s| s.to_string()))
    }
    pub fn motion_blur(&self) -> Option<MotionBlur> {
        self.primary_text().and_then(|tb| tb.motion_blur.clone())
    }
    pub fn underline(&self) -> bool {
        self.primary_text().map_or(false, |tb| tb.underline)
    }
    pub fn strikethrough(&self) -> bool {
        self.primary_text().map_or(false, |tb| tb.strikethrough)
    }
    pub fn case_transform(&self) -> TextCase {
        self.primary_text().map_or(TextCase::None, |tb| tb.case_transform)
    }
    pub fn baseline_shift(&self) -> f32 {
        self.primary_text().map_or(0.0, |tb| tb.baseline_shift)
    }
    pub fn drop_cap_scale(&self) -> f32 {
        self.primary_text().map_or(0.0, |tb| tb.drop_cap_scale)
    }
    pub fn style_runs(&self) -> Vec<StyleRun> {
        self.primary_text().map(|tb| tb.style_runs.clone()).unwrap_or_default()
    }
    pub fn ass_style_name(&self) -> Option<String> {
        self.primary_text().and_then(|tb| tb.ass_style_name.clone())
    }
}

/// Default ObjectTransform for a fresh full-frame text object —
/// centred at (0.5, 0.5), sized 1.0×1.0, no rotation, anchor at
/// box centre. Templates that need a smaller/positioned object
/// override the relevant fields.
pub fn default_text_transform() -> ObjectTransform {
    ObjectTransform {
        center: TitlePosition { x: 0.5, y: 0.5 },
        center_x_keyframes: Vec::new(),
        center_y_keyframes: Vec::new(),
        size: TitlePosition { x: 1.0, y: 1.0 },
        rotation: 0.0,
        anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
    }
}

/// Title motion preset — drives keyframe generation (scroll/crawl) or
/// per-frame `write_on` override (typewriter). Static = no animation.
///
/// Variant authoring conventions:
/// - "Continuous" motion (Scroll/Crawl/Pulse/Flash/Blink) animate the
///   entire clip duration; the parameter (speed / rate) drives the
///   period of motion.
/// - "One-shot enter" animations (FadeIn/SlideInX/ZoomIn) play once
///   over `duration_secs` from the start of the clip and hold the
///   final pose for the rest. Their counterparts FadeOut/ZoomOut
///   play once over `duration_secs` ending at the clip's end.
/// - FadeInOut combines the two into a single `fade_secs` envelope.
/// - All variants compose with the textbox's `transform.center` so
///   the user can drag the textbox while keeping the animation; the
///   keyframes write to clip-level opacity / scale / pos channels
///   that translate the entire title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum TitleAnimationType {
    #[default]
    Static,
    /// Scrolls bottom-to-top (credits roll). Speed drives keyframe
    /// generation but the actual travel is clip-duration-bounded.
    ScrollUp { speed_px_per_sec: f32 },
    /// Scrolls top-to-bottom.
    ScrollDown { speed_px_per_sec: f32 },
    /// Crawls right-to-left (news ticker).
    CrawlLeft { speed_px_per_sec: f32 },
    /// Crawls left-to-right.
    CrawlRight { speed_px_per_sec: f32 },
    /// Per-character reveal (typewriter effect).
    Typewriter { chars_per_sec: f32 },
    /// Opacity ramps 0 → 1 over `duration_secs` at clip start, then
    /// holds at 1 for the rest of the clip.
    FadeIn { duration_secs: f32 },
    /// Opacity ramps 1 → 0 over `duration_secs` ending at clip end;
    /// holds at 1 before that.
    FadeOut { duration_secs: f32 },
    /// Combined: fade in at start, hold full opacity, fade out at end.
    /// Both ramps are `fade_secs` long.
    FadeInOut { fade_secs: f32 },
    /// Square-wave opacity at `rate_hz` (50% duty cycle on/off). Use
    /// for "ALERT" / "WARNING" style flashing titles.
    Flash { rate_hz: f32 },
    /// Like Flash but with an authored `duty_cycle` (fraction of the
    /// period that the title is visible — 0.1 = brief blip, 0.9 =
    /// brief gap). 0.5 is identical to Flash.
    Blink { rate_hz: f32, duty_cycle: f32 },
    /// Smooth sinusoidal opacity oscillation between `min_opacity` and
    /// 1.0 at `rate_hz`. Common for "breathing" emphasis.
    Pulse { rate_hz: f32, min_opacity: f32 },
    /// Scale ramps from `from_scale` → 1.0 over `duration_secs` at
    /// clip start. `from_scale > 1.0` zooms IN from a larger size;
    /// `from_scale < 1.0` zooms in from smaller.
    ZoomIn { duration_secs: f32, from_scale: f32 },
    /// Scale ramps 1.0 → `to_scale` over `duration_secs` ending at
    /// clip end. Symmetric counterpart to ZoomIn.
    ZoomOut { duration_secs: f32, to_scale: f32 },
    /// Title slides in from off-screen-left to its anchor over
    /// `duration_secs`. Holds in place for the rest of the clip.
    SlideInLeft { duration_secs: f32 },
    /// Slides in from off-screen-right.
    SlideInRight { duration_secs: f32 },
    /// Slides in from off-screen-top.
    SlideInTop { duration_secs: f32 },
    /// Slides in from off-screen-bottom.
    SlideInBottom { duration_secs: f32 },
    /// Rotates from `from_degrees` → 0 over `duration_secs` while
    /// holding the textbox at its anchor. Negative angles spin
    /// clockwise; positive counter-clockwise.
    SpinIn { duration_secs: f32, from_degrees: f32 },
    /// Scale-based overshoot entrance: 0 → ~1.15× → 1.0 over
    /// `duration_secs`, giving a tactile "pop" before settling.
    BounceIn { duration_secs: f32 },
    /// Continuous vertical sine wave across the whole clip;
    /// `amplitude_px` is the peak displacement in 1080p reference
    /// pixels, `rate_hz` the cycle frequency.
    Wave { amplitude_px: f32, rate_hz: f32 },
    /// Continuous random jitter on both axes — emphasis for
    /// "ALERT" / "EARTHQUAKE" style titles. `amplitude_px` is the
    /// max displacement; `rate_hz` controls how rapidly the jitter
    /// updates (higher = more frenetic).
    Shake { amplitude_px: f32, rate_hz: f32 },
    /// Continuous hue rotation of the text fill colour. `rate_hz`
    /// is full-revolution-per-second (1.0 = one complete spin
    /// through the colour wheel each second). Saturation and value
    /// are taken from the resting fill colour. Primary use cases
    /// are music-video titles, party invites, kids' content.
    ColorCycle { rate_hz: f32 },
    /// Continuous looping horizontal scroll — the title travels
    /// across the frame and instantly re-enters from the opposite
    /// edge, repeating for the entire clip duration. Distinct from
    /// CrawlLeft / CrawlRight, which are one-shot (text exits the
    /// frame and is gone). `speed_px_per_sec` drives travel speed
    /// in 1080p reference pixels; positive = right→left (newsroom
    /// ticker direction), negative = left→right.
    Marquee { speed_px_per_sec: f32 },
    /// Inverse of `Typewriter`: text starts fully visible and
    /// deletes one character at a time from the right at the given
    /// rate. Drives `write_on` from 1.0 → 0.0 over the clip.
    TypewriterOut { chars_per_sec: f32 },
    /// Animated number ticker: the rendered text is rewritten each
    /// frame to a value linearly interpolated between `from_value`
    /// and `to_value` over `duration_secs`. After the ramp, holds
    /// at `to_value` for the rest of the clip. `decimals` controls
    /// formatting (0..3); the user-authored `text` field is ignored
    /// while Counter is active. Common uses: stats, sports scores,
    /// fundraiser totals, sequence countdowns.
    Counter {
        from_value: f64,
        to_value: f64,
        duration_secs: f32,
        decimals: u8,
    },
    /// Outline draw-on: text outlines reveal progressively from the
    /// left edge over `duration_secs`. Drives `outline_write_on`
    /// from 0 → 1 inside the renderer; the body fill remains
    /// stationary so the outline "draws onto" the text.
    OutlineDrawOn { duration_secs: f32 },
    /// Two-colour blend: text fill smoothly oscillates between
    /// `color_a` and `color_b` at `rate_hz`. Distinct from
    /// ColorCycle (which rotates through the whole hue wheel) —
    /// use this when the design calls for two specific colours
    /// (brand A / B, accent / accent_alt, day / night).
    ColorBlend {
        color_a: [u8; 4],
        color_b: [u8; 4],
        rate_hz: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotionBlur {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Shutter angle in degrees (0-360, default 180 = half-frame blur)
    pub shutter_angle: f32,
    /// Per-frame keyframes for `shutter_angle`; empty by default.
    /// `samples` is integer-valued and not keyframable — animating
    /// sample count would visibly pop between frames rather than
    /// interpolate, so it stays a static dial.
    #[serde(default)]
    pub shutter_angle_keyframes: Vec<klipp_core::Keyframe>,
    /// Number of samples (more = smoother but slower, 4-16 typical)
    pub samples: u8,
}

impl MotionBlur {
    /// Effective `shutter_angle` at `time_secs`. Static fallback when
    /// the keyframe vec is empty.
    pub fn evaluated_shutter_angle(&self, time_secs: f64) -> f32 {
        if self.shutter_angle_keyframes.is_empty() {
            self.shutter_angle
        } else {
            klipp_core::keyframe::evaluate(
                &self.shutter_angle_keyframes,
                time_secs,
                self.shutter_angle,
            )
        }
    }

    /// Mutable accessor for the keyframe vec named by `param_key`.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::MOTION_BLUR_SHUTTER_ANGLE => {
                Some(&mut self.shutter_angle_keyframes)
            }
            _ => None,
        }
    }

    /// Read-only mirror of `keyframes_mut_for`.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::MOTION_BLUR_SHUTTER_ANGLE => {
                Some(&self.shutter_angle_keyframes)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextExtrusion {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Front face color (if [0,0,0,0] uses title.color)
    pub face_color: [u8; 4],
    /// Extruded side color
    pub side_color: [u8; 4],
    /// Extrusion distance in pixels (1-50)
    pub depth: f32,
    /// Per-frame keyframes for `depth`; empty by default.
    #[serde(default)]
    pub depth_keyframes: Vec<klipp_core::Keyframe>,
    /// Direction in degrees (default 135 = down-right shadow direction)
    pub angle: f32,
    /// Per-frame keyframes for `angle`.
    #[serde(default)]
    pub angle_keyframes: Vec<klipp_core::Keyframe>,
    /// 0.0-1.0, simulated directional lighting
    pub lighting: f32,
    /// Per-frame keyframes for `lighting`.
    #[serde(default)]
    pub lighting_keyframes: Vec<klipp_core::Keyframe>,
}

impl TextExtrusion {
    /// Effective `depth` at `time_secs`. Static fallback when the
    /// keyframe vec is empty.
    pub fn evaluated_depth(&self, time_secs: f64) -> f32 {
        if self.depth_keyframes.is_empty() {
            self.depth
        } else {
            klipp_core::keyframe::evaluate(
                &self.depth_keyframes,
                time_secs,
                self.depth,
            )
        }
    }

    /// Effective `angle` at `time_secs`. Static fallback when empty.
    pub fn evaluated_angle(&self, time_secs: f64) -> f32 {
        if self.angle_keyframes.is_empty() {
            self.angle
        } else {
            klipp_core::keyframe::evaluate(
                &self.angle_keyframes,
                time_secs,
                self.angle,
            )
        }
    }

    /// Effective `lighting` at `time_secs`. Static fallback when empty.
    pub fn evaluated_lighting(&self, time_secs: f64) -> f32 {
        if self.lighting_keyframes.is_empty() {
            self.lighting
        } else {
            klipp_core::keyframe::evaluate(
                &self.lighting_keyframes,
                time_secs,
                self.lighting,
            )
        }
    }

    /// Mutable accessor for the keyframe vec named by `param_key`.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::EXTRUSION_DEPTH => Some(&mut self.depth_keyframes),
            klipp_core::title_effect_keys::EXTRUSION_ANGLE => Some(&mut self.angle_keyframes),
            klipp_core::title_effect_keys::EXTRUSION_LIGHTING => Some(&mut self.lighting_keyframes),
            _ => None,
        }
    }

    /// Read-only mirror of `keyframes_mut_for`.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::EXTRUSION_DEPTH => Some(&self.depth_keyframes),
            klipp_core::title_effect_keys::EXTRUSION_ANGLE => Some(&self.angle_keyframes),
            klipp_core::title_effect_keys::EXTRUSION_LIGHTING => Some(&self.lighting_keyframes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TextPath {
    /// Text follows a circular arc
    Arc {
        radius: f32,      // radius in pixels (100-500)
        start_angle: f32,  // degrees, where text starts (0 = right, 90 = bottom)
        sweep: f32,        // degrees of arc the text spans (positive = clockwise)
    },
    /// Text follows a full circle
    Circle {
        radius: f32,      // radius in pixels
    },
    /// Text follows a sine wave
    Wave {
        amplitude: f32,   // vertical amplitude in pixels (10-100)
        frequency: f32,   // number of wave cycles across the text (0.5-5)
        phase: f32,       // phase offset in degrees (0-360)
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharAnimation {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Type of per-character entrance — enum, not keyframable.
    pub entrance: CharEntrance,
    /// Seconds between each character's animation start
    pub stagger: f32,
    /// Per-frame keyframes for `stagger`; empty by default.
    #[serde(default)]
    pub stagger_keyframes: Vec<klipp_core::Keyframe>,
    /// Vertical wave oscillation amplitude (0 = no wave)
    pub wave_amplitude: f32,
    /// Per-frame keyframes for `wave_amplitude`.
    #[serde(default)]
    pub wave_amplitude_keyframes: Vec<klipp_core::Keyframe>,
    /// Wave speed in cycles per second
    pub wave_speed: f32,
    /// Per-frame keyframes for `wave_speed`.
    #[serde(default)]
    pub wave_speed_keyframes: Vec<klipp_core::Keyframe>,
}

impl CharAnimation {
    /// Effective `stagger` at `time_secs`. Static fallback when empty.
    pub fn evaluated_stagger(&self, time_secs: f64) -> f32 {
        if self.stagger_keyframes.is_empty() {
            self.stagger
        } else {
            klipp_core::keyframe::evaluate(
                &self.stagger_keyframes,
                time_secs,
                self.stagger,
            )
        }
    }

    /// Effective `wave_amplitude` at `time_secs`. Static fallback when empty.
    pub fn evaluated_wave_amplitude(&self, time_secs: f64) -> f32 {
        if self.wave_amplitude_keyframes.is_empty() {
            self.wave_amplitude
        } else {
            klipp_core::keyframe::evaluate(
                &self.wave_amplitude_keyframes,
                time_secs,
                self.wave_amplitude,
            )
        }
    }

    /// Effective `wave_speed` at `time_secs`. Static fallback when empty.
    pub fn evaluated_wave_speed(&self, time_secs: f64) -> f32 {
        if self.wave_speed_keyframes.is_empty() {
            self.wave_speed
        } else {
            klipp_core::keyframe::evaluate(
                &self.wave_speed_keyframes,
                time_secs,
                self.wave_speed,
            )
        }
    }

    /// Mutable accessor for the keyframe vec named by `param_key`.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::CHAR_ANIM_STAGGER => Some(&mut self.stagger_keyframes),
            klipp_core::title_effect_keys::CHAR_ANIM_WAVE_AMPLITUDE => Some(&mut self.wave_amplitude_keyframes),
            klipp_core::title_effect_keys::CHAR_ANIM_WAVE_SPEED => Some(&mut self.wave_speed_keyframes),
            _ => None,
        }
    }

    /// Read-only mirror of `keyframes_mut_for`.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::CHAR_ANIM_STAGGER => Some(&self.stagger_keyframes),
            klipp_core::title_effect_keys::CHAR_ANIM_WAVE_AMPLITUDE => Some(&self.wave_amplitude_keyframes),
            klipp_core::title_effect_keys::CHAR_ANIM_WAVE_SPEED => Some(&self.wave_speed_keyframes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum CharEntrance {
    #[default]
    None,
    FadeIn,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    ScaleUp,
    ScaleDown,
    /// Overshoot scale: each glyph briefly grows past 1.0 before
    /// settling, giving a tactile "pop" entrance per character.
    Pop,
    /// Soft entrance that simulates a focus-pull: the per-glyph
    /// alpha ramps up while the glyph is rendered at a slightly
    /// reduced scale (cheap stand-in for a true Gaussian blur —
    /// real blur requires a separate offscreen render pass).
    BlurIn,
}

fn default_one() -> f32 { 1.0 }

impl Default for TitleData {
    fn default() -> Self {
        TitleData::single_text(TextBox::default())
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum TitleBackground {
    #[default]
    None,
    Color {
        color: [u8; 4],
        padding: f32,
    },
    FullFrame {
        color: [u8; 4],
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleShadow {
    /// Soft-disable: when `false`, renderer skips this effect but the
    /// parameters are preserved so the inspector's subcard toggle can
    /// re-enable without losing tweaks. Keyframe vecs are preserved
    /// too, matching mute-not-delete semantics.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub color: [u8; 4],
    pub offset_x: f32,
    /// Per-frame keyframes for `offset_x`; empty by default.
    #[serde(default)]
    pub offset_x_keyframes: Vec<klipp_core::Keyframe>,
    pub offset_y: f32,
    /// Per-frame keyframes for `offset_y`.
    #[serde(default)]
    pub offset_y_keyframes: Vec<klipp_core::Keyframe>,
    pub blur: f32,
    /// Per-frame keyframes for `blur`.
    #[serde(default)]
    pub blur_keyframes: Vec<klipp_core::Keyframe>,
}

impl TitleShadow {
    /// Effective `offset_x` at `time_secs`: keyframe-evaluated when
    /// the corresponding vec is non-empty, else the static value.
    pub fn evaluated_offset_x(&self, time_secs: f64) -> f32 {
        if self.offset_x_keyframes.is_empty() {
            self.offset_x
        } else {
            klipp_core::keyframe::evaluate(
                &self.offset_x_keyframes,
                time_secs,
                self.offset_x,
            )
        }
    }

    /// Effective `offset_y` at `time_secs`. See `evaluated_offset_x`.
    pub fn evaluated_offset_y(&self, time_secs: f64) -> f32 {
        if self.offset_y_keyframes.is_empty() {
            self.offset_y
        } else {
            klipp_core::keyframe::evaluate(
                &self.offset_y_keyframes,
                time_secs,
                self.offset_y,
            )
        }
    }

    /// Effective `blur` at `time_secs`. See `evaluated_offset_x`.
    pub fn evaluated_blur(&self, time_secs: f64) -> f32 {
        if self.blur_keyframes.is_empty() {
            self.blur
        } else {
            klipp_core::keyframe::evaluate(
                &self.blur_keyframes,
                time_secs,
                self.blur,
            )
        }
    }

    /// Mutable accessor for the keyframe vec named by `param_key`
    /// (one of the `klipp_core::title_effect_keys` constants). Returns
    /// `None` for unknown keys.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::SHADOW_OFFSET_X => Some(&mut self.offset_x_keyframes),
            klipp_core::title_effect_keys::SHADOW_OFFSET_Y => Some(&mut self.offset_y_keyframes),
            klipp_core::title_effect_keys::SHADOW_BLUR => Some(&mut self.blur_keyframes),
            _ => None,
        }
    }

    /// Read-only mirror of `keyframes_mut_for`.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::SHADOW_OFFSET_X => Some(&self.offset_x_keyframes),
            klipp_core::title_effect_keys::SHADOW_OFFSET_Y => Some(&self.offset_y_keyframes),
            klipp_core::title_effect_keys::SHADOW_BLUR => Some(&self.blur_keyframes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextOutline {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub color: [u8; 4],
    /// Width in pixels, typically 1-8
    pub width: f32,
    /// Per-frame keyframes for `width`; empty by default.
    /// Each `TextOutline` in a stack carries its own keyframe vec, so
    /// stacked outlines animate independently. The wire-format
    /// param_key is `outline.{index}.width` where `index` is the
    /// position in `TextBox.outlines`.
    #[serde(default)]
    pub width_keyframes: Vec<klipp_core::Keyframe>,
}

impl TextOutline {
    /// Effective `width` at `time_secs`. Static fallback when the
    /// keyframe vec is empty.
    pub fn evaluated_width(&self, time_secs: f64) -> f32 {
        if self.width_keyframes.is_empty() {
            self.width
        } else {
            klipp_core::keyframe::evaluate(
                &self.width_keyframes,
                time_secs,
                self.width,
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextGlow {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub color: [u8; 4],
    /// Blur radius in pixels, typically 5-30.
    pub radius: f32,
    /// Per-frame keyframes for `radius`. When non-empty the renderer
    /// samples this against clip-local time and overrides `radius`
    /// for that frame; empty by default. Paired-adjacent layout makes
    /// the static fallback and its animation track read together.
    #[serde(default)]
    pub radius_keyframes: Vec<klipp_core::Keyframe>,
    /// Intensity 0.0-1.0, default 0.8.
    pub intensity: f32,
    /// Per-frame keyframes for `intensity`. Same precedence rule as
    /// `radius_keyframes`.
    #[serde(default)]
    pub intensity_keyframes: Vec<klipp_core::Keyframe>,
}

impl TextGlow {
    /// Effective `radius` at `time_secs`: keyframe-evaluated when
    /// `radius_keyframes` is non-empty, else the static `radius`.
    /// Empty vec short-circuits, so titles without keyframed glow pay
    /// no per-frame sampling cost.
    pub fn evaluated_radius(&self, time_secs: f64) -> f32 {
        if self.radius_keyframes.is_empty() {
            self.radius
        } else {
            klipp_core::keyframe::evaluate(
                &self.radius_keyframes,
                time_secs,
                self.radius,
            )
        }
    }

    /// Effective `intensity` at `time_secs`. See `evaluated_radius`.
    pub fn evaluated_intensity(&self, time_secs: f64) -> f32 {
        if self.intensity_keyframes.is_empty() {
            self.intensity
        } else {
            klipp_core::keyframe::evaluate(
                &self.intensity_keyframes,
                time_secs,
                self.intensity,
            )
        }
    }

    /// Mutable accessor for the keyframe vec corresponding to
    /// `param_key` (one of the constants in
    /// [`klipp_core::title_effect_keys`]). Returns `None` for an
    /// unknown key — handlers should debug_assert on that.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::GLOW_RADIUS => Some(&mut self.radius_keyframes),
            klipp_core::title_effect_keys::GLOW_INTENSITY => Some(&mut self.intensity_keyframes),
            _ => None,
        }
    }

    /// Read-only accessor mirroring `keyframes_mut_for`. Used by the
    /// inspector to build a `KeyframeState` for the diamond UI.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::GLOW_RADIUS => Some(&self.radius_keyframes),
            klipp_core::title_effect_keys::GLOW_INTENSITY => Some(&self.intensity_keyframes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextGradient {
    pub top_color: [u8; 4],
    pub bottom_color: [u8; 4],
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InnerShadow {
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub color: [u8; 4],
    pub offset_x: f32,
    /// Per-frame keyframes for `offset_x`; empty by default.
    #[serde(default)]
    pub offset_x_keyframes: Vec<klipp_core::Keyframe>,
    pub offset_y: f32,
    /// Per-frame keyframes for `offset_y`.
    #[serde(default)]
    pub offset_y_keyframes: Vec<klipp_core::Keyframe>,
    pub blur: f32,
    /// Per-frame keyframes for `blur`.
    #[serde(default)]
    pub blur_keyframes: Vec<klipp_core::Keyframe>,
}

impl InnerShadow {
    /// Effective `offset_x` at `time_secs`. Static fallback when the
    /// keyframe vec is empty.
    pub fn evaluated_offset_x(&self, time_secs: f64) -> f32 {
        if self.offset_x_keyframes.is_empty() {
            self.offset_x
        } else {
            klipp_core::keyframe::evaluate(
                &self.offset_x_keyframes,
                time_secs,
                self.offset_x,
            )
        }
    }

    /// Effective `offset_y` at `time_secs`. Static fallback when the
    /// keyframe vec is empty.
    pub fn evaluated_offset_y(&self, time_secs: f64) -> f32 {
        if self.offset_y_keyframes.is_empty() {
            self.offset_y
        } else {
            klipp_core::keyframe::evaluate(
                &self.offset_y_keyframes,
                time_secs,
                self.offset_y,
            )
        }
    }

    /// Effective `blur` at `time_secs`. Static fallback when the
    /// keyframe vec is empty.
    pub fn evaluated_blur(&self, time_secs: f64) -> f32 {
        if self.blur_keyframes.is_empty() {
            self.blur
        } else {
            klipp_core::keyframe::evaluate(
                &self.blur_keyframes,
                time_secs,
                self.blur,
            )
        }
    }

    /// Mutable accessor for the keyframe vec named by `param_key`.
    pub fn keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        match param_key {
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_X => Some(&mut self.offset_x_keyframes),
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_Y => Some(&mut self.offset_y_keyframes),
            klipp_core::title_effect_keys::INNER_SHADOW_BLUR => Some(&mut self.blur_keyframes),
            _ => None,
        }
    }

    /// Read-only mirror of `keyframes_mut_for`.
    pub fn keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        match param_key {
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_X => Some(&self.offset_x_keyframes),
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_Y => Some(&self.offset_y_keyframes),
            klipp_core::title_effect_keys::INNER_SHADOW_BLUR => Some(&self.blur_keyframes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum TextPattern {
    HorizontalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
    VerticalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
    Checkerboard { color_a: [u8; 4], color_b: [u8; 4], cell_size: f32 },
    DiagonalStripes { color_a: [u8; 4], color_b: [u8; 4], stripe_width: f32 },
}

/// Source of the texture fill — either a user-picked file or one of
/// the bundled procedural presets. Presets are generated on the fly
/// by the renderer so they don't need any image assets to ship.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum TextureSource {
    #[default]
    None,
    File(String),
    Preset(TexturePresetId),
}

/// Bundled procedural texture presets. The renderer generates each
/// one algorithmically so adding a preset is just a renderer arm,
/// no image asset / licence audit needed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TexturePresetId {
    /// Soft fibrous noise — paper-stock grain.
    PaperGrain,
    /// Horizontal CRT-style scanlines.
    Scanlines,
    /// Diagonal hatch lines.
    DiagonalHatch,
    /// Smooth vertical light → dark sweep.
    GradientSweep,
}

/// How the texture maps into the glyph bounding box.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum TextureTileMode {
    /// Scale to fill the bbox exactly. May distort aspect ratio.
    #[default]
    Stretch,
    /// Repeat the texture at native (or `scale`d) size across the bbox.
    Tile,
    /// Scale to fit inside the bbox, preserving aspect — may show gaps.
    Fit,
    /// Scale to cover the bbox, preserving aspect — may crop.
    Cover,
}

/// Texture fill on a TextBox — replaces the legacy
/// `texture_fill_path` + `texture_fill_enabled` pair with a single
/// struct that also carries tile mode, scale, offset, rotation,
/// opacity, and tint. Soft-disable is the `enabled` field; the
/// renderer skips the effect when `enabled` is false but the source
/// + params are preserved for round-trip on re-enable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureFill {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: TextureSource,
    #[serde(default)]
    pub tile_mode: TextureTileMode,
    /// Multiplier on the texture's native size. 1.0 = native.
    #[serde(default = "default_one_f32")]
    pub scale: f32,
    /// Horizontal offset in texture-space pixels. 0 = no shift.
    #[serde(default)]
    pub offset_x: f32,
    /// Vertical offset in texture-space pixels.
    #[serde(default)]
    pub offset_y: f32,
    /// Rotation in degrees, applied to the UV space before sampling.
    #[serde(default)]
    pub rotation: f32,
    /// 0..1 — multiplies the texture's alpha during composite.
    #[serde(default = "default_one_f32")]
    pub opacity: f32,
    /// Multiplied with the sampled colour. White (255,255,255,255)
    /// = no tint; mid-grey halves brightness; coloured tint
    /// recolours the texture.
    #[serde(default = "default_white_rgba")]
    pub tint: [u8; 4],
}

fn default_white_rgba() -> [u8; 4] {
    [255, 255, 255, 255]
}

impl Default for TextureFill {
    fn default() -> Self {
        Self {
            enabled: true,
            source: TextureSource::default(),
            tile_mode: TextureTileMode::default(),
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            tint: default_white_rgba(),
        }
    }
}

impl TextureFill {
    /// Construct from a legacy file path. Used by the project-load
    /// migration path when reading pre-TextureFill projects.
    pub fn from_legacy_path(path: String, enabled: bool) -> Self {
        Self {
            enabled,
            source: TextureSource::File(path),
            ..Self::default()
        }
    }

    /// True when the texture should actually contribute to the
    /// rendered output — both the user toggle is on AND the source
    /// resolves to something the renderer can sample.
    pub fn is_active(&self) -> bool {
        self.enabled && !matches!(self.source, TextureSource::None)
    }

    /// File path if the source is a user-picked file, else `None`.
    /// Used by the project-save / consolidate paths that need to
    /// rewrite paths when the project moves.
    pub fn file_path(&self) -> Option<&str> {
        match &self.source {
            TextureSource::File(p) => Some(p.as_str()),
            _ => None,
        }
    }

    /// Mutable file-path accessor. Returns `None` when the source
    /// isn't `File`, so consolidate / relink can rewrite in place.
    pub fn file_path_mut(&mut self) -> Option<&mut String> {
        match &mut self.source {
            TextureSource::File(p) => Some(p),
            _ => None,
        }
    }
}

// ─── Style Run Manipulation Helpers ─────────────────────────────────────────

/// Apply a style mutation to a byte range within style runs.
/// Ensures full coverage of the text, splits runs at range boundaries,
/// applies the update, then merges adjacent runs with identical styles.
pub fn apply_style_to_range(
    style_runs: &mut Vec<StyleRun>,
    text_len: usize,
    range_start: usize,
    range_end: usize,
    update: impl Fn(&mut StyleRun),
) {
    if text_len == 0 || range_start >= range_end || range_start >= text_len {
        return;
    }
    let range_end = range_end.min(text_len);

    // Step 1: Ensure runs cover the full text (fill gaps with default runs)
    if style_runs.is_empty() {
        style_runs.push(StyleRun::default_for_range(0, text_len));
    } else {
        ensure_full_coverage(style_runs, text_len);
    }

    // Step 2: Split any run that straddles the range boundaries
    split_at(style_runs, range_start);
    split_at(style_runs, range_end);

    // Step 3: Apply the update to all runs that overlap the range
    for run in style_runs.iter_mut() {
        if run.start >= range_start && run.end <= range_end {
            update(run);
        }
    }

    // Step 4: Merge adjacent runs with identical styles
    merge_adjacent(style_runs);

    // Step 5: If all runs are fully inherited, clear the vec (clean state)
    if style_runs.iter().all(|r| r.is_all_inherited()) {
        style_runs.clear();
    }
}

/// Ensure style runs cover [0, text_len) with no gaps or overlaps.
fn ensure_full_coverage(runs: &mut Vec<StyleRun>, text_len: usize) {
    runs.sort_by_key(|r| r.start);

    // Clamp all runs to text_len
    for r in runs.iter_mut() {
        r.end = r.end.min(text_len);
    }
    runs.retain(|r| r.start < r.end);

    // Fill gaps
    let mut filled = Vec::new();
    let mut cursor = 0usize;
    for r in runs.iter() {
        if r.start > cursor {
            filled.push(StyleRun::default_for_range(cursor, r.start));
        }
        cursor = cursor.max(r.end);
        filled.push(r.clone());
    }
    if cursor < text_len {
        filled.push(StyleRun::default_for_range(cursor, text_len));
    }
    *runs = filled;
}

/// Split the run containing `pos` into two runs at that byte offset.
fn split_at(runs: &mut Vec<StyleRun>, pos: usize) {
    if let Some(idx) = runs.iter().position(|r| r.start < pos && pos < r.end) {
        let mut second = runs[idx].clone();
        second.start = pos;
        runs[idx].end = pos;
        runs.insert(idx + 1, second);
    }
}

/// Merge adjacent runs that have identical style fields.
fn merge_adjacent(runs: &mut Vec<StyleRun>) {
    let mut i = 0;
    while i + 1 < runs.len() {
        if runs[i].end == runs[i + 1].start && runs[i].same_style(&runs[i + 1]) {
            runs[i].end = runs[i + 1].end;
            runs.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// After text editing changes the length, adjust style runs to match new text_len.
/// Truncates runs that extend past text_len, removes empty runs.
pub fn adjust_style_runs_for_text_change(style_runs: &mut Vec<StyleRun>, new_text_len: usize) {
    if style_runs.is_empty() {
        return;
    }
    for r in style_runs.iter_mut() {
        if r.start >= new_text_len {
            r.start = new_text_len;
            r.end = new_text_len;
        } else if r.end > new_text_len {
            r.end = new_text_len;
        }
    }
    style_runs.retain(|r| r.start < r.end);
    merge_adjacent(style_runs);
    if style_runs.iter().all(|r| r.is_all_inherited()) {
        style_runs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{GeneratorData, MediaId, SequenceId};

    #[test]
    fn clip_kind_playability() {
        assert!(ClipKind::Media { media_id: MediaId::new() }.is_playable());
        assert!(ClipKind::Title(TitleData::default()).is_playable());
        assert!(ClipKind::Generator(GeneratorData::default()).is_playable());
        assert!(ClipKind::Sequence { sequence_id: SequenceId::new() }.is_playable());
        assert!(!ClipKind::Gap.is_playable());
        assert!(!ClipKind::AdjustmentLayer.is_playable());
    }

    #[test]
    fn clip_kind_decoder_need() {
        assert!(ClipKind::Media { media_id: MediaId::new() }.needs_decoder());
        assert!(ClipKind::Sequence { sequence_id: SequenceId::new() }.needs_decoder());
        assert!(!ClipKind::Title(TitleData::default()).needs_decoder());
        assert!(!ClipKind::Generator(GeneratorData::default()).needs_decoder());
    }

    /// Default `ObjectTransform` (and the renderer fallback path) ships
    /// empty position-keyframe vecs — the renderer's "static center"
    /// path stays in effect until the user explicitly animates.
    #[test]
    fn default_text_transform_has_no_position_keyframes() {
        let t = default_text_transform();
        assert!(t.center_x_keyframes.is_empty());
        assert!(t.center_y_keyframes.is_empty());
    }

    /// Round-trip: a `TextBox` with non-empty position keyframes on its
    /// transform serialises and deserialises to an equivalent value,
    /// preserving every keyframe field (time, value, interpolation,
    /// tangents).
    #[test]
    fn position_keyframes_round_trip_through_serde() {
        use klipp_core::{Interpolation, Keyframe};

        let tb = TextBox::default();
        let mut td = TitleData::single_text(tb);
        let obj = td.objects.first_mut().expect("default has one object");
        obj.transform.center_x_keyframes = vec![
            Keyframe {
                time_secs: 0.0,
                value: 0.1,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.5,
                value: 0.5,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.25, -0.05),
                tangent_out: (0.25, 0.05),
            },
        ];
        obj.transform.center_y_keyframes = vec![Keyframe {
            time_secs: 1.0,
            value: 0.85,
            interpolation: Interpolation::Step,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
        }];

        let json = serde_json::to_string(&td).expect("serialise");
        let round: TitleData = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(td, round, "round-trip must be lossless");
    }

    /// `upsert_position_keyframe` inserts in time order and replaces
    /// any existing keyframe within the ±20ms dedup window. This is
    /// the contract every `AddTitleObjectPositionKeyframe` handler
    /// invocation relies on.
    #[test]
    fn upsert_position_keyframe_inserts_sorted_and_dedups() {
        use klipp_core::{Interpolation, TitlePositionAxis};

        let mut t = default_text_transform();
        // Insert out of order: 1.0s, 0.0s, 0.5s. Vec must end up sorted.
        t.upsert_position_keyframe(TitlePositionAxis::X, 1.0, 0.8);
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.0, 0.1);
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.5, 0.4);
        let times: Vec<f64> = t.center_x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![0.0, 0.5, 1.0], "vec must stay sorted by time");

        // Y vec is independent of X.
        assert!(t.center_y_keyframes.is_empty());

        // Re-upsert at 0.5s with a new value: count stays at 3, value
        // updates in place.
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.5, 0.42);
        assert_eq!(t.center_x_keyframes.len(), 3);
        let mid = t.center_x_keyframes.iter().find(|k| (k.time_secs - 0.5).abs() < 0.01).unwrap();
        assert_eq!(mid.value, 0.42, "second upsert at same time must replace value");

        // Within ±20ms is "same time" for the dedup — so 0.51s
        // overwrites the 0.5s entry rather than adding a new one.
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.51, 0.5);
        assert_eq!(t.center_x_keyframes.len(), 3, "±20ms dedup must not duplicate");

        // Default interpolation is Linear.
        for kf in &t.center_x_keyframes {
            assert_eq!(kf.interpolation, Interpolation::Linear);
        }
    }

    /// `remove_position_keyframe_at` removes the matching keyframe and
    /// no-ops when no keyframe is within the dedup window.
    #[test]
    fn remove_position_keyframe_at_removes_matching_only() {
        use klipp_core::TitlePositionAxis;

        let mut t = default_text_transform();
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.0, 0.1);
        t.upsert_position_keyframe(TitlePositionAxis::X, 1.0, 0.5);
        t.upsert_position_keyframe(TitlePositionAxis::X, 2.0, 0.9);
        assert_eq!(t.center_x_keyframes.len(), 3);

        // Remove at 1.0s → middle gone.
        t.remove_position_keyframe_at(TitlePositionAxis::X, 1.0);
        let times: Vec<f64> = t.center_x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![0.0, 2.0]);

        // No-op when no keyframe is within ±20ms.
        t.remove_position_keyframe_at(TitlePositionAxis::X, 5.0);
        assert_eq!(t.center_x_keyframes.len(), 2, "remove far from any kf must no-op");

        // Remove on the Y axis must not touch the X vec.
        t.remove_position_keyframe_at(TitlePositionAxis::Y, 0.0);
        assert_eq!(t.center_x_keyframes.len(), 2);
    }

    /// `move_position_keyframe` updates time + value at the index and
    /// re-sorts so the vec remains in time order.
    #[test]
    fn move_position_keyframe_updates_and_re_sorts() {
        use klipp_core::TitlePositionAxis;

        let mut t = default_text_transform();
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.0, 0.1);
        t.upsert_position_keyframe(TitlePositionAxis::X, 1.0, 0.5);
        t.upsert_position_keyframe(TitlePositionAxis::X, 2.0, 0.9);

        // Move index-0 keyframe to time=3.0 — must end up at the end of
        // the vec after the re-sort.
        let ok = t.move_position_keyframe(TitlePositionAxis::X, 0, 3.0, 0.7);
        assert!(ok);
        let times: Vec<f64> = t.center_x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![1.0, 2.0, 3.0]);
        let last = t.center_x_keyframes.last().unwrap();
        assert_eq!(last.value, 0.7);

        // Out-of-range index returns false, no mutation.
        assert!(!t.move_position_keyframe(TitlePositionAxis::X, 99, 5.0, 0.0));
        assert_eq!(t.center_x_keyframes.len(), 3);
    }

    /// `set_position_keyframe_interpolation` flips the interp mode at
    /// the given index without disturbing time/value.
    #[test]
    fn set_position_keyframe_interpolation_updates_mode_only() {
        use klipp_core::{Interpolation, TitlePositionAxis};

        let mut t = default_text_transform();
        t.upsert_position_keyframe(TitlePositionAxis::Y, 0.0, 0.1);
        t.upsert_position_keyframe(TitlePositionAxis::Y, 1.0, 0.9);

        let ok = t.set_position_keyframe_interpolation(
            TitlePositionAxis::Y, 1, Interpolation::Bezier,
        );
        assert!(ok);
        assert_eq!(t.center_y_keyframes[0].interpolation, Interpolation::Linear);
        assert_eq!(t.center_y_keyframes[1].interpolation, Interpolation::Bezier);
        // Time + value untouched.
        assert_eq!(t.center_y_keyframes[1].time_secs, 1.0);
        assert_eq!(t.center_y_keyframes[1].value, 0.9);

        // Out-of-range returns false.
        assert!(!t.set_position_keyframe_interpolation(
            TitlePositionAxis::Y, 99, Interpolation::Step,
        ));
    }

    /// Backward-compat: a `TitleData` JSON written before the keyframe
    /// fields existed must deserialise with empty vecs (via
    /// `#[serde(default)]`) — every project file on disk pre-#2 stays
    /// loadable.
    #[test]
    fn legacy_titledata_without_position_keyframes_loads_with_defaults() {
        // Strip the keyframe fields from a fresh JSON dump to simulate
        // a pre-#2 save file. Using regex-style replace would be
        // brittle; instead we hand-roll a minimal JSON matching the
        // pre-keyframe shape.
        let legacy_json = r#"{
            "objects": [
                {
                    "id": 1,
                    "name": "",
                    "transform": {
                        "center": { "x": 0.5, "y": 0.5 },
                        "size": { "x": 1.0, "y": 1.0 }
                    },
                    "visible": true,
                    "locked": false,
                    "opacity": 1.0,
                    "content": {
                        "TextBox": {
                            "text": "Hello",
                            "font_size": 32.0,
                            "font_name": "sans-serif",
                            "font_style": "Regular",
                            "color": [255, 255, 255, 255],
                            "alignment": "Center",
                            "outlines": [],
                            "tracking": 0.0,
                            "line_spacing": 1.0,
                            "write_on": 1.0,
                            "style_runs": []
                        }
                    }
                }
            ],
            "next_object_id": 2,
            "background": "None"
        }"#;
        let td: TitleData = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert_eq!(td.objects.len(), 1);
        let xform = &td.objects[0].transform;
        assert!(xform.center_x_keyframes.is_empty());
        assert!(xform.center_y_keyframes.is_empty());
        assert_eq!(xform.center.x, 0.5);
        assert_eq!(xform.center.y, 0.5);
    }

    /// `evaluated_radius` and `evaluated_intensity` short-circuit to
    /// the static value when the corresponding keyframe vec is empty,
    /// pinning the no-overhead path.
    #[test]
    fn textglow_evaluator_empty_kfs_returns_static() {
        let g = TextGlow {
            enabled: true,
            color: [255, 255, 255, 255],
            radius: 10.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        };
        // Any time should return the static fallback when no kfs.
        assert_eq!(g.evaluated_radius(0.0), 10.0);
        assert_eq!(g.evaluated_radius(99.0), 10.0);
        assert_eq!(g.evaluated_intensity(0.0), 0.8);
        assert_eq!(g.evaluated_intensity(99.0), 0.8);
    }

    /// With keyframes, the evaluator overrides the static value at
    /// the requested time. Boundary behaviour matches `keyframe::evaluate`
    /// (hold-before-first / hold-after-last / linear in between).
    #[test]
    fn textglow_evaluator_with_keyframes_returns_interpolated() {
        use klipp_core::{Interpolation, Keyframe};

        let g = TextGlow {
            enabled: true,
            color: [255, 255, 255, 255],
            radius: 10.0,
            // Static value should be ignored when keyframes are
            // present — this radius_keyframes ramps 4 → 24 over 1s.
            radius_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 4.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        };
        assert!((g.evaluated_radius(0.0) - 4.0).abs() < 1e-5);
        assert!((g.evaluated_radius(0.5) - 14.0).abs() < 1e-5);
        assert!((g.evaluated_radius(1.0) - 24.0).abs() < 1e-5);
        // After last keyframe holds.
        assert!((g.evaluated_radius(99.0) - 24.0).abs() < 1e-5);
        // Intensity has no kfs — static fallback regardless of time.
        assert_eq!(g.evaluated_intensity(0.5), 0.8);
    }

    /// Round-trip: a `TextGlow` with non-empty keyframes serialises
    /// and deserialises losslessly. The reorder of declaration fields
    /// (`enabled` first to match the user's locked layout) must not
    /// disturb the on-disk JSON shape.
    #[test]
    fn textglow_keyframes_round_trip_through_serde() {
        use klipp_core::{Interpolation, Keyframe};

        let g = TextGlow {
            enabled: true,
            color: [10, 20, 30, 255],
            radius: 12.0,
            radius_keyframes: vec![Keyframe {
                time_secs: 0.5,
                value: 18.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.1, 0.0),
                tangent_out: (0.1, 0.0),
            }],
            intensity: 0.9,
            intensity_keyframes: vec![Keyframe {
                time_secs: 0.0,
                value: 0.5,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
            }],
        };
        let json = serde_json::to_string(&g).expect("serialise");
        let round: TextGlow = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(g, round, "round-trip must be lossless");
    }

    /// Backward-compat: a `TextGlow` JSON written before the keyframe
    /// fields existed must deserialise with empty vecs via
    /// `#[serde(default)]`. Every project file on disk pre-#5 stays
    /// loadable.
    #[test]
    fn legacy_textglow_without_keyframe_fields_loads_with_defaults() {
        let legacy_json = r#"{
            "enabled": true,
            "color": [10, 20, 30, 255],
            "radius": 12.0,
            "intensity": 0.9
        }"#;
        let g: TextGlow = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert!(g.radius_keyframes.is_empty());
        assert!(g.intensity_keyframes.is_empty());
        assert_eq!(g.radius, 12.0);
        assert_eq!(g.intensity, 0.9);
    }

    /// The `keyframes_mut_for` / `keyframes_for` accessors route on
    /// `param_key` strings from `klipp_core::title_effect_keys`. A
    /// typo (or any unknown key) returns `None` — handlers
    /// debug_assert on that.
    #[test]
    fn textglow_keyframes_router_dispatches_on_param_key() {
        use klipp_core::title_effect_keys;

        let mut g = TextGlow {
            enabled: true,
            color: [255, 255, 255, 255],
            radius: 10.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        };
        // Mutable router: insert a keyframe through the param-key path.
        klipp_core::keyframe::upsert_keyframe(
            g.keyframes_mut_for(title_effect_keys::GLOW_RADIUS).unwrap(),
            0.0, 8.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            g.keyframes_mut_for(title_effect_keys::GLOW_INTENSITY).unwrap(),
            1.0, 0.4,
        );
        assert_eq!(g.radius_keyframes.len(), 1);
        assert_eq!(g.intensity_keyframes.len(), 1);

        // Read-only router round-trips the same data.
        assert_eq!(
            g.keyframes_for(title_effect_keys::GLOW_RADIUS).unwrap().len(),
            1,
        );

        // Unknown key returns None on both sides — typo guard.
        assert!(g.keyframes_mut_for("glow.intencity").is_none());
        assert!(g.keyframes_for("glow.intencity").is_none());
    }

    /// `evaluated_offset_x`, `evaluated_offset_y`, `evaluated_blur`
    /// short-circuit to the static value when their keyframe vec is
    /// empty — pins the no-overhead path for unanimated shadows.
    #[test]
    fn titleshadow_evaluator_empty_kfs_returns_static() {
        let s = TitleShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 3.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 3.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: Vec::new(),
        };
        assert_eq!(s.evaluated_offset_x(0.0), 3.0);
        assert_eq!(s.evaluated_offset_y(99.0), 3.0);
        assert_eq!(s.evaluated_blur(1.5), 6.0);
    }

    /// With keyframes, each evaluator overrides its static counterpart
    /// at the requested time. Independence: keyframing one param
    /// doesn't disturb the others' static-fallback behaviour.
    #[test]
    fn titleshadow_evaluator_with_keyframes_returns_interpolated() {
        use klipp_core::{Interpolation, Keyframe};

        let s = TitleShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 3.0,
            offset_x_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: -10.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
            offset_y: 3.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 2.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 16.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
        };
        // offset_x animates -10 → +10.
        assert!((s.evaluated_offset_x(0.0) - (-10.0)).abs() < 1e-5);
        assert!((s.evaluated_offset_x(0.5) - 0.0).abs() < 1e-5);
        assert!((s.evaluated_offset_x(1.0) - 10.0).abs() < 1e-5);
        // offset_y has no kfs — static fallback at every time.
        assert_eq!(s.evaluated_offset_y(0.0), 3.0);
        assert_eq!(s.evaluated_offset_y(0.5), 3.0);
        // blur animates 2 → 16.
        assert!((s.evaluated_blur(0.0) - 2.0).abs() < 1e-5);
        assert!((s.evaluated_blur(0.5) - 9.0).abs() < 1e-5);
    }

    /// Round-trip: a `TitleShadow` with non-empty keyframes serialises
    /// and deserialises losslessly.
    #[test]
    fn titleshadow_keyframes_round_trip_through_serde() {
        use klipp_core::{Interpolation, Keyframe};

        let s = TitleShadow {
            enabled: true,
            color: [10, 20, 30, 200],
            offset_x: 3.0,
            offset_x_keyframes: vec![Keyframe {
                time_secs: 0.5, value: 5.0,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.1, 0.0), tangent_out: (0.1, 0.0),
            }],
            offset_y: 3.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: vec![Keyframe {
                time_secs: 0.0, value: 0.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            }],
        };
        let json = serde_json::to_string(&s).expect("serialise");
        let round: TitleShadow = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(s, round);
    }

    /// Backward-compat: a pre-#5 `TitleShadow` JSON without the keyframe
    /// fields must deserialise with empty vecs.
    #[test]
    fn legacy_titleshadow_without_keyframe_fields_loads_with_defaults() {
        let legacy_json = r#"{
            "enabled": true,
            "color": [10, 20, 30, 200],
            "offset_x": 3.0,
            "offset_y": 4.0,
            "blur": 6.0
        }"#;
        let s: TitleShadow = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert!(s.offset_x_keyframes.is_empty());
        assert!(s.offset_y_keyframes.is_empty());
        assert!(s.blur_keyframes.is_empty());
        assert_eq!(s.offset_x, 3.0);
        assert_eq!(s.offset_y, 4.0);
        assert_eq!(s.blur, 6.0);
    }

    /// `TextBox::effect_keyframes_mut_for` routes the `shadow.*`
    /// prefix to the right `TitleShadow` keyframe vec — and returns
    /// `None` if no shadow is configured. Same prefix-router contract
    /// as the glow path.
    #[test]
    fn textbox_router_dispatches_shadow_param_keys() {
        use klipp_core::title_effect_keys;

        let mut tb = TextBox::default();
        // No shadow yet: router returns None for every shadow key.
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_BLUR).is_none());
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_OFFSET_X).is_none());

        tb.shadow = Some(TitleShadow {
            enabled: true,
            color: [0, 0, 0, 180],
            offset_x: 3.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 3.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: Vec::new(),
        });
        // After enabling: each param key lands on its own vec.
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_OFFSET_X).unwrap(),
            0.0, 5.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_OFFSET_Y).unwrap(),
            0.0, 7.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_BLUR).unwrap(),
            0.5, 12.0,
        );
        let s = tb.shadow.as_ref().unwrap();
        assert_eq!(s.offset_x_keyframes.len(), 1);
        assert_eq!(s.offset_y_keyframes.len(), 1);
        assert_eq!(s.blur_keyframes.len(), 1);
        assert_eq!(s.offset_x_keyframes[0].value, 5.0);
        assert_eq!(s.offset_y_keyframes[0].value, 7.0);
        assert_eq!(s.blur_keyframes[0].value, 12.0);

        // Glow keys must not accidentally match shadow.
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::GLOW_RADIUS).is_none());
        // Unknown shadow key returns None.
        assert!(tb.effect_keyframes_mut_for("shadow.opacity").is_none());
    }

    /// Backward-compat for the InnerShadow keyframe fields.
    #[test]
    fn legacy_innershadow_without_keyframe_fields_loads_with_defaults() {
        let legacy_json = r#"{
            "enabled": true,
            "color": [0, 0, 0, 180],
            "offset_x": 2.0,
            "offset_y": 3.0,
            "blur": 4.0
        }"#;
        let s: InnerShadow = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert!(s.offset_x_keyframes.is_empty());
        assert!(s.offset_y_keyframes.is_empty());
        assert!(s.blur_keyframes.is_empty());
        assert_eq!(s.offset_x, 2.0);
        assert_eq!(s.blur, 4.0);
    }

    /// `InnerShadow::evaluated_*` short-circuit on empty kfs and
    /// interpolate when populated.
    #[test]
    fn innershadow_evaluator_static_and_interpolated() {
        use klipp_core::{Interpolation, Keyframe};

        let mut s = InnerShadow {
            enabled: true,
            color: [0, 0, 0, 180],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 3.0, offset_y_keyframes: Vec::new(),
            blur: 4.0, blur_keyframes: Vec::new(),
        };
        // Empty: returns static.
        assert_eq!(s.evaluated_offset_x(0.5), 2.0);
        assert_eq!(s.evaluated_blur(99.0), 4.0);

        // With kfs on blur only: blur ramps, offsets stay static.
        s.blur_keyframes = vec![
            Keyframe {
                time_secs: 0.0, value: 0.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0, value: 10.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
        ];
        assert!((s.evaluated_blur(0.5) - 5.0).abs() < 1e-5);
        assert_eq!(s.evaluated_offset_x(0.5), 2.0);
    }

    /// `TextExtrusion::evaluated_*` short-circuit on empty kfs and
    /// interpolate when populated. Independence between params holds.
    #[test]
    fn textextrusion_evaluator_static_and_interpolated() {
        use klipp_core::{Interpolation, Keyframe};

        let mut e = TextExtrusion {
            enabled: true,
            face_color: [255, 255, 255, 255],
            side_color: [80, 80, 80, 255],
            depth: 10.0, depth_keyframes: Vec::new(),
            angle: 135.0, angle_keyframes: Vec::new(),
            lighting: 0.5, lighting_keyframes: Vec::new(),
        };
        // Empty: returns static.
        assert_eq!(e.evaluated_depth(0.5), 10.0);
        assert_eq!(e.evaluated_angle(0.5), 135.0);
        assert_eq!(e.evaluated_lighting(0.5), 0.5);

        // Keyframe just `depth`: depth ramps, others stay static.
        e.depth_keyframes = vec![
            Keyframe {
                time_secs: 0.0, value: 5.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0, value: 25.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
        ];
        assert!((e.evaluated_depth(0.5) - 15.0).abs() < 1e-5);
        assert_eq!(e.evaluated_angle(0.5), 135.0);
        assert_eq!(e.evaluated_lighting(0.5), 0.5);
    }

    /// Round-trip: a fully-populated `TextureFill` (file source +
    /// every transform / blend param) serialises and deserialises
    /// losslessly via the new top-level `texture_fill` field on
    /// TextBox.
    #[test]
    fn texturefill_round_trips_through_serde() {
        let mut tb = TextBox::default();
        tb.texture_fill = Some(TextureFill {
            enabled: true,
            source: TextureSource::File("/tmp/paper.png".to_string()),
            tile_mode: TextureTileMode::Tile,
            scale: 1.5,
            offset_x: 12.0,
            offset_y: -8.0,
            rotation: 30.0,
            opacity: 0.75,
            tint: [200, 180, 140, 255],
        });
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: TextBox = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(tb.texture_fill, round.texture_fill);
    }

    /// Round-trip with a procedural preset source — checks the
    /// `Preset(TexturePresetId)` variant survives JSON.
    #[test]
    fn texturefill_preset_source_round_trips() {
        let tf = TextureFill {
            source: TextureSource::Preset(TexturePresetId::PaperGrain),
            ..TextureFill::default()
        };
        let json = serde_json::to_string(&tf).expect("serialise");
        let round: TextureFill = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(tf.source, round.source);
    }

    /// Backward-compat: a `TextureFill` JSON missing the optional
    /// transform / blend fields fills them with sensible defaults
    /// (`opacity=1`, `scale=1`, `tint=white`, `tile_mode=Stretch`).
    #[test]
    fn legacy_texturefill_without_optional_fields_uses_defaults() {
        // Only the required `source` field — everything else takes
        // its #[serde(default)].
        let legacy_json = r#"{
            "source": { "File": "/tmp/legacy.png" }
        }"#;
        let tf: TextureFill = serde_json::from_str(legacy_json)
            .expect("legacy texturefill JSON without optionals must parse");
        assert!(tf.enabled);
        assert!(matches!(tf.source, TextureSource::File(_)));
        assert!(matches!(tf.tile_mode, TextureTileMode::Stretch));
        assert_eq!(tf.scale, 1.0);
        assert_eq!(tf.offset_x, 0.0);
        assert_eq!(tf.offset_y, 0.0);
        assert_eq!(tf.rotation, 0.0);
        assert_eq!(tf.opacity, 1.0);
        assert_eq!(tf.tint, [255, 255, 255, 255]);
    }

    /// `is_active()` returns true only when the toggle is on AND
    /// the source actually points somewhere.
    #[test]
    fn texturefill_is_active_predicate() {
        let mut tf = TextureFill::default();
        // Default: enabled=true but source=None → not active.
        assert!(!tf.is_active());

        tf.source = TextureSource::File("/tmp/a.png".to_string());
        assert!(tf.is_active());

        tf.enabled = false;
        assert!(!tf.is_active());

        tf.enabled = true;
        tf.source = TextureSource::Preset(TexturePresetId::Scanlines);
        assert!(tf.is_active());
    }

    /// Backward-compat for the TextExtrusion keyframe fields.
    #[test]
    fn legacy_textextrusion_without_keyframe_fields_loads_with_defaults() {
        let legacy_json = r#"{
            "enabled": true,
            "face_color": [255, 255, 255, 255],
            "side_color": [80, 80, 80, 255],
            "depth": 12.0,
            "angle": 135.0,
            "lighting": 0.6
        }"#;
        let e: TextExtrusion = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert!(e.depth_keyframes.is_empty());
        assert!(e.angle_keyframes.is_empty());
        assert!(e.lighting_keyframes.is_empty());
        assert_eq!(e.depth, 12.0);
        assert_eq!(e.lighting, 0.6);
    }

    /// TextBox prefix router lands `extrusion.*` on TextExtrusion's
    /// vecs and returns None for unknown extrusion subkeys.
    #[test]
    fn textbox_router_dispatches_extrusion_param_keys() {
        use klipp_core::title_effect_keys;

        let mut tb = TextBox::default();
        // No extrusion yet: every extrusion.* key returns None.
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::EXTRUSION_DEPTH).is_none());

        tb.extrusion = Some(TextExtrusion {
            enabled: true,
            face_color: [255, 255, 255, 255],
            side_color: [80, 80, 80, 255],
            depth: 10.0, depth_keyframes: Vec::new(),
            angle: 135.0, angle_keyframes: Vec::new(),
            lighting: 0.5, lighting_keyframes: Vec::new(),
        });
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::EXTRUSION_DEPTH).unwrap(),
            0.0, 20.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::EXTRUSION_ANGLE).unwrap(),
            0.0, 90.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::EXTRUSION_LIGHTING).unwrap(),
            0.0, 0.8,
        );
        let e = tb.extrusion.as_ref().unwrap();
        assert_eq!(e.depth_keyframes[0].value, 20.0);
        assert_eq!(e.angle_keyframes[0].value, 90.0);
        assert_eq!(e.lighting_keyframes[0].value, 0.8);

        // Unknown extrusion key returns None.
        assert!(tb.effect_keyframes_mut_for("extrusion.skew").is_none());
    }

    /// `TextOutline::evaluated_width` short-circuits on empty kfs and
    /// interpolates when populated. Each TextOutline in a stack carries
    /// its own keyframe vec — independence between stack slots holds.
    #[test]
    fn textoutline_evaluator_static_and_interpolated() {
        use klipp_core::{Interpolation, Keyframe};

        let mut o = TextOutline {
            enabled: true,
            color: [0, 0, 0, 255],
            width: 2.0,
            width_keyframes: Vec::new(),
        };
        assert_eq!(o.evaluated_width(0.0), 2.0);

        o.width_keyframes = vec![
            Keyframe {
                time_secs: 0.0, value: 1.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0, value: 9.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
        ];
        assert!((o.evaluated_width(0.5) - 5.0).abs() < 1e-5);
    }

    /// Backward-compat: pre-#5 TextOutline JSON without the keyframe
    /// field deserialises with empty vec.
    #[test]
    fn legacy_textoutline_without_keyframe_field_loads_with_default() {
        let legacy_json = r#"{
            "enabled": true,
            "color": [0, 0, 0, 255],
            "width": 3.0
        }"#;
        let o: TextOutline = serde_json::from_str(legacy_json)
            .expect("legacy save without width_keyframes must deserialise");
        assert!(o.width_keyframes.is_empty());
        assert_eq!(o.width, 3.0);
    }

    /// TextBox prefix router lands `outline.{idx}.width` on the
    /// matching `outlines[idx].width_keyframes` slot. Out-of-range
    /// indices return None. Stacked outlines are independent.
    #[test]
    fn textbox_router_dispatches_indexed_outline_param_keys() {
        use klipp_core::title_effect_keys;

        let mut tb = TextBox::default();
        // Push two outline layers with width_keyframes empty.
        tb.outlines.push(TextOutline {
            enabled: true, color: [255, 0, 0, 255], width: 2.0,
            width_keyframes: Vec::new(),
        });
        tb.outlines.push(TextOutline {
            enabled: true, color: [0, 0, 255, 255], width: 4.0,
            width_keyframes: Vec::new(),
        });

        // Index-0 keyframe lands on outlines[0] only.
        let key0 = title_effect_keys::outline_width_key(0);
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key0).unwrap(),
            0.0, 8.0,
        );
        assert_eq!(tb.outlines[0].width_keyframes.len(), 1);
        assert_eq!(tb.outlines[1].width_keyframes.len(), 0);

        // Index-1 keyframe lands on outlines[1] only — independence.
        let key1 = title_effect_keys::outline_width_key(1);
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key1).unwrap(),
            0.0, 12.0,
        );
        assert_eq!(tb.outlines[0].width_keyframes.len(), 1);
        assert_eq!(tb.outlines[1].width_keyframes.len(), 1);
        assert_eq!(tb.outlines[0].width_keyframes[0].value, 8.0);
        assert_eq!(tb.outlines[1].width_keyframes[0].value, 12.0);

        // Out-of-range index returns None.
        let key99 = title_effect_keys::outline_width_key(99);
        assert!(tb.effect_keyframes_mut_for(&key99).is_none());

        // Unknown subfield returns None even with valid index.
        assert!(tb.effect_keyframes_mut_for("outline.0.color").is_none());
    }

    /// `CharAnimation::evaluated_*` short-circuits on empty kfs and
    /// interpolates when populated. Backward-compat: legacy JSON
    /// without keyframe fields deserialises with empty vecs.
    #[test]
    fn charanimation_evaluator_and_legacy_load() {
        use klipp_core::{Interpolation, Keyframe};

        let mut c = CharAnimation {
            enabled: true,
            entrance: CharEntrance::FadeIn,
            stagger: 0.05, stagger_keyframes: Vec::new(),
            wave_amplitude: 0.0, wave_amplitude_keyframes: Vec::new(),
            wave_speed: 1.0, wave_speed_keyframes: Vec::new(),
        };
        // Empty: returns static.
        assert_eq!(c.evaluated_stagger(0.5), 0.05);
        assert_eq!(c.evaluated_wave_amplitude(99.0), 0.0);
        assert_eq!(c.evaluated_wave_speed(0.0), 1.0);

        // Keyframe stagger: ramps, others stay static.
        c.stagger_keyframes = vec![
            Keyframe {
                time_secs: 0.0, value: 0.02,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0, value: 0.10,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
        ];
        assert!((c.evaluated_stagger(0.5) - 0.06).abs() < 1e-5);
        assert_eq!(c.evaluated_wave_amplitude(0.5), 0.0);

        // Backward-compat: pre-#5 JSON without the keyframe vecs.
        let legacy_json = r#"{
            "enabled": true,
            "entrance": "FadeIn",
            "stagger": 0.05,
            "wave_amplitude": 0.0,
            "wave_speed": 1.0
        }"#;
        let c2: CharAnimation = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe fields must deserialise");
        assert!(c2.stagger_keyframes.is_empty());
        assert!(c2.wave_amplitude_keyframes.is_empty());
        assert!(c2.wave_speed_keyframes.is_empty());
        assert_eq!(c2.stagger, 0.05);
    }

    /// `MotionBlur::evaluated_shutter_angle` short-circuits on empty
    /// kfs and interpolates when populated. Backward-compat: legacy
    /// JSON without the keyframe field deserialises with empty vec.
    #[test]
    fn motionblur_evaluator_and_legacy_load() {
        use klipp_core::{Interpolation, Keyframe};

        let mut mb = MotionBlur {
            enabled: true,
            shutter_angle: 180.0,
            shutter_angle_keyframes: Vec::new(),
            samples: 8,
        };
        assert_eq!(mb.evaluated_shutter_angle(0.0), 180.0);
        mb.shutter_angle_keyframes = vec![
            Keyframe {
                time_secs: 0.0, value: 90.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
            Keyframe {
                time_secs: 1.0, value: 270.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            },
        ];
        assert!((mb.evaluated_shutter_angle(0.5) - 180.0).abs() < 1e-5);

        let legacy_json = r#"{
            "enabled": true,
            "shutter_angle": 180.0,
            "samples": 8
        }"#;
        let m: MotionBlur = serde_json::from_str(legacy_json)
            .expect("legacy save without keyframe field must deserialise");
        assert!(m.shutter_angle_keyframes.is_empty());
        assert_eq!(m.shutter_angle, 180.0);
    }

    /// TextBox prefix router lands `inner_shadow.*` on InnerShadow's
    /// vecs WITHOUT bleeding into TitleShadow's `shadow.*` arm. The
    /// dispatcher must prefer the longer `inner_shadow.` prefix
    /// because `shadow.` would otherwise match it.
    #[test]
    fn textbox_router_dispatches_inner_shadow_param_keys_distinctly() {
        use klipp_core::title_effect_keys;

        let mut tb = TextBox::default();
        // No inner shadow yet: inner_shadow.* returns None.
        assert!(tb
            .effect_keyframes_mut_for(title_effect_keys::INNER_SHADOW_BLUR)
            .is_none());

        tb.inner_shadow = Some(InnerShadow {
            enabled: true,
            color: [0, 0, 0, 180],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 3.0, offset_y_keyframes: Vec::new(),
            blur: 4.0, blur_keyframes: Vec::new(),
        });
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(title_effect_keys::INNER_SHADOW_OFFSET_X)
                .unwrap(),
            0.0, 5.0,
        );
        // inner_shadow.* lands on the right vec.
        assert_eq!(
            tb.inner_shadow.as_ref().unwrap().offset_x_keyframes.len(),
            1,
        );
        // The (separate) shadow path is still empty — no bleed.
        assert!(tb.shadow.is_none());
        // shadow.* without a TitleShadow returns None and does NOT
        // accidentally hit the InnerShadow vec.
        assert!(tb
            .effect_keyframes_mut_for(title_effect_keys::SHADOW_BLUR)
            .is_none());
    }
}
