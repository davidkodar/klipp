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
    pub logo_texture: Option<egui::TextureHandle>,
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
            state.fullscreen_overlay_timer = state.viewer_prefs.fullscreen_overlay_timeout_secs as f64;
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
            // Empty state: Klipp logo at 40% opacity
            let center = viewer_rect.center();
            let logo_tex = self.logo_texture.get_or_insert_with(|| {
                let png_bytes = include_bytes!("../../../../assets/icons/klipp_app_icons/png/dark/klipp-dark-128.png");
                let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes.as_slice()));
                let mut reader = decoder.read_info().expect("viewer logo PNG decode");
                let mut buf = vec![0u8; reader.output_buffer_size()];
                let info = reader.next_frame(&mut buf).expect("viewer logo PNG frame");
                let bytes = &buf[..info.buffer_size()];
                let pixels: Vec<egui::Color32> = match info.color_type {
                    png::ColorType::Rgba => bytes
                        .chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                        .collect(),
                    png::ColorType::Rgb => bytes
                        .chunks_exact(3)
                        .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
                        .collect(),
                    _ => panic!("Unsupported viewer logo PNG color type"),
                };
                let image = egui::ColorImage {
                    size: [info.width as usize, info.height as usize],
                    pixels,
                };
                ui.ctx().load_texture("klipp_viewer_logo", image, egui::TextureOptions::LINEAR)
            });
            let logo_size = 48.0;
            let logo_rect = egui::Rect::from_center_size(
                egui::pos2(center.x, center.y - 8.0),
                egui::vec2(logo_size, logo_size),
            );
            ui.painter().image(
                logo_tex.id(),
                logo_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::from_white_alpha(102),
            );
            let logo_color = egui::Color32::from_rgba_premultiplied(232, 160, 32, 102);
            ui.painter().text(
                egui::pos2(center.x, center.y + logo_size * 0.5),
                egui::Align2::CENTER_CENTER,
                "Klipp",
                egui::FontId::proportional(16.0),
                logo_color,
            );
            return;
        }

        // Use the actual video texture dimensions for aspect ratio — this matches what's
        // actually being displayed. The engine may render at sequence resolution (e.g. 1920×1080)
        // even if the source clip is 1920×818. Using the texture size ensures WYSIWYG.
        let tex_dims = self.video_texture.as_ref()
            .map(|t| t.size_vec2())
            .filter(|s| s.x > 0.0 && s.y > 0.0);
        let (video_w, video_h) = tex_dims
            .map(|s| (s.x, s.y))
            .or_else(|| current_media_info.map(|(w, h, _)| (w as f32, h as f32)))
            .or_else(|| state.media_pool.all().next().map(|m| (m.resolution.width as f32, m.resolution.height as f32)))
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

        // Draw title overlay (separate texture, alpha-blended on top of video)
        // Show it even during text edit mode so style changes are visible in real-time
        {
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
                            state.viewer_char_positions.clear(); // trigger recompute
                            state.viewer_text_cursor = 0;
                            state.viewer_text_select_anchor = None;
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
                state.viewer_text_selection = None;
                state.viewer_text_selection_locked = None;
                state.viewer_text_cursor = 0;
                state.viewer_char_positions.clear();
                state.viewer_text_select_anchor = None;
            }
        }

        // ── Text editor: visible egui TextEdit overlaid on the viewer ──
        // Uses egui's native TextEdit for ALL text operations (typing, cursor,
        // selection, copy/paste, undo) — no custom event handling needed.
        // The styled title overlay renders behind the TextEdit as a live preview.
        if state.viewer_text_edit {
            let edit_id = ui.id().with("viewer_text_edit");
            let prev_buf = state.viewer_text_edit_buffer.clone();

            // Get title font size for display scaling
            let title_font_px = state.viewer_text_edit_clip
                .and_then(|cid| state.sequence().tracks.iter()
                    .find_map(|t| t.find_clip(cid))
                    .and_then(|c| c.title_data())
                    .map(|td| td.font_size))
                .unwrap_or(72.0);

            // Scale the editor font to approximate the title size in the viewer.
            // Title renders at font_size * (height/1080) pixels in the frame,
            // then the frame is scaled to fit image_rect.
            let frame_scale = image_rect.height() / video_h;
            let editor_font_px = (title_font_px * (video_h / 1080.0) * frame_scale).clamp(10.0, 200.0);

            // ── Dark scrim behind the TextEdit ──
            let fg_painter = ui.painter_at(image_rect).with_layer_id(
                egui::LayerId::new(egui::Order::Foreground, ui.id().with("text_edit_overlay")),
            );
            fg_painter.rect_filled(image_rect, 0.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 100));

            // Accent border
            fg_painter.rect_stroke(image_rect.shrink(1.0), 0.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(56, 132, 244, 180)));

            // Edit mode badge
            let badge_galley = ui.painter().layout_no_wrap(
                "Text Editing \u{2014} Esc to exit".to_string(),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgba_unmultiplied(200, 220, 255, 220),
            );
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.left() + 1.0, image_rect.top() + 1.0),
                egui::vec2(badge_galley.rect.width() + 16.0, badge_galley.rect.height() + 6.0),
            );
            fg_painter.rect_filled(badge_rect, 0.0,
                egui::Color32::from_rgba_unmultiplied(30, 60, 120, 180));
            fg_painter.galley(
                egui::pos2(badge_rect.left() + 8.0, badge_rect.top() + 3.0),
                badge_galley, egui::Color32::WHITE,
            );

            // ── Visible TextEdit centered on the viewer ──
            // Inset from image edges for comfortable editing
            let edit_margin = 20.0;
            let te_rect = image_rect.shrink2(egui::vec2(edit_margin, edit_margin + 20.0));

            let mut te_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(te_rect),
            );
            te_ui.style_mut().visuals.extreme_bg_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 40);
            te_ui.style_mut().visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(50, 110, 230, 160);

            let te_output = egui::TextEdit::multiline(&mut state.viewer_text_edit_buffer)
                .id(edit_id)
                .desired_width(te_rect.width())
                .desired_rows(1)
                .frame(false)
                .margin(egui::Margin::symmetric(8.0, 8.0))
                .text_color(egui::Color32::from_rgb(230, 230, 240))
                .font(egui::FontId::proportional(editor_font_px))
                .horizontal_align(egui::Align::Center)
                .show(&mut te_ui);

            // Request focus on first frame
            if state.viewer_text_edit_request_focus {
                state.viewer_text_edit_request_focus = false;
                te_output.response.request_focus();
            }

            // Sync cursor/selection from egui's TextEdit to our state (for inspector)
            if let Some(cursor_range) = te_output.cursor_range {
                let primary = cursor_range.primary.ccursor.index;
                let secondary = cursor_range.secondary.ccursor.index;
                state.viewer_text_cursor = primary;
                if primary != secondary {
                    let (s, e) = if primary < secondary { (primary, secondary) } else { (secondary, primary) };
                    state.viewer_text_selection = Some((s, e));
                    state.viewer_text_selection_locked = Some((s, e));
                } else {
                    // Don't clear locked selection if pointer is down (inspector interactions)
                    if !ui.input(|i| i.pointer.primary_down()) {
                        state.viewer_text_selection = None;
                        // Keep locked selection for inspector — only clear on explicit deselect
                    }
                }
            }

            // Detect text changes
            let text_changed = state.viewer_text_edit_buffer != prev_buf;

            // ── Resolve font size + color at cursor position for inspector ──
            if let Some(clip_id) = state.viewer_text_edit_clip {
                let style_info: Option<(Vec<klipp_timeline::StyleRun>, f32, [u8; 4])> = state.sequence().tracks.iter()
                    .find_map(|t| t.find_clip(clip_id))
                    .and_then(|c| c.title_data())
                    .map(|td| (td.style_runs.clone(), td.font_size, td.color));
                if let Some((runs, base_size, base_color)) = style_info {
                    let cursor_byte = char_to_byte(&state.viewer_text_edit_buffer, state.viewer_text_cursor);
                    let check_byte = if cursor_byte > 0 { cursor_byte - 1 } else { cursor_byte };
                    let mut found_size = None;
                    let mut found_color = None;
                    for run in &runs {
                        if check_byte >= run.start && check_byte < run.end {
                            found_size = run.font_size;
                            found_color = run.color;
                            break;
                        }
                    }
                    state.viewer_text_cursor_font_size = Some(found_size.unwrap_or(base_size));
                    state.viewer_text_cursor_color = Some(found_color.unwrap_or(base_color));
                }
            }

            // Emit UpdateTitleData when text changes
            if text_changed {
                state.viewer_char_positions.clear();
                if let Some(clip_id) = state.viewer_text_edit_clip {
                    let update_text = state.viewer_text_edit_buffer.clone();
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
                            text: update_text,
                            font_size: td.font_size,
                            font_name: td.font_name.clone(),
                            font_style,
                            color: td.color,
                        });
                    }
                }
            }

            // Exit on Escape or when TextEdit loses focus due to click outside
            let exit_escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let click_outside_in_viewer = ui.input(|i| i.pointer.primary_clicked())
                && ui.input(|i| i.pointer.interact_pos())
                    .is_some_and(|pos| !image_rect.contains(pos) && viewer_rect.contains(pos));
            if exit_escape || click_outside_in_viewer {
                state.viewer_text_edit = false;
                state.viewer_text_edit_clip = None;
                state.viewer_text_edit_buffer.clear();
                state.viewer_text_selection = None;
                state.viewer_text_selection_locked = None;
                state.viewer_text_cursor = 0;
                state.viewer_char_positions.clear();
                state.viewer_text_select_anchor = None;
            }
        }

        // ── On-screen transform controls ──
        // Suppress transform controls during text edit mode
        if state.show_transform_controls && !state.viewer_text_edit && state.eyedropper_mode == crate::state::EyedropperMode::None && !state.has_modal_open() {
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
                ViewerOverlay::SafeAreas => Some("SAFE AREAS"),
                ViewerOverlay::Grid => Some("GRID"),
                ViewerOverlay::Checkerboard => None, // no badge for checkerboard
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

        // Safe area overlay: title safe (80%) and action safe (90%)
        if matches!(state.viewer_overlay, crate::state::ViewerOverlay::SafeAreas) {
            let center = image_rect.center();
            let w = image_rect.width();
            let h = image_rect.height();
            // 90% action safe
            let action_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.action_safe_pct, h * state.viewer_prefs.action_safe_pct));
            ui.painter().rect_stroke(action_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 200, 0, 120)));
            ui.painter().text(
                egui::pos2(action_rect.left() + 4.0, action_rect.top() + 2.0),
                egui::Align2::LEFT_TOP, "Action Safe",
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgba_premultiplied(255, 200, 0, 140),
            );
            // 80% title safe
            let title_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.title_safe_pct, h * state.viewer_prefs.title_safe_pct));
            ui.painter().rect_stroke(title_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(0, 180, 255, 120)));
            ui.painter().text(
                egui::pos2(title_rect.left() + 4.0, title_rect.top() + 2.0),
                egui::Align2::LEFT_TOP, "Title Safe",
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgba_premultiplied(0, 180, 255, 140),
            );
        }

        // Grid overlay: rule of thirds + center crosshair
        if matches!(state.viewer_overlay, crate::state::ViewerOverlay::Grid) {
            let w = image_rect.width();
            let h = image_rect.height();
            let gc = state.viewer_prefs.grid_color;
            let stroke = egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], gc[3]));
            // Rule of thirds: vertical lines
            for i in 1..=2 {
                let x = image_rect.left() + w * i as f32 / 3.0;
                ui.painter().line_segment([egui::pos2(x, image_rect.top()), egui::pos2(x, image_rect.bottom())], stroke);
            }
            // Rule of thirds: horizontal lines
            for i in 1..=2 {
                let y = image_rect.top() + h * i as f32 / 3.0;
                ui.painter().line_segment([egui::pos2(image_rect.left(), y), egui::pos2(image_rect.right(), y)], stroke);
            }
            // Center crosshair
            let cx = image_rect.center().x;
            let cy = image_rect.center().y;
            let cross = egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 100, 100, 100));
            ui.painter().line_segment([egui::pos2(cx - 12.0, cy), egui::pos2(cx + 12.0, cy)], cross);
            ui.painter().line_segment([egui::pos2(cx, cy - 12.0), egui::pos2(cx, cy + 12.0)], cross);
        }

        // Checkerboard transparency overlay (drawn behind video, replaces black background)
        if matches!(state.viewer_overlay, crate::state::ViewerOverlay::Checkerboard) {
            let tile: f32 = state.viewer_prefs.checkerboard_tile_size;
            let c1 = egui::Color32::from_rgb(80, 80, 80);
            let c2 = egui::Color32::from_rgb(120, 120, 120);
            let x0 = image_rect.left();
            let y0 = image_rect.top();
            let mut y = y0;
            let mut row = 0;
            while y < image_rect.bottom() {
                let mut x = x0;
                let mut col = 0;
                while x < image_rect.right() {
                    let color = if (row + col) % 2 == 0 { c1 } else { c2 };
                    let r = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(tile.min(image_rect.right() - x), tile.min(image_rect.bottom() - y)),
                    );
                    ui.painter().rect_filled(r, 0.0, color);
                    x += tile;
                    col += 1;
                }
                y += tile;
                row += 1;
            }
        }

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

        // Zoom dropdown
        {
            let zoom_label = if state.viewer_zoom <= 0.0 {
                "Fit".to_string()
            } else {
                format!("{}%", (state.viewer_zoom * 100.0) as u32)
            };
            let zoom_rect = egui::Rect::from_min_size(
                egui::pos2(rx - 42.0, cy - 8.0),
                egui::vec2(44.0, 16.0),
            );
            let zoom_resp = ui.allocate_rect(zoom_rect, egui::Sense::click());
            ui.painter().text(
                egui::pos2(rx, cy),
                egui::Align2::RIGHT_CENTER,
                &zoom_label,
                egui::FontId::monospace(11.0),
                if zoom_resp.hovered() {
                    egui::Color32::from_rgb(200, 200, 220)
                } else {
                    egui::Color32::from_rgb(120, 120, 130)
                },
            );
            zoom_resp.context_menu(|ui| {
                for &(label, value) in &[("Fit", 0.0), ("25%", 0.25), ("50%", 0.5), ("100%", 1.0), ("200%", 2.0)] {
                    if ui.button(label).clicked() {
                        state.viewer_zoom = value;
                        ui.close_menu();
                    }
                }
            });
            if zoom_resp.clicked() {
                // Toggle between fit and 100%
                state.viewer_zoom = if state.viewer_zoom <= 0.0 { 1.0 } else { 0.0 };
            }
        }

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
                                    MaskHandle::BezierPoint(_) => egui::CursorIcon::Move,
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
                                    MaskHandle::BezierPoint(_) => egui::CursorIcon::Move,
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
                // Bezier freeform path
                klipp_core::MaskShape::Bezier { points } => {
                    if points.len() >= 2 {
                        let screen_pts: Vec<egui::Pos2> = points.iter().map(|(px, py)| {
                            egui::pos2(
                                image_rect.left() + px * image_rect.width(),
                                image_rect.top() + py * image_rect.height(),
                            )
                        }).collect();
                        // Draw path edges
                        for i in 0..screen_pts.len() {
                            let a = screen_pts[i];
                            let b = screen_pts[(i + 1) % screen_pts.len()];
                            ui.painter().line_segment([a, b], egui::Stroke::new(1.5, outline_color));
                        }
                        // Draw control point handles
                        if state.viewer_mask_drag.is_none() {
                            for (pi, sp) in screen_pts.iter().enumerate() {
                                let handle_rect = egui::Rect::from_center_size(*sp, egui::vec2(12.0, 12.0));
                                let resp = ui.allocate_rect(handle_rect, egui::Sense::drag());
                                let color = if resp.hovered() || resp.dragged() {
                                    egui::Color32::WHITE
                                } else {
                                    outline_color
                                };
                                ui.painter().circle_filled(*sp, 4.0, color);
                                if resp.drag_started() {
                                    if let Some(pos) = resp.interact_pointer_pos() {
                                        state.viewer_mask_drag = Some(ViewerMaskDrag {
                                            clip_id,
                                            node_id,
                                            mask_index: mask_idx,
                                            handle: MaskHandle::BezierPoint(pi),
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
                    (klipp_core::MaskShape::Bezier { points }, MaskHandle::BezierPoint(idx)) => {
                        let mut new_points = points.clone();
                        if idx < new_points.len() {
                            new_points[idx] = (
                                ((current_pos.x - image_rect.min.x) / iw).clamp(0.0, 1.0),
                                ((current_pos.y - image_rect.min.y) / ih).clamp(0.0, 1.0),
                            );
                        }
                        klipp_core::MaskShape::Bezier { points: new_points }
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
        // Use a foreground-layer painter so transform handles draw ON TOP of
        // everything (toolbar, transport bar, etc.) and are never covered.
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            ui.id().with("transform_controls"),
        ));

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

// ── WYSIWYG text edit helpers ──

/// Convert a character index to a byte offset in the string.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(bi, _)| bi)
        .unwrap_or(s.len())
}

#[allow(dead_code)] // Used in tests; may be re-enabled for viewer click mapping
fn find_nearest_char_index(positions: &[klipp_title::CharPosition], fx: f32, fy: f32) -> usize {
    if positions.is_empty() {
        return 0;
    }

    // Group positions by line (characters with similar y values)
    // Find the line closest to fy first, then find the nearest char on that line.
    let mut best_line_y = positions[0].y;
    let mut best_line_dist = (fy - positions[0].y - positions[0].height * 0.5).abs();
    for cp in positions {
        let cy = cp.y + cp.height * 0.5;
        let dist = (fy - cy).abs();
        if dist < best_line_dist {
            best_line_dist = dist;
            best_line_y = cp.y;
        }
    }

    // Among chars on the best line (only those with advance > 0, skip end-of-text marker)
    let line_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - best_line_y).abs() < cp.height * 0.5 && cp.advance > 0.0)
        .collect();

    if line_chars.is_empty() {
        // Only end-of-text positions on this line — return the first char_index on the line
        return positions.iter()
            .find(|cp| (cp.y - best_line_y).abs() < cp.height * 0.5)
            .map(|cp| cp.char_index)
            .unwrap_or(0);
    }

    // Check if click is before the first char
    if fx <= line_chars[0].x {
        return line_chars[0].char_index;
    }

    // Check each character: if fx is in the left half, place cursor before; right half, after
    for cp in &line_chars {
        let mid = cp.x + cp.advance * 0.5;
        if fx >= cp.x && fx < cp.x + cp.advance {
            return if fx < mid { cp.char_index } else { cp.char_index + 1 };
        }
    }

    // Click is past the last visible char on this line — place cursor after it
    if let Some(last) = line_chars.last() {
        last.char_index + 1
    } else {
        0
    }
}

#[allow(dead_code)] // Used in tests; may be re-enabled for vertical cursor nav
/// Move cursor up or down by `direction` lines (-1 = up, +1 = down).
fn move_cursor_vertical(
    positions: &[klipp_title::CharPosition],
    cursor: usize,
    direction: i32,
) -> usize {
    if positions.is_empty() {
        return cursor;
    }

    // Find the current cursor position
    let cur_cp = positions.iter().find(|cp| cp.char_index == cursor);
    let (cur_x, cur_y) = match cur_cp {
        Some(cp) => (cp.x, cp.y),
        None => {
            // Cursor at end — use last position
            if let Some(last) = positions.last() {
                (last.x + last.advance, last.y)
            } else {
                return cursor;
            }
        }
    };

    // Build a sorted list of unique line y-values
    let mut line_ys: Vec<f32> = Vec::new();
    for cp in positions {
        if line_ys.is_empty() || (cp.y - line_ys.last().unwrap()).abs() > 1.0 {
            if !line_ys.contains(&cp.y) {
                line_ys.push(cp.y);
            }
        }
    }
    line_ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    line_ys.dedup_by(|a, b| (*a - *b).abs() < 1.0);

    // Find current line index
    let cur_line_idx = line_ys.iter().position(|&y| (y - cur_y).abs() < 1.0).unwrap_or(0);

    // Target line
    let target_line_idx = if direction < 0 {
        if cur_line_idx == 0 { return 0; } // Already on first line, go to start
        cur_line_idx - 1
    } else {
        if cur_line_idx >= line_ys.len() - 1 {
            // Already on last line, go to end
            return positions.last().map(|p| p.char_index).unwrap_or(cursor);
        }
        cur_line_idx + 1
    };

    let target_y = line_ys[target_line_idx];

    // Find the char on the target line closest to cur_x
    let target_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - target_y).abs() < 1.0)
        .collect();

    if target_chars.is_empty() {
        return cursor;
    }

    // Find nearest by x
    target_chars.iter()
        .min_by_key(|cp| ((cp.x - cur_x).abs() * 100.0) as i64)
        .map(|cp| cp.char_index)
        .unwrap_or(cursor)
}

#[allow(dead_code)]
fn move_cursor_to_line_edge(
    positions: &[klipp_title::CharPosition],
    cursor: usize,
    to_end: bool,
) -> usize {
    if positions.is_empty() {
        return cursor;
    }

    // Find current line y
    let cur_y = positions.iter()
        .find(|cp| cp.char_index == cursor)
        .or_else(|| positions.last())
        .map(|cp| cp.y)
        .unwrap_or(0.0);

    // Get chars on this line
    let line_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - cur_y).abs() < 1.0)
        .collect();

    if line_chars.is_empty() {
        return cursor;
    }

    if to_end {
        line_chars.last().map(|cp| cp.char_index).unwrap_or(cursor)
    } else {
        line_chars.first().map(|cp| cp.char_index).unwrap_or(cursor)
    }
}

