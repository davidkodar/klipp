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
        NodeKind::Qualifier | NodeKind::LumaKey | NodeKind::Mask | NodeKind::CombineMattes | NodeKind::ExternalMatte => {
            egui::Color32::from_rgb(0x5c, 0x3a, 0x6e)
        }
        NodeKind::Layer | NodeKind::Blend | NodeKind::ApplyMatte | NodeKind::LayerMixer => {
            egui::Color32::from_rgb(0x3a, 0x5c, 0x6e)
        }
        NodeKind::Lut3D | NodeKind::FilmGrain | NodeKind::ColorMatch => {
            egui::Color32::from_rgb(0x5c, 0x5a, 0x3a)
        }
        // Compound gets a distinctive teal so it reads as
        // "structural" at a glance (different from the orange/red
        // grade-node palette) — reinforced by the stacked-card
        // shadow painted behind it in the render loop.
        NodeKind::Compound => egui::Color32::from_rgb(0x2f, 0x5e, 0x60),
        _ => egui::Color32::from_rgb(0x3a, 0x4a, 0x5c),
    }
}

/// Add `n` to each channel (saturating). Used to lift "active" node
/// fills slightly above their idle palette so an enabled node reads
/// as live without the colour drifting off the kind-palette hue.
fn lift(c: egui::Color32, n: u8) -> egui::Color32 {
    egui::Color32::from_rgb(
        c.r().saturating_add(n),
        c.g().saturating_add(n),
        c.b().saturating_add(n),
    )
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
    /// Live search query for the add-node menu. Cleared every time
    /// the menu re-opens (each invocation is a fresh search). Match
    /// is case-insensitive substring against the entry's display
    /// label — predictable enough that "sat" hits Saturation /
    /// Wheel Sat/Bri / Hue/Sat without surprising matches the way
    /// subsequence-style fuzzy can.
    add_menu_search: String,
    /// Recently-added node kinds (kind_str values), most-recent
    /// first, capped at 6 entries. Surfaced as a "Recent" row at
    /// the top of the add-node menu so the user's last few picks
    /// are one click away. In-memory only; resets on app restart.
    add_menu_recents: Vec<&'static str>,
    /// Set of category names whose disclosure triangle is
    /// collapsed. In-memory; defaults to all-expanded so first-time
    /// users see every entry. When the search field is non-empty
    /// this state is ignored (matches across all categories paint
    /// inline).
    add_menu_collapsed: std::collections::HashSet<&'static str>,
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
    /// Per-ExternalMatte-node thumbnail cache. Keyed by node id so
    /// two ExternalMatte nodes with different images don't collide;
    /// the stored `PathBuf` lets us invalidate when the user picks a
    /// new image. The `TextureHandle` is uploaded via
    /// `ctx.load_texture` and dropped from egui's pool on panel
    /// destruction (or when the entry is overwritten on path change).
    matte_thumbs: std::collections::HashMap<NodeId, (std::path::PathBuf, egui::TextureHandle)>,
    /// Per-node output-thumbnail textures. Keyed by node id; value is
    /// `(node_thumbs_gen at upload time, texture handle)`. Re-uploads
    /// when the engine bumps `FrameBuffer::node_thumbs_gen`; otherwise
    /// reuses the cached texture.
    output_thumbs: std::collections::HashMap<NodeId, (u64, egui::TextureHandle)>,
    /// The set of node-ids we last told the engine to capture thumbs
    /// for, so we can avoid re-sending `SetVisibleNodeThumbs` every
    /// frame. Snapshotted at end of each render; compared against the
    /// current frame's visible set on the next render.
    last_visible_thumb_nodes: Vec<NodeId>,
    /// Box-select / marquee state. Set when the user presses the
    /// primary mouse button on empty canvas (no node, no port hit);
    /// the live `current` is updated on every drag frame so the
    /// rectangle paints as it grows. Cleared on release: any node
    /// whose body rectangle intersects the marquee gets added to
    /// `selected_set`. Shift-drag adds to the existing selection;
    /// plain drag replaces it (the press-time clear already wiped
    /// the previous set in that case).
    box_select: Option<(egui::Pos2, egui::Pos2)>,
    /// Last primary-press timestamp + screen position. Used for
    /// manual double-click detection on node bodies (egui's raw
    /// input API doesn't expose a button_double_clicked on this
    /// version; rolling our own keeps the behaviour under our
    /// control — 400 ms window, 8 px tolerance).
    last_primary_press: Option<(std::time::Instant, egui::Pos2)>,
    /// Wireless-link label editor. `Some((node_id, screen_pos, buf))`
    /// when the user chose "Set wireless label…" in the node menu;
    /// rendered as a small floating popup with a text input + Save /
    /// Clear / Cancel. None otherwise.
    wireless_label_editor: Option<(NodeId, egui::Pos2, String)>,
}

impl Default for NodeGraphV2Panel {
    fn default() -> Self {
        Self {
            pan: egui::Vec2::ZERO,
            zoom: 1.0,
            drag_node: None,
            drag_wire: None,
            box_select: None,
            last_primary_press: None,
            add_menu_open: false,
            add_menu_screen_pos: egui::Pos2::ZERO,
            add_menu_graph_pos: egui::Pos2::ZERO,
            add_menu_opened_frame: 0,
            add_menu_search: String::new(),
            add_menu_recents: Vec::new(),
            add_menu_collapsed: std::collections::HashSet::new(),
            node_menu: None,
            node_menu_opened_frame: 0,
            last_fit_clip: None,
            fit_pending: false,
            matte_thumbs: std::collections::HashMap::new(),
            output_thumbs: std::collections::HashMap::new(),
            last_visible_thumb_nodes: Vec::new(),
            wireless_label_editor: None,
        }
    }
}

