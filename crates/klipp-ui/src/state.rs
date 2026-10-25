use klipp_timeline::GradeAccess;
use crate::history::{CommandHistory, EditCommand, HistoryEntry};

/// Of a sequence's stamp and the project's, the history whose step moved
/// later (#1986).
fn later(sequence: Option<u64>, project: Option<u64>) -> Option<crate::history::Scope> {
    match (sequence, project) {
        (None, None) => None,
        (Some(_), None) => Some(crate::history::Scope::Sequence),
        (None, Some(_)) => Some(crate::history::Scope::Project),
        (Some(s), Some(p)) => Some(if p > s { crate::history::Scope::Project } else { crate::history::Scope::Sequence }),
    }
}
use klipp_core::{AudioParam, ClipId, Fps, MediaId, SequenceId, Timecode, Tool, TrackId, TransitionId, VideoParam, ViewerMode};
use klipp_engine::PlaybackState;
use klipp_media::{MediaPool, ThumbnailCache};
use klipp_timeline::Sequence;
use std::path::PathBuf;

/// Identifies which panel is currently the user's primary interaction
/// target — set by panels via [`note_active_panel`] in their update
/// fn and consulted by the keybinding dispatcher to scope shortcuts
/// (e.g. Ctrl+C should mean "copy colour nodes" only when the node
/// graph or color inspector is active, not when the user happens to
/// have the timeline panel's container under focus).
///
/// Variants exist for every panel that has — or might have — a
/// panel-scoped keybinding. Adding a new panel without adding a
/// variant here is fine; it just won't be addressable as a key-gate
/// target until the variant is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    NodeGraph,
    /// Picture canvas: its shortcuts must not edit timeline clips.
    PictureGraph,
    NodeInspector,
    /// The whole browser dock — Files, Media, and Library tabs are
    /// the same widget so they share one panel kind. The browser's
    /// internal active_browser_tab + library_category state picks
    /// which TextEdit a Ctrl+F focus request lands on.
    Browser,
    Timeline,
    Viewer,
    Inspector,
    AudioMixer,
    SubtitleEditor,
    LooksPanel,
    Markers,
    CurveEditor,
    Transport,
    Toolbar,
    StatusBar,
    Palette,
}

/// Mark `kind` as the active panel when the user clicks or drags
/// inside `panel_rect` this frame. Called once per frame from each
/// panel's update fn against its outer rect (typically `ui.max_rect()`).
/// Detection is "did any pointer press start inside the rect this
/// frame", which catches clicks on inner widgets (nodes, buttons,
/// dropdowns) as well as raw clicks on empty panel space — i.e. any
/// pointer interaction that the user reasonably intends as "I'm
/// focusing on this panel now".
///
/// The setter never CLEARS the active panel — once a panel claims
/// focus it stays active until another panel claims it. That matches
/// the user's mental model: keyboard focus persists across mouse
/// movement (e.g. Ctrl+C should still mean "copy colour nodes" when
/// the user has clicked the node graph and is now hovering the
/// inspector to read details).
pub fn note_active_panel(
    state: &mut AppState,
    kind: PanelKind,
    ctx: &egui::Context,
    panel_rect: egui::Rect,
) {
    let pressed_inside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.interact_pos().is_some_and(|p| panel_rect.contains(p))
    });
    if pressed_inside {
        state.active_panel = Some(kind);
    }
}

/// Subgraph snapshot held in the grade-node copy/paste clipboard.
///
/// Edges with both endpoints inside the copied node set are
/// preserved; edges with one endpoint outside the selection are
/// dropped (the half-attached edge would dangle on paste). The
/// `node_positions` map keeps relative layout so a multi-node paste
/// reproduces the source clip's arrangement, offset by a default
/// delta so it doesn't stack on top of existing graph nodes.
#[derive(Debug, Clone)]
pub struct GradeNodeClipboard {
    pub nodes: Vec<klipp_nodes::Node>,
    pub edges: Vec<klipp_nodes::Edge>,
    pub node_positions: std::collections::HashMap<klipp_nodes::NodeId, klipp_nodes::NodePos>,
}

impl GradeNodeClipboard {
    /// Extract the subgraph induced by `selection` from `graph` —
    /// i.e. the nodes whose ids are in `selection`, the edges whose
    /// BOTH endpoints are in `selection`, and the positions for
    /// those nodes. Pure — testable without app state.
    pub fn from_selection(
        graph: &klipp_nodes::NodeGraph,
        selection: &std::collections::HashSet<klipp_nodes::NodeId>,
    ) -> Self {
        let nodes: Vec<klipp_nodes::Node> = graph.nodes.iter()
            .filter(|n| selection.contains(&n.id))
            .cloned()
            .collect();
        let edges: Vec<klipp_nodes::Edge> = graph.edges.iter()
            .filter(|e| selection.contains(&e.from_node) && selection.contains(&e.to_node))
            .cloned()
            .collect();
        let node_positions: std::collections::HashMap<_, _> = graph.node_positions.iter()
            .filter(|(id, _)| selection.contains(id))
            .map(|(id, pos)| (*id, *pos))
            .collect();
        Self { nodes, edges, node_positions }
    }

    /// Merge `self` into `target`, allocating fresh ids for every
    /// pasted node and rewriting Edge endpoints + positions via the
    /// (old → new) remap. `position_offset` shifts every pasted
    /// position so the new nodes don't stack exactly on top of any
    /// existing nodes. Returns the number of nodes appended (always
    /// equal to `self.nodes.len()` unless caller mutates between).
    pub fn merge_into(
        self,
        target: &mut klipp_nodes::NodeGraph,
        position_offset: (f32, f32),
    ) -> usize {
        self.merge_into_ids(target, position_offset).len()
    }

    /// [`Self::merge_into`], returning the new nodes' ids in the
    /// clipboard's order, for the editor to select them (#1745).
    pub fn merge_into_ids(
        self,
        target: &mut klipp_nodes::NodeGraph,
        position_offset: (f32, f32),
    ) -> Vec<klipp_nodes::NodeId> {
        use std::collections::HashMap;
        let remap: HashMap<klipp_nodes::NodeId, klipp_nodes::NodeId> = self.nodes.iter()
            .map(|n| (n.id, klipp_nodes::NodeId::new_v4()))
            .collect();
        let ids: Vec<klipp_nodes::NodeId> = self.nodes.iter().map(|n| remap[&n.id]).collect();
        for node in self.nodes.into_iter() {
            let new_id = remap[&node.id];
            let mut new_node = node;
            new_node.id = new_id;
            target.nodes.push(new_node);
        }
        for edge in self.edges.into_iter() {
            let (Some(&new_from), Some(&new_to)) =
                (remap.get(&edge.from_node), remap.get(&edge.to_node))
            else { continue };
            target.edges.push(klipp_nodes::Edge {
                from_node: new_from,
                from_port: edge.from_port,
                to_node: new_to,
                to_port: edge.to_port,
            });
        }
        for (old_id, pos) in self.node_positions.into_iter() {
            if let Some(&new_id) = remap.get(&old_id) {
                target.node_positions.insert(new_id, klipp_nodes::NodePos {
                    x: pos.x + position_offset.0,
                    y: pos.y + position_offset.1,
                });
            }
        }
        ids
    }
}

#[derive(Debug, Clone)]
pub struct BrowserClipSelection {
    pub media_id: MediaId,
    pub range_in_secs: f64,
    pub range_out_secs: f64,
}

/// Which transform handle is being dragged in the viewer
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformHandle {
    Position,
    ScaleTopLeft,
    ScaleTopRight,
    ScaleBottomLeft,
    ScaleBottomRight,
    Rotation,
}

/// State for viewer transform control interaction
#[derive(Debug, Clone)]
pub struct ViewerTransformDrag {
    pub clip_id: klipp_core::ClipId,
    pub effect_index: usize,
    pub handle: TransformHandle,
    pub initial_transform: crate::inspector::video_tab::TransformParams,
    pub drag_start_pos: egui::Pos2,
    /// Whether the pointer has moved far enough for this to be a drag.
    /// A press alone changes nothing: the Transform effect is added, and
    /// the undo gesture opened, only once it has.
    pub started: bool,
    /// Whether this drag added the Transform effect: the clip had none
    /// when the pointer crossed the threshold. A drag taken back then
    /// removes it again.
    pub added: bool,
}

/// The viewer's surface tool (#684 phase 2): four corners being set on a
/// surface in a clip, in fractions of the frame as it is shown, before a
/// track is run from them.
#[derive(Debug, Clone)]
pub struct SurfaceTool {
    pub clip_id: klipp_core::ClipId,
    /// The surface tracked again or continued; `None` makes a new one.
    pub surface: Option<uuid::Uuid>,
    pub name: Option<String>,
    pub corners: [[f32; 2]; 4],
    /// 1 or -1 continues the track only later or only earlier from here,
    /// laid over the rest of it; 0 tracks both ways.
    pub direction: i8,
    /// The corner held, or 4 for the whole quad, and where the press was.
    pub drag: Option<(usize, egui::Pos2, [[f32; 2]; 4])>,
}

/// A corner-pin handle being dragged in the viewer: which corner, and
/// the four as they were when the drag last wrote them.
#[derive(Debug, Clone, Copy)]
pub struct ViewerCornerPinDrag {
    pub clip_id: klipp_core::ClipId,
    pub effect_index: usize,
    pub corner: usize,
    pub corners: [[f32; 2]; 4],
    pub drag_start_pos: egui::Pos2,
    /// Whether the pointer has moved far enough for this to be a drag;
    /// the undo gesture opens only once it has.
    pub started: bool,
}

/// Which part of a Title-object's bounding box is being dragged.
/// Used by the on-canvas title object editor; mirrors the standard
/// 8-handle bounding box from pro NLEs (corner = font scale,
/// edge = box resize, plus a rotation handle and the box body for
/// translation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TitleObjectHandle {
    /// Drag inside the box body — translates `transform.center`.
    Body,
    CornerTopLeft,
    CornerTopRight,
    CornerBottomLeft,
    CornerBottomRight,
    EdgeTop,
    EdgeBottom,
    EdgeLeft,
    EdgeRight,
    /// Stalk above the top-centre — rotates around the anchor.
    Rotation,
    /// Crosshair dot inside the box — moves the rotation/anchor
    /// pivot without affecting the object's visible position.
    Anchor,
    /// Where a text set on an arc starts (#1714): turns the arc's start
    /// angle about the layer's centre.
    PathStart,
}

#[derive(Debug, Clone)]
pub struct TitleObjectDrag {
    pub clip_id: klipp_core::ClipId,
    pub object_id: klipp_timeline::TitleObjectId,
    pub handle: TitleObjectHandle,
    /// Object transform at the moment the drag started; the ongoing
    /// drag mutates relative to this baseline so jitter / round-trip
    /// drift doesn't accumulate.
    pub initial_transform: klipp_timeline::ObjectTransform,
    /// For corner / font-scale resizes, the TextBox font_size at
    /// drag start. Stored separately so we can apply the scale to
    /// the typography rather than the bounding box only.
    pub initial_font_size: f32,
    pub drag_start_pos: egui::Pos2,
    /// The picture's width over its height on screen, so a turn and a
    /// resize of a turned box follow the pointer as it is seen (#1714).
    pub frame_aspect: f32,
}

/// Which part of a mask shape is being dragged in the viewer
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaskHandle {
    Center,
    EdgeRight,
    EdgeBottom,
    /// Bezier control point by index.
    BezierPoint(usize),
}

/// State for viewer mask handle interaction
#[derive(Debug, Clone)]
pub struct ViewerMaskDrag {
    pub clip_id: klipp_core::ClipId,
    pub node_id: klipp_nodes::NodeId,
    pub mask_index: usize,
    pub handle: MaskHandle,
    pub initial_shape: klipp_core::MaskShape,
    pub drag_start_pos: egui::Pos2,
}

/// What the audio rubber band displays — either a clip-level audio param
/// (Volume/Pan/Gain, the legacy three) or a per-effect param (any
/// keyframable param on any audio effect, identified by effect index +
/// param name). Reserved param names "__bypass" and "__wetdry" route
/// to the slot's bypass-keyframe and wet/dry-keyframe tracks.
#[derive(Debug, Clone, PartialEq)]
pub enum RubberBandParam {
    /// Clip-level audio param (Volume/Pan/Gain). Carries the legacy
    /// `AudioParam` so existing AddAudioKeyframe / MoveAudioKeyframe
    /// dispatches keep working unchanged.
    ClipParam(AudioParam),
    /// Per-effect param identified by `(effect_index, param_name)` —
    /// keyed the same way as `clip.audio_effect_keyframes` map keys.
    EffectParam {
        effect_index: usize,
        param_name: String,
    },
}

impl Default for RubberBandParam {
    fn default() -> Self {
        Self::ClipParam(AudioParam::Volume)
    }
}

impl RubberBandParam {
    /// If this is a clip-level param, return the underlying `AudioParam`.
    /// Used at sites that still operate purely on clip kfs.
    pub fn as_clip_param(&self) -> Option<AudioParam> {
        match self {
            Self::ClipParam(p) => Some(*p),
            Self::EffectParam { .. } => None,
        }
    }

    /// True when the rubber band is showing a per-effect param.
    pub fn is_effect(&self) -> bool {
        matches!(self, Self::EffectParam { .. })
    }

    /// Map an EffectParam variant back to the
    /// "{effect_index}:{param_name}" key used in
    /// `clip.audio_effect_keyframes`. Returns None for ClipParam.
    pub fn effect_key(&self) -> Option<String> {
        match self {
            Self::EffectParam { effect_index, param_name } => {
                Some(format!("{effect_index}:{param_name}"))
            }
            Self::ClipParam(_) => None,
        }
    }
}

/// State during an active audio keyframe drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct DraggingKeyframe {
    pub clip_id: ClipId,
    pub param: AudioParam,
    pub keyframe_index: usize,
    /// True when the drag was initiated inside the per-track expandable
    /// automation lane. Drag handlers use this to map pointer
    /// Y against the lane sub-rect rather than the clip rect, so the
    /// diamond stays under the cursor regardless of which surface
    /// started the gesture.
    pub in_automation_lane: bool,
}

/// State during an active audio EFFECT keyframe drag on the timeline
/// rubber band. Parallel to `DraggingKeyframe` for clip-level kfs;
/// kept separate so the existing clip-kf drag handler doesn't grow a
/// branch on every step. Effect kfs are stored in
/// `clip.audio_effect_keyframes["{effect_index}:{param_name}"]`.
#[derive(Debug, Clone)]
pub struct DraggingEffectKeyframe {
    pub clip_id: ClipId,
    pub effect_index: usize,
    pub param_name: String,
    pub keyframe_index: usize,
}

/// State during an active Bezier tangent handle drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct BezierHandleDrag {
    pub clip_id: ClipId,
    pub param: AudioParam,
    pub keyframe_index: usize,
    /// true = tangent_out (right handle), false = tangent_in (left handle)
    pub is_out: bool,
    /// See [`DraggingKeyframe::in_automation_lane`].
    pub in_automation_lane: bool,
}

/// State during an active Bezier tangent handle drag on an audio EFFECT
/// keyframe (rubber band rendering an effect param). Parallel to
/// `BezierHandleDrag` for the same reason `DraggingEffectKeyframe` is.
#[derive(Debug, Clone)]
pub struct BezierEffectHandleDrag {
    pub clip_id: ClipId,
    pub effect_index: usize,
    pub param_name: String,
    pub keyframe_index: usize,
    pub is_out: bool,
}

/// State during an active Bezier tangent handle drag on the video timeline rubber band.
#[derive(Debug, Clone)]
pub struct BezierVideoHandleDrag {
    pub clip_id: ClipId,
    pub param: VideoParam,
    pub keyframe_index: usize,
    /// true = tangent_out (right handle), false = tangent_in (left handle)
    pub is_out: bool,
}

/// State during an active video keyframe drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct DraggingVideoKeyframe {
    pub clip_id: ClipId,
    pub param: VideoParam,
    pub keyframe_index: usize,
}

/// Clipboard for keyframe curves (copy/paste between clips/params).
#[derive(Debug, Clone)]
pub struct KeyframeClipboard {
    /// The copied keyframes.
    pub keyframes: Vec<klipp_core::Keyframe>,
    /// Duration of the source clip (for time scaling when pasting to different-length clips).
    pub source_duration_secs: f64,
}

/// Which kind of parameter a keyframe context menu or selection refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyframeParamKind {
    Title { object_id: u32, param: crate::panels::title_curve::TitleCurveParam },
    Audio(AudioParam),
    Video(VideoParam),
    /// Audio EFFECT param keyframe — identified by effect index +
    /// param name, same as `audio_effect_keyframes` HashMap keys.
    /// Reserved keys `__bypass` / `__wetdry` route here too.
    AudioEffect { effect_index: usize, param_name: String },
    /// An effect node's parameter, by the node's path and its key (M3
    /// step 3).
    Node { path: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// A mask's parameter, or a freeform mask's shape (#1947).
    Mask(crate::mask_tracks::MaskTrack),
}

/// State for the keyframe interpolation context menu (right-click on diamond).
#[derive(Debug, Clone)]
pub struct KeyframeContextMenu {
    pub clip_id: ClipId,
    pub param_kind: KeyframeParamKind,
    pub keyframe_index: usize,
    /// Screen position where the menu was opened.
    pub screen_pos: egui::Pos2,
}

/// Per-track automation-lane param picker. Stored on AppState so the
/// popup survives across frames (egui Areas need stable IDs and a
/// multi-frame layout pass before their rect is known — an inline
/// popup would close on the same right-click that opened it).
#[derive(Debug, Clone)]
pub struct AutomationLaneParamMenu {
    pub track_idx: usize,
    pub screen_pos: egui::Pos2,
}

/// A key dragged on a clip's parameter lane (#2307): its clip and track,
/// its time now, the values the lane spanned when the drag began (kept
/// so the key stays under the pointer), and whether its step has begun.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamLaneDrag {
    pub clip_id: ClipId,
    pub kind: KeyframeParamKind,
    pub at: f64,
    pub span: (f32, f32),
    pub began: bool,
}

/// A selected keyframe on the timeline rubber band.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedKeyframe {
    pub clip_id: ClipId,
    pub param_kind: KeyframeParamKind,
    pub keyframe_index: usize,
}

/// One of the sequence camera's keys, named by the list it is in and
/// the moment it is stored at rather than by its index: a drag re-sorts
/// the list every frame, and a moment survives that where an index
/// does not. The lane and the curve editor share this selection; it
/// belongs to no clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraKeyRef {
    pub param: klipp_core::CameraParam,
    pub time_secs: f64,
}

/// The right-click menu on a camera key, wherever it was opened.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraKeyMenu {
    pub key: CameraKeyRef,
    pub screen_pos: egui::Pos2,
}

/// A drag of a camera key or of one of its Bezier handles, in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DraggingCameraKey {
    pub key: CameraKeyRef,
    /// `None` drags the key itself; `Some(true)` its out-handle,
    /// `Some(false)` its in-handle.
    pub handle: Option<bool>,
}

/// State during an active range handle drag or ruler range drag.
#[derive(Debug, Clone)]
pub struct RangeDragState {
    pub kind: RangeDragKind,
    pub original_in: Option<Timecode>,
    pub original_out: Option<Timecode>,
}

/// What kind of range drag is in progress.
#[derive(Debug, Clone, Copy)]
pub enum RangeDragKind {
    /// Dragging the in-point handle.
    InHandle,
    /// Dragging the out-point handle.
    OutHandle,
    /// Dragging on empty ruler to create a new range.
    RulerCreate { anchor_secs: f64 },
}


/// What one timeline of two stacked ones holds of its own (#82): the
/// sequence it shows and its playhead, its zoom and scroll, what is
/// selected and hovered on it, and the drags begun on it. The timeline
/// being worked in keeps these in the app's state as ever; the other's
/// are parked here, and [`TimelineView::swap`] trades them, so every
/// handler goes on reading the one being worked in.
macro_rules! timeline_view {
    ($($field:ident: $ty:ty = $init:expr),* $(,)?) => {
        pub struct TimelineView {
            $(pub $field: $ty,)*
        }

        impl TimelineView {
            /// Every field a timeline holds of its own, by name: for the
            /// test that no other per-timeline field is left shared.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];

            /// A timeline's own state, nothing selected or dragged, showing
            /// sequence `sequence_idx` at its `fps` from `playhead` at `zoom`.
            pub fn fresh(sequence_idx: usize, fps: Fps, playhead: Timecode, zoom: f32) -> Self {
                let mut view = TimelineView { $($field: $init,)* };
                view.active_sequence_idx = sequence_idx;
                view.fps = fps;
                view.playhead = playhead;
                view.playhead_anchor = playhead;
                view.playhead_smooth_secs = playhead.to_secs_f64();
                view.timeline_zoom = zoom;
                view
            }

            /// This view's fields and `state`'s traded.
            pub fn swap(&mut self, state: &mut AppState) {
                $(std::mem::swap(&mut self.$field, &mut state.$field);)*
                state.selected_clip_loc_cache.set(None);
            }
        }
    };
}

timeline_view! {
    active_sequence_idx: usize = 0,
    fps: Fps = Fps::new(24, 1),
    compound_clip_stack: Vec<(usize, klipp_core::Timecode)> = Vec::new(),
    playhead: Timecode = Timecode::zero(),
    playhead_smooth_secs: f64 = 0.0,
    playhead_anchor: Timecode = Timecode::zero(),
    timeline_zoom: f32 = 50.0,
    timeline_scroll_x: f32 = 0.0,
    timeline_scroll_y: f32 = 0.0,
    timeline_resizing_track: Option<(usize, f32, f32)> = None,
    timeline_track_pre_collapse: std::collections::HashMap<usize, f32> = Default::default(),
    ruler_press_origin: Option<egui::Pos2> = None,
    last_timeline_tracks_height: f32 = 0.0,
    last_timeline_content_width: f32 = 0.0,
    skim_speed_px_per_sec: f32 = 0.0,
    skim_last_update: std::time::Instant = std::time::Instant::now(),
    skim_last_x: f32 = 0.0,
    program_skim_active: bool = false,
    program_skim_revert_deadline: Option<std::time::Instant> = None,
    selected_clip: Option<ClipId> = None,
    selected_clips: Vec<ClipId> = Vec::new(),
    selected_edits: Vec<crate::edit_point::EditPoint> = Vec::new(),
    selected_gap: Option<(usize, Timecode, Timecode)> = None,
    selected_transition: Option<(usize, TransitionId)> = None,
    hovered_clip: Option<ClipId> = None,
    trim_entry: Option<String> = None,
    dynamic_trim: Option<Timecode> = None,
    range_drag: Option<RangeDragState> = None,
    range_snap_line_x: Option<f32> = None,
    selected_keyframe: Option<SelectedKeyframe> = None,
    key_selection: crate::key_selection::KeySelection = Default::default(),
    multi_key_drag: Option<crate::key_selection::MultiKeyDrag> = None,
    timeline_mask_lane: Option<(ClipId, crate::mask_tracks::MaskTrack)> = None,
    mask_lane_drag: Option<(ClipId, crate::mask_tracks::MaskTrack, f64, bool)> = None,
    timeline_param_lane: Option<(ClipId, KeyframeParamKind)> = None,
    param_lane_drag: Option<ParamLaneDrag> = None,
    selected_camera_key: Option<CameraKeyRef> = None,
    dragging_camera_key: Option<DraggingCameraKey> = None,
    dragging_audio_keyframe: Option<DraggingKeyframe> = None,
    dragging_video_keyframe: Option<DraggingVideoKeyframe> = None,
    dragging_rubber_band: Option<ClipId> = None,
    dragging_rubber_band_in_lane: bool = false,
    dragging_bezier_handle: Option<BezierHandleDrag> = None,
    dragging_video_bezier_handle: Option<BezierVideoHandleDrag> = None,
    dragging_audio_effect_keyframe: Option<DraggingEffectKeyframe> = None,
    dragging_audio_effect_bezier_handle: Option<BezierEffectHandleDrag> = None,
    dragging_fade_handle: Option<(ClipId, bool, f32)> = None,
    frame_glide_drag: Option<crate::panels::frame_glide_tool::GlideDrag> = None,
}

impl AppState {
    /// Make sequence `sequence_idx` the one shown, as its tab does: its tab
    /// opened, this playhead kept on the sequence left and the other's
    /// taken back, the rate the sequence's. A tab clears the selection and
    /// the way back out of a compound clip; `keep_view` keeps them, for a
    /// stacked timeline that brings its own (#82). Whether it switched (the
    /// app then stops playback and gives the engine the sequence).
    pub fn switch_sequence(&mut self, sequence_idx: usize, keep_view: bool) -> bool {
        // Switching to a sequence opens its tab again (#906).
        if let Some(seq) = self.sequences.get(sequence_idx) {
            let id = seq.id;
            self.closed_sequence_tabs.remove(&id);
        }
        if sequence_idx >= self.sequences.len() || sequence_idx == self.active_sequence_idx {
            return false;
        }
        // The playhead kept on the sequence left.
        self.sequences[self.active_sequence_idx].last_playhead = Some(klipp_core::RationalTime { value: self.playhead.value, rate: self.playhead.rate });
        // A parameter drag in flight on the sequence left can't be
        // finished here: its pending step is abandoned, so the next push
        // there does not clobber whatever is pending on the next one.
        self.active_history_mut().abandon_pending_drag();
        // A tab is another sequence, not a step out of an opened compound
        // clip: the way back to its parents is dropped, so the breadcrumb
        // and Ctrl+Up never lead into a sequence it no longer sits in
        // (#910). A stacked timeline brings its own way back.
        if !keep_view {
            self.compound_clip_stack.clear();
        }
        self.active_sequence_idx = sequence_idx;
        // The playhead taken back from the sequence shown.
        let seq_rate = self.sequences[sequence_idx].fps.rate();
        let restored = self.sequences[sequence_idx].last_playhead.unwrap_or(klipp_core::RationalTime::from_frames(0, seq_rate));
        self.playhead = restored;
        self.playhead_smooth_secs = restored.to_secs_f64();
        self.playhead_anchor = restored;
        self.playhead_anchor_wall = std::time::Instant::now();
        self.playhead_speed = 1.0;
        self.fps = self.sequences[sequence_idx].fps;
        // The selection cleared, unless the timeline brings its own.
        if !keep_view {
            self.selected_clip = None;
            self.selected_clips.clear();
            self.hovered_clip = None;
        }
        true
    }

    /// Every sequence of the project, as the control socket lists them,
    /// and the open one's index (M3 step 18).
    pub fn sequence_summaries(&self) -> (Vec<klipp_core::SequenceSummary>, usize) {
        let list = self.sequences.iter().map(|seq| klipp_core::SequenceSummary {
            id: seq.id.to_string(),
            name: seq.name.clone(),
            clips: seq.tracks.iter().map(|t| t.clips.len()).sum(),
            used_in: self.sequences.iter().find(|other| {
                other.id != seq.id && other.tracks.iter().flat_map(|t| &t.clips).any(|c| matches!(c.kind, klipp_timeline::ClipKind::Sequence { sequence_id, .. } if sequence_id == seq.id))
            }).map(|other| other.name.clone()),
        }).collect();
        (list, self.active_sequence_idx)
    }
}

/// Workspace mode — determines panel arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Workspace {
    #[default]
    Edit,
    Color,
    Export,
}

impl Workspace {
    /// Does this workspace borrow the browser's width?
    ///
    /// Colour and Export both fill the window with something the
    /// browser has no part in — scopes and the picture in one, the
    /// export form and the picture in the other — and neither is a
    /// workspace you drag effects in. Edit is, and keeps it.
    ///
    /// Borrowed, not taken: the panel rail leaves it one click away,
    /// and [`browser_borrow_for`] puts back what was there on the way
    /// out.
    pub fn borrows_the_browser(self) -> bool {
        matches!(self, Workspace::Color | Workspace::Export)
    }
}

/// What a workspace switch should do to the browser panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserBorrow {
    /// Record what the browser is doing now, then collapse it.
    Take,
    /// Collapse it; a borrow is already recorded and stands.
    KeepTaken,
    /// Put back whatever was recorded.
    Give,
    /// Neither workspace wants the space; leave the panel alone.
    Leave,
}

/// The browser's fate when the workspace goes from `from` to `to`.
///
/// Split out from the app so the whole matrix can be checked without
/// building one. The case that needs the care is Colour straight to
/// Export: both borrow, and recording a second time would store a
/// collapsed browser as the thing to restore — the panel would then
/// never come back.
pub fn browser_borrow_for(from: Workspace, to: Workspace) -> BrowserBorrow {
    match (from.borrows_the_browser(), to.borrows_the_browser()) {
        (false, true) => BrowserBorrow::Take,
        (true, true) => BrowserBorrow::KeepTaken,
        (true, false) => BrowserBorrow::Give,
        (false, false) => BrowserBorrow::Leave,
    }
}

/// Has the user taken the browser back while the borrower still holds
/// it?
///
/// True when a borrow is recorded, the workspace is one that borrows,
/// and the browser is nonetheless visible — which can only mean it was
/// asked for, by the rail, a menu or Reveal in Browser. The recorded
/// value is then dropped: leaving the workspace must not undo a choice
/// the user made inside it.
pub fn borrow_released_by_hand(
    holding: bool,
    workspace: Workspace,
    browser_visible: bool,
) -> bool {
    holding && workspace.borrows_the_browser() && browser_visible
}

/// Where to dock the color grading tools (wheels, curves, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorToolsPosition {
    /// Color tools in the right-side inspector panel (default).
    #[default]
    Right,
    /// Color tools in a panel below the viewer.
    Bottom,
}

/// A saved workspace layout (panel visibility + sizes + name).
///
/// Sizing fields are `Option<f32>` with `#[serde(default)]` so legacy
/// workspaces.json files saved before this field set loaded cleanly:
/// missing fields → `None` → the applier leaves the current dimension
/// alone rather than collapsing it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedWorkspaceLayout {
    pub name: String,
    pub show_browser: bool,
    pub show_inspector: bool,
    pub show_history: bool,
    pub show_markers: bool,
    pub show_curve_editor: bool,
    pub show_scopes: bool,
    pub show_color_panel: bool,
    #[serde(default)]
    pub browser_width: Option<f32>,
    #[serde(default)]
    pub inspector_width: Option<f32>,
    #[serde(default)]
    pub timeline_height: Option<f32>,
    #[serde(default)]
    pub color_panel_height: Option<f32>,
}

impl SavedWorkspaceLayout {
    /// What to put in front of a user, as opposed to [`Self::name`],
    /// which is an identifier written into `workspaces.json` and
    /// matched back by string.
    ///
    /// The two differ for exactly one built-in: the layout is stored as
    /// "Color" so respelling it would not orphan saved workspaces, but
    /// the Window menu was rendering that identifier straight to the
    /// screen — one line under an entry spelled "Colour", in an
    /// application that is British everywhere else.
    ///
    /// A user's own saved workspace is shown exactly as they named it.
    pub fn display_name(&self) -> &str {
        match self.name.as_str() {
            "Color" => "Colour",
            other => other,
        }
    }
}

/// The 5 built-in workspace layouts shipped out of the box. Mirrors
/// Kdenlive's workspace vocabulary (Logging / Editing / Effects /
/// Audio / Color) so users coming from Kdenlive find familiar names.
///
/// Never persisted to disk — computed fresh at runtime so layout
/// tweaks in later releases reach all users, not just new installs.
pub fn builtin_workspace_layouts() -> Vec<SavedWorkspaceLayout> {
    vec![
        // ── Logging ── media review focus: wide browser, narrow inspector,
        // tall-ish timeline for scrubbing through takes, markers panel open.
        SavedWorkspaceLayout {
            name: "Logging".into(),
            show_browser: true,
            show_inspector: false,
            show_history: false,
            show_markers: true,
            show_curve_editor: false,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(420.0),
            inspector_width: Some(280.0),
            timeline_height: Some(180.0),
            color_panel_height: None,
        },
        // ── Editing ── the default balanced layout.
        SavedWorkspaceLayout {
            name: "Editing".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: false,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(280.0),
            inspector_width: Some(crate::theme::INSPECTOR_DEFAULT_W),
            timeline_height: Some(240.0),
            color_panel_height: None,
        },
        // ── Effects ── wider inspector for effect params, curve editor on
        // for keyframe tweaking, narrower browser to save horizontal space.
        SavedWorkspaceLayout {
            name: "Effects".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: true,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(240.0),
            inspector_width: Some(360.0),
            timeline_height: Some(220.0),
            color_panel_height: None,
        },
        // ── Audio ── tall timeline so waveforms are readable, curve editor
        // for volume/pan automation, narrower inspector sized for the Audio
        // inspector tab rather than the wider Effects tab.
        SavedWorkspaceLayout {
            name: "Audio".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: true,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(240.0),
            inspector_width: Some(340.0),
            timeline_height: Some(300.0),
            color_panel_height: None,
        },
        // ── Color ── browser hidden to maximise scopes + viewer real estate,
        // color panel below viewer, scopes on, short timeline since editing
        // is usually done before grading.
        SavedWorkspaceLayout {
            name: "Color".into(),
            show_browser: false,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: false,
            show_scopes: true,
            show_color_panel: true,
            browser_width: Some(200.0),
            inspector_width: Some(crate::theme::INSPECTOR_DEFAULT_W),
            timeline_height: Some(180.0),
            color_panel_height: Some(320.0),
        },
    ]
}

/// Active tab in the simple color inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTab {
    #[default]
    Adjustments,
    Wheels,
    Curves,
    HueSat,
    Warper,
}

/// Register every real model — the ones gated by feature flags.
/// Called once on the shared registry; the panel-side
/// `AiToolsRegistry` and the worker-side `RegistryHandle` both
/// observe the same set.
#[allow(unused_variables)]
fn register_real_models(r: &mut klipp_ml::ModelRegistry) {
    #[cfg(feature = "ai-ort")]
    {
        register_birefnet(r);
        register_rf_detr_seg(r);
        register_rf_detr_seg_medium(r);
        register_mobile_sam(r);
    }
}

/// Build the single shared `Arc<RwLock<ModelRegistry>>` that backs
/// both the panel-side `AiToolsRegistry` and the worker-side
/// `RegistryHandle`. Stub registered first so prompt-bearing tasks
/// always have a fallback; real backends layer on top under their
/// feature flags.
fn build_shared_registry() -> std::sync::Arc<std::sync::RwLock<klipp_ml::ModelRegistry>> {
    let mut r = klipp_ml::ModelRegistry::new();
    r.register(
        klipp_ml::ModelInfo {
            id: klipp_ml::STUB_GENERATOR_ID.into(),
            display_name: "Stub (placeholder)".into(),
            capabilities: klipp_ml::MaskCapabilities::POINT_PROMPT
                | klipp_ml::MaskCapabilities::BOX_PROMPT
                | klipp_ml::MaskCapabilities::SCRIBBLE
                | klipp_ml::MaskCapabilities::AUTO_DETECT,
        },
        Box::new(|| Ok(Box::new(klipp_ml::StubMaskGenerator::new()))),
    );

    register_real_models(&mut r);

    std::sync::Arc::new(std::sync::RwLock::new(r))
}

#[cfg(feature = "ai-ort")]
fn register_mobile_sam(r: &mut klipp_ml::ModelRegistry) {
    use klipp_ml_ort::{MobileSamBuilder, MOBILE_SAM_DECODER_ID, MOBILE_SAM_MODEL_ID};

    let manifest = match klipp_ml::manifest::load_effective() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(target: "klipp_ui", "ai-ort: skipping MobileSAM (manifest load failed: {e})");
            return;
        }
    };
    let entry = match manifest.get(MOBILE_SAM_MODEL_ID) {
        Some(e) => e.clone(),
        None => {
            tracing::warn!(
                target: "klipp_ui",
                "ai-ort: skipping MobileSAM (no manifest entry for {})",
                MOBILE_SAM_MODEL_ID
            );
            return;
        }
    };
    let cache = match klipp_ml::ModelCache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "klipp_ui", "ai-ort: skipping MobileSAM (cache open failed: {e})");
            return;
        }
    };
    let weights_path = cache.path_for(&entry);
    let decoder_path = cache.path_for_id(MOBILE_SAM_DECODER_ID);

    r.register(
        klipp_ml::ModelInfo {
            id: MOBILE_SAM_MODEL_ID.into(),
            display_name: "MobileSAM".into(),
            // SAM-family capability set: clicks, boxes, scribbles,
            // and "no prompts = everything mask" auto-detect mode.
            capabilities: klipp_ml::MaskCapabilities::POINT_PROMPT
                | klipp_ml::MaskCapabilities::BOX_PROMPT
                | klipp_ml::MaskCapabilities::SCRIBBLE
                | klipp_ml::MaskCapabilities::AUTO_DETECT,
        },
        Box::new(move || {
            MobileSamBuilder::new()
                .weights(weights_path.clone())
                .decoder(decoder_path.clone())
                .build()
                .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
        }),
    );
}

#[cfg(feature = "ai-ort")]
fn register_rf_detr_seg(r: &mut klipp_ml::ModelRegistry) {
    use klipp_ml_ort::{RF_DETR_SEG_DISPLAY_NAME, RF_DETR_SEG_MODEL_ID};
    register_rf_detr_seg_variant(r, RF_DETR_SEG_MODEL_ID, RF_DETR_SEG_DISPLAY_NAME);
}

#[cfg(feature = "ai-ort")]
fn register_rf_detr_seg_medium(r: &mut klipp_ml::ModelRegistry) {
    use klipp_ml_ort::{RF_DETR_SEG_MEDIUM_DISPLAY_NAME, RF_DETR_SEG_MEDIUM_MODEL_ID};
    register_rf_detr_seg_variant(
        r,
        RF_DETR_SEG_MEDIUM_MODEL_ID,
        RF_DETR_SEG_MEDIUM_DISPLAY_NAME,
    );
}

/// Shared registration for every RF-DETR-Seg tier (preview, medium,
/// future large/x-large). The architecture and 3-output decode are
/// identical — only the manifest entry + weight path + reported
/// id/display_name differ.
#[cfg(feature = "ai-ort")]
fn register_rf_detr_seg_variant(
    r: &mut klipp_ml::ModelRegistry,
    model_id: &'static str,
    display_name: &'static str,
) {
    use klipp_ml_ort::RfDetrSegBuilder;

    let manifest = match klipp_ml::manifest::load_effective() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                target: "klipp_ui",
                "ai-ort: skipping {model_id} (manifest load failed: {e})"
            );
            return;
        }
    };
    let entry = match manifest.get(model_id) {
        Some(e) => e.clone(),
        None => {
            tracing::warn!(
                target: "klipp_ui",
                "ai-ort: skipping {model_id} (no manifest entry)"
            );
            return;
        }
    };
    let cache = match klipp_ml::ModelCache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                target: "klipp_ui",
                "ai-ort: skipping {model_id} (cache open failed: {e})"
            );
            return;
        }
    };
    let weights_path = cache.path_for(&entry);

    r.register(
        klipp_ml::ModelInfo {
            id: model_id.into(),
            display_name: display_name.into(),
            // Auto-detect only — RF-DETR consumes no user prompts.
            // Each detection becomes a MaskCandidate; the panel
            // renders bbox + class label per candidate.
            capabilities: klipp_ml::MaskCapabilities::AUTO_DETECT,
        },
        Box::new(move || {
            RfDetrSegBuilder::new()
                .weights(weights_path.clone())
                .model_id(model_id, display_name)
                .build()
                .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
        }),
    );
}

