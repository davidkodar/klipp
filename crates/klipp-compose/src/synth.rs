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
    if let klipp_core::GeneratorData::Slate {
        project, scene, take, director, camera, date, framerate, notes,
        text_color, background_color,
    } = gen_data
    {
        // The slate's words over its colour as the title stacks them,
        // unrounded (#190).
        let title_data = build_slate_title_data(
            title_renderer, project, scene, take, director, camera, date, framerate, notes,
            *text_color, *background_color, w, h,
        );
        return title_renderer.render_raster(&title_data, w, h).to_frame_f16();
    }
    if matches!(gen_data, klipp_core::GeneratorData::Placeholder { .. }) {
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
            title_renderer, project, scene, take, director, camera, date, framerate, notes,
            *text_color, *background_color, w, h,
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

/// Build a `TitleData` for a Slate generator: its fields in two columns,
/// the labels and their values, each row's pair on one line. Empty
/// fields are skipped, so a fresh slate with only the project name set
/// renders as that one row. The block is sized and centred to fill the
/// frame inside a margin whatever its aspect (#2774): measured at a
/// reference size with `renderer`, then scaled so its width and height
/// both fit. Background colour is painted via
/// `TitleBackground::FullFrame`.
#[allow(clippy::too_many_arguments)]
pub fn build_slate_title_data(
    renderer: &mut klipp_title::TitleRenderer,
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
    width: u32,
    height: u32,
) -> klipp_timeline::TitleData {
    use klipp_timeline::{TextAlignment, TitleObjectContent, TitlePosition};
    let rows: Vec<(&str, &str)> = [
        ("PROJECT", project),
        ("SCENE", scene),
        ("TAKE", take),
        ("DIRECTOR", director),
        ("CAMERA", camera),
        ("DATE", date),
        ("FRAMERATE", framerate),
        ("NOTES", notes),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    let column = |text: String, size: f32, bold: bool| klipp_timeline::TextBox {
        text,
        color: text_color,
        font_size: size,
        font_style: if bold { klipp_timeline::FontStyle::Bold } else { klipp_timeline::FontStyle::Regular },
        alignment: TextAlignment::Left,
        line_spacing: 1.4,
        ..Default::default()
    };
    let labels = rows.iter().map(|(l, _)| *l).collect::<Vec<_>>().join("\n");
    let values = rows.iter().map(|(_, v)| *v).collect::<Vec<_>>().join("\n");
    // Each column alone, unwrapped, at a reference size.
    const REFERENCE: f32 = 60.0;
    let mut measure = |text: &str, bold: bool| {
        if text.is_empty() {
            return (0.0f32, 0.0f32);
        }
        let mut td = klipp_timeline::TitleData::single_text(column(text.into(), REFERENCE, bold));
        td.objects[0].transform.center = TitlePosition { x: 0.0, y: 0.5 };
        td.objects[0].transform.size = TitlePosition { x: 50.0, y: 50.0 };
        let l = renderer.render_layout(&td, width, height);
        ((l.text_right - l.text_left).max(0.0), (l.text_bottom - l.text_top).max(0.0))
    };
    let (label_w, label_h) = measure(&labels, true);
    let (value_w, value_h) = measure(&values, false);
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let gap = label_h.max(value_h) / rows.len().max(1) as f32 * 0.6;
    let block_w = label_w + gap + value_w;
    let block_h = label_h.max(value_h);
    // Inside a margin of a twelfth of the frame's short side, and no
    // larger than a frame's twelfth a line, so one field alone isn't huge.
    let margin = w.min(h) / 12.0;
    let fit = ((w - 2.0 * margin) / block_w.max(1.0)).min((h - 2.0 * margin) / block_h.max(1.0));
    let line = h / 12.0 / (block_h.max(1.0) / rows.len().max(1) as f32);
    let k = fit.min(line).max(0.05);
    let size = (REFERENCE * k).max(8.0);
    let left = (w - block_w * k) / 2.0;
    let values_at = left + (label_w + gap) * k;

    let mut td = klipp_timeline::TitleData::single_text(column(labels, size, true));
    td.objects[0].transform.center = TitlePosition { x: left / w, y: 0.5 };
    td.objects[0].transform.size = TitlePosition { x: (label_w * k + 1.0) / w * 1.05, y: 0.95 };
    let mut transform = klipp_timeline::default_text_transform();
    // Each block is centred on its ink, which a value's descender
    // lengthens: the values' first line is put level with the labels'.
    transform.center = TitlePosition { x: values_at / w, y: 0.5 + (value_h - label_h) * k / 2.0 / h };
    transform.size = TitlePosition { x: (value_w * k + 1.0) / w * 1.05, y: 0.95 };
    td.add_object(TitleObjectContent::TextBox(column(values, size, false)), transform);
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




/// The title renderer's stack order, evaluated before recording GPU passes.
pub(crate) fn title_style_passes(fx: &klipp_timeline::LayerFx, scale: f32, time: f64) -> Vec<klipp_effects::gpu::TitleStyle> {
    use klipp_effects::gpu::TitleStyle;
    let mut passes=Vec::new();
    let mut glows:Vec<_>=fx.glows.iter().enumerate().filter(|(i,g)|g.enabled&&(*i==0||fx.glow_stack_enabled)).map(|(_,g)|(g.evaluated_radius(time),g)).collect();
    glows.sort_by(|a,b|b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (radius,g) in glows {passes.push(TitleStyle::Glow{radius:radius*scale,color:g.color,intensity:g.evaluated_intensity(time)});}
    for (i,s) in fx.shadows.iter().enumerate().rev() {
        if s.enabled&&(i==0||fx.shadow_stack_enabled){passes.push(TitleStyle::Shadow{offset:[(s.evaluated_offset_x(time)*scale).round() as i32,(s.evaluated_offset_y(time)*scale).round() as i32],radius:s.evaluated_blur(time)*scale,color:s.color});}
    }
    let mut outlines:Vec<_>=fx.outlines.iter().enumerate().filter(|(i,o)|o.enabled&&(*i==0||fx.outline_stack_enabled)).map(|(_,o)|(o.evaluated_width(time),o)).collect();
    outlines.sort_by(|a,b|b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (radius,o) in outlines {passes.push(TitleStyle::Outline{radius:radius*scale,color:o.color});}
    passes.push(TitleStyle::Face);
    for (i,s) in fx.inner_shadows.iter().enumerate().rev() {
        if s.enabled&&(i==0||fx.inner_shadow_stack_enabled){passes.push(TitleStyle::InnerShadow{offset:[(s.evaluated_offset_x(time)*scale).round() as i32,(s.evaluated_offset_y(time)*scale).round() as i32],radius:s.evaluated_blur(time)*scale,color:s.color});}
    }
    passes
}

#[cfg(test)]
mod tests {
    /// A slate in half floats is its byte frame's picture within a code
    /// value, and its words' edges keep the fractions the bytes rounded
    /// away (#190).
    #[test]
    fn a_slate_in_half_floats_is_its_byte_frame_unrounded() {
        let slate = klipp_core::GeneratorData::Slate {
            project: "Two Shores".into(),
            scene: "12".into(),
            take: "3".into(),
            director: "Ana Ruiz".into(),
            camera: "A".into(),
            date: "2026-10-13".into(),
            framerate: "25".into(),
            notes: String::new(),
            text_color: [240, 240, 240, 255],
            background_color: [20, 24, 32, 255],
        };
        let (w, h) = (640u32, 360u32);
        let bytes = super::render_generator_with_slate(&mut klipp_title::TitleRenderer::new(), &slate, w, h, 0.0, 5.0);
        let half = super::render_generator_with_slate_f16(&mut klipp_title::TitleRenderer::new(), &slate, w, h, 0.0, 5.0);
        let un = |bits: u32| half::f16::from_bits(bits as u16).to_f32() * 255.0;
        let mut worst = 0.0f32;
        let mut fractional = 0usize;
        for (b, p) in bytes.chunks_exact(4).zip(half.chunks_exact(2)) {
            let got = [un(p[0] & 0xFFFF), un(p[0] >> 16), un(p[1] & 0xFFFF), un(p[1] >> 16)];
            for c in 0..4 {
                worst = worst.max((got[c] - b[c] as f32).abs());
            }
            fractional += usize::from((got[0] - got[0].round()).abs() > 0.05);
        }
        assert!(worst <= 1.0, "{worst} code values from the byte slate");
        assert!(fractional > 100, "the words' edges keep their fractions: {fractional} pixels between codes");
    }

    /// The full slate.
    fn slate() -> klipp_core::GeneratorData {
        klipp_core::GeneratorData::Slate {
            project: "Two Shores".into(),
            scene: "12".into(),
            take: "3".into(),
            director: "Ana Ruiz".into(),
            camera: "A".into(),
            date: "2026-10-13".into(),
            framerate: "25".into(),
            notes: "Wind from the west, second unit".into(),
            text_color: [240, 240, 240, 255],
            background_color: [20, 24, 32, 255],
        }
    }

    /// Where `frame`'s words are: the box round its light pixels, as
    /// fractions of the frame (left, top, right, bottom).
    fn ink(frame: &[u8], w: u32, h: u32) -> [f32; 4] {
        let (mut l, mut t, mut r, mut b) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if frame[((y * w + x) * 4) as usize] > 128 {
                    (l, t, r, b) = (l.min(x), t.min(y), r.max(x + 1), b.max(y + 1));
                }
            }
        }
        [l as f32 / w as f32, t as f32 / h as f32, r as f32 / w as f32, b as f32 / h as f32]
    }

    /// #2774: in a wide frame and an upright one alike, the slate's
    /// fields fill the frame legibly: inside a margin on every side, over
    /// most of its height or width, never past its edge.
    #[test]
    fn a_slate_fills_any_frame_inside_its_margins() {
        for (w, h) in [(1920u32, 1080u32), (1080, 1920)] {
            let frame = super::render_generator_with_slate(&mut klipp_title::TitleRenderer::new(), &slate(), w, h, 0.0, 5.0);
            let [l, t, r, b] = ink(&frame, w, h);
            assert!(l >= 0.04 && t >= 0.04 && r <= 0.96 && b <= 0.96, "{w}x{h}: inside the margins: {:?}", [l, t, r, b]);
            assert!(r - l >= 0.6 || b - t >= 0.6, "{w}x{h}: fills the frame: {:?}", [l, t, r, b]);
            assert!(((l + r) / 2.0 - 0.5).abs() < 0.1 && ((t + b) / 2.0 - 0.5).abs() < 0.1, "{w}x{h}: centred: {:?}", [l, t, r, b]);
        }
    }
}
