use crate::format::*;
use crate::path_util::{atomic_write, to_project_relative};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{ClipKind, Look, Sequence, TrackKind};
use std::path::Path;
use tracing::{info, warn};

/// Project-level settings persisted alongside sequence data.
#[derive(Debug, Clone)]
pub struct ColorSettings {
    pub global_lut_path: Option<std::path::PathBuf>,
    pub global_lut_opacity: f32,
    pub global_lut_enabled: bool,
    pub track_header_width: f32,
    /// Per-project: generate filmstrip thumbnails on import. None = use global pref.
    pub auto_generate_filmstrips: Option<bool>,
    /// Per-project: generate skim transcodes on import. None = use global pref.
    pub auto_generate_skim: Option<bool>,
    /// Working colorspace the grade chain operates in. Defaults to
    /// `Rec709Linear` for back-compat (every existing project file
    /// has no `working_colorspace` field; serde fills the default).
    pub working_colorspace: klipp_core::WorkingColorspace,
}

impl Default for ColorSettings {
    fn default() -> Self {
        Self {
            global_lut_path: None,
            global_lut_opacity: 1.0,
            global_lut_enabled: true,
            track_header_width: 160.0,
            auto_generate_filmstrips: None,
            auto_generate_skim: None,
            working_colorspace: klipp_core::WorkingColorspace::default(),
        }
    }
}

pub fn save_project(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
) -> Result<(), String> {
    save_project_with_color(dir, sequences, media_pool, &ColorSettings::default(), None, None, "")
}

pub fn save_project_with_color(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
    color: &ColorSettings,
    active_sequence_id: Option<&str>,
    created_at: Option<&str>,
    notes: &str,
) -> Result<(), String> {
    save_project_with_color_and_backups(dir, sequences, media_pool, color, active_sequence_id, created_at, notes, 5, None, None)
}

/// Save with configurable backup count and optional view state
/// (`timeline_zoom`, `timeline_scroll_x`) so timeline zoom/scroll —
/// including the result of "Fit Timeline to Window" — round-trips
/// across save/load.
#[allow(clippy::too_many_arguments)]
pub fn save_project_with_color_and_backups(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
    color: &ColorSettings,
    active_sequence_id: Option<&str>,
    created_at: Option<&str>,
    notes: &str,
    backup_count: u32,
    timeline_zoom: Option<f32>,
    timeline_scroll_x: Option<f32>,
) -> Result<(), String> {
    save_project_full(
        dir, sequences, media_pool, color,
        active_sequence_id, created_at, notes, backup_count,
        timeline_zoom, timeline_scroll_x,
        &std::collections::HashMap::new(),
    )
}

