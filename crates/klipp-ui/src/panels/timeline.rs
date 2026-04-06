use crate::icons::Icon;
use crate::state::{AppState, PlayheadFollow};
use klipp_core::{AppCommand, ClipId, RationalTime, Timecode, Tool};
use klipp_engine::PlaybackState;
use klipp_timeline::TrackKind;
use std::collections::HashMap;

const TRACK_HEADER_MIN_WIDTH: f32 = 80.0;
const TRACK_HEADER_MAX_WIDTH: f32 = 320.0;
const RULER_HEIGHT: f32 = 24.0;
// Default track heights — overridable via prefs.timeline (synced to state.timeline_prefs)
pub(crate) const VIDEO_TRACK_HEIGHT: f32 = 50.0;
pub(crate) const AUDIO_TRACK_HEIGHT: f32 = 48.0;
pub(crate) const TEXT_TRACK_HEIGHT: f32 = 32.0;
const MIN_TRACK_HEIGHT: f32 = 32.0;
const MAX_TRACK_HEIGHT: f32 = 300.0;

/// Get effective track height, respecting custom_height override and prefs.
pub(crate) fn effective_track_height(track: &klipp_timeline::Track) -> f32 {
    track.custom_height.unwrap_or(match track.kind {
        TrackKind::Audio => AUDIO_TRACK_HEIGHT,
        TrackKind::Text | TrackKind::Subtitle => TEXT_TRACK_HEIGHT,
        _ => VIDEO_TRACK_HEIGHT,
    })
}

/// Get effective track height using preferences for defaults.
#[allow(dead_code)]
fn effective_track_height_with_prefs(track: &klipp_timeline::Track, tl: &crate::prefs::TimelinePrefs) -> f32 {
    track.custom_height.unwrap_or(match track.kind {
        TrackKind::Audio => tl.min_track_height.max(AUDIO_TRACK_HEIGHT), // use pref default
        TrackKind::Text | TrackKind::Subtitle => tl.text_track_height,
        _ => tl.min_track_height.max(VIDEO_TRACK_HEIGHT),
    })
}

const MEDIA_POOL_HEIGHT: f32 = 50.0;
const STATUS_BAR_HEIGHT: f32 = 18.0;

// Timeline layout values — set from state.timeline_prefs at start of each frame.
// Using thread_local so all functions can read without parameter changes.
thread_local! {
    static TL_CLIP_GAP: std::cell::Cell<f32> = const { std::cell::Cell::new(3.0) };
    static TL_TRIM_HANDLE_WIDTH: std::cell::Cell<f32> = const { std::cell::Cell::new(14.0) };
    static TL_OVERVIEW_BAR_HEIGHT: std::cell::Cell<f32> = const { std::cell::Cell::new(24.0) };
    static TL_SNAP_THRESHOLD_PX: std::cell::Cell<f32> = const { std::cell::Cell::new(10.0) };
    static TL_CLIP_COLORS: std::cell::RefCell<Vec<[u8; 3]>> = std::cell::RefCell::new(vec![
        [50,90,160], [50,130,80], [160,90,50], [130,50,130], [50,130,130], [160,50,80]
    ]);
}

/// Sync timeline prefs into thread-locals at start of each show() call.
fn sync_timeline_prefs(prefs: &crate::prefs::TimelinePrefs) {
    TL_CLIP_GAP.set(prefs.clip_gap_px);
    TL_TRIM_HANDLE_WIDTH.set(prefs.trim_handle_width);
    TL_OVERVIEW_BAR_HEIGHT.set(prefs.overview_bar_height);
    TL_CLIP_COLORS.with(|c| *c.borrow_mut() = prefs.clip_colors.clone());
}

/// Sync snap threshold from editing prefs.
fn sync_editing_prefs_to_timeline(snap_px: f32) {
    TL_SNAP_THRESHOLD_PX.set(snap_px);
}

fn snap_threshold_px() -> f32 { TL_SNAP_THRESHOLD_PX.get() }

// Accessor functions replacing the old consts
fn clip_gap() -> f32 { TL_CLIP_GAP.get() }
#[allow(dead_code)]
fn trim_handle_width() -> f32 { TL_TRIM_HANDLE_WIDTH.get() }

// Keep const for backward compat in places that can't use function calls
const CLIP_GAP: f32 = 3.0; // fallback only — prefer clip_gap()
const TRIM_HANDLE_WIDTH: f32 = 14.0;
const OVERVIEW_BAR_HEIGHT: f32 = 24.0;

const CLIP_COLORS: &[(u8, u8, u8)] = &[
    (50, 90, 160),   // blue
    (50, 130, 80),   // green
    (160, 90, 50),   // orange
    (130, 50, 130),  // purple
    (50, 130, 130),  // teal
    (160, 50, 80),   // red
];

fn clip_color(index: usize) -> egui::Color32 {
    TL_CLIP_COLORS.with(|colors| {
        let colors = colors.borrow();
        let c = colors[index % colors.len()];
        egui::Color32::from_rgb(c[0], c[1], c[2])
    })
}

fn clip_color_bright(index: usize) -> egui::Color32 {
    TL_CLIP_COLORS.with(|colors| {
        let colors = colors.borrow();
        let c = colors[index % colors.len()];
        egui::Color32::from_rgb(c[0].saturating_add(30), c[1].saturating_add(30), c[2].saturating_add(30))
    })
}

/// All geometry computed once per frame at the start of show().
/// Passed by reference to every sub-function so they don't recompute it.
pub struct TimelineLayout {
    pub panel_rect: egui::Rect,
    pub status_rect: egui::Rect,
    pub effective_clip: egui::Rect,
    pub header_rect: egui::Rect,
    pub content_rect: egui::Rect,
    pub content_left: f32,
    pub overview_height: f32,
    pub ruler_height: f32,
    pub ruler_rect: egui::Rect,
    pub pool_height: f32,
    pub tracks_start_y: f32,
    pub tracks_clip: egui::Rect,
    pub tracks_clip_bottom: f32,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub zoom: f32,
    pub track_count: usize,
    pub track_header_width: f32,
}

/// Per-frame scratch data recomputed at the start of every `TimelinePanel::show()` call.
/// Not persisted between frames — reset unconditionally at the top of `show()`.
#[derive(Default)]
pub(crate) struct TimelineFrameCache {
    /// Current snap guide X in screen coordinates (None if no snap active this frame).
    pub snap_line_x: Option<f32>,
    /// Per-track rects this frame: (track_idx, kind, rect). Used for cross-track drag hit testing.
    pub track_rects: Vec<(usize, TrackKind, egui::Rect)>,
    /// Clip id → screen rect, populated while drawing clips. Used for linked-clip connection lines.
    pub clip_screen_rects: HashMap<ClipId, egui::Rect>,
}

#[derive(Default)]
pub struct TimelinePanel {
    /// When true, skip internal toolbar/headers/status rendering (embedded in new shell)
    pub embedded: bool,
    dragging_clip: Option<DragState>,
    trimming_clip: Option<TrimState>,
    roll_trimming: Option<RollTrimState>,
    slipping_clip: Option<SlipState>,
    sliding_clip: Option<SlideState>,
    /// Stores the pointer position at initial mouse-down, before egui's drag threshold kicks in.
    /// Used so trim/move detection uses the click position, not the drifted drag_started position.
    press_origin: Option<egui::Pos2>,
    /// Filmstrip stats for status bar: (visible_unique, total)
    filmstrip_visible: usize,
    filmstrip_total: usize,
    /// Last filmstrip refinement request time — debounce to avoid spamming
    last_refine_request: Option<std::time::Instant>,
    /// Suppress clip context menu when keyframe diamond right-click was handled
    suppress_clip_context_menu: bool,
    /// Track height resize drag: (track_index, start_y, original_height)
    resizing_track: Option<(usize, f32, f32)>,
    /// Marquee selection drag: start position in screen coords
    marquee_start: Option<egui::Pos2>,
    /// Per-frame scratch cache — cleared at the top of `show()`.
    pub(crate) frame_cache: TimelineFrameCache,
}

struct DragState {
    clip_id: ClipId,
    track_idx: usize,
    start_x: f32,
    original_timeline_in: Timecode,
    linked_clip_id: Option<ClipId>,
    linked_original_in: Option<Timecode>,
}

struct TrimState {
    clip_id: ClipId,
    track_idx: usize,
    edge: TrimEdge,
    start_x: f32,
    original_source_in: Timecode,
    original_source_out: Timecode,
    original_timeline_in: Timecode,
    original_timeline_out: Timecode,
    /// For ripple trim: original positions of clips after the trimmed clip
    subsequent_clips: Vec<(ClipId, Timecode, Timecode)>, // (id, original_timeline_in, original_timeline_out)
}

/// Roll trim: adjusts the boundary between two adjacent clips simultaneously.
/// Clip A's out point and Clip B's in point move together, keeping total duration constant.
struct RollTrimState {
    clip_a_id: ClipId,
    clip_b_id: ClipId,
    track_idx: usize,
    start_x: f32,
    /// Original boundary timecode (clip A's timeline_out == clip B's timeline_in)
    original_boundary: Timecode,
    /// Original source out of clip A
    original_a_source_out: Timecode,
    /// Original source in of clip B
    original_b_source_in: Timecode,
    /// Original timeline_out of clip A
    original_a_timeline_out: Timecode,
    /// Original timeline_in of clip B
    original_b_timeline_in: Timecode,
}

#[allow(dead_code)]
struct SlipState {
    clip_id: ClipId,
    track_idx: usize,
    start_x: f32,
    original_source_in: Timecode,
    original_source_out: Timecode,
    /// Media duration for clamping (if known)
    media_duration: Option<Timecode>,
    /// Linked partner clip (for linked video+audio)
    linked_clip_id: Option<ClipId>,
    linked_original_source_in: Option<Timecode>,
    linked_original_source_out: Option<Timecode>,
}

struct SlideState {
    clip_id: ClipId,
    track_idx: usize,
    start_x: f32,
    original_timeline_in: Timecode,
    original_timeline_out: Timecode,
    left_clip_id: Option<ClipId>,
    original_left_out: Option<Timecode>,
    original_left_source_out: Option<Timecode>,
    right_clip_id: Option<ClipId>,
    original_right_in: Option<Timecode>,
    original_right_source_in: Option<Timecode>,
    /// Linked partner clip (for linked video+audio)
    linked_clip_id: Option<ClipId>,
    linked_original_in: Option<Timecode>,
    linked_original_out: Option<Timecode>,
}

#[derive(Clone, Copy)]
enum TrimEdge {
    Left,
    Right,
}

impl TimelinePanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        // Sync timeline prefs into thread-locals for this frame
        sync_timeline_prefs(&state.timeline_prefs);
        sync_editing_prefs_to_timeline(state.snap_threshold_px);

        let mut cmds = Vec::new();
        // Reset per-frame scratch cache — these fields are rebuilt below.
        self.frame_cache = TimelineFrameCache::default();
        let available = ui.available_size();

        // Reset per-frame state
        state.hovered_clip = None;
        self.suppress_clip_context_menu = false;

        self.handle_scroll_zoom(ui, state, &mut cmds);

        let full_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        ui.advance_cursor_after_rect(full_rect);

        let status_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.left(), full_rect.bottom() - STATUS_BAR_HEIGHT),
            full_rect.max,
        );
        let panel_rect = egui::Rect::from_min_max(
            full_rect.min,
            egui::pos2(full_rect.right(), status_rect.top()),
        );

        // Clip the entire timeline panel — nothing should render outside panel_rect
        let effective_clip = panel_rect.intersect(ui.clip_rect());
        ui.set_clip_rect(effective_clip);
        let bg_painter = ui.painter_at(effective_clip);

        // Background
        bg_painter.rect_filled(panel_rect, 0.0, egui::Color32::from_rgb(25, 25, 30));

        // Track header area — skip when embedded (new track_headers.rs handles it)
        let header_rect = if self.embedded {
            // In embedded mode, use zero-width header (content fills full width)
            egui::Rect::from_min_size(panel_rect.min, egui::vec2(0.0, panel_rect.height()))
        } else {
            let rect = egui::Rect::from_min_size(
                panel_rect.min,
                egui::vec2(state.track_header_width, panel_rect.height()),
            );
            bg_painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
            bg_painter.line_segment(
                [egui::pos2(rect.right(), rect.top()), egui::pos2(rect.right(), rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 42, 48)),
            );
            // Track header resize handle
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 2.0, panel_rect.top()),
                egui::vec2(4.0, panel_rect.height()),
            );
            let handle_response = ui.interact(handle_rect, ui.id().with("header_resize"), egui::Sense::drag());
            if handle_response.dragged() {
                state.track_header_width = (state.track_header_width + handle_response.drag_delta().x)
                    .clamp(TRACK_HEADER_MIN_WIDTH, TRACK_HEADER_MAX_WIDTH);
            }
            if handle_response.hovered() || handle_response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                bg_painter.line_segment(
                    [egui::pos2(rect.right() - 1.0, panel_rect.top()), egui::pos2(rect.right() - 1.0, panel_rect.bottom())],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 80, 100)),
                );
            }
            rect
        };

        // Timeline content area — starts after header (0px in embedded mode)
        let content_left = if self.embedded {
            panel_rect.left()
        } else {
            panel_rect.left() + state.track_header_width
        };
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(content_left, panel_rect.top()),
            panel_rect.max,
        );
        state.last_timeline_content_width = content_rect.width();
        let painter = bg_painter.with_clip_rect(content_rect.intersect(ui.clip_rect()));

        // Overview / minimap bar (above ruler) — skip when embedded
        let overview_height = if state.show_overview_bar && !self.embedded { OVERVIEW_BAR_HEIGHT } else { 0.0 };
        if state.show_overview_bar && !self.embedded {
            let overview_rect = egui::Rect::from_min_size(
                content_rect.min,
                egui::vec2(content_rect.width(), OVERVIEW_BAR_HEIGHT),
            );
            self.draw_overview_bar(&painter, &overview_rect, state);
        }

        // Overview bar interaction — raw pointer (ui.interact doesn't work in manual layout)
        // Skip when embedded — the new timeline shell handles its own overview
        let mut overview_bar_consumed = false;
        if state.show_overview_bar && !self.embedded {
            let overview_rect = egui::Rect::from_min_size(
                content_rect.min,
                egui::vec2(content_rect.width(), OVERVIEW_BAR_HEIGHT),
            );
            let dur_secs = state.sequence().total_duration().to_secs_f64() as f32;
            if dur_secs > 0.0 {
                let px_per_sec = overview_rect.width() / dur_secs;
                let zoom = state.timeline_zoom;
                let content_w = state.last_timeline_content_width;
                let (pointer_pos, primary_down, primary_released, drag_delta) = ui.input(|i| (
                    i.pointer.latest_pos(),
                    i.pointer.primary_down(),
                    i.pointer.primary_released(),
                    i.pointer.delta(),
                ));
                let in_overview = pointer_pos.is_some_and(|p| overview_rect.contains(p));

                // Drag: pointer is in overview and button held with movement
                if in_overview && primary_down && drag_delta.x.abs() > 0.5 {
                    overview_bar_consumed = true;
                    let delta_secs = drag_delta.x / px_per_sec;
                    state.timeline_scroll_x = (state.timeline_scroll_x + delta_secs * zoom).max(0.0);
                }
                // Click: pointer released in overview — jump viewport
                if in_overview && primary_released {
                    overview_bar_consumed = true;
                    if let Some(pos) = pointer_pos {
                        let click_secs = (pos.x - overview_rect.left()) / px_per_sec;
                        let view_width_secs = content_w / zoom;
                        let new_start_secs = click_secs - view_width_secs / 2.0;
                        state.timeline_scroll_x = (new_start_secs * zoom).max(0.0);
                    }
                }
            }
        }

        // Time ruler — skip when embedded (new ruler.rs handles it)
        let ruler_height_used = if self.embedded { 0.0 } else { RULER_HEIGHT };
        let ruler_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.top() + overview_height),
            egui::vec2(content_rect.width(), ruler_height_used),
        );
        if !self.embedded {
            painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_rgb(30, 30, 34));
            self.draw_ruler_ticks(&painter, &ruler_rect, state);
        }

        // Sequence markers + ruler click — skip when embedded
        if !self.embedded {
            let zoom = state.timeline_zoom;
            let scroll = state.timeline_scroll_x;
            for marker in &state.sequence().markers {
                let x = content_rect.left() + marker.position.to_secs_f64() as f32 * zoom - scroll;
                if x >= ruler_rect.left() - 6.0 && x <= ruler_rect.right() + 6.0 {
                    let (r, g, b) = marker.color.to_rgb();
                    let color = egui::Color32::from_rgb(r, g, b);
                    let top = ruler_rect.bottom() - 10.0;
                    let bot = ruler_rect.bottom() - 1.0;
                    let half_w = 4.0;
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(x - half_w, top),
                            egui::pos2(x + half_w, top),
                            egui::pos2(x, bot),
                        ],
                        color,
                        egui::Stroke::NONE,
                    ));
                }
            }

            let ruler_response = ui.interact(ruler_rect, ui.id().with("ruler"), egui::Sense::click_and_drag());
            // Track scrub start/end for proxy auto-switch
            if ruler_response.drag_started() && !state.is_scrubbing {
                state.is_scrubbing = true;
                state.pre_scrub_use_proxies = state.use_proxies;
                cmds.push(AppCommand::BeginScrub);
            }
            if ruler_response.drag_stopped() && state.is_scrubbing {
                state.is_scrubbing = false;
                cmds.push(AppCommand::EndScrub);
            }
            if !overview_bar_consumed && (ruler_response.clicked() || ruler_response.dragged()) {
                if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                    let x_in_content = pos.x - content_rect.left() + state.timeline_scroll_x;
                    let secs = x_in_content / state.timeline_zoom;
                    let tc = RationalTime::from_secs_f64(secs as f64, state.fps.rate());
                    let dur = state.sequence_duration();
                    let clamped = if dur.value > 0 {
                        RationalTime { value: tc.value.max(0).min(dur.value), rate: tc.rate }
                    } else {
                        RationalTime { value: tc.value.max(0), rate: tc.rate }
                    };
                    cmds.push(AppCommand::SeekTo(clamped));
                }
            }
        }

        // Media pool, zoom buttons, toggle buttons — skip when embedded
        if !self.embedded {
            let pool_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.left(), content_rect.top() + RULER_HEIGHT),
                egui::vec2(content_rect.width(), MEDIA_POOL_HEIGHT),
            );
            painter.rect_filled(pool_rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
            self.draw_media_pool(ui, &painter, &pool_rect, state, &mut cmds);
            self.draw_zoom_buttons(ui, &header_rect, &ruler_rect, state, &mut cmds);
            self.draw_toggle_buttons(ui, &header_rect, content_rect.top() + RULER_HEIGHT, state, &mut cmds);
        }

        // Draw tracks: Video 2, Video 1, Audio 1
        let pool_h = if self.embedded { 0.0 } else { MEDIA_POOL_HEIGHT };
        let tracks_start_y = content_rect.top() + overview_height + ruler_height_used + pool_h;

        let track_count = state.sequence().tracks.len();

        // Vertical scroll: compute total track height and clamp scroll offset
        let total_tracks_h: f32 = (0..track_count)
            .map(|ti| effective_track_height(&state.sequence().tracks[ti]))
            .sum();
        let available_h = content_rect.bottom() - tracks_start_y;
        let max_scroll_y = (total_tracks_h - available_h).max(0.0);
        state.timeline_scroll_y = state.timeline_scroll_y.clamp(0.0, max_scroll_y);

        let scroll_y = state.timeline_scroll_y;
        let mut track_y = tracks_start_y - scroll_y;

        // Clip rect for the track area — prevents rendering below status bar
        let tracks_clip_bottom = content_rect.bottom();
        let tracks_clip = egui::Rect::from_min_max(
            egui::pos2(panel_rect.left(), tracks_start_y),
            egui::pos2(panel_rect.right(), tracks_clip_bottom),
        );
        // Header painter clipped to tracks area (not the full panel) to prevent track labels
        // and buttons from bleeding into the ruler above or status bar below.
        let header_painter = bg_painter.with_clip_rect(tracks_clip.intersect(effective_clip));

        let layout = TimelineLayout {
            panel_rect,
            status_rect,
            effective_clip,
            header_rect,
            content_rect,
            content_left,
            overview_height,
            ruler_height: ruler_height_used,
            ruler_rect,
            pool_height: pool_h,
            tracks_start_y,
            tracks_clip,
            tracks_clip_bottom,
            scroll_x: state.timeline_scroll_x,
            scroll_y,
            zoom: state.timeline_zoom,
            track_count,
            track_header_width: state.track_header_width,
        };

        // Pre-compute all track rects so cross-track drag can find any track
        {
            let mut pre_y = tracks_start_y - scroll_y;
            for ti in 0..track_count {
                let tk = state.sequence().tracks[ti].kind;
                let th = effective_track_height(&state.sequence().tracks[ti]);
                let rect = egui::Rect::from_min_size(
                    egui::pos2(content_rect.left(), pre_y),
                    egui::vec2(content_rect.width(), th),
                );
                self.frame_cache.track_rects.push((ti, tk, rect));
                pre_y += th;
            }
        }

        // Clip all track interactions to the tracks area — prevents ui.interact() zones
        // from registering below the status bar or above the ruler.
        let saved_clip = ui.clip_rect();
        ui.set_clip_rect(tracks_clip.intersect(saved_clip));

        for track_idx in 0..track_count {
            let track_kind = state.sequence().tracks[track_idx].kind;
            let track_name = state.sequence().tracks[track_idx].name.clone();
            let track_height = effective_track_height(&state.sequence().tracks[track_idx]);

            // Draw track header label and controls — skip when embedded or outside visible area.
            // A track is visible if at least 16px of it is showing (enough for a label row).
            let track_visible = track_y < tracks_clip_bottom - 4.0 && track_y + track_height > tracks_start_y;
            if !self.embedded && track_visible {
                self.draw_single_track_header(
                    ui, state, track_idx, track_y, track_height, &track_name,
                    &layout, &header_painter, &mut cmds,
                );
            } // end if !self.embedded (track header controls)

            let track_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.left(), track_y),
                egui::vec2(content_rect.width(), track_height),
            );

            // Draw clips for this track (clipped to lane rect, bounded by tracks area)
            // Skip rendering for tracks fully outside the visible area
            if track_visible {
            let lane_painter = painter.with_clip_rect(track_rect.intersect(tracks_clip));
            self.draw_track_clips(
                ui, &lane_painter, &track_rect, &content_rect,
                state, &mut cmds, track_idx, true,
            );

            // Media pool drag-and-drop: highlight track when dragging over it
            if let Some(drag_media_id) = state.dragging_media {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) {
                        // Check media type vs track type compatibility:
                        // Audio-only → audio tracks only. Video → video tracks only.
                        let is_audio_only = state.media_pool.get(&drag_media_id)
                            .map(|item| item.video_codec.is_empty())
                            .unwrap_or(false);
                        let compatible = match track_kind {
                            TrackKind::Audio => is_audio_only,
                            _ => !is_audio_only,
                        };
                        if compatible {
                            // Highlight drop target (cyan = valid)
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255)),
                            );
                            // Drop on release
                            if !any_down {
                                state.dragging_media = None;
                                tracing::info!("Media drop: {:?} -> track {} ({})", drag_media_id, track_idx, track_name);
                                cmds.push(AppCommand::AppendClipToTrack {
                                    media_id: drag_media_id,
                                    track_index: track_idx,
                                });
                            }
                        } else {
                            // Show red border for invalid drop target
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 60, 60)),
                            );
                        }
                    }
                }
            }

            // Effect drag-and-drop: highlight clip when dragging effect over it
            if matches!(&state.drag_drop.payload, Some(crate::drag_drop::DragPayload::Effect { .. })) {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) {
                        let zoom = state.timeline_zoom;
                        let scroll = state.timeline_scroll_x;
                        let clips = &state.sequences[state.active_sequence_idx].tracks[track_idx].clips;
                        let mut hit_clip_id = None;
                        for clip in clips {
                            let clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                            let clip_w = clip.duration().to_secs_f64() as f32 * zoom;
                            let clip_rect = egui::Rect::from_min_size(
                                egui::pos2(clip_x + CLIP_GAP, track_rect.top() + 2.0),
                                egui::vec2((clip_w - CLIP_GAP * 2.0).max(2.0), track_rect.height() - 4.0),
                            );
                            if clip_rect.contains(pos) {
                                hit_clip_id = Some(clip.id);
                                // Draw purple highlight border on the hovered clip
                                painter.rect_stroke(
                                    clip_rect,
                                    3.0,
                                    egui::Stroke::new(2.0, egui::Color32::from_rgb(160, 100, 220)),
                                );
                                break;
                            }
                        }

                        if !any_down {
                            if let Some(clip_id) = hit_clip_id {
                                if let Some(crate::drag_drop::DragPayload::Effect { name, is_builtin, plugin_id }) = state.drag_drop.end() {
                                    if is_builtin {
                                        // Look up default builtin effect from the registry by name
                                        let effect = state.effect_registry.effects.iter()
                                            .find(|e| e.name == name)
                                            .and_then(|e| match &e.source {
                                                crate::browser::effect_registry::EffectSource::Builtin(eff) => Some(eff.clone()),
                                                _ => None,
                                            });
                                        if let Some(effect) = effect {
                                            cmds.push(AppCommand::AddBuiltinEffect { clip_id, effect });
                                            state.toast.show(format!("\"{}\" applied to clip", name));
                                        }
                                    } else {
                                        cmds.push(AppCommand::AddEffect { clip_id, plugin_name: plugin_id });
                                        state.toast.show(format!("\"{}\" applied to clip", name));
                                    }
                                }
                            } else {
                                // Dropped on track but not on a clip — just end the drag
                                state.drag_drop.end();
                            }
                        }
                    }
                }
            }

            // Audio effect drag-and-drop: highlight audio clip when dragging audio effect over it
            if matches!(&state.drag_drop.payload, Some(crate::drag_drop::DragPayload::AudioEffect { .. })) {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) {
                        let zoom = state.timeline_zoom;
                        let scroll = state.timeline_scroll_x;
                        let clips = &state.sequences[state.active_sequence_idx].tracks[track_idx].clips;
                        let mut hit_clip_id = None;
                        for clip in clips {
                            let clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                            let clip_w = clip.duration().to_secs_f64() as f32 * zoom;
                            let clip_rect = egui::Rect::from_min_size(
                                egui::pos2(clip_x + CLIP_GAP, track_rect.top() + 2.0),
                                egui::vec2((clip_w - CLIP_GAP * 2.0).max(2.0), track_rect.height() - 4.0),
                            );
                            if clip_rect.contains(pos) {
                                hit_clip_id = Some(clip.id);
                                painter.rect_stroke(
                                    clip_rect, 3.0,
                                    egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 140)),
                                );
                                break;
                            }
                        }

                        if !any_down {
                            if let Some(clip_id) = hit_clip_id {
                                if let Some(crate::drag_drop::DragPayload::AudioEffect { name, type_id }) = state.drag_drop.end() {
                                    cmds.push(AppCommand::AddAudioEffect { clip_id, effect_type: type_id });
                                    state.toast.show(format!("\"{}\" applied to clip", name));
                                }
                            } else {
                                state.drag_drop.end();
                            }
                        }
                    }
                }
            }

            // Title template drag-and-drop: highlight text/video track when dragging title over it
            if matches!(&state.drag_drop.payload, Some(crate::drag_drop::DragPayload::Title { .. })) {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) {
                        let compatible = track_kind == TrackKind::Text || track_kind == TrackKind::Video;
                        if compatible {
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(180, 140, 255)),
                            );
                            if !any_down {
                                if let Some(crate::drag_drop::DragPayload::Title { title_data, template_name, animation }) = state.drag_drop.end() {
                                    // Calculate timeline position from cursor x
                                    let rel_x = pos.x - content_rect.left();
                                    let time_secs = (rel_x + state.timeline_scroll_x) / state.timeline_zoom;
                                    let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
                                    let frame = (time_secs * fps_rate as f32) as i64;
                                    let timeline_in = RationalTime::from_frames(frame.max(0), fps_rate);
                                    let duration_frames = fps_rate * 5;
                                    state.pending_title_data = Some(title_data);
                                    state.pending_title_animation = animation;
                                    cmds.push(AppCommand::CreateTitleClip {
                                        track_index: track_idx,
                                        timeline_in,
                                        duration_frames,
                                    });
                                    state.toast.show(format!("\"{}\" title added to track", template_name));
                                }
                            }
                        } else {
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 60, 60)),
                            );
                        }
                    }
                }
            }
            } // end if track_visible (clip rendering + drag-drop)

            // ── Track height resize handle (bottom edge of header) ──
            if !self.embedded && track_visible {
                let handle_h = 4.0;
                let handle_rect = egui::Rect::from_min_size(
                    egui::pos2(header_rect.left(), track_y + track_height - handle_h),
                    egui::vec2(state.track_header_width, handle_h * 2.0),
                );
                let handle_resp = ui.interact(
                    handle_rect,
                    egui::Id::new(("track_resize", track_idx)),
                    egui::Sense::click_and_drag(),
                );
                if handle_resp.hovered() || handle_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }

                let track_id = state.sequence().tracks[track_idx].id;

                // Double-click: collapse/expand toggle with height memory.
                // Direct mutation on `pre_collapse_height` (UI memory); the new
                // custom_height is routed through SetTrackCustomHeight so the
                // gesture enters undo/engine sync.
                if handle_resp.double_clicked() {
                    let new_h = {
                        let track = &mut state.sequences[state.active_sequence_idx].tracks[track_idx];
                        if track.custom_height.is_some_and(|h| h <= MIN_TRACK_HEIGHT + 1.0) {
                            // Collapsed → expand: restore pre-collapse height or default
                            track.pre_collapse_height.take()
                        } else {
                            // Expand → collapse: save current height, set to minimum
                            track.pre_collapse_height = track.custom_height.or(Some(track_height));
                            Some(MIN_TRACK_HEIGHT)
                        }
                    };
                    cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: new_h });
                }

                // Right-click: preset heights context menu — all committed values
                // go through the command.
                handle_resp.context_menu(|ui| {
                    if ui.button("Small (32px)").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(32.0) });
                        ui.close_menu();
                    }
                    if ui.button("Medium (64px)").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(64.0) });
                        ui.close_menu();
                    }
                    if ui.button("Large (120px)").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(120.0) });
                        ui.close_menu();
                    }
                    if ui.button("Expanded (200px)").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(200.0) });
                        ui.close_menu();
                    }
                    if ui.button("Maximum (300px)").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(300.0) });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Reset to default").clicked() {
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: None });
                        ui.close_menu();
                    }
                });

                if handle_resp.drag_started() {
                    self.resizing_track = Some((track_idx, handle_resp.interact_pointer_pos.map(|p| p.y).unwrap_or(0.0), track_height));
                }
                // Drag — direct per-frame mutation for smooth visual feedback.
                // The terminal value is routed through SetTrackCustomHeight on drag_stopped.
                if handle_resp.dragged() {
                    if let Some((ri, start_y, orig_h)) = self.resizing_track {
                        if ri == track_idx {
                            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                                let delta = pos.y - start_y;
                                let new_h = (orig_h + delta).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
                                let ctrl = ui.input(|i| i.modifiers.ctrl);
                                if ctrl {
                                    // Ctrl+drag: resize all tracks to the same height
                                    for t in &mut state.sequences[state.active_sequence_idx].tracks {
                                        t.custom_height = Some(new_h);
                                    }
                                } else {
                                    state.sequences[state.active_sequence_idx].tracks[track_idx].custom_height = Some(new_h);
                                }
                                state.dirty = true;
                            }
                        }
                    }
                }
                if handle_resp.drag_stopped() {
                    self.resizing_track = None;
                    // Commit terminal value through the command so the gesture is undoable.
                    // Ctrl+drag (all tracks) is not undoable here — the handler is per-track;
                    // a future AddCommand::SetAllTrackHeights would cover it.
                    let final_h = state.sequence().tracks[track_idx].custom_height;
                    cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: final_h });
                }
                // Draw subtle resize line on hover
                if handle_resp.hovered() || handle_resp.dragged() {
                    painter.line_segment(
                        [
                            egui::pos2(header_rect.left() + 2.0, track_y + track_height - 1.0),
                            egui::pos2(header_rect.right() - 2.0, track_y + track_height - 1.0),
                        ],
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(200, 160, 40)),
                    );
                }
            }

            track_y += track_height;
        }

        // Restore clip rect for marquee selection and other post-track rendering
        ui.set_clip_rect(saved_clip);

        // ── Marquee selection (drag on empty track space with Select tool) ──
        let m_zoom = state.timeline_zoom;
        let m_scroll = state.timeline_scroll_x;
        if !self.embedded && state.active_tool == Tool::Select {
            let pointer = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released()));
            let (pressed, down, released) = pointer;
            let cur_pos = ui.input(|i| i.pointer.latest_pos());

            // Start marquee: primary press with no existing drag operation
            if pressed && self.marquee_start.is_none()
                && self.dragging_clip.is_none() && self.trimming_clip.is_none()
                && self.roll_trimming.is_none() && self.slipping_clip.is_none()
                && self.sliding_clip.is_none() && self.resizing_track.is_none()
            {
                if let Some(pos) = cur_pos {
                    // Only start if in the content area (not header) and not on a clip
                    if pos.x > content_rect.left() && pos.y >= tracks_start_y - scroll_y {
                        let on_clip = self.frame_cache.track_rects.iter().any(|(ti, _, tr)| {
                            let clips = &state.sequence().tracks[*ti].clips;
                            self.hit_test_clip_body(clips, &content_rect, tr, m_zoom, m_scroll,
                                tr.height(), pos).is_some()
                        });
                        if !on_clip {
                            self.marquee_start = Some(pos);
                        }
                    }
                }
            }

            // Draw and update marquee during drag
            if let Some(start) = self.marquee_start {
                if down {
                    if let Some(cur) = cur_pos {
                        let marquee_rect = egui::Rect::from_two_pos(start, cur);
                        // Draw selection rectangle
                        ui.painter().rect_filled(
                            marquee_rect, 2.0,
                            egui::Color32::from_rgba_premultiplied(80, 130, 220, 30),
                        );
                        ui.painter().rect_stroke(
                            marquee_rect, 2.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 130, 220)),
                        );

                        // Collect clips intersecting the marquee
                        let mut selected = Vec::new();
                        for (ti, _, track_rect) in &self.frame_cache.track_rects {
                            if marquee_rect.max.y < track_rect.top() || marquee_rect.min.y > track_rect.bottom() {
                                continue; // marquee doesn't overlap this track
                            }
                            let th = track_rect.height();
                            for clip in &state.sequence().tracks[*ti].clips {
                                if clip.kind.is_gap() { continue; }
                                let cr = Self::clip_screen_rect(clip, &content_rect, track_rect, m_zoom, m_scroll, th);
                                if marquee_rect.intersects(cr) {
                                    selected.push(clip.id);
                                }
                            }
                        }
                        state.selected_clips = selected;
                        if let Some(first) = state.selected_clips.first().copied() {
                            cmds.push(AppCommand::SelectClip(Some(first)));
                        }
                    }
                }
                if released {
                    self.marquee_start = None;
                }
            }
        }

        // ── Empty area below tracks: right-click to add new track ──
        if !self.embedded {
            let empty_area = egui::Rect::from_min_max(
                egui::pos2(header_rect.left(), track_y),
                egui::pos2(header_rect.right(), header_rect.bottom()),
            );
            if empty_area.height() > 10.0 {
                let empty_resp = ui.interact(
                    empty_area,
                    ui.id().with("empty_track_area_ctx"),
                    egui::Sense::click(),
                );
                empty_resp.context_menu(|ui| {
                    ui.set_min_width(160.0);
                    ui.label(egui::RichText::new("Add Track").strong().size(11.0));
                    ui.separator();
                    if ui.button("Video Track").clicked() {
                        let name = state.sequence().next_track_name(TrackKind::Video);
                        let track = klipp_timeline::Track::new(&name, TrackKind::Video);
                        state.sequences[state.active_sequence_idx].tracks.push(track);
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                    if ui.button("Audio Track").clicked() {
                        let name = state.sequence().next_track_name(TrackKind::Audio);
                        let track = klipp_timeline::Track::new(&name, TrackKind::Audio);
                        state.sequences[state.active_sequence_idx].tracks.push(track);
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                    if ui.button("Text Track").clicked() {
                        let name = format!("Text {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Text).count() + 1);
                        let track = klipp_timeline::Track::new(&name, TrackKind::Text);
                        state.sequences[state.active_sequence_idx].tracks.push(track);
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                    if ui.button("Subtitle Track").clicked() {
                        let name = format!("Subtitle {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Subtitle).count() + 1);
                        let track = klipp_timeline::Track::new(&name, TrackKind::Subtitle);
                        state.sequences[state.active_sequence_idx].tracks.push(track);
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                });
            }
        }

        // ── Linked clip connection lines ──────────────────────────────
        // Draw subtle vertical lines between linked video+audio clip pairs
        {
            let connection_color = egui::Color32::from_rgba_premultiplied(150, 150, 160, 60);
            let connection_stroke = egui::Stroke::new(1.0, connection_color);
            // Collect linked pairs to avoid double-drawing (only draw from video -> audio)
            let mut drawn_pairs = std::collections::HashSet::new();
            for track in &state.sequence().tracks {
                for clip in &track.clips {
                    if let Some(linked_id) = clip.linked_clip_id {
                        // Only draw once per pair
                        let pair_key = if clip.id.0 < linked_id.0 {
                            (clip.id, linked_id)
                        } else {
                            (linked_id, clip.id)
                        };
                        if drawn_pairs.contains(&pair_key) {
                            continue;
                        }
                        drawn_pairs.insert(pair_key);

                        if let (Some(rect_a), Some(rect_b)) = (
                            self.frame_cache.clip_screen_rects.get(&clip.id),
                            self.frame_cache.clip_screen_rects.get(&linked_id),
                        ) {
                            // Only draw if clips are on different tracks (vertically separated)
                            if (rect_a.top() - rect_b.top()).abs() > 1.0 {
                                let (upper, lower) = if rect_a.top() < rect_b.top() {
                                    (rect_a, rect_b)
                                } else {
                                    (rect_b, rect_a)
                                };
                                let x = upper.left() + 4.0;
                                // Only draw if x is in visible area
                                if x >= content_rect.left() && x <= content_rect.right() {
                                    painter.line_segment(
                                        [egui::pos2(x, upper.bottom()), egui::pos2(x, lower.top())],
                                        connection_stroke,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Media drag released outside any track — auto-create track if enabled
        if let Some(drag_media_id) = state.dragging_media {
            if !ui.input(|i| i.pointer.any_down()) {
                let mut handled = false;
                if state.auto_create_tracks {
                    if let Some(cursor_pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let is_audio_only = state.media_pool.get(&drag_media_id)
                            .map(|item| item.video_codec.is_empty())
                            .unwrap_or(false);
                        // Check if cursor is above topmost track or below bottommost track
                        let first_rect_top = self.frame_cache.track_rects.first().map(|(_, _, r)| r.top());
                        let last_rect_bottom = self.frame_cache.track_rects.last().map(|(_, _, r)| r.bottom());
                        let above_all = first_rect_top.is_some_and(|top| cursor_pos.y < top);
                        let below_all = last_rect_bottom.is_some_and(|bot| cursor_pos.y > bot);

                        if above_all && !is_audio_only {
                            // Create new video track at top
                            let name = state.sequence().next_track_name(TrackKind::Video);
                            let track_idx = {
                                state.sequence_mut().add_video_track(name);
                                // New video track inserted before first audio track — find it
                                // It's the topmost video track (index 0 after insertion)
                                0
                            };
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created video track for media drop above timeline");
                        } else if below_all && is_audio_only {
                            // Create new audio track at bottom
                            let name = state.sequence().next_track_name(TrackKind::Audio);
                            state.sequence_mut().add_audio_track(name);
                            let track_idx = state.sequence().tracks.len() - 1;
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created audio track for media drop below timeline");
                        } else if below_all && !is_audio_only {
                            // Video media below all tracks — create video track above audio
                            let name = state.sequence().next_track_name(TrackKind::Video);
                            state.sequence_mut().add_video_track(name);
                            // add_video_track inserts before first audio track
                            let track_idx = state.sequence().tracks.iter()
                                .rposition(|t| t.kind == TrackKind::Video)
                                .unwrap_or(0);
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created video track for media drop below timeline");
                        } else if above_all && is_audio_only {
                            // Audio media above all tracks — create audio track at bottom
                            let name = state.sequence().next_track_name(TrackKind::Audio);
                            state.sequence_mut().add_audio_track(name);
                            let track_idx = state.sequence().tracks.len() - 1;
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created audio track for media drop above timeline");
                        }
                    }
                }
                if handled {
                    state.dirty = true;
                    cmds.push(AppCommand::SequenceUpdated);
                }
                state.dragging_media = None;
            }
        }

        // Draw media drag ghost — a translucent clip preview following the cursor
        if let Some(drag_media_id) = state.dragging_media {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.latest_pos()) {
                let item_name = state.media_pool.get(&drag_media_id)
                    .map(|m| m.filename.clone())
                    .unwrap_or_default();
                let item_dur = state.media_pool.get(&drag_media_id)
                    .map(|m| m.duration.to_secs_f64() as f32)
                    .unwrap_or(1.0);

                let ghost_w = (item_dur * state.timeline_zoom).clamp(60.0, 300.0);
                let ghost_h = 36.0;
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(cursor_pos.x - ghost_w * 0.5, cursor_pos.y - ghost_h * 0.5),
                    egui::vec2(ghost_w, ghost_h),
                );

                // Use a top-level painter so the ghost draws above everything
                let ghost_painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.id().with("drag_ghost"),
                ));

                // Clip background
                ghost_painter.rect_filled(ghost_rect, 4.0,
                    egui::Color32::from_rgba_unmultiplied(60, 80, 140, 200));
                ghost_painter.rect_stroke(ghost_rect, 4.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(120, 160, 240, 220)));

                // Thumbnail if available
                if let Some(tex) = state.thumbnail_textures.get(&drag_media_id) {
                    let thumb_rect = egui::Rect::from_min_size(
                        ghost_rect.min + egui::vec2(3.0, 3.0),
                        egui::vec2(ghost_h - 6.0, ghost_h - 6.0),
                    );
                    ghost_painter.image(
                        tex.id(),
                        thumb_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

                // Filename label
                let label_x = ghost_rect.left() + ghost_h + 2.0;
                let truncated = if item_name.len() > 20 {
                    format!("{}...", &item_name[..17])
                } else {
                    item_name
                };
                ghost_painter.text(
                    egui::pos2(label_x, ghost_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &truncated,
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );

                // Duration label
                let dur_label = format!("{:.1}s", item_dur);
                ghost_painter.text(
                    egui::pos2(ghost_rect.right() - 4.0, ghost_rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    &dur_label,
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_rgb(180, 180, 200),
                );

                // Request repaint during drag for smooth following
                ui.ctx().request_repaint();
            }
        }

        // Title template drag released outside any track — auto-create text track
        if matches!(&state.drag_drop.payload, Some(crate::drag_drop::DragPayload::Title { .. }))
            && !ui.input(|i| i.pointer.any_down())
        {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.hover_pos()) {
                // Check if cursor is outside all track rects
                let on_any_track = self.frame_cache.track_rects.iter().any(|(_, _, r)| r.contains(cursor_pos));
                if !on_any_track && content_rect.contains(cursor_pos) {
                    if let Some(crate::drag_drop::DragPayload::Title { title_data, template_name, animation }) = state.drag_drop.end() {
                        // Create a new text track and place the title there
                        let track_num = state.sequence().tracks.iter()
                            .filter(|t| t.kind == TrackKind::Text).count() + 1;
                        let track = klipp_timeline::Track::new(
                            &format!("Text {track_num}"),
                            TrackKind::Text,
                        );
                        state.sequences[state.active_sequence_idx].tracks.insert(0, track);
                        let track_idx = 0;
                        let rel_x = cursor_pos.x - content_rect.left();
                        let time_secs = (rel_x + state.timeline_scroll_x) / state.timeline_zoom;
                        let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
                        let frame = (time_secs * fps_rate as f32) as i64;
                        let timeline_in = RationalTime::from_frames(frame.max(0), fps_rate);
                        let duration_frames = fps_rate * 5;
                        state.pending_title_data = Some(title_data);
                        state.pending_title_animation = animation;
                        cmds.push(AppCommand::CreateTitleClip {
                            track_index: track_idx,
                            timeline_in,
                            duration_frames,
                        });
                        state.dirty = true;
                        cmds.push(AppCommand::SequenceUpdated);
                        state.toast.show(format!("\"{}\" title added to new track", template_name));
                    }
                } else if !on_any_track {
                    // Released outside content area — cancel drag
                    state.drag_drop.end();
                }
            } else {
                state.drag_drop.end();
            }
        }

        // Draw title drag ghost — a translucent title preview following the cursor
        if let Some(crate::drag_drop::DragPayload::Title { ref template_name, .. }) = state.drag_drop.payload
        {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.latest_pos()) {
                let ghost_w = 140.0;
                let ghost_h = 32.0;
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(cursor_pos.x - ghost_w * 0.5, cursor_pos.y - ghost_h * 0.5),
                    egui::vec2(ghost_w, ghost_h),
                );

                let ghost_painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.id().with("title_drag_ghost"),
                ));

                ghost_painter.rect_filled(ghost_rect, 4.0,
                    egui::Color32::from_rgba_unmultiplied(120, 80, 180, 200));
                ghost_painter.rect_stroke(ghost_rect, 4.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(180, 140, 255, 220)));

                // "T" icon
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 14.0, ghost_rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    "T",
                    egui::FontId::proportional(14.0),
                    egui::Color32::WHITE,
                );

                // Template name
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 28.0, ghost_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    template_name,
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );

                ui.ctx().request_repaint();
            }
        }

        // Draw effect drag ghost — a translucent pill following the cursor
        if let Some(crate::drag_drop::DragPayload::Effect { ref name, .. }) = state.drag_drop.payload
        {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.latest_pos()) {
                let ghost_w = 120.0;
                let ghost_h = 28.0;
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(cursor_pos.x - ghost_w * 0.5, cursor_pos.y - ghost_h * 0.5),
                    egui::vec2(ghost_w, ghost_h),
                );

                let ghost_painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.id().with("effect_drag_ghost"),
                ));

                ghost_painter.rect_filled(ghost_rect, 6.0,
                    egui::Color32::from_rgba_unmultiplied(140, 80, 200, 210));
                ghost_painter.rect_stroke(ghost_rect, 6.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(200, 160, 255, 230)));

                // "FX" icon
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 16.0, ghost_rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    "FX",
                    egui::FontId::monospace(10.0),
                    egui::Color32::WHITE,
                );

                // Effect name
                let truncated = if name.len() > 14 { format!("{}...", &name[..11]) } else { name.clone() };
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 32.0, ghost_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &truncated,
                    egui::FontId::proportional(10.0),
                    egui::Color32::WHITE,
                );

                ui.ctx().request_repaint();
            }
        }

        // Draw audio effect drag ghost — teal pill following the cursor
        if let Some(crate::drag_drop::DragPayload::AudioEffect { ref name, .. }) = state.drag_drop.payload
        {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.latest_pos()) {
                let ghost_w = 130.0;
                let ghost_h = 28.0;
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(cursor_pos.x - ghost_w * 0.5, cursor_pos.y - ghost_h * 0.5),
                    egui::vec2(ghost_w, ghost_h),
                );

                let ghost_painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.id().with("audio_effect_drag_ghost"),
                ));

                ghost_painter.rect_filled(ghost_rect, 6.0,
                    egui::Color32::from_rgba_unmultiplied(60, 160, 120, 210));
                ghost_painter.rect_stroke(ghost_rect, 6.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(100, 220, 170, 230)));

                // Audio icon
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 14.0, ghost_rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    "\u{266B}", // musical note
                    egui::FontId::proportional(12.0),
                    egui::Color32::WHITE,
                );

                // Effect name
                let truncated = if name.len() > 14 { format!("{}...", &name[..11]) } else { name.clone() };
                ghost_painter.text(
                    egui::pos2(ghost_rect.left() + 28.0, ghost_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &truncated,
                    egui::FontId::proportional(10.0),
                    egui::Color32::WHITE,
                );

                ui.ctx().request_repaint();
            }
        }

        // Global drag cleanup — if mouse released anywhere (not on a valid target), end the drag
        {
            let any_down = ui.input(|i| i.pointer.any_down());
            if !any_down
                && matches!(&state.drag_drop.payload, Some(crate::drag_drop::DragPayload::Effect { .. })
                    | Some(crate::drag_drop::DragPayload::AudioEffect { .. })) {
                    state.drag_drop.end();
                }
        }

        // Range guide lines — 1px amber vertical lines at in/out points through track area
        {
            let theme = crate::theme::KlippTheme::default();
            let zoom = state.timeline_zoom;
            let scroll = state.timeline_scroll_x;
            if let Some(in_pt) = state.sequence().in_point {
                let x = content_rect.left() + in_pt.to_secs_f64() as f32 * zoom - scroll;
                if x >= content_rect.left() && x <= content_rect.right() {
                    crate::timeline::range_visuals::draw_guide_line(&painter, x, tracks_start_y, content_rect.bottom(), &theme);
                }
            }
            if let Some(out_pt) = state.sequence().out_point {
                let x = content_rect.left() + out_pt.to_secs_f64() as f32 * zoom - scroll;
                if x >= content_rect.left() && x <= content_rect.right() {
                    crate::timeline::range_visuals::draw_guide_line(&painter, x, tracks_start_y, content_rect.bottom(), &theme);
                }
            }
        }

        // Playhead line + follow mode
        let seq_dur = state.sequence_duration();
        if state.playback != PlaybackState::Idle || seq_dur.value > 0 {
            let playhead_secs = state.playhead_smooth_secs as f32;
            let view_width = content_rect.width();

            // Auto-scroll based on follow mode — only during playback, not on user clicks
            if state.playback == PlaybackState::Playing {
                match state.playhead_follow {
                    PlayheadFollow::Page => {
                        // Direct mutation intentional — smooth playhead follow requires
                        // per-frame interpolation with frame-timing sensitivity.
                        // Converting to a command would introduce one-frame lag that
                        // breaks smooth follow. This is pure viewport state with no
                        // undo value.
                        let playhead_x_abs = playhead_secs * state.timeline_zoom;
                        let view_end = state.timeline_scroll_x + view_width;
                        if playhead_x_abs > view_end || playhead_x_abs < state.timeline_scroll_x {
                            // Jump one page so playhead is at ~10% from left
                            state.timeline_scroll_x =
                                (playhead_x_abs - view_width * 0.1).max(0.0);
                        }
                    }
                    PlayheadFollow::Smooth => {
                        // Keep playhead at ~25% from left edge during playback
                        let target_scroll =
                            (playhead_secs * state.timeline_zoom - view_width * 0.25).max(0.0);
                        // Smooth lerp to avoid jarring jumps
                        state.timeline_scroll_x +=
                            (target_scroll - state.timeline_scroll_x) * 0.15;
                    }
                    PlayheadFollow::Off => {}
                }
            }

            let playhead_x =
                content_rect.left() + playhead_secs * state.timeline_zoom - state.timeline_scroll_x;

            if playhead_x >= content_rect.left() && playhead_x <= content_rect.right() {
                painter.line_segment(
                    [
                        egui::pos2(playhead_x, content_rect.top()),
                        egui::pos2(playhead_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 80, 80)),
                );
            }
        }

        // Snap indicator line (amber, full height, with timecode label)
        if let Some(snap_x) = self.frame_cache.snap_line_x {
            if snap_x >= content_rect.left() && snap_x <= content_rect.right() {
                let snap_color = egui::Color32::from_rgb(232, 160, 32);
                // Thick amber line spanning full track area
                painter.line_segment(
                    [
                        egui::pos2(snap_x, tracks_start_y),
                        egui::pos2(snap_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(2.0, snap_color),
                );
                // Glow effect — wider translucent line behind
                painter.line_segment(
                    [
                        egui::pos2(snap_x, tracks_start_y),
                        egui::pos2(snap_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(6.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 50)),
                );
                // Timecode label with background pill at top of snap line
                let snap_secs = (snap_x - content_rect.left() + state.timeline_scroll_x) / state.timeline_zoom;
                let snap_tc = RationalTime::from_secs_f64(snap_secs as f64, state.fps.rate());
                let label = snap_tc.to_timecode_string();
                let label_pos = egui::pos2(snap_x + 4.0, tracks_start_y + 3.0);
                // Background pill for readability
                let text_galley = painter.layout_no_wrap(
                    label.clone(),
                    egui::FontId::monospace(9.0),
                    snap_color,
                );
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(label_pos.x - 2.0, label_pos.y - 1.0),
                    egui::vec2(text_galley.size().x + 4.0, text_galley.size().y + 2.0),
                );
                painter.rect_filled(pill_rect, 2.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
                painter.text(
                    label_pos,
                    egui::Align2::LEFT_TOP,
                    &label,
                    egui::FontId::monospace(9.0),
                    snap_color,
                );
            }
        }

        // --- Status bar --- (skip when embedded)
        if self.embedded { return cmds; }
        self.draw_status_bar(ui, state, &layout);

        // ── Audio keyframe rubber band drag update ─────────────────────
        // Process dragging_audio_keyframe: update position on drag, clear on release.
        let pointer_released = ui.input(|i| i.pointer.primary_released());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if let Some(ref drag_kf) = state.dragging_audio_keyframe.clone() {
            if pointer_released || !pointer_down {
                // Release: send end-drag for undo coalescing, then clear drag state
                let drag_key = format!("kf:audio:{}:{:?}:{}", drag_kf.clip_id.0, drag_kf.param, drag_kf.keyframe_index);
                cmds.push(AppCommand::EndKeyframeDrag { key: drag_key, clip_id: drag_kf.clip_id });
                state.dragging_audio_keyframe = None;
            } else if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                // Find the clip's screen rect to map position back to time/value
                let zoom = state.timeline_zoom;
                let scroll = state.timeline_scroll_x;
                // Find clip in sequence to get its rect
                for (ti, track) in state.sequence().tracks.iter().enumerate() {
                    if let Some(clip) = track.find_clip(drag_kf.clip_id) {
                        let track_height = effective_track_height(track);
                        // Find track_rect from track_rects cache
                        if let Some((_, _, track_rect)) = self.frame_cache.track_rects.iter().find(|(idx, _, _)| *idx == ti) {
                            let content_left = track_rect.left(); // approximation — content_rect not available here
                            let clip_rect_approx = {
                                let cx = content_left + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                let cw = clip.duration().to_secs_f64() as f32 * zoom;
                                egui::Rect::from_min_size(
                                    egui::pos2(cx + CLIP_GAP, track_rect.top() + 2.0),
                                    egui::vec2((cw - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
                                )
                            };
                            let clip_dur_secs = clip.duration().to_secs_f64();
                            if clip_dur_secs > 0.0 && clip_rect_approx.width() > 0.0 {
                                let frac = ((pos.x - clip_rect_approx.left()) / clip_rect_approx.width()).clamp(0.0, 1.0) as f64;
                                let mut new_time = frac * clip_dur_secs;

                                // ── Snap keyframe to notable positions ──
                                // Snap threshold: 8px converted to time
                                let snap_thresh_secs = snap_threshold_px() as f64 / (zoom as f64);

                                // Snap to playhead position (relative to clip start)
                                let playhead_abs = state.playhead_smooth_secs;
                                let clip_start_abs = clip.timeline_in.to_secs_f64();
                                let playhead_rel = playhead_abs - clip_start_abs;
                                if playhead_rel >= 0.0 && playhead_rel <= clip_dur_secs
                                    && (new_time - playhead_rel).abs() < snap_thresh_secs {
                                        new_time = playhead_rel;
                                    }

                                // Snap to clip start boundary
                                if new_time < snap_thresh_secs {
                                    new_time = 0.0;
                                }
                                // Snap to clip end boundary
                                if (new_time - clip_dur_secs).abs() < snap_thresh_secs {
                                    new_time = clip_dur_secs;
                                }

                                // Snap to other keyframes on the same clip/param
                                let keyframes = match drag_kf.param {
                                    klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                };
                                for (ki, kf) in keyframes.iter().enumerate() {
                                    if ki != drag_kf.keyframe_index && (kf.time_secs - new_time).abs() < snap_thresh_secs {
                                        new_time = kf.time_secs;
                                        break;
                                    }
                                }

                                let normalized = ((clip_rect_approx.bottom() - pos.y) / clip_rect_approx.height()).clamp(0.0, 1.0);
                                // Auto-scale Y range to match the drawing code
                                let (static_val, kfs_ref) = match drag_kf.param {
                                    klipp_core::AudioParam::Volume => (clip.volume, &clip.volume_keyframes),
                                    klipp_core::AudioParam::Pan => (clip.pan, &clip.pan_keyframes),
                                    klipp_core::AudioParam::Gain => (clip.gain, &clip.gain_keyframes),
                                };
                                let mut v_min = static_val;
                                let mut v_max = static_val;
                                for kf in kfs_ref.iter() {
                                    if kf.value < v_min { v_min = kf.value; }
                                    if kf.value > v_max { v_max = kf.value; }
                                }
                                let v_span = (v_max - v_min).max(0.01);
                                let v_pad = v_span * 0.2;
                                let p_min = v_min - v_pad;
                                let p_range = v_span + v_pad * 2.0;
                                let new_value = p_min + normalized * p_range;
                                cmds.push(AppCommand::MoveAudioKeyframe {
                                    clip_id: drag_kf.clip_id,
                                    param: drag_kf.param,
                                    index: drag_kf.keyframe_index,
                                    new_time_secs: new_time,
                                    new_value,
                                });

                                // Live value readout tooltip
                                let value_text = match drag_kf.param {
                                    klipp_core::AudioParam::Volume => format!("{:.2}", new_value),
                                    klipp_core::AudioParam::Pan => format!("{:.2}", new_value),
                                    klipp_core::AudioParam::Gain => format!("{:.2}", new_value),
                                };
                                let time_text = format!("{:.2}s", new_time);
                                let tooltip = format!("{} | {}", time_text, value_text);
                                let painter = ui.painter().with_clip_rect(
                                    ui.clip_rect().intersect(ui.max_rect()),
                                );
                                painter.text(
                                    egui::pos2(pos.x + 12.0, pos.y - 16.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    &tooltip,
                                    egui::FontId::proportional(10.0),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                                );
                            }
                        }
                        break;
                    }
                }
            }
        }

        // ── Video keyframe rubber band drag update ─────────────────────
        if let Some(ref drag_kf) = state.dragging_video_keyframe.clone() {
            if pointer_released || !pointer_down {
                let drag_key = format!("kf:video:{}:{:?}:{}", drag_kf.clip_id.0, drag_kf.param, drag_kf.keyframe_index);
                cmds.push(AppCommand::EndKeyframeDrag { key: drag_key, clip_id: drag_kf.clip_id });
                state.dragging_video_keyframe = None;
            } else if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                let zoom = state.timeline_zoom;
                let scroll = state.timeline_scroll_x;
                for (ti, track) in state.sequence().tracks.iter().enumerate() {
                    if let Some(clip) = track.find_clip(drag_kf.clip_id) {
                        let track_height = effective_track_height(track);
                        if let Some((_, _, track_rect)) = self.frame_cache.track_rects.iter().find(|(idx, _, _)| *idx == ti) {
                            let content_left = track_rect.left();
                            let clip_rect_approx = {
                                let cx = content_left + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                let cw = clip.duration().to_secs_f64() as f32 * zoom;
                                egui::Rect::from_min_size(
                                    egui::pos2(cx + CLIP_GAP, track_rect.top() + 2.0),
                                    egui::vec2((cw - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
                                )
                            };
                            let clip_dur_secs = clip.duration().to_secs_f64();
                            if clip_dur_secs > 0.0 && clip_rect_approx.width() > 0.0 {
                                let frac = ((pos.x - clip_rect_approx.left()) / clip_rect_approx.width()).clamp(0.0, 1.0) as f64;
                                let mut new_time = frac * clip_dur_secs;

                                // Snap to notable positions
                                let snap_thresh_secs = snap_threshold_px() as f64 / (zoom as f64);
                                let playhead_rel = state.playhead_smooth_secs - clip.timeline_in.to_secs_f64();
                                if playhead_rel >= 0.0 && playhead_rel <= clip_dur_secs
                                    && (new_time - playhead_rel).abs() < snap_thresh_secs {
                                        new_time = playhead_rel;
                                    }
                                if new_time < snap_thresh_secs { new_time = 0.0; }
                                if (new_time - clip_dur_secs).abs() < snap_thresh_secs { new_time = clip_dur_secs; }

                                // Snap to other keyframes
                                let keyframes = match drag_kf.param {
                                    klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                    klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                    klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                    klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                    klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                };
                                for (ki, kf) in keyframes.iter().enumerate() {
                                    if ki != drag_kf.keyframe_index && (kf.time_secs - new_time).abs() < snap_thresh_secs {
                                        new_time = kf.time_secs;
                                        break;
                                    }
                                }

                                // Map Y to param value using auto-scaled range
                                let base_val = match drag_kf.param {
                                    klipp_core::VideoParam::Opacity => {
                                        clip.builtin_effects.iter()
                                            .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                                            .unwrap_or(1.0)
                                    }
                                    _ => {
                                        let t = clip.builtin_effects.iter()
                                            .find_map(|e| match e {
                                                klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, .. } =>
                                                    Some((*position_x, *position_y, *scale_x, *rotation)),
                                                _ => None,
                                            })
                                            .unwrap_or((0.0, 0.0, 1.0, 0.0));
                                        match drag_kf.param {
                                            klipp_core::VideoParam::PositionX => t.0,
                                            klipp_core::VideoParam::PositionY => t.1,
                                            klipp_core::VideoParam::Scale => t.2,
                                            klipp_core::VideoParam::Rotation => t.3,
                                            _ => unreachable!(),
                                        }
                                    }
                                };
                                let mut val_min = base_val;
                                let mut val_max = base_val;
                                for kf in keyframes.iter() {
                                    if kf.value < val_min { val_min = kf.value; }
                                    if kf.value > val_max { val_max = kf.value; }
                                }
                                let val_span = (val_max - val_min).max(0.01);
                                let padding = val_span * 0.2;
                                let p_min = val_min - padding;
                                let p_range = val_span + padding * 2.0;

                                let normalized = ((clip_rect_approx.bottom() - pos.y) / clip_rect_approx.height()).clamp(0.0, 1.0);
                                let new_value = p_min + normalized * p_range;

                                cmds.push(AppCommand::MoveVideoKeyframe {
                                    clip_id: drag_kf.clip_id,
                                    param: drag_kf.param,
                                    index: drag_kf.keyframe_index,
                                    new_time_secs: new_time,
                                    new_value,
                                });

                                // Live value readout tooltip
                                let value_text = match drag_kf.param {
                                    klipp_core::VideoParam::Opacity => format!("{:.0}%", new_value * 100.0),
                                    klipp_core::VideoParam::PositionX => format!("X:{:.0}", new_value),
                                    klipp_core::VideoParam::PositionY => format!("Y:{:.0}", new_value),
                                    klipp_core::VideoParam::Scale => format!("{:.2}x", new_value),
                                    klipp_core::VideoParam::Rotation => format!("{:.1}°", new_value),
                                };
                                let time_text = format!("{:.2}s", new_time);
                                let tooltip = format!("{} | {}", time_text, value_text);
                                let painter = ui.painter().with_clip_rect(
                                    ui.clip_rect().intersect(ui.max_rect()),
                                );
                                painter.text(
                                    egui::pos2(pos.x + 12.0, pos.y - 16.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    &tooltip,
                                    egui::FontId::proportional(10.0),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                                );
                            }
                        }
                        break;
                    }
                }
            }
        }

        // ── Flat rubber band drag release ────────────────────────────────
        if state.dragging_rubber_band.is_some() && (pointer_released || !pointer_down) {
            state.dragging_rubber_band = None;
        }

        // ── Bezier tangent handle drag update ──────────────────────────────
        if let Some(ref drag_bh) = state.dragging_bezier_handle.clone() {
            if pointer_released || !pointer_down {
                let drag_key = format!("kf:tangent:{}:{:?}:{}", drag_bh.clip_id.0, drag_bh.param, drag_bh.keyframe_index);
                cmds.push(AppCommand::EndKeyframeDrag { key: drag_key, clip_id: drag_bh.clip_id });
                state.dragging_bezier_handle = None;
            } else if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                let zoom = state.timeline_zoom;
                let scroll = state.timeline_scroll_x;
                for (ti, track) in state.sequence().tracks.iter().enumerate() {
                    if let Some(clip) = track.find_clip(drag_bh.clip_id) {
                        let track_height = effective_track_height(track);
                        if let Some((_, _, track_rect)) = self.frame_cache.track_rects.iter().find(|(idx, _, _)| *idx == ti) {
                            let content_left = track_rect.left();
                            let clip_rect_approx = {
                                let cx = content_left + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                let cw = clip.duration().to_secs_f64() as f32 * zoom;
                                egui::Rect::from_min_size(
                                    egui::pos2(cx + CLIP_GAP, track_rect.top() + 2.0),
                                    egui::vec2((cw - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
                                )
                            };
                            let clip_dur_secs = clip.duration().to_secs_f64();
                            // Auto-scale Y range to match the drawing code
                            let keyframes = match drag_bh.param {
                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                            };
                            let bh_static = match drag_bh.param {
                                klipp_core::AudioParam::Volume => clip.volume,
                                klipp_core::AudioParam::Pan => clip.pan,
                                klipp_core::AudioParam::Gain => clip.gain,
                            };
                            let mut bh_vmin = bh_static;
                            let mut bh_vmax = bh_static;
                            for k in keyframes.iter() {
                                if k.value < bh_vmin { bh_vmin = k.value; }
                                if k.value > bh_vmax { bh_vmax = k.value; }
                            }
                            let bh_span = (bh_vmax - bh_vmin).max(0.01);
                            let bh_pad = bh_span * 0.2;
                            let p_min = bh_vmin - bh_pad;
                            let p_range = bh_span + bh_pad * 2.0;
                            if let Some(kf) = keyframes.get(drag_bh.keyframe_index) {
                                let kf_x = {
                                    let frac = if clip_dur_secs > 0.0 { (kf.time_secs / clip_dur_secs).clamp(0.0, 1.0) } else { 0.0 };
                                    clip_rect_approx.left() + frac as f32 * clip_rect_approx.width()
                                };
                                let kf_y = {
                                    let norm = ((kf.value - p_min) / p_range).clamp(0.0, 1.0);
                                    clip_rect_approx.bottom() - norm * clip_rect_approx.height()
                                };
                                let pixels_per_sec = if clip_dur_secs > 0.0 { clip_rect_approx.width() / clip_dur_secs as f32 } else { 1.0 };
                                let value_scale = clip_rect_approx.height() / p_range;

                                // Convert mouse position to tangent offset
                                let time_offset = (pos.x - kf_x) / pixels_per_sec;
                                let value_offset = -(pos.y - kf_y) / value_scale;

                                let (ti_arg, to_arg) = if drag_bh.is_out {
                                    // Constrain tangent_out time offset to be non-negative
                                    let clamped_time = time_offset.max(0.0) as f64;
                                    (None, Some((clamped_time, value_offset)))
                                } else {
                                    // Constrain tangent_in time offset to be non-positive
                                    let clamped_time = time_offset.min(0.0) as f64;
                                    (Some((clamped_time, value_offset)), None)
                                };
                                cmds.push(AppCommand::SetAudioKeyframeTangent {
                                    clip_id: drag_bh.clip_id,
                                    param: drag_bh.param,
                                    keyframe_index: drag_bh.keyframe_index,
                                    tangent_in: ti_arg,
                                    tangent_out: to_arg,
                                });

                                // Live tangent readout tooltip
                                let (dt, dv) = if drag_bh.is_out {
                                    (time_offset.max(0.0), value_offset)
                                } else {
                                    (time_offset.min(0.0), value_offset)
                                };
                                let tooltip = format!("dt:{:.3}s dv:{:.3}", dt, dv);
                                let painter = ui.painter().with_clip_rect(
                                    ui.clip_rect().intersect(ui.max_rect()),
                                );
                                painter.text(
                                    egui::pos2(pos.x + 12.0, pos.y - 16.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    &tooltip,
                                    egui::FontId::proportional(10.0),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                                );
                            }
                        }
                        break;
                    }
                }
            }
        }

        // ── Video Bezier tangent handle drag update ─────────────────────────
        if let Some(ref drag_vbh) = state.dragging_video_bezier_handle.clone() {
            if pointer_released || !pointer_down {
                let drag_key = format!("kf:vtangent:{}:{:?}:{}", drag_vbh.clip_id.0, drag_vbh.param, drag_vbh.keyframe_index);
                cmds.push(AppCommand::EndKeyframeDrag { key: drag_key, clip_id: drag_vbh.clip_id });
                state.dragging_video_bezier_handle = None;
            } else if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                let zoom = state.timeline_zoom;
                let scroll = state.timeline_scroll_x;
                for (ti, track) in state.sequence().tracks.iter().enumerate() {
                    if let Some(clip) = track.find_clip(drag_vbh.clip_id) {
                        let track_height = effective_track_height(track);
                        if let Some((_, _, track_rect)) = self.frame_cache.track_rects.iter().find(|(idx, _, _)| *idx == ti) {
                            let content_left = track_rect.left();
                            let clip_rect_approx = {
                                let cx = content_left + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                let cw = clip.duration().to_secs_f64() as f32 * zoom;
                                egui::Rect::from_min_size(
                                    egui::pos2(cx + CLIP_GAP, track_rect.top() + 2.0),
                                    egui::vec2((cw - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
                                )
                            };
                            let clip_dur_secs = clip.duration().to_secs_f64();

                            // Read keyframes for the dragged video parameter
                            let vrb_opacity_static = clip.builtin_effects.iter()
                                .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                                .unwrap_or(1.0);
                            let vrb_transform = clip.builtin_effects.iter()
                                .find_map(|e| match e {
                                    klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, .. } =>
                                        Some((*position_x, *position_y, *scale_x, *rotation)),
                                    _ => None,
                                })
                                .unwrap_or((0.0, 0.0, 1.0, 0.0));
                            let (vrb_static_val, vrb_keyframes) = match drag_vbh.param {
                                klipp_core::VideoParam::Opacity => (vrb_opacity_static, &clip.opacity_keyframes),
                                klipp_core::VideoParam::PositionX => (vrb_transform.0, &clip.transform_pos_x_keyframes),
                                klipp_core::VideoParam::PositionY => (vrb_transform.1, &clip.transform_pos_y_keyframes),
                                klipp_core::VideoParam::Scale => (vrb_transform.2, &clip.transform_scale_keyframes),
                                klipp_core::VideoParam::Rotation => (vrb_transform.3, &clip.transform_rotation_keyframes),
                            };

                            // Compute auto-scale Y axis (same as rendering)
                            let mut vrb_val_min = vrb_static_val;
                            let mut vrb_val_max = vrb_static_val;
                            for kf in vrb_keyframes.iter() {
                                if kf.value < vrb_val_min { vrb_val_min = kf.value; }
                                if kf.value > vrb_val_max { vrb_val_max = kf.value; }
                            }
                            let vrb_val_span = (vrb_val_max - vrb_val_min).max(0.01);
                            let vrb_padding = vrb_val_span * 0.2;
                            let p_min = vrb_val_min - vrb_padding;
                            let p_range = vrb_val_span + vrb_padding * 2.0;

                            if let Some(kf) = vrb_keyframes.get(drag_vbh.keyframe_index) {
                                let kf_x = {
                                    let frac = if clip_dur_secs > 0.0 { (kf.time_secs / clip_dur_secs).clamp(0.0, 1.0) } else { 0.0 };
                                    clip_rect_approx.left() + frac as f32 * clip_rect_approx.width()
                                };
                                let kf_y = {
                                    let norm = ((kf.value - p_min) / p_range).clamp(0.0, 1.0);
                                    clip_rect_approx.bottom() - norm * clip_rect_approx.height()
                                };
                                let pixels_per_sec = if clip_dur_secs > 0.0 { clip_rect_approx.width() / clip_dur_secs as f32 } else { 1.0 };
                                let value_scale = clip_rect_approx.height() / p_range;

                                // Convert mouse position to tangent offset
                                let time_offset = (pos.x - kf_x) / pixels_per_sec;
                                let value_offset = -(pos.y - kf_y) / value_scale;

                                let (ti_arg, to_arg) = if drag_vbh.is_out {
                                    let clamped_time = time_offset.max(0.0) as f64;
                                    (None, Some((clamped_time, value_offset)))
                                } else {
                                    let clamped_time = time_offset.min(0.0) as f64;
                                    (Some((clamped_time, value_offset)), None)
                                };
                                cmds.push(AppCommand::SetVideoKeyframeTangent {
                                    clip_id: drag_vbh.clip_id,
                                    param: drag_vbh.param,
                                    keyframe_index: drag_vbh.keyframe_index,
                                    tangent_in: ti_arg,
                                    tangent_out: to_arg,
                                });

                                // Live tangent readout tooltip
                                let (dt, dv) = if drag_vbh.is_out {
                                    (time_offset.max(0.0), value_offset)
                                } else {
                                    (time_offset.min(0.0), value_offset)
                                };
                                let tooltip = format!("dt:{:.3}s dv:{:.3}", dt, dv);
                                let painter = ui.painter().with_clip_rect(
                                    ui.clip_rect().intersect(ui.max_rect()),
                                );
                                painter.text(
                                    egui::pos2(pos.x + 12.0, pos.y - 16.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    &tooltip,
                                    egui::FontId::proportional(10.0),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
                                );
                            }
                        }
                        break;
                    }
                }
            }
        }

        // ── Keyframe interpolation context menu popup ───────────────────────
        if let Some(ref ctx_menu) = state.keyframe_context_menu.clone() {
            let menu_id = egui::Id::new("kf_interp_ctx_menu");
            let mut close_menu = false;
            let menu_resp = egui::Area::new(menu_id)
                .fixed_pos(ctx_menu.screen_pos)
                .order(egui::Order::Foreground)
                .interactable(true)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style())
                        .inner_margin(egui::Margin::same(4.0))
                        .show(ui, |ui| {
                            ui.set_min_width(140.0);

                            // ── Copy / Paste / Reverse keyframes ────────
                            {
                                // Determine if current param has keyframes
                                let has_kfs = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == ctx_menu.clip_id)
                                    .map(|clip| {
                                        let kfs: &[klipp_core::Keyframe] = match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(p) => match p {
                                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                            },
                                            crate::state::KeyframeParamKind::Video(p) => match p {
                                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                            },
                                        };
                                        !kfs.is_empty()
                                    })
                                    .unwrap_or(false);

                                // Copy Keyframes (only if keyframes exist)
                                if has_kfs
                                    && ui.add(egui::Button::new("  Copy Keyframes").frame(false)).clicked() {
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(param) => {
                                                cmds.push(AppCommand::CopyAudioKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                            crate::state::KeyframeParamKind::Video(param) => {
                                                cmds.push(AppCommand::CopyVideoKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                        }
                                        close_menu = true;
                                    }

                                // Paste Keyframes (only if clipboard has data)
                                if state.keyframe_clipboard.is_some()
                                    && ui.add(egui::Button::new("  Paste Keyframes").frame(false)).clicked() {
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(param) => {
                                                cmds.push(AppCommand::PasteAudioKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                            crate::state::KeyframeParamKind::Video(param) => {
                                                cmds.push(AppCommand::PasteVideoKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                        }
                                        close_menu = true;
                                    }

                                // Reverse Keyframes (only if >= 2 keyframes)
                                let has_enough = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == ctx_menu.clip_id)
                                    .map(|clip| {
                                        let kfs: &[klipp_core::Keyframe] = match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(p) => match p {
                                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                            },
                                            crate::state::KeyframeParamKind::Video(p) => match p {
                                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                            },
                                        };
                                        kfs.len() >= 2
                                    })
                                    .unwrap_or(false);

                                if has_enough
                                    && ui.add(egui::Button::new("  Reverse Keyframes").frame(false)).clicked() {
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(param) => {
                                                cmds.push(AppCommand::ReverseAudioKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                            crate::state::KeyframeParamKind::Video(param) => {
                                                cmds.push(AppCommand::ReverseVideoKeyframes {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                });
                                            }
                                        }
                                        close_menu = true;
                                    }

                                if has_kfs || state.keyframe_clipboard.is_some() {
                                    ui.separator();
                                }
                            }

                            ui.label(egui::RichText::new("Interpolation").small().strong());
                            ui.separator();

                            // Get current interpolation for the keyframe
                            let current_interp = state.sequence().tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == ctx_menu.clip_id)
                                .and_then(|clip| {
                                    let kfs: &[klipp_core::Keyframe] = match &ctx_menu.param_kind {
                                        crate::state::KeyframeParamKind::Audio(p) => match p {
                                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                        },
                                        crate::state::KeyframeParamKind::Video(p) => match p {
                                            klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                            klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                            klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                            klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                            klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                        },
                                    };
                                    kfs.get(ctx_menu.keyframe_index).map(|kf| kf.interpolation)
                                });

                            // Helper closure: emit interpolation command
                            let emit_interp = |cmds: &mut Vec<AppCommand>, interp: klipp_core::Interpolation| {
                                match &ctx_menu.param_kind {
                                    crate::state::KeyframeParamKind::Audio(param) => {
                                        cmds.push(AppCommand::SetAudioKeyframeInterpolation {
                                            clip_id: ctx_menu.clip_id,
                                            param: *param,
                                            keyframe_index: ctx_menu.keyframe_index,
                                            interpolation: interp,
                                        });
                                    }
                                    crate::state::KeyframeParamKind::Video(param) => {
                                        cmds.push(AppCommand::SetVideoKeyframeInterpolation {
                                            clip_id: ctx_menu.clip_id,
                                            param: *param,
                                            keyframe_index: ctx_menu.keyframe_index,
                                            interpolation: interp,
                                        });
                                    }
                                }
                            };

                            // Helper: emit tangent commands for ease presets
                            let emit_ease = |cmds: &mut Vec<AppCommand>, preset: crate::widgets::scrubby_param::EasePreset| {
                                let (out_ratio, in_ratio) = preset.tangent_ratios();
                                let kfs: Option<Vec<klipp_core::Keyframe>> = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == ctx_menu.clip_id)
                                    .map(|clip| {
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(p) => match p {
                                                klipp_core::AudioParam::Volume => clip.volume_keyframes.clone(),
                                                klipp_core::AudioParam::Pan => clip.pan_keyframes.clone(),
                                                klipp_core::AudioParam::Gain => clip.gain_keyframes.clone(),
                                            },
                                            crate::state::KeyframeParamKind::Video(p) => match p {
                                                klipp_core::VideoParam::Opacity => clip.opacity_keyframes.clone(),
                                                klipp_core::VideoParam::PositionX => clip.transform_pos_x_keyframes.clone(),
                                                klipp_core::VideoParam::PositionY => clip.transform_pos_y_keyframes.clone(),
                                                klipp_core::VideoParam::Scale => clip.transform_scale_keyframes.clone(),
                                                klipp_core::VideoParam::Rotation => clip.transform_rotation_keyframes.clone(),
                                            },
                                        }
                                    });
                                if let Some(kfs) = kfs {
                                    let ki = ctx_menu.keyframe_index;
                                    let len = kfs.len();
                                    for idx in [ki.saturating_sub(1), ki] {
                                        if idx >= len { continue; }
                                        let prev_t = if idx > 0 { kfs[idx - 1].time_secs } else { kfs[idx].time_secs - 1.0 };
                                        let next_t = if idx + 1 < len { kfs[idx + 1].time_secs } else { kfs[idx].time_secs + 1.0 };
                                        let span_out = (next_t - kfs[idx].time_secs).abs();
                                        let span_in = (kfs[idx].time_secs - prev_t).abs();
                                        let t_out = Some((span_out * out_ratio, 0.0f32));
                                        let t_in = Some((-span_in * in_ratio, 0.0f32));
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(param) => {
                                                cmds.push(AppCommand::SetAudioKeyframeTangent {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                    keyframe_index: idx,
                                                    tangent_in: t_in,
                                                    tangent_out: t_out,
                                                });
                                            }
                                            crate::state::KeyframeParamKind::Video(param) => {
                                                cmds.push(AppCommand::SetVideoKeyframeTangent {
                                                    clip_id: ctx_menu.clip_id,
                                                    param: *param,
                                                    keyframe_index: idx,
                                                    tangent_in: t_in,
                                                    tangent_out: t_out,
                                                });
                                            }
                                        }
                                    }
                                }
                            };

                            // Linear
                            {
                                let is_current = current_interp == Some(klipp_core::Interpolation::Linear);
                                let text = if is_current {
                                    egui::RichText::new("  Linear").strong()
                                } else {
                                    egui::RichText::new("  Linear")
                                };
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    emit_interp(&mut cmds, klipp_core::Interpolation::Linear);
                                    close_menu = true;
                                }
                            }

                            // Bezier (Custom)
                            {
                                let is_current = current_interp == Some(klipp_core::Interpolation::Bezier);
                                let text = if is_current {
                                    egui::RichText::new("  Bezier (Custom)").strong()
                                } else {
                                    egui::RichText::new("  Bezier (Custom)")
                                };
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    emit_interp(&mut cmds, klipp_core::Interpolation::Bezier);
                                    close_menu = true;
                                }
                            }

                            // Ease presets
                            for (label, preset) in [
                                ("Ease In", crate::widgets::scrubby_param::EasePreset::EaseIn),
                                ("Ease Out", crate::widgets::scrubby_param::EasePreset::EaseOut),
                                ("Ease Both", crate::widgets::scrubby_param::EasePreset::EaseBoth),
                            ] {
                                let text = egui::RichText::new(format!("  {label}"));
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    emit_interp(&mut cmds, klipp_core::Interpolation::Bezier);
                                    emit_ease(&mut cmds, preset);
                                    close_menu = true;
                                }
                            }

                            // Step (Hold)
                            {
                                let is_current = current_interp == Some(klipp_core::Interpolation::Step);
                                let text = if is_current {
                                    egui::RichText::new("  Step (Hold)").strong()
                                } else {
                                    egui::RichText::new("  Step (Hold)")
                                };
                                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    emit_interp(&mut cmds, klipp_core::Interpolation::Step);
                                    close_menu = true;
                                }
                            }

                            // ── Timing ──
                            ui.separator();
                            ui.label(egui::RichText::new("Timing").small().strong());

                            // Gather clip data for timing operations
                            let clip_data: Option<(f64, f64)> = state.sequence().tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == ctx_menu.clip_id)
                                .map(|clip| {
                                    let clip_dur = clip.duration().to_secs_f64();
                                    let kfs: &[klipp_core::Keyframe] = match &ctx_menu.param_kind {
                                        crate::state::KeyframeParamKind::Audio(p) => match p {
                                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                        },
                                        crate::state::KeyframeParamKind::Video(p) => match p {
                                            klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                            klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                            klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                            klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                            klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                        },
                                    };
                                    let max_t = kfs.iter().map(|k| k.time_secs).fold(0.0f64, f64::max);
                                    (clip_dur, max_t)
                                });

                            if ui.add(egui::Button::new("  Half Speed (0.5x)").frame(false)).clicked() {
                                match &ctx_menu.param_kind {
                                    crate::state::KeyframeParamKind::Audio(param) => {
                                        cmds.push(AppCommand::ScaleAudioKeyframeTiming {
                                            clip_id: ctx_menu.clip_id, param: *param, factor: 2.0,
                                        });
                                    }
                                    crate::state::KeyframeParamKind::Video(param) => {
                                        cmds.push(AppCommand::ScaleVideoKeyframeTiming {
                                            clip_id: ctx_menu.clip_id, param: *param, factor: 2.0,
                                        });
                                    }
                                }
                                close_menu = true;
                            }
                            if ui.add(egui::Button::new("  Double Speed (2x)").frame(false)).clicked() {
                                match &ctx_menu.param_kind {
                                    crate::state::KeyframeParamKind::Audio(param) => {
                                        cmds.push(AppCommand::ScaleAudioKeyframeTiming {
                                            clip_id: ctx_menu.clip_id, param: *param, factor: 0.5,
                                        });
                                    }
                                    crate::state::KeyframeParamKind::Video(param) => {
                                        cmds.push(AppCommand::ScaleVideoKeyframeTiming {
                                            clip_id: ctx_menu.clip_id, param: *param, factor: 0.5,
                                        });
                                    }
                                }
                                close_menu = true;
                            }
                            // Fit to Clip: scale so max keyframe time == clip duration
                            if let Some((clip_dur, max_t)) = clip_data {
                                let can_fit = max_t > 0.001 && clip_dur > 0.001;
                                let btn = egui::Button::new("  Fit to Clip").frame(false);
                                if ui.add_enabled(can_fit, btn).clicked() {
                                    let fit_factor = clip_dur / max_t;
                                    match &ctx_menu.param_kind {
                                        crate::state::KeyframeParamKind::Audio(param) => {
                                            cmds.push(AppCommand::ScaleAudioKeyframeTiming {
                                                clip_id: ctx_menu.clip_id, param: *param, factor: fit_factor,
                                            });
                                        }
                                        crate::state::KeyframeParamKind::Video(param) => {
                                            cmds.push(AppCommand::ScaleVideoKeyframeTiming {
                                                clip_id: ctx_menu.clip_id, param: *param, factor: fit_factor,
                                            });
                                        }
                                    }
                                    close_menu = true;
                                }
                            }

                            // ── Insert Bump ──
                            ui.separator();
                            {
                                // Compute bump defaults from playhead and current value
                                let bump_data: Option<(f64, f32, f64)> = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == ctx_menu.clip_id)
                                    .map(|clip| {
                                        let clip_start = clip.timeline_in.to_secs_f64();
                                        let clip_dur = clip.duration().to_secs_f64();
                                        let playhead_rel = (state.playhead_smooth_secs - clip_start).clamp(0.0, clip_dur);
                                        let kfs: &[klipp_core::Keyframe] = match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(p) => match p {
                                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                            },
                                            crate::state::KeyframeParamKind::Video(p) => match p {
                                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                            },
                                        };
                                        // Evaluate current value at playhead (simple: nearest keyframe or default)
                                        let base_val = if kfs.is_empty() {
                                            match &ctx_menu.param_kind {
                                                crate::state::KeyframeParamKind::Audio(p) => match p {
                                                    klipp_core::AudioParam::Volume => clip.volume,
                                                    klipp_core::AudioParam::Pan => clip.pan,
                                                    klipp_core::AudioParam::Gain => clip.gain,
                                                },
                                                crate::state::KeyframeParamKind::Video(p) => match p {
                                                    klipp_core::VideoParam::Opacity => 1.0,
                                                    klipp_core::VideoParam::PositionX => 0.0,
                                                    klipp_core::VideoParam::PositionY => 0.0,
                                                    klipp_core::VideoParam::Scale => 1.0,
                                                    klipp_core::VideoParam::Rotation => 0.0,
                                                },
                                            }
                                        } else {
                                            // Find the keyframe pair surrounding playhead_rel and interpolate simply
                                            let before = kfs.iter().rev().find(|k| k.time_secs <= playhead_rel);
                                            let after = kfs.iter().find(|k| k.time_secs >= playhead_rel);
                                            match (before, after) {
                                                (Some(b), _) => b.value,
                                                (_, Some(a)) => a.value,
                                                _ => 0.0,
                                            }
                                        };
                                        (playhead_rel, base_val, clip_dur)
                                    });

                                if let Some((center_time, base_val, _clip_dur)) = bump_data {
                                    // Compute a sensible bump target: +20% of param range
                                    let bump_val = match &ctx_menu.param_kind {
                                        crate::state::KeyframeParamKind::Audio(p) => match p {
                                            klipp_core::AudioParam::Volume => (base_val + 0.2).min(2.0),
                                            klipp_core::AudioParam::Pan => (base_val + 0.2).min(1.0),
                                            klipp_core::AudioParam::Gain => (base_val + 3.0).min(24.0),
                                        },
                                        crate::state::KeyframeParamKind::Video(p) => match p {
                                            klipp_core::VideoParam::Opacity => (base_val + 0.2).min(1.0),
                                            klipp_core::VideoParam::PositionX => base_val + 0.05,
                                            klipp_core::VideoParam::PositionY => base_val + 0.05,
                                            klipp_core::VideoParam::Scale => (base_val * 1.2).max(base_val + 0.1),
                                            klipp_core::VideoParam::Rotation => base_val + 15.0,
                                        },
                                    };
                                    if ui.add(egui::Button::new("  Insert Bump").frame(false)).clicked() {
                                        match &ctx_menu.param_kind {
                                            crate::state::KeyframeParamKind::Audio(param) => {
                                                cmds.push(AppCommand::InsertAudioKeyframeBump {
                                                    clip_id: ctx_menu.clip_id, param: *param,
                                                    center_time, bump_value: bump_val, base_value: base_val,
                                                    duration_secs: 0.5, ramp_secs: 0.15,
                                                });
                                            }
                                            crate::state::KeyframeParamKind::Video(param) => {
                                                cmds.push(AppCommand::InsertVideoKeyframeBump {
                                                    clip_id: ctx_menu.clip_id, param: *param,
                                                    center_time, bump_value: bump_val, base_value: base_val,
                                                    duration_secs: 0.5, ramp_secs: 0.15,
                                                });
                                            }
                                        }
                                        close_menu = true;
                                    }
                                }
                            }
                        });
                });

            // Close menu on click outside or after selection
            if close_menu {
                state.keyframe_context_menu = None;
            } else {
                // Close if clicked anywhere outside the menu area
                let menu_rect = menu_resp.response.rect;
                if ui.input(|i| i.pointer.any_click()) {
                    if let Some(click_pos) = ui.input(|i| i.pointer.interact_pos()) {
                        if !menu_rect.contains(click_pos) {
                            state.keyframe_context_menu = None;
                        }
                    }
                }
                // Also close on Escape
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    state.keyframe_context_menu = None;
                }
            }
        }

        cmds
    }

    /// Draw a single track's header: name, lock/visibility/mute/solo buttons, controls, context menu.
    #[allow(clippy::too_many_arguments)]
    fn draw_single_track_header(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        track_idx: usize,
        track_y: f32,
        track_height: f32,
        track_name: &str,
        layout: &TimelineLayout,
        bg_painter: &egui::Painter,
        cmds: &mut Vec<AppCommand>,
    ) {
        let track_kind = state.sequence().tracks[track_idx].kind;
        let header_rect = layout.header_rect;
        let tracks_clip = layout.tracks_clip;
        let panel_rect = layout.panel_rect;

        let label_color = match track_kind {
            TrackKind::Audio => egui::Color32::from_rgb(100, 150, 110),
            TrackKind::Text | TrackKind::Subtitle => egui::Color32::from_rgb(180, 150, 100),
            _ => egui::Color32::from_rgb(140, 140, 150),
        };
        let track_locked = state.sequence().tracks[track_idx].locked;

        if track_locked {
            let dim_rect = egui::Rect::from_min_size(
                egui::pos2(header_rect.left(), track_y),
                egui::vec2(state.track_header_width, track_height),
            );
            bg_painter.rect_filled(dim_rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 60));
        }

        bg_painter.text(
            egui::pos2(header_rect.left() + 4.0, track_y + 8.0),
            egui::Align2::LEFT_TOP,
            track_name,
            egui::FontId::proportional(10.0),
            if track_locked { egui::Color32::from_rgb(90, 90, 100) } else { label_color },
        );

        // Track control buttons
        {
            let track = &state.sequence().tracks[track_idx];
            match track_kind {
                TrackKind::Text | TrackKind::Subtitle => {
                    let l_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.right() - 42.0, track_y + (track_height - 14.0) / 2.0),
                        egui::vec2(18.0, 14.0),
                    );
                    let l_resp = ui.interact(l_rect, ui.id().with(("lock_btn", track_idx)), egui::Sense::click());
                    let l_bg = if track_locked {
                        egui::Color32::from_rgb(160, 120, 40)
                    } else if l_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    };
                    bg_painter.rect_filled(l_rect, 2.0, l_bg);
                    let l_icon = if track_locked { Icon::Lock } else { Icon::LockOpen };
                    let l_color = if track_locked { egui::Color32::WHITE } else { egui::Color32::from_rgb(100, 100, 110) };
                    l_icon.image(10.0).tint(l_color).paint_at(ui, l_rect.shrink(2.0).intersect(tracks_clip));
                    if l_resp.clicked() {
                        let track_id = state.sequence().tracks[track_idx].id;
                        cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                    }

                    let v_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.right() - 22.0, track_y + (track_height - 14.0) / 2.0),
                        egui::vec2(18.0, 14.0),
                    );
                    let v_resp = ui.interact(v_rect, ui.id().with(("vis_btn", track_idx)), egui::Sense::click());
                    bg_painter.rect_filled(v_rect, 2.0, if v_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    });
                    let v_icon = if track.visible { Icon::Eye } else { Icon::EyeSlash };
                    let v_col = if track.visible { egui::Color32::from_rgb(120, 120, 140) } else { egui::Color32::from_rgb(60, 60, 70) };
                    v_icon.image(10.0).tint(v_col).paint_at(ui, v_rect.shrink(2.0).intersect(tracks_clip));
                    if v_resp.clicked() {
                        cmds.push(AppCommand::SetTrackVisibility { track_index: track_idx, visible: !track.visible });
                    }
                }
                TrackKind::Video => {
                    let l_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.right() - 42.0, track_y + 4.0),
                        egui::vec2(18.0, 14.0),
                    );
                    let l_resp = ui.interact(l_rect, ui.id().with(("lock_btn", track_idx)), egui::Sense::click());
                    let l_bg = if track_locked {
                        egui::Color32::from_rgb(160, 120, 40)
                    } else if l_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    };
                    bg_painter.rect_filled(l_rect, 2.0, l_bg);
                    let l_icon = if track_locked { Icon::Lock } else { Icon::LockOpen };
                    let l_color = if track_locked { egui::Color32::WHITE } else { egui::Color32::from_rgb(100, 100, 110) };
                    l_icon.image(10.0).tint(l_color).paint_at(ui, l_rect.shrink(2.0).intersect(tracks_clip));
                    if l_resp.clicked() {
                        let track_id = state.sequence().tracks[track_idx].id;
                        cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                    }

                    let v_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.right() - 22.0, track_y + 4.0),
                        egui::vec2(18.0, 14.0),
                    );
                    let v_resp = ui.interact(v_rect, ui.id().with(("vis_btn", track_idx)), egui::Sense::click());
                    let v_col = if track.visible {
                        egui::Color32::from_rgb(120, 120, 140)
                    } else {
                        egui::Color32::from_rgb(60, 60, 70)
                    };
                    bg_painter.rect_filled(v_rect, 2.0, if v_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    });
                    let v_icon = if track.visible { Icon::Eye } else { Icon::EyeSlash };
                    v_icon.image(10.0).tint(v_col).paint_at(ui, v_rect.shrink(2.0).intersect(tracks_clip));
                    if v_resp.clicked() {
                        cmds.push(AppCommand::SetTrackVisibility {
                            track_index: track_idx,
                            visible: !track.visible,
                        });
                    }

                    let opacity_pct = (track.opacity * 100.0) as u32;
                    let opacity_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.left() + 2.0, track_y + track_height - 18.0),
                        egui::vec2(header_rect.width() - 26.0, 14.0),
                    );
                    let opacity_resp = ui.interact(
                        opacity_rect, ui.id().with(("opacity_ctrl", track_idx)),
                        egui::Sense::click_and_drag(),
                    );
                    bg_painter.rect_filled(opacity_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                    let fill_w = opacity_rect.width() * track.opacity;
                    let fill_rect = egui::Rect::from_min_size(
                        opacity_rect.left_top(), egui::vec2(fill_w, opacity_rect.height()),
                    );
                    bg_painter.rect_filled(fill_rect, 2.0, egui::Color32::from_rgb(60, 80, 120));
                    bg_painter.text(
                        opacity_rect.center(), egui::Align2::CENTER_CENTER,
                        format!("{}%", opacity_pct), egui::FontId::proportional(9.0),
                        egui::Color32::from_rgb(180, 180, 200),
                    );
                    if opacity_resp.double_clicked() {
                        cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: 1.0 });
                    } else if opacity_resp.dragged() {
                        let dx = opacity_resp.drag_delta().x;
                        let new_opacity = (track.opacity + dx / opacity_rect.width()).clamp(0.0, 1.0);
                        cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: new_opacity });
                    }
                }
                TrackKind::Audio => {
                    let al_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.right() - 22.0, track_y + 4.0),
                        egui::vec2(18.0, 14.0),
                    );
                    let al_resp = ui.interact(al_rect, ui.id().with(("lock_btn", track_idx)), egui::Sense::click());
                    let al_bg = if track_locked {
                        egui::Color32::from_rgb(160, 120, 40)
                    } else if al_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    };
                    bg_painter.rect_filled(al_rect, 2.0, al_bg);
                    let al_icon = if track_locked { Icon::Lock } else { Icon::LockOpen };
                    let al_color = if track_locked { egui::Color32::WHITE } else { egui::Color32::from_rgb(100, 100, 110) };
                    al_icon.image(10.0).tint(al_color).paint_at(ui, al_rect.shrink(2.0).intersect(tracks_clip));
                    if al_resp.clicked() {
                        let track_id = state.sequence().tracks[track_idx].id;
                        cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                    }

                    let btn_y = track_y + track_height - 16.0;
                    let m_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.left() + 4.0, btn_y), egui::vec2(16.0, 12.0),
                    );
                    let m_resp = ui.interact(m_rect, ui.id().with(("mute_btn", track_idx)), egui::Sense::click());
                    let m_bg = if track.muted {
                        egui::Color32::from_rgb(160, 50, 50)
                    } else if m_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    };
                    bg_painter.rect_filled(m_rect, 2.0, m_bg);
                    let m_icon = if track.muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
                    m_icon.image(10.0).tint(egui::Color32::WHITE).paint_at(ui, m_rect.shrink(1.0).intersect(tracks_clip));
                    if m_resp.clicked() {
                        cmds.push(AppCommand::SetTrackMute { track_index: track_idx, muted: !track.muted });
                    }

                    let s_rect = egui::Rect::from_min_size(
                        egui::pos2(m_rect.right() + 2.0, btn_y), egui::vec2(16.0, 12.0),
                    );
                    let s_resp = ui.interact(s_rect, ui.id().with(("solo_btn", track_idx)), egui::Sense::click());
                    let s_bg = if track.solo {
                        egui::Color32::from_rgb(50, 120, 180)
                    } else if s_resp.hovered() {
                        egui::Color32::from_rgb(50, 50, 60)
                    } else {
                        egui::Color32::from_rgb(35, 35, 42)
                    };
                    bg_painter.rect_filled(s_rect, 2.0, s_bg);
                    bg_painter.text(s_rect.center(), egui::Align2::CENTER_CENTER, "S",
                        egui::FontId::proportional(9.0), egui::Color32::WHITE);
                    if s_resp.clicked() {
                        cmds.push(AppCommand::SetTrackSolo { track_index: track_idx, solo: !track.solo });
                    }

                    let vol_rect = egui::Rect::from_min_size(
                        egui::pos2(s_rect.right() + 4.0, btn_y),
                        egui::vec2(header_rect.right() - s_rect.right() - 8.0, 12.0),
                    );
                    let vol_resp = ui.interact(
                        vol_rect, ui.id().with(("vol_ctrl", track_idx)),
                        egui::Sense::click_and_drag(),
                    );
                    bg_painter.rect_filled(vol_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                    let fill_w = vol_rect.width() * (track.volume / 1.3);
                    let fill_rect = egui::Rect::from_min_size(
                        vol_rect.left_top(), egui::vec2(fill_w, vol_rect.height()),
                    );
                    bg_painter.rect_filled(fill_rect, 2.0, egui::Color32::from_rgb(50, 100, 60));
                    let vol_pct = (track.volume * 100.0) as u32;
                    bg_painter.text(
                        vol_rect.center(), egui::Align2::CENTER_CENTER,
                        format!("{}%", vol_pct), egui::FontId::proportional(9.0),
                        egui::Color32::from_rgb(180, 200, 180),
                    );
                    if vol_resp.double_clicked() {
                        cmds.push(AppCommand::SetTrackVolume { track_index: track_idx, volume: 1.0 });
                    } else if vol_resp.dragged() {
                        let dx = vol_resp.drag_delta().x;
                        let new_vol = (track.volume + dx / vol_rect.width() * 1.3).clamp(0.0, 1.3);
                        cmds.push(AppCommand::SetTrackVolume { track_index: track_idx, volume: new_vol });
                    }

                    let pan_y = btn_y - 14.0;
                    let pan_rect = egui::Rect::from_min_size(
                        egui::pos2(header_rect.left() + 4.0, pan_y),
                        egui::vec2(header_rect.width() - 8.0, 10.0),
                    );
                    let pan_resp = ui.interact(
                        pan_rect, ui.id().with(("pan_ctrl", track_idx)),
                        egui::Sense::click_and_drag(),
                    );
                    bg_painter.rect_filled(pan_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                    let cx = pan_rect.center().x;
                    bg_painter.line_segment(
                        [egui::pos2(cx, pan_rect.top()), egui::pos2(cx, pan_rect.bottom())],
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 70)),
                    );
                    let pan_x = cx + (track.pan * pan_rect.width() * 0.5);
                    bg_painter.rect_filled(
                        egui::Rect::from_center_size(egui::pos2(pan_x, pan_rect.center().y), egui::vec2(4.0, 8.0)),
                        1.0, egui::Color32::from_rgb(140, 180, 150),
                    );
                    let pan_label = if track.pan.abs() < 0.05 {
                        "C".to_string()
                    } else if track.pan < 0.0 {
                        format!("L{}", (track.pan.abs() * 100.0) as u32)
                    } else {
                        format!("R{}", (track.pan * 100.0) as u32)
                    };
                    bg_painter.text(
                        egui::pos2(pan_rect.right() - 2.0, pan_rect.center().y),
                        egui::Align2::RIGHT_CENTER, &pan_label,
                        egui::FontId::proportional(7.0), egui::Color32::from_rgb(140, 140, 150),
                    );
                    if pan_resp.dragged() {
                        let dx = pan_resp.drag_delta().x;
                        let new_pan = (track.pan + dx / (pan_rect.width() * 0.5)).clamp(-1.0, 1.0);
                        cmds.push(AppCommand::SetTrackPan { track_index: track_idx, pan: new_pan });
                    } else if pan_resp.double_clicked() {
                        cmds.push(AppCommand::SetTrackPan { track_index: track_idx, pan: 0.0 });
                    }

                    let vu_y = track_y + 4.0;
                    let vu_w = header_rect.width() - 8.0;
                    let vu_h = 3.0;
                    let (peak_l, peak_r) = state.track_peak_levels
                        .get(track_idx).copied().unwrap_or((0.0, 0.0));
                    for (ch, peak) in [(0, peak_l), (1, peak_r)] {
                        let bar_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.left() + 4.0, vu_y + ch as f32 * (vu_h + 1.0)),
                            egui::vec2(vu_w, vu_h),
                        );
                        bg_painter.rect_filled(bar_rect, 1.0, egui::Color32::from_rgb(20, 20, 25));
                        let fill = (peak.min(1.0) * vu_w).max(0.0);
                        if fill > 0.0 {
                            let color = if peak > 0.9 {
                                egui::Color32::from_rgb(220, 50, 50)
                            } else if peak > 0.7 {
                                egui::Color32::from_rgb(220, 180, 40)
                            } else {
                                egui::Color32::from_rgb(50, 180, 70)
                            };
                            let fill_rect = egui::Rect::from_min_size(
                                bar_rect.left_top(), egui::vec2(fill, vu_h),
                            );
                            bg_painter.rect_filled(fill_rect, 1.0, color);
                        }
                    }
                }
            }
        }

        // Track header right-click context menu
        {
            let hdr_rect = egui::Rect::from_min_size(
                egui::pos2(header_rect.left(), track_y),
                egui::vec2(state.track_header_width, track_height),
            );
            let hdr_resp = ui.interact(
                hdr_rect, ui.id().with(("track_hdr_ctx", track_idx)),
                egui::Sense::click(),
            );
            let track_id = state.sequence().tracks[track_idx].id;
            hdr_resp.context_menu(|ui| {
                ui.set_min_width(180.0);
                ui.menu_button("Add Track Above", |ui| {
                    for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio),
                        ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                        if ui.button(label).clicked() {
                            let name = if kind == TrackKind::Text {
                                format!("Text {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Text).count() + 1)
                            } else if kind == TrackKind::Subtitle {
                                format!("Subtitle {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Subtitle).count() + 1)
                            } else {
                                state.sequence().next_track_name(kind)
                            };
                            let track = klipp_timeline::Track::new(&name, kind);
                            state.sequences[state.active_sequence_idx].tracks.insert(track_idx, track);
                            cmds.push(AppCommand::SequenceUpdated);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Add Track Below", |ui| {
                    for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio),
                        ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                        if ui.button(label).clicked() {
                            let name = if kind == TrackKind::Text {
                                format!("Text {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Text).count() + 1)
                            } else if kind == TrackKind::Subtitle {
                                format!("Subtitle {}", state.sequence().tracks.iter().filter(|t| t.kind == TrackKind::Subtitle).count() + 1)
                            } else {
                                state.sequence().next_track_name(kind)
                            };
                            let track = klipp_timeline::Track::new(&name, kind);
                            let idx = (track_idx + 1).min(state.sequences[state.active_sequence_idx].tracks.len());
                            state.sequences[state.active_sequence_idx].tracks.insert(idx, track);
                            cmds.push(AppCommand::SequenceUpdated);
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button("Rename Track").clicked() { ui.close_menu(); }
                ui.separator();
                let has_clips = !state.sequence().tracks[track_idx].clips.is_empty();
                let remove_resp = ui.add_enabled(!has_clips, egui::Button::new("Remove Track"));
                if !has_clips && remove_resp.clicked() {
                    cmds.push(AppCommand::RemoveTrack { track_id });
                    ui.close_menu();
                }
                if has_clips { remove_resp.on_disabled_hover_text("Remove clips first"); }
                if ui.button("Remove Track (with clips)").clicked() {
                    cmds.push(AppCommand::RemoveTrack { track_id });
                    ui.close_menu();
                }
            });
        }

        // Track separator
        bg_painter.line_segment(
            [egui::pos2(panel_rect.left(), track_y), egui::pos2(panel_rect.right(), track_y)],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(35, 35, 40)),
        );
    }

    /// Draw the status bar at the bottom of the timeline panel.
    fn draw_status_bar(&self, ui: &mut egui::Ui, state: &AppState, layout: &TimelineLayout) {
        let status_painter = ui.painter_at(layout.status_rect);
        status_painter.rect_filled(layout.status_rect, 0.0, egui::Color32::from_rgb(18, 18, 22));
        status_painter.line_segment(
            [layout.status_rect.left_top(), layout.status_rect.right_top()],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 48, 56)),
        );

        let dim = egui::Color32::from_rgb(100, 100, 110);
        let bright = egui::Color32::from_rgb(170, 170, 180);
        let font = egui::FontId::proportional(9.5);
        let sep = "  \u{2022}  ";
        let cy = layout.status_rect.center().y;

        let total_clips: usize = state.sequence().tracks.iter().map(|t| t.clips.len()).sum();
        let mut left_text = format!("{} clip{}", total_clips, if total_clips != 1 { "s" } else { "" });
        if let Some(clip_id) = state.selected_clip {
            if let Some(clip) = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) {
                if let Some(media) = clip.media_id().and_then(|mid| state.media_pool.get(&mid)) {
                    left_text += &format!("{sep}{}", media.filename);
                }
                let dur = clip.duration().to_timecode_string();
                left_text += &format!("{sep}{dur}");
            }
        }
        status_painter.text(
            egui::pos2(layout.status_rect.left() + 8.0, cy),
            egui::Align2::LEFT_CENTER, left_text, font.clone(), bright,
        );

        let view_secs = layout.content_rect.width() / state.timeline_zoom;
        let zoom_label = if view_secs >= 7200.0 {
            format!("{:.0}h", view_secs / 3600.0)
        } else if view_secs >= 120.0 {
            format!("{:.0}m", view_secs / 60.0)
        } else if view_secs >= 1.0 {
            format!("{:.0}s", view_secs)
        } else {
            let fps = state.sequence().fps.num as f32 / state.sequence().fps.den as f32;
            format!("{:.0}f", view_secs * fps)
        };
        let mut right_text = format!("Zoom: {zoom_label} visible");
        if self.filmstrip_total > 0 {
            right_text = format!(
                "Filmstrip: {}/{}{sep}{}",
                self.filmstrip_visible, self.filmstrip_total, right_text
            );
        }
        status_painter.text(
            egui::pos2(layout.status_rect.right() - 8.0, cy),
            egui::Align2::RIGHT_CENTER, right_text, font, dim,
        );
    }

    /// Handle scroll (vertical/horizontal) and zoom (Ctrl+wheel) input.
    fn handle_scroll_zoom(&mut self, ui: &mut egui::Ui, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
        let avail_rect = ui.available_rect_before_wrap();
        let approx_tracks_y = avail_rect.top()
            + (if state.show_overview_bar && !self.embedded { OVERVIEW_BAR_HEIGHT } else { 0.0 })
            + RULER_HEIGHT
            + (if self.embedded { 0.0 } else { MEDIA_POOL_HEIGHT });
        if ui.rect_contains_pointer(avail_rect) {
            let (zoom_factor, pointer_pos) = ui.input(|i| {
                (i.zoom_delta(), i.pointer.interact_pos())
            });

            if (zoom_factor - 1.0).abs() > 0.001 {
                let old_zoom = state.timeline_zoom;
                let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 2000.0);
                let content_left = avail_rect.left() + state.track_header_width;
                let new_scroll_x = if let Some(cursor_pos) = pointer_pos {
                    let cursor_x = cursor_pos.x - content_left;
                    let time_at_cursor = (state.timeline_scroll_x + cursor_x) / old_zoom;
                    Some((time_at_cursor * new_zoom - cursor_x).max(0.0))
                } else {
                    None
                };
                cmds.push(AppCommand::SetTimelineZoom {
                    zoom: new_zoom,
                    scroll_x: new_scroll_x,
                });
            } else {
                let (smooth_sx, smooth_sy) = ui.input_mut(|i| {
                    let s = i.smooth_scroll_delta;
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    (s.x, s.y)
                });
                let shift = ui.input(|i| i.modifiers.shift);
                let pointer_in_tracks = pointer_pos.is_some_and(|p| p.y >= approx_tracks_y - 40.0);

                if pointer_in_tracks && !shift {
                    if smooth_sy.abs() > 0.1 {
                        state.timeline_scroll_y =
                            (state.timeline_scroll_y - smooth_sy).max(0.0);
                    }
                    if smooth_sx.abs() > 0.1 {
                        state.timeline_scroll_x =
                            (state.timeline_scroll_x - smooth_sx).max(0.0);
                        state.playhead_follow = PlayheadFollow::Off;
                    }
                } else {
                    if smooth_sy.abs() > 0.1 {
                        state.timeline_scroll_x =
                            (state.timeline_scroll_x - smooth_sy).max(0.0);
                        state.playhead_follow = PlayheadFollow::Off;
                    }
                }
            }
        }
    }

    /// Draw border, badges (FX, proxy, group, multicam), trim handles, and clip name label.
    fn draw_clip_badges(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        state: &AppState,
        track_kind: klipp_timeline::TrackKind,
        _is_title: bool,
        is_offline: bool,
        border_color: egui::Color32,
        border_width: f32,
    ) {
        // Border
        painter.rect_stroke(
            clip_rect,
            3.0,
            egui::Stroke::new(border_width, border_color),
        );

        // FX badge — show if clip has non-Transform effects applied
        if clip_rect.width() > 40.0 {
            let has_fx = !clip.effects.is_empty()
                || clip.builtin_effects.iter().any(|e| !matches!(e, klipp_effects::BuiltinEffect::Transform { .. }));
            if has_fx {
                let fx_font = egui::FontId::proportional(8.0);
                let fx_galley = painter.layout_no_wrap(
                    "FX".to_string(),
                    fx_font,
                    egui::Color32::WHITE,
                );
                let pill_w = fx_galley.size().x + 6.0;
                let pill_h = fx_galley.size().y + 2.0;
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(clip_rect.right() - pill_w - 4.0, clip_rect.top() + 4.0),
                    egui::vec2(pill_w, pill_h),
                );
                // Dark outline for contrast on any clip color
                painter.rect_filled(
                    pill_rect.expand(1.0),
                    pill_h / 2.0 + 1.0,
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160),
                );
                painter.rect_filled(
                    pill_rect,
                    pill_h / 2.0,
                    egui::Color32::from_rgba_unmultiplied(140, 80, 200, 220),
                );
                painter.galley(
                    egui::pos2(pill_rect.left() + 3.0, pill_rect.top() + 1.0),
                    fx_galley,
                    egui::Color32::WHITE,
                );
            }
        }

        // Linked-clip indicator — subtle lock icon at top-right corner
        if clip.linked_clip_id.is_some() && clip_rect.width() > 20.0 {
            let icon_rect = egui::Rect::from_center_size(
                egui::pos2(clip_rect.right() - 8.0, clip_rect.top() + 8.0),
                egui::vec2(9.0, 9.0),
            );
            crate::icons::Icon::Lock.image(9.0)
                .tint(egui::Color32::from_rgba_premultiplied(255, 255, 255, 120))
                .paint_at(ui, icon_rect);
        }

        // Always-visible trim handle bars on both edges
        if clip_rect.width() > 12.0 {
            let handle_w = 5.0;
            let handle_inset = 2.0; // vertical inset from clip edges
            let handle_color = egui::Color32::from_rgba_premultiplied(200, 200, 210, 60);
            let grip_color = egui::Color32::from_rgba_premultiplied(255, 255, 255, 40);

            // Left handle
            let lh = egui::Rect::from_min_size(
                egui::pos2(clip_rect.left(), clip_rect.top() + handle_inset),
                egui::vec2(handle_w, clip_rect.height() - handle_inset * 2.0),
            );
            painter.rect_filled(lh, 2.0, handle_color);
            // Grip lines (3 small horizontal dashes)
            let cx = lh.center().x;
            let cy = lh.center().y;
            for dy in [-5.0_f32, 0.0, 5.0] {
                painter.line_segment(
                    [egui::pos2(cx - 1.5, cy + dy), egui::pos2(cx + 1.5, cy + dy)],
                    egui::Stroke::new(1.0, grip_color),
                );
            }

            // Right handle
            let rh = egui::Rect::from_min_size(
                egui::pos2(clip_rect.right() - handle_w, clip_rect.top() + handle_inset),
                egui::vec2(handle_w, clip_rect.height() - handle_inset * 2.0),
            );
            painter.rect_filled(rh, 2.0, handle_color);
            for dy in [-5.0_f32, 0.0, 5.0] {
                painter.line_segment(
                    [egui::pos2(rh.center().x - 1.5, cy + dy), egui::pos2(rh.center().x + 1.5, cy + dy)],
                    egui::Stroke::new(1.0, grip_color),
                );
            }
        }

        // Clip name label
        if clip_rect.width() > 40.0 {
            if let Some(title_data) = clip.title_data() {
                // Title clip: show "T" badge and text preview
                let font_size = 11.0;
                let max_text_width = clip_rect.width() - 28.0;
                // "T" badge
                let badge_rect = egui::Rect::from_min_size(
                    egui::pos2(clip_rect.left() + 3.0, clip_rect.center().y - 8.0),
                    egui::vec2(16.0, 16.0),
                );
                painter.rect_filled(badge_rect, 3.0, egui::Color32::from_rgba_premultiplied(200, 180, 255, 180));
                painter.text(
                    badge_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "T",
                    egui::FontId::proportional(10.0),
                    egui::Color32::BLACK,
                );
                // Text preview
                if max_text_width > 10.0 {
                    let text_pos = egui::pos2(clip_rect.left() + 22.0, clip_rect.center().y);
                    let truncated = truncate_text(&title_data.text, max_text_width, font_size);
                    let font = egui::FontId::proportional(font_size);
                    let shadow_color = egui::Color32::from_rgba_premultiplied(0, 0, 0, 200);
                    for &(dx, dy) in &[(-1.0f32, 0.0f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                        painter.text(
                            egui::pos2(text_pos.x + dx, text_pos.y + dy),
                            egui::Align2::LEFT_CENTER,
                            truncated.clone(),
                            font.clone(),
                            shadow_color,
                        );
                    }
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        truncated,
                        font,
                        egui::Color32::from_rgb(220, 200, 255),
                    );
                }
            } else if let Some(media) = clip.media_id().and_then(|mid| state.media_pool.get(&mid)) {
                let font_size = if matches!(track_kind, TrackKind::Audio) { 10.0 } else { 11.0 };
                let max_text_width = clip_rect.width() - 8.0;
                let label = if is_offline {
                    format!("\u{26A0} offline: {}", media.filename)
                } else {
                    media.filename.clone()
                };
                let is_audio_clip = matches!(track_kind, TrackKind::Audio);
                let label_color = if is_offline {
                    egui::Color32::from_rgb(255, 180, 80)
                } else if is_audio_clip {
                    // Semi-transparent white so it doesn't overwhelm the waveform
                    egui::Color32::from_rgba_premultiplied(220, 220, 220, 200)
                } else {
                    egui::Color32::WHITE
                };
                // Audio clips: position label near top so it sits above the waveform
                let text_y = if is_audio_clip {
                    clip_rect.top() + 7.0
                } else {
                    clip_rect.center().y
                };
                let text_pos =
                    egui::pos2(clip_rect.left() + 4.0, text_y);
                let truncated = truncate_text(&label, max_text_width, font_size);
                let font = egui::FontId::proportional(font_size);
                // Draw dark outline/shadow for readability on any background
                let shadow_offsets: &[(f32, f32)] = &[
                    (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0),
                ];
                let shadow_color = egui::Color32::from_rgba_premultiplied(0, 0, 0, 200);
                for &(dx, dy) in shadow_offsets {
                    painter.text(
                        egui::pos2(text_pos.x + dx, text_pos.y + dy),
                        egui::Align2::LEFT_CENTER,
                        truncated.clone(),
                        font.clone(),
                        shadow_color,
                    );
                }
                painter.text(
                    text_pos,
                    egui::Align2::LEFT_CENTER,
                    truncated,
                    font,
                    label_color,
                );

                // Proxy badge [P] — orange pill when using proxy for this clip
                if media.proxy_ready && state.use_proxies && clip_rect.width() > 30.0 {
                    let badge_font = egui::FontId::proportional(8.0);
                    let badge_pos = egui::pos2(clip_rect.right() - 12.0, clip_rect.top() + 8.0);
                    let badge_rect = egui::Rect::from_center_size(
                        badge_pos,
                        egui::vec2(12.0, 11.0),
                    );
                    painter.rect_filled(badge_rect, 3.0, egui::Color32::from_rgb(220, 170, 40));
                    painter.text(
                        badge_pos,
                        egui::Align2::CENTER_CENTER,
                        "P",
                        badge_font,
                        egui::Color32::from_rgb(30, 30, 35),
                    );
                }
            }
        }

        // Render cache indicator — 3px amber bar at top edge showing cached frame %
        if let Some(&(_, cached, total)) = state.render_cache_ranges.iter()
            .find(|(id, _, _)| *id == clip.id)
        {
            if cached > 0 && total > 0 && clip_rect.width() > 10.0 {
                let frac = (cached as f32 / total as f32).clamp(0.0, 1.0);
                let bar_w = clip_rect.width() * frac;
                let bar_rect = egui::Rect::from_min_size(
                    clip_rect.left_top(),
                    egui::vec2(bar_w, 3.0),
                );
                painter.rect_filled(bar_rect, 0.0,
                    egui::Color32::from_rgba_unmultiplied(0xe8, 0xa0, 0x20, 160));
            }
        }

        // Color grade indicator — bottom-right amber dot
        if !clip.node_graph.is_identity() && clip_rect.width() > 20.0 {
            let dot_x = clip_rect.right() - 8.0;
            let dot_y = clip_rect.bottom() - 7.0;
            let dot_pos = egui::pos2(dot_x, dot_y);
            painter.circle_filled(dot_pos, 3.5, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120));
            painter.circle_filled(dot_pos, 2.5, egui::Color32::from_rgb(0xe8, 0xa0, 0x20));
        }

        // Group badge [G] — bottom-left
        if clip.group_id.is_some() && clip_rect.width() > 30.0 {
            let g_pos = egui::pos2(clip_rect.left() + 10.0, clip_rect.bottom() - 8.0);
            let g_rect = egui::Rect::from_center_size(g_pos, egui::vec2(14.0, 11.0));
            painter.rect_filled(g_rect, 3.0, egui::Color32::from_rgb(80, 140, 220));
            painter.text(g_pos, egui::Align2::CENTER_CENTER, "G",
                egui::FontId::proportional(8.0), egui::Color32::WHITE);
        }

        // Multicam indicators (gated by multicam_show_angles pref synced to state)
        if state.multicam_show_angles && clip.multicam_group.is_some() && clip_rect.width() > 20.0 {
            // Angle badge (top-right corner) — positioned left of FX/lock badges
            let angle_text = format!("{}", clip.multicam_angle + 1);
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(clip_rect.right() - 18.0, clip_rect.top() + 2.0),
                egui::vec2(16.0, 14.0),
            );
            painter.rect_filled(badge_rect, 3.0, egui::Color32::from_rgb(0x40, 0x80, 0xe0));
            painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER, &angle_text,
                egui::FontId::proportional(9.0), egui::Color32::WHITE);

            // "MC" badge (top-left)
            let mc_rect = egui::Rect::from_min_size(
                egui::pos2(clip_rect.left() + 2.0, clip_rect.top() + 2.0),
                egui::vec2(20.0, 14.0),
            );
            painter.rect_filled(mc_rect, 3.0, egui::Color32::from_rgba_premultiplied(20, 15, 3, 40));
            painter.text(mc_rect.center(), egui::Align2::CENTER_CENTER, "MC",
                egui::FontId::proportional(8.0), egui::Color32::from_rgb(0xe8, 0xa0, 0x20));

            // Cut list visualization (only on primary angle)
            if clip.multicam_angle == 0 && !clip.multicam_cuts.is_empty() {
                let clip_dur = clip.duration().to_secs_f64();
                let bar_h = 4.0;
                let bar_y = clip_rect.bottom() - bar_h - 1.0;

                // Color palette for angles
                let angle_colors = [
                    egui::Color32::from_rgb(0xe8, 0xa0, 0x20), // amber
                    egui::Color32::from_rgb(0x40, 0x80, 0xe0), // blue
                    egui::Color32::from_rgb(0x40, 0xb0, 0x60), // green
                    egui::Color32::from_rgb(0xe0, 0x60, 0x60), // red
                    egui::Color32::from_rgb(0xa0, 0x60, 0xc0), // purple
                    egui::Color32::from_rgb(0x60, 0xc0, 0xc0), // cyan
                ];

                if clip_dur > 0.0 {
                    for i in 0..clip.multicam_cuts.len() {
                        let (start_t, angle) = clip.multicam_cuts[i];
                        let end_t = if i + 1 < clip.multicam_cuts.len() {
                            clip.multicam_cuts[i + 1].0
                        } else {
                            clip_dur
                        };

                        let x_start = clip_rect.left() + (start_t / clip_dur) as f32 * clip_rect.width();
                        let x_end = clip_rect.left() + (end_t / clip_dur) as f32 * clip_rect.width();
                        let color = angle_colors.get(angle as usize).copied()
                            .unwrap_or(egui::Color32::from_rgb(0x80, 0x80, 0x80));

                        painter.rect_filled(
                            egui::Rect::from_min_max(egui::pos2(x_start, bar_y), egui::pos2(x_end, bar_y + bar_h)),
                            0.0, color,
                        );
                    }
                }
            }
        }
    }

    /// Draw clip overlays: locked track, range selection, hover/skimming, blade cursor.
    #[allow(clippy::too_many_arguments)]
    fn draw_clip_overlays(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        _track_kind: klipp_timeline::TrackKind,
        content_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        interactive: bool,
        pointer_pos: Option<egui::Pos2>,
    ) {
        // Locked track overlay: semi-transparent dark with diagonal lines
        if state.sequence().tracks[track_idx].locked {
            painter.rect_filled(
                clip_rect,
                3.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 80),
            );
            // Diagonal hatch lines
            let step = 8.0;
            let mut lx = clip_rect.left() - clip_rect.height();
            while lx < clip_rect.right() {
                let x0 = lx.max(clip_rect.left());
                let y0_raw = clip_rect.top() + (x0 - lx);
                let y0 = y0_raw.max(clip_rect.top());
                let x1 = (lx + clip_rect.height()).min(clip_rect.right());
                let y1_raw = clip_rect.bottom() - (lx + clip_rect.height() - x1);
                let y1 = y1_raw.min(clip_rect.bottom());
                painter.line_segment(
                    [egui::pos2(x0, y0), egui::pos2(x1, y1)],
                    egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 30)),
                );
                lx += step;
            }
        }

        // Range selection overlay — amber tint on clips within in/out range
        if let (Some(in_pt), Some(out_pt)) = (state.sequence().in_point, state.sequence().out_point) {
            let range_in_secs = in_pt.to_secs_f64();
            let range_out_secs = out_pt.to_secs_f64();
            let clip_in_secs = clip.timeline_in.to_secs_f64();
            let clip_out_secs = clip.timeline_out.to_secs_f64();
            if clip_out_secs > range_in_secs && clip_in_secs < range_out_secs {
                let range_in_x = content_rect.left() + range_in_secs as f32 * zoom - scroll;
                let range_out_x = content_rect.left() + range_out_secs as f32 * zoom - scroll;
                let overlay_left = clip_rect.left().max(range_in_x);
                let overlay_right = clip_rect.right().min(range_out_x);
                if overlay_right > overlay_left {
                    let overlay_rect = egui::Rect::from_min_max(
                        egui::pos2(overlay_left, clip_rect.top()),
                        egui::pos2(overlay_right, clip_rect.bottom()),
                    );
                    crate::timeline::range_visuals::draw_clip_overlay(painter, overlay_rect, &crate::theme::KlippTheme::default());
                }
            }
        }

        if !interactive {
            return;
        }

        // Hover detection — only if pointer is inside the visible content area
        // Blocked when a modal dialog is open to prevent background interaction
        if let Some(pos) = pointer_pos {
            if clip_rect.contains(pos) && content_rect.contains(pos) && !state.has_modal_open() {
                state.hovered_clip = Some(clip.id);

                // Show metadata tooltip on clip hover (only when not skimming)
                if !state.skimming_enabled {
                    if let Some(media) = clip.media_id().and_then(|mid| state.media_pool.get(&mid)) {
                        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new("clip_meta_tooltip"), |ui| {
                            ui.label(egui::RichText::new(&media.filename).strong());
                            ui.label(format!("{} x {} \u{00b7} {}", media.resolution.width, media.resolution.height, media.video_codec));
                            if media.camera_make.is_some() || media.camera_model.is_some() {
                                let make = media.camera_make.as_deref().unwrap_or("");
                                let model = media.camera_model.as_deref().unwrap_or("");
                                ui.label(format!("Camera: {make} {model}").trim().to_string());
                            }
                            if let Some(bd) = media.bit_depth {
                                let pf = media.pixel_format.as_deref().unwrap_or("");
                                ui.label(format!("{bd}-bit {pf}"));
                            }
                            if let Some(vbr) = media.video_bitrate_kbps {
                                if vbr >= 1000 { ui.label(format!("{:.1} Mbps", vbr as f64 / 1000.0)); }
                                else { ui.label(format!("{vbr} kbps")); }
                            }
                        });
                    }
                }

                // Timeline skimming: show frame at hover position in viewer
                if state.skimming_enabled && !state.has_modal_open() {
                    if let Some(media_id) = clip.media_id() {
                        state.browser_skim_active_this_frame = true;

                        // Map hover X to source time
                        let frac = ((pos.x - clip_rect.min.x) / clip_rect.width()).clamp(0.0, 1.0);
                        let clip_dur = clip.duration().to_secs_f64();
                        let source_in_secs = clip.source_in.to_secs_f64();
                        let skim_time = source_in_secs + clip_dur * frac as f64;

                        // Check if skim file exists for this media
                        let has_skim_file = state.media_pool.get(&media_id)
                            .and_then(|m| m.skim_file.as_ref())
                            .is_some_and(|p| p.exists());

                        if has_skim_file {
                            if !state.skim_decode_in_flight {
                                cmds.push(AppCommand::BrowserSkimFrame {
                                    media_id,
                                    time_secs: skim_time,
                                });
                            } else {
                                state.skim_queued_request = Some((media_id, skim_time));
                            }
                        }

                        // Fallback: poster thumbnail
                        if state.browser_skim_texture.is_none() {
                            if let Some(tex) = state.thumbnail_textures.get(&media_id) {
                                state.browser_skim_texture = Some(tex.clone());
                                state.browser_skim_media_id = Some(media_id);
                            }
                        } else {
                            state.browser_skim_media_id = Some(media_id);
                        }

                        // Stay in Program mode during timeline skimming — the skim frame
                        // goes through the frame buffer → video_texture path, which uses
                        // the correct media resolution for letterboxing (WYSIWYG).
                        // Only set source_media_id for skim cleanup tracking.
                        state.source_media_id = Some(media_id);

                        // Draw skim position indicator on clip
                        painter.line_segment(
                            [
                                egui::Pos2::new(pos.x, clip_rect.min.y),
                                egui::Pos2::new(pos.x, clip_rect.max.y),
                            ],
                            egui::Stroke::new(1.5, egui::Color32::WHITE),
                        );

                        // Audio skimming on timeline hover
                        if state.audio_skimming_enabled {
                            // Speed calculation
                            let now = std::time::Instant::now();
                            let dt = now.duration_since(state.skim_last_update).as_secs_f32();
                            let dx = (pos.x - state.skim_last_x).abs();
                            let raw_speed = if dt > 0.001 { dx / dt } else { 0.0 };
                            state.skim_speed_px_per_sec = state.skim_speed_px_per_sec * 0.7 + raw_speed * 0.3;
                            state.skim_last_x = pos.x;
                            state.skim_last_update = now;

                            if let Some(source_path) = state.media_pool.get(&media_id).map(|m| m.path.clone()) {
                                let is_new = state.skim_audio_session.is_none();
                                let elapsed = state.skim_last_audio_dispatch.elapsed().as_millis() as u64;

                                if is_new {
                                    state.skim_last_audio_dispatch = std::time::Instant::now();
                                    cmds.push(AppCommand::BrowserSkimAudioStart {
                                        media_id,
                                        source_path,
                                        time_secs: skim_time,
                                    });
                                } else if elapsed >= 16 {
                                    state.skim_last_audio_dispatch = std::time::Instant::now();
                                    cmds.push(AppCommand::BrowserSkimAudioUpdate {
                                        media_id,
                                        time_secs: skim_time,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Blade cursor/line with snap support
        if state.active_tool == Tool::Blade {
            if let Some(pos) = pointer_pos {
                if clip_rect.contains(pos) && content_rect.contains(pos) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    let blade_snap = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
                    let raw_x = pos.x - content_rect.left() + scroll;
                    let raw_secs = (raw_x as f64 / zoom as f64).max(0.0);
                    let mut blade_x = pos.x;

                    if blade_snap {
                        let snap_thresh_secs = (snap_threshold_px() * 2.0) as f64 / zoom as f64;
                        // Snap targets: playhead, clip edges, in/out markers
                        let mut best_dist = f64::MAX;
                        let mut best_secs = raw_secs;

                        // Playhead
                        let ph_secs = state.playhead.to_secs_f64();
                        let d = (raw_secs - ph_secs).abs();
                        if d < snap_thresh_secs && d < best_dist {
                            best_dist = d;
                            best_secs = ph_secs;
                        }
                        // All clip edges
                        for track in &state.sequence().tracks {
                            for c in &track.clips {
                                for &edge_secs in &[c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()] {
                                    let d = (raw_secs - edge_secs).abs();
                                    if d < snap_thresh_secs && d < best_dist {
                                        best_dist = d;
                                        best_secs = edge_secs;
                                    }
                                }
                            }
                        }
                        // In/out markers
                        if let Some(inp) = state.sequence().in_point {
                            let d = (raw_secs - inp.to_secs_f64()).abs();
                            if d < snap_thresh_secs && d < best_dist {
                                best_dist = d;
                                best_secs = inp.to_secs_f64();
                            }
                        }
                        if let Some(outp) = state.sequence().out_point {
                            let d = (raw_secs - outp.to_secs_f64()).abs();
                            if d < snap_thresh_secs && d < best_dist {
                                best_secs = outp.to_secs_f64();
                            }
                        }

                        if best_dist < f64::MAX {
                            blade_x = content_rect.left() + best_secs as f32 * zoom - scroll;
                        }
                    }

                    // Draw blade line (yellow if snapped, red if unsnapped)
                    let snapped = (blade_x - pos.x).abs() > 0.5;
                    let blade_color = if snapped {
                        egui::Color32::from_rgb(255, 220, 60)
                    } else {
                        egui::Color32::from_rgb(255, 60, 60)
                    };
                    painter.line_segment(
                        [
                            egui::pos2(blade_x, clip_rect.top()),
                            egui::pos2(blade_x, clip_rect.bottom()),
                        ],
                        egui::Stroke::new(if snapped { 2.0 } else { 1.0 }, blade_color),
                    );
                }
            }
        }
    }

    /// Draw audio fade in/out visual indicators on clips.
    #[allow(clippy::too_many_arguments)]
    fn draw_audio_fade_overlay(
        &self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        state: &AppState,
        track_kind: klipp_timeline::TrackKind,
    ) {
        // ── Audio fade in/out visual indicators ────────────────────────
        let show_fade = match state.audio_fade_overlay_mode {
            crate::prefs::FadeOverlayMode::Hidden => false,
            crate::prefs::FadeOverlayMode::AudioOnly => track_kind == TrackKind::Audio,
            crate::prefs::FadeOverlayMode::Both => {
                track_kind == TrackKind::Audio
                    || clip.media_id().and_then(|mid| state.media_pool.get(&mid)).is_some_and(|m| m.has_audio)
            }
        };
        if show_fade && clip_rect.width() > 8.0 && !clip.kind.is_gap() {
            // Resolve fade params (same pattern as rubber band)
            let resolve_fade = |c: &klipp_timeline::Clip| -> (f32, f32, klipp_timeline::FadeCurve, klipp_timeline::FadeCurve) {
                (c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve)
            };
            let (fi_secs, fo_secs, fi_curve, fo_curve) = if let Some(linked_id) = clip.linked_clip_id {
                let mut found = resolve_fade(clip);
                for t in &state.sequence().tracks {
                    if let Some(vc) = t.find_clip(linked_id) {
                        found = resolve_fade(vc);
                        break;
                    }
                }
                found
            } else {
                resolve_fade(clip)
            };

            let clip_dur_secs = clip.duration().to_secs_f64() as f32;
            let fade_clip_painter = painter.with_clip_rect(clip_rect);

            // Curve evaluation: maps t in 0..1 through the selected curve shape
            let eval_curve = |t: f32, curve: klipp_timeline::FadeCurve| -> f32 {
                match curve {
                    klipp_timeline::FadeCurve::Linear => t,
                    klipp_timeline::FadeCurve::Exponential => t * t,
                    klipp_timeline::FadeCurve::Logarithmic => 1.0 - (1.0 - t) * (1.0 - t),
                    klipp_timeline::FadeCurve::SCurve => 3.0 * t * t - 2.0 * t * t * t,
                }
            };

            let fade_line_color = egui::Color32::from_rgba_unmultiplied(255, 195, 50, 200);
            let fade_line_highlight = egui::Color32::from_rgba_unmultiplied(255, 235, 160, 100);
            let fade_line_shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120);
            let num_segments: usize = 40;
            let num_gradient_strips: usize = 10;

            // ── Fade In ──
            if fi_secs > 0.001 && clip_dur_secs > 0.001 {
                let fade_in_px = (fi_secs / clip_dur_secs) * clip_rect.width();
                let fade_in_px = fade_in_px.min(clip_rect.width());

                // Subtle gradient overlay: darker at clip edge (left), transparent at interior (right)
                let fade_rect = egui::Rect::from_min_max(
                    clip_rect.left_top(),
                    egui::pos2(clip_rect.left() + fade_in_px, clip_rect.bottom()),
                );
                let strip_w = fade_rect.width() / num_gradient_strips as f32;
                for s in 0..num_gradient_strips {
                    // frac=0 at interior (right), frac=1 at edge (left)
                    let frac = 1.0 - (s as f32 + 0.5) / num_gradient_strips as f32;
                    let alpha = (frac * frac * 100.0) as u8;
                    let strip_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, alpha);
                    let strip_x = fade_rect.left() + s as f32 * strip_w;
                    let strip_rect = egui::Rect::from_min_size(
                        egui::pos2(strip_x, fade_rect.top()),
                        egui::vec2(strip_w, fade_rect.height()),
                    );
                    fade_clip_painter.rect_filled(strip_rect, 0.0, strip_color);
                }

                // Build curve polyline: from (left, bottom) up to (left + fade_in_px, top-region)
                // The curve represents the gain going from 0 (bottom) to 1 (rubber band level)
                let mut points: Vec<egui::Pos2> = Vec::with_capacity(num_segments + 1);
                for i in 0..=num_segments {
                    let t = i as f32 / num_segments as f32;
                    let gain = eval_curve(t, fi_curve);
                    let x = clip_rect.left() + t * fade_in_px;
                    // Map gain 0..1 to bottom..top of clip rect
                    let y = clip_rect.bottom() - gain * clip_rect.height();
                    points.push(egui::pos2(x, y));
                }

                // Shadow
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(2.5, fade_line_shadow),
                    );
                }
                // Bright highlight (subtle glow)
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(2.2, fade_line_highlight),
                    );
                }
                // Colored curve line
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(1.5, fade_line_color),
                    );
                }

                // Vertical boundary line at fade end
                fade_clip_painter.line_segment(
                    [
                        egui::pos2(clip_rect.left() + fade_in_px, clip_rect.top()),
                        egui::pos2(clip_rect.left() + fade_in_px, clip_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 195, 50, 80)),
                );
            }

            // ── Fade Out ──
            if fo_secs > 0.001 && clip_dur_secs > 0.001 {
                let fade_out_px = (fo_secs / clip_dur_secs) * clip_rect.width();
                let fade_out_px = fade_out_px.min(clip_rect.width());
                let fade_start_x = clip_rect.right() - fade_out_px;

                // Subtle gradient overlay: transparent at interior (left), darker at clip edge (right)
                let fade_rect = egui::Rect::from_min_max(
                    egui::pos2(fade_start_x, clip_rect.top()),
                    clip_rect.right_bottom(),
                );
                let strip_w = fade_rect.width() / num_gradient_strips as f32;
                for s in 0..num_gradient_strips {
                    // frac=0 at interior (left), frac=1 at edge (right)
                    let frac = (s as f32 + 0.5) / num_gradient_strips as f32;
                    let alpha = (frac * frac * 100.0) as u8;
                    let strip_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, alpha);
                    let strip_x = fade_rect.left() + s as f32 * strip_w;
                    let strip_rect = egui::Rect::from_min_size(
                        egui::pos2(strip_x, fade_rect.top()),
                        egui::vec2(strip_w, fade_rect.height()),
                    );
                    fade_clip_painter.rect_filled(strip_rect, 0.0, strip_color);
                }

                // Build curve polyline: from (fade_start_x, top-region) down to (right, bottom)
                // t goes 0..1 across the fade-out region; gain goes 1→0
                let mut points: Vec<egui::Pos2> = Vec::with_capacity(num_segments + 1);
                for i in 0..=num_segments {
                    let t = i as f32 / num_segments as f32;
                    // remaining = 1 - t, so gain fades from 1 to 0
                    let remaining = 1.0 - t;
                    let gain = eval_curve(remaining, fo_curve);
                    let x = fade_start_x + t * fade_out_px;
                    let y = clip_rect.bottom() - gain * clip_rect.height();
                    points.push(egui::pos2(x, y));
                }

                // Shadow
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(2.5, fade_line_shadow),
                    );
                }
                // Bright highlight (subtle glow)
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(2.2, fade_line_highlight),
                    );
                }
                // Colored curve line
                for i in 0..points.len().saturating_sub(1) {
                    fade_clip_painter.line_segment(
                        [points[i], points[i + 1]],
                        egui::Stroke::new(1.5, fade_line_color),
                    );
                }

                // Vertical boundary line at fade start
                fade_clip_painter.line_segment(
                    [
                        egui::pos2(fade_start_x, clip_rect.top()),
                        egui::pos2(fade_start_x, clip_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 195, 50, 80)),
                );
            }
        }
    }

    /// Draw audio waveform overlay on audio track clips.
    #[allow(clippy::too_many_arguments)]
    fn draw_audio_waveform(
        &self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        state: &AppState,
        track_kind: klipp_timeline::TrackKind,
        bright_color: egui::Color32,
    ) {
        if track_kind == TrackKind::Audio && !clip.kind.is_gap() && state.audio_show_waveform {
            // Draw waveform overlay for clips with audio peak data
            // Try waveform_data first, then fall back to audio_cache.peaks
            if let Some(mid) = clip.media_id() {
                let wf_data = state.media_pool.get(&mid).and_then(|m| m.waveform_data.as_ref().cloned());
                let ac_peaks = state.media_pool.get(&mid)
                    .and_then(|m| m.audio_cache.as_ref())
                    .map(|c| c.peaks.clone());
                let peaks_ref = wf_data.or(ac_peaks);
                if let Some(ref peaks) = peaks_ref {
                    if !peaks.is_empty() {
                        // Map clip source range to peak indices
                        // Each peak bin covers PEAK_BIN_SIZE/48000 seconds
                        const PEAK_BIN_SECS: f64 = 256.0 / 48000.0;
                        let src_in = clip.source_in.to_secs_f64();
                        let src_out = clip.source_out.to_secs_f64();
                        let first_bin = (src_in / PEAK_BIN_SECS) as usize;
                        let last_bin = ((src_out / PEAK_BIN_SECS) as usize).min(peaks.len());

                        if first_bin < last_bin {
                            let clip_peaks = &peaks[first_bin..last_bin];
                            // Bright waveform: lighter than background for clear visibility
                            let waveform_color = egui::Color32::from_rgba_premultiplied(
                                bright_color.r().saturating_add(100),
                                bright_color.g().saturating_add(100),
                                bright_color.b().saturating_add(100),
                                220,
                            );

                            let clip_painter = painter.with_clip_rect(clip_rect);
                            let center_y = clip_rect.center().y;
                            let half_h = clip_rect.height() * 0.48; // nearly full height
                            let w = clip_rect.width();

                            // Resolve fade params for waveform shaping (FCP-style tapered waveform)
                            let wf_resolve_fade = |c: &klipp_timeline::Clip| -> (f32, f32, klipp_timeline::FadeCurve, klipp_timeline::FadeCurve) {
                                (c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve)
                            };
                            let (wf_fi_secs, wf_fo_secs, wf_fi_curve, wf_fo_curve) = if let Some(linked_id) = clip.linked_clip_id {
                                let mut found = wf_resolve_fade(clip);
                                for t in &state.sequence().tracks {
                                    if let Some(vc) = t.find_clip(linked_id) {
                                        found = wf_resolve_fade(vc);
                                        break;
                                    }
                                }
                                found
                            } else {
                                wf_resolve_fade(clip)
                            };
                            let wf_clip_dur = clip.duration().to_secs_f64() as f32;

                            // Compute fade envelope at a given time position
                            let wf_fade_envelope = |col_time: f32| -> f32 {
                                let mut gain = 1.0_f32;
                                if wf_fi_secs > 0.001 && col_time < wf_fi_secs {
                                    let frac = (col_time / wf_fi_secs).clamp(0.0, 1.0);
                                    gain = match wf_fi_curve {
                                        klipp_timeline::FadeCurve::Linear => frac,
                                        klipp_timeline::FadeCurve::Exponential => frac * frac,
                                        klipp_timeline::FadeCurve::Logarithmic => 1.0 - (1.0 - frac) * (1.0 - frac),
                                        klipp_timeline::FadeCurve::SCurve => 3.0 * frac * frac - 2.0 * frac * frac * frac,
                                    };
                                }
                                if wf_fo_secs > 0.001 && wf_clip_dur > 0.001 && col_time > wf_clip_dur - wf_fo_secs {
                                    let remaining = wf_clip_dur - col_time;
                                    let frac = (remaining / wf_fo_secs).clamp(0.0, 1.0);
                                    gain *= match wf_fo_curve {
                                        klipp_timeline::FadeCurve::Linear => frac,
                                        klipp_timeline::FadeCurve::Exponential => frac * frac,
                                        klipp_timeline::FadeCurve::Logarithmic => 1.0 - (1.0 - frac) * (1.0 - frac),
                                        klipp_timeline::FadeCurve::SCurve => 3.0 * frac * frac - 2.0 * frac * frac * frac,
                                    };
                                }
                                gain
                            };

                            // Draw min/max envelope with sqrt scaling.
                            // sqrt(amplitude) makes quiet parts much more visible while
                            // preserving the shape. This is how most NLEs display waveforms.
                            // e.g. peak=0.003 -> sqrt(0.003)=0.055 -> 5.5% of height (visible!)
                            //      peak=1.0   -> sqrt(1.0)=1.0     -> 100% of height
                            let num_cols = (w as usize).max(1);
                            for col in 0..num_cols {
                                let x = clip_rect.left() + col as f32;
                                let peak_idx = col * clip_peaks.len() / num_cols;
                                let (pmin, pmax) = clip_peaks[peak_idx.min(clip_peaks.len() - 1)];

                                // Shape waveform by fade envelope (FCP-style)
                                let col_frac = col as f32 / num_cols as f32;
                                let col_time = col_frac * wf_clip_dur;
                                let fade = wf_fade_envelope(col_time);

                                let y_top = center_y - pmax.abs().sqrt().min(1.0) * half_h * fade;
                                let y_bot = center_y + pmin.abs().sqrt().min(1.0) * half_h * fade;

                                if (y_bot - y_top) > 0.5 {
                                    clip_painter.line_segment(
                                        [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                                        egui::Stroke::new(1.0, waveform_color),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Draw filmstrip/thumbnail background for video clips, or solid fill for audio clips.
    #[allow(clippy::too_many_arguments)]
    fn draw_filmstrip_background(
        &mut self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_kind: klipp_timeline::TrackKind,
        content_rect: &egui::Rect,
        fill_color: egui::Color32,
        is_being_dragged: bool,
        clip_index: usize,
    ) {
        let filmstrip_tint = if is_being_dragged {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)
        } else {
            egui::Color32::WHITE
        };

        // Draw thumbnail/filmstrip background for video/text/subtitle clips
        if track_kind != TrackKind::Audio {
            let mut drew_filmstrip = false;
            if let Some(filmstrip) = clip.media_id().and_then(|mid| state.filmstrip_textures.get(&mid)) {
                if !filmstrip.is_empty() {
                    drew_filmstrip = true;
                    let clip_in_secs = clip.source_in.to_secs_f64();
                    let clip_out_secs = clip.source_out.to_secs_f64();
                    let (_, first_tex) = &filmstrip[0];
                    let frame_w = first_tex.size()[0] as f32;
                    let frame_h = first_tex.size()[1] as f32;
                    let frame_aspect = frame_w / frame_h.max(1.0);
                    let tile_h = clip_rect.height();
                    let tile_w = tile_h * frame_aspect;

                    let clip_painter = painter.with_clip_rect(clip_rect);

                    // Only draw tiles that are actually visible in the viewport
                    let num_tiles = ((clip_rect.width() / tile_w).ceil() as usize).max(1);
                    let view_left = content_rect.left();
                    let view_right = content_rect.right();

                    // Compute first/last visible tile indices
                    let first_visible = if clip_rect.left() >= view_left {
                        0
                    } else {
                        ((view_left - clip_rect.left()) / tile_w).floor() as usize
                    };
                    let last_visible = if clip_rect.right() <= view_right {
                        num_tiles
                    } else {
                        (((view_right - clip_rect.left()) / tile_w).ceil() as usize).min(num_tiles)
                    };

                    let sep_w = 1.0_f32;
                    let mut prev_idx: Option<usize> = None;
                    let mut visible_count = 0usize;

                    for ti in first_visible..last_visible {
                        let x0 = clip_rect.left() + ti as f32 * tile_w;
                        let x1 = if ti == num_tiles - 1 {
                            clip_rect.right()
                        } else {
                            x0 + tile_w
                        };

                        let tile_center_x = (x0 + x1) * 0.5;
                        let frac = ((tile_center_x - clip_rect.left()) / clip_rect.width().max(1.0)) as f64;
                        let source_time = clip_in_secs + frac * (clip_out_secs - clip_in_secs);

                        let idx = match filmstrip.binary_search_by(|f| {
                            f.0.partial_cmp(&source_time).unwrap_or(std::cmp::Ordering::Equal)
                        }) {
                            Ok(i) => i,
                            Err(i) => {
                                if i == 0 { 0 }
                                else if i >= filmstrip.len() { filmstrip.len() - 1 }
                                else {
                                    let d_left = (filmstrip[i - 1].0 - source_time).abs();
                                    let d_right = (filmstrip[i].0 - source_time).abs();
                                    if d_left <= d_right { i - 1 } else { i }
                                }
                            }
                        };

                        // Draw separator when frame changes
                        let draw_sep = prev_idx.is_some_and(|pi| pi != idx);
                        let img_x0 = if draw_sep { x0 + sep_w } else { x0 };
                        prev_idx = Some(idx);
                        visible_count += 1;

                        if draw_sep {
                            clip_painter.line_segment(
                                [egui::pos2(x0, clip_rect.top()), egui::pos2(x0, clip_rect.bottom())],
                                egui::Stroke::new(sep_w, egui::Color32::from_rgba_premultiplied(0, 0, 0, 140)),
                            );
                        }

                        let (_, ref tex) = filmstrip[idx];
                        let tile_rect = egui::Rect::from_min_max(
                            egui::pos2(img_x0, clip_rect.top()),
                            egui::pos2(x1, clip_rect.bottom()),
                        );
                        // Draw dark base behind the frame so dark content is still distinguishable
                        clip_painter.rect_filled(tile_rect, 0.0, egui::Color32::from_rgb(35, 35, 40));
                        clip_painter.image(
                            tex.id(),
                            tile_rect,
                            egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            ),
                            filmstrip_tint,
                        );
                        // Subtle bottom gradient to lift dark frames
                        let grad_h = 4.0;
                        let grad_rect = egui::Rect::from_min_max(
                            egui::pos2(img_x0, clip_rect.bottom() - grad_h),
                            egui::pos2(x1, clip_rect.bottom()),
                        );
                        clip_painter.rect_filled(
                            grad_rect,
                            0.0,
                            egui::Color32::from_rgba_premultiplied(60, 60, 70, 80),
                        );
                    }

                    self.filmstrip_visible = visible_count;
                    self.filmstrip_total = filmstrip.len();

                    // Request refinement if tiles are too sparse for current zoom.
                    // Debounce: only request once every 2 seconds.
                    if visible_count > 3 && filmstrip.len() > 1 {
                        let secs_per_tile = (clip_out_secs - clip_in_secs) / num_tiles.max(1) as f64;
                        let min_interval = clip.media_id().and_then(|mid| state.thumbnail_cache.filmstrip_min_interval(&mid));
                        let debounce_ok = self.last_refine_request
                            .map(|t| t.elapsed().as_secs_f64() > 0.5)
                            .unwrap_or(true);
                        if let Some(min_iv) = min_interval {
                            if secs_per_tile < min_iv * 0.5 && !state.filmstrip_generating && debounce_ok {
                                let vis_start_frac = ((first_visible as f32 * tile_w) / clip_rect.width().max(1.0)) as f64;
                                let vis_end_frac = ((last_visible as f32 * tile_w) / clip_rect.width().max(1.0)).min(1.0) as f64;
                                let range_start = clip_in_secs + vis_start_frac * (clip_out_secs - clip_in_secs);
                                let range_end = clip_in_secs + vis_end_frac * (clip_out_secs - clip_in_secs);
                                let new_interval = secs_per_tile.max(0.5);
                                if let Some(mid) = clip.media_id() {
                                    cmds.push(AppCommand::RefineFilmstrip {
                                        media_id: mid,
                                        start_secs: range_start,
                                        end_secs: range_end,
                                        interval_secs: new_interval,
                                    });
                                    self.last_refine_request = Some(std::time::Instant::now());
                                }
                            }
                        }
                    }

                    // Very subtle color tint at top edge only (like Resolve)
                    let (r, g, b) = CLIP_COLORS[clip_index % CLIP_COLORS.len()];
                    let tint_h = 3.0;
                    let tint_rect = egui::Rect::from_min_max(
                        clip_rect.left_top(),
                        egui::pos2(clip_rect.right(), clip_rect.top() + tint_h),
                    );
                    painter.rect_filled(
                        tint_rect,
                        0.0,
                        egui::Color32::from_rgba_premultiplied(r, g, b, 180),
                    );
                }
            }
            if !drew_filmstrip {
                // Fallback: single thumbnail or solid color
                if let Some(tex) = clip.media_id().and_then(|mid| state.thumbnail_textures.get(&mid)) {
                    painter.image(
                        tex.id(),
                        clip_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        filmstrip_tint,
                    );
                    let (r, g, b) = CLIP_COLORS[clip_index % CLIP_COLORS.len()];
                    painter.rect_filled(
                        clip_rect,
                        3.0,
                        egui::Color32::from_rgba_premultiplied(r, g, b, 102),
                    );
                } else {
                    painter.rect_filled(clip_rect, 3.0, fill_color);
                }
            }
        } else {
            // Non-video clip (audio, etc): fill background, then poster frame, then waveform on top
            painter.rect_filled(clip_rect, 3.0, fill_color);

            // Poster frame thumbnail for audio clips linked to video
            if let Some(linked_id) = clip.linked_clip_id {
                let linked_media_id = state.sequence().tracks.iter()
                    .find_map(|t| t.find_clip(linked_id))
                    .and_then(|c| c.media_id());
                if let Some(mid) = linked_media_id {
                    if let Some(tex) = state.thumbnail_textures.get(&mid) {
                        let thumb_h = clip_rect.height();
                        let aspect = tex.size()[0] as f32 / tex.size()[1] as f32;
                        let thumb_w = (thumb_h * aspect).min(40.0);
                        let thumb_rect = egui::Rect::from_min_size(
                            clip_rect.left_top(),
                            egui::vec2(thumb_w, thumb_h),
                        );
                        let clip_painter = painter.with_clip_rect(clip_rect);
                        clip_painter.image(
                            tex.id(),
                            thumb_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100),
                        );
                    }
                }
            }
        }
    }

    /// Process ongoing fade handle drag — update fade values while dragging, emit undo events on release.
    #[allow(clippy::too_many_arguments)]
    fn process_fade_drag(
        &mut self,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        // ── Fade handle drag processing ──────────────────────────────────
        if let Some((fade_clip_id, is_fade_in, _original_fade)) = state.dragging_fade_handle {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    // Find the clip to get its rect
                    if let Some(clip) = clips.iter().find(|c| c.id == fade_clip_id) {
                        let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
                        let clip_dur_secs = clip.duration().to_secs_f64() as f32;
                        if clip_dur_secs > 0.001 && cr.width() > 0.0 {
                            let frac = if is_fade_in {
                                ((pos.x - cr.left()) / cr.width()).clamp(0.0, 0.95)
                            } else {
                                ((cr.right() - pos.x) / cr.width()).clamp(0.0, 0.95)
                            };
                            let new_fade = frac * clip_dur_secs;

                            // Resolve the command clip_id (linked video clip for linked audio)
                            let cmd_clip_id = clip.linked_clip_id.unwrap_or(clip.id);

                            // Read current audio params from the target clip
                            let target = if let Some(linked_id) = clip.linked_clip_id {
                                state.sequence().tracks.iter()
                                    .find_map(|t| t.find_clip(linked_id))
                                    .map(|c| (c.gain, c.volume, c.pan, c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve, c.channel_config))
                            } else {
                                Some((clip.gain, clip.volume, clip.pan, clip.fade_in_secs, clip.fade_out_secs, clip.fade_in_curve, clip.fade_out_curve, clip.channel_config))
                            };

                            if let Some((gain, volume, pan, fi, fo, fi_c, fo_c, ch)) = target {
                                let (new_fi, new_fo) = if is_fade_in { (new_fade, fo) } else { (fi, new_fade) };
                                cmds.push(AppCommand::UpdateClipAudio {
                                    clip_id: cmd_clip_id,
                                    gain, volume, pan,
                                    fade_in_secs: new_fi,
                                    fade_out_secs: new_fo,
                                    fade_in_curve: fi_c,
                                    fade_out_curve: fo_c,
                                    channel_config: ch,
                                });
                            }
                        }
                    }
                }
            }
            // Release: end drag
            if pointer_released || !pointer_down {
                // Emit EndAudioDrag for undo
                if let Some(clip) = clips.iter().find(|c| c.id == fade_clip_id) {
                    let cmd_clip_id = clip.linked_clip_id.unwrap_or(clip.id);
                    let target = if let Some(linked_id) = clip.linked_clip_id {
                        state.sequence().tracks.iter()
                            .find_map(|t| t.find_clip(linked_id))
                            .map(|c| (c.gain, c.volume, c.pan, c.fade_in_secs, c.fade_out_secs))
                    } else {
                        Some((clip.gain, clip.volume, clip.pan, clip.fade_in_secs, clip.fade_out_secs))
                    };
                    if let Some((g, v, p, fi, fo)) = target {
                        cmds.push(AppCommand::EndAudioDrag {
                            key: format!("fade_handle_{:?}", cmd_clip_id),
                            clip_id: cmd_clip_id,
                            new_gain: g, new_volume: v, new_pan: p,
                            new_fade_in: fi, new_fade_out: fo,
                        });
                    }
                }
                state.dragging_fade_handle = None;
            }
        }
    }

    /// Detect drag start: edge-first detection for trim, fade handles, roll trim, then clip-body for move/slip/slide.
    #[allow(clippy::too_many_arguments)]
    fn detect_drag_start(
        &mut self,
        ui: &egui::Ui,
        track_response: &egui::Response,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        track_kind: klipp_timeline::TrackKind,
        pointer_pos: Option<egui::Pos2>,
    ) {
        // Drag start: edge-first detection for trim, then clip-body for move.
        // Uses press_origin (where user first clicked) instead of current pointer pos,
        // because egui delays drag_started() until pointer moves ~4px from click point.
        if track_response.drag_started() {
            let decision_pos = self.press_origin.or(pointer_pos);
            if let Some(pos) = decision_pos {
                // Zero: check if pointer is near a fade handle (top corner of audio clip)
                let mut started_fade_drag = false;
                if state.active_tool == Tool::Select {
                    for clip in clips {
                        if clip.kind.is_gap() { continue; }
                        let clip_has_audio = track_kind == TrackKind::Audio
                            || clip.media_id().and_then(|mid| state.media_pool.get(&mid)).is_some_and(|m| m.has_audio);
                        if !clip_has_audio { continue; }
                        let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
                        if cr.width() < 20.0 { continue; }

                        // Resolve current fade values
                        let (cur_fi, cur_fo) = if let Some(linked_id) = clip.linked_clip_id {
                            let mut fi = clip.fade_in_secs;
                            let mut fo = clip.fade_out_secs;
                            for t in &state.sequence().tracks {
                                if let Some(vc) = t.find_clip(linked_id) {
                                    fi = vc.fade_in_secs;
                                    fo = vc.fade_out_secs;
                                    break;
                                }
                            }
                            (fi, fo)
                        } else {
                            (clip.fade_in_secs, clip.fade_out_secs)
                        };
                        let clip_dur_secs = clip.duration().to_secs_f64() as f32;
                        let fi_offset_px = if clip_dur_secs > 0.001 { (cur_fi / clip_dur_secs) * cr.width() } else { 0.0 };
                        let fi_x = cr.left() + fi_offset_px;
                        let fo_offset_px = if clip_dur_secs > 0.001 { (cur_fo / clip_dur_secs) * cr.width() } else { 0.0 };
                        let fo_x = cr.right() - fo_offset_px;

                        let near_fi = (pos.x - fi_x).abs() < 8.0 && (pos.y - cr.top()).abs() < 10.0
                            && cr.contains(pos) && content_rect.contains(pos);
                        let near_fo = (pos.x - fo_x).abs() < 8.0 && (pos.y - cr.top()).abs() < 10.0
                            && cr.contains(pos) && content_rect.contains(pos);

                        if near_fi {
                            // Resolve the clip_id for the UpdateClipAudio command
                            let cmd_clip_id = clip.linked_clip_id.unwrap_or(clip.id);
                            // Emit BeginAudioDrag for undo tracking
                            let (g, v, p, fi_s, fo_s) = if let Some(linked_id) = clip.linked_clip_id {
                                state.sequence().tracks.iter()
                                    .find_map(|t| t.find_clip(linked_id))
                                    .map(|c| (c.gain, c.volume, c.pan, c.fade_in_secs, c.fade_out_secs))
                                    .unwrap_or((1.0, 1.0, 0.0, cur_fi, cur_fo))
                            } else {
                                (clip.gain, clip.volume, clip.pan, cur_fi, cur_fo)
                            };
                            cmds.push(AppCommand::BeginAudioDrag {
                                key: format!("fade_handle_{:?}", cmd_clip_id),
                                clip_id: cmd_clip_id,
                                old_gain: g, old_volume: v, old_pan: p,
                                old_fade_in: fi_s, old_fade_out: fo_s,
                            });
                            state.dragging_fade_handle = Some((clip.id, true, cur_fi));
                            started_fade_drag = true;
                            break;
                        }
                        if near_fo {
                            let cmd_clip_id = clip.linked_clip_id.unwrap_or(clip.id);
                            let (g, v, p, fi_s, fo_s) = if let Some(linked_id) = clip.linked_clip_id {
                                state.sequence().tracks.iter()
                                    .find_map(|t| t.find_clip(linked_id))
                                    .map(|c| (c.gain, c.volume, c.pan, c.fade_in_secs, c.fade_out_secs))
                                    .unwrap_or((1.0, 1.0, 0.0, cur_fi, cur_fo))
                            } else {
                                (clip.gain, clip.volume, clip.pan, cur_fi, cur_fo)
                            };
                            cmds.push(AppCommand::BeginAudioDrag {
                                key: format!("fade_handle_{:?}", cmd_clip_id),
                                clip_id: cmd_clip_id,
                                old_gain: g, old_volume: v, old_pan: p,
                                old_fade_in: fi_s, old_fade_out: fo_s,
                            });
                            state.dragging_fade_handle = Some((clip.id, false, cur_fo));
                            started_fade_drag = true;
                            break;
                        }
                    }
                }
                // Check for roll trim junction before regular trim
                let mut started_roll_trim = false;
                if !started_fade_drag && state.active_tool == Tool::Select && clips.len() >= 2 {
                    let roll_threshold = 4.0_f32;
                    for i in 0..clips.len() - 1 {
                        if clips[i].kind.is_gap() || clips[i + 1].kind.is_gap() { continue; }
                        let cr_a = Self::clip_screen_rect(&clips[i], content_rect, track_rect, zoom, scroll, track_height);
                        let cr_b = Self::clip_screen_rect(&clips[i + 1], content_rect, track_rect, zoom, scroll, track_height);
                        let junction_x = (cr_a.right() + cr_b.left()) / 2.0;
                        if (pos.x - junction_x).abs() < roll_threshold
                            && pos.y >= track_rect.top() && pos.y <= track_rect.top() + track_height
                        {
                            // Verify clips are truly adjacent (no gap)
                            let gap_secs = (clips[i + 1].timeline_in - clips[i].timeline_out).to_secs_f64().abs();
                            if gap_secs < 0.001 {
                                tracing::debug!("drag_started ROLL TRIM: clip_a={:?}, clip_b={:?}", clips[i].id, clips[i + 1].id);
                                self.roll_trimming = Some(RollTrimState {
                                    clip_a_id: clips[i].id,
                                    clip_b_id: clips[i + 1].id,
                                    track_idx,
                                    start_x: pos.x,
                                    original_boundary: clips[i].timeline_out,
                                    original_a_source_out: clips[i].source_out,
                                    original_b_source_in: clips[i + 1].source_in,
                                    original_a_timeline_out: clips[i].timeline_out,
                                    original_b_timeline_in: clips[i + 1].timeline_in,
                                });
                                started_roll_trim = true;
                                break;
                            }
                        }
                    }
                }
                // First: check if pointer is near any clip edge (trim)
                if !started_fade_drag && !started_roll_trim {
                if let Some((_cr, hit_clip, edge)) = self.hit_test_edge(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    tracing::debug!("drag_started TRIM: clip={:?}, edge={:?}", hit_clip.id, match edge { TrimEdge::Left => "left", TrimEdge::Right => "right" });
                    // Collect subsequent clips for ripple trim
                    let subsequent = if state.ripple_enabled {
                        clips.iter()
                            .filter(|c| c.id != hit_clip.id && c.timeline_in >= hit_clip.timeline_in)
                            .map(|c| (c.id, c.timeline_in, c.timeline_out))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    self.trimming_clip = Some(TrimState {
                        clip_id: hit_clip.id,
                        track_idx,
                        edge,
                        start_x: pos.x,
                        original_source_in: hit_clip.source_in,
                        original_source_out: hit_clip.source_out,
                        original_timeline_in: hit_clip.timeline_in,
                        original_timeline_out: hit_clip.timeline_out,
                        subsequent_clips: subsequent,
                    });
                }
                // Range tool: drag anywhere in track area sets range
                else if state.active_tool == Tool::Range {
                    let x_in_content = pos.x - content_rect.left() + scroll;
                    let anchor_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                    state.range_drag = Some(crate::state::RangeDragState {
                        kind: crate::state::RangeDragKind::RulerCreate { anchor_secs },
                        original_in: state.sequence().in_point,
                        original_out: state.sequence().out_point,
                    });
                }
                // Second: check if pointer is inside a clip body (drag/move or slip/slide)
                else if let Some((_cr, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    if state.active_tool == Tool::Select {
                        let alt_held = ui.input(|i| i.modifiers.alt);
                        let shift_held = ui.input(|i| i.modifiers.shift);

                        if alt_held && shift_held {
                            // ALT+SHIFT+drag on clip body = SLIDE
                            tracing::debug!("drag_started SLIDE: clip={:?}", hit_clip.id);
                            // Find left and right neighbor clips on the same track
                            let clip_idx = clips.iter().position(|c| c.id == hit_clip.id);
                            let left = clip_idx.and_then(|i| {
                                if i > 0 {
                                    let lc = &clips[i - 1];
                                    if !lc.kind.is_gap() { Some(lc) } else { None }
                                } else { None }
                            });
                            let right = clip_idx.and_then(|i| {
                                if i + 1 < clips.len() {
                                    let rc = &clips[i + 1];
                                    if !rc.kind.is_gap() { Some(rc) } else { None }
                                } else { None }
                            });
                            let linked_id = hit_clip.linked_clip_id;
                            let (linked_orig_in, linked_orig_out) = linked_id
                                .and_then(|lid| {
                                    state.sequence().tracks.iter()
                                        .flat_map(|t| t.clips.iter())
                                        .find(|c| c.id == lid)
                                        .map(|c| (c.timeline_in, c.timeline_out))
                                })
                                .map(|(a, b)| (Some(a), Some(b)))
                                .unwrap_or((None, None));
                            self.sliding_clip = Some(SlideState {
                                clip_id: hit_clip.id,
                                track_idx,
                                start_x: pos.x,
                                original_timeline_in: hit_clip.timeline_in,
                                original_timeline_out: hit_clip.timeline_out,
                                left_clip_id: left.map(|c| c.id),
                                original_left_out: left.map(|c| c.timeline_out),
                                original_left_source_out: left.map(|c| c.source_out),
                                right_clip_id: right.map(|c| c.id),
                                original_right_in: right.map(|c| c.timeline_in),
                                original_right_source_in: right.map(|c| c.source_in),
                                linked_clip_id: linked_id,
                                linked_original_in: linked_orig_in,
                                linked_original_out: linked_orig_out,
                            });
                        } else if alt_held {
                            // ALT+drag on clip body = SLIP
                            tracing::debug!("drag_started SLIP: clip={:?}", hit_clip.id);
                            let media_dur = hit_clip.media_id().and_then(|mid| {
                                state.media_pool.get(&mid).map(|m| m.duration)
                            });
                            let linked_id = hit_clip.linked_clip_id;
                            let (linked_src_in, linked_src_out) = linked_id
                                .and_then(|lid| {
                                    state.sequence().tracks.iter()
                                        .flat_map(|t| t.clips.iter())
                                        .find(|c| c.id == lid)
                                        .map(|c| (c.source_in, c.source_out))
                                })
                                .map(|(a, b)| (Some(a), Some(b)))
                                .unwrap_or((None, None));
                            self.slipping_clip = Some(SlipState {
                                clip_id: hit_clip.id,
                                track_idx,
                                start_x: pos.x,
                                original_source_in: hit_clip.source_in,
                                original_source_out: hit_clip.source_out,
                                media_duration: media_dur,
                                linked_clip_id: linked_id,
                                linked_original_source_in: linked_src_in,
                                linked_original_source_out: linked_src_out,
                            });
                        } else {
                            // Normal drag/move
                            tracing::debug!("drag_started MOVE: clip={:?}", hit_clip.id);
                            let linked_id = hit_clip.linked_clip_id;
                            let linked_orig = linked_id.and_then(|lid| {
                                state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == lid)
                                    .map(|c| c.timeline_in)
                            });
                            self.dragging_clip = Some(DragState {
                                clip_id: hit_clip.id,
                                track_idx,
                                start_x: pos.x,
                                original_timeline_in: hit_clip.timeline_in,
                                linked_clip_id: linked_id,
                                linked_original_in: linked_orig,
                            });
                        }
                    }
                }
                } // end if !started_fade_drag
            }
        }
    }

    /// Handle click on clips: blade, range select, clip selection, deselect, compound clip double-click.
    #[allow(clippy::too_many_arguments)]
    fn handle_clip_click(
        &mut self,
        ui: &egui::Ui,
        track_response: &egui::Response,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        fps: klipp_core::Fps,
    ) {
        // Click handling
        if track_response.clicked() {
            if let Some(pos) = track_response.interact_pointer_pos {
                let x_in_content = pos.x - content_rect.left() + scroll;
                let click_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                let click_tc = RationalTime::from_secs_f64(click_secs, fps.rate());

                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    if state.active_tool == Tool::Blade {
                        // Snap blade position when snap is enabled (hold Shift to override)
                        let mut cut_secs = click_secs;
                        let blade_snap = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
                        if blade_snap {
                            let snap_thresh_secs = (snap_threshold_px() * 2.0) as f64 / zoom as f64;
                            let mut best_dist = f64::MAX;
                            // Playhead
                            let ph_secs = state.playhead.to_secs_f64();
                            let d = (click_secs - ph_secs).abs();
                            if d < snap_thresh_secs && d < best_dist {
                                best_dist = d;
                                cut_secs = ph_secs;
                            }
                            // Clip edges
                            for track in &state.sequence().tracks {
                                for c in &track.clips {
                                    for &edge_secs in &[c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()] {
                                        let d = (click_secs - edge_secs).abs();
                                        if d < snap_thresh_secs && d < best_dist {
                                            best_dist = d;
                                            cut_secs = edge_secs;
                                        }
                                    }
                                }
                            }
                            // In/out markers
                            if let Some(inp) = state.sequence().in_point {
                                let d = (click_secs - inp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    best_dist = d;
                                    cut_secs = inp.to_secs_f64();
                                }
                            }
                            if let Some(outp) = state.sequence().out_point {
                                let d = (click_secs - outp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    cut_secs = outp.to_secs_f64();
                                }
                            }
                            let _ = best_dist;
                        }
                        let cut_tc = RationalTime::from_secs_f64(cut_secs, fps.rate());
                        cmds.push(AppCommand::BladeClip {
                            track_index: track_idx,
                            clip_id: hit_clip.id,
                            cut_at: cut_tc,
                        });
                    } else if state.active_tool == Tool::Range {
                        // Range tool: click on clip = set range to clip boundaries
                        cmds.push(AppCommand::SetSequenceInPoint(hit_clip.timeline_in));
                        cmds.push(AppCommand::SetSequenceOutPoint(hit_clip.timeline_out));
                        state.toast.show("Range: clip selected");
                    } else {
                        let modifiers = ui.input(|i| i.modifiers);
                        if modifiers.shift || modifiers.ctrl || modifiers.command {
                            // Multi-select: toggle in selected_clips
                            if let Some(pos) = state.selected_clips.iter().position(|&id| id == hit_clip.id) {
                                state.selected_clips.remove(pos);
                            } else {
                                state.selected_clips.push(hit_clip.id);
                            }
                        } else {
                            // If clip is grouped, select all group members
                            if let Some(gid) = hit_clip.group_id {
                                state.selected_clips = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .filter(|c| c.group_id == Some(gid))
                                    .map(|c| c.id)
                                    .collect();
                            } else {
                                state.selected_clips.clear();
                            }
                        }
                        cmds.push(AppCommand::SelectClip(Some(hit_clip.id)));
                        // Clear media browser selection so inspector shows timeline clip (State 3)
                        state.selected_media_ids.clear();
                    }
                } else {
                    // Clicked empty area — move playhead and deselect
                    cmds.push(AppCommand::SeekTo(click_tc));
                    if self.dragging_clip.is_none() && self.trimming_clip.is_none() {
                        state.selected_clips.clear();
                        cmds.push(AppCommand::SelectClip(None));
                    }
                }
            }
        }

        // Double-click: open compound clips
        if track_response.double_clicked() {
            if let Some(pos) = track_response.interact_pointer_pos {
                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    if matches!(hit_clip.kind, klipp_timeline::ClipKind::Sequence { .. }) {
                        cmds.push(AppCommand::OpenCompoundClip { clip_id: hit_clip.id });
                    }
                }
            }
        }
    }

    /// Right-click context menu on clips and empty track space.
    #[allow(clippy::too_many_arguments)]
    fn show_clip_context_menu(
        &mut self,
        _ui: &mut egui::Ui,
        track_response: &egui::Response,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        track_rect: &egui::Rect,
        pointer_pos: Option<egui::Pos2>,
        fps: klipp_core::Fps,
    ) {
        // Right-click context menu on clips (suppressed when keyframe context menu was opened)
        if track_response.secondary_clicked() && !self.suppress_clip_context_menu {
            if let Some(pos) = pointer_pos {
                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    // Store the clip id so the context menu can use it
                    cmds.push(AppCommand::SelectClip(Some(hit_clip.id)));
                }
            }
        }
        track_response.context_menu(|ui| {
            ui.set_min_width(160.0);
            if let Some(clip_id) = state.selected_clip {
                // ── Title/Text clip actions ──
                let title_data_cloned: Option<klipp_timeline::TitleData> = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .and_then(|c| c.title_data())
                    .cloned();
                if let Some(ref td) = title_data_cloned {
                    if ui.button("\u{270F} Edit Text").clicked() {
                        state.viewer_text_edit = true;
                        state.viewer_text_edit_clip = Some(clip_id);
                        state.viewer_text_edit_request_focus = true;
                        state.viewer_char_positions.clear();
                        state.viewer_text_cursor = 0;
                        state.viewer_text_select_anchor = None;
                        state.viewer_text_edit_buffer = td.text.clone();
                        cmds.push(AppCommand::SelectClip(Some(clip_id)));
                        ui.close_menu();
                    }
                    {
                        // Alignment submenu
                        let current_align = &td.alignment;
                        ui.menu_button(format!("\u{2630} Alignment: {}", match current_align {
                            klipp_timeline::TextAlignment::Left => "Left",
                            klipp_timeline::TextAlignment::Center => "Center",
                            klipp_timeline::TextAlignment::Right => "Right",
                        }), |ui| {
                            if ui.selectable_label(*current_align == klipp_timeline::TextAlignment::Left, "Left").clicked() {
                                cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: 0 });
                                ui.close_menu();
                            }
                            if ui.selectable_label(*current_align == klipp_timeline::TextAlignment::Center, "Center").clicked() {
                                cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: 1 });
                                ui.close_menu();
                            }
                            if ui.selectable_label(*current_align == klipp_timeline::TextAlignment::Right, "Right").clicked() {
                                cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: 2 });
                                ui.close_menu();
                            }
                        });
                        // Font style submenu
                        let is_bold = matches!(td.font_style, klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic);
                        let is_italic = matches!(td.font_style, klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic);
                        ui.menu_button("\u{1D5D4} Font Style", |ui| {
                            if ui.selectable_label(!is_bold && !is_italic, "Regular").clicked() {
                                cmds.push(AppCommand::UpdateTitleData {
                                    clip_id, text: td.text.clone(), font_size: td.font_size,
                                    font_name: td.font_name.clone(), font_style: 0, color: td.color,
                                });
                                ui.close_menu();
                            }
                            if ui.selectable_label(is_bold && !is_italic, "Bold").clicked() {
                                cmds.push(AppCommand::UpdateTitleData {
                                    clip_id, text: td.text.clone(), font_size: td.font_size,
                                    font_name: td.font_name.clone(), font_style: 1, color: td.color,
                                });
                                ui.close_menu();
                            }
                            if ui.selectable_label(!is_bold && is_italic, "Italic").clicked() {
                                cmds.push(AppCommand::UpdateTitleData {
                                    clip_id, text: td.text.clone(), font_size: td.font_size,
                                    font_name: td.font_name.clone(), font_style: 2, color: td.color,
                                });
                                ui.close_menu();
                            }
                            if ui.selectable_label(is_bold && is_italic, "Bold Italic").clicked() {
                                cmds.push(AppCommand::UpdateTitleData {
                                    clip_id, text: td.text.clone(), font_size: td.font_size,
                                    font_name: td.font_name.clone(), font_style: 3, color: td.color,
                                });
                                ui.close_menu();
                            }
                        });
                        // Reset styles (clear all style runs)
                        if !td.style_runs.is_empty()
                            && ui.button("\u{21BA} Reset Text Styles").clicked() {
                                cmds.push(AppCommand::ApplyTitleStyle {
                                    clip_id, range_start: 0, range_end: td.text.len(),
                                    property: "clear_all".into(), value: String::new(),
                                });
                                ui.close_menu();
                            }
                    } // end alignment/style block
                    ui.separator();
                } // end title_data_cloned

                if ui.button("Remove Clip").clicked() {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                    ui.close_menu();
                }
                if ui.button("Duplicate Clip  Ctrl+D").clicked() {
                    cmds.push(AppCommand::DuplicateClip { clip_id });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Copy Attributes  Ctrl+Shift+C").clicked() {
                    cmds.push(AppCommand::CopyAttributes { clip_id });
                    ui.close_menu();
                }
                if ui.add_enabled(state.attributes_clipboard.is_some(), egui::Button::new("Paste Attributes...  Ctrl+Shift+V")).clicked() {
                    state.show_paste_attributes_dialog = true;
                    ui.close_menu();
                }
                // Compound clip operations
                let is_compound = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .is_some_and(|c| matches!(c.kind, klipp_timeline::ClipKind::Sequence { .. }));
                if is_compound
                    && ui.button("Open Compound Clip").clicked() {
                        cmds.push(AppCommand::OpenCompoundClip { clip_id });
                        ui.close_menu();
                    }
                let has_multi_for_compound = state.selected_clips.len() >= 2
                    || (state.selected_clip.is_some() && !state.selected_clips.is_empty());
                if ui.add_enabled(has_multi_for_compound, egui::Button::new("Create Compound Clip...")).clicked() {
                    cmds.push(AppCommand::CreateCompoundClip { name: "Compound Clip".into() });
                    ui.close_menu();
                }
                if ui.button("Create Multicam Clip").clicked() {
                    cmds.push(AppCommand::CreateMulticamClip);
                    ui.close_menu();
                }
                // Motion presets submenu
                ui.menu_button("Motion Presets", |ui| {
                    for (label, preset) in [
                        ("Ken Burns: Zoom In", "ken_burns_zoom_in"),
                        ("Ken Burns: Zoom Out", "ken_burns_zoom_out"),
                        ("Ken Burns: Pan Left", "ken_burns_pan_left"),
                        ("PiP: Top Right", "pip_top_right"),
                        ("PiP: Bottom Left", "pip_bottom_left"),
                    ] {
                        if ui.button(label).clicked() {
                            cmds.push(AppCommand::ApplyMotionPreset { clip_id, preset: preset.into() });
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                // Enable/Disable clip
                let clip_enabled = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .is_none_or(|c| c.enabled);
                let enable_label = if clip_enabled { "Disable Clip" } else { "Enable Clip" };
                if ui.button(enable_label).clicked() {
                    cmds.push(AppCommand::SetClipEnabled { clip_id, enabled: !clip_enabled });
                    ui.close_menu();
                }

                // Link/Unlink
                let clip_info = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id);
                if let Some(clip) = clip_info {
                    if clip.linked_clip_id.is_some() {
                        if ui.button("Unlink Clip").clicked() {
                            cmds.push(AppCommand::UnlinkClip { clip_id });
                            ui.close_menu();
                        }
                    } else {
                        // Find a candidate clip on another track at the same position
                        let candidate = state.sequence().tracks.iter().flat_map(|t| t.clips.iter())
                            .find(|c| {
                                c.id != clip_id
                                    && c.linked_clip_id.is_none()
                                    && c.timeline_in == clip.timeline_in
                            });
                        if let Some(partner) = candidate {
                            let partner_id = partner.id;
                            if ui.button("Link to Matching Clip").clicked() {
                                cmds.push(AppCommand::LinkClips { clip_a: clip_id, clip_b: partner_id });
                                ui.close_menu();
                            }
                        }
                    }
                }

                // Reveal source in media browser
                if let Some(clip) = clip_info {
                    if let Some(mid) = clip.media_id() {
                        if ui.button("Reveal Source in Browser").clicked() {
                            cmds.push(AppCommand::RevealMediaInBrowser { media_id: mid });
                            ui.close_menu();
                        }
                    }
                }

                ui.separator();

                // Three-point edit operations
                let has_source = state.source_media_id.is_some();
                let has_in_out = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
                if ui.add_enabled(has_source, egui::Button::new("Insert (,)")).clicked() {
                    cmds.push(AppCommand::InsertEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_source, egui::Button::new("Overwrite (.)")).clicked() {
                    cmds.push(AppCommand::OverwriteEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_in_out, egui::Button::new("Lift (;)")).clicked() {
                    cmds.push(AppCommand::LiftEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_in_out, egui::Button::new("Extract (')")).clicked() {
                    cmds.push(AppCommand::ExtractEdit);
                    ui.close_menu();
                }

                // Magnetic operations
                ui.separator();
                if ui.button("Ripple Trim Head to Playhead  Alt+[").clicked() {
                    cmds.push(AppCommand::RippleTrimHead { clip_id });
                    ui.close_menu();
                }
                if ui.button("Ripple Trim Tail to Playhead  Alt+]").clicked() {
                    cmds.push(AppCommand::RippleTrimTail { clip_id });
                    ui.close_menu();
                }
                if ui.button("Insert Gap (1s)").clicked() {
                    cmds.push(AppCommand::InsertGapAtPlayhead);
                    ui.close_menu();
                }

                // Transitions
                ui.separator();
                // Find adjacent clip pairs involving the selected clip
                if let Some(track) = state.sequence().tracks.get(track_idx) {
                    let clip_pos = track.clips.iter().position(|c| c.id == clip_id);
                    if let Some(pos) = clip_pos {
                        // Check if there's a clip after this one (transition at the cut point)
                        if pos + 1 < track.clips.len() {
                            let next_clip = &track.clips[pos + 1];
                            let has_transition = track.transitions.iter().any(|t| {
                                t.clip_a_id == clip_id && t.clip_b_id == next_clip.id
                            });
                            let next_id = next_clip.id;
                            if has_transition {
                                // Show existing transition with option to change/remove
                                let tid = track.transitions.iter()
                                    .find(|t| t.clip_a_id == clip_id && t.clip_b_id == next_id)
                                    .map(|t| t.id);
                                if let Some(tid) = tid {
                                    if ui.button("Remove Transition →").clicked() {
                                        cmds.push(AppCommand::RemoveTransition {
                                            track_index: track_idx,
                                            transition_id: tid,
                                        });
                                        ui.close_menu();
                                    }
                                }
                            } else {
                                ui.menu_button("Add Transition →", |ui| {
                                    for &(kind_id, label) in klipp_engine::transition::all_transition_kinds() {
                                        if ui.button(label).clicked() {
                                            cmds.push(AppCommand::AddTransition {
                                                track_index: track_idx,
                                                clip_a_id: clip_id,
                                                clip_b_id: next_id,
                                                kind_id,
                                                duration_frames: state.default_transition_frames,
                                            });
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        }
                        // Check if there's a clip before this one
                        if pos > 0 {
                            let prev_clip = &track.clips[pos - 1];
                            let has_transition = track.transitions.iter().any(|t| {
                                t.clip_a_id == prev_clip.id && t.clip_b_id == clip_id
                            });
                            let prev_id = prev_clip.id;
                            if has_transition {
                                let tid = track.transitions.iter()
                                    .find(|t| t.clip_a_id == prev_id && t.clip_b_id == clip_id)
                                    .map(|t| t.id);
                                if let Some(tid) = tid {
                                    if ui.button("Remove Transition ←").clicked() {
                                        cmds.push(AppCommand::RemoveTransition {
                                            track_index: track_idx,
                                            transition_id: tid,
                                        });
                                        ui.close_menu();
                                    }
                                }
                            } else {
                                ui.menu_button("Add Transition ←", |ui| {
                                    for &(kind_id, label) in klipp_engine::transition::all_transition_kinds() {
                                        if ui.button(label).clicked() {
                                            cmds.push(AppCommand::AddTransition {
                                                track_index: track_idx,
                                                clip_a_id: prev_id,
                                                clip_b_id: clip_id,
                                                kind_id,
                                                duration_frames: state.default_transition_frames,
                                            });
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        }
                    }
                }

                ui.separator();

                // Group / Ungroup
                let has_multi = state.selected_clips.len() >= 2
                    || (state.selected_clips.len() == 1 && state.selected_clip.is_some_and(|id| !state.selected_clips.contains(&id)));
                if ui.add_enabled(has_multi, egui::Button::new("Group").shortcut_text("Ctrl+G")).clicked() {
                    let mut ids = state.selected_clips.clone();
                    if let Some(primary) = state.selected_clip {
                        if !ids.contains(&primary) {
                            ids.push(primary);
                        }
                    }
                    cmds.push(AppCommand::GroupClips { clip_ids: ids });
                    ui.close_menu();
                }
                let clip_group = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| Some(c.id) == state.selected_clip)
                    .and_then(|c| c.group_id);
                if ui.add_enabled(clip_group.is_some(), egui::Button::new("Ungroup").shortcut_text("Ctrl+Shift+G")).clicked() {
                    if let Some(gid) = clip_group {
                        cmds.push(AppCommand::UngroupClips { group_id: gid });
                    }
                    ui.close_menu();
                }

                // Magnetic connect/disconnect
                ui.separator();
                let is_on_primary = state.sequence().tracks.get(track_idx)
                    .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);
                if !is_on_primary {
                    let clip_data = state.sequence().tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id);
                    if let Some(cd) = clip_data {
                        if cd.magnetic_anchor.is_some() {
                            ui.label("\u{26D3} Connected to Primary");
                            if ui.button("Disconnect (Shift+G)").clicked() {
                                ui.close_menu();
                            }
                        } else if ui.button("Connect to Primary (G)").clicked() {
                            ui.close_menu();
                        }
                    }
                    // Storyline promotion
                    let track_role = state.sequence().tracks.get(track_idx)
                        .map(|t| t.magnetic_role.clone());
                    if let Some(role) = track_role {
                        if role == klipp_timeline::TrackRole::Connected {
                            if ui.button("Promote to Storyline (Shift+S)").clicked() {
                                ui.close_menu();
                            }
                        } else if role == klipp_timeline::TrackRole::ConnectedStoryline {
                            ui.label("\u{1F517} Connected Storyline");
                            if ui.button("Break Apart Storyline (Shift+S)").clicked() {
                                ui.close_menu();
                            }
                        }
                    }
                }

                // Collision resolution (connected/storyline tracks only)
                if !is_on_primary {
                    let collision = state.sequence().tracks.get(track_idx).and_then(|track| {
                        let clip = track.clips.iter().find(|c| c.id == clip_id)?;
                        // Find overlapping clip on same track
                        track.clips.iter().find(|other| {
                            other.id != clip_id
                                && !other.kind.is_gap()
                                && clip.timeline_in < other.timeline_out
                                && clip.timeline_out > other.timeline_in
                        }).map(|other| other.id)
                    });
                    if let Some(colliding_id) = collision {
                        ui.separator();
                        if ui.button("Push to Resolve").clicked() {
                            let result = klipp_timeline::magnetic::MagneticEditor::resolve_collision_push(
                                state.sequence_mut(), clip_id, colliding_id);
                            state.history.push(crate::history::HistoryEntry {
                                sequence_idx: state.active_sequence_idx,
                                command: crate::history::EditCommand::Magnetic(result.command),
                            });
                            state.dirty = true;
                            cmds.push(AppCommand::SequenceUpdated);
                            ui.close_menu();
                        }
                    }
                }

                // Lock Track
                ui.separator();
                let track_locked = state.sequence().tracks.get(track_idx).is_some_and(|t| t.locked);
                let lock_label = if track_locked { "Unlock Track" } else { "Lock Track" };
                if ui.button(lock_label).clicked() {
                    let track_id = state.sequence().tracks[track_idx].id;
                    cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                    ui.close_menu();
                }

                // Track height
                let has_custom_h = state.sequence().tracks.get(track_idx)
                    .is_some_and(|t| t.custom_height.is_some());
                if has_custom_h
                    && ui.button("Reset Track Height").clicked() {
                        let track_id = state.sequence().tracks[track_idx].id;
                        cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: None });
                        ui.close_menu();
                    }

                ui.separator();
                let r = ui.add_enabled(false, egui::Button::new("Properties..."));
                r.on_disabled_hover_text("(coming soon)");
            } else {
                // No clip selected — offer gap operations on empty track space
                // Find gap at click position on this track
                let click_tc = if let Some(pos) = pointer_pos {
                    let x_in_content = pos.x - content_rect.left() + scroll;
                    let click_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                    Some(RationalTime::from_secs_f64(click_secs, fps.rate()))
                } else {
                    None
                };
                if let Some(tc) = click_tc {
                    if let Some(track) = state.sequence().tracks.get(track_idx) {
                        // Find gap around the clicked position
                        let mut clips_sorted: Vec<_> = track.clips.iter()
                            .filter(|c| !matches!(c.kind, klipp_timeline::ClipKind::Gap))
                            .collect();
                        clips_sorted.sort_by_key(|c| c.timeline_in);
                        // Look for a gap between clips that contains `tc`
                        let mut gap_found = None;
                        if clips_sorted.is_empty() {
                            // Empty track — no gap to close
                        } else {
                            // Check gap before first clip
                            let zero = RationalTime::from_secs_f64(0.0, fps.rate());
                            if clips_sorted[0].timeline_in > zero && tc < clips_sorted[0].timeline_in {
                                gap_found = Some((zero, clips_sorted[0].timeline_in));
                            }
                            // Check gaps between clips
                            for pair in clips_sorted.windows(2) {
                                let end_a = pair[0].timeline_out;
                                let start_b = pair[1].timeline_in;
                                if start_b > end_a && tc >= end_a && tc < start_b {
                                    gap_found = Some((end_a, start_b));
                                    break;
                                }
                            }
                        }
                        if let Some((gap_start, gap_end)) = gap_found {
                            let dur_secs = (gap_end - gap_start).to_secs_f64();
                            if ui.button(format!("Close Gap ({:.2}s)", dur_secs)).clicked() {
                                cmds.push(AppCommand::CloseGap {
                                    track_index: track_idx,
                                    gap_start,
                                    gap_end,
                                });
                                ui.close_menu();
                            }
                        }
                    }
                }
            }
        });
    }

    /// Process ongoing clip drag (magnetic and legacy free move, ghost rendering, cross-track, auto-connect).
    #[allow(clippy::too_many_arguments)]
    fn process_clip_drag(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        fps: klipp_core::Fps,
        snap_enabled: bool,
        snap_threshold_secs: f64,
        snap_targets: &[(f64, RationalTime, Option<ClipId>)],
        track_height: f32,
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        let find_snap = |edge_time: RationalTime, exclude_clip: ClipId| -> Option<(RationalTime, f64)> {
            Self::find_snap_target(snap_enabled, snap_threshold_secs, snap_targets, edge_time, exclude_clip)
        };

        // Handle ongoing drag (only process once, on the drag's source track)
        if let Some(ref drag) = self.dragging_clip {
            if drag.track_idx != track_idx {
                // Skip — will be handled when we draw the correct track
            } else if let Some(pos) = pointer_pos {
                // Determine which track the pointer is hovering over
                let hover_track_idx = self.frame_cache.track_rects.iter()
                    .find(|(_, _, rect)| rect.contains(pos))
                    .map(|(idx, _, _)| *idx);

                if pointer_down {
                    let dx = pos.x - drag.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());
                    let new_timeline_in = {
                        let sum = drag.original_timeline_in + dt_frames;
                        if sum.value < 0 {
                            RationalTime { value: 0, rate: sum.rate }
                        } else {
                            sum
                        }
                    };

                    let mut final_in = new_timeline_in;
                    let mut snapped = false;

                    if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                        if let Some(clip) = track.find_clip(drag.clip_id) {
                            let dur = clip.duration();
                            let new_out = new_timeline_in + dur;

                            // Snap left edge
                            if let Some((snap_target, snap_secs)) = find_snap(new_timeline_in, drag.clip_id) {
                                final_in = snap_target;
                                snapped = true;
                                self.frame_cache.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                            }
                            // Snap right edge (if no left-edge snap found, or if right is closer)
                            if let Some((snap_target, snap_secs)) = find_snap(new_out, drag.clip_id) {
                                let right_snap_in = snap_target - dur;
                                // Prefer whichever snap is closer in pixels
                                let left_dist = if snapped {
                                    ((final_in.to_secs_f64() - new_timeline_in.to_secs_f64()).abs() * zoom as f64) as f32
                                } else {
                                    f32::MAX
                                };
                                let right_dist = ((snap_target.to_secs_f64() - new_out.to_secs_f64()).abs() * zoom as f64) as f32;
                                if !snapped || right_dist < left_dist {
                                    final_in = right_snap_in;
                                    snapped = true;
                                    self.frame_cache.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }
                            }
                        }
                    }

                    // Clamp to >= 0
                    if final_in.value < 0 {
                        final_in.value = 0;
                    }

                    let is_primary = state.sequence().tracks.get(drag.track_idx)
                        .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);

                    if is_primary {
                        // Alt override: temporarily treat as Overwrite when Alt is held
                        let alt_held = ui.input(|i| i.modifiers.alt);
                        let effective_mode = if alt_held {
                            klipp_timeline::magnetic::EditMode::Overwrite
                        } else {
                            state.edit_mode
                        };

                        // PRIMARY TRACK: Don't move clip in sequence.
                        // Keep it at original position. Draw a floating ghost + insertion indicator.
                        let clip_dur = state.sequence().tracks.get(drag.track_idx)
                            .and_then(|t| t.find_clip(drag.clip_id))
                            .map(|c| c.duration().to_secs_f64() as f32)
                            .unwrap_or(1.0);

                        // Ghost clip following cursor (drawn on tooltip layer so it's above everything)
                        let ghost_x = content_rect.left() + final_in.to_secs_f64() as f32 * zoom - scroll;
                        let ghost_w = clip_dur * zoom;
                        let ghost_rect = egui::Rect::from_min_size(
                            egui::pos2(ghost_x, track_rect.top() + 2.0),
                            egui::vec2(ghost_w, track_rect.height() - 4.0),
                        );

                        // Draw ghost with 50% opacity
                        let ghost_fill = egui::Color32::from_rgba_unmultiplied(60, 80, 140, 128);
                        let ghost_border = egui::Color32::from_rgba_unmultiplied(120, 160, 240, 180);
                        painter.rect_filled(ghost_rect, 3.0, ghost_fill);
                        painter.rect_stroke(ghost_rect, 3.0, egui::Stroke::new(2.0, ghost_border));

                        // Draw filmstrip in ghost if available
                        if let Some(media_id) = state.sequence().tracks.get(drag.track_idx)
                            .and_then(|t| t.find_clip(drag.clip_id))
                            .and_then(|c| c.media_id())
                        {
                            if let Some(tex) = state.thumbnail_textures.get(&media_id) {
                                let thumb_painter = painter.with_clip_rect(ghost_rect);
                                thumb_painter.image(
                                    tex.id(),
                                    ghost_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 128),
                                );
                            }
                        }

                        // Insertion indicator: find where the clip would land between existing clips
                        let target_secs = final_in.to_secs_f64();
                        if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                            // Find insertion point (between which clips)
                            let mut insert_x = content_rect.left() - scroll; // before all clips
                            for c in &track.clips {
                                if c.id == drag.clip_id { continue; }
                                let c_mid = (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0;
                                if target_secs > c_mid {
                                    insert_x = content_rect.left() + c.timeline_out.to_secs_f64() as f32 * zoom - scroll;
                                }
                            }
                            // Draw insertion line — amber for Insert, blue for Overwrite
                            let insert_color = match effective_mode {
                                klipp_timeline::magnetic::EditMode::Insert => egui::Color32::from_rgb(232, 160, 32),
                                klipp_timeline::magnetic::EditMode::Overwrite => egui::Color32::from_rgb(60, 120, 220),
                            };
                            painter.line_segment(
                                [egui::pos2(insert_x, track_rect.top()), egui::pos2(insert_x, track_rect.bottom())],
                                egui::Stroke::new(3.0, insert_color),
                            );
                            // Small triangles at top and bottom of insertion line
                            let tri_size = 5.0;
                            painter.add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(insert_x, track_rect.top()),
                                    egui::pos2(insert_x - tri_size, track_rect.top() - tri_size),
                                    egui::pos2(insert_x + tri_size, track_rect.top() - tri_size),
                                ],
                                insert_color, egui::Stroke::NONE,
                            ));
                            painter.add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(insert_x, track_rect.bottom()),
                                    egui::pos2(insert_x - tri_size, track_rect.bottom() + tri_size),
                                    egui::pos2(insert_x + tri_size, track_rect.bottom() + tri_size),
                                ],
                                insert_color, egui::Stroke::NONE,
                            ));
                        }

                        // Drop zone highlight for primary track — amber glow at ghost position
                        if snapped {
                            let drop_rect = egui::Rect::from_min_size(
                                egui::pos2(ghost_x - 1.0, track_rect.top() + 1.0),
                                egui::vec2(ghost_w + 2.0, track_rect.height() - 2.0),
                            );
                            painter.rect_stroke(drop_rect, 4.0,
                                egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)));
                            painter.rect_stroke(drop_rect, 4.0,
                                egui::Stroke::new(1.5, egui::Color32::from_rgb(232, 160, 32)));
                        }

                        // ── Items 11-14: Cascade preview, ripple arrows, connected previews, anchor snap ──
                        if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                            let amber = egui::Color32::from_rgb(232, 160, 32);
                            let drag_clip_in = drag.original_timeline_in;
                            let delta_secs = final_in.to_secs_f64() - drag_clip_in.to_secs_f64();

                            // Item 12: Ripple direction arrows on primary clips in Insert mode
                            if effective_mode == klipp_timeline::magnetic::EditMode::Insert {
                                let insert_secs = final_in.to_secs_f64();
                                for c in &track.clips {
                                    if c.id == drag.clip_id { continue; }
                                    let c_in_secs = c.timeline_in.to_secs_f64();
                                    // Clips to the right of insertion point will ripple right
                                    let will_ripple_right = c_in_secs >= insert_secs && delta_secs > 0.01;
                                    let will_ripple_left = c_in_secs >= drag_clip_in.to_secs_f64() && delta_secs < -0.01 && c_in_secs > insert_secs;
                                    if will_ripple_right || will_ripple_left {
                                        let c_rect = Self::clip_screen_rect(c, content_rect, track_rect, zoom, scroll, track_height);
                                        if c_rect.right() < content_rect.left() || c_rect.left() > content_rect.right() {
                                            continue; // off-screen
                                        }
                                        let c_w = c_rect.width();
                                        let arrow_right = will_ripple_right;
                                        let arrow_h = 6.0_f32;
                                        let arrow_w = 8.0_f32;
                                        let cy = c_rect.center().y;

                                        // Determine number of arrows based on clip width
                                        let arrow_count = if c_w > 40.0 { 3 } else if c_w > 20.0 { 1 } else { 0 };
                                        if arrow_count > 0 {
                                            let spacing = c_w / (arrow_count as f32 + 1.0);
                                            for i in 0..arrow_count {
                                                let ax = c_rect.left() + spacing * (i as f32 + 1.0);
                                                let points = if arrow_right {
                                                    vec![
                                                        egui::pos2(ax - arrow_w / 2.0, cy - arrow_h / 2.0),
                                                        egui::pos2(ax + arrow_w / 2.0, cy),
                                                        egui::pos2(ax - arrow_w / 2.0, cy + arrow_h / 2.0),
                                                    ]
                                                } else {
                                                    vec![
                                                        egui::pos2(ax + arrow_w / 2.0, cy - arrow_h / 2.0),
                                                        egui::pos2(ax - arrow_w / 2.0, cy),
                                                        egui::pos2(ax + arrow_w / 2.0, cy + arrow_h / 2.0),
                                                    ]
                                                };
                                                painter.add(egui::Shape::convex_polygon(
                                                    points,
                                                    egui::Color32::from_rgba_unmultiplied(232, 160, 32, 180),
                                                    egui::Stroke::NONE,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }

                            // Items 11 + 13: Cascade preview — connected clips show preview positions
                            if delta_secs.abs() > 0.001 {
                                for (other_ti, other_track) in state.sequence().tracks.iter().enumerate() {
                                    if other_ti == drag.track_idx { continue; }
                                    let is_connected = other_track.magnetic_role == klipp_timeline::TrackRole::Connected
                                        || other_track.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline;
                                    if !is_connected { continue; }

                                    let other_track_rect = self.frame_cache.track_rects.iter()
                                        .find(|(idx, _, _)| *idx == other_ti)
                                        .map(|(_, _, r)| *r);
                                    let other_track_rect = match other_track_rect {
                                        Some(r) => r,
                                        None => continue,
                                    };
                                    let other_h = effective_track_height(other_track);

                                    for clip in &other_track.clips {
                                        if clip.kind.is_gap() { continue; }
                                        // Check if this clip is anchored to a primary clip that would move
                                        let would_cascade = if other_track.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline {
                                            // Storyline: all clips cascade if any primary anchor moves
                                            other_track.storyline_anchor.as_ref().is_some_and(|sa| {
                                                track.clips.iter().any(|pc| pc.id == sa.anchor_clip_id)
                                            })
                                        } else {
                                            clip.magnetic_anchor.as_ref().is_some_and(|anchor| {
                                                // Cascade if the anchor is on a primary clip that ripples
                                                track.clips.iter().any(|pc| pc.id == anchor.anchor_clip_id)
                                            })
                                        };
                                        if !would_cascade { continue; }

                                        // Compute preview position with cascade delta
                                        let delta_rt = final_in - drag_clip_in;
                                        let preview_in = clip.timeline_in + delta_rt;
                                        let preview_in_secs = preview_in.to_secs_f64();
                                        let clip_dur_secs = clip.duration().to_secs_f64();

                                        // Item 11: Draw connected clip at preview position with 60% opacity
                                        let px = content_rect.left() + preview_in_secs as f32 * zoom - scroll;
                                        let pw = clip_dur_secs as f32 * zoom;
                                        let preview_rect = egui::Rect::from_min_size(
                                            egui::pos2(px, other_track_rect.top() + 2.0),
                                            egui::vec2(pw.max(2.0), other_h - 4.0),
                                        );
                                        if preview_rect.right() >= content_rect.left() && preview_rect.left() <= content_rect.right() {
                                            // 60% opacity fill
                                            painter.rect_filled(preview_rect, 3.0,
                                                egui::Color32::from_rgba_unmultiplied(60, 80, 140, 153));
                                            // Dashed amber border
                                            let dash_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 180);
                                            let dash_len = 5.0_f32;
                                            let gap_len = 3.0_f32;
                                            // Top edge
                                            let mut dx = preview_rect.left();
                                            while dx < preview_rect.right() {
                                                let end_x = (dx + dash_len).min(preview_rect.right());
                                                painter.line_segment(
                                                    [egui::pos2(dx, preview_rect.top()), egui::pos2(end_x, preview_rect.top())],
                                                    egui::Stroke::new(1.5, dash_color),
                                                );
                                                dx += dash_len + gap_len;
                                            }
                                            // Bottom edge
                                            dx = preview_rect.left();
                                            while dx < preview_rect.right() {
                                                let end_x = (dx + dash_len).min(preview_rect.right());
                                                painter.line_segment(
                                                    [egui::pos2(dx, preview_rect.bottom()), egui::pos2(end_x, preview_rect.bottom())],
                                                    egui::Stroke::new(1.5, dash_color),
                                                );
                                                dx += dash_len + gap_len;
                                            }
                                        }

                                        // Item 13: Ghost outline at destination (30% opacity) if different from current
                                        let current_secs = clip.timeline_in.to_secs_f64();
                                        if (preview_in_secs - current_secs).abs() > 0.001 {
                                            let dest_x = content_rect.left() + preview_in_secs as f32 * zoom - scroll;
                                            let dest_rect = egui::Rect::from_min_size(
                                                egui::pos2(dest_x, other_track_rect.top() + 2.0),
                                                egui::vec2(pw.max(2.0), other_h - 4.0),
                                            );
                                            if dest_rect.right() >= content_rect.left() && dest_rect.left() <= content_rect.right() {
                                                painter.rect_stroke(dest_rect, 3.0,
                                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 77)));
                                            }
                                        }

                                        // Draw preview anchor line from connected clip to primary
                                        if let Some(anchor) = &clip.magnetic_anchor {
                                            let anchor_clip = track.clips.iter().find(|pc| pc.id == anchor.anchor_clip_id);
                                            if let Some(ac) = anchor_clip {
                                                let anchor_preview_in = ac.timeline_in + delta_rt;
                                                let anchor_x = content_rect.left()
                                                    + (anchor_preview_in + anchor.offset).to_secs_f64() as f32 * zoom - scroll;
                                                let from_y = preview_rect.top();
                                                let to_y = track_rect.bottom();
                                                let (top_y, bottom_y) = if from_y < to_y { (from_y, to_y) } else { (to_y, from_y) };
                                                // Dashed amber anchor line
                                                let mut dy = top_y;
                                                while dy < bottom_y {
                                                    let end_y = (dy + 5.0).min(bottom_y);
                                                    painter.line_segment(
                                                        [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 120)),
                                                    );
                                                    dy += 8.0;
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Item 14: Anchor snap indicator during non-primary drag
                            // (This renders when dragging clips near/far from primary on connected tracks)
                            // Already handled in the non-primary drag section above.
                            let _ = amber; // suppress unused
                        }

                    } else {
                        // NON-PRIMARY: Legacy free movement
                        let delta = final_in - drag.original_timeline_in;

                        if let Some(track) = state.sequence_mut().tracks.get_mut(drag.track_idx) {
                            if let Some(clip) = track.find_clip_mut(drag.clip_id) {
                                let dur = clip.duration();
                                clip.timeline_in = final_in;
                                clip.timeline_out = clip.timeline_in + dur;
                            }
                        }

                        // Move linked partner by same delta (absolute from its original position)
                        if let (Some(lid), Some(orig_in)) = (drag.linked_clip_id, drag.linked_original_in) {
                            for track in &mut state.sequence_mut().tracks {
                                if let Some(lclip) = track.find_clip_mut(lid) {
                                    let dur = lclip.duration();
                                    let new_in = orig_in + delta;
                                    lclip.timeline_in = if new_in.value < 0 {
                                        RationalTime { value: 0, rate: new_in.rate }
                                    } else {
                                        new_in
                                    };
                                    lclip.timeline_out = lclip.timeline_in + dur;
                                    break;
                                }
                            }
                        }

                        // Drop zone highlight — amber glow around the clip when snapped (non-primary only)
                        if snapped {
                            if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                                if let Some(clip) = track.find_clip(drag.clip_id) {
                                    let dur_secs = clip.duration().to_secs_f64() as f32;
                                    let drop_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                    let drop_w = dur_secs * zoom;
                                    let drop_rect = egui::Rect::from_min_size(
                                        egui::pos2(drop_x - 1.0, track_rect.top() + 1.0),
                                        egui::vec2(drop_w + 2.0, track_rect.height() - 2.0),
                                    );
                                    painter.rect_stroke(drop_rect, 4.0,
                                        egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)));
                                    painter.rect_stroke(drop_rect, 4.0,
                                        egui::Stroke::new(1.5, egui::Color32::from_rgb(232, 160, 32)));
                                }
                            }
                        }
                    }

                    // Auto-connect/disconnect indicator during non-primary drag
                    if !is_primary {
                        let primary_rect = self.frame_cache.track_rects.iter()
                            .find(|(idx, _, _)| state.sequence().tracks.get(*idx)
                                .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary))
                            .map(|(_, _, r)| *r);
                        if let Some(primary_r) = primary_rect {
                            let drag_clip_y = if let Some(pos) = pointer_pos { pos.y } else { track_rect.center().y };
                            let primary_center_y = primary_r.center().y;
                            let dist_px = (drag_clip_y - primary_center_y).abs();
                            let track_h = track_rect.height();
                            let dist_tracks = dist_px / track_h;

                            let clip_is_connected = state.sequence().tracks.get(drag.track_idx)
                                .and_then(|t| t.find_clip(drag.clip_id))
                                .is_some_and(|c| c.magnetic_anchor.is_some());

                            let connect_threshold = klipp_timeline::magnetic::AUTO_CONNECT_TRACK_DISTANCE;
                            let disconnect_threshold = klipp_timeline::magnetic::AUTO_DISCONNECT_TRACK_DISTANCE;

                            if dist_tracks < connect_threshold && !clip_is_connected {
                                // Will auto-connect on drop — show chain icon + dashed anchor line
                                if let Some(pos) = pointer_pos {
                                    let amber = egui::Color32::from_rgb(232, 160, 32);
                                    let pill_rect = egui::Rect::from_min_size(
                                        egui::pos2(pos.x + 12.0, pos.y - 20.0),
                                        egui::vec2(90.0, 16.0),
                                    );
                                    painter.rect_filled(pill_rect, 4.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220));
                                    painter.rect_stroke(pill_rect, 4.0, egui::Stroke::new(1.0, amber));
                                    painter.text(pill_rect.center(), egui::Align2::CENTER_CENTER,
                                        "\u{229E} Will connect", egui::FontId::monospace(8.0), amber);

                                    // Dashed amber vertical line from dragged clip to primary
                                    let clip_x = state.sequence().tracks.get(drag.track_idx)
                                        .and_then(|t| t.find_clip(drag.clip_id))
                                        .map(|c| content_rect.left() + c.timeline_in.to_secs_f64() as f32 * zoom - scroll);
                                    if let Some(anchor_x) = clip_x {
                                        let (top_y, bottom_y) = if primary_r.center().y > track_rect.center().y {
                                            (track_rect.bottom(), primary_r.top())
                                        } else {
                                            (primary_r.bottom(), track_rect.top())
                                        };
                                        let mut dy = top_y;
                                        let dash_len = 5.0_f32;
                                        let gap_len = 3.0_f32;
                                        while dy < bottom_y {
                                            let end_y = (dy + dash_len).min(bottom_y);
                                            painter.line_segment(
                                                [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 160)),
                                            );
                                            dy += dash_len + gap_len;
                                        }
                                    }
                                }
                            } else if dist_tracks > disconnect_threshold && clip_is_connected {
                                // Will auto-disconnect on drop — show broken chain icon + red dashed line
                                if let Some(pos) = pointer_pos {
                                    let warn_color = egui::Color32::from_rgb(192, 96, 96);
                                    let pill_rect = egui::Rect::from_min_size(
                                        egui::pos2(pos.x + 12.0, pos.y - 20.0),
                                        egui::vec2(100.0, 16.0),
                                    );
                                    painter.rect_filled(pill_rect, 4.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220));
                                    painter.rect_stroke(pill_rect, 4.0, egui::Stroke::new(1.0, warn_color));
                                    painter.text(pill_rect.center(), egui::Align2::CENTER_CENTER,
                                        "\u{229F} Will disconnect", egui::FontId::monospace(8.0), warn_color);

                                    // Existing anchor line turns red-dashed to indicate it will break
                                    if let Some(anchor) = state.sequence().tracks.get(drag.track_idx)
                                        .and_then(|t| t.find_clip(drag.clip_id))
                                        .and_then(|c| c.magnetic_anchor.clone())
                                    {
                                        let anchor_clip_in = state.sequence().tracks.iter()
                                            .flat_map(|t| t.clips.iter())
                                            .find(|c| c.id == anchor.anchor_clip_id)
                                            .map(|c| c.timeline_in);
                                        if let Some(ac_in) = anchor_clip_in {
                                            let anchor_x = content_rect.left()
                                                + (ac_in + anchor.offset).to_secs_f64() as f32 * zoom - scroll;
                                            let (top_y, bottom_y) = if primary_r.center().y > track_rect.center().y {
                                                (track_rect.bottom(), primary_r.top())
                                            } else {
                                                (primary_r.bottom(), track_rect.top())
                                            };
                                            let mut dy = top_y;
                                            while dy < bottom_y {
                                                let end_y = (dy + 5.0).min(bottom_y);
                                                painter.line_segment(
                                                    [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(192, 96, 96, 160)),
                                                );
                                                dy += 8.0;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Highlight target track if different from source
                    if let Some(target_idx) = hover_track_idx {
                        if target_idx != drag.track_idx {
                            let source_kind = state.sequence().tracks.get(drag.track_idx)
                                .map(|t| t.kind);
                            let target_kind = state.sequence().tracks.get(target_idx)
                                .map(|t| t.kind);
                            if source_kind == target_kind {
                                if let Some((_, _, rect)) = self.frame_cache.track_rects.iter()
                                    .find(|(idx, _, _)| *idx == target_idx)
                                {
                                    painter.rect_stroke(
                                        *rect, 0.0,
                                        egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255)),
                                    );
                                }
                            }
                        }
                    }

                    let _ = snapped; // suppress unused warning
                }
            }

            if pointer_released && drag.track_idx == track_idx {
                let clip_id = drag.clip_id;
                let from_track = drag.track_idx;

                // Check if pointer is over a different compatible track
                let target_track = pointer_pos.and_then(|pos| {
                    self.frame_cache.track_rects.iter()
                        .find(|(_, _, rect)| rect.contains(pos))
                        .map(|(idx, _, _)| *idx)
                });

                if let Some(to_track) = target_track {
                    if to_track != from_track {
                        let source_kind = state.sequence().tracks.get(from_track).map(|t| t.kind);
                        let target_kind = state.sequence().tracks.get(to_track).map(|t| t.kind);
                        if source_kind == target_kind {
                            // Cross-track move
                            if let Some(track) = state.sequence().tracks.get(from_track) {
                                if let Some(clip) = track.find_clip(clip_id) {
                                    cmds.push(AppCommand::MoveClipToTrack {
                                        clip_id,
                                        new_timeline_in: clip.timeline_in,
                                        from_track,
                                        to_track,
                                    });
                                }
                            }
                        } else {
                            // Incompatible track type — revert position
                            if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                if let Some(clip) = track.find_clip_mut(clip_id) {
                                    let dur = clip.duration();
                                    clip.timeline_in = drag.original_timeline_in;
                                    clip.timeline_out = drag.original_timeline_in + dur;
                                }
                            }
                        }
                    } else {
                        // Same track move
                        let is_primary = state.sequence().tracks.get(from_track)
                            .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);
                        if is_primary {
                            // Magnetic: route by edit mode (Insert = ripple move, Overwrite = replace)
                            let target_time = pointer_pos.map(|pos| {
                                let x_in_content = pos.x - content_rect.left() + scroll;
                                let secs = (x_in_content as f64 / zoom as f64).max(0.0);
                                RationalTime::from_secs_f64(secs, fps.rate())
                            });
                            if let Some(target_time) = target_time {
                                if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                    track.sort_clips();
                                }
                                // Alt override: temporarily treat as Overwrite when Alt is held
                                let alt_held = ui.input(|i| i.modifiers.alt);
                                let effective_mode = if alt_held {
                                    klipp_timeline::magnetic::EditMode::Overwrite
                                } else {
                                    state.edit_mode
                                };
                                let result = match effective_mode {
                                    klipp_timeline::magnetic::EditMode::Insert => {
                                        klipp_timeline::magnetic::MagneticEditor::move_primary(
                                            state.sequence_mut(), clip_id, target_time)
                                    }
                                    klipp_timeline::magnetic::EditMode::Overwrite => {
                                        // Remove clip from track first, then overwrite at target
                                        let clip_data = state.sequence().tracks.get(from_track)
                                            .and_then(|t| t.find_clip(clip_id)).cloned();
                                        if let Some(clip_data) = clip_data {
                                            // Remove the clip being dragged
                                            if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                                track.clips.retain(|c| c.id != clip_id);
                                            }
                                            klipp_timeline::magnetic::MagneticEditor::overwrite_primary(
                                                state.sequence_mut(), clip_data, target_time)
                                        } else {
                                            klipp_timeline::magnetic::MagneticEditor::move_primary(
                                                state.sequence_mut(), clip_id, target_time)
                                        }
                                    }
                                };
                                state.history.push(crate::history::HistoryEntry { sequence_idx: state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                                state.dirty = true;
                                cmds.push(AppCommand::SequenceUpdated);
                            }
                        } else {
                            // Legacy: free move
                            if let Some(track) = state.sequence().tracks.get(from_track) {
                                if let Some(clip) = track.find_clip(clip_id) {
                                    cmds.push(AppCommand::MoveClip {
                                        clip_id,
                                        new_timeline_in: clip.timeline_in,
                                    });
                                }
                            }

                            // Auto-connect/disconnect on drop for non-primary tracks
                            let primary_rect = self.frame_cache.track_rects.iter()
                                .find(|(idx, _, _)| state.sequence().tracks.get(*idx)
                                    .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary))
                                .map(|(pi, _, r)| (*pi, *r));
                            if let (Some(pos), Some((primary_ti, primary_r))) = (pointer_pos, primary_rect) {
                                let dist_px = (pos.y - primary_r.center().y).abs();
                                let track_h = track_rect.height().max(1.0);
                                let dist_tracks = dist_px / track_h;

                                let clip_is_connected = state.sequence().tracks.get(from_track)
                                    .and_then(|t| t.find_clip(clip_id))
                                    .is_some_and(|c| c.magnetic_anchor.is_some());

                                if dist_tracks < klipp_timeline::magnetic::AUTO_CONNECT_TRACK_DISTANCE && !clip_is_connected {
                                    // Auto-connect: find nearest primary clip
                                    let clip_in = state.sequence().tracks.get(from_track)
                                        .and_then(|t| t.find_clip(clip_id))
                                        .map(|c| c.timeline_in);
                                    if let Some(clip_in) = clip_in {
                                        let anchor_id = state.sequence().tracks[primary_ti].clips.iter()
                                            .min_by_key(|c| {
                                                let mid = c.timeline_in + (c.timeline_out - c.timeline_in);
                                                let diff = mid - clip_in;
                                                diff.value.unsigned_abs()
                                            })
                                            .map(|c| c.id);
                                        if let Some(anchor_id) = anchor_id {
                                            let result = klipp_timeline::magnetic::MagneticEditor::connect_clip(
                                                state.sequence_mut(), clip_id, anchor_id,
                                                klipp_timeline::AnchorAlignment::StartAligned);
                                            state.history.push(crate::history::HistoryEntry {
                                                sequence_idx: state.active_sequence_idx,
                                                command: crate::history::EditCommand::Magnetic(result.command),
                                            });
                                            state.dirty = true;
                                            cmds.push(AppCommand::SequenceUpdated);
                                        }
                                    }
                                } else if dist_tracks > klipp_timeline::magnetic::AUTO_DISCONNECT_TRACK_DISTANCE && clip_is_connected {
                                    // Auto-disconnect
                                    let result = klipp_timeline::magnetic::MagneticEditor::disconnect_clip(
                                        state.sequence_mut(), clip_id);
                                    state.history.push(crate::history::HistoryEntry {
                                        sequence_idx: state.active_sequence_idx,
                                        command: crate::history::EditCommand::Magnetic(result.command),
                                    });
                                    state.dirty = true;
                                    cmds.push(AppCommand::SequenceUpdated);
                                }
                            }
                        }
                    }
                } else if state.auto_create_tracks {
                    // Dropped outside all tracks — auto-create a matching track
                    if let Some(pos) = pointer_pos {
                        let source_kind = state.sequence().tracks.get(from_track).map(|t| t.kind);
                        let first_rect_top = self.frame_cache.track_rects.first().map(|(_, _, r)| r.top());
                        let last_rect_bottom = self.frame_cache.track_rects.last().map(|(_, _, r)| r.bottom());
                        let above_all = first_rect_top.is_some_and(|top| pos.y < top);
                        let below_all = last_rect_bottom.is_some_and(|bot| pos.y > bot);

                        if let Some(kind) = source_kind {
                            let should_create = match kind {
                                TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => above_all || below_all,
                                TrackKind::Audio => below_all || above_all,
                            };
                            if should_create {
                                let name = state.sequence().next_track_name(kind);
                                let new_track_idx = match kind {
                                    TrackKind::Video => {
                                        state.sequence_mut().add_video_track(name);
                                        // Find the newly added video track
                                        state.sequence().tracks.iter()
                                            .rposition(|t| t.kind == TrackKind::Video)
                                            .unwrap_or(0)
                                    }
                                    TrackKind::Audio => {
                                        state.sequence_mut().add_audio_track(name);
                                        state.sequence().tracks.len() - 1
                                    }
                                    TrackKind::Text => {
                                        state.sequence_mut().add_text_track(name);
                                        state.sequence().tracks.iter()
                                            .rposition(|t| t.kind == TrackKind::Text)
                                            .unwrap_or(0)
                                    }
                                    TrackKind::Subtitle => {
                                        state.sequence_mut().add_subtitle_track(name);
                                        state.sequence().tracks.len() - 1
                                    }
                                };
                                // Move clip to the new track
                                if let Some(track) = state.sequence().tracks.get(from_track) {
                                    if let Some(clip) = track.find_clip(clip_id) {
                                        cmds.push(AppCommand::MoveClipToTrack {
                                            clip_id,
                                            new_timeline_in: clip.timeline_in,
                                            from_track,
                                            to_track: new_track_idx,
                                        });
                                    }
                                }
                                state.dirty = true;
                                cmds.push(AppCommand::SequenceUpdated);
                                tracing::info!("Auto-created {:?} track for clip drag outside timeline", kind);
                            }
                        }
                    }
                }
                self.dragging_clip = None;
                self.press_origin = None;
            }
        }
    }

    /// Process ongoing slide drag.
    #[allow(clippy::too_many_arguments)]
    fn process_slide_drag(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        // ── Slide drag processing ────────────────────────────────────────
        if let Some(ref slide) = self.sliding_clip {
            if slide.track_idx == track_idx {
                if let Some(pos) = pointer_pos {
                    if pointer_down {
                        let dx = pos.x - slide.start_x;
                        let dt_secs = dx as f64 / zoom as f64;
                        let dt = RationalTime::from_secs_f64(dt_secs, slide.original_timeline_in.rate);

                        let mut new_tl_in = slide.original_timeline_in + dt;
                        let mut new_tl_out = slide.original_timeline_out + dt;

                        // Clamp: left neighbor can't get negative duration
                        if let (Some(left_id), Some(orig_left_out)) = (slide.left_clip_id, slide.original_left_out) {
                            // Left neighbor's new out = original_left_out + dt
                            // Its in stays the same, so duration = (orig_left_out + dt) - left_in
                            // We need the left clip's timeline_in
                            let left_in = state.sequence().tracks.get(track_idx)
                                .and_then(|t| t.find_clip(left_id))
                                .map(|c| c.timeline_in);
                            if let Some(left_in) = left_in {
                                let min_dur = RationalTime { value: 1, rate: orig_left_out.rate };
                                let min_left_out = left_in + min_dur;
                                let new_left_out = orig_left_out + dt;
                                if new_left_out.value < min_left_out.value {
                                    let correction = min_left_out - new_left_out;
                                    new_tl_in += correction;
                                    new_tl_out += correction;
                                }
                            }
                        }

                        // Clamp: right neighbor can't get negative duration
                        if let (Some(right_id), Some(orig_right_in)) = (slide.right_clip_id, slide.original_right_in) {
                            let right_out = state.sequence().tracks.get(track_idx)
                                .and_then(|t| t.find_clip(right_id))
                                .map(|c| c.timeline_out);
                            if let Some(right_out) = right_out {
                                let recalc_dt = new_tl_in - slide.original_timeline_in;
                                let new_right_in = orig_right_in + recalc_dt;
                                let min_dur = RationalTime { value: 1, rate: orig_right_in.rate };
                                if (right_out - new_right_in).value < min_dur.value {
                                    let max_right_in = right_out - min_dur;
                                    let correction = new_right_in - max_right_in;
                                    new_tl_in -= correction;
                                    new_tl_out -= correction;
                                }
                            }
                        }

                        let actual_dt = new_tl_in - slide.original_timeline_in;

                        // Apply slide to the clip
                        if let Some(track) = state.sequence_mut().tracks.get_mut(track_idx) {
                            if let Some(clip) = track.find_clip_mut(slide.clip_id) {
                                clip.timeline_in = new_tl_in;
                                clip.timeline_out = new_tl_out;
                            }
                            // Adjust left neighbor's out point (and source_out)
                            if let (Some(left_id), Some(orig_left_out), Some(orig_left_src_out)) =
                                (slide.left_clip_id, slide.original_left_out, slide.original_left_source_out)
                            {
                                if let Some(lc) = track.find_clip_mut(left_id) {
                                    lc.timeline_out = orig_left_out + actual_dt;
                                    lc.source_out = orig_left_src_out + actual_dt;
                                }
                            }
                            // Adjust right neighbor's in point (and source_in)
                            if let (Some(right_id), Some(orig_right_in), Some(orig_right_src_in)) =
                                (slide.right_clip_id, slide.original_right_in, slide.original_right_source_in)
                            {
                                if let Some(rc) = track.find_clip_mut(right_id) {
                                    rc.timeline_in = orig_right_in + actual_dt;
                                    rc.source_in = orig_right_src_in + actual_dt;
                                }
                            }
                        }

                        // Apply slide to linked partner
                        if let (Some(lid), Some(linked_in), Some(linked_out)) =
                            (slide.linked_clip_id, slide.linked_original_in, slide.linked_original_out)
                        {
                            for track in &mut state.sequence_mut().tracks {
                                if let Some(lclip) = track.find_clip_mut(lid) {
                                    lclip.timeline_in = linked_in + actual_dt;
                                    lclip.timeline_out = linked_out + actual_dt;
                                    break;
                                }
                            }
                        }

                        // Visual: cursor + tooltip
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                        let delta_secs = dt_secs;
                        let sign = if delta_secs >= 0.0 { "+" } else { "" };
                        let badge_text = format!("SLIDE  {sign}{:.2}s", delta_secs);
                        let badge_pos = egui::pos2(pos.x + 14.0, pos.y - 22.0);
                        let badge_rect = egui::Rect::from_min_size(badge_pos, egui::vec2(108.0, 16.0));
                        painter.rect_filled(badge_rect, 4.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220));
                        painter.rect_stroke(badge_rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 160, 200)));
                        painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER,
                            &badge_text, egui::FontId::monospace(9.0), egui::Color32::from_rgb(60, 160, 200));

                        // Highlight all three involved clips
                        let highlight_color = egui::Color32::from_rgba_unmultiplied(60, 160, 200, 50);
                        let highlight_border = egui::Color32::from_rgba_unmultiplied(60, 160, 200, 140);
                        for cid in [Some(slide.clip_id), slide.left_clip_id, slide.right_clip_id].into_iter().flatten() {
                            if let Some(clip) = clips.iter().find(|c| c.id == cid) {
                                let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
                                painter.rect_filled(cr, 3.0, highlight_color);
                                painter.rect_stroke(cr, 3.0, egui::Stroke::new(1.5, highlight_border));
                            }
                        }
                    }
                }

                if pointer_released {
                    // Finalize slide: push undo entry and emit sync command
                    // Extract all values before borrowing state.history
                    let slide_vals = {
                        let track = state.sequence().tracks.get(track_idx);
                        track.and_then(|t| {
                            let clip = t.find_clip(slide.clip_id)?;
                            let nl_out = slide.left_clip_id.and_then(|lid| t.find_clip(lid).map(|c| c.timeline_out));
                            let nr_in = slide.right_clip_id.and_then(|rid| t.find_clip(rid).map(|c| c.timeline_in));
                            Some((clip.timeline_in, clip.timeline_out, nl_out, nr_in))
                        })
                    };
                    if let Some((new_tl_in, new_tl_out, new_left_out, new_right_in)) = slide_vals {
                        if new_tl_in != slide.original_timeline_in {
                            let cid = slide.clip_id;
                            let old_tl_in = slide.original_timeline_in;
                            let old_tl_out = slide.original_timeline_out;
                            let l_id = slide.left_clip_id;
                            let old_l_out = slide.original_left_out;
                            let r_id = slide.right_clip_id;
                            let old_r_in = slide.original_right_in;
                            state.history.push(crate::history::HistoryEntry {
                                sequence_idx: state.active_sequence_idx,
                                command: crate::history::EditCommand::LegacySlideClip {
                                    clip_id: cid,
                                    old_timeline_in: old_tl_in,
                                    old_timeline_out: old_tl_out,
                                    new_timeline_in: new_tl_in,
                                    new_timeline_out: new_tl_out,
                                    left_clip_id: l_id,
                                    old_left_out: old_l_out,
                                    new_left_out,
                                    right_clip_id: r_id,
                                    old_right_in: old_r_in,
                                    new_right_in,
                                },
                            });
                            cmds.push(AppCommand::SlideClip {
                                clip_id: cid,
                                new_timeline_in: new_tl_in,
                                new_timeline_out: new_tl_out,
                                left_clip_id: l_id,
                                new_left_out,
                                right_clip_id: r_id,
                                new_right_in,
                            });
                        }
                    }
                    self.sliding_clip = None;
                    self.press_origin = None;
                }
            }
        }
    }

    /// Process ongoing slip drag.
    #[allow(clippy::too_many_arguments)]
    fn process_slip_drag(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        // ── Slip drag processing ─────────────────────────────────────────
        if let Some(ref slip) = self.slipping_clip {
            if slip.track_idx == track_idx {
                if let Some(pos) = pointer_pos {
                    if pointer_down {
                        let dx = pos.x - slip.start_x;
                        let dt_secs = dx as f64 / zoom as f64;
                        let dt = RationalTime::from_secs_f64(dt_secs, slip.original_source_in.rate);

                        let mut new_src_in = slip.original_source_in + dt;
                        let mut new_src_out = slip.original_source_out + dt;

                        // Clamp: source_in >= 0
                        if new_src_in.value < 0 {
                            let shift = RationalTime { value: -new_src_in.value, rate: new_src_in.rate };
                            new_src_in.value = 0;
                            new_src_out += shift;
                        }
                        // Clamp: source_out <= media duration
                        if let Some(media_dur) = slip.media_duration {
                            let media_dur_rt = media_dur.rescale_to(new_src_out.rate);
                            if new_src_out.value > media_dur_rt.value {
                                let overshoot = RationalTime { value: new_src_out.value - media_dur_rt.value, rate: new_src_out.rate };
                                new_src_out = media_dur_rt;
                                new_src_in -= overshoot;
                                if new_src_in.value < 0 { new_src_in.value = 0; }
                            }
                        }

                        // Apply slip to clip
                        if let Some(track) = state.sequence_mut().tracks.get_mut(track_idx) {
                            if let Some(clip) = track.find_clip_mut(slip.clip_id) {
                                clip.source_in = new_src_in;
                                clip.source_out = new_src_out;
                            }
                        }

                        // Apply to linked partner
                        if let Some(lid) = slip.linked_clip_id {
                            for track in &mut state.sequence_mut().tracks {
                                if let Some(lclip) = track.find_clip_mut(lid) {
                                    lclip.source_in = new_src_in;
                                    lclip.source_out = new_src_out;
                                    break;
                                }
                            }
                        }

                        // Visual: cursor + tooltip
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                        let delta_secs = dt_secs;
                        let sign = if delta_secs >= 0.0 { "+" } else { "" };
                        let badge_text = format!("SLIP  {sign}{:.2}s", delta_secs);
                        let badge_pos = egui::pos2(pos.x + 14.0, pos.y - 22.0);
                        let badge_rect = egui::Rect::from_min_size(badge_pos, egui::vec2(100.0, 16.0));
                        painter.rect_filled(badge_rect, 4.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220));
                        painter.rect_stroke(badge_rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 160, 60)));
                        painter.text(badge_rect.center(), egui::Align2::CENTER_CENTER,
                            &badge_text, egui::FontId::monospace(9.0), egui::Color32::from_rgb(200, 160, 60));

                        // Draw semi-transparent directional arrow overlay on the clip
                        if let Some(clip) = clips.iter().find(|c| c.id == slip.clip_id) {
                            let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
                            let overlay_color = egui::Color32::from_rgba_unmultiplied(200, 160, 60, 40);
                            painter.rect_filled(cr, 3.0, overlay_color);
                        }
                    }
                }

                if pointer_released {
                    // Finalize slip: push undo entry and emit sync command
                    // Extract values before borrowing state.history
                    let new_vals = state.sequence().tracks.get(track_idx)
                        .and_then(|t| t.find_clip(slip.clip_id))
                        .map(|c| (c.source_in, c.source_out));
                    if let Some((new_src_in, new_src_out)) = new_vals {
                        if new_src_in != slip.original_source_in || new_src_out != slip.original_source_out {
                            let cid = slip.clip_id;
                            let old_si = slip.original_source_in;
                            let old_so = slip.original_source_out;
                            state.history.push(crate::history::HistoryEntry {
                                sequence_idx: state.active_sequence_idx,
                                command: crate::history::EditCommand::LegacySlipClip {
                                    clip_id: cid,
                                    old_source_in: old_si,
                                    old_source_out: old_so,
                                    new_source_in: new_src_in,
                                    new_source_out: new_src_out,
                                },
                            });
                            cmds.push(AppCommand::SlipClip {
                                clip_id: cid,
                                new_source_in: new_src_in,
                                new_source_out: new_src_out,
                            });
                        }
                    }
                    self.slipping_clip = None;
                    self.press_origin = None;
                }
            }
        }
    }

    /// Process ongoing roll trim drag.
    #[allow(clippy::too_many_arguments)]
    fn process_roll_trim(
        &mut self,
        painter: &egui::Painter,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        fps: klipp_core::Fps,
        snap_enabled: bool,
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        // Handle ongoing roll trim
        if let Some(ref roll) = self.roll_trimming {
            if roll.track_idx != track_idx {
                // Skip — will be handled when we draw the correct track
            } else if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - roll.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());

                    // Get media durations for clamping
                    let media_a_dur = state.sequence().tracks.get(roll.track_idx)
                        .and_then(|t| t.find_clip(roll.clip_a_id))
                        .and_then(|c| c.media_id())
                        .and_then(|mid| state.media_pool.get(&mid))
                        .map(|m| m.duration)
                        .unwrap_or(RationalTime { value: i64::MAX, rate: fps.rate() });
                    let _media_b_dur = state.sequence().tracks.get(roll.track_idx)
                        .and_then(|t| t.find_clip(roll.clip_b_id))
                        .and_then(|c| c.media_id())
                        .and_then(|mid| state.media_pool.get(&mid))
                        .map(|m| m.duration)
                        .unwrap_or(RationalTime { value: i64::MAX, rate: fps.rate() });

                    // Compute new boundary position
                    let mut new_a_source_out = roll.original_a_source_out + dt_frames;
                    let mut new_b_source_in = roll.original_b_source_in + dt_frames;

                    // Get clip A's source_in (start of used range) for clamping
                    let clip_a_source_in = state.sequence().tracks.get(roll.track_idx)
                        .and_then(|t| t.find_clip(roll.clip_a_id))
                        .map(|c| c.source_in)
                        .unwrap_or(roll.original_a_source_out);
                    // Get clip B's source_out for clamping
                    let clip_b_source_out = state.sequence().tracks.get(roll.track_idx)
                        .and_then(|t| t.find_clip(roll.clip_b_id))
                        .map(|c| c.source_out)
                        .unwrap_or(roll.original_b_source_in);

                    // Clamp: clip A source_out must stay > clip A source_in (min 1 frame)
                    let min_a_out = clip_a_source_in + RationalTime { value: 1, rate: clip_a_source_in.rate };
                    // Clamp: clip A source_out must not exceed media duration
                    if new_a_source_out < min_a_out { new_a_source_out = min_a_out; }
                    if new_a_source_out > media_a_dur { new_a_source_out = media_a_dur; }
                    // Clamp: clip B source_in must stay >= 0 and < clip B source_out
                    let zero = RationalTime { value: 0, rate: new_b_source_in.rate };
                    let max_b_in = clip_b_source_out - RationalTime { value: 1, rate: clip_b_source_out.rate };
                    if new_b_source_in < zero { new_b_source_in = zero; }
                    if new_b_source_in > max_b_in { new_b_source_in = max_b_in; }

                    // Recompute consistent delta from the clamped values
                    let actual_delta_a = new_a_source_out - roll.original_a_source_out;
                    let actual_delta_b = new_b_source_in - roll.original_b_source_in;
                    // Use the smaller magnitude delta to keep both clips valid
                    let use_delta = if actual_delta_a.value.abs() <= actual_delta_b.value.abs() {
                        actual_delta_a
                    } else {
                        actual_delta_b
                    };

                    let pre_snap_tl_out = roll.original_a_timeline_out + use_delta;

                    // Snap the boundary to playhead (8px threshold)
                    let mut snapped_boundary = pre_snap_tl_out;
                    if snap_enabled {
                        let playhead_secs = state.playhead.to_secs_f64();
                        let boundary_secs = pre_snap_tl_out.to_secs_f64();
                        let dist_px = ((boundary_secs - playhead_secs).abs() * zoom as f64) as f32;
                        if dist_px < 8.0 {
                            snapped_boundary = state.playhead;
                            let snap_x = content_rect.left() + playhead_secs as f32 * zoom - state.timeline_scroll_x;
                            self.frame_cache.snap_line_x = Some(snap_x);
                        }
                    }

                    let snap_delta = snapped_boundary - roll.original_a_timeline_out;
                    let final_a_source_out = roll.original_a_source_out + snap_delta;
                    let final_b_source_in = roll.original_b_source_in + snap_delta;
                    let final_a_timeline_out = snapped_boundary;
                    let final_b_timeline_in = snapped_boundary;

                    // Apply to state
                    if let Some(track) = state.sequence_mut().tracks.get_mut(roll.track_idx) {
                        if let Some(c) = track.find_clip_mut(roll.clip_a_id) {
                            c.source_out = final_a_source_out;
                            c.timeline_out = final_a_timeline_out;
                        }
                        if let Some(c) = track.find_clip_mut(roll.clip_b_id) {
                            c.source_in = final_b_source_in;
                            c.timeline_in = final_b_timeline_in;
                        }
                    }

                    // Rich overlay for roll trim
                    let delta_frames = snapped_boundary - roll.original_boundary;
                    let dur_a = final_a_timeline_out - state.sequence().tracks.get(roll.track_idx)
                        .and_then(|t| t.find_clip(roll.clip_a_id))
                        .map(|c| c.timeline_in)
                        .unwrap_or(roll.original_a_timeline_out);
                    Self::paint_trim_overlay(
                        painter, content_rect, track_rect, zoom, scroll,
                        snapped_boundary, delta_frames, dur_a,
                        "Roll", fps,
                    );
                }
            }

            if pointer_released && roll.track_idx == track_idx {
                // Read final values and emit command
                let a_out = state.sequence().tracks.get(roll.track_idx)
                    .and_then(|t| t.find_clip(roll.clip_a_id))
                    .map(|c| (c.source_out, c.timeline_out));
                let b_in = state.sequence().tracks.get(roll.track_idx)
                    .and_then(|t| t.find_clip(roll.clip_b_id))
                    .map(|c| (c.source_in, c.timeline_in));

                if let (Some((a_src_out, a_tl_out)), Some((b_src_in, b_tl_in))) = (a_out, b_in) {
                    // Restore to original before emitting command (command handler does the mutation)
                    if let Some(track) = state.sequence_mut().tracks.get_mut(roll.track_idx) {
                        if let Some(c) = track.find_clip_mut(roll.clip_a_id) {
                            c.source_out = roll.original_a_source_out;
                            c.timeline_out = roll.original_a_timeline_out;
                        }
                        if let Some(c) = track.find_clip_mut(roll.clip_b_id) {
                            c.source_in = roll.original_b_source_in;
                            c.timeline_in = roll.original_b_timeline_in;
                        }
                    }
                    cmds.push(AppCommand::RollTrimClip {
                        clip_a_id: roll.clip_a_id,
                        clip_b_id: roll.clip_b_id,
                        new_a_source_out: a_src_out,
                        new_a_timeline_out: a_tl_out,
                        new_b_source_in: b_src_in,
                        new_b_timeline_in: b_tl_in,
                    });
                }
                self.roll_trimming = None;
                self.press_origin = None;
            }
        }
    }

    /// Find the best snap target for a given edge time.
    fn find_snap_target(
        snap_enabled: bool,
        snap_threshold_secs: f64,
        snap_targets: &[(f64, RationalTime, Option<ClipId>)],
        edge_time: RationalTime,
        exclude_clip: ClipId,
    ) -> Option<(RationalTime, f64)> {
        if !snap_enabled { return None; }
        let edge_secs = edge_time.to_secs_f64();
        let mut best: Option<(f64, RationalTime)> = None;
        for &(target_secs, target_tc, owner) in snap_targets {
            if owner == Some(exclude_clip) { continue; }
            let dist = (edge_secs - target_secs).abs();
            if dist < snap_threshold_secs && (best.is_none() || dist < best.unwrap().0) {
                best = Some((dist, target_tc));
            }
        }
        best.map(|(_, target)| (target, target.to_secs_f64()))
    }

    /// Process ongoing trim drag (only on the trim's own track).
    #[allow(clippy::too_many_arguments)]
    fn process_trim_drag(
        &mut self,
        _ui: &egui::Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        fps: klipp_core::Fps,
        snap_enabled: bool,
        snap_threshold_secs: f64,
        snap_targets: &[(f64, RationalTime, Option<ClipId>)],
        pointer_pos: Option<egui::Pos2>,
        pointer_down: bool,
        pointer_released: bool,
    ) {
        let find_snap = |edge_time: RationalTime, exclude_clip: ClipId| -> Option<(RationalTime, f64)> {
            Self::find_snap_target(snap_enabled, snap_threshold_secs, snap_targets, edge_time, exclude_clip)
        };

        // Handle ongoing trim (only process on the trim's own track to avoid duplicate tooltips)
        if let Some(ref trim) = self.trimming_clip {
            if trim.track_idx != track_idx {
                // Skip — will be handled when we draw the correct track
            } else if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - trim.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());

                    let media_duration = state.sequence().tracks.get(trim.track_idx)
                        .and_then(|t| t.find_clip(trim.clip_id))
                        .and_then(|c| c.media_id())
                        .and_then(|mid| state.media_pool.get(&mid))
                        .map(|m| m.duration)
                        .unwrap_or(RationalTime { value: i64::MAX, rate: fps.rate() });

                    match trim.edge {
                        TrimEdge::Left => {
                            let new_source_in = {
                                let sum = trim.original_source_in + dt_frames;
                                let zero = RationalTime { value: 0, rate: sum.rate };
                                let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                if sum < zero { zero }
                                else if sum > max_in { max_in }
                                else { sum }
                            };
                            let source_delta = new_source_in - trim.original_source_in;

                            if trim.subsequent_clips.is_empty() {
                                // Non-ripple: move timeline_in, keep timeline_out fixed
                                let mut new_timeline_in = trim.original_timeline_in + source_delta;
                                // Clamp so timeline_in doesn't go negative
                                if new_timeline_in.value < 0 {
                                    new_timeline_in.value = 0;
                                }

                                // Snap the left edge
                                if let Some((snap_target, snap_secs)) = find_snap(new_timeline_in, trim.clip_id) {
                                    let snap_delta = snap_target - trim.original_timeline_in;
                                    let snapped_source_in = trim.original_source_in + snap_delta;
                                    let zero = RationalTime { value: 0, rate: snapped_source_in.rate };
                                    let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                    if snapped_source_in >= zero && snapped_source_in <= max_in {
                                        new_timeline_in = snap_target;
                                        self.frame_cache.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                    }
                                }

                                let final_delta = new_timeline_in - trim.original_timeline_in;
                                let final_source_in = trim.original_source_in + final_delta;

                                if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
                                    if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                        c.source_in = final_source_in;
                                        c.timeline_in = new_timeline_in;
                                    }
                                }

                                // Rich timecode overlay during trim
                                let new_duration = trim.original_timeline_out - new_timeline_in;
                                let delta_frames = new_timeline_in - trim.original_timeline_in;
                                Self::paint_trim_overlay(
                                    painter, content_rect, track_rect, zoom, scroll,
                                    new_timeline_in, delta_frames, new_duration,
                                    "In", fps,
                                );
                            } else {
                                // Ripple: keep timeline_in fixed, shrink/grow timeline_out,
                                // shift subsequent clips by the duration change
                                let final_source_in = new_source_in;
                                // Duration change: negative when clip gets shorter (source_in moved right)
                                let duration_delta = trim.original_source_in - final_source_in; // negative = shorter
                                let new_timeline_out = trim.original_timeline_out + duration_delta;

                                // Snap the new timeline_out
                                let mut snapped_timeline_out = new_timeline_out;
                                if let Some((snap_target, snap_secs)) = find_snap(new_timeline_out, trim.clip_id) {
                                    snapped_timeline_out = snap_target;
                                    self.frame_cache.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }

                                // Recompute source_in from the snapped duration
                                let snapped_duration = snapped_timeline_out - trim.original_timeline_in;
                                let snapped_source_in = trim.original_source_out - snapped_duration;
                                let zero = RationalTime { value: 0, rate: snapped_source_in.rate };
                                let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                let clamped_source_in = if snapped_source_in < zero { zero }
                                    else if snapped_source_in > max_in { max_in }
                                    else { snapped_source_in };

                                // Recalculate timeline_out from clamped source
                                let final_duration = trim.original_source_out - clamped_source_in;
                                let final_timeline_out = trim.original_timeline_in + final_duration;
                                let ripple_delta = final_timeline_out - trim.original_timeline_out;

                                if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
                                    if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                        c.source_in = clamped_source_in;
                                        // timeline_in stays at original
                                        c.timeline_in = trim.original_timeline_in;
                                        c.timeline_out = final_timeline_out;
                                    }
                                    for (sub_id, orig_in, orig_out) in &trim.subsequent_clips {
                                        if let Some(sc) = track.find_clip_mut(*sub_id) {
                                            sc.timeline_in = *orig_in + ripple_delta;
                                            sc.timeline_out = *orig_out + ripple_delta;
                                        }
                                    }
                                }

                                // Rich timecode overlay during ripple trim
                                let new_duration = final_timeline_out - trim.original_timeline_in;
                                let delta_frames = clamped_source_in - trim.original_source_in;
                                Self::paint_trim_overlay(
                                    painter, content_rect, track_rect, zoom, scroll,
                                    trim.original_timeline_in, delta_frames, new_duration,
                                    "In", fps,
                                );
                            }
                        }
                        TrimEdge::Right => {
                            let new_source_out = {
                                let sum = trim.original_source_out + dt_frames;
                                let min_out = trim.original_source_in + RationalTime { value: 1, rate: trim.original_source_in.rate };
                                if sum < min_out { min_out }
                                else if sum > media_duration { media_duration }
                                else { sum }
                            };
                            let delta = new_source_out - trim.original_source_out;
                            let mut new_timeline_out = trim.original_timeline_out + delta;

                            // Snap the right edge (timeline_out)
                            if let Some((snap_target, snap_secs)) = find_snap(new_timeline_out, trim.clip_id) {
                                let snap_delta = snap_target - trim.original_timeline_out;
                                let snapped_source_out = trim.original_source_out + snap_delta;
                                let min_out = trim.original_source_in + RationalTime { value: 1, rate: trim.original_source_in.rate };
                                if snapped_source_out >= min_out && snapped_source_out <= media_duration {
                                    new_timeline_out = snap_target;
                                    self.frame_cache.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }
                            }

                            let final_delta = new_timeline_out - trim.original_timeline_out;
                            let final_source_out = trim.original_source_out + final_delta;

                            if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
                                if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                    c.source_out = final_source_out;
                                    c.timeline_out = new_timeline_out;
                                }
                                // Ripple: shift subsequent clips by the out-point change
                                if !trim.subsequent_clips.is_empty() {
                                    let ripple_delta = new_timeline_out - trim.original_timeline_out;
                                    for (sub_id, orig_in, orig_out) in &trim.subsequent_clips {
                                        if let Some(sc) = track.find_clip_mut(*sub_id) {
                                            sc.timeline_in = *orig_in + ripple_delta;
                                            sc.timeline_out = *orig_out + ripple_delta;
                                        }
                                    }
                                }
                            }

                            // Rich timecode overlay during trim
                            let cur_timeline_in = state.sequence().tracks.get(trim.track_idx)
                                .and_then(|t| t.find_clip(trim.clip_id))
                                .map(|c| c.timeline_in)
                                .unwrap_or(trim.original_timeline_in);
                            let new_duration = new_timeline_out - cur_timeline_in;
                            let delta_frames = new_timeline_out - trim.original_timeline_out;
                            Self::paint_trim_overlay(
                                painter, content_rect, track_rect, zoom, scroll,
                                new_timeline_out, delta_frames, new_duration,
                                "Out", fps,
                            );
                        }
                    }
                }
            }

            if pointer_released && trim.track_idx == track_idx {
                if let Some(track) = state.sequence().tracks.get(trim.track_idx) {
                    if let Some(c) = track.find_clip(trim.clip_id) {
                        cmds.push(AppCommand::TrimClip {
                            clip_id: trim.clip_id,
                            old_source_in: trim.original_source_in,
                            old_source_out: trim.original_source_out,
                            old_timeline_in: trim.original_timeline_in,
                            old_timeline_out: trim.original_timeline_out,
                            new_source_in: c.source_in,
                            new_source_out: c.source_out,
                            new_timeline_in: c.timeline_in,
                            new_timeline_out: c.timeline_out,
                            ripple: state.ripple_enabled,
                        });
                    }
                }
                self.trimming_clip = None;
                self.press_origin = None;
            }
        }
    }

    /// Trim handle hover highlight — edge-first detection + roll trim junction detection,
    /// plus active trim and active roll trim highlights.
    #[allow(clippy::too_many_arguments)]
    fn handle_trim_hover(
        &mut self,
        painter: &egui::Painter,
        ui: &egui::Ui,
        state: &AppState,
        track_idx: usize,
        clips: &[klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pointer_pos: Option<egui::Pos2>,
        track_kind: klipp_timeline::TrackKind,
        _interactive: bool,
    ) {
        // Trim handle hover highlight — edge-first detection + roll trim junction detection
        if state.active_tool == Tool::Select && self.trimming_clip.is_none() && self.roll_trimming.is_none() && self.dragging_clip.is_none() {
            if let Some(pos) = pointer_pos {
                // First: check for roll trim junction (boundary between two adjacent clips)
                let mut found_roll_junction = false;
                if clips.len() >= 2 {
                    let roll_threshold = 4.0_f32; // pixels
                    for i in 0..clips.len() - 1 {
                        let cr_a = Self::clip_screen_rect(&clips[i], content_rect, track_rect, zoom, scroll, track_height);
                        let cr_b = Self::clip_screen_rect(&clips[i + 1], content_rect, track_rect, zoom, scroll, track_height);
                        // Junction is where clip A ends and clip B begins
                        let junction_x = (cr_a.right() + cr_b.left()) / 2.0;
                        if (pos.x - junction_x).abs() < roll_threshold
                            && pos.y >= track_rect.top() && pos.y <= track_rect.top() + track_height
                            && !clips[i].kind.is_gap() && !clips[i + 1].kind.is_gap()
                        {
                            // Check clips are truly adjacent (no gap between them)
                            let gap_secs = (clips[i + 1].timeline_in - clips[i].timeline_out).to_secs_f64().abs();
                            if gap_secs < 0.001 {
                                found_roll_junction = true;
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);

                                // Highlight both edges — amber color for roll trim
                                let roll_color = egui::Color32::from_rgba_premultiplied(255, 180, 40, 200);
                                let bar_w = 3.0;
                                // Right edge of clip A
                                painter.rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(cr_a.right() - bar_w, cr_a.top()),
                                        egui::vec2(bar_w, cr_a.height()),
                                    ),
                                    0.0, roll_color,
                                );
                                // Left edge of clip B
                                painter.rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(cr_b.left(), cr_b.top()),
                                        egui::vec2(bar_w, cr_b.height()),
                                    ),
                                    0.0, roll_color,
                                );
                                // "Roll trim" label
                                painter.text(
                                    egui::pos2(junction_x, cr_a.top() - 2.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    "Roll trim",
                                    egui::FontId::proportional(8.0),
                                    egui::Color32::from_rgba_premultiplied(255, 200, 80, 220),
                                );
                                break;
                            }
                        }
                    }
                }

                if !found_roll_junction {
                    if let Some((cr, hit_clip, edge)) = self.hit_test_edge(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                        // Check if pointer is in the fade handle zone (top corner) — skip trim highlight there
                        let at_top = pos.y - cr.top() < 12.0;
                        let at_near_edge = match edge {
                            TrimEdge::Left => pos.x - cr.left() < 10.0,
                            TrimEdge::Right => cr.right() - pos.x < 10.0,
                        };
                        let clip_has_audio = track_kind == TrackKind::Audio
                            || hit_clip.media_id().and_then(|mid| state.media_pool.get(&mid)).is_some_and(|m| m.has_audio);
                        let in_fade_zone = at_top && at_near_edge && clip_has_audio && !hit_clip.kind.is_gap();

                        if !in_fade_zone {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);

                            // Amber/yellow highlight bar at the trim edge (2-3px, full clip height)
                            let amber_hover = egui::Color32::from_rgba_premultiplied(255, 200, 60, 200);
                            let bar_w = 3.0;
                            let edge_x = match edge {
                                TrimEdge::Left => cr.left(),
                                TrimEdge::Right => cr.right() - bar_w,
                            };
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(edge_x, cr.top()),
                                    egui::vec2(bar_w, cr.height()),
                                ),
                                0.0, amber_hover,
                            );

                            // Handle grip area with grip lines
                            let handle_w = 6.0;
                            let handle_x = match edge {
                                TrimEdge::Left => cr.left(),
                                TrimEdge::Right => cr.right() - handle_w,
                            };
                            let handle_rect = egui::Rect::from_min_size(
                                egui::pos2(handle_x, cr.top()),
                                egui::vec2(handle_w, cr.height()),
                            );
                            let cx = handle_rect.center().x;
                            let cy = handle_rect.center().y;
                            for dy in [-6.0_f32, -2.0, 2.0, 6.0] {
                                painter.line_segment(
                                    [egui::pos2(cx - 1.5, cy + dy), egui::pos2(cx + 1.5, cy + dy)],
                                    egui::Stroke::new(1.0, egui::Color32::WHITE),
                                );
                            }
                            // Trim tooltip label
                            let tip_x = match edge {
                                TrimEdge::Left => cr.left() + 10.0,
                                TrimEdge::Right => cr.right() - 10.0,
                            };
                            let tip_align = match edge {
                                TrimEdge::Left => egui::Align2::LEFT_CENTER,
                                TrimEdge::Right => egui::Align2::RIGHT_CENTER,
                            };
                            painter.text(
                                egui::pos2(tip_x, cr.top() + 6.0),
                                tip_align,
                                "Drag to trim clip",
                                egui::FontId::proportional(8.0),
                                egui::Color32::from_rgba_premultiplied(255, 255, 255, 180),
                            );
                        }
                    }
                }
            }
        }

        // Active trim highlight — show bright bar + colored tint during active trim
        if let Some(ref trim) = self.trimming_clip {
            if trim.track_idx == track_idx {
                if let Some(clip) = clips.iter().find(|c| c.id == trim.clip_id) {
                    let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);

                    // Brighter, wider bar (3-4px) at the active trim edge
                    let active_bar_w = 4.0;
                    let active_color = egui::Color32::from_rgba_premultiplied(255, 220, 80, 240);
                    let edge_x = match trim.edge {
                        TrimEdge::Left => cr.left(),
                        TrimEdge::Right => cr.right() - active_bar_w,
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(edge_x, cr.top()),
                            egui::vec2(active_bar_w, cr.height()),
                        ),
                        0.0, active_color,
                    );

                    // Subtle colored tint over the clip being trimmed
                    let tint_color = egui::Color32::from_rgba_unmultiplied(255, 200, 60, 20);
                    painter.rect_filled(cr, 3.0, tint_color);
                }
            }
        }

        // Active roll trim highlight — show both edges highlighted
        if let Some(ref roll) = self.roll_trimming {
            if roll.track_idx == track_idx {
                let roll_active_color = egui::Color32::from_rgba_premultiplied(255, 180, 40, 240);
                let bar_w = 4.0;
                if let Some(clip_a) = clips.iter().find(|c| c.id == roll.clip_a_id) {
                    let cr_a = Self::clip_screen_rect(clip_a, content_rect, track_rect, zoom, scroll, track_height);
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(cr_a.right() - bar_w, cr_a.top()),
                            egui::vec2(bar_w, cr_a.height()),
                        ),
                        0.0, roll_active_color,
                    );
                    // Subtle tint
                    painter.rect_filled(cr_a, 3.0, egui::Color32::from_rgba_unmultiplied(255, 180, 40, 15));
                }
                if let Some(clip_b) = clips.iter().find(|c| c.id == roll.clip_b_id) {
                    let cr_b = Self::clip_screen_rect(clip_b, content_rect, track_rect, zoom, scroll, track_height);
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(cr_b.left(), cr_b.top()),
                            egui::vec2(bar_w, cr_b.height()),
                        ),
                        0.0, roll_active_color,
                    );
                    painter.rect_filled(cr_b, 3.0, egui::Color32::from_rgba_unmultiplied(255, 180, 40, 15));
                }
            }
        }
    }

    /// Draw transition indicators between clips on a track.
    #[allow(clippy::too_many_arguments)]
    fn draw_transition_overlays(
        &self,
        painter: &egui::Painter,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        track_idx: usize,
        state: &AppState,
        zoom: f32,
        scroll: f32,
        track_height: f32,
    ) {
        if let Some(track) = state.sequence().tracks.get(track_idx) {
            for transition in &track.transitions {
                // Find clip_b (the incoming clip) to determine the transition position
                let clip_b = track.find_clip(transition.clip_b_id);
                if let Some(clip_b) = clip_b {
                    let trans_start_secs = clip_b.timeline_in.to_secs_f64();
                    let trans_dur_secs = transition.duration.to_secs_f64();
                    let trans_start_x = content_rect.left() + (trans_start_secs * zoom as f64) as f32 - scroll;
                    let trans_end_x = content_rect.left() + ((trans_start_secs + trans_dur_secs) * zoom as f64) as f32 - scroll;

                    // Only draw if visible
                    if trans_end_x > content_rect.left() && trans_start_x < content_rect.right() {
                        let trans_w = (trans_end_x - trans_start_x).max(4.0);
                        let indicator_rect = egui::Rect::from_min_size(
                            egui::pos2(trans_start_x, track_rect.top()),
                            egui::vec2(trans_w, track_height),
                        );
                        // Semi-transparent overlay
                        painter.rect_filled(
                            indicator_rect,
                            0.0,
                            egui::Color32::from_rgba_premultiplied(255, 180, 40, 35),
                        );
                        // Diamond marker at center
                        let cx = (trans_start_x + trans_end_x) / 2.0;
                        let cy = track_rect.center().y;
                        let d = 5.0;
                        let diamond = vec![
                            egui::pos2(cx, cy - d),
                            egui::pos2(cx + d, cy),
                            egui::pos2(cx, cy + d),
                            egui::pos2(cx - d, cy),
                        ];
                        painter.add(egui::Shape::convex_polygon(
                            diamond,
                            egui::Color32::from_rgb(255, 200, 60),
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 150, 30)),
                        ));
                        // Label if wide enough
                        if trans_w > 40.0 {
                            let kind_label = match transition.kind {
                                klipp_timeline::TransitionKind::CrossDissolve => "Dissolve",
                                klipp_timeline::TransitionKind::WipeLeft => "Wipe L",
                                klipp_timeline::TransitionKind::WipeRight => "Wipe R",
                                klipp_timeline::TransitionKind::WipeUp => "Wipe U",
                                klipp_timeline::TransitionKind::WipeDown => "Wipe D",
                                klipp_timeline::TransitionKind::DipToBlack => "Dip Blk",
                                klipp_timeline::TransitionKind::DipToWhite => "Dip Wht",
                                klipp_timeline::TransitionKind::PushLeft => "Push L",
                                klipp_timeline::TransitionKind::FilmDissolve => "Film",
                                klipp_timeline::TransitionKind::BlurDissolve => "Blur",
                                klipp_timeline::TransitionKind::Flash => "Flash",
                                klipp_timeline::TransitionKind::Spin => "Spin",
                                klipp_timeline::TransitionKind::LightLeak => "Leak",
                            };
                            painter.text(
                                egui::pos2(cx, cy - d - 8.0),
                                egui::Align2::CENTER_BOTTOM,
                                kind_label,
                                egui::FontId::proportional(9.0),
                                egui::Color32::from_rgb(255, 220, 100),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Draw a speed curve on video clips that have speed keyframes.
    fn draw_speed_overlay(
        &self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
    ) {
        if clip.speed_keyframes.is_empty() || clip_rect.width() <= 12.0 || clip.kind.is_gap() {
            return;
        }
        let spd_clip_dur = clip.duration().to_secs_f64();
        if spd_clip_dur <= 0.0 {
            return;
        }
        let spd_painter = painter.with_clip_rect(clip_rect);

        // Map time/speed to pixel coordinates
        // Y axis: 0x at bottom, 2x at top (unity 1x at center)
        let spd_max = 2.0_f32;
        let map_spd_x = |t: f64| -> f32 {
            clip_rect.left() + (t / spd_clip_dur) as f32 * clip_rect.width()
        };
        let map_spd_y = |v: f32| -> f32 {
            let frac = (v / spd_max).clamp(0.0, 1.0);
            clip_rect.bottom() - frac * clip_rect.height()
        };

        // Unity line (speed = 1.0) as a dim reference
        let unity_y = map_spd_y(1.0);
        spd_painter.line_segment(
            [egui::pos2(clip_rect.left(), unity_y), egui::pos2(clip_rect.right(), unity_y)],
            egui::Stroke::new(0.5, egui::Color32::from_white_alpha(30)),
        );

        // Build polyline from evaluated speed curve
        let spd_color = egui::Color32::from_rgb(100, 200, 255);
        let spd_fill_color = egui::Color32::from_rgba_unmultiplied(100, 200, 255, 25);
        let num_spd_pts = (clip_rect.width() as usize / 3).max(2).min(120);
        let mut spd_points: Vec<egui::Pos2> = Vec::with_capacity(num_spd_pts);
        for i in 0..num_spd_pts {
            let t = (i as f64 / (num_spd_pts - 1) as f64) * spd_clip_dur;
            let v = klipp_core::keyframe::evaluate(&clip.speed_keyframes, t, clip.speed as f32);
            spd_points.push(egui::pos2(map_spd_x(t), map_spd_y(v)));
        }

        // Filled area under the curve
        if spd_points.len() >= 2 {
            let mut fill_poly: Vec<egui::Pos2> = spd_points.clone();
            fill_poly.push(egui::pos2(clip_rect.right(), clip_rect.bottom()));
            fill_poly.push(egui::pos2(clip_rect.left(), clip_rect.bottom()));
            spd_painter.add(egui::Shape::convex_polygon(fill_poly, spd_fill_color, egui::Stroke::NONE));
        }

        // Draw the speed curve line
        if spd_points.len() >= 2 {
            for pair in spd_points.windows(2) {
                spd_painter.line_segment(
                    [pair[0], pair[1]],
                    egui::Stroke::new(1.5, spd_color),
                );
            }
        }

        // Draw keyframe dots
        for kf in &clip.speed_keyframes {
            let px = map_spd_x(kf.time_secs);
            let py = map_spd_y(kf.value);
            spd_painter.circle_filled(egui::pos2(px, py), 3.0, spd_color);
            spd_painter.circle_stroke(
                egui::pos2(px, py), 3.0,
                egui::Stroke::new(0.5, egui::Color32::WHITE),
            );
        }
    }

    /// Draw small colored ticks at the top edge of clips at marker positions.
    fn draw_clip_markers(
        &self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
    ) {
        if clip.clip_markers.is_empty() || clip_rect.width() <= 8.0 || clip.kind.is_gap() {
            return;
        }
        let clip_dur = clip.duration().to_secs_f64();
        if clip_dur <= 0.0 {
            return;
        }
        let marker_painter = painter.with_clip_rect(clip_rect);
        for m in &clip.clip_markers {
            let frac = (m.time_secs / clip_dur) as f32;
            if (0.0..=1.0).contains(&frac) {
                let x = clip_rect.left() + frac * clip_rect.width();
                let (r, g, b) = m.color.to_rgb();
                let color = egui::Color32::from_rgb(r, g, b);
                let dark = egui::Color32::from_rgba_premultiplied(0, 0, 0, 160);
                // Small downward-pointing triangle at top edge
                let half_w = 3.0;
                let tick_h = 6.0;
                let top = clip_rect.top();
                // Dark outline
                let outline = vec![
                    egui::pos2(x - half_w - 0.5, top),
                    egui::pos2(x + half_w + 0.5, top),
                    egui::pos2(x, top + tick_h + 0.5),
                ];
                marker_painter.add(egui::Shape::convex_polygon(outline, dark, egui::Stroke::NONE));
                // Colored fill
                let tri = vec![
                    egui::pos2(x - half_w, top),
                    egui::pos2(x + half_w, top),
                    egui::pos2(x, top + tick_h),
                ];
                marker_painter.add(egui::Shape::convex_polygon(tri, color, egui::Stroke::NONE));
            }
        }
    }

    /// Draw a gap clip (hatched pattern + duration label + selection). Returns true if it was a gap.
    fn draw_gap_clip(
        &self,
        painter: &egui::Painter,
        clip_rect: egui::Rect,
        clip: &klipp_timeline::Clip,
        content_rect: &egui::Rect,
        pointer_pos: Option<egui::Pos2>,
        pointer_released: bool,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) -> bool {
        if !clip.kind.is_gap() {
            return false;
        }

        let gap_bg = egui::Color32::from_rgb(0x0a, 0x0a, 0x0c);
        painter.rect_filled(clip_rect, 3.0, gap_bg);

        // Diagonal hatched stripes
        let stripe_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 25);
        let stripe_spacing = 8.0_f32;
        let clip_painter = painter.with_clip_rect(clip_rect);
        let mut sx = clip_rect.min.x - clip_rect.height();
        while sx < clip_rect.max.x {
            let p1 = egui::pos2(sx, clip_rect.max.y);
            let p2 = egui::pos2(sx + clip_rect.height(), clip_rect.min.y);
            clip_painter.line_segment([p1, p2], egui::Stroke::new(1.0, stripe_color));
            sx += stripe_spacing;
        }

        // Amber dashed border
        painter.rect_stroke(
            clip_rect,
            3.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)),
        );

        // Duration label centered
        let dur = clip.duration();
        let secs = dur.to_secs_f64();
        let label = if secs >= 1.0 {
            format!("{:.1}s", secs)
        } else {
            format!("{:.0}f", dur.value as f64)
        };
        painter.text(
            clip_rect.center(),
            egui::Align2::CENTER_CENTER,
            &label,
            egui::FontId::monospace(9.0),
            egui::Color32::from_rgba_unmultiplied(232, 160, 32, 120),
        );

        // Gap clips ARE selectable — handle click (only inside visible content)
        if let Some(pos) = pointer_pos {
            if clip_rect.contains(pos) && content_rect.contains(pos) {
                if pointer_released {
                    cmds.push(AppCommand::SelectClip(Some(clip.id)));
                }
                state.hovered_clip = Some(clip.id);
            }
        }

        // Selection highlight
        if state.selected_clip == Some(clip.id) {
            painter.rect_stroke(
                clip_rect,
                3.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 160, 240)),
            );
        }

        true
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_track_clips(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        track_rect: &egui::Rect,
        content_rect: &egui::Rect,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        interactive: bool,
    ) {
        let track_kind = state.sequence().tracks[track_idx].kind;
        let clips: Vec<_> = state.sequence().tracks[track_idx].clips.clone();

        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let fps = state.fps;
        let track_height = track_rect.height();

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_released = ui.input(|i| i.pointer.primary_released());

        for (clip_index, clip) in clips.iter().enumerate() {
            let clip_x =
                content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
            let clip_w = clip.duration().to_secs_f64() as f32 * zoom;

            // Visual rect has a small gap so adjacent clips don't touch
            let clip_rect = egui::Rect::from_min_size(
                egui::pos2(clip_x + CLIP_GAP, track_rect.top() + 2.0),
                egui::vec2((clip_w - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
            );

            // Cache clip rect for connection line drawing
            self.frame_cache.clip_screen_rects.insert(clip.id, clip_rect);

            // Skip if not visible (horizontal or vertical)
            if clip_rect.right() < content_rect.left()
                || clip_rect.left() > content_rect.right()
                || clip_rect.bottom() < track_rect.top()
                || clip_rect.top() > track_rect.bottom()
            {
                continue;
            }

            // Gap clip rendering — hatched pattern with duration label
            if self.draw_gap_clip(painter, clip_rect, clip, content_rect, pointer_pos, pointer_released, state, cmds) {
                continue;
            }

            let is_selected = state.selected_clip == Some(clip.id);
            let is_multi_selected = state.selected_clips.contains(&clip.id);
            let is_hovered = state.hovered_clip == Some(clip.id);
            let is_title = clip.title_data().is_some();
            let is_disabled = !clip.enabled;
            let is_offline = clip.media_id().and_then(|mid| state.media_pool.get(&mid))
                .map(|m| m.offline)
                .unwrap_or(false);
            let is_being_dragged = self.dragging_clip.as_ref()
                .is_some_and(|d| d.clip_id == clip.id);

            // Use per-clip role color for audio clips (and B-Roll/Graphics video clips).
            let theme_colors = crate::theme::KlippTheme::default();
            let role_color = theme_colors.clip_role_color(clip.role);
            let use_role_color = match track_kind {
                TrackKind::Audio => true,
                _ => matches!(clip.role, klipp_timeline::ClipRole::BRoll | klipp_timeline::ClipRole::Graphics),
            };
            let base_color = if is_title {
                egui::Color32::from_rgb(60, 45, 90) // purple tint for title clips
            } else if is_offline {
                egui::Color32::from_rgb(58, 32, 32)
            } else if use_role_color {
                egui::Color32::from_rgb(
                    role_color.r().saturating_sub(10),
                    role_color.g().saturating_sub(10),
                    role_color.b().saturating_sub(10),
                )
            } else {
                clip_color(clip_index)
            };
            let bright_color = if is_title {
                egui::Color32::from_rgb(90, 65, 130)
            } else if is_offline {
                egui::Color32::from_rgb(80, 45, 45)
            } else if use_role_color {
                egui::Color32::from_rgb(
                    role_color.r().saturating_add(20),
                    role_color.g().saturating_add(20),
                    role_color.b().saturating_add(20),
                )
            } else {
                clip_color_bright(clip_index)
            };

            let (fill, border_color, border_width) = if is_being_dragged {
                // Semi-transparent when being dragged so clips underneath are visible
                let f = egui::Color32::from_rgba_unmultiplied(base_color.r(), base_color.g(), base_color.b(), 140);
                (f, egui::Color32::from_rgb(232, 160, 32), 2.0)
            } else if is_selected {
                (bright_color, egui::Color32::from_rgb(120, 160, 240), 2.0)
            } else if is_multi_selected {
                (bright_color, egui::Color32::from_rgb(80, 130, 220), 1.5)
            } else if is_hovered {
                (base_color, egui::Color32::from_rgb(160, 160, 180), 1.0)
            } else {
                (base_color, bright_color, 1.0)
            };

            // Dim disabled clips
            let (fill, border_color) = if is_disabled {
                let dim = |c: egui::Color32| {
                    egui::Color32::from_rgba_unmultiplied(
                        c.r() / 2, c.g() / 2, c.b() / 2, 160,
                    )
                };
                (dim(fill), dim(border_color))
            } else {
                (fill, border_color)
            };

            self.draw_filmstrip_background(painter, clip_rect, clip, state, cmds, track_kind, content_rect, fill, is_being_dragged, clip_index);

            self.draw_audio_waveform(painter, clip_rect, clip, state, track_kind, bright_color);

            // ── Audio parameter rubber band overlay ──────────────────────
            // Drawn on audio track clips. Displays the parameter selected by rubber_band_param.
            // For linked audio clips, reads keyframes from the linked video clip.
            if track_kind == TrackKind::Audio && clip_rect.width() > 8.0 && !clip.kind.is_gap() && state.audio_show_rubber_band {
                let rb_param = state.rubber_band_param;

                // Resolve the clip that holds the keyframes (may be linked video clip)
                // Clone keyframes to avoid borrow conflict with state mutation for drag interaction
                let resolve_clip = |c: &klipp_timeline::Clip| -> (f32, Vec<klipp_core::Keyframe>, klipp_core::ClipId, f32, f32, f32, f32, klipp_timeline::FadeCurve, klipp_timeline::FadeCurve, klipp_timeline::ChannelConfig) {
                    let (static_val, kfs) = match rb_param {
                        klipp_core::AudioParam::Volume => (c.volume, c.volume_keyframes.clone()),
                        klipp_core::AudioParam::Pan => (c.pan, c.pan_keyframes.clone()),
                        klipp_core::AudioParam::Gain => (c.gain, c.gain_keyframes.clone()),
                    };
                    (static_val, kfs, c.id, c.gain, c.pan, c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve, c.channel_config)
                };

                let (kf_static_val, kf_keyframes, kf_clip_id, kf_gain, kf_pan, kf_fade_in, kf_fade_out, kf_fade_in_curve, kf_fade_out_curve, kf_channel_config) = if let Some(linked_id) = clip.linked_clip_id {
                    let mut found = resolve_clip(clip);
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) {
                            found = resolve_clip(vc);
                            break;
                        }
                    }
                    found
                } else {
                    resolve_clip(clip)
                };
                // Also capture static volume for the flat-line drag command (always volume)
                let kf_volume = if let Some(linked_id) = clip.linked_clip_id {
                    let mut v = clip.volume;
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) { v = vc.volume; break; }
                    }
                    v
                } else {
                    clip.volume
                };
                let kf_keyframes = &kf_keyframes[..];


                // Auto-scale Y axis to actual keyframe values (with padding),
                // matching the video rubber band approach so small changes are visible.
                let mut val_min = kf_static_val;
                let mut val_max = kf_static_val;
                for kf in kf_keyframes.iter() {
                    if kf.value < val_min { val_min = kf.value; }
                    if kf.value > val_max { val_max = kf.value; }
                }
                // Add 20% padding on each side so curves don't touch edges
                let val_span = (val_max - val_min).max(0.01);
                let val_padding = val_span * 0.2;
                let param_min = val_min - val_padding;
                let param_range = val_span + val_padding * 2.0;

                let clip_dur_secs = clip.duration().to_secs_f64();
                let clip_painter = painter.with_clip_rect(clip_rect);

                // Map param value to Y position within clip rect
                // min = bottom, max = top
                let value_to_y = |val: f32| -> f32 {
                    let normalized = ((val - param_min) / param_range).clamp(0.0, 1.0);
                    clip_rect.bottom() - normalized * clip_rect.height()
                };

                // Map time (relative to clip start) to X position
                let time_to_x = |t: f64| -> f32 {
                    let frac = if clip_dur_secs > 0.0 { (t / clip_dur_secs).clamp(0.0, 1.0) } else { 0.0 };
                    clip_rect.left() + frac as f32 * clip_rect.width()
                };

                // Color per param: amber=Volume, cyan=Pan, green=Gain
                let (line_color, line_color_dim, diamond_fill_sel, diamond_fill_norm, _diamond_stroke_color) = match rb_param {
                    klipp_core::AudioParam::Volume => (
                        egui::Color32::from_rgba_unmultiplied(255, 195, 50, 220),
                        egui::Color32::from_rgba_unmultiplied(240, 180, 50, 140),
                        egui::Color32::from_rgba_unmultiplied(255, 210, 60, 250),
                        egui::Color32::from_rgba_unmultiplied(255, 195, 50, 200),
                        egui::Color32::from_rgba_unmultiplied(200, 150, 30, 240),
                    ),
                    klipp_core::AudioParam::Pan => (
                        egui::Color32::from_rgba_unmultiplied(50, 195, 255, 220),
                        egui::Color32::from_rgba_unmultiplied(50, 180, 240, 140),
                        egui::Color32::from_rgba_unmultiplied(60, 210, 255, 250),
                        egui::Color32::from_rgba_unmultiplied(50, 195, 255, 200),
                        egui::Color32::from_rgba_unmultiplied(30, 150, 200, 240),
                    ),
                    klipp_core::AudioParam::Gain => (
                        egui::Color32::from_rgba_unmultiplied(50, 220, 100, 220),
                        egui::Color32::from_rgba_unmultiplied(50, 200, 90, 140),
                        egui::Color32::from_rgba_unmultiplied(60, 240, 110, 250),
                        egui::Color32::from_rgba_unmultiplied(50, 220, 100, 200),
                        egui::Color32::from_rgba_unmultiplied(30, 170, 60, 240),
                    ),
                };
                let shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);
                let diamond_fill = if is_selected || is_multi_selected { diamond_fill_sel } else { diamond_fill_norm };
                let _diamond_shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
                let active_line_color = if is_selected || is_multi_selected { line_color } else { line_color_dim };
                let rb_line_width = if is_selected || is_multi_selected { 2.5 } else { 2.0 };
                let rb_shadow_width = rb_line_width + 1.0;

                // Draw param pill badge at top-left of clip (below clip name area)
                let param_label = match rb_param {
                    klipp_core::AudioParam::Volume => "Vol",
                    klipp_core::AudioParam::Pan => "Pan",
                    klipp_core::AudioParam::Gain => "Gain",
                };
                // Measure text width to size the pill dynamically
                let pill_font = egui::FontId::proportional(9.0);
                let pill_text_galley = ui.painter().layout_no_wrap(param_label.to_string(), pill_font.clone(), egui::Color32::WHITE);
                let pill_w = pill_text_galley.size().x + 8.0; // 4px padding each side
                let pill_h = 14.0;
                let pill_pos = egui::pos2(clip_rect.left() + 4.0, clip_rect.top() + 14.0);
                let pill_rect = egui::Rect::from_min_size(pill_pos, egui::vec2(pill_w, pill_h));
                // Only draw and handle clicks if clip is wide enough
                if clip_rect.width() > 30.0 {
                    // Background: rubber band color at ~40% opacity
                    let pill_bg = match rb_param {
                        klipp_core::AudioParam::Volume => egui::Color32::from_rgba_unmultiplied(255, 195, 50, 100),
                        klipp_core::AudioParam::Pan => egui::Color32::from_rgba_unmultiplied(50, 195, 255, 100),
                        klipp_core::AudioParam::Gain => egui::Color32::from_rgba_unmultiplied(50, 220, 100, 100),
                    };
                    clip_painter.rect_filled(pill_rect, 4.0, pill_bg);
                    clip_painter.text(
                        pill_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        param_label,
                        pill_font,
                        egui::Color32::WHITE,
                    );
                    // Click pill to cycle param
                    if interactive {
                        if let Some(pos) = pointer_pos {
                            if pill_rect.contains(pos) && ui.input(|i| i.pointer.primary_clicked()) {
                                state.rubber_band_param = match rb_param {
                                    klipp_core::AudioParam::Volume => klipp_core::AudioParam::Pan,
                                    klipp_core::AudioParam::Pan => klipp_core::AudioParam::Gain,
                                    klipp_core::AudioParam::Gain => klipp_core::AudioParam::Volume,
                                };
                            }
                        }
                    }
                }

                if kf_keyframes.is_empty() {
                    // No keyframes: draw a single horizontal line at the static value
                    let y = value_to_y(kf_static_val);
                    // Shadow line behind for contrast
                    clip_painter.line_segment(
                        [egui::pos2(clip_rect.left(), y), egui::pos2(clip_rect.right(), y)],
                        egui::Stroke::new(rb_shadow_width, shadow_color),
                    );
                    clip_painter.line_segment(
                        [egui::pos2(clip_rect.left(), y), egui::pos2(clip_rect.right(), y)],
                        egui::Stroke::new(rb_line_width, active_line_color),
                    );
                } else {
                    // Draw curve by densely sampling evaluate() — correctly renders
                    // all interpolation types (Linear, Bezier, Step) regardless of keyframe count.
                    let clip_time_range = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();

                    // Dense sampling: evaluate the keyframe curve at many points across the clip
                    let num_samples = (clip_rect.width() as usize / 2).clamp(20, 400);
                    let mut curve_pts: Vec<egui::Pos2> = Vec::with_capacity(num_samples + 1);
                    for si in 0..=num_samples {
                        let t_frac = si as f64 / num_samples as f64;
                        let t_secs = t_frac * clip_time_range;
                        let val = klipp_core::keyframe::evaluate(kf_keyframes, t_secs, kf_static_val);
                        let x = clip_rect.left() + t_frac as f32 * clip_rect.width();
                        let y = value_to_y(val);
                        curve_pts.push(egui::pos2(x, y));
                    }

                    // Draw shadow pass then color pass
                    for i in 0..curve_pts.len().saturating_sub(1) {
                        clip_painter.line_segment(
                            [curve_pts[i], curve_pts[i + 1]],
                            egui::Stroke::new(rb_shadow_width, shadow_color),
                        );
                    }
                    for i in 0..curve_pts.len().saturating_sub(1) {
                        clip_painter.line_segment(
                            [curve_pts[i], curve_pts[i + 1]],
                            egui::Stroke::new(rb_line_width, active_line_color),
                        );
                    }

                    // Draw diamond markers at each keyframe
                    // Check if pointer is near any keyframe for hover effect
                    let hovered_kf_idx: Option<usize> = pointer_pos.and_then(|pos| {
                        if !clip_rect.contains(pos) { return None; }
                        let mut best: Option<(usize, f32)> = None;
                        for (ki, kf) in kf_keyframes.iter().enumerate() {
                            if kf.time_secs >= 0.0 && kf.time_secs <= clip_time_range {
                                let kx = time_to_x(kf.time_secs);
                                let ky = value_to_y(kf.value);
                                let dist = ((pos.x - kx).powi(2) + (pos.y - ky).powi(2)).sqrt();
                                if dist < 10.0
                                    && best.is_none_or(|(_, bd)| dist < bd) {
                                        best = Some((ki, dist));
                                    }
                            }
                        }
                        best.map(|(i, _)| i)
                    });

                    for (ki, kf) in kf_keyframes.iter().enumerate() {
                        if kf.time_secs >= 0.0 && kf.time_secs <= clip_time_range {
                            let cx = time_to_x(kf.time_secs);
                            let cy = value_to_y(kf.value);
                            let is_hovered = hovered_kf_idx == Some(ki);
                            let is_kf_selected = state.selected_keyframe.as_ref().is_some_and(|sk| {
                                sk.clip_id == kf_clip_id && sk.param_kind == crate::state::KeyframeParamKind::Audio(rb_param) && sk.keyframe_index == ki
                            });
                            let d = if is_hovered || is_kf_selected { 9.0_f32 } else { 8.0_f32 };
                            // Dark outline diamond behind for contrast
                            let outline_d = d + 1.5;
                            let outline_diamond = vec![
                                egui::pos2(cx, cy - outline_d),
                                egui::pos2(cx + outline_d, cy),
                                egui::pos2(cx, cy + outline_d),
                                egui::pos2(cx - outline_d, cy),
                            ];
                            clip_painter.add(egui::Shape::convex_polygon(
                                outline_diamond,
                                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
                                egui::Stroke::NONE,
                            ));
                            // Brighten fill on hover
                            let fill = if is_hovered {
                                egui::Color32::from_rgba_unmultiplied(
                                    diamond_fill.r().saturating_add(30),
                                    diamond_fill.g().saturating_add(30),
                                    diamond_fill.b().saturating_add(30),
                                    255,
                                )
                            } else {
                                diamond_fill
                            };
                            let diamond = vec![
                                egui::pos2(cx, cy - d),
                                egui::pos2(cx + d, cy),
                                egui::pos2(cx, cy + d),
                                egui::pos2(cx - d, cy),
                            ];
                            let stroke = if is_kf_selected {
                                egui::Stroke::new(2.0, egui::Color32::WHITE)
                            } else {
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220))
                            };
                            clip_painter.add(egui::Shape::convex_polygon(diamond, fill, stroke));

                            // ── Bezier tangent handles (only when clip is selected) ──
                            if kf.interpolation == klipp_core::Interpolation::Bezier && (is_selected || is_multi_selected) {
                                let pixels_per_sec = if clip_dur_secs > 0.0 { clip_rect.width() / clip_dur_secs as f32 } else { 1.0 };
                                let value_scale = clip_rect.height() / param_range;
                                let tangent_line_color = egui::Color32::from_rgba_unmultiplied(
                                    active_line_color.r(), active_line_color.g(), active_line_color.b(), 120,
                                );
                                let handle_radius = 4.0_f32;

                                // Tangent out handle (right)
                                let tout_x = cx + kf.tangent_out.0 as f32 * pixels_per_sec;
                                let tout_y = cy - kf.tangent_out.1 * value_scale;
                                clip_painter.line_segment(
                                    [egui::pos2(cx, cy), egui::pos2(tout_x, tout_y)],
                                    egui::Stroke::new(1.0, tangent_line_color),
                                );
                                clip_painter.circle(
                                    egui::pos2(tout_x, tout_y),
                                    handle_radius,
                                    active_line_color,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 200)),
                                );

                                // Tangent in handle (left)
                                let tin_x = cx + kf.tangent_in.0 as f32 * pixels_per_sec;
                                let tin_y = cy - kf.tangent_in.1 * value_scale;
                                clip_painter.line_segment(
                                    [egui::pos2(cx, cy), egui::pos2(tin_x, tin_y)],
                                    egui::Stroke::new(1.0, tangent_line_color),
                                );
                                clip_painter.circle(
                                    egui::pos2(tin_x, tin_y),
                                    handle_radius,
                                    active_line_color,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 200)),
                                );
                            }
                        }
                    }
                }

                // ── Rubber band interaction ──────────────────────────────────
                if interactive {
                    if let Some(pos) = pointer_pos {
                        if clip_rect.contains(pos) && content_rect.contains(pos) {
                            let alt_held = ui.input(|i| i.modifiers.alt);
                            let src_in_secs = clip.source_in.to_secs_f64();
                            let src_out_secs = clip.source_out.to_secs_f64();

                            // Helper: X position to time (relative to source_in)
                            let x_to_time = |x: f32| -> f64 {
                                let frac = ((x - clip_rect.left()) / clip_rect.width()).clamp(0.0, 1.0) as f64;
                                frac * clip_dur_secs
                            };
                            // Helper: Y position to param value
                            let y_to_value = |y: f32| -> f32 {
                                let normalized = ((clip_rect.bottom() - y) / clip_rect.height()).clamp(0.0, 1.0);
                                param_min + normalized * param_range
                            };

                            // Check if hovering near a bezier tangent handle
                            let mut nearest_bezier_handle: Option<(usize, bool, f32)> = None; // (kf_idx, is_out, dist)
                            if (is_selected || is_multi_selected) && state.dragging_bezier_handle.is_none() {
                                let pixels_per_sec_h = if clip_dur_secs > 0.0 { clip_rect.width() / clip_dur_secs as f32 } else { 1.0 };
                                let value_scale_h = clip_rect.height() / param_range;
                                for (ki, kf) in kf_keyframes.iter().enumerate() {
                                    if kf.interpolation != klipp_core::Interpolation::Bezier { continue; }
                                    if kf.time_secs < 0.0 || kf.time_secs > (src_out_secs - src_in_secs) { continue; }
                                    let cx_h = time_to_x(kf.time_secs);
                                    let cy_h = value_to_y(kf.value);
                                    // Tangent out handle
                                    let tout_hx = cx_h + kf.tangent_out.0 as f32 * pixels_per_sec_h;
                                    let tout_hy = cy_h - kf.tangent_out.1 * value_scale_h;
                                    let dist_out = ((pos.x - tout_hx).powi(2) + (pos.y - tout_hy).powi(2)).sqrt();
                                    if dist_out < 8.0
                                        && nearest_bezier_handle.is_none_or(|(_, _, d)| dist_out < d) {
                                            nearest_bezier_handle = Some((ki, true, dist_out));
                                        }
                                    // Tangent in handle
                                    let tin_hx = cx_h + kf.tangent_in.0 as f32 * pixels_per_sec_h;
                                    let tin_hy = cy_h - kf.tangent_in.1 * value_scale_h;
                                    let dist_in = ((pos.x - tin_hx).powi(2) + (pos.y - tin_hy).powi(2)).sqrt();
                                    if dist_in < 8.0
                                        && nearest_bezier_handle.is_none_or(|(_, _, d)| dist_in < d) {
                                            nearest_bezier_handle = Some((ki, false, dist_in));
                                        }
                                }
                            }

                            // Check if hovering near a keyframe diamond (for drag start)
                            let mut nearest_kf_idx: Option<usize> = None;
                            let mut nearest_kf_dist = f32::MAX;
                            for (ki, kf) in kf_keyframes.iter().enumerate() {
                                if kf.time_secs >= 0.0 && kf.time_secs <= (src_out_secs - src_in_secs) {
                                    let kx = time_to_x(kf.time_secs);
                                    let ky = value_to_y(kf.value);
                                    let dist = ((pos.x - kx).powi(2) + (pos.y - ky).powi(2)).sqrt();
                                    if dist < 10.0 && dist < nearest_kf_dist {
                                        nearest_kf_dist = dist;
                                        nearest_kf_idx = Some(ki);
                                    }
                                }
                            }

                            // Check if hovering near the rubber band line
                            let rubber_band_y = if kf_keyframes.is_empty() {
                                value_to_y(kf_static_val)
                            } else {
                                let t = x_to_time(pos.x);
                                value_to_y(klipp_core::keyframe::evaluate(kf_keyframes, t, kf_static_val))
                            };
                            let near_line = (pos.y - rubber_band_y).abs() < 6.0;

                            // ── Cursor feedback ──
                            let is_dragging_this_rb = state.dragging_rubber_band == Some(kf_clip_id);
                            let is_dragging_this_kf = state.dragging_audio_keyframe.as_ref().is_some_and(|d| d.clip_id == kf_clip_id);
                            let is_dragging_bezier = state.dragging_bezier_handle.as_ref().is_some_and(|d| d.clip_id == kf_clip_id);
                            if is_dragging_this_rb || is_dragging_this_kf || is_dragging_bezier {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                            } else if nearest_bezier_handle.is_some() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                            } else if nearest_kf_idx.is_some() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                            } else if near_line {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                            }

                            // ── Double-click diamond to delete keyframe ──
                            if let Some(ki) = nearest_kf_idx {
                                if ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                                    let kf = &kf_keyframes[ki];
                                    cmds.push(AppCommand::RemoveAudioKeyframe {
                                        clip_id: kf_clip_id,
                                        param: rb_param,
                                        time_secs: kf.time_secs,
                                    });
                                }
                            }

                            // ── Click keyframe diamond to SELECT it ──
                            if let Some(ki) = nearest_kf_idx {
                                if ui.input(|i| i.pointer.primary_clicked()) && state.dragging_audio_keyframe.is_none() {
                                    state.selected_keyframe = Some(crate::state::SelectedKeyframe {
                                        clip_id: kf_clip_id,
                                        param_kind: crate::state::KeyframeParamKind::Audio(rb_param),
                                        keyframe_index: ki,
                                    });
                                }
                            }

                            // ── Right-click: open context menu for selected keyframe or nearest diamond ──
                            if ui.input(|i| i.pointer.secondary_released()) {
                                let target_ki = nearest_kf_idx.or_else(|| {
                                    // If no diamond nearby, use the selected keyframe if it's on this clip
                                    state.selected_keyframe.as_ref()
                                        .filter(|sk| sk.clip_id == kf_clip_id && sk.param_kind == crate::state::KeyframeParamKind::Audio(rb_param))
                                        .map(|sk| sk.keyframe_index)
                                });
                                if let Some(ki) = target_ki {
                                    if ki < kf_keyframes.len() {
                                        state.keyframe_context_menu = Some(crate::state::KeyframeContextMenu {
                                            clip_id: kf_clip_id,
                                            param_kind: crate::state::KeyframeParamKind::Audio(rb_param),
                                            keyframe_index: ki,
                                            screen_pos: pos,
                                        });
                                        self.suppress_clip_context_menu = true;
                                    }
                                }
                            }

                            // ── Click empty area to deselect keyframe ──
                            if ui.input(|i| i.pointer.primary_clicked()) && nearest_kf_idx.is_none() && nearest_bezier_handle.is_none()
                                && state.selected_keyframe.as_ref().is_some_and(|sk| sk.clip_id == kf_clip_id) {
                                    state.selected_keyframe = None;
                                }

                            // ── Start bezier tangent handle drag ──
                            if let Some((bh_ki, bh_is_out, _)) = nearest_bezier_handle {
                                if pointer_down && state.dragging_bezier_handle.is_none()
                                    && state.dragging_audio_keyframe.is_none()
                                    && state.dragging_fade_handle.is_none()
                                {
                                    let drag_key = format!("kf:tangent:{}:{:?}:{}", kf_clip_id.0, rb_param, bh_ki);
                                    cmds.push(AppCommand::BeginKeyframeDrag { key: drag_key, clip_id: kf_clip_id });
                                    state.dragging_bezier_handle = Some(crate::state::BezierHandleDrag {
                                        clip_id: kf_clip_id,
                                        param: rb_param,
                                        keyframe_index: bh_ki,
                                        is_out: bh_is_out,
                                    });
                                }
                            }

                            // Start keyframe drag only after mouse moves beyond threshold (3px)
                            // This allows click-to-select without immediately starting a drag
                            if nearest_kf_idx.is_some() && nearest_bezier_handle.is_none()
                                && pointer_down && state.dragging_audio_keyframe.is_none()
                                && state.dragging_bezier_handle.is_none() && state.dragging_fade_handle.is_none()
                                && !alt_held
                            {
                                let dragged_far_enough = ui.input(|i| {
                                    i.pointer.is_decidedly_dragging()
                                });
                                if dragged_far_enough {
                                    if let Some(ki) = nearest_kf_idx {
                                        let drag_key = format!("kf:audio:{}:{:?}:{}", kf_clip_id.0, rb_param, ki);
                                        cmds.push(AppCommand::BeginKeyframeDrag { key: drag_key, clip_id: kf_clip_id });
                                        state.dragging_audio_keyframe = Some(crate::state::DraggingKeyframe {
                                            clip_id: kf_clip_id,
                                            param: rb_param,
                                            keyframe_index: ki,
                                        });
                                    }
                                }
                            }

                            // ── Flat-line drag: drag rubber band to change volume (no keyframes) ──
                            if kf_keyframes.is_empty() && near_line && pointer_down
                                && state.dragging_audio_keyframe.is_none()
                                && state.dragging_rubber_band.is_none()
                                && state.dragging_fade_handle.is_none()
                                && !state.drag_drop.is_active() // don't start rubber band during drag-and-drop
                                && !alt_held
                            {
                                state.dragging_rubber_band = Some(kf_clip_id);
                            }

                            // Process active flat-line rubber band drag (blocked during fade handle drag and drag-and-drop)
                            if state.dragging_rubber_band == Some(kf_clip_id) && state.dragging_fade_handle.is_none() && !state.drag_drop.is_active()
                                && pointer_down {
                                    let new_val = y_to_value(pos.y);
                                    let (drag_gain, drag_volume, drag_pan) = match rb_param {
                                        klipp_core::AudioParam::Volume => (kf_gain, new_val, kf_pan),
                                        klipp_core::AudioParam::Pan => (kf_gain, kf_volume, new_val),
                                        klipp_core::AudioParam::Gain => (new_val, kf_volume, kf_pan),
                                    };
                                    cmds.push(AppCommand::UpdateClipAudio {
                                        clip_id: kf_clip_id,
                                        gain: drag_gain,
                                        volume: drag_volume,
                                        pan: drag_pan,
                                        fade_in_secs: kf_fade_in,
                                        fade_out_secs: kf_fade_out,
                                        fade_in_curve: kf_fade_in_curve,
                                        fade_out_curve: kf_fade_out_curve,
                                        channel_config: kf_channel_config,
                                    });
                                    // Show tooltip with param-appropriate format
                                    let tooltip_text = match rb_param {
                                        klipp_core::AudioParam::Volume => format!("{:.0}%", new_val * 100.0),
                                        klipp_core::AudioParam::Pan => {
                                            if new_val.abs() < 0.01 { "C".to_string() }
                                            else if new_val < 0.0 { format!("L{:.0}", -new_val * 100.0) }
                                            else { format!("R{:.0}", new_val * 100.0) }
                                        }
                                        klipp_core::AudioParam::Gain => format!("{:.1}x", new_val),
                                    };
                                    clip_painter.text(
                                        egui::pos2(pos.x, pos.y - 14.0),
                                        egui::Align2::CENTER_BOTTOM,
                                        tooltip_text,
                                        egui::FontId::proportional(10.0),
                                        active_line_color,
                                    );
                                }

                            // Alt+click on line to add keyframe (blocked during fade drag)
                            if alt_held && near_line && state.dragging_fade_handle.is_none() && ui.input(|i| i.pointer.primary_clicked()) {
                                let t = x_to_time(pos.x);
                                let v = y_to_value(pos.y);
                                cmds.push(AppCommand::AddAudioKeyframe {
                                    clip_id: kf_clip_id,
                                    param: rb_param,
                                    time_secs: t,
                                    value: v,
                                });
                            }

                            // Show value tooltip when hovering near a keyframe
                            if let Some(ki) = nearest_kf_idx {
                                let kf = &kf_keyframes[ki];
                                let tooltip_text = match rb_param {
                                    klipp_core::AudioParam::Volume => format!("{:.1}%", kf.value * 100.0),
                                    klipp_core::AudioParam::Pan => {
                                        if kf.value.abs() < 0.01 { "C".to_string() }
                                        else if kf.value < 0.0 { format!("L{:.0}", -kf.value * 100.0) }
                                        else { format!("R{:.0}", kf.value * 100.0) }
                                    }
                                    klipp_core::AudioParam::Gain => format!("{:.2}x", kf.value),
                                };
                                let kx = time_to_x(kf.time_secs);
                                let ky = value_to_y(kf.value);
                                clip_painter.text(
                                    egui::pos2(kx, ky - 14.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    tooltip_text,
                                    egui::FontId::proportional(9.0),
                                    active_line_color,
                                );
                            }
                        }
                    }
                }
            }

            self.draw_audio_fade_overlay(painter, clip_rect, clip, state, track_kind);

            // ── Fade drag handles (triangular grab handles at top-left / top-right) ──
            // Visible when clip is hovered or selected, and clip has audio (audio track or linked audio)
            let clip_has_audio = track_kind == TrackKind::Audio
                || clip.media_id().and_then(|mid| state.media_pool.get(&mid)).is_some_and(|m| m.has_audio);
            let is_pointer_over_clip = pointer_pos.is_some_and(|p| clip_rect.contains(p) && content_rect.contains(p));
            let is_hovered_or_selected = is_pointer_over_clip || state.selected_clip == Some(clip.id);
            let is_dragging_this_fade = state.dragging_fade_handle.is_some_and(|(id, _, _)| id == clip.id);
            if clip_has_audio && !clip.kind.is_gap() && clip_rect.width() > 20.0
                && (is_hovered_or_selected || is_dragging_this_fade)
            {
                let handle_size = 6.0_f32;
                let handle_color = egui::Color32::from_rgba_unmultiplied(255, 195, 50, 220);
                let handle_hover_color = egui::Color32::from_rgb(255, 220, 80);

                // Resolve current fade values for visual positioning
                let (cur_fi, cur_fo) = if let Some(linked_id) = clip.linked_clip_id {
                    let mut fi = clip.fade_in_secs;
                    let mut fo = clip.fade_out_secs;
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) {
                            fi = vc.fade_in_secs;
                            fo = vc.fade_out_secs;
                            break;
                        }
                    }
                    (fi, fo)
                } else {
                    (clip.fade_in_secs, clip.fade_out_secs)
                };
                let clip_dur_secs = clip.duration().to_secs_f64() as f32;

                // Fade-in handle position: at the fade boundary (or clip left if no fade)
                let fi_offset_px = if clip_dur_secs > 0.001 { (cur_fi / clip_dur_secs) * clip_rect.width() } else { 0.0 };
                let fi_x = clip_rect.left() + fi_offset_px;
                // Fade-out handle position: at the fade boundary (or clip right if no fade)
                let fo_offset_px = if clip_dur_secs > 0.001 { (cur_fo / clip_dur_secs) * clip_rect.width() } else { 0.0 };
                let fo_x = clip_rect.right() - fo_offset_px;

                // Check if pointer is near a handle for hover highlight
                let near_fi = pointer_pos.is_some_and(|p| {
                    (p.x - fi_x).abs() < 8.0 && (p.y - clip_rect.top()).abs() < 10.0
                        && clip_rect.contains(p) && content_rect.contains(p)
                });
                let near_fo = pointer_pos.is_some_and(|p| {
                    (p.x - fo_x).abs() < 8.0 && (p.y - clip_rect.top()).abs() < 10.0
                        && clip_rect.contains(p) && content_rect.contains(p)
                });
                let dragging_fi = state.dragging_fade_handle.is_some_and(|(id, is_in, _)| id == clip.id && is_in);
                let dragging_fo = state.dragging_fade_handle.is_some_and(|(id, is_in, _)| id == clip.id && !is_in);

                // Draw fade-in handle (triangle pointing right at top-left area)
                let fi_color = if near_fi || dragging_fi { handle_hover_color } else { handle_color };
                let fi_tri = vec![
                    egui::pos2(fi_x, clip_rect.top()),
                    egui::pos2(fi_x + handle_size, clip_rect.top()),
                    egui::pos2(fi_x, clip_rect.top() + handle_size),
                ];
                painter.add(egui::Shape::convex_polygon(fi_tri, fi_color, egui::Stroke::NONE));

                // Draw fade-out handle (triangle pointing left at top-right area)
                let fo_color = if near_fo || dragging_fo { handle_hover_color } else { handle_color };
                let fo_tri = vec![
                    egui::pos2(fo_x, clip_rect.top()),
                    egui::pos2(fo_x - handle_size, clip_rect.top()),
                    egui::pos2(fo_x, clip_rect.top() + handle_size),
                ];
                painter.add(egui::Shape::convex_polygon(fo_tri, fo_color, egui::Stroke::NONE));

                // Cursor feedback when hovering handles
                if (near_fi || near_fo) && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }

                // Hover tooltip labels for fade handles (only when not dragging)
                if !dragging_fi && !dragging_fo && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
                    let tip_color = egui::Color32::from_rgba_premultiplied(255, 255, 255, 180);
                    let tip_font = egui::FontId::proportional(8.0);
                    if near_fi {
                        painter.text(
                            egui::pos2(fi_x + 10.0, clip_rect.top() + 6.0),
                            egui::Align2::LEFT_CENTER,
                            "Drag to set fade in",
                            tip_font,
                            tip_color,
                        );
                    } else if near_fo {
                        painter.text(
                            egui::pos2(fo_x - 10.0, clip_rect.top() + 6.0),
                            egui::Align2::RIGHT_CENTER,
                            "Drag to set fade out",
                            tip_font,
                            tip_color,
                        );
                    }
                }

                // Draw visual feedback during fade handle drag
                if dragging_fi || dragging_fo {
                    if let Some(pos) = pointer_pos {
                        let boundary_x = if dragging_fi {
                            pos.x.clamp(clip_rect.left(), clip_rect.right())
                        } else {
                            pos.x.clamp(clip_rect.left(), clip_rect.right())
                        };
                        // Vertical line at current drag position
                        painter.line_segment(
                            [egui::pos2(boundary_x, clip_rect.top()), egui::pos2(boundary_x, clip_rect.bottom())],
                            egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 195, 50, 160)),
                        );
                        // Tooltip with duration
                        let frac = if dragging_fi {
                            ((boundary_x - clip_rect.left()) / clip_rect.width()).clamp(0.0, 1.0)
                        } else {
                            ((clip_rect.right() - boundary_x) / clip_rect.width()).clamp(0.0, 1.0)
                        };
                        let fade_dur = frac * clip_dur_secs;
                        let label = if dragging_fi {
                            format!("Fade In: {:.2}s", fade_dur)
                        } else {
                            format!("Fade Out: {:.2}s", fade_dur)
                        };
                        egui::show_tooltip_at_pointer(
                            ui.ctx(), ui.layer_id(), ui.id().with("fade_handle_tooltip"),
                            |ui: &mut egui::Ui| { ui.label(label); },
                        );
                    }
                }
            }

            // ── Video parameter rubber band overlay ──────────────────────
            // Drawn on video track clips that have keyframes for the selected video param.
            // Mirrors the audio rubber band pattern with cyan/blue color scheme.
            if track_kind != TrackKind::Audio && clip_rect.width() > 8.0 && !clip.kind.is_gap() {
                let vrb_param = state.video_rubber_band_param;

                // Read static values from builtin effects
                let vrb_opacity_static = clip.builtin_effects.iter()
                    .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                    .unwrap_or(1.0);
                let vrb_transform = clip.builtin_effects.iter()
                    .find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, .. } =>
                            Some((*position_x, *position_y, *scale_x, *rotation)),
                        _ => None,
                    })
                    .unwrap_or((0.0, 0.0, 1.0, 0.0));

                // Read keyframes for the selected video parameter
                let (vrb_static_val, vrb_keyframes) = match vrb_param {
                    klipp_core::VideoParam::Opacity => (vrb_opacity_static, clip.opacity_keyframes.clone()),
                    klipp_core::VideoParam::PositionX => (vrb_transform.0, clip.transform_pos_x_keyframes.clone()),
                    klipp_core::VideoParam::PositionY => (vrb_transform.1, clip.transform_pos_y_keyframes.clone()),
                    klipp_core::VideoParam::Scale => (vrb_transform.2, clip.transform_scale_keyframes.clone()),
                    klipp_core::VideoParam::Rotation => (vrb_transform.3, clip.transform_rotation_keyframes.clone()),
                };

                // Check if ANY video param has keyframes (for deciding whether to draw anything)
                let has_any_video_kf = !clip.opacity_keyframes.is_empty()
                    || !clip.transform_pos_x_keyframes.is_empty()
                    || !clip.transform_pos_y_keyframes.is_empty()
                    || !clip.transform_scale_keyframes.is_empty()
                    || !clip.transform_rotation_keyframes.is_empty();

                // Only draw when at least one video param has keyframes
                if has_any_video_kf {
                    let vrb_keyframes = &vrb_keyframes[..];

                    // Auto-scale Y axis to actual keyframe values (with padding)
                    // so small value changes are clearly visible on the timeline
                    let mut vrb_val_min = vrb_static_val;
                    let mut vrb_val_max = vrb_static_val;
                    for kf in vrb_keyframes.iter() {
                        if kf.value < vrb_val_min { vrb_val_min = kf.value; }
                        if kf.value > vrb_val_max { vrb_val_max = kf.value; }
                    }
                    // Add 20% padding on each side so curves don't touch edges
                    let vrb_val_span = (vrb_val_max - vrb_val_min).max(0.01);
                    let vrb_padding = vrb_val_span * 0.2;
                    let vrb_param_min = vrb_val_min - vrb_padding;
                    let vrb_param_range = vrb_val_span + vrb_padding * 2.0;

                    let vrb_clip_dur_secs = clip.duration().to_secs_f64();
                    let vrb_clip_painter = painter.with_clip_rect(clip_rect);

                    // Map param value to Y position within clip rect (min=bottom, max=top)
                    let vrb_value_to_y = |val: f32| -> f32 {
                        let normalized = ((val - vrb_param_min) / vrb_param_range).clamp(0.0, 1.0);
                        clip_rect.bottom() - normalized * clip_rect.height()
                    };

                    // Map time (relative to clip start) to X position
                    let vrb_time_to_x = |t: f64| -> f32 {
                        let frac = if vrb_clip_dur_secs > 0.0 { (t / vrb_clip_dur_secs).clamp(0.0, 1.0) } else { 0.0 };
                        clip_rect.left() + frac as f32 * clip_rect.width()
                    };

                    // Cyan/blue color scheme for video (distinct from audio amber)
                    let (vrb_line_color, vrb_line_color_dim, vrb_diamond_fill_sel, vrb_diamond_fill_norm) = match vrb_param {
                        klipp_core::VideoParam::Opacity => (
                            egui::Color32::from_rgba_unmultiplied(80, 180, 255, 220),
                            egui::Color32::from_rgba_unmultiplied(70, 160, 230, 140),
                            egui::Color32::from_rgba_unmultiplied(90, 200, 255, 250),
                            egui::Color32::from_rgba_unmultiplied(80, 180, 255, 200),
                        ),
                        klipp_core::VideoParam::PositionX | klipp_core::VideoParam::PositionY => (
                            egui::Color32::from_rgba_unmultiplied(100, 220, 200, 220),
                            egui::Color32::from_rgba_unmultiplied(90, 200, 180, 140),
                            egui::Color32::from_rgba_unmultiplied(110, 240, 220, 250),
                            egui::Color32::from_rgba_unmultiplied(100, 220, 200, 200),
                        ),
                        klipp_core::VideoParam::Scale => (
                            egui::Color32::from_rgba_unmultiplied(140, 160, 255, 220),
                            egui::Color32::from_rgba_unmultiplied(120, 140, 230, 140),
                            egui::Color32::from_rgba_unmultiplied(160, 180, 255, 250),
                            egui::Color32::from_rgba_unmultiplied(140, 160, 255, 200),
                        ),
                        klipp_core::VideoParam::Rotation => (
                            egui::Color32::from_rgba_unmultiplied(180, 130, 255, 220),
                            egui::Color32::from_rgba_unmultiplied(160, 110, 230, 140),
                            egui::Color32::from_rgba_unmultiplied(200, 150, 255, 250),
                            egui::Color32::from_rgba_unmultiplied(180, 130, 255, 200),
                        ),
                    };
                    let vrb_shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);
                    let vrb_diamond_fill = if is_selected || is_multi_selected { vrb_diamond_fill_sel } else { vrb_diamond_fill_norm };
                    let vrb_active_line_color = if is_selected || is_multi_selected { vrb_line_color } else { vrb_line_color_dim };
                    let vrb_line_width = if is_selected || is_multi_selected { 2.5 } else { 2.0 };
                    let vrb_shadow_width = vrb_line_width + 1.0;

                    // Draw param pill badge at bottom-left of clip
                    let vrb_param_label = match vrb_param {
                        klipp_core::VideoParam::Opacity => "Opa",
                        klipp_core::VideoParam::PositionX => "PosX",
                        klipp_core::VideoParam::PositionY => "PosY",
                        klipp_core::VideoParam::Scale => "Scl",
                        klipp_core::VideoParam::Rotation => "Rot",
                    };
                    let vrb_pill_font = egui::FontId::proportional(9.0);
                    let vrb_pill_galley = ui.painter().layout_no_wrap(vrb_param_label.to_string(), vrb_pill_font.clone(), egui::Color32::WHITE);
                    let vrb_pill_w = vrb_pill_galley.size().x + 8.0;
                    let vrb_pill_h = 14.0;
                    let vrb_pill_pos = egui::pos2(clip_rect.left() + 4.0, clip_rect.bottom() - vrb_pill_h - 4.0);
                    let vrb_pill_rect = egui::Rect::from_min_size(vrb_pill_pos, egui::vec2(vrb_pill_w, vrb_pill_h));
                    if clip_rect.width() > 30.0 {
                        let vrb_pill_bg = match vrb_param {
                            klipp_core::VideoParam::Opacity => egui::Color32::from_rgba_unmultiplied(80, 180, 255, 100),
                            klipp_core::VideoParam::PositionX | klipp_core::VideoParam::PositionY => egui::Color32::from_rgba_unmultiplied(100, 220, 200, 100),
                            klipp_core::VideoParam::Scale => egui::Color32::from_rgba_unmultiplied(140, 160, 255, 100),
                            klipp_core::VideoParam::Rotation => egui::Color32::from_rgba_unmultiplied(180, 130, 255, 100),
                        };
                        vrb_clip_painter.rect_filled(vrb_pill_rect, 4.0, vrb_pill_bg);
                        vrb_clip_painter.text(
                            vrb_pill_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            vrb_param_label,
                            vrb_pill_font,
                            egui::Color32::WHITE,
                        );
                        // Click pill to cycle video param
                        if interactive {
                            if let Some(pos) = pointer_pos {
                                if vrb_pill_rect.contains(pos) && ui.input(|i| i.pointer.primary_clicked()) {
                                    state.video_rubber_band_param = match vrb_param {
                                        klipp_core::VideoParam::Opacity => klipp_core::VideoParam::PositionX,
                                        klipp_core::VideoParam::PositionX => klipp_core::VideoParam::PositionY,
                                        klipp_core::VideoParam::PositionY => klipp_core::VideoParam::Scale,
                                        klipp_core::VideoParam::Scale => klipp_core::VideoParam::Rotation,
                                        klipp_core::VideoParam::Rotation => klipp_core::VideoParam::Opacity,
                                    };
                                }
                            }
                        }
                    }

                    if vrb_keyframes.is_empty() {
                        // Selected param has no keyframes — draw flat line at static value
                        let vy = vrb_value_to_y(vrb_static_val);
                        vrb_clip_painter.line_segment(
                            [egui::pos2(clip_rect.left(), vy), egui::pos2(clip_rect.right(), vy)],
                            egui::Stroke::new(vrb_shadow_width, vrb_shadow_color),
                        );
                        vrb_clip_painter.line_segment(
                            [egui::pos2(clip_rect.left(), vy), egui::pos2(clip_rect.right(), vy)],
                            egui::Stroke::new(vrb_line_width, vrb_active_line_color),
                        );
                    } else {
                        // Draw curve by densely sampling evaluate() — this correctly renders
                        // all interpolation types (Linear, Bezier, Step) regardless of keyframe count.
                        let vrb_clip_time_range = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();

                        // Dense sampling: evaluate the keyframe curve at many points across the clip
                        let num_samples = (clip_rect.width() as usize / 2).clamp(20, 400);
                        let mut vrb_curve_pts: Vec<egui::Pos2> = Vec::with_capacity(num_samples + 1);
                        for si in 0..=num_samples {
                            let t_frac = si as f64 / num_samples as f64;
                            let t_secs = t_frac * vrb_clip_time_range;
                            let val = klipp_core::keyframe::evaluate(vrb_keyframes, t_secs, vrb_static_val);
                            let x = clip_rect.left() + t_frac as f32 * clip_rect.width();
                            let y = vrb_value_to_y(val);
                            vrb_curve_pts.push(egui::pos2(x, y));
                        }

                        // Draw shadow pass then color pass
                        for i in 0..vrb_curve_pts.len().saturating_sub(1) {
                            vrb_clip_painter.line_segment(
                                [vrb_curve_pts[i], vrb_curve_pts[i + 1]],
                                egui::Stroke::new(vrb_shadow_width, vrb_shadow_color),
                            );
                        }
                        for i in 0..vrb_curve_pts.len().saturating_sub(1) {
                            vrb_clip_painter.line_segment(
                                [vrb_curve_pts[i], vrb_curve_pts[i + 1]],
                                egui::Stroke::new(vrb_line_width, vrb_active_line_color),
                            );
                        }

                        // Draw diamond markers at each keyframe
                        let vrb_hovered_kf_idx: Option<usize> = pointer_pos.and_then(|pos| {
                            if !clip_rect.contains(pos) { return None; }
                            let mut best: Option<(usize, f32)> = None;
                            for (ki, kf) in vrb_keyframes.iter().enumerate() {
                                if kf.time_secs >= 0.0 && kf.time_secs <= vrb_clip_time_range {
                                    let kx = vrb_time_to_x(kf.time_secs);
                                    let ky = vrb_value_to_y(kf.value);
                                    let dist = ((pos.x - kx).powi(2) + (pos.y - ky).powi(2)).sqrt();
                                    if dist < 10.0
                                        && best.is_none_or(|(_, bd)| dist < bd) {
                                            best = Some((ki, dist));
                                        }
                                }
                            }
                            best.map(|(i, _)| i)
                        });

                        for (ki, kf) in vrb_keyframes.iter().enumerate() {
                            if kf.time_secs >= 0.0 && kf.time_secs <= vrb_clip_time_range {
                                let cx = vrb_time_to_x(kf.time_secs);
                                let cy = vrb_value_to_y(kf.value);
                                let is_hovered = vrb_hovered_kf_idx == Some(ki);
                                let d = if is_hovered { 9.0_f32 } else { 8.0_f32 };
                                // Dark outline diamond behind for contrast
                                let outline_d = d + 1.5;
                                let outline_diamond = vec![
                                    egui::pos2(cx, cy - outline_d),
                                    egui::pos2(cx + outline_d, cy),
                                    egui::pos2(cx, cy + outline_d),
                                    egui::pos2(cx - outline_d, cy),
                                ];
                                vrb_clip_painter.add(egui::Shape::convex_polygon(
                                    outline_diamond,
                                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
                                    egui::Stroke::NONE,
                                ));
                                let fill = if is_hovered {
                                    egui::Color32::from_rgba_unmultiplied(
                                        vrb_diamond_fill.r().saturating_add(30),
                                        vrb_diamond_fill.g().saturating_add(30),
                                        vrb_diamond_fill.b().saturating_add(30),
                                        255,
                                    )
                                } else {
                                    vrb_diamond_fill
                                };
                                let diamond = vec![
                                    egui::pos2(cx, cy - d),
                                    egui::pos2(cx + d, cy),
                                    egui::pos2(cx, cy + d),
                                    egui::pos2(cx - d, cy),
                                ];
                                let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220));
                                vrb_clip_painter.add(egui::Shape::convex_polygon(diamond, fill, stroke));

                                // Show value tooltip on hover
                                if is_hovered {
                                    let tooltip_text = match vrb_param {
                                        klipp_core::VideoParam::Opacity => format!("{:.0}%", kf.value * 100.0),
                                        klipp_core::VideoParam::PositionX => format!("X:{:.0}", kf.value),
                                        klipp_core::VideoParam::PositionY => format!("Y:{:.0}", kf.value),
                                        klipp_core::VideoParam::Scale => format!("{:.2}x", kf.value),
                                        klipp_core::VideoParam::Rotation => format!("{:.1}\u{00B0}", kf.value),
                                    };
                                    vrb_clip_painter.text(
                                        egui::pos2(cx, cy - 14.0),
                                        egui::Align2::CENTER_BOTTOM,
                                        tooltip_text,
                                        egui::FontId::proportional(9.0),
                                        vrb_active_line_color,
                                    );
                                }

                                // ── Bezier tangent handles (only when clip is selected) ──
                                if kf.interpolation == klipp_core::Interpolation::Bezier && (is_selected || is_multi_selected) {
                                    let vrb_pixels_per_sec = if vrb_clip_dur_secs > 0.0 { clip_rect.width() / vrb_clip_dur_secs as f32 } else { 1.0 };
                                    let vrb_bz_value_scale = clip_rect.height() / vrb_param_range;
                                    let vrb_tangent_line_color = egui::Color32::from_rgba_unmultiplied(
                                        vrb_active_line_color.r(), vrb_active_line_color.g(), vrb_active_line_color.b(), 120,
                                    );
                                    let vrb_handle_radius = 4.0_f32;

                                    // Tangent out handle (right)
                                    let vrb_tout_x = cx + kf.tangent_out.0 as f32 * vrb_pixels_per_sec;
                                    let vrb_tout_y = cy - kf.tangent_out.1 * vrb_bz_value_scale;
                                    vrb_clip_painter.line_segment(
                                        [egui::pos2(cx, cy), egui::pos2(vrb_tout_x, vrb_tout_y)],
                                        egui::Stroke::new(1.0, vrb_tangent_line_color),
                                    );
                                    vrb_clip_painter.circle(
                                        egui::pos2(vrb_tout_x, vrb_tout_y),
                                        vrb_handle_radius,
                                        vrb_active_line_color,
                                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 200)),
                                    );

                                    // Tangent in handle (left)
                                    let vrb_tin_x = cx + kf.tangent_in.0 as f32 * vrb_pixels_per_sec;
                                    let vrb_tin_y = cy - kf.tangent_in.1 * vrb_bz_value_scale;
                                    vrb_clip_painter.line_segment(
                                        [egui::pos2(cx, cy), egui::pos2(vrb_tin_x, vrb_tin_y)],
                                        egui::Stroke::new(1.0, vrb_tangent_line_color),
                                    );
                                    vrb_clip_painter.circle(
                                        egui::pos2(vrb_tin_x, vrb_tin_y),
                                        vrb_handle_radius,
                                        vrb_active_line_color,
                                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 200)),
                                    );
                                }
                            }
                        }

                        // ── Video keyframe interaction ───────────────────────
                        if interactive {
                            if let Some(pos) = pointer_pos {
                                if clip_rect.contains(pos) {
                                    let vrb_clip_id = clip.id;
                                    let vrb_clip_time_range_i = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();

                                    // Check if hovering near a video bezier tangent handle
                                    let mut vrb_nearest_bezier_handle: Option<(usize, bool, f32)> = None; // (kf_idx, is_out, dist)
                                    if (is_selected || is_multi_selected) && state.dragging_video_bezier_handle.is_none() {
                                        let vrb_pps_h = if vrb_clip_dur_secs > 0.0 { clip_rect.width() / vrb_clip_dur_secs as f32 } else { 1.0 };
                                        let vrb_vs_h = clip_rect.height() / vrb_param_range;
                                        for (ki, kf) in vrb_keyframes.iter().enumerate() {
                                            if kf.interpolation != klipp_core::Interpolation::Bezier { continue; }
                                            if kf.time_secs < 0.0 || kf.time_secs > vrb_clip_time_range_i { continue; }
                                            let cx_h = vrb_time_to_x(kf.time_secs);
                                            let cy_h = vrb_value_to_y(kf.value);
                                            // Tangent out handle
                                            let tout_hx = cx_h + kf.tangent_out.0 as f32 * vrb_pps_h;
                                            let tout_hy = cy_h - kf.tangent_out.1 * vrb_vs_h;
                                            let dist_out = ((pos.x - tout_hx).powi(2) + (pos.y - tout_hy).powi(2)).sqrt();
                                            if dist_out < 8.0
                                                && vrb_nearest_bezier_handle.is_none_or(|(_, _, d)| dist_out < d) {
                                                    vrb_nearest_bezier_handle = Some((ki, true, dist_out));
                                                }
                                            // Tangent in handle
                                            let tin_hx = cx_h + kf.tangent_in.0 as f32 * vrb_pps_h;
                                            let tin_hy = cy_h - kf.tangent_in.1 * vrb_vs_h;
                                            let dist_in = ((pos.x - tin_hx).powi(2) + (pos.y - tin_hy).powi(2)).sqrt();
                                            if dist_in < 8.0
                                                && vrb_nearest_bezier_handle.is_none_or(|(_, _, d)| dist_in < d) {
                                                    vrb_nearest_bezier_handle = Some((ki, false, dist_in));
                                                }
                                        }
                                    }

                                    // Cursor feedback
                                    let is_dragging_vrb_kf = state.dragging_video_keyframe.as_ref().is_some_and(|d| d.clip_id == vrb_clip_id);
                                    let is_dragging_vrb_bezier = state.dragging_video_bezier_handle.as_ref().is_some_and(|d| d.clip_id == vrb_clip_id);
                                    if is_dragging_vrb_kf || is_dragging_vrb_bezier {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                    } else if vrb_nearest_bezier_handle.is_some() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                    } else if vrb_hovered_kf_idx.is_some() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                    }

                                    // Double-click diamond to delete keyframe
                                    if let Some(ki) = vrb_hovered_kf_idx {
                                        if ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                                            let kf = &vrb_keyframes[ki];
                                            cmds.push(AppCommand::RemoveVideoKeyframe {
                                                clip_id: vrb_clip_id,
                                                param: vrb_param,
                                                time_secs: kf.time_secs,
                                            });
                                        }
                                    }

                                    // Click diamond to select
                                    if let Some(ki) = vrb_hovered_kf_idx {
                                        if ui.input(|i| i.pointer.primary_clicked()) && state.dragging_video_keyframe.is_none() {
                                            state.selected_keyframe = Some(crate::state::SelectedKeyframe {
                                                clip_id: vrb_clip_id,
                                                param_kind: crate::state::KeyframeParamKind::Video(vrb_param),
                                                keyframe_index: ki,
                                            });
                                        }
                                    }

                                    // Right-click: open interpolation context menu
                                    if ui.input(|i| i.pointer.secondary_released()) {
                                        let target_ki = vrb_hovered_kf_idx.or_else(|| {
                                            state.selected_keyframe.as_ref()
                                                .filter(|sk| sk.clip_id == vrb_clip_id && sk.param_kind == crate::state::KeyframeParamKind::Video(vrb_param))
                                                .map(|sk| sk.keyframe_index)
                                        });
                                        if let Some(ki) = target_ki {
                                            if ki < vrb_keyframes.len() {
                                                state.keyframe_context_menu = Some(crate::state::KeyframeContextMenu {
                                                    clip_id: vrb_clip_id,
                                                    param_kind: crate::state::KeyframeParamKind::Video(vrb_param),
                                                    keyframe_index: ki,
                                                    screen_pos: pos,
                                                });
                                                self.suppress_clip_context_menu = true;
                                            }
                                        }
                                    }

                                    // Click empty area to deselect
                                    if ui.input(|i| i.pointer.primary_clicked()) && vrb_hovered_kf_idx.is_none() && vrb_nearest_bezier_handle.is_none()
                                        && state.selected_keyframe.as_ref().is_some_and(|sk| sk.clip_id == vrb_clip_id) {
                                            state.selected_keyframe = None;
                                        }

                                    // ── Start video bezier tangent handle drag ──
                                    if let Some((vbh_ki, vbh_is_out, _)) = vrb_nearest_bezier_handle {
                                        if pointer_down && state.dragging_video_bezier_handle.is_none()
                                            && state.dragging_video_keyframe.is_none()
                                            && state.dragging_fade_handle.is_none()
                                        {
                                            let drag_key = format!("kf:vtangent:{}:{:?}:{}", vrb_clip_id.0, vrb_param, vbh_ki);
                                            cmds.push(AppCommand::BeginKeyframeDrag { key: drag_key, clip_id: vrb_clip_id });
                                            state.dragging_video_bezier_handle = Some(crate::state::BezierVideoHandleDrag {
                                                clip_id: vrb_clip_id,
                                                param: vrb_param,
                                                keyframe_index: vbh_ki,
                                                is_out: vbh_is_out,
                                            });
                                        }
                                    }

                                    // Start drag on keyframe diamond
                                    let vrb_alt_held = ui.input(|i| i.modifiers.alt);
                                    if vrb_hovered_kf_idx.is_some() && vrb_nearest_bezier_handle.is_none()
                                        && pointer_down && state.dragging_video_keyframe.is_none()
                                        && state.dragging_audio_keyframe.is_none()
                                        && state.dragging_video_bezier_handle.is_none()
                                        && state.dragging_fade_handle.is_none()
                                        && !vrb_alt_held
                                        && ui.input(|i| i.pointer.is_decidedly_dragging()) {
                                            if let Some(ki) = vrb_hovered_kf_idx {
                                                let drag_key = format!("kf:video:{}:{:?}:{}", vrb_clip_id.0, vrb_param, ki);
                                                cmds.push(AppCommand::BeginKeyframeDrag { key: drag_key, clip_id: vrb_clip_id });
                                                state.dragging_video_keyframe = Some(crate::state::DraggingVideoKeyframe {
                                                    clip_id: vrb_clip_id,
                                                    param: vrb_param,
                                                    keyframe_index: ki,
                                                });
                                            }
                                        }

                                    // Alt+click on rubber band line to add video keyframe
                                    if vrb_alt_held && vrb_hovered_kf_idx.is_none() && ui.input(|i| i.pointer.primary_clicked()) {
                                        // Compute Y of curve at pointer X
                                        let vrb_x_frac = ((pos.x - clip_rect.left()) / clip_rect.width()).clamp(0.0, 1.0) as f64;
                                        let vrb_hover_t = vrb_x_frac * vrb_clip_time_range;
                                        let vrb_curve_y_at_ptr = vrb_value_to_y(
                                            klipp_core::keyframe::evaluate(vrb_keyframes, vrb_hover_t, vrb_static_val),
                                        );
                                        let vrb_near_line = (pos.y - vrb_curve_y_at_ptr).abs() < 6.0;
                                        if vrb_near_line {
                                            // Inverse Y-to-value: normalized = (clip_rect.bottom() - y) / clip_rect.height()
                                            let vrb_normalized = ((clip_rect.bottom() - pos.y) / clip_rect.height()).clamp(0.0, 1.0);
                                            let vrb_click_val = vrb_param_min + vrb_normalized * vrb_param_range;
                                            cmds.push(AppCommand::AddVideoKeyframe {
                                                clip_id: vrb_clip_id,
                                                param: vrb_param,
                                                time_secs: vrb_hover_t,
                                                value: vrb_click_val,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Per-clip markers ──
            self.draw_clip_markers(painter, clip_rect, clip);

            // ── Speed ramp overlay ──
            self.draw_speed_overlay(painter, clip_rect, clip);

            self.draw_clip_badges(ui, painter, clip_rect, clip, state, track_kind, is_title, is_offline, border_color, border_width);

            self.draw_clip_overlays(ui, painter, clip_rect, clip, state, cmds, track_idx, track_kind, content_rect, zoom, scroll, interactive, pointer_pos);

        }

        // Draw transition indicators between clips
        self.draw_transition_overlays(painter, content_rect, track_rect, track_idx, state, zoom, scroll, track_height);

        // Draw audio crossfade indicators at adjacent clip boundaries
        if track_kind == TrackKind::Audio && state.audio_show_crossfade_indicators {
            let frame_dur = 1.0 / fps.rate() as f64;
            for i in 0..clips.len().saturating_sub(1) {
                if clips[i].kind.is_gap() || clips[i + 1].kind.is_gap() { continue; }
                let end_secs = clips[i].timeline_out.to_secs_f64();
                let start_secs = clips[i + 1].timeline_in.to_secs_f64();
                // Adjacent within 1 frame tolerance
                if (end_secs - start_secs).abs() <= frame_dur * 1.5 {
                    let boundary_x = content_rect.left() + end_secs as f32 * zoom - scroll;
                    // Only draw if visible
                    if boundary_x >= content_rect.left() - 10.0 && boundary_x <= content_rect.right() + 10.0 {
                        let cy = track_rect.center().y;
                        let s = 5.0_f32; // half-size of the X
                        let xf_color = egui::Color32::from_rgba_unmultiplied(180, 220, 255, 140);
                        // Draw "X" crossfade indicator
                        painter.line_segment(
                            [egui::pos2(boundary_x - s, cy - s), egui::pos2(boundary_x + s, cy + s)],
                            egui::Stroke::new(1.5, xf_color),
                        );
                        painter.line_segment(
                            [egui::pos2(boundary_x - s, cy + s), egui::pos2(boundary_x + s, cy - s)],
                            egui::Stroke::new(1.5, xf_color),
                        );
                        // Small overlapping fade triangles
                        let tri_h = 4.0_f32;
                        let tri_w = 6.0_f32;
                        let top_y = cy - s - 4.0;
                        // Left fade-out triangle (filled)
                        let tri_out = vec![
                            egui::pos2(boundary_x - tri_w, top_y),
                            egui::pos2(boundary_x, top_y),
                            egui::pos2(boundary_x - tri_w, top_y + tri_h),
                        ];
                        painter.add(egui::Shape::convex_polygon(
                            tri_out, egui::Color32::from_rgba_unmultiplied(180, 220, 255, 80), egui::Stroke::NONE,
                        ));
                        // Right fade-in triangle (filled)
                        let tri_in = vec![
                            egui::pos2(boundary_x, top_y),
                            egui::pos2(boundary_x + tri_w, top_y),
                            egui::pos2(boundary_x + tri_w, top_y + tri_h),
                        ];
                        painter.add(egui::Shape::convex_polygon(
                            tri_in, egui::Color32::from_rgba_unmultiplied(180, 220, 255, 80), egui::Stroke::NONE,
                        ));
                    }
                }
            }
        }

        // Draw anchor connection lines for connected clips
        for clip in &clips {
            if let Some(ref anchor) = clip.magnetic_anchor {
                if let Some(primary_idx) = state.sequence().primary_track_index() {
                    let anchor_clip = state.sequence().tracks[primary_idx].clips.iter()
                        .find(|c| c.id == anchor.anchor_clip_id);
                    if let Some(ac) = anchor_clip {
                        let anchor_x = content_rect.left()
                            + (ac.timeline_in.to_secs_f64() as f32 + anchor.offset.to_secs_f64() as f32) * zoom
                            - scroll;

                        // Only draw if visible
                        if anchor_x >= content_rect.left() - 10.0 && anchor_x <= content_rect.right() + 10.0 {
                            let _clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                            let clip_top = track_rect.top();

                            // Find primary track rect for the bottom of the line
                            let primary_bottom = self.frame_cache.track_rects.iter()
                                .find(|(idx, _, _)| *idx == primary_idx)
                                .map(|(_, _, r)| r.bottom())
                                .unwrap_or(track_rect.bottom());

                            let line_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 160);

                            // Dashed vertical line
                            let dash_len = 5.0_f32;
                            let gap_len = 3.0_f32;
                            let start_y = clip_top.min(primary_bottom);
                            let end_y = clip_top.max(primary_bottom);
                            let mut y = start_y;
                            while y < end_y {
                                let dash_end = (y + dash_len).min(end_y);
                                painter.line_segment(
                                    [egui::pos2(anchor_x, y), egui::pos2(anchor_x, dash_end)],
                                    egui::Stroke::new(1.0, line_color),
                                );
                                y += dash_len + gap_len;
                            }

                            // Anchor dot on the primary track
                            painter.circle_filled(
                                egui::pos2(anchor_x, primary_bottom),
                                3.0,
                                line_color,
                            );
                        }
                    }
                }
            }
        }

        // Draw collision indicators for overlapping clips on connected tracks
        if state.sequence().tracks.get(track_idx).is_some_and(|t| {
            t.magnetic_role == klipp_timeline::TrackRole::Connected
                || t.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline
        }) {
            for (ci, clip) in clips.iter().enumerate() {
                if clip.kind.is_gap() { continue; }
                for (oi, other) in clips.iter().enumerate() {
                    if ci >= oi { continue; }
                    if other.kind.is_gap() { continue; }
                    // Check overlap
                    if clip.timeline_in < other.timeline_out && clip.timeline_out > other.timeline_in {
                        let overlap_start = if clip.timeline_in > other.timeline_in { clip.timeline_in } else { other.timeline_in };
                        let overlap_end = if clip.timeline_out < other.timeline_out { clip.timeline_out } else { other.timeline_out };
                        let ox = content_rect.left() + overlap_start.to_secs_f64() as f32 * zoom - scroll;
                        let ow = (overlap_end - overlap_start).to_secs_f64() as f32 * zoom;
                        let overlap_rect = egui::Rect::from_min_size(
                            egui::pos2(ox, track_rect.top() + 2.0),
                            egui::vec2(ow, track_rect.height() - 4.0),
                        );
                        if overlap_rect.right() < content_rect.left() || overlap_rect.left() > content_rect.right() {
                            continue;
                        }
                        // Amber striped overlay
                        let stripe_color = egui::Color32::from_rgba_unmultiplied(255, 200, 0, 80);
                        let stripe_spacing = 6.0_f32;
                        let clip_painter = painter.with_clip_rect(overlap_rect);
                        let mut sx = overlap_rect.min.x - overlap_rect.height();
                        while sx < overlap_rect.max.x {
                            let p1 = egui::pos2(sx, overlap_rect.max.y);
                            let p2 = egui::pos2(sx + overlap_rect.height(), overlap_rect.min.y);
                            clip_painter.line_segment([p1, p2], egui::Stroke::new(1.5, stripe_color));
                            sx += stripe_spacing;
                        }
                        // Amber border
                        painter.rect_stroke(overlap_rect, 2.0,
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(0xff, 0xc8, 0x00)));
                    }
                }
            }
        }

        self.handle_trim_hover(painter, ui, state, track_idx, &clips, content_rect, track_rect, zoom, scroll, track_height, pointer_pos, track_kind, interactive);

        if !interactive {
            return;
        }

        // Single interaction rect for the whole track — avoids per-clip hit-test priority
        // issues in egui. We do our own hit-testing against clip rects.
        let track_interact_id = ui.id().with(("track_interact", track_idx));
        let track_response = ui.interact(*track_rect, track_interact_id, egui::Sense::click_and_drag());

        // Capture press origin on initial mouse-down (before drag threshold)
        if pointer_down && self.press_origin.is_none() && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
            if let Some(pos) = pointer_pos {
                if track_rect.contains(pos) {
                    self.press_origin = Some(pos);
                }
            }
        }
        // Clear press origin when pointer released without starting drag/trim
        if !pointer_down && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
            self.press_origin = None;
        }

        // Debug: log all interaction states
        if track_response.drag_started() {
            tracing::debug!("track {} drag_started, pointer={:?}, press_origin={:?}", track_idx, pointer_pos, self.press_origin);
        }
        if track_response.clicked() {
            tracing::debug!("track {} clicked, pointer={:?}", track_idx, pointer_pos);
        }
        if track_response.hovered() && pointer_down {
            // Only log occasionally to avoid spam
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if c.is_multiple_of(60) {
                tracing::debug!("track {} hovered+pointer_down, dragging={}, trimming={}", track_idx, self.dragging_clip.is_some(), self.trimming_clip.is_some());
            }
        }

        self.show_clip_context_menu(ui, &track_response, state, cmds, track_idx, &clips, content_rect, zoom, scroll, track_height, track_rect, pointer_pos, fps);

        self.handle_clip_click(ui, &track_response, state, cmds, track_idx, &clips, content_rect, track_rect, zoom, scroll, track_height, fps);

        self.detect_drag_start(ui, &track_response, state, cmds, track_idx, &clips, content_rect, track_rect, zoom, scroll, track_height, track_kind, pointer_pos);

        self.process_fade_drag(state, cmds, &clips, content_rect, track_rect, zoom, scroll, track_height, pointer_pos, pointer_down, pointer_released);

        // Collect snap targets upfront (avoids borrow conflicts with state mutation)
        let snap_enabled = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
        let snap_threshold_secs = snap_threshold_px() as f64 / zoom as f64;
        // Collect all snap points: (time_secs, RationalTime) for each clip edge + playhead + zero
        let snap_targets: Vec<(f64, RationalTime, Option<ClipId>)> = {
            let mut targets = Vec::new();
            for track in &state.sequence().tracks {
                for clip in &track.clips {
                    targets.push((clip.timeline_in.to_secs_f64(), clip.timeline_in, Some(clip.id)));
                    targets.push((clip.timeline_out.to_secs_f64(), clip.timeline_out, Some(clip.id)));
                }
            }
            targets.push((state.playhead.to_secs_f64(), state.playhead, None));
            targets.push((0.0, RationalTime { value: 0, rate: fps.rate() }, None));
            // Snap to in/out markers if set
            if let Some(inp) = state.sequence().in_point {
                targets.push((inp.to_secs_f64(), inp, None));
            }
            if let Some(outp) = state.sequence().out_point {
                targets.push((outp.to_secs_f64(), outp, None));
            }
            targets
        };
        let _find_snap = |edge_time: RationalTime, exclude_clip: ClipId| -> Option<(RationalTime, f64)> {
            if !snap_enabled { return None; }
            let edge_secs = edge_time.to_secs_f64();
            let mut best: Option<(f64, RationalTime)> = None;
            for &(target_secs, target_tc, owner) in &snap_targets {
                if owner == Some(exclude_clip) { continue; }
                let dist = (edge_secs - target_secs).abs();
                if dist < snap_threshold_secs && (best.is_none() || dist < best.unwrap().0) {
                    best = Some((dist, target_tc));
                }
            }
            best.map(|(_, target)| (target, target.to_secs_f64()))
        };

        // Handle Range tool drag in track area (update in/out as user drags)
        if state.active_tool == Tool::Range {
            if let Some(ref drag) = state.range_drag.clone() {
                if let crate::state::RangeDragKind::RulerCreate { anchor_secs } = drag.kind {
                    if let Some(pos) = pointer_pos {
                        if pointer_down {
                            let x_in_content = pos.x - content_rect.left() + scroll;
                            let current_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                            let anchor_tc = RationalTime::from_secs_f64(anchor_secs, fps.rate());
                            let current_tc = RationalTime::from_secs_f64(current_secs, fps.rate());
                            if current_secs < anchor_secs {
                                cmds.push(AppCommand::SetSequenceInPoint(current_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(anchor_tc));
                            } else {
                                cmds.push(AppCommand::SetSequenceInPoint(anchor_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(current_tc));
                            }
                        }
                        if pointer_released {
                            if let Some(drag) = state.range_drag.take() {
                                let new_in = state.sequence().in_point;
                                let new_out = state.sequence().out_point;
                                if drag.original_in != new_in || drag.original_out != new_out {
                                    state.history.push(crate::history::HistoryEntry {
                                        sequence_idx: state.active_sequence_idx,
                                        command: crate::history::EditCommand::SetRange {
                                            old_in: drag.original_in,
                                            old_out: drag.original_out,
                                            new_in,
                                            new_out,
                                        },
                                    });
                                }
                            }
                        }
                    }
                }
            }
            // Show crosshair cursor when Range tool is active
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        self.process_trim_drag(ui, painter, state, cmds, track_idx, content_rect, track_rect, zoom, scroll, fps, snap_enabled, snap_threshold_secs, &snap_targets, pointer_pos, pointer_down, pointer_released);

        self.process_roll_trim(painter, state, cmds, track_idx, content_rect, track_rect, zoom, scroll, fps, snap_enabled, pointer_pos, pointer_down, pointer_released);

        self.process_slip_drag(ui, painter, state, cmds, track_idx, &clips, content_rect, track_rect, zoom, scroll, track_height, pointer_pos, pointer_down, pointer_released);

        self.process_slide_drag(ui, painter, state, cmds, track_idx, &clips, content_rect, track_rect, zoom, scroll, track_height, pointer_pos, pointer_down, pointer_released);

        self.process_clip_drag(ui, painter, state, cmds, track_idx, content_rect, track_rect, zoom, scroll, fps, snap_enabled, snap_threshold_secs, &snap_targets, track_height, pointer_pos, pointer_down, pointer_released);
    }

    /// Compute the visual rect for a clip on screen.
    fn clip_screen_rect(
        clip: &klipp_timeline::Clip,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
    ) -> egui::Rect {
        let clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
        let clip_w = clip.duration().to_secs_f64() as f32 * zoom;
        let gap = clip_gap();
        egui::Rect::from_min_size(
            egui::pos2(clip_x + gap, track_rect.top() + 2.0),
            egui::vec2((clip_w - gap * 2.0).max(2.0), track_height - 4.0),
        )
    }

    /// Paint a floating timecode overlay near a trim edge.
    /// Shows: timecode at the edge, delta from original, new clip duration.
    #[allow(clippy::too_many_arguments)]
    fn paint_trim_overlay(
        painter: &egui::Painter,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        edge_time: RationalTime,
        delta: RationalTime,
        duration: RationalTime,
        label: &str,
        _fps: klipp_core::Fps,
    ) {
        let edge_x = content_rect.left() + edge_time.to_secs_f64() as f32 * zoom - scroll;

        // Format the timecode
        let tc_str = edge_time.to_timecode_string();

        // Format delta as +/-frames
        let delta_frames = delta.value;
        let delta_sign = if delta_frames >= 0 { "+" } else { "" };
        // Convert delta to seconds:frames for readability
        let abs_frames = delta_frames.unsigned_abs() as i64;
        let rate = delta.rate.max(1) as i64;
        let d_secs = abs_frames / rate;
        let d_ff = abs_frames % rate;
        let delta_str = if d_secs > 0 {
            format!("{delta_sign}{d_secs}:{d_ff:02}")
        } else {
            format!("{delta_sign}{d_ff}")
        };

        // Format duration
        let dur_str = duration.to_timecode_string();

        // Build overlay text lines
        let line1 = format!("{label}: {tc_str}");
        let line2 = format!("\u{0394} {delta_str}");
        let line3 = format!("Dur: {dur_str}");

        let font = egui::FontId::proportional(9.0);
        let text_color = egui::Color32::WHITE;
        let bg_color = egui::Color32::from_rgba_unmultiplied(20, 20, 25, 220);
        let border_color = egui::Color32::from_rgba_unmultiplied(255, 200, 60, 180);

        // Measure text width for the panel
        let galley1 = painter.layout_no_wrap(line1.clone(), font.clone(), text_color);
        let galley2 = painter.layout_no_wrap(line2.clone(), font.clone(), text_color);
        let galley3 = painter.layout_no_wrap(line3.clone(), font.clone(), text_color);
        let max_w = galley1.rect.width()
            .max(galley2.rect.width())
            .max(galley3.rect.width());

        let pad = 4.0;
        let line_h = 12.0;
        let panel_w = max_w + pad * 2.0;
        let panel_h = line_h * 3.0 + pad * 2.0;

        // Position: above the track, near the trim edge
        let panel_x = (edge_x - panel_w / 2.0).clamp(content_rect.left(), content_rect.right() - panel_w);
        let panel_y = track_rect.top() - panel_h - 4.0;

        let panel_rect = egui::Rect::from_min_size(
            egui::pos2(panel_x, panel_y),
            egui::vec2(panel_w, panel_h),
        );

        painter.rect_filled(panel_rect, 4.0, bg_color);
        painter.rect_stroke(panel_rect, 4.0, egui::Stroke::new(1.0, border_color));

        // Draw text
        let text_x = panel_rect.left() + pad;
        let text_y = panel_rect.top() + pad;
        painter.galley(egui::pos2(text_x, text_y), galley1, text_color);
        painter.galley(egui::pos2(text_x, text_y + line_h), galley2, text_color);
        painter.galley(egui::pos2(text_x, text_y + line_h * 2.0), galley3, text_color);
    }

    /// Edge-first hit-test: find the closest clip edge within TRIM_HANDLE_WIDTH.
    /// Returns (clip_rect, clip, TrimEdge) if an edge is close enough.
    /// This ensures that at shared borders between adjacent clips, each clip's
    /// edge is independently grabbable — standard NLE behavior.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_edge<'a>(
        &self,
        clips: &'a [klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pos: egui::Pos2,
    ) -> Option<(egui::Rect, &'a klipp_timeline::Clip, TrimEdge)> {
        // Check vertical bounds
        let y_top = track_rect.top();
        let y_bot = track_rect.top() + track_height;
        if pos.y < y_top || pos.y > y_bot {
            return None;
        }

        // Collect all edges with their distance to pointer
        let mut best: Option<(f32, egui::Rect, &klipp_timeline::Clip, TrimEdge)> = None;

        for clip in clips {
            let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);

            let dist_left = (pos.x - cr.left()).abs();
            let dist_right = (pos.x - cr.right()).abs();

            if dist_left < TRIM_HANDLE_WIDTH
                && (best.is_none() || dist_left < best.unwrap().0)
            {
                best = Some((dist_left, cr, clip, TrimEdge::Left));
            }
            if dist_right < TRIM_HANDLE_WIDTH
                && (best.is_none() || dist_right < best.unwrap().0)
            {
                best = Some((dist_right, cr, clip, TrimEdge::Right));
            }
        }

        best.map(|(_, cr, clip, edge)| (cr, clip, edge))
    }

    /// Hit-test: find which clip body (not edge) is under the pointer.
    /// Used for drag/move and selection. Does NOT match edge zones.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_clip_body<'a>(
        &self,
        clips: &'a [klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pos: egui::Pos2,
    ) -> Option<(egui::Rect, &'a klipp_timeline::Clip)> {
        for clip in clips.iter().rev() {
            let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
            if cr.contains(pos) {
                return Some((cr, clip));
            }
        }
        None
    }

    fn draw_zoom_buttons(
        &self,
        ui: &mut egui::Ui,
        header_rect: &egui::Rect,
        ruler_rect: &egui::Rect,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let btn_w = 22.0;
        let btn_h = 16.0;
        let y = ruler_rect.top() + (RULER_HEIGHT - btn_h) / 2.0;
        let mut x = header_rect.left() + 2.0;
        let painter = ui.painter().with_clip_rect(
            ui.clip_rect().intersect(ui.max_rect()),
        );

        // Fit button
        let fit_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let fit_resp = ui.interact(fit_rect, ui.id().with("zoom_fit"), egui::Sense::click());
        painter.rect_filled(fit_rect, 2.0, if fit_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(fit_rect.center(), egui::Align2::CENTER_CENTER, "Fit",
            egui::FontId::proportional(8.0), egui::Color32::from_rgb(220, 220, 230));
        if fit_resp.clicked() {
            cmds.push(AppCommand::ZoomTimelineToFit);
        }
        x += btn_w + 2.0;

        // - button
        let minus_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let minus_resp = ui.interact(minus_rect, ui.id().with("zoom_minus"), egui::Sense::click());
        painter.rect_filled(minus_rect, 2.0, if minus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        Icon::Minus.bold(12.0).tint(egui::Color32::from_rgb(220, 220, 230)).paint_at(ui, minus_rect.shrink(2.0));
        if minus_resp.clicked() {
            let new_zoom = (state.timeline_zoom / 1.5).clamp(0.1, 2000.0);
            cmds.push(AppCommand::SetTimelineZoom { zoom: new_zoom, scroll_x: None });
        }
        x += btn_w + 2.0;

        // + button
        let plus_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let plus_resp = ui.interact(plus_rect, ui.id().with("zoom_plus"), egui::Sense::click());
        painter.rect_filled(plus_rect, 2.0, if plus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        Icon::Plus.bold(12.0).tint(egui::Color32::from_rgb(220, 220, 230)).paint_at(ui, plus_rect.shrink(2.0));
        if plus_resp.clicked() {
            let new_zoom = (state.timeline_zoom * 1.5).clamp(0.1, 2000.0);
            cmds.push(AppCommand::SetTimelineZoom { zoom: new_zoom, scroll_x: None });
        }
    }

    fn draw_media_pool(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        pool_rect: &egui::Rect,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        painter.text(
            egui::pos2(pool_rect.left() + 4.0, pool_rect.top() + 4.0),
            egui::Align2::LEFT_TOP,
            "Media",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(100, 100, 110),
        );

        let item_size = 40.0;
        let padding = 4.0;
        let mut x = pool_rect.left() + 4.0;
        let y = pool_rect.top() + 16.0;

        let items: Vec<_> = state.media_pool.all().collect();
        for item in &items {
            if x + item_size > pool_rect.right() {
                break;
            }

            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(item_size, item_size - 8.0),
            );

            if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                painter.image(
                    tex.id(),
                    thumb_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else {
                painter.rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(60, 60, 70));
            }

            painter.rect_stroke(
                thumb_rect,
                2.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 90)),
            );

            let resp = ui.interact(thumb_rect, ui.id().with(("pool_item", item.id.0)), egui::Sense::click_and_drag());
            if resp.clicked() {
                // Single click: load in source monitor
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if resp.double_clicked() {
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id });
            }
            // Drag: start dragging media to a track
            if resp.drag_started() {
                state.dragging_media = Some(item.id);
            }
            if resp.hovered() && state.dragging_media.is_none() {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    ui.id().with(("pool_thumb", item.id)),
                    |ui: &mut egui::Ui| {
                        ui.label(format!("{} (drag to track)", &item.filename));
                    },
                );
            }

            x += item_size + padding;
        }
    }

    fn draw_toggle_buttons(
        &self,
        ui: &mut egui::Ui,
        header_rect: &egui::Rect,
        y_offset: f32,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let painter = ui.painter_at(*header_rect);
        let btn_w = 36.0;
        let btn_h = 14.0;
        let x = header_rect.left() + 2.0;

        // Snap toggle
        let snap_rect = egui::Rect::from_min_size(
            egui::pos2(x, y_offset + 2.0),
            egui::vec2(btn_w, btn_h),
        );
        let snap_resp = ui.interact(snap_rect, ui.id().with("snap_toggle"), egui::Sense::click());
        let snap_col = if state.snap_enabled {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(snap_rect, 2.0, snap_col);
        painter.text(snap_rect.center(), egui::Align2::CENTER_CENTER, "Snap",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if snap_resp.clicked() {
            cmds.push(AppCommand::SetSnapEnabled(!state.snap_enabled));
        }

        // Ripple toggle
        let ripple_rect = egui::Rect::from_min_size(
            egui::pos2(x + btn_w + 2.0, y_offset + 2.0),
            egui::vec2(btn_w, btn_h),
        );
        let ripple_resp = ui.interact(ripple_rect, ui.id().with("ripple_toggle"), egui::Sense::click());
        let ripple_col = if state.ripple_enabled {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(ripple_rect, 2.0, ripple_col);
        painter.text(ripple_rect.center(), egui::Align2::CENTER_CENTER, "Ripple",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if ripple_resp.clicked() {
            cmds.push(AppCommand::SetRippleEnabled(!state.ripple_enabled));
        }

        // Follow playhead toggle (cycles Off → Page → Smooth)
        let follow_w = 50.0;
        let follow_rect = egui::Rect::from_min_size(
            egui::pos2(x + (btn_w + 2.0) * 2.0, y_offset + 2.0),
            egui::vec2(follow_w, btn_h),
        );
        let follow_resp = ui.interact(follow_rect, ui.id().with("follow_toggle"), egui::Sense::click());
        let follow_col = if state.playhead_follow != PlayheadFollow::Off {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(follow_rect, 2.0, follow_col);
        let follow_label = format!("Follow: {}", state.playhead_follow.label());
        painter.text(follow_rect.center(), egui::Align2::CENTER_CENTER, follow_label,
            egui::FontId::proportional(7.0), egui::Color32::WHITE);
        if follow_resp.clicked() {
            state.playhead_follow = state.playhead_follow.next();
        }
        follow_resp.on_hover_text("Playhead follow mode: Off / Page / Smooth");

        // Proxy toggle
        let proxy_w = 40.0;
        let proxy_rect = egui::Rect::from_min_size(
            egui::pos2(x + (btn_w + 2.0) * 2.0 + follow_w + 4.0, y_offset + 2.0),
            egui::vec2(proxy_w, btn_h),
        );
        let proxy_resp = ui.interact(proxy_rect, ui.id().with("proxy_toggle"), egui::Sense::click());
        let proxy_col = if state.use_proxies {
            egui::Color32::from_rgb(180, 140, 30)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(proxy_rect, 2.0, proxy_col);
        painter.text(proxy_rect.center(), egui::Align2::CENTER_CENTER, "Proxy",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if proxy_resp.clicked() {
            cmds.push(AppCommand::SetUseProxies(!state.use_proxies));
        }
        proxy_resp.on_hover_text("Use proxy files for playback (lower quality, faster decode)");
    }

    fn draw_overview_bar(
        &self,
        painter: &egui::Painter,
        rect: &egui::Rect,
        state: &AppState,
    ) {
        // Background
        painter.rect_filled(*rect, 0.0, egui::Color32::from_rgb(16, 16, 20));
        // Bottom border
        painter.line_segment(
            [
                egui::pos2(rect.left(), rect.bottom()),
                egui::pos2(rect.right(), rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
        );

        // Compute total sequence duration
        let dur_secs = state.sequence().total_duration().to_secs_f64() as f32;
        if dur_secs <= 0.0 {
            return;
        }

        let bar_width = rect.width();
        let px_per_sec = bar_width / dur_secs;

        // Viewport rectangle bounds (needed for dimming)
        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let content_width = state.last_timeline_content_width;
        let view_start_secs = scroll / zoom;
        let view_end_secs = (scroll + content_width) / zoom;
        let vp_x0 = (rect.left() + view_start_secs * px_per_sec).max(rect.left());
        let vp_x1 = (rect.left() + view_end_secs * px_per_sec).min(rect.right());

        // Draw clips as tiny colored rectangles with lane gaps
        let track_count = state.sequence().tracks.len().max(1) as f32;
        let lane_gap = 1.0_f32;
        let total_gap = lane_gap * (track_count - 1.0).max(0.0);
        let lane_h = ((rect.height() - 4.0 - total_gap) / track_count).max(2.0);

        for (track_idx, track) in state.sequence().tracks.iter().enumerate() {
            let lane_y = rect.top() + 2.0 + track_idx as f32 * (lane_h + lane_gap);

            for clip in &track.clips {
                let x0 = rect.left() + clip.timeline_in.to_secs_f64() as f32 * px_per_sec;
                let x1 = rect.left() + clip.timeline_out.to_secs_f64() as f32 * px_per_sec;
                if x1 < rect.left() || x0 > rect.right() {
                    continue;
                }
                let cx0 = x0.max(rect.left());
                let cx1 = x1.min(rect.right());
                // Minimum 2px width so tiny clips are visible
                let cx1 = cx1.max(cx0 + 2.0);
                let clip_rect = egui::Rect::from_min_max(
                    egui::pos2(cx0, lane_y),
                    egui::pos2(cx1, (lane_y + lane_h).min(rect.bottom() - 2.0)),
                );
                let (r, g, b) = CLIP_COLORS[track_idx % CLIP_COLORS.len()];
                // Brighter fill inside viewport, dimmer outside
                let in_viewport = cx1 >= vp_x0 && cx0 <= vp_x1;
                let color = if in_viewport {
                    egui::Color32::from_rgb(r.saturating_add(20), g.saturating_add(20), b.saturating_add(20))
                } else {
                    egui::Color32::from_rgb(r / 2, g / 2, b / 2)
                };
                painter.rect_filled(clip_rect, 1.0, color);
                // Subtle outline
                painter.rect_stroke(
                    clip_rect,
                    1.0,
                    egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 25)),
                );
            }
        }

        // Dim areas outside viewport
        let dim = egui::Color32::from_rgba_premultiplied(0, 0, 0, 100);
        if vp_x0 > rect.left() {
            painter.rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(vp_x0, rect.bottom())),
                0.0,
                dim,
            );
        }
        if vp_x1 < rect.right() {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(vp_x1, rect.top()), rect.max),
                0.0,
                dim,
            );
        }

        // Viewport border
        let vp_rect = egui::Rect::from_min_max(
            egui::pos2(vp_x0, rect.top()),
            egui::pos2(vp_x1, rect.bottom()),
        );
        painter.rect_stroke(
            vp_rect,
            0.0,
            egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 120)),
        );

        // Draw playhead as thin red vertical line
        let playhead_secs = state.playhead.to_secs_f64() as f32;
        let ph_x = rect.left() + playhead_secs * px_per_sec;
        if ph_x >= rect.left() && ph_x <= rect.right() {
            painter.line_segment(
                [egui::pos2(ph_x, rect.top()), egui::pos2(ph_x, rect.bottom())],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(220, 50, 50)),
            );
        }

        // (interaction handled in show() to avoid z-order conflicts)
    }

    fn draw_ruler_ticks(
        &self,
        painter: &egui::Painter,
        ruler_rect: &egui::Rect,
        state: &AppState,
    ) {
        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let width = ruler_rect.width();
        let fps = state.fps.as_f64() as f32;
        let rate = state.fps.rate();

        // Determine tick interval based on zoom level
        // At low zoom: major ticks every 10s or 5s; at high zoom: every second or per-frame
        let secs_per_pixel = 1.0 / zoom;
        let min_label_spacing_px = 60.0; // minimum pixels between labels
        let min_label_interval_secs = secs_per_pixel * min_label_spacing_px;

        // Choose a "nice" major interval in seconds — covers frame-level to multi-hour
        let major_interval = if min_label_interval_secs > 3600.0 { 3600.0 }  // 1h
            else if min_label_interval_secs > 1800.0 { 1800.0 }  // 30m
            else if min_label_interval_secs > 600.0 { 600.0 }    // 10m
            else if min_label_interval_secs > 300.0 { 300.0 }    // 5m
            else if min_label_interval_secs > 120.0 { 120.0 }    // 2m
            else if min_label_interval_secs > 60.0 { 60.0 }      // 1m
            else if min_label_interval_secs > 30.0 { 30.0 }
            else if min_label_interval_secs > 10.0 { 10.0 }
            else if min_label_interval_secs > 5.0 { 5.0 }
            else if min_label_interval_secs > 2.0 { 2.0 }
            else if min_label_interval_secs > 1.0 { 1.0 }
            else if min_label_interval_secs > 0.5 { 0.5 }
            else { 1.0 / fps }; // per-frame at extreme zoom

        let start_secs = (scroll / zoom) - 1.0;
        let end_secs = (scroll + width) / zoom + 1.0;

        // Draw major ticks with labels
        let mut t = (start_secs / major_interval).floor() * major_interval;
        while t <= end_secs {
            if t >= 0.0 {
                let x = ruler_rect.left() + t * zoom - scroll;
                if x >= ruler_rect.left() - 10.0 && x <= ruler_rect.right() + 10.0 {
                    painter.line_segment(
                        [
                            egui::pos2(x, ruler_rect.bottom() - 12.0),
                            egui::pos2(x, ruler_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 90)),
                    );

                    // Format label based on zoom level
                    let label = if major_interval < 1.0 {
                        // Show HH:MM:SS:FF at frame-level zoom
                        let tc = RationalTime::from_secs_f64(t as f64, rate);
                        tc.to_timecode_string()
                    } else {
                        let total_secs = t.round() as i64;
                        let hh = total_secs / 3600;
                        let mm = (total_secs / 60) % 60;
                        let ss = total_secs % 60;
                        if major_interval >= 3600.0 {
                            format!("{hh}h{mm:02}m")
                        } else if major_interval >= 60.0 {
                            if hh > 0 {
                                format!("{hh}:{mm:02}:{ss:02}")
                            } else {
                                format!("{mm}:{ss:02}")
                            }
                        } else if major_interval <= 1.0 {
                            let total_ms = (t * 1000.0) as i64;
                            let ms = total_ms % 1000;
                            format!("{mm}:{ss:02}.{ms:03}")
                        } else if major_interval <= 5.0 {
                            let total_ms = (t * 1000.0) as i64;
                            let tenths = (total_ms % 1000) / 100;
                            if tenths > 0 {
                                format!("{mm}:{ss:02}.{tenths}")
                            } else {
                                format!("{mm}:{ss:02}")
                            }
                        } else if hh > 0 {
                            format!("{hh}:{mm:02}:{ss:02}")
                        } else {
                            format!("{mm}:{ss:02}")
                        }
                    };

                    painter.text(
                        egui::pos2(x + 3.0, ruler_rect.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        label,
                        egui::FontId::monospace(10.0),
                        egui::Color32::from_rgb(140, 140, 150),
                    );
                }
            }
            t += major_interval;
        }

        // Draw minor ticks (subdivisions between major ticks)
        let minor_interval = if major_interval >= 600.0 { major_interval / 5.0 }   // 10m+ → 5 subdivisions
            else if major_interval >= 60.0 { major_interval / 6.0 }   // 1m+ → 10s subdivisions
            else if major_interval >= 10.0 { major_interval / 5.0 }
            else if major_interval >= 2.0 { 1.0 }
            else if major_interval >= 1.0 { 0.5 }
            else { major_interval / 2.0 };

        if minor_interval < major_interval {
            let mut t = (start_secs / minor_interval).floor() * minor_interval;
            while t <= end_secs {
                if t >= 0.0 {
                    // Skip positions that coincide with major ticks
                    let is_major = (t / major_interval).round() * major_interval;
                    if (t - is_major).abs() > 0.001 {
                        let x = ruler_rect.left() + t * zoom - scroll;
                        if x >= ruler_rect.left() && x <= ruler_rect.right() {
                            painter.line_segment(
                                [
                                    egui::pos2(x, ruler_rect.bottom() - 6.0),
                                    egui::pos2(x, ruler_rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 70)),
                            );
                        }
                    }
                }
                t += minor_interval;
            }
        }

        // Draw per-frame ticks at high zoom (when frames are visible)
        let frame_width = zoom / fps;
        if frame_width >= 4.0 && major_interval >= (1.0 / fps) * 1.5 {
            let frame_secs = 1.0 / fps;
            let mut t = (start_secs / frame_secs).floor() * frame_secs;
            while t <= end_secs {
                if t >= 0.0 {
                    let x = ruler_rect.left() + t * zoom - scroll;
                    if x >= ruler_rect.left() && x <= ruler_rect.right() {
                        painter.line_segment(
                            [
                                egui::pos2(x, ruler_rect.bottom() - 3.0),
                                egui::pos2(x, ruler_rect.bottom()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
                        );
                    }
                }
                t += frame_secs;
            }
        }
    }
}

fn truncate_text(text: &str, max_width: f32, font_size: f32) -> String {
    let char_width = font_size * 0.6;
    let max_chars = (max_width / char_width) as usize;
    if text.len() <= max_chars {
        text.to_string()
    } else if max_chars > 3 {
        format!("{}...", &text[..max_chars - 3])
    } else {
        text[..max_chars.min(text.len())].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::Track;

    fn make_video_track() -> Track {
        Track::new("Video 1", TrackKind::Video)
    }

    fn make_audio_track() -> Track {
        Track::new("Audio 1", TrackKind::Audio)
    }

    fn make_text_track() -> Track {
        Track::new("Text 1", TrackKind::Text)
    }

    // ═══════════════════════════════════════════════════════════════
    // effective_track_height
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn default_heights_by_kind() {
        assert_eq!(effective_track_height(&make_video_track()), VIDEO_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&make_audio_track()), AUDIO_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&make_text_track()), TEXT_TRACK_HEIGHT);
    }

    #[test]
    fn custom_height_overrides_default() {
        let mut t = make_video_track();
        t.custom_height = Some(80.0);
        assert_eq!(effective_track_height(&t), 80.0);
    }

    #[test]
    fn none_custom_height_uses_default() {
        let mut t = make_audio_track();
        t.custom_height = None;
        assert_eq!(effective_track_height(&t), AUDIO_TRACK_HEIGHT);
    }

    // ═══════════════════════════════════════════════════════════════
    // Track height clamp
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn clamp_below_minimum() {
        let new_h = (10.0_f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
        assert_eq!(new_h, MIN_TRACK_HEIGHT);
    }

    #[test]
    fn clamp_above_maximum() {
        let new_h = (500.0_f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
        assert_eq!(new_h, MAX_TRACK_HEIGHT);
    }

    #[test]
    fn clamp_within_range() {
        let new_h = (100.0_f32).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
        assert_eq!(new_h, 100.0);
    }

    // ═══════════════════════════════════════════════════════════════
    // Collapse/expand logic
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn collapse_sets_minimum_height() {
        let mut t = make_video_track();
        // Simulate collapse: set to minimum
        t.custom_height = Some(MIN_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&t), MIN_TRACK_HEIGHT);
    }

    #[test]
    fn expand_from_collapsed_restores_pre_collapse() {
        let mut t = make_video_track();
        // Set a custom height, then collapse
        t.custom_height = Some(120.0);
        t.pre_collapse_height = t.custom_height.or(Some(VIDEO_TRACK_HEIGHT));
        t.custom_height = Some(MIN_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&t), MIN_TRACK_HEIGHT);

        // Expand: restore pre-collapse height
        t.custom_height = t.pre_collapse_height.take();
        assert_eq!(effective_track_height(&t), 120.0);
    }

    #[test]
    fn expand_from_collapsed_default_height() {
        let mut t = make_video_track();
        // Collapse from default (no custom_height)
        t.pre_collapse_height = Some(VIDEO_TRACK_HEIGHT);
        t.custom_height = Some(MIN_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&t), MIN_TRACK_HEIGHT);

        // Expand: restore to default via pre_collapse
        t.custom_height = t.pre_collapse_height.take();
        assert_eq!(effective_track_height(&t), VIDEO_TRACK_HEIGHT);
    }

    #[test]
    fn double_click_toggle_cycle() {
        let mut t = make_audio_track();
        let default_h = AUDIO_TRACK_HEIGHT;
        assert_eq!(effective_track_height(&t), default_h);

        // First double-click: collapse (store current height)
        t.pre_collapse_height = t.custom_height.or(Some(default_h));
        t.custom_height = Some(MIN_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&t), MIN_TRACK_HEIGHT);

        // Second double-click: expand (restore stored height)
        t.custom_height = t.pre_collapse_height.take();
        assert_eq!(effective_track_height(&t), default_h);
    }

    #[test]
    fn double_click_cycle_with_custom_height() {
        let mut t = make_video_track();
        t.custom_height = Some(150.0);

        // Collapse
        t.pre_collapse_height = t.custom_height.or(Some(VIDEO_TRACK_HEIGHT));
        t.custom_height = Some(MIN_TRACK_HEIGHT);
        assert_eq!(effective_track_height(&t), MIN_TRACK_HEIGHT);
        assert_eq!(t.pre_collapse_height, Some(150.0));

        // Expand — should restore 150, not the default 50
        t.custom_height = t.pre_collapse_height.take();
        assert_eq!(effective_track_height(&t), 150.0);
    }

    #[test]
    fn pre_collapse_height_none_by_default() {
        let t = make_video_track();
        assert!(t.pre_collapse_height.is_none());
    }

    // ═══════════════════════════════════════════════════════════════
    // Vertical scroll bounds
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn scroll_max_correct_for_track_heights() {
        // 3 tracks: 50 + 48 + 32 = 130px total
        let total_h = VIDEO_TRACK_HEIGHT + AUDIO_TRACK_HEIGHT + TEXT_TRACK_HEIGHT;
        assert_eq!(total_h, 130.0);

        // If available height is 100px, max scroll = 30px
        let available = 100.0_f32;
        let max_scroll = (total_h - available).max(0.0);
        assert_eq!(max_scroll, 30.0);
    }

    #[test]
    fn scroll_max_zero_when_tracks_fit() {
        let total_h = 50.0_f32; // one video track
        let available = 200.0_f32;
        let max_scroll = (total_h - available).max(0.0);
        assert_eq!(max_scroll, 0.0); // no scroll needed
    }

    #[test]
    fn scroll_clamps_to_range() {
        let max_scroll = 30.0_f32;
        assert_eq!((-10.0_f32).clamp(0.0, max_scroll), 0.0);
        assert_eq!((50.0_f32).clamp(0.0, max_scroll), 30.0);
        assert_eq!((15.0_f32).clamp(0.0, max_scroll), 15.0);
    }

    // ═══════════════════════════════════════════════════════════════
    // Track visibility culling
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn track_visible_when_in_view() {
        let tracks_start_y = 100.0_f32;
        let tracks_clip_bottom = 300.0_f32;
        let track_y = 150.0_f32;
        let track_height = 50.0_f32;
        let visible = track_y < tracks_clip_bottom && track_y + track_height > tracks_start_y;
        assert!(visible);
    }

    #[test]
    fn track_hidden_when_below_clip() {
        let tracks_start_y = 100.0_f32;
        let tracks_clip_bottom = 300.0_f32;
        let track_y = 350.0_f32; // fully below visible area
        let track_height = 50.0_f32;
        let visible = track_y < tracks_clip_bottom && track_y + track_height > tracks_start_y;
        assert!(!visible);
    }

    #[test]
    fn track_hidden_when_above_clip() {
        let tracks_start_y = 100.0_f32;
        let tracks_clip_bottom = 300.0_f32;
        let track_y = 20.0_f32; // fully above visible area
        let track_height = 50.0_f32; // ends at 70, before tracks_start_y
        let visible = track_y < tracks_clip_bottom && track_y + track_height > tracks_start_y;
        assert!(!visible);
    }

    #[test]
    fn track_partially_visible_counts_as_visible() {
        let tracks_start_y = 100.0_f32;
        let tracks_clip_bottom = 300.0_f32;
        let track_y = 280.0_f32; // starts at 280, ends at 330 — partially visible
        let track_height = 50.0_f32;
        let visible = track_y < tracks_clip_bottom && track_y + track_height > tracks_start_y;
        assert!(visible);
    }

    // ═══════════════════════════════════════════════════════════════
    // Preset heights
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn preset_heights_within_bounds() {
        for h in [32.0_f32, 64.0, 120.0, 200.0, 300.0] {
            assert!(h >= MIN_TRACK_HEIGHT, "{h} below min");
            assert!(h <= MAX_TRACK_HEIGHT, "{h} above max");
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Ctrl+drag resize all
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn resize_all_sets_same_height() {
        let mut tracks = vec![make_video_track(), make_audio_track(), make_text_track()];
        let new_h = 80.0_f32;
        for t in &mut tracks {
            t.custom_height = Some(new_h);
        }
        for t in &tracks {
            assert_eq!(effective_track_height(t), 80.0);
        }
    }
}
