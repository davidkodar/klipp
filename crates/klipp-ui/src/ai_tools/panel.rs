//! egui rendering for the AI Tools panel.
//!
//! ## State machine + visual posture
//!
//! Three visual states the panel renders, switching off
//! `state.inference`:
//!
//! * **Idle** — TASK / MODEL / PROMPTS / RUN / OUTPUT (empty
//!   placeholder) / ADVANCED. The user picks a task, picks a
//!   model, captures prompts via the viewer, and clicks Generate.
//! * **Running** *(commit 2)* — same skeleton, but tiles compress,
//!   model row swaps to a spinner + "Inferring on …" sub-line, a
//!   progress bar replaces the Generate button, and OUTPUT shows
//!   a static prompt overlay on the source frame.
//! * **Complete** *(commit 3)* — banner with frame + duration,
//!   OUTPUT shows mask preview with display-mode tabs (Overlay /
//!   Mask only / Cutout), Re-run + Apply-to-clip buttons, Recent
//!   list of prior inferences below.
//!
//! Commit 1 (this file's current shape) lands the visual frame
//! for the idle state. The wiring for running/complete is
//! follow-up commits.
//!
//! ## Panel ↔ dispatcher contract
//!
//! The panel never touches the worker thread. It only ever
//! mutates state on `AiToolsState`:
//!
//! * `pending_run = true` → app dispatcher reads this each frame,
//!   builds an `OwnedMaskRequest` from the live viewer frame, and
//!   calls `spawn_inference`.
//! * `pending_cancel = true` → dispatcher cancels via
//!   `InferenceState::cancel`.
//! * `selected_model_id` / `last_picked_model_per_task` /
//!   `model_tunables` / `scope` / `advanced_settings` are read by
//!   the dispatcher when it builds the request.
//!
//! ## Theme
//!
//! Every colour comes from `KlippTheme` so cream / light / dark
//! variants stay legible. Orange accents reuse `theme.marker_warning`
//! (the existing curated warning hue) rather than a hardcoded
//! orange.

use klipp_ml::ModelInfo;

use crate::icons::Icon;
use crate::theme::KlippTheme;

use super::registry::AiToolsRegistry;
use super::{
    AiScope, AiTask, AiToolsState, DisplayMode, InferenceState, OutputFormat, PromptMode,
};

/// Stateless renderer for the AI Tools panel. Holds nothing —
/// every frame the caller passes in the mutable state + the
/// (read-only) registry + the active theme.
pub struct AiToolsPanel;

impl AiToolsPanel {
    /// Render the panel inside the supplied egui Ui (typically a
    /// `SidePanel` body the caller has already opened). The caller
    /// decides docking. `show_models_dialog` is a write-back flag
    /// the panel flips to `true` when the user clicks "Change…"
    /// or the model card's chevron — opens the AI Models dialog.
    pub fn show(
        ui: &mut egui::Ui,
        state: &mut AiToolsState,
        registry: &AiToolsRegistry,
        theme: &KlippTheme,
        show_models_dialog: &mut bool,
    ) {
        // Hard-cap content width to whatever the SidePanel gives
        // us this frame. Without this, any Frame::none() with
        // inner_margin reports `available_width + margin*2` as
        // its outer size, the SidePanel reads that as "content
        // wants more", and the panel grows by margin*2 each
        // frame until it hits the screen edge. Cap holds the
        // ScrollArea + every nested ui at the parent's width.
        let max_w = ui.available_width();
        ui.set_max_width(max_w);

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.set_max_width(max_w);
                // ── 1. TASK ─────────────────────────────────────────
                section_step_header(ui, 1, "TASK", state.active_task.is_some(), theme);
                task_grid(ui, state, registry, theme);
                if state.can_restore_background && ui.button("Restore Background").on_hover_text("Restore the selected clip's whole picture; keep any matte still used by its grade").clicked() {
                    state.pending_restore_background = true;
                }

                ui.add_space(10.0);
                ui.separator();

                // ── 2. MODEL ────────────────────────────────────────
                let model_active = state
                    .active_task
                    .is_some()
                    && state
                        .selected_model_id
                        .as_ref()
                        .is_some_and(|id| registry.has_id(id));
                // Section header link routes to the plugin
                // manager dialog. "Manage…" rather than "Change…"
                // because the inline picker on the card itself
                // handles ordinary model-switching — this link
                // is for installing / uninstalling / verifying.
                section_step_header_with_action(
                    ui,
                    2,
                    "MODEL",
                    model_active,
                    "Manage…",
                    theme,
                    show_models_dialog,
                );
                model_card(ui, state, registry, theme, show_models_dialog);

                ui.add_space(10.0);
                ui.separator();

                // ── 3. PROMPTS ──────────────────────────────────────
                let prompts_active = !state.prompts.is_empty();
                section_step_header(ui, 3, "PROMPTS", prompts_active, theme);
                prompts_section(ui, state, theme);

                ui.add_space(10.0);
                ui.separator();

                // ── 4. RUN ──────────────────────────────────────────
                section_step_header(ui, 4, "RUN", false, theme);
                run_section(ui, state, registry, theme);

                ui.add_space(10.0);
                ui.separator();

                // ── OUTPUT ──────────────────────────────────────────
                output_section(ui, state, theme);

                ui.add_space(10.0);
                ui.separator();

                // ── ADVANCED (collapsible) ──────────────────────────
                advanced_section(ui, state, theme);
            });
    }
}

// ============================================================
// Section helpers — step badges + headers
// ============================================================

/// Section header with the numbered step badge on the left + the
/// section label in caps on the right. Used for TASK / MODEL /
/// PROMPTS / RUN. `active` flips the badge from grey to orange
/// once the step has user input.
fn section_step_header(
    ui: &mut egui::Ui,
    n: usize,
    label: &str,
    active: bool,
    theme: &KlippTheme,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        step_badge(ui, n, active, theme);
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(label)
                .small()
                .strong()
                .color(theme.text_secondary),
        );
    });
    ui.add_space(2.0);
}

/// Section header with a right-aligned text-link action. Used for
/// MODEL's "Change…" affordance + future cousins. Sets the
/// caller's `action_flag` to true on click.
fn section_step_header_with_action(
    ui: &mut egui::Ui,
    n: usize,
    label: &str,
    active: bool,
    action_label: &str,
    theme: &KlippTheme,
    action_flag: &mut bool,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        step_badge(ui, n, active, theme);
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(label)
                .small()
                .strong()
                .color(theme.text_secondary),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(action_label)
                                .color(theme.marker_warning)
                                .small()
                                .strong(),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    *action_flag = true;
                }
            },
        );
    });
    ui.add_space(2.0);
}

/// Filled circle with the step number centred inside. Active
/// (user has provided input) → marker_warning fill + light text.
/// Inactive → muted bg + dim text.
fn step_badge(ui: &mut egui::Ui, n: usize, active: bool, theme: &KlippTheme) {
    let size = 18.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(size, size),
        egui::Sense::hover(),
    );
    let (fill, fg) = if active {
        (theme.marker_warning, egui::Color32::WHITE)
    } else {
        (theme.bg_raised, theme.text_dim)
    };
    ui.painter().circle_filled(rect.center(), size * 0.5, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        n.to_string(),
        egui::FontId::proportional(11.0),
        fg,
    );
}

