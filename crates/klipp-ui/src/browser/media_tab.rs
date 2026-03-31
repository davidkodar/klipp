use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MediaId, ViewerMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Duration,
    Rating,
    Codec,
    FileSize,
    DateAdded,
    ShootDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum BrowserViewMode {
    #[default]
    List,
    Grid,
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
    sort_by: &mut SortBy,
    sort_ascending: &mut bool,
    selected_bin: &mut Option<String>,
    view_mode: &BrowserViewMode,
    renaming_bin: &mut Option<(String, String)>,
    creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
    smart_filter: super::SmartBinFilter,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Skim flag is reset at the start of each frame in app.rs update().
    // Cleanup of skim state happens in app.rs after all panels render.

    // Collect and filter items
    let search_lower = search_query.to_lowercase();
    let mut items: Vec<_> = state
        .media_pool
        .all()
        .filter(|m| {
            if let Some(ref bin) = selected_bin {
                if m.bin.as_ref() != Some(bin) {
                    return false;
                }
            }
            // Smart bin filter
            match smart_filter {
                super::SmartBinFilter::None => {}
                super::SmartBinFilter::Unused => {
                    if state.media_usage_cache.get(&m.id).copied().unwrap_or(0) > 0 { return false; }
                }
                super::SmartBinFilter::VideoOnly => {
                    if m.video_codec.is_empty() || m.is_still { return false; }
                }
                super::SmartBinFilter::AudioOnly => {
                    if !m.has_audio || !m.video_codec.is_empty() { return false; }
                }
                super::SmartBinFilter::StillsOnly => {
                    if !m.is_still { return false; }
                }
                super::SmartBinFilter::HighRating => {
                    if m.rating < 4 { return false; }
                }
            }
            if !search_lower.is_empty() {
                return m.filename.to_lowercase().contains(&search_lower)
                    || m.path.to_string_lossy().to_lowercase().contains(&search_lower)
                    || m.tags.iter().any(|t| t.to_lowercase().contains(&search_lower));
            }
            true
        })
        .map(|m| MediaRow {
            id: m.id,
            filename: m.filename.clone(),
            duration: m.duration,
            codec: m.video_codec.clone(),
            _resolution: m.resolution,
            rating: m.rating,
            _proxy_ready: m.proxy_ready,
            path: m.path.clone(),
            bin: m.bin.clone(),
            fps: m.fps,
            file_size_bytes: m.file_size_bytes,
            added_at: m.added_at,
            shoot_date: m.media_created_at
                .or(m.file_modified_at)
                .unwrap_or(m.added_at),
            color_label: m.color_label,
        })
        .collect();

    // Sort
    match sort_by {
        SortBy::Name => items.sort_by(|a, b| a.filename.to_lowercase().cmp(&b.filename.to_lowercase())),
        SortBy::Duration => items.sort_by(|a, b| a.duration.value.cmp(&b.duration.value)),
        SortBy::Rating => items.sort_by(|a, b| b.rating.cmp(&a.rating)),
        SortBy::Codec => items.sort_by(|a, b| a.codec.cmp(&b.codec)),
        SortBy::FileSize => items.sort_by(|a, b| a.file_size_bytes.cmp(&b.file_size_bytes)),
        SortBy::DateAdded => items.sort_by(|a, b| a.added_at.cmp(&b.added_at)),
        SortBy::ShootDate => items.sort_by(|a, b| a.shoot_date.cmp(&b.shoot_date)),
    }
    if !*sort_ascending {
        items.reverse();
    }

    // Group by bin
    let mut bins_seen: Vec<Option<String>> = Vec::new();
    for item in &items {
        if !bins_seen.contains(&item.bin) {
            bins_seen.push(item.bin.clone());
        }
    }

    // Ordered list of all visible IDs (used for Shift+click range selection)
    let all_ordered_ids: Vec<MediaId> = items.iter().map(|i| i.id).collect();

    // Ctrl+A: select all visible items when browser is focused
    if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
        state.selected_media_ids = all_ordered_ids.iter().copied().collect();
    }

    // Render
    egui::ScrollArea::vertical().id_salt("browser_media_scroll").show(ui, |ui| {
        if items.is_empty() {
            ui.add_space(40.0);
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Drop media here or use File \u{2192} Import")
                        .size(11.0)
                        .color(theme.text_dim),
                );
            });
        } else {
            match view_mode {
                BrowserViewMode::List => {
                    render_list_view(ui, state, theme, &bins_seen, &items, &all_ordered_ids, &mut cmds, renaming_bin, creating_bin, scroll_to_media_id);
                }
                BrowserViewMode::Grid => {
                    render_grid_view(ui, state, theme, &bins_seen, &items, &all_ordered_ids, &mut cmds, renaming_bin, creating_bin, scroll_to_media_id);
                }
            }
        }
    });


    cmds
}

