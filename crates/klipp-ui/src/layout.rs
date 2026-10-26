//! Pure layout computation for the main window. Extracted from `app.rs` so
//! rect math can be unit tested without spinning up a full `KlippApp`.

use crate::layout_state::LayoutState;
use crate::state::AppState;
use crate::theme;

/// All panel rects computed once per frame from layout state and visibility flags.
/// Pure computation — no rendering, no input, no mutation.
#[derive(Debug, Clone)]
pub(crate) struct LayoutRects {
    /// The input rect this layout was computed from. Retained for tests and
    /// future callers that may want to re-derive things from it.
    #[allow(dead_code)]
    pub full: egui::Rect,
    pub toolbar_rect: egui::Rect,
    pub statusbar_rect: egui::Rect,
    pub browser_rect: egui::Rect,
    pub left_resizer_rect: egui::Rect,
    pub inspector_rect: egui::Rect,
    pub right_resizer_rect: egui::Rect,
    /// Left half of the top viewer row — the Source Viewer.
    pub source_viewer_rect: egui::Rect,
    /// Right half of the top viewer row — the Program Viewer.
    /// Historically named `viewer_rect` back when Klipp had a single viewer;
    /// kept under that name to minimise downstream churn.
    pub viewer_rect: egui::Rect,
    pub h_resizer_rect: egui::Rect,
    /// The Audio Mixer's top edge, which sets its height. The same rect
    /// as `h_resizer_rect` when nothing sits between it and the viewers;
    /// nothing when the mixer is hidden.
    pub audio_mixer_resizer_rect: egui::Rect,
    pub curve_editor_rect: egui::Rect,
    pub audio_mixer_rect: egui::Rect,
    pub timeline_rect: egui::Rect,
    pub toggle_bar_rect: egui::Rect,
    pub scopes_rect: egui::Rect,
    pub nodes_rect: egui::Rect,
    pub nodes_resizer_rect: egui::Rect,
    pub nodes_on_right: bool,
}

