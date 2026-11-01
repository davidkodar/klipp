use crate::widgets::menu::{menu_toggle, MenuRows, SubmenuRows};
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;
use klipp_timeline::magnetic::EditMode;

/// The timeline's own toolbar: everything that changes how an edit on
/// the timeline behaves lives here and nowhere else. Left to right:
/// the edit tools, the insert / overwrite mode, snap and skimming, and the
/// marker button. When clips are selected, a stable contextual action group
/// is inserted before the zoom controls; the persistent controls never move.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>, kb: &crate::keybindings::KeyBindings) {
    let btn = crate::theme::control::REGULAR;
    // What the row has before the zoom group and a "»" button, which keep
    // their place at the right (#2944).
    let budget = ui.available_width() - ZOOM_GROUP_W - MORE_W - 8.0;
    let widths_id = ui.id().with("timeline_toolbar_widths");
    let mut widths: [f32; GROUPS.len()] = ui.ctx().data(|d| d.get_temp(widths_id)).unwrap_or([0.0; GROUPS.len()]);
    let mut folded: Vec<Group> = Vec::new();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(8.0);
        // The groups in order while each fits, by its width the last time
        // it was drawn; from the first that does not, the rest fold into
        // the "»" menu, so nothing is drawn half under the zoom group.
        let left = ui.cursor().min.x;
        for (k, group) in GROUPS.iter().enumerate() {
            if !folded.is_empty() || ui.cursor().min.x - left + widths[k] > budget {
                folded.push(*group);
                continue;
            }
            let x0 = ui.cursor().min.x;
            draw_group(*group, ui, state, theme, cmds, kb, btn);
            if SEPARATOR_AFTER.contains(group) {
                separator(ui, theme);
            }
            widths[k] = ui.cursor().min.x - x0;
        }
        if !folded.is_empty() {
            let resp = ui.add(egui::Button::new("\u{00bb}").min_size(egui::vec2(MORE_W - 4.0, btn)));
            styled_tooltip(&resp, theme, "More timeline controls", None);
            let popup = ui.id().with("timeline_toolbar_more");
            if resp.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup));
            }
            egui::popup::popup_below_widget(ui, popup, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(200.0);
                for group in &folded {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        draw_group(*group, ui, state, theme, cmds, kb, btn);
                    });
                }
            });
        }

        // ── Zoom (right-aligned): [−] [slider] [+] [Fit] ──
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let fit_resp = ui.add(egui::Button::new("Fit").min_size(egui::vec2(36.0, crate::theme::control::COMPACT)));
            styled_tooltip(&fit_resp, theme, "Fit timeline to window", Some(&kb.display_for("fit_to_window")));
            if fit_resp.clicked() {
                cmds.push(AppCommand::ZoomTimelineToFit);
            }
            ui.add_space(4.0);

            ui.label(egui::RichText::new("+").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
            let mut zoom_t = ((state.timeline_zoom.ln() - 0.1f32.ln())
                / (2000.0f32.ln() - 0.1f32.ln())).clamp(0.0, 1.0);
            let slider = egui::Slider::new(&mut zoom_t, 0.0..=1.0).show_value(false);
            let resp = ui.add_sized([90.0, 16.0], slider);
            if resp.changed() {
                let new_zoom = (0.1f32.ln() + zoom_t * (2000.0f32.ln() - 0.1f32.ln())).exp();
                cmds.push(AppCommand::SetTimelineZoom { zoom: new_zoom, scroll_x: None });
            }
            ui.label(egui::RichText::new("\u{2212}").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        });
    });
    ui.ctx().data_mut(|d| d.insert_temp(widths_id, widths));
}

/// The zoom group's width at the row's right edge, kept clear of the
/// groups: Fit, the slider and its ends.
const ZOOM_GROUP_W: f32 = 200.0;
/// The "»" button's room.
const MORE_W: f32 = 30.0;

/// The toolbar's groups, left to right; those that do not fit fold into
/// the "»" menu from the right (#2944).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Tools,
    EditMode,
    Ripple,
    Snap,
    Skim,
    Options,
    Linked,
    Marker,
    Title,
    Context,
}

const GROUPS: [Group; 10] = [Group::Tools, Group::EditMode, Group::Ripple, Group::Snap, Group::Skim, Group::Options, Group::Linked, Group::Marker, Group::Title, Group::Context];
const SEPARATOR_AFTER: [Group; 3] = [Group::Tools, Group::Ripple, Group::Linked];

