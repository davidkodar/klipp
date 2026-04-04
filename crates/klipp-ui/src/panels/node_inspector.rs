use crate::icons::Icon;
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
    // Log wheel states (HDR mode)
    pub log_low_wheel: ColorWheelState,
    pub log_mid_wheel: ColorWheelState,
    pub log_high_wheel: ColorWheelState,
    pub active_curve_channel: CurveChannel,
}

impl Default for NodeInspectorPanel {
    fn default() -> Self {
        Self {
            lift_wheel: ColorWheelState::default(),
            gamma_wheel: ColorWheelState::default(),
            gain_wheel: ColorWheelState::default(),
            offset_wheel: ColorWheelState::default(),
            log_low_wheel: ColorWheelState::default(),
            log_mid_wheel: ColorWheelState::default(),
            log_high_wheel: ColorWheelState::default(),
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
        clip_time_secs: f64,
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
                            params, keyframes, clip_id, &mut cmds, theme, clip_time_secs,
                        );
                    }
                } else {
                    ui.label(egui::RichText::new("Select a node").color(theme.text_dim));
                }
            });

        cmds
    }

    /// Simple tabbed view matching the HTML mockup design.
    pub fn show_simple(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        clip_time_secs: f64,
    ) -> Vec<AppCommand> {
        self.show_simple_layout(ui, graph, clip_id, selected_node, color_tab, theme, clip_time_secs, false)
    }

    /// Show color inspector with horizontal layout (for bottom panel — wide, short).
    pub fn show_simple_horizontal(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        clip_time_secs: f64,
    ) -> Vec<AppCommand> {
        self.show_simple_layout(ui, graph, clip_id, selected_node, color_tab, theme, clip_time_secs, true)
    }

    fn show_simple_layout(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        clip_time_secs: f64,
        wide_layout: bool,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // ── Tab row (equal-width, amber underline on active) ──
        let tab_h = 28.0;
        let avail_w = ui.available_width();
        let tab_w = avail_w / 5.0;

        let (tab_rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, tab_h), egui::Sense::hover());

        // Draw tabs using painter clone to avoid borrow conflict
        {
            let painter = ui.painter().clone();
            painter.line_segment(
                [egui::pos2(tab_rect.min.x, tab_rect.max.y), egui::pos2(tab_rect.max.x, tab_rect.max.y)],
                egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 18)),
            );

            for (i, (tab, label)) in [
                (ColorTab::Adjustments, "Color"),
                (ColorTab::Wheels, "Wheels"),
                (ColorTab::Curves, "Curves"),
                (ColorTab::HueSat, "Hue/Sat"),
                (ColorTab::Warper, "Warper"),
            ].iter().enumerate() {
                let active = *color_tab == *tab;
                let x = tab_rect.min.x + i as f32 * tab_w;
                let rect = egui::Rect::from_min_size(egui::pos2(x, tab_rect.min.y), egui::vec2(tab_w, tab_h));

                let color = if active { theme.text_accent } else { egui::Color32::from_rgba_premultiplied(255, 255, 255, 107) };
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, label,
                    egui::FontId::proportional(11.0), color);

                if active {
                    painter.line_segment(
                        [egui::pos2(rect.min.x + 4.0, rect.max.y), egui::pos2(rect.max.x - 4.0, rect.max.y)],
                        egui::Stroke::new(1.5, theme.text_accent),
                    );
                }
            }
        }

        // Tab click interaction (separate pass)
        for (i, (tab, _label)) in [
            (ColorTab::Adjustments, "Color"),
            (ColorTab::Wheels, "Wheels"),
            (ColorTab::Curves, "Curves"),
            (ColorTab::HueSat, "Hue/Sat"),
            (ColorTab::Warper, "Warper"),
        ].iter().enumerate() {
            let x = tab_rect.min.x + i as f32 * tab_w;
            let rect = egui::Rect::from_min_size(egui::pos2(x, tab_rect.min.y), egui::vec2(tab_w, tab_h));
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            if resp.clicked() { *color_tab = *tab; }
        }

        // ── Tab content ──
        // In wide layout, non-Wheels tabs need width constraints (sliders shouldn't be 1000px wide)
        let needs_width_cap = wide_layout && *color_tab != ColorTab::Wheels;
        egui::ScrollArea::vertical()
            .id_salt("simple_inspector_scroll")
            .show(ui, |ui| {
                let full_w = ui.available_width();
                if needs_width_cap {
                    // Center a constrained column — max ~480px for sliders
                    let max_content_w = 480.0_f32.min(full_w);
                    let pad = ((full_w - max_content_w) / 2.0).max(0.0);
                    if pad > 10.0 { ui.add_space(0.0); } // force layout
                    ui.set_max_width(max_content_w);
                    // Center by adding horizontal indent
                    if pad > 10.0 {
                        ui.indent("wide_center", |ui| {
                            ui.set_max_width(max_content_w);
                            Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, clip_time_secs, wide_layout);
                        });
                    } else {
                        Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, clip_time_secs, wide_layout);
                    }
                } else {
                    ui.set_min_width(full_w);
                    Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, clip_time_secs, wide_layout);
                }
            });

        cmds
    }

    fn render_tab_content(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        clip_time_secs: f64,
        wide_layout: bool,
    ) {
        match color_tab {
            ColorTab::Adjustments => {
                self.show_adjustments_tab(ui, graph, clip_id, selected_node, cmds, theme);
            }
            ColorTab::Wheels if wide_layout => {
                // Wide layout: combined wheels + sat/bri in unified columns
                self.show_wheels_wide(ui, graph, clip_id, cmds, theme);
            }
            _ => {
                let target_kinds: Vec<NodeKind> = match color_tab {
                    ColorTab::Wheels => vec![NodeKind::ColorWheels, NodeKind::WheelSatBri],
                    ColorTab::Curves => vec![NodeKind::RGBCurves],
                    ColorTab::HueSat => vec![NodeKind::HueSatCurves],
                    ColorTab::Warper => vec![NodeKind::ColorWarper],
                    _ => vec![],
                };
                for target_kind in &target_kinds {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == *target_kind && n.enabled) {
                        let node_id = node.id;
                        let node_kind = node.kind.clone();
                        *selected_node = Some(node_id);
                        let node_enabled = &mut node.enabled;
                        let params = &mut node.params;
                        let keyframes = &mut node.keyframes;
                        self.show_inspector_layout(ui, node_id, &node_kind, node_enabled,
                            params, keyframes, clip_id, cmds, theme, clip_time_secs, wide_layout);
                    }
                }
            }
        }
    }

    /// Wide-layout combined wheels + sat/bri — each wheel column includes its sliders beneath.
    fn show_wheels_wide(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Find both nodes
        let wheels_idx = graph.nodes.iter().position(|n| n.kind == NodeKind::ColorWheels && n.enabled);
        let satbri_idx = graph.nodes.iter().position(|n| n.kind == NodeKind::WheelSatBri && n.enabled);

        let avail_w = ui.available_width();
        // 4 columns with 2px gaps between them: 3 gaps = 6px
        let cell_w = ((avail_w - 6.0) / 4.0).floor();
        let wheel_d = (cell_w - 14.0).clamp(50.0, 150.0);

        // Linear / Log toggle
        if let Some(wi) = wheels_idx {
            let log_mode = graph.nodes[wi].params.wheels_log_mode.unwrap_or(false);
            let node_id = graph.nodes[wi].id;
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui.selectable_label(!log_mode, "Linear").clicked() && log_mode {
                    graph.nodes[wi].params.wheels_log_mode = Some(false);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "wheels_log_mode".into(), value: 0.0 });
                }
                if ui.selectable_label(log_mode, "Log (HDR)").clicked() && !log_mode {
                    graph.nodes[wi].params.wheels_log_mode = Some(true);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "wheels_log_mode".into(), value: 1.0 });
                }
            });
            ui.add_space(4.0);
        }

        // Helper: render one wheel + its sat/bri sliders in a column
        let wheels_node_id = wheels_idx.map(|i| graph.nodes[i].id);
        let satbri_node_id = satbri_idx.map(|i| graph.nodes[i].id);

        // Define the 4 wheel groups: (label, wheel_state_accessor, param_prefix, sat_param, bri_param)
        let groups: [(&str, &str); 4] = [
            ("Lift", "lift"), ("Gamma", "gamma"), ("Gain", "gain"), ("Offset", "offset"),
        ];

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, prefix) in &groups {
                let col_rect = ui.allocate_exact_size(
                    egui::vec2(cell_w, wheel_d + 120.0),
                    egui::Sense::hover(),
                ).0;
                let mut col_ui = ui.new_child(egui::UiBuilder::new().max_rect(col_rect).layout(egui::Layout::top_down(egui::Align::Center)));

                // Wheel
                if let Some(wi) = wheels_idx {
                    let params = &mut graph.nodes[wi].params;
                    let node_id = wheels_node_id.unwrap();
                    let (x_field, y_field, m_field) = match *prefix {
                        "lift" => (&mut params.lift_x, &mut params.lift_y, &mut params.lift_master),
                        "gamma" => (&mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master),
                        "gain" => (&mut params.gain_x, &mut params.gain_y, &mut params.gain_master),
                        _ => (&mut params.offset_x, &mut params.offset_y, &mut params.offset_master),
                    };
                    let mut offset = [x_field.unwrap_or(0.0), y_field.unwrap_or(0.0)];
                    let mut master = m_field.unwrap_or(0.0);
                    let wheel_state = match *prefix {
                        "lift" => &mut self.lift_wheel,
                        "gamma" => &mut self.gamma_wheel,
                        "gain" => &mut self.gain_wheel,
                        _ => &mut self.offset_wheel,
                    };
                    let resp = color_wheel::color_wheel_sized(
                        &mut col_ui, wheel_state, theme, label, &mut offset, &mut master, wheel_d, Some(cell_w),
                    );
                    if resp.changed {
                        *x_field = Some(offset[0]);
                        *y_field = Some(offset[1]);
                        *m_field = Some(master);
                        push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                    }
                }

                // Sat/Bri sliders painted manually below the wheel
                if let Some(si) = satbri_idx {
                    let params = &mut graph.nodes[si].params;
                    let node_id = satbri_node_id.unwrap();
                    let sat_name = format!("{prefix}_sat");
                    let bri_name = format!("{prefix}_bri");
                    let sat_field = match *prefix {
                        "lift" => &mut params.lift_sat,
                        "gamma" => &mut params.gamma_sat,
                        "gain" => &mut params.gain_sat,
                        _ => &mut params.offset_sat,
                    };
                    let bri_field = match *prefix {
                        "lift" => &mut params.lift_bri,
                        "gamma" => &mut params.gamma_bri,
                        "gain" => &mut params.gain_bri,
                        _ => &mut params.offset_bri,
                    };

                    let painter = col_ui.painter().clone();
                    let _text_dim = theme.text_dim;
                    let accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
                    let track_bg = egui::Color32::from_rgb(0x3a, 0x3a, 0x46);
                    let slider_h = 14.0;
                    let label_h = 12.0;
                    let margin = 12.0;
                    let track_w = cell_w - margin * 2.0;
                    let value_w = 30.0;
                    let track_draw_w = track_w - value_w - 4.0;

                    let cursor_y = col_ui.cursor().min.y;
                    let cx = col_rect.min.x + margin;

                    // -- Saturation --
                    let sat_label_y = cursor_y + 4.0;
                    painter.text(
                        egui::pos2(col_rect.center().x, sat_label_y + label_h / 2.0),
                        egui::Align2::CENTER_CENTER,
                        "Saturation",
                        egui::FontId::proportional(8.0),
                        egui::Color32::from_rgb(0x80, 0x80, 0x90),
                    );
                    let sat_track_y = sat_label_y + label_h + 2.0;
                    let sat_track_rect = egui::Rect::from_min_size(
                        egui::pos2(cx, sat_track_y + slider_h / 2.0 - 1.5),
                        egui::vec2(track_draw_w, 4.0),
                    );
                    painter.rect_filled(sat_track_rect, 2.0, track_bg);
                    let sat_hit = egui::Rect::from_min_size(egui::pos2(cx - 4.0, sat_track_y), egui::vec2(track_draw_w + 8.0, slider_h));
                    let sat_resp = col_ui.allocate_rect(sat_hit, egui::Sense::click_and_drag());
                    let mut s = sat_field.unwrap_or(0.0);
                    if sat_resp.dragged() || sat_resp.clicked() {
                        if let Some(pos) = sat_resp.interact_pointer_pos() {
                            let t = ((pos.x - cx) / track_draw_w).clamp(0.0, 1.0);
                            s = t * 2.0 - 1.0;
                            *sat_field = Some(s);
                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.clone(), value: s });
                        }
                    }
                    if sat_resp.double_clicked() || sat_resp.secondary_clicked() {
                        s = 0.0; *sat_field = Some(s);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name, value: 0.0 });
                    }
                    let sat_t = (s + 1.0) / 2.0;
                    let sat_thumb_x = cx + sat_t * track_draw_w;
                    painter.circle_filled(egui::pos2(sat_thumb_x, sat_track_y + slider_h / 2.0), 4.0, accent);
                    let is_default = s.abs() < 0.005;
                    let val_color = if is_default { egui::Color32::from_rgb(0x90, 0x90, 0xa0) } else { accent };
                    painter.text(
                        egui::pos2(cx + track_draw_w + value_w - 2.0, sat_track_y + slider_h / 2.0),
                        egui::Align2::RIGHT_CENTER,
                        format!("{s:+.2}"),
                        egui::FontId::monospace(8.0),
                        val_color,
                    );

                    // -- Brightness --
                    let bri_label_y = sat_track_y + slider_h + 4.0;
                    painter.text(
                        egui::pos2(col_rect.center().x, bri_label_y + label_h / 2.0),
                        egui::Align2::CENTER_CENTER,
                        "Brightness",
                        egui::FontId::proportional(8.0),
                        egui::Color32::from_rgb(0x80, 0x80, 0x90),
                    );
                    let bri_track_y = bri_label_y + label_h + 2.0;
                    let bri_track_rect = egui::Rect::from_min_size(
                        egui::pos2(cx, bri_track_y + slider_h / 2.0 - 1.5),
                        egui::vec2(track_draw_w, 4.0),
                    );
                    painter.rect_filled(bri_track_rect, 2.0, track_bg);
                    let bri_hit = egui::Rect::from_min_size(egui::pos2(cx - 4.0, bri_track_y), egui::vec2(track_draw_w + 8.0, slider_h));
                    let bri_resp = col_ui.allocate_rect(bri_hit, egui::Sense::click_and_drag());
                    let mut b = bri_field.unwrap_or(0.0);
                    if bri_resp.dragged() || bri_resp.clicked() {
                        if let Some(pos) = bri_resp.interact_pointer_pos() {
                            let t = ((pos.x - cx) / track_draw_w).clamp(0.0, 1.0);
                            b = t * 2.0 - 1.0;
                            *bri_field = Some(b);
                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.clone(), value: b });
                        }
                    }
                    if bri_resp.double_clicked() || bri_resp.secondary_clicked() {
                        b = 0.0; *bri_field = Some(b);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name, value: 0.0 });
                    }
                    let bri_t = (b + 1.0) / 2.0;
                    let bri_thumb_x = cx + bri_t * track_draw_w;
                    painter.circle_filled(egui::pos2(bri_thumb_x, bri_track_y + slider_h / 2.0), 4.0, accent);
                    let is_default = b.abs() < 0.005;
                    let val_color = if is_default { egui::Color32::from_rgb(0x90, 0x90, 0xa0) } else { accent };
                    painter.text(
                        egui::pos2(cx + track_draw_w + value_w - 2.0, bri_track_y + slider_h / 2.0),
                        egui::Align2::RIGHT_CENTER,
                        format!("{b:+.2}"),
                        egui::FontId::monospace(8.0),
                        val_color,
                    );
                }
            }
        });
    }

    /// Adjustments tab — matches the HTML mockup layout exactly.
    fn show_adjustments_tab(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Helper: find a node by kind
        let find_node = |graph: &mut NodeGraph, kind: NodeKind| -> Option<(NodeId, bool)> {
            graph.nodes.iter().find(|n| n.kind == kind).map(|n| (n.id, n.enabled))
        };

        // ── Exposure section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::Exposure) {
            *selected_node = Some(nid);
            let (reset, is_open) = section_header(ui, "EXPOSURE", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        node.params.exposure = Some(0.0);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "exposure".into(), value: 0.0 });
                    }
                    let mut val = node.params.exposure.unwrap_or(0.0);
                    if inspector_row(ui, "Exposure", &mut val, -3.0..=3.0, "stops", theme) {
                        node.params.exposure = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "exposure".into(), value: val });
                    }
                }
                section_divider(ui);
            }
        }

        // ── Contrast section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::Contrast) {
            let (reset, is_open) = section_header(ui, "CONTRAST", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        node.params.contrast = Some(0.0);
                        node.params.clarity = Some(0.0);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "contrast".into(), value: 0.0 });
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "clarity".into(), value: 0.0 });
                    }
                    let mut val = node.params.contrast.unwrap_or(0.0);
                    if inspector_row(ui, "Contrast", &mut val, -1.0..=1.0, "", theme) {
                        node.params.contrast = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "contrast".into(), value: val });
                    }
                    let mut cval = node.params.clarity.unwrap_or(0.0);
                    if inspector_row(ui, "Clarity", &mut cval, -1.0..=1.0, "", theme) {
                        node.params.clarity = Some(cval);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "clarity".into(), value: cval });
                    }
                }
                section_divider(ui);
            }
        }

        // ── Saturation section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::Saturation) {
            let (reset, is_open) = section_header(ui, "SATURATION", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        node.params.saturation = Some(1.0);
                        node.params.vibrance = Some(0.0);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "saturation".into(), value: 1.0 });
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "vibrance".into(), value: 0.0 });
                    }
                    let mut val = node.params.saturation.unwrap_or(1.0);
                    if inspector_row(ui, "Saturation", &mut val, 0.0..=2.0, "", theme) {
                        node.params.saturation = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "saturation".into(), value: val });
                    }
                    let mut vval = node.params.vibrance.unwrap_or(0.0);
                    if inspector_row(ui, "Vibrance", &mut vval, -1.0..=1.0, "", theme) {
                        node.params.vibrance = Some(vval);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "vibrance".into(), value: vval });
                    }
                }
                section_divider(ui);
            }
        }

        // ── Tone Controls section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::ToneControls) {
            let (reset, is_open) = section_header(ui, "TONE CONTROLS", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        for p in ["highlights", "shadows", "whites", "blacks"] {
                            node.params.apply_override(p, 0.0);
                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: p.into(), value: 0.0 });
                        }
                    }
                    for (label, param_name, default) in [
                        ("Highlights", "highlights", 0.0_f32),
                        ("Shadows", "shadows", 0.0),
                        ("Whites", "whites", 0.0),
                        ("Blacks", "blacks", 0.0),
                    ] {
                        let mut val = match param_name {
                            "highlights" => node.params.highlights.unwrap_or(default),
                            "shadows" => node.params.shadows.unwrap_or(default),
                            "whites" => node.params.whites.unwrap_or(default),
                            _ => node.params.blacks.unwrap_or(default),
                        };
                        if inspector_row(ui, label, &mut val, -1.0..=1.0, "", theme) {
                            node.params.apply_override(param_name, val);
                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: param_name.into(), value: val });
                        }
                    }
                }
                section_divider(ui);
            }
        }

        // ── Color Balance section — with gradient sliders ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::ColorBalance) {
            let (reset, is_open) = section_header(ui, "COLOR BALANCE", enabled, theme);
            if is_open { if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                if reset {
                    for (p, d) in [("temperature", 0.0), ("tint", 0.0), ("hue", 0.0), ("global_mix", 1.0)] {
                        node.params.apply_override(p, d);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: p.into(), value: d });
                    }
                }
                // Temperature: blue → orange gradient
                let mut temp = node.params.temperature.unwrap_or(0.0);
                if gradient_slider_row(ui, "Temperature", &mut temp, -50.0..=50.0,
                    egui::Color32::from_rgb(0x6a, 0xb4, 0xf5),
                    egui::Color32::from_rgb(0xf5, 0xa0, 0x5a), theme)
                {
                    node.params.temperature = Some(temp);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "temperature".into(), value: temp });
                }

                // Tint: green → pink gradient
                let mut tint = node.params.tint.unwrap_or(0.0);
                if gradient_slider_row(ui, "Tint", &mut tint, -50.0..=50.0,
                    egui::Color32::from_rgb(0xa8, 0xf5, 0xa8),
                    egui::Color32::from_rgb(0xf5, 0xa8, 0xc8), theme)
                {
                    node.params.tint = Some(tint);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "tint".into(), value: tint });
                }

                // Auto White Balance button
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if ui.add(egui::Button::new(
                        egui::RichText::new("Auto WB").size(11.0)
                    ).min_size(egui::vec2(60.0, 18.0))).clicked() {
                        cmds.push(AppCommand::AutoWhiteBalance { clip_id });
                    }
                });
                ui.add_space(2.0);

                // Hue: rainbow spectrum
                let mut hue = node.params.hue.unwrap_or(0.0);
                if hue_slider_row(ui, "Hue", &mut hue, theme) {
                    node.params.hue = Some(hue);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "hue".into(), value: hue });
                }

                // Global Mix: subtle grey → white
                let mut mix = node.params.global_mix.unwrap_or(1.0);
                if gradient_slider_row(ui, "Global Mix", &mut mix, 0.0..=1.0,
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 20),
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 90), theme)
                {
                    node.params.global_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "global_mix".into(), value: mix });
                }
            } }
        }

        // ── Noise Reduction section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::Denoise) {
            let (reset, is_open) = section_header(ui, "NOISE REDUCTION", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        node.params.denoise_luma = Some(0.0);
                        node.params.denoise_chroma = Some(0.0);
                        node.params.denoise_radius = Some(3);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_luma".into(), value: 0.0 });
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_chroma".into(), value: 0.0 });
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_radius".into(), value: 3.0 });
                    }
                    let mut luma = node.params.denoise_luma.unwrap_or(0.0);
                    if inspector_row(ui, "Luma", &mut luma, 0.0..=1.0, "", theme) {
                        node.params.denoise_luma = Some(luma);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_luma".into(), value: luma });
                    }
                    let mut chroma = node.params.denoise_chroma.unwrap_or(0.0);
                    if inspector_row(ui, "Chroma", &mut chroma, 0.0..=1.0, "", theme) {
                        node.params.denoise_chroma = Some(chroma);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_chroma".into(), value: chroma });
                    }
                    let mut radius = node.params.denoise_radius.unwrap_or(3) as f32;
                    if inspector_row(ui, "Radius", &mut radius, 1.0..=7.0, "px", theme) {
                        let r = radius.round() as u32;
                        node.params.denoise_radius = Some(r);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_radius".into(), value: r as f32 });
                    }
                }
                section_divider(ui);
            }
        }
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
        clip_time_secs: f64,
    ) {
        self.show_inspector_layout(ui, node_id, kind, enabled, params, _keyframes, clip_id, cmds, theme, clip_time_secs, false)
    }

    fn show_inspector_layout(
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
        clip_time_secs: f64,
        wide_layout: bool,
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
                let mut cval = params.clarity.unwrap_or(0.0);
                if grade_slider(ui, "Clarity", &mut cval, -1.0..=1.0, "") {
                    params.clarity = Some(cval);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "clarity".into(), value: cval });
                }
            }
            NodeKind::Saturation => {
                let mut val = params.saturation.unwrap_or(1.0);
                if grade_slider(ui, "Saturation", &mut val, 0.0..=2.0, "") {
                    params.saturation = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: val });
                }
                let mut vval = params.vibrance.unwrap_or(0.0);
                if grade_slider(ui, "Vibrance", &mut vval, -1.0..=1.0, "") {
                    params.vibrance = Some(vval);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "vibrance".into(), value: vval });
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
                let avail_w = ui.available_width();
                let four_col = wide_layout && avail_w >= 450.0;
                let two_col = !four_col && avail_w >= 250.0;
                let wheel_d = if four_col {
                    ((avail_w - 40.0) / 4.0).clamp(50.0, 130.0)
                } else if two_col {
                    ((avail_w - 20.0) / 2.0).clamp(50.0, 110.0)
                } else {
                    (avail_w - 10.0).clamp(60.0, 140.0)
                };

                // Linear / Log mode toggle tabs
                let log_mode = params.wheels_log_mode.unwrap_or(false);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let linear_sel = !log_mode;
                    let log_sel = log_mode;
                    if ui.selectable_label(linear_sel, "Linear").clicked() && !linear_sel {
                        params.wheels_log_mode = Some(false);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "wheels_log_mode".into(), value: 0.0 });
                    }
                    if ui.selectable_label(log_sel, "Log (HDR)").clicked() && !log_sel {
                        params.wheels_log_mode = Some(true);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "wheels_log_mode".into(), value: 1.0 });
                    }
                });
                ui.add_space(4.0);

                // Helper closure for each wheel
                let mut show_wheel = |ui: &mut egui::Ui, label: &str,
                    state: &mut ColorWheelState,
                    x: &mut Option<f32>, y: &mut Option<f32>, m: &mut Option<f32>,
                    prefix: &str|
                {
                    let mut offset = [x.unwrap_or(0.0), y.unwrap_or(0.0)];
                    let mut master = m.unwrap_or(0.0);
                    let resp = color_wheel::color_wheel_sized(
                        ui, state, theme, label, &mut offset, &mut master, wheel_d, None,
                    );
                    if resp.changed {
                        *x = Some(offset[0]); *y = Some(offset[1]); *m = Some(master);
                        push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                    }
                };

                if log_mode {
                    // Log wheels: Low / Mid / High (3 wheels for HDR)
                    if two_col {
                        let pair_w = (wheel_d + 10.0) * 2.0;
                        let center_pad = ((avail_w - pair_w) / 2.0).max(0.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Low", &mut self.log_low_wheel,
                                    &mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master, "log_low");
                            });
                            ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Mid", &mut self.log_mid_wheel,
                                    &mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master, "log_mid");
                            });
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            ui.allocate_ui(egui::vec2(wheel_d + 10.0, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "High", &mut self.log_high_wheel,
                                    &mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master, "log_high");
                            });
                        });
                    } else {
                        show_wheel(ui, "Low", &mut self.log_low_wheel,
                            &mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master, "log_low");
                        show_wheel(ui, "Mid", &mut self.log_mid_wheel,
                            &mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master, "log_mid");
                        show_wheel(ui, "High", &mut self.log_high_wheel,
                            &mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master, "log_high");
                    }
                } else {
                    // Linear wheels: Lift / Gamma / Gain / Offset (traditional)
                    if four_col {
                        // Wide layout: all 4 wheels in one horizontal row, left-aligned
                        let cell_w = wheel_d + 10.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.allocate_ui(egui::vec2(cell_w, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Lift", &mut self.lift_wheel,
                                    &mut params.lift_x, &mut params.lift_y, &mut params.lift_master, "lift");
                            });
                            ui.allocate_ui(egui::vec2(cell_w, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Gamma", &mut self.gamma_wheel,
                                    &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master, "gamma");
                            });
                            ui.allocate_ui(egui::vec2(cell_w, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Gain", &mut self.gain_wheel,
                                    &mut params.gain_x, &mut params.gain_y, &mut params.gain_master, "gain");
                            });
                            ui.allocate_ui(egui::vec2(cell_w, wheel_d + 50.0), |ui| {
                                show_wheel(ui, "Offset", &mut self.offset_wheel,
                                    &mut params.offset_x, &mut params.offset_y, &mut params.offset_master, "offset");
                            });
                        });
                    } else if two_col {
                        let pair_w = (wheel_d + 10.0) * 2.0;
                        let center_pad = ((avail_w - pair_w) / 2.0).max(0.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
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
                            ui.add_space(center_pad);
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
            }
            NodeKind::WheelSatBri => {
                let groups = [
                    ("LIFT", "lift_sat", "lift_bri", params.lift_sat.unwrap_or(0.0), params.lift_bri.unwrap_or(0.0)),
                    ("GAMMA", "gamma_sat", "gamma_bri", params.gamma_sat.unwrap_or(0.0), params.gamma_bri.unwrap_or(0.0)),
                    ("GAIN", "gain_sat", "gain_bri", params.gain_sat.unwrap_or(0.0), params.gain_bri.unwrap_or(0.0)),
                    ("OFFSET", "offset_sat", "offset_bri", params.offset_sat.unwrap_or(0.0), params.offset_bri.unwrap_or(0.0)),
                ];
                if wide_layout && ui.available_width() >= 450.0 {
                    // Wide: 4 columns matching wheel widths, left-aligned
                    let avail = ui.available_width();
                    // Match wheel cell_w: same formula as ColorWheels four_col
                    let wheel_d = ((avail - 40.0) / 4.0).clamp(50.0, 130.0);
                    let col_w = wheel_d + 10.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (group_label, sat_name, bri_name, sat_val, bri_val) in &groups {
                            ui.allocate_ui(egui::vec2(col_w, 55.0), |ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(egui::RichText::new(*group_label).size(8.0).color(theme.text_dim).strong());
                                let slider_w = col_w - 40.0; // room for label + value
                                let mut s = *sat_val;
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("S").size(8.0).color(theme.text_dim));
                                    ui.add_sized([slider_w, 14.0],
                                        egui::Slider::new(&mut s, -1.0..=1.0).show_value(false));
                                    ui.label(egui::RichText::new(format!("{s:.2}")).size(8.0).color(theme.text_dim));
                                });
                                if s != *sat_val {
                                    params.apply_override(sat_name, s);
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: (*sat_name).into(), value: s });
                                }
                                let mut b = *bri_val;
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("B").size(8.0).color(theme.text_dim));
                                    ui.add_sized([slider_w, 14.0],
                                        egui::Slider::new(&mut b, -1.0..=1.0).show_value(false));
                                    ui.label(egui::RichText::new(format!("{b:.2}")).size(8.0).color(theme.text_dim));
                                });
                                if b != *bri_val {
                                    params.apply_override(bri_name, b);
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: (*bri_name).into(), value: b });
                                }
                            });
                        }
                    });
                } else {
                    // Normal: stacked vertically with collapsible sections
                    for (group_label, sat_name, bri_name, sat_val, bri_val) in groups {
                        let (reset, is_open) = section_header(ui, group_label, true, theme);
                        if is_open {
                            if reset {
                                params.apply_override(sat_name, 0.0);
                                params.apply_override(bri_name, 0.0);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: 0.0 });
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.into(), value: 0.0 });
                            }
                            let mut s = sat_val;
                            if inspector_row(ui, "Saturation", &mut s, -1.0..=1.0, "", theme) {
                                params.apply_override(sat_name, s);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: s });
                            }
                            let mut b = bri_val;
                            if inspector_row(ui, "Brightness", &mut b, -1.0..=1.0, "", theme) {
                                params.apply_override(bri_name, b);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.into(), value: b });
                            }
                            section_divider(ui);
                        }
                    }
                }
            }
            NodeKind::RGBCurves => {
                // Section header: "RGB CURVES" with reset + enable
                let (reset, is_open) = section_header(ui, "RGB CURVES", *enabled, theme);
                if is_open && reset {
                    // Reset all curves to identity (2 points: [0,0] and [1,1])
                    if let Some(c) = &mut params.curves {
                        c.master = vec![[0.0, 0.0], [1.0, 1.0]];
                        c.red = vec![[0.0, 0.0], [1.0, 1.0]];
                        c.green = vec![[0.0, 0.0], [1.0, 1.0]];
                        c.blue = vec![[0.0, 0.0], [1.0, 1.0]];
                    }
                    params.curves_mix = Some(1.0);
                    params.curves_preserve_luma = Some(false);
                    cmds.push(AppCommand::SetNodeCurves {
                        clip_id, node_id,
                        curves: params.curves.clone().unwrap_or_default(),
                    });
                }

                if is_open {
                // Channel selector pills + eyedropper icon
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(PAD);
                    for (ch, label, color) in [
                        (CurveChannel::Master, "M", theme.text_accent),
                        (CurveChannel::Red, "R", egui::Color32::from_rgb(0xc0, 0x60, 0x60)),
                        (CurveChannel::Green, "G", egui::Color32::from_rgb(0x60, 0xb0, 0x60)),
                        (CurveChannel::Blue, "B", egui::Color32::from_rgb(0x60, 0x70, 0xc0)),
                    ] {
                        let active = self.active_curve_channel == ch;
                        let size = egui::vec2(26.0, 22.0);
                        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                        let bg = if active { color } else { egui::Color32::from_rgba_premultiplied(255, 255, 255, 18) };
                        let text_color = if active { egui::Color32::from_rgb(0x1a, 0x1a, 0x1c) } else { color };
                        ui.painter().rect_filled(rect, 4.0, bg);
                        ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(0.5,
                            egui::Color32::from_rgba_premultiplied(255, 255, 255, 30)));
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                            label, egui::FontId::proportional(11.0), text_color);
                        if resp.clicked() { self.active_curve_channel = ch; }
                    }

                    // Eyedropper button — pushed to the right
                    let remaining = ui.available_width() - PAD;
                    if remaining > 22.0 {
                        ui.add_space(remaining - 22.0);
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(22.0, 18.0), egui::Sense::click());
                        let icon_tint = if resp.hovered() {
                            theme.text_accent
                        } else {
                            egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
                        };
                        let icon_rect = egui::Rect::from_center_size(r.center(), egui::vec2(12.0, 12.0));
                        Icon::Eyedropper.image(12.0).tint(icon_tint).paint_at(ui, icon_rect);
                    }
                });
                ui.add_space(6.0);

                // Curve editor — fill available width, make it roughly square
                let curves = params.curves.get_or_insert_with(Default::default);
                let curve_w = ui.available_width() - PAD * 2.0;
                let curve_h = curve_w.clamp(100.0, 500.0);
                let ch = self.active_curve_channel;
                let mut curves_changed = false;
                {
                    let points = match ch {
                        CurveChannel::Master => &mut curves.master, CurveChannel::Red => &mut curves.red,
                        CurveChannel::Green => &mut curves.green, CurveChannel::Blue => &mut curves.blue,
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(PAD);
                        let resp = curves_editor::curves_editor_sized(ui, theme, ch, points, curve_h, None);
                        if resp.changed { curves_changed = true; }
                    });
                }
                if curves_changed {
                    cmds.push(AppCommand::SetNodeCurves { clip_id, node_id, curves: curves.clone() });
                }
                ui.add_space(6.0);

                // Preserve Luma checkbox
                ui.horizontal(|ui| {
                    ui.add_space(PAD);
                    let mut preserve = params.curves_preserve_luma.unwrap_or(false);
                    if ui.checkbox(&mut preserve, egui::RichText::new("Preserve Luma").size(11.0)
                        .color(egui::Color32::from_rgba_premultiplied(255, 255, 255, 140))).changed()
                    {
                        params.curves_preserve_luma = Some(preserve);
                    }
                });
                ui.add_space(2.0);

                // Mix slider — same style as Color tab
                let mut mix = params.curves_mix.unwrap_or(1.0);
                if inspector_row(ui, "Mix", &mut mix, 0.0..=1.0, "", theme) {
                    params.curves_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_mix".into(), value: mix });
                }
                } // is_open
            }
            NodeKind::HueSatCurves => {
                let huesat = params.huesat.get_or_insert_with(Default::default);

                // Stacked curve editors: LUMA (HvH), RED (HvS), GREEN (HvL), BLUE (LvS)
                // Plus SvS and OvS as additional curves
                // Matching FCP's Color Curves layout
                let curve_entries: Vec<(&str, egui::Color32, CurveChannel, usize)> = vec![
                    ("LUMA", egui::Color32::from_rgb(0xcc, 0xcc, 0xcc), CurveChannel::Master, 0),
                    ("RED", egui::Color32::from_rgb(0xff, 0x50, 0x50), CurveChannel::Red, 1),
                    ("GREEN", egui::Color32::from_rgb(0x40, 0xcc, 0x50), CurveChannel::Green, 2),
                    ("BLUE", egui::Color32::from_rgb(0x50, 0x60, 0xff), CurveChannel::Blue, 3),
                ];

                let curve_h = 90.0_f32;

                let mut huesat_changed = false;

                for (label, color, channel, curve_idx) in &curve_entries {
                    // Header: colored dot + label + reset icon + eyedropper
                    let mut reset_clicked = false;
                    ui.horizontal(|ui| {
                        ui.add_space(PAD);
                        // Colored dot
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 4.0, *color);
                        // Label
                        ui.label(egui::RichText::new(*label).size(10.0)
                            .color(egui::Color32::from_rgba_premultiplied(255, 255, 255, 180)));

                        // Push icons to the right
                        let remaining = ui.available_width() - PAD;
                        if remaining > 60.0 {
                            ui.add_space(remaining - 56.0);

                            // Eyedropper icon button (Phosphor)
                            let ed_resp = ui.allocate_rect(
                                ui.cursor().with_max_x(ui.cursor().min.x + 22.0).with_max_y(ui.cursor().min.y + 18.0),
                                egui::Sense::click(),
                            );
                            let ed_tint = if ed_resp.hovered() {
                                theme.text_accent
                            } else {
                                egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
                            };
                            let ed_icon_rect = egui::Rect::from_center_size(ed_resp.rect.center(), egui::vec2(12.0, 12.0));
                            Icon::Eyedropper.image(12.0).tint(ed_tint).paint_at(ui, ed_icon_rect);
                            ui.advance_cursor_after_rect(ed_resp.rect);

                            ui.add_space(4.0);

                            // Reset icon button (Phosphor ArrowCounterClockwise)
                            let rs_resp = ui.allocate_rect(
                                ui.cursor().with_max_x(ui.cursor().min.x + 22.0).with_max_y(ui.cursor().min.y + 18.0),
                                egui::Sense::click(),
                            );
                            let rs_tint = if rs_resp.hovered() {
                                theme.text_accent
                            } else {
                                egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
                            };
                            let rs_icon_rect = egui::Rect::from_center_size(rs_resp.rect.center(), egui::vec2(12.0, 12.0));
                            Icon::ArrowCounterClockwise.image(12.0).tint(rs_tint).paint_at(ui, rs_icon_rect);
                            ui.advance_cursor_after_rect(rs_resp.rect);

                            if rs_resp.clicked() {
                                reset_clicked = true;
                            }
                        }
                    });

                    // Handle reset
                    if reset_clicked {
                        match curve_idx {
                            0 => huesat.hue_vs_hue.clear(),
                            1 => huesat.hue_vs_sat.clear(),
                            2 => huesat.hue_vs_lum.clear(),
                            _ => huesat.lum_vs_sat.clear(),
                        }
                        huesat_changed = true;
                    }

                    // Curve editor
                    let points = match curve_idx {
                        0 => &mut huesat.hue_vs_hue,
                        1 => &mut huesat.hue_vs_sat,
                        2 => &mut huesat.hue_vs_lum,
                        _ => &mut huesat.lum_vs_sat,
                    };

                    let resp = curves_editor::curves_editor_sized(
                        ui, theme, *channel, points, curve_h, None,
                    );
                    if resp.changed { huesat_changed = true; }
                    ui.add_space(4.0);
                }

                if huesat_changed {
                    cmds.push(AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat: huesat.clone() });
                }

                ui.add_space(4.0);

                // Preserve Luma checkbox
                ui.horizontal(|ui| {
                    ui.add_space(PAD);
                    let mut preserve = params.curves_preserve_luma.unwrap_or(false);
                    if ui.checkbox(&mut preserve, egui::RichText::new("Preserve Luma").size(11.0)
                        .color(egui::Color32::from_rgba_premultiplied(255, 255, 255, 128))).changed()
                    {
                        params.curves_preserve_luma = Some(preserve);
                    }
                });
                ui.add_space(2.0);

                // Mix slider
                let mut mix = params.huesat_mix.unwrap_or(1.0);
                if inspector_row(ui, "Mix", &mut mix, 0.0..=1.0, "", theme) {
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
                let masks = params.masks.get_or_insert_with(Default::default);
                if masks.masks.is_empty() {
                    ui.label(egui::RichText::new("No masks. Add masks in the viewer.").size(10.0).color(theme.text_dim));
                }
                let mut masks_changed = false;
                let mask_count = masks.masks.len();
                for mask_idx in 0..mask_count {
                    // Gather read-only data from the mask before the mutable borrow phase
                    let shape_label = match &masks.masks[mask_idx].shape {
                        klipp_core::MaskShape::Circle { .. } => "Circle",
                        klipp_core::MaskShape::Rectangle { .. } => "Rectangle",
                        klipp_core::MaskShape::LinearGradient { .. } => "Linear Gradient",
                        klipp_core::MaskShape::Bezier { .. } => "Bezier",
                    };
                    let mask_enabled = masks.masks[mask_idx].enabled;
                    let animatable_params: Vec<&str> = klipp_core::MaskKeyframes::params_for_shape(&masks.masks[mask_idx].shape).to_vec();

                    // Pre-compute per-param display info to avoid holding borrows across UI calls
                    struct ParamInfo {
                        name: &'static str,
                        display_val: f32,
                        has_keyframes: bool,
                        has_kf_at_playhead: bool,
                        range: std::ops::RangeInclusive<f32>,
                    }
                    let param_infos: Vec<ParamInfo> = animatable_params.iter().map(|&param_name| {
                        let mask = &masks.masks[mask_idx];
                        let static_val = mask.param_value(param_name).unwrap_or(0.0);
                        let track = mask.keyframes.track(param_name);
                        let has_keyframes = track.map_or(false, |t| !t.is_empty());
                        let display_val = if has_keyframes {
                            klipp_core::keyframe::evaluate(track.unwrap(), clip_time_secs, static_val)
                        } else {
                            static_val
                        };
                        let has_kf_at_playhead = track.map_or(false, |t| {
                            t.iter().any(|k| (k.time_secs - clip_time_secs).abs() < 1e-3)
                        });
                        let range = match param_name {
                            "center_x" | "center_y" | "start_x" | "start_y" | "end_x" | "end_y" => 0.0..=1.0,
                            "radius" | "width" | "height" | "feather" | "opacity" => 0.0..=1.0,
                            "rotation" => -180.0..=180.0,
                            _ => 0.0..=1.0,
                        };
                        ParamInfo { name: param_name, display_val, has_keyframes, has_kf_at_playhead, range }
                    }).collect();

                    ui.add_space(4.0);
                    // Header row with enabled checkbox
                    {
                        let mask = &mut masks.masks[mask_idx];
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            if ui.checkbox(&mut mask.enabled, "").changed() { masks_changed = true; }
                            ui.label(egui::RichText::new(format!("Mask {} - {}", mask_idx + 1, shape_label))
                                .size(11.0).color(theme.text_accent));
                        });
                    }
                    if !mask_enabled { continue; }

                    // Parameter sliders with keyframe diamonds
                    for info in &param_infos {
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);

                            // Keyframe diamond button
                            let diamond_color = if info.has_kf_at_playhead {
                                theme.text_accent
                            } else if info.has_keyframes {
                                egui::Color32::from_rgba_premultiplied(255, 191, 0, 128)
                            } else {
                                theme.text_dim
                            };
                            let diamond_text = if info.has_kf_at_playhead { "\u{25C6}" } else { "\u{25C7}" };
                            let diamond_resp = ui.add(
                                egui::Button::new(egui::RichText::new(diamond_text).size(12.0).color(diamond_color))
                                    .frame(false)
                            );
                            if diamond_resp.clicked() {
                                if info.has_kf_at_playhead {
                                    cmds.push(AppCommand::RemoveMaskKeyframe {
                                        clip_id, node_id, mask_index: mask_idx,
                                        param: info.name.to_string(), time_secs: clip_time_secs,
                                    });
                                } else {
                                    cmds.push(AppCommand::AddMaskKeyframe {
                                        clip_id, node_id, mask_index: mask_idx,
                                        param: info.name.to_string(), time_secs: clip_time_secs,
                                        value: info.display_val,
                                    });
                                }
                            }
                            // Context menu for clearing keyframes
                            if info.has_keyframes {
                                diamond_resp.context_menu(|ui| {
                                    if ui.button("Clear all keyframes").clicked() {
                                        cmds.push(AppCommand::ClearMaskKeyframes {
                                            clip_id, node_id, mask_index: mask_idx,
                                            param: info.name.to_string(),
                                        });
                                        ui.close_menu();
                                    }
                                });
                            }
                            diamond_resp.on_hover_text(if info.has_kf_at_playhead {
                                "Remove keyframe at playhead"
                            } else {
                                "Add keyframe at playhead"
                            });

                            // Label
                            ui.label(egui::RichText::new(info.name.replace('_', " "))
                                .size(10.0).color(theme.text_dim));
                            ui.add_space(4.0);

                            // Slider
                            let mut val = info.display_val;
                            let resp = ui.add(egui::Slider::new(&mut val, info.range.clone())
                                .show_value(true)
                                .custom_formatter(|v, _| format!("{:.3}", v))
                                .min_decimals(3)
                            );
                            if resp.changed() {
                                if info.has_keyframes {
                                    // When keyframed, editing the slider adds/updates a keyframe at playhead
                                    cmds.push(AppCommand::AddMaskKeyframe {
                                        clip_id, node_id, mask_index: mask_idx,
                                        param: info.name.to_string(), time_secs: clip_time_secs,
                                        value: val,
                                    });
                                } else {
                                    // Static: update the mask param directly via command
                                    let mask = &mut masks.masks[mask_idx];
                                    match info.name {
                                        "feather" => mask.feather = val,
                                        "opacity" => mask.opacity = val,
                                        _ => match &mut mask.shape {
                                            klipp_core::MaskShape::Circle { center_x, center_y, radius } => match info.name {
                                                "center_x" => *center_x = val,
                                                "center_y" => *center_y = val,
                                                "radius" => *radius = val,
                                                _ => {}
                                            },
                                            klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation } => match info.name {
                                                "center_x" => *center_x = val,
                                                "center_y" => *center_y = val,
                                                "width" => *width = val,
                                                "height" => *height = val,
                                                "rotation" => *rotation = val,
                                                _ => {}
                                            },
                                            klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => match info.name {
                                                "start_x" => *start_x = val,
                                                "start_y" => *start_y = val,
                                                "end_x" => *end_x = val,
                                                "end_y" => *end_y = val,
                                                _ => {}
                                            },
                                            _ => {}
                                        }
                                    }
                                    masks_changed = true;
                                }
                            }
                        });
                    }

                    // Invert checkbox
                    {
                        let mask = &mut masks.masks[mask_idx];
                        ui.horizontal(|ui| {
                            ui.add_space(30.0);
                            if ui.checkbox(&mut mask.invert, egui::RichText::new("Invert").size(10.0)).changed() {
                                masks_changed = true;
                            }
                        });
                    }
                }
                if masks_changed {
                    cmds.push(AppCommand::SetNodeMasks { clip_id, node_id, masks: masks.clone() });
                }
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
            NodeKind::Denoise => {
                let mut luma = params.denoise_luma.unwrap_or(0.0);
                if grade_slider(ui, "Luma Strength", &mut luma, 0.0..=1.0, "") {
                    params.denoise_luma = Some(luma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_luma".into(), value: luma });
                }
                let mut chroma = params.denoise_chroma.unwrap_or(0.0);
                if grade_slider(ui, "Chroma Strength", &mut chroma, 0.0..=1.0, "") {
                    params.denoise_chroma = Some(chroma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_chroma".into(), value: chroma });
                }
                let mut radius = params.denoise_radius.unwrap_or(3) as f32;
                if grade_slider(ui, "Radius", &mut radius, 1.0..=7.0, "px") {
                    let r = radius.round() as u32;
                    params.denoise_radius = Some(r);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_radius".into(), value: r as f32 });
                }
            }
            NodeKind::ColorWarper => {
                self.show_warper_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::CombineMattes => { ui.label("Multiplies two matte inputs"); }
            NodeKind::Source | NodeKind::Output | NodeKind::Layer
            | NodeKind::ColorMatch | NodeKind::Unknown => {
                ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
            }
        }
    }

    /// Draw the 2D color warper widget: circular hue/sat grid with 12 draggable control points.
    fn show_warper_widget(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        params: &mut NodeParams,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        let warper = params.warper_params.get_or_insert_with(Default::default);

        let size = ui.available_width().min(250.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
        let center = rect.center();
        let radius = size / 2.0 - 16.0;
        let painter = ui.painter();

        // Background circle
        painter.circle_filled(center, radius + 4.0, egui::Color32::from_rgb(0x18, 0x18, 0x20));
        painter.circle_stroke(center, radius, egui::Stroke::new(1.0, egui::Color32::from_rgb(0x30, 0x30, 0x38)));

        // Crosshair lines
        painter.line_segment(
            [egui::pos2(center.x - radius, center.y), egui::pos2(center.x + radius, center.y)],
            egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 15)),
        );
        painter.line_segment(
            [egui::pos2(center.x, center.y - radius), egui::pos2(center.x, center.y + radius)],
            egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 15)),
        );

        // Inner reference circles at 33% and 66% radius
        painter.circle_stroke(center, radius * 0.33, egui::Stroke::new(0.3, egui::Color32::from_rgba_premultiplied(255, 255, 255, 10)));
        painter.circle_stroke(center, radius * 0.66, egui::Stroke::new(0.3, egui::Color32::from_rgba_premultiplied(255, 255, 255, 10)));

        let sector_labels = ["R", "O", "Y", "YG", "G", "CG", "C", "CB", "B", "BM", "M", "MR"];
        let neutral_r = radius * 0.7;

        // Draw subtle hue wedge markers and sector labels
        for i in 0..12 {
            let angle = (i as f32 * 30.0).to_radians();
            let hue_deg = i as f32 * 30.0;
            let (hr, hg, hb) = klipp_color::hsv_to_rgb(hue_deg, 1.0, 1.0);

            // Radial line for each sector boundary
            let line_angle = ((i as f32 * 30.0) - 15.0).to_radians();
            let lx0 = center.x + line_angle.cos() * radius * 0.15;
            let ly0 = center.y - line_angle.sin() * radius * 0.15;
            let lx1 = center.x + line_angle.cos() * radius;
            let ly1 = center.y - line_angle.sin() * radius;
            painter.line_segment(
                [egui::pos2(lx0, ly0), egui::pos2(lx1, ly1)],
                egui::Stroke::new(0.3, egui::Color32::from_rgba_premultiplied(255, 255, 255, 12)),
            );

            // Subtle hue dot at center of each sector
            let dot_x = center.x + angle.cos() * radius * 0.5;
            let dot_y = center.y - angle.sin() * radius * 0.5;
            painter.circle_filled(
                egui::pos2(dot_x, dot_y),
                8.0,
                egui::Color32::from_rgba_premultiplied(
                    (hr * 40.0) as u8, (hg * 40.0) as u8, (hb * 40.0) as u8, 40
                ),
            );

            // Labels outside circle
            let lx = center.x + angle.cos() * (radius + 12.0);
            let ly = center.y - angle.sin() * (radius + 12.0);
            painter.text(
                egui::pos2(lx, ly),
                egui::Align2::CENTER_CENTER,
                sector_labels[i],
                egui::FontId::proportional(8.0),
                egui::Color32::from_rgba_premultiplied(
                    (hr * 180.0 + 75.0).min(255.0) as u8,
                    (hg * 180.0 + 75.0).min(255.0) as u8,
                    (hb * 180.0 + 75.0).min(255.0) as u8,
                    180,
                ),
            );
        }

        // Draw control points and shift indicators
        for i in 0..12 {
            let base_angle = (i as f32 * 30.0).to_radians();
            let hue_deg = i as f32 * 30.0;
            let (hr, hg, hb) = klipp_color::hsv_to_rgb(hue_deg, 1.0, 1.0);

            // Neutral position on the circle
            let nx = center.x + base_angle.cos() * neutral_r;
            let ny = center.y - base_angle.sin() * neutral_r;

            // Shifted position: tangential = hue shift, radial = sat shift
            let hue_offset_angle = warper.hue_shifts[i].to_radians() * 0.02; // scale down for UI
            let sat_offset_r = warper.sat_shifts[i] * radius * 0.25;

            let point_angle = base_angle + hue_offset_angle;
            let point_r = neutral_r + sat_offset_r;

            let px = center.x + point_angle.cos() * point_r;
            let py = center.y - point_angle.sin() * point_r;

            // Draw shift line from neutral to current position
            if warper.hue_shifts[i].abs() > 0.1 || warper.sat_shifts[i].abs() > 0.01 {
                painter.line_segment(
                    [egui::pos2(nx, ny), egui::pos2(px, py)],
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(
                        (hr * 200.0) as u8, (hg * 200.0) as u8, (hb * 200.0) as u8, 100
                    )),
                );
            }

            // Control point dot
            let point_color = egui::Color32::from_rgb(
                (hr * 255.0) as u8, (hg * 255.0) as u8, (hb * 255.0) as u8
            );
            painter.circle_filled(egui::pos2(px, py), 5.0, point_color);
            painter.circle_stroke(egui::pos2(px, py), 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }

        // Handle drag interaction
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let dx = pos.x - center.x;
                let dy = -(pos.y - center.y); // flip Y for math convention
                let angle = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                let dist = (dx * dx + dy * dy).sqrt();

                // Find nearest sector
                let sector = ((angle / 30.0).round() as usize) % 12;

                // Compute hue shift (tangential displacement) and sat shift (radial)
                let base_angle = (sector as f32 * 30.0).to_radians();

                // Tangential component = hue shift
                let tangent_x = -base_angle.sin();
                let tangent_y = base_angle.cos();
                let tangent_proj = dx * tangent_x + dy * tangent_y;
                let new_hue_shift = (tangent_proj / radius * 180.0).clamp(-180.0, 180.0);

                // Radial component = sat shift
                let new_sat_shift = ((dist - neutral_r) / (radius * 0.25)).clamp(-1.0, 1.0);

                warper.hue_shifts[sector] = new_hue_shift;
                warper.sat_shifts[sector] = new_sat_shift;

                cmds.push(AppCommand::SetNodeParam {
                    clip_id,
                    node_id,
                    param: format!("warper_hue_{sector}"),
                    value: new_hue_shift,
                });
                cmds.push(AppCommand::SetNodeParam {
                    clip_id,
                    node_id,
                    param: format!("warper_sat_{sector}"),
                    value: new_sat_shift,
                });
            }
        }

        // Reset button
        ui.add_space(6.0);
        if ui.small_button("Reset Warper").clicked() {
            warper.hue_shifts = [0.0; 12];
            warper.sat_shifts = [0.0; 12];
            for s in 0..12 {
                cmds.push(AppCommand::SetNodeParam {
                    clip_id,
                    node_id,
                    param: format!("warper_hue_{s}"),
                    value: 0.0,
                });
                cmds.push(AppCommand::SetNodeParam {
                    clip_id,
                    node_id,
                    param: format!("warper_sat_{s}"),
                    value: 0.0,
                });
            }
        }

        // Show active sector values as compact readout
        ui.add_space(4.0);
        let active_count = (0..12).filter(|&i| warper.hue_shifts[i].abs() > 0.1 || warper.sat_shifts[i].abs() > 0.01).count();
        if active_count > 0 {
            ui.label(egui::RichText::new(format!("{active_count} sector(s) adjusted")).size(9.0).color(theme.text_dim));
        }
    }
}