// ============================================================
// 1. TASK — 2x2 tile grid
// ============================================================

fn task_grid(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
    theme: &KlippTheme,
) {
    // Compute tile width from the parent's CURRENT available
    // space, leave room for the grid's spacing AND a small safety
    // margin so the row never asks for more than the parent has
    // (otherwise the SidePanel resizer reads "content needs N+x"
    // and grows by x each frame). No `.max()` floor — we let
    // tiles shrink with the panel rather than forcing an
    // overflow.
    let avail = ui.available_width();
    let gap = 8.0;
    let safety = 4.0;
    let tile_w = ((avail - gap - safety) / 2.0).max(80.0);
    // Compressed mode = inference is running. Tiles shrink and
    // drop their description line so the panel can dedicate more
    // vertical real estate to the progress + output sections.
    let compressed = matches!(state.inference, InferenceState::Running { .. });
    let tile_h = if compressed { 56.0 } else { 92.0 };

    egui::Grid::new("ai_tools_task_grid")
        .num_columns(2)
        .spacing([gap, gap])
        .show(ui, |ui| {
            for (i, task) in AiTask::ALL.iter().enumerate() {
                task_tile(ui, *task, tile_w, tile_h, compressed, state, registry, theme);
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
}

/// One 2×2-grid task tile. Active task gets an orange border +
/// faint orange tint. Tasks with no capable model installed are
/// rendered greyed-out with an "install from AI Models" tooltip.
fn task_tile(
    ui: &mut egui::Ui,
    task: AiTask,
    w: f32,
    h: f32,
    compressed: bool,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
    theme: &KlippTheme,
) {
    // Track Subject asks for its model itself, when it runs (#239).
    let has_model = registry.has_model_for(task) || task == AiTask::TrackObject;
    let active = state.active_task == Some(task);

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(w, h),
        if has_model {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );

    // Background fill + border. Active = warning fill (very low
    // alpha) + warning stroke. Hover (when enabled) = subtle
    // raised fill. Disabled = no fill, no stroke.
    let painter = ui.painter();
    let (fill, stroke) = if active {
        (
            theme.marker_warning.gamma_multiply(0.10),
            egui::Stroke::new(1.5_f32, theme.marker_warning),
        )
    } else if has_model && response.hovered() {
        (theme.bg_raised, egui::Stroke::new(1.0_f32, theme.text_dim.gamma_multiply(0.4)))
    } else {
        (theme.bg_raised.gamma_multiply(0.6), egui::Stroke::NONE)
    };
    painter.rect(rect, 6.0, fill, stroke);

    // Content padding inside the tile.
    let pad = 10.0;
    let icon_size = 18.0;
    let icon_color = if !has_model {
        theme.text_dim
    } else if active {
        theme.marker_warning
    } else {
        theme.text_primary
    };

    // Icon at top-left. Render via egui::Image positioned manually
    // so the tile content composes cleanly without a nested
    // horizontal layout.
    let icon_pos = rect.min + egui::vec2(pad, pad);
    let icon_rect = egui::Rect::from_min_size(
        icon_pos,
        egui::vec2(icon_size, icon_size),
    );
    let mut icon_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(icon_rect)
            .layout(egui::Layout::default()),
    );
    icon_ui.add(
        task.icon()
            .image(icon_size)
            .tint(icon_color),
    );

    // Title + description text laid out below the icon.
    let title_pos = icon_pos + egui::vec2(0.0, icon_size + 10.0);
    let title_rect = egui::Rect::from_min_size(
        title_pos,
        egui::vec2(w - pad * 2.0, h - pad - icon_size - 10.0),
    );
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(title_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    let title_color = if has_model {
        theme.text_primary
    } else {
        theme.text_dim
    };
    text_ui.label(
        egui::RichText::new(task.display_label())
            .strong()
            .size(13.0)
            .color(title_color),
    );
    // Drop the description line when the panel is in its
    // compressed (running) mode — the tiles still tell the user
    // which task is selected, but the prose subtitle is what
    // makes idle-mode discoverable. Once running the user has
    // clearly committed to a task already.
    if !compressed {
        text_ui.label(
            egui::RichText::new(task.short_description())
                .small()
                .color(theme.text_secondary),
        );
    }

    // Click → activate the task. Auto-pick the user's last
    // explicit choice for this task if they have one (via
    // pick_default_model_for_task), otherwise the first capable
    // model in registry order.
    if has_model && response.clicked() {
        let capable = registry.models_for(task);
        state.active_task = Some(task);
        state.selected_model_id = pick_default_model_for_task(task, state, &capable);
    }

    // Tooltips. Disabled tile gets the "install from AI Models"
    // hint that points users at the dialog rather than guessing
    // why the button is greyed.
    if !has_model {
        response.on_hover_text(disabled_task_tooltip(task));
    } else if !active {
        response.on_hover_text(format!(
            "Activate {} (currently {} capable model{})",
            task.display_label(),
            registry.models_for(task).len(),
            if registry.models_for(task).len() == 1 { "" } else { "s" },
        ));
    }
}

/// Tooltip text for a disabled task tile — explains exactly which
/// models would unlock it and points at the AI Models dialog.
fn disabled_task_tooltip(task: AiTask) -> &'static str {
    match task {
        AiTask::TrackObject => {
            "Requires SAM 2.1 Large or RF-DETR-Seg with temporal support \
             — install from AI Models. Klipp's v1 lineup doesn't ship a \
             temporal-tracking model yet (tracker entry F-97-VIDEOTRACK)."
        }
        AiTask::RemoveBackground => {
            "Install BiRefNet Lite (or another matting model) from AI \
             Models to enable Remove Background."
        }
        AiTask::IsolateObject => {
            "Install MobileSAM (or another SAM-family model) from AI \
             Models to enable Isolate Object."
        }
        AiTask::DetectObjects => {
            "Install RF-DETR-Seg (or another detection model) from AI \
             Models to enable Detect Objects."
        }
    }
}

/// Decide which model id should pre-populate the dropdown when the
/// user (re)opens a task. Three-tier fallback:
///
/// 1. User's last explicit pick for this task (if still capable).
///    Wins always — once a user has chosen, that choice sticks
///    until they change it.
/// 2. The task's recommended model (if installed). Lets newcomers
///    pick a task and have the right tool selected without
///    knowing the lineup.
/// 3. First capable model in registry order. Last-resort fallback
///    when neither the user's history nor the recommendation
///    resolves.
pub(super) fn pick_default_model_for_task(
    task: AiTask,
    state: &AiToolsState,
    capable: &[klipp_ml::ModelInfo],
) -> Option<String> {
    // Tier 1: user's previous explicit pick.
    if let Some(restored) = state
        .last_picked_model_per_task
        .get(&task)
        .filter(|id| capable.iter().any(|m| m.id == **id))
    {
        return Some(restored.clone());
    }
    // Tier 2: task-recommended model, if it's installed.
    if let Some(rec_id) = recommended_model_for_task(task) {
        if capable.iter().any(|m| m.id == rec_id) {
            return Some(rec_id.to_string());
        }
    }
    // Tier 3: first capable.
    capable.first().map(|m| m.id.clone())
}

/// Best-for-task model id for newcomers. Picks reflect the
/// curated quality ranking inside each task's lineup; same
/// recommendations the AI Models dialog flags with the
/// RECOMMENDED badge.
///
/// Returns `None` when no model is recommended for the task
/// (e.g. Track Object — Klipp's v1 lineup has no temporal model;
/// the task is greyed out anyway).
fn recommended_model_for_task(task: AiTask) -> Option<&'static str> {
    match task {
        AiTask::RemoveBackground => Some("birefnet_lite_v1"),
        AiTask::IsolateObject => Some("mobile_sam_v1"),
        AiTask::DetectObjects => Some("rf_detr_seg_medium_v1"),
        AiTask::TrackObject => None,
    }
}

// ============================================================
// 2. MODEL — card with status dot + name + sub-line + change link
// ============================================================

fn model_card(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
    theme: &KlippTheme,
    show_models_dialog: &mut bool,
) {
    let info = state
        .selected_model_id
        .as_ref()
        .and_then(|id| registry.info(id));

    let running = matches!(state.inference, InferenceState::Running { .. });

    egui::Frame::none()
        .fill(theme.bg_raised.gamma_multiply(0.6))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Status indicator — spinning glyph during
                // inference, static dot otherwise. Hue follows
                // theme so cream/light themes stay legible.
                if running {
                    ui.add(
                        Icon::Spinner
                            .image(10.0)
                            .tint(theme.marker_warning),
                    );
                } else {
                    let dot_color = if info.is_some() {
                        theme.marker_success
                    } else {
                        theme.text_dim
                    };
                    paint_status_dot(ui, dot_color, 8.0);
                }

                ui.add_space(6.0);
                ui.vertical(|ui| {
                    if let Some(info) = info {
                        ui.label(
                            egui::RichText::new(&info.display_name)
                                .strong()
                                .size(13.0),
                        );
                        let subline = if running {
                            format!("Inferring on {}", inference_device_label())
                        } else {
                            model_subline(&info, state.active_task)
                        };
                        ui.label(
                            egui::RichText::new(subline)
                                .small()
                                .color(theme.text_secondary),
                        );
                    } else if state.active_task.is_some() {
                        ui.label(
                            egui::RichText::new("No model selected")
                                .strong()
                                .size(13.0),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Click Change… to install or pick one",
                            )
                            .small()
                            .color(theme.text_secondary),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("Pick a task above")
                                .strong()
                                .size(13.0)
                                .color(theme.text_dim),
                        );
                    }
                });

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Inline model picker. Lightweight popup
                        // listing every model capable of the
                        // active task, plus a "Manage models…"
                        // footer that opens the full plugin
                        // dialog. Replaces the previous "Change
                        // ›" link that always opened the heavier
                        // manager — picking a different variant
                        // is a much more frequent action than
                        // installing one.
                        model_picker_button(
                            ui,
                            state,
                            registry,
                            theme,
                            show_models_dialog,
                        );
                    },
                );
            });
        });
}

