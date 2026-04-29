//! AI Models management dialog.
//!
//! One row per entry in the manifest:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ AI Models                                          [✕]      │
//! ├─────────────────────────────────────────────────────────────┤
//! │ ▢  BiRefNet Lite             [Installed]  [Reinstall][⌫]    │
//! │    Remove Background • MIT • 214 MB                         │
//! │    huggingface.co/onnx-community/BiRefNet_lite-ONNX         │
//! ├─────────────────────────────────────────────────────────────┤
//! │ ▢  SAM2.1 Tiny               [Not Installed]  [Download]    │
//! │    Mask Generator • Apache-2.0 • 155 MB                     │
//! │    Meta AI Research                                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! Status badges:
//! * **Installed** — the file exists at the expected cache path.
//!   We don't re-verify the SHA on every dialog open (a 900 MB
//!   model takes ~2-3s to hash); the consent dialog + downloader
//!   already verify on download, and a "Verify now" affordance
//!   triggers an explicit SHA check when needed.
//! * **Not installed** — file is missing from the cache.
//! * **Mismatch** — file exists but its SHA didn't match on the
//!   last verify. Surfaced with a warning colour and a Reinstall
//!   action.
//!
//! Action buttons fire the same `model_download` flow the command
//! palette already uses: clicking Download flips
//! `state.model_download` to `Consenting { entry }` and the existing
//! consent dialog takes over from there.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use klipp_ml::manifest::ModelEntry;
use klipp_ml::ModelCache;

use crate::ai_tools::{format_relative, ModelTelemetry};
use crate::model_download::ModelDownloadState;
use crate::state::AppState;
use crate::theme::KlippTheme;

/// Threshold for "Uninstall unused". A model whose last successful
/// run was more than this long ago counts as stale. 30 days mirrors
/// what the F-97-MODEL-MANAGER spec calls out and is long enough to
/// avoid false positives across a normal vacation.
const STALE_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Per-row cached status. Decoupled from
/// `klipp_ml::cache::CachedStatus` so the dialog can carry a
/// "FilePresent (size in bytes, but unverified)" state without
/// pretending it's verified.
#[derive(Debug, Clone)]
pub enum ModelInstallStatus {
    /// File exists at the cache path. We haven't re-verified the
    /// SHA-256 on this dialog open, but the downloader verifies on
    /// download and on its own re-verifies after every byte. So
    /// "present" is "trusted" until the user clicks Verify.
    Present { size_bytes: u64 },
    /// File missing from the cache. Caller should offer a Download
    /// action.
    Missing,
    /// File present but its SHA didn't match on the last verify.
    Mismatch,
    /// Manifest entry exists but the download URL or expected size
    /// is still a placeholder (size_bytes = 0). The release-CI
    /// mirror hasn't populated this entry yet, so install actions
    /// are blocked. Visible in the dialog so users can see what's
    /// roadmapped without being able to break anything.
    Pending,
    /// Status couldn't be determined (cache open failed). Treated
    /// as Missing for action purposes but flagged so the user
    /// understands why.
    Unknown(String),
}

impl ModelInstallStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Present { .. } => "Installed",
            Self::Missing => "Not installed",
            Self::Mismatch => "Mismatch",
            Self::Pending => "Pending release",
            Self::Unknown(_) => "Unknown",
        }
    }

    pub fn badge_color(&self, theme: &KlippTheme) -> egui::Color32 {
        match self {
            Self::Present { .. } => theme.marker_success,
            Self::Missing => theme.text_dim,
            Self::Mismatch => theme.marker_warning,
            Self::Pending => theme.marker_info,
            Self::Unknown(_) => theme.text_dim,
        }
    }
}

/// `true` when the entry's telemetry says the model has been used
/// at least once AND the last use was more than [`STALE_AGE`] ago.
/// Models that have never been used are intentionally not counted —
/// the user might have just installed them.
fn is_stale(entry: &ModelEntry, telemetry: &HashMap<String, ModelTelemetry>) -> bool {
    let Some(t) = telemetry.get(&entry.id) else {
        return false;
    };
    if !t.has_runs() {
        return false;
    }
    let now = SystemTime::now();
    now.duration_since(t.last_used)
        .map(|elapsed| elapsed > STALE_AGE)
        .unwrap_or(false)
}

