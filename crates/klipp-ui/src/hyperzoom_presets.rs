//! Hyperzoom preset registry — save the *tuned* hyperzoom config
//! (without focal point or target choice) as a named JSON file in
//! `~/.config/klipp/hyperzoom_presets/`, load it back to populate
//! the inspector's scratch slot for any clip.
//!
//! # Why a separate module from [`effect_presets`]
//!
//! `EffectPreset` is shaped around video-effect parameters
//! (`effect_key + Option<BuiltinEffect> + ofx_params: HashMap`).
//! Hyperzoom presets carry 11 typed fields; routing them through
//! `ofx_params` would lose type safety and add a parsing layer at
//! load. Sibling module mirrors the architecture without
//! duplicating the core save/load code paths — both targets are
//! small enough that the redundancy is the right call.
//!
//! # Field exclusions — by type, not by serde attribute
//!
//! [`HyperzoomPreset`] is a strict subset of [`HyperzoomScratch`]:
//! `focal_x` / `focal_y` / `target_choice` are NOT present at all.
//! Future scratch additions can't accidentally land in saved files
//! because the include/exclude decision is a type-level fact.
//! [`HyperzoomPreset::from_scratch`] is the build constructor;
//! [`HyperzoomPreset::apply_to_scratch`] is the load operation —
//! it overwrites only the preset-worthy fields and leaves
//! `focal_x` / `focal_y` / `target_choice` untouched.
//!
//! # On-disk format
//!
//! One JSON file per preset at `~/.config/klipp/hyperzoom_presets/{sanitised_name}.json`.
//! Mirrors the themes (`~/.config/klipp/themes/*.toml`), effect
//! presets (`~/.config/klipp/effect_presets/{key}/*.json`), and
//! color presets directories. Each file carries a `version: u32`
//! field for future migration; unknown-newer-version files are
//! skipped at load with a warning (NOT an error — old binaries
//! shouldn't choke on files saved by newer ones).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hyperzoom::{HyperzoomEasing, HyperzoomScratch};

/// Wire-format version. v1 is the initial release. Bump when the
/// JSON shape changes; the loader skips files with version > the
/// currently-known max so old binaries don't choke on newer saves.
pub const HYPERZOOM_PRESET_VERSION: u32 = 1;

/// A saved hyperzoom configuration — strict subset of
/// [`HyperzoomScratch`]. Per-cut fields (`focal_x`, `focal_y`,
/// `target_choice`) are explicitly absent so loading a preset on
/// a different clip doesn't stomp on focal-point picks. The
/// custom-whoosh `PathBuf` is also excluded — it lives in a
/// separate egui-Memory slot keyed by clip-id, never on
/// `HyperzoomScratch`, so it's already decoupled from the save
/// path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HyperzoomPreset {
    /// Wire-format version. Currently `1`. Loaders that don't
    /// understand the version skip the file with a warning rather
    /// than erroring out.
    #[serde(default = "default_version")]
    pub version: u32,
    /// User-facing preset name. Doubles as the filename stem
    /// (after sanitisation).
    pub name: String,
    pub duration_secs: f64,
    pub max_scale: f32,
    /// Easing wire-format tag: 0 = Linear, 1 = Bezier. Round-trips
    /// through [`HyperzoomEasing::as_u8`] / [`from_u8`].
    pub easing: u8,
    pub hold_secs: f64,
    pub add_radial_blur: bool,
    pub radial_blur_amount: f32,
    pub add_chromatic_aberration: bool,
    pub chromatic_aberration_amount: f32,
    pub add_light_leak: bool,
    pub light_leak_intensity: f32,
    pub whoosh_enabled: bool,
    /// Whoosh variant tag: 0=Short, 1=Medium, 2=Long, 255=Custom.
    /// When 255, the actual custom file path is per-cut and lives
    /// in a separate egui-Memory slot — not part of the preset.
    pub whoosh_variant: u8,
}

fn default_version() -> u32 {
    HYPERZOOM_PRESET_VERSION
}