/// Caret-down button that opens an inline popup of every model
/// capable of the active task, with "Manage models…" as the
/// footer escape hatch into the plugin manager. Disabled when
/// there's no active task (no menu to populate from).
fn model_picker_button(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
    theme: &KlippTheme,
    show_models_dialog: &mut bool,
) {
    let Some(task) = state.active_task else {
        ui.add_enabled(
            false,
            egui::Button::new(
                egui::RichText::new("˅")
                    .small()
                    .color(theme.text_dim),
            )
            .frame(false),
        )
        .on_disabled_hover_text("Pick a task first to choose a model");
        return;
    };

    let response = ui
        .add(
            egui::Button::new(
                egui::RichText::new("˅ Pick model")
                    .small()
                    .color(theme.text_secondary),
            )
            .frame(false),
        )
        .on_hover_text(
            "Switch to another installed model for this task. \
             Use Manage… for install / uninstall / verify.",
        );

    let popup_id = ui.make_persistent_id("ai_model_picker_popup");
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
            let models = registry.models_for(task);
            if models.is_empty() {
                ui.label(
                    egui::RichText::new("No installed models for this task")
                        .small()
                        .color(theme.text_dim),
                );
            } else {
                for m in &models {
                    let is_selected =
                        state.selected_model_id.as_deref() == Some(m.id.as_str());
                    let label = if is_selected {
                        format!("✓  {}", m.display_name)
                    } else {
                        format!("    {}", m.display_name)
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(label).size(12.0),
                            )
                            .frame(false)
                            .min_size(egui::vec2(220.0, 22.0)),
                        )
                        .clicked()
                    {
                        state.selected_model_id = Some(m.id.clone());
                        // Remember the explicit pick under this
                        // task so the next switch back to it
                        // restores this selection (matches the
                        // behaviour of the dropdown that used to
                        // live in the panel header pre-redesign).
                        state
                            .last_picked_model_per_task
                            .insert(task, m.id.clone());
                        // Persist to ai_tools.toml so the choice
                        // survives the next launch.
                        crate::ai_tools::persistence::save(
                            &crate::ai_tools::persistence::AiToolsConfig::from_state(
                                state,
                            ),
                        );
                    }
                }
            }
            ui.separator();
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("⚙  Manage models…")
                            .size(12.0)
                            .color(theme.text_secondary),
                    )
                    .frame(false)
                    .min_size(egui::vec2(220.0, 22.0)),
                )
                .clicked()
            {
                *show_models_dialog = true;
            }
        },
    );
}

/// "Mask Generator · 213.6 MB · Ready" sub-line for the model
/// card. Size pulled from the manifest if we can resolve it;
/// otherwise just kind + Ready.
fn model_subline(info: &ModelInfo, _task: Option<AiTask>) -> String {
    // Resolve manifest size best-effort — we don't fail the
    // panel if the manifest read errors.
    let size_str = klipp_ml::manifest::load_effective()
        .ok()
        .and_then(|m| {
            m.models
                .iter()
                .find(|e| e.id == info.id)
                .map(|e| crate::panels::ai_models::format_bytes(e.size_bytes))
        })
        .unwrap_or_else(|| "—".into());

    let kind_label = capability_kind_label(info.capabilities);
    format!("{kind_label} · {size_str} · Ready")
}

/// Device label for the model card's "Inferring on {device}"
/// sub-line during the running state. Combines the cuda feature
/// flag with the runtime vendor probe so AMD / Intel users see
/// "GPU (AMD) — falling back to CPU" rather than a flat "CPU"
/// that hides the fact that they DO have a GPU but Klipp's not
/// using it yet.
#[cfg(not(feature = "ai-ort"))]
fn inference_device_label() -> String {
    "CPU".into()
}

#[cfg(feature = "ai-ort")]
fn inference_device_label() -> String {
    use klipp_ml_ort::{GpuVendor, RuntimeStatus};
    let cuda_feature_on =
        cfg!(feature = "ai-ort-cuda") || cfg!(feature = "ai-candle-cuda");
    match klipp_ml_ort::resolve_runtime_status() {
        RuntimeStatus::GpuAccelerationActive(GpuVendor::Nvidia) if cuda_feature_on => {
            "GPU (NVIDIA)".into()
        }
        RuntimeStatus::GpuAccelerationActive(GpuVendor::Nvidia) => {
            "CPU (NVIDIA detected, GPU support not built)".into()
        }
        RuntimeStatus::GpuAvailableNotInstalled(GpuVendor::Nvidia) => {
            "CPU (NVIDIA detected, GPU runtime missing)".into()
        }
        RuntimeStatus::GpuVendorUnsupported(v) => {
            format!("CPU ({} GPU not yet supported)", v.display_label())
        }
        RuntimeStatus::GpuRuntimeCpuOnly(v) => {
            format!("CPU ({} detected, onnxruntime is a CPU-only build)", v.display_label())
        }
        _ => "CPU".into(),
    }
}