/// Duration in seconds to HH:MM:SS string.
fn format_duration(dur: &klipp_core::RationalTime) -> String {
    let dur_secs = if dur.rate > 0 {
        dur.value as f64 / dur.rate as f64
    } else {
        0.0
    };
    let hours = (dur_secs / 3600.0) as u32;
    let mins = ((dur_secs % 3600.0) / 60.0) as u32;
    let secs = (dur_secs % 60.0) as u32;
    format!("{:02}:{:02}:{:02}", hours, mins, secs)
}

/// FPS as display string.
fn format_fps(fps: &klipp_core::Fps) -> String {
    if fps.den == 1 {
        format!("{}", fps.num)
    } else {
        format!("{:.2}", fps.num as f64 / fps.den as f64)
    }
}

/// Check if a media item is used in the timeline.
fn is_media_used(state: &AppState, media_id: MediaId) -> bool {
    state.media_usage_cache.get(&media_id).copied().unwrap_or(0) > 0
}

// ── List View ──

#[allow(clippy::too_many_arguments)]
fn render_list_view(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    bins_seen: &[Option<String>],
    items: &[MediaRow],
    all_ordered_ids: &[MediaId],
    cmds: &mut Vec<AppCommand>,
    renaming_bin: &mut Option<(String, String)>,
    _creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
) {
    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();

        // Folder header
        let header_resp = render_bin_header(ui, theme, bin, renaming_bin, cmds);

        // Bin header context menu (only for named bins)
        if let Some(bin_name) = bin {
            header_resp.context_menu(|ui| {
                if ui.button("Rename Bin").clicked() {
                    *renaming_bin = Some((bin_name.clone(), bin_name.clone()));
                    ui.close_menu();
                }
                if ui.button("Delete Bin").clicked() {
                    cmds.push(AppCommand::DeleteBin { name: bin_name.clone() });
                    ui.close_menu();
                }
            });
        }

        // Drag-to-bin highlight
        if let Some(dragging_id) = state.dragging_media {
            if bin.is_some() && header_resp.hovered() {
                ui.painter().rect_stroke(
                    header_resp.rect,
                    2.0,
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(232, 160, 32)),
                );
                if !ui.ctx().input(|i| i.pointer.primary_down()) {
                    // Released over bin header — move item
                    cmds.push(AppCommand::MoveItemToBin {
                        media_id: dragging_id,
                        bin: bin.clone(),
                    });
                    state.dragging_media = None;
                }
            }
        }

        for item in &bin_items {
            let is_selected = state.selected_media_ids.contains(&item.id)
                || (state.selected_media_ids.is_empty() && state.source_media_id == Some(item.id));

            // Fixed-height row: allocate exact rect, then paint content inside it
            let row_h = 52.0;
            let avail = ui.available_rect_before_wrap();
            let row_rect = egui::Rect::from_min_size(
                avail.min,
                egui::vec2(avail.width(), row_h),
            );
            let resp = ui.allocate_rect(row_rect, egui::Sense::click_and_drag());

            // Scroll-to if requested
            if *scroll_to_media_id == Some(item.id) {
                resp.scroll_to_me(Some(egui::Align::Center));
                *scroll_to_media_id = None;
            }

            // Background
            let hovered = resp.hovered();
            let bg = if is_selected {
                theme.accent_subtle
            } else if hovered {
                egui::Color32::from_rgb(42, 42, 50)
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(row_rect, 0.0, bg);

            // Selected: 2px amber left border
            if is_selected {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(row_rect.min, egui::vec2(2.0, row_h)),
                    0.0,
                    egui::Color32::from_rgb(232, 160, 32),
                );
            }

            // Colour label stripe (3px left edge, after selection border)
            if let Some((r, g, b)) = item.color_label.rgb() {
                let stripe_x = if is_selected { row_rect.min.x + 2.0 } else { row_rect.min.x };
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(stripe_x, row_rect.min.y),
                        egui::vec2(3.0, row_h),
                    ),
                    0.0,
                    egui::Color32::from_rgb(r, g, b),
                );
            }

            // Thumbnail (80x45), vertically centred in the row
            let thumb_x = row_rect.min.x + 12.0;
            let thumb_y = row_rect.min.y + (row_h - 45.0) / 2.0;
            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(thumb_x, thumb_y),
                egui::vec2(80.0, 45.0),
            );
            if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                ui.painter().image(
                    tex.id(),
                    thumb_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else {
                ui.painter().rect_filled(thumb_rect, 3.0, theme.bg_raised);
            }

            // Hover skim (list view: use whole row as skim area)
            handle_hover_skim(ui, state, &row_rect, item, cmds, false);

            // Info: filename (single line, truncated) + metadata
            let text_x = thumb_rect.max.x + 8.0;
            let text_max_w = row_rect.max.x - text_x - 24.0; // leave space for used dot

            // Filename — truncate to fit
            let display_name = truncate_to_width(
                &item.filename,
                egui::FontId::proportional(13.0),
                text_max_w,
                ui,
            );
            ui.painter().text(
                egui::pos2(text_x, row_rect.min.y + 10.0),
                egui::Align2::LEFT_TOP,
                &display_name,
                egui::FontId::proportional(13.0),
                theme.text_primary,
            );

            // Metadata line
            let dur_str = format_duration(&item.duration);
            let fps_str = format_fps(&item.fps);
            let size_mb = item.file_size_bytes as f64 / (1024.0 * 1024.0);
            let size_str = if size_mb >= 1024.0 {
                format!("{:.1}GB", size_mb / 1024.0)
            } else {
                format!("{:.1}MB", size_mb)
            };
            let meta = format!(
                "{} \u{00b7} {}fps \u{00b7} {} \u{00b7} {}",
                dur_str, fps_str, item.codec, size_str
            );
            ui.painter().text(
                egui::pos2(text_x, row_rect.min.y + 30.0),
                egui::Align2::LEFT_TOP,
                &meta,
                egui::FontId::monospace(10.0),
                theme.text_dim,
            );

            // Used indicator (right side)
            if is_media_used(state, item.id) {
                ui.painter().circle_filled(
                    egui::pos2(row_rect.max.x - 14.0, row_rect.center().y),
                    3.0,
                    egui::Color32::from_rgb(232, 160, 32),
                );
            }

            if resp.clicked() {
                let modifiers = ui.input(|i| i.modifiers);
                handle_media_click(
                    item.id,
                    &modifiers,
                    &all_ordered_ids,
                    &mut state.selected_media_ids,
                    &mut state.last_clicked_media_id,
                );
                // Clear timeline clip selection so inspector shows bin media (State 2)
                state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if resp.double_clicked() {
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id });
            }
            if resp.dragged() {
                // Drag all selected items if the dragged item is in the selection
                if state.selected_media_ids.contains(&item.id) && state.selected_media_ids.len() > 1 {
                    state.dragging_media = Some(item.id);
                } else {
                    state.dragging_media = Some(item.id);
                }
            }
            resp.context_menu(|ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, cmds);
                }
            });
        }
    }
}

