//! "3D text" in the Title tab (#1512), simple first: a switch, three
//! looks, a Turn and a Depth slider and a puck for the light's
//! direction; the bevels, the camera, the rest of the light and the
//! colours fold under Details. The numbers key at the playhead through
//! the title layer keyframe commands, as `text3d.*`.

use klipp_core::text3d::{Bevel, BevelProfile, Text3d, LOOKS};
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{TextBox, TitleObjectId};

use crate::theme::KlippTheme;

fn look_label(name: &str) -> &'static str {
    match name {
        "block" => "Block",
        "bevelled" => "Bevelled",
        _ => "Chrome",
    }
}

/// The puck's point for a light at `azimuth` and `elevation` degrees,
/// in a circle of radius `r` about its centre: straight at the viewer
/// in the middle, grazing at the rim, the direction its azimuth (0 from
/// the right, 90 from below).
pub fn puck_point(azimuth: f32, elevation: f32, r: f32) -> egui::Vec2 {
    let d = (1.0 - elevation.clamp(0.0, 90.0) / 90.0) * r;
    let a = azimuth.to_radians();
    egui::vec2(a.cos() * d, a.sin() * d)
}

/// The light at the puck's point `p` from its centre, radius `r`:
/// `(azimuth, elevation)`.
pub fn puck_light(p: egui::Vec2, r: f32) -> (f32, f32) {
    let d = (p.length() / r).min(1.0);
    let azimuth = if p.length() < 1e-3 { 0.0 } else { p.y.atan2(p.x).to_degrees() };
    (azimuth, 90.0 - d * 90.0)
}

/// The puck: where the light comes from; dragged, the new direction.
pub(crate) fn light_puck(ui: &mut egui::Ui, theme: &KlippTheme, azimuth: f32, elevation: f32) -> Option<(f32, f32)> {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::click_and_drag());
    let (c, r) = (rect.center(), 30.0);
    let painter = ui.painter_at(rect);
    painter.circle_filled(c, r, theme.bg_panel_alt);
    painter.circle_stroke(c, r, egui::Stroke::new(1.0_f32, theme.border));
    let dot = c + puck_point(azimuth, elevation, r);
    painter.line_segment([c, dot], egui::Stroke::new(1.0_f32, theme.text_secondary));
    painter.circle_filled(dot, 5.0, theme.accent);
    resp.on_hover_text("Where the light comes from: the middle straight on, the rim from the side").interact_pointer_pos().filter(|_| ui.input(|i| i.pointer.primary_down())).map(|p| puck_light(p - c, r))
}

