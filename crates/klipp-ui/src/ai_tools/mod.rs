//! AI Tools panel — task-oriented buttons (Remove Background /
//! Isolate Object / Detect Objects / Track Object) backed by
//! `klipp_ml::ModelRegistry`.
//!
//! ## Module layout
//!
//! * [`panel`] — egui rendering of the side-panel surface.
//! * [`registry`] — read-only view over the panel's stub registry
//!   (`AiToolsRegistry`); separate from the worker-side
//!   [`inference::RegistryHandle`].
//! * [`prompt_input`] — pure-data gesture capture
//!   (`PromptCapture`, `PromptMode`, `ClickButton`, `DragState`).
//! * [`inference`] — worker-thread dispatch
//!   (`spawn_inference`, `InferenceState`, `InferenceHandle`,
//!   `OwnedMaskRequest`, `RegistryHandle`).
//! * [`overlay`] — semi-transparent mask preview renderer.
//!
//! ## AppState integration
//!
//! `AppState` carries:
//!
//! ```ignore
//! pub ai_tools_state: ai_tools::AiToolsState,
//! pub ai_tools_registry: ai_tools::AiToolsRegistry,
//! pub ml_registry: ai_tools::RegistryHandle,
//! ```
//!
//! Render the panel from `app::update`:
//!
//! ```ignore
//! if state.ai_tools_state.panel_visible {
//!     egui::SidePanel::right("ai_tools_side")
//!         .resizable(true)
//!         .default_width(280.0)
//!         .show(ctx, |ui| {
//!             ai_tools::AiToolsPanel::show(
//!                 ui,
//!                 &mut state.ai_tools_state,
//!                 &state.ai_tools_registry,
//!             );
//!         });
//! }
//! ```
//!
//! The panel's "Run on current frame" button only flips
//! `state.ai_tools_state.pending_run = true`; the dispatcher in
//! `app.rs` drains that flag, builds an [`OwnedMaskRequest`] from
//! the current viewer frame + `prompt_input::PromptCapture::build()`,
//! and calls [`spawn_inference`].

pub mod custom_install;
pub mod inference;
pub mod overlay;
pub mod panel;
pub mod persistence;
pub mod prompt_input;
pub mod registry;
pub mod telemetry;

pub use inference::{
    first_candidate, run_inference_blocking, spawn_inference, InferenceEvent, InferenceHandle,
    InferenceState, OwnedMaskRequest, RegistryHandle,
};
pub use overlay::{
    build_mask_texture, build_multi_detection_texture, colorize_mask, detection_color,
    paint_mask_texture, render_mask_overlay, render_mask_overlay_with_label,
    DEFAULT_MASK_OVERLAY_COLOR, DETECTION_PALETTE,
};
pub use panel::AiToolsPanel;
pub use prompt_input::{ClickButton, DragState, PromptCapture, PromptMode};
pub use registry::AiToolsRegistry;
pub use telemetry::{format_relative, ModelTelemetry};

