use egui::Color32;
use serde::{Deserialize, Serialize};

mod color_serde;
mod registry;

pub use color_serde::HexColor;
pub use registry::{ThemeId, ThemeRegistry, ThemeSource, BUILTIN_DARK, BUILTIN_LIGHT};

/// All design tokens for the Klipp GUI. Every colour, size, and spacing
/// constant lives here — do not hardcode `Color32` literals elsewhere.
///
/// `KlippTheme` is the in-memory, runtime form. Themes themselves are
/// authored as TOML against [`ThemeColors`] (see `theme/builtin/*.toml`
/// and `~/.config/klipp/themes/*.toml`); the registry deserialises
/// each TOML into a `ThemeColors` and `KlippTheme::from_colors`
/// promotes it to this runtime representation. Adding a new theme is
/// dropping a TOML file — no code change.
///
/// # Contrast Rule
///
/// **Any amber/gold/yellow background MUST use `text_on_accent` (dark) for text.**
/// Never use `text_primary`, `text_secondary`, or any light color on an amber background.
/// The accent color (`#e8a020`) works great as TEXT on dark backgrounds, but when used
/// as a BACKGROUND fill, all text on top must be dark (`#1a1a1e` or black).
/// Violating this makes text nearly unreadable.
#[derive(Clone, Debug)]
pub struct KlippTheme {
    // Backgrounds — darkest to lightest
    pub bg_void: Color32,
    pub bg_deep: Color32,
    pub bg_panel: Color32,
    pub bg_panel_alt: Color32,
    pub bg_raised: Color32,
    pub bg_hover: Color32,
    /// Elevated container background for the program viewer — bg_deep + ~4% brightness.
    /// Used as the Frame fill around the viewer; the inner video letterbox stays pure black.
    pub bg_viewer: Color32,

    // Borders
    pub border: Color32,
    pub border_light: Color32,
    /// Idle fill color for panel resizer strips. Brighter than `border` so the
    /// strip reads as a distinct divider against `bg_raised` inspector cards.
    pub resizer_idle: Color32,

    // Accent — warm amber/gold
    pub accent: Color32,
    pub accent_dim: Color32,
    pub accent_glow: Color32,
    pub accent_subtle: Color32,

    // Text
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_dim: Color32,
    pub text_accent: Color32,
    /// Dark text for use ON accent-colored backgrounds (amber buttons, badges).
    /// RULE: Any amber/gold background MUST use this color for text, never light text.
    pub text_on_accent: Color32,

    // Role colours — one per clip/track role
    pub role_video: Color32,
    pub role_dialogue: Color32,
    pub role_music: Color32,
    pub role_sfx: Color32,
    pub role_title: Color32,
    pub role_broll: Color32,
    pub role_ambience: Color32,
    pub role_foley: Color32,
    pub role_voiceover: Color32,
    pub role_graphics: Color32,
}

/// Serde-friendly mirror of [`KlippTheme`]. Every field is a hex
/// colour string (`"#rrggbb"` or `"#rrggbbaa"`) so themes round-trip
/// through TOML without bespoke encoders. Built-in palettes ship as
/// embedded TOML files (`theme/builtin/dark.toml`, `light.toml`); user
/// themes drop into `~/.config/klipp/themes/*.toml`.
///
/// Keep this struct in sync with `KlippTheme` field-for-field — the
/// `From<ThemeColors> for KlippTheme` conversion is a 1:1 mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeColors {
    pub bg_void: HexColor,
    pub bg_deep: HexColor,
    pub bg_panel: HexColor,
    pub bg_panel_alt: HexColor,
    pub bg_raised: HexColor,
    pub bg_hover: HexColor,
    pub bg_viewer: HexColor,

    pub border: HexColor,
    pub border_light: HexColor,
    pub resizer_idle: HexColor,

    pub accent: HexColor,
    pub accent_dim: HexColor,
    pub accent_glow: HexColor,
    pub accent_subtle: HexColor,

    pub text_primary: HexColor,
    pub text_secondary: HexColor,
    pub text_dim: HexColor,
    pub text_accent: HexColor,
    pub text_on_accent: HexColor,

    pub role_video: HexColor,
    pub role_dialogue: HexColor,
    pub role_music: HexColor,
    pub role_sfx: HexColor,
    pub role_title: HexColor,
    pub role_broll: HexColor,
    pub role_ambience: HexColor,
    pub role_foley: HexColor,
    pub role_voiceover: HexColor,
    pub role_graphics: HexColor,
}

