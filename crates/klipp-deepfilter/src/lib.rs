//! DeepFilterNet3 speech enhancement: the inference path of DeepFilterNet's
//! `libDF` (v0.5.6), vendored into Klipp.
//!
//! Upstream: <https://github.com/Rikorose/DeepFilterNet>, © 2021 Hendrik
//! Schröter, licensed MIT OR Apache-2.0 (see `LICENSE-MIT` and
//! `LICENSE-APACHE` in this crate). This crate keeps that licence.
//!
//! Changes from upstream, for Klipp:
//! - only the tract inference path (`dsp`: STFT, ERB features and
//!   normalisation; `tract`: the model); training, datasets, the C API,
//!   file I/O and the bundled default model are left out;
//! - no `unsafe`: slice reinterpretation goes through bytemuck, buffers
//!   start zeroed, and the encoder inputs are owned tensors rather than
//!   values written through raw pointers;
//! - logging goes through `tracing`.

#![forbid(unsafe_code)]

pub mod dsp;
pub mod tract;