/// Full save — includes synthetic title assets. Existing wrappers
/// (`save_project`, `save_project_with_color`,
/// `save_project_with_color_and_backups`) call this with an empty
/// asset map so callers that don't yet know about assets keep
/// working unchanged.
#[allow(clippy::too_many_arguments)]
pub fn save_project_full(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
    color: &ColorSettings,
    active_sequence_id: Option<&str>,
    created_at: Option<&str>,
    notes: &str,
    backup_count: u32,
    timeline_zoom: Option<f32>,
    timeline_scroll_x: Option<f32>,
    title_assets: &std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset>,
) -> Result<(), String> {
    // Create directory structure
    std::fs::create_dir_all(dir.join("media"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;

    // Build project.json
    let now = iso8601_now();

    let title_assets_json: Vec<crate::format::TitleAssetJson> = title_assets
        .iter()
        .map(|(id, asset)| crate::format::TitleAssetJson {
            id: id.0.to_string(),
            display_name: asset.display_name.clone(),
            data: serde_json::to_value(&asset.data).unwrap_or(serde_json::Value::Null),
            animation: asset.animation.clone(),
        })
        .collect();

    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
        klipp_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        saved_at: Some(now),
        created_at: created_at.map(|s| s.to_string()),
        platform: Some(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
        sequences: sequences.iter().map(|s| sequence_to_json(s, color, dir)).collect(),
        active_sequence_id: active_sequence_id.map(|s| s.to_string()),
        notes: notes.to_string(),
        timeline_zoom,
        timeline_scroll_x,
        working_colorspace: color.working_colorspace,
        title_assets: title_assets_json,
    };

    let project_json = serde_json::to_string_pretty(&project)
        .map_err(|e| format!("Failed to serialize project: {e}"))?;

    // Rotate backup versions before overwriting
    let project_file = dir.join("project.json");
    if project_file.exists() && backup_count > 0 {
        rotate_backups(&project_file, backup_count);
    }

    atomic_write(&project_file, project_json.as_bytes())
        .map_err(|e| format!("Failed to write project.json: {e}"))?;

    // Build media/manifest.json — store paths relative to project dir
    let manifest = MediaManifestJson {
        version: "1.0.0".to_string(),
        items: media_pool.all().map(|item| media_item_to_json(item, dir)).collect(),
        bins: media_pool.bins.iter().map(|b| BinJson {
            id: b.id.to_string(),
            name: b.name.clone(),
            parent_id: b.parent_id.map(|p| p.to_string()),
        }).collect(),
        smart_bins: media_pool.smart_bins.clone(),
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize media manifest: {e}"))?;
    atomic_write(&dir.join("media").join("manifest.json"), manifest_json.as_bytes())
        .map_err(|e| format!("Failed to write manifest.json: {e}"))?;

    info!("Project saved to {:?}", dir);
    Ok(())
}

fn timecode_to_json(tc: &klipp_core::Timecode) -> TimecodeJson {
    TimecodeJson { value: tc.value, rate: tc.rate }
}

fn sequence_to_json(seq: &Sequence, color: &ColorSettings, project_dir: &Path) -> SequenceJson {
    SequenceJson {
        id: seq.id.to_string(),
        name: seq.name.clone(),
        fps: FpsJson { num: seq.fps.num, den: seq.fps.den },
        resolution: ResolutionJson { width: seq.resolution.width, height: seq.resolution.height },
        in_point: seq.in_point.as_ref().map(timecode_to_json),
        out_point: seq.out_point.as_ref().map(timecode_to_json),
        global_lut_path: color.global_lut_path.as_ref().map(|p| {
            to_project_relative(project_dir, p)
                .to_string_lossy()
                .to_string()
        }),
        global_lut_opacity: color.global_lut_opacity,
        global_lut_enabled: color.global_lut_enabled,
        track_header_width: color.track_header_width,
        auto_generate_filmstrips: color.auto_generate_filmstrips,
        auto_generate_skim: color.auto_generate_skim,
        brief: seq.brief.clone(),
        edit_plan: seq.edit_plan.clone(),
        camera: seq.camera.clone(),
        drop_frame: Some(seq.drop_frame),
        last_playhead: seq.last_playhead.map(|rt| TimecodeJson { value: rt.value, rate: rt.rate }),
        markers: seq.markers.iter().map(|m| {
            MarkerJson {
                id: m.id.to_string(),
                position: timecode_to_json(&m.position),
                label: m.label.clone(),
                color: format!("{:?}", m.color),
                duration: m.duration.as_ref().map(timecode_to_json),
                comment: m.comment.clone(),
                kind: format!("{:?}", m.kind),
            }
        }).collect(),
        tracks: seq.tracks.iter().map(|t| {
            TrackJson {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: match t.kind {
                    TrackKind::Video => "Video".to_string(),
                    TrackKind::Audio => "Audio".to_string(),
                    TrackKind::Text => "Text".to_string(),
                    TrackKind::Subtitle => "Subtitle".to_string(),
                },
                opacity: t.opacity,
                volume: t.volume,
                muted: t.muted,
                solo: t.solo,
                visible: t.visible,
                pan: t.pan,
                locked: t.locked,
                custom_height: t.custom_height,
                automation_lane_visible: t.automation_lane_visible.map(|p| match p {
                    klipp_core::AudioParam::Volume => "Volume".to_string(),
                    klipp_core::AudioParam::Pan => "Pan".to_string(),
                    klipp_core::AudioParam::Gain => "Gain".to_string(),
                }),
                automation_lane_height: t.automation_lane_height,
                magnetic_role: Some(t.magnetic_role.clone()),
                storyline_anchor: t.storyline_anchor.clone(),
                transitions: t.transitions.iter().map(|tr| {
                    TransitionJson {
                        id: tr.id.to_string(),
                        kind: format!("{:?}", tr.kind),
                        duration: timecode_to_json(&tr.duration),
                        clip_a_id: tr.clip_a_id.to_string(),
                        clip_b_id: tr.clip_b_id.to_string(),
                        user_params: tr.user_params,
                        alignment: tr.alignment,
                    }
                }).collect(),
                clips: t.clips.iter().map(|c| {
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id().map(|mid| mid.to_string()),
                        kind: Some(clip_kind_to_json(&c.kind, project_dir)),
                        timeline_in: c.timeline_in.value,
                        timeline_out: c.timeline_out.value,
                        source_in: c.source_in.value,
                        source_out: c.source_out.value,
                        rate: c.timeline_in.rate,
                        node_graph: Some(c.node_graph.clone()),
                        color_grade: None,
                        effects: c.effects.iter().map(effect_to_json).collect(),
                        builtin_effects: if c.builtin_effects.is_empty() {
                            None
                        } else {
                            serde_json::to_value(&c.builtin_effects).ok()
                        },
                        builtin_effects_enabled: c.builtin_effects_enabled.clone(),
                        magnetic_anchor: c.magnetic_anchor.clone(),
                        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
                        linked_title_asset_id: c.linked_title_asset_id.map(|id| id.to_string()),
                        group_id: c.group_id.map(|id| id.to_string()),
                        gain: if (c.gain - 1.0).abs() > f32::EPSILON { Some(c.gain) } else { None },
                        volume: if (c.volume - 1.0).abs() > f32::EPSILON { Some(c.volume) } else { None },
                        pan: if c.pan.abs() > f32::EPSILON { Some(c.pan) } else { None },
                        fade_in_secs: if c.fade_in_secs.abs() > f32::EPSILON { Some(c.fade_in_secs) } else { None },
                        fade_out_secs: if c.fade_out_secs.abs() > f32::EPSILON { Some(c.fade_out_secs) } else { None },
                        fade_in_curve: if c.fade_in_curve != klipp_timeline::FadeCurve::Linear { Some(c.fade_in_curve) } else { None },
                        fade_out_curve: if c.fade_out_curve != klipp_timeline::FadeCurve::Linear { Some(c.fade_out_curve) } else { None },
                        channel_config: if c.channel_config != klipp_timeline::ChannelConfig::Default { Some(c.channel_config) } else { None },
                        volume_keyframes: if c.volume_keyframes.is_empty() { None } else { Some(c.volume_keyframes.clone()) },
                        pan_keyframes: if c.pan_keyframes.is_empty() { None } else { Some(c.pan_keyframes.clone()) },
                        gain_keyframes: if c.gain_keyframes.is_empty() { None } else { Some(c.gain_keyframes.clone()) },
                        opacity_keyframes: if c.opacity_keyframes.is_empty() { None } else { Some(c.opacity_keyframes.clone()) },
                        transform_pos_x_keyframes: if c.transform_pos_x_keyframes.is_empty() { None } else { Some(c.transform_pos_x_keyframes.clone()) },
                        transform_pos_y_keyframes: if c.transform_pos_y_keyframes.is_empty() { None } else { Some(c.transform_pos_y_keyframes.clone()) },
                        transform_scale_keyframes: if c.transform_scale_keyframes.is_empty() { None } else { Some(c.transform_scale_keyframes.clone()) },
                        transform_rotation_keyframes: if c.transform_rotation_keyframes.is_empty() { None } else { Some(c.transform_rotation_keyframes.clone()) },
                        transform_tilt_x_keyframes: (!c.transform_tilt_x_keyframes.is_empty()).then(|| c.transform_tilt_x_keyframes.clone()),
                        transform_tilt_y_keyframes: (!c.transform_tilt_y_keyframes.is_empty()).then(|| c.transform_tilt_y_keyframes.clone()),
                        matte_feather_keyframes: if c.matte_feather_keyframes.is_empty() { None } else { Some(c.matte_feather_keyframes.clone()) },
                        matte_choke_keyframes: if c.matte_choke_keyframes.is_empty() { None } else { Some(c.matte_choke_keyframes.clone()) },
                        clip_markers: if c.clip_markers.is_empty() { None } else {
                            Some(c.clip_markers.iter().map(|m| crate::format::ClipMarkerJson {
                                time_secs: m.time_secs,
                                name: m.name.clone(),
                                color: format!("{:?}", m.color),
                                comment: m.comment.clone(),
                            }).collect())
                        },
                        effect_keyframes: if c.effect_keyframes.is_empty() { None } else { Some(c.effect_keyframes.clone()) },
                        role: if c.role == klipp_core::ClipRole::default() { None } else { Some(c.role) },
                        param_modifiers: if c.param_modifiers.is_empty() {
                            None
                        } else {
                            Some(c.param_modifiers.iter().map(|(k, v)| (k.to_key(), v.clone())).collect())
                        },
                        speed: if (c.speed - 1.0).abs() < f64::EPSILON { None } else { Some(c.speed) },
                        speed_keyframes: if c.speed_keyframes.is_empty() { None } else { Some(c.speed_keyframes.clone()) },
                        source_rotation: if c.source_rotation == 0 { None } else { Some(c.source_rotation) },
                        enabled: if c.enabled { None } else { Some(false) },
                        audio_effects: if c.audio_effects.is_empty() { None } else { Some(c.audio_effects.clone()) },
                        audio_effect_keyframes: c.audio_effect_keyframes.clone(),
                        multicam_group: c.multicam_group.map(|g| g.to_string()),
                        multicam_angle: if c.multicam_angle == 0 { None } else { Some(c.multicam_angle) },
                        multicam_cuts: c.multicam_cuts.clone(),
                        solo: if c.solo { Some(true) } else { None },
                        blend_mode: if matches!(c.blend_mode, klipp_core::BlendMode::Normal) {
                            None
                        } else {
                            Some(c.blend_mode)
                        },
                        depth: ((c.depth - 1.0).abs() > 1e-6).then_some(c.depth),
                        matte_source: c.matte_source.as_ref().map(|m| crate::format::MatteBindingJson {
                            source_clip_id: m.source_clip_id.0.to_string(),
                            channel: m.channel,
                            invert: m.invert,
                            feather_px: m.feather_px,
                            choke_px: m.choke_px,
                        }),
                        matte_stencil: c.matte_stencil.as_ref().map(|s| crate::format::StencilSettingsJson {
                            channel: s.channel,
                            invert: s.invert,
                            feather_px: s.feather_px,
                            choke_px: s.choke_px,
                        }),
                        consumed_as_matte: if c.consumed_as_matte { Some(true) } else { None },
                        transform_link: c.transform_link.as_ref().map(|tl| crate::format::TransformLinkJson {
                            target: tl.target,
                            mode: tl.mode,
                            inherit_scale: tl.inherit_scale,
                            inherit_position: tl.inherit_position,
                            inherit_rotation: tl.inherit_rotation,
                            inherit_anchor: tl.inherit_anchor,
                        }),
                        grade_versions: c.grade_versions.iter().map(|gv| GradeVersionJson {
                            name: gv.name.clone(),
                            node_graph: gv.node_graph.clone(),
                        }).collect(),
                        active_grade_version: c.active_grade_version,
                        track_points: c.track_points.clone(),
                    }
                }).collect(),
            }
        }).collect(),
    }
}

fn effect_to_json(effect: &klipp_plugin::AppliedEffect) -> EffectJson {
    EffectJson {
        plugin_name: effect.plugin_name.clone(),
        enabled: effect.enabled,
        param_values: effect.param_values.iter().map(|(k, v): (&String, &klipp_plugin::OfxValue)| {
            let json_val = match v {
                klipp_plugin::OfxValue::Double(d) => OfxValueJson::Double(*d),
                klipp_plugin::OfxValue::Double2(a, b) => OfxValueJson::Double2([*a, *b]),
                klipp_plugin::OfxValue::Double3(a, b, c) => OfxValueJson::Double3([*a, *b, *c]),
                klipp_plugin::OfxValue::Int(i) => OfxValueJson::Int(*i),
                klipp_plugin::OfxValue::Int2(a, b) => OfxValueJson::Int2([*a, *b]),
                klipp_plugin::OfxValue::Int3(a, b, c) => OfxValueJson::Int3([*a, *b, *c]),
                klipp_plugin::OfxValue::Bool(b) => OfxValueJson::Bool(*b),
                klipp_plugin::OfxValue::Choice(c) => OfxValueJson::Choice(*c),
                klipp_plugin::OfxValue::String(s) => OfxValueJson::String(s.clone()),
                klipp_plugin::OfxValue::Rgba(r, g, b, a) => OfxValueJson::Rgba([*r, *g, *b, *a]),
                klipp_plugin::OfxValue::Rgb(r, g, b) => OfxValueJson::Rgb([*r, *g, *b]),
            };
            (k.clone(), json_val)
        }).collect(),
    }
}

/// The title as it goes into the file: every texture path made relative
/// to the project directory, the way the flat fields have always stored
/// it, so a project that is moved still finds its textures. The
/// in-memory data keeps its absolute paths.
fn title_data_for_file(data: &klipp_timeline::TitleData, project_dir: &Path) -> klipp_timeline::TitleData {
    let mut copy = data.clone();
    for obj in &mut copy.objects {
        match &mut obj.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                if let Some(p) = tb.texture_fill.as_mut().and_then(|t| t.file_path_mut()) {
                    *p = to_project_relative(project_dir, Path::new(p)).to_string_lossy().to_string();
                }
            }
            klipp_timeline::TitleObjectContent::Image(img) => {
                img.path = to_project_relative(project_dir, Path::new(&img.path)).to_string_lossy().to_string();
            }
            klipp_timeline::TitleObjectContent::Shape(_) => {}
        }
    }
    copy
}

