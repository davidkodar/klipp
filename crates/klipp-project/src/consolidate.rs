//! Project consolidation / archive — copy all media into a self-contained
//! project folder so the project can be moved, backed up, or handed off
//! without the scattered source files.
//!
//! Operation: **copy-to-new-location**. The source project and its media
//! are never touched. On any failure the destination directory is removed
//! entirely (rollback), so a partially-copied project never survives.
//!
//! Scope (MVP):
//! - Copies `MediaItem::path` for every online media item into
//!   `dest_dir/media/<name>`, with counter-based filename collision
//!   handling (`Clip.mov`, `Clip_001.mov`, `Clip_002.mov`, ...).
//! - Optionally copies `proxy_path` into `dest_dir/media/proxies/` and
//!   `skim_file` into `dest_dir/media/skims/` (both opt-in).
//! - Copies `ColorSettings::global_lut_path` into `dest_dir/media/luts/`.
//! - Copies every `TitleData::texture_fill_path` into
//!   `dest_dir/media/textures/` (deduplicated by source path).
//! - Writes the project via the existing
//!   `save_project_with_color_and_backups` flow — path relativisation is
//!   handled by the existing `to_project_relative` plumbing.
//!
//! Offline media items are skipped and returned in
//! `ConsolidateResult::offline_skipped` so the UI can surface them.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use klipp_core::MediaId;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{ClipKind, Sequence};

use crate::save::{save_project_with_color_and_backups, ColorSettings};

/// Options for a consolidation run.
#[derive(Debug, Clone)]
pub struct ConsolidateOptions {
    /// Destination directory. Must not already exist.
    pub dest_dir: PathBuf,
    /// Copy proxy files (off by default — proxies are regeneratable).
    pub include_proxies: bool,
    /// Copy skim (all-keyframe) transcodes (off by default).
    pub include_skims: bool,
}

/// Progress callback signature: `(files_done, files_total, current_filename)`.
pub type ProgressFn = Box<dyn Fn(usize, usize, &str) + Send>;

/// Result summary for a consolidation run.
#[derive(Debug)]
pub struct ConsolidateResult {
    pub files_copied: usize,
    pub offline_skipped: Vec<String>,
    pub warnings: Vec<String>,
}

/// Errors that can abort consolidation before completion.
#[derive(Debug, thiserror::Error)]
pub enum ConsolidateError {
    #[error("Destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("Source project directory not found: {0}")]
    SourceNotFound(PathBuf),
    #[error("Failed to create destination directory: {0}")]
    CreateDir(std::io::Error),
    #[error("Failed to copy {src} → {dst}: {err}")]
    CopyFailed {
        src: PathBuf,
        dst: PathBuf,
        err: std::io::Error,
    },
    #[error("Project save failed after copying: {0}")]
    SaveFailed(String),
}

/// One planned file copy. Built up-front before any disk writes so the copy
/// loop itself is purely I/O.
#[derive(Debug, Clone)]
struct CopyItem {
    src: PathBuf,
    /// Destination filename (within its destination subfolder).
    dst_name: String,
    /// Destination subfolder under `dest_dir/media/`. Empty string = directly
    /// under `media/`. Other values: `"proxies"`, `"skims"`, `"luts"`, `"textures"`.
    subfolder: &'static str,
}

impl CopyItem {
    fn dst_path(&self, dest_dir: &Path) -> PathBuf {
        if self.subfolder.is_empty() {
            dest_dir.join("media").join(&self.dst_name)
        } else {
            dest_dir.join("media").join(self.subfolder).join(&self.dst_name)
        }
    }
}

/// Result of the planning phase — the final paths each item should use in
/// the consolidated project. Needed so the remapped MediaPool / sequences
/// can point at the right destination before `save_project` is called.
struct CopyPlan {
    items: Vec<CopyItem>,
    /// media_id → new absolute path under dest_dir.
    media_new_paths: HashMap<MediaId, PathBuf>,
    /// media_id → new absolute proxy path.
    proxy_new_paths: HashMap<MediaId, PathBuf>,
    /// media_id → new absolute skim path.
    skim_new_paths: HashMap<MediaId, PathBuf>,
    /// Texture source path (as stored on TitleData) → new absolute path.
    texture_new_paths: HashMap<String, PathBuf>,
    /// New LUT path, if the original was present and exists on disk.
    lut_new_path: Option<PathBuf>,
    offline_skipped: Vec<String>,
    warnings: Vec<String>,
}

