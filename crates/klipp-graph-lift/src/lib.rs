#![forbid(unsafe_code)]
//! Klipp's pictures in revision 2 and back (#997, #1089): the structural
//! form (`to_v2`, `to_v1`, `canon`), the semantic lift and its inverse
//! (`lift`, `delift`), the splice of group and track stages (`splice`),
//! node paths (`paths`), the graph edits over them (`ops`, checked
//! against the frozen old value types in `v1`) and the revision-2
//! conversion of every store (`revision2`, with the old fields that keep
//! old builds out). Pure
//! functions over the files' JSON and `klipp-graph-format`: the app may
//! depend on this crate, and it depends on no app crate.
pub mod canon;
pub mod delift;
pub mod edit_planner;
pub mod effect_params;
pub mod ids;
pub mod journal;
pub mod lift;
pub mod load_rewrites;
pub mod old_fields;
pub mod ops;
pub mod paths;
pub mod revision;
pub mod revision2;
pub mod revision3;
pub mod splice;
pub mod stack_view;
pub mod to_v1;
pub mod to_v2;
mod topology;
pub mod unlift;
pub mod v1;

#[cfg(test)]
mod lift_tests;

#[cfg(test)]
mod to_v2_tests;
