use crate::inspector::keyframe_selection::FrameSelection;
use crate::icons::Icon;
use crate::presets::{self, ToolPreset, ToolScope};
use crate::state::ColorTab;
use crate::theme::KlippTheme;
use crate::widgets::color_wheel::{self, ColorWheelState};
use crate::widgets::curves_editor::{self, CurveChannel};
use klipp_core::{AppCommand, ClipId};
use klipp_nodes::{NodeGraph, NodeId, NodeKind, NodeParams};

// `ensure_corrector_node` now lives on `klipp_nodes::NodeGraph` as
// `ensure_corrector` — called from both this module and the SetNode*
// handlers in `app.rs` when a phantom-id dispatch materialises a real
// Corrector.

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
    // ── Tool presets ──
    pub wheels_presets: Vec<ToolPreset>,
    pub curves_presets: Vec<ToolPreset>,
    pub huesat_presets: Vec<ToolPreset>,
    pub warper_presets: Vec<ToolPreset>,
    /// The viewer frame's histogram, drawn behind the curves.
    ///
    /// A curve without the image's distribution behind it is placed
    /// blind: you cannot see where the data you are moving actually
    /// lives, which is most of what the tool is for. The widget has
    /// taken an optional histogram since it was written and every call
    /// site passed `None`.
    ///
    /// Computed from the same `SharedFrameBuffer` the Colour Check
    /// window reads, and gated on its generation, so a paused viewer
    /// costs one pass and not one per repaint.
    curve_histogram: Option<klipp_color::scopes::HistogramData>,
    curve_histogram_generation: u64,
    pub color_presets: Vec<ToolPreset>,
    pub preset_save_name_tool: String,
    pub preset_saving_tool: Option<ToolScope>,
    /// The selected clip's input colour space and whether detection
    /// chose it, set by the caller each frame: the InputTransform node
    /// edits the media item's format, which this panel cannot see.
    pub input_colour_space: (klipp_core::LogFormat, bool),
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
            wheels_presets: presets::load_tool_presets(&ToolScope::Wheels),
            curves_presets: presets::load_tool_presets(&ToolScope::Curves),
            huesat_presets: presets::load_tool_presets(&ToolScope::HueSat),
            warper_presets: presets::load_tool_presets(&ToolScope::Warper),
            curve_histogram: None,
            curve_histogram_generation: u64::MAX,
            color_presets: presets::load_tool_presets(&ToolScope::Color),
            preset_save_name_tool: String::new(),
            preset_saving_tool: None,
            input_colour_space: (klipp_core::LogFormat::Srgb, false),
        }
    }
}

