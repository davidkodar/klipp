use crate::widgets::menu::{SubmenuRows, MenuRows};
use crate::inspector::keyframe_selection::FrameSelection;
use crate::icons::Icon;
use crate::presets::{self, ToolPreset, ToolScope};
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
    /// One printer point, from the colour preferences (#241), for the
    /// offset's readout in points.
    pub printer_point: f32,
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
    /// Why the last tool-preset save was refused, shown under the name
    /// field until the next attempt.
    pub preset_save_error: Option<String>,
    /// The selected clip's input colour space and whether detection
    /// chose it, set by the caller each frame: the InputTransform node
    /// edits the media item's format, which this panel cannot see.
    pub input_colour_space: (klipp_core::LogFormat, bool),
    /// With the generated grade inspector on, the clip's own grade nodes,
    /// set by the caller each frame: the tabbed page draws the parameters
    /// it shares with the per-node layout as their generated rows from it
    /// (M3 step 11c). `None` draws them as before.
    pub(crate) generated: Option<crate::inspector::grade_view::GradeSnapshot>,
    /// The masks of the node being drawn that a tracker drives, by index,
    /// so the mask list offers them Re-track and Untrack. Set by the
    /// generated view (`grade_view::show`, which has the clip) for its
    /// draw, as the inspector's Colour tab passes its own.
    pub(crate) tracked_masks: Vec<usize>,
    /// An HDR Zones node's picked zone (#240), by node: the zone menu's
    /// choice, Global until one is picked.
    pub(crate) hdr_zone_pick: std::collections::HashMap<NodeId, usize>,
    /// The picked zone's colour wheel.
    pub hdr_zone_wheel: ColorWheelState,
}