/// Compute all panel rects from the full available rect, layout state, and
/// visibility flags. Called once per frame before any rendering.
pub(crate) fn compute_layout(
    full: egui::Rect,
    layout: &LayoutState,
    state: &AppState,
) -> LayoutRects {
    // ── Toolbar (top 44px) ──
    let toolbar_rect = egui::Rect::from_min_size(
        full.min,
        egui::vec2(full.width(), theme::TOOLBAR_HEIGHT),
    );

    // ── Status bar (bottom 26px) ──
    let statusbar_rect = egui::Rect::from_min_size(
        egui::pos2(full.min.x, full.max.y - theme::STATUSBAR_HEIGHT),
        egui::vec2(full.width(), theme::STATUSBAR_HEIGHT),
    );

    // ── Main row (between toolbar and status bar) ──
    let main_top = toolbar_rect.max.y;
    let main_bottom = statusbar_rect.min.y;
    let main_height = main_bottom - main_top;

    // Browser (left) — collapsible to a rail, never to nothing. The
    // rail carries the tab icons, so the panel can be brought back by
    // the same click that sent it away; collapsing to zero would leave
    // only a menu item nobody knows about.
    let browser_w = if state.show_browser_panel {
        layout.browser_width
    } else {
        theme::COLLAPSED_RAIL_W
    };
    let browser_rect = egui::Rect::from_min_size(
        egui::pos2(full.min.x, main_top),
        egui::vec2(browser_w, main_height),
    );

    // Left resizer
    let left_resizer_rect = egui::Rect::from_min_size(
        egui::pos2(full.min.x + browser_w, main_top),
        egui::vec2(theme::RESIZER_THICKNESS, main_height),
    );

    // Inspector (right) — collapsible, clamp to available space
    let inspector_w = if state.show_inspector_panel {
        let max_inspector_w = (full.width() - browser_w - theme::RESIZER_THICKNESS * 2.0 - 100.0).max(theme::INSPECTOR_MIN_W);
        layout.inspector_width.min(max_inspector_w)
    } else {
        theme::COLLAPSED_RAIL_W
    };
    let inspector_rect = egui::Rect::from_min_size(
        egui::pos2(full.max.x - inspector_w, main_top),
        egui::vec2(inspector_w, main_height),
    );

    // Right resizer
    let right_resizer_rect = egui::Rect::from_min_size(
        egui::pos2(
            full.max.x - inspector_w - theme::RESIZER_THICKNESS,
            main_top,
        ),
        egui::vec2(theme::RESIZER_THICKNESS, main_height),
    );

    // Center column (between left resizer and right resizer)
    let center_left = left_resizer_rect.max.x;
    let center_right = right_resizer_rect.min.x;
    let center_w = (center_right - center_left).max(100.0);

    // ── Color workspace: viewer + toggle bar + scopes + nodes ──
    // Scopes are now allowed in any workspace (FCP/Premiere convention) so
    // the V key / Toggle Scopes icon do something meaningful from Edit and
    // Export too. The Color-only [Scopes/Nodes] toggle bar and the node
    // strip stay gated to Color since they're Color-specific tooling.
    let in_color_ws = state.show_color_panel;
    let show_scopes_bar = state.scopes_docked();
    let nodes_on_right = state.graph_is_visible()
        && layout.node_graph_placement == crate::layout_state::NodeGraphPlacement::Right
        && center_w >= 640.0;
    let show_nodes_bar = state.graph_is_visible() && !nodes_on_right;

    // In Color workspace: allocate bottom space for toggle + scopes + nodes
    // Use ratio to split between viewer+timeline and bottom panels
    let toggle_bar_h: f32 = if in_color_ws { crate::widgets::inspector_group::SECTION_HEADER_H } else { 0.0 };
    let center_total = main_bottom - main_top;
    let has_bottom_panels = show_scopes_bar || show_nodes_bar;

    // Bottom panels (scopes + nodes) share a flexible height controlled by the ratio
    let fixed_h = toggle_bar_h + theme::RESIZER_THICKNESS;
    let flexible_h = (center_total - fixed_h).max(200.0);
    let bottom_panel_h: f32 = if has_bottom_panels {
        let viewer_ratio = layout.color_viewer_height_ratio.clamp(0.15, 0.90);
        (flexible_h * (1.0 - viewer_ratio)).clamp(60.0, flexible_h * 0.85)
    } else {
        0.0
    };

    // Split bottom panel between scopes and nodes
    let (scopes_h, nodes_h) = match (show_scopes_bar, show_nodes_bar) {
        (true, true) => (bottom_panel_h * 0.6, bottom_panel_h * 0.4),
        (true, false) => (bottom_panel_h, 0.0),
        (false, true) => (0.0, bottom_panel_h),
        (false, false) => (0.0, 0.0),
    };

    let color_bottom_total = toggle_bar_h + scopes_h + nodes_h;
    let color_bottom_top = main_bottom - color_bottom_total;

    let toggle_bar_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, color_bottom_top),
        egui::vec2(center_w, toggle_bar_h),
    );
    let scopes_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, toggle_bar_rect.max.y),
        egui::vec2(center_w, scopes_h),
    );
    let mut nodes_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, scopes_rect.max.y),
        egui::vec2(center_w, nodes_h),
    );

    // Timeline: hidden in Color workspace, normal otherwise
    // Timeline sits above the scopes/nodes area (or fills to bottom if none)
    let timeline_bottom = color_bottom_top;
    let max_timeline_h = (timeline_bottom - main_top - theme::RESIZER_THICKNESS) * theme::TIMELINE_MAX_H_PCT;
    let timeline_min_h = theme::TIMELINE_MIN_H;
    // In Color the timeline is a strip: no toolbar, no tabs, its own
    // remembered height.
    // The Edit timeline starts as a share of the editing area rather
    // than a fixed height, so it is the same portion of the window on
    // every display. A stored height means the divider has been dragged
    // and that wins. Colour keeps its own short strip: grading is not
    // cutting, and its timeline is deliberately a ribbon.
    let wanted_timeline_h = if in_color_ws {
        layout.color_timeline_height
    } else if layout.timeline_height <= theme::TIMELINE_H_UNSET {
        (main_bottom - main_top) * theme::TIMELINE_DEFAULT_FRAC
    } else {
        layout.timeline_height
    };
    let mut timeline_h = wanted_timeline_h.clamp(timeline_min_h, max_timeline_h.max(timeline_min_h));
    if in_color_ws {
        // The strip is for moving between shots: never taller than its
        // tracks need, nor more than a share of the column. A height
        // dragged out, or carried over in layout.json, left a band of
        // empty tracks where the picture should have been.
        let tracks_h: f32 = state.sequence().tracks.iter().map(crate::panels::timeline::track_row_height).sum();
        let content_h = theme::RULER_HEIGHT + crate::timeline::camera_lane::height(state) + tracks_h + theme::COLOR_STRIP_SLACK;
        let share = (timeline_bottom - main_top) * theme::COLOR_STRIP_MAX_FRAC;
        timeline_h = timeline_h.min(content_h.max(timeline_min_h)).min(share.max(timeline_min_h));
    }

    let timeline_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, timeline_bottom - timeline_h),
        egui::vec2(center_w, timeline_h),
    );

    // Audio mixer (between viewer and timeline when visible), as tall as
    // dragged but never past half the room above the timeline.
    let audio_mixer_h: f32 = if state.show_audio_mixer {
        let room = (timeline_rect.min.y - main_top) * 0.5;
        layout.audio_mixer_height.clamp(crate::panels::audio_mixer::MIXER_MIN_H, theme::AUDIO_MIXER_MAX_H).min(room.max(0.0))
    } else { 0.0 };
    let audio_mixer_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, timeline_rect.min.y - audio_mixer_h - if audio_mixer_h > 0.0 { theme::RESIZER_THICKNESS } else { 0.0 }),
        egui::vec2(center_w, audio_mixer_h),
    );

    // Curve editor (between viewer and audio mixer / timeline when visible)
    let curve_editor_h: f32 = if state.show_curve_editor { 180.0 } else { 0.0 };
    let curve_editor_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, audio_mixer_rect.min.y - curve_editor_h - if curve_editor_h > 0.0 { theme::RESIZER_THICKNESS } else { 0.0 }),
        egui::vec2(center_w, curve_editor_h),
    );

    // Horizontal resizer (between viewer and curve editor / audio mixer / timeline)
    let h_resizer_rect = egui::Rect::from_min_size(
        egui::pos2(
            center_left,
            curve_editor_rect.min.y - theme::RESIZER_THICKNESS,
        ),
        egui::vec2(center_w, theme::RESIZER_THICKNESS),
    );

    // Viewer row (top of center, above horizontal resizer). Split 50/50
    // into Source (left) and Program (right) with a thin gutter between —
    // standard Premiere/Avid/Resolve-Edit convention. Equal sizes make
    // three-point-edit frame comparison visually balanced.
    //
    // The panel rect itself stays the FULL half-slot regardless of project
    // aspect — chrome row (transport controls) is always the full panel
    // width so the buttons live in predictable positions, the video area
    // above it is always `panel.h - chrome_h`, and project-aspect fitting
    // is done by `compute_video_layout` / `show_source_viewer` inside the
    // video area (black letterbox / pillarbox bars as needed). This is
    // how Resolve, Premiere, FCPX, and Avid all do it.
    let viewer_row_top = main_top;
    let viewer_row_bottom = h_resizer_rect.min.y;
    // Outside Color, nodes placed below sit right under the viewers, the
    // timeline under them: at the window's foot they were a viewer's
    // length away from the picture they change. The space is the same,
    // so the timeline only moves down by it. A zero-height toggle bar on
    // the nodes' top edge carries the divider above them.
    let nodes_under_viewer = show_nodes_bar && !in_color_ws && !show_scopes_bar;
    let mut toggle_bar_rect = toggle_bar_rect;
    let (mut h_resizer_rect, mut curve_editor_rect, mut audio_mixer_rect, mut timeline_rect) =
        (h_resizer_rect, curve_editor_rect, audio_mixer_rect, timeline_rect);
    if nodes_under_viewer {
        let down = egui::vec2(0.0, nodes_h);
        for r in [&mut h_resizer_rect, &mut curve_editor_rect, &mut audio_mixer_rect, &mut timeline_rect] {
            *r = r.translate(down);
        }
        nodes_rect = egui::Rect::from_min_max(
            egui::pos2(center_left, viewer_row_bottom + theme::RESIZER_THICKNESS),
            egui::pos2(center_left + center_w, h_resizer_rect.min.y),
        );
        toggle_bar_rect = egui::Rect::from_min_size(nodes_rect.min, egui::vec2(center_w, 0.0));
    }
    //
    // A single-viewer layout hands the whole row to whichever viewer
    // `viewer_mode` names and leaves the other with no width.
    let mut nodes_resizer_rect = egui::Rect::NOTHING;
    let viewer_w = if nodes_on_right {
        let usable = center_w - theme::RESIZER_THICKNESS;
        let node_w = (usable * layout.node_width_ratio.clamp(0.0, 1.0))
            .clamp(240.0, usable - 320.0);
        let width = usable - node_w;
        nodes_resizer_rect = egui::Rect::from_min_max(
            egui::pos2(center_left + width, viewer_row_top),
            egui::pos2(center_left + width + theme::RESIZER_THICKNESS, viewer_row_bottom),
        );
        nodes_rect = egui::Rect::from_min_max(
            nodes_resizer_rect.right_top(), egui::pos2(center_left + center_w, viewer_row_bottom),
        );
        width
    } else { center_w };
    let viewer_gutter = theme::RESIZER_THICKNESS;
    let full_row = egui::Rect::from_min_max(
        egui::pos2(center_left, viewer_row_top),
        egui::pos2(center_left + viewer_w, viewer_row_bottom),
    );
    let (source_viewer_rect, viewer_rect) = if state.viewers_side_by_side() {
        let half_w = ((viewer_w - viewer_gutter) * 0.5).max(0.0);
        (
            egui::Rect::from_min_max(
                egui::pos2(center_left, viewer_row_top),
                egui::pos2(center_left + half_w, viewer_row_bottom),
            ),
            egui::Rect::from_min_max(
                egui::pos2(center_left + half_w + viewer_gutter, viewer_row_top),
                egui::pos2(center_left + viewer_w, viewer_row_bottom),
            ),
        )
    } else if !state.single_viewer_is_program() && state.viewer_mode == klipp_core::ViewerMode::Source {
        (full_row, egui::Rect::from_min_size(full_row.right_top(), egui::vec2(0.0, full_row.height())))
    } else {
        (egui::Rect::from_min_size(full_row.left_top(), egui::vec2(0.0, full_row.height())), full_row)
    };

    LayoutRects {
        full,
        toolbar_rect,
        statusbar_rect,
        browser_rect,
        left_resizer_rect,
        inspector_rect,
        right_resizer_rect,
        source_viewer_rect,
        viewer_rect,
        h_resizer_rect,
        audio_mixer_resizer_rect: if audio_mixer_h > 0.0 {
            egui::Rect::from_min_size(audio_mixer_rect.min - egui::vec2(0.0, theme::RESIZER_THICKNESS), egui::vec2(center_w, theme::RESIZER_THICKNESS))
        } else {
            egui::Rect::NOTHING
        },
        curve_editor_rect,
        audio_mixer_rect,
        timeline_rect,
        toggle_bar_rect,
        scopes_rect,
        nodes_rect,
        nodes_resizer_rect,
        nodes_on_right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_full_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1920.0, 1080.0))
    }

    #[test]
    fn test_toolbar_at_top() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(rects.toolbar_rect.min.y, 0.0);
        assert_eq!(rects.toolbar_rect.height(), theme::TOOLBAR_HEIGHT);
    }

    #[test]
    fn test_statusbar_at_bottom() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(rects.statusbar_rect.max.y, 1080.0);
        assert_eq!(rects.statusbar_rect.height(), theme::STATUSBAR_HEIGHT);
    }

    #[test]
    fn test_viewer_below_toolbar() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert!(rects.viewer_rect.min.y >= rects.toolbar_rect.max.y);
    }

    #[test]
    fn scopes_outside_color_leave_the_timeline_alone() {
        // The dock flag survives a layout restore; in Edit it must not
        // take rows from the timeline.
        let layout = LayoutState::default();
        let mut state = AppState { show_color_panel: false, show_scopes: false, ..Default::default() };
        let plain = compute_layout(test_full_rect(), &layout, &state);
        state.show_scopes = true;
        let docked = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(docked.scopes_rect.height(), 0.0);
        assert_eq!(docked.timeline_rect, plain.timeline_rect);
        state.show_color_panel = true;
        let color = compute_layout(test_full_rect(), &layout, &state);
        assert!(color.scopes_rect.height() > 0.0);
    }

    /// A Colour strip dragged or carried over tall (the walkthrough's
    /// layout.json had 471 px) shows no more than its tracks, so the
    /// picture keeps the space; 4K at 1.25 is 3072x1728 logical.
    #[test]
    fn the_colour_strip_is_no_taller_than_its_tracks() {
        let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(3072.0, 1728.0));
        let layout = LayoutState { color_timeline_height: 471.0, ..Default::default() };
        let state = AppState { show_color_panel: true, show_scopes: true, ..Default::default() };
        let r = compute_layout(full, &layout, &state);
        let tracks: f32 = state.sequence().tracks.iter().map(crate::panels::timeline::track_row_height).sum();
        let want = theme::RULER_HEIGHT + crate::timeline::camera_lane::height(&state) + tracks + theme::COLOR_STRIP_SLACK;
        assert!(r.timeline_rect.height() <= want.max(theme::TIMELINE_MIN_H) + 0.5, "strip {} for tracks needing {want}", r.timeline_rect.height());
        assert!(r.viewer_rect.height() > 700.0, "the picture keeps the room: {}", r.viewer_rect.height());
    }

    #[test]
    fn a_single_viewer_takes_the_whole_row() {
        let layout = LayoutState::default();
        let mut state = AppState { dual_viewers: false, ..Default::default() };
        state.viewer_mode = klipp_core::ViewerMode::Program;
        let r = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(r.source_viewer_rect.width(), 0.0);
        assert!(r.viewer_rect.width() > 1000.0);
        state.viewer_mode = klipp_core::ViewerMode::Source;
        let r = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(r.viewer_rect.width(), 0.0);
        assert!(r.source_viewer_rect.width() > 1000.0);
        state.dual_viewers = true;
        let r = compute_layout(test_full_rect(), &layout, &state);
        assert!((r.source_viewer_rect.width() - r.viewer_rect.width()).abs() < 1.0);
    }

    #[test]
    fn test_timeline_above_statusbar() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert!(rects.timeline_rect.max.y <= rects.statusbar_rect.min.y);
    }

    #[test]
    fn an_untouched_edit_timeline_is_the_same_share_of_any_window() {
        // The point of a proportion: the editor's main surface should
        // not shrink as the display grows. A fixed 210 px was a third
        // of a small window and a seventh of a large one.
        let layout = LayoutState::default();
        let state = AppState::default();
        let mut shares = Vec::new();
        for (w, h) in [(1280.0, 800.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
            let full = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
            let r = compute_layout(full, &layout, &state);
            let editing_area = r.statusbar_rect.min.y - r.toolbar_rect.max.y;
            shares.push(r.timeline_rect.height() / editing_area);
        }
        for s in &shares {
            assert!(
                (*s - theme::TIMELINE_DEFAULT_FRAC).abs() < 0.02,
                "timeline took {:.0}% of the editing area, expected {:.0}%",
                s * 100.0,
                theme::TIMELINE_DEFAULT_FRAC * 100.0,
            );
        }
    }

    #[test]
    fn a_dragged_timeline_height_is_kept() {
        // The proportion is only a starting point; once someone sets a
        // height it is theirs.
        let layout = LayoutState { timeline_height: 260.0, ..LayoutState::default() };
        let state = AppState::default();
        let r = compute_layout(test_full_rect(), &layout, &state);
        assert!((r.timeline_rect.height() - 260.0).abs() < 1.0);
    }

    /// #2693: the Audio Mixer is as tall as its top edge was dragged,
    /// never shorter than its strips need, and that edge is the divider
    /// under the viewers when nothing sits between them.
    #[test]
    fn the_audio_mixer_takes_its_dragged_height() {
        let state = AppState { show_audio_mixer: true, ..AppState::default() };
        let full = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        for (stored, shown) in [(280.0, 280.0), (10.0, crate::panels::audio_mixer::MIXER_MIN_H)] {
            let layout = LayoutState { audio_mixer_height: stored, ..LayoutState::default() };
            let r = compute_layout(full, &layout, &state);
            assert!((r.audio_mixer_rect.height() - shown).abs() < 0.5, "stored {stored}: {}", r.audio_mixer_rect.height());
            assert_eq!(r.audio_mixer_resizer_rect, r.h_resizer_rect);
            assert!(r.audio_mixer_rect.max.y < r.timeline_rect.min.y);
        }
        let hidden = compute_layout(full, &LayoutState::default(), &AppState::default());
        assert!(!hidden.audio_mixer_resizer_rect.is_positive());
    }

    #[test]
    fn a_collapsed_browser_keeps_its_rail() {
        // Not zero: the rail is the way back. A panel that collapses to
        // nothing can only be restored from a menu, which is the part
        // people do not find.
        let layout = LayoutState::default();
        let state = AppState {
            show_browser_panel: false,
            ..Default::default()
        };
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(rects.browser_rect.width(), theme::COLLAPSED_RAIL_W);
    }

    #[test]
    fn a_collapsed_inspector_keeps_its_rail() {
        let layout = LayoutState::default();
        let state = AppState {
            show_inspector_panel: false,
            ..Default::default()
        };
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(rects.inspector_rect.width(), theme::COLLAPSED_RAIL_W);
    }

    #[test]
    fn collapsing_both_panels_gives_the_centre_the_width_back() {
        let full = test_full_rect();
        let layout = LayoutState::default();
        let open = compute_layout(full, &layout, &AppState::default());
        let shut = compute_layout(
            full,
            &layout,
            &AppState { show_browser_panel: false, show_inspector_panel: false, ..Default::default() },
        );
        let open_centre = open.viewer_rect.width() + open.source_viewer_rect.width();
        let shut_centre = shut.viewer_rect.width() + shut.source_viewer_rect.width();
        assert!(
            shut_centre > open_centre,
            "collapsing both panels should widen the centre: {open_centre} -> {shut_centre}"
        );
    }

    #[test]
    fn test_source_and_program_viewers_are_equal_width() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        let diff = (rects.source_viewer_rect.width() - rects.viewer_rect.width()).abs();
        assert!(diff < 0.01, "source and program viewers should have equal widths (diff = {diff})");
    }

    #[test]
    fn test_source_viewer_left_of_program_with_gutter() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert!(rects.source_viewer_rect.max.x <= rects.viewer_rect.min.x,
            "source viewer must sit to the left of the program viewer");
        assert!(rects.source_viewer_rect.max.x < rects.viewer_rect.min.x,
            "source and program viewers must have a gutter between them");
    }

    #[test]
    fn test_viewer_panels_are_project_aspect_independent() {
        // Pro-NLE convention: viewer panel shape doesn't depend on
        // project aspect — chrome row stays the full panel width and
        // the video content is letterboxed inside the video area.
        // Same input window across very different project aspects
        // should give identical panel rects.
        let layout = LayoutState::default();
        let mut portrait = AppState::default();
        portrait.sequence_mut().resolution = klipp_core::Resolution { width: 2160, height: 3840 };
        let mut landscape = AppState::default();
        landscape.sequence_mut().resolution = klipp_core::Resolution { width: 3840, height: 2160 };
        let mut square = AppState::default();
        square.sequence_mut().resolution = klipp_core::Resolution { width: 1080, height: 1080 };

        let rp = compute_layout(test_full_rect(), &layout, &portrait);
        let rl = compute_layout(test_full_rect(), &layout, &landscape);
        let rs = compute_layout(test_full_rect(), &layout, &square);

        for other in [&rl, &rs] {
            let dw = (rp.viewer_rect.width() - other.viewer_rect.width()).abs();
            let dh = (rp.viewer_rect.height() - other.viewer_rect.height()).abs();
            assert!(dw < 0.01 && dh < 0.01,
                "viewer panel size must not depend on project aspect");
        }
    }

    #[test]
    fn test_no_panel_overlap() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        // Toolbar and viewer should not overlap
        assert!(rects.toolbar_rect.max.y <= rects.viewer_rect.min.y);
        // Timeline and statusbar should not overlap
        assert!(rects.timeline_rect.max.y <= rects.statusbar_rect.min.y);
        // Browser and inspector should not overlap
        assert!(rects.browser_rect.max.x <= rects.inspector_rect.min.x);
    }
}

