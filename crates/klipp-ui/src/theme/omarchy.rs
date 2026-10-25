//! Follow the Omarchy system theme.
//!
//! Omarchy (the Arch + Hyprland distribution) resolves every theme
//! switch into one directory, `$XDG_STATE_HOME/omarchy/current/`:
//!
//! ```text
//! current/
//! ├── theme/colors.toml   # the palette, merged and fully resolved
//! ├── theme.name          # slug, e.g. "tokyo-night"
//! └── background          # symlink to the wallpaper
//! ```
//!
//! `omarchy-theme-set` rebuilds `theme/` under a new inode
//! (`rm -rf theme && mv next-theme theme`) and writes `theme.name`
//! last, so [`start_watcher`] watches the *parent* directory and keys
//! on that file: a watch on `colors.toml` itself would die on the
//! first switch, and an event on `theme.name` means the palette is
//! complete.
//!
//! `colors.toml` is a flat table of `#rrggbb` strings: a `mode`, three
//! semantic colours (`accent`, `selection`, `muted`), a four-step
//! background ladder, a four-step foreground ladder and the sixteen
//! ANSI colours. Older and hand-written themes use aliases (`bg`,
//! `fg`, `color0`..`color15`, `purple`, `theme_type`); the resolver
//! here copies the alias and mode rules of Omarchy's own
//! `omarchy-theme-color` so Klipp sees the same palette every other
//! themed app does. [`map`] then turns those keys into the 39 Klipp
//! tokens. Nothing outside the state directory is ever read, and only
//! colour strings are parsed, so a theme installed from a stranger's
//! repository cannot do more than pick colours.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::Color32;

use super::{HexColor, ThemeColors};

/// Registry key of the live Omarchy entry. Also what
/// `AppearancePrefs::theme_name` never has to hold: following is a
/// separate boolean, so the user's own choice survives toggling.
pub const THEME_ID: &str = "Omarchy";

/// Where Omarchy keeps the active theme, plus its slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmarchyState {
    /// `$XDG_STATE_HOME/omarchy/current`.
    pub current_dir: PathBuf,
    /// Contents of `theme.name`, e.g. `tokyo-night`; empty if the
    /// file is missing (older layouts).
    pub slug: String,
}

impl OmarchyState {
    pub fn colors_path(&self) -> PathBuf {
        self.current_dir.join("theme").join("colors.toml")
    }

    /// "tokyo-night" → "Tokyo Night", the same way `omarchy theme
    /// current` prints it.
    pub fn display_name(&self) -> String {
        display_name(&self.slug)
    }
}

/// The Omarchy state directory, honouring `XDG_STATE_HOME`.
fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        let dir = PathBuf::from(dir);
        if dir.is_absolute() {
            return Some(dir);
        }
    }
    dirs::home_dir().map(|h| h.join(".local").join("state"))
}

/// Detect a running Omarchy install. The test is the presence of the
/// resolved palette, not an environment variable: that is what
/// matters, and it also works when Klipp is launched from a shell that
/// lacks `OMARCHY_PATH`.
pub fn detect() -> Option<OmarchyState> {
    let current_dir = state_dir()?.join("omarchy").join("current");
    detect_in(current_dir)
}

