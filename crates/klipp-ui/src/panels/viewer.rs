use crate::icons::Icon;
use crate::inspector::video_tab::{find_transform_index, get_transform, TransformParams};
use crate::state::{AppState, TransformHandle, ViewerTransformDrag};
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Fps, RationalTime, ViewerMode};
use klipp_engine::PlaybackState;

#[derive(Default)]
pub struct ViewerPanel {
    pub video_texture: Option<egui::TextureHandle>,
    /// Separate texture for title overlay — rendered on UI thread for instant preview.
    /// Drawn on top of video_texture with alpha blending. Cleared when title is deselected.
    pub title_overlay_texture: Option<egui::TextureHandle>,
    pub title_overlay_size: (u32, u32),
}

impl ViewerPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // Use max_rect to get the FULL allocated rect — no insets from cursor/spacing
        let panel_rect = ui.max_rect();

        // Zero out spacing so nothing insets the content
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

        // Background: pure black, painted to the full rect
        ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);
        ui.advance_cursor_after_rect(panel_rect);

        // Reserve space: scrub bar (4px) + playbar (40px)
        let scrub_h = 4.0;
        let playbar_h = 40.0;
        let bottom_h = scrub_h + playbar_h;
        let viewer_rect = egui::Rect::from_min_max(
            panel_rect.min,
            egui::pos2(panel_rect.max.x, panel_rect.max.y - bottom_h),
        );

        // Source/Program toggle (top-left of viewer)
        {
            let toggle_y = panel_rect.min.y + 4.0;
            let toggle_x = panel_rect.min.x + 8.0;

            let prog_active = state.viewer_mode == ViewerMode::Program;
            let src_active = state.viewer_mode == ViewerMode::Source;

            // Program button
            let prog_rect = egui::Rect::from_min_size(
                egui::pos2(toggle_x, toggle_y),
                egui::vec2(60.0, 18.0),
            );
            let prog_resp = ui.allocate_rect(prog_rect, egui::Sense::click());
            let prog_color = if prog_active { theme.text_accent } else { theme.text_dim };
            ui.painter().text(
                prog_rect.center(), egui::Align2::CENTER_CENTER,
                "Program", egui::FontId::proportional(10.0), prog_color,
            );
            if prog_resp.clicked() {
                cmds.push(AppCommand::SetViewerMode(ViewerMode::Program));
            }

            // Source button
            let src_rect = egui::Rect::from_min_size(
                egui::pos2(toggle_x + 64.0, toggle_y),
                egui::vec2(50.0, 18.0),
            );
            let src_resp = ui.allocate_rect(src_rect, egui::Sense::click());
            let src_color = if src_active { theme.text_accent } else { theme.text_dim };
            ui.painter().text(
                src_rect.center(), egui::Align2::CENTER_CENTER,
                "Source", egui::FontId::proportional(10.0), src_color,
            );
            if src_resp.clicked() {
                cmds.push(AppCommand::SetViewerMode(ViewerMode::Source));
            }

            // Underline active tab
            if prog_active {
                ui.painter().hline(prog_rect.x_range(), prog_rect.max.y, egui::Stroke::new(2.0, theme.accent));
            } else {
                ui.painter().hline(src_rect.x_range(), src_rect.max.y, egui::Stroke::new(2.0, theme.accent));
            }
        }

        match state.viewer_mode {
            ViewerMode::Program => {
                self.show_program_viewer(ui, state, theme, viewer_rect, &mut cmds);
            }
            ViewerMode::Source => {
                self.show_source_viewer(ui, state, viewer_rect, &mut cmds);
            }
        }

        // ── Scrub bar ──
        let scrub_rect = egui::Rect::from_min_size(
            egui::pos2(panel_rect.min.x, viewer_rect.max.y),
            egui::vec2(panel_rect.width(), scrub_h),
        );
        self.draw_scrub_bar(ui, state, theme, scrub_rect, &mut cmds);

        // ── Playbar ──
        let playbar_rect = egui::Rect::from_min_size(
            egui::pos2(panel_rect.min.x, scrub_rect.max.y),
            egui::vec2(panel_rect.width(), playbar_h),
        );
        self.draw_playbar(ui, state, theme, playbar_rect, &mut cmds);

        cmds
    }

    pub fn show_fullscreen(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let available = ui.available_size();

        let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        let resp = ui.allocate_rect(panel_rect, egui::Sense::hover());
        ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);

        if let Some(tex) = &self.video_texture {
            let tex_size = tex.size_vec2();
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = available.x / available.y;
                let (dw, dh) = if video_aspect > panel_aspect {
                    (available.x, available.x / video_aspect)
                } else {
                    (available.y * video_aspect, available.y)
                };
                let image_rect = egui::Rect::from_center_size(
                    panel_rect.center(),
                    egui::vec2(dw, dh),
                );
                ui.painter().image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // Mouse movement shows overlay
        let dt = ui.input(|i| i.unstable_dt) as f64;
        if resp.hovered() && ui.input(|i| i.pointer.velocity().length() > 1.0) {
            state.fullscreen_overlay_visible = true;
            state.fullscreen_overlay_timer = 3.0;
        }
        if state.fullscreen_overlay_timer > 0.0 {
            state.fullscreen_overlay_timer -= dt;
            if state.fullscreen_overlay_timer <= 0.0 {
                state.fullscreen_overlay_visible = false;
                state.fullscreen_overlay_timer = 0.0;
            }
        }

        if state.fullscreen_overlay_visible {
            let bar_h = 48.0;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(panel_rect.left(), panel_rect.bottom() - bar_h),
                egui::vec2(panel_rect.width(), bar_h),
            );
            ui.painter().rect_filled(
                bar_rect, 0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 180),
            );
            let tc_text = state.playhead.to_timecode_string();
            ui.painter().text(
                egui::pos2(bar_rect.left() + 16.0, bar_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &tc_text,
                egui::FontId::monospace(16.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 220),
            );
            let playback_text = match state.playback {
                PlaybackState::Playing => "\u{25B6} Playing",
                PlaybackState::Paused => "\u{23F8} Paused",
                PlaybackState::Idle | PlaybackState::Stopped => "\u{23F9} Idle",
            };
            ui.painter().text(
                egui::pos2(bar_rect.right() - 16.0, bar_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                playback_text,
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgba_premultiplied(200, 200, 210, 200),
            );
            ui.ctx().request_repaint();
        }

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cmds.push(AppCommand::ToggleFullscreen);
        }
        if state.playback == PlaybackState::Playing {
            ui.ctx().request_repaint();
        }

        cmds
    }

    fn show_program_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let _ = cmds;

        // Extract media info up-front so the borrow is short-lived
        let current_media_info: Option<(u32, u32, bool)> = state.sequence().tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(state.playhead))
            .and_then(|(_, clip)| clip.media_id().and_then(|mid| state.media_pool.get(&mid)))
            .map(|m| (m.resolution.width, m.resolution.height, m.proxy_ready));

        let has_content = state.playback != PlaybackState::Idle || current_media_info.is_some();
        let lb_h = viewer_rect.height() * 0.08;

        if !has_content {
            // Empty state: Klipp shield + text at 40% opacity
            let logo_color = egui::Color32::from_rgba_premultiplied(232, 160, 32, 102);
            let center = viewer_rect.center();
            let shield_pts = vec![
                egui::pos2(center.x - 12.0, center.y - 24.0),
                egui::pos2(center.x, center.y - 30.0),
                egui::pos2(center.x + 12.0, center.y - 24.0),
                egui::pos2(center.x + 12.0, center.y - 8.0),
                egui::pos2(center.x, center.y),
                egui::pos2(center.x - 12.0, center.y - 8.0),
            ];
            ui.painter().add(egui::Shape::convex_polygon(
                shield_pts,
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.5, logo_color),
            ));
            ui.painter().text(
                egui::pos2(center.x, center.y + 16.0),
                egui::Align2::CENTER_CENTER,
                "Klipp",
                egui::FontId::proportional(16.0),
                logo_color,
            );
            // Empty state: no letterbox, no safe area — just clean black + logo
            return;
        }

        let (video_w, video_h) = current_media_info
            .map(|(w, h, _)| (w as f32, h as f32))
            .or_else(|| state.media_pool.all().next().map(|m| (m.resolution.width as f32, m.resolution.height as f32)))
            .unwrap_or((1920.0, 1080.0));

        if video_w == 0.0 || video_h == 0.0 { return; }

        // Small margin so the video frame never touches the panel edges.
        // This ensures transform handles at the frame corners are always fully visible.
        // The black background still fills the full panel — only the video fit area is inset.
        let fit_margin = 6.0;
        let fit_rect = viewer_rect.shrink(fit_margin);

        let video_aspect = video_w / video_h;
        let panel_aspect = fit_rect.width() / fit_rect.height();
        let (display_w, display_h) = if video_aspect > panel_aspect {
            (fit_rect.width(), fit_rect.width() / video_aspect)
        } else {
            (fit_rect.height() * video_aspect, fit_rect.height())
        };

        let image_rect = egui::Rect::from_center_size(
            fit_rect.center(),
            egui::vec2(display_w, display_h),
        );

        // Draw video frame
        if let Some(tex) = &self.video_texture {
            ui.painter().image(
                tex.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Draw title overlay (separate texture, alpha-blended on top of video)
        // Skip drawing the static overlay when in text edit mode (the TextEdit widget replaces it)
        if !state.viewer_text_edit {
            if let Some(tex) = &self.title_overlay_texture {
                ui.painter().image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // ── Viewer text edit mode ──
        // Double-click on viewer while a title clip is selected → enter text edit mode
        if !state.viewer_text_edit {
            if let Some(clip_id) = state.selected_clip {
                let is_title = state.sequence().tracks.iter()
                    .any(|t| t.find_clip(clip_id).is_some_and(|c| c.title_data().is_some()));
                if is_title && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                    if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                        if image_rect.contains(pos) {
                            state.viewer_text_edit = true;
                            state.viewer_text_edit_clip = Some(clip_id);
                            state.viewer_text_edit_request_focus = true;
                            // Load current text into edit buffer
                            if let Some(td) = state.sequence().tracks.iter()
                                .find_map(|t| t.find_clip(clip_id))
                                .and_then(|c| c.title_data())
                            {
                                state.viewer_text_edit_buffer = td.text.clone();
                            }
                        }
                    }
                }
            }
        }

        // Auto-exit text edit if the clip was deselected or changed
        if state.viewer_text_edit {
            let still_selected = state.viewer_text_edit_clip
                .is_some_and(|cid| state.selected_clip == Some(cid));
            if !still_selected {
                state.viewer_text_edit = false;
                state.viewer_text_edit_clip = None;
                state.viewer_text_edit_buffer.clear();
            }
        }

        // Draw text edit overlay when active
        if state.viewer_text_edit {
            // Semi-transparent dark overlay behind the text editor, clipped to image_rect
            let clipped_painter = ui.painter().with_clip_rect(image_rect);
            let overlay_bg = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120);
            clipped_painter.rect_filled(image_rect, 0.0, overlay_bg);

            // Get title position from TitleData for placement
            let (title_x, title_y) = state.viewer_text_edit_clip
                .and_then(|cid| {
                    state.sequence().tracks.iter()
                        .find_map(|t| t.find_clip(cid))
                        .and_then(|c| c.title_data().map(|td| (td.position.x, td.position.y)))
                })
                .unwrap_or((0.5, 0.5));

            // Position relative to image_rect (the letterboxed video area)
            let text_center = egui::pos2(
                image_rect.min.x + title_x as f32 * image_rect.width(),
                image_rect.min.y + title_y as f32 * image_rect.height(),
            );

            let text_edit_rect = egui::Rect::from_center_size(
                text_center,
                egui::vec2(image_rect.width() * 0.8, image_rect.height() * 0.4),
            );

            // Clamp the rect to stay within the image area
            let text_edit_rect = text_edit_rect.intersect(image_rect);

            let mut text = state.viewer_text_edit_buffer.clone();
            let te_response = ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(text_edit_rect),
                |ui| {
                    ui.set_clip_rect(image_rect);
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_width(text_edit_rect.width())
                            .font(egui::FontId::proportional(24.0))
                            .text_color(egui::Color32::WHITE)
                            .frame(false)
                            .cursor_at_end(true)
                    )
                },
            );

            // Request focus on the first frame
            if state.viewer_text_edit_request_focus {
                te_response.inner.request_focus();
                state.viewer_text_edit_request_focus = false;
            }

            if text != state.viewer_text_edit_buffer {
                state.viewer_text_edit_buffer = text.clone();
                // Emit UpdateTitleData command to sync changes live
                if let Some(clip_id) = state.viewer_text_edit_clip {
                    if let Some(td) = state.sequence().tracks.iter()
                        .find_map(|t| t.find_clip(clip_id))
                        .and_then(|c| c.title_data())
                    {
                        let font_style = match td.font_style {
                            klipp_timeline::FontStyle::Regular => 0u8,
                            klipp_timeline::FontStyle::Bold => 1,
                            klipp_timeline::FontStyle::Italic => 2,
                            klipp_timeline::FontStyle::BoldItalic => 3,
                        };
                        cmds.push(AppCommand::UpdateTitleData {
                            clip_id,
                            text,
                            font_size: td.font_size,
                            font_name: td.font_name.clone(),
                            font_style,
                            color: td.color,
                        });
                    }
                }
            }

            // Border and label clipped to image_rect so they don't overflow
            let accent = egui::Color32::from_rgb(255, 195, 50);
            clipped_painter.rect_stroke(
                text_edit_rect,
                4.0,
                egui::Stroke::new(2.0, accent),
            );

            // "Editing" label above the edit area (only if there's room within image_rect)
            let label_y = text_edit_rect.top() - 4.0;
            if label_y >= image_rect.top() + 12.0 {
                clipped_painter.text(
                    egui::pos2(text_edit_rect.left(), label_y),
                    egui::Align2::LEFT_BOTTOM,
                    "Editing Title \u{2014} press Escape to exit",
                    egui::FontId::proportional(11.0),
                    accent,
                );
            }

            // Exit on Escape or click outside the text edit area
            let exit_escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let click_outside = ui.input(|i| i.pointer.primary_clicked())
                && ui.input(|i| i.pointer.interact_pos())
                    .is_some_and(|pos| !text_edit_rect.contains(pos));
            if exit_escape || click_outside {
                state.viewer_text_edit = false;
                state.viewer_text_edit_clip = None;
                state.viewer_text_edit_buffer.clear();
            }
        }

        // ── On-screen transform controls ──
        // Suppress transform controls during text edit mode
        if state.show_transform_controls && !state.viewer_text_edit && state.eyedropper_mode == crate::state::EyedropperMode::None {
            self.draw_transform_controls(ui, state, image_rect, video_w, video_h, cmds);
        }

        // ── Before/After split divider ──
        if state.before_after_active {
            let split_x = image_rect.min.x + state.split_position * image_rect.width();
            let divider_color = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

            // Vertical divider line
            ui.painter().vline(
                split_x,
                image_rect.y_range(),
                egui::Stroke::new(2.0, divider_color),
            );

            // Labels
            let label_font = egui::FontId::proportional(10.0);
            let label_y = image_rect.min.y + 14.0;
            ui.painter().text(
                egui::pos2(split_x - 8.0, label_y),
                egui::Align2::RIGHT_CENTER,
                "Original",
                label_font.clone(),
                egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 180),
            );
            ui.painter().text(
                egui::pos2(split_x + 8.0, label_y),
                egui::Align2::LEFT_CENTER,
                "Graded",
                label_font,
                egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 180),
            );

            // Small triangle arrows on the divider
            let arrow_y = image_rect.center().y;
            let tri_size = 6.0;
            // Left arrow
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(split_x - 2.0, arrow_y - tri_size),
                    egui::pos2(split_x - 2.0 - tri_size, arrow_y),
                    egui::pos2(split_x - 2.0, arrow_y + tri_size),
                ],
                divider_color,
                egui::Stroke::NONE,
            ));
            // Right arrow
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(split_x + 2.0, arrow_y - tri_size),
                    egui::pos2(split_x + 2.0 + tri_size, arrow_y),
                    egui::pos2(split_x + 2.0, arrow_y + tri_size),
                ],
                divider_color,
                egui::Stroke::NONE,
            ));

            // Drag interaction on divider
            let drag_rect = egui::Rect::from_center_size(
                egui::pos2(split_x, image_rect.center().y),
                egui::vec2(16.0, image_rect.height()),
            );
            let drag_resp = ui.allocate_rect(drag_rect, egui::Sense::drag());
            if drag_resp.dragged() {
                if let Some(pos) = drag_resp.interact_pointer_pos() {
                    state.split_position = ((pos.x - image_rect.min.x) / image_rect.width())
                        .clamp(0.05, 0.95);
                }
            }
            if drag_resp.hovered() || drag_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }

        // ── Letterbox bars (drawn ON TOP of video frame) ──
        ui.painter().rect_filled(
            egui::Rect::from_min_size(viewer_rect.min, egui::vec2(viewer_rect.width(), lb_h)),
            0.0, egui::Color32::BLACK,
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(viewer_rect.min.x, viewer_rect.max.y - lb_h),
                egui::vec2(viewer_rect.width(), lb_h),
            ),
            0.0, egui::Color32::BLACK,
        );

        // ── Eyedropper interaction (White Balance or Qualifier) ──
        if state.eyedropper_mode != crate::state::EyedropperMode::None {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let eyedropper_resp = ui.allocate_rect(image_rect, egui::Sense::click());
            if eyedropper_resp.clicked() {
                if let Some(pos) = eyedropper_resp.interact_pointer_pos() {
                    let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    match state.eyedropper_mode {
                        crate::state::EyedropperMode::WhiteBalance => {
                            state.wb_sample_request = Some((nx, ny));
                        }
                        crate::state::EyedropperMode::Qualifier => {
                            state.qualifier_sample_request = Some((nx, ny));
                        }
                        crate::state::EyedropperMode::None => {}
                    }
                    state.eyedropper_mode = crate::state::EyedropperMode::None;
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.eyedropper_mode = crate::state::EyedropperMode::None;
            }
        }

        // ── Interactive mask handles on viewer ──
        // Draw and interact with masks from the selected node in the clip's node graph.
        self.draw_mask_handles(ui, state, image_rect, cmds);

        // ── Overlay mode badge (top-right of video) ──
        {
            use crate::state::ViewerOverlay;
            let overlay_label = match state.viewer_overlay {
                ViewerOverlay::Zebra => Some("ZEBRA"),
                ViewerOverlay::FalseColor => Some("FALSE COLOR"),
                ViewerOverlay::None => None,
            };
            if let Some(label) = overlay_label {
                let badge_pos = egui::pos2(image_rect.max.x - 8.0, image_rect.min.y + 12.0);
                let badge_color = egui::Color32::from_rgb(0xff, 0x40, 0x40);
                // Shadow
                ui.painter().text(
                    egui::pos2(badge_pos.x + 0.5, badge_pos.y + 0.5),
                    egui::Align2::RIGHT_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_rgb(0x00, 0x00, 0x00),
                );
                ui.painter().text(
                    badge_pos,
                    egui::Align2::RIGHT_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    badge_color,
                );
            }
        }

        // Safe area overlay removed — it looked like a viewer border

        // Timecode + zoom now shown in playbar below scrub bar

        // Proxy badge
        if state.use_proxies && current_media_info.is_some_and(|(_, _, proxy_ready)| proxy_ready) {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            ui.painter().text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PROXY",
                egui::FontId::monospace(9.0),
                theme.accent,
            );
        }

        // Performance overlay
        if state.show_perf_overlay {
            self.draw_perf_overlay(ui, state, image_rect);
        }
    }

    fn draw_scrub_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // Track background
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);

        // Progress fill
        let dur_secs = state.sequence_duration().to_secs_f64();
        let progress = if dur_secs > 0.0 {
            (state.playhead_smooth_secs / dur_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let fill_w = progress * rect.width();
        if fill_w > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height())),
                0.0,
                theme.accent,
            );
        }

        // Playhead head (11x11 circle)
        let head_x = rect.min.x + fill_w;
        let head_center = egui::pos2(head_x, rect.center().y);
        ui.painter().circle_filled(head_center, 7.0, theme.accent_glow);
        ui.painter().circle_filled(head_center, 5.5, theme.accent);

        // Scrub interaction
        if resp.dragged() || resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let t = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0) as f64;
                let target_secs = t * dur_secs;
                let target_tc = klipp_core::Timecode::from_secs_f64(target_secs, state.fps.rate());
                cmds.push(AppCommand::SeekTo(target_tc));
            }
        }
    }

    fn draw_playbar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let enabled = state.playback != PlaybackState::Idle;
        let fps_val = state.fps.as_f64();
        let cy = rect.center().y;

        // Background
        ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(24, 24, 30));

        // ── Left: Timecode ──
        let tc_str = format_timecode_hmsf(state.playhead_smooth_secs, state.fps.rate());
        if state.editing_timecode {
            let tc_rect = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 8.0, cy - 12.0),
                egui::vec2(110.0, 24.0),
            );
            let mut tc_ui = ui.new_child(egui::UiBuilder::new().max_rect(tc_rect));
            let response = tc_ui.add(
                egui::TextEdit::singleline(&mut state.timecode_input)
                    .desired_width(100.0)
                    .font(egui::TextStyle::Monospace),
            );
            if !response.has_focus() { response.request_focus(); }
            let lost = response.lost_focus();
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if lost || esc {
                if enter {
                    if let Some(tc) = parse_timecode_input(&state.timecode_input, state.fps) {
                        cmds.push(AppCommand::SeekTo(tc));
                    }
                }
                state.editing_timecode = false;
            }
        } else {
            let tc_resp = ui.allocate_rect(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 8.0, rect.min.y),
                    egui::vec2(110.0, rect.height()),
                ),
                egui::Sense::click(),
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 12.0, cy),
                egui::Align2::LEFT_CENTER,
                &tc_str,
                egui::FontId::monospace(14.0),
                egui::Color32::from_rgb(200, 200, 210),
            );
            if tc_resp.clicked() {
                state.editing_timecode = true;
                state.timecode_input = tc_str;
            }
        }

        // ── Center: Transport buttons ──
        let btn_size = 30.0;
        let play_size = 38.0;
        let btn_spacing = 4.0;
        let total_w = 7.0 * btn_size + play_size - btn_size + 6.0 * btn_spacing;
        let mut x = rect.center().x - total_w / 2.0;

        let nav_fg = egui::Color32::from_rgb(220, 220, 230);
        let nav_bg = egui::Color32::from_rgb(40, 40, 48);
        let nav_hover = egui::Color32::from_rgb(55, 55, 65);
        let disabled_fg = egui::Color32::from_rgb(80, 80, 90);

        // Skip back
        x += self.playbar_nav_btn(ui, Icon::SkipBack, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to start (Home)", cmds, Some(AppCommand::SeekTo(klipp_core::Timecode::zero())));
        // Rewind
        x += self.playbar_nav_btn(ui, Icon::Rewind, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Rewind (J)", cmds, Some(AppCommand::SeekRelative(-(fps_val * 10.0) as i64)));
        // Step back
        x += self.playbar_nav_btn(ui, Icon::CaretLeft, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step back 1 frame", cmds, Some(AppCommand::SeekRelative(-1)));

        // Play / Pause (larger, amber)
        {
            let play_rect = egui::Rect::from_center_size(
                egui::pos2(x + play_size / 2.0, cy),
                egui::vec2(play_size, play_size),
            );
            let resp = ui.allocate_rect(play_rect, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
            let is_playing = state.playback == PlaybackState::Playing;
            let bg = if enabled { theme.accent } else { egui::Color32::from_rgb(80, 60, 20) };
            ui.painter().rect_filled(play_rect, play_size / 2.0, bg);
            let play_icon = if is_playing { Icon::Pause } else { Icon::Play };
            let icon_rect = egui::Rect::from_center_size(play_rect.center(), egui::vec2(18.0, 18.0));
            play_icon.bold(18.0).tint(egui::Color32::BLACK).paint_at(ui, icon_rect);
            let clicked = resp.clicked();
            resp.on_hover_text("Play / Pause (Space)");
            if clicked && enabled {
                if is_playing { cmds.push(AppCommand::Pause); } else { cmds.push(AppCommand::Play); }
            }
        }
        x += play_size + btn_spacing;

        // Step forward
        x += self.playbar_nav_btn(ui, Icon::CaretRight, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step forward 1 frame", cmds, Some(AppCommand::SeekRelative(1)));
        // Fast forward
        x += self.playbar_nav_btn(ui, Icon::FastForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Fast forward (L)", cmds, Some(AppCommand::SeekRelative((fps_val * 10.0) as i64)));
        // Skip forward
        {
            let dur = state.sequence_duration();
            self.playbar_nav_btn(ui, Icon::SkipForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to end (End)", cmds, Some(AppCommand::SeekTo(dur)));
        }

        // ── Right: Transform | Mute | Slider | Vol% | Zoom% ──
        // Layout from right: zoom(44) gap(8) vol%(40) gap(6) slider(60) gap(6) mute(20) gap(8) transform(20)
        let rx = rect.max.x - 12.0;

        // Zoom percentage
        ui.painter().text(
            egui::pos2(rx, cy),
            egui::Align2::RIGHT_CENTER,
            "100%",
            egui::FontId::monospace(11.0),
            egui::Color32::from_rgb(120, 120, 130),
        );

        // Volume percentage
        let vol_pct = (state.master_volume * 100.0) as u32;
        let vol_text_x = rx - 52.0;
        ui.painter().text(
            egui::pos2(vol_text_x, cy),
            egui::Align2::RIGHT_CENTER,
            &format!("{}%", vol_pct),
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgb(140, 140, 150),
        );

        // Volume slider
        let slider_w = 60.0;
        let slider_right = vol_text_x - 8.0;
        let slider_rect = egui::Rect::from_min_size(
            egui::pos2(slider_right - slider_w, cy - 8.0),
            egui::vec2(slider_w, 16.0),
        );
        let mut vol = state.master_volume;
        let mut slider_ui = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
        let slider = egui::Slider::new(&mut vol, 0.0..=1.3).show_value(false);
        if slider_ui.add(slider).changed() {
            state.master_volume = vol;
            let effective = if state.master_muted { 0.0 } else { vol };
            cmds.push(AppCommand::SetMasterVolume(effective));
        }

        // Mute button
        let mute_x = slider_rect.min.x - 22.0;
        let mute_rect = egui::Rect::from_center_size(
            egui::pos2(mute_x, cy),
            egui::vec2(20.0, 20.0),
        );
        let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click());
        let mute_icon = if state.master_muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
        let mute_color = if state.master_muted {
            egui::Color32::from_rgb(255, 80, 80)
        } else {
            egui::Color32::from_rgb(160, 160, 170)
        };
        let mute_icon_rect = egui::Rect::from_center_size(mute_rect.center(), egui::vec2(14.0, 14.0));
        mute_icon.image(14.0).tint(mute_color).paint_at(ui, mute_icon_rect);
        if mute_resp.clicked() {
            state.master_muted = !state.master_muted;
            let effective = if state.master_muted { 0.0 } else { state.master_volume };
            cmds.push(AppCommand::SetMasterVolume(effective));
        }

        // Transform controls toggle button
        let xform_x = mute_x - 26.0;
        let xform_rect = egui::Rect::from_center_size(
            egui::pos2(xform_x, cy),
            egui::vec2(20.0, 20.0),
        );
        let xform_resp = ui.allocate_rect(xform_rect, egui::Sense::click());
        let xform_color = if state.show_transform_controls {
            theme.accent
        } else {
            egui::Color32::from_rgb(100, 100, 110)
        };
        // Draw a small bounding-box icon (square with corner handles)
        let ir = xform_rect.shrink(4.0);
        ui.painter().rect_stroke(ir, 0.0, egui::Stroke::new(1.2, xform_color));
        let cs = 2.0; // corner square half-size
        for corner in [ir.left_top(), ir.right_top(), ir.left_bottom(), ir.right_bottom()] {
            ui.painter().rect_filled(
                egui::Rect::from_center_size(corner, egui::vec2(cs * 2.0, cs * 2.0)),
                0.0,
                xform_color,
            );
        }
        if xform_resp.clicked() {
            state.show_transform_controls = !state.show_transform_controls;
        }
        xform_resp.on_hover_text("Toggle transform controls");
    }

    /// Helper for playbar navigation buttons. Returns advance width (btn_size + spacing).
    #[allow(clippy::too_many_arguments)]
    fn playbar_nav_btn(
        &self,
        ui: &mut egui::Ui,
        icon: Icon,
        x: f32,
        cy: f32,
        size: f32,
        enabled: bool,
        fg: egui::Color32,
        bg: egui::Color32,
        hover_bg: egui::Color32,
        disabled_fg: egui::Color32,
        tooltip: &str,
        cmds: &mut Vec<AppCommand>,
        cmd: Option<AppCommand>,
    ) -> f32 {
        let btn_rect = egui::Rect::from_center_size(
            egui::pos2(x + size / 2.0, cy),
            egui::vec2(size, size),
        );
        let resp = ui.allocate_rect(btn_rect, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
        let fill = if resp.hovered() && enabled { hover_bg } else { bg };
        ui.painter().rect_filled(btn_rect, size / 2.0, fill);
        let icon_rect = egui::Rect::from_center_size(btn_rect.center(), egui::vec2(14.0, 14.0));
        icon.bold(14.0).tint(if enabled { fg } else { disabled_fg }).paint_at(ui, icon_rect);
        let clicked = resp.clicked();
        resp.on_hover_text(tooltip);
        if clicked && enabled {
            if let Some(c) = cmd { cmds.push(c); }
        }
        size + 4.0 // btn_size + spacing
    }

    fn draw_perf_overlay(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        image_rect: egui::Rect,
    ) {
        let font = egui::FontId::monospace(11.0);
        let line_h = 15.0;
        let pad = 6.0;

        let hw_label = if state.perf_hw_decode { "NVDEC" } else { "SW" };
        let playback_label = match state.playback {
            PlaybackState::Playing => "PLAY",
            PlaybackState::Paused => "PAUSE",
            PlaybackState::Stopped => "STOP",
            PlaybackState::Idle => "IDLE",
        };

        let lines = [
            format!("UI FPS: {:.0}", state.perf_fps),
            format!("Decode: {hw_label}"),
            format!("State:  {playback_label}"),
            format!("Frames: {}", state.perf_frame_count),
            format!("Drops:  {}", state.perf_dropped_frames),
            format!("CPU:    {:.0}%", state.perf_cpu_percent),
            format!("Mem:    {:.0} MB", state.perf_memory_mb),
        ];

        let max_w = lines.iter()
            .map(|l| {
                ui.painter()
                    .layout_no_wrap(l.clone(), font.clone(), egui::Color32::WHITE)
                    .size().x
            })
            .fold(0.0f32, f32::max);

        let box_w = max_w + pad * 2.0;
        let box_h = lines.len() as f32 * line_h + pad * 2.0;
        let box_pos = egui::pos2(image_rect.left() + 4.0, image_rect.top() + 4.0);
        let box_rect = egui::Rect::from_min_size(box_pos, egui::vec2(box_w, box_h));

        ui.painter().rect_filled(
            box_rect, 4.0,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 200),
        );

        for (i, line) in lines.iter().enumerate() {
            let color = if i == 4 && state.perf_dropped_frames > 0 {
                egui::Color32::from_rgb(255, 100, 100)
            } else if i == 1 && state.perf_hw_decode {
                egui::Color32::from_rgb(100, 255, 100)
            } else if i == 5 && state.perf_cpu_percent > 80.0 {
                egui::Color32::from_rgb(255, 180, 80)
            } else {
                egui::Color32::from_rgb(220, 220, 230)
            };
            ui.painter().text(
                egui::pos2(box_pos.x + pad, box_pos.y + pad + i as f32 * line_h),
                egui::Align2::LEFT_TOP,
                line,
                font.clone(),
                color,
            );
        }
    }

    /// Draw interactive mask shape outlines and drag handles in the viewer.
    fn draw_mask_handles(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        use crate::state::{MaskHandle, ViewerMaskDrag};

        // Only show masks when a clip and node are selected
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                state.viewer_mask_drag = None;
                return;
            }
        };
        let node_id = match state.selected_node_id {
            Some(id) => id,
            None => {
                state.viewer_mask_drag = None;
                return;
            }
        };

        // Find the selected node's masks in the clip's node graph
        let masks_opt: Option<klipp_core::MaskStack> = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .and_then(|clip| clip.node_graph.nodes.iter().find(|n| n.id == node_id))
            .and_then(|node| node.params.masks.clone());

        let masks = match masks_opt {
            Some(ms) if !ms.masks.is_empty() => ms,
            _ => {
                state.viewer_mask_drag = None;
                return;
            }
        };

        let iw = image_rect.width();
        let ih = image_rect.height();
        let handle_radius = 5.0_f32;
        let outline_color = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
        let handle_color = egui::Color32::WHITE;

        // Helper: convert normalized coords to screen coords
        let to_screen = |nx: f32, ny: f32| -> egui::Pos2 {
            egui::pos2(image_rect.min.x + nx * iw, image_rect.min.y + ny * ih)
        };

        for (mask_idx, mask) in masks.masks.iter().enumerate() {
            if !mask.enabled { continue; }

            match &mask.shape {
                klipp_core::MaskShape::Circle { center_x, center_y, radius } => {
                    let center = to_screen(*center_x, *center_y);
                    let r_px = radius * iw;

                    // Draw outline ellipse (approximate with circle)
                    ui.painter().circle_stroke(center, r_px, egui::Stroke::new(1.5, outline_color));

                    // Center handle
                    let center_handle = egui::Rect::from_center_size(center, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(center, handle_radius, handle_color);

                    // Right edge handle (radius control)
                    let right = egui::pos2(center.x + r_px, center.y);
                    let right_handle = egui::Rect::from_center_size(right, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(right, handle_radius, handle_color);

                    // Bottom edge handle (radius control)
                    let bottom = egui::pos2(center.x, center.y + r_px);
                    let bottom_handle = egui::Rect::from_center_size(bottom, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(bottom, handle_radius, handle_color);

                    // Interaction — start drags
                    if state.viewer_mask_drag.is_none() {
                        for (handle_rect, handle_kind) in [
                            (right_handle, MaskHandle::EdgeRight),
                            (bottom_handle, MaskHandle::EdgeBottom),
                            (center_handle, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::drag());
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: mask.shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(match handle_kind {
                                    MaskHandle::Center => egui::CursorIcon::Move,
                                    MaskHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                                    MaskHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                                });
                            }
                        }
                    }
                }
                klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation: _ } => {
                    let center = to_screen(*center_x, *center_y);
                    let hw = width * iw * 0.5;
                    let hh = height * ih * 0.5;

                    // Draw outline rectangle (ignoring rotation for simplicity in handles)
                    let rect = egui::Rect::from_center_size(center, egui::vec2(hw * 2.0, hh * 2.0));
                    ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.5, outline_color));

                    // Center handle
                    let center_handle = egui::Rect::from_center_size(center, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(center, handle_radius, handle_color);

                    // Right edge handle (width control)
                    let right = egui::pos2(center.x + hw, center.y);
                    let right_handle = egui::Rect::from_center_size(right, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(right, handle_radius, handle_color);

                    // Bottom edge handle (height control)
                    let bottom = egui::pos2(center.x, center.y + hh);
                    let bottom_handle = egui::Rect::from_center_size(bottom, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(bottom, handle_radius, handle_color);

                    if state.viewer_mask_drag.is_none() {
                        for (handle_rect, handle_kind) in [
                            (right_handle, MaskHandle::EdgeRight),
                            (bottom_handle, MaskHandle::EdgeBottom),
                            (center_handle, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::drag());
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: mask.shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(match handle_kind {
                                    MaskHandle::Center => egui::CursorIcon::Move,
                                    MaskHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                                    MaskHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                                });
                            }
                        }
                    }
                }
                klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                    // Draw a line from start to end
                    let start = to_screen(*start_x, *start_y);
                    let end = to_screen(*end_x, *end_y);
                    ui.painter().line_segment([start, end], egui::Stroke::new(1.5, outline_color));
                    ui.painter().circle_filled(start, handle_radius, handle_color);
                    ui.painter().circle_filled(end, handle_radius, handle_color);
                    // Linear gradient handles: use Center for start, EdgeRight for end
                    if state.viewer_mask_drag.is_none() {
                        let start_rect = egui::Rect::from_center_size(start, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                        let end_rect = egui::Rect::from_center_size(end, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                        for (handle_rect, handle_kind) in [
                            (end_rect, MaskHandle::EdgeRight),
                            (start_rect, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::drag());
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: mask.shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                            }
                        }
                    }
                }
            }
        }

        // Process ongoing drag
        if let Some(drag) = &state.viewer_mask_drag {
            let drag_clip_id = drag.clip_id;
            let drag_node_id = drag.node_id;
            let drag_mask_idx = drag.mask_index;
            let drag_handle = drag.handle;
            let drag_start = drag.drag_start_pos;
            let initial_shape = drag.initial_shape.clone();

            // Check if pointer is still down
            let pointer_down = ui.input(|i| i.pointer.any_down());
            let current_pos = ui.input(|i| i.pointer.interact_pos()).unwrap_or(drag_start);

            if pointer_down {
                let dx_norm = (current_pos.x - drag_start.x) / iw;
                let dy_norm = (current_pos.y - drag_start.y) / ih;

                let new_shape = match (&initial_shape, drag_handle) {
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius }, MaskHandle::Center) => {
                        klipp_core::MaskShape::Circle {
                            center_x: (center_x + dx_norm).clamp(0.0, 1.0),
                            center_y: (center_y + dy_norm).clamp(0.0, 1.0),
                            radius: *radius,
                        }
                    }
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius: _ }, MaskHandle::EdgeRight) => {
                        let new_r = ((current_pos.x - (image_rect.min.x + center_x * iw)) / iw).abs().max(0.01);
                        klipp_core::MaskShape::Circle { center_x: *center_x, center_y: *center_y, radius: new_r }
                    }
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius: _ }, MaskHandle::EdgeBottom) => {
                        let new_r = ((current_pos.y - (image_rect.min.y + center_y * ih)) / iw).abs().max(0.01);
                        klipp_core::MaskShape::Circle { center_x: *center_x, center_y: *center_y, radius: new_r }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation }, MaskHandle::Center) => {
                        klipp_core::MaskShape::Rectangle {
                            center_x: (center_x + dx_norm).clamp(0.0, 1.0),
                            center_y: (center_y + dy_norm).clamp(0.0, 1.0),
                            width: *width,
                            height: *height,
                            rotation: *rotation,
                        }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width: _, height, rotation }, MaskHandle::EdgeRight) => {
                        let new_w = ((current_pos.x - (image_rect.min.x + center_x * iw)) / iw * 2.0).abs().max(0.01);
                        klipp_core::MaskShape::Rectangle { center_x: *center_x, center_y: *center_y, width: new_w, height: *height, rotation: *rotation }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width, height: _, rotation }, MaskHandle::EdgeBottom) => {
                        let new_h = ((current_pos.y - (image_rect.min.y + center_y * ih)) / ih * 2.0).abs().max(0.01);
                        klipp_core::MaskShape::Rectangle { center_x: *center_x, center_y: *center_y, width: *width, height: new_h, rotation: *rotation }
                    }
                    (klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y }, MaskHandle::Center) => {
                        klipp_core::MaskShape::LinearGradient {
                            start_x: (start_x + dx_norm).clamp(0.0, 1.0),
                            start_y: (start_y + dy_norm).clamp(0.0, 1.0),
                            end_x: (end_x + dx_norm).clamp(0.0, 1.0),
                            end_y: (end_y + dy_norm).clamp(0.0, 1.0),
                        }
                    }
                    (klipp_core::MaskShape::LinearGradient { start_x, start_y, .. }, MaskHandle::EdgeRight) => {
                        klipp_core::MaskShape::LinearGradient {
                            start_x: *start_x,
                            start_y: *start_y,
                            end_x: ((current_pos.x - image_rect.min.x) / iw).clamp(0.0, 1.0),
                            end_y: ((current_pos.y - image_rect.min.y) / ih).clamp(0.0, 1.0),
                        }
                    }
                    _ => initial_shape.clone(),
                };

                // Build updated mask stack and emit command
                let mut updated_masks = masks.clone();
                if drag_mask_idx < updated_masks.masks.len() {
                    updated_masks.masks[drag_mask_idx].shape = new_shape;
                }
                cmds.push(AppCommand::SetNodeMasks {
                    clip_id: drag_clip_id,
                    node_id: drag_node_id,
                    masks: updated_masks,
                });
            } else {
                // Pointer released — end drag
                state.viewer_mask_drag = None;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_transform_controls(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        frame_w: f32,
        frame_h: f32,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Only show when a video clip is selected
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };

        // Find the clip in the sequence
        let clip = match state.sequence().tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
        {
            Some(c) => c,
            None => return,
        };

        // Only show transform controls for clips (skip gaps)
        if matches!(clip.kind, klipp_timeline::ClipKind::Gap) {
            return;
        }

        let transform = get_transform(clip);
        let effect_index = find_transform_index(clip);

        // Coordinate mapping: frame pixels -> screen pixels
        let scale_to_screen_x = image_rect.width() / frame_w;
        let scale_to_screen_y = image_rect.height() / frame_h;
        let center_screen = image_rect.center();

        // Anchor point (0.0-1.0, default 0.5 = center)
        let anchor_x = transform.anchor_x;
        let anchor_y = transform.anchor_y;

        // pos_screen is the anchor/pivot point in screen space
        let pos_screen = egui::pos2(
            center_screen.x + transform.position_x * scale_to_screen_x,
            center_screen.y + transform.position_y * scale_to_screen_y,
        );

        // Bounding box half-extents in screen space
        let half_w = frame_w * 0.5 * transform.scale_x * scale_to_screen_x;
        let half_h = frame_h * 0.5 * transform.scale_y * scale_to_screen_y;

        // The anchor offset: how far the box center is from the anchor point.
        // When anchor is (0.5, 0.5) this is zero (center pivot).
        // When anchor is (0.0, 0.0) the pivot is at the top-left, so the box
        // center is shifted right and down by half_w/half_h.
        let anchor_offset_x = (anchor_x - 0.5) * frame_w * transform.scale_x * scale_to_screen_x;
        let anchor_offset_y = (anchor_y - 0.5) * frame_h * transform.scale_y * scale_to_screen_y;

        // The bounding box center, offset so the anchor maps to pos_screen
        let box_center_x = pos_screen.x - anchor_offset_x;
        let box_center_y = pos_screen.y - anchor_offset_y;

        let rotation_rad = transform.rotation.to_radians();

        // Unrotated corners relative to box center
        let raw_corners = [
            egui::pos2(box_center_x - half_w, box_center_y - half_h), // top-left
            egui::pos2(box_center_x + half_w, box_center_y - half_h), // top-right
            egui::pos2(box_center_x + half_w, box_center_y + half_h), // bottom-right
            egui::pos2(box_center_x - half_w, box_center_y + half_h), // bottom-left
        ];

        // Rotate corners around pos_screen (the anchor/pivot point)
        let corners: [egui::Pos2; 4] = std::array::from_fn(|i| {
            rotate_point(raw_corners[i], pos_screen, rotation_rad)
        });

        // Use a painter with an expanded clip rect so transform handles
        // near the edges of image_rect are not clipped (FCP-style: handles
        // remain visible even when the bounding box extends outside the video frame).
        // Use an expanded clip rect so transform handles at the frame edges are fully visible.
        // The handles are 8px squares and the rotation handle extends 30px above the frame.
        // Use screen rect to ensure handles can be drawn anywhere visible
        let screen_rect = ui.ctx().screen_rect();
        let painter = ui.painter().with_clip_rect(screen_rect);

        // Draw bounding box outline
        let outline_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180);
        let outline_stroke = egui::Stroke::new(1.5, outline_color);
        for i in 0..4 {
            painter.line_segment([corners[i], corners[(i + 1) % 4]], outline_stroke);
        }

        // Draw diagonal crosshairs through center (subtle)
        let cross_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60);
        let cross_stroke = egui::Stroke::new(0.5, cross_color);
        let mid_top = egui::pos2(
            (corners[0].x + corners[1].x) * 0.5,
            (corners[0].y + corners[1].y) * 0.5,
        );
        let mid_bottom = egui::pos2(
            (corners[2].x + corners[3].x) * 0.5,
            (corners[2].y + corners[3].y) * 0.5,
        );
        let mid_left = egui::pos2(
            (corners[0].x + corners[3].x) * 0.5,
            (corners[0].y + corners[3].y) * 0.5,
        );
        let mid_right = egui::pos2(
            (corners[1].x + corners[2].x) * 0.5,
            (corners[1].y + corners[2].y) * 0.5,
        );
        painter.line_segment([mid_top, mid_bottom], cross_stroke);
        painter.line_segment([mid_left, mid_right], cross_stroke);

        // Corner handles (scale)
        let handle_size = 8.0;
        let handle_half = handle_size / 2.0;
        for corner in &corners {
            let handle_rect = egui::Rect::from_center_size(*corner, egui::vec2(handle_size, handle_size));
            painter.rect_filled(handle_rect, 0.0, egui::Color32::WHITE);
            painter.rect_stroke(handle_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
        }

        // Center handle (position) — circle
        let center_radius = 6.0;
        painter.circle_filled(pos_screen, center_radius, egui::Color32::WHITE);
        painter.circle_stroke(pos_screen, center_radius, egui::Stroke::new(1.0, egui::Color32::BLACK));
        // Small crosshair inside center handle
        let ch = 3.0;
        painter.line_segment(
            [egui::pos2(pos_screen.x - ch, pos_screen.y), egui::pos2(pos_screen.x + ch, pos_screen.y)],
            egui::Stroke::new(1.0, egui::Color32::BLACK),
        );
        painter.line_segment(
            [egui::pos2(pos_screen.x, pos_screen.y - ch), egui::pos2(pos_screen.x, pos_screen.y + ch)],
            egui::Stroke::new(1.0, egui::Color32::BLACK),
        );

        // Anchor point indicator — small diamond at the pivot
        let anchor_diamond = 4.0;
        let anchor_pts = [
            egui::pos2(pos_screen.x, pos_screen.y - anchor_diamond),
            egui::pos2(pos_screen.x + anchor_diamond, pos_screen.y),
            egui::pos2(pos_screen.x, pos_screen.y + anchor_diamond),
            egui::pos2(pos_screen.x - anchor_diamond, pos_screen.y),
        ];
        let anchor_color = egui::Color32::from_rgba_unmultiplied(255, 200, 50, 220);
        for i in 0..4 {
            painter.line_segment(
                [anchor_pts[i], anchor_pts[(i + 1) % 4]],
                egui::Stroke::new(1.5, anchor_color),
            );
        }

        // Rotation handle — small circle above the top edge
        let rot_distance = 25.0;
        let rot_handle_pos = rotate_point(
            egui::pos2(box_center_x, box_center_y - half_h - rot_distance),
            pos_screen,
            rotation_rad,
        );
        let rot_connector_base = mid_top;
        painter.line_segment(
            [rot_connector_base, rot_handle_pos],
            egui::Stroke::new(1.5, outline_color),
        );
        let rot_radius = 5.0;
        painter.circle_filled(rot_handle_pos, rot_radius, egui::Color32::WHITE);
        painter.circle_stroke(rot_handle_pos, rot_radius, egui::Stroke::new(1.0, egui::Color32::BLACK));
        // Small arc icon inside rotation handle
        let arc_r = 2.5;
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x - arc_r, rot_handle_pos.y),
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
            ],
            egui::Stroke::new(0.8, egui::Color32::BLACK),
        );
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
                egui::pos2(rot_handle_pos.x + arc_r, rot_handle_pos.y),
            ],
            egui::Stroke::new(0.8, egui::Color32::BLACK),
        );

        // ── Interaction ──
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_released = ui.input(|i| i.pointer.primary_released());
        let shift_held = ui.input(|i| i.modifiers.shift);
        let hit_radius = 10.0;

        // Handle ongoing drag
        if let Some(drag) = state.viewer_transform_drag.take() {
            if pointer_released {
                // Drag ended — emit EndEffectDrag for undo coalescing
                let final_transform = self.compute_dragged_transform(
                    &drag, pointer_pos,
                    scale_to_screen_x, scale_to_screen_y,
                    center_screen, shift_held,
                );
                if let Some(idx) = effect_index.or(Some(drag.effect_index)) {
                    cmds.push(AppCommand::EndEffectDrag {
                        key: format!("viewer_transform_{}", clip_id.0),
                        clip_id,
                        effect_index: idx,
                        new_effect: final_transform.to_builtin_effect(),
                    });
                }
                // Don't put drag back — it's done
            } else if pointer_down {
                // Drag continues — compute new transform and emit update
                let new_transform = self.compute_dragged_transform(
                    &drag, pointer_pos,
                    scale_to_screen_x, scale_to_screen_y,
                    center_screen, shift_held,
                );
                let idx = effect_index.unwrap_or(drag.effect_index);
                cmds.push(AppCommand::UpdateBuiltinEffect {
                    clip_id,
                    index: idx,
                    effect: new_transform.to_builtin_effect(),
                });

                // Set cursor icon based on handle
                match drag.handle {
                    TransformHandle::Position => {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                    TransformHandle::Rotation => {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                    _ => {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                }

                // Put drag back
                state.viewer_transform_drag = Some(drag);
                ui.ctx().request_repaint();
            }
            // If pointer is neither down nor released (shouldn't happen), drop drag
        } else if let Some(pointer) = pointer_pos {
            // Check for new drag start
            if pointer_down && ui.input(|i| i.pointer.primary_pressed()) {
                // Only start a drag if the pointer is within the image_rect (with some margin)
                let expanded = image_rect.expand(40.0);
                if expanded.contains(pointer) {
                    let handle = self.hit_test_handles(
                        pointer, pos_screen, &corners, rot_handle_pos,
                        center_radius, handle_half, rot_radius, hit_radius,
                    );

                    if let Some(handle) = handle {
                        let eff_idx = effect_index.unwrap_or(0);
                        // If no transform effect exists, add one first
                        if effect_index.is_none() {
                            cmds.push(AppCommand::AddBuiltinEffect {
                                clip_id,
                                effect: transform.to_builtin_effect(),
                            });
                        }

                        // Begin undo coalescing
                        cmds.push(AppCommand::BeginEffectDrag {
                            key: format!("viewer_transform_{}", clip_id.0),
                            clip_id,
                            effect_index: eff_idx,
                            old_effect: transform.to_builtin_effect(),
                        });

                        state.viewer_transform_drag = Some(ViewerTransformDrag {
                            clip_id,
                            effect_index: eff_idx,
                            handle,
                            initial_transform: transform,
                            drag_start_pos: pointer,
                        });
                    }
                }
            } else {
                // Hover cursor feedback
                let handle = self.hit_test_handles(
                    pointer, pos_screen, &corners, rot_handle_pos,
                    center_radius, handle_half, rot_radius, hit_radius,
                );
                if let Some(h) = handle {
                    match h {
                        TransformHandle::Position => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        TransformHandle::Rotation => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        TransformHandle::ScaleTopLeft | TransformHandle::ScaleBottomRight => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                        }
                        TransformHandle::ScaleTopRight | TransformHandle::ScaleBottomLeft => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNeSw);
                        }
                    }
                }
            }
        }
    }

    /// Hit-test transform handles and return which one the pointer is over, if any.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_handles(
        &self,
        pointer: egui::Pos2,
        center: egui::Pos2,
        corners: &[egui::Pos2; 4],
        rot_handle: egui::Pos2,
        center_radius: f32,
        handle_half: f32,
        rot_radius: f32,
        hit_radius: f32,
    ) -> Option<TransformHandle> {
        // Check rotation handle first (smallest, on top)
        if (pointer - rot_handle).length() <= rot_radius + hit_radius {
            return Some(TransformHandle::Rotation);
        }
        // Corner handles (scale)
        let corner_handles = [
            TransformHandle::ScaleTopLeft,
            TransformHandle::ScaleTopRight,
            TransformHandle::ScaleBottomRight,
            TransformHandle::ScaleBottomLeft,
        ];
        for (i, corner) in corners.iter().enumerate() {
            if (pointer - *corner).length() <= handle_half + hit_radius {
                return Some(corner_handles[i]);
            }
        }
        // Center handle (position)
        if (pointer - center).length() <= center_radius + hit_radius {
            return Some(TransformHandle::Position);
        }
        None
    }

    /// Compute the new transform parameters based on the current drag state.
    #[allow(clippy::too_many_arguments)]
    fn compute_dragged_transform(
        &self,
        drag: &ViewerTransformDrag,
        pointer_pos: Option<egui::Pos2>,
        scale_to_screen_x: f32,
        scale_to_screen_y: f32,
        center_screen: egui::Pos2,
        shift_held: bool,
    ) -> TransformParams {
        let pointer = match pointer_pos {
            Some(p) => p,
            None => return drag.initial_transform,
        };

        let initial = &drag.initial_transform;

        // The anchor/pivot point in screen space (based on initial transform)
        let initial_pos_screen = egui::pos2(
            center_screen.x + initial.position_x * scale_to_screen_x,
            center_screen.y + initial.position_y * scale_to_screen_y,
        );

        match drag.handle {
            TransformHandle::Position => {
                let delta_screen_x = pointer.x - drag.drag_start_pos.x;
                let delta_screen_y = pointer.y - drag.drag_start_pos.y;
                let delta_frame_x = delta_screen_x / scale_to_screen_x;
                let delta_frame_y = delta_screen_y / scale_to_screen_y;
                TransformParams {
                    position_x: initial.position_x + delta_frame_x,
                    position_y: initial.position_y + delta_frame_y,
                    ..*initial
                }
            }
            TransformHandle::ScaleTopLeft
            | TransformHandle::ScaleTopRight
            | TransformHandle::ScaleBottomLeft
            | TransformHandle::ScaleBottomRight => {
                if shift_held {
                    // Non-uniform scale: X and Y independently based on drag direction
                    let initial_dx = (drag.drag_start_pos.x - initial_pos_screen.x).abs();
                    let initial_dy = (drag.drag_start_pos.y - initial_pos_screen.y).abs();
                    let current_dx = (pointer.x - initial_pos_screen.x).abs();
                    let current_dy = (pointer.y - initial_pos_screen.y).abs();
                    let scale_x = if initial_dx > 1.0 { current_dx / initial_dx } else { 1.0 };
                    let scale_y = if initial_dy > 1.0 { current_dy / initial_dy } else { 1.0 };
                    TransformParams {
                        scale_x: (initial.scale_x * scale_x).max(0.01),
                        scale_y: (initial.scale_y * scale_y).max(0.01),
                        ..*initial
                    }
                } else {
                    // Uniform scale: distance from anchor point
                    let initial_dist = (drag.drag_start_pos - initial_pos_screen).length().max(1.0);
                    let current_dist = (pointer - initial_pos_screen).length();
                    let scale_factor = current_dist / initial_dist;
                    TransformParams {
                        scale_x: (initial.scale_x * scale_factor).max(0.01),
                        scale_y: (initial.scale_y * scale_factor).max(0.01),
                        ..*initial
                    }
                }
            }
            TransformHandle::Rotation => {
                let initial_angle = (drag.drag_start_pos.y - initial_pos_screen.y)
                    .atan2(drag.drag_start_pos.x - initial_pos_screen.x);
                let current_angle = (pointer.y - initial_pos_screen.y)
                    .atan2(pointer.x - initial_pos_screen.x);
                let delta_degrees = (current_angle - initial_angle).to_degrees();
                TransformParams {
                    rotation: initial.rotation + delta_degrees,
                    ..*initial
                }
            }
        }
    }

    fn show_source_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        // If a skim texture is available, show it as the video frame (letterboxed)
        if let Some(tex) = &state.browser_skim_texture {
            let tex_size = tex.size_vec2();
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = viewer_rect.width() / viewer_rect.height();
                let (dw, dh) = if video_aspect > panel_aspect {
                    (viewer_rect.width(), viewer_rect.width() / video_aspect)
                } else {
                    (viewer_rect.height() * video_aspect, viewer_rect.height())
                };
                let image_rect = egui::Rect::from_center_size(
                    viewer_rect.center(),
                    egui::vec2(dw, dh),
                );
                ui.painter().image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                // Show filename overlay in top-left
                if let Some(media) = state.source_media_id.and_then(|mid| state.media_pool.get(&mid)) {
                    ui.painter().text(
                        egui::pos2(viewer_rect.min.x + 12.0, viewer_rect.min.y + 28.0),
                        egui::Align2::LEFT_TOP,
                        &media.filename,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgba_premultiplied(200, 200, 210, 180),
                    );

                    // Graphical IN/OUT markers
                    Self::draw_source_in_out_markers(ui, &image_rect, media);
                }
                return;
            }
        }

        let media = state.source_media_id
            .and_then(|mid| state.media_pool.get(&mid));

        if let Some(media) = media {
            let cx = viewer_rect.center().x;
            let cy = viewer_rect.center().y;
            let dim = egui::Color32::from_rgb(120, 120, 130);
            let label_col = egui::Color32::from_rgb(180, 180, 190);
            let val_col = egui::Color32::from_rgb(210, 210, 220);

            ui.painter().text(
                egui::pos2(cx, cy - 60.0),
                egui::Align2::CENTER_CENTER,
                &media.filename,
                egui::FontId::proportional(14.0),
                label_col,
            );

            let dur_secs = if media.duration.rate > 0 {
                media.duration.value as f64 / media.duration.rate as f64
            } else { 0.0 };
            let dur_m = (dur_secs / 60.0) as u32;
            let dur_s = dur_secs % 60.0;
            let fps_str = if media.fps.den == 1 {
                format!("{}", media.fps.num)
            } else {
                format!("{:.2}", media.fps.num as f64 / media.fps.den as f64)
            };
            let res_line = format!(
                "{}x{}  {}fps  {}:{:05.2}",
                media.resolution.width, media.resolution.height, fps_str, dur_m, dur_s,
            );
            ui.painter().text(
                egui::pos2(cx, cy - 38.0),
                egui::Align2::CENTER_CENTER,
                &res_line,
                egui::FontId::monospace(12.0),
                val_col,
            );

            let size_mb = media.file_size_bytes as f64 / (1024.0 * 1024.0);
            let size_str = if size_mb >= 1024.0 {
                format!("{:.1} GB", size_mb / 1024.0)
            } else {
                format!("{:.1} MB", size_mb)
            };
            let audio_str = if media.has_audio {
                media.audio_codec.to_lowercase()
            } else {
                "none".to_string()
            };
            let codec_line = format!(
                "{}  \u{2502}  {}  \u{2502}  {}",
                media.video_codec.to_lowercase(), audio_str, size_str,
            );
            ui.painter().text(
                egui::pos2(cx, cy - 20.0),
                egui::Align2::CENTER_CENTER,
                &codec_line,
                egui::FontId::monospace(11.0),
                dim,
            );

            let in_str = media.default_source_in
                .map(|t| t.to_timecode_string())
                .unwrap_or_else(|| "00:00:00:00".to_string());
            let out_str = media.default_source_out
                .map(|t| t.to_timecode_string())
                .unwrap_or_else(|| media.duration.to_timecode_string());
            ui.painter().text(
                egui::pos2(cx, cy + 6.0),
                egui::Align2::CENTER_CENTER,
                format!("In: {}  Out: {}", in_str, out_str),
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(200, 160, 80),
            );

            // Graphical IN/OUT markers on the viewer rect
            Self::draw_source_in_out_markers(ui, &viewer_rect, media);

            // Edit buttons
            let has_in_out = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
            let edit_buttons: &[(&str, AppCommand, bool)] = &[
                ("Insert (,)", AppCommand::InsertEdit, true),
                ("Overwrite (.)", AppCommand::OverwriteEdit, true),
                ("Lift (;)", AppCommand::LiftEdit, has_in_out),
                ("Extract (')", AppCommand::ExtractEdit, has_in_out),
            ];

            let btn_w = 90.0;
            let btn_gap = 6.0;
            let total_w = btn_w * 4.0 + btn_gap * 3.0;
            let btn_y = cy + 34.0;
            let btn_x_start = cx - total_w / 2.0;

            for (i, (label, cmd, enabled)) in edit_buttons.iter().enumerate() {
                let bx = btn_x_start + (btn_w + btn_gap) * i as f32;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(bx, btn_y),
                    egui::vec2(btn_w, 24.0),
                );
                let resp = ui.allocate_rect(rect, if *enabled { egui::Sense::click() } else { egui::Sense::hover() });
                let fill = if !enabled {
                    egui::Color32::from_rgb(30, 30, 35)
                } else if resp.hovered() {
                    egui::Color32::from_rgb(60, 60, 70)
                } else {
                    egui::Color32::from_rgb(45, 45, 55)
                };
                let text_color = if *enabled {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(70, 70, 80)
                };
                ui.painter().rect_filled(rect, 3.0, fill);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    *label,
                    egui::FontId::proportional(10.0),
                    text_color,
                );
                if *enabled && resp.clicked() {
                    cmds.push(cmd.clone());
                }
            }
        } else {
            ui.painter().text(
                viewer_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Click a media item to load it here",
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(80, 80, 90),
            );
        }
    }

    /// Draw graphical IN/OUT markers on the source viewer.
    fn draw_source_in_out_markers(
        ui: &mut egui::Ui,
        image_rect: &egui::Rect,
        media: &klipp_media::MediaItem,
    ) {
        let amber = egui::Color32::from_rgb(232, 160, 32);
        let bar_h = 4.0_f32;
        let marker_w = 3.0_f32;
        let label_font = egui::FontId::monospace(8.0);

        let has_in = media.default_source_in.is_some();
        let has_out = media.default_source_out.is_some();

        // IN marker — amber vertical bar on left edge
        if has_in {
            let in_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.left(), image_rect.top()),
                egui::vec2(marker_w, image_rect.height()),
            );
            ui.painter().rect_filled(in_rect, 0.0, amber);
            // Small downward triangle at bottom
            let tri_size = 5.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(image_rect.left() + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size),
                    egui::pos2(image_rect.left() - tri_size + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                    egui::pos2(image_rect.left() + tri_size + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                ],
                amber, egui::Stroke::NONE,
            ));
            // "IN" label
            ui.painter().text(
                egui::pos2(image_rect.left() + marker_w + 4.0, image_rect.bottom() - bar_h - 10.0),
                egui::Align2::LEFT_CENTER,
                "IN",
                label_font.clone(),
                amber,
            );
        }

        // OUT marker — amber vertical bar on right edge
        if has_out {
            let out_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.right() - marker_w, image_rect.top()),
                egui::vec2(marker_w, image_rect.height()),
            );
            ui.painter().rect_filled(out_rect, 0.0, amber);
            // Small downward triangle at bottom
            let tri_size = 5.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(image_rect.right() - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size),
                    egui::pos2(image_rect.right() - tri_size - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                    egui::pos2(image_rect.right() + tri_size - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                ],
                amber, egui::Stroke::NONE,
            ));
            // "OUT" label
            ui.painter().text(
                egui::pos2(image_rect.right() - marker_w - 4.0, image_rect.bottom() - bar_h - 10.0),
                egui::Align2::RIGHT_CENTER,
                "OUT",
                label_font,
                amber,
            );
        }

        // Progress bar between IN and OUT at bottom of viewer
        if has_in && has_out {
            let fill_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.left(), image_rect.bottom() - bar_h),
                egui::vec2(image_rect.width(), bar_h),
            );
            ui.painter().rect_filled(
                fill_rect, 0.0,
                egui::Color32::from_rgba_unmultiplied(232, 160, 32, 60),
            );
        }
    }
}

