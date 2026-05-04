//! Wire-format param keys for per-effect title-keyframe AppCommands.
//!
//! `AppCommand::SetTitleEffectKeyframe` and friends carry a
//! stringly-typed `param_key` so one command family covers every
//! keyframable param across every effect type. To keep typos out of
//! both the dispatch site and the call site, every valid key lives
//! here as a `pub const &str`. The handler dispatches via a `match`
//! against these constants; the inspector emits commands using the
//! same constants. Adding a new keyframable param means adding one
//! constant here and one match arm in the handler — the schema is
//! one place.

// ── TextGlow ──
pub const GLOW_RADIUS: &str = "glow.radius";
pub const GLOW_INTENSITY: &str = "glow.intensity";

// ── TitleShadow ──
pub const SHADOW_OFFSET_X: &str = "shadow.offset_x";
pub const SHADOW_OFFSET_Y: &str = "shadow.offset_y";
pub const SHADOW_BLUR: &str = "shadow.blur";

// ── InnerShadow ──
pub const INNER_SHADOW_OFFSET_X: &str = "inner_shadow.offset_x";
pub const INNER_SHADOW_OFFSET_Y: &str = "inner_shadow.offset_y";
pub const INNER_SHADOW_BLUR: &str = "inner_shadow.blur";

// ── TextExtrusion ──
pub const EXTRUSION_DEPTH: &str = "extrusion.depth";
pub const EXTRUSION_ANGLE: &str = "extrusion.angle";
pub const EXTRUSION_LIGHTING: &str = "extrusion.lighting";