#[cfg(feature = "ai-ort")]
fn register_birefnet(r: &mut klipp_ml::ModelRegistry) {
    use klipp_ml_ort::{BiRefNetLiteBuilder, BIREFNET_LITE_MODEL_ID};

    // Resolve the weights path through the manifest + cache. If
    // anything fails (no manifest entry, no XDG dir), skip the
    // registration — the panel just won't show BiRefNet rather
    // than crashing the app.
    let manifest = match klipp_ml::manifest::load_effective() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(target: "klipp_ui", "ai-ort: skipping BiRefNet (manifest load failed: {e})");
            return;
        }
    };
    let entry = match manifest.get(BIREFNET_LITE_MODEL_ID) {
        Some(e) => e.clone(),
        None => {
            tracing::warn!(
                target: "klipp_ui",
                "ai-ort: skipping BiRefNet (no manifest entry for {})",
                BIREFNET_LITE_MODEL_ID
            );
            return;
        }
    };
    let cache = match klipp_ml::ModelCache::open() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "klipp_ui", "ai-ort: skipping BiRefNet (cache open failed: {e})");
            return;
        }
    };
    let weights_path = cache.path_for(&entry);

    r.register(
        klipp_ml::ModelInfo {
            id: BIREFNET_LITE_MODEL_ID.into(),
            display_name: "BiRefNet Lite".into(),
            // Background matting is auto-detect only — no clicks /
            // boxes / scribbles. The panel's task→capability filter
            // will surface this for Remove Background only.
            capabilities: klipp_ml::MaskCapabilities::AUTO_DETECT,
        },
        Box::new(move || {
            BiRefNetLiteBuilder::new()
                .weights(weights_path.clone())
                .build()
                .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
        }),
    );
}

/// Register a single user-added custom model against the shared
/// registry. Called during startup for every `CustomModelEntry`
/// loaded from `~/.config/klipp/ai_tools.toml`, and again from the
/// paste-URL UI when the user confirms a freshly-downloaded file.
///
/// Adapter routing: `CustomAdapter` carries the user's pick from
/// the modal — Klipp can't infer the architecture from a raw ONNX
/// without loading it, so the user tells us which builder to use.
/// Capability flags mirror the canonical entry for that adapter.
///
/// Feature-gated: an adapter whose feature is off this build still
/// registers its `ModelInfo` (so the dialog shows the entry) but
/// the factory closure returns a `BackendInit` error explaining
/// the missing flag. Same UX shape the built-in feature-gated
/// entries already produce.
pub fn register_custom_model(
    r: &mut klipp_ml::ModelRegistry,
    entry: &crate::ai_tools::persistence::CustomModelEntry,
) {
    use crate::ai_tools::persistence::CustomAdapter;

    // The disk cache is only consulted by feature-gated factory branches
    // (ai-ort / ai-candle). In a no-AI build every adapter ends up with
    // the "feature missing" error factory, so we skip the cache open
    // entirely — both to avoid a dead `weights_path` and to skip pointless
    // disk work when no backend can use the weights anyway.
    #[cfg(any(feature = "ai-ort", feature = "ai-candle"))]
    let weights_path = {
        let cache = match klipp_ml::ModelCache::open() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    target: "klipp_ui",
                    "custom model: skipping {} (cache open failed: {e})",
                    entry.id
                );
                return;
            }
        };
        cache.path_for_id(&entry.id)
    };
    let id = entry.id.clone();
    let display_name = entry.display_name.clone();
    let adapter = entry.adapter;

    let capabilities = match adapter {
        CustomAdapter::BiRefNet => klipp_ml::MaskCapabilities::AUTO_DETECT,
        CustomAdapter::MobileSam => {
            klipp_ml::MaskCapabilities::POINT_PROMPT
                | klipp_ml::MaskCapabilities::BOX_PROMPT
                | klipp_ml::MaskCapabilities::SCRIBBLE
                | klipp_ml::MaskCapabilities::AUTO_DETECT
        }
        CustomAdapter::RfDetrSeg => klipp_ml::MaskCapabilities::AUTO_DETECT,
    };

    let factory: klipp_ml::MaskGeneratorFactory = match adapter {
        #[cfg(feature = "ai-ort")]
        CustomAdapter::BiRefNet => {
            let path = weights_path.clone();
            Box::new(move || {
                klipp_ml_ort::BiRefNetLiteBuilder::new()
                    .weights(path.clone())
                    .build()
                    .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
            })
        }
        #[cfg(not(feature = "ai-ort"))]
        CustomAdapter::BiRefNet => Box::new(|| {
            Err(klipp_ml::MaskError::BackendInit(
                "Custom BiRefNet model requires --features ai-ort".into(),
            ))
        }),
        // A custom SAM-family image encoder (ONNX) with MobileSAM's mask
        // decoder from the model cache: SAM's decoder reads any of their
        // embeddings.
        #[cfg(feature = "ai-ort")]
        CustomAdapter::MobileSam => {
            let path = weights_path.clone();
            let decoder = klipp_ml::ModelCache::open().map(|c| c.path_for_id(klipp_ml_ort::MOBILE_SAM_DECODER_ID)).ok();
            Box::new(move || {
                let Some(decoder) = decoder.clone().filter(|p| p.is_file()) else {
                    return Err(klipp_ml::MaskError::BackendInit(
                        "A custom MobileSAM model uses MobileSAM's mask decoder: download MobileSAM first".into(),
                    ));
                };
                klipp_ml_ort::MobileSamBuilder::new()
                    .weights(path.clone())
                    .decoder(decoder)
                    .build()
                    .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
            })
        }
        #[cfg(not(feature = "ai-ort"))]
        CustomAdapter::MobileSam => Box::new(|| {
            Err(klipp_ml::MaskError::BackendInit(
                "Custom MobileSAM model requires --features ai-ort".into(),
            ))
        }),
        #[cfg(feature = "ai-ort")]
        CustomAdapter::RfDetrSeg => {
            let path = weights_path.clone();
            let model_id: &'static str = Box::leak(id.clone().into_boxed_str());
            let display: &'static str =
                Box::leak(display_name.clone().into_boxed_str());
            Box::new(move || {
                klipp_ml_ort::RfDetrSegBuilder::new()
                    .weights(path.clone())
                    .model_id(model_id, display)
                    .build()
                    .map(|g| Box::new(g) as Box<dyn klipp_ml::MaskGenerator>)
            })
        }
        #[cfg(not(feature = "ai-ort"))]
        CustomAdapter::RfDetrSeg => Box::new(|| {
            Err(klipp_ml::MaskError::BackendInit(
                "Custom RF-DETR-Seg model requires --features ai-ort".into(),
            ))
        }),
    };

    r.register(
        klipp_ml::ModelInfo {
            id,
            display_name,
            capabilities,
        },
        factory,
    );
}

/// Quick-preset for the Position block in the Title inspector.
/// Coordinates are normalized 0..1 in the title's reference frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitlePositionPreset {
    Top,
    Center,
    LowerThird,
}

impl TitlePositionPreset {
    /// Canonical (x, y) for this preset. `Top` sits a touch below
    /// the upper edge; `Lower 3rd` sits at the standard NLE lower-
    /// third line.
    pub fn coords(self) -> (f32, f32) {
        match self {
            TitlePositionPreset::Top => (0.5, 0.10),
            TitlePositionPreset::Center => (0.5, 0.5),
            TitlePositionPreset::LowerThird => (0.5, 0.85),
        }
    }

    /// Tolerance for considering a layer's current X/Y "still on"
    /// the preset. 0.005 ≈ 10 px of slop on a 1920-wide frame.
    pub const EPSILON: f32 = 0.005;

    /// Returns this preset if `(x, y)` is within `EPSILON` of its
    /// canonical coordinates.
    pub fn matches(self, x: f32, y: f32) -> bool {
        let (px, py) = self.coords();
        (x - px).abs() < Self::EPSILON && (y - py).abs() < Self::EPSILON
    }
}

/// An aspect version to reframe once `waiting`'s framing analysis is done
/// (#2211).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRefit {
    pub sequence: klipp_core::SequenceId,
    pub waiting: Vec<klipp_core::MediaId>,
}

/// Document ops a panel asks the app to run as one undo step.
#[derive(Debug, Clone)]
pub struct DocOpsRequest {
    /// What history calls the step. `{n}` in it is written as the
    /// number of ops applied, so a sweep says how many cuts were made,
    /// not how many were ticked.
    pub label: String,
    pub ops: Vec<klipp_document::ops::Op>,
    /// The ops stand on their own — a sweep of cuts, each at its own
    /// place — so one that is refused is left out and reported while the
    /// rest are applied. Otherwise one refusal stops the whole request.
    pub independent: bool,
}

/// Play In to Out under way (#921): where it starts and stops, when it
/// was asked for, and whether playback has started from the In.
#[derive(Debug, Clone, Copy)]
pub struct PlayToOut {
    pub from: klipp_core::Timecode,
    pub out: klipp_core::Timecode,
    pub asked: std::time::Instant,
    pub started: bool,
    /// Back to `from` at the Out instead of stopping: play around an
    /// edit (#914).
    pub looping: bool,
}

pub struct AppState {
    pub playback: PlaybackState,
    pub playhead: Timecode,
    pub fps: Fps,
    pub playback_speed: f64,
    /// What each further J or L press multiplies the speed by: the
    /// playback preferences' JKL speed steps.
    pub jkl_speed_step: f64,
    /// The search radius a new track point starts with, in pixels: the
    /// editing preferences' tracking search radius.
    pub track_search_radius: u32,
    /// How Create Synced Multicam syncs its angles in one click: the
    /// editing preferences' default sync method.
    pub multicam_default_sync: klipp_core::MulticamSync,
    pub loop_playback_enabled: bool,

    // Active workspace
    pub active_workspace: Workspace,
    /// User-saved workspace layouts.
    pub custom_workspaces: Vec<SavedWorkspaceLayout>,
    /// Show the save workspace name dialog.
    pub show_save_workspace_dialog: bool,
    /// Name for the workspace being saved.
    pub save_workspace_name: String,
    /// Show the "Save hyperzoom as preset" modal dialog
    /// (HZ-NEXT-SAVED-PRESETS). Separate slot from
    /// `show_save_workspace_dialog` per the feature audit —
    /// avoids one-of-N-modals-open tracking + keeps the save-
    /// preset code path independent of workspace lifecycle.
    pub show_save_hyperzoom_preset_dialog: bool,
    /// Name being typed into the save-preset dialog. Persists
    /// across frames while the dialog is open; cleared on
    /// successful save or cancel. The clip-id whose hyperzoom
    /// scratch the dialog is reading from at open time —
    /// stashed so a focus loss on the inspector doesn't lose
    /// the binding.
    pub save_hyperzoom_preset_name: String,
    /// Inline validation message shown under the save-preset
    /// dialog's text input. Set when the user submits an empty /
    /// whitespace-only / all-disallowed-chars name.
    /// `None` means no error visible.
    pub save_hyperzoom_preset_error: Option<String>,
    /// Source clip the save-preset dialog is bound to. Pinned
    /// when the dialog opens so the saved preset reflects the
    /// hyperzoom scratch state of that specific clip — even if
    /// the user navigates the timeline before clicking Save.
    pub save_hyperzoom_preset_source_clip: Option<klipp_core::ClipId>,
    /// Show the workspace-manager modal (delete/rename/export/import).
    pub show_workspace_manager: bool,
    /// Currently highlighted workspace name in the manager dialog.
    /// String rather than index so built-ins and user presets share one
    /// selector without needing a variant enum.
    pub workspace_manager_selected: Option<String>,