/// The section for the text `tb` of the targeted layer (`target`), at
/// the clip's time `t`.
#[allow(clippy::too_many_arguments)]
pub fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: ClipId, target: Option<TitleObjectId>, tb: &TextBox, t: f64, tolerance: f64, cmds: &mut Vec<AppCommand>) {
    let set = |three_d: Text3d| AppCommand::SetTitleText3d { clip_id, three_d: Some(three_d) };
    let on = tb.three_d.as_ref().is_some_and(|t| t.enabled);
    ui.horizontal(|ui| {
        let mut want = on;
        if ui.checkbox(&mut want, "3D text").on_hover_text("The letters as solids: extruded, bevelled, turned and lit").changed() {
            let base = tb.three_d.clone().unwrap_or_else(|| Text3d::default().with_look("bevelled").unwrap_or_default());
            cmds.push(set(Text3d { enabled: want, ..base }));
        }
    });
    let Some(base) = tb.three_d.clone().filter(|_| on) else { return };
    if tb.extrusion.as_ref().is_some_and(|e| e.enabled) {
        ui.label(egui::RichText::new("Drawn in place of the Extrusion below").size(crate::theme::type_scale::CAPTION).color(theme.text_accent));
    }
    let now = base.at(t);
    ui.horizontal(|ui| {
        let current = base.look();
        for name in LOOKS {
            if ui.selectable_label(current == Some(name), look_label(name)).clicked() {
                if let Some(l) = base.with_look(name) {
                    cmds.push(set(l));
                }
            }
        }
    });
    // A keyable row: its value at the playhead shown times `scale`
    // (100 for a percentage), set or keyed in the model's units.
    let row = |ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, label: &str, key: &str, value: f32, scale: f32, range: std::ops::RangeInclusive<f32>, default: f32, suffix: &str, apply: &dyn Fn(&mut Text3d, f32)| {
        let keys = base.keys.get(key).unwrap_or(&[]);
        let ks = target.map(|_| crate::inspector::keyframe_selection::state(keys, t, tolerance));
        let mut v = value * scale;
        let resp = crate::widgets::scrubby_param::scrubby_param_row(ui, label, &mut v, range, default, 0.5, 0, suffix, ks);
        super::handle_text_key_edit(cmds, clip_id, target, &format!("text3d.{key}"), v / scale, keys, t, tolerance, (&resp).into(), |m| {
            let mut next = base.clone();
            apply(&mut next, m);
            AppCommand::SetTitleText3d { clip_id, three_d: Some(next) }
        });
    };
    row(ui, cmds, "Turn", "rotate_y", now.rotation[1], 1.0, -180.0..=180.0, 0.0, "\u{00b0}", &|n, v| n.rotation[1] = v);
    row(ui, cmds, "Depth", "depth", now.depth, 100.0, 0.0..=200.0, 25.0, "%", &|n, v| n.depth = v.max(0.0));
    ui.horizontal(|ui| {
        ui.label("Light");
        if let Some((azimuth, elevation)) = light_puck(ui, theme, now.light.azimuth, now.light.elevation) {
            if base.keys.light_azimuth.is_empty() && base.keys.light_elevation.is_empty() {
                let mut next = base.clone();
                (next.light.azimuth, next.light.elevation) = (azimuth, elevation);
                cmds.push(set(next));
            } else if let Some(oid) = target {
                // Keyed: the puck keys both at the playhead.
                for (key, value) in [("light_azimuth", azimuth), ("light_elevation", elevation)] {
                    cmds.push(AppCommand::AddTitleObjectEffectKeyframe { clip_id, object_id: oid.0, param_key: format!("text3d.{key}"), time_secs: t, value });
                }
            }
        }
    });
    egui::CollapsingHeader::new("Details").id_salt(("title_text3d", clip_id)).show(ui, |ui| {
        row(ui, cmds, "Tilt", "rotate_x", now.rotation[0], 1.0, -180.0..=180.0, 0.0, "\u{00b0}", &|n, v| n.rotation[0] = v);
        row(ui, cmds, "Roll", "rotate_z", now.rotation[2], 1.0, -180.0..=180.0, 0.0, "\u{00b0}", &|n, v| n.rotation[2] = v);
        // The bevels: none, straight or round, and how big.
        for (front, label) in [(true, "Front bevel"), (false, "Back bevel")] {
            let bevel = if front { base.front_bevel } else { base.back_bevel };
            ui.horizontal(|ui| {
                ui.label(label);
                let kind = bevel.map(|b| b.profile);
                for (choice, name) in [(None, "None"), (Some(BevelProfile::Flat), "Straight"), (Some(BevelProfile::Round), "Round")] {
                    if ui.selectable_label(kind == choice, name).clicked() && kind != choice {
                        let mut next = base.clone();
                        let b = choice.map(|profile| Bevel { profile, ..bevel.unwrap_or_default() });
                        if front { next.front_bevel = b } else { next.back_bevel = b }
                        cmds.push(set(next));
                    }
                }
            });
        }
        if let Some(b) = now.front_bevel {
            row(ui, cmds, "Bevel size", "bevel", b.size, 100.0, 0.0..=15.0, 3.0, "%", &|n, v| {
                for b in [&mut n.front_bevel, &mut n.back_bevel].into_iter().flatten() {
                    b.size = v.max(0.0);
                }
            });
        }
        row(ui, cmds, "Field of view", "fov", now.camera.fov, 1.0, 1.0..=120.0, 30.0, "\u{00b0}", &|n, v| n.camera.fov = v.clamp(1.0, 170.0));
        let mut ortho = base.camera.orthographic;
        if ui.checkbox(&mut ortho, "No perspective").changed() {
            let mut next = base.clone();
            next.camera.orthographic = ortho;
            cmds.push(set(next));
        }
        row(ui, cmds, "Light strength", "light_intensity", now.light.intensity, 100.0, 0.0..=300.0, 100.0, "%", &|n, v| n.light.intensity = v.max(0.0));
        let mut light = base.light;
        let mut light_changed = false;
        light_changed |= ui.add(egui::Slider::new(&mut light.ambient, 0.0..=1.0).text("Ambient")).changed();
        light_changed |= ui.add(egui::Slider::new(&mut light.specular, 0.0..=1.0).text("Highlight")).changed();
        light_changed |= ui.add(egui::Slider::new(&mut light.shininess, 1.0..=256.0).logarithmic(true).text("Shininess")).changed();
        if light_changed {
            cmds.push(set(Text3d { light, ..base.clone() }));
        }
        // The colours: the face its own or the text's, the walls, the
        // bevel its own or the face's.
        let swatch = |ui: &mut egui::Ui, c: [u8; 4]| -> Option<[u8; 4]> {
            let mut col = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            ui.color_edit_button_srgba(&mut col).changed().then(|| col.to_srgba_unmultiplied())
        };
        ui.horizontal(|ui| {
            let mut own = base.face_color.is_some();
            if ui.checkbox(&mut own, "Face colour").on_hover_text("Off: the text's own colour").changed() {
                cmds.push(set(Text3d { face_color: own.then_some(tb.color), ..base.clone() }));
            }
            if let Some(c) = base.face_color.and_then(|c| swatch(ui, c)) {
                cmds.push(set(Text3d { face_color: Some(c), ..base.clone() }));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Walls");
            if let Some(c) = swatch(ui, base.side_color) {
                cmds.push(set(Text3d { side_color: c, ..base.clone() }));
            }
        });
        ui.horizontal(|ui| {
            let mut own = base.bevel_color.is_some();
            if ui.checkbox(&mut own, "Bevel colour").on_hover_text("Off: the face's colour").changed() {
                cmds.push(set(Text3d { bevel_color: own.then_some(base.face_color.unwrap_or(tb.color)), ..base.clone() }));
            }
            if let Some(c) = base.bevel_color.and_then(|c| swatch(ui, c)) {
                cmds.push(set(Text3d { bevel_color: Some(c), ..base.clone() }));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Frame {
        texts: Vec<(String, egui::Pos2)>,
        diamonds: Vec<egui::Pos2>,
        cmds: Vec<AppCommand>,
    }

    fn clip() -> ClipId {
        ClipId(uuid::Uuid::nil())
    }

    fn frame(ctx: &egui::Context, tb: &TextBox, events: Vec<egui::Event>) -> Frame {
        let mut cmds = Vec::new();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2000.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &KlippTheme::default(), clip(), Some(TitleObjectId(2)), tb, 0.5, 0.01, &mut cmds));
            },
        );
        let (mut texts, mut diamonds) = (Vec::new(), Vec::new());
        for s in &out.shapes {
            match &s.shape {
                egui::Shape::Text(t) => texts.push((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                egui::Shape::Path(p) if p.closed && p.points.len() == 4 => {
                    diamonds.push((p.points.iter().fold(egui::Vec2::ZERO, |a, q| a + q.to_vec2()) / 4.0).to_pos2());
                }
                _ => {}
            }
        }
        Frame { texts, diamonds, cmds }
    }

    fn at(f: &Frame, text: &str) -> egui::Pos2 {
        f.texts.iter().find(|(t, _)| t == text).map(|(_, p)| *p).unwrap_or_else(|| panic!("{text} is drawn"))
    }

    fn click(ctx: &egui::Context, tb: &TextBox, pos: egui::Pos2) -> Vec<AppCommand> {
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, tb, vec![egui::Event::PointerMoved(pos), button(true)]).cmds;
        cmds.extend(frame(ctx, tb, vec![button(false)]).cmds);
        cmds
    }

    fn set_to(cmds: &[AppCommand]) -> Text3d {
        match cmds {
            [AppCommand::SetTitleText3d { three_d: Some(t), .. }] => t.clone(),
            other => panic!("one SetTitleText3d: {other:?}"),
        }
    }

    /// The switch turns 3D on in one step, bevelled to start with;
    /// off, nothing else shows.
    #[test]
    fn the_switch_turns_3d_on_bevelled() {
        let ctx = egui::Context::default();
        let flat = TextBox::default();
        let f = frame(&ctx, &flat, Vec::new());
        assert!(f.texts.iter().all(|(t, _)| t != "Turn"), "off, the controls are hidden");
        let t = set_to(&click(&ctx, &flat, at(&f, "3D text")));
        assert!(t.enabled);
        assert_eq!(t.look(), Some("bevelled"));
    }

    /// A look is one click; Turn and Depth show, and Turn's diamond keys
    /// `text3d.rotate_y` at the playhead; a text that also has the older
    /// extrusion says this one is drawn instead.
    #[test]
    fn looks_turn_and_depth_are_one_click_and_key_by_their_names() {
        let ctx = egui::Context::default();
        let mut tb = TextBox { three_d: Text3d::default().with_look("bevelled"), ..Default::default() };
        tb.extrusion = Some(klipp_timeline::TextExtrusion { enabled: true, depth: 10.0, angle: 135.0, face_color: [0; 4], side_color: [0; 4], lighting: 0.5, depth_keyframes: Vec::new(), angle_keyframes: Vec::new(), lighting_keyframes: Vec::new() });
        let f = frame(&ctx, &tb, Vec::new());
        at(&f, "Drawn in place of the Extrusion below");
        let chrome = set_to(&click(&ctx, &tb, at(&f, "Chrome")));
        assert_eq!(chrome.look(), Some("chrome"));
        let turn_y = at(&f, "Turn").y;
        let diamond = *f.diamonds.iter().find(|d| (d.y - turn_y).abs() < 8.0).expect("Turn has a diamond");
        assert!(f.diamonds.iter().all(|d| d.x < 420.0), "inside the panel: {:?}", f.diamonds);
        let cmds = click(&ctx, &tb, diamond);
        assert!(matches!(&cmds[..], [AppCommand::AddTitleObjectEffectKeyframe { object_id: 2, param_key, time_secs, .. }] if param_key == "text3d.rotate_y" && *time_secs == 0.5), "{cmds:?}");
        at(&f, "Depth");
    }

    /// The puck: the middle is straight on, the rim grazing, the side the
    /// light comes from its azimuth, and back again.
    #[test]
    fn the_puck_maps_the_light_both_ways() {
        let p = puck_point(0.0, 90.0, 30.0);
        assert!(p.length() < 1e-4);
        let p = puck_point(90.0, 0.0, 30.0);
        assert!((p.x).abs() < 1e-3 && (p.y - 30.0).abs() < 1e-3, "{p:?}");
        for (az, el) in [(-135.0f32, 45.0f32), (30.0, 10.0), (170.0, 70.0)] {
            let (a, e) = puck_light(puck_point(az, el, 30.0), 30.0);
            assert!((a - az).abs() < 1e-3 && (e - el).abs() < 1e-3, "{az},{el} -> {a},{e}");
        }
    }
}