impl Default for NodeInspectorPanel {
    fn default() -> Self {
        Self {
            printer_point: 0.0125,
            lift_wheel: ColorWheelState::default(),
            gamma_wheel: ColorWheelState::default(),
            gain_wheel: ColorWheelState::default(),
            offset_wheel: ColorWheelState::default(),
            log_low_wheel: ColorWheelState::default(),
            log_mid_wheel: ColorWheelState::default(),
            log_high_wheel: ColorWheelState::default(),
            hdr_zone_pick: Default::default(),
            hdr_zone_wheel: ColorWheelState::default(),
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
            preset_save_error: None,
            input_colour_space: (klipp_core::LogFormat::Srgb, false),
            generated: None,
            tracked_masks: Vec::new(),
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
        // Under its header, so it can be switched off like any node (#1351).
        let idt = selected_node
            .and_then(|id| graph.nodes.iter().find(|n| n.id == id))
            .filter(|n| n.kind == NodeKind::InputTransform);
        if let Some(n) = idt {
            node_header(ui, &n.kind, n.enabled, &n.keyframes, n.id, clip_id, &mut cmds, theme, frame);
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
        // The pivot isn't on this row: it sits with Contrast on the
        // Colour tab (`show_adjustments_tab`) and in the node's own
        // inspector, so this row stays one control wide.
        if *color_tab == ColorTab::Wheels {
            self.show_wheels_contrast_row(ui, graph, clip_id, cmds, theme, frame);
        }

        match color_tab {
            ColorTab::Adjustments => {
                self.show_adjustments_tab(ui, graph, clip_id, selected_node, cmds, theme, frame);
            }
            ColorTab::Wheels if wide_layout => {
                // Wide layout: combined wheels + sat/bri in unified columns
                self.show_wheels_wide(ui, graph, clip_id, cmds, theme, frame);
            }
            _ => {
                // The tab is its target's generated rows, the editors among
                // them registered (M3 step 19), and a level with no
                // Corrector, the grade's top or a compound in it, draws
                // the one its first edit makes (#1483).
                let tab_kind = match color_tab {
                    ColorTab::Wheels => Some(NodeKind::ColorWheels),
                    ColorTab::Curves => Some(NodeKind::RGBCurves),
                    ColorTab::HueSat => Some(NodeKind::HueSatCurves),
                    ColorTab::Warper => Some(NodeKind::ColorWarper),
                    _ => None,
                };
                if let Some(tab_kind) = tab_kind {
                    self.generated_tab(ui, graph, clip_id, tab_kind, cmds, theme, frame);
                }
            }
        }
    }

    /// One of the Wheels, Curves, Hue-Sat and Warper tabs, generated (M3
    /// step 19): the node it always targeted (an enabled Corrector, else an
    /// enabled node of the tab's kind), under its header, as its generated
    /// rows in the tab's band of the Corrector's page, or all of another
    /// kind's. With neither, the Corrector the snapshot holds pending,
    /// drawn from its defaults: its first edit makes it (IC16: beside a
    /// disabled Corrector too, which is left as it is).
    ///
    /// The widgets edit a copy of the node's values: every change is the
    /// commands they push, never a write into the grade while drawing.
    #[allow(clippy::too_many_arguments)]
    fn generated_tab(&mut self, ui: &mut egui::Ui, graph: &NodeGraph, clip_id: ClipId, tab_kind: NodeKind, cmds: &mut Vec<AppCommand>, theme: &KlippTheme, frame: FrameSelection) {
        let Some(snapshot) = self.generated.take() else { return };
        let target = graph.nodes.iter().find(|n| n.kind == NodeKind::Corrector && n.enabled)
            .or_else(|| graph.nodes.iter().find(|n| n.kind == tab_kind && n.enabled))
            .map(|n| (n.id, n.kind, n.enabled, n.keyframes.clone(), n.params.clone()))
            .or_else(|| snapshot.pending().map(|id| (id, NodeKind::Corrector, true, Default::default(), NodeParams::default())));
        if let Some((node, kind, enabled, keyframes, mut params)) = target {
            node_header(ui, &tab_kind, enabled, &keyframes, node, clip_id, cmds, theme, frame);
            // Where each tab's rows sit on the Corrector's page.
            let band = (kind == NodeKind::Corrector).then_some(match tab_kind {
                NodeKind::ColorWheels => 10..=29,
                NodeKind::RGBCurves => 40..=49,
                NodeKind::HueSatCurves => 50..=59,
                _ => 60..=69,
            });
            snapshot.tab(ui, theme, node, band, &mut params, self, cmds, frame);
        }
        self.generated = Some(snapshot);
    }

    /// Wide-layout combined wheels + sat/bri — each wheel column includes its sliders beneath.
    fn show_wheels_wide(
        &mut self,
        ui: &mut egui::Ui,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
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

        // In log mode the grade reads the Low, Mid and High wheels and not
        // the linear four (the executor's `corrector_stages`), so those
        // are the wheels drawn: three columns, without saturation and
        // brightness, which log wheels don't have.
        let log_mode = wheels_idx.is_some_and(|wi| graph.nodes[wi].params.wheels_log_mode.unwrap_or(false));
        let columns: f32 = if log_mode { 3.0 } else { 4.0 };
        let avail_w = ui.available_width();
        // The columns with 2px gaps between them.
        let cell_w = ((avail_w - 2.0 * (columns - 1.0)) / columns).floor();
        let wheel_d = (cell_w - 14.0).clamp(50.0, 150.0);

        // Linear / Log toggle
        if let Some(wi) = wheels_idx {
            let log_mode = graph.nodes[wi].params.wheels_log_mode.unwrap_or(false);
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "wheels_log_mode", "Linear / Log (HDR)", log_mode);
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
            wheel_shape_sliders(ui, &graph.nodes[wi].params, log_mode, node_id, cmds, clip_id, theme);
            ui.add_space(4.0);
        }

        // The offset as printer points (#241), as the keys move it.
        if let Some(wi) = wheels_idx {
            let p = &graph.nodes[wi].params;
            let [r, g, b] = klipp_color::printer_points((p.offset_x.unwrap_or(0.0), p.offset_y.unwrap_or(0.0), p.offset_master.unwrap_or(0.0)), self.printer_point.max(1e-4));
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("Printer points  R{r:+}  G{g:+}  B{b:+}")).small().monospace().color(theme.text_dim))
                    .on_hover_text("The offset in printer points. Ctrl+Alt+7/8/9 raise red, green, blue a point, Ctrl+Alt+4/5/6 lower them, Ctrl+Alt+3/1 move all three; the step is in Preferences \u{2192} Colour.");
            });
        }

        // Helper: render one wheel + its sat/bri sliders in a column
        let wheels_node_id = wheels_idx.map(|i| graph.nodes[i].id);
        let satbri_node_id = satbri_idx.map(|i| graph.nodes[i].id);

        // The wheel groups: (label, param prefix).
        let groups: &[(&str, &str)] = if log_mode {
            &[("Low", "log_low"), ("Mid", "log_mid"), ("High", "log_high")]
        } else {
            &[("Lift", "lift"), ("Gamma", "gamma"), ("Gain", "gain"), ("Offset", "offset")]
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, prefix) in groups {
                let col_rect = ui.allocate_exact_size(
                    egui::vec2(cell_w, wheel_d + 120.0),
                    egui::Sense::hover(),
                ).0;
                let mut col_ui = ui.new_child(egui::UiBuilder::new().max_rect(col_rect).layout(egui::Layout::top_down(egui::Align::Center)));

                // Wheel
                if let Some(wi) = wheels_idx {
                    let node = &mut graph.nodes[wi];
                    let keys = NodeKeys { tracks: &node.keyframes, frame };
                    let params = &mut node.params;
                    let node_id = wheels_node_id.unwrap();
                    let (x_field, y_field, m_field) = match *prefix {
                        "lift" => (&mut params.lift_x, &mut params.lift_y, &mut params.lift_master),
                        "gamma" => (&mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master),
                        "gain" => (&mut params.gain_x, &mut params.gain_y, &mut params.gain_master),
                        "log_low" => (&mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master),
                        "log_mid" => (&mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master),
                        "log_high" => (&mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master),
                        _ => (&mut params.offset_x, &mut params.offset_y, &mut params.offset_master),
                    };
                    // The wheel's parts, keyed together (#891).
                    let names = [format!("{prefix}_x"), format!("{prefix}_y"), format!("{prefix}_master")];
                    let parts = [names[0].as_str(), names[1].as_str(), names[2].as_str()];
                    let mut offset = [keys.shown(parts[0], x_field.unwrap_or(0.0)), keys.shown(parts[1], y_field.unwrap_or(0.0))];
                    let mut master = keys.shown(parts[2], m_field.unwrap_or(0.0));
                    let shown = [offset[0], offset[1], master];
                    let wheel_state = match *prefix {
                        "lift" => &mut self.lift_wheel,
                        "gamma" => &mut self.gamma_wheel,
                        "gain" => &mut self.gain_wheel,
                        "log_low" => &mut self.log_low_wheel,
                        "log_mid" => &mut self.log_mid_wheel,
                        "log_high" => &mut self.log_high_wheel,
                        _ => &mut self.offset_wheel,
                    };
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(col_ui.next_auto_id(), "colour_wheel", prefix, label, (offset, master));
                    let resp = color_wheel::color_wheel_sized(
                        &mut col_ui, wheel_state, theme, label, &mut offset, &mut master, wheel_d, Some(cell_w), Some(keys.state(&parts)),
                    );
                    if resp.drag_started {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if resp.changed {
                        // A click, a reset or a typed pill is one step;
                        // a drag's step is its gesture's. A keyed wheel's
                        // edit is a key.
                        let values = [offset[0], offset[1], master];
                        if keys.keyed(&parts) {
                            if resp.dragging {
                                keys.key_values(cmds, clip_id, node_id, &parts, &values);
                            } else {
                                atomic_node_edit_batch(cmds, clip_id, |c| keys.key_values(c, clip_id, node_id, &parts, &values));
                            }
                        } else {
                            *x_field = Some(offset[0]);
                            *y_field = Some(offset[1]);
                            *m_field = Some(master);
                            if resp.dragging {
                                push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                            } else {
                                atomic_node_edit_batch(cmds, clip_id, |c| push_wheel_cmds(c, clip_id, node_id, prefix, offset, master));
                            }
                        }
                    }
                    // Released, the drag is one step; ended by Escape, taken back.
                    if let (_, Some(end)) = gesture_of(&mut col_ui, false, resp.drag_ended) {
                        cmds.push(end(clip_id));
                    }
                    keys.strip_commands(cmds, clip_id, node_id, &parts, &shown, resp.key);
                }

                // Sat/Bri sliders painted manually below the wheel, each
                // with its key strip.
                if let Some(si) = satbri_idx.filter(|_| !log_mode) {
                    let node = &mut graph.nodes[si];
                    let keys = NodeKeys { tracks: &node.keyframes, frame };
                    let params = &mut node.params;
                    let node_id = satbri_node_id.unwrap();
                    let (sat_field, bri_field) = match *prefix {
                        "lift" => (&mut params.lift_sat, &mut params.lift_bri),
                        "gamma" => (&mut params.gamma_sat, &mut params.gamma_bri),
                        "gain" => (&mut params.gain_sat, &mut params.gain_bri),
                        _ => (&mut params.offset_sat, &mut params.offset_bri),
                    };

                    let painter = col_ui.painter().clone();
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

                    let mut label_y = cursor_y + 4.0;
                    for (title, field, name) in [("Saturation", sat_field, format!("{prefix}_sat")), ("Brightness", bri_field, format!("{prefix}_bri"))] {
                        let title_rect = painter.text(
                            egui::pos2(col_rect.center().x, label_y + label_h / 2.0),
                            egui::Align2::CENTER_CENTER,
                            title,
                            egui::FontId::proportional(10.0),
                            theme.text_dim,
                        );
                        let track_y = label_y + label_h + 2.0;
                        let track_rect = egui::Rect::from_min_size(
                            egui::pos2(cx, track_y + slider_h / 2.0 - 1.5),
                            egui::vec2(track_draw_w, 4.0),
                        );
                        painter.rect_filled(track_rect, 2.0, track_bg);
                        let hit = egui::Rect::from_min_size(egui::pos2(cx - 4.0, track_y), egui::vec2(track_draw_w + 8.0, slider_h));
                        let resp = col_ui.allocate_rect(hit, egui::Sense::click_and_drag());
                        let param = [name.as_str()];
                        let keyed = keys.keyed(&param);
                        let mut v = keys.shown(&name, field.unwrap_or(0.0));
                        let shown = v;
                        #[cfg(any(test, feature = "row-recorder"))]
                        {
                            crate::widgets::row_recorder::next_parameter(&name);
                            crate::widgets::row_recorder::numeric(resp.id, "wheel_slider", title, v, &(-1.0..=1.0), Some(0.0), None, Some(2), "", None);
                        }
                        if resp.drag_started() {
                            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                        }
                        if let (_, Some(end)) = gesture_of(&mut col_ui, false, resp.drag_stopped()) {
                            cmds.push(end(clip_id));
                        }
                        // One edit's commands: a key when keyed, else the
                        // value; a drag's step is its gesture's, anything
                        // else one step of its own.
                        let mut write = |cmds: &mut Vec<AppCommand>, value: f32, dragged: bool| {
                            let body = |c: &mut Vec<AppCommand>| if keyed {
                                keys.key_values(c, clip_id, node_id, &param, &[value]);
                            } else {
                                c.push(AppCommand::SetNodeParam { clip_id, node_id, param: name.clone(), value });
                            };
                            if !keyed {
                                *field = Some(value);
                            }
                            if dragged { body(cmds) } else { atomic_node_edit_batch(cmds, clip_id, body) }
                        };
                        if resp.dragged() || resp.clicked() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let t = ((pos.x - cx) / track_draw_w).clamp(0.0, 1.0);
                                v = t * 2.0 - 1.0;
                                write(cmds, v, resp.dragged());
                            }
                        }
                        if resp.double_clicked() || resp.secondary_clicked() {
                            v = 0.0;
                            write(cmds, 0.0, false);
                        }
                        let thumb_x = cx + (v + 1.0) / 2.0 * track_draw_w;
                        painter.circle_filled(egui::pos2(thumb_x, track_y + slider_h / 2.0), 4.0, accent);
                        let val_color = if v.abs() < 0.005 { theme.text_dim } else { accent };
                        painter.text(
                            egui::pos2(cx + track_draw_w + value_w - 2.0, track_y + slider_h / 2.0),
                            egui::Align2::RIGHT_CENTER,
                            format!("{v:+.2}"),
                            egui::FontId::monospace(10.0),
                            val_color,
                        );
                        // The key strip right of the title, as a wheel's is,
                        // so the rail keeps its length.
                        let key_w = if keys.state(&param).has_any_keyframes { 36.0 } else { 16.0 };
                        let strip_at = egui::Rect::from_min_size(egui::pos2(title_rect.right() + 2.0, title_rect.center().y - 9.0), egui::vec2(key_w, 18.0));
                        let strip = col_ui.allocate_new_ui(egui::UiBuilder::new().max_rect(strip_at), |ui| crate::widgets::scrubby_param::key_strip(ui, keys.state(&param))).inner;
                        keys.strip_commands(cmds, clip_id, node_id, &param, &[shown], strip);
                        label_y = track_y + slider_h + 4.0;
                    }
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "mask_stack", "masks", "Masks", &*masks);
        let mut masks_changed = false;
        // A slider drag is one gesture, one undo step (IC13), not one step
        // per frame: begun as it starts, ended on release, taken back on
        // Escape.
        let mut ended: Option<fn(ClipId) -> AppCommand> = None;

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.menu_rows(egui::RichText::new("+ Add Mask").size(11.0), |ui| {
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
                        shape: klipp_core::MaskShape::polygon(vec![
                                (0.5, 0.35), (0.65, 0.5),
                                (0.5, 0.65), (0.35, 0.5),
                            ]),
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
                keys: usize,
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
                let keys = track.map_or(0, |t| t.len());
                ParamInfo { name: param_name, display_val, has_keyframes, has_kf_at_playhead, key_time, range, keys }
            }).collect();

            ui.add_space(4.0);
            let mut delete_this = false;
            let mut duplicate_this = false;
            let mut move_to: Option<usize> = None;
            {
                let mask = &mut masks.masks[mask_idx];
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui.checkbox(&mut mask.enabled, "").changed() { masks_changed = true; }
                    let title = if mask.name.trim().is_empty() { format!("Mask {}", mask_idx + 1) } else { mask.name.clone() };
                    // Its copy and its place in the stack, from its menu (#74).
                    let header = ui.add(egui::Label::new(egui::RichText::new(format!("{title} \u{00B7} {shape_label}"))
                        .size(11.0).color(theme.text_accent)).sense(egui::Sense::click()));
                    header.context_menu_rows(|ui| crate::widgets::menu::menu_body(ui, |ui| {
                        if crate::widgets::menu::menu_item(ui, "Duplicate", "").clicked() {
                            duplicate_this = true;
                            ui.close_menu();
                        }
                        if mask_idx > 0 && crate::widgets::menu::menu_item(ui, "Move Up", "").clicked() {
                            move_to = Some(mask_idx - 1);
                            ui.close_menu();
                        }
                        if mask_idx + 1 < mask_count && crate::widgets::menu::menu_item(ui, "Move Down", "").clicked() {
                            move_to = Some(mask_idx + 1);
                            ui.close_menu();
                        }
                        // Its keys on the clip's key lane (#1947).
                        if !mask.keyed_tracks().is_empty() && crate::widgets::menu::menu_item(ui, "Show Keys on Timeline", "").clicked() {
                            cmds.push(AppCommand::ShowMaskKeysOnTimeline { clip_id, node_id, mask_index: mask_idx });
                            ui.close_menu();
                        }
                    }));
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
                        // Perspective (#238): follow a flat surface's four
                        // corners — a screen, a sign — instead of a move,
                        // size and turn. Off by default; a window tracked
                        // in perspective re-tracks the same way.
                        let persp_id = ui.id().with(("track_perspective", node_id, mask_idx));
                        let was_perspective = mask.keyframes.track_perspective.is_some();
                        let mut perspective = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(persp_id, || was_perspective));
                        if ui.selectable_label(perspective, egui::RichText::new("Perspective").size(10.0))
                            .on_hover_text("Track the window's four corners on a flat surface seen at an angle, so the window turns away with it")
                            .clicked()
                        {
                            perspective = !perspective;
                            ui.data_mut(|d| d.insert_temp(persp_id, perspective));
                        }
                        ui.add_space(4.0);
                        let label = if is_tracked { "\u{1F3AF} Re-track" } else { "\u{1F3AF} Track" };
                        let hover = if is_tracked {
                            "Re-run tracking from the playhead — overwrites the window's tracked keyframes"
                        } else {
                            "Track this region across the clip — keyframes the window's position, size and rotation as it follows"
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
                                clip_id, node_id, mask_index: mask_idx, perspective,
                            });
                        }
                    });
                });
            }
            // Its name and lock (#74).
            {
                let mask = &mut masks.masks[mask_idx];
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    let name = ui.add(egui::TextEdit::singleline(&mut mask.name).hint_text(format!("Mask {}", mask_idx + 1)).desired_width(110.0).font(egui::FontId::proportional(crate::theme::type_scale::CAPTION)));
                    // Typing is one step: begun as the field takes the
                    // keys, ended as it lets them go.
                    if name.gained_focus() {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if name.changed() {
                        masks_changed = true;
                    }
                    if name.lost_focus() {
                        ended = Some(|clip_id| AppCommand::EndNodeParamEdit { clip_id });
                    }
                    let tip = "Keep the mask from edits in the viewer";
                    let lock = ui.add(crate::widgets::toggle::toggle(&mut mask.locked)).on_hover_text(tip);
                    let label = ui.add(egui::Label::new(egui::RichText::new("Lock").size(crate::theme::type_scale::CAPTION).color(theme.text_dim)).sense(egui::Sense::click())).on_hover_text(tip);
                    if label.clicked() {
                        mask.locked = !mask.locked;
                    }
                    if lock.changed() || label.clicked() {
                        masks_changed = true;
                    }
                    if let klipp_core::MaskShape::Bezier { points } = &mask.shape {
                        ui.label(egui::RichText::new(format!("{} points", points.len())).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    }
                });
            }
            if duplicate_this {
                let copy = masks.masks[mask_idx].clone();
                masks.masks.push(copy);
                masks_changed = true;
            }
            if let Some(to) = move_to {
                cmds.push(AppCommand::MoveMaskShape { clip_id, node_id, from: mask_idx, to });
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

            // A freeform mask's shape over time (#74): keyed on or off, its
            // key at the playhead added or taken off, the keys either side
            // gone to, and how the shape leaves the key here.
            if matches!(masks.masks[mask_idx].shape, klipp_core::MaskShape::Bezier { .. }) {
                let caption = crate::theme::type_scale::CAPTION;
                let mask = masks.masks[mask_idx].clone();
                let keys = &mask.keyframes.path;
                let here = keys.iter().position(|k| (k.time_secs - clip_time_secs).abs() <= frame.tolerance);
                let mut changed: Option<klipp_core::Mask> = None;
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    let keyed = !keys.is_empty();
                    let diamond = if here.is_some() { "\u{25C6}" } else { "\u{25C7}" };
                    let colour = if here.is_some() { theme.text_accent } else if keyed { theme.accent_dim } else { theme.text_dim };
                    let tip = if here.is_some() { "Take off the shape key at the playhead" } else { "Key the shape at the playhead" };
                    if ui.add(egui::Button::new(egui::RichText::new(diamond).size(12.0).color(colour)).frame(false)).on_hover_text(tip).clicked() {
                        changed = Some(match here {
                            Some(i) => {
                                let mut m = mask.clone();
                                // The last key gone: the shape stays as it was there.
                                if m.keyframes.path.len() == 1 {
                                    m = m.with_path_keyed(false, clip_time_secs);
                                } else {
                                    m.keyframes.path.remove(i);
                                }
                                m
                            }
                            None => {
                                let now = mask.path_at(clip_time_secs).unwrap_or_default();
                                mask.with_path_keyed(true, clip_time_secs).with_path_at(clip_time_secs, now, frame.tolerance)
                            }
                        });
                    }
                    ui.label(egui::RichText::new("Shape").size(caption).color(theme.text_dim));
                    let before = keys.iter().rev().find(|k| k.time_secs < clip_time_secs - frame.tolerance).map(|k| k.time_secs);
                    let after = keys.iter().find(|k| k.time_secs > clip_time_secs + frame.tolerance).map(|k| k.time_secs);
                    if ui.add_enabled(before.is_some(), egui::Button::new(egui::RichText::new("\u{25C0}").size(caption)).frame(false)).on_hover_text("The shape key before").clicked() {
                        cmds.push(frame.seek_to(before.unwrap_or_default()));
                    }
                    if ui.add_enabled(after.is_some(), egui::Button::new(egui::RichText::new("\u{25B6}").size(caption)).frame(false)).on_hover_text("The shape key after").clicked() {
                        cmds.push(frame.seek_to(after.unwrap_or_default()));
                    }
                    if keyed {
                        ui.label(egui::RichText::new(format!("{} key{}", keys.len(), if keys.len() == 1 { "" } else { "s" })).size(caption).color(theme.text_dim));
                    }
                    if let Some(i) = here {
                        let ease = keys[i].ease;
                        let name = |e: klipp_core::mask_path::PathEase| match e {
                            klipp_core::mask_path::PathEase::Linear => "Linear",
                            klipp_core::mask_path::PathEase::Ease => "Ease",
                            klipp_core::mask_path::PathEase::Hold => "Hold",
                        };
                        egui::ComboBox::from_id_salt(("shape_ease", node_id, mask_idx))
                            .selected_text(egui::RichText::new(name(ease)).size(caption))
                            .width(70.0)
                            .show_ui(ui, |ui| crate::widgets::menu::menu_body(ui, |ui| {
                                for e in [klipp_core::mask_path::PathEase::Linear, klipp_core::mask_path::PathEase::Ease, klipp_core::mask_path::PathEase::Hold] {
                                    if crate::widgets::menu::menu_choice(ui, name(e), "", ease == e).clicked() && ease != e {
                                        let mut m = mask.clone();
                                        m.keyframes.path[i].ease = e;
                                        changed = Some(m);
                                    }
                                }
                            }))
                            .response
                            .on_hover_text("How the shape moves on from this key");
                    }
                    if keyed && ui.small_button(egui::RichText::new("Clear").size(caption)).on_hover_text("Take off every shape key, keeping the shape as it is here").clicked() {
                        changed = Some(mask.with_path_keyed(false, clip_time_secs));
                    }
                });
                if let Some(m) = changed {
                    masks.masks[mask_idx] = m;
                    masks_changed = true;
                }
            }

            for info in &param_infos {
                let mut picked = false;
                let row = ui.horizontal(|ui| {
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
                    picked |= diamond_resp.clicked();
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
                        diamond_resp.context_menu_rows(|ui| {
                            if ui.button("Clear all keyframes").clicked() {
                                cmds.push(AppCommand::ClearMaskKeyframes {
                                    clip_id, node_id, mask_index: mask_idx,
                                    param: info.name.to_string(),
                                });
                                ui.close_menu();
                            }
                        });
                    }
                    let explained = crate::param_focus::diamond_hover(info.keys, info.has_kf_at_playhead);
                    diamond_resp.on_hover_text(if let Some(text) = &explained { text.as_str() } else if info.has_kf_at_playhead {
                        "Remove keyframe at playhead"
                    } else {
                        "Add keyframe at playhead"
                    });
                    ui.label(egui::RichText::new(param_label(info.name))
                        .size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    ui.add_space(4.0);
                    let mut val = info.display_val;
                    let resp = ui.add(egui::Slider::new(&mut val, info.range.clone())
                        .show_value(true)
                        .custom_formatter(|v, _| format!("{:.3}", v))
                        .min_decimals(3)
                    );
                    // Begun before the drag's first write (a key, or the
                    // stack sent below), so its step starts from before it.
                    let (began, end) = gesture_of(ui, resp.drag_started(), resp.drag_stopped());
                    if began {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    ended = ended.or(end);
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
                // The row as its track, for the Inspector's focus (#2296).
                if info.keys > 0 {
                    let tag = crate::param_focus::Tag::new(crate::state::KeyframeParamKind::Mask(crate::mask_tracks::MaskTrack { node: node_id, mask: mask_idx, param: info.name.to_string() }), info.keys);
                    crate::param_focus::row_drawn(ui, &tag, row.response.rect, picked);
                }
            }

            {
                let mask = &mut masks.masks[mask_idx];
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    if ui.checkbox(&mut mask.invert, egui::RichText::new("Invert").size(crate::theme::type_scale::CAPTION)).changed() {
                        masks_changed = true;
                    }
                    // How it joins the masks above it (#74).
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Mode").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    egui::ComboBox::from_id_salt(("mask_combine", node_id, mask_idx))
                        .selected_text(egui::RichText::new(combine_label(mask.combine)).size(crate::theme::type_scale::CAPTION))
                        .width(84.0)
                        .show_ui(ui, |ui| crate::widgets::menu::menu_body(ui, |ui| {
                            for c in [klipp_core::MaskCombine::Add, klipp_core::MaskCombine::Subtract, klipp_core::MaskCombine::Intersect] {
                                if crate::widgets::menu::menu_choice(ui, combine_label(c), "", mask.combine == c).clicked() && mask.combine != c {
                                    mask.combine = c;
                                    masks_changed = true;
                                }
                            }
                        }));
                });
                // The soft edge inside the line (#74), beside the
                // feather's outside one.
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    ui.label(egui::RichText::new("Feather inside").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    let resp = ui.add(egui::Slider::new(&mut mask.feather_inner, 0.0..=1.0).custom_formatter(|v, _| format!("{:.3}", v)).min_decimals(3));
                    let (began, end) = gesture_of(ui, resp.drag_started(), resp.drag_stopped());
                    if began {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    ended = ended.or(end);
                    if resp.changed() {
                        masks_changed = true;
                    }
                });
            }
        }
        if masks_changed {
            cmds.push(AppCommand::SetNodeMasks { clip_id, node_id, masks: masks.clone() });
        }
        if let Some(end) = ended {
            cmds.push(end(clip_id));
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
        frame: FrameSelection,
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
        self.shared_row(ui, nid, "contrast", cmds, theme, frame);

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

    /// Draw `node`'s parameter `key` as its generated row from the
    /// snapshot the caller set (`generated`): whether it drew it, so the
    /// tabbed page draws its own row only when it didn't.
    fn shared_row(&self, ui: &mut egui::Ui, node: NodeId, key: &str, cmds: &mut Vec<AppCommand>, theme: &KlippTheme, frame: FrameSelection) -> bool {
        self.generated.as_ref().is_some_and(|g| g.row(ui, theme, node, key, cmds, frame))
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
            let (track_has_kfs, effective_on, key_time, bypass_keyframes) = {
                let node = graph.nodes.iter().find(|n| n.id == target_id);
                let track = node.and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == klipp_nodes::ENABLED_TRACK));
                let has_kfs = track.is_some_and(|t| !t.keyframes.is_empty());
                let selected = track.and_then(|t| crate::inspector::keyframe_selection::nearest(&t.keyframes, clip_time_secs, frame.tolerance).map(|i| &t.keyframes[i]));
                let on = selected.map(|k| k.value).or_else(|| track.and_then(|t| t.evaluate(clip_time_secs)))
                    .map(|v| v >= 0.5)
                    .unwrap_or(target_enabled);
                (has_kfs, on, selected.map_or(clip_time_secs, |k| k.time_secs), track.map(|t| t.keyframes.clone()).unwrap_or_default())
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
                    bypass_keys(ui, &bypass_keyframes, effective_on, target_id, clip_id, cmds, theme, frame, 13.0);
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
                    self.shared_row(ui, nid, "exposure", cmds, theme, frame);
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
                    self.shared_row(ui, nid, "contrast", cmds, theme, frame);
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
                    self.shared_row(ui, nid, "contrast_pivot", cmds, theme, frame);
                    self.shared_row(ui, nid, "clarity", cmds, theme, frame);
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
                    self.shared_row(ui, nid, "saturation", cmds, theme, frame);
                    self.shared_row(ui, nid, "vibrance", cmds, theme, frame);
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
                    // The Corrector's and Tone Controls' tone rows are
                    // registered like the rows above (#1420).
                    for param_name in ["highlights", "shadows", "whites", "blacks"] {
                        self.shared_row(ui, nid, param_name, cmds, theme, frame);
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
                self.shared_row(ui, nid, "temperature", cmds, theme, frame);

                // Tint: green → pink gradient
                self.shared_row(ui, nid, "tint", cmds, theme, frame);

                // Auto White Balance, and the eyedropper for when Auto WB
                // cannot tell a grey from a coloured surface: the user
                // clicks what they know is grey.
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if ui.add(egui::Button::new(
                        egui::RichText::new("Auto WB").size(11.0)
                    ).min_size(egui::vec2(60.0, 18.0))).clicked() {
                        cmds.push(AppCommand::AutoWhiteBalance { clip_id });
                    }
                    let pick = ui.add(egui::Button::image(Icon::Eyedropper.image(12.0).tint(theme.overlay(160))).min_size(egui::vec2(22.0, 18.0)))
                        .on_hover_text("Pick a grey: click something grey, white or black in the viewer");
                    if pick.clicked() {
                        cmds.push(AppCommand::PickWhiteBalance { clip_id });
                    }
                });
                ui.add_space(2.0);

                // Hue: rainbow spectrum
                self.shared_row(ui, nid, "hue", cmds, theme, frame);

                // Global Mix: subtle grey → white
                self.shared_row(ui, nid, "global_mix", cmds, theme, frame);
            } }
        }

        // ── Noise Reduction section ──
        // Its rows are the generated ones, from the snapshot the caller set,
        // as the Corrector's are; the hand-written rows only where there is
        // none (a nested grade, the #1391 boundary).
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
                    self.shared_row(ui, nid, "denoise_luma", cmds, theme, frame);
                    self.shared_row(ui, nid, "denoise_chroma", cmds, theme, frame);
                    self.shared_row(ui, nid, "denoise_radius", cmds, theme, frame);
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
                // The mask editor the node's own page draws, its tracked
                // masks marked as there.
                let tracked = self.tracked_masks.clone();
                Self::render_masks_body(ui, masks, node_id, clip_id, cmds, theme, frame, &tracked);
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.menu_rows(egui::RichText::new("+ Add Mask").size(11.0), |ui| {
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
                                shape: klipp_core::MaskShape::polygon(vec![
                                        (0.5, 0.35), (0.65, 0.5),
                                        (0.5, 0.65), (0.35, 0.5),
                                    ]),
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "log_format", "Input Colour Space", current);
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

    /// The generated grade layout's specialist editors (M3 step 11): the
    /// editor a row names by `id`, drawn by the per-node layout's own
    /// widget, unchanged. The wheels (`"wheels"`, any wheel's part) are
    /// drawn as one block, with saturation and brightness under each where
    /// the node's type has them, each keyable when `keys` (the node's
    /// tracks) are given. Whether it drew one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grade_editor(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        ty: &klipp_nodes::registry::NodeType,
        params: &mut NodeParams,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
        keys: Option<&klipp_nodes::NodeKeyframeTracks>,
    ) -> bool {
        match id {
            // An HDR Zones node's wheel is its picked zone's (#240).
            "wheels" if ty.kind == NodeKind::HdrZones => {
                self.hdr_zones_head(ui, node_id, params, keys.map(|tracks| NodeKeys { tracks, frame }), clip_id, cmds, theme);
            }
            "wheels" => {
                // Laid out from the width before the generated rows' indent,
                // as the old page lays its wheels out: the same diameter, so
                // a pixel moves a puck as far in either view.
                let mut rect = ui.available_rect_before_wrap();
                rect.min.x -= ui.spacing().indent;
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                    let avail_w = ui.available_width();
                    let columns = if avail_w >= 252.0 { 2 } else { 1 };
                    let cell_w = ((avail_w - 12.0 * (columns - 1) as f32) / columns as f32).min(194.0);
                    let wheel_d = (cell_w - 14.0).min(180.0);
                    let log_mode = params.wheels_log_mode.unwrap_or(false);
                    let with_sat_bri = ty.desc.params.iter().any(|p| p.key == "lift_sat");
                    self.wheel_block(ui, params, log_mode, node_id, clip_id, cmds, theme, (columns, cell_w, wheel_d), with_sat_bri, keys.map(|tracks| NodeKeys { tracks, frame }));
                });
            }
            "curves" => {
                self.curves_block(ui, params, true, node_id, clip_id, cmds, theme);
            }
            "huesat_curves" => self.huesat_block(ui, params, node_id, clip_id, cmds, theme),
            "colour_warper" => self.warper_block(ui, params, node_id, clip_id, cmds, theme),
            "mask_stack" => {
                let masks = params.masks.get_or_insert_with(Default::default);
                let tracked = self.tracked_masks.clone();
                Self::render_masks_body(ui, masks, node_id, clip_id, cmds, theme, frame, &tracked);
            }
            "lut_file" => lut_picker(ui, params, node_id, clip_id, cmds, theme),
            "camera_encoding" => self.show_input_colour_space(ui, clip_id, cmds, theme),
            "compound_body" => compound_summary(ui, params, theme),
            "key_mixer_inputs" => key_mixer_inputs(ui, params, node_id, clip_id, cmds, theme),
            _ => return false,
        }
        true
    }

    /// A grade node whose own widget draws every one of its parameters
    /// (an external matte, a difference key, a colour match, a layer
    /// mixer), drawn by that widget, unchanged, in place of its rows.
    /// Whether `kind` is one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grade_whole_node(
        &mut self,
        ui: &mut egui::Ui,
        kind: NodeKind,
        params: &mut NodeParams,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) -> bool {
        match kind {
            NodeKind::ExternalMatte => self.show_external_matte_widget(ui, node_id, params, clip_id, cmds, theme),
            NodeKind::DifferenceKey => self.show_difference_key_widget(ui, node_id, params, clip_id, cmds, theme),
            NodeKind::ColorMatch => self.show_color_match_widget(ui, node_id, params, clip_id, cmds, theme),
            NodeKind::LayerMixer => self.show_layer_mixer_widget(ui, node_id, params, clip_id, cmds, theme),
            _ => return false,
        }
        true
    }

    /// A Colour Warper's section, with its reset, and its wheel.
    pub(crate) fn warper_block(&mut self, ui: &mut egui::Ui, params: &mut NodeParams, node_id: NodeId, clip_id: ClipId, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
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

    /// A hue/sat curves node's six curves, each with its eyedropper,
    /// reset and axis strip.
    pub(crate) fn huesat_block(
        &mut self,
        ui: &mut egui::Ui,
        params: &mut NodeParams,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Unset curves are shown as their defaults and stored only by an
        // edit's command: drawing them wrote them into the project
        // unrecorded, and an edit written here as well left its step
        // starting from itself, so undo changed nothing (#1392).
        let mut edited = params.huesat.clone().unwrap_or_default();
        let huesat = &mut edited;

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
        let mut gesture: (bool, Option<fn(ClipId) -> AppCommand>) = (false, None);

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

                    // Eyedropper: on the three curves read by hue, a
                    // click in the viewer puts a range round the hue
                    // picked (#612). The others read brightness or
                    // saturation and keep the space empty.
                    let picks_hue = *curve_idx <= 2;
                    let ed_resp = ui.allocate_rect(
                        ui.cursor().with_max_x(ui.cursor().min.x + 22.0).with_max_y(ui.cursor().min.y + 18.0),
                        if picks_hue { egui::Sense::click() } else { egui::Sense::hover() },
                    );
                    if picks_hue {
                        let ed_tint = if ed_resp.hovered() {
                            theme.text_accent
                        } else {
                            theme.overlay(100)
                        };
                        let ed_icon_rect = egui::Rect::from_center_size(ed_resp.rect.center(), egui::vec2(12.0, 12.0));
                        Icon::Eyedropper.image(12.0).tint(ed_tint).paint_at(ui, ed_icon_rect);
                        let ed_resp = ed_resp.clone().on_hover_text("Pick a colour in the viewer: three points round its hue, the middle one to drag");
                        if ed_resp.clicked() {
                            cmds.push(AppCommand::PickHueCurve { clip_id, node_id, curve: *curve_idx as u8 });
                        }
                    }
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
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "huesat_curve", &format!("huesat/{curve_idx}"), label, &*points);
            let resp = curves_editor::curves_editor_in(
                ui, theme, *channel, points, curve_h, hist.as_ref(),
                // Relative: the pipeline fills these LUTs with
                // 0.5 and reads them as (v - 0.5) * 2.
                curves_editor::CurveSpace::Relative,
                x_axis,
            );
            if resp.changed { huesat_changed = true; }
            let (began, ended) = gesture_of(ui, resp.drag_started, resp.drag_ended);
            gesture.0 |= began;
            gesture.1 = gesture.1.or(ended);
            ui.add_space(4.0);
        }

        // A drag is one gesture, one undo step (IC13): begun as it starts,
        // ended on release, taken back on Escape.
        if gesture.0 {
            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        }
        if huesat_changed {
            // The command writes it, so its step starts from the grade
            // before the edit and an undo leaves it as it was (#1392).
            cmds.push(AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat: edited });
        }
        if let Some(end) = gesture.1 {
            cmds.push(end(clip_id));
        }
    }

    /// A curves node's RGB curves: the section with its reset, the
    /// channel pills and the curve editor over the viewer's histogram.
    /// Whether the section is open.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn curves_block(
        &mut self,
        ui: &mut egui::Ui,
        params: &mut NodeParams,
        enabled: bool,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) -> bool {
        // Section header: "RGB CURVES" with reset + enable
        let (reset, is_open) = section_header(ui, "RGB CURVES", enabled, theme);
        if is_open && reset {
            // Reset all curves to identity (2 points: [0,0] and [1,1]),
            // with Mix and Preserve Luma, as one undo step made by the
            // commands rather than written into the node here (#894).
            let mut curves = params.curves.clone().unwrap_or_default();
            curves.master = vec![[0.0, 0.0], [1.0, 1.0]];
            curves.red = vec![[0.0, 0.0], [1.0, 1.0]];
            curves.green = vec![[0.0, 0.0], [1.0, 1.0]];
            curves.blue = vec![[0.0, 0.0], [1.0, 1.0]];
            atomic_node_edit_batch(cmds, clip_id, |cmds| {
                cmds.push(AppCommand::SetNodeCurves { clip_id, node_id, curves });
                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_mix".into(), value: 1.0 });
                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_preserve_luma".into(), value: 0.0 });
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
        // Unset curves are shown as their defaults and stored only by an
        // edit's command: drawing them wrote them into the project
        // unrecorded, and an edit written here as well left its step
        // starting from itself, so undo changed nothing (#1392).
        let mut edited = params.curves.clone().unwrap_or_default();
        let curves = &mut edited;
        let curve_w = ui.available_width() - PAD * 2.0;
        let curve_h = curve_w.clamp(100.0, 500.0);
        let ch = self.active_curve_channel;
        let mut curves_changed = false;
        let mut gesture = (false, None);
        {
            let points = match ch {
                CurveChannel::Master => &mut curves.master, CurveChannel::Red => &mut curves.red,
                CurveChannel::Green => &mut curves.green, CurveChannel::Blue => &mut curves.blue,
            };
            ui.horizontal(|ui| {
                // The graph is `curve_w` wide, a pad in from each side:
                // left to fill the row, it ran to the panel's edge, under
                // the inspector's scroll bar (#2669).
                ui.set_max_width(PAD + curve_w);
                ui.add_space(PAD);
                let hist = self.histogram_for(ch).copied();
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "rgb_curve", &format!("curves/{ch:?}"), &format!("{ch:?}"), &*points);
                let resp = curves_editor::curves_editor_sized(
                    ui, theme, ch, points, curve_h, hist.as_ref(),
                );
                if resp.changed { curves_changed = true; }
                gesture = gesture_of(ui, resp.drag_started, resp.drag_ended);
            });
        }
        // A drag is one gesture, one undo step (IC13): begun as it starts,
        // ended on release, taken back on Escape.
        if gesture.0 {
            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        }
        if curves_changed {
            // The command writes it, so its step starts from the grade
            // before the edit and an undo leaves it as it was (#1392).
            cmds.push(AppCommand::SetNodeCurves { clip_id, node_id, curves: edited });
        }
        if let Some(end) = gesture.1 {
            cmds.push(end(clip_id));
        }
        ui.add_space(6.0);
        } // is_open
        is_open
    }

    /// A grade node's colour wheels, laid out in `columns` cells of
    /// `cell_w` with wheels `wheel_d` across: the three log wheels in log
    /// mode, else the four primaries, each with its saturation and
    /// brightness under it when `with_sat_bri` (a type that has them).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn wheel_block(
        &mut self,
        ui: &mut egui::Ui,
        params: &mut NodeParams,
        log_mode: bool,
        node_id: NodeId,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        (columns, cell_w, wheel_d): (usize, f32, f32),
        with_sat_bri: bool,
        keys: Option<NodeKeys>,
    ) {
        // Keep each wheel's saturation/brightness controls in its
        // own cell. Log wheels have no per-wheel sat/bri fields.
        let satbri_h = if with_sat_bri { 52.0 } else { 0.0 };
        let mut show_wheel = |ui: &mut egui::Ui, label: &str,
            state: &mut ColorWheelState,
            x: &mut Option<f32>, y: &mut Option<f32>, m: &mut Option<f32>,
            satbri: Option<(&mut Option<f32>, &mut Option<f32>)>,
            prefix: &str|
        {
            // The wheel's parts, keyed together (#891): shown as their keys
            // give them at the frame.
            let names = [format!("{prefix}_x"), format!("{prefix}_y"), format!("{prefix}_master")];
            let parts = [names[0].as_str(), names[1].as_str(), names[2].as_str()];
            let mut offset = [x.unwrap_or(0.0), y.unwrap_or(0.0)];
            let mut master = m.unwrap_or(0.0);
            if let Some(k) = keys {
                offset = [k.shown(parts[0], offset[0]), k.shown(parts[1], offset[1])];
                master = k.shown(parts[2], master);
            }
            let shown = [offset[0], offset[1], master];
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "colour_wheel", prefix, label, (offset, master));
            let resp = color_wheel::color_wheel_sized(
                ui, state, theme, label, &mut offset, &mut master, wheel_d, Some(cell_w), keys.map(|k| k.state(&parts)),
            );
            if resp.drag_started {
                cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
            }
            if resp.changed {
                // A click, a reset or a typed pill is one step; a drag's
                // step is its gesture's. A keyed wheel's edit is a key.
                let values = [offset[0], offset[1], master];
                match keys.filter(|k| k.keyed(&parts)) {
                    Some(k) if resp.dragging => k.key_values(cmds, clip_id, node_id, &parts, &values),
                    Some(k) => atomic_node_edit_batch(cmds, clip_id, |c| k.key_values(c, clip_id, node_id, &parts, &values)),
                    None => {
                        *x = Some(offset[0]); *y = Some(offset[1]); *m = Some(master);
                        if resp.dragging {
                            push_wheel_cmds(cmds, clip_id, node_id, prefix, offset, master);
                        } else {
                            atomic_node_edit_batch(cmds, clip_id, |c| push_wheel_cmds(c, clip_id, node_id, prefix, offset, master));
                        }
                    }
                }
            }
            // Released, the drag is one step; ended by Escape, taken back.
            if let (_, Some(end)) = gesture_of(ui, false, resp.drag_ended) {
                cmds.push(end(clip_id));
            }
            if let Some(k) = keys {
                k.strip_commands(cmds, clip_id, node_id, &parts, &shown, resp.key);
            }

            let Some((sat, bri)) = satbri else { return };
            // The sliders share the wheel's centre and width.
            let controls = ui.allocate_exact_size(egui::vec2(wheel_d, 40.0), egui::Sense::hover()).0;
            let mut controls_ui = ui.new_child(egui::UiBuilder::new()
                .max_rect(controls).layout(egui::Layout::top_down(egui::Align::Min)));
            {
                let ui = &mut controls_ui;
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
                // A keyable slider's key strip: its diamond and arrows.
                let key_w = if keys.is_some() { 36.0 } else { 0.0 };
                let slider_w = (wheel_d - LETTER_W - VALUE_W - key_w - 6.0).max(28.0);
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
                    let param = format!("{prefix}_{suffix}");
                    let mut v = keys.map_or(field.unwrap_or(0.0), |k| k.shown(&param, field.unwrap_or(0.0)));
                    let shown = v;
                    let keyed = keys.filter(|k| k.keyed(&[param.as_str()]));
                    #[cfg(any(test, feature = "row-recorder"))]
                    {
                        crate::widgets::row_recorder::next_parameter(&param);
                        crate::widgets::row_recorder::numeric(ui.next_auto_id(), "wheel_slider", letter, v, &(-1.0..=1.0), None, None, Some(2), "", None);
                    }
                    let mut strip = None;
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
                        if let Some(k) = keys {
                            let at = ui.allocate_exact_size(egui::vec2(key_w, 14.0), egui::Sense::hover()).0;
                            strip = Some(ui.allocate_new_ui(egui::UiBuilder::new().max_rect(at.expand2(egui::vec2(0.0, 2.0))), |ui| crate::widgets::scrubby_param::key_strip(ui, k.state(&[param.as_str()]))).inner);
                        }
                        r
                    }).inner;
                    if r.drag_started() {
                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                    }
                    if r.changed() {
                        let dragging = r.dragged() || r.drag_started() || r.drag_stopped();
                        match keyed {
                            // Keyed, what it shows is the animation: the edit is a key.
                            Some(k) if dragging => k.key_values(cmds, clip_id, node_id, &[param.as_str()], &[v]),
                            Some(k) => atomic_node_edit_batch(cmds, clip_id, |c| k.key_values(c, clip_id, node_id, &[param.as_str()], &[v])),
                            None => {
                                *field = Some(v);
                                if dragging {
                                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: param.clone(), value: v });
                                } else {
                                    // A press on the rail is a step of its own.
                                    push_atomic_node_edit(cmds, clip_id, node_id, &param, v);
                                }
                            }
                        }
                    }
                    if let (Some(k), Some(strip)) = (keys, strip) {
                        k.strip_commands(cmds, clip_id, node_id, &[param.as_str()], &[shown], strip);
                    }
                    // A right click or a double click puts it back to
                    // neutral, one step, as the wide layout's rails do. A
                    // slider senses drags only, and asking it for clicks
                    // too would hold a drag back to tell them apart, so the
                    // pointer is read over it.
                    let reset = ui.input(|i| {
                        i.pointer.interact_pos().is_some_and(|p| r.rect.contains(p))
                            && (i.pointer.button_clicked(egui::PointerButton::Secondary) || i.pointer.button_double_clicked(egui::PointerButton::Primary))
                    });
                    if reset {
                        match keyed {
                            Some(k) => atomic_node_edit_batch(cmds, clip_id, |c| k.key_values(c, clip_id, node_id, &[param.as_str()], &[0.0])),
                            None => {
                                *field = Some(0.0);
                                push_atomic_node_edit(cmds, clip_id, node_id, &param, 0.0);
                            }
                        }
                    }
                    if let (_, Some(end)) = gesture_of(ui, false, r.drag_stopped()) {
                        cmds.push(end(clip_id));
                    }
                }
            }
        };

        if log_mode {
            let mut wheels = [
                ("Low", &mut self.log_low_wheel, &mut params.log_low_x, &mut params.log_low_y, &mut params.log_low_master, "log_low"),
                ("Mid", &mut self.log_mid_wheel, &mut params.log_mid_x, &mut params.log_mid_y, &mut params.log_mid_master, "log_mid"),
                ("High", &mut self.log_high_wheel, &mut params.log_high_x, &mut params.log_high_y, &mut params.log_high_master, "log_high"),
            ];
            centred_wheel_rows(ui, 3, columns, cell_w, wheel_d + 56.0, |ui, i| {
                let (label, state, x, y, m, prefix) = &mut wheels[i];
                show_wheel(ui, label, state, x, y, m, None, prefix);
            });
        } else {
            let mut wheels = [
                ("Lift", &mut self.lift_wheel, &mut params.lift_x, &mut params.lift_y, &mut params.lift_master, &mut params.lift_sat, &mut params.lift_bri, "lift"),
                ("Gamma", &mut self.gamma_wheel, &mut params.gamma_x, &mut params.gamma_y, &mut params.gamma_master, &mut params.gamma_sat, &mut params.gamma_bri, "gamma"),
                ("Gain", &mut self.gain_wheel, &mut params.gain_x, &mut params.gain_y, &mut params.gain_master, &mut params.gain_sat, &mut params.gain_bri, "gain"),
                ("Offset", &mut self.offset_wheel, &mut params.offset_x, &mut params.offset_y, &mut params.offset_master, &mut params.offset_sat, &mut params.offset_bri, "offset"),
            ];
            centred_wheel_rows(ui, 4, columns, cell_w, wheel_d + 56.0 + satbri_h, |ui, i| {
                let (label, state, x, y, m, sat, bri, prefix) = &mut wheels[i];
                show_wheel(ui, label, state, x, y, m, with_sat_bri.then_some((sat, bri)), prefix);
            });
        }
    }

    /// The HDR Zones page (#240) as the per-node layout draws it: its head
    /// ([`Self::hdr_zones_head`]), the picked zone's Saturation, Boundary
    /// and Falloff (Global reaches everything and has neither), and the
    /// zone map. The generated layout draws the same head, its own rows
    /// for the picked zone, and the map.
    #[allow(clippy::too_many_arguments)]
    fn hdr_zones_page(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        params: &mut NodeParams,
        keyframes: &klipp_nodes::NodeKeyframeTracks,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        frame: FrameSelection,
    ) {
        self.hdr_zones_head(ui, node_id, params, Some(NodeKeys { tracks: keyframes, frame }), clip_id, cmds, theme);
        let picked = self.hdr_zone_picked(node_id);
        let mut zones = params.hdr_zones.clone().unwrap_or_default();
        let before = zones.clone();
        let name = |control: &str| format!("hdr_{}_{control}", klipp_nodes::params::HDR_ZONE_KEYS[picked]);
        let mut rows: Vec<(&str, String, &mut f32, std::ops::RangeInclusive<f32>, &str)> = vec![("Saturation", name("saturation"), &mut zones.saturation[picked], 0.0..=2.0, "")];
        if picked > 0 {
            rows.push(("Boundary", name("boundary"), &mut zones.boundary[picked], -8.0..=8.0, "stops"));
            rows.push(("Falloff", name("falloff"), &mut zones.falloff[picked], 0.0..=4.0, "stops"));
        }
        for (label, param, value, range, suffix) in rows {
            let mut v = *value;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::next_parameter(&param);
            if grade_slider(ui, label, &mut v, range, suffix, cmds, clip_id, theme) {
                *value = v;
                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param, value: v });
            }
        }
        if zones != before {
            params.hdr_zones = Some(zones.clone());
        }
        ui.add_space(8.0);
        hdr_zone_map(ui, &zones, picked, theme);
    }

    /// The zone an HDR Zones node's page shows (#240): the one picked from
    /// its zone menu, Global until one is.
    pub(crate) fn hdr_zone_picked(&self, node_id: NodeId) -> usize {
        self.hdr_zone_pick.get(&node_id).copied().unwrap_or(0).min(klipp_color::grade_model::hdr_zones::ZONES - 1)
    }

    /// An HDR Zones page's head (#240): the zone menu, the picked zone's
    /// colour wheel (its puck the colour, its Y the exposure in stops,
    /// keyed together when `keys` are given), and Reset zone.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn hdr_zones_head(
        &mut self,
        ui: &mut egui::Ui,
        node_id: NodeId,
        params: &mut NodeParams,
        keys: Option<NodeKeys>,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        use klipp_color::grade_model::hdr_zones::ZONE_NAMES;
        let mut zones = params.hdr_zones.clone().unwrap_or_default();
        let before = zones.clone();
        let picked = self.hdr_zone_picked(node_id);
        ui.add_space(4.0);
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "hdr_zone", "Zone", picked);
        let picked = ui
            .horizontal(|ui| crate::widgets::segmented::segmented(ui, &ZONE_NAMES, picked, theme, egui::FontId::proportional(10.0), 20.0))
            .inner
            .unwrap_or(picked);
        self.hdr_zone_pick.insert(node_id, picked);
        let key = klipp_nodes::params::HDR_ZONE_KEYS[picked];
        let name = |control: &str| format!("hdr_{key}_{control}");
        let (px, py, pe) = (name("colour_x"), name("colour_y"), name("exposure"));
        let parts = [px.as_str(), py.as_str(), pe.as_str()];

        // The wheel: puck and Y keyed together, as a Lift wheel's are.
        let shown_of = |param: &str, stored: f32| keys.map_or(stored, |k| k.shown(param, stored));
        let mut offset = [shown_of(parts[0], zones.colour_x[picked]), shown_of(parts[1], zones.colour_y[picked])];
        let mut exposure = shown_of(parts[2], zones.exposure[picked]);
        let shown = [offset[0], offset[1], exposure];
        ui.add_space(4.0);
        // Whole pixels, as the Lift wheels are laid out: the same pixel
        // moves the puck as far wherever the page is drawn.
        let width = (ui.available_width() / 2.0).floor() * 2.0;
        // 128 across, its radius a power of two: a drag of whole pixels
        // moves the puck by exactly that over the radius.
        let wheel_d = if width >= 168.0 { 128.0 } else { 64.0 };
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "colour_wheel", &format!("hdr_{key}"), "Colour", (offset, exposure));
        let at = ui.cursor().min;
        let cell = egui::Rect::from_min_size(egui::pos2(at.x.ceil(), at.y.ceil()), egui::vec2(width - 2.0, ui.available_height().max(1.0)));
        let resp = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                color_wheel::color_wheel_spanned(ui, &mut self.hdr_zone_wheel, theme, "Colour", &mut offset, &mut exposure, wheel_d, Some(width - 2.0), keys.map(|k| k.state(&parts)), 4.0)
            })
            .inner;
        if resp.drag_started {
            cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        }
        if resp.changed {
            let values = [offset[0], offset[1], exposure];
            let push = |c: &mut Vec<AppCommand>| {
                for (p, v) in parts.iter().zip(values) {
                    c.push(AppCommand::SetNodeParam { clip_id, node_id, param: (*p).into(), value: v });
                }
            };
            match keys.filter(|k| k.keyed(&parts)) {
                Some(k) if resp.dragging => k.key_values(cmds, clip_id, node_id, &parts, &values),
                Some(k) => atomic_node_edit_batch(cmds, clip_id, |c| k.key_values(c, clip_id, node_id, &parts, &values)),
                None => {
                    (zones.colour_x[picked], zones.colour_y[picked], zones.exposure[picked]) = (offset[0], offset[1], exposure);
                    if resp.dragging {
                        push(cmds);
                    } else {
                        atomic_node_edit_batch(cmds, clip_id, push);
                    }
                }
            }
        }
        if let (_, Some(end)) = gesture_of(ui, false, resp.drag_ended) {
            cmds.push(end(clip_id));
        }
        if let Some(k) = keys {
            k.strip_commands(cmds, clip_id, node_id, &parts, &shown, resp.key);
        }

        // Reset zone: its every value back as a new node has it, one step.
        // Start from: a preset's every zone, one step.
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            egui::ComboBox::from_id_salt(("hdr_zones_start", node_id))
                .selected_text("Start from")
                .width(150.0)
                .show_ui(ui, |ui| {
                    for (preset, about, settings) in klipp_core::HdrZonesSettings::presets() {
                        if ui.selectable_label(false, preset).on_hover_text(about).clicked() {
                            let writes = hdr_zones_writes(&settings);
                            zones = settings;
                            atomic_node_edit_batch(cmds, clip_id, |c| {
                                for (param, value) in writes {
                                    c.push(AppCommand::SetNodeParam { clip_id, node_id, param, value });
                                }
                            });
                        }
                    }
                });
            if ui.small_button("Reset zone").on_hover_text("Put this zone's colour, exposure, saturation and reach back").clicked() {
                let fresh = klipp_core::HdrZonesSettings::default();
                let mut values = vec![(name("exposure"), fresh.exposure[picked]), (name("colour_x"), 0.0), (name("colour_y"), 0.0), (name("saturation"), fresh.saturation[picked])];
                if picked > 0 {
                    values.push((name("boundary"), fresh.boundary[picked]));
                    values.push((name("falloff"), fresh.falloff[picked]));
                }
                (zones.exposure[picked], zones.colour_x[picked], zones.colour_y[picked], zones.saturation[picked]) = (fresh.exposure[picked], 0.0, 0.0, fresh.saturation[picked]);
                zones.boundary[picked] = fresh.boundary[picked];
                zones.falloff[picked] = fresh.falloff[picked];
                atomic_node_edit_batch(cmds, clip_id, |c| {
                    for (param, value) in values {
                        c.push(AppCommand::SetNodeParam { clip_id, node_id, param, value });
                    }
                });
            }
        });
        ui.add_space(4.0);
        if zones != before {
            params.hdr_zones = Some(zones);
        }
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
        #[cfg(any(test, feature = "row-recorder"))]
        let _recording_scope = crate::widgets::row_recorder::scope(format!("grade/{kind:?}"));
        node_header(ui, kind, *enabled, keyframes, node_id, clip_id, cmds, theme, frame);

        match kind {
            NodeKind::Exposure => {
                let mut val = params.exposure.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("exposure");
                if grade_slider(ui, "Exposure", &mut val, -3.0..=3.0, "stops", cmds, clip_id, theme) {
                    params.exposure = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "exposure".into(), value: val });
                }
            }
            NodeKind::Contrast => {
                let mut val = params.contrast.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("contrast");
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
                let mut shown = pivot_shown(
                    params.contrast_pivot.unwrap_or(klipp_color::grade_model::DEFAULT_CONTRAST_PIVOT),
                );
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("contrast_pivot");
                if grade_slider(
                    ui, "Pivot", &mut shown,
                    pivot_shown(klipp_color::grade_model::MIN_CONTRAST_PIVOT)
                        ..=pivot_shown(klipp_color::grade_model::MAX_CONTRAST_PIVOT),
                    "", cmds, clip_id, theme,
                ) {
                    let linear = pivot_linear(shown).clamp(
                        klipp_color::grade_model::MIN_CONTRAST_PIVOT,
                        klipp_color::grade_model::MAX_CONTRAST_PIVOT,
                    );
                    params.contrast_pivot = Some(linear);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "contrast_pivot".into(), value: linear });
                }
                let mut cval = params.clarity.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("clarity");
                if grade_slider(ui, "Clarity", &mut cval, -1.0..=1.0, "", cmds, clip_id, theme) {
                    params.clarity = Some(cval);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "clarity".into(), value: cval });
                }
            }
            NodeKind::Saturation => {
                let mut val = params.saturation.unwrap_or(1.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("saturation");
                if grade_slider(ui, "Saturation", &mut val, 0.0..=2.0, "", cmds, clip_id, theme) {
                    params.saturation = Some(val);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: val });
                }
                let mut vval = params.vibrance.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("vibrance");
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
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter(param_name);
                    if grade_slider(ui, label, &mut val, -1.0..=1.0, "", cmds, clip_id, theme) {
                        params.apply_override(param_name, val);
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: param_name.into(), value: val });
                    }
                }
            }
            NodeKind::ColorWheels => {
                let avail_w = ui.available_width();
                let columns = if wide_layout && avail_w >= 504.0 { 4 }
                    else if avail_w >= 252.0 { 2 } else { 1 };
                // Include the readouts, padding and inter-cell gaps in the
                // budget. Explicit rows cannot unexpectedly wrap one wheel.
                let cell_w = ((avail_w - 12.0 * (columns - 1) as f32)
                    / columns as f32).min(194.0);
                let wheel_d = (cell_w - 14.0).min(180.0);

                // Linear / Log mode toggle tabs
                let log_mode = params.wheels_log_mode.unwrap_or(false);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "wheels_log_mode", "Linear / Log (HDR)", log_mode);
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
                wheel_shape_sliders(ui, params, log_mode, node_id, cmds, clip_id, theme);
                ui.add_space(4.0);

                self.wheel_block(ui, params, log_mode, node_id, clip_id, cmds, theme, (columns, cell_w, wheel_d), true, Some(NodeKeys { tracks: keyframes, frame }));
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
                            #[cfg(any(test, feature = "row-recorder"))]
                            crate::widgets::row_recorder::next_parameter(sat_name);
                            if inspector_row(ui, "Saturation", &mut s, -1.0..=1.0, 0.0, "", theme, cmds, clip_id) {
                                params.apply_override(sat_name, s);
                                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: sat_name.into(), value: s });
                            }
                            let mut b = bri_val;
                            #[cfg(any(test, feature = "row-recorder"))]
                            crate::widgets::row_recorder::next_parameter(bri_name);
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
                let is_open = self.curves_block(ui, params, *enabled, node_id, clip_id, cmds, theme);
                if is_open {
                // Preserve Luma checkbox
                ui.horizontal(|ui| {
                    ui.add_space(PAD);
                    let mut preserve = params.curves_preserve_luma.unwrap_or(false);
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "curves_preserve_luma", "Preserve Luma", preserve);
                    if ui.checkbox(&mut preserve, egui::RichText::new("Preserve Luma").size(11.0)
                        .color(theme.overlay(140))).changed()
                    {
                        // One undo step, and the viewer told (#894).
                        push_atomic_node_edit(cmds, clip_id, node_id, "curves_preserve_luma", if preserve { 1.0 } else { 0.0 });
                    }
                });
                ui.add_space(2.0);

                // Mix slider — same style as Color tab
                let mut mix = params.curves_mix.unwrap_or(1.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("curves_mix");
                if inspector_row(ui, "Mix", &mut mix, 0.0..=1.0, 1.0, "", theme, cmds, clip_id) {
                    params.curves_mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "curves_mix".into(), value: mix });
                }
                } // is_open
            }
            NodeKind::HueSatCurves => {
                self.huesat_block(ui, params, node_id, clip_id, cmds, theme);

                // No Preserve Luma here: it is the RGB curves' setting, which
                // hue/sat curves never read, and on a Corrector it toggled the
                // RGB curves from this tab (#1039).
                ui.add_space(6.0);

                // Mix slider
                let mut mix = params.huesat_mix.unwrap_or(1.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("huesat_mix");
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
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter(pname);
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
                // Edited as a copy and sent once when it changed (#894): the
                // command makes the change, so its undo step has the value
                // from before, and an open panel sends nothing.
                let before = params.qualifier.clone().unwrap_or_default();
                let mut edited = before.clone();
                let qual = &mut edited;
                ui.horizontal(|ui| {
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "qualifier.enabled", "Enabled", qual.enabled);
                    ui.checkbox(&mut qual.enabled, "Enabled");
                    let pick = ui.add(egui::Button::image(Icon::Eyedropper.image(12.0).tint(theme.overlay(160))).min_size(egui::vec2(22.0, 18.0)))
                        .on_hover_text("Pick a colour: click it in the viewer to key it");
                    if pick.clicked() {
                        cmds.push(AppCommand::PickQualifierColour { clip_id, node_id });
                    }
                });
                if qual.enabled {
                    let mut hc = qual.hue_center;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.hue_center");
                    if grade_slider(ui, "Hue Center", &mut hc, 0.0..=360.0, "deg", cmds, clip_id, theme) { qual.hue_center = hc; }
                    let mut hw = qual.hue_width;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.hue_width");
                    if grade_slider(ui, "Hue Width", &mut hw, 0.0..=180.0, "deg", cmds, clip_id, theme) { qual.hue_width = hw; }
                    let mut hs = qual.hue_soft;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.hue_soft");
                    if grade_slider(ui, "Hue Soft", &mut hs, 0.0..=90.0, "deg", cmds, clip_id, theme) { qual.hue_soft = hs; }
                    ui.add_space(4.0);
                    let mut sl = qual.sat_low;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.sat_low");
                    if grade_slider(ui, "Sat Low", &mut sl, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_low = sl; }
                    let mut sh = qual.sat_high;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.sat_high");
                    if grade_slider(ui, "Sat High", &mut sh, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_high = sh; }
                    let mut ss = qual.sat_soft;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.sat_soft");
                    if grade_slider(ui, "Sat Soft", &mut ss, 0.0..=1.0, "", cmds, clip_id, theme) { qual.sat_soft = ss; }
                    ui.add_space(4.0);
                    let mut ll = qual.lum_low;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.lum_low");
                    if grade_slider(ui, "Lum Low", &mut ll, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_low = ll; }
                    let mut lh = qual.lum_high;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.lum_high");
                    if grade_slider(ui, "Lum High", &mut lh, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_high = lh; }
                    let mut ls = qual.lum_soft;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.lum_soft");
                    if grade_slider(ui, "Lum Soft", &mut ls, 0.0..=1.0, "", cmds, clip_id, theme) { qual.lum_soft = ls; }
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "qualifier.invert", "Invert", qual.invert);
                    ui.checkbox(&mut qual.invert, "Invert");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Matte Finesse").size(10.0).color(theme.text_dim));
                    let mut cb = qual.clean_black;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.clean_black");
                    if grade_slider(ui, "Clean Black", &mut cb, 0.0..=1.0, "", cmds, clip_id, theme) { qual.clean_black = cb; }
                    let mut cw = qual.clean_white;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.clean_white");
                    if grade_slider(ui, "Clean White", &mut cw, 0.0..=1.0, "", cmds, clip_id, theme) { qual.clean_white = cw; }
                    let mut sg = qual.shrink_grow;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.shrink_grow");
                    if grade_slider(ui, "Shrink / Grow", &mut sg, -20.0..=20.0, "px", cmds, clip_id, theme) { qual.shrink_grow = sg; }
                    let mut bl = qual.blur;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("qualifier.blur");
                    if grade_slider(ui, "Blur", &mut bl, 0.0..=50.0, "px", cmds, clip_id, theme) { qual.blur = bl; }
                }
                if edited != before {
                    cmds.push(AppCommand::SetNodeQualifier { clip_id, node_id, qualifier: edited });
                }
            }
            NodeKind::LumaKey => {
                // As the qualifier: a copy, sent once when it changed (#894).
                let before = params.luma_key.clone().unwrap_or_default();
                let mut edited = before.clone();
                let lk = &mut edited;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "luma_key.enabled", "Enabled", lk.enabled);
                ui.checkbox(&mut lk.enabled, "Enabled");
                if lk.enabled {
                    let mut low = lk.low;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("luma_key.low");
                    if grade_slider(ui, "Low", &mut low, 0.0..=1.0, "", cmds, clip_id, theme) { lk.low = low; }
                    let mut high = lk.high;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("luma_key.high");
                    if grade_slider(ui, "High", &mut high, 0.0..=1.0, "", cmds, clip_id, theme) { lk.high = high; }
                    let mut soft = lk.soft;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("luma_key.soft");
                    if grade_slider(ui, "Soft", &mut soft, 0.0..=0.5, "", cmds, clip_id, theme) { lk.soft = soft; }
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "luma_key.invert", "Invert", lk.invert);
                    ui.checkbox(&mut lk.invert, "Invert");
                }
                if edited != before {
                    cmds.push(AppCommand::SetNodeLumaKey { clip_id, node_id, luma_key: edited });
                }
            }
            NodeKind::HdrZones => self.hdr_zones_page(ui, node_id, params, keyframes, clip_id, cmds, theme, frame),
            NodeKind::SoftClip => {
                // As the qualifier: a copy, sent once when it changed (#894).
                let before = params.soft_clip.clone().unwrap_or_default();
                let mut edited = before.clone();
                let sc = &mut edited;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "soft_clip.enabled", "Enabled", sc.enabled);
                ui.checkbox(&mut sc.enabled, "Enabled");
                if sc.enabled {
                    let mut threshold = sc.threshold;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("soft_clip.threshold");
                    if grade_slider(ui, "Threshold", &mut threshold, 0.0..=1.0, "", cmds, clip_id, theme) { sc.threshold = threshold; }
                    let mut knee = sc.knee;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("soft_clip.knee");
                    if grade_slider(ui, "Knee", &mut knee, 0.0..=0.5, "", cmds, clip_id, theme) { sc.knee = knee; }
                }
                if edited != before {
                    cmds.push(AppCommand::SetNodeSoftClip { clip_id, node_id, soft_clip: edited });
                }
            }
            NodeKind::Mask => {
                // Drawn on a copy: an edit reaches the grade only through
                // its `SetNodeMasks`, so the step's "before" really is
                // before it and an undo takes it back. Written in place as
                // well, the step started from the edit itself (as the
                // curves' did, #1392).
                let mut masks = params.masks.clone().unwrap_or_default();
                // The masks a tracker drives, for Re-track and Untrack (#1398).
                let tracked = self.tracked_masks.clone();
                Self::render_masks_body(ui, &mut masks, node_id, clip_id, cmds, theme, frame, &tracked);
            }
            NodeKind::FilmGrain => {
                // As the qualifier: a copy, sent once when it changed (#894).
                // It used to be written in place with no command at all, so
                // the viewer and undo never heard of it.
                let before = params.grain.clone().unwrap_or_default();
                let mut edited = before.clone();
                let grain = &mut edited;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "grain.enabled", "Enabled", grain.enabled);
                ui.checkbox(&mut grain.enabled, "Enabled");
                if grain.enabled {
                    let mut a = grain.amount;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("grain.amount");
                    if grade_slider(ui, "Amount", &mut a, 0.0..=1.0, "", cmds, clip_id, theme) { grain.amount = a; }
                    let mut s = grain.size;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("grain.size");
                    if grade_slider(ui, "Size", &mut s, 0.5..=3.0, "", cmds, clip_id, theme) { grain.size = s; }
                    let mut sh = grain.shadows;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("grain.shadows");
                    if grade_slider(ui, "Shadows", &mut sh, 0.0..=1.0, "", cmds, clip_id, theme) { grain.shadows = sh; }
                    let mut hi = grain.highlights;
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::next_parameter("grain.highlights");
                    if grade_slider(ui, "Highlights", &mut hi, 0.0..=1.0, "", cmds, clip_id, theme) { grain.highlights = hi; }
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "grain.monochrome", "Monochrome", grain.monochrome);
                    ui.checkbox(&mut grain.monochrome, "Monochrome");
                }
                if edited != before {
                    cmds.push(AppCommand::SetNodeFilmGrain { clip_id, node_id, grain: edited });
                }
            }
            NodeKind::Lut3D => {
                lut_picker(ui, params, node_id, clip_id, cmds, theme);
                let mut opacity = params.lut_opacity.unwrap_or(1.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("lut_opacity");
                if grade_slider(ui, "Opacity", &mut opacity, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.lut_opacity = Some(opacity);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "lut_opacity".into(), value: opacity });
                }
                // Primaries tags. `.cube` and friends carry no
                // standardised colorspace metadata, so the loader
                // defaults to Rec.709 primaries on both sides; the user
                // re-tags here when the LUT was built for other
                // primaries (e.g. an AP1 LUT). The lookup is on the
                // sRGB encoding whatever the tag; the tag only chooses
                // the matrices around it (#422).
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
                // What the executor does with an unset mix: Blend halves
                // the two inputs, ApplyMatte applies the grade in full. The
                // slider showed 1.0 for both (#1039).
                let unset = if *kind == NodeKind::Blend { 0.5 } else { 1.0 };
                let mut mix = params.mix.unwrap_or(unset);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("mix");
                if grade_slider(ui, "Mix", &mut mix, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.mix = Some(mix);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "mix".into(), value: mix });
                }
            }
            NodeKind::Denoise => {
                let mut luma = params.denoise_luma.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("denoise_luma");
                if grade_slider(ui, "Luma Strength", &mut luma, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.denoise_luma = Some(luma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_luma".into(), value: luma });
                }
                let mut chroma = params.denoise_chroma.unwrap_or(0.0);
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("denoise_chroma");
                if grade_slider(ui, "Chroma Strength", &mut chroma, 0.0..=1.0, "", cmds, clip_id, theme) {
                    params.denoise_chroma = Some(chroma);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_chroma".into(), value: chroma });
                }
                let mut radius = params.denoise_radius.unwrap_or(3) as f32;
                #[cfg(any(test, feature = "row-recorder"))]
                crate::widgets::row_recorder::next_parameter("denoise_radius");
                if grade_slider(ui, "Radius", &mut radius, 1.0..=DENOISE_RADIUS_MAX, "px", cmds, clip_id, theme) {
                    let r = radius.round() as u32;
                    params.denoise_radius = Some(r);
                    cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "denoise_radius".into(), value: r as f32 });
                }
            }
            NodeKind::ColorWarper => {
                self.warper_block(ui, params, node_id, clip_id, cmds, theme);
            }
            NodeKind::CombineMattes => {
                let current = params.matte_combine_op.unwrap_or_default();
                let mut chosen = current;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Combine").small().color(theme.text_dim));
                    #[cfg(any(test, feature = "row-recorder"))]
                    crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "matte_combine_op", "Combine", current);
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
            NodeKind::KeyMixer => {
                key_mixer_inputs(ui, params, node_id, clip_id, cmds, theme);
                let mixer = params.key_mixer.get_or_insert_with(Default::default);
                for (param, label, value, range) in [("key_mixer_output_gain", "Output gain", &mut mixer.output_gain, 0.0..=2.0), ("key_mixer_output_offset", "Output offset", &mut mixer.output_offset, -1.0..=1.0)] {
                    if grade_slider(ui, label, value, range, "", cmds, clip_id, theme) {
                        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: param.into(), value: *value });
                    }
                }
            }
            NodeKind::ExternalMatte => {
                self.show_external_matte_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::DifferenceKey => {
                // On a copy, as the generated view draws it: its edits
                // reach the grade through their commands, so a step starts
                // from the grade before them and nothing is stored on draw.
                let mut copy = params.clone();
                self.show_difference_key_widget(ui, node_id, &mut copy, clip_id, cmds, theme);
            }
            NodeKind::ColorMatch => {
                self.show_color_match_widget(ui, node_id, params, clip_id, cmds, theme);
            }
            NodeKind::LayerMixer => {
                // On a copy, as the generated view draws it: its edits
                // reach the grade through their commands, so a step starts
                // from the grade before them and nothing is stored on draw.
                let mut copy = params.clone();
                self.show_layer_mixer_widget(ui, node_id, &mut copy, clip_id, cmds, theme);
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
                cdl_buttons(ui, params, node_id, clip_id);
            }
            NodeKind::Source | NodeKind::Output | NodeKind::Layer
            | NodeKind::RGBSplitter | NodeKind::RGBCombiner
            | NodeKind::MaskCutout
            | NodeKind::Reroute
            | NodeKind::Effect
            | NodeKind::EffectSegment
            | NodeKind::Frei0r
            // An OFX node's parameters are its effect's rows.
            | NodeKind::Ofx
            | NodeKind::SceneToDisplay
            | NodeKind::SidePicture
            | NodeKind::OutputAlpha
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
                compound_summary(ui, params, theme);
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "image_picker", "matte_image_path", "Choose image…", &params.matte_image_path);
        // A tracked subject (#239): where its matte stands, and its one
        // action; its own channel is the matte, so only Invert follows.
        let tracked = params.matte_image_path.as_ref().filter(|p| klipp_nodes::matte_sequence::is_sequence(p)).cloned();
        if let Some(dir) = &tracked {
            cmds.extend(crate::subject_track::card_ui(ui, crate::subject_track::card(dir).as_ref(), clip_id, theme));
            ui.add_space(4.0);
        }
        // Bound by the clip's media and a model (#1881).
        let subject = params.matte_subject;
        if let Some(model) = subject {
            cmds.extend(crate::subject_track::card_ui(ui, crate::subject_track::subject_card(clip_id, model.index() as u32).as_ref(), clip_id, theme));
            ui.add_space(4.0);
        }
        let tracked = tracked.or_else(|| subject.map(|_| std::path::PathBuf::new()));
        // File picker.
        if tracked.is_none() { ui.horizontal(|ui| {
            ui.add_space(8.0);
            let label = match &params.matte_image_path {
                Some(p) => p.file_name().map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string_lossy().to_string()),
                None => "(none selected)".to_string(),
            };
            ui.label(
                egui::RichText::new(label).small().color(theme.text_dim)
            );
        }); }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("Choose image…").clicked() {
                crate::file_dialog::spawn_pick(
                    || rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"]).pick_file(),
                    move |path| vec![AppCommand::SetExternalMattePath { clip_id, node_id, path: Some(path) }],
                );
            }
            if (params.matte_image_path.is_some() || params.matte_subject.is_some())
                && ui.button("Clear").clicked()
            {
                params.matte_image_path = None;
                params.matte_subject = None;
                cmds.push(AppCommand::SetExternalMattePath {
                    clip_id, node_id, path: None,
                });
            }
        });

        ui.add_space(4.0);

        // Channel selector.
        let current_channel = params.matte_channel.unwrap_or_default();
        let invert = params.matte_invert.unwrap_or(false);
        if tracked.is_none() { ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Source").small().color(theme.text_dim));
            let mut new_channel = current_channel;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "matte_channel", "Source", current_channel);
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
        }); }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let mut inv = invert;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "matte_invert", "Invert", inv);
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "difference_key.enabled", "Enabled", enabled);
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
                crate::file_dialog::spawn_pick(
                    || rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"]).pick_file(),
                    move |path| vec![AppCommand::SetNodeDifferenceKeyPlate { clip_id, node_id, plate_path: Some(path) }],
                );
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "plate_picker", "difference_key.plate_path", "Plate", &dk.plate_path);
        let current_space = dk.match_space;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Match").small().color(theme.text_dim));
            let mut new_space = current_space;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "difference_key.match_space", "Match", current_space);
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::next_parameter("difference_key.tolerance");
        let defaults = klipp_core::color_grade::DifferenceKeySettings::default();
        if grade_slider_resetting(ui, "Tolerance", &mut tolerance, 0.0..=1.0, defaults.tolerance, "", cmds, clip_id, theme) {
            dk.tolerance = tolerance;
            cmds.push(AppCommand::SetNodeDifferenceKey {
                clip_id, node_id, difference_key: dk.clone(),
            });
        }
        let mut soft = dk.soft;
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::next_parameter("difference_key.soft");
        if grade_slider_resetting(ui, "Soft", &mut soft, 0.0..=0.5, defaults.soft, "", cmds, clip_id, theme) {
            dk.soft = soft;
            cmds.push(AppCommand::SetNodeDifferenceKey {
                clip_id, node_id, difference_key: dk.clone(),
            });
        }

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let mut invert = dk.invert;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", "difference_key.invert", "Invert", invert);
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "layer_mixer", "layer_mixer", "Inputs", &*mixer);
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
            #[cfg(any(test, feature = "row-recorder"))]
            let _record_scope = crate::widgets::row_recorder::scope(format!("input/{port}"));
            let input_settings = mixer.inputs[port];
            let label = if port == 0 { "Input 1 (top)".to_string() }
                else { format!("Input {}", port + 1) };
            ui.label(egui::RichText::new(label).strong().small());
            let mut opacity = input_settings.opacity;
            #[cfg(any(test, feature = "row-recorder"))]
            {
                crate::widgets::row_recorder::next_parameter("layer_mixer.opacity");
                crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", "Opacity", opacity, &(0.0..=1.0), None, None, Some(2), "", None);
            }
            let resp = ui.add(
                egui::Slider::new(&mut opacity, 0.0..=1.0)
                    .text("Opacity")
                    .fixed_decimals(2),
            );
            let mut blend = input_settings.blend_mode;
            let mut blend_changed = false;
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", "layer_mixer.blend_mode", "Blend", blend);
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
        // An unset warper is shown as its default and stored only through a
        // sector's command (`SetNodeParam`, which creates it): drawing it
        // wrote it into the project unrecorded (#1392).
        let mut edited = params.warper_params.clone().unwrap_or_default();
        let warper = &mut edited;
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "colour_warper", "warper_params", "Colour Warper", &*warper);

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
        let painter = ui.painter().clone();

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
        // Released, the drag is one step; ended by Escape, taken back.
        if let (_, Some(end)) = gesture_of(ui, false, resp.drag_stopped()) {
            cmds.push(end(clip_id));
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
                        ui.menu_rows("Delete...", |ui| {
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
                self.preset_save_error = None;
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
                    if let Err(e) = presets::save_tool_preset(&preset) {
                        self.preset_save_error = Some(e);
                        return;
                    }
                    self.preset_save_error = None;
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
            if let Some(e) = &self.preset_save_error {
                ui.label(egui::RichText::new(e).size(10.0).color(theme.marker_warning));
            }
        }
        ui.add_space(2.0);
    }
}

