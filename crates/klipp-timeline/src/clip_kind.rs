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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ObjectTransform {
    /// Normalised centre of the object's bounding box.
    pub center: TitlePosition,
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
    /// Image file used as text fill.
    #[serde(default)]
    pub texture_fill_path: Option<String>,
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
            tracking: 0.0,
            line_spacing: 1.0,
            write_on: 1.0,
            extrusion: None,
            text_path: None,
            char_animation: None,
            texture_fill_path: None,
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
        self.primary_text().and_then(|tb| tb.shadow)
    }
    pub fn outlines(&self) -> Vec<TextOutline> {
        self.primary_text().map(|tb| tb.outlines.clone()).unwrap_or_default()
    }
    pub fn glow(&self) -> Option<TextGlow> {
        self.primary_text().and_then(|tb| tb.glow)
    }
    pub fn gradient(&self) -> Option<TextGradient> {
        self.primary_text().and_then(|tb| tb.gradient.clone())
    }
    pub fn inner_shadow(&self) -> Option<InnerShadow> {
        self.primary_text().and_then(|tb| tb.inner_shadow)
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
        self.primary_text().and_then(|tb| tb.extrusion)
    }
    pub fn text_path(&self) -> Option<TextPath> {
        self.primary_text().and_then(|tb| tb.text_path.clone())
    }
    pub fn char_animation(&self) -> Option<CharAnimation> {
        self.primary_text().and_then(|tb| tb.char_animation)
    }
    pub fn texture_fill_path(&self) -> Option<String> {
        self.primary_text().and_then(|tb| tb.texture_fill_path.clone())
    }
    pub fn motion_blur(&self) -> Option<MotionBlur> {
        self.primary_text().and_then(|tb| tb.motion_blur)
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct MotionBlur {
    /// Shutter angle in degrees (0-360, default 180 = half-frame blur)
    pub shutter_angle: f32,
    /// Number of samples (more = smoother but slower, 4-16 typical)
    pub samples: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TextExtrusion {
    /// Extrusion distance in pixels (1-50)
    pub depth: f32,
    /// Direction in degrees (default 135 = down-right shadow direction)
    pub angle: f32,
    /// Front face color (if [0,0,0,0] uses title.color)
    pub face_color: [u8; 4],
    /// Extruded side color
    pub side_color: [u8; 4],
    /// 0.0-1.0, simulated directional lighting
    pub lighting: f32,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CharAnimation {
    /// Seconds between each character's animation start
    pub stagger: f32,
    /// Type of per-character entrance
    pub entrance: CharEntrance,
    /// Vertical wave oscillation amplitude (0 = no wave)
    pub wave_amplitude: f32,
    /// Wave speed in cycles per second
    pub wave_speed: f32,
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
}
