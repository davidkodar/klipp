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
pub const BUILTIN_HIGH_CONTRAST: &str = include_str!("builtin/high_contrast.toml");
pub const BUILTIN_CUT: &str = include_str!("builtin/cut.toml");

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
    /// The live Omarchy system theme (`theme/omarchy.rs`). One entry,
    /// present only when Omarchy is detected, replaced in place on
    /// every switch. Selected through
    /// `AppearancePrefs::follow_system_theme`, not the picker.
    Omarchy,
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
    /// BTreeMap so the iteration order in the picker is alphabetical +
    /// stable across runs (matters for the keyboard-driven cmd-K
    /// fuzzy picker we'll likely add later).
    entries: BTreeMap<ThemeId, RegistryEntry>,
    /// See [`ThemeRegistry::omarchy_name`].
    omarchy_name: Option<String>,
    /// The desktop portal's appearance settings: the fallback for
    /// following the system theme (`theme/portal.rs`).
    portal: super::portal::PortalStatus,
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
        // Before the user scan so a user theme called "Omarchy" gets
        // the usual "(user)" suffix instead of shadowing the live one.
        reg.reload_omarchy();
        reg.scan_user_themes();
        reg
    }

    /// Re-read the Omarchy system palette and replace (or add) its
    /// entry. Returns the Omarchy theme's display name when it
    /// loaded; on any failure the previous entry is kept so the UI
    /// never shows a half-swapped palette.
    pub fn reload_omarchy(&mut self) -> Option<String> {
        let state = super::omarchy::detect()?;
        let colors = super::omarchy::load(&state)?;
        let name = state.display_name();
        self.entries.insert(
            super::omarchy::THEME_ID.to_string(),
            RegistryEntry {
                theme: colors.into(),
                source: ThemeSource::Omarchy,
                path: Some(state.colors_path()),
            },
        );
        self.omarchy_name = Some(name.clone());
        Some(name)
    }

    /// Display name of the Omarchy theme currently loaded, e.g.
    /// "Tokyo Night". `None` when Omarchy was not detected.
    pub fn omarchy_name(&self) -> Option<&str> {
        self.omarchy_name.as_deref()
    }

    /// The built-in themes alone: no system palette and no user themes,
    /// whatever this machine has.
    #[cfg(test)]
    pub(crate) fn builtins_only() -> Self {
        let mut reg = Self::default();
        reg.load_builtins();
        reg
    }

    /// Take in the desktop portal's latest answer.
    pub fn set_portal(&mut self, status: super::portal::PortalStatus) {
        self.portal = status;
    }

    /// The desktop portal's latest answer.
    pub fn portal(&self) -> super::portal::PortalStatus {
        self.portal
    }

    /// The theme the desktop portal asks for: the built-in Dark or
    /// Light, in the desktop's accent when it sets one. `None` when it
    /// has no dark or light preference, or there is no portal.
    pub fn portal_theme(&self) -> Option<KlippTheme> {
        use super::portal::{ColorScheme, PortalStatus};
        let PortalStatus::Found(appearance) = self.portal else { return None };
        let (name, light) = match appearance.scheme {
            ColorScheme::Dark => ("Dark", false),
            ColorScheme::Light => ("Light", true),
            ColorScheme::NoPreference => return None,
        };
        let mut theme = self.get(name)?.clone();
        if let Some(accent) = appearance.accent {
            theme.set_accent(accent, light);
        }
        Some(theme)
    }

    /// The desktop's accent colour, when the portal gives one.
    pub fn portal_accent(&self) -> Option<egui::Color32> {
        match self.portal {
            super::portal::PortalStatus::Found(appearance) => appearance.accent,
            _ => None,
        }
    }

    /// Whether following the system theme changes anything here: a full
    /// palette, or a portal with a dark or light preference.
    pub fn has_system_theme(&self) -> bool {
        self.omarchy_name.is_some() || self.portal_theme().is_some()
    }

    /// The theme following the system gives, by name: Omarchy's, or the
    /// built-in Dark or Light the desktop's preference picks. `None` when
    /// there is nothing to follow.
    pub fn followed_theme_name(&self) -> Option<String> {
        use super::portal::{ColorScheme, PortalStatus};
        if let Some(name) = &self.omarchy_name {
            return Some(name.clone());
        }
        match self.portal {
            PortalStatus::Found(a) => match a.scheme {
                ColorScheme::Dark => Some("Dark".into()),
                ColorScheme::Light => Some("Light".into()),
                ColorScheme::NoPreference => None,
            },
            _ => None,
        }
    }

    /// What following the system theme follows, in a sentence for the
    /// setting's subtitle; `following`, whether the setting is on. Off,
    /// it says what the desktop is set to and that the theme below is
    /// used: it said "Following your desktop" while it wasn't.
    pub fn system_theme_summary(&self, following: bool) -> String {
        use super::portal::{ColorScheme, PortalStatus};
        if let Some(name) = &self.omarchy_name {
            return if following {
                format!("Omarchy: {name}. Switching the theme in Omarchy changes Klipp live.")
            } else {
                format!("Omarchy: {name}. Off, so the theme below is used.")
            };
        }
        match self.portal {
            PortalStatus::Looking => "Looking for your desktop\u{2019}s appearance settings\u{2026}".to_string(),
            PortalStatus::Absent => "No desktop appearance settings found, so the theme below is used.".to_string(),
            PortalStatus::Found(a) => {
                let mode = match a.scheme {
                    ColorScheme::Dark => "dark",
                    ColorScheme::Light => "light",
                    ColorScheme::NoPreference => return "Your desktop sets no dark or light preference, so the theme below is used.".to_string(),
                };
                let accent = a.accent.map(|c| format!(", {} accent", super::portal::colour_name(c))).unwrap_or_default();
                if following {
                    format!("Following your desktop: {mode}{accent}. Changes there apply here live.")
                } else {
                    format!("Your desktop is set to {mode}{accent}. Off, so the theme below is used.")
                }
            }
        }
    }

    fn load_builtins(&mut self) {
        for raw in [
            BUILTIN_DARK,
            BUILTIN_CUT,
            BUILTIN_LIGHT,
            BUILTIN_SOLARIZED_DARK,
            BUILTIN_CREAM,
            BUILTIN_SLATE,
            BUILTIN_NORD,
            BUILTIN_HIGH_CONTRAST,
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

#[cfg(test)]
mod portal_tests {
    use super::super::portal::{Appearance, ColorScheme, PortalStatus};
    use super::*;
    use egui::Color32;

    const BLUE: Color32 = Color32::from_rgb(53, 132, 228);

    /// The accent Light shows for a desktop's: darkened to read on it.
    fn readable_on(light: &KlippTheme, accent: Color32) -> Color32 {
        let mut t = light.clone();
        t.set_accent(accent, true);
        assert_ne!(t.accent, accent, "this blue is too light to be text on Light as it is");
        t.accent
    }

    fn found(scheme: ColorScheme, accent: Option<Color32>) -> PortalStatus {
        PortalStatus::Found(Appearance { scheme, accent })
    }

    fn following(picked: &str) -> crate::prefs::AppearancePrefs {
        crate::prefs::AppearancePrefs { follow_system_theme: true, theme_name: picked.into(), ..Default::default() }
    }

    /// #2625: prefer dark gives the built-in Dark, prefer light the
    /// built-in Light, each in the desktop's accent when it sets one.
    #[test]
    fn a_dark_or_light_preference_picks_that_theme_in_the_accent() {
        let mut reg = ThemeRegistry::builtins_only();
        let (dark, light) = (reg.get("Dark").expect("Dark").clone(), reg.get("Light").expect("Light").clone());
        reg.set_portal(found(ColorScheme::Dark, Some(BLUE)));
        let t = KlippTheme::from_prefs(&following("Solarized Dark"), &reg);
        assert_eq!((t.bg_panel, t.accent), (dark.bg_panel, BLUE));
        assert_eq!(t.text_on_accent, Color32::from_rgb(16, 16, 20), "dark text reads better on this mid blue: 5.4:1 against white's 3.9:1");
        reg.set_portal(found(ColorScheme::Dark, Some(Color32::from_rgb(20, 40, 120))));
        assert_eq!(KlippTheme::from_prefs(&following("Solarized Dark"), &reg).text_on_accent, Color32::WHITE, "and white on a deep one");
        reg.set_portal(found(ColorScheme::Light, None));
        let t = KlippTheme::from_prefs(&following("Solarized Dark"), &reg);
        assert_eq!((t.bg_panel, t.accent), (light.bg_panel, light.accent), "no accent set: the theme's own");
    }

    /// A fresh install starts Dark in the desktop's accent, even where the
    /// desktop prefers light (Breeze's default on KDE); light only when
    /// the person turns following dark and light on.
    #[test]
    fn a_fresh_install_starts_dark_in_the_desktops_accent() {
        let mut reg = ThemeRegistry::builtins_only();
        let (dark, light) = (reg.get("Dark").expect("Dark").clone(), reg.get("Light").expect("Light").clone());
        reg.set_portal(found(ColorScheme::Light, Some(BLUE)));
        let fresh = crate::prefs::Preferences::first_run(false).appearance;
        let t = KlippTheme::from_prefs(&fresh, &reg);
        assert_eq!((t.bg_panel, t.accent), (dark.bg_panel, BLUE), "dark, in the accent");
        let on = crate::prefs::AppearancePrefs { follow_system_theme: true, ..fresh.clone() };
        let t = KlippTheme::from_prefs(&on, &reg);
        // On Light the accent is darkened in its hue until it reads as text there (#3936).
        assert_eq!((t.bg_panel, t.accent), (light.bg_panel, readable_on(&light, BLUE)), "following dark and light: light");
        let own = crate::prefs::AppearancePrefs { follow_system_accent: false, ..fresh };
        assert_eq!(KlippTheme::from_prefs(&own, &reg).accent, dark.accent, "the accent not followed: the theme's");
    }

    /// An existing preferences file keeps what it followed: on, the
    /// desktop's dark or light; off, its own theme and accent, the accent
    /// switch not added to it.
    #[test]
    fn an_old_preferences_file_keeps_what_it_followed() {
        let mut reg = ThemeRegistry::builtins_only();
        let (dark, light) = (reg.get("Dark").expect("Dark").clone(), reg.get("Light").expect("Light").clone());
        reg.set_portal(found(ColorScheme::Light, Some(BLUE)));
        // A file saved before the accent switch existed: no such field.
        let old = |follow: bool| {
            let mut file = serde_json::to_value(crate::prefs::Preferences::default()).expect("prefs serialize");
            let appearance = file["appearance"].as_object_mut().expect("appearance");
            appearance.remove("follow_system_accent");
            appearance.insert("follow_system_theme".into(), follow.into());
            crate::prefs::Preferences::from_json_str(&file.to_string()).appearance
        };
        let t = KlippTheme::from_prefs(&old(true), &reg);
        assert_eq!((t.bg_panel, t.accent), (light.bg_panel, readable_on(&light, BLUE)));
        let t = KlippTheme::from_prefs(&old(false), &reg);
        assert!(!old(false).follow_system_accent);
        assert_eq!((t.bg_panel, t.accent), (dark.bg_panel, dark.accent));
    }

    /// Omarchy is followed whole, its own palette and accent, as before.
    #[test]
    fn omarchy_is_followed_whole() {
        let mut reg = ThemeRegistry::builtins_only();
        let palette = reg.get("Solarized Dark").expect("a palette").clone();
        reg.entries.insert(super::super::omarchy::THEME_ID.to_string(), RegistryEntry { theme: palette.clone(), source: ThemeSource::Omarchy, path: None });
        reg.omarchy_name = Some("Solarized".into());
        reg.set_portal(found(ColorScheme::Light, Some(BLUE)));
        let prefs = crate::prefs::Preferences::first_run(true).appearance;
        assert!(prefs.follow_system_theme, "a fresh install on Omarchy follows it");
        let t = KlippTheme::from_prefs(&prefs, &reg);
        assert_eq!((t.bg_panel, t.accent), (palette.bg_panel, palette.accent));
    }

    /// No preference, no portal, an answer not in yet, or following
    /// switched off: the theme the user picked.
    #[test]
    fn otherwise_the_picked_theme_stays() {
        let mut reg = ThemeRegistry::builtins_only();
        let picked = reg.get("Solarized Dark").expect("a built-in to pick").clone();
        for status in [found(ColorScheme::NoPreference, Some(BLUE)), PortalStatus::Absent, PortalStatus::Looking] {
            reg.set_portal(status);
            assert_eq!(KlippTheme::from_prefs(&following("Solarized Dark"), &reg).bg_panel, picked.bg_panel, "{status:?}");
            assert!(!reg.has_system_theme(), "{status:?}");
        }
        reg.set_portal(found(ColorScheme::Light, None));
        let off = crate::prefs::AppearancePrefs { follow_system_theme: false, ..following("Solarized Dark") };
        assert_eq!(KlippTheme::from_prefs(&off, &reg).bg_panel, picked.bg_panel, "not following");
    }

    /// A SettingChanged switches the theme while the app runs.
    #[test]
    fn a_live_change_switches_the_theme() {
        let mut reg = ThemeRegistry::builtins_only();
        let light = reg.get("Light").expect("Light").bg_panel;
        let mut now = Appearance { scheme: ColorScheme::Dark, accent: None };
        reg.set_portal(PortalStatus::Found(now));
        assert_ne!(KlippTheme::from_prefs(&following("Dark"), &reg).bg_panel, light);
        assert!(now.apply("org.freedesktop.appearance", "color-scheme", &zbus::zvariant::Value::U32(2)));
        reg.set_portal(PortalStatus::Found(now));
        assert_eq!(KlippTheme::from_prefs(&following("Dark"), &reg).bg_panel, light);
    }

    /// The setting's subtitle says what is followed, or that nothing is;
    /// switched off, that it is not followed.
    #[test]
    fn the_subtitle_says_what_is_followed() {
        let mut reg = ThemeRegistry::builtins_only();
        reg.set_portal(found(ColorScheme::Dark, Some(BLUE)));
        assert_eq!(reg.system_theme_summary(true), "Following your desktop: dark, blue accent. Changes there apply here live.");
        assert_eq!(reg.system_theme_summary(false), "Your desktop is set to dark, blue accent. Off, so the theme below is used.");
        assert_eq!(reg.followed_theme_name().as_deref(), Some("Dark"));
        reg.set_portal(found(ColorScheme::Light, None));
        assert_eq!(reg.system_theme_summary(true), "Following your desktop: light. Changes there apply here live.");
        assert!(!reg.system_theme_summary(false).contains("Following"), "off, it does not say it follows");
        assert_eq!(reg.followed_theme_name().as_deref(), Some("Light"));
        reg.set_portal(found(ColorScheme::NoPreference, None));
        for on in [false, true] {
            assert!(reg.system_theme_summary(on).starts_with("Your desktop sets no dark or light preference"));
        }
        assert_eq!(reg.followed_theme_name(), None);
        reg.set_portal(PortalStatus::Absent);
        assert!(reg.system_theme_summary(true).starts_with("No desktop appearance settings found"));
        assert_eq!(reg.followed_theme_name(), None);
    }
}