#[allow(dead_code)]
fn update_selection_from_anchor(state: &mut AppState) {
    if let Some(anchor) = state.viewer_text_select_anchor {
        let cursor = state.viewer_text_cursor;
        if anchor != cursor {
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            state.viewer_text_selection = Some((start, end));
            state.viewer_text_selection_locked = Some((start, end));
        } else {
            state.viewer_text_selection = None;
            state.viewer_text_selection_locked = None;
        }
    }
}

#[cfg(test)]
mod text_editor_tests {
    use super::*;

    // ── char_to_byte tests ──

    #[test]
    fn char_to_byte_ascii() {
        assert_eq!(char_to_byte("Hello", 0), 0);
        assert_eq!(char_to_byte("Hello", 1), 1);
        assert_eq!(char_to_byte("Hello", 4), 4);
        assert_eq!(char_to_byte("Hello", 5), 5); // past end
    }

    #[test]
    fn char_to_byte_unicode() {
        let s = "Héllo"; // é is 2 bytes
        assert_eq!(char_to_byte(s, 0), 0); // H
        assert_eq!(char_to_byte(s, 1), 1); // é
        assert_eq!(char_to_byte(s, 2), 3); // l (byte 3, not 2!)
        assert_eq!(char_to_byte(s, 3), 4); // l
        assert_eq!(char_to_byte(s, 4), 5); // o
        assert_eq!(char_to_byte(s, 5), 6); // past end
    }