impl From<ThemeColors> for KlippTheme {
    fn from(c: ThemeColors) -> Self {
        Self {
            bg_void: c.bg_void.0,
            bg_deep: c.bg_deep.0,
            bg_panel: c.bg_panel.0,
            bg_panel_alt: c.bg_panel_alt.0,
            bg_raised: c.bg_raised.0,
            bg_hover: c.bg_hover.0,
            bg_viewer: c.bg_viewer.0,
            border: c.border.0,
            border_light: c.border_light.0,
            resizer_idle: c.resizer_idle.0,
            accent: c.accent.0,
            accent_dim: c.accent_dim.0,
            accent_glow: c.accent_glow.0,
            accent_subtle: c.accent_subtle.0,
            text_primary: c.text_primary.0,
            text_secondary: c.text_secondary.0,
            text_dim: c.text_dim.0,
            text_accent: c.text_accent.0,
            text_on_accent: c.text_on_accent.0,
            role_video: c.role_video.0,
            role_dialogue: c.role_dialogue.0,
            role_music: c.role_music.0,
            role_sfx: c.role_sfx.0,
            role_title: c.role_title.0,
            role_broll: c.role_broll.0,
            role_ambience: c.role_ambience.0,
            role_foley: c.role_foley.0,
            role_voiceover: c.role_voiceover.0,
            role_graphics: c.role_graphics.0,
        }
    }
}

impl Default for KlippTheme {
    /// Default theme = the built-in Dark palette, parsed from the
    /// embedded `theme/builtin/dark.toml`. Every other theme
    /// (built-in or user-supplied) is loaded the same way through
    /// [`ThemeRegistry`].
    fn default() -> Self {
        registry::parse_builtin(BUILTIN_DARK)
            .expect("embedded dark theme parses")
            .into()
    }
}

impl KlippTheme {
    /// Build a theme by name from `registry`, then layer the user's
    /// accent + role colour overrides on top. Falls back to the Dark
    /// built-in if the named theme is missing (e.g. user deleted the
    /// TOML file referenced in their preferences).
    ///
    /// The accent override scales the embedded `accent_glow` /
    /// `accent_subtle` translucent overlays so the accent stays
    /// visually consistent on whatever bg polarity the picked theme
    /// uses — light themes need a slightly more opaque glow to read
    /// against white, dark themes keep their softer glow. The
    /// per-theme glow alpha hint comes from the deserialised
    /// `accent_glow` itself.
    pub fn from_prefs(
        prefs: &crate::prefs::AppearancePrefs,
        registry: &ThemeRegistry,
    ) -> Self {
        let mut t: KlippTheme = registry
            .get(&prefs.theme_name)
            .or_else(|| registry.get(registry::DEFAULT_NAME))
            .cloned()
            .unwrap_or_else(|| {
                tracing::warn!(
                    "ThemeRegistry has no theme '{}' AND no default '{}' — falling back to compiled Dark",
                    prefs.theme_name, registry::DEFAULT_NAME,
                );
                KlippTheme::default()
            });

        // Apply user accent override.
        let [ar, ag, ab] = prefs.accent_color;
        let accent = Color32::from_rgb(ar, ag, ab);
        t.accent = accent;
        t.accent_dim = Color32::from_rgb(
            ar.saturating_sub(0x48),
            ag.saturating_sub(0x32),
            ab.saturating_sub(0x0c),
        );
        // The theme's embedded accent_glow / accent_subtle carry the
        // alpha that suits its polarity. Reuse those alphas, but
        // recolour to the user's accent so the glow tint follows the
        // accent picker.
        let glow_a = t.accent_glow.a();
        let sub_a = t.accent_subtle.a();
        let pm = |c: u8, a: u8| (c as u16 * a as u16 / 255) as u8;
        t.accent_glow = Color32::from_rgba_premultiplied(pm(ar, glow_a), pm(ag, glow_a), pm(ab, glow_a), glow_a);
        t.accent_subtle = Color32::from_rgba_premultiplied(pm(ar, sub_a), pm(ag, sub_a), pm(ab, sub_a), sub_a);
        t.text_accent = accent;

        // Role colours — user prefs always win (themes specify role
        // defaults but the per-clip-role picker is universal).
        let c = |rgb: [u8; 3]| Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        t.role_video = c(prefs.role_video);
        t.role_dialogue = c(prefs.role_dialogue);
        t.role_music = c(prefs.role_music);
        t.role_sfx = c(prefs.role_sfx);
        t.role_title = c(prefs.role_title);
        t.role_broll = c(prefs.role_broll);
        t.role_ambience = c(prefs.role_ambience);
        t.role_foley = c(prefs.role_foley);
        t.role_voiceover = c(prefs.role_voiceover);
        t.role_graphics = c(prefs.role_graphics);
        t
    }

