use egui::Color32;

/// All design tokens for the Klipp GUI. Every colour, size, and spacing
/// constant lives here — do not hardcode `Color32` literals elsewhere.
///
/// # Contrast Rule
///
/// **Any amber/gold/yellow background MUST use `text_on_accent` (dark) for text.**
/// Never use `text_primary`, `text_secondary`, or any light color on an amber background.
/// The accent color (`#e8a020`) works great as TEXT on dark backgrounds, but when used
/// as a BACKGROUND fill, all text on top must be dark (`#1a1a1e` or black).
/// Violating this makes text nearly unreadable.
#[derive(Clone)]
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

impl Default for KlippTheme {
    fn default() -> Self {
        Self {
            bg_void: Color32::from_rgb(0x0a, 0x0a, 0x0c),
            bg_deep: Color32::from_rgb(0x11, 0x11, 0x14),
            bg_panel: Color32::from_rgb(0x16, 0x16, 0x1a),
            bg_panel_alt: Color32::from_rgb(0x1c, 0x1c, 0x21),
            bg_raised: Color32::from_rgb(0x22, 0x22, 0x28),
            bg_hover: Color32::from_rgb(0x2a, 0x2a, 0x32),
            bg_viewer: Color32::from_rgb(28, 28, 30),

            border: Color32::from_rgb(0x2a, 0x2a, 0x32),
            border_light: Color32::from_rgb(0x36, 0x36, 0x40),
            resizer_idle: Color32::from_rgb(55, 55, 65),

            accent: Color32::from_rgb(0xe8, 0xa0, 0x20),
            accent_dim: Color32::from_rgb(0xa0, 0x6e, 0x14),
            // CONTRAST RULE: These are used as BACKGROUNDS. With premultiplied alpha,
            // RGB channels must be <= alpha. Previous values (R=232 > A=46) were invalid
            // and rendered as near-solid amber, making light text unreadable.
            accent_glow: Color32::from_rgba_premultiplied(33, 23, 5, 46),   // ~18% alpha amber
            accent_subtle: Color32::from_rgba_premultiplied(21, 14, 3, 23), // ~9% alpha amber

            text_primary: Color32::from_rgb(0xe8, 0xe8, 0xee),
            text_secondary: Color32::from_rgb(0x90, 0x90, 0xa0),
            text_dim: Color32::from_rgb(0x50, 0x50, 0x60),
            text_accent: Color32::from_rgb(0xe8, 0xa0, 0x20),
            text_on_accent: Color32::from_rgb(0x1a, 0x1a, 0x1e),

            role_video: Color32::from_rgb(0x2d, 0x6f, 0xa8),
            role_dialogue: Color32::from_rgb(0x3a, 0x8c, 0x6e),
            role_music: Color32::from_rgb(0x42, 0x85, 0xf4),
            role_sfx: Color32::from_rgb(0xa8, 0x5c, 0x2d),
            role_title: Color32::from_rgb(0xc0, 0x39, 0x2b),
            role_broll: Color32::from_rgb(0x4a, 0x6e, 0xa8),
            role_ambience: Color32::from_rgb(0x9c, 0x27, 0xb0),
            role_foley: Color32::from_rgb(0xe9, 0x1e, 0x63),
            role_voiceover: Color32::from_rgb(0x00, 0xbc, 0xd4),
            role_graphics: Color32::from_rgb(0xff, 0xeb, 0x3b),
        }
    }
}