#[allow(clippy::needless_borrows_for_generic_args)]
fn clip_kind_to_json(kind: &ClipKind, project_dir: &Path) -> ClipKindJson {
    match kind {
        ClipKind::Media { media_id } => ClipKindJson::Media {
            media_id: media_id.to_string(),
        },
        ClipKind::Title(data) if data.objects.iter().any(|o| matches!(&o.content,klipp_timeline::TitleObjectContent::Shape(shape) if matches!(shape.kind,klipp_timeline::ShapeKind::Path(_)))) => {
            ClipKindJson::VectorTitle {title_data:title_data_for_file(data,project_dir)}
        }
        ClipKind::Title(data) => {
            // Serialise via the legacy single-text-flat shape for
            // commit 1: read the primary TextBox + first object's
            // transform centre. Multi-object titles will get a
            // proper `objects` array in the JSON in a follow-up
            // commit when the loader can read them.
            let tb = data.primary_text_or_default();
            let pos = data.position();
            ClipKindJson::Title {
                title_data: serde_json::to_value(title_data_for_file(data, project_dir)).ok(),
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: format!("{:?}", tb.font_style),
                color: tb.color,
                alignment: format!("{:?}", tb.alignment),
                position_x: pos.x,
                position_y: pos.y,
                background: serde_json::to_value(&data.background).unwrap_or_default(),
                // Legacy single-shadow JSON field — emit shadows[0]
                // for back-compat with older Klipp readers; the full
                // stack lives under `shadows`.
                shadow: serde_json::to_value(tb.shadows.first()).unwrap_or_default(),
                shadows: serde_json::to_value(&tb.shadows).unwrap_or_default(),
                shadow_stack_enabled: tb.shadow_stack_enabled,
                // Single-outline JSON field is legacy; the canonical
                // list lives under `outlines`. Emit the first outline
                // as `outline` for compat with older readers, and the
                // full list under `outlines`.
                outline: serde_json::to_value(tb.outlines.first()).unwrap_or_default(),
                outlines: serde_json::to_value(&tb.outlines).unwrap_or_default(),
                outline_stack_enabled: tb.outline_stack_enabled,
                // Legacy single-glow field — emit glows[0] for
                // back-compat. Full stack lives under `glows`.
                glow: serde_json::to_value(tb.glows.first()).unwrap_or_default(),
                glows: serde_json::to_value(&tb.glows).unwrap_or_default(),
                glow_stack_enabled: tb.glow_stack_enabled,
                gradient: serde_json::to_value(&tb.gradient).unwrap_or_default(),
                // Legacy single-inner-shadow field — emit inner_shadows[0]
                // for back-compat with older readers; the full stack
                // lives under `inner_shadows`.
                inner_shadow: serde_json::to_value(tb.inner_shadows.first()).unwrap_or_default(),
                inner_shadows: serde_json::to_value(&tb.inner_shadows).unwrap_or_default(),
                inner_shadow_stack_enabled: tb.inner_shadow_stack_enabled,
                pattern_fill: serde_json::to_value(&tb.pattern_fill).unwrap_or_default(),
                tracking: tb.tracking,
                line_spacing: tb.line_spacing,
                write_on: tb.write_on,
                extrusion: serde_json::to_value(&tb.extrusion).unwrap_or_default(),
                text_path: serde_json::to_value(&tb.text_path).unwrap_or_default(),
                char_animation: serde_json::to_value(&tb.char_animation).unwrap_or_default(),
                // Legacy path field — written when the source is a
                // File so older Klipp versions can still render the
                // texture. Procedural-preset sources have no path
                // representation, so this is None for those.
                texture_fill_path: tb.texture_fill.as_ref()
                    .and_then(|t| t.file_path())
                    .map(|p| {
                        crate::path_util::to_project_relative(project_dir, Path::new(p))
                            .to_string_lossy()
                            .to_string()
                    }),
                // New canonical field — full TextureFill struct.
                // Project-relative path is rewritten in the same
                // way before serialisation so the project stays
                // portable.
                texture_fill: tb.texture_fill.as_ref().map(|t| {
                    let mut copy = t.clone();
                    if let Some(p) = copy.file_path_mut() {
                        *p = crate::path_util::to_project_relative(project_dir, Path::new(&p))
                            .to_string_lossy()
                            .to_string();
                    }
                    serde_json::to_value(&copy).unwrap_or_default()
                }).unwrap_or_default(),
                motion_blur: serde_json::to_value(&tb.motion_blur).unwrap_or_default(),
                style_runs: serde_json::to_value(&tb.style_runs).unwrap_or_default(),
            }
        }
        ClipKind::Gap => ClipKindJson::Gap,
        ClipKind::Sequence { sequence_id } => ClipKindJson::Sequence {
            sequence_id: sequence_id.to_string(),
        },
        ClipKind::Generator(data) => ClipKindJson::Generator {
            data: serde_json::to_value(data).unwrap_or_default(),
        },
        ClipKind::AdjustmentLayer => ClipKindJson::AdjustmentLayer,
        ClipKind::Precomposition { sequence_id } => ClipKindJson::Precomposition {
            sequence_id: sequence_id.to_string(),
        },
    }
}

