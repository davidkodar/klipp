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
    /// OpenType features the run's text is shaped with, after the text
    /// box's own (#1501): "smcp" turns one on, "-liga" off, "ss01=1"
    /// sets a value. Empty (the default, left unwritten) adds none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Underline for this run alone (if None, inherits the text box's).
    /// Kept and saved, so a subtitle underlined in part keeps it through
    /// export (#1515), and drawn under the run's glyphs (#1668).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    /// The weight this run is set in, 100 to 900 (#1504); if None, the
    /// text box's, unless the run's `font_style` makes it bold or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    /// A variable font's axes for this run, each set on top of the
    /// text box's own (#1504). Empty (the default, left unwritten) sets
    /// none of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_axes: Vec<FontAxis>,
    /// A reading gloss (ruby, furigana) set small beside the run's text:
    /// above it in a horizontal box, to its right in a vertical one
    /// (#1704). None (the default, left unwritten) for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruby: Option<String>,
    /// Set as a superscript or subscript, as in x² or H₂O (#1721): in
    /// the face's own superior or inferior figures where it has them,
    /// else smaller and raised or lowered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<Script>,
}

/// A run raised or lowered small (#1721).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Script {
    Superscript,
    Subscript,
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
            features: Vec::new(),
            underline: None,
            font_weight: None,
            font_axes: Vec::new(),
            ruby: None,
            script: None,
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
            && self.features.is_empty()
            && self.underline.is_none()
            && self.font_weight.is_none()
            && self.font_axes.is_empty()
            && self.ruby.is_none()
            && self.script.is_none()
    }

    /// Returns true if two runs have identical style fields (ignoring range).
    pub fn same_style(&self, other: &StyleRun) -> bool {
        self.font_name == other.font_name
            && self.font_size == other.font_size
            && self.color == other.color
            && self.font_style == other.font_style
            && self.tracking == other.tracking
            && self.baseline_offset == other.baseline_offset
            && self.features == other.features
            && self.underline == other.underline
            && self.font_weight == other.font_weight
            && self.font_axes == other.font_axes
            && self.ruby == other.ruby
            && self.script == other.script
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
    Sequence {
        sequence_id: klipp_core::SequenceId,
    },
    /// A procedurally-generated frame source (solid color, gradient, bars, countdown).
    Generator(GeneratorData),
    /// Adjustment layer — transparent clip that applies its effects to all clips below.
    AdjustmentLayer,
    /// A sequence composed in isolation and laid in as one picture. Its
    /// tracks, camera and masks resolve on a transparent surface of their
    /// own, in the working space; the clip's transform, opacity, grade and
    /// effects then apply once to that surface, and the parent camera and
    /// shutter see one layer. The editorial compound above (`Sequence`) is
    /// flattened into the parent instead — right for an edit, wrong for a
    /// scene that must move, fade or blur as one.
    ///
    /// A kind of its own, appended after every earlier one so persisted
    /// files never reorder, rather than a flag on `Sequence`: a reader
    /// without it must refuse the file by name, not flatten the scene into
    /// the wrong picture. Child identity at render time is scoped to the
    /// instance, so one scene can be laid in twice.
    Precomposition {
        sequence_id: klipp_core::SequenceId,
    },
    /// A precomposition carrying editable template data. This separate tag
    /// makes older readers refuse the file instead of silently dropping the
    /// instance and rendering a different card.
    TemplatePrecomposition {
        sequence_id: klipp_core::SequenceId,
        template_instance: klipp_core::TemplateInstance,
    },
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
            | ClipKind::Sequence { .. }
            | ClipKind::Precomposition { .. }
            | ClipKind::TemplatePrecomposition { .. } => true,
            ClipKind::Gap | ClipKind::AdjustmentLayer => false,
        }
    }

    /// Returns true if this clip kind needs a decoder to produce frames.
    /// Media clips and nested sequences go through decoders. Synthetic clips
    /// (Title, Generator) produce frames directly without one.
    pub fn needs_decoder(&self) -> bool {
        match self {
            // A precomposition's inner media is decoded like a compound's.
            ClipKind::Media { .. } | ClipKind::Sequence { .. } | ClipKind::Precomposition { .. } | ClipKind::TemplatePrecomposition { .. } => true,
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
    #[serde(default)]
    pub size_x_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub size_y_keyframes: Vec<klipp_core::Keyframe>,
    /// Normalised width / height of the bounding box (1.0 = full
    /// frame extent). `width = 1.0, height = 0.2` is a full-frame-
    /// width × 20%-frame-height bar — a typical lower-third bar.
    pub size: TitlePosition,
    /// Rotation in degrees, positive = clockwise.
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub rotation_keyframes: Vec<klipp_core::Keyframe>,
    /// Anchor point inside the bounding box (normalised 0..1 within
    /// the box). 0.5,0.5 = centre (default). Drives rotation pivot
    /// and resize behaviour.
    #[serde(default = "default_anchor_norm")]
    pub anchor_norm: TitlePosition,
    #[serde(default)]
    pub anchor_x_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub anchor_y_keyframes: Vec<klipp_core::Keyframe>,
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

    /// Returns the complete transform sampled at a clip-local time.
    pub fn evaluated_at(&self, time_secs: f64) -> Self {
        let mut out = self.clone();
        out.center = self.evaluated_center(time_secs);
        out.size.x = self.evaluated_property(klipp_core::TitleTransformProperty::Width, time_secs);
        out.size.y = self.evaluated_property(klipp_core::TitleTransformProperty::Height, time_secs);
        out.rotation = self.evaluated_property(klipp_core::TitleTransformProperty::Rotation, time_secs);
        out.anchor_norm.x = self.evaluated_property(klipp_core::TitleTransformProperty::AnchorX, time_secs);
        out.anchor_norm.y = self.evaluated_property(klipp_core::TitleTransformProperty::AnchorY, time_secs);
        out
    }

    pub fn transform_keyframes_mut(&mut self, property: klipp_core::TitleTransformProperty) -> &mut Vec<klipp_core::Keyframe> {
        match property {
            klipp_core::TitleTransformProperty::Width => &mut self.size_x_keyframes,
            klipp_core::TitleTransformProperty::Height => &mut self.size_y_keyframes,
            klipp_core::TitleTransformProperty::Rotation => &mut self.rotation_keyframes,
            klipp_core::TitleTransformProperty::AnchorX => &mut self.anchor_x_keyframes,
            klipp_core::TitleTransformProperty::AnchorY => &mut self.anchor_y_keyframes,
        }
    }

    pub fn transform_keyframes_ref(&self, property: klipp_core::TitleTransformProperty) -> &Vec<klipp_core::Keyframe> {
        match property {
            klipp_core::TitleTransformProperty::Width => &self.size_x_keyframes,
            klipp_core::TitleTransformProperty::Height => &self.size_y_keyframes,
            klipp_core::TitleTransformProperty::Rotation => &self.rotation_keyframes,
            klipp_core::TitleTransformProperty::AnchorX => &self.anchor_x_keyframes,
            klipp_core::TitleTransformProperty::AnchorY => &self.anchor_y_keyframes,
        }
    }

    pub fn evaluated_property(&self, property: klipp_core::TitleTransformProperty, time_secs: f64) -> f32 {
        let (keys, fallback) = match property {
            klipp_core::TitleTransformProperty::Width => (&self.size_x_keyframes, self.size.x),
            klipp_core::TitleTransformProperty::Height => (&self.size_y_keyframes, self.size.y),
            klipp_core::TitleTransformProperty::Rotation => (&self.rotation_keyframes, self.rotation),
            klipp_core::TitleTransformProperty::AnchorX => (&self.anchor_x_keyframes, self.anchor_norm.x),
            klipp_core::TitleTransformProperty::AnchorY => (&self.anchor_y_keyframes, self.anchor_norm.y),
        };
        if keys.is_empty() { fallback } else { klipp_core::keyframe::evaluate(keys, time_secs, fallback) }
    }

    pub fn upsert_transform_keyframe(&mut self, property: klipp_core::TitleTransformProperty, time_secs: f64, value: f32) {
        klipp_core::keyframe::upsert_keyframe(self.transform_keyframes_mut(property), time_secs, value);
    }

    pub fn remove_transform_keyframe_at(&mut self, property: klipp_core::TitleTransformProperty, time_secs: f64) {
        klipp_core::keyframe::remove_keyframe_at(self.transform_keyframes_mut(property), time_secs);
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

    /// Insert (or replace at the same timestamp) a keyframe at `time_secs`.
    /// Keeps the vec sorted by `time_secs` so the renderer's
    /// `evaluate(...)` walk stays correct.
    pub fn upsert_position_keyframe(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        time_secs: f64,
        value: f32,
    ) {
        klipp_core::keyframe::upsert_keyframe(self.position_keyframes_mut(axis), time_secs, value);
    }

    /// Remove keyframe(s) at `time_secs` using the shared timestamp tolerance. No-op if none
    /// match; matches the NLE-standard "click diamond again to delete"
    /// affordance.
    pub fn remove_position_keyframe_at(
        &mut self,
        axis: klipp_core::TitlePositionAxis,
        time_secs: f64,
    ) {
        klipp_core::keyframe::remove_keyframe_at(self.position_keyframes_mut(axis), time_secs);
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
    /// Optional per-layer opacity animation, evaluated in clip-local time.
    /// Empty by default so existing title files remain unchanged.
    #[serde(default)]
    pub opacity_keyframes: Vec<klipp_core::Keyframe>,
    pub content: TitleObjectContent,
    /// The layer's motion and effect stacks. `None` only in a file
    /// written before this field existed: the loader migrates it to
    /// `Some` (see [`TitleData::migrate_fx`]) before anything renders,
    /// and the renderer reads it through [`TitleObject::fx`] and
    /// nowhere else. `Some(LayerFx::default())` is an explicitly empty
    /// stack and is written as such.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx: Option<LayerFx>,
    /// How the layer mixes with the layers below it in the title
    /// (#1513). Normal is the plain "over" every title has always had.
    #[serde(default = "normal_blend", skip_serializing_if = "is_normal_blend")]
    pub blend: klipp_core::BlendMode,
    /// Another layer of the title whose alpha or brightness is how much
    /// of this one shows (#1513). The source is a matte while any shown
    /// layer uses it: it masks and is not drawn itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<LayerMask>,
}

fn normal_blend() -> klipp_core::BlendMode {
    klipp_core::BlendMode::Normal
}
fn is_normal_blend(b: &klipp_core::BlendMode) -> bool {
    *b == klipp_core::BlendMode::Normal
}

/// A layer's mask: which layer of the same title masks it, and how
/// (#1513).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayerMask {
    pub source: TitleObjectId,
    #[serde(default)]
    pub kind: klipp_core::layer_mask::LayerMaskKind,
}

/// What every object can carry, because it is the object that moves
/// and glows, not its text. A text box carried these alone until now;
/// a shape or a picture could be moved and nothing else. The glyph
/// machinery is unchanged — a text layer's `motion` still staggers per
/// character, word or line — and a non-text layer takes the pose as a
/// whole, arriving after its neighbours by `Motion::delay_secs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerFx {
    /// Entrance and exit motion.
    #[serde(default, skip_serializing_if = "motion_is_still")]
    pub motion: TitleMotion,
    /// What the layer keeps doing while on screen: blink, pulse,
    /// shake, wave, colour cycle, scroll, crawl, counter. All of them
    /// run at once, on top of the entrance and exit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<TitleLoop>,
    /// Glow stack, rendered front-to-back so the lowest index is on top.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub glows: Vec<TextGlow>,
    /// Soft-disable for the secondary glow stack (`glows[1..]`): the
    /// primary is gated by its own `enabled`. The data is preserved
    /// across re-enable.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub glow_stack_enabled: bool,
    /// Drop shadow layers, front-to-back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadows: Vec<TitleShadow>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub shadow_stack_enabled: bool,
    /// Outline/stroke layers, rendered widest-first, back-to-front.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<TextOutline>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub outline_stack_enabled: bool,
    /// Inner shadow stack, front-to-back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inner_shadows: Vec<InnerShadow>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub inner_shadow_stack_enabled: bool,
    /// Motion blur (`None` = off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<MotionBlur>,
    /// The layer fading out towards the frame's edges, by where it is on
    /// screen, still or moving (#592).
    #[serde(default, skip_serializing_if = "EdgeFade::is_none")]
    pub edge_fade: EdgeFade,
    /// Animators: ranges of the text that move, turn, fade or change
    /// colour unit by unit, on top of the entrance and exit (#1510).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animators: Vec<klipp_core::title_motion::Animator>,
}

fn motion_is_still(m: &TitleMotion) -> bool {
    !m.is_active()
}
fn is_true(b: &bool) -> bool {
    *b
}

/// A still layer with nothing on it: what an object without an
/// authored stack reads as, so a reader never has to ask twice.
static STILL_FX: LayerFx = LayerFx {
    motion: TitleMotion { enter: None, exit: None },
    loops: Vec::new(),
    glows: Vec::new(),
    glow_stack_enabled: true,
    shadows: Vec::new(),
    shadow_stack_enabled: true,
    outlines: Vec::new(),
    outline_stack_enabled: true,
    inner_shadows: Vec::new(),
    inner_shadow_stack_enabled: true,
    motion_blur: None,
    edge_fade: EdgeFade::NONE,
    animators: Vec::new(),
};

/// A fresh layer's stack is the still one: nothing on it, every
/// secondary stack enabled — the flags default to on, as they always
/// did, so a derived `Default` would be the wrong stack.
impl Default for LayerFx {
    fn default() -> Self {
        STILL_FX.clone()
    }
}

impl LayerFx {
    /// Nothing authored: the stack a fresh layer has.
    pub fn is_default(&self) -> bool {
        *self == STILL_FX
    }

    /// Whether any glow, drop shadow, outline or inner shadow has a
    /// keyed value, so the styles change over time. Each style is taken
    /// apart field by field: a new field does not compile here until it
    /// is said to be keyed or not (#2335).
    pub fn styles_keyed(&self) -> bool {
        let glow = |g: &TextGlow| {
            let TextGlow { enabled: _, color: _, radius: _, radius_keyframes, intensity: _, intensity_keyframes } = g;
            !radius_keyframes.is_empty() || !intensity_keyframes.is_empty()
        };
        let shadow = |s: &TitleShadow| {
            let TitleShadow { enabled: _, color: _, offset_x: _, offset_x_keyframes, offset_y: _, offset_y_keyframes, blur: _, blur_keyframes } = s;
            !offset_x_keyframes.is_empty() || !offset_y_keyframes.is_empty() || !blur_keyframes.is_empty()
        };
        let outline = |o: &TextOutline| {
            let TextOutline { enabled: _, color: _, width: _, width_keyframes } = o;
            !width_keyframes.is_empty()
        };
        let inner = |s: &InnerShadow| {
            let InnerShadow { enabled: _, color: _, offset_x: _, offset_x_keyframes, offset_y: _, offset_y_keyframes, blur: _, blur_keyframes } = s;
            !offset_x_keyframes.is_empty() || !offset_y_keyframes.is_empty() || !blur_keyframes.is_empty()
        };
        self.glows.iter().any(glow) || self.shadows.iter().any(shadow) || self.outlines.iter().any(outline) || self.inner_shadows.iter().any(inner)
    }
}

impl TitleObject {
    /// The layer's stack — the one place the renderer reads it from.
    /// An object from before the field existed reads as still until
    /// the loader migrates it.
    pub fn fx(&self) -> &LayerFx {
        self.fx.as_ref().unwrap_or(&STILL_FX)
    }

    /// Evaluate this layer's opacity at clip-local time.
    pub fn evaluated_opacity(&self, time_secs: f64) -> f32 {
        if self.opacity_keyframes.is_empty() {
            self.opacity
        } else {
            klipp_core::keyframe::evaluate(&self.opacity_keyframes, time_secs, self.opacity)
        }
    }

    pub fn upsert_opacity_keyframe(&mut self, time_secs: f64, value: f32) {
        klipp_core::keyframe::upsert_keyframe(
            &mut self.opacity_keyframes,
            time_secs,
            value.clamp(0.0, 1.0),
        );
    }

    pub fn remove_opacity_keyframe_at(&mut self, time_secs: f64) {
        klipp_core::keyframe::remove_keyframe_at(&mut self.opacity_keyframes, time_secs);
    }

    /// The layer's stack, to edit: an object without one gets an
    /// explicitly empty one to edit into.
    pub fn fx_mut(&mut self) -> &mut LayerFx {
        self.fx.get_or_insert_with(LayerFx::default)
    }
}

/// The seven effect groups as a text box carried them before they moved
/// to the object — a reader's shape, kept so every file and payload
/// that ever wrote them still reads, and nothing else: it is never
/// written, and the renderer never looks at it. [`TitleData::migrate_fx`]
/// hoists it onto the owning object and empties it; that is the only
/// reader it has. Retained until a supported-format policy retires the
/// old shape, which a release passing does not do.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LegacyTextFx {
    #[serde(default)]
    pub shadows: Vec<TitleShadow>,
    #[serde(default = "default_true")]
    pub shadow_stack_enabled: bool,
    #[serde(default)]
    pub outlines: Vec<TextOutline>,
    #[serde(default = "default_true")]
    pub outline_stack_enabled: bool,
    #[serde(default)]
    pub glows: Vec<TextGlow>,
    #[serde(default = "default_true")]
    pub glow_stack_enabled: bool,
    #[serde(default)]
    pub inner_shadows: Vec<InnerShadow>,
    #[serde(default = "default_true")]
    pub inner_shadow_stack_enabled: bool,
    #[serde(default)]
    pub motion: TitleMotion,
    #[serde(default)]
    pub loops: Vec<TitleLoop>,
    #[serde(default)]
    pub motion_blur: Option<MotionBlur>,
}