// ── Grid View ──

const GRID_CARD_MIN_W: f32 = 140.0;
const GRID_GAP: f32 = 8.0;
const GRID_PADDING: f32 = 8.0;
const GRID_THUMB_ASPECT: f32 = 16.0 / 9.0; // 16:9 thumbnail area
const GRID_INFO_H: f32 = 38.0; // space below thumbnail for name + meta

#[allow(clippy::too_many_arguments)]
fn render_grid_view(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    bins_seen: &[Option<String>],
    items: &[MediaRow],
    all_ordered_ids: &[MediaId],
    cmds: &mut Vec<AppCommand>,
    renaming_bin: &mut Option<(String, String)>,
    _creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
) {
    let avail_w = ui.available_width() - GRID_PADDING * 2.0;
    let cols = ((avail_w + GRID_GAP) / (GRID_CARD_MIN_W + GRID_GAP)).floor().max(1.0) as usize;
    let card_w = (avail_w - (cols as f32 - 1.0) * GRID_GAP) / cols as f32;
    let thumb_h = (card_w / GRID_THUMB_ASPECT).round();
    let card_h = thumb_h + GRID_INFO_H;

    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();

        // Folder header with rename support
        let header_resp = render_bin_header(ui, theme, bin, renaming_bin, cmds);
        if let Some(bin_name) = bin {
            header_resp.context_menu(|ui| {
                if ui.button("Rename Bin").clicked() {
                    *renaming_bin = Some((bin_name.clone(), bin_name.clone()));
                    ui.close_menu();
                }
                if ui.button("Delete Bin").clicked() {
                    cmds.push(AppCommand::DeleteBin { name: bin_name.clone() });
                    ui.close_menu();
                }
            });
        }
        ui.add_space(4.0);

        // Grid layout — manual positioning
        let mut col_idx = 0;
        let row_start_x = ui.cursor().min.x + GRID_PADDING;
        let mut row_start_y = ui.cursor().min.y;

        for item in &bin_items {
            let is_selected = state.selected_media_ids.contains(&item.id)
                || (state.selected_media_ids.is_empty() && state.source_media_id == Some(item.id));

            let card_pos = egui::pos2(
                row_start_x + col_idx as f32 * (card_w + GRID_GAP),
                row_start_y,
            );
            let card_rect = egui::Rect::from_min_size(card_pos, egui::vec2(card_w, card_h));
            let card_resp = ui.allocate_rect(card_rect, egui::Sense::click_and_drag());
            let hovered = card_resp.hovered();

            // Scroll-to if requested
            if *scroll_to_media_id == Some(item.id) {
                card_resp.scroll_to_me(Some(egui::Align::Center));
                *scroll_to_media_id = None;
            }

            // Card background
            ui.painter().rect_filled(card_rect, 4.0, theme.bg_deep);

            // ── Thumbnail area (top portion, 16:9) ──
            let thumb_rect = egui::Rect::from_min_size(card_pos, egui::vec2(card_w, thumb_h));

            // Render poster thumbnail — fill the 16:9 area completely (cover mode)
            if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                let tex_size = tex.size_vec2();
                if tex_size.x > 0.0 && tex_size.y > 0.0 {
                    // Use UV cropping to cover the thumb_rect while preserving aspect ratio
                    let src_aspect = tex_size.x / tex_size.y;
                    let dst_aspect = thumb_rect.width() / thumb_rect.height();
                    let uv_rect = if src_aspect > dst_aspect {
                        // Source is wider — crop sides
                        let visible_frac = dst_aspect / src_aspect;
                        let offset = (1.0 - visible_frac) / 2.0;
                        egui::Rect::from_min_max(
                            egui::pos2(offset, 0.0),
                            egui::pos2(1.0 - offset, 1.0),
                        )
                    } else {
                        // Source is taller — crop top/bottom
                        let visible_frac = src_aspect / dst_aspect;
                        let offset = (1.0 - visible_frac) / 2.0;
                        egui::Rect::from_min_max(
                            egui::pos2(0.0, offset),
                            egui::pos2(1.0, 1.0 - offset),
                        )
                    };
                    ui.painter().image(
                        tex.id(),
                        thumb_rect,
                        uv_rect,
                        egui::Color32::WHITE,
                    );
                }
            } else {
                ui.painter().rect_filled(thumb_rect, 0.0, theme.bg_raised);
                let icon_size = 24.0;
                let icon_rect = egui::Rect::from_center_size(thumb_rect.center(), egui::vec2(icon_size, icon_size));
                Icon::FilmSlate.image(icon_size).tint(theme.text_dim).paint_at(ui, icon_rect);
            }

            // Skim indicator: if transcoding, show subtle spinner overlay
            if state.skim_transcoding.contains(&item.id) {
                let spin_center = egui::pos2(thumb_rect.max.x - 14.0, thumb_rect.min.y + 10.0);
                let spin_rect = egui::Rect::from_center_size(spin_center, egui::vec2(12.0, 12.0));
                Icon::ArrowClockwise.image(12.0).tint(egui::Color32::from_rgba_premultiplied(255, 255, 255, 140)).paint_at(ui, spin_rect);
            }

            // Card border (drawn after thumbnail so it's on top)
            let border_color = if is_selected {
                egui::Color32::from_rgb(232, 160, 32)
            } else if hovered {
                theme.accent_dim
            } else {
                theme.border
            };
            ui.painter().rect_stroke(card_rect, 4.0, egui::Stroke::new(1.0, border_color));

            // Hover skim (grid view: white scrubber line over thumbnail)
            handle_hover_skim(ui, state, &thumb_rect, item, cmds, true);

            // ── Info area (below thumbnail) ──
            let info_y = thumb_rect.max.y + 4.0;
            let text_max_w = card_w - 24.0;

            // Filename (single line, truncated)
            let display_name = truncate_to_width(
                &item.filename,
                egui::FontId::proportional(11.0),
                text_max_w,
                ui,
            );
            ui.painter().text(
                egui::pos2(card_pos.x + 6.0, info_y),
                egui::Align2::LEFT_TOP,
                &display_name,
                egui::FontId::proportional(11.0),
                theme.text_primary,
            );

            // Used indicator
            if is_media_used(state, item.id) {
                ui.painter().circle_filled(
                    egui::pos2(card_rect.max.x - 10.0, info_y + 6.0),
                    3.0,
                    egui::Color32::from_rgb(232, 160, 32),
                );
            }

            // Metadata line
            let dur_str = format_duration(&item.duration);
            let fps_str = format_fps(&item.fps);
            let meta_str = format!("{} \u{00b7} {}fps", dur_str, fps_str);
            ui.painter().text(
                egui::pos2(card_pos.x + 6.0, info_y + 16.0),
                egui::Align2::LEFT_TOP,
                &meta_str,
                egui::FontId::monospace(9.0),
                theme.text_dim,
            );

            // ── Interactions ──
            if card_resp.clicked() {
                let modifiers = ui.input(|i| i.modifiers);
                handle_media_click(
                    item.id,
                    &modifiers,
                    &all_ordered_ids,
                    &mut state.selected_media_ids,
                    &mut state.last_clicked_media_id,
                );
                // Clear timeline clip selection so inspector shows bin media (State 2)
                state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if card_resp.double_clicked() {
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id });
            }

            // Drag disambiguation
            if card_resp.drag_started() {
                if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                    state.browser_drag_start = Some((pos, std::time::Instant::now()));
                }
            }
            if card_resp.dragged() {
                if let Some((start_pos, _)) = state.browser_drag_start {
                    if let Some(current_pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                        let dx = (current_pos.x - start_pos.x).abs();
                        if dx > 8.0 && thumb_rect.contains(start_pos) {
                            // Range selection within thumbnail
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                            let dur_secs = if item.duration.rate > 0 {
                                item.duration.value as f64 / item.duration.rate as f64
                            } else { 0.0 };
                            let frac_s = ((start_pos.x - thumb_rect.min.x) / thumb_rect.width()).clamp(0.0, 1.0);
                            let frac_e = ((current_pos.x - thumb_rect.min.x) / thumb_rect.width()).clamp(0.0, 1.0);
                            state.browser_range_selection = Some(crate::state::BrowserClipSelection {
                                media_id: item.id,
                                range_in_secs: (frac_s.min(frac_e) as f64) * dur_secs,
                                range_out_secs: (frac_s.max(frac_e) as f64) * dur_secs,
                            });
                            // Amber overlay
                            let sl = thumb_rect.min.x + frac_s.min(frac_e) * thumb_rect.width();
                            let sr = thumb_rect.min.x + frac_s.max(frac_e) * thumb_rect.width();
                            ui.painter().rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(sl, thumb_rect.min.y),
                                    egui::pos2(sr, thumb_rect.max.y),
                                ),
                                0.0,
                                egui::Color32::from_rgba_premultiplied(232, 160, 32, 60),
                            );
                        } else if dx > 8.0 {
                            state.dragging_media = Some(item.id);
                        }
                    }
                }
            }
            if card_resp.drag_stopped() {
                state.browser_drag_start = None;
            }

            // Persistent range overlay
            if let Some(ref sel) = state.browser_range_selection {
                if sel.media_id == item.id && !card_resp.dragged() {
                    let dur_secs = if item.duration.rate > 0 {
                        item.duration.value as f64 / item.duration.rate as f64
                    } else { 1.0 };
                    let sl = thumb_rect.min.x + (sel.range_in_secs / dur_secs) as f32 * thumb_rect.width();
                    let sr = thumb_rect.min.x + (sel.range_out_secs / dur_secs) as f32 * thumb_rect.width();
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(sl, thumb_rect.min.y),
                            egui::pos2(sr, thumb_rect.max.y),
                        ),
                        0.0,
                        egui::Color32::from_rgba_premultiplied(232, 160, 32, 60),
                    );
                }
            }

            card_resp.context_menu(|ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, cmds);
                }
            });

            col_idx += 1;
            if col_idx >= cols {
                col_idx = 0;
                row_start_y += card_h + GRID_GAP;
            }
        }

        // Reserve space for all rows
        if col_idx > 0 {
            row_start_y += card_h + GRID_GAP;
        }
        let total_h = row_start_y - ui.cursor().min.y;
        if total_h > 0.0 {
            ui.allocate_space(egui::vec2(avail_w, total_h));
        }
    }
}

