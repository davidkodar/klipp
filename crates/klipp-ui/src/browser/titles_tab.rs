use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::{
    CharAnimation, CharEntrance, FontStyle, TextAlignment, TextExtrusion, TextGlow, TextGradient,
    TextOutline, TextPath, TitleBackground, TitleData, TitlePosition, TitleShadow,
};

struct TitleTemplate {
    name: &'static str,
    category: &'static str,
    preview_text: &'static str,
    create_data: fn() -> TitleData,
    /// Animation identifier applied when clip is created. None = static (no animation).
    animation: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Helper factories
// ---------------------------------------------------------------------------

/// Centered title with optional shadow.
fn centered(text: &str, size: f32, style: FontStyle, color: [u8; 4], shadow: bool) -> TitleData {
    TitleData {
        text: text.into(),
        font_size: size,
        font_name: "sans-serif".into(),
        font_style: style,
        color,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::None,
        shadow: if shadow {
            Some(TitleShadow { color: [0, 0, 0, 160], offset_x: 3.0, offset_y: 3.0, blur: 6.0 })
        } else {
            None
        },
        ..TitleData::default()
    }
}

/// Centered on a full-frame background.
fn centered_on_bg(text: &str, size: f32, style: FontStyle, fg: [u8; 4], bg: [u8; 4]) -> TitleData {
    TitleData {
        text: text.into(),
        font_size: size,
        font_name: "sans-serif".into(),
        font_style: style,
        color: fg,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::FullFrame { color: bg },
        shadow: None,
        ..TitleData::default()
    }
}

/// Lower-third style: left-aligned near bottom, with box background.
fn lower_third_style(text: &str, size: f32, style: FontStyle, fg: [u8; 4], bg: [u8; 4], x: f32, y: f32) -> TitleData {
    TitleData {
        text: text.into(),
        font_size: size,
        font_name: "sans-serif".into(),
        font_style: style,
        color: fg,
        alignment: TextAlignment::Left,
        position: TitlePosition { x, y },
        background: TitleBackground::Color { color: bg, padding: 12.0 },
        shadow: None,
        ..TitleData::default()
    }
}

/// Positioned text with optional shadow, no background.
#[allow(clippy::too_many_arguments)]
fn positioned(text: &str, size: f32, style: FontStyle, color: [u8; 4], align: TextAlignment, x: f32, y: f32, shadow: Option<TitleShadow>) -> TitleData {
    TitleData {
        text: text.into(),
        font_size: size,
        font_name: "sans-serif".into(),
        font_style: style,
        color,
        alignment: align,
        position: TitlePosition { x, y },
        background: TitleBackground::None,
        shadow,
        ..TitleData::default()
    }
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
    Some(TitleShadow { color: [0, 0, 0, 160], offset_x: 3.0, offset_y: 3.0, blur: 6.0 })
}

fn shadow_soft() -> Option<TitleShadow> {
    Some(TitleShadow { color: [0, 0, 0, 120], offset_x: 2.0, offset_y: 2.0, blur: 8.0 })
}

fn shadow_hard() -> Option<TitleShadow> {
    Some(TitleShadow { color: [0, 0, 0, 200], offset_x: 4.0, offset_y: 4.0, blur: 2.0 })
}

fn shadow_glow(color: [u8; 4]) -> Option<TitleShadow> {
    Some(TitleShadow { color, offset_x: 0.0, offset_y: 0.0, blur: 12.0 })
}

// ---------------------------------------------------------------------------
// Template create functions — Basic (10)
// ---------------------------------------------------------------------------

fn basic_plain_title() -> TitleData { centered("[Your Title]", 72.0, FontStyle::Regular, WHITE, true) }
fn basic_subtitle() -> TitleData {
    TitleData {
        text: "[Subtitle text]".into(),
        font_size: 28.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.88 },
        background: TitleBackground::Color { color: [0, 0, 0, 153], padding: 10.0 },
        shadow: None,
        ..TitleData::default()
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
    positioned("typewriter_text", 36.0, FontStyle::Regular, [200, 200, 180, 255], TextAlignment::Left, 0.15, 0.5, None)
}
fn cin_glitch() -> TitleData {
    positioned("GL1TCH", 72.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, Some(TitleShadow { color: [0, 255, 255, 120], offset_x: 4.0, offset_y: -2.0, blur: 2.0 }))
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
    TitleData {
        text: "SUBSCRIBE to [Channel Name]".into(),
        font_size: 42.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.8 },
        background: TitleBackground::Color { color: [200, 20, 20, 230], padding: 16.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn sm_like_share() -> TitleData {
    TitleData {
        text: "LIKE & SHARE".into(),
        font_size: 36.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.85 },
        background: TitleBackground::Color { color: [40, 100, 200, 220], padding: 12.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn sm_hashtag() -> TitleData {
    positioned("#[hashtag]", 48.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sm_comment() -> TitleData {
    TitleData {
        text: "[Your comment here...]".into(),
        font_size: 28.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: WHITE,
        alignment: TextAlignment::Left,
        position: TitlePosition { x: 0.15, y: 0.75 },
        background: TitleBackground::Color { color: [50, 50, 60, 200], padding: 14.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn sm_poll() -> TitleData {
    positioned("What do you think?", 42.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.3, shadow_default())
}

// ---------------------------------------------------------------------------
// Animated Intros (15) — all have baked-in entrance animations
// ---------------------------------------------------------------------------

fn anim_fade_up() -> TitleData { centered("Fade Up", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_slide_left() -> TitleData { centered("Slide Left", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_slide_right() -> TitleData { centered("Slide Right", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_drop_down() -> TitleData { centered("Drop Down", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_rise_up() -> TitleData { centered("Rise Up", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_pop_in() -> TitleData { centered("Pop In", 64.0, FontStyle::Bold, WHITE, true) }
fn anim_zoom_in() -> TitleData { centered("Zoom In", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_slide_left() -> TitleData { centered("Fade & Slide", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_slide_right() -> TitleData { centered("Fade & Slide", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_rise() -> TitleData { centered("Fade & Rise", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_drop() -> TitleData { centered("Fade & Drop", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_bounce_in() -> TitleData { centered("Bounce In", 64.0, FontStyle::Bold, WHITE, true) }
fn anim_grow_fade() -> TitleData { centered("Grow & Fade", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_wipe_in() -> TitleData { centered("Wipe In", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_elegant_fade() -> TitleData { centered("Elegant Fade", 56.0, FontStyle::Regular, WHITE_DIM, false) }

// ---------------------------------------------------------------------------
// Animated Outros (10) — all have exit animations
// ---------------------------------------------------------------------------

fn anim_fade_down() -> TitleData { centered("Fade Down", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_slide_out_left() -> TitleData { centered("Slide Out Left", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_slide_out_right() -> TitleData { centered("Slide Out Right", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_shrink_out() -> TitleData { centered("Shrink Out", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_out_slow() -> TitleData { centered("Fade Out Slow", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_rise_fade() -> TitleData { centered("Rise & Fade", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_drop_fade() -> TitleData { centered("Drop & Fade", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_pop_out() -> TitleData { centered("Pop Out", 64.0, FontStyle::Bold, WHITE, true) }
fn anim_dissolve() -> TitleData { centered("Dissolve", 56.0, FontStyle::Regular, WHITE_DIM, false) }
fn anim_quick_exit() -> TitleData { centered("Quick Exit", 64.0, FontStyle::Bold, WHITE, true) }

// ---------------------------------------------------------------------------
// Full Animations (10) — entrance + exit
// ---------------------------------------------------------------------------

fn anim_fade_in_out() -> TitleData { centered("Fade In & Out", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_slide_lr() -> TitleData { centered("Slide Left\u{2192}Right", 56.0, FontStyle::Regular, WHITE, true) }
fn anim_pop_fade() -> TitleData { centered("Pop In, Fade Out", 64.0, FontStyle::Bold, WHITE, true) }
fn anim_rise_hold_drop() -> TitleData { centered("Rise, Hold, Drop", 56.0, FontStyle::Regular, WHITE, true) }
fn anim_zoom_in_out() -> TitleData { centered("Zoom In & Out", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_fade_slide_both() -> TitleData { centered("Fade Slide Both", 56.0, FontStyle::Regular, WHITE, true) }
fn anim_gentle_in_out() -> TitleData { centered("Gentle In & Out", 56.0, FontStyle::Regular, WHITE_DIM, false) }
fn anim_quick_flash() -> TitleData { centered("Quick Flash", 72.0, FontStyle::Bold, WHITE, true) }
fn anim_dramatic_fade() -> TitleData { centered("Dramatic Enter", 64.0, FontStyle::Regular, WHITE, true) }
fn anim_smooth_cycle() -> TitleData { centered("Smooth Cycle", 56.0, FontStyle::Regular, WHITE, true) }

// ---------------------------------------------------------------------------
// Broadcast (10)
// ---------------------------------------------------------------------------

fn bc_breaking_news() -> TitleData {
    TitleData {
        text: "BREAKING NEWS".into(),
        font_size: 48.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.12 },
        background: TitleBackground::Color { color: [200, 20, 20, 230], padding: 16.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn bc_news_ticker() -> TitleData { lower_third_style("Breaking: [Story details here...]", 26.0, FontStyle::Regular, WHITE, [20, 20, 80, 220], 0.05, 0.94) }
fn bc_weather() -> TitleData { lower_third_style("[City]  [Temp]\u{00b0}F", 28.0, FontStyle::Regular, WHITE, [0, 80, 160, 210], 0.08, 0.87) }
fn bc_score_bug() -> TitleData {
    TitleData {
        text: "[HOME] 0 - 0 [AWAY]".into(),
        font_size: 28.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.06 },
        background: TitleBackground::Color { color: [0, 0, 0, 200], padding: 10.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn bc_reporter() -> TitleData { lower_third_style("[Reporter Name]\n[City Bureau]", 28.0, FontStyle::Bold, WHITE, [0, 60, 140, 220], 0.08, 0.84) }
fn bc_program_title() -> TitleData { centered_on_bg("[PROGRAM TITLE]", 56.0, FontStyle::Bold, WHITE, [20, 30, 60, 240]) }
fn bc_live_badge() -> TitleData {
    TitleData {
        text: "LIVE".into(),
        font_size: 24.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.92, y: 0.06 },
        background: TitleBackground::Color { color: [220, 20, 20, 240], padding: 8.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn bc_time_zone() -> TitleData {
    TitleData {
        text: "12:00 PM EST".into(),
        font_size: 22.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: WHITE_DIM,
        alignment: TextAlignment::Right,
        position: TitlePosition { x: 0.94, y: 0.94 },
        background: TitleBackground::Color { color: [0, 0, 0, 150], padding: 6.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn bc_network_bug() -> TitleData {
    positioned("NET", 18.0, FontStyle::Bold, [255, 255, 255, 120], TextAlignment::Right, 0.95, 0.05, None)
}
fn bc_alert() -> TitleData {
    TitleData {
        text: "SEVERE WEATHER ALERT".into(),
        font_size: 36.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::Color { color: [180, 40, 0, 230], padding: 18.0 },
        shadow: None,
        ..TitleData::default()
    }
}

// ---------------------------------------------------------------------------
// Educational (8)
// ---------------------------------------------------------------------------

fn edu_lesson() -> TitleData { centered_on_bg("Lesson [#]: [Topic]", 48.0, FontStyle::Bold, WHITE, [30, 30, 60, 240]) }
fn edu_key_point() -> TitleData {
    TitleData {
        text: "Key Point: [Important fact here]".into(),
        font_size: 36.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: [255, 255, 100, 255],
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.5 },
        background: TitleBackground::Color { color: [0, 0, 0, 180], padding: 16.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn edu_definition() -> TitleData {
    TitleData {
        text: "[Term]: [Definition goes here]".into(),
        font_size: 32.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Regular,
        color: WHITE,
        alignment: TextAlignment::Left,
        position: TitlePosition { x: 0.15, y: 0.5 },
        background: TitleBackground::Color { color: [20, 60, 80, 200], padding: 14.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn edu_step() -> TitleData { centered_on_bg("Step [#]: [Description]", 42.0, FontStyle::Bold, WHITE, [40, 60, 40, 240]) }
fn edu_formula() -> TitleData {
    positioned("E = mc\u{00b2}", 48.0, FontStyle::Regular, GREEN, TextAlignment::Center, 0.5, 0.5, shadow_default())
}
fn edu_annotation() -> TitleData {
    TitleData {
        text: "Note: See details".into(),
        font_size: 22.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Italic,
        color: [255, 255, 200, 255],
        alignment: TextAlignment::Right,
        position: TitlePosition { x: 0.92, y: 0.15 },
        background: TitleBackground::Color { color: [0, 0, 0, 140], padding: 8.0 },
        shadow: None,
        ..TitleData::default()
    }
}
fn edu_summary() -> TitleData { centered_on_bg("Summary", 48.0, FontStyle::Bold, WHITE, [50, 30, 80, 240]) }
fn edu_quiz() -> TitleData {
    TitleData {
        text: "[Your question here?]".into(),
        font_size: 42.0,
        font_name: "sans-serif".into(),
        font_style: FontStyle::Bold,
        color: WHITE,
        alignment: TextAlignment::Center,
        position: TitlePosition { x: 0.5, y: 0.35 },
        background: TitleBackground::Color { color: [80, 40, 160, 210], padding: 16.0 },
        shadow: None,
        ..TitleData::default()
    }
}

// ---------------------------------------------------------------------------
// Stylized (10)
// ---------------------------------------------------------------------------

fn sty_neon() -> TitleData {
    TitleData {
        glow: Some(TextGlow { color: [0, 255, 200, 255], radius: 15.0, intensity: 0.8 }),
        outline: Some(TextOutline { color: [0, 255, 200, 255], width: 2.0 }),
        ..positioned("NEON", 72.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn sty_outline() -> TitleData {
    TitleData {
        outline: Some(TextOutline { color: [255, 255, 255, 255], width: 3.0 }),
        color: [255, 255, 255, 80], // semi-transparent fill to emphasise the outline
        ..positioned("OUTLINE", 72.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn sty_handwritten() -> TitleData {
    positioned("handwritten feel", 48.0, FontStyle::Italic, WHITE_DIM, TextAlignment::Center, 0.5, 0.5, shadow_soft())
}
fn sty_stamp() -> TitleData {
    positioned("APPROVED", 72.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, shadow_hard())
}
fn sty_gradient() -> TitleData {
    TitleData {
        gradient: Some(TextGradient { top_color: [255, 200, 50, 255], bottom_color: [255, 50, 50, 255] }),
        ..positioned("GRADIENT", 72.0, FontStyle::Bold, PURPLE, TextAlignment::Center, 0.5, 0.5, shadow_glow([160, 80, 255, 150]))
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
// 3D Text (5)
// ---------------------------------------------------------------------------

fn three_d_block() -> TitleData {
    TitleData {
        extrusion: Some(TextExtrusion {
            depth: 15.0, angle: 135.0,
            face_color: [255, 255, 255, 255],
            side_color: [80, 80, 120, 255],
            lighting: 0.6,
        }),
        ..positioned("BLOCK", 84.0, FontStyle::Bold, WHITE, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn three_d_gold_emboss() -> TitleData {
    TitleData {
        extrusion: Some(TextExtrusion {
            depth: 8.0, angle: 135.0,
            face_color: [0, 0, 0, 0],
            side_color: [160, 120, 40, 255],
            lighting: 0.8,
        }),
        gradient: Some(TextGradient { top_color: [255, 220, 100, 255], bottom_color: [200, 150, 50, 255] }),
        ..positioned("EMBOSS", 72.0, FontStyle::Bold, GOLD, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn three_d_deep_shadow() -> TitleData {
    TitleData {
        extrusion: Some(TextExtrusion {
            depth: 30.0, angle: 150.0,
            face_color: [220, 50, 50, 255],
            side_color: [60, 20, 20, 255],
            lighting: 0.4,
        }),
        ..positioned("DEEP", 96.0, FontStyle::Bold, RED, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn three_d_neon_extrude() -> TitleData {
    TitleData {
        extrusion: Some(TextExtrusion {
            depth: 12.0, angle: 180.0,
            face_color: [0, 255, 200, 255],
            side_color: [0, 80, 60, 255],
            lighting: 0.7,
        }),
        glow: Some(TextGlow { color: [0, 255, 200, 255], radius: 12.0, intensity: 0.6 }),
        ..positioned("NEON 3D", 72.0, FontStyle::Bold, CYAN, TextAlignment::Center, 0.5, 0.5, None)
    }
}
fn three_d_retro_stack() -> TitleData {
    TitleData {
        extrusion: Some(TextExtrusion {
            depth: 20.0, angle: 120.0,
            face_color: [255, 220, 50, 255],
            side_color: [150, 80, 30, 255],
            lighting: 0.5,
        }),
        outline: Some(TextOutline { color: [40, 30, 20, 255], width: 2.0 }),
        ..positioned("RETRO", 84.0, FontStyle::Bold, ORANGE, TextAlignment::Center, 0.5, 0.5, None)
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
    TitleData {
        text_path: Some(TextPath::Arc { radius: 250.0, start_angle: -90.0, sweep: 180.0 }),
        ..centered("ARC TITLE", 56.0, FontStyle::Bold, WHITE, true)
    }
}
fn path_circle_text() -> TitleData {
    TitleData {
        text_path: Some(TextPath::Circle { radius: 200.0 }),
        ..centered("CIRCLE TEXT", 48.0, FontStyle::Bold, WHITE, true)
    }
}
fn path_wave_text() -> TitleData {
    TitleData {
        text_path: Some(TextPath::Wave { amplitude: 30.0, frequency: 2.0, phase: 0.0 }),
        ..centered("WAVE TEXT", 56.0, FontStyle::Bold, WHITE, true)
    }
}

// ---------------------------------------------------------------------------
// Per-Character Animation (2)
// ---------------------------------------------------------------------------

fn char_stagger_fade() -> TitleData {
    TitleData {
        char_animation: Some(CharAnimation {
            stagger: 0.05,
            entrance: CharEntrance::FadeIn,
            wave_amplitude: 0.0,
            wave_speed: 0.0,
        }),
        ..centered("Stagger Fade", 64.0, FontStyle::Regular, WHITE, true)
    }
}
fn char_wave_bounce() -> TitleData {
    TitleData {
        char_animation: Some(CharAnimation {
            stagger: 0.0,
            entrance: CharEntrance::None,
            wave_amplitude: 15.0,
            wave_speed: 2.0,
        }),
        ..centered("Wave Bounce", 64.0, FontStyle::Bold, WHITE, true)
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

    // ---- Animated Intros (15) ----
    TitleTemplate { name: "Fade Up", category: "Animated Intros", preview_text: "Fade Up", create_data: anim_fade_up, animation: Some("fade_in_0.5") },
    TitleTemplate { name: "Slide from Left", category: "Animated Intros", preview_text: "Slide Left", create_data: anim_slide_left, animation: Some("slide_left_0.5") },
    TitleTemplate { name: "Slide from Right", category: "Animated Intros", preview_text: "Slide Right", create_data: anim_slide_right, animation: Some("slide_right_0.5") },
    TitleTemplate { name: "Drop Down", category: "Animated Intros", preview_text: "Drop Down", create_data: anim_drop_down, animation: Some("slide_down_0.5") },
    TitleTemplate { name: "Rise Up", category: "Animated Intros", preview_text: "Rise Up", create_data: anim_rise_up, animation: Some("slide_up_0.5") },
    TitleTemplate { name: "Pop In", category: "Animated Intros", preview_text: "Pop In", create_data: anim_pop_in, animation: Some("pop_in_0.5") },
    TitleTemplate { name: "Zoom In", category: "Animated Intros", preview_text: "Zoom In", create_data: anim_zoom_in, animation: Some("zoom_in_0.6") },
    TitleTemplate { name: "Fade & Slide Left", category: "Animated Intros", preview_text: "Fade+Slide", create_data: anim_fade_slide_left, animation: Some("fade_in_0.8+slide_left_0.8") },
    TitleTemplate { name: "Fade & Slide Right", category: "Animated Intros", preview_text: "Fade+Slide", create_data: anim_fade_slide_right, animation: Some("fade_in_0.8+slide_right_0.8") },
    TitleTemplate { name: "Fade & Rise", category: "Animated Intros", preview_text: "Fade+Rise", create_data: anim_fade_rise, animation: Some("fade_in_0.8+slide_up_0.8") },
    TitleTemplate { name: "Fade & Drop", category: "Animated Intros", preview_text: "Fade+Drop", create_data: anim_fade_drop, animation: Some("fade_in_0.8+slide_down_0.8") },
    TitleTemplate { name: "Bounce In", category: "Animated Intros", preview_text: "Bounce In", create_data: anim_bounce_in, animation: Some("pop_in_0.6") },
    TitleTemplate { name: "Grow & Fade", category: "Animated Intros", preview_text: "Grow+Fade", create_data: anim_grow_fade, animation: Some("fade_in_0.6+grow_in_0.6") },
    TitleTemplate { name: "Wipe In", category: "Animated Intros", preview_text: "Wipe In", create_data: anim_wipe_in, animation: Some("slide_left_0.3") },
    TitleTemplate { name: "Elegant Fade", category: "Animated Intros", preview_text: "Elegant Fade", create_data: anim_elegant_fade, animation: Some("fade_in_2.0") },

    // ---- Animated Outros (10) ----
    TitleTemplate { name: "Fade Down", category: "Animated Outros", preview_text: "Fade Down", create_data: anim_fade_down, animation: Some("fade_out_0.5") },
    TitleTemplate { name: "Slide Out Left", category: "Animated Outros", preview_text: "Exit Left", create_data: anim_slide_out_left, animation: Some("slide_out_left_0.5") },
    TitleTemplate { name: "Slide Out Right", category: "Animated Outros", preview_text: "Exit Right", create_data: anim_slide_out_right, animation: Some("slide_out_right_0.5") },
    TitleTemplate { name: "Shrink Out", category: "Animated Outros", preview_text: "Shrink Out", create_data: anim_shrink_out, animation: Some("shrink_out_0.5") },
    TitleTemplate { name: "Fade Out Slow", category: "Animated Outros", preview_text: "Slow Fade", create_data: anim_fade_out_slow, animation: Some("fade_out_2.0") },
    TitleTemplate { name: "Rise & Fade", category: "Animated Outros", preview_text: "Rise+Fade", create_data: anim_rise_fade, animation: Some("fade_out_0.8+slide_out_up_0.8") },
    TitleTemplate { name: "Drop & Fade", category: "Animated Outros", preview_text: "Drop+Fade", create_data: anim_drop_fade, animation: Some("fade_out_0.8+slide_out_down_0.8") },
    TitleTemplate { name: "Pop Out", category: "Animated Outros", preview_text: "Pop Out", create_data: anim_pop_out, animation: Some("pop_out_0.5") },
    TitleTemplate { name: "Dissolve", category: "Animated Outros", preview_text: "Dissolve", create_data: anim_dissolve, animation: Some("fade_out_1.5") },
    TitleTemplate { name: "Quick Exit", category: "Animated Outros", preview_text: "Quick Exit", create_data: anim_quick_exit, animation: Some("fade_out_0.3") },

    // ---- Full Animations (10) ----
    TitleTemplate { name: "Fade In & Out", category: "Full Animations", preview_text: "Fade In/Out", create_data: anim_fade_in_out, animation: Some("fade_in_out_0.5") },
    TitleTemplate { name: "Slide Left, Exit Right", category: "Full Animations", preview_text: "Left\u{2192}Right", create_data: anim_slide_lr, animation: Some("slide_left_0.5+slide_out_right_0.5") },
    TitleTemplate { name: "Pop In, Fade Out", category: "Full Animations", preview_text: "Pop+Fade", create_data: anim_pop_fade, animation: Some("pop_in_0.5+fade_out_0.5") },
    TitleTemplate { name: "Rise, Hold, Drop", category: "Full Animations", preview_text: "Rise\u{2192}Drop", create_data: anim_rise_hold_drop, animation: Some("slide_up_0.5+slide_out_down_0.5") },
    TitleTemplate { name: "Zoom In, Zoom Out", category: "Full Animations", preview_text: "Zoom In/Out", create_data: anim_zoom_in_out, animation: Some("zoom_in_0.6+zoom_out_0.6") },
    TitleTemplate { name: "Fade Slide In & Out", category: "Full Animations", preview_text: "Fade+Slide", create_data: anim_fade_slide_both, animation: Some("fade_in_0.8+slide_left_0.8+fade_out_0.8+slide_out_right_0.8") },
    TitleTemplate { name: "Gentle In & Out", category: "Full Animations", preview_text: "Gentle", create_data: anim_gentle_in_out, animation: Some("fade_in_out_2.0") },
    TitleTemplate { name: "Quick Flash", category: "Full Animations", preview_text: "Flash!", create_data: anim_quick_flash, animation: Some("fade_in_out_0.3") },
    TitleTemplate { name: "Dramatic Enter, Fade Exit", category: "Full Animations", preview_text: "Drama+Fade", create_data: anim_dramatic_fade, animation: Some("fade_in_1.5+fade_out_0.8") },
    TitleTemplate { name: "Smooth Cycle", category: "Full Animations", preview_text: "Smooth", create_data: anim_smooth_cycle, animation: Some("fade_in_1.0+grow_in_1.0+fade_out_1.0+shrink_out_1.0") },

    // ---- Broadcast (10) ----
    TitleTemplate { name: "Breaking News", category: "Broadcast", preview_text: "BREAKING NEWS", create_data: bc_breaking_news, animation: None },
    TitleTemplate { name: "News Ticker", category: "Broadcast", preview_text: "Breaking: Story...", create_data: bc_news_ticker, animation: None },
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
    // ---- Motion Effects (20) ----
    TitleTemplate { name: "Spin In", category: "Motion", preview_text: "SPIN", create_data: || centered("SPIN", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("spin_in_1.0") },
    TitleTemplate { name: "Spin Out", category: "Motion", preview_text: "SPIN", create_data: || centered("SPIN", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.3+spin_out_1.0") },
    TitleTemplate { name: "Swirl In", category: "Motion", preview_text: "SWIRL", create_data: || centered("SWIRL", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("swirl_in_1.5") },
    TitleTemplate { name: "Bounce In", category: "Motion", preview_text: "BOUNCE", create_data: || centered("BOUNCE", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("bounce_in_1.0") },
    TitleTemplate { name: "Elastic", category: "Motion", preview_text: "ELASTIC", create_data: || centered("ELASTIC", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("elastic_1.2") },
    TitleTemplate { name: "Shake", category: "Motion", preview_text: "SHAKE!", create_data: || centered("SHAKE!", 64.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.2+shake_0.8") },
    TitleTemplate { name: "Swing In", category: "Motion", preview_text: "SWING", create_data: || centered("SWING", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("swing_in_1.0") },
    TitleTemplate { name: "Explode Out", category: "Motion", preview_text: "BOOM!", create_data: || centered("BOOM!", 96.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.3+explode_out_0.5") },
    TitleTemplate { name: "Implode In", category: "Motion", preview_text: "IMPLODE", create_data: || centered("IMPLODE", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("implode_in_1.0") },
    TitleTemplate { name: "Pulse", category: "Motion", preview_text: "PULSE", create_data: || centered("PULSE", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.3+pulse_0.6") },
    TitleTemplate { name: "Rotate In", category: "Motion", preview_text: "ROTATE", create_data: || centered("ROTATE", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("rotate_in_0.8") },
    TitleTemplate { name: "Rotate Out", category: "Motion", preview_text: "ROTATE", create_data: || centered("ROTATE", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.3+rotate_out_0.8") },
    TitleTemplate { name: "Glitch", category: "Motion", preview_text: "GL!TCH", create_data: || centered("GL!TCH", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("glitch_0.6") },
    TitleTemplate { name: "Float Up", category: "Motion", preview_text: "floating...", create_data: || centered("floating...", 48.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("fade_in_0.5+float_up_5.0") },
    TitleTemplate { name: "Spin & Bounce", category: "Motion", preview_text: "WOW", create_data: || centered("WOW", 96.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("spin_in_0.6+bounce_in_1.0") },
    TitleTemplate { name: "Shake & Fade", category: "Motion", preview_text: "ALERT!", create_data: || { let mut t = centered("ALERT!", 64.0, FontStyle::Regular, [255,255,255,255], true); t.color = [255, 60, 60, 255]; t }, animation: Some("shake_0.5+fade_in_out_2.0") },
    TitleTemplate { name: "Elastic Slide", category: "Motion", preview_text: "SNAP!", create_data: || centered("SNAP!", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("slide_left_0.3+elastic_1.0") },
    TitleTemplate { name: "Zoom & Spin", category: "Motion", preview_text: "ZOOM", create_data: || centered("ZOOM", 96.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("zoom_in_0.5+spin_in_0.8") },
    TitleTemplate { name: "Bounce & Fade Out", category: "Motion", preview_text: "BYE!", create_data: || centered("BYE!", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("bounce_in_0.8+fade_out_1.0") },
    TitleTemplate { name: "Implode & Explode", category: "Motion", preview_text: "FLASH", create_data: || centered("FLASH", 96.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("implode_in_0.5+explode_out_0.5") },
    // ---- Scrolling / Continuous (10) ----
    TitleTemplate { name: "Credits Roll Up", category: "Scrolling", preview_text: "Credits...", create_data: || centered("Directed by\nJohn Doe\n\nProduced by\nJane Smith\n\nEditor\nAlex Johnson", 36.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("scroll_up_1.0") },
    TitleTemplate { name: "Credits Roll Down", category: "Scrolling", preview_text: "Credits \u{2193}", create_data: || centered("IN MEMORIAM\n\nDedicated to...", 36.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("scroll_down_1.0") },
    TitleTemplate { name: "News Ticker R\u{2192}L", category: "Scrolling", preview_text: "\u{2190} BREAKING NEWS", create_data: || { let mut t = lower_third_style("BREAKING: Major event reported \u{2014} More details at 11", 28.0, FontStyle::Regular, [255, 255, 255, 255], [0, 0, 0, 180], 0.15, 0.85); t.background = TitleBackground::Color { color: [180, 30, 30, 220], padding: 8.0 }; t }, animation: Some("ticker_left_1.0") },
    TitleTemplate { name: "News Ticker L\u{2192}R", category: "Scrolling", preview_text: "TICKER \u{2192}", create_data: || { let mut t = lower_third_style("Stock market update: AAPL +2.3% MSFT +1.1% GOOG -0.5%", 24.0, FontStyle::Regular, [255, 220, 50, 255], [0, 0, 0, 180], 0.15, 0.85); t.background = TitleBackground::Color { color: [20, 20, 40, 220], padding: 8.0 }; t }, animation: Some("ticker_right_1.0") },
    TitleTemplate { name: "Slow Crawl Up", category: "Scrolling", preview_text: "Slow crawl \u{2191}", create_data: || centered("The end.\n\nThank you for watching.", 48.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("scroll_up_1.0") },
    TitleTemplate { name: "Continuous Spin", category: "Scrolling", preview_text: "\u{21bb} Spin", create_data: || centered("LOGO", 72.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("spinning_1.0") },
    TitleTemplate { name: "Pulsing Text", category: "Scrolling", preview_text: "\u{2764} Pulse", create_data: || { let mut t = centered("LIVE", 64.0, FontStyle::Regular, [255,255,255,255], true); t.color = [255, 60, 60, 255]; t }, animation: Some("pulsing_1.0") },
    TitleTemplate { name: "Breathing Text", category: "Scrolling", preview_text: "~ Breathe ~", create_data: || centered("breathe", 48.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("breathing_1.0") },
    TitleTemplate { name: "Floating Text", category: "Scrolling", preview_text: "\u{2601} Float", create_data: || centered("drifting...", 36.0, FontStyle::Regular, [255,255,255,255], true), animation: Some("floating_1.0") },
    TitleTemplate { name: "Side Scroll + Fade", category: "Scrolling", preview_text: "\u{2190} Scroll", create_data: || { let mut t = centered("Latest headlines streaming across your screen", 28.0, FontStyle::Regular, [255,255,255,255], true); t.position = TitlePosition { x: 0.5, y: 0.9 }; t }, animation: Some("fade_in_0.5+ticker_left_1.0") },
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
    "3D Text",
    "Text on Path",
    "Per-Character",
    "Wedding / Event",
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    let filtered: Vec<_> = TITLES
        .iter()
        .filter(|t| search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower) || t.category.to_lowercase().contains(&search_lower))
        .collect();

    egui::ScrollArea::vertical().id_salt("browser_titles_scroll").show(ui, |ui| {
        let avail_w = ui.available_width() - 16.0;
        let card_w = (avail_w - 6.0) / 2.0;

        for category in CATEGORIES {
            let cat_titles: Vec<_> = filtered.iter().filter(|t| t.category == *category).collect();
            if cat_titles.is_empty() {
                continue;
            }

            // Collapsible category (collapsed by default, auto-expand when searching)
            ui.add_space(4.0);
            let cat_id = ui.make_persistent_id(format!("title_cat_{category}"));
            let default_open = *category == "Basic"; // Only Basic open by default
            let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(), cat_id, default_open,
            );
            if !search_lower.is_empty() {
                col_state.set_open(true);
            }
            col_state.show_header(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("{}  ({})", category, cat_titles.len()))
                        .strong().size(11.0).color(theme.text_accent)
                );
            })
            .body(|ui| {
                ui.add_space(4.0);
                egui::Grid::new(format!("titles_grid_{category}"))
                    .num_columns(2)
                    .spacing([6.0, 6.0])
                    .min_col_width(card_w)
                    .show(ui, |ui| {
                        for (i, title) in cat_titles.iter().enumerate() {
                            let resp = title_card(ui, theme, title, card_w);

                            // Double-click: create title clip at playhead
                            if resp.double_clicked() {
                                let title_data = (title.create_data)();
                                state.pending_title_data = Some(title_data);
                                state.pending_title_animation = title.animation.map(|s| s.to_string());
                                let ph = state.playhead;
                                let track_index = state.sequences[state.active_sequence_idx].tracks.iter()
                                    .enumerate()
                                    .find(|(_, t)| {
                                        t.kind == klipp_timeline::TrackKind::Text
                                            && t.clip_at_timecode(ph).is_none()
                                    })
                                    .map(|(idx, _)| idx);
                                let track_index = match track_index {
                                    Some(idx) => idx,
                                    None => {
                                        let track_num = state.sequences[state.active_sequence_idx].tracks.iter()
                                            .filter(|t| t.kind == klipp_timeline::TrackKind::Text)
                                            .count() + 1;
                                        let track = klipp_timeline::Track::new(
                                            &format!("Text {track_num}"),
                                            klipp_timeline::TrackKind::Text,
                                        );
                                        state.sequences[state.active_sequence_idx].tracks.insert(0, track);
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
                                state.toast.show(format!(
                                    "\"{}\" title added to timeline",
                                    title.name
                                ));
                            }

                            // Drag: begin title drag
                            if resp.drag_started() {
                                let data = (title.create_data)();
                                state.drag_drop.begin(DragPayload::Title {
                                    template_name: title.name.to_string(),
                                    title_data: data,
                                    animation: title.animation.map(|s| s.to_string()),
                                });
                            }

                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                        if cat_titles.len() % 2 == 1 {
                            ui.end_row();
                        }
                    });
            });
        }
    });

    cmds
}

fn title_card(ui: &mut egui::Ui, theme: &KlippTheme, title: &TitleTemplate, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 76.0),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };

    // Card background
    ui.painter().rect_filled(rect, 6.0, theme.bg_raised);
    ui.painter()
        .rect_stroke(rect, 6.0, egui::Stroke::new(1.0, border_color));

    // Preview area (top 56px, black background)
    let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, 56.0));
    ui.painter().rect_filled(
        preview_rect,
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 },
        egui::Color32::BLACK,
    );

    // Render title preview
    paint_title_preview(ui.painter(), preview_rect, title, theme);

    // Animation badge (small indicator for animated templates)
    if title.animation.is_some() {
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(preview_rect.max.x - 18.0, preview_rect.min.y + 2.0),
            egui::vec2(16.0, 12.0),
        );
        ui.painter().rect_filled(badge_rect, 3.0, egui::Color32::from_rgba_premultiplied(100, 60, 200, 180));
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{25b6}",
            egui::FontId::proportional(7.0),
            egui::Color32::WHITE,
        );
    }

    // Title name
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + 56.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        title.name,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    resp
}

fn paint_title_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    title: &TitleTemplate,
    _theme: &KlippTheme,
) {
    // Determine preview style from category + template properties
    let category = title.category;
    let data = (title.create_data)();

    match category {
        "Lower Thirds" | "Broadcast" => {
            // Show a bar at the bottom with text
            let bg_color = match &data.background {
                TitleBackground::Color { color, .. } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                _ => egui::Color32::from_rgba_premultiplied(0, 0, 0, 180),
            };
            let bar_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - 20.0),
                rect.max,
            );
            painter.rect_filled(bar_rect, 0.0, bg_color);
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            let text_pos = if data.alignment == TextAlignment::Left {
                egui::pos2(rect.min.x + 8.0, rect.max.y - 10.0)
            } else {
                egui::pos2(rect.center().x, rect.max.y - 10.0)
            };
            let align = if data.alignment == TextAlignment::Left { egui::Align2::LEFT_CENTER } else { egui::Align2::CENTER_CENTER };
            painter.text(
                text_pos,
                align,
                title.preview_text,
                egui::FontId::proportional(11.0),
                text_color,
            );
        }
        "Cinematic" => {
            // Dark background with colored text
            let bg = match &data.background {
                TitleBackground::FullFrame { color } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                _ => egui::Color32::from_rgb(0x0a, 0x0a, 0x0c),
            };
            painter.rect_filled(rect, 0.0, bg);
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            let font_size = (data.font_size / 6.0).clamp(9.0, 16.0);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(font_size),
                text_color,
            );
        }
        "Stylized" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x11, 0x11, 0x11));
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            // Add glow effect for neon/gradient styles
            if let Some(ref shadow) = data.shadow {
                if shadow.offset_x == 0.0 && shadow.offset_y == 0.0 && shadow.blur > 6.0 {
                    let glow = egui::Color32::from_rgba_premultiplied(shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3] / 2);
                    for dx in [-1.0f32, 0.0, 1.0] {
                        for dy in [-1.0f32, 0.0, 1.0] {
                            painter.text(
                                egui::pos2(rect.center().x + dx, rect.center().y + dy),
                                egui::Align2::CENTER_CENTER,
                                title.preview_text,
                                egui::FontId::proportional(12.0),
                                glow,
                            );
                        }
                    }
                }
            }
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(12.0),
                text_color,
            );
        }
        "Educational" => {
            let bg = match &data.background {
                TitleBackground::FullFrame { color } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                TitleBackground::Color { color, .. } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                _ => egui::Color32::from_rgb(0x18, 0x18, 0x22),
            };
            painter.rect_filled(rect, 0.0, bg);
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(10.0),
                text_color,
            );
        }
        "Wedding / Event" => {
            let bg = match &data.background {
                TitleBackground::FullFrame { color } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                _ => egui::Color32::from_rgb(0x1a, 0x14, 0x22),
            };
            painter.rect_filled(rect, 0.0, bg);
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(11.0),
                text_color,
            );
        }
        "Animated Intros" | "Animated Outros" | "Full Animations" => {
            // Subtle gradient background for animated templates
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0c, 0x0c, 0x18));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
            // Subtle motion indicator lines
            let mid_y = rect.center().y;
            let alpha = 40u8;
            painter.line_segment(
                [egui::pos2(rect.min.x + 4.0, mid_y - 12.0), egui::pos2(rect.min.x + 16.0, mid_y - 12.0)],
                egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(100, 60, 200, alpha)),
            );
            painter.line_segment(
                [egui::pos2(rect.min.x + 4.0, mid_y + 12.0), egui::pos2(rect.min.x + 12.0, mid_y + 12.0)],
                egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(100, 60, 200, alpha)),
            );
        }
        "Social Media" => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0e, 0x0e, 0x14));
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            let font_size = (data.font_size / 6.0).clamp(9.0, 14.0);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(font_size),
                text_color,
            );
        }
        _ => {
            // Basic and fallback
            let bg = match &data.background {
                TitleBackground::FullFrame { color } => egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]),
                TitleBackground::Color { color, .. } => {
                    painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 200));
                    egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3])
                }
                TitleBackground::None => {
                    painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 200));
                    egui::Color32::TRANSPARENT
                }
            };
            if bg != egui::Color32::TRANSPARENT && !matches!(&data.background, TitleBackground::Color { .. } | TitleBackground::None) {
                painter.rect_filled(rect, 0.0, bg);
            }
            let text_color = egui::Color32::from_rgba_premultiplied(data.color[0], data.color[1], data.color[2], data.color[3]);
            let font_size = (data.font_size / 6.0).clamp(9.0, 14.0);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                title.preview_text,
                egui::FontId::proportional(font_size),
                text_color,
            );
        }
    }
}