// ── Mockup-matching helpers ──────────────────────────────────────────────────

const PAD: f32 = 14.0;
/// The largest denoise radius that does anything: the kernel is capped at
/// ten pixels across (`denoise_sigmas`), which a radius of five reaches, so
/// the slider's 6 and 7 behaved as 5 (#1039).
const DENOISE_RADIUS_MAX: f32 = 5.0;
const ROW_H: f32 = 28.0;

/// Section header: "EXPOSURE" with enabled dot and reset icon.
/// Returns true if the reset button was clicked.
/// Collapsible section header matching the inspector card style.
/// Returns (reset_clicked, is_open).
/// A Compound node's summary: how many nodes it holds, and how to open it.
pub(crate) fn compound_summary(ui: &mut egui::Ui, params: &NodeParams, theme: &KlippTheme) {
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
        egui::RichText::new("Double-click it in the graph to open it")
            .small()
            .color(theme.text_dim),
    );
}

/// A Corrector's ASC CDL import (onto its wheels and saturation) and
/// export (of them).
/// An imported CDL's writes: its slope, offset and power on the node's
/// wheels and its saturation, as one undo step.
fn cdl_commands(cdl: &klipp_grade_model::cdl::Cdl, clip_id: ClipId, node_id: NodeId) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    atomic_node_edit_batch(&mut cmds, clip_id, |cmds| {
        let (lift, gamma, gain, offset) = cdl.wheel_amounts();
        for (name, amounts) in [("lift", lift), ("gamma", gamma), ("gain", gain), ("offset", offset)] {
            let (x, y, m) = klipp_color::rgb_to_wheel_offset((amounts[0], amounts[1], amounts[2]));
            for (suffix, v) in [("x", x), ("y", y), ("master", m)] {
                cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("{name}_{suffix}"), value: v });
            }
        }
        cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: "saturation".into(), value: cdl.saturation });
    });
    cmds
}

