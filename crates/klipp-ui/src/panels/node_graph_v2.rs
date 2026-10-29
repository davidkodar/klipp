//! V2 node-graph editor — 2D canvas with drag-to-move nodes,
//! drag-to-connect wires, multi-port visualisation, pan + zoom.
//!
//! Replaces (or augments — see toggle) the legacy horizontal strip
//! in `panels/node_graph.rs`. The painter borrows a `CanvasGraph`; the
//! grade adapter keeps the same `AppCommand` mutation surface (AddNode / RemoveNode /
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
use crate::widgets::menu::SubmenuRows;

use crate::theme::KlippTheme;
use crate::widgets::menu::{menu_frame, menu_item};
use klipp_core::{AppCommand, ClipId};
use klipp_compose::InspectAt as NodeOutput;
use klipp_nodes::{NodeFrameId, NodeGraph, NodeId, NodePos, NodeStickyNoteId};

mod model;
mod picture;
pub(crate) use picture::node_name;
use model::{CanvasGraph, CanvasKind, CanvasNode, GradeCanvas, MenuCategory, Palette};

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
// Composite shifted from moss-green (#3f5042) to muted teal so
// it occupies a clearly different hue family from look/finishing
// (warm ochre) and sits cleanly between the cool primary-grade
// slate and the matte plum. Same low chroma.
const FAMILY_COMPOSITE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x50, 0x55);
const FAMILY_LOOK: egui::Color32 = egui::Color32::from_rgb(0x52, 0x4e, 0x3e);
// Structural shifted from neutral grey (#454548) to a warm-tinted
// grey so utility/routing kinds don't read as the same hue family
// as the cool primary-grade slate.
const FAMILY_STRUCTURAL: egui::Color32 = egui::Color32::from_rgb(0x4a, 0x47, 0x44);
/// Source = structural with a faint green tint (input flowing in);
/// Output = structural with a faint blue tint (sink). Both are
/// otherwise the structural-gray family — distinct enough to read
/// as separate terminals, neutral enough to sit alongside the
/// other structural kinds without claiming a unique family slot.
const FAMILY_SOURCE: egui::Color32 = egui::Color32::from_rgb(0x3e, 0x4d, 0x44);
const FAMILY_OUTPUT: egui::Color32 = egui::Color32::from_rgb(0x3e, 0x44, 0x4d);

fn node_color(kind: &impl CanvasKind, _theme: &KlippTheme) -> egui::Color32 {
    match kind.palette() {
        Palette::Grade => FAMILY_PRIMARY_GRADE,
        Palette::Matte => FAMILY_SECONDARY_MATTE,
        Palette::Composite => FAMILY_COMPOSITE,
        Palette::Look => FAMILY_LOOK,
        Palette::Structural => FAMILY_STRUCTURAL,
        Palette::Source => FAMILY_SOURCE,
        Palette::Output => FAMILY_OUTPUT,
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
    /// The input this wire was lifted off, when the drag began on a
    /// wire's input end (#1744): dropped elsewhere it reconnects, in
    /// empty canvas it disconnects.
    detached: Option<(NodeId, u8)>,
}

/// A wire, by its two ends: `(from, from_port, to, to_port)`.
type WireRef = (NodeId, u8, NodeId, u8);

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

/// Sticky-note drag state. Same emitted-delta pattern as DragNode +
/// DragFrame so streaming MoveStickyNote per tick doesn't drift
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
/// What a node did on the frame shown, when it isn't what was asked
/// (#1747): the canvas marks it with a badge and gives `reason` on hover.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeFeedback {
    pub kind: FeedbackKind,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackKind {
    /// It failed: its picture is not the one asked for.
    Failed,
    /// It didn't run as asked (a plug-in missing, a frame it can't take)
    /// and the picture went on without it, or with what it had.
    Declined,
    /// It ran, but its preview couldn't be made.
    NoPreview,
}

impl FeedbackKind {
    fn glyph(self) -> &'static str {
        match self { Self::Failed => "\u{2715}", Self::Declined => "!", Self::NoPreview => "\u{2013}" }
    }
    fn word(self) -> &'static str {
        match self { Self::Failed => "Failed", Self::Declined => "Declined", Self::NoPreview => "No preview" }
    }
}

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
    /// A node being renamed (#893): the node, where its editor sits, and
    /// the name as typed. Opened from its menu or a double-click on it.
    node_rename_editor: Option<(NodeId, egui::Pos2, String)>,
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
    /// The pass a node, note or frame menu was opened on: the right
    /// click that opened it is not a click outside it (#1743; the note
    /// and frame menus closed as they opened without it).
    node_menu_opened_frame: u64,
    /// Cleared on first paint of a new clip (or when `view_dirty`
    /// is set explicitly). Triggers a one-shot fit-to-view so the
    /// graph appears framed on first open instead of off-screen.
    /// Without this the user has to pan around hunting for nodes
    /// because the default `NodePos { x: i*200, y: 0 }` layout
    /// extends to graph x=2400+ for the default 13-node graph,
    /// far beyond the canvas's visible window at zoom=1.
    last_fit_clip: Option<ClipId>,
    last_picture_level: Option<klipp_graph_format::path::NodePath>,
    picture_move_root: Option<klipp_graph_format::path::Root>,
    /// The grade canvas's open node-drag gesture, by the clip it names
    /// (#1528).
    grade_move_root: Option<klipp_graph_format::path::Root>,
    /// Set by the toolbar's "Fit" button or programmatically
    /// (clip switch). Consumed at the top of the next `show()`.
    fit_pending: bool,
    /// The pan, zoom and canvas size the last fit left. A canvas that
    /// changes size under an untouched fit (the dock moved from below
    /// the viewers to their right) is fitted again; framed for the old
    /// size, most of the graph sat off the canvas.
    fitted_view: Option<(egui::Vec2, f32, egui::Vec2)>,
    /// A refit asked for by an edit, taken only while the view is still
    /// the one framed ([`Self::refit_if_untouched`]).
    refit_untouched: bool,
    /// How many times an edit's refit has framed the view: for the tests.
    #[cfg(test)]
    edit_refits: usize,
    /// Per-ExternalMatte-node thumbnail cache. Keyed by node id so
    /// two ExternalMatte nodes with different images don't collide;
    /// the stored `PathBuf` lets us invalidate when the user picks a
    /// new image. The `TextureHandle` is uploaded via
    /// `ctx.load_texture` and dropped from egui's pool on panel
    /// destruction (or when the entry is overwritten on path change).
    matte_thumbs: std::collections::HashMap<NodeId, (std::path::PathBuf, egui::TextureHandle)>,
    /// Per-node output-thumbnail textures. Keyed by full output address; value is
    /// `(node_thumbs_gen at upload time, texture handle)`. Re-uploads
    /// when the engine bumps `FrameBuffer::node_thumbs_gen`; otherwise
    /// reuses the cached texture.
    output_thumbs: std::collections::HashMap<NodeOutput, (u64, egui::TextureHandle)>,
    /// The output addresses we last told the engine to capture thumbs
    /// for, so we can avoid re-sending `SetVisibleNodeThumbs` every
    /// frame. Snapshotted at end of each render; compared against the
    /// current frame's visible set on the next render.
    last_visible_thumb_nodes: Vec<NodeOutput>,
    pub(crate) thumbnail_quality: Option<klipp_engine::node_thumbnails::PreviewQuality>,
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
    thumb_clipping_overlays: std::collections::HashMap<NodeOutput, (u64, egui::TextureHandle)>,
    /// Cached 64-bucket luma histograms keyed by thumb generation.
    /// Cheap to recompute (~20 K samples per node) but caching
    /// avoids the work for nodes we paint every frame anyway.
    thumb_histograms: std::collections::HashMap<NodeOutput, (u64, [u32; 64])>,
    /// Whether the grade canvas takes its single keys (Delete, F, L, M,
    /// the digits): the colour page sets it while the canvas is the
    /// focused panel (#1742). On by default, as a canvas drawn alone has
    /// them.
    pub grade_keys: bool,
    /// The node soloed in the engine, for the picture canvas to mark
    /// with its halo, chip and Clear solo (#1743); the app sets it before
    /// each draw, as it hands the grade canvas its solo.
    pub picture_solo: Option<NodeId>,
    /// The node whose input and output the viewer wipes between (#2758),
    /// for the node menu's A/B item; the app sets it before each draw.
    pub compared_node: Option<NodeId>,
    /// Group the picture canvas's add menu for the Composite page
    /// (#2758); the app sets it before each draw.
    pub composite_menu: bool,
    /// The node the add menu replaces when it is open for "Replace
    /// with…" (#1745): a pick swaps that node rather than adding one.
    replace_target: Option<NodeId>,
    /// Whether a node drag lands on the grid (#1745), toggled in the
    /// toolbar.
    pub snap_to_grid: bool,
    /// The add menu's recent picks changed since the app last kept them
    /// (#1745): it reads [`Self::recent_kinds`] and clears this.
    recents_changed: bool,
    /// The wire menu (#1744): the wire right-clicked, where the menu
    /// sits, and that point on the canvas.
    wire_menu: Option<(WireRef, egui::Pos2, egui::Pos2)>,
    /// The wire the add menu's pick goes into (#1744): opened from the
    /// wire menu, or with Tab over a wire.
    insert_wire: Option<WireRef>,
    /// The output the add menu's pick is fed from (#1744): a wire
    /// dragged from it into empty canvas.
    wire_from: Option<(NodeId, u8)>,
    /// The wire a lone dragged node is over, which it goes into when let
    /// go (#1744).
    drop_wire: Option<WireRef>,
    /// How the nodes on the canvas fared on the frame shown (#1747), by
    /// node: the app sets it before each draw.
    pub node_feedback: std::collections::HashMap<NodeId, NodeFeedback>,
    /// The colour labels of the nodes drawn (#1753), by the path the
    /// canvas names each node by, 1 to 6: the app sets it before each
    /// draw.
    pub node_labels: std::collections::HashMap<klipp_graph_format::path::NodePath, u8>,
}

/// The row of label swatches in a node's menu, None first, `current`
/// ringed: the colour picked.
fn label_swatches(ui: &mut egui::Ui, current: u8) -> Option<u8> {
    let mut picked = None;
    ui.horizontal(|ui| {
        // Inside the menu's width, whatever the spacing.
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_space(6.0);
        ui.label("Label");
        ui.add_space(4.0);
        for label in klipp_media::ColorLabel::ALL {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
            let centre = rect.center();
            let painter = ui.painter();
            let weak = ui.visuals().weak_text_color();
            match node_label_colour(label.to_u8()) {
                Some(colour) => {
                    painter.circle_filled(centre, 5.5, colour);
                }
                None => {
                    painter.circle_stroke(centre, 5.0, egui::Stroke::new(1.0_f32, weak));
                    painter.line_segment([centre + egui::vec2(-3.5, 3.5), centre + egui::vec2(3.5, -3.5)], egui::Stroke::new(1.0_f32, weak));
                }
            }
            if current == label.to_u8() {
                painter.circle_stroke(centre, 8.0, egui::Stroke::new(1.5_f32, ui.visuals().strong_text_color()));
            } else if response.hovered() {
                painter.circle_stroke(centre, 8.0, egui::Stroke::new(1.0_f32, weak));
            }
            if response.on_hover_text(label.label()).clicked() {
                picked = Some(label.to_u8());
            }
        }
    });
    picked
}

/// The colour a node's label `colour` is drawn in, the clip label
/// palette's; `None` for no label.
pub(crate) fn node_label_colour(colour: u8) -> Option<egui::Color32> {
    let label = klipp_media::ColorLabel::from_u8(colour);
    (label.to_u8() == colour).then(|| label.rgb()).flatten().map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
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
            node_rename_editor: None,
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
            last_picture_level: None,
            picture_move_root: None,
            grade_move_root: None,
            fit_pending: false,
            fitted_view: None,
            refit_untouched: false,
            #[cfg(test)]
            edit_refits: 0,
            matte_thumbs: std::collections::HashMap::new(),
            output_thumbs: std::collections::HashMap::new(),
            last_visible_thumb_nodes: Vec::new(),
            thumbnail_quality: None,
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
            grade_keys: true,
            picture_solo: None,
            compared_node: None,
            composite_menu: false,
            replace_target: None,
            snap_to_grid: false,
            recents_changed: false,
            wire_menu: None,
            insert_wire: None,
            wire_from: None,
            drop_wire: None,
            node_feedback: Default::default(),
            node_labels: Default::default(),
        }
    }
}

impl NodeGraphV2Panel {
    pub(crate) fn invalidate_thumbnail_requests(&mut self) { self.last_visible_thumb_nodes.clear(); }

    /// Reframe the graph after moving its panel to a different dock.
    pub fn request_fit(&mut self) { self.fit_pending = true; }

