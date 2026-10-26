//! Phase-0 test harness for the CRIT-7 linear-light colour pipeline migration.
//!
//! This crate exists to hold the ground-truth fixtures, transfer
//! functions, and image-diff helpers that every subsequent phase of
//! the migration will be validated against. Nothing in here is called
//! from production code — downstream crates pull it in only through
//! `[dev-dependencies]`.
//!
//! # What the harness contains
//!
//! * [`transfer`] — reference implementations of `srgb_to_linear` and
//!   `linear_to_srgb`. These are *the* source of truth the migration
//!   will be tested against. The production code already has a
//!   private `linear_to_srgb` in `klipp-color/src/transforms.rs:725`
//!   — Phase 1 promotes that one and adds the inverse. The copies
//!   here stay independent so a bug in the production impl can't
//!   hide behind "test used the same function."
//! * [`fixtures`] — synthetic image generators: 18 % mid-grey, a
//!   linear black-to-white ramp, the 24-patch Macbeth ColorChecker
//!   chart, etc. All generated programmatically — no binary blobs
//!   checked into the repo.
//! * [`diff`] — pixel-comparison helpers: exact f32 equality for
//!   point operations, PSNR for spatial ops (blur / bloom / denoise),
//!   plus an `assert_*`-style API so test failures print actionable
//!   diagnostics.

pub mod diff;
pub mod fixtures;
pub mod scratch;
pub mod snapshot;
pub mod sweep;
pub mod transfer;