// ── Mockup-matching helpers ──────────────────────────────────────────────────

const PAD: f32 = 14.0;
const ROW_H: f32 = 28.0;

/// Section header: "EXPOSURE" with enabled dot and reset icon.
/// Returns true if the reset button was clicked.
/// Collapsible section header matching the inspector card style.
/// Returns (reset_clicked, is_open).
fn section_header(ui: &mut egui::Ui, title: &str, enabled: bool, theme: &KlippTheme) -> (bool, bool) {
    ui.add_space(2.0);
    let avail_w = ui.available_width();
    let header_h = 28.0;

    // Collapse state
    let collapsed_key = egui::Id::new("color_section_collapsed");
    let is_collapsed: bool = ui.ctx().data_mut(|d| {
        let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
        set.contains(title)
    });
    let is_open = !is_collapsed;

    let (rect, header_resp) = ui.allocate_exact_size(egui::vec2(avail_w, header_h), egui::Sense::click());
    let cy = rect.center().y;

    // Background card
    let rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding::same(6.0)
    };
    ui.painter().rect_filled(rect, rounding, theme.bg_deep);

    // Left accent bar
    let accent_rounding = if is_open {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 0.0, se: 0.0 }
    } else {
        egui::Rounding { nw: 6.0, ne: 0.0, sw: 6.0, se: 0.0 }
    };
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(3.0, header_h)),
        accent_rounding, theme.border,
    );

    // Caret icon
    let caret_x = rect.min.x + 14.0;
    let caret = if is_open { "\u{25BE}" } else { "\u{25B8}" }; // ▾ or ▸
    ui.painter().text(
        egui::pos2(caret_x, cy),
        egui::Align2::CENTER_CENTER,
        caret,
        egui::FontId::proportional(10.0),
        egui::Color32::from_rgba_premultiplied(255, 255, 255, 100),
    );

    // Section title
    ui.painter().text(
        egui::pos2(rect.min.x + 24.0, cy),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    // Enabled dot
    let dot_x = rect.max.x - PAD - 22.0;
    let dot_color = if enabled { theme.text_accent } else { egui::Color32::from_rgba_premultiplied(255, 255, 255, 46) };
    ui.painter().circle_filled(egui::pos2(dot_x, cy), 3.0, dot_color);

    // Reset button
    let icon_x = rect.max.x - PAD - 6.0;
    let reset_rect = egui::Rect::from_center_size(egui::pos2(icon_x, cy), egui::vec2(16.0, 16.0));
    let reset_resp = ui.allocate_rect(reset_rect, egui::Sense::click());
    let reset_tint = if reset_resp.hovered() {
        theme.text_accent
    } else {
        egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)
    };
    let icon_rect = egui::Rect::from_center_size(egui::pos2(icon_x, cy), egui::vec2(12.0, 12.0));
    Icon::ArrowCounterClockwise.image(12.0).tint(reset_tint).paint_at(ui, icon_rect);

    // Toggle collapse on header click (but not on reset button)
    if header_resp.clicked() && !reset_resp.clicked() {
        ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
            if is_collapsed {
                set.remove(title);
            } else {
                set.insert(title.to_string());
            }
        });
    }

    (reset_resp.clicked(), is_open)
}

