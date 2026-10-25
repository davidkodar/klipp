use crate::widgets::menu::{menu_toggle, MenuRows, SubmenuRows};
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;
use klipp_timeline::TrackKind;

/// The shortest header row that has room for a video track's opacity bar
/// under its name.
const OPACITY_BAR_MIN_ROW: f32 = 40.0;

/// Render the 110px track header column: ruler spacer + per-track headers.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    header_rect: egui::Rect,
    kb: &crate::keybindings::KeyBindings,
) -> Vec<AppCommand> {
    let ruler_h = crate::theme::RULER_HEIGHT;

    // ── Ruler spacer (22px) ──
    let spacer_rect = egui::Rect::from_min_size(
        header_rect.min,
        egui::vec2(header_rect.width(), ruler_h),
    );
    ui.painter().rect_filled(spacer_rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        spacer_rect.x_range(),
        spacer_rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(spacer_rect.min.x + 6.0, spacer_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "TRACKS",
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    super::camera_lane::header_toggle(ui, state, theme, spacer_rect);

    // ── The camera lane's cell, under the spacer, when the lane is shown ──
    let lane_h = super::camera_lane::height(state);
    if lane_h > 0.0 {
        let lane_cell = egui::Rect::from_min_size(
            egui::pos2(header_rect.min.x, spacer_rect.max.y),
            egui::vec2(header_rect.width(), lane_h),
        );
        super::camera_lane::draw_header(ui, theme, lane_cell);
    }

    let mut cmds = Vec::new();

    // ── Track headers ──
    // Apply the same vertical scroll offset the clip area uses so rows stay aligned.
    let scroll_y = state.timeline_scroll_y;
    let mut y = header_rect.min.y + ruler_h + lane_h - scroll_y;
    let visible_top = header_rect.min.y + ruler_h + lane_h;
    let visible_bottom = header_rect.max.y;
    let mut toggle_mute: Option<usize> = None;
    let mut toggle_lock: Option<usize> = None;
    // Deferred magnetic operation from context menu (track_idx, action)
    #[derive(Clone)]
    enum TrackAction {
        PromoteToStoryline(usize),
        BreakApartStoryline(usize),
        SetAsPrimary(usize),
    }
    let mut deferred_action: Option<TrackAction> = None;
    // A header dragged to reorder its track (#922): the track being
    // dragged, kept between frames, and every row drawn, to drop on.
    // Each timeline's own, two being stacked (#82).
    let drag_key = ui.id().with("track_header_reorder");
    let dragging: Option<usize> = ui.data(|d| d.get_temp(drag_key));
    let mut rows: Vec<(usize, egui::Rect)> = Vec::new();

    let track_count = state.sequence().tracks.len();
    for track_idx in 0..track_count {
        let track = &state.sequences[state.active_sequence_idx].tracks[track_idx];
        // The rows as the clip area stacks them: squeezed by Fit's scale,
        // the automation lane under the clips counted in. A header at the
        // authored height drifted below its lane, and the last tracks fell
        // off the bottom where nothing could scroll to them.
        let scale = state.timeline_track_scale;
        let authored_h = crate::panels::timeline::effective_track_height(track);
        let track_h = crate::panels::timeline::scaled_track_height(track, scale);
        let row_h = crate::panels::timeline::scaled_track_row_height(track, scale);

        // Vertical visibility culling — skip rows above/below the visible header strip.
        if y + row_h < visible_top {
            y += row_h;
            continue;
        }
        if y > visible_bottom {
            break;
        }

        let track_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.min.x, y),
            egui::vec2(header_rect.width(), track_h),
        );

        // Background
        let bg = if ui.rect_contains_pointer(track_rect) {
            theme.bg_hover
        } else {
            theme.bg_deep
        };
        ui.painter().rect_filled(track_rect, 0.0, bg);

        // Bottom border
        ui.painter().hline(
            track_rect.x_range(),
            track_rect.max.y,
            egui::Stroke::new(1.0_f32, theme.border),
        );

        // Role color bar (3px left edge): the track's own colour when it
        // has one (#922), else guessed from its name.
        let own_colour = klipp_media::ColorLabel::from_u8(track.colour_label).rgb().map(|(r, g, b)| egui::Color32::from_rgb(r, g, b));
        let role_color = if let Some(c) = own_colour { c } else { match track.kind {
            TrackKind::Video => {
                // Determine role by track name
                if track.name.to_lowercase().contains("b-roll") || track.name.to_lowercase().contains("broll") {
                    theme.role_broll
                } else if track.name.to_lowercase().contains("title") {
                    theme.role_title
                } else {
                    theme.role_video
                }
            }
            TrackKind::Audio => {
                if track.name.to_lowercase().contains("dialogue") || track.name.to_lowercase().contains("vo") {
                    theme.role_dialogue
                } else if track.name.to_lowercase().contains("music") || track.name.to_lowercase().contains("score") {
                    theme.role_music
                } else if track.name.to_lowercase().contains("sfx") {
                    theme.role_sfx
                } else {
                    theme.role_dialogue
                }
            }
            TrackKind::Text => theme.role_title,
            TrackKind::Subtitle => theme.role_title,
        } };
        let bar_rect = egui::Rect::from_min_size(
            track_rect.min,
            egui::vec2(crate::theme::CLIP_ROLE_BAR_W, track_h),
        );
        ui.painter().rect_filled(
            bar_rect,
            egui::Rounding { nw: 0.0, ne: 2.0, sw: 0.0, se: 2.0 },
            role_color,
        );

        // Right-click context menu on track header area (allocated FIRST so buttons sit on top)
        rows.push((track_idx, track_rect));
        let header_resp = ui.allocate_rect(track_rect, egui::Sense::click_and_drag());
        if header_resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_key, track_idx));
        }
        // Ctrl-click on a track's header selects its clips (#844).
        if header_resp.clicked() && ui.input(|i| i.modifiers.command) {
            cmds.push(AppCommand::SelectClipsIn { track: Some(track_idx), range: klipp_core::SelectRange::All });
        }

        // Layer index badge (V1, V2, A1, etc.) — fixed, shows compositing order
        let badge_x = track_rect.min.x + crate::theme::CLIP_ROLE_BAR_W + 4.0;
        let layer_label = {
            let prefix = match track.kind {
                klipp_timeline::TrackKind::Video => "V",
                klipp_timeline::TrackKind::Audio => "A",
                klipp_timeline::TrackKind::Text => "T",
                klipp_timeline::TrackKind::Subtitle => "S",
            };
            let num = badge_number(&state.sequences[state.active_sequence_idx].tracks, track_idx);
            if num > 0 { format!("{}{}", prefix, num) } else { prefix.to_string() }
        };
        let badge_color = match track.kind {
            klipp_timeline::TrackKind::Video => theme.role_video,
            klipp_timeline::TrackKind::Audio => theme.role_dialogue,
            klipp_timeline::TrackKind::Text => theme.role_title,
            klipp_timeline::TrackKind::Subtitle => theme.role_title,
        };
        let badge_w = if layer_label.len() > 2 { 22.0 } else { 18.0 };
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, track_rect.center().y - 6.0),
            egui::vec2(badge_w, 12.0),
        );
        // Track targeting (#915): an untargeted track's badge is dimmed,
        // and clicking the badge turns targeting on or off.
        let (badge_fill, badge_text) = if track.targeted {
            (badge_color, egui::Color32::WHITE)
        } else {
            (badge_color.gamma_multiply(0.3), egui::Color32::from_white_alpha(110))
        };
        ui.painter().rect_filled(badge_rect, 2.0, badge_fill);
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            &layer_label,
            egui::FontId::monospace(10.0),
            badge_text,
        );
        // The source is patched here (#915): a bar under the badge.
        if track.source_patch {
            let bar = egui::Rect::from_min_size(badge_rect.left_bottom() + egui::vec2(0.0, 1.5), egui::vec2(badge_w, 2.0));
            ui.painter().rect_filled(bar, 1.0, theme.accent);
        }
        let badge_resp = ui
            .interact(badge_rect, ui.id().with(("track_target", track.id)), egui::Sense::click())
            .on_hover_text(format!(
                "{}: {}{}",
                layer_label,
                if track.targeted {
                    "targeted. Edit-point navigation, paste, split at playhead and match frame act on it. Click to untarget"
                } else {
                    "not targeted. Click to target"
                },
                if track.source_patch { ". Source edits land here" } else { "" },
            ));
        if badge_resp.clicked() {
            cmds.push(AppCommand::SetTrackTargeted { track_id: track.id, on: !track.targeted });
        }

        // Track name (after the badge)
        let name_x = badge_rect.right() + 4.0;
        let renaming = ui.data(|d| d.get_temp::<(klipp_core::TrackId, String)>(rename_id()))
            .filter(|(id, _)| *id == track.id);
        if let Some((rename_track, mut buf)) = renaming {
            let name_rect = egui::Rect::from_min_max(
                egui::pos2(name_x, track_rect.center().y - 8.0),
                egui::pos2(track_rect.max.x - 4.0, track_rect.center().y + 8.0),
            );
            let resp = ui.put(
                name_rect,
                egui::TextEdit::singleline(&mut buf)
                    .id(rename_edit_id())
                    .font(egui::FontId::proportional(10.0))
                    .margin(egui::Margin::symmetric(2.0, 1.0)),
            );
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                ui.data_mut(|d| d.remove::<(klipp_core::TrackId, String)>(rename_id()));
            } else if resp.lost_focus() {
                let new_name = buf.trim().to_string();
                if !new_name.is_empty() && new_name != track.name {
                    cmds.push(AppCommand::RenameTrack { track_id: rename_track, name: new_name });
                }
                ui.data_mut(|d| d.remove::<(klipp_core::TrackId, String)>(rename_id()));
            } else {
                ui.data_mut(|d| d.insert_temp(rename_id(), (rename_track, buf)));
            }
        } else {
            ui.painter().text(
                egui::pos2(name_x, track_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &track.name,
                egui::FontId::proportional(10.0),
                theme.text_secondary,
            );
            // ⇄ after the name when the track is sync-locked (#915).
            if track.sync_lock {
                let name_w = ui.painter().layout_no_wrap(track.name.clone(), egui::FontId::proportional(10.0), theme.text_secondary).rect.width();
                ui.painter().text(
                    egui::pos2(name_x + name_w + 4.0, track_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "\u{21C4}",
                    egui::FontId::proportional(10.0),
                    theme.accent,
                );
            }
        }

        // Magnetic role badge (small pill after track name)
        let role_badge = match track.magnetic_role {
            klipp_timeline::TrackRole::Primary => Some(("P", theme.marker_info)),
            klipp_timeline::TrackRole::Connected => Some(("C", theme.accent)),
            klipp_timeline::TrackRole::ConnectedStoryline => Some(("CS", theme.role_graphics)),
            klipp_timeline::TrackRole::Free => None,
        };
        if let Some((label, color)) = role_badge {
            let badge_w = if label.len() > 1 { 18.0 } else { 14.0 };
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(name_x + 2.0, track_rect.center().y + 6.0),
                egui::vec2(badge_w, 10.0),
            );
            ui.painter().rect_filled(badge_rect, 2.0, color);
            ui.painter().text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(10.0),
                egui::Color32::WHITE,
            );
        }

        // Track opacity slider (video tracks only) — horizontal bar at bottom
        // of header, where a squeezed row has room for it beside the name.
        if track.kind == TrackKind::Video && track_h >= OPACITY_BAR_MIN_ROW {
            let opacity_pct = (track.opacity * 100.0) as u32;
            let slider_h = 12.0;
            let slider_rect = egui::Rect::from_min_size(
                egui::pos2(track_rect.min.x + 4.0, track_rect.max.y - slider_h - 2.0),
                egui::vec2(track_rect.width() - 8.0, slider_h),
            );
            let slider_resp = ui.interact(
                slider_rect, ui.id().with(("track_opacity", track_idx)),
                egui::Sense::click_and_drag(),
            );
            // Background
            ui.painter().rect_filled(slider_rect, 2.0, theme.bg_panel_alt);
            // Fill bar
            let fill_w = slider_rect.width() * track.opacity;
            let fill_rect = egui::Rect::from_min_size(
                slider_rect.left_top(), egui::vec2(fill_w, slider_rect.height()),
            );
            ui.painter().rect_filled(fill_rect, 2.0, theme.role_video);
            // Label
            ui.painter().text(
                slider_rect.center(), egui::Align2::CENTER_CENTER,
                format!("{}%", opacity_pct), egui::FontId::proportional(10.0),
                theme.text_secondary,
            );
            // Interaction
            if slider_resp.double_clicked() {
                cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: 1.0 });
            } else if slider_resp.dragged() {
                let dx = slider_resp.drag_delta().x;
                let new_opacity = (track.opacity + dx / slider_rect.width()).clamp(0.0, 1.0);
                cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: new_opacity });
            }
        }

        // Per-track VU meter (audio tracks only)
        if track.kind == TrackKind::Audio {
            if let Some(&(peak_l, peak_r)) = state.track_peak_levels.get(track_idx) {
                let meter_w = 40.0;
                let bar_h = 3.0;
                let meter_x = name_x + 2.0;
                let meter_y = track_rect.center().y + 8.0; // below track name

                for (ch, peak) in [(0, peak_l), (1, peak_r)] {
                    let y = meter_y + ch as f32 * (bar_h + 1.0);
                    // Background
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(egui::pos2(meter_x, y), egui::vec2(meter_w, bar_h)),
                        1.0, theme.bg_deep,
                    );
                    // Fill
                    let fill_w = (peak.min(1.0) * meter_w).max(0.0);
                    if fill_w > 0.0 {
                        let color = if peak > 0.9 { theme.marker_error }
                            else if peak > 0.7 { theme.marker_warning }
                            else { theme.marker_success };
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(egui::pos2(meter_x, y), egui::vec2(fill_w, bar_h)),
                            1.0, color,
                        );
                    }
                }
                // Request repaint during playback for animation
                if peak_l > 0.001 || peak_r > 0.001 {
                    ui.ctx().request_repaint();
                }
            }
        }

        // Automation-lane expand/collapse button (audio tracks only).
        // Sits left of the record-arm. ▸ collapsed → ▾ expanded; click
        // dispatches `ToggleAudioAutomationLane`. Same data the inline
        // rubber band uses, just rendered at full lane height when open.
        let btn_size = 16.0;
        if track.kind == TrackKind::Audio {
            let lane_rect_btn = egui::Rect::from_min_size(
                egui::pos2(
                    track_rect.max.x - btn_size * 5.0 - 20.0,
                    track_rect.center().y - btn_size / 2.0,
                ),
                egui::vec2(btn_size, btn_size),
            );
            let lane_resp = ui.allocate_rect(lane_rect_btn, egui::Sense::click());
            let lane_open = track.automation_lane_visible.is_some();
            let lane_color = if lane_open { theme.accent } else { theme.text_dim };
            ui.painter().rect_filled(
                lane_rect_btn,
                3.0,
                if lane_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
            );
            let lane_glyph = if lane_open { "\u{25BE}" } else { "\u{25B8}" }; // ▾ vs ▸
            ui.painter().text(
                lane_rect_btn.center(),
                egui::Align2::CENTER_CENTER,
                lane_glyph,
                egui::FontId::proportional(12.0),
                lane_color,
            );
            let lane_tip = if lane_open {
                "Hide automation lane"
            } else {
                "Show automation lane"
            };
            styled_tooltip(&lane_resp, theme, lane_tip, None);
            if lane_resp.clicked() {
                cmds.push(AppCommand::ToggleAudioAutomationLane { track_idx });
            }
        }

        // Record-arm button (audio tracks only) — dim red when un-armed,
        // bright red when armed. Appears to the left of the mute button.
        if track.kind == TrackKind::Audio {
            let arm_rect = egui::Rect::from_min_size(
                egui::pos2(
                    track_rect.max.x - btn_size * 4.0 - 16.0,
                    track_rect.center().y - btn_size / 2.0,
                ),
                egui::vec2(btn_size, btn_size),
            );
            let arm_resp = ui.allocate_rect(arm_rect, egui::Sense::click());
            let arm_active = track.record_armed;
            let arm_color = if arm_active {
                theme.marker_error
            } else {
                theme.accent_dim
            };
            ui.painter().rect_filled(
                arm_rect,
                3.0,
                if arm_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
            );
            // U+25CF BLACK CIRCLE — covered by egui's default font fallback
            // chain (U+23FA, the proper record symbol, is not).
            ui.painter().text(
                arm_rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{25CF}",
                egui::FontId::proportional(11.0),
                arm_color,
            );
            let tip = if arm_active { "Un-arm for recording" } else { "Arm for recording" };
            styled_tooltip(&arm_resp, theme, tip, None);
            if arm_resp.clicked() {
                cmds.push(AppCommand::ArmTrack {
                    track_id: track.id,
                    armed: !arm_active,
                });
            }
        }

        // Eye (picture, text and subtitle tracks): the track's output on
        // or off, in the record-arm's place (#912). It was only drawn by
        // the header the app no longer shows, so a track could not be
        // hidden; a subtitle track's lets one language show at a time
        // (#2246).
        if matches!(track.kind, TrackKind::Video | TrackKind::Text | TrackKind::Subtitle) {
            let eye_rect = egui::Rect::from_min_size(
                egui::pos2(
                    track_rect.max.x - btn_size * 4.0 - 16.0,
                    track_rect.center().y - btn_size / 2.0,
                ),
                egui::vec2(btn_size, btn_size),
            );
            let eye_resp = ui.allocate_rect(eye_rect, egui::Sense::click());
            let visible = track.visible;
            ui.painter().rect_filled(
                eye_rect,
                3.0,
                if eye_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
            );
            let (eye_icon, eye_color) = if visible { (Icon::Eye, theme.text_dim) } else { (Icon::EyeSlash, theme.accent) };
            eye_icon.image(12.0).tint(eye_color).paint_at(ui, eye_rect.shrink(2.0));
            let what = if track.kind == TrackKind::Subtitle { "captions" } else { "picture" };
            styled_tooltip(&eye_resp, theme, &format!("{} this track's {what}", if visible { "Hide" } else { "Show" }), None);
            if eye_resp.clicked() {
                cmds.push(AppCommand::SetTrackVisibility { track_index: track_idx, visible: !visible });
            }
        }

        // Mute button (right side, 16x16)
        let mute_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size * 3.0 - 12.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click());
        let mute_active = track.muted;
        // A muted track shows it at a glance: the button filled red, as
        // the other header draws it (#2672).
        let (mute_bg, mute_color) = if mute_active {
            (theme.marker_error, theme.text_primary)
        } else if mute_resp.hovered() {
            (theme.bg_raised, theme.text_dim)
        } else {
            (egui::Color32::TRANSPARENT, theme.text_dim)
        };
        ui.painter().rect_filled(mute_rect, 3.0, mute_bg);
        let mute_icon = if mute_active { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
        mute_icon.image(12.0).tint(mute_color).paint_at(ui, mute_rect.shrink(2.0));
        styled_tooltip(&mute_resp, theme, if mute_active { "Unmute track" } else { "Mute track" }, None);
        if mute_resp.clicked() {
            toggle_mute = Some(track_idx);
        }

        // Solo button (16x16)
        let solo_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size * 2.0 - 8.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let solo_resp = ui.allocate_rect(solo_rect, egui::Sense::click());
        let solo_active = track.solo;
        let solo_bg = if solo_active {
            theme.accent // bright gold when active
        } else if solo_resp.hovered() {
            theme.bg_raised
        } else {
            egui::Color32::TRANSPARENT
        };
        let solo_text_color = if solo_active { theme.text_on_accent } else { theme.text_dim };
        ui.painter().rect_filled(solo_rect, 2.0, solo_bg);
        ui.painter().text(
            solo_rect.center(),
            egui::Align2::CENTER_CENTER,
            "S",
            egui::FontId::proportional(10.0),
            solo_text_color,
        );
        styled_tooltip(&solo_resp, theme, "Solo track", None);
        if solo_resp.clicked() {
            cmds.push(AppCommand::SetTrackSolo { track_index: track_idx, solo: !solo_active });
        }

        // Lock button (16x16)
        let lock_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size - 4.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let lock_resp = ui.allocate_rect(lock_rect, egui::Sense::click());
        let lock_active = track.locked;
        let lock_color = if lock_active { theme.accent } else { theme.text_dim };
        ui.painter().rect_filled(
            lock_rect,
            3.0,
            if lock_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
        );
        let lock_icon = if lock_active { Icon::Lock } else { Icon::LockOpen };
        lock_icon.image(12.0).tint(lock_color).paint_at(ui, lock_rect.shrink(2.0));
        styled_tooltip(&lock_resp, theme, "Lock track", None);
        if lock_resp.clicked() {
            toggle_lock = Some(track_idx);
        }

        let track_role = track.magnetic_role.clone();
        let track_id = track.id;
        let track_kind = track.kind;
        let track_name = track.name.clone();
        header_resp.context_menu_rows(|ui| {
            ui.set_min_width(180.0);

            crate::panels::timeline::track_select_items(ui, track_idx, &mut cmds);
            ui.separator();

            // Add track options
            ui.menu_rows("Add Track Above", |ui| {
                for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                    if ui.button(label).clicked() {
                        // One undo step (#907).
                        let before = crate::tracks_undo::before_labelled(state, "Add Track");
                        let seq = &mut state.sequences[state.active_sequence_idx];
                        let name = seq.next_track_name(kind);
                        let new_track = klipp_timeline::Track::new(&name, kind);
                        seq.tracks.insert(track_idx, new_track);
                        seq.renumber_tracks();
                        if let Some(before) = before {
                            crate::tracks_undo::record(state, before);
                        }
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_rows("Add Track Below", |ui| {
                for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                    if ui.button(label).clicked() {
                        let before = crate::tracks_undo::before_labelled(state, "Add Track");
                        let seq = &mut state.sequences[state.active_sequence_idx];
                        let name = seq.next_track_name(kind);
                        let new_track = klipp_timeline::Track::new(&name, kind);
                        let idx = (track_idx + 1).min(seq.tracks.len());
                        seq.tracks.insert(idx, new_track);
                        seq.renumber_tracks();
                        if let Some(before) = before {
                            crate::tracks_undo::record(state, before);
                        }
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                }
            });

            ui.separator();
            if ui.button("Rename Track").clicked() {
                ui.data_mut(|d| d.insert_temp(rename_id(), (track_id, track_name.clone())));
                ui.memory_mut(|m| m.request_focus(rename_edit_id()));
                ui.close_menu();
            }
            crate::panels::timeline::track_waveform_zoom_item(ui, state, track_idx, &mut cmds);
            // The track's colour, and moving it (#922).
            let own_label = state.sequence().tracks[track_idx].colour_label;
            ui.menu_rows("Track Colour", |ui| {
                for label in klipp_media::ColorLabel::ALL {
                    let swatch = label.rgb().map(|(r, g, b)| egui::Color32::from_rgb(r, g, b)).unwrap_or(ui.visuals().weak_text_color());
                    let text = egui::RichText::new(format!("\u{25CF} {}", label.label())).color(swatch);
                    if ui.selectable_label(own_label == label.to_u8(), text).clicked() {
                        cmds.push(AppCommand::SetTrackColourLabel { track_index: track_idx, label: label.to_u8() });
                        ui.close_menu();
                    }
                }
            });
            if track_kind == TrackKind::Audio {
                let apart = state.sequence().tracks[track_idx].channel_waveforms;
                if menu_toggle(ui, "Show Channels Separately", "", apart).on_hover_text("Each clip's left channel above its right").clicked() {
                    cmds.push(AppCommand::SetTrackChannelWaveforms { track_index: track_idx, on: !apart });
                    ui.close_menu();
                }
            }
            let same_kind = |i: &usize| state.sequence().tracks[*i].kind == track_kind;
            let up = (0..track_idx).rev().find(same_kind);
            let down = (track_idx + 1..state.sequence().tracks.len()).find(same_kind);
            if ui.add_enabled(up.is_some(), egui::Button::new("Move Track Up")).on_hover_text("Or drag the track's header").clicked() {
                if let Some(to) = up {
                    cmds.push(AppCommand::MoveTrack { from: track_idx, to });
                }
                ui.close_menu();
            }
            if ui.add_enabled(down.is_some(), egui::Button::new("Move Track Down")).clicked() {
                if let Some(to) = down {
                    cmds.push(AppCommand::MoveTrack { from: track_idx, to });
                }
                ui.close_menu();
            }

            // Magnetic role options, with their separator only when
            // there is one: a primary track has none.
            if track_role != klipp_timeline::TrackRole::Primary {
                ui.separator();
            }
            match track_role {
                klipp_timeline::TrackRole::Connected => {
                    // The storyline key acts on a selected clip on this
                    // track; it shares its binding with audio skim.
                    let key = kb.display_for("toggle_audio_skim");
                    if ui.add(egui::Button::new("Create Storyline").shortcut_text(&key)).clicked() {
                        deferred_action = Some(TrackAction::PromoteToStoryline(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::ConnectedStoryline => {
                    if ui.button("Break Apart Storyline").clicked() {
                        deferred_action = Some(TrackAction::BreakApartStoryline(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::Free => {
                    if ui.button("Set as Primary").clicked() {
                        deferred_action = Some(TrackAction::SetAsPrimary(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::Primary => {}
            }

            // A caption track in another language beside this one (#2213),
            // offered where there are captions to translate.
            if matches!(track_kind, TrackKind::Subtitle | TrackKind::Text) && !klipp_document::caption_translation::captions(state.sequence(), track_idx).is_empty() {
                use klipp_ml_candle::translation as tr;
                match tr::jobs::running() {
                    Some(job) => {
                        if ui.button(format!("Cancel Translation into {} ({:.0}%)", job.language, job.progress * 100.0)).clicked() {
                            let _ = tr::jobs::cancel(&job.id);
                            ui.close_menu();
                        }
                    }
                    None => {
                        ui.menu_rows("Translate Captions", |ui| {
                            for pair in tr::PAIRS {
                                let (from, to) = (tr::language_name(pair.source), tr::language_name(pair.target));
                                if ui.button(format!("{from} \u{2192} {to}")).on_hover_text("A new subtitle track under this one, each caption translated at its own time; the model downloads the first time").clicked() {
                                    cmds.push(AppCommand::TranslateCaptions { track_id, source: pair.source.into(), target: pair.target.into(), job_id: uuid::Uuid::new_v4().to_string(), glossary: Vec::new() });
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                }
            }

            // The captions' language (#2223), what an export tags its file
            // with.
            if track_kind == TrackKind::Subtitle {
                let current = state.sequence().tracks[track_idx].language.clone();
                let label = match &current {
                    Some(code) => format!("Language: {code}"),
                    None => "Language".to_string(),
                };
                ui.menu_rows(label, |ui| {
                    let mut pick = |ui: &mut egui::Ui, text: String, language: Option<String>| {
                        if menu_toggle(ui, &text, "", current == language).clicked() {
                            cmds.push(AppCommand::SetTrackLanguage { track_id, language });
                            ui.close_menu();
                        }
                    };
                    pick(ui, "None".into(), None);
                    // One set elsewhere (an imported file's) stays offered.
                    if let Some(code) = current.as_ref().filter(|c| !klipp_document::subtitle_export::LANGUAGES.iter().any(|(t, _)| t == c)) {
                        pick(ui, code.clone(), Some(code.clone()));
                    }
                    for (code, name) in klipp_document::subtitle_export::LANGUAGES {
                        pick(ui, format!("{name} ({code})"), Some((*code).to_string()));
                    }
                });
            }
            // How the track's own volume and pan keys play and record
            // (#2306), as the mixer strip's mode button sets it.
            if track_kind == TrackKind::Audio {
                let current = state.sequence().tracks[track_idx].automation.mode;
                ui.menu_rows(format!("Automation: {}", current.label()), |ui| {
                    for mode in klipp_core::automation::AutomationMode::ALL {
                        if menu_toggle(ui, mode.label(), "", mode == current).clicked() {
                            cmds.push(AppCommand::SetTrackAutomationMode { track_index: track_idx, mode });
                            ui.close_menu();
                        }
                    }
                });
            }

            ui.separator();
            crate::panels::timeline::track_attribute_items(ui, state, track_idx, &mut cmds);
            ui.separator();

            // Locking the track and its height, here rather than in each
            // clip's menu (#2674).
            let (locked, custom_height) = {
                let t = &state.sequence().tracks[track_idx];
                (t.locked, t.custom_height.is_some())
            };
            if menu_toggle(ui, "Lock Track", "", locked).clicked() {
                cmds.push(AppCommand::SetTrackLocked { track_id, locked: !locked });
                ui.close_menu();
            }
            if custom_height && ui.button("Reset Track Height").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: None });
                ui.close_menu();
            }
            // Sync lock (#915): a ripple on another track moves this one too.
            let sync_on = state.sequence().tracks[track_idx].sync_lock;
            if menu_toggle(ui, "Sync Lock", "", sync_on)
                .on_hover_text("On: a ripple on another track moves this track's clips by the same amount, keeping them in sync")
                .clicked()
            {
                cmds.push(AppCommand::SetTrackSyncLock { track_id, on: !sync_on });
                ui.close_menu();
            }
            // Track targeting and source patching (#915).
            let (targeted, patched, kind) = {
                let t = &state.sequence().tracks[track_idx];
                (t.targeted, t.source_patch, t.kind)
            };
            if menu_toggle(ui, "Target Track", "", targeted)
                .on_hover_text("On: edit-point navigation, paste, split at playhead and match frame act on this track")
                .clicked()
            {
                cmds.push(AppCommand::SetTrackTargeted { track_id, on: !targeted });
                ui.close_menu();
            }
            let what = match kind {
                klipp_timeline::TrackKind::Video => Some("Picture"),
                klipp_timeline::TrackKind::Audio => Some("Sound"),
                _ => None,
            };
            if let Some(what) = what {
                if menu_toggle(ui, &format!("Source {what} Here"), "", patched)
                    .on_hover_text(format!("On: insert, overwrite and append lay the source's {} on this track; off, on the first one", what.to_lowercase()))
                    .clicked()
                {
                    cmds.push(AppCommand::SetTrackSourcePatch { track_id, on: !patched });
                    ui.close_menu();
                }
            }
            ui.separator();

            // Remove track
            let has_clips = !state.sequence().tracks[track_idx].clips.is_empty();
            if ui.add_enabled(!has_clips, egui::Button::new("Remove Track")).clicked() {
                cmds.push(AppCommand::RemoveTrack { track_id });
                ui.close_menu();
            }
            if has_clips
                && ui.button("Remove Track (with clips)").clicked() {
                    cmds.push(AppCommand::RemoveTrack { track_id });
                    ui.close_menu();
                }
        });

        // ── Track height resize handle (bottom edge of header) ──
        let handle_rect = egui::Rect::from_min_size(
            egui::pos2(track_rect.left(), track_rect.max.y - 3.0),
            egui::vec2(track_rect.width(), 6.0),
        );
        let handle_resp = ui.interact(
            handle_rect,
            egui::Id::new(("track_resize_new", track_idx)),
            egui::Sense::click_and_drag(),
        );

        if handle_resp.hovered() || handle_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().line_segment(
                [
                    egui::pos2(track_rect.left(), track_rect.max.y - 1.0),
                    egui::pos2(track_rect.right(), track_rect.max.y - 1.0),
                ],
                egui::Stroke::new(1.0_f32, theme.border_light),
            );
        }

        // Drag start — snapshot original height
        if handle_resp.drag_started() {
            state.timeline_resizing_track = Some((
                track_idx,
                handle_resp.interact_pointer_pos().map_or(0.0, |p| p.y),
                authored_h,
            ));
        }

        // Dragging — update custom_height directly for smooth per-frame feedback.
        // The committed value is routed through SetTrackCustomHeight on drag_stopped.
        if handle_resp.dragged() {
            if let Some((ridx, start_y, orig_h)) = state.timeline_resizing_track {
                if ridx == track_idx {
                    if let Some(pos) = handle_resp.interact_pointer_pos() {
                        // On screen a squeezed row moves less than its height.
                        let delta = (pos.y - start_y) / scale.clamp(0.1, 1.0);
                        let new_h = (orig_h + delta).clamp(32.0, 300.0);
                        state.sequence_mut().tracks[track_idx].custom_height = Some(new_h);
                        state.dirty = true;
                    }
                }
            }
        }

        // Drag end — route the final committed value through the command so it
        // enters the undo/engine pipeline once per gesture.
        if handle_resp.drag_stopped() {
            state.timeline_resizing_track = None;
            let final_h = state.sequence().tracks[track_idx].custom_height;
            cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: final_h });
        }

        // Double-click — collapse/expand toggle with height memory
        if handle_resp.double_clicked() {
            let min_h = 32.0;
            let new_h = if authored_h <= min_h + 2.0 {
                // Expand — restore pre-collapse height or default by kind
                state
                    .timeline_track_pre_collapse
                    .remove(&track_idx)
                    .unwrap_or(match track_kind {
                        TrackKind::Audio => crate::panels::timeline::AUDIO_TRACK_HEIGHT,
                        TrackKind::Text | TrackKind::Subtitle => {
                            crate::panels::timeline::TEXT_TRACK_HEIGHT
                        }
                        _ => crate::panels::timeline::VIDEO_TRACK_HEIGHT,
                    })
            } else {
                // Collapse — save current height first
                state.timeline_track_pre_collapse.insert(track_idx, authored_h);
                min_h
            };
            cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(new_h) });
        }

        // Right-click context menu — preset heights
        handle_resp.context_menu_rows(|ui| {
            if ui.button("Small (32px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(32.0) });
                ui.close_menu();
            }
            if ui.button("Medium (64px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(64.0) });
                ui.close_menu();
            }
            if ui.button("Large (120px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(120.0) });
                ui.close_menu();
            }
            if ui.button("Expanded (200px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(200.0) });
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Reset to default").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: None });
                ui.close_menu();
            }
        });

        // The automation lane's part of the row, under the header.
        if row_h > track_h + 0.5 {
            let lane = egui::Rect::from_min_max(egui::pos2(header_rect.min.x, y + track_h), egui::pos2(header_rect.max.x, y + row_h));
            ui.painter().rect_filled(lane, 0.0, theme.bg_deep);
            ui.painter().hline(lane.x_range(), lane.max.y, egui::Stroke::new(1.0_f32, theme.border));
        }
        y += row_h;
    }

    // ── Empty area below tracks: right-click to add track ──
    let empty_rect = egui::Rect::from_min_max(
        egui::pos2(header_rect.min.x, y),
        header_rect.max,
    );
    if empty_rect.height() > 10.0 {
        let empty_resp = ui.interact(empty_rect, ui.id().with("empty_header_ctx"), egui::Sense::click());
        empty_resp.context_menu_rows(|ui| {
            ui.set_min_width(160.0);
            ui.label(egui::RichText::new("Add Track").strong().size(11.0));
            ui.separator();
            for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                if ui.button(label).clicked() {
                    let before = crate::tracks_undo::before_labelled(state, "Add Track");
                    let seq = &mut state.sequences[state.active_sequence_idx];
                    let name = seq.next_track_name(kind);
                    match kind {
                        TrackKind::Video => { seq.add_video_track(name); }
                        TrackKind::Audio => { seq.add_audio_track(name); }
                        TrackKind::Text => { seq.add_text_track(name); }
                        TrackKind::Subtitle => {
                            let track = klipp_timeline::Track::new(&name, kind);
                            seq.tracks.push(track);
                            seq.renumber_tracks();
                        }
                    }
                    if let Some(before) = before {
                        crate::tracks_undo::record(state, before);
                    }
                    cmds.push(AppCommand::SequenceUpdated);
                    ui.close_menu();
                }
            }
        });
    }

    // Apply deferred toggles
    if let Some(idx) = toggle_mute {
        let muted = state.sequence().tracks[idx].muted;
        state.sequence_mut().tracks[idx].muted = !muted;
        cmds.push(AppCommand::SetTrackMute { track_index: idx, muted: !muted });
    }
    if let Some(idx) = toggle_lock {
        let locked = state.sequence().tracks[idx].locked;
        state.sequence_mut().tracks[idx].locked = !locked;
    }

    // Apply deferred magnetic track actions
    if let Some(action) = deferred_action {
        match action {
            TrackAction::PromoteToStoryline(ti) => {
                // Find nearest primary clip as anchor (same logic as Shift+S in app.rs)
                let primary_idx = state.sequence().primary_track_index();
                if let Some(pi) = primary_idx {
                    let first_clip_in = state.sequence().tracks[ti].clips.first()
                        .map(|c| c.timeline_in);
                    if let Some(clip_in) = first_clip_in {
                        let anchor_id = state.sequence().tracks[pi].clips.iter()
                            .min_by_key(|c| {
                                let mid = c.timeline_in + (c.timeline_out - c.timeline_in);
                                let diff = mid - clip_in;
                                diff.value.unsigned_abs()
                            })
                            .map(|c| c.id);
                        if let Some(anchor_id) = anchor_id {
                            let result = klipp_timeline::magnetic::MagneticEditor::promote_to_storyline(
                                state.sequence_mut(), ti, anchor_id);
                            state.active_history_mut().push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::Magnetic(result.command),
                            });
                            state.dirty = true;
                            cmds.push(AppCommand::SequenceUpdated);
                        }
                    }
                }
            }
            TrackAction::BreakApartStoryline(ti) => {
                let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                    state.sequence_mut(), ti);
                state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::Magnetic(result.command),
                });
                state.dirty = true;
                cmds.push(AppCommand::SequenceUpdated);
            }
            TrackAction::SetAsPrimary(ti) => {
                // Clear old primary, set new one
                for track in &mut state.sequence_mut().tracks {
                    if track.magnetic_role == klipp_timeline::TrackRole::Primary {
                        track.magnetic_role = klipp_timeline::TrackRole::Free;
                    }
                }
                state.sequence_mut().tracks[ti].magnetic_role = klipp_timeline::TrackRole::Primary;
                state.dirty = true;
                cmds.push(AppCommand::SequenceUpdated);
            }
        }
    }

    // The dragged header: a frame round the track it would land on, of
    // its own kind, and on release the move (#922).
    if let Some(from) = dragging {
        let kind_of = |i: usize| state.sequence().tracks.get(i).map(|t| t.kind);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let target = pointer.and_then(|p| rows.iter().find(|(_, r)| r.contains(p))).map(|(i, r)| (*i, *r)).filter(|(i, _)| *i != from && kind_of(*i) == kind_of(from));
        if let Some((_, rect)) = target {
            ui.painter().rect_stroke(rect.shrink(1.0), 2.0, egui::Stroke::new(2.0_f32, theme.accent));
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            if let Some((to, _)) = target {
                cmds.push(AppCommand::MoveTrack { from, to });
            }
            ui.data_mut(|d| d.remove::<usize>(drag_key));
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }

    cmds
}

/// The number on a track's badge: the one its name ends in ("Video 3"
/// → 3), else its place among the tracks of its kind, counted as
/// `Sequence::renumber_tracks` numbers them (video from the bottom, the
/// rest from the top), so a renamed track keeps the number it had.
fn badge_number(tracks: &[klipp_timeline::Track], idx: usize) -> u32 {
    let track = &tracks[idx];
    track.name.split_whitespace().last()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_else(|| {
            let counted = if track.kind == TrackKind::Video { &tracks[idx..] } else { &tracks[..=idx] };
            counted.iter().filter(|t| t.kind == track.kind).count() as u32
        })
}

/// Where the track being renamed, and the name typed so far, are kept
/// between frames.
fn rename_id() -> egui::Id {
    egui::Id::new("track_header_rename")
}

/// The rename field's own id, so the menu item can give it focus.
fn rename_edit_id() -> egui::Id {
    egui::Id::new("track_header_rename_edit")
}

#[cfg(test)]
mod badge_tests {
    use super::badge_number;
    use klipp_timeline::{Track, TrackKind};

    #[test]
    fn a_renamed_track_keeps_its_number() {
        // Top to bottom as drawn: video numbered from the bottom, audio
        // from the top.
        let mut tracks = vec![
            Track::new("Video 2", TrackKind::Video),
            Track::new("Video 1", TrackKind::Video),
            Track::new("Audio 1", TrackKind::Audio),
            Track::new("Audio 2", TrackKind::Audio),
        ];
        let before: Vec<u32> = (0..tracks.len()).map(|i| badge_number(&tracks, i)).collect();
        assert_eq!(before, [2, 1, 1, 2]);
        for t in &mut tracks {
            t.name = "Renamed".into();
        }
        let after: Vec<u32> = (0..tracks.len()).map(|i| badge_number(&tracks, i)).collect();
        assert_eq!(after, before);
    }
}
