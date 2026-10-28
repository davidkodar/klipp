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

use crate::ai_tools::registry::required_capability;
use crate::ai_tools::{format_relative, AiTask, ModelTelemetry};
use crate::model_download::ModelDownloadState;
use crate::state::AppState;
use crate::theme::KlippTheme;

/// Sort criterion the user picks via the dropdown above the model
/// list. All sort orders are stable — entries that compare equal
/// keep their manifest order so the layout doesn't jitter as the
/// user toggles between criteria.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelSort {
    /// Curated order: recommended models first, then by manifest
    /// declaration order. The "best default for newcomers" view —
    /// puts the picks Klipp's authors stand behind at the top.
    #[default]
    Recommended,
    DisplayName,
    SizeBytes,
    LastUsed,
    License,
}

impl ModelSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recommended => "Recommended order",
            Self::DisplayName => "Display name (A→Z)",
            Self::SizeBytes => "Size (small → large)",
            Self::LastUsed => "Last used (recent first)",
            Self::License => "License (A→Z)",
        }
    }

    pub const ALL: &'static [ModelSort] = &[
        ModelSort::Recommended,
        ModelSort::DisplayName,
        ModelSort::SizeBytes,
        ModelSort::LastUsed,
        ModelSort::License,
    ];
}

/// Hardcoded curation flag — true for models Klipp's authors
/// recommend as the default pick. Surfaces the RECOMMENDED badge
/// in the row layout and pins the entry to the top of the
/// "Recommended order" sort. Migrate to a manifest field
/// (`recommended = true`) once we want users to extend this list
/// for their own custom entries.
fn is_recommended(id: &str) -> bool {
    matches!(id, "birefnet_lite_v1" | "rf_detr_seg_medium_v1" | "mobile_sam_onnx")
}

/// Hardcoded short description rendered in the (i) info-icon
/// tooltip. ~one sentence per model, written from the user's POV
/// (what does this model do well? trade-offs vs siblings?). Empty
/// string = no tooltip. Same migration story as `is_recommended`:
/// move to a manifest field when we want users to author their own.
fn description_for(id: &str) -> &'static str {
    match id {
        "birefnet_lite_v1" => {
            "High-detail mask generator. Best edge quality of the lite-tier \
             foreground/background matting models."
        }
        "mobile_sam_onnx" => {
            "Click-to-mask SAM-family model. Tiny (~45 MB) and fast; one \
             foreground click usually nails the silhouette."
        }
        "rf_detr_seg_v1" => {
            "Auto-detect instance segmentation, preview tier (~134 MB). \
             Fast, classifier sometimes confidently wrong on out-of-\
             distribution scenes."
        }
        "rf_detr_seg_medium_v1" => {
            "Auto-detect instance segmentation, medium tier (~140 MB). \
             Same speed as preview, genuinely accurate classifier."
        }
        _ => "",
    }
}

/// Per-frame UI state for the AI Models dialog. Carried on AppState
/// because the dialog needs to remember the user's filter + sort +
/// search across renders. Resets on each app launch (intentional
/// — a fresh dialog should be unfiltered).
#[derive(Debug, Clone, Default)]
pub struct AiModelsDialogState {
    /// Task filter. `None` = show every task. `Some(t)` = show only
    /// models whose capabilities are a superset of the task's
    /// required capability.
    pub filter_task: Option<AiTask>,
    /// When `true`, hide entries whose status isn't `Present` —
    /// useful for "show me Remove Background models I haven't yet
    /// installed" once you flip the task filter.
    pub installed_only: bool,
    /// Sort criterion applied before rendering rows.
    pub sort: ModelSort,
    /// Substring filter typed into the search box. Compared
    /// case-insensitively against display name, model id,
    /// attribution, license. Empty = no filter.
    pub search: String,
    /// Stable id of the model whose Show details modal is open.
    /// `None` = no modal. The three-dot overflow menu's open
    /// state is handled by egui's built-in popup memory (keyed
    /// by row id); only the modal needs cross-frame state here.
    pub details_for: Option<String>,
}

/// Threshold for "Uninstall unused". A model whose last successful
/// run was more than this long ago counts as stale. 30 days mirrors
/// what the F-97-MODEL-MANAGER spec calls out and is long enough to
/// avoid false positives across a normal vacation.
const STALE_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Per-row cached status. Decoupled from
/// `klipp_ml::cache::CachedStatus` so the dialog can carry a
/// "FilePresent (size in bytes, but unverified)" state without
/// pretending it's verified.
#[derive(Debug, Clone, PartialEq)]
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

/// Apply the dialog's filter (task + installed-only) + sort to the
/// manifest entries. Returns owned references; the underlying
/// `ModelManifest` outlives this call so the borrow stays valid.
fn filter_and_sort<'m>(
    manifest: &'m klipp_ml::manifest::ModelManifest,
    cache: Option<&ModelCache>,
    state: &AiModelsDialogState,
    telemetry: &HashMap<String, ModelTelemetry>,
) -> Vec<&'m ModelEntry> {
    let mut out: Vec<&ModelEntry> = manifest
        .models
        .iter()
        .filter(|e| match_task_filter(prompt_kinds_to_capabilities(e.prompts), state))
        .filter(|e| match_installed_filter(e, cache, state))
        // A voice is one row: its settings file and the small Kokoro
        // voices come with the download they belong to.
        .filter(|e| klipp_audio::narration::shown_in_model_manager(&e.id))
        // A file that comes with another model is that model's row.
        .filter(|e| !manifest.is_part(&e.id))
        .filter(|e| {
            match_search(
                &state.search,
                &e.display_name,
                &e.id,
                &e.attribution,
                &e.license,
            )
        })
        .collect();
    sort_entries(
        &mut out,
        state.sort,
        telemetry,
        |e| e.size_bytes,
        |e| e.license.as_str(),
        |e| e.display_name.as_str(),
        |e| &e.id,
    );
    out
}

/// Case-insensitive substring match across the four searchable
/// fields. Empty query matches everything (the "no filter" case).
fn match_search(query: &str, name: &str, id: &str, attribution: &str, license: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    name.to_ascii_lowercase().contains(&q)
        || id.to_ascii_lowercase().contains(&q)
        || attribution.to_ascii_lowercase().contains(&q)
        || license.to_ascii_lowercase().contains(&q)
}

