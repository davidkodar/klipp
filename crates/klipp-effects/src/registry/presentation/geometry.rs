//! The geometry family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). Today's labels are kept where
//! they differ from the catalog's.

use super::{ParamEntry, Presentation as P, Suffix, TypeEntry, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const CROP: &str = "klipp.fx.crop";
const TRANSFORM: &str = "klipp.fx.transform";
const ROUNDED: &str = "klipp.fx.rounded_corners";
const FILL_BARS: &str = "klipp.fx.fill_bars";
const MIRROR: &str = "klipp.fx.mirror";
const LENS: &str = "klipp.fx.lens_distortion";
const DISPLACE: &str = "klipp.fx.displace";
const LETTERBOX: &str = "klipp.fx.letterbox";
const PIN: &str = "klipp.fx.corner_pin";
const PICTURE_XF: &str = "klipp.fx.picture_transform";

pub(super) static PARAMS: &[ParamEntry] = &[
    at(CROP, "left", P::scrub(0.005, 3)),
    at(CROP, "right", P::scrub(0.005, 3)),
    at(CROP, "top", P::scrub(0.005, 3)),
    at(CROP, "bottom", P::scrub(0.005, 3)),
    at(CROP, "feather", P::scrub(0.5, 1)),
    at(TRANSFORM, "position_x", P::scrub(1.0, 0).label("Pos X")),
    at(TRANSFORM, "position_y", P::scrub(1.0, 0).label("Pos Y")),
    at(TRANSFORM, "scale_x", P::scrub(0.01, 2).label("Scale X").suffix(Suffix::Text("×"))),
    at(TRANSFORM, "scale_y", P::scrub(0.01, 2).label("Scale Y").suffix(Suffix::Text("×"))),
    at(TRANSFORM, "rotation", P::scrub(0.5, 1)),
    at(TRANSFORM, "anchor_x", P::scrub(0.005, 3).label("Anchor X")),
    at(TRANSFORM, "anchor_y", P::scrub(0.005, 3).label("Anchor Y")),
    // Today in the video tab (`inspector/video_tab.rs`), not the effects
    // tab: the effect card's own rows now (IC12).
    at(TRANSFORM, "rotation_x", P::scrub(0.5, 1)),
    at(TRANSFORM, "rotation_y", P::scrub(0.5, 1)),
    // Resets to the catalog's 0 ("unset", drawn as 1), as the card does,
    // not the row's 1 (IC3).
    at(TRANSFORM, "perspective", P::scrub(0.01, 2)),
    // A system value, kept as it is.
    at(TRANSFORM, "focal_ref", P::hidden()),
    at(ROUNDED, "radius", P::scrub(0.002, 3)),
    at(ROUNDED, "feather", P::scrub(0.001, 3)),
    at(FILL_BARS, "blur", P::scrub(0.001, 3)),
    at(FILL_BARS, "zoom", P::scrub(0.005, 2)),
    at(FILL_BARS, "darken", P::scrub(0.005, 2)),
    at(MIRROR, "horizontal", P::shown_as(Widget::Toggle)),
    at(MIRROR, "vertical", P::shown_as(Widget::Toggle)),
    at(LENS, "distortion", P::scrub(0.01, 2)),
    at(LENS, "scale", P::scrub(0.01, 2)),
    at(DISPLACE, "amount", P::scrub(0.5, 1)),
    at(DISPLACE, "size", P::scrub(1.0, 0)),
    at(DISPLACE, "speed", P::scrub(0.05, 2)),
    at(DISPLACE, "direction", P::shown_as(Widget::Combo).labels(&["Both", "Across", "Up and Down"])),
    at(DISPLACE, "seed", P::scrub(1.0, 0).soft_whole((0, 10_000))),
    at(DISPLACE, "time", P::hidden()),
    at(LETTERBOX, "aspect_ratio", P::scrub(0.01, 2).label("Aspect Ratio").suffix(Suffix::Text(":1"))),
    // A choice of two colours, not a 0-to-1 slider (IC5).
    at(LETTERBOX, "bar_color", P::shown_as(Widget::Combo).label("Bar Colour").labels(&["Black", "White"])),
    at(LETTERBOX, "opacity", P::scrub(0.005, 2)),
    at(PIN, "tl_x", P::scrub(0.002, 3).label("Top Left X")),
    at(PIN, "tl_y", P::scrub(0.002, 3).label("Top Left Y")),
    at(PIN, "tr_x", P::scrub(0.002, 3).label("Top Right X")),
    at(PIN, "tr_y", P::scrub(0.002, 3).label("Top Right Y")),
    at(PIN, "br_x", P::scrub(0.002, 3).label("Bottom Right X")),
    at(PIN, "br_y", P::scrub(0.002, 3).label("Bottom Right Y")),
    at(PIN, "bl_x", P::scrub(0.002, 3).label("Bottom Left X")),
    at(PIN, "bl_y", P::scrub(0.002, 3).label("Bottom Left Y")),
    at(PICTURE_XF, "position_x", P::scrub(1.0, 1)),
    at(PICTURE_XF, "position_y", P::scrub(1.0, 1)),
    at(PICTURE_XF, "scale_x", P::scrub(0.01, 2)),
    at(PICTURE_XF, "scale_y", P::scrub(0.01, 2)),
    at(PICTURE_XF, "rotation", P::scrub(0.5, 1)),
    at(PICTURE_XF, "anchor_x", P::scrub(0.005, 3)),
    at(PICTURE_XF, "anchor_y", P::scrub(0.005, 3)),
];

pub(super) static TYPES: &[TypeEntry] = &[TypeEntry {
    type_id: PIN,
    // The viewer's corner handles (M3 step 6).
    pres: super::TypePresentation { notice: None, extra_ui: Some("corner_pin") },
}];
