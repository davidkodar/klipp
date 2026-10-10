//! A media file's analysis, run whole (M4 step 9): the one analyser the
//! viewer and an export both run for an analysis effect whose asset is
//! missing, so the asset is the same whichever made it.

use std::path::Path;

use klipp_analysis::assets::{self, AssetKey, AssetKind, AssetStore};

/// The width an analysis decodes at: the asset key's.
const WIDTH: u32 = klipp_analysis::assets::ANALYSIS_WIDTH;

/// `kind`'s analysis of the file at `path` (`fps`, `frames` frames, as
/// [`assets::frames_of`] counts them), as its asset's data; `progress`
/// (0..1) returns false to stop.
///
/// The file is decoded on a thread of its own and the analysis takes its
/// frames as they come, over a small bounded channel: the two overlap, so
/// a stabilisation's wall time is about the slower of the two, not their
/// sum.
pub fn analyse_media(kind: AssetKind, path: &Path, fps: f64, frames: usize, progress: &dyn Fn(f32) -> bool) -> Result<serde_json::Value, String> {
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, u32, u32)>(3);
    let path = path.to_path_buf();
    // Why the decode stopped, if on an error: the analysis sees only a
    // closed channel, and its "frame N missing" would say less.
    let decoder = std::thread::Builder::new()
        .name("klipp-analyse-decode".into())
        .spawn(move || -> Result<(), String> {
            let mut decoder = crate::SequentialFrameDecoder::open(&path, WIDTH)?;
            while let Some(frame) = decoder.next_frame()? {
                // Blocks while the analysis is behind; ends when it stops.
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
            Ok(())
        })
        .map_err(|e| format!("decode thread: {e}"))?;
    let mut next = |_t: f64| frame_rx.recv().ok();
    let data = match kind {
        AssetKind::Deflicker => klipp_effects::deflicker::analyze_clip(fps, frames, &mut next, progress).and_then(json),
        AssetKind::Stabilisation => klipp_effects::stabilization::analyze_clip(fps, frames, &mut next, progress).and_then(json),
        AssetKind::RollingShutter => klipp_effects::rolling_shutter::analyze_clip(fps, frames, &mut next, progress).and_then(json),
    };
    drop(frame_rx);
    let decoded = decoder.join().map_err(|_| "the decode thread panicked".to_string())?;
    data.map_err(|e| decoded.err().map_or(e, |d| format!("decode: {d}")))
}

/// [`analyse_media`], kept in `store` under `key` as the file's asset:
/// what the viewer runs when an effect's asset is missing. An export runs
/// the same analysis through `pre_analysis::ensure`, which keeps it the
/// same way, so the two assets are equal.
pub fn analyse_into(store: &mut dyn AssetStore, key: AssetKey, path: &Path, fps: f64, frames: usize, progress: &dyn Fn(f32) -> bool) -> Result<serde_json::Value, String> {
    let data = analyse_media(key.kind, path, fps, frames, progress)?;
    assets::store(store, key, data.clone())?;
    Ok(data)
}

fn json<T: serde::Serialize>(data: T) -> Result<serde_json::Value, String> {
    serde_json::to_value(data).map_err(|e| e.to_string())
}