/// The Ripple switch's tooltip. Ripple governs trims only: Delete
/// always closes a gap and Lift always leaves one, whatever the switch.
fn ripple_tip(on: bool) -> &'static str {
    if on {
        "Ripple on: trimming closes the gap."
    } else {
        "Ripple off: trimming leaves a gap. Use Lift (Shift+Delete) to remove a clip and leave its gap."
    }
}

/// One group's controls, in the row or in the "»" menu.
fn draw_group(group: Group, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>, kb: &crate::keybindings::KeyBindings, btn: f32) {
    match group {
        Group::Tools => {
            // ── Edit tools ──
            #[allow(clippy::type_complexity)]
            let tool_defs: &[(klipp_core::Tool, &str, &str, fn(&egui::Painter, egui::Rect, egui::Color32))] = &[
                (klipp_core::Tool::Select, "Select Tool", "select_tool", draw_tl_select_icon),
                (klipp_core::Tool::Trim, "Trim Tool", "trim_tool", draw_tl_trim_icon),
                (klipp_core::Tool::Stretch, "Rate Stretch Tool: drag a clip's edge to change its speed so it fills the new length", "stretch_tool", draw_tl_stretch_icon),
                (klipp_core::Tool::Blade, "Blade Tool", "blade_tool", draw_tl_blade_icon),
                (klipp_core::Tool::Range, "Range Tool", "range_tool", draw_tl_range_icon),
                // Works in the viewer: a click or a drag there makes a title (#1713).
                (klipp_core::Tool::Text, "Text Tool: click or drag in the viewer to add a title", "text_tool", draw_tl_text_icon),
                // Works in the viewer: draws and edits a freeform mask (#74).
                (klipp_core::Tool::Mask, "Mask Tool: click in the viewer to draw a mask's points, drag to pull out curves", "mask_tool", draw_tl_mask_icon),
            ];
            for (tool, tip, action, draw_fn) in tool_defs {
                let active = state.active_tool == *tool;
                let resp = icon_button(ui, theme, btn, active, *draw_fn);
                let key = kb.display_for(action);
                styled_tooltip(&resp, theme, tip, Some(&key));
                if *tool == klipp_core::Tool::Blade {
                    // The tour's trim-or-split step points here (#2933).
                    crate::welcome::anchor(ui.ctx(), "blade_tool", resp.rect);
                }
                if resp.clicked() {
                    cmds.push(AppCommand::SetActiveTool(*tool));
                }
            }
        }
        Group::EditMode => {
            // ── Insert / Overwrite: a two-segment switch, the active mode lit ──
            {
                let active = if state.edit_mode == EditMode::Insert { 0 } else { 1 };
                let before = ui.cursor().min;
                if let Some(i) = crate::widgets::segmented::segmented(
                    ui, &["INS", "OVR"], active, theme,
                    egui::FontId::monospace(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT,
                ) {
                    state.edit_mode = if i == 0 { EditMode::Insert } else { EditMode::Overwrite };
                }
                let tip = match state.edit_mode {
                    EditMode::Insert => "Edit Mode: Insert",
                    EditMode::Overwrite => "Edit Mode: Overwrite",
                };
                let rect = egui::Rect::from_min_max(before, ui.cursor().min + egui::vec2(0.0, crate::theme::control::COMPACT));
                let hover = ui.interact(rect, ui.id().with("edit_mode_tip"), egui::Sense::hover());
                styled_tooltip(&hover, theme, tip, Some(&kb.display_for("toggle_edit_mode")));
            }
        }
        Group::Ripple => {
            // ── Ripple: lit when edits close the gaps they leave (#912) ──
            // The switch was only drawn by the timeline the app no longer
            // shows, so ripple could not be turned off.
            {
                let text = egui::RichText::new("RIPPLE")
                    .font(egui::FontId::monospace(crate::theme::type_scale::CAPTION))
                    .color(if state.ripple_enabled { theme.accent } else { theme.text_dim });
                let resp = ui.add(egui::Button::new(text).selected(state.ripple_enabled).min_size(egui::vec2(0.0, crate::theme::control::COMPACT)));
                styled_tooltip(&resp, theme, ripple_tip(state.ripple_enabled), None);
                if resp.clicked() {
                    cmds.push(AppCommand::SetRippleEnabled(!state.ripple_enabled));
                }
            }
        }
        Group::Snap => {
            // ── Snap (left-click = master; right-click = per-target menu) ──
            // Dispatches through AppCommand so the central handler can
            // persist state.snap → layout.snap on every flip (and so the
            // four snap-toggle surfaces — toolbar, N shortcut, palette,
            // View menu — stay in sync without parallel mutation paths).
            let snap_active = state.snap.master;
            let resp = icon_button(ui, theme, btn, snap_active, draw_tl_snap_icon);
            // A small chevron in the corner hints at the per-target menu.
            let chevron = if snap_active { theme.accent } else { theme.text_dim };
            ui.painter().text(
                egui::pos2(resp.rect.right() - 4.0, resp.rect.bottom() - 6.0),
                egui::Align2::RIGHT_BOTTOM,
                "\u{25BE}",
                egui::FontId::proportional(10.0),
                chevron,
            );
            styled_tooltip(&resp, theme, "Snap (right-click for per-target options)", Some(&kb.display_for("toggle_snap")));
            if resp.clicked() {
                cmds.push(AppCommand::SetSnapMaster(!state.snap.master));
            }
            resp.context_menu_rows(|ui| {
                use klipp_core::SnapTargetKind;
                let cfg = state.snap;
                ui.set_min_width(180.0);
                if menu_toggle(ui, "Snap (master)", "", cfg.master).clicked() {
                    cmds.push(AppCommand::SetSnapMaster(!cfg.master));
                    ui.close_menu();
                }
                ui.separator();
                ui.add_enabled_ui(cfg.master, |ui| {
                    let rows: &[(SnapTargetKind, &str, bool)] = &[
                        (SnapTargetKind::ClipEdge, "Snap to clips", cfg.clips),
                        (SnapTargetKind::Playhead, "Snap to playhead", cfg.playhead),
                        (SnapTargetKind::Marker, "Snap to markers", cfg.markers),
                        (SnapTargetKind::InPoint, "Snap to in/out", cfg.in_out),
                    ];
                    for &(target, label, enabled) in rows {
                        if menu_toggle(ui, label, "", enabled).clicked() {
                            cmds.push(AppCommand::SetSnapTarget { target, enabled: !enabled });
                            ui.close_menu();
                        }
                    }
                    // The grid, one choice (#3056).
                    ui.separator();
                    ui.label(egui::RichText::new("Grid").small());
                    for choice in crate::timeline::snap::Snapper::GRID_CHOICES {
                        if menu_toggle(ui, choice.label(), "", state.grid_snap_interval == choice).clicked() {
                            cmds.push(AppCommand::SetGridInterval(choice));
                            ui.close_menu();
                        }
                    }
                });
            });
        }
        Group::Skim => {
            // ── Skimming ──
            let resp = icon_button(ui, theme, btn, state.skimming_enabled, draw_tl_skim_icon);
            styled_tooltip(&resp, theme, "Skimming", Some(&kb.display_for("toggle_skimming")));
            if resp.clicked() {
                state.skimming_enabled = !state.skimming_enabled;
                let msg = if state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
                state.toast.show(msg);
            }
        }
        Group::Options => {
            // ── Timeline options: follow, proxies, what an append brings,
            // find (#912). Each was only on the timeline the app no longer
            // shows. ──
            let resp = ui.add(egui::Button::new("\u{22EF}").min_size(egui::vec2(btn, btn)));
            styled_tooltip(&resp, theme, "Timeline options", None);
            egui::popup::popup_below_widget(ui, ui.id().with("timeline_options"), &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(230.0);
                ui.label(egui::RichText::new("Follow the playhead").strong().size(11.0));
                for follow in [crate::state::PlayheadFollow::Off, crate::state::PlayheadFollow::Page, crate::state::PlayheadFollow::Smooth] {
                    if ui.radio(state.playhead_follow == follow, follow.label()).clicked() {
                        state.playhead_follow = follow;
                        state.playhead_follow_chosen = true;
                    }
                }
                ui.separator();
                if ui.checkbox(&mut state.use_proxies.clone(), "Play from proxies").on_hover_text("Lower quality, faster decode").clicked() {
                    cmds.push(AppCommand::SetUseProxies(!state.use_proxies));
                }
                if ui.checkbox(&mut state.append_brings_audio.clone(), "Appended clips bring their audio").clicked() {
                    cmds.push(AppCommand::SetAppendBringsAudio(!state.append_brings_audio));
                }
                if ui.checkbox(&mut state.show_reused_footage.clone(), "Show reused footage").on_hover_text("A strip under spans that show the same frames as another clip").clicked() {
                    cmds.push(AppCommand::SetShowReusedFootage(!state.show_reused_footage));
                }
                ui.separator();
                if ui.add(egui::Button::new("Find in Timeline\u{2026}").shortcut_text(kb.display_for("focus_panel_search"))).clicked() {
                    state.focus_search_request = Some(crate::state::PanelKind::Timeline);
                    ui.memory_mut(|m| m.close_popup());
                }
            });
            if resp.clicked() {
                ui.memory_mut(|m| m.toggle_popup(ui.id().with("timeline_options")));
            }
        }
        Group::Linked => {
            // ── Linked: picture and sound trim and move together (#916) ──
            // Off, each goes alone: a J or L cut. Alt while trimming does the
            // same for one trim.
            {
                let text = egui::RichText::new("LINKED")
                    .font(egui::FontId::monospace(crate::theme::type_scale::CAPTION))
                    .color(if state.linked_selection { theme.accent } else { theme.text_dim });
                let resp = ui.add(egui::Button::new(text).selected(state.linked_selection).min_size(egui::vec2(0.0, crate::theme::control::COMPACT)));
                let tip = if state.linked_selection {
                    "Linked: picture and sound trim and move together. Click to work on each alone; Alt while trimming does it for one trim."
                } else {
                    "Unlinked: picture and sound trim and move alone. Click to link them again."
                };
                styled_tooltip(&resp, theme, tip, None);
                if resp.clicked() {
                    state.linked_selection = !state.linked_selection;
                }
            }
        }
        Group::Marker => {
            // ── Add marker ──
            let resp = icon_button(ui, theme, btn, false, draw_tl_marker_icon);
            styled_tooltip(&resp, theme, "Add Marker", Some(&kb.display_for("add_marker")));
            if resp.clicked() {
                cmds.push(AppCommand::AddMarker {
                    position: state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
            }
        }
        Group::Title => {
            // ── Add title ──
            let resp = icon_button(ui, theme, btn, false, draw_tl_title_icon);
            styled_tooltip(&resp, theme, "Add Title", Some(&kb.display_for("create_title")));
            if resp.clicked() {
                cmds.push(AppCommand::AddTitleAtPlayhead { over_clip: None });
            }
        }
        Group::Context => {
            // ── Contextual clip actions ──
            // Keep this group empty when there is no selection so the toolbar
            // stays quiet. Once a clip is selected, expose the high-frequency
            // timeline operations that otherwise require a shortcut or menu.
            show_context_actions(ui, state, theme, cmds);
        }
    }
}

fn show_context_actions(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
) {
    let ids = selected_clip_ids(state);
    if ids.is_empty() {
        return;
    }

    separator(ui, theme);
    let action_btn = |ui: &mut egui::Ui, label: &str, tip: &str| {
        let resp = ui.add(
            egui::Button::new(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION))
                .min_size(egui::vec2(42.0, crate::theme::control::COMPACT)),
        );
        styled_tooltip(&resp, theme, tip, None);
        resp
    };

    // These operations are valid for any selected clip type.
    if ids.len() == 1 {
        let id = ids[0];
        if let Some((track_index, clip)) = find_clip(state, id) {
            let playhead_inside = state.playhead > clip.timeline_in && state.playhead < clip.timeline_out;
            let split = ui
                .add_enabled(
                    playhead_inside,
                    egui::Button::new(egui::RichText::new("Split").size(crate::theme::type_scale::CAPTION))
                        .min_size(egui::vec2(42.0, crate::theme::control::COMPACT)),
                );
            styled_tooltip(&split, theme, "Split the selected clip at the playhead", None);
            if split.clicked() {
                cmds.push(AppCommand::BladeClip {
                    track_index,
                    clip_id: id,
                    cut_at: state.playhead,
                });
            }

            let trim_in = action_btn(ui, "Trim in", "Ripple-trim the selected clip's head to the playhead");
            if trim_in.clicked() {
                cmds.push(AppCommand::RippleTrimHead { clip_id: id });
            }
            let trim_out = action_btn(ui, "Trim out", "Ripple-trim the selected clip's tail to the playhead");
            if trim_out.clicked() {
                cmds.push(AppCommand::RippleTrimTail { clip_id: id });
            }

            let duplicate = action_btn(ui, "Duplicate", "Duplicate the selected clip");
            if duplicate.clicked() {
                cmds.push(AppCommand::DuplicateClip { clip_id: id });
            }

            let delete = action_btn(ui, "Delete", "Delete the selected clip");
            if delete.clicked() {
                cmds.push(AppCommand::RemoveClip { clip_id: id });
            }

            show_transition_menu(ui, state, theme, cmds, &[id]);

            if clip.linked_clip_id.is_some() {
                let unlink = action_btn(ui, "Unlink", "Unlink the selected audio/video pair");
                if unlink.clicked() {
                    cmds.push(AppCommand::UnlinkClip { clip_id: id });
                }
            }
        }
    } else {
        let delete = action_btn(ui, "Delete", "Delete all selected clips");
        if delete.clicked() {
            for id in &ids {
                cmds.push(AppCommand::RemoveClip { clip_id: *id });
            }
        }

        if ids.len() == 2 {
            let link = action_btn(ui, "Link", "Link the two selected clips as an audio/video pair");
            if link.clicked() {
                cmds.push(AppCommand::LinkClips {
                    clip_a: ids[0],
                    clip_b: ids[1],
                });
            }
        }

        show_transition_menu(ui, state, theme, cmds, &ids);
    }
}

fn show_transition_menu(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
    selected_ids: &[klipp_core::ClipId],
) {
    let targets = transition_targets(state, selected_ids);
    let enabled = !targets.is_empty();
    ui.add_enabled_ui(enabled, |ui| {
        let response = ui.menu_rows("Transition", |ui| {
            ui.set_min_width(190.0);
            let max_h = (ui.ctx().screen_rect().height() * 0.65).max(220.0);
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                for &(kind_id, label) in klipp_engine::transition::all_transition_kinds() {
                    if targets.len() == 1 {
                        if ui.button(label).clicked() {
                            let (track_index, clip_a_id, clip_b_id) = targets[0];
                            cmds.push(AppCommand::AddTransition {
                                track_index,
                                clip_a_id,
                                clip_b_id,
                                kind_id,
                                duration_frames: state.default_transition_frames,
                            });
                            ui.close_menu();
                        }
                    } else if ui.button(format!("{label} ({} cuts)", targets.len())).clicked() {
                        for &(track_index, clip_a_id, clip_b_id) in &targets {
                            cmds.push(AppCommand::AddTransition {
                                track_index,
                                clip_a_id,
                                clip_b_id,
                                kind_id,
                                duration_frames: state.default_transition_frames,
                            });
                        }
                        ui.close_menu();
                    }
                }
            });
        });
        styled_tooltip(
            &response.response,
            theme,
            "Add a transition at the selected cut",
            None,
        );
    });
}