/// Compute install status for a single entry by hitting the
/// filesystem. Cheap: stat-only, no hash compute.
fn install_status_for(cache: &ModelCache, entry: &ModelEntry) -> ModelInstallStatus {
    let path = cache.path_for(entry);
    match path.metadata() {
        Ok(m) => ModelInstallStatus::Present {
            size_bytes: m.len(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ModelInstallStatus::Missing,
        Err(e) => ModelInstallStatus::Unknown(e.to_string()),
    }
}

/// Render the AI Models dialog. No-op when
/// `state.show_ai_models_dialog` is false.
pub fn show_ai_models_dialog(ctx: &egui::Context, state: &mut AppState, theme: &KlippTheme) {
    if !state.show_ai_models_dialog {
        return;
    }

    let manifest = match klipp_ml::manifest::load_effective() {
        Ok(m) => m,
        Err(e) => {
            // Show the failure to the user — they can't do anything
            // useful but at least know why the dialog is empty.
            let mut open = state.show_ai_models_dialog;
            egui::Window::new("AI Models")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.colored_label(
                        theme.marker_error,
                        format!("Could not load model manifest: {e}"),
                    );
                });
            state.show_ai_models_dialog = open;
            return;
        }
    };

    let cache = ModelCache::open().ok();

    // Total disk usage across every installed manifest entry. Computed
    // here (cheap stat per file) rather than re-walked every frame —
    // close + reopen of the dialog gets a fresh number.
    let total_cache_bytes: u64 = match cache.as_ref() {
        Some(c) => manifest
            .models
            .iter()
            .filter_map(|e| std::fs::metadata(c.path_for(e)).ok())
            .map(|m| m.len())
            .sum(),
        None => 0,
    };

    let mut open = state.show_ai_models_dialog;
    let mut close_requested = false;

    // Cap the window at 80% of the screen height so a long manifest
    // can't grow the dialog past the bottom edge — the ScrollArea
    // below absorbs the overflow instead. Width fixed for stable
    // row layout.
    let screen_rect = ctx.screen_rect();
    let max_h = (screen_rect.height() * 0.8).clamp(360.0, 720.0);
    let scroll_max_h = max_h - 110.0; // header + footer + padding

    // Modal scrim — full-screen Area that absorbs every pointer + key
    // event so the timeline / viewer / browser underneath can't be
    // skimmed, scrubbed, or selected while the dialog is up. The
    // Window below shows on a higher Order so it still receives input.
    egui::Area::new(egui::Id::new("ai_models_scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            // Translucent darken so the user's eye reads "this layer
            // is inactive". Theme-agnostic black at low alpha works on
            // both polarities.
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(140),
            );
            // Click on the scrim outside the dialog dismisses it —
            // matches the file-picker / context-menu convention.
            if resp.clicked() {
                close_requested = true;
            }
        });

    egui::Window::new("AI Models")
        .collapsible(false)
        .resizable(false)
        .fixed_size([620.0, max_h])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    "Local AI models. Downloads are explicit; no bytes hit your disk \
                     without your confirmation. Cache lives at \
                     ~/.local/share/klipp/models/.",
                )
                .small()
                .color(theme.text_secondary),
            );
            ui.add_space(6.0);
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .max_height(scroll_max_h)
                .show(ui, |ui| {
                    if manifest.models.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("No models declared in manifest.")
                                    .color(theme.text_dim),
                            );
                        });
                    }
                    for entry in &manifest.models {
                        let status = if entry.size_bytes == 0 {
                            // Placeholder entry — the release-CI
                            // mirror hasn't filled in size + sha256
                            // yet. Show as Pending instead of hiding;
                            // gives users visibility into the roadmap.
                            ModelInstallStatus::Pending
                        } else {
                            match &cache {
                                Some(c) => install_status_for(c, entry),
                                None => ModelInstallStatus::Unknown(
                                    "cache open failed".into(),
                                ),
                            }
                        };
                        render_row(
                            ui,
                            entry,
                            &status,
                            theme,
                            &mut state.model_download,
                            &state.ai_tools_state.model_telemetry,
                            &mut state.show_ai_model_uninstall_confirm,
                            &mut state.toast,
                        );
                        ui.separator();
                    }

                    // ── User-added custom models ────────────────────
                    // Rendered as a distinct section after the
                    // manifest entries so users can see at a glance
                    // which models were pasted vs. which Klipp
                    // ships. Empty state: no header, nothing
                    // rendered — the AI Models dialog stays clean
                    // for users who haven't added anything custom.
                    if !state.ai_tools_state.custom_models.is_empty() {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Custom models")
                                .strong()
                                .color(theme.text_secondary),
                        );
                        ui.add_space(2.0);
                        ui.separator();
                        // Clone before iter so we can mutate
                        // pending_custom_uninstall + telemetry inside
                        // the loop without aliasing state.
                        let entries = state.ai_tools_state.custom_models.clone();
                        for entry in &entries {
                            render_custom_row(
                                ui,
                                entry,
                                theme,
                                &state.ai_tools_state.model_telemetry,
                                &mut state.show_ai_custom_uninstall_confirm,
                                &mut state.toast,
                                cache.as_ref(),
                            );
                            ui.separator();
                        }
                    }
                });

            ui.add_space(6.0);
            ui.separator();
            // Count installed entries once for the bulk-action button
            // gating. Cheap stat — already done above for the cache
            // size readout, but we need the entry list (not just the
            // total) for "Uninstall all" / "Verify all".
            let installed_count = match cache.as_ref() {
                Some(c) => manifest
                    .models
                    .iter()
                    .filter(|e| e.size_bytes > 0
                        && c.path_for(e).metadata().is_ok())
                    .count(),
                None => 0,
            };
            let stale_count = match cache.as_ref() {
                Some(c) => manifest
                    .models
                    .iter()
                    .filter(|e| {
                        e.size_bytes > 0
                            && c.path_for(e).metadata().is_ok()
                            && is_stale(e, &state.ai_tools_state.model_telemetry)
                    })
                    .count(),
                None => 0,
            };
            ui.horizontal(|ui| {
                if ui.button("Open cache folder").clicked() {
                    if let Some(c) = cache.as_ref() {
                        let _ = open_in_file_browser(c.root());
                    }
                }
                if ui
                    .add_enabled(installed_count > 0, egui::Button::new("Verify all"))
                    .on_hover_text(
                        "Re-hash every installed model and toast a \
                         summary of matches vs mismatches.",
                    )
                    .on_disabled_hover_text("No models installed")
                    .clicked()
                {
                    verify_all(&manifest, cache.as_ref(), &mut state.toast);
                }
                if ui
                    .add_enabled(installed_count > 0, egui::Button::new("Uninstall all"))
                    .on_hover_text(
                        "Delete every installed model from the cache. \
                         Manifest entries stay visible so you can \
                         re-download any of them.",
                    )
                    .on_disabled_hover_text("No models installed")
                    .clicked()
                {
                    state.show_ai_models_bulk_uninstall_confirm = true;
                }
                if ui
                    .add_enabled(stale_count > 0, egui::Button::new("Uninstall unused"))
                    .on_hover_text(
                        "Delete models you haven't used in 30+ days. \
                         Telemetry is what makes this meaningful — \
                         freshly installed models that haven't been \
                         run yet are skipped.",
                    )
                    .on_disabled_hover_text(
                        "No installed models are stale yet (used in \
                         the last 30 days, or never used).",
                    )
                    .clicked()
                {
                    state.show_ai_models_stale_uninstall_confirm = true;
                }
                if ui
                    .button("Add custom model…")
                    .on_hover_text(
                        "Paste a URL to an ONNX or safetensors file \
                         and add it as a custom model. You'll be \
                         asked to pick which adapter (BiRefNet / \
                         MobileSAM / RF-DETR-Seg) backs it.",
                    )
                    .clicked()
                {
                    if matches!(
                        state.custom_model_add,
                        crate::ai_tools::custom_install::CustomInstallState::Idle
                    ) {
                        state.custom_model_add =
                            crate::ai_tools::custom_install::CustomInstallState::Form(
                                Default::default(),
                            );
                    }
                }
                ui.label(
                    egui::RichText::new(format!(
                        "Cache: {}",
                        format_bytes(total_cache_bytes),
                    ))
                    .small()
                    .color(theme.text_secondary),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close_requested = true;
                    }
                });
            });
        });

    state.show_ai_models_dialog = open && !close_requested;

    // Uninstall confirmation modal — drawn after the parent dialog
    // so it stacks on top. Synchronous: file delete + clear_instance
    // are microsecond-fast, no need for a worker thread.
    show_uninstall_confirm(ctx, state, theme, &cache);
    show_bulk_uninstall_confirm(ctx, state, theme, &manifest, &cache);
    show_stale_uninstall_confirm(ctx, state, theme, &manifest, &cache);
    show_custom_install(ctx, state, theme, &manifest, &cache);
    show_custom_uninstall_confirm(ctx, state, theme, &cache);
}

