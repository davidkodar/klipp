//! The window's layout each frame: the launch screen, the full-screen viewer,
//! the main layout's panels and rails and their resizers, and which panels a
//! workspace shows (#826).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    /// Apply a `SavedWorkspaceLayout` to AppState + LayoutState. Used by
    /// both the built-in and user-layout apply commands.
    ///
    /// Visibility flags are copied unconditionally; size fields only
    /// overwrite `self.layout.*` when `Some`, which preserves user
    /// customisation when an older layout JSON doesn't carry sizes (the
    /// serde `#[default]` lands them as `None`).
    pub(super) fn apply_workspace_layout(&mut self, layout: &crate::state::SavedWorkspaceLayout) {
        // Through the setters, so the persisted copy follows a workspace
        // switch rather than silently disagreeing with the screen.
        self.set_browser_visible(layout.show_browser);
        self.set_inspector_visible(layout.show_inspector);
        self.state.show_history_panel = layout.show_history;
        self.state.show_markers_panel = layout.show_markers;
        self.state.show_curve_editor = layout.show_curve_editor;
        self.state.show_scopes = layout.show_scopes;
        self.state.show_color_panel = layout.show_color_panel;
        if let Some(w) = layout.browser_width {
            self.layout.browser_width = w;
        }
        if let Some(w) = layout.inspector_width {
            self.layout.inspector_width = w;
        }
        if let Some(h) = layout.timeline_height {
            self.layout.timeline_height = h;
        }
        if let Some(h) = layout.color_panel_height {
            self.layout.color_panel_height = h;
        }
        self.layout.save();
    }

    fn render_launch_screen(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) -> bool {
        if self.state.project_path.is_none() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    // Auto-saves are recovery copies, not projects: they are
                    // offered from their project's card and open prompt.
                    let recent: Vec<_> = self.prefs.general.recent_projects.iter()
                        .filter(|rp| !is_autosave_path(&rp.path))
                        .cloned()
                        .collect();
                    let folder = Some(self.projects_root());
                    self.launch_screen.templates_dir = Some(self.project_templates_dir());
                    self.launch_screen.last_rate = self.prefs.general.last_format_rate;
                    let panel_cmds = self.launch_screen.show(ctx, ui, &recent, folder.as_deref(), &self.theme);
                    all_cmds.extend(panel_cmds);
                });
            true
        } else {
            false
        }
    }

    fn render_fullscreen_viewer(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) -> bool {
        if self.state.fullscreen_viewer {
            // Fullscreen viewer occupies the entire screen — record this so the
            // dim-on-play overlay (which paints around the viewer rect) computes
            // a zero-area frame and produces no visible strips.
            self.state.viewer_panel_rect = ctx.screen_rect();
            egui::CentralPanel::default().show(ctx, |ui| {
                let panel_cmds = self.viewer.show_fullscreen(ui, &mut self.state);
                all_cmds.extend(panel_cmds);
            });
            true
        } else {
            false
        }
    }

    pub(super) fn render_main_layout(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) {
        // A parameter focus set in the Inspector shows its curve, opening
        // the Curve Editor (#2296), before the layout is worked out.
        self.curve_editor.follow_focus(&mut self.state);
        if self.render_launch_screen(ctx, all_cmds) { return; }
        if self.render_fullscreen_viewer(ctx, all_cmds) { return; }
        {
            // ── Manual rect layout ──
            // Use CentralPanel as the container, then compute rects manually.
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    let full = ui.available_rect_before_wrap();

                    // Pure rect computation — see crates/klipp-ui/src/layout.rs
                    let rects = crate::layout::compute_layout(full, &self.layout, &self.state);
                    if self.nodes_were_on_right != rects.nodes_on_right {
                        self.node_graph_v2_panel.request_fit();
                        self.nodes_were_on_right = rects.nodes_on_right;
                    }
                    // Cache the viewer panel rect for overlays (e.g. dim-on-play)
                    // that need to align with the viewer regardless of layout mode.
                    self.state.viewer_panel_rect = rects.viewer_rect;
                    // The Colour strip is capped to its tracks; a stored
                    // height above what is shown would make the divider
                    // move nothing until the drag had used it up.
                    if self.state.show_color_panel && rects.timeline_rect.height() + 0.5 < self.layout.color_timeline_height {
                        self.layout.color_timeline_height = rects.timeline_rect.height();
                    }
                    // The strip's graded thumbnails, drawn off the UI
                    // thread and taken as they land (#899).
                    if self.state.show_color_panel {
                        let s = &mut self.state;
                        let now = ctx.input(|i| i.time);
                        s.graded_thumbs.update(ctx, now, &s.sequences[s.active_sequence_idx], &s.media_pool, klipp_engine::video_decoder::working_colorspace(), s.wipe_clip);
                    }
                    let crate::layout::LayoutRects {
                        full: _,
                        toolbar_rect,
                        statusbar_rect,
                        browser_rect,
                        left_resizer_rect,
                        inspector_rect,
                        right_resizer_rect,
                        source_viewer_rect,
                        viewer_rect,
                        h_resizer_rect,
                        curve_editor_rect,
                        audio_mixer_rect,
                        timeline_rect,
                        toggle_bar_rect,
                        scopes_rect,
                        nodes_rect,
                        nodes_resizer_rect,
                        nodes_on_right,
                    } = rects;

                    // Derived scalars consumed by the render section below. These mirror
                    // the locals that used to live in the inlined layout block, so every
                    // downstream use-site keeps working unchanged.
                    let main_top = toolbar_rect.max.y;
                    let main_bottom = statusbar_rect.min.y;
                    let center_left = left_resizer_rect.max.x;
                    // Bottom-dock width = the FULL center column (between
                    // the two resizers), not the program-viewer width.
                    // The viewer row is split 50/50 source + program in
                    // layout.rs, so `viewer_rect.width()` is only half
                    // the center column — using it here would (and did,
                    // pre-fix) constrain the scope dock + toggle bar +
                    // bottom tools to half their available area, leaving
                    // the source-viewer half empty below the viewer row.
                    let center_w = right_resizer_rect.min.x - left_resizer_rect.max.x;
                    let in_color_ws = self.state.show_color_panel;
                    let show_scopes_bar = self.state.scopes_docked();
                    let show_nodes_bar = self.state.graph_is_visible() && !nodes_on_right;
                    let toggle_bar_h = toggle_bar_rect.height();
                    let scopes_h = scopes_rect.height();
                    let nodes_h = if nodes_on_right { 0.0 } else { nodes_rect.height() };
                    let audio_mixer_h = audio_mixer_rect.height();
                    let curve_editor_h = curve_editor_rect.height();
                    // flexible_h is the ratio-drag denominator — same formula as in layout.rs.
                    let flexible_h = {
                        let center_total = main_bottom - main_top;
                        let fixed_h = toggle_bar_h + theme::RESIZER_THICKNESS;
                        (center_total - fixed_h).max(200.0)
                    };
                    // Timeline-height clamp bounds for the horizontal resizer drag.
                    let timeline_min_h = theme::TIMELINE_MIN_H;
                    let max_timeline_h = {
                        let color_bottom_total = toggle_bar_h + scopes_h + nodes_h;
                        let color_bottom_top = main_bottom - color_bottom_total;
                        (color_bottom_top - main_top - theme::RESIZER_THICKNESS) * theme::TIMELINE_MAX_H_PCT
                    };

                    // ── Render panels into their rects ──
                    self.render_toolbar_panel(ui, toolbar_rect, all_cmds);
                    self.render_status_bar_panel(ui, statusbar_rect);

                    if self.state.show_browser_panel {
                        self.render_browser_panel(ui, browser_rect, all_cmds);
                    } else {
                        self.render_browser_rail(ui, browser_rect);
                    }

                    if self.state.show_inspector_panel {
                        // Grading the timeline: the panels show and edit
                        // its grade as the selected clip's.
                        let stood_in = self.stand_in_timeline_grade();
                        self.render_inspector_panel(ui, inspector_rect, all_cmds);
                        self.stand_down_timeline_grade(stood_in);
                    } else {
                        self.render_inspector_rail(ui, inspector_rect);
                    }


                    let color_tools_bottom = in_color_ws
                        && self.state.color_tools_position == crate::state::ColorToolsPosition::Bottom;

                    // A single-viewer layout gives the hidden viewer no
                    // width; it is not drawn at all.
                    if source_viewer_rect.width() > 1.0 {
                        self.render_source_viewer_placeholder(ui, source_viewer_rect, all_cmds);
                    }
                    if viewer_rect.width() > 1.0 {
                        self.render_viewer_panel(ui, viewer_rect, all_cmds);
                    }

                    // The light under the pointer, while HDR monitoring is
                    // on: the engine publishes the picture's cd/m² with
                    // each frame, since an SDR screen cannot show it.
                    self.state.hdr_nits_under_pointer = match (
                        self.state.hdr_monitor.renders_hdr(),
                        self.state.viewer_pointer_pos,
                        self.state.viewer_image_rect,
                    ) {
                        (true, Some(pos), Some(rect)) => {
                            let (nx, ny) = crate::panels::viewer::viewer_screen_to_norm(rect, pos);
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            fb.nits_at(nx, ny)
                        }
                        _ => None,
                    };

                    // Compute bottom-panel scope cursor data (shared by overlay + bottom panel)
                    // If locked but data not yet computed, compute it now
                    if self.state.scope_cursor_locked && self.state.scope_cursor_locked_data.is_none() {
                        if let (Some(pos), Some(rect)) = (self.state.scope_cursor_locked_pos, self.state.viewer_image_rect) {
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            self.state.scope_cursor_locked_data = crate::widgets::scope_window::sample_viewer_pixel(
                                pos, rect, &fb, self.state.scope_cursor_sample_radius,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                            );
                        }
                    }

                    let bottom_scope_cursor: Option<crate::state::ScopeCursorData> = if self.state.scope_cursor_locked {
                        self.state.scope_cursor_locked_data
                    } else if self.state.scope_cursor_show {
                        if let (Some(pos), Some(rect)) = (self.state.viewer_pointer_pos, self.state.viewer_image_rect) {
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            crate::widgets::scope_window::sample_viewer_pixel(
                                pos, rect, &fb, self.state.scope_cursor_sample_radius,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Scope overlay on viewer (semi-transparent, bottom-right corner)
                    if self.state.show_scopes && self.state.scope_overlay_mode {
                        let overlay_w = (viewer_rect.width() * 0.3).min(200.0);
                        let overlay_h = overlay_w; // square
                        let overlay_rect = egui::Rect::from_min_size(
                            egui::pos2(viewer_rect.right() - overlay_w - 8.0, viewer_rect.bottom() - overlay_h - 8.0),
                            egui::vec2(overlay_w, overlay_h),
                        );
                        crate::ui_util::allocate_clipped_ui(ui, overlay_rect, |ui| {
                            ui.set_min_size(overlay_rect.size());
                            // Semi-transparent dark background
                            ui.painter().rect_filled(overlay_rect, 4.0, egui::Color32::from_black_alpha(160));
                            self.state.scope_state.scale = self.state.scope_scale;
                            self.state.scope_state.legal_range = self.state.scope_legal_range;
                            self.state.scope_state.hdr_levels = self.state.hdr_monitor.renders_hdr();
                            crate::widgets::scopes::show_scopes(
                                ui,
                                &mut self.state.scope_state,
                                self.state.scope_mode,
                                &self.frame_buffer,
                                self.state.scope_quality,
                                self.prefs.color.vectorscope_skin_angle,
                                self.state.playback == PlaybackState::Playing,
                                bottom_scope_cursor,
                                self.state.scope_cursor_show,
                                &self.theme,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                                self.state.scope_options.vectorscope_zoom,
                                self.state.scope_options.histogram_display,
                            );
                        });
                    }

                    if self.state.show_curve_editor && curve_editor_h > 0.0 {
                        self.render_curve_editor_panel(ui, curve_editor_rect, all_cmds);
                    } else {
                        // Not drawn, so Previous / Next walk the rubber band (#1401).
                        self.state.curve_editor_track = None;
                        self.state.curve_editor_mask_track = None;
                    }
                    if self.state.show_audio_mixer && audio_mixer_h > 0.0 {
                        self.render_audio_mixer_panel(ui, audio_mixer_rect, all_cmds);
                    }
                    self.render_timeline_panel(ui, timeline_rect, all_cmds);

                    // ── Color workspace bottom panels ──
                    // Render when in Color (toggle bar + scopes + nodes) OR
                    // when scopes are toggled on from any other workspace.
                    // The render function gates the Color-only chrome
                    // internally on `toggle_bar_h > 0` and `color_tools_bottom`.
                    if in_color_ws || show_scopes_bar || self.state.graph_dock_open {
                        let stood_in = if self.state.graph_dock_open { None } else { self.stand_in_timeline_grade() };
                        self.render_color_workspace_bottom(
                            ctx,
                            ui,
                            all_cmds,
                            ColorWsParams {
                                show_scopes_bar,
                                show_nodes_bar,
                                color_tools_bottom,
                                toggle_bar_h,
                                scopes_h,
                                center_left,
                                center_w,
                                main_top,
                                main_bottom,
                                flexible_h,
                                toggle_bar_rect,
                                nodes_rect,
                                nodes_resizer_rect,
                                bottom_scope_cursor,
                            },
                        );
                        self.stand_down_timeline_grade(stood_in);
                    }

                    self.handle_panel_resizers(
                        ui,
                        left_resizer_rect,
                        right_resizer_rect,
                        h_resizer_rect,
                        timeline_min_h,
                        max_timeline_h,
                    );

                    // Transport is now integrated into the toolbar (P1-2).
                    // Keyboard shortcuts still handled by self.transport.handle_keyboard().
                });
        }

    }

    fn handle_panel_resizers(
        &mut self,
        ui: &mut egui::Ui,
        left_resizer_rect: egui::Rect,
        right_resizer_rect: egui::Rect,
        h_resizer_rect: egui::Rect,
        timeline_min_h: f32,
        max_timeline_h: f32,
    ) {
        let theme = self.theme.clone();
        let (left_delta, left_dbl) = resizer::vertical_resizer_full(ui, left_resizer_rect, &theme);
        let (right_delta, right_dbl) = resizer::vertical_resizer_full(ui, right_resizer_rect, &theme);
        if left_dbl {
            let now = !self.state.show_browser_panel;
            self.set_browser_visible(now);
        }
        if right_dbl {
            let now = !self.state.show_inspector_panel;
            self.set_inspector_visible(now);
        }
        let h_delta = resizer::horizontal_resizer(ui, h_resizer_rect, &theme);

        // Dragging a panel well under its minimum collapses it rather
        // than sticking at the clamp. Without this the divider simply
        // refuses to move and the gesture reads as broken; with it,
        // "drag it away" works the way people expect and the rail
        // catches the result.
        const COLLAPSE_DRAG_SLOP: f32 = 60.0;
        if left_delta != 0.0 {
            let wanted = self.layout.browser_width + left_delta;
            if self.state.show_browser_panel && wanted < theme::BROWSER_MIN_W - COLLAPSE_DRAG_SLOP {
                self.set_browser_visible(false);
            } else {
                self.layout.browser_width = wanted.clamp(theme::BROWSER_MIN_W, theme::BROWSER_MAX_W);
            }
        }
        if right_delta != 0.0 {
            let wanted = self.layout.inspector_width - right_delta;
            if self.state.show_inspector_panel && wanted < theme::INSPECTOR_MIN_W - COLLAPSE_DRAG_SLOP {
                self.set_inspector_visible(false);
            } else {
                self.layout.inspector_width = wanted.clamp(theme::INSPECTOR_MIN_W, theme::INSPECTOR_MAX_W);
            }
        }
        if h_delta != 0.0 {
            if self.state.show_color_panel {
                self.layout.color_timeline_height = (self.layout.color_timeline_height - h_delta)
                    .clamp(timeline_min_h, max_timeline_h.max(timeline_min_h));
            } else {
                self.layout.timeline_height = (self.layout.timeline_height - h_delta)
                    .clamp(timeline_min_h, max_timeline_h);
            }
        }
        let resizer_active = left_delta != 0.0 || right_delta != 0.0 || h_delta != 0.0;
        if resizer_active {
            self.layout_dirty = true;
        }
        // Color's single viewer is the program viewer: the transport and
        // the keys follow it there, whatever the source viewer had.
        if self.state.single_viewer_is_program() && self.state.viewer_mode == klipp_core::ViewerMode::Source {
            self.state.viewer_mode = klipp_core::ViewerMode::Program;
        }
        if self.layout.dual_viewers != self.state.dual_viewers
            || self.layout.color_dual_viewers != self.state.color_dual_viewers
            || self.layout.export_dual_viewers != self.state.export_dual_viewers
        {
            self.layout.dual_viewers = self.state.dual_viewers;
            self.layout.color_dual_viewers = self.state.color_dual_viewers;
            self.layout.export_dual_viewers = self.state.export_dual_viewers;
            self.layout_dirty = true;
        }
        if self.layout_dirty
            && !ui.input(|i| i.pointer.any_pressed() || i.pointer.is_decidedly_dragging())
        {
            self.layout.save();
            self.layout_dirty = false;
        }
    }

    fn render_toolbar_panel(
        &mut self,
        ui: &mut egui::Ui,
        toolbar_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        let theme = &self.theme;
        let border = theme.border;
        let bg_deep = theme.bg_deep;
        crate::ui_util::allocate_clipped_ui(ui, toolbar_rect, |ui| {
            ui.set_min_size(toolbar_rect.size());
            ui.painter().rect_filled(toolbar_rect, 0.0, bg_deep);
            ui.painter().hline(
                toolbar_rect.x_range(),
                toolbar_rect.max.y,
                egui::Stroke::new(1.0_f32, border),
            );
            let recent = self.prefs.general.recent_projects.clone();
            let panel_cmds = self.toolbar.show(
                ui,
                &mut self.state,
                &recent,
                &self.theme,
                &self.key_bindings,
            );
            all_cmds.extend(panel_cmds);
        });
    }

    /// Paint the Source Viewer panel — video area on top (delegated to
    /// `ViewerPanel::show_source_viewer`, which handles skim-texture
    /// preview, metadata card, In/Out markers, and edit buttons) plus the
    /// shared transport chrome (scrub bar + play bar) below.
    ///
    /// The transport chrome currently routes to the timeline playhead
    /// because per-viewer routing isn't wired yet. That's the next piece
    /// of the source-decoder / source-playhead work.
    fn render_source_viewer_placeholder(
        &mut self,
        ui: &mut egui::Ui,
        source_viewer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Must match the transport-chrome heights inside `draw_transport_chrome`.
        // Shared with layout.rs via `theme::VIEWER_CHROME_HEIGHT` so the viewer
        // rect reserves exactly enough room for the scrub + playbar rows.
        const BOTTOM_H: f32 = theme::VIEWER_CHROME_HEIGHT;

        // Click-to-focus. Allocated FIRST so its z-order sits below
        // every interactive widget rendered later in this function
        // (the (i) icon, edit buttons, scrub bar, transport buttons).
        // Those widgets win clicks on their specific rects; clicks on
        // empty viewer area fall through and focus the source viewer.
        let focus_resp = ui.interact(
            source_viewer_rect,
            ui.id().with("source_viewer_focus"),
            egui::Sense::click(),
        );
        if focus_resp.clicked() && self.state.viewer_mode != klipp_core::ViewerMode::Source {
            all_cmds.push(AppCommand::SetViewerMode(klipp_core::ViewerMode::Source));
        }

        let video_rect = egui::Rect::from_min_max(
            source_viewer_rect.min,
            egui::pos2(source_viewer_rect.max.x, source_viewer_rect.max.y - BOTTOM_H),
        );
        let chrome_rect = egui::Rect::from_min_size(
            egui::pos2(source_viewer_rect.min.x, video_rect.max.y),
            egui::vec2(source_viewer_rect.width(), BOTTOM_H),
        );

        // Black letterbox behind the source viewer content.
        ui.painter_at(video_rect).rect_filled(video_rect, 0.0, egui::Color32::BLACK);

        // Delegate the video-area rendering to the existing ViewerPanel
        // helper — same skim texture, same metadata card, same In/Out
        // markers, same edit buttons users already saw when clicking the
        // old "Source" toggle. No duplicated logic.
        // While a multicam group is in play, the Source viewer's picture
        // is the angle grid: click a tile to cut to that angle, as keys
        // 1-9 do (through the same command, so both routes record the
        // same). The chrome below stays, so a single-viewer layout can go
        // back to Program.
        // Multicam mode (#1844): the grid of the clip under the playhead
        // (or the opened one), or a note when there is none. The Angles
        // button, top right, switches it whichever is shown.
        if self.state.multicam_mode {
            let runtime = self.multicam_grid.as_ref().filter(|r| self.state.multicam_viewer_active && !r.sources.is_empty());
            match runtime {
                Some(runtime) => {
                    if let Some(angle) = crate::multicam_grid::show(ui, video_rect, runtime, self.state.multicam_active_angle, &self.theme) {
                        all_cmds.push(AppCommand::SwitchMulticamAngle { angle: angle as usize + 1 });
                    }
                }
                None => crate::multicam_grid::no_multicam_note(ui, video_rect, &self.theme),
            }
            all_cmds.extend(crate::multicam_grid::angles_button(ui, video_rect, true, &self.theme));
            // The timeline's transport, not the Source viewer's: the
            // grid follows the timeline playhead, and the Source
            // chrome read and moved a source playhead it is not about.
            let chrome_cmds = self.viewer.draw_transport_chrome(ui, &mut self.state, &self.theme, chrome_rect, klipp_core::ViewerMode::Program);
            all_cmds.extend(chrome_cmds);
            return;
        }
        self.viewer.show_source_viewer(ui, &self.state, &self.theme, video_rect, all_cmds);
        all_cmds.extend(crate::multicam_grid::angles_button(ui, video_rect, false, &self.theme));

        // Scroll-zoom + middle-drag-pan input for the source video area.
        // Texture size = the source viewer's live frame when present, or
        // the committed source clip's first-frame, or the source media's
        // declared resolution. Falling back to the timeline size keeps
        // the math finite even when nothing is loaded.
        let source_tex_size = self
            .state
            .source_live_texture
            .as_ref()
            .map(|t| t.size_vec2())
            .or_else(|| {
                self.state.source_video_texture.as_ref().map(|(_, t)| t.size_vec2())
            })
            .or_else(|| {
                self.state.source_media_id
                    .and_then(|mid| self.state.media_pool.get(&mid))
                    .map(|m| egui::vec2(m.resolution.width as f32, m.resolution.height as f32))
            })
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        self.viewer.handle_viewer_zoom_pan_input(
            ui,
            &mut self.state,
            video_rect,
            source_tex_size,
            klipp_core::ViewerMode::Source,
        );

        // Floating "..." overflow menu in the video area's top-right
        // corner — holds the controls that don't fit in the responsive
        // playbar plus zoom presets and the viewer overlay toggles.
        self.viewer.draw_viewer_overflow_menu(
            ui,
            &mut self.state,
            &self.theme,
            video_rect,
            klipp_core::ViewerMode::Source,
            &self.key_bindings,
            all_cmds,
        );

        // Reuse the same scrub bar + play bar the Program Viewer renders,
        // routing scrub-bar drags and the timecode display to the source
        // playhead via `ViewerMode::Source`.
        let chrome_cmds = self.viewer.draw_transport_chrome(
            ui,
            &mut self.state,
            &self.theme,
            chrome_rect,
            klipp_core::ViewerMode::Source,
        );
        all_cmds.extend(chrome_cmds);
    }

    fn render_viewer_panel(
        &mut self,
        ui: &mut egui::Ui,
        viewer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Click-to-focus. Allocated BEFORE the viewer's child UI so its
        // z-order sits below every interactive widget rendered later
        // (transform handles, scrub bar, transport buttons, etc.).
        // Those widgets win clicks on their specific rects; clicks on
        // empty viewer image area fall through and focus the program
        // viewer.
        let focus_resp = ui.interact(
            viewer_rect,
            ui.id().with("program_viewer_focus"),
            egui::Sense::click(),
        );
        if focus_resp.clicked() && self.state.viewer_mode != klipp_core::ViewerMode::Program {
            all_cmds.push(AppCommand::SetViewerMode(klipp_core::ViewerMode::Program));
        }

        // DaVinci/FCP-style: the viewer fills its panel rect flush — no
        // rounded corners, no stroke, no drop shadow, no inner margin. The
        // only divider between this panel and its neighbours is the panel
        // resizer; we don't add a competing border here.
        crate::ui_util::allocate_clipped_ui(ui, viewer_rect, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.spacing_mut().window_margin = egui::Margin::ZERO;
            {
                let v = &mut ui.style_mut().visuals;
                v.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
                v.widgets.noninteractive.fg_stroke = egui::Stroke::NONE;
                v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                v.widgets.active.bg_stroke = egui::Stroke::NONE;
                v.window_stroke = egui::Stroke::NONE;
                v.window_shadow = egui::Shadow::NONE;
            }
            let frame = egui::Frame::none()
                .fill(self.theme.bg_viewer)
                .inner_margin(egui::Margin::ZERO);
            frame.show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let panel_cmds = self.viewer.show(ui, &mut self.state, &self.theme, &self.key_bindings);
                all_cmds.extend(panel_cmds);
            });
        });
    }

    fn render_curve_editor_panel(
        &mut self,
        ui: &mut egui::Ui,
        curve_editor_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        self.curve_editor.sync_param_from_state(&self.state);
        crate::ui_util::allocate_clipped_ui(ui, curve_editor_rect, |ui| {
            ui.set_min_size(curve_editor_rect.size());
            let panel_cmds = self.curve_editor.show(ui, &mut self.state, &self.theme);
            all_cmds.extend(panel_cmds);
        });
    }

    fn render_audio_mixer_panel(
        &mut self,
        ui: &mut egui::Ui,
        audio_mixer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, audio_mixer_rect, |ui| {
            ui.set_min_size(audio_mixer_rect.size());
            let panel_cmds = crate::panels::audio_mixer::show(ui, &mut self.state, &self.theme);
            all_cmds.extend(panel_cmds);
        });
    }

    fn render_timeline_panel(
        &mut self,
        ui: &mut egui::Ui,
        timeline_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, timeline_rect, |ui| {
            ui.set_min_size(timeline_rect.size());
            // Stash the active theme into the timeline's thread-local
            // so leaf render helpers (which fan out across thousands
            // of paint calls per frame) can pick up the right palette
            // without a parameter cascade through every signature.
            crate::panels::timeline::sync_theme(&self.theme);
            let panel_cmds = self.new_timeline.show(
                ui,
                &mut self.state,
                &self.theme,
                &mut self.timeline,
                &self.key_bindings,
                self.prefs.editing.adaptive_timeline_fit,
            );
            all_cmds.extend(panel_cmds);
        });
    }

    /// The collapsed browser's rail: its three tabs, stacked, each of
    /// which reopens the panel on that tab.
    /// In the Colour workspace, the clip under the playhead is the clip
    /// being graded.
    ///
    /// Every grading suite works this way: you scrub to a shot and the
    /// panel is already showing that shot's grade. Klipp selected only
    /// on click, so moving the playhead left the panel pointed at
    /// whatever was last clicked — which does not merely cost a click,
    /// it invites grading the wrong shot and not noticing.
    ///
    /// Only when the playhead has actually moved. A click that selects
    /// a clip without moving the playhead is left alone, so selecting
    /// and grading a clip that is not under the playhead still works
    /// and the two never fight over the same frame. Edit is untouched:
    /// there the selection is the user's, and a scrub should not
    /// silently reselect under them.
    pub(super) fn follow_playhead_for_grading(&mut self) {
        if self.state.active_workspace != crate::state::Workspace::Color {
            // Forget the last position, so returning to Colour follows
            // immediately rather than waiting for the next move.
            self.last_graded_playhead = None;
            return;
        }
        let now = self.state.playhead;
        if self.last_graded_playhead == Some(now) {
            return;
        }
        self.last_graded_playhead = Some(now);

        // The topmost enabled video track that has a clip here — what
        // the viewer is showing, and so what the grade should address.
        let Some(seq) = self.state.sequences.get(self.state.active_sequence_idx) else { return };
        let found = seq
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video) && t.visible)
            .find_map(|t| t.clip_at_timecode(now).map(|(_, c)| c.id));

        if let Some(id) = found {
            self.graded_positions.insert(id, now);
        }
        match found {
            Some(id) if self.state.selected_clip != Some(id) => {
                self.state.selected_clip = Some(id);
                self.state.selected_clips.clear();
            }
            // No clip under the playhead: leave the panel on the last
            // one rather than blanking it, so a gap between shots does
            // not throw away the context you were working in.
            _ => {}
        }
    }

    fn render_browser_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use crate::layout_state::BrowserTab;
        use crate::widgets::panel_rail::{self, RailAction, RailGlyph, RailItem, RailSide};

        let active = self.layout.active_browser_tab;
        let tabs = [BrowserTab::Files, BrowserTab::Media, BrowserTab::Library];
        let items = [
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::Folder), label: "Files", active: active == BrowserTab::Files },
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::FilmStrip), label: "Media", active: active == BrowserTab::Media },
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::Sliders), label: "Library", active: active == BrowserTab::Library },
        ];
        let theme = self.theme.clone();
        match panel_rail::show(ui, rect, &theme, RailSide::Left, &items, "browser") {
            RailAction::None => {}
            RailAction::Open(i) => {
                if let Some(tab) = tabs.get(i) {
                    self.layout.active_browser_tab = *tab;
                }
                self.set_browser_visible(true);
            }
        }
    }

    /// The collapsed inspector's rail. Its tabs depend on what is
    /// selected, so the rail shows the ones the panel would show.
    fn render_inspector_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use crate::layout_state::InspectorTab;
        use crate::widgets::panel_rail::{self, RailAction, RailGlyph, RailItem, RailSide};

        let active = self.layout.active_inspector_tab;
        let tabs = [InspectorTab::Video, InspectorTab::Audio, InspectorTab::Color, InspectorTab::Effects, InspectorTab::Info];
        // The inspector's own short forms, so the rail says what the tab
        // row says when it is narrow.
        let items = [
            RailItem { glyph: RailGlyph::Text("VID"), label: "Video", active: active == InspectorTab::Video },
            RailItem { glyph: RailGlyph::Text("AUD"), label: "Audio", active: active == InspectorTab::Audio },
            RailItem { glyph: RailGlyph::Text("COL"), label: "Colour", active: active == InspectorTab::Color },
            RailItem { glyph: RailGlyph::Text("FX"), label: "Effects", active: active == InspectorTab::Effects },
            RailItem { glyph: RailGlyph::Text("INFO"), label: "Info", active: active == InspectorTab::Info },
        ];
        let theme = self.theme.clone();
        match panel_rail::show(ui, rect, &theme, RailSide::Right, &items, "inspector") {
            RailAction::None => {}
            RailAction::Open(i) => {
                if let Some(tab) = tabs.get(i) {
                    self.layout.active_inspector_tab = *tab;
                }
                self.set_inspector_visible(true);
            }
        }
    }

    /// Show or hide the browser, and remember which across restarts.
    /// Visibility lives in two places — `AppState` is what the frame
    /// reads, `LayoutState` is what survives — so it is set in one
    /// place rather than at each call site, where the pair would drift.
    /// Run the enter/leave bookkeeping for a workspace switch, whoever
    /// made it.
    ///
    /// Called every frame rather than from the switch sites: there are
    /// three of those and they disagreed. The `C` shortcut borrowed the
    /// browser and the toolbar tabs did not, so the same switch gave
    /// two different layouts depending on how you asked for it. The
    /// decision itself lives in `state::browser_borrow_for`, where it
    /// can be tested without an app.
    pub(super) fn reconcile_workspace(&mut self) {
        use crate::state::{borrow_released_by_hand, browser_borrow_for, BrowserBorrow, Workspace};
        let to = self.state.active_workspace;
        if to != self.last_workspace {
            let from = std::mem::replace(&mut self.last_workspace, to);

            // The colour panel belongs to its workspace, by any route.
            // (Scopes deliberately do not: entering Colour used to
            // force them on, which meant `C` always popped the full
            // scope UI even when the grade tabs were all you wanted.)
            self.state.show_color_panel = to == Workspace::Color;

            // Render the playhead's frame again for the new page. The
            // Colour page opened on a black viewer until the playhead
            // was moved; while parked this is a regrade of the parked
            // frame, and a real seek only when there is none.
            if self.state.playback != PlaybackState::Playing {
                let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
            }

            match browser_borrow_for(from, to) {
                BrowserBorrow::Take => {
                    self.browser_before_workspace = Some(self.state.show_browser_panel);
                    self.set_browser_visible_for_now(false);
                }
                BrowserBorrow::KeepTaken => self.set_browser_visible_for_now(false),
                BrowserBorrow::Give => {
                    if let Some(was) = self.browser_before_workspace.take() {
                        self.set_browser_visible_for_now(was);
                    }
                }
                BrowserBorrow::Leave => {}
            }
            return;
        }

        if borrow_released_by_hand(
            self.browser_before_workspace.is_some(),
            to,
            self.state.show_browser_panel,
        ) {
            self.browser_before_workspace = None;
        }
    }

    /// Show or hide the browser for this session only, without
    /// recording it as what the user wants.
    ///
    /// A borrow is not a preference — it lasts exactly as long as the
    /// workspace holding it. Writing it to the layout file turns
    /// "hidden while grading" into "hidden": quit from Colour or
    /// Export and the next launch opens in Edit with no browser and
    /// nothing left that remembers there should be one. That is what
    /// it did, and the saved layout on this machine was already in
    /// that state.
    ///
    /// An explicit toggle still goes through [`Self::set_browser_visible`]
    /// and is still remembered, because that one IS a preference.
    fn set_browser_visible_for_now(&mut self, visible: bool) {
        self.state.show_browser_panel = visible;
    }

    pub(super) fn set_browser_visible(&mut self, visible: bool) {
        self.node_browser_borrow.manual_override();
        self.state.show_browser_panel = visible;
        if self.layout.browser_visible != visible {
            self.layout.browser_visible = visible;
            self.layout_dirty = true;
        }
    }

    /// Show or hide the inspector, and remember which across restarts.
    pub(super) fn set_inspector_visible(&mut self, visible: bool) {
        self.state.show_inspector_panel = visible;
        if self.layout.inspector_visible != visible {
            self.layout.inspector_visible = visible;
            self.layout_dirty = true;
        }
    }

    fn render_inspector_panel(
        &mut self,
        ui: &mut egui::Ui,
        inspector_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Refresh the letter-picker texture BEFORE the inspector
        // tabs render. Texture lifecycle stays in the App layer so
        // the old handle goes through `defer_drop_texture`; the
        // inspector itself reads `state.letter_picker_texture`
        // without touching wgpu (creating/dropping a TextureHandle
        // mid-frame from inside the inspector trips wgpu's
        // "Texture has been destroyed" validation —
        // feedback_defer_drop_texture).
        self.refresh_letter_picker_texture(ui.ctx());

        // The Export column's title row carries the same collapse button
        // as the clip inspector's; it is applied after the closure so
        // the borrow of `self` inside it stays untouched.
        let mut collapse_requested = false;
        let bg_panel = self.theme.bg_panel;
        crate::ui_util::allocate_clipped_ui(ui, inspector_rect, |ui| {
            ui.set_min_size(inspector_rect.size());
            ui.painter().rect_filled(inspector_rect, 0.0, bg_panel);
            // egui's ScrollArea paints its scrollbar gutter with
            // visuals.extreme_bg_color, which globally is bg_void (near-black)
            // and reads as a dark strip against the inspector's bg_panel. Override
            // it locally so the gutter blends into the inspector background.
            ui.style_mut().visuals.extreme_bg_color = bg_panel;

            // The Export workspace's column is the export form.
            if self.state.active_workspace == crate::state::Workspace::Export {
                let theme = self.theme.clone();
                self.prepare_export_defaults();
                if crate::widgets::inspector_group::panel_title_row(ui, "Export", false, &theme) {
                    collapse_requested = true;
                }
                let all_presets: Vec<ExportPreset> = builtin_presets()
                    .into_iter()
                    .chain(self.prefs.export.custom_presets.iter().cloned())
                    .collect();
                // Reserve the foot of the column for the actions before
                // the form gets to claim the space, so Export and Add to
                // Queue are on screen at every window height. The form
                // scrolls inside what is left.
                let full = ui.available_rect_before_wrap();
                let foot = export_foot_layout(full.height());
                let bar_h = foot.bar_h;
                let body_rect = egui::Rect::from_min_max(
                    full.min,
                    egui::pos2(full.max.x, full.max.y - bar_h),
                );
                let bar_rect = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.max.y - bar_h),
                    full.max,
                );

                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                    egui::ScrollArea::vertical().id_salt("export_ws_scroll").show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.export_settings_contents(ui, &all_presets, all_cmds, false);
                        ui.add_space(12.0);
                        // The queue lives under the form here; the Render
                        // Queue window is the same list elsewhere.
                        let jobs = self.state.render_queue.jobs.len();
                        let badge = if jobs > 0 { Some(format!("{jobs}")) } else { None };
                        let mut actions = Vec::new();
                        crate::widgets::inspector_group::section_card(ui, "RENDER QUEUE", badge, &theme, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.vertical(|ui| {
                                    ui.set_max_width(ui.available_width() - 12.0);
                                    crate::panels::render_queue::RenderQueuePanel::show_contents(
                                        ui, &self.state.render_queue, &theme, true, &mut actions,
                                    );
                                });
                            });
                        });
                        self.apply_render_queue_actions(actions);
                    });
                });

                // The bar sits on the panel's own background with a rule
                // above it, so it reads as a pinned foot rather than as
                // the last thing in the form.
                ui.painter().rect_filled(bar_rect, 0.0, theme.bg_panel);
                ui.painter().hline(
                    bar_rect.x_range(),
                    bar_rect.top(),
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                    // Centre the whole foot — summary line and buttons
                    // together — in the reserved strip, rather than the
                    // button row alone, which left the summary hanging
                    // off the top and the buttons off the bottom.
                    // Nothing at all in a strip with no room for it —
                    // a panel collapsed or mid-resize draws no half
                    // button rather than a clipped one.
                    if foot.content_h > 0.0 {
                        ui.add_space(foot.pad);
                        self.export_action_bar(ui, all_cmds, false, foot.show_summary);
                    }
                });
                return;
            }

            let in_color_workspace = self.state.show_color_panel
                && self.state.selected_clip.is_some();

            if in_color_workspace {
                let clip_id = self.state.selected_clip.unwrap();
                let seq_idx = self.state.active_sequence_idx;
                let mask_tolerance = crate::inspector::keyframe_selection::frame_tolerance(&self.state.sequences[seq_idx]);
                self.node_inspector_panel.input_colour_space =
                    crate::inspector::color_and_luts::clip_input_colour_space(&self.state, clip_id);
                // A stand-in: the timeline grade or a grade group's stage.
                let stand_in = GradeStandIn::of(&self.state.sequences[seq_idx], clip_id);
                let stand_in_title = stand_in.map(|w| w.title(&self.state.sequences[seq_idx]));
                // A stand-in's grade is drawn at its own root (M3 step 19),
                // which takes the sequence to read: taken before the clip
                // is borrowed to draw.
                let mut stand_in_snapshot = stand_in.and_then(|w| {
                    let seq = &self.state.sequences[seq_idx];
                    crate::inspector::grade_view::snapshot_for_root(seq, clip_id, w.root(seq), &self.state.graph_descent_path, &mut self.state.pending_corrector, &mut self.state.root_grade_start)
                });
                // The shots either side of the graded one, for a wipe (#899).
                let wiping = self.state.wipe_clip;
                let wipe_neighbours = {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let graded = crate::graded_thumbs::viewed_clip(seq, self.state.selected_clip, self.state.playhead);
                    let n = |forward| graded.and_then(|g| crate::graded_thumbs::neighbour(seq, g, forward));
                    (n(false), n(true))
                };
                let clip = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(clip_id));
                if let Some(clip) = clip {
                    let theme = self.theme.clone();
                    let stand_in_name = stand_in_title.clone();
                    let title = if let Some(t) = stand_in_title {
                        t
                    } else {
                        clip
                            .media_id()
                            .and_then(|m| self.state.media_pool.get(&m))
                            .map(|m| m.filename.clone())
                            .unwrap_or_else(|| "Clip".into())
                    };
                    if crate::widgets::inspector_group::panel_title_row(ui, &title, false, &theme) {
                        collapse_requested = true;
                    }

                    // Versions, stills and looks: the grade's own library,
                    // one section above the tabs, collapsible like any
                    // other section.
                    let version_badge = clip
                        .active_grade_version()
                        .map(|i| {
                            let name = &clip.grade_versions()[i].name;
                            if name.is_empty() { format!("V{}", i + 1) } else { name.clone() }
                        });
                    let caption_row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.horizontal(|ui| {
                            ui.set_min_height(crate::theme::control::COMPACT);
                            ui.add_space(12.0);
                            ui.add_sized(
                                [56.0, crate::theme::control::COMPACT],
                                egui::Label::new(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION).color(theme.text_dim)).selectable(false),
                            );
                            add(ui);
                        });
                    };
                    // Grading the timeline: `clip` is its stand-in. Its
                    // graph is the one thing written back, so versions
                    // saved on it would be lost; stills and Looks land on
                    // the timeline grade, and say so.
                    let grading_timeline = stand_in.is_some();
                    crate::widgets::inspector_group::section_card(ui, "GRADE", version_badge, &theme, |ui| {
                        caption_row(ui, "Versions", &mut |ui| {
                            let row = ui.add_enabled_ui(!grading_timeline, |ui| {
                                let num_versions = clip.grade_versions().len();
                                let active = clip.active_grade_version();
                                for i in 0..num_versions {
                                    let is_active = active == Some(i);
                                    let label = {
                                        let name = &clip.grade_versions()[i].name;
                                        if name.is_empty() { format!("V{}", i + 1) } else { name.clone() }
                                    };
                                    let resp = ui.selectable_label(is_active, egui::RichText::new(&label).size(crate::theme::type_scale::CAPTION));
                                    if resp.clicked() && !is_active {
                                        all_cmds.push(AppCommand::SwitchGradeVersion { clip_id, version_index: i });
                                    }
                                    let current_name = clip.grade_versions()[i].name.clone();
                                    resp.context_menu(|ui| {
                                        // The version's name, typed (#902): Rename used
                                        // to write a fixed "Version N".
                                        let id = ui.id().with(("grade_version_name", clip_id.0, i));
                                        let mut buf: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_else(|| current_name.clone());
                                        ui.label(egui::RichText::new("Name").size(crate::theme::type_scale::CAPTION).weak());
                                        let field = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(140.0));
                                        let entered = field.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                                        if (entered || ui.button("Rename").clicked()) && !buf.trim().is_empty() {
                                            all_cmds.push(AppCommand::RenameGradeVersion {
                                                clip_id,
                                                version_index: i,
                                                name: buf.trim().to_string(),
                                            });
                                            ui.data_mut(|d| d.remove::<String>(id));
                                            ui.close_menu();
                                        } else {
                                            ui.data_mut(|d| d.insert_temp(id, buf));
                                        }
                                        ui.separator();
                                        if !is_active && ui.button("Delete").clicked() {
                                            all_cmds.push(AppCommand::DeleteGradeVersion { clip_id, version_index: i });
                                            ui.close_menu();
                                        }
                                    });
                                }
                                if ui.small_button("+").on_hover_text("Save current grade as new version").clicked() {
                                    let name = format!("Version {}", num_versions + 1);
                                    all_cmds.push(AppCommand::SaveGradeVersion { clip_id, name });
                                }
                            });
                            if grading_timeline {
                                row.response.on_hover_text("Versions are kept per clip; the timeline grade has none. Save it as a Look to keep a copy.");
                            }
                        });

                        // Apply Grade from Still goes to the timeline
                        // selection, or the one selected clip, as Looks do;
                        // to the timeline grade when that is what is graded.
                        let grade_targets: Vec<klipp_core::ClipId> = if grading_timeline {
                            vec![clip_id]
                        } else if !self.state.selected_clips.is_empty() {
                            self.state.selected_clips.clone()
                        } else {
                            self.state.selected_clip.into_iter().collect()
                        };
                        caption_row(ui, "Stills", &mut |ui| {
                            let mut delete_idx = None;
                            let mut apply_idx = None;
                            for (i, still) in self.state.grade_stills.iter_mut().enumerate() {
                                let is_active = self.state.active_grade_still == Some(i);
                                if still.texture.is_none() && !still.rgba.is_empty() {
                                    let sw = still.width as usize;
                                    let sh = still.height as usize;
                                    let pixels: Vec<egui::Color32> = still.rgba.chunks_exact(4)
                                        .map(|px| egui::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                                        .collect();
                                    let image = egui::ColorImage { size: [sw, sh], pixels };
                                    let name = format!("grade_still_thumb_{}", i);
                                    still.texture = Some(alloc_texture_logged(
                                        ui.ctx(),
                                        &name,
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    ));
                                }
                                let btn_size = egui::vec2(36.0, 22.0);
                                let (rect, resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                                if let Some(tex) = &still.texture {
                                    ui.painter().image(
                                        tex.id(),
                                        rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE,
                                    );
                                }
                                let stroke = if is_active {
                                    egui::Stroke::new(1.5_f32, theme.accent)
                                } else {
                                    egui::Stroke::new(1.0_f32, theme.border)
                                };
                                ui.painter().rect_stroke(rect, crate::theme::radius::CONTROL, stroke);
                                if resp.clicked() {
                                    all_cmds.push(AppCommand::SetActiveGradeStill {
                                        index: if is_active { None } else { Some(i) },
                                    });
                                }
                                let hover = match &still.source {
                                    Some(src) => format!(
                                        "{}\nFrom {} at {}",
                                        still.name,
                                        if src.clip_name.is_empty() { "a clip" } else { src.clip_name.as_str() },
                                        record_timecode(src.record_secs, self.state.fps.as_f64()),
                                    ),
                                    None => still.name.clone(),
                                };
                                let resp = resp.on_hover_text(hover);
                                let has_grade = still.grade.is_some();
                                resp.context_menu(|ui| {
                                    let why = if has_grade { "Select the clips to grade" } else { "This still was grabbed before stills kept their grade" };
                                    let can = has_grade && !grade_targets.is_empty();
                                    let (apply_label, apply_hover, add_label, add_hover) = if grading_timeline {
                                        ("Apply Grade to Timeline Grade", "In place of the timeline grade", "Add Grade After Timeline Grade", "After the timeline grade, keeping it")
                                    } else {
                                        ("Apply Grade to Selected Clips", "In place of each clip's own grade", "Add Grade After Their Own", "After each clip's own grade, keeping its balance")
                                    };
                                    if ui.add_enabled(can, egui::Button::new(apply_label))
                                        .on_hover_text(apply_hover)
                                        .on_disabled_hover_text(why)
                                        .clicked()
                                    {
                                        apply_idx = Some((i, false));
                                        ui.close_menu();
                                    }
                                    if ui.add_enabled(can, egui::Button::new(add_label))
                                        .on_hover_text(add_hover)
                                        .on_disabled_hover_text(why)
                                        .clicked()
                                    {
                                        apply_idx = Some((i, true));
                                        ui.close_menu();
                                    }
                                    if ui.button("Delete").clicked() {
                                        delete_idx = Some(i);
                                        ui.close_menu();
                                    }
                                });
                            }
                            if let Some(idx) = delete_idx {
                                all_cmds.push(AppCommand::DeleteGradeStill { index: idx });
                            }
                            if let Some((idx, append)) = apply_idx {
                                all_cmds.push(AppCommand::ApplyGradeFromStill { index: idx, clip_ids: grade_targets.clone(), append });
                            }
                            // The active still's grade onto the selection:
                            // grab a finished shot, then grade the next ones
                            // from the same setup from it.
                            let active_grade = self.state.active_grade_still.filter(|&i| self.state.grade_stills.get(i).is_some_and(|s| s.grade.is_some()));
                            let (apply_label, apply_hover) = match stand_in_name.as_deref() {
                                Some(name) => (format!("Apply to {name}"), format!("Put the active still's grade on {name}, in place of it")),
                                None => ("Apply".to_string(), "Put the active still's grade on the selected clips".to_string()),
                            };
                            if ui
                                .add_enabled(active_grade.is_some() && !grade_targets.is_empty(), egui::Button::new(apply_label).small())
                                .on_hover_text(apply_hover)
                                .on_disabled_hover_text("Pick a still that kept its grade, and select the clips to grade")
                                .clicked()
                            {
                                if let Some(idx) = active_grade {
                                    all_cmds.push(AppCommand::ApplyGradeFromStill { index: idx, clip_ids: grade_targets.clone(), append: false });
                                }
                            }
                            if ui.small_button("Grab").on_hover_text("Capture the current frame as a grade still").clicked() {
                                let name = format!("Still {}", self.state.grade_stills.len() + 1);
                                all_cmds.push(AppCommand::CaptureGradeStill { name });
                            }
                            let compare_active = self.state.grade_still_compare;
                            if ui.selectable_label(compare_active, "Compare")
                                .on_hover_text("Split the viewer against the active still")
                                .clicked()
                            {
                                all_cmds.push(AppCommand::ToggleGradeStillCompare);
                            }
                            // Wipe against the shot before or after (#899):
                            // the split's left side that clip, graded. A
                            // second click on the lit one ends it.
                            for (label, target, hover, that) in [
                                ("\u{25C0} Clip", klipp_core::WipeClip::Previous, "Wipe against the clip before this one, graded", wipe_neighbours.0),
                                ("Clip \u{25B6}", klipp_core::WipeClip::Next, "Wipe against the clip after this one, graded", wipe_neighbours.1),
                            ] {
                                let lit = that.is_some() && that == wiping;
                                if ui.add_enabled(that.is_some(), egui::Button::new(label).small().selected(lit)).on_hover_text(hover).clicked() {
                                    all_cmds.push(AppCommand::WipeAgainstClip { target: (!lit).then_some(target) });
                                }
                            }
                            // Alt+B's split, shown and switchable here: the
                            // viewer showing half a frame ungraded should
                            // never be a state you cannot see is on.
                            let before_after = self.state.before_after_active;
                            if ui.selectable_label(before_after, "Before/After")
                                .on_hover_text(format!(
                                    "Split the viewer: left ungraded, right graded ({})",
                                    self.key_bindings.display_for("toggle_before_after")
                                ))
                                .clicked()
                            {
                                all_cmds.push(AppCommand::ToggleBeforeAfter);
                            }
                        });

                        // Looks: project-level reusable grades. Saves the
                        // selected clip's graph, applies to the timeline
                        // selection (or the single selected clip), or to
                        // the timeline grade while that is what is graded.
                        let looks_cmds = crate::panels::looks::show(
                            ui,
                            &mut self.looks_panel_state,
                            &self.state.looks,
                            self.state.selected_clip,
                            if grading_timeline { &[] } else { &self.state.selected_clips },
                            stand_in_name.as_deref(),
                            &theme,
                        );
                        all_cmds.extend(looks_cmds);

                        // The grade library, across projects (#899).
                        let library_cmds = crate::panels::grade_library::show(
                            ui,
                            &mut self.grade_library_panel_state,
                            &mut self.state.grade_library,
                            Some(clip_id),
                            &grade_targets,
                            &theme,
                        );
                        all_cmds.extend(library_cmds);
                    });

                    // Swap between tabbed grade inspector and per-node
                    // inspector based on what the user selected in the
                    // V2 canvas. The tabs cover the primary grade path
                    // (Corrector + legacy single-purpose grade nodes);
                    // nodes that have their own dedicated widget
                    // (Mask, Qualifier, FilmGrain, Lut3D, ColorMatch,
                    // LayerMixer, ExternalMatte, Denoise, ColorWarper)
                    // need the per-node inspector when the user clicks
                    // them — otherwise clicking e.g. an ExternalMatte
                    // in the canvas would hide its file picker behind
                    // the grade tabs.
                    let mask_frame = crate::inspector::keyframe_selection::FrameSelection::new(self.state.playhead, clip.timeline_in, mask_tolerance);
                    // Descent-aware graph: when the user is inside a
                    // compound, the inspector should read/write the
                    // sub-graph's nodes. Fall back to the root graph
                    // if the descent path has gone stale (self-heal
                    // on the next frame).
                    let path = self.state.graph_descent_path.clone();
                    // The generated layout (M3 step 11) draws a node of the
                    // clip's own grade from its catalog type, by path.
                    // A stand-in's (the timeline grade, a group stage) too,
                    // at its own root (M3 step 19).
                    // Inside a compound too: its nodes have paths (#1391).
                    let generated_node = self.state.selected_node_id.filter(|nid| {
                        crate::inspector::grade_view::v1_level(klipp_timeline::GradeAccess::grade(&*clip), &path).is_some_and(|g| g.nodes.iter().any(|n| n.id == *nid && per_node_kind(n.kind)))
                    });
                    // The tabbed page draws what it shares with the
                    // per-node layout as the same generated rows (11c).
                    self.node_inspector_panel.generated = if grading_timeline {
                        stand_in_snapshot.take()
                    } else {
                        crate::inspector::grade_view::snapshot_for(clip, &path, false, &mut self.state.pending_corrector)
                    };
                    let mut generated_cmds = Vec::new();
                    let drawn = generated_node.is_some_and(|nid| {
                        egui::ScrollArea::vertical()
                            .id_salt("node_inspector_scroll")
                            .show(ui, |ui| {
                                if !grading_timeline {
                                    return crate::inspector::grade_view::show(ui, &self.theme, clip, &path, nid, &mut generated_cmds, mask_frame, Some(&mut self.node_inspector_panel));
                                }
                                let snapshot = self.node_inspector_panel.generated.take();
                                let drawn = snapshot.as_ref().is_some_and(|s| s.page(ui, &self.theme, nid, &mut generated_cmds, mask_frame, Some(&mut self.node_inspector_panel)));
                                self.node_inspector_panel.generated = snapshot;
                                drawn
                            })
                            .inner
                    });
                    // The selected node's masks a tracker drives, as the
                    // generated view marks them (#1398).
                    self.node_inspector_panel.tracked_masks = self.state.selected_node_id.map(|n| crate::panels::node_inspector::tracked_masks_of(clip, n)).unwrap_or_default();
                    let panel_cmds = if drawn { generated_cmds } else { clip.edit_grade(|root| {
                        let graph: &mut klipp_nodes::NodeGraph =
                            match Self::resolve_graph_at_path_mut(root, &path) {
                                Ok(g) => g,
                                Err(_) => root,
                            };
                        let use_per_node_inspector = self.state.selected_node_id
                            .and_then(|nid| graph.nodes.iter().find(|n| n.id == nid))
                            .map(|n| per_node_kind(n.kind))
                            .unwrap_or(false);
                        if use_per_node_inspector {
                            self.node_inspector_panel.show(
                                ui, &self.state.selected_node_id,
                                graph, clip_id, &self.theme, mask_frame,
                            )
                        } else {
                            self.node_inspector_panel.show_simple(
                                ui, graph, clip_id,
                                &mut self.state.selected_node_id,
                                &mut self.state.color_tab,
                                &self.theme,
                                mask_frame,
                            )
                        }
                    }) };
                    self.node_inspector_panel.generated = None;
                    self.node_inspector_panel.tracked_masks.clear();
                    // A key the generated rows name at the playhead is the
                    // stored key in its frame.
                    let mut panel_cmds = panel_cmds;
                    {
                        // A stand-in's keys are its root's, stored in the
                        // grade the stand-in was put on with.
                        let grade = clip.grade().clone();
                        for cmd in &mut panel_cmds {
                            if grading_timeline {
                                crate::inspector::keyframe_selection::resolve_for_grade(&grade, mask_tolerance, cmd);
                            } else {
                                crate::inspector::keyframe_selection::resolve_for_clip(clip, mask_tolerance, cmd);
                            }
                        }
                    }
                    all_cmds.extend(panel_cmds);
                }
            } else {
                // A title layer's track in focus selects that layer once, so
                // the Title tab's rows show its keys (#2296).
                if let Some(f) = &self.state.param_focus {
                    if let crate::state::KeyframeParamKind::Title { object_id, .. } = f.kind {
                        if self.new_inspector.layer_followed != Some(f.at) {
                            self.new_inspector.layer_followed = Some(f.at);
                            self.state.selected_title_object = Some((f.clip_id, klipp_timeline::TitleObjectId(object_id)));
                        }
                    }
                }
                self.new_inspector.key_column = self.layout.inspector_keys;
                self.new_inspector.key_column_w = self.layout.inspector_keys_w;
                let panel_cmds = self.new_inspector.show(
                    ui, &self.state, &self.prefs, &self.theme,
                    &mut self.layout.active_inspector_tab,
                );
                all_cmds.extend(panel_cmds);
                // The Keyframes column, shown or resized there, kept (#2305).
                if (self.new_inspector.key_column, self.new_inspector.key_column_w) != (self.layout.inspector_keys, self.layout.inspector_keys_w) {
                    self.layout.inspector_keys = self.new_inspector.key_column;
                    self.layout.inspector_keys_w = self.new_inspector.key_column_w;
                    self.layout_dirty = true;
                }
                if let Some(area) = self.new_inspector.key_area.take() {
                    self.key_column.show(ui, &mut self.state, &self.theme, &area, all_cmds);
                }
                // A chip or a row picked: its parameter comes into focus (#2296).
                if let (Some(kind), Some(clip_id)) = (self.new_inspector.focus_request.take(), self.state.selected_clip_loc().map(|(_, c)| c.id)) {
                    self.state.param_focus = Some(crate::param_focus::ParamFocus { clip_id, kind, at: ui.input(|i| i.time) });
                }
            }
        });
        if collapse_requested {
            self.set_inspector_visible(false);
        }
        self.state.focus_title_text = self.state.focus_title_text.saturating_sub(1);
    }

    /// Scale the timeline about the playhead, so the frame under it
    /// stays put. Both zoom keys go through here rather than each
    /// keeping its own copy of the scroll arithmetic.
    pub(super) fn zoom_timeline_by(&mut self, factor: f32) {
        let playhead_secs = self.state.playhead_smooth_secs as f32;
        let old_zoom = self.state.timeline_zoom;
        let new_zoom = (old_zoom * factor).clamp(0.1, 2000.0);
        self.state.timeline_scroll_x = (playhead_secs * new_zoom
            - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
            .max(0.0);
        self.state.timeline_zoom = new_zoom;
    }

    fn render_browser_panel(
        &mut self,
        ui: &mut egui::Ui,
        browser_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // ── Prefs → browser sync (BEFORE show) ──
        // Pull any preferences-panel changes into the live browser state
        // before rendering. The browser owns its own copy of these fields
        // (so its toolbar widgets — thumbnail size buttons, sort dropdown,
        // type filter pill, etc. — can mutate them with normal egui state)
        // but the preferences panel is also allowed to write to the prefs
        // copy. This sync makes the prefs panel "win" any frame where it
        // changed a value, since the browser → prefs sync at the bottom
        // of this method runs AFTER show() and would otherwise overwrite
        // the prefs change with the stale browser value.
        if self.browser.file_thumb_size != self.prefs.file_browser.file_thumb_size {
            self.browser.file_thumb_size = self.prefs.file_browser.file_thumb_size;
        }
        if self.browser.files_sort_by != self.prefs.file_browser.file_sort_by {
            self.browser.files_sort_by = self.prefs.file_browser.file_sort_by;
        }
        if self.browser.files_sort_ascending != self.prefs.file_browser.file_sort_ascending {
            self.browser.files_sort_ascending = self.prefs.file_browser.file_sort_ascending;
        }
        if self.browser.group_by_date != self.prefs.file_browser.group_by_date {
            self.browser.group_by_date = self.prefs.file_browser.group_by_date;
        }
        if self.browser.visible_columns != self.prefs.file_browser.visible_columns {
            self.browser.visible_columns = self.prefs.file_browser.visible_columns.clone();
        }
        if self.browser.files_type_filter != self.prefs.file_browser.file_type_filter {
            self.browser.files_type_filter = self.prefs.file_browser.file_type_filter;
        }
        if self.browser.pinned_folders != self.prefs.file_browser.pinned_folders {
            self.browser.pinned_folders = self.prefs.file_browser.pinned_folders.clone();
        }
        if self.browser.favorite_effects != self.prefs.effects_browser.favorite_effects {
            self.browser.favorite_effects = self.prefs.effects_browser.favorite_effects.clone();
        }
        if self.browser.favorite_transitions != self.prefs.effects_browser.favorite_transitions {
            self.browser.favorite_transitions = self.prefs.effects_browser.favorite_transitions.clone();
        }
        if self.browser.favorite_titles != self.prefs.effects_browser.favorite_titles {
            self.browser.favorite_titles = self.prefs.effects_browser.favorite_titles.clone();
        }
        if self.browser.favorite_generators != self.prefs.effects_browser.favorite_generators {
            self.browser.favorite_generators = self.prefs.effects_browser.favorite_generators.clone();
        }

        // Mark the browser as the active panel when the user presses
        // anywhere inside it this frame. Single PanelKind for the
        // whole widget — the browser's internal active_browser_tab
        // and library_category resolve which specific search
        // TextEdit a Ctrl+F focus request lands on.
        crate::state::note_active_panel(
            &mut self.state,
            crate::state::PanelKind::Browser,
            ui.ctx(),
            browser_rect,
        );

        let bg_panel = self.theme.bg_panel;
        crate::ui_util::allocate_clipped_ui(ui, browser_rect, |ui| {
            ui.set_min_size(browser_rect.size());
            ui.painter().rect_filled(browser_rect, 0.0, bg_panel);
            let panel_cmds = self.browser.show(
                ui,
                &mut self.state,
                &self.theme,
                &mut self.layout.active_browser_tab,
                &mut self.layout.files_pinned,
                &mut self.layout.files_split_ratio,
            );
            all_cmds.extend(panel_cmds);

            // ── Browser → prefs sync (AFTER show) ──
            // Capture any toolbar-driven changes the user made inside the
            // browser this frame and persist them to disk. This direction
            // only fires when the browser modified its local state during
            // show(); the prefs → browser sync above already handled the
            // opposite direction, so the two can't fight each other.
            if self.browser.browser_view_mode != self.prefs.editing.media_browser_view_mode {
                self.prefs.editing.media_browser_view_mode = self.browser.browser_view_mode;
                let _ = self.prefs.save();
            }
            if self.browser.file_thumb_size != self.prefs.file_browser.file_thumb_size {
                self.prefs.file_browser.file_thumb_size = self.browser.file_thumb_size;
                let _ = self.prefs.save();
            }
            if self.browser.list_columns != self.prefs.file_browser.media_list_columns {
                self.prefs.file_browser.media_list_columns = self.browser.list_columns.clone();
                let _ = self.prefs.save();
            }
            if self.browser.files_sort_by != self.prefs.file_browser.file_sort_by
                || self.browser.files_sort_ascending != self.prefs.file_browser.file_sort_ascending
            {
                self.prefs.file_browser.file_sort_by = self.browser.files_sort_by;
                self.prefs.file_browser.file_sort_ascending = self.browser.files_sort_ascending;
                let _ = self.prefs.save();
            }
            if self.browser.group_by_date != self.prefs.file_browser.group_by_date {
                self.prefs.file_browser.group_by_date = self.browser.group_by_date;
                let _ = self.prefs.save();
            }
            if self.browser.visible_columns != self.prefs.file_browser.visible_columns {
                self.prefs.file_browser.visible_columns = self.browser.visible_columns.clone();
                let _ = self.prefs.save();
            }
            if self.browser.files_type_filter != self.prefs.file_browser.file_type_filter {
                self.prefs.file_browser.file_type_filter = self.browser.files_type_filter;
                let _ = self.prefs.save();
            }
            if self.browser.pinned_folders != self.prefs.file_browser.pinned_folders {
                self.prefs.file_browser.pinned_folders = self.browser.pinned_folders.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_effects != self.prefs.effects_browser.favorite_effects {
                self.prefs.effects_browser.favorite_effects = self.browser.favorite_effects.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_transitions != self.prefs.effects_browser.favorite_transitions {
                self.prefs.effects_browser.favorite_transitions = self.browser.favorite_transitions.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_titles != self.prefs.effects_browser.favorite_titles {
                self.prefs.effects_browser.favorite_titles = self.browser.favorite_titles.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_generators != self.prefs.effects_browser.favorite_generators {
                self.prefs.effects_browser.favorite_generators = self.browser.favorite_generators.clone();
                let _ = self.prefs.save();
            }
            // Save file-ratings cache to disk if changed.
            if self.browser.file_ratings_dirty {
                self.browser.file_ratings_dirty = false;
                crate::browser::save_file_ratings_cache(&self.browser.file_ratings);
            }
            // Sync recent_files_snapshot back to prefs (handles "Clear Recents").
            if self.state.recent_files_snapshot != self.prefs.file_browser.recent_files {
                self.prefs.file_browser.recent_files = self.state.recent_files_snapshot.clone();
                let _ = self.prefs.save();
            }
        });
    }

    fn render_status_bar_panel(&mut self, ui: &mut egui::Ui, statusbar_rect: egui::Rect) {
        let theme = &self.theme;
        let border = theme.border;
        let bg_void = theme.bg_void;
        crate::ui_util::allocate_clipped_ui(ui, statusbar_rect, |ui| {
            ui.set_min_size(statusbar_rect.size());
            ui.painter().rect_filled(statusbar_rect, 0.0, bg_void);
            ui.painter().hline(
                statusbar_rect.x_range(),
                statusbar_rect.min.y,
                egui::Stroke::new(1.0_f32, border),
            );
            crate::panels::status_bar::show(ui, &self.state, &self.theme);
        });
    }
}

/// Whether a selected grade node of `kind` is shown in its own layout
/// rather than the grade tabs: the nodes with their own widget, and the
/// ones with none (whose tabs would mislead).
pub(crate) fn per_node_kind(kind: klipp_nodes::NodeKind) -> bool {
    matches!(kind,
        klipp_nodes::NodeKind::Mask
        | klipp_nodes::NodeKind::Qualifier
        | klipp_nodes::NodeKind::LumaKey
        | klipp_nodes::NodeKind::SoftClip
        // Its zones' rows are its own page (#240).
        | klipp_nodes::NodeKind::HdrZones
        | klipp_nodes::NodeKind::FilmGrain
        | klipp_nodes::NodeKind::Lut3D
        | klipp_nodes::NodeKind::ColorMatch
        | klipp_nodes::NodeKind::LayerMixer
        | klipp_nodes::NodeKind::ExternalMatte
        | klipp_nodes::NodeKind::DifferenceKey
        | klipp_nodes::NodeKind::Denoise
        | klipp_nodes::NodeKind::ColorWarper
        // No tab draws its From and To (#1351).
        | klipp_nodes::NodeKind::PrimariesMatrix
        // The narrow inspector needs its numeric Sat/Bri rows (#1351).
        | klipp_nodes::NodeKind::WheelSatBri
        // Source / Output / composite-only nodes:
        // no params, but showing the grade tabs
        // for them misleads users into thinking
        // clicking one does something. Route to
        // the per-node inspector which falls
        // through to a plain "No parameters" /
        // node-specific placeholder label.
        | klipp_nodes::NodeKind::Source
        | klipp_nodes::NodeKind::Output
        | klipp_nodes::NodeKind::Layer
        | klipp_nodes::NodeKind::Blend
        | klipp_nodes::NodeKind::CombineMattes
        | klipp_nodes::NodeKind::KeyMixer
        | klipp_nodes::NodeKind::ApplyMatte
        | klipp_nodes::NodeKind::Compound
        | klipp_nodes::NodeKind::RGBSplitter
        | klipp_nodes::NodeKind::RGBCombiner
    )
}

/// The control socket's `show_panel` test hook (M3 step 18): the named
/// workspace and inspector tab, and the inspector shown whatever was named,
/// a workspace alone or nothing. Layout only, never the document. Whether
/// the saved layout changed.
pub(super) fn show_panel(state: &mut crate::state::AppState, layout: &mut crate::layout_state::LayoutState, workspace: Option<&str>, inspector_tab: Option<&str>) -> bool {
    use crate::layout_state::InspectorTab;
    use crate::state::Workspace;
    match workspace {
        Some("edit") => state.active_workspace = Workspace::Edit,
        Some("colour" | "color") => state.active_workspace = Workspace::Color,
        Some("export") => state.active_workspace = Workspace::Export,
        _ => {}
    }
    let tab = match inspector_tab {
        Some("video") => Some(InspectorTab::Video),
        Some("audio") => Some(InspectorTab::Audio),
        Some("colour" | "color") => Some(InspectorTab::Color),
        Some("effects") => Some(InspectorTab::Effects),
        Some("title") => Some(InspectorTab::Title),
        Some("generator") => Some(InspectorTab::Generator),
        Some("info") => Some(InspectorTab::Info),
        _ => None,
    };
    if let Some(tab) = tab {
        layout.active_inspector_tab = tab;
    }
    // As `KlippApp::set_inspector_visible(true)`.
    state.show_inspector_panel = true;
    !std::mem::replace(&mut layout.inspector_visible, true)
}

#[cfg(test)]
mod show_panel_tests {
    use super::show_panel;
    use crate::layout_state::{InspectorTab, LayoutState};
    use crate::state::{AppState, Workspace};

    fn hidden() -> (AppState, LayoutState) {
        let (mut state, mut layout) = (AppState::default(), LayoutState::default());
        state.show_inspector_panel = false;
        layout.inspector_visible = false;
        (state, layout)
    }

    #[test]
    fn a_hidden_inspector_is_shown_whatever_is_named() {
        for (workspace, tab) in [(Some("colour"), Some("effects")), (Some("colour"), None), (None, None)] {
            let (mut state, mut layout) = hidden();
            assert!(show_panel(&mut state, &mut layout, workspace, tab), "{workspace:?} {tab:?}: the saved layout changed");
            assert!(state.show_inspector_panel && layout.inspector_visible, "{workspace:?} {tab:?}: the inspector is shown");
        }
    }

    #[test]
    fn the_named_workspace_and_tab_are_shown() {
        let (mut state, mut layout) = hidden();
        show_panel(&mut state, &mut layout, Some("colour"), Some("effects"));
        assert_eq!((state.active_workspace, layout.active_inspector_tab), (Workspace::Color, InspectorTab::Effects));
        show_panel(&mut state, &mut layout, Some("edit"), None);
        assert_eq!((state.active_workspace, layout.active_inspector_tab), (Workspace::Edit, InspectorTab::Effects), "a workspace alone keeps the tab");
        assert!(!show_panel(&mut state, &mut layout, None, Some("info")), "already shown: the saved layout is unchanged");
        assert_eq!(layout.active_inspector_tab, InspectorTab::Info);
    }
}
