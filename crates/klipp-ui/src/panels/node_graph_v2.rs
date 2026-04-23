//! V2 node-graph editor — 2D canvas with drag-to-move nodes,
//! drag-to-connect wires, multi-port visualisation, pan + zoom.
//!
//! Replaces (or augments — see toggle) the legacy horizontal strip
//! in `panels/node_graph.rs`. Same data model (`klipp_nodes::NodeGraph`),
//! same `AppCommand` mutation surface (AddNode / RemoveNode /
//! ConnectNodes / DisconnectEdge / MoveNode / ToggleNodeEnabled),
//! so the existing engine + executor + persistence paths are
//! untouched. This file is purely a renderer + interaction layer.
//!
//! Phase 1 of the node-graph V2 project (CG-NGV2-PHASE-1):
//! - Pan via middle-mouse drag (or space + drag); zoom via wheel.
//! - Render every node at its stored `NodePos`, with input ports on
//!   the left edge and output ports on the right edge.
//! - Drag a node body to reposition (`MoveNode` on release).
//! - Drag from an output port to an input port to create an edge
//!   (`ConnectNodes` if compatible — currently any-to-any, future
//!   work will validate type/role).
//! - Right-click on empty canvas → add-node menu at cursor; right-
//!   click on a node → per-node menu (Bypass, Delete, Disconnect).

use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_nodes::{NodeGraph, NodeId, NodeKind, NodePos};

const NODE_W: f32 = 120.0;
const NODE_H: f32 = 60.0;
const NODE_HEADER_H: f32 = 16.0;
const PORT_RADIUS: f32 = 5.0;
const PORT_HIT_RADIUS: f32 = 9.0;
const GRID_SPACING: f32 = 32.0;

