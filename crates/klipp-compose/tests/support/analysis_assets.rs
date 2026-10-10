//! Analyses for the compose tests' clips (M4 step 9): an analysis effect
//! reads its media's asset, found under the key its frame source gives.
//! These tests key every media clip by one stand-in file (this test
//! binary, which exists) and keep the assets in memory.

#![allow(dead_code)]

use klipp_analysis::assets::{self, AssetKey, AssetKind, MemoryStore};
use klipp_effects::homography::Homography;
use klipp_timeline::{Clip, ClipKind};

/// The key `kind`'s analysis of a media clip is found under: the stand-in
/// file's, at 24 fps. A clip without media has none.
pub fn key(clip: &Clip, kind: AssetKind) -> Option<AssetKey> {
    matches!(clip.kind, ClipKind::Media { .. }).then_some(())?;
    assets::key_for(kind, &std::env::current_exe().ok()?, assets::ANALYSIS_WIDTH, 270, (24, 1))
}

/// A store holding `analyses`, each under its kind's key.
pub fn store(analyses: Vec<(AssetKind, serde_json::Value)>) -> Box<MemoryStore> {
    let file = std::env::current_exe().expect("the test binary");
    let mut s = MemoryStore::default();
    for (kind, data) in analyses {
        let key = assets::key_for(kind, &file, assets::ANALYSIS_WIDTH, 270, (24, 1)).expect("a key for the test binary");
        assets::store(&mut s, key, data).expect("kept");
    }
    Box::new(s)
}

/// Stabilisation data whose frame `i`'s correction is `corrections[i]`:
/// the trajectory to reach is still (identity throughout, which smoothing
/// keeps) and frame `i` sits at the inverse of its correction, so the
/// correction that brings it back is exactly the one given.
pub fn stabilised(corrections: &[Homography]) -> (AssetKind, serde_json::Value) {
    use klipp_effects::stabilization::{MotionParams, StabilizationData};
    let n = corrections.len();
    let data = StabilizationData {
        frame_transforms: vec![Homography::identity(); n.saturating_sub(1)],
        accumulated: corrections.iter().map(|c| c.inverse().expect("a correction that can be undone")).collect(),
        trajectory: vec![MotionParams { tx: 0.0, ty: 0.0, rotation: 0.0, scale: 1.0 }; n],
        fps: 24.0,
        frame_count: n,
    };
    (AssetKind::Stabilisation, serde_json::to_value(data).expect("serialises"))
}

/// [`stabilised`] by the shift (`tx`, `ty`) at every one of `frames`.
pub fn stabilised_by(tx: f64, ty: f64, frames: usize) -> (AssetKind, serde_json::Value) {
    stabilised(&vec![Homography { h: [[1.0, 0.0, tx], [0.0, 1.0, ty], [0.0, 0.0, 1.0]] }; frames])
}

/// Deflicker data: each frame's light, alternating bright and dim, so its
/// gains change every frame.
pub fn flickering(frames: usize) -> (AssetKind, serde_json::Value) {
    let luma = (0..frames).map(|i| if i % 2 == 0 { 0.35 } else { 0.55 } + 0.002 * i as f32).collect();
    let data = klipp_effects::deflicker::DeflickerData { luma, fps: 24.0 };
    (AssetKind::Deflicker, serde_json::to_value(data).expect("serialises"))
}

/// Rolling-shutter motion: every frame the picture moving right and a
/// little down between its neighbours, so the rows shear.
pub fn sheared(frames: usize) -> (AssetKind, serde_json::Value) {
    let step = |dx: f64, dy: f64| Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] };
    let data = klipp_effects::rolling_shutter::RollingShutterData { motion: vec![(step(-6.0, -1.0), step(6.0, 1.0)); frames], fps: 24.0 };
    (AssetKind::RollingShutter, serde_json::to_value(data).expect("serialises"))
}