/// Same shape but for custom entries. Custom models are always
/// "installed" by definition (the user explicitly added them); the
/// installed-only filter is a no-op for this section.
fn filter_and_sort_custom(
    custom: &[crate::ai_tools::persistence::CustomModelEntry],
    _cache: Option<&ModelCache>,
    state: &AiModelsDialogState,
    telemetry: &HashMap<String, ModelTelemetry>,
) -> Vec<crate::ai_tools::persistence::CustomModelEntry> {
    use crate::ai_tools::persistence::CustomAdapter;
    let mut out: Vec<crate::ai_tools::persistence::CustomModelEntry> = custom
        .iter()
        .filter(|e| {
            let caps = match e.adapter {
                CustomAdapter::BiRefNet => klipp_ml::MaskCapabilities::AUTO_DETECT,
                CustomAdapter::MobileSam => {
                    klipp_ml::MaskCapabilities::POINT_PROMPT
                        | klipp_ml::MaskCapabilities::BOX_PROMPT
                        | klipp_ml::MaskCapabilities::SCRIBBLE
                        | klipp_ml::MaskCapabilities::AUTO_DETECT
                }
                CustomAdapter::RfDetrSeg => klipp_ml::MaskCapabilities::AUTO_DETECT,
            };
            match_task_filter(caps, state)
        })
        .filter(|e| {
            match_search(
                &state.search,
                &e.display_name,
                &e.id,
                "",
                "",
            )
        })
        .cloned()
        .collect();
    // Sort using the closures' shape from the manifest path so the
    // ordering criteria match across both sections.
    let sort = state.sort;
    out.sort_by(|a, b| compare_for_sort(
        sort, telemetry,
        a.size_bytes, b.size_bytes,
        a.id.as_str(), b.id.as_str(),
        "", "", // custom entries have no license string today
        a.display_name.as_str(), b.display_name.as_str(),
    ));
    out
}

/// Map the manifest's `PromptKinds` bitset onto the runtime
/// `MaskCapabilities` bitset for filtering purposes. Both bitsets
/// share the same flag layout (`1<<0` through `1<<5`), so a
/// truncating bit-cast is the right primitive — it guards against
/// future flags appearing in one bitset before the other lands.
fn prompt_kinds_to_capabilities(
    p: klipp_ml::manifest::PromptKinds,
) -> klipp_ml::MaskCapabilities {
    klipp_ml::MaskCapabilities::from_bits_truncate(p.bits())
}

/// Whether `caps` satisfies the dialog's task filter. `None` filter
/// passes everything; `Some(t)` requires the entry to be a superset
/// of that task's required capability.
fn match_task_filter(
    caps: klipp_ml::MaskCapabilities,
    state: &AiModelsDialogState,
) -> bool {
    match state.filter_task {
        None => true,
        Some(t) => caps.contains(required_capability(t)),
    }
}

/// Whether the entry passes the installed-only checkbox. Off ⇒
/// pass. On ⇒ require the cache file to exist.
fn match_installed_filter(
    entry: &ModelEntry,
    cache: Option<&ModelCache>,
    state: &AiModelsDialogState,
) -> bool {
    if !state.installed_only {
        return true;
    }
    match cache {
        Some(c) => c.path_for(entry).exists(),
        None => false,
    }
}

/// Stable in-place sort over manifest entries. Stability matters —
/// equal-keyed entries keep their manifest order so toggling sort
/// doesn't visibly jitter neighbours.
fn sort_entries<E>(
    out: &mut Vec<&E>,
    sort: ModelSort,
    telemetry: &HashMap<String, ModelTelemetry>,
    size_of: impl Fn(&E) -> u64,
    license_of: impl Fn(&E) -> &str,
    display_of: impl Fn(&E) -> &str,
    id_of: impl Fn(&E) -> &str,
) {
    out.sort_by(|a, b| {
        compare_for_sort(
            sort,
            telemetry,
            size_of(a),
            size_of(b),
            id_of(a),
            id_of(b),
            license_of(a),
            license_of(b),
            display_of(a),
            display_of(b),
        )
    });
}