impl NodeGraphV2Panel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &mut Option<NodeId>,
        selected_set: &mut std::collections::HashSet<NodeId>,
        graph: &mut NodeGraph,
        clip_id: ClipId,
        theme: &KlippTheme,
        engine_node_thumbs: &std::collections::HashMap<NodeId, Vec<u8>>,
        engine_node_thumbs_gen: u64,
        deferred_drops: &mut Vec<egui::TextureHandle>,
        descent_path: &mut Vec<NodeId>,
        breadcrumb_labels: &[String],
        grade_version_names: &[String],
        active_grade_version: Option<usize>,
        clip_time_secs: f64,
        port_snap_enabled: bool,
        port_snap_radius_px: f32,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Self-heal: drop any stale ids from the selection set that
        // no longer exist in the current clip's graph (clip-switch,
        // upstream RemoveNode the engine has already applied, etc.).
        // Keeps the set bounded + makes downstream `set.contains(id)`
        // checks trustworthy without forcing every caller to clear it
        // on every transition.
        if !selected_set.is_empty() {
            selected_set.retain(|id| graph.nodes.iter().any(|n| n.id == *id));
            if let Some(p) = *selected_node {
                if !graph.nodes.iter().any(|n| n.id == p) {
                    *selected_node = selected_set.iter().next().copied();
                }
            }
        }

        // Toolbar — small strip at the top with view controls.
        // Breadcrumb row — only drawn when descended into one or
        // more compounds. "Root" always pops back to clip level;
        // each intermediate segment truncates the descent path to
        // that depth. Clicks mutate `descent_path` directly (no
        // AppCommand needed — it's UI-local state) and clear
        // selection so the inspector doesn't linger on a node that
        // lives in a different depth than the one we're now showing.
        let avail = ui.available_size();
        if !descent_path.is_empty() {
            let breadcrumb_h = 20.0;
            let (bc_rect, _) = ui.allocate_exact_size(
                egui::vec2(avail.x, breadcrumb_h), egui::Sense::hover(),
            );
            let mut bc_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(bc_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            ui.painter().rect_filled(bc_rect, 0.0, theme.bg_panel);
            bc_ui.add_space(8.0);
            if bc_ui.link(
                egui::RichText::new("Root").small().color(theme.accent)
            ).clicked() {
                descent_path.clear();
                selected_set.clear();
                *selected_node = None;
            }
            let depth = descent_path.len();
            for (i, label) in breadcrumb_labels.iter().enumerate() {
                bc_ui.label(
                    egui::RichText::new(" › ").small().color(theme.text_dim)
                );
                if i + 1 == depth {
                    // Current level — plain label, not clickable.
                    bc_ui.label(
                        egui::RichText::new(label).small().color(theme.text_primary)
                    );
                } else if bc_ui.link(
                    egui::RichText::new(label).small().color(theme.accent)
                ).clicked() {
                    descent_path.truncate(i + 1);
                    selected_set.clear();
                    *selected_node = None;
                }
            }
        }

        // Kept tight so the canvas keeps almost all the available
        // height. ~22 px including padding.
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
        // Grade-version picker. Shows "V: none" when no versions
        // exist, "V: V1 ▾" when one is active, with a menu listing
        // every saved version plus save / update / rename / delete
        // actions. Clicking a version triggers SwitchGradeVersion;
        // "Save as new version" picks the next auto-name (V1 / V2 /
        // ...). Rename / delete act on the currently-active version.
        let active_name = active_grade_version
            .and_then(|i| grade_version_names.get(i).map(|s| s.as_str()));
        let menu_label = match active_name {
            Some(n) => format!("V: {n}"),
            None if grade_version_names.is_empty() => "V: +".to_string(),
            None => "V: —".to_string(),
        };
        toolbar_ui.menu_button(menu_label, |mb_ui| {
            // Existing versions — click to switch.
            for (i, name) in grade_version_names.iter().enumerate() {
                let marker = if Some(i) == active_grade_version { "●" } else { "○" };
                if mb_ui.button(format!("{marker}  {name}")).clicked() {
                    cmds.push(AppCommand::SwitchGradeVersion {
                        clip_id,
                        version_index: i,
                    });
                    mb_ui.close_menu();
                }
            }
            if !grade_version_names.is_empty() {
                mb_ui.separator();
            }
            if mb_ui.button("Save as new version").clicked() {
                // Auto-numbered "V1", "V2", … based on current count.
                let name = format!("V{}", grade_version_names.len() + 1);
                cmds.push(AppCommand::SaveGradeVersion { clip_id, name });
                mb_ui.close_menu();
            }
            if let Some(idx) = active_grade_version {
                if mb_ui.button("Update active version with current").clicked() {
                    cmds.push(AppCommand::UpdateActiveGradeVersion { clip_id });
                    mb_ui.close_menu();
                }
                if mb_ui.button("Delete active version").clicked() {
                    cmds.push(AppCommand::DeleteGradeVersion {
                        clip_id,
                        version_index: idx,
                    });
                    mb_ui.close_menu();
                }
            }
        });
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

        // Pan / zoom interactions. Suppressed while a popup menu is
        // open (right-click add-node menu, per-node context menu, or
        // the wireless-label editor) so scroll-wheel events inside
        // the popup don't ALSO zoom the canvas behind it. Egui's
        // ScrollArea consumes scroll for its own content, but the
        // raw scroll delta we read here is global — without the gate
        // both the menu and the canvas react.
        let any_menu_open = self.add_menu_open
            || self.node_menu.is_some()
            || self.wireless_label_editor.is_some();
        if !any_menu_open {
            self.handle_pan_zoom(ui, &canvas_rect, &canvas_resp);
        }

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
        // The trailing u8 is the *dynamic* input-port count — needed
        // because LayerMixer grows past its kind's static count and
        // the editor must render every wired port + accept new ones.
        let mut node_rects: Vec<(NodeId, egui::Rect, egui::Color32, NodeKind, u8)> =
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
            let in_count = node.dynamic_input_port_count();
            node_rects.push((node.id, rect, node_color(&node.kind, theme), node.kind, in_count));
        }

        // ── Wires ─────────────────────────────────────────────────
        // Existing edges first, then the in-flight wire on top.
        // Colour follows the destination input port's role — matte
        // wires read magenta, references read orange, etc. — so the
        // user can see at a glance "that's a matte feeding here"
        // without having to chase the source node.
        //
        // Edge hover: if the cursor is within ~8 px of an edge's
        // bezier polyline AND no gesture is in flight (no drag wire,
        // no box-select, not over a node body), the closest edge
        // gets a thicker, brighter stroke. Cheap — N edges × 24 line-
        // segments × one squared-distance check per segment per
        // frame; redraw triggers from cursor-move are already handled
        // by egui, so this adds zero continuous-repaint cost.
        let cursor = ui.input(|i| i.pointer.hover_pos());
        let mut hovered_edge: Option<usize> = None;
        let cursor_in_canvas = cursor.is_some_and(|c| canvas_rect.contains(c));
        let cursor_in_node = cursor.is_some_and(|c| {
            node_rects.iter().any(|(_, r, _, _, _)| r.contains(c))
        });
        let edge_hover_eligible = self.drag_wire.is_none()
            && self.box_select.is_none()
            && cursor_in_canvas
            && !cursor_in_node;
        if edge_hover_eligible {
            if let Some(c) = cursor {
                let threshold_sq = (8.0 * zoom).powi(2);
                let mut best_sq = threshold_sq;
                for (idx, edge) in graph.edges.iter().enumerate() {
                    let Some((_, from_rect, _, from_kind, _)) =
                        node_rects.iter().find(|(id, _, _, _, _)| *id == edge.from_node).copied()
                    else { continue; };
                    let Some((_, to_rect, _, _, to_in_count)) =
                        node_rects.iter().find(|(id, _, _, _, _)| *id == edge.to_node).copied()
                    else { continue; };
                    let from = output_port_pos(&from_rect, edge.from_port, &from_kind);
                    let to = input_port_pos(&to_rect, edge.to_port, to_in_count);
                    let pts = bezier_polyline(from, to);
                    let d = distance_to_polyline_sq(&pts, c);
                    if d < best_sq {
                        best_sq = d;
                        hovered_edge = Some(idx);
                    }
                }
            }
        }

        for (idx, edge) in graph.edges.iter().enumerate() {
            let Some((_, from_rect, _, from_kind, _)) =
                node_rects.iter().find(|(id, _, _, _, _)| *id == edge.from_node).copied()
            else { continue; };
            let Some((_, to_rect, _, to_kind, to_in_count)) =
                node_rects.iter().find(|(id, _, _, _, _)| *id == edge.to_node).copied()
            else { continue; };
            let from = output_port_pos(&from_rect, edge.from_port, &from_kind);
            let to = input_port_pos(&to_rect, edge.to_port, to_in_count);
            let role = to_kind.input_port_role(edge.to_port);
            let (r, g, b) = role.editor_color();
            // Idle existing edges get a translucent stroke so they
            // don't shout over the foreground; the role colour is
            // still visible. Hovered edge: full alpha + ~1.6× width
            // so it pops without redrawing the rest of the graph.
            let is_hover = hovered_edge == Some(idx);
            let alpha = if is_hover { 255 } else { 180 };
            let width = if is_hover { 2.5 } else { 1.5 } * zoom;
            let color = egui::Color32::from_rgba_unmultiplied(r, g, b, alpha);
            draw_wire(&painter, from, to, color, width);
        }

        // Snap target: while a drag wire is in flight, find the
        // closest input port within the configured snap radius. The
        // visible drag wire endpoint locks to it AND a soft ring is
        // painted around the port so the user sees exactly where the
        // connection will land. Computed once here and reused by the
        // drop hit-test below so the highlight and the actual drop
        // can never disagree (no "snapped visually but landed
        // somewhere else" surprises).
        //
        // Tuple is `(node_id, port_index, port_pos)`. None when:
        //   - no drag in flight
        //   - cursor unavailable
        //   - snap is disabled in prefs
        //   - no input port within the radius
        let snap_target: Option<(NodeId, u8, egui::Pos2)> = if let (Some(dw), Some(p), true) =
            (self.drag_wire, ui.input(|i| i.pointer.hover_pos()), port_snap_enabled)
        {
            let radius = port_snap_radius_px * zoom.max(0.5);
            let radius_sq = radius * radius;
            let mut best_d_sq = radius_sq;
            let mut best: Option<(NodeId, u8, egui::Pos2)> = None;
            for (node_id, rect, _, _, in_count) in &node_rects {
                if *node_id == dw.from_node { continue; }
                for port in 0..*in_count {
                    let port_pos = input_port_pos(rect, port, *in_count);
                    let d_sq = (p - port_pos).length_sq();
                    if d_sq < best_d_sq {
                        best_d_sq = d_sq;
                        best = Some((*node_id, port, port_pos));
                    }
                }
            }
            best
        } else {
            None
        };

        if let Some(dw) = self.drag_wire {
            if let Some((_, from_rect, _, from_kind, _)) =
                node_rects.iter().find(|(id, _, _, _, _)| *id == dw.from_node).copied()
            {
                let from = output_port_pos(&from_rect, dw.from_port, &from_kind);
                // Endpoint snaps to the nearest input port if one is
                // within range; otherwise tracks the cursor. Falling
                // back to `from` on no-cursor keeps the wire degenerate-
                // but-not-stale on rare missing-pointer frames.
                let to = snap_target
                    .map(|(_, _, pos)| pos)
                    .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                    .unwrap_or(from);
                // Drag wire takes its colour from the SOURCE output
                // port's role (we don't know the destination yet) —
                // gives early visual feedback about what kind of
                // signal is being routed. Painted DASHED so the user
                // can tell at a glance the connection isn't committed
                // yet (committed edges always paint solid above).
                let (r, g, b) = from_kind.output_port_role(dw.from_port).editor_color();
                let color = egui::Color32::from_rgb(r, g, b);
                draw_wire_dashed(&painter, from, to, color, 2.0 * zoom);

                // Snap highlight: a soft outer ring around the target
                // port in the source role's colour, plus a small dot
                // at the centre so the eye lands on the lock point.
                // Painted on top of the wire so the snap cue is the
                // brightest thing on the canvas.
                if let Some((_, _, pos)) = snap_target {
                    let ring_color = egui::Color32::from_rgba_unmultiplied(r, g, b, 140);
                    painter.circle_stroke(
                        pos,
                        PORT_RADIUS * zoom * 1.7,
                        egui::Stroke::new(2.0, ring_color),
                    );
                    painter.circle_filled(
                        pos,
                        PORT_RADIUS * zoom * 0.55,
                        egui::Color32::from_rgb(r, g, b),
                    );
                }
            }
        }

        // Box-select marquee — translucent amber fill + 1 px outline
        // so it's visible against any canvas background but never
        // hides what it overlaps. Painted before the nodes so node
        // bodies render on top (the marquee is a UI hint, not a
        // foreground element).
        if let Some((start, current)) = self.box_select {
            let rect = egui::Rect::from_two_pos(start, current);
            let fill = egui::Color32::from_rgba_unmultiplied(
                theme.accent.r(), theme.accent.g(), theme.accent.b(), 36,
            );
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(
                rect, 0.0,
                egui::Stroke::new(1.0, theme.accent),
            );
        }

        // ── Nodes ─────────────────────────────────────────────────
        // Hoist the pointer position so we can gate per-port tooltip
        // registration on "cursor is actually near this node". Without
        // the gate, allocating a hover Sense rect + on_hover_text on
        // every port every frame keeps egui's tooltip-fade animation
        // ticking, which calls request_repaint() until the fade
        // settles — and with N ports per node the cursor parked
        // anywhere on the canvas almost always sits inside one. Net
        // effect: 100 % CPU just by having the editor open.
        let hover_pointer = ui.input(|i| i.pointer.hover_pos());
        for (idx, (node_id, rect, fill, kind, in_count)) in node_rects.iter().copied().enumerate() {
            let node = &graph.nodes[idx];
            let node_on = klipp_nodes::effective_enabled(node, clip_time_secs);
            // Selection paints amber whether the node is the
            // inspector-primary OR part of a multi-selection set.
            // Both paths must paint the same so the user can't tell
            // the primary apart from "also-selected" — that would be
            // confusing during a Group / Delete-all gesture.
            let is_selected =
                *selected_node == Some(node_id) || selected_set.contains(&node_id);

            // Body
            let header_h = NODE_HEADER_H * zoom;
            let body_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y + header_h),
                rect.max,
            );
            // Drop shadow — two stops of premultiplied black,
            // offset down + slightly grown so the node feels like
            // it's sitting above the canvas rather than printed on
            // it. Offsets are in screen pixels (not multiplied by
            // zoom) — that matches how a real drop shadow scales
            // and keeps the depth cue legible at extreme zoom-out.
            // Painted before the Compound stack ghosts so a single
            // shadow sits under the whole stack rather than each
            // ghost casting its own competing shadow.
            {
                let shadow_far = egui::Color32::from_rgba_premultiplied(0, 0, 0, 22);
                let shadow_near = egui::Color32::from_rgba_premultiplied(0, 0, 0, 36);
                painter.rect_filled(
                    rect.translate(egui::vec2(0.0, 4.0)).expand(3.0),
                    5.5, shadow_far,
                );
                painter.rect_filled(
                    rect.translate(egui::vec2(0.0, 2.0)).expand(1.5),
                    4.5, shadow_near,
                );
            }
            // Compound "stacked cards" preview: paint two offset
            // ghosts behind the real node body so the shape reads as
            // "multiple nodes packed into one" at a glance. Only
            // renders for Compound kind — every other node stays the
            // same single card. Shadow colours pick up the node's
            // fill darkened, so the stack reads even against dark
            // canvas backgrounds.
            if kind == NodeKind::Compound {
                let shade = |c: egui::Color32, d: u8| {
                    egui::Color32::from_rgb(
                        c.r().saturating_sub(d),
                        c.g().saturating_sub(d),
                        c.b().saturating_sub(d),
                    )
                };
                let off = 4.0 * zoom;
                let ghost2 = rect.translate(egui::vec2(off * 2.0, off * 2.0));
                let ghost1 = rect.translate(egui::vec2(off, off));
                painter.rect_filled(ghost2, 4.0, shade(fill, 70));
                painter.rect_stroke(ghost2, 4.0, egui::Stroke::new(0.5, theme.border));
                painter.rect_filled(ghost1, 4.0, shade(fill, 35));
                painter.rect_stroke(ghost1, 4.0, egui::Stroke::new(0.5, theme.border));
            }
            painter.rect_filled(rect, 4.0, theme.bg_panel);
            // Active nodes get a small lift on the kind-palette fill so
            // they read as live; bypassed nodes fall back to bg_panel_alt
            // (no fill colour at all). The lift is intentionally
            // conservative — too much washes out the kind-hue and
            // makes Compound/Layer/Qualifier rows harder to scan at a
            // glance.
            let body_fill = if node_on { lift(fill, 14) } else { theme.bg_panel_alt };
            painter.rect_filled(body_rect, 4.0, body_fill);

            // Internal top-edge highlight — a 1-px line just below
            // the rect's top edge that picks up "light from above".
            // Uses theme.overlay() so polarity flips on light themes
            // (becomes a soft dark line on white instead of an
            // invisible white-on-white). Inset 4 px from each side
            // so it doesn't poke out past the rounded corners.
            {
                let y = rect.min.y + 1.0;
                let highlight = theme.overlay(if is_selected { 80 } else { 50 });
                painter.line_segment(
                    [
                        egui::pos2(rect.min.x + 4.0, y),
                        egui::pos2(rect.max.x - 4.0, y),
                    ],
                    egui::Stroke::new(1.0, highlight),
                );
            }

            // Selection state: subtle amber halo + accent border.
            // The halo is two outer concentric strokes at decreasing
            // alpha so the selected node "lights up" without feeling
            // neon. Painted before the crisp 2 px border so the border
            // sits on top and the focus state stays unambiguous.
            if is_selected {
                let halo = theme.accent;
                let halo_a1 = egui::Color32::from_rgba_unmultiplied(
                    halo.r(), halo.g(), halo.b(), 70,
                );
                let halo_a2 = egui::Color32::from_rgba_unmultiplied(
                    halo.r(), halo.g(), halo.b(), 32,
                );
                painter.rect_stroke(
                    rect.expand(2.0),
                    5.0,
                    egui::Stroke::new(1.5, halo_a1),
                );
                painter.rect_stroke(
                    rect.expand(4.0),
                    6.0,
                    egui::Stroke::new(1.5, halo_a2),
                );
            }
            let border = if is_selected { theme.accent } else { theme.border };
            // Idle border bumped 0.5 → 0.9 px so the card edge has
            // some bite against the dimmed canvas grid; the new
            // shadow + top-highlight tier the depth, the border
            // crisps the silhouette so the eye lands on the node
            // before the canvas. Selected border keeps 2.0 px so
            // the focus state still dominates.
            let border_w = if is_selected { 2.0_f32 } else { 0.9_f32 };
            painter.rect_stroke(rect, 4.0, egui::Stroke::new(border_w, border));

            // Label (header)
            let label_color = if node_on { theme.text_primary } else { theme.text_dim };
            painter.text(
                egui::pos2(rect.center().x, rect.min.y + header_h * 0.5),
                egui::Align2::CENTER_CENTER,
                &node.label,
                egui::FontId::proportional(10.0 * zoom),
                label_color,
            );

            // Compound body: overlay "N nodes" + a hint that double-
            // click descends. No thumbnail for compounds yet (the
            // executor doesn't recurse into sub-graphs — that lands
            // with stage 5); for now the count is the best visual
            // cue that the node "contains something".
            if kind == NodeKind::Compound && node_on {
                let n = node.params.compound_subgraph
                    .as_ref()
                    .map(|g| {
                        // Don't count the synthetic Source / Output
                        // wrappers the Group operation adds — the
                        // user-meaningful count is the packed nodes.
                        g.nodes.iter()
                            .filter(|n| !matches!(n.kind,
                                NodeKind::Source | NodeKind::Output))
                            .count()
                    })
                    .unwrap_or(0);
                let label = if n == 1 { "1 node".to_string() }
                    else { format!("{n} nodes") };
                painter.text(
                    body_rect.center() - egui::vec2(0.0, 5.0 * zoom),
                    egui::Align2::CENTER_CENTER,
                    &label,
                    egui::FontId::proportional(11.0 * zoom),
                    theme.text_primary,
                );
                painter.text(
                    body_rect.center() + egui::vec2(0.0, 8.0 * zoom),
                    egui::Align2::CENTER_CENTER,
                    "(double-click)",
                    egui::FontId::proportional(8.0 * zoom),
                    theme.text_dim,
                );
            }

            if !node_on {
                painter.text(
                    body_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "OFF",
                    egui::FontId::proportional(9.0 * zoom),
                    theme.text_dim,
                );
            }

            // Wireless-link chips. Painted above the node body so they
            // read as "extra" state, not part of the header.
            //   - Sender chip: top-right corner, showing "⚲ label".
            //   - Receiver chips: one per input port that has a label
            //     set, painted above the port row on the left edge.
            // A tiny cyan dot inside the chip ties them visually to
            // the wireless feature. Hidden at extreme zoom-out so the
            // canvas stays readable.
            if zoom >= 0.55 {
                let chip_fg = egui::Color32::from_rgb(120, 200, 220);
                let chip_bg = egui::Color32::from_rgba_unmultiplied(
                    chip_fg.r(), chip_fg.g(), chip_fg.b(), 48);
                let chip_font = egui::FontId::proportional(9.0 * zoom);
                if let Some(label) = node.output_link.as_ref() {
                    let text = format!("⚲ {label}");
                    let galley = painter.layout_no_wrap(text, chip_font.clone(), chip_fg);
                    let pad = egui::vec2(6.0, 3.0) * zoom;
                    let chip_size = galley.size() + pad * 2.0;
                    let chip_pos = egui::pos2(
                        rect.max.x - chip_size.x - 4.0 * zoom,
                        rect.min.y - chip_size.y - 2.0 * zoom,
                    );
                    let chip_rect = egui::Rect::from_min_size(chip_pos, chip_size);
                    painter.rect_filled(chip_rect, 3.0, chip_bg);
                    painter.rect_stroke(chip_rect, 3.0, egui::Stroke::new(0.5, chip_fg));
                    painter.galley(chip_rect.min + pad, galley, chip_fg);
                }
                for (port, slot) in node.input_links.iter().enumerate() {
                    if let Some(label) = slot {
                        let port_pos = input_port_pos(&rect, port as u8, in_count);
                        let text = format!("{label} ⚲");
                        let galley = painter.layout_no_wrap(text, chip_font.clone(), chip_fg);
                        let pad = egui::vec2(5.0, 2.0) * zoom;
                        let chip_size = galley.size() + pad * 2.0;
                        let chip_pos = egui::pos2(
                            port_pos.x - chip_size.x - 6.0 * zoom,
                            port_pos.y - chip_size.y * 0.5,
                        );
                        let chip_rect = egui::Rect::from_min_size(chip_pos, chip_size);
                        painter.rect_filled(chip_rect, 3.0, chip_bg);
                        painter.rect_stroke(chip_rect, 3.0, egui::Stroke::new(0.5, chip_fg));
                        painter.galley(chip_rect.min + pad, galley, chip_fg);
                    }
                }
            }

            // ExternalMatte: paint the loaded image as a thumbnail
            // inside the node body so the user sees which mask is
            // attached without having to click through the inspector.
            if kind == NodeKind::ExternalMatte {
                if let Some(path) = node.params.matte_image_path.as_ref() {
                    let cached = self.matte_thumbs.get(&node_id);
                    let needs_load = match cached {
                        Some((p, _)) => p != path,
                        None => true,
                    };
                    if needs_load {
                        if let Ok(img) = image::open(path) {
                            let rgba = img.to_rgba8();
                            let size = [rgba.width() as usize, rgba.height() as usize];
                            let pixels = rgba.as_flat_samples();
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                size, pixels.as_slice(),
                            );
                            let name = format!("matte_thumb_{}", node_id.as_simple());
                            let tex = ui.ctx().load_texture(
                                name, color_image, egui::TextureOptions::LINEAR,
                            );
                            // Defer-drop the previous handle; see the
                            // comment on output_thumbs for the wgpu
                            // "Texture has been destroyed" panic this
                            // avoids.
                            if let Some((_, old)) = self.matte_thumbs.remove(&node_id) {
                                deferred_drops.push(old);
                            }
                            self.matte_thumbs.insert(node_id, (path.clone(), tex));
                        }
                    }
                    if let Some((_, tex)) = self.matte_thumbs.get(&node_id) {
                        // Inset the thumb inside the node body with a
                        // small margin so the magenta body rim still
                        // reads as "this is a matte node".
                        let inset = 4.0 * zoom;
                        let thumb_rect = body_rect.shrink(inset);
                        painter.image(
                            tex.id(),
                            thumb_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                } else {
                    // No image set — defer-drop any stale texture so a
                    // fresh pick starts clean. See output_thumbs for
                    // why this is a defer rather than a direct drop.
                    if let Some((_, old)) = self.matte_thumbs.remove(&node_id) {
                        deferred_drops.push(old);
                    }
                }
            }

            // Output thumbnail — per-node small preview of the node's
            // output on the last paused / post-seek re-grade. Empty
            // during playback (engine skips capture) and for mid-GPU-
            // chain nodes (output buffer is a sentinel). ExternalMatte
            // nodes already painted their own source-image thumb
            // above; let that one win.
            if kind != NodeKind::ExternalMatte {
                if let Some(thumb_bytes) = engine_node_thumbs.get(&node_id) {
                    if !thumb_bytes.is_empty() {
                        let needs_upload = match self.output_thumbs.get(&node_id) {
                            Some((gen, _)) => *gen != engine_node_thumbs_gen,
                            None => true,
                        };
                        if needs_upload {
                            // Thumb is 192×108 sRGB u8 RGBA per the
                            // executor's THUMB_W/THUMB_H. Fixed so we
                            // don't need to pass dimensions — both
                            // sides agree. Bump both constants in
                            // lockstep if you change the size.
                            const TW: usize = 192;
                            const TH: usize = 108;
                            if thumb_bytes.len() == TW * TH * 4 {
                                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                    [TW, TH], thumb_bytes,
                                );
                                let name = format!("node_out_thumb_{}", node_id.as_simple());
                                let tex = ui.ctx().load_texture(
                                    name, color_image, egui::TextureOptions::LINEAR,
                                );
                                // Replacing the map entry would drop
                                // the old handle synchronously while
                                // egui-wgpu still has a submit in
                                // flight that references it — the
                                // "Texture with 'egui_texid_Managed(N)'
                                // label has been destroyed" panic.
                                // Defer the drop to a later frame (see
                                // `AppState::deferred_texture_drops`).
                                if let Some((_, old)) = self.output_thumbs.remove(&node_id) {
                                    deferred_drops.push(old);
                                }
                                self.output_thumbs.insert(
                                    node_id, (engine_node_thumbs_gen, tex),
                                );
                            }
                        }
                        if let Some((_, tex)) = self.output_thumbs.get(&node_id) {
                            let inset = 4.0 * zoom;
                            let thumb_rect = body_rect.shrink(inset);
                            painter.image(
                                tex.id(),
                                thumb_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                } else {
                    // Engine didn't capture for this node (playback, or
                    // node not in visible set) — defer-drop any stale
                    // thumb so wgpu's in-flight submit completes before
                    // the TextureHandle's destructor runs.
                    if let Some((_, old)) = self.output_thumbs.remove(&node_id) {
                        deferred_drops.push(old);
                    }
                }
            }

            // Pointer-proximity gate — only register port tooltips for
            // nodes the cursor is actually near. Saves egui from
            // animating tooltip fades on every port of every node and
            // burning the CPU at idle.
            let node_hover_band = rect.expand(PORT_HIT_RADIUS * 2.0);
            let cursor_near_node = hover_pointer
                .map(|p| node_hover_band.contains(p))
                .unwrap_or(false);

            // Input ports (left edge) — role-coloured stroke +
            // hover tooltip naming the port. Use the dynamic count so
            // a LayerMixer with 5 wires shows 5 ports.
            for p in 0..in_count {
                let pos = input_port_pos(&rect, p, in_count);
                let role = kind.input_port_role(p);
                let (rr, gg, bb) = role.editor_color();
                painter.circle_filled(pos, PORT_RADIUS * zoom, theme.bg_deep);
                painter.circle_stroke(
                    pos, PORT_RADIUS * zoom,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(rr, gg, bb)),
                );
                if cursor_near_node {
                    let port_rect = egui::Rect::from_center_size(
                        pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                    );
                    ui.allocate_rect(port_rect, egui::Sense::hover())
                        .on_hover_text(format!("{} (in)", kind.input_port_label(p)));
                }
            }
            // LayerMixer "+" affordance — a small ghost port just
            // below the last input port hinting that you can drop a
            // wire on the empty stretch of the left edge to add a new
            // port. Pure visual; the actual port allocation happens
            // on wire drop in the input-port hit-test fall-through.
            if kind == NodeKind::LayerMixer {
                let last = input_port_pos(&rect, in_count.saturating_sub(1), in_count);
                let next = input_port_pos(
                    &rect,
                    in_count,
                    in_count.saturating_add(1),
                );
                // Halfway between last existing port and where the
                // next would land, clamped so it doesn't escape the
                // node body when the count is high.
                let hint_y = ((last.y + next.y) * 0.5).clamp(rect.min.y + 12.0, rect.max.y - 4.0);
                let hint_pos = egui::pos2(rect.min.x, hint_y);
                let role = NodeKind::LayerMixer.input_port_role(0);
                let (rr, gg, bb) = role.editor_color();
                painter.circle_stroke(
                    hint_pos, PORT_RADIUS * zoom * 0.7,
                    egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 90),
                    ),
                );
                painter.text(
                    hint_pos,
                    egui::Align2::CENTER_CENTER,
                    "+",
                    egui::FontId::proportional(8.0 * zoom),
                    egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 140),
                );
            }

            // Output ports (right edge) — same proximity gate as inputs.
            for p in 0..kind.output_port_count() {
                let pos = output_port_pos(&rect, p, &kind);
                let role = kind.output_port_role(p);
                let (rr, gg, bb) = role.editor_color();
                painter.circle_filled(pos, PORT_RADIUS * zoom, theme.bg_deep);
                painter.circle_stroke(
                    pos, PORT_RADIUS * zoom,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(rr, gg, bb)),
                );
                if cursor_near_node {
                    let port_rect = egui::Rect::from_center_size(
                        pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                    );
                    ui.allocate_rect(port_rect, egui::Sense::hover())
                        .on_hover_text(format!("{} (out)", kind.output_port_label(p)));
                }
            }
        }

        // ── Interactions ─────────────────────────────────────────
        let pointer_pos = ui.input(|i| i.pointer.hover_pos());
        let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
        let primary_released = ui.input(|i| i.pointer.primary_released());
        let secondary_clicked = ui.input(|i| i.pointer.secondary_clicked());

        // Node-body click + drag → start move; click on output port → start wire.
        //
        // Suppress the canvas handler when a floating menu (node
        // context menu or add-node menu) is already open — the press
        // belongs to the menu's button, not to the canvas. Without
        // this guard every menu-button click ALSO ran through the
        // canvas selection logic: cursor wasn't over a node body so
        // `node_hit=false` → `selected_set.clear()`. By the time the
        // button's `.clicked()` fired on release the selection had
        // already been wiped, Group-into-Compound saw `inside.len() <
        // 2` and silently returned. Gating here lets the menu's own
        // `clicked_elsewhere` close-detection handle outside-clicks.
        let menu_open = self.node_menu.is_some() || self.add_menu_open;
        if primary_pressed && !menu_open {
            if let Some(p) = pointer_pos {
                if canvas_rect.contains(p) {
                    // Output port hit?
                    let mut wire_started = false;
                    for (node_id, rect, _, kind, _) in &node_rects {
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
                        let shift = ui.input(|i| i.modifiers.shift);
                        // Double-click on a Compound node descends
                        // into its sub-graph. Must fire ALONGSIDE
                        // the single-click selection (the second
                        // press of a double-click is itself a
                        // primary_pressed) so selection still
                        // updates; the DescendIntoCompound handler
                        // then clears selection anyway on success.
                        // Manual double-click detection — press
                        // within 400 ms and 8 px of the previous
                        // press counts as a double-click.
                        let now = std::time::Instant::now();
                        let primary_double_clicked = self
                            .last_primary_press
                            .map(|(t, prev)| {
                                now.duration_since(t)
                                    < std::time::Duration::from_millis(400)
                                    && (p - prev).length() < 8.0
                            })
                            .unwrap_or(false);
                        self.last_primary_press = Some((now, p));
                        let mut node_hit = false;
                        for (node_id, rect, _, kind, _) in &node_rects {
                            if rect.contains(p) {
                                if primary_double_clicked
                                    && *kind == NodeKind::Compound
                                {
                                    cmds.push(AppCommand::DescendIntoCompound {
                                        clip_id,
                                        node_id: *node_id,
                                    });
                                    node_hit = true;
                                    break;
                                }
                                if shift {
                                    // Shift-click toggles set membership.
                                    // Adding a node also makes it the
                                    // primary so the inspector follows
                                    // the user's hand. Removing a node
                                    // promotes any remaining set member
                                    // to primary, or clears primary if
                                    // the set just emptied.
                                    if selected_set.contains(node_id) {
                                        selected_set.remove(node_id);
                                        if *selected_node == Some(*node_id) {
                                            *selected_node = selected_set.iter().next().copied();
                                        }
                                    } else {
                                        selected_set.insert(*node_id);
                                        *selected_node = Some(*node_id);
                                    }
                                } else {
                                    // Plain click replaces the
                                    // selection with this single node.
                                    selected_set.clear();
                                    selected_set.insert(*node_id);
                                    *selected_node = Some(*node_id);
                                }
                                let g = to_graph(p);
                                self.drag_node = Some(DragNode {
                                    node_id: *node_id,
                                    emitted_dx: 0.0,
                                    emitted_dy: 0.0,
                                    start_cursor_x: g.x,
                                    start_cursor_y: g.y,
                                });
                                node_hit = true;
                                break;
                            }
                        }
                        // Click on empty canvas (not on a node body)
                        // clears the selection — matches every
                        // canvas-style editor (Photoshop, Figma, etc.).
                        // Excludes shift-click so the user can build
                        // up a selection without losing it on a near-
                        // miss between nodes. The press also seeds a
                        // box-select marquee; if the user releases
                        // without moving it stays a click, otherwise
                        // the released-frame branch adds intersected
                        // nodes to the set.
                        if !node_hit {
                            if !shift {
                                selected_set.clear();
                                *selected_node = None;
                            }
                            self.box_select = Some((p, p));
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
            // Wire drop: did we land on an input port? For LayerMixer
            // we additionally allow dropping anywhere on the node's
            // left edge — that allocates a new port at index `in_count`
            // and the engine grows the params Vec to match.
            if let Some(dw) = self.drag_wire.take() {
                // Snap-target wins over per-port hit-test if it's set
                // — the user saw the wire visually lock onto that port,
                // dropping anywhere within the snap radius MUST land
                // there or the visual feedback would be a lie.
                if let Some((to_node, to_port, _)) = snap_target {
                    cmds.push(AppCommand::ConnectNodes {
                        clip_id,
                        from: dw.from_node,
                        from_port: dw.from_port,
                        to: to_node,
                        to_port,
                    });
                } else if let Some(p) = pointer_pos {
                    'drop: for (node_id, rect, _, kind, in_count) in &node_rects {
                        if *node_id == dw.from_node { continue; }
                        let in_count = *in_count;
                        let mut hit_existing = false;
                        for port in 0..in_count {
                            let port_pos = input_port_pos(rect, port, in_count);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                cmds.push(AppCommand::ConnectNodes {
                                    clip_id,
                                    from: dw.from_node,
                                    from_port: dw.from_port,
                                    to: *node_id,
                                    to_port: port,
                                });
                                hit_existing = true;
                                break;
                            }
                        }
                        if hit_existing { break 'drop; }
                        // LayerMixer fall-through: drop on left edge
                        // band → grow port count by 1 and connect to
                        // the new port. The band is the full left edge
                        // of the node body (not just the header strip)
                        // so users have a generous target.
                        if *kind == NodeKind::LayerMixer {
                            let edge_band = egui::Rect::from_min_max(
                                egui::pos2(rect.min.x - PORT_HIT_RADIUS, rect.min.y),
                                egui::pos2(rect.min.x + PORT_HIT_RADIUS, rect.max.y),
                            );
                            if edge_band.contains(p) {
                                cmds.push(AppCommand::ConnectNodes {
                                    clip_id,
                                    from: dw.from_node,
                                    from_port: dw.from_port,
                                    to: *node_id,
                                    to_port: in_count,
                                });
                                break 'drop;
                            }
                        }
                    }
                }
            }
            // Node drop: nothing to commit; deltas were streamed live.
            self.drag_node = None;

            // Box-select drop: any node whose body intersects the
            // marquee rect joins the selection set. The press-time
            // branch already cleared the previous set (unless
            // shift-drag), so this is a pure additive pass — works
            // for both "replace via marquee" and "shift-marquee to
            // extend". Tiny marquees (< 4 px on either axis) are
            // treated as a click and dropped silently — that's the
            // user clicking on empty canvas, which the press-time
            // branch already handled.
            if let Some((start, _)) = self.box_select.take() {
                let end = pointer_pos.unwrap_or(start);
                let marquee = egui::Rect::from_two_pos(start, end);
                if marquee.width() >= 4.0 || marquee.height() >= 4.0 {
                    let mut last_added: Option<NodeId> = None;
                    for (node_id, rect, _, _, _) in &node_rects {
                        if marquee.intersects(*rect) {
                            selected_set.insert(*node_id);
                            last_added = Some(*node_id);
                        }
                    }
                    // Promote the last intersected node to primary so
                    // the inspector lands on something concrete.
                    if let Some(id) = last_added {
                        *selected_node = Some(id);
                    }
                }
            }
        }

        // Live box-select: keep `current` in sync with the cursor so
        // the rendered marquee tracks the drag. No-op when not in a
        // marquee gesture.
        if let Some((_, ref mut current)) = self.box_select {
            if let Some(p) = pointer_pos {
                *current = p;
            }
        }

        // Delete key — bulk-remove every node in the selection set.
        // Source / Output are skipped (they're structural; the per-
        // node menu hides actions on them too). The set is cleared
        // for the survivors so the next render isn't holding stale
        // ids that the engine just removed. Gated on `wants_keyboard_
        // input` so the user typing into a slider doesn't accidentally
        // wipe nodes.
        if !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && !selected_set.is_empty()
        {
            let to_delete: Vec<NodeId> = selected_set
                .iter()
                .copied()
                .filter(|id| {
                    graph
                        .nodes
                        .iter()
                        .find(|n| n.id == *id)
                        .map(|n| !matches!(n.kind, NodeKind::Source | NodeKind::Output))
                        .unwrap_or(false)
                })
                .collect();
            for id in &to_delete {
                cmds.push(AppCommand::RemoveNode { clip_id, node_id: *id });
                selected_set.remove(id);
                if *selected_node == Some(*id) {
                    *selected_node = None;
                }
            }
        }

        // Right-click handling. Three cases, in priority order:
        //   1. Near an input port that has an edge → disconnect it.
        //      Fastest way to clear a wire without going through a menu.
        //   2. On a node → per-node context menu (Bypass / Delete / …).
        //   3. Empty canvas → add-node menu.
        if secondary_clicked {
            if let Some(p) = pointer_pos {
                if canvas_rect.contains(p) {
                    // 1) Input-port disconnect. Only fires when the
                    // cursor is LEFT of the node body — otherwise a
                    // right-click anywhere near the node's left edge
                    // would eat the event before the node context
                    // menu could open (the port hit radius overlaps
                    // the node body by design).
                    let mut disconnected = false;
                    for (node_id, rect, _, _, in_count) in &node_rects {
                        if p.x > rect.min.x { continue; }
                        for port in 0..*in_count {
                            let port_pos = input_port_pos(rect, port, *in_count);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                let has_edge = graph.edges.iter().any(|e|
                                    e.to_node == *node_id && e.to_port == port
                                );
                                if has_edge {
                                    cmds.push(AppCommand::DisconnectEdge {
                                        clip_id, to: *node_id, to_port: port,
                                    });
                                    disconnected = true;
                                }
                                break;
                            }
                        }
                        if disconnected { break; }
                    }
                    if disconnected {
                        // Suppress the node/add menu on this click —
                        // we consumed the right-click for the disconnect.
                    } else {
                        // 2/3) Fall through to the node or add menu.
                        let mut on_node = None;
                        for (node_id, rect, _, _, _) in &node_rects {
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
                            // Each menu invocation is a fresh search.
                            self.add_menu_search.clear();
                            self.add_menu_opened_frame = frame_nr;
                            self.node_menu = None;
                        }
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
                self.draw_node_menu(ui, clip_id, nid, kind, enabled, &mut cmds, selected_node, selected_set, graph);
            } else {
                self.node_menu = None;
            }
        }
        // Wireless-label editor popup. Rendered after the node menu
        // so opening it from the menu works in the same frame.
        self.draw_wireless_label_editor(ui, clip_id, &mut cmds);

        // Tell the engine which nodes want thumbnails. Diff against
        // the previous set so we don't resend the same list every
        // frame — the engine only needs to know when the visible set
        // changes. We ask for thumbs of every grade-pipeline node on
        // screen (Source/Output are structural — no value showing
        // their source pixels; skip).
        let mut current_visible: Vec<NodeId> = node_rects.iter()
            .filter(|(_, _, _, kind, _)| !matches!(kind, NodeKind::Source | NodeKind::Output))
            .map(|(id, _, _, _, _)| *id)
            .collect();
        current_visible.sort();
        if current_visible != self.last_visible_thumb_nodes {
            cmds.push(AppCommand::SetVisibleNodeThumbs {
                clip_id,
                node_ids: current_visible.clone(),
            });
            self.last_visible_thumb_nodes = current_visible;
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
        // Grid alpha was 30; dropped to 16 so the canvas chrome stays
        // present (pan/zoom is still readable) without competing with
        // the node bodies for visual weight.
        let stroke = egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_unmultiplied(
            theme.accent_dim.r(), theme.accent_dim.g(), theme.accent_dim.b(), 16,
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
        // Salt the menu id with `add_menu_opened_frame` so each open
        // is a fresh egui::Area / ScrollArea layer with no cached
        // state from previous invocations (the Area's saved size
        // could otherwise carry a tiny rect over from a prior search
        // session into the next open).
        let menu_id = ui.id().with(("ngv2_add_menu", self.add_menu_opened_frame));
        let search_id = menu_id.with("search");
        let mut close = false;
        let mut chosen_kind: Option<&'static str> = None;
        // egui::Area returns a response we can use to detect
        // outside-clicks via `clicked_elsewhere`. Doing the close
        // logic THIS way instead of a global `primary_clicked()`
        // check avoids the bug where the same right-click that just
        // opened the menu (this frame) also tripped the close.
        // Cap at 70 % of screen height (floor 240 px) so the
        // entries stay reachable on small viewports / scaled
        // displays / dock layouts that leave the node-graph panel
        // less than the full window height. ScrollArea handles
        // wheel scroll for free; same pattern the Add Transition
        // submenu uses on the timeline side.
        let max_h = (ui.ctx().screen_rect().height() * 0.70).max(240.0);
        // Categories — display order top-to-bottom in the menu when
        // search is empty. Source of truth for both the flat-search
        // path AND the categorised path; entries the user can pick
        // are exactly these.
        const CATEGORIES: &[(&str, &[(&str, &str)])] = &[
            ("Primary Grade", &[
                ("Corrector (all-in-one)", "Corrector"),
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
            ]),
            ("Secondary", &[
                ("Qualifier", "Qualifier"),
                ("Luma Key", "LumaKey"),
                ("Soft Clip", "SoftClip"),
                ("Mask", "Mask"),
                ("External Matte", "ExternalMatte"),
                ("Combine Mattes", "CombineMattes"),
                ("Mask Cutout", "MaskCutout"),
            ]),
            ("Compositing", &[
                ("Layer", "Layer"),
                ("Layer Mixer", "LayerMixer"),
                ("Blend", "Blend"),
                ("Apply Matte", "ApplyMatte"),
            ]),
            ("Utilities", &[
                ("3D LUT", "Lut3D"),
                ("Film Grain", "FilmGrain"),
                ("Color Warper", "ColorWarper"),
                ("Noise Reduction", "Denoise"),
                ("RGB Splitter", "RGBSplitter"),
                ("RGB Combiner", "RGBCombiner"),
            ]),
            ("Structural", &[
                ("Compound", "Compound"),
            ]),
        ];

        // Lookup helper — recents store kind_str; we want to render
        // them with their display label, so we walk CATEGORIES once.
        let label_for_kind = |kind: &str| -> Option<&'static str> {
            for (_, entries) in CATEGORIES {
                for (label, k) in *entries {
                    if *k == kind { return Some(*label); }
                }
            }
            None
        };

        let area_resp = egui::Area::new(menu_id)
            .fixed_pos(self.add_menu_screen_pos)
            .order(egui::Order::Foreground)
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    // Lock the popup width so it doesn't oscillate
                    // as the visible-entry count changes while the
                    // user types. Without this, a 2-match result
                    // measures narrower than the 29-entry list, the
                    // popup reflows narrower, then on backspace the
                    // popup has to grow back — which egui sometimes
                    // resolves a frame late and looks janky.
                    ui.set_min_width(260.0);
                    ui.set_max_width(260.0);

                    // ── Search field ─────────────────────────────
                    let search_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.add_menu_search)
                            .id(search_id)
                            .hint_text("Search nodes…")
                            .desired_width(200.0),
                    );
                    // Auto-focus on first frame the menu is shown so
                    // the user can start typing immediately. egui
                    // grabs focus exactly once per request_focus()
                    // call; calling it on the open-frame only is the
                    // minimum to land focus without stealing it back
                    // from any subsequent interaction inside the menu.
                    if ui.ctx().cumulative_pass_nr() == self.add_menu_opened_frame {
                        search_resp.request_focus();
                    }
                    ui.separator();

                    let q = self.add_menu_search.trim().to_lowercase();
                    let in_search = !q.is_empty();
                    let matches = |label: &str| -> bool {
                        if !in_search { return true; }
                        label.to_lowercase().contains(&q)
                    };

                    egui::ScrollArea::vertical()
                        .id_salt(("ngv2_add_node_scroll", self.add_menu_opened_frame))
                        // auto_shrink Y = false: the ScrollArea
                        // always fills max_height, so backspacing
                        // a search query restores the full-size
                        // menu rather than the popup retaining the
                        // shrunk-to-a-handful-of-matches height
                        // from when the query had matches. X stays
                        // shrink-to-content so the popup width
                        // doesn't blow up to fill the screen.
                        .auto_shrink([true, false])
                        .max_height(max_h)
                        .show(ui, |ui| {
                            // ── Recent (only when not searching) ──
                            // Surfaces the user's last few picks at
                            // the top so the common case ("add the
                            // same kind I just added on the previous
                            // node") is one click away. Hidden when
                            // empty so first-open is uncluttered.
                            if !in_search && !self.add_menu_recents.is_empty() {
                                ui.label(egui::RichText::new("RECENT").small().weak());
                                for kind in self.add_menu_recents.clone() {
                                    if let Some(label) = label_for_kind(kind) {
                                        if ui.button(label).clicked() {
                                            chosen_kind = Some(kind);
                                        }
                                    }
                                }
                                ui.separator();
                            }

                            // ── Categories ───────────────────────
                            for (cat_name, entries) in CATEGORIES {
                                // Pre-filter so we know whether the
                                // category has any visible entries
                                // before painting its header — empty
                                // categories during search would
                                // otherwise leave naked headers.
                                let visible: Vec<_> = entries.iter()
                                    .filter(|(label, _)| matches(label))
                                    .collect();
                                if visible.is_empty() { continue; }

                                if in_search {
                                    // Search mode: skip collapse UI;
                                    // show every match flat under a
                                    // weak category caption so the
                                    // user can still tell which
                                    // bucket each entry came from.
                                    ui.label(egui::RichText::new(cat_name.to_uppercase())
                                        .small().weak());
                                    for (label, kind_str) in visible {
                                        if ui.button(*label).clicked() {
                                            chosen_kind = Some(*kind_str);
                                        }
                                    }
                                } else {
                                    // Idle mode: collapsible header.
                                    // Tracking by name (not by index)
                                    // means re-orderings won't flip
                                    // a user's collapsed sections.
                                    let collapsed = self.add_menu_collapsed.contains(*cat_name);
                                    let arrow = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
                                    let header_label = format!("{arrow}  {cat_name}");
                                    let header_resp = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(header_label).strong()
                                        ).sense(egui::Sense::click()),
                                    );
                                    if header_resp.clicked() {
                                        if collapsed {
                                            self.add_menu_collapsed.remove(*cat_name);
                                        } else {
                                            self.add_menu_collapsed.insert(*cat_name);
                                        }
                                    }
                                    if !collapsed {
                                        ui.indent(("ngv2_add_cat", *cat_name), |ui| {
                                            for (label, kind_str) in visible {
                                                if ui.button(*label).clicked() {
                                                    chosen_kind = Some(*kind_str);
                                                }
                                            }
                                        });
                                    }
                                }
                            }

                            // Empty-state for search-with-no-hits.
                            if in_search {
                                let any = CATEGORIES.iter().any(|(_, entries)| {
                                    entries.iter().any(|(label, _)| matches(label))
                                });
                                if !any {
                                    ui.add_space(6.0);
                                    ui.label(egui::RichText::new("No matches").weak());
                                }
                            }
                        });
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
        if let Some(kind_str) = chosen_kind {
            cmds.push(AppCommand::AddNode {
                clip_id,
                kind: kind_str.to_string(),
                position: (self.add_menu_graph_pos.x, self.add_menu_graph_pos.y),
            });
            // Bump kind to the front of recents (dedupe + cap 6).
            // Storing the &'static str (not the display label) means
            // a future label rename doesn't poison the recents list.
            self.add_menu_recents.retain(|k| *k != kind_str);
            self.add_menu_recents.insert(0, kind_str);
            self.add_menu_recents.truncate(6);
            close = true;
        }
        if close { self.add_menu_open = false; }
    }

    fn draw_wireless_label_editor(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((node_id, pos, _)) = self.wireless_label_editor.as_ref().map(
            |(n, p, b)| (*n, *p, b.clone())
        ) else { return; };
        let mut close = false;
        let mut save = false;
        let mut clear = false;
        let area_id = ui.id().with(("ngv2_wireless_edit", node_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Wireless send label").small().weak());
                    if let Some((_, _, buf)) = self.wireless_label_editor.as_mut() {
                        let resp = ui.add(
                            egui::TextEdit::singleline(buf)
                                .hint_text("e.g. Matte_A")
                                .desired_width(160.0),
                        );
                        if resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            save = true;
                        }
                        resp.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() { save = true; }
                        if ui.button("Clear").clicked() { clear = true; }
                        if ui.button("Cancel").clicked() { close = true; }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if save {
            if let Some((_, _, buf)) = self.wireless_label_editor.as_ref() {
                let label = buf.trim();
                cmds.push(AppCommand::SetNodeOutputLink {
                    clip_id, node_id,
                    label: if label.is_empty() { None } else { Some(label.to_string()) },
                });
            }
            close = true;
        }
        if clear {
            cmds.push(AppCommand::SetNodeOutputLink {
                clip_id, node_id, label: None,
            });
            close = true;
        }
        if area_resp.response.clicked_elsewhere() {
            close = true;
        }
        if close { self.wireless_label_editor = None; }
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
        selected_set: &mut std::collections::HashSet<NodeId>,
        graph: &NodeGraph,
    ) {
        let pos = self.node_menu.map(|(_, p)| p).unwrap_or_default();
        let menu_id = ui.id().with(("ngv2_node_menu", node_id));
        let mut close = false;
        let area_resp = egui::Area::new(menu_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    // Source / Output only get the wireless controls
                    // below — bypass / delete / group don't apply to
                    // them, so skip those entries entirely.
                    let is_structural = matches!(kind, NodeKind::Source | NodeKind::Output);
                    if !is_structural {
                        let toggle = if enabled { "Bypass" } else { "Enable" };
                        if ui.button(toggle).clicked() {
                            cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
                            close = true;
                        }
                    }
                    let in_set = selected_set.contains(&node_id);
                    // Group-into-Compound: only offered when the
                    // right-clicked node is part of a multi-selection
                    // of 2+ nodes. Engine-side handler validates the
                    // selection's external-edge shape and rejects
                    // gracefully (toast) if the v1 single-port
                    // restriction would be violated.
                    if !is_structural && in_set && selected_set.len() >= 2 {
                        let label = format!(
                            "Group {} into Compound",
                            selected_set.len(),
                        );
                        if ui.button(label).clicked() {
                            cmds.push(AppCommand::GroupIntoCompound {
                                clip_id,
                                node_ids: selected_set.iter().copied().collect(),
                            });
                            close = true;
                        }
                    }
                    // Ungroup — only for compound nodes. Explodes
                    // the nested sub-graph back into the parent.
                    // Shown in addition to Group (which only fires
                    // on multi-selection), so right-clicking a
                    // compound always gets Ungroup regardless of
                    // how many other nodes happen to be selected.
                    if kind == NodeKind::Compound {
                        if ui.button("Ungroup Compound").clicked() {
                            cmds.push(AppCommand::UngroupCompound {
                                clip_id,
                                node_id,
                            });
                            close = true;
                        }
                    }

                    // Wireless-link controls: a sender label on the
                    // primary output (one per node) and per-input-port
                    // receiver labels that resolve to whichever node
                    // currently broadcasts the matching label.
                    // Source / Output are structural, so Source gets
                    // only the send control and Output only the receive.
                    let node_ref = graph.nodes.iter().find(|n| n.id == node_id);
                    let current_send = node_ref.and_then(|n| n.output_link.clone());
                    let current_receives: Vec<Option<String>> = node_ref
                        .map(|n| n.input_links.clone()).unwrap_or_default();
                    let in_count = node_ref.map(|n| n.dynamic_input_port_count()).unwrap_or(0);
                    let can_send = kind != NodeKind::Output;
                    let can_receive = kind != NodeKind::Source && in_count > 0;
                    let available_labels: Vec<String> = graph.nodes.iter()
                        .filter(|n| n.id != node_id)
                        .filter_map(|n| n.output_link.clone())
                        .collect();

                    if can_send {
                        let send_label = match &current_send {
                            Some(l) => format!("Wireless: sending \"{l}\"…"),
                            None => "Wireless: set send label…".to_string(),
                        };
                        if ui.button(send_label).clicked() {
                            self.wireless_label_editor = Some((
                                node_id, pos,
                                current_send.clone().unwrap_or_default(),
                            ));
                            close = true;
                        }
                    }
                    if can_receive && !available_labels.is_empty() {
                        ui.menu_button("Wireless: receive from ▸", |ui| {
                            for port in 0..in_count {
                                let cur = current_receives.get(port as usize)
                                    .and_then(|s| s.clone());
                                ui.label(egui::RichText::new(format!("Port {port}"))
                                    .weak());
                                for lab in &available_labels {
                                    let active = cur.as_ref() == Some(lab);
                                    let marker = if active { "●" } else { "○" };
                                    if ui.button(format!("{marker}  {lab}")).clicked() {
                                        cmds.push(AppCommand::SetNodeInputLink {
                                            clip_id, node_id,
                                            port,
                                            label: if active { None } else { Some(lab.clone()) },
                                        });
                                        ui.close_menu();
                                        close = true;
                                    }
                                }
                                if cur.is_some() {
                                    if ui.button("Clear").clicked() {
                                        cmds.push(AppCommand::SetNodeInputLink {
                                            clip_id, node_id, port, label: None,
                                        });
                                        ui.close_menu();
                                        close = true;
                                    }
                                }
                                ui.separator();
                            }
                        });
                    } else if can_receive && current_receives.iter().any(|s| s.is_some()) {
                        if ui.button("Wireless: clear all receives").clicked() {
                            for (port, _) in current_receives.iter().enumerate()
                                .filter(|(_, s)| s.is_some())
                            {
                                cmds.push(AppCommand::SetNodeInputLink {
                                    clip_id, node_id, port: port as u8, label: None,
                                });
                            }
                            close = true;
                        }
                    }
                    let multi_label = if in_set && selected_set.len() > 1 {
                        format!("Delete {} selected", selected_set.len())
                    } else {
                        "Delete".to_string()
                    };
                    if !is_structural && ui.button(multi_label).clicked() {
                        if in_set {
                            // Right-click landed on one of the
                            // multi-selected nodes — delete the whole
                            // set in one gesture (DaVinci-equivalent).
                            // Source / Output are skipped same as the
                            // Delete-key path.
                            let to_delete: Vec<NodeId> = selected_set
                                .iter()
                                .copied()
                                .filter(|id| {
                                    graph.nodes.iter()
                                        .find(|n| n.id == *id)
                                        .map(|n| !matches!(n.kind, NodeKind::Source | NodeKind::Output))
                                        .unwrap_or(false)
                                })
                                .collect();
                            for id in &to_delete {
                                cmds.push(AppCommand::RemoveNode { clip_id, node_id: *id });
                                selected_set.remove(id);
                                if *selected_node == Some(*id) { *selected_node = None; }
                            }
                        } else {
                            // Right-clicked a node that's not in the
                            // current set — delete just that one and
                            // leave the (unrelated) selection alone.
                            cmds.push(AppCommand::RemoveNode { clip_id, node_id });
                            if *selected_node == Some(node_id) { *selected_node = None; }
                        }
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

fn input_port_pos(rect: &egui::Rect, port: u8, count: u8) -> egui::Pos2 {
    port_pos_on_edge(rect, port, count.max(1), false)
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

const WIRE_STEPS: usize = 24;

/// Sample the wire's cubic bezier as a polyline. Shared by drawing
/// (`draw_wire` / `draw_wire_dashed`) AND hit-testing
/// (`distance_to_polyline_sq`) so the cursor-proximity check
/// matches the pixels actually painted.
///
/// Cubic bezier with horizontal handles whose length scales with
/// the X distance — gives the classic "S" curve when nodes are
/// side-by-side and a clean "U" when wired backwards.
fn bezier_polyline(from: egui::Pos2, to: egui::Pos2) -> [egui::Pos2; WIRE_STEPS + 1] {
    let dx = (to.x - from.x).abs().max(40.0);
    let h = dx * 0.5;
    let c1 = egui::pos2(from.x + h, from.y);
    let c2 = egui::pos2(to.x - h, to.y);
    let mut pts = [from; WIRE_STEPS + 1];
    for i in 1..=WIRE_STEPS {
        let t = i as f32 / WIRE_STEPS as f32;
        let mt = 1.0 - t;
        pts[i] = egui::pos2(
            mt * mt * mt * from.x + 3.0 * mt * mt * t * c1.x + 3.0 * mt * t * t * c2.x + t * t * t * to.x,
            mt * mt * mt * from.y + 3.0 * mt * mt * t * c1.y + 3.0 * mt * t * t * c2.y + t * t * t * to.y,
        );
    }
    pts
}

fn draw_wire(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32) {
    let pts = bezier_polyline(from, to);
    let stroke = egui::Stroke::new(width.max(1.0), color);
    for i in 1..pts.len() {
        painter.line_segment([pts[i - 1], pts[i]], stroke);
    }
}

/// Same bezier as `draw_wire`, but only every other segment paints
/// — i.e. 12 dashes spread evenly along the curve. Used for the in-
/// flight drag wire so the user can tell at a glance "this isn't
/// committed yet". Dash density follows the curve length naturally
/// because the polyline has a fixed step count.
fn draw_wire_dashed(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32) {
    let pts = bezier_polyline(from, to);
    let stroke = egui::Stroke::new(width.max(1.0), color);
    let mut i = 1;
    while i < pts.len() {
        painter.line_segment([pts[i - 1], pts[i]], stroke);
        i += 2;
    }
}

fn point_to_segment_dist_sq(a: egui::Pos2, b: egui::Pos2, p: egui::Pos2) -> f32 {
    let ab = b - a;
    let ap = p - a;
    let len_sq = ab.length_sq();
    if len_sq < 1e-6 {
        return ap.length_sq();
    }
    let t = (ap.dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (p - proj).length_sq()
}

/// Min squared distance from `p` to the polyline. Used for edge
/// hover hit-testing — N segments per edge × M edges per frame; at
/// 24 segments and typical grade graphs (< 30 edges) this is
/// trivial CPU.
fn distance_to_polyline_sq(pts: &[egui::Pos2], p: egui::Pos2) -> f32 {
    let mut best = f32::MAX;
    for i in 1..pts.len() {
        let d = point_to_segment_dist_sq(pts[i - 1], pts[i], p);
        if d < best { best = d; }
    }
    best
}