impl KlippTheme {
    /// Light-mode palette. Mirrors the structure of `Default` (dark
    /// mode) but flips backgrounds to off-whites/soft greys, text to
    /// near-blacks, and tunes the accent glow alpha so it reads
    /// against light backgrounds the same way the dark glow reads
    /// against dark backgrounds.
    ///
    /// Light-mode contrast rule: amber/gold accent backgrounds still
    /// require dark text (`text_on_accent`) — the same rule as dark
    /// mode, because the rule is about contrast against the accent
    /// itself, not against the surrounding theme.
    pub fn light() -> Self {
        Self {
            // Backgrounds: lightest (void = window border / extreme bg)
            // → darkest (raised = card surfaces). Inverted polarity from
            // Default but with the same six-step gradient so depth cues
            // (panel-on-panel layering) still read correctly.
            bg_void: Color32::from_rgb(0xee, 0xee, 0xf0),
            bg_deep: Color32::from_rgb(0xf4, 0xf4, 0xf6),
            bg_panel: Color32::from_rgb(0xfa, 0xfa, 0xfc),
            bg_panel_alt: Color32::from_rgb(0xf0, 0xf0, 0xf3),
            bg_raised: Color32::from_rgb(0xff, 0xff, 0xff),
            bg_hover: Color32::from_rgb(0xe6, 0xe6, 0xea),
            bg_viewer: Color32::from_rgb(0xe0, 0xe0, 0xe4),

            border: Color32::from_rgb(0xcc, 0xcc, 0xd2),
            border_light: Color32::from_rgb(0xb8, 0xb8, 0xc0),
            resizer_idle: Color32::from_rgb(0xb0, 0xb0, 0xb8),

            // Same warm amber accent — works on both polarities. The
            // glow alphas need re-tuning per polarity though: a
            // translucent amber that looks like a soft glow on dark
            // turns into a muddy yellow stain on white, so light-mode
            // glow uses a slightly higher alpha + lower RGB to
            // produce a similar perceptual "hint of accent" overlay.
            accent: Color32::from_rgb(0xc8, 0x86, 0x10),
            accent_dim: Color32::from_rgb(0x90, 0x60, 0x0c),
            accent_glow: Color32::from_rgba_premultiplied(50, 35, 8, 60),
            accent_subtle: Color32::from_rgba_premultiplied(33, 22, 5, 30),

            text_primary: Color32::from_rgb(0x18, 0x18, 0x1c),
            text_secondary: Color32::from_rgb(0x55, 0x55, 0x60),
            text_dim: Color32::from_rgb(0x88, 0x88, 0x90),
            text_accent: Color32::from_rgb(0xa8, 0x70, 0x10),
            // Dark text for use ON accent backgrounds — same rule and
            // same value as dark mode (the accent fill is the same
            // colour on both polarities, so the contrast partner is
            // also the same).
            text_on_accent: Color32::from_rgb(0x1a, 0x1a, 0x1e),

            // Role colors — keep the dark-mode hues; they sit OK on
            // light backgrounds because they're saturated mid-value
            // colours, not pastels. Per-clip override stays available
            // via prefs.
            role_video: Color32::from_rgb(0x2d, 0x6f, 0xa8),
            role_dialogue: Color32::from_rgb(0x3a, 0x8c, 0x6e),
            role_music: Color32::from_rgb(0x42, 0x85, 0xf4),
            role_sfx: Color32::from_rgb(0xa8, 0x5c, 0x2d),
            role_title: Color32::from_rgb(0xc0, 0x39, 0x2b),
            role_broll: Color32::from_rgb(0x4a, 0x6e, 0xa8),
            role_ambience: Color32::from_rgb(0x9c, 0x27, 0xb0),
            role_foley: Color32::from_rgb(0xe9, 0x1e, 0x63),
            role_voiceover: Color32::from_rgb(0x00, 0x96, 0xa8),
            role_graphics: Color32::from_rgb(0xc8, 0xa0, 0x10),
        }
    }

    /// Build theme from appearance preferences. Picks the dark or
    /// light base palette per `prefs.theme_mode`, then layers the
    /// user's accent + role color overrides on top.
    pub fn from_prefs(prefs: &crate::prefs::AppearancePrefs) -> Self {
        let [ar, ag, ab] = prefs.accent_color;
        let accent = Color32::from_rgb(ar, ag, ab);
        let mut t = match prefs.theme_mode {
            crate::prefs::ThemeMode::Dark => Self::default(),
            crate::prefs::ThemeMode::Light => Self::light(),
        };
        t.accent = accent;
        t.accent_dim = Color32::from_rgb(ar.saturating_sub(0x48), ag.saturating_sub(0x32), ab.saturating_sub(0x0c));
        // CONTRAST RULE: premultiplied alpha requires RGB <= alpha.
        // Scale RGB by (alpha/255) to get correct translucent tint.
        // Light mode wants a slightly more visible glow alpha to
        // overcome white backgrounds; dark mode keeps its softer glow.
        let (glow_a, sub_a) = match prefs.theme_mode {
            crate::prefs::ThemeMode::Dark => (46u8, 23u8),
            crate::prefs::ThemeMode::Light => (60u8, 30u8),
        };
        let pm = |c: u8, a: u8| (c as u16 * a as u16 / 255) as u8;
        t.accent_glow = Color32::from_rgba_premultiplied(pm(ar, glow_a), pm(ag, glow_a), pm(ab, glow_a), glow_a);
        t.accent_subtle = Color32::from_rgba_premultiplied(pm(ar, sub_a), pm(ag, sub_a), pm(ab, sub_a), sub_a);
        t.text_accent = accent;
        // Role colors from prefs
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
