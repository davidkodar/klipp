use egui::Color32;
use serde::{Deserialize, Serialize};

mod color_serde;
pub mod omarchy;
pub mod portal;
mod registry;

pub use color_serde::HexColor;
pub use registry::{ThemeId, ThemeRegistry, ThemeSource, BUILTIN_DARK, BUILTIN_LIGHT};
#[cfg(test)]
pub(crate) use registry::parse_builtin;

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
    /// Secondary accent — used for things that need to read as
    /// "different from the primary accent" without taking on a
    /// status meaning. Examples in the codebase: per-effect A/B
    /// compare highlight, Title-clip identity badge, drag-drop
    /// highlight on Effect / Title payloads (where the primary
    /// accent's amber would clash with the orange Effect / red
    /// Title role colours). A theme can pick any colour here as
    /// long as it's perceptually distinct from `accent` on its
    /// background polarity.
    pub accent_alt: Color32,
    /// The playhead and the record/live marks: the one warm colour in a
    /// neutral interface, kept apart from `accent` so selection and
    /// active state never look like the playhead.
    pub playhead: Color32,

    // Text
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_dim: Color32,
    pub text_accent: Color32,
    /// Dark text for use ON accent-colored backgrounds (amber buttons, badges).
    /// RULE: Any amber/gold background MUST use this color for text, never light text.
    pub text_on_accent: Color32,

    // Functional marker / status colours.
    //
    // FUNCTIONAL naming (`marker_error`, `marker_success`, …) rather
    // than visual (`red`, `green`, …) so future themes can map these
    // signals onto whatever palette they want. A high-contrast theme
    // might pick magenta for `marker_error`; a colour-blind-friendly
    // theme might pick orange and blue instead of green and red.
    pub marker_error: Color32,
    pub marker_success: Color32,
    pub marker_info: Color32,
    pub marker_warning: Color32,

    // Scope display tokens.
    //
    // `scope_grid` is a translucent overlay drawn on top of the scope
    // trace; its alpha is polarity-dependent (a low-alpha amber that
    // reads as a hint on dark scopes turns into a stripe on light
    // scopes), so each theme tunes its own alpha.
    // `scope_label` is the small text label drawn next to scope ticks.
    pub scope_grid: Color32,
    pub scope_label: Color32,
    /// Background fill of the scope display surface (waveform /
    /// parade / vectorscope / histogram canvas). Industry-standard
    /// NLEs (Resolve, Premiere, FCP) keep this near-black even in
    /// their Light themes because the trace colours (white / RGB
    /// channel hues / luminance gradients) are designed to read on
    /// dark backgrounds. A future light-friendly trace shader could
    /// let this pick up the surrounding panel polarity, but until
    /// then themes that try to invert it will lose trace legibility.
    pub scope_bg: Color32,

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
    pub accent_alt: HexColor,
    pub playhead: HexColor,

    pub text_primary: HexColor,
    pub text_secondary: HexColor,
    pub text_dim: HexColor,
    pub text_accent: HexColor,
    pub text_on_accent: HexColor,

    pub marker_error: HexColor,
    pub marker_success: HexColor,
    pub marker_info: HexColor,
    pub marker_warning: HexColor,

    pub scope_grid: HexColor,
    pub scope_label: HexColor,
    pub scope_bg: HexColor,

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
            accent_alt: c.accent_alt.0,
            playhead: c.playhead.0,
            text_primary: c.text_primary.0,
            text_secondary: c.text_secondary.0,
            text_dim: c.text_dim.0,
            text_accent: c.text_accent.0,
            text_on_accent: c.text_on_accent.0,
            marker_error: c.marker_error.0,
            marker_success: c.marker_success.0,
            marker_info: c.marker_info.0,
            marker_warning: c.marker_warning.0,
            scope_grid: c.scope_grid.0,
            scope_label: c.scope_label.0,
            scope_bg: c.scope_bg.0,
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
    /// Colour the theme in `accent`, the tones made from it the way the
    /// system palette makes them: a dimmer accent, the translucent glow
    /// and fill, and text on it in whichever of black and white reads.
    pub fn set_accent(&mut self, accent: Color32, light: bool) {
        use omarchy::{mix, rel_luminance, veil};
        let (glow, subtle) = if light { (0.24, 0.22) } else { (0.18, 0.22) };
        self.accent = accent;
        self.accent_dim = mix(accent, self.bg_panel, 0.4);
        self.accent_glow = veil(accent, glow);
        self.accent_subtle = veil(accent, subtle);
        // Contrast against white is (1.05) / (L + 0.05), against black
        // (L + 0.05) / 0.05; they are equal at L = 0.179.
        self.text_on_accent = if rel_luminance(accent) > 0.179 { Color32::from_rgb(16, 16, 20) } else { Color32::WHITE };
    }

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
        // Following the system theme: the Omarchy entry is the theme
        // while it exists, and `theme_name` keeps the user's own
        // choice untouched for when following is switched off (or the
        // preferences travel to a machine without Omarchy).
        if prefs.follow_system_theme {
            if let Some(t) = registry.get(omarchy::THEME_ID) {
                let mut t = t.clone();
                // The system palette supplies the role defaults; a
                // user who has picked their own role colours keeps
                // them, exactly as with every other theme.
                if !prefs.roles_are_default() {
                    t.apply_role_prefs(prefs);
                }
                return t;
            }
            // Elsewhere the desktop portal's dark or light preference
            // and accent; with no preference, the picked theme below.
            if let Some(mut t) = registry.portal_theme() {
                t.apply_role_prefs(prefs);
                return t;
            }
        }

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

        // The accent is the theme's. It used to follow a preference,
        // which put the old amber over every palette that came after
        // and made the accent a decoration rather than a signal.

        // Role colours — user prefs always win (themes specify role
        // defaults but the per-clip-role picker is universal).
        t.apply_role_prefs(prefs);
        t
    }

    fn apply_role_prefs(&mut self, prefs: &crate::prefs::AppearancePrefs) {
        let t = self;
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
    }

    /// Polarity-aware "soft chrome overlay" — translucent white on
    /// dark themes, translucent black on light themes, at the
    /// requested alpha (0-255). Use anywhere the previous code
    /// hardcoded `Color32::from_rgba_premultiplied(255, 255, 255, N)`
    /// for "subtle white veil over the panel" — section dividers,
    /// dim text on amber rows, faint icon tints, etc. Light theme
    /// flips polarity so the overlay still reads as a soft grey
    /// against the bg instead of being invisible (white-on-white).
    ///
    /// Premultiplied alpha format: RGB == alpha (so the colour is
    /// pre-blended with its own opacity). On dark, that's a fully
    /// premultiplied translucent white; on light, fully
    /// premultiplied translucent black (RGB = 0, alpha = N).
    pub fn overlay(&self, alpha: u8) -> Color32 {
        if self.is_light() {
            Color32::from_rgba_premultiplied(0, 0, 0, alpha)
        } else {
            Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
        }
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

        // A pressed widget keeps its text colour: egui also uses this
        // slot for every `strong()` label, so an accent here would turn
        // each bold heading in the app blue.
        visuals.widgets.active.bg_fill = self.bg_hover;
        visuals.widgets.active.fg_stroke =
            egui::Stroke::new(1.0_f32, self.text_primary);
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
            Color32::from_rgb(0xd6, 0xe4, 0xf8)
        } else {
            Color32::from_rgb(0x1e, 0x2f, 0x4a)
        };
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, self.accent);
        // egui's own status colours follow the theme's markers, so a
        // destructive menu item or a warning label reads in the palette.
        visuals.error_fg_color = self.marker_error;
        visuals.warn_fg_color = self.marker_warning;
        visuals.hyperlink_color = self.accent;

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

