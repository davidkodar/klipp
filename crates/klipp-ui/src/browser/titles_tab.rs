use crate::browser::effects_tab::{EffectSort, EffectViewMode};
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::{
    CharAnimation, CharEntrance, FontStyle, StyleRun, TextAlignment, TextExtrusion, TextGlow,
    TextGradient, TextOutline, TextPath, TitleAnimationType, TitleBackground, TitleData,
    TitlePosition, TitleShadow,
};

/// Preview canvas background used behind every title card. The user
/// picks one from the swatch row above the search bar so they can
/// see how a template would read against different real-world
/// surfaces (black footage, white footage, gradient sky, warm wood,
/// green chroma).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlesPreviewBg {
    Black,
    White,
    BlueGradient,
    OrangeWood,
    GreenPattern,
}

impl Default for TitlesPreviewBg {
    fn default() -> Self {
        TitlesPreviewBg::BlueGradient
    }
}

/// Built-in vs user-imported source filter for the Titles tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlesSourceFilter {
    All,
    Builtin,
    User,
}

impl Default for TitlesSourceFilter {
    fn default() -> Self {
        TitlesSourceFilter::All
    }
}

pub(crate) struct TitleTemplate {
    pub name: &'static str,
    pub category: &'static str,
    #[allow(dead_code)]
    pub preview_text: &'static str,
    pub create_data: fn() -> TitleData,
    /// Animation identifier applied when clip is created. None = static (no animation).
    pub animation: Option<&'static str>,
}

/// Test-only handle to the TITLES registry so cross-module tests can
/// walk every template's create_data and verify the contract that
/// every animated template actually animates after going through the
/// clip-creation path. Production code uses the private `TITLES`.
#[cfg(test)]
pub(crate) const TITLES_FOR_TESTS: &[TitleTemplate] = TITLES;

// ---------------------------------------------------------------------------
// Helper factories
// ---------------------------------------------------------------------------

/// Centered title with optional shadow.
fn centered(text: &str, size: f32, style: FontStyle, color: [u8; 4], shadow: bool) -> TitleData {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = text.into();
    tb.font_size = size;
    tb.font_name = "sans-serif".into();
    tb.font_style = style;
    tb.color = color;
    tb.alignment = TextAlignment::Center;
    tb.shadow = if shadow {
        Some(TitleShadow { color: [0, 0, 0, 160], offset_x: 3.0, offset_y: 3.0, blur: 6.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() })
    } else {
        None
    };
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
    }
    td
}

/// Centered on a full-frame background.
fn centered_on_bg(text: &str, size: f32, style: FontStyle, fg: [u8; 4], bg: [u8; 4]) -> TitleData {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = text.into();
    tb.font_size = size;
    tb.font_name = "sans-serif".into();
    tb.font_style = style;
    tb.color = fg;
    tb.alignment = TextAlignment::Center;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::FullFrame { color: bg };
    td
}

/// Lower-third style: left-aligned near bottom, with box background.
fn lower_third_style(text: &str, size: f32, style: FontStyle, fg: [u8; 4], bg: [u8; 4], x: f32, y: f32) -> TitleData {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = text.into();
    tb.font_size = size;
    tb.font_name = "sans-serif".into();
    tb.font_style = style;
    tb.color = fg;
    tb.alignment = TextAlignment::Left;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: bg, padding: 12.0 };
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = TitlePosition { x, y };
    }
    td
}

/// Attach a `TitleAnimationType` to a static template. Used as a
/// thin wrapper around the existing factory helpers so animated
/// templates ship with their animation_type wired through to
/// playback / export — picking "Fade Up" actually fades, picking
/// "Bounce In" actually bounces. Without this the `animation:` field
/// on TitleTemplate was a cosmetic UI badge with no runtime effect.
fn with_anim(mut td: TitleData, anim: TitleAnimationType) -> TitleData {
    td.animation_type = anim;
    td
}

/// Apply a `glitch_intensity` value to the primary text of a static
/// template. Wraps the cinematic / motion-glitch presets so they
/// actually glitch on playback instead of just looking glitchy in
/// the card preview.
fn with_glitch(mut td: TitleData, intensity: f32) -> TitleData {
    if let Some(tb) = td.primary_text_mut() {
        tb.glitch_intensity = intensity.clamp(0.0, 1.0);
    }
    td
}

/// Apply a chromatic-aberration offset to the primary text. Used by
/// the cyberpunk / VHS template presets.
fn with_chromatic(mut td: TitleData, offset_px: f32) -> TitleData {
    if let Some(tb) = td.primary_text_mut() {
        tb.chromatic_aberration_px = offset_px.max(0.0);
    }
    td
}

/// Positioned text with optional shadow, no background.
#[allow(clippy::too_many_arguments)]
fn positioned(text: &str, size: f32, style: FontStyle, color: [u8; 4], align: TextAlignment, x: f32, y: f32, shadow: Option<TitleShadow>) -> TitleData {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = text.into();
    tb.font_size = size;
    tb.font_name = "sans-serif".into();
    tb.font_style = style;
    tb.color = color;
    tb.alignment = align;
    tb.shadow = shadow;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = TitlePosition { x, y };
    }
    td
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const WHITE_DIM: [u8; 4] = [200, 200, 200, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [255, 60, 60, 255];
const GOLD: [u8; 4] = [255, 195, 50, 255];
const CYAN: [u8; 4] = [0, 200, 220, 255];
const GREEN: [u8; 4] = [80, 220, 80, 255];
const _BLUE: [u8; 4] = [60, 120, 255, 255];
const _PINK: [u8; 4] = [255, 100, 180, 255];
const ORANGE: [u8; 4] = [255, 160, 40, 255];
const PURPLE: [u8; 4] = [160, 80, 255, 255];

fn shadow_default() -> Option<TitleShadow> {
    Some(TitleShadow { color: [0, 0, 0, 160], offset_x: 3.0, offset_y: 3.0, blur: 6.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() })
}

fn shadow_soft() -> Option<TitleShadow> {
    Some(TitleShadow { color: [0, 0, 0, 120], offset_x: 2.0, offset_y: 2.0, blur: 8.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() })
}

fn shadow_hard() -> Option<TitleShadow> {
    Some(TitleShadow { color: [0, 0, 0, 200], offset_x: 4.0, offset_y: 4.0, blur: 2.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() })
}

fn shadow_glow(color: [u8; 4]) -> Option<TitleShadow> {
    Some(TitleShadow { color, offset_x: 0.0, offset_y: 0.0, blur: 12.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() })
}

// ---------------------------------------------------------------------------
// Template create functions — Basic (10)
// ---------------------------------------------------------------------------

fn basic_plain_title() -> TitleData { centered("[Your Title]", 72.0, FontStyle::Regular, WHITE, true) }
fn basic_subtitle() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "[Subtitle text]".into();
    tb.font_size = 28.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Regular;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [0, 0, 0, 153], padding: 10.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.88 }; }
    td
}
}
fn basic_credits() -> TitleData { centered("DIRECTED BY\n[Your Name]", 48.0, FontStyle::Regular, WHITE, false) }
fn basic_big_bold() -> TitleData { centered("[Your Title]", 96.0, FontStyle::Bold, WHITE, true) }
fn basic_small_caption() -> TitleData {
    positioned("[Caption text]", 20.0, FontStyle::Regular, WHITE_DIM, TextAlignment::Center, 0.5, 0.92, None)
}
fn basic_centered_quote() -> TitleData {
    positioned("\u{201c}[Your quote here]\u{201d}", 42.0, FontStyle::Italic, WHITE_DIM, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn basic_label() -> TitleData {
    positioned("[LABEL]", 24.0, FontStyle::Bold, WHITE, TextAlignment::Left, 0.06, 0.06, shadow_default())
}
fn basic_watermark() -> TitleData {
    positioned("[WATERMARK]", 20.0, FontStyle::Regular, [255, 255, 255, 80], TextAlignment::Right, 0.94, 0.94, None)
}
fn basic_end_card() -> TitleData { centered_on_bg("THE END", 64.0, FontStyle::Regular, WHITE, BLACK) }
fn basic_simple_slide() -> TitleData { centered("[Your Title]", 56.0, FontStyle::Regular, WHITE, false) }

// ---------------------------------------------------------------------------
// Lower Thirds (12)
// ---------------------------------------------------------------------------

fn lt_classic() -> TitleData { lower_third_style("[Name] \u{00b7} [Title]", 36.0, FontStyle::Regular, WHITE, [0, 0, 0, 180], 0.15, 0.85) }
fn lt_minimal_bar() -> TitleData { lower_third_style("[Interview Topic]", 24.0, FontStyle::Regular, WHITE, [200, 160, 40, 200], 0.08, 0.88) }
fn lt_interview() -> TitleData { lower_third_style("[Guest Name]\n[Guest Title]", 28.0, FontStyle::Regular, WHITE, [0, 0, 0, 200], 0.12, 0.84) }
fn lt_news_banner() -> TitleData { lower_third_style("BREAKING NEWS", 32.0, FontStyle::Bold, WHITE, [180, 20, 20, 220], 0.05, 0.88) }
fn lt_documentary() -> TitleData { lower_third_style("[Subject Name]", 30.0, FontStyle::Italic, WHITE_DIM, [40, 40, 50, 200], 0.12, 0.85) }
fn lt_location() -> TitleData { lower_third_style("[City, Country]", 26.0, FontStyle::Regular, WHITE, [0, 0, 0, 170], 0.1, 0.87) }
fn lt_date_stamp() -> TitleData { lower_third_style("[Month Day, Year]", 22.0, FontStyle::Regular, WHITE_DIM, [0, 0, 0, 160], 0.1, 0.9) }
fn lt_social_handle() -> TitleData { lower_third_style("@[username]", 28.0, FontStyle::Bold, CYAN, [0, 0, 0, 180], 0.1, 0.87) }
fn lt_quote_attribution() -> TitleData { lower_third_style("\u{2014} [Speaker Name]", 24.0, FontStyle::Italic, WHITE_DIM, [0, 0, 0, 150], 0.15, 0.88) }
fn lt_podcast_guest() -> TitleData { lower_third_style("[Guest Name]\n[Podcast Title]", 30.0, FontStyle::Regular, WHITE, [60, 20, 120, 200], 0.1, 0.84) }
fn lt_corporate() -> TitleData { lower_third_style("[Full Name]\n[Job Title, Company]", 28.0, FontStyle::Regular, WHITE, [30, 50, 80, 210], 0.12, 0.84) }
fn lt_broadcast() -> TitleData { lower_third_style("[REPORTER NAME]", 30.0, FontStyle::Bold, WHITE, [0, 60, 140, 220], 0.08, 0.86) }

// ---------------------------------------------------------------------------
// Cinematic (12)
// ---------------------------------------------------------------------------

fn cin_chapter_card() -> TitleData { centered_on_bg("[PART I]", 64.0, FontStyle::Regular, GOLD, [20, 20, 25, 240]) }
fn cin_opening_title() -> TitleData { centered("[Film Title]", 96.0, FontStyle::Bold, WHITE, true) }
fn cin_film_credits() -> TitleData { centered("Written and Directed by\n[Your Name]", 42.0, FontStyle::Regular, WHITE, false) }
fn cin_directors_credit() -> TitleData { centered("A FILM BY\n[Director Name]", 48.0, FontStyle::Regular, WHITE_DIM, false) }
fn cin_dramatic_reveal() -> TitleData { centered("[Title]", 84.0, FontStyle::Bold, WHITE, true) }
fn cin_movie_poster() -> TitleData {
    positioned("[MOVIE TITLE]", 90.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, shadow_hard())
}
fn cin_scene_location() -> TitleData {
    positioned("[CITY, COUNTRY]\n[Year]", 36.0, FontStyle::Regular, WHITE, TextAlignment::Left, 0.08, 0.88, shadow_default())
}
fn cin_retro_title() -> TitleData {
    positioned("RETRO", 72.0, FontStyle::Bold, [255, 220, 150, 255], TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn cin_typewriter() -> TitleData {
    with_anim(
        positioned("typewriter_text", 36.0, FontStyle::Regular, [200, 200, 180, 255], TextAlignment::Left, 0.15, 0.5, None),
        TitleAnimationType::Typewriter { chars_per_sec: 12.0 },
    )
}
fn cin_glitch() -> TitleData {
    // Real glitch: cyan/magenta channel offset + slice displacement +
    // distinctive red colour. The CRT/VHS look in one preset.
    let td = positioned("GL1TCH", 72.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, Some(TitleShadow { color: [0, 255, 255, 120], offset_x: 4.0, offset_y: -2.0, blur: 2.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() }));
    let td = with_chromatic(td, 6.0);
    with_glitch(td, 0.35)
}
fn cin_minimal_cinematic() -> TitleData { centered("m i n i m a l", 36.0, FontStyle::Regular, WHITE_DIM, false) }
fn cin_epic() -> TitleData {
    positioned("EPIC TITLE", 96.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, shadow_hard())
}

// ---------------------------------------------------------------------------
// Social Media (10)
// ---------------------------------------------------------------------------

fn sm_youtube() -> TitleData {
    positioned("WATCH THIS!", 72.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.35, shadow_hard())
}
fn sm_instagram() -> TitleData { centered("Instagram Story", 48.0, FontStyle::Bold, WHITE, true) }
fn sm_tiktok() -> TitleData {
    positioned("TikTok Title", 64.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.4, shadow_hard())
}
fn sm_twitter() -> TitleData {
    positioned("\u{201c}Tweet goes here\u{201d}", 36.0, FontStyle::Regular, WHITE, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sm_thumbnail() -> TitleData {
    positioned("THUMBNAIL", 96.0, FontStyle::Bold, [255, 255, 0, 255], TextAlignment::Center, 0.5, 0.5, shadow_hard())
}
fn sm_subscribe_cta() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "SUBSCRIBE to [Channel Name]".into();
    tb.font_size = 42.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [200, 20, 20, 230], padding: 16.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.8 }; }
    td
}
}
fn sm_like_share() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "LIKE & SHARE".into();
    tb.font_size = 36.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [40, 100, 200, 220], padding: 12.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.85 }; }
    td
}
}
fn sm_hashtag() -> TitleData {
    positioned("#[hashtag]", 48.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sm_comment() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "[Your comment here...]".into();
    tb.font_size = 28.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Regular;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Left;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [50, 50, 60, 200], padding: 14.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.15, y: 0.75 }; }
    td
}
}
fn sm_poll() -> TitleData {
    positioned("What do you think?", 42.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.3, shadow_default())
}

// ---------------------------------------------------------------------------
// Animated Intros (15) — all have baked-in entrance animations
// ---------------------------------------------------------------------------

// Animated Intros — every factory now wires its TitleAnimationType
// so picking the template actually animates on playback / export.
fn anim_fade_up() -> TitleData { with_anim(centered("Fade Up", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::FadeIn { duration_secs: 0.5 }) }
fn anim_slide_left() -> TitleData { with_anim(centered("Slide Left", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::SlideInLeft { duration_secs: 0.5 }) }
fn anim_slide_right() -> TitleData { with_anim(centered("Slide Right", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::SlideInRight { duration_secs: 0.5 }) }
fn anim_drop_down() -> TitleData { with_anim(centered("Drop Down", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::SlideInTop { duration_secs: 0.5 }) }
fn anim_rise_up() -> TitleData { with_anim(centered("Rise Up", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::SlideInBottom { duration_secs: 0.5 }) }
fn anim_zoom_in() -> TitleData { with_anim(centered("Zoom In", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::ZoomIn { duration_secs: 0.6, from_scale: 0.5 }) }
fn anim_grow_fade() -> TitleData { with_anim(centered("Grow & Fade", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::ZoomIn { duration_secs: 0.6, from_scale: 0.4 }) }
fn anim_wipe_in() -> TitleData { with_anim(centered("Wipe In", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::SlideInLeft { duration_secs: 0.3 }) }
fn anim_elegant_fade() -> TitleData { with_anim(centered("Elegant Fade", 56.0, FontStyle::Regular, WHITE_DIM, false), TitleAnimationType::FadeIn { duration_secs: 2.0 }) }
// Removed (no underlying impl or duplicate):
//   anim_pop_in (collapsed into Motion's "Bounce In")
//   anim_fade_slide_left / _right / _rise / _drop (dupes of plain slide variants)
//   anim_bounce_in (collapsed into Motion's "Bounce In")

// ---------------------------------------------------------------------------
// Animated Outros (10) — all have exit animations
// ---------------------------------------------------------------------------

// Animated Outros — wired through to FadeOut / ZoomOut.
fn anim_fade_out() -> TitleData { with_anim(centered("Fade Out", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::FadeOut { duration_secs: 0.5 }) }
fn anim_shrink_out() -> TitleData { with_anim(centered("Shrink Out", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::ZoomOut { duration_secs: 0.5, to_scale: 0.05 }) }
fn anim_fade_out_slow() -> TitleData { with_anim(centered("Fade Out Slow", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::FadeOut { duration_secs: 2.0 }) }
fn anim_pop_out() -> TitleData { with_anim(centered("Pop Out", 64.0, FontStyle::Bold, WHITE, true), TitleAnimationType::ZoomOut { duration_secs: 0.5, to_scale: 1.5 }) }
fn anim_dissolve() -> TitleData { with_anim(centered("Dissolve", 56.0, FontStyle::Regular, WHITE_DIM, false), TitleAnimationType::FadeOut { duration_secs: 1.5 }) }
fn anim_quick_exit() -> TitleData { with_anim(centered("Quick Exit", 64.0, FontStyle::Bold, WHITE, true), TitleAnimationType::FadeOut { duration_secs: 0.3 }) }
// Removed (no underlying impl):
//   anim_slide_out_left / _right (no SlideOut* variants in the core engine)
//   anim_rise_fade / anim_drop_fade (combo animations not representable as a single TitleAnimationType)

// ---------------------------------------------------------------------------
// Full Animations (10) — entrance + exit
// ---------------------------------------------------------------------------

// Full Animations — only the variants representable as a single
// TitleAnimationType remain. Combo animations (Slide-in then Slide-
// out, Pop-in then Fade-out, etc.) need separate Set commands or a
// future "AnimationChain" type and are tracked elsewhere.
fn anim_fade_in_out() -> TitleData { with_anim(centered("Fade In & Out", 64.0, FontStyle::Regular, WHITE, true), TitleAnimationType::FadeInOut { fade_secs: 0.5 }) }
fn anim_gentle_in_out() -> TitleData { with_anim(centered("Gentle In & Out", 56.0, FontStyle::Regular, WHITE_DIM, false), TitleAnimationType::FadeInOut { fade_secs: 2.0 }) }
fn anim_quick_flash() -> TitleData { with_anim(centered("Quick Flash", 72.0, FontStyle::Bold, WHITE, true), TitleAnimationType::FadeInOut { fade_secs: 0.3 }) }
// Removed (combo animations needing two TitleAnimationType slots):
//   anim_slide_lr / anim_pop_fade / anim_rise_hold_drop / anim_zoom_in_out
//   anim_fade_slide_both / anim_dramatic_fade / anim_smooth_cycle

// ---------------------------------------------------------------------------
// Broadcast (10)
// ---------------------------------------------------------------------------

fn bc_breaking_news() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "BREAKING NEWS".into();
    tb.font_size = 48.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [200, 20, 20, 230], padding: 16.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.12 }; }
    td
}
}
fn bc_news_ticker() -> TitleData { lower_third_style("Breaking: [Story details here...]", 26.0, FontStyle::Regular, WHITE, [20, 20, 80, 220], 0.05, 0.94) }
fn bc_weather() -> TitleData { lower_third_style("[City]  [Temp]\u{00b0}F", 28.0, FontStyle::Regular, WHITE, [0, 80, 160, 210], 0.08, 0.87) }
fn bc_score_bug() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "[HOME] 0 - 0 [AWAY]".into();
    tb.font_size = 28.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 10.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.06 }; }
    td
}
}
fn bc_reporter() -> TitleData { lower_third_style("[Reporter Name]\n[City Bureau]", 28.0, FontStyle::Bold, WHITE, [0, 60, 140, 220], 0.08, 0.84) }
fn bc_program_title() -> TitleData { centered_on_bg("[PROGRAM TITLE]", 56.0, FontStyle::Bold, WHITE, [20, 30, 60, 240]) }
fn bc_live_badge() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "LIVE".into();
    tb.font_size = 24.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [220, 20, 20, 240], padding: 8.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.92, y: 0.06 }; }
    td
}
}
fn bc_time_zone() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "12:00 PM EST".into();
    tb.font_size = 22.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Regular;
    tb.color = WHITE_DIM;
    tb.alignment = TextAlignment::Right;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [0, 0, 0, 150], padding: 6.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.94, y: 0.94 }; }
    td
}
}
fn bc_network_bug() -> TitleData {
    positioned("NET", 18.0, FontStyle::Bold, [255, 255, 255, 120], TextAlignment::Right, 0.95, 0.05, None)
}
fn bc_alert() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "SEVERE WEATHER ALERT".into();
    tb.font_size = 36.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [180, 40, 0, 230], padding: 18.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.5 }; }
    td
}
}

