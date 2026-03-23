use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ViewerMode};
use klipp_engine::PlaybackState;

#[derive(Default)]
pub struct ViewerPanel {
    pub video_texture: Option<egui::TextureHandle>,
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

        // Scrub bar height
        let scrub_h = 3.0;
        let viewer_rect = egui::Rect::from_min_max(
            panel_rect.min,
            egui::pos2(panel_rect.max.x, panel_rect.max.y - scrub_h),
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

        // ── Scrub bar (bottom 3px) ──
        let scrub_rect = egui::Rect::from_min_size(
            egui::pos2(panel_rect.min.x, panel_rect.max.y - scrub_h),
            egui::vec2(panel_rect.width(), scrub_h),
        );
        self.draw_scrub_bar(ui, state, theme, scrub_rect, &mut cmds);

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

        let current_media = state.sequence().tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(state.playhead))
            .and_then(|(_, clip)| clip.media_id().and_then(|mid| state.media_pool.get(&mid)));

        let has_content = state.playback != PlaybackState::Idle || current_media.is_some();
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

        let (video_w, video_h) = current_media
            .or_else(|| state.media_pool.all().next())
            .map(|m| (m.resolution.width as f32, m.resolution.height as f32))
            .unwrap_or((1920.0, 1080.0));

        if video_w == 0.0 || video_h == 0.0 { return; }

        let video_aspect = video_w / video_h;
        let panel_aspect = viewer_rect.width() / viewer_rect.height();
        let (display_w, display_h) = if video_aspect > panel_aspect {
            (viewer_rect.width(), viewer_rect.width() / video_aspect)
        } else {
            (viewer_rect.height() * video_aspect, viewer_rect.height())
        };

        let image_rect = egui::Rect::from_center_size(
            viewer_rect.center(),
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

        // ── White balance eyedropper interaction ──
        if state.wb_eyedropper_active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            // Allocate click sense over the image area
            let eyedropper_resp = ui.allocate_rect(image_rect, egui::Sense::click());
            if eyedropper_resp.clicked() {
                if let Some(pos) = eyedropper_resp.interact_pointer_pos() {
                    // Normalized coordinates (0-1) within the video frame
                    let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    state.wb_sample_request = Some((nx, ny));
                    state.wb_eyedropper_active = false;
                }
            }
            // Cancel on Escape
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.wb_eyedropper_active = false;
            }
        }

        // ── Mask outlines on viewer ──
        if let Some(clip_id) = state.selected_clip {
            let masks = state.sequences.get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id))
                .map(|c| &c.color_grade.masks);

            if let Some(masks) = masks {
                let mask_stroke = egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(0xe8, 0xa0, 0x20, 160));
                let aspect = image_rect.height() / image_rect.width();

                for mask in &masks.masks {
                    if !mask.enabled { continue; }
                    match &mask.shape {
                        klipp_core::MaskShape::Circle { center_x, center_y, radius } => {
                            let cx = image_rect.min.x + center_x * image_rect.width();
                            let cy = image_rect.min.y + center_y * image_rect.height();
                            let r = radius * image_rect.width();
                            ui.painter().circle_stroke(egui::pos2(cx, cy), r, mask_stroke);
                        }
                        klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
                            let cx = image_rect.min.x + center_x * image_rect.width();
                            let cy = image_rect.min.y + center_y * image_rect.height();
                            let hw = width / 2.0 * image_rect.width();
                            let hh = height / 2.0 * image_rect.width(); // use width for aspect
                            let rad = rotation.to_radians();
                            let cos_r = rad.cos();
                            let sin_r = rad.sin();
                            // Four corners rotated
                            let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
                            let points: Vec<egui::Pos2> = corners.iter().map(|&(lx, ly)| {
                                egui::pos2(
                                    cx + lx * cos_r - ly * sin_r,
                                    cy + lx * sin_r + ly * cos_r,
                                )
                            }).collect();
                            for i in 0..4 {
                                ui.painter().line_segment(
                                    [points[i], points[(i + 1) % 4]],
                                    mask_stroke,
                                );
                            }
                            // Center dot
                            ui.painter().circle_filled(egui::pos2(cx, cy), 3.0, mask_stroke.color);
                        }
                        klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                            let sx = image_rect.min.x + start_x * image_rect.width();
                            let sy = image_rect.min.y + start_y * image_rect.height();
                            let ex = image_rect.min.x + end_x * image_rect.width();
                            let ey = image_rect.min.y + end_y * image_rect.height();
                            // Draw gradient line with dots at start/end
                            ui.painter().line_segment(
                                [egui::pos2(sx, sy), egui::pos2(ex, ey)],
                                mask_stroke,
                            );
                            ui.painter().circle_filled(egui::pos2(sx, sy), 4.0, mask_stroke.color);
                            ui.painter().circle_stroke(egui::pos2(ex, ey), 4.0, mask_stroke);
                        }
                    }
                }
                let _ = aspect; // suppress unused warning
            }
        }

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

        // ── Viewer overlay (bottom, above letterbox) ──
        let overlay_y = viewer_rect.max.y - lb_h;
        // Subtle gradient background
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(viewer_rect.min.x, overlay_y - 24.0),
                egui::vec2(viewer_rect.width(), 24.0),
            ),
            0.0,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 80),
        );

        // Timecode (left)
        let tc_str = {
            let s = state.playhead_smooth_secs;
            let hh = (s / 3600.0) as u32;
            let mm = ((s % 3600.0) / 60.0) as u32;
            let ss = (s % 60.0) as u32;
            let ff = ((s % 1.0) * state.fps.rate() as f64) as u32;
            format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
        };
        ui.painter().text(
            egui::pos2(viewer_rect.min.x + 12.0, overlay_y - 12.0),
            egui::Align2::LEFT_CENTER,
            &tc_str,
            egui::FontId::monospace(11.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 140),
        );

        // Zoom level (right)
        ui.painter().text(
            egui::pos2(viewer_rect.max.x - 12.0, overlay_y - 12.0),
            egui::Align2::RIGHT_CENTER,
            "100%",
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 89),
        );

        // Proxy badge
        if state.use_proxies && current_media.is_some_and(|m| m.proxy_ready) {
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
}