#[cfg(test)]
mod side_node_tests {
    use super::*;
    use crate::layout_state::NodeGraphPlacement;

    #[test]
    fn side_nodes_keep_viewers_scopes_and_inspector_separate() {
        for width in [1024.0, 1280.0, 1920.0, 3072.0] {
            for browser in [false, true] {
                for dual in [false, true] {
                    for scopes in [false, true] {
                        for ratio in [0.0, 0.42, 1.0] {
                            let full = egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(width, 900.0));
                            let layout = LayoutState { node_graph_placement: NodeGraphPlacement::Right, node_width_ratio: ratio, ..Default::default() };
                            let mut state = AppState { show_color_panel: true, show_node_strip: true,
                                show_browser_panel: browser, show_scopes: scopes, color_dual_viewers: dual,
                                active_workspace: crate::state::Workspace::Color, ..Default::default() };
                            let r = compute_layout(full, &layout, &state);
                            let center_w = r.right_resizer_rect.left() - r.left_resizer_rect.right();
                            assert_eq!(r.nodes_on_right, center_w >= 640.0);
                            assert!(full.contains_rect(r.nodes_rect));
                            assert!(r.nodes_rect.width() > 0.0 && r.nodes_rect.height() > 0.0);
                            assert!(r.nodes_rect.right() <= r.inspector_rect.left());
                            if r.nodes_on_right {
                                assert!(r.viewer_rect.right() <= r.nodes_resizer_rect.left());
                                assert_eq!(r.nodes_resizer_rect.right(), r.nodes_rect.left());
                                assert_eq!(r.nodes_rect.top(), r.viewer_rect.top());
                                assert_eq!(r.nodes_rect.bottom(), r.viewer_rect.bottom());
                                assert!(r.nodes_rect.width() >= 239.9);
                                let viewer_span = r.nodes_resizer_rect.left() - r.left_resizer_rect.right();
                                assert!(viewer_span >= 319.9);
                                assert!((r.timeline_rect.width() - center_w).abs() < 0.01);
                                if dual { assert!((r.source_viewer_rect.width() - r.viewer_rect.width()).abs() < 0.01); }
                                state.show_node_strip = false;
                                let hidden = compute_layout(full, &layout, &state);
                                assert_eq!(r.timeline_rect, hidden.timeline_rect);
                                assert_eq!(r.scopes_rect, hidden.scopes_rect);
                            } else {
                                assert!(!r.nodes_resizer_rect.is_positive());
                                assert!(r.nodes_rect.top() >= r.timeline_rect.bottom());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn below_remains_the_default_and_resize_fallback_preserves_preference() {
        let mut layout = LayoutState::default();
        let state = AppState { show_color_panel: true, show_node_strip: true, show_browser_panel: false, ..Default::default() };
        let rect = |w| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 1080.0));
        assert!(!compute_layout(rect(1920.0), &layout, &state).nodes_on_right);
        layout.node_graph_placement = NodeGraphPlacement::Right;
        assert!(compute_layout(rect(1920.0), &layout, &state).nodes_on_right);
        assert!(!compute_layout(rect(900.0), &layout, &state).nodes_on_right);
        assert_eq!(layout.node_graph_placement, NodeGraphPlacement::Right);
        assert!(compute_layout(rect(1920.0), &layout, &state).nodes_on_right);
    }
}