/// 0.5px divider line between sections
fn section_divider(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 18));
    ui.add_space(4.0);
}

/// Single parameter row: [label] [==slider==] [value] [suffix]
/// Matches the HTML mockup — shorter slider, proper proportions.
fn inspector_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    theme: &KlippTheme,
) -> bool {
    let avail_w = ui.available_width();
    let range_span = (range.end() - range.start()).abs();
    let speed = range_span / 800.0;

    // Proportions matching the mockup:
    // Label: ~28% | Slider: ~38% | Value: ~18% | Suffix: ~12% | Padding: rest
    let label_w = (avail_w * 0.28).max(50.0);
    let suffix_w = if suffix.is_empty() { 0.0 } else { (avail_w * 0.10).max(28.0) };
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - suffix_w - 8.0).max(40.0);

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());

    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 13));
    }

    let cy = row_rect.center().y;
    let pct = ((*value - range.start()) / range_span).clamp(0.0, 1.0);

    {
        let painter = ui.painter().clone();

        // Label
        painter.text(
            egui::pos2(row_rect.min.x + PAD, cy),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 107),
        );

        // Slider track
        let slider_x = row_rect.min.x + PAD + label_w;
        let track_y = cy - 1.5;

        // Filled portion (amber, left of thumb)
        let fill_w = slider_w * pct;
        if fill_w > 0.5 {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(slider_x, track_y), egui::vec2(fill_w, 3.0)),
                1.5,
                egui::Color32::from_rgba_premultiplied(
                    theme.text_accent.r(), theme.text_accent.g(), theme.text_accent.b(), 160,
                ),
            );
        }
        // Unfilled portion (dark grey, right of thumb)
        let unfill_x = slider_x + fill_w;
        let unfill_w = slider_w - fill_w;
        if unfill_w > 0.5 {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(unfill_x, track_y), egui::vec2(unfill_w, 3.0)),
                1.5,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 26),
            );
        }

        // Thumb — clean white circle with subtle shadow
        let thumb_x = slider_x + fill_w;
        let thumb_r = 6.0;
        painter.circle_filled(egui::pos2(thumb_x, cy), thumb_r + 0.5,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 60));
        painter.circle_filled(egui::pos2(thumb_x, cy), thumb_r,
            egui::Color32::from_rgb(0xf0, 0xf0, 0xf0));

        // Value
        let value_x = slider_x + slider_w + 8.0;
        painter.text(
            egui::pos2(value_x, cy),
            egui::Align2::LEFT_CENTER,
            format!("{:.2}", *value),
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 166),
        );

        // Suffix
        if !suffix.is_empty() {
            painter.text(
                egui::pos2(value_x + value_w, cy),
                egui::Align2::LEFT_CENTER,
                suffix,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 107),
            );
        }
    }

    // Slider interaction
    let slider_x = row_rect.min.x + PAD + label_w;
    let interact_rect = egui::Rect::from_min_size(
        egui::pos2(slider_x - 6.0, cy - 12.0),
        egui::vec2(slider_w + 12.0, 24.0),
    );
    let slider_resp = ui.allocate_rect(interact_rect, egui::Sense::click_and_drag());
    let mut changed = false;

    if slider_resp.dragged() || slider_resp.clicked() {
        if let Some(pos) = slider_resp.interact_pointer_pos() {
            let new_pct = ((pos.x - slider_x) / slider_w).clamp(0.0, 1.0);
            *value = range.start() + new_pct * range_span;
            changed = true;
        }
    }

    // Value drag (precision adjustment by dragging the number)
    let value_x = slider_x + slider_w + 8.0;
    let val_rect = egui::Rect::from_min_size(
        egui::pos2(value_x - 4.0, cy - 10.0),
        egui::vec2(value_w + suffix_w + 8.0, 20.0),
    );
    let val_resp = ui.allocate_rect(val_rect, egui::Sense::click_and_drag());
    if val_resp.dragged() {
        let delta = val_resp.drag_delta().x * speed;
        *value = (*value + delta).clamp(*range.start(), *range.end());
        changed = true;
    }

    changed
}

