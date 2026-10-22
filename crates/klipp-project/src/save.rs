use crate::format::*;
use crate::path_util::{atomic_write, to_project_relative};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::picture_v1::ClipPictureV1;
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
    /// The grade encoding chosen (#467); BT.1886 by default.
    pub grade_encoding: klipp_core::GradeEncoding,
    /// The output transform new exports start from (#466).
    pub default_odt: klipp_core::OdtChoice,
    /// The input format new clips take when their file says none (#466).
    pub default_input_format: Option<klipp_core::LogFormat>,
    /// The LUT new clips start their grade with (#466).
    pub default_input_lut: Option<std::path::PathBuf>,
    /// The look the viewer shows through, on screen only (#466).
    pub viewing_lut: Option<std::path::PathBuf>,
    /// The scopes measure after the viewing LUT (#466).
    pub scopes_after_viewing_lut: bool,
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
            grade_encoding: klipp_core::GradeEncoding::default(),
            default_odt: klipp_core::OdtChoice::default(),
            default_input_format: None,
            default_input_lut: None,
            viewing_lut: None,
            scopes_after_viewing_lut: false,
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
    save_project_with_color_and_backups(dir, sequences, media_pool, color, active_sequence_id, created_at, notes, 5, None, None, None)
}

/// Save with configurable backup count and optional view state
/// (`timeline_zoom`, `timeline_scroll_x`, `timeline_track_scale`) so timeline view —
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
    timeline_track_scale: Option<f32>,
) -> Result<(), String> {
    save_project_full(
        dir, sequences, media_pool, color,
        active_sequence_id, created_at, notes, backup_count,
        timeline_zoom, timeline_scroll_x, timeline_track_scale,
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
    timeline_track_scale: Option<f32>,
    title_assets: &std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset>,
) -> Result<(), String> {
    save_project_full_as(
        dir, sequences, media_pool, color, active_sequence_id, created_at, notes, backup_count,
        timeline_zoom, timeline_scroll_x, timeline_track_scale, title_assets, crate::FileForm::Revision2,
    )
}

/// Today's v1 writer, kept only to prove the one-time conversion (#1089):
/// the project as written before revision 2, by the same writer without its
/// revision-2 step. Behind `v1-oracle`, removed once the conversion has run
/// and been verified.
#[cfg(feature = "v1-oracle")]
pub fn save_project_v1(dir: &Path, sequences: &[Sequence], media_pool: &MediaPool) -> Result<(), String> {
    save_project_full_as(
        dir, sequences, media_pool, &ColorSettings::default(), None, None, "", 5,
        None, None, None, &std::collections::HashMap::new(), crate::FileForm::V1,
    )
}

#[allow(clippy::too_many_arguments)]
fn save_project_full_as(
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
    timeline_track_scale: Option<f32>,
    title_assets: &std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset>,
    form: crate::FileForm,
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

    // One id for this save, written into both files (#821), and into the
    // pending mark while it is under way (#840).
    let generation = new_generation();
    let saving = generation.clone();
    let project = ProjectJson {
        version: "1.0.0".to_string(),
        klipp_version_min: "0.1.0".to_string(),
        format_revision: crate::FORMAT_REVISION,
        generation: Some(generation.clone()),
        klipp_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        saved_at: Some(now),
        created_at: created_at.map(|s| s.to_string()),
        platform: Some(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
        sequences: sequences.iter().map(|s| sequence_to_json(s, color, dir)).collect(),
        active_sequence_id: active_sequence_id.map(|s| s.to_string()),
        notes: notes.to_string(),
        timeline_zoom,
        timeline_scroll_x,
        timeline_track_scale,
        working_colorspace: color.working_colorspace,
        grade_encoding: color.grade_encoding,
        default_odt: color.default_odt,
        default_input_format: color.default_input_format,
        default_input_lut: color.default_input_lut.as_deref().map(|p| to_project_relative(dir, p).to_string_lossy().into_owned()),
        viewing_lut: color.viewing_lut.as_deref().map(|p| to_project_relative(dir, p).to_string_lossy().into_owned()),
        scopes_after_viewing_lut: color.scopes_after_viewing_lut,
        title_assets: title_assets_json,
    };

    let project_json = match form {
        crate::FileForm::Revision2 => revision2_text(&project, sequences)?,
        #[cfg(feature = "v1-oracle")]
        crate::FileForm::V1 => {
            let mut project = project;
            project.format_revision = 0;
            serde_json::to_string_pretty(&project).map_err(|e| format!("Failed to serialize project: {e}"))?
        }
    };

    // Build media/manifest.json — store paths relative to project dir
    let manifest = MediaManifestJson {
        version: "1.0.0".to_string(),
        generation: Some(generation),
        // By id: the pool is a hash map, whose order changes from run to
        // run, and an unchanged project should save the same (#1354).
        items: {
            let mut items: Vec<_> = media_pool.all().map(|item| media_item_to_json(item, dir)).collect();
            items.sort_by(|a, b| a.id.cmp(&b.id));
            items
        },
        bins: media_pool.bins.iter().map(|b| BinJson {
            id: b.id.to_string(),
            name: b.name.clone(),
            parent_id: b.parent_id.map(|p| p.to_string()),
        }).collect(),
        smart_bins: media_pool.smart_bins.clone(),
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize media manifest: {e}"))?;

    commit_pair(dir, project_json.as_bytes(), manifest_json.as_bytes(), backup_count, &saving)?;

    info!("Project saved to {:?}", dir);
    Ok(())
}

/// A fresh id for one save: the time to the nanosecond and the process,
/// unique enough that two saves never share one.
fn new_generation() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}-{:x}", std::process::id())
}

/// Where a save keeps the pair it replaces until the new one is whole
/// (#840): `project.json`, `manifest.json`, and while a save is under way,
/// `pending` with its generation.
pub(crate) const RECOVERY_DIR: &str = ".recovery";

/// Writes a save's two files as one step that can be recovered (#821,
/// #840). Each write is atomic on its own; the pair was not, so a failure
/// between them left the new timeline beside the old media list. Now:
///
/// - the pair being replaced is copied into `.recovery/` first, and a
///   `pending` mark holding this save's generation is written: whatever
///   the backup setting, an open after a crash finds the pair from before
///   (`crate::load`), including for a project saved before generations;
///   a save that cannot make that copy does not go ahead;
/// - the manifest is written first and `project.json` last, the commit
///   point: if the manifest fails nothing has changed, and if
///   `project.json` fails the previous manifest is put back;
/// - the mark is removed once both are written;
/// - the user's backups rotate together, so `.bak.N` of each is the same
///   earlier save, and a backup that fails is said rather than ignored.
fn commit_pair(dir: &Path, project: &[u8], manifest: &[u8], backup_count: u32, generation: &str) -> Result<(), String> {
    let project_file = dir.join("project.json");
    let manifest_file = dir.join("media").join("manifest.json");
    let recovery = dir.join(RECOVERY_DIR);
    std::fs::create_dir_all(&recovery).map_err(|e| format!("Failed to make the recovery folder: {e}"))?;
    // After a save that stopped between its files the pair on disk is
    // broken (its two files from different saves), and the recovery copy
    // is the good one: copying the broken pair over it would lose it if
    // this save stopped too (#858 review). It is kept as it is. The files
    // are compared with each other, not with the pending mark, which a
    // retry that stopped before its manifest has already rewritten (#927).
    let broken = recovery.join("pending").exists() && {
        let read = |f: &Path| std::fs::read_to_string(f).unwrap_or_default();
        !crate::load::pair_is_whole(&read(&project_file), &read(&manifest_file))
    };
    if broken {
        tracing::warn!("the last save stopped between its files; keeping its recovery copy rather than copying the broken pair over it");
    } else if project_file.exists() && manifest_file.exists() {
        for (from, name) in [(&manifest_file, "manifest.json"), (&project_file, "project.json")] {
            let data = std::fs::read(from).map_err(|e| format!("Failed to read {name} to keep a recovery copy: {e}"))?;
            atomic_write(&recovery.join(name), &data).map_err(|e| format!("Failed to keep a recovery copy of {name}: {e}"))?;
        }
    }
    atomic_write(&recovery.join("pending"), generation.as_bytes()).map_err(|e| format!("Failed to mark the save as under way: {e}"))?;
    if backup_count > 0 {
        for file in [&project_file, &manifest_file] {
            if file.exists() {
                if let Err(e) = rotate_backups(file, backup_count) {
                    tracing::warn!("{e}; the save goes on, its recovery copy is kept");
                }
            }
        }
    }
    let previous_manifest = std::fs::read(&manifest_file).ok();
    atomic_write(&manifest_file, manifest).map_err(|e| format!("Failed to write manifest.json: {e}"))?;
    if let Err(e) = atomic_write(&project_file, project) {
        // Put the media list back with the timeline it belongs to.
        let restored = match &previous_manifest {
            Some(old) => atomic_write(&manifest_file, old).is_ok(),
            None => std::fs::remove_file(&manifest_file).is_ok(),
        };
        if !restored {
            tracing::warn!("project.json failed and the previous media manifest could not be put back; the recovery copy is used on open");
        }
        return Err(format!("Failed to write project.json: {e}"));
    }
    if let Err(e) = std::fs::remove_file(recovery.join("pending")) {
        tracing::warn!("the save is complete, but its pending mark could not be removed ({e}); the next open still reads the new pair");
    }
    Ok(())
}

/// A clip's four times as values at one rate, which is how a clip is
/// stored. The times may carry different rates (a clip at its media's
/// rate bladed or moved at the sequence's), and a value written at a rate
/// it was not counted in reads back as another time. The rate is the
/// least common multiple of theirs, so every time is stored exactly; a
/// clip whose times already share a rate is written as before.
fn one_rate(times: [klipp_core::Timecode; 4]) -> ([i64; 4], u32) {
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let rate = times.iter().try_fold(1u64, |acc, t| {
        let r = u64::from(t.rate.max(1));
        let lcm = acc / gcd(acc, r) * r;
        (lcm <= u64::from(u32::MAX)).then_some(lcm)
    });
    let exact = rate.and_then(|rate| {
        let values = times.map(|t| i64::try_from(rate / u64::from(t.rate.max(1))).ok().and_then(|k| t.value.checked_mul(k)));
        values.iter().all(Option::is_some).then(|| (values.map(Option::unwrap), rate as u32))
    });
    // Rates with no common multiple a u32 holds: the finest of them, to
    // the nearest tick.
    exact.unwrap_or_else(|| {
        let finest = times.iter().map(|t| t.rate).max().unwrap_or(1);
        (times.map(|t| t.rescale_to(finest).value), finest)
    })
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
        first_clip_autoset: seq.first_clip_autoset,
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
        reviews: seq.reviews.clone(),
        awaiting_refit: seq.awaiting_refit,
        camera: seq.camera.clone(),
        timeline_graph: seq.timeline_grade().cloned(),
        grade_groups: seq.grade_groups.clone(),
        drop_frame: Some(seq.drop_frame),
        title_nits: (seq.title_nits != klipp_timeline::DEFAULT_TITLE_NITS).then_some(seq.title_nits),
        // A label whose node is gone names nothing any more (#1753).
        node_labels: seq.live_node_labels(),
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
                review: m.review.clone(),
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
                sync_lock: t.sync_lock,
                targeted: t.targeted,
                source_patch: t.source_patch,
                caption_style: t.caption_style.clone(),
                language: t.language.clone(),
                source_file: t.source_file.as_ref().map(|p| to_project_relative(project_dir, p).to_string_lossy().into_owned()),
                custom_height: t.custom_height,
                colour_label: t.colour_label,
                channel_waveforms: t.channel_waveforms,
                automation_lane_visible: t.automation_lane_visible.map(|p| match (p, t.automation_lane_track) {
                    (klipp_core::AudioParam::Volume, true) => "TrackVolume".to_string(),
                    (klipp_core::AudioParam::Pan, true) => "TrackPan".to_string(),
                    (klipp_core::AudioParam::Volume, false) => "Volume".to_string(),
                    (klipp_core::AudioParam::Pan, false) => "Pan".to_string(),
                    (klipp_core::AudioParam::Gain, _) => "Gain".to_string(),
                }),
                automation_lane_height: t.automation_lane_height,
                automation: (!t.automation.is_default()).then(|| t.automation.clone()),
                magnetic_role: Some(t.magnetic_role.clone()),
                storyline_anchor: t.storyline_anchor.clone(),
                effects: (!t.effects.is_empty()).then(|| crate::format::TrackEffectsJson {
                    video: if t.effects.video_effects().is_empty() { None } else { serde_json::to_value(t.effects.video_effects()).ok() },
                    video_enabled: t.effects.video_effects_enabled().to_vec(),
                    grade: t.effects.track_grade().filter(|g| !g.is_identity()).cloned(),
                    audio: t.effects.audio.clone(),
                }),
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
                    // The picture fields come from the one mapping the
                    // picture store lifts too (#1089, M1b).
                    let ClipPictureV1 {
                        node_graph,
                        color_grade,
                        effects,
                        builtin_effects,
                        builtin_effects_enabled,
                        opacity_keyframes,
                        transform_pos_x_keyframes,
                        transform_pos_y_keyframes,
                        transform_scale_keyframes,
                        transform_scale_y_keyframes,
                        transform_rotation_keyframes,
                        transform_tilt_x_keyframes,
                        transform_tilt_y_keyframes,
                        effect_keyframes,
                        param_modifiers,
                        grade_versions,
                        active_grade_version,
                    } = ClipPictureV1::of(c);
                    let ([timeline_in, timeline_out, source_in, source_out], rate) =
                        one_rate([c.timeline_in, c.timeline_out, c.source_in, c.source_out]);
                    ClipJson {
                        id: c.id.to_string(),
                        media_id: c.media_id().map(|mid| mid.to_string()),
                        kind: Some(clip_kind_to_json(&c.kind, project_dir)),
                        timeline_in,
                        timeline_out,
                        source_in,
                        source_out,
                        rate,
                        node_graph,
                        color_grade,
                        effects,
                        builtin_effects,
                        builtin_effects_enabled,
                        magnetic_anchor: c.magnetic_anchor.clone(),
                        linked_clip_id: c.linked_clip_id.map(|id| id.to_string()),
                        linked_title_asset_id: c.linked_title_asset_id.map(|id| id.to_string()),
                        group_id: c.group_id.map(|id| id.to_string()),
                        grade_group: c.grade_group.map(|id| id.to_string()),
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
                        opacity_keyframes,
                        transform_pos_x_keyframes,
                        transform_pos_y_keyframes,
                        transform_scale_keyframes,
                        transform_scale_y_keyframes,
                        transform_rotation_keyframes,
                        transform_tilt_x_keyframes,
                        transform_tilt_y_keyframes,
                        frame_glide: c.frame_glide,
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
                        effect_keyframes,
                        role: if c.role == klipp_core::ClipRole::default() { None } else { Some(c.role) },
                        role_source: c.role_source,
                        matched_to: c.matched_to,
                        param_modifiers,
                        speed: if (c.speed - 1.0).abs() < f64::EPSILON { None } else { Some(c.speed) },
                        speed_keyframes: if c.speed_keyframes.is_empty() { None } else { Some(c.speed_keyframes.clone()) },
                        keep_pitch: c.keep_pitch.then_some(true),
                        source_rotation: if c.source_rotation == 0 { None } else { Some(c.source_rotation) },
                        enabled: if c.enabled { None } else { Some(false) },
                        audio_effects: if c.audio_effects.is_empty() { None } else { Some(c.audio_effects.clone()) },
                        audio_effect_keyframes: c.audio_effect_keyframes.clone(),
                        multicam_group: c.multicam_group.map(|g| g.to_string()),
                        multicam_angle: if c.multicam_angle == 0 { None } else { Some(c.multicam_angle) },
                        multicam_cuts: c.multicam_cuts.clone(),
                        multicam_sound: Some(c.multicam_sound).filter(|s| !s.is_default()),
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
                        grade_versions,
                        active_grade_version,
                        track_points: c.track_points.clone(),
                        surfaces: c.surfaces.clone(),
                        surface_pin: c.surface_pin,
                        name: c.name.clone(),
                        colour_label: c.colour_label,
                        grade_flag: c.grade_flag,
                    }
                }).collect(),
            }
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
        // A layer with a stack of its own, a blend or a mask (#1513) is
        // the newest thing a title can carry, and takes the newest tag; a
        // path without one keeps its tag; a title with neither keeps the
        // oldest.
        ClipKind::Title(data) if data.objects.iter().any(|o| !o.fx().is_default() || o.blend != klipp_core::BlendMode::Normal || o.mask.is_some()) => {
            ClipKindJson::TitleV2 { title_data: title_data_for_file(data, project_dir) }
        }
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
            // Still and bare by the arm above, so these are the empty
            // stacks an older reader expects to find here.
            let fx = data.primary_fx();
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
                shadow: serde_json::to_value(fx.shadows.first()).unwrap_or_default(),
                shadows: serde_json::to_value(&fx.shadows).unwrap_or_default(),
                shadow_stack_enabled: fx.shadow_stack_enabled,
                // Single-outline JSON field is legacy; the canonical
                // list lives under `outlines`. Emit the first outline
                // as `outline` for compat with older readers, and the
                // full list under `outlines`.
                outline: serde_json::to_value(fx.outlines.first()).unwrap_or_default(),
                outlines: serde_json::to_value(&fx.outlines).unwrap_or_default(),
                outline_stack_enabled: fx.outline_stack_enabled,
                // Legacy single-glow field — emit glows[0] for
                // back-compat. Full stack lives under `glows`.
                glow: serde_json::to_value(fx.glows.first()).unwrap_or_default(),
                glows: serde_json::to_value(&fx.glows).unwrap_or_default(),
                glow_stack_enabled: fx.glow_stack_enabled,
                gradient: serde_json::to_value(&tb.gradient).unwrap_or_default(),
                // Legacy single-inner-shadow field — emit inner_shadows[0]
                // for back-compat with older readers; the full stack
                // lives under `inner_shadows`.
                inner_shadow: serde_json::to_value(fx.inner_shadows.first()).unwrap_or_default(),
                inner_shadows: serde_json::to_value(&fx.inner_shadows).unwrap_or_default(),
                inner_shadow_stack_enabled: fx.inner_shadow_stack_enabled,
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
                motion_blur: serde_json::to_value(&fx.motion_blur).unwrap_or_default(),
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
        ClipKind::TemplatePrecomposition { sequence_id, template_instance } => ClipKindJson::TemplatePrecomposition {
            sequence_id: sequence_id.to_string(),
            template_instance: template_instance.clone(),
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

/// Whether `a` and `b` would save the same: what a project's media
/// manifest holds for each. A file re-probed when its project opens, and
/// found as it was stored, leaves nothing to save (#1353), while what is
/// only in memory (whether the probe has run, its caches) doesn't count.
pub fn media_items_save_alike(a: &MediaItem, b: &MediaItem) -> bool {
    let dir = Path::new("");
    let saved = |item: &MediaItem| serde_json::to_value(media_item_to_json(item, dir)).ok();
    saved(a).is_some() && saved(a) == saved(b)
}

fn media_item_to_json(item: &MediaItem, project_dir: &Path) -> MediaItemJson {
    MediaItemJson {
        id: item.id.to_string(),
        kind: (item.kind != klipp_media::MediaItemKind::File).then(|| item.kind.clone()),
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
        log: item.log.clone(),
        markers: item.markers.clone(),
        synced_audio: item.synced_audio.clone(),
        span: item.span,
        screen_text: item.screen_text.clone(),
        audio_role: item.audio_role,
        shot: item.shot.clone(),
        truncated_from_secs: item.truncated_from_secs,
    }
}

/// Rotate backup files: project.json → project.json.bak.1, .bak.1 → .bak.2, etc.
/// Keeps up to `max_backups` versions. A step that fails is said in the
/// error, so a backup that was not made is not taken for one (#840).
fn rotate_backups(path: &Path, max_backups: u32) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Remove oldest backup if at limit
    let oldest = path.with_extension(format!("json.bak.{max_backups}"));
    if oldest.exists() {
        std::fs::remove_file(&oldest).map_err(|e| format!("the oldest backup of {name} could not be removed: {e}"))?;
    }

    // Shift existing backups: .bak.N-1 → .bak.N
    for i in (1..max_backups).rev() {
        let from = path.with_extension(format!("json.bak.{i}"));
        let to = path.with_extension(format!("json.bak.{}", i + 1));
        if from.exists() {
            std::fs::rename(&from, &to).map_err(|e| format!("backup {i} of {name} could not be moved along: {e}"))?;
        }
    }

    // Copy current file to .bak.1 atomically (read + atomic_write)
    let bak1 = path.with_extension("json.bak.1");
    let data = std::fs::read(path).map_err(|e| format!("{name} could not be read for its backup: {e}"))?;
    atomic_write(&bak1, &data).map_err(|e| format!("the backup of {name} could not be written: {e}"))
}

