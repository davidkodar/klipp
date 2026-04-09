//! Project validation and pre-flight checks for Klipp.
//!
//! This crate houses pure validation logic that can be used by both the UI
//! (for inline warnings) and the export pipeline (for pre-render checks).
//! It depends only on data-model crates (klipp-core, klipp-timeline,
//! klipp-media) — no engine, no render, no UI.
//!
//! Current validators:
//!   - `handles` — transition handle-frame availability detection

pub mod handles;