/// Truncate text to fit within max_width, adding ellipsis if needed.
fn truncate_to_width(text: &str, font: egui::FontId, max_w: f32, ui: &egui::Ui) -> String {
    if text.is_empty() || max_w <= 0.0 {
        return text.to_string();
    }
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE);
    if galley.size().x <= max_w {
        return text.to_string();
    }
    // Simple linear scan (short strings, fast enough)
    let chars: Vec<char> = text.chars().collect();
    for len in (1..chars.len()).rev() {
        let candidate: String = chars[..len].iter().chain(std::iter::once(&'\u{2026}')).collect();
        let g = ui.painter().layout_no_wrap(candidate.clone(), font.clone(), egui::Color32::WHITE);
        if g.size().x <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// Render a bin folder header, with inline rename support.
fn render_bin_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    bin: &Option<String>,
    renaming_bin: &mut Option<(String, String)>,
    cmds: &mut Vec<AppCommand>,
) -> egui::Response {
    let bin_label = bin.as_deref().unwrap_or("Ungrouped");

    // Check if we're renaming this bin
    let mut should_clear_rename = false;
    if let Some((ref old_name, ref mut edit_text)) = renaming_bin {
        if bin.as_deref() == Some(old_name.as_str()) {
            let old_clone = old_name.clone();
            let resp = ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(Icon::Folder.image(10.0).tint(theme.text_dim));
                let text_resp = ui.add(
                    egui::TextEdit::singleline(edit_text)
                        .desired_width(120.0)
                        .font(egui::FontId::proportional(10.0)),
                );
                // Commit on lost focus (Enter, click away, Escape)
                if text_resp.lost_focus() {
                    let new_name = edit_text.trim().to_string();
                    if !new_name.is_empty() && new_name != old_clone {
                        cmds.push(AppCommand::RenameBin {
                            old_name: old_clone,
                            new_name,
                        });
                    }
                    should_clear_rename = true;
                }
                text_resp.request_focus();
            });
            if should_clear_rename {
                *renaming_bin = None;
            }
            return resp.response;
        }
    }

    // Normal header
    let resp = ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("\u{1F4C1}")
                .size(10.0)
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(bin_label.to_uppercase())
                .size(10.0)
                .strong()
                .color(theme.text_secondary),
        );
    });
    resp.response
}

