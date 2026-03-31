use crate::format::*;
use crate::path_util::resolve_media_path;
use klipp_core::*;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{
    Clip, ClipKind, FontStyle, Sequence, TextAlignment, TitleBackground, TitleData,
    TitlePosition, TitleShadow, Track, TrackKind,
};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};
use uuid::Uuid;

/// Result of loading a project: sequences, media pool, color settings, active sequence ID, created_at.
pub type LoadResult = (Vec<Sequence>, MediaPool, crate::save::ColorSettings, Option<String>, Option<String>);

pub fn load_project(dir: &Path) -> Result<LoadResult, String> {
    let project_path = dir.join("project.json");
    let manifest_path = dir.join("media").join("manifest.json");

    let project_str = std::fs::read_to_string(&project_path)
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

    let manifest_str = std::fs::read_to_string(&manifest_path)
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
        let media_item = MediaItem {
            id: MediaId(parse_uuid(&item_json.id)?),
            path,
            filename: item_json.filename.clone(),
            duration: RationalTime { value: item_json.duration_frames, rate: item_json.duration_rate },
            fps: Fps::new(item_json.fps.num, item_json.fps.den),
            resolution: Resolution {
                width: item_json.resolution.width,
                height: item_json.resolution.height,
            },
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
            added_at: item_json.added_at.unwrap_or(0),
            fingerprint: None, // recomputed if needed
            file_modified_at: item_json.file_modified_at,
            media_created_at: item_json.media_created_at,
            color_label: klipp_media::ColorLabel::from_u8(item_json.color_label),
            colour_space: klipp_media::ColourSpace::from_u8(item_json.colour_space),
            needs_reprobe: false,
            log_format: Default::default(),
            source_rotation: item_json.source_rotation.unwrap_or(0),
            skim_file: None,
            waveform_data: None,
            audio_cache: None,
        };
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
    let sequences: Vec<Sequence> = project.sequences.iter()
        .filter_map(|seq_json| load_sequence_from_json(seq_json).ok())
        .collect();

    if sequences.is_empty() {
        return Err("Project contains no valid sequences".into());
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
    };

    info!("Project loaded from {:?} ({} sequence(s))", dir, sequences.len());
    Ok((sequences, media_pool, color_settings, project.active_sequence_id, project.created_at))
}

fn load_sequence_from_json(seq_json: &SequenceJson) -> Result<Sequence, String> {
    Ok(Sequence {
        id: SequenceId(parse_uuid(&seq_json.id)?),
        name: seq_json.name.clone(),
        fps: Fps::new(seq_json.fps.num, seq_json.fps.den),
        resolution: Resolution {
            width: seq_json.resolution.width,
            height: seq_json.resolution.height,
        },
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
            track.transitions = t.transitions.iter().filter_map(|tr| {
                let id = parse_uuid(&tr.id).ok().map(TransitionId)?;
                let kind = match tr.kind.as_str() {
                    "CrossDissolve" => klipp_timeline::TransitionKind::CrossDissolve,
                    "WipeLeft" => klipp_timeline::TransitionKind::WipeLeft,
                    "WipeRight" => klipp_timeline::TransitionKind::WipeRight,
                    "WipeUp" => klipp_timeline::TransitionKind::WipeUp,
                    "WipeDown" => klipp_timeline::TransitionKind::WipeDown,
                    "DipToBlack" => klipp_timeline::TransitionKind::DipToBlack,
                    "DipToWhite" => klipp_timeline::TransitionKind::DipToWhite,
                    _ => klipp_timeline::TransitionKind::CrossDissolve,
                };
                Some(klipp_timeline::Transition {
                    id,
                    kind,
                    duration: RationalTime { value: tr.duration.value, rate: tr.duration.rate },
                    clip_a_id: ClipId(parse_uuid(&tr.clip_a_id).ok()?),
                    clip_b_id: ClipId(parse_uuid(&tr.clip_b_id).ok()?),
                })
            }).collect();
            track.clips = t.clips.iter().map(|c| {
                let clip_kind = load_clip_kind(c);
                Clip {
                    id: ClipId(parse_uuid(&c.id).unwrap_or_else(|_| Uuid::new_v4())),
                    kind: clip_kind,
                    timeline_in: RationalTime { value: c.timeline_in, rate: c.rate },
                    timeline_out: RationalTime { value: c.timeline_out, rate: c.rate },
                    source_in: RationalTime { value: c.source_in, rate: c.rate },
                    source_out: RationalTime { value: c.source_out, rate: c.rate },
                    node_graph: c.node_graph.clone().unwrap_or_default(),
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
                    source_rotation: c.source_rotation.unwrap_or(0),
                    enabled: c.enabled.unwrap_or(true),
                    audio_effects: c.audio_effects.clone().unwrap_or_default(),
                    stabilization_data: None,
                    smoothed_trajectory: None,
                }
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
            })
        }).collect(),
        last_playhead: seq_json.last_playhead.as_ref().map(|tc| RationalTime { value: tc.value, rate: tc.rate }),
    })
}

/// Load ClipKind from JSON — backward-compatible with old `media_id`-only format.
fn load_clip_kind(c: &ClipJson) -> ClipKind {
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
                ClipKind::Title(TitleData {
                    text: text.clone(),
                    font_size: *font_size,
                    font_name: font_name.clone(),
                    font_style: fs,
                    color: *color,
                    alignment: align,
                    position: TitlePosition {
                        x: *position_x,
                        y: *position_y,
                    },
                    background: bg,
                    shadow: sh,
                    outline: ol,
                    outlines: ols,
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
                    texture_fill_path: texture_fill_path.clone(),
                    motion_blur: mb,
                    style_runs: sr,
                })
            }
            ClipKindJson::Gap => ClipKind::Gap,
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