/// Comparator shared by the manifest path and the custom path.
/// Pulled into a free function so both can call it with their own
/// field accessors.
#[allow(clippy::too_many_arguments)]
fn compare_for_sort(
    sort: ModelSort,
    telemetry: &HashMap<String, ModelTelemetry>,
    size_a: u64, size_b: u64,
    id_a: &str, id_b: &str,
    license_a: &str, license_b: &str,
    display_a: &str, display_b: &str,
) -> std::cmp::Ordering {
    match sort {
        ModelSort::Recommended => {
            // Recommended ids first (true sorts before false ⇒
            // reverse-cmp), then preserve declaration order via
            // display name as the secondary criterion.
            match (is_recommended(id_a), is_recommended(id_b)) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => display_a.cmp(display_b),
            }
        }
        ModelSort::DisplayName => display_a.cmp(display_b),
        ModelSort::SizeBytes => size_a.cmp(&size_b),
        ModelSort::License => license_a.cmp(license_b),
        ModelSort::LastUsed => {
            // Most-recently-used first (descending). Models with
            // no telemetry sort to the bottom — UNIX_EPOCH is
            // older than any real run.
            let last_a = telemetry
                .get(id_a)
                .map(|t| t.last_used)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let last_b = telemetry
                .get(id_b)
                .map(|t| t.last_used)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            last_b.cmp(&last_a)
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
/// filesystem: present only with every file that comes with it, its
/// size theirs together. Cheap: stat-only, no hash compute.
fn install_status_for(cache: &ModelCache, manifest: &klipp_ml::manifest::ModelManifest, entry: &ModelEntry) -> ModelInstallStatus {
    let mut size_bytes = 0;
    for part in manifest.parts(entry) {
        match cache.path_for(part).metadata() {
            Ok(m) => size_bytes += m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ModelInstallStatus::Missing,
            Err(e) => return ModelInstallStatus::Unknown(e.to_string()),
        }
    }
    ModelInstallStatus::Present { size_bytes }
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

    // Dialog sizing: default ~900px wide so the row layout fits
    // without truncation; resizable so users with smaller monitors
    // can shrink it (rows degrade gracefully with the truncate +
    // tooltip path below). Min width set high enough that the
    // toolbar's TASK / SORT / search controls don't overlap.
    let default_w = (screen_rect.width() * 0.65).clamp(720.0, 1100.0);
    egui::Window::new("AI Models")
        .collapsible(false)
        .resizable(true)
        .default_size([default_w, max_h])
        .min_width(680.0)
        .min_height(360.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            // Header description — wraps cleanly across multiple
            // lines instead of pushing toolbar/list off-screen.
            // Cache path on its own line so it stays visible even
            // at narrow widths.
            ui.label(
                egui::RichText::new(
                    "Local AI models. Downloads are explicit — no bytes hit your \
                     disk without your confirmation. Cache lives at",
                )
                .small()
                .color(theme.text_secondary),
            );
            ui.label(
                egui::RichText::new("~/.local/share/klipp/models/")
                    .small()
                    .monospace()
                    .color(theme.text_secondary),
            );
            ui.add_space(6.0);

            // ── Toolbar: TASK + SORT + Installed only + search ──
            // Visual style mirrors the F-97-MODEL-MANAGER mockup
            // — small-caps labels, tight ComboBoxes, search box
            // pinned to the right edge.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("TASK")
                        .small()
                        .strong()
                        .color(theme.text_dim),
                );
                let task_label = match state.ai_models_dialog.filter_task {
                    None => "All tasks",
                    Some(AiTask::RemoveBackground) => "Remove Background",
                    Some(AiTask::IsolateObject) => "Isolate Object",
                    Some(AiTask::DetectObjects) => "Detect Objects",
                    Some(AiTask::TrackObject) => "Track Object",
                };
                egui::ComboBox::from_id_salt("ai_models_filter_task")
                    .selected_text(task_label)
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                state.ai_models_dialog.filter_task.is_none(),
                                "All tasks",
                            )
                            .clicked()
                        {
                            state.ai_models_dialog.filter_task = None;
                        }
                        for task in AiTask::ALL {
                            let is_selected =
                                state.ai_models_dialog.filter_task == Some(*task);
                            if ui
                                .selectable_label(is_selected, task.display_label())
                                .clicked()
                            {
                                state.ai_models_dialog.filter_task = Some(*task);
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("SORT")
                        .small()
                        .strong()
                        .color(theme.text_dim),
                );
                egui::ComboBox::from_id_salt("ai_models_sort")
                    .selected_text(state.ai_models_dialog.sort.label())
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for s in ModelSort::ALL {
                            let is_selected = state.ai_models_dialog.sort == *s;
                            if ui.selectable_label(is_selected, s.label()).clicked() {
                                state.ai_models_dialog.sort = *s;
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.checkbox(
                    &mut state.ai_models_dialog.installed_only,
                    "Installed only",
                );
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.ai_models_dialog.search)
                                .hint_text("🔍 Filter models…")
                                .desired_width(200.0),
                        );
                    },
                );
            });
            ui.add_space(6.0);
            ui.separator();

            // Build the filtered + sorted list of manifest entries
            // once before render so the comparator runs once per
            // frame, not per row. Telemetry lookups in the LastUsed
            // comparator stay clean of borrow-checker drama.
            let visible_entries: Vec<&ModelEntry> = filter_and_sort(
                &manifest,
                cache.as_ref(),
                &state.ai_models_dialog,
                &state.ai_tools_state.model_telemetry,
            );
            // Custom models go through the same filter so the task
            // dropdown and Installed-only checkbox affect both
            // sections. The sort applies independently within each.
            let visible_custom: Vec<crate::ai_tools::persistence::CustomModelEntry> =
                filter_and_sort_custom(
                    &state.ai_tools_state.custom_models,
                    cache.as_ref(),
                    &state.ai_models_dialog,
                    &state.ai_tools_state.model_telemetry,
                );
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .max_height(scroll_max_h)
                .show(ui, |ui| {
                    if visible_entries.is_empty() && visible_custom.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(
                                    if manifest.models.is_empty() {
                                        "No models declared in manifest."
                                    } else {
                                        "No models match the current filter."
                                    },
                                )
                                .color(theme.text_dim),
                            );
                        });
                    }
                    // Group by ModelKind so each kind gets its own
                    // section header ("MASK GENERATOR", "OBJECT
                    // DETECTOR", "VIDEO TRACKER") with a count.
                    // Stable iteration order: MaskGenerator first,
                    // then ObjectDetector, then VideoTracker.
                    use klipp_ml::manifest::ModelKind;
                    let kinds_in_order: &[(ModelKind, &str)] = &[
                        (ModelKind::MaskGenerator, "MASK GENERATOR"),
                        (ModelKind::ObjectDetector, "OBJECT DETECTOR"),
                        (ModelKind::VideoTracker, "VIDEO TRACKER"),
                        (ModelKind::Transcriber, "TRANSCRIBER"),
                        (ModelKind::TextDetector, "SCREEN TEXT (OCR)"),
                        (ModelKind::TextRecognizer, "SCREEN TEXT (OCR)"),
                        (ModelKind::FaceDetector, "FACES"),
                        (ModelKind::Voice, "NARRATION (VOICE)"),
                        (ModelKind::SpeechEnhancer, "VOICE ISOLATION"),
                        (ModelKind::VoiceActivity, "SPEECH DETECTION"),
                        (ModelKind::Translator, "CAPTION TRANSLATION"),
                        (ModelKind::TranslationVocabulary, "CAPTION TRANSLATION"),
                        (ModelKind::SoundTagger, "AUDIO ROLES"),
                        (ModelKind::ImageTextEmbedder, "PICTURE SEARCH"),
                    ];
                    for (kind, header) in kinds_in_order {
                        let entries: Vec<&ModelEntry> = visible_entries
                            .iter()
                            .copied()
                            .filter(|e| e.kind == *kind)
                            .collect();
                        if entries.is_empty() {
                            continue;
                        }
                        section_header(ui, header, entries.len(), theme);
                        for &entry in &entries {
                            let status = if entry.size_bytes == 0 {
                                ModelInstallStatus::Pending
                            } else {
                                match &cache {
                                    Some(c) => install_status_for(c, &manifest, entry),
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
                                cache.as_ref(),
                                &mut state.model_download,
                                &state.ai_tools_state.model_telemetry,
                                &mut state.show_ai_model_uninstall_confirm,
                                &mut state.toast,
                                &mut state.ai_models_dialog.details_for,
                            );
                        }
                    }

                    // ── User-added custom models ────────────────────
                    // Rendered as a distinct section after the
                    // manifest entries so users can see at a glance
                    // which models were pasted vs. which Klipp
                    // ships. Hidden when the filter excludes every
                    // custom entry — including when the user picks
                    // a task that no custom model qualifies for.
                    if !visible_custom.is_empty() {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Custom models")
                                .strong()
                                .color(theme.text_secondary),
                        );
                        ui.add_space(2.0);
                        ui.separator();
                        for entry in &visible_custom {
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

            // ── Footer ──
            // Cache size + GPU status pinned to the left in caps;
            // primary actions on the right edge. Uninstall all and
            // Uninstall unused are reachable via the more verbose
            // confirmations they always required, kept here as
            // text-button links.
            let installed_count = match cache.as_ref() {
                Some(c) => manifest
                    .models
                    .iter()
                    .filter(|e| e.size_bytes > 0
                        && !manifest.is_part(&e.id)
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
                            && !manifest.is_part(&e.id)
                            && c.path_for(e).metadata().is_ok()
                            && is_stale(e, &state.ai_tools_state.model_telemetry)
                    })
                    .count(),
                None => 0,
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("CACHE   {}", format_bytes(total_cache_bytes)))
                        .small()
                        .strong()
                        .color(theme.text_secondary),
                );
                ui.add_space(8.0);
                let (accel_text, accel_color, accel_tooltip) = inference_status_label(theme);
                ui.label(
                    egui::RichText::new(accel_text)
                        .small()
                        .strong()
                        .color(accel_color),
                )
                .on_hover_text(accel_tooltip);
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Done").clicked() {
                            close_requested = true;
                        }
                        ui.add_space(8.0);
                        if ui
                            .add_enabled(
                                installed_count > 0,
                                egui::Button::new(
                                    egui::RichText::new("Verify all").strong(),
                                ),
                            )
                            .on_hover_text(
                                "Re-hash every installed model and toast a \
                                 summary of matches vs mismatches.",
                            )
                            .on_disabled_hover_text("No models installed")
                            .clicked()
                        {
                            verify_all(&manifest, cache.as_ref(), &mut state.toast);
                        }
                        ui.add_space(4.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Add custom model…")
                                        .color(theme.text_secondary),
                                )
                                .frame(false),
                            )
                            .clicked()
                            && matches!(
                                state.custom_model_add,
                                crate::ai_tools::custom_install::CustomInstallState::Idle
                            ) {
                                state.custom_model_add =
                                    crate::ai_tools::custom_install::CustomInstallState::Form(
                                        Default::default(),
                                    );
                            }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Open cache folder")
                                        .color(theme.text_secondary),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            if let Some(c) = cache.as_ref() {
                                let _ = open_in_file_browser(c.root());
                            }
                        }
                        // Less-frequent actions still reachable; kept
                        // as small text links so they don't crowd the
                        // primary Verify all / Done buttons.
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Edit user override…")
                                        .small()
                                        .color(theme.text_dim),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            if let Err(e) = open_user_override(&mut state.toast) {
                                state
                                    .toast
                                    .show(format!("Couldn't open user override: {e}"));
                            }
                        }
                        if ui
                            .add_enabled(
                                installed_count > 0,
                                egui::Button::new(
                                    egui::RichText::new("Uninstall all")
                                        .small()
                                        .color(theme.text_dim),
                                )
                                .frame(false),
                            )
                            .on_disabled_hover_text("No models installed")
                            .clicked()
                        {
                            state.show_ai_models_bulk_uninstall_confirm = true;
                        }
                        if ui
                            .add_enabled(
                                stale_count > 0,
                                egui::Button::new(
                                    egui::RichText::new("Uninstall unused")
                                        .small()
                                        .color(theme.text_dim),
                                )
                                .frame(false),
                            )
                            .on_disabled_hover_text(
                                "No installed models are stale yet (used in \
                                 the last 30 days, or never used).",
                            )
                            .clicked()
                        {
                            state.show_ai_models_stale_uninstall_confirm = true;
                        }
                    },
                );
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
    show_model_details(ctx, state, theme, &manifest);
}