/// Handle hover skimming: when hovering over a browser clip, show it in the source viewer.
/// Layer 1: Use the poster thumbnail (good quality, instant).
/// Layer 3: Once skim file exists, use BrowserSkimFrame for full-resolution per-frame scrub.
/// Returns true if skimming is active on this item.
fn handle_hover_skim(
    ui: &egui::Ui,
    state: &mut AppState,
    skim_rect: &egui::Rect,
    item: &MediaRow,
    cmds: &mut Vec<AppCommand>,
    draw_scrubber: bool,
) -> bool {
    if !state.skimming_enabled || state.has_modal_open() {
        return false;
    }
    // Use raw pointer position
    let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());
    let hover_pos = match pointer_pos {
        Some(p) if skim_rect.contains(p) => p,
        _ => return false,
    };
    state.browser_skim_active_this_frame = true;

    // Calculate skim speed (px/sec) for audio mode selection
    let now = std::time::Instant::now();
    let dt = now.duration_since(state.skim_last_update).as_secs_f32();
    let dx = (hover_pos.x - state.skim_last_x).abs();
    // EMA-smooth to avoid jitter (alpha=0.3)
    let raw_speed = if dt > 0.001 { dx / dt } else { 0.0 };
    state.skim_speed_px_per_sec = state.skim_speed_px_per_sec * 0.7 + raw_speed * 0.3;
    state.skim_last_x = hover_pos.x;
    state.skim_last_update = now;

    let fraction = ((hover_pos.x - skim_rect.min.x) / skim_rect.width()).clamp(0.0, 1.0);
    let dur_secs = if item.duration.rate > 0 {
        item.duration.value as f64 / item.duration.rate as f64
    } else {
        0.0
    };
    let skim_time = dur_secs * fraction as f64;

    // Check if skim file is ready (Layer 3 — full resolution per-frame decode)
    let has_skim_file = state.media_pool.get(&item.id)
        .and_then(|m| m.skim_file.as_ref())
        .is_some_and(|p| p.exists());

    if has_skim_file {
        // Layer 3: Request full-resolution frame from skim file
        if !state.skim_decode_in_flight {
            cmds.push(AppCommand::BrowserSkimFrame {
                media_id: item.id,
                time_secs: skim_time,
            });
        } else {
            // Queue latest position so it's picked up when current decode finishes
            state.skim_queued_request = Some((item.id, skim_time));
        }
    }

    // If we already have a skim texture (from Layer 3 decode), keep using it.
    // Only fall back to poster thumbnail if nothing else is available.
    if state.browser_skim_texture.is_none() {
        // No Layer 3 frame yet — show poster as fallback
        if let Some(tex) = state.thumbnail_textures.get(&item.id) {
            state.browser_skim_texture = Some(tex.clone());
            state.browser_skim_media_id = Some(item.id);
        }
    } else {
        // Keep existing skim texture (from Layer 3), just update the media_id
        state.browser_skim_media_id = Some(item.id);
    }

    // Switch viewer to Source mode directly (not via command queue — needs to take
    // effect THIS frame so the viewer renders the skim texture)
    state.viewer_mode = ViewerMode::Source;
    state.source_media_id = Some(item.id);

    // Draw white scrubber line (grid view)
    if draw_scrubber {
        ui.painter().line_segment(
            [
                egui::Pos2::new(hover_pos.x, skim_rect.min.y),
                egui::Pos2::new(hover_pos.x, skim_rect.max.y),
            ],
            egui::Stroke::new(1.5, egui::Color32::WHITE),
        );
    }

    // Draw position indicator on thumbnail (thin vertical line)
    ui.painter().line_segment(
        [
            egui::Pos2::new(hover_pos.x, skim_rect.min.y),
            egui::Pos2::new(hover_pos.x, skim_rect.max.y),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
    );

    // Audio skimming: dispatch Start or Update (throttled)
    if state.audio_skimming_enabled {
        let is_new_session = state.skim_audio_session.is_none();
        // Throttle updates: 80ms minimum between dispatches
        let elapsed = state.skim_last_audio_dispatch.elapsed().as_millis() as u64;

        if is_new_session {
            state.skim_last_audio_dispatch = std::time::Instant::now();
            cmds.push(AppCommand::BrowserSkimAudioStart {
                media_id: item.id,
                source_path: item.path.clone(),
                time_secs: skim_time,
            });
        } else if elapsed >= 16 {
            state.skim_last_audio_dispatch = std::time::Instant::now();
            cmds.push(AppCommand::BrowserSkimAudioUpdate {
                media_id: item.id,
                time_secs: skim_time,
            });
        }
    }

    true
}

