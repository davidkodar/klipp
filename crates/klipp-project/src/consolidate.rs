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
//! - Copies every file a grade node reads (a LUT, an external matte or
//!   AI mask, a difference key's plate), in the sequences and in the
//!   project's Looks, into `dest_dir/media/grade/`, and writes the Looks
//!   into the destination (#1016). Graphs inside Compound nodes are
//!   walked too, and the grade stills come along with their grades
//!   (#1042).
//! - Writes the project via the existing
//!   `save_project_with_color_and_backups` flow — path relativisation is
//!   handled by the existing `to_project_relative` plumbing.
//!
//! Offline media items are skipped and returned in
//! `ConsolidateResult::offline_skipped` so the UI can surface them.
//!
//! Three modes (#77), chosen by [`ConsolidateMode`]: collect the whole
//! pool, whole files, to hand a project on; only the media the sequences
//! use, whole files; or an archive of only what they use, each file cut
//! to the stretches its clips use plus handles, the timeline moved onto
//! the cut files (see [`crate::consolidate_trim`]). [`summarise`] says
//! what a run will copy before anything is written.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use klipp_core::MediaId;
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::NodeGraph;
use klipp_timeline::{ClipKind, GradeAccess, Look, Sequence};

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
    /// What is copied (#77).
    pub mode: ConsolidateMode,
    /// The project's Looks library, written into the destination with
    /// the files its grades read (#1016).
    pub looks: Vec<Look>,
    /// The font families the titles name, as found where the project is
    /// (#3009): copied with their licences into `fonts/`, or listed with
    /// why not. [`crate::project_fonts::fonts_used`] names them.
    pub fonts: Vec<crate::project_fonts::CollectFont>,
}

/// What a consolidation copies (#77).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ConsolidateMode {
    /// Every item in the pool, whole files: the project handed to someone
    /// who may want the unused takes.
    #[default]
    Collect,
    /// Only the media the sequences use, whole files.
    UsedOnly,
    /// Only what the sequences use, each file cut to the stretches its
    /// clips use, widened by `handle_secs` both ways.
    Trimmed { handle_secs: f64 },
}

/// What a consolidation will do, said before it is run (#77).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConsolidateSummary {
    /// Files written: whole copies and cut stretches.
    pub files: usize,
    /// Their size, in bytes; a cut file's estimated from its share of
    /// the source's length.
    pub bytes: u64,
    /// Files to be cut to what is used.
    pub trimmed: usize,
    /// Media in the pool left out because no sequence uses it.
    pub left_out: Vec<String>,
    /// Media that cannot be copied because it is offline.
    pub offline: Vec<String>,
}

/// Progress callback signature: `(files_done, files_total, current_filename)`.
pub type ProgressFn = Box<dyn Fn(usize, usize, &str) + Send>;

/// Progress callback for the archive phase: `(bytes_done, bytes_total, current_filename)`.
/// Unlike `ProgressFn`, this is byte-granular — a single 10 GB file is one callback
/// at the end (improvable later with a counting writer).
pub type ArchiveProgressFn = Box<dyn Fn(u64, u64, &str) + Send>;

/// Result summary for a consolidation run.
#[derive(Debug)]
pub struct ConsolidateResult {
    pub files_copied: usize,
    pub offline_skipped: Vec<String>,
    pub warnings: Vec<String>,
    /// Media left out because no sequence uses it (#77).
    pub left_out: Vec<String>,
    /// Files cut to what the sequences use (#77).
    pub files_trimmed: usize,
    /// Font families copied in (#3009); one left out is in `warnings`.
    pub fonts_copied: Vec<String>,
    /// The copy checked (#3241): every resource it refers to and where it
    /// leads, as written to its `collect_manifest.json`; `Err` when it
    /// couldn't be checked.
    pub verification: Result<crate::collect_verify::Manifest, String>,
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
    /// A source path pointed to by the project turned into a symlink
    /// between planning and copy. Refusing is a defense-in-depth
    /// backstop for the plan-time check; if this fires it means
    /// either a race or a filesystem oddity and we'd rather abort
    /// than follow the symlink.
    #[error("Refused symlinked source {src} mid-copy — potential exfiltration")]
    SymlinkRefused { src: PathBuf },
    #[error("Project save failed after copying: {0}")]
    SaveFailed(String),
    #[error("Archive failed at {zip_path}: {err}")]
    ArchiveFailed { zip_path: PathBuf, err: String },
}