/// Generate an ISO 8601 UTC timestamp string.
pub fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let mut y = 1970i64;
    let mut d = days as i64;
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 0usize;
    for md in &mdays {
        if d < *md { break; }
        d -= md;
        mo += 1;
    }
    format!("{y:04}-{:02}-{:02}T{h:02}:{m:02}:{s:02}Z", mo + 1, d + 1)
}

fn media_item_to_json(item: &MediaItem, project_dir: &Path) -> MediaItemJson {
    MediaItemJson {
        id: item.id.to_string(),
        path: to_project_relative(project_dir, &item.path)
            .to_string_lossy()
            .to_string(),
        filename: item.filename.clone(),
        duration_frames: item.duration.value,
        duration_rate: item.duration.rate,
        fps: FpsJson { num: item.fps.num, den: item.fps.den },
        resolution: ResolutionJson { width: item.resolution.width, height: item.resolution.height },
        video_codec: item.video_codec.clone(),
        audio_codec: item.audio_codec.clone(),
        has_audio: item.has_audio,
        file_size_bytes: item.file_size_bytes,
        sample_rate: item.sample_rate,
        audio_channels: item.audio_channels,
        is_still: item.is_still,
        proxy_path: item.proxy_path.as_ref().map(|p| {
            to_project_relative(project_dir, p)
                .to_string_lossy()
                .to_string()
        }),
        proxy_ready: item.proxy_ready,
        default_source_in: item.default_source_in.as_ref().map(timecode_to_json),
        default_source_out: item.default_source_out.as_ref().map(timecode_to_json),
        rating: item.rating,
        tags: item.tags.clone(),
        user_notes: item.user_notes.clone(),
        bin: item.bin.clone(),
        bin_id: item.bin_id.map(|id| id.to_string()),
        parent_media_id: item.parent_media_id.map(|id| id.to_string()),
        added_at: if item.added_at > 0 { Some(item.added_at) } else { None },
        file_modified_at: item.file_modified_at,
        media_created_at: item.media_created_at,
        color_label: item.color_label.to_u8(),
        colour_space: item.colour_space.to_u8(),
        source_rotation: if item.source_rotation == 0 { None } else { Some(item.source_rotation) },
        camera_make: item.camera_make.clone(),
        camera_model: item.camera_model.clone(),
        encoder: item.encoder.clone(),
        bit_depth: item.bit_depth,
        video_bitrate_kbps: item.video_bitrate_kbps,
        audio_bitrate_kbps: item.audio_bitrate_kbps,
        color_primaries: item.color_primaries.clone(),
        color_transfer: item.color_transfer.clone(),
        pixel_format: item.pixel_format.clone(),
        log_format: item.log_format,
        log_format_user_set: item.log_format_user_set,
    }
}

