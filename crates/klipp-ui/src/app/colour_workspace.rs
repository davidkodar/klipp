//! The Colour workspace's bottom: its toggle bar, its tools, the scopes and
//! their settings, and the node strip (#826).

use super::*;

impl KlippApp {
    pub(super) fn render_color_workspace_bottom(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        p: ColorWsParams,
    ) {
        let ColorWsParams {
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
        } = p;
        let theme = self.theme.clone();

        // Fill the entire bottom region with pure black FIRST so any
        // 1-2 px egui-spacing gaps between the toggle bar, scope
        // content, resizer, and the parent panel show #000000 instead
        // of leaking either the workspace's `bg_deep` (grey) or the
        // theme's `bg_void` (`#0a0a0c`, a near-but-not-pure black that
        // reads as a visible halo against the FFmpeg scope texture's
        // true `#000000` interior).
        if show_scopes_bar && !color_tools_bottom {
            let bottom_rect = egui::Rect::from_min_max(
                egui::pos2(center_left, toggle_bar_rect.min.y - theme::RESIZER_THICKNESS),
                egui::pos2(center_left + center_w, main_bottom),
            );
            ui.painter().rect_filled(bottom_rect, 0.0, egui::Color32::BLACK);
        }

        // Resizer between viewer and bottom panels (draggable)
        if show_scopes_bar || show_nodes_bar {
            let resizer_rect = egui::Rect::from_min_size(
                egui::pos2(center_left, toggle_bar_rect.min.y - theme::RESIZER_THICKNESS),
                egui::vec2(center_w, theme::RESIZER_THICKNESS),
            );
            // Idle line uses bg_void so the resizer sits flush against
            // the scope dock's black background instead of showing a
            // visible grey hairline directly above the toggle bar.
            let delta = resizer::horizontal_resizer_with_idle_color(
                ui,
                resizer_rect,
                &theme,
                theme.bg_void,
            );
            if delta != 0.0 {
                let new_ratio = self.layout.color_viewer_height_ratio + delta / flexible_h;
                self.layout.color_viewer_height_ratio = new_ratio.clamp(0.15, 0.90);
                self.layout_dirty = true;
            }
        }

        // Toggle bar: [Scopes] [Nodes] toggle buttons
        if toggle_bar_h > 0.0 {
            self.render_color_ws_toggle_bar(ui, all_cmds, toggle_bar_rect, &theme);
        }

        // Bottom panel: color tools (when docked at bottom) OR scopes (default)
        if color_tools_bottom {
            self.render_color_ws_bottom_tools(
                ui, all_cmds, center_left, center_w, scopes_h,
                main_top, main_bottom, toggle_bar_h, toggle_bar_rect, &theme,
            );
        } else if show_scopes_bar {
            self.render_color_ws_bottom_scopes(
                ui, center_left, center_w, scopes_h, main_top, main_bottom,
                toggle_bar_h, show_nodes_bar, toggle_bar_rect, bottom_scope_cursor,
            );
        }

        // Scope settings dropdown
        if self.state.scope_settings_open && show_scopes_bar {
            self.render_color_ws_scope_settings_dropdown(ctx, ui, toggle_bar_rect, &theme);
        }

        if nodes_resizer_rect.is_positive() {
            let delta = resizer::vertical_resizer(ui, nodes_resizer_rect, &theme);
            if delta != 0.0 {
                let usable = center_w - theme::RESIZER_THICKNESS;
                let width = (nodes_rect.width() - delta).clamp(240.0, usable - 320.0);
                self.layout.node_width_ratio = width / usable;
                self.layout_dirty = true;
            }
        }

        // Node strip panel — read live state
        if self.state.show_node_strip && nodes_rect.is_positive() {
            self.render_color_ws_node_strip(ui, all_cmds, nodes_rect, &theme);
        }
    }