fn node_color(kind: &NodeKind, theme: &KlippTheme) -> egui::Color32 {
    match kind {
        NodeKind::Source | NodeKind::Output => theme.bg_raised,
        NodeKind::Qualifier | NodeKind::Mask | NodeKind::CombineMattes => {
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

#[derive(Clone, Copy)]
struct DragNode {
    node_id: NodeId,
    /// Cumulative delta we've already emitted to the engine since
    /// the drag started, in graph coordinates. Used to compute the
    /// next per-frame delta without rounding drift.
    emitted_dx: f32,
    emitted_dy: f32,
    /// Cursor position in graph coordinates at the start of the
    /// drag — combined with `emitted_dx/dy` lets us derive the
    /// new delta to emit each frame from the current cursor.
    start_cursor_x: f32,
    start_cursor_y: f32,
}

#[derive(Clone, Copy)]
struct DragWire {
    from_node: NodeId,
    from_port: u8,
}

pub struct NodeGraphV2Panel {
    pan: egui::Vec2,
    zoom: f32,
    drag_node: Option<DragNode>,
    drag_wire: Option<DragWire>,
    add_menu_open: bool,
    add_menu_screen_pos: egui::Pos2,
    add_menu_graph_pos: egui::Pos2,
    /// Frame on which `add_menu_open` was set. The close-detection
    /// (`clicked_elsewhere`) fires true on the SAME frame for the
    /// secondary click that opened the menu (the click point is at
    /// the menu's top-left corner, which egui sometimes counts as
    /// outside the interact_rect). Skipping the close check on the
    /// open frame is the simplest fix.
    add_menu_opened_frame: u64,
    /// Per-node context menu — opened on right-click on a node.
    /// `(node_id, screen_pos)`; cleared on close.
    node_menu: Option<(NodeId, egui::Pos2)>,
    node_menu_opened_frame: u64,
    /// Cleared on first paint of a new clip (or when `view_dirty`
    /// is set explicitly). Triggers a one-shot fit-to-view so the
    /// graph appears framed on first open instead of off-screen.
    /// Without this the user has to pan around hunting for nodes
    /// because the default `NodePos { x: i*200, y: 0 }` layout
    /// extends to graph x=2400+ for the default 13-node graph,
    /// far beyond the canvas's visible window at zoom=1.
    last_fit_clip: Option<ClipId>,
    /// Set by the toolbar's "Fit" button or programmatically
    /// (clip switch). Consumed at the top of the next `show()`.
    fit_pending: bool,
}

impl Default for NodeGraphV2Panel {
    fn default() -> Self {
        Self {
            pan: egui::Vec2::ZERO,
            zoom: 1.0,
            drag_node: None,
            drag_wire: None,
            add_menu_open: false,
            add_menu_screen_pos: egui::Pos2::ZERO,
            add_menu_graph_pos: egui::Pos2::ZERO,
            add_menu_opened_frame: 0,
            node_menu: None,
            node_menu_opened_frame: 0,
            last_fit_clip: None,
            fit_pending: false,
        }
    }
}

impl NodeGraphV2Panel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &mut Option<NodeId>,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Toolbar — small strip at the top with view controls.
        // Kept tight so the canvas keeps almost all the available
        // height. ~22 px including padding.
        let avail = ui.available_size();
        let toolbar_h = 22.0;
        let (toolbar_rect, _) = ui.allocate_exact_size(
            egui::vec2(avail.x, toolbar_h), egui::Sense::hover(),
        );
        let mut toolbar_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(toolbar_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        toolbar_ui.add_space(6.0);
        let fit_clicked = toolbar_ui.button("Fit").on_hover_text(
            "Frame all nodes in the canvas (shortcut: F)"
        ).clicked();
        let reset_clicked = toolbar_ui.button("1:1").on_hover_text(
            "Reset zoom to 100 % and centre on the graph origin"
        ).clicked();
        toolbar_ui.add_space(8.0);
        toolbar_ui.label(
            egui::RichText::new(format!("zoom {:.0} %", self.zoom * 100.0))
                .small().color(theme.text_dim)
        );
        if reset_clicked {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }

        let (canvas_rect, canvas_resp) = ui.allocate_exact_size(
            egui::vec2(avail.x, (avail.y - toolbar_h).max(80.0)),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter_at(canvas_rect);

        // First-paint auto-fit per clip + on-demand fit via toolbar
        // or `F` shortcut. Done before any rendering so the first
        // frame is already framed instead of needing a second pass.
        let f_pressed = ui.input(|i| {
            i.key_pressed(egui::Key::F)
                && !i.modifiers.ctrl
                && !i.modifiers.command
        });
        let need_fit = fit_clicked
            || f_pressed
            || self.fit_pending
            || self.last_fit_clip != Some(clip_id);
        if need_fit && !graph.nodes.is_empty() {
            self.fit_to_nodes(graph, canvas_rect);
            self.fit_pending = false;
            self.last_fit_clip = Some(clip_id);
        }

        // Background + subtle grid (so pan is visually obvious).
        painter.rect_filled(canvas_rect, 0.0, theme.bg_deep);
        self.draw_grid(&painter, canvas_rect, theme);

        // Pan / zoom interactions.
        self.handle_pan_zoom(ui, &canvas_rect, &canvas_resp);

        // Coordinate-transform helpers — closures capture the current
        // pan + zoom + canvas origin so positions update mid-frame
        // when the user pans.
        let origin = canvas_rect.center();
        let zoom = self.zoom;
        let pan = self.pan;
        let to_screen = |g: egui::Pos2| -> egui::Pos2 {
            origin + (g.to_vec2() + pan) * zoom
        };
        let to_graph = |s: egui::Pos2| -> egui::Pos2 {
            ((s - origin) / zoom).to_pos2() - pan
        };

        // Snapshot node screen rects (for hit-testing + wire endpoints).
        let mut node_rects: Vec<(NodeId, egui::Rect, egui::Color32, NodeKind)> =
            Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let pos = graph
                .node_positions
                .get(&node.id)
                .copied()
                .unwrap_or(NodePos { x: 0.0, y: 0.0 });
            let center = to_screen(egui::pos2(pos.x, pos.y));
            let size = egui::vec2(NODE_W * zoom, NODE_H * zoom);
            let rect = egui::Rect::from_center_size(center, size);
            node_rects.push((node.id, rect, node_color(&node.kind, theme), node.kind));
        }

        // ── Wires ─────────────────────────────────────────────────
        // Existing edges first, then the in-flight wire on top.
        // Colour follows the destination input port's role — matte
        // wires read magenta, references read orange, etc. — so the
        // user can see at a glance "that's a matte feeding here"
        // without having to chase the source node.
        for edge in &graph.edges {
            let Some((_, from_rect, _, from_kind)) =
                node_rects.iter().find(|(id, _, _, _)| *id == edge.from_node).copied()
            else { continue; };
            let Some((_, to_rect, _, to_kind)) =
                node_rects.iter().find(|(id, _, _, _)| *id == edge.to_node).copied()
            else { continue; };
            let from = output_port_pos(&from_rect, edge.from_port, &from_kind);
            let to = input_port_pos(&to_rect, edge.to_port, &to_kind);
            let role = to_kind.input_port_role(edge.to_port);
            let (r, g, b) = role.editor_color();
            // Idle existing edges get a translucent stroke so they
            // don't shout over the foreground; the role colour is
            // still visible.
            let color = egui::Color32::from_rgba_unmultiplied(r, g, b, 180);
            draw_wire(&painter, from, to, color, 1.5 * zoom);
        }

        if let Some(dw) = self.drag_wire {
            if let Some((_, from_rect, _, from_kind)) =
                node_rects.iter().find(|(id, _, _, _)| *id == dw.from_node).copied()
            {
                let from = output_port_pos(&from_rect, dw.from_port, &from_kind);
                let to = ui.input(|i| i.pointer.hover_pos().unwrap_or(from));
                // Drag wire takes its colour from the SOURCE output
                // port's role (we don't know the destination yet) —
                // gives early visual feedback about what kind of
                // signal is being routed.
                let (r, g, b) = from_kind.output_port_role(dw.from_port).editor_color();
                let color = egui::Color32::from_rgb(r, g, b);
                draw_wire(&painter, from, to, color, 2.0 * zoom);
            }
        }

        // ── Nodes ─────────────────────────────────────────────────
        for (idx, (node_id, rect, fill, kind)) in node_rects.iter().copied().enumerate() {
            let node = &graph.nodes[idx];
            let is_selected = *selected_node == Some(node_id);

            // Body
            let header_h = NODE_HEADER_H * zoom;
            let body_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y + header_h),
                rect.max,
            );
            painter.rect_filled(rect, 4.0, theme.bg_panel);
            painter.rect_filled(body_rect, 4.0, if node.enabled { fill } else { theme.bg_panel_alt });

            // Selection / border
            let border = if is_selected { theme.accent } else { theme.border };
            let border_w = if is_selected { 2.0_f32 } else { 0.5_f32 };
            painter.rect_stroke(rect, 4.0, egui::Stroke::new(border_w, border));

            // Label (header)
            let label_color = if node.enabled { theme.text_primary } else { theme.text_dim };
            painter.text(
                egui::pos2(rect.center().x, rect.min.y + header_h * 0.5),
                egui::Align2::CENTER_CENTER,
                &node.label,
                egui::FontId::proportional(10.0 * zoom),
                label_color,
            );

            if !node.enabled {
                painter.text(
                    body_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "OFF",
                    egui::FontId::proportional(9.0 * zoom),
                    theme.text_dim,
                );
            }

            // Input ports (left edge) — role-coloured stroke +
            // hover tooltip naming the port.
            for p in 0..kind.input_port_count() {
                let pos = input_port_pos(&rect, p, &kind);
                let role = kind.input_port_role(p);
                let (rr, gg, bb) = role.editor_color();
                painter.circle_filled(pos, PORT_RADIUS * zoom, theme.bg_deep);
                painter.circle_stroke(
                    pos, PORT_RADIUS * zoom,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(rr, gg, bb)),
                );
                let port_rect = egui::Rect::from_center_size(
                    pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                );
                ui.allocate_rect(port_rect, egui::Sense::hover())
                    .on_hover_text(format!("{} (in)", kind.input_port_label(p)));
            }
            // Output ports (right edge)
            for p in 0..kind.output_port_count() {
                let pos = output_port_pos(&rect, p, &kind);
                let role = kind.output_port_role(p);
                let (rr, gg, bb) = role.editor_color();
                painter.circle_filled(pos, PORT_RADIUS * zoom, theme.bg_deep);
                painter.circle_stroke(
                    pos, PORT_RADIUS * zoom,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(rr, gg, bb)),
                );
                let port_rect = egui::Rect::from_center_size(
                    pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                );
                ui.allocate_rect(port_rect, egui::Sense::hover())
                    .on_hover_text(format!("{} (out)", kind.output_port_label(p)));
            }
        }

        // ── Interactions ─────────────────────────────────────────
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
        let primary_released = ui.input(|i| i.pointer.primary_released());
        let secondary_clicked = ui.input(|i| i.pointer.secondary_clicked());

        // Node-body click + drag → start move; click on output port → start wire.
        if primary_pressed {
            if let Some(p) = pointer_pos {
                if canvas_rect.contains(p) {
                    // Output port hit?
                    let mut wire_started = false;
                    for (node_id, rect, _, kind) in &node_rects {
                        for port in 0..kind.output_port_count() {
                            let port_pos = output_port_pos(rect, port, kind);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                self.drag_wire = Some(DragWire { from_node: *node_id, from_port: port });
                                wire_started = true;
                                break;
                            }
                        }
                        if wire_started { break; }
                    }
                    if !wire_started {
                        // Node body hit?
                        for (node_id, rect, _, _) in &node_rects {
                            if rect.contains(p) {
                                *selected_node = Some(*node_id);
                                let g = to_graph(p);
                                self.drag_node = Some(DragNode {
                                    node_id: *node_id,
                                    emitted_dx: 0.0,
                                    emitted_dy: 0.0,
                                    start_cursor_x: g.x,
                                    start_cursor_y: g.y,
                                });
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Live drag — emit MoveNode delta as the cursor moves.
        if let Some(ref mut dn) = self.drag_node {
            if let Some(p) = pointer_pos {
                let g = to_graph(p);
                let total_dx = g.x - dn.start_cursor_x;
                let total_dy = g.y - dn.start_cursor_y;
                let delta_x = total_dx - dn.emitted_dx;
                let delta_y = total_dy - dn.emitted_dy;
                if delta_x.abs() > 0.01 || delta_y.abs() > 0.01 {
                    cmds.push(AppCommand::MoveNode {
                        clip_id,
                        node_id: dn.node_id,
                        delta_x,
                        delta_y,
                    });
                    dn.emitted_dx = total_dx;
                    dn.emitted_dy = total_dy;
                }
            }
        }

        // Drop — finalise drags.
        if primary_released {
            // Wire drop: did we land on an input port?
            if let Some(dw) = self.drag_wire.take() {
                if let Some(p) = pointer_pos {
                    for (node_id, rect, _, kind) in &node_rects {
                        if *node_id == dw.from_node { continue; }
                        for port in 0..kind.input_port_count() {
                            let port_pos = input_port_pos(rect, port, kind);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                cmds.push(AppCommand::ConnectNodes {
                                    clip_id,
                                    from: dw.from_node,
                                    from_port: dw.from_port,
                                    to: *node_id,
                                    to_port: port,
                                });
                                break;
                            }
                        }
                    }
                }
            }
            // Node drop: nothing to commit; deltas were streamed live.
            self.drag_node = None;
        }

        // Right-click handling.
        if secondary_clicked {
            if let Some(p) = pointer_pos {
                if canvas_rect.contains(p) {
                    // Node hit?
                    let mut on_node = None;
                    for (node_id, rect, _, _) in &node_rects {
                        if rect.contains(p) { on_node = Some(*node_id); break; }
                    }
                    let frame_nr = ui.ctx().cumulative_pass_nr();
                    if let Some(nid) = on_node {
                        self.node_menu = Some((nid, p));
                        self.node_menu_opened_frame = frame_nr;
                        self.add_menu_open = false;
                    } else {
                        self.add_menu_open = true;
                        self.add_menu_screen_pos = p;
                        self.add_menu_graph_pos = to_graph(p);
                        self.add_menu_opened_frame = frame_nr;
                        self.node_menu = None;
                    }
                }
            }
        }

        // ── Floating menus (egui::Area) ──────────────────────────
        if self.add_menu_open {
            self.draw_add_menu(ui, clip_id, &mut cmds);
        }
        if let Some((nid, _pos)) = self.node_menu {
            // Snapshot kind+enabled so we can borrow out of `graph` cleanly
            let info = graph.nodes.iter().find(|n| n.id == nid).map(|n| (n.kind, n.enabled));
            if let Some((kind, enabled)) = info {
                self.draw_node_menu(ui, clip_id, nid, kind, enabled, &mut cmds, selected_node);
            } else {
                self.node_menu = None;
            }
        }

        cmds
    }

    /// Compute zoom + pan that frames every node in the graph
    /// inside the canvas with a comfortable margin. Called on first
    /// open of a clip, on Fit toolbar click, and on `F` press.
    fn fit_to_nodes(&mut self, graph: &NodeGraph, canvas_rect: egui::Rect) {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &graph.nodes {
            let pos = graph.node_positions.get(&node.id).copied()
                .unwrap_or(NodePos { x: 0.0, y: 0.0 });
            // Node bounds in graph coords = (pos ± NODE_W/2, pos ± NODE_H/2)
            let half_w = NODE_W * 0.5;
            let half_h = NODE_H * 0.5;
            min_x = min_x.min(pos.x - half_w);
            min_y = min_y.min(pos.y - half_h);
            max_x = max_x.max(pos.x + half_w);
            max_y = max_y.max(pos.y + half_h);
        }
        if !min_x.is_finite() || !max_x.is_finite() { return; }

        let graph_w = (max_x - min_x).max(1.0);
        let graph_h = (max_y - min_y).max(1.0);
        // 12 % padding on each side so framed nodes don't touch the edge.
        let target_w = canvas_rect.width() * 0.76;
        let target_h = canvas_rect.height() * 0.76;
        let zoom_x = target_w / graph_w;
        let zoom_y = target_h / graph_h;
        let new_zoom = zoom_x.min(zoom_y).clamp(0.25, 3.0);

        // Pan so the graph centre lands on the canvas centre.
        let graph_cx = (min_x + max_x) * 0.5;
        let graph_cy = (min_y + max_y) * 0.5;
        // Inverse of `to_screen`: screen = origin + (graph + pan) * zoom
        // We want graph_centre to map to canvas_centre, so:
        //   canvas_centre = canvas_centre + (graph_centre + pan) * zoom
        //   pan = -graph_centre
        self.zoom = new_zoom;
        self.pan = egui::vec2(-graph_cx, -graph_cy);
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: egui::Rect, theme: &KlippTheme) {
        let zoom = self.zoom;
        // Skip the grid when fully zoomed out — lines fuse into a wash.
        if zoom < 0.4 { return; }
        let spacing = GRID_SPACING * zoom;
        let origin = rect.center() + self.pan * zoom;
        let stroke = egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_unmultiplied(
            theme.accent_dim.r(), theme.accent_dim.g(), theme.accent_dim.b(), 30,
        ));
        // Vertical
        let mut x = origin.x;
        while x > rect.min.x { x -= spacing; }
        while x < rect.max.x {
            painter.line_segment([egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)], stroke);
            x += spacing;
        }
        // Horizontal
        let mut y = origin.y;
        while y > rect.min.y { y -= spacing; }
        while y < rect.max.y {
            painter.line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], stroke);
            y += spacing;
        }
    }

    fn handle_pan_zoom(
        &mut self,
        ui: &mut egui::Ui,
        canvas_rect: &egui::Rect,
        canvas_resp: &egui::Response,
    ) {
        // Pan: middle-mouse drag, OR primary drag while holding space.
        let space_held = ui.input(|i| i.modifiers.shift && i.pointer.primary_down())
            || ui.input(|i| i.key_down(egui::Key::Space));
        let middle_dragging = ui.input(|i| i.pointer.middle_down());
        let panning = middle_dragging || (space_held && canvas_resp.dragged());

        if panning {
            let delta = ui.input(|i| i.pointer.delta());
            if delta.length_sq() > 0.0 {
                self.pan += delta / self.zoom;
            }
        }

        // Zoom: mouse wheel, anchored on the cursor (so the point under
        // the cursor stays put through the zoom).
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            if let Some(cursor) = ui.input(|i| i.pointer.hover_pos()) {
                if canvas_rect.contains(cursor) {
                    let factor = (1.0 + scroll * 0.0015).clamp(0.5, 1.5);
                    let new_zoom = (self.zoom * factor).clamp(0.25, 3.0);
                    if (new_zoom - self.zoom).abs() > 1e-4 {
                        // Keep cursor's graph point fixed during zoom.
                        let origin = canvas_rect.center();
                        let graph_pt = ((cursor - origin) / self.zoom).to_pos2() - self.pan;
                        self.zoom = new_zoom;
                        let new_screen = origin + (graph_pt.to_vec2() + self.pan) * self.zoom;
                        self.pan += (cursor - new_screen) / self.zoom;
                    }
                }
            }
        }
    }

    fn draw_add_menu(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
    ) {
        let menu_id = ui.id().with("ngv2_add_menu");
        let mut close = false;
        // egui::Area returns a response we can use to detect
        // outside-clicks via `clicked_elsewhere`. Doing the close
        // logic THIS way instead of a global `primary_clicked()`
        // check avoids the bug where the same right-click that just
        // opened the menu (this frame) also tripped the close.
        let area_resp = egui::Area::new(menu_id)
            .fixed_pos(self.add_menu_screen_pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    for (label, kind_str) in [
                        ("Exposure", "Exposure"),
                        ("Contrast", "Contrast"),
                        ("Saturation", "Saturation"),
                        ("Tone Controls", "ToneControls"),
                        ("Color Wheels", "ColorWheels"),
                        ("Wheel Sat/Bri", "WheelSatBri"),
                        ("Curves", "RGBCurves"),
                        ("Hue/Sat", "HueSatCurves"),
                        ("Color Balance", "ColorBalance"),
                        ("Color Match", "ColorMatch"),
                        ("Qualifier", "Qualifier"),
                        ("Mask", "Mask"),
                        ("Combine Mattes", "CombineMattes"),
                        ("Layer", "Layer"),
                        ("Blend", "Blend"),
                        ("Apply Matte", "ApplyMatte"),
                        ("Film Grain", "FilmGrain"),
                        ("Color Warper", "ColorWarper"),
                        ("Noise Reduction", "Denoise"),
                        ("3D LUT", "Lut3D"),
                    ] {
                        if ui.button(label).clicked() {
                            cmds.push(AppCommand::AddNode {
                                clip_id,
                                kind: kind_str.to_string(),
                                position: (self.add_menu_graph_pos.x, self.add_menu_graph_pos.y),
                            });
                            close = true;
                        }
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        // Skip outside-click detection on the same frame the menu
        // was opened — the secondary click that opened it can
        // register as `clicked_elsewhere` if its landing point is
        // a pixel off the area's interact_rect.
        if ui.ctx().cumulative_pass_nr() != self.add_menu_opened_frame
            && area_resp.response.clicked_elsewhere()
        {
            close = true;
        }
        if close { self.add_menu_open = false; }
    }

    fn draw_node_menu(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        node_id: NodeId,
        kind: NodeKind,
        enabled: bool,
        cmds: &mut Vec<AppCommand>,
        selected_node: &mut Option<NodeId>,
    ) {
        let pos = self.node_menu.map(|(_, p)| p).unwrap_or_default();
        let menu_id = ui.id().with(("ngv2_node_menu", node_id));
        let mut close = false;
        let area_resp = egui::Area::new(menu_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if matches!(kind, NodeKind::Source | NodeKind::Output) {
                        ui.label("(no actions)");
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                        return;
                    }
                    let toggle = if enabled { "Bypass" } else { "Enable" };
                    if ui.button(toggle).clicked() {
                        cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
                        close = true;
                    }
                    if ui.button("Delete").clicked() {
                        cmds.push(AppCommand::RemoveNode { clip_id, node_id });
                        if *selected_node == Some(node_id) { *selected_node = None; }
                        close = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        // Same open-frame guard as the add menu — see the long
        // comment in `draw_add_menu`. Without this the secondary
        // click that opened the menu also fires `clicked_elsewhere`
        // on the same frame and the menu vanishes before the user
        // sees it (~9/10 of the time).
        if ui.ctx().cumulative_pass_nr() != self.node_menu_opened_frame
            && area_resp.response.clicked_elsewhere()
        {
            close = true;
        }
        if close { self.node_menu = None; }
    }
}

// ── Port position helpers ─────────────────────────────────────────
// Ports are evenly distributed along the left (input) / right
// (output) edge of the node body rectangle.

fn input_port_pos(rect: &egui::Rect, port: u8, kind: &NodeKind) -> egui::Pos2 {
    let count = kind.input_port_count().max(1);
    port_pos_on_edge(rect, port, count, false)
}

fn output_port_pos(rect: &egui::Rect, port: u8, kind: &NodeKind) -> egui::Pos2 {
    let count = kind.output_port_count().max(1);
    port_pos_on_edge(rect, port, count, true)
}

fn port_pos_on_edge(rect: &egui::Rect, port: u8, count: u8, output: bool) -> egui::Pos2 {
    let body_top = rect.min.y + NODE_HEADER_H * (rect.height() / NODE_H);
    let body_h = rect.max.y - body_top;
    let frac = (port as f32 + 0.5) / count as f32;
    let y = body_top + body_h * frac;
    let x = if output { rect.max.x } else { rect.min.x };
    egui::pos2(x, y)
}

fn draw_wire(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32) {
    // Cubic bezier with horizontal handles whose length scales with
    // the X distance — gives the classic "S" curve when nodes are
    // side-by-side and a clean "U" when wired backwards.
    let dx = (to.x - from.x).abs().max(40.0);
    let h = dx * 0.5;
    let c1 = egui::pos2(from.x + h, from.y);
    let c2 = egui::pos2(to.x - h, to.y);
    let stroke = egui::Stroke::new(width.max(1.0), color);

    // Approximate cubic with a polyline (egui has no native bezier
    // primitive at this stroke level).
    const STEPS: usize = 24;
    let mut prev = from;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let p = egui::pos2(
            mt * mt * mt * from.x + 3.0 * mt * mt * t * c1.x + 3.0 * mt * t * t * c2.x + t * t * t * to.x,
            mt * mt * mt * from.y + 3.0 * mt * mt * t * c1.y + 3.0 * mt * t * t * c2.y + t * t * t * to.y,
        );
        painter.line_segment([prev, p], stroke);
        prev = p;
    }
}