/// Empty old fields, with the stack flags on as the old text box had
/// them — what a text box that has been through the loader carries.
impl Default for LegacyTextFx {
    fn default() -> Self {
        Self::default_authored()
    }
}

impl LegacyTextFx {
    /// Nothing in the old fields.
    pub fn is_default(&self) -> bool {
        *self == Self::default_authored()
    }

    fn default_authored() -> Self {
        Self {
            shadows: Vec::new(),
            shadow_stack_enabled: true,
            outlines: Vec::new(),
            outline_stack_enabled: true,
            glows: Vec::new(),
            glow_stack_enabled: true,
            inner_shadows: Vec::new(),
            inner_shadow_stack_enabled: true,
            motion: TitleMotion::default(),
            loops: Vec::new(),
            motion_blur: None,
        }
    }

    /// The old fields as the object's stack.
    pub fn into_fx(self) -> LayerFx {
        LayerFx {
            motion: self.motion,
            loops: self.loops,
            glows: self.glows,
            glow_stack_enabled: self.glow_stack_enabled,
            shadows: self.shadows,
            shadow_stack_enabled: self.shadow_stack_enabled,
            outlines: self.outlines,
            outline_stack_enabled: self.outline_stack_enabled,
            inner_shadows: self.inner_shadows,
            inner_shadow_stack_enabled: self.inner_shadow_stack_enabled,
            motion_blur: self.motion_blur,
            edge_fade: EdgeFade::NONE,
            animators: Vec::new(),
        }
    }
}

fn default_one_f32() -> f32 { 1.0 }
fn default_one_f32_tb() -> f32 { 1.0 }

fn default_true() -> bool { true }

/// What kind of layer an object is. `TextBox` for typography,
/// `Shape` for primitives like the bar in a lower-third. New layer
/// kinds (image, mask, sub-title, etc.) extend this enum without
/// disturbing existing instances.
// TextBox is 696 bytes against Shape's few dozen. A title holds a
// handful of layers, so the waste is kilobytes per title clip; boxing
// would move every text layer behind a pointer in the layout and render
// hot paths to save that. Left flat deliberately.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TitleObjectContent {
    TextBox(TextBox),
    Shape(Shape),
    /// A picture from disk: a logo, a bug, a still.
    Image(ImageLayer),
}

/// How an image layer fills its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImageFit {
    /// Whole picture inside the box, letterboxed.
    #[default]
    Contain,
    /// The box filled, the picture cropped.
    Cover,
    /// The picture stretched to the box.
    Stretch,
}

impl ImageFit {
    pub const ALL: [ImageFit; 3] = [ImageFit::Contain, ImageFit::Cover, ImageFit::Stretch];
    pub fn label(self) -> &'static str {
        match self {
            ImageFit::Contain => "Fit",
            ImageFit::Cover => "Fill",
            ImageFit::Stretch => "Stretch",
        }
    }
}

/// An image layer's content: the file, and how it sits in its box.
/// The layer's `ObjectTransform` places and sizes the box; the
/// layer's `opacity` fades it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageLayer {
    /// Absolute in memory; project-relative in the file.
    pub path: String,
    #[serde(default)]
    pub fit: ImageFit,
}

impl ImageLayer {
    /// The file's name without its extension, for layer lists.
    pub fn stem(&self) -> String {
        std::path::Path::new(&self.path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Image".to_string())
    }
}

/// A text layer's content + style. All the rich-text styling fields
/// (fonts, colour, gradients, outlines, glow, extrusion, path,
/// per-character animation, motion blur, style runs) live here so
/// each text layer is independently stylable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextBox {
    pub text: String,
    /// Font size in points, default 72.0. The renderer scales by the
    /// frame's short side over 1080 (`klipp_title::frame_scale`) so this
    /// stays resolution-stable, and reads the same on a vertical frame.
    pub font_size: f32,
    pub font_name: String,
    #[serde(default)]
    pub font_style: FontStyle,
    pub color: [u8; 4],
    pub alignment: TextAlignment,
    /// The effect stacks and motion a text box carried before they
    /// moved to its object ([`TitleObject::fx`]), as any older file or
    /// payload still writes them. Read only by [`TitleData::migrate_fx`],
    /// which hoists them onto the object; never written, never
    /// rendered. Empty on every text box that has been through the
    /// loader.
    #[serde(flatten, default, skip_serializing)]
    pub legacy_fx: LegacyTextFx,
    /// Gradient fill (overrides solid colour when set).
    #[serde(default)]
    pub gradient: Option<TextGradient>,
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
    /// The size, tracking and line spacing over the clip, in clip-local
    /// time (#1511); empty, the field above holds. Written only when
    /// keyed, so a title without keys saves as it did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_size_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracking_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_spacing_keyframes: Vec<klipp_core::Keyframe>,
    /// The colour over the clip (#1511), a channel each; empty, `color`
    /// holds. Written only when keyed.
    #[serde(default, skip_serializing_if = "ColourKeys::is_empty")]
    pub color_keyframes: ColourKeys,
    /// Write-on reveal: 0.0 = no text, 1.0 = all visible. Animation
    /// overrides this per frame for Typewriter; otherwise 1.0.
    #[serde(default = "default_one")]
    pub write_on: f32,
    /// 3D extrusion (pseudo-depth).
    #[serde(default)]
    pub extrusion: Option<TextExtrusion>,
    /// Real 3D (#1512): the letters as solids, bevelled, turned and lit.
    /// Drawn in place of `extrusion` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub three_d: Option<crate::Text3d>,
    /// A metal face (#2706): gold, chrome… reflecting an environment
    /// through a bevel. `color` is what a frame drawn without a GPU shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<klipp_core::title_material::TitleMaterial>,
    /// Text follows a geometric path (arc / circle / wave).
    #[serde(default)]
    pub text_path: Option<TextPath>,
    /// Legacy per-character entrance animation. Projects that carry
    /// one are migrated into `motion` and `loops` on load
    /// (`TitleData::migrate_legacy_animation`); nothing writes it.
    #[serde(default)]
    pub char_animation: Option<CharAnimation>,
    /// Texture fill — file path or procedural preset, plus the
    /// transform / blend params (scale, offset, rotation, opacity,
    /// tint, tile mode). `None` = no texture fill configured.
    /// Soft-disable lives inside `TextureFill::enabled`.
    #[serde(default)]
    pub texture_fill: Option<TextureFill>,
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
    /// Vertical text's columns from the left (#1507), as Mongolian is
    /// set; `false` (the default, left unwritten) from the right, as
    /// Japanese and Chinese are.
    #[serde(default, skip_serializing_if = "is_default")]
    pub vertical_columns_ltr: bool,
    /// Text direction. `Ltr` (default) = standard left-to-right.
    /// `Rtl` = right-to-left for Hebrew. Limited Arabic support —
    /// proper Arabic shaping (joining forms, lam-alif ligatures, …)
    /// requires harfbuzz integration tracked for a follow-up. RTL
    /// reverses the visual character order and right-aligns the
    /// text by default.
    #[serde(default)]
    pub text_direction: TextDirection,
    /// Where the last line of each paragraph sits when `alignment` is
    /// [`TextAlignment::Justify`]; read by nothing else (#1506). Left
    /// unwritten at its default, so a text box saved before it existed
    /// saves the same.
    #[serde(default, skip_serializing_if = "is_default")]
    pub last_line: LastLineAlign,
    /// Where the text sits up and down its box (#1506). `Middle`, the
    /// default, is where a text box has always set its text: the block
    /// centred on the box's centre. Left unwritten at its default.
    #[serde(default, skip_serializing_if = "is_default")]
    pub vertical_align: TextVerticalAlign,
    /// How the text meets its box when it is too long for it (#1722).
    /// Left unwritten at its default, `Wrap`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub fit: TextFit,
    /// The box's paragraphs: space between them and their indents
    /// (#1721). Left unwritten at its default, none.
    #[serde(default, skip_serializing_if = "is_default")]
    pub paragraph: Paragraph,
    /// OpenType features the box's text is shaped with, on top of the
    /// shaper's defaults (kerning, ligatures, contextual alternates)
    /// (#1501): "smcp" turns one on, "-liga" off, "ss01=1" sets a value.
    /// Empty (the default) is left unwritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// The weight the text is set in, 100 (thin) to 900 (black), when
    /// the box names one (#1504): the family's face nearest it, picked
    /// as CSS picks one, with a variable face's weight axis set to it.
    /// `None` (the default, left unwritten) sets the weight by
    /// `font_style` alone, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    /// A variable font's axes the text is set at (#1504): "wght",
    /// "wdth", "slnt", "opsz" or any other the face has, each in the
    /// axis's own units. Empty (the default, left unwritten) sets the
    /// face's default instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub font_axes: Vec<FontAxis>,
}