/// Save the project's Looks library to `<dir>/looks/{id}.json`. One
/// file per Look — keeps the diffs small when the user edits a single
/// Look in a project tracked by version control. Stale files (Looks
/// the user deleted in the session) are pruned on save so the on-disk
/// library matches in-memory state.
pub fn save_looks(dir: &Path, looks: &[Look]) -> Result<(), String> {
    save_looks_as(dir, looks, crate::FileForm::Revision2)
}

/// Today's v1 Look writer, kept only to prove the one-time conversion
/// (#1089), behind `v1-oracle`.
#[cfg(feature = "v1-oracle")]
pub fn save_looks_v1(dir: &Path, looks: &[Look]) -> Result<(), String> {
    save_looks_as(dir, looks, crate::FileForm::V1)
}

fn save_looks_as(dir: &Path, looks: &[Look], form: crate::FileForm) -> Result<(), String> {
    let looks_dir = dir.join("looks");
    if looks.is_empty() {
        // Nothing to save — leave an empty looks/ directory if it
        // already exists, otherwise skip the mkdir entirely.
        if looks_dir.exists() {
            prune_stale_looks(&looks_dir, &[], form);
        }
        return Ok(());
    }
    std::fs::create_dir_all(&looks_dir)
        .map_err(|e| format!("Failed to create looks dir: {e}"))?;
    // Every Look is written, and every failure reported (#1135): one that
    // can't be saved used to stop the rest unwritten. A failed Look keeps
    // its file from the last save, not pruned below.
    let mut failed = Vec::new();
    for look in looks {
        if let Err(e) = write_look(&looks_dir, look, form) {
            failed.push(e);
        }
    }
    let kept_ids: Vec<String> = looks.iter().map(|l| l.id.to_string()).collect();
    prune_stale_looks(&looks_dir, &kept_ids, form);
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

/// Write one Look's file in `form`.
fn write_look(looks_dir: &Path, look: &Look, form: crate::FileForm) -> Result<(), String> {
    {
        let json = match form {
            // Through text, as the project: an f32 keeps today's digits.
            crate::FileForm::Revision2 => {
                let text = serde_json::to_string(look).map_err(|e| format!("Failed to serialize Look {}: {e}", look.name))?;
                let v1: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Failed to serialize Look {}: {e}", look.name))?;
                let (v2, notes) = klipp_graph_lift::revision2::look_noted(&v1).map_err(|e| format!("Failed to write Look {}'s revision-2 file: {e}", look.name))?;
                if let Some(n) = notes.iter().find(|n| klipp_graph_lift::revision2::is_lost(n)) {
                    return Err(format!("Saving Look {} would lose part of it ({n:?}), so it was not saved", look.name));
                }
                serde_json::to_string_pretty(&v2).map_err(|e| format!("Failed to serialize Look {}: {e}", look.name))?
            }
            #[cfg(feature = "v1-oracle")]
            crate::FileForm::V1 => serde_json::to_string_pretty(look).map_err(|e| format!("Failed to serialize Look {}: {e}", look.name))?,
        };
        let path = looks_dir.join(format!("{}.json", look.id));
        atomic_write(&path, json.as_bytes())
            .map_err(|e| format!("Failed to write Look {}: {e}", look.name))?;
    }
    Ok(())
}

/// Remove `looks/*.json` files whose UUID isn't in `kept_ids`. Deleted
/// Looks shouldn't reappear after the next save just because their
/// file lingers on disk.
fn prune_stale_looks(looks_dir: &Path, kept_ids: &[String], form: crate::FileForm) {
    let Ok(entries) = std::fs::read_dir(looks_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") { continue; }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if !kept_ids.iter().any(|id| id == stem) {
            // A Look this build could not read was never loaded, so it is
            // not among the kept ids: that is not a reason to delete it
            // (#993), nor is a Look from before revision 2 (#1089). Only
            // what a load would read is pruned: the same test.
            let readable = crate::path_util::read_to_string_capped(&path, crate::path_util::MAX_INPUT_FILE_BYTES)
                .is_ok_and(|t| crate::load::read_look(&t, form).is_ok());
            if !readable {
                warn!("Keeping Look {:?}: this Klipp cannot read it in full, so it is not removed", path);
                continue;
            }
            if let Err(e) = std::fs::remove_file(&path) {
                warn!("Failed to remove stale look {:?}: {e}", path);
            }
        }
    }
}