/// Render the "Delete <model>?" modal when state has a pending entry.
/// On confirm: remove the cache file + drop the registry's cached
/// generator + toast result. On cancel / scrim click: clear the
/// pending entry and do nothing.
fn show_uninstall_confirm(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    cache: &Option<ModelCache>,
) {
    let Some(entry) = state.show_ai_model_uninstall_confirm.clone() else {
        return;
    };

    let screen_rect = ctx.screen_rect();

    // Modal scrim — same darken-the-world treatment the parent
    // dialog uses, on a higher layer so it stacks above.
    egui::Area::new(egui::Id::new("ai_model_uninstall_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(160),
            );
            if resp.clicked() {
                state.show_ai_model_uninstall_confirm = None;
            }
        });

    let mut close_modal = false;
    let mut do_uninstall = false;

    egui::Window::new("Uninstall AI model")
        .collapsible(false)
        .resizable(false)
        .fixed_size([420.0, 180.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Delete {}?", entry.display_name)).strong());
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Frees {} on disk. The model entry stays \
                     visible in the dialog so you can re-download \
                     it any time.",
                    format_bytes(entry.size_bytes),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_modal = true;
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Uninstall")
                                        .color(theme.marker_error),
                                ),
                            )
                            .clicked()
                        {
                            do_uninstall = true;
                        }
                    },
                );
            });
        });

    if close_modal {
        state.show_ai_model_uninstall_confirm = None;
    }
    if do_uninstall {
        execute_uninstall(state, &entry, cache);
        state.show_ai_model_uninstall_confirm = None;
    }
}

/// Bulk verify: re-hash every installed model and toast a summary.
/// Synchronous on the UI thread; with today's cache (~600 MB across
/// 4 models) the hash sweep finishes in ~2-3 seconds. If the cache
/// grows past a couple of gigabytes, hoist this to a worker thread
/// alongside `verify_now`.
fn verify_all(
    manifest: &klipp_ml::manifest::ModelManifest,
    cache: Option<&ModelCache>,
    toast: &mut crate::widgets::toast::ToastState,
) {
    let Some(cache) = cache else {
        toast.show("Verify all failed: cache unavailable");
        return;
    };
    let mut matched = 0u32;
    let mut mismatched_names: Vec<String> = Vec::new();
    let mut errors = 0u32;
    for entry in &manifest.models {
        if entry.size_bytes == 0 {
            continue; // placeholder manifest entry, nothing to hash
        }
        let path = cache.path_for(entry);
        if !path.exists() {
            continue; // not installed, skip silently
        }
        match klipp_ml::cache::sha256_of_file(&path) {
            Ok(actual) => {
                if actual.eq_ignore_ascii_case(&entry.sha256) {
                    matched += 1;
                } else {
                    mismatched_names.push(entry.display_name.clone());
                }
            }
            Err(_) => errors += 1,
        }
    }
    if mismatched_names.is_empty() && errors == 0 {
        toast.show(format!("Verified ✓ {matched} model(s)"));
    } else if errors == 0 {
        toast.show(format!(
            "{} verified, {} mismatch(es): {}",
            matched,
            mismatched_names.len(),
            mismatched_names.join(", "),
        ));
    } else {
        toast.show(format!(
            "{matched} verified, {} mismatch(es), {errors} error(s)",
            mismatched_names.len(),
        ));
    }
}

/// Bulk-uninstall confirmation modal. Lists every installed model
/// + the total disk space freed; on confirm, loops file delete +
/// clear_instance for each.
fn show_bulk_uninstall_confirm(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    manifest: &klipp_ml::manifest::ModelManifest,
    cache: &Option<ModelCache>,
) {
    if !state.show_ai_models_bulk_uninstall_confirm {
        return;
    }
    let Some(c) = cache.as_ref() else {
        // Cache unavailable — clear the flag and toast so the user
        // isn't stuck on a non-functional modal.
        state.show_ai_models_bulk_uninstall_confirm = false;
        state.toast.show("Uninstall all failed: cache unavailable");
        return;
    };

    // Collect installed entries + their sizes once. Recomputed each
    // frame the modal is open — cheap (stat-only) and the list can
    // change between frames if the user runs Uninstall on a single
    // row while the bulk confirm is open.
    let installed: Vec<(klipp_ml::manifest::ModelEntry, u64)> = manifest
        .models
        .iter()
        .filter(|e| e.size_bytes > 0)
        .filter_map(|e| c.path_for(e).metadata().ok().map(|m| (e.clone(), m.len())))
        .collect();
    let total: u64 = installed.iter().map(|(_, s)| *s).sum();

    if installed.is_empty() {
        // Nothing left to uninstall — close the modal silently.
        state.show_ai_models_bulk_uninstall_confirm = false;
        return;
    }

    let screen_rect = ctx.screen_rect();

    egui::Area::new(egui::Id::new("ai_model_bulk_uninstall_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(160),
            );
            if resp.clicked() {
                state.show_ai_models_bulk_uninstall_confirm = false;
            }
        });

    let mut close_modal = false;
    let mut do_uninstall_all = false;

    egui::Window::new("Uninstall all AI models")
        .collapsible(false)
        .resizable(false)
        .fixed_size([460.0, 320.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Delete {} installed model(s)?",
                    installed.len(),
                ))
                .strong(),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "Frees {} on disk. Manifest entries stay visible \
                     so you can re-download any of these later.",
                    format_bytes(total),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (entry, size) in &installed {
                        ui.label(
                            egui::RichText::new(format!(
                                "• {} ({})",
                                entry.display_name,
                                format_bytes(*size),
                            ))
                            .small(),
                        );
                    }
                });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_modal = true;
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Uninstall all")
                                    .color(theme.marker_error),
                            ))
                            .clicked()
                        {
                            do_uninstall_all = true;
                        }
                    },
                );
            });
        });

    if close_modal {
        state.show_ai_models_bulk_uninstall_confirm = false;
    }
    if do_uninstall_all {
        execute_bulk_uninstall(state, &installed, c);
        state.show_ai_models_bulk_uninstall_confirm = false;
    }
}