/// Top-level state for the AI Tools panel. Lives on `AppState`.
/// All fields default to "panel closed, nothing running" so adding
/// this to AppState is a no-op until the user actually opens it.
pub struct AiToolsState {
    /// Currently active task (one button highlighted). `None` means
    /// no task picked yet — the panel still renders, but no model
    /// dropdown is shown.
    pub active_task: Option<AiTask>,
    /// Model id picked from the per-task dropdown. May be stale if
    /// the user switches tasks; the panel always re-validates
    /// against the current task's capability filter.
    pub selected_model_id: Option<String>,
    /// Whether the panel itself is rendered. App startup leaves it
    /// hidden; a toolbar toggle / keybinding flips this.
    pub panel_visible: bool,
    /// Inference dispatch state machine. See
    /// [`inference::InferenceState`].
    pub inference: InferenceState,
    /// Set to `true` by the panel's "Run on current frame" button.
    /// The app dispatcher drains this each frame, builds an owned
    /// request, and calls [`spawn_inference`]. Reset back to `false`
    /// after the dispatcher consumes it.
    pub pending_run: bool,
    /// Set to `true` by the panel's "Cancel" button. Drained by the
    /// app dispatcher, which calls [`InferenceState::cancel`].
    pub pending_cancel: bool,
    /// User-prompt capture. The viewer mutates this on click / drag
    /// when the panel is open and `active_task.is_some()`. Snapshotted
    /// via [`PromptCapture::build`] when the dispatcher fires the
    /// inference.
    pub prompts: PromptCapture,
    /// Tint applied to the mask overlay over the viewer. Defaults to
    /// [`DEFAULT_MASK_OVERLAY_COLOR`] (orange, ~50% alpha).
    pub mask_overlay_color: egui::Color32,
    /// Cached overlay texture for the current `Done` mask. Owned here
    /// rather than re-uploaded per frame because dropping a
    /// `TextureHandle` mid-`update()` panics wgpu on the next submit
    /// (see `feedback_defer_drop_texture`). The dispatcher (re)builds
    /// it whenever inference completes and routes the old handle
    /// through `KlippApp::defer_drop_texture` on replacement.
    pub mask_overlay_texture: Option<egui::TextureHandle>,
    /// Per-model usage stats — populated each time the dispatcher
    /// observes an `InferenceState::Idle → Done` transition. Read by
    /// the AI Models dialog to surface "Last used: 2m ago • avg
    /// 142ms". Ephemeral: resets on app restart.
    pub model_telemetry: std::collections::HashMap<String, ModelTelemetry>,
    /// Per-model tuning knobs (RF-DETR's score / NMS thresholds, etc.)
    /// that the user has set via the panel sliders. Keyed by stable
    /// model id; the dispatcher hands the matching entry into
    /// [`OwnedMaskRequest::tunables`] each run, so changing a slider
    /// takes effect on the next click of "Run on current frame"
    /// without rebuilding the cached generator instance. Ephemeral.
    pub model_tunables: std::collections::HashMap<String, klipp_ml::MaskTunables>,
    /// Last model the user picked for each task. Restored when the
    /// user switches back to that task, so an explicit "RF-DETR-Seg
    /// (Medium)" choice on Detect Objects survives a round-trip
    /// through Remove Background. Falls back to the first capable
    /// model when no entry is stored. Ephemeral — resets on app
    /// restart.
    pub last_picked_model_per_task: std::collections::HashMap<AiTask, String>,
    /// User-added custom models. Each was pasted as a URL, downloaded,
    /// hashed + size-checked, then confirmed by the user. Persisted
    /// to ~/.config/klipp/ai_tools.toml under [[custom_model]] so the
    /// next launch can re-register them without re-downloading. The
    /// shared registry holds the matching factory closures.
    pub custom_models: Vec<persistence::CustomModelEntry>,
    /// Dispatch scope for the next inference run. Today only
    /// [`AiScope::CurrentFrame`] actually fires inference; the
    /// other variants are visible-but-disabled chips so the panel
    /// telegraphs the planned batch features. See AppCommand
    /// dispatch in `app.rs`.
    pub scope: AiScope,
    /// Whether the panel's Advanced section (mask threshold, edge
    /// feather, output format) is currently expanded. Persists for
    /// the lifetime of the panel; resets on app restart.
    pub advanced_open: bool,
    /// Per-task post-processing knobs surfaced in the Advanced
    /// drawer. `mask_threshold` aliases `MaskTunables.score_threshold`
    /// when sent to inference; `edge_feather` is a CPU post-process
    /// applied to the final mask; `output_format` controls how the
    /// mask leaves the panel toward Apply-to-clip.
    pub advanced_settings: AdvancedSettings,
}

/// Where the next inference run should target. Visible UI for all
/// three even though only `CurrentFrame` is wired today — Selection
/// and FullClip are disabled chips with "coming soon" tooltips so
/// the planned batch features stay forward-compatible in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiScope {
    #[default]
    CurrentFrame,
    Selection,
    FullClip,
}

impl AiScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::CurrentFrame => "Current frame",
            Self::Selection => "Selection",
            Self::FullClip => "Full clip",
        }
    }

    /// `true` when the scope is wired through to a real dispatch
    /// path. Only CurrentFrame is real today; the others render
    /// as disabled chips.
    pub fn is_implemented(self) -> bool {
        matches!(self, Self::CurrentFrame)
    }
}

