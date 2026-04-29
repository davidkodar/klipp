//! egui rendering for the AI Tools panel.
//!
//! Visual layout (Side panel docked right by default; toggleable):
//!
//! ```text
//! ┌────────────────────────────────────────────┐
//! │ AI Tools                            [×]    │
//! ├────────────────────────────────────────────┤
//! │ ┌────────┬──────────┬─────────┬──────────┐ │
//! │ │Remove  │ Isolate  │ Detect  │  Track   │ │
//! │ │  BG    │  Object  │ Objects │  Object  │ │
//! │ └────────┴──────────┴─────────┴──────────┘ │
//! ├────────────────────────────────────────────┤
//! │ Model:  [Stub (placeholder)        ▾]      │
//! │ Caps:   POINT_PROMPT  AUTO_DETECT          │
//! │ Cached: ✓                                  │
//! ├────────────────────────────────────────────┤
//! │ [ Run on current frame ]                   │
//! ├────────────────────────────────────────────┤
//! │ Mask preview:                              │
//! │ ┌──────────────┐                           │
//! │ │  (preview    │  conf 0.92                │
//! │ │   thumbnail) │  inference 14 ms          │
//! │ └──────────────┘                           │
//! └────────────────────────────────────────────┘
//! ```
//!
//! The actual click-on-viewer plumbing (collecting `MaskPrompts`
//! from cursor events on the viewer canvas) is Track F's job. This
//! file only renders the panel + advances the dispatch state
//! machine. The "Run on current frame" button moves
//! Clicking "Run on current frame" sets `state.pending_run = true`;
//! the app dispatcher drains that, builds an `OwnedMaskRequest` from
//! the live viewer frame, and calls `spawn_inference`. The panel
//! never touches the worker thread directly.

use klipp_ml::{MaskCapabilities, ModelInfo};

use super::registry::{required_capability, AiToolsRegistry};
use super::{AiTask, AiToolsState, InferenceState};

/// Stateless renderer for the AI Tools panel. Holds nothing —
/// every frame the caller passes in the mutable state + the
/// (read-only) registry.
pub struct AiToolsPanel;

impl AiToolsPanel {
    /// Render the panel inside the supplied egui Ui (typically a
    /// `SidePanel` body the caller has already opened, or a
    /// `Window` central area). The caller decides docking.
    pub fn show(ui: &mut egui::Ui, state: &mut AiToolsState, registry: &AiToolsRegistry) {
        ui.heading("AI Tools");
        ui.add_space(4.0);

        // ── Task buttons ────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            for task in AiTask::ALL {
                task_button(ui, *task, state, registry);
            }
        });

        ui.add_space(8.0);
        ui.separator();

        // ── Model picker + info card ────────────────────────────
        match state.active_task {
            Some(task) => model_section(ui, task, state, registry),
            None => {
                ui.label("Pick a task above to choose a model.");
            }
        }

        ui.add_space(8.0);
        ui.separator();

        // ── Per-model tunables ──────────────────────────────────
        tunables_section(ui, state);

        ui.add_space(8.0);
        ui.separator();

        // ── Prompt mode + summary ───────────────────────────────
        prompt_section(ui, state);

        ui.add_space(8.0);
        ui.separator();

        // ── Run button ──────────────────────────────────────────
        run_section(ui, state, registry);

        ui.add_space(8.0);
        ui.separator();

        // ── Mask preview ────────────────────────────────────────
        preview_section(ui, state);
    }
}

fn task_button(
    ui: &mut egui::Ui,
    task: AiTask,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
) {
    let label = task.display_label();
    let has_model = registry.has_model_for(task);
    let active = state.active_task == Some(task);

    // Greyed out when no model satisfies the task; click is
    // suppressed and the tooltip nudges the user to install one.
    let response = ui.add_enabled(
        has_model,
        egui::Button::new(label)
            .min_size(egui::vec2(110.0, 44.0))
            .selected(active),
    );

    if !has_model {
        response.on_hover_text("No model installed — open Model Manager");
        return;
    }

    if response.clicked() {
        let capable = registry.models_for(task);
        state.active_task = Some(task);
        state.selected_model_id = pick_default_model_for_task(task, state, &capable);
    }
}

