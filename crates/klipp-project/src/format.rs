use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectJson {
    pub version: String,
    pub klipp_version_min: String,
    /// The Klipp build version that wrote this file.
    #[serde(default)]
    pub klipp_version: Option<String>,
    /// ISO 8601 timestamp of when the project was saved.
    #[serde(default)]
    pub saved_at: Option<String>,
    /// ISO 8601 timestamp of when the project was first created.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Platform that saved the project (e.g. "linux-x86_64").
    #[serde(default)]
    pub platform: Option<String>,
    /// All sequences in this project. Always at least one.
    pub sequences: Vec<SequenceJson>,
    /// Which sequence was active when the project was last saved.
    #[serde(default)]
    pub active_sequence_id: Option<String>,
    /// Free-form project notes (shot list, delivery specs, etc.).
    #[serde(default)]
    pub notes: String,
    /// Last timeline zoom (px-per-second) when the project was saved.
    /// Restored on load so view state survives across sessions — including
    /// the result of "Fit Timeline to Window". `None` for legacy files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_zoom: Option<f32>,
    /// Last horizontal timeline scroll position when saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_scroll_x: Option<f32>,
    /// Working colorspace for this project's grade chain. Old project
    /// files written before Phase E lack this field; serde default
    /// gives them `Rec709Linear`, matching the pipeline's behaviour
    /// before the upgrade so existing projects open unchanged.
    #[serde(default)]
    pub working_colorspace: klipp_core::WorkingColorspace,
    /// Project-scoped synthetic title assets. Each entry is keyed by
    /// the `MediaId` of its synthetic `MediaItem` (the entry that
    /// shows up in the Media tab) and carries the canonical
    /// `TitleData` + display name + animation token. Empty for
    /// projects saved before the synthetic-asset feature shipped —
    /// `#[serde(default)]` keeps backward compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub title_assets: Vec<TitleAssetJson>,
}