pub(crate) fn cdl_buttons(ui: &mut egui::Ui, params: &NodeParams, node_id: NodeId, clip_id: ClipId) {
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
                        tracing::info!("Imported CDL from {}", path.display());
                        cdl_commands(&cdl, clip_id, node_id)
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

/// A LUT node's file: its name, and Browse, the bundled looks and Clear.
pub(crate) fn lut_picker(ui: &mut egui::Ui, params: &NodeParams, node_id: NodeId, clip_id: ClipId, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::control(ui.next_auto_id(), "lut_picker", "lut_path", "LUT", &params.lut_path);
    if let Some(path) = &params.lut_path {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
        ui.label(egui::RichText::new(name).size(11.0)).on_hover_text(path.display().to_string());
    } else {
        ui.label(egui::RichText::new("No LUT loaded").size(10.0).color(theme.text_dim));
    }
    // Choosing the node's LUT (#892): a file, or one of the
    // bundled looks; Clear takes it off.
    ui.horizontal(|ui| {
        if ui.button("Browse\u{2026}").on_hover_text("Load a .cube, .3dl or .csp file into this node").clicked() {
            crate::file_dialog::spawn_pick(
                || rfd::FileDialog::new().set_title("LUT for this node").add_filter("LUT files", &["cube", "3dl", "csp"]).pick_file(),
                move |path| vec![AppCommand::SetNodeLut { clip_id, node_id, path: Some(path) }],
            );
        }
        if let Some(dir) = klipp_core::assets::bundled_luts_dir() {
            ui.menu_rows("Bundled", |ui| {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for (category, files) in bundled_luts(dir) {
                        ui.menu_rows(category, |ui| {
                            for path in files {
                                let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                if ui.selectable_label(params.lut_path.as_ref() == Some(&path), name).clicked() {
                                    cmds.push(AppCommand::SetNodeLut { clip_id, node_id, path: Some(path) });
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                });
            });
        }
        if params.lut_path.is_some() && ui.button("Clear").clicked() {
            cmds.push(AppCommand::SetNodeLut { clip_id, node_id, path: None });
        }
    });
}

/// Which of `node`'s masks a tracker of `clip` drives, by index: the masks
/// the mask list offers Re-track and Untrack (#1398).
pub(crate) fn tracked_masks_of(clip: &klipp_timeline::Clip, node: NodeId) -> Vec<usize> {
    clip.track_points.iter().filter_map(|tp| tp.linked_mask).filter(|(n, _)| *n == node).map(|(_, i)| i).collect()
}

/// A grade node's header: its name, its on switch, and the diamond that
/// keys the switch. Shared by the per-node layout and the generated one
/// (M3 step 11).
#[allow(clippy::too_many_arguments)]
pub(crate) fn node_header(
    ui: &mut egui::Ui,
    kind: &NodeKind,
    enabled: bool,
    keyframes: &klipp_nodes::NodeKeyframeTracks,
    node_id: NodeId,
    clip_id: ClipId,
    cmds: &mut Vec<AppCommand>,
    theme: &KlippTheme,
    frame: FrameSelection,
) {
    let clip_time_secs = frame.time_secs;
    let enabled_track = keyframes.tracks.iter().find(|t| t.param_name == klipp_nodes::ENABLED_TRACK);
    let bypass_track_has_kfs = enabled_track.is_some_and(|t| !t.keyframes.is_empty());
    let selected = enabled_track.and_then(|t| crate::inspector::keyframe_selection::nearest(&t.keyframes, clip_time_secs, frame.tolerance).map(|i| &t.keyframes[i]));
    let key_time = selected.map_or(clip_time_secs, |k| k.time_secs);
    let effective_on = selected.map(|k| k.value).or_else(|| enabled_track.and_then(|t| t.evaluate(clip_time_secs)))
        .map(|v| v >= 0.5)
        .unwrap_or(enabled);

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
            bypass_keys(ui, enabled_track.map_or(&[], |t| &t.keyframes), effective_on, node_id, clip_id, cmds, theme, frame, 12.0);
        });
    });
    // The frame on screen didn't draw this node as asked: why, as the
    // viewer's chip says (M4 step 10).
    if let Some(reason) = crate::panels::viewer_declines::reason(ui.ctx(), clip_id, node_id) {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(reason).size(11.0).color(theme.marker_warning));
        });
    }
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
}