/// One axis of a variable font set to a value (#1504): the axis's
/// four-letter tag and its value in the axis's own units.
pub type FontAxis = (String, f32);

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl LayerFx {
    /// Mutable accessor for the effect-keyframe vec named by
    /// `param_key`, for the params that live on the stack: `glow.*`,
    /// `shadow.*`, `inner_shadow.*`, `outline.*`, `motion_blur.*`.
    /// `None` for a param that is not the stack's — see
    /// [`TitleObject::effect_keyframes_mut_for`] for the whole object.
    pub fn effect_keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        // Note: check `inner_shadow.` before `shadow.` — the latter
        // would otherwise prefix-match the former.
        if let Some((idx, field)) = klipp_core::title_effect_keys::parse_animator_param_key(param_key) {
            let a = self.animators.get_mut(idx)?;
            // `within.start` and so on are the limiting selector's.
            let (selector, field) = match field.strip_prefix("within.") {
                Some(f) => (a.within.as_mut()?, f),
                None => (&mut a.selector, field),
            };
            match field {
                "start" => Some(&mut selector.start_keyframes),
                "end" => Some(&mut selector.end_keyframes),
                "offset" => Some(&mut selector.offset_keyframes),
                _ => None,
            }
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_glow_param_key(param_key) {
            // Per-stack-element glow keyframes (`glow.{idx}.field`).
            let layer = self.glows.get_mut(idx)?;
            match field {
                "radius" => Some(&mut layer.radius_keyframes),
                "intensity" => Some(&mut layer.intensity_keyframes),
                _ => None,
            }
        } else if param_key.starts_with("glow.") {
            // Legacy unindexed glow keys → glows[0].
            self.glows.get_mut(0).and_then(|g| g.keyframes_mut_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_inner_shadow_param_key(param_key) {
            let layer = self.inner_shadows.get_mut(idx)?;
            match field {
                "offset_x" => Some(&mut layer.offset_x_keyframes),
                "offset_y" => Some(&mut layer.offset_y_keyframes),
                "blur" => Some(&mut layer.blur_keyframes),
                _ => None,
            }
        } else if param_key.starts_with("inner_shadow.") {
            self.inner_shadows.get_mut(0).and_then(|s| s.keyframes_mut_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_shadow_param_key(param_key) {
            let layer = self.shadows.get_mut(idx)?;
            match field {
                "offset_x" => Some(&mut layer.offset_x_keyframes),
                "offset_y" => Some(&mut layer.offset_y_keyframes),
                "blur" => Some(&mut layer.blur_keyframes),
                _ => None,
            }
        } else if param_key.starts_with("shadow.") {
            // Legacy unindexed shadow keys always route to shadows[0]
            // so old projects keep working.
            self.shadows.get_mut(0).and_then(|s| s.keyframes_mut_for(param_key))
        } else if param_key.starts_with("motion_blur.") {
            self.motion_blur.as_mut().and_then(|m| m.keyframes_mut_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_outline_param_key(param_key) {
            match field {
                "width" => self.outlines.get_mut(idx).map(|o| &mut o.width_keyframes),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Read-only mirror of `effect_keyframes_mut_for`.
    pub fn effect_keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        if let Some((idx, field)) = klipp_core::title_effect_keys::parse_animator_param_key(param_key) {
            let a = self.animators.get(idx)?;
            // `within.start` and so on are the limiting selector's.
            let (selector, field) = match field.strip_prefix("within.") {
                Some(f) => (a.within.as_ref()?, f),
                None => (&a.selector, field),
            };
            match field {
                "start" => Some(&selector.start_keyframes),
                "end" => Some(&selector.end_keyframes),
                "offset" => Some(&selector.offset_keyframes),
                _ => None,
            }
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_glow_param_key(param_key) {
            let layer = self.glows.get(idx)?;
            match field {
                "radius" => Some(layer.radius_keyframes.as_slice()),
                "intensity" => Some(layer.intensity_keyframes.as_slice()),
                _ => None,
            }
        } else if param_key.starts_with("glow.") {
            self.glows.first().and_then(|g| g.keyframes_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_inner_shadow_param_key(param_key) {
            let layer = self.inner_shadows.get(idx)?;
            match field {
                "offset_x" => Some(layer.offset_x_keyframes.as_slice()),
                "offset_y" => Some(layer.offset_y_keyframes.as_slice()),
                "blur" => Some(layer.blur_keyframes.as_slice()),
                _ => None,
            }
        } else if param_key.starts_with("inner_shadow.") {
            self.inner_shadows.first().and_then(|s| s.keyframes_for(param_key))
        } else if let Some((idx, field)) = klipp_core::title_effect_keys::parse_shadow_param_key(param_key) {
            let layer = self.shadows.get(idx)?;
            match field {
                "offset_x" => Some(layer.offset_x_keyframes.as_slice()),
                "offset_y" => Some(layer.offset_y_keyframes.as_slice()),
                "blur" => Some(layer.blur_keyframes.as_slice()),
                _ => None,
            }
        } else if param_key.starts_with("shadow.") {
            self.shadows.first().and_then(|s| s.keyframes_for(param_key))
        } else if param_key.starts_with("motion_blur.") {
            self.motion_blur.as_ref().and_then(|m| m.keyframes_for(param_key))
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

impl TextBox {
    /// About how large the text is at `font_px`, (width, height), as a
    /// travelling loop measures it: the longest line at a bit over half
    /// an em a character, the lines at their spacing. The renderer and
    /// the inspector's roll speed both read it, so the time the Title
    /// tab shows is the time the roll takes.
    pub fn loop_extent(&self, font_px: f32) -> (f32, f32) {
        let lines = self.text.lines().count().max(1) as f32;
        let max_chars = self.text.lines().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as f32;
        (max_chars * font_px * 0.55, lines * font_px * self.line_spacing.max(0.5) * 1.4)
    }

    /// Whether the size, tracking, line spacing or colour is keyed
    /// (#1511).
    pub fn has_keyframes(&self) -> bool {
        !self.font_size_keyframes.is_empty() || !self.tracking_keyframes.is_empty() || !self.line_spacing_keyframes.is_empty() || !self.color_keyframes.is_empty()
            || self.gradient.as_ref().is_some_and(TextGradient::has_keyframes)
            || self.three_d.as_ref().is_some_and(|t| !t.keys.is_empty())
    }

    /// The text at `time_secs` of its clip: its keyed size, tracking,
    /// line spacing and colour as they are then, its keys gone (#1511).
    pub fn at(&self, time_secs: f64) -> TextBox {
        let mut tb = self.clone();
        let key = |keys: &mut Vec<klipp_core::Keyframe>, value: &mut f32| {
            if !keys.is_empty() {
                *value = klipp_core::keyframe::evaluate(keys, time_secs, *value);
                keys.clear();
            }
        };
        let (mut size, mut tracking, mut spacing) = (tb.font_size, tb.tracking, tb.line_spacing);
        key(&mut tb.font_size_keyframes, &mut size);
        key(&mut tb.tracking_keyframes, &mut tracking);
        key(&mut tb.line_spacing_keyframes, &mut spacing);
        (tb.font_size, tb.tracking, tb.line_spacing) = (size.max(1.0), tracking, spacing.max(0.1));
        if !tb.color_keyframes.is_empty() {
            tb.color = tb.color_keyframes.at(time_secs, tb.color);
            tb.color_keyframes = ColourKeys::default();
        }
        if let Some(g) = tb.gradient.as_mut().filter(|g| g.has_keyframes()) {
            *g = g.at(time_secs);
        }
        if let Some(t) = tb.three_d.as_mut().filter(|t| !t.keys.is_empty()) {
            *t = t.at(time_secs);
        }
        if tb.paragraph != Paragraph::default() {
            tb.paragraph = tb.paragraph.at(time_secs);
        }
        tb
    }

    /// Mutable accessor for the keyframe vec of a param that is the
    /// text's own — `text.*`, `extrusion.*`, `char_anim.*`. The stack's
    /// params are the object's: [`TitleObject::effect_keyframes_mut_for`].
    pub fn effect_keyframes_mut_for(
        &mut self,
        param_key: &str,
    ) -> Option<&mut Vec<klipp_core::Keyframe>> {
        use klipp_core::title_effect_keys as k;
        match param_key {
            k::TEXT_SIZE => return Some(&mut self.font_size_keyframes),
            k::TEXT_TRACKING => return Some(&mut self.tracking_keyframes),
            k::TEXT_LINE_SPACING => return Some(&mut self.line_spacing_keyframes),
            k::TEXT_SPACE_BEFORE | k::TEXT_SPACE_AFTER | k::TEXT_INDENT_START | k::TEXT_INDENT_END | k::TEXT_INDENT_FIRST => {
                return self.paragraph.keys_mut_for(param_key);
            }
            k::TEXT_PATH_OFFSET => {
                return match &mut self.text_path {
                    Some(TextPath::Layer { offset_keyframes, .. }) => Some(offset_keyframes),
                    _ => None,
                };
            }
            _ => {}
        }
        if let Some(channel) = param_key.strip_prefix("text.color.") {
            return self.color_keyframes.channel_mut(channel);
        }
        if let Some(name) = param_key.strip_prefix("text3d.") {
            return self.three_d.as_mut()?.keys.get_mut(name);
        }
        if let Some((i, channel)) = k::parse_gradient_stop_key(param_key) {
            let g = self.gradient.as_mut()?;
            if i >= g.stops().len() {
                return None;
            }
            if g.stop_keyframes.len() <= i {
                g.stop_keyframes.resize(i + 1, ColourKeys::default());
            }
            return g.stop_keyframes[i].channel_mut(channel);
        }
        if param_key.starts_with("extrusion.") {
            self.extrusion.as_mut().and_then(|e| e.keyframes_mut_for(param_key))
        } else if param_key.starts_with("char_anim.") {
            self.char_animation.as_mut().and_then(|c| c.keyframes_mut_for(param_key))
        } else {
            None
        }
    }

    /// Read-only mirror of `effect_keyframes_mut_for`.
    pub fn effect_keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        use klipp_core::title_effect_keys as k;
        match param_key {
            k::TEXT_SIZE => return Some(&self.font_size_keyframes),
            k::TEXT_TRACKING => return Some(&self.tracking_keyframes),
            k::TEXT_LINE_SPACING => return Some(&self.line_spacing_keyframes),
            k::TEXT_SPACE_BEFORE | k::TEXT_SPACE_AFTER | k::TEXT_INDENT_START | k::TEXT_INDENT_END | k::TEXT_INDENT_FIRST => {
                return self.paragraph.keys_for(param_key);
            }
            k::TEXT_PATH_OFFSET => {
                return match &self.text_path {
                    Some(TextPath::Layer { offset_keyframes, .. }) => Some(offset_keyframes),
                    _ => None,
                };
            }
            _ => {}
        }
        if let Some(channel) = param_key.strip_prefix("text.color.") {
            return self.color_keyframes.channel(channel);
        }
        if let Some(name) = param_key.strip_prefix("text3d.") {
            return self.three_d.as_ref()?.keys.get(name);
        }
        if let Some((i, channel)) = k::parse_gradient_stop_key(param_key) {
            let g = self.gradient.as_ref()?;
            if i >= g.stops().len() {
                return None;
            }
            return match g.stop_keyframes.get(i) {
                Some(keys) => keys.channel(channel),
                None => ["r", "g", "b", "a"].contains(&channel).then_some(&[][..]),
            };
        }
        if param_key.starts_with("extrusion.") {
            self.extrusion.as_ref().and_then(|e| e.keyframes_for(param_key))
        } else if param_key.starts_with("char_anim.") {
            self.char_animation.as_ref().and_then(|c| c.keyframes_for(param_key))
        } else {
            None
        }
    }
}

impl TitleObject {
    /// The keyframe vec of any effect param on this object: the
    /// stack's, or, for a text layer, the text's own.
    pub fn effect_keyframes_mut_for(&mut self, param_key: &str) -> Option<&mut Vec<klipp_core::Keyframe>> {
        // The layer's own opacity (#2301), keyed as the effects are.
        if param_key == klipp_core::title_effect_keys::LAYER_OPACITY {
            return Some(&mut self.opacity_keyframes);
        }
        let Self { content, fx, .. } = self;
        if let Some(keys) = fx.as_mut().and_then(|f| f.effect_keyframes_mut_for(param_key)) {
            return Some(keys);
        }
        match content {
            TitleObjectContent::TextBox(tb) => tb.effect_keyframes_mut_for(param_key),
            _ => None,
        }
    }

    /// Read-only mirror of [`Self::effect_keyframes_mut_for`].
    pub fn effect_keyframes_for(&self, param_key: &str) -> Option<&[klipp_core::Keyframe]> {
        if param_key == klipp_core::title_effect_keys::LAYER_OPACITY {
            return Some(&self.opacity_keyframes);
        }
        if let Some(keys) = self.fx().effect_keyframes_for(param_key) {
            return Some(keys);
        }
        match &self.content {
            TitleObjectContent::TextBox(tb) => tb.effect_keyframes_for(param_key),
            _ => None,
        }
    }
}

fn default_one_u8() -> u8 { 1 }

/// A colour keyed over time (#1511): a keyframe track per channel, red,
/// green and blue in linear light and alpha, each 0..1, so a change of
/// colour blends as light does rather than through the muddier sRGB
/// midpoints.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColourKeys {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub g: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub b: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub a: Vec<klipp_core::Keyframe>,
}

impl ColourKeys {
    pub fn is_empty(&self) -> bool {
        self.r.is_empty() && self.g.is_empty() && self.b.is_empty() && self.a.is_empty()
    }

    /// The track of channel `r`, `g`, `b` or `a`.
    pub fn channel(&self, name: &str) -> Option<&[klipp_core::Keyframe]> {
        Some(match name { "r" => &self.r, "g" => &self.g, "b" => &self.b, "a" => &self.a, _ => return None })
    }

    pub fn channel_mut(&mut self, name: &str) -> Option<&mut Vec<klipp_core::Keyframe>> {
        Some(match name { "r" => &mut self.r, "g" => &mut self.g, "b" => &mut self.b, "a" => &mut self.a, _ => return None })
    }

    /// `color`'s four channel values, as the tracks hold them.
    pub fn values(color: [u8; 4]) -> [f32; 4] {
        [srgb_to_linear(color[0]), srgb_to_linear(color[1]), srgb_to_linear(color[2]), color[3] as f32 / 255.0]
    }

    /// The colour at `time_secs`: each keyed channel as its track has
    /// it then, the others `base`'s.
    pub fn at(&self, time_secs: f64, base: [u8; 4]) -> [u8; 4] {
        let v = Self::values(base);
        let ch = |keys: &[klipp_core::Keyframe], v: f32| klipp_core::keyframe::evaluate(keys, time_secs, v).clamp(0.0, 1.0);
        [
            linear_to_srgb(ch(&self.r, v[0])),
            linear_to_srgb(ch(&self.g, v[1])),
            linear_to_srgb(ch(&self.b, v[2])),
            (ch(&self.a, v[3]) * 255.0).round() as u8,
        ]
    }

    /// Key `color` at `time_secs` on every channel.
    pub fn set(&mut self, time_secs: f64, color: [u8; 4]) {
        let v = Self::values(color);
        for (keys, v) in [&mut self.r, &mut self.g, &mut self.b, &mut self.a].into_iter().zip(v) {
            klipp_core::keyframe::upsert_keyframe(keys, time_secs, v);
        }
    }

    /// Take the key within `tolerance` of `time_secs` from every channel.
    pub fn remove(&mut self, time_secs: f64, tolerance: f64) {
        for keys in [&mut self.r, &mut self.g, &mut self.b, &mut self.a] {
            keys.retain(|k| (k.time_secs - time_secs).abs() > tolerance);
        }
    }
}

/// An sRGB byte in linear light, 0..1.
fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Linear light, 0..1, as an sRGB byte.
fn linear_to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

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
            font_name: String::from("Inter"),
            font_style: FontStyle::Regular,
            color: [255, 255, 255, 255],
            alignment: TextAlignment::Center,
            legacy_fx: LegacyTextFx::default_authored(),
            gradient: None,
            pattern_fill: None,
            pattern_fill_enabled: true,
            tracking: 0.0,
            line_spacing: 1.0,
            font_size_keyframes: Vec::new(),
            tracking_keyframes: Vec::new(),
            line_spacing_keyframes: Vec::new(),
            color_keyframes: ColourKeys::default(),
            write_on: 1.0,
            extrusion: None,
            three_d: None,
            material: None,
            text_path: None,
            char_animation: None,
            texture_fill: None,
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
            vertical_columns_ltr: false,
            text_direction: TextDirection::Ltr,
            last_line: LastLineAlign::Left,
            vertical_align: TextVerticalAlign::Middle,
            fit: TextFit::Wrap,
            paragraph: Paragraph::default(),
            features: Vec::new(),
            font_weight: None,
            font_axes: Vec::new(),
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
    /// Gradient fill over the shape's box (#1509); in place of `fill`
    /// when set and enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<TextGradient>,
    pub stroke: Option<ShapeStroke>,
    /// Fit to the laid-out bounds of a text layer. Missing or hidden
    /// targets suppress the backing rather than painting a stale box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<ShapeFit>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ShapeFit {
    pub target: TitleObjectId,
    /// Horizontal / vertical padding in pixels at 1080p.
    pub padding: TitlePosition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShapeKind {
    Rectangle {
        /// Corner radius in pixels at 1080p reference resolution.
        /// 0.0 = sharp corners.
        #[serde(default)]
        corner_radius: f32,
    },
    /// Editable cubic contours with explicit topology-preserving morph keys.
    Path(crate::VectorPath),
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
    /// Legacy whole-title animation. Projects that carry one are
    /// migrated into the primary text layer's `motion` and `loops` on
    /// load (`migrate_legacy_animation`); nothing writes it and the
    /// renderer ignores it.
    #[serde(default)]
    pub animation_type: TitleAnimationType,
    /// Optional title-level background bar. Lower-third templates
    /// use this to paint a coloured strip behind the text. This
    /// stays as a clip-level field for commit 1; a future commit
    /// migrates lower-third templates to a `Shape` object instead,
    /// at which point this field can be retired.
    #[serde(default)]
    pub background: TitleBackground,
    /// The brief's type style this title follows, by name (#66): a brief
    /// that changes the style carries the title with it, in every field
    /// the title still has as the style had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// The fields a motion-graphics template keeps editable once applied
    /// (#1139): what the inspector shows first. Empty for a plain title.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published: Vec<crate::PublishedField>,
    /// The title template this title was made from, by the name the
    /// titles browser keeps it under (#1716), so its look can be carried
    /// to every title made from it. `None` for a title made from none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Keep the text readable over whatever is under it (#2734): with
    /// `Auto` the picture behind and round the glyphs is darkened (or, for
    /// dark text, lightened) as far as its brightness and busyness ask;
    /// `Always` does so even where it would not need to. `Off` for a
    /// title saved before it, so its look does not change; a new title
    /// takes `Auto`. A title with a backing is not protected: the backing
    /// is what its text stands out on.
    #[serde(default, skip_serializing_if = "ContrastProtect::is_off")]
    pub protect: ContrastProtect,
}

/// How a title keeps its text readable over the picture (#2734): see
/// [`TitleData::protect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContrastProtect {
    #[default]
    Off,
    Auto,
    Always,
}

impl ContrastProtect {
    pub fn is_off(&self) -> bool {
        *self == ContrastProtect::Off
    }

    /// As the compositor's passes take it: 0 off, 1 auto, 2 always.
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// What `TitleData::migrate_legacy_animation` did.
/// What [`TitleData::migrate_fx`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FxMigration {
    /// Any object's stack was made explicit or old fields were dropped.
    pub changed: bool,
    /// Text boxes whose old fields sat beside an explicit stack and
    /// were dropped in its favour; the loader says so.
    pub dropped_legacy: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegacyMigration {
    /// Something was converted; the title should be saved again.
    pub changed: bool,
    /// The old whole-title kind used to bake opacity, position, scale
    /// or rotation keyframes onto the clip: the caller clears them,
    /// since the loops now do the moving.
    pub clear_clip_keyframes: bool,
}

/// One legacy `CharEntrance` as a motion kind and easing.
fn char_entrance_motion(e: CharEntrance) -> Option<(MotionKind, Easing)> {
    Some(match e {
        CharEntrance::None => return None,
        CharEntrance::FadeIn => (MotionKind::Fade, Easing::OutCubic),
        CharEntrance::SlideUp => (MotionKind::Rise, Easing::OutCubic),
        CharEntrance::SlideDown => (MotionKind::Drop, Easing::OutCubic),
        CharEntrance::SlideLeft => (MotionKind::SlideLeft, Easing::OutCubic),
        CharEntrance::SlideRight => (MotionKind::SlideRight, Easing::OutCubic),
        CharEntrance::ScaleUp => (MotionKind::ScaleUp, Easing::OutCubic),
        CharEntrance::ScaleDown => (MotionKind::ScaleDown, Easing::OutCubic),
        CharEntrance::Pop => (MotionKind::ScaleUp, Easing::OutBack),
        CharEntrance::BlurIn => (MotionKind::Blur, Easing::OutCubic),
    })
}

impl TitleData {
    /// Fold the two legacy animation systems (the whole-title
    /// `animation_type` and each layer's `char_animation`) into the
    /// layers' `motion` and `loops`, then clear them. Old projects
    /// go through this once on load; a title with neither is
    /// untouched.
    pub fn migrate_legacy_animation(&mut self) -> LegacyMigration {
        use TitleAnimationType as T;
        let mut out = LegacyMigration::default();
        // The stacks first, so what follows writes into them.
        out.changed |= self.migrate_fx().changed;
        let legacy = std::mem::take(&mut self.animation_type);
        if !matches!(legacy, T::Static) {
            out.changed = true;
            out.clear_clip_keyframes = !matches!(
                legacy,
                T::Typewriter { .. } | T::TypewriterOut { .. } | T::Counter { .. }
                    | T::ColorCycle { .. } | T::ColorBlend { .. } | T::OutlineDrawOn { .. }
            );
            let chars = self.text().chars().count().max(1) as f32;
            let font = self.font_size().max(1.0);
            if let Some(tb) = self.primary_object_mut().map(TitleObject::fx_mut) {
                let enter = |k: MotionKind, d: f32| Motion::new(k).duration(d.max(0.05));
                let typing = |cps: f32| Motion::new(MotionKind::Typewriter).duration(0.05).stagger(StaggerUnit::Char, 1.0 / cps.max(0.5));
                match legacy {
                    T::Static => {}
                    T::ScrollUp { speed_px_per_sec } => tb.loops.push(TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec, shape: Default::default() }),
                    T::ScrollDown { speed_px_per_sec } => tb.loops.push(TitleLoop::Scroll { direction: ScrollDirection::Down, speed_px_per_sec, shape: Default::default() }),
                    T::CrawlLeft { speed_px_per_sec } => tb.loops.push(TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec, repeat: false, shape: Default::default() }),
                    T::CrawlRight { speed_px_per_sec } => tb.loops.push(TitleLoop::Crawl { direction: CrawlDirection::Right, speed_px_per_sec, repeat: false, shape: Default::default() }),
                    T::Marquee { speed_px_per_sec } => tb.loops.push(TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec, repeat: true, shape: Default::default() }),
                    T::Typewriter { chars_per_sec } => tb.motion.enter = Some(typing(chars_per_sec)),
                    T::TypewriterOut { chars_per_sec } => tb.motion.exit = Some(typing(chars_per_sec)),
                    T::OutlineDrawOn { duration_secs } => {
                        tb.motion.enter = Some(Motion::new(MotionKind::Typewriter).duration(0.05).stagger(StaggerUnit::Char, duration_secs.max(0.1) / chars));
                    }
                    T::FadeIn { duration_secs } => tb.motion.enter = Some(enter(MotionKind::Fade, duration_secs)),
                    T::FadeOut { duration_secs } => tb.motion.exit = Some(enter(MotionKind::Fade, duration_secs).as_exit()),
                    T::FadeInOut { fade_secs } => {
                        tb.motion.enter = Some(enter(MotionKind::Fade, fade_secs));
                        tb.motion.exit = Some(enter(MotionKind::Fade, fade_secs).as_exit());
                    }
                    T::Flash { rate_hz } => tb.loops.push(TitleLoop::Blink { rate_hz, duty: 0.5 }),
                    T::Blink { rate_hz, duty_cycle } => tb.loops.push(TitleLoop::Blink { rate_hz, duty: duty_cycle }),
                    T::Pulse { rate_hz, min_opacity } => tb.loops.push(TitleLoop::Pulse { rate_hz, min_opacity }),
                    T::ZoomIn { duration_secs, from_scale } => {
                        let k = if from_scale > 1.0 { MotionKind::ScaleDown } else { MotionKind::ScaleUp };
                        tb.motion.enter = Some(enter(k, duration_secs));
                    }
                    T::ZoomOut { duration_secs, to_scale } => {
                        let k = if to_scale > 1.0 { MotionKind::ScaleDown } else { MotionKind::ScaleUp };
                        tb.motion.exit = Some(enter(k, duration_secs).as_exit());
                    }
                    T::SlideInLeft { duration_secs } => tb.motion.enter = Some(enter(MotionKind::SlideRight, duration_secs).distance(6.0)),
                    T::SlideInRight { duration_secs } => tb.motion.enter = Some(enter(MotionKind::SlideLeft, duration_secs).distance(6.0)),
                    T::SlideInTop { duration_secs } => tb.motion.enter = Some(enter(MotionKind::Drop, duration_secs).distance(6.0)),
                    T::SlideInBottom { duration_secs } => tb.motion.enter = Some(enter(MotionKind::Rise, duration_secs).distance(6.0)),
                    T::SpinIn { duration_secs, .. } => tb.motion.enter = Some(enter(MotionKind::ScaleUp, duration_secs).easing(Easing::OutBack)),
                    T::BounceIn { duration_secs } => tb.motion.enter = Some(enter(MotionKind::Rise, duration_secs).easing(Easing::OutBounce).distance(1.5)),
                    T::Wave { amplitude_px, rate_hz } => tb.loops.push(TitleLoop::Wave { amplitude: amplitude_px / font, rate_hz }),
                    T::Shake { amplitude_px, rate_hz } => tb.loops.push(TitleLoop::Shake { amplitude: amplitude_px / font, rate_hz }),
                    T::ColorCycle { rate_hz } => tb.loops.push(TitleLoop::ColorCycle { rate_hz }),
                    T::ColorBlend { color_a, color_b, rate_hz } => tb.loops.push(TitleLoop::ColorBlend { color_a, color_b, rate_hz }),
                    T::Counter { from_value, to_value, duration_secs, decimals } => {
                        tb.loops.push(TitleLoop::Counter { from: from_value, to: to_value, duration_secs, decimals })
                    }
                }
            }
        }
        for obj in &mut self.objects {
            let TitleObject { content, fx, .. } = obj;
            if let TitleObjectContent::TextBox(tb) = content {
                if let Some(ca) = tb.char_animation.take() {
                    out.changed = true;
                    if !ca.enabled {
                        continue;
                    }
                    let fx = fx.get_or_insert_with(LayerFx::default);
                    if let Some((kind, easing)) = char_entrance_motion(ca.entrance) {
                        if fx.motion.enter.is_none() {
                            fx.motion.enter = Some(Motion::new(kind).duration(0.4).easing(easing).stagger(StaggerUnit::Char, ca.stagger.max(0.0)));
                        }
                    }
                    if ca.wave_amplitude > 0.0 {
                        fx.loops.push(TitleLoop::Wave { amplitude: ca.wave_amplitude * 0.3 / tb.font_size.max(1.0), rate_hz: ca.wave_speed });
                    }
                }
            }
        }
        out
    }

    /// Create a TitleData with one default-styled TextBox at frame
    /// centre — the canonical "fresh title" shape used by `Default`
    /// and most templates.
    pub fn single_text(text_box: TextBox) -> Self {
        let mut td = Self {
            objects: Vec::new(),
            next_object_id: 0,
            animation_type: TitleAnimationType::default(),
            background: TitleBackground::None,
            style: None,
            published: Vec::new(),
            template: None,
            protect: ContrastProtect::Off,
        };
        td.add_object(TitleObjectContent::TextBox(text_box), default_text_transform());
        td
    }

    /// [`single_text`](Self::single_text) with the layer's stack given
    /// alongside the text, since the stack is the object's, not the
    /// text box's.
    pub fn single_text_fx(text_box: TextBox, fx: LayerFx) -> Self {
        let mut td = Self::single_text(text_box);
        td.objects[0].fx = Some(fx);
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
        let mut content = content;
        let name = match &content {
            TitleObjectContent::TextBox(_) => format!("Text {}", id.0 + 1),
            TitleObjectContent::Shape(_) => format!("Shape {}", id.0 + 1),
            TitleObjectContent::Image(img) => img.stem(),
        };
        // A text box that still carries the old fields — read from an
        // older payload and wrapped here — gives them to its object.
        let fx = match &mut content {
            TitleObjectContent::TextBox(tb) => std::mem::take(&mut tb.legacy_fx).into_fx(),
            _ => LayerFx::default(),
        };
        self.objects.push(TitleObject {
            id,
            name,
            transform,
            visible: true,
            locked: false,
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            content,
            fx: Some(fx),
            blend: klipp_core::BlendMode::Normal,
            mask: None,
        });
        id
    }

    /// Look up an object by id. Returns `None` if the id was deleted
    /// or never existed in this title.
    pub fn object(&self, id: TitleObjectId) -> Option<&TitleObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    /// A copy of layer `id` with an id of its own, just above it in the
    /// stack (#1715): its name with " copy" after it, when it has one;
    /// what it masks with, if anything, the same. `None` for a layer the
    /// title does not have.
    pub fn duplicate_object(&mut self, id: TitleObjectId) -> Option<TitleObjectId> {
        let at = self.objects.iter().position(|o| o.id == id)?;
        let mut copy = self.objects[at].clone();
        copy.id = TitleObjectId(self.next_object_id);
        self.next_object_id = self.next_object_id.saturating_add(1);
        if !copy.name.is_empty() {
            copy.name = format!("{} copy", copy.name);
        }
        let new_id = copy.id;
        self.objects.insert(at + 1, copy);
        Some(new_id)
    }

    pub fn object_mut(&mut self, id: TitleObjectId) -> Option<&mut TitleObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// The layer that masks `obj` and how, when its mask names another
    /// layer of this title (#1513). A mask naming the layer itself or a
    /// deleted one masks nothing.
    pub fn mask_of(&self, obj: &TitleObject) -> Option<(&TitleObject, klipp_core::layer_mask::LayerMaskKind)> {
        let mask = obj.mask?;
        if mask.source == obj.id {
            return None;
        }
        self.object(mask.source).map(|src| (src, mask.kind))
    }

    /// Whether `id` is a matte: a shown layer is masked by it, so it
    /// masks and is not drawn itself (#1513).
    pub fn is_matte(&self, id: TitleObjectId) -> bool {
        self.objects.iter().any(|o| o.visible && self.mask_of(o).is_some_and(|(src, _)| src.id == id))
    }

    /// Whether a shown layer blends other than "over" or is masked: the
    /// title is then its layers mixed, not each laid on the last
    /// (#1513).
    pub fn mixes_layers(&self) -> bool {
        self.objects.iter().any(|o| o.visible && (o.blend != klipp_core::BlendMode::Normal || self.mask_of(o).is_some()))
    }

    /// This title's look with `target`'s words (#1716, #1718): every
    /// layer, effect, motion and setting this title has, each text layer
    /// given the text (and the styled runs of it) of `target`'s text
    /// layer in the same place, and `target`'s style and template kept;
    /// the layers keep this title's ids, which the look refers to them by.
    /// `None` when the two do not have the same layers, in kind and
    /// order: there is no telling then which words go where.
    pub fn with_texts_of(&self, target: &TitleData) -> Option<TitleData> {
        let kinds = |td: &TitleData| -> Vec<std::mem::Discriminant<TitleObjectContent>> { td.objects.iter().map(|o| std::mem::discriminant(&o.content)).collect() };
        if kinds(self) != kinds(target) {
            return None;
        }
        let mut out = self.clone();
        for (o, t) in out.objects.iter_mut().zip(&target.objects) {
            if let (TitleObjectContent::TextBox(tb), TitleObjectContent::TextBox(theirs)) = (&mut o.content, &t.content) {
                tb.text = theirs.text.clone();
                tb.style_runs = theirs.style_runs.clone();
            }
        }
        // The layers keep this title's ids, which its masks, fits, paths
        // and published fields name them by.
        out.style = target.style.clone();
        out.template = target.template.clone();
        Some(out)
    }

    /// `target` styled like this title (#1718), its words kept: this
    /// title's look whole when the two have the same layers
    /// ([`Self::with_texts_of`]); else this title's main text layer's
    /// type, fill, effects, motion and animators on `target`'s main text
    /// layer, which keeps its words, styled runs and place, and this
    /// title's background. `None` when either has no text layer.
    pub fn styled_like(&self, target: &TitleData) -> Option<TitleData> {
        if let Some(whole) = self.with_texts_of(target) {
            return Some(whole);
        }
        let from = self.primary_object()?;
        let TitleObjectContent::TextBox(look) = &from.content else { return None };
        let mut out = target.clone();
        let to = out.primary_object_mut()?;
        let TitleObjectContent::TextBox(tb) = &mut to.content else { return None };
        // Its words, runs and the path it follows (a path names a layer
        // of its own title) are its own.
        let (text, runs, path) = (std::mem::take(&mut tb.text), std::mem::take(&mut tb.style_runs), tb.text_path.take());
        *tb = TextBox { text, style_runs: runs, text_path: path, ..look.clone() };
        to.fx = from.fx.clone();
        to.opacity = from.opacity;
        to.blend = from.blend;
        out.background = self.background.clone();
        Some(out)
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

    /// The object holding the primary text.
    pub fn primary_object(&self) -> Option<&TitleObject> {
        self.objects.iter().find(|o| matches!(o.content, TitleObjectContent::TextBox(_)))
    }

    pub fn primary_object_mut(&mut self) -> Option<&mut TitleObject> {
        self.objects.iter_mut().find(|o| matches!(o.content, TitleObjectContent::TextBox(_)))
    }

    /// The primary text's stack, or a still one when there is no text.
    pub fn primary_fx(&self) -> &LayerFx {
        self.primary_object().map_or(&STILL_FX, TitleObject::fx)
    }

    /// Every object's stack made explicit, and the old text-box fields
    /// hoisted onto their object. One direction, once: an object with
    /// no stack takes the text box's old fields (a shape or picture
    /// takes an empty one); an object that already has a stack keeps
    /// it, and old fields found beside it are dropped — the stack is
    /// the one place, and an explicitly empty one wins. Afterwards no
    /// text box carries old fields, so a save writes one shape.
    pub fn migrate_fx(&mut self) -> FxMigration {
        let mut out = FxMigration::default();
        for obj in &mut self.objects {
            let TitleObject { content, fx, .. } = obj;
            let legacy = match content {
                TitleObjectContent::TextBox(tb) => std::mem::take(&mut tb.legacy_fx),
                _ => LegacyTextFx::default_authored(),
            };
            match fx {
                None => {
                    *fx = Some(legacy.into_fx());
                    out.changed = true;
                }
                Some(_) if !legacy.is_default() => out.dropped_legacy += 1,
                Some(_) => {}
            }
        }
        out.changed |= out.dropped_legacy > 0;
        out
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
        self.primary_fx().shadows.first().cloned()
    }
    pub fn shadows(&self) -> Vec<TitleShadow> {
        self.primary_fx().shadows.clone()
    }
    pub fn outlines(&self) -> Vec<TextOutline> {
        self.primary_fx().outlines.clone()
    }
    pub fn glow(&self) -> Option<TextGlow> {
        self.primary_fx().glows.first().cloned()
    }
    pub fn glows(&self) -> Vec<TextGlow> {
        self.primary_fx().glows.clone()
    }
    pub fn gradient(&self) -> Option<TextGradient> {
        self.primary_text().and_then(|tb| tb.gradient.clone())
    }
    pub fn inner_shadow(&self) -> Option<InnerShadow> {
        self.primary_fx().inner_shadows.first().cloned()
    }
    pub fn inner_shadows(&self) -> Vec<InnerShadow> {
        self.primary_fx().inner_shadows.clone()
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
    /// The motion blur the compositor applies to the title: the primary
    /// text's, else the first layer's that has one — a shape or picture
    /// moving as one smears as one.
    pub fn motion_blur(&self) -> Option<MotionBlur> {
        self.primary_fx()
            .motion_blur
            .clone()
            .or_else(|| self.objects.iter().find_map(|o| o.fx().motion_blur.clone()))
    }
    pub fn underline(&self) -> bool {
        self.primary_text().is_some_and(|tb| tb.underline)
    }
    pub fn strikethrough(&self) -> bool {
        self.primary_text().is_some_and(|tb| tb.strikethrough)
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
        size_x_keyframes: Vec::new(),
        size_y_keyframes: Vec::new(),
        size: TitlePosition { x: 1.0, y: 1.0 },
        rotation: 0.0,
        rotation_keyframes: Vec::new(),
        anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
        anchor_x_keyframes: Vec::new(),
        anchor_y_keyframes: Vec::new(),
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
    /// Front face color (if `[0,0,0,0]` uses title.color)
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
    /// Text follows a drawn path (#1508): one contour of a path layer in
    /// the same title, as that layer draws it at each moment, its morph
    /// keys and transform included. A missing layer, or one that is not
    /// a path, lays the text straight.
    Layer {
        layer: TitleObjectId,
        /// Which of the path's contours, from 0.
        #[serde(default, skip_serializing_if = "is_default")]
        contour: usize,
        /// Where along the contour the text sits, as a fraction of its
        /// length: 0 its start, 1 its end; on a closed contour it wraps.
        #[serde(default, skip_serializing_if = "is_default")]
        offset: f32,
        /// `offset` over the clip's time, so the text travels.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        offset_keyframes: Vec<klipp_core::Keyframe>,
        /// Which part of the text sits at `offset`.
        #[serde(default, skip_serializing_if = "is_default")]
        align: PathTextAlign,
        /// The text on the path's other side, running the other way.
        #[serde(default, skip_serializing_if = "is_default")]
        flip: bool,
    },
}

impl TextPath {
    /// A drawn path's `offset` at `time_secs` of its clip; 0 for the
    /// fixed shapes.
    pub fn offset_at(&self, time_secs: f64) -> f32 {
        match self {
            TextPath::Layer { offset, offset_keyframes, .. } if !offset_keyframes.is_empty() => klipp_core::keyframe::evaluate(offset_keyframes, time_secs, *offset),
            TextPath::Layer { offset, .. } => *offset,
            _ => 0.0,
        }
    }
}

/// Which part of a text on a drawn path sits at its offset (#1508).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PathTextAlign {
    #[default]
    Start,
    Center,
    End,
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

use klipp_core::title_motion::{CrawlDirection, EdgeFade, Easing, Motion, MotionKind, ScrollDirection, StaggerUnit, TitleLoop, TitleMotion};

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
    /// Every line but a paragraph's last fills the box's width, its
    /// word gaps widened to do it; the last line follows
    /// [`TextBox::last_line`] (#1506). Last, so a saved alignment
    /// reads as it did.
    Justify,
}

/// Where a justified paragraph's last line sits (#1506).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum LastLineAlign {
    #[default]
    Left,
    Center,
    /// Justified like the lines above it.
    Justify,
}

/// Where a text box's text sits up and down the box (#1506).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TextVerticalAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// A text box's paragraphs (#1721): a paragraph is the text between
/// line breaks. Lengths are pixels at 1080p, scaled with the frame as a
/// text's size is; each is keyable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Paragraph {
    /// Space above a paragraph, past the first.
    #[serde(default, skip_serializing_if = "is_default")]
    pub space_before: f32,
    /// Space below a paragraph, before the last.
    #[serde(default, skip_serializing_if = "is_default")]
    pub space_after: f32,
    /// Every line's indent from the box's start edge: the left, or the
    /// right in right-to-left text.
    #[serde(default, skip_serializing_if = "is_default")]
    pub indent_start: f32,
    /// Every line's indent from the box's end edge.
    #[serde(default, skip_serializing_if = "is_default")]
    pub indent_end: f32,
    /// A paragraph's first line's indent past `indent_start`; below 0 it
    /// hangs out of it, as far as the box's edge, the lines under it
    /// staying in.
    #[serde(default, skip_serializing_if = "is_default")]
    pub indent_first: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_before_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_after_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indent_start_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indent_end_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indent_first_keyframes: Vec<klipp_core::Keyframe>,
    /// Where a tab in the text moves what follows it (#1721), in order
    /// from the start indent, pixels at 1080p. Past the last, a stop
    /// falls every two ems.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabStop>,
    /// Each paragraph as an item of a list (#1721): its marker at the
    /// start indent, its text `indent` past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<ListStyle>,
    /// The fewest words a wrapped paragraph's last line may have (#1721):
    /// one short of it, the line above gives it its last words, where
    /// they fit. 0 and 1 leave the lines as they break.
    #[serde(default, skip_serializing_if = "is_default")]
    pub min_last_words: u8,
}

/// A list's look (#1721): the marker before each paragraph that has
/// text, and how far past the start indent the items' text starts, in
/// pixels at 1080p; the marker hangs in that space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListStyle {
    pub marker: ListMarker,
    pub indent: f32,
}

/// What marks a list's items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ListMarker {
    /// The same mark before each item: "•", "–", "■"…
    Bullet { mark: String },
    /// Items counted from `start`, in `style`, `suffix` after the count:
    /// "1.", "a)", "iv."
    Numbered { style: NumberStyle, start: u32, suffix: String },
}

/// How a numbered list counts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum NumberStyle {
    /// 1, 2, 3
    #[default]
    Arabic,
    /// a, b, … z, aa
    LowerAlpha,
    /// A, B, … Z, AA
    UpperAlpha,
    /// i, ii, iii, iv
    LowerRoman,
    /// I, II, III, IV
    UpperRoman,
}

impl ListMarker {
    /// The marker of the item `n` from 0.
    pub fn label(&self, n: usize) -> String {
        match self {
            ListMarker::Bullet { mark } => mark.clone(),
            ListMarker::Numbered { style, start, suffix } => format!("{}{suffix}", style.count(*start as usize + n)),
        }
    }
}

impl NumberStyle {
    /// `n` (from 1) counted in this style; 0 is "0" in any of them.
    pub fn count(self, n: usize) -> String {
        let alpha = |mut n: usize, a: u8| {
            let mut s = Vec::new();
            while n > 0 {
                n -= 1;
                s.push(a + (n % 26) as u8);
                n /= 26;
            }
            s.reverse();
            String::from_utf8(s).unwrap_or_default()
        };
        let roman = |mut n: usize| {
            let mut s = String::new();
            for (v, r) in [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")] {
                while n >= v {
                    s.push_str(r);
                    n -= v;
                }
            }
            s
        };
        if n == 0 {
            return "0".into();
        }
        match self {
            NumberStyle::Arabic => n.to_string(),
            NumberStyle::LowerAlpha => alpha(n, b'a'),
            NumberStyle::UpperAlpha => alpha(n, b'A'),
            NumberStyle::LowerRoman => roman(n),
            NumberStyle::UpperRoman => roman(n).to_uppercase(),
        }
    }
}

/// A tab stop (#1721): `at` pixels at 1080p from the start indent, and
/// which part of the text after the tab lands there.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TabStop {
    pub at: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub align: TabAlign,
}

/// Which part of a tab's text sits on its stop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TabAlign {
    /// The text starts at the stop.
    #[default]
    Left,
    /// The text is centred on the stop.
    Center,
    /// The text ends at the stop.
    Right,
    /// The text's decimal point is on the stop; a number without one
    /// ends there.
    Decimal,
}

impl Paragraph {
    /// The settings at `time_secs`, keys evaluated and cleared; spacing
    /// and indents never below 0, and a first line never hung past the
    /// box's edge.
    pub fn at(&self, time_secs: f64) -> Paragraph {
        let key = |keys: &[klipp_core::Keyframe], v: f32| if keys.is_empty() { v } else { klipp_core::keyframe::evaluate(keys, time_secs, v) };
        let start = key(&self.indent_start_keyframes, self.indent_start).max(0.0);
        Paragraph {
            space_before: key(&self.space_before_keyframes, self.space_before).max(0.0),
            space_after: key(&self.space_after_keyframes, self.space_after).max(0.0),
            indent_start: start,
            indent_end: key(&self.indent_end_keyframes, self.indent_end).max(0.0),
            indent_first: key(&self.indent_first_keyframes, self.indent_first).max(-start),
            tabs: self.tabs.clone(),
            list: self.list.clone(),
            min_last_words: self.min_last_words,
            ..Paragraph::default()
        }
    }

    /// The keys of the setting named `key`, to change.
    pub fn keys_mut_for(&mut self, key: &str) -> Option<&mut Vec<klipp_core::Keyframe>> {
        use klipp_core::title_effect_keys as k;
        Some(match key {
            k::TEXT_SPACE_BEFORE => &mut self.space_before_keyframes,
            k::TEXT_SPACE_AFTER => &mut self.space_after_keyframes,
            k::TEXT_INDENT_START => &mut self.indent_start_keyframes,
            k::TEXT_INDENT_END => &mut self.indent_end_keyframes,
            k::TEXT_INDENT_FIRST => &mut self.indent_first_keyframes,
            _ => return None,
        })
    }

    /// The keys of the setting named `key`.
    pub fn keys_for(&self, key: &str) -> Option<&[klipp_core::Keyframe]> {
        use klipp_core::title_effect_keys as k;
        Some(match key {
            k::TEXT_SPACE_BEFORE => &self.space_before_keyframes,
            k::TEXT_SPACE_AFTER => &self.space_after_keyframes,
            k::TEXT_INDENT_START => &self.indent_start_keyframes,
            k::TEXT_INDENT_END => &self.indent_end_keyframes,
            k::TEXT_INDENT_FIRST => &self.indent_first_keyframes,
            _ => return None,
        })
    }

    /// The setting named `key`, to change.
    pub fn value_mut_for(&mut self, key: &str) -> Option<&mut f32> {
        use klipp_core::title_effect_keys as k;
        Some(match key {
            k::TEXT_SPACE_BEFORE => &mut self.space_before,
            k::TEXT_SPACE_AFTER => &mut self.space_after,
            k::TEXT_INDENT_START => &mut self.indent_start,
            k::TEXT_INDENT_END => &mut self.indent_end,
            k::TEXT_INDENT_FIRST => &mut self.indent_first,
            _ => return None,
        })
    }
}

/// How a text box's text meets the box when it is too long for it
/// (#1722).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum TextFit {
    /// Wrapped between words at the box's width, and set smaller until
    /// it is no taller than the box: what a text box has always done.
    #[default]
    Wrap,
    /// Kept on the lines it was typed on, and set smaller until its
    /// widest line fits the box's width and the lines its height — never
    /// below `min_size`, in the units of `TextBox::font_size`. Smaller
    /// than that would need, it wraps at the box's width there. A lower
    /// third's name: a long one takes the line at a smaller size.
    Shrink { min_size: f32 },
    /// Wrapped at the box's width at its own size, the box growing with
    /// the lines rather than the type shrinking; from its vertical
    /// alignment, so a top-aligned box grows down. A backing fitted to
    /// it grows with it.
    Grow,
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

/// A gradient fill, on a text or a shape. Saved as two colours, top to
/// bottom, as it always was; a gradient of more stops, or one at an
/// angle or radial (#1509), adds `stops` and `kind`. `top_color` and
/// `bottom_color` always hold the first and the last stop, so whatever
/// reads or keys the pair still finds the ends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextGradient {
    pub top_color: [u8; 4],
    pub bottom_color: [u8; 4],
    /// Soft-disable: see [`TitleShadow::enabled`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Every stop in position order, when there are more than the pair
    /// (#1509). Empty: the two colours above, at 0 and 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<crate::GradientStop>,
    /// Linear at an angle or radial (#1509); top to bottom by default.
    #[serde(default, skip_serializing_if = "crate::GradientKind::is_default")]
    pub kind: crate::GradientKind,
    /// Each stop's colour over the clip (#1511), by the stop's place in
    /// [`Self::stops`]; a stop without keys keeps its colour. Written
    /// only when keyed.
    #[serde(default, skip_serializing_if = "no_stop_keys")]
    pub stop_keyframes: Vec<ColourKeys>,
}

fn no_stop_keys(keys: &[ColourKeys]) -> bool {
    keys.iter().all(ColourKeys::is_empty)
}

impl TextGradient {
    /// Whether a stop's colour is keyed.
    pub fn has_keyframes(&self) -> bool {
        !no_stop_keys(&self.stop_keyframes)
    }

    /// The gradient at `time_secs`: each keyed stop's colour as it is
    /// then, the pair's colours the ends', its keys gone.
    pub fn at(&self, time_secs: f64) -> TextGradient {
        let mut stops = self.stops();
        for (stop, keys) in stops.iter_mut().zip(&self.stop_keyframes) {
            if !keys.is_empty() {
                stop.color = keys.at(time_secs, stop.color);
            }
        }
        let mut g = self.clone();
        g.stop_keyframes = Vec::new();
        g.top_color = stops.first().map_or(g.top_color, |s| s.color);
        g.bottom_color = stops.last().map_or(g.bottom_color, |s| s.color);
        if g.stops.len() >= 2 {
            g.stops = stops;
        }
        g
    }

    /// The stop keys of `before` carried onto this gradient's stops: a
    /// stop keeps its keys where it is still at the same place, so a
    /// stop added or taken away does not move the others' keys onto
    /// their neighbours. With as many stops as before, each keeps its
    /// own, wherever it was dragged.
    pub fn carry_keys_from(&mut self, before: &TextGradient) {
        let (old, new) = (before.stops(), self.stops());
        let key = |i: usize| before.stop_keyframes.get(i).cloned().unwrap_or_default();
        self.stop_keyframes = if old.len() == new.len() {
            (0..new.len()).map(key).collect()
        } else {
            new.iter().map(|s| old.iter().position(|o| o.position == s.position).map(key).unwrap_or_default()).collect()
        };
        if no_stop_keys(&self.stop_keyframes) {
            self.stop_keyframes.clear();
        }
    }

    /// The two-colour gradient, top to bottom.
    pub fn pair(top_color: [u8; 4], bottom_color: [u8; 4], enabled: bool) -> Self {
        Self { top_color, bottom_color, enabled, stops: Vec::new(), kind: crate::GradientKind::default(), stop_keyframes: Vec::new() }
    }

    /// A gradient through `stops` laid out by `kind` (#1509). Two stops
    /// at 0 and 1 are kept as the pair, so a pair saves as it always
    /// has; the ends are mirrored into `top_color` and `bottom_color`.
    pub fn with_stops(stops: &[crate::GradientStop], kind: crate::GradientKind, enabled: bool) -> Self {
        let mut stops = stops.to_vec();
        stops.sort_by(|a, b| a.position.total_cmp(&b.position));
        let first = stops.first().map(|s| s.color).unwrap_or([255; 4]);
        let last = stops.last().map(|s| s.color).unwrap_or(first);
        let pair = stops.len() == 2 && stops[0].position == 0.0 && stops[1].position == 1.0;
        Self { top_color: first, bottom_color: last, enabled, stops: if pair { Vec::new() } else { stops }, kind, stop_keyframes: Vec::new() }
    }

    /// The gradient as it was before #1509: two colours, top to bottom.
    /// A text draws it as it always did, mixing the colours row by row
    /// in sRGB; every other gradient mixes in linear light.
    pub fn is_pair(&self) -> bool {
        self.stops.len() < 2 && self.kind.is_default()
    }

    /// Every stop in position order: `stops`, or the pair at 0 and 1.
    pub fn stops(&self) -> Vec<crate::GradientStop> {
        if self.stops.len() >= 2 {
            let mut stops = self.stops.clone();
            stops.sort_by(|a, b| a.position.total_cmp(&b.position));
            stops
        } else {
            vec![
                crate::GradientStop { position: 0.0, color: self.top_color },
                crate::GradientStop { position: 1.0, color: self.bottom_color },
            ]
        }
    }
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

    /// Each keyed field of each style makes the layer's styles keyed,
    /// and nothing else does (#2335).
    #[test]
    fn styles_are_keyed_by_their_keyed_fields_alone() {
        let k = || vec![klipp_core::Keyframe { time_secs: 0.0, value: 1.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
        let glow = TextGlow { enabled: true, color: [1; 4], radius: 4.0, radius_keyframes: vec![], intensity: 1.0, intensity_keyframes: vec![] };
        let shadow = TitleShadow { enabled: true, color: [1; 4], offset_x: 1.0, offset_x_keyframes: vec![], offset_y: 1.0, offset_y_keyframes: vec![], blur: 1.0, blur_keyframes: vec![] };
        let outline = TextOutline { enabled: true, color: [1; 4], width: 1.0, width_keyframes: vec![] };
        let inner = InnerShadow { enabled: true, color: [1; 4], offset_x: 1.0, offset_x_keyframes: vec![], offset_y: 1.0, offset_y_keyframes: vec![], blur: 1.0, blur_keyframes: vec![] };
        let still = LayerFx { glows: vec![glow.clone()], shadows: vec![shadow.clone()], outlines: vec![outline.clone()], inner_shadows: vec![inner.clone()], ..Default::default() };
        assert!(!still.styles_keyed());
        let keyed = [
            LayerFx { glows: vec![TextGlow { radius_keyframes: k(), ..glow.clone() }], ..still.clone() },
            LayerFx { glows: vec![TextGlow { intensity_keyframes: k(), ..glow }], ..still.clone() },
            LayerFx { shadows: vec![TitleShadow { offset_x_keyframes: k(), ..shadow.clone() }], ..still.clone() },
            LayerFx { shadows: vec![TitleShadow { offset_y_keyframes: k(), ..shadow.clone() }], ..still.clone() },
            LayerFx { shadows: vec![TitleShadow { blur_keyframes: k(), ..shadow }], ..still.clone() },
            LayerFx { outlines: vec![TextOutline { width_keyframes: k(), ..outline }], ..still.clone() },
            LayerFx { inner_shadows: vec![InnerShadow { offset_x_keyframes: k(), ..inner.clone() }], ..still.clone() },
            LayerFx { inner_shadows: vec![InnerShadow { offset_y_keyframes: k(), ..inner.clone() }], ..still.clone() },
            LayerFx { inner_shadows: vec![InnerShadow { blur_keyframes: k(), ..inner }], ..still.clone() },
        ];
        for (i, fx) in keyed.iter().enumerate() {
            assert!(fx.styles_keyed(), "case {i}");
        }
    }

    use klipp_core::{GeneratorData, MediaId, SequenceId};

    #[test]
    fn clip_kind_playability() {
        assert!(ClipKind::Media { media_id: MediaId::new() }.is_playable());
        assert!(ClipKind::Title(TitleData::default()).is_playable());
        assert!(ClipKind::Generator(GeneratorData::default()).is_playable());
        assert!(ClipKind::Sequence { sequence_id: SequenceId::new() }.is_playable());
        assert!(ClipKind::Precomposition { sequence_id: SequenceId::new() }.is_playable());
        assert!(!ClipKind::Gap.is_playable());
        assert!(!ClipKind::AdjustmentLayer.is_playable());
    }

    #[test]
    fn clip_kind_decoder_need() {
        assert!(ClipKind::Media { media_id: MediaId::new() }.needs_decoder());
        assert!(ClipKind::Sequence { sequence_id: SequenceId::new() }.needs_decoder());
        assert!(ClipKind::Precomposition { sequence_id: SequenceId::new() }.needs_decoder());
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

    #[test]
    fn transform_property_keyframes_evaluate_and_round_trip() {
        use klipp_core::TitleTransformProperty as P;
        let mut t = default_text_transform();
        t.upsert_transform_keyframe(P::Width, 0.0, 0.5);
        t.upsert_transform_keyframe(P::Width, 1.0, 1.0);
        t.upsert_transform_keyframe(P::Rotation, 0.0, -10.0);
        t.upsert_transform_keyframe(P::Rotation, 1.0, 30.0);
        t.upsert_transform_keyframe(P::AnchorX, 0.0, 0.0);
        t.upsert_transform_keyframe(P::AnchorX, 1.0, 1.0);
        let mid = t.evaluated_at(0.5);
        assert!((mid.size.x - 0.75).abs() < 1e-6);
        assert!((mid.rotation - 10.0).abs() < 1e-6);
        assert!((mid.anchor_norm.x - 0.5).abs() < 1e-6);
        let json = serde_json::to_string(&t).unwrap();
        let round: ObjectTransform = serde_json::from_str(&json).unwrap();
        assert_eq!(t, round);
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.5,
                value: 0.5,
                interpolation: Interpolation::Bezier,
                tangent_in: (-0.25, -0.05),
                tangent_out: (0.25, 0.05),
                handles: Default::default(),
            },
        ];
        obj.transform.center_y_keyframes = vec![Keyframe {
            time_secs: 1.0,
            value: 0.85,
            interpolation: Interpolation::Step,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
            handles: Default::default(),
        }];

        let json = serde_json::to_string(&td).expect("serialise");
        let round: TitleData = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(td, round, "round-trip must be lossless");
    }

    /// `upsert_position_keyframe` inserts in time order and replaces
    /// an existing keyframe at the same timestamp. This is
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

        // A nearby but distinct timestamp must retain its own key.
        t.upsert_position_keyframe(TitlePositionAxis::X, 0.51, 0.5);
        assert_eq!(t.center_x_keyframes.len(), 4, "nearby keys must survive");
        t.remove_position_keyframe_at(TitlePositionAxis::X, 0.51);
        assert_eq!(t.center_x_keyframes[1].time_secs, 0.5);
        assert_eq!(t.center_x_keyframes.len(), 3);

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

        // No-op when no keyframe matches the timestamp.
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

    #[test]
    fn title_object_opacity_keyframes_evaluate_and_round_trip() {
        let mut td = TitleData::default();
        let oid = td.add_object(
            TitleObjectContent::TextBox(TextBox::default()),
            default_text_transform(),
        );
        let obj = td.object_mut(oid).unwrap();
        obj.upsert_opacity_keyframe(0.0, 0.0);
        obj.upsert_opacity_keyframe(1.0, 1.0);
        assert!((obj.evaluated_opacity(0.5) - 0.5).abs() < 1e-6);
        let json = serde_json::to_string(&td).unwrap();
        let restored: TitleData = serde_json::from_str(&json).unwrap();
        let restored_obj = restored.object(oid).unwrap();
        assert_eq!(restored_obj.opacity_keyframes.len(), 2);
        assert!((restored_obj.evaluated_opacity(0.5) - 0.5).abs() < 1e-6);
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
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
                handles: Default::default(),
            }],
            intensity: 0.9,
            intensity_keyframes: vec![Keyframe {
                time_secs: 0.0,
                value: 0.5,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 16.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
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
                handles: Default::default(),
            }],
            offset_y: 3.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: vec![Keyframe {
                time_secs: 0.0, value: 0.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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

        let mut tb = LayerFx::default();
        // No shadow yet: router returns None for every shadow key.
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_BLUR).is_none());
        assert!(tb.effect_keyframes_mut_for(title_effect_keys::SHADOW_OFFSET_X).is_none());

        tb.shadows.push(TitleShadow {
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
        let s = tb.shadows.first().unwrap();
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0, value: 10.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0, value: 25.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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
        let tb = TextBox {
            texture_fill: Some(TextureFill {
                enabled: true,
                source: TextureSource::File("/tmp/paper.png".to_string()),
                tile_mode: TextureTileMode::Tile,
                scale: 1.5,
                offset_x: 12.0,
                offset_y: -8.0,
                rotation: 30.0,
                opacity: 0.75,
                tint: [200, 180, 140, 255],
            }),
            ..Default::default()
        };
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

    /// Per-index shadow keyframes are independent across slots and
    /// don't bleed when a sibling layer is touched. Pins the
    /// indexed-routing contract that the inspector stack rows now
    /// depend on for keyframe-aware editing.
    #[test]
    fn shadow_indexed_keyframe_routing_is_per_slot() {
        let mut tb = LayerFx::default();
        tb.shadows.push(TitleShadow {
            enabled: true, color: [0, 0, 0, 200],
            offset_x: 1.0, offset_x_keyframes: Vec::new(),
            offset_y: 1.0, offset_y_keyframes: Vec::new(),
            blur: 1.0, blur_keyframes: Vec::new(),
        });
        tb.shadows.push(TitleShadow {
            enabled: true, color: [255, 0, 0, 200],
            offset_x: 5.0, offset_x_keyframes: Vec::new(),
            offset_y: 5.0, offset_y_keyframes: Vec::new(),
            blur: 5.0, blur_keyframes: Vec::new(),
        });

        // shadow.0.offset_x → shadows[0]. shadow.1.blur → shadows[1].
        let key_0_ox = klipp_core::title_effect_keys::shadow_offset_x_key(0);
        let key_1_bl = klipp_core::title_effect_keys::shadow_blur_key(1);
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key_0_ox).unwrap(),
            0.0, 7.0,
        );
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key_1_bl).unwrap(),
            0.0, 12.0,
        );
        assert_eq!(tb.shadows[0].offset_x_keyframes.len(), 1);
        assert_eq!(tb.shadows[0].offset_x_keyframes[0].value, 7.0);
        assert_eq!(tb.shadows[1].blur_keyframes.len(), 1);
        assert_eq!(tb.shadows[1].blur_keyframes[0].value, 12.0);
        // Sibling slots remain untouched.
        assert_eq!(tb.shadows[1].offset_x_keyframes.len(), 0);
        assert_eq!(tb.shadows[0].blur_keyframes.len(), 0);

        // Out-of-range index returns None.
        let key_oob = klipp_core::title_effect_keys::shadow_offset_x_key(99);
        assert!(tb.effect_keyframes_mut_for(&key_oob).is_none());

        // Legacy unindexed key still routes to slot 0.
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(klipp_core::title_effect_keys::SHADOW_OFFSET_Y).unwrap(),
            0.0, 9.0,
        );
        assert_eq!(tb.shadows[0].offset_y_keyframes.len(), 1);
        assert_eq!(tb.shadows[0].offset_y_keyframes[0].value, 9.0);
        assert_eq!(tb.shadows[1].offset_y_keyframes.len(), 0);
    }

    /// Round-trip + legacy migration for the glow stack.
    #[test]
    fn glows_round_trip_and_legacy_default() {
        let mut tb = LayerFx::default();
        tb.glows.push(TextGlow {
            enabled: true,
            color: [10, 200, 255, 255],
            radius: 12.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        });
        tb.glows.push(TextGlow {
            enabled: false,
            color: [255, 100, 0, 200],
            radius: 24.0,
            radius_keyframes: Vec::new(),
            intensity: 0.4,
            intensity_keyframes: Vec::new(),
        });
        tb.glow_stack_enabled = false;
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: LayerFx = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(tb.glows, round.glows);
        assert!(!round.glow_stack_enabled);

        let legacy_json = r#"{
            "text": "Hi",
            "font_size": 72.0,
            "font_name": "sans-serif",
            "color": [255, 255, 255, 255],
            "alignment": "Center"
        }"#;
        let legacy: TextBox = serde_json::from_str(legacy_json).expect("legacy parse");
        assert!(legacy.legacy_fx.glows.is_empty());
        assert!(legacy.legacy_fx.glow_stack_enabled);
    }

    /// Indexed glow keyframe routing: `glow.{idx}.field` lands on
    /// `glows[idx].*_keyframes`.
    #[test]
    fn glow_indexed_keyframe_routing() {
        let mut tb = LayerFx::default();
        tb.glows.push(TextGlow {
            enabled: true,
            color: [0, 200, 255, 255],
            radius: 8.0,
            radius_keyframes: Vec::new(),
            intensity: 0.6,
            intensity_keyframes: Vec::new(),
        });
        tb.glows.push(TextGlow {
            enabled: true,
            color: [255, 100, 0, 200],
            radius: 16.0,
            radius_keyframes: Vec::new(),
            intensity: 0.4,
            intensity_keyframes: Vec::new(),
        });
        let key1 = klipp_core::title_effect_keys::glow_radius_key(1);
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key1).unwrap(),
            0.0, 30.0,
        );
        assert_eq!(tb.glows[1].radius_keyframes.len(), 1);
        assert_eq!(tb.glows[0].radius_keyframes.len(), 0);
        // Legacy `glow.radius` still routes to glows[0].
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(klipp_core::title_effect_keys::GLOW_RADIUS).unwrap(),
            0.0, 8.0,
        );
        assert_eq!(tb.glows[0].radius_keyframes.len(), 1);
    }

    /// Round-trip + legacy migration for the inner-shadow stack.
    #[test]
    fn inner_shadows_round_trip_and_legacy_default() {
        let mut tb = LayerFx::default();
        tb.inner_shadows.push(InnerShadow {
            enabled: true,
            color: [10, 20, 30, 200],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 3.0, offset_y_keyframes: Vec::new(),
            blur: 4.0, blur_keyframes: Vec::new(),
        });
        tb.inner_shadows.push(InnerShadow {
            enabled: false,
            color: [50, 60, 70, 220],
            offset_x: 5.0, offset_x_keyframes: Vec::new(),
            offset_y: 6.0, offset_y_keyframes: Vec::new(),
            blur: 8.0, blur_keyframes: Vec::new(),
        });
        tb.inner_shadow_stack_enabled = false;
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: LayerFx = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(tb.inner_shadows, round.inner_shadows);
        assert!(!round.inner_shadow_stack_enabled);

        // Legacy: TextBox JSON without inner_shadows / stack flag.
        let legacy_json = r#"{
            "text": "Hi",
            "font_size": 72.0,
            "font_name": "sans-serif",
            "color": [255, 255, 255, 255],
            "alignment": "Center"
        }"#;
        let legacy: TextBox = serde_json::from_str(legacy_json).expect("legacy parse");
        assert!(legacy.legacy_fx.inner_shadows.is_empty());
        assert!(legacy.legacy_fx.inner_shadow_stack_enabled);
    }

    /// Indexed inner-shadow keyframe routing: `inner_shadow.{idx}.field`
    /// lands on `inner_shadows[idx].*_keyframes`.
    #[test]
    fn inner_shadow_indexed_keyframe_routing() {
        let mut tb = LayerFx::default();
        tb.inner_shadows.push(InnerShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 1.0, offset_x_keyframes: Vec::new(),
            offset_y: 1.0, offset_y_keyframes: Vec::new(),
            blur: 2.0, blur_keyframes: Vec::new(),
        });
        tb.inner_shadows.push(InnerShadow {
            enabled: true,
            color: [255, 0, 0, 200],
            offset_x: 4.0, offset_x_keyframes: Vec::new(),
            offset_y: 4.0, offset_y_keyframes: Vec::new(),
            blur: 6.0, blur_keyframes: Vec::new(),
        });
        let key1 = klipp_core::title_effect_keys::inner_shadow_blur_key(1);
        klipp_core::keyframe::upsert_keyframe(
            tb.effect_keyframes_mut_for(&key1).unwrap(),
            0.0, 12.0,
        );
        assert_eq!(tb.inner_shadows[1].blur_keyframes.len(), 1);
        // shadows[0] untouched.
        assert_eq!(tb.inner_shadows[0].blur_keyframes.len(), 0);
        // Out-of-range index returns None.
        let key_oob = klipp_core::title_effect_keys::inner_shadow_blur_key(99);
        assert!(tb.effect_keyframes_mut_for(&key_oob).is_none());
    }

    /// Round-trip a multi-element shadow stack on TextBox. Each layer
    /// keeps its color / offsets / blur and per-element keyframe vecs
    /// across serialise → deserialise.
    #[test]
    fn shadows_round_trip_through_serde() {
        use klipp_core::{Interpolation, Keyframe};
        let mut tb = LayerFx::default();
        tb.shadows.push(TitleShadow {
            enabled: true,
            color: [10, 20, 30, 200],
            offset_x: 3.0,
            offset_x_keyframes: vec![Keyframe {
                time_secs: 0.0, value: 3.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
            }],
            offset_y: 4.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: Vec::new(),
        });
        tb.shadows.push(TitleShadow {
            enabled: false,
            color: [50, 60, 70, 220],
            offset_x: 9.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 12.0,
            offset_y_keyframes: Vec::new(),
            blur: 8.0,
            blur_keyframes: Vec::new(),
        });
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: LayerFx = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(tb.shadows, round.shadows);
        assert!(round.shadow_stack_enabled, "default missing field is true");
    }

    /// Legacy JSON without `shadows` / `shadow_stack_enabled`: an
    /// older TextBox payload (no array, only a singular `shadow`)
    /// must deserialise into a TextBox with empty `shadows` and
    /// `shadow_stack_enabled = true`. Project load.rs is responsible
    /// for synthesising shadows[0] from the legacy `shadow` field at
    /// the project layer; the timeline crate just needs to round-trip
    /// the new fields with sane defaults.
    #[test]
    fn shadows_legacy_json_without_array_loads_with_defaults() {
        let legacy_json = r#"{
            "text": "Hi",
            "font_size": 72.0,
            "font_name": "sans-serif",
            "color": [255, 255, 255, 255],
            "alignment": "Center"
        }"#;
        let legacy: TextBox = serde_json::from_str(legacy_json).expect("legacy parse");
        assert!(legacy.legacy_fx.shadows.is_empty());
        assert!(legacy.legacy_fx.shadow_stack_enabled);
    }

    /// `shadow_stack_enabled = false` round-trips the explicit false.
    #[test]
    fn shadow_stack_enabled_round_trips_false() {
        let tb = LayerFx {
            shadow_stack_enabled: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: LayerFx = serde_json::from_str(&json).expect("deserialise");
        assert!(!round.shadow_stack_enabled);
    }

    /// Round-trip for the `outline_stack_enabled` soft-disable bit
    /// on TextBox. Confirms the field serialises and deserialises
    /// losslessly, and that a missing field on legacy JSON deserialises
    /// to `true` so old projects keep their secondary outlines visible.
    #[test]
    fn outline_stack_enabled_round_trips_and_legacy_defaults_true() {
        // Round-trip `false`.
        let tb = LayerFx {
            outline_stack_enabled: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&tb).expect("serialise");
        let round: LayerFx = serde_json::from_str(&json).expect("deserialise");
        assert!(!round.outline_stack_enabled, "explicit false must survive round-trip");

        // Legacy JSON: omit the field entirely. Must default to true.
        let legacy_json = r#"{
            "text": "Hi",
            "font_size": 72.0,
            "font_name": "sans-serif",
            "color": [255, 255, 255, 255],
            "alignment": "Center"
        }"#;
        let legacy: TextBox = serde_json::from_str(legacy_json)
            .expect("legacy TextBox JSON without outline_stack_enabled must parse");
        assert!(
            legacy.legacy_fx.outline_stack_enabled,
            "missing field must default to true (preserve old project behaviour)"
        );
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0, value: 9.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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

        let mut tb = LayerFx::default();
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0, value: 0.10,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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
                handles: Default::default(),
            },
            Keyframe {
                time_secs: 1.0, value: 270.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                handles: Default::default(),
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

        let mut tb = LayerFx::default();
        // No inner shadow yet: inner_shadow.* returns None.
        assert!(tb
            .effect_keyframes_mut_for(title_effect_keys::INNER_SHADOW_BLUR)
            .is_none());

        tb.inner_shadows.push(InnerShadow {
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
            tb.inner_shadows.first().unwrap().offset_x_keyframes.len(),
            1,
        );
        // The (separate) shadow path is still empty — no bleed.
        assert!(tb.shadows.is_empty());
        // shadow.* without a TitleShadow returns None and does NOT
        // accidentally hit the InnerShadow vec.
        assert!(tb
            .effect_keyframes_mut_for(title_effect_keys::SHADOW_BLUR)
            .is_none());
    }
}

