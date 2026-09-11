//! Titles and generators: clips rendered rather than decoded.
//!
//! Moved from klipp-engine so the viewer and export rasterise them with
//! the same code.

/// Render a generator clip's frame, intercepting `GeneratorData::Slate`
/// to route through `klipp-title`'s `TitleRenderer` (klipp-core can't
/// render text — its fallback is a plain background-coloured frame).
/// Every other variant is delegated to `klipp_core::generator::render_generator`.
///
/// Free function — takes `&mut TitleRenderer` so any caller with
/// access to the engine's title_renderer can use it without going
/// through &mut self.
pub fn render_generator_with_slate(
    title_renderer: &mut klipp_title::TitleRenderer,
    gen_data: &klipp_core::GeneratorData,
    w: u32,
    h: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u8> {
    if let klipp_core::GeneratorData::Slate {
        project, scene, take, director, camera, date, framerate, notes,
        text_color, background_color,
    } = gen_data
    {
        let title_data = build_slate_title_data(
            project, scene, take, director, camera, date, framerate, notes,
            *text_color, *background_color, h,
        );
        return title_renderer.render(&title_data, w, h);
    }
    klipp_core::generator::render_generator(gen_data, w, h, elapsed_secs, total_secs)
}

/// Build a `TitleData` for a Slate generator. One TextBox containing
/// the 8 fields as `LABEL:    value` lines, joined with `\n`. Empty
/// fields are skipped — a fresh slate with only the project name set
/// renders as just that one line. Background colour is painted via
/// `TitleBackground::FullFrame`.
#[allow(clippy::too_many_arguments)]
pub fn build_slate_title_data(
    project: &str,
    scene: &str,
    take: &str,
    director: &str,
    camera: &str,
    date: &str,
    framerate: &str,
    notes: &str,
    text_color: [u8; 4],
    background_color: [u8; 4],
    height: u32,
) -> klipp_timeline::TitleData {
    // Fixed-width label column so the field values line up vertically
    // even in proportional fonts. Pad to 11 chars to leave room for
    // the longest label ("FRAMERATE:").
    let mut lines: Vec<String> = Vec::with_capacity(8);
    let push = |lines: &mut Vec<String>, label: &str, value: &str| {
        if !value.is_empty() {
            lines.push(format!("{label:<11}{value}"));
        }
    };
    push(&mut lines, "PROJECT:",   project);
    push(&mut lines, "SCENE:",     scene);
    push(&mut lines, "TAKE:",      take);
    push(&mut lines, "DIRECTOR:",  director);
    push(&mut lines, "CAMERA:",    camera);
    push(&mut lines, "DATE:",      date);
    push(&mut lines, "FRAMERATE:", framerate);
    push(&mut lines, "NOTES:",     notes);
    let text = lines.join("\n");

    let mut text_box = klipp_timeline::TextBox::default();
    text_box.text = text;
    text_box.color = text_color;
    // Roughly 1080p / 24 ≈ 45 px font; floor at 20 so very small
    // resolutions still produce legible output.
    text_box.font_size = ((height as f32) / 24.0).max(20.0);
    text_box.alignment = klipp_timeline::TextAlignment::Left;
    text_box.line_spacing = 1.4;

    let mut td = klipp_timeline::TitleData::single_text(text_box);
    td.background = klipp_timeline::TitleBackground::FullFrame { color: background_color };
    td
}

/// Wrapper adapter so we can implement AlphaMaskProcessor (from klipp-title)
/// for GpuEffectsPipeline (from klipp-effects). Orphan rule requires the newtype pattern.
pub struct GpuAlphaMaskAdapter<'a>(pub &'a mut klipp_effects::gpu::GpuEffectsPipeline);

impl klipp_title::AlphaMaskProcessor for GpuAlphaMaskAdapter<'_> {
    fn blur(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.blur_alpha_gpu(mask, w, h, radius)
    }
    fn dilate(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.dilate_alpha_gpu(mask, w, h, radius)
    }
}


/// Time-aware override that rewrites the title for the current clip
/// time. Returns `Some(owned)` when the animation_type evaluates to a
/// per-frame derived view of the title (Typewriter / TypewriterOut
/// drive `write_on`; Counter rewrites the text body to the current
/// interpolated number). Returns `None` for animation kinds that
/// don't depend on time at the title-content level — those are
/// evaluated by the compositor via clip-level keyframe channels.
pub fn typewriter_override(
    td: &klipp_timeline::TitleData,
    clip_elapsed_secs: f64,
) -> Option<klipp_timeline::TitleData> {
    use klipp_timeline::TitleAnimationType as T;
    match td.animation_type {
        T::Typewriter { chars_per_sec } => {
            let chars = td.text().chars().count().max(1);
            let write_on = ((clip_elapsed_secs * chars_per_sec as f64) / chars as f64)
                .clamp(0.0, 1.0) as f32;
            let mut out = td.clone();
            if let Some(tb) = out.primary_text_mut() {
                tb.write_on = write_on;
            }
            Some(out)
        }
        T::TypewriterOut { chars_per_sec } => {
            let chars = td.text().chars().count().max(1);
            // Ramp from 1.0 (fully visible) → 0.0 (cleared); each
            // step exposes `chars_per_sec` characters per second of
            // deletion travelling right→left.
            let write_on = (1.0 - (clip_elapsed_secs * chars_per_sec as f64) / chars as f64)
                .clamp(0.0, 1.0) as f32;
            let mut out = td.clone();
            if let Some(tb) = out.primary_text_mut() {
                tb.write_on = write_on;
            }
            Some(out)
        }
        T::Counter { from_value, to_value, duration_secs, decimals } => {
            let progress = if duration_secs <= 0.0 {
                1.0
            } else {
                (clip_elapsed_secs / duration_secs as f64).clamp(0.0, 1.0)
            };
            let value = from_value + (to_value - from_value) * progress;
            let formatted = format_counter_value(value, decimals);
            let mut out = td.clone();
            if let Some(tb) = out.primary_text_mut() {
                tb.text = formatted;
            }
            Some(out)
        }
        _ => None,
    }
}

/// Render a Counter's current value as a display string with the
/// requested decimal-place count. `decimals` is clamped to 0..=3 —
/// values higher than 3 are visually noisy and lose precision when
/// rasterised at common font sizes.
pub fn format_counter_value(value: f64, decimals: u8) -> String {
    let d = decimals.min(3) as usize;
    format!("{value:.d$}", d = d)
}