impl HyperzoomPreset {
    /// Build a preset from the inspector's current scratch state.
    /// Excludes `focal_x` / `focal_y` / `target_choice` by type —
    /// the saved JSON will not contain those fields at all.
    pub fn from_scratch(name: String, s: &HyperzoomScratch) -> Self {
        Self {
            version: HYPERZOOM_PRESET_VERSION,
            name,
            duration_secs: s.duration_secs,
            max_scale: s.max_scale,
            easing: s.easing.as_u8(),
            hold_secs: s.hold_secs,
            add_radial_blur: s.add_radial_blur,
            radial_blur_amount: s.radial_blur_amount,
            add_chromatic_aberration: s.add_chromatic_aberration,
            chromatic_aberration_amount: s.chromatic_aberration_amount,
            add_light_leak: s.add_light_leak,
            light_leak_intensity: s.light_leak_intensity,
            whoosh_enabled: s.whoosh_enabled,
            whoosh_variant: s.whoosh_variant,
        }
    }

    /// Apply this preset to a scratch state. Overwrites only the
    /// preset-worthy fields; `focal_x` / `focal_y` /
    /// `target_choice` survive untouched. Pinned by
    /// `apply_to_scratch_preserves_existing_focal` and
    /// `apply_to_scratch_preserves_existing_target_choice` —
    /// regression here would silently stomp on the user's
    /// per-cut work.
    pub fn apply_to_scratch(&self, s: &mut HyperzoomScratch) {
        s.duration_secs = self.duration_secs;
        s.max_scale = self.max_scale;
        s.easing = HyperzoomEasing::from_u8(self.easing);
        s.hold_secs = self.hold_secs;
        s.add_radial_blur = self.add_radial_blur;
        s.radial_blur_amount = self.radial_blur_amount;
        s.add_chromatic_aberration = self.add_chromatic_aberration;
        s.chromatic_aberration_amount = self.chromatic_aberration_amount;
        s.add_light_leak = self.add_light_leak;
        s.light_leak_intensity = self.light_leak_intensity;
        s.whoosh_enabled = self.whoosh_enabled;
        s.whoosh_variant = self.whoosh_variant;
    }
}

/// Failure modes for [`sanitise_preset_filename`]. Empty /
/// whitespace-only names reject with an error — silently
/// corrupting a save with `_.json` would lose data on the next
/// rename of a real preset to the same sanitised stem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetNameError {
    /// User submitted an empty string.
    Empty,
    /// User submitted whitespace only / nothing left after
    /// trimming and sanitising.
    OnlyDisallowedChars,
}

impl std::fmt::Display for PresetNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("preset name is empty"),
            Self::OnlyDisallowedChars => f.write_str(
                "preset name contains no usable characters \
                 (allowed: letters, digits, space, '-', '_')",
            ),
        }
    }
}

impl std::error::Error for PresetNameError {}

