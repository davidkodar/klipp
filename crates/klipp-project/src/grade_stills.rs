//! Grade reference still persistence — sidecar PNGs in `media/stills/` plus
//! a JSON index at `media/stills.json`. The UI's runtime `GradeStill` type
//! lives in `klipp-ui::state`; this module deals only with the on-disk
//! representation so klipp-project stays free of egui dependencies.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Header for one saved still — what gets written into `stills.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeStillMeta {
    pub name: String,
    pub filename: String,
}

/// Everything needed to reconstruct a runtime `GradeStill` after load.
#[derive(Debug, Clone)]
pub struct GradeStillData {
    pub name: String,
    pub filename: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
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

fn stills_index_path(project_dir: &Path) -> PathBuf {
    project_dir.join("media").join("stills.json")
}

/// Encode and write all stills into `<project>/media/stills/` plus an
/// index file. Idempotent — safe to call repeatedly.
pub fn save_grade_stills(
    project_dir: &Path,
    stills: &[GradeStillData],
) -> Result<(), String> {
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
        })
        .collect();
    let index_json = serde_json::to_string_pretty(&metas)
        .map_err(|e| format!("Failed to serialize stills index: {e}"))?;
    atomic_write(&stills_index_path(project_dir), index_json.as_bytes())
        .map_err(|e| format!("Failed to write stills index: {e}"))?;

    info!("Saved {} grade still(s) to {:?}", stills.len(), stills_dir);
    Ok(())
}

/// Read the stills index and decode every PNG. Missing files are skipped
/// with a warning rather than failing the whole load.
pub fn load_grade_stills(project_dir: &Path) -> Vec<GradeStillData> {
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
    let mut metas: Vec<GradeStillMeta> = match serde_json::from_str(&index_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse stills index: {e}");
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
}