/// Rotate backup files: project.json → project.json.bak.1, .bak.1 → .bak.2, etc.
/// Keeps up to `max_backups` versions.
fn rotate_backups(path: &Path, max_backups: u32) {
    // Remove oldest backup if at limit
    let oldest = path.with_extension(format!("json.bak.{max_backups}"));
    let _ = std::fs::remove_file(&oldest);

    // Shift existing backups: .bak.N-1 → .bak.N
    for i in (1..max_backups).rev() {
        let from = path.with_extension(format!("json.bak.{i}"));
        let to = path.with_extension(format!("json.bak.{}", i + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }

    // Copy current file to .bak.1 atomically (read + atomic_write)
    let bak1 = path.with_extension("json.bak.1");
    if let Ok(data) = std::fs::read(path) {
        let _ = atomic_write(&bak1, &data);
    }
}

/// Save the project's Looks library to `<dir>/looks/{id}.json`. One
/// file per Look — keeps the diffs small when the user edits a single
/// Look in a project tracked by version control. Stale files (Looks
/// the user deleted in the session) are pruned on save so the on-disk
/// library matches in-memory state.
pub fn save_looks(dir: &Path, looks: &[Look]) -> Result<(), String> {
    let looks_dir = dir.join("looks");
    if looks.is_empty() {
        // Nothing to save — leave an empty looks/ directory if it
        // already exists, otherwise skip the mkdir entirely.
        if looks_dir.exists() {
            prune_stale_looks(&looks_dir, &[]);
        }
        return Ok(());
    }
    std::fs::create_dir_all(&looks_dir)
        .map_err(|e| format!("Failed to create looks dir: {e}"))?;
    for look in looks {
        let json = serde_json::to_string_pretty(look)
            .map_err(|e| format!("Failed to serialize Look {}: {e}", look.name))?;
        let path = looks_dir.join(format!("{}.json", look.id));
        atomic_write(&path, json.as_bytes())
            .map_err(|e| format!("Failed to write Look {}: {e}", look.name))?;
    }
    let kept_ids: Vec<String> = looks.iter().map(|l| l.id.to_string()).collect();
    prune_stale_looks(&looks_dir, &kept_ids);
    Ok(())
}

/// Remove `looks/*.json` files whose UUID isn't in `kept_ids`. Deleted
/// Looks shouldn't reappear after the next save just because their
/// file lingers on disk.
fn prune_stale_looks(looks_dir: &Path, kept_ids: &[String]) {
    let Ok(entries) = std::fs::read_dir(looks_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") { continue; }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if !kept_ids.iter().any(|id| id == stem) {
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Failed to remove stale look {:?}: {e}", path);
            }
        }
    }
}