/// Decide which model id should pre-populate the dropdown when the
/// user (re)opens a task. Restores the last explicit pick if it's
/// still capable for the task; otherwise falls back to the first
/// capable model. Pure data logic — kept out of `task_button` so
/// the unit tests below can drive it without an egui Ui.
fn pick_default_model_for_task(
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

fn model_section(
    ui: &mut egui::Ui,
    task: AiTask,
    state: &mut AiToolsState,
    registry: &AiToolsRegistry,
) {
    let models = registry.models_for(task);

    ui.horizontal(|ui| {
        ui.label("Model:");

        let current_label = state
            .selected_model_id
            .as_ref()
            .and_then(|id| registry.info(id))
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| "—".to_string());

        egui::ComboBox::from_id_salt("ai_tools_model_picker")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for m in &models {
                    let is_selected = state.selected_model_id.as_deref() == Some(m.id.as_str());
                    if ui
                        .selectable_label(is_selected, &m.display_name)
                        .clicked()
                    {
                        state.selected_model_id = Some(m.id.clone());
                        // Remember the explicit pick under the
                        // current task so the next switch back to
                        // this task restores it. Keyed by `task`
                        // (the function-local arg), which is the
                        // same as state.active_task here.
                        state
                            .last_picked_model_per_task
                            .insert(task, m.id.clone());
                        // Persist to ~/.config/klipp/ai_tools.toml
                        // so the pick survives the next launch.
                        // Save failures are logged + swallowed —
                        // the in-memory state still works for the
                        // current session.
                        super::persistence::save(
                            &super::persistence::AiToolsConfig::from_map(
                                &state.last_picked_model_per_task,
                            ),
                        );
                    }
                }
            });
    });

    // Info card for the picked model.
    if let Some(info) = state
        .selected_model_id
        .as_ref()
        .and_then(|id| registry.info(id))
    {
        info_card(ui, &info, task);
    } else {
        ui.label("(no model selected)");
    }
}

fn info_card(ui: &mut egui::Ui, info: &ModelInfo, task: AiTask) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            ui.label(format!("Display name: {}", info.display_name));
            ui.label(format!("Id: {}", info.id));
            ui.horizontal(|ui| {
                ui.label("Capabilities:");
                for (flag, name) in capability_labels(info.capabilities) {
                    let required = required_capability(task);
                    let highlight = required.contains(flag);
                    let mut text = egui::RichText::new(name);
                    if highlight {
                        text = text.strong();
                    }
                    ui.label(text);
                }
            });
            // Cached/not-cached state will surface once we wire in
            // klipp_ml::ModelCache lookups; UI placeholder for now.
            ui.label("Cached: (status pending cache wire-up)");
        });
}

fn capability_labels(caps: MaskCapabilities) -> Vec<(MaskCapabilities, &'static str)> {
    let mut out = Vec::new();
    let pairs: &[(MaskCapabilities, &'static str)] = &[
        (MaskCapabilities::POINT_PROMPT, "POINT"),
        (MaskCapabilities::BOX_PROMPT, "BOX"),
        (MaskCapabilities::SCRIBBLE, "SCRIBBLE"),
        (MaskCapabilities::TEXT_PROMPT, "TEXT"),
        (MaskCapabilities::AUTO_DETECT, "AUTO"),
        (MaskCapabilities::TEMPORAL, "TEMPORAL"),
        (MaskCapabilities::BATCH, "BATCH"),
    ];
    for (flag, name) in pairs {
        if caps.contains(*flag) {
            out.push((*flag, *name));
        }
    }
    out
}

/// Per-model tuning knobs. Today only RF-DETR-Seg variants expose
/// any: a confidence cut + an NMS overlap threshold. Both clamp to
/// safe ranges on read inside the generator, so the slider can't
/// produce a degenerate request. Other models render nothing.
fn tunables_section(ui: &mut egui::Ui, state: &mut AiToolsState) {
    let Some(model_id) = state.selected_model_id.clone() else {
        return;
    };
    if !is_rf_detr(&model_id) {
        return;
    }

    let entry = state
        .model_tunables
        .entry(model_id.clone())
        .or_default();

    // Convert from Option<f32> → f32 with the same defaults the
    // builder uses, so the slider always shows a concrete value.
    // 0.5 / 0.5 are the construction defaults from
    // `RfDetrSegBuilder::new()`.
    let mut score = entry.score_threshold.unwrap_or(0.5);
    let mut nms = entry.nms_iou_threshold.unwrap_or(0.5);

    ui.label(egui::RichText::new("Detection knobs").small().strong());
    ui.horizontal(|ui| {
        ui.label("Score:");
        if ui
            .add(egui::Slider::new(&mut score, 0.0..=1.0).fixed_decimals(2))
            .on_hover_text(
                "Minimum confidence for emitting a detection. Lower \
                 surfaces more candidates (including noisier ones).",
            )
            .changed()
        {
            entry.score_threshold = Some(score);
        }
    });
    ui.horizontal(|ui| {
        ui.label("NMS:");
        if ui
            .add(egui::Slider::new(&mut nms, 0.0..=1.05).fixed_decimals(2))
            .on_hover_text(
                "IoU + IoMin threshold above which two same-class \
                 detections collapse to one. Set to 1.05 to disable \
                 NMS entirely (raw model output).",
            )
            .changed()
        {
            entry.nms_iou_threshold = Some(nms);
        }
    });

    if ui
        .small_button("Reset to defaults")
        .on_hover_text("Restore the model's built-in score + NMS values")
        .clicked()
    {
        entry.score_threshold = None;
        entry.nms_iou_threshold = None;
    }
}

/// Recognise RF-DETR-Seg variants by stable id prefix. The only
/// models today that expose tuning knobs through the panel sliders.
fn is_rf_detr(model_id: &str) -> bool {
    model_id.starts_with("rf_detr_seg_")
}

fn prompt_section(ui: &mut egui::Ui, state: &mut AiToolsState) {
    use super::PromptMode;
    ui.label("Prompt mode:");
    ui.horizontal(|ui| {
        for (mode, label) in [
            (PromptMode::PointPrompt, "Point"),
            (PromptMode::BoxPrompt, "Box"),
            (PromptMode::Scribble, "Scribble"),
        ] {
            let active = state.prompts.mode == mode;
            if ui
                .add(egui::Button::new(label).selected(active))
                .clicked()
            {
                state.prompts.set_mode(mode);
            }
        }
    });

    let prompts = &state.prompts;
    ui.label(format!(
        "Captured: {} click(s), {} box(es), {} scribble(s)",
        prompts.clicks.len(),
        prompts.boxes.len(),
        prompts.finalized_scribbles.len(),
    ));

    if ui.button("Clear prompts").clicked() {
        state.prompts.clear();
    }
}

fn run_section(ui: &mut egui::Ui, state: &mut AiToolsState, registry: &AiToolsRegistry) {
    let can_run = state.active_task.is_some()
        && state
            .selected_model_id
            .as_ref()
            .map(|id| registry.info(id).is_some())
            .unwrap_or(false)
        && !matches!(state.inference, InferenceState::Running { .. });

    if ui
        .add_enabled(
            can_run,
            egui::Button::new("Run on current frame").min_size(egui::vec2(180.0, 32.0)),
        )
        .on_disabled_hover_text(disabled_run_reason(state))
        .clicked()
    {
        state.pending_run = true;
    }

    if matches!(state.inference, InferenceState::Running { .. })
        && ui.button("Cancel").clicked()
    {
        state.pending_cancel = true;
    }

    // Status line under the button so the user always sees state.
    match &state.inference {
        InferenceState::Idle => {
            ui.label(egui::RichText::new("Ready").weak());
        }
        InferenceState::Running { model_id, .. } => {
            ui.label(format!("Running on {}…", model_id));
        }
        InferenceState::Failed { error, .. } => {
            ui.colored_label(egui::Color32::from_rgb(220, 80, 80), error);
        }
        InferenceState::Done { response, .. } => {
            ui.label(format!("Done in {} ms", response.inference_ms));
        }
    }
}

fn disabled_run_reason(state: &AiToolsState) -> &'static str {
    if state.active_task.is_none() {
        "Pick a task first"
    } else if state.selected_model_id.is_none() {
        "Pick a model first"
    } else if matches!(state.inference, InferenceState::Running { .. }) {
        "Inference already running"
    } else {
        "Cannot run"
    }
}

