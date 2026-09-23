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
/// [`render_generator_with_slate`] as sRGB-encoded half floats.
///
/// A slate or a placeholder is text, drawn by the title renderer into
/// bytes, so those are widened the way the decode pass widens them.
/// Everything else goes to `render_generator_f16`, where the two
/// continuous-tone generators draw straight into half floats. The sRGB
/// decode stays on the GPU, where the eight-bit path's decode is.
pub fn render_generator_with_slate_f16(
    title_renderer: &mut klipp_title::TitleRenderer,
    gen_data: &klipp_core::GeneratorData,
    w: u32,
    h: u32,
    elapsed_secs: f64,
    total_secs: f64,
) -> Vec<u32> {
    if matches!(
        gen_data,
        klipp_core::GeneratorData::Slate { .. } | klipp_core::GeneratorData::Placeholder { .. }
    ) {
        let bytes = render_generator_with_slate(title_renderer, gen_data, w, h, elapsed_secs, total_secs);
        return klipp_core::generator::srgb_u8_to_f16(&bytes);
    }
    klipp_core::generator::render_generator_f16(gen_data, w, h, elapsed_secs, total_secs)
}

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
    if let klipp_core::GeneratorData::Placeholder { label, color } = gen_data {
        let mut backdrop = klipp_core::generator::render_placeholder_backdrop(*color, w, h);
        let text_box = klipp_timeline::TextBox {
            text: if label.trim().is_empty() { "PLACEHOLDER".into() } else { label.clone() },
            color: [255, 255, 255, 255],
            font_name: "Inter".into(),
            font_style: klipp_timeline::FontStyle::Bold,
            font_size: ((h as f32) / 9.0).max(24.0),
            alignment: klipp_timeline::TextAlignment::Center,
            case_transform: klipp_timeline::TextCase::Uppercase,
            tracking: 4.0,
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(text_box);
        let text = title_renderer.render(&td, w, h);
        // Straight-alpha text over the backdrop.
        for (dst, src) in backdrop.chunks_exact_mut(4).zip(text.chunks_exact(4)) {
            let a = src[3] as u32;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                dst[c] = ((src[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
            }
        }
        return backdrop;
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

    let text_box = klipp_timeline::TextBox {
        text,
        color: text_color,
        // Roughly 1080p / 24 ≈ 45 px font; floor at 20 so very small
        // resolutions still produce legible output.
        font_size: ((height as f32) / 24.0).max(20.0),
        alignment: klipp_timeline::TextAlignment::Left,
        line_spacing: 1.4,
        ..Default::default()
    };

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



