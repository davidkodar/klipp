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

// ── MotionBlur ──
pub const MOTION_BLUR_SHUTTER_ANGLE: &str = "motion_blur.shutter_angle";

// ── CharAnimation ──
pub const CHAR_ANIM_STAGGER: &str = "char_anim.stagger";
pub const CHAR_ANIM_WAVE_AMPLITUDE: &str = "char_anim.wave_amplitude";
pub const CHAR_ANIM_WAVE_SPEED: &str = "char_anim.wave_speed";

// ── TextOutline ──
//
// Outlines stack on `TextBox.outlines` (Vec<TextOutline>) with each
// slot keyframable independently, so the wire-format param_key is
// `outline.{index}.width` where `index` is the position in the stack.
// Use `outline_width_key(idx)` to build the string and
// `parse_outline_param_key(key)` to extract `(index, "width")` on the
// dispatch side. The constant `OUTLINE_PREFIX` is what
// `effect_keyframes_mut_for` matches on.
pub const OUTLINE_PREFIX: &str = "outline.";

/// Build the param_key for `outlines[idx].width_keyframes`.
pub fn outline_width_key(idx: usize) -> String {
    format!("outline.{idx}.width")
}

/// Parse `outline.{index}.{field}` and return `(index, field)`. The
/// router uses this; the inspector uses `outline_width_key` to build
/// the same shape from the same source. Returns `None` if the prefix
/// is wrong or the index can't be parsed.
pub fn parse_outline_param_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix(OUTLINE_PREFIX)?;
    let (idx_str, field) = rest.split_once('.')?;
    let idx = idx_str.parse::<usize>().ok()?;
    Some((idx, field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_param_key_round_trips() {
        let k = outline_width_key(3);
        assert_eq!(k, "outline.3.width");
        let (idx, field) = parse_outline_param_key(&k).unwrap();
        assert_eq!(idx, 3);
        assert_eq!(field, "width");
    }

    #[test]
    fn outline_parser_rejects_malformed_keys() {
        // Wrong prefix
        assert!(parse_outline_param_key("glow.radius").is_none());
        // No index after prefix
        assert!(parse_outline_param_key("outline.width").is_none());
        // Non-numeric index
        assert!(parse_outline_param_key("outline.first.width").is_none());
        // Empty index ("outline..width") parses idx_str = "" which fails parse
        assert!(parse_outline_param_key("outline..width").is_none());
    }
}
