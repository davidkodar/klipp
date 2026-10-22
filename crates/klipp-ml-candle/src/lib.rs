//! Candle (pure-Rust) backend for klipp-ml.
//!
//! candle-transformers ships SAM v1 + MobileSAM (TinyViT) as
//! built-in models — these are the highest-leverage AI features
//! we can deliver without FFI:
//!
//! * `MobileSAM` — click-to-mask via TinyViT image encoder
//!   (~40 MB) + SAM prompt encoder + mask decoder. The marquee
//!   "Isolate Object" feature.
//! * `Sam v1 ViT-B` — heavier (~360 MB) but more accurate. Same
//!   trait surface; users pick via the model picker.
//!
//! ## Why a separate backend
//!
//! `klipp-ml-ort` (the ort/ONNX backend) covers BiRefNet / RF-DETR /
//! future SAM2 video, but pulls in a C library. `klipp-ml-candle`
//! covers what candle-transformers has natively, with **zero FFI**
//! beyond what candle's tensor backends already need (BLAS / CUDA
//! when those features are on). For users who want a pure-Rust AI
//! stack, `--features ai-candle` gives them SAM-family without
//! requiring `libonnxruntime.so`.
//!
//! ## Build matrix
//!
//! The SAM pipeline sits behind the `candle` feature flag. Without
//! it the crate still compiles, and [`klipp_ml::MaskGenerator::generate`]
//! on [`MobileSam`] returns [`klipp_ml::MaskError::BackendInit`] telling
//! the caller to rebuild with the flag, so the error is actionable
//! rather than a missing symbol.
//!
//! ## Public surface
//!
//! [`mobile_sam::MobileSam`] is the only impl today. SAM v1 ViT-B
//! would live in its own module behind the same trait; what stands in
//! the way is distributing its weights, not the code here.

pub mod mobile_sam;
pub mod translation;

pub use mobile_sam::{MobileSam, MobileSamBuilder, MOBILE_SAM_MODEL_ID};
