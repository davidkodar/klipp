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
    pub viewer_rect: egui::Rect,
    pub h_resizer_rect: egui::Rect,
    pub curve_editor_rect: egui::Rect,
    pub audio_mixer_rect: egui::Rect,
    pub timeline_rect: egui::Rect,
    pub toggle_bar_rect: egui::Rect,
    pub scopes_rect: egui::Rect,
    pub nodes_rect: egui::Rect,
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

    // Browser (left) — collapsible
    let browser_w = if state.show_browser_panel { layout.browser_width } else { 0.0 };
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
        0.0
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
    let in_color_ws = state.show_color_panel;
    let show_scopes_bar = in_color_ws && state.show_scopes;
    let show_nodes_bar = in_color_ws && state.show_node_strip;

    // In Color workspace: allocate bottom space for toggle + scopes + nodes
    // Use ratio to split between viewer+timeline and bottom panels
    let toggle_bar_h: f32 = if in_color_ws { 22.0 } else { 0.0 };
    let center_total = main_bottom - main_top;
    let has_bottom_panels = show_scopes_bar || show_nodes_bar;

    // Bottom panels (scopes + nodes) share a flexible height controlled by the ratio
    let fixed_h = toggle_bar_h + theme::RESIZER_THICKNESS;
    let flexible_h = (center_total - fixed_h).max(200.0);
    let bottom_panel_h: f32 = if has_bottom_panels {
        let viewer_ratio = layout.color_viewer_height_ratio.clamp(0.3, 0.75);
        (flexible_h * (1.0 - viewer_ratio)).clamp(80.0, flexible_h * 0.6)
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
    let nodes_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, scopes_rect.max.y),
        egui::vec2(center_w, nodes_h),
    );

    // Timeline: hidden in Color workspace, normal otherwise
    // Timeline sits above the scopes/nodes area (or fills to bottom if none)
    let timeline_bottom = color_bottom_top;
    let max_timeline_h = (timeline_bottom - main_top - theme::RESIZER_THICKNESS) * theme::TIMELINE_MAX_H_PCT;
    let timeline_min_h = theme::TIMELINE_MIN_H;
    let timeline_h = layout
        .timeline_height
        .clamp(timeline_min_h, max_timeline_h);

    let timeline_rect = egui::Rect::from_min_size(
        egui::pos2(center_left, timeline_bottom - timeline_h),
        egui::vec2(center_w, timeline_h),
    );

    // Audio mixer (between viewer and timeline when visible)
    let audio_mixer_h: f32 = if state.show_audio_mixer { 220.0 } else { 0.0 };
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

    // Viewer (top of center, above horizontal resizer)
    let viewer_rect = egui::Rect::from_min_max(
        egui::pos2(center_left, main_top),
        egui::pos2(center_left + center_w, h_resizer_rect.min.y),
    );

    LayoutRects {
        full,
        toolbar_rect,
        statusbar_rect,
        browser_rect,
        left_resizer_rect,
        inspector_rect,
        right_resizer_rect,
        viewer_rect,
        h_resizer_rect,
        curve_editor_rect,
        audio_mixer_rect,
        timeline_rect,
        toggle_bar_rect,
        scopes_rect,
        nodes_rect,
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
    fn test_timeline_above_statusbar() {
        let layout = LayoutState::default();
        let state = AppState::default();
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert!(rects.timeline_rect.max.y <= rects.statusbar_rect.min.y);
    }

    #[test]
    fn test_browser_zero_width_when_hidden() {
        let layout = LayoutState::default();
        let mut state = AppState::default();
        state.show_browser_panel = false;
        let rects = compute_layout(test_full_rect(), &layout, &state);
        assert_eq!(rects.browser_rect.width(), 0.0);
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
