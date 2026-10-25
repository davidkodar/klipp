use crate::widgets::menu::MenuRows;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_nodes::{NodeGraph, NodeId, NodeKind};

// ── Strip constants ─────────────────────────────────────────────────────────

const STRIP_NODE_W: f32 = 80.0;
const STRIP_NODE_H: f32 = 50.0;
const STRIP_LABEL_H: f32 = 14.0;
const STRIP_PAD: f32 = 8.0;
const STRIP_GAP: f32 = 4.0;
const WIRE_Y_OFFSET: f32 = STRIP_LABEL_H + (STRIP_NODE_H - STRIP_LABEL_H) * 0.5;

fn node_color(kind: &NodeKind, theme: &KlippTheme) -> egui::Color32 {
    match kind {
        NodeKind::Source | NodeKind::Output => theme.bg_raised,
        NodeKind::Qualifier | NodeKind::LumaKey | NodeKind::Mask | NodeKind::CombineMattes | NodeKind::KeyMixer | NodeKind::DifferenceKey => {
            egui::Color32::from_rgb(0x5c, 0x3a, 0x6e)
        }
        NodeKind::Layer | NodeKind::Blend | NodeKind::ApplyMatte => {
            egui::Color32::from_rgb(0x3a, 0x5c, 0x6e)
        }
        NodeKind::Lut3D | NodeKind::FilmGrain | NodeKind::ColorMatch => {
            egui::Color32::from_rgb(0x5c, 0x5a, 0x3a)
        }
        _ => egui::Color32::from_rgb(0x3a, 0x4a, 0x5c),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// NodeGraphPanel — compact DaVinci-style horizontal node strip
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Default)]
pub struct NodeGraphPanel {
    add_menu_open: bool,
}


impl NodeGraphPanel {
    /// Render the compact node strip. Returns commands.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &mut Option<NodeId>,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        let avail = ui.available_size();
        let strip_h = STRIP_NODE_H + STRIP_PAD * 2.0;

        // Dark background
        let (strip_rect, _) = ui.allocate_exact_size(
            egui::vec2(avail.x, strip_h),
            egui::Sense::hover(),
        );
        let painter = ui.painter_at(strip_rect);
        painter.rect_filled(strip_rect, 0.0, theme.bg_deep);

        // Get topological order for rendering
        let order = graph.topo_sort().unwrap_or_else(|_| {
            graph.nodes.iter().map(|n| n.id).collect()
        });

        // Layout nodes horizontally with auto-spacing
        let total_nodes = order.len();
        let total_w = total_nodes as f32 * (STRIP_NODE_W + STRIP_GAP) - STRIP_GAP;

        // Horizontal scroll if nodes overflow
        egui::ScrollArea::horizontal()
            .id_salt("node_strip_scroll")
            .max_height(strip_h)
            .show_viewport(ui, |ui, _viewport| {
                let (content_rect, _) = ui.allocate_exact_size(
                    egui::vec2(total_w.max(avail.x), STRIP_NODE_H + STRIP_PAD),
                    egui::Sense::click(),
                );
                let painter = ui.painter_at(content_rect);

                // Center nodes vertically in the strip
                let base_y = content_rect.min.y + STRIP_PAD * 0.5;
                let mut x = content_rect.min.x + STRIP_PAD;

                // Track node screen positions for wire drawing
                let mut node_positions: Vec<(NodeId, egui::Rect)> = Vec::new();

                for node_id in &order {
                    let node = match graph.nodes.iter().find(|n| n.id == *node_id) {
                        Some(n) => n,
                        None => continue,
                    };

                    let node_rect = egui::Rect::from_min_size(
                        egui::pos2(x, base_y),
                        egui::vec2(STRIP_NODE_W, STRIP_NODE_H),
                    );
                    node_positions.push((*node_id, node_rect));

                    let is_selected = *selected_node == Some(node.id);

                    // Node body — colored rectangle (thumbnail placeholder)
                    let body_rect = egui::Rect::from_min_max(
                        egui::pos2(node_rect.min.x, node_rect.min.y + STRIP_LABEL_H),
                        node_rect.max,
                    );
                    let fill = if node.enabled {
                        node_color(&node.kind, theme)
                    } else {
                        theme.bg_panel
                    };
                    painter.rect_filled(body_rect, 2.0, fill);

                    // Selection border
                    let border_color = if is_selected { theme.accent } else { theme.border };
                    let border_width = if is_selected { 2.0_f32 } else { 0.5_f32 };
                    painter.rect_stroke(node_rect, 2.0, egui::Stroke::new(border_width, border_color));

                    // Label
                    let label = crate::ui_util::elide(&node.label, 11);
                    let label_color = if node.enabled { theme.text_primary } else { theme.text_dim };
                    painter.text(
                        egui::pos2(node_rect.center().x, node_rect.min.y + STRIP_LABEL_H * 0.5),
                        egui::Align2::CENTER_CENTER,
                        &label,
                        egui::FontId::proportional(10.0),
                        label_color,
                    );

                    // Bypass indicator
                    if !node.enabled {
                        painter.text(
                            body_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "OFF",
                            egui::FontId::proportional(10.0),
                            theme.text_dim,
                        );
                    }

                    // Interaction
                    let resp = ui.allocate_rect(node_rect, egui::Sense::click());
                    if resp.clicked() {
                        *selected_node = Some(node.id);
                    }
                    resp.context_menu_rows(|ui| {
                        if node.kind != NodeKind::Source && node.kind != NodeKind::Output {
                            let toggle = if node.enabled { "Bypass" } else { "Enable" };
                            if ui.button(toggle).clicked() {
                                cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id: node.id });
                                ui.close_menu();
                            }
                            if ui.button("Delete").clicked() {
                                cmds.push(AppCommand::RemoveNode { clip_id, node_id: node.id });
                                if *selected_node == Some(node.id) { *selected_node = None; }
                                ui.close_menu();
                            }
                        }
                    });

                    x += STRIP_NODE_W + STRIP_GAP;
                }