/// Rotate a point around a center by the given angle in radians.
fn rotate_point(point: egui::Pos2, center: egui::Pos2, angle_rad: f32) -> egui::Pos2 {
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let cos = angle_rad.cos();
    let sin = angle_rad.sin();
    egui::pos2(
        center.x + dx * cos - dy * sin,
        center.y + dx * sin + dy * cos,
    )
}

fn format_timecode_hmsf(smooth_secs: f64, rate: u32) -> String {
    let s = smooth_secs.abs();
    let sign = if smooth_secs < 0.0 { "-" } else { "" };
    let hh = (s / 3600.0) as u32;
    let mm = ((s % 3600.0) / 60.0) as u32;
    let ss = (s % 60.0) as u32;
    let ff = ((s % 1.0) * rate as f64) as u32;
    format!("{sign}{hh:02}:{mm:02}:{ss:02}:{ff:02}")
}

fn parse_timecode_input(input: &str, fps: Fps) -> Option<RationalTime> {
    let input = input.trim();
    if let Some(tc) = parse_hmsf(input, fps.rate()) { return Some(tc); }
    if let Ok(f) = input.parse::<i64>() { return Some(RationalTime::from_frames(f, fps.rate())); }
    if let Ok(s) = input.parse::<f64>() { return Some(RationalTime::from_secs_f64(s, fps.rate())); }
    None
}

fn parse_hmsf(input: &str, rate: u32) -> Option<RationalTime> {
    let p: Vec<&str> = input.split(':').collect();
    if p.len() != 4 { return None; }
    let hh: i64 = p[0].parse().ok()?;
    let mm: i64 = p[1].parse().ok()?;
    let ss: i64 = p[2].parse().ok()?;
    let ff: i64 = p[3].parse().ok()?;
    Some(RationalTime::from_frames(((hh * 3600 + mm * 60 + ss) * rate as i64) + ff, rate))
}
