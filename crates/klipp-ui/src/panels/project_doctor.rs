//! Project Doctor — read-only scan of the loaded `.klipp` directory
//! for problems that today only surface as load-time errors or
//! silent missing-asset behaviour.
//!
//! v1 scope: detect-and-report. No auto-fix yet — surfacing issues
//! is the first reliability win; fix actions land iteration by
//! iteration.
//!
//! Categories of checks:
//!   * MISSING MEDIA — `MediaItem.path` doesn't exist on disk
//!   * MISSING AI MASK — clip has an `ExternalMatte` node whose PNG
//!     is gone from the cache (rare; happens when the mask cache is
//!     evicted before the project is reopened)
//!   * MISSING LADSPA PLUGIN — clip's audio chain references a
//!     `ladspa:{path}:{label}` plugin that's no longer installed
//!   * DISABLED LADSPA PLUGIN IN USE — plugin is installed but
//!     user-disabled in audio_plugins.toml; the existing clip still
//!     loads it (non-destructive disable semantic) but the user
//!     should know they're using a plugin they marked off-list

use klipp_timeline::GradeAccess;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId, MediaId};
use std::path::PathBuf;

/// Severity for a Project Doctor finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Project will not work as intended without resolving — missing
    /// media, missing plugins.
    Error,
    /// Project works but in a degraded or surprising state.
    Warning,
    /// Informational — surfaces a fact the user might want to know.
    #[allow(dead_code)]
    Info,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub category: String,
    pub summary: String,
    pub detail: String,
    /// Per-finding fix actions rendered as buttons next to the
    /// detail text. v1 had none; v2 adds them per category.
    /// Multiple fixes are allowed (e.g. DISABLED LADSPA IN USE
    /// offers both "Re-enable" and "Remove from clip").
    pub fixes: Vec<Fix>,
}

#[derive(Debug, Clone)]
pub struct Fix {
    pub label: String,
    pub action: FixAction,
}

/// Typed actions a Project Doctor fix button can perform. Most
/// dispatch an existing `AppCommand` directly; `RelinkMedia` is
/// special because it needs a file picker and runs synchronously
/// from the dialog UI thread (rfd is not async-safe in the
/// general case but Klipp uses it sync everywhere).
#[derive(Debug, Clone)]
pub enum FixAction {
    RelinkMedia {
        media_id: MediaId,
        original_path: PathBuf,
    },
    RemoveAudioEffectFromClip {
        clip_id: ClipId,
        effect_index: usize,
    },
    /// Strip a single `BuiltinEffect` entry from a clip's video chain.
    /// Used by the missing-frei0r-plugin finding to give users a
    /// one-click way to remove the dangling reference without
    /// reaching for the inspector.
    RemoveBuiltinEffectFromClip {
        clip_id: ClipId,
        effect_index: usize,
        /// The entry's picture node, as the scan found it: the fix removes
        /// that node by path (M3 step 19), and is refused if the node is
        /// gone by then, rather than removing whatever sits at
        /// `effect_index` now. `None` only for a list the picture doesn't
        /// name, removed by its place.
        node: Option<uuid::Uuid>,
    },
    ReEnableLadspaPlugin {
        unique_id: u64,
    },
    /// Pick the project's global LUT again, from where it was (#1019).
    RelinkGlobalLut {
        original_path: PathBuf,
    },
}

/// Render the Project Doctor modal. The scan runs every frame the
/// dialog is open — cheap because all data is already in memory
/// (state.media_pool, state.sequences, state.disabled_audio_plugins).
/// stat()-ing media paths is the only I/O and it's bounded by the
/// project's media count.
/// The frei0r plug-ins `sequences` use that no installed plug-in is, by
/// their ids, found as a render finds them (#3200): for saying so when a
/// project opens, rather than leaving the effects to pass the picture on.
pub(crate) fn missing_frei0r_plugins(sequences: &[klipp_timeline::Sequence]) -> Vec<String> {
    let mut missing = std::collections::BTreeSet::new();
    for clip in sequences.iter().flat_map(|s| s.all_clips()) {
        let Ok(effects) = clip.picture_effects() else { continue };
        for fx in effects {
            if let klipp_effects::BuiltinEffect::Frei0r { type_id, .. } = &fx.effect {
                if klipp_effects::frei0r_effect::lookup(type_id).is_none() {
                    missing.insert(type_id.clone());
                }
            }
        }
    }
    missing.into_iter().collect()
}