/// Inline label for a model's coarse kind, derived from the
/// capability bits — no separate source-of-truth needed in the
/// panel (the registry's `MaskCapabilities` IS the truth).
fn capability_kind_label(caps: klipp_ml::MaskCapabilities) -> &'static str {
    // At this coarseness there are two kinds: something that tracks
    // through time, and everything else. The prompt, auto-detect and
    // fallback arms all returned "Mask Generator" separately, which read
    // as though the distinction mattered here.
    if caps.contains(klipp_ml::MaskCapabilities::TEMPORAL) {
        "Video Tracker"
    } else {
        "Mask Generator"
    }
}

/// Small filled circle used as a status indicator. Mirrors the
/// AI Models dialog's row dot for visual consistency across the
/// surfaces.
fn paint_status_dot(ui: &mut egui::Ui, color: egui::Color32, diameter: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(diameter, diameter),
        egui::Sense::hover(),
    );
    ui.painter().circle_filled(rect.center(), diameter * 0.5, color);
}

// ============================================================
// 3. PROMPTS — tab bar + counter chips + tags + help text
// ============================================================

fn prompts_section(ui: &mut egui::Ui, state: &mut AiToolsState, theme: &KlippTheme) {
    let active_task = state.active_task;

    // Tab bar — three equal-width segments, selected gets accent.
    ui.horizontal(|ui| {
        let avail = ui.available_width();
        let tab_w = (avail - 8.0) / 3.0;
        prompt_tab(ui, state, PromptMode::PointPrompt, "● Point", tab_w, theme);
        prompt_tab(ui, state, PromptMode::BoxPrompt, "▢ Box", tab_w, theme);
        prompt_tab(ui, state, PromptMode::Scribble, "～ Scribble", tab_w, theme);
    });

    ui.add_space(6.0);

    // Counter chips: "0 points  0 boxes  0 strokes  Clear"
    ui.horizontal(|ui| {
        let prompts = &state.prompts;
        counter_chip(
            ui,
            theme,
            "●",
            prompts.clicks.len(),
            "point",
            state.prompts.mode == PromptMode::PointPrompt,
        );
        counter_chip(
            ui,
            theme,
            "▢",
            prompts.boxes.len(),
            "box",
            state.prompts.mode == PromptMode::BoxPrompt,
        );
        counter_chip(
            ui,
            theme,
            "～",
            prompts.finalized_scribbles.len(),
            "stroke",
            state.prompts.mode == PromptMode::Scribble,
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Clear")
                                .small()
                                .color(theme.text_dim),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    state.prompts.clear();
                }
            },
        );
    });

    // Keep / exclude breakdown — only renders when the user has
    // captured at least one positive or negative click. Lets
    // SAM-style click flows show "+ N keep / - N exclude" as a
    // running tally without forcing a Generate to find out.
    let keep_count = state.prompts.clicks.iter().filter(|c| c.positive).count();
    let exclude_count = state.prompts.clicks.iter().filter(|c| !c.positive).count();
    if keep_count > 0 || exclude_count > 0 {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if keep_count > 0 {
                tag_chip(
                    ui,
                    theme.marker_success,
                    &format!("+ {keep_count} keep"),
                );
            }
            if exclude_count > 0 {
                tag_chip(
                    ui,
                    theme.marker_warning,
                    &format!("- {exclude_count} exclude"),
                );
            }
        });
    }

    ui.add_space(4.0);

    // Help text below the chips. Wording shifts with the active
    // task so the user sees task-relevant guidance (e.g.
    // RemoveBackground tells them the task auto-detects).
    let help = prompt_help_text(active_task, state.prompts.mode);
    ui.horizontal_wrapped(|ui| {
        ui.add(Icon::Info.image(11.0).tint(theme.text_dim));
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(help)
                .small()
                .color(theme.text_secondary),
        );
    });
}

fn prompt_tab(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    mode: PromptMode,
    label: &str,
    width: f32,
    theme: &KlippTheme,
) {
    let active = state.prompts.mode == mode;
    let color = if active {
        theme.marker_warning
    } else {
        theme.text_secondary
    };
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 28.0),
        egui::Sense::click(),
    );
    if active {
        ui.painter().rect_filled(
            rect,
            4.0,
            theme.bg_raised,
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            4.0,
            theme.bg_raised.gamma_multiply(0.5),
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        color,
    );
    if resp.clicked() {
        state.prompts.set_mode(mode);
    }
}

/// Pill-shaped tag with a translucent fill in `accent` and a
/// matching outline. Used for the "+ N keep" / "- N exclude"
/// breakdown under the prompt counter chips. Same visual
/// language we use elsewhere for accent badges.
fn tag_chip(ui: &mut egui::Ui, accent: egui::Color32, label: &str) {
    egui::Frame::none()
        .fill(accent.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, accent))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 1.5))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .small()
                    .strong()
                    .color(accent),
            );
        });
}

fn counter_chip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    glyph: &str,
    n: usize,
    word: &str,
    active: bool,
) {
    let plural = if n == 1 { "" } else { "s" };
    let text = format!("{glyph} {n} {word}{plural}");
    let color = if active {
        theme.marker_warning
    } else {
        theme.text_secondary
    };
    egui::Frame::none()
        .fill(theme.bg_raised.gamma_multiply(0.5))
        .rounding(2.0)
        .inner_margin(egui::Margin::symmetric(6.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .color(color),
            );
        });
}

fn prompt_help_text(task: Option<AiTask>, mode: PromptMode) -> &'static str {
    match (task, mode) {
        (Some(AiTask::RemoveBackground), _) => {
            "Remove BG runs auto-detect — no clicks needed. Click Generate \
             Mask to extract the foreground."
        }
        (Some(AiTask::DetectObjects), _) => {
            "Detect Objects runs auto-detect — no clicks needed. Click \
             Generate Mask to find every object in frame."
        }
        (Some(AiTask::TrackObject), _) => {
            "Track Subject follows the person through the selected clip and \
             gates its grade with them. It runs in the background; its progress \
             shows on the matte's card and in the status bar."
        }
        (_, PromptMode::PointPrompt) => {
            "Click in the viewport to add a positive point. Hold Alt for \
             negative (excluded) regions."
        }
        (_, PromptMode::BoxPrompt) => {
            "Drag in the viewport to draw a bounding box around the subject."
        }
        (_, PromptMode::Scribble) => {
            "Drag in the viewport to scribble over the subject. Hold Alt \
             for excluded regions."
        }
    }
}

// ============================================================
// 4. RUN — Generate Mask button + scope chips
// ============================================================