fn transition_targets(
    state: &AppState,
    selected_ids: &[klipp_core::ClipId],
) -> Vec<(usize, klipp_core::ClipId, klipp_core::ClipId)> {
    let mut targets = Vec::new();
    for (track_index, track) in state.sequence().tracks.iter().enumerate() {
        let positions: Vec<usize> = track
            .clips
            .iter()
            .enumerate()
            .filter_map(|(index, clip)| selected_ids.contains(&clip.id).then_some(index))
            .collect();
        if positions.len() == 2 && positions[1] == positions[0] + 1 {
            targets.push((track_index, track.clips[positions[0]].id, track.clips[positions[1]].id));
        } else if positions.len() == 1 {
            let pos = positions[0];
            if pos > 0 {
                targets.push((track_index, track.clips[pos - 1].id, track.clips[pos].id));
            }
            if pos + 1 < track.clips.len() {
                targets.push((track_index, track.clips[pos].id, track.clips[pos + 1].id));
            }
        }
    }
    targets
}

fn selected_clip_ids(state: &AppState) -> Vec<klipp_core::ClipId> {
    let mut ids = state.selected_clips.clone();
    if ids.is_empty() {
        if let Some(id) = state.selected_clip {
            ids.push(id);
        }
    } else if let Some(id) = state.selected_clip {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn find_clip(
    state: &AppState,
    clip_id: klipp_core::ClipId,
) -> Option<(usize, &klipp_timeline::Clip)> {
    state
        .sequence()
        .tracks
        .iter()
        .enumerate()
        .find_map(|(track_index, track)| track.find_clip(clip_id).map(|clip| (track_index, clip)))
}

/// A square glyph button: a tinted field and the accent on the glyph
/// when active, the hover tone under the pointer, nothing otherwise.
fn icon_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    size: f32,
    active: bool,
    draw: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let (bg, fg) = if active {
        (theme.accent_subtle, theme.accent)
    } else if resp.hovered() {
        (theme.bg_hover, theme.text_primary)
    } else {
        (egui::Color32::TRANSPARENT, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, crate::theme::radius::CONTROL, bg);
    draw(ui.painter(), rect, fg);
    resp
}

fn separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme.border_light);
    ui.add_space(2.0);
}