/// Gradient slider row: track has a left→right color gradient instead of amber fill.
/// Used for Temperature (blue→orange), Tint (green→pink), Global Mix.
fn gradient_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    color_left: egui::Color32,
    color_right: egui::Color32,
    _theme: &KlippTheme,
) -> bool {
    let avail_w = ui.available_width();
    let range_span = (range.end() - range.start()).abs();
    let label_w = (avail_w * 0.28).max(50.0);
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - 8.0).max(40.0);
    let pct = ((*value - range.start()) / range_span).clamp(0.0, 1.0);

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 13));
    }
    let cy = row_rect.center().y;
    let slider_x = row_rect.min.x + PAD + label_w;

    {
        let painter = ui.painter().clone();

        painter.text(egui::pos2(row_rect.min.x + PAD, cy), egui::Align2::LEFT_CENTER,
            label, egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 107));

        // Gradient track — draw pixel by pixel horizontally
        let track_h = 6.0;
        let track_y = cy - track_h / 2.0;
        let steps = slider_w as usize;
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            let r = (color_left.r() as f32 * (1.0 - t) + color_right.r() as f32 * t) as u8;
            let g = (color_left.g() as f32 * (1.0 - t) + color_right.g() as f32 * t) as u8;
            let b = (color_left.b() as f32 * (1.0 - t) + color_right.b() as f32 * t) as u8;
            let px = slider_x + i as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(px, track_y), egui::vec2(1.0, track_h)),
                0.0, egui::Color32::from_rgb(r, g, b),
            );
        }
        // Round ends
        painter.circle_filled(egui::pos2(slider_x, cy), track_h / 2.0, color_left);
        painter.circle_filled(egui::pos2(slider_x + slider_w, cy), track_h / 2.0, color_right);

        // Thumb
        let thumb_x = slider_x + slider_w * pct;
        painter.circle_filled(egui::pos2(thumb_x, cy), 6.5, egui::Color32::from_rgba_premultiplied(0, 0, 0, 60));
        painter.circle_filled(egui::pos2(thumb_x, cy), 6.0, egui::Color32::from_rgb(0xf0, 0xf0, 0xf0));

        // Value
        painter.text(egui::pos2(slider_x + slider_w + 8.0, cy), egui::Align2::LEFT_CENTER,
            format!("{:.2}", *value), egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 166));
    }

    let interact_rect = egui::Rect::from_min_size(
        egui::pos2(slider_x - 6.0, cy - 12.0), egui::vec2(slider_w + 12.0, 24.0));
    let slider_resp = ui.allocate_rect(interact_rect, egui::Sense::click_and_drag());
    let mut changed = false;
    if slider_resp.dragged() || slider_resp.clicked() {
        if let Some(pos) = slider_resp.interact_pointer_pos() {
            *value = range.start() + ((pos.x - slider_x) / slider_w).clamp(0.0, 1.0) * range_span;
            changed = true;
        }
    }
    changed
}