    // Project
    pub sequences: Vec<Sequence>,
    pub active_sequence_idx: usize,
    /// Moves every time a project is opened, created or closed. A file
    /// picker opened under one epoch has its result discarded if it
    /// closes under another (see `file_dialog`).
    pub project_epoch: u64,
    /// Breadcrumb stack for navigating into compound clips. Each entry is
    /// (parent_sequence_idx, parent_playhead) so we can navigate back.
    pub compound_clip_stack: Vec<(usize, klipp_core::Timecode)>,
    pub media_pool: MediaPool,
    /// Cache of synthesised whoosh `MediaId`s, keyed by the
    /// `WhooshVariant` u8 wire-format tag (0/1/2). Populated lazily
    /// the first time `ApplyHyperzoom { add_whoosh: true, .. }`
    /// fires for a given variant; subsequent applies reuse the
    /// existing pool entry. AppState-scoped per audit note (iv);
    /// a global cache index file is a polish item if cross-session
    /// re-synth becomes a noticeable cost.
    pub whoosh_media_ids: std::collections::HashMap<u8, klipp_core::MediaId>,
    /// Saved hyperzoom presets (HZ-NEXT-SAVED-PRESETS). Loaded
    /// once at app startup from `~/.config/klipp/hyperzoom_presets/`;
    /// mutations (save / delete) refresh in-memory + write to
    /// disk synchronously. Per-cut fields (focal_x/y,
    /// target_choice, custom whoosh path) are explicitly NOT
    /// part of the saved preset shape — see
    /// `hyperzoom_presets::HyperzoomPreset` for the type-level
    /// exclusion.
    pub hyperzoom_preset_registry: crate::hyperzoom_presets::HyperzoomPresetRegistry,
    /// Project-scoped synthetic title assets, keyed by the
    /// `MediaId` of the matching synthetic `MediaItem` in the pool.
    /// The map is the canonical source of truth for `TitleData`
    /// edits — title clips with `linked_title_asset_id = Some(id)`
    /// read from here at render time, and Inspector edits write
    /// back to here.
    pub title_assets: std::collections::HashMap<klipp_core::MediaId, klipp_timeline::TitleAsset>,
    pub project_path: Option<PathBuf>,
    pub created_at: Option<String>,
    /// Free-form project notes — shot list, delivery specs, anything
    /// the editor wants to keep alongside the project. Persisted to
    /// project.json and edited via the Project Notes window.
    pub project_notes: String,
    pub last_import_report: Option<klipp_project::import_report::ImportReport>,
    pub show_import_report: bool,
    /// Whether the Project Notes window is open.
    pub show_project_notes: bool,
    /// Whether the Edit Plan window is open.
    pub show_edit_plan: bool,
    /// The Colour Check window: what a colourist would read off the
    /// frame on screen, with the fault named and the knob to turn.
    pub show_colour_check: bool,
    /// The Delivery Check window (#1978): the check's findings, each
    /// reviewed where it is read.
    pub show_delivery_check: bool,
    /// The Edit List window (#923): the sequence's clips, transitions and
    /// markers in a sortable table.
    pub show_edit_list: bool,
    /// The Sound Over Time window: a whole file's sound as frequency
    /// against time, with what is wrong with it named.
    pub show_sound_over_time: bool,
    /// The Narrate window: a scratch voice from text.
    pub show_narrate: bool,
    /// What the Edit Plan window last did or could not do, shown under
    /// its buttons.
    pub edit_plan_note: Option<String>,
    /// Mirrored from `prefs.audio.tp_oversample`. Drives the meter
    /// tooltip wording so users know whether they're seeing real
    /// 4× true-peak or sample-hold.
    pub tp_oversample: bool,
    pub dirty: bool,
    /// Per-sequence undo stack. Keyed by `SequenceId` (stable across
    /// reorder/delete) so an entry never points at a sequence that
    /// shifted underneath it. Use `active_history` /
    /// `active_history_mut` / `push_history` to access the current
    /// sequence's stack.
    pub command_histories: std::collections::HashMap<SequenceId, CommandHistory>,
    /// The project's own undo history (#1986): the steps that belong to
    /// the whole project — its colour set-up, a file's camera encoding,
    /// the media pool's records — reachable by ctrl+z from any sequence.
    /// Use `push_history`, `take_undo` and `take_redo`, which choose
    /// between it and the active sequence's by when each step moved.
    pub project_history: CommandHistory,
    /// The clock every history stamps its steps from, so the project's
    /// and a sequence's compare.
    pub history_clock: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Project-level autosave counter — incremented on every history
    /// push regardless of which sequence owned it. Cleared by the
    /// project-save path. Lives on AppState (not on CommandHistory)
    /// because autosave fires on total project mutations.
    pub history_ops_since_save: u64,
    /// The last edit recorded by `tracks_undo` and when, so a dragged
    /// value's changes fold into one undo step.
    pub tracks_undo_gesture: Option<(&'static str, std::time::Instant)>,
    /// What the control socket reads without the UI: the document
    /// revision (advanced by every recorded edit, undo and redo) and the
    /// render queue's jobs. Shared with `klipp_control` by the app.
    pub control_shared: std::sync::Arc<klipp_core::ControlShared>,
    /// The project's other sequences, published for the control socket's
    /// `edit_from_selects` (#2385), and which sequences, with which open,
    /// they were published for.
    pub control_others: klipp_document::ops::SharedSelects,
    pub control_others_for: Option<(Vec<klipp_core::SequenceId>, usize)>,
    /// Mirrors the `editing.undo_limit` pref so newly-created sequences
    /// can be initialised with the same per-sequence cap. Updated
    /// alongside `set_max_size` calls on existing histories.
    pub history_max_size: usize,

    // Layout
    pub viewer_inspector_split: f32,
    pub upper_timeline_split: f32,
    pub timeline_zoom: f32,
    pub timeline_scroll_x: f32,
    pub timeline_scroll_y: f32,
    /// Transient vertical scale applied by Fit so a tall track stack can
    /// fit the visible timeline without changing the user's track heights.
    pub timeline_track_scale: f32,
    /// Visible height available to timeline tracks, refreshed by the panel.
    pub last_timeline_tracks_height: f32,
    /// (track_idx, pointer_y_at_drag_start, original_height) — set while a track resize is in progress.
    pub timeline_resizing_track: Option<(usize, f32, f32)>,
    /// Per-track pre-collapse height memory for double-click collapse/expand toggle.
    pub timeline_track_pre_collapse: std::collections::HashMap<usize, f32>,
    pub playhead_follow: PlayheadFollow,
    /// The editor chose the follow mode in the timeline's options (#912):
    /// playback no longer turns Off into Page for them.
    pub playhead_follow_chosen: bool,
    pub track_header_width: f32,
    /// Independent zoom + pan for the Source viewer. Source and Program
    /// are kept fully separate — scrolling over one viewer zooms only
    /// that one. Matches Resolve / Premiere behaviour.
    pub source_view: ViewerView,
    /// Independent zoom + pan for the Program viewer.
    pub program_view: ViewerView,
    pub last_timeline_content_width: f32,
    pub show_overview_bar: bool,

    // Selection
    pub selected_clip: Option<ClipId>,
    /// Single-frame lookup cache for `selected_clip_loc()`. Stores
    /// `(clip_id, track_idx)` of the most recently resolved selection.
    /// On the next call, the helper checks the cached track first
    /// (`Track::find_clip`, O(clips_in_one_track)) and falls back to
    /// a full sequence walk only if the cache is stale (clip moved
    /// tracks, was deleted, or selection changed). Self-healing —
    /// nothing else needs to invalidate it. Cell so the helper can
    /// take `&self` and still update the cache.
    ///
    /// Inspector and viewer paths previously
    /// did `tracks.iter().find_map(|t| t.find_clip(cid))` per render
    /// — O(tracks × clips) per frame. With this cache, the steady
    /// state is O(1) (single-track find_clip), with the full walk
    /// only on actual selection / topology change.
    pub selected_clip_loc_cache: std::cell::Cell<Option<(ClipId, usize)>>,
    /// UI-only state — not undoable. Multi-clip selection is managed directly
    /// for performance (marquee updates every frame). Do not convert to commands.
    pub selected_clips: Vec<ClipId>,
    /// Edit points selected for trimming from the keyboard (#913), one
    /// per track at most; empty when none is.
    pub selected_edits: Vec<crate::edit_point::EditPoint>,
    /// A trim amount being typed (`+12`, `-1:05`) for the selected edit
    /// points, until Enter applies it or Escape drops it.
    pub trim_entry: Option<String>,
    /// A dynamic trim armed at this cut (#914): K commits the selected
    /// edit points to the playhead, Esc goes back here.
    pub dynamic_trim: Option<Timecode>,
    pub hovered_clip: Option<ClipId>,
    /// Currently selected transition (track_idx, transition_id). UI-only state.
    pub selected_transition: Option<(usize, TransitionId)>,

    // UI state
    pub show_unsaved_dialog: bool,
    pub unsaved_dialog_action: UnsavedAction,
    pub pending_media_path: Option<PathBuf>,
    /// Explicit pool imports awaiting their probe; history is recorded only on success.
    pub pending_undo_imports: std::collections::HashSet<klipp_core::MediaId>,
    /// Per-import opt-outs, consumed when the async load succeeds or fails.
    pub pending_skip_analysis: std::collections::HashSet<klipp_core::MediaId>,
    /// Media imported with a timeline destination (e.g. from the Audio
    /// Library): once its async load lands in the pool, drop it there.
    pub pending_media_drop: std::collections::HashMap<klipp_core::MediaId, klipp_core::events::ClipDrop>,
    /// Media of projects no longer open. A probe the engine was still
    /// running when the project changed reports back afterwards, and
    /// the load handler would add that item to whatever project is
    /// open by then — a new project saved with a clip of the last one.
    pub retired_media_ids: std::collections::HashSet<klipp_core::MediaId>,

    // Thumbnails
    pub thumbnail_cache: ThumbnailCache,
    pub thumbnail_textures: std::collections::HashMap<MediaId, egui::TextureHandle>,
    /// Filmstrip textures: media_id → Vec of (time_secs, texture_handle)
    pub filmstrip_textures: std::collections::HashMap<MediaId, Vec<(f64, egui::TextureHandle)>>,
    /// The Colour page strip's graded thumbnails, by clip (#899).
    pub graded_thumbs: crate::graded_thumbs::GradedThumbs,

    // Export
    pub show_export_dialog: bool,

    // Transport-bar jog wheel state (#38, scoped — software jog only).
    // Lives on AppState so in-progress drags survive panel relayouts.
    pub jog_wheel_state: crate::widgets::jog_wheel::JogWheelState,
    /// Whether the export-preset manager modal is visible. Opens from a
    /// button next to the preset dropdown in the export dialog.
    pub show_preset_manager: bool,
    /// Currently highlighted preset id inside the manager dialog. Session
    /// state only — lives on AppState rather than locally so the selection
    /// survives re-opens of the dialog within the same session.
    pub preset_manager_selected: Option<String>,

    /// Whether the AI Models management dialog is visible.
    pub show_ai_models_dialog: bool,

    /// Whether the Audio Plugins management dialog is visible.
    pub show_audio_plugins_dialog: bool,

    /// Per-dialog state for the Audio Plugins management modal: search
    /// query, "last scanned" timestamp, in-flight rescan flag.
    pub audio_plugins_dialog: crate::panels::audio_plugins::AudioPluginsDialogState,

    /// Whether the System Probe modal is visible. Snapshots GPU /
    /// ONNX runtime / audio backend state for bug reports.
    pub show_system_probe: bool,

    /// Whether the Project Doctor modal is visible.
    pub show_project_doctor: bool,

    // Cache manager dialog (#78)
    /// Whether the Cache Manager modal is visible.
    pub show_cache_manager: bool,
    /// Sized snapshot of on-disk cache categories for the Cache Manager
    /// dialog. Re-populated via `RefreshCacheManager` when opened or after
    /// a clear action.
    pub cache_categories: Vec<crate::cache_manager::CacheCategory>,

    // Clip properties dialog (#47)
    /// Whether the Clip Properties modal is visible.
    pub show_clip_properties: bool,
    /// Media item currently open in the Clip Properties modal. `None` when
    /// the modal is hidden.
    pub clip_properties_media_id: Option<MediaId>,
    /// Active tab inside the Clip Properties modal.
    pub clip_properties_tab: ClipPropertiesTab,

    // Phase 4: Tools & modes
    pub active_tool: Tool,
    pub viewer_mode: ViewerMode,
    /// Snap configuration: master gate + per-target booleans (clip
    /// edges, playhead, markers, in/out, grid). Synced from
    /// `LayoutState.snap` at startup; mutated via the
    /// `AppCommand::SetSnapMaster` and `AppCommand::SetSnapTarget`
    /// handlers (and persisted back via `layout_dirty`).
    pub snap: crate::layout_state::SnapConfig,
    pub ripple_enabled: bool,
    /// Linked selection (#916): on, a linked picture and sound trim and
    /// move together; off (or Alt while trimming), each on its own, to
    /// make a J or L cut.
    pub linked_selection: bool,
    /// Whether appending a clip brings its audio to an audio track.
    ///
    /// Source patching, in one boolean. On is right for camera footage
    /// and is the default; off is what you want cutting to music, where
    /// forty clips of camera hiss under the track is not a feature.
    /// Premiere and Avid spell this as per-track targeting; with one
    /// audio target the useful question is just whether the sound comes
    /// along.
    ///
    /// Session state, like `ripple_enabled` and the snap toggles — it
    /// goes back to on when the app restarts.
    pub append_brings_audio: bool,
    /// Default transition duration in frames (synced from prefs).
    pub default_transition_frames: u32,
    /// Frames Shift+Left and Shift+Right move, 0 for one second (synced
    /// from prefs, #921).
    pub shift_step_frames: u32,
    /// While Play In to Out runs: the Out it stops at, and whether
    /// playback has started yet (#921).
    pub play_to_out: Option<PlayToOut>,
    /// Timeline visual prefs (synced from prefs as whole struct).
    pub timeline_prefs: crate::prefs::TimelinePrefs,
    /// Viewer prefs (synced from prefs as whole struct).
    pub viewer_prefs: crate::prefs::ViewerPrefs,
    /// Snap threshold in pixels (synced from prefs).
    pub snap_threshold_px: f32,
    /// Grid-snap interval (synced from prefs.editing.grid_snap_interval).
    /// `Off` (default) means dragged clip edges never snap to a time
    /// grid; flipping to any other variant adds a grid-tick snap
    /// candidate on top of the existing clip-edge / playhead /
    /// marker / range targets.
    pub grid_snap_interval: klipp_core::GridInterval,
    /// Zoom step multiplier (synced from prefs).
    pub zoom_step: f32,
    /// Default resolution preset index for new sequences (mirrored from
    /// `prefs.editing.default_sequence_resolution_idx`). Index into
    /// `panels::new_project_dialog::RESOLUTION_PRESETS`. 0 = Automatic
    /// (inherit from active sequence). Used by the timeline "+" button.
    pub default_sequence_resolution_idx: usize,
    /// Toast duration in seconds (synced from prefs).
    pub toast_duration_secs: f64,
    /// Modal backdrop opacity (synced from prefs).
    pub modal_backdrop_alpha: u8,
    /// Scope quality (synced from prefs).
    pub scope_quality: klipp_core::ScopeQuality,
    /// Scope update interval ms (synced from prefs).
    pub scope_update_interval_ms: u32,
    /// Magnetic timeline edit mode: Insert (ripple) or Overwrite (replace)
    pub edit_mode: klipp_timeline::magnetic::EditMode,
    /// Whether magnetic timeline is enabled (synced from prefs).
    pub magnetic_timeline_enabled: bool,
    /// Whether multicam angle badges/cut list are drawn on timeline clips (synced from prefs).
    pub multicam_show_angles: bool,
    pub fullscreen_viewer: bool,
    pub fullscreen_changed: bool,

    // Phase 4: Clipboard
    pub clipboard: Option<ClipboardContent>,
    /// Text for the system clipboard, set when clips are copied or cut
    /// and handed to the window on the next frame. The window layer
    /// reports Ctrl+V only when that clipboard holds text, so with it
    /// empty a paste of copied clips did nothing.
    pub system_clipboard_text: Option<String>,
    /// Attributes clipboard for Paste Attributes (Ctrl+Shift+V).
    pub attributes_clipboard: Option<AttributesClipboard>,
    /// A title's look copied to paste on others (#1718).
    pub title_style_clipboard: Option<klipp_timeline::TitleData>,
    /// The title the style eyedropper gives the next title clicked's
    /// look to (#1718).
    pub title_style_pick: Option<ClipId>,
    /// Whether to show the Paste Attributes dialog.
    pub show_paste_attributes_dialog: bool,
    /// Whether to show the Set Clip Speed dialog.
    pub show_speed_dialog: bool,
    /// An editing helper's dialog, open on a clip (#988).
    pub edit_helper: Option<crate::edit_helpers::EditHelperDialog>,
    /// Current value in the speed dialog (percentage, e.g. 100.0 = normal).
    pub speed_dialog_value: f64,
    /// Whether speed dialog reverse is checked.
    pub speed_dialog_reverse: bool,
    /// The Speed dialog's Keep pitch box (#925).
    pub speed_dialog_keep_pitch: bool,
    /// The gap selected by a click on empty track space (#925): its track,
    /// start and end. Delete closes it.
    pub selected_gap: Option<(usize, Timecode, Timecode)>,
    /// The Speed dialog's "move the clips after it" (#639).
    pub speed_dialog_ripple: bool,

    // Source viewer
    pub source_media_id: Option<MediaId>,
    pub source_playhead: Timecode,
    /// Last wall-clock when the source-viewer scrub bar dispatched
    /// a `SeekSource` to the engine. Used to throttle drag-fire to
    /// ~30 Hz so the engine's decoder thread isn't pinned with
    /// per-UI-frame demux + BSF + seek work that the user can't
    /// perceive at sub-33 ms granularity anyway.
    pub source_seek_dispatch_at: Option<std::time::Instant>,
    /// First-frame texture for the currently-loaded source clip. The
    /// `MediaId` is paired with the texture so a stale frame from a
    /// previous load is dropped the moment the user switches clips,
    /// rather than briefly flashing the old image.
    pub source_video_texture: Option<(MediaId, egui::TextureHandle)>,
    /// Source viewer's transport state — kept in sync with the
    /// engine's `source_state` via `SourcePlaybackStarted/Paused`
    /// events. Read by transport key handlers + on-screen play
    /// button to decide whether Space should toggle to Play or Pause.
    pub source_playback: PlaybackState,
    /// Live source-viewer texture sourced from `source_frame_buffer`.
    /// Engine writes decoded source frames there during playback /
    /// seek; the source viewer panel uploads from it via the same
    /// defer-drop pattern as the program viewer. Replaces / wins over
    /// `source_video_texture` (the cold-start thumbnail) once the
    /// engine produces its first frame.
    pub source_live_texture: Option<egui::TextureHandle>,
    /// Generation last uploaded into `source_live_texture`. Compared
    /// against `source_frame_buffer.generation` to decide when to re-
    /// upload (cheap one-int compare per render frame).
    pub source_live_texture_gen: u64,

    // Media browser multi-select
    pub selected_media_ids: std::collections::HashSet<MediaId>,
    pub last_clicked_media_id: Option<MediaId>,

    // Phase 4: Proxy
    pub use_proxies: bool,
    /// True while the user is actively scrubbing (dragging playhead on timeline).
    pub is_scrubbing: bool,
    /// Saved proxy state before scrub started (restored when scrub ends).
    pub pre_scrub_use_proxies: bool,
    /// Whether playback was running when scrub started — restored on scrub end.
    pub pre_scrub_was_playing: bool,
    /// True while waiting for a full-res frame after a seek — viewer shows filmstrip preview.
    pub showing_thumbnail_preview: bool,
    /// Set to true when a SeekTo is sent, cleared when FrameReady arrives.
    pub awaiting_seek_frame: bool,
    /// Render cache coverage per clip: (clip_id, cached_count, total_count).
    pub render_cache_ranges: Vec<(klipp_core::ClipId, u32, u32)>,
    /// The nodes the frame on screen didn't draw as asked (M4 step 10),
    /// for the viewer's chips.
    pub frame_declines: Vec<klipp_nodes::Decline>,
    /// The render status strip over the ruler (#924): graded clips' spans,
    /// cached or still to render, in timeline ticks.
    pub render_status: Vec<klipp_engine::texture::RenderSpan>,
    /// Render In to Out under way: frames done of the total (#924).
    pub render_range: Option<(usize, usize)>,

    // Phase 4: Relink
    pub show_relink_dialog: bool,
    pub offline_media_ids: Vec<MediaId>,

    // Phase 4: About
    pub show_about_dialog: bool,
    /// The update question or a new version's notice is up, in the middle
    /// of the window, and keeps the keys.
    pub update_notice_open: bool,

    // Phase 4: Timecode input
    pub editing_timecode: bool,
    pub timecode_input: String,

    // Phase 4: Fullscreen overlay
    pub fullscreen_overlay_visible: bool,
    pub fullscreen_overlay_timer: f64,

    // Phase 4: Media pool drag-and-drop
    pub dragging_media: Option<MediaId>,
    /// Every item travelling with the drag, in browser order — the
    /// dragged one alone, or the whole multi-selection it belongs to
    /// (see `drag_drop::drag_group`). Cleared with `dragging_media`.
    pub dragging_media_group: Vec<MediaId>,

    // GUI redesign
    pub skimming_enabled: bool,
    /// Whether VIDEO files in the file browser are hover-skimmable.
    /// Mirrored from `prefs.file_browser.file_browser_video_skim`. Independent
    /// of the global `skimming_enabled` toolbar toggle — both must be on
    /// for the file browser to spawn skim decodes.
    pub file_browser_video_skim_enabled: bool,
    /// Whether AUDIO files in the file browser are hover-skimmable.
    /// Mirrored from `prefs.file_browser.file_browser_audio_skim`. Same gating
    /// as `file_browser_video_skim_enabled`.
    pub file_browser_audio_skim_enabled: bool,
    pub toast: crate::widgets::toast::ToastState,
    pub drag_drop: crate::drag_drop::DragDropState,

    // Deferred texture drops. egui-wgpu destroys the underlying
    // `wgpu::Texture` synchronously both when a `TextureHandle` is
    // dropped AND when `TextureHandle::set` is called with an image
    // whose dimensions differ from the current texture — in the
    // latter case the handle is reused but the wgpu::Texture backing
    // it is replaced. The crash surfaces as
    // "Texture with 'egui_texid_Managed(N)' has been destroyed"
    // during the next `Queue::submit` that still references the old
    // ID from a pending command buffer. glow tolerated this;
    // wgpu validation does not.
    //
    // We hold handles we intend to drop in a three-slot ring (two
    // full frames of deferral). At the start of every `update()` the
    // slot index advances and the newly-current slot is cleared —
    // dropping the handles stashed there ~2 frames earlier, by which
    // point every submit that could have referenced them has
    // completed on the GPU.
    //
    // Every `TextureHandle::set` with a potentially changing frame
    // size must be size-guarded: if the new image differs, take the
    // old handle, push it here, and allocate fresh. The helper
    // `KlippApp::upload_texture_deferred` implements this pattern.
    pub deferred_texture_drops: [Vec<egui::TextureHandle>; 3],
    pub deferred_texture_drops_slot: usize,

    // Browser skimming
    pub browser_skim_texture: Option<egui::TextureHandle>,
    pub browser_skim_media_id: Option<MediaId>,
    pub skim_transcoding: std::collections::HashSet<MediaId>,
    /// In-flight skim transcodes keyed by output path. The Vec
    /// holds every MediaId that wants notifying when this output
    /// completes — multiple MediaIds (e.g. project-pool path vs
    /// file-browser absolute path) can canonicalize to the same
    /// source file and would otherwise both spawn ffmpeg jobs that
    /// race on the shared parts/ dir.
    pub skim_in_flight_paths: std::collections::HashMap<std::path::PathBuf, Vec<MediaId>>,
    /// Active skim transcode progress: (filename, progress 0.0..1.0)
    pub skim_transcode_status: Option<(String, f32)>,
    /// When the last skim transcode finished (for "Done" display)
    pub skim_transcode_done_at: Option<std::time::Instant>,
    /// Pending skim frame RGBA to upload as texture (media_id, rgba, width, height)
    /// `(media_id, rgba, width, height, target)` — pending skim frame
    /// awaiting upload. `target` decides whether it lands in the
    /// engine's frame buffer (Program) or the source viewer's overlay
    /// texture (Source).
    pub browser_skim_pending: Option<(MediaId, Vec<u8>, u32, u32, klipp_core::ViewerMode)>,
    /// True while a skim decode thread is running (prevents spawning more threads)
    pub skim_decode_in_flight: bool,
    /// When the current skim decode was started (for watchdog timeout reset)
    pub skim_decode_started_at: Option<std::time::Instant>,
    /// Latest skim request queued while a decode was in flight (media_id, time_secs)
    /// `(media_id, time_secs, target)` — queued skim request waiting
    /// for the in-flight decode to finish. `target` follows the request
    /// through the queue so the eventual response routes to the correct
    /// viewer.
    pub skim_queued_request: Option<(MediaId, f64, klipp_core::ViewerMode)>,
    /// `(media_id, frame, target)` of the skim frame last asked for: the
    /// frame on screen while a skim rests on it. A resting pointer redraws
    /// the UI, and a request for the frame already shown is not sent
    /// again. Cleared when the skim ends or a frame fails.
    pub skim_last_requested: Option<(MediaId, i64, klipp_core::ViewerMode)>,
    /// Set to true each frame if hover skim is active (used to clear texture when mouse leaves)
    pub browser_skim_active_this_frame: bool,
    /// Sticky flag: `true` once a skim with `target = Program` has
    /// written its frame into the engine's frame_buffer, cleared in the
    /// end-of-frame skim cleanup. Used to detect timeline-skim end and
    /// fire a `SeekTo(playhead)` refresh so the Program viewer reverts
    /// from the last-skimmed frame back to the committed playhead —
    /// pro-NLE skim is transient: mouse-leave = back to playhead.
    pub program_skim_active: bool,
    /// Deadline for the debounced skim-end revert seek. Armed when
    /// `program_skim_active` clears, disarmed when a new Program skim
    /// frame arrives before it fires. Debouncing matters: continuous
    /// skimming flaps the active flag for a frame at clip boundaries,
    /// and firing the revert on every flap both flickers the viewer
    /// and queues a seek per flap into the engine.
    pub program_skim_revert_deadline: Option<std::time::Instant>,
    /// Media IDs that need skim transcode started (queued during drain_events)
    pub pending_skim_transcodes: Vec<MediaId>,

    // Audio skimming (Shift+S toggle, independent of video skimming)
    /// Whether audio plays during skimming. Independent of skimming_enabled.
    pub audio_skimming_enabled: bool,
    /// Current skim speed in pixels per second (for mode selection).
    pub skim_speed_px_per_sec: f32,
    /// Time of last skim position update (for speed calculation).
    pub skim_last_update: std::time::Instant,
    /// Last skim x position in screen pixels (for speed calculation).
    pub skim_last_x: f32,
    /// The active skim audio session. Some() while hovering a skimmable clip.
    pub skim_audio_session: Option<klipp_engine::skim_audio::SkimAudioSession>,
    /// True while the FX-audition transport owns the skim session.
    /// All cursor-skim handlers (file browser hover, media tab hover,
    /// timeline scrub) bail out early when this is set so they can't
    /// silence the audition by re-running `determine_skim_mode` on a
    /// stationary cursor.
    pub audition_active: bool,
    /// System audio mute detection (Linux/PipeWire only). Polled via
    /// `wpctl get-volume @DEFAULT_AUDIO_SINK@` every ~2 seconds. When
    /// the system sink is muted, this is `true` and the transport bar
    /// shows a warning so users don't chase phantom audio bugs.
    pub system_audio_muted: bool,
    pub system_audio_last_check: std::time::Instant,
    /// When the last "muted by system" toast was shown. Used to re-show
    /// the reminder at the configured interval.
    pub system_audio_last_reminder: std::time::Instant,
    /// Reminder interval in seconds, mirrored from prefs.
    pub mute_reminder_interval_secs: u32,
    /// True when the file browser panel has keyboard focus (user
    /// clicked a row). Mirrored from `BrowserPanel::files_panel_focused`
    /// so the transport handler knows to yield Spacebar to Quick Look
    /// instead of triggering play/pause.
    pub files_panel_focused: bool,
    /// Quick Look preview: the file currently being previewed via
    /// Spacebar in the file browser. `None` = Quick Look is closed.
    /// The path is used to decode a preview frame (video/image) or
    /// start an audio playback session.
    pub quick_look_path: Option<std::path::PathBuf>,
    /// Cached texture for the Quick Look preview (video poster frame
    /// or decoded image). `None` = not yet decoded or audio file.
    pub quick_look_texture: Option<egui::TextureHandle>,
    /// Cached media header info for the Quick Look overlay text.
    pub quick_look_info: Option<klipp_engine::MediaHeader>,
    /// Audio session for Quick Look (audio files auto-play).
    pub quick_look_audio: Option<klipp_engine::skim_audio::SkimAudioSession>,
    /// Whether the Recents sidebar button is visible. Mirrored from
    /// `prefs.file_browser.show_recent_files`.
    pub show_recent_files: bool,
    /// Whether filmstrip hover preview is enabled in the file browser.
    pub filmstrip_hover_enabled: bool,
    /// Decode quality for filmstrip hover frames.
    pub filmstrip_quality: crate::prefs::FilmstripQuality,
    /// Keep filmstrip tiles visible after cursor leaves.
    pub filmstrip_persist: bool,
    /// Whether the file browser auto-refreshes on external filesystem
    /// changes. Mirrored from `prefs.file_browser.auto_refresh`.
    pub file_browser_auto_refresh: bool,
    /// Snapshot of `prefs.file_browser.recent_files` synced into state so
    /// the browser panel can read it without borrowing `prefs`. Updated
    /// at startup and whenever the prefs change.
    pub recent_files_snapshot: Vec<std::path::PathBuf>,
    /// Time of last audio skim decode dispatch (throttle to prevent starvation).
    pub skim_last_audio_dispatch: std::time::Instant,

    // Video stabilization analysis state
    /// Clips whose per-frame light is being measured for Deflicker.
    pub deflicker_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub deflicker_progress: std::collections::HashMap<klipp_core::ClipId, f32>,
    pub stabilization_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub stabilization_progress: std::collections::HashMap<klipp_core::ClipId, f32>,
    /// The analyses found in the analysis cache (M4 step 9), each a
    /// media file's, by its asset key: what `ensure_analyses` last found,
    /// so the inspector reads them without the disk each frame. Keyed by
    /// the media, not the clip: a trim, a split or a duplicate of a clip
    /// finds the same analysis ([`AppState::analysis_of`]).
    pub analyses: std::collections::HashMap<klipp_analysis::assets::AssetKey, std::sync::Arc<serde_json::Value>>,
    /// The stamp each analysis `ensure_analyses` loaded had when it was
    /// read: one whose stamp moves (analysed again, or gone from the
    /// cache) is read again, and one no clip of the sequence uses is let
    /// go. An analysis put in `analyses` by anything else has none.
    pub analysis_stamps: std::collections::HashMap<klipp_analysis::assets::AssetKey, u64>,
    /// When `ensure_analyses` last looked.
    pub analysis_checked: Option<std::time::Instant>,
    /// The sequence's revision when `ensure_analyses` last looked: an edit
    /// since (an effect added by any path: the inspector, a graph op over
    /// the control socket, an undo, a paste) is looked at on its own frame,
    /// never left to a frame that may not come while the window is idle.
    pub analysis_revision: Option<klipp_timeline::SequenceRevision>,
    /// Analyses `ensure_analyses` started, by asset: one a file however
    /// many clips read it. Each is let go once its run has ended and the
    /// asset is there, so an asset gone later is made again; one that
    /// ended without it is started again after [`ANALYSIS_RETRY`].
    pub analysis_started: std::collections::HashMap<klipp_analysis::assets::AssetKey, StartedAnalysis>,
    /// A rolling-shutter analysis's run, which sends no events: set when
    /// its thread ends.
    pub analysis_threads: std::collections::HashMap<klipp_analysis::assets::AssetKey, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Clips whose analysis can't be made: the media's original file isn't
    /// there (a proxy alone), so the inspector says why rather than wait.
    pub analysis_unavailable: std::collections::HashSet<(klipp_core::ClipId, klipp_analysis::assets::AssetKind)>,

    // Scene (shot-boundary) detection state
    pub scene_detect_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub scene_detect_progress: std::collections::HashMap<klipp_core::ClipId, f32>,
    /// Detections that split their clip when they finish (Detect Scene
    /// Cuts) rather than marking it (#616).
    pub scene_split_pending: std::collections::HashSet<klipp_core::ClipId>,
    /// Set to stop a running detection; its thread reads it each frame.
    pub scene_detect_cancel: std::collections::HashMap<klipp_core::ClipId, std::sync::Arc<std::sync::atomic::AtomicBool>>,

    // Audio spectrum analyser — shared PCM ring fed by the mix thread,
    // plus the latest FFT magnitude bins (dBFS, log-frequency) rendered by
    // the audio-spectrum scope. Sample rate is set from the engine once.
    pub spectrum_ring: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
    pub spectrum_bins: Vec<f32>,
    pub spectrum_sample_rate: u32,

    // Point tracking state
    /// Whether the viewer is in tracking placement mode (click to place tracker).
    pub tracking_mode: bool,
    /// Index of the track point currently being edited/dragged in the viewer.
    pub active_track_point: Option<usize>,
    /// Track analysis in progress for these clip IDs.
    pub tracking_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    /// Track analysis progress (0.0-1.0) per clip.
    pub tracking_progress: std::collections::HashMap<klipp_core::ClipId, f32>,
    /// A surface track running on a clip, stopped by setting this (#684).
    pub surface_track_cancel: std::collections::HashMap<klipp_core::ClipId, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Snaps of a surface's corners still being worked out (#684).
    pub surface_snaps_pending: usize,
    /// Pending auto-link bindings keyed by `(clip_id, point_index)`.
    /// When a `TrackingComplete` event arrives, the handler checks
    /// this map; if an entry exists, it dispatches
    /// `GenerateKeyframesFromTrack` with the recorded target and
    /// removes the entry. Set by the `TrackMaskRegion` command so
    /// the user gets one-gesture mask tracking.
    pub pending_mask_track_link: std::collections::HashMap<
        (klipp_core::ClipId, usize),
        klipp_core::TrackTarget,
    >,

    // Browser range selection (filmstrip drag-to-select in grid view)
    pub browser_range_selection: Option<BrowserClipSelection>,
    pub browser_drag_start: Option<(egui::Pos2, std::time::Instant)>,

    // Timeline range handle drag state
    pub range_drag: Option<RangeDragState>,
    /// Snap line x position during range handle drag (drawn in mod.rs).
    pub range_snap_line_x: Option<f32>,
    /// Press origin for 4px drag threshold on ruler.
    pub ruler_press_origin: Option<egui::Pos2>,

    // Phase 5: Performance overlay
    pub show_perf_overlay: bool,
    /// Whether a perf-telemetry recording is currently running. When
    /// true, the overlay shows a "REC" badge and the ring buffer
    /// captures samples from the `begin_recording` mark forward.
    pub perf_recording_active: bool,
    /// UI-side signal that something (menu / toolbar / keybinding
    /// other than the `Ctrl+Alt+P` shortcut) requested the
    /// recording be toggled. `KlippApp::update` consumes this on
    /// each paint and dispatches to `toggle_perf_recording`.
    pub pending_perf_recording_toggle: bool,
    /// Path of the most recent session dump written by a completed
    /// recording. Used by the overlay and toast to let the user find
    /// the file.
    pub perf_last_session_path: Option<std::path::PathBuf>,
    /// Latest per-frame perf sample snapshotted from the engine's
    /// shared ring buffer. `None` until the engine processes its
    /// first frame.
    pub perf_latest_sample: Option<klipp_engine::perf::PerfSample>,
    /// Rolling averages over the last ~2 s of samples (or the whole
    /// ring if it's shorter). Refreshed on the same 250 ms cadence
    /// as `perf_fps` / `perf_cpu_percent`.
    pub perf_averages: klipp_engine::perf::PerfAverages,
    /// Drops observed within the last second of wall clock.
    /// Painted in the overlay as "Drops: N in last 1s".
    pub perf_drops_last_sec: u32,
    pub perf_fps: f64,
    pub perf_frame_count: u64,
    /// Delivered video frames per second, as opposed to [`Self::perf_fps`]
    /// which is the egui repaint rate.
    ///
    /// These are easy to confuse and were: the overlay showed "UI FPS 119"
    /// and "Target 59.9 fps" while video actually ran at 22 fps, with no
    /// field anywhere reporting the rate actually achieved.
    pub perf_video_fps: f64,
    /// Average GPU->CPU readback per delivered frame over the last
    /// interval. Not part of the engine's post-process stage table, so it
    /// was invisible on the overlay while being the dominant per-frame
    /// cost at 4K.
    pub perf_readback_ms: f64,
    pub(crate) perf_last_frame_count: u64,
    pub(crate) perf_last_readback_count: u64,
    pub(crate) perf_last_readback_us: u64,
    pub perf_dropped_frames: u64,
    pub perf_memory_mb: f64,
    pub perf_cpu_percent: f64,
    /// None = no video decoder currently open (overlay shows "—").
    pub perf_hw_decode: Option<bool>,
    pub perf_last_update: std::time::Instant,
    pub perf_frame_times: Vec<f64>,
    pub perf_prev_cpu_ticks: u64,
    pub perf_prev_wall_time: std::time::Instant,

    // Filmstrip generation progress
    pub filmstrip_generating: bool,
    pub filmstrip_generate_start: std::time::Instant,
    pub filmstrip_toast_dismissed: bool,
    /// Media IDs currently generating filmstrips — prevents duplicate spawns.
    pub filmstrip_in_progress: std::collections::HashSet<MediaId>,
    /// Queue of media IDs waiting for filmstrip generation (serialized, one at a time).
    pub filmstrip_queue: Vec<(MediaId, PathBuf, klipp_core::RationalTime)>,
    /// Total number of filmstrip jobs queued in this batch (for progress display).
    pub filmstrip_total_queued: usize,
    /// Number of filmstrip jobs completed in this batch.
    pub filmstrip_completed: usize,
    /// Per-job frame progress: media_id → (frames_done, frames_expected).
    /// Updated on every `FilmstripResult::Frame`. Cleared on `Done`.
    /// Drives the "N/500" indicator in the status bar so the user can
    /// tell how much of a 4K filmstrip is still pending.
    pub filmstrip_progress: std::collections::HashMap<MediaId, (u32, u32)>,

    // Global LUT
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,
    pub global_lut_enabled: bool,

    /// Cube edge length for the next "Bake Grade as .cube" action.
    /// 17 = preview, 33 = broadcast default, 65 = high-quality.
    pub bake_lut_size: usize,

    // Color panel / node graph
    pub show_color_panel: bool,
    /// The Colour workspace grades the timeline grade — the look over
    /// the whole sequence — rather than the selected clip.
    pub grade_timeline: bool,
    /// The Colour workspace grades this grade group's pre-clip or
    /// post-clip grade rather than the selected clip (#237). Never with
    /// `grade_timeline`.
    pub grade_group_stage: Option<(klipp_core::GradeGroupId, klipp_timeline::GradeStage)>,
    /// The video track whose own grade the Colour workspace grades, when
    /// it grades one (#1408): after each of its clips' own grades.
    pub grade_track: Option<klipp_core::TrackId>,
    /// A title field's drag under way (#1139): the title as it found it,
    /// recorded as one step when it ends.
    pub title_field_before: Option<(klipp_core::ClipId, klipp_timeline::TitleData)>,
    /// The last title colour field edited, and when: picked colours close
    /// together are one step (#1139).
    pub title_colour_edit: Option<(klipp_core::ClipId, usize, std::time::Instant)>,
    /// Where color tools are docked: Right (inspector) or Bottom (below viewer).
    pub color_tools_position: ColorToolsPosition,
    /// Selected node in the node graph (shared between canvas and inspector).
    /// This is the **primary** of a possibly-larger selection set —
    /// the inspector reads it to decide which node's params to show.
    /// On a single click it equals the only entry in `selected_node_ids`;
    /// on a multi-select gesture (shift-click / box-select) it tracks
    /// the most recent addition so the inspector follows the user's
    /// hand. Cleared together with the set on clip switch.
    pub selected_node_id: Option<klipp_nodes::NodeId>,
    /// All currently-selected nodes on the V2 canvas. The canvas
    /// renders an amber border on every entry; multi-node operations
    /// (Delete, Group-into-Compound, future Bypass-all) iterate this
    /// set. Single-node operations (param edit, inspector display)
    /// use `selected_node_id` instead. Always either empty or
    /// containing `selected_node_id` as one of its members.
    pub selected_node_ids: std::collections::HashSet<klipp_nodes::NodeId>,
    /// Which panel is currently the user's primary interaction
    /// target. Read by the keybinding dispatcher to scope shortcuts
    /// to a panel — e.g. Ctrl+C means "copy colour nodes" only when
    /// `Some(PanelKind::NodeGraph)` or `Some(PanelKind::NodeInspector)`.
    /// Panels mark themselves active via [`note_active_panel`].
    pub active_panel: Option<PanelKind>,
    /// One-shot focus request for a panel's search field. Set by the
    /// global Ctrl+F (focus_panel_search) dispatcher to the current
    /// `active_panel`. Each search-bearing panel checks this on its
    /// next show(), calls `request_focus()` on its TextEdit, then
    /// clears the field. If no panel consumes it (e.g. active panel is
    /// Inspector / Viewer / Timeline — none of which expose a search
    /// surface today), app.rs clears it at end-of-update as a
    /// watchdog so a stale request can't get stuck.
    pub focus_search_request: Option<PanelKind>,
    /// One-shot "advance to next timeline search match" request. Set
    /// by the palette's "Find Next" entry; consumed by the timeline
    /// panel on its next show(). Noop when the search bar is hidden
    /// or there are no current hits — that's intended (the entry is
    /// a power-user convenience, not a force-open).
    pub timeline_search_next: bool,
    /// Mirror of [`Self::timeline_search_next`] for the previous-match
    /// direction. Set by the palette's "Find Previous" entry.
    pub timeline_search_prev: bool,
    /// In-memory clipboard for grade-node copy/paste between clips.
    /// Holds nodes plus edges induced by the copy selection, so a
    /// multi-node copy round-trips internal wiring; the paste handler
    /// remaps every Edge endpoint via the fresh-NodeId table built
    /// during paste. `None` means nothing has been copied yet.
    pub grade_node_clipboard: Option<GradeNodeClipboard>,
    /// The picture canvas's copied nodes, as the picture holds them
    /// (#1745). Empty: nothing copied.
    pub picture_node_clipboard: Vec<klipp_graph_format::Node>,
    /// Stack of NodeIds representing the V2 canvas's descent path into
    /// nested compound subgraphs. Empty = editing the clip's root
    /// graph (the common case). Each entry is the id of a
    /// `NodeKind::Compound` node in its parent graph; the canvas
    /// renders `compound.params.compound_subgraph` instead of the
    /// parent. Descending appends an id; ascending (breadcrumb click
    /// or Escape) truncates. Cleared alongside `selected_node_id`s
    /// on clip switch so stale ids don't accumulate.
    pub graph_descent_path: Vec<klipp_nodes::NodeId>,
    /// Whether the compact node strip is visible in Color workspace.
    pub show_node_strip: bool,
    /// Render the V2 2D node-graph editor instead of the legacy
    /// horizontal strip. True by default — the strip is kept as a
    /// fallback in case the V2 editor reveals a workflow regression
    /// during rollout. Toggle via the Color workspace's "Editor"
    /// label next to the Nodes toggle.
    pub use_node_graph_v2: bool,
    /// Active tab in the simple (non-node) color inspector.
    pub color_tab: ColorTab,        // right inspector panel
    pub color_tab_bottom: ColorTab, // bottom panel
    pub color_grade_drag_start: Option<klipp_nodes::NodeGraph>,
    pub color_grade_clipboard: Option<klipp_nodes::NodeGraph>,
    pub curves_single_mode: bool,
    pub curve_histograms: Option<klipp_color::scopes::HistogramData>,
    pub show_preset_menu: bool,
    pub preset_save_name: String,
    pub show_preset_save: bool,
    /// The grade library, across projects (#899).
    pub grade_library: Vec<crate::grade_library::LibraryGrade>,
    pub huesat_single_mode: bool,
    pub huesat_active_curve: u8, // 0-5 index for Single mode

    // Scopes
    pub show_scopes: bool,
    /// Source and program viewers side by side (true) or one viewer
    /// that shows whichever `viewer_mode` names (false).
    pub dual_viewers: bool,
    /// The same choice for the Color workspace, which starts with one
    /// program viewer so the picture gets the room.
    pub color_dual_viewers: bool,
    /// And for Export, which reviews the output: one program viewer.
    pub export_dual_viewers: bool,
    /// Render scopes as a semi-transparent overlay on the viewer instead of a separate panel.
    pub scope_overlay_mode: bool,
    pub show_keyframe_editor: bool,
    pub scope_mode: ScopeMode,
    pub scope_state: crate::widgets::scopes::ScopeState,
    pub scope_scale: ScopeScale,
    /// Mark the legal range, codes 64 to 940, on the docked scopes.
    pub scope_legal_range: bool,
    pub scope_show_r: bool,
    pub scope_show_g: bool,
    pub scope_show_b: bool,
    pub scope_low_pass: bool,
    /// The scope options, one struct so the main-dock popover and the
    /// floating scope window edit the same values.
    pub scope_options: ScopeOptions,
    pub scope_bloom: bool,
    pub scope_settings_open: bool,
    /// Floating scope window state.
    pub scope_window: ScopeWindowState,
    /// The docked scopes in the Colour workspace, when shown as more
    /// than one pane.
    ///
    /// Its own pane set and its own layout, kept apart from
    /// `scope_window` so opening the floating window does not rearrange
    /// the dock and vice versa. `open` is unused here — the dock's
    /// visibility is `show_scopes` / `scopes_docked()` as before.
    ///
    /// At [`ScopeLayout::OnePane`] the dock keeps its original single
    /// renderer driven by `scope_mode`, so the familiar
    /// Wave/Parade/Vector/Hist control and the R/G/B buttons behave
    /// exactly as they did. The pane grid takes over above one.
    pub scope_dock: ScopeWindowState,

    /// Viewer image rect (set by viewer each frame for scope cursor sampling).
    pub viewer_image_rect: Option<egui::Rect>,
    /// The viewer panel's outer rect (NOT the post-letterbox image rect).
    /// Set every frame by `render_main_layout` (windowed) or `render_fullscreen_viewer`
    /// (fullscreen). Used by overlays that need to align with the viewer regardless of
    /// layout mode. `egui::Rect::NOTHING` until first frame.
    pub viewer_panel_rect: egui::Rect,
    /// Pointer position over the viewer image (set by viewer on hover, None when not hovering).
    pub viewer_pointer_pos: Option<egui::Pos2>,

    // ── Bottom panel scope cursor (lock-capable) ──
    /// When true, the scope cursor is locked to a fixed position.
    pub scope_cursor_locked: bool,
    /// The locked pointer position (screen coords, set on click).
    pub scope_cursor_locked_pos: Option<egui::Pos2>,
    /// The locked cursor data (computed at lock time).
    pub scope_cursor_locked_data: Option<ScopeCursorData>,
    /// Sample box half-size for the bottom panel scope cursor.
    pub scope_cursor_sample_radius: u32,
    /// Whether to show the scope cursor indicator on the bottom panel.
    pub scope_cursor_show: bool,
    /// What the viewer shows while grading for an HDR delivery.
    pub hdr_monitor: klipp_core::HdrMonitor,
    /// Whether the HDR picture is being published for the reference-output
    /// window. Separate from `hdr_monitor`, which is about *this* screen.
    pub hdr_reference_output: bool,
    /// The light under the pointer, cd/m², while HDR monitoring is on —
    /// read from the frame buffer's map each frame.
    pub hdr_nits_under_pointer: Option<f32>,

    // Eyedropper mode (shared between WB and qualifier)
    pub eyedropper_mode: EyedropperMode,
    /// What was added to the timeline from the Library tabs this
    /// session, for their Recent lists.
    pub library_recent: crate::browser::library_card::LibraryRecent,
    /// Pixel coordinates to sample for white balance (set by viewer on click, consumed by app)
    /// The clip the white balance eyedropper was armed for.
    pub wb_pick_clip: Option<ClipId>,
    /// The clip and Qualifier node the qualifier eyedropper was armed for.
    pub qualifier_pick: Option<(ClipId, uuid::Uuid)>,
    /// The clip and Qualifier node a 3D stroke was armed for, and whether
    /// it takes colours out (#895).
    pub qualifier_stroke_pick: Option<(ClipId, uuid::Uuid, bool)>,
    /// A drawn stroke's samples on their way back from the engine.
    pub qualifier_stroke: Option<StrokeSampling>,
    /// The clip, Hue/Sat Curves node and curve the hue eyedropper was
    /// armed for, until it is clicked (#612).
    pub hue_curve_pick: Option<(ClipId, uuid::Uuid, u8)>,
    /// The same once clicked, until the engine's sample of the node's
    /// input comes back.
    pub hue_curve_awaiting: Option<(ClipId, uuid::Uuid, u8)>,
    /// Matte preview — show B&W qualifier/mask matte in viewer
    pub matte_preview_active: bool,
    /// Node-graph solo target — when set, the engine returns this
    /// node's output as the final image so the user can preview any
    /// mid-chain node as if it were the Output. Mirrors the same
    /// field on the engine; UI uses it to paint the yellow halo +
    /// "S" badge on the soloed node and the toolbar's "SOLO: …"
    /// indicator. Cleared by AppCommand::SetNodeSolo { at: None }.
    pub solo_node: Option<klipp_nodes::NodeId>,
    /// What the program viewer shows instead of the finished picture
    /// while a node is viewed: the node's name (#1763).
    pub viewed_node_name: Option<String>,

    // Before/After split view
    pub before_after_active: bool,
    pub split_position: f32, // 0.0-1.0, default 0.5

    // Stabilization compare split view (raw vs stabilized).
    // Mutually exclusive with before_after_active and grade_still_compare.
    pub stabilization_compare_active: bool,


    // M3's node-addressed UI state, declared once here (step 2) so the
    // inspector and graph-dock steps that read and set it never edit this
    // struct. Addresses are node paths, never list places: a clip with the
    // same local node id as another shares none of it.
    /// The node the inspector and the graph dock show, by path.
    pub selected_node_addr: Option<klipp_graph_format::path::NodePath>,
    /// The path drag a background result closed while its row was still
    /// held (`close_path_drag_before_edit`), with the clip selected then:
    /// the row's next tick reopens it, so the rest of the gesture is one
    /// more step, not one per tick. Forgotten as soon as the gesture can't
    /// be going on (`forget_stale_path_drag`).
    pub resumable_path_drag: Option<(String, Option<ClipId>)>,
    /// The effect track the keyframe curve editor shows, on which clip,
    /// while it is drawn and shows one: what Previous / Next keyframe walk
    /// then (#1401). Set by the panel each frame it's drawn, cleared when
    /// it isn't.
    pub curve_editor_track: Option<(ClipId, klipp_graph_format::path::NodePath, klipp_graph_format::ParamKey)>,
    /// The mask track the curve editor shows, likewise (#1947).
    pub curve_editor_mask_track: Option<(ClipId, crate::mask_tracks::MaskTrack)>,
    /// The id a clip's Corrector takes when the tabbed page's first edit
    /// makes it (M3 step 19): reserved, for the clip selected, while its
    /// grade has no enabled Corrector, so the rows and editors drawn before
    /// it exists name the node it will be (see `grade_view::snapshot_for`).
    pub pending_corrector: Option<(ClipId, klipp_nodes::NodeId)>,
    /// The graph the timeline grade's or a group stage's stand-in was put
    /// on with while that root has no grade yet: what the inspector drew,
    /// and what the root's first path edit begins from, so its node ids
    /// are the ones drawn (M3 step 19, see `grade_view::snapshot_for_root`).
    /// Each stand-in's draw sets it and nothing clears it when the
    /// selection moves on: it is read only for its own root, and only
    /// while that root still has no grade, so a stale one is never used.
    pub root_grade_start: Option<(klipp_graph_format::path::Root, klipp_nodes::NodeGraph)>,
    /// The split view's compared node: its clip, and the node whose
    /// picture the "before" half stops at.
    pub split_compare_node: Option<(ClipId, klipp_graph_format::path::NodePath)>,
    /// The graph dock is open.
    pub graph_dock_open: bool,
    /// The graph the dock shows.
    pub graph_dock_root: Option<klipp_graph_format::path::Root>,
    /// The groups the dock has descended into from its root, outermost
    /// first; empty at the root's top level.
    pub graph_dock_path: Vec<klipp_graph_format::NodeId>,

    // Grade stills (reference frame comparison)
    /// Saved grade reference stills for comparison.
    pub grade_stills: Vec<GradeStill>,
    /// Index of the active grade still being compared (None = no comparison).
    pub active_grade_still: Option<usize>,
    /// Whether split-view comparison with a grade still is active.
    pub grade_still_compare: bool,
    /// The clip the viewer wipes against (#899), its graded frame on the
    /// split's left; `None` for no clip wipe.
    pub wipe_clip: Option<ClipId>,

    /// Reference frame captured for the next ColorMatch run. Stored as
    /// raw RGBA bytes + dimensions; not persisted with the project,
    /// purely a working buffer between "Capture reference" and
    /// "Apply match" clicks in the inspector. The match algorithm
    /// produces a small `ColorMatchSettings` that IS persisted on
    /// the node, so the reference frame's bytes don't need to live
    /// past the compute step.
    pub color_match_reference: Option<(Vec<u8>, u32, u32)>,

    /// Remembered ClipId of whichever clip provided the current
    /// reference, so the timeline context menu can mark it with a
    /// "●" and the "Match Colour to Reference" entry can grey out on
    /// the clip that IS the reference. Parallel to
    /// `audio_reference_clip_id`. Cleared when
    /// `color_match_reference` is cleared.
    pub color_match_reference_clip: Option<klipp_core::ClipId>,

    /// Per-node output thumbnails for the V2 canvas, snapshotted
    /// from the current `FrameBuffer::node_thumbnail_batch`. Sourced by the
    /// engine on paused/post-seek re-grades when a non-empty
    /// `SetVisibleNodeThumbs` list is active. Rebuilt lazily in
    /// `update()` only when the engine's `node_thumbs_gen` advances.
    pub node_thumbs: std::collections::HashMap<klipp_compose::InspectAt, Vec<u8>>,
    pub node_thumbs_gen: u64,
    /// The asked-for node previews the render couldn't make, and why
    /// (#1747), from the same batch as `node_thumbs`.
    pub node_thumbs_unavailable: std::collections::HashMap<klipp_compose::InspectAt, String>,
    pub(crate) node_thumbnail_state: crate::app::node_thumbnails::ThumbnailState,

    /// Project-level reusable colour grades. Persisted to
    /// `.klipp/looks/{id}.json` on save, loaded back into this Vec
    /// on project open.
    pub looks: Vec<klipp_timeline::Look>,

    // Viewer overlay (zebra / false color)
    pub viewer_overlays: ViewerOverlays,
    pub frame_count: u64, // for zebra animation
    /// Pre-built 256-entry LUT for false color: luma → [R, G, B]. Rebuilt when palette changes.
    pub false_color_lut: [u8; 768],
    /// Pending false color palette switch from toolbar quick-switcher (applied next frame).
    pub pending_false_color_palette: Option<crate::prefs::FalseColorPalette>,

    // OFX plugins
    pub available_plugins: Vec<klipp_plugin::OfxPluginInfo>,

    // LADSPA audio plugins — discovered once at startup. Holds *all*
    // discovered plugins regardless of user-disable state so the
    // Audio Plugins dialog can render every row with its enable/
    // disable toggle. Filtering for the user-facing browser/picker
    // lives in `effect_registry`, which is rebuilt to exclude
    // disabled plugins on toggle and on startup.
    pub ladspa_plugins: Vec<klipp_audio::ladspa::LadspaPluginInfo>,

    /// All frei0r 1.x video plugins discovered at startup (and on
    /// Rescan). Mirrors the `ladspa_plugins` shape but for video FX
    /// — populates the runtime cache in
    /// `klipp_effects::frei0r_effect` so that
    /// `BuiltinEffect::Frei0r { type_id, .. }` lookups can resolve
    /// the dlopened plugin without going through disk discovery
    /// each render. `klipp-frei0r` discovery walks
    /// `/usr/lib/frei0r-1`, `/usr/local/lib/frei0r-1`, and
    /// `~/.frei0r-1/lib` (plus `FREI0R_PATH` override), 1+2-deep
    /// for vendor subdirs.
    pub frei0r_plugins: Vec<klipp_frei0r::Frei0rPluginInfo>,

    /// All LV2 audio plugins discovered at startup (and on Rescan).
    /// Populates the runtime cache in `klipp_audio::lv2` so that any
    /// `AudioEffectInstance.effect_type = "lv2:{uri}"` can be
    /// resolved without re-reading the system search paths each
    /// render. v1 supports plugins with ≤ 2 audio channels in/out
    /// and the control-port subset of LV2; surround / state-ext /
    /// MIDI / native UI deferred to LV2-NEXT-* queue items.
    pub lv2_plugins: Vec<klipp_audio::lv2::Lv2PluginInfo>,

    /// All CLAP audio plugins discovered at startup (and on Rescan).
    /// Populates the runtime cache in `klipp_audio::clap` so any
    /// `AudioEffectInstance.effect_type = "clap:{plugin_id}"` can
    /// resolve without re-walking the system search paths each
    /// render. v1 stub runtime — discovery + UI pipe ready, full
    /// process() lands with CLAP-NEXT-RUNTIME-WIRING.
    pub clap_plugins: Vec<klipp_audio::clap::ClapPluginInfo>,

    /// Plugins the user has explicitly disabled, keyed on the
    /// stable LADSPA `unique_id`. Persisted to
    /// `~/.config/klipp/audio_plugins.toml` on every toggle and
    /// loaded at startup before the first effect-registry build.
    /// Disabling a plugin that's already in a saved project is
    /// non-destructive: the existing AudioEffectInstance still
    /// loads through `dsp::create_effect`. The disable only blocks
    /// the plugin from the browser / "+ Add Effect" picker so the
    /// user can't make new picks against it.
    pub disabled_audio_plugins: std::collections::HashSet<u64>,

    // Unified effects registry (built-in + OFX merged)
    pub effect_registry: crate::browser::effect_registry::EffectRegistry,
    /// Mutually-exclusive source filter: All / Built-in / OFX. Replaces
    /// the previous pair of `effect_filter_builtin` + `effect_filter_ofx`
    /// bools that backed three competing colored pills.
    pub effect_source_filter: crate::browser::effects_tab::SourceFilter,
    /// Reserved for the GPU-only filter pill — wired in a follow-up.
    pub effect_filter_gpu: bool,
    pub effect_filter_hide_unsupported: bool,
    /// Effect presets (built-in + user-created)
    pub preset_manager: crate::effect_presets::PresetManager,
    /// The user's saved title templates (`~/.config/klipp/title_templates`).
    pub title_templates: crate::title_templates::TitleTemplateStore,
    /// User-authored composition templates with typed exposed parameters.
    pub composition_templates: crate::parameterized_templates::CompositionTemplateStore,
    /// The open save / rename / delete title-template dialog.
    pub title_template_dialog: Option<crate::title_templates::TemplateDialog>,
    /// The open credits-from-a-list dialog (#1514).
    pub credits_dialog: Option<crate::credits_dialog::CreditsDialog>,
    /// The open Normalize dialog (#1843).
    pub normalize_dialog: Option<crate::normalize_dialog::NormalizeDialog>,
    /// The open Find by Picture window (#2212).
    pub visual_search: Option<crate::visual_search_dialog::VisualSearch>,
    /// The open Fit Music to Length dialog (#2210).
    pub music_fit_dialog: Option<crate::music_fit_dialog::MusicFitDialog>,
    pub effect_thumbnails: crate::browser::effect_thumbnails::ThumbnailRenderer,

    // Render queue
    pub render_queue: klipp_export::RenderQueue,
    pub show_render_queue: bool,

    // Smooth playhead interpolation
    /// Last engine-reported playhead and the wall time it was received
    pub playhead_anchor: Timecode,
    pub playhead_anchor_wall: std::time::Instant,
    pub playhead_speed: f64,
    /// High-precision playhead for smooth display (not quantized to frame rate)
    pub playhead_smooth_secs: f64,

    // Inspector section collapse state
    /// Names of inspector sections that are currently collapsed.
    pub collapsed_sections: std::collections::HashSet<String>,
    /// Whether to persist collapsed sections across restarts.
    pub remember_inspector_layout: bool,

    // Audio backend status
    /// True if the audio output device was successfully opened (cpal/PipeWire).
    pub audio_backend_ok: bool,
    /// Audio backend sample rate (probed at startup).
    pub audio_sample_rate: u32,

    // ── Audio recording (#60 tier 2a/2b) ──────────────────────────────
    // None of these serialize — recording state is transient per app run.
    // AppState itself is not Serialize, so no serde annotations needed.
    /// True while a recording is in progress.
    pub is_recording: bool,
    /// Track to place the recorded clip on when recording stops.
    pub recording_track_id: Option<klipp_core::TrackId>,
    /// Destination WAV path of the active recording.
    pub recording_path: Option<std::path::PathBuf>,
    /// Stop-signal sender for the WAV writer thread. Sending `()` tells it
    /// to drain the final ring-buffer tail, patch the WAV header sizes, and
    /// exit cleanly.
    pub recording_stop_tx: Option<std::sync::mpsc::Sender<()>>,
    /// Live cpal input stream. Dropping halts capture; kept here so the
    /// stream lives as long as the recording session.
    pub recording_audio_input: Option<klipp_engine::audio_input::AudioInput>,
    /// Where the take being recorded lands (#114).
    pub recording_punch: Option<crate::punch_in::Punch>,
    /// A stopped take waiting for its file to load, to be placed then.
    pub pending_take: Option<(klipp_core::MediaId, crate::punch_in::Punch)>,
    /// The input opened for its meter while a track is armed and nothing
    /// records (#114): levels are seen before rolling.
    pub input_meter: Option<klipp_engine::audio_input::AudioInput>,
    /// The input could not be opened for metering: not tried again until
    /// the arming changes.
    pub input_meter_failed: bool,
    /// The input's level as the meter shows it, 0 to 1, falling back.
    pub input_level: f32,

    // Phase 6B: VU metering
    /// Per-track peak levels (display values with decay)
    pub track_peak_levels: Vec<(f32, f32)>,
    /// Master peak levels (display values with decay)
    pub master_peak_left: f32,
    pub master_peak_right: f32,
    /// Master volume control
    pub master_volume: f32,
    /// Master mute
    pub master_muted: bool,
    /// BS.1770-4 LUFS metering (raw from engine)
    pub lufs_momentary: f32,
    pub lufs_short_term: f32,
    pub lufs_integrated: f32,
    pub lufs_true_peak_dbfs: f32,
    /// Loudness range (EBU R128 LRA)
    pub lufs_lra: f32,
    /// What playback adds to be heard at the export's delivered loudness,
    /// as the engine last said (#1565).
    pub delivery_loudness: DeliveryLoudness,
    /// Rolling short-term LUFS history for the loudness-history scope.
    /// Capacity: 600 samples @ ~10 Hz = ~60 seconds of timeline. Pushed
    /// once per `LufsReadings` event (see app.rs handler) and popped from
    /// the front when the cap is exceeded.
    pub lufs_short_term_history: std::collections::VecDeque<f32>,
    /// Rolling history of FFT bin slices for the spectrogram waterfall.
    /// Each entry is one normalized spectrum snapshot (256 bins, 0.0–1.0).
    /// Capacity: 300 slices ≈ ~10s at 30fps refresh. Pushed from the
    /// spectrum refresh loop in `app.rs` and popped from the front when
    /// the cap is exceeded.
    pub spectrum_history: std::collections::VecDeque<Vec<f32>>,
    /// Smoothed display values (EMA-filtered for stable visual)
    pub lufs_momentary_smooth: f32,
    pub lufs_short_term_smooth: f32,
    /// Latched clipping indicator (true if any sample exceeded 0 dBFS since last reset)
    pub clip_indicator: bool,
    /// LUFS target from preferences (-23 EBU R128, -24 ATSC A/85)
    pub lufs_target: f32,
    /// Peak hold time in seconds from preferences
    pub peak_hold_time_secs: f32,
    /// Mixer: track name being edited (track_index, buffer)
    pub mixer_editing_name: Option<(usize, String)>,

    // Phase 7: Dynamic tracks
    pub confirm_remove_track: Option<TrackId>,
    /// Sequences whose tab was closed with its ×. Closing a tab only
    /// hides it: the sequence stays in the project, and the tab bar's
    /// sequence menu opens it again (#906).
    pub closed_sequence_tabs: std::collections::HashSet<SequenceId>,
    /// The sequence the tab menu asked to delete, waiting for the
    /// confirmation dialog.
    pub confirm_delete_sequence: Option<SequenceId>,
    pub show_markers_panel: bool,
    /// The Auto Align Audio dialog, open on a pair, with the method chosen
    /// (#615).
    pub auto_align: Option<(crate::auto_align::AlignPair, crate::auto_align::AlignMethod)>,
    /// A marker to open for editing in the Markers panel: a double-click
    /// on its flag, or Edit Marker… (#909).
    pub edit_marker_request: Option<klipp_core::MarkerId>,
    /// Visibility of the dedicated subtitle editor panel.
    pub show_subtitle_editor: bool,
    /// Visibility of the Transcript panel.
    pub show_transcript_panel: bool,
    /// Whether the pointer was over the Transcript panel on the last
    /// frame it was drawn. Delete over the panel cuts the selected words,
    /// and the global Delete shortcut stands aside; both read this same
    /// value in a frame, so the key goes to one of them, never both.
    pub transcript_panel_hovered: bool,
    /// Document ops a panel asked for (captions, transcript cuts):
    /// resolved against the timeline and applied as one undo step by
    /// the app on its next frame, through the same resolver the
    /// assistant's ops use.
    pub pending_doc_ops: Vec<DocOpsRequest>,
    /// Aspect versions waiting for their files' framing analysis before
    /// they are reframed (#2211), by sequence, so switching tabs meanwhile
    /// reframes the right one when it is next open.
    pub pending_refits: Vec<PendingRefit>,
    /// Clips kept matched followed their shot in a step just recorded
    /// (#2236): the engine is given the sequence again.
    pub matched_follow_sync: bool,
    /// What the open sequence was when its clips were last proposed roles
    /// from their sound (#2216): analysis, sequence, history, clip count
    /// and locked tracks, so the look is taken again only when one changes.
    pub role_sweep_key: Option<(u64, klipp_core::SequenceId, usize, usize, usize)>,
    /// Per-session editing state for the subtitle editor panel.
    pub subtitle_editor: SubtitleEditorState,
    /// Live state for a running project-consolidation operation (None = no
    /// consolidation in progress). Progress + completion arrive via the
    /// embedded mpsc channel; see consolidate::ConsolidateMessage.
    pub consolidate_progress: Option<ConsolidateProgress>,
    /// Editable destination path in the consolidate dialog.
    pub consolidate_dest_path: String,
    /// Dialog toggle: include proxy files.
    pub consolidate_include_proxies: bool,
    /// Dialog toggle: include skim transcodes.
    pub consolidate_include_skims: bool,
    /// Dialog toggle: after copying, zip the consolidated folder into
    /// `<dest_dir>.zip` alongside it. Default off.
    pub consolidate_create_zip: bool,
    /// Dialog toggle: after a successful zip, recursively delete the
    /// consolidated folder so only the archive remains. Default off.
    /// Only visible when `consolidate_create_zip` is true.
    pub consolidate_delete_folder_after_zip: bool,
    /// What the consolidate dialog copies (#77): 0 everything, 1 only the
    /// used media, 2 only what is used, cut with handles.
    pub consolidate_mode: u8,
    /// The handles kept either side of what is used, in seconds (#77).
    pub consolidate_handle_secs: f64,
    /// The dialog's summary and what it was worked out for (#77): worked
    /// out again when the options or the project change, not every frame.
    pub consolidate_summary: Option<(u64, klipp_project::consolidate::ConsolidateSummary)>,
    pub show_browser_panel: bool,
    /// When set, the browser panel should scroll to this media item.
    pub reveal_in_browser: Option<klipp_core::MediaId>,
    pub show_inspector_panel: bool,

    /// True when any egui popup/menu/combobox is open. Set at start of each frame.
    pub modal_or_popup_open: bool,

    // Phase 9: Preferences
    pub show_preferences: bool,
    /// A new interface size from the View menu (percent, 0 = Auto),
    /// applied and saved by the app, which holds the preferences.
    pub ui_scale_request: Option<u32>,
    /// A whole-window capture asked for (the file, the widest it may be),
    /// and whether egui has been asked for the screenshot yet.
    pub window_capture: Option<(std::path::PathBuf, Option<u32>, bool)>,
    /// Multicam mode (#1844): the Source viewer shows the angle grid of
    /// the multicam clip under the playhead, or of `multicam_opened`.
    /// Switched only by the Angles button, its shortcut, the View menu
    /// and opening a multicam clip; never by the playhead.
    pub multicam_mode: bool,
    /// The multicam clip last opened (double-clicked), shown in multicam
    /// mode when none is under the playhead.
    pub multicam_opened: Option<ClipId>,
    /// A multicam group of two angles or more is under the playhead (or
    /// the opened clip): the program viewer's angle bar follows this, not
    /// the mode.
    pub multicam_at_playhead: bool,
    /// The interface size in effect, for the menu's check mark.
    pub ui_scale_percent: u32,
    /// Auto-create tracks when dropping clips outside existing tracks (synced from prefs).
    pub auto_create_tracks: bool,

    // Per-project media generation overrides (None = use global pref)
    pub project_auto_filmstrips: Option<bool>,
    pub project_auto_skim: Option<bool>,
    /// Working colorspace the project's grade chain operates in.
    /// Driven by the Project Info panel's dropdown; engine receives
    /// the value via `klipp_engine::video_decoder::set_working_colorspace`
    /// at project load and on every change. Defaults to
    /// `Rec709Linear` so brand-new projects and back-compat loads
    /// of pre-Phase-E project files behave exactly like they did
    /// before.
    pub project_working_colorspace: klipp_core::WorkingColorspace,
    /// What the grade's controls act in (#467): BT.1886, or ACEScct or
    /// ACEScc under ACEScg. Mirrored into `klipp_core::set_grade_encoding`.
    pub project_grade_encoding: klipp_core::GradeEncoding,
    /// The output transform new exports start from (#466).
    pub project_default_odt: klipp_core::OdtChoice,
    /// The input format new clips take when their file says none (#466).
    pub project_default_input_format: Option<klipp_core::LogFormat>,
    /// The LUT new clips start their grade with (#466).
    pub project_default_input_lut: Option<std::path::PathBuf>,
    /// The look the viewer shows through, on screen only (#466).
    pub project_viewing_lut: Option<std::path::PathBuf>,
    /// The scopes measure after the viewing LUT (#466).
    pub project_scopes_after_viewing_lut: bool,
    /// The Colour Management window is open (#466).
    pub show_colour_management: bool,

    // Media panel
    pub show_media_panel: bool,

    // Remove media confirmation
    pub show_remove_media_confirm: Option<MediaId>,
    /// Delete unused media confirmation: (count, total_bytes)
    pub show_delete_unused_confirm: Option<(usize, u64)>,
    /// AI Model uninstall confirmation. `Some` when the user has
    /// clicked Uninstall on a row in the AI Models dialog and we're
    /// awaiting confirmation. The dialog renders a modal asking
    /// `"Delete <X>? Frees N MB"`; on confirm we remove the file from
    /// the cache + drop the cached generator instance through the
    /// shared registry's `clear_instance`.
    pub show_ai_model_uninstall_confirm: Option<klipp_ml::manifest::ModelEntry>,
    /// Bulk-uninstall confirmation. `true` when the user clicked
    /// "Uninstall all" in the AI Models dialog footer; the dialog
    /// renders a modal listing every installed model + total size
    /// freed, and on confirm loops through them all.
    pub show_ai_models_bulk_uninstall_confirm: bool,
    /// Stale-uninstall confirmation. `true` when the user clicked
    /// "Uninstall unused" — same shape as the bulk modal but
    /// pre-filtered to models that have been used at least once
    /// AND whose last_used is more than `STALE_DAYS` ago.
    pub show_ai_models_stale_uninstall_confirm: bool,
    /// "Add custom model" flow state. `Idle` when the modal isn't
    /// open; transitions to `Form` when the user clicks Add custom
    /// model…, then `Downloading` while the worker fetches, then
    /// `Confirming` once the download completes (showing computed
    /// SHA + size for the user to confirm), then back to `Idle`
    /// after register + persist or after a cancel.
    pub custom_model_add: crate::ai_tools::custom_install::CustomInstallState,
    /// Uninstall confirmation for a user-added custom model. Same
    /// shape as `show_ai_model_uninstall_confirm` but keyed on the
    /// `CustomModelEntry` instead of a manifest `ModelEntry` —
    /// the two flow through different removal paths because the
    /// custom side also has to drop the entry from
    /// `ai_tools_state.custom_models` + persist.
    pub show_ai_custom_uninstall_confirm:
        Option<crate::ai_tools::persistence::CustomModelEntry>,
    /// AI Models dialog filter / sort state. Persists across renders
    /// so the dialog remembers the user's selection while it's open.
    /// Resets on app launch — re-opening starts unfiltered.
    pub ai_models_dialog: crate::panels::ai_models::AiModelsDialogState,
    /// Export offline warning: list of (filename, timecode_string) for offline clips
    pub export_offline_warning: Option<(Vec<(String, String)>, Box<klipp_core::ExportSettings>)>,
    /// Export-target collides with a project media file — guard
    /// against silently overwriting source media mid-render.
    /// Tuple = (matching media filename, settings to re-dispatch on
    /// confirm). Surfaced as the "Export Will Overwrite Source"
    /// dialog with three options: pick another file, auto-rename
    /// with a numeric suffix, or overwrite anyway.
    pub export_source_overwrite_warning: Option<(String, Box<klipp_core::ExportSettings>)>,
    /// Clips whose tracked subject matte doesn't cover what an export
    /// would read, one line each: the export waits for them, never
    /// shipping a stale matte (#239, #985). Each: the clip, the command
    /// that tracks the rest, the line.
    pub export_matte_warning: Option<Vec<(klipp_core::ClipId, klipp_core::AppCommand, String)>>,

    // Usage count cache: MediaId → number of clips referencing it.
    // Rebuilt lazily when media_usage_cache_dirty is set (after any timeline edit).
    pub media_usage_cache: std::collections::HashMap<MediaId, usize>,
    pub media_usage_cache_dirty: bool,
    /// Moves every time an analysis result lands on a media item, so a
    /// panel caching what it read from the analysis knows to read again.
    pub analysis_generation: u64,

    // New Project dialog
    pub show_new_project_dialog: bool,
    /// The selects sequence stacked above the cut (#82), read-only, to
    /// skim and take ranges from; `None` when the strip is closed.
    pub selects_sequence: Option<klipp_core::SequenceId>,
    /// The range marked in the selects, in its own time.
    pub selects_range: Option<(Timecode, Timecode)>,
    /// The selects range being dragged down into the cut.
    pub selects_drag: Option<(klipp_core::SequenceId, Timecode, Timecode)>,
    /// The selects strip's height, in points.
    pub selects_height: f32,
    /// The stacked timelines' focus asked to swap, by its shortcut; the
    /// timeline panel does it as it draws them (#2324).
    pub stacked_focus_swap: bool,
    /// The selects stacked above the cut edited as a timeline of their
    /// own, not the read-only strip (#82).
    pub selects_edit: bool,
    /// Clips carried by a Ctrl-drag from one stacked timeline to the
    /// other: the sequence they are in, and which (#82).
    pub clip_carry: Option<(klipp_core::SequenceId, Vec<ClipId>)>,
    /// An AAF file waiting on OpenTimelineIO's converter (#2086).
    pub aaf_install: Option<crate::aaf_install::AafInstall>,
    /// An AAF being read through the converter, off the UI thread.
    pub aaf_reading: Option<crate::aaf_install::AafReading>,
    /// Where project templates are kept (#618); `None` for the user's
    /// configuration folder's.
    pub project_templates_dir: Option<std::path::PathBuf>,
    /// The Shared Library's media collections, across projects (#614).
    pub shared_library: crate::shared_library::SharedLibrary,
    /// Where the Shared Library is kept (#614); `None` for the user's
    /// configuration folder's.
    pub shared_library_dir: Option<std::path::PathBuf>,
    /// The collection the Library's SHARED view shows, and the one files
    /// go to when none is named (#614).
    pub shared_collection: String,
    /// The Save Project as Template dialog, when it is open (#618).
    pub save_template_dialog: Option<crate::panels::new_project_dialog::SaveTemplateDialog>,
    /// The clip whose Frame Glide is being framed in the viewer (#611):
    /// the viewer shows it without its move, its Start and End over it.
    pub frame_glide_clip: Option<ClipId>,
    /// A Track Subject waiting on its model's download (#239, #985): the
    /// command, sent once the model it names is there.
    pub pending_track_subject: Option<(klipp_core::AppCommand, String)>,
    /// The clips whose subject is being tracked (#239), with their media
    /// and folder, and how many frames the render could see when last
    /// looked (to refresh the picture as more come).
    pub tracking_subjects: std::collections::HashMap<ClipId, Tracking>,
    /// When the running passes were last looked at.
    pub track_subject_looked: Option<std::time::Instant>,
    /// The status bar's lines, one a running subject pass.
    pub status_tracking: Vec<String>,
    /// The caption translation running (#2213), for the status bar:
    /// what it is doing and how far.
    pub status_translation: Option<String>,
    /// A Frame Glide drag in the viewer, sent when it is let go.
    pub frame_glide_drag: Option<crate::panels::frame_glide_tool::GlideDrag>,
    // Batch rename dialog (for bin media items).
    pub show_batch_rename_dialog: bool,
    /// Find and replace in titles (#1717): open, what is looked for, its
    /// replacement, whether over every sequence, and the match picked.
    pub show_title_find: bool,
    pub title_find: klipp_document::find::TextQuery,
    pub title_find_replace: String,
    pub title_find_project: bool,
    pub title_find_selected: Option<usize>,
    pub batch_rename_ids: Vec<MediaId>,
    pub batch_rename_mode: crate::app::BatchRenameMode,
    pub batch_rename_find: String,
    pub batch_rename_replace: String,
    pub batch_rename_match_case: bool,
    pub batch_rename_prefix: String,
    pub batch_rename_suffix: String,
    pub batch_rename_start: u32,
    pub batch_rename_padding: u8,
    // Project Info window
    pub show_project_info: bool,
    /// File Info window — Some(path) when open, None when closed.
    /// Set from the file browser right-click menu.
    pub file_info: Option<std::path::PathBuf>,
    /// Lazily-loaded thumbnail texture for the currently-shown File Info window.
    /// Cleared when `file_info` is reset to None. Loaded from the shared
    /// media-pool disk cache (`klipp_media::disk_cache`).
    pub file_info_thumb: Option<egui::TextureHandle>,
    // Consolidate Project dialog
    pub show_consolidate_dialog: bool,

    // Sequence tab rename
    pub renaming_sequence_idx: Option<usize>,
    pub rename_sequence_buf: String,

    // Auto-save recovery dialog
    pub show_autosave_recovery: bool,
    pub autosave_recovery_path: Option<PathBuf>,
    pub autosave_original_path: Option<PathBuf>,

    // Audio keyframe rubber band drag state
    pub dragging_audio_keyframe: Option<DraggingKeyframe>,
    // Video keyframe rubber band drag state
    pub dragging_video_keyframe: Option<DraggingVideoKeyframe>,
    /// Clip whose flat rubber band line is being dragged (no-keyframes volume adjust)
    pub dragging_rubber_band: Option<ClipId>,
    /// True when the active flat-line rubber band drag (`dragging_rubber_band`)
    /// was initiated inside the per-track expandable automation lane.
    /// The drag-update logic checks this to decide whether to compute the new
    /// value against the inline clip rect or the lane sub-rect. Cleared
    /// alongside `dragging_rubber_band` on release. Only meaningful when
    /// `dragging_rubber_band.is_some()`.
    pub dragging_rubber_band_in_lane: bool,
    /// Bezier tangent handle drag state (audio)
    pub dragging_bezier_handle: Option<BezierHandleDrag>,
    /// Bezier tangent handle drag state (video)
    pub dragging_video_bezier_handle: Option<BezierVideoHandleDrag>,
    /// Audio EFFECT keyframe drag (rubber band rendering an effect param).
    pub dragging_audio_effect_keyframe: Option<DraggingEffectKeyframe>,
    /// Bezier tangent handle drag state for audio EFFECT keyframes.
    pub dragging_audio_effect_bezier_handle: Option<BezierEffectHandleDrag>,
    /// Keyframe interpolation right-click context menu
    pub keyframe_context_menu: Option<KeyframeContextMenu>,
    /// Per-lane param picker. Right-clicking the empty automation lane
    /// background opens a small popup that lets the user switch the
    /// lane between Volume / Pan / Gain without having to collapse +
    /// re-expand via the global pill picker.
    pub automation_lane_param_menu: Option<AutomationLaneParamMenu>,
    /// Keyframe clipboard for copy/paste between clips/params
    pub keyframe_clipboard: Option<KeyframeClipboard>,
    /// Keys copied in the Curve Editor (#2278), their times from the
    /// first one's, to paste at the playhead on any curve.
    pub curve_key_clipboard: Vec<klipp_core::Keyframe>,
    /// Currently selected keyframe on timeline (click to select, right-click for menu)
    pub selected_keyframe: Option<SelectedKeyframe>,
    /// The keys selected together, `selected_keyframe` the primary among
    /// them (#1958).
    pub key_selection: crate::key_selection::KeySelection,
    /// The animated parameter in focus (#2296), shared by the Inspector,
    /// the Curve Editor and the Dope Sheet.
    pub param_focus: Option<crate::param_focus::ParamFocus>,
    /// The track controls the user is holding now (#2306), by sequence
    /// index: what Touch and Latch record.
    pub automation_held: std::collections::HashSet<(usize, klipp_core::automation::TrackParam)>,
    /// The automation pass being recorded, and the track controls being
    /// moved over their keys (#2306).
    pub automation_pass: crate::automation_record::Pass,
    /// A drag of those keys in progress.
    pub multi_key_drag: Option<crate::key_selection::MultiKeyDrag>,
    /// The sequence camera's key that is selected, on the lane or in
    /// the curve editor. Separate from `selected_keyframe`, which is
    /// a clip's.
    pub selected_camera_key: Option<CameraKeyRef>,
    /// The right-click menu on a camera key.
    pub camera_key_menu: Option<CameraKeyMenu>,
    /// A camera key being dragged on the lane or in the curve editor.
    pub dragging_camera_key: Option<DraggingCameraKey>,
    /// Whether the camera's lane is shown under the ruler. Toggled from
    /// the track header column; off until asked for.
    pub show_camera_lane: bool,
    /// What the audio rubber band displays — clip-level Volume/Pan/Gain
    /// or a per-effect keyframable param.
    pub rubber_band_param: RubberBandParam,
    /// Which video parameter the timeline rubber band displays.
    pub video_rubber_band_param: VideoParam,
    /// The mask track the clip's key lane shows instead of a picture
    /// parameter (#1947), and the clip it is chosen on.
    pub timeline_mask_lane: Option<(ClipId, crate::mask_tracks::MaskTrack)>,
    /// A mask key dragged on that lane: its clip and track, its time now,
    /// and whether its gesture has begun.
    pub mask_lane_drag: Option<(ClipId, crate::mask_tracks::MaskTrack, f64, bool)>,
    /// Any other keyed parameter the clip's key lane shows, picked from
    /// the clip's menu (#2307): an effect's, a grade node's, a title
    /// layer's, and the clip it is picked on.
    pub timeline_param_lane: Option<(ClipId, KeyframeParamKind)>,
    /// A key dragged on that lane.
    pub param_lane_drag: Option<ParamLaneDrag>,
    /// Audio prefs synced from preferences (fade overlay mode, show rubber band, show waveform)
    pub audio_fade_overlay_mode: crate::prefs::FadeOverlayMode,
    pub audio_show_rubber_band: bool,
    pub audio_show_waveform: bool,
    pub audio_anti_click_fade: bool,
    /// A fade added to a clip (its Add Fade In or Out, or a fade handle
    /// dragged out from none): how long, and its curve. The audio
    /// preferences' default fade.
    pub default_fade_secs: f32,
    pub default_fade_curve: klipp_core::FadeCurve,
    /// See `prefs::AudioPrefs::show_crossfade_indicators`: a fade
    /// handoff between two butted clips, not a crossfade.
    pub audio_show_crossfade_indicators: bool,
    /// Per-track waveform amplitude zoom. Default is 1.0 when a key is absent.
    /// Alt+scroll over an audio track adjusts this; right-click track header resets it.
    /// Clamped to [0.1, 8.0] by the UI handlers.
    pub waveform_zoom: std::collections::HashMap<TrackId, f32>,
    /// What Copy Track Attributes kept, for Paste Track Attributes (#846).
    pub copied_track_settings: Option<(String, klipp_timeline::TrackSettings)>,

    /// The clip currently designated as the audio-sync reference (set via the
    /// clip context menu). `AlignToAudioReference` shifts a target clip's
    /// timeline position to align its audio to this reference.
    pub audio_reference_clip_id: Option<ClipId>,

    /// Fade handle drag state: (clip_id, true=fade_in, false=fade_out, original_fade_secs)
    pub dragging_fade_handle: Option<(ClipId, bool, f32)>,

    // Proxy generation tracking
    /// Media IDs currently generating proxies — shown as status in UI
    pub proxy_generating: std::collections::HashSet<MediaId>,
    /// Per-media proxy generation progress (0.0..1.0)
    pub proxy_progress: std::collections::HashMap<MediaId, f32>,
    /// Per-media proxy generation start time (for ETA calculation)
    pub proxy_start_times: std::collections::HashMap<MediaId, std::time::Instant>,
    /// Last proxy generation message (toast-like feedback)
    pub proxy_status_message: Option<(String, std::time::Instant)>,
    /// Whether the proxy toast was dismissed by the user
    pub proxy_toast_dismissed: bool,

    // Viewer transform controls
    /// Active drag on a viewer transform handle
    pub viewer_transform_drag: Option<ViewerTransformDrag>,
    /// A drag on the selected clip's motion path in the viewer (#2276).
    pub motion_path_drag: Option<crate::panels::motion_path::MotionPathDrag>,
    /// A corner-pin handle drag in the viewer.
    pub viewer_corner_pin_drag: Option<ViewerCornerPinDrag>,
    pub surface_tool: Option<SurfaceTool>,
    /// Whether on-screen transform handles are shown when a clip is selected
    pub show_transform_controls: bool,
    /// Currently selected object inside a Title clip. The first
    /// component is the parent clip's id; the second is the object's
    /// id within that clip's `TitleData.objects` list. `None` means
    /// no per-object selection — viewer behaviour falls back to
    /// clip-level transform handles. Set by clicking an object's
    /// bounding box on the viewer; cleared on clip-deselect.
    pub selected_title_object: Option<(ClipId, klipp_timeline::TitleObjectId)>,
    /// Active drag of a Title-object bounding box / handle on the
    /// viewer. `None` outside drag.
    pub title_object_drag: Option<TitleObjectDrag>,
    /// Active drag on a viewer mask handle
    pub viewer_mask_drag: Option<ViewerMaskDrag>,
    /// What the Mask tool is doing in the viewer (#74).
    pub mask_edit: crate::mask_edit::MaskEdit,

    /// Whether the curve editor panel is visible
    pub show_curve_editor: bool,
    /// Whether the audio mixer panel is visible
    pub show_audio_mixer: bool,
    /// Whether the history panel is visible
    pub show_history_panel: bool,
    /// Command palette state
    pub show_command_palette: bool,
    pub command_palette_query: String,
    pub command_palette_selected: usize,

    /// Active model-download state machine. Drives the consent
    /// dialog + progress dialog rendered each frame in app.rs.
    /// Default `Idle` paints nothing.
    pub model_download: crate::model_download::ModelDownloadState,

    /// AI Tools panel state — task selection, last mask, panel
    /// visibility. Default closes the panel; flipping
    /// `panel_visible` opens it. Wired through the registry below.
    pub ai_tools_state: crate::ai_tools::AiToolsState,
    /// Registry of `MaskGenerator` impls. Built once at app start
    /// with the stub generator pre-registered; backend crates (e.g.
    /// `klipp-ml-candle`, `klipp-ml-ort`) add their models in later
    /// phases.
    pub ai_tools_registry: crate::ai_tools::AiToolsRegistry,
    /// Worker-side registry handle. Cloned cheaply onto the inference
    /// thread by [`crate::ai_tools::spawn_inference`]. Carries the
    /// same set of registered models as `ai_tools_registry` (today
    /// just the stub).
    pub ml_registry: crate::ai_tools::RegistryHandle,

    /// Available system font families (populated at startup from font discovery)
    pub available_fonts: Vec<String>,
    /// The font picker's starred and recent families, as the
    /// preferences hold them (#1719).
    pub favourite_fonts: Vec<String>,
    pub recent_fonts: Vec<String>,
    /// The font picker's previews, each family in its own face.
    pub font_previews: crate::inspector::font_picker::FontPreviews,
    /// The missing-fonts dialog, while it is open (#1720).
    pub missing_fonts_dialog: Option<crate::missing_fonts::MissingFontsDialog>,

    /// Pending title data for styled title creation (consumed by CreateTitleClip handler)
    pub pending_title_data: Option<klipp_timeline::TitleData>,
    /// Frames left in which the Title inspector should take keyboard
    /// focus on its text field: set when a title is added.
    pub focus_title_text: u8,
    /// Title clip that needs its overlay texture refreshed on the next frame.
    pub pending_title_preview_clip: Option<klipp_core::ClipId>,
    /// Phase 4C — track-matte preview thumbnail wiring.
    /// `matte_thumb_for` tags which consumer the cached `matte_thumb_handle`
    /// belongs to; the inspector skips painting if the cached id has
    /// drifted from the clip currently in view (e.g. user clicked a
    /// different clip and the engine hasn't emitted a fresh preview yet).
    pub matte_thumb_for: Option<klipp_core::ClipId>,
    pub matte_thumb_handle: Option<egui::TextureHandle>,
    /// `matte_preview_active_target` mirrors what the engine currently
    /// thinks the preview target is (last value the inspector dispatched
    /// via `SetInspectorMattePreviewTarget`). Used to debounce the
    /// dispatch — without it we'd send a command on every paint.
    pub matte_preview_active_target: Option<klipp_core::ClipId>,
    /// Visual letter picker — per-title-clip cache of glyph positions
    /// for the HYPERZOOM inspector preview. Keyed by the title clip's
    /// id (NOT the hyperzoom source's id — multiple sources may pick
    /// from the same title). Recomputed via
    /// `klipp_title::ensure_glyph_positions` when the cached
    /// `layout_hash` no longer matches the title's current
    /// `TitleData` + render frame size.
    ///
    /// Wrapped in `RefCell` so the inspector — which runs through
    /// `&AppState` references — can write to the cache without
    /// propagating `&mut` up the entire `show_clip_video` →
    /// `show_hyperzoom_section` call chain. Single-threaded UI; no
    /// concurrency concern.
    ///
    /// Cache primitives live in `klipp-title` so the engine's
    /// glyph-anchored focal-point resolver (HZ-NEXT-GLYPH-LOCK) can
    /// share the same shape — different consumer, identical logic.
    pub glyph_position_cache: std::cell::RefCell<std::collections::HashMap<
        klipp_core::ClipId,
        klipp_title::GlyphCacheEntry,
    >>,
    /// Letter-picker preview texture — title bitmap rasterised at the
    /// inspector preview's pixel size. Owned by AppState so the App
    /// layer can manage the handle's lifecycle through
    /// `defer_drop_texture`; the inspector only ever reads it.
    /// Creating + dropping a `TextureHandle` mid-frame from inside
    /// the inspector trips wgpu's "Texture has been destroyed"
    /// validation.
    pub letter_picker_texture: Option<egui::TextureHandle>,
    /// Cache key for `letter_picker_texture` —
    /// `(title_clip_id, layout_hash, frame_w, frame_h)`. The App
    /// refreshes the texture only when this tuple changes; the
    /// `layout_hash` covers every TitleData field that affects
    /// rendering output (text, font, style, position, …).
    pub letter_picker_texture_for: Option<(klipp_core::ClipId, u64, u32, u32)>,
    /// Set during scrub when no title clip is under the playhead — signals
    /// the next `upload_title_overlay` to clear the overlay so stale titles
    /// don't linger as the user drags past them.
    pub pending_title_overlay_clear: bool,
    /// Clear title overlay texture (animation preset applied — text starts invisible)
    pub clear_title_overlay: bool,

    /// Duration (in seconds) for title animation fade presets.
    pub title_fade_duration: f32,

    /// Multicam mode is on and has a group to show: the Source viewer is
    /// the angle grid.
    pub multicam_viewer_active: bool,
    /// The angles of the current multicam group, in order (0 the
    /// primary). Not assumed to run 0..n: labelled by position, a group
    /// lacking an angle named the wrong camera.
    pub multicam_angles: Vec<u8>,
    /// The angle the group's cut list has in effect at the playhead.
    pub multicam_active_angle: u8,

    // Viewer text edit mode (double-click title in viewer)
    /// When true, the viewer shows an editable text overlay for a title clip.
    pub viewer_text_edit: bool,
    /// When inline-edit is active on a specific Title-object (the
    /// preferred path), tracks which object inside the clip is
    /// being edited. `None` while the clip-wide editor is active
    /// (legacy double-click anywhere on the viewer flow).
    pub viewer_text_edit_object: Option<klipp_timeline::TitleObjectId>,
    /// The text being edited (clone of TitleData.text, synced on every change).
    pub viewer_text_edit_buffer: String,
    /// Clip being text-edited.
    pub viewer_text_edit_clip: Option<ClipId>,
    /// Whether to request focus on the text edit widget (first frame only).
    pub viewer_text_edit_request_focus: bool,
    /// Whether the in-place editor selects all of its text as it next
    /// takes the keys, so typing replaces it: set for a title the Text
    /// tool has just made (#1713).
    pub viewer_text_edit_select_all: bool,
    /// Character selection range in viewer text edit mode (character indices, start..end).
    /// This is updated ONLY when the TextEdit has focus and reports a selection.
    /// It persists when focus moves to the inspector so style changes apply to it.
    pub viewer_text_selection: Option<(usize, usize)>,
    /// Locked copy of the selection that survives focus changes.
    /// Set whenever a real selection is made, cleared only on explicit deselect or exit.
    pub viewer_text_selection_locked: Option<(usize, usize)>,
    /// Cursor position (character index) in viewer WYSIWYG text editor.
    pub viewer_text_cursor: usize,
    /// Cached character positions from title renderer (frame pixel coords).
    /// Recomputed when title text/style changes.
    pub viewer_char_positions: Vec<klipp_title::CharPosition>,
    /// Anchor position for shift-click / shift-arrow selection extension.
    pub viewer_text_select_anchor: Option<usize>,
    /// Wall-clock time of last cursor movement (for blink reset).
    pub viewer_text_cursor_blink_time: f64,
    /// Font size at current cursor position (resolved from style runs).
    /// Updated when cursor moves. Inspector shows this value.
    pub viewer_text_cursor_font_size: Option<f32>,
    /// Color at current cursor position (resolved from style runs).
    pub viewer_text_cursor_color: Option<[u8; 4]>,
}

/// A saved reference frame for grading comparison. Persistence is via
/// PNG sidecars in `<project>/media/stills/` indexed by `filename`;
/// only `name + filename` survive in JSON, the rgba pixels are loaded
/// from disk on project open.
#[derive(Clone)]
pub struct GradeStill {
    pub name: String,
    /// Sidecar PNG filename (e.g. `still_1700000000.png`). Empty until
    /// the still is saved or attached to a project on disk.
    pub filename: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Which clip, and when, the still was grabbed from.
    pub source: Option<klipp_project::StillSource>,
    /// That clip's grade at the time: what Apply Grade from Still puts
    /// on other clips. `None` for stills from before stills kept it.
    pub grade: Option<klipp_nodes::NodeGraph>,
    /// That grade's node colour labels, by path inside it (#1866).
    pub grade_labels: Vec<klipp_timeline::GradeNodeLabel>,
    pub texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for GradeStill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GradeStill")
            .field("name", &self.name)
            .field("filename", &self.filename)
            .field("rgba_len", &self.rgba.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("has_texture", &self.texture.is_some())
            .finish()
    }
}

/// Eyedropper mode — determines what happens when the user clicks the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EyedropperMode {
    #[default]
    None,
    WhiteBalance,
    Qualifier,
    /// A hue for a Hue/Sat curve (#612).
    HueCurve,
    /// A 3D qualifier stroke (#895): dragged, not clicked.
    QualifierStroke,
}