fn run_section(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
    theme: &KlippTheme,
) {
    let running = matches!(state.inference, InferenceState::Running { .. });
    if running {
        running_progress_section(ui, state, theme);
        return;
    }
    // Track Subject is one action on the selected clip (#239).
    if state.active_task == Some(AiTask::TrackObject) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("Track Subject").on_hover_text("Track the subject through the selected clip, in the background, to grade it").clicked() {
                state.pending_track_subject = true;
                state.pending_cut_out = false;
            }
            // Or cut the clip out round it, over the tracks below (#73).
            if ui.button("Cut Out Subject").on_hover_text("Track the subject and cut the clip out round it: the tracks below show everywhere else").clicked() {
                state.pending_track_subject = true;
                state.pending_cut_out = true;
            }
        });
        let _ = (registry, theme);
        return;
    }

    let can_run = state.active_task.is_some()
        && state
            .selected_model_id
            .as_ref()
            .map(|id| registry.has_id(id))
            .unwrap_or(false);

    ui.horizontal(|ui| {
        // Same anti-feedback safety margin as the task grid: leave
        // headroom in the button width so the panel resizer
        // doesn't read "content wants exactly avail+epsilon" and
        // grow on the next frame.
        let avail = ui.available_width();
        let button_w = (avail - 36.0 - 12.0).max(120.0);
        let resp = ui.add_sized(
            [button_w, 36.0],
            egui::Button::new(
                egui::RichText::new("▶ Generate Mask")
                    .strong()
                    .size(13.0),
            )
            .fill(if can_run {
                theme.marker_warning
            } else {
                theme.bg_raised
            }),
        );
        if can_run && resp.clicked() {
            state.pending_run = true;
        }
        if !can_run {
            resp.on_disabled_hover_text(disabled_run_reason(state, registry));
        }
        // Trailing ▾ — placeholder for future "run-with-presets"
        // overflow. Disabled visually for v1.
        ui.add_enabled(
            false,
            egui::Button::new("▾").min_size(egui::vec2(32.0, 36.0)),
        )
        .on_disabled_hover_text("Per-run presets — coming soon");
    });

    ui.add_space(6.0);

    // SCOPE chips. CurrentFrame is wired; Selection / FullClip
    // are visible-but-disabled with "coming soon" tooltips so the
    // batch-dispatch story is forward-compatible in the UI.
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("SCOPE:")
                .small()
                .strong()
                .color(theme.text_dim),
        );
        for sc in &[AiScope::CurrentFrame, AiScope::Selection, AiScope::FullClip] {
            scope_chip(ui, *sc, state, theme);
        }
    });
}

/// Render the progress bar + elapsed/ETA + Cancel link that
/// replaces the Generate Mask button while inference is running.
///
/// Percentage is FAKE for v1: we don't have streaming progress
/// from the generator yet, so we interpolate elapsed/avg_ms
/// from telemetry and cap at 95% until Done lands. Real streaming
/// can swap the percentage source without changing the UI.
fn running_progress_section(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    theme: &KlippTheme,
) {
    let model_id = match &state.inference {
        InferenceState::Running { model_id, .. } => model_id.clone(),
        _ => return,
    };

    // Estimate the run's expected duration from the model's
    // rolling telemetry average. Falls back to 1500ms when we
    // have no history (a generous "feels like ~a couple seconds"
    // ballpark). Fake-progress capped at 95% so the bar can't
    // sit at 100% before the actual Done event lands.
    let avg_ms = state
        .model_telemetry
        .get(&model_id)
        .and_then(|t| t.avg_inference_ms())
        .unwrap_or(1500.0);
    let elapsed_ms = state
        .inference_start_time
        .map(|t| t.elapsed().as_millis() as f64)
        .unwrap_or(0.0);
    let pct = (elapsed_ms / avg_ms.max(1.0)).clamp(0.0, 0.95);
    let pct_label = format!("{:.0}%", pct * 100.0);

    // Big progress bar — matches the Generate Mask button's
    // 36px height so the section's vertical rhythm doesn't jump.
    let avail = ui.available_width();
    let bar_w = (avail - 12.0).max(120.0);
    let bar_h = 36.0;
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.marker_warning))
        .rounding(4.0)
        .inner_margin(egui::Margin::same(0.0))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(bar_w, bar_h),
                egui::Sense::hover(),
            );
            // Filled portion of the bar.
            let mut fill_rect = rect;
            fill_rect.set_width(rect.width() * pct as f32);
            ui.painter()
                .rect_filled(fill_rect, 4.0, theme.marker_warning.gamma_multiply(0.5));
            // Label centered on the bar; percentage right-aligned.
            ui.painter().text(
                rect.left_center() + egui::vec2(12.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "⟳ Generating mask…",
                egui::FontId::proportional(13.0),
                theme.text_primary,
            );
            ui.painter().text(
                rect.right_center() - egui::vec2(12.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                &pct_label,
                egui::FontId::proportional(13.0),
                theme.text_primary,
            );
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let remaining_ms = (avg_ms - elapsed_ms).max(0.0);
        let remaining_text = if remaining_ms < 1000.0 {
            "~1s remaining".to_string()
        } else {
            format!("~{:.0}s remaining", remaining_ms / 1000.0)
        };
        ui.label(
            egui::RichText::new(format!("Frame 1 of 1 · {remaining_text}"))
                .small()
                .color(theme.text_secondary),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Cancel")
                                .small()
                                .strong()
                                .color(theme.marker_warning),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    state.pending_cancel = true;
                }
            },
        );
    });
}

fn scope_chip(
    ui: &mut egui::Ui,
    sc: AiScope,
    state: &mut AiToolsState,
    theme: &KlippTheme,
) {
    let active = state.scope == sc;
    let enabled = sc.is_implemented();
    let label = sc.label();
    let resp = ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(label).color(if active {
                theme.marker_warning
            } else if enabled {
                theme.text_secondary
            } else {
                theme.text_dim
            }),
        )
        .stroke(if active {
            egui::Stroke::new(1.0_f32, theme.marker_warning)
        } else {
            egui::Stroke::NONE
        })
        .frame(active),
    );
    if !enabled {
        resp.on_disabled_hover_text(
            "Batch processing — coming soon. Today only Current frame \
             dispatches inference.",
        );
    } else if resp.clicked() {
        state.scope = sc;
    }
}

fn disabled_run_reason(state: &AiToolsState, registry: &AiToolsRegistry) -> &'static str {
    if state.active_task.is_none() {
        "Pick a task first"
    } else if state
        .selected_model_id
        .as_ref()
        .map(|id| registry.has_id(id))
        .unwrap_or(false)
    {
        "Inference already running"
    } else {
        "Pick a model first"
    }
}

// ============================================================
// OUTPUT — empty state for v1; running / complete in commits 2-3
// ============================================================

fn output_section(ui: &mut egui::Ui, state: &mut AiToolsState, theme: &KlippTheme) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("OUTPUT")
                .small()
                .strong()
                .color(theme.text_secondary),
        );
        // "Live preview" pill on the right edge while running,
        // matching the screenshot.
        if matches!(state.inference, InferenceState::Running { .. }) {
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new("Live preview")
                            .small()
                            .color(theme.text_dim),
                    );
                },
            );
        }
    });
    ui.add_space(4.0);

    match &state.inference {
        InferenceState::Running { .. } => {
            running_output_overlay(ui, state, theme);
            ui.add_space(6.0);
            display_mode_tabs(ui, state, theme, /*has_mask=*/ false);
        }
        InferenceState::Done { response, .. } if !response.candidates.is_empty() => {
            done_preview(ui, state, theme);
        }
        _ => {
            empty_output_placeholder(ui, theme);
        }
    }

    // Recent history — appears below the OUTPUT block once at
    // least one inference has resolved. Hidden in the empty
    // state to keep the idle panel clean.
    if !state.recent_inferences.is_empty() {
        ui.add_space(10.0);
        ui.separator();
        recent_list(ui, state, theme);
    }
}

