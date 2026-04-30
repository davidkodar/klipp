//! Persistence for the per-user "disabled audio plugins" set.
//!
//! Stored as a tiny TOML file at `~/.config/klipp/audio_plugins.toml`.
//! Schema:
//!
//! ```toml
//! disabled_unique_ids = [12345, 67890]
//! ```
//!
//! Keying on LADSPA `unique_id` (a u64 baked into the plugin
//! descriptor) rather than path so a plugin moved between
//! distribution layouts (e.g. /usr/lib64/ladspa → /usr/lib/ladspa)
//! stays disabled. Stable across plugin version bumps too.
//!
//! Read at startup, written on every toggle. Failures here are
//! non-fatal: disable state simply doesn't persist if the config
//! dir isn't writeable, but the in-memory state still works for
//! the current session.

use std::collections::HashSet;
use std::path::PathBuf;

const FILE_NAME: &str = "audio_plugins.toml";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    disabled_unique_ids: Vec<u64>,
}

fn config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?.join("klipp");
    Some(base.join(FILE_NAME))
}

/// Read the disabled set from disk. Returns an empty set when the
/// file is missing, unreadable, or malformed — the user just sees
/// "no plugins disabled" rather than a startup error.
pub fn load() -> HashSet<u64> {
    let Some(path) = config_path() else {
        return HashSet::new();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    match toml::from_str::<OnDisk>(&data) {
        Ok(parsed) => parsed.disabled_unique_ids.into_iter().collect(),
        Err(e) => {
            tracing::warn!(
                "audio_plugins.toml parse failed at {}: {e} — treating as empty",
                path.display()
            );
            HashSet::new()
        }
    }
}

/// Write the disabled set to disk. Creates the parent directory
/// if needed. Failures are logged but never propagated — a save
/// failure shouldn't block the user from clicking other things.
pub fn save(disabled: &HashSet<u64>) {
    let Some(path) = config_path() else {
        tracing::warn!("audio_plugins.toml: no config dir, skipping save");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("audio_plugins.toml: failed to mkdir {}: {e}", parent.display());
            return;
        }
    }
    let mut ids: Vec<u64> = disabled.iter().copied().collect();
    ids.sort_unstable(); // deterministic output for diff-friendliness
    let on_disk = OnDisk { disabled_unique_ids: ids };
    let serialised = match toml::to_string(&on_disk) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("audio_plugins.toml: serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, serialised) {
        tracing::warn!(
            "audio_plugins.toml: write to {} failed: {e}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ondisk_roundtrips_empty() {
        let serialised = toml::to_string(&OnDisk::default()).unwrap();
        let parsed: OnDisk = toml::from_str(&serialised).unwrap();
        assert!(parsed.disabled_unique_ids.is_empty());
    }

    #[test]
    fn ondisk_roundtrips_ids() {
        let on_disk = OnDisk {
            disabled_unique_ids: vec![1, 42, 999_999],
        };
        let s = toml::to_string(&on_disk).unwrap();
        let parsed: OnDisk = toml::from_str(&s).unwrap();
        assert_eq!(parsed.disabled_unique_ids, vec![1, 42, 999_999]);
    }

    #[test]
    fn parses_file_with_unknown_fields() {
        // Forward-compat: a future Klipp could add fields here. The
        // load path must be robust to that.
        let s = "disabled_unique_ids = [10, 20]\nfuture_field = \"whatever\"\n";
        let parsed: OnDisk = toml::from_str(s).unwrap();
        assert_eq!(parsed.disabled_unique_ids, vec![10, 20]);
    }
}
