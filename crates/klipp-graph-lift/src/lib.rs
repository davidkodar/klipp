#![forbid(unsafe_code)]
//! Klipp's pictures in revision 2 and back (#997, #1089): the structural
//! form (`to_v2`, `to_v1`, `canon`), the semantic lift and its inverse
//! (`lift`, `delift`), the splice of group and track stages (`splice`),
//! node paths (`paths`) and the revision-2 conversion of every store
//! (`revision2`, with the old fields that keep old builds out). Pure
//! functions over the files' JSON and `klipp-graph-format`: the app may
//! depend on this crate, and it depends on no app crate.
pub mod canon;
pub mod delift;
pub mod ids;
pub mod lift;
pub mod old_fields;
pub mod paths;
pub mod revision;
pub mod revision2;
pub mod splice;
pub mod to_v1;
pub mod to_v2;

#[cfg(test)]
mod lift_tests;

#[cfg(test)]
mod to_v2_tests;