#[cfg(test)]
mod legacy_animation_migration_tests {
    use super::*;

    fn title() -> TitleData {
        TitleData::single_text(TextBox { text: "Roll\nCredits".into(), ..Default::default() })
    }

    #[test]
    fn a_static_title_is_left_alone() {
        let mut td = title();
        let m = td.migrate_legacy_animation();
        assert!(!m.changed && !m.clear_clip_keyframes);
        assert!(td.primary_fx().loops.is_empty());
    }

    #[test]
    fn a_scroll_becomes_a_loop_and_its_baked_keyframes_go() {
        let mut td = title();
        td.animation_type = TitleAnimationType::ScrollUp { speed_px_per_sec: 60.0 };
        let m = td.migrate_legacy_animation();
        assert!(m.changed && m.clear_clip_keyframes);
        assert!(matches!(td.animation_type, TitleAnimationType::Static));
        assert_eq!(
            td.primary_fx().loops,
            vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 60.0, shape: Default::default() }]
        );
    }

    #[test]
    fn a_fade_in_out_becomes_an_entrance_and_an_exit() {
        let mut td = title();
        td.animation_type = TitleAnimationType::FadeInOut { fade_secs: 0.8 };
        td.migrate_legacy_animation();
        let fx = td.primary_fx();
        assert_eq!(fx.motion.enter.map(|m| (m.kind, m.duration_secs)), Some((MotionKind::Fade, 0.8)));
        assert_eq!(fx.motion.exit.map(|m| (m.kind, m.duration_secs)), Some((MotionKind::Fade, 0.8)));
    }

    #[test]
    fn a_counter_keeps_the_clip_keyframes_it_never_wrote() {
        let mut td = title();
        td.animation_type = TitleAnimationType::Counter { from_value: 0.0, to_value: 9.0, duration_secs: 2.0, decimals: 1 };
        let m = td.migrate_legacy_animation();
        assert!(m.changed && !m.clear_clip_keyframes);
        assert!(matches!(td.primary_fx().loops[0], TitleLoop::Counter { decimals: 1, .. }));
    }

    #[test]
    fn a_character_animation_becomes_a_staggered_entrance_and_a_wave() {
        let mut td = title();
        td.primary_text_mut().unwrap().char_animation = Some(CharAnimation {
            enabled: true,
            entrance: CharEntrance::SlideUp,
            stagger: 0.07,
            stagger_keyframes: Vec::new(),
            wave_amplitude: 24.0,
            wave_amplitude_keyframes: Vec::new(),
            wave_speed: 2.0,
            wave_speed_keyframes: Vec::new(),
        });
        td.migrate_legacy_animation();
        assert!(td.primary_text().unwrap().char_animation.is_none());
        let fx = td.primary_fx();
        let e = fx.motion.enter.unwrap();
        assert_eq!((e.kind, e.stagger, e.stagger_secs), (MotionKind::Rise, StaggerUnit::Char, 0.07));
        assert!(matches!(fx.loops[0], TitleLoop::Wave { rate_hz, .. } if rate_hz == 2.0));
    }
}