/// A grade node's bypass key controls, drawn right to left as a number
/// row's are: the next-key caret, the diamond that adds or removes the key
/// at the frame, the previous-key caret (#1456). The carets seek, writing
/// nothing; the diamond's menu sets the key's interpolation (a switch
/// holds, so Step is the one offered) and clears the track.
#[allow(clippy::too_many_arguments)]
fn bypass_keys(ui: &mut egui::Ui, keys: &[klipp_core::Keyframe], effective_on: bool, node_id: NodeId, clip_id: ClipId, cmds: &mut Vec<AppCommand>, theme: &KlippTheme, frame: FrameSelection, size: f32) {
    use crate::inspector::keyframe_selection;
    let around = keyframe_selection::state(keys, frame.time_secs, frame.tolerance);
    let selected = keyframe_selection::nearest(keys, frame.time_secs, frame.tolerance).map(|i| &keys[i]);
    let key_time = selected.map_or(frame.time_secs, |k| k.time_secs);
    let param = || klipp_nodes::ENABLED_TRACK.to_string();
    let caret = |ui: &mut egui::Ui, icon: Icon, to: Option<f64>, cmds: &mut Vec<AppCommand>| {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), if to.is_some() { egui::Sense::click() } else { egui::Sense::hover() });
        let tint = if to.is_none() { theme.text_dim.gamma_multiply(0.4) } else if resp.hovered() { theme.text_accent } else { theme.text_dim };
        icon.image(8.0).tint(tint).paint_at(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(8.0, 8.0)));
        if let Some(t) = to.filter(|_| resp.clicked()) {
            cmds.push(frame.seek_to(t));
        }
    };
    if !keys.is_empty() {
        caret(ui, Icon::CaretRight, around.next_keyframe_time, cmds);
    }
    let diamond_color = if selected.is_some() {
        theme.text_accent
    } else if !keys.is_empty() {
        egui::Color32::from_rgba_premultiplied(255, 191, 0, 128)
    } else {
        theme.text_dim
    };
    let diamond_text = if selected.is_some() { "\u{25C6}" } else { "\u{25C7}" };
    let diamond_resp = ui.add(egui::Button::new(egui::RichText::new(diamond_text).size(size).color(diamond_color)).frame(false));
    if diamond_resp.clicked() {
        if selected.is_some() {
            cmds.push(AppCommand::RemoveKeyframe { clip_id, node_id, param: param(), time_secs: key_time });
        } else {
            cmds.push(AppCommand::AddKeyframe { clip_id, node_id, param: param(), time_secs: key_time, value: if effective_on { 1.0 } else { 0.0 } });
        }
    }
    if !keys.is_empty() {
        diamond_resp.context_menu_rows(|ui| {
            if let Some(key) = selected {
                let step = key.interpolation == klipp_core::Interpolation::Step;
                if ui.button(format!("{}Step (Hold)", if step { "  * " } else { "    " })).clicked() {
                    cmds.push(AppCommand::SetKeyframeInterpolation { clip_id, node_id, param: param(), time_secs: key_time, interpolation: klipp_core::Interpolation::Step });
                    ui.close_menu();
                }
            }
            if ui.button("Clear bypass keyframes").clicked() {
                cmds.push(AppCommand::ClearKeyframes { clip_id, node_id, param: param() });
                ui.close_menu();
            }
        });
    }
    diamond_resp.on_hover_text(if selected.is_some() { "Remove bypass keyframe at playhead" } else { "Add bypass keyframe at playhead" });
    if !keys.is_empty() {
        caret(ui, Icon::CaretLeft, around.prev_keyframe_time, cmds);
    }
}

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
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::numeric(id, "inspector_slider", label, *value, &range, Some(default), None, Some(2), suffix, None);
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
    // A one-shot change (a click, a reset, a typed value) is its own undo
    // step: Begin goes out with it and End at the row's next frame, after
    // the caller's SetNodeParam, as a drag's End follows its last step.
    let pending_end = id.with("one_shot_end");
    if ui.data_mut(|d| d.remove_temp::<bool>(pending_end)).unwrap_or(false) {
        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
    }
    let one_shot = |ui: &egui::Ui, cmds: &mut Vec<AppCommand>| {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        ui.data_mut(|d| d.insert_temp(pending_end, true));
        ui.ctx().request_repaint();
    };

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
            one_shot(ui, cmds);
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
                one_shot(ui, cmds);
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
            // The grade rule, shared with the generated rows (M3-4).
            *value = crate::widgets::grade_drag::grade_drag(*value, drag_dx(ui, &val) * slider_drag_factor(ui), span).clamp(lo, hi);
            changed = true;
        }
        if val.drag_stopped() {
            cmds.push(AppCommand::EndNodeParamEdit { clip_id });
        }
        if val.double_clicked() {
            ui.data_mut(|d| d.insert_temp(edit_id, format!("{:.2}", *value)));
        } else if val.secondary_clicked() {
            one_shot(ui, cmds);
            *value = default;
            changed = true;
        }
        val.on_hover_text("Drag to adjust (Shift fine, Ctrl coarse) · double-click to type · right-click to reset");
    }
    if track.double_clicked() || track.secondary_clicked() {
        // A double-click's first click may already have opened a step for
        // its jump; the reset joins it rather than opening another.
        if !ui.data(|d| d.get_temp::<bool>(pending_end).unwrap_or(false)) {
            one_shot(ui, cmds);
        }
        *value = default;
        changed = true;
    }
    changed
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
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", param_key, label, *field);
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
            push_atomic_node_edit(cmds, clip_id, node_id, param_key, value);
        }
    });
}