/// JSON projection of a `klipp_timeline::TitleAsset`. Stored
/// alongside the rest of the project (one centralized list rather
/// than per-sequence) so the same asset is reusable across every
/// sequence in the project.
#[derive(Debug, Serialize, Deserialize)]
pub struct TitleAssetJson {
    /// Stable UUID — must match the `MediaItem.id` of the synthetic
    /// pool entry that represents this asset.
    pub id: String,
    pub display_name: String,
    /// `TitleData` is JSON-serializable directly (see `TitleData`'s
    /// `Serialize` derive). We store it as a nested object rather
    /// than a string so future field additions inherit serde's own
    /// backward-compat handling.
    pub data: serde_json::Value,
    #[serde(default)]
    pub animation: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SequenceJson {
    pub id: String,
    pub name: String,
    pub fps: FpsJson,
    pub resolution: ResolutionJson,
    pub tracks: Vec<TrackJson>,
    #[serde(default)]
    pub in_point: Option<TimecodeJson>,
    #[serde(default)]
    pub out_point: Option<TimecodeJson>,
    #[serde(default)]
    pub global_lut_path: Option<String>,
    #[serde(default = "default_lut_opacity")]
    pub global_lut_opacity: f32,
    #[serde(default = "default_true")]
    pub global_lut_enabled: bool,
    #[serde(default)]
    pub markers: Vec<MarkerJson>,
    #[serde(default = "default_header_width")]
    pub track_header_width: f32,
    #[serde(default)]
    pub last_playhead: Option<TimecodeJson>,
    /// Per-project filmstrip generation override. None = use global pref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_generate_filmstrips: Option<bool>,
    /// Per-project skim transcode override. None = use global pref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_generate_skim: Option<bool>,
    /// What the piece is for. Absent in projects saved before there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<klipp_timeline::Brief>,
    /// The paper edit awaiting or past approval. Absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_plan: Option<klipp_timeline::EditPlan>,
    /// The move over the whole sequence. Absent when there is no camera,
    /// which is every project saved before there could be one.
    #[serde(default, skip_serializing_if = "klipp_timeline::Camera::is_default")]
    pub camera: klipp_timeline::Camera,
    /// Whether positions are counted drop-frame; see
    /// `Sequence::drop_frame`.
    ///
    /// `Option` so that absent and `false` stay distinguishable: a
    /// project saved before this existed says nothing, and takes the
    /// rate's default on load rather than being read as a deliberate
    /// "non-drop". A sequence that has chosen writes the choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_frame: Option<bool>,
}

fn default_header_width() -> f32 { 160.0 }
fn default_lut_opacity() -> f32 { 1.0 }
fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimecodeJson {
    pub value: i64,
    pub rate: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FpsJson {
    pub num: u32,
    pub den: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResolutionJson {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackJson {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub clips: Vec<ClipJson>,
    #[serde(default)]
    pub transitions: Vec<TransitionJson>,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_height: Option<f32>,
    /// Per-track automation lane state. `None` = collapsed, `Some(param)`
    /// = lane open showing the named param's curve. Stored as the
    /// stringified `AudioParam` variant ("Volume" / "Pan" / "Gain").
    /// Skipped when None so legacy projects round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_visible: Option<String>,
    /// User-overridden lane height in pixels. `None` falls back to
    /// the default lane height. Skipped when None so legacy projects
    /// round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_height: Option<f32>,
    /// The track's magnetic role. `None` only in files written before it
    /// was saved; the loader then gives such a sequence the roles a new
    /// one gets (first video track Primary, first audio track Connected),
    /// which is what every reload had silently taken away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnetic_role: Option<klipp_timeline::TrackRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline_anchor: Option<klipp_timeline::MagneticAnchor>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransitionJson {
    pub id: String,
    pub kind: String,
    pub duration: TimecodeJson,
    pub clip_a_id: String,
    pub clip_b_id: String,
    /// The user's per-kind settings (feather, radius, …). Absent in files
    /// written before they were saved at all; the loader then stamps the
    /// kind's defaults, which is what those files had shown.
    #[serde(default)]
    pub user_params: klipp_core::TransitionUserParams,
    /// Where the transition sits relative to the cut. Absent → centred,
    /// which is what older files rendered.
    #[serde(default)]
    pub alignment: klipp_core::TransitionAlignment,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_visible() -> bool { true }

fn default_rate() -> u32 { 24 }

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipJson {
    pub id: String,
    /// Old field — kept for backward compatibility when loading old projects.
    #[serde(default)]
    pub media_id: Option<String>,
    /// New field — used for all new saves.
    #[serde(default)]
    pub kind: Option<ClipKindJson>,
    pub timeline_in: i64,
    pub timeline_out: i64,
    pub source_in: i64,
    pub source_out: i64,
    #[serde(default = "default_rate")]
    pub rate: u32,
    /// Node-based color grading graph (new format).
    /// Missing → default 12-node pipeline. Old `color_grade` field is ignored on load.
    #[serde(default)]
    pub node_graph: Option<klipp_nodes::NodeGraph>,
    /// Legacy color_grade field — kept only so old project files still parse.
    #[serde(default)]
    pub color_grade: Option<serde_json::Value>,
    #[serde(default)]
    pub effects: Vec<EffectJson>,
    #[serde(default)]
    pub builtin_effects: Option<serde_json::Value>,
    /// Per-effect enabled flags, parallel to `builtin_effects`; a missing
    /// entry means enabled. Empty is left out of the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builtin_effects_enabled: Vec<bool>,
    /// The clip's magnetic anchor, when it is a connected clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnetic_anchor: Option<klipp_timeline::MagneticAnchor>,
    #[serde(default)]
    pub linked_clip_id: Option<String>,
    /// Project-asset MediaId this clip is a linked instance of.
    /// Serialized as a UUID string (matches the rest of the project
    /// format's ID encoding).
    #[serde(default)]
    pub linked_title_asset_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub gain: Option<f32>,
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub pan: Option<f32>,
    #[serde(default)]
    pub fade_in_secs: Option<f32>,
    #[serde(default)]
    pub fade_out_secs: Option<f32>,
    #[serde(default)]
    pub fade_in_curve: Option<klipp_timeline::FadeCurve>,
    #[serde(default)]
    pub fade_out_curve: Option<klipp_timeline::FadeCurve>,
    #[serde(default)]
    pub channel_config: Option<klipp_timeline::ChannelConfig>,
    #[serde(default)]
    pub volume_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub pan_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub gain_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub opacity_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_pos_x_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_pos_y_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_scale_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_rotation_keyframes: Option<Vec<klipp_core::Keyframe>>,
    /// 3D tilt tracks; absent for a flat picture, which is every clip
    /// saved before there could be tilt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_x_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_y_keyframes: Option<Vec<klipp_core::Keyframe>>,
    /// Phase 4D — keyframable feather radius for the track-matte
    /// binding. Optional + skip-if-empty so projects saved before
    /// this landed deserialise unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_feather_keyframes: Option<Vec<klipp_core::Keyframe>>,
    /// Phase 4D — keyframable choke radius. Same back-compat shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_choke_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub clip_markers: Option<Vec<ClipMarkerJson>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_keyframes: Option<std::collections::HashMap<String, Vec<klipp_core::Keyframe>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<klipp_core::ClipRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_modifiers: Option<HashMap<String, klipp_core::ModifierStack>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rotation: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_effects: Option<Vec<klipp_core::AudioEffectInstance>>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam_angle: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicam_cuts: Vec<(f64, u8)>,
    #[serde(default)]
    pub solo: Option<bool>,
    /// Per-clip blend mode (Normal, Multiply, Screen, …). Older projects
    /// without this field load as `Normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<klipp_core::BlendMode>,
    /// How far back the clip sits for the sequence camera. Absent means 1,
    /// the composed plane, which every clip saved before there was depth is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    /// Track-matte binding (this clip's alpha is masked by another clip's
    /// rendered RGBA). `None` = no matte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_source: Option<MatteBindingJson>,
    /// Phase 4B — stencil mode: this clip's pixels mask every clip on
    /// a lower track at the same time. None = not a stencil.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_stencil: Option<StencilSettingsJson>,
    /// True when this clip is being used as a matte source by another clip
    /// AND should be hidden from the visible composite. Default false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_as_matte: Option<bool>,
    /// Optional transform inheritance binding. None = no link.
    /// Skipped on save when None to keep the wire format quiet for
    /// the dominant unlinked case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_link: Option<TransformLinkJson>,
    #[serde(default)]
    pub grade_versions: Vec<GradeVersionJson>,
    #[serde(default)]
    pub active_grade_version: Option<usize>,
    #[serde(default)]
    pub track_points: Vec<klipp_core::TrackPoint>,
}

/// Wire-format mirror of [`klipp_timeline::MatteBinding`]. The clip-id is
/// stored as a string for stable cross-platform serialisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatteBindingJson {
    pub source_clip_id: String,
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub feather_px: f32,
    #[serde(default)]
    pub choke_px: f32,
}

/// Wire-format mirror of [`klipp_timeline::TransformLink`]. The
/// target clip-id is stored as a string for stable cross-platform
/// serialisation; the inherit flags default to `true` so an old
/// project file that learns about a partial-spec link (e.g. just
/// `target` + `mode`) deserialises with full inheritance — the same
/// shape as the easy-path `TransformLink::new` constructor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformLinkJson {
    /// Migrated from `String` (bare uuid) to
    /// `klipp_timeline::TransformLinkTarget` to support tracker
    /// targets (HZ-MATTE-NEXT-TRACKER-DRIVER). The
    /// `#[serde(untagged)]` on `TransformLinkTarget` means legacy
    /// project files with `"target": "<uuid>"` still load — the
    /// bare-uuid form deserialises into `Clip(...)`. Tagged-object
    /// form deserialises into `Tracker { ... }`. Round-trip
    /// stability: serializing `Clip(uuid)` produces the same bare
    /// uuid string the legacy field used. Tests pin the migration
    /// shape directly.
    pub target: klipp_timeline::TransformLinkTarget,
    #[serde(default)]
    pub mode: klipp_timeline::LinkMode,
    #[serde(default = "default_true_json")]
    pub inherit_scale: bool,
    #[serde(default = "default_true_json")]
    pub inherit_position: bool,
    #[serde(default = "default_true_json")]
    pub inherit_rotation: bool,
    #[serde(default = "default_true_json")]
    pub inherit_anchor: bool,
}

fn default_true_json() -> bool { true }

/// Phase 4B wire-format mirror of [`klipp_timeline::StencilSettings`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StencilSettingsJson {
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub feather_px: f32,
    #[serde(default)]
    pub choke_px: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeVersionJson {
    pub name: String,
    pub node_graph: klipp_nodes::NodeGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipMarkerJson {
    pub time_secs: f64,
    #[serde(default)]
    pub name: String,
    pub color: String,
    /// Optional free-text comment. Skipped on serialise when empty so
    /// legacy projects don't pick up a noisy field; legacy files
    /// without this field deserialise to "".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

// 560 bytes in the Title variant, but this is the on-disk DTO: one
// exists at a time while a clip is being written or read, never in a
// collection. Boxing would buy nothing and add an allocation per clip.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipKindJson {
    Media {
        media_id: String,
    },
    Title {
        /// The whole `TitleData`, verbatim: every object, its transform,
        /// the animation, the background. The flat fields that follow are
        /// the legacy single-text shape, still written so older readers
        /// open the file; a reader that finds `title_data` uses it and
        /// ignores them. Before this the flat shape was all that was
        /// saved, and a title lost its animation and every object after
        /// the first on each reload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title_data: Option<serde_json::Value>,
        text: String,
        #[serde(default = "default_font_size")]
        font_size: f32,
        #[serde(default = "default_font_name")]
        font_name: String,
        #[serde(default = "default_font_style")]
        font_style: String,
        #[serde(default = "default_color_white")]
        color: [u8; 4],
        #[serde(default = "default_alignment")]
        alignment: String,
        #[serde(default = "default_half")]
        position_x: f32,
        #[serde(default = "default_half")]
        position_y: f32,
        #[serde(default)]
        background: serde_json::Value,
        #[serde(default)]
        shadow: serde_json::Value,
        /// Full drop-shadow stack. Optional so legacy projects with
        /// only the `shadow` field continue to load — load.rs falls
        /// back to synthesising a single-element vec from the legacy
        /// field when this is empty/absent.
        #[serde(default)]
        shadows: serde_json::Value,
        /// Soft-disable for the secondary shadow stack (`shadows[1..]`).
        /// Defaults true so legacy projects render unchanged.
        #[serde(default = "default_true")]
        shadow_stack_enabled: bool,
        #[serde(default)]
        outline: serde_json::Value,
        #[serde(default)]
        outlines: serde_json::Value,
        /// Soft-disable for the secondary outline stack
        /// (`outlines[1..]`). Defaults to true so projects saved
        /// before this field existed render the stack as before.
        #[serde(default = "default_true")]
        outline_stack_enabled: bool,
        #[serde(default)]
        glow: serde_json::Value,
        /// Full glow stack (canonical). Optional so legacy projects
        /// with only the singular `glow` continue to load.
        #[serde(default)]
        glows: serde_json::Value,
        /// Soft-disable for the secondary glow stack.
        #[serde(default = "default_true")]
        glow_stack_enabled: bool,
        #[serde(default)]
        gradient: serde_json::Value,
        #[serde(default)]
        inner_shadow: serde_json::Value,
        /// Full inner-shadow stack (canonical). Optional so legacy
        /// projects with only the singular `inner_shadow` continue
        /// to load — load.rs falls back to wrapping the legacy field
        /// as a one-element vec.
        #[serde(default)]
        inner_shadows: serde_json::Value,
        /// Soft-disable for the secondary inner-shadow stack.
        #[serde(default = "default_true")]
        inner_shadow_stack_enabled: bool,
        #[serde(default)]
        pattern_fill: serde_json::Value,
        #[serde(default)]
        tracking: f32,
        #[serde(default = "default_one_f32")]
        line_spacing: f32,
        #[serde(default = "default_one_f32")]
        write_on: f32,
        #[serde(default)]
        extrusion: serde_json::Value,
        #[serde(default)]
        text_path: serde_json::Value,
        #[serde(default)]
        char_animation: serde_json::Value,
        /// Legacy texture path — kept for downgrade compatibility
        /// and read by load.rs when no `texture_fill` struct is
        /// present. New saves still populate this when the source
        /// is a file so older Klipp versions can keep rendering.
        #[serde(default)]
        texture_fill_path: Option<String>,
        /// Full TextureFill struct (source / tile_mode / scale /
        /// offset / rotation / opacity / tint). Optional so legacy
        /// projects that only have `texture_fill_path` still load.
        #[serde(default)]
        texture_fill: serde_json::Value,
        #[serde(default)]
        motion_blur: serde_json::Value,
        /// Per-character style runs for rich text.
        #[serde(default)]
        style_runs: serde_json::Value,
    },
    Gap,
    Sequence {
        sequence_id: String,
    },
    Generator {
        #[serde(default)]
        data: serde_json::Value,
    },
    AdjustmentLayer,
}

fn default_one_f32() -> f32 { 1.0 }
fn default_font_size() -> f32 { 72.0 }
fn default_font_name() -> String { "sans-serif".to_string() }
fn default_font_style() -> String { "Regular".to_string() }
fn default_color_white() -> [u8; 4] { [255, 255, 255, 255] }
fn default_alignment() -> String { "Center".to_string() }
fn default_half() -> f32 { 0.5 }

#[derive(Debug, Serialize, Deserialize)]
pub struct EffectJson {
    pub plugin_name: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
    #[serde(default)]
    pub param_values: HashMap<String, OfxValueJson>,
}

/// OFX parameter value for JSON serialisation.
/// Uses tagged format for unambiguous round-trips.
/// Old projects (untagged) are handled by a custom Deserialize fallback.
#[derive(Debug, Serialize)]
#[serde(tag = "t", content = "v")]
pub enum OfxValueJson {
    Double(f64),
    Double2([f64; 2]),
    Double3([f64; 3]),
    Int(i32),
    Int2([i32; 2]),
    Int3([i32; 3]),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba([f64; 4]),
    Rgb([f64; 3]),
}

// Custom Deserialize: try tagged first, then fall back to untagged for old projects.
impl<'de> serde::Deserialize<'de> for OfxValueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        use serde_json::Value;

        let v = Value::deserialize(deserializer)?;

        // Tagged format: {"t": "Double", "v": 1.0}
        if let Some(tag) = v.get("t").and_then(|t| t.as_str()) {
            return match tag {
                "Double" => v.get("v").and_then(|x| x.as_f64()).map(OfxValueJson::Double)
                    .ok_or_else(|| de::Error::custom("invalid Double value")),
                "Double2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double2).map_err(de::Error::custom),
                "Double3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double3).map_err(de::Error::custom),
                "Int" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Int(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Int value")),
                "Int2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int2).map_err(de::Error::custom),
                "Int3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int3).map_err(de::Error::custom),
                "Bool" => v.get("v").and_then(|x| x.as_bool()).map(OfxValueJson::Bool)
                    .ok_or_else(|| de::Error::custom("invalid Bool value")),
                "Choice" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Choice(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Choice value")),
                "String" => v.get("v").and_then(|x| x.as_str()).map(|s| OfxValueJson::String(s.to_string()))
                    .ok_or_else(|| de::Error::custom("invalid String value")),
                "Rgba" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgba).map_err(de::Error::custom),
                "Rgb" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgb).map_err(de::Error::custom),
                _ => Err(de::Error::custom(format!("unknown OfxValueJson tag: {tag}"))),
            };
        }

        // Legacy untagged format: bare JSON values
        if let Some(b) = v.as_bool() {
            return Ok(OfxValueJson::Bool(b));
        }
        if let Some(s) = v.as_str() {
            return Ok(OfxValueJson::String(s.to_string()));
        }
        if let Some(n) = v.as_i64() {
            // Old format stored Int as i64
            return Ok(OfxValueJson::Int(n as i32));
        }
        if let Some(n) = v.as_f64() {
            return Ok(OfxValueJson::Double(n));
        }

        Err(de::Error::custom("cannot parse OfxValueJson"))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaManifestJson {
    pub version: String,
    pub items: Vec<MediaItemJson>,
    #[serde(default)]
    pub bins: Vec<BinJson>,
    /// The saved smart bins, as they are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_bins: Vec<klipp_core::SmartBin>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BinJson {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaItemJson {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub duration_frames: i64,
    #[serde(default = "default_rate")]
    pub duration_rate: u32,
    pub fps: FpsJson,
    pub resolution: ResolutionJson,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub audio_channels: Option<u16>,
    #[serde(default)]
    pub is_still: bool,
    #[serde(default)]
    pub proxy_path: Option<String>,
    #[serde(default)]
    pub proxy_ready: bool,
    #[serde(default)]
    pub default_source_in: Option<TimecodeJson>,
    #[serde(default)]
    pub default_source_out: Option<TimecodeJson>,
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub user_notes: String,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub bin_id: Option<String>,
    /// Subclip parent MediaId as a UUID string. None for normal items.
    /// Missing from legacy `.klipp` files — serde default gives us `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_media_id: Option<String>,
    #[serde(default)]
    pub added_at: Option<u64>,
    #[serde(default)]
    pub file_modified_at: Option<u64>,
    #[serde(default)]
    pub media_created_at: Option<u64>,
    #[serde(default)]
    pub color_label: u8,
    #[serde(default)]
    pub colour_space: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rotation: Option<i32>,
    // Probed metadata — preserved across save/load so the log-format
    // heuristic (`MediaItem::detect_log_format`) still works on
    // re-load. Without these the project file forgets the camera
    // tags and every reopen falls back to Srgb on cameras whose
    // make/model isn't tagged in the container (Sony A6700, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_bitrate_kbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_primaries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_transfer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    /// User's per-media log format choice. Promoted from the
    /// Inspector dropdown when the user picks a format on a clip
    /// whose source media is still on the Srgb default — see
    /// `apply_detected_log_format` in app.rs.
    #[serde(default, skip_serializing_if = "is_default_log_format")]
    pub log_format: klipp_core::LogFormat,
    /// The user explicitly chose `log_format` — including choosing the
    /// default, sRGB, to override a detected log. Older projects lack it;
    /// a non-default `log_format` there was necessarily a choice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub log_format_user_set: bool,
}

fn is_default_log_format(f: &klipp_core::LogFormat) -> bool {
    *f == klipp_core::LogFormat::default()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MarkerJson {
    pub id: String,
    pub position: TimecodeJson,
    pub label: String,
    #[serde(default = "default_marker_color_str")]
    pub color: String,
    /// Optional range-marker duration. `None` serializes as absent; legacy
    /// files without the field deserialize to `None` via the serde default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<TimecodeJson>,
    /// Free-text comment / note. Empty by default. Skipped on
    /// serialise when empty so legacy projects don't pick up a noisy
    /// field; legacy files without this field deserialise to "".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Marker kind / role tag. Stored as an enum-name string
    /// ("Chapter" / "ToDo" / "Comment" / "BestTake" / "Section") for
    /// stability across versions. Default "None" is skipped on
    /// serialise; legacy files without this field deserialise to "None".
    #[serde(default = "default_marker_kind_str", skip_serializing_if = "is_none_kind")]
    pub kind: String,
}

fn default_marker_kind_str() -> String {
    "None".to_string()
}

fn is_none_kind(s: &String) -> bool {
    s == "None"
}

fn default_marker_color_str() -> String {
    "Blue".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.klipp` project.json from before Phase E (no `working_colorspace`
    /// field) must deserialise as `Rec709Linear` — that's the
    /// back-compat contract for any existing project on disk.
    #[test]
    fn legacy_project_json_defaults_working_colorspace_to_rec709_linear() {
        let legacy = r#"{
            "version": "1.0.0",
            "klipp_version_min": "0.1.0",
            "sequences": []
        }"#;
        let project: ProjectJson = serde_json::from_str(legacy)
            .expect("legacy project.json should deserialise");
        assert_eq!(
            project.working_colorspace,
            klipp_core::WorkingColorspace::Rec709Linear,
            "old projects without working_colorspace must default to Rec709Linear",
        );
    }

    /// Round-trip with a non-default value to confirm serde writes
    /// and reads the field correctly. `AcesCG` is the value that
    /// distinguishes a wide-gamut working space from the legacy default.
    #[test]
    fn working_colorspace_round_trips_when_set() {
        let project = ProjectJson {
            version: "1.0.0".to_string(),
            klipp_version_min: "0.1.0".to_string(),
            klipp_version: None,
            saved_at: None,
            created_at: None,
            platform: None,
            sequences: vec![],
            active_sequence_id: None,
            notes: String::new(),
            timeline_zoom: None,
            timeline_scroll_x: None,
            working_colorspace: klipp_core::WorkingColorspace::AcesCG,
            title_assets: Vec::new(),
        };
        let serialised = serde_json::to_string(&project).expect("serialise");
        let back: ProjectJson = serde_json::from_str(&serialised).expect("deserialise");
        assert_eq!(
            back.working_colorspace,
            klipp_core::WorkingColorspace::AcesCG,
        );
    }

    /// Pin the on-disk variant names. The string tags are part of the
    /// `.klipp` file format contract — a future enum rename must
    /// migrate, not silently break old projects on load.
    #[test]
    fn working_colorspace_serialises_to_stable_names() {
        use klipp_core::WorkingColorspace;
        assert_eq!(
            serde_json::to_string(&WorkingColorspace::Rec709Linear).unwrap(),
            "\"Rec709Linear\"",
        );
        assert_eq!(
            serde_json::to_string(&WorkingColorspace::AcesCG).unwrap(),
            "\"AcesCG\"",
        );
        assert_eq!(
            serde_json::to_string(&WorkingColorspace::Bt2020Linear).unwrap(),
            "\"Bt2020Linear\"",
        );
    }

    /// JSON round-trip for `audio_effect_keyframes` on `ClipJson`. Phase 1
    /// of the audio-keyframe support: storing the field on a clip is
    /// useless if save/load drops it on the floor, so we pin the
    /// behaviour with a focused serde roundtrip.
    ///
    /// Three properties pinned here:
    /// 1. The field round-trips with values intact (kf time + value + interp).
    /// 2. The reserved `__bypass` and `__wetdry` keys survive (they're
    ///    plain strings to serde — no special handling).
    /// 3. An empty map skips serialisation (`skip_serializing_if`) so old
    ///    projects don't grow a noisy empty field on every save.
    #[test]
    fn audio_effect_keyframes_round_trip_through_clip_json() {
        let mut audio_kfs: std::collections::HashMap<String, Vec<klipp_core::Keyframe>> =
            std::collections::HashMap::new();
        audio_kfs.insert(
            "0:gain".to_string(),
            vec![
                klipp_core::Keyframe {
                    time_secs: 0.0,
                    value: -6.0,
                    interpolation: klipp_core::Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                klipp_core::Keyframe {
                    time_secs: 1.5,
                    value: 0.0,
                    interpolation: klipp_core::Interpolation::Bezier,
                    tangent_in: (-0.3, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
        );
        audio_kfs.insert(
            "0:__bypass".to_string(),
            vec![klipp_core::Keyframe {
                time_secs: 0.5,
                value: 1.0,
                interpolation: klipp_core::Interpolation::Step,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
            }],
        );

        let clip = ClipJson {
            id: "test-clip".to_string(),
            media_id: None,
            kind: None,
            timeline_in: 0,
            timeline_out: 48,
            source_in: 0,
            source_out: 48,
            rate: 24,
            node_graph: None,
            color_grade: None,
            effects: vec![],
            builtin_effects: None,
            builtin_effects_enabled: Vec::new(),
            magnetic_anchor: None,
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            gain: None, volume: None, pan: None,
            fade_in_secs: None, fade_out_secs: None,
            fade_in_curve: None, fade_out_curve: None,
            channel_config: None,
            volume_keyframes: None, pan_keyframes: None, gain_keyframes: None,
            opacity_keyframes: None,
            transform_pos_x_keyframes: None, transform_pos_y_keyframes: None,
            transform_scale_keyframes: None, transform_rotation_keyframes: None, transform_tilt_x_keyframes: None, transform_tilt_y_keyframes: None,
            matte_feather_keyframes: None, matte_choke_keyframes: None,
            clip_markers: None,
            effect_keyframes: None,
            role: None,
            param_modifiers: None,
            speed: None, speed_keyframes: None,
            source_rotation: None,
            enabled: None,
            audio_effects: None,
            audio_effect_keyframes: audio_kfs.clone(),
            multicam_group: None, multicam_angle: None,
            multicam_cuts: vec![],
            solo: None,
            blend_mode: None,
            depth: None,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: None,
            transform_link: None,
            grade_versions: vec![],
            active_grade_version: None,
            track_points: vec![],
        };

        let serialised = serde_json::to_string(&clip).expect("serialise clip");
        let back: ClipJson = serde_json::from_str(&serialised).expect("deserialise clip");

        // 1. Both kf tracks survived.
        assert_eq!(back.audio_effect_keyframes.len(), 2);
        assert!(back.audio_effect_keyframes.contains_key("0:gain"));
        assert!(back.audio_effect_keyframes.contains_key("0:__bypass"));

        // 2. Values intact, including bezier tangents.
        let gain = &back.audio_effect_keyframes["0:gain"];
        assert_eq!(gain.len(), 2);
        assert!((gain[0].value - (-6.0)).abs() < 1e-6);
        assert!((gain[1].time_secs - 1.5).abs() < 1e-9);
        assert!(matches!(gain[1].interpolation, klipp_core::Interpolation::Bezier));
        assert!((gain[1].tangent_in.0 - (-0.3)).abs() < 1e-9);

        // 3. Reserved bypass key keeps its Step interp.
        let bypass = &back.audio_effect_keyframes["0:__bypass"];
        assert_eq!(bypass.len(), 1);
        assert!(matches!(bypass[0].interpolation, klipp_core::Interpolation::Step));
        assert!((bypass[0].value - 1.0).abs() < 1e-6);
    }

    /// `skip_serializing_if = "HashMap::is_empty"` keeps the on-disk
    /// representation lean: a clip with no audio kfs must not write the
    /// field at all. This pins that — if someone "cleans up" the attr
    /// later the JSON will get noisier and this test will catch it.
    #[test]
    fn audio_effect_keyframes_empty_map_is_omitted_from_json() {
        let clip = ClipJson {
            id: "test-clip".to_string(),
            media_id: None, kind: None,
            timeline_in: 0, timeline_out: 24,
            source_in: 0, source_out: 24, rate: 24,
            node_graph: None, color_grade: None,
            effects: vec![], builtin_effects: None, builtin_effects_enabled: Vec::new(), magnetic_anchor: None,
            linked_clip_id: None, linked_title_asset_id: None, group_id: None,
            gain: None, volume: None, pan: None,
            fade_in_secs: None, fade_out_secs: None,
            fade_in_curve: None, fade_out_curve: None,
            channel_config: None,
            volume_keyframes: None, pan_keyframes: None, gain_keyframes: None,
            opacity_keyframes: None,
            transform_pos_x_keyframes: None, transform_pos_y_keyframes: None,
            transform_scale_keyframes: None, transform_rotation_keyframes: None, transform_tilt_x_keyframes: None, transform_tilt_y_keyframes: None,
            matte_feather_keyframes: None, matte_choke_keyframes: None,
            clip_markers: None,
            effect_keyframes: None,
            role: None, param_modifiers: None,
            speed: None, speed_keyframes: None,
            source_rotation: None, enabled: None,
            audio_effects: None,
            audio_effect_keyframes: std::collections::HashMap::new(),
            multicam_group: None, multicam_angle: None,
            multicam_cuts: vec![],
            solo: None,
            blend_mode: None,
            depth: None,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: None,
            transform_link: None,
            grade_versions: vec![],
            active_grade_version: None,
            track_points: vec![],
        };
        let serialised = serde_json::to_string(&clip).expect("serialise");
        assert!(!serialised.contains("audio_effect_keyframes"),
            "empty audio_effect_keyframes must be skipped, got: {serialised}");
    }

    /// `Track.automation_lane_visible` survives the on-disk JSON round
    /// trip for every AudioParam variant. Without this the lane state
    /// silently resets on project reload (the bug uncovered while
    /// wiring the per-lane param picker).
    #[test]
    fn automation_lane_visible_round_trips_through_track_json() {
        for label in ["Volume", "Pan", "Gain"] {
            let track = TrackJson {
                magnetic_role: None,
                storyline_anchor: None,
                id: "t1".to_string(),
                name: "A1".to_string(),
                kind: "Audio".to_string(),
                clips: vec![],
                transitions: vec![],
                opacity: 1.0,
                volume: 1.0,
                muted: false,
                solo: false,
                visible: true,
                pan: 0.0,
                locked: false,
                custom_height: None,
                automation_lane_visible: Some(label.to_string()),
                automation_lane_height: None,
            };
            let s = serde_json::to_string(&track).expect("serialise");
            let back: TrackJson = serde_json::from_str(&s).expect("deserialise");
            assert_eq!(back.automation_lane_visible.as_deref(), Some(label));
        }
    }

    /// Legacy projects without the `automation_lane_visible` field
    /// must still parse, with the lane defaulting to collapsed.
    #[test]
    fn track_json_without_automation_lane_field_loads_as_collapsed() {
        let legacy = r#"{
            "id": "t1", "name": "A1", "kind": "Audio",
            "clips": [], "opacity": 1.0, "volume": 1.0, "pan": 0.0
        }"#;
        let track: TrackJson = serde_json::from_str(legacy)
            .expect("legacy track JSON without automation_lane_visible must parse");
        assert!(track.automation_lane_visible.is_none());
    }

    /// `None` lane state must be omitted from the on-disk JSON via
    /// `skip_serializing_if` so legacy projects don't accumulate a
    /// noisy null field on every save.
    #[test]
    fn collapsed_lane_state_is_omitted_from_serialised_track() {
        let track = TrackJson {
            magnetic_role: None,
            storyline_anchor: None,
            id: "t1".to_string(),
            name: "A1".to_string(),
            kind: "Audio".to_string(),
            clips: vec![],
            transitions: vec![],
            opacity: 1.0,
            volume: 1.0,
            muted: false,
            solo: false,
            visible: true,
            pan: 0.0,
            locked: false,
            custom_height: None,
            automation_lane_visible: None,
            automation_lane_height: None,
        };
        let s = serde_json::to_string(&track).expect("serialise");
        assert!(!s.contains("automation_lane_visible"),
            "None lane must be skipped, got: {s}");
        assert!(!s.contains("automation_lane_height"),
            "None lane height must be skipped, got: {s}");
    }

    /// `Track.automation_lane_height` round-trips through TrackJson
    /// when the user has overridden the default.
    #[test]
    fn automation_lane_height_round_trips_through_track_json() {
        let track = TrackJson {
            magnetic_role: None,
            storyline_anchor: None,
            id: "t1".to_string(),
            name: "A1".to_string(),
            kind: "Audio".to_string(),
            clips: vec![],
            transitions: vec![],
            opacity: 1.0,
            volume: 1.0,
            muted: false,
            solo: false,
            visible: true,
            pan: 0.0,
            locked: false,
            custom_height: None,
            automation_lane_visible: Some("Volume".to_string()),
            automation_lane_height: Some(180.0),
        };
        let s = serde_json::to_string(&track).expect("serialise");
        let back: TrackJson = serde_json::from_str(&s).expect("deserialise");
        assert_eq!(back.automation_lane_height, Some(180.0));
    }

    /// Legacy projects without `automation_lane_height` parse with
    /// the default-restoring None value (lane uses the default 120 px).
    #[test]
    fn track_json_without_automation_lane_height_loads_as_none() {
        let legacy = r#"{
            "id": "t1", "name": "A1", "kind": "Audio",
            "clips": [], "opacity": 1.0, "volume": 1.0, "pan": 0.0,
            "automation_lane_visible": "Volume"
        }"#;
        let track: TrackJson = serde_json::from_str(legacy)
            .expect("legacy track JSON without automation_lane_height must parse");
        assert!(track.automation_lane_height.is_none());
        assert_eq!(track.automation_lane_visible.as_deref(), Some("Volume"));
    }

    // ════════════════════════════════════════════════════════════════
    // Track-matte / blend-mode wire-format round trip (Phase 1.5)
    // ════════════════════════════════════════════════════════════════

    /// Build a minimal `ClipJson` with all fields defaulted; tests fill in
    /// only what they're exercising.
    fn empty_clip_json() -> ClipJson {
        ClipJson {
            id: "test-clip".to_string(),
            media_id: None, kind: None,
            timeline_in: 0, timeline_out: 24,
            source_in: 0, source_out: 24, rate: 24,
            node_graph: None, color_grade: None,
            effects: vec![], builtin_effects: None, builtin_effects_enabled: Vec::new(), magnetic_anchor: None,
            linked_clip_id: None, linked_title_asset_id: None, group_id: None,
            gain: None, volume: None, pan: None,
            fade_in_secs: None, fade_out_secs: None,
            fade_in_curve: None, fade_out_curve: None,
            channel_config: None,
            volume_keyframes: None, pan_keyframes: None, gain_keyframes: None,
            opacity_keyframes: None,
            transform_pos_x_keyframes: None, transform_pos_y_keyframes: None,
            transform_scale_keyframes: None, transform_rotation_keyframes: None, transform_tilt_x_keyframes: None, transform_tilt_y_keyframes: None,
            matte_feather_keyframes: None, matte_choke_keyframes: None,
            clip_markers: None, effect_keyframes: None,
            role: None, param_modifiers: None,
            speed: None, speed_keyframes: None,
            source_rotation: None, enabled: None,
            audio_effects: None,
            audio_effect_keyframes: std::collections::HashMap::new(),
            multicam_group: None, multicam_angle: None,
            multicam_cuts: vec![],
            solo: None,
            blend_mode: None,
            depth: None,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: None,
            transform_link: None,
            grade_versions: vec![],
            active_grade_version: None,
            track_points: vec![],
        }
    }

    /// Default values must NOT be serialised — keeps existing project files
    /// from gaining noisy `"blend_mode":null,"matte_source":null` lines on
    /// every save/load cycle.
    #[test]
    fn matte_and_blend_default_fields_are_omitted_from_json() {
        let clip = empty_clip_json();
        let s = serde_json::to_string(&clip).expect("serialise");
        assert!(!s.contains("blend_mode"), "default blend_mode must be skipped: {s}");
        assert!(!s.contains("matte_source"), "default matte_source must be skipped: {s}");
        assert!(!s.contains("consumed_as_matte"), "default consumed_as_matte must be skipped: {s}");
    }

    /// Old `.klipp` files (saved before track-matte landed) must still
    /// load — the new fields are all `Option` with `serde(default)`. This
    /// test pins that invariant: serde must accept JSON missing every
    /// new key.
    #[test]
    fn legacy_clip_without_new_fields_loads_with_defaults() {
        let legacy = r#"{
            "id": "11111111-1111-1111-1111-111111111111",
            "media_id": null,
            "timeline_in": 0, "timeline_out": 48,
            "source_in": 0, "source_out": 48, "rate": 24,
            "effects": [],
            "audio_effect_keyframes": {},
            "multicam_cuts": [],
            "grade_versions": [],
            "track_points": []
        }"#;
        let clip: ClipJson = serde_json::from_str(legacy)
            .expect("legacy clip JSON must parse without the new track-matte fields");
        assert!(clip.blend_mode.is_none(), "legacy clip should have no blend_mode (defaults to Normal)");
        assert!(clip.matte_source.is_none(), "legacy clip should have no matte binding");
        assert!(clip.consumed_as_matte.is_none(), "legacy clip should have no consumed_as_matte flag");
    }

    /// A populated binding survives the round trip with all five fields
    /// intact. Mirrors the regression bar that landed for similar keyframe
    /// fields earlier — silent loss of one field on save would be a near-
    /// invisible bug at edit time, painful at deliver time.
    #[test]
    fn matte_binding_round_trips_through_clip_json() {
        let mut clip = empty_clip_json();
        clip.blend_mode = Some(klipp_core::BlendMode::Multiply);
        clip.matte_source = Some(MatteBindingJson {
            source_clip_id: "22222222-2222-2222-2222-222222222222".to_string(),
            channel: klipp_nodes::params::MatteChannel::Alpha,
            invert: true,
            feather_px: 4.5,
            choke_px: 1.25,
        });
        clip.consumed_as_matte = Some(true);

        let s = serde_json::to_string(&clip).expect("serialise");
        let back: ClipJson = serde_json::from_str(&s).expect("deserialise");

        assert_eq!(back.blend_mode, Some(klipp_core::BlendMode::Multiply));
        assert_eq!(back.consumed_as_matte, Some(true));
        let m = back.matte_source.expect("matte_source preserved");
        assert_eq!(m.source_clip_id, "22222222-2222-2222-2222-222222222222");
        assert_eq!(m.channel, klipp_nodes::params::MatteChannel::Alpha);
        assert!(m.invert);
        assert!((m.feather_px - 4.5).abs() < f32::EPSILON);
        assert!((m.choke_px - 1.25).abs() < f32::EPSILON);
    }
}