impl NodeInspectorPanel {
    /// Recompute the curves' histogram when the viewer shows a new
    /// frame. Cheap enough to call every frame: it returns immediately
    /// unless the generation moved.
    pub fn refresh_curve_histogram(&mut self, frame_buffer: &klipp_engine::SharedFrameBuffer) {
        let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
            return;
        }
        if fb.generation == self.curve_histogram_generation {
            return;
        }
        self.curve_histogram_generation = fb.generation;
        self.curve_histogram = Some(klipp_color::scopes::compute_histogram(
            &fb.data, fb.width, fb.height,
        ));
    }

    /// The 256 bins for one curve channel, if a frame has been measured.
    fn histogram_for(&self, channel: curves_editor::CurveChannel) -> Option<&[u32; 256]> {
        let h = self.curve_histogram.as_ref()?;
        Some(match channel {
            curves_editor::CurveChannel::Red => &h.r,
            curves_editor::CurveChannel::Green => &h.g,
            curves_editor::CurveChannel::Blue => &h.b,
            _ => &h.luma,
        })
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &Option<NodeId>,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        egui::ScrollArea::vertical()
            .id_salt("node_inspector_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());

                if let Some(sel_id) = *selected_node {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == sel_id) {
                        let node_id = node.id;
                        let node_kind = node.kind;
                        let node_enabled = &mut node.enabled;
                        let params = &mut node.params;
                        let keyframes = &mut node.keyframes;
                        self.show_inspector(
                            ui, node_id, &node_kind, node_enabled,
                            params, keyframes, clip_id, &mut cmds, theme, frame,
                        );
                    }
                } else {
                    ui.label(egui::RichText::new("Select a node").color(theme.text_dim));
                }
            });

        cmds
    }

    /// Simple tabbed view matching the HTML mockup design.
    pub(crate) fn show_simple(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) -> Vec<AppCommand> {
        self.show_simple_layout(ui, graph, clip_id, selected_node, color_tab, theme, frame, false)
    }

    /// Show color inspector with horizontal layout (for bottom panel — wide, short).
    pub(crate) fn show_simple_horizontal(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) -> Vec<AppCommand> {
        self.show_simple_layout(ui, graph, clip_id, selected_node, color_tab, theme, frame, true)
    }

    fn show_simple_layout(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        color_tab: &mut ColorTab,
        theme: &KlippTheme,
        frame: FrameSelection,
        wide_layout: bool,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // The InputTransform node is the input colour space, not a grade:
        // the tabs below would show Corrector sliders for it.
        let idt_selected = selected_node
            .and_then(|id| graph.nodes.iter().find(|n| n.id == id))
            .is_some_and(|n| n.kind == NodeKind::InputTransform);
        if idt_selected {
            self.show_input_colour_space(ui, clip_id, &mut cmds, theme);
            return cmds;
        }

        // ── Tab row: the same strip as the Edit inspector ──
        let tabs = [
            (ColorTab::Adjustments, "COLOUR", "COL"),
            (ColorTab::Wheels, "WHEELS", "WHL"),
            (ColorTab::Curves, "CURVES", "CRV"),
            (ColorTab::HueSat, "HUE/SAT", "HUE"),
            (ColorTab::Warper, "WARPER", "WRP"),
        ];
        let labels: Vec<&str> = tabs.iter().map(|t| t.1).collect();
        let brief: Vec<&str> = tabs.iter().map(|t| t.2).collect();
        let active = tabs.iter().position(|t| t.0 == *color_tab).unwrap_or(0);
        if let Some(i) = crate::widgets::inspector_group::tab_strip(ui, &labels, &brief, active, theme) {
            *color_tab = tabs[i].0;
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
                            Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, frame, wide_layout);
                        });
                    } else {
                        Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, frame, wide_layout);
                    }
                } else {
                    ui.set_min_width(full_w);
                    Self::render_tab_content(self, ui, graph, clip_id, selected_node, color_tab, &mut cmds, theme, frame, wide_layout);
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
        frame: FrameSelection,
        wide_layout: bool,
    ) {
        // ── Preset bar ──
        let tool_scope = match color_tab {
            ColorTab::Adjustments => Some(ToolScope::Color),
            ColorTab::Wheels => Some(ToolScope::Wheels),
            ColorTab::Curves => Some(ToolScope::Curves),
            ColorTab::HueSat => Some(ToolScope::HueSat),
            ColorTab::Warper => Some(ToolScope::Warper),
        };
        if let Some(tool) = tool_scope {
            self.show_tool_preset_bar(ui, &tool, graph, clip_id, cmds, theme);
        }

        // Contrast belongs with the wheels. It is the control a
        // colourist works against them continuously — set the lift,
        // check the contrast, adjust the gain, check it again — and
        // having it only on the COLOUR tab meant leaving the wheels to
        // reach it and coming back to see what it did. Every grading
        // suite puts contrast on the primaries bar for this reason.
        //
        // Pivot deliberately not offered: this grade model pivots
        // contrast on mid grey, 0.18 linear, as a fixed point in the
        // grade encoding. Making it adjustable is a change to the
        // colour science and its CPU/GPU parity, not a slider.
        if *color_tab == ColorTab::Wheels {
            self.show_wheels_contrast_row(ui, graph, clip_id, cmds, theme);
        }

        match color_tab {
            ColorTab::Adjustments => {
                self.show_adjustments_tab(ui, graph, clip_id, selected_node, cmds, theme, frame);
            }
            ColorTab::Wheels if wide_layout => {
                // Wide layout: combined wheels + sat/bri in unified columns
                self.show_wheels_wide(ui, graph, clip_id, cmds, theme);
            }
            _ => {
                let target_kinds: Vec<NodeKind> = match color_tab {
                    // Just the wheels. Each one now carries its own
                    // saturation and brightness in its own cell, so the
                    // standalone `Wheel Sat/Bri` section below them
                    // would be the same eight sliders a second time.
                    // The section itself is kept for graphs that really
                    // do have a legacy `WheelSatBri` node and no
                    // wheels, and for the wide layout's own path.
                    ColorTab::Wheels => vec![NodeKind::ColorWheels],
                    ColorTab::Curves => vec![NodeKind::RGBCurves],
                    ColorTab::HueSat => vec![NodeKind::HueSatCurves],
                    ColorTab::Warper => vec![NodeKind::ColorWarper],
                    _ => vec![],
                };
                // Lazy-create flow: if no grade node exists yet, render
                // the widgets against a scratch NodeParams::default()
                // using a fixed phantom NodeId. The first widget to
                // emit a change dispatches its SetNode* command tagged
                // with that phantom id; the handler in app.rs detects
                // the id, materialises a real Corrector on the clip,
                // and reroutes the write. User never sees an empty
                // state or an "Add" button — they just grade.
                let any_target_exists = graph.nodes.iter().any(|n|
                    n.kind == NodeKind::Corrector && n.enabled
                ) || target_kinds.iter().any(|k|
                    graph.nodes.iter().any(|n| n.kind == *k && n.enabled)
                );
                if !any_target_exists {
                    let mut scratch_params = klipp_nodes::NodeParams::default();
                    let mut scratch_enabled = true;
                    let mut scratch_keyframes = klipp_nodes::NodeKeyframeTracks::default();
                    for target_kind in &target_kinds {
                        let node_kind = *target_kind;
                        self.show_inspector_layout(
                            ui,
                            klipp_nodes::PHANTOM_CORRECTOR_ID,
                            &node_kind,
                            &mut scratch_enabled,
                            &mut scratch_params,
                            &mut scratch_keyframes,
                            clip_id, cmds, theme, frame, wide_layout,
                        );
                    }
                    return;
                }
                for target_kind in &target_kinds {
                    // Prefer the Corrector (new default graph). Fall
                    // back to the dedicated single-purpose node when
                    // the graph is legacy-shaped.
                    let target_idx = graph.nodes.iter().position(|n|
                        n.kind == NodeKind::Corrector && n.enabled
                    ).or_else(||
                        graph.nodes.iter().position(|n| n.kind == *target_kind && n.enabled)
                    );
                    let Some(idx) = target_idx else { continue; };
                    {
                        let node_id = graph.nodes[idx].id;
                        // The widget code below dispatches on this kind
                        // — pass the user's TAB choice, not the actual
                        // node's kind, so a Corrector-backed Wheels tab
                        // still renders the wheels widgets (which read
                        // the same fields off the Corrector's params).
                        let node_kind = *target_kind;
                        // Don't overwrite the user's canvas selection.
                        // The tabs always write to the Corrector
                        // regardless of which node is highlighted in
                        // the V2 canvas; previously we clobbered the
                        // selection to the Corrector every render,
                        // which meant clicking Source / Output / Apply
                        // Matte never looked selected because the
                        // inspector bumped the highlight back to the
                        // Corrector immediately.
                        let node = &mut graph.nodes[idx];
                        let node_enabled = &mut node.enabled;
                        let params = &mut node.params;
                        let keyframes = &mut node.keyframes;
                        self.show_inspector_layout(ui, node_id, &node_kind, node_enabled,
                            params, keyframes, clip_id, cmds, theme, frame, wide_layout);
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
        // Prefer the Corrector (it carries both wheel + sat/bri
        // fields) over the legacy split-node pair. Auto-creating here
        // would revive a just-deleted Corrector, so when neither is
        // present just show a hint that a slider touch in a narrow
        // tab will conjure one. Users drop out of wide layout for
        // that interaction, which is natural on first use.
        let corrector_present = graph.nodes.iter().any(|n| n.kind == NodeKind::Corrector);
        let legacy_present = graph.nodes.iter().any(|n|
            matches!(n.kind, NodeKind::ColorWheels | NodeKind::WheelSatBri)
        );
        if !corrector_present && !legacy_present {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(
                        "Touch any slider to start grading this clip."
                    ).small().color(theme.text_dim)
                );
            });
            return;
        }
        let corrector_idx = graph.nodes.iter().position(|n|
            n.kind == NodeKind::Corrector && n.enabled
        );
        let wheels_idx = corrector_idx.or_else(||
            graph.nodes.iter().position(|n| n.kind == NodeKind::ColorWheels && n.enabled)
        );
        let satbri_idx = corrector_idx.or_else(||
            graph.nodes.iter().position(|n| n.kind == NodeKind::WheelSatBri && n.enabled)
        );

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
                    push_atomic_node_edit(cmds, clip_id, node_id, "wheels_log_mode", 0.0);
                }
                if ui.selectable_label(log_mode, "Log (HDR)").clicked() && !log_mode {
                    graph.nodes[wi].params.wheels_log_mode = Some(true);
                    push_atomic_node_edit(cmds, clip_id, node_id, "wheels_log_mode", 1.0);
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
                    if resp.drag_started {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if resp.changed {
                        *x_field = Some(offset[0]);
                        *y_field = Some(offset[1]);
                        *m_field = Some(master);
                        push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                    }
                    if resp.drag_ended {
                        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
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
                    let accent = theme.accent;
                    let track_bg = theme.bg_raised;
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
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
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
                    if sat_resp.drag_started() {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if sat_resp.drag_stopped() {
                        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
                    }
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
                        push_atomic_node_edit(cmds, clip_id, node_id, &sat_name, 0.0);
                    }
                    let sat_t = (s + 1.0) / 2.0;
                    let sat_thumb_x = cx + sat_t * track_draw_w;
                    painter.circle_filled(egui::pos2(sat_thumb_x, sat_track_y + slider_h / 2.0), 4.0, accent);
                    let is_default = s.abs() < 0.005;
                    let val_color = if is_default { theme.text_dim } else { accent };
                    painter.text(
                        egui::pos2(cx + track_draw_w + value_w - 2.0, sat_track_y + slider_h / 2.0),
                        egui::Align2::RIGHT_CENTER,
                        format!("{s:+.2}"),
                        egui::FontId::monospace(10.0),
                        val_color,
                    );

                    // -- Brightness --
                    let bri_label_y = sat_track_y + slider_h + 4.0;
                    painter.text(
                        egui::pos2(col_rect.center().x, bri_label_y + label_h / 2.0),
                        egui::Align2::CENTER_CENTER,
                        "Brightness",
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
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
                    if bri_resp.drag_started() {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if bri_resp.drag_stopped() {
                        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
                    }
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
                        push_atomic_node_edit(cmds, clip_id, node_id, &bri_name, 0.0);
                    }
                    let bri_t = (b + 1.0) / 2.0;
                    let bri_thumb_x = cx + bri_t * track_draw_w;
                    painter.circle_filled(egui::pos2(bri_thumb_x, bri_track_y + slider_h / 2.0), 4.0, accent);
                    let is_default = b.abs() < 0.005;
                    let val_color = if is_default { theme.text_dim } else { accent };
                    painter.text(
                        egui::pos2(cx + track_draw_w + value_w - 2.0, bri_track_y + slider_h / 2.0),
                        egui::Align2::RIGHT_CENTER,
                        format!("{b:+.2}"),
                        egui::FontId::monospace(10.0),
                        val_color,
                    );
                }
            }
        });
    }

    /// Adjustments tab — matches the HTML mockup layout exactly.
    /// Render the mask-editor body: "+ Add Mask" menu, per-mask
    /// header + delete, animatable param sliders with keyframe
    /// diamonds, invert checkbox. Shared between
    /// `show_inspector_layout`'s NodeKind::Mask arm (node-canvas
    /// users), `show_adjustments_tab`'s MASKS section (color-tab
    /// users who never open the canvas), and the Edit-workspace
    /// Color inspector (clone-based invocation from
    /// `show_color_and_luts` — the mutations on the local clone are
    /// discarded; only the `SetNodeMasks` command at the end
    /// persists). On any change we push one `SetNodeMasks` so every
    /// path lands in history identically.
    pub(crate) fn render_masks_body(
        ui: &mut egui::Ui,
        masks: &mut klipp_core::MaskStack,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
        tracked_mask_indices: &[usize],
    ) {
        let clip_time_secs = frame.time_secs;
        let mut masks_changed = false;

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.menu_button(egui::RichText::new("+ Add Mask").size(11.0), |ui| {
                if ui.button("Circle").clicked() {
                    masks.masks.push(klipp_core::Mask {
                        shape: klipp_core::MaskShape::Circle {
                            center_x: 0.5, center_y: 0.5, radius: 0.2,
                        },
                        ..Default::default()
                    });
                    masks_changed = true;
                    ui.close_menu();
                }
                if ui.button("Rectangle").clicked() {
                    masks.masks.push(klipp_core::Mask {
                        shape: klipp_core::MaskShape::Rectangle {
                            center_x: 0.5, center_y: 0.5,
                            width: 0.4, height: 0.3, rotation: 0.0,
                        },
                        ..Default::default()
                    });
                    masks_changed = true;
                    ui.close_menu();
                }
                if ui.button("Linear Gradient").clicked() {
                    masks.masks.push(klipp_core::Mask {
                        shape: klipp_core::MaskShape::LinearGradient {
                            start_x: 0.2, start_y: 0.5,
                            end_x: 0.8, end_y: 0.5,
                        },
                        ..Default::default()
                    });
                    masks_changed = true;
                    ui.close_menu();
                }
                if ui.button("Bezier").clicked() {
                    masks.masks.push(klipp_core::Mask {
                        shape: klipp_core::MaskShape::Bezier {
                            points: vec![
                                (0.5, 0.35), (0.65, 0.5),
                                (0.5, 0.65), (0.35, 0.5),
                            ],
                        },
                        ..Default::default()
                    });
                    masks_changed = true;
                    ui.close_menu();
                }
            });
            if !masks.masks.is_empty() {
                ui.label(egui::RichText::new(
                    format!("({} mask{})", masks.masks.len(),
                        if masks.masks.len() == 1 { "" } else { "s" })
                ).size(10.0).color(theme.text_dim));
            }
        });

        if masks.masks.is_empty() {
            ui.label(egui::RichText::new(
                "No masks yet. Use the + Add Mask menu above."
            ).size(10.0).color(theme.text_dim));
        }
        let mask_count = masks.masks.len();
        for mask_idx in 0..mask_count {
            let shape_label = match &masks.masks[mask_idx].shape {
                klipp_core::MaskShape::Circle { .. } => "Circle",
                klipp_core::MaskShape::Rectangle { .. } => "Rectangle",
                klipp_core::MaskShape::LinearGradient { .. } => "Linear Gradient",
                klipp_core::MaskShape::Bezier { .. } => "Bezier",
            };
            let mask_enabled = masks.masks[mask_idx].enabled;
            let animatable_params: Vec<&str> = klipp_core::MaskKeyframes::params_for_shape(&masks.masks[mask_idx].shape).to_vec();

            struct ParamInfo {
                name: &'static str,
                display_val: f32,
                has_keyframes: bool,
                has_kf_at_playhead: bool,
                key_time: f64,
                range: std::ops::RangeInclusive<f32>,
            }
            let param_infos: Vec<ParamInfo> = animatable_params.iter().map(|&param_name| {
                let mask = &masks.masks[mask_idx];
                let static_val = mask.param_value(param_name).unwrap_or(0.0);
                let track = mask.keyframes.track(param_name);
                let has_keyframes = track.is_some_and(|t| !t.is_empty());
                let display_val = if has_keyframes {
                    klipp_core::keyframe::evaluate(track.unwrap(), clip_time_secs, static_val)
                } else {
                    static_val
                };
                let selected = track.and_then(|t| crate::inspector::keyframe_selection::nearest(t, clip_time_secs, frame.tolerance).map(|i| &t[i]));
                let has_kf_at_playhead = selected.is_some();
                let key_time = selected.map_or(clip_time_secs, |k| k.time_secs);
                // Like the camera row, this edits the selected key's value,
                // which can differ from the viewer's interpolated value.
                let display_val = selected.map_or(display_val, |k| k.value);
                let range = match param_name {
                    "center_x" | "center_y" | "start_x" | "start_y" | "end_x" | "end_y" => 0.0..=1.0,
                    "radius" | "width" | "height" | "feather" | "opacity" => 0.0..=1.0,
                    "rotation" => -180.0..=180.0,
                    _ => 0.0..=1.0,
                };
                ParamInfo { name: param_name, display_val, has_keyframes, has_kf_at_playhead, key_time, range }
            }).collect();

            ui.add_space(4.0);
            let mut delete_this = false;
            {
                let mask = &mut masks.masks[mask_idx];
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui.checkbox(&mut mask.enabled, "").changed() { masks_changed = true; }
                    ui.label(egui::RichText::new(format!("Mask {} - {}", mask_idx + 1, shape_label))
                        .size(11.0).color(theme.text_accent));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Push everything well off the right edge so
                        // the × delete button doesn't overlap the
                        // ScrollArea's scrollbar gutter — clicking
                        // there scrolled the panel instead of
                        // deleting the mask.
                        ui.add_space(14.0);
                        if ui.add(
                            egui::Button::new(
                                egui::RichText::new("\u{00D7}").size(13.0).color(theme.text_dim)
                            )
                            .frame(true)
                            .min_size(egui::vec2(22.0, 22.0))
                        ).on_hover_text("Delete mask").clicked() {
                            delete_this = true;
                        }
                        // One-shot mask tracking. When a tracker is
                        // already bound to this mask, the button flips
                        // to "Re-track" (overwrites prior keyframes) and
                        // an Untrack button appears next to it.
                        let is_tracked = tracked_mask_indices.contains(&mask_idx);
                        ui.add_space(6.0);
                        if is_tracked {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("\u{1F6AB} Untrack").size(10.0)
                                    .color(theme.text_dim)
                            ).frame(true))
                                .on_hover_text("Remove the tracker bound to this mask + clear its centre keyframes")
                                .clicked()
                            {
                                cmds.push(AppCommand::RemoveMaskTracking {
                                    clip_id, node_id, mask_index: mask_idx,
                                });
                            }
                            ui.add_space(4.0);
                        }
                        let label = if is_tracked { "\u{1F3AF} Re-track" } else { "\u{1F3AF} Track" };
                        let hover = if is_tracked {
                            "Re-run tracking from the playhead — overwrites the existing centre keyframes"
                        } else {
                            "Track this region across the clip — auto-keyframes the mask centre"
                        };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(label).size(10.0)
                        ).frame(true))
                            .on_hover_text(hover)
                            .clicked()
                        {
                            // Re-track: drop the existing tracker first
                            // so we don't leave duplicates piling up.
                            if is_tracked {
                                cmds.push(AppCommand::RemoveMaskTracking {
                                    clip_id, node_id, mask_index: mask_idx,
                                });
                            }
                            cmds.push(AppCommand::TrackMaskRegion {
                                clip_id, node_id, mask_index: mask_idx,
                            });
                        }
                    });
                });
            }
            if delete_this {
                // Route through the AppCommand handler so the
                // centralized cleanup runs — it drops any TrackPoint
                // bound to this (mask_node_id, mask_index) and
                // slides remaining tracker bindings down to match
                // the post-removal indices. The previous in-place
                // `masks.masks.remove(mask_idx)` here bypassed all
                // of that, leaving orphaned trackers attached to a
                // mask that no longer existed.
                cmds.push(AppCommand::RemoveMaskShape { clip_id, mask_index: mask_idx });
                break;
            }
            if !mask_enabled { continue; }

            for info in &param_infos {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
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
                                param: info.name.to_string(), time_secs: info.key_time,
                            });
                        } else {
                            cmds.push(AppCommand::AddMaskKeyframe {
                                clip_id, node_id, mask_index: mask_idx,
                                param: info.name.to_string(), time_secs: info.key_time,
                                value: info.display_val,
                            });
                        }
                    }
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
                    ui.label(egui::RichText::new(info.name.replace('_', " "))
                        .size(10.0).color(theme.text_dim));
                    ui.add_space(4.0);
                    let mut val = info.display_val;
                    let resp = ui.add(egui::Slider::new(&mut val, info.range.clone())
                        .show_value(true)
                        .custom_formatter(|v, _| format!("{:.3}", v))
                        .min_decimals(3)
                    );
                    if resp.changed() {
                        if info.has_keyframes {
                            cmds.push(AppCommand::AddMaskKeyframe {
                                clip_id, node_id, mask_index: mask_idx,
                                param: info.name.to_string(), time_secs: info.key_time,
                                value: val,
                            });
                        } else {
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

    /// Contrast, and a reset for every wheel, sitting above the wheels
    /// themselves.
    ///
    /// Contrast is worked against the wheels continuously, so reaching
    /// it meant leaving the tab. Reset-all exists because the wheels
    /// each reset individually (double-click or right-click) and
    /// starting the primaries over meant eight separate gestures.
    fn show_wheels_contrast_row(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Same target the wheels themselves write to: the Corrector when
        // there is one, else a standalone node of the right kind.
        let target = graph
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Corrector)
            .map(|n| n.id)
            .or_else(|| graph.nodes.iter().find(|n| n.kind == NodeKind::Contrast).map(|n| n.id));
        let Some(nid) = target else { return };

        ui.add_space(4.0);
        if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
            let mut val = node.params.contrast.unwrap_or(0.0);
            if inspector_row(ui, "Contrast", &mut val, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                node.params.contrast = Some(val);
                cmds.push(AppCommand::SetNodeParam {
                    clip_id, node_id: nid, param: "contrast".into(), value: val,
                });
            }
        }

        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                let resp = ui.add(
                    egui::Button::new(
                        egui::RichText::new("Reset wheels").size(10.0).color(theme.text_dim),
                    )
                    .frame(true),
                );
                if resp.on_hover_text("Lift, Gamma, Gain and Offset back to neutral").clicked() {
                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                        let p = &mut node.params;
                        p.lift_x = Some(0.0); p.lift_y = Some(0.0); p.lift_master = Some(0.0);
                        p.gamma_x = Some(0.0); p.gamma_y = Some(0.0); p.gamma_master = Some(0.0);
                        p.gain_x = Some(0.0); p.gain_y = Some(0.0); p.gain_master = Some(0.0);
                        p.offset_x = Some(0.0); p.offset_y = Some(0.0); p.offset_master = Some(0.0);
                    }
                    // One undo step for the lot, like every other
                    // multi-parameter reset in this panel.
                    atomic_node_edit_batch(cmds, clip_id, |c| {
                        for param in [
                            "lift_x", "lift_y", "lift_master",
                            "gamma_x", "gamma_y", "gamma_master",
                            "gain_x", "gain_y", "gain_master",
                            "offset_x", "offset_y", "offset_master",
                        ] {
                            c.push(AppCommand::SetNodeParam {
                                clip_id, node_id: nid, param: param.into(), value: 0.0,
                            });
                        }
                    });
                }
            });
        });
        ui.add_space(4.0);
        section_divider(ui, theme);
    }

    fn show_adjustments_tab(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        selected_node: &mut Option<NodeId>,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) {
        let clip_time_secs = frame.time_secs;
        // When no grade node exists on the clip, the sections below
        // naturally skip (find_node returns None for every kind).
        // Show a one-line hint pointing at the tabs that DO support
        // lazy-create so the user isn't staring at a blank panel.
        let has_grade_target = graph.nodes.iter().any(|n| matches!(n.kind,
            NodeKind::Corrector | NodeKind::Exposure | NodeKind::Contrast
            | NodeKind::Saturation | NodeKind::ToneControls | NodeKind::ColorBalance
        ));
        if !has_grade_target {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(
                        "Touch any slider in Wheels, Curves, Hue-Sat or Warper to start grading."
                    ).small().color(theme.text_dim)
                );
            });
            return;
        }

        // Helper: pick the node each section's sliders should write
        // to. Priority:
        //   1. The user-selected node if it matches `kind` — when the
        //      user explicitly selected a standalone Contrast /
        //      Exposure / Saturation / etc. node in the canvas, the
        //      tabs must target THAT node so the selected-node
        //      feedback loop works (this matters for RGB-split
        //      chains where a grade node lives on one channel and
        //      must grade only that channel, not the upstream
        //      Corrector).
        //   2. Otherwise, the Corrector — every section-relevant
        //      param lives on it, so the legacy "all tabs on one
        //      canonical node" workflow keeps working.
        //   3. Otherwise, the first node of the requested `kind`.
        let selected_id = *selected_node;
        let find_node = |graph: &mut NodeGraph, kind: NodeKind| -> Option<(NodeId, bool)> {
            if let Some(sid) = selected_id {
                if let Some(n) = graph.nodes.iter().find(|n| n.id == sid && n.kind == kind) {
                    return Some((n.id, n.enabled));
                }
            }
            graph.nodes.iter()
                .find(|n| n.kind == NodeKind::Corrector)
                .map(|n| (n.id, n.enabled))
                .or_else(||
                    graph.nodes.iter().find(|n| n.kind == kind).map(|n| (n.id, n.enabled))
                )
        };

        // Node-bypass strip: targets the user-selected node if any,
        // else the Corrector. One diamond per inspector view — the
        // other sections below are just slider groupings against the
        // same underlying node, so a single bypass control is enough.
        let bypass_target = selected_id
            .and_then(|id| graph.nodes.iter().find(|n| n.id == id).map(|n| (n.id, n.label.clone(), n.enabled)))
            .or_else(|| graph.nodes.iter()
                .find(|n| n.kind == NodeKind::Corrector)
                .map(|n| (n.id, n.label.clone(), n.enabled)));
        if let Some((target_id, target_label, target_enabled)) = bypass_target {
            let (track_has_kfs, kf_at_playhead, effective_on, key_time) = {
                let node = graph.nodes.iter().find(|n| n.id == target_id);
                let track = node.and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == klipp_nodes::ENABLED_TRACK));
                let has_kfs = track.is_some_and(|t| !t.keyframes.is_empty());
                let selected = track.and_then(|t| crate::inspector::keyframe_selection::nearest(&t.keyframes, clip_time_secs, frame.tolerance).map(|i| &t.keyframes[i]));
                let at_ph = selected.is_some();
                let on = selected.map(|k| k.value).or_else(|| track.and_then(|t| t.evaluate(clip_time_secs)))
                    .map(|v| v >= 0.5)
                    .unwrap_or(target_enabled);
                (has_kfs, at_ph, on, selected.map_or(clip_time_secs, |k| k.time_secs))
            };
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let mut shown = effective_on;
                if ui.checkbox(&mut shown, "").changed() {
                    if track_has_kfs {
                        cmds.push(AppCommand::AddKeyframe {
                            clip_id, node_id: target_id,
                            param: klipp_nodes::ENABLED_TRACK.to_string(),
                            time_secs: key_time,
                            value: if shown { 1.0 } else { 0.0 },
                        });
                    } else {
                        cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id: target_id });
                    }
                }
                // The checkbox is bound to `effective_on` — ticked means
                // the grade is *running*. Labelling that "Bypass" said
                // the opposite of what the tick meant, on the one
                // control that answers "am I looking at the graded
                // picture or not". Getting that backwards is how a
                // colourist spends ten minutes grading a shot that was
                // bypassed the whole time.
                //
                // So the label states the state rather than naming a
                // mode, and the dangerous state is the loud one: an
                // active grade reads quietly, a bypassed one says so in
                // the warning colour.
                if shown {
                    ui.label(egui::RichText::new(target_label.to_string())
                        .size(11.0).color(theme.text_dim));
                } else {
                    ui.label(egui::RichText::new(format!("BYPASSED — {target_label}"))
                        .size(11.0).strong().color(theme.marker_warning));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    let diamond_color = if kf_at_playhead {
                        theme.text_accent
                    } else if track_has_kfs {
                        egui::Color32::from_rgba_premultiplied(255, 191, 0, 128)
                    } else {
                        theme.text_dim
                    };
                    let diamond_text = if kf_at_playhead { "\u{25C6}" } else { "\u{25C7}" };
                    let diamond_resp = ui.add(
                        egui::Button::new(egui::RichText::new(diamond_text).size(13.0).color(diamond_color))
                            .frame(false),
                    );
                    if diamond_resp.clicked() {
                        if kf_at_playhead {
                            cmds.push(AppCommand::RemoveKeyframe {
                                clip_id, node_id: target_id,
                                param: klipp_nodes::ENABLED_TRACK.to_string(),
                                time_secs: key_time,
                            });
                        } else {
                            cmds.push(AppCommand::AddKeyframe {
                                clip_id, node_id: target_id,
                                param: klipp_nodes::ENABLED_TRACK.to_string(),
                                time_secs: key_time,
                                value: if effective_on { 1.0 } else { 0.0 },
                            });
                        }
                    }
                    if track_has_kfs {
                        diamond_resp.context_menu(|ui| {
                            if ui.button("Clear bypass keyframes").clicked() {
                                cmds.push(AppCommand::ClearKeyframes {
                                    clip_id, node_id: target_id,
                                    param: klipp_nodes::ENABLED_TRACK.to_string(),
                                });
                                ui.close_menu();
                            }
                        });
                    }
                    diamond_resp.on_hover_text(if kf_at_playhead {
                        "Remove bypass keyframe at playhead"
                    } else {
                        "Add bypass keyframe at playhead"
                    });
                });
            });
            ui.add_space(4.0);
            ui.separator();
        }

        // ── Exposure section ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::Exposure) {
            // Intentionally NOT clobbering `*selected_node` here — see
            // the matching comment in `show_simple_layout`. Clobbering
            // it made clicking Source/Output/Apply-Matte in the V2
            // canvas look like selection never happened because the
            // inspector bumped the highlight back to the grade node
            // every frame.
            let (reset, is_open) = section_header(ui, "EXPOSURE", enabled, theme);
            if is_open {
                if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                    if reset {
                        node.params.exposure = Some(0.0);
                        atomic_node_edit_batch(cmds, clip_id, |c| {
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "exposure".into(), value: 0.0 });
                        });
                    }
                    let mut val = node.params.exposure.unwrap_or(0.0);
                    if inspector_row(ui, "Exposure", &mut val, -3.0..=3.0, 0.0, "stops", theme, cmds, clip_id) {
                        node.params.exposure = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "exposure".into(), value: val });
                    }
                }
                section_divider(ui, theme);
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
                        atomic_node_edit_batch(cmds, clip_id, |c| {
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "contrast".into(), value: 0.0 });
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "clarity".into(), value: 0.0 });
                        });
                    }
                    let mut val = node.params.contrast.unwrap_or(0.0);
                    if inspector_row(ui, "Contrast", &mut val, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                        node.params.contrast = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "contrast".into(), value: val });
                    }
                    // Where the contrast hinges: below it a push
                    // darkens, above it a push brightens, so the pivot
                    // decides whether contrast is spent on the shadows
                    // or the highlights. Mid grey, 0.18, is where it
                    // always was and is the default.
                    //
                    // In linear working-space units rather than an
                    // abstract 0..1, because 0.18 is the number a
                    // colourist already carries for mid grey and the
                    // rest of the codebase names it that.
                    // Where the contrast hinges: below it a push
                    // darkens, above it a push brightens, so the pivot
                    // decides whether contrast is spent on the shadows
                    // or the highlights. Mid grey, 0.18 linear, is where
                    // it always was and is the default.
                    //
                    // Stored linear — anchored to the physical value, so
                    // a project keeps meaning the same thing if the
                    // grade encoding is ever retuned — but scrubbed in
                    // that encoding, which is roughly perceptual. On a
                    // linear scale 0.18 sits 8 %% along and the whole
                    // useful range is crushed into the left end; in the
                    // encoding it lands near the middle and equal travel
                    // is an equal change, which is why Resolve's pivot
                    // reads about 0.43 rather than 0.18.
                    let mut shown = klipp_color::grade_model::grade_encode(
                        node.params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                    );
                    if inspector_row(
                        ui, "Pivot", &mut shown,
                        klipp_color::grade_model::grade_encode(klipp_color::grade_model::MIN_CONTRAST_PIVOT)
                            ..=klipp_color::grade_model::grade_encode(klipp_color::grade_model::MAX_CONTRAST_PIVOT),
                        klipp_color::grade_model::grade_encode(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                        "", theme, cmds, clip_id,
                    ) {
                        let linear = klipp_color::grade_model::grade_decode(shown).clamp(
                            klipp_color::grade_model::MIN_CONTRAST_PIVOT,
                            klipp_color::grade_model::MAX_CONTRAST_PIVOT,
                        );
                        node.params.contrast_pivot = Some(linear);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "contrast_pivot".into(), value: linear });
                    }
                    let mut cval = node.params.clarity.unwrap_or(0.0);
                    if inspector_row(ui, "Clarity", &mut cval, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                        node.params.clarity = Some(cval);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "clarity".into(), value: cval });
                    }
                }
                section_divider(ui, theme);
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
                        atomic_node_edit_batch(cmds, clip_id, |c| {
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "saturation".into(), value: 1.0 });
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "vibrance".into(), value: 0.0 });
                        });
                    }
                    let mut val = node.params.saturation.unwrap_or(1.0);
                    // Saturation is a multiplier: 1.0 is neutral, unlike
                    // every other row in this panel where neutral is 0.
                    // Rendered in the same widget with no unit it read as
                    // an inconsistency — "1.00" sitting under "0.00" with
                    // nothing to say why. The multiplier sign says why.
                    if inspector_row(ui, "Saturation", &mut val, 0.0..=2.0, 1.0, "\u{00D7}", theme, cmds, clip_id) {
                        node.params.saturation = Some(val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "saturation".into(), value: val });
                    }
                    let mut vval = node.params.vibrance.unwrap_or(0.0);
                    if inspector_row(ui, "Vibrance", &mut vval, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                        node.params.vibrance = Some(vval);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "vibrance".into(), value: vval });
                    }
                }
                section_divider(ui, theme);
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
                        }
                        atomic_node_edit_batch(cmds, clip_id, |c| {
                            for p in ["highlights", "shadows", "whites", "blacks"] {
                                c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: p.into(), value: 0.0 });
                            }
                        });
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
                        if inspector_row(ui, label, &mut val, -1.0..=1.0, default, "", theme, cmds, clip_id) {
                            node.params.apply_override(param_name, val);
                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: param_name.into(), value: val });
                        }
                    }
                }
                section_divider(ui, theme);
            }
        }

        // ── Color Balance section — with gradient sliders ──
        if let Some((nid, enabled)) = find_node(graph, NodeKind::ColorBalance) {
            let (reset, is_open) = section_header(ui, "COLOUR BALANCE", enabled, theme);
            if is_open { if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == nid) {
                if reset {
                    for (p, d) in [("temperature", 0.0), ("tint", 0.0), ("hue", 0.0), ("global_mix", 1.0)] {
                        node.params.apply_override(p, d);
                    }
                    atomic_node_edit_batch(cmds, clip_id, |c| {
                        for (p, d) in [("temperature", 0.0), ("tint", 0.0), ("hue", 0.0), ("global_mix", 1.0)] {
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: p.into(), value: d });
                        }
                    });
                }
                // Temperature: blue → orange gradient
                let mut temp = node.params.temperature.unwrap_or(0.0);
                if gradient_slider_row(ui, "Temperature", &mut temp, -50.0..=50.0, 0.0,
                    egui::Color32::from_rgb(0x6a, 0xb4, 0xf5),
                    egui::Color32::from_rgb(0xf5, 0xa0, 0x5a), theme, cmds, clip_id)
                {
                    node.params.temperature = Some(temp);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "temperature".into(), value: temp });
                }

                // Tint: green → pink gradient
                let mut tint = node.params.tint.unwrap_or(0.0);
                if gradient_slider_row(ui, "Tint", &mut tint, -50.0..=50.0, 0.0,
                    egui::Color32::from_rgb(0xa8, 0xf5, 0xa8),
                    egui::Color32::from_rgb(0xf5, 0xa8, 0xc8), theme, cmds, clip_id)
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
                if hue_slider_row(ui, "Hue", &mut hue, theme, cmds, clip_id) {
                    node.params.hue = Some(hue);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "hue".into(), value: hue });
                }

                // Global Mix: subtle grey → white
                let mut mix = node.params.global_mix.unwrap_or(1.0);
                if gradient_slider_row(ui, "Global Mix", &mut mix, 0.0..=1.0, 1.0,
                    theme.overlay(20),
                    theme.overlay(90), theme, cmds, clip_id)
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
                        atomic_node_edit_batch(cmds, clip_id, |c| {
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_luma".into(), value: 0.0 });
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_chroma".into(), value: 0.0 });
                            c.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_radius".into(), value: 3.0 });
                        });
                    }
                    let mut luma = node.params.denoise_luma.unwrap_or(0.0);
                    if inspector_row(ui, "Luma", &mut luma, 0.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                        node.params.denoise_luma = Some(luma);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_luma".into(), value: luma });
                    }
                    let mut chroma = node.params.denoise_chroma.unwrap_or(0.0);
                    if inspector_row(ui, "Chroma", &mut chroma, 0.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                        node.params.denoise_chroma = Some(chroma);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_chroma".into(), value: chroma });
                    }
                    let mut radius = node.params.denoise_radius.unwrap_or(3) as f32;
                    if inspector_row(ui, "Radius", &mut radius, 1.0..=7.0, 3.0, "px", theme, cmds, clip_id) {
                        let r = radius.round() as u32;
                        node.params.denoise_radius = Some(r);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id: nid, param: "denoise_radius".into(), value: r as f32 });
                    }
                }
                section_divider(ui, theme);
            }
        }

        // ── Masks section ──
        // Exposed here so Color-workspace users can create + manage
        // masks without opening the node canvas. Two paths:
        //   - Mask node exists: route directly to render_masks_body
        //     so the full editor (shape params, keyframes, feather,
        //     opacity, invert) is available inline. Same code path
        //     as the per-node inspector.
        //   - No Mask node yet: bootstrap menu dispatching
        //     AddMaskShape, which lazy-creates the Mask node AND
        //     appends the shape in one history step.
        let (_, is_open) = section_header(ui, "MASKS", true, theme);
        if is_open {
            ui.add_space(4.0);
            let mask_node_idx = graph.nodes.iter().position(|n| n.kind == NodeKind::Mask);
            if let Some(idx) = mask_node_idx {
                let node_id = graph.nodes[idx].id;
                let masks = graph.nodes[idx].params.masks.get_or_insert_with(Default::default);
                Self::render_masks_body(ui, masks, node_id, clip_id, cmds, theme, frame, &[]);
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.menu_button(egui::RichText::new("+ Add Mask").size(11.0), |ui| {
                        if ui.button("Circle").clicked() {
                            cmds.push(AppCommand::AddMaskShape {
                                clip_id,
                                shape: klipp_core::MaskShape::Circle {
                                    center_x: 0.5, center_y: 0.5, radius: 0.2,
                                },
                            });
                            ui.close_menu();
                        }
                        if ui.button("Rectangle").clicked() {
                            cmds.push(AppCommand::AddMaskShape {
                                clip_id,
                                shape: klipp_core::MaskShape::Rectangle {
                                    center_x: 0.5, center_y: 0.5,
                                    width: 0.4, height: 0.3, rotation: 0.0,
                                },
                            });
                            ui.close_menu();
                        }
                        if ui.button("Linear Gradient").clicked() {
                            cmds.push(AppCommand::AddMaskShape {
                                clip_id,
                                shape: klipp_core::MaskShape::LinearGradient {
                                    start_x: 0.2, start_y: 0.5,
                                    end_x: 0.8, end_y: 0.5,
                                },
                            });
                            ui.close_menu();
                        }
                        if ui.button("Bezier").clicked() {
                            cmds.push(AppCommand::AddMaskShape {
                                clip_id,
                                shape: klipp_core::MaskShape::Bezier {
                                    points: vec![
                                        (0.5, 0.35), (0.65, 0.5),
                                        (0.5, 0.65), (0.35, 0.5),
                                    ],
                                },
                            });
                            ui.close_menu();
                        }
                    });
                    ui.label(egui::RichText::new(
                        "No masks yet — pick a shape to start."
                    ).size(10.0).color(theme.text_dim));
                });
            }
            section_divider(ui, theme);
        }
    }

    // ── Inspector ───────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    /// The InputTransform node's editor: the clip's input colour space.
    fn show_input_colour_space(&self, ui: &mut egui::Ui, clip_id: ClipId, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
        let (current, detected) = self.input_colour_space;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("INPUT COLOUR SPACE").size(10.0).color(theme.text_dim));
        });
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            crate::inspector::color_and_luts::input_colour_space_combo(
                ui, "node_idt_log_format", clip_id, current, detected, cmds,
            );
        });
        egui::Frame::none()
            .inner_margin(egui::Margin { left: 12.0, right: 12.0, top: 4.0, bottom: 0.0 })
            .show(ui, |ui| {
                ui.label(egui::RichText::new("The encoding the file is converted from. Applies to every clip of this file.")
                    .size(10.0).color(theme.text_dim));
            });
    }

    fn show_inspector(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        kind: &NodeKind,
        enabled: &mut bool,
        params: &mut NodeParams,
        keyframes: &mut klipp_nodes::NodeKeyframeTracks,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) {
        self.show_inspector_layout(ui, node_id, kind, enabled, params, keyframes, clip_id, cmds, theme, frame, false)
    }

    fn show_inspector_layout(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        kind: &NodeKind,
        enabled: &mut bool,
        params: &mut NodeParams,
        keyframes: &mut klipp_nodes::NodeKeyframeTracks,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
        wide_layout: bool,
    ) {
        let clip_time_secs = frame.time_secs;
        let enabled_track = keyframes.tracks.iter().find(|t| t.param_name == klipp_nodes::ENABLED_TRACK);
        let bypass_track_has_kfs = enabled_track.is_some_and(|t| !t.keyframes.is_empty());
        let selected = enabled_track.and_then(|t| crate::inspector::keyframe_selection::nearest(&t.keyframes, clip_time_secs, frame.tolerance).map(|i| &t.keyframes[i]));
        let key_time = selected.map_or(clip_time_secs, |k| k.time_secs);
        let bypass_kf_at_playhead = selected.is_some();
        let effective_on = selected.map(|k| k.value).or_else(|| enabled_track.and_then(|t| t.evaluate(clip_time_secs)))
            .map(|v| v >= 0.5)
            .unwrap_or(*enabled);

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(kind.display_name())
                .size(12.0).color(theme.text_accent).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                let mut shown = effective_on;
                if ui.checkbox(&mut shown, "").changed() {
                    if bypass_track_has_kfs {
                        cmds.push(AppCommand::AddKeyframe {
                            clip_id, node_id,
                            param: klipp_nodes::ENABLED_TRACK.to_string(),
                            time_secs: key_time,
                            value: if shown { 1.0 } else { 0.0 },
                        });
                    } else {
                        cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
                    }
                }
                ui.add_space(4.0);
                let diamond_color = if bypass_kf_at_playhead {
                    theme.text_accent
                } else if bypass_track_has_kfs {
                    egui::Color32::from_rgba_premultiplied(255, 191, 0, 128)
                } else {
                    theme.text_dim
                };
                let diamond_text = if bypass_kf_at_playhead { "\u{25C6}" } else { "\u{25C7}" };
                let diamond_resp = ui.add(
                    egui::Button::new(egui::RichText::new(diamond_text).size(12.0).color(diamond_color))
                        .frame(false),
                );
                if diamond_resp.clicked() {
                    if bypass_kf_at_playhead {
                        cmds.push(AppCommand::RemoveKeyframe {
                            clip_id, node_id,
                            param: klipp_nodes::ENABLED_TRACK.to_string(),
                            time_secs: key_time,
                        });
                    } else {
                        cmds.push(AppCommand::AddKeyframe {
                            clip_id, node_id,
                            param: klipp_nodes::ENABLED_TRACK.to_string(),
                            time_secs: key_time,
                            value: if effective_on { 1.0 } else { 0.0 },
                        });
                    }
                }
                if bypass_track_has_kfs {
                    diamond_resp.context_menu(|ui| {
                        if ui.button("Clear bypass keyframes").clicked() {
                            cmds.push(AppCommand::ClearKeyframes {
                                clip_id, node_id,
                                param: klipp_nodes::ENABLED_TRACK.to_string(),
                            });
                            ui.close_menu();
                        }
                    });
                }
                diamond_resp.on_hover_text(if bypass_kf_at_playhead {
                    "Remove bypass keyframe at playhead"
                } else {
                    "Add bypass keyframe at playhead"
                });
            });
        });
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(4.0);

        match kind {
            NodeKind::Exposure => {
                let mut val = params.exposure.unwrap_or(0.0);
                if grade_slider(ui, "Exposure", &mut val, -3.0..=3.0, "stops", cmds, clip_id, theme) {
                    params.exposure = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "exposure".into(), value: val });
                }
            }
            NodeKind::Contrast => {
                let mut val = params.contrast.unwrap_or(0.0);
                if grade_slider(ui, "Contrast", &mut val, -1.0..=1.0, "", cmds, clip_id, theme) {
                    params.contrast = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "contrast".into(), value: val });
                }
                // Where the contrast hinges. Below it a push darkens,
                // above it a push brightens, so moving the pivot is how
                // you decide whether contrast is spent on the shadows
                // or the highlights. Mid grey, 0.18, is where it always
                // was, and is the default.
                //
                // In linear working-space units rather than a 0..1
                // control, because 0.18 is the number a colourist
                // already has in their head for mid grey and the whole
                // codebase names it that.
                // Same control as the Colour panel's: stored linear,
                // scrubbed in the grade encoding. See the note there.
                let mut shown = klipp_color::grade_model::grade_encode(
                    params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                );
                if grade_slider(
                    ui, "Pivot", &mut shown,
                    klipp_color::grade_model::grade_encode(klipp_color::grade_model::MIN_CONTRAST_PIVOT)
                        ..=klipp_color::grade_model::grade_encode(klipp_color::grade_model::MAX_CONTRAST_PIVOT),
                    "", cmds, clip_id, theme,
                ) {
                    let linear = klipp_color::grade_model::grade_decode(shown).clamp(
                        klipp_color::grade_model::MIN_CONTRAST_PIVOT,
                        klipp_color::grade_model::MAX_CONTRAST_PIVOT,
                    );
                    params.contrast_pivot = Some(linear);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "contrast_pivot".into(), value: linear });
                }
                let mut cval = params.clarity.unwrap_or(0.0);
                if grade_slider(ui, "Clarity", &mut cval, -1.0..=1.0, "", cmds, clip_id, theme) {
                    params.clarity = Some(cval);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "clarity".into(), value: cval });
                }
            }
            NodeKind::Saturation => {
                let mut val = params.saturation.unwrap_or(1.0);
                if grade_slider(ui, "Saturation", &mut val, 0.0..=2.0, "", cmds, clip_id, theme) {
                    params.saturation = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: val });
                }
                let mut vval = params.vibrance.unwrap_or(0.0);
                if grade_slider(ui, "Vibrance", &mut vval, -1.0..=1.0, "", cmds, clip_id, theme) {
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
                    if grade_slider(ui, label, &mut val, -1.0..=1.0, "", cmds, clip_id, theme) {
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
                        push_atomic_node_edit(cmds, clip_id, node_id, "wheels_log_mode", 0.0);
                    }
                    if ui.selectable_label(log_sel, "Log (HDR)").clicked() && !log_sel {
                        params.wheels_log_mode = Some(true);
                        push_atomic_node_edit(cmds, clip_id, node_id, "wheels_log_mode", 1.0);
                    }
                });
                ui.add_space(4.0);

                // Helper closure for each wheel, and — where the wheel
                // has them — its own saturation and brightness beneath
                // it, inside the same cell.
                //
                // They used to live in a separate `Wheel Sat/Bri`
                // section below all four wheels, four labelled blocks
                // of two sliders. They worked, but which slider went
                // with which wheel was something you read rather than
                // something you saw, and adjusting Lift's saturation
                // meant scrolling past every wheel to find the word
                // LIFT.
                //
                // Putting each wheel's pair in four columns of their
                // own — what the issue proposed — was measured and
                // rejected: at the default 300 px panel that is a 59 px
                // disc, against 110 px now. Half the travel to place a
                // puck with is not a grading control. The wheels keep
                // their 2x2 grid and the sliders come to them instead,
                // which is what the wide layout has always done.
                //
                // Log mode's Low/Mid/High have no per-wheel sat/bri
                // fields, so they pass `None` and are unchanged.
                // Two 14 px slider rows, their spacing, and clearance
                // beneath so the next row of wheels does not collide
                // with the B slider above it.
                let satbri_h = 52.0;
                let mut show_wheel = |ui: &mut egui::Ui, label: &str,
                    state: &mut ColorWheelState,
                    x: &mut Option<f32>, y: &mut Option<f32>, m: &mut Option<f32>,
                    satbri: Option<(&mut Option<f32>, &mut Option<f32>)>,
                    prefix: &str|
                {
                    let mut offset = [x.unwrap_or(0.0), y.unwrap_or(0.0)];
                    let mut master = m.unwrap_or(0.0);
                    let resp = color_wheel::color_wheel_sized(
                        ui, state, theme, label, &mut offset, &mut master, wheel_d, None,
                    );
                    if resp.drag_started {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if resp.changed {
                        *x = Some(offset[0]); *y = Some(offset[1]); *m = Some(master);
                        push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                    }
                    if resp.drag_ended {
                        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
                    }

                    let Some((sat, bri)) = satbri else { return };
                    // Stacked under the wheel, in the wheel's own width.
                    //
                    // The cell inherits its layout from the
                    // `horizontal_wrapped` row the wheels sit in, so
                    // these have to open a vertical scope of their own
                    // — without it they lay out *beside* the wheel,
                    // overflow the cell, and push the next wheel onto
                    // its own row.
                    ui.vertical(|ui| {
                        ui.set_max_width(wheel_d);
                        ui.spacing_mut().item_spacing = egui::vec2(3.0, 2.0);
                        // One letter each: the cell is only as wide as
                        // the wheel, and the wheel's own title directly
                        // above already says which one this is. S and B
                        // are the wide layout's labels, so the two
                        // layouts agree.
                        // Every part of the row is given an exact width,
                        // so letter + slider + value always add up to
                        // the wheel's own width. Left to size itself the
                        // value label pushed the row wider than the cell
                        // and sat on the slider's right end.
                        const LETTER_W: f32 = 10.0;
                        const VALUE_W: f32 = 34.0;
                        let slider_w = (wheel_d - LETTER_W - VALUE_W - 12.0).max(28.0);
                        // `Slider` takes its length from `slider_width`,
                        // not from the rect `add_sized` gives it — left
                        // alone it draws the full available width and
                        // pushes the value label out past the cell and
                        // onto the next wheel's column.
                        ui.spacing_mut().slider_width = slider_w;
                        for (letter, field, suffix, hover) in [
                            ("S", sat, "sat", "Saturation"),
                            ("B", bri, "bri", "Brightness"),
                        ] {
                            let mut v = field.unwrap_or(0.0);
                            let r = ui.horizontal(|ui| {
                                ui.add_sized(
                                    [LETTER_W, 14.0],
                                    egui::Label::new(
                                        egui::RichText::new(letter).size(10.0).color(theme.text_dim),
                                    ),
                                )
                                .on_hover_text(hover);
                                let r = ui.add_sized(
                                    [slider_w, 14.0],
                                    egui::Slider::new(&mut v, -1.0..=1.0).show_value(false),
                                );
                                ui.add_sized(
                                    [VALUE_W, 14.0],
                                    egui::Label::new(
                                        egui::RichText::new(format!("{v:+.2}"))
                                            .size(10.0)
                                            .color(theme.text_dim),
                                    ),
                                );
                                r
                            }).inner;
                            if r.drag_started() {
                                cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                            }
                            if r.changed() {
                                *field = Some(v);
                                let param = format!("{prefix}_{suffix}");
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param, value: v });
                            }
                            if r.drag_stopped() {
                                cmds.push(AppCommand::EndNodeParamEdit { clip_id });
                            }
                        }
                    });
                };

                if log_mode {
                    // Log wheels: Low / Mid / High (3 wheels for HDR)
                    if two_col {
                        let pair_w = (wheel_d + 10.0) * 2.0;
                        let center_pad = ((avail_w - pair_w) / 2.0).max(0.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Low", &mut self.log_low_wheel,
                                    &mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master, None, "log_low");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Mid", &mut self.log_mid_wheel,
                                    &mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master, None, "log_mid");
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "High", &mut self.log_high_wheel,
                                    &mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master, None, "log_high");
                            }
                        });
                    } else {
                        show_wheel(ui, "Low", &mut self.log_low_wheel,
                            &mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master, None, "log_low");
                        show_wheel(ui, "Mid", &mut self.log_mid_wheel,
                            &mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master, None, "log_mid");
                        show_wheel(ui, "High", &mut self.log_high_wheel,
                            &mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master, None, "log_high");
                    }
                } else {
                    // Linear wheels: Lift / Gamma / Gain / Offset (traditional)
                    if four_col {
                        // Wide layout: all 4 wheels in one horizontal row, left-aligned
                        let cell_w = wheel_d + 10.0;
                        ui.horizontal_wrapped(|ui| {
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(cell_w, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Lift", &mut self.lift_wheel,
                                    &mut params.lift_x, &mut params.lift_y, &mut params.lift_master,
                                    Some((&mut params.lift_sat, &mut params.lift_bri)), "lift");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(cell_w, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Gamma", &mut self.gamma_wheel,
                                    &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master,
                                    Some((&mut params.gamma_sat, &mut params.gamma_bri)), "gamma");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(cell_w, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Gain", &mut self.gain_wheel,
                                    &mut params.gain_x, &mut params.gain_y, &mut params.gain_master,
                                    Some((&mut params.gain_sat, &mut params.gain_bri)), "gain");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(cell_w, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Offset", &mut self.offset_wheel,
                                    &mut params.offset_x, &mut params.offset_y, &mut params.offset_master,
                                    Some((&mut params.offset_sat, &mut params.offset_bri)), "offset");
                            }
                        });
                    } else if two_col {
                        let pair_w = (wheel_d + 10.0) * 2.0;
                        let center_pad = ((avail_w - pair_w) / 2.0).max(0.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Lift", &mut self.lift_wheel,
                                    &mut params.lift_x, &mut params.lift_y, &mut params.lift_master,
                                    Some((&mut params.lift_sat, &mut params.lift_bri)), "lift");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Gamma", &mut self.gamma_wheel,
                                    &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master,
                                    Some((&mut params.gamma_sat, &mut params.gamma_bri)), "gamma");
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(center_pad);
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Gain", &mut self.gain_wheel,
                                    &mut params.gain_x, &mut params.gain_y, &mut params.gain_master,
                                    Some((&mut params.gain_sat, &mut params.gain_bri)), "gain");
                            }
                            {
                                // A cell of its own, laid out top-down and centred:
                                // inherited from the wrapping row it would be a
                                // horizontal Ui, and the wheel's sliders would land
                                // beside the wheel instead of under it. Same shape
                                // the wide layout builds its columns with.
                                let cell = ui.allocate_exact_size(
                                    egui::vec2(wheel_d + 10.0, wheel_d + 50.0 + satbri_h),
                                    egui::Sense::hover(),
                                ).0;
                                let mut cell_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(egui::Layout::top_down(egui::Align::Center)),
                                );
                                show_wheel(&mut cell_ui, "Offset", &mut self.offset_wheel,
                                    &mut params.offset_x, &mut params.offset_y, &mut params.offset_master,
                                    Some((&mut params.offset_sat, &mut params.offset_bri)), "offset");
                            }
                        });
                    } else {
                        show_wheel(ui, "Lift", &mut self.lift_wheel,
                            &mut params.lift_x, &mut params.lift_y, &mut params.lift_master,
                                    Some((&mut params.lift_sat, &mut params.lift_bri)), "lift");
                        show_wheel(ui, "Gamma", &mut self.gamma_wheel,
                            &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master,
                                    Some((&mut params.gamma_sat, &mut params.gamma_bri)), "gamma");
                        show_wheel(ui, "Gain", &mut self.gain_wheel,
                            &mut params.gain_x, &mut params.gain_y, &mut params.gain_master,
                                    Some((&mut params.gain_sat, &mut params.gain_bri)), "gain");
                        show_wheel(ui, "Offset", &mut self.offset_wheel,
                            &mut params.offset_x, &mut params.offset_y, &mut params.offset_master,
                                    Some((&mut params.offset_sat, &mut params.offset_bri)), "offset");
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
                                ui.label(egui::RichText::new(*group_label).size(10.0).color(theme.text_dim).strong());
                                let slider_w = col_w - 40.0; // room for label + value
                                let mut s = *sat_val;
                                let sat_resp = ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("S").size(10.0).color(theme.text_dim));
                                    let r = ui.add_sized([slider_w, 14.0],
                                        egui::Slider::new(&mut s, -1.0..=1.0).show_value(false));
                                    ui.label(egui::RichText::new(format!("{s:.2}")).size(10.0).color(theme.text_dim));
                                    r
                                }).inner;
                                if sat_resp.drag_started() {
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                }
                                if s != *sat_val {
                                    params.apply_override(sat_name, s);
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: (*sat_name).into(), value: s });
                                }
                                if sat_resp.drag_stopped() {
                                    cmds.push(AppCommand::EndNodeParamEdit { clip_id });
                                }
                                let mut b = *bri_val;
                                let bri_resp = ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("B").size(10.0).color(theme.text_dim));
                                    let r = ui.add_sized([slider_w, 14.0],
                                        egui::Slider::new(&mut b, -1.0..=1.0).show_value(false));
                                    ui.label(egui::RichText::new(format!("{b:.2}")).size(10.0).color(theme.text_dim));
                                    r
                                }).inner;
                                if bri_resp.drag_started() {
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                }
                                if b != *bri_val {
                                    params.apply_override(bri_name, b);
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: (*bri_name).into(), value: b });
                                }
                                if bri_resp.drag_stopped() {
                                    cmds.push(AppCommand::EndNodeParamEdit { clip_id });
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
                                atomic_node_edit_batch(cmds, clip_id, |c| {
                                    c.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: 0.0 });
                                    c.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.into(), value: 0.0 });
                                });
                            }
                            let mut s = sat_val;
                            if inspector_row(ui, "Saturation", &mut s, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                                params.apply_override(sat_name, s);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: s });
                            }
                            let mut b = bri_val;
                            if inspector_row(ui, "Brightness", &mut b, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                                params.apply_override(bri_name, b);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: bri_name.into(), value: b });
                            }
                            section_divider(ui, theme);
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
                        let bg = if active { color } else { theme.overlay(18) };
                        let text_color = if active { theme.text_on_accent } else { color };
                        ui.painter().rect_filled(rect, 4.0, bg);
                        ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(0.5_f32,
                            theme.overlay(30)));
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
                            theme.overlay(100)
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
                        let hist = self.histogram_for(ch).copied();
                        let resp = curves_editor::curves_editor_sized(
                            ui, theme, ch, points, curve_h, hist.as_ref(),
                        );
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
                        .color(theme.overlay(140))).changed()
                    {
                        params.curves_preserve_luma = Some(preserve);
                    }
                });
                ui.add_space(2.0);

                // Mix slider — same style as Color tab
                let mut mix = params.curves_mix.unwrap_or(1.0);
                if inspector_row(ui, "Mix", &mut mix, 0.0..=1.0, 1.0, "", theme, cmds, clip_id) {
                    params.curves_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_mix".into(), value: mix });
                }
                } // is_open
            }
            NodeKind::HueSatCurves => {
                let huesat = params.huesat.get_or_insert_with(Default::default);

                // All six curves the pipeline implements.
                //
                // Two were missing here and only here: the data model
                // has carried `sat_vs_sat` and `orange_vs_sat` all
                // along, the CPU executor builds all six into its
                // LUT, and the compute shader samples all
                // six — the comment below the table even said "Plus SvS
                // and OvS as additional curves". Only the table stopped
                // at four, so two working tools were unreachable. Sat
                // vs Sat in particular is what tames an oversaturated
                // red without touching its hue.
                //
                // Named for what they do, too. These were labelled
                // LUMA / RED / GREEN / BLUE, which are channel names
                // borrowed from the RGB curves tab — but none of these
                // is a channel curve, and a colourist reading "RED"
                // here would reasonably expect the red channel rather
                // than Hue vs Sat. The colours stay as a way to tell
                // the rows apart; the words now say what each curve is.
                let curve_entries: Vec<(&str, egui::Color32, CurveChannel, usize)> = vec![
                    ("HUE vs HUE", egui::Color32::from_rgb(0xcc, 0xcc, 0xcc), CurveChannel::Master, 0),
                    ("HUE vs SAT", egui::Color32::from_rgb(0xff, 0x50, 0x50), CurveChannel::Red, 1),
                    ("HUE vs LUM", egui::Color32::from_rgb(0x40, 0xcc, 0x50), CurveChannel::Green, 2),
                    ("LUM vs SAT", egui::Color32::from_rgb(0x50, 0x60, 0xff), CurveChannel::Blue, 3),
                    ("SAT vs SAT", egui::Color32::from_rgb(0xe0, 0xa0, 0x40), CurveChannel::Master, 4),
                    ("ORANGE vs SAT", egui::Color32::from_rgb(0xff, 0x90, 0x40), CurveChannel::Master, 5),
                    // Brightness by saturation — the direction the
                    // other six leave out. It is how a neon sign or a
                    // saturated sky is kept from reading too bright.
                    ("SAT vs LUM", egui::Color32::from_rgb(0x90, 0xd0, 0xc0), CurveChannel::Master, 6),
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
                            .color(theme.overlay(180)));

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
                                theme.overlay(100)
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
                                theme.overlay(100)
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
                            3 => huesat.lum_vs_sat.clear(),
                            4 => huesat.sat_vs_sat.clear(),
                            5 => huesat.orange_vs_sat.clear(),
                            _ => huesat.sat_vs_lum.clear(),
                        }
                        huesat_changed = true;
                    }

                    // Curve editor
                    let points = match curve_idx {
                        0 => &mut huesat.hue_vs_hue,
                        1 => &mut huesat.hue_vs_sat,
                        2 => &mut huesat.hue_vs_lum,
                        3 => &mut huesat.lum_vs_sat,
                        4 => &mut huesat.sat_vs_sat,
                        5 => &mut huesat.orange_vs_sat,
                        _ => &mut huesat.sat_vs_lum,
                    };

                    // Hue/Sat curves read against luma: their x axis is
                    // hue, luma or saturation, so the luma distribution
                    // is the useful backdrop rather than a per-primary one.
                    let hist = self.curve_histogram.as_ref().map(|h| h.luma);
                    // What the horizontal axis of each curve measures.
                    // Named on the row above; drawn as a strip below, so
                    // the oranges are findable by eye.
                    let x_axis = match curve_idx {
                        0..=2 => curves_editor::CurveAxis::Hue,
                        3 => curves_editor::CurveAxis::Luma,
                        // 4, 5 and 6 all read the incoming saturation.
                        _ => curves_editor::CurveAxis::Saturation,
                    };
                    let resp = curves_editor::curves_editor_in(
                        ui, theme, *channel, points, curve_h, hist.as_ref(),
                        // Relative: the pipeline fills these LUTs with
                        // 0.5 and reads them as (v - 0.5) * 2.
                        curves_editor::CurveSpace::Relative,
                        x_axis,
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
                        .color(theme.overlay(128))).changed()
                    {
                        params.curves_preserve_luma = Some(preserve);
                    }
                });
                ui.add_space(2.0);

                // Mix slider
                let mut mix = params.huesat_mix.unwrap_or(1.0);
                if inspector_row(ui, "Mix", &mut mix, 0.0..=1.0, 1.0, "", theme, cmds, clip_id) {
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
                    if grade_slider(ui, label, &mut v, range, "", cmds, clip_id, theme) {
                        params.apply_override(pname, v);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: pname.into(), value: v });
                    }
                }
            }
            NodeKind::InputTransform => {
                self.show_input_colour_space(ui, clip_id, cmds, theme);
            }
            NodeKind::Qualifier => {
                let qual = params.qualifier.get_or_insert_with(Default::default);
                ui.checkbox(&mut qual.enabled, "Enabled");
                if qual.enabled {
                    let mut hc = qual.hue_center;
                    if grade_slider(ui, "Hue Center", &mut hc, 0.0..=360.0, "deg", cmds, clip_id, theme) { qual.hue_center = hc; }
                    let mut hw = qual.hue_width;
                    if grade_slider(ui, "Hue Width", &mut hw, 0.0..=180.0, "deg", cmds, clip_id, theme) { qual.hue_width = hw; }
                    let mut hs = qual.hue_soft;
                    if grade_slider(ui, "Hue Soft", &mut hs, 0.0..=90.0, "deg", cmds, clip_id, theme) { qual.hue_soft = hs; }
                    ui.add_space(4.0);
                    let mut sl = qual.sat_low;
                    if grade_slider(ui, "Sat Low", &mut sl, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_low = sl; }
                    let mut sh = qual.sat_high;
                    if grade_slider(ui, "Sat High", &mut sh, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_high = sh; }
                    let mut ss = qual.sat_soft;
                    if grade_slider(ui, "Sat Soft", &mut ss, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_soft = ss; }
                    ui.add_space(4.0);
                    let mut ll = qual.lum_low;
                    if grade_slider(ui, "Lum Low", &mut ll, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_low = ll; }
                    let mut lh = qual.lum_high;
                    if grade_slider(ui, "Lum High", &mut lh, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_high = lh; }
                    let mut ls = qual.lum_soft;
                    if grade_slider(ui, "Lum Soft", &mut ls, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_soft = ls; }
                    ui.checkbox(&mut qual.invert, "Invert");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Matte Finesse").size(10.0).color(theme.text_dim));
                    let mut cb = qual.clean_black;
                    if grade_slider(ui, "Clean Black", &mut cb, 0.0..=1.0, "", cmds, clip_id, theme) { qual.clean_black = cb; }
                    let mut cw = qual.clean_white;
                    if grade_slider(ui, "Clean White", &mut cw, 0.0..=1.0, "", cmds, clip_id, theme) { qual.clean_white = cw; }
                    let mut sg = qual.shrink_grow;
                    if grade_slider(ui, "Shrink / Grow", &mut sg, -20.0..=20.0, "px", cmds, clip_id, theme) { qual.shrink_grow = sg; }
                    let mut bl = qual.blur;
                    if grade_slider(ui, "Blur", &mut bl, 0.0..=50.0, "px", cmds, clip_id, theme) { qual.blur = bl; }
                    cmds.push(AppCommand::SetNodeQualifier { clip_id, node_id, qualifier: qual.clone() });
                }
            }
            NodeKind::LumaKey => {
                let lk = params.luma_key.get_or_insert_with(Default::default);
                ui.checkbox(&mut lk.enabled, "Enabled");
                if lk.enabled {
                    let mut low = lk.low;
                    if grade_slider(ui, "Low", &mut low, 0.0..=1.0, "", cmds, clip_id, theme) { lk.low = low; }
                    let mut high = lk.high;
                    if grade_slider(ui, "High", &mut high, 0.0..=1.0, "", cmds, clip_id, theme) { lk.high = high; }
                    let mut soft = lk.soft;
                    if grade_slider(ui, "Soft", &mut soft, 0.0..=0.5, "", cmds, clip_id, theme) { lk.soft = soft; }
                    ui.checkbox(&mut lk.invert, "Invert");
                    cmds.push(AppCommand::SetNodeLumaKey { clip_id, node_id, luma_key: lk.clone() });
                }
            }
            NodeKind::SoftClip => {
                let sc = params.soft_clip.get_or_insert_with(Default::default);
                ui.checkbox(&mut sc.enabled, "Enabled");
                if sc.enabled {
                    let mut threshold = sc.threshold;
                    if grade_slider(ui, "Threshold", &mut threshold, 0.0..=1.0, "", cmds, clip_id, theme) { sc.threshold = threshold; }
                    let mut knee = sc.knee;
                    if grade_slider(ui, "Knee", &mut knee, 0.0..=0.5, "", cmds, clip_id, theme) { sc.knee = knee; }
                    cmds.push(AppCommand::SetNodeSoftClip { clip_id, node_id, soft_clip: sc.clone() });
                }
            }
            NodeKind::Mask => {
                let masks = params.masks.get_or_insert_with(Default::default);
                Self::render_masks_body(ui, masks, node_id, clip_id, cmds, theme, frame, &[]);
            }
            NodeKind::FilmGrain => {
                let grain = params.grain.get_or_insert_with(Default::default);
                ui.checkbox(&mut grain.enabled, "Enabled");
                if grain.enabled {
                    let mut a = grain.amount;
                    if grade_slider(ui, "Amount", &mut a, 0.0..=1.0, "", cmds, clip_id, theme) { grain.amount = a; }
                    let mut s = grain.size;
                    if grade_slider(ui, "Size", &mut s, 0.5..=3.0, "", cmds, clip_id, theme) { grain.size = s; }
                    let mut sh = grain.shadows;
                    if grade_slider(ui, "Shadows", &mut sh, 0.0..=1.0, "", cmds, clip_id, theme) { grain.shadows = sh; }
                    let mut hi = grain.highlights;
                    if grade_slider(ui, "Highlights", &mut hi, 0.0..=1.0, "", cmds, clip_id, theme) { grain.highlights = hi; }
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
                if grade_slider(ui, "Opacity", &mut opacity, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.lut_opacity = Some(opacity);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "lut_opacity".into(), value: opacity });
                }
                // Colorspace tags. `.cube` and friends carry no
                // standardised colorspace metadata, so the loader
                // defaults to Rec.709 / sRGB on both sides; the user
                // re-tags here when the LUT was authored against a
                // different working space (e.g. an ACES-AP1-tagged
                // LUT loaded into a Rec.709 project, or vice versa).
                lut_colorspace_dropdown(
                    ui, theme, "Input space",
                    "lut_input_space",
                    &mut params.lut_input_space,
                    cmds, clip_id, node_id,
                );
                lut_colorspace_dropdown(
                    ui, theme, "Output space",
                    "lut_output_space",
                    &mut params.lut_output_space,
                    cmds, clip_id, node_id,
                );
            }
            NodeKind::Blend | NodeKind::ApplyMatte => {
                let mut mix = params.mix.unwrap_or(1.0);
                if grade_slider(ui, "Mix", &mut mix, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "mix".into(), value: mix });
                }
            }
            NodeKind::Denoise => {
                let mut luma = params.denoise_luma.unwrap_or(0.0);
                if grade_slider(ui, "Luma Strength", &mut luma, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.denoise_luma = Some(luma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_luma".into(), value: luma });
                }
                let mut chroma = params.denoise_chroma.unwrap_or(0.0);
                if grade_slider(ui, "Chroma Strength", &mut chroma, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.denoise_chroma = Some(chroma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_chroma".into(), value: chroma });
                }
                let mut radius = params.denoise_radius.unwrap_or(3) as f32;
                if grade_slider(ui, "Radius", &mut radius, 1.0..=7.0, "px", cmds, clip_id, theme) {
                    let r = radius.round() as u32;
                    params.denoise_radius = Some(r);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_radius".into(), value: r as f32 });
                }
            }
            NodeKind::ColorWarper => {
                // The same header every other grade section uses, so
                // the warper resets where a colourist already looks for
                // it. Its reset was a text button under the wheel,
                // alone in the panel in using that shape.
                let (reset, _open) = section_header(ui, "COLOUR WARPER", true, theme);
                if reset {
                    if let Some(w) = params.warper_params.as_mut() {
                        w.hue_shifts = [0.0; 12];
                        w.sat_shifts = [0.0; 12];
                    }
                    atomic_node_edit_batch(cmds, clip_id, |c| {
                        for s in 0..12 {
                            c.push(AppCommand::SetNodeParam {
                                clip_id, node_id, param: format!("warper_hue_{s}"), value: 0.0,
                            });
                            c.push(AppCommand::SetNodeParam {
                                clip_id, node_id, param: format!("warper_sat_{s}"), value: 0.0,
                            });
                        }
                    });
                }
                self.show_warper_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::CombineMattes => {
                let current = params.matte_combine_op.unwrap_or_default();
                let mut chosen = current;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Combine").small().color(theme.text_dim));
                    egui::ComboBox::from_id_salt(("matte_combine_op", node_id))
                        .selected_text(chosen.display_name())
                        .show_ui(ui, |ui| {
                            for &candidate in &klipp_nodes::MatteCombineOp::ALL {
                                if ui
                                    .selectable_label(chosen == candidate, candidate.display_name())
                                    .clicked()
                                {
                                    chosen = candidate;
                                }
                            }
                        });
                });
                if chosen != current {
                    params.matte_combine_op = Some(chosen);
                    cmds.push(AppCommand::SetMatteCombineOp {
                        clip_id,
                        node_id,
                        op: klipp_nodes::MatteCombineOp::ALL
                            .iter()
                            .position(|c| *c == chosen)
                            .unwrap_or(0) as u8,
                    });
                }
            }
            NodeKind::ExternalMatte => {
                self.show_external_matte_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::DifferenceKey => {
                self.show_difference_key_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::ColorMatch => {
                self.show_color_match_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::LayerMixer => {
                self.show_layer_mixer_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::Corrector => {
                // Corrector params are surfaced through the colour-grade
                // tabs (Adjustments / Wheels / Curves / Hue-Sat / Warper)
                // — those tabs already prefer the Corrector when present.
                // Selecting the Corrector node directly in the V2 editor
                // points the user at the right place rather than
                // duplicating the widgets here.
                ui.label(
                    egui::RichText::new(
                        "Edit this node via the Adjustments / Wheels / Curves / Hue-Sat / Warper tabs."
                    ).color(theme.text_dim).small()
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Import CDL…")
                        .on_hover_text("Put an ASC CDL file's slope, offset, power and saturation on this node's wheels")
                        .clicked()
                    {
                        crate::file_dialog::spawn(move || {
                            let Some(path) = rfd::FileDialog::new().add_filter("ASC CDL", &["cdl", "cc", "ccc"]).pick_file() else {
                                return Vec::new();
                            };
                            let read = std::fs::read_to_string(&path)
                                .map_err(|e| e.to_string())
                                .and_then(|t| klipp_grade_model::cdl::Cdl::parse(&t));
                            match read {
                                Ok(cdl) => {
                                    let mut cmds = Vec::new();
                                    let (lift, gamma, gain, offset) = cdl.wheel_amounts();
                                    for (name, amounts) in [("lift", lift), ("gamma", gamma), ("gain", gain), ("offset", offset)] {
                                        let (x, y, m) = klipp_color::rgb_to_wheel_offset((amounts[0], amounts[1], amounts[2]));
                                        for (suffix, v) in [("x", x), ("y", y), ("master", m)] {
                                            cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{name}_{suffix}"), value: v });
                                        }
                                    }
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: cdl.saturation });
                                    tracing::info!("Imported CDL from {}", path.display());
                                    cmds
                                }
                                Err(e) => {
                                    tracing::warn!("CDL import failed for {}: {e}", path.display());
                                    crate::file_dialog::notify(format!("Could not import the CDL: {e}"));
                                    Vec::new()
                                }
                            }
                        });
                    }
                    if ui
                        .button("Export CDL…")
                        .on_hover_text("Write this node's wheels and saturation as an ASC CDL file")
                        .clicked()
                    {
                        // The grade is taken now, as the button was pressed;
                        // only the file is written once a place is chosen.
                        let wheel = |x: Option<f32>, y: Option<f32>, m: Option<f32>| {
                            let (r, g, b) = klipp_color::wheel_offset_to_rgb(x.unwrap_or(0.0), y.unwrap_or(0.0), m.unwrap_or(0.0));
                            [r, g, b]
                        };
                        let cdl = klipp_grade_model::cdl::Cdl::from_wheels(
                            wheel(params.lift_x, params.lift_y, params.lift_master),
                            wheel(params.gamma_x, params.gamma_y, params.gamma_master),
                            wheel(params.gain_x, params.gain_y, params.gain_master),
                            wheel(params.offset_x, params.offset_y, params.offset_master),
                            params.saturation.unwrap_or(1.0),
                        );
                        crate::file_dialog::spawn(move || {
                            if let Some(path) = rfd::FileDialog::new().add_filter("ASC CDL", &["cdl"]).set_file_name("grade.cdl").save_file() {
                                let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "klipp".into());
                                match std::fs::write(&path, cdl.to_xml(&id)) {
                                    Ok(()) => tracing::info!("Exported CDL to {}", path.display()),
                                    Err(e) => {
                                        tracing::warn!("CDL export failed for {}: {e}", path.display());
                                        crate::file_dialog::notify(format!("Could not write the CDL: {e}"));
                                    }
                                }
                            }
                            Vec::new()
                        });
                    }
                });
            }
            NodeKind::Source | NodeKind::Output | NodeKind::Layer
            | NodeKind::RGBSplitter | NodeKind::RGBCombiner
            | NodeKind::MaskCutout
            | NodeKind::Reroute
            | NodeKind::Unknown => {
                ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
            }
            NodeKind::PrimariesMatrix => {
                ui.label(
                    egui::RichText::new(
                        "Linear-light 3×3 primaries conversion. Set by the project's \
                         working-colorspace migration; safe to leave alone unless you're \
                         hand-building a custom IDT."
                    )
                    .color(theme.text_dim)
                    .small(),
                );
                let label_for = |ws: Option<klipp_core::WorkingColorspace>| -> &'static str {
                    match ws {
                        None => "(unset — passthrough)",
                        Some(klipp_core::WorkingColorspace::Rec709Linear) => "Rec.709 / sRGB linear",
                        Some(klipp_core::WorkingColorspace::AcesCG) => "ACEScg (AP1)",
                        Some(klipp_core::WorkingColorspace::Bt2020Linear) => "BT.2020 linear",
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("From:").color(theme.text_dim).small());
                    ui.label(label_for(params.primaries_from));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("To:").color(theme.text_dim).small());
                    ui.label(label_for(params.primaries_to));
                });
            }
            NodeKind::Compound => {
                // Compound nodes hold a sub-graph; the parameters
                // panel is intentionally minimal — editing the sub-
                // graph happens via descend (stage 4 will wire the
                // double-click + breadcrumb). For now we just tell
                // the user how many nodes are nested so the inspector
                // isn't a blank panel.
                let count = params.compound_subgraph
                    .as_ref()
                    .map(|g| g.nodes.len())
                    .unwrap_or(0);
                ui.label(
                    egui::RichText::new(format!(
                        "Compound subgraph — {count} node{}",
                        if count == 1 { "" } else { "s" },
                    ))
                    .color(theme.text_dim),
                );
                ui.label(
                    egui::RichText::new("Double-click to descend (coming soon)")
                        .small()
                        .color(theme.text_dim),
                );
            }
        }
    }

    /// Inspector for `ExternalMatte`: file picker, channel selector,
    /// invert toggle. The path + channel are stored in NodeParams;
    /// the executor decodes + caches per node edit.
    fn show_external_matte_widget(
        &mut self,
        ui: &mut egui::Ui,
        node_id: klipp_nodes::NodeId,
        params: &mut klipp_nodes::NodeParams,
        clip_id: klipp_core::ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &crate::theme::KlippTheme,
    ) {
        // File picker.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let label = match &params.matte_image_path {
                Some(p) => p.file_name().map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string_lossy().to_string()),
                None => "(none selected)".to_string(),
            };
            ui.label(
                egui::RichText::new(label).small().color(theme.text_dim)
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("Choose image…").clicked() {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .add_filter("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"])
                        .pick_file()
                        .map(|path| vec![AppCommand::SetExternalMattePath {
                            clip_id, node_id, path: Some(path),
                        }])
                        .unwrap_or_default()
                });
            }
            if params.matte_image_path.is_some()
                && ui.button("Clear").clicked()
            {
                params.matte_image_path = None;
                cmds.push(AppCommand::SetExternalMattePath {
                    clip_id, node_id, path: None,
                });
            }
        });

        ui.add_space(4.0);

        // Channel selector.
        let current_channel = params.matte_channel.unwrap_or_default();
        let invert = params.matte_invert.unwrap_or(false);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Source").small().color(theme.text_dim));
            let mut new_channel = current_channel;
            egui::ComboBox::from_id_salt(("ext_matte_channel", node_id))
                .selected_text(new_channel.display_name())
                .show_ui(ui, |ui| {
                    for &candidate in &klipp_nodes::MatteChannel::ALL {
                        if ui.selectable_label(new_channel == candidate, candidate.display_name()).clicked() {
                            new_channel = candidate;
                        }
                    }
                });
            if new_channel != current_channel {
                params.matte_channel = Some(new_channel);
                cmds.push(AppCommand::SetExternalMatteChannel {
                    clip_id, node_id,
                    channel: new_channel as u8,
                    invert,
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let mut inv = invert;
            if ui.checkbox(&mut inv, "Invert").changed() {
                params.matte_invert = Some(inv);
                cmds.push(AppCommand::SetExternalMatteChannel {
                    clip_id, node_id,
                    channel: current_channel as u8,
                    invert: inv,
                });
            }
        });
    }

    /// Inspector for `DifferenceKey`: clean-plate file picker + match
    /// space (RGB / Luminance) + tolerance + soft + invert. Mirrors
    /// the ExternalMatte file-picker shape; the threshold sliders use
    /// the standard `grade_slider` helper so they sit visually with
    /// the other matte-producer inspectors.
    fn show_difference_key_widget(
        &mut self,
        ui: &mut egui::Ui,
        node_id: klipp_nodes::NodeId,
        params: &mut klipp_nodes::NodeParams,
        clip_id: klipp_core::ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &crate::theme::KlippTheme,
    ) {
        let dk = params.difference_key.get_or_insert_with(Default::default);

        // Enable / disable the whole node.
        let mut enabled = dk.enabled;
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            dk.enabled = enabled;
            cmds.push(AppCommand::SetNodeDifferenceKey {
                clip_id, node_id, difference_key: dk.clone(),
            });
        }
        if !dk.enabled {
            ui.label(
                egui::RichText::new("Pick a clean-plate image and tune tolerance to enable.")
                    .size(10.0).color(theme.text_dim),
            );
            return;
        }

        ui.add_space(4.0);

        // Clean-plate file picker.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let label = match &dk.plate_path {
                Some(p) => p.file_name().map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string_lossy().to_string()),
                None => "(no plate selected)".to_string(),
            };
            ui.label(egui::RichText::new(label).small().color(theme.text_dim));
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("Choose plate…").clicked() {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .add_filter("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"])
                        .pick_file()
                        .map(|path| vec![AppCommand::SetNodeDifferenceKeyPlate {
                            clip_id, node_id, plate_path: Some(path),
                        }])
                        .unwrap_or_default()
                });
            }
            if dk.plate_path.is_some() && ui.button("Clear").clicked() {
                dk.plate_path = None;
                cmds.push(AppCommand::SetNodeDifferenceKeyPlate {
                    clip_id, node_id, plate_path: None,
                });
            }
        });

        ui.add_space(4.0);

        // Match-space dropdown (RGB / Luminance).
        let current_space = dk.match_space;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Match").small().color(theme.text_dim));
            let mut new_space = current_space;
            egui::ComboBox::from_id_salt(("diff_key_match_space", node_id))
                .selected_text(match new_space {
                    klipp_core::DifferenceKeyMatchSpace::Rgb => "RGB",
                    klipp_core::DifferenceKeyMatchSpace::Luminance => "Luminance",
                })
                .show_ui(ui, |ui| {
                    if ui.selectable_label(
                        new_space == klipp_core::DifferenceKeyMatchSpace::Rgb, "RGB",
                    ).clicked() {
                        new_space = klipp_core::DifferenceKeyMatchSpace::Rgb;
                    }
                    if ui.selectable_label(
                        new_space == klipp_core::DifferenceKeyMatchSpace::Luminance, "Luminance",
                    ).clicked() {
                        new_space = klipp_core::DifferenceKeyMatchSpace::Luminance;
                    }
                });
            if new_space != current_space {
                dk.match_space = new_space;
                cmds.push(AppCommand::SetNodeDifferenceKey {
                    clip_id, node_id, difference_key: dk.clone(),
                });
            }
        });

        // Tolerance + soft sliders.
        let mut tolerance = dk.tolerance;
        if grade_slider(ui, "Tolerance", &mut tolerance, 0.0..=1.0, "", cmds, clip_id, theme) {
            dk.tolerance = tolerance;
            cmds.push(AppCommand::SetNodeDifferenceKey {
                clip_id, node_id, difference_key: dk.clone(),
            });
        }
        let mut soft = dk.soft;
        if grade_slider(ui, "Soft", &mut soft, 0.0..=0.5, "", cmds, clip_id, theme) {
            dk.soft = soft;
            cmds.push(AppCommand::SetNodeDifferenceKey {
                clip_id, node_id, difference_key: dk.clone(),
            });
        }

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let mut invert = dk.invert;
            if ui.checkbox(&mut invert, "Invert").changed() {
                dk.invert = invert;
                cmds.push(AppCommand::SetNodeDifferenceKey {
                    clip_id, node_id, difference_key: dk.clone(),
                });
            }
        });
    }

    /// Inspector for `LayerMixer`: one row per input port, with an
    /// opacity slider and a blend-mode dropdown. Port 0 is shown at
    /// the top to match the editor's stack-order ("port 0 sits on top
    /// of the composite"). Empty state hint when no ports have been
    /// wired yet — the user has to drop a wire on the node first.
    fn show_layer_mixer_widget(
        &mut self,
        ui: &mut egui::Ui,
        node_id: klipp_nodes::NodeId,
        params: &mut klipp_nodes::NodeParams,
        clip_id: klipp_core::ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &crate::theme::KlippTheme,
    ) {
        let mixer = params.layer_mixer.get_or_insert_with(Default::default);
        if mixer.inputs.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Wire inputs into this node — drop a wire on the left edge to add a port."
                ).color(theme.text_dim).small()
            );
            return;
        }
        let count = mixer.inputs.len();
        for port in 0..count {
            let input_settings = mixer.inputs[port];
            let label = if port == 0 { "Input 1 (top)".to_string() }
                else { format!("Input {}", port + 1) };
            ui.label(egui::RichText::new(label).strong().small());
            let mut opacity = input_settings.opacity;
            let resp = ui.add(
                egui::Slider::new(&mut opacity, 0.0..=1.0)
                    .text("Opacity")
                    .fixed_decimals(2),
            );
            let mut blend = input_settings.blend_mode;
            let mut blend_changed = false;
            egui::ComboBox::from_id_salt(("layer_mixer_blend", node_id, port))
                .selected_text(blend.display_name())
                .show_ui(ui, |ui| {
                    for &candidate in &klipp_core::BlendMode::ALL {
                        if ui.selectable_label(blend == candidate, candidate.display_name()).clicked() {
                            blend = candidate;
                            blend_changed = true;
                        }
                    }
                });
            if resp.changed() || blend_changed {
                mixer.inputs[port] = klipp_core::LayerMixerInput { opacity, blend_mode: blend };
                cmds.push(AppCommand::SetNodeLayerMixerInput {
                    clip_id,
                    node_id,
                    port: port as u8,
                    input: mixer.inputs[port],
                });
            }
            ui.add_space(6.0);
        }
    }

    /// Inspector for the ColorMatch node. Two-step UX:
    /// 1. User parks the playhead on a frame whose look they want
    ///    to copy and clicks "Capture reference" — the current
    ///    program-viewer frame is stashed in `AppState`.
    /// 2. User parks the playhead on the source clip and clicks
    ///    "Apply match" — the engine runs `match_colors` against
    ///    the captured reference + the clip's pre-grade frame and
    ///    stores the four-scalar result on this node.
    ///
    /// The four computed adjustments are displayed read-only so the
    /// user can see what the algorithm did. "Reset" clears them
    /// (back to identity = pass-through).
    fn show_color_match_widget(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        params: &mut NodeParams,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Capture reference").on_hover_text(
                    "Save the current program-viewer frame as the reference look. \
                     Park the playhead on the frame you want to match before clicking."
                ).clicked() {
                    cmds.push(AppCommand::CaptureColorMatchReference);
                }
                if ui.button("Apply match").on_hover_text(
                    "Compute the colour-match adjustments using the captured reference \
                     and this clip's pre-grade frame at the current playhead."
                ).clicked() {
                    cmds.push(AppCommand::ApplyColorMatch { clip_id, node_id });
                }
            });
            ui.add_space(6.0);

            let s = params.color_match.unwrap_or_default();
            let dim = theme.text_dim;
            let val_color = if s.is_identity() { dim } else { theme.text_primary };
            let row = |ui: &mut egui::Ui, label: &str, value: String| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{label:<14}")).color(dim).monospace());
                    ui.label(egui::RichText::new(value).color(val_color).monospace());
                });
            };
            row(ui, "Exposure", format!("{:+.2} stops", s.exposure));
            row(ui, "Temperature", format!("{:+.1}", s.temperature));
            row(ui, "Tint", format!("{:+.1}", s.tint));
            row(ui, "Saturation", format!("{:.2}×", s.saturation));

            ui.add_space(6.0);
            if ui.button("Reset").on_hover_text(
                "Clear the stored match — node passes the frame through unchanged."
            ).clicked() {
                cmds.push(AppCommand::SetNodeColorMatch {
                    clip_id,
                    node_id,
                    settings: klipp_core::ColorMatchSettings::default(),
                });
            }
        });
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

        // Allocate the full width so the wheel lands in the middle of
        // the panel. Allocating only the circle's own footprint left it
        // pinned to the left edge with a hand's width of dead space
        // beside it, and the sector labels on the left — CG, C, CB —
        // ran off the panel because they are drawn outside the radius.
        let avail = ui.available_width();
        let side = avail.min(260.0);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(avail, side), egui::Sense::click_and_drag());
        let center = rect.center();
        // Room outside the ring for the twelve sector labels: they sit
        // at radius + 12 and are centred on that point, so the longest
        // of them needs about another dozen pixels beyond that.
        let radius = side / 2.0 - 28.0;
        let painter = ui.painter();

        // Background circle
        painter.circle_filled(center, radius + 4.0, theme.bg_deep);
        painter.circle_stroke(center, radius, egui::Stroke::new(1.0_f32, theme.border));

        // Crosshair lines
        painter.line_segment(
            [egui::pos2(center.x - radius, center.y), egui::pos2(center.x + radius, center.y)],
            egui::Stroke::new(0.5_f32, theme.overlay(15)),
        );
        painter.line_segment(
            [egui::pos2(center.x, center.y - radius), egui::pos2(center.x, center.y + radius)],
            egui::Stroke::new(0.5_f32, theme.overlay(15)),
        );

        // Inner reference circles at 33% and 66% radius
        painter.circle_stroke(center, radius * 0.33, egui::Stroke::new(0.3_f32, theme.overlay(10)));
        painter.circle_stroke(center, radius * 0.66, egui::Stroke::new(0.3_f32, theme.overlay(10)));

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
                egui::Stroke::new(0.3_f32, theme.overlay(12)),
            );

            // A faint tint marking which way each sector lies.
            //
            // This was an eight-pixel filled circle, which at a glance
            // was indistinguishable from a control point — twelve dim
            // dots inside the ring that looked like handles and did
            // nothing. Smaller and dimmer: enough to place the sector,
            // not enough to be mistaken for something to grab.
            let dot_x = center.x + angle.cos() * radius * 0.42;
            let dot_y = center.y - angle.sin() * radius * 0.42;
            painter.circle_filled(
                egui::pos2(dot_x, dot_y),
                2.0,
                egui::Color32::from_rgba_premultiplied(
                    (hr * 60.0) as u8, (hg * 60.0) as u8, (hb * 60.0) as u8, 60
                ),
            );

            // Labels outside circle
            let lx = center.x + angle.cos() * (radius + 12.0);
            let ly = center.y - angle.sin() * (radius + 12.0);
            painter.text(
                egui::pos2(lx, ly),
                egui::Align2::CENTER_CENTER,
                sector_labels[i],
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_premultiplied(
                    (hr * 180.0 + 75.0).min(255.0) as u8,
                    (hg * 180.0 + 75.0).min(255.0) as u8,
                    (hb * 180.0 + 75.0).min(255.0) as u8,
                    180,
                ),
            );
        }

        // ── The mesh ──
        //
        // A warper is a field, not twelve unrelated nudges, and until
        // now it was drawn as twelve unconnected dots: you could see
        // that a point had moved but not what the warp was doing
        // between them. Joining the control points into a closed loop
        // makes the deformation the thing you look at — the ring bulges
        // where saturation is pushed and leans where hue is rotated,
        // against the neutral circle it started from.
        let positions: Vec<egui::Pos2> = (0..12)
            .map(|i| {
                let base_angle = (i as f32 * 30.0).to_radians();
                let a = base_angle + warper.hue_shifts[i].to_radians() * 0.02;
                let r = neutral_r + warper.sat_shifts[i] * radius * 0.25;
                egui::pos2(center.x + a.cos() * r, center.y - a.sin() * r)
            })
            .collect();

        // The shape the warp started from, so the deformation is read
        // against something rather than in the abstract.
        painter.circle_stroke(
            center,
            neutral_r,
            egui::Stroke::new(1.0_f32, theme.overlay(35)),
        );

        // Catmull-Rom through the twelve points, closed, so the mesh
        // curves the way the warp does instead of showing twelve
        // straight chords that imply corners the grade does not have.
        let mut mesh: Vec<egui::Pos2> = Vec::with_capacity(12 * 8);
        for i in 0..12 {
            let p0 = positions[(i + 11) % 12];
            let p1 = positions[i];
            let p2 = positions[(i + 1) % 12];
            let p3 = positions[(i + 2) % 12];
            for step in 0..8 {
                let t = step as f32 / 8.0;
                let t2 = t * t;
                let t3 = t2 * t;
                let x = 0.5 * ((2.0 * p1.x) + (-p0.x + p2.x) * t
                    + (2.0 * p0.x - 5.0 * p1.x + 4.0 * p2.x - p3.x) * t2
                    + (-p0.x + 3.0 * p1.x - 3.0 * p2.x + p3.x) * t3);
                let y = 0.5 * ((2.0 * p1.y) + (-p0.y + p2.y) * t
                    + (2.0 * p0.y - 5.0 * p1.y + 4.0 * p2.y - p3.y) * t2
                    + (-p0.y + 3.0 * p1.y - 3.0 * p2.y + p3.y) * t3);
                mesh.push(egui::pos2(x, y));
            }
        }
        for w in 0..mesh.len() {
            let a = mesh[w];
            let b = mesh[(w + 1) % mesh.len()];
            painter.line_segment([a, b], egui::Stroke::new(1.6_f32, theme.overlay(150)));
        }

        // Draw control points and shift indicators
        for i in 0..12 {
            let base_angle = (i as f32 * 30.0).to_radians();
            let hue_deg = i as f32 * 30.0;
            let (hr, hg, hb) = klipp_color::hsv_to_rgb(hue_deg, 1.0, 1.0);

            // Neutral position on the circle
            let nx = center.x + base_angle.cos() * neutral_r;
            let ny = center.y - base_angle.sin() * neutral_r;

            let (px, py) = (positions[i].x, positions[i].y);

            // Draw shift line from neutral to current position
            if warper.hue_shifts[i].abs() > 0.1 || warper.sat_shifts[i].abs() > 0.01 {
                painter.line_segment(
                    [egui::pos2(nx, ny), egui::pos2(px, py)],
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_premultiplied(
                        (hr * 200.0) as u8, (hg * 200.0) as u8, (hb * 200.0) as u8, 100
                    )),
                );
            }

            // Control point dot
            let point_color = egui::Color32::from_rgb(
                (hr * 255.0) as u8, (hg * 255.0) as u8, (hb * 255.0) as u8
            );
            painter.circle_filled(egui::pos2(px, py), 5.0, point_color);
            painter.circle_stroke(egui::pos2(px, py), 5.0, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
        }

        // Drag begin/end for undo coalescing.
        if resp.drag_started() {
            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        }
        if resp.drag_stopped() {
            cmds.push(AppCommand::EndNodeParamEdit { clip_id });
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

                // Say what the drag is doing, in the units the
                // parameters are in. Without it the warper can be
                // moved but not set: you cannot match a sector across
                // two shots, or put back what you just changed.
                let label = format!(
                    "{}   hue {:+.0}\u{00B0}   sat {:+.0}%",
                    sector_labels[sector], new_hue_shift, new_sat_shift * 100.0,
                );
                let galley = painter.layout_no_wrap(
                    label,
                    egui::FontId::monospace(10.0),
                    theme.text_primary,
                );
                // Top-left of the wheel, clear of the ring and of the
                // pointer wherever it is.
                let at = egui::pos2(rect.min.x + 8.0, rect.min.y + 6.0);
                painter.rect_filled(
                    egui::Rect::from_min_size(at, galley.size()).expand(4.0),
                    3.0,
                    theme.bg_deep,
                );
                painter.galley(at, galley, theme.text_primary);

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


        // Show active sector values as compact readout
        ui.add_space(4.0);
        let active_count = (0..12).filter(|&i| warper.hue_shifts[i].abs() > 0.1 || warper.sat_shifts[i].abs() > 0.01).count();
        if active_count > 0 {
            ui.label(egui::RichText::new(format!("{active_count} sector(s) adjusted")).size(10.0).color(theme.text_dim));
        }
    }

    // ── Tool preset bar ──

    fn show_tool_preset_bar(
        &mut self,
        ui: &mut egui::Ui,
        tool: &ToolScope,
        graph: &mut NodeGraph,
        _clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        let presets = match tool {
            ToolScope::Wheels => &mut self.wheels_presets,
            ToolScope::Curves => &mut self.curves_presets,
            ToolScope::HueSat => &mut self.huesat_presets,
            ToolScope::Warper => &mut self.warper_presets,
            ToolScope::Color => &mut self.color_presets,
            ToolScope::FullGrade => return,
        };

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            // Says which of the two it is. A Look in the GRADE header
            // above saves the whole grade; this saves the tool you are
            // looking at. Two preset systems a few centimetres apart
            // with nothing distinguishing them is how people end up
            // trusting neither.
            ui.label(egui::RichText::new("Preset").size(10.0).color(theme.text_dim))
                .on_hover_text(
                    "A preset for this tool only.\n\
                     Looks, in the GRADE header, save the whole grade.",
                );

            // Preset dropdown
            let combo_id = format!("tool_preset_{:?}", tool);
            egui::ComboBox::from_id_salt(&combo_id)
                .selected_text("Select...")
                .width(130.0)
                .show_ui(ui, |ui| {
                    let mut apply_idx = None;
                    for (i, preset) in presets.iter().enumerate() {
                        if ui.selectable_label(false, &preset.name).clicked() {
                            apply_idx = Some(i);
                        }
                    }
                    if let Some(idx) = apply_idx {
                        let preset = presets[idx].clone();
                        presets::apply_tool_preset(graph, &preset);
                        cmds.push(AppCommand::RefreshGrade);
                    }
                    if !presets.is_empty() {
                        ui.separator();
                        ui.menu_button("Delete...", |ui| {
                            let mut to_delete = None;
                            for preset in presets.iter() {
                                if ui.button(&preset.name).clicked() {
                                    to_delete = Some(preset.name.clone());
                                    ui.close_menu();
                                }
                            }
                            if let Some(name) = to_delete {
                                let _ = presets::delete_tool_preset(tool, &name);
                                // Can't modify presets here (immutable ref via menu_button)
                                // — removal deferred below
                            }
                        });
                    }
                });

            // Save button
            if ui.small_button("\u{1F4BE}").on_hover_text("Save current settings as preset").clicked() {
                self.preset_saving_tool = Some(tool.clone());
                self.preset_save_name_tool.clear();
            }
        });

        // Save dialog (below the bar)
        if self.preset_saving_tool.as_ref() == Some(tool) {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.preset_save_name_tool)
                        .desired_width(130.0)
                        .hint_text("Preset name...")
                );
                // Auto-focus the text field when dialog opens
                if resp.gained_focus() || self.preset_save_name_tool.is_empty() {
                    resp.request_focus();
                }
                let name = self.preset_save_name_tool.trim().to_string();
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.small_button("Save").clicked() || enter) && !name.is_empty() {
                    let params = presets::extract_tool_params(graph, tool);
                    let preset = ToolPreset {
                        name: name.clone(),
                        tool: tool.clone(),
                        params,
                    };
                    let _ = presets::save_tool_preset(&preset);
                    let presets = match tool {
                        ToolScope::Wheels => &mut self.wheels_presets,
                        ToolScope::Curves => &mut self.curves_presets,
                        ToolScope::HueSat => &mut self.huesat_presets,
                        ToolScope::Warper => &mut self.warper_presets,
                        ToolScope::Color => &mut self.color_presets,
                        ToolScope::FullGrade => unreachable!(),
                    };
                    presets.push(preset);
                    presets.sort_by(|a, b| a.name.cmp(&b.name));
                    self.preset_save_name_tool.clear();
                    self.preset_saving_tool = None;
                }
                if ui.small_button("\u{00D7}").clicked() {
                    self.preset_saving_tool = None;
                }
            });
        }
        ui.add_space(2.0);
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
        egui::Rounding::same(4.0)
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
        theme.overlay(100),
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
    let dot_color = if enabled { theme.text_accent } else { theme.overlay(46) };
    ui.painter().circle_filled(egui::pos2(dot_x, cy), 3.0, dot_color);

    // Reset button
    let icon_x = rect.max.x - PAD - 6.0;
    let reset_rect = egui::Rect::from_center_size(egui::pos2(icon_x, cy), egui::vec2(16.0, 16.0));
    let reset_resp = ui.allocate_rect(reset_rect, egui::Sense::click());
    let reset_tint = if reset_resp.hovered() {
        theme.text_accent
    } else {
        theme.overlay(100)
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
fn section_divider(ui: &mut egui::Ui, theme: &crate::theme::KlippTheme) {
    ui.add_space(4.0);
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme.overlay(18));
    ui.add_space(4.0);
}