// ---------------------------------------------------------------------------
// Educational (8)
// ---------------------------------------------------------------------------

fn edu_lesson() -> TitleData { centered_on_bg("Lesson [#]: [Topic]", 48.0, FontStyle::Bold, WHITE, [30, 30, 60, 240]) }
fn edu_key_point() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "Key Point: [Important fact here]".into();
    tb.font_size = 36.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = [255, 255, 100, 255];
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [0, 0, 0, 180], padding: 16.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.5 }; }
    td
}
}
fn edu_definition() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "[Term]: [Definition goes here]".into();
    tb.font_size = 32.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Regular;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Left;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [20, 60, 80, 200], padding: 14.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.15, y: 0.5 }; }
    td
}
}
fn edu_step() -> TitleData { centered_on_bg("Step [#]: [Description]", 42.0, FontStyle::Bold, WHITE, [40, 60, 40, 240]) }
fn edu_formula() -> TitleData {
    positioned("E = mc\u{00b2}", 48.0, FontStyle::Regular, GREEN, TextAlignment::Center, 0.5, 0.5, shadow_default())
}
fn edu_annotation() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "Note: See details".into();
    tb.font_size = 22.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Italic;
    tb.color = [255, 255, 200, 255];
    tb.alignment = TextAlignment::Right;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [0, 0, 0, 140], padding: 8.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.92, y: 0.15 }; }
    td
}
}
fn edu_summary() -> TitleData { centered_on_bg("Summary", 48.0, FontStyle::Bold, WHITE, [50, 30, 80, 240]) }
fn edu_quiz() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = "[Your question here?]".into();
    tb.font_size = 42.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = WHITE;
    tb.alignment = TextAlignment::Center;
    tb.shadow = None;
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [80, 40, 160, 210], padding: 16.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.35 }; }
    td
}
}

// ---------------------------------------------------------------------------
// Stylized (10)
// ---------------------------------------------------------------------------