#[cfg(test)]
mod colour_key_tests {
    use super::*;

    /// A colour keyed whole keys each channel in linear light, blends
    /// there, comes away whole, and its channels are the text's own
    /// keyable params (#1511).
    #[test]
    fn a_colour_keys_and_blends_in_linear_light() {
        let mut keys = ColourKeys::default();
        keys.set(0.0, [255, 0, 0, 255]);
        keys.set(2.0, [0, 0, 255, 51]);
        assert_eq!(keys.r.len(), 2);
        assert!((keys.b[1].value - 1.0).abs() < 1e-6 && (keys.a[1].value - 0.2).abs() < 1e-6);
        assert_eq!(keys.at(1.0, [9, 9, 9, 9]), [188, 0, 188, 153], "half the light of each, half the alpha");
        assert_eq!(keys.at(0.0, [9, 9, 9, 9]), [255, 0, 0, 255]);
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(v)), v);
        }
        let mut tb = TextBox { color_keyframes: keys.clone(), ..Default::default() };
        assert!(tb.has_keyframes());
        assert_eq!(tb.effect_keyframes_for(klipp_core::title_effect_keys::TEXT_COLOR_G), Some(&keys.g[..]));
        assert!(tb.effect_keyframes_mut_for("text.color.x").is_none());
        let at = tb.at(2.0);
        assert_eq!((at.color, at.color_keyframes.is_empty()), ([0, 0, 255, 51], true));
        tb.color_keyframes.remove(2.0 + 1e-4, 0.01);
        assert!([&tb.color_keyframes.r, &tb.color_keyframes.g, &tb.color_keyframes.b, &tb.color_keyframes.a].iter().all(|k| k.len() == 1));
        let json = serde_json::to_string(&TextBox::default()).unwrap();
        assert!(!json.contains("color_keyframes"), "{json}");
    }
}