/// Per-panel mask post-processing knobs. Surface lives in the
/// Advanced drawer; values flow into the inference dispatch +
/// post-process path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdvancedSettings {
    /// Binarisation threshold for soft alpha output. RF-DETR uses
    /// this directly as `score_threshold`; BiRefNet treats it as
    /// the cut between alpha and hard mask in the post-process.
    /// Range [0.0, 1.0]; default 0.5.
    pub mask_threshold: f32,
    /// Gaussian-blur radius applied to the mask edges in pixels
    /// before the result lands in the panel's preview / Apply
    /// path. 0 = no feather. Default 2.
    pub edge_feather: u32,
    /// How the mask is encoded for delivery to a clip via
    /// Apply-to-clip. v1 ships AlphaMatte16 only; the enum is
    /// here so the dropdown surface is forward-compatible.
    pub output_format: OutputFormat,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            mask_threshold: 0.5,
            edge_feather: 2,
            output_format: OutputFormat::AlphaMatte16,
        }
    }
}

/// Bit-depth + format options for the panel's mask output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    AlphaMatte16,
    AlphaMatte8,
    HardBinary,
}

impl OutputFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::AlphaMatte16 => "Alpha matte (16-bit)",
            Self::AlphaMatte8 => "Alpha matte (8-bit)",
            Self::HardBinary => "Hard binary",
        }
    }

    pub const ALL: &'static [OutputFormat] = &[
        OutputFormat::AlphaMatte16,
        OutputFormat::AlphaMatte8,
        OutputFormat::HardBinary,
    ];
}

impl Default for AiToolsState {
    fn default() -> Self {
        Self {
            active_task: None,
            selected_model_id: None,
            panel_visible: false,
            inference: InferenceState::default(),
            pending_run: false,
            pending_cancel: false,
            prompts: PromptCapture::default(),
            mask_overlay_color: DEFAULT_MASK_OVERLAY_COLOR,
            mask_overlay_texture: None,
            model_telemetry: std::collections::HashMap::new(),
            model_tunables: std::collections::HashMap::new(),
            last_picked_model_per_task: std::collections::HashMap::new(),
            custom_models: Vec::new(),
            scope: AiScope::default(),
            advanced_open: false,
            advanced_settings: AdvancedSettings::default(),
        }
    }
}

/// The four task buttons exposed by the panel. Order matters —
/// [`AiTask::ALL`] iterates in this declaration order so the UI is
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiTask {
    /// One-click background matting (BiRefNet, RVM).
    RemoveBackground,
    /// SAM-style click-to-mask (foreground object isolation).
    IsolateObject,
    /// Auto-detect objects with class labels (RF-DETR-Seg, YOLO-Seg).
    DetectObjects,
    /// Propagate a mask across frames (SAM2 video, XMem). v1 has no
    /// model satisfying TEMPORAL → button greys out.
    TrackObject,
}

