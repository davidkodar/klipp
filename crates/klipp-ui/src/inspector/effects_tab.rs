use crate::icons::Icon;
use crate::theme::KlippTheme;
use crate::state::AppState;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

/// Empty state — no effects applied (shown for all selection states when no effects exist)
pub fn show_empty(ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    let available_h = ui.available_height();
    ui.add_space((available_h * 0.15).max(20.0));

    ui.vertical_centered(|ui| {
        // Centered + icon in a rounded square
        let icon_size = 40.0;
        let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(icon_size, icon_size), egui::Sense::hover());
        ui.painter().rect(icon_rect, 8.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));
        ui.painter().text(
            icon_rect.center(), egui::Align2::CENTER_CENTER,
            "+", egui::FontId::proportional(16.0), theme.text_dim,
        );

        ui.add_space(12.0);

        ui.label(
            egui::RichText::new("No effects applied")
                .size(13.0)
                .color(theme.text_primary),
        );

        ui.add_space(6.0);

        ui.label(
            egui::RichText::new("Add an effect from the Effects Browser or\nuse the button below")
                .size(11.0)
                .color(theme.text_dim),
        );

        ui.add_space(24.0);

        // "+ Add Effect..." button with "E" shortcut badge
        ui.horizontal(|ui| {
            let total_w = ui.available_width();
            let btn_w = total_w - 40.0;
            ui.add_space(10.0);

            let mut job = egui::text::LayoutJob::default();
            job.append("+  ", 0.0, egui::TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color: theme.accent,
                ..Default::default()
            });
            job.append("Add Effect\u{2026}", 0.0, egui::TextFormat {
                font_id: egui::FontId::proportional(12.0),
                color: theme.text_secondary,
                ..Default::default()
            });

            if ui.add_sized(
                egui::vec2(btn_w, 30.0),
                egui::Button::new(job)
                    .fill(theme.bg_raised)
                    .stroke(egui::Stroke::new(1.0_f32, theme.border))
                    .rounding(4.0),
            ).clicked() {
                cmds.push(AppCommand::ShowEffectsBrowser);
            }

            // "E" shortcut badge
            let (badge_rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().rect(badge_rect, 4.0, theme.bg_deep, egui::Stroke::new(0.5_f32, theme.border));
            ui.painter().text(
                badge_rect.center(), egui::Align2::CENTER_CENTER,
                "E", egui::FontId::monospace(10.0), theme.text_dim,
            );
        });
    });

    ui.add_space(16.0);
}

fn show_ofx_param(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    effect_index: usize,
    effect: &klipp_plugin::AppliedEffect,
    desc: &klipp_plugin::ParamDescriptor,
    cmds: &mut Vec<AppCommand>,
) {
    let current_value = effect.param_values.get(&desc.name);

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&desc.label)
                .size(10.0)
                .color(theme.text_dim),
        );
        ui.add_space(4.0);

        match desc.param_type {
            klipp_plugin::ParamType::Double => {
                let mut v = current_value
                    .and_then(|cv| cv.as_double())
                    .or(match &desc.default { klipp_plugin::ParamValue::Double(d) => Some(*d), _ => None })
                    .unwrap_or(0.0) as f32;
                let dmin = desc.display_min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                    .unwrap_or(0.0);
                let dmax = desc.display_max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double(d) => Some(*d as f32), _ => None })
                    .unwrap_or(1.0);
                if ui.add(egui::Slider::new(&mut v, dmin..=dmax).step_by(0.01_f64)).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Double(v as f64),
                    });
                }
            }
            klipp_plugin::ParamType::Int => {
                let mut v = current_value
                    .and_then(|cv| cv.as_int())
                    .or(match &desc.default { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(0);
                let imin = desc.min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(0);
                let imax = desc.max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Int(i) => Some(*i), _ => None })
                    .unwrap_or(100);
                if ui.add(egui::DragValue::new(&mut v).range(imin..=imax).speed(1)).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Int(v),
                    });
                }
            }
            klipp_plugin::ParamType::Bool => {
                let mut v = current_value
                    .and_then(|cv| cv.as_bool())
                    .or(match &desc.default { klipp_plugin::ParamValue::Bool(b) => Some(*b), _ => None })
                    .unwrap_or(false);
                if ui.checkbox(&mut v, "").changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Bool(v),
                    });
                }
            }
            klipp_plugin::ParamType::Choice => {
                let mut v = current_value
                    .and_then(|cv| cv.as_int())
                    .or(match &desc.default { klipp_plugin::ParamValue::Choice(c) => Some(*c), _ => None })
                    .unwrap_or(0);
                let selected_text = desc.choices.get(v as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                egui::ComboBox::from_id_salt(egui::Id::new(("ofx_choice", clip_id, effect_index, &desc.name)))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (ci, choice) in desc.choices.iter().enumerate() {
                            if ui.selectable_value(&mut v, ci as i32, choice).changed() {
                                cmds.push(AppCommand::SetEffectParam {
                                    clip_id,
                                    effect_index,
                                    param_name: desc.name.clone(),
                                    value: klipp_plugin::OfxValue::Choice(v),
                                });
                            }
                        }
                    });
            }
            klipp_plugin::ParamType::Rgba => {
                let (r, g, b, a) = match current_value {
                    Some(klipp_plugin::OfxValue::Rgba(r, g, b, a)) => (*r, *g, *b, *a),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Rgba(r, g, b, a) => (*r, *g, *b, *a),
                        _ => (1.0, 1.0, 1.0, 1.0),
                    },
                };
                let mut color = egui::Color32::from_rgba_unmultiplied(
                    (r * 255.0) as u8, (g * 255.0) as u8,
                    (b * 255.0) as u8, (a * 255.0) as u8,
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Rgba(
                            color.r() as f64 / 255.0,
                            color.g() as f64 / 255.0,
                            color.b() as f64 / 255.0,
                            color.a() as f64 / 255.0,
                        ),
                    });
                }
            }
            klipp_plugin::ParamType::Rgb => {
                let (r, g, b) = match current_value {
                    Some(klipp_plugin::OfxValue::Rgb(r, g, b)) => (*r, *g, *b),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Rgb(r, g, b) => (*r, *g, *b),
                        _ => (1.0, 1.0, 1.0),
                    },
                };
                let mut color = egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                );
                if ui.color_edit_button_srgba(&mut color).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id,
                        effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::Rgb(
                            color.r() as f64 / 255.0,
                            color.g() as f64 / 255.0,
                            color.b() as f64 / 255.0,
                        ),
                    });
                }
            }
            klipp_plugin::ParamType::Double2 => {
                let (mut v0, mut v1) = match current_value {
                    Some(klipp_plugin::OfxValue::Double2(a, b)) => (*a as f32, *b as f32),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Double2(a, b) => (*a as f32, *b as f32),
                        _ => (0.0, 0.0),
                    },
                };
                let dmin = desc.display_min.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double2(a, _) => Some(*a as f32), klipp_plugin::ParamValue::Double(a) => Some(*a as f32), _ => None })
                    .unwrap_or(0.0);
                let dmax = desc.display_max.as_ref()
                    .and_then(|m| match m { klipp_plugin::ParamValue::Double2(a, _) => Some(*a as f32), klipp_plugin::ParamValue::Double(a) => Some(*a as f32), _ => None })
                    .unwrap_or(1000.0);
                ui.horizontal(|ui| {
                    let changed0 = ui.add(egui::DragValue::new(&mut v0).range(dmin..=dmax).speed(0.1).prefix("x: ")).changed();
                    let changed1 = ui.add(egui::DragValue::new(&mut v1).range(dmin..=dmax).speed(0.1).prefix("y: ")).changed();
                    if changed0 || changed1 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Double2(v0 as f64, v1 as f64),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Double3 => {
                let (mut v0, mut v1, mut v2) = match current_value {
                    Some(klipp_plugin::OfxValue::Double3(a, b, c)) => (*a as f32, *b as f32, *c as f32),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Double3(a, b, c) => (*a as f32, *b as f32, *c as f32),
                        _ => (0.0, 0.0, 0.0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(0.1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(0.1).prefix("y: ")).changed();
                    let c2 = ui.add(egui::DragValue::new(&mut v2).speed(0.1).prefix("z: ")).changed();
                    if c0 || c1 || c2 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Double3(v0 as f64, v1 as f64, v2 as f64),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Int2 => {
                let (mut v0, mut v1) = match current_value {
                    Some(klipp_plugin::OfxValue::Int2(a, b)) => (*a, *b),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Int2(a, b) => (*a, *b),
                        _ => (0, 0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(1).prefix("y: ")).changed();
                    if c0 || c1 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Int2(v0, v1),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::Int3 => {
                let (mut v0, mut v1, mut v2) = match current_value {
                    Some(klipp_plugin::OfxValue::Int3(a, b, c)) => (*a, *b, *c),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::Int3(a, b, c) => (*a, *b, *c),
                        _ => (0, 0, 0),
                    },
                };
                ui.horizontal(|ui| {
                    let c0 = ui.add(egui::DragValue::new(&mut v0).speed(1).prefix("x: ")).changed();
                    let c1 = ui.add(egui::DragValue::new(&mut v1).speed(1).prefix("y: ")).changed();
                    let c2 = ui.add(egui::DragValue::new(&mut v2).speed(1).prefix("z: ")).changed();
                    if c0 || c1 || c2 {
                        cmds.push(AppCommand::SetEffectParam {
                            clip_id, effect_index,
                            param_name: desc.name.clone(),
                            value: klipp_plugin::OfxValue::Int3(v0, v1, v2),
                        });
                    }
                });
            }
            klipp_plugin::ParamType::String => {
                let mut v = match current_value {
                    Some(klipp_plugin::OfxValue::String(s)) => s.clone(),
                    _ => match &desc.default {
                        klipp_plugin::ParamValue::String(s) => s.clone(),
                        _ => String::new(),
                    },
                };
                if ui.text_edit_singleline(&mut v).changed() {
                    cmds.push(AppCommand::SetEffectParam {
                        clip_id, effect_index,
                        param_name: desc.name.clone(),
                        value: klipp_plugin::OfxValue::String(v),
                    });
                }
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!("{:?}", desc.param_type))
                        .size(10.0)
                        .color(theme.text_dim),
                );
            }
        }
    });
}

