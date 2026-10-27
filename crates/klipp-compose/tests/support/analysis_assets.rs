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

/// Stabilisation data whose frame `i`'s camera is `cameras[i]`, in the
/// frame's units (centred, its height 1): what the frame is corrected
/// from, toward its smoothed path.
pub fn stabilised(cameras: &[Homography]) -> (AssetKind, serde_json::Value) {
    use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, StabilizationData};
    let n = cameras.len();
    let frame_transforms: Vec<Homography> = cameras.windows(2).map(|w| w[1].multiply(&w[0].inverse().expect("a camera that can be undone"))).collect();
    let accumulated = if n == 0 { Vec::new() } else { accumulate_trajectory(&frame_transforms).iter().map(|a| a.multiply(&cameras[0])).collect() };
    let data = StabilizationData { trajectory: accumulated.iter().map(decompose_homography).collect(), accumulated, frame_transforms, fps: 24.0, frame_count: n };
    (AssetKind::Stabilisation, serde_json::to_value(data).expect("serialises"))
}

/// [`stabilised`] by a camera shaking every frame, to (`tx`, `ty`) frame
/// heights and back over `frames`: each even frame is corrected by about
/// (`tx`, `ty`), each odd one by about the opposite.
pub fn stabilised_by(tx: f64, ty: f64, frames: usize) -> (AssetKind, serde_json::Value) {
    let at = |i: usize| if i.is_multiple_of(2) { 1.0 } else { -1.0 };
    stabilised(&(0..frames).map(|i| Homography { h: [[1.0, 0.0, at(i) * tx], [0.0, 1.0, at(i) * ty], [0.0, 0.0, 1.0]] }).collect::<Vec<_>>())
}

/// Deflicker data: each frame's light, alternating bright and dim, so its
/// gains change every frame.
fn flicker_data(frames: usize) -> klipp_effects::deflicker::DeflickerData {
    let luma = (0..frames).map(|i| if i % 2 == 0 { 0.35 } else { 0.55 } + 0.002 * i as f32).collect();
    klipp_effects::deflicker::DeflickerData { luma, fps: 24.0 }
}

/// [`flicker_data`] as an asset.
pub fn flickering(frames: usize) -> (AssetKind, serde_json::Value) {
    (AssetKind::Deflicker, serde_json::to_value(flicker_data(frames)).expect("serialises"))
}

/// The gain [`flickering`]'s analysis gives the frame shown at
/// `source_secs`, under a window of `window_secs`: worked out from the
/// light itself, not read back from a render.
pub fn flicker_gain(frames: usize, window_secs: f64, source_secs: f64) -> f32 {
    let gains = flicker_data(frames).gains(window_secs);
    gains[((source_secs * 24.0).round().max(0.0) as usize).min(gains.len() - 1)]
}

/// The motion between a frame and its neighbours [`sheared`] gives every
/// frame: the picture moving right and a little down.
fn shear_motion() -> (Homography, Homography) {
    let step = |dx: f64, dy: f64| Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, dy], [0.0, 0.0, 1.0]] };
    (step(-6.0, -1.0), step(6.0, 1.0))
}

/// Rolling-shutter motion: every frame [`shear_motion`], so the rows
/// shear.
pub fn sheared(frames: usize) -> (AssetKind, serde_json::Value) {
    let data = klipp_effects::rolling_shutter::RollingShutterData { motion: vec![shear_motion(); frames], fps: 24.0 };
    (AssetKind::RollingShutter, serde_json::to_value(data).expect("serialises"))
}

/// The rows [`sheared`]'s analysis gives a correction of `strength` and
/// `readout_ratio` on a frame `height` rows tall: worked out from the
/// motion itself, not read back from a render.
pub fn shear_rows(height: u32, readout_ratio: f32, strength: f32) -> Vec<klipp_effects::rolling_shutter::RowTransform> {
    let (prev, next) = shear_motion();
    klipp_effects::rolling_shutter::compute_row_transforms(&prev, &next, height, readout_ratio, strength)
}
