//! Grade reference still persistence — sidecar PNGs in `media/stills/` plus
//! a JSON index at `media/stills.json`. The UI's runtime `GradeStill` type
//! lives in `klipp-ui::state`; this module deals only with the on-disk
//! representation so klipp-project stays free of egui dependencies.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Where a still was grabbed: the clip, its media, and the moment on the
/// timeline. What the gallery shows on hover, and what says which shot a
/// still's grade came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StillSource {
    pub clip_id: Option<klipp_core::ClipId>,
    pub media_id: Option<klipp_core::MediaId>,
    /// The playhead when it was grabbed, in the sequence's seconds.
    pub record_secs: f64,
    /// The clip's media name, for display.
    pub clip_name: String,
}

/// Header for one saved still — what gets written into `stills.json`.
/// The source and grade are absent in projects saved before stills
/// carried them, and read as none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeStillMeta {
    pub name: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<StillSource>,
    /// The clip's grade when the still was grabbed: a still is a store
    /// of grades as well as a picture to match against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<klipp_nodes::NodeGraph>,
    /// That grade's node colour labels, by path inside it (#1866).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grade_labels: Vec<klipp_timeline::GradeNodeLabel>,
}

/// Everything needed to reconstruct a runtime `GradeStill` after load.
#[derive(Debug, Clone)]
pub struct GradeStillData {
    pub name: String,
    pub filename: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source: Option<StillSource>,
    pub grade: Option<klipp_nodes::NodeGraph>,
    /// That grade's node colour labels, by path inside it (#1866).
    pub grade_labels: Vec<klipp_timeline::GradeNodeLabel>,
}

use crate::path_util::atomic_write;

fn stills_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("media").join("stills")
}

/// Where freeze frames and extracted stills go while there is no project
/// folder to write them into — a project never saved, or one open
/// read-only, whose folder belongs to someone else.
pub fn temp_still_roots() -> [PathBuf; 2] {
    let tmp = std::env::temp_dir();
    [tmp.join("klipp_freeze"), tmp.join("klipp_extracts")]
}

/// Whether `file` is a still written under [`temp_still_roots`].
pub fn is_temp_still(file: &Path) -> bool {
    temp_still_roots().iter().any(|root| file.starts_with(root))
}

/// Copy a still from a temporary folder into `project_dir`'s stills, and
/// return where it now is. A file of that name already there is kept, not
/// overwritten: it is the same still from an earlier save.
pub fn adopt_still(project_dir: &Path, file: &Path) -> std::io::Result<PathBuf> {
    let dir = stills_dir(project_dir);
    std::fs::create_dir_all(&dir)?;
    let name = file.file_name().ok_or_else(|| std::io::Error::other("a still with no file name"))?;
    let dest = dir.join(name);
    if !dest.exists() {
        std::fs::copy(file, &dest)?;
    }
    Ok(dest)
}

/// What [`adopt_temp_stills`] did: how many paths now point into the
/// project, and the temporary stills it could not copy, each with its
/// error. Those paths are left as they were.
#[derive(Debug, Default)]
pub struct StillAdoption {
    pub adopted: usize,
    pub failed: Vec<(PathBuf, std::io::Error)>,
}

impl StillAdoption {
    /// Whether nothing was under the temporary folders to begin with.
    pub fn is_empty(&self) -> bool {
        self.adopted == 0 && self.failed.is_empty()
    }
}

/// Bring every still under [`temp_still_roots`] into `project_dir` and
/// point whatever holds it there: media items in `pool`, and freeze
/// frames, which are not pool items but `StillFrame` generator clips on a
/// track, in every one of `sequences` (nested sequences and
/// precompositions are sequences of their own in that slice). Nothing
/// under the temporary folders means nothing is copied or changed, so this
/// is cheap to run on every save. One file referenced twice is copied
/// once, and reported once when it cannot be.
pub fn adopt_temp_stills(
    project_dir: &Path,
    pool: &mut klipp_media::MediaPool,
    sequences: &mut [klipp_timeline::Sequence],
) -> StillAdoption {
    let mut report = StillAdoption::default();
    // Where each temporary file went, or `None` once its copy has failed
    // and been reported.
    let mut done: std::collections::HashMap<PathBuf, Option<PathBuf>> = Default::default();
    let mut adopt = |path: &mut PathBuf, report: &mut StillAdoption| {
        if !is_temp_still(path) {
            return;
        }
        let to = match done.get(path.as_path()) {
            Some(to) => to.clone(),
            None => {
                let to = match adopt_still(project_dir, path) {
                    Ok(to) => Some(to),
                    Err(e) => {
                        warn!("could not bring {} into the project: {e}", path.display());
                        report.failed.push((path.clone(), e));
                        None
                    }
                };
                done.insert(path.clone(), to.clone());
                to
            }
        };
        if let Some(to) = to {
            *path = to;
            report.adopted += 1;
        }
    };
    for item in pool.all_mut() {
        adopt(&mut item.path, &mut report);
    }
    for clip in sequences.iter_mut().flat_map(|s| s.all_clips_mut()) {
        if let klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::StillFrame { png_path, .. }) = &mut clip.kind {
            adopt(png_path, &mut report);
        }
    }
    report
}

