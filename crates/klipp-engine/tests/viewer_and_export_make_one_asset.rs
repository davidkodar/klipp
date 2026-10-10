//! The viewer and an export make one asset (M4 step 9): the viewer's
//! analysis (`analyse_into`) and an export's (`pre_analysis::ensure`
//! through `analyse_media`) of one file, for each kind, store files equal
//! byte for byte, so which made it first never matters. And each kind's
//! asset, as its analysis makes it, derives for its effect: an asset read
//! back is never one its effect can't use.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use klipp_analysis::assets::{self, AssetKind, CacheStore};
use klipp_render::pre_analysis::{ensure, Analyser, Need};

const FPS: u32 = 25;
const FRAMES: usize = 40;

/// A short clip with detail to track and light that moves: a test pattern
/// panning, its brightness pumping.
fn clip() -> Option<PathBuf> {
    let path = klipp_testing::scratch::scratch_file("klipp-one-asset", "clip.mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=s=640x360:r={FPS}:d={},crop=600:340:'20*sin(t*3)':'10*cos(t*2)',eq=brightness='0.05*sin(t*20)'", FRAMES as f64 / FPS as f64))
        .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// The one asset file under `root`.
fn asset_file(root: &Path) -> Vec<u8> {
    let files: Vec<PathBuf> = walk(root).into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    assert_eq!(files.len(), 1, "one asset under {}: {files:?}", root.display());
    std::fs::read(&files[0]).unwrap()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().flat_map(|e| if e.path().is_dir() { walk(&e.path()) } else { vec![e.path()] }).collect()
}

#[test]
fn the_viewer_and_an_export_make_the_same_asset_and_it_derives() {
    let Some(path) = clip() else {
        eprintln!("skipping: ffmpeg could not write the clip");
        return;
    };
    let scratch = path.parent().unwrap().to_path_buf();
    let fps = FPS as f64;
    let frames = assets::frames_of(FRAMES as f64 / fps, fps);
    for kind in [AssetKind::Deflicker, AssetKind::Stabilisation, AssetKind::RollingShutter] {
        let key = assets::key_at(kind, &path, (600, 340), (FPS, 1)).expect("the file is there");
        // The viewer's.
        let viewer_root = scratch.join(format!("viewer-{}", kind.id()));
        let data = klipp_engine::analyse_into(&mut CacheStore::at(viewer_root.clone()), key.clone(), &path, fps, frames, &|_| true).unwrap_or_else(|e| panic!("{kind:?}: the viewer's analysis: {e}"));
        // An export's.
        let export_root = scratch.join(format!("export-{}", kind.id()));
        let need = Need { clip_name: "clip".into(), effect: "effect", kind, key: Some(key), path: path.clone(), fps, frames };
        let analyser: Analyser = Arc::new(klipp_engine::analyse_media);
        ensure(&[need], &mut CacheStore::at(export_root.clone()), Some(&analyser), &AtomicBool::new(false), &|_| {}).unwrap_or_else(|e| panic!("{kind:?}: the export's analysis: {e}"));
        assert!(asset_file(&viewer_root) == asset_file(&export_root), "{kind:?}: the viewer's and the export's assets differ");
        // The effect that reads it can: its shape is its algorithm's.
        let effect = klipp_effects::registry::effect_types()
            .map(|t| (t.sample)())
            .find(|e| klipp_compose::render::asset_kind(e) == Some(kind))
            .unwrap_or_else(|| panic!("an effect reads {kind:?}"));
        assert!(klipp_effects::node_analysis::derive(&effect, &data).is_some(), "{kind:?}: its asset doesn't derive for {effect:?}");
    }
}