/// Sanitise a user-supplied preset name into a safe filename
/// stem. Rejects empty / whitespace-only / all-disallowed-chars
/// inputs with an error — never silently produces an empty or
/// `_`-only stem (which would collide with itself across
/// distinct user inputs and effectively delete the previous
/// save).
///
/// Allowed: ASCII letters, digits, space, `-`, `_`. Trims
/// leading/trailing whitespace, replaces interior runs of
/// disallowed chars with `_`, collapses repeated `_` to a
/// single one, caps length at 64 chars (filesystem-friendly).
pub fn sanitise_preset_filename(name: &str) -> Result<String, PresetNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(PresetNameError::Empty);
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_underscore = false;
    for c in trimmed.chars() {
        let safe = c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_';
        if safe {
            out.push(c);
            last_was_underscore = c == '_';
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }
    // Collapsed runs of '_' may still leave the result as "___"
    // when the original was all-disallowed. Reject that.
    let any_keepable = out.chars().any(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-');
    if !any_keepable {
        return Err(PresetNameError::OnlyDisallowedChars);
    }
    // Cap length. 64 is generous for this surface; longer names
    // hit awkward filesystem path-length limits on some Linux
    // configs.
    if out.len() > 64 {
        out.truncate(64);
    }
    Ok(out)
}

/// In-memory + on-disk registry of saved hyperzoom presets.
/// Mirrors [`crate::effect_presets::PresetManager`] structurally.
/// Loaded once at app startup; mutations (save / delete) refresh
/// the in-memory list synchronously alongside the disk write.
pub struct HyperzoomPresetRegistry {
    presets: Vec<HyperzoomPreset>,
    presets_dir: PathBuf,
}

impl Default for HyperzoomPresetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperzoomPresetRegistry {
    /// Build a new registry pointing at the canonical user-config
    /// directory. Scans the directory at construction time;
    /// missing or unreadable files are skipped with a warning.
    pub fn new() -> Self {
        let presets_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("klipp")
            .join("hyperzoom_presets");
        let mut r = Self {
            presets: Vec::new(),
            presets_dir,
        };
        r.refresh_from_disk();
        r
    }

    /// Construct a registry rooted at an explicit directory.
    /// Test-only path — production calls always go through
    /// [`Self::new`].
    #[cfg(test)]
    pub fn for_test(presets_dir: PathBuf) -> Self {
        let mut r = Self {
            presets: Vec::new(),
            presets_dir,
        };
        r.refresh_from_disk();
        r
    }

    /// All presets currently in the registry. Order is the
    /// directory's iteration order — callers that need a stable
    /// alphabetical order should sort the returned slice.
    pub fn presets(&self) -> &[HyperzoomPreset] {
        &self.presets
    }

    /// Path the registry is reading from / writing to.
    /// Surfaced so the inspector can show a "manage these on
    /// disk" tooltip pointing at the directory (no in-app
    /// delete UI in v1).
    pub fn presets_dir(&self) -> &PathBuf {
        &self.presets_dir
    }

    /// Save a preset to disk + refresh the in-memory list.
    /// Overwrites any existing preset with the same sanitised
    /// filename stem (the inspector's save dialog should warn
    /// the user before reaching this).
    pub fn save(&mut self, preset: HyperzoomPreset) -> Result<(), SaveError> {
        let stem = sanitise_preset_filename(&preset.name).map_err(SaveError::BadName)?;
        if let Err(e) = fs::create_dir_all(&self.presets_dir) {
            return Err(SaveError::Io(e));
        }
        let path = self.presets_dir.join(format!("{stem}.json"));
        let json = serde_json::to_string_pretty(&preset).map_err(SaveError::Serialize)?;
        fs::write(&path, json).map_err(SaveError::Io)?;
        // Replace any existing in-memory entry with the same
        // sanitised stem — keeps the registry in sync with the
        // file system after an overwrite.
        self.presets.retain(|p| {
            sanitise_preset_filename(&p.name)
                .map(|s| s != stem)
                .unwrap_or(true)
        });
        self.presets.push(preset);
        Ok(())
    }

    /// Remove a preset from disk + memory by name. No-op if
    /// the preset doesn't exist (so callers can be lazy about
    /// pre-checking).
    pub fn delete(&mut self, name: &str) -> Result<(), PresetNameError> {
        let stem = sanitise_preset_filename(name)?;
        let path = self.presets_dir.join(format!("{stem}.json"));
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("failed to delete hyperzoom preset {path:?}: {e}");
            }
        }
        self.presets.retain(|p| {
            sanitise_preset_filename(&p.name)
                .map(|s| s != stem)
                .unwrap_or(true)
        });
        Ok(())
    }

    /// Re-scan the presets directory + rebuild the in-memory
    /// list. Tolerant of per-file failures: corrupted / wrong-
    /// version files log a warning and get skipped, the rest
    /// load. Public so the inspector can refresh after the
    /// user adds files manually outside the app.
    pub fn refresh_from_disk(&mut self) {
        self.presets.clear();
        let Ok(entries) = fs::read_dir(&self.presets_dir) else {
            // Directory doesn't exist yet — first run. Empty
            // registry is the correct state.
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let json = match fs::read_to_string(&path) {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!("hyperzoom_presets: read failed {path:?}: {e}");
                    continue;
                }
            };
            let preset: HyperzoomPreset = match serde_json::from_str(&json) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("hyperzoom_presets: parse failed {path:?}: {e}");
                    continue;
                }
            };
            if preset.version > HYPERZOOM_PRESET_VERSION {
                // Unknown-newer-version policy: skip with warning,
                // not error. Old binaries shouldn't choke on
                // saves from newer ones; user can update Klipp
                // to read them.
                tracing::warn!(
                    "hyperzoom_presets: skipping {path:?} (version {} > {})",
                    preset.version, HYPERZOOM_PRESET_VERSION,
                );
                continue;
            }
            self.presets.push(preset);
        }
    }
}

