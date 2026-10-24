//! On-disk persistence for AI Tools state that needs to survive
//! across sessions.
//!
//! Today this carries one thing: per-task default model picks. The
//! file format is TOML (consistent with `assets/models.toml` and the
//! user override at `~/.config/klipp/models.toml`) and lives at
//! `~/.config/klipp/ai_tools.toml`. Schema:
//!
//! ```toml
//! [last_picked]
//! detect_objects = "rf_detr_seg_medium_v1"
//! remove_background = "birefnet_lite_v1"
//! ```
//!
//! ## Why save on every change instead of on shutdown
//!
//! Klipp's shutdown path uses `std::process::exit(0)` (workaround
//! for BUG-SHUTDOWN-SEGV) which skips Rust drops. So `Drop` /
//! `on_exit` aren't reliable hooks for persistence. The file is
//! tiny (under 200 bytes for v1) and the write happens only on
//! dropdown clicks (rare), so the per-change save cost is
//! negligible.
//!
//! ## Failure mode
//!
//! Load failures (missing file, parse error, IO error) all fall
//! back to an empty config. Save failures log a warning and drop
//! the persistence — the in-memory state still works for the
//! current session.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::{AiTask, ModelTelemetry};

/// Wire-format struct. Field names are intentionally the
/// lowercase-snake-case spelling of `AiTask` variants so the file
/// is self-documenting; future task additions land here without
/// breaking older Klipp builds (#[serde(default)] keeps unknown
/// keys from breaking parse).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AiToolsConfig {
    /// Per-task default model id. Key is the snake_case task name
    /// (`detect_objects`, `remove_background`, etc.). Unknown task
    /// names are silently dropped on load — older Klipp builds
    /// shouldn't crash because a newer build wrote a task name they
    /// don't recognise yet.
    #[serde(default)]
    pub last_picked: HashMap<String, String>,
    /// Per-model usage telemetry. Key is the stable model id
    /// (`rf_detr_seg_medium_v1`, etc.). `last_used` is stored as
    /// Unix epoch seconds rather than RFC 3339 so the wire format
    /// has no transitive date-time crate dependency. Entries for
    /// models that are no longer registered round-trip cleanly —
    /// they're just not visible in the AI Models dialog.
    #[serde(default)]
    pub telemetry: HashMap<String, TelemetryWire>,
    /// User-added custom models — files Klipp didn't ship by
    /// default that the user pasted a URL for. Each entry carries
    /// enough metadata that startup can re-register the file
    /// against the matching MaskGenerator adapter without going
    /// back to the network.
    #[serde(default, rename = "custom_model")]
    pub custom_models: Vec<CustomModelEntry>,
}

/// Adapter that backs a custom model. The user picks one in the
/// paste-URL modal — Klipp can't infer the architecture from a raw
/// ONNX without loading it, and the load itself is the expensive
/// step we want to defer to first inference. Today three options
/// matching the three real backends in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomAdapter {
    BiRefNet,
    MobileSam,
    RfDetrSeg,
}

impl CustomAdapter {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::BiRefNet => "BiRefNet (matting)",
            Self::MobileSam => "MobileSAM (point/box prompts)",
            Self::RfDetrSeg => "RF-DETR-Seg (auto-detect)",
        }
    }

    pub const ALL: &'static [CustomAdapter] = &[
        CustomAdapter::BiRefNet,
        CustomAdapter::MobileSam,
        CustomAdapter::RfDetrSeg,
    ];
}

/// One user-supplied model. `id` must be unique across both built-
/// in and custom entries; the paste-URL flow rejects collisions
/// before persisting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModelEntry {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub adapter: CustomAdapter,
}

/// On-disk representation of [`ModelTelemetry`]. Mirrors the in-
/// memory shape but with `last_used_unix: u64` instead of
/// `SystemTime`. `recent_inference_ms` is a `Vec<u64>` because TOML
/// can't serialize `VecDeque` directly; conversion is one-line.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TelemetryWire {
    /// Unix epoch seconds. Zero means "never used" (matches the
    /// in-memory default of `SystemTime::UNIX_EPOCH`).
    #[serde(default)]
    pub last_used_unix: u64,
    /// Last 10 inference times in ms, oldest first.
    #[serde(default)]
    pub recent_inference_ms: Vec<u64>,
    /// Total successful runs across all sessions.
    #[serde(default)]
    pub total_runs: u64,
}

impl TelemetryWire {
    fn from_telemetry(t: &ModelTelemetry) -> Self {
        Self {
            last_used_unix: t
                .last_used
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            recent_inference_ms: t.recent_inference_ms.iter().copied().collect(),
            total_runs: t.total_runs,
        }
    }

    fn into_telemetry(self) -> ModelTelemetry {
        let last_used = if self.last_used_unix == 0 {
            UNIX_EPOCH
        } else {
            UNIX_EPOCH + std::time::Duration::from_secs(self.last_used_unix)
        };
        ModelTelemetry {
            last_used,
            recent_inference_ms: VecDeque::from(self.recent_inference_ms),
            total_runs: self.total_runs,
        }
    }
}