/// Single parameter row: [label] [==slider==] [value] [suffix]
/// Matches the HTML mockup — shorter slider, proper proportions.
fn inspector_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    suffix: &str,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
) -> bool {
    let id = ui.next_auto_id();
    let editing = slider_value_editing(ui, id);
    let avail_w = ui.available_width();
    let range_span = (range.end() - range.start()).abs();
    let RowLayout { label_w, slider_w, value_w, suffix_w } = row_layout(avail_w, !suffix.is_empty());

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());

    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, theme.overlay(13));
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
            theme.overlay(107),
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
                theme.overlay(26),
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
        if !editing {
            painter.text(
                egui::pos2(value_x, cy),
                egui::Align2::LEFT_CENTER,
                format!("{:.2}", *value),
                egui::FontId::proportional(11.0),
                theme.overlay(166),
            );
        }

        // Suffix
        if !suffix.is_empty() {
            painter.text(
                egui::pos2(value_x + value_w, cy),
                egui::Align2::LEFT_CENTER,
                suffix,
                egui::FontId::proportional(10.0),
                theme.overlay(107),
            );
        }
    }

    let slider_x = row_rect.min.x + PAD + label_w;
    slider_interact(
        ui, id, SliderGeometry { x: slider_x, w: slider_w, cy, value_w: value_w + suffix_w },
        value, range, default, cmds, clip_id,
    )
}