/// Returns the default (identity) version of the same effect variant.
#[allow(dead_code)]
fn builtin_effect_default(effect: &klipp_effects::BuiltinEffect) -> klipp_effects::BuiltinEffect {
    use klipp_effects::BuiltinEffect;
    match effect {
        BuiltinEffect::ColorFade { mode, .. } => BuiltinEffect::ColorFade {
            color: [0, 0, 0, 255], duration_secs: 1.0, mode: *mode, progress: 0.0,
        },
        BuiltinEffect::Opacity { .. } => BuiltinEffect::Opacity { value: 1.0 },
        BuiltinEffect::GaussianBlur { .. } => BuiltinEffect::GaussianBlur { radius: 0.0 },
        BuiltinEffect::RoundedCorners { .. } => BuiltinEffect::RoundedCorners { radius: 0.0, feather: 0.0 },
        BuiltinEffect::DropShadow { .. } => BuiltinEffect::DropShadow {
            offset_x: 0.0, offset_y: 0.0, blur: 0.0, color: [0, 0, 0], opacity: 0.0,
        },
        BuiltinEffect::OuterGlow { .. } => BuiltinEffect::OuterGlow { radius: 0.0, intensity: 0.0, color: [255, 255, 255] },
        // No setting of it fills nothing; this is where it starts.
        BuiltinEffect::FillBars { .. } => BuiltinEffect::FillBars { blur: 0.05, zoom: 1.0, darken: 0.25 },
        BuiltinEffect::Sharpen { .. } => BuiltinEffect::Sharpen { amount: 0.0 },
        BuiltinEffect::Crop { .. } => BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
        BuiltinEffect::Transform { .. } => BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None },
        BuiltinEffect::ChromaKey { .. } => BuiltinEffect::ChromaKey {
            key_color: [0, 255, 0], tolerance: 0.2, soft_edge: 0.1,
            spill_method: klipp_effects::SpillMethod::ChannelClamp, spill_strength: 0.6,
        },
        BuiltinEffect::LumaKey { .. } => BuiltinEffect::LumaKey { low: 0.0, high: 0.1, soft: 0.05, invert: false },
        BuiltinEffect::CornerPin { .. } => klipp_effects::corner_pin::still(),
        // The shape stays: a reset is its settings.
        BuiltinEffect::Warp { mode, .. } => {
            let Some(BuiltinEffect::Warp { amplitude, wavelength, speed, direction, angle, strength, radius, center_x, center_y, band, .. }) = klipp_effects::registry::effect_type("warp").map(|t| (t.sample)()) else { return effect.clone() };
            BuiltinEffect::Warp { mode: *mode, amplitude, wavelength, speed, direction, angle, strength, radius, center_x, center_y, band, time: 0.0 }
        }
        // The surface stays: a reset is its edges.
        BuiltinEffect::SurfacePin { surface, .. } => BuiltinEffect::SurfacePin { surface: surface.clone(), inset: 0.0, on_source: [[0.0; 2]; 4], source: [0; 2] },
        // The file stays: a reset is the look at full strength.
        BuiltinEffect::Lut3D { path, .. } => BuiltinEffect::Lut3D { path: path.clone(), opacity: 1.0, tetrahedral: true, prepared: None },
        BuiltinEffect::PictureTransform { .. } => BuiltinEffect::PictureTransform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
        },
        BuiltinEffect::Merge { .. } => BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Normal, opacity: 1.0 },
        // The colours stay: a reset is the shape and its placing.
        BuiltinEffect::Generator { color_a, color_b, .. } => BuiltinEffect::Generator {
            shape: 0, color_a: *color_a, color_b: *color_b, angle: 0.0, center_x: 0.5, center_y: 0.5, radius: 0.5,
        },
        // The item stays: a reset is its time.
        BuiltinEffect::MediaSource { media, .. } => BuiltinEffect::MediaSource { media: media.clone(), offset: 0.0 },
        // The title stays: it has nothing to reset to.
        BuiltinEffect::TitleSource { title } => BuiltinEffect::TitleSource { title: title.clone() },
        BuiltinEffect::ClipBelow {} => BuiltinEffect::ClipBelow {},
        BuiltinEffect::GradeCompound {} => BuiltinEffect::GradeCompound {},
        BuiltinEffect::ColourSpaceTransform { .. } => BuiltinEffect::ColourSpaceTransform {
            input_gamut: 0, input_curve: 0, output_gamut: 0, output_curve: 0, prepared: None,
        },
        BuiltinEffect::Bloom { .. } => BuiltinEffect::Bloom {
            threshold: 0.7, radius: 20.0, intensity: 0.5,
        },
        BuiltinEffect::Glow { .. } => BuiltinEffect::Glow {
            radius: 30.0, intensity: 0.4, threshold: 0.6,
        },
        BuiltinEffect::LightRays { .. } => BuiltinEffect::LightRays {
            center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 64, decay: 0.3, intensity: 0.5,
        },
        BuiltinEffect::Vignette { .. } => BuiltinEffect::Vignette {
            amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5,
        },
        BuiltinEffect::BinocularVignette { .. } => BuiltinEffect::BinocularVignette {
            amount: -1.0, spacing: 0.4, width: 0.85, height: 0.85, softness: 0.15, fringe: 4.0,
        },
        BuiltinEffect::Flicker { .. } => BuiltinEffect::Flicker {
            amount: 0.5, speed: 8.0, smoothness: 0.0, hold: 0.3, seed: 67, target: 0, time: 0.0,
        },
        // The kind stays: a reset sets snow's numbers back, not dust's.
        BuiltinEffect::Particles { style: kind, .. } => crate::effect_presets::particles_default(*kind),
        BuiltinEffect::Diffusion { .. } => BuiltinEffect::Diffusion {
            amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2,
        },
        BuiltinEffect::LensFlare { .. } => BuiltinEffect::LensFlare {
            position_x: 0.7, position_y: 0.3, brightness: 0.6,
            flare_size: 0.15, ghost_count: 4, ghost_intensity: 0.3,
            chromatic_aberration: 0.4,
        },
        BuiltinEffect::LightWrap { .. } => BuiltinEffect::LightWrap {
            width: 15.0, intensity: 0.5, softness: 1.0,
        },
        BuiltinEffect::AnamorphicStreak { .. } => BuiltinEffect::AnamorphicStreak {
            threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3,
        },
        BuiltinEffect::FilmHalation { .. } => BuiltinEffect::FilmHalation {
            threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2,
        },
        BuiltinEffect::Invert { .. } => BuiltinEffect::Invert { mix: 1.0 },
        BuiltinEffect::Posterize { .. } => BuiltinEffect::Posterize { levels: 8.0 },
        BuiltinEffect::Mosaic { .. } => BuiltinEffect::Mosaic { block_size: 10.0 },
        BuiltinEffect::EdgeDetect { .. } => BuiltinEffect::EdgeDetect { strength: 1.0, mix: 1.0 },
        BuiltinEffect::ChannelMixer { .. } => BuiltinEffect::ChannelMixer {
            red_red: 1.0, red_green: 0.0, red_blue: 0.0,
            green_red: 0.0, green_green: 1.0, green_blue: 0.0,
            blue_red: 0.0, blue_green: 0.0, blue_blue: 1.0,
        },
        BuiltinEffect::DirectionalBlur { .. } => BuiltinEffect::DirectionalBlur {
            angle: 0.0, distance: 20.0, samples: 8,
        },
        BuiltinEffect::Emboss { .. } => BuiltinEffect::Emboss {
            angle: 135.0, strength: 1.0, mix: 1.0,
        },
        BuiltinEffect::Mirror { .. } => BuiltinEffect::Mirror {
            horizontal: 1, vertical: 0,
        },
        BuiltinEffect::LensDistortion { .. } => BuiltinEffect::LensDistortion {
            distortion: 0.0, scale: 1.0,
        },
        BuiltinEffect::Reflection { waterline, .. } => klipp_effects::reflection::surface("calm_water", *waterline).expect("a surface"),
        BuiltinEffect::Displace { .. } => BuiltinEffect::Displace {
            amount: 12.0, size: 80.0, speed: 1.0, direction: 0, seed: 0, time: 0.0,
        },
        BuiltinEffect::Dilate { .. } => BuiltinEffect::Dilate { radius: 1 },
        BuiltinEffect::Erode { .. } => BuiltinEffect::Erode { radius: 1 },
        BuiltinEffect::Denoise { .. } => BuiltinEffect::Denoise {
            spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1,
        },
        BuiltinEffect::Gamma { .. } => BuiltinEffect::Gamma { gamma: 1.0 },
        BuiltinEffect::HSVAdjust { .. } => BuiltinEffect::HSVAdjust {
            hue_shift: 0.0, saturation: 1.0, value: 1.0,
        },
        BuiltinEffect::NoiseGenerator { .. } => BuiltinEffect::NoiseGenerator {
            amount: 0.1, monochrome: 0, seed: 0,
        },
        BuiltinEffect::FilmGrain { .. } => BuiltinEffect::FilmGrain {
            amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0,
        },
        BuiltinEffect::TiltShift { .. } => BuiltinEffect::TiltShift {
            center: 0.5, width: 0.3, softness: 0.2, radius: 12.0,
        },
        BuiltinEffect::FocusBlur { .. } => BuiltinEffect::FocusBlur { radius: 8.0, vertical: true, band: 0.3 },
        BuiltinEffect::OldFilm { .. } => BuiltinEffect::OldFilm {
            flicker: 0.4, scratches: 0.5, dust: 0.5, weave: 0.5, sepia: 0.6, seed: 0, amount: 1.0, age: 0.0, mono: 0.0, stains: 0.0, frame_lines: 0.0, leaks: 0.0, burn: 0.0,
        },
        BuiltinEffect::Vhs { .. } => BuiltinEffect::Vhs {
            tracking: 0.5, bleed: 0.5, noise: 0.4, scanlines: 0.5, seed: 0,
        },
        BuiltinEffect::MedianFilter { .. } => BuiltinEffect::MedianFilter { radius: 1 },
        BuiltinEffect::RadialBlur { .. } => BuiltinEffect::RadialBlur {
            center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0,
        },
        BuiltinEffect::Levels { .. } => BuiltinEffect::Levels {
            in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
        },
        BuiltinEffect::Equalize { .. } => BuiltinEffect::Equalize { strength: 1.0 },
        BuiltinEffect::Letterbox { .. } => BuiltinEffect::Letterbox {
            aspect_ratio: 2.39, bar_color: 0, opacity: 1.0,
        },
        BuiltinEffect::ChromaticAberration { .. } => BuiltinEffect::ChromaticAberration {
            red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5,
        },
        BuiltinEffect::RollingShutterCorrection { .. } => BuiltinEffect::RollingShutterCorrection {
            strength: 0.5, readout_ratio: 0.5,
        },
        BuiltinEffect::TemporalDenoise { .. } => BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 },
        BuiltinEffect::Deflicker { .. } => BuiltinEffect::Deflicker { strength: 0.8, window_secs: 1.0, gain: 1.0 },
        BuiltinEffect::VideoStabilization { .. } => BuiltinEffect::VideoStabilization {
            smoothing: 0.5, crop_ratio: 0.05, zoom: true,
        },
        BuiltinEffect::SpeedRamp { .. } => BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 1 },
        klipp_effects::BuiltinEffect::Vibrance { .. } => {
            klipp_effects::BuiltinEffect::Vibrance { amount: 0.0 }
        },
        klipp_effects::BuiltinEffect::Frei0r { type_id, .. } => {
            // "Default" for a frei0r effect = same plugin, fresh
            // default param values from the registry. If the plugin
            // disappeared since the project was saved, fall through to
            // an empty params list (the node refuses a plug-in that
            // isn't installed and passes its input on).
            klipp_effects::BuiltinEffect::Frei0r {
                type_id: type_id.clone(),
                params: klipp_effects::frei0r_effect::default_params(type_id)
                    .unwrap_or_default(),
            }
        },
    }
}