    /// Reframe the graph after an edit that may have put nodes outside
    /// the view (a batch of graph ops, #1905), on the next frame, unless
    /// the view has been moved since it was last framed: a view the
    /// person moved stays where they put it. Asked any number of times
    /// before that frame, it frames once.
    pub fn refit_if_untouched(&mut self) { self.refit_untouched = true; }

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
                format!("Bookmark {} empty (Shift+{} to set, Alt+{} to jump)", slot + 1, slot + 1, slot + 1),
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
        engine_node_thumbs: &std::collections::HashMap<NodeOutput, Vec<u8>>,
        engine_node_thumbs_gen: u64,
        deferred_drops: &mut Vec<egui::TextureHandle>,
        descent_path: &mut Vec<NodeId>,
        breadcrumb_labels: &[String],
        // `None` for a graph that keeps no versions: the timeline grade.
        grade_version_names: Option<&[String]>,
        active_grade_version: Option<usize>,
        clip_time_secs: f64,
        port_snap_enabled: bool,
        port_snap_radius_px: f32,
        solo_node: Option<NodeId>,
    ) -> Vec<AppCommand> {
        if self.last_picture_level.take().is_some() {
            self.fit_pending = true;
            self.last_visible_thumb_nodes.clear();
        }
        let mut cmds = Vec::new();
        if let Some(root) = self.picture_move_root.take() {
            self.drag_node = None;
            self.drag_frame = None;
            self.drag_sticky = None;
            cmds.push(AppCommand::EndNodeLayoutEdit { root });
        }
        let previous_path = descent_path.clone();
        // A node drag is one layout gesture, named by the clip the grade is
        // edited through (the app takes a sequence grade's stand-in to its
        // root): begun with its first move, ended when the drag ends, and
        // abandoned by Escape or another clip (#1528).
        let gesture = klipp_graph_format::path::Root::Clip { clip: clip_id.0 };
        if let Some(root) = self.grade_move_root.filter(|r| *r != gesture || ui.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.grade_move_root = None;
            self.drag_node = None;
            self.drag_frame = None;
            self.drag_sticky = None;
            cmds.push(AppCommand::AbandonNodeLayoutEdit { root });
        }
        let keys = self.grade_keys;
        let updates = self.show_canvas(ui, selected_node, selected_set, &GradeCanvas(graph, keys), clip_id, theme,
            engine_node_thumbs, engine_node_thumbs_gen, deferred_drops, descent_path,
            breadcrumb_labels, grade_version_names, active_grade_version, clip_time_secs,
            port_snap_enabled, port_snap_radius_px, solo_node);
        // A note's drag is the gesture's too, one step (#1745).
        if self.grade_move_root.is_none() && updates.iter().any(|cmd| matches!(cmd, AppCommand::MoveNode { .. } | AppCommand::MoveStickyNote { .. })) {
            self.grade_move_root = Some(gesture);
            cmds.push(AppCommand::BeginNodeLayoutEdit { root: gesture });
        }
        cmds.extend(updates);
        if self.drag_node.is_none() && self.drag_frame.is_none() && self.drag_sticky.is_none() {
            if let Some(root) = self.grade_move_root.take() {
                cmds.push(AppCommand::EndNodeLayoutEdit { root });
            }
        }
        if *descent_path != previous_path { self.fit_pending = true; }
        cmds
    }

    /// Draw the clip's actual picture graph at `level`. Navigation stays
    /// in the caller's UI state; edits carry complete node and port paths.
    /// Picture thumbnails are supplied by the later inspection UI step.
    pub fn show_picture(
        &mut self,
        ui: &mut egui::Ui,
        clip: &klipp_timeline::Clip,
        level: &mut klipp_graph_format::path::NodePath,
        selected_node: &mut Option<NodeId>,
        selected_set: &mut std::collections::HashSet<NodeId>,
        theme: &KlippTheme,
        clip_time_secs: f64,
        keyboard_enabled: bool,
        engine_node_thumbs: &std::collections::HashMap<NodeOutput, Vec<u8>>,
        engine_node_thumbs_gen: u64,
        deferred_drops: &mut Vec<egui::TextureHandle>,
    ) -> Result<Vec<AppCommand>, String> {
        if level.root_clip() != Some(clip.id.0) {
            return Err("The graph belongs to a different clip".into());
        }
        let lifted = clip.picture_v2().map_err(|e| e.to_string())?;
        let mut cmds = Vec::new();
        if self.last_picture_level.as_ref() != Some(level) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.drag_node = None;
            self.drag_frame = None;
            self.drag_sticky = None;
            if let Some(root) = self.picture_move_root.take() { cmds.push(AppCommand::AbandonNodeLayoutEdit { root }); }
        }
        if self.last_picture_level.as_ref() != Some(level) {
            self.fit_pending = true;
            self.last_visible_thumb_nodes.clear();
            self.last_picture_level = Some(level.clone());
        }
        let mut graph = picture::PictureCanvas::new(&lifted.picture.graph, level.clone(), *selected_node)?;
        graph.composite_menu = self.composite_menu;
        graph.keyboard_enabled = keyboard_enabled;
        let mut labels = Vec::new();
        let mut body = &lifted.picture.graph;
        for id in &level.nodes {
            let n = body.nodes.iter().find(|n| n.id == *id).ok_or("The selected group is no longer available")?;
            labels.push(if n.label.is_empty() { "Group".to_owned() } else { n.label.clone() });
            body = &n.group.as_ref().ok_or("The selected node is not a group")?.graph;
        }
        let updates = self.show_canvas(ui, selected_node, selected_set, &graph, clip.id, theme,
            engine_node_thumbs, engine_node_thumbs_gen, deferred_drops, &mut level.nodes, &labels, None, None,
            clip_time_secs, false, 0.0, self.picture_solo);
        // A batch the picture's rules refuse (a wire its chain can't take,
        // a node where none can go) says why here, where it was asked
        // for, and is not sent (#1747): the app's toast said it far away.
        let mut updates = updates;
        updates.retain(|cmd| {
            let AppCommand::GraphOps { ops, label } = cmd else { return true };
            match clip.clone().apply_picture_ops(ops) {
                Ok(_) => true,
                Err(why) => {
                    self.bookmark_toast = Some((format!("{label}: {why}"), std::time::Instant::now()));
                    false
                }
            }
        });
        // A click without movement must not create an undo entry. Begin
        // only when the first position update is about to be dispatched:
        // a node's, or a note's (#1743).
        let moves = |cmd: &AppCommand| matches!(cmd, AppCommand::SetNodePositionAt { .. })
            || matches!(cmd, AppCommand::SetAnnotationsAt { label, .. } if *label == picture::MOVE_NOTE);
        if self.picture_move_root.is_none() && updates.iter().any(moves) {
            self.picture_move_root = Some(level.root);
            cmds.push(AppCommand::BeginNodeLayoutEdit { root: level.root });
        }
        cmds.extend(updates);
        if self.drag_node.is_none() && self.drag_frame.is_none() && self.drag_sticky.is_none() {
            if let Some(root) = self.picture_move_root.take() { cmds.push(AppCommand::EndNodeLayoutEdit { root }); }
        }
        Ok(cmds)
    }

    fn show_canvas<G: CanvasGraph>(
        &mut self,
        ui: &mut egui::Ui,
        selected_node: &mut Option<NodeId>,
        selected_set: &mut std::collections::HashSet<NodeId>,
        graph: &G,
        clip_id: ClipId,
        theme: &KlippTheme,
        engine_node_thumbs: &std::collections::HashMap<NodeOutput, Vec<u8>>,
        engine_node_thumbs_gen: u64,
        deferred_drops: &mut Vec<egui::TextureHandle>,
        descent_path: &mut Vec<NodeId>,
        breadcrumb_labels: &[String],
        // `None` for a graph that keeps no versions: the timeline grade.
        grade_version_names: Option<&[String]>,
        active_grade_version: Option<usize>,
        clip_time_secs: f64,
        port_snap_enabled: bool,
        port_snap_radius_px: f32,
        solo_node: Option<NodeId>,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        let thumbnail_descent = descent_path.clone();
        // Self-heal: drop any stale ids from the selection set that
        // no longer exist in the current clip's graph (clip-switch,
        // upstream RemoveNode the engine has already applied, etc.).
        // Keeps the set bounded + makes downstream `set.contains(id)`
        // checks trustworthy without forcing every caller to clear it
        // on every transition.
        if !selected_set.is_empty() {
            selected_set.retain(|id| graph.nodes().iter().any(|n| n.id() == *id));
            if let Some(p) = *selected_node {
                if !graph.nodes().iter().any(|n| n.id() == p) {
                    *selected_node = selected_set.iter().next().copied();
                }
            }
        }

        // Toolbar — small strip at the top with view controls.
        // A clip's grade is a group within its picture. Timeline grades
        // have no clip root (and no version selector), so cannot leave it.
        let avail = ui.available_size();
        let clip_grade = graph.grade_tools() && grade_version_names.is_some();
        if clip_grade || !graph.grade_tools() || !descent_path.is_empty() {
            let mut navigated = false;
            egui::Frame::none().fill(theme.bg_panel).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0);
                    if clip_grade {
                        if ui.link(egui::RichText::new("Clip").small().color(theme.accent)).clicked() {
                            cmds.push(AppCommand::SetGraphDockRoot { root: klipp_graph_format::path::Root::Clip { clip: clip_id.0 } });
                            navigated = true;
                        }
                        ui.label(egui::RichText::new(" › ").small().color(theme.text_dim));
                    }
                    let root_label = if graph.grade_tools() { "Grade" } else { "Clip" };
                    if descent_path.is_empty() {
                        ui.label(egui::RichText::new(root_label).small().color(theme.text_primary));
                    } else if ui.link(egui::RichText::new(root_label).small().color(theme.accent)).clicked() {
                        descent_path.clear();
                        navigated = true;
                    }
                    let depth = descent_path.len();
                    for (i, label) in breadcrumb_labels.iter().take(depth).enumerate() {
                        ui.label(egui::RichText::new(" › ").small().color(theme.text_dim));
                        if i + 1 == depth {
                            ui.label(egui::RichText::new(label).small().color(theme.text_primary));
                        } else if ui.link(egui::RichText::new(label).small().color(theme.accent)).clicked() {
                            descent_path.truncate(i + 1);
                            navigated = true;
                        }
                    }
                });
            });
            if navigated {
                selected_set.clear();
                *selected_node = None;
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
                .layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true)),
        );
        toolbar_ui.add_space(6.0);
        // ── Group 1: view controls ──
        // Cheap-to-undo navigation + zoom indicator. No mutation
        // happens here, so plain buttons are fine — these set how
        // you LOOK at the graph, not what's in it.
        let fit_clicked = toolbar_ui.button("Fit").on_hover_text(
            "Frame all nodes in the canvas (shortcut: F)"
        ).clicked();
        let reset_clicked = toolbar_ui.button("1:1").on_hover_text(
            "Reset zoom to 100 % and centre on the graph origin"
        ).clicked();
        toolbar_ui.label(
            egui::RichText::new(format!("zoom {:.0} %", self.zoom * 100.0))
                .small().color(theme.text_dim)
        );

        toolbar_ui.add_space(6.0);
        toolbar_ui.separator();
        toolbar_ui.add_space(6.0);

        // ── Group 2: layout ──
        // Auto-layout rewrites every node's position. Distinct
        // accent-coloured stroke so it reads as "tool that changes
        // the graph" against the plain view buttons.
        let mutating_button = |label: egui::RichText| -> egui::Button<'static> {
            egui::Button::new(label)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(
                        theme.accent.r(), theme.accent.g(), theme.accent.b(), 110,
                    ),
                ))
        };
        if toolbar_ui.add(mutating_button(egui::RichText::new("Auto-layout")))
            .on_hover_text(
                "Recompute every node's position via hierarchical layered \
                 layout (shortcut: L)\nUndoable with Ctrl+Z."
            )
            .clicked()
        {
            cmds.push(AppCommand::AutoLayoutNodeGraph { clip_id });
        }
        if toolbar_ui.selectable_label(self.snap_to_grid, "Snap")
            .on_hover_text("Node drags land on the grid")
            .clicked()
        {
            self.snap_to_grid = !self.snap_to_grid;
        }

        toolbar_ui.add_space(6.0);
        toolbar_ui.separator();
        toolbar_ui.add_space(6.0);

        // ── Group 3: annotate ──
        // Sticky-note creation at the canvas viewport centre.
        if toolbar_ui.add(mutating_button(egui::RichText::new("\u{1F4DD} Note")))
            .on_hover_text("Add a sticky note at the canvas centre")
            .clicked()
        {
            // canvas-centre → graph: screen = origin + (graph + pan)
            // * zoom. canvas-centre maps to origin, so graph = -pan.
            cmds.push(AppCommand::CreateStickyNote {
                clip_id,
                x: -self.pan.x,
                y: -self.pan.y,
            });
        }

        toolbar_ui.add_space(6.0);
        toolbar_ui.separator();
        toolbar_ui.add_space(6.0);

        // ── Group 4: search ──
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

        toolbar_ui.add_space(6.0);
        toolbar_ui.separator();
        toolbar_ui.add_space(6.0);

        // ── Group 5: overlays ──
        // Toggleable display layers — minimap, bookmarks discovery,
        // per-thumb diagnostics. None of these mutate the graph;
        // they only change how it's drawn.
        if toolbar_ui.selectable_label(
            self.minimap_visible,
            egui::RichText::new("\u{25A6} Map").small(),
        ).on_hover_text("Toggle minimap (M)\nClick + drag the minimap to pan").clicked() {
            self.minimap_visible = !self.minimap_visible;
        }
        let used: Vec<u8> = self.view_bookmarks.iter().enumerate()
            .filter(|&(_i, b)| b.is_some()).map(|(i, _b)| (i + 1) as u8)
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
        toolbar_ui.add_space(6.0);
        toolbar_ui.separator();
        toolbar_ui.add_space(6.0);

        // ── Group 6: versions / state ──
        // The version menu sits closest to where you'd save a snap-
        // shot of "the grade I just dialled in"; the SOLO chip
        // appears next to it only when active.
        // Grade-version picker. Shows "V: none" when no versions
        // exist, "V: V1 ▾" when one is active, with a menu listing
        // every saved version plus save / update / rename / delete
        // actions. Clicking a version triggers SwitchGradeVersion;
        // "Save as new version" picks the next auto-name (V1 / V2 /
        // ...). Rename / delete act on the currently-active version.
        if graph.grade_tools() {
            let keeps_versions = grade_version_names.is_some();
            let grade_version_names = grade_version_names.unwrap_or(&[]);
            let active_name = active_grade_version
                .and_then(|i| grade_version_names.get(i).map(|s| s.as_str()));
            // Plain-language labels — the previous "V: +" / "V: —" /
            // "V: V1 ▾" was opaque to anyone who didn't already know
            // it was a version picker. Now: the verb tells you what
            // clicking does (save / browse / switch).
            let (menu_label, menu_tooltip) = match active_name {
                Some(n) => (
                    format!("Version: {n}  \u{25BE}"),
                    format!(
                        "Currently viewing version {n}.\nClick to switch, \
                         save, update, or delete versions."
                    ),
                ),
                None if grade_version_names.is_empty() => (
                    "+ Save version  \u{25BE}".to_string(),
                    "Save a snapshot of the current grade so you can come \
                     back to it later or A/B against future edits."
                        .to_string(),
                ),
                None => (
                    "Versions  \u{25BE}".to_string(),
                    format!(
                        "{} saved version(s) — none active. Click to pick \
                         one or save a new snapshot.",
                        grade_version_names.len(),
                    ),
                ),
            };
            // Keep this widget directly in the wrapping row. A nested scope
            // measures against the remaining row width and can turn the menu
            // label into a tall, clipped column before the parent wraps it.
            let menu_resp = if keeps_versions { toolbar_ui.menu_rows(menu_label, |mb_ui| {
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
            }).response } else {
                toolbar_ui.add_enabled(false, egui::Button::new(menu_label))
            };
            if keeps_versions {
                menu_resp.on_hover_text(menu_tooltip);
            } else {
                menu_resp.on_disabled_hover_text("Versions are kept per clip; the timeline grade has none. Save it as a Look to keep a copy.");
            }
            toolbar_ui.add_space(8.0);
        }
        // Solo indicator — only visible when a node is currently
        // soloed. Shows the soloed node's label (so the user knows
        // which node they're previewing without scanning the canvas
        // for the yellow halo) plus an inline × button to clear.
        // Background uses the same yellow as the on-canvas halo so
        // the link reads visually.
        if let Some(solo_id) = solo_node {
            let label = graph.nodes().iter()
                .find(|n| n.id() == solo_id)
                .map(|n| n.label().to_owned())
                .unwrap_or_else(|| "?".to_string());
            let solo_color = egui::Color32::from_rgb(0xff, 0xd5, 0x3a);
            let bg = egui::Color32::from_rgba_unmultiplied(
                solo_color.r(), solo_color.g(), solo_color.b(), 40,
            );
            egui::Frame::none()
                .fill(bg)
                .stroke(egui::Stroke::new(0.5_f32, solo_color))
                .rounding(2.0)
                .inner_margin(egui::Margin::symmetric(4.0, 1.0))
                .show(&mut toolbar_ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("Viewing: {label}"))
                            .small()
                            .color(theme.text_primary),
                    );
                    if ui.small_button("\u{2715}")
                        .on_hover_text("Back to the finished picture (1)")
                        .clicked()
                    {
                        cmds.push(AppCommand::SetNodeSolo { at: None });
                    }
                });
            toolbar_ui.add_space(8.0);
        }
        // (zoom % moved to the view-controls group at the start
        // of the toolbar; the reset_clicked branch below still
        // needs to mutate state.)
        if reset_clicked {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }

        // Side docks can be narrow. Let the toolbar wrap, then reserve
        // its actual height so every control remains above the canvas.
        ui.add_space((toolbar_ui.min_rect().height() - toolbar_h).max(0.0));
        let canvas_h = ui.available_height().max(0.0);
        if canvas_h < 1.0 { return cmds; }
        let (canvas_rect, canvas_resp) = ui.allocate_exact_size(
            egui::vec2(avail.x, canvas_h),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter_at(canvas_rect);

        // First-paint auto-fit per clip + on-demand fit via toolbar
        // or `F` shortcut. Done before any rendering so the first
        // frame is already framed instead of needing a second pass.
        let f_pressed = graph.keyboard_enabled() && ui.input(|i| {
            i.key_pressed(egui::Key::F)
                && !i.modifiers.ctrl
                && !i.modifiers.command
        });
        let resized_fit = self.fitted_view.is_some_and(|(pan, zoom, size)| {
            pan == self.pan && zoom == self.zoom && (size - canvas_rect.size()).length() > 2.0
        });
        let untouched = self.fitted_view.is_some_and(|(pan, zoom, _)| pan == self.pan && zoom == self.zoom);
        let edit_refit = std::mem::take(&mut self.refit_untouched) && untouched;
        #[cfg(test)]
        if edit_refit {
            self.edit_refits += 1;
        }
        let need_fit = fit_clicked
            || f_pressed
            || self.fit_pending
            || resized_fit
            || edit_refit
            || self.last_fit_clip != Some(clip_id);
        if need_fit && !graph.nodes().is_empty() {
            self.fit_to_nodes(graph, canvas_rect);
            self.fit_pending = false;
            self.last_fit_clip = Some(clip_id);
            self.fitted_view = Some((self.pan, self.zoom, canvas_rect.size()));
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
            || self.node_rename_editor.is_some()
            || self.frame_description_editor.is_some()
            || self.wireless_label_editor.is_some()
            || self.sticky_menu.is_some()
            || self.sticky_text_editor.is_some()
            || self.wire_menu.is_some()
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
        let mut node_rects: Vec<(NodeId, egui::Rect, egui::Color32, <G::Node as CanvasNode>::Kind<'_>, u8)> =
            Vec::with_capacity(graph.nodes().len());
        for node in graph.nodes() {
            let pos = graph.positions()
                .get(&node.id())
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
            let size = if node.kind().is_reroute() {
                egui::vec2(REROUTE_SIZE * zoom, REROUTE_SIZE * zoom)
            } else {
                egui::vec2(NODE_W * zoom, NODE_H * zoom)
            };
            let rect = egui::Rect::from_center_size(center, size);
            let in_count = node.input_count();
            node_rects.push((node.id(), rect, node_color(&node.kind(), theme), node.kind(), in_count));
        }

        // ── Search matches ────────────────────────────────────────
        // Pre-compute the set of match-node-ids so the per-node
        // paint loop and the Tab handler agree on what's matching
        // this frame. Empty query → empty set (no glow). Matches
        // against label + kind.display_name() so a renamed
        // "Sky Glow" node hits both as the user types either.
        let search_matches: Vec<NodeId> = if self.search_open && !self.search_query.is_empty() {
            let q = self.search_query.to_lowercase();
            graph.nodes().iter()
                .filter(|n| {
                    n.label().to_lowercase().contains(&q)
                        || n.kind().display_name().to_lowercase().contains(&q)
                })
                .map(|n| n.id())
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
            Vec::with_capacity(graph.frames().len());
        for frame in graph.frames() {
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
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(
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
                chip_rect, 2.0,
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
        )> = Vec::with_capacity(graph.sticky_notes().len());
        for note in graph.sticky_notes() {
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
        // A lone node being dragged is under the cursor: the wire it is
        // over (none of its own) is the one it drops into (#1744).
        let lone_drag = self.drag_node.as_ref().filter(|d| d.node_ids.len() == 1).map(|d| d.node_ids[0]);
        let edge_hover_eligible = self.drag_wire.is_none()
            && self.box_select.is_none()
            && cursor_in_canvas
            && (!cursor_in_node || lone_drag.is_some());
        if edge_hover_eligible {
            if let Some(c) = cursor {
                let threshold_sq = (8.0 * zoom).powi(2);
                let mut best_sq = threshold_sq;
                for (idx, edge) in graph.edges().iter().enumerate() {
                    if lone_drag.is_some_and(|n| edge.from_node == n || edge.to_node == n) { continue; }
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

        let wire_of = |idx: usize| graph.edges().get(idx).map(|e| (e.from_node, e.from_port, e.to_node, e.to_port));
        self.drop_wire = lone_drag.and(hovered_edge).and_then(wire_of);
        let hovered_wire = hovered_edge.and_then(wire_of);
        let lifted = self.drag_wire.and_then(|d| d.detached);
        for (idx, edge) in graph.edges().iter().enumerate() {
            // The wire whose end is being dragged is drawn as the drag.
            if lifted == Some((edge.to_node, edge.to_port)) { continue; }
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
            let role = if to_kind.is_reroute() || dest_role == klipp_nodes::PortRole::Primary {
                graph.signal_role(edge.from_node, edge.from_port)
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
                        1.0_f32,
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
                let role = if from_kind.is_reroute() {
                    graph.signal_role(dw.from_node, dw.from_port)
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
                        egui::Stroke::new(2.0_f32, ring_color),
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
                egui::Stroke::new(1.0_f32, theme.accent),
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
            let border_w = if self.selected_sticky == Some(*sticky_id) { 1.6_f32 } else { 0.8_f32 };
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
            let node = &graph.nodes()[idx];
            let node_on = node.effective_enabled(clip_time_secs);
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
            if kind.is_reroute() {
                let centre = rect.center();
                let radius = rect.width() * 0.40;
                if glow_alpha > 0 {
                    let accent = theme.accent;
                    painter.circle_stroke(
                        centre,
                        radius + 3.0,
                        egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(
                            accent.r(), accent.g(), accent.b(), glow_alpha,
                        )),
                    );
                }
                if match_alpha > 0 {
                    let cyan = SEARCH_MATCH_COLOR;
                    painter.circle_stroke(
                        centre,
                        radius + 5.0,
                        egui::Stroke::new(1.8_f32, egui::Color32::from_rgba_unmultiplied(
                            cyan.r(), cyan.g(), cyan.b(), match_alpha,
                        )),
                    );
                }
                if is_selected {
                    let halo = theme.accent;
                    painter.circle_stroke(
                        centre,
                        radius + 4.0,
                        egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(
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
                    rounding: egui::Rounding::same(4.0),
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
            if kind.is_group() {
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
                painter.rect_stroke(ghost2, 4.0, egui::Stroke::new(0.5_f32, theme.border));
                painter.rect_filled(ghost1, 4.0, shade(fill, 35));
                painter.rect_stroke(ghost1, 4.0, egui::Stroke::new(0.5_f32, theme.border));
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
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(
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
                    egui::Stroke::new(1.8_f32, egui::Color32::from_rgba_unmultiplied(
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
            // Its colour label (#1753): the header tinted, and a band
            // of the colour along its top edge.
            let label = if self.node_labels.is_empty() { None } else {
                graph.thumbnail_output(clip_id, node_id, &thumbnail_descent).and_then(|at| self.node_labels.get(&at.path).copied()).and_then(node_label_colour)
            };
            if let Some(colour) = label {
                painter.rect_filled(header_rect, 4.0, colour.gamma_multiply(0.3));
                let band = egui::Rect::from_min_max(header_rect.min, egui::pos2(header_rect.max.x, header_rect.min.y + (3.0 * zoom).max(2.0)));
                painter.rect_filled(band, egui::Rounding { nw: 4.0, ne: 4.0, sw: 0.0, se: 0.0 }, colour);
            }
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
                    egui::Stroke::new(1.0_f32, dark_overlay),
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
                        egui::Stroke::new(1.0_f32, shadow),
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
                        egui::Stroke::new(1.0_f32, highlight),
                    );
                }
            }

            let is_soloed = solo_node == Some(node_id);
            let eye_hovered = hover_pointer.is_some_and(|p| rect.expand(4.0).contains(p));

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
                    egui::Stroke::new(1.5_f32, halo_a1),
                );
                painter.rect_stroke(
                    rect.expand(4.0),
                    6.0,
                    egui::Stroke::new(1.5_f32, halo_a2),
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
                    egui::Stroke::new(1.8_f32, egui::Color32::from_rgba_unmultiplied(
                        solo_color.r(), solo_color.g(), solo_color.b(), 200,
                    )),
                );
                painter.rect_stroke(
                    rect.expand(4.0),
                    6.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgba_unmultiplied(
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
                node.label(),
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
            // Skipped only at extreme zoom-out where they'd be
            // unreadable specks; the gate is at 0.35 so they show
            // on most graphs. Painted right-to-left so the right-
            // most indicator (modified) sits closest to the edge —
            // it's the most common state for a working colorist
            // graph and benefits from being the first thing you
            // see when scanning the header.
            if zoom >= 0.35 {
                let cy = rect.min.y + header_h * 0.5;
                let dot_r = (3.5 * zoom).max(4.0);
                let glyph_size = (11.0 * zoom).max(12.0);
                let pitch = 11.0 * zoom.max(0.6);
                let mut x = rect.max.x - 7.0 * zoom.max(0.6);

                // Slot 1 (rightmost) — modified state. Lights up
                // for any node that's been edited away from its
                // default identity values. Without this, a default
                // Corrector (or Exposure / Saturation / etc.)
                // shows nothing — and a working colorist's graph
                // is mostly modified nodes, so the most useful
                // single state indicator is "this node is doing
                // something." Skipped for kinds where "default" is
                // structural (Source / Output / Reroute / Compound)
                // since those don't have grade params to begin with.
                let is_grade_kind = !kind.is_terminal() && !kind.is_reroute() && !kind.is_group();
                if is_grade_kind && node.modified() {
                    let green = egui::Color32::from_rgb(0x68, 0xc8, 0x6a);
                    painter.circle_filled(egui::pos2(x, cy), dot_r, green);
                    x -= pitch;
                }
                // Slot 2 — keyframe dot.
                if node.animated() {
                    let cyan = egui::Color32::from_rgb(0x6a, 0xc8, 0xe0);
                    painter.circle_filled(egui::pos2(x, cy), dot_r, cyan);
                    x -= pitch;
                }
                // Slot 3 — bypass slash. effective_enabled already
                // accounts for the ENABLED_TRACK keyframe state, so
                // a node that's been keyframe-bypassed at the
                // current time also lights up.
                if !node_on {
                    let red = egui::Color32::from_rgb(0xd0, 0x6a, 0x6a);
                    painter.text(
                        egui::pos2(x, cy),
                        egui::Align2::CENTER_CENTER,
                        "\u{00F8}", // ø
                        egui::FontId::proportional(glyph_size),
                        red,
                    );
                    x -= pitch;
                }
                // Slot 4 — matte producer.
                if matches!(
                    kind.output_port_role(0),
                    klipp_nodes::PortRole::Matte,
                ) {
                    let magenta = egui::Color32::from_rgb(0xc8, 0x4a, 0xa8);
                    painter.text(
                        egui::pos2(x, cy),
                        egui::Align2::CENTER_CENTER,
                        "\u{25C7}", // ◇ — diamond (matte symbol family)
                        egui::FontId::proportional(glyph_size),
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
                painter.rect_filled(chip_rect, 2.0, solo_color);
                painter.rect_stroke(chip_rect, 2.0, egui::Stroke::new(0.5_f32, theme.bg_deep));
                painter.text(
                    chip_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "S",
                    egui::FontId::proportional(10.0 * zoom),
                    theme.bg_deep,
                );
            }

            // Compound body: overlay "N nodes" + a hint that double-
            // click descends. A compound has no thumbnail of its own
            // (the thumbnail capture doesn't render sub-graphs), so the
            // count is the cue that the node contains something.
            if kind.is_group() && node_on {
                let n = node.child_count();
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
                if let Some(label) = node.output_link() {
                    let text = format!("⚲ {label}");
                    let galley = painter.layout_no_wrap(text, chip_font.clone(), chip_fg);
                    let pad = egui::vec2(6.0, 3.0) * zoom;
                    let chip_size = galley.size() + pad * 2.0;
                    let chip_pos = egui::pos2(
                        rect.max.x - chip_size.x - 4.0 * zoom,
                        rect.min.y - chip_size.y - 2.0 * zoom,
                    );
                    let chip_rect = egui::Rect::from_min_size(chip_pos, chip_size);
                    painter.rect_filled(chip_rect, 2.0, chip_bg);
                    painter.rect_stroke(chip_rect, 2.0, egui::Stroke::new(0.5_f32, chip_fg));
                    painter.galley(chip_rect.min + pad, galley, chip_fg);
                }
                for (port, slot) in node.input_links().iter().enumerate() {
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
                        painter.rect_filled(chip_rect, 2.0, chip_bg);
                        painter.rect_stroke(chip_rect, 2.0, egui::Stroke::new(0.5_f32, chip_fg));
                        painter.galley(chip_rect.min + pad, galley, chip_fg);
                    }
                }
            }

            // ExternalMatte: paint the loaded image as a thumbnail
            // inside the node body so the user sees which mask is
            // attached without having to click through the inspector.
            if kind.is_external_matte() {
                if let Some(path) = node.matte_image() {
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
                            self.matte_thumbs.insert(node_id, (path.to_owned(), tex));
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
            if !kind.is_external_matte() {
                // Output is a no-op in the executor (no entry in
                // node_outputs), so engine_node_thumbs.get(output_id)
                // is always None. Look up the feeder node instead so
                // Output paints the same thumbnail as the node
                // currently feeding it — i.e. the final composite.
                let thumb_lookup_id = if kind.is_output() {
                    graph.edges().iter()
                        .find(|e| e.to_node == node_id && e.to_port == 0)
                        .map(|e| e.from_node)
                        .unwrap_or(node_id)
                } else {
                    node_id
                };
                let thumb_output = graph.thumbnail_output(clip_id, thumb_lookup_id, &thumbnail_descent);
                if let Some((thumb_output, thumb_bytes)) = thumb_output.as_ref().and_then(|at| engine_node_thumbs.get(at).map(|bytes| (at, bytes))) {
                    if !thumb_bytes.is_empty() {
                        let needs_upload = match self.output_thumbs.get(thumb_output) {
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
                                if let Some((_, old)) = self.output_thumbs.remove(thumb_output) {
                                    deferred_drops.push(old);
                                }
                                self.output_thumbs.insert(
                                    thumb_output.clone(), (engine_node_thumbs_gen, tex),
                                );
                            }
                        }
                        if let Some((_, tex)) = self.output_thumbs.get(thumb_output) {
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
                                    .get(thumb_output).map(|(g, _)| *g);
                                if cached_gen != Some(engine_node_thumbs_gen) {
                                    let img = build_clipping_overlay_image(thumb_bytes);
                                    let name = format!("node_clip_overlay_{}", node_id.as_simple());
                                    let overlay_tex = ui.ctx().load_texture(
                                        name, img, egui::TextureOptions::NEAREST,
                                    );
                                    if let Some((_, old)) = self.thumb_clipping_overlays.remove(thumb_output) {
                                        deferred_drops.push(old);
                                    }
                                    self.thumb_clipping_overlays.insert(
                                        thumb_output.clone(), (engine_node_thumbs_gen, overlay_tex),
                                    );
                                }
                                if let Some((_, ov_tex)) = self.thumb_clipping_overlays.get(thumb_output) {
                                    paint_thumb_rounded(&painter, thumb_rect, ov_tex.id(), 3.0);
                                }
                            }

                            // Mini histogram strip — 64-bucket luma
                            // histogram painted on the bottom 18 %
                            // of the thumb. Cached the same way.
                            if self.show_thumb_histogram {
                                let cached_gen = self.thumb_histograms
                                    .get(thumb_output).map(|(g, _)| *g);
                                if cached_gen != Some(engine_node_thumbs_gen) {
                                    let bins = compute_thumb_histogram(thumb_bytes);
                                    self.thumb_histograms.insert(
                                        thumb_output.clone(), (engine_node_thumbs_gen, bins),
                                    );
                                }
                                if let Some((_, bins)) = self.thumb_histograms.get(thumb_output) {
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
                    if let Some((_, old)) = thumb_output.as_ref().and_then(|at| self.output_thumbs.remove(at)) {
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

            // Input ports (left edge) — circle. Connected ports
            // paint filled with the role colour + a tiny dark inner
            // dot ("plugged in" — signal flows into the body); un-
            // connected ports paint hollow ("waiting for input").
            // Distinct from output ports by SHAPE (output is a
            // rounded square below) so the user can tell at a
            // glance which side of a port pair is the source vs
            // the sink, without relying on x-position alone.
            for p in 0..in_count {
                let pos = input_port_pos(&rect, p, in_count);
                let role = if kind.is_reroute() {
                    // Find the edge feeding this port and take the
                    // upstream's effective signal role; fall back to
                    // Primary when disconnected.
                    graph.edges().iter()
                        .find(|e| e.to_node == node_id && e.to_port == p)
                        .map(|e| graph.signal_role(e.from_node, e.from_port))
                        .unwrap_or(klipp_nodes::PortRole::Primary)
                } else {
                    kind.input_port_role(p)
                };
                let (rr, gg, bb) = role.editor_color();
                let role_color = egui::Color32::from_rgb(rr, gg, bb);
                let is_connected = graph.edges().iter()
                    .any(|e| e.to_node == node_id && e.to_port == p);
                let r = PORT_RADIUS * zoom;

                // Drag-time compatible-port highlight — when a wire
                // is in flight, every input on every OTHER node
                // paints a faint pulsing ring so the user sees at
                // a glance where the wire could land. The
                // snap-target gets its own brighter ring (handled
                // separately above) so the cursor's current
                // candidate stays unambiguous.
                let drag_eligible_target = self.drag_wire
                    .map(|dw| dw.from_node != node_id)
                    .unwrap_or(false);
                if drag_eligible_target {
                    let t = ui.input(|i| i.time) as f32;
                    let pulse = (t * 2.5).sin() * 0.5 + 0.5;
                    let alpha = (50.0 + 50.0 * pulse) as u8;
                    painter.circle_stroke(
                        pos, r * 1.9,
                        egui::Stroke::new(
                            1.2_f32,
                            egui::Color32::from_rgba_unmultiplied(rr, gg, bb, alpha),
                        ),
                    );
                    // Continuous repaint while dragging — the wire
                    // already redraws every frame anyway, so the
                    // pulse is free.
                    ui.ctx().request_repaint();
                }

                // Direct-hover affordance (only when no drag in
                // flight) — soft outer ring around the port the
                // cursor is currently near. Communicates "this is
                // grabbable" without waiting for the inline label
                // tooltip's hover-band gating.
                let port_hovered = !drag_eligible_target
                    && hover_pointer
                        .map(|hp| (hp - pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5))
                        .unwrap_or(false);
                if port_hovered {
                    painter.circle_stroke(
                        pos, r * 1.7,
                        egui::Stroke::new(
                            1.4_f32,
                            egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 160),
                        ),
                    );
                }

                if is_connected {
                    painter.circle_filled(pos, r, role_color);
                    // Inner dark dot — keeps the port readable as a
                    // socket rather than a flat colour blob, and
                    // mirrors the output port's "pinhole" so the
                    // two share the same socket-anatomy idiom.
                    painter.circle_filled(pos, r * 0.38, theme.bg_deep);
                } else {
                    painter.circle_filled(pos, r, theme.bg_deep);
                    painter.circle_stroke(
                        pos, r,
                        egui::Stroke::new(1.5_f32, role_color),
                    );
                }
                if cursor_near_node {
                    let port_rect = egui::Rect::from_center_size(
                        pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                    );
                    let resp = ui.allocate_rect(port_rect, egui::Sense::hover());
                    if resp.hovered() {
                        paint_port_label(
                            &painter,
                            pos,
                            kind.input_port_label(p),
                            PortLabelSide::Left,
                            zoom,
                            theme,
                        );
                    }
                }
            }
            // LayerMixer "+" affordance — a small ghost port just
            // below the last input port hinting that you can drop a
            // wire on the empty stretch of the left edge to add a new
            // port. Pure visual; the actual port allocation happens
            // on wire drop in the input-port hit-test fall-through.
            if kind.is_variadic() {
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
                let role = kind.input_port_role(0);
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

            // Output ports (right edge) — rounded square. Always
            // filled with the role colour because outputs are
            // signal sources by definition (a node always has
            // something to emit, even if nothing is wired to it).
            // The square shape is the canonical "this is a source"
            // cue (Fusion / Nuke convention) and pairs with the
            // input-side circle so a wire's source vs destination
            // is unambiguous on inspection.
            for p in 0..kind.output_port_count() {
                let pos = output_port_pos(&rect, p, &kind);
                let role = if kind.is_reroute() {
                    graph.signal_role(node_id, p)
                } else {
                    kind.output_port_role(p)
                };
                let (rr, gg, bb) = role.editor_color();
                let role_color = egui::Color32::from_rgb(rr, gg, bb);
                let half = PORT_RADIUS * zoom;
                // Hover affordance — soft ring around the output
                // port the cursor is near. Tells the user "you can
                // grab this to start a wire" without waiting on the
                // inline label tooltip. Skipped while a drag is
                // already in flight (drop targets are inputs only).
                let port_hovered = self.drag_wire.is_none()
                    && hover_pointer
                        .map(|hp| (hp - pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5))
                        .unwrap_or(false);
                if port_hovered {
                    painter.circle_stroke(
                        pos, half * 1.7,
                        egui::Stroke::new(
                            1.4_f32,
                            egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 160),
                        ),
                    );
                }
                let port_rect = egui::Rect::from_center_size(
                    pos, egui::vec2(half * 2.0, half * 2.0),
                );
                // All output ports paint as filled rounded squares
                // in the role colour — matte sockets render solid
                // magenta to match the magenta wire they emit, RGB
                // sockets stay solid amber, channel sockets paint
                // R/G/B. Type differentiation lives in the role
                // colour itself, paired with the wire it carries,
                // rather than in shape — which we tried as hollow
                // squares for matte and the user found too subtle.
                painter.rect_filled(port_rect, half * 0.32, role_color);
                // Inner dark dot — same pinhole the input side
                // gets when connected, so a connected input port
                // and an output port look like sockets of the
                // same anatomy with shape + colour conveying
                // direction.
                painter.circle_filled(pos, half * 0.38, theme.bg_deep);
                if cursor_near_node {
                    let port_rect = egui::Rect::from_center_size(
                        pos, egui::vec2(PORT_HIT_RADIUS * 2.0, PORT_HIT_RADIUS * 2.0),
                    );
                    let resp = ui.allocate_rect(port_rect, egui::Sense::hover());
                    if resp.hovered() {
                        paint_port_label(
                            &painter,
                            pos,
                            kind.output_port_label(p),
                            PortLabelSide::Right,
                            zoom,
                            theme,
                        );
                    }
                }
            }
            if !kind.is_reroute() {
                draw_view_eye(&painter, rect, zoom, is_soloed, eye_hovered, theme);
            }
        }

        // ── Node feedback (#1747) ──
        // A node that failed, declined or has no preview: a badge on its
        // header, the reason on hover. The hover is registered only near
        // the node, as the port tooltips are, so a parked cursor costs
        // nothing.
        for (node_id, rect, _, _, _) in node_rects.iter().copied() {
            let Some(feedback) = self.node_feedback.get(&node_id) else { continue };
            let size = (14.0 * zoom).max(10.0);
            let badge = egui::Rect::from_min_size(egui::pos2(rect.max.x - size - 3.0 * zoom, rect.min.y + 2.0 * zoom), egui::vec2(size, size));
            let colour = match feedback.kind {
                FeedbackKind::Failed => theme.marker_error,
                FeedbackKind::Declined => theme.marker_warning,
                FeedbackKind::NoPreview => theme.text_dim,
            };
            painter.circle_filled(badge.center(), size * 0.5, colour);
            painter.text(badge.center(), egui::Align2::CENTER_CENTER, feedback.kind.glyph(), egui::FontId::proportional(size * 0.7), theme.bg_deep);
            if hover_pointer.is_some_and(|p| rect.expand(4.0).contains(p)) {
                let tip = format!("{}: {}", feedback.kind.word(), feedback.reason);
                ui.interact(badge.union(rect), ui.id().with(("ngv2_node_feedback", node_id)), egui::Sense::hover()).on_hover_text(tip);
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
        let menu_open = self.node_menu.is_some() || self.add_menu_open || self.frame_menu.is_some() || self.sticky_menu.is_some() || self.wire_menu.is_some();
        // A press with Space held is the pan's, not a selection's (#1745).
        let panning = ui.input(|i| i.key_down(egui::Key::Space));
        if primary_pressed && !menu_open && !panning {
            if let Some(p) = pointer_pos {
                if canvas_rect.contains(p) {
                    // Output port hit?
                    let mut wire_started = false;
                    for (node_id, rect, _, kind, _) in &node_rects {
                        for port in 0..kind.output_port_count() {
                            let port_pos = output_port_pos(rect, port, kind);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                self.drag_wire = Some(DragWire { from_node: *node_id, from_port: port, detached: None });
                                wire_started = true;
                                break;
                            }
                        }
                        if wire_started { break; }
                    }
                    // A wire's input end (#1744): pressed, the wire lifts
                    // off it and follows the pointer from its source.
                    if !wire_started {
                        'input: for (node_id, rect, _, _, in_count) in &node_rects {
                            for port in 0..*in_count {
                                if (p - input_port_pos(rect, port, *in_count)).length() > PORT_HIT_RADIUS * zoom.max(0.5) { continue; }
                                if let Some(e) = graph.edges().iter().find(|e| e.to_node == *node_id && e.to_port == port) {
                                    self.drag_wire = Some(DragWire { from_node: e.from_node, from_port: e.from_port, detached: Some((*node_id, port)) });
                                    wire_started = true;
                                    break 'input;
                                }
                            }
                        }
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
                                // The eye on the header views the node in
                                // the program viewer, or goes back to the
                                // finished picture (#1763).
                                if !kind.is_reroute() && view_eye_rect(*rect, zoom).expand(2.0).contains(p) {
                                    cmds.push(view_toggle(graph, clip_id, *node_id, kind.is_output(), solo_node, &thumbnail_descent));
                                    node_hit = true;
                                    break;
                                }
                                if primary_double_clicked
                                    && kind.is_group()
                                {
                                    cmds.push(AppCommand::DescendIntoCompound {
                                        clip_id,
                                        node_id: *node_id,
                                    });
                                    node_hit = true;
                                    break;
                                }
                                // Any other grade node: a double-click
                                // names it (#893).
                                if primary_double_clicked && !kind.is_terminal() {
                                    let current = graph.nodes().iter().find(|n| n.id() == *node_id).map(|n| n.label().to_owned()).unwrap_or_default();
                                    self.node_rename_editor = Some((*node_id, p, current));
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
                                    let initial = graph.sticky_notes().iter()
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
                                let frame = match graph.frames().iter().find(|f| f.id == *frame_id) {
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
        let snap = self.snap_to_grid;
        if let Some(ref mut dn) = self.drag_node {
            if let Some(p) = pointer_pos {
                let g = to_graph(p);
                let (mut total_dx, mut total_dy) = (g.x - dn.start_cursor_x, g.y - dn.start_cursor_y);
                // On the grid (#1745): the grabbed node's corner lands on
                // a grid line, the rest keep their places beside it. It
                // started where it is now less what has been sent.
                if let Some(start) = snap.then(|| dn.node_ids.first().and_then(|id| graph.positions().get(id))).flatten() {
                    let (x0, y0) = (start.x - dn.emitted_dx, start.y - dn.emitted_dy);
                    total_dx = ((x0 + total_dx) / GRID_SPACING).round() * GRID_SPACING - x0;
                    total_dy = ((y0 + total_dy) / GRID_SPACING).round() * GRID_SPACING - y0;
                }
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
                let mut target = snap_target.map(|(n, port, _)| (n, port));
                if let (None, Some(p)) = (target, pointer_pos) {
                    'drop: for (node_id, rect, _, kind, in_count) in &node_rects {
                        if *node_id == dw.from_node { continue; }
                        let in_count = *in_count;
                        for port in 0..in_count {
                            let port_pos = input_port_pos(rect, port, in_count);
                            if (p - port_pos).length() <= PORT_HIT_RADIUS * zoom.max(0.5) {
                                target = Some((*node_id, port));
                                break 'drop;
                            }
                        }
                        // LayerMixer fall-through: drop on left edge
                        // band → grow port count by 1 and connect to
                        // the new port. The band is the full left edge
                        // of the node body (not just the header strip)
                        // so users have a generous target.
                        if kind.is_variadic() {
                            let edge_band = egui::Rect::from_min_max(
                                egui::pos2(rect.min.x - PORT_HIT_RADIUS, rect.min.y),
                                egui::pos2(rect.min.x + PORT_HIT_RADIUS, rect.max.y),
                            );
                            if edge_band.contains(p) {
                                target = Some((*node_id, in_count));
                                break 'drop;
                            }
                        }
                    }
                }
                let on_a_node = pointer_pos.is_some_and(|p| node_rects.iter().any(|(_, r, _, _, _)| r.contains(p)));
                match (target, dw.detached) {
                    // Back where it was lifted from: nothing changes.
                    (Some(to), Some(old)) if to == old => {}
                    // A connection the graph can't take says why, here
                    // (#1744): the grade dropped a loop without a word.
                    (Some((to, _)), _) if to == dw.from_node || feeds(graph, to, dw.from_node) => {
                        self.bookmark_toast = Some(("That wire would make a loop: a node can't feed itself".to_owned(), std::time::Instant::now()));
                    }
                    (Some((to, to_port)), None) => cmds.push(AppCommand::ConnectNodes { clip_id, from: dw.from_node, from_port: dw.from_port, to, to_port }),
                    (Some((new_to, new_to_port)), Some((to, to_port))) => cmds.push(AppCommand::ReconnectEdge { clip_id, to, to_port, new_to, new_to_port }),
                    // Lifted off and let go away from any input: gone.
                    (None, Some((to, to_port))) => cmds.push(AppCommand::DisconnectEdge { clip_id, to, to_port }),
                    // A new wire let go in empty canvas: the add menu, its
                    // pick fed by the wire (#1744).
                    (None, None) => if let Some(p) = pointer_pos.filter(|p| canvas_rect.contains(*p) && !on_a_node) {
                        let pass = ui.ctx().cumulative_pass_nr() + 1;
                        self.open_add_menu(p, to_graph(p), pass, None);
                        self.wire_from = Some((dw.from_node, dw.from_port));
                    },
                }
            }
            // A lone node dragged and let go over a wire goes into it
            // (#1744); a click on a node lying on a wire leaves it there.
            let dragged = self.drag_node.as_ref().filter(|d| d.node_ids.len() == 1 && (d.emitted_dx != 0.0 || d.emitted_dy != 0.0)).map(|d| d.node_ids[0]);
            if let (Some(wire), Some(node)) = (self.drop_wire.take(), dragged) {
                cmds.push(AppCommand::SpliceNodeIntoWire { clip_id, node_id: node, from: wire.0, from_port: wire.1, to: wire.2, to_port: wire.3 });
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
        if graph.keyboard_enabled() && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && !selected_set.is_empty()
        {
            let to_delete: Vec<NodeId> = selected_set
                .iter()
                .copied()
                .filter(|id| {
                    graph.nodes()
                        .iter()
                        .find(|n| n.id() == *id)
                        .map(|n| !n.kind().is_terminal())
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

        // Node editing keys (#1745): Ctrl+D duplicates the selection,
        // Ctrl+C copies it, Ctrl+V pastes at the pointer, and Tab or
        // Shift+A opens the add menu at the pointer, its search taking
        // the keys. Same gate as Delete.
        // Text being typed has the keys; a toolbar button that Tab has
        // just focused does not: egui moves the focus on Tab before the
        // canvas reads it, and the canvas's Tab is its quick add.
        let ctx = ui.ctx().clone();
        let focus = ctx.memory(|m| m.focused());
        let typing = focus.is_some_and(|id| egui::TextEdit::load_state(&ctx, id).is_some());
        if graph.keyboard_enabled() && !any_menu_open && !typing {
            let (duplicate, copy, paste, quick_add) = ui.input(|i| {
                let cmd = i.modifiers.command || i.modifiers.ctrl;
                (
                    cmd && i.key_pressed(egui::Key::D),
                    (cmd && i.key_pressed(egui::Key::C)) || i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                    (cmd && i.key_pressed(egui::Key::V)) || i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                    !cmd && !i.modifiers.alt
                        && ((i.key_pressed(egui::Key::Tab) && !i.modifiers.shift) || (i.modifiers.shift && i.key_pressed(egui::Key::A))),
                )
            });
            let picked = editable_selection(graph, *selected_node, selected_set);
            if duplicate && !picked.is_empty() {
                cmds.push(AppCommand::DuplicateNodes { clip_id, node_ids: picked.clone() });
            }
            if copy && !picked.is_empty() {
                cmds.push(AppCommand::CopyNodes { clip_id, node_ids: picked });
            }
            let at = pointer_pos.filter(|p| canvas_rect.contains(*p)).unwrap_or(canvas_rect.center());
            if paste {
                let g = to_graph(at);
                cmds.push(AppCommand::PasteNodes { clip_id, position: (g.x, g.y) });
            }
            if quick_add {
                if let Some(id) = focus {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
                self.open_add_menu(at, to_graph(at), ctx.cumulative_pass_nr(), None);
                // Over a wire, the pick goes into it (#1744).
                self.insert_wire = hovered_wire;
            }
        }

        // View bookmarks (Alt+1..9 jump, Shift+1..9 set; 1 alone views
        // the selected node, #1763). Same gate as
        // Delete: no popup menu open, no text-input focus. Ctrl /
        // Alt / Cmd modifiers skip the binding so this won't collide
        // with any global Ctrl+N or Alt+N shortcut. Empty slots emit
        // a "set me first" toast instead of silently doing nothing —
        // confirms the binding registered even when no jump
        // happened.
        if graph.keyboard_enabled() && !any_menu_open && !ui.ctx().wants_keyboard_input() {
            const NUM_KEYS: [egui::Key; 9] = [
                egui::Key::Num1, egui::Key::Num2, egui::Key::Num3,
                egui::Key::Num4, egui::Key::Num5, egui::Key::Num6,
                egui::Key::Num7, egui::Key::Num8, egui::Key::Num9,
            ];
            for (i, key) in NUM_KEYS.iter().enumerate() {
                if !ui.input(|inp| inp.key_pressed(*key)) { continue; }
                let mods = ui.input(|inp| inp.modifiers);
                if mods.ctrl || mods.command { continue; }
                // 1 views the selected node in the program viewer, or
                // goes back to the finished picture (#1763), as node
                // editors do; the view bookmarks jump on Alt.
                if i == 0 && !mods.shift && !mods.alt {
                    let viewed = (*selected_node).and_then(|n| graph.nodes().iter().find(|x| x.id() == n).map(|x| (n, x.kind().is_output(), x.kind().is_reroute())));
                    cmds.push(match viewed {
                        Some((n, output, false)) => view_toggle(graph, clip_id, n, output, solo_node, &thumbnail_descent),
                        _ => AppCommand::SetNodeSolo { at: None },
                    });
                    continue;
                }
                if mods.shift { self.set_bookmark(i); } else if mods.alt { self.jump_bookmark(i); }
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
        let f_with_mod = graph.keyboard_enabled() && ui.input(|i| {
            i.key_pressed(egui::Key::F) && (i.modifiers.ctrl || i.modifiers.command)
        });
        if f_with_mod && !self.frame_rename_editor.is_some()
            && !self.node_rename_editor.is_some()
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
                                let has_edge = graph.edges().iter().any(|e|
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
                            self.node_menu_opened_frame = frame_nr;
                            self.add_menu_open = false;
                            self.node_menu = None;
                            self.frame_menu = None;
                        } else if let Some(wire) = hovered_wire {
                            // A wire's own menu (#1744), over a frame too:
                            // the wire is drawn on top of it.
                            self.wire_menu = Some((wire, p, to_graph(p)));
                            self.node_menu_opened_frame = frame_nr;
                            self.add_menu_open = false;
                            self.node_menu = None;
                            self.frame_menu = None;
                            self.sticky_menu = None;
                        } else if let Some(fid) = on_frame {
                            self.frame_menu = Some((fid, p));
                            self.node_menu_opened_frame = frame_nr;
                            self.add_menu_open = false;
                            self.node_menu = None;
                            self.sticky_menu = None;
                        } else {
                            // Each menu invocation is a fresh search.
                            self.open_add_menu(p, to_graph(p), frame_nr, None);
                        }
                    }
                }
            }
        }

        // ── Floating menus (egui::Area) ──────────────────────────
        if self.add_menu_open {
            self.draw_add_menu(ui, clip_id, &mut cmds, theme, graph.add_menu());
        }
        if let Some((nid, _pos)) = self.node_menu {
            // Snapshot kind+enabled so we can borrow out of `graph` cleanly
            let info = graph.nodes().iter().find(|n| n.id() == nid).map(|n| (n.kind(), n.enabled()));
            if let Some((kind, enabled)) = info {
                self.draw_node_menu(ui, clip_id, nid, kind, enabled, &mut cmds, selected_node, selected_set, graph, solo_node, &thumbnail_descent);
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
        if self.node_rename_editor.is_some() {
            self.draw_node_rename_editor(ui, clip_id, &mut cmds);
        }
        if self.frame_description_editor.is_some() {
            self.draw_frame_description_editor(ui, clip_id, &mut cmds);
        }
        if self.sticky_menu.is_some() {
            self.draw_sticky_menu(ui, clip_id, graph, &mut cmds);
        }
        if self.wire_menu.is_some() {
            self.draw_wire_menu(ui, clip_id, graph.grade_tools(), &mut cmds);
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
        let mut visible: Vec<_> = node_rects.iter()
            .filter(|(_, rect, _, _, _)| rect.intersects(canvas_rect))
            .map(|(id, _, _, kind, _)| {
                let priority = !selected_set.contains(id) && *selected_node != Some(*id);
                let output = if kind.is_output() {
                    graph.edges().iter().find(|e| e.to_node == *id && e.to_port == 0).map_or(*id, |e| e.from_node)
                } else { *id };
                (priority, output)
            }).collect();
        visible.sort();
        let mut current_visible = Vec::new();
        for (_, id) in visible {
            if let Some(at) = graph.thumbnail_output(clip_id, id, &thumbnail_descent) {
                if !current_visible.contains(&at) { current_visible.push(at); }
            }
            if current_visible.len() == klipp_engine::node_thumbnails::MAX_THUMBS { break; }
        }
        if current_visible != self.last_visible_thumb_nodes {
            cmds.push(AppCommand::SetVisibleNodeThumbs {
                request: 0, clip_id, at: current_visible.clone(),
            });
            self.last_visible_thumb_nodes = current_visible;
        }
        self.output_thumbs.retain(|at, (_, texture)| {
            if self.last_visible_thumb_nodes.contains(at) { true }
            else { deferred_drops.push(texture.clone()); false }
        });
        self.thumb_clipping_overlays.retain(|at, (_, texture)| {
            if self.last_visible_thumb_nodes.contains(at) { true }
            else { deferred_drops.push(texture.clone()); false }
        });
        self.thumb_histograms.retain(|at, _| self.last_visible_thumb_nodes.contains(at));
        self.output_thumbs.retain(|at, (_, texture)| {
            if engine_node_thumbs.contains_key(at) { true } else { deferred_drops.push(texture.clone()); false }
        });
        self.thumb_clipping_overlays.retain(|at, (_, texture)| {
            if engine_node_thumbs.contains_key(at) { true } else { deferred_drops.push(texture.clone()); false }
        });
        self.thumb_histograms.retain(|at, _| engine_node_thumbs.contains_key(at));

        if let Some(label) = self.thumbnail_quality.and_then(|q| q.label()) {
            painter.text(canvas_rect.right_top() + egui::vec2(-8.0, 8.0), egui::Align2::RIGHT_TOP,
                label, egui::FontId::proportional(10.0), theme.text_dim);
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
                painter.rect_stroke(map_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border));

                // Node rects.
                for node in graph.nodes() {
                    let pos = graph.positions().get(&node.id()).copied()
                        .unwrap_or(NodePos { x: 0.0, y: 0.0 });
                    let (half_w, half_h) = if node.kind().is_reroute() {
                        (REROUTE_SIZE * 0.5, REROUTE_SIZE * 0.5)
                    } else {
                        (NODE_W * 0.5, NODE_H * 0.5)
                    };
                    let n_min = graph_to_map(egui::pos2(pos.x - half_w, pos.y - half_h));
                    let n_max = graph_to_map(egui::pos2(pos.x + half_w, pos.y + half_h));
                    let n_rect = egui::Rect::from_min_max(n_min, n_max);
                    let is_selected_n = *selected_node == Some(node.id())
                        || selected_set.contains(&node.id());
                    let is_soloed_n = solo_node == Some(node.id());
                    let on_n = node.effective_enabled(clip_time_secs);
                    let mut fill = node_color(&node.kind(), theme);
                    if is_selected_n { fill = theme.accent; }
                    if is_soloed_n { fill = egui::Color32::from_rgb(0xff, 0xd5, 0x3a); }
                    let alpha: u8 = if on_n { 220 } else { 100 };
                    let fill_a = egui::Color32::from_rgba_unmultiplied(
                        fill.r(), fill.g(), fill.b(), alpha,
                    );
                    if node.kind().is_reroute() {
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
                        egui::Stroke::new(1.5_f32, theme.accent),
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
                if let Some(pos) = graph.positions().get(&target_id).copied() {
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
                    egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_unmultiplied(
                        theme.border.r(), theme.border.g(), theme.border.b(), alpha,
                    )),
                );
                painter.galley(toast_rect.min + pad, galley, fg);
                ui.ctx().request_repaint();
            } else {
                self.bookmark_toast = None;
            }
        }

        let mut made = Vec::with_capacity(cmds.len());
        for command in cmds {
            let refused = graph.refusal(&command);
            match graph.command(command) {
                Some(command) => made.push(command),
                None => if let Some(why) = refused {
                    self.bookmark_toast = Some((why, std::time::Instant::now()));
                },
            }
        }
        made
    }

    /// Compute zoom + pan that frames every node in the graph
    /// inside the canvas with a comfortable margin. Called on first
    /// open of a clip, on Fit toolbar click, and on `F` press.
    fn fit_to_nodes(&mut self, graph: &impl CanvasGraph, canvas_rect: egui::Rect) {
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
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, a)),
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
        // Shift is the selection's modifier, never a pan (#1745): a
        // Shift-drag panned the canvas under the marquee it drew.
        let space_held = ui.input(|i| i.key_down(egui::Key::Space));
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

    /// The add menu opened at `screen` (canvas point `graph`), a fresh
    /// search; for "Replace with…" when `replace` names a node (#1745).
    fn open_add_menu(&mut self, screen: egui::Pos2, graph: egui::Pos2, pass: u64, replace: Option<NodeId>) {
        self.add_menu_open = true;
        self.add_menu_screen_pos = screen;
        self.add_menu_graph_pos = graph;
        self.add_menu_search.clear();
        self.add_menu_opened_frame = pass;
        self.replace_target = replace;
        self.insert_wire = None;
        self.wire_from = None;
        self.wire_menu = None;
        self.node_menu = None;
        self.frame_menu = None;
        self.sticky_menu = None;
    }

    /// The add menu's recent picks, newest first, by kind (#1745): what
    /// the app keeps across sessions.
    pub fn recent_kinds(&self) -> Vec<String> {
        self.add_menu_recents.iter().map(|k| (*k).to_owned()).collect()
    }

    /// The recent picks kept from an earlier session: those either
    /// canvas's add menu still offers, in their order.
    pub fn restore_recents(&mut self, kinds: &[String]) {
        let offered = |kind: &str| -> Option<&'static str> {
            model::grade_menu().iter().chain(picture::picture_menu()).flat_map(|(_, e)| e.iter()).map(|(_, k)| *k).find(|k| *k == kind)
        };
        self.add_menu_recents = kinds.iter().filter_map(|k| offered(k)).take(6).collect();
    }

    /// Whether the recent picks changed since the last call (#1745).
    pub fn take_recents_changed(&mut self) -> bool {
        std::mem::take(&mut self.recents_changed)
    }

    fn draw_add_menu(
        &mut self,
        ui: &mut egui::Ui,
        clip_id: ClipId,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
        categories: &[MenuCategory],
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

        // Lookup helper — recents store kind_str; we want to render
        // them with their display label, so we walk categories once.
        let label_for_kind = |kind: &str| -> Option<&'static str> {
            for (_, entries) in categories {
                for (label, k) in entries {
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
                    let replacing = self.replace_target.is_some();
                    if replacing {
                        ui.label(egui::RichText::new("Replace with\u{2026}").strong().color(theme.text_primary));
                    }
                    let search_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.add_menu_search)
                            .id(search_id)
                            .hint_text("Search nodes…")
                            .desired_width(200.0),
                    );
                    // Enter takes the first match, as a quick add's
                    // typing expects (#1745).
                    let enter = search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                    let matches = |label: &str| -> bool { !in_search || model::menu_entry_matches(label, &q) };
                    if enter && in_search {
                        chosen_kind = categories.iter().flat_map(|(_, e)| e.iter()).find(|(label, _)| matches(label)).map(|(_, k)| *k);
                    }
                    // Paste where the menu was opened (#1745).
                    if !replacing && self.insert_wire.is_none() && self.wire_from.is_none() && !in_search && add_menu_entry(ui, "Paste", theme) {
                        cmds.push(AppCommand::PasteNodes { clip_id, position: (self.add_menu_graph_pos.x, self.add_menu_graph_pos.y) });
                        close = true;
                    }

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
                            for (cat_name, entries) in categories {
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
                                let any = categories.iter().any(|(_, entries)| {
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
            let (kind, position) = (kind_str.to_string(), (self.add_menu_graph_pos.x, self.add_menu_graph_pos.y));
            match (self.replace_target, self.insert_wire, self.wire_from) {
                (Some(node_id), _, _) => cmds.push(AppCommand::ReplaceNode { clip_id, node_id, kind }),
                (None, Some((from, from_port, to, to_port)), _) => cmds.push(AppCommand::InsertNodeOnWire { clip_id, kind, position, from, from_port, to, to_port }),
                (None, None, Some((from, from_port))) => cmds.push(AppCommand::AddNodeWired { clip_id, kind, position, from, from_port }),
                (None, None, None) => cmds.push(AppCommand::AddNode { clip_id, kind, node_id: None, position }),
            }
            self.recents_changed = true;
            // Bump kind to the front of recents (dedupe + cap 6).
            // Storing the &'static str (not the display label) means
            // a future label rename doesn't poison the recents list.
            self.add_menu_recents.retain(|k| *k != kind_str);
            self.add_menu_recents.insert(0, kind_str);
            self.add_menu_recents.truncate(6);
            close = true;
        }
        if close {
            self.add_menu_open = false;
            self.replace_target = None;
            self.insert_wire = None;
            self.wire_from = None;
        }
    }

    /// A wire's menu (#1744): Delete, Insert node… (the add menu, its
    /// pick put into the wire), and on the grade, Add reroute.
    fn draw_wire_menu(&mut self, ui: &mut egui::Ui, clip_id: ClipId, grade: bool, cmds: &mut Vec<AppCommand>) {
        let Some(((from, from_port, to, to_port), pos, at)) = self.wire_menu else { return };
        let mut close = false;
        let area_resp = egui::Area::new(ui.id().with(("ngv2_wire_menu", to, to_port)))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                menu_frame(ui, |ui| {
                    if ui.button("Insert node\u{2026}").on_hover_text("Tab over a wire").clicked() {
                        let pass = ui.ctx().cumulative_pass_nr() + 1;
                        self.open_add_menu(pos, at, pass, None);
                        self.insert_wire = Some((from, from_port, to, to_port));
                        close = true;
                    }
                    if grade && ui.button("Add reroute").clicked() {
                        cmds.push(AppCommand::InsertNodeOnWire { clip_id, kind: "Reroute".into(), position: (at.x, at.y), from, from_port, to, to_port });
                        close = true;
                    }
                    if ui.button("Delete").clicked() {
                        cmds.push(AppCommand::DisconnectEdge { clip_id, to, to_port });
                        close = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { close = true; }
                });
            });
        if ui.ctx().cumulative_pass_nr() != self.node_menu_opened_frame && area_resp.response.clicked_elsewhere() { close = true; }
        if close { self.wire_menu = None; }
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
        graph: &impl CanvasGraph,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((frame_id, pos)) = self.frame_menu else { return; };
        // Resolve current colour for the swatch row's "selected"
        // marker. None if the frame was deleted between the right-
        // click and the next paint pass.
        let current_color = graph.frames().iter()
            .find(|f| f.id == frame_id)
            .map(|f| f.color_rgba);
        let mut close = false;
        let area_id = ui.id().with(("ngv2_frame_menu", frame_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                menu_frame(ui, |ui| {
                    if ui.button("Rename…").clicked() {
                        let initial = graph.frames().iter()
                            .find(|f| f.id == frame_id)
                            .map(|f| f.label.clone())
                            .unwrap_or_default();
                        self.frame_rename_editor = Some((frame_id, pos, initial));
                        close = true;
                    }
                    if ui.button("Edit description…").clicked() {
                        let initial = graph.frames().iter()
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
                                    egui::RichText::new(glyph).size(13.0).color(swatch_color),
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
        if ui.ctx().cumulative_pass_nr() != self.node_menu_opened_frame && area_resp.response.clicked_elsewhere() { close = true; }
        if close { self.frame_menu = None; }
    }

    /// Inline rename popup for a node (#893), from its menu's "Rename…" or
    /// a double-click on it. Enter or Save names it, one undo step;
    /// Escape, Cancel or a click away leaves it. A blank name is not
    /// taken: a node always shows a name.
    fn draw_node_rename_editor(&mut self, ui: &mut egui::Ui, clip_id: ClipId, cmds: &mut Vec<AppCommand>) {
        let Some((node_id, pos)) = self.node_rename_editor.as_ref().map(|(n, p, _)| (*n, *p)) else { return };
        let (mut close, mut save) = (false, false);
        let area_resp = egui::Area::new(ui.id().with(("ngv2_node_rename", node_id)))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Node name").small().weak());
                    if let Some((_, _, buf)) = self.node_rename_editor.as_mut() {
                        let resp = ui.add(egui::TextEdit::singleline(buf).hint_text("e.g. Skin / Sky / Look").desired_width(180.0));
                        resp.request_focus();
                        // Enter saves while the editor is open. Focus is
                        // asked for every frame, so the field takes it
                        // back the frame it lets go and `lost_focus`
                        // never fires.
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
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
            let label = self.node_rename_editor.as_ref().map(|(_, _, b)| b.trim().to_string()).unwrap_or_default();
            if !label.is_empty() {
                cmds.push(AppCommand::RenameNode { clip_id, node_id, label });
            }
            close = true;
        }
        if close { self.node_rename_editor = None; }
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
                        // Enter saves while the editor is open: focus is
                        // asked for every frame, so `lost_focus` never
                        // fires (found with #893's node rename).
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
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
        graph: &impl CanvasGraph,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some((sticky_id, pos)) = self.sticky_menu else { return; };
        let mut close = false;
        let area_id = ui.id().with(("ngv2_sticky_menu", sticky_id));
        let area_resp = egui::Area::new(area_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                menu_frame(ui, |ui| {
                    if ui.button("Edit text…").clicked() {
                        let initial = graph.sticky_notes().iter()
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
        if ui.ctx().cumulative_pass_nr() != self.node_menu_opened_frame && area_resp.response.clicked_elsewhere() { close = true; }
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
        kind: impl CanvasKind,
        enabled: bool,
        cmds: &mut Vec<AppCommand>,
        selected_node: &mut Option<NodeId>,
        selected_set: &mut std::collections::HashSet<NodeId>,
        graph: &impl CanvasGraph,
        solo_node: Option<NodeId>,
        descent: &[NodeId],
    ) {
        let pos = self.node_menu.map(|(_, p)| p).unwrap_or_default();
        let menu_id = ui.id().with(("ngv2_node_menu", node_id));
        let mut close = false;
        let area_resp = egui::Area::new(menu_id)
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                menu_frame(ui, |ui| {
                    // Source / Output only get the wireless controls
                    // below — bypass / delete / group don't apply to
                    // them, so skip those entries entirely.
                    let is_structural = kind.is_terminal();
                    // Reroute is already a passthrough — Bypass
                    // would be a no-op-on-no-op; suppress to avoid
                    // user confusion (Delete / Disconnect still
                    // appear).
                    let is_passthrough_kind = kind.is_reroute();
                    // Source and Output have a menu too, as every node in
                    // a node editor does: view, add next to it, inspect.
                    if is_structural {
                        if kind.is_source() {
                            let viewed = solo_node == Some(node_id);
                            if menu_item(ui, if viewed { "Back to finished picture" } else { "View in viewer" }, "1").clicked() {
                                cmds.push(view_toggle(graph, clip_id, node_id, false, solo_node, descent));
                                close = true;
                            }
                        } else if solo_node.is_some() && menu_item(ui, "Back to finished picture", "1").clicked() {
                            cmds.push(AppCommand::SetNodeSolo { at: None });
                            close = true;
                        }
                        let add = if kind.is_source() { "Add node after\u{2026}" } else { "Add node\u{2026}" };
                        if menu_item(ui, add, "Tab").clicked() {
                            // A node added in the picture goes after the
                            // selected one, or at the chain's end.
                            *selected_node = kind.is_source().then_some(node_id);
                            selected_set.clear();
                            selected_set.extend(*selected_node);
                            let at = graph.positions().get(&node_id).map_or(egui::Pos2::ZERO, |p| egui::pos2(p.x + if kind.is_source() { 200.0 } else { -200.0 }, p.y + 110.0));
                            self.open_add_menu(pos, at, ui.ctx().cumulative_pass_nr() + 1, None);
                            close = true;
                        }
                        if ui.button("Show in Inspector").clicked() {
                            cmds.push(AppCommand::ShowPanel { workspace: None, inspector_tab: Some("video".into()) });
                            close = true;
                        }
                        ui.separator();
                    }
                    if !is_structural && ui.button("Rename\u{2026}").clicked() {
                        let current = graph.nodes().iter().find(|n| n.id() == node_id).map(|n| n.label().to_owned()).unwrap_or_default();
                        self.node_rename_editor = Some((node_id, pos, current));
                        close = true;
                    }
                    // A colour label, to find the node again (#1753): a
                    // row of the swatches in the menu itself, the one it
                    // has ringed.
                    if let Some(at) = graph.thumbnail_output(clip_id, node_id, descent) {
                        let current = self.node_labels.get(&at.path).copied().unwrap_or(0);
                        if let Some(colour) = label_swatches(ui, current) {
                            cmds.push(AppCommand::SetNodeColourLabel { node: at.path, colour });
                            close = true;
                        }
                    }
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
                        let label = if is_soloed { "Back to finished picture" } else { "View in viewer" };
                        if menu_item(ui, label, "1").clicked() {
                            let new_target = if is_soloed { None } else { Some(node_id) };
                            cmds.push(AppCommand::SetNodeSolo { at: new_target.and_then(|n| graph.thumbnail_output(clip_id, n, descent)) });
                            close = true;
                        }
                        // The viewer wiped between the node's input (left)
                        // and its output (#2758).
                        let compared = self.compared_node == Some(node_id);
                        let label = if compared { "End the A/B compare" } else { "Compare input and output (A/B)" };
                        if menu_item(ui, label, "").clicked() {
                            cmds.push(ab_toggle(graph, clip_id, node_id, compared, descent));
                            close = true;
                        }
                    }
                    let in_set = selected_set.contains(&node_id);
                    // Editing the node, or the selection it is part of
                    // (#1745): each also on a key (Ctrl+D, Ctrl+C, Tab
                    // for the add menu).
                    if !is_structural {
                        ui.separator();
                        let targets = if in_set && selected_set.len() > 1 {
                            editable_selection(graph, Some(node_id), selected_set)
                        } else {
                            vec![node_id]
                        };
                        if menu_item(ui, "Duplicate", "Ctrl+D").clicked() {
                            cmds.push(AppCommand::DuplicateNodes { clip_id, node_ids: targets.clone() });
                            close = true;
                        }
                        if menu_item(ui, "Copy", "Ctrl+C").clicked() {
                            cmds.push(AppCommand::CopyNodes { clip_id, node_ids: targets });
                            close = true;
                        }
                        // The grade where this node's key isn't (#898): a
                        // correction after it, the key the other way round.
                        if graph.takes_outside_node(node_id) && ui.button("Add Outside Node").clicked() {
                            cmds.push(AppCommand::AddOutsideNode { clip_id, node_id });
                            close = true;
                        }
                        if !kind.is_group() && !is_passthrough_kind && ui.button("Reset to defaults").clicked() {
                            cmds.push(AppCommand::ResetNode { clip_id, node_id });
                            close = true;
                        }
                        if !kind.is_group() && ui.button("Replace with\u{2026}").clicked() {
                            // Drawn from the next pass on: the add menu is
                            // drawn before this one, so its search takes
                            // the keys as it first shows.
                            let pass = ui.ctx().cumulative_pass_nr() + 1;
                            self.open_add_menu(pos, egui::Pos2::ZERO, pass, Some(node_id));
                            close = true;
                        }
                        if ui.button("Show in Inspector").clicked() {
                            *selected_node = Some(node_id);
                            selected_set.clear();
                            selected_set.insert(node_id);
                            let tab = if graph.grade_tools() { "color" } else { "effects" };
                            cmds.push(AppCommand::ShowPanel { workspace: None, inspector_tab: Some(tab.into()) });
                            close = true;
                        }
                        // Align and spread a selection (#1745), one undo
                        // step each, as a drag is.
                        if in_set && selected_set.len() >= 2 {
                            ui.menu_rows("Align \u{25B8}", |ui| {
                                for (label, how, least) in [
                                    ("Left edges", Align::Left, 2), ("Centres across", Align::Centre, 2), ("Right edges", Align::Right, 2),
                                    ("Top edges", Align::Top, 2), ("Middles down", Align::Middle, 2), ("Bottom edges", Align::Bottom, 2),
                                    ("Spread across", Align::SpreadAcross, 3), ("Spread down", Align::SpreadDown, 3),
                                ] {
                                    if selected_set.len() >= least && ui.button(label).clicked() {
                                        let ids: Vec<NodeId> = selected_set.iter().copied().collect();
                                        for (node_id, delta_x, delta_y) in align_moves(graph, &ids, how) {
                                            cmds.push(AppCommand::MoveNode { clip_id, node_id, delta_x, delta_y });
                                        }
                                        ui.close_menu();
                                        close = true;
                                    }
                                }
                            });
                        }
                        ui.separator();
                    }
                    // Group-into-Compound: only offered when the
                    // right-clicked node is part of a multi-selection
                    // of 2+ nodes. Engine-side handler validates the
                    // selection's external-edge shape and rejects
                    // gracefully (toast) if the v1 single-port
                    // restriction would be violated.
                    // A frame is the canvas's own, on the picture as on
                    // the grade (#1743); a compound is the grade's.
                    // The selected nodes of a picture saved as a recipe,
                    // placed on any clip after (#2757 step 6).
                    if !is_structural && in_set && !graph.grade_tools() && ui.button(format!("Save {} as Recipe\u{2026}", if selected_set.len() == 1 { "Node".to_string() } else { format!("{} Nodes", selected_set.len()) })).on_hover_text("Save these nodes as a recipe: the recipe op or the Recipes list puts them on any clip, its chosen numbers as controls").clicked() {
                        cmds.push(AppCommand::SaveNodesAsRecipe { clip_id, node_ids: selected_set.iter().copied().collect() });
                        close = true;
                    }
                    if !is_structural && in_set && selected_set.len() >= 2 {
                        let label = format!(
                            "Group {} into Compound",
                            selected_set.len(),
                        );
                        if graph.grade_tools() && ui.button(label).clicked() {
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
                    if graph.grade_tools() && kind.is_group()
                        && ui.button("Ungroup Compound").clicked() {
                            cmds.push(AppCommand::UngroupCompound {
                                clip_id,
                                node_id,
                            });
                            close = true;
                        }

                    // Wireless-link controls: a sender label on the
                    // primary output (one per node) and per-input-port
                    // receiver labels that resolve to whichever node
                    // currently broadcasts the matching label.
                    // Source / Output are structural, so Source gets
                    // only the send control and Output only the receive.
                    let node_ref = graph.nodes().iter().find(|n| n.id() == node_id);
                    let current_send = node_ref.and_then(|n| n.output_link().map(str::to_owned));
                    let current_receives: Vec<Option<String>> = node_ref
                        .map(|n| n.input_links().to_vec()).unwrap_or_default();
                    let in_count = node_ref.map(|n| n.input_count()).unwrap_or(0);
                    let can_send = graph.grade_tools() && !kind.is_output();
                    let can_receive = graph.grade_tools() && !kind.is_source() && in_count > 0;
                    let available_labels: Vec<String> = graph.nodes().iter()
                        .filter(|n| n.id() != node_id)
                        .filter_map(|n| n.output_link().map(str::to_owned))
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
                        ui.menu_rows("Wireless: receive from ▸", |ui| {
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
                                if cur.is_some()
                                    && ui.button("Clear").clicked() {
                                        cmds.push(AppCommand::SetNodeInputLink {
                                            clip_id, node_id, port, label: None,
                                        });
                                        ui.close_menu();
                                        close = true;
                                    }
                                ui.separator();
                            }
                        });
                    } else if can_receive && current_receives.iter().any(|s| s.is_some())
                        && ui.button("Wireless: clear all receives").clicked() {
                            for (port, _) in current_receives.iter().enumerate()
                                .filter(|(_, s)| s.is_some())
                            {
                                cmds.push(AppCommand::SetNodeInputLink {
                                    clip_id, node_id, port: port as u8, label: None,
                                });
                            }
                            close = true;
                        }
                    let multi_label = if in_set && selected_set.len() > 1 {
                        format!("Delete {} selected", selected_set.len())
                    } else {
                        "Delete".to_string()
                    };
                    if !is_structural && menu_item(ui, &multi_label, "Del").clicked() {
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
                                    graph.nodes().iter()
                                        .find(|n| n.id() == *id)
                                        .map(|n| !n.kind().is_terminal())
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

/// Whether `from` feeds `to`, through any wires (#1744): a wire from `to`
/// back into `from` would close a loop.
fn feeds(graph: &impl CanvasGraph, from: NodeId, to: NodeId) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut next = vec![from];
    while let Some(n) = next.pop() {
        if n == to {
            return true;
        }
        if seen.insert(n) {
            next.extend(graph.edges().iter().filter(|e| e.from_node == n).map(|e| e.to_node));
        }
    }
    false
}

/// The selection the node editor's own edits act on (#1745): the set,
/// else the primary, Source and Output left out, in the graph's order.
fn editable_selection(graph: &impl CanvasGraph, primary: Option<NodeId>, set: &std::collections::HashSet<NodeId>) -> Vec<NodeId> {
    graph.nodes().iter()
        .filter(|n| (set.contains(&n.id()) || (set.is_empty() && primary == Some(n.id()))) && !n.kind().is_terminal())
        .map(|n| n.id())
        .collect()
}

/// How the node menu lines a selection up (#1745).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Align { Left, Centre, Right, Top, Middle, Bottom, SpreadAcross, SpreadDown }

/// The moves that line `ids` up `how`, by each node's card: an edge or
/// centre to the selection's extreme or mean, or the cards spread evenly
/// between the first and the last. Nodes already in place are left out.
fn align_moves(graph: &impl CanvasGraph, ids: &[NodeId], how: Align) -> Vec<(NodeId, f32, f32)> {
    let mut at: Vec<(NodeId, NodePos)> = ids.iter().filter_map(|id| graph.positions().get(id).map(|p| (*id, *p))).collect();
    if at.len() < 2 {
        return Vec::new();
    }
    match how {
        Align::SpreadAcross => at.sort_by(|a, b| a.1.x.total_cmp(&b.1.x)),
        Align::SpreadDown => at.sort_by(|a, b| a.1.y.total_cmp(&b.1.y)),
        _ => {}
    }
    let xs = at.iter().map(|(_, p)| p.x);
    let ys = at.iter().map(|(_, p)| p.y);
    let (min_x, max_x) = (xs.clone().fold(f32::INFINITY, f32::min), xs.fold(f32::NEG_INFINITY, f32::max));
    let (min_y, max_y) = (ys.clone().fold(f32::INFINITY, f32::min), ys.fold(f32::NEG_INFINITY, f32::max));
    let n = at.len() as f32;
    let mean = |f: fn(&NodePos) -> f32| at.iter().map(|(_, p)| f(p)).sum::<f32>() / n;
    let target = |p: &NodePos, i: usize| -> (f32, f32) {
        let step = |lo: f32, hi: f32| lo + (hi - lo) * i as f32 / (n - 1.0);
        match how {
            Align::Left => (min_x, p.y),
            Align::Right => (max_x, p.y),
            Align::Centre => (mean(|p| p.x), p.y),
            Align::Top => (p.x, min_y),
            Align::Bottom => (p.x, max_y),
            Align::Middle => (p.x, mean(|p| p.y)),
            Align::SpreadAcross => (step(min_x, max_x), p.y),
            Align::SpreadDown => (p.x, step(min_y, max_y)),
        }
    };
    at.iter().enumerate().filter_map(|(i, (id, p))| {
        let (x, y) = target(p, i);
        let (dx, dy) = (x - p.x, y - p.y);
        (dx.abs() > 0.01 || dy.abs() > 0.01).then_some((*id, dx, dy))
    }).collect()
}

// ── Port position helpers ─────────────────────────────────────────
// Ports are evenly distributed along the left (input) / right
// (output) edge of the node body rectangle.

/// Bounding box of every node's body rectangle in graph coordinates.
/// Empty graphs return `None` (defensive — fit-to-view + minimap
/// short-circuit cleanly). Reroute nodes contribute their smaller
/// 20×20 footprint rather than NODE_W×NODE_H so the bounds reflect
/// what's actually painted.
/// Where a node's view eye sits: the left of its header (#1763).
fn view_eye_rect(node: egui::Rect, zoom: f32) -> egui::Rect {
    let size = (NODE_HEADER_H - 4.0) * zoom.max(0.6);
    egui::Rect::from_min_size(egui::pos2(node.min.x + 3.0 * zoom.max(0.6), node.min.y + 2.0 * zoom.max(0.6)), egui::vec2(size, size))
}

/// The command that views `node` in the program viewer, or goes back to
/// the finished picture when it is the one viewed or the Output (#1763).
fn view_toggle(graph: &impl CanvasGraph, clip_id: ClipId, node: NodeId, is_output: bool, viewed: Option<NodeId>, descent: &[NodeId]) -> AppCommand {
    let back = is_output || viewed == Some(node);
    AppCommand::SetNodeSolo { at: if back { None } else { graph.thumbnail_output(clip_id, node, descent) } }
}

/// The command that wipes the viewer between `node`'s input and its
/// output (#2758), or ends the wipe when it is the one compared.
fn ab_toggle(graph: &impl CanvasGraph, clip_id: ClipId, node: NodeId, compared: bool, descent: &[NodeId]) -> AppCommand {
    match graph.thumbnail_output(clip_id, node, descent).filter(|_| !compared) {
        Some(at) => AppCommand::SetSplitCompareNode { clip_id: Some(clip_id), path: Some(at.path) },
        None => AppCommand::SetSplitCompareNode { clip_id: None, path: None },
    }
}

/// The eye on a node's header: open and lit while the node is viewed,
/// faint otherwise, shown while hovered (#1763).
fn draw_view_eye(painter: &egui::Painter, node: egui::Rect, zoom: f32, viewed: bool, hovered: bool, theme: &KlippTheme) {
    if !viewed && !hovered {
        return;
    }
    let r = view_eye_rect(node, zoom);
    let colour = if viewed { egui::Color32::from_rgb(0xff, 0xd5, 0x3a) } else { theme.text_dim };
    let c = r.center();
    let (w, h) = (r.width() * 0.5, r.height() * 0.3);
    let lid = |sign: f32| -> Vec<egui::Pos2> { (0..=8).map(|i| { let t = i as f32 / 8.0; let x = -w + 2.0 * w * t; egui::pos2(c.x + x, c.y + sign * h * (1.0 - (x / w).powi(2))) }).collect() };
    let stroke = egui::Stroke::new(1.2_f32, colour);
    painter.add(egui::Shape::line(lid(-1.0), stroke));
    painter.add(egui::Shape::line(lid(1.0), stroke));
    if viewed {
        painter.circle_filled(c, h * 0.9, colour);
    } else {
        painter.circle_stroke(c, h * 0.8, stroke);
    }
}

fn graph_bounds(graph: &impl CanvasGraph) -> Option<egui::Rect> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for node in graph.nodes() {
        let pos = graph.positions().get(&node.id()).copied()
            .unwrap_or(NodePos { x: 0.0, y: 0.0 });
        let (half_w, half_h) = if node.kind().is_reroute() {
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

/// Which side of a port the hover label sits on. Inputs sit on
/// the LEFT (outside the node body, beyond the port circle); outputs
/// sit on the RIGHT. Keeps the chip from overlapping the node body.
#[derive(Clone, Copy)]
enum PortLabelSide { Left, Right }

/// Paint an inline port-label chip right next to a hovered port.
/// Replaces the egui tooltip path because the latter has a ~0.5 s
/// delay and the popup lands at the cursor (often far from the
/// port itself). This appears immediately and is anchored to the
/// port for an unambiguous "this is what THIS port is" cue.
fn paint_port_label(
    painter: &egui::Painter,
    port_pos: egui::Pos2,
    text: &str,
    side: PortLabelSide,
    zoom: f32,
    theme: &KlippTheme,
) {
    let z = zoom.max(0.6);
    let font = egui::FontId::proportional(10.0 * z);
    let galley = painter.layout_no_wrap(text.to_string(), font, theme.text_primary);
    let pad = egui::vec2(8.0 * z, 3.0 * z);
    let chip_size = galley.size() + pad * 2.0;
    let port_gap = PORT_RADIUS * zoom * 1.4 + 6.0;
    let chip_min = match side {
        PortLabelSide::Left => egui::pos2(
            port_pos.x - port_gap - chip_size.x,
            port_pos.y - chip_size.y * 0.5,
        ),
        PortLabelSide::Right => egui::pos2(
            port_pos.x + port_gap,
            port_pos.y - chip_size.y * 0.5,
        ),
    };
    let chip_rect = egui::Rect::from_min_size(chip_min, chip_size);
    // Translucent dark backdrop + thin border so the label reads
    // on top of any canvas content (wires, frames, other nodes).
    painter.rect_filled(
        chip_rect, 2.0,
        egui::Color32::from_rgba_unmultiplied(
            theme.bg_deep.r(), theme.bg_deep.g(), theme.bg_deep.b(), 230,
        ),
    );
    painter.rect_stroke(
        chip_rect, 2.0,
        egui::Stroke::new(0.5_f32, theme.border),
    );
    painter.galley(chip_min + pad, galley, theme.text_primary);
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
        painter.rect_filled(resp.rect, 2.0, bg);
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
fn kind_category_glyph(kind: &impl CanvasKind) -> &'static str { kind.glyph() }

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

fn output_port_pos(rect: &egui::Rect, port: u8, kind: &impl CanvasKind) -> egui::Pos2 {
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



#[cfg(test)]
mod rename_tests {
    //! A grade node is named from the graph (#893): a double-click on it,
    //! a name typed, Enter.
    use super::*;

    #[test]
    fn a_double_click_names_a_node() {
        let mut graph = NodeGraph::default();
        let node = graph.ensure_corrector();
        let label = graph.nodes.iter().find(|n| n.id == node).unwrap().label.clone();
        let (ctx, mut panel, theme) = (egui::Context::default(), NodeGraphV2Panel::default(), KlippTheme::default());
        let clip_id = ClipId::new();
        let (mut selected, mut set, mut path, mut drops) = (None, std::collections::HashSet::new(), Vec::new(), Vec::new());
        let thumbs = std::collections::HashMap::new();
        let mut time = 0.0;
        let mut frame = |events: Vec<egui::Event>, graph: &mut NodeGraph| -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
            time += 1.0 / 30.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut cmds = Vec::new();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = panel.show(ui, &mut selected, &mut set, graph, clip_id, &theme, &thumbs, 0, &mut drops, &mut path, &[], None, None, 0.0, false, 0.0, None);
                });
            });
            let drawn = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            }).collect();
            (drawn, cmds)
        };
        let mut drawn = Vec::new();
        for _ in 0..3 {
            drawn = frame(Vec::new(), &mut graph).0;
        }
        let at = drawn.iter().find(|(t, _)| *t == label).unwrap_or_else(|| panic!("the node's name is drawn: {drawn:?}")).1;
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for _ in 0..2 {
            frame(vec![egui::Event::PointerMoved(at), press(true)], &mut graph);
            frame(vec![press(false)], &mut graph);
        }
        let (drawn, _) = frame(Vec::new(), &mut graph);
        assert!(drawn.iter().any(|(t, _)| t == "Node name"), "the rename editor opens: {drawn:?}");
        frame(vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }], &mut graph);
        frame(vec![egui::Event::Text("Skin".into())], &mut graph);
        let (_, cmds) = frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], &mut graph);
        assert!(
            cmds.iter().any(|c| matches!(c, AppCommand::RenameNode { node_id, label, .. } if *node_id == node && label == "Skin")),
            "Enter names it: {cmds:?}"
        );
    }
}

#[cfg(test)]
mod breadcrumb_tests {
    use super::*;
    use klipp_timeline::GradeAccess;

    #[test]
    fn the_clip_crumb_leaves_the_grade_and_keeps_the_clip() {
        for width in [240.0, 800.0] {
            for nested in [false, true] {
                let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
                let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
                let id = clip.id;
                let before = clip.picture_v2().unwrap();
                let mut graph = clip.grade().clone();
                let mut panel = NodeGraphV2Panel::default();
                let mut selected = Some(graph.output_node_id);
                let mut set = [graph.output_node_id].into_iter().collect();
                let mut path = if nested { vec![NodeId::new_v4(), NodeId::new_v4()] } else { Vec::new() };
                let labels = if nested { vec!["Outer group".to_owned(), "Inner group".to_owned()] } else { Vec::new() };
                let ctx = egui::Context::default();
                let mut time = 0.0;
                let mut frame = |events| {
                    time += 0.1;
                    let mut cmds = Vec::new();
                    let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            cmds = panel.show(ui, &mut selected, &mut set, &mut graph, id, &KlippTheme::default(), &Default::default(), 0, &mut Vec::new(), &mut path, &labels, Some(&[]), None, 0.0, false, 0.0, None);
                        });
                    });
                    let texts: Vec<_> = out.shapes.iter().filter_map(|s| match &s.shape {
                        egui::Shape::Text(t) => Some((t.galley.text().to_owned(), egui::Rect::from_min_size(t.pos, t.galley.size()))), _ => None,
                    }).collect();
                    (cmds, texts)
                };
                let mut texts = Vec::new();
                for _ in 0..3 { texts = frame(Vec::new()).1; }
                for label in if nested { vec!["Clip", "Grade", "Outer group", "Inner group"] } else { vec!["Clip", "Grade"] } {
                    let r = texts.iter().find(|(s, _)| s == label).unwrap().1;
                    assert!(r.left() >= 0.0 && r.right() <= width, "{label}: {r:?}");
                }
                let at = texts.iter().find(|(s, _)| s == "Clip").unwrap().1.center();
                let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                frame(vec![egui::Event::PointerMoved(at), button(true)]);
                let cmds = frame(vec![button(false)]).0;
                let roots: Vec<_> = cmds.iter().filter_map(|cmd| if let AppCommand::SetGraphDockRoot { root } = cmd { Some(*root) } else { None }).collect();
                assert_eq!(roots, [klipp_graph_format::path::Root::Clip { clip: id.0 }]);
                let mut state = crate::state::AppState::default();
                state.sequences[0].tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(clip);
                state.selected_clip = Some(id);
                state.active_workspace = crate::state::Workspace::Color;
                state.show_color_panel = true;
                state.graph_descent_path = path;
                state.path_command(&AppCommand::SetGraphDockRoot { root: roots[0] });
                assert_eq!(state.selected_clip, Some(id));
                assert_eq!(state.active_workspace, crate::state::Workspace::Color);
                assert!(state.graph_dock_open && state.graph_dock_path.is_empty() && state.graph_descent_path.is_empty());
                assert_eq!(state.active_panel, Some(crate::state::PanelKind::PictureGraph));
                assert!(!state.dirty);
                assert_eq!(state.active_history().undo_count(), 0);
                assert_eq!(state.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
            }
        }
    }
}

#[cfg(test)]
mod dock_toolbar_tests {
    use super::*;

    #[test]
    fn narrow_docks_keep_toolbar_actions_visible() {
        for width in [240.0, 320.0, 480.0, 586.0, 800.0] {
            let ctx = egui::Context::default();
            let mut panel = NodeGraphV2Panel::default();
            let mut graph = NodeGraph::default();
            graph.ensure_corrector();
            let theme = KlippTheme::default();
            theme.apply_fonts_with_size(&ctx, 12.0);
            let clip = ClipId::new();
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0))),
                    ..Default::default()
                }, |ctx| {
                    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                        panel.show(ui, &mut None, &mut Default::default(), &mut graph, clip, &theme,
                            &Default::default(), 0, &mut Vec::new(), &mut Vec::new(), &[], Some(&[]), None,
                            0.0, false, 0.0, None);
                    });
                });
                let texts: Vec<_> = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text(), egui::Rect::from_min_size(t.pos, t.galley.size()), s.clip_rect)),
                    _ => None,
                }).collect();
                for label in ["Fit", "1:1", "Auto-layout", "+ Save version"] {
                    let (_, rect, clip) = texts.iter().find(|(t, _, _)| t.starts_with(label)).unwrap_or_else(|| panic!("missing {label}"));
                    assert!(rect.left() >= -1.0 && rect.right() <= width + 1.0, "{width}: {label} at {rect:?}");
                    assert!(clip.expand(1.0).contains_rect(*rect), "{width}: {label} clipped");
                    assert!(rect.bottom() < 100.0, "{width}: {label} creates an oversized toolbar: {rect:?}");
                }
            }
        }
    }
}

/// The add-node menu's categories — display order top-to-bottom in the menu when
/// search is empty. Source of truth for both the flat-search
/// path AND the categorised path; entries the user can pick
/// are exactly these. Every kind is one `add_color_node` can make
/// (`klipp_nodes::kinds::ADDABLE_NODE_KINDS`): a name outside it would
/// add nothing, which the test below holds.
#[cfg(test)]
const ADD_MENU_CATEGORIES: &[(&str, &[(&str, &str)])] = &[
    ("Primary Grade", &[
        ("Corrector (all-in-one)", "Corrector"),
        ("Exposure", "Exposure"),
        ("Contrast", "Contrast"),
        ("Saturation", "Saturation"),
        ("Tone Controls", "ToneControls"),
        ("Colour Wheels", "ColorWheels"),
        ("Wheel Sat/Bri", "WheelSatBri"),
        ("Curves", "RGBCurves"),
        ("Hue/Sat", "HueSatCurves"),
        ("Colour Balance", "ColorBalance"),
        ("HDR Zones", "HdrZones"),
        ("Colour Match", "ColorMatch"),
    ]),
    ("Secondary", &[
        ("Qualifier", "Qualifier"),
        ("Luma Key", "LumaKey"),
        ("Soft Clip", "SoftClip"),
        ("Mask", "Mask"),
        ("External Matte", "ExternalMatte"),
        ("Difference Key", "DifferenceKey"),
        ("Combine Mattes", "CombineMattes"),
        ("Key Mixer", "KeyMixer"),
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
        ("Colour Warper", "ColorWarper"),
        ("Noise Reduction", "Denoise"),
        ("RGB Splitter", "RGBSplitter"),
        ("RGB Combiner", "RGBCombiner"),
        ("Reroute", "Reroute"),
    ]),
    ("Structural", &[
        ("Compound", "Compound"),
    ]),
];

#[cfg(test)]
mod add_menu_tests {
    #[test]
    fn the_grade_add_menu_lists_the_same_entries() {
        use super::model::CanvasGraph;
        let graph = klipp_nodes::NodeGraph::default();
        let model = super::model::GradeCanvas(&graph, true);
        let actual: Vec<_> = model.add_menu().iter().map(|(label, entries)| (*label, entries.as_slice())).collect();
        assert_eq!(actual, super::ADD_MENU_CATEGORIES);
    }

    #[test]
    fn every_kind_the_add_menu_offers_can_be_added() {
        for (category, entries) in super::ADD_MENU_CATEGORIES {
            for (label, kind) in *entries {
                assert!(klipp_nodes::kinds::addable_node_kind(kind).is_some(), "{category} › {label}: {kind} is not an addable kind");
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod canvas_command_tests {
    use super::*;
    use klipp_nodes::NodeKind;

    /// The grade canvas drawn frame by frame on a graph, its commands
    /// returned, never dispatched (the app's dispatch tests feed them on).
    pub(crate) struct Harness {
        ctx: egui::Context,
        pub(crate) panel: NodeGraphV2Panel,
        pub(crate) graph: NodeGraph,
        clip: ClipId,
        pub(crate) selected: Option<NodeId>,
        pub(crate) set: std::collections::HashSet<NodeId>,
        texts: Vec<(String, egui::Pos2)>,
        cards: Vec<egui::Rect>,
        time: f64,
        /// The keys held through the frames that follow.
        pub(crate) mods: egui::Modifiers,
        /// Every filled rectangle drawn, and its fill.
        fills: Vec<(egui::Rect, egui::Color32)>,
        /// Every filled circle drawn: its centre and fill.
        swatches: Vec<(egui::Pos2, egui::Color32)>,
    }
    impl Harness {
        fn new() -> Self {
            let mut graph = NodeGraph::default();
            graph.ensure_corrector();
            for (i, node) in graph.nodes.iter().enumerate() {
                graph.node_positions.insert(node.id, NodePos { x: (i as f32 - 1.0) * 230.0, y: 0.0 });
            }
            Self::on(graph, ClipId::new())
        }
        /// The canvas showing `graph` for `clip`.
        pub(crate) fn on(graph: NodeGraph, clip: ClipId) -> Self {
            let mut h = Self { ctx: Default::default(), panel: NodeGraphV2Panel { last_fit_clip: Some(clip), zoom: 1.0, ..Default::default() }, graph, clip, selected: None, set: Default::default(), texts: Vec::new(), cards: Vec::new(), time: 0.0, mods: egui::Modifiers::NONE, fills: Vec::new(), swatches: Vec::new() };
            h.settle();
            h
        }
        pub(crate) fn frame(&mut self, events: Vec<egui::Event>) -> Vec<AppCommand> {
            self.time += 1.0 / 30.0;
            let mut cmds = Vec::new();
            let out = self.ctx.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
                time: Some(self.time), events, modifiers: self.mods, ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    cmds = self.panel.show(ui, &mut self.selected, &mut self.set, &mut self.graph, self.clip,
                        &KlippTheme::default(), &Default::default(), 0, &mut Vec::new(), &mut Vec::new(),
                        &[], None, None, 0.0, false, 0.0, None);
                });
            });
            self.texts = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None,
            }).collect();
            self.fills = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) => Some((r.rect, r.fill)), _ => None,
            }).collect();
            self.swatches = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Circle(c) => Some((c.center, c.fill)), _ => None,
            }).collect();
            self.cards = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if (r.rect.width() - NODE_W).abs() < 0.1 && (r.rect.height() - NODE_H).abs() < 0.1 => Some(r.rect), _ => None,
            }).collect();
            cmds
        }
        pub(crate) fn settle(&mut self) { for _ in 0..3 { self.frame(Vec::new()); } }
        pub(crate) fn text(&self, label: &str) -> egui::Pos2 {
            self.texts.iter().rev().find(|(s, _)| s == label).unwrap_or_else(|| panic!("missing {label}: {:?}", self.texts)).1
        }
        pub(crate) fn card(&self, kind: NodeKind) -> (NodeId, egui::Rect) {
            let node = self.graph.nodes.iter().find(|n| n.kind == kind).unwrap();
            let at = self.text(&node.label);
            (node.id, *self.cards.iter().find(|r| r.contains(at)).unwrap_or_else(|| panic!("no card at {at:?}: {:?}", self.cards)))
        }
        fn button(at: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
            egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }
        }
        pub(crate) fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) -> Vec<AppCommand> {
            let mut commands = self.frame(vec![egui::Event::PointerMoved(at), Self::button(at, button, true)]);
            commands.extend(self.frame(vec![Self::button(at, button, false)]));
            commands
        }
        /// The add menu opened at `graph_pos`, searched for `search`, as
        /// the canvas's own gesture leaves it.
        pub(crate) fn open_add_menu(&mut self, search: &str, graph_pos: egui::Pos2) {
            self.panel.add_menu_open = true;
            self.panel.add_menu_screen_pos = egui::pos2(100.0, 100.0);
            self.panel.add_menu_graph_pos = graph_pos;
            self.panel.add_menu_search = search.into();
            self.settle();
        }
        /// The card of node `id`, as drawn.
        pub(crate) fn card_of(&self, id: NodeId) -> egui::Rect {
            let node = self.graph.nodes.iter().find(|n| n.id == id).unwrap();
            let at = self.text(&node.label);
            *self.cards.iter().find(|r| r.contains(at)).unwrap_or_else(|| panic!("no card at {at:?}"))
        }
        /// Node `id`'s input `port`, and its output, as drawn (#1744).
        pub(crate) fn in_port(&self, id: NodeId, port: u8) -> egui::Pos2 {
            let node = self.graph.nodes.iter().find(|n| n.id == id).unwrap();
            input_port_pos(&self.card_of(id), port, node.input_count())
        }
        pub(crate) fn out_port(&self, id: NodeId) -> egui::Pos2 {
            let node = self.graph.nodes.iter().find(|n| n.id == id).unwrap();
            output_port_pos(&self.card_of(id), 0, &node.kind)
        }
        /// The middle of the wire from `from`'s output to `to`'s `port`.
        pub(crate) fn wire_mid(&self, from: NodeId, to: NodeId, port: u8) -> egui::Pos2 {
            bezier_at_phase(self.out_port(from), self.in_port(to, port), 0.5)
        }
        /// A press at `from`, moved in steps to `to`, let go there.
        pub(crate) fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) -> Vec<AppCommand> {
            let mut sent = self.frame(vec![egui::Event::PointerMoved(from), Self::button(from, egui::PointerButton::Primary, true)]);
            for i in 1..=4 {
                sent.extend(self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]));
            }
            sent.extend(self.frame(vec![Self::button(to, egui::PointerButton::Primary, false)]));
            sent
        }
        pub(crate) fn toast(&self) -> Option<String> {
            self.panel.bookmark_toast.as_ref().map(|(t, _)| t.clone())
        }
        pub(crate) fn has(&self, label: &str) -> bool {
            self.texts.iter().any(|(s, _)| s == label)
        }
        /// One key pressed with the held keys (`mods`).
        pub(crate) fn key(&mut self, key: egui::Key) -> Vec<AppCommand> {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: self.mods }])
        }
        pub(crate) fn menu_item(&mut self, at: egui::Pos2, label: &str) -> Vec<AppCommand> {
            self.click(at, egui::PointerButton::Secondary);
            self.settle();
            self.click(self.text(label), egui::PointerButton::Primary)
        }
    }

    /// #1753: a grade node's menu gives it a colour label by the path the
    /// canvas names it by (the grade's group in the clip's picture, then
    /// the node), and a labelled node's card has a band of the colour
    /// along its top; an unlabelled one has none.
    #[test]
    fn a_grade_node_is_labelled_from_its_menu_and_drawn_with_its_colour() {
        let mut h = Harness::new();
        let (id, card) = h.card(NodeKind::Corrector);
        let path = klipp_compose::inspect::grade_node(h.clip, id).path;
        let green = node_label_colour(4).unwrap();
        h.click(card.center(), egui::PointerButton::Secondary);
        h.settle();
        let swatch = h.swatches.iter().rev().find(|(_, fill)| *fill == green).expect("a green swatch in the menu").0;
        let sent = h.click(swatch, egui::PointerButton::Primary);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::SetNodeColourLabel { node, colour: 4 } if *node == path)), "{sent:?}");

        let banded = |h: &Harness| {
            let card = h.card_of(id);
            h.fills.iter().any(|(r, fill)| *fill == green && card.contains_rect(*r) && (r.top() - card.top()).abs() < 0.5)
        };
        h.settle();
        assert!(!banded(&h), "a command alone draws nothing");
        h.panel.node_labels.insert(path, 4);
        h.settle();
        assert!(banded(&h), "the labelled card has its band");
        let other = h.card(NodeKind::Source).1;
        assert!(!h.fills.iter().any(|(r, fill)| *fill == green && other.contains_rect(*r)), "only the labelled node");
    }

    /// A fitted graph is fitted again when its canvas changes size and
    /// the view is as the fit left it: moved from below the viewers to
    /// their right, the dock kept the old framing with most nodes off
    /// it. A view the user zoomed stays as they left it.
    #[test]
    fn an_untouched_fit_follows_the_canvas_size() {
        let mut h = Harness::new();
        let show = |h: &mut Harness, size: egui::Vec2| {
            for _ in 0..2 {
                let _ = h.ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        h.panel.show(ui, &mut h.selected, &mut h.set, &mut h.graph, h.clip,
                            &KlippTheme::default(), &Default::default(), 0, &mut Vec::new(), &mut Vec::new(),
                            &[], None, None, 0.0, false, 0.0, None);
                    });
                });
            }
            h.panel.zoom
        };
        h.panel.request_fit();
        let wide = show(&mut h, egui::vec2(1600.0, 260.0));
        let tall = show(&mut h, egui::vec2(460.0, 1000.0));
        assert!(tall < wide * 0.6, "fitted again for the narrow canvas: {wide} then {tall}");
        h.panel.zoom = 1.7;
        assert_eq!(show(&mut h, egui::vec2(1600.0, 260.0)), 1.7, "a zoomed view is left alone");
    }

    /// After a batch of graph ops (#1905), an untouched view is framed
    /// again, once however many edits asked, and takes in a node the
    /// batch put far off; a view moved by hand stays as it was.
    #[test]
    fn an_edit_refits_an_untouched_view_once() {
        let mut h = Harness::new();
        h.panel.request_fit();
        h.settle();
        let framed = (h.panel.pan, h.panel.zoom);
        // A node far to the right, as a batch might add it.
        let far = h.graph.nodes.last().unwrap().id;
        h.graph.node_positions.insert(far, NodePos { x: 4000.0, y: 900.0 });
        for _ in 0..3 {
            h.panel.refit_if_untouched();
        }
        h.settle();
        assert_eq!(h.panel.edit_refits, 1, "one refit for the batch");
        assert_ne!((h.panel.pan, h.panel.zoom), framed, "framed again to take in the far node");
        // Moved by hand: left alone.
        h.panel.pan += egui::vec2(37.0, 0.0);
        let moved = (h.panel.pan, h.panel.zoom);
        h.graph.node_positions.insert(far, NodePos { x: -4000.0, y: -900.0 });
        h.panel.refit_if_untouched();
        h.settle();
        assert_eq!((h.panel.pan, h.panel.zoom), moved, "a moved view stays");
        assert_eq!(h.panel.edit_refits, 1);
    }

    /// Real pointer interactions, held against the commands from the grade
    /// painter before extracting its model. The graph itself remains read-only.
    #[test]
    fn canvas_commands_are_unchanged_for_the_grade() {
        let mut h = Harness::new();
        let before = serde_json::to_value(&h.graph).unwrap();
        h.panel.add_menu_open = true;
        h.panel.add_menu_screen_pos = egui::pos2(100.0, 100.0);
        h.panel.add_menu_graph_pos = egui::pos2(42.0, 64.0);
        h.panel.add_menu_search = "Exposure".into();
        h.settle();
        let commands = h.click(h.text("Exposure"), egui::PointerButton::Primary);
        assert!(matches!(commands.as_slice(), [AppCommand::AddNode { clip_id, kind, node_id: None, position }] if *clip_id == h.clip && kind == "Exposure" && *position == (42.0, 64.0)), "{commands:?}");
        h.settle();
        let (source, source_rect) = h.card(NodeKind::Source);
        let (output, output_rect) = h.card(NodeKind::Output);
        let from = output_port_pos(&source_rect, 0, &NodeKind::Source);
        let to = input_port_pos(&output_rect, 0, NodeKind::Output.input_port_count());
        h.frame(vec![egui::Event::PointerMoved(from), Harness::button(from, egui::PointerButton::Primary, true)]);
        h.frame(vec![egui::Event::PointerMoved(to)]);
        let commands = h.frame(vec![Harness::button(to, egui::PointerButton::Primary, false)]);
        assert!(matches!(commands.as_slice(), [AppCommand::ConnectNodes { clip_id, from, from_port: 0, to, to_port: 0 }] if *clip_id == h.clip && *from == source && *to == output), "{commands:?}");
        let (corrector, rect) = h.card(NodeKind::Corrector);
        let at = rect.center();
        h.frame(vec![egui::Event::PointerMoved(at), Harness::button(at, egui::PointerButton::Primary, true)]);
        // The drag is one layout gesture (#1528): begun with its first move,
        // ended on release, named by the clip.
        let gesture = klipp_graph_format::path::Root::Clip { clip: h.clip.0 };
        for (i, offset) in [egui::vec2(50.0, 30.0), egui::vec2(100.0, 60.0)].into_iter().enumerate() {
            let commands = h.frame(vec![egui::Event::PointerMoved(at + offset)]);
            let moves = if i == 0 {
                assert!(matches!(commands.first(), Some(AppCommand::BeginNodeLayoutEdit { root }) if *root == gesture), "{commands:?}");
                &commands[1..]
            } else {
                &commands[..]
            };
            assert!(matches!(moves, [AppCommand::MoveNode { clip_id, node_id, delta_x, delta_y }] if *clip_id == h.clip && *node_id == corrector && (*delta_x - 50.0).abs() < 0.01 && (*delta_y - 30.0).abs() < 0.01), "{commands:?}");
        }
        let commands = h.frame(vec![Harness::button(at + egui::vec2(100.0, 60.0), egui::PointerButton::Primary, false)]);
        assert!(matches!(commands.as_slice(), [AppCommand::EndNodeLayoutEdit { root }] if *root == gesture), "{commands:?}");
        assert_eq!(serde_json::to_value(&h.graph).unwrap(), before);

        // Supply a second editable node as a fixture; commands are deliberately
        // not dispatched by this painter test.
        let mut extra = h.graph.nodes.iter().find(|n| n.id == corrector).unwrap().clone();
        extra.id = NodeId::new_v4(); extra.kind = NodeKind::Exposure; extra.label = "Exposure fixture".into();
        let extra_id = extra.id;
        h.graph.node_positions.insert(extra_id, NodePos { x: 0.0, y: 160.0 });
        h.graph.nodes.push(extra);
        h.set = [corrector, extra_id].into_iter().collect(); h.selected = Some(corrector);
        h.settle();
        let before = serde_json::to_value(&h.graph).unwrap();
        let commands = h.menu_item(rect.center(), "Group 2 into Compound");
        assert!(matches!(commands.as_slice(), [AppCommand::GroupIntoCompound { clip_id, node_ids }] if *clip_id == h.clip && node_ids.len() == 2 && node_ids.contains(&corrector) && node_ids.contains(&extra_id)), "{commands:?}");
        assert_eq!(serde_json::to_value(&h.graph).unwrap(), before);

        let node = h.graph.nodes.iter_mut().find(|n| n.id == extra_id).unwrap();
        node.kind = NodeKind::Compound; node.label = "Compound fixture".into();
        node.params.compound_subgraph = Some(NodeGraph::default());
        h.settle();
        let before = serde_json::to_value(&h.graph).unwrap();
        let (_, rect) = h.card(NodeKind::Compound);
        let commands = h.menu_item(rect.center(), "Ungroup Compound");
        assert!(matches!(commands.as_slice(), [AppCommand::UngroupCompound { clip_id, node_id }] if *clip_id == h.clip && *node_id == extra_id), "{commands:?}");
        h.settle();
        // Seed the preceding press to avoid a wall-clock timing dependency.
        h.panel.last_primary_press = Some((std::time::Instant::now(), rect.center()));
        let commands = h.click(rect.center(), egui::PointerButton::Primary);
        assert!(matches!(commands.as_slice(), [AppCommand::DescendIntoCompound { clip_id, node_id }] if *clip_id == h.clip && *node_id == extra_id), "{commands:?}");
        assert_eq!(serde_json::to_value(&h.graph).unwrap(), before);
    }
}