fn preview_section(ui: &mut egui::Ui, state: &AiToolsState) {
    match &state.inference {
        InferenceState::Done { response, .. } => {
            if response.candidates.is_empty() {
                ui.label("Mask preview:");
                ui.label(egui::RichText::new("(empty response)").weak());
                return;
            }

            // Header: total count + first-mask resolution + total
            // inference time. Single-candidate output (BiRefNet,
            // MobileSAM) reads naturally because the count is 1.
            let first = &response.candidates[0];
            ui.label(format!(
                "{} detection{} • {} ms • {}×{}",
                response.candidates.len(),
                if response.candidates.len() == 1 { "" } else { "s" },
                response.inference_ms,
                first.mask.width,
                first.mask.height,
            ));

            // Per-candidate row: a colored chip matching the bbox /
            // mask tint in the viewer, plus label + confidence.
            // Capped at 16 rows so a busy frame doesn't blow the
            // panel's height — the viewer overlay still draws all
            // of them.
            const MAX_ROWS: usize = 16;
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (idx, c) in response.candidates.iter().take(MAX_ROWS).enumerate() {
                        ui.horizontal(|ui| {
                            // Color chip — keep it small + square so the
                            // row stays readable.
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
                            ui.label(format!(
                                "{} — {:.0}%",
                                label,
                                c.confidence * 100.0,
                            ));
                        });
                    }
                    if response.candidates.len() > MAX_ROWS {
                        ui.label(
                            egui::RichText::new(format!(
                                "(+{} more — viewer shows all)",
                                response.candidates.len() - MAX_ROWS,
                            ))
                            .weak(),
                        );
                    }
                });
        }
        _ => {
            ui.label("Mask preview:");
            ui.label(egui::RichText::new("(none yet)").weak());
        }
    }
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
        // User had picked a model that's since been unregistered
        // (manifest churn, feature flag rebuild). The restoration
        // logic should fall back to the first capable model rather
        // than offering the stale id.
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
        // Picks for one task don't leak into another.
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
}
