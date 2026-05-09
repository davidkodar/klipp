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
use klipp_nodes::{NodeFrameId, NodeGraph, NodeId, NodeKind, NodePos, NodeStickyNoteId};

const NODE_W: f32 = 120.0;
const NODE_H: f32 = 60.0;
const NODE_HEADER_H: f32 = 16.0;
const PORT_RADIUS: f32 = 5.0;
const PORT_HIT_RADIUS: f32 = 9.0;
const GRID_SPACING: f32 = 32.0;
/// Square side for `NodeKind::Reroute` — small enough to read as a
/// "dot on the wire" rather than a node card, big enough that the
/// input + output ports don't overlap each other.
const REROUTE_SIZE: f32 = 20.0;

/// Six muted swatches offered in the frame recolour menu. Format
/// matches `NodeFrame::color_rgba` — packed RGBA u32 (little-endian
/// byte order: r,g,b,a). Chosen for legibility on both light and
/// dark themes; alpha defaults to 255 (the paint pass scales down
/// the fill alpha to 30 anyway, but the chip + border honour the
/// stored value).
const FRAME_COLOR_PALETTE: &[(&str, u32)] = &[
    ("Slate",  u32::from_le_bytes([0x6a, 0x82, 0xa8, 0xff])),
    ("Teal",   u32::from_le_bytes([0x4a, 0xa8, 0x9a, 0xff])),
    ("Green",  u32::from_le_bytes([0x68, 0xb0, 0x5a, 0xff])),
    ("Amber",  u32::from_le_bytes([0xd0, 0xa8, 0x4a, 0xff])),
    ("Red",    u32::from_le_bytes([0xc8, 0x6a, 0x6a, 0xff])),
    ("Purple", u32::from_le_bytes([0x9a, 0x6a, 0xc8, 0xff])),
];

/// Default colour assigned to freshly-created frames — the first
/// palette entry (Slate). Users change via the frame's right-click
/// menu.
const DEFAULT_FRAME_COLOR_RGBA: u32 = u32::from_le_bytes([0x6a, 0x82, 0xa8, 0xff]);

/// Cyan tint used by the in-graph search match-glow — distinct
/// from amber selection / yellow solo / magenta matte so a node
/// that's matching can still be told apart from one that's
/// selected, soloed, or sitting on a matte chain.
const SEARCH_MATCH_COLOR: egui::Color32 = egui::Color32::from_rgb(0x3a, 0xd0, 0xff);


/// Fusion-style family palette — five buckets at low saturation
/// (~25 % chroma) so the brand gold stays the only loud thing in
/// the canvas. Each family is a different hue so node kinds read
/// as their role at a glance, but no single body fights with the
/// wires (which carry the role colour at full saturation).
///
/// - `PRIMARY_GRADE`  — colour-pipeline kinds (Corrector through
///   Curves, Hue/Sat, ColorBalance, ColorMatch, ColorWarper).
///   Cool slate-blue.
/// - `SECONDARY_MATTE` — selection / matte producers + consumers
///   (Qualifier, LumaKey, Mask, CombineMattes, ExternalMatte,
///   MaskCutout). Muted plum.
/// - `COMPOSITE` — multi-input compositors (Layer, Blend,
///   ApplyMatte, LayerMixer). Muted moss green.
/// - `LOOK` — finishing / look kinds that aren't a grade per-se
///   (3D LUT, Film Grain, Denoise, SoftClip). Muted ochre.
/// - `STRUCTURAL` — routing + container kinds that don't process
///   pixels (Reroute, Compound, RGBSplitter, RGBCombiner,
///   PrimariesMatrix, InputTransform). Neutral gray.
const FAMILY_PRIMARY_GRADE: egui::Color32 = egui::Color32::from_rgb(0x40, 0x4a, 0x55);
const FAMILY_SECONDARY_MATTE: egui::Color32 = egui::Color32::from_rgb(0x4f, 0x3d, 0x55);
const FAMILY_COMPOSITE: egui::Color32 = egui::Color32::from_rgb(0x3f, 0x50, 0x42);
const FAMILY_LOOK: egui::Color32 = egui::Color32::from_rgb(0x52, 0x4e, 0x3e);
const FAMILY_STRUCTURAL: egui::Color32 = egui::Color32::from_rgb(0x45, 0x45, 0x48);
/// Source = structural with a faint green tint (input flowing in);
/// Output = structural with a faint blue tint (sink). Both are
/// otherwise the structural-gray family — distinct enough to read
/// as separate terminals, neutral enough to sit alongside the
/// other structural kinds without claiming a unique family slot.
const FAMILY_SOURCE: egui::Color32 = egui::Color32::from_rgb(0x3e, 0x4d, 0x44);
const FAMILY_OUTPUT: egui::Color32 = egui::Color32::from_rgb(0x3e, 0x44, 0x4d);