/// Read-only details modal triggered from the row's three-dot menu
/// "Show details" item. Renders every field from the manifest entry
/// — everything Copy model info would put in the clipboard, but
/// laid out for reading. Closes on scrim click or Done button.
fn show_model_details(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    manifest: &klipp_ml::manifest::ModelManifest,
) {
    let Some(id) = state.ai_models_dialog.details_for.clone() else {
        return;
    };
    // First check manifest, then custom models — supports both.
    let manifest_entry = manifest.models.iter().find(|e| e.id == id).cloned();
    let custom_entry = state
        .ai_tools_state
        .custom_models
        .iter()
        .find(|e| e.id == id)
        .cloned();
    if manifest_entry.is_none() && custom_entry.is_none() {
        // Stale id — entry was removed between renders. Drop the
        // flag silently rather than render an empty modal.
        state.ai_models_dialog.details_for = None;
        return;
    }

    let screen_rect = ctx.screen_rect();
    egui::Area::new(egui::Id::new("ai_model_details_scrim"))
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
                state.ai_models_dialog.details_for = None;
            }
        });

    let mut close = false;
    egui::Window::new("Model details")
        .collapsible(false)
        .resizable(false)
        .fixed_size([520.0, 360.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            if let Some(e) = &manifest_entry {
                ui.label(egui::RichText::new(&e.display_name).strong().size(13.0));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("ID  {}", e.id))
                        .small()
                        .color(theme.text_dim),
                );
                ui.add_space(8.0);
                detail_row(ui, theme, "Kind", kind_label(e.kind));
                detail_row(ui, theme, "License", &e.license);
                detail_row(ui, theme, "Attribution", &e.attribution);
                detail_row(ui, theme, "Size", &format_bytes(e.size_bytes));
                detail_row(ui, theme, "SHA-256", &e.sha256);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Sources")
                        .small()
                        .strong()
                        .color(theme.text_secondary),
                );
                for s in &e.sources {
                    ui.label(
                        egui::RichText::new(&s.url)
                            .small()
                            .monospace()
                            .color(theme.text_secondary),
                    );
                }
                let desc = description_for(&e.id);
                if !desc.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(desc)
                            .small()
                            .color(theme.text_secondary),
                    );
                }
            } else if let Some(e) = &custom_entry {
                ui.label(egui::RichText::new(&e.display_name).strong().size(13.0));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("ID  {}", e.id))
                        .small()
                        .color(theme.text_dim),
                );
                ui.add_space(8.0);
                detail_row(ui, theme, "Adapter", e.adapter.display_name());
                detail_row(ui, theme, "Size", &format_bytes(e.size_bytes));
                detail_row(ui, theme, "SHA-256", &e.sha256);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Source")
                        .small()
                        .strong()
                        .color(theme.text_secondary),
                );
                ui.label(
                    egui::RichText::new(&e.url)
                        .small()
                        .monospace()
                        .color(theme.text_secondary),
                );
            }
            ui.add_space(12.0);
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if ui.button("Done").clicked() {
                        close = true;
                    }
                },
            );
        });
    if close {
        state.ai_models_dialog.details_for = None;
    }
}

fn detail_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .small()
                .strong()
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(value)
                .small()
                .color(theme.text_secondary),
        );
    });
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