/// Hue slider row: rainbow spectrum track.
fn hue_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    _theme: &KlippTheme,
) -> bool {
    let avail_w = ui.available_width();
    let label_w = (avail_w * 0.28).max(50.0);
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - 8.0).max(40.0);
    let pct = (*value / 360.0).clamp(0.0, 1.0);

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 13));
    }
    let cy = row_rect.center().y;
    let slider_x = row_rect.min.x + PAD + label_w;

    {
        let painter = ui.painter().clone();

        painter.text(egui::pos2(row_rect.min.x + PAD, cy), egui::Align2::LEFT_CENTER,
            label, egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 107));

        // Rainbow spectrum track
        let track_h = 6.0;
        let track_y = cy - track_h / 2.0;
        let steps = slider_w as usize;
        for i in 0..steps {
            let hue = i as f32 / steps as f32;
            let (r, g, b) = crate::widgets::scopes::hue_to_rgb(hue);
            let px = slider_x + i as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(px, track_y), egui::vec2(1.0, track_h)),
                0.0, egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8),
            );
        }

        // Thumb
        let thumb_x = slider_x + slider_w * pct;
        painter.circle_filled(egui::pos2(thumb_x, cy), 6.5, egui::Color32::from_rgba_premultiplied(0, 0, 0, 60));
        painter.circle_filled(egui::pos2(thumb_x, cy), 6.0, egui::Color32::from_rgb(0xf0, 0xf0, 0xf0));

        // Value
        painter.text(egui::pos2(slider_x + slider_w + 8.0, cy), egui::Align2::LEFT_CENTER,
            format!("{:.2}", *value), egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 166));
    }

    let interact_rect = egui::Rect::from_min_size(
        egui::pos2(slider_x - 6.0, cy - 12.0), egui::vec2(slider_w + 12.0, 24.0));
    let slider_resp = ui.allocate_rect(interact_rect, egui::Sense::click_and_drag());
    let mut changed = false;
    if slider_resp.dragged() || slider_resp.clicked() {
        if let Some(pos) = slider_resp.interact_pointer_pos() {
            *value = ((pos.x - slider_x) / slider_w).clamp(0.0, 1.0) * 360.0;
            changed = true;
        }
    }
    changed
}

/// Legacy grade_slider for non-Adjustments tabs (backward compatible)
fn grade_slider(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    // Use a dummy theme — the inspector_row reads accent from theme
    // For legacy calls, use the simpler layout
    let pad = 14.0;
    let w = (ui.available_width() - pad * 2.0).max(60.0);
    let range_span = (range.end() - range.start()).abs();
    let speed = range_span / 800.0;
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.label(egui::RichText::new(label).size(11.0).color(
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 107)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad);
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
    ui.horizontal(|ui| {
        ui.add_space(pad);
        let resp = ui.add_sized(
            [w, 10.0],
            egui::Slider::new(value, range).show_value(false).clamping(egui::SliderClamping::Always),
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
