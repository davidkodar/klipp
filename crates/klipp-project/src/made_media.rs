//! A project carries the media Klipp made or fetched for it (#730).
//!
//! Music and sound effects from the libraries, narration, recordings and
//! synthesized sound are written to the kept made-media folder
//! (`klipp_media::made`) — and, by earlier builds, to the user's cache.
//! Neither is the project's: a cache is safe to delete, and the kept
//! folder is not moved with the project. On save, each one the media
//! pool uses is copied into `<project>/media/…`, with its credit file,
//! and the pool points there from then on.
//!
//! Copied, not moved: another project may use the same download.

use std::path::{Path, PathBuf};
use tracing::warn;

/// What a save brought into the project.
#[derive(Debug, Default)]
pub struct MediaAdoption {
    pub adopted: usize,
    pub failed: Vec<(PathBuf, std::io::Error)>,
}

impl MediaAdoption {
    pub fn is_empty(&self) -> bool {
        self.adopted == 0 && self.failed.is_empty()
    }
}

/// The credit and licence file kept beside a library download.
fn credit_of(path: &Path) -> Option<PathBuf> {
    let credit = path.with_extension("klipp-credit.txt");
    credit.is_file().then_some(credit)
}

/// Copy `from` to `to`, unless `to` is already that file.
fn copy_once(from: &Path, to: &Path) -> std::io::Result<()> {
    if let (Ok(a), Ok(b)) = (std::fs::metadata(from), std::fs::metadata(to)) {
        if a.len() == b.len() {
            return Ok(());
        }
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside and renamed into place, so a save interrupted
    // midway never leaves a truncated file the project points at.
    let staging = to.with_extension(format!("{}.part", to.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::copy(from, &staging)?;
    std::fs::rename(&staging, to)
}

/// Bring every pool item under the made-media folders into
/// `project_dir/media/`, and point the pool at the copies.
pub fn adopt_made_media(project_dir: &Path, pool: &mut klipp_media::MediaPool) -> MediaAdoption {
    let mut report = MediaAdoption::default();
    let media = project_dir.join("media");
    for item in pool.all_mut() {
        // Already the project's.
        if item.path.starts_with(project_dir) {
            continue;
        }
        let Some(rel) = klipp_media::made::made_relative(&item.path) else { continue };
        let to = media.join(&rel);
        match copy_once(&item.path, &to) {
            Ok(()) => {
                if let Some(credit) = credit_of(&item.path) {
                    if let Err(e) = copy_once(&credit, &to.with_extension("klipp-credit.txt")) {
                        warn!("could not bring the credit for {} into the project: {e}", item.path.display());
                    }
                }
                item.path = to;
                report.adopted += 1;
            }
            Err(e) => {
                warn!("could not bring {} into the project: {e}", item.path.display());
                report.failed.push((item.path.clone(), e));
            }
        }
    }
    report
}