/// "Uninstall unused" confirmation modal. Same shape as the bulk
/// modal but pre-filtered to entries flagged by [`is_stale`]; each
/// row carries the relative-time label so the user sees why a
/// given model is on the list.
fn show_stale_uninstall_confirm(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    manifest: &klipp_ml::manifest::ModelManifest,
    cache: &Option<ModelCache>,
) {
    if !state.show_ai_models_stale_uninstall_confirm {
        return;
    }
    let Some(c) = cache.as_ref() else {
        state.show_ai_models_stale_uninstall_confirm = false;
        state.toast.show("Uninstall unused failed: cache unavailable");
        return;
    };

    // Collect (entry, size, last_used) tuples for every stale
    // installed model. Recomputed each frame the modal is open.
    let stale: Vec<(klipp_ml::manifest::ModelEntry, u64, SystemTime)> = manifest
        .models
        .iter()
        .filter(|e| e.size_bytes > 0 && is_stale(e, &state.ai_tools_state.model_telemetry))
        .filter_map(|e| {
            let size = c.path_for(e).metadata().ok()?.len();
            let last_used = state
                .ai_tools_state
                .model_telemetry
                .get(&e.id)
                .map(|t| t.last_used)?;
            Some((e.clone(), size, last_used))
        })
        .collect();
    let total: u64 = stale.iter().map(|(_, s, _)| *s).sum();

    if stale.is_empty() {
        // The user might have just used a model — drop the modal
        // silently rather than showing an empty list.
        state.show_ai_models_stale_uninstall_confirm = false;
        return;
    }

    let screen_rect = ctx.screen_rect();

    egui::Area::new(egui::Id::new("ai_model_stale_uninstall_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(160),
            );
            if resp.clicked() {
                state.show_ai_models_stale_uninstall_confirm = false;
            }
        });

    let mut close_modal = false;
    let mut do_uninstall = false;

    egui::Window::new("Uninstall unused AI models")
        .collapsible(false)
        .resizable(false)
        .fixed_size([460.0, 320.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Delete {} stale model(s)?",
                    stale.len(),
                ))
                .strong(),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "Models last used more than 30 days ago. Frees {} \
                     on disk; manifest entries stay visible so you \
                     can re-download any of them.",
                    format_bytes(total),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (entry, size, last_used) in &stale {
                        ui.label(
                            egui::RichText::new(format!(
                                "• {} ({}) — last used {}",
                                entry.display_name,
                                format_bytes(*size),
                                format_relative(*last_used),
                            ))
                            .small(),
                        );
                    }
                });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_modal = true;
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Uninstall unused")
                                    .color(theme.marker_error),
                            ))
                            .clicked()
                        {
                            do_uninstall = true;
                        }
                    },
                );
            });
        });

    if close_modal {
        state.show_ai_models_stale_uninstall_confirm = false;
    }
    if do_uninstall {
        // Strip the last_used field — execute_bulk_uninstall takes
        // (ModelEntry, u64) tuples. Reuses the same loop body so
        // file delete + clear_instance + summary toast all match the
        // "Uninstall all" path.
        let trimmed: Vec<(klipp_ml::manifest::ModelEntry, u64)> = stale
            .into_iter()
            .map(|(e, s, _)| (e, s))
            .collect();
        execute_bulk_uninstall(state, &trimmed, c);
        state.show_ai_models_stale_uninstall_confirm = false;
    }
}

/// Loop body for "Uninstall all". For each installed entry: drop
/// the cached generator + remove the cache file. Single summary
/// toast at the end so the user gets one message, not N.
fn execute_bulk_uninstall(
    state: &mut AppState,
    installed: &[(klipp_ml::manifest::ModelEntry, u64)],
    cache: &ModelCache,
) {
    let registry = state.ai_tools_registry.shared();
    let mut deleted = 0u32;
    let mut errors: Vec<String> = Vec::new();
    let mut freed: u64 = 0;
    for (entry, size) in installed {
        // Drop the cached generator before the file delete so the
        // next dispatch rebuilds rather than reusing a stale Box.
        if let Ok(g) = registry.read() {
            let _ = g.clear_instance(&entry.id);
        }
        let path = cache.path_for(entry);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                deleted += 1;
                freed += *size;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Already gone — count as "deleted" since the user's
                // intent (file should not exist) is satisfied.
                deleted += 1;
            }
            Err(e) => {
                errors.push(format!("{}: {}", entry.display_name, e));
            }
        }
    }
    if errors.is_empty() {
        state.toast.show(format!(
            "Uninstalled {} model(s), freed {}",
            deleted,
            format_bytes(freed),
        ));
    } else {
        state.toast.show(format!(
            "Uninstalled {} of {}, errors: {}",
            deleted,
            installed.len(),
            errors.join("; "),
        ));
    }
}

/// One row in the "Custom models" section. Distinct from
/// `render_row` because the data shape (CustomModelEntry) and the
/// available actions are different — only Uninstall, no Reinstall
/// (we don't have the manifest's expected SHA to compare against,
/// the user is the trust root for these).
fn render_custom_row(
    ui: &mut egui::Ui,
    entry: &crate::ai_tools::persistence::CustomModelEntry,
    theme: &KlippTheme,
    telemetry: &HashMap<String, ModelTelemetry>,
    pending_uninstall: &mut Option<crate::ai_tools::persistence::CustomModelEntry>,
    _toast: &mut crate::widgets::toast::ToastState,
    cache: Option<&ModelCache>,
) {
    let on_disk = cache
        .map(|c| c.path_for_id(&entry.id).exists())
        .unwrap_or(false);
    let badge_label = if on_disk { "Custom" } else { "Custom (file missing)" };
    let badge_color = if on_disk {
        theme.marker_info
    } else {
        theme.marker_warning
    };

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(380.0);
            ui.label(egui::RichText::new(&entry.display_name).strong());
            ui.label(
                egui::RichText::new(format!(
                    "{} • {}",
                    entry.adapter.display_name(),
                    format_bytes(entry.size_bytes),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.label(
                egui::RichText::new(&entry.url)
                    .small()
                    .color(theme.text_dim),
            );
            // Telemetry line — same shape as render_row's. Custom
            // entries get the rolling avg too because the registry
            // dispatch path doesn't know one model from another.
            if let Some(t) = telemetry.get(&entry.id) {
                if t.has_runs() {
                    let avg = t
                        .avg_inference_ms()
                        .map(|ms| format!("avg {:.0}ms", ms))
                        .unwrap_or_else(|| "—".into());
                    ui.label(
                        egui::RichText::new(format!(
                            "Last used: {} • {}",
                            crate::ai_tools::format_relative(t.last_used),
                            avg,
                        ))
                        .small()
                        .color(theme.text_dim),
                    );
                }
            }
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Uninstall")
                .on_hover_text(
                    "Delete the cached file + drop the in-memory \
                     generator + remove the entry from \
                     ~/.config/klipp/ai_tools.toml.",
                )
                .clicked()
            {
                *pending_uninstall = Some(entry.clone());
            }
            ui.add_space(8.0);
            let badge = egui::Frame::none()
                .fill(badge_color.gamma_multiply(0.18))
                .stroke(egui::Stroke::new(1.0, badge_color))
                .rounding(3.0)
                .inner_margin(egui::Margin::symmetric(6.0, 2.0));
            badge.show(ui, |ui| {
                ui.label(
                    egui::RichText::new(badge_label)
                        .small()
                        .color(badge_color),
                );
            });
        });
    });
}

