use crate::format::*;
use crate::path_util::{
    resolve_media_path, read_to_string_capped, sanitize_fps, sanitize_resolution,
    MAX_INPUT_FILE_BYTES,
};
use klipp_core::*;
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{
    Clip, ClipKind, FontStyle, Look, Sequence, TextAlignment, TitleBackground, TitleData,
    TitlePosition, TitleShadow, Track, TrackKind,
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
/// timeline zoom (px-per-second) and optional timeline scroll position.
/// The two view-state fields round-trip through `project.json` so e.g.
/// "Fit Timeline to Window" persists across sessions.
/// Project-scoped synthetic title assets, keyed by their
/// `MediaItem.id`. Empty `HashMap` for projects that pre-date the
/// synthetic-asset feature. The 9th tuple element of `LoadResult`.
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
    LoadedTitleAssets,
);

pub fn load_project(dir: &Path) -> Result<LoadResult, String> {
    let project_path = dir.join("project.json");
    let manifest_path = dir.join("media").join("manifest.json");

    let project_str = read_to_string_capped(&project_path, MAX_INPUT_FILE_BYTES)
        .map_err(|e| format!("Failed to read project.json: {e}"))?;

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

    let manifest_str = read_to_string_capped(&manifest_path, MAX_INPUT_FILE_BYTES)
        .map_err(|e| format!("Failed to read manifest.json: {e}"))?;
    let manifest: MediaManifestJson = serde_json::from_str(&manifest_str)
        .map_err(|e| format!("Failed to parse manifest.json: {e}"))?;

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
            skim_file: None,
            waveform_data: None,
            audio_cache: None,
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
        let data: klipp_timeline::TitleData =
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
        title_assets,
    ))
}


fn load_sequence_from_json(seq_json: &SequenceJson, project_dir: &Path) -> Result<Sequence, String> {
    Ok(Sequence {
        id: SequenceId(parse_uuid(&seq_json.id)?),
        name: seq_json.name.clone(),
        fps: sanitize_fps(seq_json.fps.num, seq_json.fps.den),
        resolution: sanitize_resolution(
            seq_json.resolution.width,
            seq_json.resolution.height,
        ),
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
            track.custom_height = t.custom_height;
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
                    user_params: Default::default(),
                    alignment: Default::default(),
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
                    builtin_effects: c.builtin_effects.as_ref()
                        .and_then(|v| serde_json::from_value::<Vec<klipp_effects::BuiltinEffect>>(v.clone()).ok())
                        .unwrap_or_default(),
                    builtin_effects_enabled: Vec::new(),
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
                    magnetic_anchor: None,
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
                    stabilization_data: None,
                    smoothed_trajectory: None,
                    track_points: c.track_points.clone(),
                };
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
                    Some((nid, mi)) => mask_counts.get(&nid).map_or(false, |&n| mi < n),
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
            Some(klipp_timeline::Marker {
                id,
                position: RationalTime { value: m.position.value, rate: m.position.rate },
                label: m.label.clone(),
                color,
                duration: m.duration.as_ref().map(|d| RationalTime { value: d.value, rate: d.rate }),
            })
        }).collect(),
        last_playhead: seq_json.last_playhead.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
    })
}

/// Load ClipKind from JSON — backward-compatible with old `media_id`-only format.
fn load_clip_kind(c: &ClipJson, project_dir: &Path) -> ClipKind {
    // New format: `kind` field is present
    if let Some(kind_json) = &c.kind {
        return match kind_json {
            ClipKindJson::Media { media_id } => {
                let mid = parse_uuid(media_id)
                    .map(MediaId)
                    .unwrap_or_else(|_| MediaId::new());
                ClipKind::Media { media_id: mid }
            }
            ClipKindJson::Title {
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
                outline,
                outlines,
                glow,
                gradient,
                inner_shadow,
                pattern_fill,
                tracking,
                line_spacing,
                write_on,
                extrusion,
                text_path,
                char_animation,
                texture_fill_path,
                motion_blur,
                style_runs,
            } => {
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
                let sh: Option<TitleShadow> = serde_json::from_value(shadow.clone()).ok().flatten();
                let ol: Option<klipp_timeline::TextOutline> = serde_json::from_value(outline.clone()).ok().flatten();
                let ols: Vec<klipp_timeline::TextOutline> = serde_json::from_value(outlines.clone()).unwrap_or_default();
                let gl: Option<klipp_timeline::TextGlow> = serde_json::from_value(glow.clone()).ok().flatten();
                let gr: Option<klipp_timeline::TextGradient> = serde_json::from_value(gradient.clone()).ok().flatten();
                let is: Option<klipp_timeline::InnerShadow> = serde_json::from_value(inner_shadow.clone()).ok().flatten();
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
                let tb = klipp_timeline::TextBox {
                    text: text.clone(),
                    font_size: *font_size,
                    font_name: font_name.clone(),
                    font_style: fs,
                    color: *color,
                    alignment: align,
                    shadow: sh,
                    outlines: outlines_vec,
                    glow: gl,
                    gradient: gr,
                    inner_shadow: is,
                    pattern_fill: pf,
                    tracking: *tracking,
                    line_spacing: *line_spacing,
                    write_on: *write_on,
                    extrusion: ext,
                    text_path: tp,
                    char_animation: ca,
                    texture_fill_path: texture_fill_path.as_ref().map(|p| {
                        crate::path_util::resolve_media_path(project_dir, Path::new(p))
                            .to_string_lossy()
                            .to_string()
                    }),
                    motion_blur: mb,
                    style_runs: sr,
                    ass_style_name: None,
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
        if path.extension().map_or(true, |e| e != "json") { continue; }
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
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}