#[allow(dead_code)]
fn builtin_effect_has_multi_params(effect: &klipp_effects::BuiltinEffect) -> bool {
    use klipp_effects::BuiltinEffect;
    matches!(effect, BuiltinEffect::Crop { .. } | BuiltinEffect::Transform { .. } | BuiltinEffect::ChromaKey { .. } | BuiltinEffect::LumaKey { .. } | BuiltinEffect::Bloom { .. } | BuiltinEffect::LightRays { .. }
        | BuiltinEffect::Vignette { .. } | BuiltinEffect::Diffusion { .. } | BuiltinEffect::LensFlare { .. }
        | BuiltinEffect::LightWrap { .. } | BuiltinEffect::AnamorphicStreak { .. } | BuiltinEffect::FilmHalation { .. }
        | BuiltinEffect::EdgeDetect { .. } | BuiltinEffect::ChannelMixer { .. } | BuiltinEffect::DirectionalBlur { .. }
        | BuiltinEffect::Emboss { .. } | BuiltinEffect::Mirror { .. } | BuiltinEffect::LensDistortion { .. }
        | BuiltinEffect::Displace { .. }
        | BuiltinEffect::Reflection { .. }
        | BuiltinEffect::Denoise { .. } | BuiltinEffect::RollingShutterCorrection { .. } | BuiltinEffect::VideoStabilization { .. } | BuiltinEffect::SpeedRamp { .. }
        | BuiltinEffect::Deflicker { .. } | BuiltinEffect::TemporalDenoise { .. }
        | BuiltinEffect::HSVAdjust { .. } | BuiltinEffect::NoiseGenerator { .. }
        | BuiltinEffect::RadialBlur { .. } | BuiltinEffect::Levels { .. }
        | BuiltinEffect::Letterbox { .. } | BuiltinEffect::ChromaticAberration { .. }
        | BuiltinEffect::FilmGrain { .. } | BuiltinEffect::TiltShift { .. }
        | BuiltinEffect::OldFilm { .. } | BuiltinEffect::Vhs { .. }
        | BuiltinEffect::Flicker { .. } | BuiltinEffect::BinocularVignette { .. } | BuiltinEffect::Particles { .. })
}