/// Confirmation modal for "Uninstall this custom model". Three-line
/// difference from `show_uninstall_confirm`: keyed on
/// `CustomModelEntry`, removes the entry from
/// `ai_tools_state.custom_models`, and persists the updated state
/// to disk so the next launch doesn't re-register the deleted
/// model.
fn show_custom_uninstall_confirm(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    cache: &Option<ModelCache>,
) {
    let Some(entry) = state.show_ai_custom_uninstall_confirm.clone() else {
        return;
    };

    let screen_rect = ctx.screen_rect();

    egui::Area::new(egui::Id::new("ai_custom_uninstall_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(160),
            );
            if resp.clicked() {
                state.show_ai_custom_uninstall_confirm = None;
            }
        });

    let mut close_modal = false;
    let mut do_uninstall = false;

    egui::Window::new("Uninstall custom AI model")
        .collapsible(false)
        .resizable(false)
        .fixed_size([460.0, 200.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Delete {}?", entry.display_name)).strong());
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Frees {} on disk and removes the entry from \
                     ~/.config/klipp/ai_tools.toml. The next launch \
                     won't try to register this model again.",
                    format_bytes(entry.size_bytes),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_modal = true;
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Uninstall")
                                    .color(theme.marker_error),
                            ))
                            .clicked()
                        {
                            do_uninstall = true;
                        }
                    },
                );
            });
        });

    if close_modal {
        state.show_ai_custom_uninstall_confirm = None;
    }
    if do_uninstall {
        execute_custom_uninstall(state, &entry, cache);
        state.show_ai_custom_uninstall_confirm = None;
    }
}

/// Single-row uninstall body for a custom model. Differs from
/// `execute_uninstall` (which keys on `ModelEntry`) in that it
/// also drops the entry from `ai_tools_state.custom_models` and
/// persists the updated config so the next launch doesn't try to
/// re-register.
fn execute_custom_uninstall(
    state: &mut AppState,
    entry: &crate::ai_tools::persistence::CustomModelEntry,
    cache: &Option<ModelCache>,
) {
    let Some(c) = cache.as_ref() else {
        state.toast.show("Uninstall failed: cache unavailable");
        return;
    };
    let path = c.path_for_id(&entry.id);

    // Drop the cached generator so a future re-add (under the same
    // id) doesn't reuse a stale Box pointing at the deleted file.
    let registry = state.ai_tools_registry.shared();
    if let Ok(g) = registry.read() {
        let _ = g.clear_instance(&entry.id);
    }
    // Drop the registration itself so the entry stops appearing in
    // the panel dropdown immediately. Done under a write lock — no
    // public unregister method on ModelRegistry today, so we pull
    // it out by writing through the shared Arc directly... actually
    // ModelRegistry has no remove API. Skip for v1; the in-memory
    // registration goes away on next launch when load_persistent
    // skips this id. The factory closure still exists but a future
    // dispatch would fail at the file-load step (file is gone), so
    // the user sees "the model failed: ..." rather than silently
    // running a deleted model.
    // TODO: add ModelRegistry::unregister so the panel dropdown
    // reflects the uninstall immediately rather than at next launch.

    let removed_from_disk = match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            state.toast.show(format!(
                "Uninstall failed: {} ({e})",
                entry.display_name,
            ));
            return;
        }
    };

    // Drop from ai_tools_state.custom_models (in-memory state).
    state
        .ai_tools_state
        .custom_models
        .retain(|c| c.id != entry.id);

    // Persist updated state (without the now-removed custom entry).
    crate::ai_tools::persistence::save(
        &crate::ai_tools::persistence::AiToolsConfig::from_state(
            &state.ai_tools_state,
        ),
    );

    if removed_from_disk {
        state
            .toast
            .show(format!("Uninstalled {}", entry.display_name));
    }
}