fn sty_neon() -> TitleData {
    {
    let mut td = positioned("NEON", 72.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.glow = Some(TextGlow { color: [0, 255, 200, 255], radius: 15.0, intensity: 0.8, enabled: true, radius_keyframes: Vec::new(), intensity_keyframes: Vec::new() });
        tb.outlines.insert(0, TextOutline { color: [0, 255, 200, 255], width: 2.0, enabled: true, width_keyframes: Vec::new() });
    }
    td
}
}
fn sty_outline() -> TitleData {
    {
    let mut tb = klipp_timeline::TextBox::default();
    tb.outlines.insert(0, TextOutline { color: [255, 255, 255, 255], width: 3.0, enabled: true, width_keyframes: Vec::new() });
    tb.color = [255, 255, 255, 80];
    let td = klipp_timeline::TitleData::single_text(tb);
    td
}
}
fn sty_handwritten() -> TitleData {
    positioned("handwritten feel", 48.0, FontStyle::Italic, WHITE_DIM, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sty_stamp() -> TitleData {
    positioned("APPROVED", 72.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, shadow_hard())
}
fn sty_gradient() -> TitleData {
    {
    let mut td = positioned("GRADIENT", 72.0, FontStyle::Bold, PURPLE, TextAlignment::Center, 0.5, 0.5, shadow_glow([160, 80, 255, 150]));
    if let Some(tb) = td.primary_text_mut() {
        tb.gradient = Some(TextGradient { top_color: [255, 200, 50, 255], bottom_color: [255, 50, 50, 255], enabled: true });
    }
    td
}
}
fn sty_vintage() -> TitleData {
    positioned("Vintage", 56.0, FontStyle::Regular, [255, 220, 150, 255], TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sty_minimal_white() -> TitleData { centered("minimal", 42.0, FontStyle::Regular, WHITE, false) }
fn sty_dark_mode() -> TitleData { centered_on_bg("Dark Mode", 48.0, FontStyle::Regular, WHITE_DIM, [15, 15, 20, 250]) }
fn sty_kinetic() -> TitleData {
    positioned("KINETIC", 84.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, shadow_hard())
}
fn sty_brush() -> TitleData {
    positioned("Brushstroke", 56.0, FontStyle::BoldItalic, ORANGE, TextAlignment::Center, 0.5, 0.5, shadow_glow([255, 160, 40, 120]))
}

// ---------------------------------------------------------------------------
// Texture Fill (3) — user selects image via inspector
// ---------------------------------------------------------------------------

fn tex_fill_bold() -> TitleData {
    {
    let mut td = positioned("TEXTURE", 96.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.text = String::from("TEXTURE");
        tb.font_size = 96.0;
        tb.font_style = FontStyle::Bold;
        tb.texture_fill = None;
    }
    td
}
}

fn tex_fill_shadow() -> TitleData {
    {
    let mut td = positioned("IMAGE FILL", 72.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.text = String::from("IMAGE FILL");
        tb.font_size = 72.0;
        tb.font_style = FontStyle::Bold;
        tb.texture_fill = None;
        tb.outlines.insert(0, TextOutline { color: [255, 255, 255, 180], width: 1.0, enabled: true, width_keyframes: Vec::new() });
    }
    td
}
}

fn tex_fill_glow() -> TitleData {
    {
    let mut td = positioned("PHOTO TEXT", 84.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.text = String::from("PHOTO TEXT");
        tb.font_size = 84.0;
        tb.font_style = FontStyle::Bold;
        tb.texture_fill = None;
    }
    td
}
}

// ---------------------------------------------------------------------------
// 3D Text (5)
// ---------------------------------------------------------------------------

fn three_d_block() -> TitleData {
    {
    let mut td = positioned("BLOCK", 84.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(TextExtrusion {
            depth: 15.0,
            angle: 135.0,
            face_color: [255, 255, 255, 255],
            side_color: [80, 80, 120, 255],
            lighting: 0.6,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
    }
    td
}
}
fn three_d_gold_emboss() -> TitleData {
    {
    let mut td = positioned("EMBOSS", 72.0, FontStyle::Bold, GOLD, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(TextExtrusion {
            depth: 8.0,
            angle: 135.0,
            face_color: [0, 0, 0, 0],
            side_color: [160, 120, 40, 255],
            lighting: 0.8,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
        tb.gradient = Some(TextGradient { top_color: [255, 220, 100, 255], bottom_color: [200, 150, 50, 255], enabled: true });
    }
    td
}
}
fn three_d_deep_shadow() -> TitleData {
    {
    let mut td = positioned("DEEP", 96.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(TextExtrusion {
            depth: 30.0,
            angle: 150.0,
            face_color: [220, 50, 50, 255],
            side_color: [60, 20, 20, 255],
            lighting: 0.4,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
    }
    td
}
}
fn three_d_neon_extrude() -> TitleData {
    {
    let mut td = positioned("NEON 3D", 72.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(TextExtrusion {
            depth: 12.0,
            angle: 180.0,
            face_color: [0, 255, 200, 255],
            side_color: [0, 80, 60, 255],
            lighting: 0.7,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
        tb.glow = Some(TextGlow { color: [0, 255, 200, 255], radius: 12.0, intensity: 0.6, enabled: true, radius_keyframes: Vec::new(), intensity_keyframes: Vec::new() });
    }
    td
}
}
fn three_d_retro_stack() -> TitleData {
    {
    let mut td = positioned("RETRO", 84.0, FontStyle::Bold, ORANGE, TextAlignment::Center, 0.5, 0.5, None);
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(TextExtrusion {
            depth: 20.0,
            angle: 120.0,
            face_color: [255, 220, 50, 255],
            side_color: [150, 80, 30, 255],
            lighting: 0.5,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
        tb.outlines.insert(0, TextOutline { color: [40, 30, 20, 255], width: 2.0, enabled: true, width_keyframes: Vec::new() });
    }
    td
}
}

// ---------------------------------------------------------------------------
// Wedding / Event (5)
// ---------------------------------------------------------------------------

fn wed_save_date() -> TitleData {
    positioned("Save the Date\n[Month Day, Year]", 48.0, FontStyle::Italic, GOLD, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn wed_couple_names() -> TitleData {
    positioned("[Name] & [Name]", 64.0, FontStyle::Italic, WHITE, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn wed_thank_you() -> TitleData {
    positioned("Thank You", 72.0, FontStyle::Italic, GOLD, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn wed_event_details() -> TitleData {
    centered_on_bg("[Day, Month Date]\n[Time]\n[Venue Name]", 36.0, FontStyle::Regular, WHITE, [30, 25, 40, 230])
}
fn wed_invitation() -> TitleData {
    centered_on_bg("You Are Cordially Invited", 42.0, FontStyle::Italic, GOLD, [20, 15, 30, 240])
}

// ---------------------------------------------------------------------------
// Text on Path (3)
// ---------------------------------------------------------------------------

fn path_arc_title() -> TitleData {
    {
    let mut td = centered("ARC TITLE", 56.0, FontStyle::Bold, WHITE, true);
    if let Some(tb) = td.primary_text_mut() {
        tb.text_path = Some(TextPath::Arc { radius: 250.0, start_angle: -90.0, sweep: 180.0 });
    }
    td
}
}
fn path_circle_text() -> TitleData {
    {
    let mut td = centered("CIRCLE TEXT", 48.0, FontStyle::Bold, WHITE, true);
    if let Some(tb) = td.primary_text_mut() {
        tb.text_path = Some(TextPath::Circle { radius: 200.0 });
    }
    td
}
}
fn path_wave_text() -> TitleData {
    {
    let mut td = centered("WAVE TEXT", 56.0, FontStyle::Bold, WHITE, true);
    if let Some(tb) = td.primary_text_mut() {
        tb.text_path = Some(TextPath::Wave { amplitude: 30.0, frequency: 2.0, phase: 0.0 });
    }
    td
}
}

// ---------------------------------------------------------------------------
// Per-Character Animation (2)
// ---------------------------------------------------------------------------

fn char_stagger_fade() -> TitleData {
    {
    let mut td = centered("Stagger Fade", 64.0, FontStyle::Regular, WHITE, true);
    if let Some(tb) = td.primary_text_mut() {
        tb.char_animation = Some(CharAnimation {
            stagger: 0.05,
            entrance: CharEntrance::FadeIn,
            wave_amplitude: 0.0,
            wave_speed: 0.0,
            enabled: true,
            stagger_keyframes: Vec::new(),
            wave_amplitude_keyframes: Vec::new(),
            wave_speed_keyframes: Vec::new(),
        });
    }
    td
}
}
fn char_wave_bounce() -> TitleData {
    {
    let mut td = centered("Wave Bounce", 64.0, FontStyle::Bold, WHITE, true);
    if let Some(tb) = td.primary_text_mut() {
        tb.char_animation = Some(CharAnimation {
            stagger: 0.0,
            entrance: CharEntrance::None,
            wave_amplitude: 15.0,
            wave_speed: 2.0,
            enabled: true,
            stagger_keyframes: Vec::new(),
            wave_amplitude_keyframes: Vec::new(),
            wave_speed_keyframes: Vec::new(),
        });
    }
    td
}
}

// ---------------------------------------------------------------------------
// Master template array
// ---------------------------------------------------------------------------

const TITLES: &[TitleTemplate] = &[
    // ---- Basic (10) ----
    TitleTemplate { name: "Plain Title", category: "Basic", preview_text: "[Your Title]", create_data: basic_plain_title, animation: None },
    TitleTemplate { name: "Subtitle", category: "Basic", preview_text: "[Subtitle text]", create_data: basic_subtitle, animation: None },
    TitleTemplate { name: "Credits", category: "Basic", preview_text: "DIRECTED BY", create_data: basic_credits, animation: None },
    TitleTemplate { name: "Big Bold", category: "Basic", preview_text: "[Your Title]", create_data: basic_big_bold, animation: None },
    TitleTemplate { name: "Small Caption", category: "Basic", preview_text: "[Caption text]", create_data: basic_small_caption, animation: None },
    TitleTemplate { name: "Centered Quote", category: "Basic", preview_text: "\u{201c}Quote\u{201d}", create_data: basic_centered_quote, animation: None },
    TitleTemplate { name: "Label", category: "Basic", preview_text: "[LABEL]", create_data: basic_label, animation: None },
    TitleTemplate { name: "Watermark", category: "Basic", preview_text: "[WATERMARK]", create_data: basic_watermark, animation: None },
    TitleTemplate { name: "End Card", category: "Basic", preview_text: "THE END", create_data: basic_end_card, animation: None },
    TitleTemplate { name: "Simple Slide", category: "Basic", preview_text: "[Your Title]", create_data: basic_simple_slide, animation: None },

    // ---- Lower Thirds (12) ----
    TitleTemplate { name: "Classic Lower Third", category: "Lower Thirds", preview_text: "[Name] \u{00b7} [Title]", create_data: lt_classic, animation: None },
    TitleTemplate { name: "Minimal Bar", category: "Lower Thirds", preview_text: "[Interview]", create_data: lt_minimal_bar, animation: None },
    TitleTemplate { name: "Interview", category: "Lower Thirds", preview_text: "[Guest Name]", create_data: lt_interview, animation: None },
    TitleTemplate { name: "News Banner", category: "Lower Thirds", preview_text: "BREAKING NEWS", create_data: lt_news_banner, animation: None },
    TitleTemplate { name: "Documentary", category: "Lower Thirds", preview_text: "[Subject]", create_data: lt_documentary, animation: None },
    TitleTemplate { name: "Location Tag", category: "Lower Thirds", preview_text: "[City, Country]", create_data: lt_location, animation: None },
    TitleTemplate { name: "Date Stamp", category: "Lower Thirds", preview_text: "[Month Day]", create_data: lt_date_stamp, animation: None },
    TitleTemplate { name: "Social Handle", category: "Lower Thirds", preview_text: "@[username]", create_data: lt_social_handle, animation: None },
    TitleTemplate { name: "Quote Attribution", category: "Lower Thirds", preview_text: "\u{2014} [Speaker]", create_data: lt_quote_attribution, animation: None },
    TitleTemplate { name: "Podcast Guest", category: "Lower Thirds", preview_text: "[Guest Name]", create_data: lt_podcast_guest, animation: None },
    TitleTemplate { name: "Corporate", category: "Lower Thirds", preview_text: "[Full Name]", create_data: lt_corporate, animation: None },
    TitleTemplate { name: "Broadcast", category: "Lower Thirds", preview_text: "[REPORTER]", create_data: lt_broadcast, animation: None },

    // ---- Cinematic (12) ----
    TitleTemplate { name: "Chapter Card", category: "Cinematic", preview_text: "[PART I]", create_data: cin_chapter_card, animation: None },
    TitleTemplate { name: "Opening Title", category: "Cinematic", preview_text: "[Film Title]", create_data: cin_opening_title, animation: None },
    TitleTemplate { name: "Film Credits", category: "Cinematic", preview_text: "Written by...", create_data: cin_film_credits, animation: None },
    TitleTemplate { name: "Director's Credit", category: "Cinematic", preview_text: "A FILM BY", create_data: cin_directors_credit, animation: None },
    TitleTemplate { name: "Dramatic Reveal", category: "Cinematic", preview_text: "[Title]", create_data: cin_dramatic_reveal, animation: None },
    TitleTemplate { name: "Movie Poster", category: "Cinematic", preview_text: "[MOVIE TITLE]", create_data: cin_movie_poster, animation: None },
    TitleTemplate { name: "Scene Location", category: "Cinematic", preview_text: "[CITY, Year]", create_data: cin_scene_location, animation: None },
    TitleTemplate { name: "Retro Title", category: "Cinematic", preview_text: "RETRO", create_data: cin_retro_title, animation: None },
    TitleTemplate { name: "Typewriter", category: "Cinematic", preview_text: "typewriter_text", create_data: cin_typewriter, animation: None },
    TitleTemplate { name: "Glitch Title", category: "Cinematic", preview_text: "GL1TCH", create_data: cin_glitch, animation: None },
    TitleTemplate { name: "Minimal Cinematic", category: "Cinematic", preview_text: "m i n i m a l", create_data: cin_minimal_cinematic, animation: None },
    TitleTemplate { name: "Epic Title", category: "Cinematic", preview_text: "EPIC TITLE", create_data: cin_epic, animation: None },

    // ---- Social Media (10) ----
    TitleTemplate { name: "YouTube Title", category: "Social Media", preview_text: "WATCH THIS!", create_data: sm_youtube, animation: None },
    TitleTemplate { name: "Instagram Story", category: "Social Media", preview_text: "Instagram Story", create_data: sm_instagram, animation: None },
    TitleTemplate { name: "TikTok Style", category: "Social Media", preview_text: "TikTok Title", create_data: sm_tiktok, animation: None },
    TitleTemplate { name: "Twitter/X Post", category: "Social Media", preview_text: "\u{201c}Tweet\u{201d}", create_data: sm_twitter, animation: None },
    TitleTemplate { name: "Thumbnail Title", category: "Social Media", preview_text: "THUMBNAIL", create_data: sm_thumbnail, animation: None },
    TitleTemplate { name: "Subscribe CTA", category: "Social Media", preview_text: "SUBSCRIBE", create_data: sm_subscribe_cta, animation: None },
    TitleTemplate { name: "Like & Share", category: "Social Media", preview_text: "LIKE & SHARE", create_data: sm_like_share, animation: None },
    TitleTemplate { name: "Hashtag", category: "Social Media", preview_text: "#trending", create_data: sm_hashtag, animation: None },
    TitleTemplate { name: "Comment Highlight", category: "Social Media", preview_text: "User comment...", create_data: sm_comment, animation: None },
    TitleTemplate { name: "Poll Question", category: "Social Media", preview_text: "What do you think?", create_data: sm_poll, animation: None },

    // ---- Animated Intros (10) ----
    // Each template wires a TitleAnimationType in its create_data so
    // it actually animates on playback / export. The pre-cleanup
    // catalogue had 15 entries; "Pop In", "Bounce In" and the four
    // "Fade & Slide" variants were either duplicates of simpler
    // entries or combo animations not representable as a single
    // TitleAnimationType — those have been collapsed.
    TitleTemplate { name: "Fade Up", category: "Animated Intros", preview_text: "Fade Up", create_data: anim_fade_up, animation: Some("FadeIn(0.5s)") },
    TitleTemplate { name: "Slide from Left", category: "Animated Intros", preview_text: "Slide Left", create_data: anim_slide_left, animation: Some("SlideInLeft(0.5s)") },
    TitleTemplate { name: "Slide from Right", category: "Animated Intros", preview_text: "Slide Right", create_data: anim_slide_right, animation: Some("SlideInRight(0.5s)") },
    TitleTemplate { name: "Drop Down", category: "Animated Intros", preview_text: "Drop Down", create_data: anim_drop_down, animation: Some("SlideInTop(0.5s)") },
    TitleTemplate { name: "Rise Up", category: "Animated Intros", preview_text: "Rise Up", create_data: anim_rise_up, animation: Some("SlideInBottom(0.5s)") },
    TitleTemplate { name: "Zoom In", category: "Animated Intros", preview_text: "Zoom In", create_data: anim_zoom_in, animation: Some("ZoomIn(0.6s, 0.5\u{00d7})") },
    TitleTemplate { name: "Grow & Fade", category: "Animated Intros", preview_text: "Grow+Fade", create_data: anim_grow_fade, animation: Some("ZoomIn(0.6s, 0.4\u{00d7})") },
    TitleTemplate { name: "Wipe In", category: "Animated Intros", preview_text: "Wipe In", create_data: anim_wipe_in, animation: Some("SlideInLeft(0.3s)") },
    TitleTemplate { name: "Elegant Fade", category: "Animated Intros", preview_text: "Elegant Fade", create_data: anim_elegant_fade, animation: Some("FadeIn(2.0s)") },

    // ---- Animated Outros (6) ----
    // Wired through to FadeOut / ZoomOut. SlideOut and combo outros
    // ("Rise & Fade", "Drop & Fade") removed — their effects need
    // SlideOut* TitleAnimationType variants we don't yet ship; will
    // be re-added when those land.
    TitleTemplate { name: "Fade Out", category: "Animated Outros", preview_text: "Fade Out", create_data: anim_fade_out, animation: Some("FadeOut(0.5s)") },
    TitleTemplate { name: "Shrink Out", category: "Animated Outros", preview_text: "Shrink Out", create_data: anim_shrink_out, animation: Some("ZoomOut(0.5s, 0.05\u{00d7})") },
    TitleTemplate { name: "Fade Out Slow", category: "Animated Outros", preview_text: "Slow Fade", create_data: anim_fade_out_slow, animation: Some("FadeOut(2.0s)") },
    TitleTemplate { name: "Pop Out", category: "Animated Outros", preview_text: "Pop Out", create_data: anim_pop_out, animation: Some("ZoomOut(0.5s, 1.5\u{00d7})") },
    TitleTemplate { name: "Dissolve", category: "Animated Outros", preview_text: "Dissolve", create_data: anim_dissolve, animation: Some("FadeOut(1.5s)") },
    TitleTemplate { name: "Quick Exit", category: "Animated Outros", preview_text: "Quick Exit", create_data: anim_quick_exit, animation: Some("FadeOut(0.3s)") },

    // ---- Full Animations (3) ----
    // FadeInOut at three time scales. Slide+Slide-out / Pop+Fade /
    // Rise+Drop / Zoom+Zoom combos removed — they need a future
    // "AnimationChain" type to sequence two TitleAnimationType slots.
    TitleTemplate { name: "Fade In & Out", category: "Full Animations", preview_text: "Fade In/Out", create_data: anim_fade_in_out, animation: Some("FadeInOut(0.5s)") },
    TitleTemplate { name: "Gentle In & Out", category: "Full Animations", preview_text: "Gentle", create_data: anim_gentle_in_out, animation: Some("FadeInOut(2.0s)") },
    TitleTemplate { name: "Quick Flash", category: "Full Animations", preview_text: "Flash!", create_data: anim_quick_flash, animation: Some("FadeInOut(0.3s)") },

    // ---- Broadcast (10) ----
    TitleTemplate { name: "Breaking News", category: "Broadcast", preview_text: "BREAKING NEWS", create_data: bc_breaking_news, animation: None },
    // News ticker (Broadcast category): static lower-third placeholder.
    // The Scrolling-category "News Ticker" entry below carries the
    // CrawlLeft animation by default; users tweak direction in the
    // Inspector instead of choosing a preset per direction.
    TitleTemplate { name: "Lower-Third Ticker", category: "Broadcast", preview_text: "Breaking: Story...", create_data: bc_news_ticker, animation: None },
    TitleTemplate { name: "Weather Tag", category: "Broadcast", preview_text: "New York 72\u{00b0}F", create_data: bc_weather, animation: None },
    TitleTemplate { name: "Score Bug", category: "Broadcast", preview_text: "HOME 3-2 AWAY", create_data: bc_score_bug, animation: None },
    TitleTemplate { name: "Reporter Name", category: "Broadcast", preview_text: "REPORTER", create_data: bc_reporter, animation: None },
    TitleTemplate { name: "Program Title", category: "Broadcast", preview_text: "PROGRAM TITLE", create_data: bc_program_title, animation: None },
    TitleTemplate { name: "Live Badge", category: "Broadcast", preview_text: "LIVE", create_data: bc_live_badge, animation: None },
    TitleTemplate { name: "Time Zone", category: "Broadcast", preview_text: "12:00 PM EST", create_data: bc_time_zone, animation: None },
    TitleTemplate { name: "Network Bug", category: "Broadcast", preview_text: "NET", create_data: bc_network_bug, animation: None },
    TitleTemplate { name: "Alert Banner", category: "Broadcast", preview_text: "WEATHER ALERT", create_data: bc_alert, animation: None },

    // ---- Educational (8) ----
    TitleTemplate { name: "Lesson Title", category: "Educational", preview_text: "Lesson 1", create_data: edu_lesson, animation: None },
    TitleTemplate { name: "Key Point", category: "Educational", preview_text: "Key Point:", create_data: edu_key_point, animation: None },
    TitleTemplate { name: "Definition", category: "Educational", preview_text: "Term: Definition", create_data: edu_definition, animation: None },
    TitleTemplate { name: "Step Number", category: "Educational", preview_text: "Step 1:", create_data: edu_step, animation: None },
    TitleTemplate { name: "Formula", category: "Educational", preview_text: "E = mc\u{00b2}", create_data: edu_formula, animation: None },
    TitleTemplate { name: "Annotation", category: "Educational", preview_text: "Note: See details", create_data: edu_annotation, animation: None },
    TitleTemplate { name: "Summary", category: "Educational", preview_text: "Summary", create_data: edu_summary, animation: None },
    TitleTemplate { name: "Quiz Question", category: "Educational", preview_text: "What is...?", create_data: edu_quiz, animation: None },

    // ---- Stylized (10) ----
    TitleTemplate { name: "Neon Glow", category: "Stylized", preview_text: "NEON", create_data: sty_neon, animation: None },
    TitleTemplate { name: "Outline Only", category: "Stylized", preview_text: "OUTLINE", create_data: sty_outline, animation: None },
    TitleTemplate { name: "Handwritten", category: "Stylized", preview_text: "handwritten", create_data: sty_handwritten, animation: None },
    TitleTemplate { name: "Stamp", category: "Stylized", preview_text: "APPROVED", create_data: sty_stamp, animation: None },
    TitleTemplate { name: "Gradient Text", category: "Stylized", preview_text: "GRADIENT", create_data: sty_gradient, animation: None },
    TitleTemplate { name: "Vintage", category: "Stylized", preview_text: "Vintage", create_data: sty_vintage, animation: None },
    TitleTemplate { name: "Minimal White", category: "Stylized", preview_text: "minimal", create_data: sty_minimal_white, animation: None },
    TitleTemplate { name: "Dark Mode", category: "Stylized", preview_text: "Dark Mode", create_data: sty_dark_mode, animation: None },
    TitleTemplate { name: "Kinetic Bold", category: "Stylized", preview_text: "KINETIC", create_data: sty_kinetic, animation: None },
    TitleTemplate { name: "Brush Stroke", category: "Stylized", preview_text: "Brushstroke", create_data: sty_brush, animation: None },

    // ---- Texture Fill (3) ----
    TitleTemplate { name: "Texture Fill", category: "Texture Fill", preview_text: "TEXTURE", create_data: tex_fill_bold, animation: None },
    TitleTemplate { name: "Image + Shadow", category: "Texture Fill", preview_text: "IMAGE FILL", create_data: tex_fill_shadow, animation: None },
    TitleTemplate { name: "Photo Glow", category: "Texture Fill", preview_text: "PHOTO TEXT", create_data: tex_fill_glow, animation: None },

    // ---- 3D Text (5) ----
    TitleTemplate { name: "3D Block", category: "3D Text", preview_text: "BLOCK", create_data: three_d_block, animation: None },
    TitleTemplate { name: "Gold Emboss", category: "3D Text", preview_text: "EMBOSS", create_data: three_d_gold_emboss, animation: None },
    TitleTemplate { name: "Deep Shadow", category: "3D Text", preview_text: "DEEP", create_data: three_d_deep_shadow, animation: None },
    TitleTemplate { name: "Neon 3D", category: "3D Text", preview_text: "NEON 3D", create_data: three_d_neon_extrude, animation: None },
    TitleTemplate { name: "Retro Stack", category: "3D Text", preview_text: "RETRO", create_data: three_d_retro_stack, animation: None },

    // ---- Text on Path (3) ----
    TitleTemplate { name: "Arc Title", category: "Text on Path", preview_text: "ARC TITLE", create_data: path_arc_title, animation: None },
    TitleTemplate { name: "Circle Text", category: "Text on Path", preview_text: "CIRCLE TEXT", create_data: path_circle_text, animation: None },
    TitleTemplate { name: "Wave Text", category: "Text on Path", preview_text: "WAVE TEXT", create_data: path_wave_text, animation: None },

    // ---- Per-Character (2) ----
    TitleTemplate { name: "Stagger Fade", category: "Per-Character", preview_text: "Stagger Fade", create_data: char_stagger_fade, animation: None },
    TitleTemplate { name: "Wave Bounce", category: "Per-Character", preview_text: "Wave Bounce", create_data: char_wave_bounce, animation: None },

    // ---- Wedding / Event (5) ----
    TitleTemplate { name: "Save the Date", category: "Wedding / Event", preview_text: "Save the Date", create_data: wed_save_date, animation: None },
    TitleTemplate { name: "Couple Names", category: "Wedding / Event", preview_text: "Emma & James", create_data: wed_couple_names, animation: None },
    TitleTemplate { name: "Thank You", category: "Wedding / Event", preview_text: "Thank You", create_data: wed_thank_you, animation: None },
    TitleTemplate { name: "Event Details", category: "Wedding / Event", preview_text: "Saturday, June 15", create_data: wed_event_details, animation: None },
    TitleTemplate { name: "Invitation", category: "Wedding / Event", preview_text: "You Are Invited", create_data: wed_invitation, animation: None },
    // ---- Motion (8) ----
    // Each template wires a TitleAnimationType. Templates that
    // promised animations the engine doesn't yet support
    // (Swirl / Elastic / Swing / Explode / Implode / Float / and
    // every "X & Y" combo from the pre-cleanup catalogue) have been
    // removed — they shipped as static-only despite their names.
    TitleTemplate { name: "Spin In", category: "Motion", preview_text: "SPIN", create_data: || with_anim(centered("SPIN", 72.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::SpinIn { duration_secs: 0.8, from_degrees: -360.0 }), animation: Some("SpinIn(0.8s, -360\u{00b0})") },
    TitleTemplate { name: "Rotate In", category: "Motion", preview_text: "ROTATE", create_data: || with_anim(centered("ROTATE", 72.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::SpinIn { duration_secs: 0.8, from_degrees: 90.0 }), animation: Some("SpinIn(0.8s, 90\u{00b0})") },
    TitleTemplate { name: "Bounce In", category: "Motion", preview_text: "BOUNCE", create_data: || with_anim(centered("BOUNCE", 72.0, FontStyle::Bold, [255,255,255,255], true), TitleAnimationType::BounceIn { duration_secs: 0.6 }), animation: Some("BounceIn(0.6s)") },
    TitleTemplate { name: "Pulse", category: "Motion", preview_text: "PULSE", create_data: || with_anim(centered("PULSE", 72.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::Pulse { rate_hz: 1.0, min_opacity: 0.4 }), animation: Some("Pulse(1\u{00a0}Hz)") },
    TitleTemplate { name: "Wave", category: "Motion", preview_text: "WAVE", create_data: || with_anim(centered("WAVE", 72.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::Wave { amplitude_px: 25.0, rate_hz: 0.8 }), animation: Some("Wave(25\u{00a0}px, 0.8\u{00a0}Hz)") },
    TitleTemplate { name: "Shake", category: "Motion", preview_text: "SHAKE!", create_data: || with_anim({ let mut t = centered("SHAKE!", 64.0, FontStyle::Bold, [255,255,255,255], true); if let Some(tb) = t.primary_text_mut() { tb.color = [255, 60, 60, 255]; } t }, TitleAnimationType::Shake { amplitude_px: 12.0, rate_hz: 14.0 }), animation: Some("Shake(12\u{00a0}px, 14\u{00a0}Hz)") },
    TitleTemplate { name: "Glitch", category: "Motion", preview_text: "GL!TCH", create_data: || { let t = centered("GL!TCH", 72.0, FontStyle::Bold, [255,255,255,255], true); with_chromatic(with_glitch(t, 0.4), 5.0) }, animation: Some("Glitch + chromatic") },
    TitleTemplate { name: "Colour Cycle", category: "Motion", preview_text: "RAINBOW", create_data: || with_anim({ let mut t = centered("RAINBOW", 72.0, FontStyle::Bold, [255, 100, 100, 255], true); if let Some(tb) = t.primary_text_mut() { tb.color = [255, 100, 100, 255]; } t }, TitleAnimationType::ColorCycle { rate_hz: 0.5 }), animation: Some("ColorCycle(0.5\u{00a0}Hz)") },

    // ---- Scrolling / Continuous (5) ----
    // Two generic scroll templates: "Credits Roll" (full-frame, scroll
    // up by default) and "News Ticker" (lower-third bar, crawl left by
    // default). Users change direction (Up / Down / Left / Right) and
    // speed via the Inspector's "Scroll & Animation" section — one
    // entry per visual style instead of one per direction. The
    // continuous-motion variants stay separate because they're
    // conceptually different effects, not direction variants.
    // "Continuous Spin" removed — there's no Spin-loop variant; users
    // pick "Spin In" with a long duration if they want extended rotation.
    TitleTemplate { name: "Credits Roll", category: "Scrolling", preview_text: "Credits...", create_data: || with_anim(centered("Directed by\nJohn Doe\n\nProduced by\nJane Smith\n\nEditor\nAlex Johnson", 36.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::ScrollUp { speed_px_per_sec: 0.0 }), animation: Some("ScrollUp(Auto)") },
    TitleTemplate { name: "News Ticker", category: "Scrolling", preview_text: "\u{2190} BREAKING NEWS", create_data: || { let mut t = lower_third_style("BREAKING: Major event reported \u{2014} More details at 11", 28.0, FontStyle::Regular, [255, 255, 255, 255], [0, 0, 0, 180], 0.15, 0.85); t.background = TitleBackground::Color { color: [180, 30, 30, 220], padding: 8.0 }; with_anim(t, TitleAnimationType::Marquee { speed_px_per_sec: 200.0 }) }, animation: Some("Marquee(200\u{00a0}px/s)") },
    TitleTemplate { name: "Pulsing Text", category: "Scrolling", preview_text: "\u{2764} Pulse", create_data: || with_anim({ let mut t = centered("LIVE", 64.0, FontStyle::Regular, [255,255,255,255], true); if let Some(tb) = t.primary_text_mut() { tb.color = [255, 60, 60, 255]; } t }, TitleAnimationType::Pulse { rate_hz: 1.5, min_opacity: 0.3 }), animation: Some("Pulse(1.5\u{00a0}Hz)") },
    TitleTemplate { name: "Breathing Text", category: "Scrolling", preview_text: "~ Breathe ~", create_data: || with_anim(centered("breathe", 48.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::Pulse { rate_hz: 0.3, min_opacity: 0.6 }), animation: Some("Pulse(0.3\u{00a0}Hz)") },
    TitleTemplate { name: "Floating Text", category: "Scrolling", preview_text: "\u{2601} Float", create_data: || with_anim(centered("drifting...", 36.0, FontStyle::Regular, [255,255,255,255], true), TitleAnimationType::Wave { amplitude_px: 30.0, rate_hz: 0.4 }), animation: Some("Wave(30\u{00a0}px, 0.4\u{00a0}Hz)") },
    // ---- Rich Text (8) — pre-styled multi-color/size templates ----
    // Each template ships its rich style_runs; the few that promise
    // a fade/pop intro now wire FadeIn / BounceIn so the promised
    // effect actually happens on playback.
    TitleTemplate { name: "Name & Title", category: "Rich Text", preview_text: "Name + Title", create_data: || {
        let text = "John Smith\nCreative Director"; // 28 bytes
        let mut t = centered(text, 48.0, FontStyle::Bold, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 10, font_size: Some(48.0), font_style: Some(FontStyle::Bold), color: Some([255, 255, 255, 255]), ..StyleRun::default_for_range(0, 10) },
            StyleRun { start: 11, end: 28, font_size: Some(28.0), font_style: Some(FontStyle::Regular), color: Some([180, 180, 190, 255]), ..StyleRun::default_for_range(11, 28) },
        ]; }
        with_anim(t, TitleAnimationType::FadeIn { duration_secs: 0.5 })
    }, animation: Some("FadeIn(0.5s)") },
    TitleTemplate { name: "Highlight Word", category: "Rich Text", preview_text: "BOLD color", create_data: || {
        let text = "This is IMPORTANT text";
        let mut t = centered(text, 48.0, FontStyle::Regular, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 8, color: Some([200, 200, 210, 255]), ..StyleRun::default_for_range(0, 8) },
            StyleRun { start: 8, end: 17, font_style: Some(FontStyle::Bold), color: Some([255, 195, 50, 255]), font_size: Some(56.0), ..StyleRun::default_for_range(8, 17) },
            StyleRun { start: 17, end: 22, color: Some([200, 200, 210, 255]), ..StyleRun::default_for_range(17, 22) },
        ]; }
        t
    }, animation: None },
    TitleTemplate { name: "Duo Tone", category: "Rich Text", preview_text: "Two Colors", create_data: || {
        let text = "HELLO WORLD";
        let mut t = centered(text, 72.0, FontStyle::Bold, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 5, color: Some([80, 180, 255, 255]), ..StyleRun::default_for_range(0, 5) },
            StyleRun { start: 5, end: 11, color: Some([255, 100, 150, 255]), ..StyleRun::default_for_range(5, 11) },
        ]; }
        with_anim(t, TitleAnimationType::FadeIn { duration_secs: 0.5 })
    }, animation: Some("FadeIn(0.5s)") },
    TitleTemplate { name: "Quote + Author", category: "Rich Text", preview_text: "\"Quote\" — Author", create_data: || {
        let text = "\"The best way to predict\nthe future is to create it.\"\n\n— Abraham Lincoln"; // 72 bytes
        let author_start = text.find('\u{2014}').unwrap_or(55); // em dash
        let mut t = centered(text, 36.0, FontStyle::Italic, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: author_start, font_size: Some(36.0), font_style: Some(FontStyle::Italic), color: Some([230, 230, 240, 255]), ..StyleRun::default_for_range(0, author_start) },
            StyleRun { start: author_start, end: text.len(), font_size: Some(24.0), font_style: Some(FontStyle::Regular), color: Some([180, 160, 120, 255]), ..StyleRun::default_for_range(author_start, text.len()) },
        ]; }
        with_anim(t, TitleAnimationType::FadeIn { duration_secs: 1.0 })
    }, animation: Some("FadeIn(1.0s)") },
    TitleTemplate { name: "Big + Small", category: "Rich Text", preview_text: "BIG small", create_data: || {
        let text = "MEGA tiny";
        let mut t = centered(text, 72.0, FontStyle::Bold, [255, 255, 255, 255], false);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 4, font_size: Some(96.0), font_style: Some(FontStyle::Bold), color: Some([255, 255, 255, 255]), ..StyleRun::default_for_range(0, 4) },
            StyleRun { start: 5, end: 9, font_size: Some(28.0), font_style: Some(FontStyle::Regular), color: Some([160, 160, 170, 255]), ..StyleRun::default_for_range(5, 9) },
        ]; }
        t
    }, animation: None },
    TitleTemplate { name: "Rainbow Text", category: "Rich Text", preview_text: "Rainbow!", create_data: || {
        let text = "RAINBOW";
        let mut t = centered(text, 72.0, FontStyle::Bold, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 1, color: Some([255, 50, 50, 255]), ..StyleRun::default_for_range(0, 1) },
            StyleRun { start: 1, end: 2, color: Some([255, 150, 0, 255]), ..StyleRun::default_for_range(1, 2) },
            StyleRun { start: 2, end: 3, color: Some([255, 255, 0, 255]), ..StyleRun::default_for_range(2, 3) },
            StyleRun { start: 3, end: 4, color: Some([50, 200, 50, 255]), ..StyleRun::default_for_range(3, 4) },
            StyleRun { start: 4, end: 5, color: Some([50, 150, 255, 255]), ..StyleRun::default_for_range(4, 5) },
            StyleRun { start: 5, end: 6, color: Some([100, 50, 255, 255]), ..StyleRun::default_for_range(5, 6) },
            StyleRun { start: 6, end: 7, color: Some([200, 50, 200, 255]), ..StyleRun::default_for_range(6, 7) },
        ]; }
        with_anim(t, TitleAnimationType::BounceIn { duration_secs: 0.6 })
    }, animation: Some("BounceIn(0.6s)") },
    TitleTemplate { name: "Price Tag", category: "Rich Text", preview_text: "$9.99/mo", create_data: || {
        let text = "$9.99/month";
        let mut t = centered(text, 48.0, FontStyle::Bold, [255, 255, 255, 255], false);
        t.background = TitleBackground::Color { color: [30, 30, 40, 220], padding: 20.0 };
        if let Some(tb) = t.primary_text_mut() { tb.style_runs = vec![
            StyleRun { start: 0, end: 1, font_size: Some(36.0), color: Some([180, 180, 190, 255]), ..StyleRun::default_for_range(0, 1) },
            StyleRun { start: 1, end: 5, font_size: Some(72.0), font_style: Some(FontStyle::Bold), color: Some([255, 220, 50, 255]), ..StyleRun::default_for_range(1, 5) },
            StyleRun { start: 5, end: 11, font_size: Some(24.0), color: Some([140, 140, 150, 255]), ..StyleRun::default_for_range(5, 11) },
        ]; }
        with_anim(t, TitleAnimationType::FadeIn { duration_secs: 0.3 })
    }, animation: Some("FadeIn(0.3s)") },
    TitleTemplate { name: "Breaking + Detail", category: "Rich Text", preview_text: "BREAKING + info", create_data: || {
        let text = "BREAKING NEWS\nExplosion reported downtown, emergency services responding";
        let tlen = text.len();
        {
    let mut tb = klipp_timeline::TextBox::default();
    tb.text = text.into();
    tb.font_size = 36.0;
    tb.font_name = "sans-serif".into();
    tb.font_style = FontStyle::Bold;
    tb.color = [255, 255, 255, 255];
    tb.alignment = TextAlignment::Left;
    tb.style_runs = vec![
                StyleRun { start: 0, end: 13, font_size: Some(42.0), font_style: Some(FontStyle::Bold), color: Some([255, 255, 255, 255]), ..StyleRun::default_for_range(0, 13) },
                StyleRun { start: 14, end: tlen, font_size: Some(24.0), font_style: Some(FontStyle::Regular), color: Some([240, 220, 200, 255]), ..StyleRun::default_for_range(14, tlen) },
            ];
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::Color { color: [180, 20, 20, 220], padding: 12.0 };
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.15, y: 0.85 }; }
    with_anim(td, TitleAnimationType::SlideInLeft { duration_secs: 0.5 })
}
    }, animation: Some("SlideInLeft(0.5s)") },

    // ─────────── New Effects category — combos hard to author manually ───────────
    // These exercise multi-field combinations that would take a user
    // many Inspector clicks to reproduce from scratch.
    TitleTemplate { name: "Cyberpunk", category: "Effects", preview_text: "[CYBERPUNK]", create_data: || {
        let mut tb = klipp_timeline::TextBox::default();
        tb.text = "[CYBERPUNK]".into();
        tb.font_size = 84.0;
        tb.font_style = FontStyle::Bold;
        tb.color = [50, 255, 220, 255];
        tb.alignment = TextAlignment::Center;
        tb.outlines.push(klipp_timeline::TextOutline { color: [10, 10, 14, 220], width: 2.0, enabled: true, width_keyframes: Vec::new() });
        tb.glow = Some(klipp_timeline::TextGlow { color: [50, 255, 220, 200], radius: 14.0, intensity: 1.5, enabled: true, radius_keyframes: Vec::new(), intensity_keyframes: Vec::new() });
        tb.chromatic_aberration_px = 4.0;
        tb.glitch_intensity = 0.15;
        let mut td = klipp_timeline::TitleData::single_text(tb);
        td.animation_type = TitleAnimationType::ColorCycle { rate_hz: 0.25 };
        td
    }, animation: Some("ColorCycle + glitch + chromatic") },
    TitleTemplate { name: "Heartbeat Pulse", category: "Effects", preview_text: "\u{2764} LIVE", create_data: || {
        let mut t = centered("LIVE", 72.0, FontStyle::Bold, [255, 60, 60, 255], true);
        if let Some(tb) = t.primary_text_mut() {
            tb.outlines.push(klipp_timeline::TextOutline { color: [40, 0, 0, 220], width: 2.5, enabled: true, width_keyframes: Vec::new() });
        }
        with_anim(t, TitleAnimationType::Pulse { rate_hz: 1.2, min_opacity: 0.55 })
    }, animation: Some("Pulse(1.2\u{00a0}Hz)") },
    TitleTemplate { name: "Stock Ticker", category: "Effects", preview_text: "AAPL +1.24% \u{2014} TSLA \u{2212}0.55%", create_data: || {
        let mut t = lower_third_style(
            "AAPL +1.24% \u{2014} TSLA \u{2212}0.55% \u{2014} MSFT +0.42% \u{2014} GOOG +0.88%",
            28.0, FontStyle::Bold,
            [120, 255, 140, 255], [10, 14, 20, 230],
            0.5, 0.92,
        );
        if let Some(tb) = t.primary_text_mut() {
            tb.alignment = TextAlignment::Left;
        }
        with_anim(t, TitleAnimationType::Marquee { speed_px_per_sec: 220.0 })
    }, animation: Some("Marquee(220\u{00a0}px/s)") },
    TitleTemplate { name: "Award Reveal", category: "Effects", preview_text: "Best Picture", create_data: || {
        let mut t = centered("Best Picture", 84.0, FontStyle::Bold, [240, 200, 90, 255], true);
        if let Some(tb) = t.primary_text_mut() {
            tb.drop_cap_scale = 2.5;
            tb.glow = Some(klipp_timeline::TextGlow { color: [240, 200, 90, 180], radius: 18.0, intensity: 1.2, enabled: true, radius_keyframes: Vec::new(), intensity_keyframes: Vec::new() });
            tb.alignment = TextAlignment::Left;
        }
        if let Some(o) = t.objects.first_mut() {
            o.transform.center = TitlePosition { x: 0.15, y: 0.5 };
        }
        with_anim(t, TitleAnimationType::BounceIn { duration_secs: 0.8 })
    }, animation: Some("BounceIn + drop cap + glow") },
    TitleTemplate { name: "Subtitle Track", category: "Effects", preview_text: "[Spoken line of dialogue]", create_data: || {
        let mut t = positioned(
            "[Spoken line of dialogue]",
            32.0, FontStyle::Regular,
            [255, 255, 255, 255], TextAlignment::Center,
            0.5, 0.88,
            Some(TitleShadow { color: [0, 0, 0, 200], offset_x: 1.5, offset_y: 1.5, blur: 3.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() }),
        );
        if let Some(tb) = t.primary_text_mut() {
            tb.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 220], width: 1.5, enabled: true, width_keyframes: Vec::new() });
        }
        with_anim(t, TitleAnimationType::Typewriter { chars_per_sec: 60.0 })
    }, animation: Some("Typewriter(60 ch/s)") },
    TitleTemplate { name: "Multi-Column Article", category: "Effects", preview_text: "Lorem ipsum dolor sit amet \u{2026}", create_data: || {
        let body = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
                    tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim \
                    veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea \
                    commodo consequat.";
        let mut t = centered(body, 24.0, FontStyle::Regular, [240, 240, 240, 255], false);
        if let Some(tb) = t.primary_text_mut() {
            tb.alignment = TextAlignment::Left;
            tb.columns = 3;
            tb.line_spacing = 1.25;
        }
        t
    }, animation: None },
    TitleTemplate { name: "Vertical CJK", category: "Effects", preview_text: "\u{6587}\u{5B57}", create_data: || {
        let mut t = centered("\u{6587}\u{5B57}\u{5782}\u{76F4}", 80.0, FontStyle::Regular, [240, 240, 240, 255], true);
        if let Some(tb) = t.primary_text_mut() {
            tb.vertical_layout = true;
        }
        t
    }, animation: None },
    TitleTemplate { name: "Hebrew (RTL)", category: "Effects", preview_text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}", create_data: || {
        let mut t = centered("\u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD}", 72.0, FontStyle::Bold, [255, 255, 255, 255], true);
        if let Some(tb) = t.primary_text_mut() {
            tb.alignment = TextAlignment::Right;
            tb.text_direction = klipp_timeline::TextDirection::Rtl;
        }
        with_anim(t, TitleAnimationType::FadeIn { duration_secs: 0.6 })
    }, animation: Some("FadeIn(0.6s) + RTL") },
    TitleTemplate { name: "Countdown 10\u{2192}0", category: "Effects", preview_text: "10", create_data: || {
        let mut tb = klipp_timeline::TextBox::default();
        tb.text = "10".into();
        tb.font_size = 200.0;
        tb.font_style = FontStyle::Bold;
        tb.color = [255, 70, 70, 255];
        tb.alignment = TextAlignment::Center;
        tb.outlines.push(klipp_timeline::TextOutline { color: [40, 0, 0, 220], width: 4.0, enabled: true, width_keyframes: Vec::new() });
        let mut td = klipp_timeline::TitleData::single_text(tb);
        td.animation_type = TitleAnimationType::Counter {
            from_value: 10.0,
            to_value: 0.0,
            duration_secs: 10.0,
            decimals: 0,
        };
        td
    }, animation: Some("Counter(10\u{2192}0, 10s)") },
    TitleTemplate { name: "Drop Cap Pull Quote", category: "Effects", preview_text: "Lorem ipsum dolor sit \u{2026}", create_data: || {
        let body = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
                    tempor incididunt ut labore et dolore magna aliqua.";
        let mut t = centered(body, 32.0, FontStyle::Italic, [240, 240, 240, 255], false);
        if let Some(tb) = t.primary_text_mut() {
            tb.alignment = TextAlignment::Left;
            tb.drop_cap_scale = 3.5;
            tb.line_spacing = 1.3;
        }
        if let Some(o) = t.objects.first_mut() {
            o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        }
        t
    }, animation: None },
    TitleTemplate { name: "Neon Sign", category: "Effects", preview_text: "OPEN", create_data: || {
        let accent: [u8; 4] = [255, 60, 200, 255];
        let mut tb = klipp_timeline::TextBox::default();
        tb.text = "OPEN".into();
        tb.font_size = 120.0;
        tb.font_style = FontStyle::Bold;
        tb.color = accent;
        tb.alignment = TextAlignment::Center;
        tb.outlines.push(klipp_timeline::TextOutline { color: [10, 10, 14, 200], width: 3.0, enabled: true, width_keyframes: Vec::new() });
        tb.glow = Some(klipp_timeline::TextGlow { color: accent, radius: 22.0, intensity: 1.6, enabled: true, radius_keyframes: Vec::new(), intensity_keyframes: Vec::new() });
        let mut td = klipp_timeline::TitleData::single_text(tb);
        td.animation_type = TitleAnimationType::Pulse { rate_hz: 0.5, min_opacity: 0.7 };
        td
    }, animation: Some("Pulse(0.5\u{00a0}Hz) + neon") },
];

/// Ordered list of categories for display.
const CATEGORIES: &[&str] = &[
    "Basic",
    "Lower Thirds",
    "Cinematic",
    "Social Media",
    "Animated Intros",
    "Animated Outros",
    "Full Animations",
    "Motion",
    "Scrolling",
    "Broadcast",
    "Educational",
    "Stylized",
    "Texture Fill",
    "3D Text",
    "Text on Path",
    "Per-Character",
    "Rich Text",
    "Effects",
    "Wedding / Event",
];

const TITLES_SIDEBAR_W: f32 = 180.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut Option<&'static str>,
    preview_bg: &mut TitlesPreviewBg,
    preview_text: &mut String,
    source_filter: &mut TitlesSourceFilter,
    animated_only: &mut bool,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
    selected: &mut Option<String>,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

    // ── Split: sidebar (left) | content (right) ──
    let sidebar_w = TITLES_SIDEBAR_W.min(avail.width() * 0.40);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Sidebar ─────────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.bg_panel);
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0_f32, theme.border),
        );

        egui::ScrollArea::vertical()
            .id_salt("titles_sidebar_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                // Counts (registry-wide, ignore the active filter so
                // sidebar numbers don't dance as the user types).
                let installed_total = TITLES.len();
                // No favorites/recent persistence yet — show 0/0 stubs
                // matching the audio FX sidebar shape.
                titles_general_row(
                    ui,
                    theme,
                    Icon::Star,
                    "Favorites",
                    0,
                    false,
                    "titles_general_favourites",
                    &mut |_| {},
                );
                titles_general_row(
                    ui,
                    theme,
                    Icon::ArrowCounterClockwise,
                    "Recent",
                    0,
                    false,
                    "titles_general_recent",
                    &mut |_| {},
                );
                titles_general_row(
                    ui,
                    theme,
                    Icon::UploadSimple,
                    "Installed",
                    installed_total,
                    false,
                    "titles_general_installed",
                    &mut |_| {},
                );

                ui.add_space(8.0);
                titles_section_separator(ui, theme);
                titles_section_header(ui, theme, Some(Icon::FilmSlate), "Titles");

                // "All" pseudo-category at the top of the Titles
                // section, then each category with its registry count.
                titles_sidebar_row(
                    ui,
                    theme,
                    Some(Icon::List),
                    "All",
                    installed_total,
                    active_category.is_none(),
                    "titles_cat_all",
                    &mut |opt| *active_category = opt,
                    None,
                );
                for cat in CATEGORIES {
                    let count = TITLES.iter().filter(|t| t.category == *cat).count();
                    let is_active = *active_category == Some(*cat);
                    titles_sidebar_row(
                        ui,
                        theme,
                        Some(category_icon(cat)),
                        cat,
                        count,
                        is_active,
                        &format!("titles_cat_{cat}"),
                        &mut |opt| *active_category = opt,
                        Some(*cat),
                    );
                }
                ui.add_space(8.0);
            });
    });

    // ── Right pane ──────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());

        ui.add_space(8.0);

        // ── Preview header: PREVIEW ON [swatches] [text input] ──
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("\u{1F5BC} PREVIEW ON")
                            .size(10.0)
                            .strong()
                            .color(theme.text_dim),
                    );
                    ui.add_space(8.0);
                    preview_swatch(ui, theme, TitlesPreviewBg::Black, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::White, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::BlueGradient, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::OrangeWood, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::GreenPattern, preview_bg);
                    ui.add_space(8.0);
                    // Live preview text input — empty falls back to
                    // each template's own preview_text.
                    let edit = egui::TextEdit::singleline(preview_text)
                        .hint_text(
                            egui::RichText::new("Klipp the new Linux NLE!")
                                .color(theme.text_dim),
                        )
                        .desired_width(ui.available_width().max(120.0))
                        .font(egui::FontId::proportional(11.0))
                        .text_color(theme.text_primary);
                    ui.add(edit);
                });
            });

        ui.add_space(8.0);

        // ── Search bar ──
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                titles_search_field(ui, theme, search_query);
            });
        ui.add_space(6.0);

        // ── Filter row: All / Built-in / User + Animated only + view toggle ──
        let scoped_total = TITLES
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .count();
        // Built-in vs User: every entry is "Built-in" today (no user-
        // importable templates yet); User stays at zero so the filter
        // chip honestly reports.
        let builtin_count = scoped_total;
        let user_count: usize = 0;

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    titles_segmented_filter(
                        ui,
                        theme,
                        source_filter,
                        scoped_total,
                        builtin_count,
                        user_count,
                    );
                    ui.add_space(6.0);
                    animated_only_pill(ui, theme, animated_only);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            view_mode_button(
                                ui,
                                theme,
                                view_mode,
                                EffectViewMode::List,
                                Icon::List,
                                "List view",
                            );
                            view_mode_button(
                                ui,
                                theme,
                                view_mode,
                                EffectViewMode::Grid,
                                Icon::GridFour,
                                "Grid view",
                            );
                        },
                    );
                });
            });

        ui.add_space(8.0);

        // ── Filter the registry ──
        let search_lower = search_query.to_lowercase();
        let mut filtered: Vec<&TitleTemplate> = TITLES
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .filter(|_| match *source_filter {
                TitlesSourceFilter::All | TitlesSourceFilter::Builtin => true,
                TitlesSourceFilter::User => false, // no user templates yet
            })
            .filter(|t| !*animated_only || t.animation.is_some())
            .filter(|t| {
                if search_lower.is_empty() {
                    return true;
                }
                t.name.to_lowercase().contains(&search_lower)
                    || t.category.to_lowercase().contains(&search_lower)
            })
            .collect();
        match *sort_mode {
            EffectSort::NameAsc => {
                filtered.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            }
            EffectSort::NameDesc => {
                filtered.sort_by(|a, b| b.name.to_lowercase().cmp(&a.name.to_lowercase()));
            }
            EffectSort::Category => {
                filtered.sort_by(|a, b| {
                    a.category
                        .to_lowercase()
                        .cmp(&b.category.to_lowercase())
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            EffectSort::Source => {
                filtered.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            }
        }

        // ── Result strip: count + sort dropdown ──
        titles_result_strip(ui, theme, filtered.len(), sort_mode);
        ui.add_space(4.0);

        // ── Card grid ──
        egui::ScrollArea::vertical()
            .id_salt("titles_grid_scroll")
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if !search_query.is_empty() {
                            format!("No titles match \"{}\"", search_query)
                        } else {
                            "No titles in this category".to_string()
                        };
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    });
                    return;
                }

                let avail_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let render_grid = *view_mode == EffectViewMode::Grid;

                if render_grid {
                    let card_w = ((avail_w - spacing) / 2.0).floor();
                    let card_h = (card_w * 9.0 / 16.0).round().max(96.0);
                    egui::Grid::new("titles_grid")
                        .num_columns(2)
                        .spacing([spacing, spacing])
                        .min_col_width(card_w)
                        .show(ui, |ui| {
                            for (i, title) in filtered.iter().enumerate() {
                                let is_selected =
                                    selected.as_deref() == Some(title.name);
                                let display_text = effective_preview_text(
                                    preview_text,
                                    title,
                                );
                                // Read elapsed time from the previous
                                // frame's hover state — egui Response
                                // isn't available until after paint, so
                                // we lag by one frame (imperceptible).
                                let elapsed = read_anim_elapsed(ui, title.name);
                                let resp = title_card(
                                    ui,
                                    theme,
                                    title,
                                    card_w,
                                    card_h,
                                    *preview_bg,
                                    &display_text,
                                    is_selected,
                                    elapsed,
                                );
                                let active =
                                    resp.hovered() || is_selected;
                                update_anim_state(ui, title.name, active);
                                if active && title.animation.is_some() {
                                    ui.ctx().request_repaint();
                                }
                                handle_title_interaction(
                                    state, &mut cmds, title, &resp, selected,
                                );
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                } else {
                    for title in &filtered {
                        let is_selected =
                            selected.as_deref() == Some(title.name);
                        let display_text =
                            effective_preview_text(preview_text, title);
                        let elapsed = read_anim_elapsed(ui, title.name);
                        let resp = title_list_row(
                            ui,
                            theme,
                            title,
                            *preview_bg,
                            &display_text,
                            is_selected,
                            elapsed,
                        );
                        let active = resp.hovered() || is_selected;
                        update_anim_state(ui, title.name, active);
                        if active && title.animation.is_some() {
                            ui.ctx().request_repaint();
                        }
                        handle_title_interaction(
                            state, &mut cmds, title, &resp, selected,
                        );
                    }
                }
            });
    });

    cmds
}