/// The bundled LUT library under `dir` (#892): each category folder, by
/// its name, with its LUT files, sorted. Folders with none are left out.
pub(crate) fn bundled_luts(dir: &std::path::Path) -> Vec<(String, Vec<std::path::PathBuf>)> {
    let is_lut = |p: &std::path::Path| p.extension().is_some_and(|e| e == "cube" || e == "3dl" || e == "csp");
    let mut out: Vec<(String, Vec<std::path::PathBuf>)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|cat| {
            let mut files: Vec<_> = std::fs::read_dir(&cat).into_iter().flatten().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_lut(p)).collect();
            files.sort();
            let name = cat.file_name()?.to_string_lossy().replace('-', " ");
            (!files.is_empty()).then_some((name, files))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The wheels' Luma Mix, and in log mode the Low and High Range (#900):
/// how much of the wheels' change to brightness is kept, and where the
/// low band ends and the high band begins on the encoded luma.
/// Centre every row independently, including a single wheel in a narrow
/// inspector or the last Log wheel. Cells own a top-down layout so all
/// readouts and sliders stay beneath their wheel.
fn centred_wheel_rows(
    ui: &mut egui::Ui,
    count: usize,
    columns: usize,
    cell_w: f32,
    cell_h: f32,
    mut show: impl FnMut(&mut egui::Ui, usize),
) {
    for start in (0..count).step_by(columns) {
        let n = columns.min(count - start);
        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), cell_h), egui::Sense::hover(),
        );
        let row_w = n as f32 * cell_w + (n - 1) as f32 * 12.0;
        let left = row.center().x - row_w / 2.0;
        for col in 0..n {
            let rect = egui::Rect::from_min_size(
                egui::pos2(left + col as f32 * (cell_w + 12.0), row.top()),
                egui::vec2(cell_w, cell_h),
            );
            let mut cell = ui.new_child(egui::UiBuilder::new()
                .id_salt(("wheel_cell", start + col))
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Center)));
            show(&mut cell, start + col);
        }
        ui.add_space(8.0);
    }
}

fn wheel_shape_sliders(
    ui: &mut egui::Ui,
    params: &NodeParams,
    log_mode: bool,
    node_id: NodeId,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    theme: &crate::theme::KlippTheme,
) {
    use klipp_color::grade_model::{LOG_HIGH_RANGE, LOG_LOW_RANGE, MID_GREY_ENCODED};
    let mut rows = vec![("Luma Mix", "luma_mix", params.luma_mix.unwrap_or(1.0), 0.0..=1.0, 1.0)];
    if log_mode {
        rows.push(("Low Range", "log_low_range", params.log_low_range.unwrap_or(LOG_LOW_RANGE), 0.0..=MID_GREY_ENCODED - 0.08, LOG_LOW_RANGE));
        rows.push(("High Range", "log_high_range", params.log_high_range.unwrap_or(LOG_HIGH_RANGE), MID_GREY_ENCODED + 0.08..=1.2, LOG_HIGH_RANGE));
    }
    // Sent, not written into `params`: they are the clip's own, and a
    // value written there on the press would already be in the snapshot
    // the gesture's BeginNodeParamEdit takes, so undo would return to it.
    for (label, pname, val, range, default) in rows {
        let mut v = val;
        #[cfg(any(test, feature = "row-recorder"))]
        crate::widgets::row_recorder::next_parameter(pname);
        if grade_slider_resetting(ui, label, &mut v, range, default, "", cmds, clip_id, theme) {
            cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: pname.into(), value: v });
        }
    }
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
    grade_slider_with(ui, label, value, range, None, suffix, cmds, clip_id, theme)
}

/// [`grade_slider`] whose label resets it to `default` when clicked, one
/// undo step, as every number row of the inspector resets: for the
/// editors the generated pages draw too (the wheels' shape sliders, the
/// Difference Key's).
#[allow(clippy::too_many_arguments)]
fn grade_slider_resetting(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: f32,
    suffix: &str,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    theme: &crate::theme::KlippTheme,
) -> bool {
    grade_slider_with(ui, label, value, range, Some(default), suffix, cmds, clip_id, theme)
}

#[allow(clippy::too_many_arguments)]
fn grade_slider_with(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default: Option<f32>,
    suffix: &str,
    cmds: &mut Vec<AppCommand>,
    clip_id: ClipId,
    theme: &crate::theme::KlippTheme,
) -> bool {
    let pad = 14.0;
    let w = (ui.available_width() - pad * 2.0).max(60.0);
    let range_span = (range.end() - range.start()).abs();
    let speed = range_span / 800.0;
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::numeric(ui.next_auto_id(), "grade_slider", label, *value, &range, None, Some(f64::from(speed)), Some(2), suffix, None);
    let mut changed = false;
    // A one-shot change (a typed value, a keyboard nudge, a press on the
    // rail) is its own undo step, as `slider_interact`'s are: Begin goes out
    // with it, before the caller's SetNodeParam, and End on the first frame
    // after it with no button held. A press that turns into a drag joins
    // the drag's gesture, whose End closes it first.
    let one_shot_end = ui.next_auto_id().with("grade_slider_one_shot");
    if ui.data(|d| d.get_temp::<bool>(one_shot_end)).unwrap_or(false) && !ui.input(|i| i.pointer.any_down()) {
        ui.data_mut(|d| d.remove::<bool>(one_shot_end));
        cmds.push(AppCommand::EndNodeParamEdit { clip_id });
    }
    let mut one_shot = false;
    // Begin/End signals collected inside the horizontal closures (which can't
    // mutably borrow `cmds` directly) and flushed after each closure returns.
    let mut drag_value_started = false;
    let mut drag_value_ended = false;
    let mut drag_slider_started = false;
    let mut drag_slider_ended = false;

    ui.horizontal(|ui| {
        ui.add_space(pad);
        let text = egui::Label::new(egui::RichText::new(label).size(11.0).color(theme.overlay(107)));
        match default {
            Some(default) => {
                if ui.add(text.sense(egui::Sense::click())).on_hover_text("Click to reset").clicked() && *value != default {
                    *value = default;
                    changed = true;
                    one_shot = true;
                }
            }
            None => {
                ui.add(text);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(pad);
            // The number's drag moves the value by the grade rule, shared with
            // the generated rows (M3-4), unrounded: the field's own drag is
            // overridden, Shift still dragging a tenth as fast; its typing and
            // keyboard nudges (at its speed) are kept.
            let before_drag = *value;
            let resp = ui.add(
                egui::DragValue::new(value)
                    .range(range.clone())
                    .speed(speed)
                    .custom_formatter(|v, _| {
                        if suffix.is_empty() { format!("{v:.2}") } else { format!("{v:.2} {suffix}") }
                    }),
            );
            if resp.dragged() {
                let fine = if ui.input(|i| i.modifiers.shift_only()) { 0.1 } else { 1.0 };
                *value = crate::widgets::grade_drag::grade_drag(before_drag, drag_dx(ui, &resp) * fine, range_span).clamp(*range.start(), *range.end());
            }
            if resp.drag_started() { drag_value_started = true; }
            if resp.changed() { changed = true; }
            if resp.changed() && !resp.dragged() && !resp.drag_stopped() { one_shot = true; }
            if resp.drag_stopped() { drag_value_ended = true; }
        });
    });
    if drag_value_started {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    }
    // Released, the drag is one step; ended by Escape, taken back.
    if let (_, Some(end)) = gesture_of(ui, false, drag_value_ended) {
        cmds.push(end(clip_id));
    }
    ui.horizontal(|ui| {
        ui.add_space(pad);
        let resp = ui.add_sized(
            [w, 10.0],
            egui::Slider::new(value, range).show_value(false).clamping(egui::SliderClamping::Always),
        );
        if resp.drag_started() { drag_slider_started = true; }
        if resp.changed() { changed = true; }
        if resp.changed() && !resp.dragged() && !resp.drag_stopped() { one_shot = true; }
        if resp.drag_stopped() { drag_slider_ended = true; }
    });
    if one_shot {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
        ui.data_mut(|d| d.insert_temp(one_shot_end, true));
        ui.ctx().request_repaint();
    }
    if drag_slider_started {
        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
    }
    // Released, the drag is one step; ended by Escape, taken back.
    if let (_, Some(end)) = gesture_of(ui, false, drag_slider_ended) {
        cmds.push(end(clip_id));
    }
    changed
}

/// A grade editor's drag (a curve, a wheel, its sliders) as a node-param
/// gesture: whether it began this
/// frame, and how it ended, if it did: committed on release
/// (`EndNodeParamEdit`), or taken back when Escape ended it
/// (`AbandonNodeParamEdit`), as the other rows' drags are.
/// A mask parameter's name in the list: "center_x" is "Center x".
fn param_label(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut c = words.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// A mask's join, as the Mode menu names it (#74).
fn combine_label(c: klipp_core::MaskCombine) -> &'static str {
    match c {
        klipp_core::MaskCombine::Add => "Add",
        klipp_core::MaskCombine::Subtract => "Subtract",
        klipp_core::MaskCombine::Intersect => "Intersect",
    }
}

fn gesture_of(ui: &mut egui::Ui, started: bool, ended: bool) -> (bool, Option<fn(ClipId) -> AppCommand>) {
    let end: Option<fn(ClipId) -> AppCommand> = ended.then(|| {
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            (|clip_id| AppCommand::AbandonNodeParamEdit { clip_id }) as fn(ClipId) -> AppCommand
        } else {
            |clip_id| AppCommand::EndNodeParamEdit { clip_id }
        }
    });
    (started, end)
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

/// A grade node's keys as a control drawn at `frame` reads them: its
/// tracks, by parameter (#891). A control over several parameters (a
/// colour wheel's puck and master) keys them together.
#[derive(Clone, Copy)]
pub(crate) struct NodeKeys<'a> {
    pub tracks: &'a klipp_nodes::NodeKeyframeTracks,
    pub frame: FrameSelection,
}