fn detect_in(current_dir: PathBuf) -> Option<OmarchyState> {
    if !current_dir.join("theme").join("colors.toml").is_file() {
        return None;
    }
    let slug = std::fs::read_to_string(current_dir.join("theme.name"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(OmarchyState { current_dir, slug })
}

pub fn display_name(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read and resolve the current palette. `None` while Omarchy is
/// mid-switch (the directory is briefly absent) or if the file does
/// not parse; the caller keeps the previous theme.
pub fn load(state: &OmarchyState) -> Option<ThemeColors> {
    let raw = std::fs::read_to_string(state.colors_path()).ok()?;
    let light_marker = state.current_dir.join("theme").join("light.mode").is_file();
    let palette = Palette::parse(&raw, light_marker)?;
    Some(map(&palette))
}

// ── The resolved palette ──────────────────────────────────────────

/// A `colors.toml` after alias resolution. Every accessor has a
/// fallback, mirroring `omarchy-theme-color`.
#[derive(Debug, Clone)]
pub struct Palette {
    colors: HashMap<String, Color32>,
    pub light: bool,
}

impl Palette {
    /// Parse the TOML text. `light_marker` is whether a `light.mode`
    /// file sits beside it (the legacy way to flag a light theme).
    pub fn parse(raw: &str, light_marker: bool) -> Option<Self> {
        let table: toml::Table = toml::from_str(raw).ok()?;
        let mut colors = HashMap::new();
        let mut mode: Option<String> = None;
        for (k, v) in table {
            let Some(s) = v.as_str() else { continue };
            match k.as_str() {
                "mode" => mode = Some(s.trim().to_ascii_lowercase()),
                "theme_type" => {
                    if mode.is_none() {
                        mode = Some(s.trim().to_ascii_lowercase());
                    }
                }
                _ => {
                    if let Some(c) = parse_hex(s) {
                        colors.insert(k, c);
                    }
                }
            }
        }

        // Alias cascade, both directions. Legacy names first so that
        // a file written against the new schema is never overridden
        // by a stray old key.
        let aliases: &[(&str, &str)] = &[
            ("background", "bg"),
            ("foreground", "fg"),
            ("background", "color0"),
            ("foreground", "color7"),
            ("dark_background", "background"),
            ("darker_background", "dark_background"),
            ("lighter_background", "selection"),
            ("lighter_background", "background"),
            ("bright_foreground", "color15"),
            ("bright_foreground", "foreground"),
            ("light_foreground", "bright_foreground"),
            ("dark_foreground", "color8"),
            ("dark_foreground", "muted"),
            ("dark_foreground", "foreground"),
            ("muted", "color8"),
            ("muted", "dark_foreground"),
            ("selection", "selection_background"),
            ("selection", "lighter_background"),
            ("magenta", "purple"),
            ("purple", "magenta"),
            ("red", "color1"),
            ("green", "color2"),
            ("yellow", "color3"),
            ("blue", "color4"),
            ("magenta", "color5"),
            ("cyan", "color6"),
            ("bright_red", "color9"),
            ("bright_green", "color10"),
            ("bright_yellow", "color11"),
            ("bright_blue", "color12"),
            ("bright_magenta", "color13"),
            ("bright_cyan", "color14"),
            ("bright_red", "red"),
            ("bright_green", "green"),
            ("bright_yellow", "yellow"),
            ("bright_blue", "blue"),
            ("bright_magenta", "magenta"),
            ("bright_cyan", "cyan"),
            ("accent", "blue"),
        ];
        // Two passes so a chain (bg → background → dark_background)
        // resolves regardless of order.
        for _ in 0..2 {
            for (key, from) in aliases {
                if !colors.contains_key(*key) {
                    if let Some(c) = colors.get(*from).copied() {
                        colors.insert((*key).to_string(), c);
                    }
                }
            }
        }
        // A palette without even a background is not a palette.
        let bg = *colors.get("background")?;
        colors.entry("foreground".to_string()).or_insert(if luminance_sum(bg) > 382 {
            Color32::BLACK
        } else {
            Color32::WHITE
        });

        let light = match mode.as_deref() {
            Some("light") => true,
            Some("dark") => false,
            _ => light_marker || luminance_sum(bg) > 382,
        };
        Some(Self { colors, light })
    }

    /// A colour by key, falling back through `fallbacks` in order,
    /// then to `foreground`.
    pub fn get(&self, key: &str, fallbacks: &[&str]) -> Color32 {
        if let Some(c) = self.colors.get(key) {
            return *c;
        }
        for f in fallbacks {
            if let Some(c) = self.colors.get(*f) {
                return *c;
            }
        }
        self.colors
            .get("foreground")
            .copied()
            .unwrap_or(Color32::WHITE)
    }

    pub fn has(&self, key: &str) -> bool {
        self.colors.contains_key(key)
    }
}

fn parse_hex(s: &str) -> Option<Color32> {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
    Some(Color32::from_rgb(b(0)?, b(2)?, b(4)?))
}

fn luminance_sum(c: Color32) -> u32 {
    c.r() as u32 + c.g() as u32 + c.b() as u32
}

/// Per-channel sRGB interpolation, rounded the way Omarchy's own
/// `mix_color` rounds, so derived shades match the ones it writes into
/// its other app themes.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t + 0.5).floor() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// A translucent overlay of `c` at `alpha`, in the premultiplied form
/// the Klipp tokens store (`#rrggbbaa` with channels scaled by alpha).
pub(crate) fn veil(c: Color32, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let ch = |x: u8| ((x as f32) * (a as f32) / 255.0 + 0.5) as u8;
    Color32::from_rgba_premultiplied(ch(c.r()), ch(c.g()), ch(c.b()), a)
}

/// Relative luminance in 0..1, for the text-on-accent contrast pick.
pub(crate) fn rel_luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Map a resolved Omarchy palette onto Klipp's tokens.
///
/// The ladders map straight across: Omarchy's background steps are
/// semantic (`dark_background` on a light theme is a slightly darker
/// page, not a dark colour), which is what lets one mapping serve
/// both polarities. Role colours come from the ANSI set pulled a
/// quarter of the way towards the background, so the timeline reads
/// as one surface the way the built-in palettes do.
pub fn map(p: &Palette) -> ThemeColors {
    let bg = p.get("background", &[]);
    let dark_bg = p.get("dark_background", &["background"]);
    let darker_bg = p.get("darker_background", &["dark_background"]);
    let lighter_bg = p.get("lighter_background", &["selection"]);
    let selection = p.get("selection", &["lighter_background"]);
    let muted = p.get("muted", &["dark_foreground"]);
    let fg = p.get("foreground", &[]);
    let dark_fg = p.get("dark_foreground", &["muted"]);
    let bright_fg = p.get("bright_foreground", &["foreground"]);
    let accent = p.get("accent", &["blue"]);

    let red = p.get("red", &[]);
    let green = p.get("green", &[]);
    let yellow = p.get("yellow", &[]);
    let blue = p.get("blue", &["accent"]);
    let magenta = p.get("magenta", &["purple", "blue"]);
    let cyan = p.get("cyan", &["blue"]);
    let bright_red = p.get("bright_red", &["red"]);
    let bright_blue = p.get("bright_blue", &["blue"]);
    let bright_magenta = p.get("bright_magenta", &["magenta"]);
    let bright_cyan = p.get("bright_cyan", &["cyan"]);
    let orange = if p.has("orange") { p.get("orange", &[]) } else { mix(red, yellow, 0.5) };

    // The secondary accent must not be the accent.
    let accent_alt = if accent == magenta { blue } else { magenta };

    // Text on an accent fill: whichever of the page and the brightest
    // foreground is further from the accent in luminance.
    let la = rel_luminance(accent);
    let text_on_accent = if (rel_luminance(bg) - la).abs() >= (rel_luminance(bright_fg) - la).abs() {
        bg
    } else {
        bright_fg
    };

    let (glow_a, subtle_a, grid_a) = if p.light { (0.24, 0.22, 0.25) } else { (0.18, 0.22, 0.13) };
    let role = |c: Color32| mix(c, bg, 0.25);
    let h = HexColor;

    ThemeColors {
        bg_void: h(darker_bg),
        bg_deep: h(dark_bg),
        bg_panel: h(bg),
        bg_panel_alt: h(lighter_bg),
        bg_raised: h(mix(lighter_bg, fg, 0.08)),
        bg_hover: h(selection),
        bg_viewer: h(darker_bg),

        border: h(mix(bg, muted, 0.6)),
        border_light: h(muted),
        resizer_idle: h(muted),

        accent: h(accent),
        accent_dim: h(mix(accent, bg, 0.4)),
        accent_glow: h(veil(accent, glow_a)),
        accent_subtle: h(veil(accent, subtle_a)),
        accent_alt: h(accent_alt),
        playhead: h(if p.light { red } else { bright_red }),

        text_primary: h(fg),
        text_secondary: h(mix(fg, dark_fg, 0.5)),
        text_dim: h(dark_fg),
        text_accent: h(bright_fg),
        text_on_accent: h(text_on_accent),

        marker_error: h(red),
        marker_success: h(green),
        marker_info: h(blue),
        marker_warning: h(yellow),

        scope_grid: h(veil(yellow, grid_a)),
        scope_label: h(mix(yellow, darker_bg, 0.35)),
        scope_bg: h(darker_bg),

        role_video: h(role(blue)),
        role_dialogue: h(role(green)),
        role_music: h(role(bright_blue)),
        role_sfx: h(role(orange)),
        role_title: h(role(red)),
        role_broll: h(role(cyan)),
        role_ambience: h(role(magenta)),
        role_foley: h(role(bright_magenta)),
        role_voiceover: h(role(bright_cyan)),
        role_graphics: h(role(yellow)),
    }
}

// ── The watcher ───────────────────────────────────────────────────

/// Watch Omarchy's `current/` directory. When `theme.name` (or the
/// `theme` directory itself, on layouts without the name file) is
/// created or modified, set `changed` and wake the UI. The app polls
/// the flag once per frame and reloads. Returns the watcher handle,
/// which must be kept alive; dropping it stops following.
pub fn start_watcher(
    state: &OmarchyState,
    changed: Arc<AtomicBool>,
    ctx: egui::Context,
) -> Option<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            return;
        }
        let hit = event.paths.iter().any(|p: &PathBuf| {
            matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("theme.name") | Some("theme")
            )
        });
        if hit {
            changed.store(true, Ordering::Release);
            ctx.request_repaint();
        }
    })
    .ok()?;

    let dir: &Path = &state.current_dir;
    if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
        tracing::warn!("Omarchy theme watcher: cannot watch {}: {e}", dir.display());
        return None;
    }
    tracing::info!("Omarchy theme watcher: following {}", dir.display());
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKYO_NIGHT: &str = r##"
mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"

bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

    /// `white`: light, monochrome, no orange/brown.
    const WHITE: &str = r##"
mode = "light"
accent = "#6e6e6e"
selection = "#c0c0c0"
muted = "#808080"
background = "#ffffff"
dark_background = "#f5f5f5"
darker_background = "#e8e8e8"
lighter_background = "#c0c0c0"
foreground = "#000000"
dark_foreground = "#c0c0c0"
light_foreground = "#000000"
bright_foreground = "#000000"
red = "#2a2a2a"
yellow = "#4a4a4a"
green = "#3a3a3a"
cyan = "#3e3e3e"
blue = "#1a1a1a"
magenta = "#2e2e2e"
bright_red = "#2a2a2a"
bright_yellow = "#4a4a4a"
bright_green = "#3a3a3a"
bright_cyan = "#3e3e3e"
bright_blue = "#1a1a1a"
bright_magenta = "#2e2e2e"
"##;

    /// A pre-colors.toml theme converted from alacritty: only the
    /// legacy names.
    const LEGACY: &str = r##"
bg = "#282828"
fg = "#ebdbb2"
color0 = "#282828"
color1 = "#cc241d"
color2 = "#98971a"
color3 = "#d79921"
color4 = "#458588"
color5 = "#b16286"
color6 = "#689d6a"
color7 = "#a89984"
color8 = "#928374"
color9 = "#fb4934"
color10 = "#b8bb26"
color11 = "#fabd2f"
color12 = "#83a598"
color13 = "#d3869b"
color14 = "#8ec07c"
color15 = "#ebdbb2"
"##;

    #[test]
    fn tokyo_night_maps_every_token() {
        let p = Palette::parse(TOKYO_NIGHT, false).unwrap();
        assert!(!p.light);
        let t = map(&p);
        assert_eq!(t.bg_panel.0, Color32::from_rgb(0x1a, 0x1b, 0x26));
        assert_eq!(t.bg_void.0, Color32::from_rgb(0x0e, 0x0e, 0x14));
        assert_eq!(t.accent.0, Color32::from_rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(t.text_primary.0, Color32::from_rgb(0xa9, 0xb1, 0xd6));
        assert_eq!(t.marker_error.0, Color32::from_rgb(0xf7, 0x76, 0x8e));
        // accent is magenta-free, so accent_alt is the magenta.
        assert_eq!(t.accent_alt.0, Color32::from_rgb(0xad, 0x8e, 0xe6));
        // The page is far darker than the accent: text on accent is the page.
        assert_eq!(t.text_on_accent.0, t.bg_panel.0);
        // Overlays are premultiplied at the dark alphas.
        assert_eq!(t.accent_glow.0.a(), 46);
        assert_eq!(t.accent_subtle.0.a(), 56);
        // Roles are pulled a quarter towards the page.
        assert_eq!(t.role_video.0, mix(Color32::from_rgb(0x7a, 0xa2, 0xf7), Color32::from_rgb(0x1a, 0x1b, 0x26), 0.25));
        // A KlippTheme builds from it.
        let _: super::super::KlippTheme = t.into();
    }

    #[test]
    fn white_is_light_and_fills_missing_orange() {
        let p = Palette::parse(WHITE, false).unwrap();
        assert!(p.light);
        assert!(!p.has("orange"));
        let t = map(&p);
        assert_eq!(t.text_primary.0, Color32::BLACK);
        assert_eq!(t.bg_panel.0, Color32::WHITE);
        // orange = mix(red, yellow, 0.5) = mix(#2a2a2a, #4a4a4a) = #3a3a3a, then 25% to white.
        assert_eq!(t.role_sfx.0, mix(Color32::from_rgb(0x3a, 0x3a, 0x3a), Color32::WHITE, 0.25));
        // Light alphas.
        assert_eq!(t.accent_glow.0.a(), 61);
        assert_eq!(t.scope_grid.0.a(), 64);
        // Accent is mid-grey (#6e6e6e, luminance 0.16): the white page
        // is further from it than black text, so text on accent is white.
        assert_eq!(t.text_on_accent.0, Color32::WHITE);
    }

    #[test]
    fn legacy_aliases_resolve() {
        let p = Palette::parse(LEGACY, false).unwrap();
        assert!(!p.light, "gruvbox background is dark by luminance");
        assert_eq!(p.get("background", &[]), Color32::from_rgb(0x28, 0x28, 0x28));
        assert_eq!(p.get("foreground", &[]), Color32::from_rgb(0xeb, 0xdb, 0xb2));
        assert_eq!(p.get("red", &[]), Color32::from_rgb(0xcc, 0x24, 0x1d));
        assert_eq!(p.get("bright_blue", &[]), Color32::from_rgb(0x83, 0xa5, 0x98));
        assert_eq!(p.get("muted", &[]), Color32::from_rgb(0x92, 0x83, 0x74));
        // accent falls back to blue.
        assert_eq!(p.get("accent", &["blue"]), Color32::from_rgb(0x45, 0x85, 0x88));
        let t = map(&p);
        assert_eq!(t.accent.0, Color32::from_rgb(0x45, 0x85, 0x88));
    }

    #[test]
    fn mode_precedence() {
        // Explicit mode wins over luminance.
        let p = Palette::parse("mode = \"light\"\nbackground = \"#000000\"\n", false).unwrap();
        assert!(p.light);
        // theme_type is the legacy spelling.
        let p = Palette::parse("theme_type = \"light\"\nbackground = \"#000000\"\n", false).unwrap();
        assert!(p.light);
        // The light.mode marker beats luminance.
        let p = Palette::parse("background = \"#000000\"\n", true).unwrap();
        assert!(p.light);
        // Luminance: 3 × 0x80 = 384 > 382.
        let p = Palette::parse("background = \"#808080\"\n", false).unwrap();
        assert!(p.light);
        let p = Palette::parse("background = \"#7f7f7f\"\n", false).unwrap();
        assert!(!p.light);
    }

    #[test]
    fn rejects_garbage_and_non_colours() {
        assert!(Palette::parse("not toml at all = = =", false).is_none());
        assert!(Palette::parse("accent = \"#123456\"\n", false).is_none(), "no background");
        let p = Palette::parse("background = \"#101010\"\nred = \"rgb(1,2,3)\"\nblue = \"#12345\"\n", false).unwrap();
        assert!(!p.has("red"));
        assert!(!p.has("blue"));
    }

    #[test]
    fn mix_rounds_like_omarchy() {
        assert_eq!(mix(Color32::from_rgb(0, 0, 0), Color32::from_rgb(255, 255, 255), 0.5), Color32::from_rgb(128, 128, 128));
        assert_eq!(mix(Color32::from_rgb(10, 20, 30), Color32::from_rgb(10, 20, 30), 0.3), Color32::from_rgb(10, 20, 30));
    }

    #[test]
    fn slug_display_name() {
        assert_eq!(display_name("tokyo-night"), "Tokyo Night");
        assert_eq!(display_name("catppuccin-latte"), "Catppuccin Latte");
        assert_eq!(display_name("white"), "White");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn detect_reads_state_dir() {
        let dir = std::env::temp_dir().join(format!("klipp-omarchy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(detect_in(dir.clone()).is_none());
        std::fs::create_dir_all(dir.join("theme")).unwrap();
        std::fs::write(dir.join("theme").join("colors.toml"), TOKYO_NIGHT).unwrap();
        std::fs::write(dir.join("theme.name"), "tokyo-night\n").unwrap();
        let s = detect_in(dir.clone()).unwrap();
        assert_eq!(s.slug, "tokyo-night");
        assert_eq!(s.display_name(), "Tokyo Night");
        let colors = load(&s).unwrap();
        assert_eq!(colors.accent.0, Color32::from_rgb(0x7a, 0xa2, 0xf7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every stock theme on a machine that has Omarchy installed
    /// must parse and map. Skipped elsewhere.
    #[test]
    fn stock_themes_parse_when_present() {
        let root = Path::new("/usr/share/omarchy/themes");
        let Ok(rd) = std::fs::read_dir(root) else { return };
        let mut n = 0;
        for entry in rd.flatten() {
            let file = entry.path().join("colors.toml");
            let Ok(raw) = std::fs::read_to_string(&file) else { continue };
            let p = Palette::parse(&raw, false).unwrap_or_else(|| panic!("{} did not parse", file.display()));
            let _ = map(&p);
            n += 1;
        }
        assert!(n > 0);
    }
}