/// A project as its revision-2 file (#1089): today's model as its v1 JSON,
/// then the conversion. The value is built from text, not `to_value`, which
/// widens an f32 field to f64 and changes the digits today's writer prints.
/// A conversion that would lose something refuses the save rather than
/// write a changed file.
fn revision2_text(project: &ProjectJson, sequences: &[Sequence]) -> Result<String, String> {
    // Each clip's picture as the clip keeps it lifted (#1089, M1b 2b): as
    // read from the file if the clip hasn't changed, so it is written back
    // exactly, otherwise lifted now. A clip that can't be lifted refuses
    // the save, by name.
    // By sequence and clip: a duplicated sequence keeps its clips' ids. A
    // clip id twice in one sequence takes no cached picture (each is
    // lifted from its own record) rather than one taking the other's.
    let mut pictures = std::collections::HashMap::new();
    let mut repeated = std::collections::HashSet::new();
    for seq in sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                let picture = clip.picture_v2().map_err(|e| {
                    format!("Saving would lose part of clip {} on track \"{}\" ({e}), so it was not saved", clip.id.0, track.name)
                })?;
                let key = (seq.id.to_string(), clip.id.0.to_string());
                if pictures.insert(key.clone(), picture).is_some() {
                    repeated.insert(key);
                }
            }
        }
    }
    for key in &repeated {
        pictures.remove(key);
    }
    let stored = |seq: &str, id: &str| pictures.get(&(seq.to_string(), id.to_string())).map(|p| (p.picture.clone(), p.versions.clone()));
    let text = serde_json::to_string(project).map_err(|e| format!("Failed to serialize project: {e}"))?;
    let mut v1: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Failed to serialize project: {e}"))?;
    if let Some(o) = v1.as_object_mut() {
        o.remove("format_revision");
    }
    let (mut v2, notes) = klipp_graph_lift::revision2::project_with(&v1, &stored).map_err(|e| format!("Failed to write the project's revision-2 file: {e}"))?;
    if let Some(n) = notes.iter().find(|n| klipp_graph_lift::revision2::is_lost(n)) {
        return Err(format!("Saving would lose part of the project ({n:?}), so it was not saved"));
    }
    // A sequence grade's mix and mask channel (#1321): its v1 graph leaves
    // them out, so they go into its stored body here.
    klipp_graph_lift::sequence_grade_mix::write_project(&mut v2, &|sid, root| sequences.iter().find(|s| s.id.0 == sid).map(|s| s.root_blends(root)).unwrap_or_default())
        .map_err(|e| format!("Saving would lose a sequence grade's mix ({e}), so it was not saved"))?;
    // A clip grade version's too (#1322).
    let version_blends = |cid: uuid::Uuid, n: usize| sequences.iter().flat_map(|s| s.all_clips()).find(|c| c.id.0 == cid).map(|c| c.version_blends(n)).unwrap_or_default();
    klipp_graph_lift::sequence_grade_mix::write_versions(&mut v2, &version_blends)
        .map_err(|e| format!("Saving would lose a grade version's mix ({e}), so it was not saved"))?;
    serde_json::to_string_pretty(&v2).map_err(|e| format!("Failed to serialize project: {e}"))
}

#[cfg(test)]
mod save_alike_tests {
    use super::media_items_save_alike;
    use klipp_media::MediaItem;

    fn item() -> MediaItem {
        MediaItem::synthetic_for_test(std::path::PathBuf::from("/media/a.mp4"), None)
    }

    #[test]
    fn a_reprobe_that_finds_what_was_stored_saves_alike() {
        let stored = item();
        let mut probed = stored.clone();
        // What only the open's re-probe changes: whether it has run.
        probed.metadata_read = !stored.metadata_read;
        probed.needs_reprobe = !stored.needs_reprobe;
        assert!(media_items_save_alike(&stored, &probed));
    }

    #[test]
    fn a_file_changed_while_closed_or_new_metadata_saves_differently() {
        let stored = item();
        let mut touched = stored.clone();
        touched.file_modified_at = stored.file_modified_at.map(|t| t + 1).or(Some(1));
        assert!(!media_items_save_alike(&stored, &touched), "the file changed");
        let mut read = stored.clone();
        read.shot.reel_name = Some("A001".into());
        assert!(!media_items_save_alike(&stored, &read), "the probe found a reel");
    }
}