/// Widths across an `inspector_row`: label ~28%, slider ~38%, value
/// ~14%, suffix ~10%, padding the rest.
struct RowLayout {
    label_w: f32,
    slider_w: f32,
    value_w: f32,
    suffix_w: f32,
}

fn row_layout(avail_w: f32, has_suffix: bool) -> RowLayout {
    let label_w = (avail_w * 0.28).max(50.0);
    let suffix_w = if has_suffix { (avail_w * 0.10).max(28.0) } else { 0.0 };
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - suffix_w - 8.0).max(40.0);
    RowLayout { label_w, slider_w, value_w, suffix_w }
}

/// Where a slider row's track and value field sit.
struct SliderGeometry {
    /// Left end of the track.
    x: f32,
    /// Track length.
    w: f32,
    cy: f32,
    /// Width of the value field (and suffix) to the right of the track.
    value_w: f32,
}

/// Whether the row with this id is showing a text field for its value.
fn slider_value_editing(ui: &egui::Ui, id: egui::Id) -> bool {
    ui.data(|d| d.get_temp::<String>(id.with("edit")).is_some())
}

/// Shift for fine steps and Ctrl (Cmd) for coarse ones, as the colour
/// wheels do.
fn slider_drag_factor(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        if i.modifiers.shift {
            0.1
        } else if i.modifiers.command {
            4.0
        } else {
            1.0
        }
    })
}