/// Done-state preview: success banner, mask thumbnail with the
/// selected display-mode tint, Re-run + Apply-to-clip buttons.
fn done_preview(ui: &mut egui::Ui, state: &mut AiToolsState, theme: &KlippTheme) {
    // Pull the response we need without holding the borrow into
    // any mutating call below.
    let (frame_label, duration_ms, mask) = match &state.inference {
        InferenceState::Done { response, .. } if !response.candidates.is_empty() => {
            let m = response.candidates[0].mask.clone();
            (
                "frame".to_string(),
                response.inference_ms,
                m,
            )
        }
        _ => return,
    };

    // Success banner.
    egui::Frame::none()
        .fill(theme.marker_success.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, theme.marker_success))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Icon::Check.image(12.0).tint(theme.marker_success));
                ui.label(
                    egui::RichText::new(format!("Mask generated on {frame_label}"))
                        .strong()
                        .small()
                        .color(theme.marker_success),
                );
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.label(
                            egui::RichText::new(format!("{:.2}s", duration_ms as f64 / 1000.0))
                                .small()
                                .color(theme.marker_success),
                        );
                    },
                );
            });
        });

    ui.add_space(6.0);

    // Mask preview rect. Display mode controls how the alpha is
    // tinted — Overlay = warning hue, MaskOnly = white grayscale,
    // Cutout = white over a checker bg. The preview shows the
    // mask itself, not the source frame ⊗ mask (proper Cutout
    // with source pixels needs the panel to hold the source
    // frame across frames; tracked as a follow-up).
    let avail = ui.available_width();
    let h = (avail * 9.0 / 16.0).clamp(120.0, 220.0);
    let preview_w = (avail - 4.0).max(120.0);
    egui::Frame::none()
        .rounding(4.0)
        .inner_margin(egui::Margin::same(0.0))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(preview_w, h),
                egui::Sense::hover(),
            );
            let painter = ui.painter_at(rect);
            // Background — checker for Cutout, dim raised for the
            // others. Same checker pattern most NLE alpha previews
            // use.
            if state.display_mode == DisplayMode::Cutout {
                paint_checker_bg(&painter, rect, theme);
            } else {
                painter.rect_filled(
                    rect,
                    6.0,
                    theme.bg_raised.gamma_multiply(0.4),
                );
            }
            // Mask alpha rendered at the rect's aspect. Tile
            // sampling — we don't have a real upload-to-texture
            // path here for the panel preview, so we draw the
            // mask as a coarse heatmap of rectangles. Good enough
            // for the v1 preview; the real "live full-res mask"
            // belongs to the streaming work.
            paint_mask_heatmap(
                &painter,
                rect,
                &mask,
                state.display_mode,
                theme,
            );
            // Watermark when there are no actual mask pixels (a
            // misbehaving generator returning empty bytes).
            if mask.alpha.iter().all(|&a| a == 0) {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Empty mask",
                    egui::FontId::proportional(13.0),
                    theme.text_dim,
                );
            }
        });

    ui.add_space(6.0);
    display_mode_tabs(ui, state, theme, /*has_mask=*/ true);

    ui.add_space(8.0);

    // Action row — Re-run, Apply to clip (the grade), Cut out (#73).
    ui.horizontal(|ui| {
        let avail = ui.available_width();
        let half = (avail / 3.0 - 6.0).max(70.0);
        if ui
            .add_sized(
                [half, 32.0],
                egui::Button::new(
                    egui::RichText::new("↻ Re-run").size(13.0),
                ),
            )
            .clicked()
        {
            state.pending_rerun = true;
        }
        if ui
            .add_sized(
                [half, 32.0],
                egui::Button::new(
                    egui::RichText::new("✓ Apply to clip")
                        .strong()
                        .size(13.0),
                )
                .fill(theme.marker_warning),
            )
            .on_hover_text("Grade inside the mask")
            .clicked()
        {
            state.pending_apply_to_clip = true;
            state.pending_cut_out = false;
        }
        if ui
            .add_sized([half, 32.0], egui::Button::new(egui::RichText::new("✂ Cut out").size(13.0)))
            .on_hover_text("Cut the clip out by the mask: the tracks below show outside it")
            .clicked()
        {
            state.pending_apply_to_clip = true;
            state.pending_cut_out = true;
        }
    });
}

/// Paint a 2-tone alpha-checker background — the universal "this
/// pixel is transparent" indicator. Tile is 8×8.
fn paint_checker_bg(
    painter: &egui::Painter,
    rect: egui::Rect,
    theme: &KlippTheme,
) {
    let tile = 8.0;
    let bg_a = theme.bg_raised.gamma_multiply(0.4);
    let bg_b = theme.bg_raised.gamma_multiply(0.6);
    let mut y = rect.min.y;
    let mut row = 0u32;
    while y < rect.max.y {
        let mut x = rect.min.x;
        let mut col = 0u32;
        while x < rect.max.x {
            let c = if (row + col).is_multiple_of(2) { bg_a } else { bg_b };
            let cell = egui::Rect::from_min_max(
                egui::pos2(x, y),
                egui::pos2((x + tile).min(rect.max.x), (y + tile).min(rect.max.y)),
            );
            painter.rect_filled(cell, 0.0, c);
            x += tile;
            col += 1;
        }
        y += tile;
        row += 1;
    }
}

/// Coarse alpha visualization — samples the mask buffer at a grid
/// of points and paints rectangles tinted by the alpha value.
/// Not a pixel-perfect render (the real one needs an egui
/// TextureHandle); the heatmap gives the user the gist of the
/// mask shape without the texture-upload plumbing in the v1.
fn paint_mask_heatmap(
    painter: &egui::Painter,
    rect: egui::Rect,
    mask: &klipp_ml::MaskBuffer,
    mode: DisplayMode,
    theme: &KlippTheme,
) {
    if mask.width == 0 || mask.height == 0 {
        return;
    }
    let cols: u32 = 64;
    let rows: u32 = (cols as f32 * rect.height() / rect.width()).max(8.0) as u32;
    let cell_w = rect.width() / cols as f32;
    let cell_h = rect.height() / rows as f32;
    let tint = match mode {
        DisplayMode::Overlay => theme.marker_warning,
        DisplayMode::MaskOnly => egui::Color32::WHITE,
        DisplayMode::Cutout => egui::Color32::WHITE,
    };
    for r in 0..rows {
        for c in 0..cols {
            let mx = ((c as f32 + 0.5) / cols as f32 * mask.width as f32) as u32;
            let my = ((r as f32 + 0.5) / rows as f32 * mask.height as f32) as u32;
            let mx = mx.min(mask.width.saturating_sub(1));
            let my = my.min(mask.height.saturating_sub(1));
            let alpha = mask.at(mx, my);
            if alpha < 8 {
                continue;
            }
            let cell = egui::Rect::from_min_size(
                egui::pos2(
                    rect.min.x + c as f32 * cell_w,
                    rect.min.y + r as f32 * cell_h,
                ),
                egui::vec2(cell_w + 0.5, cell_h + 0.5),
            );
            let color = egui::Color32::from_rgba_unmultiplied(
                tint.r(),
                tint.g(),
                tint.b(),
                alpha,
            );
            painter.rect_filled(cell, 0.0, color);
        }
    }
}

