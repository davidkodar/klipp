use crate::clip_codec::{load_clip, with_takes};
use crate::format::*;
use crate::path_util::{
    resolve_media_path, read_to_string_capped, sanitize_fps, sanitize_media_resolution, sanitize_resolution,
    MAX_INPUT_FILE_BYTES,
};
use klipp_core::*;
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{GradeAccess, Look, Sequence, Track, TrackKind};
use klipp_nodes::{NodeGraph, NodeKind, NodeId};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};
use uuid::Uuid;

/// Migrate old project graphs forward — insert nodes that were added in later versions.
/// Public for the converter's dry run, which decides as this loader leaves a graph.
pub fn migrate_graph(graph: &mut NodeGraph) {
    // Skip the ColorWarper auto-insert when the graph already uses the
    // new Corrector default (or when the user has explicitly removed
    // the legacy primary chain). The ColorWarper migration was added
    // back when every clip carried a 12-node legacy chain — applying
    // it to a Corrector graph would dump a stray Warper node before
    // the Output and pollute every newly-loaded project.
    let has_corrector = graph.nodes.iter().any(|n| n.kind == NodeKind::Corrector);
    let has_legacy_chain = graph.nodes.iter().any(|n|
        matches!(n.kind, NodeKind::Exposure | NodeKind::Contrast | NodeKind::ColorWheels | NodeKind::HueSatCurves)
    );
    if has_corrector || !has_legacy_chain {
        return;
    }

    // ColorWarper: added between HueSatCurves and ColorBalance
    let has_warper = graph.nodes.iter().any(|n| n.kind == NodeKind::ColorWarper);
    if !has_warper {
        let id = NodeId::new_v4();
        let node = klipp_nodes::Node {
            id,
            kind: NodeKind::ColorWarper,
            label: "Color Warper".to_string(),
            enabled: true,
            params: klipp_nodes::NodeParams::default(),
            keyframes: klipp_nodes::NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        // Insert after HueSatCurves in the node list
        if let Some(pos) = graph.nodes.iter().position(|n| n.kind == NodeKind::HueSatCurves) {
            graph.nodes.insert(pos + 1, node);
        } else {
            // Fallback: insert before Output
            if let Some(pos) = graph.nodes.iter().position(|n| n.kind == NodeKind::Output) {
                graph.nodes.insert(pos, node);
            } else {
                graph.nodes.push(node);
            }
        }
        // Wire it into the edge chain: find the edge from HueSat→next and splice in
        if let Some(huesat_id) = graph.nodes.iter().find(|n| n.kind == NodeKind::HueSatCurves).map(|n| n.id) {
            if let Some(edge_idx) = graph.edges.iter().position(|e| e.from_node == huesat_id) {
                let old_target = graph.edges[edge_idx].to_node;
                let old_port = graph.edges[edge_idx].to_port;
                // HueSat → Warper
                graph.edges[edge_idx].to_node = id;
                graph.edges[edge_idx].to_port = 0;
                // Warper → old target
                graph.edges.push(klipp_nodes::Edge {
                    from_node: id,
                    from_port: 0,
                    to_node: old_target,
                    to_port: old_port,
                });
            }
        }
        info!("Migrated graph: added ColorWarper node");
    }
}

/// Result of loading a project: sequences, media pool, color settings,
/// active sequence ID, created_at, free-form project notes, optional
/// timeline zoom (px-per-second), scroll position, and vertical track scale.
/// The view-state fields round-trip through `project.json` so e.g.
/// "Fit Timeline to Window" persists across sessions.
/// Project-scoped synthetic title assets, keyed by their
/// `MediaItem.id`. Empty `HashMap` for projects that pre-date the
/// synthetic-asset feature. The 10th tuple element of `LoadResult`.
pub type LoadedTitleAssets =
    std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset>;

pub type LoadResult = (
    Vec<Sequence>,
    MediaPool,
    crate::save::ColorSettings,
    Option<String>,
    Option<String>,
    String,
    Option<f32>,
    Option<f32>,
    Option<f32>,
    LoadedTitleAssets,
);

/// A clip's (or a track's) built-in effects from the file, each read on its own so the
/// one that fails is the one named: an effect tag from a newer Klipp,
/// or a value that is not an effect at all, refuses the load rather
/// than costing the clip the effects around it.
fn read_builtin_effects(value: &serde_json::Value, whose: &str) -> Result<Vec<klipp_effects::BuiltinEffect>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!("{whose}: its effects are not a list; the project cannot be read as it was saved"));
    };
    items
        .iter()
        .map(|item| {
            // A number past its safe range is held at it, one that isn't
            // a number given its default (#2995), and the file still opens.
            let mut item = item.clone();
            for g in klipp_effects::registry::guard_entry(&mut item, klipp_effects::registry::NotFinite::Default).unwrap_or_default() {
                tracing::warn!("{whose}: effect value {} held at {} (the file had {})", g.key, g.now, g.was);
            }
            serde_json::from_value::<klipp_effects::BuiltinEffect>(item.clone()).map_err(|e| {
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some(tag) => format!(
                        "{whose}: effect \"{tag}\" is not one this Klipp knows; the project was saved by a newer version, open it there ({e})"
                    ),
                    None => format!("{whose}: an effect could not be read: {e}"),
                }
            })
        })
        .collect()
}