    #[test]
    fn char_to_byte_past_end() {
        assert_eq!(char_to_byte("Hi", 99), 2); // returns s.len()
        assert_eq!(char_to_byte("", 0), 0);
    }

    // ── Text insertion simulation ──
    // This simulates exactly what the editor does when typing

    fn sim_insert(text: &mut String, cursor: &mut usize, ch: &str) {
        let bp = char_to_byte(text, *cursor);
        text.insert_str(bp, ch);
        *cursor += ch.chars().count();
    }

    fn sim_backspace(text: &mut String, cursor: &mut usize) {
        if *cursor > 0 {
            *cursor -= 1;
            let bp = char_to_byte(text, *cursor);
            if let Some(ch) = text[bp..].chars().next() {
                text.replace_range(bp..bp + ch.len_utf8(), "");
            }
        }
    }

    fn sim_arrow_left(cursor: &mut usize) {
        if *cursor > 0 { *cursor -= 1; }
    }

    fn sim_arrow_right(text: &str, cursor: &mut usize) {
        let cc = text.chars().count();
        if *cursor < cc { *cursor += 1; }
    }

    #[test]
    fn test_type_at_end() {
        let mut text = "Hello".to_string();
        let mut cursor = 5; // end of text
        sim_insert(&mut text, &mut cursor, " ");
        sim_insert(&mut text, &mut cursor, "W");
        sim_insert(&mut text, &mut cursor, "o");
        sim_insert(&mut text, &mut cursor, "r");
        sim_insert(&mut text, &mut cursor, "l");
        sim_insert(&mut text, &mut cursor, "d");
        assert_eq!(text, "Hello World");
        assert_eq!(cursor, 11);
    }

