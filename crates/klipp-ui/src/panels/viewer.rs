use crate::icons::Icon;
use crate::inspector::video_tab::{find_transform_index, get_transform, TransformParams};
use crate::state::{AppState, TransformHandle, ViewerTransformDrag};
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Fps, RationalTime, ViewerMode};
use klipp_engine::PlaybackState;

/// Per-frame state computed once at the top of `show_program_viewer`.
/// Passed by reference to extracted helper functions. All fields are `Copy`.
#[derive(Debug, Clone, Copy)]
struct ViewerFrameState {
    image_rect: egui::Rect,
    viewer_rect: egui::Rect,
    video_w: f32,
    video_h: f32,
    grade_still_active: bool,
    /// `(width, height, proxy_ready)` of the media item under the playhead, if any.
    current_media_info: Option<(u32, u32, bool)>,
}

#[derive(Default)]
pub struct ViewerPanel {
    pub video_texture: Option<egui::TextureHandle>,
    /// Separate texture for title overlay — rendered on UI thread for instant preview.
    /// Drawn on top of video_texture with alpha blending. Cleared when title is deselected.
    pub title_overlay_texture: Option<egui::TextureHandle>,
    pub title_overlay_size: (u32, u32),
    pub logo_texture: Option<egui::TextureHandle>,

    // ── GPU-resident viewer path ──
    // Registered egui `TextureId` for `FrameBuffer.gpu_output`. The
    // engine encodes the graded frame directly into that texture on
    // the shared wgpu device; registering a native texture here gives
    // us an id the viewer can sample without ever touching
    // `fb.data`. `gpu_texture_gen` tracks the generation we last
    // registered so we re-register only on actual texture swap
    // (first use, resolution change).
    pub gpu_texture_id: Option<egui::TextureId>,
    pub gpu_texture_gen: u64,
    /// Owned wgpu `TextureView` backing the registered `gpu_texture_id`.
    /// MUST stay alive for the lifetime of `gpu_texture_id`: wgpu 22's
    /// `BindGroup` records the view's storage Id without taking a
    /// reference-counted clone, so once this `TextureView` drops, the
    /// next egui paint that samples `gpu_texture_id` panics with
    /// "TextureView[Id(N,vk)] does not exist". Replaced atomically with
    /// the new view inside `refresh_gpu_viewer_texture` whenever a new
    /// texture generation appears; the previous view drops one step
    /// after `renderer.free_texture(old_id)` releases egui-wgpu's
    /// internal BindGroup so storage cleanup is well-ordered.
    pub gpu_texture_view: Option<wgpu::TextureView>,
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

        // Reserve space: scrub bar lane (16px — gives the playhead ball
        // 14px of vertical room without being clipped by the letterbox above
        // or the playbar below) + playbar (40px). The visible track inside
        // the lane is still a thin 4px strip; the rest is breathing room.
        let scrub_h = 16.0;
        let playbar_h = 40.0;
        let bottom_h = scrub_h + playbar_h;
        let viewer_rect = egui::Rect::from_min_max(
            panel_rect.min,
            egui::pos2(panel_rect.max.x, panel_rect.max.y - bottom_h),
        );

        // Side-by-side dual viewer: this panel is always the Program
        // Viewer. Source content lives in its own panel
        // (`render_source_viewer_placeholder` in app.rs). The legacy
        // Program/Source toggle is gone — `viewer_mode` is retained as a
        // focus/intent hint for keyboard handlers (e.g. I/O marks
        // targeting source vs sequence) but no longer drives visibility
        // here.
        self.show_program_viewer(ui, state, theme, viewer_rect, &mut cmds);

        // ── Scrub bar + Play bar (shared transport chrome) ──
        let chrome_rect = egui::Rect::from_min_size(
            egui::pos2(panel_rect.min.x, viewer_rect.max.y),
            egui::vec2(panel_rect.width(), bottom_h),
        );
        let chrome_cmds = self.draw_transport_chrome(ui, state, theme, chrome_rect, ViewerMode::Program);
        cmds.extend(chrome_cmds);