/// Bulk-uninstall confirmation modal. Lists every installed model +
/// the total disk space freed; on confirm, loops file delete +
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
        .filter(|e| e.size_bytes > 0 && !manifest.is_part(&e.id))
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
        .filter(|e| e.size_bytes > 0 && !manifest.is_part(&e.id) && is_stale(e, &state.ai_tools_state.model_telemetry))
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
    let manifest = klipp_ml::manifest::load_effective().ok();
    let mut deleted = 0u32;
    let mut errors: Vec<String> = Vec::new();
    let mut freed: u64 = 0;
    for (entry, size) in installed {
        // Drop the cached generator before the file delete so the
        // next dispatch rebuilds rather than reusing a stale Box.
        if let Ok(g) = registry.read() {
            let _ = g.clear_instance(&entry.id);
        }
        // The files that come with it go with it.
        for part in manifest.iter().flat_map(|m| m.parts(entry).into_iter().skip(1)) {
            if let Ok(m) = std::fs::metadata(cache.path_for(part)) {
                if std::fs::remove_file(cache.path_for(part)).is_ok() {
                    freed += m.len();
                }
            }
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

    let row_h = 52.0;
    let right_reserve = 220.0_f32; // size + badge + Uninstall button
    let avail = ui.available_width();
    let left_w = (avail - right_reserve - 28.0).max(200.0);

    egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_min_height(row_h);
            ui.horizontal(|ui| {
                paint_status_dot(ui, badge_color);
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, row_h),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_max_width(left_w);
                        ui.label(
                            egui::RichText::new(&entry.display_name)
                                .strong()
                                .size(13.0),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{}   Custom",
                                entry.adapter.display_name(),
                            ))
                            .small()
                            .color(theme.text_secondary),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&entry.url)
                                    .small()
                                    .color(theme.text_dim),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&entry.url);
                        if let Some(t) = telemetry.get(&entry.id) {
                            if t.has_runs() {
                                let avg = t
                                    .avg_inference_ms()
                                    .map(|ms| format!("avg {:.0}ms", ms))
                                    .unwrap_or_else(|| "—".into());
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Last used {} · {}",
                                        crate::ai_tools::format_relative(t.last_used),
                                        avg,
                                    ))
                                    .small()
                                    .color(theme.text_dim),
                                );
                            }
                        }
                    },
                );

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
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
                            .stroke(egui::Stroke::new(1.0_f32, badge_color))
                            .rounding(2.0)
                            .inner_margin(egui::Margin::symmetric(6.0, 2.0));
                        badge.show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(badge_label)
                                    .small()
                                    .color(badge_color),
                            );
                        });
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format_bytes(entry.size_bytes))
                                .small()
                                .color(theme.text_secondary),
                        );
                    },
                );
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

    // Drop the entry entirely so the panel dropdown stops listing
    // it immediately. unregister cleans up both the factory and
    // the cached Box (Entry's Mutex<Option<Box>> goes out of scope
    // on entries.remove). For built-in manifest entries this
    // would be wrong — we want them to stay registered after
    // single-row Uninstall so the dialog can offer Download again
    // — but custom entries are user-owned and should fully exit
    // the app's view.
    let registry = state.ai_tools_registry.shared();
    if let Ok(mut g) = registry.write() {
        g.unregister(&entry.id);
    }

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
            // HEAD pre-flight — best-effort. Single round-trip
            // (~100ms for a healthy server) gives us two wins:
            //   1. Progress bar renders determinate from the first
            //      body chunk instead of indeterminate.
            //   2. Fast-fail on over-cap files before any body
            //      bytes flow — saves the user's time + bandwidth
            //      when a URL is wrong.
            // HEAD failures don't abort — some servers reject HEAD
            // outright. Fall through to the GET; the streaming
            // size guard inside write_and_compute still bails on
            // cap.
            let head_total: u64 = if cancel_for_worker.load(Ordering::Relaxed) {
                0
            } else {
                match klipp_ml::head_content_length(&url) {
                    Some(content_length) => {
                        if content_length > CUSTOM_DOWNLOAD_MAX_BYTES {
                            let payload = CustomDownloadResult::Err(format!(
                                "Server reports {} byte(s); Klipp's per-download \
                                 cap is {} byte(s)",
                                content_length, CUSTOM_DOWNLOAD_MAX_BYTES,
                            ));
                            let _ = tx.send(CustomDownloadEvent::Completed(payload));
                            ctx_for_worker.request_repaint();
                            return;
                        }
                        // Hint the total to the UI before any body
                        // chunks flow. `downloaded: 0` stays put;
                        // streaming updates it from there.
                        let _ = tx.send(CustomDownloadEvent::Progress {
                            downloaded: 0,
                            total: content_length,
                        });
                        content_length
                    }
                    None => 0,
                }
            };

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
                    // write_and_compute calls started(0) — preserve
                    // any HEAD-derived total instead of zeroing it.
                    if total_bytes > 0 {
                        self.total = total_bytes;
                    }
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
                total: head_total,
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

    // A row stands for the files that come with it (the Kokoro voices,
    // a Piper voice's settings, a model's `with` parts), and they go
    // with it.
    if let Ok(manifest) = klipp_ml::manifest::load_effective() {
        for extra in klipp_audio::narration::extra_files(c, &manifest, &entry.id) {
            // The Piper engine's is the folder it was unpacked into.
            let _ = if extra.is_dir() { std::fs::remove_dir_all(extra) } else { std::fs::remove_file(extra) };
        }
        for part in manifest.parts(entry).into_iter().skip(1) {
            let _ = std::fs::remove_file(c.path_for(part));
        }
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
    cache: Option<&ModelCache>,
    download: &mut ModelDownloadState,
    telemetry: &HashMap<String, ModelTelemetry>,
    pending_uninstall: &mut Option<ModelEntry>,
    toast: &mut crate::widgets::toast::ToastState,
    details_for: &mut Option<String>,
) {
    let row_h = 52.0;
    let dot_color = status.badge_color(theme);
    // Reserve the right column up front so the left block can be
    // explicitly width-bounded — without this the vertical block
    // fills available_width and the second line either pushes the
    // right column off-screen or wraps to multiple lines.
    // Empirical numbers: size column ~70px, badge ~80px, primary
    // action button ~80px, overflow ⋮ ~24px, plus inter-element
    // spacing ~30px → ~290px reserve.
    let right_reserve = 290.0_f32;
    let avail = ui.available_width();
    // Leave room for the status dot (~18px) + a small margin.
    let left_w = (avail - right_reserve - 28.0).max(200.0);
    egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_min_height(row_h);
            ui.horizontal(|ui| {
                // Status dot — small + vertically centered. Same
                // colour mapping as the status badge so the two
                // signals reinforce.
                paint_status_dot(ui, dot_color);

                // Centre block: name + (optional) RECOMMENDED + (i),
                // and a kind/license/attribution line below. Width
                // is bounded so the second-line label can truncate
                // cleanly with an ellipsis when attribution is long.
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, row_h),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_max_width(left_w);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&entry.display_name)
                                    .strong()
                                    .size(13.0),
                            );
                            if is_recommended(&entry.id) {
                                recommended_badge(ui, theme);
                            }
                            let desc = description_for(&entry.id);
                            if !desc.is_empty() {
                                // Frameless button so the (i)
                                // glyph reads as a chip but still
                                // takes click + hover senses.
                                // Hover shows a one-line preview;
                                // click opens the same Show details
                                // modal the kebab menu's "Show
                                // details" item targets, so users
                                // get to the full metadata via
                                // either path.
                                let resp = ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("ⓘ")
                                                .size(12.0)
                                                .color(theme.text_dim),
                                        )
                                        .frame(false),
                                    )
                                    .on_hover_text(desc);
                                if resp.clicked() {
                                    *details_for = Some(entry.id.clone());
                                }
                            }
                        });
                        let line = format!(
                            "{}   {}   {}",
                            kind_label(entry.kind),
                            entry.license,
                            entry.attribution,
                        );
                        // Single-line label that ellipsizes on
                        // overflow; full text revealed on hover so
                        // long attributions stay accessible.
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&line)
                                    .small()
                                    .color(theme.text_secondary),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&line);
                        if let Some(t) = telemetry.get(&entry.id) {
                            if t.has_runs() {
                                let avg = t
                                    .avg_inference_ms()
                                    .map(|ms| format!("avg {:.0}ms", ms))
                                    .unwrap_or_else(|| "—".into());
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Last used {} · {}",
                                        format_relative(t.last_used),
                                        avg,
                                    ))
                                    .small()
                                    .color(theme.text_dim),
                                );
                            }
                        }
                    },
                );

                // Right block: size + status badge + primary action +
                // overflow menu.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Three-dot overflow — only meaningful for
                        // non-Pending statuses (Pending entries have
                        // nothing to install/uninstall/verify yet).
                        if !matches!(status, ModelInstallStatus::Pending) {
                            row_overflow_menu(
                                ui,
                                entry,
                                status,
                                theme,
                                cache,
                                download,
                                pending_uninstall,
                                toast,
                                details_for,
                            );
                        }

                        // Primary action button on the right edge.
                        primary_action_button(
                            ui, entry, status, theme, download,
                            pending_uninstall, toast,
                        );

                        ui.add_space(8.0);
                        status_badge(ui, status, theme);

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format_bytes(entry.size_bytes))
                                .small()
                                .color(theme.text_secondary),
                        );
                    },
                );
            });
        });
}