// ── Painted icons for timeline toolbar (13x13 icon area inside 26x26 button) ──

fn icon_area(btn: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(btn.center(), egui::vec2(13.0, 13.0))
}

fn draw_tl_select_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    // Arrow cursor triangle
    let pts = vec![
        egui::pos2(ir.min.x + 1.0, ir.min.y),
        egui::pos2(ir.min.x + 1.0, ir.max.y - 1.0),
        egui::pos2(ir.min.x + 5.0, ir.max.y - 4.0),
        egui::pos2(ir.min.x + 10.0, ir.center().y + 2.0),
    ];
    painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
}

fn draw_tl_blade_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // Diagonal razor line
    painter.line_segment([egui::pos2(ir.min.x + 1.0, ir.max.y - 1.0), egui::pos2(ir.max.x - 1.0, ir.min.y + 1.0)], s);
    // Small triangle tip
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(ir.max.x - 1.0, ir.min.y + 1.0),
        egui::pos2(ir.max.x - 4.0, ir.min.y + 2.0),
        egui::pos2(ir.max.x - 2.0, ir.min.y + 4.0),
    ], c, egui::Stroke::NONE));
}

fn draw_tl_trim_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // Two vertical bars with inward ticks
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 2.0, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.center().y), egui::pos2(ir.min.x + 5.0, ir.center().y)], s);
    painter.line_segment([egui::pos2(ir.max.x - 5.0, ir.center().y), egui::pos2(ir.max.x - 2.0, ir.center().y)], s);
}