/// A 3D qualifier stroke being sampled (#895): the node, whether it takes
/// colours out, the samples read so far and how many are still to come.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeSampling {
    pub clip_id: ClipId,
    pub node_id: uuid::Uuid,
    pub remove: bool,
    pub samples: Vec<[f32; 3]>,
    pub pending: usize,
}

/// Messages sent from the consolidation background thread to the UI thread.
///
/// Ordering guarantees: `Progress`* then `Done`. If the optional archive
/// phase runs, `ArchiveProgress`* then `ArchiveDone` follow, strictly after
/// `Done`.
#[derive(Debug)]
pub enum ConsolidateMessage {
    Progress {
        done: usize,
        total: usize,
        filename: String,
    },
    Done(Result<klipp_project::consolidate::ConsolidateResult, String>),
    /// Phase 2 archive progress in bytes (granular per-file).
    ArchiveProgress {
        bytes_done: u64,
        bytes_total: u64,
        filename: String,
    },
    /// Phase 2 terminal message. On success carries the zip path; on failure
    /// the error string (partial zip is already deleted by the writer).
    ArchiveDone(Result<std::path::PathBuf, String>),
}

/// Live state for an active consolidation operation. Dropped when the
/// operation completes (success or failure).
pub struct ConsolidateProgress {
    pub files_done: usize,
    pub files_total: usize,
    pub current_file: String,
    /// Set at construction time from the dialog's "Also create .zip archive"
    /// checkbox. The drain uses it to know whether to keep the progress
    /// struct alive after Phase 1 `Done` (archive messages will follow) or
    /// drop it immediately (Phase 1 was the only phase).
    pub expects_archive: bool,
    /// Set to true once the first `ArchiveProgress` or `ArchiveDone` message
    /// arrives. Keeps the dialog in archive mode for status rendering.
    pub archiving: bool,
    pub archive_bytes_done: u64,
    pub archive_bytes_total: u64,
    pub receiver: std::sync::mpsc::Receiver<ConsolidateMessage>,
}

impl std::fmt::Debug for ConsolidateProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsolidateProgress")
            .field("files_done", &self.files_done)
            .field("files_total", &self.files_total)
            .field("current_file", &self.current_file)
            .field("expects_archive", &self.expects_archive)
            .field("archiving", &self.archiving)
            .field("archive_bytes_done", &self.archive_bytes_done)
            .field("archive_bytes_total", &self.archive_bytes_total)
            .finish_non_exhaustive()
    }
}

/// Per-session editing state for the subtitle editor panel.
/// Not persisted — resets each time the panel opens.
#[derive(Debug, Clone, Default)]
pub struct SubtitleEditorState {
    /// Which clip's text field is being edited (None = display mode).
    pub editing_text: Option<ClipId>,
    /// Text buffer for the active text edit.
    pub text_buf: String,
    /// Which timecode field is being edited (None = display mode).
    /// Scoped by egui::Id so start- and end-fields of the same row don't collide.
    pub editing_timecode: Option<egui::Id>,
    /// Timecode edit buffer.
    pub timecode_buf: String,
    /// Ripple mode — when true, timing changes shift downstream clips.
    pub ripple: bool,
    /// Selected subtitle track (None = none selected / no filter). Used by the
    /// track-grouped editor to highlight the active track.
    pub selected_subtitle_track: Option<klipp_core::TrackId>,
    /// Set of subtitle track IDs currently collapsed in the editor.
    pub subtitle_track_collapsed: std::collections::HashSet<klipp_core::TrackId>,
    /// Track being renamed inline (header double-click). Name edit buffer.
    pub editing_track_name: Option<klipp_core::TrackId>,
    pub track_name_buf: String,
    /// The "Captions from Transcript" form, when open.
    pub captions_form: Option<CaptionsForm>,
    /// The caption style being edited for a track, before it is saved (#70).
    pub style_draft: Option<(klipp_core::TrackId, klipp_timeline::CaptionStyle)>,
}

/// How to make captions from the transcript: the settings of
/// `Op::Captions`, as the subtitle editor's form holds them.
#[derive(Debug, Clone)]
pub struct CaptionsForm {
    pub max_chars: usize,
    pub max_lines: usize,
    /// Only between the sequence's in and out points.
    pub in_out_only: bool,
    /// Make them again over captions already there.
    pub replace: bool,
}

impl Default for CaptionsForm {
    fn default() -> Self {
        Self { max_chars: 42, max_lines: 2, in_out_only: false, replace: false }
    }
}

/// Per-viewer zoom + pan state. `zoom == 0.0` is the "Fit" sentinel
/// (letterbox / pillarbox to the viewer's video area); any other value
/// is a multiplier on source-native pixels (`1.0` = 1:1). `pan` is
/// the offset in screen pixels from the viewer's geometric center,
/// applied only when zoomed in. Scroll-wheel zoom maintains the
/// cursor-anchor invariant; middle-mouse drag updates `pan` directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerView {
    pub zoom: f32,
    pub pan: egui::Vec2,
}

impl Default for ViewerView {
    fn default() -> Self {
        Self { zoom: 0.0, pan: egui::Vec2::ZERO }
    }
}

/// Independent viewer overlay toggles — multiple can be active simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewerOverlays {
    pub zebras: bool,
    pub false_color: bool,
    pub focus_peaking: bool,
    pub safe_areas: bool,
    pub grid: bool,
    pub checkerboard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

/// The units a waveform, parade or histogram graticule is marked in. The
/// trace always spans the full 10-bit code range, 0 to 1023; the units
/// say what each height is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeScale {
    #[default]
    TenBit,
    /// 0 IRE at code 64 and 100 IRE at code 940, the legal range of a
    /// 10-bit signal: a full-range picture reaches from -7.3 to 109.5.
    Ire,
    /// Percent of the full range: 0 at code 0, 100 at code 1023.
    Percent,
    /// Display light in cd/m² on the PQ curve the scopes measure on
    /// while HDR monitoring is on; the graticule takes it by itself then.
    Nits,
}

/// The first and last legal codes of a 10-bit signal.
pub const LEGAL_BLACK: f32 = 64.0;
pub const LEGAL_WHITE: f32 = 940.0;

/// One mark of a graticule: its value in the scale's units, and whether
/// it is a major mark, drawn strong and labelled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScopeTick {
    pub value: f32,
    pub major: bool,
}

impl ScopeScale {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TenBit => "10-bit",
            Self::Ire => "0\u{2013}100 IRE",
            Self::Percent => "0\u{2013}100%",
            Self::Nits => "nits (PQ)",
        }
    }
    pub fn button_label(&self) -> &'static str {
        match self {
            Self::TenBit => "10-bit",
            Self::Ire => "IRE",
            Self::Percent => "%",
            Self::Nits => "nits",
        }
    }

    /// Where `value`, in these units, sits on the trace: 0 at the bottom,
    /// 1 at the top.
    pub fn fraction(self, value: f32) -> f32 {
        match self {
            Self::TenBit => value / 1023.0,
            Self::Percent => value / 100.0,
            Self::Ire => (LEGAL_BLACK + value / 100.0 * (LEGAL_WHITE - LEGAL_BLACK)) / 1023.0,
            Self::Nits => klipp_color::hdr::pq_encode_nits(value.max(0.0)),
        }
    }

    /// The value in these units at `fraction` of the trace's height.
    pub fn value(self, fraction: f32) -> f32 {
        match self {
            Self::TenBit => fraction * 1023.0,
            Self::Percent => fraction * 100.0,
            Self::Ire => (fraction * 1023.0 - LEGAL_BLACK) / (LEGAL_WHITE - LEGAL_BLACK) * 100.0,
            Self::Nits => klipp_color::hdr::pq_decode_nits(fraction.clamp(0.0, 1.0)),
        }
    }

    /// The graticule's marks, from the bottom of the trace up. Every mark
    /// lies on the trace: IRE runs from -5 to 105, the codes a full-range
    /// picture reaches.
    pub fn ticks(self) -> Vec<ScopeTick> {
        let tick = |value: f32, major: bool| ScopeTick { value, major };
        match self {
            Self::TenBit => {
                let mut out: Vec<ScopeTick> = (0..1023).step_by(32).map(|v| tick(v as f32, v % 128 == 0)).collect();
                out.push(tick(1023.0, true));
                out
            }
            Self::Percent => (0..=20).map(|i| tick(i as f32 * 5.0, i % 2 == 0)).collect(),
            Self::Ire => (-1..=21).map(|i| tick(i as f32 * 5.0, i % 2 == 0)).collect(),
            Self::Nits => [
                (0.0, true), (1.0, true), (2.0, false), (5.0, false), (10.0, true), (20.0, false), (50.0, false),
                (100.0, true), (203.0, true), (500.0, false), (1000.0, true), (2000.0, false), (4000.0, false),
                (10_000.0, true),
            ]
            .into_iter()
            .map(|(v, m)| tick(v, m))
            .collect(),
        }
    }

    /// A mark's label: the value, thousands of nits as "k".
    pub fn tick_label(self, value: f32) -> String {
        match self {
            Self::Nits if value >= 1000.0 => format!("{}k", (value / 1000.0).round() as i32),
            _ => format!("{}", value.round() as i32),
        }
    }

    /// The legal range, codes 64 to 940, in these units — none in nits,
    /// where an HDR signal has no such range.
    pub fn legal_range(self) -> Option<(f32, f32)> {
        match self {
            Self::Nits => None,
            _ => Some((self.value(LEGAL_BLACK / 1023.0), self.value(LEGAL_WHITE / 1023.0))),
        }
    }
}