#[allow(dead_code)]
fn builtin_effect_is_default(effect: &klipp_effects::BuiltinEffect) -> bool {
    let def = builtin_effect_default(effect);
    // Compare by debug string — simple and correct for all variants
    format!("{:?}", effect) == format!("{:?}", def)
}

/// The line under Deflicker's window row: what the window decides.
pub(crate) const DEFLICKER_WINDOW_HINT: &str =
    "Variation faster than the window is flicker and goes; slower is the shot's own brightness and stays.";

/// Temporal Noise Reduction's note on where it renders.
pub(crate) const TEMPORAL_DENOISE_CPU_NOTE: &str =
    "A clip using this is rendered on the CPU rather than the GPU: the frames it averages against are decoded there.";

/// `clip`'s analysis of `kind`, derived with the settings of its first
/// enabled effect that `is` (M4 step 9): what the render draws by. Kept
/// in the panel's memory for the clip while the analysis read (the same
/// `Arc`, held there) and the settings hold, so a frame of the inspector costs no
/// derivation.
fn derived_analysis(ctx: &egui::Context, clip: &Clip, state: &AppState, kind: klipp_analysis::assets::AssetKind, is: fn(&klipp_effects::BuiltinEffect) -> bool) -> Option<std::sync::Arc<klipp_effects::node_analysis::NodeAnalysis>> {
    // The analysis itself is held, so its address can't be another's.
    type Kept = (std::sync::Arc<serde_json::Value>, u64, Option<std::sync::Arc<klipp_effects::node_analysis::NodeAnalysis>>);
    let raw = state.analysis_of(clip, kind)?;
    let effect = clip.enabled_effects().find(|e| is(e))?;
    let params = klipp_effects::node_analysis::derivation_params(effect)?;
    let id = egui::Id::new(("derived_analysis", clip.id, kind));
    if let Some((seen, p, kept)) = ctx.data(|d| d.get_temp::<Kept>(id)) {
        if std::sync::Arc::ptr_eq(&seen, raw) && p == params {
            return kept;
        }
    }
    let derived = klipp_effects::node_analysis::derive(effect, raw).map(std::sync::Arc::new);
    ctx.data_mut(|d| d.insert_temp::<Kept>(id, (std::sync::Arc::clone(raw), params, derived.clone())));
    derived
}