/// The type scale: four sizes, hierarchy from weight and colour rather
/// than from a dozen sizes.
pub mod type_scale {
    /// Captions, units, secondary metadata.
    pub const CAPTION: f32 = 10.0;
    /// Body text and most labels.
    pub const BODY: f32 = 11.0;
    /// Control labels, values, tab titles.
    pub const CONTROL: f32 = 12.0;
    /// Panel and section titles.
    pub const TITLE: f32 = 13.0;
    /// The rare headline: dialog titles, empty states.
    pub const HEADLINE: f32 = 16.0;
}

/// Two radii: controls are nearly square, panels and popups a little
/// softer. Anything rounder reads as a pill.
pub mod radius {
    pub const CONTROL: f32 = 2.0;
    pub const PANEL: f32 = 4.0;
}

/// Control heights on a 4 px grid.
pub mod control {
    pub const COMPACT: f32 = 22.0;
    pub const REGULAR: f32 = 26.0;
    pub const LARGE: f32 = 32.0;
}

pub const TOOLBAR_HEIGHT: f32 = 36.0;
pub const STATUSBAR_HEIGHT: f32 = 26.0;
pub const TIMELINE_MIN_H: f32 = 60.0;
/// What share of the editing area the timeline starts with.
///
/// A proportion, not a pixel count. The old default was 210 px, which
/// is a third of the room on a small window and a seventh on a large
/// one — the editor's most-used surface shrinking as the display grows.
/// Premiere, Resolve and Final Cut all give the edit timeline roughly
/// 35-45% of the window; 38% sits in the middle of that.
///
/// Only the starting point. Dragging the divider stores a pixel height
/// and that is honoured from then on, so nobody's arrangement moves.
pub const TIMELINE_DEFAULT_FRAC: f32 = 0.38;