fn stills_index_path(project_dir: &Path) -> PathBuf {
    project_dir.join("media").join("stills.json")
}

/// The stills index as this Klipp reads it (#1089): a revision-2 index,
/// brought back through `unlift`, and only one this build reads in full
/// (#993). An index from before revision 2 is refused, naming the
/// converter. The one test a load and a save both ask, so an index that
/// isn't loaded is never written over either.
fn read_index(text: &str, form: crate::FileForm) -> Result<Vec<GradeStillMeta>, String> {
    let v1_text = match form {
        crate::FileForm::Revision2 => {
            let v2: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
            if let Some(why) = crate::side_store_revision_refusal(&v2) {
                return Err(why);
            }
            serde_json::to_string(&klipp_graph_lift::unlift::stills(&v2)?).map_err(|e| e.to_string())?
        }
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => text.to_owned(),
    };
    if !klipp_nodes::json_check::reads_in_full(&v1_text) {
        return Err("they were saved by a newer Klipp (#993)".into());
    }
    serde_json::from_str(&v1_text).map_err(|e| format!("the index doesn't parse: {e}"))
}

/// Encode and write all stills into `<project>/media/stills/` plus an
/// index file. Idempotent — safe to call repeatedly.
pub fn save_grade_stills(project_dir: &Path, stills: &[GradeStillData]) -> Result<(), String> {
    save_grade_stills_as(project_dir, stills, crate::FileForm::Revision2)
}

/// Today's v1 stills writer, kept only to prove the one-time conversion
/// (#1089), behind `v1-oracle`.
#[cfg(feature = "v1-oracle")]
pub fn save_grade_stills_v1(project_dir: &Path, stills: &[GradeStillData]) -> Result<(), String> {
    save_grade_stills_as(project_dir, stills, crate::FileForm::V1)
}

fn save_grade_stills_as(
    project_dir: &Path,
    stills: &[GradeStillData],
    form: crate::FileForm,
) -> Result<(), String> {
    // An index this build cannot read in full was not loaded, so the
    // stills in memory are not all the project's stills: writing them
    // would delete the rest (#993). It is left as it is instead.
    let index_path = stills_index_path(project_dir);
    if index_path.exists() {
        let read = crate::path_util::read_to_string_capped(&index_path, crate::path_util::MAX_INPUT_FILE_BYTES)
            .map_err(|e| e.to_string())
            .and_then(|t| read_index(&t, form));
        if let Err(why) = read {
            return Err(format!("The grade stills in {} are left as they are and not overwritten: {why}", index_path.display()));
        }
    }
    let stills_dir = stills_dir(project_dir);
    std::fs::create_dir_all(&stills_dir)
        .map_err(|e| format!("Failed to create stills directory: {e}"))?;

    // Encode + write each PNG.
    for still in stills {
        let img = image::RgbaImage::from_raw(still.width, still.height, still.rgba.clone())
            .ok_or_else(|| {
                format!(
                    "Grade still '{}' has rgba length {} but expected {}",
                    still.filename,
                    still.rgba.len(),
                    still.width as usize * still.height as usize * 4
                )
            })?;
        let mut png_bytes: Vec<u8> = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode still '{}': {e}", still.filename))?;
        let png_path = stills_dir.join(&still.filename);
        atomic_write(&png_path, &png_bytes)
            .map_err(|e| format!("Failed to write still '{}': {e}", still.filename))?;
    }

    // Write the index.
    let metas: Vec<GradeStillMeta> = stills
        .iter()
        .map(|s| GradeStillMeta {
            name: s.name.clone(),
            filename: s.filename.clone(),
            source: s.source.clone(),
            grade: s.grade.clone(),
            grade_labels: s.grade_labels.clone(),
        })
        .collect();
    let index_json = match form {
        // Through text, as the project: an f32 keeps today's digits.
        crate::FileForm::Revision2 => {
            let text = serde_json::to_string(&metas).map_err(|e| format!("Failed to serialize stills index: {e}"))?;
            let v1: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Failed to serialize stills index: {e}"))?;
            let (v2, notes) = klipp_graph_lift::revision2::stills_noted(&v1).map_err(|e| format!("Failed to write the stills index's revision-2 file: {e}"))?;
            if let Some(n) = notes.iter().find(|n| klipp_graph_lift::revision2::is_lost(n)) {
                return Err(format!("Saving the grade stills would lose part of them ({n:?}), so they were not saved"));
            }
            serde_json::to_string_pretty(&v2).map_err(|e| format!("Failed to serialize stills index: {e}"))?
        }
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => serde_json::to_string_pretty(&metas).map_err(|e| format!("Failed to serialize stills index: {e}"))?,
    };
    atomic_write(&stills_index_path(project_dir), index_json.as_bytes())
        .map_err(|e| format!("Failed to write stills index: {e}"))?;

    info!("Saved {} grade still(s) to {:?}", stills.len(), stills_dir);
    Ok(())
}