#[cfg(test)]
mod thumbnail_request_tests {
    use super::*;

    #[test]
    fn nested_canvas_requests_visible_nodes_by_full_path_with_selection_first() {
        let mut graph = NodeGraph::default();
        let id = graph.ensure_corrector();
        let prototype = graph.nodes.iter().find(|n| n.id == id).unwrap().clone();
        for _ in 0..70 {
            let mut node = prototype.clone();
            node.id = NodeId::new_v4();
            graph.nodes.push(node);
        }
        let hidden = graph.nodes.last().unwrap().id;
        graph.node_positions.insert(hidden, NodePos { x: 100_000.0, y: 0.0 });
        let selected = graph.nodes[graph.nodes.len() - 2].id;
        let clip = ClipId::new();
        let group = NodeId::new_v4();
        let mut panel = NodeGraphV2Panel { last_fit_clip: Some(clip), thumbnail_quality: Some(klipp_engine::node_thumbnails::PreviewQuality::Filmstrip), ..Default::default() };
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = ctx.run(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            ..Default::default()
        }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                commands = panel.show(ui, &mut Some(selected), &mut [selected].into(), &mut graph, clip,
                    &KlippTheme::default(), &Default::default(), 0, &mut Vec::new(), &mut vec![group],
                    &["Group".into()], Some(&[]), None, 0.0, false, 0.0, None);
            });
        });
        let wanted = commands.iter().find_map(|cmd| match cmd {
            AppCommand::SetVisibleNodeThumbs { at, .. } => Some(at), _ => None,
        }).expect("the canvas requests its previews");
        assert_eq!(wanted.len(), klipp_engine::node_thumbnails::MAX_THUMBS);
        assert_eq!(wanted[0].path.node(), Some(selected));
        assert!(wanted.iter().all(|at| at.path.nodes.len() == 3 && at.path.nodes[1] == group));
        assert!(wanted.iter().all(|at| at.path.root_clip() == Some(clip.0)));
        assert!(!wanted.iter().any(|at| at.path.node() == Some(hidden)));
        assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "Low-res preview")));
    }
}

