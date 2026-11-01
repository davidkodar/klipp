//! Checking a collected project (#3241): every file it refers to is
//! inside the collected folder, and a manifest of what is there.
//!
//! References are read as the collected project stores them, never as the
//! loader would find them: the loader falls back to where a file was
//! before the copy, and the check must not pass because an original is
//! still reachable. Each one is resolved against the folder only, and
//! counts as inside on its canonical path (links followed, `..` resolved),
//! never by how its string looks.
//!
//! The manifest, `collect_manifest.json`, lists each resource's id, its
//! path relative to the folder, size, SHA-256 and status, so a copy can be
//! checked again later ([`reverify`]). It is not a second project format.
//! Read back it is untrusted: capped in size, every path relative and
//! kept inside the folder.

use std::path::{Component, Path, PathBuf};

use klipp_timeline::GradeAccess as _;
use serde::{Deserialize, Serialize};

/// The manifest's file name in a collected folder.
pub const MANIFEST: &str = "collect_manifest.json";

/// The largest manifest read back.
const MAX_MANIFEST_BYTES: u64 = 8 << 20;

/// Where a reference leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A file inside the collected folder.
    Collected,
    /// Out of it: a path that resolves elsewhere, through a link or `..`
    /// or as an absolute path left pointing at the original.
    Outside,
    /// Nothing there.
    Missing,
}

/// One resource the collected project uses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    /// What refers to it: a media item's id, or the kind and its place.
    pub id: String,
    pub kind: String,
    /// Relative to the folder when collected; as stored otherwise.
    pub path: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub status: Status,
}

/// What a check found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub resources: Vec<Resource>,
}

impl Manifest {
    /// Every resource is collected.
    pub fn verified(&self) -> bool {
        self.resources.iter().all(|r| r.status == Status::Collected)
    }

    /// The ones that aren't, in words: "media <id>: <path> is outside the folder".
    pub fn failures(&self) -> Vec<String> {
        self.resources
            .iter()
            .filter(|r| r.status != Status::Collected)
            .map(|r| format!("{} {}: {} {}", r.kind, r.id, r.path, if r.status == Status::Outside { "is outside the collected folder" } else { "is missing" }))
            .collect()
    }
}

/// `stored`, as a reference of the project in `root` (canonical), is
/// inside it, outside it or missing; and its path relative to `root` when
/// inside.
fn place(root: &Path, stored: &Path) -> (Status, Option<PathBuf>) {
    let candidate = if stored.is_absolute() { stored.to_path_buf() } else { root.join(stored) };
    match candidate.canonicalize() {
        Ok(real) if real.is_file() => match real.strip_prefix(root) {
            Ok(rel) => (Status::Collected, Some(rel.to_path_buf())),
            Err(_) => (Status::Outside, None),
        },
        Ok(_) => (Status::Missing, None),
        // Not there: outside when even its string leaves the folder.
        Err(_) => (if crate::path_util::normalize_path(&candidate).starts_with(root) { Status::Missing } else { Status::Outside }, None),
    }
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use sha2::Digest as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn resource(root: &Path, id: String, kind: &str, stored: &Path) -> Resource {
    let (status, rel) = place(root, stored);
    let (path, size, sha256) = match &rel {
        Some(rel) => {
            let real = root.join(rel);
            let size = std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0);
            (rel.to_string_lossy().into_owned(), size, sha256_of(&real).ok())
        }
        None => (stored.to_string_lossy().into_owned(), 0, None),
    };
    Resource { id, kind: kind.into(), path, size, sha256, status }
}

/// Every file the project in `dir` refers to, as it stores them, each
/// checked against `dir` alone; the fonts it carries too.
pub fn verify(dir: &Path) -> Result<Manifest, String> {
    let root = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut resources = Vec::new();
    // The media, from the manifest as written: not the loader's paths,
    // which fall back to where a file was before it was collected.
    let text = std::fs::read_to_string(root.join("media").join("manifest.json")).map_err(|e| format!("media/manifest.json: {e}"))?;
    let media: crate::format::MediaManifestJson = serde_json::from_str(&text).map_err(|e| format!("media/manifest.json: {e}"))?;
    for item in &media.items {
        if item.kind.as_ref().is_some_and(|k| *k != klipp_media::MediaItemKind::File) {
            continue;
        }
        resources.push(resource(&root, item.id.clone(), "media", Path::new(&item.path)));
        if let Some(proxy) = item.proxy_path.as_deref().filter(|_| item.proxy_ready) {
            resources.push(resource(&root, item.id.clone(), "proxy", Path::new(proxy)));
        }
    }
    // What the sequences, the colour settings and the Looks read.
    let (sequences, _, colour, ..) = crate::load::load_project(&root)?;
    for (lut, which) in [(&colour.global_lut_path, "timeline"), (&colour.default_input_lut, "default input"), (&colour.viewing_lut, "viewing")] {
        if let Some(lut) = lut {
            resources.push(resource(&root, which.into(), "lut", lut));
        }
    }
    for seq in &sequences {
        for clip in seq.tracks.iter().flat_map(|t| &t.clips) {
            if let klipp_timeline::ClipKind::Title(td) = &clip.kind {
                for file in crate::consolidate::title_files(td) {
                    resources.push(resource(&root, clip.id.0.to_string(), "title file", Path::new(&file)));
                }
            }
        }
    }
    let looks = crate::load::load_looks(&root);
    let graphs = sequences.iter().flat_map(|s| s.grades()).chain(looks.iter().map(|l| l.grade()));
    for g in graphs {
        for file in crate::consolidate::node_files(g) {
            resources.push(resource(&root, "grade".into(), "grade file", file));
        }
    }
    // The fonts it carries: files, not references.
    if let Ok(entries) = std::fs::read_dir(root.join("fonts")) {
        let mut fonts: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        fonts.sort();
        for f in fonts {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            resources.push(resource(&root, name, "font", &f));
        }
    }
    resources.dedup_by(|a, b| a.kind == b.kind && a.path == b.path);
    Ok(Manifest { format: 1, resources })
}

/// [`verify`], written as the folder's manifest.
pub fn verify_and_write(dir: &Path) -> Result<Manifest, String> {
    let manifest = verify(dir)?;
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MANIFEST), text).map_err(|e| format!("{MANIFEST}: {e}"))?;
    Ok(manifest)
}

/// The folder checked again against its manifest: each collected file
/// still inside, its size and checksum unchanged. The manifest is read as
/// untrusted: capped, every path relative with no `..`, resolved inside.
/// What doesn't hold, said; empty when all does.
pub fn reverify(dir: &Path) -> Result<Vec<String>, String> {
    let root = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = root.join(MANIFEST);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{MANIFEST}: {e}"))?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST} isn't a plain file of at most {} MB", MAX_MANIFEST_BYTES >> 20));
    }
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let mut wrong = Vec::new();
    for r in manifest.resources.iter().filter(|r| r.status == Status::Collected) {
        let rel = Path::new(&r.path);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            wrong.push(format!("{}: not a path inside the folder", r.path));
            continue;
        }
        let now = resource(&root, r.id.clone(), &r.kind, rel);
        if now.status != Status::Collected {
            wrong.push(format!("{}: {}", r.path, if now.status == Status::Outside { "now outside the folder" } else { "gone" }));
        } else if now.size != r.size || now.sha256 != r.sha256 {
            wrong.push(format!("{}: changed since it was collected", r.path));
        }
    }
    Ok(wrong)
}