/// This frame's horizontal drag. egui calls it a drag only after the
/// pointer has moved a few pixels; the first frame counts from where the
/// button went down, so those pixels are not lost.
fn drag_dx(ui: &egui::Ui, resp: &egui::Response) -> f32 {
    if resp.drag_started() {
        ui.input(|i| i.pointer.press_origin().zip(i.pointer.latest_pos()).map(|(o, p)| p.x - o.x)).unwrap_or(0.0)
    } else {
        resp.drag_delta().x
    }
}

/// What a click at `click_x` on a track does: nothing near the thumb, so
/// grabbing it to drag does not move it; elsewhere, jump there.
fn track_click_target(click_x: f32, thumb_x: f32, x: f32, w: f32) -> Option<f32> {
    ((click_x - thumb_x).abs() > 8.0).then(|| ((click_x - x) / w).clamp(0.0, 1.0))
}

/// The Colour tab's slider behaviour, shared by its rows (#321). A drag
/// moves the value from where it was by the distance dragged, not to the
/// pointer; Shift is fine and Ctrl coarse. A click jumps only when it is
/// away from the thumb. Double-click or right-click resets the one
/// parameter to `default`. Double-click the number to type a value
/// (Enter keeps it, Escape drops it); drag the number for small steps.
fn slider_interact(
    ui: &mut egui::Ui,
    id: egui::Id,
    g: SliderGeometry,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let span = (hi - lo).abs().max(f32::EPSILON);
    let mut changed = false;

    // The track's hit area stops short of the value field, so a drag
    // that starts on the number is the number's.
    let track_rect = egui::Rect::from_min_max(
        egui::pos2(g.x - 6.0, g.cy - 12.0),
        egui::pos2(g.x + g.w + 3.0, g.cy + 12.0),
    );
    let track = ui.interact(track_rect, id.with("track"), egui::Sense::click_and_drag());
    if track.drag_started() {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    }
    if track.dragged() {
        let dx = drag_dx(ui, &track);
        if dx != 0.0 {
            *value = (*value + dx / g.w * span * slider_drag_factor(ui)).clamp(lo, hi);
            changed = true;
        }
    } else if track.clicked() {
        let thumb_x = g.x + g.w * ((*value - lo) / span).clamp(0.0, 1.0);
        if let Some(t) = track.interact_pointer_pos().and_then(|p| track_click_target(p.x, thumb_x, g.x, g.w)) {
            *value = lo + t * span;
            changed = true;
        }
    }
    if track.drag_stopped() {
        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
    }

    let val_rect = egui::Rect::from_min_size(
        egui::pos2(g.x + g.w + 4.0, g.cy - 10.0),
        egui::vec2(g.value_w + 8.0, 20.0),
    );
    let edit_id = id.with("edit");
    if let Some(mut text) = ui.data(|d| d.get_temp::<String>(edit_id)) {
        let field = ui.put(val_rect, egui::TextEdit::singleline(&mut text).font(egui::FontId::proportional(11.0)));
        if !field.has_focus() && !field.lost_focus() {
            field.request_focus();
        }
        if field.lost_focus() {
            ui.data_mut(|d| d.remove::<String>(edit_id));
            let keep = ui.input(|i| !i.key_pressed(egui::Key::Escape));
            if let Some(v) = keep.then(|| text.trim().parse::<f32>().ok()).flatten().filter(|v| v.is_finite()) {
                *value = v.clamp(lo, hi);
                changed = true;
            }
        } else {
            ui.data_mut(|d| d.insert_temp(edit_id, text));
        }
    } else {
        let val = ui.interact(val_rect, id.with("value"), egui::Sense::click_and_drag());
        if val.drag_started() {
            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        }
        if val.dragged() {
            *value = (*value + drag_dx(ui, &val) * span / 800.0 * slider_drag_factor(ui)).clamp(lo, hi);
            changed = true;
        }
        if val.drag_stopped() {
            cmds.push(AppCommand::EndNodeParamEdit { clip_id });
        }
        if val.double_clicked() {
            ui.data_mut(|d| d.insert_temp(edit_id, format!("{:.2}", *value)));
        } else if val.secondary_clicked() {
            *value = default;
            changed = true;
        }
        val.on_hover_text("Drag to adjust (Shift fine, Ctrl coarse) · double-click to type · right-click to reset");
    }
    if track.double_clicked() || track.secondary_clicked() {
        *value = default;
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
    default: f32,
    color_left: egui::Color32,
    color_right: egui::Color32,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
) -> bool {
    let id = ui.next_auto_id();
    let editing = slider_value_editing(ui, id);
    let avail_w = ui.available_width();
    let range_span = (range.end() - range.start()).abs();
    let label_w = (avail_w * 0.28).max(50.0);
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - 8.0).max(40.0);
    let pct = ((*value - range.start()) / range_span).clamp(0.0, 1.0);

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, theme.overlay(13));
    }
    let cy = row_rect.center().y;
    let slider_x = row_rect.min.x + PAD + label_w;

    {
        let painter = ui.painter().clone();

        painter.text(egui::pos2(row_rect.min.x + PAD, cy), egui::Align2::LEFT_CENTER,
            label, egui::FontId::proportional(11.0),
            theme.overlay(107));

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
        if !editing {
            painter.text(egui::pos2(slider_x + slider_w + 8.0, cy), egui::Align2::LEFT_CENTER,
                format!("{:.2}", *value), egui::FontId::proportional(11.0),
                theme.overlay(166));
        }
    }

    slider_interact(
        ui, id, SliderGeometry { x: slider_x, w: slider_w, cy, value_w },
        value, range, default, cmds, clip_id,
    )
}