/// Each clip's lifted picture as the file holds it, with its grade
/// versions' bodies, by sequence id and clip id (#1089, M1b 2b): a
/// duplicated sequence keeps its clips' ids. A clip whose stored form
/// doesn't read, or whose key appears twice, is left out and lifted again
/// when it's needed.
///
/// A grade's LUT path is read as the clip's fields read it (#1270): the
/// fields resolve a stored name against this machine's LUT library and a
/// lift of them names a bundled LUT `bundled:<name>`, so a picture saved
/// with the old absolute path into some `assets/luts/` is given that name
/// too, or it would never be what the fields give again.
fn stored_pictures(v2: &serde_json::Value) -> HashMap<(String, String), klipp_timeline::LiftedPicture> {
    use klipp_core::assets::{bundled_luts_dir, portable_lut_path, resolve_lut_path};
    let library = bundled_luts_dir();
    let rule = |s: &str| portable_lut_path(&resolve_lut_path(s, library), library);
    let portable = |v: &serde_json::Value| {
        let mut v = v.clone();
        klipp_graph_lift::load_rewrites::portable_lut_paths(&mut v, &rule);
        v
    };
    let mut out = HashMap::new();
    let mut repeated = std::collections::HashSet::new();
    let all = |v: Option<&serde_json::Value>| v.and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    for seq in all(v2.get("sequences")) {
        for track in all(seq.get("tracks")) {
            for clip in all(track.get("clips")) {
                let (Some(id), Some(picture)) = (clip.get("id").and_then(serde_json::Value::as_str), clip.get("graph_v2")) else {
                    continue;
                };
                let Ok(mut picture) = serde_json::from_value(portable(picture)) else {
                    continue;
                };
                // Its effects' values held in their maps, as a lift holds
                // them (M3-2), each entry first as its effect writes it now;
                // a picture that can't be is lifted again.
                if klipp_graph_lift::effect_params::picture_upgraded_entries(&mut picture).is_err() || klipp_graph_lift::effect_params::picture_into_memory(&mut picture).is_err() {
                    continue;
                }
                // A version's mix and mask channel come off its body: the
                // clip's versions keep them as run settings (#1322).
                let versions: Option<Vec<_>> = all(clip.get("grade_versions"))
                    .iter()
                    .map(|v| v.get("graph_v2").map(portable).and_then(|mut b| klipp_graph_lift::sequence_grade_mix::take(&mut b).ok().map(|_| b)))
                    .collect();
                if let Some(versions) = versions {
                    let key = (seq.get("id").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(), id.to_string());
                    if out.insert(key.clone(), klipp_timeline::LiftedPicture { picture, versions }).is_some() {
                        repeated.insert(key);
                    }
                }
            }
        }
    }
    out.retain(|key, _| !repeated.contains(key));
    out
}

/// Keep each clip's picture as its file held it, where it is exactly what
/// lifting the clip as loaded gives; a clip whose stored picture
/// disagrees keeps none and is lifted again at the next save. In a debug
/// build and in tests that disagreement stops the load: it means the
/// first edit of that clip would write something else than it read.
fn adopt_stored_pictures(sequences: &mut [Sequence], mut stored: HashMap<(String, String), klipp_timeline::LiftedPicture>, strict: bool) -> Result<(), String> {
    for seq in sequences.iter_mut() {
        let seq_id = seq.id.to_string();
        for track in &mut seq.tracks {
            let track_name = track.name.clone();
            for clip in &mut track.clips {
                let Some(picture) = stored.remove(&(seq_id.clone(), clip.id.0.to_string())) else {
                    continue;
                };
                if let Err(e) = clip.adopt_stored_picture(picture) {
                    if strict {
                        return Err(format!("clip {} on track \"{track_name}\": {e}", clip.id.0));
                    }
                    warn!("Clip {} on track \"{track_name}\": {e}; it will be lifted again when saved", clip.id.0);
                    debug_assert!(false, "clip {} on track \"{track_name}\": {e}", clip.id.0);
                }
            }
        }
    }
    Ok(())
}

/// The generation a project or manifest file was written with, read
/// without parsing the rest.
fn generation_of(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text).ok()?.get("generation")?.as_str().map(str::to_string)
}

/// Whether a project.json and a manifest.json were written by the same
/// save: the same generation, or both from before generations.
pub(crate) fn pair_is_whole(project: &str, manifest: &str) -> bool {
    generation_of(project) == generation_of(manifest)
}

/// The project.json and manifest.json to open, as text, and whether a
/// save stopped part way (#840, #927). A save copies the pair it replaces
/// into `.recovery/` and leaves a `pending` mark until both new files are
/// written; a mark still there means a save stopped, and the files on disk
/// say how far it got:
///
/// - a pair whose two files are from the same save is whole: the save
///   finished all but the mark, or stopped before its manifest;
/// - files from different saves: it stopped between them, and the
///   recovery copy, the pair from before, is opened.
///
/// The pair is judged by its files, not by the generation in the mark: a
/// retried save rewrites the mark before writing anything else, so a mark
/// that names neither file says nothing about whether they match (#927).
/// When the recovery copy cannot be used, the pair on disk is returned
/// with `interrupted` set, and `mend_manifest` mends it from a backup or
/// refuses it, a project from before generations included.
fn pair_to_open(dir: &Path) -> Result<(String, String, bool), String> {
    let project = read_to_string_capped(&dir.join("project.json"), MAX_INPUT_FILE_BYTES)
        .map_err(|e| format!("Failed to read project.json: {e}"))?;
    let manifest = read_to_string_capped(&dir.join("media").join("manifest.json"), MAX_INPUT_FILE_BYTES)
        .map_err(|e| format!("Failed to read manifest.json: {e}"))?;
    let recovery = dir.join(crate::save::RECOVERY_DIR);
    if !recovery.join("pending").exists() || pair_is_whole(&project, &manifest) {
        return Ok((project, manifest, false));
    }
    let kept = (
        read_to_string_capped(&recovery.join("project.json"), MAX_INPUT_FILE_BYTES),
        read_to_string_capped(&recovery.join("manifest.json"), MAX_INPUT_FILE_BYTES),
    );
    if let (Ok(p), Ok(m)) = kept {
        if pair_is_whole(&p, &m) {
            warn!("the last save stopped between its two files; opening the pair from before it, kept in {}", recovery.display());
            return Ok((p, m, false));
        }
    }
    warn!("the last save stopped between its two files, and its recovery copy in {} is missing or not whole", recovery.display());
    Ok((project, manifest, true))
}

/// The media manifest that belongs with the timeline (#821). A save
/// writes the manifest and then `project.json`, both stamped with the same
/// generation. When the generations still differ (a stop the recovery
/// copy could not cover), the backed-up manifest whose generation is the
/// timeline's is used instead (backups of the two rotate together). Files
/// from before generations are read as they are; a pair from different
/// saves with nothing to mend it is refused rather than opened as if it
/// were whole (#840).
fn mend_manifest(dir: &Path, project_generation: Option<&str>, manifest: MediaManifestJson, interrupted: bool) -> Result<MediaManifestJson, String> {
    let Some(wanted) = project_generation else {
        // A timeline from before generations beside a media list a stopped
        // save wrote: no backup can be matched to it, so it is refused
        // like any other pair from different saves (#927).
        if interrupted && manifest.generation.is_some() {
            return Err(unmendable(dir));
        }
        return Ok(manifest);
    };
    if manifest.generation.as_deref().is_none_or(|g| g == wanted) {
        return Ok(manifest);
    }
    let base = dir.join("media").join("manifest.json");
    for n in 1..=64 {
        let backup = base.with_extension(format!("json.bak.{n}"));
        if !backup.exists() {
            break;
        }
        let Ok(text) = read_to_string_capped(&backup, MAX_INPUT_FILE_BYTES) else { continue };
        let Ok(candidate) = serde_json::from_str::<MediaManifestJson>(&text) else { continue };
        if candidate.generation.as_deref() == Some(wanted) {
            warn!(
                "the media manifest is from a later save than project.json (a save stopped between its two files); using {} instead, which matches the timeline",
                backup.display()
            );
            return Ok(candidate);
        }
    }
    Err(unmendable(dir))
}