/// Two bars with arrows pointing out between them: the clip made longer.
fn draw_tl_stretch_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    let (l, r, y) = (ir.min.x + 2.0, ir.max.x - 2.0, ir.center().y);
    painter.line_segment([egui::pos2(l, ir.min.y + 1.0), egui::pos2(l, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(r, ir.min.y + 1.0), egui::pos2(r, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(l + 2.0, y), egui::pos2(r - 2.0, y)], s);
    for (tip, dir) in [(l + 2.0, 1.0), (r - 2.0, -1.0)] {
        painter.line_segment([egui::pos2(tip, y), egui::pos2(tip + 2.5 * dir, y - 2.5)], s);
        painter.line_segment([egui::pos2(tip, y), egui::pos2(tip + 2.5 * dir, y + 2.5)], s);
    }
}

fn draw_tl_range_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // Left bracket bar
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 1.0), egui::pos2(ir.min.x + 2.0, ir.max.y - 1.0)], s);
    // Right bracket bar
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 1.0), egui::pos2(ir.max.x - 2.0, ir.max.y - 1.0)], s);
    // Horizontal connecting line
    painter.line_segment([egui::pos2(ir.min.x + 3.0, ir.center().y), egui::pos2(ir.max.x - 3.0, ir.center().y)], s);
    // Subtle fill
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(ir.min.x + 2.0, ir.min.y + 3.0),
            egui::pos2(ir.max.x - 2.0, ir.max.y - 3.0),
        ),
        0.0,
        egui::Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 30),
    );
}