/// Is `path`'s final component a symbolic link?
///
/// Uses `symlink_metadata` so this only flags the leaf — symlinked
/// *directory* components in the ancestry (e.g. `/home/u/nas` being
/// a symlink while the leaf `clip.mov` is a regular file under it)
/// are fine. We only refuse when the consolidate is about to copy
/// the contents of a link itself, which is the exfiltration vector.
///
/// Returns `false` for anything we can't metadata-stat (the caller
/// has already done an `exists()` check, so a stat failure here is
/// rare; being permissive in that edge avoids a false-refuse).
fn is_symlinked_leaf(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
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
    input_lut_new_path: Option<PathBuf>,
    viewing_lut_new_path: Option<PathBuf>,
    /// A file a grade node reads → its new absolute path (#1016).
    node_file_new_paths: HashMap<PathBuf, PathBuf>,
    offline_skipped: Vec<String>,
    warnings: Vec<String>,
    /// Items left out as unused (#77), by file name.
    left_out: Vec<String>,
}

/// Build the copy plan from the media pool, sequences, and color settings.
/// Purely in-memory — no filesystem side effects. Returns the plan plus any
/// upfront warnings (missing LUT, missing texture files).
/// `only`, when given, is the media to copy (the used media); `cut` the
/// files cut separately, whose items are not copied whole.
fn build_copy_plan(
    media_pool: &MediaPool,
    sequences: &[Sequence],
    color_settings: &ColorSettings,
    options: &ConsolidateOptions,
    only: Option<&HashSet<MediaId>>,
    cut: &HashSet<PathBuf>,
    still_grades: &[&NodeGraph],
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
    let mut left_out: Vec<String> = Vec::new();
    // A file several items share (a subclip and its parent) is copied once.
    let mut copied_paths: HashMap<PathBuf, PathBuf> = HashMap::new();

    // Primary media
    for item in media_pool.all() {
        if item.kind != klipp_media::MediaItemKind::File {
            continue;
        }
        if only.is_some_and(|used| !used.contains(&item.id)) {
            left_out.push(item.filename.clone());
            continue;
        }
        if cut.contains(&item.path) {
            continue;
        }
        if let Some(dst) = copied_paths.get(&item.path) {
            media_new_paths.insert(item.id, dst.clone());
            continue;
        }
        if item.offline || !item.path.exists() {
            offline_skipped.push(item.path.display().to_string());
            continue;
        }
        if is_symlinked_leaf(&item.path) {
            // Refuse: a hostile .klipp can point `item.path` at
            // ~/.ssh/id_rsa (or similar) via a symlink, and a naive
            // fs::copy would bake the target bytes into the
            // consolidated folder that the victim is about to
            // ship. Warn + skip; the consolidated project lists
            // the clip as offline.
            warnings.push(format!(
                "Refused symlinked media source (potential exfiltration): {}",
                item.path.display()
            ));
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
        copied_paths.insert(item.path.clone(), options.dest_dir.join("media").join(&dst_name));

        // Proxy (opt-in)
        if options.include_proxies {
            if let Some(proxy) = &item.proxy_path {
                if item.proxy_ready && proxy.exists() && is_symlinked_leaf(proxy) {
                    warnings.push(format!(
                        "Refused symlinked proxy (potential exfiltration): {}",
                        proxy.display()
                    ));
                } else if item.proxy_ready && proxy.exists() {
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
                    if is_symlinked_leaf(skim) {
                        warnings.push(format!(
                            "Refused symlinked skim (potential exfiltration): {}",
                            skim.display()
                        ));
                    } else {
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
    }

    // The colour settings' LUTs: the timeline's, the default input and
    // the viewing LUT.
    let mut lut = |path: &Option<PathBuf>, what: &str| match path {
        Some(lut) if lut.exists() && is_symlinked_leaf(lut) => {
            warnings.push(format!("Refused symlinked {what} (potential exfiltration): {}", lut.display()));
            None
        }
        Some(lut) if lut.exists() => {
            let lname = lut.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "project.cube".to_string());
            let dst = collision_safe_name(&lname, &mut used_lut);
            items.push(CopyItem { src: lut.clone(), dst_name: dst.clone(), subfolder: "luts" });
            Some(options.dest_dir.join("media").join("luts").join(&dst))
        }
        Some(lut) => {
            warnings.push(format!("{} not found on disk, skipped: {}", capitalise(what), lut.display()));
            None
        }
        None => None,
    };
    let lut_new_path = lut(&color_settings.global_lut_path, "global LUT");
    let input_lut_new_path = lut(&color_settings.default_input_lut, "default input LUT");
    let viewing_lut_new_path = lut(&color_settings.viewing_lut, "viewing LUT");

    // The files titles draw: each text layer's texture and each picture
    // layer's image, once each however many titles use them.
    for seq in sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                if let ClipKind::Title(td) = &clip.kind {
                    for tex_owned in title_files(td) {
                        let tex = tex_owned.as_str();
                        if texture_new_paths.contains_key(tex) {
                            continue;
                        }
                        let p = Path::new(tex);
                        if !p.exists() {
                            warnings.push(format!(
                                "Title file not found on disk, skipped: {tex}"
                            ));
                            continue;
                        }
                        if is_symlinked_leaf(p) {
                            warnings.push(format!(
                                "Refused symlinked title file (potential exfiltration): {tex}"
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
                            tex.to_string(),
                            options.dest_dir.join("media").join("textures").join(&dst),
                        );
                    }
                }
            }
        }
    }

    // Files the grade nodes read, in the sequences and the Looks: LUTs,
    // external mattes (AI masks among them, which live in a cache that is
    // pruned), difference-key plates. A LUT from the library that ships
    // with Klipp is saved by its name there, so it is left where it is.
    let mut node_file_new_paths: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut used_grade: HashSet<String> = HashSet::new();
    let library = klipp_core::assets::bundled_luts_dir();
    let graphs = sequences.iter().flat_map(|s| s.grades()).chain(options.looks.iter().map(|l| l.grade())).chain(still_grades.iter().copied());
    for path in graphs.flat_map(node_files) {
        if node_file_new_paths.contains_key(path) || library.is_some_and(|lib| path.starts_with(lib)) {
            continue;
        }
        if !path.exists() {
            warnings.push(format!("File a grade reads not found on disk, skipped: {}", path.display()));
            continue;
        }
        if is_symlinked_leaf(path) {
            warnings.push(format!("Refused symlinked file a grade reads (potential exfiltration): {}", path.display()));
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "grade_file".to_string());
        let dst = collision_safe_name(&name, &mut used_grade);
        items.push(CopyItem { src: path.clone(), dst_name: dst.clone(), subfolder: "grade" });
        // Relative to the project, so the folder can move: opened, the
        // project resolves it against where it is.
        node_file_new_paths.insert(path.clone(), Path::new("media").join("grade").join(&dst));
    }

    CopyPlan {
        items,
        media_new_paths,
        proxy_new_paths,
        skim_new_paths,
        texture_new_paths,
        lut_new_path,
        input_lut_new_path,
        viewing_lut_new_path,
        node_file_new_paths,
        offline_skipped,
        warnings,
        left_out,
    }
}

/// The files a graph's nodes read: LUTs, external mattes and
/// difference-key plates, in the graph and in every graph a Compound node
/// in it holds (#1042).
pub(crate) fn node_files(g: &NodeGraph) -> Vec<&PathBuf> {
    let mut out = Vec::new();
    for n in &g.nodes {
        let p = &n.params;
        out.extend([p.lut_path.as_ref(), p.matte_image_path.as_ref(), p.difference_key.as_ref().and_then(|d| d.plate_path.as_ref())].into_iter().flatten());
        if let Some(inner) = &p.compound_subgraph {
            out.extend(node_files(inner));
        }
    }
    out
}

/// A grade's file path a collected project stores relative to itself,
/// under its `media/grade/` folder, resolved against `dir`. The file is
/// untrusted: only a path of plain names is, never one with `..` or a
/// root that could lead out of the folder. Any other path, absolute or a
/// library LUT's name, is left as it is.
pub(crate) fn resolve_collected_grade_files(g: &mut NodeGraph, dir: &Path) {
    let fix = |p: &mut PathBuf| {
        let plain = p.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        if plain && p.starts_with("media/grade") {
            *p = dir.join(&*p);
        }
    };
    for n in &mut g.nodes {
        let params = &mut n.params;
        for p in [params.lut_path.as_mut(), params.matte_image_path.as_mut(), params.difference_key.as_mut().and_then(|d| d.plate_path.as_mut())].into_iter().flatten() {
            fix(p);
        }
        if let Some(inner) = params.compound_subgraph.as_mut() {
            resolve_collected_grade_files(inner, dir);
        }
    }
}

/// Each file a title draws: a text layer's texture, a picture layer's
/// image (#3241).
pub(crate) fn title_files(td: &klipp_timeline::TitleData) -> Vec<String> {
    let mut out = Vec::new();
    for o in &td.objects {
        match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => out.extend(tb.texture_fill.as_ref().and_then(|t| t.file_path()).map(str::to_string)),
            klipp_timeline::TitleObjectContent::Image(img) => out.push(img.path.clone()),
            _ => {}
        }
    }
    out
}

/// Each file path a title draws, to change in place.
fn for_each_title_file(td: &mut klipp_timeline::TitleData, mut f: impl FnMut(&mut String)) {
    for o in &mut td.objects {
        match &mut o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                if let Some(p) = tb.texture_fill.as_mut().and_then(|t| t.file_path_mut()) {
                    f(p);
                }
            }
            klipp_timeline::TitleObjectContent::Image(img) => f(&mut img.path),
            _ => {}
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|first| first.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Point every file `g`'s nodes read at its copy, where it has one, in
/// Compound nodes' graphs too.
fn remap_node_files(g: &mut NodeGraph, new_paths: &HashMap<PathBuf, PathBuf>) {
    for n in &mut g.nodes {
        let p = &mut n.params;
        for path in [p.lut_path.as_mut(), p.matte_image_path.as_mut(), p.difference_key.as_mut().and_then(|d| d.plate_path.as_mut())].into_iter().flatten() {
            if let Some(new) = new_paths.get(path) {
                *path = new.clone();
            }
        }
        if let Some(inner) = p.compound_subgraph.as_mut() {
            remap_node_files(inner, new_paths);
        }
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
/// The credit file kept beside a library download (`song.klipp-credit.txt`
/// for `song.mp3`), copied beside each copy of the file: the whole copy,
/// or each cut of it. What can't be copied is said, not fatal (#1893).
fn copy_credits(original: &MediaPool, collected: &MediaPool, cuts: &HashMap<PathBuf, Vec<crate::consolidate_trim::Segment>>) -> Vec<String> {
    let credit_of = |p: &Path| p.with_extension("klipp-credit.txt");
    let mut warnings = Vec::new();
    for item in original.all() {
        let credit = credit_of(&item.path);
        if !credit.is_file() || is_symlinked_leaf(&credit) {
            continue;
        }
        let copies: Vec<PathBuf> = match cuts.get(&item.path) {
            Some(segments) => segments.iter().map(|s| s.path.clone()).collect(),
            None => collected.get(&item.id).map(|m| m.path.clone()).filter(|p| *p != item.path).into_iter().collect(),
        };
        for copy in copies {
            let to = credit_of(&copy);
            if to.exists() {
                continue;
            }
            if let Err(e) = std::fs::copy(&credit, &to) {
                warnings.push(format!("the credit for {} was not copied: {e}", item.filename));
            }
        }
    }
    warnings
}

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
        // TOCTOU backstop: `build_copy_plan` rejected symlinked
        // leaves but a malicious process could swap the source out
        // between plan and copy. Re-check here and abort the whole
        // consolidate rather than risk copying a link's target.
        if is_symlinked_leaf(&item.src) {
            return Err(ConsolidateError::SymlinkRefused {
                src: item.src.clone(),
            });
        }
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
        seq.edit_each_grade(|g| remap_node_files(g, &plan.node_file_new_paths));
        for track in &mut seq.tracks {
            for clip in &mut track.clips {
                if let ClipKind::Title(td) = &mut clip.kind {
                    for_each_title_file(td, |p| {
                        if let Some(new) = plan.texture_new_paths.get(p.as_str()) {
                            *p = new.to_string_lossy().to_string();
                        }
                    });
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
        // P-9: clippy flagged the clone as redundant — we partial-move
        // `dest_dir` out; `options` isn't used again on this error path.
        return Err(ConsolidateError::DestinationExists(options.dest_dir));
    }

    // Create destination layout (media/ is always needed).
    std::fs::create_dir_all(options.dest_dir.join("media"))
        .map_err(ConsolidateError::CreateDir)?;

    let used = (options.mode != ConsolidateMode::Collect).then(|| crate::consolidate_trim::used_media(media_pool, sequences));
    // The archive's cut files first: a file that cannot be cut cleanly is
    // copied whole below instead.
    let mut cut_warnings = Vec::new();
    let cuts = match options.mode {
        ConsolidateMode::Trimmed { handle_secs } => cut_used_files(media_pool, sequences, handle_secs, &options.dest_dir, progress.as_ref(), &mut cut_warnings),
        _ => HashMap::new(),
    };
    // A file with a clip no cut of it holds is copied whole instead: it
    // would be remapped wrong (#963 review).
    let mut cuts = cuts;
    for path in crate::consolidate_trim::unheld(media_pool, sequences, &cuts) {
        if let Some(segments) = cuts.remove(&path) {
            for s in segments {
                let _ = std::fs::remove_file(&s.path);
            }
            cut_warnings.push(format!("{} copied whole: a clip runs past what was cut", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
    }
    let cut_paths: HashSet<PathBuf> = cuts.keys().cloned().collect();
    // The grade stills, as saved with the source project: their pictures
    // go with the project, and their grades read files like any other.
    let mut stills = crate::grade_stills::load_grade_stills(source_project_dir);
    let still_grades: Vec<&NodeGraph> = stills.iter().filter_map(|s| s.grade.as_ref()).collect();
    let mut plan = build_copy_plan(media_pool, sequences, color_settings, &options, used.as_ref(), &cut_paths, &still_grades);
    plan.warnings.extend(cut_warnings);
    let files_trimmed = cuts.len();

    let files_total = plan.items.len() + cuts.values().map(Vec::len).sum::<usize>();

    if let Err(e) = execute_copy_plan(&plan, &options.dest_dir, progress.as_ref()) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(e);
    }

    // Remap MediaPool and sequences to use the new paths.
    let new_pool = remap_media_pool(media_pool, &plan);
    let new_seqs = remap_sequences(sequences, &plan);
    let (new_pool, new_seqs) = if cuts.is_empty() { (new_pool, new_seqs) } else { crate::consolidate_trim::remap(&new_pool, &new_seqs, &cuts) };
    // Unused media leaves the archive's pool; items split onto several
    // cut files are new and stay.
    let new_pool = match &used {
        Some(used) => {
            let mut pool = new_pool;
            let drop: Vec<MediaId> = pool
                .all()
                .filter(|m| m.kind == klipp_media::MediaItemKind::File && media_pool.get(&m.id).is_some() && !used.contains(&m.id))
                .map(|m| m.id)
                .collect();
            for id in drop {
                pool.remove(&id);
            }
            pool
        }
        None => new_pool,
    };
    // The credit and licence beside a library download go with its copy,
    // or the collected project loses what its music asks of it (#1893).
    let credit_warnings = copy_credits(media_pool, &new_pool, &cuts);
    plan.warnings.extend(credit_warnings);
    let mut new_color = color_settings.clone();
    // A global LUT that was copied is read from its copy. One that could
    // not be (not on disk, or refused as a symlink) keeps the path the
    // project names, so it can be relinked rather than lost: the open
    // path keeps a missing LUT for the same reason (#1019).
    new_color.global_lut_path = plan.lut_new_path.clone().or_else(|| color_settings.global_lut_path.clone());
    new_color.default_input_lut = plan.input_lut_new_path.clone().or_else(|| color_settings.default_input_lut.clone());
    new_color.viewing_lut = plan.viewing_lut_new_path.clone().or_else(|| color_settings.viewing_lut.clone());

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
        "",
        5,
        None,
        None,
        None,
    ) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(ConsolidateError::SaveFailed(e));
    }
    // The Looks, reading their files where they were copied to.
    let mut looks = options.looks.clone();
    for look in &mut looks {
        look.edit_grade(|g| remap_node_files(g, &plan.node_file_new_paths));
    }
    if let Err(e) = crate::save::save_looks(&options.dest_dir, &looks) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(ConsolidateError::SaveFailed(e));
    }
    for still in &mut stills {
        if let Some(g) = still.grade.as_mut() {
            remap_node_files(g, &plan.node_file_new_paths);
        }
    }
    if !stills.is_empty() {
        if let Err(e) = crate::grade_stills::save_grade_stills(&options.dest_dir, &stills) {
            let _ = std::fs::remove_dir_all(&options.dest_dir);
            return Err(ConsolidateError::SaveFailed(e));
        }
    }
    // The titles' fonts, with their licences, listed beside the project
    // (#3009); one that can't go is said, not fatal.
    let (fonts, font_warnings) = crate::project_fonts::copy_fonts(&options.fonts, &options.dest_dir);
    plan.warnings.extend(font_warnings);
    if let Err(e) = crate::project_fonts::save_fonts(&options.dest_dir, &fonts) {
        let _ = std::fs::remove_dir_all(&options.dest_dir);
        return Err(ConsolidateError::SaveFailed(e));
    }

    // Checked against the copy alone, and the manifest written beside it.
    let verification = crate::collect_verify::verify_and_write(&options.dest_dir);
    match &verification {
        Ok(m) => plan.warnings.extend(m.failures()),
        Err(e) => plan.warnings.push(format!("The copy could not be checked: {e}")),
    }
    Ok(ConsolidateResult {
        verification,
        files_copied: files_total,
        offline_skipped: plan.offline_skipped,
        warnings: plan.warnings,
        left_out: plan.left_out,
        files_trimmed,
        fonts_copied: fonts.into_iter().map(|f| f.family).collect(),
    })
}

/// The files worth cutting, cut into `dest_dir/media/trimmed/`: each
/// used file whose used stretches, with handles, leave out a fifth or
/// more of it. A file whose cut fails or does not check out is left to
/// be copied whole, with a warning.
fn cut_used_files(
    pool: &MediaPool,
    sequences: &[Sequence],
    handle_secs: f64,
    dest_dir: &Path,
    progress: Option<&ProgressFn>,
    warnings: &mut Vec<String>,
) -> HashMap<PathBuf, Vec<crate::consolidate_trim::Segment>> {
    use crate::consolidate_trim as trim;
    let planned = trim_candidates(pool, sequences, handle_secs);
    let total: usize = planned.iter().map(|(_, r)| r.len()).sum();
    let mut done = 0;
    let mut names: HashSet<String> = HashSet::new();
    let mut cuts = HashMap::new();
    let dir = dest_dir.join("media").join("trimmed");
    if !planned.is_empty() && std::fs::create_dir_all(&dir).is_err() {
        warnings.push("Could not make the trimmed folder; files copied whole".into());
        return cuts;
    }
    for (item, ranges) in planned {
        let filename = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("media_{}", item.id.0));
        let mut segments = Vec::new();
        for (j, range) in ranges.iter().enumerate() {
            let stem = Path::new(&filename).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = Path::new(&filename).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let name = collision_safe_name(&format!("{stem}_part{}{ext}", j + 1), &mut names);
            done += 1;
            if let Some(cb) = progress {
                cb(done, total, &name);
            }
            match trim::cut(item, *range, &dir.join(&name)) {
                Ok(segment) => segments.push(segment),
                Err(why) => {
                    warnings.push(format!("{filename} copied whole: {why}"));
                    for s in &segments {
                        let _ = std::fs::remove_file(&s.path);
                    }
                    segments.clear();
                    break;
                }
            }
        }
        if !segments.is_empty() {
            cuts.insert(item.path.clone(), segments);
        }
    }
    cuts
}

/// The used files worth cutting, each with its used stretches: one item
/// of the file stands for it (its probe facts are the file's).
fn trim_candidates<'a>(pool: &'a MediaPool, sequences: &[Sequence], handle_secs: f64) -> Vec<(&'a MediaItem, Vec<crate::consolidate_trim::Range>)> {
    use crate::consolidate_trim as trim;
    let mut out: Vec<(&MediaItem, Vec<trim::Range>)> = trim::used_ranges(pool, sequences, handle_secs)
        .into_iter()
        .filter_map(|(path, ranges)| {
            let item = pool.all().find(|m| m.path == path && !m.offline && m.path.exists() && !is_symlinked_leaf(&m.path) && trim::can_cut(m))?;
            trim::worth_trimming(&ranges, item.duration.to_secs_f64()).then_some((item, ranges))
        })
        .collect();
    out.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    out
}

/// What [`consolidate_project`] will copy with these options, said before
/// it runs (#77). Reads file sizes; writes nothing.
pub fn summarise(media_pool: &MediaPool, sequences: &[Sequence], color_settings: &ColorSettings, options: &ConsolidateOptions) -> ConsolidateSummary {
    let used = (options.mode != ConsolidateMode::Collect).then(|| crate::consolidate_trim::used_media(media_pool, sequences));
    let candidates = match options.mode {
        ConsolidateMode::Trimmed { handle_secs } => trim_candidates(media_pool, sequences, handle_secs),
        _ => Vec::new(),
    };
    let cut: HashSet<PathBuf> = candidates.iter().map(|(m, _)| m.path.clone()).collect();
    let plan = build_copy_plan(media_pool, sequences, color_settings, options, used.as_ref(), &cut, &[]);
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let mut bytes: u64 = plan.items.iter().map(|i| size(&i.src)).sum();
    let mut files = plan.items.len();
    for (item, ranges) in &candidates {
        let duration = item.duration.to_secs_f64().max(1e-9);
        let share: f64 = ranges.iter().map(|r| r.len()).sum::<f64>() / duration;
        bytes += (size(&item.path) as f64 * share.min(1.0)) as u64;
        files += ranges.len();
    }
    ConsolidateSummary { files, bytes, trimmed: candidates.len(), left_out: plan.left_out, offline: plan.offline_skipped }
}

/// Stream a folder into a zip archive at `zip_path` using stored (no-compression)
/// entries. Paths inside the archive are relative to `src_folder`.
///
/// Media files are already compressed upstream (H.264, ProRes, DNxHR, etc.), so
/// Deflate would buy 0–3 % for minutes of CPU. The zip is a container here, not
/// a compressor — `CompressionMethod::Stored` is used unconditionally.
///
/// Files are written in sorted relative-path order so two runs over identical
/// input produce byte-identical archives.
///
/// On any error a best-effort `remove_file(zip_path)` cleans up the partial
/// archive; the source folder is never touched.
pub fn execute_zip_folder(
    src_folder: &Path,
    zip_path: &Path,
    progress: Option<&ArchiveProgressFn>,
) -> Result<(), ConsolidateError> {
    let archive_err = |err: String| ConsolidateError::ArchiveFailed {
        zip_path: zip_path.to_path_buf(),
        err,
    };

    // 1. Collect (absolute, relative) pairs for every regular file, sorted
    //    deterministically by relative path. Also compute total byte count.
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    collect_files(src_folder, src_folder, &mut files).map_err(|e| archive_err(e.to_string()))?;
    files.sort_by(|a, b| a.1.cmp(&b.1));

    let mut total_bytes: u64 = 0;
    for (abs, _) in &files {
        total_bytes = total_bytes.saturating_add(
            std::fs::metadata(abs)
                .map_err(|e| archive_err(format!("stat {}: {}", abs.display(), e)))?
                .len(),
        );
    }

    // 2. Open zip writer. Any error below must clean up the partial file.
    let result = write_zip(zip_path, &files, total_bytes, progress);

    if let Err(e) = &result {
        // Best-effort cleanup — ignore errors here; the original is what matters.
        let _ = std::fs::remove_file(zip_path);
        tracing::warn!("zip archive failed, removed partial file: {e}");
    }
    result
}

/// Recursively walk `dir`, appending `(absolute, relative)` pairs for every
/// regular file. `root` anchors the relative path. Symlinks are followed only
/// if they resolve to a regular file inside `root` — but we take the simple
/// route and just follow `metadata()` (not `symlink_metadata`), so missing
/// targets surface as an error in the next stat call (see `execute_zip_folder`).
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_files(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| path.clone());
            out.push((path, rel));
        }
        // Symlinks, fifos etc. are ignored — consolidated projects never
        // produce them.
    }
    Ok(())
}