/// Custom-URL paste flow modal. Renders one of three views based
/// on the current [`CustomInstallState`]:
///
/// * **Form** — URL + adapter + display name + id. "Download"
///   validates + spawns the worker thread; "Cancel" returns to Idle.
/// * **Downloading** — progress bar + Cancel button.
/// * **Confirming** — shows computed SHA + size; "Register" does
///   register + persist + toast, "Cancel" deletes the just-downloaded
///   file.
fn show_custom_install(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    manifest: &klipp_ml::manifest::ModelManifest,
    cache: &Option<ModelCache>,
) {
    use crate::ai_tools::custom_install::{
        finalize_entry, validate_form, CustomInstallState, CUSTOM_DOWNLOAD_MAX_BYTES,
    };
    use crate::ai_tools::persistence::CustomAdapter;

    if matches!(state.custom_model_add, CustomInstallState::Idle) {
        return;
    }

    let screen_rect = ctx.screen_rect();

    // Modal scrim — same darken treatment the other AI Models
    // modals use.
    egui::Area::new(egui::Id::new("ai_custom_install_scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            // Don't dismiss on scrim click during Downloading —
            // the user might bump it accidentally and orphan the
            // worker thread. Form / Confirming let scrim close.
            let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_black_alpha(160),
            );
            let cancel_via_scrim = resp.clicked();
            if cancel_via_scrim {
                match &state.custom_model_add {
                    CustomInstallState::Form(_) | CustomInstallState::Confirming { .. } => {
                        state.custom_model_add = CustomInstallState::Idle;
                    }
                    _ => {}
                }
            }
        });

    egui::Window::new("Add custom AI model")
        .collapsible(false)
        .resizable(false)
        .fixed_size([520.0, 360.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            match &mut state.custom_model_add {
                CustomInstallState::Idle => {}
                CustomInstallState::Form(form) => {
                    ui.label(
                        egui::RichText::new(
                            "Paste a URL to an ONNX (or safetensors) file. Klipp \
                             will download it, compute its SHA-256, and ask you \
                             to confirm before adding it as a custom model.",
                        )
                        .small()
                        .color(theme.text_secondary),
                    );
                    ui.add_space(10.0);

                    ui.label("URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.url)
                            .hint_text("https://...")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);

                    ui.label("Adapter");
                    let current_label = form
                        .adapter
                        .map(|a| a.display_name())
                        .unwrap_or("(pick one)");
                    egui::ComboBox::from_id_salt("ai_custom_adapter")
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for a in CustomAdapter::ALL {
                                if ui
                                    .selectable_label(form.adapter == Some(*a), a.display_name())
                                    .clicked()
                                {
                                    form.adapter = Some(*a);
                                }
                            }
                        });
                    ui.label(
                        egui::RichText::new(
                            "Pick the architecture this file matches. Klipp \
                             can't infer it without loading the model.",
                        )
                        .small()
                        .color(theme.text_dim),
                    );
                    ui.add_space(8.0);

                    ui.label("Display name");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.display_name)
                            .hint_text("e.g. My Custom RF-DETR")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);

                    ui.label("Stable id (used internally; alphanumeric / _ / -)");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.id)
                            .hint_text("e.g. my_custom_rfdetr_v1")
                            .desired_width(f32::INFINITY),
                    );

                    if let Some(err) = &form.error {
                        ui.add_space(8.0);
                        ui.colored_label(theme.marker_error, err);
                    }

                    ui.add_space(14.0);
                    let mut close_to_idle = false;
                    let mut start_download = false;
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close_to_idle = true;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("Download").clicked() {
                                    start_download = true;
                                }
                            },
                        );
                    });

                    if close_to_idle {
                        state.custom_model_add = CustomInstallState::Idle;
                    } else if start_download {
                        // Build the existing-id list once so the form
                        // validation can reject collisions.
                        let mut existing_ids: Vec<String> = manifest
                            .models
                            .iter()
                            .map(|e| e.id.clone())
                            .collect();
                        for c in &state.ai_tools_state.custom_models {
                            existing_ids.push(c.id.clone());
                        }
                        match validate_form(form, &existing_ids) {
                            Err(reason) => {
                                form.error = Some(reason);
                            }
                            Ok(()) => {
                                let Some(c) = cache.as_ref() else {
                                    form.error =
                                        Some("Cache directory unavailable".into());
                                    return;
                                };
                                let form_clone = form.clone();
                                let target_partial =
                                    c.root().join(format!("{}.bin.partial", form.id));
                                let target_final = c.path_for_id(&form.id);
                                let handle = spawn_custom_download(
                                    form.url.clone(),
                                    target_partial,
                                    target_final,
                                    ctx.clone(),
                                );
                                state.custom_model_add =
                                    CustomInstallState::Downloading {
                                        form: form_clone,
                                        handle,
                                        downloaded: 0,
                                        total: 0,
                                    };
                            }
                        }
                    }
                }
                CustomInstallState::Downloading {
                    form,
                    handle,
                    downloaded,
                    total,
                } => {
                    ui.label(
                        egui::RichText::new(format!("Downloading {}…", form.display_name))
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(&form.url)
                            .small()
                            .color(theme.text_secondary),
                    );
                    ui.add_space(12.0);
                    let bar = if *total > 0 {
                        egui::ProgressBar::new((*downloaded as f32) / (*total as f32))
                            .text(format!(
                                "{} / {}",
                                format_bytes(*downloaded),
                                format_bytes(*total)
                            ))
                    } else {
                        // Server didn't give Content-Length up front.
                        // Show indeterminate animation instead.
                        egui::ProgressBar::new(0.0)
                            .text(format!("{} downloaded", format_bytes(*downloaded)))
                            .animate(true)
                    };
                    ui.add(bar.desired_width(f32::INFINITY));

                    ui.add_space(14.0);
                    if ui.button("Cancel download").clicked() {
                        handle
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        // Don't drop the state yet — let poll() see
                        // the cancellation come back through the
                        // channel and transition to Form with an
                        // error message.
                    }
                }
                CustomInstallState::Confirming {
                    form,
                    sha256,
                    size_bytes,
                } => {
                    ui.label(
                        egui::RichText::new(format!(
                            "Download complete — confirm {}?",
                            form.display_name,
                        ))
                        .strong(),
                    );
                    ui.add_space(10.0);
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::same(8.0))
                        .show(ui, |ui| {
                            ui.label(format!("Adapter: {}",
                                form.adapter
                                    .map(|a| a.display_name())
                                    .unwrap_or("?")));
                            ui.label(format!("Size: {}", format_bytes(*size_bytes)));
                            ui.label(format!("SHA-256: {sha256}"));
                            ui.label(format!("URL: {}", form.url));
                        });
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(
                            "Klipp doesn't validate that the file actually matches \
                             the adapter you picked — that check happens the first \
                             time you run inference. If the file is the wrong \
                             architecture, the run will fail with a clean error.",
                        )
                        .small()
                        .color(theme.text_dim),
                    );

                    ui.add_space(14.0);
                    let mut do_register = false;
                    let mut do_cancel = false;
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            do_cancel = true;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("Register").color(
                                            theme.marker_success,
                                        ),
                                    ))
                                    .clicked()
                                {
                                    do_register = true;
                                }
                            },
                        );
                    });

                    if do_cancel {
                        // Delete the just-downloaded file — the user
                        // bailed out, no point keeping bytes around.
                        if let Some(c) = cache.as_ref() {
                            let _ = std::fs::remove_file(c.path_for_id(&form.id));
                        }
                        state.custom_model_add = CustomInstallState::Idle;
                    } else if do_register {
                        let entry = match finalize_entry(
                            form,
                            sha256.clone(),
                            *size_bytes,
                        ) {
                            Some(e) => e,
                            None => {
                                state
                                    .toast
                                    .show("Custom install failed: form lost adapter");
                                state.custom_model_add = CustomInstallState::Idle;
                                return;
                            }
                        };
                        // Register against the shared registry.
                        let registry = state.ai_tools_registry.shared();
                        if let Ok(mut r) = registry.write() {
                            crate::state::register_custom_model(&mut r, &entry);
                        }
                        // Append to AppState + persist the full
                        // updated state to disk.
                        state
                            .ai_tools_state
                            .custom_models
                            .push(entry.clone());
                        crate::ai_tools::persistence::save(
                            &crate::ai_tools::persistence::AiToolsConfig::from_state(
                                &state.ai_tools_state,
                            ),
                        );
                        state.toast.show(format!(
                            "Added custom model {} (registered + persisted)",
                            entry.display_name,
                        ));
                        state.custom_model_add = CustomInstallState::Idle;
                    }
                    let _ = CUSTOM_DOWNLOAD_MAX_BYTES; // re-export marker
                }
            }
        });
}