/// A T: the Text tool (#1713).
fn draw_tl_text_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // Crossbar with serifs dropping from its ends
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 2.0), egui::pos2(ir.max.x - 2.0, ir.min.y + 2.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 2.0, ir.min.y + 2.0), egui::pos2(ir.min.x + 2.0, ir.min.y + 4.0)], s);
    painter.line_segment([egui::pos2(ir.max.x - 2.0, ir.min.y + 2.0), egui::pos2(ir.max.x - 2.0, ir.min.y + 4.0)], s);
    // Stem and foot
    painter.line_segment([egui::pos2(ir.center().x, ir.min.y + 2.0), egui::pos2(ir.center().x, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.center().x - 2.0, ir.max.y - 1.0), egui::pos2(ir.center().x + 2.0, ir.max.y - 1.0)], s);
}

fn draw_tl_mask_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // A curve between two points, a handle out of the first
    let (a, b) = (egui::pos2(ir.min.x + 2.0, ir.max.y - 2.0), egui::pos2(ir.max.x - 2.0, ir.min.y + 3.0));
    let (ca, cb) = (egui::pos2(ir.min.x + 2.0, ir.min.y + 2.0), egui::pos2(ir.center().x, ir.max.y - 1.0));
    let curve = egui::epaint::CubicBezierShape::from_points_stroke([a, ca, cb, b], false, egui::Color32::TRANSPARENT, s);
    painter.add(curve);
    painter.line_segment([a, ca], egui::Stroke::new(1.0_f32, c.gamma_multiply(0.6)));
    painter.rect_filled(egui::Rect::from_center_size(a, egui::vec2(3.0, 3.0)), 0.0, c);
    painter.rect_filled(egui::Rect::from_center_size(b, egui::vec2(3.0, 3.0)), 0.0, c);
}