/// Read the stills index and decode every PNG. Missing files are skipped
/// with a warning rather than failing the whole load.
pub fn load_grade_stills(project_dir: &Path) -> Vec<GradeStillData> {
    let mut stills = load_grade_stills_as(project_dir, crate::FileForm::Revision2);
    // A collected project's grade files, stored relative to it.
    for g in stills.iter_mut().filter_map(|s| s.grade.as_mut()) {
        crate::consolidate::resolve_collected_grade_files(g, project_dir);
    }
    stills
}

/// Today's v1 stills reader, kept only to prove the one-time conversion
/// (#1089), behind `v1-oracle`.
#[cfg(feature = "v1-oracle")]
pub fn load_grade_stills_v1(project_dir: &Path) -> Vec<GradeStillData> {
    load_grade_stills_as(project_dir, crate::FileForm::V1)
}

fn load_grade_stills_as(project_dir: &Path, form: crate::FileForm) -> Vec<GradeStillData> {
    let index_path = stills_index_path(project_dir);
    if !index_path.exists() {
        return Vec::new();
    }

    let index_str = match crate::path_util::read_to_string_capped(&index_path, crate::path_util::MAX_INPUT_FILE_BYTES) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to read stills index: {e}");
            return Vec::new();
        }
    };
    let mut metas: Vec<GradeStillMeta> = match read_index(&index_str, form) {
        Ok(v) => v,
        Err(why) => {
            warn!("Leaving the grade stills as they are: {why}");
            return Vec::new();
        }
    };

    // Cap entry count so a crafted stills.json claiming a billion entries
    // can't allocate a billion-sized Vec up front.
    const MAX_STILLS: usize = 10_000;
    if metas.len() > MAX_STILLS {
        warn!(
            "Stills index claims {} entries (cap is {MAX_STILLS}) — truncating",
            metas.len(),
        );
        metas.truncate(MAX_STILLS);
    }

    let stills_dir = stills_dir(project_dir);
    let mut out = Vec::with_capacity(metas.len());
    for meta in metas {
        // Reject filenames that try to escape the stills directory via
        // path components or absolute paths. stills.json is user-writable
        // via the project folder, so treat it as untrusted.
        if meta.filename.is_empty()
            || meta.filename.contains('/')
            || meta.filename.contains('\\')
            || meta.filename.contains("..")
            || Path::new(&meta.filename).is_absolute()
        {
            warn!(
                "Grade still filename rejected (path components or absolute): {:?}",
                meta.filename,
            );
            continue;
        }
        let png_path = stills_dir.join(&meta.filename);
        let img = match image::open(&png_path) {
            Ok(i) => i.to_rgba8(),
            Err(e) => {
                warn!("Failed to load grade still '{}': {e}", meta.filename);
                continue;
            }
        };
        let (width, height) = img.dimensions();
        out.push(GradeStillData {
            name: meta.name,
            filename: meta.filename,
            rgba: img.into_raw(),
            width,
            height,
            source: meta.source,
            grade: meta.grade,
            grade_labels: meta.grade_labels,
        });
    }
    info!("Loaded {} grade still(s) from {:?}", out.len(), stills_dir);
    out
}