/// In-memory shape returned by [`AiToolsConfig::into_state_data`].
/// Three concerns the file persists, all in one struct so callers
/// don't have to thread separate return values through.
#[derive(Debug, Default)]
pub struct LoadedAiToolsState {
    pub last_picked_model_per_task: HashMap<AiTask, String>,
    pub model_telemetry: HashMap<String, ModelTelemetry>,
    pub custom_models: Vec<CustomModelEntry>,
}

impl AiToolsConfig {
    /// Materialise from in-memory `AiToolsState`. The dropdown
    /// handler + the inference-completion hook both call this
    /// before [`save`] so disk reflects current state every time
    /// the user touches anything that's persisted.
    pub fn from_state(state: &super::AiToolsState) -> Self {
        let last_picked = state
            .last_picked_model_per_task
            .iter()
            .map(|(task, id)| (task_to_key(*task).to_string(), id.clone()))
            .collect();
        let telemetry = state
            .model_telemetry
            .iter()
            .map(|(id, t)| (id.clone(), TelemetryWire::from_telemetry(t)))
            .collect();
        let custom_models = state.custom_models.clone();
        Self {
            last_picked,
            telemetry,
            custom_models,
        }
    }

    /// Decode the wire format into the two HashMaps that
    /// `AiToolsState` carries. Unknown task keys are silently
    /// dropped (forward-compat). Unknown model ids in the telemetry
    /// section round-trip cleanly — they sit in the map without a
    /// corresponding registered model and remain invisible until /
    /// unless that model id is registered in a later session.
    pub fn into_state_data(self) -> LoadedAiToolsState {
        let last_picked_model_per_task = self
            .last_picked
            .into_iter()
            .filter_map(|(k, v)| key_to_task(&k).map(|t| (t, v)))
            .collect();
        let model_telemetry = self
            .telemetry
            .into_iter()
            .map(|(id, w)| (id, w.into_telemetry()))
            .collect();
        LoadedAiToolsState {
            last_picked_model_per_task,
            model_telemetry,
            custom_models: self.custom_models,
        }
    }
}

/// Resolved path to the persistence file. Returns `None` when
/// `dirs::config_dir()` can't determine a home (CI sandbox, hostile
/// env). Callers fall back to "no persistence" in that case.
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("klipp").join("ai_tools.toml"))
}

/// Read the config from disk. Returns an empty config on any
/// failure path (missing file, parse error, IO error). Each
/// failure is logged at the appropriate level so a debugging user
/// can find what went wrong without the app crashing on a corrupt
/// file.
pub fn load() -> AiToolsConfig {
    let Some(path) = config_path() else {
        tracing::warn!(
            target: "klipp_ui",
            "ai_tools persistence: no config dir, defaults will not persist"
        );
        return AiToolsConfig::default();
    };
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return AiToolsConfig::default();
        }
        Err(e) => {
            tracing::warn!(
                target: "klipp_ui",
                "ai_tools persistence: read failed at {}: {e}",
                path.display()
            );
            return AiToolsConfig::default();
        }
    };
    match toml::from_str(&body) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                target: "klipp_ui",
                "ai_tools persistence: parse failed at {}: {e}",
                path.display()
            );
            AiToolsConfig::default()
        }
    }
}

/// Write the config to disk. Creates `~/.config/klipp/` if
/// missing. Failures are logged + swallowed — the in-memory state
/// still works for the current session even when persistence
/// breaks.
pub fn save(cfg: &AiToolsConfig) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                target: "klipp_ui",
                "ai_tools persistence: mkdir failed at {}: {e}",
                parent.display()
            );
            return;
        }
    }
    let body = match toml::to_string_pretty(cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "klipp_ui",
                "ai_tools persistence: serialize failed: {e}"
            );
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(
            target: "klipp_ui",
            "ai_tools persistence: write failed at {}: {e}",
            path.display()
        );
    }
}

/// Stable string spelling for each task. Pinned by the
/// `task_key_round_trips` test so renaming a variant doesn't
/// silently invalidate existing config files.
fn task_to_key(task: AiTask) -> &'static str {
    match task {
        AiTask::RemoveBackground => "remove_background",
        AiTask::IsolateObject => "isolate_object",
        AiTask::DetectObjects => "detect_objects",
        AiTask::TrackObject => "track_object",
    }
}