/// The camera's horizontal path in `raw` (a stabilisation analysis), for
/// the trajectory plot: parsed once for the clip while `raw` holds.
fn trajectory_x(ctx: &egui::Context, clip: &Clip, raw: &std::sync::Arc<serde_json::Value>) -> Option<std::sync::Arc<Vec<f32>>> {
    type Kept = (std::sync::Arc<serde_json::Value>, Option<std::sync::Arc<Vec<f32>>>);
    let id = egui::Id::new(("trajectory_x", clip.id));
    if let Some((seen, kept)) = ctx.data(|d| d.get_temp::<Kept>(id)) {
        if std::sync::Arc::ptr_eq(&seen, raw) {
            return kept;
        }
    }
    let xs = serde_json::from_value::<klipp_effects::stabilization::StabilizationData>((**raw).clone()).ok().map(|d| std::sync::Arc::new(d.trajectory.iter().map(|p| p.tx as f32).collect::<Vec<f32>>()));
    ctx.data_mut(|d| d.insert_temp::<Kept>(id, (std::sync::Arc::clone(raw), xs.clone())));
    xs
}

/// Deflicker's analysis: measuring (with Cancel), done (with Re-analyse
/// and what it found), or not yet (starting by itself). Shared by the
/// effects tab and the generated stack's adapter (M3 step 6).
pub(crate) fn deflicker_status(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, state: &AppState, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let has_data = state.analysis_of(clip, klipp_analysis::assets::AssetKind::Deflicker).is_some();
    let is_analyzing = state.deflicker_in_progress.contains(&clip_id);

    if is_analyzing {
        let progress = state.deflicker_progress.get(&clip_id).copied().unwrap_or(0.0);
        ui.add(egui::ProgressBar::new(progress).text(format!("Measuring {:.0}%", progress * 100.0)));
        if ui.small_button("Cancel").clicked() {
            cmds.push(AppCommand::CancelDeflicker { clip_id });
        }
    } else if has_data {
        ui.horizontal(|ui| {
            ui.colored_label(theme.marker_success, "Analysis complete");
            if ui.small_button("Re-analyse").clicked() {
                cmds.push(AppCommand::AnalyzeDeflicker { clip_id });
            }
        });
        // What it found at the window the effect is set to, so "nothing
        // happened" is answerable.
        let derived = derived_analysis(ui.ctx(), clip, state, klipp_analysis::assets::AssetKind::Deflicker, |e| matches!(e, klipp_effects::BuiltinEffect::Deflicker { .. }));
        if let Some(klipp_effects::node_analysis::NodeAnalysis::Deflicker { gains, .. }) = derived.as_deref() {
            let worst = gains
                .iter()
                .map(|g| (g - 1.0).abs())
                .fold(0.0f32, f32::max);
            let (colour, text) = if worst < 0.01 {
                (theme.text_dim, "No flicker found — the shot is already steady".to_string())
            } else {
                (
                    theme.marker_success,
                    format!("Flicker found: up to {:.1}% per frame", worst * 100.0),
                )
            };
            ui.colored_label(colour, egui::RichText::new(text).size(11.0));
        }
    } else if state.analysis_unavailable.contains(&(clip_id, klipp_analysis::assets::AssetKind::Deflicker)) {
        // No key without the original: a proxy alone can't be analysed.
        ui.colored_label(theme.marker_warning, egui::RichText::new("Can't analyse: the media's original file isn't available, so the frame isn't deflickered. Relink it to analyse.").size(11.0));
    } else {
        // It starts by itself (`ensure_analyses`); until it's done the
        // frame is drawn as it is, and says so.
        ui.colored_label(theme.marker_warning, egui::RichText::new("Analysis needed: measuring the clip's light starts by itself. Until then the frame isn't deflickered.").size(11.0));
    }
}