/// Remove a single still PNG from disk (best-effort).
pub fn delete_grade_still_file(project_dir: &Path, filename: &str) {
    let png_path = stills_dir(project_dir).join(filename);
    if png_path.exists() {
        if let Err(e) = std::fs::remove_file(&png_path) {
            warn!("Failed to delete grade still {:?}: {e}", png_path);
        }
    }
}

/// Encode an RGBA8 buffer as PNG and write it atomically to
/// `<project_dir>/media/stills/<filename>`. Returns the absolute path on
/// success — the caller stores that path inside `GeneratorData::StillFrame`.
/// Shares the atomic-write pattern with `save_grade_stills` so freeze frames
/// and grade stills live side-by-side under the same directory.
pub fn save_freeze_frame_png(
    project_dir: &Path,
    filename: &str,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<PathBuf, String> {
    let dir = stills_dir(project_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create stills directory: {e}"))?;
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
        format!(
            "Freeze frame rgba length {} does not match {}x{}x4",
            rgba.len(),
            width,
            height
        )
    })?;
    let mut png_bytes: Vec<u8> = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode freeze frame: {e}"))?;
    let png_path = dir.join(filename);
    atomic_write(&png_path, &png_bytes)
        .map_err(|e| format!("Failed to write freeze frame: {e}"))?;
    info!("Saved freeze frame to {:?}", png_path);
    Ok(png_path)
}

#[cfg(test)]
mod adopt_tests {
    use super::*;

    #[test]
    fn a_temporary_still_is_copied_into_the_project_it_is_saved_with() {
        let root = &temp_still_roots()[0];
        let src_dir = root.join(format!("adopt-{}", std::process::id())).join("media").join("stills");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("freeze_1.png");
        std::fs::write(&src, b"png").unwrap();
        assert!(is_temp_still(&src));
        let project = std::env::temp_dir().join(format!("klipp-adopt-project-{}", std::process::id()));
        let dest = adopt_still(&project, &src).unwrap();
        assert_eq!(dest, project.join("media/stills/freeze_1.png"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"png");
        assert!(!is_temp_still(&dest));
        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(root.join(format!("adopt-{}", std::process::id()))).ok();
    }

    use klipp_core::{Fps, MediaId, Timecode};
    use klipp_media::{MediaItem, MediaItemKind};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    fn file_item(path: &Path) -> MediaItem {
        let mut m = MediaItem::new_synthetic_title(MediaId::new(), String::new(), Timecode::from_secs_f64(1.0, 24), Fps::film_24());
        m.kind = MediaItemKind::File;
        m.path = path.to_path_buf();
        m.filename = path.file_name().unwrap().to_string_lossy().into_owned();
        m
    }

    fn freeze_clip(png_path: &Path) -> Clip {
        let t0 = Timecode::from_secs_f64(0.0, 24);
        let t1 = Timecode::from_secs_f64(1.0, 24);
        Clip {
            kind: ClipKind::Generator(klipp_core::GeneratorData::StillFrame { png_path: png_path.to_path_buf(), width: 2, height: 2 }),
            ..Clip::gap(t0, t1)
        }
    }

    fn seq_with_clip(clip: Clip) -> Sequence {
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips.push(clip);
        let mut s = Sequence::new_default();
        s.tracks = vec![track];
        s
    }

    fn still_frame_path(seq: &Sequence) -> &Path {
        match &seq.tracks[0].clips[0].kind {
            ClipKind::Generator(klipp_core::GeneratorData::StillFrame { png_path, .. }) => png_path,
            other => panic!("not a freeze frame: {other:?}"),
        }
    }

    /// The #349 case: a freeze frame is a generator clip, not a pool item,
    /// and it sits in a nested sequence as easily as in the main one.
    #[test]
    fn a_pool_item_and_a_freeze_frame_clip_both_move_into_the_project() {
        let root = &temp_still_roots()[0];
        let src_dir = root.join(format!("adopt-all-{}", std::process::id()));
        std::fs::create_dir_all(&src_dir).unwrap();
        let extracted = src_dir.join("extract_1.png");
        let freeze = src_dir.join("freeze_2.png");
        let nested_freeze = src_dir.join("freeze_3.png");
        for f in [&extracted, &freeze, &nested_freeze] {
            std::fs::write(f, b"png").unwrap();
        }
        let project = std::env::temp_dir().join(format!("klipp-adopt-all-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);

        let mut pool = klipp_media::MediaPool::new();
        let item_id = pool.add(file_item(&extracted));
        let mut sequences = vec![seq_with_clip(freeze_clip(&freeze)), seq_with_clip(freeze_clip(&nested_freeze))];

        let report = adopt_temp_stills(&project, &mut pool, &mut sequences);
        assert_eq!(report.adopted, 3);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let stills = project.join("media").join("stills");
        let item_path = pool.get(&item_id).unwrap().path.clone();
        assert_eq!(item_path, stills.join("extract_1.png"));
        assert_eq!(still_frame_path(&sequences[0]), stills.join("freeze_2.png"));
        assert_eq!(still_frame_path(&sequences[1]), stills.join("freeze_3.png"));
        for p in [&item_path, still_frame_path(&sequences[0]), still_frame_path(&sequences[1])] {
            assert!(!is_temp_still(p));
            assert_eq!(std::fs::read(p).unwrap(), b"png", "{}", p.display());
        }

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&src_dir).ok();
    }

    #[test]
    fn stills_already_in_a_project_are_left_alone() {
        let elsewhere = std::env::temp_dir().join("klipp-not-a-temp-still").join("media").join("stills").join("freeze_9.png");
        let mut pool = klipp_media::MediaPool::new();
        let item_id = pool.add(file_item(&elsewhere));
        let mut sequences = vec![seq_with_clip(freeze_clip(&elsewhere))];
        let project = std::env::temp_dir().join(format!("klipp-adopt-none-project-{}", std::process::id()));
        let report = adopt_temp_stills(&project, &mut pool, &mut sequences);
        assert!(report.is_empty());
        assert_eq!(pool.get(&item_id).unwrap().path, elsewhere);
        assert_eq!(still_frame_path(&sequences[0]), elsewhere);
        assert!(!project.exists(), "an adoption with nothing to adopt made a folder");
    }

    #[test]
    fn a_still_that_cannot_be_copied_is_reported_once_and_keeps_its_path() {
        let root = &temp_still_roots()[1];
        let missing = root.join(format!("adopt-missing-{}", std::process::id())).join("gone.png");
        assert!(is_temp_still(&missing) && !missing.exists());
        let project = std::env::temp_dir().join(format!("klipp-adopt-missing-project-{}", std::process::id()));
        let mut pool = klipp_media::MediaPool::new();
        let item_id = pool.add(file_item(&missing));
        let mut sequences = vec![seq_with_clip(freeze_clip(&missing))];
        let report = adopt_temp_stills(&project, &mut pool, &mut sequences);
        assert_eq!(report.adopted, 0);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, missing);
        assert_eq!(pool.get(&item_id).unwrap().path, missing);
        assert_eq!(still_frame_path(&sequences[0]), missing);
        std::fs::remove_dir_all(&project).ok();
    }
}

#[cfg(test)]
mod carry_tests {
    use super::*;