    /// Return the theme color for a clip role.
    pub fn clip_role_color(&self, role: klipp_timeline::ClipRole) -> Color32 {
        use klipp_timeline::ClipRole;
        match role {
            ClipRole::Dialogue => self.role_dialogue,
            ClipRole::Music => self.role_music,
            ClipRole::SFX => self.role_sfx,
            ClipRole::Ambience => self.role_ambience,
            ClipRole::Foley => self.role_foley,
            ClipRole::Voiceover => self.role_voiceover,
            ClipRole::BRoll => self.role_broll,
            ClipRole::Graphics => self.role_graphics,
        }
    }

    /// True iff this palette is the light-mode variant (text_primary
    /// is darker than bg_panel). Used to pick polarity-aware values
    /// for egui's built-in widgets that don't track our theme tokens
    /// directly (selection bg fill, etc.).
    fn is_light(&self) -> bool {
        // Average channel value of text_primary vs bg_panel — if text
        // is darker than bg, we're light-mode.
        let avg = |c: Color32| (c.r() as u32 + c.g() as u32 + c.b() as u32) / 3;
        avg(self.text_primary) < avg(self.bg_panel)
    }

    /// Apply theme colours to egui's global visuals.
    pub fn apply_to_egui(&self, ctx: &egui::Context) {
        // Pick the right base — egui uses the base to colour widgets
        // we don't override (popups, tooltips, scrollbar handles…)
        // and to pick reasonable defaults for everything else.
        let mut visuals = if self.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };

        visuals.window_fill = self.bg_void;
        visuals.panel_fill = self.bg_panel;
        visuals.faint_bg_color = self.bg_deep;
        visuals.extreme_bg_color = self.bg_void;