#[cfg(test)]
mod animator_key_tests {
    use super::*;

    /// An animator's range is keyed through the layer's effect keys, by
    /// its place in the stack; a stack without animators writes none
    /// (#1510).
    #[test]
    fn an_animators_range_is_a_keyable_param() {
        let k = |i: usize, f: &str| klipp_core::title_effect_keys::animator_key(i, f);
        let mut fx = LayerFx { animators: vec![Default::default(), Default::default()], ..Default::default() };
        for field in ["start", "end", "offset"] {
            fx.effect_keyframes_mut_for(&k(1, field)).expect(field).push(klipp_core::Keyframe {
                time_secs: 0.5, value: 0.25, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0),
                handles: Default::default(),
            });
        }
        let s = &fx.animators[1].selector;
        assert_eq!((s.start_keyframes.len(), s.end_keyframes.len(), s.offset_keyframes.len()), (1, 1, 1));
        assert!(fx.animators[0].selector.start_keyframes.is_empty());
        assert_eq!(fx.effect_keyframes_for(&k(1, "offset")).map(<[_]>::len), Some(1));
        assert!(fx.effect_keyframes_for(&k(2, "start")).is_none() && fx.effect_keyframes_for(&k(0, "amount")).is_none());
        // The limiting selector's range keys as `within.*`, once it has one.
        assert!(fx.effect_keyframes_mut_for(&k(0, "within.start")).is_none());
        fx.animators[0].within = Some(Default::default());
        assert!(fx.effect_keyframes_mut_for(&k(0, "within.end")).is_some());
        assert_eq!(fx.effect_keyframes_for(&k(0, "within.offset")).map(<[_]>::len), Some(0));
        assert!(!serde_json::to_string(&LayerFx::default()).unwrap().contains("animators"));
        assert!(LayerFx::default().is_default());
    }
}