impl NodeKeys<'_> {
    fn of(&self, param: &str) -> &[klipp_core::Keyframe] {
        self.tracks.tracks.iter().find(|t| t.param_name == param).map_or(&[], |t| &t.keyframes)
    }

    /// The parameters' keys together, by time: a key wherever any of them
    /// has one (the first's, at a time two share).
    fn union(&self, params: &[&str]) -> Vec<klipp_core::Keyframe> {
        let mut all: Vec<klipp_core::Keyframe> = params.iter().flat_map(|p| self.of(p).iter().cloned()).collect();
        all.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
        all.dedup_by(|b, a| (a.time_secs - b.time_secs).abs() < klipp_core::keyframe::KEYFRAME_TIME_EPSILON);
        all
    }

    /// The control's key state at the frame.
    fn state(&self, params: &[&str]) -> crate::widgets::scrubby_param::KeyframeState {
        crate::inspector::keyframe_selection::state(&self.union(params), self.frame.time_secs, self.frame.tolerance)
    }

    /// Whether any of the parameters is keyed: an edit of the control is
    /// then a key, since what it shows is the animation.
    fn keyed(&self, params: &[&str]) -> bool {
        params.iter().any(|p| !self.of(p).is_empty())
    }

    /// What `param` shows at the frame: its key there, or its keys' value
    /// between them, else `stored`.
    fn shown(&self, param: &str, stored: f32) -> f32 {
        let keys = self.of(param);
        if keys.is_empty() { stored } else { crate::inspector::keyframe_selection::editable_value(keys, self.frame.time_secs, stored, self.frame.tolerance) }
    }

    /// `param`'s own key in the frame, by its stored time.
    fn own_key(&self, param: &str) -> Option<f64> {
        let keys = self.of(param);
        crate::inspector::keyframe_selection::nearest(keys, self.frame.time_secs, self.frame.tolerance).map(|i| keys[i].time_secs)
    }

    /// Where `param`'s key goes when the control keys it: its own key in
    /// the frame, else the control's (another part's), else the frame's
    /// time, so the parts' keys line up.
    fn key_time(&self, param: &str, params: &[&str]) -> f64 {
        let union = self.union(params);
        let control = crate::inspector::keyframe_selection::nearest(&union, self.frame.time_secs, self.frame.tolerance).map(|i| union[i].time_secs);
        self.own_key(param).or(control).unwrap_or(self.frame.time_secs)
    }

    /// The edit `values` (each parameter's new value) of a keyed control:
    /// a key on each part at the control's key time.
    fn key_values(&self, cmds: &mut Vec<AppCommand>, clip_id: ClipId, node_id: NodeId, params: &[&str], values: &[f32]) {
        for (param, value) in params.iter().zip(values) {
            cmds.push(AppCommand::AddKeyframe { clip_id, node_id, param: (*param).to_owned(), time_secs: self.key_time(param, params), value: *value });
        }
    }

    /// What the control's key strip asked for, as commands: the diamond
    /// adds a key on every part with the values shown (`values`), or takes
    /// the parts' keys in the frame away, one step; an arrow seeks to the
    /// control's key that way; the key menu sets the interpolation or the
    /// ease of each part's key there, one step.
    fn strip_commands(&self, cmds: &mut Vec<AppCommand>, clip_id: ClipId, node_id: NodeId, params: &[&str], values: &[f32], strip: crate::widgets::scrubby_param::KeyStrip) {
        use crate::widgets::scrubby_param::KeyMenuPick;
        let state = self.state(params);
        if strip.toggled {
            atomic_node_edit_batch(cmds, clip_id, |cmds| {
                if state.has_keyframe_at_playhead {
                    for param in params {
                        if let Some(time_secs) = self.own_key(param) {
                            cmds.push(AppCommand::RemoveKeyframe { clip_id, node_id, param: (*param).to_owned(), time_secs });
                        }
                    }
                } else {
                    self.key_values(cmds, clip_id, node_id, params, values);
                }
            });
        }
        let to = if strip.prev { state.prev_keyframe_time } else if strip.next { state.next_keyframe_time } else { None };
        if let Some(t) = to {
            cmds.push(self.frame.seek_to(t));
        }
        if let Some(pick) = strip.pick {
            atomic_node_edit_batch(cmds, clip_id, |cmds| {
                for param in params {
                    let Some(time_secs) = self.own_key(param) else { continue };
                    let param = (*param).to_owned();
                    cmds.push(match pick {
                        KeyMenuPick::Interpolation(interpolation) => AppCommand::SetKeyframeInterpolation { clip_id, node_id, param, time_secs, interpolation },
                        KeyMenuPick::Ease(preset) => AppCommand::SetKeyframeEase { clip_id, node_id, param, time_secs, ease: preset.key_ease() },
                    });
                }
            });
        }
    }
}

/// The HDR zones' map (#240), drawn, not edited: a stop axis from −8 to
/// +8 about mid grey, each zone's weight over it, the picked zone's
/// strongest and its boundary marked. Its only words are the axis's ends,
/// written with a minus sign, never a number a row could be read as.
/// Every value of the HDR zones `z` (#240) by its key, as a node's rows
/// write them: what a preset sets, all of it.
fn hdr_zones_writes(z: &klipp_core::HdrZonesSettings) -> Vec<(String, f32)> {
    let mut z = z.clone();
    klipp_nodes::registry::node_type(NodeKind::HdrZones)
        .map_or(&[][..], |t| t.desc.params)
        .iter()
        .filter_map(|d| klipp_nodes::params::hdr_zone_slot(&mut z, d.key).map(|v| (d.key.to_owned(), *v)))
        .collect()
}

pub(crate) fn hdr_zone_map(ui: &mut egui::Ui, zones: &klipp_core::HdrZonesSettings, picked: usize, theme: &KlippTheme) {
    use klipp_color::grade_model::hdr_zones::{weight, ZONES};
    const SPAN: f32 = 8.0;
    let w = (ui.available_width() - 28.0).max(80.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 74.0), egui::Sense::hover());
    let plot = egui::Rect::from_min_size(egui::pos2(rect.min.x + 14.0, rect.min.y + 4.0), egui::vec2(w, 52.0));
    let painter = ui.painter_at(rect);
    painter.rect_filled(plot, 2.0, theme.bg_deep);
    let x_of = |s: f32| plot.min.x + (s + SPAN) / (2.0 * SPAN) * plot.width();
    let y_of = |wt: f32| plot.max.y - wt * (plot.height() - 4.0) - 2.0;
    // Mid grey, and a tick every stop.
    for s in -8..=8 {
        let x = x_of(s as f32);
        let h = if s == 0 { plot.height() } else if s % 4 == 0 { 6.0 } else { 3.0 };
        painter.line_segment([egui::pos2(x, plot.max.y - h), egui::pos2(x, plot.max.y)], egui::Stroke::new(if s == 0 { 1.0_f32 } else { 0.5_f32 }, theme.text_dim));
    }
    let samples = 96;
    for i in (1..ZONES).filter(|i| *i != picked).chain((picked > 0).then_some(picked)) {
        let points: Vec<egui::Pos2> = (0..=samples)
            .map(|k| {
                let s = -SPAN + 2.0 * SPAN * k as f32 / samples as f32;
                egui::pos2(x_of(s), y_of(weight(i, s, zones.boundary[i], zones.falloff[i])))
            })
            .collect();
        let stroke = if i == picked { egui::Stroke::new(2.0_f32, theme.accent) } else { egui::Stroke::new(1.0_f32, theme.text_dim.gamma_multiply(0.6)) };
        painter.add(egui::Shape::line(points, stroke));
    }
    if picked > 0 {
        let x = x_of(zones.boundary[picked].clamp(-SPAN, SPAN));
        painter.line_segment([egui::pos2(x, plot.min.y), egui::pos2(x, plot.max.y)], egui::Stroke::new(1.0_f32, theme.accent));
    } else {
        // Global reaches everything.
        painter.line_segment([egui::pos2(plot.min.x, y_of(1.0)), egui::pos2(plot.max.x, y_of(1.0))], egui::Stroke::new(2.0_f32, theme.accent));
    }
    let font = egui::FontId::proportional(9.0);
    painter.text(egui::pos2(plot.min.x, plot.max.y + 3.0), egui::Align2::LEFT_TOP, "\u{2212}8 stops", font.clone(), theme.text_dim);
    painter.text(egui::pos2(x_of(0.0), plot.max.y + 3.0), egui::Align2::CENTER_TOP, "mid grey", font.clone(), theme.text_dim);
    painter.text(egui::pos2(plot.max.x, plot.max.y + 3.0), egui::Align2::RIGHT_TOP, "+8 stops", font, theme.text_dim);
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
mod sends_on_change {
    //! The panels that edit a node's settings as one value (#894): drawn,
    //! they send nothing and leave the node as it was; changed, they send
    //! the new value once, so the command makes the change and its undo
    //! step holds the value from before.
    use super::*;

    struct Rig {
        ctx: egui::Context,
        time: f64,
        panel: NodeInspectorPanel,
        graph: NodeGraph,
        node: NodeId,
        sent: Vec<AppCommand>,
    }

    impl Rig {
        fn new(kind: NodeKind, params: impl FnOnce(&mut NodeParams)) -> Self {
            let mut graph = NodeGraph::default();
            let node = graph.ensure_qualifier();
            let n = graph.nodes.iter_mut().find(|n| n.id == node).unwrap();
            n.kind = kind;
            params(&mut n.params);
            Rig { ctx: egui::Context::default(), time: 0.0, panel: NodeInspectorPanel::default(), graph, node, sent: Vec::new() }
        }

        fn params(&self) -> &NodeParams {
            &self.graph.nodes.iter().find(|n| n.id == self.node).unwrap().params
        }

        /// One frame with `events`: the text drawn, with where.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 1400.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let theme = KlippTheme::default();
            let selected = Some(self.node);
            let frame = crate::inspector::keyframe_selection::FrameSelection::new(klipp_core::Timecode::zero(), klipp_core::Timecode::zero(), 0.02);
            let (panel, graph, sent) = (&mut self.panel, &mut self.graph, &mut self.sent);
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    *sent = panel.show(ui, &selected, graph, ClipId::new(), &theme, frame);
                });
            });
            out.shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                })
                .collect()
        }

        /// Click the control labelled `label`; what that frame sent.
        fn click(&mut self, label: &str) -> Vec<AppCommand> {
            let drawn = self.frame(Vec::new());
            let at = drawn.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("{label} is drawn: {drawn:?}")).1;
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]);
            std::mem::take(&mut self.sent)
        }

        fn idle(&mut self) -> Vec<AppCommand> {
            let mut all = Vec::new();
            for _ in 0..3 {
                self.frame(Vec::new());
                all.append(&mut self.sent);
            }
            all
        }
    }

    #[test]
    fn an_open_panel_sends_nothing_and_leaves_the_node_alone() {
        let rigs = [
            Rig::new(NodeKind::Qualifier, |p| p.qualifier = Some(klipp_core::color_grade::QualifierSettings { enabled: true, ..Default::default() })),
            Rig::new(NodeKind::LumaKey, |p| p.luma_key = Some(klipp_core::color_grade::LumaKeySettings { enabled: true, ..Default::default() })),
            Rig::new(NodeKind::SoftClip, |p| p.soft_clip = Some(klipp_core::color_grade::SoftClipSettings { enabled: true, ..Default::default() })),
            Rig::new(NodeKind::FilmGrain, |p| p.grain = Some(klipp_core::color_grade::FilmGrainSettings { enabled: true, ..Default::default() })),
            Rig::new(NodeKind::Qualifier, |_| {}),
        ];
        for mut rig in rigs {
            let before = rig.params().clone();
            let sent = rig.idle();
            assert!(sent.is_empty(), "drawing the panel sent {sent:?}");
            assert_eq!(rig.params(), &before, "drawing the panel changed the node");
        }
    }

    #[test]
    fn a_checkbox_sends_the_new_value_once_and_leaves_the_change_to_it() {
        let mut grain = Rig::new(NodeKind::FilmGrain, |p| p.grain = Some(klipp_core::color_grade::FilmGrainSettings { enabled: true, ..Default::default() }));
        let sent = grain.click("Monochrome");
        let set: Vec<_> = sent.iter().filter_map(|c| match c { AppCommand::SetNodeFilmGrain { grain, .. } => Some(grain), _ => None }).collect();
        assert_eq!(set.len(), 1, "one SetNodeFilmGrain: {sent:?}");
        // Monochrome starts on, so the click turns it off.
        assert!(!set[0].monochrome && set[0].enabled, "{:?}", set[0]);
        assert!(grain.params().grain.as_ref().unwrap().monochrome, "the command makes the change, not the panel");

        let mut qual = Rig::new(NodeKind::Qualifier, |p| p.qualifier = Some(klipp_core::color_grade::QualifierSettings { enabled: true, ..Default::default() }));
        let sent = qual.click("Enabled");
        assert!(
            sent.iter().any(|c| matches!(c, AppCommand::SetNodeQualifier { qualifier, .. } if !qualifier.enabled)),
            "turning the qualifier off is sent: {sent:?}"
        );
    }

    /// An imported CDL is one undo step: its thirteen writes go out between
    /// one Begin and one End.
    #[test]
    fn an_imported_cdl_is_one_undo_step() {
        let cdl = klipp_grade_model::cdl::Cdl::parse("<ColorCorrection><SOPNode><Slope>1.1 1 0.9</Slope><Offset>0.01 0 -0.01</Offset><Power>1 1 1</Power></SOPNode><SatNode><Saturation>0.8</Saturation></SatNode></ColorCorrection>").unwrap();
        let sent = cdl_commands(&cdl, ClipId::new(), NodeId::new_v4());
        assert!(matches!(sent.first(), Some(AppCommand::BeginNodeParamEdit { .. })) && matches!(sent.last(), Some(AppCommand::EndNodeParamEdit { .. })), "{sent:?}");
        assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::SetNodeParam { .. })).count(), 13);
        assert_eq!(sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. } | AppCommand::EndNodeParamEdit { .. })).count(), 2);
    }

    #[test]
    fn the_curves_reset_is_one_undo_step_made_by_its_commands() {
        let mut rig = Rig::new(NodeKind::RGBCurves, |p| {
            // Drawing the curve editor fills an unset curve set with the
            // identity, which changes nothing; start with it set.
            let identity = vec![[0.0, 0.0], [1.0, 1.0]];
            p.curves = Some(klipp_core::color_grade::CurveSet { master: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]], red: identity.clone(), green: identity.clone(), blue: identity });
            p.curves_mix = Some(0.4);
            p.curves_preserve_luma = Some(true);
        });
        let before = rig.params().clone();
        let drawn = rig.frame(Vec::new());
        // The section header's reset is the ↺ beside "RGB CURVES".
        let header = drawn.iter().find(|(t, _)| t == "RGB CURVES").expect("the section header").1;
        let reset = drawn.iter().filter(|(t, p)| (p.y - header.y).abs() < 6.0 && p.x > header.x && t.chars().count() == 1).map(|(_, p)| *p).next();
        let Some(at) = reset else { panic!("a reset button on the header row: {drawn:?}") };
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        rig.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        rig.frame(vec![button(false)]);
        let sent = std::mem::take(&mut rig.sent);
        let begins = sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count();
        assert!(begins == 1 && matches!(sent.last(), Some(AppCommand::EndNodeParamEdit { .. })), "one undo step: {sent:?}");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SetNodeParam { param, value, .. } if param == "curves_preserve_luma" && *value == 0.0)), "{sent:?}");
        assert_eq!(rig.params(), &before, "the commands make the change, not the panel");
    }

    #[test]
    fn the_lut_node_clears_its_lut_through_a_command() {
        let mut rig = Rig::new(NodeKind::Lut3D, |p| p.lut_path = Some("/tmp/look.cube".into()));
        let sent = rig.click("Clear");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SetNodeLut { path: None, .. })), "{sent:?}");
        assert!(rig.params().lut_path.is_some(), "the command takes it off, not the panel");
    }

    #[test]
    fn the_bundled_luts_are_listed_by_category() {
        let dir = std::env::temp_dir().join(format!("klipp_bundled_luts_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (cat, files) in [("film-print", &["b.cube", "a.3dl", "notes.txt"][..]), ("cinematic", &["teal.cube"][..]), ("empty", &[][..])] {
            std::fs::create_dir_all(dir.join(cat)).unwrap();
            for f in files {
                std::fs::write(dir.join(cat).join(f), "").unwrap();
            }
        }
        let listed: Vec<(String, Vec<String>)> = super::bundled_luts(&dir)
            .into_iter()
            .map(|(c, fs)| (c, fs.iter().map(|f| f.file_name().unwrap().to_string_lossy().to_string()).collect()))
            .collect();
        assert_eq!(listed, vec![("cinematic".to_string(), vec!["teal.cube".to_string()]), ("film print".to_string(), vec!["a.3dl".to_string(), "b.cube".to_string()])]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1039: the hue/sat curves draw no Preserve Luma; it is the RGB
    /// curves' setting, which hue/sat curves never read.
    #[test]
    fn the_hue_sat_curves_offer_no_preserve_luma() {
        let mut rig = Rig::new(NodeKind::HueSatCurves, |_| {});
        let drawn = rig.frame(Vec::new());
        assert!(drawn.iter().all(|(t, _)| t != "Preserve Luma"), "{drawn:?}");
        let mut rig = Rig::new(NodeKind::RGBCurves, |_| {});
        assert!(rig.frame(Vec::new()).iter().any(|(t, _)| t == "Preserve Luma"), "the RGB curves keep it");
    }

    /// #1039: an unset mix shows what the executor does with it: half on
    /// a Blend, all of the grade on an ApplyMatte.
    #[test]
    fn an_unset_mix_shows_what_the_node_does() {
        for (kind, shown) in [(NodeKind::Blend, "0.50"), (NodeKind::ApplyMatte, "1.00")] {
            let mut rig = Rig::new(kind, |_| {});
            let drawn = rig.frame(Vec::new());
            assert!(drawn.iter().any(|(t, _)| t == shown), "{kind:?}: an unset mix shows {shown}: {drawn:?}");
        }
    }

    #[test]
    fn preserve_luma_is_one_undo_step() {
        let mut rig = Rig::new(NodeKind::RGBCurves, |_| {});
        let sent = rig.click("Preserve Luma");
        assert!(
            matches!(sent.as_slice(), [AppCommand::BeginNodeParamEdit { .. }, AppCommand::SetNodeParam { param, value, .. }, AppCommand::EndNodeParamEdit { .. }] if param == "curves_preserve_luma" && *value == 1.0),
            "{sent:?}"
        );
        assert_eq!(rig.params().curves_preserve_luma, None, "the command makes the change");
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

    /// A reset and a typed value are each one undo step: Begin before
    /// the change and End after it, as a drag is.
    #[test]
    fn a_reset_and_a_typed_value_are_each_an_undo_step() {
        let begins_and_ends = |cmds: &[AppCommand]| -> Vec<char> {
            cmds.iter()
                .filter_map(|c| match c {
                    AppCommand::BeginNodeParamEdit { .. } => Some('('),
                    AppCommand::EndNodeParamEdit { .. } => Some(')'),
                    _ => None,
                })
                .collect()
        };
        let mut rig = Rig::new(0.4);
        let at = rig.number();
        rig.button(at, egui::PointerButton::Secondary, true, egui::Modifiers::NONE);
        rig.button(at, egui::PointerButton::Secondary, false, egui::Modifiers::NONE);
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        assert_eq!(rig.value, 0.0);
        assert_eq!(begins_and_ends(&rig.cmds), vec!['(', ')'], "the reset is not one undo step");

        rig.cmds.clear();
        rig.time += 1.0;
        let at = rig.number();
        rig.click(at);
        rig.click(at);
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        rig.frame(
            vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }],
            egui::Modifiers::COMMAND,
        );
        rig.frame(vec![egui::Event::Text("0.25".into())], egui::Modifiers::NONE);
        rig.frame(
            vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            egui::Modifiers::NONE,
        );
        rig.frame(Vec::new(), egui::Modifiers::NONE);
        assert!((rig.value - 0.25).abs() < 1e-6);
        assert_eq!(begins_and_ends(&rig.cmds), vec!['(', ')'], "the typed value is not one undo step");
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

/// The encoding the grade acts in now (#467): the project's choice under
/// the working space, as the executors take it.
fn grade_encoding_now() -> klipp_color::grade_model::Encoding {
    let tag = klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8();
    klipp_color::grade_model::Encoding::from_u8(tag)
}

/// A linear contrast pivot as the slider shows it: in the grade encoding.
fn pivot_shown(linear: f32) -> f32 {
    klipp_color::grade_model::encode(linear, grade_encoding_now())
}

/// The slider's value back to a linear pivot.
fn pivot_linear(shown: f32) -> f32 {
    klipp_color::grade_model::decode(shown, grade_encoding_now())
}

#[cfg(test)]
mod wheel_layout_tests {
    use super::*;

    /// The old per-node panel's Mask node offers a tracked mask Re-track
    /// and Untrack, as the generated view does, and an untracked one Track
    /// alone (#1398); which masks are tracked is read from the clip.
    #[test]
    fn the_old_mask_panel_offers_retrack_and_untrack_for_a_tracked_mask() {
        let node = NodeId::new_v4();
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        clip.track_points.push(serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "name": "Mask 1", "color": [255, 200, 0, 255],
            "positions": [[0.5, 0.5]], "start_frame": 0, "enabled": true, "search_radius": 24,
            "linked_mask": [node, 0],
        })).unwrap());
        assert_eq!(tracked_masks_of(&clip, node), vec![0]);
        assert!(tracked_masks_of(&clip, NodeId::new_v4()).is_empty(), "another node's");
        let circle = klipp_core::Mask { shape: klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }, ..Default::default() };
        for (tracked, want) in [(vec![0], ["\u{1F3AF} Re-track", "\u{1F6AB} Untrack"].as_slice()), (Vec::new(), ["\u{1F3AF} Track"].as_slice())] {
            let ctx = egui::Context::default();
            let mut panel = NodeInspectorPanel { tracked_masks: tracked.clone(), ..Default::default() };
            let mut params = NodeParams { masks: Some(klipp_core::MaskStack { masks: vec![circle.clone()] }), ..Default::default() };
            let mut texts = Vec::new();
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 2000.0))), ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        panel.show_inspector_layout(ui, node, &NodeKind::Mask, &mut true, &mut params, &mut Default::default(), clip.id, &mut Vec::new(), &KlippTheme::default(),
                            FrameSelection::new(klipp_core::Timecode::zero(), klipp_core::Timecode::zero(), 0.02), false);
                    });
                });
                texts = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_owned()), _ => None }).collect::<Vec<_>>();
            }
            for w in want {
                assert!(texts.iter().any(|t| t == w), "tracked {tracked:?}: no {w:?} among {texts:?}");
            }
            assert_eq!(texts.iter().any(|t| t == "\u{1F6AB} Untrack"), !tracked.is_empty(), "tracked {tracked:?}");
        }
    }

    #[test]
    fn wheels_and_controls_fit_and_stay_centred_across_resizing() {
        for width in [180.0, 220.0, 251.0, 252.0, 253.0, 280.0, 320.0, 360.0, 600.0, 780.0] {
            for log in [false, true] {
                let ctx = egui::Context::default();
                let mut panel = NodeInspectorPanel::default();
                let mut params = NodeParams { wheels_log_mode: Some(log), ..Default::default() };
                let mut out = None;
                let mut textures = egui::TexturesDelta::default();
                // Egui settles font/widget metrics on its first pass.
                for _ in 0..3 {
                    out = Some(ctx.run(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 2000.0))),
                        ..Default::default()
                    }, |ctx| {
                        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                            panel.show_inspector_layout(ui, NodeId::new_v4(), &NodeKind::ColorWheels,
                                &mut true, &mut params, &mut Default::default(), ClipId::new(),
                                &mut Vec::new(), &KlippTheme::default(),
                                FrameSelection::new(klipp_core::Timecode::zero(), klipp_core::Timecode::zero(), 0.02), false);
                        });
                    }));
                    textures.append(out.as_ref().unwrap().textures_delta.clone());
                }
                let out = out.unwrap();
                if let Ok(dir) = std::env::var("KLIPP_WHEEL_REVIEW_DIR") {
                    render_wheel_layout_review(&ctx, &out, &textures, width as u32, &format!("{dir}/wheels-{width}-{log}.png"));
                }
                let texts: Vec<_> = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text(), egui::Rect::from_min_size(t.pos, t.galley.size()), s.clip_rect)),
                    _ => None,
                }).collect();
                for (text, rect, clip) in &texts {
                    assert!(rect.left() >= -1.0 && rect.right() <= width + 1.0,
                        "{width}, log={log}: {text:?} outside panel: {rect:?}");
                    assert!(clip.expand(1.0).contains_rect(*rect),
                        "{width}, log={log}: {text:?} clipped: {rect:?} vs {clip:?}");
                }
                let names: &[&str] = if log { &["Low", "Mid", "High"] } else { &["Lift", "Gamma", "Gain", "Offset"] };
                let centres: Vec<_> = names.iter().map(|name| texts.iter().find(|(t, _, _)| t == name).unwrap().1.center()).collect();
                if width < 252.0 {
                    for c in &centres { assert!((c.x - width / 2.0).abs() < 1.0, "{width}: {centres:?}"); }
                    for pair in centres.windows(2) { assert!(pair[1].y > pair[0].y + 100.0); }
                } else {
                    assert!((centres[0].y - centres[1].y).abs() < 1.0);
                    assert!(((centres[0].x + centres[1].x) / 2.0 - width / 2.0).abs() < 1.0);
                    if log { assert!((centres[2].x - width / 2.0).abs() < 1.0); }
                    else { assert!((centres[2].x - centres[0].x).abs() < 1.0); assert!((centres[3].x - centres[1].x).abs() < 1.0); }
                }
            }
        }
    }
}