/// Build the copy plan from the media pool, sequences, and color settings.
/// Purely in-memory — no filesystem side effects. Returns the plan plus any
/// upfront warnings (missing LUT, missing texture files).
fn build_copy_plan(
    media_pool: &MediaPool,
    sequences: &[Sequence],
    color_settings: &ColorSettings,
    options: &ConsolidateOptions,
) -> CopyPlan {
    let mut items: Vec<CopyItem> = Vec::new();
    let mut used_primary: HashSet<String> = HashSet::new();
    let mut used_proxy: HashSet<String> = HashSet::new();
    let mut used_skim: HashSet<String> = HashSet::new();
    let mut used_lut: HashSet<String> = HashSet::new();
    let mut used_texture: HashSet<String> = HashSet::new();

    let mut media_new_paths = HashMap::new();
    let mut proxy_new_paths = HashMap::new();
    let mut skim_new_paths = HashMap::new();
    let mut texture_new_paths = HashMap::new();
    let mut offline_skipped: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Primary media
    for item in media_pool.all() {
        if item.offline || !item.path.exists() {
            offline_skipped.push(item.path.display().to_string());
            continue;
        }
        let filename = item
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("media_{}", item.id.0));
        let dst_name = collision_safe_name(&filename, &mut used_primary);
        items.push(CopyItem {
            src: item.path.clone(),
            dst_name: dst_name.clone(),
            subfolder: "",
        });
        media_new_paths.insert(item.id, options.dest_dir.join("media").join(&dst_name));

        // Proxy (opt-in)
        if options.include_proxies {
            if let Some(proxy) = &item.proxy_path {
                if item.proxy_ready && proxy.exists() {
                    let pname = proxy
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("proxy_{}", item.id.0));
                    let dst = collision_safe_name(&pname, &mut used_proxy);
                    items.push(CopyItem {
                        src: proxy.clone(),
                        dst_name: dst.clone(),
                        subfolder: "proxies",
                    });
                    proxy_new_paths.insert(
                        item.id,
                        options.dest_dir.join("media").join("proxies").join(&dst),
                    );
                }
            }
        }

        // Skim (opt-in)
        if options.include_skims {
            if let Some(skim) = &item.skim_file {
                if skim.exists() {
                    let sname = skim
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("skim_{}", item.id.0));
                    let dst = collision_safe_name(&sname, &mut used_skim);
                    items.push(CopyItem {
                        src: skim.clone(),
                        dst_name: dst.clone(),
                        subfolder: "skims",
                    });
                    skim_new_paths.insert(
                        item.id,
                        options.dest_dir.join("media").join("skims").join(&dst),
                    );
                }
            }
        }
    }

    // Global LUT
    let lut_new_path = match &color_settings.global_lut_path {
        Some(lut) if lut.exists() => {
            let lname = lut
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project.cube".to_string());
            let dst = collision_safe_name(&lname, &mut used_lut);
            items.push(CopyItem {
                src: lut.clone(),
                dst_name: dst.clone(),
                subfolder: "luts",
            });
            Some(options.dest_dir.join("media").join("luts").join(&dst))
        }
        Some(lut) => {
            warnings.push(format!(
                "Global LUT not found on disk, skipped: {}",
                lut.display()
            ));
            None
        }
        None => None,
    };

    // Title texture fills — dedupe by source path so the same texture used
    // by many title clips is only copied once.
    for seq in sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                if let ClipKind::Title(td) = &clip.kind {
                    if let Some(tex) = &td.texture_fill_path {
                        if texture_new_paths.contains_key(tex) {
                            continue;
                        }
                        let p = Path::new(tex);
                        if !p.exists() {
                            warnings.push(format!(
                                "Title texture not found on disk, skipped: {tex}"
                            ));
                            continue;
                        }
                        let tname = p
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "texture.png".to_string());
                        let dst = collision_safe_name(&tname, &mut used_texture);
                        items.push(CopyItem {
                            src: p.to_path_buf(),
                            dst_name: dst.clone(),
                            subfolder: "textures",
                        });
                        texture_new_paths.insert(
                            tex.clone(),
                            options.dest_dir.join("media").join("textures").join(&dst),
                        );
                    }
                }
            }
        }
    }

    CopyPlan {
        items,
        media_new_paths,
        proxy_new_paths,
        skim_new_paths,
        texture_new_paths,
        lut_new_path,
        offline_skipped,
        warnings,
    }
}