/// Hue slider row: rainbow spectrum track.
fn hue_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
) -> bool {
    let id = ui.next_auto_id();
    let editing = slider_value_editing(ui, id);
    let avail_w = ui.available_width();
    let label_w = (avail_w * 0.28).max(50.0);
    let value_w = (avail_w * 0.14).max(32.0);
    let slider_w = (avail_w - PAD * 2.0 - label_w - value_w - 8.0).max(40.0);
    let pct = (*value / 360.0).clamp(0.0, 1.0);

    let (row_rect, resp) = ui.allocate_exact_size(egui::vec2(avail_w, ROW_H), egui::Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(row_rect, 0.0, theme.overlay(13));
    }
    let cy = row_rect.center().y;
    let slider_x = row_rect.min.x + PAD + label_w;

    {
        let painter = ui.painter().clone();

        painter.text(egui::pos2(row_rect.min.x + PAD, cy), egui::Align2::LEFT_CENTER,
            label, egui::FontId::proportional(11.0),
            theme.overlay(107));

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
        if !editing {
            painter.text(egui::pos2(slider_x + slider_w + 8.0, cy), egui::Align2::LEFT_CENTER,
                format!("{:.2}", *value), egui::FontId::proportional(11.0),
                theme.overlay(166));
        }
    }

    slider_interact(
        ui, id, SliderGeometry { x: slider_x, w: slider_w, cy, value_w },
        value, 0.0..=360.0, 0.0, cmds, clip_id,
    )
}

