//! Theme registry — discovers built-in palettes (compiled-in TOML)
//! and user-supplied palettes (`~/.config/klipp/themes/*.toml`),
//! exposes them by name to the rest of the app.
//!
//! # Adding a new theme
//!
//! Drop a TOML file into `~/.config/klipp/themes/`. Restart Klipp (or
//! click "Reload themes" in Preferences → Appearance — coming next).
//! The file's `[colors]` table fills [`super::ThemeColors`] field-for-
//! field; the file's `name` becomes the dropdown entry.
//!
//! TOML schema:
//! ```toml
//! name = "My Theme"          # display name shown in the picker
//!
//! [colors]
//! bg_void = "#0a0a0c"
//! bg_deep = "#11111
//! # … every field on ThemeColors must appear ( serde will reject
//! # the file if any are missing).
//! ```
//!
//! See `theme/builtin/dark.toml` and `light.toml` for full templates.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{KlippTheme, ThemeColors};

/// Compiled-in default Dark theme. Lives in `theme/builtin/dark.toml`
/// and is included at compile time so a fresh install works without
/// any files on disk. The `&str` constant is also re-exported from
/// `theme.rs` so the rest of the crate can reference the canonical
/// "Dark" name without depending on the registry.
pub const BUILTIN_DARK: &str = include_str!("builtin/dark.toml");
pub const BUILTIN_LIGHT: &str = include_str!("builtin/light.toml");
pub const BUILTIN_SOLARIZED_DARK: &str = include_str!("builtin/solarized_dark.toml");
pub const BUILTIN_CREAM: &str = include_str!("builtin/cream.toml");
pub const BUILTIN_SLATE: &str = include_str!("builtin/slate.toml");
pub const BUILTIN_NORD: &str = include_str!("builtin/nord.toml");

/// Default theme name used when [`crate::prefs::AppearancePrefs::theme_name`]
/// references a theme that the registry doesn't know about (e.g. a
/// user-supplied TOML was renamed or deleted between sessions).
pub const DEFAULT_NAME: &str = "Dark";

/// On-disk representation of a theme TOML file: top-level `name` plus
/// the `[colors]` table that fills [`ThemeColors`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThemeFile {
    name: String,
    colors: ThemeColors,
}

/// Identifier the registry uses to address a single theme. Currently
/// the same string the user sees in the dropdown; kept distinct so we
/// can add slug-vs-display separation later without churn.
pub type ThemeId = String;

/// Where a particular theme came from — affects the picker UI and
/// what the "Edit theme" affordance can do. Built-ins are read-only;
/// user files are editable in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSource {
    Builtin,
    User,
}

#[derive(Debug, Clone)]
struct RegistryEntry {
    theme: KlippTheme,
    source: ThemeSource,
    /// Path on disk for User themes; None for Builtin.
    path: Option<PathBuf>,
}

/// In-memory registry of all themes the app knows about. Built-ins
/// are loaded once at construction; user themes are scanned from
/// `~/.config/klipp/themes/*.toml`.
#[derive(Debug, Clone, Default)]
pub struct ThemeRegistry {
    /// BTreeMap so the iteration order in the picker is alphabetical
    /// + stable across runs (matters for the keyboard-driven cmd-K
    /// fuzzy picker we'll likely add later).
    entries: BTreeMap<ThemeId, RegistryEntry>,
}

impl ThemeRegistry {
    /// Build a registry preloaded with the built-in Dark + Light
    /// themes, then scan the user themes directory and add any
    /// `*.toml` files that parse cleanly. Files that fail to parse
    /// are skipped with a warning — the app keeps running on the
    /// built-ins.
    pub fn load() -> Self {
        let mut reg = Self::default();
        reg.load_builtins();
        reg.scan_user_themes();
        reg
    }

    fn load_builtins(&mut self) {
        for raw in [
            BUILTIN_DARK,
            BUILTIN_LIGHT,
            BUILTIN_SOLARIZED_DARK,
            BUILTIN_CREAM,
            BUILTIN_SLATE,
            BUILTIN_NORD,
        ] {
            match parse_theme_file(raw) {
                Ok(file) => {
                    let theme: KlippTheme = file.colors.into();
                    self.entries.insert(
                        file.name.clone(),
                        RegistryEntry {
                            theme,
                            source: ThemeSource::Builtin,
                            path: None,
                        },
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "ThemeRegistry: built-in theme TOML failed to parse: {e}. \
                         This is a code bug — built-ins ship inside the binary."
                    );
                }
            }
        }
    }

    fn scan_user_themes(&mut self) {
        let dir = match user_themes_dir() {
            Some(d) => d,
            None => return,
        };
        if !dir.exists() {
            // Auto-create so users have an obvious place to drop files.
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("ThemeRegistry: cannot create {dir:?}: {e}");
                return;
            }
        }
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                tracing::warn!("ThemeRegistry: read_dir({dir:?}) failed: {e}");
                return;
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("ThemeRegistry: read {path:?} failed: {e}");
                    continue;
                }
            };
            match parse_theme_file(&raw) {
                Ok(file) => {
                    let theme: KlippTheme = file.colors.into();
                    let name = if self.entries.contains_key(&file.name) {
                        // User file shadows a built-in name by adding
                        // a "(user)" suffix — keeps the built-in
                        // accessible and surfaces the conflict in the
                        // picker.
                        let suffixed = format!("{} (user)", file.name);
                        tracing::info!(
                            "ThemeRegistry: user theme name '{}' collides with built-in; loaded as '{}'",
                            file.name, suffixed,
                        );
                        suffixed
                    } else {
                        file.name.clone()
                    };
                    self.entries.insert(
                        name,
                        RegistryEntry {
                            theme,
                            source: ThemeSource::User,
                            path: Some(path),
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "ThemeRegistry: skipping {path:?} — TOML parse failed: {e}"
                    );
                }
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&KlippTheme> {
        self.entries.get(name).map(|e| &e.theme)
    }

    pub fn source(&self, name: &str) -> Option<ThemeSource> {
        self.entries.get(name).map(|e| e.source)
    }

    pub fn path(&self, name: &str) -> Option<&PathBuf> {
        self.entries.get(name).and_then(|e| e.path.as_ref())
    }

    /// Iterate (name, source) pairs in alphabetical order. Used by
    /// the Preferences dropdown to populate the picker.
    pub fn iter(&self) -> impl Iterator<Item = (&str, ThemeSource)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.source))
    }

    /// True iff at least one entry exists. False after a catastrophic
    /// load failure (built-in TOML rejected by serde — shouldn't
    /// happen in a release build but guards against it).
    pub fn is_populated(&self) -> bool {
        !self.entries.is_empty()
    }
}

/// Public so the `KlippTheme::Default` impl can fall back to parsing
/// the embedded Dark TOML when the registry isn't available (early
/// init, tests, etc.).
pub fn parse_builtin(raw: &str) -> Result<ThemeColors, String> {
    parse_theme_file(raw).map(|f| f.colors)
}

fn parse_theme_file(raw: &str) -> Result<ThemeFile, String> {
    toml::from_str::<ThemeFile>(raw).map_err(|e| e.to_string())
}

/// `~/.config/klipp/themes/`. Returns `None` if the platform has no
/// config dir (very rare).
fn user_themes_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("klipp").join("themes"))
}