/// Pixel data sampled from the viewer for scope cursor overlay.
#[derive(Debug, Clone, Copy)]
pub struct ScopeCursorData {
    /// Normalised position in the video frame (0.0–1.0).
    pub x_frac: f32,
    pub y_frac: f32,
    /// The **display-encoded** values under the pointer, as shown on
    /// screen — these come from the framebuffer, which has been through
    /// the display transform.
    pub r: f32,
    pub g: f32,
    pub b: f32,
    /// Luma and CbCr, in the project's basis, of those display values.
    pub luma: f32,
    pub cb: f32,
    pub cr: f32,
    /// The picture's own linear value there, in the working space, when the
    /// engine is publishing samples for it. This is what the chromaticity
    /// scope plots, so it is what its cursor has to read: the display
    /// values above have already been fitted to the screen's gamut and
    /// cannot be turned back into the colour the picture holds.
    pub linear_working: Option<[f32; 3]>,
}

impl ScopeCursorData {
    /// From the display-encoded values under the pointer, measured in
    /// `basis` so the readouts agree with the traces they sit on.
    pub fn from_display(
        x_frac: f32,
        y_frac: f32,
        sr: f32,
        sg: f32,
        sb: f32,
        basis: klipp_color::scopes::ScopeBasis,
        linear_working: Option<[f32; 3]>,
    ) -> Self {
        let luma = basis.luma([sr, sg, sb]);
        let (cb, cr) = basis.chroma([sr, sg, sb]);
        Self { x_frac, y_frac, r: sr, g: sg, b: sb, luma, cb, cr, linear_working }
    }

    /// Convert sRGB to HSL.
    pub fn to_hsl(&self) -> (f32, f32, f32) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let l = (max + min) / 2.0;
        if (max - min).abs() < 1e-6 {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        let h = if (max - self.r).abs() < 1e-6 {
            (self.g - self.b) / d + if self.g < self.b { 6.0 } else { 0.0 }
        } else if (max - self.g).abs() < 1e-6 {
            (self.b - self.r) / d + 2.0
        } else {
            (self.r - self.g) / d + 4.0
        };
        (h * 60.0, s, l)
    }
}

// ── Floating scope window types ─────────────────────────────────────────

/// Active tab in the Clip Properties modal dialog (#47). Session state
/// only — not persisted across runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipPropertiesTab {
    #[default]
    Info,
    File,
    Tags,
    Markers,
    Usage,
    Streams,
    Metadata,
}

impl ClipPropertiesTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::File => "File",
            Self::Tags => "Tags",
            Self::Markers => "Markers",
            Self::Usage => "Usage",
            Self::Streams => "Streams",
            Self::Metadata => "Metadata",
        }
    }

    pub const ALL: [ClipPropertiesTab; 7] = [
        Self::Info, Self::File, Self::Tags, Self::Markers,
        Self::Usage, Self::Streams, Self::Metadata,
    ];
}

/// Scope type for floating window panes (extends ScopeMode with CIE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeType {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
    CIEChromaticity,
    /// The picture repainted by exposure, with a band that means "skin
    /// is exposed as the look asks". A colourist cannot judge exposure
    /// by looking, because the eye adapts; this paints the answer onto
    /// the face itself.
    FalseColour,
    AudioSpectrum,
    /// Audio phase/correlation goniometer — Lissajous plot of L vs R plus
    /// a Pearson-correlation strip meter. Audio-only (skips the video
    /// frame-buffer lock, same as `AudioSpectrum`).
    AudioCorrelation,
    /// Rolling LUFS history graph — short-term LUFS over the last ~60s
    /// with the user's loudness target drawn as a horizontal line.
    LoudnessHistory,
    /// Scrolling FFT waterfall — x=time, y=frequency, color=magnitude.
    /// Rolling 10-second history of normalized spectrum slices; audio-only
    /// (skips the video frame-buffer lock, same as other audio scopes).
    Spectrogram,
}

impl From<ScopeMode> for ScopeType {
    fn from(m: ScopeMode) -> Self {
        match m {
            ScopeMode::Waveform => Self::Waveform,
            ScopeMode::Parade => Self::Parade,
            ScopeMode::Vectorscope => Self::Vectorscope,
            ScopeMode::Histogram => Self::Histogram,
        }
    }
}

impl ScopeType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Waveform => "Waveform",
            Self::Parade => "Parade",
            Self::Vectorscope => "Vectorscope",
            Self::Histogram => "Histogram",
            Self::CIEChromaticity => "CIE Chromaticity",
            Self::FalseColour => "False Colour",
            Self::AudioSpectrum => "Audio Spectrum",
            Self::AudioCorrelation => "Audio Correlation",
            Self::LoudnessHistory => "Loudness History",
            Self::Spectrogram => "Spectrogram",
        }
    }

    pub const ALL: [ScopeType; 10] = [
        Self::Waveform, Self::Parade, Self::Vectorscope, Self::Histogram, Self::CIEChromaticity, Self::FalseColour,
        Self::AudioSpectrum, Self::AudioCorrelation, Self::LoudnessHistory, Self::Spectrogram,
    ];

    /// Convert to ScopeMode for the existing render pipeline (CIE/Audio scopes have no video render mode).
    pub fn to_scope_mode(&self) -> Option<ScopeMode> {
        match self {
            Self::Waveform => Some(ScopeMode::Waveform),
            Self::Parade => Some(ScopeMode::Parade),
            Self::Vectorscope => Some(ScopeMode::Vectorscope),
            Self::Histogram => Some(ScopeMode::Histogram),
            Self::CIEChromaticity => None,
            Self::FalseColour => None,
            Self::AudioSpectrum => None,
            Self::AudioCorrelation => None,
            Self::LoudnessHistory => None,
            Self::Spectrogram => None,
        }
    }
}

/// Layout configuration for the floating scope window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeLayout {
    OnePane,
    TwoPane,
    #[default]
    FourPane,
    NinePane,
}

impl ScopeLayout {
    pub fn pane_count(&self) -> usize {
        match self {
            Self::OnePane => 1,
            Self::TwoPane => 2,
            Self::FourPane => 4,
            Self::NinePane => 9,
        }
    }

    /// Grid dimensions (cols, rows) for this layout.
    pub fn grid(&self) -> (usize, usize) {
        match self {
            Self::OnePane => (1, 1),
            Self::TwoPane => (2, 1),
            Self::FourPane => (2, 2),
            Self::NinePane => (3, 3),
        }
    }
}

/// Waveform channel selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WaveformChannel {
    #[default]
    Y,
    R,
    G,
    B,
}

impl WaveformChannel {
    pub fn label(&self) -> &'static str {
        match self { Self::Y => "Y", Self::R => "R", Self::G => "G", Self::B => "B" }
    }
}

/// Skin tone indicator angle (degrees) — standard vectorscope convention.
pub const SKIN_TONE_ANGLE_DEGREES: f32 = 123.0;

/// Waveform trace base colour (monochrome mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WaveformPaintMode {
    /// Classic green phosphor (default).
    #[default]
    Green,
    /// Warm yellow trace.
    Yellow,
    /// Neutral white trace.
    White,
    /// Per-channel RGB — R channel red, G channel green, B channel blue.
    Classic,
}

/// Per-pane scope settings — unified struct with all per-type fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneScopeSettings {
    // ── Common ──
    pub waveform_channel: WaveformChannel,
    pub low_pass: bool,
    pub bloom: bool,
    pub quality: klipp_core::ScopeQuality,
    pub scale: ScopeScale,

    // ── Waveform-specific ──
    pub colorize: bool,
    pub extents: bool,
    pub waveform_brightness: f32,
    pub graticule_brightness: f32,
    pub show_reference_levels: bool,
    pub reference_low: f32,
    pub reference_high: f32,

    // ── Vectorscope-specific ──
    pub vectorscope_brightness: f32,
    pub vectorscope_graticule_brightness: f32,
    pub low_range: f32,
    pub high_range: f32,
    pub show_skin_tone_indicator: bool,
    pub show_graticule: bool,

    // ── Parade-specific ──
    pub overlay_on_viewer: bool,

    // ── Scope cursor (picker overlay) ──
    pub show_scope_cursor: bool,
    /// Half-size of the sample box (0 = 1×1 single pixel, 2 = 5×5, up to 7 = 15×15).
    pub scope_cursor_sample_radius: u32,

    // ── Additional reference / display options (serde-defaulted for backward compat) ──
    /// Vectorscope: draw the NTSC I/Q axes (I = skin tone, Q = perpendicular).
    #[serde(default)]
    pub show_iq_lines: bool,
    /// Waveform: base trace colour in monochrome mode (Green/Yellow/White/Classic).
    #[serde(default)]
    pub waveform_paint_mode: WaveformPaintMode,
    /// Spectrogram: lower bound of the dynamic range in dBFS. Anything
    /// softer than this maps to the darkest colour. Default -72 matches
    /// the old hardcoded value; commonly-useful range is -48 (loud-only
    /// view) to -96 (full noise floor). Upper bound is always 0 dBFS.
    #[serde(default = "default_spectrogram_db_floor")]
    pub spectrogram_db_floor: f32,
}

fn default_spectrogram_db_floor() -> f32 { -72.0 }

impl Default for PaneScopeSettings {
    fn default() -> Self {
        Self {
            waveform_channel: WaveformChannel::Y,
            low_pass: false,
            bloom: false,
            quality: klipp_core::ScopeQuality::Medium,
            scale: ScopeScale::TenBit,
            // Waveform
            colorize: true,
            extents: false,
            waveform_brightness: 0.5,
            graticule_brightness: 0.5,
            show_reference_levels: false,
            reference_low: 0.0,
            reference_high: 1023.0,
            // Vectorscope
            vectorscope_brightness: 0.5,
            vectorscope_graticule_brightness: 0.5,
            low_range: 0.30,
            high_range: 0.70,
            show_skin_tone_indicator: false,
            show_graticule: true,
            // Parade
            overlay_on_viewer: false,
            // Scope cursor
            show_scope_cursor: true,
            scope_cursor_sample_radius: 2,
            // Additional reference / display options
            show_iq_lines: false,
            waveform_paint_mode: WaveformPaintMode::Green,
            spectrogram_db_floor: default_spectrogram_db_floor(),
        }
    }
}

/// Compute tone map gain from waveform brightness setting (0.0–1.0).
pub fn compute_tone_map_gain(settings: &PaneScopeSettings) -> f32 {
    // Map 0.0–1.0 brightness to 0.3–3.0 gain range (log-ish)
    0.3 + settings.waveform_brightness * 2.7
}

/// Compute graticule alpha from brightness setting (0.0–1.0).
pub fn graticule_alpha(brightness: f32) -> f32 {
    // Always at least slightly visible, up to full brightness
    (0.05 + brightness * 0.95).clamp(0.05, 1.0)
}

/// Reference line position for a given value and panel height.
pub struct ReferenceLine {
    pub y: f32,
    pub value: f32,
}

/// Compute reference level line positions.
pub fn compute_reference_lines(settings: &PaneScopeSettings, height: f32) -> Vec<ReferenceLine> {
    if !settings.show_reference_levels {
        return Vec::new();
    }
    let draw_h = height - 14.0 - 16.0; // PAD_TOP, PAD_BOTTOM
    let val_to_y = |val: f32| -> f32 {
        14.0 + (1.0 - settings.scale.fraction(val)) * draw_h
    };
    vec![
        ReferenceLine { y: val_to_y(settings.reference_high), value: settings.reference_high },
        ReferenceLine { y: val_to_y(settings.reference_low), value: settings.reference_low },
    ]
}

/// Saturation ring for vectorscope.
pub struct SaturationRing {
    pub radius: f32,
    pub range_value: f32,
}

/// Compute vectorscope saturation ring radii.
pub fn compute_saturation_rings(settings: &PaneScopeSettings, diameter: f32) -> Vec<SaturationRing> {
    let max_r = diameter / 2.0 - 8.0;
    vec![
        SaturationRing { radius: settings.low_range * max_r, range_value: settings.low_range },
        SaturationRing { radius: settings.high_range * max_r, range_value: settings.high_range },
    ]
}

/// Configuration for a single pane in the floating scope window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneConfig {
    pub scope_type: ScopeType,
    pub settings: PaneScopeSettings,
}

impl PaneConfig {
    pub fn new(scope_type: ScopeType) -> Self {
        Self { scope_type, settings: PaneScopeSettings::default() }
    }

    /// Default pane configurations for a given layout.
    pub fn default_for_layout(layout: ScopeLayout) -> Vec<PaneConfig> {
        let defaults = [
            ScopeType::Parade,
            ScopeType::Waveform,
            ScopeType::Vectorscope,
            ScopeType::Histogram,
            ScopeType::Waveform,
            ScopeType::Parade,
            ScopeType::Histogram,
            ScopeType::Vectorscope,
            ScopeType::Waveform,
        ];
        defaults[..layout.pane_count()].iter().map(|&t| PaneConfig::new(t)).collect()
    }
}

/// Vectorscope scale style (for ··· menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum VectorscopeScaleStyle {
    #[default]
    SeventyFive,
    Hundred,
}

impl VectorscopeScaleStyle {
    pub fn label(&self) -> &'static str {
        match self { Self::SeventyFive => "75% colour bars", Self::Hundred => "100% colour bars" }
    }
}

/// The scope options, one struct so the main-dock scope-settings popover
/// and the floating scope window edit the same values; the publish
/// bridge reads it each frame and ships [`klipp_core::ScopeSettings`] to
/// the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScopeOptions {
    /// Draw each column's lowest and highest level as a line.
    pub waveform_envelope: bool,
    /// Waveform and parade trace brightness, 0..1; 0.5 is the density
    /// as drawn, 1 six times brighter, 0 six times dimmer.
    pub waveform_intensity: f32,
    /// When true, Wave and Parade render as a single white Y (luma)
    /// trace instead of three coloured R/G/B traces.
    pub waveform_luma_only: bool,
    /// Tint the vectorscope trace by hue; otherwise mono.
    pub vectorscope_colour: bool,
    /// Only pixels whose Y′ is at least this are plotted. Default 0.
    pub vectorscope_luma_lo: f32,
    /// Only pixels whose Y′ is at most this are plotted. Default 1.
    pub vectorscope_luma_hi: f32,
    /// Magnification of the vectorscope's chroma plane: 1, 2 or 4.
    pub vectorscope_zoom: f32,
    /// How the histogram lays out its channels.
    pub histogram_display: klipp_core::HistogramDisplayMode,
    /// Histogram height by the count or by its logarithm.
    pub histogram_levels: klipp_core::HistogramLevels,
    /// Brightness of the area under each histogram curve. Default 0.7.
    pub histogram_fill: f32,
}

impl Default for ScopeOptions {
    fn default() -> Self {
        Self {
            waveform_envelope: false,
            waveform_intensity: 0.5,
            waveform_luma_only: false,
            vectorscope_colour: true,
            vectorscope_luma_lo: 0.0,
            vectorscope_luma_hi: 1.0,
            vectorscope_zoom: 1.0,
            histogram_display: klipp_core::HistogramDisplayMode::Overlay,
            histogram_levels: klipp_core::HistogramLevels::Linear,
            histogram_fill: 0.7,
        }
    }
}

/// Persistent state for the floating scope window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScopeWindowState {
    pub open: bool,
    pub layout: ScopeLayout,
    pub panes: Vec<PaneConfig>,
    pub global_low_pass: bool,
    pub global_quality: klipp_core::ScopeQuality,
    /// The units the ··· menu last set every level pane to.
    pub waveform_scale: ScopeScale,
    /// Mark the legal range, codes 64 to 940, on every level pane.
    #[serde(default)]
    pub show_legal_range: bool,
    pub vectorscope_scale: VectorscopeScaleStyle,
    pub qualifier_focus: bool,
    /// The skin level the brief's look asks for, as the false-colour
    /// pane's green band. Set from the sequence each frame rather than
    /// read here, because this state is serialised with the window
    /// layout and a look belongs to the project, not to the window.
    #[serde(skip)]
    pub look_skin_l: Option<f32>,
}

impl Default for ScopeWindowState {
    fn default() -> Self {
        let layout = ScopeLayout::FourPane;
        Self {
            open: false,
            layout,
            panes: PaneConfig::default_for_layout(layout),
            global_low_pass: false,
            global_quality: klipp_core::ScopeQuality::Medium,
            waveform_scale: ScopeScale::default(),
            show_legal_range: false,
            vectorscope_scale: VectorscopeScaleStyle::default(),
            qualifier_focus: false,
            look_skin_l: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayheadFollow {
    /// No auto-scroll
    Off,
    /// Jump one page when playhead reaches edge
    #[default]
    Page,
    /// Continuously scroll to keep playhead visible
    Smooth,
}

impl PlayheadFollow {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Page => "Page",
            Self::Smooth => "Smooth",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Off => Self::Page,
            Self::Page => Self::Smooth,
            Self::Smooth => Self::Off,
        }
    }
}

/// What Cut and Copy put on the clipboard (#458, #845).
pub use crate::clip_clipboard::ClipboardContent;

/// A built-in effect as a clip's picture holds it, for Paste Attributes
/// (M4 step 12c-5): the effect, whether it's on, and its animations by key
/// (keys and modifier stacks).
#[derive(Debug, Clone)]
pub struct CopiedEffect {
    pub effect: klipp_effects::BuiltinEffect,
    pub enabled: bool,
    pub anims: std::collections::BTreeMap<klipp_graph_format::ParamKey, klipp_graph_format::Animation>,
}

impl CopiedEffect {
    /// `clip`'s built-in effects as its picture holds them, in list order.
    pub fn of(clip: &klipp_timeline::Clip) -> Vec<Self> {
        let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
        (0..)
            .map_while(|i| clip.effect_node_at(false, i))
            .filter_map(|id| {
                let n = lifted.picture.graph.nodes.iter().find(|n| n.id == id)?;
                // Its values in its map, as the renderer reads them.
                let mut held = n.clone();
                klipp_graph_lift::effect_params::into_memory(&mut held).ok()?;
                Some(Self { effect: klipp_graph_lift::effect_params::effect_of(&held).ok()?, enabled: n.enabled, anims: n.anim.clone() })
            })
            .collect()
    }
}

/// The placement and opacity lanes a paste carries: the height's own
/// (#1471) with the width's, so a pasted scale is the one copied.
const LANES: [&str; 6] = ["pos_x", "pos_y", "scale", "scale_y", "rotation", "opacity"];

/// `clip`'s placement and opacity lanes as its picture holds them.
fn placement_anims(clip: &klipp_timeline::Clip) -> std::collections::BTreeMap<&'static str, klipp_graph_format::Animation> {
    let Ok(lifted) = clip.picture_v2() else { return Default::default() };
    let p = &lifted.picture;
    LANES
        .iter()
        .filter_map(|key| {
            let anim = if *key == "opacity" { p.opacity_anim.clone() } else { p.placement_anim.get(&klipp_graph_format::ParamKey::from(*key)).cloned() };
            Some((*key, anim?))
        })
        .collect()
}

/// Snapshot of a clip's attributes for Paste Attributes.
#[derive(Debug, Clone)]
pub struct AttributesClipboard {
    /// The built-in effects as the clip's picture holds them, in list
    /// order, each with its flag, keys and modifier stacks (M4 step 12c-5).
    pub picture_effects: Vec<CopiedEffect>,
    /// OFX effects chain.
    pub effects: Vec<klipp_plugin::AppliedEffect>,
    /// Node-based color grade.
    pub node_graph: klipp_nodes::NodeGraph,
    /// The clip's picture held a grade correction's mix or mask channel
    /// (M3 step 17a), which `node_graph` doesn't: its grade isn't pasted.
    pub grade_mix_held: bool,
    /// Audio params.
    pub volume: f32,
    pub pan: f32,
    pub gain: f32,
    pub fade_in_secs: f32,
    pub fade_out_secs: f32,
    pub fade_in_curve: klipp_timeline::FadeCurve,
    pub fade_out_curve: klipp_timeline::FadeCurve,
    pub volume_keyframes: Vec<klipp_core::Keyframe>,
    pub pan_keyframes: Vec<klipp_core::Keyframe>,
    pub gain_keyframes: Vec<klipp_core::Keyframe>,
    /// Audio effects.
    pub audio_effects: Vec<klipp_core::AudioEffectInstance>,
    /// Audio-effect keyframes (mirror of effect_keyframes for the audio chain).
    pub audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// The placement and opacity lanes as the clip's picture holds them,
    /// each its keys and modifier stack, by key (M4 step 12c-5).
    pub placement_anims: std::collections::BTreeMap<&'static str, klipp_graph_format::Animation>,
    /// Param modifiers (behaviors): the clip's own that aren't picture
    /// animation (its audio's) are pasted from here.
    pub param_modifiers: std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    /// Speed.
    pub speed: f64,
    /// Speed keyframes.
    pub speed_keyframes: Vec<klipp_core::Keyframe>,
    /// The grade's node colour labels, by path inside it (#1866): set
    /// by the app, which has the sequence they are kept on.
    pub grade_labels: Vec<klipp_timeline::GradeNodeLabel>,
}

/// Which parts of the attributes clipboard a paste writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PasteChoice {
    pub effects: bool,
    pub color_grade: bool,
    pub audio: bool,
    pub transform: bool,
    pub speed: bool,
}

impl AttributesClipboard {
    /// What Copy Attributes takes from `clip`.
    pub fn of(clip: &klipp_timeline::Clip) -> Self {
        Self {
            picture_effects: CopiedEffect::of(clip),
            effects: clip.ofx_effects().to_vec(),
            node_graph: clip.grade().clone(),
            grade_mix_held: clip.holds_grade_mix(),
            volume: clip.volume,
            pan: clip.pan,
            gain: clip.gain,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            volume_keyframes: clip.volume_keyframes.clone(),
            pan_keyframes: clip.pan_keyframes.clone(),
            gain_keyframes: clip.gain_keyframes.clone(),
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
            placement_anims: placement_anims(clip),
            param_modifiers: clip.param_modifiers().clone(),
            speed: clip.speed,
            speed_keyframes: clip.speed_keyframes.clone(),
            grade_labels: Vec::new(),
        }
    }

    /// What Copy Attributes takes from `clip` of `seq`, its grade's node
    /// colour labels with it (#1866).
    pub fn of_in(seq: &klipp_timeline::Sequence, clip: &klipp_timeline::Clip) -> Self {
        Self { grade_labels: klipp_timeline::grade_node_labels(&seq.node_labels, clip.id), ..Self::of(clip) }
    }

    /// Write the chosen parts onto `clip`. A pasted grade gets fresh node
    /// ids, so the clips it lands on do not share ids — the executor's
    /// per-node caches are keyed by them. The clip keeps its own Input
    /// Transform, and takes none from the source: the input colour space
    /// is the footage's, not part of the grade. A clip without one (a
    /// Rec.709 shot, audio, a title, the timeline grade's stand-in) gets
    /// the grade alone.
    /// The copied effects in place of `clip`'s, through its picture (M4
    /// step 12c-5): its built-in list replaced in one edit, each pasted
    /// effect with its flag, then its keys and modifier stacks set by path
    /// on its new node; its plug-ins replaced the same way. A picture that
    /// refuses either is left as it was, and the refusal logged.
    fn paste_effects(&self, clip: &mut klipp_timeline::Clip) {
        use klipp_graph_format::path::{NodePath, Root};
        use klipp_timeline::{EffectEdit, OfxEdit};
        let listed = |clip: &klipp_timeline::Clip, ofx: bool| (0..).take_while(|i| clip.effect_node_at(ofx, *i).is_some()).count();
        let mut edits: Vec<EffectEdit> = (0..listed(clip, false)).map(|_| EffectEdit::Remove(0)).collect();
        edits.extend(self.picture_effects.iter().map(|e| EffectEdit::Add(&e.effect)));
        edits.extend(self.picture_effects.iter().enumerate().filter(|(_, e)| !e.enabled).map(|(i, _)| EffectEdit::SetEnabled(i, false)));
        match clip.apply_effect_edits(&edits) {
            Some(Ok(_)) => {
                let mut ops = Vec::new();
                for (i, e) in self.picture_effects.iter().enumerate() {
                    let Some(node) = clip.effect_node_at(false, i) else { continue };
                    for (key, anim) in &e.anims {
                        ops.push(klipp_graph_lift::ops::GraphOp::SetAnim { path: NodePath::new(Root::Clip { clip: clip.id.0 }, [node]), key: key.clone(), anim: Some(anim.clone()) });
                    }
                }
                if let Err(why) = clip.apply_picture_ops(&ops) {
                    tracing::warn!("paste: clip {:?}: the effects' keys: {why}", clip.id);
                }
            }
            Some(Err(why)) => tracing::warn!("paste: clip {:?}: the effects: {why}", clip.id),
            None => tracing::warn!("paste: clip {:?}: the effects can't be pasted through its picture", clip.id),
        }
        let mut plugins: Vec<OfxEdit> = (0..listed(clip, true)).map(|_| OfxEdit::Remove(0)).collect();
        plugins.extend(self.effects.iter().map(OfxEdit::Add));
        if let Err(why) = clip.apply_ofx_edits(&plugins) {
            tracing::warn!("paste: clip {:?}: the plug-ins: {why}", clip.id);
        }
    }

    /// The copied placement and opacity lanes' keys in place of `clip`'s,
    /// by path on its placement (M4 step 12c-5); a lane the source didn't
    /// key is cleared.
    fn paste_lanes(&self, clip: &mut klipp_timeline::Clip) {
        let Some(at) = clip.placement_path() else { return };
        let ops: Vec<_> = LANES
            .iter()
            // The keys alone: a lane's modifiers aren't set with its keys,
            // and travel with the clip's own (`paste_clip_modifiers`).
            .map(|key| {
                let anim = self.placement_anims.get(key).filter(|a| !a.keys.is_empty()).map(|a| klipp_graph_format::Animation { keys: a.keys.clone(), modifiers: None });
                klipp_graph_lift::ops::GraphOp::SetAnim { path: at.clone(), key: klipp_graph_format::ParamKey::from(*key), anim }
            })
            .collect();
        if let Err(why) = clip.apply_picture_ops(&ops) {
            tracing::warn!("paste: clip {:?}: the placement: {why}", clip.id);
        }
    }

    /// Replace `clip`'s own modifiers (opacity, position and the rest)
    /// with these, leaving its effects' alone: those travel with the
    /// effects (`restore_effects_state`).
    fn paste_clip_modifiers(&self, clip: &mut klipp_timeline::Clip) {
        use klipp_core::ParamId as P;
        let own = |id: &P| !matches!(id, P::Effect { .. } | P::Node { .. });
        clip.edit_param_modifiers(|v| v.retain(|id, _| !own(id)));
        clip.edit_param_modifiers(|v| v.extend(self.param_modifiers.iter().filter(|(id, _)| own(id)).map(|(id, m)| (id.clone(), m.clone()))));
    }

    pub fn paste_onto(&self, clip: &mut klipp_timeline::Clip, what: PasteChoice) {
        self.paste_onto_mapped(clip, what);
    }

    /// [`Self::paste_onto`], and, when the grade was pasted, the id each
    /// of its nodes took on `clip` (#1866).
    pub fn paste_onto_mapped(&self, clip: &mut klipp_timeline::Clip, what: PasteChoice) -> Option<std::collections::HashMap<klipp_nodes::NodeId, klipp_nodes::NodeId>> {
        let mut map = None;
        if what.effects {
            // The effects with their flags, keyframes and modifiers, as new
            // entries on this clip. An effect's modifiers go with it, as its
            // keyframes do: a pasted wiggle was lost (#992).
            self.paste_effects(clip);
        }
        if what.color_grade {
            let (grade, ids) = clip.grade().with_grade_of_mapped(&self.node_graph);
            clip.set_grade(grade);
            map = Some(ids);
        }
        if what.audio {
            clip.volume = self.volume;
            clip.pan = self.pan;
            clip.gain = self.gain;
            clip.fade_in_secs = self.fade_in_secs;
            clip.fade_out_secs = self.fade_out_secs;
            clip.fade_in_curve = self.fade_in_curve;
            clip.fade_out_curve = self.fade_out_curve;
            clip.volume_keyframes = self.volume_keyframes.clone();
            clip.pan_keyframes = self.pan_keyframes.clone();
            clip.gain_keyframes = self.gain_keyframes.clone();
            clip.audio_effects = self.audio_effects.clone();
            clip.audio_effect_keyframes = self.audio_effect_keyframes.clone();
        }
        if what.transform {
            self.paste_lanes(clip);
            self.paste_clip_modifiers(clip);
        }
        if what.speed {
            // The ramp as well as the rate: copied with the clip, it
            // was never written, so a pasted ramp came out a constant.
            clip.speed = self.speed;
            clip.speed_keyframes = self.speed_keyframes.clone();
        }
        map
    }
}

impl AppState {
    /// `kind`'s analysis of `clip`'s media as `ensure_analyses` found it
    /// (M4 step 9): the file's, whichever clip of it asks.
    pub fn analysis_of(&self, clip: &klipp_timeline::Clip, kind: klipp_analysis::assets::AssetKind) -> Option<&std::sync::Arc<serde_json::Value>> {
        let item = self.media_pool.get(&clip.media_id()?)?;
        self.analyses.get(&klipp_analysis::assets::asset_key(kind, item)?)
    }

    /// Whether `ensure_analyses` looks now, at `now` with the sequence at
    /// `revision`; when it does, it's noted as having looked. Every
    /// [`ANALYSIS_EVERY`] at most, at once after a completed analysis
    /// clears `analysis_checked`, and after any edit of the sequence
    /// within [`ANALYSIS_EDIT_GAP`]: a slider's drag doesn't read every
    /// clip's picture each tick. An edit not looked at yet says how long
    /// until it is, for the caller to ask for a frame then: an idle
    /// window draws none of its own.
    pub fn analysis_due(&mut self, revision: klipp_timeline::SequenceRevision, now: std::time::Instant) -> AnalysisDue {
        let since = self.analysis_checked.map(|t| now.saturating_duration_since(t));
        let wait = match since {
            None => std::time::Duration::ZERO,
            Some(since) if self.analysis_revision != Some(revision) => ANALYSIS_EDIT_GAP.saturating_sub(since),
            Some(since) if since >= ANALYSIS_EVERY => std::time::Duration::ZERO,
            Some(_) => return AnalysisDue::NotYet,
        };
        if !wait.is_zero() {
            return AnalysisDue::After(wait);
        }
        self.analysis_checked = Some(now);
        self.analysis_revision = Some(revision);
        AnalysisDue::Now
    }

    /// Of `uses` (each key a clip of the sequence reads, its kind, a clip
    /// of it, and whether its analysis runs now), the ones to start at
    /// `now`: a key `missing` from the store that no run of it is making,
    /// and that didn't end without its asset within [`ANALYSIS_RETRY`].
    /// A started key whose run has ended is let go once its asset is
    /// there, or once no clip reads it.
    pub fn analyses_to_start(&mut self, uses: &[AnalysisUse], missing: &[klipp_analysis::assets::AssetKey], now: std::time::Instant) -> Vec<AnalysisUse> {
        for (key, started) in self.analysis_started.iter_mut() {
            let running = uses.iter().any(|u| u.key == *key && u.running);
            if !running && started.ended.is_none() {
                started.ended = Some(now);
            }
        }
        self.analysis_started.retain(|key, started| started.ended.is_none() || (missing.contains(key) && uses.iter().any(|u| u.key == *key)));
        let started = &self.analysis_started;
        self.analysis_threads.retain(|key, _| started.contains_key(key));
        let mut start = Vec::new();
        for u in uses.iter().filter(|u| !u.running && missing.contains(&u.key)) {
            let waiting = self.analysis_started.get(&u.key).is_some_and(|s| s.ended.is_none_or(|ended| now.duration_since(ended) < ANALYSIS_RETRY));
            if waiting || start.iter().any(|s: &AnalysisUse| s.key == u.key) {
                continue;
            }
            self.analysis_started.insert(u.key.clone(), StartedAnalysis { ended: None });
            start.push(u.clone());
        }
        start
    }

    /// The analyses `used` (each asset key the sequence's clips use, and
    /// whether to read it now) kept in step with `store` (M4 step 9): one
    /// loaded here is read again when its stamp moves and let go when it's
    /// gone or no clip uses it; one to read that isn't held is read when
    /// ready. The keys to read that `store` has no ready asset for come
    /// back, for an analysis to make. An analysis put in `analyses` by
    /// anything else (it has no stamp) is left as it is.
    pub fn keep_analyses(&mut self, store: &dyn klipp_analysis::assets::AssetStore, used: &[(klipp_analysis::assets::AssetKey, bool)]) -> Vec<klipp_analysis::assets::AssetKey> {
        use klipp_analysis::assets::{lookup, Found};
        let stale: Vec<_> = self.analysis_stamps.iter().filter(|(k, seen)| !used.iter().any(|(u, _)| u == *k) || store.stamp(k) != Some(**seen)).map(|(k, _)| k.clone()).collect();
        for key in stale {
            self.analyses.remove(&key);
            self.analysis_stamps.remove(&key);
        }
        let mut missing = Vec::new();
        for (key, read) in used {
            if !read || self.analyses.contains_key(key) {
                continue;
            }
            let stamp = store.stamp(key);
            match lookup(store, key) {
                Found::Ready(raw) => {
                    if let Some(stamp) = stamp {
                        self.analysis_stamps.insert(key.clone(), stamp);
                    }
                    self.analyses.insert(key.clone(), std::sync::Arc::new(raw));
                }
                _ => missing.push(key.clone()),
            }
        }
        missing
    }

    /// The explicit dock works in either workspace; Colour keeps its
    /// existing grade strip when the new dock is closed.
    pub fn graph_is_visible(&self) -> bool {
        self.graph_dock_open || (self.show_color_panel && self.show_node_strip)
    }

    /// The scope dock under the viewer exists only in the Color
    /// workspace, where the timeline is hidden. Elsewhere it would take
    /// the timeline's rows, so the dock flag is ignored there.
    pub fn scopes_docked(&self) -> bool {
        self.show_scopes && self.show_color_panel
    }

    /// Whether any scope is on screen: the Color dock, the overlay in the
    /// viewer, or the floating window. The engine draws scopes only then.
    pub fn scopes_shown(&self) -> bool {
        self.scopes_docked()
            || (self.show_scopes && self.scope_overlay_mode)
            || self.scope_window.open
    }

    /// Whether the viewers sit side by side in the current workspace.
    pub fn viewers_side_by_side(&self) -> bool {
        match self.active_workspace {
            Workspace::Edit => self.dual_viewers,
            Workspace::Color => self.color_dual_viewers,
            Workspace::Export => self.export_dual_viewers,
        }
    }

    /// Flip the viewer layout of the current workspace.
    pub fn toggle_viewer_layout(&mut self) {
        let flag = match self.active_workspace {
            Workspace::Edit => &mut self.dual_viewers,
            Workspace::Color => &mut self.color_dual_viewers,
            Workspace::Export => &mut self.export_dual_viewers,
        };
        *flag = !*flag;
    }

    /// Only Edit works with a source clip; a single viewer in Color or
    /// Export is the program viewer.
    pub fn single_viewer_is_program(&self) -> bool {
        !self.viewers_side_by_side() && self.active_workspace != Workspace::Edit
    }

    /// The scopes toggle (V, the toolbar icon, the View menu): the dock in
    /// Color, the viewer overlay when that mode is chosen, otherwise the
    /// floating window, which never steals space from the timeline.
    pub fn toggle_scopes(&mut self) {
        if self.show_color_panel || self.scope_overlay_mode {
            self.show_scopes = !self.show_scopes;
        } else {
            self.scope_window.open = !self.scope_window.open;
        }
    }

    /// Returns true when any modal dialog, window, or popup menu is open.
    /// Used to block background interactions like skimming, timeline hover, etc.
    pub fn has_modal_open(&self) -> bool {
        self.modal_or_popup_open || self.dialog_open()
    }

    /// Whether a dialog that keeps the keys is open: its own fields and
    /// buttons take them, and no shortcut reaches the app behind it (#2551).
    pub fn dialog_open(&self) -> bool {
        self.show_preferences
            || self.show_unsaved_dialog
            || self.show_export_dialog
            || self.show_new_project_dialog
            || self.show_relink_dialog
            || self.show_about_dialog
            || self.update_notice_open
            || self.show_autosave_recovery
            || self.show_render_queue
            || self.show_ai_models_dialog
            || self.show_audio_plugins_dialog
            || self.show_system_probe
            || self.show_project_doctor
            || self.show_cache_manager
            || self.show_command_palette
            || self.quick_look_path.is_some()
            || self.auto_align.is_some()
            // The confirmations: Delete and the like acted behind them.
            || self.confirm_delete_sequence.is_some()
            || self.confirm_remove_track.is_some()
    }
}

#[derive(Debug, Clone)]
pub enum UnsavedAction {
    None,
    NewProject,
    OpenNewProjectDialog,
    OpenProject(PathBuf),
    /// A timeline from another editor, opened as a new project (#2083).
    OpenFromAnotherEditor(PathBuf),
    CloseProject,
    Quit,
}