#[cfg(test)]
mod editing_tests {
    //! The node editor's own editing (#1745), on the grade canvas drawn
    //! frame by frame: its keys, its menus, the pan and the grid.
    use super::canvas_command_tests::Harness;
    use super::*;
    use klipp_nodes::NodeKind;

    fn button(at: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// A canvas whose Corrector is selected, the pointer over empty canvas.
    fn canvas() -> (Harness, NodeId, egui::Pos2) {
        let mut graph = NodeGraph::default();
        graph.ensure_corrector();
        for (i, node) in graph.nodes.iter().enumerate() {
            graph.node_positions.insert(node.id, NodePos { x: (i as f32 - 1.0) * 230.0, y: 0.0 });
        }
        let clip = ClipId::new();
        let mut h = Harness::on(graph, clip);
        let (corrector, rect) = h.card(NodeKind::Corrector);
        h.selected = Some(corrector);
        h.set = [corrector].into_iter().collect();
        let empty = rect.center() + egui::vec2(0.0, 200.0);
        h.frame(vec![egui::Event::PointerMoved(empty)]);
        (h, corrector, empty)
    }

    /// Ctrl+D duplicates the selection, Ctrl+C copies it and Ctrl+V
    /// pastes at the pointer; Source and Output are never taken. With
    /// no Ctrl they do nothing.
    #[test]
    fn ctrl_d_c_and_v_duplicate_copy_and_paste() {
        let (mut h, corrector, _) = canvas();
        let source = h.graph.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        h.set.insert(source);
        h.mods = egui::Modifiers::COMMAND;
        let sent = h.key(egui::Key::D);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::DuplicateNodes { node_ids, .. } if *node_ids == [corrector])), "{sent:?}");
        let sent = h.key(egui::Key::C);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::CopyNodes { node_ids, .. } if *node_ids == [corrector])), "{sent:?}");
        let sent = h.key(egui::Key::V);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::PasteNodes { .. })), "{sent:?}");
        h.mods = egui::Modifiers::NONE;
        for key in [egui::Key::D, egui::Key::C, egui::Key::V] {
            let sent = h.key(key);
            assert!(!sent.iter().any(|c| matches!(c, AppCommand::DuplicateNodes { .. } | AppCommand::CopyNodes { .. } | AppCommand::PasteNodes { .. })), "{key:?}: {sent:?}");
        }
        // An unfocused canvas takes none of them.
        h.panel.grade_keys = false;
        h.mods = egui::Modifiers::COMMAND;
        assert!(!h.key(egui::Key::D).iter().any(|c| matches!(c, AppCommand::DuplicateNodes { .. })));
    }

    /// Tab, or Shift+A, opens the add menu where the pointer is, its
    /// search taking the keys: typed and Enter, the first match is added
    /// there. The pick is a recent one, kept for the next session.
    #[test]
    fn tab_and_shift_a_open_the_add_menu_at_the_pointer() {
        let (mut h, _, at) = canvas();
        h.key(egui::Key::Tab);
        assert!(h.panel.add_menu_open, "Tab opens the add menu");
        assert_eq!(h.panel.add_menu_screen_pos, at, "at the pointer");
        h.settle();
        h.frame(vec![egui::Event::Text("satur".into())]);
        let sent = h.key(egui::Key::Enter);
        let graph_pos = h.panel.add_menu_graph_pos;
        assert!(sent.iter().any(|c| matches!(c, AppCommand::AddNode { kind, position, .. } if kind == "Saturation" && *position == (graph_pos.x, graph_pos.y))), "{sent:?}");
        assert!(!h.panel.add_menu_open, "the pick closes it");
        assert!(h.panel.take_recents_changed() && h.panel.recent_kinds() == ["Saturation"]);

        h.mods = egui::Modifiers::SHIFT;
        h.key(egui::Key::A);
        assert!(h.panel.add_menu_open, "Shift+A opens it too");

        // The next session's canvas starts with the pick as a recent one.
        let mut next = NodeGraphV2Panel::default();
        next.restore_recents(&h.panel.recent_kinds());
        assert_eq!(next.recent_kinds(), ["Saturation"]);
        next.restore_recents(&["no_such_kind".to_owned()]);
        assert!(next.recent_kinds().is_empty(), "a kind no menu offers is dropped");
    }

    /// Shift+drag on empty canvas draws a marquee and never pans: it
    /// panned under the marquee. Space+drag pans (the control).
    #[test]
    fn shift_drag_selects_without_panning() {
        let (mut h, _, at) = canvas();
        h.set.clear();
        h.selected = None;
        let pan = h.panel.pan;
        h.mods = egui::Modifiers::SHIFT;
        h.frame(vec![egui::Event::PointerMoved(at), button(at, egui::PointerButton::Primary, true)]);
        for step in [30.0, 60.0, 90.0] {
            h.frame(vec![egui::Event::PointerMoved(at + egui::vec2(step, -step * 2.0))]);
        }
        h.frame(vec![button(at + egui::vec2(90.0, -180.0), egui::PointerButton::Primary, false)]);
        assert_eq!(h.panel.pan, pan, "Shift-drag did not pan");

        h.mods = egui::Modifiers::NONE;
        let space = |pressed| egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        h.frame(vec![space(true), egui::Event::PointerMoved(at), button(at, egui::PointerButton::Primary, true)]);
        for step in [30.0, 60.0] {
            h.frame(vec![egui::Event::PointerMoved(at + egui::vec2(step, 0.0))]);
        }
        h.frame(vec![button(at + egui::vec2(60.0, 0.0), egui::PointerButton::Primary, false), space(false)]);
        assert_ne!(h.panel.pan, pan, "Space-drag pans");
    }

    /// With Snap on, a dragged node lands on the grid; off, where it was
    /// let go.
    #[test]
    fn snap_puts_a_dragged_node_on_the_grid() {
        for snap in [true, false] {
            let (mut h, corrector, _) = canvas();
            if snap {
                h.click(h.text("Snap"), egui::PointerButton::Primary);
                assert!(h.panel.snap_to_grid);
            }
            let start = h.graph.node_positions[&corrector];
            let (_, rect) = h.card(NodeKind::Corrector);
            let at = rect.center();
            let apply = |h: &mut Harness, cmds: Vec<AppCommand>| {
                for c in cmds {
                    if let AppCommand::MoveNode { node_id, delta_x, delta_y, .. } = c {
                        let p = h.graph.node_positions.get_mut(&node_id).unwrap();
                        p.x += delta_x;
                        p.y += delta_y;
                    }
                }
            };
            let cmds = h.frame(vec![egui::Event::PointerMoved(at), button(at, egui::PointerButton::Primary, true)]);
            apply(&mut h, cmds);
            for step in [egui::vec2(20.0, 7.0), egui::vec2(41.0, 13.0)] {
                let cmds = h.frame(vec![egui::Event::PointerMoved(at + step)]);
                apply(&mut h, cmds);
            }
            let cmds = h.frame(vec![button(at + egui::vec2(41.0, 13.0), egui::PointerButton::Primary, false)]);
            apply(&mut h, cmds);
            let end = h.graph.node_positions[&corrector];
            if snap {
                assert_eq!((end.x % GRID_SPACING, end.y % GRID_SPACING), (0.0, 0.0), "{start:?} to {end:?}");
            } else {
                assert!((end.x - start.x - 41.0 / h.panel.zoom).abs() < 0.01, "{start:?} to {end:?}");
            }
        }
    }

    /// A correction a key gates offers its outside node (#898) from its
    /// menu, sending the command for it; one no key gates doesn't offer it.
    #[test]
    fn a_keyed_correction_offers_its_outside_node() {
        let (mut h, corrector, _) = canvas();
        let (_, rect) = h.card(NodeKind::Corrector);
        h.click(rect.center(), egui::PointerButton::Secondary);
        h.settle();
        assert!(!h.has("Add Outside Node"), "no key, no outside node");
        h.key(egui::Key::Escape);
        h.settle();
        let mut graph = h.graph.clone();
        let (key, keyed) = graph.ensure_masked_secondary(NodeKind::Corrector);
        assert!(graph.input_of(keyed, 1) == Some(key), "keyed");
        for (id, y) in [(key, -220.0), (keyed, 220.0)] {
            graph.node_positions.entry(id).or_insert(NodePos { x: 0.0, y });
        }
        let _ = corrector;
        let mut h = Harness::on(graph, ClipId::new());
        let rect = h.card_of(keyed);
        let sent = h.menu_item(rect.center(), "Add Outside Node");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::AddOutsideNode { node_id, .. } if *node_id == keyed)), "{sent:?}");
    }

    /// The node menu offers the node's editing, each sending its command:
    /// Duplicate, Copy, Reset to defaults, Show in Inspector (which also
    /// selects it); "Replace with…" opens the add menu for it, and a pick
    /// there replaces rather than adds.
    #[test]
    fn the_node_menu_edits_the_node() {
        let (mut h, corrector, _) = canvas();
        let (_, rect) = h.card(NodeKind::Corrector);
        let sent = h.menu_item(rect.center(), "Duplicate");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::DuplicateNodes { node_ids, .. } if *node_ids == [corrector])), "{sent:?}");
        h.settle();
        let sent = h.menu_item(rect.center(), "Copy");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::CopyNodes { .. })), "{sent:?}");
        h.settle();
        let sent = h.menu_item(rect.center(), "Reset to defaults");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::ResetNode { node_id, .. } if *node_id == corrector)), "{sent:?}");
        h.settle();
        h.selected = None;
        h.set.clear();
        let sent = h.menu_item(rect.center(), "Show in Inspector");
        assert!(sent.iter().any(|c| matches!(c, AppCommand::ShowPanel { inspector_tab: Some(t), .. } if t == "color")), "{sent:?}");
        assert_eq!(h.selected, Some(corrector));
        h.settle();
        h.menu_item(rect.center(), "Replace with\u{2026}");
        h.settle();
        assert!(h.panel.add_menu_open && h.has("Replace with\u{2026}"), "the add menu, for the replace");
        h.frame(vec![egui::Event::Text("satur".into())]);
        let sent = h.key(egui::Key::Enter);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::ReplaceNode { node_id, kind, .. } if *node_id == corrector && kind == "Saturation")), "{sent:?}");
        assert!(!sent.iter().any(|c| matches!(c, AppCommand::AddNode { .. })));
    }
}