/// Failure modes for [`HyperzoomPresetRegistry::save`].
#[derive(Debug)]
pub enum SaveError {
    BadName(PresetNameError),
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName(e) => write!(f, "bad preset name: {e}"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Serialize(e) => write!(f, "serialise error: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperzoom::HyperzoomTargetChoice;
    use klipp_core::ClipId;

    fn tweaked_scratch() -> HyperzoomScratch {
        let mut s = HyperzoomScratch::default();
        s.focal_x = 0.3;
        s.focal_y = 0.7;
        s.duration_secs = 0.42;
        s.max_scale = 75.0;
        s.target_choice = HyperzoomTargetChoice::Explicit(ClipId::new());
        s.easing = HyperzoomEasing::Bezier;
        s.hold_secs = 0.12;
        s.add_radial_blur = true;
        s.radial_blur_amount = 22.5;
        s.add_chromatic_aberration = true;
        s.chromatic_aberration_amount = 9.0;
        s.add_light_leak = true;
        s.light_leak_intensity = 0.55;
        s.whoosh_enabled = true;
        s.whoosh_variant = 1; // Medium
        s
    }

    fn tmp_dir(stem: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("klipp-hyperzoom-presets-{}-{}", stem, uuid_like()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Cheap unique stem so parallel cargo-test workers don't
    /// stomp on each other in /tmp.
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{nanos:x}-{:x}", std::process::id())
    }

    // ────────── from_scratch / apply_to_scratch ──────────

    #[test]
    fn from_scratch_excludes_focal_and_target() {
        // Pin: a serialised preset must contain none of the
        // per-cut fields. The exclusion is type-level — the
        // struct doesn't even have those fields — but the
        // round-trip-through-JSON test confirms the wire shape
        // doesn't leak them via some derived attribute.
        let s = tweaked_scratch();
        let p = HyperzoomPreset::from_scratch("test".into(), &s);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("focal_x"), "json: {json}");
        assert!(!json.contains("focal_y"), "json: {json}");
        assert!(!json.contains("target_choice"), "json: {json}");
    }

    #[test]
    fn apply_to_scratch_overwrites_preset_fields_only() {
        // Pin: every preset-worthy field on a fresh-default
        // scratch gets overwritten; the per-cut fields stay at
        // their default values.
        let p = HyperzoomPreset::from_scratch("test".into(), &tweaked_scratch());
        let mut s = HyperzoomScratch::default();
        // Capture per-cut fields before apply.
        let fx = s.focal_x;
        let fy = s.focal_y;
        let tgt = s.target_choice;
        p.apply_to_scratch(&mut s);
        // Per-cut survive
        assert_eq!(s.focal_x, fx);
        assert_eq!(s.focal_y, fy);
        assert_eq!(s.target_choice, tgt);
        // Preset-worthy overwritten
        assert_eq!(s.duration_secs, 0.42);
        assert_eq!(s.max_scale, 75.0);
        assert_eq!(s.easing, HyperzoomEasing::Bezier);
        assert_eq!(s.hold_secs, 0.12);
        assert!(s.add_radial_blur);
        assert_eq!(s.radial_blur_amount, 22.5);
        assert!(s.add_chromatic_aberration);
        assert_eq!(s.chromatic_aberration_amount, 9.0);
        assert!(s.add_light_leak);
        assert!((s.light_leak_intensity - 0.55).abs() < 1e-6);
        assert!(s.whoosh_enabled);
        assert_eq!(s.whoosh_variant, 1);
    }

    #[test]
    fn apply_to_scratch_preserves_existing_focal() {
        // Audit (i): the additive contract — a load operation
        // must not stomp on a focal point the user has already
        // tuned for this clip. Different clip might want the
        // same look applied to a different region of frame.
        let p = HyperzoomPreset::from_scratch("test".into(), &tweaked_scratch());
        let mut s = HyperzoomScratch::default();
        s.focal_x = 0.18;
        s.focal_y = 0.82;
        p.apply_to_scratch(&mut s);
        assert_eq!(s.focal_x, 0.18, "focal_x untouched by load");
        assert_eq!(s.focal_y, 0.82, "focal_y untouched by load");
    }

    #[test]
    fn apply_to_scratch_preserves_existing_target_choice() {
        // Audit (i): Explicit clip-id target picks must survive
        // a preset load. ClipId-bearing targets are inherently
        // per-cut (clip-ids don't cross projects); the load
        // must not collapse Explicit back to Auto.
        let p = HyperzoomPreset::from_scratch("test".into(), &tweaked_scratch());
        let mut s = HyperzoomScratch::default();
        let target_id = ClipId::new();
        s.target_choice = HyperzoomTargetChoice::Explicit(target_id);
        p.apply_to_scratch(&mut s);
        assert_eq!(
            s.target_choice,
            HyperzoomTargetChoice::Explicit(target_id),
            "target_choice untouched by load",
        );
    }

    // ────────── Round-trip through disk ──────────

    #[test]
    fn round_trip_through_disk_preserves_all_fields() {
        let dir = tmp_dir("round_trip");
        let mut reg = HyperzoomPresetRegistry::for_test(dir.clone());
        let original = HyperzoomPreset::from_scratch("My signature".into(), &tweaked_scratch());
        reg.save(original.clone()).expect("save");
        // Force a reload from disk.
        let mut reg2 = HyperzoomPresetRegistry::for_test(dir);
        let presets = reg2.presets();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0], original);
        // Apply round-trips identically.
        let mut s_a = HyperzoomScratch::default();
        let mut s_b = HyperzoomScratch::default();
        original.apply_to_scratch(&mut s_a);
        presets[0].apply_to_scratch(&mut s_b);
        assert_eq!(s_a.duration_secs, s_b.duration_secs);
        assert_eq!(s_a.max_scale, s_b.max_scale);
        assert_eq!(s_a.easing, s_b.easing);
        // Touch the registry to keep it alive
        let _ = reg2.presets();
    }