impl Default for AppState {
    fn default() -> Self {
        // Build the shared registry once and share it between the
        // panel view + worker handle. See `build_shared_registry`
        // for what gets registered (stub + every backend gated by
        // its feature flag).
        let shared_registry = build_shared_registry();
        Self {
            playback: PlaybackState::Idle,
            playhead: Timecode { value: 0, rate: 24 },
            fps: Fps::film_24(),
            playback_speed: 1.0,
            jkl_speed_step: 2.0,
            track_search_radius: 32,
            multicam_default_sync: klipp_core::MulticamSync::TimecodeThenAudio,
            loop_playback_enabled: false,
            active_workspace: Workspace::default(),
            custom_workspaces: Vec::new(),
            show_save_workspace_dialog: false,
            save_workspace_name: String::new(),
            show_save_hyperzoom_preset_dialog: false,
            save_hyperzoom_preset_name: String::new(),
            save_hyperzoom_preset_error: None,
            save_hyperzoom_preset_source_clip: None,
            show_workspace_manager: false,
            workspace_manager_selected: None,
            sequences: vec![Sequence::new_default()],
            active_sequence_idx: 0,
            project_epoch: 0,
            compound_clip_stack: Vec::new(),
            media_pool: MediaPool::new(),
            whoosh_media_ids: std::collections::HashMap::new(),
            hyperzoom_preset_registry: crate::hyperzoom_presets::HyperzoomPresetRegistry::new(),
            title_assets: std::collections::HashMap::new(),
            project_path: None,
            created_at: None,
            dirty: false,
            command_histories: std::collections::HashMap::new(),
            project_history: CommandHistory::new(),
            history_clock: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            history_ops_since_save: 0,
            tracks_undo_gesture: None,
            control_shared: std::sync::Arc::new(klipp_core::ControlShared::default()),
            control_others: Default::default(),
            control_others_for: None,
            history_max_size: 1000,
            viewer_inspector_split: 0.6,
            upper_timeline_split: 0.65,
            timeline_zoom: 50.0,
            timeline_scroll_x: 0.0,
            timeline_scroll_y: 0.0,
            timeline_track_scale: 1.0,
            last_timeline_tracks_height: 600.0,
            timeline_resizing_track: None,
            timeline_track_pre_collapse: std::collections::HashMap::new(),
            playhead_follow: PlayheadFollow::default(),
            playhead_follow_chosen: false,
            track_header_width: 180.0,
            source_view: ViewerView::default(),
            program_view: ViewerView::default(),
            last_timeline_content_width: 800.0,
            show_overview_bar: true,
            selected_clip: None,
            selected_clip_loc_cache: std::cell::Cell::new(None),
            selected_transition: None,
            selected_clips: Vec::new(),
            selected_edits: Vec::new(),
            trim_entry: None,
            dynamic_trim: None,
            hovered_clip: None,
            show_unsaved_dialog: false,
            unsaved_dialog_action: UnsavedAction::None,
            pending_media_path: None,
            pending_undo_imports: std::collections::HashSet::new(),
            pending_skip_analysis: std::collections::HashSet::new(),
            pending_media_drop: std::collections::HashMap::new(),
            retired_media_ids: std::collections::HashSet::new(),
            thumbnail_cache: ThumbnailCache::new(),
            thumbnail_textures: std::collections::HashMap::new(),
            filmstrip_textures: std::collections::HashMap::new(),
            graded_thumbs: Default::default(),
            show_export_dialog: false,
            jog_wheel_state: crate::widgets::jog_wheel::JogWheelState::new(),
            show_preset_manager: false,
            preset_manager_selected: None,
            show_ai_models_dialog: false,
            show_audio_plugins_dialog: false,
            audio_plugins_dialog: crate::panels::audio_plugins::AudioPluginsDialogState::default(),
            show_system_probe: false,
            show_project_doctor: false,
            show_cache_manager: false,
            cache_categories: Vec::new(),
            show_clip_properties: false,
            clip_properties_media_id: None,
            clip_properties_tab: ClipPropertiesTab::default(),
            active_tool: Tool::Select,
            viewer_mode: ViewerMode::Program,
            snap: crate::layout_state::SnapConfig::default(),
            ripple_enabled: true,
            linked_selection: true,
            append_brings_audio: true,
            default_transition_frames: 24,
            shift_step_frames: 0,
            play_to_out: None,
            timeline_prefs: Default::default(),
            viewer_prefs: Default::default(),
            snap_threshold_px: 10.0,
            grid_snap_interval: klipp_core::GridInterval::Off,
            zoom_step: 1.5,
            default_sequence_resolution_idx: 0,
            toast_duration_secs: 2.2,
            modal_backdrop_alpha: 120,
            scope_quality: Default::default(),
            scope_update_interval_ms: 32,
            edit_mode: klipp_timeline::magnetic::EditMode::default(),
            magnetic_timeline_enabled: true,
            multicam_show_angles: true,
            fullscreen_viewer: false,
            fullscreen_changed: false,
            clipboard: None,
            system_clipboard_text: None,
            attributes_clipboard: None,
            title_style_clipboard: None,
            title_style_pick: None,
            show_paste_attributes_dialog: false,
            show_speed_dialog: false,
            edit_helper: None,
            speed_dialog_value: 100.0,
            speed_dialog_reverse: false,
            speed_dialog_keep_pitch: false,
            selected_gap: None,
            speed_dialog_ripple: true,
            source_media_id: None,
            source_playhead: Timecode::zero(),
            source_seek_dispatch_at: None,
            source_video_texture: None,
            source_playback: PlaybackState::Idle,
            source_live_texture: None,
            source_live_texture_gen: 0,
            selected_media_ids: std::collections::HashSet::new(),
            last_clicked_media_id: None,
            use_proxies: true,
            is_scrubbing: false,
            pre_scrub_use_proxies: true,
            pre_scrub_was_playing: false,
            showing_thumbnail_preview: false,
            awaiting_seek_frame: false,
            render_cache_ranges: Vec::new(),
            frame_declines: Vec::new(),
            render_status: Vec::new(),
            render_range: None,
            show_relink_dialog: false,
            offline_media_ids: Vec::new(),
            show_about_dialog: false,
            update_notice_open: false,
            editing_timecode: false,
            timecode_input: String::new(),
            fullscreen_overlay_visible: false,
            fullscreen_overlay_timer: 0.0,
            dragging_media: None,
            dragging_media_group: Vec::new(),
            skimming_enabled: true,
            file_browser_video_skim_enabled: true,
            file_browser_audio_skim_enabled: true,
            toast: crate::widgets::toast::ToastState::default(),
            drag_drop: crate::drag_drop::DragDropState::default(),
            deferred_texture_drops: [Vec::new(), Vec::new(), Vec::new()],
            deferred_texture_drops_slot: 0,
            browser_skim_texture: None,
            browser_skim_media_id: None,
            skim_transcoding: std::collections::HashSet::new(),
            skim_in_flight_paths: std::collections::HashMap::new(),
            skim_transcode_status: None,
            skim_transcode_done_at: None,
            browser_skim_pending: None,
            skim_decode_in_flight: false,
            skim_decode_started_at: None,
            skim_queued_request: None,
            skim_last_requested: None,
            browser_skim_active_this_frame: false,
            program_skim_active: false,
            program_skim_revert_deadline: None,
            pending_skim_transcodes: Vec::new(),
            audio_skimming_enabled: true,
            skim_speed_px_per_sec: 0.0,
            skim_last_update: std::time::Instant::now(),
            skim_last_x: 0.0,
            skim_audio_session: None,
            audition_active: false,
            system_audio_muted: false,
            system_audio_last_check: std::time::Instant::now(),
            system_audio_last_reminder: std::time::Instant::now(),
            mute_reminder_interval_secs: 15,
            files_panel_focused: false,
            quick_look_path: None,
            quick_look_texture: None,
            quick_look_info: None,
            quick_look_audio: None,
            show_recent_files: true,
            filmstrip_hover_enabled: true,
            filmstrip_quality: Default::default(),
            filmstrip_persist: false,
            file_browser_auto_refresh: true,
            recent_files_snapshot: Vec::new(),
            skim_last_audio_dispatch: std::time::Instant::now(),
            deflicker_in_progress: std::collections::HashSet::new(),
            deflicker_progress: std::collections::HashMap::new(),
            stabilization_in_progress: std::collections::HashSet::new(),
            stabilization_progress: std::collections::HashMap::new(),
            analyses: Default::default(),
            analysis_stamps: Default::default(),
            analysis_checked: None,
            analysis_revision: None,
            analysis_started: Default::default(),
            analysis_threads: Default::default(),
            analysis_unavailable: Default::default(),
            scene_detect_in_progress: std::collections::HashSet::new(),
            scene_detect_progress: std::collections::HashMap::new(),
            scene_split_pending: std::collections::HashSet::new(),
            scene_detect_cancel: std::collections::HashMap::new(),
            spectrum_ring: std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            spectrum_bins: vec![-96.0f32; 256],
            spectrum_sample_rate: 48000,
            tracking_mode: false,
            active_track_point: None,
            tracking_in_progress: std::collections::HashSet::new(),
            tracking_progress: std::collections::HashMap::new(),
            surface_track_cancel: std::collections::HashMap::new(),
            surface_snaps_pending: 0,
            pending_mask_track_link: std::collections::HashMap::new(),
            browser_range_selection: None,
            browser_drag_start: None,
            range_drag: None,
            range_snap_line_x: None,
            ruler_press_origin: None,
            show_perf_overlay: false,
            perf_recording_active: false,
            pending_perf_recording_toggle: false,
            perf_last_session_path: None,
            perf_latest_sample: None,
            perf_averages: klipp_engine::perf::PerfAverages::default(),
            perf_drops_last_sec: 0,
            perf_fps: 0.0,
            perf_video_fps: 0.0,
            perf_readback_ms: 0.0,
            perf_last_frame_count: 0,
            perf_last_readback_count: 0,
            perf_last_readback_us: 0,
            perf_frame_count: 0,
            perf_dropped_frames: 0,
            perf_memory_mb: 0.0,
            perf_cpu_percent: 0.0,
            perf_hw_decode: None,
            perf_last_update: std::time::Instant::now(),
            perf_frame_times: Vec::with_capacity(120),
            perf_prev_cpu_ticks: 0,
            perf_prev_wall_time: std::time::Instant::now(),
            global_lut_path: None,
            global_lut_opacity: 1.0,
            global_lut_enabled: true,
            bake_lut_size: 33,
            show_color_panel: false,
            grade_timeline: false,
            grade_group_stage: None,
            grade_track: None,
            title_field_before: None,
            title_colour_edit: None,
            color_tools_position: ColorToolsPosition::Right,
            selected_node_id: None,
            selected_node_ids: std::collections::HashSet::new(),
            active_panel: None,
            focus_search_request: None,
            timeline_search_next: false,
            timeline_search_prev: false,
            grade_node_clipboard: None,
            picture_node_clipboard: Vec::new(),
            graph_descent_path: Vec::new(),
            show_node_strip: false,
            use_node_graph_v2: true,
            color_tab: ColorTab::default(),
            color_tab_bottom: ColorTab::default(),
            color_grade_drag_start: None,
            color_grade_clipboard: None,
            curves_single_mode: false,
            curve_histograms: None,
            show_preset_menu: false,
            preset_save_name: String::new(),
            show_preset_save: false,
            grade_library: crate::grade_library::dir().map(|d| crate::grade_library::load_in(&d)).unwrap_or_default(),
            huesat_single_mode: false,
            huesat_active_curve: 0,
            show_scopes: false,
            dual_viewers: true,
            color_dual_viewers: false,
            export_dual_viewers: false,
            scope_overlay_mode: false,
            scope_state: Default::default(),
            show_keyframe_editor: false,
            scope_mode: ScopeMode::Waveform,
            scope_scale: ScopeScale::TenBit,
            scope_legal_range: false,
            scope_show_r: true,
            scope_show_g: true,
            scope_show_b: true,
            scope_low_pass: true,
            scope_options: ScopeOptions::default(),
            // Off: at a large scope's size a glow washes the trace out.
            scope_bloom: false,
            scope_settings_open: false,
            scope_window: ScopeWindowState::default(),
            // The dock starts as the single large scope it has always
            // been. Multi-pane is one click away in the scope bar, and
            // defaulting to it would shrink the instrument for everyone
            // working in a small window — where four panes are worse
            // than one good one.
            scope_dock: ScopeWindowState {
                layout: ScopeLayout::OnePane,
                panes: PaneConfig::default_for_layout(ScopeLayout::OnePane),
                ..ScopeWindowState::default()
            },
            viewer_image_rect: None,
            viewer_panel_rect: egui::Rect::NOTHING,
            viewer_pointer_pos: None,
            scope_cursor_locked: false,
            scope_cursor_locked_pos: None,
            scope_cursor_locked_data: None,
            scope_cursor_sample_radius: 2,
            scope_cursor_show: true,
            hdr_monitor: klipp_core::HdrMonitor::Off,
            hdr_reference_output: false,
            hdr_nits_under_pointer: None,
            eyedropper_mode: EyedropperMode::None,
            qualifier_stroke_pick: None,
            qualifier_stroke: None,
            library_recent: Default::default(),
            wb_pick_clip: None,
            qualifier_pick: None,
            hue_curve_pick: None,
            hue_curve_awaiting: None,
            matte_preview_active: false,
            solo_node: None,
            viewed_node_name: None,
            before_after_active: false,
            stabilization_compare_active: false,
            resumable_path_drag: None,
            curve_editor_track: None,
            curve_editor_mask_track: None,
            pending_corrector: None,
            root_grade_start: None,
            selected_node_addr: None,
            split_compare_node: None,
            graph_dock_open: false,
            graph_dock_root: None,
            graph_dock_path: Vec::new(),
            split_position: 0.5,
            grade_stills: Vec::new(),
            active_grade_still: None,
            grade_still_compare: false,
            wipe_clip: None,
            color_match_reference: None,
            color_match_reference_clip: None,
            node_thumbs: std::collections::HashMap::new(),
            node_thumbs_gen: 0,
            node_thumbs_unavailable: Default::default(),
            node_thumbnail_state: Default::default(),
            looks: Vec::new(),
            viewer_overlays: ViewerOverlays::default(),
            frame_count: 0,
            false_color_lut: crate::prefs::FalseColorPalette::broadcast().build_lut(),
            pending_false_color_palette: None,
            available_plugins: Vec::new(),
            ladspa_plugins: Vec::new(),
            frei0r_plugins: Vec::new(),
            lv2_plugins: Vec::new(),
            clap_plugins: Vec::new(),
            disabled_audio_plugins: std::collections::HashSet::new(),
            effect_registry: crate::browser::effect_registry::EffectRegistry::new(),
            effect_source_filter: crate::browser::effects_tab::SourceFilter::default(),
            effect_filter_gpu: false,
            effect_filter_hide_unsupported: false,
            preset_manager: crate::effect_presets::PresetManager::new(),
            title_templates: crate::title_templates::TitleTemplateStore::load_default(),
            composition_templates: crate::parameterized_templates::CompositionTemplateStore::load_default(),
            title_template_dialog: None,
            credits_dialog: None,
            normalize_dialog: None,
            visual_search: None,
            music_fit_dialog: None,
            effect_thumbnails: crate::browser::effect_thumbnails::ThumbnailRenderer::default(),
            render_queue: {
                // An export runs an analysis its clips need and haven't
                // got the way the viewer's analyses do (M4 step 9).
                let mut q = klipp_export::RenderQueue::new();
                q.set_analyser(std::sync::Arc::new(klipp_engine::analyse_media));
                q
            },
            show_render_queue: false,
            filmstrip_generating: false,
            filmstrip_generate_start: std::time::Instant::now(),
            filmstrip_toast_dismissed: false,
            filmstrip_in_progress: std::collections::HashSet::new(),
            filmstrip_queue: Vec::new(),
            filmstrip_total_queued: 0,
            filmstrip_completed: 0,
            filmstrip_progress: std::collections::HashMap::new(),
            playhead_anchor: Timecode::zero(),
            playhead_anchor_wall: std::time::Instant::now(),
            playhead_speed: 1.0,
            playhead_smooth_secs: 0.0,
            collapsed_sections: std::collections::HashSet::new(),
            remember_inspector_layout: true,
            audio_backend_ok: false,
            audio_sample_rate: 48000,
            is_recording: false,
            recording_track_id: None,
            recording_path: None,
            recording_stop_tx: None,
            recording_audio_input: None,
            recording_punch: None,
            pending_take: None,
            input_meter: None,
            input_meter_failed: false,
            input_level: 0.0,
            track_peak_levels: Vec::new(),
            master_peak_left: 0.0,
            master_peak_right: 0.0,
            master_volume: 1.0,
            master_muted: false,
            lufs_momentary: f32::NEG_INFINITY,
            lufs_short_term: f32::NEG_INFINITY,
            lufs_integrated: f32::NEG_INFINITY,
            lufs_true_peak_dbfs: f32::NEG_INFINITY,
            delivery_loudness: DeliveryLoudness::default(),
            lufs_lra: 0.0,
            lufs_short_term_history: {
                let mut v = std::collections::VecDeque::new();
                v.reserve(600);
                v
            },
            spectrum_history: {
                let mut v = std::collections::VecDeque::new();
                v.reserve(300);
                v
            },
            lufs_momentary_smooth: f32::NEG_INFINITY,
            lufs_short_term_smooth: f32::NEG_INFINITY,
            clip_indicator: false,
            mixer_editing_name: None,
            lufs_target: -23.0,
            peak_hold_time_secs: 3.0,
            confirm_remove_track: None,
            closed_sequence_tabs: Default::default(),
            confirm_delete_sequence: None,
            show_markers_panel: false,
            auto_align: None,
            edit_marker_request: None,
            show_subtitle_editor: false,
            show_transcript_panel: false,
            transcript_panel_hovered: false,
            pending_doc_ops: Vec::new(),
            pending_refits: Vec::new(),
            matched_follow_sync: false,
            role_sweep_key: None,
            show_project_notes: false,
            show_edit_plan: false,
            show_colour_check: false,
            show_delivery_check: false,
            show_edit_list: false,
            show_sound_over_time: false,
            show_narrate: false,
            edit_plan_note: None,
            tp_oversample: true,
            project_notes: String::new(),
            last_import_report: None,
            show_import_report: false,
            subtitle_editor: SubtitleEditorState::default(),
            consolidate_progress: None,
            consolidate_dest_path: String::new(),
            consolidate_include_proxies: false,
            consolidate_include_skims: false,
            consolidate_create_zip: false,
            consolidate_mode: 0,
            consolidate_handle_secs: 1.0,
            consolidate_summary: None,
            consolidate_delete_folder_after_zip: false,
            show_browser_panel: true,
            reveal_in_browser: None,
            show_inspector_panel: true,
            modal_or_popup_open: false,
            show_preferences: false,
            ui_scale_request: None,
            window_capture: None,
            multicam_mode: false,
            multicam_opened: None,
            multicam_at_playhead: false,
            ui_scale_percent: 0,
            auto_create_tracks: true,
            project_auto_filmstrips: None,
            project_auto_skim: None,
            project_working_colorspace: klipp_core::WorkingColorspace::default(),
            project_grade_encoding: klipp_core::GradeEncoding::default(),
            project_default_odt: klipp_core::OdtChoice::default(),
            project_default_input_format: None,
            project_default_input_lut: None,
            project_viewing_lut: None,
            project_scopes_after_viewing_lut: false,
            show_colour_management: false,
            show_media_panel: false,
            show_remove_media_confirm: None,
            show_delete_unused_confirm: None,
            show_ai_model_uninstall_confirm: None,
            show_ai_models_bulk_uninstall_confirm: false,
            show_ai_models_stale_uninstall_confirm: false,
            custom_model_add: crate::ai_tools::custom_install::CustomInstallState::Idle,
            show_ai_custom_uninstall_confirm: None,
            ai_models_dialog: crate::panels::ai_models::AiModelsDialogState::default(),
            export_offline_warning: None,
            export_source_overwrite_warning: None,
            export_matte_warning: None,
            media_usage_cache: std::collections::HashMap::new(),
            media_usage_cache_dirty: true,
            analysis_generation: 0,
            show_new_project_dialog: false,
            selects_sequence: None,
            selects_range: None,
            selects_drag: None,
            selects_edit: false,
            clip_carry: None,
            aaf_install: None,
            aaf_reading: None,
            selects_height: 120.0,
            stacked_focus_swap: false,
            project_templates_dir: None,
            shared_library: crate::shared_library::dir().map(|d| crate::shared_library::SharedLibrary::load(&d)).unwrap_or_default(),
            shared_library_dir: None,
            shared_collection: String::new(),
            save_template_dialog: None,
            frame_glide_clip: None,
            pending_track_subject: None,
            tracking_subjects: Default::default(),
            track_subject_looked: None,
            status_tracking: Vec::new(),
            status_translation: None,
            frame_glide_drag: None,
            show_batch_rename_dialog: false,
            show_title_find: false,
            title_find: Default::default(),
            title_find_replace: String::new(),
            title_find_project: false,
            title_find_selected: None,
            batch_rename_ids: Vec::new(),
            batch_rename_mode: crate::app::BatchRenameMode::FindReplace,
            batch_rename_find: String::new(),
            batch_rename_replace: String::new(),
            batch_rename_match_case: true,
            batch_rename_prefix: String::new(),
            batch_rename_suffix: String::new(),
            batch_rename_start: 1,
            batch_rename_padding: 2,
            show_project_info: false,
            file_info: None,
            file_info_thumb: None,
            show_consolidate_dialog: false,
            renaming_sequence_idx: None,
            rename_sequence_buf: String::new(),
            show_autosave_recovery: false,
            autosave_recovery_path: None,
            autosave_original_path: None,
            dragging_audio_keyframe: None,
            dragging_video_keyframe: None,
            dragging_rubber_band: None,
            dragging_rubber_band_in_lane: false,
            dragging_bezier_handle: None,
            dragging_video_bezier_handle: None,
            dragging_audio_effect_keyframe: None,
            dragging_audio_effect_bezier_handle: None,
            keyframe_context_menu: None,
            automation_lane_param_menu: None,
            keyframe_clipboard: None,
            curve_key_clipboard: Vec::new(),
            selected_keyframe: None,
            key_selection: Default::default(),
            param_focus: None,
            automation_held: Default::default(),
            automation_pass: Default::default(),
            multi_key_drag: None,
            selected_camera_key: None,
            camera_key_menu: None,
            dragging_camera_key: None,
            show_camera_lane: false,
            rubber_band_param: RubberBandParam::default(),
            video_rubber_band_param: VideoParam::Opacity,
            timeline_mask_lane: None,
            mask_lane_drag: None,
            timeline_param_lane: None,
            param_lane_drag: None,
            audio_fade_overlay_mode: crate::prefs::FadeOverlayMode::default(),
            audio_show_rubber_band: true,
            audio_show_waveform: true,
            audio_anti_click_fade: true,
            default_fade_secs: 0.5,
            default_fade_curve: klipp_core::FadeCurve::default(),
            audio_show_crossfade_indicators: true,
            waveform_zoom: std::collections::HashMap::new(),
            copied_track_settings: None,
            audio_reference_clip_id: None,
            dragging_fade_handle: None,
            proxy_generating: std::collections::HashSet::new(),
            proxy_progress: std::collections::HashMap::new(),
            proxy_start_times: std::collections::HashMap::new(),
            proxy_status_message: None,
            proxy_toast_dismissed: false,
            viewer_transform_drag: None,
            motion_path_drag: None,
            viewer_corner_pin_drag: None,
            surface_tool: None,
            show_transform_controls: true,
            selected_title_object: None,
            title_object_drag: None,
            viewer_mask_drag: None,
            mask_edit: Default::default(),
            show_curve_editor: false,
            show_audio_mixer: false,
            show_history_panel: false,
            show_command_palette: false,
            command_palette_query: String::new(),
            command_palette_selected: 0,
            model_download: crate::model_download::ModelDownloadState::default(),
            ai_tools_state: {
                // Persistent state lives in ~/.config/klipp/ai_tools.toml:
                // last_picked default model per task + per-model
                // telemetry + user-added custom models. All three
                // load here. Custom models are also registered
                // against the shared registry below so they show up
                // in the panel dropdown + the AI Models dialog.
                let mut s = crate::ai_tools::AiToolsState::default();
                let loaded = crate::ai_tools::persistence::load().into_state_data();
                s.last_picked_model_per_task = loaded.last_picked_model_per_task;
                s.model_telemetry = loaded.model_telemetry;
                s.custom_models = loaded.custom_models;
                {
                    let reg = shared_registry.write();
                    if let Ok(mut r) = reg {
                        for entry in &s.custom_models {
                            register_custom_model(&mut r, entry);
                        }
                    }
                }
                s
            },
            // Both views observe the same `Arc<RwLock<ModelRegistry>>`
            // — runtime install / uninstall through either side
            // reflects on the other. Tested by
            // registry::tests::from_shared_views_see_each_others_writes.
            ai_tools_registry: crate::ai_tools::AiToolsRegistry::from_shared(
                std::sync::Arc::clone(&shared_registry),
            ),
            ml_registry: crate::ai_tools::RegistryHandle::from_shared(shared_registry),
            available_fonts: Vec::new(),
            favourite_fonts: Vec::new(),
            recent_fonts: Vec::new(),
            font_previews: Default::default(),
            missing_fonts_dialog: None,
            pending_title_data: None,
            focus_title_text: 0,
            pending_title_preview_clip: None,
            matte_thumb_for: None,
            matte_thumb_handle: None,
            glyph_position_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            letter_picker_texture: None,
            letter_picker_texture_for: None,
            matte_preview_active_target: None,
            pending_title_overlay_clear: false,
            clear_title_overlay: false,
            title_fade_duration: 0.5,
            multicam_viewer_active: false,
            multicam_angles: Vec::new(),
            multicam_active_angle: 0,
            viewer_text_edit: false,
            viewer_text_edit_object: None,
            viewer_text_edit_buffer: String::new(),
            viewer_text_edit_clip: None,
            viewer_text_edit_request_focus: false,
            viewer_text_edit_select_all: false,
            viewer_text_selection: None,
            viewer_text_selection_locked: None,
            viewer_text_cursor: 0,
            viewer_char_positions: Vec::new(),
            viewer_text_select_anchor: None,
            viewer_text_cursor_blink_time: 0.0,
            viewer_text_cursor_font_size: None,
            viewer_text_cursor_color: None,
        }
    }
}

impl AppState {
    /// Whether a node canvas, the grade's or the picture's, has the keys:
    /// its single keys and its Ctrl+G are its own, not the timeline's
    /// (#1742).
    pub fn node_canvas_focused(&self) -> bool {
        matches!(self.active_panel, Some(PanelKind::NodeGraph | PanelKind::PictureGraph))
    }

    /// Why the clip cannot have its scene cuts found and split (#616):
    /// not a clip of moving picture whose file is here, or on a locked
    /// track. `None` when it can.
    pub fn scene_cut_refusal(&self, clip_id: klipp_core::ClipId) -> Option<&'static str> {
        klipp_document::ops::scene_cut_refusal(self.sequence(), &self.media_pool, clip_id)
    }

    /// Whether the clip meets a piece of its own shot after it (`next`)
    /// or before it that it can be joined to (#683): the join, tried.
    pub fn can_join_through_edit(&self, clip_id: klipp_core::ClipId, next: bool) -> bool {
        let side = if next { klipp_document::ops::JoinSide::Next } else { klipp_document::ops::JoinSide::Previous };
        let op = klipp_document::ops::Op::Join { clip_id: klipp_document::ops::ClipRef::Id(clip_id), side };
        klipp_document::ops::resolve(self.sequence(), &self.media_pool, std::slice::from_ref(&op)).is_ok()
    }

    /// The clip ending at the playhead whose edit there can be joined: on
    /// the selected clip's track, or else the first picture track with
    /// one. What Timeline → Join Through Edit at Playhead closes.
    pub fn through_edit_at_playhead(&self) -> Option<klipp_core::ClipId> {
        let seq = self.sequence();
        let half = 0.5 / seq.fps.as_f64().max(1.0);
        let at = self.playhead.to_secs_f64();
        let on = |t: &klipp_timeline::Track| t.clips.iter().find(|c| (c.timeline_out.to_secs_f64() - at).abs() < half).map(|c| c.id).filter(|id| self.can_join_through_edit(*id, true));
        let selected = self.selected_clip.and_then(|id| seq.tracks.iter().find(|t| t.find_clip(id).is_some()));
        match selected {
            Some(t) => on(t),
            None => seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).find_map(on),
        }
    }
}

impl AppState {
    /// Switch multicam mode (#1844). On, it remembers the clip `open`ed,
    /// and a single viewer that may show the Source viewer shows it,
    /// since that is where the grid is. Off, it forgets the opened clip.
    pub fn set_multicam_mode(&mut self, on: bool, open: Option<ClipId>) {
        self.multicam_mode = on;
        if on {
            if open.is_some() {
                self.multicam_opened = open;
            }
            if !self.viewers_side_by_side() && !self.single_viewer_is_program() {
                self.viewer_mode = klipp_core::ViewerMode::Source;
            }
        } else {
            self.multicam_opened = None;
        }
    }

    /// Whether the viewer's Mask tool takes Delete, Enter and Escape
    /// (#74): the tool is up, the viewer has the focus, and there are
    /// points selected or a shape being drawn. Then they are the
    /// mask's, and never reach the timeline's clip.
    pub fn mask_keys_owned(&self) -> bool {
        self.active_tool == klipp_core::Tool::Mask && self.active_panel == Some(PanelKind::Viewer) && self.mask_edit.has_keys()
    }

    /// The scope dock under the viewer exists only in the Color
    /// workspace, where the timeline is hidden. Elsewhere it would take
    /// the timeline's rows, so the dock flag is ignored there.
    pub fn sequence(&self) -> &Sequence {
        &self.sequences[self.active_sequence_idx]
    }
    /// How the active sequence shows a position on its clock, and reads
    /// one back. Use `.position()` for a place on the timeline and
    /// `.duration()` for a length — drop-frame applies to the first and
    /// not the second; see `TimecodeDisplay`.
    pub fn timecode_display(&self) -> klipp_core::TimecodeDisplay {
        self.sequence().timecode_display()
    }
    pub fn sequence_mut(&mut self) -> &mut Sequence {
        &mut self.sequences[self.active_sequence_idx]
    }
    /// One `BladeClip` per clip under the playhead, for Split at Playhead.
    /// A clip whose linked partner is already queued is left out: blading
    /// a clip cuts its partner too, so queueing both would cut the partner
    /// a second time exactly on its new edge. A locked track is left
    /// alone, and a clip the playhead sits within a frame of an edge of
    /// is not cut, by the blade's own rule (`klipp_document::blade`); a
    /// pair across a lock is queued from its unlocked side so the blade
    /// refuses it whole, with a word on which track to unlock.
    pub fn split_at_playhead_commands(&self) -> Vec<klipp_core::AppCommand> {
        let ph = self.playhead;
        let fps = self.sequence().fps;
        let mut queued: Vec<ClipId> = Vec::new();
        let mut cmds = Vec::new();
        for (track_index, track) in self.sequence().tracks.iter().enumerate() {
            // Locked and untargeted tracks are left whole (#915).
            if track.locked || !self.sequence().is_targeted(track_index) { continue; }
            let Some((_, clip)) = track.clip_at_timecode(ph) else { continue };
            if !klipp_document::blade::cut_is_inside(clip, ph, fps) { continue; }
            if clip.linked_clip_id.is_some_and(|l| queued.contains(&l)) { continue; }
            queued.push(clip.id);
            cmds.push(klipp_core::AppCommand::BladeClip { track_index, clip_id: clip.id, cut_at: ph });
        }
        cmds
    }
    pub fn sequence_duration(&self) -> Timecode {
        self.sequence().total_duration()
    }

    /// Read-only access to the Source / Program viewer's zoom + pan.
    pub fn view_for(&self, kind: klipp_core::ViewerMode) -> ViewerView {
        match kind {
            klipp_core::ViewerMode::Source => self.source_view,
            klipp_core::ViewerMode::Program => self.program_view,
        }
    }
    /// Mutable access to the Source / Program viewer's zoom + pan.
    pub fn view_for_mut(&mut self, kind: klipp_core::ViewerMode) -> &mut ViewerView {
        match kind {
            klipp_core::ViewerMode::Source => &mut self.source_view,
            klipp_core::ViewerMode::Program => &mut self.program_view,
        }
    }

    /// Read-only access to the active sequence's undo/redo stack. Falls
    /// back to a pinned default when the map entry hasn't been seeded
    /// yet (only happens for legacy on-disk projects loaded before the
    /// per-sequence migration).
    pub fn active_history(&self) -> &CommandHistory {
        let id = self.sequences[self.active_sequence_idx].id;
        self.history_for(id)
    }
    /// Mutable access to the active sequence's undo/redo stack. Lazily
    /// inserts an empty history sized by `history_max_size` if needed.
    pub fn active_history_mut(&mut self) -> &mut CommandHistory {
        let id = self.sequences[self.active_sequence_idx].id;
        self.history_for_mut(id)
    }
    pub fn history_for(&self, seq_id: SequenceId) -> &CommandHistory {
        // The map is seeded eagerly on sequence creation, so a missing
        // entry indicates a project loaded before the migration —
        // return a pinned empty history rather than panic on undo.
        static EMPTY: std::sync::OnceLock<CommandHistory> = std::sync::OnceLock::new();
        self.command_histories
            .get(&seq_id)
            .unwrap_or_else(|| EMPTY.get_or_init(CommandHistory::new))
    }
    pub fn history_for_mut(&mut self, seq_id: SequenceId) -> &mut CommandHistory {
        let max = self.history_max_size;
        let shared = std::sync::Arc::clone(&self.control_shared);
        let h = self.command_histories.entry(seq_id).or_insert_with(|| {
            let mut h = CommandHistory::new();
            h.set_max_size(max);
            h
        });
        // Every push, undo and redo advances the document revision,
        // whichever path pushed it. Set here rather than at creation
        // because the shared counter can arrive after the histories do.
        if !h.has_revision_shared() {
            h.set_revision_shared(shared);
        }
        if !h.has_clock() {
            h.set_clock(std::sync::Arc::clone(&self.history_clock));
        }
        h
    }

    /// The project's history, set up as the sequences' are: their size,
    /// the document revision and the shared clock (#1986).
    pub fn project_history_mut(&mut self) -> &mut CommandHistory {
        let h = &mut self.project_history;
        h.set_max_size(self.history_max_size);
        if !h.has_revision_shared() {
            h.set_revision_shared(std::sync::Arc::clone(&self.control_shared));
        }
        if !h.has_clock() {
            h.set_clock(std::sync::Arc::clone(&self.history_clock));
        }
        h
    }

    /// Which history ctrl+z takes the next step back from (#1986): the
    /// project's or the active sequence's, whichever top step moved last.
    pub fn next_undo(&self) -> Option<crate::history::Scope> {
        later(self.active_history().undo_stamp(), self.project_history.undo_stamp())
    }

    /// Which history redo puts the next step back from: the step taken
    /// back last, unless an edit made since has made it stale, as an edit
    /// in its own history would. A sequence's step goes stale with a newer
    /// project step; a project step with a newer edit in any sequence,
    /// since putting it back would put back the grades it converted over
    /// that edit.
    pub fn next_redo(&self) -> Option<crate::history::Scope> {
        let (seq, project) = (self.redo_fresh_sequence(), self.redo_fresh_project());
        later(seq.then(|| self.active_history().redo_stamp()).flatten(), project.then(|| self.project_history.redo_stamp()).flatten())
    }

    /// Whether the active sequence's redo is newer than every project edit.
    fn redo_fresh_sequence(&self) -> bool {
        self.active_history().redo_stamp().is_some_and(|s| s > self.project_history.last_edit_stamp())
    }

    /// Whether the project's redo is newer than every sequence's edits.
    fn redo_fresh_project(&self) -> bool {
        let last = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        self.project_history.redo_stamp().is_some_and(|s| s > last)
    }

    /// Which history the next undo and redo come from, for the control
    /// socket (#1986): "project" or "sequence".
    pub fn undo_scopes(&self) -> (Option<&'static str>, Option<&'static str>) {
        let name = |s: crate::history::Scope| match s {
            crate::history::Scope::Project => "project",
            crate::history::Scope::Sequence => "sequence",
        };
        (self.next_undo().map(name), self.next_redo().map(name))
    }

    /// What ctrl+z would take back, as the Edit menu and the control
    /// socket name it: a project step says it is the project's.
    pub fn undo_label(&self) -> Option<String> {
        match self.next_undo()? {
            crate::history::Scope::Project => self.project_history.undo_label().map(|l| format!("{l} (project)")),
            crate::history::Scope::Sequence => self.active_history().undo_label().map(str::to_owned),
        }
    }

    /// What redo would put back, named as `undo_label` names it.
    pub fn redo_label(&self) -> Option<String> {
        match self.next_redo()? {
            crate::history::Scope::Project => self.project_history.redo_label().map(|l| format!("{l} (project)")),
            crate::history::Scope::Sequence => self.active_history().redo_label().map(str::to_owned),
        }
    }

    /// How many steps ctrl+z can take back from here: the active
    /// sequence's and the project's.
    pub fn undo_depth(&self) -> usize {
        self.active_history().undo_count() + self.project_history.undo_count()
    }

    /// How many steps redo can put back from here.
    pub fn redo_depth(&self) -> usize {
        let last_sequence_edit = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        let project_edit = self.project_history.last_edit_stamp();
        self.active_history().redo_stamped().filter(|(s, _)| *s > project_edit).count()
            + self.project_history.redo_stamped().filter(|(s, _)| *s > last_sequence_edit).count()
    }

    /// Drop redo steps an edit has made stale (#1986): they would never be
    /// offered again, and a project step can hold a whole colour change.
    /// A stack's steps go stale from the top down, so a stale top means
    /// all of it.
    fn prune_stale_redo(&mut self) {
        if self.project_history.redo_stamp().is_some() && !self.redo_fresh_project() {
            self.project_history.clear_redo();
        }
        if self.active_history().redo_stamp().is_some() && !self.redo_fresh_sequence() {
            self.active_history_mut().clear_redo();
        }
    }

    /// The steps in reach, as the History panel lists them (#1986): the
    /// active sequence's and the project's in the order they moved, each
    /// with its label, the next to undo last; and those redo can put
    /// back, the next to redo last. A project step's label says so.
    pub fn history_view(&self) -> (Vec<String>, Vec<String>) {
        let (seq, project) = (self.active_history(), &self.project_history);
        let label = |e: &HistoryEntry, is_project: bool| if is_project { format!("{} (project)", e.label()) } else { e.label().to_owned() };
        let mut undo: Vec<(u64, String)> = seq.undo_stamped().map(|(s, e)| (s, label(e, false))).chain(project.undo_stamped().map(|(s, e)| (s, label(e, true)))).collect();
        undo.sort_by_key(|(s, _)| *s);
        let last_sequence_edit = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        let mut redo: Vec<(u64, String)> = seq.redo_stamped().filter(|(s, _)| *s > project.last_edit_stamp()).map(|(s, e)| (s, label(e, false)))
            .chain(project.redo_stamped().filter(|(s, _)| *s > last_sequence_edit).map(|(s, e)| (s, label(e, true))))
            .collect();
        redo.sort_by_key(|(s, _)| *s);
        (undo.into_iter().map(|(_, l)| l).collect(), redo.into_iter().map(|(_, l)| l).collect())
    }

    /// Take the next step back, from whichever history holds it.
    pub fn take_undo(&mut self) -> Option<HistoryEntry> {
        self.prune_stale_redo();
        match self.next_undo()? {
            crate::history::Scope::Project => self.project_history_mut().undo(),
            crate::history::Scope::Sequence => self.active_history_mut().undo(),
        }
    }

    /// Put the next step back, from whichever history holds it.
    pub fn take_redo(&mut self) -> Option<HistoryEntry> {
        self.prune_stale_redo();
        match self.next_redo()? {
            crate::history::Scope::Project => self.project_history_mut().redo(),
            crate::history::Scope::Sequence => self.active_history_mut().redo(),
        }
    }
    /// A clip's input colour space set to `log_format`, as the grade's
    /// Input Colour Space menu sets it (`SetClipLogFormat`), recorded as
    /// one step (#1396): the clip's Input Transform (put in after its
    /// Source when it has none), and, since the format belongs to the file
    /// (a camera file has one encoding, and the decoder converts per file),
    /// the file's own format and every clip of it, in every sequence. The
    /// file, when the clip has one, for the caller to bring the engine and
    /// its pictures alongside.
    pub fn set_clip_log_format(&mut self, clip_id: klipp_core::ClipId, log_format: klipp_core::LogFormat) -> Option<klipp_core::MediaId> {
        let active = self.active_sequence_idx;
        let media = self.sequences[active].all_clips().find(|c| c.id == clip_id)?.media_id();
        let format_of = |state: &Self| media.and_then(|m| state.media_pool.get(&m)).map_or((Default::default(), false), |i| (i.log_format, i.log_format_user_set));
        let old = format_of(self);
        let of_file = |c: &klipp_timeline::Clip| c.id == clip_id || (media.is_some() && c.media_id() == media);
        let before: Vec<(klipp_core::ClipId, klipp_nodes::NodeGraph)> = self.sequences.iter().flat_map(|s| s.all_clips()).filter(|c| of_file(c)).map(|c| (c.id, c.grade().clone())).collect();
        if let Some(clip) = self.sequences[active].all_clips_mut().find(|c| c.id == clip_id) {
            clip.set_input_log_format(log_format);
        }
        if let Some(mid) = media {
            if let Some(item) = self.media_pool.get_mut(&mid) {
                item.log_format = log_format;
                item.log_format_user_set = true;
            }
            for clip in self.sequences.iter_mut().flat_map(|s| s.all_clips_mut()).filter(|c| c.media_id() == Some(mid)) {
                clip.set_input_log_format(log_format);
            }
        }
        let grades = before.into_iter().filter_map(|(id, was)| {
            let now = self.sequences.iter().find_map(|s| s.all_clips().find(|c| c.id == id))?.grade().clone();
            Some((id, was, now))
        }).collect();
        let new = format_of(self);
        self.push_history(EditCommand::SetMediaLogFormat { media_id: media, old, new, grades });
        self.dirty = true;
        media
    }

