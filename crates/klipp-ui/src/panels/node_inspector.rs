use crate::state::ColorTab;
use crate::theme::KlippTheme;
use crate::widgets::color_wheel::{self, ColorWheelState};
use crate::widgets::curves_editor::{self, CurveChannel};
use klipp_core::{AppCommand, ClipId};
use klipp_nodes::{NodeGraph, NodeId, NodeKind, NodeParams};

// ═══════════════════════════════════════════════════════════════════════════════
// NodeInspectorPanel — per-node parameter editor
// ═══════════════════════════════════════════════════════════════════════════════

pub struct NodeInspectorPanel {
    // ── Inspector widgets ──
    pub lift_wheel: ColorWheelState,
    pub gamma_wheel: ColorWheelState,
    pub gain_wheel: ColorWheelState,
    pub offset_wheel: ColorWheelState,
    pub active_curve_channel: CurveChannel,
}

impl Default for NodeInspectorPanel {
    fn default() -> Self {
        Self {
            lift_wheel: ColorWheelState::default(),
            gamma_wheel: ColorWheelState::default(),
            gain_wheel: ColorWheelState::default(),
            offset_wheel: ColorWheelState::default(),
            active_curve_channel: CurveChannel::Master,
        }
    }
}

impl NodeInspectorPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &Option<NodeId>,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        egui::ScrollArea::vertical()
            .id_salt("node_inspector_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                if let Some(sel_id) = *selected_node {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == sel_id) {
                        let node_id = node.id;
                        let node_kind = node.kind.clone();
                        let node_enabled = &mut node.enabled;
                        let params = &mut node.params;
                        let keyframes = &mut node.keyframes;
                        self.show_inspector(
                            ui, node_id, &node_kind, node_enabled,
                            params, keyframes, clip_id, &mut cmds, theme,
                        );
                    }
                } else {
                    ui.label(egui::RichText::new("Select a node").color(theme.text_dim));
                }
            });

        cmds
    }

    /// Simple tabbed view for new users — hides node complexity.
    /// Tabs: Adjustments / Wheels / Curves / HueSat.
    /// Each tab finds the corresponding node by kind and shows its params.
    pub fn show_simple(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Padding
        ui.add_space(4.0);

        // Tab bar
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            for (tab, label) in [
                (ColorTab::Adjustments, "Adjust"),
                (ColorTab::Wheels, "Wheels"),
                (ColorTab::Curves, "Curves"),
                (ColorTab::HueSat, "HueSat"),
            ] {
                let active = *color_tab == tab;
                let text = egui::RichText::new(label).size(11.0);
                let text = if active { text.strong().color(theme.text_accent) } else { text.color(theme.text_secondary) };
                if ui.selectable_label(active, text).clicked() {
                    *color_tab = tab;
                }
            }
        });
        ui.separator();
        ui.add_space(4.0);

        // Map tab to node kinds and auto-select the right node
        let target_kinds: Vec<NodeKind> = match color_tab {
            ColorTab::Adjustments => vec![
                NodeKind::Exposure, NodeKind::Contrast, NodeKind::Saturation,
                NodeKind::ToneControls, NodeKind::ColorBalance,
            ],
            ColorTab::Wheels => vec![NodeKind::ColorWheels, NodeKind::WheelSatBri],
            ColorTab::Curves => vec![NodeKind::RGBCurves],
            ColorTab::HueSat => vec![NodeKind::HueSatCurves],
        };

        // Render params for each matching node kind in this tab
        egui::ScrollArea::vertical()
            .id_salt("simple_inspector_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                for target_kind in &target_kinds {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == *target_kind && n.enabled) {
                        let node_id = node.id;
                        let node_kind = node.kind.clone();
                        *selected_node = Some(node_id);
                        let node_enabled = &mut node.enabled;
                        let params = &mut node.params;
                        let keyframes = &mut node.keyframes;
                        self.show_inspector(
                            ui, node_id, &node_kind, node_enabled,
                            params, keyframes, clip_id, &mut cmds, theme,
                        );
                        ui.add_space(8.0);
                    }
                }
            });

        cmds
    }

    // ── Inspector ───────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn show_inspector(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        kind: &NodeKind,
        enabled: &mut bool,
        params: &mut NodeParams,
        _keyframes: &mut klipp_nodes::NodeKeyframeTracks,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(kind.display_name())
                .size(12.0).color(theme.text_accent).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                if ui.checkbox(enabled, "").changed() {
                    cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
                }
            });
        });
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(4.0);

        match kind {
            NodeKind::Exposure => {
                let mut val = params.exposure.unwrap_or(0.0);
                if grade_slider(ui, "Exposure", &mut val, -3.0..=3.0, "stops") {
                    params.exposure = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "exposure".into(), value: val });
                }
            }
            NodeKind::Contrast => {
                let mut val = params.contrast.unwrap_or(0.0);
                if grade_slider(ui, "Contrast", &mut val, -1.0..=1.0, "") {
                    params.contrast = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "contrast".into(), value: val });
                }
            }
            NodeKind::Saturation => {
                let mut val = params.saturation.unwrap_or(1.0);
                if grade_slider(ui, "Saturation", &mut val, 0.0..=2.0, "") {
                    params.saturation = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: val });
                }
            }
            NodeKind::ToneControls => {
                for (label, field, param_name) in [
                    ("Highlights", params.highlights.unwrap_or(0.0), "highlights"),
                    ("Shadows", params.shadows.unwrap_or(0.0), "shadows"),
                    ("Whites", params.whites.unwrap_or(0.0), "whites"),
                    ("Blacks", params.blacks.unwrap_or(0.0), "blacks"),
                ] {
                    let mut val = field;
                    if grade_slider(ui, label, &mut val, -1.0..=1.0, "") {
                        params.apply_override(param_name, val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: param_name.into(), value: val });
                    }
                }
            }
            NodeKind::ColorWheels => {
                // Determine wheel diameter from available width.
                // 4-across if >520px, 2x2 if >280px, vertical stack otherwise.
                let avail_w = ui.available_width();
                let two_col = avail_w >= 250.0;
                let wheel_d = if two_col {
                    ((avail_w - 20.0) / 2.0).clamp(50.0, 110.0)
                } else {
                    (avail_w - 10.0).clamp(60.0, 140.0)
                };

                // Helper closure for each wheel
                let mut show_wheel = |ui: &mut egui::Ui, label: &str,
                    state: &mut ColorWheelState,
                    x: &mut Option<f32>, y: &mut Option<f32>, m: &mut Option<f32>,
                    prefix: &str|
                {
                    let mut offset = [x.unwrap_or(0.0), y.unwrap_or(0.0)];
                    let mut master = m.unwrap_or(0.0);
                    let resp = color_wheel::color_wheel_sized(
                        ui, state, theme, label, &mut offset, &mut master, wheel_d,
                    );
                    if resp.changed {
                        *x = Some(offset[0]); *y = Some(offset[1]); *m = Some(master);
                        push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                    }
                };

                if two_col {
                    // 2x2 grid using horizontal pairs
                    ui.horizontal_wrapped(|ui| {
                        ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                            show_wheel(ui, "Lift", &mut self.lift_wheel,
                                &mut params.lift_x, &mut params.lift_y, &mut params.lift_master, "lift");
                        });
                        ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                            show_wheel(ui, "Gamma", &mut self.gamma_wheel,
                                &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master, "gamma");
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                            show_wheel(ui, "Gain", &mut self.gain_wheel,
                                &mut params.gain_x, &mut params.gain_y, &mut params.gain_master, "gain");
                        });
                        ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                            show_wheel(ui, "Offset", &mut self.offset_wheel,
                                &mut params.offset_x, &mut params.offset_y, &mut params.offset_master, "offset");
                        });
                    });
                } else {
                    // Vertical stack for narrow panels
                    show_wheel(ui, "Lift", &mut self.lift_wheel,
                        &mut params.lift_x, &mut params.lift_y, &mut params.lift_master, "lift");
                    show_wheel(ui, "Gamma", &mut self.gamma_wheel,
                        &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master, "gamma");
                    show_wheel(ui, "Gain", &mut self.gain_wheel,
                        &mut params.gain_x, &mut params.gain_y, &mut params.gain_master, "gain");
                    show_wheel(ui, "Offset", &mut self.offset_wheel,
                        &mut params.offset_x, &mut params.offset_y, &mut params.offset_master, "offset");
                }
            }
            NodeKind::WheelSatBri => {
                for (label, sat_name, bri_name, sat_val, bri_val) in [
                    ("Lift", "lift_sat", "lift_bri", params.lift_sat.unwrap_or(0.0), params.lift_bri.unwrap_or(0.0)),
                    ("Gamma", "gamma_sat", "gamma_bri", params.gamma_sat.unwrap_or(0.0), params.gamma_bri.unwrap_or(0.0)),
                    ("Gain", "gain_sat", "gain_bri", params.gain_sat.unwrap_or(0.0), params.gain_bri.unwrap_or(0.0)),
                    ("Offset", "offset_sat", "offset_bri", params.offset_sat.unwrap_or(0.0), params.offset_bri.unwrap_or(0.0)),
                ] {
                    ui.label(egui::RichText::new(label).size(10.0).color(theme.text_dim));
                    let mut s = sat_val;
                    if grade_slider(ui, "Sat", &mut s, -1.0..=1.0, "") {
                        params.apply_override(sat_name, s);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: s });
                    }
                    let mut b = bri_val;
                    if grade_slider(ui, "Bri", &mut b, -1.0..=1.0, "") {
                        params.apply_override(bri_name, b);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.into(), value: b });
                    }
                }
            }
            NodeKind::RGBCurves => {
                ui.horizontal(|ui| {
                    for ch in [CurveChannel::Master, CurveChannel::Red, CurveChannel::Green, CurveChannel::Blue] {
                        let label = match ch { CurveChannel::Master => "M", CurveChannel::Red => "R", CurveChannel::Green => "G", CurveChannel::Blue => "B" };
                        if ui.selectable_label(self.active_curve_channel == ch, label).clicked() {
                            self.active_curve_channel = ch;
                        }
                    }
                });
                let curves = params.curves.get_or_insert_with(Default::default);
                let points = match self.active_curve_channel {
                    CurveChannel::Master => &mut curves.master, CurveChannel::Red => &mut curves.red,
                    CurveChannel::Green => &mut curves.green, CurveChannel::Blue => &mut curves.blue,
                };
                let curve_h = ui.available_width().clamp(100.0, 200.0);
                let resp = curves_editor::curves_editor_sized(ui, theme, self.active_curve_channel, points, curve_h, None);
                if resp.changed {
                    cmds.push(AppCommand::SetNodeCurves { clip_id, node_id, curves: curves.clone() });
                }
                let mut preserve = params.curves_preserve_luma.unwrap_or(false);
                if ui.checkbox(&mut preserve, "Preserve Luma").changed() { params.curves_preserve_luma = Some(preserve); }
                let mut mix = params.curves_mix.unwrap_or(1.0);
                if grade_slider(ui, "Mix", &mut mix, 0.0..=1.0, "") {
                    params.curves_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_mix".into(), value: mix });
                }
            }
            NodeKind::HueSatCurves => {
                let huesat = params.huesat.get_or_insert_with(Default::default);
                for (i, label) in ["HvH", "HvS", "HvL", "LvS", "SvS", "OvS"].iter().enumerate() {
                    let points = match i {
                        0 => &mut huesat.hue_vs_hue, 1 => &mut huesat.hue_vs_sat,
                        2 => &mut huesat.hue_vs_lum, 3 => &mut huesat.lum_vs_sat,
                        4 => &mut huesat.sat_vs_sat, _ => &mut huesat.orange_vs_sat,
                    };
                    ui.label(egui::RichText::new(*label).size(10.0).color(theme.text_dim));
                    let hs_h = ui.available_width().clamp(50.0, 100.0);
                    let resp = curves_editor::curves_editor_sized(ui, theme, CurveChannel::Master, points, hs_h, None);
                    if resp.changed {
                        cmds.push(AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat: huesat.clone() });
                    }
                }
                let mut mix = params.huesat_mix.unwrap_or(1.0);
                if grade_slider(ui, "Mix", &mut mix, 0.0..=1.0, "") {
                    params.huesat_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "huesat_mix".into(), value: mix });
                }
            }
            NodeKind::ColorBalance => {
                for (label, pname, val, range) in [
                    ("Temperature", "temperature", params.temperature.unwrap_or(0.0), -50.0..=50.0),
                    ("Tint", "tint", params.tint.unwrap_or(0.0), -50.0..=50.0),
                    ("Hue", "hue", params.hue.unwrap_or(0.0), 0.0..=360.0),
                    ("Global Mix", "global_mix", params.global_mix.unwrap_or(1.0), 0.0..=1.0),
                ] {
                    let mut v = val;
                    if grade_slider(ui, label, &mut v, range, "") {
                        params.apply_override(pname, v);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: pname.into(), value: v });
                    }
                }
            }
            NodeKind::InputTransform => {
                ui.label("Input Device Transform");
                ui.label(egui::RichText::new("Set via Inspector > Log Format").size(10.0).color(theme.text_dim));
            }
            NodeKind::Qualifier => {
                let qual = params.qualifier.get_or_insert_with(Default::default);
                ui.checkbox(&mut qual.enabled, "Enabled");
                if qual.enabled {
                    let mut hc = qual.hue_center;
                    if grade_slider(ui, "Hue Center", &mut hc, 0.0..=360.0, "deg") { qual.hue_center = hc; }
                    let mut hw = qual.hue_width;
                    if grade_slider(ui, "Hue Width", &mut hw, 0.0..=180.0, "deg") { qual.hue_width = hw; }
                    let mut hs = qual.hue_soft;
                    if grade_slider(ui, "Hue Soft", &mut hs, 0.0..=90.0, "deg") { qual.hue_soft = hs; }
                    ui.add_space(4.0);
                    let mut sl = qual.sat_low;
                    if grade_slider(ui, "Sat Low", &mut sl, 0.0..=1.0, "") { qual.sat_low = sl; }
                    let mut sh = qual.sat_high;
                    if grade_slider(ui, "Sat High", &mut sh, 0.0..=1.0, "") { qual.sat_high = sh; }
                    let mut ss = qual.sat_soft;
                    if grade_slider(ui, "Sat Soft", &mut ss, 0.0..=1.0, "") { qual.sat_soft = ss; }
                    ui.add_space(4.0);
                    let mut ll = qual.lum_low;
                    if grade_slider(ui, "Lum Low", &mut ll, 0.0..=1.0, "") { qual.lum_low = ll; }
                    let mut lh = qual.lum_high;
                    if grade_slider(ui, "Lum High", &mut lh, 0.0..=1.0, "") { qual.lum_high = lh; }
                    let mut ls = qual.lum_soft;
                    if grade_slider(ui, "Lum Soft", &mut ls, 0.0..=1.0, "") { qual.lum_soft = ls; }
                    ui.checkbox(&mut qual.invert, "Invert");
                    cmds.push(AppCommand::SetNodeQualifier { clip_id, node_id, qualifier: qual.clone() });
                }
            }
            NodeKind::Mask => {
                ui.label(egui::RichText::new("Mask parameters — Session 6").size(10.0).color(theme.text_dim));
            }
            NodeKind::FilmGrain => {
                let grain = params.grain.get_or_insert_with(Default::default);
                ui.checkbox(&mut grain.enabled, "Enabled");
                if grain.enabled {
                    let mut a = grain.amount;
                    if grade_slider(ui, "Amount", &mut a, 0.0..=1.0, "") { grain.amount = a; }
                    let mut s = grain.size;
                    if grade_slider(ui, "Size", &mut s, 0.5..=3.0, "") { grain.size = s; }
                    let mut sh = grain.shadows;
                    if grade_slider(ui, "Shadows", &mut sh, 0.0..=1.0, "") { grain.shadows = sh; }
                    let mut hi = grain.highlights;
                    if grade_slider(ui, "Highlights", &mut hi, 0.0..=1.0, "") { grain.highlights = hi; }
                    ui.checkbox(&mut grain.monochrome, "Monochrome");
                }
            }
            NodeKind::Lut3D => {
                if let Some(path) = &params.lut_path {
                    ui.label(egui::RichText::new(path.display().to_string()).size(10.0));
                } else {
                    ui.label(egui::RichText::new("No LUT loaded").size(10.0).color(theme.text_dim));
                }
                let mut opacity = params.lut_opacity.unwrap_or(1.0);
                if grade_slider(ui, "Opacity", &mut opacity, 0.0..=1.0, "") {
                    params.lut_opacity = Some(opacity);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "lut_opacity".into(), value: opacity });
                }
            }
            NodeKind::Blend | NodeKind::ApplyMatte => {
                let mut mix = params.mix.unwrap_or(1.0);
                if grade_slider(ui, "Mix", &mut mix, 0.0..=1.0, "") {
                    params.mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "mix".into(), value: mix });
                }
            }
            NodeKind::CombineMattes => { ui.label("Multiplies two matte inputs"); }
            NodeKind::Source | NodeKind::Output | NodeKind::Layer
            | NodeKind::ColorMatch | NodeKind::Unknown => {
                ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn grade_slider(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    let pad = 12.0;
    let w = (ui.available_width() - pad * 2.0).max(60.0);

    // Drag speed: how many units per pixel of mouse movement
    // Smaller = more precision. Based on range span.
    let range_span = (range.end() - range.start()).abs();
    let speed = range_span / 800.0; // 800px of drag to cover full range — very precise

    let mut changed = false;

    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.set_max_width(w);
        ui.label(egui::RichText::new(label).size(10.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Draggable value readout — click and drag to adjust precisely
            let resp = ui.add(
                egui::DragValue::new(value)
                    .range(range.clone())
                    .speed(speed)
                    .custom_formatter(|v, _| {
                        if suffix.is_empty() { format!("{v:.2}") } else { format!("{v:.2} {suffix}") }
                    }),
            );
            if resp.changed() { changed = true; }
        });
    });

    // Visual slider bar
    ui.horizontal(|ui| {
        ui.add_space(pad);
        let resp = ui.add_sized(
            [w, 12.0],
            egui::Slider::new(value, range)
                .show_value(false)
                .clamping(egui::SliderClamping::Always),
        );
        if resp.changed() { changed = true; }
    });

    changed
}

fn push_wheel_cmds(cmds: &mut Vec<AppCommand>, clip_id: ClipId, node_id: NodeId, prefix: &str, offset: [f32; 2], master: f32) {
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_x", prefix), value: offset[0] });
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_y", prefix), value: offset[1] });
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_master", prefix), value: master });
}