fn node_color(kind: &NodeKind, theme: &KlippTheme) -> egui::Color32 {
    let _ = theme;
    match kind {
        NodeKind::Source => FAMILY_SOURCE,
        NodeKind::Output => FAMILY_OUTPUT,

        // Secondary / matte family.
        NodeKind::Qualifier
        | NodeKind::LumaKey
        | NodeKind::Mask
        | NodeKind::CombineMattes
        | NodeKind::ExternalMatte
        | NodeKind::MaskCutout => FAMILY_SECONDARY_MATTE,

        // Composite family.
        NodeKind::Layer
        | NodeKind::Blend
        | NodeKind::ApplyMatte
        | NodeKind::LayerMixer => FAMILY_COMPOSITE,

        // Look / finishing family.
        NodeKind::Lut3D
        | NodeKind::FilmGrain
        | NodeKind::Denoise
        | NodeKind::SoftClip => FAMILY_LOOK,

        // Structural family — routing + containers.
        NodeKind::Reroute
        | NodeKind::Compound
        | NodeKind::RGBSplitter
        | NodeKind::RGBCombiner
        | NodeKind::PrimariesMatrix
        | NodeKind::InputTransform => FAMILY_STRUCTURAL,

        // Everything else falls into the primary-grade family —
        // Corrector, Exposure, Contrast, Saturation, ToneControls,
        // ColorWheels, WheelSatBri, RGBCurves, HueSatCurves,
        // ColorBalance, ColorMatch, ColorWarper, plus future
        // colour-pipeline kinds.
        _ => FAMILY_PRIMARY_GRADE,
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

#[derive(Clone)]
struct DragNode {
    /// Every node id that should follow the cursor — a single id
    /// for the simple click-and-drag case, the entire multi-
    /// selection when the user grabs a member of a marqueed set.
    /// Live drag emits one MoveNode per id per tick (same shape
    /// as DragFrame).
    node_ids: Vec<NodeId>,
    /// Cumulative delta we've already emitted to the engine since
    /// the drag started, in graph coordinates. Used to compute the
    /// next per-frame delta without rounding drift. Shared across
    /// every node_id since they all move in lockstep.
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

/// Frame-drag state — when the user grabs a frame's empty body,
/// every member node moves in lockstep until release. Tracks the
/// same per-frame emitted-delta pattern `DragNode` uses so per-
/// frame MoveNode emissions don't drift from cursor motion.
#[derive(Clone)]
struct DragFrame {
    member_ids: Vec<NodeId>,
    emitted_dx: f32,
    emitted_dy: f32,
    start_cursor_x: f32,
    start_cursor_y: f32,
}

/// Sticky-note drag state. Same emitted-delta pattern as DragNode
/// + DragFrame so streaming MoveStickyNote per tick doesn't drift
/// from cursor motion across many frames.
#[derive(Clone, Copy)]
struct DragSticky {
    sticky_id: NodeStickyNoteId,
    emitted_dx: f32,
    emitted_dy: f32,
    start_cursor_x: f32,
    start_cursor_y: f32,
}

/// A saved view position. Bookmarks live on the panel, not on the
/// graph or project — they're a navigation aid for "where am I
/// looking", not a property of the grade. In-memory only; the full
/// canvas state (pan + zoom) is captured at set-time and re-applied
/// verbatim on jump.
#[derive(Clone, Copy)]
struct ViewBookmark {
    pan: egui::Vec2,
    zoom: f32,
}

pub struct NodeGraphV2Panel {
    pan: egui::Vec2,
    zoom: f32,
    drag_node: Option<DragNode>,
    drag_wire: Option<DragWire>,
    /// Active frame drag — set on primary-press in a frame's empty
    /// body, cleared on release. While set, the live-drag loop
    /// emits one MoveNode per member per tick so the whole group
    /// follows the cursor.
    drag_frame: Option<DragFrame>,
    /// Per-frame context menu — `(frame_id, screen_pos)`. Mirrors
    /// the per-node `node_menu` field so the right-click branch
    /// can pop the rename/recolour/delete chrome at the click point.
    frame_menu: Option<(NodeFrameId, egui::Pos2)>,
    /// Frame currently being renamed via the inline label editor.
    /// `(frame_id, screen_pos, buf)` — None when idle.
    frame_rename_editor: Option<(NodeFrameId, egui::Pos2, String)>,
    /// Frame currently editing its multi-line description in a
    /// floating popup. `(frame_id, screen_pos, buf)` — same shape
    /// as the rename editor; only one of the two can be open at a
    /// time (frame menu closes itself before opening either).
    frame_description_editor: Option<(NodeFrameId, egui::Pos2, String)>,
    /// Active sticky-note drag — set on primary-press inside a
    /// note's body. Tracks emitted-delta so streaming MoveStickyNote
    /// per tick doesn't drift from cursor motion.
    drag_sticky: Option<DragSticky>,
    /// Per-sticky right-click menu — `(sticky_id, screen_pos)`.
    sticky_menu: Option<(NodeStickyNoteId, egui::Pos2)>,
    /// Inline multi-line text editor for a sticky note —
    /// `(sticky_id, screen_pos, buf)`.
    sticky_text_editor: Option<(NodeStickyNoteId, egui::Pos2, String)>,
    /// Selected sticky note id. Independent of node selection so
    /// a sticky can be highlighted without dragging an inspector
    /// onto the canvas. Cleared when a node is clicked or the
    /// canvas background is clicked.
    selected_sticky: Option<NodeStickyNoteId>,
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
    /// View bookmarks 1..9 — Shift+N sets slot N to the current
    /// pan + zoom, plain N jumps to slot N if set. In-memory only;
    /// resets on app restart. Empty slots show a "set me first"
    /// toast on attempted jump rather than silently doing nothing.
    view_bookmarks: [Option<ViewBookmark>; 9],
    /// Transient toast painted in the canvas's top-right corner when
    /// a bookmark is set / jumped / found-empty. Fades over ~1.5 s.
    /// Stays panel-local (the global `state.toast` would require
    /// plumbing AppState into the panel for what is purely a node-
    /// graph navigation cue).
    bookmark_toast: Option<(String, std::time::Instant)>,
    /// Minimap visibility — toggleable via the `M` hotkey (gated on
    /// canvas focus + no popup) and the toolbar `Map` button. The
    /// minimap is a 140 × 100 px overview painted in the canvas's
    /// bottom-right corner: every node's bounding rect scaled to
    /// fit, plus a viewport-rect overlay showing what's currently
    /// on-screen. Click + drag to pan.
    minimap_visible: bool,
    /// In-graph search overlay — Ctrl+F toggles. While open, every
    /// node whose label or kind display-name contains the query
    /// (case-insensitive substring) glows cyan; Tab / Shift+Tab
    /// cycle through matches and pan the canvas to centre on each.
    /// Also matches frame labels + descriptions so a renamed Sky
    /// frame is one Ctrl+F-and-type away.
    search_open: bool,
    search_query: String,
    /// Which match index Tab is currently on. Wraps modulo the
    /// number of matches; reset to 0 whenever the query changes.
    search_active_idx: usize,
    /// Toggle for the per-thumb exposure-clipping overlay — red
    /// where any RGB channel is at the upper sentinel, blue where
    /// all channels are at the lower sentinel. Off by default;
    /// toolbar button flips it.
    show_thumb_clipping: bool,
    /// Toggle for the per-thumb mini-histogram strip painted at
    /// the bottom of each output thumbnail. Off by default;
    /// toolbar button flips it.
    show_thumb_histogram: bool,
    /// Cached clipping-overlay textures, one per node. Keyed by
    /// `engine_node_thumbs_gen` so the cache invalidates when the
    /// engine pushes a new thumb. Defer-dropped through the same
    /// `deferred_texture_drops` queue the output-thumb cache uses.
    thumb_clipping_overlays: std::collections::HashMap<NodeId, (u64, egui::TextureHandle)>,
    /// Cached 64-bucket luma histograms keyed by thumb generation.
    /// Cheap to recompute (~20 K samples per node) but caching
    /// avoids the work for nodes we paint every frame anyway.
    thumb_histograms: std::collections::HashMap<NodeId, (u64, [u32; 64])>,
}

impl Default for NodeGraphV2Panel {
    fn default() -> Self {
        Self {
            pan: egui::Vec2::ZERO,
            zoom: 1.0,
            drag_node: None,
            drag_wire: None,
            drag_frame: None,
            frame_menu: None,
            frame_rename_editor: None,
            frame_description_editor: None,
            drag_sticky: None,
            sticky_menu: None,
            sticky_text_editor: None,
            selected_sticky: None,
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
            view_bookmarks: [None; 9],
            bookmark_toast: None,
            minimap_visible: true,
            search_open: false,
            search_query: String::new(),
            search_active_idx: 0,
            show_thumb_clipping: false,
            show_thumb_histogram: false,
            thumb_clipping_overlays: std::collections::HashMap::new(),
            thumb_histograms: std::collections::HashMap::new(),
        }
    }
}

impl NodeGraphV2Panel {
    /// Capture the current pan + zoom into bookmark slot `slot`
    /// (0..8 → hotkeys 1..9). Out-of-range slots are silently
    /// ignored. Used by the command palette / toolbar entries to
    /// share the same code path as the keyboard handler.
    pub fn set_bookmark(&mut self, slot: usize) {
        if let Some(s) = self.view_bookmarks.get_mut(slot) {
            *s = Some(ViewBookmark { pan: self.pan, zoom: self.zoom });
            self.bookmark_toast = Some((
                format!("Bookmark {} set", slot + 1),
                std::time::Instant::now(),
            ));
        }
    }

    /// Restore pan + zoom from bookmark slot `slot`. Empty slots
    /// emit the same "set me first" toast the keyboard path does.
    pub fn jump_bookmark(&mut self, slot: usize) {
        let Some(s) = self.view_bookmarks.get(slot) else { return; };
        if let Some(b) = *s {
            self.pan = b.pan;
            self.zoom = b.zoom;
            self.bookmark_toast = Some((
                format!("Jumped to bookmark {}", slot + 1),
                std::time::Instant::now(),
            ));
        } else {
            self.bookmark_toast = Some((
                format!("Bookmark {} empty (Shift+{} to set)", slot + 1, slot + 1),
                std::time::Instant::now(),
            ));
        }
    }

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
        solo_node: Option<NodeId>,
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
        if toolbar_ui.button("Auto-layout").on_hover_text(
            "Recompute every node's position via hierarchical layered layout (shortcut: L)\n\
            Undoable with Ctrl+Z."
        ).clicked() {
            cmds.push(AppCommand::AutoLayoutNodeGraph { clip_id });
        }
        // Sticky-note creation. Drops a fresh note at the centre of
        // the current viewport in graph coords so the user always
        // sees it appear regardless of where they're panned. Drag-
        // body to reposition; double-click to edit text.
        if toolbar_ui.button(egui::RichText::new("\u{1F4DD} Note"))
            .on_hover_text("Add a sticky note at the canvas centre")
            .clicked()
        {
            // We don't have the to_graph closure built yet at this
            // point in the toolbar block; reconstruct the canvas-
            // centre → graph mapping inline. screen = origin +
            // (graph + pan) * zoom, so graph = (screen − origin)/zoom
            // − pan. The canvas-centre ↔ origin term collapses to
            // zero, leaving graph = -pan.
            cmds.push(AppCommand::CreateStickyNote {
                clip_id,
                x: -self.pan.x,
                y: -self.pan.y,
            });
        }
        // Per-thumbnail diagnostic toggles. Off by default so users
        // who don't grade in this canvas don't pay paint cost. The
        // toolbar pair makes them flickable mid-grade rather than
        // buried in prefs — same UX shape as the bookmarks ★ and
        // minimap ▦ toggles.
        if toolbar_ui.selectable_label(
            self.show_thumb_clipping,
            egui::RichText::new("\u{26A0}").small(),
        ).on_hover_text("Exposure clipping overlay on node thumbs\n\
            Red: any RGB channel ≥ 248\n\
            Blue: all channels ≤ 8")
            .clicked()
        {
            self.show_thumb_clipping = !self.show_thumb_clipping;
        }
        if toolbar_ui.selectable_label(
            self.show_thumb_histogram,
            egui::RichText::new("\u{1F4CA}").small(),
        ).on_hover_text("Mini luma histogram on node thumbs\n\
            64-bucket Rec.709 luma distribution at the bottom of each preview")
            .clicked()
        {
            self.show_thumb_histogram = !self.show_thumb_histogram;
        }
        // Find — proper button (not selectable_label) so the click
        // target reads as "tool" rather than "tag". `selected`-style
        // tinting communicates the open/closed state visually
        // alongside the canvas overlay.
        let find_button = if self.search_open {
            egui::Button::new(egui::RichText::new("\u{1F50D} Find").color(theme.accent))
                .fill(egui::Color32::from_rgba_unmultiplied(
                    theme.accent.r(), theme.accent.g(), theme.accent.b(), 30,
                ))
        } else {
            egui::Button::new(egui::RichText::new("\u{1F50D} Find"))
        };
        if toolbar_ui.add(find_button)
            .on_hover_text("Find nodes (Ctrl+F)\nTab cycles matches, Esc closes")
            .clicked()
        {
            self.search_open = !self.search_open;
            if !self.search_open {
                self.search_query.clear();
                self.search_active_idx = 0;
            }
        }
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
        // Bookmarks hint — purely a discovery surface for the
        // Shift+1..9 / 1..9 hotkeys. Static label, no menu, hover
        // for the keybinding reminder + a one-line summary of which
        // slots are currently in use.
        let used: Vec<u8> = self.view_bookmarks.iter().enumerate()
            .filter_map(|(i, b)| b.is_some().then(|| (i + 1) as u8))
            .collect();
        let hint = if used.is_empty() {
            "Bookmarks  ·  no slots set yet".to_string()
        } else {
            let used_str = used.iter().map(|n| n.to_string())
                .collect::<Vec<_>>().join(", ");
            format!("Bookmarks  ·  set: {used_str}")
        };
        toolbar_ui.label(egui::RichText::new("\u{2605}").small().color(theme.text_dim))
            .on_hover_text(format!(
                "View bookmarks\n\n\
                Shift + 1…9   save the current pan + zoom\n\
                1…9                 jump back\n\n\
                {hint}"
            ));
        toolbar_ui.add_space(8.0);
        // Minimap toggle — proper selectable button so the on/off
        // state is unambiguous (selected = highlighted in accent).
        // Same M shortcut shown on hover. The glyph + label combo
        // ("▦ Map") reads as a labelled chip rather than a tiny
        // unmarked icon, which earlier reviewers couldn't find.
        if toolbar_ui.selectable_label(
            self.minimap_visible,
            egui::RichText::new("\u{25A6} Map").small(),
        ).on_hover_text("Toggle minimap (M)\nClick + drag the minimap to pan").clicked() {
            self.minimap_visible = !self.minimap_visible;
        }
        toolbar_ui.add_space(8.0);
        // Solo indicator — only visible when a node is currently
        // soloed. Shows the soloed node's label (so the user knows
        // which node they're previewing without scanning the canvas
        // for the yellow halo) plus an inline × button to clear.
        // Background uses the same yellow as the on-canvas halo so
        // the link reads visually.
        if let Some(solo_id) = solo_node {
            let label = graph.nodes.iter()
                .find(|n| n.id == solo_id)
                .map(|n| n.label.clone())
                .unwrap_or_else(|| "?".to_string());
            let solo_color = egui::Color32::from_rgb(0xff, 0xd5, 0x3a);
            let bg = egui::Color32::from_rgba_unmultiplied(
                solo_color.r(), solo_color.g(), solo_color.b(), 40,
            );
            egui::Frame::none()
                .fill(bg)
                .stroke(egui::Stroke::new(0.5, solo_color))
                .rounding(3.0)
                .inner_margin(egui::Margin::symmetric(4.0, 1.0))
                .show(&mut toolbar_ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("\u{1F3AF} SOLO: {label}"))
                            .small()
                            .color(theme.text_primary),
                    );
                    if ui.small_button("\u{2715}")
                        .on_hover_text("Clear solo")
                        .clicked()
                    {
                        cmds.push(AppCommand::SetNodeSolo { node_id: None });
                    }
                });
            toolbar_ui.add_space(8.0);
        }
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
            || self.frame_menu.is_some()
            || self.frame_rename_editor.is_some()
            || self.frame_description_editor.is_some()
            || self.wireless_label_editor.is_some()
            || self.sticky_menu.is_some()
            || self.sticky_text_editor.is_some()
            || self.search_open;
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
            // Reroute is a wire-routing aid, not a grade — render
            // it as a 20×20 px dot rather than a full 120×60 card so
            // it doesn't compete visually with real nodes. The
            // smaller rect feeds back into port-position math via a
            // height threshold in `port_pos_on_edge` so input/output
            // ports land on the dot's edges, not in empty space
            // beside it.
            let size = if matches!(node.kind, NodeKind::Reroute) {
                egui::vec2(REROUTE_SIZE * zoom, REROUTE_SIZE * zoom)
            } else {
                egui::vec2(NODE_W * zoom, NODE_H * zoom)
            };
            let rect = egui::Rect::from_center_size(center, size);
            let in_count = node.dynamic_input_port_count();
            node_rects.push((node.id, rect, node_color(&node.kind, theme), node.kind, in_count));
        }

        // ── Search matches ────────────────────────────────────────
        // Pre-compute the set of match-node-ids so the per-node
        // paint loop and the Tab handler agree on what's matching
        // this frame. Empty query → empty set (no glow). Matches
        // against label + kind.display_name() so a renamed
        // "Sky Glow" node hits both as the user types either.
        let search_matches: Vec<NodeId> = if self.search_open && !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            graph.nodes.iter()
                .filter(|n| {
                    n.label.to_lowercase().contains(&q)
                        || n.kind.display_name().to_lowercase().contains(&q)
                })
                .map(|n| n.id)
                .collect()
        } else {
            Vec::new()
        };
        let search_match_set: std::collections::HashSet<NodeId> =
            search_matches.iter().copied().collect();

        // ── Frames ────────────────────────────────────────────────
        // Painted before wires so wires cross over the frame's
        // translucent fill (read as "wire passes through frame")
        // rather than disappearing under it. Each frame's screen
        // rect is the union of member node screen rects + padding;
        // empty frames (every member id is stale) silently skip.
        const FRAME_PAD: f32 = 18.0;
        let mut frame_rects: Vec<(NodeFrameId, egui::Rect, egui::Color32)> =
            Vec::with_capacity(graph.frames.len());
        for frame in &graph.frames {
            let mut min = egui::pos2(f32::INFINITY, f32::INFINITY);
            let mut max = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
            for (nid, rect, _, _, _) in &node_rects {
                if frame.contained_node_ids.contains(nid) {
                    min.x = min.x.min(rect.min.x);
                    min.y = min.y.min(rect.min.y);
                    max.x = max.x.max(rect.max.x);
                    max.y = max.y.max(rect.max.y);
                }
            }
            if !min.x.is_finite() { continue; }
            let pad = FRAME_PAD * zoom;
            let frame_rect = egui::Rect::from_min_max(
                egui::pos2(min.x - pad, min.y - pad),
                egui::pos2(max.x + pad, max.y + pad),
            );
            let bytes = frame.color_rgba.to_le_bytes();
            let color = egui::Color32::from_rgba_unmultiplied(
                bytes[0], bytes[1], bytes[2], bytes[3],
            );
            // Tint fill (alpha 30) + border (alpha 180).
            painter.rect_filled(
                frame_rect, 4.0,
                egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30),
            );
            painter.rect_stroke(
                frame_rect, 4.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(
                    color.r(), color.g(), color.b(), 180,
                )),
            );
            // Label chip on the top edge — coloured bg + dark text
            // for legible contrast on light tints.
            let label_text = if frame.label.is_empty() { "—".to_string() } else { frame.label.clone() };
            let chip_font = egui::FontId::proportional(11.0 * zoom.max(0.5));
            let galley = painter.layout_no_wrap(label_text, chip_font, theme.bg_deep);
            let chip_pad = egui::vec2(8.0 * zoom, 3.0 * zoom);
            let chip_size = galley.size() + chip_pad * 2.0;
            let chip_pos = egui::pos2(
                frame_rect.min.x + 6.0 * zoom,
                frame_rect.min.y - chip_size.y * 0.5,
            );
            let chip_rect = egui::Rect::from_min_size(chip_pos, chip_size);
            painter.rect_filled(
                chip_rect, 3.0,
                egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 220),
            );
            painter.galley(chip_rect.min + chip_pad, galley, theme.bg_deep);

            // Description text — multi-line annotation painted just
            // below the title chip, inset from the frame's left
            // edge. Skipped at extreme zoom-out (text would be
            // unreadable and just clutter the wash) and when empty
            // (the frame's interior stays clean for member nodes).
            // Wrapped to the frame's interior width so long lines
            // soft-break instead of overflowing past the frame's
            // right border.
            if !frame.description.is_empty() && zoom >= 0.6 {
                let descr_color = egui::Color32::from_rgba_unmultiplied(
                    theme.text_primary.r(),
                    theme.text_primary.g(),
                    theme.text_primary.b(),
                    150,
                );
                let descr_font = egui::FontId::proportional(10.5 * zoom);
                let max_w = (frame_rect.width() - 12.0 * zoom).max(40.0);
                let descr_pos = egui::pos2(
                    frame_rect.min.x + 6.0 * zoom,
                    frame_rect.min.y + chip_size.y * 0.5 + 6.0 * zoom,
                );
                let galley = painter.layout(
                    frame.description.clone(),
                    descr_font,
                    descr_color,
                    max_w,
                );
                painter.galley(descr_pos, galley, descr_color);
            }

            frame_rects.push((frame.id, frame_rect, color));
        }

        // ── Sticky-note screen rects ──────────────────────────────
        // Compute once here so the hit-test path (press handler
        // below) and the paint pass (after wires) agree on
        // geometry. Height = wrapped-text height + 12 px padding,
        // bounded to a sensible minimum so an empty sticky still
        // renders at a hittable size.
        let mut sticky_rects: Vec<(
            NodeStickyNoteId,
            egui::Rect,
            egui::Color32,
            std::sync::Arc<egui::epaint::text::Galley>,
            bool,
        )> = Vec::with_capacity(graph.sticky_notes.len());
        for note in &graph.sticky_notes {
            let bytes = note.color_rgba.to_le_bytes();
            let color = egui::Color32::from_rgba_unmultiplied(
                bytes[0], bytes[1], bytes[2], bytes[3],
            );
            let width_screen = note.width * zoom;
            let inner_pad_x = 8.0 * zoom;
            let inner_pad_y = 6.0 * zoom;
            let max_text_w = (width_screen - inner_pad_x * 2.0).max(40.0);
            let is_placeholder = note.text.is_empty();
            let display_text = if is_placeholder {
                "Click to edit…".to_string()
            } else {
                note.text.clone()
            };
            let font = egui::FontId::proportional(11.0 * zoom.max(0.5));
            let galley = painter.layout(
                display_text,
                font,
                theme.bg_deep,
                max_text_w,
            );
            let height_screen = (galley.size().y + inner_pad_y * 2.0).max(28.0 * zoom.max(0.5));
            let top_left = to_screen(egui::pos2(note.pos.x, note.pos.y));
            let rect = egui::Rect::from_min_size(
                top_left, egui::vec2(width_screen, height_screen),
            );
            sticky_rects.push((note.id, rect, color, galley, is_placeholder));
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
            // Wire colour follows the destination's expected input
            // role (matte ports magenta, etc.) UNLESS the destination
            // is generic — Reroute or any other kind whose port-role
            // resolves to Primary. In that case fall through to the
            // signal's actual role at the source, walking back through
            // any Reroute chain so a magenta matte stays magenta even
            // after several reroute hops.
            let dest_role = to_kind.input_port_role(edge.to_port);
            let role = if matches!(to_kind, NodeKind::Reroute) || dest_role == klipp_nodes::PortRole::Primary {
                klipp_nodes::effective_signal_role_at(graph, edge.from_node, edge.from_port)
            } else {
                dest_role
            };
            let (r, g, b) = role.editor_color();
            // Two-pass paint per wire: a wider, dimmer "signal
            // glow" underlay first, then the crisp role-coloured
            // stroke on top. The glow gives every committed wire a
            // subtle halo so it reads as a carrying signal rather
            // than a pen line, without any animation cost. Hover
            // boosts both the underlay alpha + width AND the main
            // stroke width so the focus state pops further.
            let is_hover = hovered_edge == Some(idx);
            let main_width = if is_hover { 3.0 } else { 1.5 } * zoom;
            let glow_width = if is_hover { 5.0 } else { 3.5 } * zoom;
            let glow_alpha = if is_hover { 90 } else { 40 };
            let main_alpha = if is_hover { 255 } else { 180 };
            let glow = egui::Color32::from_rgba_unmultiplied(r, g, b, glow_alpha);
            draw_wire(&painter, from, to, glow, glow_width);
            let main = egui::Color32::from_rgba_unmultiplied(r, g, b, main_alpha);
            draw_wire(&painter, from, to, main, main_width);
            // Animated traversal pulse on the hovered wire only —
            // small bright bead travels source → destination at
            // ~1.5 s/cycle. Direction matches data flow so it
            // doubles as a visual cue for "this is feeding that".
            // Continuous repaint is gated to is_hover so idle
            // canvases stay quiet (only one wire animates at a
            // time; the existing 100 % CPU bug from over-eager
            // hover gates is the constraint we're respecting).
            if is_hover {
                let time = ui.input(|i| i.time) as f32;
                let cycle = 1.5_f32;
                let phase = (time / cycle).fract();
                let p = bezier_at_phase(from, to, phase);
                let bead_r = (3.5 * zoom).max(2.0);
                painter.circle_filled(
                    p, bead_r,
                    egui::Color32::from_rgb(r, g, b),
                );
                painter.circle_stroke(
                    p, bead_r,
                    egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(r, g, b, 80),
                    ),
                );
                ui.ctx().request_repaint();
            }
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
                // Walking back through any Reroute chain at the source
                // so dragging from a Reroute output keeps the upstream
                // signal's colour rather than defaulting to amber.
                let role = if matches!(from_kind, NodeKind::Reroute) {
                    klipp_nodes::effective_signal_role_at(graph, dw.from_node, dw.from_port)
                } else {
                    from_kind.output_port_role(dw.from_port)
                };
                let (r, g, b) = role.editor_color();
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

        // ── Sticky notes ──────────────────────────────────────────
        // Painted above wires so the annotation stays visible, but
        // before nodes so functional cards always pop on top. Light
        // 2-stop drop shadow matches the node card depth so they
        // feel like sibling artefacts rather than floating overlays.
        for (sticky_id, rect, color, galley, is_placeholder) in sticky_rects.iter() {
            // Drop shadow (single stop — stickies are smaller and
            // don't need the 3-stop ambience the cards use).
            painter.rect_filled(
                rect.translate(egui::vec2(0.0, 2.0)).expand(1.0),
                4.5,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 30),
            );
            // Body — full opaque sticky tint so text contrast is
            // honest. Rounded corners + 1 px border for definition.
            painter.rect_filled(*rect, 4.0, *color);
            let border = if self.selected_sticky == Some(*sticky_id) {
                theme.accent
            } else {
                egui::Color32::from_rgba_unmultiplied(
                    color.r().saturating_sub(40),
                    color.g().saturating_sub(40),
                    color.b().saturating_sub(40),
                    255,
                )
            };
            let border_w = if self.selected_sticky == Some(*sticky_id) { 1.6 } else { 0.8 };
            painter.rect_stroke(*rect, 4.0, egui::Stroke::new(border_w, border));
            // Text — placeholder gets dimmed so it reads as a hint
            // rather than committed content.
            let inner_pad = egui::vec2(8.0 * zoom, 6.0 * zoom);
            let text_pos = rect.min + inner_pad;
            if *is_placeholder {
                let dim = egui::Color32::from_rgba_unmultiplied(
                    theme.bg_deep.r(),
                    theme.bg_deep.g(),
                    theme.bg_deep.b(),
                    140,
                );
                painter.galley(text_pos, galley.clone(), dim);
            } else {
                painter.galley(text_pos, galley.clone(), theme.bg_deep);
            }
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

            // Hover glow — animated soft accent ring around the
            // node when the cursor is on it AND no other state
            // (selection, drag, popup) is taking visual priority.
            // egui's animate_value_with_time keeps the fade smooth
            // and auto-requests repaint while transitioning;
            // settled animations short-circuit to a HashMap lookup
            // with no side effects so calling it for every node
            // every frame is cheap.
            let hover_eligible = !is_selected
                && self.drag_node.is_none()
                && self.drag_frame.is_none()
                && self.drag_wire.is_none()
                && self.box_select.is_none()
                && !any_menu_open;
            let is_hovered = hover_eligible
                && hover_pointer.map(|p| rect.contains(p)).unwrap_or(false);
            let glow_id = ui.id().with(("ngv2_node_hover_glow", node_id));
            let glow_target = if is_hovered { 1.0_f32 } else { 0.0_f32 };
            let glow_duration = if is_hovered { 0.12 } else { 0.20 };
            let glow = ui.ctx().animate_value_with_time(glow_id, glow_target, glow_duration);
            let glow_alpha = (32.0 * glow) as u8;

            // Search match — distinct cyan ring, animated through
            // its own channel so it can stack with hover glow + the
            // amber selection halo without colour confusion. Brighter
            // when this is the Tab-active match so the user can tell
            // at a glance "this is the one you'd jump to next".
            let is_match = search_match_set.contains(&node_id);
            let is_active_match = is_match
                && !search_matches.is_empty()
                && search_matches.get(self.search_active_idx % search_matches.len().max(1))
                    .copied() == Some(node_id);
            let match_id = ui.id().with(("ngv2_node_search_match", node_id));
            let match_target = if is_match { 1.0_f32 } else { 0.0_f32 };
            let match_glow = ui.ctx().animate_value_with_time(match_id, match_target, 0.15);
            let match_alpha = ((if is_active_match { 200.0 } else { 110.0 }) * match_glow) as u8;

            // Reroute is rendered as a dot, not a card — fully
            // self-contained branch so we don't pay for shadow /
            // highlight / header / body / chip / thumbnail logic
            // that doesn't apply. Selection halo is a single
            // amber ring, port hit-test continues to use the rect
            // edges which `port_pos_on_edge` translates correctly
            // for the small rect.
            if matches!(kind, NodeKind::Reroute) {
                let centre = rect.center();
                let radius = rect.width() * 0.40;
                if glow_alpha > 0 {
                    let accent = theme.accent;
                    painter.circle_stroke(
                        centre,
                        radius + 3.0,
                        egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(
                            accent.r(), accent.g(), accent.b(), glow_alpha,
                        )),
                    );
                }
                if match_alpha > 0 {
                    let cyan = SEARCH_MATCH_COLOR;
                    painter.circle_stroke(
                        centre,
                        radius + 5.0,
                        egui::Stroke::new(1.8, egui::Color32::from_rgba_unmultiplied(
                            cyan.r(), cyan.g(), cyan.b(), match_alpha,
                        )),
                    );
                }
                if is_selected {
                    let halo = theme.accent;
                    painter.circle_stroke(
                        centre,
                        radius + 4.0,
                        egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(
                            halo.r(), halo.g(), halo.b(), 70,
                        )),
                    );
                }
                painter.circle_filled(centre, radius, theme.bg_panel);
                let border = if is_selected { theme.accent } else { theme.border };
                let border_w = if is_selected { 1.6_f32 } else { 0.9_f32 };
                painter.circle_stroke(centre, radius, egui::Stroke::new(border_w, border));
                // Tiny inner dot in the kind colour so the routing
                // node still has a visual identity even at extreme
                // zoom-out where the outline disappears.
                painter.circle_filled(centre, radius * 0.4, fill);
                continue;
            }

            // Body
            let header_h = NODE_HEADER_H * zoom;
            let body_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y + header_h),
                rect.max,
            );
            // Drop shadow — one native-blurred rect (Gaussian
            // falloff) for the ambient halo + one tighter stop for
            // contact darkness. Replaces the older 3-stop alpha-
            // stack which gave stair-step feathering. egui's
            // `epaint::RectShape::blur_width` runs the blur on the
            // GPU as a single quad, so this is also less paint work
            // than 3 stacked rects. Offsets are in screen pixels
            // (not multiplied by zoom) — that matches how a real
            // drop shadow scales and keeps the depth cue legible at
            // extreme zoom-out. Painted before the Compound stack
            // ghosts so a single shadow sits under the whole stack
            // rather than each ghost casting its own competing one.
            {
                painter.add(egui::Shape::Rect(egui::epaint::RectShape {
                    rect: rect.translate(egui::vec2(0.0, 5.0)).expand(2.0),
                    rounding: egui::Rounding::same(6.0),
                    fill: egui::Color32::from_rgba_premultiplied(0, 0, 0, 50),
                    stroke: egui::Stroke::NONE,
                    blur_width: 8.0,
                    fill_texture_id: egui::TextureId::default(),
                    uv: egui::Rect::ZERO,
                }));
                // Tighter contact stop for the dark edge directly
                // under the card — keeps the shadow from feeling
                // floaty.
                painter.rect_filled(
                    rect.translate(egui::vec2(0.0, 1.5)).expand(0.5),
                    4.5, egui::Color32::from_rgba_premultiplied(0, 0, 0, 40),
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
            // Hover glow — soft accent ring around the rect, alpha
            // tracking the smoothed `glow` value. Painted before
            // the card body so it sits behind the rect's fill and
            // doesn't tint the body itself. Selection halos take
            // priority later in the paint order; the gate above
            // already disables the glow for selected nodes.
            if glow_alpha > 0 {
                let accent = theme.accent;
                painter.rect_stroke(
                    rect.expand(2.5),
                    5.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(
                        accent.r(), accent.g(), accent.b(), glow_alpha,
                    )),
                );
            }
            // Search match — cyan ring, distinct from amber hover/
            // selection so a node that's BOTH selected AND matching
            // shows two independent rings.
            if match_alpha > 0 {
                let cyan = SEARCH_MATCH_COLOR;
                painter.rect_stroke(
                    rect.expand(4.0),
                    6.0,
                    egui::Stroke::new(1.8, egui::Color32::from_rgba_unmultiplied(
                        cyan.r(), cyan.g(), cyan.b(), match_alpha,
                    )),
                );
            }
            painter.rect_filled(rect, 4.0, theme.bg_panel);
            // Header sits ~6 % brighter than the body so the card
            // reads as "headed strip + body" rather than a uniform
            // slab. Polarity-aware: theme.overlay flips dark on
            // light themes so this stays a brightening cue both
            // ways.
            let header_rect = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.min.y + header_h),
            );
            painter.rect_filled(header_rect, 4.0, theme.overlay(14));
            // Active nodes get a small lift on the kind-palette fill so
            // they read as live; bypassed nodes fall back to bg_panel_alt
            // (no fill colour at all). The lift is intentionally
            // conservative — too much washes out the kind-hue and
            // makes Compound/Layer/Qualifier rows harder to scan at a
            // glance.
            let body_fill = if node_on { lift(fill, 14) } else { theme.bg_panel_alt };
            painter.rect_filled(body_rect, 4.0, body_fill);
            // Header/body hairline divider — 1 px shadow line at
            // the bottom of the header strip. Polarity-aware: a
            // dark hairline on dark themes (theme.overlay flips to
            // dark in that case via its inverse path), light hair-
            // line on light themes. Reinforces that the header and
            // body are two separate material planes with negligible
            // paint cost.
            {
                let divider_y = rect.min.y + header_h - 0.5;
                let dark_overlay = if theme.bg_panel.r() as u32
                    + theme.bg_panel.g() as u32
                    + theme.bg_panel.b() as u32 > 384
                {
                    // Light theme — overlay() returns dark; that's
                    // already what we want for the divider.
                    theme.overlay(40)
                } else {
                    // Dark theme — overlay() returns light, but we
                    // want the divider DARKER than the body. Mix
                    // black in directly.
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90)
                };
                painter.line_segment(
                    [
                        egui::pos2(rect.min.x + 1.0, divider_y),
                        egui::pos2(rect.max.x - 1.0, divider_y),
                    ],
                    egui::Stroke::new(1.0, dark_overlay),
                );
            }
            // Body inner-bottom shadow — 4 stacked horizontal lines
            // at the bottom of the body fading from invisible to
            // dim over 4 px. Mirror of the top-edge highlight; the
            // pair gives the body volume — light catches the top
            // edge and the bottom curves into shadow. Alpha capped
            // at 35 so it stays a hint, not a halftone.
            {
                let max_alpha: u8 = 35;
                for i in 0..4 {
                    let y = body_rect.max.y - i as f32 - 0.5;
                    if y <= body_rect.min.y { break; }
                    let a = (max_alpha as f32 * (1.0 - i as f32 / 4.0)) as u8;
                    if a == 0 { break; }
                    let shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, a);
                    painter.line_segment(
                        [
                            egui::pos2(body_rect.min.x + 4.0, y),
                            egui::pos2(body_rect.max.x - 4.0, y),
                        ],
                        egui::Stroke::new(1.0, shadow),
                    );
                }
            }

            // Internal top-edge gradient — five stacked horizontal
            // lines fading from a bright top toward invisible over
            // ~5 px so the rect's top edge picks up "light from
            // above" as a soft glow rather than a hard pinstripe.
            // egui has no native vertical-gradient primitive at
            // this stroke level so we fake it with stacked alpha.
            // Polarity-aware via theme.overlay; selected state
            // brightens the top stop so the focus halo + gradient
            // reinforce each other.
            {
                let max_alpha: u8 = if is_selected { 110 } else { 70 };
                for i in 0..5 {
                    let y = rect.min.y + i as f32 + 0.5;
                    // Linear falloff: alpha = max * (1 - i/5).
                    let a = (max_alpha as f32 * (1.0 - i as f32 / 5.0)) as u8;
                    if a == 0 { break; }
                    let highlight = theme.overlay(a);
                    painter.line_segment(
                        [
                            egui::pos2(rect.min.x + 4.0, y),
                            egui::pos2(rect.max.x - 4.0, y),
                        ],
                        egui::Stroke::new(1.0, highlight),
                    );
                }
            }

            let is_soloed = solo_node == Some(node_id);

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
            // Solo halo — yellow rings around the soloed node so the
            // user sees at a glance "this is what I'm previewing".
            // Distinct hue from the amber selection halo so a node
            // that's BOTH selected AND soloed shows two stacked rings
            // (amber + yellow) rather than one ambiguously-coloured
            // glow.
            if is_soloed {
                let solo_color = egui::Color32::from_rgb(0xff, 0xd5, 0x3a);
                painter.rect_stroke(
                    rect.expand(2.0),
                    5.0,
                    egui::Stroke::new(1.8, egui::Color32::from_rgba_unmultiplied(
                        solo_color.r(), solo_color.g(), solo_color.b(), 200,
                    )),
                );
                painter.rect_stroke(
                    rect.expand(4.0),
                    6.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(
                        solo_color.r(), solo_color.g(), solo_color.b(), 70,
                    )),
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

            // Header state indicators — tiny glyphs at the right
            // edge of the header strip so the user can read a
            // node's state at a glance without expanding the
            // inspector. Three slots:
            //   - keyframe dot (cyan): node has at least one
            //     animation track with keyframes
            //   - bypass slash (red): node is disabled OR is being
            //     keyframe-bypassed at the current clip time
            //   - matte key (magenta): node's output is a matte
            //     signal (Qualifier / LumaKey / Mask / CombineMattes
            //     / ExternalMatte) — useful as a "this drives masks
            //     downstream" cue
            // Skipped at extreme zoom-out where they'd render as
            // unreadable specks. Painted in reverse so the right-
            // most indicator (keyframe) sits closest to the edge.
            if zoom >= 0.55 {
                let cy = rect.min.y + header_h * 0.5;
                let dot_r = (3.0 * zoom).max(2.0);
                let pitch = 9.0 * zoom;
                let mut x = rect.max.x - 6.0 * zoom;

                // Slot 1 — keyframe dot.
                if !node.keyframes.is_empty() {
                    let cyan = egui::Color32::from_rgb(0x6a, 0xc8, 0xe0);
                    painter.circle_filled(egui::pos2(x, cy), dot_r, cyan);
                    x -= pitch;
                }
                // Slot 2 — bypass slash. effective_enabled already
                // accounts for the ENABLED_TRACK keyframe state, so
                // a node that's been keyframe-bypassed at the
                // current time also lights up.
                if !node_on {
                    let red = egui::Color32::from_rgb(0xd0, 0x6a, 0x6a);
                    painter.text(
                        egui::pos2(x, cy),
                        egui::Align2::CENTER_CENTER,
                        "\u{00F8}", // ø
                        egui::FontId::proportional(10.0 * zoom),
                        red,
                    );
                    x -= pitch;
                }
                // Slot 3 — matte producer.
                if matches!(
                    kind.output_port_role(0),
                    klipp_nodes::PortRole::Matte,
                ) {
                    let magenta = egui::Color32::from_rgb(0xc8, 0x4a, 0xa8);
                    painter.text(
                        egui::pos2(x, cy),
                        egui::Align2::CENTER_CENTER,
                        "\u{25C7}", // ◇ — diamond (matte symbol family)
                        egui::FontId::proportional(10.0 * zoom),
                        magenta,
                    );
                }
            }

            // Solo "S" badge — small yellow chip in the top-left
            // corner so the solo state still reads even when the
            // halo is occluded by overlapping nodes. Sits above
            // the wireless-link chip's top-right slot so the two
            // never collide.
            if is_soloed {
                let chip_size = egui::vec2(14.0 * zoom, 14.0 * zoom);
                let chip_pos = egui::pos2(
                    rect.min.x - chip_size.x * 0.35,
                    rect.min.y - chip_size.y * 0.35,
                );
                let chip_rect = egui::Rect::from_min_size(chip_pos, chip_size);
                let solo_color = egui::Color32::from_rgb(0xff, 0xd5, 0x3a);
                painter.rect_filled(chip_rect, 3.0, solo_color);
                painter.rect_stroke(chip_rect, 3.0, egui::Stroke::new(0.5, theme.bg_deep));
                painter.text(
                    chip_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "S",
                    egui::FontId::proportional(10.0 * zoom),
                    theme.bg_deep,
                );
            }

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
                        // reads as "this is a matte node". Aspect-fit
                        // (16:9) so the matte source isn't squished
                        // into the wider-than-tall body cell, and
                        // round the corners 3 px to match the card
                        // chrome. Category glyph in the bottom-right
                        // tells the user "this thumb is a matte" at
                        // a glance.
                        let inset = 4.0 * zoom;
                        let cell = body_rect.shrink(inset);
                        let thumb_rect = aspect_fit_rect(cell, 16.0 / 9.0);
                        paint_thumb_rounded(&painter, thumb_rect, tex.id(), 3.0);
                        paint_thumb_badge(
                            &painter, thumb_rect,
                            kind_category_glyph(&kind), theme, zoom,
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
                // Output is a no-op in the executor (no entry in
                // node_outputs), so engine_node_thumbs.get(output_id)
                // is always None. Look up the feeder node instead so
                // Output paints the same thumbnail as the node
                // currently feeding it — i.e. the final composite.
                let thumb_lookup_id = if matches!(kind, NodeKind::Output) {
                    graph.edges.iter()
                        .find(|e| e.to_node == node_id && e.to_port == 0)
                        .map(|e| e.from_node)
                        .unwrap_or(node_id)
                } else {
                    node_id
                };
                if let Some(thumb_bytes) = engine_node_thumbs.get(&thumb_lookup_id) {
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
                            // Aspect-fit the 16:9 thumb into the wider-
                            // than-tall body cell with rounded corners
                            // and a category glyph in the bottom-right.
                            // See the matte-thumb path above for the
                            // shared helpers.
                            let inset = 4.0 * zoom;
                            let cell = body_rect.shrink(inset);
                            let thumb_rect = aspect_fit_rect(cell, 16.0 / 9.0);
                            paint_thumb_rounded(&painter, thumb_rect, tex.id(), 3.0);

                            // Exposure-clipping overlay — lazy build
                            // a 192×108 RGBA texture marking
                            // clipped/crushed pixels and paint it on
                            // top of the thumb. Cached by gen so we
                            // only rebuild when the engine pushes a
                            // new thumb. Defer-drop the old texture
                            // through the same queue the output-thumb
                            // cache uses.
                            if self.show_thumb_clipping {
                                let cached_gen = self.thumb_clipping_overlays
                                    .get(&node_id).map(|(g, _)| *g);
                                if cached_gen != Some(engine_node_thumbs_gen) {
                                    let img = build_clipping_overlay_image(thumb_bytes);
                                    let name = format!("node_clip_overlay_{}", node_id.as_simple());
                                    let overlay_tex = ui.ctx().load_texture(
                                        name, img, egui::TextureOptions::NEAREST,
                                    );
                                    if let Some((_, old)) = self.thumb_clipping_overlays.remove(&node_id) {
                                        deferred_drops.push(old);
                                    }
                                    self.thumb_clipping_overlays.insert(
                                        node_id, (engine_node_thumbs_gen, overlay_tex),
                                    );
                                }
                                if let Some((_, ov_tex)) = self.thumb_clipping_overlays.get(&node_id) {
                                    paint_thumb_rounded(&painter, thumb_rect, ov_tex.id(), 3.0);
                                }
                            }

                            // Mini histogram strip — 64-bucket luma
                            // histogram painted on the bottom 18 %
                            // of the thumb. Cached the same way.
                            if self.show_thumb_histogram {
                                let cached_gen = self.thumb_histograms
                                    .get(&node_id).map(|(g, _)| *g);
                                if cached_gen != Some(engine_node_thumbs_gen) {
                                    let bins = compute_thumb_histogram(thumb_bytes);
                                    self.thumb_histograms.insert(
                                        node_id, (engine_node_thumbs_gen, bins),
                                    );
                                }
                                if let Some((_, bins)) = self.thumb_histograms.get(&node_id) {
                                    paint_thumb_histogram(&painter, thumb_rect, bins, theme);
                                }
                            }

                            paint_thumb_badge(
                                &painter, thumb_rect,
                                kind_category_glyph(&kind), theme, zoom,
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
            // a LayerMixer with 5 wires shows 5 ports. For Reroute,
            // the static input role is Primary; reach back through
            // the upstream chain so the port matches the wire colour
            // we just painted into it.
            for p in 0..in_count {
                let pos = input_port_pos(&rect, p, in_count);
                let role = if matches!(kind, NodeKind::Reroute) {
                    // Find the edge feeding this port and take the
                    // upstream's effective signal role; fall back to
                    // Primary when disconnected.
                    graph.edges.iter()
                        .find(|e| e.to_node == node_id && e.to_port == p)
                        .map(|e| klipp_nodes::effective_signal_role_at(graph, e.from_node, e.from_port))
                        .unwrap_or(klipp_nodes::PortRole::Primary)
                } else {
                    kind.input_port_role(p)
                };
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
            // Reroute's output reflects whatever's flowing through it,
            // so use the chain-walking helper to pick a role consistent
            // with the wire we'll paint downstream.
            for p in 0..kind.output_port_count() {
                let pos = output_port_pos(&rect, p, &kind);
                let role = if matches!(kind, NodeKind::Reroute) {
                    klipp_nodes::effective_signal_role_at(graph, node_id, p)
                } else {
                    kind.output_port_role(p)
                };
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
        let menu_open = self.node_menu.is_some() || self.add_menu_open || self.frame_menu.is_some() || self.sticky_menu.is_some();
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
                                } else if selected_set.contains(node_id)
                                    && selected_set.len() >= 2
                                {
                                    // Plain click on a node that's
                                    // already part of a multi-selection
                                    // — preserve the selection so the
                                    // following drag moves every member
                                    // in lockstep. Without this, every
                                    // attempt to drag a marqueed group
                                    // collapsed the selection back to
                                    // the one node under the cursor.
                                    *selected_node = Some(*node_id);
                                } else {
                                    // Plain click on a non-selected node
                                    // (or a single-selected one) replaces
                                    // the selection with just this node.
                                    selected_set.clear();
                                    selected_set.insert(*node_id);
                                    *selected_node = Some(*node_id);
                                }
                                let g = to_graph(p);
                                // Drag set: every selected node when
                                // there's a multi-selection that
                                // includes this one, else just the
                                // single clicked node. Stable id order
                                // doesn't matter for the per-tick
                                // MoveNode emission since each id
                                // gets the same delta.
                                let drag_ids: Vec<NodeId> = if selected_set.contains(node_id)
                                    && selected_set.len() >= 2
                                {
                                    selected_set.iter().copied().collect()
                                } else {
                                    vec![*node_id]
                                };
                                self.drag_node = Some(DragNode {
                                    node_ids: drag_ids,
                                    emitted_dx: 0.0,
                                    emitted_dy: 0.0,
                                    start_cursor_x: g.x,
                                    start_cursor_y: g.y,
                                });
                                node_hit = true;
                                break;
                            }
                        }
                        // Frame body hit (between members)? Click
                        // selects all frame members + starts a
                        // frame drag (every member moves in lockstep
                        // with the cursor). Walks frame_rects in
                        // reverse so frames painted later (visually
                        // on top) win on overlap. Skipped on shift
                        // to keep shift-build-up selections intact.
                        // Sticky-note hit — between node and frame
                        // priority. Reverse so stickies painted later
                        // (visually on top) win on overlap. Single-
                        // click selects + starts drag; double-click
                        // opens the inline text editor.
                        let mut sticky_hit = false;
                        if !node_hit {
                            for (sticky_id, sr_rect, _, _, _) in sticky_rects.iter().rev() {
                                if !sr_rect.contains(p) { continue; }
                                if primary_double_clicked {
                                    let initial = graph.sticky_notes.iter()
                                        .find(|s| s.id == *sticky_id)
                                        .map(|s| s.text.clone())
                                        .unwrap_or_default();
                                    self.sticky_text_editor = Some((*sticky_id, p, initial));
                                } else {
                                    self.selected_sticky = Some(*sticky_id);
                                    selected_set.clear();
                                    *selected_node = None;
                                    let g = to_graph(p);
                                    self.drag_sticky = Some(DragSticky {
                                        sticky_id: *sticky_id,
                                        emitted_dx: 0.0,
                                        emitted_dy: 0.0,
                                        start_cursor_x: g.x,
                                        start_cursor_y: g.y,
                                    });
                                }
                                sticky_hit = true;
                                break;
                            }
                        }
                        if node_hit { self.selected_sticky = None; }
                        let mut frame_hit = false;
                        if !node_hit && !sticky_hit && !shift {
                            for (frame_id, fr_rect, _) in frame_rects.iter().rev() {
                                if !fr_rect.contains(p) { continue; }
                                let frame = match graph.frames.iter().find(|f| f.id == *frame_id) {
                                    Some(f) => f,
                                    None => continue,
                                };
                                let members: Vec<NodeId> =
                                    frame.contained_node_ids.iter().copied().collect();
                                selected_set.clear();
                                for id in &members { selected_set.insert(*id); }
                                *selected_node = members.first().copied();
                                let g = to_graph(p);
                                self.drag_frame = Some(DragFrame {
                                    member_ids: members,
                                    emitted_dx: 0.0,
                                    emitted_dy: 0.0,
                                    start_cursor_x: g.x,
                                    start_cursor_y: g.y,
                                });
                                frame_hit = true;
                                break;
                            }
                        }
                        // Click on empty canvas (not on a node body
                        // or a frame body) clears the selection —
                        // matches every canvas-style editor
                        // (Photoshop, Figma, etc.). Excludes shift-
                        // click so the user can build up a selection
                        // without losing it on a near-miss between
                        // nodes. The press also seeds a box-select
                        // marquee; if the user releases without
                        // moving it stays a click, otherwise the
                        // released-frame branch adds intersected
                        // nodes to the set.
                        if !node_hit && !frame_hit && !sticky_hit {
                            if !shift {
                                selected_set.clear();
                                *selected_node = None;
                                self.selected_sticky = None;
                            }
                            self.box_select = Some((p, p));
                        }
                    }
                }
            }
        }

        // Live drag — emit a MoveNode per id in the drag set per
        // tick. Same emitted-delta accounting whether the set has
        // one node or N: the cursor-derived delta is shared across
        // every member so they all translate in lockstep without
        // drift across many frames.
        if let Some(ref mut dn) = self.drag_node {
            if let Some(p) = pointer_pos {
                let g = to_graph(p);
                let total_dx = g.x - dn.start_cursor_x;
                let total_dy = g.y - dn.start_cursor_y;
                let delta_x = total_dx - dn.emitted_dx;
                let delta_y = total_dy - dn.emitted_dy;
                if delta_x.abs() > 0.01 || delta_y.abs() > 0.01 {
                    for nid in &dn.node_ids {
                        cmds.push(AppCommand::MoveNode {
                            clip_id,
                            node_id: *nid,
                            delta_x,
                            delta_y,
                        });
                    }
                    dn.emitted_dx = total_dx;
                    dn.emitted_dy = total_dy;
                }
            }
        }
        // Live frame drag — emit one MoveNode per member each tick.
        // Same emitted-delta accounting as DragNode so the per-
        // frame cursor delta doesn't drift across many frames.
        if let Some(ref mut df) = self.drag_frame {
            if let Some(p) = pointer_pos {
                let g = to_graph(p);
                let total_dx = g.x - df.start_cursor_x;
                let total_dy = g.y - df.start_cursor_y;
                let delta_x = total_dx - df.emitted_dx;
                let delta_y = total_dy - df.emitted_dy;
                if delta_x.abs() > 0.01 || delta_y.abs() > 0.01 {
                    for nid in &df.member_ids {
                        cmds.push(AppCommand::MoveNode {
                            clip_id,
                            node_id: *nid,
                            delta_x,
                            delta_y,
                        });
                    }
                    df.emitted_dx = total_dx;
                    df.emitted_dy = total_dy;
                }
            }
        }
        // Live sticky-note drag.
        if let Some(ref mut ds) = self.drag_sticky {
            if let Some(p) = pointer_pos {
                let g = to_graph(p);
                let total_dx = g.x - ds.start_cursor_x;
                let total_dy = g.y - ds.start_cursor_y;
                let delta_x = total_dx - ds.emitted_dx;
                let delta_y = total_dy - ds.emitted_dy;
                if delta_x.abs() > 0.01 || delta_y.abs() > 0.01 {
                    cmds.push(AppCommand::MoveStickyNote {
                        clip_id,
                        sticky_id: ds.sticky_id,
                        delta_x,
                        delta_y,
                    });
                    ds.emitted_dx = total_dx;
                    ds.emitted_dy = total_dy;
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
            self.drag_frame = None;
            self.drag_sticky = None;

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

        // View bookmarks (1..9 jump, Shift+1..9 set). Same gate as
        // Delete: no popup menu open, no text-input focus. Ctrl /
        // Alt / Cmd modifiers skip the binding so this won't collide
        // with any global Ctrl+N or Alt+N shortcut. Empty slots emit
        // a "set me first" toast instead of silently doing nothing —
        // confirms the binding registered even when no jump
        // happened.
        if !any_menu_open && !ui.ctx().wants_keyboard_input() {
            const NUM_KEYS: [egui::Key; 9] = [
                egui::Key::Num1, egui::Key::Num2, egui::Key::Num3,
                egui::Key::Num4, egui::Key::Num5, egui::Key::Num6,
                egui::Key::Num7, egui::Key::Num8, egui::Key::Num9,
            ];
            for (i, key) in NUM_KEYS.iter().enumerate() {
                if !ui.input(|inp| inp.key_pressed(*key)) { continue; }
                let mods = ui.input(|inp| inp.modifiers);
                if mods.ctrl || mods.alt || mods.command { continue; }
                if mods.shift { self.set_bookmark(i); } else { self.jump_bookmark(i); }
            }
            // M — toggle minimap. Bare key, same gate as bookmarks.
            if ui.input(|inp| inp.key_pressed(egui::Key::M)) {
                let mods = ui.input(|inp| inp.modifiers);
                if !mods.ctrl && !mods.alt && !mods.command && !mods.shift {
                    self.minimap_visible = !self.minimap_visible;
                }
            }
            // L — auto-layout. Same modifier discipline as the
            // other panel hotkeys; emits the AppCommand so the
            // mutation is undoable like a toolbar click.
            if ui.input(|inp| inp.key_pressed(egui::Key::L)) {
                let mods = ui.input(|inp| inp.modifiers);
                if !mods.ctrl && !mods.alt && !mods.command && !mods.shift {
                    cmds.push(AppCommand::AutoLayoutNodeGraph { clip_id });
                }
            }
        }

        // Ctrl+F (or Cmd+F) toggles the search overlay. Fires
        // even when the search overlay itself is already open so
        // a re-press closes it — matches the find-bar toggle
        // semantics in most editors. Skipped when other text
        // inputs are focused (rename popup, description editor)
        // so typing Ctrl+F into a TextEdit doesn't yank focus.
        let f_with_mod = ui.input(|i| {
            i.key_pressed(egui::Key::F) && (i.modifiers.ctrl || i.modifiers.command)
        });
        if f_with_mod && !self.frame_rename_editor.is_some()
            && !self.frame_description_editor.is_some()
            && !self.wireless_label_editor.is_some()
        {
            self.search_open = !self.search_open;
            if !self.search_open {
                self.search_query.clear();
                self.search_active_idx = 0;
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
                        // 2/3/4/5) Fall through to the node, sticky,
                        // frame, or add menu — node first, then
                        // sticky (above frames), then frame body,
                        // then empty canvas.
                        let mut on_node = None;
                        for (node_id, rect, _, _, _) in &node_rects {
                            if rect.contains(p) { on_node = Some(*node_id); break; }
                        }
                        let mut on_sticky: Option<NodeStickyNoteId> = None;
                        if on_node.is_none() {
                            for (sid, sr_rect, _, _, _) in sticky_rects.iter().rev() {
                                if sr_rect.contains(p) { on_sticky = Some(*sid); break; }
                            }
                        }
                        let mut on_frame: Option<NodeFrameId> = None;
                        if on_node.is_none() && on_sticky.is_none() {
                            // Reverse so a smaller frame painted on
                            // top of a larger one wins on overlap.
                            for (fid, fr_rect, _) in frame_rects.iter().rev() {
                                if fr_rect.contains(p) { on_frame = Some(*fid); break; }
                            }
                        }
                        let frame_nr = ui.ctx().cumulative_pass_nr();
                        if let Some(nid) = on_node {
                            self.node_menu = Some((nid, p));
                            self.node_menu_opened_frame = frame_nr;
                            self.add_menu_open = false;
                            self.frame_menu = None;
                            self.sticky_menu = None;
                        } else if let Some(sid) = on_sticky {
                            self.sticky_menu = Some((sid, p));
                            self.add_menu_open = false;
                            self.node_menu = None;
                            self.frame_menu = None;
                        } else if let Some(fid) = on_frame {
                            self.frame_menu = Some((fid, p));
                            self.add_menu_open = false;
                            self.node_menu = None;
                            self.sticky_menu = None;
                        } else {
                            self.add_menu_open = true;
                            self.add_menu_screen_pos = p;
                            self.add_menu_graph_pos = to_graph(p);
                            // Each menu invocation is a fresh search.
                            self.add_menu_search.clear();
                            self.add_menu_opened_frame = frame_nr;
                            self.node_menu = None;
                            self.frame_menu = None;
                            self.sticky_menu = None;
                        }
                    }
                }
            }
        }

        // ── Floating menus (egui::Area) ──────────────────────────
        if self.add_menu_open {
            self.draw_add_menu(ui, clip_id, &mut cmds, theme);
        }
        if let Some((nid, _pos)) = self.node_menu {
            // Snapshot kind+enabled so we can borrow out of `graph` cleanly
            let info = graph.nodes.iter().find(|n| n.id == nid).map(|n| (n.kind, n.enabled));
            if let Some((kind, enabled)) = info {
                self.draw_node_menu(ui, clip_id, nid, kind, enabled, &mut cmds, selected_node, selected_set, graph, solo_node);
            } else {
                self.node_menu = None;
            }
        }
        // Wireless-label editor popup. Rendered after the node menu
        // so opening it from the menu works in the same frame.
        self.draw_wireless_label_editor(ui, clip_id, &mut cmds);

        // Frame context menu. Rendered after the node menu so they
        // can never overlap visually (they're mutually exclusive at
        // the dispatcher anyway, but defence in depth).
        if self.frame_menu.is_some() {
            self.draw_frame_menu(ui, clip_id, graph, &mut cmds);
        }
        // Frame rename editor — small floating popup with a text
        // input. Triggered from the frame menu's "Rename".
        if self.frame_rename_editor.is_some() {
            self.draw_frame_rename_editor(ui, clip_id, &mut cmds);
        }
        if self.frame_description_editor.is_some() {
            self.draw_frame_description_editor(ui, clip_id, &mut cmds);
        }
        if self.sticky_menu.is_some() {
            self.draw_sticky_menu(ui, clip_id, graph, &mut cmds);
        }
        if self.sticky_text_editor.is_some() {
            self.draw_sticky_text_editor(ui, clip_id, &mut cmds);
        }

        // Tell the engine which nodes want thumbnails. Diff against
        // the previous set so we don't resend the same list every
        // frame — the engine only needs to know when the visible
        // set changes. Source IS included now (it's a passthrough
        // of the source frame, so its thumb shows the raw input —
        // genuinely useful as a "this is what's coming in" cue).
        // Output is excluded because it's a no-op in the executor
        // — node_outputs has no entry for it; the UI paints
        // Output's preview by looking up its FEEDER node's thumb
        // instead (see the Output-thumb branch in the paint loop).
        let mut current_visible: Vec<NodeId> = node_rects.iter()
            .filter(|(_, _, _, kind, _)| !matches!(kind, NodeKind::Output))
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

        // ── Vignette ──────────────────────────────────────────────
        // Painted after all canvas content so it sits on top of
        // nodes at the perimeter (gives the editor area a subtle
        // chamber-depth feel), but before the minimap / toolbar /
        // search-overlay layers which have their own backgrounds
        // and shouldn't be darkened.
        self.draw_vignette(&painter, canvas_rect);

        // ── Minimap ──────────────────────────────────────────────
        // 140 × 100 px overview anchored bottom-right of the canvas
        // with an 8 px inset. Each node paints as a kind-coloured
        // rect scaled to fit the minimap; the visible canvas area
        // draws as an accent-coloured stroke rect overlay. Click +
        // drag inside the minimap pans the canvas to centre on the
        // graph point under the cursor.
        //
        // Rendering is gated on graph_bounds — empty graphs simply
        // skip the minimap (the toolbar toggle still works; the
        // map just won't paint until there's anything to show).
        if self.minimap_visible {
            if let Some(bounds) = graph_bounds(graph) {
                const MAP_W: f32 = 140.0;
                const MAP_H: f32 = 100.0;
                const MAP_INSET: f32 = 8.0;
                let map_min = egui::pos2(
                    canvas_rect.max.x - MAP_W - MAP_INSET,
                    canvas_rect.max.y - MAP_H - MAP_INSET,
                );
                let map_rect = egui::Rect::from_min_size(map_min, egui::vec2(MAP_W, MAP_H));

                // Map graph bounds → minimap rect, with a small inner
                // padding so node rects don't kiss the minimap border.
                const MAP_PAD: f32 = 4.0;
                let inner = map_rect.shrink(MAP_PAD);
                let scale_x = inner.width() / bounds.width().max(1.0);
                let scale_y = inner.height() / bounds.height().max(1.0);
                let scale = scale_x.min(scale_y);
                // Centre the scaled graph inside `inner` so non-square
                // graphs aren't pinned to a corner.
                let scaled_w = bounds.width() * scale;
                let scaled_h = bounds.height() * scale;
                let map_origin = egui::pos2(
                    inner.min.x + (inner.width() - scaled_w) * 0.5,
                    inner.min.y + (inner.height() - scaled_h) * 0.5,
                );
                let graph_to_map = |g: egui::Pos2| -> egui::Pos2 {
                    egui::pos2(
                        map_origin.x + (g.x - bounds.min.x) * scale,
                        map_origin.y + (g.y - bounds.min.y) * scale,
                    )
                };
                let map_to_graph = |m: egui::Pos2| -> egui::Pos2 {
                    egui::pos2(
                        bounds.min.x + (m.x - map_origin.x) / scale,
                        bounds.min.y + (m.y - map_origin.y) / scale,
                    )
                };

                // Background + border. Translucent so the canvas
                // behind faintly shows through (helps users see
                // they're not covering content).
                painter.rect_filled(
                    map_rect, 4.0,
                    egui::Color32::from_rgba_unmultiplied(
                        theme.bg_deep.r(), theme.bg_deep.g(), theme.bg_deep.b(), 200,
                    ),
                );
                painter.rect_stroke(map_rect, 4.0, egui::Stroke::new(1.0, theme.border));

                // Node rects.
                for node in &graph.nodes {
                    let pos = graph.node_positions.get(&node.id).copied()
                        .unwrap_or(NodePos { x: 0.0, y: 0.0 });
                    let (half_w, half_h) = if matches!(node.kind, NodeKind::Reroute) {
                        (REROUTE_SIZE * 0.5, REROUTE_SIZE * 0.5)
                    } else {
                        (NODE_W * 0.5, NODE_H * 0.5)
                    };
                    let n_min = graph_to_map(egui::pos2(pos.x - half_w, pos.y - half_h));
                    let n_max = graph_to_map(egui::pos2(pos.x + half_w, pos.y + half_h));
                    let n_rect = egui::Rect::from_min_max(n_min, n_max);
                    let is_selected_n = *selected_node == Some(node.id)
                        || selected_set.contains(&node.id);
                    let is_soloed_n = solo_node == Some(node.id);
                    let on_n = klipp_nodes::effective_enabled(node, clip_time_secs);
                    let mut fill = node_color(&node.kind, theme);
                    if is_selected_n { fill = theme.accent; }
                    if is_soloed_n { fill = egui::Color32::from_rgb(0xff, 0xd5, 0x3a); }
                    let alpha: u8 = if on_n { 220 } else { 100 };
                    let fill_a = egui::Color32::from_rgba_unmultiplied(
                        fill.r(), fill.g(), fill.b(), alpha,
                    );
                    if matches!(node.kind, NodeKind::Reroute) {
                        painter.circle_filled(n_rect.center(), n_rect.width().max(2.0) * 0.5, fill_a);
                    } else {
                        painter.rect_filled(n_rect, 1.0, fill_a);
                    }
                }

                // Viewport overlay — the canvas's visible area
                // mapped onto the minimap. Painted last so it sits
                // on top of every node rect.
                let canvas_centre = canvas_rect.center();
                let viewport_min_g = egui::pos2(
                    (canvas_rect.min.x - canvas_centre.x) / self.zoom - self.pan.x,
                    (canvas_rect.min.y - canvas_centre.y) / self.zoom - self.pan.y,
                );
                let viewport_max_g = egui::pos2(
                    (canvas_rect.max.x - canvas_centre.x) / self.zoom - self.pan.x,
                    (canvas_rect.max.y - canvas_centre.y) / self.zoom - self.pan.y,
                );
                let v_rect = egui::Rect::from_min_max(
                    graph_to_map(viewport_min_g),
                    graph_to_map(viewport_max_g),
                ).intersect(map_rect);
                if v_rect.is_positive() {
                    painter.rect_stroke(
                        v_rect, 2.0,
                        egui::Stroke::new(1.5, theme.accent),
                    );
                }

                // Click + drag → pan canvas to centre on the graph
                // point under the cursor in the minimap. Sense both
                // click and drag so a single click also re-centres
                // (no need to wiggle the cursor to get a pan).
                let map_resp = ui.interact(
                    map_rect,
                    ui.id().with("ngv2_minimap"),
                    egui::Sense::click_and_drag(),
                );
                if map_resp.clicked() || map_resp.dragged() {
                    if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                        if map_rect.contains(p) {
                            let target = map_to_graph(p);
                            self.pan = egui::vec2(-target.x, -target.y);
                        }
                    }
                }
            }
        }

        // ── In-graph search overlay ───────────────────────────────
        // Floating text input near the canvas top-left while
        // search_open is true. Tab / Shift+Tab cycle the active
        // match and pan the canvas to centre on it. Esc closes
        // and clears. Match counter ("3 of 12") on the right side
        // of the input gives instant feedback that the query is
        // matching.
        if self.search_open {
            let area_id = ui.id().with("ngv2_search_overlay");
            let pos = egui::pos2(canvas_rect.min.x + 12.0, canvas_rect.min.y + 12.0);
            let prev_query = self.search_query.clone();
            let mut close = false;
            let mut tab_forward = false;
            let mut tab_back = false;
            egui::Area::new(area_id)
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("\u{1F50D}").size(13.0));
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .hint_text("Find node by label or kind…")
                                    .desired_width(200.0),
                            );
                            // Auto-focus on every paint while open
                            // — re-focuses if the user clicks
                            // elsewhere accidentally. Cheap because
                            // request_focus is idempotent.
                            resp.request_focus();
                            // Tab while focused = cycle. egui's
                            // default Tab behaviour moves focus to
                            // the next widget; we want it as a hot
                            // gesture instead.
                            if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Tab)) {
                                let shift = ui.input(|i| i.modifiers.shift);
                                if shift { tab_back = true; } else { tab_forward = true; }
                            }
                            if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                close = true;
                            }
                            // Match counter — current/total when
                            // there are matches, "no matches" when
                            // there's a query but nothing hit, blank
                            // when the query is empty.
                            let total = search_matches.len();
                            let active = if total == 0 { 0 } else {
                                (self.search_active_idx % total) + 1
                            };
                            let counter = if self.search_query.is_empty() {
                                String::new()
                            } else if total == 0 {
                                "no matches".to_string()
                            } else {
                                format!("{active} of {total}")
                            };
                            if !counter.is_empty() {
                                ui.label(
                                    egui::RichText::new(counter)
                                        .small()
                                        .color(egui::Color32::from_rgba_unmultiplied(
                                            theme.text_primary.r(),
                                            theme.text_primary.g(),
                                            theme.text_primary.b(),
                                            180,
                                        )),
                                );
                            }
                            if ui.small_button("\u{2715}").on_hover_text("Close (Esc)").clicked() {
                                close = true;
                            }
                        });
                    });
                });
            // Reset Tab cursor whenever the query mutates so the
            // first match is always "1 of N" for a fresh query.
            if self.search_query != prev_query {
                self.search_active_idx = 0;
            }
            // Apply Tab cycling + pan.
            if (tab_forward || tab_back) && !search_matches.is_empty() {
                let n = search_matches.len();
                if tab_forward {
                    self.search_active_idx = (self.search_active_idx + 1) % n;
                } else {
                    self.search_active_idx = (self.search_active_idx + n - 1) % n;
                }
                let target_id = search_matches[self.search_active_idx % n];
                if let Some(pos) = graph.node_positions.get(&target_id).copied() {
                    self.pan = egui::vec2(-pos.x, -pos.y);
                }
            }
            if close {
                self.search_open = false;
                self.search_query.clear();
                self.search_active_idx = 0;
            }
        }

        // Bookmark toast — transient feedback for set/jump/empty.
        // Self-fading; ~1.5 s lifetime, painted in the canvas's top-
        // right corner. Polarity-aware via theme tokens so it stays
        // legible on light themes too.
        if let Some((msg, set_at)) = self.bookmark_toast.clone() {
            let age = set_at.elapsed().as_secs_f32();
            const LIFETIME: f32 = 1.5;
            if age < LIFETIME {
                let fade = 1.0 - age / LIFETIME;
                let alpha = (fade * 255.0).clamp(0.0, 255.0) as u8;
                let bg_alpha = (fade * 200.0).clamp(0.0, 200.0) as u8;
                let bg = egui::Color32::from_rgba_unmultiplied(
                    theme.bg_raised.r(), theme.bg_raised.g(), theme.bg_raised.b(), bg_alpha,
                );
                let fg = egui::Color32::from_rgba_unmultiplied(
                    theme.text_primary.r(), theme.text_primary.g(), theme.text_primary.b(), alpha,
                );
                let pad = egui::vec2(10.0, 6.0);
                let font = egui::FontId::proportional(11.0);
                let galley = painter.layout_no_wrap(msg, font, fg);
                let size = galley.size() + pad * 2.0;
                let pos = egui::pos2(
                    canvas_rect.max.x - size.x - 12.0,
                    canvas_rect.min.y + 12.0,
                );
                let toast_rect = egui::Rect::from_min_size(pos, size);
                painter.rect_filled(toast_rect, 4.0, bg);
                painter.rect_stroke(
                    toast_rect, 4.0,
                    egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(
                        theme.border.r(), theme.border.g(), theme.border.b(), alpha,
                    )),
                );
                painter.galley(toast_rect.min + pad, galley, fg);
                ui.ctx().request_repaint();
            } else {
                self.bookmark_toast = None;
            }
        }

        cmds
    }

    /// Compute zoom + pan that frames every node in the graph
    /// inside the canvas with a comfortable margin. Called on first
    /// open of a clip, on Fit toolbar click, and on `F` press.
    fn fit_to_nodes(&mut self, graph: &NodeGraph, canvas_rect: egui::Rect) {
        let Some(bounds) = graph_bounds(graph) else { return; };
        let graph_w = bounds.width().max(1.0);
        let graph_h = bounds.height().max(1.0);
        // 12 % padding on each side so framed nodes don't touch the edge.
        let target_w = canvas_rect.width() * 0.76;
        let target_h = canvas_rect.height() * 0.76;
        let zoom_x = target_w / graph_w;
        let zoom_y = target_h / graph_h;
        let new_zoom = zoom_x.min(zoom_y).clamp(0.25, 3.0);

        // Pan so the graph centre lands on the canvas centre.
        let graph_centre = bounds.center();
        // Inverse of `to_screen`: screen = origin + (graph + pan) * zoom
        // We want graph_centre to map to canvas_centre, so:
        //   canvas_centre = canvas_centre + (graph_centre + pan) * zoom
        //   pan = -graph_centre
        self.zoom = new_zoom;
        self.pan = egui::vec2(-graph_centre.x, -graph_centre.y);
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: egui::Rect, theme: &KlippTheme) {
        let zoom = self.zoom;
        // Skip the grid entirely when fully zoomed out — lines fuse
        // into a wash and hurt readability.
        if zoom < 0.4 { return; }
        let origin = rect.center() + self.pan * zoom;

        // Adaptive zoom curves — each tier fades in over a small
        // band so the grid never pops on/off, it gradates as the
        // user zooms.
        //   minor:     fades 0 → 1 over zoom 0.4..0.7
        //   major:     1.0 across the full visible range
        //   sub-minor: fades 0 → 1 over zoom 1.2..1.8 so close-up
        //              work has a finer reference without
        //              cluttering at typical zoom.
        let minor_alpha = ((zoom - 0.4) / 0.3).clamp(0.0, 1.0);
        let sub_minor_alpha = ((zoom - 1.2) / 0.6).clamp(0.0, 1.0);

        // Cool blue-gray grid lines — was theme.accent_dim, but that
        // sat in the same warm amber family as the brand gold and
        // made the whole canvas read monochromatic. Pulling the
        // grid into a neutral cool tone (#9aa8b8 base, alpha-modulated
        // per tier) lets the gold of the accent / matched roles /
        // selection halo pop hard against an unambiguously different
        // hue family.
        const GRID_COOL: egui::Color32 = egui::Color32::from_rgb(0x9a, 0xa8, 0xb8);
        let _ = theme;
        let mk = |alpha: u8, w: f32| {
            egui::Stroke::new(
                w,
                egui::Color32::from_rgba_unmultiplied(
                    GRID_COOL.r(),
                    GRID_COOL.g(),
                    GRID_COOL.b(),
                    alpha,
                ),
            )
        };

        // Sub-minor — every 8 graph-px. Only paints once we're
        // zoomed in enough that 8-px-spacing in graph coords
        // translates to a visually discernible spacing.
        if sub_minor_alpha > 0.001 {
            let sub_spacing = (GRID_SPACING * 0.25) * zoom;
            let stroke = mk((10.0 * sub_minor_alpha) as u8, 0.5);
            self.draw_grid_lines(painter, rect, origin, sub_spacing, stroke);
        }

        // Minor — every 32 graph-px. The "default" grid most users
        // navigate by.
        if minor_alpha > 0.001 {
            let stroke = mk((18.0 * minor_alpha) as u8, 0.5);
            self.draw_grid_lines(painter, rect, origin, GRID_SPACING * zoom, stroke);
        }

        // Major — every 4 × 32 = 128 graph-px. ~2× the minor alpha
        // and a 1.0 px stroke so the structural divisions read at
        // a glance without dominating the canvas. Always at full
        // alpha as long as the grid is showing at all (no zoom
        // fade) — they're the visual ruler.
        let stroke = mk(38, 1.0);
        self.draw_grid_lines(painter, rect, origin, GRID_SPACING * 4.0 * zoom, stroke);
    }

    fn draw_grid_lines(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        origin: egui::Pos2,
        spacing: f32,
        stroke: egui::Stroke,
    ) {
        if spacing <= 0.5 { return; }
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

    /// Edge vignette — feathered dark band around the canvas
    /// perimeter that gives the editor area a subtle "in a
    /// chamber" depth instead of a flat slab. Pure black with
    /// linear-falloff alpha; works on both light and dark themes
    /// because darker corners read as "depth" universally.
    fn draw_vignette(&self, painter: &egui::Painter, rect: egui::Rect) {
        const FEATHER_PX: i32 = 14;
        const PEAK_ALPHA: f32 = 60.0;
        for i in 0..FEATHER_PX {
            let a = (PEAK_ALPHA * (1.0 - i as f32 / FEATHER_PX as f32)) as u8;
            if a == 0 { break; }
            let inset_rect = rect.shrink(i as f32);
            painter.rect_stroke(
                inset_rect, 0.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, a)),
            );
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
        theme: &KlippTheme,
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
                ("Reroute", "Reroute"),
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
                            // Caption colour: full text_primary at
                            // small size. Brighter than .weak()
                            // (which is text_dim) so labels are
                            // legible at a glance; size still
                            // subordinates them visually so they
                            // read as captions rather than entries.
                            let caption_color = theme.text_primary;

                            // ── Recent (only when not searching) ──
                            // Surfaces the user's last few picks at
                            // the top so the common case ("add the
                            // same kind I just added on the previous
                            // node") is one click away. Hidden when
                            // empty so first-open is uncluttered.
                            if !in_search && !self.add_menu_recents.is_empty() {
                                ui.label(
                                    egui::RichText::new("RECENT")
                                        .small()
                                        .color(caption_color),
                                );
                                for kind in self.add_menu_recents.clone() {
                                    if let Some(label) = label_for_kind(kind) {
                                        if add_menu_entry(ui, label, theme) {
                                            chosen_kind = Some(kind);
                                        }
                                    }
                                }
                                ui.add_space(8.0);
                            }

                            // ── Categories ───────────────────────
                            // Inter-category spacing for a calmer
                            // scan rhythm. Padding lives here so the
                            // last category doesn't trail dead
                            // whitespace at the bottom of the menu.
                            let mut first_cat = true;
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
                                if !first_cat { ui.add_space(8.0); }
                                first_cat = false;

                                if in_search {
                                    // Search mode: skip collapse UI;
                                    // show every match flat under a
                                    // category caption so the user
                                    // can still tell which bucket
                                    // each entry came from.
                                    ui.label(
                                        egui::RichText::new(cat_name.to_uppercase())
                                            .small()
                                            .color(caption_color),
                                    );
                                    for (label, kind_str) in visible {
                                        if add_menu_entry(ui, label, theme) {
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
                                            egui::RichText::new(header_label)
                                                .strong()
                                                .color(theme.text_primary),
                                        )
                                        .sense(egui::Sense::click()),
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
                                                if add_menu_entry(ui, label, theme) {
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
                                    ui.label(
                                        egui::RichText::new("No matches")
                                            .color(theme.text_primary),
                                    );
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

    /// Right-click menu for frames. Rename / Recolour swatches /
    /// Delete frame (members keep their positions).
    fn draw_frame_menu(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        graph: &NodeGraph,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((frame_id, pos)) = self.frame_menu else { return; };
        // Resolve current colour for the swatch row's "selected"
        // marker. None if the frame was deleted between the right-
        // click and the next paint pass.
        let current_color = graph.frames.iter()
            .find(|f| f.id == frame_id)
            .map(|f| f.color_rgba);
        let mut close = false;
        let area_id = ui.id().with(("ngv2_frame_menu", frame_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui.button("Rename…").clicked() {
                        let initial = graph.frames.iter()
                            .find(|f| f.id == frame_id)
                            .map(|f| f.label.clone())
                            .unwrap_or_default();
                        self.frame_rename_editor = Some((frame_id, pos, initial));
                        close = true;
                    }
                    if ui.button("Edit description…").clicked() {
                        let initial = graph.frames.iter()
                            .find(|f| f.id == frame_id)
                            .map(|f| f.description.clone())
                            .unwrap_or_default();
                        self.frame_description_editor = Some((frame_id, pos, initial));
                        close = true;
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Colour").small().weak());
                    ui.horizontal(|ui| {
                        for &(label, rgba) in FRAME_COLOR_PALETTE.iter() {
                            let bytes = rgba.to_le_bytes();
                            let swatch_color = egui::Color32::from_rgba_unmultiplied(
                                bytes[0], bytes[1], bytes[2], 255,
                            );
                            let selected = current_color == Some(rgba);
                            let glyph = if selected { "\u{25C9}" } else { "\u{25CB}" };
                            let resp = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(glyph).size(14.0).color(swatch_color),
                                ).sense(egui::Sense::click()),
                            ).on_hover_text(label);
                            if resp.clicked() {
                                cmds.push(AppCommand::RecolorNodeFrame {
                                    clip_id, frame_id, color_rgba: rgba,
                                });
                                close = true;
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Delete frame").clicked() {
                        cmds.push(AppCommand::DeleteNodeFrame { clip_id, frame_id });
                        close = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if area_resp.response.clicked_elsewhere() { close = true; }
        if close { self.frame_menu = None; }
    }

    /// Inline rename popup for a frame's label. Triggered from the
    /// frame menu's "Rename…" entry.
    fn draw_frame_rename_editor(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((frame_id, pos, _)) = self.frame_rename_editor.as_ref().map(
            |(f, p, b)| (*f, *p, b.clone())
        ) else { return; };
        let mut close = false;
        let mut save = false;
        let area_id = ui.id().with(("ngv2_frame_rename", frame_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Frame label").small().weak());
                    if let Some((_, _, buf)) = self.frame_rename_editor.as_mut() {
                        let resp = ui.add(
                            egui::TextEdit::singleline(buf)
                                .hint_text("e.g. Sky / Skin / Roto")
                                .desired_width(180.0),
                        );
                        resp.request_focus();
                        if resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            save = true;
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() { save = true; }
                        if ui.button("Cancel").clicked() { close = true; }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if area_resp.response.clicked_elsewhere() { close = true; }
        if save {
            let label = self.frame_rename_editor.as_ref()
                .map(|(_, _, b)| b.clone()).unwrap_or_default();
            cmds.push(AppCommand::RenameNodeFrame { clip_id, frame_id, label });
            close = true;
        }
        if close { self.frame_rename_editor = None; }
    }

    /// Right-click menu for a sticky note: Edit text… / Delete.
    fn draw_sticky_menu(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        graph: &NodeGraph,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((sticky_id, pos)) = self.sticky_menu else { return; };
        let mut close = false;
        let area_id = ui.id().with(("ngv2_sticky_menu", sticky_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui.button("Edit text…").clicked() {
                        let initial = graph.sticky_notes.iter()
                            .find(|s| s.id == sticky_id)
                            .map(|s| s.text.clone())
                            .unwrap_or_default();
                        self.sticky_text_editor = Some((sticky_id, pos, initial));
                        close = true;
                    }
                    if ui.button("Delete note").clicked() {
                        cmds.push(AppCommand::DeleteStickyNote { clip_id, sticky_id });
                        close = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if area_resp.response.clicked_elsewhere() { close = true; }
        if close { self.sticky_menu = None; }
    }

    /// Multi-line text editor for a sticky note. Auto-focused on
    /// open; Ctrl+Enter saves; Esc cancels; click-elsewhere also
    /// saves (sticky notes are throwaway annotation, so accidental
    /// dismissals shouldn't lose what the user typed — this is the
    /// opposite of the frame description editor's "discard on
    /// click-out" behaviour, which gates on stricter intent).
    fn draw_sticky_text_editor(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((sticky_id, pos, _)) = self.sticky_text_editor.as_ref().map(
            |(s, p, b)| (*s, *p, b.clone())
        ) else { return; };
        let mut close = false;
        let mut save_and_close = false;
        let area_id = ui.id().with(("ngv2_sticky_edit", sticky_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Sticky note").small().weak());
                    if let Some((_, _, buf)) = self.sticky_text_editor.as_mut() {
                        let resp = ui.add(
                            egui::TextEdit::multiline(buf)
                                .hint_text("Notes…")
                                .desired_rows(4)
                                .desired_width(240.0),
                        );
                        resp.request_focus();
                        let mods = ui.input(|i| i.modifiers);
                        if resp.has_focus()
                            && (mods.ctrl || mods.command)
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            save_and_close = true;
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() { save_and_close = true; }
                        if ui.button("Cancel").clicked() { close = true; }
                        ui.label(egui::RichText::new("Ctrl+Enter to save").small().weak());
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        // Click-elsewhere → save (sticky-note semantics: throwaway
        // notes shouldn't lose what the user typed).
        if area_resp.response.clicked_elsewhere() { save_and_close = true; }
        if save_and_close {
            let text = self.sticky_text_editor.as_ref()
                .map(|(_, _, b)| b.clone()).unwrap_or_default();
            cmds.push(AppCommand::SetStickyNoteText { clip_id, sticky_id, text });
            close = true;
        }
        if close { self.sticky_text_editor = None; }
    }

    /// Multi-line description editor for a frame. Triggered from
    /// the frame menu's "Edit description…" entry. Ctrl+Enter
    /// saves; Esc cancels; click-elsewhere also closes (the buffer
    /// is dropped without saving — matches the rename-editor
    /// pattern's behaviour for accidental dismissals).
    fn draw_frame_description_editor(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((frame_id, pos, _)) = self.frame_description_editor.as_ref().map(
            |(f, p, b)| (*f, *p, b.clone())
        ) else { return; };
        let mut close = false;
        let mut save = false;
        let area_id = ui.id().with(("ngv2_frame_descr", frame_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Frame description").small().weak());
                    if let Some((_, _, buf)) = self.frame_description_editor.as_mut() {
                        let resp = ui.add(
                            egui::TextEdit::multiline(buf)
                                .hint_text("Notes about this group of nodes…")
                                .desired_rows(4)
                                .desired_width(280.0),
                        );
                        // Auto-focus the editor on first paint so
                        // the user can start typing immediately.
                        if resp.gained_focus() || ui.ctx().memory(|m| m.focused().is_none()) {
                            resp.request_focus();
                        }
                        // Ctrl+Enter or Cmd+Enter saves — plain
                        // Enter inserts a newline (multiline
                        // semantics are what makes this distinct
                        // from the rename popup).
                        let mods = ui.input(|i| i.modifiers);
                        if resp.has_focus()
                            && (mods.ctrl || mods.command)
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            save = true;
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() { save = true; }
                        if ui.button("Cancel").clicked() { close = true; }
                        ui.label(egui::RichText::new("Ctrl+Enter to save").small().weak());
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if area_resp.response.clicked_elsewhere() { close = true; }
        if save {
            let description = self.frame_description_editor.as_ref()
                .map(|(_, _, b)| b.clone()).unwrap_or_default();
            cmds.push(AppCommand::SetNodeFrameDescription {
                clip_id, frame_id, description,
            });
            close = true;
        }
        if close { self.frame_description_editor = None; }
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
        solo_node: Option<NodeId>,
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
                    // Reroute is already a passthrough — Bypass
                    // would be a no-op-on-no-op; suppress to avoid
                    // user confusion (Delete / Disconnect still
                    // appear).
                    let is_passthrough_kind = matches!(kind, NodeKind::Reroute);
                    if !is_structural && !is_passthrough_kind {
                        let toggle = if enabled { "Bypass" } else { "Enable" };
                        if ui.button(toggle).clicked() {
                            cmds.push(AppCommand::ToggleNodeEnabled { clip_id, node_id });
                            close = true;
                        }
                    }
                    // Solo — gated to the same set as Bypass (skip
                    // structural Source/Output, skip Reroute since
                    // soloing a passthrough just shows what's
                    // upstream of it, which is what the user already
                    // sees by soloing that upstream node directly).
                    // Toggle label flips when the right-clicked node
                    // is the current solo so the user has a one-
                    // click clear from the same menu they set it in.
                    if !is_structural && !is_passthrough_kind {
                        let is_soloed = solo_node == Some(node_id);
                        let label = if is_soloed { "Clear solo" } else { "Solo this node" };
                        if ui.button(label).clicked() {
                            let new_target = if is_soloed { None } else { Some(node_id) };
                            cmds.push(AppCommand::SetNodeSolo { node_id: new_target });
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
                        // Lightweight frame — purely visual grouping.
                        // Distinct from Compound (which collapses into
                        // a sub-graph). Users get both options on a
                        // multi-select; Compound for "I want to hide
                        // this complexity", Frame for "I want to label
                        // this region".
                        let frame_label = format!(
                            "Frame {} (visual group)",
                            selected_set.len(),
                        );
                        if ui.button(frame_label).clicked() {
                            cmds.push(AppCommand::CreateNodeFrame {
                                clip_id,
                                node_ids: selected_set.iter().copied().collect(),
                                label: String::new(),
                                color_rgba: DEFAULT_FRAME_COLOR_RGBA,
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

/// Bounding box of every node's body rectangle in graph coordinates.
/// Empty graphs return `None` (defensive — fit-to-view + minimap
/// short-circuit cleanly). Reroute nodes contribute their smaller
/// 20×20 footprint rather than NODE_W×NODE_H so the bounds reflect
/// what's actually painted.
pub(super) fn graph_bounds(graph: &NodeGraph) -> Option<egui::Rect> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for node in &graph.nodes {
        let pos = graph.node_positions.get(&node.id).copied()
            .unwrap_or(NodePos { x: 0.0, y: 0.0 });
        let (half_w, half_h) = if matches!(node.kind, NodeKind::Reroute) {
            (REROUTE_SIZE * 0.5, REROUTE_SIZE * 0.5)
        } else {
            (NODE_W * 0.5, NODE_H * 0.5)
        };
        min_x = min_x.min(pos.x - half_w);
        min_y = min_y.min(pos.y - half_h);
        max_x = max_x.max(pos.x + half_w);
        max_y = max_y.max(pos.y + half_h);
    }
    if !min_x.is_finite() || !max_x.is_finite() { return None; }
    Some(egui::Rect::from_min_max(
        egui::pos2(min_x, min_y),
        egui::pos2(max_x, max_y),
    ))
}

/// Custom add-menu entry row — full-width 22 px clickable strip
/// with stronger hover feedback than egui's default button. Text
/// in theme.text_primary at alpha 230 idle, alpha 255 + accent-
/// tinted bg fill on hover so the hovered choice pops without
/// shouting. Returns true on click.
fn add_menu_entry(ui: &mut egui::Ui, label: &str, theme: &KlippTheme) -> bool {
    let size = egui::vec2(ui.available_width(), 22.0);
    let resp = ui.allocate_response(size, egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        let a = theme.accent;
        let bg = egui::Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 32);
        painter.rect_filled(resp.rect, 3.0, bg);
    }
    let text_color = if resp.hovered() {
        theme.text_primary
    } else {
        egui::Color32::from_rgba_unmultiplied(
            theme.text_primary.r(),
            theme.text_primary.g(),
            theme.text_primary.b(),
            230,
        )
    };
    painter.text(
        egui::pos2(resp.rect.min.x + 8.0, resp.rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        text_color,
    );
    resp.clicked()
}

/// Largest sub-rect of `container` that preserves `image_aspect`
/// (width/height), centred. Result either pillar-boxes (container
/// wider than image) or letter-boxes (container taller). Used to
/// stop the per-node thumbnail looking vertically cramped when
/// the body cell's aspect doesn't match the 16:9 thumb.
fn aspect_fit_rect(container: egui::Rect, image_aspect: f32) -> egui::Rect {
    let cw = container.width().max(0.001);
    let ch = container.height().max(0.001);
    let c_aspect = cw / ch;
    if c_aspect > image_aspect {
        // Container wider than image — pillarbox.
        let h = ch;
        let w = h * image_aspect;
        let x = container.center().x - w * 0.5;
        egui::Rect::from_min_size(egui::pos2(x, container.min.y), egui::vec2(w, h))
    } else {
        // Container taller than image — letterbox.
        let w = cw;
        let h = w / image_aspect;
        let y = container.center().y - h * 0.5;
        egui::Rect::from_min_size(egui::pos2(container.min.x, y), egui::vec2(w, h))
    }
}

/// Paint a textured rect with rounded corners. egui's `painter.image`
/// is unrounded; this routes through the lower-level `Shape::Rect`
/// with `fill_texture_id` + a 0..1 UV so the same texture renders
/// with a corner radius. Used for per-node thumbnail previews so
/// they read as "framed previews" rather than slabs.
fn paint_thumb_rounded(
    painter: &egui::Painter,
    rect: egui::Rect,
    tex_id: egui::TextureId,
    rounding_px: f32,
) {
    painter.add(egui::Shape::Rect(egui::epaint::RectShape {
        rect,
        rounding: egui::Rounding::same(rounding_px),
        fill: egui::Color32::WHITE,
        stroke: egui::Stroke::NONE,
        blur_width: 0.0,
        fill_texture_id: tex_id,
        uv: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
    }));
}

/// One-letter category badge for the per-node thumbnail's corner
/// chip. 5 buckets — corrections / matte / compositing / looks /
/// routing — plus a couple of structural one-offs so the badge
/// stays distinct rather than always reading "C".
fn kind_category_glyph(kind: &NodeKind) -> &'static str {
    match kind {
        // Source = play (input flowing IN); Output = eject (the
        // final composite leaving the graph). Distinct so the two
        // terminals are visually unambiguous on the thumbnail
        // badge.
        NodeKind::Source => "\u{25B6}",   // ▶
        NodeKind::Output => "\u{23CF}",   // ⏏
        NodeKind::Compound => "\u{229E}",                  // ⊞
        NodeKind::Reroute
        | NodeKind::RGBSplitter
        | NodeKind::RGBCombiner
        | NodeKind::PrimariesMatrix => "\u{21B9}",         // ↹
        NodeKind::Qualifier
        | NodeKind::LumaKey
        | NodeKind::Mask
        | NodeKind::CombineMattes
        | NodeKind::ExternalMatte
        | NodeKind::MaskCutout => "M",
        NodeKind::Layer
        | NodeKind::Blend
        | NodeKind::ApplyMatte
        | NodeKind::LayerMixer => "L",
        NodeKind::Lut3D
        | NodeKind::FilmGrain
        | NodeKind::ColorMatch
        | NodeKind::ColorWarper
        | NodeKind::Denoise => "F",
        // Everything else is the primary-correction family.
        _ => "C",
    }
}

/// Build a 192×108 RGBA overlay image marking exposure-clipped
/// pixels — red where any channel is at the upper sentinel
/// (>= 248) and blue where all channels are at the lower sentinel
/// (<= 8). Transparent everywhere else so the overlay paints
/// cleanly on top of the thumb without hiding the underlying
/// image. Sentinel asymmetry (any-vs-all) matches the standard
/// scope-side convention: even one clipped channel is a problem
/// for highlights, but a "crushed" pixel typically needs every
/// channel pinned at the bottom.
fn build_clipping_overlay_image(thumb_bytes: &[u8]) -> egui::ColorImage {
    const TW: usize = 192;
    const TH: usize = 108;
    let mut out = vec![0u8; TW * TH * 4];
    if thumb_bytes.len() < TW * TH * 4 {
        return egui::ColorImage::from_rgba_unmultiplied([TW, TH], &out);
    }
    for i in 0..(TW * TH) {
        let r = thumb_bytes[i * 4];
        let g = thumb_bytes[i * 4 + 1];
        let b = thumb_bytes[i * 4 + 2];
        if r >= 248 || g >= 248 || b >= 248 {
            out[i * 4] = 0xff;
            out[i * 4 + 1] = 0x55;
            out[i * 4 + 2] = 0x55;
            out[i * 4 + 3] = 0xb4;
        } else if r <= 8 && g <= 8 && b <= 8 {
            out[i * 4] = 0x55;
            out[i * 4 + 1] = 0x88;
            out[i * 4 + 2] = 0xff;
            out[i * 4 + 3] = 0xb4;
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([TW, TH], &out)
}

/// Compute a 64-bucket luma histogram from a 192×108 RGBA u8 thumb.
/// Luma weights match Rec.709 (the playback default; the histogram
/// is a quick visual aid not a calibrated scope, so a fixed
/// weighting is fine). Returns the bin counts; the caller
/// normalises against the max bin when painting.
fn compute_thumb_histogram(thumb_bytes: &[u8]) -> [u32; 64] {
    let mut bins = [0u32; 64];
    if thumb_bytes.len() < 4 { return bins; }
    let pixels = thumb_bytes.len() / 4;
    for i in 0..pixels {
        let r = thumb_bytes[i * 4] as f32;
        let g = thumb_bytes[i * 4 + 1] as f32;
        let b = thumb_bytes[i * 4 + 2] as f32;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let bucket = ((luma / 256.0) * 64.0).clamp(0.0, 63.0) as usize;
        bins[bucket] += 1;
    }
    bins
}

/// Paint a tiny histogram strip across the bottom 18 % of the
/// thumb. Translucent dark backdrop + light bars; bars are
/// normalised against the bin max so the shape always reads
/// regardless of total pixel count.
fn paint_thumb_histogram(
    painter: &egui::Painter,
    thumb_rect: egui::Rect,
    bins: &[u32; 64],
    theme: &KlippTheme,
) {
    let strip_h = thumb_rect.height() * 0.18;
    let strip_rect = egui::Rect::from_min_max(
        egui::pos2(thumb_rect.min.x, thumb_rect.max.y - strip_h),
        thumb_rect.max,
    );
    // Backdrop — translucent dark band so the bars read on
    // bright thumbs (e.g. blown-out skies) too.
    painter.rect_filled(
        strip_rect, 0.0,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 110),
    );
    let max_bin = bins.iter().copied().max().unwrap_or(1).max(1) as f32;
    let bar_w = strip_rect.width() / bins.len() as f32;
    for (i, count) in bins.iter().enumerate() {
        let normalised = (*count as f32 / max_bin).clamp(0.0, 1.0);
        let bar_h = strip_rect.height() * normalised;
        let x0 = strip_rect.min.x + bar_w * i as f32;
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(x0, strip_rect.max.y - bar_h),
            egui::pos2(x0 + bar_w, strip_rect.max.y),
        );
        let bar_color = egui::Color32::from_rgba_unmultiplied(
            theme.text_primary.r(),
            theme.text_primary.g(),
            theme.text_primary.b(),
            210,
        );
        painter.rect_filled(bar_rect, 0.0, bar_color);
    }
}

/// Paint the bottom-right category badge over a thumbnail. Small
/// rounded chip with theme.bg_deep at alpha 200 + the category
/// glyph; inset from the thumb's corner so it sits inside the
/// rounded edge.
fn paint_thumb_badge(
    painter: &egui::Painter,
    thumb_rect: egui::Rect,
    glyph: &'static str,
    theme: &KlippTheme,
    zoom: f32,
) {
    let chip_size = (12.0 * zoom).max(9.0);
    let inset = 3.0 * zoom.max(0.5);
    let chip_rect = egui::Rect::from_min_size(
        egui::pos2(
            thumb_rect.max.x - chip_size - inset,
            thumb_rect.max.y - chip_size - inset,
        ),
        egui::vec2(chip_size, chip_size),
    );
    let bg = egui::Color32::from_rgba_unmultiplied(
        theme.bg_deep.r(), theme.bg_deep.g(), theme.bg_deep.b(), 200,
    );
    painter.rect_filled(chip_rect, 2.5, bg);
    painter.text(
        chip_rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional((chip_size * 0.65).max(7.0)),
        theme.text_primary,
    );
}

fn input_port_pos(rect: &egui::Rect, port: u8, count: u8) -> egui::Pos2 {
    port_pos_on_edge(rect, port, count.max(1), false)
}

fn output_port_pos(rect: &egui::Rect, port: u8, kind: &NodeKind) -> egui::Pos2 {
    let count = kind.output_port_count().max(1);
    port_pos_on_edge(rect, port, count, true)
}

fn port_pos_on_edge(rect: &egui::Rect, port: u8, count: u8, output: bool) -> egui::Pos2 {
    // Small-rect fast path: rects under 40 px tall are dot-style
    // nodes (Reroute today; future routing kinds in the same family).
    // They have no header strip, so ports land on the centre of the
    // rect's left/right edge directly.
    let small = rect.height() < 40.0;
    let (top, h) = if small {
        (rect.min.y, rect.height())
    } else {
        let body_top = rect.min.y + NODE_HEADER_H * (rect.height() / NODE_H);
        (body_top, rect.max.y - body_top)
    };
    let frac = (port as f32 + 0.5) / count as f32;
    let y = top + h * frac;
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

/// Position along the bezier at fractional `phase` in [0, 1].
/// Indexes into the 25-point sampled polyline rather than re-
/// solving the bezier — the polyline is already accurate enough
/// for a moving pulse (each segment is ~1/24 of the total length).
fn bezier_at_phase(from: egui::Pos2, to: egui::Pos2, phase: f32) -> egui::Pos2 {
    let pts = bezier_polyline(from, to);
    let phase = phase.clamp(0.0, 1.0);
    let idx_f = phase * (pts.len() - 1) as f32;
    let idx = idx_f.floor() as usize;
    if idx >= pts.len() - 1 {
        return pts[pts.len() - 1];
    }
    let frac = idx_f - idx as f32;
    egui::pos2(
        pts[idx].x + (pts[idx + 1].x - pts[idx].x) * frac,
        pts[idx].y + (pts[idx + 1].y - pts[idx].y) * frac,
    )
}