    /// Forget a file's thumbnail and filmstrip, so they are made again for
    /// its current decode settings: the cached filmstrip emptied, its
    /// textures taken out (returned, for the caller to drop once they are
    /// off screen) and its filmstrip no longer counted as in progress.
    pub(crate) fn forget_media_pictures(&mut self, media_id: klipp_core::MediaId) -> Vec<egui::TextureHandle> {
        self.thumbnail_cache.set_filmstrip(media_id, Vec::new());
        self.filmstrip_in_progress.remove(&media_id);
        let strip = self.filmstrip_textures.remove(&media_id).into_iter().flatten().map(|(_, t)| t);
        strip.chain(self.thumbnail_textures.remove(&media_id)).collect()
    }

    /// The files whose pictures a document edit asks to have made again
    /// (`Effects::refresh_media_pictures`), each once, that the pool still
    /// holds.
    pub(crate) fn pictures_to_remake(&self, effects: &klipp_document::apply::Effects) -> Vec<klipp_core::MediaId> {
        let mut ids: Vec<klipp_core::MediaId> = Vec::new();
        for id in &effects.refresh_media_pictures {
            if !ids.contains(id) && self.media_pool.get(id).is_some() {
                ids.push(*id);
            }
        }
        ids
    }

    /// Grade track `id`'s own grade in the Colour workspace (#1408), as
    /// the toolbar's Track menu picks it: the other targets let go, and no
    /// node of the last one selected.
    pub fn pick_track_grade(&mut self, id: klipp_core::TrackId) {
        self.grade_track = Some(id);
        self.grade_timeline = false;
        self.grade_group_stage = None;
        self.graph_descent_path.clear();
        self.selected_node_id = None;
        self.selected_node_ids.clear();
    }

    /// Set published field `index` of title `clip_id` (#1139), one undo
    /// step per change: a drag's steps (`done` false) shown at once and
    /// recorded as one when it ends, typed text joined as the title's text
    /// is, and colours picked close together on one field joined too. A
    /// field whose layer is gone is refused with nothing changed.
    pub fn set_title_field(&mut self, clip_id: klipp_core::ClipId, index: usize, value: &klipp_core::TitleFieldValue, done: bool) -> Result<(), String> {
        use klipp_core::TitleFieldValue as V;
        let seq = self.active_sequence_idx;
        let clip = self.sequences[seq].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).ok_or("the title is gone")?;
        let klipp_timeline::ClipKind::Title(td) = &mut clip.kind else { return Err("that clip isn't a title".into()) };
        let now = td.clone();
        let mut edited = now.clone();
        edited.set_field(index, value)?;
        *td = edited.clone();
        self.dirty = true;
        // The title as the gesture found it, kept from its first step.
        let old = match self.title_field_before.take() {
            Some((c, before)) if c == clip_id => before,
            _ => now,
        };
        if !done {
            self.title_field_before = Some((clip_id, old));
            return Ok(());
        }
        if old == edited {
            return Ok(());
        }
        let description = match value {
            V::Text(_) => "Edit Title Text",
            V::Colour(_) => "Title Colour",
            V::Speed(_) => "Title Speed",
            V::Size(_) => "Title Size",
        };
        let history = self.active_history_mut();
        if matches!(value, V::Text(_)) && history.try_coalesce_title_text(clip_id, &old, &edited) {
            return Ok(());
        }
        let recent = self.title_colour_edit.is_some_and(|(c, i, at)| c == clip_id && i == index && at.elapsed() < std::time::Duration::from_millis(1500));
        if matches!(value, V::Colour(_)) {
            self.title_colour_edit = Some((clip_id, index, std::time::Instant::now()));
            if recent && self.active_history_mut().amend_last_title(clip_id, description, &edited) {
                return Ok(());
            }
        }
        self.active_history_mut().push(crate::history::HistoryEntry {
            command: crate::history::EditCommand::EditTitle { clip_id, description, old_data: old, new_data: edited },
        });
        if matches!(value, V::Text(_)) {
            self.active_history_mut().note_title_text_edit(clip_id);
        }
        Ok(())
    }

    /// The track header's Grade in Colour (#1408): the Colour workspace,
    /// grading that track's grade.
    pub fn grade_track_in_colour(&mut self, id: klipp_core::TrackId) {
        self.active_workspace = Workspace::Color;
        self.pick_track_grade(id);
    }

    /// Finish a probe, recording explicit pool imports only when they first
    /// enter the project. Re-probes and timeline drops keep their existing history.
    pub(crate) fn complete_pool_import(&mut self, item: klipp_media::MediaItem, is_new: bool) {
        let requested = self.pending_undo_imports.remove(&item.id);
        if requested && is_new {
            self.push_history(EditCommand::AddMediaToPool { item: item.clone() });
        }
        self.media_pool.add(item);
    }

    /// Push a single edit onto the active sequence's undo stack and
    /// bump the project-level autosave counter. Replaces the prior
    /// `state.active_history_mut().push(HistoryEntry { sequence_idx, command })`
    /// idiom — callers no longer pass a sequence index.
    pub fn push_history(&mut self, command: EditCommand) {
        let command = self.with_matched_follow(command);
        // The project's steps go to its own history, reachable from any
        // sequence (#1986); inside a batch already open they are one more
        // edit of it, and the batch stays where it is.
        let project = command.scope() == crate::history::Scope::Project && !self.active_history().is_batching();
        if project {
            // A gesture of the sequence's under way ends here: typing,
            // a dragged value, a title colour being picked go on as new
            // steps, not folded into one from before this (#1986).
            self.active_history_mut().end_burst();
            self.tracks_undo_gesture = None;
            self.title_colour_edit = None;
            self.project_history_mut().push(HistoryEntry { command });
        } else {
            self.active_history_mut().push(HistoryEntry { command });
        }
        self.prune_stale_redo();
        self.history_ops_since_save = self.history_ops_since_save.saturating_add(1);
    }
    /// `command`, already applied, with what it means for the clips kept
    /// matched to a shot (#2236): their follow, applied now and recorded in
    /// the same step, so undo and redo replay it rather than work it out
    /// again. A link that can no longer be followed is dropped, and the
    /// toast says why.
    fn with_matched_follow(&mut self, command: EditCommand) -> EditCommand {
        if command.scope() != crate::history::Scope::Sequence || !self.sequence().all_clips().any(|c| c.matched_to.is_some()) {
            return command;
        }
        // The sequence as it was: the edit undone on a copy. A sequence's
        // edit is undone on the sequence alone, the media pool left out.
        let mut before = vec![self.sequence().clone()];
        let (mut pool, mut fps) = (klipp_media::MediaPool::new(), self.fps);
        klipp_document::apply::DocumentView { sequences: &mut before, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&command);
        let follow = klipp_document::matched::follow(&before[0], self.sequence(), &self.media_pool);
        if follow.edits.is_empty() {
            return command;
        }
        let active = self.active_sequence_idx;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut self.sequences, active, media_pool: &mut self.media_pool, fps: &mut self.fps };
        for e in &follow.edits {
            view.apply_forward(e);
        }
        self.matched_follow_sync = true;
        if let [(_, why), rest @ ..] = follow.dropped.as_slice() {
            let more = if rest.is_empty() { String::new() } else { format!(" (and {} more)", rest.len()) };
            self.toast.show(format!("A clip is no longer kept matched: {why}{more}"));
        }
        let (label, edit) = match command {
            EditCommand::Named { label, edit } => (label, *edit),
            other => (other.label().to_string(), other),
        };
        EditCommand::Named { label, edit: Box::new(EditCommand::Batch(std::iter::once(edit).chain(follow.edits).collect())) }
    }
    /// `edits`, already applied, recorded as one undo step named `label`,
    /// or, inside a batch already open (an agent's, a socket batch), as
    /// one more edit of that batch, which then names the step. Opening a
    /// batch of its own there threw away what the open one had collected,
    /// as the path edits' recorder did (#1446). Each edit counts towards
    /// autosave, as when they were pushed one by one.
    pub fn push_edits_as_one(&mut self, edits: &[EditCommand], label: &str) {
        let edit = match edits {
            [] => return,
            [one] => one.clone(),
            many => EditCommand::Batch(many.to_vec()),
        };
        self.push_history(EditCommand::Named { label: label.to_string(), edit: Box::new(edit) });
        self.history_ops_since_save = self.history_ops_since_save.saturating_add(edits.len() as u64 - 1);
    }
    /// Reset the project-level autosave counter. Call from the
    /// project-save path.
    pub fn reset_ops_since_save(&mut self) {
        self.history_ops_since_save = 0;
    }
    /// Apply `max` to every existing per-sequence history and update
    /// the seed for sequences created later. Call from the prefs-changed path.
    pub fn set_history_max_size(&mut self, max: usize) {
        self.history_max_size = max;
        for h in self.command_histories.values_mut() {
            h.set_max_size(max);
        }
        self.project_history.set_max_size(max);
    }
    /// Are *any* of the per-sequence histories mid-drag? Used by the
    /// autosave timer to skip while the user is gesturing.
    pub fn any_drag_in_progress(&self) -> bool {
        self.command_histories.values().any(|h| h.is_any_drag_in_progress()) || self.project_history.is_any_drag_in_progress()
    }

    /// Resolve `selected_clip` to its (track_idx, &Clip) location in
    /// the active sequence, using the `selected_clip_loc_cache` to
    /// avoid the O(tracks × clips) walk on every UI render.
    ///
    /// Cache contract: stores `(clip_id, track_idx)`. The next call
    /// checks the cached track first; if the clip is still there it
    /// returns immediately. Cache misses (clip moved tracks, was
    /// deleted, or selection changed) trigger a full sequence walk
    /// and refresh the cache. Self-healing — call sites don't have
    /// to invalidate after edits.
    ///
    /// Returns `None` when no clip is selected, or the selected clip
    /// id no longer exists in the active sequence (in which case
    /// the cache is also cleared).
    pub fn selected_clip_loc(&self) -> Option<(usize, &klipp_timeline::Clip)> {
        let cid = self.selected_clip?;
        find_clip_cached(self.sequence(), cid, &self.selected_clip_loc_cache)
    }
}

/// Free-function form of the selected-clip cached lookup. Extracted
/// so the cache-hit / cache-miss / stale / deleted paths can be unit-
/// tested directly against a `Sequence` + `Cell<...>` pair without
/// having to materialise a full `AppState`.
pub(crate) fn find_clip_cached<'a>(
    seq: &'a klipp_timeline::Sequence,
    cid: klipp_core::ClipId,
    cache: &std::cell::Cell<Option<(klipp_core::ClipId, usize)>>,
) -> Option<(usize, &'a klipp_timeline::Clip)> {
    // Fast path: cache says this clip lives on `cached_ti`. Verify
    // it's still there (one Track::find_clip).
    if let Some((cached_cid, cached_ti)) = cache.get() {
        if cached_cid == cid {
            if let Some(track) = seq.tracks.get(cached_ti) {
                if let Some(clip) = track.find_clip(cid) {
                    return Some((cached_ti, clip));
                }
            }
            // Same id, cache stale (clip moved tracks or track
            // deleted). Fall through to the full walk; the
            // refreshed cache write below covers the next call.
        }
        // selected_clip changed since the cache was set —
        // also fall through.
    }
    // Slow path: walk every track. Updates the cache so the next
    // call hits the fast path.
    for (ti, track) in seq.tracks.iter().enumerate() {
        if let Some(clip) = track.find_clip(cid) {
            cache.set(Some((cid, ti)));
            return Some((ti, clip));
        }
    }
    // Phantom selection — clip id doesn't exist anywhere. Clear
    // the cache so we don't keep paying the full-walk cost
    // searching for a non-existent id.
    cache.set(None);
    None
}

/// How often `ensure_analyses` looks with nothing edited.
pub const ANALYSIS_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// The least time between looks that follow edits.
pub const ANALYSIS_EDIT_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether `ensure_analyses` looks now ([`AppState::analysis_due`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisDue {
    Now,
    /// An edit is waiting to be looked at, after this long.
    After(std::time::Duration),
    /// Nothing new to look at yet.
    NotYet,
}

/// How long an analysis that ended without its asset waits before it's
/// started again: a file that can't be analysed isn't tried on every look.
pub const ANALYSIS_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// An analysis `ensure_analyses` started: when its run ended, if it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedAnalysis {
    pub ended: Option<std::time::Instant>,
}

/// An analysis a clip of the sequence reads: its asset's key and kind, a
/// clip that reads it, and whether a run of it is making it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisUse {
    pub key: klipp_analysis::assets::AssetKey,
    pub kind: klipp_analysis::assets::AssetKind,
    pub clip: klipp_core::ClipId,
    pub running: bool,
}

#[cfg(test)]
mod tests {

    use super::{
        borrow_released_by_hand, browser_borrow_for, BrowserBorrow, Workspace,
    };

    /// Edits recorded as one inside a batch already open (a socket or an
    /// agent batch) join it: the batch keeps its earlier edit and is one
    /// step. A batch opened of their own threw that edit away, the class
    /// of #1446's path-edit bug. With no batch open they are one step,
    /// named.
    #[test]
    fn edits_as_one_inside_an_open_batch_join_it() {
        use crate::history::EditCommand;
        let mark = |l: &str| EditCommand::Named { label: l.into(), edit: Box::new(EditCommand::Batch(vec![])) };
        let mut s = super::AppState::default();
        s.active_history_mut().begin_batch();
        s.push_history(mark("first"));
        s.push_edits_as_one(&[mark("second"), mark("third")], "Paste Attributes");
        s.active_history_mut().end_batch();
        assert_eq!(s.active_history().undo_count(), 1, "one step");
        let entry = s.active_history_mut().undo().expect("the batch's step");
        let EditCommand::Batch(inner) = &entry.command else { panic!("not the open batch's step: {:?}", entry.command) };
        assert_eq!(inner.len(), 2, "the batch kept its first edit: {inner:?}");
        let mut alone = super::AppState::default();
        alone.push_edits_as_one(&[mark("a"), mark("b")], "Paste Attributes");
        assert_eq!(alone.active_history().undo_count(), 1);
        assert_eq!(alone.active_history().undo_label(), Some("Paste Attributes"));
    }

    /// Edit is where you drag effects from, so it is the one workspace
    /// that keeps the browser.
    #[test]
    fn only_the_workspaces_with_nothing_to_browse_borrow_it() {
        assert!(!Workspace::Edit.borrows_the_browser());
        assert!(Workspace::Color.borrows_the_browser());
        assert!(Workspace::Export.borrows_the_browser());
    }

    #[test]
    fn entering_a_borrowing_workspace_records_and_collapses() {
        assert_eq!(
            browser_borrow_for(Workspace::Edit, Workspace::Export),
            BrowserBorrow::Take,
        );
        assert_eq!(
            browser_borrow_for(Workspace::Edit, Workspace::Color),
            BrowserBorrow::Take,
        );
    }

    #[test]
    fn leaving_for_edit_puts_the_browser_back() {
        assert_eq!(
            browser_borrow_for(Workspace::Export, Workspace::Edit),
            BrowserBorrow::Give,
        );
        assert_eq!(
            browser_borrow_for(Workspace::Color, Workspace::Edit),
            BrowserBorrow::Give,
        );
    }

    /// The case that would break it. Both workspaces borrow, so
    /// recording again would store a *collapsed* browser as the thing
    /// to restore, and Edit would never get it back.
    #[test]
    fn moving_between_two_borrowers_does_not_record_a_second_time() {
        for (a, b) in [
            (Workspace::Color, Workspace::Export),
            (Workspace::Export, Workspace::Color),
        ] {
            assert_eq!(browser_borrow_for(a, b), BrowserBorrow::KeepTaken, "{a:?} -> {b:?}");
        }
    }

    #[test]
    fn edit_to_edit_leaves_the_panel_alone() {
        assert_eq!(
            browser_borrow_for(Workspace::Edit, Workspace::Edit),
            BrowserBorrow::Leave,
        );
    }

    /// A round trip has to be lossless in both directions: someone who
    /// had the browser hidden in Edit must not find it opened for them
    /// on the way back.
    #[test]
    fn a_round_trip_restores_whatever_was_there() {
        for started_visible in [true, false] {
            let mut visible = started_visible;
            // What the app would stash. `Take` always records, so this
            // is a plain bool rather than the app's `Option`, which
            // also carries "nothing borrowed".
            let held: bool;

            match browser_borrow_for(Workspace::Edit, Workspace::Export) {
                BrowserBorrow::Take => {
                    held = visible;
                    visible = false;
                }
                other => panic!("expected Take, got {other:?}"),
            }
            assert!(!visible, "the browser should be collapsed inside Export");

            match browser_borrow_for(Workspace::Export, Workspace::Edit) {
                BrowserBorrow::Give => visible = held,
                other => panic!("expected Give, got {other:?}"),
            }
            assert_eq!(visible, started_visible, "round trip changed the browser");
        }
    }

    /// Opening the browser inside a borrowing workspace is an explicit
    /// ask, and has to outrank the borrow — otherwise leaving quietly
    /// undoes it.
    #[test]
    fn an_explicit_ask_inside_the_workspace_wins() {
        assert!(borrow_released_by_hand(true, Workspace::Export, true));
        assert!(borrow_released_by_hand(true, Workspace::Color, true));
        // Still collapsed: the borrow stands.
        assert!(!borrow_released_by_hand(true, Workspace::Export, false));
        // Nothing borrowed, nothing to release.
        assert!(!borrow_released_by_hand(false, Workspace::Export, true));
        // Edit never borrows, so a visible browser there says nothing.
        assert!(!borrow_released_by_hand(true, Workspace::Edit, true));
    }
    use super::*;

    #[test]
    fn the_scopes_toggle_docks_only_in_color() {
        let mut s = AppState { show_color_panel: false, scope_overlay_mode: false, ..Default::default() };
        s.toggle_scopes();
        assert!(s.scope_window.open && !s.show_scopes);
        assert!(s.scopes_shown() && !s.scopes_docked());
        s.toggle_scopes();
        assert!(!s.scopes_shown());

        s.show_color_panel = true;
        s.toggle_scopes();
        assert!(s.show_scopes && s.scopes_docked() && !s.scope_window.open);
        // Leaving Color keeps the flag but the dock goes with the workspace.
        s.show_color_panel = false;
        assert!(!s.scopes_docked());
    }

    #[test]
    fn subtitle_editor_state_default_is_inactive() {
        let s = SubtitleEditorState::default();
        assert!(s.editing_text.is_none());
        assert!(s.editing_timecode.is_none());
        assert!(s.text_buf.is_empty());
        assert!(s.timecode_buf.is_empty());
        assert!(!s.ripple);
    }

    #[test]
    fn consolidate_progress_channel_roundtrip() {
        // Sanity: the mpsc channel used by ConsolidateProgress can deliver
        // both Progress and Done messages, and try_recv returns them in order.
        let (tx, rx) = std::sync::mpsc::channel::<ConsolidateMessage>();
        tx.send(ConsolidateMessage::Progress {
            done: 1,
            total: 3,
            filename: "clip.mp4".into(),
        })
        .unwrap();
        tx.send(ConsolidateMessage::Done(Err("simulated".into())))
            .unwrap();
        drop(tx);

        let first = rx.recv().unwrap();
        match first {
            ConsolidateMessage::Progress { done, total, filename } => {
                assert_eq!(done, 1);
                assert_eq!(total, 3);
                assert_eq!(filename, "clip.mp4");
            }
            _ => panic!("expected Progress first"),
        }
        let second = rx.recv().unwrap();
        assert!(matches!(second, ConsolidateMessage::Done(Err(_))));
    }

    #[test]
    fn split_compare_state_round_trip() {
        // The compare-effect state is an Option<(ClipId, usize, bool)>. The
        // bool discriminates builtin (false) from OFX (true). Round-trip the
        // two cases plus the cleared state to guard against a field shape
        // change.
        let cid = klipp_core::ClipId::new();

        let mut slot: Option<(klipp_core::ClipId, usize, bool)> = None;
        assert!(slot.is_none());

        slot = Some((cid, 3, false));
        assert_eq!(slot, Some((cid, 3, false)));
        assert!(matches!(slot, Some((_, 3, false))));

        slot = Some((cid, 7, true));
        assert_eq!(slot, Some((cid, 7, true)));
        assert!(matches!(slot, Some((_, _, true))));

        slot = None;
        assert!(slot.is_none());
    }

    #[test]
    fn split_compare_clears_on_effect_remove() {
        // Behaviour mirrored from the RemoveBuiltinEffect / RemoveEffect
        // handlers in app.rs — on remove, the compare pointer is cleared
        // when it targeted the removed effect, or decremented when it
        // targeted a later one in the same clip, or left alone otherwise.
        fn adjust(
            slot: &mut Option<(klipp_core::ClipId, usize, bool)>,
            removed_clip: klipp_core::ClipId,
            removed_index: usize,
            removed_is_ofx: bool,
        ) {
            if let Some((cid, idx, is_ofx)) = *slot {
                if cid == removed_clip && is_ofx == removed_is_ofx {
                    if idx == removed_index {
                        *slot = None;
                    } else if idx > removed_index {
                        *slot = Some((cid, idx - 1, is_ofx));
                    }
                }
            }
        }

        let a = klipp_core::ClipId::new();
        let b = klipp_core::ClipId::new();

        // Case 1: compared effect is removed → cleared.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 2, false);
        assert!(slot.is_none());

        // Case 2: earlier sibling removed → index shifts down.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 0, false);
        assert_eq!(slot, Some((a, 1, false)));

        // Case 3: later sibling removed → index unchanged.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 5, false);
        assert_eq!(slot, Some((a, 2, false)));

        // Case 4: different clip → untouched.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, b, 2, false);
        assert_eq!(slot, Some((a, 2, false)));

        // Case 5: different list (builtin vs OFX) → untouched.
        let mut slot = Some((a, 2, true));
        adjust(&mut slot, a, 2, false);
        assert_eq!(slot, Some((a, 2, true)));
    }

    // ── Workspace layout tests (#39) ──

    #[test]
    fn builtin_layouts_has_five_entries() {
        let layouts = builtin_workspace_layouts();
        assert_eq!(layouts.len(), 5);
    }

    #[test]
    fn builtin_layout_names_are_unique() {
        let layouts = builtin_workspace_layouts();
        let names: std::collections::HashSet<_> =
            layouts.iter().map(|l| l.name.clone()).collect();
        assert_eq!(names.len(), layouts.len());
    }

    #[test]
    fn builtin_layouts_include_expected_names() {
        let layouts = builtin_workspace_layouts();
        let names: std::collections::HashSet<_> =
            layouts.iter().map(|l| l.name.clone()).collect();
        // "Color" keeps the American spelling deliberately: this name
        // is written into workspaces.json and matched back by string,
        // so respelling it would orphan every saved workspace that
        // refers to it. The label a user sees is British; this is an
        // identifier that happens to look like a word.
        for expected in ["Logging", "Editing", "Effects", "Audio", "Color"] {
            assert!(names.contains(expected), "missing built-in: {expected}");
        }
    }

    /// The claim in the test above — that the stored name is an
    /// identifier and the user sees a British label — was untrue for a
    /// while: the Window menu and the workspace manager both rendered
    /// `name` straight to the screen, so "Colour" and "Color" sat two
    /// lines apart in the same menu. Hold the two apart here.
    #[test]
    fn no_built_in_shows_an_american_spelling() {
        for layout in builtin_workspace_layouts() {
            let shown = layout.display_name();
            assert!(
                !shown.contains("Color"),
                "built-in {:?} would put {shown:?} on screen",
                layout.name,
            );
        }
        let color = builtin_workspace_layouts()
            .into_iter()
            .find(|l| l.name == "Color")
            .expect("the Color built-in");
        assert_eq!(color.display_name(), "Colour");
        // …while the identifier that workspaces.json is matched against
        // stays exactly as it was written.
        assert_eq!(color.name, "Color");
    }

    #[test]
    fn apply_layout_with_none_sizes_preserves_existing_size() {
        // Mirrors the apply path in app.rs: a None size must leave the
        // current pixel width untouched. Pure logic — no egui needed.
        let mut browser_width = 350.0f32;
        let layout_browser_width: Option<f32> = None;
        if let Some(w) = layout_browser_width {
            browser_width = w;
        }
        assert_eq!(browser_width, 350.0);

        // Sanity: Some _does_ overwrite.
        let layout_browser_width: Option<f32> = Some(420.0);
        if let Some(w) = layout_browser_width {
            browser_width = w;
        }
        assert_eq!(browser_width, 420.0);
    }

    #[test]
    fn color_layout_has_color_panel_height() {
        // The Color preset is the only built-in that uses the color
        // panel — guard that the dedicated size field is populated so
        // applying Color doesn't leave an unsized panel.
        let layouts = builtin_workspace_layouts();
        let color = layouts.iter().find(|l| l.name == "Color").unwrap();
        assert!(color.show_color_panel);
        assert!(color.color_panel_height.is_some());
    }

    // ── selected-clip cache (U-11) ──

    fn make_seq_with(clip_specs: &[(usize, klipp_core::ClipId)]) -> klipp_timeline::Sequence {
        // 4-track sequence; `clip_specs` is a list of (track_idx,
        // clip_id) entries to populate. Each clip is a 1-second
        // gap-on-video — content doesn't matter for these tests, only
        // the (track, id) topology does.
        let mut seq = klipp_timeline::Sequence::new_default();
        // new_default has 1 video + 1 audio. Pad with extra video tracks.
        while seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video)).count() < 4 {
            seq.add_video_track(format!("V{}", seq.tracks.len()));
        }
        for &(ti, cid) in clip_specs {
            // Build a minimal Media-kind clip directly so we can
            // pin the id ourselves (append_clip allocates a fresh id).
            let one_sec = klipp_core::Timecode { value: 24, rate: 24 };
            let mut clip = klipp_timeline::Clip {
                id: cid,
                kind: klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() },
                timeline_in: klipp_core::Timecode { value: 0, rate: 24 },
                timeline_out: one_sec,
                source_in: klipp_core::Timecode { value: 0, rate: 24 },
                source_out: one_sec,
                ..klipp_timeline::Clip::gap(
                    klipp_core::Timecode { value: 0, rate: 24 },
                    one_sec,
                )
            };
            // gap() set kind to Gap; restore Media + the ids above.
            clip.id = cid;
            clip.kind = klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() };
            seq.tracks[ti].clips.push(clip);
        }
        seq
    }

    #[test]
    fn find_clip_cached_warm_path_returns_clip_and_populates_cache() {
        let cid = klipp_core::ClipId::new();
        let seq = make_seq_with(&[(2, cid)]);
        let cache = std::cell::Cell::new(None);
        let (ti, clip) = find_clip_cached(&seq, cid, &cache).expect("first lookup");
        assert_eq!(ti, 2, "clip is on track 2");
        assert_eq!(clip.id, cid);
        assert_eq!(cache.get(), Some((cid, 2)), "cache populated after first walk");
    }

    #[test]
    fn find_clip_cached_fast_path_uses_cache_when_clip_unmoved() {
        let cid = klipp_core::ClipId::new();
        let seq = make_seq_with(&[(2, cid)]);
        // Pre-seed cache with the correct location — this is what
        // a steady-state UI render sees on every frame after the
        // first.
        let cache = std::cell::Cell::new(Some((cid, 2)));
        let (ti, clip) = find_clip_cached(&seq, cid, &cache).expect("cached lookup");
        assert_eq!(ti, 2);
        assert_eq!(clip.id, cid);
        assert_eq!(cache.get(), Some((cid, 2)), "cache unchanged on fast path");
    }

    #[test]
    fn find_clip_cached_self_heals_when_clip_moves_tracks() {
        let cid = klipp_core::ClipId::new();
        // Cache says clip is on track 0, but actually it's on track 3
        // — simulates a "clip dragged to a different track between
        // frames" without an explicit invalidation.
        let seq = make_seq_with(&[(3, cid)]);
        let cache = std::cell::Cell::new(Some((cid, 0)));
        let (ti, clip) = find_clip_cached(&seq, cid, &cache).expect("self-heal");
        assert_eq!(ti, 3);
        assert_eq!(clip.id, cid);
        assert_eq!(cache.get(), Some((cid, 3)), "cache updated to new track");
    }

    #[test]
    fn find_clip_cached_clears_cache_when_clip_deleted() {
        let cid = klipp_core::ClipId::new();
        let seq = make_seq_with(&[]); // clip not in any track
        let cache = std::cell::Cell::new(Some((cid, 1))); // stale cache
        let result = find_clip_cached(&seq, cid, &cache);
        assert!(result.is_none(), "deleted clip → None");
        assert_eq!(cache.get(), None, "cache cleared on phantom id");
    }

    #[test]
    fn find_clip_cached_handles_selection_change() {
        // Cache holds clip A; we now ask about clip B (which exists
        // on a different track). The helper must walk and update.
        let a = klipp_core::ClipId::new();
        let b = klipp_core::ClipId::new();
        let seq = make_seq_with(&[(0, a), (3, b)]);
        let cache = std::cell::Cell::new(Some((a, 0)));
        let (ti, clip) = find_clip_cached(&seq, b, &cache).expect("new selection");
        assert_eq!(ti, 3);
        assert_eq!(clip.id, b);
        assert_eq!(cache.get(), Some((b, 3)), "cache updated to the new clip's location");
    }

    #[test]
    fn find_clip_cached_clears_cache_when_track_deleted_under_us() {
        // Cache holds (cid, track 5) but seq only has 4 tracks now —
        // the helper's `seq.tracks.get(cached_ti)` returns None and
        // it falls through to the full walk; if the clip isn't
        // anywhere else, cache clears. Guards the
        // tracks-shrink-without-invalidate path.
        let cid = klipp_core::ClipId::new();
        let seq = make_seq_with(&[]); // empty
        let cache = std::cell::Cell::new(Some((cid, 5))); // out-of-range track
        let result = find_clip_cached(&seq, cid, &cache);
        assert!(result.is_none());
        assert_eq!(cache.get(), None);
    }
}

// ═══════════════════════════════════════════════════════════════════
// RubberBandParam helper tests (Phase B1).
//
// The enum is the foundation that lets the audio rubber band display
// either a clip-level param (Volume/Pan/Gain) or a per-effect param.
// These tests pin the helpers used by the rubber band's resolver and
// the popup picker so a future "let me simplify the enum" change
// can't silently break the routing.
// ═══════════════════════════════════════════════════════════════════
/// #915: split at playhead leaves untargeted tracks whole.
#[cfg(test)]
mod split_targeting_tests {
    use super::*;
    use klipp_timeline::{Clip, Track, TrackKind};

    #[test]
    fn split_at_playhead_leaves_untargeted_tracks_whole() {
        let mut state = AppState::default();
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let seq = state.sequence_mut();
        seq.tracks.clear();
        for (name, kind) in [("V1", TrackKind::Video), ("A1", TrackKind::Audio)] {
            let mut t = Track::new(name, kind);
            t.clips.push(Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0)));
            seq.tracks.push(t);
        }
        state.playhead = tc(2.0);
        let cut_tracks = |state: &AppState| -> Vec<usize> {
            state.split_at_playhead_commands().iter().filter_map(|c| match c {
                klipp_core::AppCommand::BladeClip { track_index, .. } => Some(*track_index),
                _ => None,
            }).collect()
        };
        assert_eq!(cut_tracks(&state), vec![0, 1]);
        state.sequence_mut().tracks[1].targeted = false;
        assert_eq!(cut_tracks(&state), vec![0]);
    }
}

#[cfg(test)]
mod rubber_band_param_tests {
    use super::*;

    #[test]
    fn default_is_clip_volume() {
        // Rubber band starts on the clip-level Volume param so existing
        // muscle memory (open project → see volume rubber band) is
        // preserved. Changing the default in a future refactor would
        // surprise every existing project on first open.
        let rb = RubberBandParam::default();
        assert_eq!(rb, RubberBandParam::ClipParam(AudioParam::Volume));
        assert_eq!(rb.as_clip_param(), Some(AudioParam::Volume));
        assert!(!rb.is_effect());
    }

    #[test]
    fn as_clip_param_returns_none_for_effect_variant() {
        let rb = RubberBandParam::EffectParam {
            effect_index: 2,
            param_name: "threshold".to_string(),
        };
        assert_eq!(rb.as_clip_param(), None);
        assert!(rb.is_effect());
    }

    #[test]
    fn effect_key_round_trips_via_audio_effect_keyframes_format() {
        // The effect_key must match the same "{idx}:{name}" key shape
        // that `clip.audio_effect_keyframes` uses. This is what lets
        // the rubber band's resolver pull the right kf vec from the
        // map without re-implementing the format anywhere else.
        let rb = RubberBandParam::EffectParam {
            effect_index: 0,
            param_name: "threshold".to_string(),
        };
        assert_eq!(rb.effect_key().as_deref(), Some("0:threshold"));

        // Reserved key names (__bypass / __wetdry) flow through verbatim
        // so the bypass strip and the rubber band see the same key.
        let rb_bypass = RubberBandParam::EffectParam {
            effect_index: 1,
            param_name: "__bypass".to_string(),
        };
        assert_eq!(rb_bypass.effect_key().as_deref(), Some("1:__bypass"));

        let rb_wet = RubberBandParam::EffectParam {
            effect_index: 1,
            param_name: "__wetdry".to_string(),
        };
        assert_eq!(rb_wet.effect_key().as_deref(), Some("1:__wetdry"));
    }

    #[test]
    fn effect_key_returns_none_for_clip_variant() {
        let rb = RubberBandParam::ClipParam(AudioParam::Pan);
        assert!(rb.effect_key().is_none());
    }

    #[test]
    fn equality_distinguishes_effect_index_and_param_name() {
        // Pin that two EffectParams with the same name but different
        // indices are NOT equal — matters for picker state and for the
        // "is the selected param still on this clip?" check that the
        // rubber band uses to decide whether to render or fall back.
        let a = RubberBandParam::EffectParam {
            effect_index: 0,
            param_name: "threshold".to_string(),
        };
        let b = RubberBandParam::EffectParam {
            effect_index: 1,
            param_name: "threshold".to_string(),
        };
        let c = RubberBandParam::EffectParam {
            effect_index: 0,
            param_name: "ratio".to_string(),
        };
        let a_clone = a.clone();
        assert_ne!(a, b, "different indices must not match");
        assert_ne!(a, c, "different param names must not match");
        assert_eq!(a, a_clone, "clone must equal");
    }
}

#[cfg(test)]
mod per_sequence_history_tests {
    //! Behavioural tests for the per-sequence undo stack. These walk the
    //! AppState helpers directly so they're independent of the broader
    //! Klipp app machinery.
    use super::*;
    use klipp_core::Resolution;
    use klipp_timeline::Sequence;

    fn add_seq(state: &mut AppState, name: &str) -> SequenceId {
        let mut seq = Sequence::new_default();
        seq.name = name.to_string();
        let id = seq.id;
        state.sequences.push(seq);
        id
    }

    fn dummy_command() -> EditCommand {
        // SetRange has no required-borrow side effects and a small payload.
        EditCommand::SetRange {
            old_in: None,
            old_out: None,
            new_in: None,
            new_out: None,
        }
    }

    #[test]
    fn push_targets_active_sequence() {
        let mut state = AppState::default();
        // Make sure we have a known starting active sequence.
        assert!(!state.sequences.is_empty());
        let seq_a_id = state.sequences[0].id;
        let seq_b_id = add_seq(&mut state, "B");

        state.active_sequence_idx = 0;
        state.push_history(dummy_command());
        assert_eq!(state.history_for(seq_a_id).undo_count(), 1);
        assert_eq!(state.history_for(seq_b_id).undo_count(), 0);

        state.active_sequence_idx = 1;
        state.push_history(dummy_command());
        state.push_history(dummy_command());
        assert_eq!(state.history_for(seq_a_id).undo_count(), 1);
        assert_eq!(state.history_for(seq_b_id).undo_count(), 2);
    }

    #[test]
    fn switching_does_not_bleed_history() {
        let mut state = AppState::default();
        let _seq_a = state.sequences[0].id;
        let _seq_b = add_seq(&mut state, "B");
        state.active_sequence_idx = 0;
        state.push_history(dummy_command());
        // Switch to B; B's history should be empty.
        state.active_sequence_idx = 1;
        assert!(!state.active_history().can_undo(),
            "B should not see A's edits — that's the point of per-sequence history");
        // Pop on B is a no-op.
        let popped = state.active_history_mut().undo();
        assert!(popped.is_none());
    }

    #[test]
    fn ops_since_save_is_project_global() {
        let mut state = AppState::default();
        let _seq_a = state.sequences[0].id;
        let _seq_b = add_seq(&mut state, "B");
        state.active_sequence_idx = 0;
        state.push_history(dummy_command());
        state.active_sequence_idx = 1;
        state.push_history(dummy_command());
        state.push_history(dummy_command());
        assert_eq!(state.history_ops_since_save, 3, "autosave counter sums across all sequences");
        state.reset_ops_since_save();
        assert_eq!(state.history_ops_since_save, 0);
    }

    /// A step of the whole project's: tagging a file (#1986).
    fn project_command(tag: &str) -> EditCommand {
        EditCommand::SetMediaTags { media_id: klipp_core::MediaId::new(), old_tags: Vec::new(), new_tags: vec![tag.into()] }
    }

    fn seq_command(name: &'static str) -> EditCommand {
        EditCommand::Named { label: name.into(), edit: Box::new(dummy_command()) }
    }

    fn undo_label_of(state: &mut AppState) -> Option<String> {
        let label = state.undo_label();
        state.take_undo().map(|_| label.unwrap_or_default())
    }

    fn redo_label_of(state: &mut AppState) -> Option<String> {
        let label = state.redo_label();
        state.take_redo().map(|_| label.unwrap_or_default())
    }

    /// #1986: the project's steps and a sequence's interleave by when
    /// they were made: undo takes back the latest wherever it lives, and
    /// redo puts them back in the order they were taken.
    #[test]
    fn project_and_sequence_steps_undo_in_the_order_made() {
        let mut state = AppState::default();
        state.push_history(seq_command("A"));
        state.push_history(project_command("P"));
        state.push_history(seq_command("B"));
        assert!(state.project_history.can_undo() && state.active_history().undo_count() == 2, "the project's step is kept apart");
        let undone: Vec<String> = std::iter::from_fn(|| undo_label_of(&mut state)).collect();
        assert_eq!(undone, ["B", "Set Tags (project)", "A"]);
        let redone: Vec<String> = std::iter::from_fn(|| redo_label_of(&mut state)).collect();
        assert_eq!(redone, ["A", "Set Tags (project)", "B"]);
    }