/// Return `name` unchanged on first use. On collision, append `_001`,
/// `_002`, ... before the extension until a free slot is found.
fn collision_safe_name(name: &str, used: &mut HashSet<String>) -> String {
    if !used.contains(name) {
        used.insert(name.to_string());
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut counter: u32 = 1;
    loop {
        let candidate = format!("{stem}_{counter:03}{ext}");
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        counter += 1;
    }
}

/// Execute all planned copies in order, invoking progress after each.
/// Creates any needed subfolders. Returns the first copy failure, stopping
/// the loop (caller will roll back the whole destination).
fn execute_copy_plan(
    plan: &CopyPlan,
    dest_dir: &Path,
    progress: Option<&ProgressFn>,
) -> Result<(), ConsolidateError> {
    // Ensure subfolders exist before copying into them.
    let mut created_subfolders: HashSet<&'static str> = HashSet::new();
    for item in &plan.items {
        if item.subfolder.is_empty() {
            continue;
        }
        if created_subfolders.insert(item.subfolder) {
            let sub = dest_dir.join("media").join(item.subfolder);
            std::fs::create_dir_all(&sub).map_err(ConsolidateError::CreateDir)?;
        }
    }

    let total = plan.items.len();
    for (idx, item) in plan.items.iter().enumerate() {
        let dst = item.dst_path(dest_dir);
        if let Err(err) = std::fs::copy(&item.src, &dst) {
            return Err(ConsolidateError::CopyFailed {
                src: item.src.clone(),
                dst,
                err,
            });
        }
        if let Some(cb) = progress {
            cb(idx + 1, total, &item.dst_name);
        }
    }
    Ok(())
}

/// Build a new MediaPool where each item's `path` / `proxy_path` / `skim_file`
/// points at its consolidated destination.
fn remap_media_pool(original: &MediaPool, plan: &CopyPlan) -> MediaPool {
    let mut new_pool = MediaPool::new();
    for bin in &original.bins {
        new_pool.bins.push(bin.clone());
    }
    for item in original.all() {
        let mut new_item: MediaItem = item.clone();
        if let Some(p) = plan.media_new_paths.get(&item.id) {
            new_item.path = p.clone();
            new_item.offline = false;
        }
        new_item.proxy_path = plan.proxy_new_paths.get(&item.id).cloned()
            .or_else(|| item.proxy_path.clone());
        new_item.skim_file = plan.skim_new_paths.get(&item.id).cloned()
            .or_else(|| item.skim_file.clone());
        new_pool.add(new_item);
    }
    new_pool
}

/// Build new sequences where `TitleData::texture_fill_path` entries point
/// at their consolidated destinations.
fn remap_sequences(original: &[Sequence], plan: &CopyPlan) -> Vec<Sequence> {
    let mut out = original.to_vec();
    for seq in &mut out {
        for track in &mut seq.tracks {
            for clip in &mut track.clips {
                if let ClipKind::Title(td) = &mut clip.kind {
                    if let Some(orig) = td.texture_fill_path.clone() {
                        if let Some(new) = plan.texture_new_paths.get(&orig) {
                            td.texture_fill_path = Some(new.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

/// Consolidate a project to a new, self-contained directory.
///
/// The original project is never modified. On any error the destination
/// directory is deleted in full so no half-consolidated remains.
pub fn consolidate_project(
    source_project_dir: &Path,
    media_pool: &MediaPool,
    sequences: &[Sequence],
    color_settings: &ColorSettings,
    options: ConsolidateOptions,
    progress: Option<ProgressFn>,
) -> Result<ConsolidateResult, ConsolidateError> {
    if !source_project_dir.exists() {
        return Err(ConsolidateError::SourceNotFound(source_project_dir.to_path_buf()));
    }
    if options.dest_dir.exists() {
        return Err(ConsolidateError::DestinationExists(options.dest_dir.clone()));
    }

    let plan = build_copy_plan(media_pool, sequences, color_settings, &options);

    // Create destination layout (media/ is always needed).
    std::fs::create_dir_all(options.dest_dir.join("media"))
        .map_err(ConsolidateError::CreateDir)?;

    let files_total = plan.items.len();

    if let Err(e) = execute_copy_plan(&plan, &options.dest_dir, progress.as_ref()) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(e);
    }

    // Remap MediaPool and sequences to use the new paths.
    let new_pool = remap_media_pool(media_pool, &plan);
    let new_seqs = remap_sequences(sequences, &plan);
    let mut new_color = color_settings.clone();
    new_color.global_lut_path = plan.lut_new_path.clone();

    // Save the consolidated project in the destination directory. The
    // existing save_project_with_color_and_backups automatically relativises
    // paths against dest_dir so everything stays portable.
    if let Err(e) = save_project_with_color_and_backups(
        &options.dest_dir,
        &new_seqs,
        &new_pool,
        &new_color,
        None,
        None,
        5,
    ) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(ConsolidateError::SaveFailed(e));
    }

    Ok(ConsolidateResult {
        files_copied: files_total,
        offline_skipped: plan.offline_skipped,
        warnings: plan.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "klipp_consolidate_{name}_{pid}_{nanos}",
            pid = std::process::id(),
            nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    // ── collision_safe_name ─────────────────────────────────────────

    #[test]
    fn collision_safe_first_use_unchanged() {
        let mut used = HashSet::new();
        assert_eq!(collision_safe_name("Clip.mov", &mut used), "Clip.mov");
    }

    #[test]
    fn collision_safe_second_use_gets_001() {
        let mut used = HashSet::new();
        collision_safe_name("Clip.mov", &mut used);
        assert_eq!(collision_safe_name("Clip.mov", &mut used), "Clip_001.mov");
    }

    #[test]
    fn collision_safe_many_reuses_get_counter() {
        let mut used = HashSet::new();
        for expected in &["Clip.mov", "Clip_001.mov", "Clip_002.mov", "Clip_003.mov"] {
            assert_eq!(&collision_safe_name("Clip.mov", &mut used), expected);
        }
    }

    #[test]
    fn collision_safe_no_extension() {
        let mut used = HashSet::new();
        collision_safe_name("README", &mut used);
        assert_eq!(collision_safe_name("README", &mut used), "README_001");
    }

    // ── Smoke tests that don't require a full project ──────────────

    #[test]
    fn destination_exists_returns_error() {
        let src = tmp_path("src");
        let dst = tmp_path("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let pool = MediaPool::new();
        let seqs: Vec<Sequence> = Vec::new();
        let color = ColorSettings::default();

        let err = consolidate_project(
            &src,
            &pool,
            &seqs,
            &color,
            ConsolidateOptions {
                dest_dir: dst.clone(),
                include_proxies: false,
                include_skims: false,
            },
            None,
        )
        .expect_err("should fail when dest exists");
        assert!(matches!(err, ConsolidateError::DestinationExists(_)));

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn source_not_found_returns_error() {
        let src = tmp_path("nonexistent");
        let dst = tmp_path("dst");
        let pool = MediaPool::new();
        let seqs: Vec<Sequence> = Vec::new();
        let color = ColorSettings::default();

        let err = consolidate_project(
            &src,
            &pool,
            &seqs,
            &color,
            ConsolidateOptions {
                dest_dir: dst,
                include_proxies: false,
                include_skims: false,
            },
            None,
        )
        .expect_err("should fail when source is missing");
        assert!(matches!(err, ConsolidateError::SourceNotFound(_)));
    }
}