    #[test]
    fn save_overwrite_replaces_existing_entry_in_place() {
        let dir = tmp_dir("overwrite");
        let mut reg = HyperzoomPresetRegistry::for_test(dir);
        let mut s = tweaked_scratch();
        reg.save(HyperzoomPreset::from_scratch("Same name".into(), &s)).unwrap();
        s.max_scale = 99.0; // tweak
        reg.save(HyperzoomPreset::from_scratch("Same name".into(), &s)).unwrap();
        let presets = reg.presets();
        assert_eq!(presets.len(), 1, "overwrite, not append");
        assert_eq!(presets[0].max_scale, 99.0, "second save wins");
    }

    #[test]
    fn delete_removes_disk_file_and_in_memory_entry() {
        let dir = tmp_dir("delete");
        let mut reg = HyperzoomPresetRegistry::for_test(dir.clone());
        reg.save(HyperzoomPreset::from_scratch("Doomed".into(), &tweaked_scratch())).unwrap();
        let stem = sanitise_preset_filename("Doomed").unwrap();
        let path = dir.join(format!("{stem}.json"));
        assert!(path.exists());
        reg.delete("Doomed").unwrap();
        assert!(!path.exists(), "file removed from disk");
        assert_eq!(reg.presets().len(), 0, "removed from in-memory list");
    }

    // ────────── Filename sanitisation ──────────

    #[test]
    fn sanitised_filename_strips_path_chars() {
        // Slash, backslash, colon, question mark, asterisk,
        // angle brackets, pipe — the standard "filesystem hostile"
        // set across Linux + macOS + Windows.
        let stem = sanitise_preset_filename("evil/name:with*chars").unwrap();
        for c in ['/', ':', '*', '\\', '<', '>', '?', '|', '"'] {
            assert!(!stem.contains(c), "stem still has '{c}': {stem}");
        }
        // Some replacement happened, so we should have an
        // underscore in there.
        assert!(stem.contains('_'));
    }

    #[test]
    fn sanitised_filename_rejects_empty() {
        assert_eq!(sanitise_preset_filename(""), Err(PresetNameError::Empty));
    }

    #[test]
    fn sanitised_filename_rejects_whitespace_only() {
        // Audit (ii): all-whitespace input must reject — silently
        // turning "   " into "" or "_" loses the user's data on
        // the next save under the same blank.
        assert_eq!(sanitise_preset_filename("   "), Err(PresetNameError::Empty));
        assert_eq!(sanitise_preset_filename("\t\n  "), Err(PresetNameError::Empty));
    }

