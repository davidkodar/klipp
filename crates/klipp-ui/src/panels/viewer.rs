use crate::icons::Icon;
use crate::inspector::video_tab::{find_transform_index, get_transform, TransformParams};
use crate::state::{AppState, TransformHandle, ViewerTransformDrag};
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Fps, RationalTime, ViewerMode};
use klipp_engine::PlaybackState;

/// Drop the alpha so the input color renders as a solid outline /
/// label-badge fill. AI Tools' detection palette colors are
/// pre-multiplied at ~50% alpha to look right as mask tints; the
/// bbox outline + label badge want the same hue at full alpha so
/// they read crisply against the viewer.
fn opaque_for_outline(c: egui::Color32) -> egui::Color32 {
    if c.a() == 0 {
        return c;
    }
    // Reverse the pre-multiply: divide each RGB channel by the
    // existing alpha (so we recover the un-premultiplied color),
    // then re-emit at alpha=255.
    let inv_a = 255.0 / (c.a() as f32);
    let r = ((c.r() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    let g = ((c.g() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    let b = ((c.b() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(r, g, b)
}

/// Map a normalised `(0..1, 0..1)` clip-space point to the image
/// rect's pixel coordinates. The viewer paints frame content into
/// `image_rect`; gizmos that store positions in clip-space (focal
/// points, anchors, hit-targets) call this to project to screen.
///
/// Callers that want a clamped-into-frame result clamp the input
/// before passing in — this helper preserves `(nx, ny)` outside
/// `[0, 1]` so gizmos that legitimately overflow the frame (a title
/// object dragged past the canvas) still draw at the right offset.
pub(crate) fn viewer_norm_to_screen(image_rect: egui::Rect, nx: f32, ny: f32) -> egui::Pos2 {
    egui::pos2(
        image_rect.min.x + nx * image_rect.width(),
        image_rect.min.y + ny * image_rect.height(),
    )
}

/// Inverse of [`viewer_norm_to_screen`]. Returns the clip-space
/// `(nx, ny)` for a screen-space pointer position. Result is **not**
/// clamped — callers that want `[0, 1]` (e.g. focal-point storage)
/// `.clamp(0.0, 1.0)` after reading. The drag handler for the
/// title-object editor needs unclamped results so drag-deltas stay
/// accurate when the cursor leaves the canvas.
///
/// Width / height each clamp to `>= 1.0` to avoid divide-by-zero on
/// a degenerate image rect (briefly possible during layout settle).
pub(crate) fn viewer_screen_to_norm(image_rect: egui::Rect, p: egui::Pos2) -> (f32, f32) {
    let w = image_rect.width().max(1.0);
    let h = image_rect.height().max(1.0);
    (
        (p.x - image_rect.min.x) / w,
        (p.y - image_rect.min.y) / h,
    )
}

/// Map a frame-pixel position (e.g. a glyph bounding-box from
/// `klipp_title::compute_char_positions`, which returns coords in
/// frame pixels at the title's render resolution) to the viewer
/// image-rect's screen pixels. Composes [`viewer_norm_to_screen`]
/// with the frame_w/h normalisation; pulls a degenerate-frame guard
/// (`>= 1.0`) so a zero-sized frame doesn't divide-by-zero.
///
/// Used by the visual letter picker
/// (`crate::inspector::letter_picker`).
pub(crate) fn viewer_frame_px_to_screen(
    image_rect: egui::Rect,
    frame_w: f32, frame_h: f32,
    px: f32, py: f32,
) -> egui::Pos2 {
    let fw = frame_w.max(1.0);
    let fh = frame_h.max(1.0);
    viewer_norm_to_screen(image_rect, px / fw, py / fh)
}

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
    /// Set when the window could not be started. The output is still on
    /// and still publishing — only the convenience of opening a viewer for
    /// it failed — so the button says so rather than claiming success.
    pub hdr_output_unopened: bool,
    /// The HDR reference-output window, when this panel launched it.
    ///
    /// Owned here so toggling the output off closes the window it opened,
    /// rather than leaving one behind that a user has to hunt down. A
    /// presenter the user started themselves is not in here and is left
    /// alone — it attaches and detaches on its own.
    pub hdr_output_child: Option<std::process::Child>,
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
    /// Whether the newest frame is in that texture rather than in
    /// `video_texture` (`FrameBuffer::gpu_output_live`), and its size.
    /// A frame of bytes that follows one on the GPU paints from
    /// `video_texture` again.
    pub gpu_frame_live: bool,
    pub gpu_frame_size: (u32, u32),
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

impl Drop for ViewerPanel {
    /// Take the reference-output window with us.
    ///
    /// Best effort: a process that exits through `std::process::exit` runs
    /// no destructors, and the presenter is built to cope with that — it
    /// sees the shared frame's magic gone and says the viewer stopped
    /// publishing rather than freezing on the last picture. This is so the
    /// ordinary case does not leave a window behind for the user to find.
    fn drop(&mut self) {
        self.close_hdr_output_window();
    }
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

        // Scroll-zoom + middle-drag-pan over the video area. Allocated
        // AFTER `show_program_viewer` so transform handles / on-canvas
        // gizmos painted inside the viewer keep priority on LMB events.
        let tex_size = self.video_texture.as_ref()
            .map(|t| t.size_vec2())
            .unwrap_or_else(|| {
                state.media_pool.all().next()
                    .map(|m| egui::vec2(m.resolution.width as f32, m.resolution.height as f32))
                    .unwrap_or(egui::vec2(1920.0, 1080.0))
            });
        self.handle_viewer_zoom_pan_input(
            ui,
            state,
            viewer_rect,
            tex_size,
            ViewerMode::Program,
        );

        // Floating "..." overflow menu in the video area's top-right
        // corner — mirrors the Source side. Holds whatever the responsive
        // playbar dropped at the current width plus zoom presets and the
        // viewer overlay toggles.
        self.draw_viewer_overflow_menu(
            ui,
            state,
            theme,
            viewer_rect,
            ViewerMode::Program,
            &mut cmds,
        );

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
        // (state mut-borrow flows through to scrub_bar so the
        // playhead can update visually on the same frame the user
        // drags — without that the bar lags engine seek latency.)
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
        let gpu_id = self.gpu_frame_live.then_some(self.gpu_texture_id).flatten();
        if let Some(tex) = &self.video_texture {
            let tex_size = match gpu_id {
                Some(_) if self.gpu_frame_size.0 > 0 && self.gpu_frame_size.1 > 0 => {
                    egui::vec2(self.gpu_frame_size.0 as f32, self.gpu_frame_size.1 as f32)
                }
                _ => tex.size_vec2(),
            };
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
                let image_id = gpu_id.unwrap_or_else(|| tex.id());
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
                // Text sits on top of a black-translucent perf
                // overlay bar — stays light regardless of theme
                // polarity (the bar is the contrast partner, not
                // the panel underneath).
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
                egui::FontId::proportional(13.0),
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

    /// Capture clicks/drags onto the viewer image when the AI Tools
    /// panel is open with an active task. Routes pointer events into
    /// `state.ai_tools_state.prompts` in normalised image coords.
    fn handle_ai_tools_capture(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        if !state.ai_tools_state.panel_visible
            || state.ai_tools_state.active_task.is_none()
            || state.eyedropper_mode != crate::state::EyedropperMode::None
        {
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        let resp = ui.allocate_rect(image_rect, egui::Sense::click_and_drag());
        let normalise = |p: egui::Pos2| -> (f32, f32) {
            let nx = ((p.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
            let ny = ((p.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
            (nx, ny)
        };

        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state
                    .ai_tools_state
                    .prompts
                    .record_click(nx, ny, crate::ai_tools::ClickButton::Left);
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state
                    .ai_tools_state
                    .prompts
                    .record_click(nx, ny, crate::ai_tools::ClickButton::Right);
            }
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                let button = if ui.input(|i| i.pointer.secondary_down()) {
                    crate::ai_tools::ClickButton::Right
                } else {
                    crate::ai_tools::ClickButton::Left
                };
                state.ai_tools_state.prompts.start_drag(nx, ny, button);
            }
        } else if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state.ai_tools_state.prompts.extend_drag(nx, ny);
            }
        }
        if resp.drag_stopped() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state.ai_tools_state.prompts.end_drag(nx, ny);
            }
        }
    }

    /// Paint the AI Tools mask overlay + prompt markers onto the
    /// viewer when the panel is open and there's something to show.
    /// No-op when the panel is closed.
    fn draw_ai_tools_overlay(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        if !state.ai_tools_state.panel_visible {
            return;
        }

        let painter = ui.painter_at(image_rect);

        if let Some(tex) = &state.ai_tools_state.mask_overlay_texture {
            crate::ai_tools::paint_mask_texture(&painter, image_rect, tex);
        }

        // Per-candidate bbox + class label badge. RF-DETR-Seg
        // emits up to 64 candidates each with bbox + label +
        // confidence; we draw all of them so the user sees
        // multi-object detections rather than just the
        // highest-confidence one. BiRefNet / MobileSAM still
        // produce a single candidate with `bbox = None` so the
        // bbox-drawing branch falls through cleanly for them.
        let candidates = state.ai_tools_state.inference.candidates();
        for (idx, cand) in candidates.iter().enumerate() {
            let Some(bbox) = cand.bbox else {
                continue;
            };
            let bbox_color = crate::ai_tools::detection_color(idx);
            let stroke = egui::Stroke::new(2.0_f32, opaque_for_outline(bbox_color));
            let bx0 = image_rect.min.x + bbox.x * image_rect.width();
            let by0 = image_rect.min.y + bbox.y * image_rect.height();
            let bx1 = bx0 + bbox.w * image_rect.width();
            let by1 = by0 + bbox.h * image_rect.height();
            let rect = egui::Rect::from_min_max(
                egui::pos2(bx0, by0),
                egui::pos2(bx1, by1),
            );
            painter.rect_stroke(rect, 2.0, stroke);

            // Label badge — class name + confidence at the top-left
            // of the bbox. White text on the bbox color so it stays
            // readable on any background.
            let label_text = format!(
                "{} {:.0}%",
                cand.label.as_deref().unwrap_or("?"),
                cand.confidence * 100.0,
            );
            let font_id = egui::FontId::proportional(11.0);
            let galley = painter.layout_no_wrap(
                label_text,
                font_id,
                egui::Color32::WHITE,
            );
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(bx0, by0 - galley.rect.height() - 4.0),
                egui::vec2(galley.rect.width() + 8.0, galley.rect.height() + 4.0),
            );
            painter.rect_filled(badge_rect, 2.0, opaque_for_outline(bbox_color));
            painter.galley(
                badge_rect.min + egui::vec2(4.0, 2.0),
                galley,
                egui::Color32::WHITE,
            );
        }

        // Click / drag markers — small dots for clicks, rectangle for
        // boxes, polyline for scribbles. Visualises what the model is
        // about to receive so the user can preview their prompt set.
        let to_screen = |x: f32, y: f32| -> egui::Pos2 {
            egui::pos2(
                image_rect.min.x + x * image_rect.width(),
                image_rect.min.y + y * image_rect.height(),
            )
        };
        for click in &state.ai_tools_state.prompts.clicks {
            let color = if click.positive {
                egui::Color32::from_rgb(80, 220, 100)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            };
            painter.circle_filled(to_screen(click.x, click.y), 5.0, color);
            painter.circle_stroke(
                to_screen(click.x, click.y),
                5.0,
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
        }
        for b in &state.ai_tools_state.prompts.boxes {
            let r = egui::Rect::from_two_pos(to_screen(b.x1, b.y1), to_screen(b.x2, b.y2));
            painter.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 200, 240)),
            );
        }
        for s in &state.ai_tools_state.prompts.finalized_scribbles {
            if s.points.len() < 2 {
                continue;
            }
            let stroke = egui::Stroke::new(
                2.0_f32,
                if s.positive {
                    egui::Color32::from_rgb(80, 220, 100)
                } else {
                    egui::Color32::from_rgb(220, 80, 80)
                },
            );
            for window in s.points.windows(2) {
                let a = to_screen(window[0].0, window[0].1);
                let b = to_screen(window[1].0, window[1].1);
                painter.line_segment([a, b], stroke);
            }
        }
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

        // Honor the user's per-viewer zoom selection. `zoom == 0.0`
        // means "fit to viewer" (the default and pro-NLE letterbox
        // behavior). Any other value is a multiplier on the source's
        // native pixel dimensions. Pan only applies in zoom-in mode;
        // Fit always centers. The painter is clipped to `viewer_rect`
        // below so high-zoom output doesn't bleed into chrome.
        let view = state.program_view;
        let (display_w, display_h) = if view.zoom <= 0.0 {
            if video_aspect > panel_aspect {
                (viewer_rect.width(), viewer_rect.width() / video_aspect)
            } else {
                (viewer_rect.height() * video_aspect, viewer_rect.height())
            }
        } else {
            (video_w * view.zoom, video_h * view.zoom)
        };

        let pan = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
        let image_rect = egui::Rect::from_center_size(
            viewer_rect.center() + pan,
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
            theme.accent_alt // per-effect compare — distinct from primary accent
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
        if ov.false_color { labels.push("FALSE COLOUR"); }
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
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgba_premultiplied(255, 200, 0, 140),
        );
        // 80% title safe
        let title_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.title_safe_pct, h * state.viewer_prefs.title_safe_pct));
        painter.rect_stroke(title_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(0, 180, 255, 120)));
        painter.text(
            egui::pos2(title_rect.left() + 4.0, title_rect.top() + 2.0),
            egui::Align2::LEFT_TOP, "Title Safe",
            egui::FontId::proportional(10.0),
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
                egui::FontId::monospace(10.0),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140),
            );
        } else if state.use_proxies && frame.current_media_info.is_some_and(|(_, _, proxy_ready)| proxy_ready) {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PROXY",
                egui::FontId::monospace(10.0),
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
                        egui::FontId::proportional(10.0),
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
        if state.viewer_text_edit {
            return;
        }
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };
        if !ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            return;
        }
        let pos = match ui.input(|i| i.pointer.interact_pos()) {
            Some(p) => p,
            None => return,
        };
        if !image_rect.contains(pos) {
            return;
        }
        // Pull the title's data once. If the selected clip isn't a
        // Title, bail out.
        let td_opt = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .and_then(|c| c.title_data())
            .cloned();
        let td = match td_opt {
            Some(td) => td,
            None => return,
        };

        // Double-click on a TextBox object's bounding box → enter
        // inline edit for THAT object. Iterate top-down so the
        // front-most layer wins when stacks overlap. Hidden /
        // locked layers are excluded.
        let mut target_object: Option<(klipp_timeline::TitleObjectId, String)> = None;
        for obj in td.objects.iter().rev() {
            if !obj.visible || obj.locked {
                continue;
            }
            if let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content {
                if object_screen_bb(obj, image_rect).contains(pos) {
                    target_object = Some((obj.id, tb.text.clone()));
                    break;
                }
            }
        }

        if let Some((oid, text)) = target_object {
            // Inline-edit path: enter edit mode anchored to a
            // specific object, and seed the buffer from that
            // object's text rather than `td.text()` (which is the
            // primary TextBox — fine for single-text titles, wrong
            // for multi-object).
            cmds.push(AppCommand::BeginTitleEdit { clip_id });
            cmds.push(AppCommand::SelectTitleObject {
                clip_id,
                object_id: Some(oid.0),
            });
            state.viewer_text_edit_object = Some(oid);
            state.viewer_text_edit_request_focus = true;
            state.viewer_char_positions.clear();
            state.viewer_text_cursor = 0;
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_buffer = text;
        } else if let Some((oid, text)) = td.objects.iter().find_map(|o| match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) if o.visible && !o.locked => Some((o.id, tb.text.clone())),
            _ => None,
        }) {
            // A double-click beside the text still edits: the first
            // text layer, in place.
            cmds.push(AppCommand::BeginTitleEdit { clip_id });
            cmds.push(AppCommand::SelectTitleObject { clip_id, object_id: Some(oid.0) });
            state.viewer_text_edit_object = Some(oid);
            state.viewer_text_edit_request_focus = true;
            state.viewer_char_positions.clear();
            state.viewer_text_cursor = text.chars().count();
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_buffer = text;
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
                state.viewer_text_edit_object = None;
            }
        }
    }

    /// Editing text on the render itself. The engine's overlay is the
    /// text; an invisible `TextEdit` handles the keys, the clipboard
    /// and undo; the caret and the selection are drawn from the
    /// engine's own glyph boxes, and a click or a drag on the text
    /// places them by those boxes too.
    fn draw_text_edit_widget(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        if !state.viewer_text_edit {
            return;
        }
        let image_rect = frame.image_rect;
        let viewer_rect = frame.viewer_rect;
        let (video_w, video_h) = (frame.video_w.max(1.0), frame.video_h.max(1.0));
        let edit_id = ui.id().with("viewer_text_edit");
        let prev_buf = state.viewer_text_edit_buffer.clone();
        let Some(clip_id) = state.viewer_text_edit_clip else { return };
        let Some(td) = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip_id)).and_then(|c| c.title_data()).cloned() else {
            return;
        };
        // The layer being edited: the targeted one, else the first
        // text layer.
        let obj = state
            .viewer_text_edit_object
            .and_then(|oid| td.object(oid).cloned())
            .or_else(|| td.objects.iter().find(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))).cloned());
        let Some(obj) = obj else { return };
        if state.viewer_text_edit_object != Some(obj.id) {
            state.viewer_text_edit_object = Some(obj.id);
            state.viewer_char_positions.clear();
        }
        let bb = object_screen_bb(&obj, image_rect);
        let to_screen = |x: f32, y: f32| {
            egui::pos2(image_rect.min.x + x / video_w * image_rect.width(), image_rect.min.y + y / video_h * image_rect.height())
        };

        // ── The pointer on the text: caret and selection by glyph box ──
        let hit = ui.interact(bb.expand(8.0), edit_id.with("hit"), egui::Sense::click_and_drag());
        let index_at = |positions: &[klipp_title::CharPosition], p: egui::Pos2| -> Option<usize> {
            if positions.is_empty() {
                return None;
            }
            // The row under the pointer (nearest row when between
            // lines), then the nearest glyph edge within it.
            let row_of = |cp: &klipp_title::CharPosition| (to_screen(cp.x, cp.y).y, to_screen(cp.x, cp.y + cp.height).y);
            let mut best_row: Option<(f32, f32)> = None;
            for cp in positions.iter() {
                let (top, bottom) = row_of(cp);
                let d = if p.y < top { top - p.y } else if p.y > bottom { p.y - bottom } else { 0.0 };
                if best_row.is_none_or(|(bd, _)| d < bd) {
                    best_row = Some((d, top));
                }
            }
            let row_top = best_row?.1;
            let mut best: Option<(f32, usize)> = None;
            for cp in positions.iter() {
                if (row_of(cp).0 - row_top).abs() > 0.5 {
                    continue;
                }
                let left = to_screen(cp.x, cp.y).x;
                let right = to_screen(cp.x + cp.advance, cp.y).x;
                for (edge, idx) in [(left, cp.char_index), (right, cp.char_index + 1)] {
                    let d = (p.x - edge).abs();
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, idx));
                    }
                }
            }
            best.map(|(_, i)| i)
        };
        let mut set_range: Option<(usize, usize)> = None;
        if let Some(p) = hit.interact_pointer_pos() {
            if hit.drag_started() || hit.clicked() {
                if let Some(i) = index_at(&state.viewer_char_positions, p) {
                    state.viewer_text_select_anchor = Some(i);
                    set_range = Some((i, i));
                }
            } else if hit.dragged() {
                if let (Some(a), Some(i)) = (state.viewer_text_select_anchor, index_at(&state.viewer_char_positions, p)) {
                    set_range = Some((a, i));
                }
            }
        }
        if hit.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        if let Some((a, b)) = set_range {
            let len = state.viewer_text_edit_buffer.chars().count();
            let (a, b) = (a.min(len), b.min(len));
            let mut st = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(a), egui::text::CCursor::new(b))));
            egui::TextEdit::store_state(ui.ctx(), edit_id, st);
            state.viewer_text_cursor_blink_time = ui.input(|i| i.time);
        }

        // ── The invisible editor: keys, clipboard, undo ──
        let mut te_ui = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(bb.min, egui::vec2(1.0, 1.0))));
        te_ui.style_mut().visuals.selection.bg_fill = egui::Color32::TRANSPARENT;
        te_ui.style_mut().visuals.selection.stroke = egui::Stroke::NONE;
        let te_output = egui::TextEdit::multiline(&mut state.viewer_text_edit_buffer)
            .id(edit_id)
            .desired_width(1.0)
            .desired_rows(1)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .text_color(egui::Color32::TRANSPARENT)
            .show(&mut te_ui);
        if state.viewer_text_edit_request_focus || !te_output.response.has_focus() {
            state.viewer_text_edit_request_focus = false;
            te_output.response.request_focus();
        }
        if let Some(cursor_range) = te_output.cursor_range {
            let primary = cursor_range.primary.ccursor.index;
            let secondary = cursor_range.secondary.ccursor.index;
            if primary != state.viewer_text_cursor {
                state.viewer_text_cursor_blink_time = ui.input(|i| i.time);
            }
            state.viewer_text_cursor = primary;
            if primary != secondary {
                let (s, e) = if primary < secondary { (primary, secondary) } else { (secondary, primary) };
                state.viewer_text_selection = Some((s, e));
                state.viewer_text_selection_locked = Some((s, e));
            } else if !ui.input(|i| i.pointer.primary_down()) {
                state.viewer_text_selection = None;
            }
        }

        // ── Style under the caret, for the inspector's selection-aware tools ──
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content {
            let cursor_byte = char_to_byte(&state.viewer_text_edit_buffer, state.viewer_text_cursor);
            let check_byte = cursor_byte.saturating_sub(1);
            let run = tb.style_runs.iter().find(|r| check_byte >= r.start && check_byte < r.end);
            state.viewer_text_cursor_font_size = Some(run.and_then(|r| r.font_size).unwrap_or(tb.font_size));
            state.viewer_text_cursor_color = Some(run.and_then(|r| r.color).unwrap_or(tb.color));
        }

        // ── Paint: box, selection, caret, badge ──
        let painter = ui.painter_at(viewer_rect);
        painter.rect_stroke(bb.expand(4.0), crate::theme::radius::CONTROL, egui::Stroke::new(1.5_f32, theme.accent));
        let positions = &state.viewer_char_positions;
        if let Some((s, e)) = state.viewer_text_selection {
            let fill = egui::Color32::from_rgba_unmultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 90);
            for cp in positions.iter().filter(|cp| cp.char_index >= s && cp.char_index < e) {
                let r = egui::Rect::from_min_max(to_screen(cp.x, cp.y), to_screen(cp.x + cp.advance, cp.y + cp.height));
                painter.rect_filled(r, 0.0, fill);
            }
        }
        let caret = {
            let c = state.viewer_text_cursor;
            if let Some(cp) = positions.iter().find(|cp| cp.char_index == c) {
                Some((cp.x, cp.y, cp.height))
            } else {
                positions.last().map(|l| (l.x + l.advance, l.y, l.height))
            }
        };
        if let Some((x, y, h)) = caret {
            let t = ui.input(|i| i.time);
            let since = t - state.viewer_text_cursor_blink_time;
            let on = since < 0.05 || (since * 2.0).fract() < 0.5;
            if on {
                painter.line_segment([to_screen(x, y), to_screen(x, y + h)], egui::Stroke::new(2.0_f32, theme.text_primary));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        } else if positions.is_empty() {
            // Nothing laid out yet (empty text, or the boxes still
            // coming): a caret at the box centre so the editor still
            // reads as live.
            painter.line_segment(
                [egui::pos2(bb.center().x, bb.min.y + 4.0), egui::pos2(bb.center().x, bb.max.y - 4.0)],
                egui::Stroke::new(2.0_f32, theme.text_primary),
            );
        }
        {
            let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
            let galley = painter.layout_no_wrap("Editing \u{2014} Esc to finish".to_string(), font, theme.text_primary);
            let pad = egui::vec2(8.0, 3.0);
            let size = galley.size() + pad * 2.0;
            let x = bb.min.x.max(image_rect.min.x + 2.0);
            let y = (bb.min.y - 8.0 - size.y).max(image_rect.min.y + 2.0);
            let r = egui::Rect::from_min_size(egui::pos2(x, y), size);
            painter.rect_filled(r, crate::theme::radius::CONTROL, theme.bg_panel);
            painter.rect_stroke(r, crate::theme::radius::CONTROL, egui::Stroke::new(1.0_f32, theme.border));
            painter.galley(r.min + pad, galley, theme.text_primary);
        }

        // ── Commit typing ──
        if state.viewer_text_edit_buffer != prev_buf {
            state.viewer_char_positions.clear();
            cmds.push(AppCommand::SetTitleObjectText { clip_id, object_id: obj.id.0, text: state.viewer_text_edit_buffer.clone() });
        }

        // ── Leave: Esc, or a click elsewhere in the viewer ──
        let exit_escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let click_outside = ui.input(|i| i.pointer.primary_clicked())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pos| !bb.expand(8.0).contains(pos) && viewer_rect.contains(pos));
        if exit_escape || click_outside {
            cmds.push(AppCommand::EndTitleEdit);
            state.viewer_text_edit_buffer.clear();
            state.viewer_text_selection = None;
            state.viewer_text_selection_locked = None;
            state.viewer_text_cursor = 0;
            state.viewer_char_positions.clear();
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_object = None;
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
            let image_id = self.gpu_frame_live
                .then_some(self.gpu_texture_id)
                .flatten()
                .or_else(|| self.video_texture.as_ref().map(|t| t.id()));
            if let Some(id) = image_id {
                // `painter_at(viewer_rect)` clips to the viewer's panel
                // so high-zoom output stays inside it instead of spilling
                // into the chrome row / neighbouring panels.
                ui.painter_at(viewer_rect).image(
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
                ui.painter_at(viewer_rect).image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        self.handle_text_edit_entry(ui, state, &frame, cmds);
        self.handle_text_edit_exit(ui, state, cmds);
        self.draw_text_edit_widget(ui, state, theme, &frame, cmds);

        // ── On-screen transform controls ──
        // Suppress transform controls during text edit mode
        if state.show_transform_controls && !state.viewer_text_edit && state.eyedropper_mode == crate::state::EyedropperMode::None && !state.has_modal_open() {
            self.draw_transform_controls(ui, state, image_rect, video_w, video_h, cmds);
        }

        // ── On-canvas Title-object editor ──
        // Active for any selected Title clip, regardless of
        // `show_transform_controls`: title objects have their own
        // editing affordance (bounding boxes + handles) that's
        // visually distinct from the clip-level transform handles.
        // Suppressed during text-edit mode and other modal flows.
        if !state.viewer_text_edit
            && state.eyedropper_mode == crate::state::EyedropperMode::None
            && !state.has_modal_open()
        {
            self.draw_title_object_editor(ui, state, image_rect, cmds);
            // Hyperzoom focal-point handle. Reads/writes the same
            // egui-memory scratch that the inspector's HYPERZOOM
            // section spinboxes use, so dragging here updates the
            // spinboxes live and editing the spinboxes moves the
            // crosshair on next paint. Skipped on title-text-edit /
            // eyedropper / modal flows for the same reasons as the
            // title-object editor above.
            self.draw_hyperzoom_focal_handle(ui, state, image_rect);
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
        self.handle_ai_tools_capture(ui, state, image_rect);
        self.draw_ai_tools_overlay(ui, state, image_rect);

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
        self.draw_hdr_monitor(ui, state, theme, image_rect, cmds);
    }

    /// HDR monitoring: what the viewer shows for an HDR delivery, and the
    /// light under the pointer.
    ///
    /// An SDR screen cannot show an HDR picture, so the numbers are the
    /// instrument: the readout says what the picture is in cd/m² at the
    /// pointer, and false colour says it for the whole frame at once. The
    /// levels to judge against are 26 (an 18% grey card), 203 (diffuse
    /// white) and 1000 (the master's peak).
    /// Open the HDR reference-output window.
    ///
    /// Looked for beside the running executable first, because that is
    /// where a built or installed Klipp keeps its own binaries, then on
    /// `PATH`. If neither has it the output still turns *on* — publishing
    /// is the engine's job and works regardless — so someone running the
    /// presenter themselves is not blocked by this convenience failing.
    fn open_hdr_output_window(&mut self) {
        self.hdr_output_unopened = false;
        if self.hdr_output_child.is_some() {
            return;
        }
        const BIN: &str = "klipp-hdr-out";
        let beside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(BIN)))
            .filter(|p| p.is_file());
        let program = beside.unwrap_or_else(|| std::path::PathBuf::from(BIN));
        match std::process::Command::new(&program).spawn() {
            Ok(child) => {
                tracing::info!("HDR reference output: opened {}", program.display());
                self.hdr_output_child = Some(child);
            }
            Err(e) => {
                // Naming the build command, because the likeliest reason
                // is that only `klipp-app` was built: the presenter is a
                // separate binary and `cargo build -p klipp-app` does not
                // produce it.
                tracing::warn!(
                    "could not open {} ({e}). The picture is still being published — build \
                     the window with `cargo build --release -p klipp-hdr-out` and run it, or \
                     run an installed `klipp-hdr-out`.",
                    program.display(),
                );
                // Said on the button, not only logged: the output *is* on
                // and publishing, and the only thing missing is the window.
                self.hdr_output_unopened = true;
            }
        }
    }

    /// Close the window this panel opened, if it opened one.
    fn close_hdr_output_window(&mut self) {
        if let Some(mut child) = self.hdr_output_child.take() {
            // Asked to go, then reaped: without the wait it becomes a
            // zombie for as long as Klipp runs.
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("HDR reference output: closed the window");
        }
    }

    fn draw_hdr_monitor(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        use klipp_core::HdrMonitor;
        const H: f32 = 22.0;
        let label = match state.hdr_monitor {
            HdrMonitor::Off => "HDR: off",
            HdrMonitor::Preview => "HDR: preview",
            HdrMonitor::FalseColour => "HDR: false colour",
        };
        let width = if state.hdr_monitor == HdrMonitor::Off { 78.0 } else { 118.0 };
        let btn = egui::Rect::from_min_size(
            egui::pos2(image_rect.max.x - width - 8.0, image_rect.min.y + 8.0),
            egui::vec2(width, H),
        );
        let resp = ui.interact(btn, ui.id().with("hdr_monitor"), egui::Sense::click());
        let on = state.hdr_monitor.renders_hdr();
        let bg = if on { theme.accent } else { theme.bg_raised.gamma_multiply(0.9) };
        ui.painter().rect_filled(btn, 4.0, bg);
        ui.painter().text(
            btn.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            if on { theme.text_on_accent } else { theme.text_secondary },
        );
        if resp.clicked() {
            // Off → preview → false colour → off.
            state.hdr_monitor = match state.hdr_monitor {
                HdrMonitor::Off => HdrMonitor::Preview,
                HdrMonitor::Preview => HdrMonitor::FalseColour,
                HdrMonitor::FalseColour => HdrMonitor::Off,
            };
            cmds.push(AppCommand::SetHdrMonitor(state.hdr_monitor));
        }
        resp.on_hover_text(
            "What the viewer shows for an HDR delivery. Preview: the HDR picture as SDR, \
             highlights rolled off. False colour: the levels as flat colours — grey 26, \
             white 203, peak 1000 cd/m². The readout below gives the light under the pointer.",
        );

        // The reference output, and only while monitoring is on: the engine
        // publishes the display light the render produces, and it only
        // produces display light for monitoring. Offering the toggle when
        // nothing would be published would be a switch that does nothing.
        if on {
            let out = egui::Rect::from_min_size(
                egui::pos2(btn.min.x, btn.max.y + 4.0),
                egui::vec2(width, H),
            );
            let resp = ui.interact(out, ui.id().with("hdr_reference_output"), egui::Sense::click());
            let live = state.hdr_reference_output;
            let bg = if live { theme.accent } else { theme.bg_raised.gamma_multiply(0.9) };
            ui.painter().rect_filled(out, 4.0, bg);
            ui.painter().text(
                out.center(),
                egui::Align2::CENTER_CENTER,
                match (live, self.hdr_output_unopened) {
                    (true, true) => "Ref out: no window",
                    (true, false) => "Ref out: on",
                    (false, _) => "Ref out: off",
                },
                egui::FontId::proportional(11.0),
                if live { theme.text_on_accent } else { theme.text_secondary },
            );
            if resp.clicked() {
                state.hdr_reference_output = !state.hdr_reference_output;
                cmds.push(AppCommand::SetHdrReferenceOutput(state.hdr_reference_output));
                if state.hdr_reference_output {
                    self.open_hdr_output_window();
                } else {
                    self.close_hdr_output_window();
                    self.hdr_output_unopened = false;
                }
            }
            resp.on_hover_text(
                "Send the HDR picture to a separate window whose surface can \
                 carry it: linear, sixteen bits, highlights above reference \
                 white intact. That window is where an HDR reference monitor \
                 should look — this one is showing you an SDR stand-in for \
                 it.\n\nThe display has to be in HDR too for the picture to \
                 arrive as HDR; on Hyprland that is the monitor's `cm` and \
                 `bitdepth, 10`.\n\nCosts a frame readback, so playback is \
                 slower while it is on.",
            );
        }

        // The readout: the picture's light where the pointer is.
        if on {
            let text = match state.hdr_nits_under_pointer {
                Some(nits) if nits >= 100.0 => format!("{nits:.0} cd/m²"),
                Some(nits) if nits >= 10.0 => format!("{nits:.1} cd/m²"),
                Some(nits) => format!("{nits:.2} cd/m²"),
                None => "— cd/m²".to_string(),
            };
            let row = egui::Rect::from_min_size(
                egui::pos2(btn.min.x, btn.max.y + 8.0 + H),
                egui::vec2(width, H),
            );
            ui.painter().rect_filled(row, 4.0, theme.bg_raised.gamma_multiply(0.9));
            ui.painter().text(
                row.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::monospace(11.0),
                theme.text_primary,
            );
        }
    }

    fn draw_scrub_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
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

        // The source clip's marks: favourite stretches in green and
        // rejected ones in red along the track, notes as ticks above it.
        if viewer_kind == ViewerMode::Source {
            if let Some(media) = state.source_media_id.and_then(|mid| state.media_pool.get(&mid)) {
                let dur = media.duration.to_secs_f64();
                if dur > 0.0 {
                    let x_of = |secs: f64| track_rect.min.x + (secs / dur).clamp(0.0, 1.0) as f32 * track_rect.width();
                    for m in &media.markers {
                        match m.kind {
                            klipp_core::MediaMarkerKind::Note => {
                                let x = x_of(m.secs);
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_max(egui::pos2(x - 1.0, track_rect.min.y - 6.0), egui::pos2(x + 1.0, track_rect.max.y)),
                                    0.0,
                                    egui::Color32::from_rgb(90, 160, 255),
                                );
                            }
                            klipp_core::MediaMarkerKind::Analysis => {
                                // Analysis marks are quieter than the editor's: a
                                // thin grey line for a cut, a faint band for a silence.
                                let colour = egui::Color32::from_rgba_unmultiplied(170, 180, 200, 110);
                                if m.is_range() {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_max(egui::pos2(x_of(m.secs), track_rect.min.y + 1.0), egui::pos2(x_of(m.end_secs()), track_rect.max.y - 1.0)),
                                        0.0,
                                        colour,
                                    );
                                } else {
                                    let x = x_of(m.secs);
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_max(egui::pos2(x - 0.5, track_rect.min.y - 4.0), egui::pos2(x + 0.5, track_rect.max.y)),
                                        0.0,
                                        colour,
                                    );
                                }
                            }
                            kind => {
                                let colour = if kind == klipp_core::MediaMarkerKind::Favourite {
                                    egui::Color32::from_rgba_unmultiplied(80, 200, 120, 170)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(220, 70, 70, 170)
                                };
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_max(egui::pos2(x_of(m.secs), track_rect.min.y - 2.0), egui::pos2(x_of(m.end_secs()), track_rect.max.y + 2.0)),
                                    1.0,
                                    colour,
                                );
                            }
                        }
                    }
                }
            }
        }

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
        let ph = theme.playhead;
        ui.painter().circle_filled(head_center, 7.0, egui::Color32::from_rgba_unmultiplied(ph.r(), ph.g(), ph.b(), 60));
        ui.painter().circle_filled(head_center, 5.0, ph);

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
                    // Instant visual feedback: paint the playhead at
                    // the cursor position right now, on this frame,
                    // instead of waiting for the engine seek to ack.
                    // Without this, the yellow blob lags the cursor
                    // by the seek latency and scrubbing feels
                    // disconnected from the input.
                    match viewer_kind {
                        ViewerMode::Source => {
                            state.source_playhead = target_tc;
                        }
                        ViewerMode::Program => {
                            state.playhead = target_tc;
                            state.playhead_smooth_secs = target_secs;
                            state.playhead_anchor = target_tc;
                            state.playhead_anchor_wall = std::time::Instant::now();
                        }
                    }
                    // Throttle engine dispatch to ~30 Hz. UI repaints
                    // at 60+ Hz on drag — sending a SeekSource per
                    // frame piles 30+ events on the engine in a
                    // single second; even with command coalescing
                    // each one keeps the decoder thread spinning on
                    // demux + BSF work and the fan winds up. 33 ms
                    // is below the human-perception floor for scrub
                    // smoothness so we lose nothing visually. The
                    // final position on drag-release always lands
                    // (gated below).
                    let now = std::time::Instant::now();
                    let throttled = match viewer_kind {
                        ViewerMode::Source => state
                            .source_seek_dispatch_at
                            .map(|t| now.duration_since(t) < std::time::Duration::from_millis(33))
                            .unwrap_or(false),
                        ViewerMode::Program => false,
                    };
                    let drag_release = resp.drag_stopped();
                    if !throttled || drag_release || resp.clicked() {
                        let cmd = match viewer_kind {
                            ViewerMode::Program => AppCommand::SeekTo(target_tc),
                            ViewerMode::Source => {
                                state.source_seek_dispatch_at = Some(now);
                                AppCommand::SeekSource(target_tc)
                            }
                        };
                        cmds.push(cmd);
                    }
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
        // Per-viewer enable. The Source playbar gates on "is a
        // clip loaded into the source viewer?" because
        // `state.source_playback` only transitions to Playing /
        // Paused — it stays Idle until the source engine has
        // played at least once, which the user can't trigger if
        // the button itself is disabled (chicken-and-egg). The
        // Program playbar keeps the existing "engine non-Idle"
        // gate, which the timeline engine sets to Stopped when a
        // project loads.
        let enabled = match viewer_kind {
            ViewerMode::Source => state.source_media_id.is_some(),
            ViewerMode::Program => state.playback != PlaybackState::Idle,
        };
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
                egui::FontId::monospace(13.0),
                theme.text_primary,
            );
            if tc_resp.clicked() {
                state.editing_timecode = true;
                state.timecode_input = tc_str;
            }
        }

        // ── Single viewer: which one this is, and the way to the other ──
        if !state.viewers_side_by_side() && !state.single_viewer_is_program() {
            let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
            let labels = ["Source", "Program"];
            let h = crate::theme::control::COMPACT;
            let size = crate::widgets::segmented::segmented_size(ui, &labels, &font, h);
            let sw = egui::Rect::from_min_size(egui::pos2(rect.min.x + 126.0, cy - h / 2.0), size);
            let active = if viewer_kind == ViewerMode::Source { 0 } else { 1 };
            if let Some(i) = crate::widgets::segmented::segmented_in(ui, sw, &labels, active, theme, font) {
                if i != active {
                    cmds.push(AppCommand::SetViewerMode(if i == 0 { ViewerMode::Source } else { ViewerMode::Program }));
                }
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
            // No block of colour under the play glyph: the glyph is the
            // control, lit when it can be pressed, the field only under
            // the pointer.
            if enabled && resp.hovered() {
                ui.painter().rect_filled(play_rect, crate::theme::radius::CONTROL, theme.bg_hover);
            }
            let play_icon = if is_playing { Icon::Pause } else { Icon::Play };
            let icon_rect = egui::Rect::from_center_size(play_rect.center(), egui::vec2(18.0, 18.0));
            let tint = if enabled { theme.text_primary } else { theme.text_dim };
            play_icon.image(18.0).tint(tint).paint_at(ui, icon_rect);
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

        // ── Right side controls ── (responsive)
        //
        // Layout right-to-left from `rect.max.x - 12`. Each control consumes
        // its slot width plus a 6-px gap before the next leftward control.
        // When the chrome rect is narrow (e.g. portrait-project viewers,
        // ~470 px wide on a 1080p monitor) we'd otherwise march straight
        // into the center transport group and overlap. So we keep controls
        // in this priority order (most-essential first; the rest drop when
        // the budget runs out):
        //   1. Mute toggle               — audio is global, always reachable
        //   2. Match-frame button        — pro NLE workflow control, kept
        //                                  on both Source and Program sides
        //   3. Volume slider             — fine adjust
        //   4. Volume percentage         — cosmetic readout
        //   5. Transform toggle (Program)
        //   6. Camera/extract (Program)
        let center_half_w = total_w / 2.0;
        let center_pad = 8.0; // breathing room between center group + side group
        let right_margin = 12.0;
        let budget_right = (rect.width() / 2.0 - center_half_w - center_pad - right_margin).max(0.0);

        let btn_w = 20.0;
        let gap = 6.0;
        let mute_slot = btn_w + gap;
        let mf_slot = btn_w + gap;
        let slider_slot = 60.0 + gap;
        let vol_pct_slot = 36.0 + gap;
        let xform_slot = btn_w + gap;
        let cam_slot = btn_w + gap;
        let scopes_slot = btn_w + gap;
        let layout_slot = btn_w + gap;

        let mut remaining = budget_right;
        let mut show_mute = false;
        let mut show_mf = false;
        let mut show_slider = false;
        let mut show_vol_pct = false;
        let mut show_xform = false;
        let mut show_camera = false;
        let mut show_scopes = false;
        let mut show_layout = false;
        if remaining >= mute_slot { remaining -= mute_slot; show_mute = true; }
        if remaining >= mf_slot { remaining -= mf_slot; show_mf = true; }
        if viewer_kind == ViewerMode::Program {
            // Scopes and the viewer layout are the picture's own
            // controls; they outrank the volume readout for room.
            if remaining >= scopes_slot { remaining -= scopes_slot; show_scopes = true; }
            if remaining >= layout_slot { remaining -= layout_slot; show_layout = true; }
        }
        if remaining >= slider_slot { remaining -= slider_slot; show_slider = true; }
        if remaining >= vol_pct_slot { remaining -= vol_pct_slot; show_vol_pct = true; }
        if viewer_kind == ViewerMode::Program {
            if remaining >= xform_slot { remaining -= xform_slot; show_xform = true; }
            if remaining >= cam_slot { remaining -= cam_slot; show_camera = true; }
        }
        let _ = remaining; // suppress unused warning past the last conditional

        // Walking cursor: the right edge of the next control we'll place
        // (each placement also consumes a 6-px gap before the one to its
        // left). Starts at the panel's right margin.
        let mut cursor_right = rect.max.x - right_margin;

        // Volume percentage — fixed 36px slot at the far right.
        if show_vol_pct {
            let vol_pct = (state.master_volume * 100.0) as u32;
            let vol_text = format!("{}%", vol_pct);
            ui.painter().text(
                egui::pos2(cursor_right, cy),
                egui::Align2::RIGHT_CENTER,
                &vol_text,
                egui::FontId::monospace(10.0),
                theme.text_dim,
            );
            cursor_right -= 36.0 + gap;
        }

        // Volume slider — 60px wide. When vol_pct is hidden, the slider
        // shifts right to meet the panel edge.
        //
        // CRITICAL: egui::Slider's track width is governed by `spacing.slider_width`,
        // NOT by the parent UI's max_rect or by `add_sized`. The slider widget
        // internally calls `ui.allocate_response(vec2(spacing.slider_width, ...))`
        // which ignores both the child UI's bounds and any size hint passed to
        // add_sized. The ONLY way to make the slider narrower is to mutate
        // `spacing_mut().slider_width` on the UI before adding the slider.
        if show_slider {
            let slider_w = 60.0_f32;
            let slider_h = 16.0_f32;
            let slider_rect = egui::Rect::from_min_size(
                egui::pos2(cursor_right - slider_w, cy - slider_h * 0.5),
                egui::vec2(slider_w, slider_h),
            );
            let mut vol = state.master_volume;
            let mut slider_ui = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
            slider_ui.spacing_mut().slider_width = slider_w;
            let slider = egui::Slider::new(&mut vol, 0.0..=1.3).show_value(false);
            if slider_ui.add(slider).changed() {
                cmds.push(AppCommand::SetMasterVolume(vol));
            }
            cursor_right -= slider_w + gap;
        }

        // Mute button — always shown when any right-side chrome fits.
        if show_mute {
            let mute_x = cursor_right - btn_w / 2.0;
            let mute_rect = egui::Rect::from_center_size(
                egui::pos2(mute_x, cy),
                egui::vec2(btn_w, btn_w),
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
            cursor_right -= btn_w + gap;
        }

        // Match Frame — appears on both viewers in the same slot;
        // direction is determined by which viewer hosts the button
        // (Resolve / Premiere convention). The Program viewer's
        // button finds the source frame for the current program
        // playhead and loads it in the source viewer. The Source
        // viewer's button jumps the program playhead to the next
        // timeline occurrence of the source frame.
        if show_mf {
            let mf_x = cursor_right - btn_w / 2.0;
            let (mf_cmd, mf_enabled, mf_tooltip) = match viewer_kind {
                ViewerMode::Program => (
                    AppCommand::MatchFrameProgramToSource,
                    true, // playhead-resolution happens at handler time; toast on miss
                    "Match Frame — load source for program frame (F)",
                ),
                ViewerMode::Source => (
                    AppCommand::MatchFrameSourceToProgram,
                    state.source_media_id.is_some(),
                    "Match Frame — jump timeline to source frame (Shift+F)",
                ),
            };
            let mf_rect = egui::Rect::from_center_size(
                egui::pos2(mf_x, cy),
                egui::vec2(btn_w, btn_w),
            );
            let mf_resp = ui.allocate_rect(
                mf_rect,
                if mf_enabled { egui::Sense::click() } else { egui::Sense::hover() },
            );
            let mf_color = if !mf_enabled {
                theme.text_dim
            } else if mf_resp.hovered() {
                theme.accent
            } else {
                theme.text_secondary
            };
            let mf_icon_rect = egui::Rect::from_center_size(mf_rect.center(), egui::vec2(14.0, 14.0));
            Icon::FrameCorners.image(14.0).tint(mf_color).paint_at(ui, mf_icon_rect);
            let mf_clicked = mf_resp.clicked();
            mf_resp.on_hover_text(mf_tooltip);
            if mf_clicked && mf_enabled {
                cmds.push(mf_cmd);
            }
            cursor_right -= btn_w + gap;
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
            if show_xform {
                let xform_x = cursor_right - btn_w / 2.0;
                let xform_rect = egui::Rect::from_center_size(
                    egui::pos2(xform_x, cy),
                    egui::vec2(btn_w, btn_w),
                );
                let xform_resp = ui.allocate_rect(xform_rect, egui::Sense::click());
                let xform_color = if !xform_enabled {
                    theme.text_dim
                } else if state.show_transform_controls {
                    theme.accent
                } else {
                    theme.text_secondary
                };
                let stroke_w = if xform_enabled { 1.5_f32 } else { 1.2_f32 };
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
                cursor_right -= btn_w + gap;
            } else {
                let _ = xform_enabled;
            }

            if show_camera {
                let cam_x = cursor_right - btn_w / 2.0;
                let cam_rect = egui::Rect::from_center_size(
                    egui::pos2(cam_x, cy),
                    egui::vec2(btn_w, btn_w),
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
                cursor_right -= btn_w + gap;
            }

            // Scopes: the dock in Color, the floating window elsewhere.
            if show_scopes {
                let r = egui::Rect::from_center_size(egui::pos2(cursor_right - btn_w / 2.0, cy), egui::vec2(btn_w, btn_w));
                let resp = ui.allocate_rect(r, egui::Sense::click());
                let on = state.scopes_shown();
                let colour = if on { theme.accent } else if resp.hovered() { theme.text_primary } else { theme.text_secondary };
                if on {
                    ui.painter().rect_filled(r, crate::theme::radius::CONTROL, theme.accent_subtle);
                }
                crate::panels::toolbar::draw_scopes_icon(ui.painter(), r, colour);
                if resp.clicked() {
                    state.toggle_scopes();
                }
                resp.on_hover_text("Scopes (Alt+V)");
                cursor_right -= btn_w + gap;
            }

            // One viewer or two.
            if show_layout {
                let r = egui::Rect::from_center_size(egui::pos2(cursor_right - btn_w / 2.0, cy), egui::vec2(btn_w, btn_w));
                let resp = ui.allocate_rect(r, egui::Sense::click());
                let colour = if resp.hovered() { theme.text_primary } else { theme.text_secondary };
                let dual = state.viewers_side_by_side();
                draw_viewer_layout_icon(ui.painter(), r, dual, colour);
                if resp.clicked() {
                    state.toggle_viewer_layout();
                }
                resp.on_hover_text(if dual { "Single viewer" } else { "Dual viewers" });
                cursor_right -= btn_w + gap;
            }
        }
        let _ = cursor_right;
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
        icon.image(14.0).tint(if enabled { fg } else { disabled_fg }).paint_at(ui, icon_rect);
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

        let hw_label = match state.perf_hw_decode {
            Some(true) => "NVDEC",
            Some(false) => "SW",
            None => "—", // no decoder open right now
        };
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
        // Decode and post-process run on separate threads (pipelined),
        // so frame pacing is bound by the slowest stage, not the sum.
        // Decode and post-process run on separate threads (pipelined), so
        // pacing is bound by the slowest stage. The readback is engine-side
        // work that the post-process stage table does not cover, so it adds
        // to that side rather than being max'd against it — leaving it out
        // is what let this line report 5.2 ms while frames took 35 ms.
        let engine_ms = avg.total_ms + state.perf_readback_ms as f32;
        let actual_ms = engine_ms.max(avg.decode_ms);
        let (res_line, fps_line, budget_line) = match state.perf_latest_sample {
            Some(ref s) => (
                format!("Frame:  {}x{}", s.width, s.height),
                {
                    // The number that was missing. A target with no
                    // achieved figure beside it reads as a promise.
                    let got = state.perf_video_fps as f32;
                    let short = s.target_fps > 0.0 && got > 0.0 && got < s.target_fps * 0.9;
                    format!(
                        "Target: {:.1} fps   actual {:.1} fps{}",
                        s.target_fps,
                        got,
                        if short { "  ⚠ BELOW TARGET" } else { "" },
                    )
                },
                match s.frame_budget_ms() {
                    Some(budget) => {
                        format!(
                            "Budget: {:.1} ms  actual {:.1} ms",
                            budget, actual_ms,
                        )
                    }
                    None => format!("Budget: — ms  actual {:.1} ms", actual_ms),
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
                "  decode      {:.2} ms",
                avg.decode_ms,
            ),
            format!(
                "  readback    {:.2} ms",
                state.perf_readback_ms,
            ),
            format!(
                "  seq read    {:.2} ms",
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
            // Wrap-stage timings — populated by `update_wrap_stages`.
            // bridge u8 / reencode only run when paused, scrubbing, or
            // comparing (the pregrade encode is skipped during forward
            // playback), and scope only when a scope is open — so 0.00
            // during plain playback is expected, not a bug.
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
                "Drops:  {} in last 1s · {} total (frames never produced are not drops)",
                state.perf_drops_last_sec, state.perf_dropped_frames,
            ),
            format!("Frames: {}", state.perf_frame_count),
            {
                // top-style convention: 100% = one core fully busy.
                // Spell it out so 858% doesn't read as nonsense.
                static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
                let cores = *CORES.get_or_init(|| {
                    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
                });
                format!(
                    "CPU:    {:.0}% ({:.1} of {} cores)",
                    state.perf_cpu_percent,
                    state.perf_cpu_percent / 100.0,
                    cores,
                )
            },
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
            .map(|b| actual_ms > b)
            .unwrap_or(false);
        for (i, line) in lines.iter().enumerate() {
            let _ = i;
            // One arm per status line. Two arms share a colour, which
            // is coincidence, not a case to merge.
            #[allow(clippy::if_same_then_else)]
            let color = if line.starts_with("● REC") {
                theme.marker_error
            } else if line.starts_with("Decode:") && state.perf_hw_decode == Some(true) {
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
            ui.painter().rect_filled(label_bg, 2.0, egui::Color32::from_rgba_unmultiplied(
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
                            egui::FontId::proportional(10.0),
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
        let outline_color = ui.visuals().selection.stroke.color;
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
    /// On-canvas Title-object editor: bounding boxes + 8 resize
    /// handles + rotation handle for the active title's objects.
    /// Pointer behaviour:
    ///
    /// - Click on an object's body → select that object.
    /// - Click on a handle of the selected object → start drag.
    /// - Drag body → translate `transform.center`.
    /// - Drag corner handle → uniform scale (resize box AND
    ///   the TextBox's font_size, pro-NLE convention).
    /// - Drag edge handle → resize the box on that axis only.
    /// - Drag rotation handle → rotate around `transform.anchor_norm`.
    /// - Click empty image area → deselect the object (reverts to
    ///   clip-level transform handles).
    ///
    /// Coordinates: object transforms live in normalised 0..1 frame
    /// space; the editor maps to/from `image_rect` (the rendered
    /// video's on-screen rect) for hit-testing and painting.
    fn draw_title_object_editor(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };
        // Only operate on Title clips.
        let title_data = state.sequence().tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
            .and_then(|c| c.title_data())
            .cloned();
        let td = match title_data {
            Some(td) => td,
            None => return,
        };

        // Object-editor colours are deliberately fixed (not theme-
        // The theme's accent, as every selection in Klipp is drawn.
        let accent = ui.visuals().selection.stroke.color;
        let inactive_stroke = egui::Color32::from_rgba_unmultiplied(220, 220, 230, 180);
        let handle_fill = egui::Color32::from_rgb(20, 20, 24);
        let painter = ui.painter_at(image_rect);

        // Local closures bridging to the file-level viewer-geom
        // helpers — keeps the rest of `draw_title_object_editor`'s
        // body byte-identical (avoids touching every `to_screen` /
        // `to_norm` site for a no-behaviour-change refactor).
        let to_screen = |nx: f32, ny: f32| viewer_norm_to_screen(image_rect, nx, ny);
        let to_norm = |p: egui::Pos2| viewer_screen_to_norm(image_rect, p);

        // ── Paint each visible object's bounding box ──
        // Skip the object that's currently in inline-edit mode —
        // its bounding box is drawn by the text-edit widget instead
        // (dashed accent stroke + Esc-to-exit badge), and painting
        // the standard solid box on top would double-stroke.
        let edit_oid = if state.viewer_text_edit {
            state.viewer_text_edit_object
        } else {
            None
        };
        for obj in &td.objects {
            if !obj.visible {
                continue;
            }
            if Some(obj.id) == edit_oid {
                continue;
            }
            let cx = obj.transform.center.x;
            let cy = obj.transform.center.y;
            let half_w = obj.transform.size.x * 0.5;
            let half_h = obj.transform.size.y * 0.5;
            let bb = egui::Rect::from_min_max(
                to_screen(cx - half_w, cy - half_h),
                to_screen(cx + half_w, cy + half_h),
            );
            let is_selected = state.selected_title_object == Some((clip_id, obj.id));
            let stroke = if is_selected {
                egui::Stroke::new(2.0_f32, accent)
            } else {
                egui::Stroke::new(1.0_f32, inactive_stroke)
            };
            painter.rect_stroke(bb, 0.0, stroke);

            if !is_selected {
                continue;
            }

            // Handles: 4 corners + 4 mid-edges + rotation stalk.
            let handle_size = 8.0_f32;
            let handle_stroke = egui::Stroke::new(1.5_f32, accent);
            let handle_rect = |p: egui::Pos2| egui::Rect::from_center_size(p, egui::vec2(handle_size, handle_size));

            for hp in [
                bb.min,
                egui::pos2(bb.max.x, bb.min.y),
                egui::pos2(bb.min.x, bb.max.y),
                bb.max,
                egui::pos2(bb.center().x, bb.min.y),
                egui::pos2(bb.center().x, bb.max.y),
                egui::pos2(bb.min.x, bb.center().y),
                egui::pos2(bb.max.x, bb.center().y),
            ] {
                let r = handle_rect(hp);
                painter.rect_filled(r, 1.5, handle_fill);
                painter.rect_stroke(r, 1.5, handle_stroke);
            }

            // Rotation stalk above the top-centre handle.
            let stalk_top = egui::pos2(bb.center().x, bb.min.y - 24.0);
            painter.line_segment(
                [egui::pos2(bb.center().x, bb.min.y), stalk_top],
                handle_stroke,
            );
            painter.circle_filled(stalk_top, 5.0, accent);
            painter.circle_stroke(
                stalk_top,
                5.0,
                egui::Stroke::new(1.0_f32, handle_fill),
            );

            // Anchor dot — drag-handle for `transform.anchor_norm`.
            // Positioned at the bbox-relative anchor; uses a small
            // crosshair shape so it reads as a separate affordance
            // from the corner / edge resize handles.
            let an_x = bb.min.x + obj.transform.anchor_norm.x * bb.width();
            let an_y = bb.min.y + obj.transform.anchor_norm.y * bb.height();
            let an_p = egui::pos2(an_x, an_y);
            let an_r = 4.5;
            painter.circle_filled(an_p, an_r, handle_fill);
            painter.circle_stroke(an_p, an_r, handle_stroke);
            painter.line_segment(
                [egui::pos2(an_x - an_r - 2.0, an_y), egui::pos2(an_x + an_r + 2.0, an_y)],
                egui::Stroke::new(1.0_f32, accent),
            );
            painter.line_segment(
                [egui::pos2(an_x, an_y - an_r - 2.0), egui::pos2(an_x, an_y + an_r + 2.0)],
                egui::Stroke::new(1.0_f32, accent),
            );
        }

        // ── Pointer interaction ──
        // Allocated invisibly over the image so we can read input
        // without stealing focus from other widgets.
        let resp = ui.interact(image_rect, ui.id().with("title_object_editor"), egui::Sense::click_and_drag());

        // Resolve pointer position once per frame.
        let pointer = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.hover_pos()));

        // ── Drag: ongoing? ──
        if let Some(drag) = state.title_object_drag.clone() {
            if drag.clip_id != clip_id {
                state.title_object_drag = None;
            } else if resp.dragged() {
                if let Some(p) = pointer {
                    let (nx, ny) = to_norm(p);
                    let (start_nx, start_ny) = to_norm(drag.drag_start_pos);
                    let dnx = nx - start_nx;
                    let dny = ny - start_ny;
                    let mut new_t = compute_drag_transform(
                        &drag, dnx, dny,
                        (start_nx, start_ny), (nx, ny),
                    );
                    // Snapping while the body moves: the frame's
                    // centre lines, the title-safe edges and the other
                    // layers' centres. Alt drags free.
                    if drag.handle == crate::state::TitleObjectHandle::Body && !ui.input(|i| i.modifiers.alt) {
                        let snap_x = 6.0 / image_rect.width().max(1.0);
                        let snap_y = 6.0 / image_rect.height().max(1.0);
                        let safe = state.viewer_prefs.title_safe_pct.clamp(0.5, 1.0);
                        let (left, right) = (0.5 - safe * 0.5, 0.5 + safe * 0.5);
                        let (top, bottom) = (0.5 - safe * 0.5, 0.5 + safe * 0.5);
                        let (hw, hh) = (new_t.size.x * 0.5, new_t.size.y * 0.5);
                        let mut guides: Vec<(bool, f32)> = Vec::new();
                        // (value the centre lands on, where the guide is drawn)
                        let mut xs: Vec<(f32, f32)> = vec![(0.5, 0.5), (left + hw, left), (right - hw, right)];
                        let mut ys: Vec<(f32, f32)> = vec![(0.5, 0.5), (top + hh, top), (bottom - hh, bottom)];
                        for o in td.objects.iter().filter(|o| o.id != drag.object_id && o.visible) {
                            xs.push((o.transform.center.x, o.transform.center.x));
                            ys.push((o.transform.center.y, o.transform.center.y));
                        }
                        let nearest = |cands: &[(f32, f32)], v: f32, snap: f32| {
                            cands
                                .iter()
                                .filter(|(c, _)| (v - c).abs() < snap)
                                .min_by(|a, b| (v - a.0).abs().partial_cmp(&(v - b.0).abs()).unwrap_or(std::cmp::Ordering::Equal))
                                .copied()
                        };
                        if let Some((v, g)) = nearest(&xs, new_t.center.x, snap_x) {
                            new_t.center.x = v;
                            guides.push((true, g));
                        }
                        if let Some((v, g)) = nearest(&ys, new_t.center.y, snap_y) {
                            new_t.center.y = v;
                            guides.push((false, g));
                        }
                        let guide_stroke = egui::Stroke::new(1.0_f32, accent);
                        for (vertical, g) in guides {
                            if vertical {
                                let x = to_screen(g, 0.0).x;
                                painter.line_segment([egui::pos2(x, image_rect.min.y), egui::pos2(x, image_rect.max.y)], guide_stroke);
                            } else {
                                let y = to_screen(0.0, g).y;
                                painter.line_segment([egui::pos2(image_rect.min.x, y), egui::pos2(image_rect.max.x, y)], guide_stroke);
                            }
                        }
                    }
                    // The readout: the layer's centre in frame pixels,
                    // beside the pointer.
                    {
                        let res = state.sequence().resolution;
                        let text = format!("x {:.0}  y {:.0}", new_t.center.x * res.width as f32, new_t.center.y * res.height as f32);
                        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(crate::theme::type_scale::CAPTION), egui::Color32::WHITE);
                        let pad = egui::vec2(6.0, 3.0);
                        let size = galley.size() + pad * 2.0;
                        let mut at = p + egui::vec2(14.0, 14.0);
                        if at.x + size.x > image_rect.max.x {
                            at.x = p.x - 14.0 - size.x;
                        }
                        if at.y + size.y > image_rect.max.y {
                            at.y = p.y - 14.0 - size.y;
                        }
                        let r = egui::Rect::from_min_size(at, size);
                        painter.rect_filled(r, 2.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
                        painter.galley(r.min + pad, galley, egui::Color32::WHITE);
                    }
                    let new_font_size = if matches!(
                        drag.handle,
                        crate::state::TitleObjectHandle::CornerTopLeft
                            | crate::state::TitleObjectHandle::CornerTopRight
                            | crate::state::TitleObjectHandle::CornerBottomLeft
                            | crate::state::TitleObjectHandle::CornerBottomRight
                    ) {
                        // Uniform font scale = ratio of new size to
                        // old size on whichever axis grew. Use the
                        // smaller of the two axis scales so corner
                        // drag stays uniform even if the user hasn't
                        // held shift (we treat corner-drag as always
                        // uniform — pro convention).
                        let sx_scale = if drag.initial_transform.size.x.abs() > 0.0001 {
                            new_t.size.x / drag.initial_transform.size.x
                        } else { 1.0 };
                        let sy_scale = if drag.initial_transform.size.y.abs() > 0.0001 {
                            new_t.size.y / drag.initial_transform.size.y
                        } else { 1.0 };
                        let scale = sx_scale.min(sy_scale).max(0.05);
                        Some((drag.initial_font_size * scale).clamp(4.0, 1000.0))
                    } else {
                        None
                    };

                    if let Some(fs) = new_font_size {
                        cmds.push(AppCommand::SetTitleObjectTransformAndFont {
                            clip_id,
                            object_id: drag.object_id.0,
                            center_x: new_t.center.x,
                            center_y: new_t.center.y,
                            size_x: new_t.size.x,
                            size_y: new_t.size.y,
                            rotation: new_t.rotation,
                            anchor_x: new_t.anchor_norm.x,
                            anchor_y: new_t.anchor_norm.y,
                            font_size: fs,
                        });
                    } else {
                        cmds.push(AppCommand::SetTitleObjectTransform {
                            clip_id,
                            object_id: drag.object_id.0,
                            center_x: new_t.center.x,
                            center_y: new_t.center.y,
                            size_x: new_t.size.x,
                            size_y: new_t.size.y,
                            rotation: new_t.rotation,
                            anchor_x: new_t.anchor_norm.x,
                            anchor_y: new_t.anchor_norm.y,
                        });
                    }
                }
            }
            if resp.drag_stopped() {
                state.title_object_drag = None;
            }
            return;
        }

        // ── Drag start: only when no drag is active and the user
        // pressed inside the image. Hit-test in priority order:
        // selected object's handles → any object's body → empty.
        if resp.drag_started() {
            if let Some(p) = pointer {
                if !image_rect.contains(p) {
                    return;
                }
                // 1. If we have a selected object, check its handles.
                let selected = state.selected_title_object
                    .filter(|(c, _)| *c == clip_id)
                    .and_then(|(_, oid)| td.object(oid).cloned());
                if let Some(obj) = &selected {
                    if let Some(handle) = hit_test_handles(obj, image_rect, p) {
                        let initial_font_size = match &obj.content {
                            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.font_size,
                            _ => 0.0,
                        };
                        state.title_object_drag = Some(crate::state::TitleObjectDrag {
                            clip_id,
                            object_id: obj.id,
                            handle,
                            initial_transform: obj.transform.clone(),
                            initial_font_size,
                            drag_start_pos: p,
                        });
                        return;
                    }
                }
                // 2. Hit-test any object's body (top-most first → reverse).
                for obj in td.objects.iter().rev() {
                    if !obj.visible || obj.locked {
                        continue;
                    }
                    let bb = object_screen_bb(obj, image_rect);
                    if bb.contains(p) {
                        // Select this object and start a body drag.
                        cmds.push(AppCommand::SelectTitleObject {
                            clip_id,
                            object_id: Some(obj.id.0),
                        });
                        let initial_font_size = match &obj.content {
                            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.font_size,
                            _ => 0.0,
                        };
                        state.title_object_drag = Some(crate::state::TitleObjectDrag {
                            clip_id,
                            object_id: obj.id,
                            handle: crate::state::TitleObjectHandle::Body,
                            initial_transform: obj.transform.clone(),
                            initial_font_size,
                            drag_start_pos: p,
                        });
                        return;
                    }
                }
                // 3. Empty image area → deselect the object.
                if state.selected_title_object.is_some() {
                    cmds.push(AppCommand::SelectTitleObject {
                        clip_id,
                        object_id: None,
                    });
                }
            }
        } else if resp.clicked() {
            // Single click without drag: same hit-test for selection
            // change (allows clicking to switch between stacked
            // objects without immediately starting a drag).
            if let Some(p) = pointer {
                let mut hit = None;
                for obj in td.objects.iter().rev() {
                    if !obj.visible || obj.locked {
                        continue;
                    }
                    if object_screen_bb(obj, image_rect).contains(p) {
                        hit = Some(obj.id);
                        break;
                    }
                }
                if let Some(id) = hit {
                    if state.selected_title_object != Some((clip_id, id)) {
                        cmds.push(AppCommand::SelectTitleObject {
                            clip_id,
                            object_id: Some(id.0),
                        });
                    }
                } else if state.selected_title_object.is_some() {
                    cmds.push(AppCommand::SelectTitleObject {
                        clip_id,
                        object_id: None,
                    });
                }
            }
        }
    }

    /// Draggable focal-point gizmo on the viewer for the HYPERZOOM
    /// inspector section. Visual language matches the title-object
    /// anchor handle (filled circle + ring + cross) — that's the
    /// established "this is a draggable pivot point" affordance, so
    /// reusing it makes the focal-point handle read instantly. The
    /// scratch state lives in egui memory keyed by clip-id (see
    /// `crate::hyperzoom::hyperzoom_scratch_id`), shared with the
    /// inspector spinboxes — drag here updates the spinboxes live;
    /// type into the spinboxes and the crosshair jumps.
    ///
    /// Skipped when:
    ///
    /// * No clip is selected.
    /// * Selected clip is on a kind that hyperzoom doesn't apply to
    ///   (`AdjustmentLayer` / `Gap` / nested `Sequence`) — these are
    ///   the same kinds the inspector's HYPERZOOM section refuses
    ///   to render for.
    /// * The image rect is degenerate (zero width or height).
    fn draw_hyperzoom_focal_handle(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        let Some(clip_id) = state.selected_clip else { return; };
        if image_rect.width() < 1.0 || image_rect.height() < 1.0 {
            return;
        }
        // Source-of-truth check: the clip must still be in the active
        // sequence and its kind must qualify for hyperzoom.
        let qualifies = state
            .sequence()
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
            .map(|c| {
                !matches!(
                    c.kind,
                    klipp_timeline::ClipKind::AdjustmentLayer
                        | klipp_timeline::ClipKind::Gap
                        | klipp_timeline::ClipKind::Sequence { .. }
                )
            })
            .unwrap_or(false);
        if !qualifies {
            return;
        }

        let scratch = crate::hyperzoom::read_hyperzoom_scratch(ui.ctx(), clip_id);
        let fx = scratch.focal_x.clamp(0.0, 1.0);
        let fy = scratch.focal_y.clamp(0.0, 1.0);

        // Same accent + handle colours as the title-object editor so
        // the two gizmos read as the same visual family.
        let accent = ui.visuals().selection.stroke.color;
        let handle_fill = egui::Color32::from_rgb(20, 20, 24);
        let handle_stroke = egui::Stroke::new(1.5_f32, accent);

        let painter = ui.painter_at(image_rect);
        let centre = viewer_norm_to_screen(image_rect, fx, fy);
        let cx = centre.x;
        let cy = centre.y;
        let r = 5.5_f32;

        // Filled disc + accent ring + cross arms. Slightly bigger
        // than the title-object anchor (4.5 → 5.5) so the focal
        // handle is the more prominent affordance when both are on
        // screen — the focal point is "the thing the user is
        // tuning" while the title anchor is a static reference.
        painter.circle_filled(centre, r, handle_fill);
        painter.circle_stroke(centre, r, handle_stroke);
        let arm = r + 4.0;
        painter.line_segment(
            [egui::pos2(cx - arm, cy), egui::pos2(cx + arm, cy)],
            egui::Stroke::new(1.0_f32, accent),
        );
        painter.line_segment(
            [egui::pos2(cx, cy - arm), egui::pos2(cx, cy + arm)],
            egui::Stroke::new(1.0_f32, accent),
        );

        // Hit test only the handle itself, not the whole image rect —
        // dragging anywhere else on the canvas should still fall
        // through to the title-object editor / transform controls.
        let hit = egui::Rect::from_center_size(centre, egui::vec2(arm * 2.0, arm * 2.0));
        let resp = ui.interact(
            hit,
            ui.id().with(("hyperzoom_focal_handle", clip_id)),
            egui::Sense::click_and_drag(),
        );
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = viewer_screen_to_norm(image_rect, p);
                crate::hyperzoom::update_hyperzoom_scratch(ui.ctx(), clip_id, |s| {
                    s.focal_x = nx.clamp(0.0, 1.0);
                    s.focal_y = ny.clamp(0.0, 1.0);
                });
            }
        }

        // Hover affordance: tiny tooltip that names the value the
        // user is dragging — without it the floating crosshair on a
        // black letterbox is a UFO.
        if resp.hovered() || resp.dragged() {
            resp.on_hover_text(format!(
                "Hyperzoom focal point ({:.3}, {:.3})\nDrag to move; spinboxes in HYPERZOOM stay in sync.",
                fx, fy,
            ));
        }
    }

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

        // Painter for the transform overlay. Sits in `Order::Middle` —
        // above the viewer image (which paints in the central panel
        // at `Order::Background`) but BELOW everything in
        // `Order::Foreground`: context menus, dropdowns, modal
        // dialogs, tooltips. Earlier this layer was on `Foreground`,
        // which made the bounding-box outline + handles bleed
        // through any open clip-context menu (egui's menus also live
        // on `Foreground`, and within the same Order they stack by
        // creation sequence — the per-frame redraw of the overlay
        // ended up on top). `Middle` keeps the overlay visible over
        // the viewer image (the toolbar / transport / panel chrome
        // don't overlap the viewer rect, so they don't visually
        // cover the handles in regular dock layouts) while letting
        // every popup naturally occlude it.
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Middle,
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

    /// Mouse-driven zoom + pan input for a viewer's video area.
    ///
    /// * **Scroll wheel** zooms in/out by a 1.1× step per "tick",
    ///   anchored at the cursor so the image pixel under the cursor
    ///   stays under the cursor. Scrolling out past the fit zoom
    ///   snaps back to Fit and resets the pan so the user can return
    ///   to the default view without manual recentering.
    /// * **Middle-mouse drag** pans when the viewer is zoomed in.
    ///   Disabled in Fit mode (there's nothing to pan).
    ///
    /// Call this once per frame for each viewer's video rect, BEFORE
    /// any other interactive widgets allocate inside the same rect —
    /// it senses only middle-button drag and scroll, so click-to-focus
    /// / transform handles / etc. that consume the left button keep
    /// working unchanged.
    pub fn handle_viewer_zoom_pan_input(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        video_rect: egui::Rect,
        tex_size: egui::Vec2,
        viewer_kind: ViewerMode,
    ) {
        if tex_size.x <= 0.0 || tex_size.y <= 0.0 { return; }
        if video_rect.width() <= 0.0 || video_rect.height() <= 0.0 { return; }

        const ZOOM_MIN: f32 = 0.05;
        const ZOOM_MAX: f32 = 16.0;
        const SCROLL_FACTOR_PER_LINE: f32 = 1.10;

        // `Sense::hover()` claims hover priority for scroll routing
        // without consuming LMB clicks — that way the click-to-focus
        // helper allocated earlier, on-canvas transform handles, the
        // title-object editor, etc. all keep their normal LMB behavior.
        // Middle-mouse pan is driven from raw input state below since
        // `Sense::drag()` would also block LMB-drag gestures on widgets
        // painted into the same rect.
        let id = ui.id().with(match viewer_kind {
            ViewerMode::Source => "source_viewer_zoom_pan",
            ViewerMode::Program => "program_viewer_zoom_pan",
        });
        let resp = ui.interact(video_rect, id, egui::Sense::hover());
        if !resp.hovered() { return; }

        let (cursor_opt, middle_held, middle_delta, scroll_y) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_down(egui::PointerButton::Middle),
                i.pointer.delta(),
                i.smooth_scroll_delta.y,
            )
        });

        let view = state.view_for_mut(viewer_kind);

        // Middle-mouse drag → pan. Only meaningful when zoomed in.
        if view.zoom > 0.0 && middle_held && middle_delta != egui::Vec2::ZERO {
            view.pan += middle_delta;
        }

        if scroll_y.abs() < f32::EPSILON { return; }
        let cursor = match cursor_opt {
            Some(p) => p,
            None => return,
        };

        // The "fit" zoom is the effective scale of the image when in
        // Fit mode. Computed once so scroll-out-to-Fit can snap cleanly.
        let fit_zoom = (video_rect.width() / tex_size.x)
            .min(video_rect.height() / tex_size.y);

        let old_zoom_effective = if view.zoom <= 0.0 { fit_zoom } else { view.zoom };
        // Smooth scrolling reports a continuous delta — we map it onto
        // 1.1× per ~50 px of wheel travel so a single notch on a mouse
        // wheel matches the Resolve / Premiere zoom-step intuition.
        let factor = SCROLL_FACTOR_PER_LINE.powf(scroll_y / 50.0);
        let new_zoom_raw = (old_zoom_effective * factor).clamp(ZOOM_MIN, ZOOM_MAX);

        // Snap-to-Fit: scrolling out below the fit threshold collapses
        // to Fit mode with pan reset. The 0.5 % slop avoids float churn
        // where we'd otherwise tip in and out of Fit on rounding noise.
        if new_zoom_raw <= fit_zoom * 1.005 {
            view.zoom = 0.0;
            view.pan = egui::Vec2::ZERO;
            return;
        }

        // Cursor-anchored zoom: the image pixel under the cursor stays
        // under the cursor. Derived from
        //     pixel_norm = (cursor - image.center)/image.size + 0.5
        //     image.center = viewer.center + pan
        //   ⇒ pan_new = c - (c - pan_old) * (new/old)   where c = cursor - viewer.center
        let c = cursor - video_rect.center();
        let pan_old = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
        let scale = new_zoom_raw / old_zoom_effective;
        let pan_new = c - (c - pan_old) * scale;

        view.zoom = new_zoom_raw;
        view.pan = pan_new;
    }

    /// Floating "viewer options" button anchored over the top-right
    /// corner of the video area, with a popup menu containing the
    /// controls that don't fit in the responsive playbar (volume slider,
    /// volume %, etc.) plus viewer-wide power-user toggles (zoom presets,
    /// safe-area / grid / zebra / false-color / focus-peaking overlays,
    /// match-frame, and — on the Program side — extract-frame and
    /// transform-controls).
    ///
    /// The trigger fades in only when the pointer is inside `video_rect`
    /// or the popup is already open, so it doesn't obscure the picture
    /// during normal viewing. Resolve and Kdenlive use the same pattern.
    pub fn draw_viewer_overflow_menu(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        video_rect: egui::Rect,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        const BTN_SIZE: f32 = 24.0;
        const INSET: f32 = 10.0;

        let trigger_rect = egui::Rect::from_min_size(
            egui::pos2(
                video_rect.max.x - INSET - BTN_SIZE,
                video_rect.min.y + INSET,
            ),
            egui::vec2(BTN_SIZE, BTN_SIZE),
        );

        let popup_id = ui.make_persistent_id(match viewer_kind {
            ViewerMode::Source => "viewer_overflow_menu_source",
            ViewerMode::Program => "viewer_overflow_menu_program",
        });

        let popup_open = ui.memory(|m| m.is_popup_open(popup_id));
        let trigger_visible = popup_open || ui.rect_contains_pointer(video_rect);

        let resp = ui.allocate_rect(trigger_rect, egui::Sense::click());

        if trigger_visible {
            let hovered = resp.hovered();
            let bg_alpha = if hovered { 230 } else { 160 };
            let bg = egui::Color32::from_rgba_unmultiplied(
                theme.bg_panel.r(),
                theme.bg_panel.g(),
                theme.bg_panel.b(),
                bg_alpha,
            );
            let painter = ui.painter();
            painter.rect_filled(trigger_rect, BTN_SIZE * 0.25, bg);
            let stroke_alpha = if hovered { 200 } else { 90 };
            let stroke_col = egui::Color32::from_rgba_unmultiplied(
                theme.text_secondary.r(),
                theme.text_secondary.g(),
                theme.text_secondary.b(),
                stroke_alpha,
            );
            painter.rect_stroke(
                trigger_rect,
                BTN_SIZE * 0.25,
                egui::Stroke::new(1.0_f32, stroke_col),
            );

            // Three vertical dots (kebab).
            let cx = trigger_rect.center().x;
            let cy = trigger_rect.center().y;
            let dot_color = if hovered { theme.text_primary } else { theme.text_secondary };
            let dot_r = 1.6;
            let dot_gap = 4.5;
            painter.circle_filled(egui::pos2(cx, cy - dot_gap), dot_r, dot_color);
            painter.circle_filled(egui::pos2(cx, cy), dot_r, dot_color);
            painter.circle_filled(egui::pos2(cx, cy + dot_gap), dot_r, dot_color);
        }

        let resp = resp.on_hover_text("Viewer options");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(220.0);
                Self::draw_viewer_overflow_menu_content(ui, state, theme, viewer_kind, cmds);
            },
        );
    }

    fn draw_viewer_overflow_menu_content(
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        let section_label = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(text)
                    .size(10.0)
                    .color(theme.text_dim),
            );
        };

        // ── Zoom ──
        //
        // Continuous + / − stepping (× 1.25 per click, clamped to a
        // sensible 5 % – 1600 % range), with a "Fit" toggle next to it
        // for the letterbox-to-viewer default. Zoom of `0.0` is the
        // sentinel for fit mode — anything else is a native-pixel
        // multiplier consumed by the source / program render path.
        const ZOOM_MIN: f32 = 0.05;
        const ZOOM_MAX: f32 = 16.0;
        const ZOOM_STEP: f32 = 1.25;
        section_label(ui, "ZOOM");
        ui.horizontal(|ui| {
            let view = state.view_for(viewer_kind);
            let in_fit = view.zoom <= 0.0;
            let mut fit_btn = egui::Button::new(
                egui::RichText::new("Fit").size(11.0),
            )
            .min_size(egui::vec2(40.0, 22.0));
            if in_fit {
                fit_btn = fit_btn.fill(theme.accent_dim);
            }
            if ui.add(fit_btn).clicked() {
                let v = state.view_for_mut(viewer_kind);
                v.zoom = 0.0;
                v.pan = egui::Vec2::ZERO;
            }

            let minus = egui::Button::new(egui::RichText::new("−").size(13.0))
                .min_size(egui::vec2(22.0, 22.0));
            if ui.add(minus).on_hover_text("Zoom out").clicked() {
                let current = if view.zoom <= 0.0 { 1.0 } else { view.zoom };
                state.view_for_mut(viewer_kind).zoom = (current / ZOOM_STEP).max(ZOOM_MIN);
            }

            let pct_label = if in_fit {
                "Fit".to_string()
            } else {
                format!("{}%", (view.zoom * 100.0).round() as i32)
            };
            ui.add_sized(
                egui::vec2(56.0, 22.0),
                egui::Label::new(
                    egui::RichText::new(pct_label)
                        .monospace()
                        .size(12.0)
                        .color(theme.text_primary),
                ),
            );

            let plus = egui::Button::new(egui::RichText::new("+").size(13.0))
                .min_size(egui::vec2(22.0, 22.0));
            if ui.add(plus).on_hover_text("Zoom in").clicked() {
                let current = if view.zoom <= 0.0 { 1.0 } else { view.zoom };
                state.view_for_mut(viewer_kind).zoom = (current * ZOOM_STEP).min(ZOOM_MAX);
            }
        });

        ui.separator();

        // ── Volume ── (always reachable here so power-users have a
        // home for the slider even when the chrome row drops it.)
        section_label(ui, "VOLUME");
        let mut vol = state.master_volume;
        let vol_label = format!("{}%", (vol * 100.0) as i32);
        let slider = egui::Slider::new(&mut vol, 0.0..=1.3).text(vol_label);
        if ui.add(slider).changed() {
            cmds.push(AppCommand::SetMasterVolume(vol));
        }
        let mute_label = if state.master_muted { "✓ Muted" } else { "  Mute master output" };
        if ui.button(mute_label).clicked() {
            cmds.push(AppCommand::SetMasterMute(!state.master_muted));
        }

        ui.separator();

        // ── Overlays ──
        section_label(ui, "OVERLAYS");
        ui.checkbox(&mut state.viewer_overlays.safe_areas, "Safe areas (Alt+S)");
        ui.checkbox(&mut state.viewer_overlays.grid, "Grid (Alt+G)");
        ui.checkbox(&mut state.viewer_overlays.zebras, "Zebras (Z)");
        ui.checkbox(&mut state.viewer_overlays.false_color, "False colour (Alt+F)");
        ui.checkbox(&mut state.viewer_overlays.focus_peaking, "Focus peaking (Ctrl+P)");
        ui.checkbox(&mut state.viewer_overlays.checkerboard, "Checkerboard");

        ui.separator();

        // ── Layout ──
        section_label(ui, "LAYOUT");
        let dual_label = if state.viewers_side_by_side() { "\u{2714} Dual viewers" } else { "  Dual viewers" };
        if ui.button(dual_label).clicked() {
            state.toggle_viewer_layout();
        }
        let scopes_label = if state.scopes_shown() { "\u{2714} Scopes (Alt+V)" } else { "  Scopes (Alt+V)" };
        if ui.button(scopes_label).clicked() {
            state.toggle_scopes();
        }

        ui.separator();

        // ── Actions ──
        section_label(ui, "ACTIONS");
        match viewer_kind {
            ViewerMode::Source => {
                let enabled = state.source_media_id.is_some();
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new("Match frame → timeline (Shift+F)"),
                    )
                    .clicked()
                {
                    cmds.push(AppCommand::MatchFrameSourceToProgram);
                }
            }
            ViewerMode::Program => {
                if ui.button("Match frame ← source (F)").clicked() {
                    cmds.push(AppCommand::MatchFrameProgramToSource);
                }
                if ui.button("Extract frame to bin (Ctrl+Alt+B)").clicked() {
                    cmds.push(AppCommand::ExtractFrameToBin);
                }
                if ui.button("Extract frame to file… (Ctrl+Alt+S)").clicked() {
                    cmds.push(AppCommand::FrameGrab);
                }
                let xform_label = if state.show_transform_controls {
                    "✓ Transform controls"
                } else {
                    "  Transform controls"
                };
                if ui.button(xform_label).clicked() {
                    state.show_transform_controls = !state.show_transform_controls;
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
        // Live texture (engine-decoded) always wins over the cold-
        // start thumbnail. Previously we deferred to the thumbnail
        // when paused because the engine seek was 500 ms-per-frame
        // and the thumbnail decoder was the only thing fast enough
        // to follow a drag — that's no longer true: seek_source
        // now dispatches the seek without blocking, and the per-
        // tick paused-drain pumps the resulting frame into
        // `source_live_texture` within ~30 ms. Showing the stale
        // thumbnail during scrub was why "frames don't follow the
        // progressbar." Browser skim hover overlay still wins on
        // top of everything.
        // No branch on playback state: as the note above says, the live
        // texture always wins now. The `if playing { .. } else { .. }` this
        // replaces had identical arms — a leftover from when paused
        // deferred to the thumbnail, and misleading to read as though the
        // playback state still mattered here.
        let playback_tex = live_tex.or(committed_tex);
        let active_tex = state.browser_skim_texture.as_ref().or(playback_tex);
        if let Some(tex) = active_tex {
            let tex_size = tex.size_vec2();
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = viewer_rect.width() / viewer_rect.height();
                // Same zoom semantics as the Program viewer: 0.0 = fit,
                // any other value is a multiplier on native source pixels.
                let view = state.source_view;
                let (dw, dh) = if view.zoom <= 0.0 {
                    if video_aspect > panel_aspect {
                        (viewer_rect.width(), viewer_rect.width() / video_aspect)
                    } else {
                        (viewer_rect.height() * video_aspect, viewer_rect.height())
                    }
                } else {
                    (tex_size.x * view.zoom, tex_size.y * view.zoom)
                };
                let pan = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
                let image_rect = egui::Rect::from_center_size(
                    viewer_rect.center() + pan,
                    egui::vec2(dw, dh),
                );
                // Clip to `viewer_rect` so high-zoom overflow stays inside
                // the source-viewer panel.
                ui.painter_at(viewer_rect).image(
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
            // Everything drawn here stays inside the viewer: a narrow
            // viewer once let this text and these buttons bleed over the
            // panel beside it.
            let painter = ui.painter_at(viewer_rect);

            painter.text(
                egui::pos2(cx, cy - 60.0),
                egui::Align2::CENTER_CENTER,
                &media.filename,
                egui::FontId::proportional(13.0),
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
            painter.text(
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
            painter.text(
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
            painter.text(
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

            // As wide as fits the viewer, up to 90 px each.
            let btn_gap = 6.0;
            let btn_count = edit_buttons.len() as f32;
            let btn_w = ((viewer_rect.width() - 16.0 - btn_gap * (btn_count - 1.0)) / btn_count).clamp(44.0, 90.0);
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
                painter.rect_filled(rect, 2.0, fill);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    *label,
                    egui::FontId::proportional(if btn_w < 70.0 { 9.0 } else { 10.0 }),
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
                egui::FontId::proportional(13.0),
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
        let label_font = egui::FontId::monospace(10.0);

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
/// Compute the bounding box of a Title object in screen coords,
/// derived from its normalised `transform.center` / `transform.size`
/// mapped through the rendered video's `image_rect`.
/// One rectangle, or two side by side: the viewer layout the click
/// will switch to is the one drawn.
fn draw_viewer_layout_icon(painter: &egui::Painter, btn: egui::Rect, dual_now: bool, c: egui::Color32) {
    let ir = btn.shrink(4.0);
    let s = egui::Stroke::new(1.2_f32, c);
    if dual_now {
        painter.rect_stroke(ir, 1.0, s);
    } else {
        let half = (ir.width() - 2.0) / 2.0;
        painter.rect_stroke(egui::Rect::from_min_size(ir.min, egui::vec2(half, ir.height())), 1.0, s);
        painter.rect_stroke(egui::Rect::from_min_size(egui::pos2(ir.min.x + half + 2.0, ir.min.y), egui::vec2(half, ir.height())), 1.0, s);
    }
}

fn object_screen_bb(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
) -> egui::Rect {
    let cx = obj.transform.center.x;
    let cy = obj.transform.center.y;
    let half_w = obj.transform.size.x * 0.5;
    let half_h = obj.transform.size.y * 0.5;
    let to_screen = |nx: f32, ny: f32| egui::pos2(
        image_rect.min.x + nx * image_rect.width(),
        image_rect.min.y + ny * image_rect.height(),
    );
    egui::Rect::from_min_max(
        to_screen(cx - half_w, cy - half_h),
        to_screen(cx + half_w, cy + half_h),
    )
}

/// Hit-test a click against a selected object's handle hot-spots.
/// Returns the handle the user grabbed, or `None` if they hit empty
/// space (the caller falls back to body-drag / object-switch).
/// Hit zones use a small inflation around each handle so the user
/// doesn't need pixel-perfect aim.
fn hit_test_handles(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
    p: egui::Pos2,
) -> Option<crate::state::TitleObjectHandle> {
    use crate::state::TitleObjectHandle as H;
    let bb = object_screen_bb(obj, image_rect);
    let hit_radius = 10.0_f32;
    let inflate = egui::vec2(hit_radius, hit_radius);
    let near = |a: egui::Pos2| egui::Rect::from_center_size(a, inflate * 2.0).contains(p);

    if near(egui::pos2(bb.center().x, bb.min.y - 24.0)) {
        return Some(H::Rotation);
    }
    // Anchor dot — checked before the other handles so a near-
    // simultaneous overlap with an edge handle prefers the anchor.
    let an_x = bb.min.x + obj.transform.anchor_norm.x * bb.width();
    let an_y = bb.min.y + obj.transform.anchor_norm.y * bb.height();
    if near(egui::pos2(an_x, an_y)) {
        return Some(H::Anchor);
    }
    if near(bb.min) { return Some(H::CornerTopLeft); }
    if near(egui::pos2(bb.max.x, bb.min.y)) { return Some(H::CornerTopRight); }
    if near(egui::pos2(bb.min.x, bb.max.y)) { return Some(H::CornerBottomLeft); }
    if near(bb.max) { return Some(H::CornerBottomRight); }
    if near(egui::pos2(bb.center().x, bb.min.y)) { return Some(H::EdgeTop); }
    if near(egui::pos2(bb.center().x, bb.max.y)) { return Some(H::EdgeBottom); }
    if near(egui::pos2(bb.min.x, bb.center().y)) { return Some(H::EdgeLeft); }
    if near(egui::pos2(bb.max.x, bb.center().y)) { return Some(H::EdgeRight); }
    None
}

/// Compute the new `ObjectTransform` produced by an in-progress
/// drag, given the drag's normalised delta (drag-pos − drag-start
/// in 0..1 frame coords), the current and start pointer positions
/// in normalised coords (used by the rotation handle to compute
/// angles), and the handle being dragged.
///
/// Resize semantics:
/// - **Edge handles** (top / bottom / left / right) anchor the
///   opposite edge in place and pivot the box from there. Center
///   shifts by half the size delta.
/// - **Corner handles** anchor the diagonally-opposite corner;
///   uniform aspect kept by clamping the larger axis to match.
///   The font-size scaling lives at the call site (the resulting
///   transform is fed back through `SetTitleObjectTransformAndFont`).
/// - **Body** drag translates `transform.center` by the delta.
/// - **Rotation** drag computes the angle change between the
///   pointer's start and current position relative to the box's
///   centre, added to the initial rotation. The centre is the pivot.
fn compute_drag_transform(
    drag: &crate::state::TitleObjectDrag,
    dnx: f32,
    dny: f32,
    start_norm: (f32, f32),
    cur_norm: (f32, f32),
) -> klipp_timeline::ObjectTransform {
    use crate::state::TitleObjectHandle as H;
    let mut t = drag.initial_transform.clone();
    match drag.handle {
        H::Body => {
            t.center.x = (t.center.x + dnx).clamp(-2.0, 3.0);
            t.center.y = (t.center.y + dny).clamp(-2.0, 3.0);
        }
        H::EdgeLeft => {
            // Right edge stays put; left edge moves by dnx.
            let right = t.center.x + t.size.x * 0.5;
            let new_left = (t.center.x - t.size.x * 0.5) + dnx;
            let lo = new_left.min(right - 0.01);
            t.size.x = (right - lo).max(0.01);
            t.center.x = (lo + right) * 0.5;
        }
        H::EdgeRight => {
            let left = t.center.x - t.size.x * 0.5;
            let new_right = (t.center.x + t.size.x * 0.5) + dnx;
            let hi = new_right.max(left + 0.01);
            t.size.x = (hi - left).max(0.01);
            t.center.x = (left + hi) * 0.5;
        }
        H::EdgeTop => {
            let bot = t.center.y + t.size.y * 0.5;
            let new_top = (t.center.y - t.size.y * 0.5) + dny;
            let lo = new_top.min(bot - 0.01);
            t.size.y = (bot - lo).max(0.01);
            t.center.y = (lo + bot) * 0.5;
        }
        H::EdgeBottom => {
            let top = t.center.y - t.size.y * 0.5;
            let new_bot = (t.center.y + t.size.y * 0.5) + dny;
            let hi = new_bot.max(top + 0.01);
            t.size.y = (hi - top).max(0.01);
            t.center.y = (top + hi) * 0.5;
        }
        H::CornerTopLeft => {
            // Pin bottom-right; new top-left moves by (dnx, dny).
            let br_x = t.center.x + t.size.x * 0.5;
            let br_y = t.center.y + t.size.y * 0.5;
            let new_tl_x = (t.center.x - t.size.x * 0.5) + dnx;
            let new_tl_y = (t.center.y - t.size.y * 0.5) + dny;
            let lo_x = new_tl_x.min(br_x - 0.01);
            let lo_y = new_tl_y.min(br_y - 0.01);
            t.size.x = (br_x - lo_x).max(0.01);
            t.size.y = (br_y - lo_y).max(0.01);
            t.center.x = (lo_x + br_x) * 0.5;
            t.center.y = (lo_y + br_y) * 0.5;
        }
        H::CornerTopRight => {
            let bl_x = t.center.x - t.size.x * 0.5;
            let br_y = t.center.y + t.size.y * 0.5;
            let new_tr_x = (t.center.x + t.size.x * 0.5) + dnx;
            let new_tr_y = (t.center.y - t.size.y * 0.5) + dny;
            let hi_x = new_tr_x.max(bl_x + 0.01);
            let lo_y = new_tr_y.min(br_y - 0.01);
            t.size.x = (hi_x - bl_x).max(0.01);
            t.size.y = (br_y - lo_y).max(0.01);
            t.center.x = (bl_x + hi_x) * 0.5;
            t.center.y = (lo_y + br_y) * 0.5;
        }
        H::CornerBottomLeft => {
            let br_x = t.center.x + t.size.x * 0.5;
            let tl_y = t.center.y - t.size.y * 0.5;
            let new_bl_x = (t.center.x - t.size.x * 0.5) + dnx;
            let new_bl_y = (t.center.y + t.size.y * 0.5) + dny;
            let lo_x = new_bl_x.min(br_x - 0.01);
            let hi_y = new_bl_y.max(tl_y + 0.01);
            t.size.x = (br_x - lo_x).max(0.01);
            t.size.y = (hi_y - tl_y).max(0.01);
            t.center.x = (lo_x + br_x) * 0.5;
            t.center.y = (tl_y + hi_y) * 0.5;
        }
        H::CornerBottomRight => {
            let tl_x = t.center.x - t.size.x * 0.5;
            let tl_y = t.center.y - t.size.y * 0.5;
            let new_br_x = (t.center.x + t.size.x * 0.5) + dnx;
            let new_br_y = (t.center.y + t.size.y * 0.5) + dny;
            let hi_x = new_br_x.max(tl_x + 0.01);
            let hi_y = new_br_y.max(tl_y + 0.01);
            t.size.x = (hi_x - tl_x).max(0.01);
            t.size.y = (hi_y - tl_y).max(0.01);
            t.center.x = (tl_x + hi_x) * 0.5;
            t.center.y = (tl_y + hi_y) * 0.5;
        }
        H::Anchor => {
            // Anchor delta in normalised frame coords → bbox-local
            // delta. Need to divide by the box's normalised size so
            // dragging by 0.1 frame-units across a 0.4-frame-wide
            // box moves the anchor by 0.25 box-units. Clamp to 0..1
            // so the anchor stays inside the box (rotating around
            // an off-box anchor is a niche case worth surfacing
            // later, not in commit 6).
            let inv_w = if drag.initial_transform.size.x.abs() > 0.0001 {
                1.0 / drag.initial_transform.size.x
            } else { 0.0 };
            let inv_h = if drag.initial_transform.size.y.abs() > 0.0001 {
                1.0 / drag.initial_transform.size.y
            } else { 0.0 };
            t.anchor_norm.x = (drag.initial_transform.anchor_norm.x + dnx * inv_w)
                .clamp(0.0, 1.0);
            t.anchor_norm.y = (drag.initial_transform.anchor_norm.y + dny * inv_h)
                .clamp(0.0, 1.0);
        }
        H::Rotation => {
            // Angle change = (angle from centre to current pointer)
            //              − (angle from centre to start pointer).
            // atan2 returns degrees CCW from +X; we add to the
            // initial rotation so the gesture feels relative-to-
            // grab-point rather than absolute. Aspect stretches
            // would distort the angle, so we use raw normalised
            // coords (image-rect aspect-correction is a polish
            // pass — for typical 16:9 frames the visual error is
            // a few degrees at most, and the gesture intent stays
            // intuitive: clockwise drag = clockwise rotate).
            let cx = drag.initial_transform.center.x;
            let cy = drag.initial_transform.center.y;
            let a_start = (start_norm.1 - cy).atan2(start_norm.0 - cx);
            let a_cur = (cur_norm.1 - cy).atan2(cur_norm.0 - cx);
            let delta_deg = (a_cur - a_start).to_degrees();
            t.rotation = drag.initial_transform.rotation + delta_deg;
        }
    }
    t
}

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
mod viewer_geom_tests {
    //! Pin the three viewer-geom helpers so a future refactor (e.g.
    //! moving them into a sibling `viewer_geom.rs`, or unifying
    //! with title-edit cursor placement) can't silently change the
    //! coordinate-conversion semantics that the focal-point gizmo,
    //! title-object editor, and (incoming) letter picker all share.
    use super::*;
    use egui::{pos2, Rect};
    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn norm_to_screen_centre_lands_on_image_centre() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let p = viewer_norm_to_screen(r, 0.5, 0.5);
        assert!((p.x - 500.0).abs() < 1e-6);
        assert!((p.y - 500.0).abs() < 1e-6);
    }

    #[test]
    fn norm_to_screen_corners() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let tl = viewer_norm_to_screen(r, 0.0, 0.0);
        let br = viewer_norm_to_screen(r, 1.0, 1.0);
        assert!((tl.x - 100.0).abs() < 1e-6);
        assert!((tl.y - 200.0).abs() < 1e-6);
        assert!((br.x - 900.0).abs() < 1e-6);
        assert!((br.y - 800.0).abs() < 1e-6);
    }

    #[test]
    fn norm_to_screen_preserves_overflow() {
        // Title-object editor case: an object dragged past the
        // canvas edge has nx > 1.0 and the gizmo paints offscreen.
        // Helper must NOT clamp — the caller decides.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let p = viewer_norm_to_screen(r, 1.5, -0.25);
        assert!((p.x - 150.0).abs() < 1e-6);
        assert!((p.y - -25.0).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_centre_round_trips() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let (nx, ny) = viewer_screen_to_norm(r, pos2(500.0, 500.0));
        assert!((nx - 0.5).abs() < 1e-6);
        assert!((ny - 0.5).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_returns_unclamped_for_drag_accuracy() {
        // Pointer dragged past the canvas — focal-point caller will
        // clamp to [0,1] before storing; title-object drag handler
        // needs the unclamped delta. Helper preserves both.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (nx, ny) = viewer_screen_to_norm(r, pos2(150.0, -25.0));
        assert!((nx - 1.5).abs() < 1e-6);
        assert!((ny - -0.25).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_handles_degenerate_image_rect() {
        // During layout settle the image rect can briefly be
        // zero-sized. The width/height >= 1.0 guard prevents NaN
        // from a divide-by-zero.
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(0.0, 0.0));
        let (nx, ny) = viewer_screen_to_norm(r, pos2(50.0, 50.0));
        assert!(nx.is_finite());
        assert!(ny.is_finite());
    }

    #[test]
    fn frame_px_to_screen_maps_glyph_centroid() {
        // Glyph centroid at frame-pixel (960, 540) on a 1920×1080
        // title source, viewed in a 800×450 image rect at (100,200)
        // — should land at the rect centre.
        let r = rect(100.0, 200.0, 800.0, 450.0);
        let p = viewer_frame_px_to_screen(r, 1920.0, 1080.0, 960.0, 540.0);
        assert!((p.x - 500.0).abs() < 1e-6);
        assert!((p.y - 425.0).abs() < 1e-6);
    }

    #[test]
    fn frame_px_to_screen_handles_zero_frame_size() {
        // Defensive: a degenerate frame size shouldn't NaN.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let p = viewer_frame_px_to_screen(r, 0.0, 0.0, 50.0, 50.0);
        assert!(p.x.is_finite());
        assert!(p.y.is_finite());
    }

    #[test]
    fn round_trip_norm_through_screen_preserves_value() {
        // Focal-point gizmo writes (norm) → reads pointer (screen) →
        // writes back (norm). Should be lossless within float epsilon.
        let r = rect(100.0, 200.0, 800.0, 600.0);
        for (nx, ny) in [(0.0, 0.0), (0.25, 0.75), (0.5, 0.5), (0.999, 0.001)] {
            let p = viewer_norm_to_screen(r, nx, ny);
            let (nx2, ny2) = viewer_screen_to_norm(r, p);
            assert!((nx - nx2).abs() < 1e-5, "x: {nx} vs {nx2}");
            assert!((ny - ny2).abs() < 1e-5, "y: {ny} vs {ny2}");
        }
    }
}

#[cfg(test)]
mod title_object_drag_tests {
    use super::*;
    use crate::state::{TitleObjectDrag, TitleObjectHandle};
    use klipp_timeline::{ObjectTransform, TitleObjectId, TitlePosition};

    fn baseline_transform() -> ObjectTransform {
        ObjectTransform {
            center: TitlePosition { x: 0.5, y: 0.5 },
            center_x_keyframes: Vec::new(),
            center_y_keyframes: Vec::new(),
            size: TitlePosition { x: 0.4, y: 0.2 },
            rotation: 0.0,
            anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
        }
    }

    fn drag(handle: TitleObjectHandle) -> TitleObjectDrag {
        TitleObjectDrag {
            clip_id: klipp_core::ClipId::new(),
            object_id: TitleObjectId(0),
            handle,
            initial_transform: baseline_transform(),
            initial_font_size: 72.0,
            drag_start_pos: egui::pos2(0.0, 0.0),
        }
    }

    #[test]
    fn body_drag_translates_centre() {
        // Move +0.1 in x and -0.05 in y → centre shifts by exactly that.
        let t = compute_drag_transform(&drag(TitleObjectHandle::Body), 0.1, -0.05, (0.0, 0.0), (0.1, -0.05));
        assert!((t.center.x - 0.6).abs() < 1e-5);
        assert!((t.center.y - 0.45).abs() < 1e-5);
        // Size stays the same on body drag.
        assert!((t.size.x - 0.4).abs() < 1e-5);
        assert!((t.size.y - 0.2).abs() < 1e-5);
    }

    #[test]
    fn edge_right_drag_anchors_left() {
        // Initial right edge at 0.7. Drag +0.1 → new right at 0.8.
        // Left edge stays at 0.3. New centre = 0.55, new width = 0.5.
        let t = compute_drag_transform(&drag(TitleObjectHandle::EdgeRight), 0.1, 0.0, (0.0, 0.0), (0.1, 0.0));
        assert!((t.center.x - 0.55).abs() < 1e-5);
        assert!((t.size.x - 0.5).abs() < 1e-5);
        // Y axis untouched.
        assert!((t.center.y - 0.5).abs() < 1e-5);
        assert!((t.size.y - 0.2).abs() < 1e-5);
    }

    #[test]
    fn corner_top_left_drag_anchors_bottom_right() {
        // Initial bottom-right at (0.7, 0.6). Drag TL by (-0.1, -0.05)
        // → new TL at (0.2, 0.35). New centre = ((0.2+0.7)/2, (0.35+0.6)/2)
        // = (0.45, 0.475). New size = (0.5, 0.25).
        let t = compute_drag_transform(&drag(TitleObjectHandle::CornerTopLeft), -0.1, -0.05, (0.0, 0.0), (-0.1, -0.05));
        assert!((t.center.x - 0.45).abs() < 1e-5);
        assert!((t.center.y - 0.475).abs() < 1e-5);
        assert!((t.size.x - 0.5).abs() < 1e-5);
        assert!((t.size.y - 0.25).abs() < 1e-5);
    }

    #[test]
    fn edge_drag_clamps_minimum_size() {
        // Try to drag the right edge past the left edge — the math
        // must clamp size at the minimum so the box doesn't invert.
        let t = compute_drag_transform(&drag(TitleObjectHandle::EdgeRight), -10.0, 0.0, (0.0, 0.0), (-10.0, 0.0));
        assert!(t.size.x > 0.0, "size should never go negative");
        assert!(t.size.x >= 0.01 - 1e-6, "size clamped to minimum");
    }

    #[test]
    fn anchor_drag_translates_anchor_within_box() {
        // Initial anchor at (0.5, 0.5) — box centre. Drag by half
        // the box's width in X — anchor_norm.x should move by
        // dnx / size.x = 0.2 / 0.4 = 0.5 → ends at 1.0 (the right
        // edge of the box, clamped).
        let d = drag(TitleObjectHandle::Anchor);
        let t = compute_drag_transform(&d, 0.2, 0.0, (0.0, 0.0), (0.2, 0.0));
        assert!((t.anchor_norm.x - 1.0).abs() < 1e-5);
        assert!((t.anchor_norm.y - 0.5).abs() < 1e-5);
        // Object's size / centre / rotation untouched.
        assert!((t.size.x - 0.4).abs() < 1e-5);
        assert!((t.center.x - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rotation_drag_changes_angle() {
        // Start pointer directly above centre, drag to directly
        // right of centre → 90° change CCW (atan2 convention).
        let d = drag(TitleObjectHandle::Rotation);
        let cx = d.initial_transform.center.x;
        let cy = d.initial_transform.center.y;
        let start = (cx, cy - 0.2);
        let end = (cx + 0.2, cy);
        let dnx = end.0 - start.0;
        let dny = end.1 - start.1;
        let t = compute_drag_transform(&d, dnx, dny, start, end);
        assert!((t.rotation - 90.0).abs() < 1.0, "got rotation {}", t.rotation);
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
        assert!((6..=7).contains(&idx), "Click on line 2 should give index 6-7, got {}", idx);
    }

    // ── Full editor simulation with char_positions ──

    #[test]
    fn test_full_editor_with_positions() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = "Hello World".to_string();
        let mut cursor = text.chars().count(); // start at end

        // Compute positions
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
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
            let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
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

            let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
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

        for (event_type, _should_insert) in events {
            if let Some(ch) = event_type.strip_prefix("text_") {
                // Process Event::Text
                // "a"
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
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
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
