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
//
// Glows stack on `TextBox.glows` (Vec<TextGlow>). Indexed param keys
// are `glow.{idx}.{field}`; unindexed `glow.radius`/`glow.intensity`
// are legacy aliases routed to `glows[0]`.
pub const GLOW_RADIUS: &str = "glow.radius";
pub const GLOW_INTENSITY: &str = "glow.intensity";

pub fn glow_radius_key(idx: usize) -> String {
    format!("glow.{idx}.radius")
}
pub fn glow_intensity_key(idx: usize) -> String {
    format!("glow.{idx}.intensity")
}

/// Parse `glow.{index}.{field}` and return `(index, field)`. Returns
/// `None` for legacy unindexed `glow.radius` (those route to `glows[0]`
/// via the prefix branch).
pub fn parse_glow_param_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("glow.")?;
    let (idx_str, field) = rest.split_once('.')?;
    let idx = idx_str.parse::<usize>().ok()?;
    Some((idx, field))
}

// ── TitleShadow ──
//
// Drop shadows stack on `TextBox.shadows` (Vec<TitleShadow>) with each
// slot keyframable independently. The wire-format param_key is
// `shadow.{idx}.{field}` where field ∈ {offset_x, offset_y, blur}.
// The unindexed `shadow.offset_x` etc. constants are legacy aliases
// kept for projects saved before the stack landed; the runtime routes
// them to `shadows[0]` so old keyframes survive the migration.
pub const SHADOW_OFFSET_X: &str = "shadow.offset_x";
pub const SHADOW_OFFSET_Y: &str = "shadow.offset_y";
pub const SHADOW_BLUR: &str = "shadow.blur";

/// Build the param_key for `shadows[idx].offset_x_keyframes`.
pub fn shadow_offset_x_key(idx: usize) -> String {
    format!("shadow.{idx}.offset_x")
}
/// Build the param_key for `shadows[idx].offset_y_keyframes`.
pub fn shadow_offset_y_key(idx: usize) -> String {
    format!("shadow.{idx}.offset_y")
}
/// Build the param_key for `shadows[idx].blur_keyframes`.
pub fn shadow_blur_key(idx: usize) -> String {
    format!("shadow.{idx}.blur")
}

/// Parse `shadow.{index}.{field}` and return `(index, field)`. Used
/// by the keyframe router to resolve indexed shadow keys. Returns
/// `None` for legacy unindexed `shadow.offset_x` (those are routed
/// to `shadows[0]` by the prefix branch directly).
pub fn parse_shadow_param_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("shadow.")?;
    let (idx_str, field) = rest.split_once('.')?;
    let idx = idx_str.parse::<usize>().ok()?;
    Some((idx, field))
}

// ── InnerShadow ──
//
// Same indexed shape as drop shadows: `inner_shadow.{idx}.{field}`.
// Unindexed `inner_shadow.offset_x` etc. are legacy aliases routed
// to `inner_shadows[0]`.
pub const INNER_SHADOW_OFFSET_X: &str = "inner_shadow.offset_x";
pub const INNER_SHADOW_OFFSET_Y: &str = "inner_shadow.offset_y";
pub const INNER_SHADOW_BLUR: &str = "inner_shadow.blur";

pub fn inner_shadow_offset_x_key(idx: usize) -> String {
    format!("inner_shadow.{idx}.offset_x")
}
pub fn inner_shadow_offset_y_key(idx: usize) -> String {
    format!("inner_shadow.{idx}.offset_y")
}
pub fn inner_shadow_blur_key(idx: usize) -> String {
    format!("inner_shadow.{idx}.blur")
}

/// Parse `inner_shadow.{index}.{field}` and return `(index, field)`.
/// Returns `None` for legacy unindexed `inner_shadow.offset_x` (those
/// hit the prefix branch directly and route to `inner_shadows[0]`).
pub fn parse_inner_shadow_param_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("inner_shadow.")?;
    let (idx_str, field) = rest.split_once('.')?;
    let idx = idx_str.parse::<usize>().ok()?;
    Some((idx, field))
}

// ── TextExtrusion ──
pub const EXTRUSION_DEPTH: &str = "extrusion.depth";
pub const EXTRUSION_ANGLE: &str = "extrusion.angle";
pub const EXTRUSION_LIGHTING: &str = "extrusion.lighting";

// ── A layer's own (#2301) ──
//
// `TitleObject.opacity_keyframes`: the layer's opacity, 0..1.
pub const LAYER_OPACITY: &str = "layer.opacity";

// ── The text's own type (#1511) ──
//
// `TextBox.{font_size,tracking,line_spacing}_keyframes`.
pub const TEXT_SIZE: &str = "text.size";
pub const TEXT_TRACKING: &str = "text.tracking";
pub const TEXT_LINE_SPACING: &str = "text.line_spacing";
/// A text box's paragraph settings (#1721), in pixels at 1080p:
/// `TextBox.paragraph.*_keyframes`.
pub const TEXT_SPACE_BEFORE: &str = "text.space_before";
pub const TEXT_SPACE_AFTER: &str = "text.space_after";
pub const TEXT_INDENT_START: &str = "text.indent_start";
pub const TEXT_INDENT_END: &str = "text.indent_end";
pub const TEXT_INDENT_FIRST: &str = "text.indent_first";
/// Where a text on a drawn path sits along it, 0..1 (#1508):
/// `TextPath::Layer.offset_keyframes`.
pub const TEXT_PATH_OFFSET: &str = "text.path_offset";

// A text gradient's stop colours, a stop at a time (#1511): stop `i`
// of the gradient's stops in position order, keyed whole as
// `gradient.stop.{i}` and channel by channel as `gradient.stop.{i}.{r,g,b,a}`.
pub fn gradient_stop_key(idx: usize) -> String {
    format!("gradient.stop.{idx}")
}

/// Parse `gradient.stop.{index}` or `gradient.stop.{index}.{channel}`
/// into `(index, channel)`, the channel empty for the whole colour.
pub fn parse_gradient_stop_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("gradient.stop.")?;
    let (idx, channel) = rest.split_once('.').unwrap_or((rest, ""));
    Some((idx.parse::<usize>().ok()?, channel))
}

// `TextBox.color_keyframes`: a channel each, in linear light 0..1.
pub const TEXT_COLOR: &str = "text.color";
pub const TEXT_COLOR_R: &str = "text.color.r";
pub const TEXT_COLOR_G: &str = "text.color.g";
pub const TEXT_COLOR_B: &str = "text.color.b";
pub const TEXT_COLOR_A: &str = "text.color.a";

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

// ── Animators (#1510) ──
//
// A layer's animators stack on `LayerFx.animators`; each selector's
// range is keyable as `animator.{idx}.{field}`, field ∈ {start, end,
// offset}.
pub fn animator_key(idx: usize, field: &str) -> String {
    format!("animator.{idx}.{field}")
}

/// Parse `animator.{index}.{field}` into `(index, field)`.
pub fn parse_animator_param_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("animator.")?;
    let (idx_str, field) = rest.split_once('.')?;
    Some((idx_str.parse::<usize>().ok()?, field))
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