/// Pick the text to render inside a card — user override beats the
/// template's hand-picked preview_text.
fn effective_preview_text<'a>(
    user_text: &'a str,
    title: &'a TitleTemplate,
) -> std::borrow::Cow<'a, str> {
    if user_text.trim().is_empty() {
        std::borrow::Cow::Borrowed(title.preview_text)
    } else {
        std::borrow::Cow::Borrowed(user_text)
    }
}

/// Shared interaction handler for both grid + list rendering — keeps
/// double-click-to-add and drag-to-timeline logic in one place.
fn handle_title_interaction(
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    title: &TitleTemplate,
    resp: &egui::Response,
    selected: &mut Option<String>,
) {
    if resp.clicked() {
        *selected = Some(title.name.to_string());
    }
    if resp.double_clicked() {
        let title_data = (title.create_data)();
        state.pending_title_data = Some(title_data);
        state.pending_title_animation = title.animation.map(|s| s.to_string());
        let ph = state.playhead;
        let track_index = state.sequences[state.active_sequence_idx]
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind == klipp_timeline::TrackKind::Text
                    && t.clip_at_timecode(ph).is_none()
            })
            .map(|(idx, _)| idx);
        let track_index = match track_index {
            Some(idx) => idx,
            None => {
                let track_num = state.sequences[state.active_sequence_idx]
                    .tracks
                    .iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Text)
                    .count()
                    + 1;
                let track = klipp_timeline::Track::new(
                    &format!("Text {track_num}"),
                    klipp_timeline::TrackKind::Text,
                );
                state.sequences[state.active_sequence_idx]
                    .tracks
                    .insert(0, track);
                0
            }
        };
        let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
        let duration_frames = fps_rate * 5;
        cmds.push(AppCommand::CreateTitleClip {
            track_index,
            timeline_in: ph,
            duration_frames,
        });
        state
            .toast
            .show(format!("\"{}\" title added to timeline", title.name));
    }
    if resp.drag_started() {
        let data = (title.create_data)();
        state.drag_drop.begin(DragPayload::Title {
            template_name: title.name.to_string(),
            title_data: data,
            animation: title.animation.map(|s| s.to_string()),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn title_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    title: &TitleTemplate,
    width: f32,
    height: f32,
    preview_bg: TitlesPreviewBg,
    preview_text_override: &str,
    is_selected: bool,
    elapsed_secs: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, height),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();
    let border_color = if is_selected {
        theme.accent
    } else if hovered {
        theme.accent_dim
    } else {
        theme.border
    };
    let stroke_w: f32 = if is_selected { 2.0 } else { 1.0 };
    let rounding = egui::Rounding::same(8.0);
    paint_preview_canvas(ui.painter(), rect, rounding, preview_bg);
    ui.painter()
        .rect_stroke(rect, rounding, egui::Stroke::new(stroke_w, border_color));

    // Inset so the rendered text doesn't sit flush against the badge
    // area at the top. The inner rect is the "frame" we treat as the
    // 0–1 normalized coordinate space for `TitleData::position`.
    let inner = rect.shrink2(egui::vec2(8.0, 16.0));
    paint_card_text(
        ui.painter(),
        inner,
        title,
        preview_text_override,
        elapsed_secs,
    );

    // Top-right BUILT-IN / USER badge (USER stub for now since the
    // registry has no user-imported templates).
    paint_corner_badge(ui, theme, rect, "BUILT-IN", BadgeKind::Builtin, true);
    // Top-left "+ ANIMATED" badge.
    if title.animation.is_some() {
        paint_corner_badge(ui, theme, rect, "+ ANIMATED", BadgeKind::Animated, false);
    }
    resp
}

// ──────────────────────────── Animation tracking ────────────────────────────

/// Read the elapsed-time-since-hover-began for `title_name`. Returns
/// 0.0 when the card is not currently hovered/selected (which kicks
/// the renderer into static-pose mode).
fn read_anim_elapsed(ui: &egui::Ui, title_name: &str) -> f32 {
    let active_id = ui.id().with(("titles_anim_active", title_name));
    let start_id = ui.id().with(("titles_anim_start", title_name));
    let now = ui.ctx().input(|i| i.time);
    let active: bool = ui
        .memory(|m| m.data.get_temp(active_id))
        .unwrap_or(false);
    if !active {
        return 0.0;
    }
    let start: f64 = ui.memory(|m| m.data.get_temp(start_id)).unwrap_or(now);
    (now - start).max(0.0) as f32
}

/// Persist the current-frame hover-or-selected state. On a rising
/// edge (false → true), records the start instant so the next frame's
/// `read_anim_elapsed` returns 0 and counts up from there.
fn update_anim_state(ui: &egui::Ui, title_name: &str, active: bool) {
    let active_id = ui.id().with(("titles_anim_active", title_name));
    let start_id = ui.id().with(("titles_anim_start", title_name));
    let now = ui.ctx().input(|i| i.time);
    let prev_active: bool = ui
        .memory(|m| m.data.get_temp(active_id))
        .unwrap_or(false);
    ui.memory_mut(|m| {
        m.data.insert_temp(active_id, active);
        if active && !prev_active {
            m.data.insert_temp(start_id, now);
        }
        if !active {
            m.data.remove::<f64>(start_id);
        }
    });
}

#[derive(Debug, Clone, Copy)]
enum BadgeKind {
    Builtin,
    Animated,
}

/// Place a small pill-shaped badge in either the top-left or top-right
/// corner of `rect`. Both badges share the dark high-contrast style so
/// they read against any preview background.
fn paint_corner_badge(
    ui: &egui::Ui,
    theme: &KlippTheme,
    rect: egui::Rect,
    label: &str,
    kind: BadgeKind,
    right_aligned: bool,
) {
    let (fg, bg) = match kind {
        BadgeKind::Builtin => (
            theme.text_secondary,
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180),
        ),
        BadgeKind::Animated => (
            theme.text_primary,
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180),
        ),
    };
    let painter = ui.painter();
    let font = egui::FontId::proportional(9.0);
    let galley = painter.layout_no_wrap(label.to_string(), font, fg);
    let pad_x = 7.0;
    let h = 17.0;
    let w = galley.size().x + pad_x * 2.0;
    let pos = if right_aligned {
        egui::pos2(rect.max.x - w - 8.0, rect.min.y + 8.0)
    } else {
        egui::pos2(rect.min.x + 8.0, rect.min.y + 8.0)
    };
    let badge_rect = egui::Rect::from_min_size(pos, egui::vec2(w, h));
    painter.rect_filled(badge_rect, egui::Rounding::same(3.0), bg);
    painter.galley(
        egui::pos2(
            badge_rect.min.x + pad_x,
            badge_rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
}

/// Compact list-mode row used when the user toggles to list view.
fn title_list_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    title: &TitleTemplate,
    preview_bg: TitlesPreviewBg,
    preview_text_override: &str,
    is_selected: bool,
    elapsed_secs: f32,
) -> egui::Response {
    let avail_w = ui.available_width();
    let row_h = 56.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();

    if is_selected {
        let bg = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            28,
        );
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), bg);
    } else if hovered {
        ui.painter().rect_filled(rect, egui::Rounding::same(6.0), theme.bg_raised);
    }

    // Mini preview on the left, text block on the right.
    let mini_w = 96.0;
    let mini_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 8.0, rect.min.y + 6.0),
        egui::vec2(mini_w, row_h - 12.0),
    );
    paint_preview_canvas(ui.painter(), mini_rect, egui::Rounding::same(4.0), preview_bg);
    let inner = mini_rect.shrink(4.0);
    paint_card_text(ui.painter(), inner, title, preview_text_override, elapsed_secs);

    let text_x = mini_rect.max.x + 12.0;
    let text_y = rect.center().y;
    ui.painter().text(
        egui::pos2(text_x, text_y - 7.0),
        egui::Align2::LEFT_CENTER,
        title.name,
        egui::FontId::proportional(12.0),
        theme.text_primary,
    );
    let mut meta = title.category.to_string();
    if title.animation.is_some() {
        meta.push_str("  \u{00B7} Animated");
    }
    ui.painter().text(
        egui::pos2(text_x, text_y + 8.0),
        egui::Align2::LEFT_CENTER,
        meta,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
    resp
}