/// Why a pair from different saves is not opened, and what to do.
fn unmendable(dir: &Path) -> String {
    format!(
        "project.json and media/manifest.json in {} are from different saves, and neither the recovery copy nor a backup matches the timeline. Opening them together would put clips on the wrong media. Restore a matching pair from the .bak files beside them (project.json.bak.N with media/manifest.json.bak.N), or from a copy of the project.",
        dir.display()
    )
}

/// Why a side store's file (a Look, a stills index, a colour preset) isn't
/// read, by its `format_revision`, or `None` when it is this build's: one
/// from a newer Klipp says so, as a project does (#1135), and one from
/// before revision 2 names the converter.
pub fn side_store_revision_refusal(v: &serde_json::Value) -> Option<String> {
    match v.get("format_revision").and_then(serde_json::Value::as_u64) {
        Some(r) if r == u64::from(crate::SIDE_STORE_REVISION) => None,
        Some(r) if r > u64::from(crate::SIDE_STORE_REVISION) => {
            Some(format!("it was saved by a newer Klipp (format {r}; this Klipp reads up to {})", crate::SIDE_STORE_REVISION))
        }
        _ => Some("it is from before revision 2: convert it with `klipp-convert run`".into()),
    }
}

/// Refuse a project whose `format_revision` is newer than this Klipp
/// reads (#994). Read from the raw JSON, so the refusal does not depend on
/// the rest of the file parsing as this Klipp expects.
fn refuse_newer_format(value: &serde_json::Value) -> Result<(), String> {
    let revision = value.get("format_revision").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if revision > u64::from(crate::FORMAT_REVISION) {
        return Err(format!(
            "This project was saved by a newer Klipp (project format {revision}; this Klipp reads up to {}). Open it in that version.",
            crate::FORMAT_REVISION
        ));
    }
    Ok(())
}

/// Refuse a project from before the current revision (#1089): this Klipp
/// reads only the node-graph format with stored effect ids, and
/// `klipp-convert run` converts every project once, with a backup.
fn refuse_unconverted(value: &serde_json::Value) -> Result<(), String> {
    let revision = value.get("format_revision").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if revision < u64::from(crate::FORMAT_REVISION) {
        return Err(format!(
            "This project is from before project format {} (it is format {revision}). Convert it with `klipp-convert run`, which converts every project once and keeps a backup.",
            crate::FORMAT_REVISION
        ));
    }
    Ok(())
}

/// Refuse a project the one-time conversion (`klipp-convert run`) hasn't
/// finished (#1089): its staging copy or set-aside original is beside it,
/// or its journal entry hasn't reached the last step. Opening it could read
/// either half and save over it; the run finishes or rolls it back from
/// the journal.
fn refuse_half_converted(dir: &Path) -> Result<(), String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let root = klipp_graph_lift::journal::backup_root(&home);
    // The run journals the canonical path (#1135): a relative, linked or
    // trailing-slash path names the same project.
    let dir = &std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    match klipp_graph_lift::journal::unfinished(&root, dir) {
        Some(why) => Err(format!(
            "This project's conversion to the node-graph format isn't finished: {why}. Run `klipp-convert run` again; it finishes or rolls back each project from its journal."
        )),
        None => Ok(()),
    }
}

