//! The drawn "Klipp" wordmark from `assets/branding/klipp-refined`.

const ON_DARK: &[u8] = include_bytes!("../../../../assets/branding/klipp-refined/klipp-wordmark.png");
const ON_LIGHT: &[u8] = include_bytes!("../../../../assets/branding/klipp-refined/klipp-wordmark-light.png");

/// Draw the wordmark `height` points tall (cap to descender), in the
/// version that reads on the current theme. The texture loads once per
/// version and is kept in the context's memory.
pub fn wordmark(ui: &mut egui::Ui, height: f32) -> egui::Response {
    let tex = texture(ui.ctx(), ui.visuals().dark_mode);
    ui.add(egui::Image::new((tex.id(), size(&tex, height))))
}

/// Paint the wordmark `height` points tall with its top edge centred on
/// `top_centre`, for layouts placed by hand. Returns the rect it took.
pub fn paint(ui: &egui::Ui, top_centre: egui::Pos2, height: f32) -> egui::Rect {
    let tex = texture(ui.ctx(), ui.visuals().dark_mode);
    let size = size(&tex, height);
    let rect = egui::Rect::from_min_size(egui::pos2(top_centre.x - size.x / 2.0, top_centre.y), size);
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
    rect
}

/// The wordmark's texture for a dark or a light theme, loaded once.
fn texture(ctx: &egui::Context, dark: bool) -> egui::TextureHandle {
    let id = egui::Id::new(("klipp_wordmark", dark));
    ctx.data_mut(|d| d.get_temp::<egui::TextureHandle>(id)).unwrap_or_else(|| {
        let image = crate::ui_util::decode_png_or_fallback(if dark { ON_DARK } else { ON_LIGHT });
        let tex = crate::app::alloc_texture_logged(ctx, "klipp_wordmark", image, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
        tex
    })
}

fn size(tex: &egui::TextureHandle, height: f32) -> egui::Vec2 {
    let [w, h] = tex.size();
    egui::vec2(height * w as f32 / h.max(1) as f32, height)
}
