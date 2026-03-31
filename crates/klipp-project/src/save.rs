use crate::format::*;
use crate::path_util::to_project_relative;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{ClipKind, Sequence, TrackKind};
use std::path::Path;
use tracing::info;

/// Project-level settings persisted alongside sequence data.
pub struct ColorSettings {
    pub global_lut_path: Option<std::path::PathBuf>,
    pub global_lut_opacity: f32,
    pub global_lut_enabled: bool,
    pub track_header_width: f32,
    /// Per-project: generate filmstrip thumbnails on import. None = use global pref.
    pub auto_generate_filmstrips: Option<bool>,
    /// Per-project: generate skim transcodes on import. None = use global pref.
    pub auto_generate_skim: Option<bool>,
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
        }
    }
}

pub fn save_project(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
) -> Result<(), String> {
    save_project_with_color(dir, sequences, media_pool, &ColorSettings::default(), None, None)
}

pub fn save_project_with_color(
    dir: &Path,
    sequences: &[Sequence],
    media_pool: &MediaPool,
    color: &ColorSettings,
    active_sequence_id: Option<&str>,
    created_at: Option<&str>,
) -> Result<(), String> {
    // Create directory structure
    std::fs::create_dir_all(dir.join("media"))
        .map_err(|e| format!("Failed to create project directory: {e}"))?;

    // Build project.json
    let now = iso8601_now();

    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
        klipp_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        saved_at: Some(now),
        created_at: created_at.map(|s| s.to_string()),
        platform: Some(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
        sequences: sequences.iter().map(|s| sequence_to_json(s, color, dir)).collect(),
        active_sequence_id: active_sequence_id.map(|s| s.to_string()),
    };

    let project_json = serde_json::to_string_pretty(&project)
        .map_err(|e| format!("Failed to serialize project: {e}"))?;
    std::fs::write(dir.join("project.json"), project_json)
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
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize media manifest: {e}"))?;
    std::fs::write(dir.join("media").join("manifest.json"), manifest_json)
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
        last_playhead: seq.last_playhead.map(|rt| TimecodeJson { value: rt.value, rate: rt.rate }),
        markers: seq.markers.iter().map(|m| {
            MarkerJson {
                id: m.id.to_string(),
                position: timecode_to_json(&m.position),
                label: m.label.clone(),
                color: format!("{:?}", m.color),
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
                transitions: t.transitions.iter().map(|tr| {
                    TransitionJson {
                        id: tr.id.to_string(),
                        kind: format!("{:?}", tr.kind),
                        duration: timecode_to_json(&tr.duration),
                        clip_a_id: tr.clip_a_id.to_string(),
                        clip_b_id: tr.clip_b_id.to_string(),
                    }
                }).collect(),
                clips: t.clips.iter().map(|c| {
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id().map(|mid| mid.to_string()),
                        kind: Some(clip_kind_to_json(&c.kind)),
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
                        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
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
                        clip_markers: if c.clip_markers.is_empty() { None } else {
                            Some(c.clip_markers.iter().map(|m| crate::format::ClipMarkerJson {
                                time_secs: m.time_secs,
                                name: m.name.clone(),
                                color: format!("{:?}", m.color),
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
                        source_rotation: if c.source_rotation == 0 { None } else { Some(c.source_rotation) },
                        audio_effects: if c.audio_effects.is_empty() { None } else { Some(c.audio_effects.clone()) },
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

#[allow(clippy::needless_borrows_for_generic_args)]
fn clip_kind_to_json(kind: &ClipKind) -> ClipKindJson {
    match kind {
        ClipKind::Media { media_id } => ClipKindJson::Media {
            media_id: media_id.to_string(),
        },
        ClipKind::Title(data) => ClipKindJson::Title {
            text: data.text.clone(),
            font_size: data.font_size,
            font_name: data.font_name.clone(),
            font_style: format!("{:?}", data.font_style),
            color: data.color,
            alignment: format!("{:?}", data.alignment),
            position_x: data.position.x,
            position_y: data.position.y,
            background: serde_json::to_value(&data.background).unwrap_or_default(),
            shadow: serde_json::to_value(&data.shadow).unwrap_or_default(),
            outline: serde_json::to_value(&data.outline).unwrap_or_default(),
            outlines: serde_json::to_value(&data.outlines).unwrap_or_default(),
            glow: serde_json::to_value(&data.glow).unwrap_or_default(),
            gradient: serde_json::to_value(&data.gradient).unwrap_or_default(),
            inner_shadow: serde_json::to_value(&data.inner_shadow).unwrap_or_default(),
            pattern_fill: serde_json::to_value(&data.pattern_fill).unwrap_or_default(),
            tracking: data.tracking,
            line_spacing: data.line_spacing,
            write_on: data.write_on,
            extrusion: serde_json::to_value(&data.extrusion).unwrap_or_default(),
            text_path: serde_json::to_value(&data.text_path).unwrap_or_default(),
            char_animation: serde_json::to_value(&data.char_animation).unwrap_or_default(),
            texture_fill_path: data.texture_fill_path.clone(),
            motion_blur: serde_json::to_value(&data.motion_blur).unwrap_or_default(),
            style_runs: serde_json::to_value(&data.style_runs).unwrap_or_default(),
        },
        ClipKind::Gap => ClipKindJson::Gap,
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
        added_at: if item.added_at > 0 { Some(item.added_at) } else { None },
        file_modified_at: item.file_modified_at,
        media_created_at: item.media_created_at,
        color_label: item.color_label.to_u8(),
        colour_space: item.colour_space.to_u8(),
        source_rotation: if item.source_rotation == 0 { None } else { Some(item.source_rotation) },
    }
}
