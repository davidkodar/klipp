//! Candle (pure-Rust) backend for klipp-ml: the models candle-transformers
//! has natively, with no FFI beyond what candle's tensor backends need —
//! translation (Opus-MT) and the sound tagger (PANNs).
//!
//! The picture models moved to ONNX Runtime (`klipp-ml-ort`, #2245),
//! where the provider ladder runs them on any vendor's GPU: CLIP and
//! MobileSAM.

pub mod translation;
pub mod panns;