/// Per-node colorspace dropdown for the Lut3D inspector. Renders a
/// 4-option ComboBox (Use LUT default / Rec.709 linear / ACEScg /
/// BT.2020) wired to a `Option<WorkingColorspace>` field on
/// `NodeParams`. "Use LUT default" maps to `None` — the executor
/// falls back to whatever tag the LUT carries internally (always
/// `Rec709Linear` for parser-loaded `.cube` files until a tagged
/// LUT format ships).
///
/// The `param_key` argument is passed through as the `SetNodeParam`
/// command's parameter name so the existing
/// `param_key → NodeParams field` dispatch in `klipp_nodes::params`
/// handles persistence + history. WorkingColorspace is encoded as
/// its `as_u8` tag, then dispatched as f32 — matches the pattern
/// already used by `curves_preserve_luma` etc. that reuse the
/// `f32 → bool` shape.
fn lut_colorspace_dropdown(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    param_key: &str,
    field: &mut Option<klipp_core::WorkingColorspace>,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    node_id: NodeId,
) {
    use klipp_core::WorkingColorspace;
    // Sentinel u8 = 255: "use LUT default". Anything in 0..=2 is a
    // real working space tag; the dispatcher's defensive bounds
    // check turns out-of-range values back into None.
    const USE_LUT_DEFAULT: u8 = 255;
    let label_text = match *field {
        None => "Use LUT default",
        Some(WorkingColorspace::Rec709Linear) => "Rec.709 / sRGB",
        Some(WorkingColorspace::AcesCG) => "ACEScg",
        Some(WorkingColorspace::Bt2020Linear) => "BT.2020",
    };
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label}:"))
                .size(11.0)
                .color(theme.text_dim),
        );
        let mut current_tag: u8 = field.map(|w| w.as_u8()).unwrap_or(USE_LUT_DEFAULT);
        let prev_tag = current_tag;
        egui::ComboBox::from_id_salt((param_key, node_id))
            .width(150.0)
            .selected_text(label_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut current_tag, USE_LUT_DEFAULT, "Use LUT default");
                ui.selectable_value(
                    &mut current_tag,
                    WorkingColorspace::Rec709Linear.as_u8(),
                    "Rec.709 / sRGB",
                );
                ui.selectable_value(
                    &mut current_tag,
                    WorkingColorspace::AcesCG.as_u8(),
                    "ACEScg",
                );
                ui.selectable_value(
                    &mut current_tag,
                    WorkingColorspace::Bt2020Linear.as_u8(),
                    "BT.2020",
                );
            });
        if current_tag != prev_tag {
            *field = if current_tag == USE_LUT_DEFAULT {
                None
            } else {
                Some(WorkingColorspace::from_u8(current_tag))
            };
            // Encode for the dispatcher: 255 sentinel maps to None
            // via the defensive bounds check (>= 256 OR == 255 both
            // route through to `None`); 0/1/2 round-trip cleanly.
            let value = if current_tag == USE_LUT_DEFAULT {
                f32::INFINITY
            } else {
                current_tag as f32
            };
            cmds.push(AppCommand::SetNodeParam {
                clip_id,
                node_id,
                param: param_key.into(),
                value,
            });
        }
    });
}