/// Small-caps section header rendered above each ModelKind group
/// in the dialog. Format: "MASK GENERATOR  4" — caps name +
/// count badge.
fn section_header(ui: &mut egui::Ui, header: &str, count: usize, theme: &KlippTheme) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(header)
                .small()
                .strong()
                .color(theme.text_dim),
        );
        ui.add_space(4.0);
        let badge = egui::Frame::none()
            .fill(theme.bg_raised)
            .rounding(2.0)
            .inner_margin(egui::Margin::symmetric(5.0, 0.5));
        badge.show(ui, |ui| {
            ui.label(
                egui::RichText::new(count.to_string())
                    .small()
                    .color(theme.text_secondary),
            );
        });
    });
    ui.add_space(2.0);
}

/// Small filled circle used as the per-row status indicator. The
/// fill colour mirrors the status badge so the two visuals
/// reinforce — green = installed, amber = mismatch, blue = pending,
/// dim grey = missing/unknown.
fn paint_status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(10.0, 10.0),
        egui::Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Inline pill for recommended entries. Yellow fill + dark text,
/// distinct from the status badge so the two read as different
/// signals.
fn recommended_badge(ui: &mut egui::Ui, theme: &KlippTheme) {
    let bg = theme.marker_warning;
    egui::Frame::none()
        .fill(bg)
        .rounding(2.0)
        .inner_margin(egui::Margin::symmetric(6.0, 1.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("RECOMMENDED")
                    .small()
                    .strong()
                    .color(egui::Color32::BLACK),
            );
        });
}

/// Status badge to the right of the size column. Same shape the
/// previous design used — kept consistent so the visual cue is
/// stable across the rewrite.
fn status_badge(
    ui: &mut egui::Ui,
    status: &ModelInstallStatus,
    theme: &KlippTheme,
) {
    let color = status.badge_color(theme);
    egui::Frame::none()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .rounding(2.0)
        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(status.label().to_uppercase())
                    .small()
                    .color(color),
            );
        });
}

