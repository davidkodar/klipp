//! "Metal" in the Title tab (#2706), simple first: the metals as a row
//! of choices, None taking it off; then how polished it is, its edge and
//! highlights, and the key light's puck. The bevel, the brush, what it
//! reflects, the lights' colours and the rim light fold under Details.

use klipp_core::title_material::{MetalLight, MetalPreset, TitleMaterial};
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::TextBox;

use crate::theme::KlippTheme;

/// A metal amount's row: dragged, one undo step for the drag.
#[allow(clippy::too_many_arguments)]
fn row(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, clip_id: ClipId, base: &TitleMaterial, label: &str, value: f32, scale: f32, range: std::ops::RangeInclusive<f32>, default: f32, suffix: &str, apply: &dyn Fn(&mut TitleMaterial, f32)) {
    let mut v = value * scale;
    let resp = crate::widgets::scrubby_param::scrubby_param_row(ui, label, &mut v, range, default, 0.5, 0, suffix, None);
    if resp.drag_started {
        cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
    }
    if resp.changed {
        let mut next = *base;
        apply(&mut next, v / scale);
        cmds.push(AppCommand::SetTitleMaterial { clip_id, material: Some(next) });
    }
    if resp.drag_stopped {
        cmds.push(AppCommand::EndTitleObjectDrag { clip_id });
    }
}

/// A colour row: the swatch, picked, sets the colour.
fn colour(ui: &mut egui::Ui, label: &str, rgb: [u8; 3]) -> Option<[u8; 3]> {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut c = rgb;
        ui.color_edit_button_srgb(&mut c).changed().then_some(c)
    })
    .inner
}

fn to_srgb(linear: [f32; 3]) -> [u8; 3] {
    linear.map(|x| {
        let x = x.clamp(0.0, 1.0);
        let e = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u8
    })
}

fn to_linear(srgb: [u8; 3]) -> [f32; 3] {
    srgb.map(|v| {
        let x = f32::from(v) / 255.0;
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    })
}

/// A light's puck and strength; moved, the light as it then is.
fn light(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, l: MetalLight) -> Option<MetalLight> {
    let mut next = None;
    ui.horizontal(|ui| {
        ui.label(label);
        if let Some((azimuth, elevation)) = super::title_text3d::light_puck(ui, theme, l.azimuth, l.elevation) {
            next = Some(MetalLight { azimuth, elevation, ..l });
        }
    });
    next
}