/// Paint the chosen preview-canvas background fill. Each variant is
/// a single-pass rect/gradient/pattern — no expensive image loads,
/// since these run inside every card on every frame.
fn paint_preview_canvas(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: egui::Rounding,
    bg: TitlesPreviewBg,
) {
    match bg {
        TitlesPreviewBg::Black => {
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
        }
        TitlesPreviewBg::White => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0xe6, 0xe6, 0xe6));
        }
        TitlesPreviewBg::BlueGradient => {
            // Simple two-tone "sky" — paint base then a top-half
            // lighter strip to fake a gradient cheaply.
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x4d, 0x77, 0xab));
            let upper = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.min.y + rect.height() * 0.5),
            );
            painter.rect_filled(upper, rounding, egui::Color32::from_rgb(0x6b, 0x8d, 0xb5));
        }
        TitlesPreviewBg::OrangeWood => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x8a, 0x4c, 0x2f));
            // Vertical "grain" stripes — quick visual hint.
            let stripe_w = 6.0;
            let mut x = rect.min.x;
            let mut alt = false;
            while x < rect.max.x {
                let s = egui::Rect::from_min_size(
                    egui::pos2(x, rect.min.y),
                    egui::vec2(stripe_w * 0.5, rect.height()),
                );
                let s = s.intersect(rect);
                if alt {
                    painter.rect_filled(
                        s,
                        egui::Rounding::ZERO,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 30),
                    );
                }
                x += stripe_w;
                alt = !alt;
            }
        }
        TitlesPreviewBg::GreenPattern => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x2c, 0x5e, 0x3a));
            // Dotted grid — chroma-screen reference.
            let step = 8.0;
            let dot = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24);
            let mut y = rect.min.y + step * 0.5;
            while y < rect.max.y {
                let mut x = rect.min.x + step * 0.5;
                while x < rect.max.x {
                    painter.circle_filled(egui::pos2(x, y), 1.0, dot);
                    x += step;
                }
                y += step;
            }
        }
    }
}