#[cfg(test)]
mod gradient_key_tests {
    use super::*;
    use crate::GradientStop;

    fn three() -> TextGradient {
        let stop = |position: f32, color: [u8; 4]| GradientStop { position, color };
        TextGradient::with_stops(&[stop(0.0, [255, 0, 0, 255]), stop(0.5, [0, 255, 0, 255]), stop(1.0, [0, 0, 255, 255])], Default::default(), true)
    }

    /// A stop's colour keyed blends in linear light; the pair's ends are
    /// its first and last stops'; the keys go once evaluated (#1511).
    #[test]
    fn a_keyed_stop_blends_and_the_ends_follow() {
        let mut g = TextGradient::pair([255, 0, 0, 255], [0, 0, 0, 255], true);
        let mut keys = ColourKeys::default();
        keys.set(0.0, [255, 0, 0, 255]);
        keys.set(2.0, [0, 0, 255, 255]);
        g.stop_keyframes = vec![keys.clone()];
        assert!(g.has_keyframes());
        let at = g.at(1.0);
        assert_eq!((at.top_color, at.bottom_color, at.stop_keyframes.is_empty(), at.stops.is_empty()), ([188, 0, 188, 255], [0, 0, 0, 255], true, true));
        let mut g3 = three();
        g3.stop_keyframes = vec![ColourKeys::default(), ColourKeys::default(), keys];
        let at = g3.at(2.0);
        assert_eq!((at.stops[2].color, at.bottom_color, at.top_color), ([0, 0, 255, 255], [0, 0, 255, 255], [255, 0, 0, 255]));
        assert!(!serde_json::to_string(&TextGradient::pair([0; 4], [0; 4], true)).unwrap().contains("stop_keyframes"));
        let mut tb = TextBox { gradient: Some(g3.clone()), ..Default::default() };
        assert!(tb.has_keyframes());
        assert_eq!(tb.at(1.0).gradient.unwrap().stops[2].color, [188, 0, 188, 255]);
        assert_eq!(tb.effect_keyframes_for("gradient.stop.2.b").map(<[_]>::len), Some(2));
        assert_eq!(tb.effect_keyframes_for("gradient.stop.0.r").map(<[_]>::len), Some(0));
        assert!(tb.effect_keyframes_for("gradient.stop.3.r").is_none(), "no fourth stop");
        assert!(tb.effect_keyframes_mut_for("gradient.stop.1.g").is_some());
    }