/// Refuse a project holding grade nodes or settings this Klipp would lose
/// by reading it (#993): a node kind it does not know, or a setting it
/// would drop. Found on the saved JSON, wherever a graph is stored.
fn refuse_graph_losses(value: &serde_json::Value) -> Result<(), String> {
    let losses = klipp_nodes::json_check::losses(value);
    let Some(first) = losses.first() else { return Ok(()) };
    let more = match losses.len() {
        1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    Err(format!(
        "This project has {first}{more}, which this Klipp does not know. It was saved by a newer Klipp: open it there. Opening it here would lose them on the next save."
    ))
}

pub fn load_project(dir: &Path) -> Result<LoadResult, String> {
    load_project_as(dir, crate::FileForm::Revision2, false)
}

/// [`load_project`], refusing a clip whose stored picture its fields don't
/// give again instead of lifting it anew, in a release build as in a debug
/// one: a proof that the file reads back whole (the revision-3 step's).
pub fn load_project_strict(dir: &Path) -> Result<LoadResult, String> {
    load_project_as(dir, crate::FileForm::Revision2, true)
}

/// Today's v1 reader, kept only to prove the one-time conversion (#1089): a
/// v1 file, read exactly as before revision 2, by the same loader without
/// its revision-2 step. Behind `v1-oracle`, removed once the conversion has
/// run and been verified.
#[cfg(feature = "v1-oracle")]
pub fn load_project_v1(dir: &Path) -> Result<LoadResult, String> {
    load_project_as(dir, crate::FileForm::V1, false)
}

fn load_project_as(dir: &Path, form: crate::FileForm, strict: bool) -> Result<LoadResult, String> {
    if matches!(form, crate::FileForm::Revision2) {
        refuse_half_converted(dir)?;
    }
    // The pair a save left whole, from its recovery copy if it stopped (#840).
    let (project_str, manifest_str, interrupted) = pair_to_open(dir)?;

    // Two-step deserialization: handle old singular "sequence" vs new plural "sequences"
    let mut value: serde_json::Value = serde_json::from_str(&project_str)
        .map_err(|e| format!("Failed to parse project.json: {e}"))?;

    if value.get("sequences").is_none() {
        if let Some(seq) = value.get("sequence").cloned() {
            let obj = value.as_object_mut().unwrap();
            obj.insert("sequences".to_string(), serde_json::Value::Array(vec![seq]));
            obj.remove("sequence");
            info!("Migrated old single-sequence project format");
        }
    }

    // Before anything is read as this Klipp understands it: a file that
    // needs a newer reader is refused here, whole, with a sentence that
    // says why, rather than failing somewhere inside with a parse error
    // or, worse, reading with something missing (#994).
    refuse_newer_format(&value)?;
    // Renamed effects, grade nodes and effect settings read as they are
    // called now, and removed ones refused with why (#2888).
    crate::migrate_ids::migrate(&mut value)?;

    // Revision 2 only (#1089): a file from before it is refused, naming the
    // tool that converts it, and a converted one is read back into today's
    // model through `unlift`.
    let mut stored = match form {
        crate::FileForm::Revision2 => stored_pictures(&value),
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => HashMap::new(),
    };
    // A sequence grade's mix and mask channel (#1321), which its v1 graph
    // has no place for, read from its stored body.
    let sequence_blends = match form {
        crate::FileForm::Revision2 => klipp_graph_lift::sequence_grade_mix::read_project(&value).map_err(|e| format!("This project's revision-2 file can't be read: {e}"))?,
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => Vec::new(),
    };
    // A clip grade version's (#1322).
    let version_blends = match form {
        crate::FileForm::Revision2 => klipp_graph_lift::sequence_grade_mix::read_versions(&value).map_err(|e| format!("This project's revision-2 file can't be read: {e}"))?,
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => Vec::new(),
    };
    let value = match form {
        crate::FileForm::Revision2 => {
            refuse_unconverted(&value)?;
            klipp_graph_lift::unlift::project(&value).map_err(|e| format!("This project's revision-2 file can't be read: {e}"))?
        }
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => {
            if value.get("format_revision").and_then(serde_json::Value::as_u64).unwrap_or(0) > 0 {
                return Err("the v1 reader reads only a file from before revision 2".into());
            }
            value
        }
    };

    // A grade node or setting this Klipp does not know is refused by name,
    // as an unknown effect is (#993), before anything is read: read, it
    // would run as a passthrough and the next save would write it away.
    refuse_graph_losses(&value)?;

    let project: ProjectJson = serde_json::from_value(value)
        .map_err(|e| format!("Failed to deserialize project: {e}"))?;

    let manifest: MediaManifestJson = serde_json::from_str(&manifest_str)
        .map_err(|e| format!("Failed to parse manifest.json: {e}"))?;
    let manifest = mend_manifest(dir, project.generation.as_deref(), manifest, interrupted)?;

    // Reconstruct media pool — resolve stored paths (relative or absolute) to absolute
    let mut media_pool = MediaPool::new();
    // Where media may sit in a hidden folder: the project's own, Klipp's.
    let trusted_folders = klipp_media::media_check::trusted_roots(Some(dir));
    let trusted: Vec<&Path> = trusted_folders.iter().map(|p| p.as_path()).collect();
    for item_json in &manifest.items {
        // Older manifests omitted the kind even for reusable title assets.
        // The project-owned asset with the same id is their discriminator.
        let kind = item_json.kind.clone().unwrap_or_else(|| {
            if project.title_assets.iter().any(|asset| asset.id == item_json.id) {
                MediaItemKind::SyntheticTitle
            } else {
                MediaItemKind::File
            }
        });
        let stored_path = Path::new(&item_json.path);
        let mut path = if kind == MediaItemKind::File { resolve_media_path(dir, stored_path) } else { stored_path.to_path_buf() };
        // The project folder copied or moved: the file where it was (#2790).
        if kind == MediaItemKind::File && !path.exists() {
            if let Some(absolute) = item_json.absolute_path.as_deref().map(Path::new).filter(|p| p.is_absolute() && p.exists()) {
                path = absolute.to_path_buf();
            }
        }
        // A file that lists other inputs (a playlist, a concat list) is
        // not media, whatever its name: offline, never handed to ffmpeg.
        let listed = kind == MediaItemKind::File && path.exists() && klipp_media::is_input_list(&path);
        // A project can name any file on the machine: one that is
        // not a picture, sound or still, or one in a hidden folder, stays
        // offline with why, and is never read or collected.
        let refused = (kind == MediaItemKind::File && path.exists() && !listed).then(|| klipp_media::media_check::media_file_check(&path, &trusted).err()).flatten();
        let offline = kind == MediaItemKind::File && (!path.exists() || listed || refused.is_some());
        // The file's own fingerprint where it is there, else as stored, for
        // a relink to tell the same content (#2790).
        let fingerprint = if kind != MediaItemKind::File { None } else if offline { item_json.fingerprint } else { klipp_media::relink::fingerprint(&path) };
        // The file changed while the project was closed: the proxy shows
        // what it held.
        let proxy_of_other_content = item_json.fingerprint.is_some() && !offline && fingerprint != item_json.fingerprint;
        let proxy_path = item_json.proxy_path.as_ref().filter(|_| !proxy_of_other_content).map(|p| resolve_media_path(dir, Path::new(p)));
        if listed {
            warn!("Media file is a playlist or a list of other files, not media; marking offline: {:?}", path);
        } else if let Some(why) = &refused {
            warn!("Media entry {:?} is kept offline: {why}", path);
        } else if offline {
            warn!("Media file not found, marking offline: {:?}", path);
        }
        let mut media_item = MediaItem {
            id: MediaId(parse_uuid(&item_json.id)?),
            path,
            kind,
            filename: item_json.filename.clone(),
            duration: RationalTime { value: item_json.duration_frames, rate: item_json.duration_rate },
            fps: sanitize_fps(item_json.fps.num, item_json.fps.den),
            resolution: sanitize_media_resolution(
                item_json.resolution.width,
                item_json.resolution.height,
            ),
            video_codec: item_json.video_codec.clone(),
            audio_codec: item_json.audio_codec.clone(),
            has_audio: item_json.has_audio,
            file_size_bytes: item_json.file_size_bytes,
            sample_rate: item_json.sample_rate,
            audio_channels: item_json.audio_channels,
            offline,
            is_still: item_json.is_still,
            proxy_path: proxy_path.clone(),
            proxy_ready: item_json.proxy_ready && proxy_path.as_deref().is_some_and(Path::exists),
            default_source_in: item_json.default_source_in.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
            default_source_out: item_json.default_source_out.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
            rating: item_json.rating,
            tags: item_json.tags.clone(),
            user_notes: item_json.user_notes.clone(),
            bin: item_json.bin.clone(),
            bin_id: item_json.bin_id.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::BinId)),
            parent_media_id: item_json.parent_media_id.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::MediaId)),
            added_at: item_json.added_at.unwrap_or(0),
            fingerprint,
            cache_key: item_json.cache_key.clone(),
            file_modified_at: item_json.file_modified_at,
            media_created_at: item_json.media_created_at,
            color_label: klipp_media::ColorLabel::from_u8(item_json.color_label),
            colour_space: klipp_media::ColourSpace::from_u8(item_json.colour_space),
            needs_reprobe: false,
            log_format: item_json.log_format,
            source_rotation: item_json.source_rotation.unwrap_or(0),
            camera_make: item_json.camera_make.clone(),
            camera_model: item_json.camera_model.clone(),
            encoder: item_json.encoder.clone(),
            bit_depth: item_json.bit_depth,
            video_bitrate_kbps: item_json.video_bitrate_kbps,
            audio_bitrate_kbps: item_json.audio_bitrate_kbps,
            color_primaries: item_json.color_primaries.clone(),
            color_transfer: item_json.color_transfer.clone(),
            pixel_format: item_json.pixel_format.clone(),
            detected_log_format: Default::default(),
            log_format_user_set: item_json.log_format_user_set
                || item_json.log_format != klipp_core::LogFormat::default(),
            skim_file: None,
            waveform_data: None,
            audio_cache: None,
            shot: item_json.shot.clone(),
            // Retain the offline snapshot, but re-probe available files:
            // a source may have changed while the project was closed.
            metadata_read: false,
            truncated_from_secs: item_json.truncated_from_secs,
            stream_ends: item_json.stream_ends,
            dolby_vision_profile: item_json.dolby_vision_profile,
            log: item_json.log.clone(),
            synced_audio: item_json.synced_audio.clone(),
            markers: item_json.markers.clone(),
            screen_text: item_json.screen_text.clone(),
            audio_role: item_json.audio_role,
            span: item_json.span,
        };
        // Re-run the log-format heuristic now that the metadata
        // fields are restored, so files saved before #35 (or files
        // whose detected format wasn't serialised) get the right
        // IDT seed on next clip-add. Cheap (no I/O — pure
        // string/bit-depth heuristic) and idempotent.
        media_item.detected_log_format = media_item.detect_log_format();
        if let Some(why) = refused {
            media_pool.refused.insert(media_item.id, why);
        }
        media_pool.add(media_item);
    }

    // Load bins from manifest
    for bin_json in &manifest.bins {
        if let Ok(uuid) = parse_uuid(&bin_json.id) {
            media_pool.bins.push(klipp_media::Bin {
                id: klipp_core::BinId(uuid),
                name: bin_json.name.clone(),
                parent_id: bin_json.parent_id.as_ref()
                    .and_then(|s| parse_uuid(s).ok().map(klipp_core::BinId)),
            });
        }
    }

    media_pool.smart_bins = manifest.smart_bins.clone();
    // Read as someone else's file may write it: checked, the built-in
    // roles always there.
    media_pool.roles = manifest.roles.clone().map(klipp_core::roles::RoleSet::checked).unwrap_or_default();

    // Migration: if items have bin (String) but no bin_id, create bins and assign IDs
    let items_needing_migration: Vec<(klipp_core::MediaId, String)> = media_pool.all()
        .filter(|m| m.bin.is_some() && m.bin_id.is_none())
        .map(|m| (m.id, m.bin.clone().unwrap()))
        .collect();
    for (mid, bin_name) in items_needing_migration {
        let bin_id = media_pool.find_or_create_bin(&bin_name);
        if let Some(item) = media_pool.get_mut(&mid) {
            item.bin_id = Some(bin_id);
        }
    }

    // Refuse invalid vector data — and an effect this reader does not
    // know, by name — before legacy sequence recovery can skip the
    // sequence that carries it and read the rest as if it were whole.
    for sequence in &project.sequences {
        for track in &sequence.tracks {
            if let Some(value) = track.effects.as_ref().and_then(|fx| fx.video.as_ref()) {
                read_builtin_effects(value, &format!("track \"{}\"", track.name))?;
            }
            for clip in &track.clips {
                if let Some(value) = &clip.builtin_effects {
                    read_builtin_effects(value, &format!("clip {}", clip.id))?;
                }
                if let Some(ClipKindJson::Title { title_data: Some(value), .. }) = &clip.kind {
                    // The legacy wrapper is permissive for old text DTOs, but a
                    // path payload must never bypass the path contract.
                    if let Some(objects) = value.get("objects").and_then(serde_json::Value::as_array) {
                        for object in objects {
                            if let Some(path) = object.pointer("/content/Shape/kind/Path") {
                                let path: klipp_timeline::VectorPath = serde_json::from_value(path.clone()).map_err(|e| format!("invalid path: {e}"))?;
                                path.validate()?;
                            }
                        }
                    }
                }
                if let Some(ClipKindJson::VectorTitle { title_data } | ClipKindJson::TitleV2 { title_data }) = &clip.kind {
                    for object in &title_data.objects {
                        if let klipp_timeline::TitleObjectContent::Shape(shape) = &object.content {
                            if let klipp_timeline::ShapeKind::Path(path) = &shape.kind {
                                path.validate()?;
                            }
                        }
                    }
                }
            }
        }
    }

    // Reconstruct all sequences
    // Clips a load step rewrites on purpose (an old title's animation),
    // which keep no stored picture: theirs is lifted again when saved.
    let mut rewritten = std::collections::HashSet::new();
    let mut sequences: Vec<Sequence> = project.sequences.iter()
        .filter_map(|seq_json| load_sequence_from_json(seq_json, dir, &mut rewritten).ok())
        .collect();

    if sequences.is_empty() {
        return Err("Project contains no valid sequences".into());
    }

    // Migrate transitions saved before per-kind defaults were introduced.
    // Any transition whose user_params is the all-default value gets upgraded
    // to default_for_kind, which is a no-op for kinds with no specific defaults
    // and silently fixes BlurDissolve/Spin/etc as their defaults are added.
    let default_params = klipp_core::TransitionUserParams::default();
    for sequence in &mut sequences {
        for track in &mut sequence.tracks {
            // A transition whose clip is gone, which a removal used to
            // leave behind and every save kept (#979): it joins no cut, so
            // it goes, and the project heals on opening.
            let before = track.transitions.len();
            let clips: Vec<ClipId> = track.clips.iter().map(|c| c.id).collect();
            track.transitions.retain(|t| clips.contains(&t.clip_a_id) && clips.contains(&t.clip_b_id));
            if track.transitions.len() < before {
                warn!("dropped {} transition(s) on track {:?} whose clips are gone", before - track.transitions.len(), track.name);
            }
            for transition in &mut track.transitions {
                if transition.user_params == default_params {
                    transition.user_params =
                        klipp_core::spec_for_kind(transition.kind).default_params;
                }
            }
        }
    }

    // Use color settings from the first sequence (global LUT is project-wide)
    let first = &project.sequences[0];
    let color_settings = crate::save::ColorSettings {
        global_lut_path: first.global_lut_path.as_ref().map(|p| {
            resolve_media_path(dir, Path::new(p))
        }),
        global_lut_opacity: first.global_lut_opacity,
        global_lut_enabled: first.global_lut_enabled,
        track_header_width: first.track_header_width,
        auto_generate_filmstrips: first.auto_generate_filmstrips,
        auto_generate_skim: first.auto_generate_skim,
        // ProjectJson.working_colorspace is project-level (not per-
        // sequence). Old project.json files lack the field; serde
        // gave us Rec709Linear via #[serde(default)].
        working_colorspace: project.working_colorspace,
        grade_encoding: project.grade_encoding,
        default_odt: project.default_odt,
        default_input_format: project.default_input_format,
        default_input_lut: project.default_input_lut.as_ref().map(|p| resolve_media_path(dir, Path::new(p))),
        viewing_lut: project.viewing_lut.as_ref().map(|p| resolve_media_path(dir, Path::new(p))),
        scopes_after_viewing_lut: project.scopes_after_viewing_lut,
    };

    // Decode the synthetic title-asset list into a MediaId-keyed
    // map. Projects saved before the asset feature shipped have an
    // empty (or absent — defaulted) list, which becomes an empty
    // map. Bad UUIDs / malformed `data` blobs are skipped with a
    // warning so a partly-corrupted assets section doesn't refuse
    // the whole project.
    let mut title_assets: std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset> =
        std::collections::HashMap::new();
    for entry in &project.title_assets {
        let id = match parse_uuid(&entry.id) {
            Ok(u) => klipp_core::MediaId(u),
            Err(e) => {
                tracing::warn!("title_asset: skipping entry with bad id {:?}: {}", entry.id, e);
                continue;
            }
        };
        let mut data: klipp_timeline::TitleData =
            match serde_json::from_value(entry.data.clone()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        "title_asset {:?}: skipping (bad data field): {}",
                        entry.id, e
                    );
                    continue;
                }
            };
        // An asset is a title like any other: its layers' stacks are
        // made explicit and the old text-box fields hoisted, once.
        let fx = data.migrate_fx();
        if fx.dropped_legacy > 0 {
            tracing::warn!("title_asset {:?}: {} text box(es) had old effect fields beside a layer stack; the stack wins", entry.id, fx.dropped_legacy);
        }
        title_assets.insert(
            id,
            klipp_timeline::TitleAsset {
                display_name: entry.display_name.clone(),
                data,
                animation: entry.animation.clone(),
            },
        );
    }

    stored.retain(|(_, id), _| !rewritten.contains(id));
    adopt_stored_pictures(&mut sequences, stored, strict)?;
    for (sid, cid, n, blends) in version_blends {
        let clip = sequences.iter_mut().filter(|s| s.id.0 == sid).flat_map(|s| s.all_clips_mut()).find(|c| c.id.0 == cid).ok_or_else(|| format!("This project's clip {cid} isn't read, so its grade version's mix can't be kept"))?;
        clip.set_version_blends(n, &blends).map_err(|e| format!("This project's clip {cid} can't keep its grade version's mix: {e}"))?;
    }
    for (sid, root, blends) in sequence_blends {
        let seq = sequences.iter_mut().find(|s| s.id.0 == sid).ok_or_else(|| format!("This project's sequence {sid} isn't read, so its grade's mix can't be kept"))?;
        seq.set_root_blends(root, &blends).map_err(|e| format!("This project's sequence {sid} can't keep its grade's mix: {e}"))?;
    }

    // A collected project's grade files, stored relative to it.
    for seq in &mut sequences {
        if seq.grades().into_iter().any(crate::consolidate::has_collected_grade_files) {
            seq.edit_each_grade(|g| crate::consolidate::resolve_collected_grade_files(g, dir));
        }
    }

    // A collected project's analysis results, back in the cache under
    // the keys its files have here (#3241).
    let files: Vec<&Path> = media_pool.all().filter(|i| i.kind == MediaItemKind::File && !i.offline).map(|i| i.path.as_path()).collect();
    crate::collect_analysis::bring_in(dir, &klipp_media::disk_cache::analysis_dir(), &files);

    info!(
        "Project loaded from {:?} ({} sequence(s), {} title asset(s))",
        dir,
        sequences.len(),
        title_assets.len(),
    );
    Ok((
        sequences,
        media_pool,
        color_settings,
        project.active_sequence_id,
        project.created_at,
        project.notes,
        project.timeline_zoom,
        project.timeline_scroll_x,
        project.timeline_track_scale,
        title_assets,
    ))
}

