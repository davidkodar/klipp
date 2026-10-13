//! The composite family's rows (#1746): a merge's blend and opacity, and
//! a generator's shape, colours and placing. Made after the old inspector
//! was recorded, so the tables alone say how they show.

use super::{ParamEntry, Presentation as P, TypeEntry, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const MERGE: &str = "klipp.fx.merge";
const GENERATOR: &str = "klipp.fx.generator";

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
];

pub(super) static TYPES: &[TypeEntry] = &[];