/// One-button on the right edge: Verify when the entry is
/// installed, Download when missing/unknown, Reinstall on Mismatch.
/// Pending entries get a disabled Download button.
fn primary_action_button(
    ui: &mut egui::Ui,
    entry: &ModelEntry,
    status: &ModelInstallStatus,
    _theme: &KlippTheme,
    download: &mut ModelDownloadState,
    _pending_uninstall: &mut Option<ModelEntry>,
    toast: &mut crate::widgets::toast::ToastState,
) {
    match status {
        ModelInstallStatus::Pending => {
            ui.add_enabled(false, egui::Button::new("Download"))
                .on_disabled_hover_text(
                    "Awaiting Klipp release-CI mirror — the file isn't \
                     downloadable yet.",
                );
        }
        ModelInstallStatus::Missing | ModelInstallStatus::Unknown(_) => {
            if ui
                .add_enabled(
                    matches!(download, ModelDownloadState::Idle),
                    egui::Button::new("Download"),
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
            if ui
                .button("Verify")
                .on_hover_text(
                    "Re-hash the file on disk and confirm it matches \
                     the manifest's SHA-256.",
                )
                .clicked()
            {
                verify_now(entry, toast);
            }
        }
    }
}

/// Three-dot overflow menu for the row. Items follow the F-97-
/// MODEL-MANAGER design mockup:
///   • Show details   — opens a modal with full id/sha/url/license.
///   • Verify integrity — same as the row's Verify button (re-hash).
///   • Reinstall       — fires the consent + download flow.
///   • Show in cache folder — xdg-opens the parent dir.
///   • Copy model info — clipboard copy of structured metadata.
///   • Uninstall...    — destructive; stages the confirm modal.
fn row_overflow_menu(
    ui: &mut egui::Ui,
    entry: &ModelEntry,
    status: &ModelInstallStatus,
    theme: &KlippTheme,
    cache: Option<&ModelCache>,
    download: &mut ModelDownloadState,
    pending_uninstall: &mut Option<ModelEntry>,
    toast: &mut crate::widgets::toast::ToastState,
    details_for: &mut Option<String>,
) {
    // Use egui's built-in popup memory so the show + close
    // semantics line up with the rest of the app (Esc closes,
    // clicking outside closes, click-on-button toggles). The
    // earlier external-state approach didn't drive
    // popup_below_widget's internal is_popup_open flag, which is
    // why the menu never appeared.
    let response = ui
        .add(
            egui::Button::new(egui::RichText::new("⋮").size(16.0))
                .frame(false),
        )
        .on_hover_text("More actions");
    let popup_id = ui.make_persistent_id(("ai_row_overflow", &entry.id));
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &response,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(220.0);
            menu_item(ui, "ⓘ", "Show details", false, theme, || {
                *details_for = Some(entry.id.clone());
            });
            if matches!(status, ModelInstallStatus::Present { .. }) {
                menu_item(ui, "✓", "Verify integrity", false, theme, || {
                    verify_now(entry, toast);
                });
            }
            if !matches!(status, ModelInstallStatus::Pending) {
                menu_item(ui, "↻", "Reinstall", false, theme, || {
                    if matches!(download, ModelDownloadState::Idle) {
                        *download = ModelDownloadState::Consenting {
                            entry: entry.clone(),
                        };
                    } else {
                        toast.show("Another download is in progress");
                    }
                });
            }
            if matches!(status, ModelInstallStatus::Present { .. }) {
                menu_item(ui, "📁", "Show in cache folder", false, theme, || {
                    if let Some(c) = cache {
                        let _ = open_in_file_browser(c.root());
                    }
                });
            }
            // Clone ctx out before the menu_item closure so the
            // closure captures `ctx` (by move) instead of `ui` —
            // ui is mutably borrowed by menu_item itself.
            let ctx_for_copy = ui.ctx().clone();
            menu_item(ui, "⎘", "Copy model info", false, theme, || {
                let info = format!(
                    "id: {}\ndisplay_name: {}\nkind: {}\nlicense: {}\n\
                     attribution: {}\nsize_bytes: {}\nsha256: {}\nsources: {}",
                    entry.id,
                    entry.display_name,
                    kind_label(entry.kind),
                    entry.license,
                    entry.attribution,
                    entry.size_bytes,
                    entry.sha256,
                    entry
                        .sources
                        .iter()
                        .map(|s| s.url.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                ctx_for_copy.copy_text(info);
                toast.show(format!("Copied {} info to clipboard", entry.display_name));
            });
            if matches!(status, ModelInstallStatus::Present { .. }) {
                ui.separator();
                menu_item(ui, "🗑", "Uninstall…", true, theme, || {
                    *pending_uninstall = Some(entry.clone());
                });
            }
        },
    );
}

/// One row inside the overflow popup: glyph + label, full-width
/// click target, optional destructive-red colour. The glyph is a
/// monospace unicode symbol so we don't depend on any icon font.
/// Replaces a plain `ui.button(label)` so all rows have a
/// consistent left-glyph + label-text alignment.
fn menu_item(
    ui: &mut egui::Ui,
    glyph: &str,
    label: &str,
    destructive: bool,
    theme: &KlippTheme,
    on_click: impl FnOnce(),
) {
    let color = if destructive {
        theme.marker_error
    } else {
        ui.style().visuals.text_color()
    };
    let resp = ui.add(
        egui::Button::new(
            egui::RichText::new(format!("  {glyph}    {label}"))
                .color(color),
        )
        .frame(false)
        .min_size(egui::vec2(220.0, 22.0)),
    );
    if resp.clicked() {
        on_click();
    }
}

/// User-friendly label for `ModelKind`. The enum's snake_case serde
/// names are wire-format-friendly but not great in UI.
fn kind_label(kind: klipp_ml::manifest::ModelKind) -> &'static str {
    use klipp_ml::manifest::ModelKind;
    match kind {
        ModelKind::MaskGenerator => "Mask Generator",
        ModelKind::VideoTracker => "Video Tracker",
        ModelKind::ObjectDetector => "Object Detector",
        ModelKind::Transcriber => "Transcriber",
        ModelKind::TextDetector => "Text Detector (OCR)",
        ModelKind::TextRecognizer => "Text Recognizer (OCR)",
        ModelKind::Voice => "Narration Voice",
        ModelKind::FaceDetector => "Face Detector",
        ModelKind::SpeechEnhancer => "Voice Isolation",
        ModelKind::VoiceActivity => "Speech Detection",
        ModelKind::Translator => "Caption Translator",
        ModelKind::TranslationVocabulary => "Translation Vocabulary",
        ModelKind::SoundTagger => "Audio Roles",
        ModelKind::ImageTextEmbedder => "Picture Search",
    }
}

/// Open `path` in the user's default file browser. Best-effort —
/// failures fall through silently because there's nothing useful
/// the dialog can do about it.
/// Footer cell describing inference acceleration. Returns
/// `(label, color, hover_tooltip)`. Combines two layers of info:
///
/// 1. **Vendor probe** (runtime): does this machine have an
///    NVIDIA / AMD / Intel GPU? Resolved via `lspci` / `/sys/`
///    in `klipp_ml_ort::resolve_runtime_status`.
/// 2. **Compile-time feature**: was Klipp built with
///    `ai-ort-cuda`? Without it, even an NVIDIA user gets CPU
///    inference because the EP isn't registered.
///
/// Combinations the user sees:
///   * NVIDIA + cuda feature on  → "GPU (NVIDIA)" neutral
///   * NVIDIA + cuda feature off → "⚠ Built without GPU support"
///   * NVIDIA + cuda + libort missing → "⚠ NVIDIA — runtime missing"
///   * AMD / Intel              → "⚠ {vendor} — coming soon"
///   * No GPU / probe failed    → "CPU only" neutral
#[cfg(not(feature = "ai-ort"))]
fn inference_status_label(theme: &KlippTheme) -> (String, egui::Color32, String) {
    (
        "CPU only".into(),
        theme.text_secondary,
        "Klipp built without --features ai-ort; AI Tools \
         inference uses Klipp's stub generator only."
            .into(),
    )
}

/// What each model opened so far runs on, a line each after a blank
/// one, or nothing before one has opened (#2250).
#[cfg(feature = "ai-ort")]
fn models_on() -> String {
    let lines: Vec<String> = klipp_ml_ort::provider::used().iter().map(|u| u.line()).collect();
    if lines.is_empty() {
        String::new()
    } else {
        format!("\n\nModels opened:\n{}", lines.join("\n"))
    }
}