/// A sequence from its record, adding to `rewritten` the id of each clip
/// whose picture a load step changed from what the file holds.
fn load_sequence_from_json(
    seq_json: &SequenceJson,
    project_dir: &Path,
    rewritten: &mut std::collections::HashSet<String>,
) -> Result<Sequence, String> {
    // Refuse an effect this reader does not know, by name, before any
    // clip is built. The effects are stored as one tagged array; read as
    // a whole and swallowed, one unknown tag — a project written by a
    // newer Klipp with an effect appended since — cost the clip every
    // effect it had, silently, and the next save wrote it back without
    // them (#222).
    let mut effects_by_clip: HashMap<&str, Vec<klipp_effects::BuiltinEffect>> = HashMap::new();
    for clip in seq_json.tracks.iter().flat_map(|t| t.clips.iter()).flat_map(with_takes) {
        if let Some(value) = &clip.builtin_effects {
            effects_by_clip.insert(clip.id.as_str(), read_builtin_effects(value, &format!("clip {}", clip.id))?);
        }
    }
    let mut sequence = Sequence {
        timeline_graph: seq_json.timeline_graph.clone().map(Into::into),
        grade_groups: seq_json.grade_groups.clone(),
        id: SequenceId(parse_uuid(&seq_json.id)?),
        name: seq_json.name.clone(),
        fps: sanitize_fps(seq_json.fps.num, seq_json.fps.den),
        resolution: sanitize_resolution(
            seq_json.resolution.width,
            seq_json.resolution.height,
        ),
        first_clip_autoset: seq_json.first_clip_autoset,
        tracks: seq_json.tracks.iter().map(|t| {
            let kind = match t.kind.as_str() {
                "Audio" => TrackKind::Audio,
                "Text" => TrackKind::Text,
                "Subtitle" => TrackKind::Subtitle,
                _ => TrackKind::Video,
            };
            let mut track = Track::new(&t.name, kind);
            track.id = TrackId(parse_uuid(&t.id).unwrap_or_else(|_| Uuid::new_v4()));
            track.opacity = t.opacity;
            track.volume = t.volume;
            track.muted = t.muted;
            track.solo = t.solo;
            track.visible = t.visible;
            track.pan = t.pan;
            track.locked = t.locked;
            track.frozen = t.frozen;
            track.lock_while_frozen = t.lock_while_frozen;
            track.sync_lock = t.sync_lock;
            track.targeted = t.targeted;
            track.source_patch = t.source_patch;
            track.caption_style = t.caption_style.clone();
            track.language = t.language.clone();
            track.role = t.role.clone();
            track.source_file = t.source_file.as_ref().map(|p| resolve_media_path(project_dir, Path::new(p)));
            track.custom_height = t.custom_height;
            track.colour_label = t.colour_label;
            track.channel_waveforms = t.channel_waveforms;
            if let Some(role) = &t.magnetic_role {
                track.magnetic_role = role.clone();
            }
            track.storyline_anchor = t.storyline_anchor.clone();
            track.automation_lane_visible = t.automation_lane_visible.as_deref().and_then(|s| {
                match s {
                    "Volume" => Some(klipp_core::AudioParam::Volume),
                    "Pan" => Some(klipp_core::AudioParam::Pan),
                    "Gain" => Some(klipp_core::AudioParam::Gain),
                    "TrackVolume" => Some(klipp_core::AudioParam::Volume),
                    "TrackPan" => Some(klipp_core::AudioParam::Pan),
                    _ => None,
                }
            });
            track.automation_lane_track = matches!(t.automation_lane_visible.as_deref(), Some("TrackVolume" | "TrackPan"));
            track.automation_lane_height = t.automation_lane_height;
            track.automation = t.automation.clone().unwrap_or_default();
            track.sends = t.sends.clone();
            if let Some(fx) = &t.effects {
                // Read once already, before anything was loaded: an effect
                // this Klipp does not know refused the load there.
                track.effects = klipp_timeline::TrackEffects {
                    video_effect_list: klipp_timeline::TrackEffectList::new(
                        fx.video.as_ref().and_then(|v| read_builtin_effects(v, &format!("track \"{}\"", t.name)).ok()).unwrap_or_default(),
                        fx.video_enabled.clone(),
                    ),
                    grade: fx.grade.clone().map(|mut g| {
                        migrate_graph(&mut g);
                        g.into()
                    }),
                    audio: fx.audio.clone(),
                };
            }
            track.transitions = t.transitions.iter().filter_map(|tr| {
                let id = parse_uuid(&tr.id).ok().map(TransitionId)?;
                let kind = klipp_core::spec_for_string_key(tr.kind.as_str())
                    .map(|s| s.kind)
                    .unwrap_or(klipp_timeline::TransitionKind::CrossDissolve);
                Some(klipp_timeline::Transition {
                    id,
                    kind,
                    duration: RationalTime { value: tr.duration.value, rate: tr.duration.rate },
                    clip_a_id: ClipId(parse_uuid(&tr.clip_a_id).ok()?),
                    clip_b_id: ClipId(parse_uuid(&tr.clip_b_id).ok()?),
                    user_params: tr.user_params,
                    alignment: tr.alignment,
                })
            }).collect();
            track.clips = t.clips.iter().map(|c| load_clip(c, project_dir, &effects_by_clip, rewritten)).collect();
            // A clip the file got wrong (by hand, or damaged) is put right
            // rather than played with a time that has no rate.
            for c in &mut track.clips {
                let changed = klipp_timeline::invariants::repair(c);
                if !changed.is_empty() {
                    tracing::warn!(clip = %c.id.0, "loaded a clip that {}: put right", changed.join(", "));
                }
            }
            track
        }).collect(),
        in_point: seq_json.in_point.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
        out_point: seq_json.out_point.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
        markers: seq_json.markers.iter().filter_map(|m| {
            let id = parse_uuid(&m.id).ok().map(MarkerId)?;
            let color = match m.color.as_str() {
                "Red" => klipp_core::MarkerColor::Red,
                "Orange" => klipp_core::MarkerColor::Orange,
                "Yellow" => klipp_core::MarkerColor::Yellow,
                "Green" => klipp_core::MarkerColor::Green,
                "Purple" => klipp_core::MarkerColor::Purple,
                _ => klipp_core::MarkerColor::Blue,
            };
            let kind = parse_marker_kind(&m.kind);
            Some(klipp_timeline::Marker { review: m.review.clone(),
                id,
                position: RationalTime { value: m.position.value, rate: m.position.rate },
                label: m.label.clone(),
                color,
                duration: m.duration.as_ref().map(|d| RationalTime { value: d.value, rate: d.rate }),
                comment: m.comment.clone(),
                kind,
            })
        }).collect(),
        last_playhead: seq_json.last_playhead.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
        brief: seq_json.brief.clone(),
        edit_plan: seq_json.edit_plan.clone(),
        reviews: seq_json.reviews.clone(),
        covers: klipp_timeline::cover::checked(seq_json.covers.clone()),
        awaiting_refit: seq_json.awaiting_refit,
        version_of: seq_json.version_of,
        camera: seq_json.camera.clone(),
        // Said nothing: take the rate's default, which is drop-frame at
        // 29.97 and 59.94. A project from before this field is not
        // asserting non-drop, it simply predates the question, and
        // reading it as a deliberate choice would leave every NTSC
        // project drifting against the clock.
        drop_frame: seq_json
            .drop_frame
            .unwrap_or_else(|| klipp_timeline::Sequence::default_drop_frame(sanitize_fps(seq_json.fps.num, seq_json.fps.den))),
        // Held inside the range a sequence may ask for, a NaN included.
        title_nits: seq_json
            .title_nits
            .filter(|n| n.is_finite())
            .map_or(klipp_timeline::DEFAULT_TITLE_NITS, |n| {
                n.clamp(*klipp_timeline::TITLE_NITS_RANGE.start(), *klipp_timeline::TITLE_NITS_RANGE.end())
            }),
        // A colour past the six is no label.
        node_labels: seq_json.node_labels.iter().filter(|l| (1..=klipp_timeline::NODE_LABEL_COLOURS).contains(&l.colour)).cloned().collect(),
        // Mended where it is opened, which says what changed.
        buses: seq_json.buses.clone(),
    };
    // A file from before roles were saved carries none: give it the roles
    // a new sequence gets, which is what it had when it was made and what
    // every reload had been silently taking away. A file that says a
    // track is Free is left Free.
    if seq_json.tracks.iter().all(|t| t.magnetic_role.is_none()) {
        if let Some(v) = sequence.tracks.iter_mut().find(|t| t.kind == TrackKind::Video) {
            v.magnetic_role = klipp_timeline::TrackRole::Primary;
        }
        if let Some(a) = sequence.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio) {
            a.magnetic_role = klipp_timeline::TrackRole::Connected;
        }
    }
    Ok(sequence)
}