/// Video Stabilization's analysis: analysing (with Cancel), done (with
/// Re-analyse, Compare, the shake it removed and the trajectory plot), or
/// not yet (starting by itself). Shared by the effects tab and the
/// generated stack's adapter (M3 step 6).
pub(crate) fn stabilization_status(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, state: &AppState, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    // Analysis status
    let raw = state.analysis_of(clip, klipp_analysis::assets::AssetKind::Stabilisation);
    let has_data = raw.is_some();
    let is_analyzing = state.stabilization_in_progress.contains(&clip_id);

    if is_analyzing {
        let progress = state.stabilization_progress.get(&clip_id).copied().unwrap_or(0.0);
        ui.add(egui::ProgressBar::new(progress).text(format!("Analyzing {:.0}%", progress * 100.0)));
        if ui.small_button("Cancel").clicked() {
            cmds.push(AppCommand::CancelStabilization { clip_id });
        }
    } else if has_data {
        ui.horizontal(|ui| {
            ui.colored_label(theme.marker_success, "Analysis complete");
            if ui.small_button("Re-analyze").clicked() {
                cmds.push(AppCommand::AnalyzeStabilization { clip_id });
            }
            let compare_on = state.stabilization_compare_active;
            let compare_label = if compare_on { "Exit Compare" } else { "Compare" };
            let compare_btn = ui.small_button(compare_label)
                .on_hover_text("Split-view raw vs stabilized. Drag the divider on the viewer to compare.");
            if compare_btn.clicked() {
                cmds.push(AppCommand::ToggleStabilizationCompare);
            }
        });
        // Quality metric: how much shake the smoothing actually removed, at
        // the smoothing the effect is set to.
        let derived = derived_analysis(ui.ctx(), clip, state, klipp_analysis::assets::AssetKind::Stabilisation, |e| matches!(e, klipp_effects::BuiltinEffect::VideoStabilization { .. }));
        if let Some(klipp_effects::node_analysis::NodeAnalysis::Stabilisation { smoothed, .. }) = derived.as_deref() {
            let pct = smoothed.jitter_reduction_pct();
            let (label_color, headline) = if smoothed.raw_jitter_px < 0.25 {
                // Source was already smooth — nothing to remove.
                (theme.text_dim, "Source already smooth — minimal shake detected".to_string())
            } else if pct >= 60.0 {
                (theme.marker_success, format!("Shake reduced by {pct:.0}%"))
            } else if pct >= 25.0 {
                (theme.marker_warning, format!("Shake reduced by {pct:.0}%"))
            } else {
                (theme.marker_error, format!("Shake reduced by {pct:.0}%"))
            };
            ui.colored_label(label_color, egui::RichText::new(headline).size(11.0));
            ui.label(
                egui::RichText::new(format!(
                    "Jitter: {:.1} → {:.1} px/frame",
                    smoothed.raw_jitter_px, smoothed.smoothed_jitter_px
                ))
                .size(10.0)
                .color(theme.text_dim),
            );

            // Trajectory graph: raw camera tx (jagged) vs smoothed tx (clean).
            // Visualizes what stabilization is actually doing.
            if let Some(raw) = raw.and_then(|r| trajectory_x(ui.ctx(), clip, r)) {
                let smooth: Vec<f32> = smoothed.smoothed.iter().map(|p| p.tx as f32).collect();
                if raw.len() >= 2 && smooth.len() == raw.len() {
                    let avail = ui.available_width();
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(avail, 64.0),
                        egui::Sense::hover(),
                    );
                    let p = ui.painter_at(rect);
                    // Background
                    p.rect_filled(rect, 2.0, theme.bg_deep);
                    p.rect_stroke(rect, 2.0, egui::Stroke::new(0.5_f32, theme.border));

                    // Y range covers both lines with small padding.
                    let mut lo = f32::INFINITY;
                    let mut hi = f32::NEG_INFINITY;
                    for &v in raw.iter().chain(smooth.iter()) {
                        if v < lo { lo = v; }
                        if v > hi { hi = v; }
                    }
                    let span = (hi - lo).max(1.0);
                    let pad_y = 4.0;
                    let inner = rect.shrink2(egui::vec2(4.0, pad_y));

                    let to_pt = |i: usize, v: f32| -> egui::Pos2 {
                        let x = inner.left() + (i as f32 / (raw.len() - 1) as f32) * inner.width();
                        let t = (v - lo) / span;
                        let y = inner.bottom() - t * inner.height();
                        egui::pos2(x, y)
                    };

                    // Zero/baseline reference line at the smoothed midpoint.
                    let mid = (lo + hi) * 0.5;
                    let mid_y = inner.bottom() - ((mid - lo) / span) * inner.height();
                    p.line_segment(
                        [egui::pos2(inner.left(), mid_y), egui::pos2(inner.right(), mid_y)],
                        egui::Stroke::new(0.5_f32, theme.border),
                    );

                    let raw_pts: Vec<egui::Pos2> = raw.iter().enumerate().map(|(i, &v)| to_pt(i, v)).collect();
                    let smooth_pts: Vec<egui::Pos2> = smooth.iter().enumerate().map(|(i, &v)| to_pt(i, v)).collect();
                    // Raw trajectory: dim, thin
                    p.add(egui::Shape::line(raw_pts, egui::Stroke::new(1.0_f32, theme.text_dim)));
                    // Smoothed trajectory: bright accent, thicker
                    p.add(egui::Shape::line(smooth_pts, egui::Stroke::new(1.6_f32, theme.accent)));

                    // Tiny legend
                    p.text(
                        egui::pos2(rect.left() + 6.0, rect.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        "raw",
                        egui::FontId::proportional(8.5),
                        theme.text_dim,
                    );
                    p.text(
                        egui::pos2(rect.left() + 28.0, rect.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        "smoothed",
                        egui::FontId::proportional(8.5),
                        theme.accent,
                    );
                }
            }
        }
    } else if state.analysis_unavailable.contains(&(clip_id, klipp_analysis::assets::AssetKind::Stabilisation)) {
        // No key without the original: a proxy alone can't be analysed.
        ui.colored_label(theme.marker_warning, egui::RichText::new("Can't analyse: the media's original file isn't available, so the frame isn't stabilised. Relink it to analyse.").size(11.0));
    } else {
        // It starts by itself (`ensure_analyses`); until it's done the
        // frame is drawn as it is, and says so.
        ui.colored_label(theme.marker_warning, egui::RichText::new("Analysis needed: measuring the clip's motion starts by itself. Until then the frame isn't stabilised.").size(11.0));
    }
}

/// A frei0r effect's body: the resolution warning or the missing-plug-in
/// one, then a widget per declared plug-in parameter, editing `params` in
/// place. Whether anything changed. Shared by the effects tab and the
/// generated stack's editor (M3 step 6).
pub(crate) fn frei0r_body(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, type_id: &str, params: &mut Vec<klipp_frei0r::Frei0rValue>) -> bool {
    let mut changed = false;
    // Resolution-warning banner: a plug-in that keeps frame history
    // can't be tiled above 2048 (other sizes frei0r can't take are
    // padded and cropped back). Pulled from the live registry so a
    // missing-plugin clip falls through quietly.
    let plugin_info = klipp_effects::frei0r_effect::lookup(type_id);
    let resolution = state.sequence().resolution;
    let project_w = resolution.width;
    let project_h = resolution.height;
    if let Some(ref info) = plugin_info {
        if !klipp_effects::frei0r_effect::supports_resolution(info, project_w, project_h) {
            let why = format!(
                "{} keeps frame history and can't be tiled: {}×{} is above 2048",
                info.name, project_w, project_h
            );
            ui.colored_label(theme.marker_warning, format!("⚠ {why}"));
            ui.add_space(4.0);
        }
    } else {
        ui.colored_label(
            theme.marker_warning,
            format!("⚠ Plugin not installed: {}", type_id),
        );
        ui.add_space(4.0);
    }

    // Per-param widgets — typed by the plugin's declared kind.
    // We read declared param info from the registry; if the
    // plugin disappeared, fall through to passthrough (no
    // widgets).
    let Some(plugin_info) = plugin_info else {
        return changed;
    };

    for (i, param_info) in plugin_info.params.iter().enumerate() {
        // Lazily extend params Vec if a plugin grew between
        // saves (defensive — plugins shouldn't change shape
        // across versions, but covers the case cleanly).
        while params.len() <= i {
            params.push(klipp_frei0r::Frei0rValue::default_for(
                plugin_info.params[params.len()].kind,
            ));
            changed = true;
        }
        let value = &mut params[i];
        ui.horizontal(|ui| {
            ui.label(&param_info.name);
            match (param_info.kind, value) {
                (klipp_frei0r::ParamKind::Bool, klipp_frei0r::Frei0rValue::Bool(b)) => {
                    if ui.checkbox(b, "").changed() {
                        changed = true;
                    }
                }
                (
                    klipp_frei0r::ParamKind::Double,
                    klipp_frei0r::Frei0rValue::Double(d),
                ) => {
                    // Spec: f0r_param_double range is [0, 1].
                    if ui.add(egui::Slider::new(d, 0.0..=1.0)).changed() {
                        changed = true;
                    }
                }
                (
                    klipp_frei0r::ParamKind::Color,
                    klipp_frei0r::Frei0rValue::Color(rgb),
                ) => {
                    let mut color = [
                        (rgb[0].clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb[1].clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb[2].clamp(0.0, 1.0) * 255.0) as u8,
                    ];
                    if ui.color_edit_button_srgb(&mut color).changed() {
                        rgb[0] = color[0] as f32 / 255.0;
                        rgb[1] = color[1] as f32 / 255.0;
                        rgb[2] = color[2] as f32 / 255.0;
                        changed = true;
                    }
                }
                (
                    klipp_frei0r::ParamKind::Position,
                    klipp_frei0r::Frei0rValue::Position(xy),
                ) => {
                    ui.label("X");
                    if ui
                        .add(egui::Slider::new(&mut xy[0], 0.0..=1.0))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.label("Y");
                    if ui
                        .add(egui::Slider::new(&mut xy[1], 0.0..=1.0))
                        .changed()
                    {
                        changed = true;
                    }
                }
                (
                    klipp_frei0r::ParamKind::String,
                    klipp_frei0r::Frei0rValue::String(s),
                ) => {
                    if ui.text_edit_singleline(s).changed() {
                        changed = true;
                    }
                }
                _ => {
                    // Type/value mismatch — render a clear
                    // disabled hint instead of silently
                    // ignoring the row.
                    ui.colored_label(theme.text_dim, "(unsupported param type)");
                }
            }
        });
    }
    changed
}

/// A clip's plug-in (OFX) effect cards, with their reorder and remove.
/// Shared by the effects tab and the generated stack, which draws them
/// unchanged under its own rows (M4 owns them; M3 step 12).
pub(crate) fn show_ofx_cards(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>) {
    let clip_id = clip.id;
    let pad = 14.0;
    let card_bg = theme.bg_raised;
    let card_header_bg = egui::Color32::from_rgb(
        (card_bg.r() as u16).saturating_sub(8) as u8,
        (card_bg.g() as u16).saturating_sub(8) as u8,
        (card_bg.b() as u16).saturating_sub(8) as u8,
    );
    let card_rounding = 6.0;
    let header_h = 32.0;
    let badge_gray = theme.text_dim;
    // ── OFX effects ──
    let mut ofx_remove = None;
    let mut ofx_move_to: Option<(usize, usize)> = None;

    for (i, effect) in clip.ofx_effects().iter().enumerate() {
        let display_name = effect.plugin_name
            .rsplit_once('.')
            .map(|(_, name)| name)
            .unwrap_or(&effect.plugin_name);

        let header_id = ui.make_persistent_id(format!("ofx_{clip_id:?}_{i}"));
        let mut col_state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(), header_id, true,
        );
        let is_open = col_state.is_open();
        let card_w = ui.available_width();

        // ── Card header ──
        let (header_rect, header_resp) = ui.allocate_exact_size(
            egui::vec2(card_w, header_h), egui::Sense::click_and_drag(),
        );

        let header_rounding = if is_open {
            egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 }
        } else {
            egui::Rounding::same(card_rounding)
        };
        ui.painter().rect_filled(header_rect, header_rounding, card_header_bg);
        ui.painter().rect_stroke(header_rect.shrink(0.25), header_rounding, egui::Stroke::new(0.5_f32, theme.border));

        // Collapse caret (SVG icon)
        let caret_x = header_rect.min.x + pad;
        let caret_icon = if is_open { Icon::CaretDown } else { Icon::CaretRight };
        caret_icon.image(10.0).tint(theme.text_dim).paint_at(ui, egui::Rect::from_center_size(
            egui::pos2(caret_x + 5.0, header_rect.center().y), egui::vec2(10.0, 10.0),
        ));

        // Effect name
        let name_x = caret_x + 18.0;
        ui.painter().text(
            egui::pos2(name_x, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            display_name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );

        // "OFX" badge — outlined pill, gray
        let badge_text = "OFX";
        let badge_font = egui::FontId::proportional(10.0);
        let badge_galley = ui.painter().layout_no_wrap(badge_text.to_string(), badge_font.clone(), badge_gray);
        let badge_w = badge_galley.size().x + 10.0;
        let badge_h = 16.0;
        let name_galley = ui.painter().layout_no_wrap(display_name.to_string(), egui::FontId::proportional(13.0), theme.text_primary);
        let badge_x = name_x + name_galley.size().x + 8.0;
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, header_rect.center().y - badge_h / 2.0),
            egui::vec2(badge_w, badge_h),
        );
        ui.painter().rect_stroke(badge_rect, badge_h / 2.0, egui::Stroke::new(0.8_f32, badge_gray));
        ui.painter().text(
            badge_rect.center(), egui::Align2::CENTER_CENTER,
            badge_text, badge_font, badge_gray,
        );

        // Right side: × remove button
        let remove_x = header_rect.max.x - pad - 6.0;
        let remove_center = egui::pos2(remove_x, header_rect.center().y);
        let remove_rect = egui::Rect::from_center_size(remove_center, egui::vec2(18.0, 18.0));
        let remove_resp = ui.interact(remove_rect, ui.id().with(("ofx_rm", i)), egui::Sense::click());
        let remove_color = if remove_resp.hovered() { theme.text_primary } else { theme.text_dim };
        ui.painter().text(
            remove_center, egui::Align2::CENTER_CENTER,
            "\u{00d7}", egui::FontId::proportional(13.0), remove_color,
        );
        let remove_clicked = remove_resp.on_hover_text("Remove effect").clicked();
        if remove_clicked {
            ofx_remove = Some(i);
        }

        // Enable/disable eye-icon toggle — same style as the builtin path.
        let enabled = effect.enabled;
        let toggle_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 26.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let toggle_resp = ui
            .interact(toggle_rect, ui.id().with(("ofx_tog", i)), egui::Sense::click())
            .on_hover_text(if enabled { "Disable effect" } else { "Enable effect" });
        let eye_icon = if enabled { Icon::Eye } else { Icon::EyeSlash };
        let tint = if enabled {
            theme.text_primary
        } else {
            theme.text_dim
        };
        eye_icon.image(14.0).tint(tint).paint_at(ui, toggle_rect);
        let toggle_clicked = toggle_resp.clicked();
        if toggle_clicked {
            cmds.push(AppCommand::SetEffectEnabled {
                clip_id,
                effect_index: i,
                enabled: !enabled,
            });
        }

        // Split-compare (A/B) toggle — mirrors the builtin path.
        // The compared effect is a node of the clip's picture (M3 step 4),
        // so a reorder or a removal before it leaves the compare on it.
        let compare_path = clip.effect_node_at(true, i).map(|n| klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip_id.0 }, [n]));
        let compare_active = compare_path.is_some()
            && matches!(&state.split_compare_node, Some((cid, path)) if *cid == clip_id && Some(path) == compare_path.as_ref());
        let compare_rect = egui::Rect::from_min_size(
            egui::pos2(remove_x - 48.0, header_rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let compare_resp = ui
            .interact(compare_rect, ui.id().with(("ofx_cmp", i)), egui::Sense::click())
            .on_hover_text(if compare_active { "Stop A/B compare" } else { "A/B compare: bypass vs applied" });
        let compare_tint = if compare_active {
            theme.accent_alt
        } else if compare_resp.hovered() {
            theme.text_primary
        } else {
            theme.text_dim
        };
        ui.painter().text(
            compare_rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{25D0}",
            egui::FontId::proportional(13.0),
            compare_tint,
        );
        let compare_clicked = compare_resp.clicked();
        if compare_clicked {
            if compare_active {
                cmds.push(AppCommand::SetSplitCompareNode { clip_id: None, path: None });
            } else if let Some(path) = compare_path {
                cmds.push(AppCommand::SetSplitCompareNode { clip_id: Some(clip_id), path: Some(path) });
            }
        }

        // Click header to toggle collapse
        if header_resp.clicked() && !remove_clicked && !toggle_clicked && !compare_clicked {
            col_state.toggle(ui);
        }

        col_state.store(ui.ctx());

        // ── Card body ──
        if is_open {
            let body_frame = egui::Frame::none()
                .fill(card_bg)
                .stroke(egui::Stroke::new(0.5_f32, theme.border))
                .rounding(egui::Rounding { nw: 0.0, ne: 0.0, sw: card_rounding, se: card_rounding })
                .inner_margin(egui::Margin { left: pad, right: pad, top: 8.0, bottom: 8.0 });
            body_frame.show(ui, |ui: &mut egui::Ui| {
                // When disabled, dim and lock the params — mirrors the
                // builtin path for consistency.
                ui.add_enabled_ui(enabled, |ui| {
                // CPU-only notice for float-only plugins (CImg, keyers, etc.)
                let is_float_plugin = effect.plugin_name.contains("cimg")
                    || effect.plugin_name.contains("CImg")
                    || effect.plugin_name.contains("PIK")
                    || effect.plugin_name.contains("Keyer")
                    || effect.plugin_name.contains("IDistort")
                    || effect.plugin_name.contains("STMap")
                    || effect.plugin_name.contains("VectorToColor")
                    || effect.plugin_name.contains("ContactSheet");
                if is_float_plugin {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("CPU only").size(10.0).color(theme.marker_warning).strong());
                        ui.label(egui::RichText::new("\u{2014} adjust while paused, apply on export").size(10.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Render-only notice for a plug-in that declared it reads
                // frames at other times, as the render path reads it.
                let is_temporal = state.available_plugins.iter().any(|p| p.name == effect.plugin_name && p.temporal);
                if is_temporal {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Render only").size(10.0).color(theme.marker_error).strong());
                        ui.label(egui::RichText::new("\u{2014} not real-time, applied on export").size(10.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Multi-view/stereo notice
                let is_stereo = effect.plugin_name.contains("anaglyph")
                    || effect.plugin_name.contains("Anaglyph")
                    || effect.plugin_name.contains("joinViews")
                    || effect.plugin_name.contains("JoinView")
                    || effect.plugin_name.contains("mixViews")
                    || effect.plugin_name.contains("MixView")
                    || effect.plugin_name.contains("oneView")
                    || effect.plugin_name.contains("OneView")
                    || effect.plugin_name.contains("sideBySide")
                    || effect.plugin_name.contains("SideBySide");
                if is_stereo {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Requires multi-view").size(10.0).color(theme.marker_error).strong());
                        ui.label(egui::RichText::new("\u{2014} stereo 3D not supported yet").size(10.0).color(theme.text_dim));
                    });
                    ui.add_space(2.0);
                }

                // Show parameter controls if plugin has declared params
                if let Some(plugin_info) = state.available_plugins.iter().find(|p| p.name == effect.plugin_name) {
                    for desc in &plugin_info.parameters {
                        if desc.secret {
                            continue;
                        }
                        match desc.param_type {
                            klipp_plugin::ParamType::Group | klipp_plugin::ParamType::Page => continue,
                            _ => {}
                        }
                        show_ofx_param(ui, theme, clip_id, i, effect, desc, cmds);
                    }
                    if plugin_info.parameters.is_empty() {
                        ui.label(
                            egui::RichText::new("No parameters")
                                .size(10.0)
                                .color(theme.text_dim),
                        );
                    }
                }
                }); // end ui.add_enabled_ui
            });
        }

        // Drag visual feedback + reorder
        if header_resp.dragged_by(egui::PointerButton::Primary) {
            ui.painter().rect_stroke(
                header_rect,
                if is_open { egui::Rounding { nw: card_rounding, ne: card_rounding, sw: 0.0, se: 0.0 } } else { egui::Rounding::same(card_rounding) },
                egui::Stroke::new(2.0_f32, theme.accent),
            );
            let drag_dy = header_resp.drag_delta().y;
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("ofx_drag_accum").with(i);
                let accum: &mut f32 = d.get_temp_mut_or_default(key);
                *accum += drag_dy;
                *accum
            });
            if total_drag < -15.0 && i > 0 {
                ui.painter().hline(header_rect.x_range(), header_rect.min.y - 2.0, egui::Stroke::new(2.0_f32, theme.accent));
            } else if total_drag > 15.0 && i + 1 < clip.ofx_effects().len() {
                let line_y = if is_open { ui.cursor().min.y + 2.0 } else { header_rect.max.y + 2.0 };
                ui.painter().hline(header_rect.x_range(), line_y, egui::Stroke::new(2.0_f32, theme.accent));
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if header_resp.drag_stopped() {
            let total_drag: f32 = ui.ctx().data_mut(|d| {
                let key = egui::Id::new("ofx_drag_accum").with(i);
                let accum = d.get_temp::<f32>(key).unwrap_or(0.0);
                d.remove_temp::<f32>(key);
                accum
            });
            let slot_h = 40.0_f32;
            let steps = (total_drag.abs() / slot_h).floor() as usize;
            if steps > 0 {
                let max_idx = clip.ofx_effects().len() - 1;
                let target = if total_drag < 0.0 {
                    i.saturating_sub(steps)
                } else {
                    (i + steps).min(max_idx)
                };
                if target != i {
                    ofx_move_to = Some((i, target));
                }
            }
        }
        if header_resp.hovered() && !header_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        ui.add_space(4.0);
    }

    if let Some(idx) = ofx_remove {
        cmds.push(AppCommand::RemoveEffect { clip_id, effect_index: idx });
    }
    if let Some((from, to)) = ofx_move_to {
        cmds.push(AppCommand::ReorderEffect { clip_id, from_index: from, to_index: to });
    }
}
