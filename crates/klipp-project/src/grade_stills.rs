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

/// Write data to a file atomically (mirrors save.rs's helper).
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp_path = path.with_extension("tmp");
    std::fs::write(&temp_path, data)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

fn stills_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("media").join("stills")
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

    let index_str = match std::fs::read_to_string(&index_path) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to read stills index: {e}");
            return Vec::new();
        }
    };
    let metas: Vec<GradeStillMeta> = match serde_json::from_str(&index_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse stills index: {e}");
            return Vec::new();
        }
    };

    let stills_dir = stills_dir(project_dir);
    let mut out = Vec::with_capacity(metas.len());
    for meta in metas {
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
