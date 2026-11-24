//! The models this crate runs, put in a [`klipp_ml::ModelRegistry`] by
//! id: BiRefNet Lite, the RF-DETR-Seg tiers and MobileSAM, each with its
//! weights from the manifest and the model cache. A caller asks the
//! registry for a model by id; which ONNX builder makes it, and where
//! its weights are, is this crate's.

use std::path::PathBuf;

use klipp_ml::{MaskCapabilities, MaskGenerator, ModelInfo, ModelRegistry};

use crate::{BiRefNetLiteBuilder, MobileSamBuilder, RfDetrSegBuilder};

/// Put every built-in model in `r` whose weights the manifest names. A
/// model it can't place (no manifest entry, no cache folder) is left out
/// with a warning, and the AI tools then don't list it.
pub fn register_built_in(r: &mut ModelRegistry) {
    if let Some(weights) = weights_of(crate::BIREFNET_LITE_MODEL_ID) {
        r.register(
            // Background matting: auto-detect only, so Remove Background
            // is the task that lists it.
            ModelInfo { id: crate::BIREFNET_LITE_MODEL_ID.into(), display_name: "BiRefNet Lite".into(), capabilities: MaskCapabilities::AUTO_DETECT },
            Box::new(move || BiRefNetLiteBuilder::new().weights(weights.clone()).build().map(|g| Box::new(g) as Box<dyn MaskGenerator>)),
        );
    }
    // Every RF-DETR-Seg tier is the same architecture and decode; only the
    // weights and the name it reports differ. Auto-detect only: each
    // detection a candidate with its box and class.
    for (id, display) in [(crate::RF_DETR_SEG_MODEL_ID, crate::RF_DETR_SEG_DISPLAY_NAME), (crate::RF_DETR_SEG_MEDIUM_MODEL_ID, crate::RF_DETR_SEG_MEDIUM_DISPLAY_NAME)] {
        if let Some(weights) = weights_of(id) {
            r.register(
                ModelInfo { id: id.into(), display_name: display.into(), capabilities: MaskCapabilities::AUTO_DETECT },
                Box::new(move || RfDetrSegBuilder::new().weights(weights.clone()).model_id(id, display).build().map(|g| Box::new(g) as Box<dyn MaskGenerator>)),
            );
        }
    }
    if let Some(weights) = weights_of(crate::MOBILE_SAM_MODEL_ID) {
        let decoder = klipp_ml::ModelCache::open().map(|c| c.path_for_id(crate::MOBILE_SAM_DECODER_ID)).unwrap_or_default();
        r.register(
            // SAM-family: clicks, boxes, scribbles, and no prompts for
            // every mask it finds.
            ModelInfo {
                id: crate::MOBILE_SAM_MODEL_ID.into(),
                display_name: "MobileSAM".into(),
                capabilities: MaskCapabilities::POINT_PROMPT | MaskCapabilities::BOX_PROMPT | MaskCapabilities::SCRIBBLE | MaskCapabilities::AUTO_DETECT,
            },
            Box::new(move || MobileSamBuilder::new().weights(weights.clone()).decoder(decoder.clone()).build().map(|g| Box::new(g) as Box<dyn MaskGenerator>)),
        );
    }
}

/// Where model `id`'s weights are in the cache, by its manifest entry;
/// `None`, said in the log, when the manifest or the cache can't say.
fn weights_of(id: &str) -> Option<PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| tracing::warn!("skipping {id}: the manifest didn't load: {e}")).ok()?;
    let Some(entry) = manifest.get(id) else {
        tracing::warn!("skipping {id}: no manifest entry");
        return None;
    };
    let cache = klipp_ml::ModelCache::open().map_err(|e| tracing::warn!("skipping {id}: the model cache didn't open: {e}")).ok()?;
    Some(cache.path_for(entry))
}
