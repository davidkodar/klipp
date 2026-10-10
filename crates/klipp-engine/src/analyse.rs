//! A media file's analysis, run whole (M4 step 9): what an export runs
//! for an analysis effect whose asset is missing, through the decoder and
//! the measures the viewer's analyses use, so both make the same asset.

use std::path::Path;

use klipp_analysis::assets::AssetKind;

/// The width an analysis decodes at: the asset key's.
const WIDTH: u32 = klipp_analysis::assets::ANALYSIS_WIDTH;

/// `kind`'s analysis of the file at `path` (`fps`, `frames` frames), as
/// its asset's data; `progress` (0..1) returns false to stop.
pub fn analyse_media(kind: AssetKind, path: &Path, fps: f64, frames: usize, progress: &dyn Fn(f32) -> bool) -> Result<serde_json::Value, String> {
    let mut decoder = crate::SequentialFrameDecoder::open(path, WIDTH)?;
    let mut failed = None;
    // Frames in file order: the analyses ask for them in that order.
    let mut next = |_t: f64| match decoder.next_frame() {
        Ok(frame) => frame,
        Err(e) => {
            failed = Some(e);
            None
        }
    };
    let data = match kind {
        AssetKind::Deflicker => klipp_effects::deflicker::analyze_clip(fps, frames, &mut next, progress).and_then(json),
        AssetKind::Stabilisation => klipp_effects::stabilization::analyze_clip(fps, frames, &mut next, progress).and_then(json),
        AssetKind::RollingShutter => klipp_effects::rolling_shutter::analyze_clip(fps, frames, &mut next, progress).and_then(json),
    };
    // A decode error says more than the analysis' "frame N missing".
    data.map_err(|e| failed.map_or(e, |d| format!("decode: {d}")))
}

fn json<T: serde::Serialize>(data: T) -> Result<serde_json::Value, String> {
    serde_json::to_value(data).map_err(|e| e.to_string())
}