/// What opening a project says of its missing frei0r plug-ins, each by its
/// name; `None` when none is.
pub(crate) fn missing_frei0r_message(missing: &[String]) -> Option<String> {
    let names: Vec<&str> = missing.iter().map(|id| id.rsplit(':').next().unwrap_or(id)).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!("The frei0r plug-in {one} isn't installed here: its effect leaves the picture unchanged. Project Doctor lists it.")),
        many => Some(format!("{} frei0r plug-ins aren't installed here ({}): their effects leave the picture unchanged. Project Doctor lists them.", many.len(), many.join(", "))),
    }
}

pub fn show_project_doctor_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
) {
    if !state.show_project_doctor {
        return;
    }

    let screen_rect = ctx.screen_rect();
    let max_h = (screen_rect.height() * 0.85).clamp(400.0, 720.0);
    let max_w = (screen_rect.width() * 0.70).clamp(560.0, 800.0);
    let default_w = (screen_rect.width() * 0.55).clamp(560.0, max_w);
    let default_h = (screen_rect.height() * 0.65).clamp(400.0, max_h);
    let centre = screen_rect.center();
    let default_pos = egui::pos2(
        centre.x - default_w * 0.5,
        centre.y - default_h * 0.5,
    );

    let findings = scan_project(state);
    let mut open = true;
    egui::Window::new("Project Doctor")
        .open(&mut open)
        .resizable(true)
        .movable(true)
        .default_size([default_w, default_h])
        .default_pos(default_pos)
        .min_width(480.0)
        .min_height(360.0)
        .max_width(max_w)
        .max_height(max_h)
        .show(ctx, |ui| {
            render_body(ui, theme, &findings, cmds, max_h);
        });

    if !open {
        state.show_project_doctor = false;
    }
}

fn render_body(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    findings: &[Finding],
    cmds: &mut Vec<AppCommand>,
    max_h: f32,
) {
    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();
    let infos = findings.iter().filter(|f| f.severity == Severity::Info).count();

    // ── Header ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(if findings.is_empty() {
                "No issues detected. Project looks healthy.".to_string()
            } else {
                let mut parts = Vec::new();
                if errors > 0 {
                    parts.push(format!("{errors} error{}", if errors == 1 { "" } else { "s" }));
                }
                if warnings > 0 {
                    parts.push(format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" }));
                }
                if infos > 0 {
                    parts.push(format!("{infos} info", ));
                }
                format!("{} issue{} found: {}",
                    findings.len(),
                    if findings.len() == 1 { "" } else { "s" },
                    parts.join(", "))
            })
            .size(11.0)
            .strong()
            .color(if errors > 0 {
                theme.marker_error
            } else if warnings > 0 {
                theme.accent
            } else {
                theme.text_primary
            }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Re-scan").clicked() {
                // Re-render flips the closure; nothing to do — scan
                // runs every frame anyway. Button kept for users
                // who want to confirm "yes I just fixed something
                // and want to see the new state".
            }
            if ui.button("Copy report").on_hover_text(
                "Copy the full findings list to the clipboard as plain text."
            ).clicked() {
                ui.ctx().copy_text(report_as_text(findings));
            }
        });
    });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // ── Body ─────────────────────────────────────────────────────
    let other_budget = 100.0;
    let scroll_max_h = (max_h - other_budget).max(200.0);

    egui::ScrollArea::vertical()
        .max_height(scroll_max_h)
        .show(ui, |ui| {
            if findings.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("\u{2713}")
                            .size(32.0)
                            .color(theme.accent),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("All clear.")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                });
                return;
            }
            for finding in findings {
                render_finding(ui, theme, finding, cmds);
            }
        });

    // ── Footer ───────────────────────────────────────────────────
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(
                "Per-finding fix buttons dispatch existing AppCommands. Most fixes \
                 are immediate; Relink opens a file picker.",
            )
            .size(10.0)
            .color(theme.text_dim),
        );
    });
}