    /// A project step made in one sequence is taken back, and put back,
    /// from another, past that one's older steps.
    #[test]
    fn a_project_step_is_reached_from_any_sequence() {
        let mut state = AppState::default();
        add_seq(&mut state, "B");
        state.active_sequence_idx = 1;
        state.push_history(seq_command("older in B"));
        state.active_sequence_idx = 0;
        state.push_history(project_command("P"));
        state.active_sequence_idx = 1;
        assert_eq!(state.undo_scopes().0, Some("project"));
        assert_eq!(state.undo_label().as_deref(), Some("Set Tags (project)"));
        assert_eq!(state.undo_depth(), 2);
        assert_eq!(undo_label_of(&mut state).as_deref(), Some("Set Tags (project)"));
        assert_eq!(state.undo_label().as_deref(), Some("older in B"), "B's own step is left");
        // Undone from B, redone from A.
        state.active_sequence_idx = 0;
        assert_eq!(redo_label_of(&mut state).as_deref(), Some("Set Tags (project)"));
    }

    /// Redo stays linear across both: a newer edit anywhere makes a
    /// project step stale, and a newer project step makes a sequence's
    /// stale — however the edit was recorded, a drag's end included.
    #[test]
    fn a_newer_edit_anywhere_ends_redo() {
        let mut state = AppState::default();
        add_seq(&mut state, "B");
        state.push_history(project_command("P"));
        state.take_undo();
        state.active_sequence_idx = 1;
        state.push_history(seq_command("in B"));
        state.active_sequence_idx = 0;
        assert_eq!(state.next_redo(), None, "an edit in B after P was undone: P is stale everywhere");
        assert_eq!(state.redo_depth(), 0);
        assert!(!state.project_history.can_redo(), "the push dropped the stale project step");
        // A drag's end, recorded past push_history, does the same.
        state.push_history(project_command("Q"));
        state.take_undo();
        let text = |t: &str| klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: t.into(), ..Default::default() });
        let title = |a: &str, b: &str| HistoryEntry { command: EditCommand::EditTitle { clip_id: klipp_core::ClipId::nil(), description: "Title Size", old_data: text(a), new_data: text(b) } };
        state.active_history_mut().begin_drag("size".into(), title("a", "a"));
        state.active_history_mut().end_drag(title("a", "b"));
        assert_eq!(state.next_redo(), None, "a drag ended after Q was undone");
        // And the other way: a sequence step undone, then a project step.
        state.push_history(seq_command("S"));
        state.take_undo();
        assert_eq!(state.next_redo(), Some(crate::history::Scope::Sequence));
        state.push_history(project_command("R"));
        assert_eq!(state.next_redo(), None, "the project step is newer than S's undoing");
    }

    /// A project step inside an open batch is one more edit of the batch:
    /// the batch stays one step, in the sequence's history.
    #[test]
    fn a_project_step_in_an_open_batch_stays_with_it() {
        let mut state = AppState::default();
        // An older project step, taken back: closing the batch ends it.
        state.push_history(project_command("older"));
        state.take_undo();
        assert_eq!(state.next_redo(), Some(crate::history::Scope::Project));
        state.active_history_mut().begin_batch();
        state.push_history(seq_command("A"));
        state.push_history(project_command("P"));
        state.active_history_mut().end_batch();
        assert!(!state.project_history.can_undo());
        assert_eq!(state.active_history().undo_count(), 1);
        assert!(state.active_history().last_edit_stamp() > state.project_history.last_edit_stamp(), "the batch, project edit and all, is the sequence's last edit");
        assert_eq!(state.next_redo(), None, "closing the batch ended the older project redo");
        assert_eq!(state.redo_depth(), 0);
        state.push_history(seq_command("next"));
        assert!(!state.project_history.can_redo(), "and the stale step is dropped, not kept");
    }

    /// Typing on either side of a project step is two steps, not one
    /// folded across it.
    #[test]
    fn a_project_step_ends_a_typing_burst() {
        let mut state = AppState::default();
        let clip = klipp_core::ClipId::new();
        let text = |t: &str| klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: t.into(), ..Default::default() });
        state.push_history(EditCommand::EditTitle { clip_id: clip, description: "Edit Title Text", old_data: text("a"), new_data: text("ab") });
        state.active_history_mut().note_title_text_edit(clip);
        state.push_history(project_command("P"));
        assert!(!state.active_history_mut().try_coalesce_title_text(clip, &text("ab"), &text("abc")), "the burst ended at the project step");
    }

    /// With the stacks full, the oldest steps go and the order holds.
    #[test]
    fn evicted_steps_leave_the_order_whole() {
        let mut state = AppState::default();
        state.set_history_max_size(2);
        for n in ["A", "B", "C"] {
            state.push_history(seq_command(n));
        }
        state.push_history(project_command("P"));
        state.push_history(seq_command("D"));
        let undone: Vec<String> = std::iter::from_fn(|| undo_label_of(&mut state)).collect();
        assert_eq!(undone, ["D", "Set Tags (project)", "C"], "B and A went with the size");
    }

    #[test]
    fn set_history_max_size_propagates_to_all_existing() {
        let mut state = AppState::default();
        let _seq_a = state.sequences[0].id;
        let seq_b = add_seq(&mut state, "B");
        state.active_sequence_idx = 0;
        state.push_history(dummy_command()); // seeds A's history
        state.active_sequence_idx = 1;
        state.push_history(dummy_command()); // seeds B's history

        state.set_history_max_size(50);
        // We can't observe max_size directly, but we can fill A past 50
        // and confirm the cap kicks in.
        state.active_sequence_idx = 0;
        for _ in 0..120 {
            state.push_history(dummy_command());
        }
        assert!(state.active_history().undo_count() <= 50);
        // B unaffected by activity on A.
        assert_eq!(state.history_for(seq_b).undo_count(), 1);
    }

    #[test]
    fn abandon_pending_drag_drops_envelope_without_pushing() {
        let mut state = AppState {
            active_sequence_idx: 0,
            project_epoch: 0,
            ..Default::default()
        };
        let h = state.active_history_mut();
        h.begin_drag(
            "test_key".into(),
            HistoryEntry { command: EditCommand::SetRange {
                old_in: None, old_out: None, new_in: None, new_out: None,
            } },
        );
        assert!(h.is_dragging("test_key"));
        h.abandon_pending_drag();
        assert!(!h.is_dragging("test_key"));
        assert_eq!(h.undo_count(), 0, "abandon must NOT push the pre-drag snapshot");
    }

    #[test]
    fn lazy_history_seed_for_unseen_sequence() {
        let mut state = AppState::default();
        let new_id = SequenceId::new();
        // Reading a never-pushed sequence's history returns empty.
        assert_eq!(state.history_for(new_id).undo_count(), 0);
        // Writing creates the entry on demand.
        let _ = state.history_for_mut(new_id);
        assert!(state.command_histories.contains_key(&new_id));
    }

    /// Avoid breaking the legacy default-resolution check — included
    /// here so adding a new test doesn't require touching unrelated
    /// expectations.
    #[test]
    fn new_default_seq_has_known_shape() {
        let seq = Sequence::new_default();
        assert_eq!(seq.resolution, Resolution::hd_1080());
    }
}

#[cfg(test)]
mod grade_node_clipboard_tests {
    use super::GradeNodeClipboard;
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodePos};
    use std::collections::HashSet;

    fn make_graph_with_corrector_and_lift() -> (NodeGraph, NodeId, NodeId, NodeId) {
        // Build a tiny graph: Source → Corrector → Output, plus an
        // unrelated "Wheels" node sitting on the side.
        let source_id = NodeId::new_v4();
        let corrector_id = NodeId::new_v4();
        let output_id = NodeId::new_v4();
        let wheels_id = NodeId::new_v4();
        let graph = NodeGraph {
            nodes: vec![
                Node { id: source_id, kind: NodeKind::Source, label: "Src".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() },
                Node { id: corrector_id, kind: NodeKind::Corrector, label: "Corr".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() },
                Node { id: output_id, kind: NodeKind::Output, label: "Out".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() },
                Node { id: wheels_id, kind: NodeKind::ColorWheels, label: "Wheels".into(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() },
            ],
            edges: vec![
                Edge { from_node: source_id, from_port: 0, to_node: corrector_id, to_port: 0 },
                Edge { from_node: corrector_id, from_port: 0, to_node: output_id, to_port: 0 },
                // Half-attached: from wheels (outside) to corrector (inside)
                // — should be dropped on copy when only Corrector is selected.
                Edge { from_node: wheels_id, from_port: 0, to_node: corrector_id, to_port: 1 },
            ],
            output_node_id: output_id,
            node_positions: [
                (source_id, NodePos { x: 0.0, y: 0.0 }),
                (corrector_id, NodePos { x: 100.0, y: 50.0 }),
                (output_id, NodePos { x: 200.0, y: 0.0 }),
                (wheels_id, NodePos { x: 100.0, y: 200.0 }),
            ].into_iter().collect(),
            working_space_override: None,
            frames: Vec::new(),
            sticky_notes: Vec::new(),
        };
        (graph, source_id, corrector_id, output_id)
    }

    #[test]
    fn copy_includes_only_selected_nodes() {
        let (graph, _src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        assert_eq!(cb.nodes.len(), 1);
        assert_eq!(cb.nodes[0].id, corr);
    }

    #[test]
    fn copy_drops_edges_with_endpoint_outside_selection() {
        let (graph, _src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        // All three edges either start or end outside Corrector,
        // so none should make it into the clipboard.
        assert_eq!(cb.edges.len(), 0);
    }

    #[test]
    fn copy_preserves_edges_when_both_endpoints_selected() {
        let (graph, src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(src);
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        assert_eq!(cb.nodes.len(), 2);
        // Source → Corrector edge is in; Corrector → Output is out (output not selected).
        assert_eq!(cb.edges.len(), 1);
        assert_eq!(cb.edges[0].from_node, src);
        assert_eq!(cb.edges[0].to_node, corr);
    }

    #[test]
    fn copy_filters_node_positions_to_selection() {
        let (graph, _src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        assert_eq!(cb.node_positions.len(), 1);
        assert!(cb.node_positions.contains_key(&corr));
    }

    #[test]
    fn paste_assigns_fresh_ids_to_nodes() {
        let (mut graph, _src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        let n_before = graph.nodes.len();
        let count = cb.merge_into(&mut graph, (10.0, 10.0));
        assert_eq!(count, 1);
        assert_eq!(graph.nodes.len(), n_before + 1);
        // The new node must NOT share an id with the original Corrector.
        let pasted = graph.nodes.last().unwrap();
        assert_ne!(pasted.id, corr);
        // Same kind / label / enabled state.
        assert!(matches!(pasted.kind, NodeKind::Corrector));
    }

    #[test]
    fn paste_remaps_edges_via_id_table() {
        let (mut graph, src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(src);
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        let edges_before = graph.edges.len();
        cb.merge_into(&mut graph, (0.0, 0.0));
        assert_eq!(graph.edges.len(), edges_before + 1);
        // The pasted edge should reference the two NEW node ids, not
        // the originals — find the new edge by exclusion.
        let new_edge = graph.edges.last().unwrap();
        assert_ne!(new_edge.from_node, src);
        assert_ne!(new_edge.to_node, corr);
    }

    #[test]
    fn paste_offset_shifts_positions() {
        let (mut graph, _src, corr, _out) = make_graph_with_corrector_and_lift();
        let mut sel = HashSet::new();
        sel.insert(corr);
        let cb = GradeNodeClipboard::from_selection(&graph, &sel);
        let pasted_id = cb.nodes[0].id;
        cb.merge_into(&mut graph, (40.0, 40.0));
        // The clipboard's source position was (100.0, 50.0) — the
        // pasted entry uses the NEW id, so look for the offset
        // values rather than the original key.
        let offset_pos = graph.node_positions.values()
            .find(|p| (p.x - 140.0).abs() < 0.01 && (p.y - 90.0).abs() < 0.01);
        assert!(offset_pos.is_some(), "no position with paste offset found");
        // Original id no longer maps to its old position because we
        // didn't touch the source — it should still be at (100, 50).
        let _ = pasted_id; // silence unused — we asserted via value match
    }

    #[test]
    fn paste_skips_dangling_edges_defensively() {
        // Construct a clipboard that — by misuse — references a node
        // id NOT in its node set. The merge should silently skip it
        // rather than panicking.
        let mut graph = NodeGraph::default();
        let stray_id = NodeId::new_v4();
        let cb = GradeNodeClipboard {
            nodes: Vec::new(),
            edges: vec![Edge { from_node: stray_id, from_port: 0, to_node: stray_id, to_port: 0 }],
            node_positions: std::collections::HashMap::new(),
        };
        let edges_before = graph.edges.len();
        cb.merge_into(&mut graph, (0.0, 0.0));
        assert_eq!(graph.edges.len(), edges_before);
    }
}

#[cfg(test)]
mod focus_search_request_tests {
    use super::PanelKind;

    /// AppState's Default initialiser leaves the request unset so a
    /// fresh app can't have a stale Ctrl+F focus pending.
    #[test]
    fn default_is_none() {
        let s = super::AppState::default();
        assert!(s.focus_search_request.is_none());
    }

    /// Round-trip: dispatcher copies active_panel into the request,
    /// the consumer reads it, clears it, and the next read sees None.
    #[test]
    fn round_trip_set_then_clear() {
        let mut s = super::AppState {
            active_panel: Some(PanelKind::Browser),
            ..Default::default()
        };

        // Dispatcher path
        s.focus_search_request = s.active_panel;
        assert_eq!(s.focus_search_request, Some(PanelKind::Browser));

        // Consumer path (matches the panel — clears)
        if s.focus_search_request == Some(PanelKind::Browser) {
            s.focus_search_request = None;
        }
        assert!(s.focus_search_request.is_none());
    }

    /// Consumer in a different panel should NOT clear someone else's
    /// request — the watchdog clears it at end-of-update instead.
    #[test]
    fn wrong_panel_does_not_consume() {
        let mut s = super::AppState {
            focus_search_request: Some(PanelKind::Markers),
            ..Default::default()
        };

        // Browser's MediaBrowser consumer runs but the request is
        // for Markers — must leave the flag intact.
        if s.focus_search_request == Some(PanelKind::Browser) {
            s.focus_search_request = None;
        }
        assert_eq!(s.focus_search_request, Some(PanelKind::Markers));
    }

    /// When active_panel is None (no panel claimed focus yet),
    /// dispatching Ctrl+F leaves request as None — silent no-op.
    #[test]
    fn no_active_panel_yields_no_request() {
        let mut s = super::AppState::default();
        s.active_panel = None;
        s.focus_search_request = s.active_panel;
        assert!(s.focus_search_request.is_none());
    }
}

#[cfg(test)]
mod split_at_playhead_tests {
    use super::AppState;
    use klipp_core::{AppCommand, MediaId, RationalTime};
    use klipp_timeline::magnetic::MagneticEditor;
    use klipp_timeline::TrackKind;

    fn t(secs: i64) -> RationalTime { RationalTime { value: secs * 30, rate: 30 } }

    /// Issue #250: Ctrl+B on a clip with linked audio queued a blade for
    /// both clips. The first blade already cut the partner, so the
    /// second landed exactly on the partner's new out edge and aborted.
    #[test]
    fn a_linked_pair_is_bladed_once_and_the_stale_partner_blade_is_a_noop() {
        let mut state = AppState::default();
        let media = MediaId::new();
        let seq = state.sequence_mut();
        let vi = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        let ai = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio).unwrap();
        let v = seq.tracks[vi].append_clip(media, t(10));
        let a = seq.tracks[ai].append_clip(media, t(10));
        seq.tracks[vi].find_clip_mut(v).unwrap().linked_clip_id = Some(a);
        seq.tracks[ai].find_clip_mut(a).unwrap().linked_clip_id = Some(v);
        state.playhead = t(4);

        let cmds = state.split_at_playhead_commands();
        assert_eq!(cmds.len(), 1, "the linked partner was queued for a second blade");
        assert!(matches!(cmds[0], AppCommand::BladeClip { clip_id, .. } if clip_id == v));

        // What the handler does for the one command: the clip, then its
        // partner. A stale blade of the partner's original id afterwards
        // must change nothing rather than panic.
        let seq = state.sequence_mut();
        MagneticEditor::blade(seq, v, t(4)).unwrap();
        MagneticEditor::blade(seq, a, t(4)).unwrap();
        assert!(MagneticEditor::blade(seq, a, t(4)).is_none());
        assert_eq!(seq.tracks[ai].clips.len(), 2);
    }

    #[test]
    fn unlinked_clips_under_the_playhead_each_get_a_blade() {
        let mut state = AppState::default();
        let seq = state.sequence_mut();
        let vi = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        let ai = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio).unwrap();
        seq.tracks[vi].append_clip(MediaId::new(), t(10));
        seq.tracks[ai].append_clip(MediaId::new(), t(10));
        state.playhead = t(4);
        assert_eq!(state.split_at_playhead_commands().len(), 2);

        // On a clip's first frame there is nothing to cut, nor within a
        // frame of it: that is the blade's edge rule.
        state.playhead = t(0);
        assert!(state.split_at_playhead_commands().is_empty());
        state.playhead = RationalTime { value: 1, rate: 30 };
        assert!(state.split_at_playhead_commands().is_empty());
    }

    /// #407: a locked track is not split. A pair across a lock is queued
    /// from its unlocked side, so the blade refuses it whole and says
    /// which track to unlock, rather than cutting one side.
    #[test]
    fn a_locked_track_is_left_alone_and_a_pair_across_a_lock_is_queued_once() {
        let mut state = AppState::default();
        let seq = state.sequence_mut();
        let vi = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        let ai = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio).unwrap();
        seq.tracks[vi].append_clip(MediaId::new(), t(10));
        seq.tracks[ai].append_clip(MediaId::new(), t(10));
        seq.tracks[vi].locked = true;
        state.playhead = t(4);
        let cmds = state.split_at_playhead_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], AppCommand::BladeClip { track_index, .. } if track_index == ai));

        let seq = state.sequence_mut();
        let v = seq.tracks[vi].clips[0].id;
        let a = seq.tracks[ai].clips[0].id;
        seq.tracks[vi].find_clip_mut(v).unwrap().linked_clip_id = Some(a);
        seq.tracks[ai].find_clip_mut(a).unwrap().linked_clip_id = Some(v);
        let cmds = state.split_at_playhead_commands();
        assert_eq!(cmds.len(), 1, "the pair is queued once, from the unlocked side");
        assert!(matches!(cmds[0], AppCommand::BladeClip { clip_id, .. } if clip_id == a));
        let seq = state.sequence_mut();
        assert_eq!(
            klipp_document::blade::blade_linked(seq, ai, a, t(4), |_| true).err(),
            Some(klipp_document::blade::BladeRefusal::LockedAcrossPair { locked: seq.tracks[vi].name.clone(), unlocked: seq.tracks[ai].name.clone() })
        );
        assert_eq!(seq.tracks[ai].clips.len(), 1, "nothing was cut");
    }
}

#[cfg(test)]
mod paste_speed_tests {
    use super::{AttributesClipboard, PasteChoice};
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::test_clip::TestClip;

    /// Pasting speed brings the ramp, not only the rate it started at.
    #[test]
    fn pasting_speed_brings_the_ramp() {
        let tc = |f| RationalTime::from_frames(f, 24);
        let mut source = klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        source.speed = 0.5;
        source.speed_keyframes = vec![
            klipp_core::Keyframe { time_secs: 0.0, value: 1.0, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
            klipp_core::Keyframe { time_secs: 1.0, value: 0.25, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
        ];
        let copied = AttributesClipboard::of(&source);
        let mut target = klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        copied.paste_onto(&mut target, PasteChoice { effects: false, color_grade: false, audio: false, transform: false, speed: true });
        assert_eq!(target.speed, 0.5);
        assert_eq!(target.speed_keyframes, source.speed_keyframes);
    }

    /// An effect's modifiers paste with the effects, and a clip's own
    /// modifiers with the transform, each without touching the other.
    #[test]
    fn pasting_effects_brings_their_modifiers() {
        use klipp_core::{ModifierStack, ParamId};
        let tc = |f| RationalTime::from_frames(f, 24);
        let wiggle = ParamId::Effect { effect_index: 0, param_name: "amount".into() };
        let mut source = TestClip::of(klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48))).effect(klipp_effects::BuiltinEffect::Vibrance { amount: 0.3 }).build();
        // A real stack each: the picture keeps no empty one.
        let stack = || ModifierStack { modifiers: vec![klipp_core::Modifier::Oscillate { amplitude: 1.0, frequency: 1.0, phase: 0.0, waveform: Default::default(), blend: Default::default(), enabled: true }] };
        source.edit_param_modifiers(|v| v.insert(wiggle.clone(), stack()));
        source.edit_param_modifiers(|v| v.insert(ParamId::Scale, stack()));
        let copied = AttributesClipboard::of(&source);
        let none = PasteChoice { effects: false, color_grade: false, audio: false, transform: false, speed: false };

        let mut target = klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        target.edit_param_modifiers(|v| v.insert(ParamId::Opacity, ModifierStack::default()));
        copied.paste_onto(&mut target, PasteChoice { effects: true, ..none });
        assert!(target.param_modifiers().contains_key(&wiggle), "the effect's modifier was lost");
        assert!(target.param_modifiers().contains_key(&ParamId::Opacity), "the clip's own modifier was replaced");
        assert!(!target.param_modifiers().contains_key(&ParamId::Scale), "a transform modifier came with the effects");

        let mut target = klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        copied.paste_onto(&mut target, PasteChoice { transform: true, ..none });
        assert!(target.param_modifiers().contains_key(&ParamId::Scale));
        assert!(!target.param_modifiers().contains_key(&wiggle), "an effect modifier came without its effect");
    }

    /// A paste replaces the target's effects with the source's as its
    /// picture holds them (M4 step 12c-5): each effect with its flag and
    /// its keys; and the transform paste puts the source's lanes in place
    /// of the target's.
    #[test]
    fn pasting_effects_brings_their_keys_and_flags_through_the_picture() {
        use klipp_timeline::test_clip;
        let tc = |f| RationalTime::from_frames(f, 24);
        let clip = || klipp_timeline::Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        let source = TestClip::of(clip())
            .effect(klipp_effects::BuiltinEffect::Vibrance { amount: 0.3 })
            .key(0, "amount", 0.0, 0.1)
            .key(0, "amount", 1.0, 0.9)
            .effect(klipp_effects::BuiltinEffect::Gamma { gamma: 1.2 })
            .off()
            .lane("pos_x", 0.0, 10.0)
            .build();
        let copied = AttributesClipboard::of(&source);
        let none = PasteChoice { effects: false, color_grade: false, audio: false, transform: false, speed: false };

        let mut target = TestClip::of(clip()).effect(klipp_effects::BuiltinEffect::Invert { mix: 1.0 }).lane("pos_x", 0.5, 99.0).build();
        copied.paste_onto(&mut target, PasteChoice { effects: true, ..none });
        assert_eq!(test_clip::effects(&target), test_clip::effects(&source), "the target's effects are replaced by the source's");
        assert_eq!(test_clip::enabled(&target), vec![true, false], "each with its flag");
        let pasted = crate::state::CopiedEffect::of(&target);
        let keys = |e: &crate::state::CopiedEffect| e.anims.values().map(|a| a.keys.len()).sum::<usize>();
        assert_eq!(keys(&pasted[0]), 2, "the effect's keys came with it");

        copied.paste_onto(&mut target, PasteChoice { transform: true, ..none });
        let lane = crate::state::placement_anims(&target);
        let pos_x = lane.get("pos_x").expect("the source's lane");
        assert_eq!(pos_x.keys.len(), 1, "the source's lane replaced the target's: {pos_x:?}");
    }
}

#[cfg(test)]
mod sequence_summaries_tests {
    use super::AppState;

    #[test]
    fn every_sequence_is_listed_with_its_clips_and_the_open_one() {
        let mut state = AppState::default();
        let mut second = state.sequences[0].clone();
        second.id = klipp_core::SequenceId::new();
        second.name = "Social cut".into();
        state.sequences.push(second);
        state.active_sequence_idx = 1;
        let (list, active) = state.sequence_summaries();
        assert_eq!(list.len(), 2);
        assert_eq!((list[1].name.as_str(), list[1].id.clone()), ("Social cut", state.sequences[1].id.to_string()));
        assert_eq!(list[0].clips, state.sequences[0].tracks.iter().map(|t| t.clips.len()).sum::<usize>());
        assert_eq!(active, 1);
    }
}

#[cfg(test)]
mod log_format_undo_tests {
    use super::*;

    /// Picking a file's camera encoding sets the file and every clip of it,
    /// in every sequence, as one step, and one undo takes it all back
    /// (#1396).
    #[test]
    fn a_camera_encoding_pick_is_one_step_undone_whole() {
        let mut state = AppState::default();
        let media = klipp_core::MediaId::new();
        let fps = state.sequences[0].fps;
        let item = klipp_media::MediaItem::new_synthetic_title(media, "shot".into(), klipp_core::RationalTime::from_secs_f64(2.0, 24), fps);
        state.media_pool.add(item);
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = || klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        state.sequences.push(state.sequences[0].clone());
        let (a, b) = (clip(), clip());
        let (a_id, b_id) = (a.id, b.id);
        state.sequences[0].tracks[0].clips.push(a);
        state.sequences[1].tracks[0].clips.push(b);
        let picture = |s: &AppState, id| serde_json::to_value(s.sequences.iter().find_map(|q| q.all_clips().find(|c| c.id == id)).unwrap().grade()).unwrap();
        let format = |s: &AppState| s.media_pool.get(&media).map(|i| (i.log_format, i.log_format_user_set)).unwrap();
        let before = (picture(&state, a_id), picture(&state, b_id), format(&state));
        assert_eq!(state.set_clip_log_format(a_id, klipp_core::LogFormat::SLog3), Some(media));
        assert_eq!(format(&state), (klipp_core::LogFormat::SLog3, true));
        assert_ne!(picture(&state, b_id), before.1, "the other sequence's clip of the file too");
        assert_eq!((state.undo_depth(), state.undo_scopes().0), (1, Some("project")), "one step, the project's (#1986)");
        let after = (picture(&state, a_id), picture(&state, b_id), format(&state));
        let entry = state.take_undo().unwrap();
        let active = state.active_sequence_idx;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps };
        doc.apply_inverse(&entry.command);
        assert_eq!((picture(&state, a_id), picture(&state, b_id), format(&state)), before, "undo takes it all back");
        let entry = state.take_redo().unwrap();
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps };
        doc.apply_forward(&entry.command);
        assert_eq!((picture(&state, a_id), picture(&state, b_id), format(&state)), after, "redo puts it back");
    }

    /// Undoing (and redoing) a camera encoding has the file's pictures made
    /// again, so the thumbnail and filmstrip shown are the restored
    /// encoding's, not the undone one's: the step names the file, and the
    /// pictures made for the other encoding are forgotten, as the app's
    /// undo forgets them before it asks for new ones (#1396).
    #[test]
    fn undoing_a_camera_encoding_remakes_the_files_pictures() {
        let mut state = AppState::default();
        let media = klipp_core::MediaId::new();
        let fps = state.sequences[0].fps;
        state.media_pool.add(klipp_media::MediaItem::new_synthetic_title(media, "shot".into(), klipp_core::RationalTime::from_secs_f64(2.0, 24), fps));
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let id = clip.id;
        state.sequences[0].tracks[0].clips.push(clip);
        state.set_clip_log_format(id, klipp_core::LogFormat::SLog3);
        let ctx = egui::Context::default();
        let texture = |name: &str| ctx.load_texture(name, egui::ColorImage::new([1, 1], egui::Color32::BLACK), Default::default());
        let active = state.active_sequence_idx;
        for undo in [true, false] {
            // The pictures made for the encoding the step leaves.
            state.thumbnail_textures.insert(media, texture("thumbnail"));
            state.filmstrip_textures.insert(media, vec![(0.0, texture("filmstrip"))]);
            state.thumbnail_cache.set_filmstrip(media, vec![klipp_media::FilmstripFrame { time_secs: 0.0, rgba: vec![0; 4], width: 1, height: 1 }]);
            state.filmstrip_in_progress.insert(media);
            let effects = if undo {
                let entry = state.take_undo().unwrap();
                klipp_document::apply::DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_inverse(&entry.command)
            } else {
                let entry = state.take_redo().unwrap();
                klipp_document::apply::DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(&entry.command)
            };
            let remake = state.pictures_to_remake(&effects);
            assert_eq!(remake, vec![media], "undo {undo}: the file's pictures are made again");
            let dropped: usize = remake.into_iter().map(|m| state.forget_media_pictures(m).len()).sum();
            assert_eq!(dropped, 2, "undo {undo}: its thumbnail and filmstrip textures let go");
            assert!(!state.thumbnail_textures.contains_key(&media) && !state.filmstrip_textures.contains_key(&media), "undo {undo}");
            assert!(state.thumbnail_cache.filmstrip(&media).is_some_and(|f| f.is_empty()), "undo {undo}: the old filmstrip is not shown");
            assert!(!state.filmstrip_in_progress.contains(&media), "undo {undo}: a new filmstrip may start");
        }
    }
}

#[cfg(test)]
mod keep_analyses_tests {
    use super::*;
    use klipp_analysis::assets::{store, AssetKey, AssetKind, CacheStore};

    fn key(media: &str) -> AssetKey {
        AssetKey { kind: AssetKind::Deflicker, media: media.into(), width: 480, height: 240, fps: (25, 1) }
    }

    fn luma(n: usize) -> serde_json::Value {
        serde_json::to_value(klipp_effects::deflicker::DeflickerData { luma: vec![0.5; n], fps: 25.0 }).unwrap()
    }

    /// What's loaded follows the cache: read when ready, read again when
    /// analysed again, let go when the asset is gone or no clip uses it;
    /// one put there by anything else is left alone.
    #[test]
    fn the_analyses_follow_the_cache() {
        let root = std::env::temp_dir().join(format!("klipp-keep-analyses-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut cache = CacheStore::at(root.clone());
        let mut state = AppState::default();
        let (a, b) = (key("a"), key("b"));
        let lumas = |state: &AppState, k: &AssetKey| state.analyses.get(k).and_then(|v| v.get("luma")).and_then(|l| l.as_array()).map(|l| l.len());

        // Not there yet: owed back, nothing held.
        assert_eq!(state.keep_analyses(&cache, &[(a.clone(), true)]), vec![a.clone()]);
        assert!(state.analyses.is_empty());

        // Ready: read.
        store(&mut cache, a.clone(), luma(10)).unwrap();
        assert!(state.keep_analyses(&cache, &[(a.clone(), true)]).is_empty());
        assert_eq!(lumas(&state, &a), Some(10));

        // Analysed again (another length, so its stamp moves): read again.
        store(&mut cache, a.clone(), luma(20)).unwrap();
        state.keep_analyses(&cache, &[(a.clone(), true)]);
        assert_eq!(lumas(&state, &a), Some(20), "the new analysis");

        // Gone from the cache: let go, and owed again.
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(state.keep_analyses(&cache, &[(a.clone(), true)]), vec![a.clone()]);
        assert!(!state.analyses.contains_key(&a) && !state.analysis_stamps.contains_key(&a));

        // No clip uses it: let go. One put there by anything else stays.
        store(&mut cache, a.clone(), luma(10)).unwrap();
        state.keep_analyses(&cache, &[(a.clone(), true)]);
        state.analyses.insert(b.clone(), std::sync::Arc::new(luma(5)));
        state.keep_analyses(&cache, &[]);
        assert!(!state.analyses.contains_key(&a), "unused: let go");
        assert_eq!(lumas(&state, &b), Some(5), "not loaded here: kept");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Playback's delivery gain, from the engine (#1565).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeliveryLoudness {
    /// The loudness playback is heard at; `None` for unity.
    pub target_lufs: Option<f32>,
    /// The gain playback adds, once measured.
    pub gain_db: Option<f32>,
    /// The export's own gain, larger than playback's when it limits.
    pub export_gain_db: Option<f32>,
    /// The mix is being measured.
    pub measuring: bool,
    /// The target last sent to the engine, so it is sent on change only.
    pub sent: Option<Option<(f32, f32)>>,
    /// The export or the brief sets a loudness, so there is a delivery
    /// to hear, whether or not playback is heard at it.
    pub available: bool,
    /// The preference: playback heard at the delivery's loudness.
    pub enabled: bool,
}

impl DeliveryLoudness {
    /// The line beside the master meter, `None` with no target:
    /// "Delivery +4.1 dB → -14 LUFS", with "(limited in export)" when the
    /// export's limiter takes the peaks playback holds under.
    pub fn label(&self) -> Option<String> {
        if self.available && !self.enabled {
            return Some("Delivery loudness off".into());
        }
        let target = self.target_lufs?;
        Some(match (self.gain_db, self.measuring) {
            (_, true) => format!("Delivery \u{2192} {target:.0} LUFS, measuring\u{2026}"),
            (Some(g), false) => {
                let limited = self.export_gain_db.is_some_and(|e| e - g > 0.05);
                format!("Delivery {g:+.1} dB \u{2192} {target:.0} LUFS{}", if limited { " (limited in export)" } else { "" })
            }
            (None, false) => format!("Delivery \u{2192} {target:.0} LUFS, not measured"),
        })
    }
}

#[cfg(test)]
mod delivery_loudness_tests {
    use super::DeliveryLoudness;

    /// #1565: the label says the gain and the target, that the export
    /// limits where playback is held under it, and nothing with no target.
    #[test]
    fn the_label_says_the_delivery() {
        assert_eq!(DeliveryLoudness::default().label(), None);
        let d = DeliveryLoudness { target_lufs: Some(-14.0), gain_db: Some(4.1), export_gain_db: Some(4.1), ..Default::default() };
        assert_eq!(d.label().unwrap(), "Delivery +4.1 dB \u{2192} -14 LUFS");
        let d = DeliveryLoudness { export_gain_db: Some(6.0), ..d };
        assert_eq!(d.label().unwrap(), "Delivery +4.1 dB \u{2192} -14 LUFS (limited in export)");
        let d = DeliveryLoudness { measuring: true, ..d };
        assert!(d.label().unwrap().contains("measuring"));
        let off = DeliveryLoudness { available: true, enabled: false, ..Default::default() };
        assert_eq!(off.label().unwrap(), "Delivery loudness off");
    }
}

/// A subject being tracked through a clip (#239).
#[derive(Debug, Clone)]
pub struct Tracking {
    pub media: klipp_core::MediaId,
    pub folder: std::path::PathBuf,
    /// The frames the render could see when last looked, in order.
    pub seen: Vec<i64>,
    pub started: std::time::Instant,
    /// The media's file name, for the status bar and the toast.
    pub name: String,
}

#[cfg(test)]
mod pool_import_undo_tests {
    use super::*;

    fn replay(state: &mut AppState, redo: bool) {
        let entry = if redo { state.take_redo() } else { state.take_undo() }.expect("history step");
        let mut doc = klipp_document::apply::DocumentView {
            sequences: &mut state.sequences, active: state.active_sequence_idx,
            media_pool: &mut state.media_pool, fps: &mut state.fps,
        };
        let effects = if redo { doc.apply_forward(&entry.command) } else { doc.apply_inverse(&entry.command) };
        assert!(effects.sync_media_pool);
    }

    #[test]
    fn completed_pool_import_undo_redo_preserves_file_and_metadata() {
        let path = std::env::temp_dir().join(format!("klipp-import-undo-{}.wav", klipp_core::MediaId::new()));
        std::fs::write(&path, b"source bytes").unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
        item.tags = vec!["original".into()];
        let id = item.id;
        let mut state = AppState::default();
        let revision = std::sync::Arc::new(klipp_core::ControlShared::default());
        state.active_history_mut().set_revision_shared(revision.clone());
        state.pending_undo_imports.insert(id);
        assert_eq!(state.undo_depth(), 0, "queuing a probe is not a completed edit");
        state.complete_pool_import(item, true);
        assert_eq!(state.undo_depth(), 1);
        assert!(revision.revision.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert!(!state.pending_undo_imports.contains(&id));
        replay(&mut state, false);
        assert!(state.media_pool.get(&id).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"source bytes", "undo never deletes or changes source media");
        replay(&mut state, true);
        assert_eq!(state.media_pool.get(&id).unwrap().tags, ["original"]);
        assert_eq!(state.media_pool.get(&id).unwrap().path, path);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn asynchronous_batch_imports_complete_as_individual_steps() {
        let mut state = AppState::default();
        let a = klipp_media::MediaItem::synthetic_for_test("first.wav".into(), None);
        let b = klipp_media::MediaItem::synthetic_for_test("second.wav".into(), None);
        let (a_id, b_id) = (a.id, b.id);
        state.pending_undo_imports.extend([a_id, b_id]);
        // The engine may complete a later request first.
        state.complete_pool_import(b, true);
        state.complete_pool_import(a, true);
        assert_eq!(state.undo_depth(), 2);
        replay(&mut state, false);
        assert!(state.media_pool.get(&a_id).is_none());
        assert!(state.media_pool.get(&b_id).is_some());
        replay(&mut state, false);
        assert!(state.media_pool.get(&b_id).is_none());
        replay(&mut state, true);
        replay(&mut state, true);
        assert!(state.media_pool.get(&a_id).is_some() && state.media_pool.get(&b_id).is_some());
    }

    #[test]
    fn reprobes_and_timeline_drops_do_not_gain_import_steps() {
        let mut state = AppState::default();
        let item = klipp_media::MediaItem::synthetic_for_test("drop.wav".into(), None);
        let id = item.id;
        state.complete_pool_import(item.clone(), true);
        assert_eq!(state.undo_depth(), 0, "timeline placement owns its history");
        state.pending_undo_imports.insert(id);
        state.complete_pool_import(item, false);
        assert_eq!(state.undo_depth(), 0, "a re-probe must not undo an existing item");
        assert!(state.pending_undo_imports.is_empty());
    }
}

#[cfg(test)]
mod switch_sequence_tests {
    use super::*;

    /// A tab clears the selection and the compound path; a stacked
    /// timeline's focus keeps both, and either keeps each sequence's
    /// playhead on it (#82).
    #[test]
    fn a_focus_keeps_the_view_a_tab_clears() {
        let mut state = AppState::default();
        state.sequences.push(klipp_timeline::Sequence::new_default());
        let clip = ClipId::new();
        let at = |s: f64| Timecode::from_secs_f64(s, 24);
        for keep in [false, true] {
            state.active_sequence_idx = 0;
            state.playhead = at(3.0);
            state.selected_clip = Some(clip);
            state.compound_clip_stack = vec![(0, at(1.0))];
            assert!(state.switch_sequence(1, keep));
            assert_eq!(state.active_sequence_idx, 1);
            assert_eq!(state.sequences[0].last_playhead, Some(at(3.0)));
            assert_eq!((state.selected_clip.is_some(), state.compound_clip_stack.is_empty()), (keep, !keep), "keep_view {keep}");
            state.sequences[1].last_playhead = None;
        }
        assert!(!state.switch_sequence(1, false), "already shown");
    }
}