#[cfg(feature = "ai-ort")]
fn inference_status_label(theme: &KlippTheme) -> (String, egui::Color32, String) {
    use klipp_ml_ort::{GpuVendor, RuntimeStatus};
    let runtime = klipp_ml_ort::resolve_runtime_status();
    // What to install for each vendor when the runtime can't use its GPU.
    let install = |v: GpuVendor| match v {
        GpuVendor::Nvidia => "Install a GPU build of onnxruntime (onnxruntime-cuda, with CUDA and cuDNN), or set ORT_DYLIB_PATH to a CUDA or WebGPU build.",
        _ => "Set ORT_DYLIB_PATH to a WebGPU build of onnxruntime, which runs on any vendor's GPU.",
    };

    // Every state with its text, colour and tooltip; the tooltip says
    // what to do about it.
    match runtime {
        RuntimeStatus::CpuOnly => (
            "CPU only".into(),
            theme.text_secondary,
            "No GPU detected on this machine. Klipp's AI Tools \
             will run inference on the CPU EP."
                .into(),
        ),
        RuntimeStatus::GpuAccelerationActive(v) => (
            format!("GPU ({})", v.display_label()),
            theme.text_secondary,
            format!(
                "AI Tools and analysis run on the GPU through {}. A \
                 model whose GPU provider doesn't load runs on the CPU, \
                 listed below.{}",
                klipp_ml_ort::provider::gpu_provider().unwrap_or("its GPU provider"),
                models_on(),
            ),
        ),
        RuntimeStatus::GpuAvailableNotInstalled(v) => (
            format!("⚠ {} — runtime missing", v.display_label()),
            theme.marker_warning,
            format!(
                "{} GPU detected, but no libonnxruntime.so resolved \
                 (checked ORT_DYLIB_PATH + standard install paths). {}",
                v.display_label(),
                install(v),
            ),
        ),
        RuntimeStatus::GpuRuntimeCpuOnly(v) => (
            format!("⚠ {} — CPU-only runtime", v.display_label()),
            theme.marker_warning,
            format!(
                "{} GPU detected, but the onnxruntime library in use has \
                 no provider for it: AI Tools and analysis run on the \
                 CPU. {}{}",
                v.display_label(),
                install(v),
                models_on(),
            ),
        ),
    }
}

fn open_in_file_browser(path: &std::path::Path) -> std::io::Result<()> {
    crate::ui_util::open_external(path)
}

/// Open `~/.config/klipp/models.toml` in the user's default editor.
/// Creates the file from a comment template if it doesn't exist
/// yet — without that, xdg-open would either error or open a
/// blank file with no schema hints.
///
/// Used by air-gapped / private-mirror installs that hand-edit
/// the user override file. The file's merge semantics are already
/// handled by `klipp_ml::manifest::load_effective`; this just
/// gives users a UI affordance to find + edit it.
fn open_user_override(
    toast: &mut crate::widgets::toast::ToastState,
) -> std::io::Result<()> {
    let Some(path) = klipp_ml::manifest::user_manifest_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "config dir unavailable",
        ));
    };
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, USER_OVERRIDE_TEMPLATE)?;
        toast.show(format!(
            "Created {} from template — opening editor",
            path.display()
        ));
    }
    open_in_file_browser(&path)
}

/// Comment template seeded into `~/.config/klipp/models.toml`
/// when the user opens it for the first time. Documents the
/// schema + the merge behaviour so an editor session has the
/// context it needs without external docs.
const USER_OVERRIDE_TEMPLATE: &str = "\
# Klipp AI model user override manifest.
#
# Entries here override (and extend) the bundled manifest at
# klipp/assets/models.toml. Use this file for:
#
#   - Air-gapped installs (point sources at a private mirror)
#   - Local development (real size_bytes + sha256 for entries
#     whose bundled values are still placeholders)
#   - Hand-tuning a bundled entry (e.g. swap to a self-hosted
#     URL while keeping the same id + sha256)
#
# Schema: https://github.com/davidkodar/klipp/blob/main/crates/klipp-ml/src/manifest.rs
#
# Each entry needs:
#   - id            stable identifier matching the bundled entry
#                   you want to override (or a new id for an
#                   extension entry).
#   - display_name  user-facing label.
#   - size_bytes    expected file size (bytes).
#   - sha256        64 lowercase hex chars; the security boundary.
#   - license       SPDX identifier.
#   - attribution   credit shown in the consent dialog.
#   - sources       ordered list of HTTPS URLs.
#   - kind          mask_generator / video_tracker / object_detector.
#   - runtime       ort / candle.
#   - prompts       \"point | box | scribble | auto_detect | temporal\"
#                   (any combination separated by `|`).

version = \"1\"

# Example: override the bundled BiRefNet entry to point at a
# self-hosted mirror while keeping the published sha256.
#
# [[model]]
# id = \"birefnet_lite_v1\"
# display_name = \"BiRefNet Lite (private mirror)\"
# size_bytes = 224005088
# sha256 = \"5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333\"
# license = \"MIT\"
# attribution = \"ZhengPeng7 / BiRefNet\"
# sources = [
#     { url = \"https://my-private-mirror.example.com/birefnet_lite_v1.onnx\" },
# ]
# kind = \"mask_generator\"
# runtime = \"ort\"
# prompts = \"\"
";

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

    /// A model that comes with another file is one row, installed only
    /// with both, its size theirs together (#2245).
    #[test]
    fn a_model_and_its_parts_are_one_row() {
        let mut enc = make_entry("enc");
        enc.with = vec!["dec".into()];
        let manifest = klipp_ml::manifest::ModelManifest { version: "1".into(), models: vec![enc.clone(), make_entry("dec")] };
        let dir = std::env::temp_dir().join(format!("klipp-model-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = ModelCache::open_at(&dir);
        std::fs::write(cache.path_for_id("enc"), [0u8; 30]).unwrap();
        assert_eq!(install_status_for(&cache, &manifest, &enc), ModelInstallStatus::Missing, "the decoder is missing");
        std::fs::write(cache.path_for_id("dec"), [0u8; 12]).unwrap();
        assert_eq!(install_status_for(&cache, &manifest, &enc), ModelInstallStatus::Present { size_bytes: 42 });
        let rows = filter_and_sort(&manifest, Some(&cache), &AiModelsDialogState::default(), &HashMap::new());
        assert_eq!(rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["enc"]);
        let _ = std::fs::remove_dir_all(&dir);
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
            with: Vec::new(),
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