fn render_finding(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    finding: &Finding,
    cmds: &mut Vec<AppCommand>,
) {
    let (icon, icon_color) = match finding.severity {
        Severity::Error => ("\u{2716}", theme.marker_error),
        Severity::Warning => ("\u{26A0}", theme.accent),
        Severity::Info => ("\u{2139}", theme.text_secondary),
    };

    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .outer_margin(egui::Margin::symmetric(0.0, 2.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(icon)
                        .size(13.0)
                        .strong()
                        .color(icon_color),
                );
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&finding.summary)
                                .size(11.0)
                                .strong()
                                .color(theme.text_primary),
                        );
                        ui.label(
                            egui::RichText::new(&finding.category)
                                .size(10.0)
                                .strong()
                                .color(theme.text_dim),
                        );
                    });
                    ui.label(
                        egui::RichText::new(&finding.detail)
                            .size(10.0)
                            .color(theme.text_secondary),
                    );
                    if !finding.fixes.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            for fix in &finding.fixes {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(&fix.label).size(10.0),
                                        )
                                        .min_size(egui::vec2(80.0, 0.0)),
                                    )
                                    .clicked()
                                {
                                    apply_fix(&fix.action, cmds);
                                }
                            }
                        });
                    }
                });
            });
        });
}

/// Translate a typed `FixAction` into one or more `AppCommand`s.
/// The `RelinkMedia` arm opens a file picker off the UI thread (see
/// `file_dialog`); its relink command arrives on a later frame.
fn apply_fix(action: &FixAction, cmds: &mut Vec<AppCommand>) {
    match action {
        FixAction::RelinkMedia {
            media_id,
            original_path,
        } => {
            let start_dir = original_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let original_name = original_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "media file".to_string());
            let media_id = *media_id;
            crate::file_dialog::spawn(move || {
                rfd::FileDialog::new()
                    .set_title(format!("Relink: {original_name}"))
                    .set_directory(start_dir)
                    .pick_file()
                    .map(|new_path| vec![AppCommand::RelinkMediaByHand { media_id, new_path }])
                    .unwrap_or_default()
            });
        }
        FixAction::RemoveAudioEffectFromClip {
            clip_id,
            effect_index,
        } => {
            cmds.push(AppCommand::RemoveAudioEffect {
                clip_id: *clip_id,
                effect_index: *effect_index,
            });
        }
        FixAction::RemoveBuiltinEffectFromClip {
            clip_id,
            effect_index,
            node,
        } => {
            cmds.push(match node {
                Some(node) => AppCommand::RemoveNodeAt {
                    addr: klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip_id.0 }, [*node]),
                },
                None => AppCommand::RemoveBuiltinEffect {
                    clip_id: *clip_id,
                    index: *effect_index,
                },
            });
        }
        FixAction::ReEnableLadspaPlugin { unique_id } => {
            cmds.push(AppCommand::ToggleAudioPluginDisabled {
                unique_id: *unique_id,
            });
        }
        FixAction::RelinkGlobalLut { original_path } => {
            let start_dir = original_path
                .parent()
                .filter(|d| d.is_dir())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let original_name = original_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "LUT".to_string());
            crate::file_dialog::spawn(move || {
                rfd::FileDialog::new()
                    .set_title(format!("Relink LUT: {original_name}"))
                    .set_directory(start_dir)
                    .add_filter("LUT", &["cube", "3dl", "csp"])
                    .pick_file()
                    .map(|path| vec![AppCommand::LoadGlobalLut { path }])
                    .unwrap_or_default()
            });
        }
    }
}

// ─── Scan logic ───────────────────────────────────────────────────

