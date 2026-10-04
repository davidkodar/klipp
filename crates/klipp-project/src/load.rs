use crate::format::*;
use crate::path_util::{
    resolve_media_path, read_to_string_capped, sanitize_fps, sanitize_resolution,
    MAX_INPUT_FILE_BYTES,
};
use klipp_core::*;
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{
    Clip, ClipKind, FontStyle, Look, Sequence, TextAlignment, TitleBackground, TitlePosition,
    TitleShadow, Track, TrackKind,
};
use klipp_nodes::{NodeGraph, NodeKind, NodeId};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};
use uuid::Uuid;

/// Migrate old project graphs forward — insert nodes that were added in later versions.
fn migrate_graph(graph: &mut NodeGraph) {
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

pub fn load_project(dir: &Path) -> Result<LoadResult, String> {
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

    let project: ProjectJson = serde_json::from_value(value)
        .map_err(|e| format!("Failed to deserialize project: {e}"))?;

    let manifest: MediaManifestJson = serde_json::from_str(&manifest_str)
        .map_err(|e| format!("Failed to parse manifest.json: {e}"))?;
    let manifest = mend_manifest(dir, project.generation.as_deref(), manifest, interrupted)?;

    // Reconstruct media pool — resolve stored paths (relative or absolute) to absolute
    let mut media_pool = MediaPool::new();
    for item_json in &manifest.items {
        let stored_path = Path::new(&item_json.path);
        let path = resolve_media_path(dir, stored_path);
        let offline = !path.exists();
        if offline {
            warn!("Media file not found, marking offline: {:?}", path);
        }
        let mut media_item = MediaItem {
            id: MediaId(parse_uuid(&item_json.id)?),
            path,
            kind: MediaItemKind::File,
            filename: item_json.filename.clone(),
            duration: RationalTime { value: item_json.duration_frames, rate: item_json.duration_rate },
            fps: sanitize_fps(item_json.fps.num, item_json.fps.den),
            resolution: sanitize_resolution(
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
            proxy_path: item_json.proxy_path.as_ref().map(|p| {
                resolve_media_path(dir, Path::new(p))
            }),
            proxy_ready: item_json.proxy_ready,
            default_source_in: item_json.default_source_in.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
            default_source_out: item_json.default_source_out.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
            rating: item_json.rating,
            tags: item_json.tags.clone(),
            user_notes: item_json.user_notes.clone(),
            bin: item_json.bin.clone(),
            bin_id: item_json.bin_id.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::BinId)),
            parent_media_id: item_json.parent_media_id.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::MediaId)),
            added_at: item_json.added_at.unwrap_or(0),
            fingerprint: None, // recomputed if needed
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
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            span: None,
        };
        // Re-run the log-format heuristic now that the metadata
        // fields are restored, so files saved before #35 (or files
        // whose detected format wasn't serialised) get the right
        // IDT seed on next clip-add. Cheap (no I/O — pure
        // string/bit-depth heuristic) and idempotent.
        media_item.detected_log_format = media_item.detect_log_format();
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
    let mut sequences: Vec<Sequence> = project.sequences.iter()
        .filter_map(|seq_json| load_sequence_from_json(seq_json, dir).ok())
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