/// Spawn the URL fetch on a worker thread. Streams progress events
/// through a crossbeam channel; the UI thread polls them via
/// `CustomInstallState::poll`.
fn spawn_custom_download(
    url: String,
    target_partial: std::path::PathBuf,
    target_final: std::path::PathBuf,
    ctx: egui::Context,
) -> crate::ai_tools::custom_install::CustomDownloadHandle {
    use crate::ai_tools::custom_install::{
        CustomDownloadEvent, CustomDownloadHandle, CustomDownloadResult,
        CUSTOM_DOWNLOAD_MAX_BYTES,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    let (tx, rx) = crossbeam_channel::unbounded::<CustomDownloadEvent>();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let cancel_for_worker = cancel.clone();
    let ctx_for_worker = ctx.clone();

    std::thread::Builder::new()
        .name("klipp-ai-custom-download".into())
        .spawn(move || {
            // Adapter that translates the klipp_ml ProgressSink into
            // crossbeam events the UI side can drain.
            struct ChannelProgress {
                tx: crossbeam_channel::Sender<CustomDownloadEvent>,
                ctx: egui::Context,
                total: u64,
                downloaded: u64,
            }
            impl klipp_ml::ProgressSink for ChannelProgress {
                fn started(&mut self, total_bytes: u64) {
                    self.total = total_bytes;
                }
                fn advance(&mut self, bytes_delta: u64) {
                    self.downloaded = self.downloaded.saturating_add(bytes_delta);
                    let _ = self.tx.send(CustomDownloadEvent::Progress {
                        downloaded: self.downloaded,
                        total: self.total,
                    });
                    self.ctx.request_repaint();
                }
                fn finished(&mut self) {}
            }

            let mut prog = ChannelProgress {
                tx: tx.clone(),
                ctx: ctx_for_worker.clone(),
                total: 0,
                downloaded: 0,
            };

            let result = klipp_ml::download_url_uncertified(
                &url,
                &target_partial,
                &target_final,
                &mut prog,
                &cancel_for_worker,
                CUSTOM_DOWNLOAD_MAX_BYTES,
            );
            let payload = match result {
                Ok((sha256, size_bytes)) => CustomDownloadResult::Ok {
                    sha256,
                    size_bytes,
                },
                Err(e) => CustomDownloadResult::Err(format!("{e}")),
            };
            let _ = tx.send(CustomDownloadEvent::Completed(payload));
            ctx_for_worker.request_repaint();
            // Avoid unused warning when cancel races early.
            let _ = cancel_for_worker.load(Ordering::Relaxed);
        })
        .expect("spawn klipp-ai-custom-download thread");

    CustomDownloadHandle { recv: rx, cancel }
}

/// Re-hash the model's cache file on disk and compare against the
/// manifest's expected SHA-256. Toast surfaces the result to the
/// user.
///
/// Synchronous: runs on the UI thread. SHA-256 throughput is
/// hundreds of MB/s on modern x86_64 (sha2 crate uses CPU sha
/// extensions when available), so even the biggest cached model
/// today (BiRefNet at ~224 MB) finishes in under a second. If the
/// cache grows past the gigabyte range, hoist this onto a worker
/// thread with a Verifying… spinner.
fn verify_now(entry: &ModelEntry, toast: &mut crate::widgets::toast::ToastState) {
    let cache = match ModelCache::open() {
        Ok(c) => c,
        Err(e) => {
            toast.show(format!("Verify failed: cache open ({e})"));
            return;
        }
    };
    let path = cache.path_for(entry);
    let actual = match klipp_ml::cache::sha256_of_file(&path) {
        Ok(h) => h,
        Err(e) => {
            toast.show(format!("Verify failed: hash ({e})"));
            return;
        }
    };
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        toast.show(format!("Verified ✓ {}", entry.display_name));
    } else {
        toast.show(format!(
            "SHA mismatch on {}: file is corrupted or out of date",
            entry.display_name,
        ));
    }
}

/// Delete the cached weights file + drop the registry's in-memory
/// generator. Toast surfaces success / failure to the user.
fn execute_uninstall(
    state: &mut AppState,
    entry: &ModelEntry,
    cache: &Option<ModelCache>,
) {
    let Some(c) = cache.as_ref() else {
        state.toast.show("Uninstall failed: model cache unavailable");
        return;
    };
    let path = c.path_for(entry);

    // Drop the cached generator first — even if the file delete
    // fails, we want the next dispatch to rebuild rather than reuse
    // a possibly-stale Box. clear_instance returns false when the
    // model wasn't registered (e.g. ai-ort feature off), which we
    // ignore — the file delete is the real work.
    let registry = state.ai_tools_registry.shared();
    if let Ok(g) = registry.read() {
        let _ = g.clear_instance(&entry.id);
    }

    match std::fs::remove_file(&path) {
        Ok(()) => {
            state
                .toast
                .show(format!("Uninstalled {}", entry.display_name));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            state.toast.show(format!(
                "{} was already missing from the cache",
                entry.display_name,
            ));
        }
        Err(e) => {
            state.toast.show(format!(
                "Uninstall failed: {} ({})",
                entry.display_name, e,
            ));
        }
    }
}