struct MediaRow {
    id: MediaId,
    filename: String,
    duration: klipp_core::RationalTime,
    codec: String,
    _resolution: klipp_core::Resolution,
    rating: u8,
    _proxy_ready: bool,
    path: std::path::PathBuf,
    bin: Option<String>,
    fps: klipp_core::Fps,
    file_size_bytes: u64,
    added_at: u64,
    /// Effective shoot date: media_created_at > file_modified_at > added_at
    shoot_date: u64,
    color_label: klipp_media::ColorLabel,
}

fn show_context_menu(
    ui: &mut egui::Ui,
    media_id: MediaId,
    path: &std::path::Path,
    cmds: &mut Vec<AppCommand>,
) {
    if ui.button("Load in Source").clicked() {
        cmds.push(AppCommand::LoadSourceMedia { media_id });
        ui.close_menu();
    }
    if ui.button("Add to Timeline").clicked() {
        cmds.push(AppCommand::ImportMediaLinked { media_id });
        ui.close_menu();
    }
    if ui.button("Generate Proxy").clicked() {
        cmds.push(AppCommand::GenerateProxy { media_id });
        ui.close_menu();
    }
    if ui.button("Regenerate Filmstrip").clicked() {
        cmds.push(AppCommand::RegenerateFilmstrip { media_id });
        ui.close_menu();
    }
    if ui.button("Regenerate Skim Transcode").clicked() {
        cmds.push(AppCommand::RegenerateSkimTranscode { media_id });
        ui.close_menu();
    }
    ui.separator();
    ui.menu_button("Set Rating", |ui| {
        for r in 0..=5u8 {
            let label = if r == 0 {
                "None".to_string()
            } else {
                "\u{2605}".repeat(r as usize)
            };
            if ui.button(&label).clicked() {
                cmds.push(AppCommand::SetMediaRating { media_id, rating: r });
                ui.close_menu();
            }
        }
    });
    ui.menu_button("Label colour", |ui| {
        for label in klipp_media::ColorLabel::ALL {
            let mut text = egui::RichText::new(label.label()).size(11.0);
            if let Some((r, g, b)) = label.rgb() {
                text = text.color(egui::Color32::from_rgb(r, g, b));
            }
            if ui.button(text).clicked() {
                cmds.push(AppCommand::SetMediaColorLabel { media_id, label: label.to_u8() });
                ui.close_menu();
            }
        }
    });
    ui.separator();
    if ui.button("Move to Root").clicked() {
        cmds.push(AppCommand::MoveItemToBin { media_id, bin: None });
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Remove from Project").clicked() {
        cmds.push(AppCommand::RemoveFromPool { media_id });
        ui.close_menu();
    }
    if ui.button("Reveal in File Manager").clicked() {
        cmds.push(AppCommand::RevealInFileManager {
            path: path.to_path_buf(),
        });
        ui.close_menu();
    }
}

