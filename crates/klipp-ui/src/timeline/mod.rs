pub mod toolbar;
pub mod track_headers;
pub mod ruler;
pub mod playhead;
pub mod skimmer;
pub mod sequence_tabs;
pub mod time_utils;

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// New timeline panel that wraps the existing TimelinePanel
/// with the redesigned visual shell (toolbar, track headers, ruler).
///
/// Phase 7A: Structure + track headers + ruler (new visual shell)
/// Phase 7B: Clip rendering delegated to existing panels/timeline.rs
/// Phase 7C: Full migration of interaction logic
pub struct NewTimelinePanel;

impl NewTimelinePanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        old_timeline: &mut crate::panels::timeline::TimelinePanel,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let full_rect = ui.max_rect();

        // ── Timeline toolbar (34px) ──
        let toolbar_rect = egui::Rect::from_min_size(
            full_rect.min,
            egui::vec2(full_rect.width(), 34.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(toolbar_rect), |ui| {
            ui.painter().rect_filled(toolbar_rect, 0.0, theme.bg_panel);
            ui.painter().hline(
                toolbar_rect.x_range(),
                toolbar_rect.max.y,
                egui::Stroke::new(1.0, theme.border),
            );
            toolbar::show(ui, state, theme);
        });

        // ── Sequence tabs (24px) ──
        let tabs_rect = egui::Rect::from_min_size(
            egui::pos2(full_rect.min.x, toolbar_rect.max.y),
            egui::vec2(full_rect.width(), sequence_tabs::HEIGHT),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tabs_rect), |ui| {
            let tab_cmds = sequence_tabs::show(ui, state, theme);
            cmds.extend(tab_cmds);
        });

        // Content area below tabs
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x, tabs_rect.max.y),
            full_rect.max,
        );

        // ── Track headers (left 110px) ──
        let header_w = crate::theme::TRACK_HEADER_W;
        let header_rect = egui::Rect::from_min_size(
            content_rect.min,
            egui::vec2(header_w, content_rect.height()),
        );

        // ── Timeline content (right of headers) ──
        let timeline_content_rect = egui::Rect::from_min_max(
            egui::pos2(content_rect.min.x + header_w, content_rect.min.y),
            content_rect.max,
        );

        // Draw track headers
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.painter().rect_filled(header_rect, 0.0, theme.bg_deep);
            ui.painter().vline(
                header_rect.max.x,
                header_rect.y_range(),
                egui::Stroke::new(1.0, theme.border),
            );
            track_headers::show(ui, state, theme, header_rect);
        });

        // ── New adaptive ruler (22px at top of content area) ──
        let ruler_h = crate::theme::RULER_HEIGHT;
        let ruler_rect = egui::Rect::from_min_size(
            timeline_content_rect.min,
            egui::vec2(timeline_content_rect.width(), ruler_h),
        );
        // Use the existing timeline_zoom (already pps) for compatibility
        let pps = state.timeline_zoom;
        let scroll_x = state.timeline_scroll_x;
        let duration_secs = state.sequence_duration().to_secs_f64();

        let ruler_painter = ui.painter().with_clip_rect(ruler_rect);
        ruler::draw_ruler(&ruler_painter, ruler_rect, pps, scroll_x, duration_secs, theme);

        // Ruler click/drag → seek playhead
        let ruler_resp = ui.interact(ruler_rect, ui.id().with("new_ruler_seek"), egui::Sense::click_and_drag());
        if ruler_resp.clicked() || ruler_resp.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                let x_in_content = pos.x - ruler_rect.left() + scroll_x;
                let secs = (x_in_content / pps) as f64;
                let tc = klipp_core::RationalTime::from_secs_f64(secs, state.fps.rate());
                let dur = state.sequence_duration();
                let clamped = if dur.value > 0 {
                    klipp_core::RationalTime { value: tc.value.max(0).min(dur.value), rate: tc.rate }
                } else {
                    klipp_core::RationalTime { value: tc.value.max(0), rate: tc.rate }
                };
                cmds.push(AppCommand::SeekTo(clamped));
            }
        }

        // ── Track lanes + clips (delegate to old timeline below the ruler) ──
        let tracks_rect = egui::Rect::from_min_max(
            egui::pos2(timeline_content_rect.min.x, timeline_content_rect.min.y + ruler_h),
            timeline_content_rect.max,
        );
        // CRITICAL: clip everything to prevent bleed into inspector
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tracks_rect), |ui| {
            ui.set_clip_rect(tracks_rect.intersect(ui.clip_rect()));
            let old_cmds = old_timeline.show(ui, state);
            cmds.extend(old_cmds);
        });

        cmds
    }
}