    #[test]
    fn test_type_at_beginning() {
        let mut text = "World".to_string();
        let mut cursor = 0; // beginning
        sim_insert(&mut text, &mut cursor, "H");
        assert_eq!(text, "HWorld");
        assert_eq!(cursor, 1);
        sim_insert(&mut text, &mut cursor, "i");
        assert_eq!(text, "HiWorld");
        assert_eq!(cursor, 2);
        sim_insert(&mut text, &mut cursor, " ");
        assert_eq!(text, "Hi World");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn test_type_in_middle() {
        let mut text = "Hllo".to_string();
        let mut cursor = 1; // after H
        sim_insert(&mut text, &mut cursor, "e");
        assert_eq!(text, "Hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn test_navigate_and_type() {
        let mut text = "Hello World".to_string();
        let mut cursor = 11; // end
        // Navigate left 6 times to position after "Hello"
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);
        // Insert " Beautiful"
        sim_insert(&mut text, &mut cursor, " ");
        sim_insert(&mut text, &mut cursor, "B");
        sim_insert(&mut text, &mut cursor, "e");
        sim_insert(&mut text, &mut cursor, "a");
        sim_insert(&mut text, &mut cursor, "u");
        sim_insert(&mut text, &mut cursor, "t");
        sim_insert(&mut text, &mut cursor, "i");
        sim_insert(&mut text, &mut cursor, "f");
        sim_insert(&mut text, &mut cursor, "u");
        sim_insert(&mut text, &mut cursor, "l");
        assert_eq!(text, "Hello Beautiful World");
        assert_eq!(cursor, 15);
    }

    #[test]
    fn test_backspace() {
        let mut text = "Hello".to_string();
        let mut cursor = 5;
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hell");
        assert_eq!(cursor, 4);
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hel");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn test_backspace_at_beginning() {
        let mut text = "Hello".to_string();
        let mut cursor = 0;
        sim_backspace(&mut text, &mut cursor); // should do nothing
        assert_eq!(text, "Hello");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn test_navigate_backspace_type() {
        let mut text = "Hello World".to_string();
        let mut cursor = 11;
        // Go back to position 5 (before " World")
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);
        // Backspace: delete 'o'
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hell World");
        assert_eq!(cursor, 4);
        // Type 'o'
        sim_insert(&mut text, &mut cursor, "o");
        assert_eq!(text, "Hello World");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn test_selection_delete_and_type() {
        let mut text = "Hello World".to_string();
        let sel = Some((5, 11)); // select " World"
        // Simulate typing "!" with selection active (replaces selection)
        if let Some((ss, se)) = sel {
            let bs = char_to_byte(&text, ss);
            let be = char_to_byte(&text, se);
            text.replace_range(bs..be, "!");
            let cursor = ss + 1;
            assert_eq!(text, "Hello!");
            assert_eq!(cursor, 6);
        }
    }

    #[test]
    fn test_newline_insertion() {
        let mut text = "HelloWorld".to_string();
        let mut cursor = 5;
        sim_insert(&mut text, &mut cursor, "\n");
        assert_eq!(text, "Hello\nWorld");
        assert_eq!(cursor, 6);
    }

    // ── find_nearest_char_index tests ──

    fn make_positions(chars: &str, x_start: f32, advance: f32, y: f32, height: f32) -> Vec<klipp_title::CharPosition> {
        let mut positions = Vec::new();
        for (i, (bi, _)) in chars.char_indices().enumerate() {
            positions.push(klipp_title::CharPosition {
                char_index: i,
                byte_offset: bi,
                x: x_start + i as f32 * advance,
                y,
                advance,
                height,
            });
        }
        // End-of-text position
        let cc = chars.chars().count();
        positions.push(klipp_title::CharPosition {
            char_index: cc,
            byte_offset: chars.len(),
            x: x_start + cc as f32 * advance,
            y,
            advance: 0.0,
            height,
        });
        positions
    }

    #[test]
    fn find_nearest_click_before_text() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 80.0, 60.0); // before first char
        assert_eq!(idx, 0);
    }

