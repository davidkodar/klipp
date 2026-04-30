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
                section_step_header_with_action(
                    ui,
                    2,
                    "MODEL",
                    model_active,
                    "Change…",
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
    let has_model = registry.has_model_for(task);
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
            egui::Stroke::new(1.5, theme.marker_warning),
        )
    } else if has_model && response.hovered() {
        (theme.bg_raised, egui::Stroke::new(1.0, theme.text_dim.gamma_multiply(0.4)))
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
/// user (re)opens a task. Same logic as the previous panel — kept
/// here unchanged from the F-97-MODEL-MANAGER work.
pub(super) fn pick_default_model_for_task(
    task: AiTask,
    state: &AiToolsState,
    capable: &[klipp_ml::ModelInfo],
) -> Option<String> {
    let restored = state
        .last_picked_model_per_task
        .get(&task)
        .filter(|id| capable.iter().any(|m| m.id == **id))
        .cloned();
    restored.or_else(|| capable.first().map(|m| m.id.clone()))
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
        .rounding(6.0)
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
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Change ›")
                                        .small()
                                        .color(theme.text_secondary),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Open the AI Models dialog")
                            .clicked()
                        {
                            *show_models_dialog = true;
                        }
                    },
                );
            });
        });
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

/// Compile-time device label for the model card's "Inferring on
/// {device}" sub-line. Mirrors the AI Models dialog's footer
/// status — feature flag is the source of truth for whether GPU
/// acceleration is wired (runtime libonnxruntime is a separate
/// concern surfaced in the dialog's hover tooltip).
fn inference_device_label() -> &'static str {
    if cfg!(feature = "ai-ort-cuda") || cfg!(feature = "ai-candle-cuda") {
        "GPU (NVIDIA)"
    } else {
        "CPU"
    }
}

/// Inline label for a model's coarse kind, derived from the
/// capability bits — no separate source-of-truth needed in the
/// panel (the registry's `MaskCapabilities` IS the truth).
fn capability_kind_label(caps: klipp_ml::MaskCapabilities) -> &'static str {
    if caps.contains(klipp_ml::MaskCapabilities::TEMPORAL) {
        "Video Tracker"
    } else if caps.contains(klipp_ml::MaskCapabilities::POINT_PROMPT)
        || caps.contains(klipp_ml::MaskCapabilities::BOX_PROMPT)
    {
        "Mask Generator"
    } else if caps.contains(klipp_ml::MaskCapabilities::AUTO_DETECT) {
        "Mask Generator"
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
        .stroke(egui::Stroke::new(1.0, accent))
        .rounding(10.0)
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
        .rounding(3.0)
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
            "Track Object isn't enabled — install a temporal model from AI \
             Models first."
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
        .stroke(egui::Stroke::new(1.0, theme.marker_warning))
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
            egui::Stroke::new(1.0, theme.marker_warning)
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
            done_preview_v1(ui, state, theme);
        }
        _ => {
            empty_output_placeholder(ui, theme);
        }
    }
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
        .rounding(6.0)
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
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, theme.marker_warning));
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
                    egui::Stroke::new(1.5, egui::Color32::WHITE),
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
        ui.painter().rect_filled(rect, 3.0, theme.bg_raised);
    } else if enabled && resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, theme.bg_raised.gamma_multiply(0.5));
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        mode.label(),
        egui::FontId::proportional(11.5),
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
        .rounding(6.0)
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

/// v1 fallback preview — keeps the existing single-mask + N-row
/// candidate list working for users already mid-flow when this
/// commit lands. Commit 3 replaces this with a proper
/// display-mode-tabbed preview (Overlay / Mask only / Cutout).
fn done_preview_v1(ui: &mut egui::Ui, state: &AiToolsState, theme: &KlippTheme) {
    let response = match &state.inference {
        InferenceState::Done { response, .. } => response,
        _ => return,
    };
    let first = &response.candidates[0];
    ui.label(format!(
        "{} detection{} · {} ms · {}×{}",
        response.candidates.len(),
        if response.candidates.len() == 1 { "" } else { "s" },
        response.inference_ms,
        first.mask.width,
        first.mask.height,
    ));
    const MAX_ROWS: usize = 16;
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (idx, c) in response.candidates.iter().take(MAX_ROWS).enumerate() {
                ui.horizontal(|ui| {
                    let chip_size = egui::vec2(12.0, 12.0);
                    let (chip_rect, _) = ui.allocate_exact_size(
                        chip_size,
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(
                        chip_rect,
                        2.0,
                        crate::ai_tools::detection_color(idx),
                    );
                    let label = c.label.as_deref().unwrap_or("?");
                    ui.label(format!("{} — {:.0}%", label, c.confidence * 100.0));
                });
            }
            if response.candidates.len() > MAX_ROWS {
                ui.label(
                    egui::RichText::new(format!(
                        "(+{} more — viewer shows all)",
                        response.candidates.len() - MAX_ROWS,
                    ))
                    .weak()
                    .color(theme.text_dim),
                );
            }
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
    fn pick_default_falls_back_to_first_when_no_history() {
        let state = AiToolsState::default();
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
    fn pick_default_drops_stale_remembered_choice() {
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_xlarge_v9_doesnt_exist".into(),
        );
        let capable = fake_capable();
        let pick = pick_default_model_for_task(AiTask::DetectObjects, &state, &capable);
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
            "RemoveBackground should fall back to first, not pick up DetectObjects' history",
        );
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
