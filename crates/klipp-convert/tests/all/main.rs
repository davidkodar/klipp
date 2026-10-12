//! Most of this crate's integration tests, built as one binary: one
//! link instead of one per file. Each file stays where it was and is a
//! module here. Files that time themselves or change process-wide state
//! stay their own binaries (the `[[test]]` list in Cargo.toml).

#[path = "../effect_list_saved_bytes.rs"]
mod effect_list_saved_bytes;
#[path = "../lift_load.rs"]
mod lift_load;
#[path = "../ops.rs"]
mod ops;
#[path = "../oracle.rs"]
mod oracle;
#[path = "../parity.rs"]
mod parity;
#[path = "../paths.rs"]
mod paths;
#[path = "../render_inputs.rs"]
mod render_inputs;
#[path = "../revision2.rs"]
mod revision2;
#[path = "../run.rs"]
mod run;
#[path = "../scan.rs"]
mod scan;
#[path = "../splice.rs"]
mod splice;
#[path = "../step4_ids.rs"]
mod step4_ids;
#[path = "../step_run.rs"]
mod step_run;
#[path = "../unlift.rs"]
mod unlift;