/// Inner implementation split out so the caller can wrap the whole thing in
/// a clean-up-on-error shim without repeating the `?`-heavy body.
fn write_zip(
    zip_path: &Path,
    files: &[(PathBuf, PathBuf)],
    total_bytes: u64,
    progress: Option<&ArchiveProgressFn>,
) -> Result<(), ConsolidateError> {
    use std::io::{BufWriter, Write};
    use zip::write::{SimpleFileOptions, ZipWriter};
    use zip::CompressionMethod;

    let archive_err = |err: String| ConsolidateError::ArchiveFailed {
        zip_path: zip_path.to_path_buf(),
        err,
    };

    let file = std::fs::File::create(zip_path)
        .map_err(|e| archive_err(format!("create {}: {}", zip_path.display(), e)))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));

    let options: SimpleFileOptions =
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    let mut bytes_done: u64 = 0;
    for (abs, rel) in files {
        let rel_str = rel.to_string_lossy();
        zip.start_file(rel_str.as_ref(), options)
            .map_err(|e| archive_err(format!("start_file {rel_str}: {e}")))?;

        let mut src = std::fs::File::open(abs)
            .map_err(|e| archive_err(format!("open {}: {}", abs.display(), e)))?;
        let copied = std::io::copy(&mut src, &mut zip)
            .map_err(|e| archive_err(format!("copy {}: {}", abs.display(), e)))?;

        // Explicit flush so a partial io::copy followed by a drop doesn't
        // silently leave buffered data.
        zip.flush()
            .map_err(|e| archive_err(format!("flush: {e}")))?;

        bytes_done = bytes_done.saturating_add(copied);
        if let Some(cb) = progress {
            cb(bytes_done, total_bytes, &rel_str);
        }
    }

    zip.finish()
        .map_err(|e| archive_err(format!("finish: {e}")))?;
    Ok(())
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
                mode: ConsolidateMode::Collect,
                looks: Vec::new(),
                fonts: Vec::new(),
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
                mode: ConsolidateMode::Collect,
                looks: Vec::new(),
                fonts: Vec::new(),
            },
            None,
        )
        .expect_err("should fail when source is missing");
        assert!(matches!(err, ConsolidateError::SourceNotFound(_)));
    }

    // ── execute_zip_folder ──────────────────────────────────────────

    /// Create a test folder with a nested file structure and return its root.
    fn make_sample_folder(name: &str) -> PathBuf {
        let root = tmp_path(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("project.json"), b"{\"schema\":\"test\"}").unwrap();
        std::fs::write(root.join("media").join("clip.mov"), b"\0FFMPEGBYTES\0").unwrap();
        root
    }

    #[test]
    fn zip_folder_creates_valid_zip() {
        let src = make_sample_folder("zip_valid_src");
        let zip_path = tmp_path("zip_valid.zip");
        let _ = std::fs::remove_file(&zip_path);

        execute_zip_folder(&src, &zip_path, None).expect("archive should succeed");

        assert!(zip_path.exists(), "zip file must exist after success");

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).expect("valid zip");
        assert_eq!(archive.len(), 2);

        let mut names: Vec<String> =
            (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["media/clip.mov", "project.json"]);

        // Verify contents round-trip byte-for-byte.
        use std::io::Read;
        let mut proj = String::new();
        archive.by_name("project.json").unwrap().read_to_string(&mut proj).unwrap();
        assert_eq!(proj, "{\"schema\":\"test\"}");

        let mut clip_bytes = Vec::new();
        archive.by_name("media/clip.mov").unwrap().read_to_end(&mut clip_bytes).unwrap();
        assert_eq!(clip_bytes, b"\0FFMPEGBYTES\0");

        // Source folder untouched.
        assert!(src.join("project.json").exists());
        assert!(src.join("media").join("clip.mov").exists());

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_file(&zip_path).ok();
    }

    #[test]
    fn zip_folder_deterministic_ordering() {
        let src = make_sample_folder("zip_det_src");
        let zip_a = tmp_path("zip_det_a.zip");
        let zip_b = tmp_path("zip_det_b.zip");
        for p in [&zip_a, &zip_b] {
            let _ = std::fs::remove_file(p);
        }

        execute_zip_folder(&src, &zip_a, None).unwrap();
        execute_zip_folder(&src, &zip_b, None).unwrap();

        let bytes_a = std::fs::read(&zip_a).unwrap();
        let bytes_b = std::fs::read(&zip_b).unwrap();
        // Zip headers carry a last-modified timestamp from the source file's
        // mtime, which is identical across the two runs (same source files),
        // so the archives should be byte-identical when entries are sorted.
        assert_eq!(bytes_a, bytes_b, "archives over identical input must be byte-identical");

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_file(&zip_a).ok();
        std::fs::remove_file(&zip_b).ok();
    }

    #[test]
    fn zip_folder_cleans_up_partial_on_error() {
        // Create a valid source folder, then drop a file that disappears
        // between collect_files and write_zip to provoke a mid-write failure.
        // Simplest reliable setup: include a path that points at a missing
        // file. We do that by creating the file, capturing its path into the
        // list (via a real folder walk), then deleting it before write_zip
        // opens it.
        //
        // The real-world failure modes this guards against are:
        //   - source file deleted by another process mid-archive
        //   - permission denied mid-walk
        //   - disk full mid-write
        // All three produce the same contract: partial zip removed, error
        // returned, source untouched.
        let src = tmp_path("zip_cleanup_src");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).unwrap();
        let victim = src.join("will_vanish.bin");
        std::fs::write(&victim, b"bytes").unwrap();

        let zip_path = tmp_path("zip_cleanup.zip");
        let _ = std::fs::remove_file(&zip_path);

        // Race: delete `victim` right before the archive runs. On a real
        // system the walk and the delete interleave; in this unit test we
        // force the sequence by deleting between the walk (which happens
        // inside execute_zip_folder) and... actually the walk also runs
        // inside execute_zip_folder, so by the time we delete we'd be racing.
        // Deterministic alternative: remove the file AFTER collect_files runs
        // is impossible from outside. Instead we exploit the stat-for-total
        // step: delete the file after create_dir but before calling the fn.
        //
        // Simpler and still exercises the contract: delete the file right
        // now, THEN call execute_zip_folder. collect_files walks the now-
        // emptier folder successfully (the deleted file isn't seen by
        // read_dir), and the archive succeeds with zero files — which is
        // NOT an error. So that doesn't exercise cleanup either.
        //
        // Reliable path: provide an unreadable source. On Unix we can strip
        // read perms from a file so File::open during copy fails.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&victim).unwrap().permissions();
            perms.set_mode(0o000);
            std::fs::set_permissions(&victim, perms).unwrap();

            let err = execute_zip_folder(&src, &zip_path, None)
                .expect_err("unreadable source must fail");
            assert!(
                matches!(err, ConsolidateError::ArchiveFailed { .. }),
                "expected ArchiveFailed, got {err:?}"
            );
            assert!(
                !zip_path.exists(),
                "partial zip must be removed after failure"
            );
            // Source file still present (perms stripped, not deleted) —
            // folder untouched by the archive step.
            assert!(victim.exists(), "source must not be touched on error");

            // Restore perms so tmp-dir cleanup works.
            let mut perms = std::fs::metadata(&victim).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&victim, perms).unwrap();
        }

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_file(&zip_path).ok();
    }
}