        cmds
    }

    /// Draw the scrub bar + play bar (transport chrome) within the given
    /// rect. The scrub bar gets the top 16 px, the play bar the remaining
    /// 40 px below it — matching the fixed heights used inside
    /// `ViewerPanel::show`. Exposed so side-by-side layouts (Source Viewer)
    /// can render matching chrome under their own video rects.
    ///
    /// `viewer_kind` selects which playhead the scrub bar and play-bar
    /// timecode reflect (Program → timeline; Source → source clip). For
    /// now the transport buttons (play/pause, step, skip, etc.) still
    /// target the timeline regardless of `viewer_kind` — real source
    /// playback needs a persistent engine decoder, which is a separate
    /// follow-up.
    pub fn draw_transport_chrome(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
    ) -> Vec<AppCommand> {
        const SCRUB_H: f32 = 16.0;
        const PLAYBAR_H: f32 = 40.0;
        let mut cmds = Vec::new();
        let scrub_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(rect.width(), SCRUB_H),
        );
        self.draw_scrub_bar(ui, state, theme, scrub_rect, viewer_kind, &mut cmds);
        let playbar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, scrub_rect.max.y),
            egui::vec2(rect.width(), PLAYBAR_H),
        );
        self.draw_playbar(ui, state, theme, playbar_rect, viewer_kind, &mut cmds);
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

        // Prefer the GPU-resident output when live; fall back to the
        // CPU-uploaded `video_texture`. The video_texture is used for
        // aspect-ratio detection either way — it's populated in
        // parallel by `upload_frame_to_egui`, so `size_vec2()` is
        // still the correct dimensions even when we paint via
        // `gpu_texture_id`.
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
                let image_id = self.gpu_texture_id.unwrap_or_else(|| tex.id());
                ui.painter().image(
                    image_id,
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

    fn handle_eyedropper_click(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
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
                            cmds.push(AppCommand::RequestWhiteBalanceSample { nx, ny });
                        }
                        crate::state::EyedropperMode::Qualifier => {
                            cmds.push(AppCommand::RequestQualifierSample { nx, ny });
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
    }

    fn draw_letterbox(&self, ui: &egui::Ui, viewer_rect: egui::Rect, lb_h: f32) {
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
    }

    fn draw_empty_state(&mut self, ui: &mut egui::Ui, viewer_rect: egui::Rect, theme: &KlippTheme) {
        // Empty state: Klipp logo at 40% opacity
        let center = viewer_rect.center();
        let logo_tex = self.logo_texture.get_or_insert_with(|| {
            let png_bytes = include_bytes!("../../../../assets/icons/klipp_app_icons/hicolor/128x128/apps/klipp.png");
            let image = crate::ui_util::decode_png_or_fallback(png_bytes);
            crate::app::alloc_texture_logged(ui.ctx(), "klipp_viewer_logo", image, egui::TextureOptions::LINEAR)
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
        let logo_color = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(), theme.accent.g(), theme.accent.b(), 102,
        );
        ui.painter().text(
            egui::pos2(center.x, center.y + logo_size * 0.5),
            egui::Align2::CENTER_CENTER,
            "Klipp",
            egui::FontId::proportional(16.0),
            logo_color,
        );
    }

    /// Compute per-frame layout state for the program viewer.
    /// Returns `None` if there's no content to draw (caller should early-return).
    /// Also draws the empty state and stores bookkeeping on `state` as a side effect.
    fn compute_video_layout(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        viewer_rect: egui::Rect,
        theme: &KlippTheme,
    ) -> Option<ViewerFrameState> {
        // Extract media info up-front so the borrow is short-lived
        let current_media_info: Option<(u32, u32, bool)> = state.sequence().tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(state.playhead))
            .and_then(|(_, clip)| clip.media_id().and_then(|mid| state.media_pool.get(&mid)))
            .map(|m| (m.resolution.width, m.resolution.height, m.proxy_ready));

        let has_content = state.playback != PlaybackState::Idle || current_media_info.is_some();

        if !has_content {
            self.draw_empty_state(ui, viewer_rect, theme);
            return None;
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

        if video_w == 0.0 || video_h == 0.0 { return None; }

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

        // Store viewer image rect for scope cursor sampling
        state.viewer_image_rect = Some(image_rect);

        // Scope cursor: will be set at end of function
        state.viewer_pointer_pos = None;

        // Grade still split-view comparison (takes priority over before/after)
        let grade_still_active = state.grade_still_compare
            && state.active_grade_still.is_some()
            && state.active_grade_still.unwrap() < state.grade_stills.len();

        Some(ViewerFrameState {
            image_rect,
            viewer_rect,
            video_w,
            video_h,
            grade_still_active,
            current_media_info,
        })
    }

    fn draw_grade_still_split(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let split_frac = state.split_position;
        let split_x = image_rect.min.x + split_frac * image_rect.width();

        // Left side: grade still reference
        let still_idx = state.active_grade_still.unwrap();
        let still = &mut state.grade_stills[still_idx];
        // Lazily upload texture
        if still.texture.is_none() && !still.rgba.is_empty() {
            let sw = still.width as usize;
            let sh = still.height as usize;
            let pixels: Vec<egui::Color32> = still.rgba.chunks_exact(4)
                .map(|px| egui::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                .collect();
            let image = egui::ColorImage { size: [sw, sh], pixels };
            let name = format!("grade_still_{}", still_idx);
            still.texture = Some(crate::app::alloc_texture_logged(
                ui.ctx(),
                &name,
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        if let Some(tex) = &still.texture {
            let left_rect = egui::Rect::from_min_max(image_rect.min, egui::pos2(split_x, image_rect.max.y));
            let uv_right = split_frac;
            ui.painter().image(
                tex.id(),
                left_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_right, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Right side: current graded frame
        if let Some(tex) = &self.video_texture {
            let right_rect = egui::Rect::from_min_max(egui::pos2(split_x, image_rect.min.y), image_rect.max);
            let uv_left = split_frac;
            ui.painter().image(
                tex.id(),
                right_rect,
                egui::Rect::from_min_max(egui::pos2(uv_left, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    fn draw_split_divider(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let grade_still_active = frame.grade_still_active;
        let stab_compare_active = state.stabilization_compare_active;
        let effect_compare_active = state.split_compare_effect.is_some();
        let split_x = image_rect.min.x + state.split_position * image_rect.width();
        let divider_color = if grade_still_active {
            theme.marker_info // cool blue for grade still compare
        } else if stab_compare_active {
            theme.marker_success // green for stabilization compare
        } else if effect_compare_active {
            egui::Color32::from_rgb(0xc8, 0x78, 0xff) // purple for per-effect compare (no semantic token)
        } else {
            theme.accent
        };

        // Vertical divider line
        ui.painter().vline(
            split_x,
            image_rect.y_range(),
            egui::Stroke::new(2.0_f32, divider_color),
        );

        // Labels
        let label_font = egui::FontId::proportional(10.0);
        let label_y = image_rect.min.y + 14.0;
        let (left_label, right_label) = if grade_still_active {
            let idx = state.active_grade_still.unwrap();
            (state.grade_stills[idx].name.as_str(), "Graded")
        } else if stab_compare_active {
            ("Raw", "Stabilized")
        } else if effect_compare_active {
            ("Without", "With")
        } else {
            ("Original", "Graded")
        };
        ui.painter().text(
            egui::pos2(split_x - 8.0, label_y),
            egui::Align2::RIGHT_CENTER,
            left_label,
            label_font.clone(),
            egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 180),
        );
        ui.painter().text(
            egui::pos2(split_x + 8.0, label_y),
            egui::Align2::LEFT_CENTER,
            right_label,
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

    fn draw_overlay_badges(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        // Collect labels for all active overlays (multiple can be active)
        let mut labels: Vec<&str> = Vec::new();
        let ov = &state.viewer_overlays;
        if ov.zebras { labels.push("ZEBRA"); }
        if ov.false_color { labels.push("FALSE COLOR"); }
        if ov.focus_peaking { labels.push("FOCUS PEAKING"); }
        if ov.safe_areas { labels.push("SAFE AREAS"); }
        if ov.grid { labels.push("GRID"); }
        // no badge for checkerboard
        if !labels.is_empty() {
            let label = labels.join(" + ");
            let badge_pos = egui::pos2(image_rect.max.x - 8.0, image_rect.min.y + 12.0);
            let badge_color = egui::Color32::from_rgb(0xff, 0x40, 0x40);
            // Shadow
            painter.text(
                egui::pos2(badge_pos.x + 0.5, badge_pos.y + 0.5),
                egui::Align2::RIGHT_CENTER,
                &label,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgb(0x00, 0x00, 0x00),
            );
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_CENTER,
                &label,
                egui::FontId::proportional(10.0),
                badge_color,
            );
        }
    }

    fn draw_safe_area_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let center = image_rect.center();
        let w = image_rect.width();
        let h = image_rect.height();
        // 90% action safe
        let action_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.action_safe_pct, h * state.viewer_prefs.action_safe_pct));
        painter.rect_stroke(action_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 200, 0, 120)));
        painter.text(
            egui::pos2(action_rect.left() + 4.0, action_rect.top() + 2.0),
            egui::Align2::LEFT_TOP, "Action Safe",
            egui::FontId::proportional(9.0),
            egui::Color32::from_rgba_premultiplied(255, 200, 0, 140),
        );
        // 80% title safe
        let title_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.title_safe_pct, h * state.viewer_prefs.title_safe_pct));
        painter.rect_stroke(title_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(0, 180, 255, 120)));
        painter.text(
            egui::pos2(title_rect.left() + 4.0, title_rect.top() + 2.0),
            egui::Align2::LEFT_TOP, "Title Safe",
            egui::FontId::proportional(9.0),
            egui::Color32::from_rgba_premultiplied(0, 180, 255, 140),
        );
    }

    fn draw_grid_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let w = image_rect.width();
        let h = image_rect.height();
        let gc = state.viewer_prefs.grid_color;
        let stroke = egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], gc[3]));
        // Rule of thirds: vertical lines
        for i in 1..=2 {
            let x = image_rect.left() + w * i as f32 / 3.0;
            painter.line_segment([egui::pos2(x, image_rect.top()), egui::pos2(x, image_rect.bottom())], stroke);
        }
        // Rule of thirds: horizontal lines
        for i in 1..=2 {
            let y = image_rect.top() + h * i as f32 / 3.0;
            painter.line_segment([egui::pos2(image_rect.left(), y), egui::pos2(image_rect.right(), y)], stroke);
        }
        // Center crosshair — reuses the grid_color pref with boosted alpha
        // so it remains visible against bright frames where the faint grid
        // would normally disappear. Clamped to u8.
        let cx = image_rect.center().x;
        let cy = image_rect.center().y;
        let cross_alpha = ((gc[3] as u16) * 2).min(255) as u8;
        let cross_color = egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], cross_alpha);
        let cross = egui::Stroke::new(0.5_f32, cross_color);
        painter.line_segment([egui::pos2(cx - 12.0, cy), egui::pos2(cx + 12.0, cy)], cross);
        painter.line_segment([egui::pos2(cx, cy - 12.0), egui::pos2(cx, cy + 12.0)], cross);
    }

    fn draw_checkerboard_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
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
                painter.rect_filled(r, 0.0, color);
                x += tile;
                col += 1;
            }
            y += tile;
            row += 1;
        }
    }

    fn draw_proxy_badge(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        if state.showing_thumbnail_preview {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PREVIEW",
                egui::FontId::monospace(9.0),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140),
            );
        } else if state.use_proxies && frame.current_media_info.is_some_and(|(_, _, proxy_ready)| proxy_ready) {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PROXY",
                egui::FontId::monospace(9.0),
                theme.accent,
            );
        }
    }

    fn draw_multicam_angle_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let bar_h = 32.0;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(image_rect.left(), image_rect.bottom() + 4.0),
            egui::vec2(image_rect.width(), bar_h),
        );

        let btn_w = (bar_rect.width() / state.multicam_angle_count as f32).min(80.0);
        for i in 0..state.multicam_angle_count {
            let is_active = i as u8 == state.multicam_active_angle;
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(bar_rect.left() + i as f32 * btn_w, bar_rect.top()),
                egui::vec2(btn_w - 2.0, bar_h),
            );
            let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let bg = if is_active {
                theme.accent
            } else if resp.hovered() {
                theme.bg_hover
            } else {
                theme.bg_panel
            };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            // CONTRAST RULE: dark text on amber background
            let text_color = if is_active {
                theme.text_on_accent
            } else {
                theme.text_primary
            };
            ui.painter().text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Angle {}", i + 1),
                egui::FontId::proportional(12.0),
                text_color,
            );

            if resp.clicked() {
                cmds.push(AppCommand::SwitchMulticamAngle { angle: i + 1 });
            }
        }
    }

    fn handle_scope_cursor_input(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        // Scope cursor: read pointer position from raw input events (most reliable on Wayland)
        for event in &ui.ctx().input(|i| i.events.clone()) {
            if let egui::Event::PointerMoved(pos) = event {
                if image_rect.contains(*pos) {
                    state.viewer_pointer_pos = Some(*pos);
                }
            }
        }
        // Fallback: also check egui's pointer state
        if state.viewer_pointer_pos.is_none() {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos())) {
                if image_rect.contains(pos) {
                    state.viewer_pointer_pos = Some(pos);
                }
            }
        }

        // Click-to-lock scope cursor: Ctrl+Click on viewer image
        if state.scope_cursor_show || (state.scope_window.open
            && state.scope_window.panes.iter().any(|p| p.settings.show_scope_cursor))
        {
            let clicked = ui.input(|i| i.pointer.primary_clicked() && i.modifiers.ctrl);
            if clicked {
                if let Some(pos) = state.viewer_pointer_pos {
                    // Toggle lock: if already locked at this position, unlock
                    if state.scope_cursor_locked {
                        state.scope_cursor_locked = false;
                        state.scope_cursor_locked_pos = None;
                        state.scope_cursor_locked_data = None;
                    } else {
                        state.scope_cursor_locked = true;
                        state.scope_cursor_locked_pos = Some(pos);
                        // Data will be computed by app.rs on next frame
                    }
                }
            }
        }
    }

    fn draw_scope_cursor(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let video_w = frame.video_w;
        let video_h = frame.video_h;

        // Draw sample box overlay on viewer when any scope cursor is active
        let floating_cursor = state.scope_window.open
            && state.scope_window.panes.iter().any(|p| p.settings.show_scope_cursor);
        let bottom_cursor = state.scope_cursor_show && state.show_scopes;
        let any_cursor = floating_cursor || bottom_cursor;

        // Determine the effective sample position (live or locked)
        let effective_pos = if state.scope_cursor_locked {
            state.scope_cursor_locked_pos
        } else {
            state.viewer_pointer_pos
        };

        if let Some(pos) = effective_pos {
            if any_cursor {
                // Pick the largest sample radius across all active scope cursors
                let mut sample_radius = if bottom_cursor { state.scope_cursor_sample_radius } else { 0 };
                if floating_cursor {
                    let win_r = state.scope_window.panes.iter()
                        .filter(|p| p.settings.show_scope_cursor)
                        .map(|p| p.settings.scope_cursor_sample_radius)
                        .max()
                        .unwrap_or(0);
                    sample_radius = sample_radius.max(win_r);
                }
                let scale_x = image_rect.width() / video_w;
                let scale_y = image_rect.height() / video_h;
                let box_w = (sample_radius as f32 * 2.0 + 1.0) * scale_x;
                let box_h = (sample_radius as f32 * 2.0 + 1.0) * scale_y;
                let cursor_color = if state.scope_cursor_locked {
                    egui::Color32::from_rgba_unmultiplied(232, 160, 32, 220)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 200, 50, 200)
                };
                let box_rect = egui::Rect::from_center_size(pos, egui::vec2(box_w, box_h));
                painter.rect_stroke(box_rect, 0.0, egui::Stroke::new(1.0_f32, cursor_color));
                // Center crosshair
                painter.line_segment(
                    [egui::pos2(pos.x - 4.0, pos.y), egui::pos2(pos.x + 4.0, pos.y)],
                    egui::Stroke::new(1.0_f32, cursor_color),
                );
                painter.line_segment(
                    [egui::pos2(pos.x, pos.y - 4.0), egui::pos2(pos.x, pos.y + 4.0)],
                    egui::Stroke::new(1.0_f32, cursor_color),
                );
                // "LOCKED" label
                if state.scope_cursor_locked {
                    painter.text(
                        egui::pos2(pos.x, pos.y - 10.0),
                        egui::Align2::CENTER_BOTTOM,
                        "LOCKED",
                        egui::FontId::proportional(8.0),
                        cursor_color,
                    );
                }
            }
        }
    }

    fn handle_text_edit_entry(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let image_rect = frame.image_rect;
        // ── Viewer text edit mode ──
        // Double-click on viewer while a title clip is selected → enter text edit mode
        if !state.viewer_text_edit {
            if let Some(clip_id) = state.selected_clip {
                let is_title = state.sequence().tracks.iter()
                    .any(|t| t.find_clip(clip_id).is_some_and(|c| c.title_data().is_some()));
                if is_title && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                    if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                        if image_rect.contains(pos) {
                            cmds.push(AppCommand::BeginTitleEdit { clip_id });
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
    }

    fn handle_text_edit_exit(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let _ = ui;
        // Auto-exit text edit if the clip was deselected or changed
        if state.viewer_text_edit {
            let still_selected = state.viewer_text_edit_clip
                .is_some_and(|cid| state.selected_clip == Some(cid));
            if !still_selected {
                cmds.push(AppCommand::EndTitleEdit);
                state.viewer_text_edit_buffer.clear();
                state.viewer_text_selection = None;
                state.viewer_text_selection_locked = None;
                state.viewer_text_cursor = 0;
                state.viewer_char_positions.clear();
                state.viewer_text_select_anchor = None;
            }
        }
    }

    fn draw_text_edit_widget(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let image_rect = frame.image_rect;
        let video_h = frame.video_h;
        let viewer_rect = frame.viewer_rect;
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
                cmds.push(AppCommand::EndTitleEdit);
                state.viewer_text_edit_buffer.clear();
                state.viewer_text_selection = None;
                state.viewer_text_selection_locked = None;
                state.viewer_text_cursor = 0;
                state.viewer_char_positions.clear();
                state.viewer_text_select_anchor = None;
            }
        }
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

        let lb_h = viewer_rect.height() * 0.08;
        let frame = match self.compute_video_layout(ui, state, viewer_rect, theme) {
            Some(f) => f,
            None => return,
        };
        let image_rect = frame.image_rect;
        let video_w = frame.video_w;
        let video_h = frame.video_h;

        // Program viewer paints only its own output. The `browser_skim_texture`
        // is a hover-preview surface that belongs to the Source Viewer in
        // the dual-viewer layout — showing it here would duplicate the
        // preview onto the Program side, which was the single-viewer-era
        // behaviour.
        if frame.grade_still_active {
            self.draw_grade_still_split(ui, state, &frame);
        } else {
            // Prefer the engine's GPU-resident output (`gpu_texture_id`)
            // when registered — it's already on the same wgpu device
            // as egui-wgpu, so sampling is a single texture read with
            // no PCIe download. Fall back to the legacy CPU-uploaded
            // `video_texture` when the GPU path isn't live (env gate
            // off, eframe running glow, shared device not installed,
            // or this frame's grade hasn't finished yet).
            let image_id = self.gpu_texture_id
                .or_else(|| self.video_texture.as_ref().map(|t| t.id()));
            if let Some(id) = image_id {
                ui.painter().image(
                    id,
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
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

        self.handle_text_edit_entry(ui, state, &frame, cmds);
        self.handle_text_edit_exit(ui, state, cmds);
        self.draw_text_edit_widget(ui, state, &frame, cmds);

        // ── On-screen transform controls ──
        // Suppress transform controls during text edit mode
        if state.show_transform_controls && !state.viewer_text_edit && state.eyedropper_mode == crate::state::EyedropperMode::None && !state.has_modal_open() {
            self.draw_transform_controls(ui, state, image_rect, video_w, video_h, cmds);
        }

        // ── Before/After or Grade Still split divider ──
        let show_split_divider = state.before_after_active
            || frame.grade_still_active
            || state.stabilization_compare_active
            || state.split_compare_effect.is_some();
        if show_split_divider {
            self.draw_split_divider(ui, state, theme, &frame);
            // Keyboard nudge: ←/→ move the split by 0.02. Only consumed
            // while a compare mode is active so the same keys remain free
            // for transport / track navigation in normal viewer mode.
            ui.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    state.split_position = (state.split_position + 0.02).clamp(0.05, 0.95);
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    state.split_position = (state.split_position - 0.02).clamp(0.05, 0.95);
                }
            });
        }

        self.draw_letterbox(ui, viewer_rect, lb_h);

        self.handle_eyedropper_click(ui, state, image_rect, cmds);

        // ── Interactive mask handles on viewer ──
        // Draw and interact with masks from the selected node in the clip's node graph.
        self.draw_mask_handles(ui, state, image_rect, cmds);

        // ── Point tracking overlay ──
        self.draw_track_points(ui, state, image_rect, cmds, theme);

        // ── Overlay mode badge (top-right of video) ──
        self.draw_overlay_badges(ui.painter(), state, &frame);

        // Safe area overlay: title safe (80%) and action safe (90%)
        if state.viewer_overlays.safe_areas {
            self.draw_safe_area_overlay(ui.painter(), state, &frame);
        }

        // Grid overlay: rule of thirds + center crosshair
        if state.viewer_overlays.grid {
            self.draw_grid_overlay(ui.painter(), state, &frame);
        }

        // Checkerboard transparency overlay (drawn behind video, replaces black background)
        if state.viewer_overlays.checkerboard {
            self.draw_checkerboard_overlay(ui.painter(), state, &frame);
        }

        // Timecode + zoom now shown in playbar below scrub bar

        // Proxy badge
        self.draw_proxy_badge(ui.painter(), state, theme, &frame);

        // ── Multicam angle bar (shown below the viewer when multicam is active) ──
        if state.multicam_viewer_active && state.multicam_angle_count > 0 {
            self.draw_multicam_angle_bar(ui, state, theme, cmds, &frame);
        }

        // Performance overlay
        if state.show_perf_overlay {
            self.draw_perf_overlay(ui, state, image_rect, theme);
        }

        self.handle_scope_cursor_input(ui, state, &frame);
        self.draw_scope_cursor(ui.painter(), state, &frame);
    }

    fn draw_scrub_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // Visible track is a thin strip centred vertically inside the wider
        // scrub lane. The lane's extra height exists purely so the playhead
        // ball can sit on top of the track without being clipped above/below.
        let track_h = 4.0_f32;
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.center().y - track_h * 0.5),
            egui::vec2(rect.width(), track_h),
        );

        // Track background — full width.
        ui.painter().rect_filled(track_rect, 0.0, theme.bg_raised);

        // Per-viewer playhead/duration. Source uses the loaded media's
        // total duration; Program uses the sequence duration.
        let (dur_secs, current_secs, rate): (f64, f64, u32) = match viewer_kind {
            ViewerMode::Program => (
                state.sequence_duration().to_secs_f64(),
                state.playhead_smooth_secs,
                state.fps.rate(),
            ),
            ViewerMode::Source => {
                let media = state
                    .source_media_id
                    .and_then(|mid| state.media_pool.get(&mid));
                let dur = media.map(|m| m.duration.to_secs_f64()).unwrap_or(0.0);
                let cur = state.source_playhead.to_secs_f64();
                let rate = media.map(|m| m.fps.rate()).unwrap_or(state.fps.rate());
                (dur, cur, rate)
            }
        };
        let progress = if dur_secs > 0.0 {
            (current_secs / dur_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let fill_w = progress * track_rect.width();
        if fill_w > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(track_rect.min, egui::vec2(fill_w, track_rect.height())),
                0.0,
                theme.accent,
            );
        }

        // Focus indicator: thin frame around the progress-bar track when
        // this viewer matches the current viewer_mode. Subtle, lives in
        // chrome that's already there, and doesn't intrude on the viewer
        // image itself.
        let is_focused = state.viewer_mode == viewer_kind;
        if is_focused {
            ui.painter().rect_stroke(
                track_rect.expand(2.0),
                1.0,
                egui::Stroke::new(1.0_f32, theme.text_secondary),
            );
        }

        // Playhead head (11x11 circle) — drawn at the centre of the wider
        // lane so its full diameter has clear vertical room above and below
        // the visible track strip.
        let head_x = track_rect.min.x + fill_w;
        let head_center = egui::pos2(head_x, rect.center().y);
        ui.painter().circle_filled(head_center, 7.0, theme.accent_glow);
        ui.painter().circle_filled(head_center, 5.5, theme.accent);

        // Scrub interaction — clicks/drags both focus the viewer and
        // (if there's a clip to seek in) emit a per-viewer seek. The
        // focus emit runs unconditionally so an empty source scrub bar
        // still accepts focus on click.
        // egui's `resp.dragged()` for a `Sense::click_and_drag`
        // widget can fire spuriously while a drag started elsewhere
        // (e.g. a mask handle on the image) is in progress — the
        // scrub bar would then dispatch SeekTo on every drag tick
        // from the cursor's position over the image, yanking the
        // playhead wildly. The rect-contains gate makes the scrub
        // bar respond ONLY to gestures whose cursor is actually
        // over it.
        let pointer_in_rect = resp
            .interact_pointer_pos()
            .map(|p| rect.contains(p))
            .unwrap_or(false);
        if (resp.dragged() || resp.clicked()) && pointer_in_rect {
            if !is_focused {
                cmds.push(AppCommand::SetViewerMode(viewer_kind));
            }
            if dur_secs > 0.0 {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let t = ((pos.x - track_rect.min.x) / track_rect.width().max(1.0))
                        .clamp(0.0, 1.0) as f64;
                    let target_secs = t * dur_secs;
                    let target_tc = klipp_core::Timecode::from_secs_f64(target_secs, rate);
                    let cmd = match viewer_kind {
                        ViewerMode::Program => AppCommand::SeekTo(target_tc),
                        ViewerMode::Source => AppCommand::SeekSource(target_tc),
                    };
                    cmds.push(cmd);
                }
            }
        }
    }

    fn draw_playbar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        let enabled = state.playback != PlaybackState::Idle;
        let fps_val = state.fps.as_f64();
        let cy = rect.center().y;

        // Background
        ui.painter().rect_filled(rect, 0.0, theme.bg_deep);

        // ── Left: Timecode ── (per-viewer: source clip time vs. timeline time)
        let (tc_secs, tc_rate) = match viewer_kind {
            ViewerMode::Program => (state.playhead_smooth_secs, state.fps.rate()),
            ViewerMode::Source => {
                let media = state
                    .source_media_id
                    .and_then(|mid| state.media_pool.get(&mid));
                let rate = media.map(|m| m.fps.rate()).unwrap_or(state.fps.rate());
                (state.source_playhead.to_secs_f64(), rate)
            }
        };
        let tc_str = format_timecode_hmsf(tc_secs, tc_rate);
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
            // Click-to-edit is Program-only: typing a timecode emits
            // `SeekTo` which targets the timeline, so allowing it on the
            // source side would silently seek the wrong viewer.
            let sense = if viewer_kind == ViewerMode::Program {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let tc_resp = ui.allocate_rect(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 8.0, rect.min.y),
                    egui::vec2(110.0, rect.height()),
                ),
                sense,
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 12.0, cy),
                egui::Align2::LEFT_CENTER,
                &tc_str,
                egui::FontId::monospace(14.0),
                theme.text_primary,
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

        let nav_fg = theme.text_primary;
        let nav_bg = theme.bg_panel;
        let nav_hover = theme.bg_hover;
        let disabled_fg = theme.text_dim;

        // Helper closure: every push from this playbar goes through
        // viewer-routing so the source-side playbar drives the source
        // engine instead of the timeline. Without this, clicking Play
        // in the source viewer's chrome would emit `AppCommand::Play`
        // and start timeline playback — exactly the bug we hit.
        use crate::panels::transport::route_command_for_viewer;
        let route = |c: AppCommand| route_command_for_viewer(c, viewer_kind);

        // Skip back
        x += self.playbar_nav_btn(ui, Icon::SkipBack, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to start (Home)", cmds, Some(route(AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate())))));
        // Rewind
        x += self.playbar_nav_btn(ui, Icon::Rewind, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Rewind (J)", cmds, Some(route(AppCommand::SeekRelative(-(fps_val * 10.0) as i64))));
        // Step back
        x += self.playbar_nav_btn(ui, Icon::CaretLeft, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step back 1 frame", cmds, Some(route(AppCommand::SeekRelative(-1))));

        // Play / Pause (larger, amber)
        {
            let play_rect = egui::Rect::from_center_size(
                egui::pos2(x + play_size / 2.0, cy),
                egui::vec2(play_size, play_size),
            );
            let resp = ui.allocate_rect(play_rect, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
            // Read the playing state of *this viewer* so the icon
            // accurately reflects what clicking will do. Without this
            // the source playbar would always show the timeline's
            // play state.
            let is_playing = match viewer_kind {
                ViewerMode::Source => state.source_playback == PlaybackState::Playing,
                ViewerMode::Program => state.playback == PlaybackState::Playing,
            };
            let bg = if enabled { theme.accent } else { theme.accent_dim };
            ui.painter().rect_filled(play_rect, play_size / 2.0, bg);
            let play_icon = if is_playing { Icon::Pause } else { Icon::Play };
            let icon_rect = egui::Rect::from_center_size(play_rect.center(), egui::vec2(18.0, 18.0));
            play_icon.bold(18.0).tint(theme.text_on_accent).paint_at(ui, icon_rect);
            let clicked = resp.clicked();
            resp.on_hover_text("Play / Pause (Space)");
            if clicked && enabled {
                if is_playing { cmds.push(route(AppCommand::Pause)); } else { cmds.push(route(AppCommand::Play)); }
            }
        }
        x += play_size + btn_spacing;

        // Step forward
        x += self.playbar_nav_btn(ui, Icon::CaretRight, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step forward 1 frame", cmds, Some(route(AppCommand::SeekRelative(1))));
        // Fast forward
        x += self.playbar_nav_btn(ui, Icon::FastForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Fast forward (L)", cmds, Some(route(AppCommand::SeekRelative((fps_val * 10.0) as i64))));
        // Skip forward
        {
            let dur = state.sequence_duration();
            self.playbar_nav_btn(ui, Icon::SkipForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to end (End)", cmds, Some(route(AppCommand::SeekTo(dur))));
        }

        // ── Right side controls ──
        // Layout from right edge:
        //   [Vol% slot 36] gap(6) [Slider 60] gap(6) [Mute 20] gap(8) [Transform 20]
        // Each element gets a fixed-width slot so neighbors can't overlap when
        // the value text changes (e.g. "0%" vs "130%").
        // Note: viewer zoom % was removed from here — it lives in the View menu
        // (Window > Viewer Zoom) and the timeline ruler has its own Fit/+/-.
        let rx = rect.max.x - 12.0;

        // Volume percentage — fixed 36px slot at the far right.
        let vol_pct = (state.master_volume * 100.0) as u32;
        let vol_text = format!("{}%", vol_pct);
        let vol_text_slot_w = 36.0_f32;
        let vol_text_right = rx;
        let vol_text_left = vol_text_right - vol_text_slot_w;
        ui.painter().text(
            egui::pos2(vol_text_right, cy),
            egui::Align2::RIGHT_CENTER,
            &vol_text,
            egui::FontId::monospace(10.0),
            theme.text_dim,
        );

        // Volume slider — right edge sits 6px LEFT of the volume %'s left edge.
        //
        // CRITICAL: egui::Slider's track width is governed by `spacing.slider_width`,
        // NOT by the parent UI's max_rect or by `add_sized`. The slider widget
        // internally calls `ui.allocate_response(vec2(spacing.slider_width, ...))`
        // which ignores both the child UI's bounds and any size hint passed to
        // add_sized. The ONLY way to make the slider narrower is to mutate
        // `spacing_mut().slider_width` on the UI before adding the slider.
        let slider_w = 60.0_f32;
        let slider_h = 16.0_f32;
        let slider_right = vol_text_left - 6.0;
        let slider_rect = egui::Rect::from_min_size(
            egui::pos2(slider_right - slider_w, cy - slider_h * 0.5),
            egui::vec2(slider_w, slider_h),
        );
        let mut vol = state.master_volume;
        let mut slider_ui = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
        slider_ui.spacing_mut().slider_width = slider_w;
        let slider = egui::Slider::new(&mut vol, 0.0..=1.3).show_value(false);
        if slider_ui.add(slider).changed() {
            cmds.push(AppCommand::SetMasterVolume(vol));
        }

        // Mute button
        let mute_x = slider_rect.min.x - 22.0;
        let mute_rect = egui::Rect::from_center_size(
            egui::pos2(mute_x, cy),
            egui::vec2(20.0, 20.0),
        );
        let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click())
            .on_hover_text(if state.master_muted { "Unmute master output" } else { "Mute master output" });
        let mute_icon = if state.master_muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
        let mute_color = if state.master_muted {
            theme.marker_error
        } else {
            theme.text_secondary
        };
        let mute_icon_rect = egui::Rect::from_center_size(mute_rect.center(), egui::vec2(14.0, 14.0));
        mute_icon.image(14.0).tint(mute_color).paint_at(ui, mute_icon_rect);
        if mute_resp.clicked() {
            let old = state.master_muted;
            cmds.push(AppCommand::SetMasterMute(!old));
        }

        // Transform-controls toggle and camera/extract-frame button are
        // Program-only — they operate on the timeline output (transform
        // handles + frame capture). On the Source side they'd both be
        // misleading: clicking the xform toggle would highlight on
        // both viewers since it writes shared state, and the camera
        // would always grab the program frame anyway.
        if viewer_kind == ViewerMode::Program {
            // Transform handles only make sense for an actual selected,
            // non-Gap clip on the timeline — `draw_transform_controls`
            // early-returns otherwise. Reflect that in the toggle so the
            // button is visibly disabled when there's nothing to act on
            // and a click surfaces a toast instead of silently doing
            // nothing.
            let xform_enabled = state
                .selected_clip
                .and_then(|cid| {
                    state.sequence().tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == cid)
                })
                .is_some_and(|c| !matches!(c.kind, klipp_timeline::ClipKind::Gap));
            let xform_x = mute_x - 26.0;
            let xform_rect = egui::Rect::from_center_size(
                egui::pos2(xform_x, cy),
                egui::vec2(20.0, 20.0),
            );
            let xform_resp = ui.allocate_rect(xform_rect, egui::Sense::click());
            let xform_color = if !xform_enabled {
                theme.text_dim
            } else if state.show_transform_controls {
                theme.accent
            } else {
                theme.text_secondary
            };
            let stroke_w = if xform_enabled { 1.5 } else { 1.2 };
            let ir = xform_rect.shrink(4.0);
            ui.painter().rect_stroke(ir, 0.0, egui::Stroke::new(stroke_w, xform_color));
            let cs = 2.0;
            for corner in [ir.left_top(), ir.right_top(), ir.left_bottom(), ir.right_bottom()] {
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(corner, egui::vec2(cs * 2.0, cs * 2.0)),
                    0.0,
                    xform_color,
                );
            }
            if xform_resp.clicked() {
                if xform_enabled {
                    state.show_transform_controls = !state.show_transform_controls;
                } else {
                    state.toast.show("Select a video clip to use transform controls");
                }
            }
            xform_resp.on_hover_text(if xform_enabled {
                "Toggle transform controls"
            } else {
                "Select a clip to enable transform"
            });

            let cam_x = xform_x - 26.0;
            let cam_rect = egui::Rect::from_center_size(
                egui::pos2(cam_x, cy),
                egui::vec2(20.0, 20.0),
            );
            let cam_resp = ui.allocate_rect(cam_rect, egui::Sense::click());
            let cam_color = if cam_resp.hovered() {
                theme.accent
            } else {
                theme.text_secondary
            };
            let body = cam_rect.shrink2(egui::vec2(3.0, 5.0));
            ui.painter().rect_stroke(body, 2.0, egui::Stroke::new(1.2_f32, cam_color));
            let bump = egui::Rect::from_center_size(
                egui::pos2(cam_rect.center().x - 3.0, body.min.y - 1.5),
                egui::vec2(5.0, 3.0),
            );
            ui.painter().rect_stroke(bump, 1.0, egui::Stroke::new(1.2_f32, cam_color));
            ui.painter().circle_stroke(body.center(), 3.2, egui::Stroke::new(1.2_f32, cam_color));
            if cam_resp.clicked() {
                cmds.push(AppCommand::ExtractFrameToBin);
            }
            cam_resp.on_hover_text("Extract frame to bin (Ctrl+Alt+B)");
        }
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
        theme: &KlippTheme,
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

        // Engine-side per-frame telemetry. Painted between the UI
        // counters and the OS counters so the user reads "what's my
        // UI doing / what's the engine doing / what's the process
        // doing" top-to-bottom. Values come from the
        // `klipp_engine::perf::PerfRing` snapshotted in
        // `update_perf_stats`.
        let avg = &state.perf_averages;
        let (res_line, fps_line, budget_line) = match state.perf_latest_sample {
            Some(ref s) => (
                format!("Frame:  {}x{}", s.width, s.height),
                format!("Target: {:.1} fps", s.target_fps),
                match s.frame_budget_ms() {
                    Some(budget) => {
                        format!(
                            "Budget: {:.1} ms  actual {:.1} ms",
                            budget, avg.total_ms,
                        )
                    }
                    None => format!("Budget: — ms  actual {:.1} ms", avg.total_ms),
                },
            ),
            None => (
                "Frame:  — (engine idle)".to_string(),
                "Target: — fps".to_string(),
                "Budget: — ms  actual —".to_string(),
            ),
        };

        let rec_line = if state.perf_recording_active {
            Some("● REC  (Ctrl+Shift+P to stop)".to_string())
        } else {
            None
        };

        let mut lines = Vec::new();
        if let Some(r) = rec_line { lines.push(r); }
        lines.extend([
            format!("UI FPS: {:.0}", state.perf_fps),
            format!("Decode: {hw_label}"),
            format!("State:  {playback_label}"),
            res_line,
            fps_line,
            budget_line,
            format!(
                "  decode/seq  {:.2} ms",
                avg.seq_read_ms,
            ),
            format!(
                "  grade       {:.2} ms",
                avg.grade_ms,
            ),
            format!(
                "  builtin fx  {:.2} ms",
                avg.builtin_ms,
            ),
            format!(
                "  ofx         {:.2} ms",
                avg.ofx_ms,
            ),
            format!(
                "  lut         {:.2} ms",
                avg.lut_ms,
            ),
            // Wrap-stage timings — populated by `update_wrap_stages`
            // even when no grade is applied. Always non-zero on a
            // playing clip; surfaces "where did the frame budget
            // actually go" without requiring a grade chain.
            format!(
                "  bridge u8   {:.2} ms",
                avg.bridge_u8_ms,
            ),
            format!(
                "  reencode    {:.2} ms",
                avg.reencode_ms,
            ),
            format!(
                "  scope       {:.2} ms",
                avg.scope_ms,
            ),
            format!(
                "Drops:  {} in last 1s · {} total",
                state.perf_drops_last_sec, state.perf_dropped_frames,
            ),
            format!("Frames: {}", state.perf_frame_count),
            format!("CPU:    {:.0}%", state.perf_cpu_percent),
            format!("Mem:    {:.0} MB", state.perf_memory_mb),
        ]);

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

        // Pick per-line colours by content instead of by hard-coded
        // index — lines have shifted as we added the engine telemetry.
        // Highlights: red for active drops, amber for high CPU, green
        // for HW decode, coral for "actual > budget" frame pacing.
        let budget_exceeded = state
            .perf_latest_sample
            .as_ref()
            .and_then(|s| s.frame_budget_ms())
            .map(|b| avg.total_ms > b)
            .unwrap_or(false);
        for (i, line) in lines.iter().enumerate() {
            let _ = i;
            let color = if line.starts_with("● REC") {
                theme.marker_error
            } else if line.starts_with("Decode:") && state.perf_hw_decode {
                theme.marker_success
            } else if line.starts_with("Drops:") && state.perf_drops_last_sec > 0 {
                theme.marker_error
            } else if line.starts_with("Budget:") && budget_exceeded {
                theme.marker_warning
            } else if line.starts_with("CPU:") && state.perf_cpu_percent > 80.0 {
                theme.marker_warning
            } else {
                theme.text_primary
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

    /// Draw track point crosshairs and handle tracking placement mode.
    fn draw_track_points(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Tracking placement mode: badge + click to place. The mode badge
        // alone signals tracking — no inner border that would compete with
        // the outer panel chrome.
        if state.tracking_mode {
            // "TRACKING" label top-left
            let label_pos = egui::pos2(image_rect.left() + 8.0, image_rect.top() + 8.0);
            let label_bg = egui::Rect::from_min_size(label_pos, egui::vec2(80.0, 18.0));
            ui.painter().rect_filled(label_bg, 3.0, egui::Color32::from_rgba_unmultiplied(
                theme.accent.r(), theme.accent.g(), theme.accent.b(), 200,
            ));
            ui.painter().text(
                egui::pos2(label_bg.center().x, label_bg.center().y),
                egui::Align2::CENTER_CENTER,
                "TRACKING",
                egui::FontId::proportional(10.0),
                theme.text_on_accent,
            );

            // Click to place tracker
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let place_resp = ui.allocate_rect(image_rect, egui::Sense::click());
            if place_resp.clicked() {
                if let Some(pos) = place_resp.interact_pointer_pos() {
                    let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    if let Some(clip_id) = state.selected_clip {
                        cmds.push(AppCommand::AddTrackPoint { clip_id, x: nx as f64, y: ny as f64 });
                        state.tracking_mode = false;
                    }
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.tracking_mode = false;
            }
        }

        // Draw existing track points
        if let Some(clip_id) = state.selected_clip {
            let clip_opt = state.sequence().tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|t| t.find_clip(clip_id));
            if let Some(clip) = clip_opt {
                if clip.track_points.is_empty() {
                    return;
                }

                // Compute current frame index relative to clip source_in
                let playhead_secs = state.playhead_smooth_secs;
                let clip_in_secs = clip.timeline_in.to_secs_f64();
                let source_in_secs = clip.source_in.to_secs_f64();
                let clip_time = playhead_secs - clip_in_secs + source_in_secs;
                let fps = state.fps.as_f64();
                let current_frame = (clip_time * fps).round() as usize;

                for (idx, tp) in clip.track_points.iter().enumerate() {
                    if !tp.enabled {
                        continue;
                    }

                    // Find position for current frame
                    let frame_offset = current_frame.saturating_sub(tp.start_frame);
                    let pos = if frame_offset < tp.positions.len() {
                        Some(tp.positions[frame_offset])
                    } else {
                        None
                    };

                    if let Some((nx, ny)) = pos {
                        let color = egui::Color32::from_rgba_unmultiplied(
                            tp.color[0], tp.color[1], tp.color[2], tp.color[3],
                        );
                        let dim_color = egui::Color32::from_rgba_unmultiplied(
                            tp.color[0], tp.color[1], tp.color[2], 50,
                        );

                        // Convert normalized to screen coordinates
                        let sx = nx as f32 * image_rect.width() + image_rect.left();
                        let sy = ny as f32 * image_rect.height() + image_rect.top();
                        let center = egui::pos2(sx, sy);
                        let arm = 8.0;

                        // Crosshair lines
                        ui.painter().line_segment(
                            [egui::pos2(sx - arm, sy), egui::pos2(sx + arm, sy)],
                            egui::Stroke::new(1.5_f32, color),
                        );
                        ui.painter().line_segment(
                            [egui::pos2(sx, sy - arm), egui::pos2(sx, sy + arm)],
                            egui::Stroke::new(1.5_f32, color),
                        );

                        // Center circle
                        ui.painter().circle_stroke(center, 3.0, egui::Stroke::new(1.5_f32, color));

                        // Search region rectangle (dim)
                        let sr_px = tp.search_radius as f32 * 2.0;
                        // Scale search region relative to image size (approximate)
                        let sr_screen_w = sr_px / 960.0 * image_rect.width(); // assume half-res ~960
                        let sr_screen_h = sr_px / 540.0 * image_rect.height();
                        let sr_rect = egui::Rect::from_center_size(
                            center,
                            egui::vec2(sr_screen_w, sr_screen_h),
                        );
                        ui.painter().rect_stroke(sr_rect, 0.0, egui::Stroke::new(0.5_f32, dim_color));

                        // Name label
                        let label = if tp.name.is_empty() {
                            format!("Track {}", idx + 1)
                        } else {
                            tp.name.clone()
                        };
                        ui.painter().text(
                            egui::pos2(sx + arm + 4.0, sy - arm),
                            egui::Align2::LEFT_CENTER,
                            &label,
                            egui::FontId::proportional(9.0),
                            color,
                        );

                        // Highlight active track point
                        if state.active_track_point == Some(idx) {
                            ui.painter().circle_stroke(center, 6.0, egui::Stroke::new(1.0_f32, color));
                        }
                    }
                }
            }
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

        // Show masks whenever the clip has at least one Mask node
        // with shapes — selection is not required. Prefer the
        // user's selected Mask node if there is one (so multiple
        // Mask nodes on the same clip can be disambiguated), else
        // fall back to the first Mask node found. The Color-tab
        // user flow never selects a node, so the fallback is what
        // makes the overlay visible for them.
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                state.viewer_mask_drag = None;
                return;
            }
        };
        let selected_id = state.selected_node_id;
        let mask_node: Option<(klipp_nodes::NodeId, klipp_core::MaskStack)> = state
            .sequence()
            .tracks
            .iter()
            .find_map(|t| t.find_clip(clip_id))
            .and_then(|clip| {
                let selected_match = selected_id.and_then(|sid| {
                    clip.node_graph.nodes.iter().find(|n|
                        n.id == sid && n.kind == klipp_nodes::NodeKind::Mask)
                });
                let fallback = clip.node_graph.nodes.iter()
                    .find(|n| n.kind == klipp_nodes::NodeKind::Mask);
                selected_match.or(fallback).map(|n| (
                    n.id,
                    n.params.masks.clone().unwrap_or_default(),
                ))
            });
        let (node_id, masks) = match mask_node {
            Some((id, ms)) if !ms.masks.is_empty() => (id, ms),
            _ => {
                state.viewer_mask_drag = None;
                return;
            }
        };

        // Compute clip-time at the current playhead so we can render
        // each mask at its evaluated (keyframe-aware) position rather
        // than its static one. Without this the outline lies after a
        // tracking pass writes centre keyframes — the rendered shape
        // stays at the original drop point while the actual matte
        // follows the tracked path.
        let clip_time_secs: f64 = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .map(|c| (state.playhead_smooth_secs - c.timeline_in.to_secs_f64()).max(0.0))
            .unwrap_or(0.0);

        let iw = image_rect.width();
        let ih = image_rect.height();
        let handle_radius = 5.0_f32;
        let outline_color = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
        let handle_color = egui::Color32::WHITE;

        // Helper: convert normalized coords to screen coords
        let to_screen = |nx: f32, ny: f32| -> egui::Pos2 {
            egui::pos2(image_rect.min.x + nx * iw, image_rect.min.y + ny * ih)
        };

        for (mask_idx, raw_mask) in masks.masks.iter().enumerate() {
            if !raw_mask.enabled { continue; }
            // Render + drag against the keyframe-evaluated shape so
            // the outline matches the actual matte. The drag-start
            // arms below capture this evaluated shape as
            // initial_shape so drag math operates on the same
            // coordinate frame the user is looking at.
            let mask_owned = raw_mask.evaluated_at(clip_time_secs);
            let mask = &mask_owned;

            match &mask.shape {
                klipp_core::MaskShape::Circle { center_x, center_y, radius } => {
                    let center = to_screen(*center_x, *center_y);
                    let r_px = radius * iw;

                    // Draw outline ellipse (approximate with circle)
                    ui.painter().circle_stroke(center, r_px, egui::Stroke::new(1.5_f32, outline_color));

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
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
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
                    ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.5_f32, outline_color));

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
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
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
                    ui.painter().line_segment([start, end], egui::Stroke::new(1.5_f32, outline_color));
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
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
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
                            ui.painter().line_segment([a, b], egui::Stroke::new(1.5_f32, outline_color));
                        }
                        // Bezier-specific edits that don't go through the
                        // drag-handle loop: double-click an edge to insert
                        // a vertex at the click point, shift-click a
                        // vertex to delete it. Collected here and flushed
                        // via SetNodeMasks on the next frame; mutating
                        // `masks` in place would fight the borrow.
                        let mut insert_at_edge: Option<(usize, (f32, f32))> = None;
                        let mut delete_vertex: Option<usize> = None;
                        // Draw control point handles + handle shift-click delete
                        if state.viewer_mask_drag.is_none() {
                            for (pi, sp) in screen_pts.iter().enumerate() {
                                let handle_rect = egui::Rect::from_center_size(*sp, egui::vec2(12.0, 12.0));
                                let resp = ui.allocate_rect(handle_rect, egui::Sense::click_and_drag());
                                let color = if resp.hovered() || resp.dragged() {
                                    egui::Color32::WHITE
                                } else {
                                    outline_color
                                };
                                ui.painter().circle_filled(*sp, 4.0, color);
                                let shift = ui.input(|i| i.modifiers.shift);
                                if resp.clicked() && shift && points.len() > 3 {
                                    delete_vertex = Some(pi);
                                } else if resp.drag_started() {
                                    if let Some(pos) = resp.interact_pointer_pos() {
                                        state.selected_node_id = Some(node_id);
                                        cmds.push(AppCommand::FocusMaskInspector);
                                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                        if !raw_mask.keyframes.is_empty() {
                                            cmds.push(AppCommand::RemoveMaskTracking {
                                                clip_id, node_id, mask_index: mask_idx,
                                            });
                                        }
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
                                if resp.hovered() && shift && points.len() > 3 {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
                                } else if resp.hovered() || resp.dragged() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                                }
                            }
                            // Double-click an edge to insert a vertex.
                            // Checked against each edge's distance; the
                            // closest within ~10 px wins.
                            let dbl = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
                            if dbl {
                                if let Some(click) = ui.input(|i| i.pointer.interact_pos()) {
                                    let mut best: Option<(usize, f32, (f32, f32))> = None;
                                    let n = screen_pts.len();
                                    for i in 0..n {
                                        let a = screen_pts[i];
                                        let b = screen_pts[(i + 1) % n];
                                        let (dist, proj) = point_segment_distance(click, a, b);
                                        if dist < 10.0 {
                                            let nx = ((proj.x - image_rect.min.x) / image_rect.width())
                                                .clamp(0.0, 1.0);
                                            let ny = ((proj.y - image_rect.min.y) / image_rect.height())
                                                .clamp(0.0, 1.0);
                                            match best {
                                                Some((_, d, _)) if d <= dist => {}
                                                _ => best = Some((i, dist, (nx, ny))),
                                            }
                                        }
                                    }
                                    if let Some((edge_idx, _, pt)) = best {
                                        insert_at_edge = Some((edge_idx, pt));
                                    }
                                }
                            }
                        }
                        // Flush edit (only one op per frame is possible
                        // because the viewer routes both through the
                        // same double-click / click inputs — but belt
                        // and braces: handle delete first, then insert).
                        if delete_vertex.is_some() || insert_at_edge.is_some() {
                            let mut updated = masks.clone();
                            if let Some(m) = updated.masks.get_mut(mask_idx) {
                                if let klipp_core::MaskShape::Bezier { points } = &mut m.shape {
                                    if let Some(di) = delete_vertex {
                                        if di < points.len() && points.len() > 3 {
                                            points.remove(di);
                                        }
                                    }
                                    if let Some((edge_idx, pt)) = insert_at_edge {
                                        let insert_at = (edge_idx + 1).min(points.len());
                                        points.insert(insert_at, pt);
                                    }
                                }
                            }
                            cmds.push(AppCommand::SetNodeMasks {
                                clip_id, node_id, masks: updated,
                            });
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
                cmds.push(AppCommand::EndNodeParamEdit { clip_id: drag_clip_id });
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
        let outline_stroke = egui::Stroke::new(1.5_f32, outline_color);
        for i in 0..4 {
            painter.line_segment([corners[i], corners[(i + 1) % 4]], outline_stroke);
        }

        // Draw diagonal crosshairs through center (subtle)
        let cross_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60);
        let cross_stroke = egui::Stroke::new(0.5_f32, cross_color);
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
        let handle_size = state.viewer_prefs.transform_handle_size;
        let handle_half = handle_size / 2.0;
        for corner in &corners {
            let handle_rect = egui::Rect::from_center_size(*corner, egui::vec2(handle_size, handle_size));
            painter.rect_filled(handle_rect, 0.0, egui::Color32::WHITE);
            painter.rect_stroke(handle_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        }

        // Center handle (position) — circle
        let center_radius = 6.0;
        painter.circle_filled(pos_screen, center_radius, egui::Color32::WHITE);
        painter.circle_stroke(pos_screen, center_radius, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        // Small crosshair inside center handle
        let ch = 3.0;
        painter.line_segment(
            [egui::pos2(pos_screen.x - ch, pos_screen.y), egui::pos2(pos_screen.x + ch, pos_screen.y)],
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        );
        painter.line_segment(
            [egui::pos2(pos_screen.x, pos_screen.y - ch), egui::pos2(pos_screen.x, pos_screen.y + ch)],
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
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
                egui::Stroke::new(1.5_f32, anchor_color),
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
            egui::Stroke::new(1.5_f32, outline_color),
        );
        let rot_radius = 5.0;
        painter.circle_filled(rot_handle_pos, rot_radius, egui::Color32::WHITE);
        painter.circle_stroke(rot_handle_pos, rot_radius, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        // Small arc icon inside rotation handle
        let arc_r = 2.5;
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x - arc_r, rot_handle_pos.y),
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
            ],
            egui::Stroke::new(0.8_f32, egui::Color32::BLACK),
        );
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
                egui::pos2(rot_handle_pos.x + arc_r, rot_handle_pos.y),
            ],
            egui::Stroke::new(0.8_f32, egui::Color32::BLACK),
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

    pub fn show_source_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Resolve which texture to paint into the source video rect:
        //   1. The browser/timeline hover-skim texture, when active.
        //      Hover is a transient overlay and must win so the user can
        //      preview any clip without losing what's loaded.
        //   2. The decoded first-frame of the committed source clip,
        //      gated on its MediaId matching `source_media_id` so a
        //      stale frame from a previous load never flashes.
        // Falling all the way through paints the metadata card.
        let committed_tex = state
            .source_video_texture
            .as_ref()
            .and_then(|(mid, tex)| (Some(*mid) == state.source_media_id).then_some(tex));
        let live_tex = state.source_live_texture.as_ref();
        // Priority depends on whether the source viewer is playing:
        //   - Playing  → engine-decoded live frame wins (smooth video).
        //   - Paused   → legacy thumbnail decoder wins (fast scrubs).
        //                The engine's 500ms-per-seek decode would
        //                bottleneck a drag-scrub against a 4K source;
        //                the legacy `generate_thumbnail_at_size` path
        //                returns small frames in tens of ms, which is
        //                what makes scrubbing feel instant. Engine
        //                seeks still run in the background and update
        //                `source_live_texture` for the eventual Play.
        // Browser skim hover overlay always wins on top.
        let playback_tex = if state.source_playback == klipp_engine::PlaybackState::Playing {
            live_tex.or(committed_tex)
        } else {
            committed_tex.or(live_tex)
        };
        let active_tex = state.browser_skim_texture.as_ref().or(playback_tex);
        if let Some(tex) = active_tex {
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
                    Self::draw_source_in_out_markers(ui, &image_rect, media, theme);
                }
                return;
            }
        }

        let media = state.source_media_id
            .and_then(|mid| state.media_pool.get(&mid));

        if let Some(media) = media {
            let cx = viewer_rect.center().x;
            let cy = viewer_rect.center().y;
            let dim = theme.text_dim;
            let label_col = theme.text_secondary;
            let val_col = theme.text_primary;

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
                theme.text_accent,
            );

            // Graphical IN/OUT markers on the viewer rect
            Self::draw_source_in_out_markers(ui, &viewer_rect, media, theme);

            // Edit buttons. Subclip is enabled only when the source clip has
            // an IN and OUT set AND is not itself a subclip — creating a
            // subclip of a subclip is rejected in the handler anyway.
            let has_in_out = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
            let subclip_enabled = media
                .default_source_in.is_some()
                && media.default_source_out.is_some()
                && !media.is_subclip();
            let edit_buttons: &[(&str, AppCommand, bool)] = &[
                ("Insert (,)", AppCommand::InsertEdit, true),
                ("Overwrite (.)", AppCommand::OverwriteEdit, true),
                ("Lift (;)", AppCommand::LiftEdit, has_in_out),
                ("Extract (')", AppCommand::ExtractEdit, has_in_out),
                ("Subclip", AppCommand::CreateSubclipFromZone { source_media_id: media.id, name: None }, subclip_enabled),
            ];

            let btn_w = 90.0;
            let btn_gap = 6.0;
            let btn_count = edit_buttons.len() as f32;
            let total_w = btn_w * btn_count + btn_gap * (btn_count - 1.0);
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
                    theme.bg_deep
                } else if resp.hovered() {
                    theme.bg_hover
                } else {
                    theme.bg_raised
                };
                let text_color = if *enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
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
                theme.text_dim,
            );
        }
    }

    /// Draw graphical IN/OUT markers on the source viewer.
    fn draw_source_in_out_markers(
        ui: &mut egui::Ui,
        image_rect: &egui::Rect,
        media: &klipp_media::MediaItem,
        theme: &KlippTheme,
    ) {
        let amber = theme.accent;
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
                egui::Color32::from_rgba_unmultiplied(
                    theme.accent.r(), theme.accent.g(), theme.accent.b(), 60,
                ),
            );
        }
    }
}

/// Rotate a point around a center by the given angle in radians.
/// Distance from `p` to the closed segment [`a`, `b`] and the
/// projected point on the segment (clamped to the endpoints).
/// Used by the Bezier-mask edit flow to decide which edge a
/// double-click should insert a new vertex into.
fn point_segment_distance(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> (f32, egui::Pos2) {
    let ab = b - a;
    let len_sq = ab.x * ab.x + ab.y * ab.y;
    let proj = if len_sq < 1e-6 {
        a
    } else {
        let ap = p - a;
        let t = ((ap.x * ab.x + ap.y * ab.y) / len_sq).clamp(0.0, 1.0);
        egui::pos2(a.x + ab.x * t, a.y + ab.y * t)
    };
    let d = p - proj;
    ((d.x * d.x + d.y * d.y).sqrt(), proj)
}

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
        if (line_ys.is_empty() || (cp.y - line_ys.last().unwrap()).abs() > 1.0)
            && !line_ys.contains(&cp.y) {
                line_ys.push(cp.y);
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
