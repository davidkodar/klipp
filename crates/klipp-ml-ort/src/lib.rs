//! ONNX Runtime backend for klipp-ml.
//!
//! This crate hosts the `MaskGenerator` / `ObjectDetector` impls
//! whose models ship as ONNX weights and run through the ORT
//! library (the canonical reference implementation). Coverage is
//! intentionally broad — it's the one backend that handles every
//! model klipp-ml-candle does NOT have native support for:
//!
//! * `BiRefNet_lite` — Remove Background (this crate's first model)
//! * `U2Net_p` — fast Remove Background tier
//! * `RF-DETR-Seg` — Detect Objects (Apache-2.0 alternative to YOLO)
//! * `SAM2 video` — temporal mask propagation (Phase 4, deferred)
//!
//! ## Build matrix
//!
//! Today the crate ships scaffolding with no `ort` dependency — the
//! actual inference path lands behind the `ort` feature flag in a
//! follow-up commit, after the prebuilt-binary distribution story is
//! worked out (pyke `download-binaries` for dev, `load-dynamic` for
//! shipped releases). With no flag the crate compiles and registers
//! its models, but `MaskGenerator::generate` returns
//! `MaskError::BackendInit` so callers see an actionable error
//! instead of a silent failure.
//!
//! ## Public surface
//!
//! [`birefnet_lite::BiRefNetLite`] is the only impl today. Future
//! models live in their own modules with their own builders; the
//! trait surface is the runtime layer, not the construction layer
//! (per kalosm-style architecture).

pub mod birefnet_lite;
pub mod postprocess;
pub mod preprocess;
pub mod rf_detr_seg;

pub use birefnet_lite::{BiRefNetLite, BiRefNetLiteBuilder, BIREFNET_LITE_MODEL_ID};
pub use rf_detr_seg::{
    coco_label, RfDetrSeg, RfDetrSegBuilder, COCO_LABELS, RF_DETR_INPUT_SIZE,
    RF_DETR_PROTOTYPE_COUNT, RF_DETR_SEG_DISPLAY_NAME, RF_DETR_SEG_MEDIUM_DISPLAY_NAME,
    RF_DETR_SEG_MEDIUM_MODEL_ID, RF_DETR_SEG_MODEL_ID,
};
pub use postprocess::{
    bilinear_resize_single_channel, postprocess, sigmoid,
};
pub use preprocess::{
    bilinear_resize_rgba, preprocess, preprocess_to_size, PreprocessedTensor,
    BIREFNET_INPUT_SIZE, IMAGENET_MEAN, IMAGENET_STD,
};