    /// Stops added, taken away or dragged keep their own keys.
    #[test]
    fn stop_keys_stay_with_their_stops() {
        let mut keyed = three();
        let mut k = ColourKeys::default();
        k.set(0.0, [9, 9, 9, 255]);
        keyed.stop_keyframes = vec![ColourKeys::default(), ColourKeys::default(), k.clone()];
        let stop = |position: f32| GradientStop { position, color: [1, 2, 3, 255] };
        // One added before the last: the last keeps its keys.
        let mut added = TextGradient::with_stops(&[stop(0.0), stop(0.25), stop(0.5), stop(1.0)], Default::default(), true);
        added.carry_keys_from(&keyed);
        assert_eq!(added.stop_keyframes.iter().map(ColourKeys::is_empty).collect::<Vec<_>>(), [true, true, true, false]);
        // The middle one taken away: the last still keeps them.
        let mut removed = TextGradient::with_stops(&[stop(0.0), stop(1.0)], Default::default(), true);
        removed.carry_keys_from(&keyed);
        assert_eq!(removed.stop_keyframes.get(1), Some(&k));
        // The last dragged: as many stops, so each keeps its own.
        let mut dragged = TextGradient::with_stops(&[stop(0.0), stop(0.5), stop(0.9)], Default::default(), true);
        dragged.carry_keys_from(&keyed);
        assert_eq!(dragged.stop_keyframes.get(2), Some(&k));
        // A stop without keys left none behind.
        let mut plain = three();
        plain.carry_keys_from(&three());
        assert!(plain.stop_keyframes.is_empty());
    }
}

#[cfg(test)]
mod text3d_key_tests {
    use crate::Text3d;

    fn key(t: f64, v: f32) -> klipp_core::Keyframe {
        klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// The text's 3D keys are its own keyable params, as `text3d.*`,
    /// and make the text keyed.
    #[test]
    fn a_3d_texts_keys_are_text_params() {
        let mut tb = crate::TextBox { three_d: Some(Text3d::default()), ..Default::default() };
        assert!(!tb.has_keyframes());
        tb.effect_keyframes_mut_for("text3d.rotate_y").unwrap().push(key(0.0, 15.0));
        assert!(tb.has_keyframes());
        assert_eq!(tb.effect_keyframes_for("text3d.rotate_y").map(<[_]>::len), Some(1));
        assert!(tb.effect_keyframes_for("text3d.spin").is_none());
        assert_eq!(tb.at(0.0).three_d.unwrap().rotation[1], 15.0);
        assert!(crate::TextBox::default().effect_keyframes_for("text3d.depth").is_none(), "no 3D, no keys");
    }
}

#[cfg(test)]
mod styled_like_tests {
    use super::*;

    fn titled(text: &str, font: &str, size: f32) -> TitleData {
        TitleData::single_text(TextBox { text: text.into(), font_name: font.into(), font_size: size, ..Default::default() })
    }

    /// #1718: a title styled like another takes its look and keeps its
    /// words: all of it when the two have the same layers, else the main
    /// text layer's type and effects, its own place and path kept.
    #[test]
    fn a_title_styled_like_another_keeps_its_words() {
        let mut look = titled("Look", "Libre Baskerville", 72.0);
        look.objects[0].fx_mut().glows.push(TextGlow { enabled: true, color: [0, 255, 255, 255], radius: 18.0, radius_keyframes: Vec::new(), intensity: 0.7, intensity_keyframes: Vec::new() });
        look.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 12.0 };
        let mut target = titled("Ana Diaz", "Inter", 40.0);
        if let TitleObjectContent::TextBox(tb) = &mut target.objects[0].content {
            tb.style_runs = vec![StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(0, 3) }];
        }
        let styled = look.styled_like(&target).unwrap();
        let tb = styled.primary_text().unwrap();
        assert_eq!((tb.text.as_str(), tb.font_name.as_str(), tb.font_size), ("Ana Diaz", "Libre Baskerville", 72.0));
        assert_eq!(tb.style_runs, target.primary_text().unwrap().style_runs, "its styled runs too");
        assert_eq!(styled.primary_fx().glows.len(), 1);
        assert_eq!(styled.background, look.background);

        // A target with another layer: only the main text's look goes on it.
        let mut two = titled("Ben Holt", "Inter", 40.0);
        two.add_object(TitleObjectContent::TextBox(TextBox { text: "Producer".into(), ..Default::default() }), default_text_transform());
        two.objects[0].transform.center.x = 0.2;
        let styled = look.styled_like(&two).unwrap();
        assert_eq!(styled.objects.len(), 2, "its layers kept");
        let tb = styled.primary_text().unwrap();
        assert_eq!((tb.text.as_str(), tb.font_name.as_str()), ("Ben Holt", "Libre Baskerville"));
        assert_eq!(styled.objects[0].transform.center.x, 0.2, "its place kept");
        assert!(matches!(&styled.objects[1].content, TitleObjectContent::TextBox(tb) if tb.text == "Producer" && tb.font_name != "Libre Baskerville"), "the other layer as it was");
    }
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;

    /// A duplicate is the layer again, with a new id, named a copy, just
    /// above the original; the ids after it keep counting on (#1715).
    #[test]
    fn a_duplicate_sits_above_its_original_with_an_id_of_its_own() {
        let mut td = TitleData::single_text(TextBox { text: "Hi".into(), ..Default::default() });
        let first = td.objects[0].id;
        td.objects[0].name = "Name".into();
        let shape = td.add_object(TitleObjectContent::TextBox(TextBox::default()), default_text_transform());
        let copy = td.duplicate_object(first).unwrap();
        assert_eq!(td.objects.iter().map(|o| o.id).collect::<Vec<_>>(), [first, copy, shape]);
        assert_ne!(copy, first);
        assert_ne!(copy, shape);
        assert_eq!(td.objects[1].name, "Name copy");
        assert_eq!(td.objects[1].content, td.objects[0].content);
        let next = td.add_object(TitleObjectContent::TextBox(TextBox::default()), default_text_transform());
        assert!(next.0 > copy.0, "ids keep counting on");
        assert_eq!(td.duplicate_object(TitleObjectId(99)), None);
    }
}