fn draw_tl_snap_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    // Crosshair / magnet shape
    let cx = ir.center().x;
    let cy = ir.center().y;
    painter.line_segment([egui::pos2(cx, ir.min.y + 1.0), egui::pos2(cx, ir.max.y - 1.0)], s);
    painter.line_segment([egui::pos2(ir.min.x + 1.0, cy), egui::pos2(ir.min.x + 4.0, cy)], s);
    painter.line_segment([egui::pos2(ir.max.x - 4.0, cy), egui::pos2(ir.max.x - 1.0, cy)], s);
    painter.circle_stroke(egui::pos2(cx, cy), 3.0, s);
}

fn draw_tl_skim_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let s = egui::Stroke::new(1.3_f32, c);
    let cx = ir.center().x;
    // Vertical line with upward arrow
    painter.line_segment([egui::pos2(cx, ir.min.y + 4.0), egui::pos2(cx, ir.max.y - 1.0)], s);
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx, ir.min.y + 1.0),
        egui::pos2(cx - 3.0, ir.min.y + 5.0),
        egui::pos2(cx + 3.0, ir.min.y + 5.0),
    ], c, egui::Stroke::NONE));
}

fn draw_tl_marker_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let ir = icon_area(btn);
    let cx = ir.center().x;
    // Downward triangle (flag) + vertical line
    painter.line_segment(
        [egui::pos2(cx, ir.min.y + 1.0), egui::pos2(cx, ir.max.y - 1.0)],
        egui::Stroke::new(1.3_f32, c),
    );
    painter.add(egui::Shape::convex_polygon(vec![
        egui::pos2(cx - 3.5, ir.min.y + 1.0),
        egui::pos2(cx + 3.5, ir.min.y + 1.0),
        egui::pos2(cx, ir.min.y + 5.0),
    ], c, egui::Stroke::NONE));
}

/// A title: a bold T.
fn draw_tl_title_icon(painter: &egui::Painter, btn: egui::Rect, c: egui::Color32) {
    let a = icon_area(btn);
    painter.text(
        a.center(),
        egui::Align2::CENTER_CENTER,
        "T",
        egui::FontId::proportional(a.height() * 1.05),
        c,
    );
}

#[cfg(test)]
mod tests {
    /// The Ripple tooltip speaks of trimming only, as the switch governs
    /// only trims; off, it points to Lift for leaving a clip's gap.
    #[test]
    fn the_ripple_tooltip_speaks_only_of_trimming() {
        assert_eq!(super::ripple_tip(true), "Ripple on: trimming closes the gap.");
        assert_eq!(super::ripple_tip(false), "Ripple off: trimming leaves a gap. Use Lift (Shift+Delete) to remove a clip and leave its gap.");
    }
}