        visuals.widgets.noninteractive.bg_fill = self.bg_panel;
        visuals.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.0_f32, self.text_secondary);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0_f32, self.border);

        visuals.widgets.inactive.bg_fill = self.bg_raised;
        visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0_f32, self.text_secondary);
        visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, self.border);

        visuals.widgets.hovered.bg_fill = self.bg_hover;
        visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0_f32, self.text_primary);
        visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, self.border_light);

        visuals.widgets.active.bg_fill = self.bg_hover;
        visuals.widgets.active.fg_stroke =
            egui::Stroke::new(1.0_f32, self.accent);
        visuals.widgets.active.bg_stroke =
            egui::Stroke::new(1.0_f32, self.accent_dim);

        // CONTRAST RULE: selection.bg_fill is what egui paints behind
        // selected dropdown items, text selections, etc. egui draws
        // TEXT on this background using fg_stroke (text_primary). So
        // the selection bg has to contrast with text_primary, which
        // means dark in dark-mode (so light text reads) and light in
        // light-mode (so dark text reads). Both variants use a warm
        // tint — same accent family, different polarity — to mark
        // selection without dominating the panel.
        visuals.selection.bg_fill = if self.is_light() {
            Color32::from_rgb(0xf2, 0xe6, 0xc8)
        } else {
            Color32::from_rgb(0x38, 0x2c, 0x14)
        };
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, self.accent);

        visuals.window_stroke = egui::Stroke::new(1.0_f32, self.border);
        visuals.window_shadow = egui::Shadow::NONE;

        ctx.set_visuals(visuals);
    }

    /// Configure egui font sizes. `base_size` is the body text size (default 13.0).
    pub fn apply_fonts(&self, ctx: &egui::Context) {
        self.apply_fonts_with_size(ctx, 13.0);
    }

    /// Configure egui font sizes with a custom base size from preferences.
    pub fn apply_fonts_with_size(&self, ctx: &egui::Context, base_size: f32) {
        use egui::{FontFamily, FontId, TextStyle};

        install_unicode_fallback_fonts(ctx);

        let mut style = (*ctx.style()).clone();
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(base_size, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(base_size - 2.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(base_size, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(base_size - 3.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(base_size - 1.0, FontFamily::Monospace),
        );
        ctx.set_style(style);
    }
}

/// Register system fallback fonts so egui renders characters outside
/// the bundled font's Latin coverage. Two symptoms without this:
///   * Info icon ⓘ (U+24D8, Enclosed Alphanumerics) shows as a
///     tofu rectangle — egui's default font has no glyph.
///   * Fullwidth pipe ｜ (U+FF5C, commonly found in YouTube-scraped
///     filenames) shows as tofu — same reason.
///
/// We append (not replace) so egui's primary font still wins for
/// Latin glyphs. The two fallbacks ordered by coverage:
///   1. DejaVu Sans — broad Latin/symbol/box-drawing coverage,
///      fills in ⓘ and most Unicode symbols.
///   2. Noto Sans CJK — CJK ideographs, fullwidth forms (｜),
///      Japanese/Chinese/Korean glyphs in media filenames.
///
/// Both are tried at common Linux paths; missing fonts are skipped
/// silently so headless/minimal installs still run.
fn install_unicode_fallback_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontFamily};

    let mut defs = egui::FontDefinitions::default();

    fn try_load_first(paths: &[&str]) -> Option<Vec<u8>> {
        for p in paths {
            if let Ok(bytes) = std::fs::read(p) {
                return Some(bytes);
            }
        }
        None
    }

    // DejaVu Sans — symbols / Enclosed Alphanumerics / Box Drawing.
    const DEJAVU_PATHS: &[&str] = &[
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    if let Some(bytes) = try_load_first(DEJAVU_PATHS) {
        defs.font_data.insert(
            "klipp_fallback_dejavu".to_owned(),
            FontData::from_owned(bytes),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families
                .entry(family)
                .or_default()
                .push("klipp_fallback_dejavu".to_owned());
        }
    }

    // Noto Sans CJK — fullwidth punctuation, CJK ideographs in
    // media filenames (｜, 「」, Japanese subs, etc.). The .ttc
    // holds JP/SC/TC/KR/HK variants; index 0 is JP, which is fine
    // as a generic CJK fallback (all variants share Han + fullwidth
    // coverage for what media filenames throw at us).
    const CJK_PATHS: &[&str] = &[
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    if let Some(bytes) = try_load_first(CJK_PATHS) {
        let mut data = FontData::from_owned(bytes);
        data.index = 0;
        defs.font_data.insert("klipp_fallback_cjk".to_owned(), data);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families
                .entry(family)
                .or_default()
                .push("klipp_fallback_cjk".to_owned());
        }
    }

    ctx.set_fonts(defs);
}

// ── Fixed size constants ──

pub const TOOLBAR_HEIGHT: f32 = 44.0;
pub const STATUSBAR_HEIGHT: f32 = 26.0;
pub const TIMELINE_MIN_H: f32 = 60.0;
pub const TIMELINE_DEFAULT_H: f32 = 210.0;
pub const TIMELINE_MAX_H_PCT: f32 = 0.85;
pub const BROWSER_DEFAULT_W: f32 = 270.0;
pub const BROWSER_MIN_W: f32 = 80.0;
pub const BROWSER_MAX_W: f32 = 900.0;
pub const INSPECTOR_DEFAULT_W: f32 = 230.0;
pub const INSPECTOR_MIN_W: f32 = 120.0;
pub const INSPECTOR_MAX_W: f32 = 800.0;
pub const RESIZER_THICKNESS: f32 = 5.0;
pub const TRACK_VIDEO_H: f32 = 50.0;
pub const TRACK_AUDIO_H: f32 = 48.0;
pub const TRACK_TEXT_H: f32 = 32.0;
pub const TRACK_HEADER_W: f32 = 110.0;
pub const RULER_HEIGHT: f32 = 22.0;
pub const FILMSTRIP_FRAME_W: f32 = 46.0;
pub const CLIP_CORNER_RADIUS: f32 = 4.0;
pub const CLIP_ROLE_BAR_W: f32 = 3.0;
pub const PLAY_BTN_SIZE: f32 = 38.0;
pub const TOOL_BTN_SIZE: f32 = 32.0;
pub const SMALL_BTN_SIZE: f32 = 26.0;
