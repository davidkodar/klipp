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

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::AiTask;

/// Wire-format struct. Field names are intentionally the
/// lowercase-snake-case spelling of `AiTask` variants so the file
/// is self-documenting; future task additions land here without
/// breaking older Klipp builds (#[serde(default)] keeps unknown
/// keys from breaking parse).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AiToolsConfig {
    /// Per-task default model id. Key is the snake_case task name
    /// (`detect_objects`, `remove_background`, etc.). Unknown task
    /// names round-trip through `Unknown` keys that are silently
    /// dropped on load.
    #[serde(default)]
    pub last_picked: HashMap<String, String>,
}

impl AiToolsConfig {
    /// Materialise from an in-memory `HashMap<AiTask, String>`.
    /// Used by the dropdown handler before [`Self::save`].
    pub fn from_map(picks: &HashMap<AiTask, String>) -> Self {
        let last_picked = picks
            .iter()
            .map(|(task, id)| (task_to_key(*task).to_string(), id.clone()))
            .collect();
        Self { last_picked }
    }

    /// Turn the wire-format back into `HashMap<AiTask, String>` for
    /// the panel to consume. Unknown task keys are silently
    /// dropped — older Klipp builds shouldn't crash because a newer
    /// build wrote a task name they don't recognise yet.
    pub fn into_map(self) -> HashMap<AiTask, String> {
        self.last_picked
            .into_iter()
            .filter_map(|(k, v)| key_to_task(&k).map(|t| (t, v)))
            .collect()
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
    fn from_map_into_map_preserves_picks() {
        let mut picks = HashMap::new();
        picks.insert(
            AiTask::DetectObjects,
            "rf_detr_seg_medium_v1".to_string(),
        );
        picks.insert(
            AiTask::RemoveBackground,
            "birefnet_lite_v1".to_string(),
        );
        let cfg = AiToolsConfig::from_map(&picks);
        let restored = cfg.into_map();
        assert_eq!(restored, picks);
    }

    #[test]
    fn into_map_drops_unknown_keys() {
        // Future Klipp adds a new task → older Klipp shouldn't
        // crash on the unknown key, just ignore it.
        let mut cfg = AiToolsConfig::default();
        cfg.last_picked
            .insert("invent_a_new_task".into(), "some_model".into());
        cfg.last_picked
            .insert("detect_objects".into(), "rf_detr_seg_medium_v1".into());
        let map = cfg.into_map();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get(&AiTask::DetectObjects).map(String::as_str),
            Some("rf_detr_seg_medium_v1")
        );
    }

    #[test]
    fn default_config_has_empty_picks() {
        let cfg = AiToolsConfig::default();
        assert!(cfg.last_picked.is_empty());
        let map = cfg.into_map();
        assert!(map.is_empty());
    }

    #[test]
    fn missing_last_picked_section_parses_as_empty() {
        // Parsing an empty TOML body should yield default — the
        // load path relies on this when the file exists but is
        // truncated / mid-write.
        let cfg: AiToolsConfig = toml::from_str("").expect("empty body parses");
        assert!(cfg.last_picked.is_empty());
    }

    #[test]
    fn round_trip_through_toml_string() {
        let mut picks = HashMap::new();
        picks.insert(
            AiTask::IsolateObject,
            "mobile_sam_v1".to_string(),
        );
        let cfg = AiToolsConfig::from_map(&picks);
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: AiToolsConfig = toml::from_str(&body).expect("deserialize");
        let restored = parsed.into_map();
        assert_eq!(restored, picks);
    }
}
