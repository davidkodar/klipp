//! ONNX Runtime backend for klipp-ml.
//!
//! This crate hosts the `MaskGenerator` / `ObjectDetector` impls
//! whose models ship as ONNX weights and run through the ORT
//! library (the canonical reference implementation). Coverage is
//! intentionally broad — it's the one backend that handles every
//! model klipp-ml-candle does NOT have native support for:
//!
//! * `BiRefNet_lite` — Remove Background
//! * `RF-DETR-Seg` — Detect Objects, and the framing analyser
//! * `YuNet` — face detection for the colour measurements
//! * `PP-OCR` — on-screen text in screen recordings
//!
//! ## Build matrix
//!
//! Without the `ort` feature: scaffolding only. `MaskGenerator::generate`
//! returns `MaskError::BackendInit("...rebuild with --features ai-ort")`
//! so callers see an actionable error instead of a silent failure.
//!
//! With `--features ort`: links ort 2.0.0-rc.12 via load-dynamic.
//! Caller sets `ORT_DYLIB_PATH` to point at a libonnxruntime.so
//! they've installed locally (release tarball at
//! github.com/microsoft/onnxruntime/releases). Two flavors:
//!
//! * **CPU build** — `onnxruntime-linux-x64-1.X.Y.tgz`. Default
//!   path; no GPU acceleration but no env deps either.
//! * **GPU build** — `onnxruntime-linux-x64-gpu-1.X.Y.tgz`. ~150 MB
//!   download. Includes the CUDA EP. Runtime needs CUDA + cuDNN
//!   libs on the user's `LD_LIBRARY_PATH` (CUDA 12.8+/cuDNN 9.19+
//!   verified with 1.24.4).
//!
//! With `--features cuda` (= `--features ai-ort-cuda` from
//! klipp-ui): registers `CUDAExecutionProvider` against the session
//! builder. Without a GPU build of libonnxruntime.so to back it,
//! the EP registration logs a warning and ort falls through to the
//! CPU EP — non-fatal, just no acceleration. Build itself doesn't
//! need nvcc (load-dynamic).
//!
//! AMD (ROCm via MIGraphX EP) and Intel (OpenVINO EP) need
//! Klipp-shipped libonnxruntime.so built from source against those
//! providers — pyke's CDN doesn't ship them. Tracked as
//! F-97-AMD-ROCM and F-97-INTEL-OPENVINO.
//!
//! ## Public surface
//!
//! [`birefnet_lite::BiRefNetLite`] is the only impl today. Future
//! models live in their own modules with their own builders; the
//! trait surface is the runtime layer, not the construction layer
//! (per kalosm-style architecture).

pub mod birefnet_lite;
pub mod clip;
pub mod kokoro;
pub mod mobile_sam;
pub mod postprocess;
pub mod ppocr;
pub mod preprocess;
pub mod provider;
pub mod rf_detr_seg;
pub mod yunet;
pub mod runtime_resolve;

pub use birefnet_lite::{BiRefNetLite, BiRefNetLiteBuilder, BIREFNET_LITE_MODEL_ID};
pub use mobile_sam::{MobileSam, MobileSamBuilder, MOBILE_SAM_DECODER_ID, MOBILE_SAM_DISPLAY_NAME, MOBILE_SAM_MODEL_ID};
pub use rf_detr_seg::{
    coco_label, coco_label_for, RfDetrSeg, RfDetrSegBuilder, COCO_91_LABELS, COCO_LABELS, RF_DETR_INPUT_SIZE,
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
pub use runtime_resolve::{
    detect_gpu_vendor, ensure_runtime_loadable, find_libonnxruntime, resolve_runtime_status, GpuVendor,
    RuntimeStatus, StatusSeverity,
};