    #[test]
    fn sanitised_filename_rejects_all_disallowed() {
        // After sanitisation this would be all '_' / nothing —
        // reject explicitly.
        let err = sanitise_preset_filename("///").unwrap_err();
        assert_eq!(err, PresetNameError::OnlyDisallowedChars);
        let err = sanitise_preset_filename("@#$%").unwrap_err();
        assert_eq!(err, PresetNameError::OnlyDisallowedChars);
    }

    #[test]
    fn sanitised_filename_caps_length_at_64() {
        let very_long = "a".repeat(200);
        let stem = sanitise_preset_filename(&very_long).unwrap();
        assert_eq!(stem.len(), 64);
    }

    #[test]
    fn sanitised_filename_trims_leading_trailing_whitespace() {
        let stem = sanitise_preset_filename("  hello  ").unwrap();
        assert_eq!(stem, "hello");
    }

    // ────────── Registry tolerance ──────────

    #[test]
    fn load_skips_malformed_json_files() {
        let dir = tmp_dir("malformed");
        // Write one valid + one corrupted file directly to disk.
        let mut reg = HyperzoomPresetRegistry::for_test(dir.clone());
        reg.save(HyperzoomPreset::from_scratch("Good".into(), &tweaked_scratch())).unwrap();
        fs::write(dir.join("garbage.json"), b"{ not valid json").unwrap();
        // Re-scan the directory.
        let mut reg2 = HyperzoomPresetRegistry::for_test(dir);
        let presets = reg2.presets();
        assert_eq!(presets.len(), 1, "good preset loaded, garbage skipped");
        assert_eq!(presets[0].name, "Good");
        let _ = reg2.refresh_from_disk(); // exercise the public refresh path too
    }

    #[test]
    fn load_skips_unknown_newer_version_files() {
        // Write a JSON with version=99 — newer than we know.
        // Loader skips with warning, doesn't error.
        let dir = tmp_dir("future");
        let json = r#"{
            "version": 99,
            "name": "Future preset",
            "duration_secs": 0.5,
            "max_scale": 50.0,
            "easing": 0,
            "hold_secs": 0.0,
            "add_radial_blur": false,
            "radial_blur_amount": 0.0,
            "add_chromatic_aberration": false,
            "chromatic_aberration_amount": 0.0,
            "add_light_leak": false,
            "light_leak_intensity": 0.0,
            "whoosh_enabled": false,
            "whoosh_variant": 0
        }"#;
        fs::write(dir.join("future.json"), json).unwrap();
        let reg = HyperzoomPresetRegistry::for_test(dir);
        assert_eq!(reg.presets().len(), 0, "newer-version preset skipped");
    }

    #[test]
    fn version_field_present_in_saved_json() {
        let dir = tmp_dir("version_field");
        let mut reg = HyperzoomPresetRegistry::for_test(dir.clone());
        reg.save(HyperzoomPreset::from_scratch("V1".into(), &tweaked_scratch())).unwrap();
        let stem = sanitise_preset_filename("V1").unwrap();
        let raw = fs::read_to_string(dir.join(format!("{stem}.json"))).unwrap();
        assert!(raw.contains("\"version\""));
        assert!(raw.contains(&format!("\"version\": {}", HYPERZOOM_PRESET_VERSION)));
    }

    #[test]
    fn save_returns_error_on_empty_name() {
        let dir = tmp_dir("save_empty");
        let mut reg = HyperzoomPresetRegistry::for_test(dir);
        let result = reg.save(HyperzoomPreset::from_scratch("".into(), &tweaked_scratch()));
        assert!(matches!(result, Err(SaveError::BadName(PresetNameError::Empty))));
        assert_eq!(reg.presets().len(), 0, "no entry registered on bad-name save");
    }

    #[test]
    fn empty_directory_yields_empty_registry() {
        // Defensive: first-run / cleared-config case.
        let dir = tmp_dir("first_run");
        let reg = HyperzoomPresetRegistry::for_test(dir);
        assert_eq!(reg.presets().len(), 0);
    }
}