/// Paint the title's preview inside the card. Honors the template's
/// position, background, alignment, color, shadow, and (for Rich Text
/// templates) per-character style runs. Applies a small subset of
/// animation transforms when `elapsed_secs > 0` so hovered/selected
/// cards show the actual movement of an animated template.
///
/// Note: we don't re-run the full klipp-title renderer for every card
/// every frame — that would be expensive. We approximate the look so
/// the user can scan the library and recognise styles at a glance.
fn paint_card_text(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &TitleTemplate,
    preview_text_override: &str,
    elapsed_secs: f32,
) {
    let data = (title.create_data)();

    // Decide whether to render the user-supplied preview text or the
    // template's hand-crafted preview_text. For Rich-Text templates
    // with style runs, we always use the original text since the runs
    // are pinned to character indices in that string.
    let use_template_text = !data.style_runs().is_empty();
    let display_text_raw: String = if use_template_text {
        data.text().clone()
    } else if !preview_text_override.trim().is_empty() {
        preview_text_override.to_string()
    } else {
        title.preview_text.to_string()
    };

    // Translate the title's normalized 0–1 position into card pixels.
    let data_pos = data.position();
    let pos_unanimated = egui::pos2(
        rect.min.x + data_pos.x * rect.width(),
        rect.min.y + data_pos.y * rect.height(),
    );
    let align = match data.alignment() {
        TextAlignment::Left => egui::Align2::LEFT_CENTER,
        TextAlignment::Right => egui::Align2::RIGHT_CENTER,
        TextAlignment::Center => egui::Align2::CENTER_CENTER,
    };

    // Compute animation transform from the elapsed time.
    let xform = if let Some(anim) = title.animation {
        animation_transform(anim, elapsed_secs, rect.width(), rect.height())
    } else {
        TextTransform::identity()
    };

    let pos = pos_unanimated + xform.offset;
    let font_size = ((data.font_size() / 5.0) * xform.scale).clamp(8.0, 28.0);

    // Apply animation_type (Typewriter / Scroll / Crawl) on hover. On
    // a static card we render the full text in its anchor pose; while
    // hovered, these modify either the visible character count or the
    // anchor position.
    let (display_text, type_pos, anim_alpha, anim_scale, anim_color_override) = apply_animation_type(
        &data,
        &display_text_raw,
        rect,
        pos,
        font_size,
        elapsed_secs,
    );
    let pos = type_pos;
    // Multiply animation alpha/scale on top of the text-style xform so
    // fade/flash/zoom previews also respect any underlying card-level
    // transition (e.g. card hover-in alpha).
    let xform = TextTransform {
        alpha: xform.alpha * anim_alpha,
        offset: xform.offset,
        scale: xform.scale * anim_scale,
    };
    let font_size = ((data.font_size() / 5.0) * xform.scale).clamp(8.0, 28.0);
    // Resolve the effective fill colour: ColorCycle / ColorBlend
    // animations supply a per-frame override; everything else uses
    // the static authored colour.
    let effective_color = anim_color_override.unwrap_or_else(|| data.color());

    // ── 1. Background ──
    paint_title_background(
        painter,
        rect,
        &data,
        align,
        pos,
        font_size,
        &display_text,
        xform.alpha,
    );

    // ── 2. Glow (soft outer halo, multi-pass) ──
    if let Some(ref glow) = data.glow() {
        paint_glow(painter, pos, align, font_size, &display_text, glow, xform.alpha);
    }

    // ── 3. Extrusion (3D depth — repeated offset draws) ──
    if let Some(ref ext) = data.extrusion() {
        paint_extrusion(
            painter,
            pos,
            align,
            font_size,
            &display_text,
            ext,
            effective_color,
            xform.alpha,
        );
    }

    // ── 4. Outline(s) — multi-layer stroke behind main fill ──
    let outlines = data.outlines();
    for outline in &outlines {
        paint_outline(painter, pos, align, font_size, &display_text, outline, xform.alpha);
    }

    // ── 5. Shadow ──
    if let Some(ref shadow) = data.shadow() {
        let shadow_color = scaled_color(shadow.color, xform.alpha);
        // For Bold weight we paint shadow twice (slightly thicker).
        let bold = matches!(data.font_style(), FontStyle::Bold | FontStyle::BoldItalic);
        let offsets: &[(f32, f32)] = if bold {
            &[(0.0, 0.0), (0.5, 0.0)]
        } else {
            &[(0.0, 0.0)]
        };
        for (dx, dy) in offsets {
            painter.text(
                egui::pos2(
                    pos.x + shadow.offset_x * 0.4 + dx,
                    pos.y + shadow.offset_y * 0.4 + dy,
                ),
                align,
                &display_text,
                egui::FontId::proportional(font_size),
                shadow_color,
            );
        }
    }

    // ── 6. Main fill (gradient / pattern / style runs / per-char anim / text path) ──
    if let Some(ref path) = data.text_path() {
        paint_text_along_path(
            painter,
            rect,
            &display_text,
            font_size,
            effective_color,
            path,
            xform.alpha,
            &data.font_style(),
        );
    } else if let Some(ref ca) = data.char_animation() {
        paint_char_animated(
            painter,
            pos,
            align,
            font_size,
            &display_text,
            effective_color,
            ca,
            elapsed_secs,
            xform.alpha,
            &data.font_style(),
        );
    } else if !data.style_runs().is_empty() {
        paint_styled_runs(painter, pos, align, font_size, &data, xform.alpha);
    } else if let Some(ref grad) = data.gradient() {
        paint_gradient_text(
            painter, pos, align, font_size, &display_text, grad, xform.alpha,
            &data.font_style(),
        );
    } else {
        // Chromatic aberration preview: when the template carries
        // chromatic_aberration_px > 0, paint the text three times with
        // R / G / B colour separations offset horizontally. Cheap and
        // reads as "this template has the chromatic-glitch look" even
        // without the renderer's per-pixel split.
        let ca_px = data
            .primary_text()
            .map_or(0.0, |tb| tb.chromatic_aberration_px)
            .clamp(0.0, 40.0);
        if ca_px > 0.5 {
            // Card preview is small (~font_px/5); scale CA accordingly.
            let off = (ca_px / 5.0).max(1.0);
            let r = scaled_color([255, 60, 60, effective_color[3]], xform.alpha * 0.85);
            let g = scaled_color(effective_color, xform.alpha);
            let b = scaled_color([60, 60, 255, effective_color[3]], xform.alpha * 0.85);
            paint_styled_text(painter, egui::pos2(pos.x - off, pos.y), align, font_size, &display_text, r, &data.font_style());
            paint_styled_text(painter, egui::pos2(pos.x + off, pos.y), align, font_size, &display_text, b, &data.font_style());
            paint_styled_text(painter, pos, align, font_size, &display_text, g, &data.font_style());
        } else {
            paint_styled_text(
                painter,
                pos,
                align,
                font_size,
                &display_text,
                scaled_color(effective_color, xform.alpha),
                &data.font_style(),
            );
        }
        // Glitch preview: deterministic per-elapsed horizontal offset
        // overlaid on top of the main text. Same seed pattern as the
        // renderer's apply_glitch so card preview matches export.
        let glitch = data
            .primary_text()
            .map_or(0.0, |tb| tb.glitch_intensity)
            .clamp(0.0, 1.0);
        if glitch > 0.05 {
            let n = (elapsed_secs * 12.0) as u32;
            let seed = 0x9E37_79B9u32
                .wrapping_mul(1664525)
                .wrapping_add(n.wrapping_mul(2654435761));
            let dx = ((seed >> 16) as f32 / u16::MAX as f32 - 0.5) * 8.0 * glitch;
            // Faint duplicate of the text shifted horizontally — shows
            // the slice-displacement character of the glitch effect at
            // card scale.
            paint_styled_text(
                painter,
                egui::pos2(pos.x + dx, pos.y),
                align,
                font_size,
                &display_text,
                scaled_color(effective_color, xform.alpha * 0.5),
                &data.font_style(),
            );
        }
    }
}

/// Bold weight is approximated by painting the text twice with a 1-px
/// horizontal offset (egui's painter.text doesn't support font weights
/// without a separate font face). Italic isn't faked — it'd require a
/// shear that the painter API can't apply cheaply. The card readout is
/// "this template uses bold" rather than a pixel-perfect match.
fn paint_styled_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    color: egui::Color32,
    style: &FontStyle,
) {
    let bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    if bold {
        painter.text(
            egui::pos2(pos.x - 0.5, pos.y),
            align,
            text,
            egui::FontId::proportional(font_size),
            color,
        );
        painter.text(
            egui::pos2(pos.x + 0.5, pos.y),
            align,
            text,
            egui::FontId::proportional(font_size),
            color,
        );
    } else {
        painter.text(pos, align, text, egui::FontId::proportional(font_size), color);
    }
}

/// Outline pass — draws the text 8 times around the centre with the
/// outline colour. Width clamps at 3 px so the text doesn't blob out
/// at small card font sizes.
fn paint_outline(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    outline: &TextOutline,
    alpha_mul: f32,
) {
    let color = scaled_color(outline.color, alpha_mul);
    let w = (outline.width * 0.4).clamp(0.5, 3.0);
    for (dx, dy) in [
        (-w, -w), (0.0, -w), (w, -w),
        (-w, 0.0),            (w, 0.0),
        (-w, w),  (0.0, w),  (w, w),
    ] {
        painter.text(
            egui::pos2(pos.x + dx, pos.y + dy),
            align,
            text,
            egui::FontId::proportional(font_size),
            color,
        );
    }
}

/// Soft glow — multiple passes at a slightly larger size with low
/// alpha. Cheaper than a real Gaussian blur but reads as "glowing"
/// at thumbnail scale.
fn paint_glow(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    glow: &TextGlow,
    alpha_mul: f32,
) {
    let base_a = (glow.color[3] as f32 * glow.intensity * alpha_mul).clamp(0.0, 255.0);
    let layers = 4;
    for i in 1..=layers {
        let scale = 1.0 + (i as f32 / layers as f32) * (glow.radius * 0.04).clamp(0.05, 0.5);
        let alpha = (base_a * (1.0 - i as f32 / layers as f32) * 0.5) as u8;
        let halo_color = egui::Color32::from_rgba_unmultiplied(
            glow.color[0], glow.color[1], glow.color[2], alpha,
        );
        painter.text(
            pos,
            align,
            text,
            egui::FontId::proportional(font_size * scale),
            halo_color,
        );
    }
}

/// Pseudo-3D extrusion — paint the text repeatedly along the
/// extrusion angle with the side colour, then the front face on top.
/// Lightness tapers along the depth so the back layers fade.
fn paint_extrusion(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    ext: &TextExtrusion,
    base_color: [u8; 4],
    alpha_mul: f32,
) {
    let depth = (ext.depth * 0.4).clamp(1.0, 8.0);
    let angle_rad = ext.angle.to_radians();
    let dx = angle_rad.cos();
    let dy = angle_rad.sin();
    let layers = depth as usize;
    let side_color = if ext.side_color[3] == 0 { base_color } else { ext.side_color };
    for i in (1..=layers).rev() {
        // Lighter at the back, darker toward the front.
        let fade = 1.0 - (i as f32 / layers as f32) * 0.5;
        let layer_color = egui::Color32::from_rgba_unmultiplied(
            (side_color[0] as f32 * fade) as u8,
            (side_color[1] as f32 * fade) as u8,
            (side_color[2] as f32 * fade) as u8,
            (side_color[3] as f32 * alpha_mul) as u8,
        );
        painter.text(
            egui::pos2(pos.x + dx * i as f32, pos.y + dy * i as f32),
            align,
            text,
            egui::FontId::proportional(font_size),
            layer_color,
        );
    }
}

/// Lay out a string of characters along a circular arc. Used by
/// both `TextPath::Arc` and `TextPath::Circle` (the latter passes a
/// 360° sweep). Each char paints individually with axis-aligned
/// rotation — egui's painter can't rotate raster glyphs without
/// going through a tessellator pass, so the readable approximation
/// is "characters arranged on a curve" rather than "characters
/// rotated to follow the curve."
#[allow(clippy::too_many_arguments)]
fn paint_text_along_arc(
    painter: &egui::Painter,
    rect: egui::Rect,
    chars: &[char],
    font_size: f32,
    color: egui::Color32,
    style: &FontStyle,
    radius: f32,
    start_angle: f32,
    sweep: f32,
) {
    let r = (radius * 0.06).clamp(20.0, rect.width().min(rect.height()) * 0.35);
    let centre = rect.center();
    let step = sweep / chars.len().max(1) as f32;
    for (i, ch) in chars.iter().enumerate() {
        let angle = (start_angle + step * i as f32).to_radians();
        let pos = egui::pos2(
            centre.x + angle.cos() * r,
            centre.y + angle.sin() * r,
        );
        paint_styled_text(
            painter,
            pos,
            egui::Align2::CENTER_CENTER,
            font_size,
            &ch.to_string(),
            color,
            style,
        );
    }
}

/// Lay out characters along an Arc / Circle / Wave path. Each char is
/// painted individually with the per-position rotation approximated
/// (egui can't rotate raster text, so the chars stay axis-aligned).
fn paint_text_along_path(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    font_size: f32,
    base_color: [u8; 4],
    path: &TextPath,
    alpha_mul: f32,
    style: &FontStyle,
) {
    let color = scaled_color(base_color, alpha_mul);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    match *path {
        TextPath::Arc { radius, start_angle, sweep } => {
            paint_text_along_arc(
                painter, rect, &chars, font_size, color, style,
                radius, start_angle, sweep,
            );
        }
        TextPath::Circle { radius } => {
            paint_text_along_arc(
                painter, rect, &chars, font_size, color, style,
                radius, -90.0, 360.0,
            );
        }
        TextPath::Wave { amplitude, frequency, phase } => {
            let amp = (amplitude * 0.3).clamp(2.0, rect.height() * 0.25);
            let centre_y = rect.center().y;
            // Estimate line width by char count.
            let total_chars = chars.len() as f32;
            let approx_total_w = total_chars * font_size * 0.55;
            let start_x = rect.center().x - approx_total_w * 0.5;
            for (i, ch) in chars.iter().enumerate() {
                let frac = i as f32 / total_chars.max(1.0);
                let x = start_x + i as f32 * font_size * 0.55;
                let theta = (phase + frac * frequency * 360.0).to_radians();
                let y = centre_y + theta.sin() * amp;
                paint_styled_text(
                    painter,
                    egui::pos2(x, y),
                    egui::Align2::CENTER_CENTER,
                    font_size,
                    &ch.to_string(),
                    color,
                    style,
                );
            }
        }
    }
}

/// Per-character animation — applies CharEntrance + wave to each
/// character based on `elapsed`. Always paints the full string at
/// once when the card is static (`elapsed = 0`); on hover, characters
/// stagger in, fade in, slide in, scale in, or oscillate.
#[allow(clippy::too_many_arguments)]
fn paint_char_animated(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    base_color: [u8; 4],
    ca: &CharAnimation,
    elapsed: f32,
    alpha_mul: f32,
    style: &FontStyle,
) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    // Approximate per-char width.
    let char_w = font_size * 0.55;
    let total_w = chars.len() as f32 * char_w;
    let start_x = match align {
        egui::Align2::LEFT_CENTER => pos.x,
        egui::Align2::RIGHT_CENTER => pos.x - total_w,
        _ => pos.x - total_w * 0.5,
    };
    let stagger = ca.stagger.max(0.01);
    for (i, ch) in chars.iter().enumerate() {
        let local_t = if elapsed > 0.0 {
            ((elapsed - i as f32 * stagger) / stagger.max(0.05)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Wave oscillation (always applied if amplitude > 0).
        let wave_y = if ca.wave_amplitude > 0.0 {
            let phase = elapsed * ca.wave_speed * std::f32::consts::TAU
                + i as f32 * 0.5;
            phase.sin() * ca.wave_amplitude * 0.3
        } else {
            0.0
        };
        // Entrance pose offset.
        let (mut entrance_alpha, mut entrance_dx, mut entrance_dy, mut entrance_scale) =
            (1.0, 0.0, 0.0, 1.0);
        match ca.entrance {
            CharEntrance::None => {}
            CharEntrance::FadeIn => {
                entrance_alpha = local_t;
            }
            CharEntrance::SlideUp => {
                entrance_alpha = local_t;
                entrance_dy = (1.0 - local_t) * font_size;
            }
            CharEntrance::SlideDown => {
                entrance_alpha = local_t;
                entrance_dy = -(1.0 - local_t) * font_size;
            }
            CharEntrance::SlideLeft => {
                entrance_alpha = local_t;
                entrance_dx = (1.0 - local_t) * font_size;
            }
            CharEntrance::SlideRight => {
                entrance_alpha = local_t;
                entrance_dx = -(1.0 - local_t) * font_size;
            }
            CharEntrance::ScaleUp => {
                entrance_alpha = local_t;
                entrance_scale = local_t.max(0.05);
            }
            CharEntrance::ScaleDown => {
                // Start oversized and settle to 1.0 — the symmetric
                // counterpart to ScaleUp. Cap to avoid the first-frame
                // glyph being painted ~3× the cell size and disappearing
                // under siblings.
                entrance_alpha = local_t;
                entrance_scale = (2.0 - local_t).max(1.0).min(2.0);
            }
            CharEntrance::Pop => {
                entrance_alpha = local_t;
                entrance_scale = if local_t < 0.6 {
                    local_t / 0.6 * 1.4
                } else {
                    1.4 - (local_t - 0.6) / 0.4 * 0.4
                };
            }
            CharEntrance::BlurIn => {
                entrance_alpha = local_t;
                entrance_scale = 0.7 + 0.3 * local_t;
            }
        }
        let cx = start_x + i as f32 * char_w + char_w * 0.5 + entrance_dx;
        let cy = pos.y + wave_y + entrance_dy;
        let color = scaled_color(base_color, alpha_mul * entrance_alpha);
        paint_styled_text(
            painter,
            egui::pos2(cx, cy),
            egui::Align2::CENTER_CENTER,
            font_size * entrance_scale,
            &ch.to_string(),
            color,
            style,
        );
    }
}

/// Approximate a vertical gradient fill by drawing the text twice in
/// the top and bottom halves with the gradient endpoints, plus a
/// blended middle stripe. Real gradient text would need a glyph-mask
/// pass; this is "close enough at thumbnail scale".
fn paint_gradient_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    font_size: f32,
    text: &str,
    grad: &TextGradient,
    alpha_mul: f32,
    style: &FontStyle,
) {
    // Paint bottom colour as the base, then a top-half overlay with
    // the top colour. Crude but readable.
    let bot = scaled_color(grad.bottom_color, alpha_mul);
    paint_styled_text(painter, pos, align, font_size, text, bot, style);
    // Top half painted slightly higher with reduced alpha so the seam
    // softens — closer to a real gradient appearance.
    let top = egui::Color32::from_rgba_unmultiplied(
        grad.top_color[0],
        grad.top_color[1],
        grad.top_color[2],
        ((grad.top_color[3] as f32) * 0.85 * alpha_mul) as u8,
    );
    paint_styled_text(
        painter,
        egui::pos2(pos.x, pos.y - font_size * 0.18),
        align,
        font_size,
        text,
        top,
        style,
    );
}