fn load_sequence_from_json(seq_json: &SequenceJson, project_dir: &Path) -> Result<Sequence, String> {
    // Refuse an effect this reader does not know, by name, before any
    // clip is built. The effects are stored as one tagged array; read as
    // a whole and swallowed, one unknown tag — a project written by a
    // newer Klipp with an effect appended since — cost the clip every
    // effect it had, silently, and the next save wrote it back without
    // them (#222).
    let mut effects_by_clip: HashMap<&str, Vec<klipp_effects::BuiltinEffect>> = HashMap::new();
    for clip in seq_json.tracks.iter().flat_map(|t| t.clips.iter()) {
        if let Some(value) = &clip.builtin_effects {
            effects_by_clip.insert(clip.id.as_str(), read_builtin_effects(value, &format!("clip {}", clip.id))?);
        }
    }
    let mut sequence = Sequence {
        timeline_graph: seq_json.timeline_graph.clone(),
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
            track.sync_lock = t.sync_lock;
            track.custom_height = t.custom_height;
            if let Some(role) = &t.magnetic_role {
                track.magnetic_role = role.clone();
            }
            track.storyline_anchor = t.storyline_anchor.clone();
            track.automation_lane_visible = t.automation_lane_visible.as_deref().and_then(|s| {
                match s {
                    "Volume" => Some(klipp_core::AudioParam::Volume),
                    "Pan" => Some(klipp_core::AudioParam::Pan),
                    "Gain" => Some(klipp_core::AudioParam::Gain),
                    _ => None,
                }
            });
            track.automation_lane_height = t.automation_lane_height;
            if let Some(fx) = &t.effects {
                // Read once already, before anything was loaded: an effect
                // this Klipp does not know refused the load there.
                track.effects = klipp_timeline::TrackEffects {
                    video: fx.video.as_ref().and_then(|v| read_builtin_effects(v, &format!("track \"{}\"", t.name)).ok()).unwrap_or_default(),
                    video_enabled: fx.video_enabled.clone(),
                    grade: fx.grade.clone().map(|mut g| {
                        migrate_graph(&mut g);
                        g
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
            track.clips = t.clips.iter().map(|c| {
                let clip_kind = load_clip_kind(c, project_dir);
                let mut clip = Clip {
                    id: ClipId(parse_uuid(&c.id).unwrap_or_else(|_| Uuid::new_v4())),
                    kind: clip_kind,
                    timeline_in: RationalTime { value: c.timeline_in, rate: c.rate },
                    timeline_out: RationalTime { value: c.timeline_out, rate: c.rate },
                    source_in: RationalTime { value: c.source_in, rate: c.rate },
                    source_out: RationalTime { value: c.source_out, rate: c.rate },
                    node_graph: {
                        let mut g = c.node_graph.clone().unwrap_or_default();
                        migrate_graph(&mut g);
                        g
                    },
                    builtin_effects: effects_by_clip.get(c.id.as_str()).cloned().unwrap_or_default(),
                    builtin_effects_enabled: c.builtin_effects_enabled.clone(),
                    effects: c.effects.iter().map(|e| {
                        klipp_plugin::AppliedEffect {
                            plugin_name: e.plugin_name.clone(),
                            enabled: e.enabled,
                            param_values: e.param_values.iter().map(|(k, v)| {
                                let val = match v {
                                    OfxValueJson::Double(d) => klipp_plugin::OfxValue::Double(*d),
                                    OfxValueJson::Double2([a, b]) => klipp_plugin::OfxValue::Double2(*a, *b),
                                    OfxValueJson::Double3([a, b, c]) => klipp_plugin::OfxValue::Double3(*a, *b, *c),
                                    OfxValueJson::Int(i) => klipp_plugin::OfxValue::Int(*i),
                                    OfxValueJson::Int2([a, b]) => klipp_plugin::OfxValue::Int2(*a, *b),
                                    OfxValueJson::Int3([a, b, c]) => klipp_plugin::OfxValue::Int3(*a, *b, *c),
                                    OfxValueJson::Bool(b) => klipp_plugin::OfxValue::Bool(*b),
                                    OfxValueJson::Choice(c) => klipp_plugin::OfxValue::Choice(*c),
                                    OfxValueJson::String(s) => klipp_plugin::OfxValue::String(s.clone()),
                                    OfxValueJson::Rgba([r, g, b, a]) => klipp_plugin::OfxValue::Rgba(*r, *g, *b, *a),
                                    OfxValueJson::Rgb([r, g, b]) => klipp_plugin::OfxValue::Rgb(*r, *g, *b),
                                };
                                (k.clone(), val)
                            }).collect::<HashMap<_, _>>(),
                        ..Default::default()
                        }
                    }).collect(),
                    linked_clip_id: c.linked_clip_id.as_ref().and_then(|s| {
                        parse_uuid(s).ok().map(ClipId)
                    }),
                    linked_title_asset_id: c.linked_title_asset_id.as_ref().and_then(|s| {
                        parse_uuid(s).ok().map(klipp_core::MediaId)
                    }),
                    group_id: c.group_id.as_ref().and_then(|s| {
                        parse_uuid(s).ok().map(GroupId)
                    }),
                    grade_group: c.grade_group.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::GradeGroupId)),
                    own_node_graph: None,
                    own_effects: None,
                    magnetic_anchor: c.magnetic_anchor.clone(),
                    gain: c.gain.unwrap_or(1.0),
                    volume: c.volume.unwrap_or(1.0),
                    pan: c.pan.unwrap_or(0.0),
                    fade_in_secs: c.fade_in_secs.unwrap_or(0.0),
                    fade_out_secs: c.fade_out_secs.unwrap_or(0.0),
                    fade_in_curve: c.fade_in_curve.unwrap_or_default(),
                    fade_out_curve: c.fade_out_curve.unwrap_or_default(),
                    channel_config: c.channel_config.unwrap_or_default(),
                    volume_keyframes: c.volume_keyframes.clone().unwrap_or_default(),
                    pan_keyframes: c.pan_keyframes.clone().unwrap_or_default(),
                    gain_keyframes: c.gain_keyframes.clone().unwrap_or_default(),
                    opacity_keyframes: c.opacity_keyframes.clone().unwrap_or_default(),
                    transform_pos_x_keyframes: c.transform_pos_x_keyframes.clone().unwrap_or_default(),
                    transform_pos_y_keyframes: c.transform_pos_y_keyframes.clone().unwrap_or_default(),
                    transform_scale_keyframes: c.transform_scale_keyframes.clone().unwrap_or_default(),
                    transform_rotation_keyframes: c.transform_rotation_keyframes.clone().unwrap_or_default(),
                    transform_tilt_x_keyframes: c.transform_tilt_x_keyframes.clone().unwrap_or_default(),
                    transform_tilt_y_keyframes: c.transform_tilt_y_keyframes.clone().unwrap_or_default(),
                    matte_feather_keyframes: c.matte_feather_keyframes.clone().unwrap_or_default(),
                    matte_choke_keyframes: c.matte_choke_keyframes.clone().unwrap_or_default(),
                    clip_markers: c.clip_markers.as_ref().map(|markers| {
                        markers.iter().map(|m| klipp_timeline::ClipMarker {
                            time_secs: m.time_secs,
                            name: m.name.clone(),
                            color: match m.color.as_str() {
                                "Red" => klipp_core::MarkerColor::Red,
                                "Orange" => klipp_core::MarkerColor::Orange,
                                "Yellow" => klipp_core::MarkerColor::Yellow,
                                "Green" => klipp_core::MarkerColor::Green,
                                "Purple" => klipp_core::MarkerColor::Purple,
                                _ => klipp_core::MarkerColor::Blue,
                            },
                            comment: m.comment.clone(),
                        }).collect()
                    }).unwrap_or_default(),
                    effect_keyframes: c.effect_keyframes.clone().unwrap_or_default(),
                    role: c.role.unwrap_or_default(),
                    param_modifiers: c.param_modifiers.as_ref().map(|m| {
                        m.iter().filter_map(|(k, v)| {
                            klipp_core::ParamId::from_key(k).map(|pid| (pid, v.clone()))
                        }).collect()
                    }).unwrap_or_default(),
                    speed: c.speed.unwrap_or(1.0),
                    speed_keyframes: c.speed_keyframes.clone().unwrap_or_default(),
                    source_rotation: c.source_rotation.unwrap_or(0),
                    enabled: c.enabled.unwrap_or(true),
                    solo: c.solo.unwrap_or(false),
                    blend_mode: c.blend_mode.unwrap_or(klipp_core::BlendMode::Normal),
                    depth: c.depth.filter(|d| d.is_finite() && *d >= 0.0).unwrap_or(1.0),
                    matte_source: c.matte_source.as_ref().and_then(load_matte_binding),
                    matte_stencil: c.matte_stencil.as_ref().map(|s| klipp_timeline::StencilSettings {
                        channel: s.channel,
                        invert: s.invert,
                        feather_px: s.feather_px,
                        choke_px: s.choke_px,
                    }),
                    consumed_as_matte: c.consumed_as_matte.unwrap_or(false),
                    transform_link: c.transform_link.as_ref().and_then(load_transform_link),
                    grade_versions: c.grade_versions.iter().map(|gv| {
                        let mut g = gv.node_graph.clone();
                        migrate_graph(&mut g);
                        klipp_timeline::GradeVersion {
                            name: gv.name.clone(),
                            node_graph: g,
                        }
                    }).collect(),
                    active_grade_version: c.active_grade_version,
                    audio_effects: c.audio_effects.clone().unwrap_or_default(),
                    audio_effect_keyframes: c.audio_effect_keyframes.clone(),
                    multicam_group: c.multicam_group.as_ref().and_then(|s| parse_uuid(s).ok().map(klipp_core::GroupId)),
                    multicam_angle: c.multicam_angle.unwrap_or(0),
                    multicam_cuts: c.multicam_cuts.clone(),
                    multicam_sound: c.multicam_sound.unwrap_or_default(),
                    deflicker_data: None,
                    deflicker_gains: None,
                    stabilization_data: None,
                    smoothed_trajectory: None,
                    track_points: c.track_points.clone(),
                    surfaces: c.surfaces.clone(),
                    surface_pin: c.surface_pin,
                    name: c.name.clone(),
                    colour_label: c.colour_label,
                    precomp: None,
                };
                // Titles saved with the old whole-title or
                // per-character animation get the layer model; the
                // keyframes the old kinds baked onto the clip go
                // with them, since the loops now do the moving.
                if let ClipKind::Title(td) = &mut clip.kind {
                    // The layer stacks first: an object from before they
                    // existed takes its text box's old fields; a text box
                    // with old fields beside an explicit stack loses them
                    // to it, and the log says so.
                    let fx = td.migrate_fx();
                    if fx.dropped_legacy > 0 {
                        warn!("title {}: {} text box(es) had old effect fields beside a layer stack; the stack wins", clip.id, fx.dropped_legacy);
                    }
                    let m = td.migrate_legacy_animation();
                    if m.clear_clip_keyframes {
                        clip.opacity_keyframes.clear();
                        clip.transform_pos_x_keyframes.clear();
                        clip.transform_pos_y_keyframes.clear();
                        clip.transform_scale_keyframes.clear();
                        clip.transform_rotation_keyframes.clear();
                    }
                }
                // Self-heal orphaned mask trackers from older
                // projects that were saved before the inspector's
                // delete-mask button routed through the centralized
                // cleanup. A `TrackPoint` whose `linked_mask` points
                // at a Mask node that no longer exists in any grade
                // version (or at a `mask_index` past the end of the
                // mask stack on whichever Mask node it does match)
                // would otherwise stay attached forever and there's
                // no UI to remove an orphaned tracker because the
                // Untrack button lives inside the missing mask's
                // inspector row. Drop them at load time.
                let mask_counts: std::collections::HashMap<uuid::Uuid, usize> = clip
                    .grade_versions.iter()
                    .flat_map(|gv| gv.node_graph.nodes.iter())
                    .filter(|n| n.kind == klipp_nodes::NodeKind::Mask)
                    .map(|n| {
                        let count = n.params.masks.as_ref()
                            .map(|ms| ms.masks.len())
                            .unwrap_or(0);
                        (n.id, count)
                    })
                    .collect();
                let pre = clip.track_points.len();
                clip.track_points.retain(|tp| match tp.linked_mask {
                    None => true,
                    Some((nid, mi)) => mask_counts.get(&nid).is_some_and(|&n| mi < n),
                });
                let dropped = pre - clip.track_points.len();
                if dropped > 0 {
                    tracing::info!(
                        "load: pruned {dropped} orphaned mask tracker(s) from clip {:?}",
                        clip.id,
                    );
                }
                clip
            }).collect();
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
            Some(klipp_timeline::Marker {
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
        camera: seq_json.camera.clone(),
        // Said nothing: take the rate's default, which is drop-frame at
        // 29.97 and 59.94. A project from before this field is not
        // asserting non-drop, it simply predates the question, and
        // reading it as a deliberate choice would leave every NTSC
        // project drifting against the clock.
        drop_frame: seq_json
            .drop_frame
            .unwrap_or_else(|| klipp_timeline::Sequence::default_drop_frame(sanitize_fps(seq_json.fps.num, seq_json.fps.den))),
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
        _ => klipp_core::MarkerKind::None,
    }
}

fn resolve_title_paths(td: &mut klipp_timeline::TitleData, project_dir: &Path) {
    // Texture paths are stored project-relative, like the flat fields.
    for obj in &mut td.objects {
        match &mut obj.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                if let Some(p) = tb.texture_fill.as_mut().and_then(|t| t.file_path_mut()) {
                    *p = crate::path_util::resolve_media_path(project_dir, Path::new(p))
                        .to_string_lossy().to_string();
                }
            }
            klipp_timeline::TitleObjectContent::Image(img) => {
                img.path = crate::path_util::resolve_media_path(project_dir, Path::new(&img.path))
                    .to_string_lossy().to_string();
            }
            klipp_timeline::TitleObjectContent::Shape(_) => {}
        }
    }
}

/// Load ClipKind from JSON — backward-compatible with old `media_id`-only format.
fn load_clip_kind(c: &ClipJson, project_dir: &Path) -> ClipKind {
    // New format: `kind` field is present
    if let Some(kind_json) = &c.kind {
        return match kind_json {
            ClipKindJson::VectorTitle {title_data} => { let mut td=title_data.clone();resolve_title_paths(&mut td,project_dir);ClipKind::Title(td) },
            ClipKindJson::TitleV2 { title_data } => {
                let mut td = title_data.clone();
                resolve_title_paths(&mut td, project_dir);
                ClipKind::Title(td)
            }
            ClipKindJson::Media { media_id } => {
                let mid = parse_uuid(media_id)
                    .map(MediaId)
                    .unwrap_or_else(|_| MediaId::new());
                ClipKind::Media { media_id: mid }
            }
            ClipKindJson::Title {
                title_data,
                text,
                font_size,
                font_name,
                font_style,
                color,
                alignment,
                position_x,
                position_y,
                background,
                shadow,
                shadows,
                shadow_stack_enabled,
                outline,
                outlines,
                outline_stack_enabled,
                glow,
                glows,
                glow_stack_enabled,
                gradient,
                inner_shadow,
                inner_shadows,
                inner_shadow_stack_enabled,
                pattern_fill,
                tracking,
                line_spacing,
                write_on,
                extrusion,
                text_path,
                char_animation,
                texture_fill_path,
                texture_fill,
                motion_blur,
                style_runs,
            } => {
                if let Some(v) = title_data {
                    match serde_json::from_value::<klipp_timeline::TitleData>(v.clone()) {
                        Ok(mut td) => {
                            resolve_title_paths(&mut td, project_dir);
                            return ClipKind::Title(td);
                        }
                        Err(e) => warn!("title_data could not be read, using the flat title fields: {e}"),
                    }
                }
                let align = match alignment.as_str() {
                    "Left" => TextAlignment::Left,
                    "Right" => TextAlignment::Right,
                    _ => TextAlignment::Center,
                };
                let fs = match font_style.as_str() {
                    "Bold" => FontStyle::Bold,
                    "Italic" => FontStyle::Italic,
                    "BoldItalic" => FontStyle::BoldItalic,
                    _ => FontStyle::Regular,
                };
                let bg: TitleBackground = serde_json::from_value(background.clone())
                    .unwrap_or(TitleBackground::None);
                // Drop-shadow stack migration: prefer the new
                // `shadows` array; fall back to wrapping the legacy
                // single `shadow` field as a one-element vec when
                // `shadows` is absent or empty.
                let shadow_vec: Vec<TitleShadow> = {
                    let from_new: Vec<TitleShadow> =
                        serde_json::from_value(shadows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<TitleShadow> =
                            serde_json::from_value(shadow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let ol: Option<klipp_timeline::TextOutline> = serde_json::from_value(outline.clone()).ok().flatten();
                let ols: Vec<klipp_timeline::TextOutline> = serde_json::from_value(outlines.clone()).unwrap_or_default();
                // Glow stack migration: prefer `glows` array; fall
                // back to wrapping legacy `glow` as a one-element vec.
                let glow_vec: Vec<klipp_timeline::TextGlow> = {
                    let from_new: Vec<klipp_timeline::TextGlow> =
                        serde_json::from_value(glows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<klipp_timeline::TextGlow> =
                            serde_json::from_value(glow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let gr: Option<klipp_timeline::TextGradient> = serde_json::from_value(gradient.clone()).ok().flatten();
                // Inner shadow stack migration: prefer `inner_shadows`
                // array; fall back to wrapping legacy `inner_shadow`.
                let inner_shadow_vec: Vec<klipp_timeline::InnerShadow> = {
                    let from_new: Vec<klipp_timeline::InnerShadow> =
                        serde_json::from_value(inner_shadows.clone()).unwrap_or_default();
                    if !from_new.is_empty() {
                        from_new
                    } else {
                        let legacy: Option<klipp_timeline::InnerShadow> =
                            serde_json::from_value(inner_shadow.clone()).ok().flatten();
                        legacy.into_iter().collect()
                    }
                };
                let pf: Option<klipp_timeline::TextPattern> = serde_json::from_value(pattern_fill.clone()).ok().flatten();
                let ext: Option<klipp_timeline::TextExtrusion> = serde_json::from_value(extrusion.clone()).ok().flatten();
                let tp: Option<klipp_timeline::TextPath> = serde_json::from_value(text_path.clone()).ok().flatten();
                let ca: Option<klipp_timeline::CharAnimation> = serde_json::from_value(char_animation.clone()).ok().flatten();
                let mb: Option<klipp_timeline::MotionBlur> = serde_json::from_value(motion_blur.clone()).ok().flatten();
                let sr: Vec<klipp_timeline::StyleRun> = serde_json::from_value(style_runs.clone()).unwrap_or_default();
                // Build a TextBox from the legacy flat JSON fields,
                // then wrap it in a TitleData with one TextBox object.
                // The legacy single-outline `outline` field is folded
                // into the `outlines` vec (rendered widest-first).
                let mut outlines_vec = ols;
                if let Some(o) = ol {
                    if !outlines_vec.iter().any(|existing| existing == &o) {
                        outlines_vec.insert(0, o);
                    }
                }
                // The flat fields are the old text-box shape; they go in
                // as that shape and the object takes them when the text
                // box is wrapped (`single_text` → `add_object`).
                let tb = klipp_timeline::TextBox {
                    text: text.clone(),
                    font_size: *font_size,
                    font_name: font_name.clone(),
                    font_style: fs,
                    color: *color,
                    alignment: align,
                    legacy_fx: klipp_timeline::LegacyTextFx {
                        shadows: shadow_vec,
                        shadow_stack_enabled: *shadow_stack_enabled,
                        outlines: outlines_vec,
                        outline_stack_enabled: *outline_stack_enabled,
                        glows: glow_vec,
                        glow_stack_enabled: *glow_stack_enabled,
                        inner_shadows: inner_shadow_vec,
                        inner_shadow_stack_enabled: *inner_shadow_stack_enabled,
                        motion: Default::default(),
                        loops: Vec::new(),
                        motion_blur: mb,
                    },
                    gradient: gr,
                    pattern_fill: pf,
                    pattern_fill_enabled: true,
                    tracking: *tracking,
                    line_spacing: *line_spacing,
                    write_on: *write_on,
                    extrusion: ext,
                    text_path: tp,
                    char_animation: ca,
                    // Prefer the new TextureFill struct when present
                    // (post-migration saves). Fall back to the legacy
                    // bare path for old projects, synthesizing a
                    // TextureFill with default transform/blend params.
                    // The path inside the new struct is also resolved
                    // through resolve_media_path so a project that
                    // moved still finds its texture file.
                    texture_fill: {
                        let from_new: Option<klipp_timeline::TextureFill> =
                            serde_json::from_value(texture_fill.clone()).ok().flatten();
                        if let Some(mut tf) = from_new {
                            if let Some(p) = tf.file_path_mut() {
                                *p = crate::path_util::resolve_media_path(project_dir, Path::new(&p))
                                    .to_string_lossy()
                                    .to_string();
                            }
                            Some(tf)
                        } else {
                            texture_fill_path.as_ref().map(|p| {
                                let resolved = crate::path_util::resolve_media_path(project_dir, Path::new(p))
                                    .to_string_lossy()
                                    .to_string();
                                klipp_timeline::TextureFill::from_legacy_path(resolved, true)
                            })
                        }
                    },
                    style_runs: sr,
                    ass_style_name: None,
                    underline: false,
                    strikethrough: false,
                    case_transform: klipp_timeline::TextCase::None,
                    baseline_shift: 0.0,
                    drop_cap_scale: 0.0,
                    outline_write_on: 1.0,
                    chromatic_aberration_px: 0.0,
                    glitch_intensity: 0.0,
                    columns: 1,
                    vertical_layout: false,
                    text_direction: klipp_timeline::TextDirection::Ltr,
                };
                let mut td = klipp_timeline::TitleData::single_text(tb);
                if let Some(obj) = td.objects.first_mut() {
                    obj.transform.center = TitlePosition {
                        x: *position_x,
                        y: *position_y,
                    };
                }
                td.background = bg;
                ClipKind::Title(td)
            }
            ClipKindJson::Gap => ClipKind::Gap,
            ClipKindJson::Sequence { sequence_id } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::Sequence { sequence_id: sid }
            }
            ClipKindJson::Generator { data } => {
                let gen_data = serde_json::from_value(data.clone())
                    .unwrap_or_default();
                ClipKind::Generator(gen_data)
            }
            ClipKindJson::AdjustmentLayer => ClipKind::AdjustmentLayer,
            ClipKindJson::Precomposition { sequence_id } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::Precomposition { sequence_id: sid }
            }
            ClipKindJson::TemplatePrecomposition { sequence_id, template_instance } => {
                let sid = parse_uuid(sequence_id)
                    .map(klipp_core::SequenceId)
                    .unwrap_or_else(|_| klipp_core::SequenceId::new());
                ClipKind::TemplatePrecomposition { sequence_id: sid, template_instance: template_instance.clone() }
            }
        };
    }

    // Old format: `media_id` field only (Phase 6/7 backward compat)
    if let Some(media_id_str) = &c.media_id {
        let mid = parse_uuid(media_id_str)
            .map(MediaId)
            .unwrap_or_else(|_| MediaId::new());
        return ClipKind::Media { media_id: mid };
    }

    // Fallback — should not happen
    warn!("Clip {} has neither 'kind' nor 'media_id' — using default", c.id);
    ClipKind::default()
}

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Decode a wire-format `MatteBindingJson` into a runtime `MatteBinding`.
/// Returns `None` if the source-clip UUID is malformed — the consumer clip
/// then loads as if it had no matte (the alternative would be to drop the
/// whole project load, which is far worse than silently losing one binding
/// that the user can re-attach in the inspector). The orphan-clip-id case
/// (well-formed UUID that doesn't match any clip) is allowed through here
/// and cleaned up by the post-load pass that runs across the whole sequence.
fn load_matte_binding(j: &crate::format::MatteBindingJson) -> Option<klipp_timeline::MatteBinding> {
    let id = parse_uuid(&j.source_clip_id).ok()?;
    Some(klipp_timeline::MatteBinding {
        source_clip_id: ClipId(id),
        channel: j.channel,
        invert: j.invert,
        feather_px: j.feather_px,
        choke_px: j.choke_px,
    })
}

/// Resolve a `TransformLinkJson` back to its in-memory form. Same
/// dangling-id policy as `load_matte_binding`: a well-formed-but-
/// missing target survives this function and gets cleaned up by
/// the post-load orphan sweep. The malformed-uuid path is now
/// handled by `TransformLinkTarget`'s own deserialization (a
/// non-uuid string fails the `Clip(ClipId)` arm of the untagged
/// enum and the file load surfaces the error there).
fn load_transform_link(j: &crate::format::TransformLinkJson) -> Option<klipp_timeline::TransformLink> {
    Some(klipp_timeline::TransformLink {
        target: j.target,
        mode: j.mode,
        inherit_scale: j.inherit_scale,
        inherit_position: j.inherit_position,
        inherit_rotation: j.inherit_rotation,
        inherit_anchor: j.inherit_anchor,
    })
}

/// Read every `<dir>/looks/*.json` and deserialize as a Look. Files
/// that fail to parse are logged and skipped — a single corrupt Look
/// shouldn't block the rest of the project from loading.
pub fn load_looks(dir: &Path) -> Vec<Look> {
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
        match read_to_string_capped(&path, MAX_INPUT_FILE_BYTES) {
            Ok(text) => match serde_json::from_str::<Look>(&text) {
                Ok(look) => out.push(look),
                Err(e) => warn!("Skipping corrupt Look {:?}: {e}", path),
            },
            Err(e) => warn!("Failed to read Look {:?}: {e}", path),
        }
    }
    // Stable order — UUIDs sort lexicographically. Avoids the UI list
    // jumping around between loads on the same project.
    out.sort_by_key(|a| a.id);
    out
}