/// Legacy grade_slider for non-Adjustments tabs. Takes `cmds` + `clip_id` so
/// drag gestures are wrapped in `BeginNodeParamEdit` / `EndNodeParamEdit` and
/// enter the undo history as a single step.
fn grade_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    theme: &crate::theme::KlippTheme,
) -> bool {
    let pad = 14.0;
    let w = (ui.available_width() - pad * 2.0).max(60.0);
    let range_span = (range.end() - range.start()).abs();
    let speed = range_span / 800.0;
    let mut changed = false;
    // Begin/End signals collected inside the horizontal closures (which can't
    // mutably borrow `cmds` directly) and flushed after each closure returns.
    let mut drag_value_started = false;
    let mut drag_value_ended = false;
    let mut drag_slider_started = false;
    let mut drag_slider_ended = false;

    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.label(egui::RichText::new(label).size(11.0).color(
            theme.overlay(107)));
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
            if resp.drag_started() { drag_value_started = true; }
            if resp.changed() { changed = true; }
            if resp.drag_stopped() { drag_value_ended = true; }
        });
    });
    if drag_value_started {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    }
    if drag_value_ended {
        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
    }
    ui.horizontal(|ui| {
        ui.add_space(pad);
        let resp = ui.add_sized(
            [w, 10.0],
            egui::Slider::new(value, range).show_value(false).clamping(egui::SliderClamping::Always),
        );
        if resp.drag_started() { drag_slider_started = true; }
        if resp.changed() { changed = true; }
        if resp.drag_stopped() { drag_slider_ended = true; }
    });
    if drag_slider_started {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    }
    if drag_slider_ended {
        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
    }
    changed
}

/// Push a single atomic node-param edit wrapped in a `BeginNodeParamEdit` /
/// `EndNodeParamEdit` pair so it enters the undo history as one step. Use for
/// click-to-reset, preset buttons, and other instantaneous (non-drag) edits.
fn push_atomic_node_edit(
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    node_id: NodeId,
    param: &str,
    value: f32,
) {
    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    cmds.push(AppCommand::SetNodeParam {
        clip_id,
        node_id,
        param: param.into(),
        value,
    });
    cmds.push(AppCommand::EndNodeParamEdit { clip_id });
}

/// Wrap a closure that emits one or more `SetNodeParam` commands with a single
/// `BeginNodeParamEdit` / `EndNodeParamEdit` pair so the whole batch becomes
/// one undoable history entry. Use for multi-param resets (section reset,
/// "Reset Warper", etc.).
fn atomic_node_edit_batch<F: FnOnce(&mut Vec<AppCommand>)>(
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    body: F,
) {
    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    body(cmds);
    cmds.push(AppCommand::EndNodeParamEdit { clip_id });
}

fn push_wheel_cmds(cmds: &mut Vec<AppCommand>, clip_id: ClipId, node_id: NodeId, prefix: &str, offset: [f32; 2], master: f32) {
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_x", prefix), value: offset[0] });
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_y", prefix), value: offset[1] });
    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{}_master", prefix), value: master });
}

#[cfg(test)]
mod huesat_curve_coverage {
    //! Every Hue/Sat curve the pipeline applies must be reachable.
    //!
    //! `sat_vs_sat` and `orange_vs_sat` existed in `HueSatCurves`, were
    //! built into the executor's LUT, and were sampled by the compute
    //! shader — and could not be touched, because the inspector's table
    //! listed four of the six. Two finished tools were invisible for
    //! want of two rows.
    //!
    //! Both matches below end in a catch-all, so a curve added to the
    //! set and not to the table would quietly edit the last one rather
    //! than fail to compile. This is what notices.

    const SRC: &str = include_str!("node_inspector.rs");

    /// The fields of `klipp_core::HueSatCurves`, which is what the LUT
    /// builder walks in order.
    const CURVES: &[&str] = &[
        "hue_vs_hue", "hue_vs_sat", "hue_vs_lum",
        "lum_vs_sat", "sat_vs_sat", "orange_vs_sat", "sat_vs_lum",
    ];

    #[test]
    fn every_pipeline_curve_is_editable() {
        // The inspector reaches a curve by naming its field in the
        // points match; a curve absent there has no editor.
        let missing: Vec<&&str> = CURVES
            .iter()
            .filter(|c| !SRC.contains(&format!("&mut huesat.{c}")))
            .collect();
        assert!(
            missing.is_empty(),
            "these Hue/Sat curves are applied by the pipeline but have no editor: {missing:?}"
        );
    }

    #[test]
    fn every_editable_curve_can_be_reset() {
        let missing: Vec<&&str> = CURVES
            .iter()
            .filter(|c| !SRC.contains(&format!("huesat.{c}.clear()")))
            .collect();
        assert!(
            missing.is_empty(),
            "these Hue/Sat curves have an editor but no reset: {missing:?}"
        );
    }
}

#[cfg(test)]
mod slider_behaviour {
    //! The Colour tab's sliders, driven with real pointer and key events
    //! (#321): a drag moves the value by the distance dragged, a click on
    //! the thumb does not move it, double-click or right-click resets
    //! the one parameter, and a double-click on the number types it.
    use super::*;

    const WIDTH: f32 = 400.0;

    struct Rig {
        ctx: egui::Context,
        time: f64,
        value: f32,
        cmds: Vec<AppCommand>,
    }

    impl Rig {
        fn new(value: f32) -> Self {
            let mut rig = Rig { ctx: egui::Context::default(), time: 0.0, value, cmds: Vec::new() };
            rig.frame(Vec::new(), egui::Modifiers::NONE);
            rig
        }

        fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 100.0))),
                time: Some(self.time),
                modifiers,
                events,
                ..Default::default()
            };
            let theme = KlippTheme::default();
            let (value, cmds) = (&mut self.value, &mut self.cmds);
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    inspector_row(ui, "Blacks", value, -1.0..=1.0, 0.0, "", &theme, cmds, ClipId::new());
                });
            });
        }

        fn geometry(&self) -> (f32, f32, f32) {
            let l = row_layout(WIDTH, false);
            let x = PAD + l.label_w;
            (x, l.slider_w, ROW_H / 2.0)
        }

        fn thumb(&self) -> egui::Pos2 {
            let (x, w, cy) = self.geometry();
            egui::pos2(x + w * ((self.value + 1.0) / 2.0), cy)
        }

        fn number(&self) -> egui::Pos2 {
            let (x, w, cy) = self.geometry();
            egui::pos2(x + w + 16.0, cy)
        }

        fn button(&mut self, at: egui::Pos2, button: egui::PointerButton, pressed: bool, modifiers: egui::Modifiers) {
            self.frame(
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton { pos: at, button, pressed, modifiers },
                ],
                modifiers,
            );
        }

        fn click(&mut self, at: egui::Pos2) {
            self.button(at, egui::PointerButton::Primary, true, egui::Modifiers::NONE);
            self.button(at, egui::PointerButton::Primary, false, egui::Modifiers::NONE);
        }

        fn drag(&mut self, from: egui::Pos2, dx: f32, modifiers: egui::Modifiers) {
            self.button(from, egui::PointerButton::Primary, true, modifiers);
            for i in 1..=4 {
                self.frame(vec![egui::Event::PointerMoved(from + egui::vec2(dx * i as f32 / 4.0, 0.0))], modifiers);
            }
            self.button(from + egui::vec2(dx, 0.0), egui::PointerButton::Primary, false, modifiers);
            self.frame(Vec::new(), modifiers);
        }
    }

    #[test]
    fn a_drag_moves_the_value_by_the_distance_not_to_the_pointer() {
        let mut rig = Rig::new(0.0);
        let (_, w, _) = rig.geometry();
        // Grab a little to the right of the thumb, as a hand does.
        let grab = rig.thumb() + egui::vec2(5.0, 0.0);
        rig.drag(grab, 20.0, egui::Modifiers::NONE);
        let want = 20.0 / w * 2.0;
        assert!((rig.value - want).abs() < 0.01, "moved to {} for a 20 px drag, want {want}", rig.value);
    }

    #[test]
    fn shift_drags_finely_and_ctrl_coarsely() {
        let mut fine = Rig::new(0.0);
        let at = fine.thumb();
        fine.drag(at, 20.0, egui::Modifiers::SHIFT);
        let mut plain = Rig::new(0.0);
        let at = plain.thumb();
        plain.drag(at, 20.0, egui::Modifiers::NONE);
        let mut coarse = Rig::new(0.0);
        let at = coarse.thumb();
        coarse.drag(at, 20.0, egui::Modifiers::COMMAND);
        assert!(fine.value > 0.0 && fine.value < plain.value / 5.0, "fine {} vs plain {}", fine.value, plain.value);
        assert!(coarse.value > plain.value * 2.0, "coarse {} vs plain {}", coarse.value, plain.value);
    }

    #[test]
    fn a_click_on_the_thumb_leaves_it_and_one_elsewhere_jumps() {
        let mut rig = Rig::new(0.3);
        let at = rig.thumb() + egui::vec2(4.0, 0.0);
        rig.click(at);
        assert_eq!(rig.value, 0.3, "a click on the thumb moved it");
        let (x, w, cy) = rig.geometry();
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        rig.time += 1.0;
        rig.click(egui::pos2(x + w * 0.25, cy));
        assert!((rig.value + 0.5).abs() < 0.02, "a click a quarter along should jump to -0.5, got {}", rig.value);
    }

    #[test]
    fn double_click_or_right_click_resets_the_one_slider() {
        let mut rig = Rig::new(-0.4);
        let at = rig.thumb();
        rig.click(at);
        rig.click(at);
        assert_eq!(rig.value, 0.0, "double-click did not reset");
        rig.value = 0.6;
        rig.time += 1.0;
        let at = rig.number();
        rig.button(at, egui::PointerButton::Secondary, true, egui::Modifiers::NONE);
        rig.button(at, egui::PointerButton::Secondary, false, egui::Modifiers::NONE);
        assert_eq!(rig.value, 0.0, "right-click on the number did not reset");
    }

    #[test]
    fn the_number_can_be_typed() {
        let mut rig = Rig::new(0.1);
        let at = rig.number();
        rig.click(at);
        rig.click(at);
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        // Replace the field's text, then Enter.
        rig.frame(
            vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }],
            egui::Modifiers::COMMAND,
        );
        rig.frame(vec![egui::Event::Text("-0.35".into())], egui::Modifiers::NONE);
        rig.frame(
            vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            egui::Modifiers::NONE,
        );
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        assert!((rig.value + 0.35).abs() < 1e-6, "typed -0.35, got {}", rig.value);
        // Escape keeps the old value.
        let at = rig.number();
        rig.time += 1.0;
        rig.click(at);
        rig.click(at);
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        rig.frame(vec![egui::Event::Text("9".into())], egui::Modifiers::NONE);
        rig.frame(
            vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            egui::Modifiers::NONE,
        );
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        assert!((rig.value + 0.35).abs() < 1e-6, "Escape changed the value to {}", rig.value);
    }

    #[test]
    fn a_drag_that_starts_on_the_number_is_the_numbers() {
        let mut rig = Rig::new(0.0);
        let (x, w, cy) = rig.geometry();
        // Just right of the track's end, where the number begins: this
        // used to be the track, and jumped the value to the end.
        rig.drag(egui::pos2(x + w + 5.0, cy), 10.0, egui::Modifiers::NONE);
        assert!(rig.value > 0.0 && rig.value < 0.05, "the number's small step, got {}", rig.value);
    }
}