fn scan_project(state: &AppState) -> Vec<Finding> {
    let mut findings = Vec::new();

    // 0. The project's global LUT, kept when its file is missing so it
    //    can be relinked rather than lost on the next save (#1019).
    if let Some(lut) = state.global_lut_path.as_ref().filter(|p| !p.exists()) {
        let name = lut.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| lut.to_string_lossy().into_owned());
        findings.push(Finding {
            severity: Severity::Error,
            category: "MISSING LUT".to_string(),
            summary: format!("The project's LUT {name} is missing"),
            detail: format!("{} is not on disk, so the timeline renders without it. Relink it to apply it again.", lut.display()),
            fixes: vec![Fix { label: "Relink…".into(), action: FixAction::RelinkGlobalLut { original_path: lut.clone() } }],
        });
    }

    // 1. Missing media — every MediaItem in the pool whose path
    //    doesn't exist on disk. This is the most common project
    //    breakage (drive moved, media folder renamed, etc.).
    for item in state.media_pool.all() {
        if !item.path.exists() {
            let display: String = item
                .path
                .file_name()
                .map(|s: &std::ffi::OsStr| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| item.path.to_string_lossy().into_owned());
            findings.push(Finding {
                severity: Severity::Error,
                category: "MISSING MEDIA".to_string(),
                summary: format!("{display} is offline"),
                detail: format!(
                    "Source path no longer exists: {}",
                    item.path.to_string_lossy()
                ),
                fixes: vec![Fix {
                    label: "Relink…".into(),
                    action: FixAction::RelinkMedia {
                        media_id: item.id,
                        original_path: item.path.clone(),
                    },
                }],
            });
        }
    }

    // 2. LADSPA plugin findings — walk every clip's audio_effects
    //    list and flag entries that reference a "ladspa:..." plugin
    //    that isn't currently in state.ladspa_plugins, OR is in
    //    state.ladspa_plugins but disabled.
    let installed_ladspa: std::collections::HashSet<String> = state
        .ladspa_plugins
        .iter()
        .map(|p| format!("{}:{}", p.path.to_string_lossy(), p.label))
        .collect();
    let disabled_by_pathlabel: std::collections::HashSet<String> = state
        .ladspa_plugins
        .iter()
        .filter(|p| state.disabled_audio_plugins.contains(&p.unique_id))
        .map(|p| format!("{}:{}", p.path.to_string_lossy(), p.label))
        .collect();

    // Map plugin "path:label" → unique_id so we can dispatch
    // ToggleAudioPluginDisabled from a fix button without re-doing
    // the lookup.
    let unique_id_by_pathlabel: std::collections::HashMap<String, u64> = state
        .ladspa_plugins
        .iter()
        .map(|p| (format!("{}:{}", p.path.to_string_lossy(), p.label), p.unique_id))
        .collect();

    let active = state.sequences.get(state.active_sequence_idx);
    if let Some(seq) = active {
        for track in &seq.tracks {
            for clip in &track.clips {
                for (effect_index, fx) in clip.audio_effects.iter().enumerate() {
                    if let Some(rest) = fx.effect_type.strip_prefix("ladspa:") {
                        if installed_ladspa.contains(rest) {
                            if disabled_by_pathlabel.contains(rest) {
                                let mut fixes = Vec::new();
                                if let Some(uid) = unique_id_by_pathlabel.get(rest) {
                                    fixes.push(Fix {
                                        label: "Re-enable plugin".into(),
                                        action: FixAction::ReEnableLadspaPlugin {
                                            unique_id: *uid,
                                        },
                                    });
                                }
                                fixes.push(Fix {
                                    label: "Remove from clip".into(),
                                    action: FixAction::RemoveAudioEffectFromClip {
                                        clip_id: clip.id,
                                        effect_index,
                                    },
                                });
                                findings.push(Finding {
                                    severity: Severity::Warning,
                                    category: "DISABLED LADSPA IN USE".into(),
                                    summary: format!(
                                        "Clip uses a disabled plugin: {rest}"
                                    ),
                                    detail: "The plugin is installed but you've marked it \
                                             disabled in Tools \u{2192} Audio Plugins. The \
                                             clip still loads it because the disable is \
                                             non-destructive, but new clips can't pick \
                                             it. Re-enable the plugin or remove the \
                                             effect from the clip."
                                        .into(),
                                    fixes,
                                });
                            }
                        } else {
                            findings.push(Finding {
                                severity: Severity::Error,
                                category: "MISSING LADSPA PLUGIN".into(),
                                summary: format!("Plugin not installed: {rest}"),
                                detail: "The clip has an audio effect referencing a \
                                         LADSPA plugin that's no longer on disk. \
                                         Install the plugin and rescan from Tools \
                                         \u{2192} Audio Plugins, or remove the effect \
                                         from the clip."
                                    .into(),
                                fixes: vec![Fix {
                                    label: "Remove from clip".into(),
                                    action: FixAction::RemoveAudioEffectFromClip {
                                        clip_id: clip.id,
                                        effect_index,
                                    },
                                }],
                            });
                        }
                    }
                }
            }
        }
    }

    // 2c. LV2 plugin findings — walk every clip's audio_effects list
    //     and flag entries that reference an `"lv2:{uri}"` plugin not
    //     present in the runtime registry. Mirrors the LADSPA shape
    //     except keyed on URI rather than path:label.
    let installed_lv2_uris: std::collections::HashSet<String> =
        klipp_audio::lv2::registry_snapshot()
            .into_iter()
            .map(|info| info.uri)
            .collect();

    if let Some(seq) = active {
        for track in &seq.tracks {
            for clip in &track.clips {
                for (effect_index, fx) in clip.audio_effects.iter().enumerate() {
                    let Some(uri) = fx.effect_type.strip_prefix("lv2:") else {
                        continue;
                    };
                    if installed_lv2_uris.contains(uri) {
                        continue;
                    }
                    findings.push(Finding {
                        severity: Severity::Error,
                        category: "MISSING LV2 PLUGIN".into(),
                        summary: format!("LV2 plugin not installed: {uri}"),
                        detail: "The clip has an audio effect referencing \
                                 an LV2 plugin that's no longer on disk. \
                                 Install the plugin into ~/.lv2 or \
                                 /usr/lib/lv2 and rescan, or remove the \
                                 effect from the clip."
                            .into(),
                        fixes: vec![Fix {
                            label: "Remove from clip".into(),
                            action: FixAction::RemoveAudioEffectFromClip {
                                clip_id: clip.id,
                                effect_index,
                            },
                        }],
                    });
                }
            }
        }
    }

    // 2d. CLAP plugin findings — same shape as LV2 but keyed on
    //     CLAP plugin ID.
    let installed_clap_ids: std::collections::HashSet<String> =
        klipp_audio::clap::registry_snapshot()
            .into_iter()
            .map(|info| info.id)
            .collect();

    if let Some(seq) = active {
        for track in &seq.tracks {
            for clip in &track.clips {
                for (effect_index, fx) in clip.audio_effects.iter().enumerate() {
                    let Some(plugin_id) = fx.effect_type.strip_prefix("clap:") else {
                        continue;
                    };
                    if installed_clap_ids.contains(plugin_id) {
                        continue;
                    }
                    findings.push(Finding {
                        severity: Severity::Error,
                        category: "MISSING CLAP PLUGIN".into(),
                        summary: format!("CLAP plugin not installed: {plugin_id}"),
                        detail: "The clip has an audio effect referencing \
                                 a CLAP plugin that's no longer on disk. \
                                 Install the plugin into ~/.clap or \
                                 /usr/lib/clap and rescan, or remove the \
                                 effect from the clip."
                            .into(),
                        fixes: vec![Fix {
                            label: "Remove from clip".into(),
                            action: FixAction::RemoveAudioEffectFromClip {
                                clip_id: clip.id,
                                effect_index,
                            },
                        }],
                    });
                }
            }
        }
    }

    // 2b. Frei0r plugin findings — walk every clip's builtin_effects
    //     list and flag `BuiltinEffect::Frei0r { type_id, .. }`
    //     entries whose plugin isn't currently installed. We use the
    //     runtime registry (klipp_effects::frei0r_effect::lookup)
    //     rather than the on-disk cache so the result reflects what
    //     the user can actually run right now, including changes
    //     since the last Rescan.
    if let Some(seq) = active {
        for track in &seq.tracks {
            for clip in &track.clips {
                // The clip's effects as its picture holds them (M4 step
                // 12b), each with its node, so the fix removes it by path.
                let Ok(effects) = clip.picture_effects() else { continue };
                for (effect_index, fx) in effects.iter().enumerate() {
                    if let klipp_effects::BuiltinEffect::Frei0r { type_id, .. } = &fx.effect {
                        // Found as a render finds it: by its file and its
                        // name but for case wherever installed (#3200).
                        if klipp_effects::frei0r_effect::lookup(type_id).is_some() {
                            continue;
                        }
                        findings.push(Finding {
                            severity: Severity::Error,
                            category: "MISSING FREI0R PLUGIN".into(),
                            summary: format!("Plugin not installed: {type_id}"),
                            detail: "The clip has a video effect referencing a \
                                     frei0r plugin that's no longer on disk. \
                                     Install the plugin into /usr/lib/frei0r-1 \
                                     (or ~/.frei0r-1/lib) and use Tools \u{2192} \
                                     Plugins \u{2192} Rescan, or remove the \
                                     effect from the clip."
                                .into(),
                            fixes: vec![Fix {
                                label: "Remove from clip".into(),
                                action: FixAction::RemoveBuiltinEffectFromClip {
                                    clip_id: clip.id,
                                    effect_index,
                                    node: Some(fx.node),
                                },
                            }],
                        });
                    }
                }
            }
        }
    }

    // 3. Missing AI mask files — every ExternalMatte node whose
    //    matte_image_path has been evicted from ~/.cache/klipp/ai_masks
    //    (the cache is LRU-pruned to 2 GB on app start).
    if let Some(seq) = active {
        for track in &seq.tracks {
            for clip in &track.clips {
                for node in &clip.grade().nodes {
                    if let klipp_nodes::NodeKind::ExternalMatte = node.kind {
                        if let Some(path) = node.params.matte_image_path.as_ref() {
                            if !path.exists() {
                                let display: String = path
                                    .file_name()
                                    .map(|s: &std::ffi::OsStr| {
                                        s.to_string_lossy().into_owned()
                                    })
                                    .unwrap_or_else(|| {
                                        path.to_string_lossy().into_owned()
                                    });
                                findings.push(Finding {
                                    severity: Severity::Warning,
                                    category: "MISSING AI MASK".into(),
                                    summary: format!(
                                        "Mask cache file missing: {display}"
                                    ),
                                    detail: format!(
                                        "An AI-generated matte was evicted from the \
                                         2 GB mask cache before the project was \
                                         reopened. The clip's mask falls back to \
                                         pass-through (no mask). Re-run the AI tool \
                                         on this clip to regenerate. Path: {}",
                                        path.display()
                                    ),
                                    // No fix action for missing AI masks: we'd
                                    // need a node-graph mutation command (drop
                                    // the ExternalMatte node) that doesn't
                                    // exist yet; meanwhile the user can re-run
                                    // the AI tool which overwrites the dead
                                    // matte path. Detail text already explains.
                                    fixes: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    findings
}

fn report_as_text(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "Klipp Project Doctor\n====================\n\nNo issues detected.\n".into();
    }
    let mut out = String::new();
    out.push_str("Klipp Project Doctor\n====================\n\n");
    out.push_str(&format!("{} issue(s) found:\n\n", findings.len()));
    for f in findings {
        let sev = match f.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
            Severity::Info => "INFO ",
        };
        out.push_str(&format!("[{}] [{}] {}\n", sev, f.category, f.summary));
        out.push_str(&format!("    {}\n\n", f.detail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing frei0r plug-in's fix removes the node
    /// the scan found, by path, one step: the right one even when the list
    /// moved under it since; and once that node is gone the fix is
    /// refused, changing nothing, rather than removing what sits at its
    /// old place now (M3 step 19).
    #[test]
    fn a_missing_plugin_is_removed_by_its_node() {
        use klipp_effects::BuiltinEffect as E;
        use klipp_timeline::test_clip::{self, TestClip};
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let clip = TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effect(E::GaussianBlur { radius: 3.0 })
            .effect(E::Frei0r { type_id: "frei0r:klipp-test-not-installed".into(), params: Vec::new() })
            .build();
        let id = clip.id;
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.push(clip);
        let fix = scan_project(&state).into_iter().flat_map(|f| f.fixes).find_map(|f| match f.action {
            a @ FixAction::RemoveBuiltinEffectFromClip { .. } => Some(a),
            _ => None,
        }).expect("the missing plug-in's fix");
        let effects = |s: &AppState| test_clip::effects(s.sequences[0].all_clips().find(|c| c.id == id).unwrap());
        // An effect put before it since the scan: the plug-in is now third.
        let first = state.sequences[0].all_clips().find(|c| c.id == id).unwrap().effect_node_at(false, 0).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({ "id": uuid::Uuid::new_v4(), "type_id": klipp_graph_lift::names::effect_node_type("sharpen"), "legacy": { "payload": E::Sharpen { amount: 0.5 } } })).unwrap();
        let add = AppCommand::AddNodeAt { after: klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: id.0 }, [first]), node: Box::new(node) };
        assert!(state.path_command(&add).is_some());
        let steps = state.active_history().undo_count();
        let mut cmds = Vec::new();
        apply_fix(&fix, &mut cmds);
        assert!(matches!(cmds.as_slice(), [AppCommand::RemoveNodeAt { .. }]), "{cmds:?}");
        assert!(state.path_command(&cmds[0]).is_some());
        assert_eq!(serde_json::to_value(effects(&state)).unwrap(), serde_json::to_value([E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]).unwrap(), "the plug-in removed, the effect put before it kept");
        assert_eq!(state.active_history().undo_count(), steps + 1);
        // Its node gone, the same fix again changes nothing.
        let now = effects(&state);
        state.path_command(&cmds[0]);
        assert_eq!(serde_json::to_value(effects(&state)).unwrap(), serde_json::to_value(now).unwrap());
        assert_eq!(state.active_history().undo_count(), steps + 1);
        assert!(state.toast.message_for_test().is_some(), "the refusal is said");
    }

    /// #3200: a project made where frei0r lives in another folder, or
    /// with a plug-in's name in another case, opens with its plug-ins
    /// found; one this machine hasn't is named when it opens, once
    /// however many clips use it.
    #[test]
    fn a_project_from_another_machine_names_only_the_plug_ins_missing_here() {
        use klipp_effects::BuiltinEffect as E;
        use klipp_timeline::test_clip::TestClip;
        klipp_effects::frei0r_effect::_register_named_for_test("/usr/lib/frei0r-1/klipp3200here.so", "Klipp3200Here");
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let with = |type_id: &str| TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(E::Frei0r { type_id: type_id.into(), params: Vec::new() }).build();
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        // As saved on a machine with frei0r in /usr/lib64 and in ~/.frei0r-1.
        state.sequences[0].tracks[v1].clips.extend([
            with("frei0r:/usr/lib64/frei0r-1/klipp3200here.so:klipp3200here"),
            with("frei0r:/home/them/.frei0r-1/lib/klipp3200gone.so:Klipp3200Gone"),
            with("frei0r:/home/them/.frei0r-1/lib/klipp3200gone.so:Klipp3200Gone"),
        ]);
        let missing = missing_frei0r_plugins(&state.sequences);
        assert_eq!(missing, ["frei0r:/home/them/.frei0r-1/lib/klipp3200gone.so:Klipp3200Gone"], "the one here found, the missing one once");
        let said = missing_frei0r_message(&missing).expect("said on open");
        assert!(said.contains("Klipp3200Gone") && said.contains("Project Doctor"), "{said}");
        assert_eq!(missing_frei0r_message(&[]), None);
        assert_eq!(scan_project(&state).iter().filter(|f| f.category == "MISSING FREI0R PLUGIN").count(), 2, "the doctor lists the missing one's two effects, not the one found here");
    }

    fn err(cat: &str) -> Finding {
        Finding {
            severity: Severity::Error,
            category: cat.into(),
            summary: format!("issue in {cat}"),
            detail: "details".into(),
            fixes: Vec::new(),
        }
    }

    #[test]
    fn report_text_empty_says_no_issues() {
        let s = report_as_text(&[]);
        assert!(s.contains("No issues detected"));
    }

    #[test]
    fn apply_fix_remove_audio_effect_dispatches_command() {
        let mut cmds = Vec::new();
        let clip_id = ClipId::new();
        apply_fix(
            &FixAction::RemoveAudioEffectFromClip {
                clip_id,
                effect_index: 3,
            },
            &mut cmds,
        );
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AppCommand::RemoveAudioEffect {
                clip_id: c,
                effect_index,
            } => {
                assert_eq!(*c, clip_id);
                assert_eq!(*effect_index, 3);
            }
            other => panic!("expected RemoveAudioEffect, got {other:?}"),
        }
    }

    #[test]
    fn apply_fix_reenable_plugin_dispatches_toggle() {
        let mut cmds = Vec::new();
        apply_fix(
            &FixAction::ReEnableLadspaPlugin { unique_id: 12345 },
            &mut cmds,
        );
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            AppCommand::ToggleAudioPluginDisabled { unique_id } => {
                assert_eq!(*unique_id, 12345);
            }
            other => panic!("expected ToggleAudioPluginDisabled, got {other:?}"),
        }
    }

    #[test]
    fn report_text_lists_findings_with_severity_tags() {
        let findings = vec![
            err("MISSING MEDIA"),
            Finding {
                severity: Severity::Warning,
                category: "DISABLED LADSPA IN USE".into(),
                summary: "warn summary".into(),
                detail: "warn detail".into(),
                fixes: Vec::new(),
            },
        ];
        let s = report_as_text(&findings);
        assert!(s.contains("[ERROR]"));
        assert!(s.contains("[WARN ]"));
        assert!(s.contains("MISSING MEDIA"));
        assert!(s.contains("DISABLED LADSPA IN USE"));
        assert!(s.contains("2 issue(s) found"));
    }
}