fn key_to_task(key: &str) -> Option<AiTask> {
    match key {
        "remove_background" => Some(AiTask::RemoveBackground),
        "isolate_object" => Some(AiTask::IsolateObject),
        "detect_objects" => Some(AiTask::DetectObjects),
        "track_object" => Some(AiTask::TrackObject),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_tools::AiToolsState;

    #[test]
    fn task_key_round_trips() {
        for task in AiTask::ALL {
            let key = task_to_key(*task);
            assert_eq!(
                key_to_task(key),
                Some(*task),
                "round-trip failed for {task:?}"
            );
        }
    }

    #[test]
    fn from_state_into_state_data_preserves_picks() {
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_medium_v1".to_string(),
        );
        state.last_picked_model_per_task.insert(
            AiTask::RemoveBackground,
            "birefnet_lite_v1".to_string(),
        );
        let cfg = AiToolsConfig::from_state(&state);
        let loaded = cfg.into_state_data();
        assert_eq!(
            loaded.last_picked_model_per_task,
            state.last_picked_model_per_task
        );
    }

    #[test]
    fn into_state_data_drops_unknown_task_keys() {
        // Future Klipp adds a new task → older Klipp shouldn't
        // crash on the unknown key, just ignore it.
        let mut cfg = AiToolsConfig::default();
        cfg.last_picked
            .insert("invent_a_new_task".into(), "some_model".into());
        cfg.last_picked
            .insert("detect_objects".into(), "rf_detr_seg_medium_v1".into());
        let loaded = cfg.into_state_data();
        assert_eq!(loaded.last_picked_model_per_task.len(), 1);
        assert_eq!(
            loaded
                .last_picked_model_per_task
                .get(&AiTask::DetectObjects)
                .map(String::as_str),
            Some("rf_detr_seg_medium_v1")
        );
    }

    #[test]
    fn default_config_has_empty_state() {
        let cfg = AiToolsConfig::default();
        let loaded = cfg.into_state_data();
        assert!(loaded.last_picked_model_per_task.is_empty());
        assert!(loaded.model_telemetry.is_empty());
    }

    #[test]
    fn missing_sections_parse_as_empty() {
        let cfg: AiToolsConfig = toml::from_str("").expect("empty body parses");
        assert!(cfg.last_picked.is_empty());
        assert!(cfg.telemetry.is_empty());
    }

    #[test]
    fn round_trip_through_toml_string_preserves_picks() {
        let mut state = AiToolsState::default();
        state.last_picked_model_per_task.insert(
            AiTask::IsolateObject,
            "mobile_sam_onnx".to_string(),
        );
        let cfg = AiToolsConfig::from_state(&state);
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: AiToolsConfig = toml::from_str(&body).expect("deserialize");
        let loaded = parsed.into_state_data();
        assert_eq!(
            loaded.last_picked_model_per_task,
            state.last_picked_model_per_task
        );
    }

    #[test]
    fn telemetry_round_trips_through_wire_format() {
        // Construct telemetry with concrete values, push it through
        // from_state → TOML → into_state_data, verify identity.
        let mut state = AiToolsState::default();
        let mut t = ModelTelemetry::default();
        t.record_run(142);
        t.record_run(148);
        t.record_run(139);
        // Snapshot the bits the wire format actually carries —
        // last_used SystemTime is preserved at 1-second resolution.
        let original_total = t.total_runs;
        let original_recent: Vec<u64> = t.recent_inference_ms.iter().copied().collect();
        let original_last_used_secs = t
            .last_used
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        state.model_telemetry.insert("test_model_v1".into(), t);

        let cfg = AiToolsConfig::from_state(&state);
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: AiToolsConfig = toml::from_str(&body).expect("deserialize");
        let loaded = parsed.into_state_data();

        let restored = loaded
            .model_telemetry
            .get("test_model_v1")
            .expect("model present");
        assert_eq!(restored.total_runs, original_total);
        let restored_recent: Vec<u64> =
            restored.recent_inference_ms.iter().copied().collect();
        assert_eq!(restored_recent, original_recent);
        let restored_secs = restored
            .last_used
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        assert_eq!(restored_secs, original_last_used_secs);
    }

    #[test]
    fn telemetry_for_unregistered_id_round_trips_silently() {
        // A model id that's no longer registered (e.g. because the
        // user disabled a feature flag in this build) should still
        // round-trip through TOML — the entry just sits in the map
        // and isn't visible until that id is registered again.
        let mut cfg = AiToolsConfig::default();
        cfg.telemetry.insert(
            "phantom_model_v9".into(),
            TelemetryWire {
                last_used_unix: 1_700_000_000,
                recent_inference_ms: vec![100, 200],
                total_runs: 5,
            },
        );
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: AiToolsConfig = toml::from_str(&body).expect("deserialize");
        let loaded = parsed.into_state_data();
        let t = loaded
            .model_telemetry
            .get("phantom_model_v9")
            .expect("phantom entry preserved");
        assert_eq!(t.total_runs, 5);
    }

    #[test]
    fn never_used_telemetry_loads_as_unix_epoch() {
        // last_used_unix=0 is the on-disk encoding for "never run".
        // The in-memory shape uses SystemTime::UNIX_EPOCH for the
        // same meaning; ModelTelemetry::has_runs() returns false
        // because total_runs is also 0 in that case.
        let mut cfg = AiToolsConfig::default();
        cfg.telemetry.insert(
            "fresh_install".into(),
            TelemetryWire::default(),
        );
        let loaded = cfg.into_state_data();
        let t = loaded
            .model_telemetry
            .get("fresh_install")
            .expect("entry present");
        assert!(!t.has_runs());
        assert_eq!(t.last_used, UNIX_EPOCH);
    }
}
