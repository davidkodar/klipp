//! An export's analyses, before its first frame (M4 step 9).
//!
//! A clip's Deflicker, Stabilisation or Rolling Shutter effect reads its
//! media's analysis, kept as the file's asset. The viewer starts a missing
//! one by itself; an export never writes a clip without it. Before
//! rendering, the export finds every analysis its range needs and runs
//! each missing or stale one, once a file, through the [`Analyser`] it was
//! given. One it can't make — no analyser, a media whose rate or size
//! isn't known, an analysis that fails — refuses the export, naming the
//! clip and the effect.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use klipp_analysis::assets::{self, AssetKey, AssetKind, AssetStore};
use klipp_timeline::ClipKind;

use crate::{media_analysis_key, RenderError, SequenceSnapshot};

/// Runs one analysis of a media file: what to measure, the file, its
/// frame rate and frame count, and a progress callback (0..1, false to
/// stop). The asset's data, or why it couldn't be made.
pub type Analyser = Arc<dyn Fn(AssetKind, &Path, f64, usize, &dyn Fn(f32) -> bool) -> Result<serde_json::Value, String> + Send + Sync>;

/// An analysis the export's range needs: the clip and effect that read
/// it, and the media to analyse.
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub clip_name: String,
    pub effect: &'static str,
    pub kind: AssetKind,
    /// `None` when the media's rate or size isn't known: no key, so no
    /// asset can be found or kept.
    pub key: Option<AssetKey>,
    pub path: PathBuf,
    pub fps: f64,
    pub frames: usize,
}

/// The effect's name, as the inspector shows it.
fn effect_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Deflicker => "Deflicker",
        AssetKind::Stabilisation => "Stabilisation",
        AssetKind::RollingShutter => "Rolling Shutter",
    }
}

/// Every analysis an enabled effect of a media clip in the export's range
/// reads, in timeline order.
pub fn needs(snapshot: &SequenceSnapshot) -> Vec<Need> {
    let fps = snapshot.fps.max(1.0);
    let (start, end) = match snapshot.export_range {
        Some((s, e)) => (s as f64 / fps, e as f64 / fps),
        None => (0.0, snapshot.duration_frames as f64 / fps),
    };
    let mut out = Vec::new();
    for clip in snapshot.video.sequence.tracks.iter().flat_map(|t| t.clips.iter()) {
        let ClipKind::Media { media_id } = &clip.kind else { continue };
        if clip.timeline_out.to_secs_f64() <= start || clip.timeline_in.to_secs_f64() >= end {
            continue;
        }
        let Some(file) = snapshot.video.media.get(media_id) else { continue };
        for effect in clip.enabled_effects() {
            let Some(kind) = klipp_compose::render::asset_kind(effect) else { continue };
            let media_fps = file.fps.filter(|(_, d)| *d > 0).map_or(fps, |(n, d)| n as f64 / d as f64);
            out.push(Need {
                // Its own name, else its media's, as the timeline shows it.
                clip_name: clip.name.clone().unwrap_or_else(|| file.path.file_name().map_or_else(|| file.path.display().to_string(), |n| n.to_string_lossy().into_owned())),
                effect: effect_name(kind),
                kind,
                key: media_analysis_key(file, kind),
                path: file.path.clone(),
                fps: media_fps,
                frames: file.duration_secs.map_or(0, |d| assets::frames_of(d, media_fps)),
            });
        }
    }
    out
}