/// Apply the template's animation_type to the card-preview pose. On
/// a static card we render the full text in its anchor pose; while
/// hovered, animation variants advance through a 3-second loop.
/// Returns `(text, pos, alpha_mul, scale_mul, color_override)` so the
/// caller can layer the preview pose on top of the text-style transform
/// AND substitute a per-frame colour for the colour-driven animations
/// (ColorCycle hue rotation, ColorBlend two-colour lerp).
fn apply_animation_type(
    data: &TitleData,
    text: &str,
    rect: egui::Rect,
    base_pos: egui::Pos2,
    _font_size: f32,
    elapsed: f32,
) -> (String, egui::Pos2, f32, f32, Option<[u8; 4]>) {
    if elapsed <= 0.0 {
        return (text.to_string(), base_pos, 1.0, 1.0, None);
    }
    const LOOP_SECS: f32 = 3.0;
    let t = elapsed.rem_euclid(LOOP_SECS) / LOOP_SECS;
    match &data.animation_type {
        TitleAnimationType::Static => (text.to_string(), base_pos, 1.0, 1.0, None),
        TitleAnimationType::ScrollUp { .. } => {
            let dy = rect.height() * 0.5 - rect.height() * t;
            (text.to_string(), egui::pos2(base_pos.x, base_pos.y + dy), 1.0, 1.0, None)
        }
        TitleAnimationType::ScrollDown { .. } => {
            let dy = -rect.height() * 0.5 + rect.height() * t;
            (text.to_string(), egui::pos2(base_pos.x, base_pos.y + dy), 1.0, 1.0, None)
        }
        TitleAnimationType::CrawlLeft { .. } => {
            let dx = rect.width() * 0.5 - rect.width() * t;
            (text.to_string(), egui::pos2(base_pos.x + dx, base_pos.y), 1.0, 1.0, None)
        }
        TitleAnimationType::CrawlRight { .. } => {
            let dx = -rect.width() * 0.5 + rect.width() * t;
            (text.to_string(), egui::pos2(base_pos.x + dx, base_pos.y), 1.0, 1.0, None)
        }
        TitleAnimationType::Typewriter { .. } => {
            let chars: Vec<char> = text.chars().collect();
            let visible = ((t * (chars.len() as f32 + 6.0)) as usize)
                .min(chars.len());
            let revealed: String = chars.iter().take(visible).collect();
            (revealed, base_pos, 1.0, 1.0, None)
        }
        TitleAnimationType::FadeIn { .. } => {
            let alpha = (t / 0.33).clamp(0.0, 1.0);
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::FadeOut { .. } => {
            let alpha = (1.0 - (t - 0.67) / 0.33).clamp(0.0, 1.0);
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::FadeInOut { .. } => {
            let alpha = if t < 0.25 {
                t / 0.25
            } else if t > 0.75 {
                (1.0 - t) / 0.25
            } else {
                1.0
            };
            (text.to_string(), base_pos, alpha.clamp(0.0, 1.0), 1.0, None)
        }
        TitleAnimationType::Flash { rate_hz } => {
            let rate = (*rate_hz).clamp(0.5, 8.0);
            let phase = (elapsed * rate) % 1.0;
            let alpha = if phase < 0.5 { 1.0 } else { 0.0 };
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::Blink { rate_hz, duty_cycle } => {
            let rate = (*rate_hz).clamp(0.5, 8.0);
            let phase = (elapsed * rate) % 1.0;
            let alpha = if phase < *duty_cycle { 1.0 } else { 0.0 };
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::Pulse { rate_hz, min_opacity } => {
            let rate = (*rate_hz).clamp(0.1, 4.0);
            let phase = (elapsed * rate) * std::f32::consts::TAU;
            let s = 0.5 + 0.5 * phase.sin();
            let alpha = min_opacity + (1.0 - min_opacity) * s;
            (text.to_string(), base_pos, alpha.clamp(0.0, 1.0), 1.0, None)
        }
        TitleAnimationType::ZoomIn { from_scale, .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let scale = from_scale + (1.0 - from_scale) * p;
            (text.to_string(), base_pos, 1.0, scale.clamp(0.05, 4.0), None)
        }
        TitleAnimationType::ZoomOut { to_scale, .. } => {
            let p = ((t - 0.5) / 0.5).clamp(0.0, 1.0);
            let scale = 1.0 + (to_scale - 1.0) * p;
            (text.to_string(), base_pos, 1.0, scale.clamp(0.05, 4.0), None)
        }
        TitleAnimationType::SlideInLeft { .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let dx = (1.0 - p) * -rect.width() * 0.5;
            (text.to_string(), egui::pos2(base_pos.x + dx, base_pos.y), 1.0, 1.0, None)
        }
        TitleAnimationType::SlideInRight { .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let dx = (1.0 - p) * rect.width() * 0.5;
            (text.to_string(), egui::pos2(base_pos.x + dx, base_pos.y), 1.0, 1.0, None)
        }
        TitleAnimationType::SlideInTop { .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let dy = (1.0 - p) * -rect.height() * 0.5;
            (text.to_string(), egui::pos2(base_pos.x, base_pos.y + dy), 1.0, 1.0, None)
        }
        TitleAnimationType::SlideInBottom { .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let dy = (1.0 - p) * rect.height() * 0.5;
            (text.to_string(), egui::pos2(base_pos.x, base_pos.y + dy), 1.0, 1.0, None)
        }
        TitleAnimationType::SpinIn { .. } => {
            // Browser preview can't rotate text easily; fade-in stand-
            // in so the user sees there IS animation. Real renderer
            // applies the rotation in the timeline.
            let alpha = (t / 0.5).clamp(0.0, 1.0);
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::BounceIn { .. } => {
            let p = (t / 0.5).clamp(0.0, 1.0);
            let scale = if p < 0.6 { p / 0.6 * 1.15 } else { 1.15 - (p - 0.6) / 0.4 * 0.15 };
            (text.to_string(), base_pos, 1.0, scale.clamp(0.05, 4.0), None)
        }
        TitleAnimationType::Wave { amplitude_px, rate_hz } => {
            let phase = elapsed * rate_hz * std::f32::consts::TAU;
            let dy = phase.sin() * amplitude_px * 0.5;
            (text.to_string(), egui::pos2(base_pos.x, base_pos.y + dy), 1.0, 1.0, None)
        }
        TitleAnimationType::Shake { amplitude_px, rate_hz } => {
            let n = (elapsed * rate_hz) as u32;
            let seed = 0x9E37_79B9u32
                .wrapping_mul(1664525)
                .wrapping_add(n.wrapping_mul(2654435761));
            let dx = ((seed >> 16) as f32 / u16::MAX as f32 - 0.5) * amplitude_px;
            let dy = ((seed >> 1) as f32 / u32::MAX as f32 - 0.5) * amplitude_px * 2.0;
            (text.to_string(), egui::pos2(base_pos.x + dx * 0.5, base_pos.y + dy * 0.5), 1.0, 1.0, None)
        }
        TitleAnimationType::ColorCycle { rate_hz } => {
            // Hue-rotate the resting fill colour. Saturation and value
            // are taken from the authored colour so the cycle stays in
            // the user's chosen brightness band.
            let resting = data.color();
            let (_, sat, val) = rgba_to_hsv(resting);
            let hue = (elapsed * rate_hz * 360.0).rem_euclid(360.0);
            let new_color = hsv_to_rgba(hue, sat, val, resting[3]);
            (text.to_string(), base_pos, 1.0, 1.0, Some(new_color))
        }
        TitleAnimationType::Marquee { .. } => {
            let t_loop = t.rem_euclid(1.0);
            let dx = rect.width() * 0.5 - rect.width() * t_loop;
            (text.to_string(), egui::pos2(base_pos.x + dx, base_pos.y), 1.0, 1.0, None)
        }
        TitleAnimationType::TypewriterOut { .. } => {
            let chars: Vec<char> = text.chars().collect();
            let kept = chars.len().saturating_sub(
                ((t * (chars.len() as f32 + 6.0)) as usize).min(chars.len()),
            );
            let revealed: String = chars.iter().take(kept).collect();
            (revealed, base_pos, 1.0, 1.0, None)
        }
        TitleAnimationType::Counter { from_value, to_value, .. } => {
            let v = from_value + (to_value - from_value) * t as f64;
            (format!("{v:.0}"), base_pos, 1.0, 1.0, None)
        }
        TitleAnimationType::OutlineDrawOn { .. } => {
            // The preview painter doesn't expose per-outline alpha, so
            // we emulate the draw-on as a brief fade-in over the first
            // half of the loop. Conveys "outline appears" even if the
            // real left-to-right reveal is renderer-only.
            let alpha = (t / 0.5).clamp(0.0, 1.0);
            (text.to_string(), base_pos, alpha, 1.0, None)
        }
        TitleAnimationType::ColorBlend { color_a, color_b, rate_hz } => {
            // Sinusoidal lerp between the two authored colours,
            // matching what the renderer does at playback.
            let phase = elapsed * rate_hz * std::f32::consts::TAU;
            let mix = 0.5 + 0.5 * phase.sin();
            let new_color = lerp_rgba(*color_a, *color_b, mix);
            (text.to_string(), base_pos, 1.0, 1.0, Some(new_color))
        }
    }
}

/// Linearly interpolate two RGBA-u8 colours. Used by the ColorBlend
/// hover preview to match the title renderer's blend at playback.
fn lerp_rgba(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 * (1.0 - t) + b[0] as f32 * t).round().clamp(0.0, 255.0) as u8,
        (a[1] as f32 * (1.0 - t) + b[1] as f32 * t).round().clamp(0.0, 255.0) as u8,
        (a[2] as f32 * (1.0 - t) + b[2] as f32 * t).round().clamp(0.0, 255.0) as u8,
        (a[3] as f32 * (1.0 - t) + b[3] as f32 * t).round().clamp(0.0, 255.0) as u8,
    ]
}

/// HSV → RGBA-u8. Hue in degrees [0, 360), sat / val in [0, 1].
fn hsv_to_rgba(h: f32, s: f32, v: f32, alpha: u8) -> [u8; 4] {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        alpha,
    ]
}

/// RGBA-u8 → (hue degrees, sat 0..1, val 0..1).
fn rgba_to_hsv(rgba: [u8; 4]) -> (f32, f32, f32) {
    let r = rgba[0] as f32 / 255.0;
    let g = rgba[1] as f32 / 255.0;
    let b = rgba[2] as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta < 1e-6 {
        0.0
    } else if (max - r).abs() < 1e-6 {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if (max - g).abs() < 1e-6 {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let s = if max < 1e-6 { 0.0 } else { delta / max };
    (h, s, max)
}

/// Build a `Color32` from a u8 RGBA tuple with `alpha_mul` (0–1)
/// applied to the alpha channel.
fn scaled_color(rgba: [u8; 4], alpha_mul: f32) -> egui::Color32 {
    let a = (rgba[3] as f32 * alpha_mul.clamp(0.0, 1.0)) as u8;
    egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], a)
}

#[derive(Debug, Clone, Copy)]
struct TextTransform {
    alpha: f32,
    offset: egui::Vec2,
    scale: f32,
}

impl TextTransform {
    fn identity() -> Self {
        Self {
            alpha: 1.0,
            offset: egui::Vec2::ZERO,
            scale: 1.0,
        }
    }
}

/// Cheap easing — smoothstep-ish ease-out so the in-animations don't
/// look mechanical.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Parse the animation token string and return the visual transform
/// to apply at this `elapsed`. Composite tokens (`a+b`) play
/// sequentially; the first token covers `0..d_a`, the second covers
/// `d_a..d_a+d_b`, and so on.
///
/// For an MVP we match a handful of common token prefixes and ignore
/// the rest (returning identity). The card still shows the static
/// styled pose, so unknown animations gracefully degrade.
fn animation_transform(
    anim: &str,
    elapsed: f32,
    card_w: f32,
    card_h: f32,
) -> TextTransform {
    // Loop the animation every ~3 seconds so the card keeps
    // demonstrating the effect while the user keeps the cursor on it.
    const LOOP_SECS: f32 = 3.0;
    let t_loop = elapsed.rem_euclid(LOOP_SECS);

    // Walk composite chain — segments separated by `+`. Each segment
    // is `name_durationSecs`. We interpret the FIRST matching segment
    // for the current time slice; if elapsed lands past all in-
    // animations, we hold at "fully arrived" pose.
    let segments: Vec<(&str, f32)> = anim
        .split('+')
        .map(|seg| parse_segment(seg))
        .collect();
    let mut t = t_loop;
    for (name, dur) in &segments {
        let dur = (*dur).max(0.05);
        if t < dur {
            return apply_segment(name, t / dur, card_w, card_h, dur, t);
        }
        t -= dur;
    }
    // Beyond the chain — hold in arrived state for the rest of the
    // loop; for continuous effects (ticker/scroll/spinning/floating
    // /breathing/pulsing) the segment itself handles the steady
    // motion via t/dur saturating, so this branch only matters for
    // one-shot in-animations.
    let last = segments.last().copied().unwrap_or(("", 0.0));
    apply_segment(last.0, 1.0, card_w, card_h, last.1.max(0.05), t_loop)
}

/// Split a segment like "fade_in_0.8" into ("fade_in", 0.8).
fn parse_segment(seg: &str) -> (&str, f32) {
    if let Some((name, num)) = seg.rsplit_once('_') {
        if let Ok(d) = num.parse::<f32>() {
            return (name, d);
        }
    }
    (seg, 1.0)
}

#[allow(clippy::manual_range_contains)]
fn apply_segment(
    name: &str,
    progress: f32,
    card_w: f32,
    card_h: f32,
    seg_dur: f32,
    elapsed_in_loop: f32,
) -> TextTransform {
    let p = progress.clamp(0.0, 1.0);
    let eased = ease_out(p);
    match name {
        "fade_in" => TextTransform {
            alpha: p,
            offset: egui::Vec2::ZERO,
            scale: 1.0,
        },
        "fade_out" => TextTransform {
            alpha: 1.0 - p,
            offset: egui::Vec2::ZERO,
            scale: 1.0,
        },
        "fade_in_out" => {
            // Symmetric fade across the whole loop window.
            let bell = (p * std::f32::consts::PI).sin();
            TextTransform {
                alpha: bell,
                offset: egui::Vec2::ZERO,
                scale: 1.0,
            }
        }
        "slide_left" => TextTransform {
            alpha: 1.0,
            offset: egui::vec2((1.0 - eased) * card_w * 0.6, 0.0),
            scale: 1.0,
        },
        "slide_right" => TextTransform {
            alpha: 1.0,
            offset: egui::vec2(-(1.0 - eased) * card_w * 0.6, 0.0),
            scale: 1.0,
        },
        "slide_up" | "float_up" => TextTransform {
            alpha: 1.0,
            offset: egui::vec2(0.0, (1.0 - eased) * card_h * 0.4),
            scale: 1.0,
        },
        "slide_down" => TextTransform {
            alpha: 1.0,
            offset: egui::vec2(0.0, -(1.0 - eased) * card_h * 0.4),
            scale: 1.0,
        },
        "zoom_in" => TextTransform {
            alpha: p,
            offset: egui::Vec2::ZERO,
            scale: eased,
        },
        "zoom_out" => TextTransform {
            alpha: 1.0 - p,
            offset: egui::Vec2::ZERO,
            scale: 1.0 + p * 0.5,
        },
        "bounce_in" => {
            // Slightly overshoot then settle.
            let s = if p < 0.7 {
                eased * 1.15
            } else {
                1.15 - (p - 0.7) / 0.3 * 0.15
            };
            TextTransform {
                alpha: p.min(1.0),
                offset: egui::Vec2::ZERO,
                scale: s,
            }
        }
        "spin_in" | "rotate_in" | "rotate_out" => {
            // egui's painter.text doesn't rotate; fake it with a
            // scale jitter so the user sees motion.
            TextTransform {
                alpha: p,
                offset: egui::Vec2::ZERO,
                scale: 0.5 + 0.5 * eased,
            }
        }
        "shake" => {
            let phase = elapsed_in_loop * 30.0;
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(phase.sin() * 4.0, (phase * 1.3).cos() * 3.0),
                scale: 1.0,
            }
        }
        "glitch" => {
            let phase = elapsed_in_loop * 14.0;
            let dx = phase.sin().signum() * 3.0;
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(dx, 0.0),
                scale: 1.0,
            }
        }
        "pulsing" | "pulse" => {
            let osc = (elapsed_in_loop * 4.0).sin() * 0.5 + 0.5;
            TextTransform {
                alpha: 0.5 + 0.5 * osc,
                offset: egui::Vec2::ZERO,
                scale: 1.0 + 0.05 * osc,
            }
        }
        "breathing" => {
            let osc = (elapsed_in_loop * 1.6).sin() * 0.5 + 0.5;
            TextTransform {
                alpha: 1.0,
                offset: egui::Vec2::ZERO,
                scale: 0.95 + 0.1 * osc,
            }
        }
        "floating" => {
            let osc = (elapsed_in_loop * 1.4).sin();
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(0.0, osc * 4.0),
                scale: 1.0,
            }
        }
        "spinning" => {
            // No real rotation; just oscillate scale to suggest motion.
            let osc = (elapsed_in_loop * 2.0).sin();
            TextTransform {
                alpha: 1.0,
                offset: egui::Vec2::ZERO,
                scale: 1.0 + 0.06 * osc,
            }
        }
        "ticker_left" => {
            // Continuous left scroll across the seg duration.
            let cycle = elapsed_in_loop / seg_dur.max(0.05);
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(card_w - card_w * cycle, 0.0),
                scale: 1.0,
            }
        }
        "ticker_right" => {
            let cycle = elapsed_in_loop / seg_dur.max(0.05);
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(-card_w + card_w * cycle, 0.0),
                scale: 1.0,
            }
        }
        "scroll_up" => {
            let cycle = elapsed_in_loop / seg_dur.max(0.05);
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(0.0, card_h - card_h * cycle),
                scale: 1.0,
            }
        }
        "scroll_down" => {
            let cycle = elapsed_in_loop / seg_dur.max(0.05);
            TextTransform {
                alpha: 1.0,
                offset: egui::vec2(0.0, -card_h + card_h * cycle),
                scale: 1.0,
            }
        }
        "elastic" => {
            // Damped oscillation around 1.0
            let osc = ((p * 6.0).cos() * (1.0 - p)).abs();
            TextTransform {
                alpha: 1.0,
                offset: egui::Vec2::ZERO,
                scale: 1.0 + 0.15 * osc,
            }
        }
        "implode_in" => TextTransform {
            alpha: p,
            offset: egui::Vec2::ZERO,
            scale: (1.0 - p) * 2.0 + p,
        },
        "explode_out" => TextTransform {
            alpha: (1.0 - p).max(0.0),
            offset: egui::Vec2::ZERO,
            scale: 1.0 + p * 1.5,
        },
        _ => TextTransform::identity(),
    }
}