/// Marks a timeline height that has never been set, so the proportion
/// above decides it against the real window instead.
pub const TIMELINE_H_UNSET: f32 = 0.0;
pub const TIMELINE_MAX_H_PCT: f32 = 0.85;
/// The Colour page's timeline strip: at most this share of the column
/// above the scope dock, and room under its tracks for the scrollbar.
pub const COLOR_STRIP_MAX_FRAC: f32 = 0.25;
pub const COLOR_STRIP_SLACK: f32 = 14.0;
pub const BROWSER_DEFAULT_W: f32 = 300.0;
pub const BROWSER_MIN_W: f32 = 220.0;
pub const BROWSER_MAX_W: f32 = 900.0;
pub const INSPECTOR_DEFAULT_W: f32 = 360.0;
pub const INSPECTOR_MIN_W: f32 = 260.0;
pub const INSPECTOR_MAX_W: f32 = 800.0;
pub const RESIZER_THICKNESS: f32 = 5.0;
/// The Audio Mixer's height until its top edge is dragged.
pub const AUDIO_MIXER_DEFAULT_H: f32 = 260.0;
/// The tallest the Audio Mixer is dragged to.
pub const AUDIO_MIXER_MAX_H: f32 = 600.0;
/// Width of the rail a collapsed side panel leaves behind.
///
/// A collapsed panel is not a gone panel: it keeps a strip carrying its
/// tab icons, so the way back is visible and is the same click that got
/// you there. Collapsing to nothing would leave the edge of the window
/// as the only affordance, which is a thing you have to already know
/// about. 30 px is wide enough for a 16 px icon with breathing room and
/// narrow enough to be worth collapsing to.
pub const COLLAPSED_RAIL_W: f32 = 30.0;
/// Height of the transport chrome below each viewer's video area
/// (16-px scrub bar + 40-px playbar). Used by `layout.rs` so the
/// video portion of the viewer can be sized to project aspect with
/// the chrome reserved below it.
pub const VIEWER_CHROME_HEIGHT: f32 = 56.0;
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