/// Parse the JSON-side marker-kind tag back into the enum. Unknown
/// values fall through to `None` so a project with a value from a
/// future build (e.g. a new variant we haven't added yet) loads
/// gracefully without losing the rest of the marker.
fn parse_marker_kind(s: &str) -> klipp_core::MarkerKind {
    match s {
        "Chapter" => klipp_core::MarkerKind::Chapter,
        "ToDo" => klipp_core::MarkerKind::ToDo,
        "Comment" => klipp_core::MarkerKind::Comment,
        "BestTake" => klipp_core::MarkerKind::BestTake,
        "Section" => klipp_core::MarkerKind::Section,
        "Review" => klipp_core::MarkerKind::Review,
        _ => klipp_core::MarkerKind::None,
    }
}

pub(crate) fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Read every `<dir>/looks/*.json` and deserialize as a Look. Files
/// that fail to parse are logged and skipped — a single corrupt Look
/// shouldn't block the rest of the project from loading.
/// A Look file as this Klipp reads it (#1089): a revision-2 Look, brought
/// back through `unlift`, and only one this build reads in full (#993). A
/// Look from before revision 2 is refused, naming the converter. The one
/// test both a load and a save's pruning ask, so a Look that isn't loaded
/// is never removed either.
pub(crate) fn read_look(text: &str, form: crate::FileForm) -> Result<Look, String> {
    let mut labels = Vec::new();
    let v1_text = match form {
        crate::FileForm::Revision2 => {
            let mut v2: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
            if let Some(why) = crate::side_store_revision_refusal(&v2) {
                return Err(why);
            }
            // Its node labels, kept beside the grade (#1866); none in a Look
            // saved before they were kept.
            if let Some(kept) = v2.as_object_mut().and_then(|o| o.remove(crate::save::LOOK_NODE_LABELS)) {
                labels = serde_json::from_value(kept).map_err(|e| format!("its node labels: {e}"))?;
            }
            let v1 = klipp_graph_lift::unlift::look(&v2)?;
            serde_json::to_string(&v1).map_err(|e| e.to_string())?
        }
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => text.to_owned(),
    };
    if !klipp_nodes::json_check::reads_in_full(&v1_text) {
        return Err("it was saved by a newer Klipp, or cannot be read".into());
    }
    let mut look = serde_json::from_str::<Look>(&v1_text).map_err(|e| format!("corrupt: {e}"))?;
    if look.node_labels.is_empty() {
        look.node_labels = labels;
    }
    Ok(look)
}

