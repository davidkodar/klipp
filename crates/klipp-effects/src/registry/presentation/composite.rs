//! The composite family's rows (#1746): a merge's blend and opacity, a
//! generator's shape, colours and placing, a media source's item and
//! offset, and a title source's title (#2758). Made after the old
//! inspector was recorded, so the tables alone say how they show.

use super::{ParamEntry, Presentation as P, TypeEntry, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const MERGE: &str = "klipp.fx.merge";
const GENERATOR: &str = "klipp.fx.generator";
const MEDIA_SOURCE: &str = "klipp.fx.media_source";
const TITLE_SOURCE: &str = "klipp.fx.title_source";

/// A merge's blend modes in words, as saved by name.
pub(super) const BLEND_LABELS: &[&str] = &["Normal", "Add", "Multiply", "Screen", "Overlay", "Soft Light", "Hard Light", "Difference", "Exclusion", "Colour Dodge", "Colour Burn", "Darken", "Lighten"];

pub(super) static PARAMS: &[ParamEntry] = &[
    at(MERGE, "blend_mode", P::shown_as(Widget::Combo).labels(BLEND_LABELS)),
    at(MERGE, "opacity", P::scrub(0.005, 2)),
    at(GENERATOR, "shape", P::shown_as(Widget::Combo)),
    at(GENERATOR, "color_a", P::shown_as(Widget::ColourRgba)),
    at(GENERATOR, "color_b", P::shown_as(Widget::ColourRgba)),
    at(GENERATOR, "angle", P::scrub(0.5, 1)),
    at(GENERATOR, "center_x", P::scrub(0.005, 3)),
    at(GENERATOR, "center_y", P::scrub(0.005, 3)),
    at(GENERATOR, "radius", P::scrub(0.005, 3)),
    at(MEDIA_SOURCE, "media", P::shown_as(Widget::Custom("effect_media"))),
    at(MEDIA_SOURCE, "offset", P::scrub(0.05, 2)),
    // Made and changed by the title op, as a title is: not a row.
    at(TITLE_SOURCE, "title", P::hidden()),
];

pub(super) static TYPES: &[TypeEntry] = &[];