/// The section for the text `tb` of the targeted layer.
pub fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: ClipId, tb: &TextBox, cmds: &mut Vec<AppCommand>) {
    let set = |m: TitleMaterial| AppCommand::SetTitleMaterial { clip_id, material: Some(m) };
    let current = tb.material.map(|m| m.preset);
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(current.is_none(), "None").on_hover_text("A painted face: its colour, gradient or pattern").clicked() && current.is_some() {
            cmds.push(AppCommand::SetTitleMaterial { clip_id, material: None });
        }
        for p in MetalPreset::ALL {
            if ui.selectable_label(current == Some(p), p.label()).clicked() && current != Some(p) {
                cmds.push(set(TitleMaterial::preset(p)));
            }
        }
    });
    let Some(base) = tb.material else { return };
    row(ui, cmds, clip_id, &base, "Polish", 1.0 - base.roughness, 100.0, 0.0..=100.0, 100.0 * (1.0 - TitleMaterial::preset(base.preset).roughness), "%", &|m, v| m.roughness = 1.0 - v.clamp(0.0, 1.0));
    row(ui, cmds, clip_id, &base, "Edge", base.edge, 100.0, 0.0..=100.0, 100.0 * TitleMaterial::preset(base.preset).edge, "%", &|m, v| m.edge = v.clamp(0.0, 1.0));
    row(ui, cmds, clip_id, &base, "Highlights", base.highlights, 100.0, 0.0..=200.0, 100.0, "%", &|m, v| m.highlights = v.clamp(0.0, 2.0));
    row(ui, cmds, clip_id, &base, "Glints", base.glints, 100.0, 0.0..=100.0, 100.0 * TitleMaterial::preset(base.preset).glints, "%", &|m, v| m.glints = v.clamp(0.0, 1.0));
    if let Some(key) = light(ui, theme, "Light", base.key_light) {
        cmds.push(set(TitleMaterial { key_light: key, ..base }));
    }
    egui::CollapsingHeader::new("Details").id_salt(("title_material", clip_id)).show(ui, |ui| {
        if let Some(c) = colour(ui, "Colour", to_srgb(base.tint)) {
            cmds.push(set(TitleMaterial { tint: to_linear(c), ..base }));
        }
        row(ui, cmds, clip_id, &base, "Bevel", base.bevel, 100.0, 0.0..=30.0, 6.0, "%", &|m, v| m.bevel = v.clamp(0.0, 0.3));
        row(ui, cmds, clip_id, &base, "Bevel depth", base.depth, 100.0, 0.0..=100.0, 70.0, "%", &|m, v| m.depth = v.clamp(0.0, 1.0));
        row(ui, cmds, clip_id, &base, "Contour", base.contour, 100.0, 0.0..=100.0, 100.0 * TitleMaterial::preset(base.preset).contour, "%", &|m, v| m.contour = v.clamp(0.0, 1.0));
        row(ui, cmds, clip_id, &base, "Brushed", base.anisotropy, 100.0, 0.0..=100.0, 0.0, "%", &|m, v| m.anisotropy = v.clamp(0.0, 1.0));
        if base.anisotropy > 0.0 {
            row(ui, cmds, clip_id, &base, "Brush angle", base.anisotropy_angle, 1.0, -180.0..=180.0, 0.0, "\u{00b0}", &|m, v| m.anisotropy_angle = v);
        }
        ui.label(egui::RichText::new("Reflects").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        row(ui, cmds, clip_id, &base, "Studio", base.environment.studio, 100.0, 0.0..=200.0, 100.0 * TitleMaterial::preset(base.preset).environment.studio, "%", &|m, v| m.environment.studio = v.clamp(0.0, 2.0));
        row(ui, cmds, clip_id, &base, "Sky", base.environment.sky, 100.0, 0.0..=200.0, 100.0 * TitleMaterial::preset(base.preset).environment.sky, "%", &|m, v| m.environment.sky = v.clamp(0.0, 2.0));
        row(ui, cmds, clip_id, &base, "Shot behind", base.environment.shot, 100.0, 0.0..=200.0, 100.0 * TitleMaterial::preset(base.preset).environment.shot, "%", &|m, v| m.environment.shot = v.clamp(0.0, 2.0));
        row(ui, cmds, clip_id, &base, "Shot blur", base.environment.shot_blur, 100.0, 0.0..=20.0, 4.0, "%", &|m, v| m.environment.shot_blur = v.clamp(0.0, 0.2));
        if let Some(c) = colour(ui, "Sky colour", base.environment.sky_tint) {
            let mut next = base;
            next.environment.sky_tint = c;
            cmds.push(set(next));
        }
        row(ui, cmds, clip_id, &base, "Light strength", base.key_light.strength, 100.0, 0.0..=400.0, 100.0, "%", &|m, v| m.key_light.strength = v.clamp(0.0, 4.0));
        if let Some(c) = colour(ui, "Light colour", base.key_light.color) {
            cmds.push(set(TitleMaterial { key_light: MetalLight { color: c, ..base.key_light }, ..base }));
        }
        let mut rim_on = base.rim_light.is_some();
        if ui.checkbox(&mut rim_on, "Rim light").on_hover_text("A second light catching the edges from behind").changed() {
            let rim = rim_on.then(|| TitleMaterial::preset(base.preset).rim_light.unwrap_or(base.key_light));
            cmds.push(set(TitleMaterial { rim_light: rim, ..base }));
        }
        if let Some(rim) = base.rim_light {
            if let Some(l) = light(ui, theme, "Rim", rim) {
                cmds.push(set(TitleMaterial { rim_light: Some(l), ..base }));
            }
            row(ui, cmds, clip_id, &base, "Rim strength", rim.strength, 100.0, 0.0..=400.0, 60.0, "%", &|m, v| {
                if let Some(r) = &mut m.rim_light {
                    r.strength = v.clamp(0.0, 4.0);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The section for `tb`, drawn twice, with the pointer pressed and
    /// released on the text `click` when given: what it sends.
    fn drawn(tb: &TextBox, click: Option<&str>) -> Vec<AppCommand> {
        let ctx = egui::Context::default();
        let clip_id = ClipId::new();
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let mut at = None;
        for round in 0..4 {
            let mut input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 900.0))), ..Default::default() };
            if let Some(pos) = at {
                input.events.push(egui::Event::PointerMoved(pos));
                input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: round == 2, modifiers: Default::default() });
            }
            cmds.clear();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &theme, clip_id, tb, &mut cmds));
            });
            if at.is_none() {
                at = click.and_then(|c| out.shapes.iter().find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == c => Some(t.pos + t.galley.size() / 2.0),
                    _ => None,
                }));
            }
        }
        cmds
    }

    /// #2706: picking Gold makes the text gold as the preset makes it;
    /// None takes it off; a metal text shows its controls.
    #[test]
    fn picking_a_metal_sets_it() {
        let plain = TextBox::default();
        let cmds = drawn(&plain, Some("Gold"));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleMaterial { material: Some(m), .. }] if *m == TitleMaterial::preset(MetalPreset::Gold)), "{cmds:?}");
        let gold = TextBox { material: Some(TitleMaterial::preset(MetalPreset::Gold)), ..Default::default() };
        let cmds = drawn(&gold, Some("None"));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleMaterial { material: None, .. }]), "{cmds:?}");
        let cmds = drawn(&gold, Some("Chrome"));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleMaterial { material: Some(m), .. }] if m.preset == MetalPreset::Chrome), "{cmds:?}");
    }

    /// The Gold look makes the text gold, its colour the metal's for a
    /// frame without a GPU; a painted look takes the metal off.
    #[test]
    fn the_gold_look_is_metal_and_a_painted_look_is_not() {
        let mut tb = TextBox::default();
        let mut fx = klipp_timeline::LayerFx::default();
        crate::app::apply_title_preset(&mut tb, &mut fx, 6);
        assert_eq!(tb.material, Some(TitleMaterial::preset(MetalPreset::Gold)));
        assert_eq!(tb.color, TitleMaterial::preset(MetalPreset::Gold).fallback_color());
        crate::app::apply_title_preset(&mut tb, &mut fx, 2);
        assert_eq!((tb.material, tb.color), (None, [255, 255, 255, 255]));
    }
}