pub fn load_looks(dir: &Path) -> Vec<Look> {
    load_looks_as(dir, crate::FileForm::Revision2)
}

/// Today's v1 Look reader, kept only to prove the one-time conversion
/// (#1089), behind `v1-oracle`.
#[cfg(feature = "v1-oracle")]
pub fn load_looks_v1(dir: &Path) -> Vec<Look> {
    load_looks_as(dir, crate::FileForm::V1)
}

fn load_looks_as(dir: &Path, form: crate::FileForm) -> Vec<Look> {
    let looks_dir = dir.join("looks");
    if !looks_dir.exists() {
        return Vec::new();
    }
    let entries = match std::fs::read_dir(&looks_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read looks dir {:?}: {e}", looks_dir);
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") { continue; }
        // A Look this build can't read in full, or from before revision 2,
        // is not read, and the file is left alone (#993, #1089).
        match read_to_string_capped(&path, MAX_INPUT_FILE_BYTES) {
            Ok(text) => match read_look(&text, form) {
                Ok(mut look) => {
                    if crate::consolidate::has_collected_grade_files(look.grade()) {
                        look.edit_grade(|g| crate::consolidate::resolve_collected_grade_files(g, dir));
                    }
                    out.push(look)
                }
                Err(why) => warn!("Leaving Look {:?} as it is: {why}", path),
            },
            Err(e) => warn!("Failed to read Look {:?}: {e}", path),
        }
    }
    // Stable order — UUIDs sort lexicographically. Avoids the UI list
    // jumping around between loads on the same project.
    out.sort_by_key(|a| a.id);
    out
}

#[cfg(test)]
mod guard_tests {
    /// A file's effect values past their range, or not numbers (#2995):
    /// the project still opens, each held at its range or its default.
    #[test]
    fn a_hostile_effect_value_in_a_file_is_held() {
        let list = serde_json::json!([
            {"type": "gaussian_blur", "radius": 1e30},
            {"type": "gaussian_blur", "radius": null},
            {"type": "gaussian_blur", "radius": -1e30},
        ]);
        let fx = super::read_builtin_effects(&list, "clip").expect("the file opens");
        let hi = klipp_effects::registry::effect_type("gaussian_blur").unwrap().desc.params[0].hard().unwrap().1;
        let radii: Vec<f32> = fx.iter().map(|e| match e {
            klipp_effects::BuiltinEffect::GaussianBlur { radius } => *radius,
            e => panic!("{e:?}"),
        }).collect();
        assert_eq!(radii, [hi, 0.0, 0.0]);
    }
}