/// Every need ready in `store`: each missing or stale analysis run once a
/// file through `analyse` and kept, `stage` told which. Refused, naming
/// the clip and the effect, where one can't be made.
pub fn ensure(needs: &[Need], store: &mut dyn AssetStore, analyse: Option<&Analyser>, cancel: &AtomicBool, stage: &dyn Fn(&str)) -> Result<(), RenderError> {
    let mut done: HashSet<AssetKey> = HashSet::new();
    for need in needs {
        let refuse = |why: String| RenderError::Analysis(format!("clip \u{201c}{}\u{201d}: {} {why}", need.clip_name, need.effect));
        let Some(key) = need.key.clone() else {
            return Err(refuse(format!("needs an analysis of {}, whose frame rate or size isn't known", need.path.display())));
        };
        if done.contains(&key) || store.stamp(&key).is_some() {
            continue;
        }
        let Some(analyse) = analyse else {
            return Err(refuse(format!("has no analysis of {} yet; analyse it, then export again", need.path.display())));
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        stage(&format!("Analysing {} for {}\u{2026}", need.path.file_name().map_or_else(|| need.path.display().to_string(), |n| n.to_string_lossy().into_owned()), need.effect));
        let data = analyse(need.kind, &need.path, need.fps, need.frames, &|_| !cancel.load(Ordering::Relaxed)).map_err(|e| {
            if cancel.load(Ordering::Relaxed) {
                RenderError::Cancelled
            } else {
                refuse(format!("couldn't analyse {}: {e}", need.path.display()))
            }
        })?;
        assets::store(store, key.clone(), data).map_err(|e| refuse(format!("couldn't keep its analysis: {e}")))?;
        done.insert(key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_analysis::assets::Found;
    use klipp_analysis::assets::MemoryStore;
    use std::sync::Mutex;

    fn need(name: &str, path: &str, key: bool) -> Need {
        Need {
            clip_name: name.into(),
            effect: "Stabilisation",
            kind: AssetKind::Stabilisation,
            key: key.then(|| assets::key_for(AssetKind::Stabilisation, Path::new(path), 480, 270, (25, 1))).flatten(),
            path: PathBuf::from(path),
            fps: 25.0,
            frames: 50,
        }
    }

    /// An analyser that counts its runs and answers `answer`.
    fn counting(runs: Arc<Mutex<Vec<PathBuf>>>, answer: Result<serde_json::Value, String>) -> Analyser {
        Arc::new(move |_, path, _, _, _| {
            runs.lock().unwrap().push(path.to_path_buf());
            answer.clone()
        })
    }

    fn quiet(_: &str) {}

    /// A missing analysis is run, once for a file two clips read, and
    /// kept; one already kept isn't run again.
    #[test]
    fn a_missing_analysis_is_run_once_a_file_and_kept() {
        let dir = std::env::temp_dir().join(format!("klipp-pre-analysis-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot.mov");
        std::fs::write(&file, b"frames").unwrap();
        let path = file.to_str().unwrap().to_owned();
        let needs = [need("A", &path, true), need("B", &path, true)];
        let runs = Arc::new(Mutex::new(Vec::new()));
        let analyse = counting(Arc::clone(&runs), Ok(serde_json::json!({"frames": 50})));
        let mut store = MemoryStore::default();
        ensure(&needs, &mut store, Some(&analyse), &AtomicBool::new(false), &quiet).unwrap();
        assert_eq!(runs.lock().unwrap().len(), 1, "once for the file both clips read");
        assert!(matches!(assets::lookup(&store, needs[0].key.as_ref().unwrap()), Found::Ready(_)), "kept");
        ensure(&needs, &mut store, Some(&analyse), &AtomicBool::new(false), &quiet).unwrap();
        assert_eq!(runs.lock().unwrap().len(), 1, "kept, so not run again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without an analyser, with media whose rate or size isn't known, or
    /// with an analysis that fails, the export refuses, naming the clip
    /// and the effect.
    #[test]
    fn an_analysis_that_cant_be_made_refuses_naming_the_clip_and_effect() {
        let dir = std::env::temp_dir().join(format!("klipp-pre-analysis-refuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot.mov");
        std::fs::write(&file, b"frames").unwrap();
        let path = file.to_str().unwrap().to_owned();
        let refused = |e: Result<(), RenderError>| match e {
            Err(RenderError::Analysis(why)) => why,
            other => panic!("not refused: {other:?}"),
        };
        let mut store = MemoryStore::default();
        let why = refused(ensure(&[need("Beach", &path, true)], &mut store, None, &AtomicBool::new(false), &quiet));
        assert!(why.contains("\u{201c}Beach\u{201d}") && why.contains("Stabilisation") && why.contains("no analysis"), "{why}");
        let why = refused(ensure(&[need("Beach", &path, false)], &mut store, None, &AtomicBool::new(false), &quiet));
        assert!(why.contains("isn't known"), "{why}");
        let failing = counting(Arc::default(), Err("the decoder stopped at frame 12".into()));
        let why = refused(ensure(&[need("Beach", &path, true)], &mut store, Some(&failing), &AtomicBool::new(false), &quiet));
        assert!(why.contains("\u{201c}Beach\u{201d}") && why.contains("frame 12"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