/// Recent inferences list — three thumbnails with task / model
/// captions and a relative-time + duration sub-line. Shown only
/// when the deque has at least one entry.
fn recent_list(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    theme: &KlippTheme,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("RECENT")
                .small()
                .strong()
                .color(theme.text_secondary),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Clear")
                                .small()
                                .color(theme.marker_warning),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    state.recent_inferences.clear();
                }
            },
        );
    });
    ui.add_space(4.0);

    // Render up to the last 3 entries (most-recent first). The
    // VecDeque is push-front in app.rs, so the front is freshest.
    for entry in state.recent_inferences.iter().take(3) {
        recent_row(ui, entry, theme);
    }
}

fn recent_row(
    ui: &mut egui::Ui,
    entry: &super::RecentEntry,
    theme: &KlippTheme,
) {
    egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(0.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Thumbnail or coloured placeholder block.
                let thumb_size = egui::vec2(54.0, 36.0);
                if let Some(thumb) = &entry.thumbnail {
                    let img = egui::Image::new(thumb)
                        .fit_to_exact_size(thumb_size)
                        .rounding(2.0);
                    ui.add(img);
                } else {
                    let (rect, _) = ui.allocate_exact_size(
                        thumb_size,
                        egui::Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(rect, 2.0, theme.bg_raised);
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    let model_short = entry.model_display_name.clone();
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {model_short}",
                            entry.task.display_label(),
                        ))
                        .strong()
                        .small(),
                    );
                    let mut subline = format!(
                        "{} · {:.2}s",
                        super::format_relative(entry.timestamp),
                        entry.duration_ms as f64 / 1000.0,
                    );
                    if entry.candidates_count > 1 {
                        subline.push_str(&format!(
                            " · {} found",
                            entry.candidates_count,
                        ));
                    }
                    ui.label(
                        egui::RichText::new(subline)
                            .small()
                            .color(theme.text_dim),
                    );
                });
                // Replay glyph at the right edge — currently a
                // visual cue only; full re-fire belongs to a
                // follow-up that wires through the dispatcher.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Button::new(
                                egui::RichText::new("↻")
                                    .size(13.0)
                                    .color(theme.text_dim),
                            )
                            .frame(false),
                        )
                        .on_hover_text(
                            "Replay — coming soon. Captured prompts are stored on \
                             the entry; the wiring to re-fire the inference lands \
                             in a follow-up commit.",
                        );
                    },
                );
            });
        });
}

/// Static visualization of the user's captured prompts during
/// inference. We don't have streaming preview so the body of the
/// "preview" is drawn entirely from prompt data — point markers,
/// box outlines, scribble polylines — overlaid on a placeholder
/// frame rect. Once streaming partial masks lands, this renderer
/// keeps drawing the prompts and a streamed mask layer slots
/// underneath them.
fn running_output_overlay(
    ui: &mut egui::Ui,
    state: &AiToolsState,
    theme: &KlippTheme,
) {
    let avail = ui.available_width();
    let h = (avail * 9.0 / 16.0).clamp(120.0, 220.0);
    egui::Frame::none()
        .fill(theme.bg_raised.gamma_multiply(0.4))
        .rounding(4.0)
        .inner_margin(egui::Margin::same(0.0))
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(avail - 4.0, h),
                egui::Sense::hover(),
            );
            let painter = ui.painter_at(rect);
            // Draw boxes (dashed warning outline).
            for b in &state.prompts.boxes {
                let r = egui::Rect::from_min_max(
                    rect.min
                        + egui::vec2(b.x1 * rect.width(), b.y1 * rect.height()),
                    rect.min
                        + egui::vec2(b.x2 * rect.width(), b.y2 * rect.height()),
                );
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5_f32, theme.marker_warning));
            }
            // Draw point markers — green for keep, warning-red
            // for exclude. Cosmetically matches the keep/exclude
            // tag chips above the run section.
            for p in &state.prompts.clicks {
                let pos = rect.min
                    + egui::vec2(p.x * rect.width(), p.y * rect.height());
                let color = if p.positive {
                    theme.marker_success
                } else {
                    theme.marker_error
                };
                painter.circle_filled(pos, 6.0, color.gamma_multiply(0.85));
                painter.circle_stroke(
                    pos,
                    6.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
                );
            }
            // Faint hint text when there are no prompts (e.g.
            // auto-detect tasks). Centred so the area doesn't
            // look broken.
            if state.prompts.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Auto-detecting…",
                    egui::FontId::proportional(13.0),
                    theme.text_dim,
                );
            }
        });
}

/// Three-tab bar under the OUTPUT preview: Overlay / Mask only /
/// Cutout. When `has_mask` is false (running state, or before
/// the user has run anything), Mask only and Cutout are disabled
/// with "Mask not yet generated" tooltips. Forward-compatible
/// with the streaming-live-preview work — same UI, different
/// mask source.
fn display_mode_tabs(
    ui: &mut egui::Ui,
    state: &mut AiToolsState,
    theme: &KlippTheme,
    has_mask: bool,
) {
    ui.horizontal(|ui| {
        let avail = ui.available_width();
        let tab_w = (avail - 8.0) / 3.0;
        for mode in DisplayMode::ALL {
            display_mode_tab(ui, *mode, tab_w, state, theme, has_mask);
        }
    });
}

fn display_mode_tab(
    ui: &mut egui::Ui,
    mode: DisplayMode,
    width: f32,
    state: &mut AiToolsState,
    theme: &KlippTheme,
    has_mask: bool,
) {
    // Overlay is always available (even with no mask, "overlay"
    // means "prompts on the source frame" which IS our running
    // visualization). MaskOnly / Cutout require a finished mask.
    let enabled = has_mask || matches!(mode, DisplayMode::Overlay);
    let active = state.display_mode == mode;
    let color = if !enabled {
        theme.text_dim
    } else if active {
        theme.marker_warning
    } else {
        theme.text_secondary
    };
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 26.0),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if active {
        ui.painter().rect_filled(rect, 2.0, theme.bg_raised);
    } else if enabled && resp.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, theme.bg_raised.gamma_multiply(0.5));
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        mode.label(),
        egui::FontId::proportional(11.0),
        color,
    );
    if enabled && resp.clicked() {
        state.display_mode = mode;
    }
    if !enabled {
        resp.on_hover_text("Mask not yet generated — run inference first");
    }
}

fn empty_output_placeholder(ui: &mut egui::Ui, theme: &KlippTheme) {
    // The Frame fills its parent's available width naturally —
    // we deliberately do NOT set_min_width here. Reading
    // ui.available_width() and re-asserting it as a min creates a
    // 1-pixel-per-frame growth loop with the SidePanel's resize
    // logic.
    egui::Frame::none()
        .fill(theme.bg_raised.gamma_multiply(0.6))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 36.0))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add(Icon::Info.image(28.0).tint(theme.text_dim));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Add a prompt and run to preview the mask here.",
                    )
                    .small()
                    .color(theme.text_secondary),
                );
            });
        });
}