#[cfg(test)]
mod collected_grade_file_tests {
    use super::*;

    /// A collected project's own grade file resolves into its folder; a
    /// hostile one climbing out with `..`, one elsewhere in `media/`, an
    /// absolute path and a library name are left as they are.
    #[test]
    fn only_a_plain_path_under_media_grade_resolves() {
        let dir = Path::new("/projects/collected.klipp");
        let mut g = NodeGraph::default();
        for lut in ["media/grade/look.cube", "media/../../etc/passwd", "media/grade/../../../etc/passwd", "media/proxies/x.cube", "/abs/look.cube", "Rec709.cube"] {
            g.nodes.push(klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind: klipp_nodes::NodeKind::Lut3D, label: String::new(), enabled: true, params: klipp_nodes::NodeParams { lut_path: Some(lut.into()), ..Default::default() }, keyframes: Default::default(), output_link: None, input_links: Vec::new() });
        }
        resolve_collected_grade_files(&mut g, dir);
        let got: Vec<String> = g.nodes.iter().filter_map(|n| n.params.lut_path.as_ref()).map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(got, ["/projects/collected.klipp/media/grade/look.cube", "media/../../etc/passwd", "media/grade/../../../etc/passwd", "media/proxies/x.cube", "/abs/look.cube", "Rec709.cube"]);
    }
}