    #[test]
    fn find_nearest_click_on_first_char_left_half() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 105.0, 60.0); // left half of 'H'
        assert_eq!(idx, 0);
    }

    #[test]
    fn find_nearest_click_on_first_char_right_half() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 115.0, 60.0); // right half of 'H'
        assert_eq!(idx, 1);
    }

    #[test]
    fn find_nearest_click_on_middle_char() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        // 'l' (index 2) is at x=140..160. Click at x=155 (right half)
        let idx = find_nearest_char_index(&positions, 155.0, 60.0);
        assert_eq!(idx, 3);
    }

    #[test]
    fn find_nearest_click_after_text() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 300.0, 60.0); // way past end
        // Last visible char is 'o' at index 4. Clicking past end returns 4+1 = 5.
        // 5 = char_count = valid end-of-text cursor position.
        assert_eq!(idx, 5);
    }

    #[test]
    fn find_nearest_multiline() {
        let mut positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        // Remove end-of-text position, add second line
        positions.pop();
        let cc1 = 5;
        // Add newline position
        positions.push(klipp_title::CharPosition {
            char_index: 5,
            byte_offset: 5,
            x: 200.0, // end of line 1
            y: 50.0,
            advance: 0.0,
            height: 30.0,
        });
        // Second line: "World" at y=90
        for (i, (bi, _)) in "World".char_indices().enumerate() {
            positions.push(klipp_title::CharPosition {
                char_index: cc1 + 1 + i,
                byte_offset: 6 + bi,
                x: 100.0 + i as f32 * 20.0,
                y: 90.0,
                advance: 20.0,
                height: 30.0,
            });
        }
        // End of text
        positions.push(klipp_title::CharPosition {
            char_index: 11,
            byte_offset: 11,
            x: 200.0,
            y: 90.0,
            advance: 0.0,
            height: 30.0,
        });

        // Click on first line
        let idx = find_nearest_char_index(&positions, 105.0, 60.0);
        assert!(idx <= 1, "Click on line 1 should give index 0-1, got {}", idx);

        // Click on second line
        let idx = find_nearest_char_index(&positions, 105.0, 100.0);
        assert!(idx >= 6 && idx <= 7, "Click on line 2 should give index 6-7, got {}", idx);
    }

    // ── Full editor simulation with char_positions ──

    #[test]
    fn test_full_editor_with_positions() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = "Hello World".to_string();
        let mut cursor = text.chars().count(); // start at end

        // Compute positions
        let td = klipp_timeline::TitleData {
            text: text.clone(),
            font_size: 72.0,
            ..klipp_timeline::TitleData::default()
        };
        let positions = renderer.compute_char_positions(&td, 1920, 1080);

        // Verify we have correct number of positions
        assert_eq!(positions.len(), 12, "11 chars + 1 end position");

        // Navigate left 6 chars (to position 5)
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);

        // Insert "," at cursor
        sim_insert(&mut text, &mut cursor, ",");
        assert_eq!(text, "Hello, World");
        assert_eq!(cursor, 6);

        // Navigate right 6 chars (to position 12, end)
        for _ in 0..6 { sim_arrow_right(&text, &mut cursor); }
        assert_eq!(cursor, 12);

        // Type "!" at end
        sim_insert(&mut text, &mut cursor, "!");
        assert_eq!(text, "Hello, World!");
        assert_eq!(cursor, 13);
    }

    /// Simulates the full round-trip: type chars, update title data, recompute positions,
    /// verify positions match the text at every step.
    #[test]
    fn test_full_roundtrip_typing() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = String::new();
        let mut cursor = 0usize;

        let input = "Ok, this is interesting";
        for ch in input.chars() {
            // Type the character
            sim_insert(&mut text, &mut cursor, &ch.to_string());

            // Simulate UpdateTitleData: create TitleData with new text
            let td = klipp_timeline::TitleData {
                text: text.clone(),
                font_size: 72.0,
                ..klipp_timeline::TitleData::default()
            };

            // Recompute char positions (as upload_title_overlay would)
            let positions = renderer.compute_char_positions(&td, 1920, 1080);

            // Verify: positions should have text.chars().count() + 1 entries
            let cc = text.chars().count();
            assert_eq!(positions.len(), cc + 1,
                "After typing '{}', text='{}': expected {} positions, got {}",
                ch, text, cc + 1, positions.len());

            // Verify: sequential char_indices
            for (i, cp) in positions.iter().enumerate() {
                assert_eq!(cp.char_index, i,
                    "After typing '{}', text='{}': position {} has wrong char_index {}",
                    ch, text, i, cp.char_index);
            }

            // Verify: cursor is valid
            assert!(cursor <= cc,
                "After typing '{}', text='{}': cursor {} exceeds char_count {}",
                ch, text, cursor, cc);

            // Verify: char_to_byte at cursor matches
            let cursor_byte = char_to_byte(&text, cursor);
            assert!(cursor_byte <= text.len(),
                "After typing '{}', text='{}': cursor_byte {} exceeds text.len() {}",
                ch, text, cursor_byte, text.len());
        }

        assert_eq!(text, "Ok, this is interesting");
        assert_eq!(cursor, 23);
    }

    /// Test that navigating left then typing inserts correctly
    #[test]
    fn test_roundtrip_navigate_and_type() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = "Hello World".to_string();
        let mut cursor = text.chars().count(); // 11, end

        // Navigate left 6 chars
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);

        // Type "Beautiful "
        for ch in "Beautiful ".chars() {
            sim_insert(&mut text, &mut cursor, &ch.to_string());

            let td = klipp_timeline::TitleData {
                text: text.clone(),
                font_size: 72.0,
                ..klipp_timeline::TitleData::default()
            };
            let positions = renderer.compute_char_positions(&td, 1920, 1080);

            let cc = text.chars().count();
            assert_eq!(positions.len(), cc + 1);
            for (i, cp) in positions.iter().enumerate() {
                assert_eq!(cp.char_index, i);
            }
        }

        assert_eq!(text, "HelloBeautiful  World");
        // Hmm that's wrong, we want "Hello Beautiful World"
        // The cursor was at position 5 (between 'o' and ' ')
        // Inserting "B" at 5 gives "HelloB World" not "Hello B World"
        // That's correct! Position 5 is BEFORE the space.
    }

    /// Simulate the EXACT egui event flow: each keypress generates both
    /// Event::Key AND Event::Text. Verify no double-processing occurs.
    #[test]
    fn test_event_processing_no_double_insert() {
        // Simulate what egui delivers when user types 'a':
        // [Event::Key { key: Key::A, pressed: true }, Event::Text("a")]
        // Our handler should only process Event::Text, not Event::Key for letter keys.

        let mut text = "Hello".to_string();
        let mut cursor = 5usize; // end

        // Simulate processing events as the viewer does
        let events = vec![
            // egui delivers Key event first, then Text event
            ("key_a", false), // Event::Key { key: A } — should NOT insert
            ("text_a", true), // Event::Text("a") — should insert
        ];

        for (event_type, should_insert) in events {
            if event_type.starts_with("text_") {
                // Process Event::Text
                let ch = &event_type[5..]; // "a"
                let bp = char_to_byte(&text, cursor);
                text.insert_str(bp, ch);
                cursor += ch.chars().count();
            }
            // Event::Key for letter keys falls through to _ => {} (no action)
        }

        assert_eq!(text, "Helloa");
        assert_eq!(cursor, 6);
    }

    /// Verify that typing a full sentence produces correct output
    #[test]
    fn test_type_full_sentence() {
        let mut text = String::new();
        let mut cursor = 0usize;

        let sentence = "Ok, this is interesting";
        for ch in sentence.chars() {
            sim_insert(&mut text, &mut cursor, &ch.to_string());
        }

        assert_eq!(text, sentence);
        assert_eq!(cursor, sentence.chars().count());
    }

    #[test]
    fn test_mouse_click_then_type() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let text = "Hello World";
        let td = klipp_timeline::TitleData {
            text: text.to_string(),
            font_size: 72.0,
            ..klipp_timeline::TitleData::default()
        };
        let positions = renderer.compute_char_positions(&td, 1920, 1080);

        // Verify positions are sequential
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "char_index mismatch at {}", i);
        }

        // Simulate click on the 'W' character (index 6)
        let w_pos = &positions[6];
        let click_x = w_pos.x + w_pos.advance * 0.3; // left half → cursor before W
        let click_y = w_pos.y + w_pos.height * 0.5;
        let nearest = find_nearest_char_index(&positions, click_x, click_y);
        assert_eq!(nearest, 6, "Click on left half of 'W' should give index 6, got {}", nearest);

        // Now insert at this position
        let mut text = text.to_string();
        let mut cursor = nearest;
        sim_insert(&mut text, &mut cursor, "B");
        assert_eq!(text, "Hello BWorld");
        assert_eq!(cursor, 7);
    }
}