// ============================================================
// ADVANCED — collapsible drawer with mask/feather/output controls
// ============================================================

fn advanced_section(ui: &mut egui::Ui, state: &mut AiToolsState, theme: &KlippTheme) {
    let count = 3; // mask threshold + edge feather + output format
    ui.horizontal(|ui| {
        let arrow = if state.advanced_open { "▾" } else { "▸" };
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(format!("{arrow}  ADVANCED"))
                        .small()
                        .strong()
                        .color(theme.text_secondary),
                )
                .frame(false),
            )
            .clicked()
        {
            state.advanced_open = !state.advanced_open;
        }
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(format!("{count} settings"))
                        .small()
                        .color(theme.text_dim),
                );
            },
        );
    });

    if !state.advanced_open {
        return;
    }
    ui.add_space(6.0);
    egui::Frame::none()
        .fill(theme.bg_raised.gamma_multiply(0.4))
        .rounding(4.0)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            // Mask threshold — slider 0.0..=1.0 with the value
            // pinned right-aligned. Reuses MaskTunables's
            // score_threshold for inference dispatch (covered
            // separately in commit 2 wiring).
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mask threshold").small());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{:.2}",
                                state.advanced_settings.mask_threshold,
                            ))
                            .small()
                            .color(theme.text_secondary),
                        );
                    },
                );
            });
            ui.add(
                egui::Slider::new(
                    &mut state.advanced_settings.mask_threshold,
                    0.0..=1.0,
                )
                .show_value(false),
            );
            ui.add_space(6.0);

            // Edge feather — slider 0..=20 px.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Edge feather").small());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} px",
                                state.advanced_settings.edge_feather,
                            ))
                            .small()
                            .color(theme.text_secondary),
                        );
                    },
                );
            });
            ui.add(
                egui::Slider::new(
                    &mut state.advanced_settings.edge_feather,
                    0..=20,
                )
                .show_value(false),
            );
            ui.add_space(6.0);

            // Output format — dropdown. Width is FIXED rather
            // than `ui.available_width()` because the latter
            // creates a width-growth loop with the SidePanel
            // resizer (combo asks for available, available grows,
            // combo asks for more, …). 200px fits the longest
            // label cleanly.
            ui.label(egui::RichText::new("Output").small());
            egui::ComboBox::from_id_salt("ai_tools_output_format")
                .selected_text(state.advanced_settings.output_format.label())
                .width(200.0)
                .show_ui(ui, |ui| {
                    for f in OutputFormat::ALL {
                        let is_selected = state.advanced_settings.output_format == *f;
                        if ui.selectable_label(is_selected, f.label()).clicked() {
                            state.advanced_settings.output_format = *f;
                        }
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::{MaskCapabilities, ModelInfo};

    fn fake_capable() -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "rf_detr_seg_v1".into(),
                display_name: "RF-DETR-Seg (Preview)".into(),
                capabilities: MaskCapabilities::AUTO_DETECT,
            },
            ModelInfo {
                id: "rf_detr_seg_medium_v1".into(),
                display_name: "RF-DETR-Seg (Medium)".into(),
                capabilities: MaskCapabilities::AUTO_DETECT,
            },
        ]
    }

    #[test]
    fn pick_default_picks_task_recommended_when_no_history() {
        // No remembered pick → tier-2 recommendation for
        // DetectObjects is rf_detr_seg_medium_v1, which IS in
        // capable, so it wins over the first-capable fallback.
        let state = AiToolsState::default();
        let capable = fake_capable();
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
        assert_eq!(pick.as_deref(), Some("rf_detr_seg_medium_v1"));
    }

    #[test]
    fn pick_default_user_pick_beats_task_recommendation() {
        // User explicitly picked the Preview tier — that beats
        // the task's recommended Medium.
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_v1".into(),
        );
        let capable = fake_capable();
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
        assert_eq!(pick.as_deref(), Some("rf_detr_seg_v1"));
    }

    #[test]
    fn pick_default_restores_remembered_choice() {
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_medium_v1".into(),
        );
        let capable = fake_capable();
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
        assert_eq!(pick.as_deref(), Some("rf_detr_seg_medium_v1"));
    }

    #[test]
    fn pick_default_drops_stale_remembered_choice_to_recommendation() {
        // Stale remembered id → falls through to the task-
        // recommended model since it's installed.
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_xlarge_v9_doesnt_exist".into(),
        );
        let capable = fake_capable();
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
        assert_eq!(pick.as_deref(), Some("rf_detr_seg_medium_v1"));
    }

    #[test]
    fn pick_default_falls_through_to_first_when_recommendation_uninstalled() {
        // RemoveBackground recommends BiRefNet, which isn't in
        // fake_capable (RF-DETR variants only). With no user
        // history either, the first capable wins.
        let state = AiToolsState::default();
        let capable = fake_capable();
        let pick =
            pick_default_model_for_task(AiTask::RemoveBackground, &state, &capable);
        assert_eq!(pick.as_deref(), Some("rf_detr_seg_v1"));
    }

    #[test]
    fn pick_default_returns_none_when_no_models_capable() {
        let state = AiToolsState::default();
        let capable: Vec<ModelInfo> = vec![];
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
        assert!(pick.is_none());
    }

    #[test]
    fn per_task_history_is_independent() {
        // RemoveBackground task — DetectObjects' history
        // shouldn't leak in. Recommended for RemoveBackground is
        // BiRefNet (uninstalled in fake_capable), so we expect
        // the first-capable fallback for that task.
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_medium_v1".into(),
        );
        let capable = fake_capable();
        let pick_other =
            pick_default_model_for_task(AiTask::RemoveBackground, &state, &capable);
        assert_eq!(
            pick_other.as_deref(),
            Some("rf_detr_seg_v1"),
            "RemoveBackground should fall back to first capable, not pick up DetectObjects' history",
        );
    }

    #[test]
    fn recommended_model_for_task_covers_every_running_task() {
        // Every task except TrackObject (which has no temporal
        // model in our v1 lineup) should have a recommendation
        // — pinning here so the lineup curation stays in sync
        // with what we actually ship.
        assert_eq!(
            recommended_model_for_task(AiTask::RemoveBackground),
            Some("birefnet_lite_v1"),
        );
        assert_eq!(
            recommended_model_for_task(AiTask::IsolateObject),
            Some("mobile_sam_v1"),
        );
        assert_eq!(
            recommended_model_for_task(AiTask::DetectObjects),
            Some("rf_detr_seg_medium_v1"),
        );
        assert_eq!(recommended_model_for_task(AiTask::TrackObject), None);
    }

    #[test]
    fn ai_scope_default_is_current_frame() {
        let state = AiToolsState::default();
        assert_eq!(state.scope, AiScope::CurrentFrame);
    }

    #[test]
    fn ai_scope_only_current_frame_is_implemented() {
        assert!(AiScope::CurrentFrame.is_implemented());
        assert!(!AiScope::Selection.is_implemented());
        assert!(!AiScope::FullClip.is_implemented());
    }

    #[test]
    fn advanced_settings_defaults() {
        let s = crate::ai_tools::AdvancedSettings::default();
        assert_eq!(s.mask_threshold, 0.5);
        assert_eq!(s.edge_feather, 2);
        assert_eq!(s.output_format, OutputFormat::AlphaMatte16);
    }
}