    #[test]
    fn a_still_keeps_its_grade_and_source_through_save_and_load() {
        let dir = std::env::temp_dir().join(format!("klipp-still-grade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut grade = klipp_nodes::NodeGraph::default();
        let node = grade.ensure_corrector();
        // Its nodes' colour labels too (#1866), by the ids the grade keeps.
        let labels = vec![klipp_timeline::GradeNodeLabel { nodes: vec![node], colour: 2 }];
        let source = StillSource { clip_id: Some(klipp_core::ClipId::new()), media_id: None, record_secs: 12.5, clip_name: "A001C003.mov".into() };
        let still = GradeStillData {
            name: "Hero".into(),
            filename: "still_1.png".into(),
            rgba: vec![255; 4 * 4],
            width: 2,
            height: 2,
            source: Some(source.clone()),
            grade: Some(grade.clone()),
            grade_labels: labels.clone(),
        };
        save_grade_stills(&dir, &[still]).unwrap();
        let back = load_grade_stills(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].source.as_ref(), Some(&source));
        let ids = |g: &klipp_nodes::NodeGraph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(ids(back[0].grade.as_ref().unwrap()), ids(&grade));
        assert_eq!(back[0].grade_labels, labels);
    }

    #[test]
    fn a_stills_index_from_before_grades_still_loads() {
        let old = r#"[{"name":"Still 1","filename":"still_1.png"}]"#;
        let metas: Vec<GradeStillMeta> = serde_json::from_str(old).unwrap();
        assert!(metas[0].grade.is_none() && metas[0].source.is_none());
        assert!(metas[0].grade_labels.is_empty(), "and no node labels (#1866)");
    }
}