    fn render_color_ws_toggle_bar(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, toggle_bar_rect, |ui| {
            ui.set_min_size(toggle_bar_rect.size());
            ui.painter().rect_filled(toggle_bar_rect, 0.0, theme.bg_panel);
            // Border between toggle bar and the scope content uses the
            // scope's own black background so there's no grey halo
            // between the bar and the trace area.
            ui.painter().hline(
                toggle_bar_rect.x_range(),
                toggle_bar_rect.min.y,
                egui::Stroke::new(0.5_f32, theme.bg_void),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(8.0);
                if ui.selectable_label(self.state.show_scopes, "Scopes").clicked() {
                    self.state.show_scopes = !self.state.show_scopes;
                    ui.ctx().request_repaint();
                }
                if ui.selectable_label(self.state.show_node_strip, "Nodes")
                    .on_hover_text("Show / hide the node graph")
                    .clicked()
                {
                    self.state.show_node_strip = !self.state.show_node_strip;
                    ui.ctx().request_repaint();
                }
                ui.menu_button("▾", |ui| {
                    use crate::layout_state::NodeGraphPlacement;
                    for (placement, label) in [(NodeGraphPlacement::Below, "Below viewer"), (NodeGraphPlacement::Right, "Right of viewer")] {
                        if ui.selectable_label(self.layout.node_graph_placement == placement, label).clicked() {
                            self.layout.node_graph_placement = placement;
                            self.state.show_node_strip = true;
                            self.layout_dirty = true;
                            ui.close_menu();
                        }
                    }
                    ui.label(egui::RichText::new("Compact windows place nodes below.").small().color(theme.text_dim));
                }).response.on_hover_text("Node graph placement");
                ui.separator();
                {
                    use crate::state::ColorToolsPosition;
                    let is_bottom = self.state.color_tools_position == ColorToolsPosition::Bottom;
                    // Labelled by what clicking does.
                    let label = if is_bottom { "Dock right \u{2192}" } else { "Dock bottom \u{2193}" };
                    let tooltip = if is_bottom {
                        "Move the colour tools panel to the right side of the canvas (currently at the bottom)"
                    } else {
                        "Move the colour tools panel to the bottom of the canvas (currently on the right)"
                    };
                    if ui.button(label).on_hover_text(tooltip).clicked() {
                        self.state.color_tools_position = if is_bottom {
                            ColorToolsPosition::Right
                        } else {
                            ColorToolsPosition::Bottom
                        };
                    }
                }
                ui.separator();
                // What the nodes and wheels grade: the selected clip, or
                // the timeline grade over every clip after its own.
                ui.label(egui::RichText::new("Grade").color(theme.text_dim));
                let grading_clip = !self.state.grade_timeline && self.state.grade_group_stage.is_none();
                if ui.selectable_label(grading_clip, "Clip")
                    .on_hover_text("Grade the selected clip")
                    .clicked()
                {
                    self.state.grade_timeline = false;
                    self.state.grade_group_stage = None;
                    self.state.graph_descent_path.clear();
                    self.state.selected_node_id = None;
                    self.state.selected_node_ids.clear();
                }
                // Marked while a timeline grade is doing something, so
                // its being there shows from Clip mode too.
                let has_timeline_grade = self.state.sequences.get(self.state.active_sequence_idx)
                    .and_then(|s| s.timeline_graph.as_ref())
                    .is_some_and(|g| !g.is_identity());
                let timeline_label = if has_timeline_grade && !self.state.grade_timeline {
                    egui::RichText::new("Timeline \u{25CF}").color(theme.accent)
                } else {
                    egui::RichText::new("Timeline")
                };
                self.grade_group_controls(ui, all_cmds, theme);
                if ui.selectable_label(self.state.grade_timeline, timeline_label)
                    .on_hover_text(
                        "Grade the whole sequence: runs on every frame after each clip's own grade, before the timeline LUT.\n\
                         Only when this sequence is the one played or exported: nested in another as a compound clip or a \
                         precomposition, it plays without its timeline grade.",
                    )
                    .clicked()
                {
                    self.state.grade_timeline = true;
                    self.state.grade_group_stage = None;
                    self.state.graph_descent_path.clear();
                    self.state.selected_node_id = None;
                    self.state.selected_node_ids.clear();
                }

                if self.state.show_scopes {
                    ui.separator();
                    use crate::state::{ScopeLayout, ScopeMode, ScopeScale};

                    // How many instruments the dock shows at once.
                    // Nine is deliberately absent: it belongs to the
                    // floating window, which can be dragged to whatever
                    // size nine panes need. In a dock sharing the
                    // workspace with the picture it would give each pane
                    // a strip too short to read.
                    let layouts = [
                        (ScopeLayout::OnePane, "1"),
                        (ScopeLayout::TwoPane, "2"),
                        (ScopeLayout::FourPane, "4"),
                    ];
                    let layout_labels: Vec<&str> = layouts.iter().map(|l| l.1).collect();
                    let layout_active = layouts
                        .iter()
                        .position(|l| l.0 == self.state.scope_dock.layout)
                        .unwrap_or(0);
                    if let Some(i) = crate::widgets::segmented::segmented(
                        ui, &layout_labels, layout_active, theme,
                        egui::FontId::proportional(crate::theme::type_scale::CAPTION),
                        crate::theme::control::COMPACT,
                    ) {
                        let picked = layouts[i].0;
                        if picked != self.state.scope_dock.layout {
                            self.state.scope_dock.layout = picked;
                            self.state.scope_dock.panes =
                                crate::state::PaneConfig::default_for_layout(picked);
                            // Going back to one pane, carry the pane the
                            // colourist was looking at into the single
                            // scope, rather than snapping to whatever
                            // `scope_mode` was left on before the grid.
                            if picked == ScopeLayout::OnePane {
                                self.state.scope_dock.panes =
                                    vec![crate::state::PaneConfig::new(self.state.scope_mode.into())];
                            }
                        }
                    }
                    ui.separator();

                    // Only meaningful for the single scope. Each pane
                    // in a grid carries its own type dropdown and its
                    // own channel settings in its header, so leaving a
                    // global Wave/Parade/Vector/Hist control up there
                    // would be a second, contradicting answer to "which
                    // scope am I looking at".
                    if self.state.scope_dock.layout == ScopeLayout::OnePane {
                        let modes = [
                            (ScopeMode::Waveform, "Wave"),
                            (ScopeMode::Parade, "Parade"),
                            (ScopeMode::Vectorscope, "Vector"),
                            (ScopeMode::Histogram, "Hist"),
                        ];
                        let labels: Vec<&str> = modes.iter().map(|m| m.1).collect();
                        let active = modes.iter().position(|m| m.0 == self.state.scope_mode).unwrap_or(0);
                        if let Some(i) = crate::widgets::segmented::segmented(
                            ui, &labels, active, theme,
                            egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT,
                        ) {
                            let mode = modes[i].0;
                            tracing::info!("UI: scope mode button clicked: {:?} (was {:?})", mode, self.state.scope_mode);
                            self.state.scope_mode = mode;
                            let mode_byte = match mode {
                                ScopeMode::Waveform => 0u8,
                                ScopeMode::Parade => 1,
                                ScopeMode::Vectorscope => 2,
                                ScopeMode::Histogram => 3,
                            };
                            let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: mode_byte });
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            // Force the per-mode staleness check to invalidate
                            // immediately so the scope dock re-renders this
                            // mode on the next frame instead of displaying
                            // its (possibly stale) cached texture.
                            self.state.scope_state.increment_settings_generation();
                            ui.ctx().request_repaint();
                        }

                        if matches!(self.state.scope_mode, ScopeMode::Waveform | ScopeMode::Parade) {
                            ui.separator();
                            for (label, color, enabled) in [
                                ("R", egui::Color32::from_rgb(0xd0, 0x40, 0x40), &mut self.state.scope_show_r),
                                ("G", egui::Color32::from_rgb(0x40, 0xb0, 0x40), &mut self.state.scope_show_g),
                                ("B", egui::Color32::from_rgb(0x40, 0x50, 0xd0), &mut self.state.scope_show_b),
                            ] {
                                let bg = if *enabled { color } else { theme.bg_raised };
                                let text_c = if *enabled { egui::Color32::WHITE } else { theme.text_dim };
                                let btn = egui::Button::new(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION).color(text_c).strong())
                                    .fill(bg)
                                    .min_size(egui::vec2(20.0, crate::theme::control::COMPACT - 4.0))
                                    .rounding(crate::theme::radius::CONTROL);
                                if ui.add(btn).on_hover_text(format!("Show the {label} channel")).clicked() { *enabled = !*enabled; }
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        // Glyph buttons: settings, floating window, picker.
                        let glyph = |ui: &mut egui::Ui, text: &str, on: bool, lit: bool| -> egui::Response {
                            let (r, resp) = ui.allocate_exact_size(egui::vec2(crate::theme::control::COMPACT, crate::theme::control::COMPACT), egui::Sense::click());
                            let (bg, fg) = if on {
                                (theme.accent_subtle, theme.accent)
                            } else if resp.hovered() {
                                (theme.bg_hover, theme.text_primary)
                            } else {
                                (egui::Color32::TRANSPARENT, if lit { theme.text_primary } else { theme.text_secondary })
                            };
                            ui.painter().rect_filled(r, crate::theme::radius::CONTROL, bg);
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), fg);
                            resp
                        };
                        if glyph(ui, "\u{2699}", self.state.scope_settings_open, false).on_hover_text("Scope settings").clicked() {
                            self.state.scope_settings_open = !self.state.scope_settings_open;
                        }
                        if glyph(ui, "\u{229e}", self.state.scope_window.open, false).on_hover_text("Floating scope window").clicked() {
                            self.state.scope_window.open = !self.state.scope_window.open;
                        }
                        let picker_active = self.state.scope_cursor_show;
                        let picker_locked = self.state.scope_cursor_locked;
                        let picker_resp = glyph(ui, "\u{2295}", picker_locked, picker_active);
                        if picker_resp.clicked() {
                            if picker_locked {
                                self.state.scope_cursor_locked = false;
                                self.state.scope_cursor_locked_pos = None;
                                self.state.scope_cursor_locked_data = None;
                            } else {
                                self.state.scope_cursor_show = !self.state.scope_cursor_show;
                            }
                        }
                        picker_resp.on_hover_text(if picker_locked {
                            "Click to unlock sample point"
                        } else if picker_active {
                            "Scope picker (click viewer to lock)"
                        } else {
                            "Enable scope picker"
                        });

                        egui::ComboBox::from_id_salt("scope_scale_sel")
                            .width(0.0)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .selected_text(crate::widgets::scopes::graticule_scale(self.state.scope_scale, self.state.hdr_monitor.renders_hdr()).button_label())
                            .show_ui(ui, |ui| {
                                for s in [ScopeScale::TenBit, ScopeScale::Ire, ScopeScale::Percent] {
                                    ui.selectable_value(&mut self.state.scope_scale, s, s.label());
                                }
                            });
                    });
                }
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_color_ws_bottom_tools(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        center_left: f32,
        center_w: f32,
        scopes_h: f32,
        main_top: f32,
        main_bottom: f32,
        toggle_bar_h: f32,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        let tools_h = if scopes_h > 0.0 { scopes_h } else {
            let ratio = self.layout.color_viewer_height_ratio.clamp(0.15, 0.90);
            let flex = (main_bottom - main_top - toggle_bar_h - theme::RESIZER_THICKNESS).max(200.0);
            (flex * (1.0 - ratio)).clamp(60.0, flex * 0.85)
        };
        let tools_rect = egui::Rect::from_min_size(
            egui::pos2(center_left, toggle_bar_rect.max.y),
            egui::vec2(center_w, tools_h),
        );
        crate::ui_util::allocate_clipped_ui(ui, tools_rect, |ui| {
            ui.set_min_size(tools_rect.size());
            ui.painter().rect_filled(tools_rect, 0.0, theme.bg_panel);
            if let Some(clip_id) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                let mask_tolerance = crate::inspector::keyframe_selection::frame_tolerance(&self.state.sequences[seq_idx]);
                self.node_inspector_panel.printer_point = self.prefs.color.printer_point;
                self.node_inspector_panel.input_colour_space =
                    crate::inspector::color_and_luts::clip_input_colour_space(&self.state, clip_id);
                let clip = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(clip_id));
                if let Some(clip) = clip {
                    let mask_frame = crate::inspector::keyframe_selection::FrameSelection::new(self.state.playhead, clip.timeline_in, mask_tolerance);
                    let path = self.state.graph_descent_path.clone();
                    let graph: &mut klipp_nodes::NodeGraph =
                        match Self::resolve_graph_at_path_mut(clip, &path) {
                            Ok(g) => g,
                            Err(_) => &mut clip.node_graph,
                        };
                    let panel_cmds = self.node_inspector_panel.show_simple_horizontal(
                        ui, graph, clip_id,
                        &mut self.state.selected_node_id,
                        &mut self.state.color_tab_bottom,
                        &self.theme,
                        mask_frame,
                    );
                    all_cmds.extend(panel_cmds);
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade")
                        .size(12.0).color(theme.text_dim));
                });
            }
        });
        // U-6: only drive a repaint when something's actually moving.
        // Engine thread calls ctx.request_repaint on every frame write,
        // and UI interactions (sliders, drags) already trigger paints
        // via egui. An idle colour workspace needs no refresh — the
        // unconditional repaint here used to pin the whole UI at max
        // FPS whenever the panel was visible.
        if self.state.playback == PlaybackState::Playing || self.state.is_scrubbing {
            ui.ctx().request_repaint();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_color_ws_bottom_scopes(
        &mut self,
        ui: &mut egui::Ui,
        center_left: f32,
        center_w: f32,
        scopes_h: f32,
        main_top: f32,
        main_bottom: f32,
        toggle_bar_h: f32,
        show_nodes_bar: bool,
        toggle_bar_rect: egui::Rect,
        bottom_scope_cursor: Option<crate::state::ScopeCursorData>,
    ) {
        let live_scopes_h = if scopes_h > 0.0 { scopes_h } else {
            let ratio = self.layout.color_viewer_height_ratio.clamp(0.15, 0.90);
            let flex = (main_bottom - main_top - toggle_bar_h - theme::RESIZER_THICKNESS).max(200.0);
            let total_bottom = (flex * (1.0 - ratio)).clamp(60.0, flex * 0.85);
            if show_nodes_bar { total_bottom * 0.6 } else { total_bottom }
        };
        let live_scopes_rect = egui::Rect::from_min_size(
            egui::pos2(center_left, toggle_bar_rect.max.y),
            egui::vec2(center_w, live_scopes_h),
        );
        crate::ui_util::allocate_clipped_ui(ui, live_scopes_rect, |ui| {
            ui.set_min_size(live_scopes_rect.size());
            // Use `bg_void` (the same colour the inner `show_scopes`
            // fills its `full_rect` with) instead of `bg_deep`. They
            // differ by ~7 % luminance, and any 1–2 px egui-spacing
            // inset between the dock rect and `full_rect` would leak
            // the lighter `bg_deep` as a visible ring around the
            // axis-label column and the trace. Matching the layers
            // makes the dock read as a single seamless black surface
            // — DaVinci-style flush black, no "panel" chrome around
            // the instrument glass.
            ui.painter().rect_filled(live_scopes_rect, 0.0, egui::Color32::BLACK);
            if self.state.scope_state.scale != self.state.scope_scale
                || self.state.scope_state.low_pass != self.state.scope_low_pass
                || self.state.scope_state.bloom != self.state.scope_bloom
                || self.state.scope_state.show_r != self.state.scope_show_r
                || self.state.scope_state.show_g != self.state.scope_show_g
                || self.state.scope_state.show_b != self.state.scope_show_b
                || self.state.scope_state.quality != self.state.scope_quality
            {
                self.state.scope_state.increment_settings_generation();
            }
            self.state.scope_state.scale = self.state.scope_scale;
            self.state.scope_state.legal_range = self.state.scope_legal_range;
            self.state.scope_state.hdr_levels = self.state.hdr_monitor.renders_hdr();
            self.state.scope_state.low_pass = self.state.scope_low_pass;
            self.state.scope_state.bloom = self.state.scope_bloom;
            self.state.scope_state.show_r = self.state.scope_show_r;
            self.state.scope_state.show_g = self.state.scope_show_g;
            self.state.scope_state.show_b = self.state.scope_show_b;
            self.state.scope_state.quality = self.state.scope_quality;

            // Above one pane the dock draws the same grid the floating
            // window does, from the same `draw_pane`: one instrument,
            // two places to put it. A colourist reading a grade wants
            // parade and vectorscope at the same time — judging chroma
            // off a scope you have to switch away from to see the other
            // one is not how the work is done.
            //
            // At one pane nothing changes: the original single-scope
            // renderer, driven by `scope_mode`, keeps the familiar
            // Wave/Parade/Vector/Hist control and the R/G/B buttons.
            if self.state.scope_dock.layout != crate::state::ScopeLayout::OnePane {
                self.scope_dock_runtime.hdr_levels = self.state.hdr_monitor.renders_hdr();
                crate::widgets::scope_window::show_shared_toggles(
                    &mut self.state.scope_dock,
                    self.state.scope_low_pass,
                    self.state.scope_bloom,
                );
                let correlation_samples: Vec<f32> =
                    if let Ok(ring) = self.state.spectrum_ring.try_lock() {
                        const N: usize = 4096;
                        let len = ring.len();
                        ring.iter().copied().skip(len.saturating_sub(N)).collect()
                    } else {
                        Vec::new()
                    };
                let lufs_history: Vec<f32> =
                    self.state.lufs_short_term_history.iter().copied().collect();
                let spectrum_history: Vec<Vec<f32>> =
                    self.state.spectrum_history.iter().cloned().collect();
                crate::widgets::scope_window::draw_pane_grid(
                    ui,
                    &mut self.state.scope_dock,
                    &mut self.state.scope_options,
                    &mut self.scope_dock_runtime,
                    &self.frame_buffer,
                    self.prefs.color.vectorscope_skin_angle,
                    self.state.playback == PlaybackState::Playing,
                    bottom_scope_cursor,
                    &self.state.spectrum_bins,
                    &correlation_samples,
                    &lufs_history,
                    self.state.lufs_target,
                    &spectrum_history,
                    &self.theme,
                    klipp_color::scopes::ScopeBasis::for_working_space(
                        self.state.project_working_colorspace,
                    ),
                );
                crate::widgets::scope_window::take_shared_toggles(
                    &self.state.scope_dock,
                    &mut self.state.scope_low_pass,
                    &mut self.state.scope_bloom,
                );
                return;
            }

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
        // U-6: same guard as render_color_ws_bottom_tools. Scopes only
        // need to animate while playback or scrubbing is live; static
        // views don't need 60 Hz repaints.
        if self.state.playback == PlaybackState::Playing || self.state.is_scrubbing {
            ui.ctx().request_repaint();
        }
    }

    fn render_color_ws_scope_settings_dropdown(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        use crate::state::ScopeScale;
        let dropdown_id = egui::Id::new("scope_settings_dropdown");
        // Cap the visible height so the popover doesn't push past the
        // window bottom on small viewers; ScrollArea handles overflow.
        let max_content_h = (ui.ctx().screen_rect().height() - toggle_bar_rect.max.y - 32.0)
            .clamp(240.0, 720.0);
        let area_response = egui::Area::new(dropdown_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(toggle_bar_rect.max.x - 240.0, toggle_bar_rect.max.y + 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(244.0);
                    ui.set_max_width(280.0);
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);
                    // egui's default scrollbar is "floating" (overlays
                    // content). Switching to a fixed bar makes it
                    // reserve real horizontal space, so the bar_*_margin
                    // values produce visible gaps instead of an
                    // overlay-on-content look.
                    ui.spacing_mut().scroll.floating = false;
                    ui.spacing_mut().scroll.bar_width = 8.0;
                    ui.spacing_mut().scroll.bar_inner_margin = 12.0;
                    ui.spacing_mut().scroll.bar_outer_margin = 4.0;

                    egui::ScrollArea::vertical()
                        .max_height(max_content_h)
                        .show(ui, |ui| {
                            // Belt-and-braces: cap the inner content
                            // width so even widgets that ignore
                            // scroll margins (egui sliders sometimes
                            // grab `available_width` early) stay clear
                            // of the bar.
                            let target = (ui.available_width() - 12.0).max(180.0);
                            ui.set_max_width(target);

                    ui.label(egui::RichText::new("DISPLAY").size(10.0).color(theme.text_dim).strong());
                    ui.checkbox(&mut self.state.scope_low_pass, "Low Pass Filter");
                    ui.checkbox(&mut self.state.scope_bloom, "Bloom on dense regions");
                    ui.checkbox(&mut self.state.scope_overlay_mode, "Overlay on viewer");

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("QUALITY").size(10.0).color(theme.text_dim).strong());
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::Low, "Draft (every 4px)");
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::Medium, "Standard (every 2px)");
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::High, "Full (every px)");

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("SCALE").size(10.0).color(theme.text_dim).strong());
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)");
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::Ire, "0 \u{2013} 100 IRE");
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::Percent, "0 \u{2013} 100%");
                    ui.checkbox(&mut self.state.scope_legal_range, "Show legal range (64\u{2013}940)")
                        .on_hover_text("Mark codes 64 and 940, the legal range of a 10-bit signal, on the waveform, parade and histogram.");
                    if self.state.hdr_monitor.renders_hdr() {
                        ui.label(egui::RichText::new("HDR monitoring: the graticule is in nits.").size(10.0).color(theme.text_dim));
                    }

                    // The scope options: the same block the floating scope
                    // window's settings show, both editing `state.scope_options`.
                    ui.add_space(4.0);
                    crate::widgets::scope_window::draw_scope_options(
                        ui,
                        &mut self.state.scope_options,
                        theme,
                    );

                    }); // end ScrollArea
                });
            });

        // Close on click-outside, using the ACTUAL rendered Area rect
        // (the old hardcoded 210×250 box was too small once envelope /
        // intensity / opacity / threshold controls landed, and made the
        // ComboBox arrow look like it "closed the window" because its
        // popup spawned outside that rect and triggered the close).
        //
        // Also skip the close while any egui popup (combobox dropdown,
        // tooltip with state) is open — those popups render in separate
        // Areas and their click events would otherwise dismiss us.
        let area_rect = area_response.response.rect;
        let any_popup_open = ctx.memory(|m| m.any_popup_open());
        if !any_popup_open && ui.input(|i| i.pointer.primary_clicked()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if !area_rect.contains(pos) && !toggle_bar_rect.contains(pos) {
                    self.state.scope_settings_open = false;
                }
            }
        }
    }

    fn render_color_ws_node_strip(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        nodes_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        // Sync per-node thumbs from the engine's FrameBuffer into
        // AppState at the start of the panel render. Engine bumps
        // `node_thumbs_gen` whenever it refreshes the map on a paused
        // / post-seek re-grade; we only clone the HashMap when it
        // actually advanced, so steady-state costs one lock + one
        // generation compare per frame.
        {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if fb.node_thumbs_gen != self.state.node_thumbs_gen {
                self.state.node_thumbs = fb.node_thumbs.clone();
                self.state.node_thumbs_gen = fb.node_thumbs_gen;
            }
        }
        crate::ui_util::allocate_clipped_ui(ui, nodes_rect, |ui| {
            ui.set_min_size(nodes_rect.size());
            ui.painter().rect_filled(nodes_rect, 0.0, theme.bg_deep);
            let clip_id = self.state.selected_clip;
            if let Some(cid) = clip_id {
                let seq_idx = self.state.active_sequence_idx;
                // The timeline grade's stand-in keeps no versions: only
                // its graph is written back.
                let keeps_versions = GradeStandIn::of(&self.state.sequences[seq_idx], cid).is_none();
                if let Some(clip) = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(cid))
                {
                    // Self-heal: if any id in the descent path doesn't
                    // resolve against the current clip's graph (clip
                    // switch, upstream RemoveNode, etc.) clear the
                    // path so the V2 panel renders the root graph on
                    // this frame. Selection clears alongside so the
                    // inspector doesn't linger on a ghost.
                    let path_valid = {
                        let mut cursor = &clip.node_graph;
                        self.state.graph_descent_path.iter().all(|id| {
                            match cursor.nodes.iter().find(|n| n.id == *id) {
                                Some(n) if n.kind == klipp_nodes::NodeKind::Compound => {
                                    match n.params.compound_subgraph.as_ref() {
                                        Some(sub) => { cursor = sub; true }
                                        None => false,
                                    }
                                }
                                _ => false,
                            }
                        })
                    };
                    if !path_valid {
                        self.state.graph_descent_path.clear();
                        self.state.selected_node_ids.clear();
                        self.state.selected_node_id = None;
                    }
                    // Build breadcrumb labels by walking the descent
                    // path through each compound's subgraph. Done
                    // BEFORE the mutable resolve so `clip.node_graph`
                    // can be read cleanly; the vec then survives the
                    // `Self::resolve_graph_at_path_mut` call.
                    let breadcrumb_labels: Vec<String> = {
                        let mut labels = Vec::with_capacity(
                            self.state.graph_descent_path.len()
                        );
                        let mut cursor = &clip.node_graph;
                        for id in &self.state.graph_descent_path {
                            if let Some(n) = cursor.nodes.iter().find(|n| n.id == *id) {
                                labels.push(n.label.clone());
                                if let Some(sub) = n.params.compound_subgraph.as_ref() {
                                    cursor = sub;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        labels
                    };
                    let panel_cmds = if self.state.use_node_graph_v2 {
                        let slot = self.state.deferred_texture_drops_slot;
                        // Snapshot grade version names + active
                        // index BEFORE grabbing the mutable graph
                        // borrow — the panel only needs the labels
                        // for the dropdown, not the full snapshots
                        // (which would be expensive to clone per
                        // render).
                        let grade_version_names: Vec<String> = clip
                            .grade_versions
                            .iter()
                            .map(|v| v.name.clone())
                            .collect();
                        let active_grade_version = clip.active_grade_version;
                        let clip_time_secs = (self.state.playhead.to_secs_f64()
                            - clip.timeline_in.to_secs_f64())
                        .max(0.0);
                        // Resolve the graph at the current descent
                        // path. If the path went stale (shouldn't
                        // happen in normal use — we self-heal), the
                        // call falls back to the root graph so the
                        // user isn't stuck on a blank canvas.
                        let path = self.state.graph_descent_path.clone();
                        let resolved = Self::resolve_graph_at_path_mut(clip, &path);
                        let graph = match resolved {
                            Ok(g) => g,
                            Err(_) => &mut clip.node_graph,
                        };
                        let panel_rect = ui.max_rect();
                        let cmds = self.node_graph_v2_panel.show(
                            ui, &mut self.state.selected_node_id,
                            &mut self.state.selected_node_ids,
                            graph, cid, &self.theme,
                            &self.state.node_thumbs,
                            self.state.node_thumbs_gen,
                            &mut self.state.deferred_texture_drops[slot],
                            &mut self.state.graph_descent_path,
                            &breadcrumb_labels,
                            keeps_versions.then_some(&grade_version_names[..]),
                            active_grade_version,
                            clip_time_secs,
                            self.prefs.color.port_snap_enabled,
                            self.prefs.color.port_snap_radius_px,
                            self.state.solo_node,
                        );
                        // Tag the node-graph panel as the active panel
                        // when the user clicks anywhere inside its rect
                        // — including on a node, the canvas background,
                        // or any inner widget. The keybinding dispatcher
                        // reads `state.active_panel` to decide whether
                        // Ctrl+C means "copy colour nodes" (here) or
                        // "copy clip" (elsewhere).
                        let ctx = ui.ctx().clone();
                        crate::state::note_active_panel(
                            &mut self.state,
                            crate::state::PanelKind::NodeGraph,
                            &ctx,
                            panel_rect,
                        );
                        cmds
                    } else {
                        self.node_graph_panel.show(
                            ui, &mut self.state.selected_node_id,
                            &mut clip.node_graph, cid, &self.theme,
                        )
                    };
                    all_cmds.extend(panel_cmds);
                } else {
                    ui.painter().text(
                        nodes_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Clip not found in sequence",
                        egui::FontId::proportional(12.0),
                        self.theme.text_dim,
                    );
                }
            } else {
                ui.painter().text(
                    nodes_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Select a clip to view its node graph",
                    egui::FontId::proportional(12.0),
                    self.theme.text_dim,
                );
            }
        });

        // Filmstrip-sourced per-node thumbnails (DaVinci-style node
        // preview). Decouples node-panel thumbs from the 4K decoder
        // pipeline: we feed the executor a pre-grade filmstrip tile at
        // ~228×128 instead of the main viewer's full-res frame. That
        // pass runs in well under a millisecond regardless of source
        // resolution, and filmstrip tiles are already in memory so
        // there's zero decode latency. Grade params are still
        // evaluated at the exact playhead so keyframed values stay
        // frame-accurate. Deduped via `last_filmstrip_thumb_send` so
        // the command only fires when something the thumbs depend on
        // (clip / tile / playhead / grade) actually changed. Pushed
        // after `panel_cmds` so any `SetVisibleNodeThumbs` from the V2
        // canvas processes first — the engine needs the visible-set
        // populated before our `RenderNodeThumbsFromSource` arrives.
        // While descended into a compound the filmstrip thumb
        // pipeline would render top-level clip graph outputs for
        // every node — misleading while the user is editing inside a
        // sub-graph, whose own nodes that pipeline doesn't capture.
        // Skip the dispatch for now; the V2 canvas falls back to
        // whatever thumb bytes are still in state.node_thumbs from
        // the last root-level capture, which is fine (stale but
        // consistent with the outer frame).
        if self.state.use_node_graph_v2 && self.state.graph_descent_path.is_empty() {
            if let Some(cid) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                let clip_info = self.state.sequences[seq_idx].tracks.iter()
                    .find_map(|t| t.clips.iter().find(|c| c.id == cid).map(|c| (
                        c.media_id(),
                        c.source_in.to_secs_f64(),
                        c.timeline_in,
                        c.speed,
                        klipp_engine::render_cache::compute_grade_hash(
                            &c.node_graph,
                            &c.builtin_effects,
                            &c.builtin_effects_enabled,
                        ),
                    )));
                if let Some((Some(media_id), src_in_secs, tl_in, speed, grade_hash)) = clip_info {
                    let playhead = self.state.playhead;
                    let src_time = src_in_secs
                        + (playhead - tl_in).to_secs_f64() * speed.max(0.001);
                    let tile_idx = self.state.thumbnail_cache.filmstrip(&media_id)
                        .and_then(|frames| {
                            if frames.is_empty() { return None; }
                            let i = match frames.binary_search_by(|f| {
                                f.time_secs.partial_cmp(&src_time)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            }) {
                                Ok(i) => i,
                                Err(0) => 0,
                                Err(i) if i >= frames.len() => frames.len() - 1,
                                Err(i) => {
                                    let before = (src_time - frames[i - 1].time_secs).abs();
                                    let after = (frames[i].time_secs - src_time).abs();
                                    if before <= after { i - 1 } else { i }
                                }
                            };
                            Some(i)
                        });
                    if let Some(idx) = tile_idx {
                        let key = (cid, idx, playhead.value, grade_hash);
                        if self.state.last_filmstrip_thumb_send != Some(key) {
                            let tile = self.state.thumbnail_cache.filmstrip(&media_id)
                                .and_then(|frames| frames.get(idx))
                                .map(|f| (f.rgba.clone(), f.width, f.height));
                            if let Some((rgba, w, h)) = tile {
                                all_cmds.push(AppCommand::RenderNodeThumbsFromSource {
                                    clip_id: cid,
                                    playhead,
                                    src_rgba: rgba,
                                    src_w: w,
                                    src_h: h,
                                });
                                self.state.last_filmstrip_thumb_send = Some(key);
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) struct ColorWsParams {
    pub(super) show_scopes_bar: bool,
    pub(super) show_nodes_bar: bool,
    pub(super) color_tools_bottom: bool,
    pub(super) toggle_bar_h: f32,
    pub(super) scopes_h: f32,
    pub(super) center_left: f32,
    pub(super) center_w: f32,
    pub(super) main_top: f32,
    pub(super) main_bottom: f32,
    pub(super) flexible_h: f32,
    pub(super) toggle_bar_rect: egui::Rect,
    pub(super) nodes_rect: egui::Rect,
    pub(super) nodes_resizer_rect: egui::Rect,
    pub(super) bottom_scope_cursor: Option<crate::state::ScopeCursorData>,
}
