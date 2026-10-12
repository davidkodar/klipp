//! The drawn "Klipp" wordmark from `assets/branding/klipp-refined`.

const ON_DARK: &[u8] = include_bytes!("../../../../assets/branding/klipp-refined/klipp-wordmark.png");
const ON_LIGHT: &[u8] = include_bytes!("../../../../assets/branding/klipp-refined/klipp-wordmark-light.png");

/// Draw the wordmark `height` points tall (cap to descender), in the
/// version that reads on the current theme. The texture loads once per
/// version and is kept in the context's memory.
pub fn wordmark(ui: &mut egui::Ui, height: f32) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let id = egui::Id::new(("klipp_wordmark", dark));
    let tex = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(id)).unwrap_or_else(|| {
        let image = crate::ui_util::decode_png_or_fallback(if dark { ON_DARK } else { ON_LIGHT });
        let tex = crate::app::alloc_texture_logged(ui.ctx(), "klipp_wordmark", image, egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(id, tex.clone()));
        tex
    });
    let [w, h] = tex.size();
    let size = egui::vec2(height * w as f32 / h.max(1) as f32, height);
    ui.add(egui::Image::new((tex.id(), size)))
}