#[cfg(test)]
fn render_wheel_layout_review(ctx: &egui::Context, out: &egui::FullOutput, textures: &egui::TexturesDelta, width: u32, path: &str) {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).unwrap();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = eframe::egui_wgpu::Renderer::new(&device, format, None, 1, false);
    for (id, delta) in &textures.set { renderer.update_texture(&device, &queue, *id, delta); }
    let jobs = ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
    let screen = eframe::egui_wgpu::ScreenDescriptor { size_in_pixels: [width, 2000], pixels_per_point: 1.0 };
    let size = wgpu::Extent3d { width, height: 2000, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None, size, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let stride = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (stride * 2000) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut encoder = device.create_command_encoder(&Default::default());
    let commands = renderer.update_buffers(&device, &queue, &mut encoder, &jobs, &screen);
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
        }).forget_lifetime();
        renderer.render(&mut pass, &jobs, &screen);
    }
    encoder.copy_texture_to_buffer(texture.as_image_copy(), wgpu::ImageCopyBuffer { buffer: &buffer,
        layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: None } }, size);
    queue.submit(commands.into_iter().chain(std::iter::once(encoder.finish())));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::Maintain::Wait);
    let mapped = buffer.slice(..).get_mapped_range();
    let pixels: Vec<u8> = mapped.chunks(stride as usize).flat_map(|row| row[..width as usize * 4].iter().copied()).collect();
    image::save_buffer(path, &pixels, width, 2000, image::ColorType::Rgba8).unwrap();
}

#[cfg(test)]
mod mask_list_tests {
    //! The mask list (#74): each mask's mode, inner feather, name, lock,
    //! copy and place in the stack, edited with real pointer events.
    use super::*;
    use klipp_core::{Mask, MaskCombine, MaskShape, MaskStack};

    struct Rig {
        ctx: egui::Context,
        time: f64,
        masks: MaskStack,
        sent: Vec<AppCommand>,
        texts: Vec<(String, egui::Pos2)>,
        node: NodeId,
        clip: ClipId,
    }

    impl Rig {
        fn new() -> Self {
            let masks = MaskStack { masks: vec![
                Mask { shape: MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.5, height: 0.5, rotation: 0.0 }, ..Mask::default() },
                Mask { shape: MaskShape::polygon([(0.3, 0.3), (0.7, 0.3), (0.5, 0.7)]), name: "Face".into(), ..Mask::default() },
            ] };
            let mut r = Rig { ctx: egui::Context::default(), time: 0.0, masks, sent: Vec::new(), texts: Vec::new(), node: NodeId::new_v4(), clip: ClipId::new() };
            r.frame(Vec::new());
            r.frame(Vec::new());
            r
        }

        /// One frame; what it sent applied as the app applies the stack.
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 1200.0))), time: Some(self.time), events, ..Default::default() };
            let frame = crate::inspector::keyframe_selection::FrameSelection::new(klipp_core::Timecode::zero(), klipp_core::Timecode::zero(), 0.02);
            let mut masks = self.masks.clone();
            let mut cmds = Vec::new();
            let (node, clip) = (self.node, self.clip);
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    NodeInspectorPanel::render_masks_body(ui, &mut masks, node, clip, &mut cmds, &KlippTheme::default(), frame, &[]);
                });
            });
            self.texts = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect();
            for cmd in &cmds {
                match cmd {
                    AppCommand::SetNodeMasks { masks, .. } => self.masks = masks.clone(),
                    AppCommand::MoveMaskShape { from, to, .. } => {
                        let m = self.masks.masks.remove(*from);
                        self.masks.masks.insert(*to, m);
                    }
                    _ => {}
                }
            }
            self.sent.extend(cmds);
        }

        fn at(&self, text: &str, nth: usize) -> egui::Pos2 {
            self.texts.iter().filter(|(t, _)| t == text).nth(nth).unwrap_or_else(|| panic!("no {text:?} #{nth}: {:?}", self.texts)).1
        }

        fn click(&mut self, p: egui::Pos2) {
            self.press(p, egui::PointerButton::Primary);
        }

        fn press(&mut self, p: egui::Pos2, button: egui::PointerButton) {
            self.frame(vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button, pressed: true, modifiers: egui::Modifiers::NONE }]);
            self.frame(vec![egui::Event::PointerButton { pos: p, button, pressed: false, modifiers: egui::Modifiers::NONE }]);
            self.frame(Vec::new());
        }

        /// `item` from the menu of the mask titled `title`.
        fn menu(&mut self, title: &str, item: &str) {
            let p = self.at(title, 0);
            self.press(p, egui::PointerButton::Secondary);
            let p = self.at(item, 0);
            self.click(p);
        }
    }

    /// A mask's mode is picked from its menu; the second mask moved up
    /// from its own menu takes the first place, by its own command; a
    /// copy goes at the end; a lock keeps it from the viewer; the list
    /// names a mask by its name, and counts a freeform one's points.
    #[test]
    fn the_mask_list_sets_mode_order_copy_and_lock() {
        let mut r = Rig::new();
        assert!(r.texts.iter().any(|(t, _)| t == "Face \u{00B7} Bezier"), "{:?}", r.texts);
        assert!(r.texts.iter().any(|(t, _)| t == "3 points"));

        let p = r.at("Add", 1);
        r.click(p);
        let p = r.at("Subtract", 0);
        r.click(p);
        assert_eq!(r.masks.masks[1].combine, MaskCombine::Subtract, "{:?}", r.sent);

        r.menu("Face \u{00B7} Bezier", "Move Up");
        assert!(r.sent.iter().any(|c| matches!(c, AppCommand::MoveMaskShape { from: 1, to: 0, .. })), "{:?}", r.sent);
        assert_eq!(r.masks.masks[0].name, "Face");

        r.menu("Face \u{00B7} Bezier", "Duplicate");
        assert_eq!(r.masks.masks.len(), 3);
        assert_eq!(r.masks.masks[2].name, "Face");

        let p = r.at("Lock", 0);
        r.click(p);
        assert!(r.masks.masks[0].locked);
        assert!(!r.texts.iter().any(|(t, _)| t.contains('\u{1F512}') || t.contains('\u{1F513}')), "the lock is the shared toggle, not an emoji");
    }

    /// The inner feather is a slider of its own, and a drag on it is one
    /// undo step.
    #[test]
    fn the_inner_feather_drags_as_one_step() {
        let mut r = Rig::new();
        let label = r.at("Feather inside", 0);
        let from = label + egui::vec2(80.0, 0.0);
        let to = from + egui::vec2(60.0, 0.0);
        let b = |p, d| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: d, modifiers: egui::Modifiers::NONE };
        r.frame(vec![egui::Event::PointerMoved(from), b(from, true)]);
        for i in 1..=4 {
            r.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
        }
        r.frame(vec![b(to, false)]);
        r.frame(Vec::new());
        assert!(r.masks.masks[0].feather_inner > 0.0, "{:?}", r.masks.masks[0]);
        assert_eq!(r.sent.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count(), 1, "{:?}", r.sent);
        assert_eq!(r.sent.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count(), 1);
    }

    /// A freeform mask's Shape row (#74): its diamond keys the shape at
    /// the playhead, the key's ease is picked from its menu, and the
    /// diamond again takes the last key off, the shape kept.
    #[test]
    fn the_shape_row_keys_eases_and_clears_the_shape() {
        let mut r = Rig::new();
        let diamond = |r: &Rig| {
            let label = r.at("Shape", 0);
            r.texts.iter().filter(|(t, p)| (t == "\u{25C7}" || t == "\u{25C6}") && (p.y - label.y).abs() < 6.0 && p.x < label.x).map(|(_, p)| *p).next_back().unwrap()
        };
        let d = diamond(&r);
        r.click(d);
        let keys = &r.masks.masks[1].keyframes.path;
        assert_eq!(keys.len(), 1, "{:?}", r.sent);
        assert_eq!(keys[0].time_secs, 0.0);
        assert!(r.texts.iter().any(|(t, _)| t == "1 key"));

        let p = r.at("Linear", 0);
        r.click(p);
        let p = r.at("Hold", 0);
        r.click(p);
        assert_eq!(r.masks.masks[1].keyframes.path[0].ease, klipp_core::mask_path::PathEase::Hold);

        let d = diamond(&r);
        r.click(d);
        assert!(r.masks.masks[1].keyframes.path.is_empty());
        assert!(matches!(r.masks.masks[1].shape, MaskShape::Bezier { ref points } if points.len() == 3), "the shape kept");
    }
}

/// A Key Mixer's inputs (#898): a row per port, its gain and whether it is
/// inverted, each set by name (`key_mixer_gain_{n}`, `key_mixer_invert_{n}`)
/// as `set_node_param` sets them, one undo step an edit. Before anything is
/// wired, how to wire.
fn key_mixer_inputs(ui: &mut egui::Ui, params: &mut NodeParams, node_id: NodeId, clip_id: ClipId, cmds: &mut Vec<AppCommand>, theme: &KlippTheme) {
    let mixer = params.key_mixer.get_or_insert_with(Default::default);
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::control(ui.next_auto_id(), "key_mixer", "key_mixer", "Inputs", &*mixer);
    if mixer.inputs.is_empty() {
        ui.label(egui::RichText::new("Wire mattes into this node; drop a wire on its left edge to add an input.").color(theme.text_dim).small());
        return;
    }
    for port in 0..mixer.inputs.len() {
        let mut input = mixer.inputs[port];
        if grade_slider(ui, &format!("Key {} gain", port + 1), &mut input.gain, -1.0..=1.0, "", cmds, clip_id, theme) {
            cmds.push(AppCommand::SetNodeParam { clip_id, node_id, param: format!("key_mixer_gain_{port}"), value: input.gain });
        }
        if ui.checkbox(&mut input.invert, "Invert").changed() {
            let value = if input.invert { 1.0 } else { 0.0 };
            cmds.extend([
                AppCommand::BeginNodeParamEdit { clip_id },
                AppCommand::SetNodeParam { clip_id, node_id, param: format!("key_mixer_invert_{port}"), value },
                AppCommand::EndNodeParamEdit { clip_id },
            ]);
        }
        mixer.inputs[port] = input;
    }
}