fn render_row(
    ui: &mut egui::Ui,
    entry: &ModelEntry,
    status: &ModelInstallStatus,
    theme: &KlippTheme,
    download: &mut ModelDownloadState,
    telemetry: &HashMap<String, ModelTelemetry>,
    pending_uninstall: &mut Option<ModelEntry>,
    toast: &mut crate::widgets::toast::ToastState,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(380.0);
            ui.label(egui::RichText::new(&entry.display_name).strong());
            ui.label(
                egui::RichText::new(format!(
                    "{} • {} • {}",
                    kind_label(entry.kind),
                    entry.license,
                    format_bytes(entry.size_bytes),
                ))
                .small()
                .color(theme.text_secondary),
            );
            ui.label(
                egui::RichText::new(&entry.attribution)
                    .small()
                    .color(theme.text_dim),
            );
            // Telemetry line — appears only if the model has been
            // run at least once this session. Surfaces "Last used:
            // 2m ago • avg 142ms" so users can compare runtime
            // across the variants they've tried.
            if let Some(t) = telemetry.get(&entry.id) {
                if t.has_runs() {
                    let avg = t
                        .avg_inference_ms()
                        .map(|ms| format!("avg {:.0}ms", ms))
                        .unwrap_or_else(|| "—".into());
                    ui.label(
                        egui::RichText::new(format!(
                            "Last used: {} • {}",
                            format_relative(t.last_used),
                            avg,
                        ))
                        .small()
                        .color(theme.text_dim),
                    );
                }
            }
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Action buttons go on the right edge.
            match status {
                ModelInstallStatus::Pending => {
                    ui.add_enabled(false, egui::Button::new("Download…"))
                        .on_disabled_hover_text(
                            "Awaiting Klipp release-CI mirror — \
                             the file isn't downloadable yet.",
                        );
                }
                ModelInstallStatus::Missing | ModelInstallStatus::Unknown(_) => {
                    if ui
                        .add_enabled(
                            matches!(download, ModelDownloadState::Idle),
                            egui::Button::new("Download…"),
                        )
                        .on_disabled_hover_text("Another download is in progress")
                        .clicked()
                    {
                        *download = ModelDownloadState::Consenting {
                            entry: entry.clone(),
                        };
                    }
                }
                ModelInstallStatus::Mismatch => {
                    if ui
                        .add_enabled(
                            matches!(download, ModelDownloadState::Idle),
                            egui::Button::new("Reinstall"),
                        )
                        .clicked()
                    {
                        *download = ModelDownloadState::Consenting {
                            entry: entry.clone(),
                        };
                    }
                }
                ModelInstallStatus::Present { .. } => {
                    // Right-to-left layout — buttons render LEFT to
                    // RIGHT in source order on screen, in reverse.
                    // We want Reinstall on the right edge (primary
                    // action), Uninstall middle, Verify left.
                    if ui
                        .button("Reinstall")
                        .on_hover_text("Re-download and verify the file")
                        .clicked()
                        && matches!(download, ModelDownloadState::Idle)
                    {
                        *download = ModelDownloadState::Consenting {
                            entry: entry.clone(),
                        };
                    }
                    if ui
                        .button("Uninstall")
                        .on_hover_text(
                            "Delete the cached weights + drop the \
                             in-memory generator. Re-download anytime.",
                        )
                        .clicked()
                    {
                        *pending_uninstall = Some(entry.clone());
                    }
                    if ui
                        .button("Verify")
                        .on_hover_text(
                            "Re-hash the file on disk and confirm \
                             it matches the manifest's SHA-256.",
                        )
                        .clicked()
                    {
                        verify_now(entry, toast);
                    }
                }
            }

            // Status badge.
            ui.add_space(8.0);
            let badge = egui::Frame::none()
                .fill(status.badge_color(theme).gamma_multiply(0.18))
                .stroke(egui::Stroke::new(1.0, status.badge_color(theme)))
                .rounding(3.0)
                .inner_margin(egui::Margin::symmetric(6.0, 2.0));
            badge.show(ui, |ui| {
                ui.label(
                    egui::RichText::new(status.label())
                        .small()
                        .color(status.badge_color(theme)),
                );
            });
        });
    });
}

/// User-friendly label for `ModelKind`. The enum's snake_case serde
/// names are wire-format-friendly but not great in UI.
fn kind_label(kind: klipp_ml::manifest::ModelKind) -> &'static str {
    use klipp_ml::manifest::ModelKind;
    match kind {
        ModelKind::MaskGenerator => "Mask Generator",
        ModelKind::VideoTracker => "Video Tracker",
        ModelKind::ObjectDetector => "Object Detector",
    }
}

/// Open `path` in the user's default file browser. Best-effort —
/// failures fall through silently because there's nothing useful
/// the dialog can do about it.
fn open_in_file_browser(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
}

/// Format a byte count as a human-readable string (B / KB / MB / GB).
/// Mirrors what the consent dialog already shows.
pub fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_handles_each_scale() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(2048).contains("KB"));
        assert!(format_bytes(2 * 1024 * 1024).contains("MB"));
        assert!(format_bytes(3 * 1024 * 1024 * 1024).contains("GB"));
    }

    #[test]
    fn install_status_label_distinguishes_states() {
        let s = ModelInstallStatus::Present { size_bytes: 100 };
        assert_eq!(s.label(), "Installed");
        assert_eq!(ModelInstallStatus::Missing.label(), "Not installed");
        assert_eq!(ModelInstallStatus::Mismatch.label(), "Mismatch");
        assert_eq!(
            ModelInstallStatus::Unknown("io error".into()).label(),
            "Unknown"
        );
    }

    fn make_entry(id: &str) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            display_name: id.into(),
            size_bytes: 100,
            sha256: "0".repeat(64),
            license: "Apache-2.0".into(),
            attribution: "test".into(),
            sources: vec![],
            kind: klipp_ml::manifest::ModelKind::MaskGenerator,
            runtime: klipp_ml::manifest::ModelRuntime::default(),
            prompts: klipp_ml::manifest::PromptKinds::default(),
        }
    }

    #[test]
    fn is_stale_false_when_no_telemetry() {
        let entry = make_entry("never_used");
        let telemetry = HashMap::new();
        assert!(!is_stale(&entry, &telemetry));
    }

    #[test]
    fn is_stale_false_when_total_runs_zero() {
        // Edge: entry exists in the map but with default values
        // (e.g. the user just installed; we created the entry but
        // never ran inference). Skip — they probably don't want it
        // uninstalled five seconds after downloading.
        let entry = make_entry("never_run");
        let mut telemetry = HashMap::new();
        telemetry.insert(entry.id.clone(), ModelTelemetry::default());
        assert!(!is_stale(&entry, &telemetry));
    }

    #[test]
    fn is_stale_false_for_recent_use() {
        // Telemetry says "ran just now". Definitely not stale.
        let entry = make_entry("fresh");
        let mut t = ModelTelemetry::default();
        t.record_run(120);
        let mut telemetry = HashMap::new();
        telemetry.insert(entry.id.clone(), t);
        assert!(!is_stale(&entry, &telemetry));
    }

    #[test]
    fn is_stale_true_when_last_used_old_enough() {
        // Synthesise a telemetry entry whose last_used is 31 days
        // ago. STALE_AGE is 30 days, so this should clear the cut.
        let entry = make_entry("ancient");
        let t = ModelTelemetry {
            last_used: SystemTime::now() - Duration::from_secs(31 * 86_400),
            recent_inference_ms: vec![100u64].into(),
            total_runs: 1,
        };
        let mut telemetry = HashMap::new();
        telemetry.insert(entry.id.clone(), t);
        assert!(is_stale(&entry, &telemetry));
    }
}