/// Paint the title-data background behind the text. Implementations:
///   - `None`            → no-op (canvas shows through)
///   - `FullFrame`       → covers the card with the chosen colour
///   - `Color`           → tight box behind the text with padding
///   - `Gradient`        → two-tone vertical gradient
fn paint_title_background(
    painter: &egui::Painter,
    rect: egui::Rect,
    data: &TitleData,
    align: egui::Align2,
    text_pos: egui::Pos2,
    font_size: f32,
    text: &str,
    alpha_mul: f32,
) {
    match &data.background {
        TitleBackground::None => {}
        TitleBackground::FullFrame { color } => {
            painter.rect_filled(
                rect,
                egui::Rounding::ZERO,
                scaled_color(*color, alpha_mul),
            );
        }
        TitleBackground::Color { color, padding } => {
            // Measure the foreground text so we can size the
            // surrounding box. `layout_no_wrap` is cheap.
            let galley = painter.layout_no_wrap(
                text.to_string(),
                egui::FontId::proportional(font_size),
                egui::Color32::WHITE,
            );
            let pad = (*padding * 0.4).clamp(2.0, 12.0);
            let size = galley.size() + egui::vec2(pad * 2.0, pad * 2.0);
            let origin = match align {
                egui::Align2::LEFT_CENTER => {
                    egui::pos2(text_pos.x - pad, text_pos.y - size.y * 0.5)
                }
                egui::Align2::RIGHT_CENTER => egui::pos2(
                    text_pos.x - galley.size().x - pad,
                    text_pos.y - size.y * 0.5,
                ),
                _ => egui::pos2(
                    text_pos.x - size.x * 0.5,
                    text_pos.y - size.y * 0.5,
                ),
            };
            let bg_rect = egui::Rect::from_min_size(origin, size);
            painter.rect_filled(
                bg_rect,
                egui::Rounding::same(2.0),
                scaled_color(*color, alpha_mul),
            );
        }
    }
}

/// Render Rich-Text templates that use per-character `StyleRun`s —
/// each run gets its own colour and (optional) size override. The
/// runs paint left-to-right starting at the alignment-anchored x.
fn paint_styled_runs(
    painter: &egui::Painter,
    text_pos: egui::Pos2,
    align: egui::Align2,
    base_font_size: f32,
    data: &TitleData,
    alpha_mul: f32,
) {
    let chars: Vec<char> = data.text().chars().collect();
    // Default colour for any character not covered by a run.
    let default_color = scaled_color(data.color(), alpha_mul);
    // Helper: resolve (color, size) at a character index, falling back
    // to the template's defaults when the run leaves a field None.
    let style_runs = data.style_runs();
    let style_at = |idx: usize| -> (egui::Color32, f32) {
        let run = style_runs
            .iter()
            .find(|r| r.start <= idx && idx < r.end);
        match run {
            Some(r) => {
                let color = match r.color {
                    Some(c) => scaled_color(c, alpha_mul),
                    None => default_color,
                };
                // r.font_size is absolute (pixels) — convert to the
                // card-relative scale we're already using by ratio
                // against the template's base font_size.
                let size_ratio = match r.font_size {
                    Some(sz) => sz / data.font_size().max(1.0),
                    None => 1.0,
                };
                (color, size_ratio)
            }
            None => (default_color, 1.0),
        }
    };

    let mut segments: Vec<(String, f32, egui::Color32)> = Vec::new();
    let mut cursor = 0usize;
    while cursor < chars.len() {
        let (color, size_ratio) = style_at(cursor);
        // Coalesce neighbouring chars with the same style.
        let mut end = cursor + 1;
        while end < chars.len() {
            let (next_color, next_size) = style_at(end);
            if next_color != color || (next_size - size_ratio).abs() > 0.001 {
                break;
            }
            end += 1;
        }
        let seg_text: String = chars[cursor..end].iter().collect();
        segments.push((seg_text, base_font_size * size_ratio, color));
        cursor = end;
    }
    // Total width for alignment.
    let total_w: f32 = segments
        .iter()
        .map(|(s, sz, _)| {
            painter
                .layout_no_wrap(
                    s.clone(),
                    egui::FontId::proportional(*sz),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
        .sum();
    let mut x = match align {
        egui::Align2::LEFT_CENTER => text_pos.x,
        egui::Align2::RIGHT_CENTER => text_pos.x - total_w,
        _ => text_pos.x - total_w * 0.5,
    };
    let y = text_pos.y;
    for (seg, sz, color) in segments {
        let galley = painter.layout_no_wrap(
            seg,
            egui::FontId::proportional(sz),
            color,
        );
        painter.galley(
            egui::pos2(x, y - galley.size().y * 0.5),
            galley.clone(),
            color,
        );
        x += galley.size().x;
    }
}

/// Single swatch in the preview-canvas selector strip. Same visual
/// language as the main `paint_preview_canvas` so the swatch
/// previews exactly what the cards will show.
fn preview_swatch(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    bg: TitlesPreviewBg,
    selected: &mut TitlesPreviewBg,
) {
    let size = 24.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(size, size),
        egui::Sense::click(),
    );
    paint_preview_canvas(ui.painter(), rect, egui::Rounding::same(4.0), bg);
    let is_active = *selected == bg;
    let stroke = if is_active {
        egui::Stroke::new(2.0_f32, theme.accent)
    } else if resp.hovered() {
        egui::Stroke::new(1.0_f32, theme.text_primary)
    } else {
        egui::Stroke::new(1.0_f32, theme.border)
    };
    ui.painter().rect_stroke(rect, egui::Rounding::same(4.0), stroke);
    if resp.clicked() {
        *selected = bg;
    }
}

// ──────────────────────────── Sidebar widgets ────────────────────────────

#[allow(clippy::too_many_arguments)]
fn titles_sidebar_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(Option<&'static str>),
    target_cat: Option<&'static str>,
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();

    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height()));
        ui.painter().rect_filled(stripe, 0.0, theme.accent);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }

    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let label_color = if is_active || hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let icon_tint = if is_active { theme.accent } else { theme.text_dim };

    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(icon_tint);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }

    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.5),
        label_color,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        label_color,
    );

    let count_color = if is_active { theme.accent } else { theme.text_dim };
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        count_color,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        count_color,
    );

    if click_resp.clicked() || resp.clicked() {
        on_click(target_cat);
    }
}

/// Stub-only general-section row (Favorites / Recent / Installed).
/// Same visual language as `titles_sidebar_row` but without the
/// category routing — there's no persistence layer for these yet.
fn titles_general_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    _on_click: &mut dyn FnMut(()),
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();
    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }
    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = icon.image(icon_size).tint(theme.text_dim);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.5),
        theme.text_secondary,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        theme.text_secondary,
    );
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        theme.text_dim,
    );
}

fn titles_section_separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        (rect.min.x + 8.0)..=(rect.max.x - 8.0),
        rect.center().y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.add_space(4.0);
}

fn titles_section_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
        egui::Sense::hover(),
    );
    let indent = 14.0;
    let icon_size = 11.0;
    let icon_gap = 6.0;
    let label_x = if icon.is_some() {
        rect.min.x + indent + icon_size + icon_gap
    } else {
        rect.min.x + indent
    };
    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(theme.text_dim);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }
    ui.painter().text(
        egui::pos2(label_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(9.5),
        theme.text_dim,
    );
}

/// Reasonable icon hint for each title sub-category. Picks from the
/// already-bundled Phosphor set; categories without an exact match
/// fall back to a generic glyph.
fn category_icon(cat: &str) -> Icon {
    match cat {
        "Basic" => Icon::FilmSlate,
        "Lower Thirds" => Icon::FrameCorners,
        "Cinematic" => Icon::FilmStrip,
        "Social Media" => Icon::Star,
        "Animated Intros" => Icon::CaretRight,
        "Animated Outros" => Icon::CaretLeft,
        "Full Animations" => Icon::Lightning,
        "Motion" => Icon::ArrowsOut,
        "Scrolling" => Icon::SortAscending,
        "Broadcast" => Icon::SpeakerHigh,
        "Educational" => Icon::Sliders,
        "Stylized" => Icon::Eyedropper,
        "Texture Fill" => Icon::GridFour,
        "3D Text" => Icon::Diamond,
        "Text on Path" => Icon::ArrowsOut,
        "Per-Character" => Icon::List,
        "Rich Text" => Icon::Sliders,
        "Wedding / Event" => Icon::Star,
        _ => Icon::FilmSlate,
    }
}

// ──────────────────────────── Search field ────────────────────────────

fn titles_search_field(ui: &mut egui::Ui, theme: &KlippTheme, query: &mut String) {
    let h = 32.0;
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, h),
        egui::Sense::hover(),
    );
    let rounding = egui::Rounding::same(6.0);
    ui.painter().rect_filled(rect, rounding, theme.bg_panel);
    ui.painter().rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    let icon_size = 13.0;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 10.0, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = Icon::MagnifyingGlass.image(icon_size).tint(theme.text_dim);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );

    let edit_left = rect.min.x + 10.0 + icon_size + 8.0;
    let edit_right = rect.max.x - 8.0;
    let edit_rect = egui::Rect::from_min_max(
        egui::pos2(edit_left, rect.min.y),
        egui::pos2(edit_right, rect.max.y),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(edit_rect),
        |ui| {
            ui.set_clip_rect(edit_rect);
            let edit = egui::TextEdit::singleline(query)
                .hint_text(
                    egui::RichText::new("Search titles\u{2026}").color(theme.text_dim),
                )
                .desired_width(edit_rect.width())
                .font(egui::FontId::proportional(11.5))
                .text_color(theme.text_primary)
                .frame(false);
            ui.add_sized(edit_rect.size(), edit);
        },
    );
}

// ──────────────────────────── Filter row widgets ────────────────────────────

fn titles_segmented_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut TitlesSourceFilter,
    all_count: usize,
    builtin_count: usize,
    user_count: usize,
) {
    let outer_h = 26.0;
    let pad = 2.0;
    let segments = [
        (TitlesSourceFilter::All, "All", all_count),
        (TitlesSourceFilter::Builtin, "Built-in", builtin_count),
        (TitlesSourceFilter::User, "User", user_count),
    ];
    let mut seg_widths = [0.0f32; 3];
    let mut total_w = pad * 2.0;
    for (i, (_, label, count)) in segments.iter().enumerate() {
        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            theme.text_secondary,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            theme.text_dim,
        );
        let w = label_galley.size().x + 6.0 + count_galley.size().x + 22.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0;
    let (outer_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, outer_h),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(outer_rect, 5.0, theme.bg_panel);
    ui.painter().rect_stroke(
        outer_rect,
        5.0,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    let mut x = outer_rect.min.x + pad;
    for (i, (variant, label, count)) in segments.iter().enumerate() {
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x, outer_rect.min.y + pad),
            egui::vec2(seg_widths[i], outer_h - pad * 2.0),
        );
        let resp = ui.interact(
            seg_rect,
            ui.id().with(("titles_source_seg", i)),
            egui::Sense::click(),
        );
        let is_active = *filter == *variant;
        let hovered = resp.hovered();
        if is_active {
            ui.painter().rect_filled(seg_rect, 3.0, theme.bg_raised);
        } else if hovered {
            ui.painter().rect_filled(seg_rect, 3.0, theme.accent_subtle);
        }
        let label_color = if is_active || hovered {
            theme.text_primary
        } else {
            theme.text_secondary
        };
        let count_color = if is_active { theme.accent } else { theme.text_dim };
        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            label_color,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            count_color,
        );
        let inner_w = label_galley.size().x + 6.0 + count_galley.size().x;
        let lx = seg_rect.center().x - inner_w * 0.5;
        ui.painter().galley(
            egui::pos2(lx, seg_rect.center().y - label_galley.size().y * 0.5),
            label_galley.clone(),
            label_color,
        );
        ui.painter().galley(
            egui::pos2(
                lx + label_galley.size().x + 6.0,
                seg_rect.center().y - count_galley.size().y * 0.5,
            ),
            count_galley,
            count_color,
        );
        if resp.clicked() {
            *filter = *variant;
        }
        x += seg_widths[i] + 1.0;
    }
}

fn animated_only_pill(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    let label_galley = ui.painter().layout_no_wrap(
        "Animated only".to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let icon_size = 11.0;
    let pad_x = 10.0;
    let gap = 5.0;
    let w = pad_x * 2.0 + icon_size + gap + label_galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, 26.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if *active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 12.0, bg);
    ui.painter().rect_stroke(rect, 12.0, egui::Stroke::new(1.0_f32, border));
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad_x, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = Icon::Lightning.image(icon_size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    ui.painter().text(
        egui::pos2(rect.min.x + pad_x + icon_size + gap, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Animated only",
        egui::FontId::proportional(11.0),
        fg,
    );
    if resp.clicked() {
        *active = !*active;
    }
}

fn view_mode_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    current: &mut EffectViewMode,
    target: EffectViewMode,
    icon: Icon,
    tooltip: &str,
) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(26.0, 26.0),
        egui::Sense::click(),
    );
    let is_active = *current == target;
    let hovered = resp.hovered();
    let (bg, border, fg) = if is_active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let size = 13.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let img = icon.image(size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    let _ = resp.clone().on_hover_text(tooltip);
    if resp.clicked() {
        *current = target;
    }
}

// ──────────────────────────── Result strip ────────────────────────────

fn titles_result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 24.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} Titles", count),
        egui::FontId::proportional(10.5),
        theme.text_secondary,
    );

    let dropdown_w = 116.0;
    let dropdown_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - dropdown_w - 8.0, rect.min.y + 1.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 1.0),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(dropdown_rect),
        |ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                egui::Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            let label = format!("Sort: {}", sort_mode.label());
            egui::ComboBox::from_id_salt("titles_sort_dd")
                .selected_text(
                    egui::RichText::new(label).size(10.5).color(theme.text_dim),
                )
                .width(dropdown_w - 4.0)
                .show_ui(ui, |ui| {
                    for variant in [
                        EffectSort::NameAsc,
                        EffectSort::NameDesc,
                        EffectSort::Category,
                    ] {
                        ui.selectable_value(sort_mode, variant, variant.label());
                    }
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No template may claim it animates (`animation: Some(_)`) and
    /// ship its `TitleData::animation_type` as `Static` AND have
    /// every TextBox-level effect field at default. Pre-cleanup, 60+
    /// templates failed this contract — they wore "+ ANIMATED" badges
    /// in the browser but dropped static text into the timeline.
    /// This test pins the contract that the animation badge means
    /// the template actually does something at render time.
    #[test]
    fn every_animated_template_actually_animates() {
        for tpl in TITLES {
            if tpl.animation.is_none() {
                continue;
            }
            let td = (tpl.create_data)();
            let has_animation_type =
                !matches!(td.animation_type, klipp_timeline::TitleAnimationType::Static);
            let has_text_effect = td.objects.iter().any(|obj| {
                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content {
                    tb.glitch_intensity > 0.0
                        || tb.chromatic_aberration_px > 0.0
                        || tb.char_animation.is_some()
                } else {
                    false
                }
            });
            assert!(
                has_animation_type || has_text_effect,
                "Template '{}' (category '{}') claims animation '{:?}' but its create_data \
                 returns a fully-static title — set TitleData::animation_type or a TextBox \
                 effect field, or change `animation` to None.",
                tpl.name, tpl.category, tpl.animation,
            );
        }
    }

    /// Every template's stated category must appear in the
    /// `CATEGORIES` sidebar list, otherwise the template is
    /// invisible in the browser.
    #[test]
    fn every_template_category_is_in_sidebar() {
        for tpl in TITLES {
            assert!(
                CATEGORIES.contains(&tpl.category),
                "Template '{}' has category '{}' which is not in CATEGORIES — \
                 it will never appear in the browser sidebar.",
                tpl.name, tpl.category,
            );
        }
    }

    /// Every template that ships an animated TitleAnimationType must
    /// produce a non-static result from `apply_animation_type` at
    /// SOME point in the preview loop — otherwise the hover preview
    /// looks identical to a static template and the user has no
    /// indication that something will animate on playback.
    ///
    /// We sample t=0.0, 0.5, 1.5 (covers the entrance window, mid-
    /// loop, and post-entrance hold) and assert that at least one
    /// sample differs from the t=0 baseline (text content, position,
    /// alpha, scale, or colour override). Static templates skip.
    #[test]
    fn every_animated_template_changes_preview_over_time() {
        use klipp_timeline::TitleAnimationType as T;
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(120.0, 80.0));
        let pos = egui::pos2(60.0, 40.0);

        for tpl in TITLES {
            let td = (tpl.create_data)();
            if matches!(td.animation_type, T::Static) {
                // Static templates with char_animation drive their
                // animation through paint_char_animated, not
                // apply_animation_type — those are tested elsewhere.
                continue;
            }

            let preview_text = "Sample Text";
            let baseline = apply_animation_type(&td, preview_text, rect, pos, 18.0, 0.0);
            let samples = [0.5_f32, 1.5_f32, 2.5_f32];
            let mut differs = false;
            for &t in &samples {
                let s = apply_animation_type(&td, preview_text, rect, pos, 18.0, t);
                let pos_changed = (s.1.x - baseline.1.x).abs() > 0.01
                    || (s.1.y - baseline.1.y).abs() > 0.01;
                let alpha_changed = (s.2 - baseline.2).abs() > 0.001;
                let scale_changed = (s.3 - baseline.3).abs() > 0.001;
                let text_changed = s.0 != baseline.0;
                let color_changed = s.4 != baseline.4;
                if pos_changed || alpha_changed || scale_changed || text_changed || color_changed {
                    differs = true;
                    break;
                }
            }
            assert!(
                differs,
                "Template '{}' (category '{}', animation_type={:?}) produces an \
                 identical apply_animation_type result at every sampled time — \
                 the hover preview will look static even though playback animates.",
                tpl.name, tpl.category, td.animation_type,
            );
        }
    }

    /// Template names must be unique within a category — duplicates
    /// confuse the user (two cards with the same label and no way
    /// to tell them apart).
    #[test]
    fn template_names_unique_within_category() {
        for cat in CATEGORIES {
            let names: Vec<&str> = TITLES
                .iter()
                .filter(|t| t.category == *cat)
                .map(|t| t.name)
                .collect();
            for (i, name) in names.iter().enumerate() {
                for other in &names[i + 1..] {
                    assert_ne!(
                        name, other,
                        "Duplicate template name '{name}' in category '{cat}' — \
                         each category must have unique names."
                    );
                }
            }
        }
    }
}