impl AiTask {
    /// Stable iteration order for the panel.
    pub const ALL: &'static [AiTask] = &[
        AiTask::RemoveBackground,
        AiTask::IsolateObject,
        AiTask::DetectObjects,
        AiTask::TrackObject,
    ];

    /// Short label for the task button.
    pub fn display_label(self) -> &'static str {
        match self {
            AiTask::RemoveBackground => "Remove BG",
            AiTask::IsolateObject => "Isolate Object",
            AiTask::DetectObjects => "Detect Objects",
            AiTask::TrackObject => "Track Object",
        }
    }

    /// One-line description rendered under the task tile title in
    /// the panel's task grid. Reads as something you'd say out
    /// loud to describe the user-facing intent of the task.
    pub fn short_description(self) -> &'static str {
        match self {
            AiTask::RemoveBackground => "Mask the subject, drop the background",
            AiTask::IsolateObject => "Click or box-select a subject",
            AiTask::DetectObjects => "Find and label everything in frame",
            AiTask::TrackObject => "Follow a subject across frames",
        }
    }

    /// Icon shown in the task tile. v1 uses closest-fit icons from
    /// our existing Phosphor set; the four follow-up SVGs noted in
    /// commit notes will tighten the visual language.
    /// (Eye for matting, Cursor for click-to-mask, GridFour for
    /// multi-detect, ArrowsOut as a weak motion proxy for tracking.)
    pub fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            AiTask::RemoveBackground => Icon::Eye,
            AiTask::IsolateObject => Icon::Cursor,
            AiTask::DetectObjects => Icon::GridFour,
            AiTask::TrackObject => Icon::ArrowsOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_ml::{MaskBuffer, MaskCandidate, MaskCapabilities, MaskResponse};

    // ── State defaults ──────────────────────────────────────────

    #[test]
    fn default_state_is_idle_and_hidden() {
        let s = AiToolsState::default();
        assert!(s.active_task.is_none());
        assert!(s.selected_model_id.is_none());
        assert!(!s.panel_visible);
        assert!(matches!(s.inference, InferenceState::Idle));
        assert!(!s.pending_run);
        assert!(!s.pending_cancel);
    }

    #[test]
    fn ai_task_all_lists_four_tasks_in_order() {
        // The panel renders in this order; reordering would be a
        // visible UX change and should require an explicit test
        // update.
        assert_eq!(AiTask::ALL.len(), 4);
        assert_eq!(AiTask::ALL[0], AiTask::RemoveBackground);
        assert_eq!(AiTask::ALL[1], AiTask::IsolateObject);
        assert_eq!(AiTask::ALL[2], AiTask::DetectObjects);
        assert_eq!(AiTask::ALL[3], AiTask::TrackObject);
    }

    #[test]
    fn display_labels_are_user_friendly() {
        for task in AiTask::ALL {
            assert!(!task.display_label().is_empty());
        }
    }

    // ── State transitions ───────────────────────────────────────

    #[test]
    fn switching_task_updates_active_task() {
        let mut s = AiToolsState::default();
        s.active_task = Some(AiTask::IsolateObject);
        assert_eq!(s.active_task, Some(AiTask::IsolateObject));
        s.active_task = Some(AiTask::RemoveBackground);
        assert_eq!(s.active_task, Some(AiTask::RemoveBackground));
    }

    #[test]
    fn done_state_carries_response() {
        // Smoke: a Done state from the worker round-trips through
        // AiToolsState — what the dispatcher will write each time
        // an inference finishes.
        let resp = MaskResponse {
            candidates: vec![MaskCandidate {
                mask: MaskBuffer::solid(4, 4, 200),
                confidence: 0.9,
                bbox: None,
                label: Some("person".into()),
            }],
            inference_ms: 17,
        };
        let mut s = AiToolsState::default();
        s.inference = InferenceState::Done {
            model_id: "stub".into(),
            response: resp,
        };
        match &s.inference {
            InferenceState::Done { response, .. } => {
                assert_eq!(response.inference_ms, 17);
                assert_eq!(response.candidates.len(), 1);
                assert_eq!(response.candidates[0].mask.at(2, 2), 200);
            }
            _ => panic!("expected Done"),
        }
    }

    // ── Capability filter on registry ───────────────────────────

    #[test]
    fn registry_capability_filter_per_task() {
        // The wrapper auto-registers the stub, which advertises
        // POINT_PROMPT | BOX_PROMPT | SCRIBBLE | AUTO_DETECT but
        // NOT TEMPORAL. So Isolate / RemoveBG / Detect resolve to
        // the stub; Track Object resolves to nothing.
        let r = AiToolsRegistry::with_stub_registered();

        let isolate = r.models_for(AiTask::IsolateObject);
        assert_eq!(isolate.len(), 1);
        assert_eq!(isolate[0].id, klipp_ml::STUB_GENERATOR_ID);

        let bg = r.models_for(AiTask::RemoveBackground);
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].id, klipp_ml::STUB_GENERATOR_ID);

        let detect = r.models_for(AiTask::DetectObjects);
        assert_eq!(detect.len(), 1);

        let track = r.models_for(AiTask::TrackObject);
        assert!(track.is_empty(), "no v1 model satisfies TEMPORAL");
    }

    #[test]
    fn empty_registry_signals_no_model_for_every_task() {
        let r = AiToolsRegistry::empty();
        for task in AiTask::ALL {
            assert!(
                !r.has_model_for(*task),
                "empty registry must have nothing for {:?}",
                task
            );
        }
    }

    #[test]
    fn stub_capabilities_match_panel_assumptions() {
        // The panel assumes the stub satisfies every prompt-bearing
        // task except Track Object. Pinning here so a future
        // capability narrowing in the stub doesn't silently change
        // panel behaviour.
        let cap = klipp_ml::StubMaskGenerator::new();
        let info = AiToolsRegistry::with_stub_registered()
            .info(klipp_ml::STUB_GENERATOR_ID)
            .expect("stub registered");
        assert!(info.capabilities.contains(MaskCapabilities::POINT_PROMPT));
        assert!(info.capabilities.contains(MaskCapabilities::AUTO_DETECT));
        assert!(!info.capabilities.contains(MaskCapabilities::TEMPORAL));
        // Suppress unused-warning on `cap` — it's here to cross-
        // reference the constructor we're pinning behaviour against.
        let _ = cap;
    }
}