                // Draw wires — simple horizontal lines between connected nodes
                for edge in &graph.edges {
                    let from_rect = node_positions.iter().find(|(id, _)| *id == edge.from_node).map(|(_, r)| r);
                    let to_rect = node_positions.iter().find(|(id, _)| *id == edge.to_node).map(|(_, r)| r);

                    if let (Some(from), Some(to)) = (from_rect, to_rect) {
                        let y = from.min.y + WIRE_Y_OFFSET;
                        let from_x = from.max.x;
                        let to_x = to.min.x;
                        // Only draw if there's a gap between nodes
                        if to_x > from_x + 1.0 {
                            painter.line_segment(
                                [egui::pos2(from_x, y), egui::pos2(to_x, y)],
                                egui::Stroke::new(1.5_f32, theme.accent_dim),
                            );
                        }
                    }
                }

                // Visible "+" button at the right edge of the strip.
                // Same size / vertical alignment as the node rectangles
                // so it reads as part of the row. Right-click on any
                // empty area also opens the menu.
                let plus_size = STRIP_NODE_H * 0.7;
                let plus_rect = egui::Rect::from_min_size(
                    egui::pos2(x + STRIP_PAD, base_y + (STRIP_NODE_H - plus_size) * 0.5),
                    egui::vec2(plus_size, plus_size),
                );
                let plus_resp = ui.allocate_rect(plus_rect, egui::Sense::click())
                    .on_hover_text("Add a node to the chain");
                let plus_color = if plus_resp.hovered() { theme.accent } else { theme.text_dim };
                painter.rect_stroke(
                    plus_rect, 2.0,
                    egui::Stroke::new(1.0_f32, plus_color),
                );
                painter.text(
                    plus_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(plus_size * 0.7),
                    plus_color,
                );
                if plus_resp.clicked() {
                    self.add_menu_open = true;
                }

                // Right-click on empty area also opens the menu (kept
                // for power users who already learned the shortcut).
                if content_rect.contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()))
                    && ui.input(|i| i.pointer.secondary_clicked()) {
                        self.add_menu_open = true;
                    }
            });

        // Add node menu
        if self.add_menu_open {
            let menu_id = ui.id().with("strip_add_node");
            // 70 % of screen height cap (240 px floor) so the 14
            // entries stay reachable on tiny windows / dense
            // dock layouts. Same pattern as the V2 add-node menu
            // and the Add Transition submenu.
            let max_h = (ui.ctx().screen_rect().height() * 0.70).max(240.0);
            egui::Area::new(menu_id)
                .fixed_pos(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("strip_add_node_scroll")
                            .max_height(max_h)
                            .show(ui, |ui| {
                                for &(label, kind_str) in STRIP_ADD_MENU {
                                    if ui.button(label).clicked() {
                                        cmds.push(AppCommand::AddNode {
                                            clip_id,
                                            kind: kind_str.to_string(),
                                            node_id: None,
                                            position: (0.0, 0.0),
                                        });
                                        self.add_menu_open = false;
                                        ui.close_menu();
                                    }
                                }
                            });
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.add_menu_open = false;
                        }
                    });
                });
        }

        cmds
    }
}

/// The strip's add-node menu: label and kind. Every kind is one
/// `add_color_node` can make (`klipp_nodes::kinds::ADDABLE_NODE_KINDS`):
/// a name outside it would add nothing, which the test below holds.
const STRIP_ADD_MENU: &[(&str, &str)] = &[
    ("Exposure", "Exposure"),
    ("Contrast", "Contrast"),
    ("Saturation", "Saturation"),
    ("Tone Controls", "ToneControls"),
    ("Colour Wheels", "ColorWheels"),
    ("Curves", "RGBCurves"),
    ("Hue/Sat", "HueSatCurves"),
    ("Colour Balance", "ColorBalance"),
    ("HDR Zones", "HdrZones"),
    ("Colour Match", "ColorMatch"),
    ("Qualifier", "Qualifier"),
    ("Film Grain", "FilmGrain"),
    ("Colour Warper", "ColorWarper"),
    ("Noise Reduction", "Denoise"),
    ("3D LUT", "Lut3D"),
];

#[cfg(test)]
mod strip_add_menu_tests {
    #[test]
    fn every_kind_the_strip_menu_offers_can_be_added() {
        for (label, kind) in super::STRIP_ADD_MENU {
            assert!(klipp_nodes::kinds::addable_node_kind(kind).is_some(), "{label}: {kind} is not an addable kind");
        }
    }
}