fn handle_media_click(
    clicked_id: MediaId,
    modifiers: &egui::Modifiers,
    ordered_ids: &[MediaId],
    selected: &mut std::collections::HashSet<MediaId>,
    last_clicked: &mut Option<MediaId>,
) {
    if modifiers.ctrl || modifiers.command {
        // Toggle individual item
        if !selected.remove(&clicked_id) {
            selected.insert(clicked_id);
        }
        *last_clicked = Some(clicked_id);
    } else if modifiers.shift {
        // Range select from last_clicked to clicked_id
        if let Some(anchor) = *last_clicked {
            let a = ordered_ids.iter().position(|x| *x == anchor).unwrap_or(0);
            let b = ordered_ids.iter().position(|x| *x == clicked_id).unwrap_or(0);
            let (lo, hi) = (a.min(b), a.max(b));
            selected.clear();
            for i in lo..=hi {
                selected.insert(ordered_ids[i]);
            }
        } else {
            selected.clear();
            selected.insert(clicked_id);
            *last_clicked = Some(clicked_id);
        }
    } else {
        // Plain click — single select
        selected.clear();
        selected.insert(clicked_id);
        *last_clicked = Some(clicked_id);
    }
}

fn show_multi_context_menu(
    ui: &mut egui::Ui,
    selected_ids: &std::collections::HashSet<MediaId>,
    cmds: &mut Vec<AppCommand>,
) {
    let count = selected_ids.len();
    ui.label(egui::RichText::new(format!("{count} items selected")).strong());
    ui.separator();
    ui.menu_button("Move to bin", |ui| {
        if ui.button("Root (no bin)").clicked() {
            for &id in selected_ids {
                cmds.push(AppCommand::MoveItemToBin { media_id: id, bin: None });
            }
            ui.close_menu();
        }
    });
    ui.menu_button("Set Rating", |ui| {
        for r in 0..=5u8 {
            let label = if r == 0 {
                "None".to_string()
            } else {
                "\u{2605}".repeat(r as usize)
            };
            if ui.button(&label).clicked() {
                for &id in selected_ids {
                    cmds.push(AppCommand::SetMediaRating { media_id: id, rating: r });
                }
                ui.close_menu();
            }
        }
    });
    ui.separator();
    if ui.button(format!("Generate Proxies ({count})")).clicked() {
        for &id in selected_ids {
            cmds.push(AppCommand::GenerateProxy { media_id: id });
        }
        ui.close_menu();
    }
    if ui.button(format!("Regenerate Filmstrips ({count})")).clicked() {
        for &id in selected_ids {
            cmds.push(AppCommand::RegenerateFilmstrip { media_id: id });
        }
        ui.close_menu();
    }
    ui.separator();
    if ui.button(format!("Remove {count} items from Project")).clicked() {
        for &id in selected_ids {
            cmds.push(AppCommand::RemoveFromPool { media_id: id });
        }
        ui.close_menu();
    }
}
