use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MediaId};

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
    List,
    /// Default: card layout with thumbnail on top, truncated filename below
    /// — the Resolve / Premiere convention. List view stays available as the
    /// dense option for power-user browsing of large bins.
    #[default]
    Grid,
}

/// Returns true when an item with the given rating should pass the minimum
/// rating filter. `None` disables the filter (all ratings pass). Pure —
/// testable without egui.
pub fn passes_rating_filter(rating: u8, min: Option<u8>) -> bool {
    match min {
        None => true,
        Some(min_r) => rating >= min_r,
    }
}

/// Returns true when an item with the given colour label should pass the
/// bitmask filter. `filter == 0` disables the filter (all labels pass).
/// Bit N represents `ColorLabel::from_u8(N)` — bit 0 is the "no label"
/// slot, bits 1..=6 are the six colours in `ColorLabel::ALL` order.
/// Multiple bits combine with OR semantics (match any).
pub fn passes_color_filter(label: klipp_media::ColorLabel, filter: u8) -> bool {
    if filter == 0 {
        return true;
    }
    let bit = 1u8 << label.to_u8();
    filter & bit != 0
}

/// Returns true when a media item should pass the smart-bin filter.
/// `usage_count` is the cached number of timeline references to the
/// item — only consulted by `Unused`. Pure — testable without egui.
pub fn passes_smart_bin_filter(
    item: &klipp_media::MediaItem,
    filter: super::SmartBinFilter,
    usage_count: usize,
) -> bool {
    match filter {
        super::SmartBinFilter::None => true,
        super::SmartBinFilter::Unused => usage_count == 0,
        super::SmartBinFilter::VideoOnly => !item.video_codec.is_empty() && !item.is_still,
        super::SmartBinFilter::AudioOnly => item.has_audio && item.video_codec.is_empty(),
        super::SmartBinFilter::StillsOnly => item.is_still,
        super::SmartBinFilter::Offline => item.offline,
    }
}

/// Returns true when a media item should pass the metadata filter.
/// AND-combined with the smart-bin / rating / colour filters by the
/// caller. Pure — testable without egui.
pub fn passes_metadata_filter(
    item: &klipp_media::MediaItem,
    filter: &super::MetadataFilter,
) -> bool {
    use super::MetadataFilter;
    match filter {
        MetadataFilter::Any => true,
        MetadataFilter::Codec(c) => item.video_codec.eq_ignore_ascii_case(c),
        MetadataFilter::Fps { num, den } => {
            item.fps.num == *num && item.fps.den == *den
        }
        MetadataFilter::Camera(c) => item.camera_make.as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(c)),
    }
}

/// Walk the media pool and return distinct values for each metadata
/// dimension, sorted alphabetically / numerically. Empty / None values
/// are skipped (a clip with no recorded camera-make doesn't show up
/// in the camera dropdown). Used by the toolbar to populate the
/// metadata filter dropdown.
///
/// Returns a tuple of (codecs, fps_pairs, cameras). Codec strings
/// preserve their stored case for display but are matched
/// case-insensitively in `passes_metadata_filter`.
pub fn enumerate_metadata_filter_values(
    items: &[klipp_media::MediaItem],
) -> (Vec<String>, Vec<(u32, u32)>, Vec<String>) {
    use std::collections::BTreeSet;
    let mut codecs: BTreeSet<String> = BTreeSet::new();
    let mut fps_set: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut cameras: BTreeSet<String> = BTreeSet::new();
    for item in items {
        if !item.video_codec.is_empty() {
            codecs.insert(item.video_codec.clone());
        }
        // Skip 1/1 (fallback / unknown) — not a meaningful filter
        // value and would otherwise pollute the dropdown.
        if !(item.fps.num == 1 && item.fps.den == 1) {
            fps_set.insert((item.fps.num, item.fps.den));
        }
        if let Some(make) = &item.camera_make {
            if !make.is_empty() {
                cameras.insert(make.clone());
            }
        }
    }
    (codecs.into_iter().collect(), fps_set.into_iter().collect(), cameras.into_iter().collect())
}

/// Format an (fps_num, fps_den) pair as a human-readable label —
/// "24" for film 24 (24/1), "23.976" for ntsc-24 (24000/1001), etc.
/// Used by the metadata filter dropdown so users see "FPS: 23.976"
/// rather than "FPS: 24000/1001".
pub fn format_fps_label(num: u32, den: u32) -> String {
    if den == 0 {
        return "—".to_string();
    }
    let f = num as f64 / den as f64;
    if (f - f.round()).abs() < 1e-3 {
        format!("{}", f.round() as u32)
    } else {
        format!("{:.3}", f)
    }
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
    view_mode: &mut BrowserViewMode,
    renaming_bin: &mut Option<(String, String)>,
    creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
    smart_filter: super::SmartBinFilter,
    min_rating_filter: Option<u8>,
    color_label_filter: u8,
    metadata_filter: &super::MetadataFilter,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Capture the panel rect upfront for the file-browser drag-drop handler
    // at the bottom of this function.
    let panel_rect = ui.max_rect();

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
            // Smart bin filter (AND-combined with the rating filter below).
            let usage = state.media_usage_cache.get(&m.id).copied().unwrap_or(0);
            if !passes_smart_bin_filter(m, smart_filter, usage) {
                return false;
            }
            // Minimum star rating filter.
            if !passes_rating_filter(m.rating, min_rating_filter) {
                return false;
            }
            // Colour-label filter (bitmask, OR semantics).
            // `0` disables the filter. `ColorLabel::None` (to_u8() == 0) is
            // bit 0 — setting it filters to unlabelled clips only.
            if !passes_color_filter(m.color_label, color_label_filter) {
                return false;
            }
            // Metadata filter (codec / fps / camera). Any disables.
            if !passes_metadata_filter(m, metadata_filter) {
                return false;
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
            resolution: m.resolution,
            camera: {
                let make = m.camera_make.as_deref().unwrap_or("");
                let model = m.camera_model.as_deref().unwrap_or("");
                if make.is_empty() && model.is_empty() { None }
                else if make.is_empty() { Some(model.to_string()) }
                else if model.is_empty() { Some(make.to_string()) }
                else { Some(format!("{make} {model}")) }
            },
            bit_depth: m.bit_depth,
            pixel_format: m.pixel_format.clone(),
            video_bitrate_kbps: m.video_bitrate_kbps,
            offline: m.offline,
            parent_media_id: m.parent_media_id,
            has_zone: m.default_source_in.is_some() && m.default_source_out.is_some(),
            kind: m.kind.clone(),
        })
        .collect();

    // Sort
    match sort_by {
        SortBy::Name => items.sort_by_key(|a| a.filename.to_lowercase()),
        SortBy::Duration => items.sort_by_key(|a| a.duration.value),
        SortBy::Rating => items.sort_by_key(|a| std::cmp::Reverse(a.rating)),
        SortBy::Codec => items.sort_by(|a, b| a.codec.cmp(&b.codec)),
        SortBy::FileSize => items.sort_by_key(|a| a.file_size_bytes),
        SortBy::DateAdded => items.sort_by_key(|a| a.added_at),
        SortBy::ShootDate => items.sort_by_key(|a| a.shoot_date),
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

        // Empty-area context menu: claim remaining space below items
        let remaining = ui.available_rect_before_wrap();
        let min_h = 40.0_f32.max(remaining.height());
        let bg_rect = egui::Rect::from_min_size(remaining.min, egui::vec2(remaining.width(), min_h));
        let bg_resp = ui.allocate_rect(bg_rect, egui::Sense::click());
        bg_resp.context_menu(|ui| {
            ui.set_min_width(180.0);

            if ui.button("New Folder").clicked() {
                *creating_bin = Some("New Folder".to_string());
                ui.close_menu();
            }

            ui.separator();

            if ui.button("Import Media\u{2026}").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Media", super::media_extensions())
                    .pick_file()
                {
                    cmds.push(AppCommand::ImportMedia { path });
                }
                ui.close_menu();
            }

            if ui.button("Import Folder\u{2026}").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    cmds.push(AppCommand::ImportMediaFolder { directory: dir });
                }
                ui.close_menu();
            }

            ui.separator();

            ui.menu_button("Sort By", |ui| {
                ui.selectable_value(sort_by, SortBy::Name, "Name");
                ui.selectable_value(sort_by, SortBy::DateAdded, "Date Added");
                ui.selectable_value(sort_by, SortBy::ShootDate, "Shoot Date");
                ui.selectable_value(sort_by, SortBy::Duration, "Duration");
                ui.selectable_value(sort_by, SortBy::Rating, "Rating");
                ui.selectable_value(sort_by, SortBy::Codec, "Codec");
                ui.selectable_value(sort_by, SortBy::FileSize, "File Size");
            });

            ui.menu_button("View", |ui| {
                if ui.selectable_value(view_mode, BrowserViewMode::List, "List").clicked() {
                    ui.close_menu();
                }
                if ui.selectable_value(view_mode, BrowserViewMode::Grid, "Grid").clicked() {
                    ui.close_menu();
                }
            });

            let (sort_icon, sort_text) = if *sort_ascending {
                (Icon::SortAscending, "Ascending")
            } else {
                (Icon::SortDescending, "Descending")
            };
            if ui.add(egui::Button::image_and_text(
                sort_icon.image(12.0),
                sort_text,
            )).clicked() {
                *sort_ascending = !*sort_ascending;
                ui.close_menu();
            }
        });
    });

    // ── File-browser drag-and-drop ──
    // If the user is dragging files from the FILES tab and the cursor is
    // inside this media-pool panel, paint a drop-accept outline. On release,
    // import all dragged files via LoadMediaFile (no clip placement —
    // they just land in the media pool, ready to be added to a timeline
    // by the user later).
    let drag_paths: Option<Vec<std::path::PathBuf>> = match &state.drag_drop.payload {
        Some(crate::drag_drop::DragPayload::FilePaths { paths }) => Some(paths.clone()),
        _ => None,
    };
    if let Some(paths) = drag_paths {
        let hover_pos = ui.input(|i| i.pointer.hover_pos());
        let any_down = ui.input(|i| i.pointer.any_down());
        if let Some(pos) = hover_pos {
            if panel_rect.contains(pos) {
                ui.painter().rect_stroke(
                    panel_rect,
                    0.0,
                    egui::Stroke::new(2.0_f32, theme.marker_info),
                );
                if !any_down {
                    state.drag_drop.end();
                    let count = paths.len();
                    for path in paths {
                        cmds.push(AppCommand::LoadMediaFile {
                            path,
                            media_id: MediaId::new(),
                        });
                    }
                    state.toast.show(format!("Imported {} file(s) to media pool", count));
                }
            }
        }
    }

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

/// Paint two non-conflicting "usage state" indicators on a media tile,
/// matching the Resolve convention:
///   * `loaded_in_source` → 2px accent border around the thumbnail rect.
///   * `on_timeline`      → 3px solid red bar across the bottom edge of
///     the thumbnail. Red because it must read instantly against any
///     thumbnail content; the accent amber is reserved for selection /
///     focus signalling so red carves out its own meaning ("in use").
///
/// Both can be visible at once. Drawn in this order so the bottom 3px
/// of the source-loaded border is overpainted by the red bar — desired,
/// because the bar is the higher-information signal at that edge.
fn paint_usage_badges(
    painter: &egui::Painter,
    thumb_rect: egui::Rect,
    accent: egui::Color32,
    loaded_in_source: bool,
    on_timeline: bool,
    theme: &KlippTheme,
) {
    if loaded_in_source {
        painter.rect_stroke(
            thumb_rect,
            3.0,
            egui::Stroke::new(2.0_f32, accent),
        );
    }
    if on_timeline {
        let bar_h = 3.0_f32;
        let bar = egui::Rect::from_min_max(
            egui::pos2(thumb_rect.min.x, thumb_rect.max.y - bar_h),
            thumb_rect.max,
        );
        painter.rect_filled(bar, 0.0, theme.marker_error);
    }
}

/// Paint a small "(i)" info icon in the bottom-right of a media tile and
/// attach a rich hover tooltip with the full metadata: full filename,
/// resolution × fps, duration, codecs, channel layout, sample rate, file
/// size, and subclip parent reference. The icon sits above the red
/// "in use on timeline" bar so they don't visually fight. Mirrors the
/// Resolve convention of "card shows minimum, hover (i) for details."
fn draw_info_icon_with_tooltip(
    ui: &mut egui::Ui,
    item: &klipp_media::MediaItem,
    thumb_rect: egui::Rect,
    theme: &KlippTheme,
) {
    let icon_size = 14.0_f32;
    let pad = 4.0_f32;
    // Placed above any "in use" red bar (3 px tall) and the source-loaded
    // border (2 px). 8 px from the bottom edge keeps it readable.
    let icon_center = egui::pos2(
        thumb_rect.max.x - icon_size * 0.5 - pad,
        thumb_rect.max.y - icon_size * 0.5 - pad - 4.0,
    );
    let icon_rect = egui::Rect::from_center_size(icon_center, egui::vec2(icon_size, icon_size));
    let hit_rect = icon_rect.expand(4.0);
    // `ui.interact(...)` registers a deterministic hit area on top of the
    // card's existing `click_and_drag` response without re-allocating
    // layout space. Using `Sense::click()` (rather than `hover()`) makes
    // the hover detection reliable when overlapping a parent click sense.
    let id = ui.id().with("info_icon").with(item.id);
    let resp = ui.interact(hit_rect, id, egui::Sense::click());

    // Translucent dark backing so the glyph stays readable on bright
    // thumbnails. Opaque on hover for a clearer affordance.
    let backing_alpha: u8 = if resp.hovered() { 200 } else { 140 };
    ui.painter().circle_filled(
        icon_center,
        icon_size * 0.6,
        egui::Color32::from_rgba_premultiplied(0, 0, 0, backing_alpha),
    );
    let tint = if resp.hovered() {
        egui::Color32::WHITE
    } else {
        // Icon sits on a black-translucent backing — stays light
        // regardless of theme polarity (the backing is the contrast
        // partner, not the panel underneath).
        egui::Color32::from_rgba_premultiplied(255, 255, 255, 220)
    };
    Icon::Info.image(icon_size).tint(tint).paint_at(ui, icon_rect);

    // Pre-render every line to a `(label, value)` pair OUTSIDE the
    // closure so the closure body itself is tiny — purely a layout walk
    // over a small Vec. This dramatically reduces the LLVM IR that LTO
    // has to digest at release link time.
    let rows = build_info_tooltip_rows(item);
    let path_string = item.path.display().to_string();
    let title = item.filename.clone();
    let is_subclip = item.parent_media_id.is_some();
    let border_light = theme.border_light;
    let text_primary = theme.text_primary;
    let text_secondary = theme.text_secondary;
    let text_dim = theme.text_dim;
    let text_accent = theme.text_accent;
    let bg_panel_alt = theme.bg_panel_alt;

    resp.on_hover_ui_at_pointer(move |ui| {
        egui::Frame::none()
            .fill(bg_panel_alt)
            .stroke(egui::Stroke::new(1.0_f32, border_light))
            .rounding(5.0)
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_max_width(420.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    egui::RichText::new(&title)
                        .size(12.0)
                        .color(text_primary)
                        .strong(),
                );
                if is_subclip {
                    ui.label(
                        egui::RichText::new("Subclip")
                            .size(10.0)
                            .color(text_accent),
                    );
                }
                ui.label(
                    egui::RichText::new(&path_string)
                        .size(10.0)
                        .color(text_dim)
                        .monospace(),
                );
                ui.add_space(2.0);
                for (k, v) in &rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(egui::RichText::new(*k).size(10.0).color(text_dim));
                        ui.label(
                            egui::RichText::new(v)
                                .size(10.0)
                                .color(text_secondary)
                                .monospace(),
                        );
                    });
                }
            });
    });
}

/// Pre-computed `(label, value)` rows for the info tooltip. Lives outside
/// `draw_info_icon_with_tooltip` so the closure passed to
/// `on_hover_ui_at_pointer` stays small and LLVM doesn't have to inline
/// a giant body across every grid card during release LTO.
fn build_info_tooltip_rows(item: &klipp_media::MediaItem) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = Vec::with_capacity(8);
    if item.is_still {
        rows.push(("Type", "Still image".to_string()));
    } else {
        let dur_secs = if item.duration.rate > 0 {
            item.duration.value as f64 / item.duration.rate as f64
        } else {
            0.0
        };
        let h = (dur_secs / 3600.0) as u32;
        let m = ((dur_secs % 3600.0) / 60.0) as u32;
        let s = dur_secs % 60.0;
        rows.push(("Duration", format!("{h:02}:{m:02}:{s:06.3}")));
    }
    if item.resolution.width > 0 && item.resolution.height > 0 {
        rows.push((
            "Resolution",
            format!("{} \u{00D7} {}", item.resolution.width, item.resolution.height),
        ));
    }
    if !item.is_still {
        rows.push(("Frame rate", format_fps(&item.fps)));
    }
    if !item.video_codec.is_empty() {
        rows.push(("Video", item.video_codec.to_lowercase()));
    }
    if item.has_audio {
        let chan = match item.audio_channels.unwrap_or(0) {
            0 => "\u{2014}".to_string(),
            1 => "Mono".to_string(),
            2 => "Stereo".to_string(),
            n => format!("{n} ch"),
        };
        let audio_str = if item.audio_codec.is_empty() {
            chan
        } else {
            format!("{} \u{00B7} {}", item.audio_codec.to_lowercase(), chan)
        };
        rows.push(("Audio", audio_str));
        if let Some(sr) = item.sample_rate {
            rows.push(("Sample rate", format!("{} kHz", sr / 1000)));
        }
    }
    let size_mb = item.file_size_bytes as f64 / (1024.0 * 1024.0);
    let size_str = if item.file_size_bytes == 0 {
        "\u{2014}".to_string()
    } else if size_mb >= 1024.0 {
        format!("{:.2} GB", size_mb / 1024.0)
    } else {
        format!("{:.1} MB", size_mb)
    };
    rows.push(("File size", size_str));

    if item.default_source_in.is_some() || item.default_source_out.is_some() {
        let in_str = item
            .default_source_in
            .map(|t| t.to_timecode_string())
            .unwrap_or_else(|| "\u{2014}".to_string());
        let out_str = item
            .default_source_out
            .map(|t| t.to_timecode_string())
            .unwrap_or_else(|| "\u{2014}".to_string());
        rows.push(("In \u{00B7} Out", format!("{in_str}  \u{00B7}  {out_str}")));
    }
    rows
}

/// Dim a thumbnail and overlay an OFFLINE pill in the top-left corner.
/// Used by both list and grid views to flag media whose source file is
/// no longer present at the stored path.
fn paint_offline_badge(painter: &egui::Painter, thumb_rect: egui::Rect, theme: &KlippTheme) {
    // Darken the thumbnail with a semi-transparent black overlay.
    painter.rect_filled(
        thumb_rect,
        3.0,
        egui::Color32::from_rgba_premultiplied(0, 0, 0, 160),
    );

    // OFFLINE pill in the top-left corner.
    let label = "OFFLINE";
    let font = egui::FontId::proportional(10.0);
    let galley = painter.layout_no_wrap(label.into(), font.clone(), egui::Color32::WHITE);
    let pad_x = 5.0;
    let pad_y = 2.0;
    let pill_size = egui::vec2(galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0);
    let pill_pos = egui::pos2(thumb_rect.min.x + 4.0, thumb_rect.min.y + 4.0);
    let pill_rect = egui::Rect::from_min_size(pill_pos, pill_size);
    painter.rect_filled(pill_rect, 2.0, theme.marker_error);
    painter.text(
        pill_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        egui::Color32::WHITE,
    );
}

/// Paint a synthetic title asset's mini-thumbnail. Renders a soft
/// blue gradient canvas (matches the Library/Titles tab's default
/// preview background so users can connect "this came from Titles")
/// and draws the asset's title text centred on top with the
/// `TitleData`'s color, font weight, alignment, and shadow.
///
/// Falls back to a "MISSING ASSET" pose if the asset got deleted
/// from the project store while the synthetic `MediaItem` still
/// lingers in the pool — defensive against partial corruption.
fn paint_synthetic_title_thumb(
    ui: &egui::Ui,
    theme: &KlippTheme,
    rect: egui::Rect,
    asset_id: MediaId,
    title_assets: &std::collections::HashMap<MediaId, klipp_timeline::TitleAsset>,
) {
    let painter = ui.painter();
    // Two-tone blue gradient — same colours as the Library Titles tab
    // BlueGradient swatch so the asset reads as "made from a title".
    painter.rect_filled(
        rect,
        egui::Rounding::same(2.0),
        egui::Color32::from_rgb(0x4d, 0x77, 0xab),
    );
    let upper = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x, rect.min.y + rect.height() * 0.5),
    );
    painter.rect_filled(
        upper,
        egui::Rounding { nw: 2.0, ne: 2.0, sw: 0.0, se: 0.0 },
        egui::Color32::from_rgb(0x6b, 0x8d, 0xb5),
    );

    let Some(asset) = title_assets.get(&asset_id) else {
        // Asset went missing — draw a placeholder instead of leaving
        // a blank gradient (which would look like a normal item with
        // no thumbnail and confuse the user).
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "MISSING ASSET",
            egui::FontId::proportional((rect.height() * 0.16).clamp(8.0, 14.0)),
            theme.marker_error,
        );
        return;
    };

    let data = &asset.data;
    // Use the title's own preview text (or the visible text if
    // there's content). Bias to the asset's own data field — this
    // is a Project Asset, the user named it, the displayed text
    // should match the asset.
    let display_text = if data.text().trim().is_empty() {
        "Title".to_string()
    } else {
        // Single-line preview — multi-line titles get squashed at
        // thumbnail size anyway. Strip past the first newline.
        data.text().lines().next().unwrap_or("").to_string()
    };

    let inner = rect.shrink2(egui::vec2(rect.width() * 0.06, rect.height() * 0.10));
    let data_pos = data.position();
    let pos = egui::pos2(
        inner.min.x + data_pos.x * inner.width(),
        inner.min.y + data_pos.y * inner.height(),
    );
    let align = match data.alignment() {
        klipp_timeline::TextAlignment::Left => egui::Align2::LEFT_CENTER,
        klipp_timeline::TextAlignment::Right => egui::Align2::RIGHT_CENTER,
        klipp_timeline::TextAlignment::Center => egui::Align2::CENTER_CENTER,
    };
    // Scale the title's font_size into a card-friendly range. The
    // template stores e.g. 72 px which would dwarf the thumbnail; we
    // map it down by a fixed factor and clamp the result.
    let font_size = (data.font_size() / 7.0).clamp(8.0, 18.0);
    let color = egui::Color32::from_rgba_unmultiplied(
        data.color()[0], data.color()[1], data.color()[2], data.color()[3],
    );
    if let Some(ref shadow) = data.shadow() {
        let sc = egui::Color32::from_rgba_unmultiplied(
            shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3],
        );
        painter.text(
            egui::pos2(pos.x + shadow.offset_x * 0.3, pos.y + shadow.offset_y * 0.3),
            align,
            &display_text,
            egui::FontId::proportional(font_size),
            sc,
        );
    }
    painter.text(
        pos,
        align,
        display_text,
        egui::FontId::proportional(font_size),
        color,
    );
}

/// Count how many timeline clips across all sequences are linked
/// to a given title asset id. Used both for the Media-tab "N×"
/// badge and the delete-confirm prompt.
pub(crate) fn count_linked_instances(state: &AppState, asset_id: MediaId) -> usize {
    let mut n = 0;
    for seq in &state.sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                if clip.linked_title_asset_id == Some(asset_id) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// "Nx" pill anchored to the bottom-right of the thumbnail, paired
/// visually with the TITLE badge in the top-right. Same dark-bg
/// readability treatment so it works against any preview canvas.
fn paint_asset_instance_count(
    painter: &egui::Painter,
    thumb_rect: egui::Rect,
    theme: &KlippTheme,
    count: usize,
) {
    let label = format!("{}\u{00D7}", count);
    let font = egui::FontId::proportional(9.0);
    let galley = painter.layout_no_wrap(label.clone(), font.clone(), theme.text_primary);
    let pad_x = 6.0;
    let pad_y = 2.0;
    let pill_size = egui::vec2(
        galley.size().x + pad_x * 2.0,
        galley.size().y + pad_y * 2.0,
    );
    let pill_pos = egui::pos2(
        thumb_rect.max.x - pill_size.x - 4.0,
        thumb_rect.max.y - pill_size.y - 4.0,
    );
    let pill_rect = egui::Rect::from_min_size(pill_pos, pill_size);
    painter.rect_filled(
        pill_rect,
        2.0,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
    );
    painter.text(
        pill_rect.center(),
        egui::Align2::CENTER_CENTER,
        &label,
        font,
        theme.text_primary,
    );
}

/// TITLE pill in the top-right of the thumbnail. Accent-coloured
/// with strong contrast white-on-amber text so it reads against any
/// thumbnail background.
fn paint_title_asset_badge(painter: &egui::Painter, thumb_rect: egui::Rect, theme: &KlippTheme) {
    let label = "TITLE";
    let font = egui::FontId::proportional(9.0);
    let galley = painter.layout_no_wrap(label.into(), font.clone(), theme.text_on_accent);
    let pad_x = 6.0;
    let pad_y = 2.0;
    let pill_size = egui::vec2(galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0);
    let pill_pos = egui::pos2(
        thumb_rect.max.x - pill_size.x - 4.0,
        thumb_rect.min.y + 4.0,
    );
    let pill_rect = egui::Rect::from_min_size(pill_pos, pill_size);
    painter.rect_filled(pill_rect, 2.0, theme.accent);
    painter.text(
        pill_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        theme.text_on_accent,
    );
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
    creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
) {
    // Inline edit for new bin creation (triggered by "New Folder" context menu item)
    if creating_bin.is_some() {
        let mut should_clear = false;
        if let Some(ref mut edit_text) = creating_bin {
            let resp = ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(Icon::Folder.image(10.0).tint(theme.text_dim));
                let text_resp = ui.add(
                    egui::TextEdit::singleline(edit_text)
                        .desired_width(120.0)
                        .font(egui::FontId::proportional(10.0)),
                );
                if text_resp.lost_focus() {
                    let name = edit_text.trim().to_string();
                    if !name.is_empty() {
                        cmds.push(AppCommand::CreateBin { name });
                    }
                    should_clear = true;
                }
                text_resp.request_focus();
            });
            let _ = resp;
        }
        if should_clear {
            *creating_bin = None;
        }
    }

    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();

        // Folder header
        let header_resp = render_bin_header(ui, theme, bin, renaming_bin, cmds);

        // Bin header context menu (only for named bins)
        if let Some(bin_name) = bin {
            header_resp.context_menu(|ui| {
                if ui.button("New Sub-Folder").clicked() {
                    if let Some(bin_record) = state.media_pool.bins.iter().find(|b| b.name == *bin_name) {
                        cmds.push(AppCommand::CreateSubBin {
                            parent_id: bin_record.id,
                            name: "New Sub-Folder".to_string(),
                        });
                    }
                    ui.close_menu();
                }
                ui.separator();
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
                    egui::Stroke::new(2.0_f32, theme.accent),
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

        // Render order: parents first, each followed immediately by its
        // subclips (in their natural order). Subclips whose parent is not
        // in this bin (rare — bin_id usually inherits) render as orphans
        // at the end. This keeps subclips visually adjacent to their
        // parent without needing expand/collapse UI for MVP.
        let bin_items: Vec<&MediaRow> = {
            let mut parents: Vec<&MediaRow> = Vec::new();
            let mut children_by_parent: std::collections::HashMap<MediaId, Vec<&MediaRow>> =
                std::collections::HashMap::new();
            for it in &bin_items {
                match it.parent_media_id {
                    Some(pid) => children_by_parent.entry(pid).or_default().push(*it),
                    None => parents.push(*it),
                }
            }
            let mut ordered: Vec<&MediaRow> = Vec::with_capacity(parents.len() + children_by_parent.values().map(Vec::len).sum::<usize>());
            for p in &parents {
                ordered.push(p);
                if let Some(children) = children_by_parent.remove(&p.id) {
                    for c in children {
                        ordered.push(c);
                    }
                }
            }
            // Orphan subclips (parent in a different bin / already deleted).
            for (_, leftover) in children_by_parent {
                for c in leftover {
                    ordered.push(c);
                }
            }
            ordered
        };

        for item in &bin_items {
            let is_subclip_row = item.parent_media_id.is_some();
            let indent = if is_subclip_row { 20.0_f32 } else { 0.0 };
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
                theme.bg_hover
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(row_rect, 0.0, bg);

            // Selected: 2px amber left border
            if is_selected {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(row_rect.min, egui::vec2(2.0, row_h)),
                    0.0,
                    theme.accent,
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

            // Thumbnail (80x45), vertically centred in the row. Subclips
            // render at a 20px indent — everything to the right shifts with
            // it so the ↳ prefix on the filename reads naturally.
            let thumb_x = row_rect.min.x + 12.0 + indent;
            let thumb_y = row_rect.min.y + (row_h - 45.0) / 2.0;
            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(thumb_x, thumb_y),
                egui::vec2(80.0, 45.0),
            );
            if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                let src_aspect = tex.size()[0] as f32 / tex.size()[1] as f32;
                let dst_aspect = thumb_rect.width() / thumb_rect.height();
                let uv_rect = if (src_aspect - dst_aspect).abs() < 0.01 {
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                } else if src_aspect > dst_aspect {
                    // Source is wider — crop sides
                    let visible_frac = dst_aspect / src_aspect;
                    let offset = (1.0 - visible_frac) / 2.0;
                    egui::Rect::from_min_max(egui::pos2(offset, 0.0), egui::pos2(1.0 - offset, 1.0))
                } else {
                    // Source is taller — crop top/bottom
                    let visible_frac = src_aspect / dst_aspect;
                    let offset = (1.0 - visible_frac) / 2.0;
                    egui::Rect::from_min_max(egui::pos2(0.0, offset), egui::pos2(1.0, 1.0 - offset))
                };
                ui.painter().image(tex.id(), thumb_rect, uv_rect, egui::Color32::WHITE);
            } else {
                ui.painter().rect_filled(thumb_rect, 3.0, theme.bg_raised);
            }

            // Offline badge: dim the thumbnail and overlay an OFFLINE pill.
            if item.offline {
                paint_offline_badge(ui.painter(), thumb_rect, theme);
            }

            // Usage indicators: source-loaded border + on-timeline dot.
            paint_usage_badges(
                ui.painter(),
                thumb_rect,
                theme.accent,
                state.source_media_id == Some(item.id),
                is_media_used(state, item.id),
                theme,
            );

            // Hover skim (list view: use whole row as skim area)
            handle_hover_skim(ui, state, &row_rect, item, cmds, false);

            // Info: filename (single line, truncated) + metadata
            let text_x = thumb_rect.max.x + 8.0;
            let text_max_w = row_rect.max.x - text_x - 24.0; // leave space for used dot

            // Filename — truncate to fit. Subclips are prefixed with "↳ "
            // so they read as a child of the item above them.
            let raw_name = if is_subclip_row {
                format!("\u{21B3} {}", item.filename)
            } else {
                item.filename.clone()
            };
            let display_name = truncate_to_width(
                &raw_name,
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
            // Metadata line. When the clip is rated, paint the stars in
            // amber first so they pop against the dim grey of the rest of
            // the metadata. `painter().text()` returns the rendered rect,
            // which we use to advance the x cursor for the grey text.
            let meta_y = row_rect.min.y + 30.0;
            let mut meta_x = text_x;
            if item.rating > 0 {
                let stars = "\u{2605}".repeat(item.rating as usize);
                let stars_rect = ui.painter().text(
                    egui::pos2(meta_x, meta_y),
                    egui::Align2::LEFT_TOP,
                    &stars,
                    egui::FontId::monospace(10.0),
                    theme.accent,
                );
                meta_x = stars_rect.max.x;
                let sep_rect = ui.painter().text(
                    egui::pos2(meta_x, meta_y),
                    egui::Align2::LEFT_TOP,
                    " \u{00b7} ",
                    egui::FontId::monospace(10.0),
                    theme.text_dim,
                );
                meta_x = sep_rect.max.x;
            }
            let meta = format!(
                "{} \u{00b7} {}fps \u{00b7} {} \u{00b7} {}",
                dur_str, fps_str, item.codec, size_str
            );
            ui.painter().text(
                egui::pos2(meta_x, meta_y),
                egui::Align2::LEFT_TOP,
                &meta,
                egui::FontId::monospace(10.0),
                theme.text_dim,
            );

            // (Used indicator removed: the red "in use on timeline" bar on
            // the thumbnail conveys this state already.)

            // Tooltip with extended metadata on hover
            resp.clone().on_hover_ui(|ui| {
                ui.label(egui::RichText::new(&item.filename).strong());
                if item.rating > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "Rating: {}",
                            "\u{2605}".repeat(item.rating as usize)
                        ))
                        .color(theme.accent),
                    );
                }
                ui.label(format!("{} x {}", item.resolution.width, item.resolution.height));
                if let Some(ref cam) = item.camera { ui.label(format!("Camera: {cam}")); }
                if let Some(bd) = item.bit_depth { ui.label(format!("Bit depth: {bd}-bit")); }
                if let Some(ref pf) = item.pixel_format { ui.label(format!("Pixel format: {pf}")); }
                if let Some(vbr) = item.video_bitrate_kbps {
                    if vbr >= 1000 { ui.label(format!("Bitrate: {:.1} Mbps", vbr as f64 / 1000.0)); }
                    else { ui.label(format!("Bitrate: {vbr} kbps")); }
                }
            });

            if resp.clicked() {
                let modifiers = ui.input(|i| i.modifiers);
                handle_media_click(
                    item.id,
                    &modifiers,
                    all_ordered_ids,
                    &mut state.selected_media_ids,
                    &mut state.last_clicked_media_id,
                );
                // Clear timeline clip selection so inspector shows bin media (State 2)
                state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if resp.double_clicked() {
                let on_new_tracks = ui.input(|i| i.modifiers.shift);
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id, on_new_tracks });
            }
            if resp.dragged() {
                // Single item, whatever the selection. `dragging_media` is
                // one `Option<MediaId>` and the timeline's drop handler
                // reads only that — it never consults
                // `state.selected_media_ids` — so dragging a multi-selection
                // drops one clip.
                //
                // This used to be an `if selected.contains(id) && len > 1`
                // with identical arms, under a comment claiming "drag all
                // selected items". Nothing implemented it. Left unimplemented
                // rather than guessed at: where several clips land on a drop
                // (end to end on one track? one per track?) is a design
                // decision, not a fix.
                state.dragging_media = Some(item.id);
            }
            resp.context_menu(|ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, item.offline, &item.filename, item.has_zone, item.parent_media_id.is_some(), cmds);
                }
            });
        }
    }
}

// ── Grid View ──

/// Default column count for the grid view. DaVinci-style: column count
/// is a user-chosen constant (with a [-]/[+] picker in a future
/// commit), and cards scale SMOOTHLY with panel width inside that
/// count — drag wider, cards grow; drag narrower, cards shrink, all
/// the way down to the panel's own minimum. Auto-deciding columns
/// from drag width creates transition jumps where widening the panel
/// can shrink cards, which we don't want.
const GRID_DEFAULT_COLS: usize = 2;
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
    creating_bin: &mut Option<String>,
    scroll_to_media_id: &mut Option<MediaId>,
) {
    // Use the clip rect (the actually-visible width) rather than
    // `ui.available_width()`. Inside a vertical `ScrollArea` egui can
    // report a layout width derived from content rather than from the
    // panel — when the panel narrows, content from the previous frame
    // can keep `available_width` artificially wide, leaving cards
    // sized for a panel that no longer exists and hidden behind the
    // splitter. `clip_rect().width()` is the panel's true visible
    // width and shrinks immediately when the user drags the divider.
    let avail_w = (ui.clip_rect().width() - GRID_PADDING * 2.0).max(1.0);
    // Fixed column count — cards just scale linearly with panel width.
    // No fallback to fewer columns, no minimum card size: dragging the
    // panel narrower keeps shrinking the cards proportionally and
    // dragging it wider keeps growing them. The layout never "flips"
    // unexpectedly under the user's hand.
    let cols = GRID_DEFAULT_COLS.max(1);
    let card_w = ((avail_w - (cols as f32 - 1.0) * GRID_GAP) / cols as f32).max(1.0);
    let thumb_h = (card_w / GRID_THUMB_ASPECT).round();
    let card_h = thumb_h + GRID_INFO_H;

    // Inline edit for new bin creation (triggered by "New Folder" context menu item)
    if creating_bin.is_some() {
        let mut should_clear = false;
        if let Some(ref mut edit_text) = creating_bin {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(Icon::Folder.image(10.0).tint(theme.text_dim));
                let text_resp = ui.add(
                    egui::TextEdit::singleline(edit_text)
                        .desired_width(120.0)
                        .font(egui::FontId::proportional(10.0)),
                );
                if text_resp.lost_focus() {
                    let name = edit_text.trim().to_string();
                    if !name.is_empty() {
                        cmds.push(AppCommand::CreateBin { name });
                    }
                    should_clear = true;
                }
                text_resp.request_focus();
            });
        }
        if should_clear {
            *creating_bin = None;
        }
    }

    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();

        // Folder header with rename support
        let header_resp = render_bin_header(ui, theme, bin, renaming_bin, cmds);
        if let Some(bin_name) = bin {
            header_resp.context_menu(|ui| {
                if ui.button("New Sub-Folder").clicked() {
                    if let Some(bin_record) = state.media_pool.bins.iter().find(|b| b.name == *bin_name) {
                        cmds.push(AppCommand::CreateSubBin {
                            parent_id: bin_record.id,
                            name: "New Sub-Folder".to_string(),
                        });
                    }
                    ui.close_menu();
                }
                ui.separator();
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

            // Synthetic project asset (title etc.) — render a styled
            // mini-preview of the title content instead of going down
            // the file-thumbnail path. Uses the same paint primitives
            // as the Library/Titles tab cards so the asset reads
            // exactly like its source template.
            let is_synth = matches!(item.kind, klipp_media::MediaItemKind::SyntheticTitle);
            if is_synth {
                paint_synthetic_title_thumb(
                    ui,
                    theme,
                    thumb_rect,
                    item.id,
                    &state.title_assets,
                );
            } else if let Some(tex) = state.thumbnail_textures.get(&item.id) {
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
                Icon::ArrowClockwise.image(12.0).tint(egui::Color32::from_rgba_premultiplied(theme.text_primary.r(), theme.text_primary.g(), theme.text_primary.b(), 140)).paint_at(ui, spin_rect);
            }

            // Card border (drawn after thumbnail so it's on top)
            let border_color = if is_selected {
                theme.accent
            } else if hovered {
                theme.accent_dim
            } else {
                theme.border
            };
            ui.painter().rect_stroke(card_rect, 4.0, egui::Stroke::new(1.0_f32, border_color));

            // Offline badge: dim the thumbnail and overlay an OFFLINE pill.
            if item.offline {
                paint_offline_badge(ui.painter(), thumb_rect, theme);
            }

            // TITLE badge + instance count for synthetic project
            // assets. The count tells users "this asset is in use" /
            // "still safe to delete" at a glance — Premiere's
            // Master Clips do this with a usage column; we put it
            // right on the card.
            if is_synth {
                paint_title_asset_badge(ui.painter(), thumb_rect, theme);
                let instance_count = count_linked_instances(state, item.id);
                if instance_count > 0 {
                    paint_asset_instance_count(
                        ui.painter(), thumb_rect, theme, instance_count,
                    );
                }
            }

            // Usage indicators: source-loaded border + on-timeline red bar.
            paint_usage_badges(
                ui.painter(),
                thumb_rect,
                theme.accent,
                state.source_media_id == Some(item.id),
                is_media_used(state, item.id),
                theme,
            );

            // (i) info icon with rich hover tooltip — full filename, path,
            // codecs, channels, sample rate, file size, in/out marks.
            // Pulls from the canonical MediaItem in the pool because the
            // grid's `item` is a denormalised `MediaRow` snapshot.
            if let Some(media_item) = state.media_pool.get(&item.id) {
                draw_info_icon_with_tooltip(ui, media_item, thumb_rect, theme);
            }

            // Hover skim (grid view: white scrubber line over thumbnail)
            handle_hover_skim(ui, state, &thumb_rect, item, cmds, true);

            // ── Label strip (attached to thumbnail, DaVinci-style) ──
            let label_strip_h = 18.0_f32;
            let label_strip = egui::Rect::from_min_size(
                egui::pos2(card_pos.x, thumb_rect.max.y),
                egui::vec2(card_w, label_strip_h),
            );
            let [lr, lg, lb] = state.timeline_prefs.clip_label_bg_color;
            let [tr, tg, tb] = state.timeline_prefs.clip_label_text_color;
            ui.painter().rect_filled(label_strip, 0.0, egui::Color32::from_rgb(lr, lg, lb));
            ui.painter().line_segment(
                [label_strip.left_top(), label_strip.right_top()],
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(0, 0, 0, 160)),
            );
            let text_max_w = card_w - 24.0;

            // Filename rendered inside the label strip, vertically centered.
            let display_name = truncate_to_width(
                &item.filename,
                egui::FontId::proportional(11.0),
                text_max_w,
                ui,
            );
            ui.painter().text(
                egui::pos2(card_pos.x + 6.0, label_strip.center().y),
                egui::Align2::LEFT_CENTER,
                &display_name,
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(tr, tg, tb),
            );

            // (Used indicator removed: the red "in use on timeline" bar
            // painted on the thumbnail by `paint_usage_badges` already
            // conveys this state without competing for label-strip space.)

            // Metadata line — stars first (amber) when rated, then dim grey
            // duration · fps.
            let dur_str = format_duration(&item.duration);
            let fps_str = format_fps(&item.fps);
            let meta_y = label_strip.max.y + 2.0;
            let mut meta_x = card_pos.x + 6.0;
            if item.rating > 0 {
                let stars = "\u{2605}".repeat(item.rating as usize);
                let stars_rect = ui.painter().text(
                    egui::pos2(meta_x, meta_y),
                    egui::Align2::LEFT_TOP,
                    &stars,
                    egui::FontId::monospace(9.0),
                    theme.accent,
                );
                meta_x = stars_rect.max.x;
                let sep_rect = ui.painter().text(
                    egui::pos2(meta_x, meta_y),
                    egui::Align2::LEFT_TOP,
                    " \u{00b7} ",
                    egui::FontId::monospace(9.0),
                    theme.text_dim,
                );
                meta_x = sep_rect.max.x;
            }
            let meta_str = format!("{} \u{00b7} {}fps", dur_str, fps_str);
            ui.painter().text(
                egui::pos2(meta_x, meta_y),
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
                    all_ordered_ids,
                    &mut state.selected_media_ids,
                    &mut state.last_clicked_media_id,
                );
                // Clear timeline clip selection so inspector shows bin media (State 2)
                state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if card_resp.double_clicked() {
                let on_new_tracks = ui.input(|i| i.modifiers.shift);
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id, on_new_tracks });
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
                                egui::Color32::from_rgba_premultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 60),
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
                        egui::Color32::from_rgba_premultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 60),
                    );
                }
            }

            card_resp.context_menu(|ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, item.offline, &item.filename, item.has_zone, item.parent_media_id.is_some(), cmds);
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

    // Calculate skim speed (px/sec) for audio mode selection.
    // Sub-pixel cursor movement counts as stationary — snap to 0 so
    // BrowserSkimAudioUpdate's silence path fires immediately on stop
    // (the EMA alone decays too slowly, and a truly still cursor
    // doesn't tick at all because egui isn't re-rendering).
    let now = std::time::Instant::now();
    let dt = now.duration_since(state.skim_last_update).as_secs_f32();
    let dx = (hover_pos.x - state.skim_last_x).abs();
    if dx < 0.5 {
        state.skim_speed_px_per_sec = 0.0;
    } else {
        let raw_speed = if dt > 0.001 { dx / dt } else { 0.0 };
        state.skim_speed_px_per_sec = state.skim_speed_px_per_sec * 0.7 + raw_speed * 0.3;
    }
    state.skim_last_x = hover_pos.x;
    state.skim_last_update = now;
    // Force the silence check to actually run after the cursor stops
    // moving. Without this re-paint nudge, no input → no render →
    // BrowserSkimAudioUpdate never gets dispatched again, and audio
    // plays through a stationary hover.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));

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
        // Layer 3: Request full-resolution frame from skim file. Bin
        // hover targets the Source Viewer.
        if !state.skim_decode_in_flight {
            cmds.push(AppCommand::BrowserSkimFrame {
                media_id: item.id,
                time_secs: skim_time,
                target: klipp_core::ViewerMode::Source,
            });
        } else {
            // Queue latest position so it's picked up when current decode finishes
            state.skim_queued_request = Some((item.id, skim_time, klipp_core::ViewerMode::Source));
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

    // Hover is a transient preview only — it must NOT commit the hovered
    // clip as the loaded source (that's what single-click → LoadSourceMedia
    // does). Previously we wrote `source_media_id` here, which clobbered
    // any committed clip the moment the user's mouse passed over a
    // different bin item; the skim cleanup at the end of the frame
    // wouldn't restore the original. The Source Viewer's render path now
    // pairs `browser_skim_texture` (transient hover) with the committed
    // `source_media_id`, so hovering overlays without overwriting.

    // Draw white scrubber line (grid view)
    if draw_scrubber {
        ui.painter().line_segment(
            [
                egui::Pos2::new(hover_pos.x, skim_rect.min.y),
                egui::Pos2::new(hover_pos.x, skim_rect.max.y),
            ],
            egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
        );
    }

    // Draw position indicator on thumbnail (thin vertical line)
    ui.painter().line_segment(
        [
            egui::Pos2::new(hover_pos.x, skim_rect.min.y),
            egui::Pos2::new(hover_pos.x, skim_rect.max.y),
        ],
        // Skim cursor sits on top of the thumbnail content (any
        // colour) — polarity-neutral white-with-alpha so it reads
        // against bright OR dark video frames in either theme.
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
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
    resolution: klipp_core::Resolution,
    camera: Option<String>,
    bit_depth: Option<u8>,
    pixel_format: Option<String>,
    video_bitrate_kbps: Option<u32>,
    /// Source file no longer exists at `path`. Drives the OFFLINE badge and
    /// the right-click "Relink…" entry.
    offline: bool,
    /// `Some(parent_id)` for subclip rows, `None` for normal items. Drives
    /// indentation in the list and the Create-Subclip gate in the menu.
    parent_media_id: Option<MediaId>,
    /// Cached `has_zone` flag — true when `default_source_in/out` are both
    /// set, for the right-click "Create Subclip from Zone" gate.
    has_zone: bool,
    /// File-backed vs synthetic project asset (title etc). Drives
    /// the special card/row rendering path so synthetic items show
    /// a custom thumbnail + TITLE badge instead of trying to read
    /// from disk.
    kind: klipp_media::MediaItemKind,
}

fn show_context_menu(
    ui: &mut egui::Ui,
    media_id: MediaId,
    path: &std::path::Path,
    offline: bool,
    filename: &str,
    has_zone: bool,
    is_subclip: bool,
    cmds: &mut Vec<AppCommand>,
) {
    // Relink — only shown for offline items, sits at the top so it's
    // the first action a user sees when reaching for a missing clip.
    if offline {
        if ui.button("Relink\u{2026}").clicked() {
            ui.close_menu();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            let mut dialog = rfd::FileDialog::new()
                .set_title(format!("Locate {}", filename));
            if !ext.is_empty() {
                dialog = dialog.add_filter(ext.to_uppercase(), &[ext.as_str()]);
            }
            if let Some(new_path) = dialog.pick_file() {
                cmds.push(AppCommand::RelinkMedia { media_id, new_path });
            }
        }
        ui.separator();
    }

    if ui.button("Rename\u{2026}").clicked() {
        ui.close_menu();
        // Single-item rename reuses the batch dialog (count 1 is supported).
        cmds.push(AppCommand::OpenBatchRenameDialog { media_ids: vec![media_id] });
    }
    // Create Subclip from Zone — only for non-subclips with a zone set.
    // The handler rejects subclip-of-subclip defensively; this gate just
    // avoids offering it when it would toast an error.
    if !is_subclip && has_zone
        && ui.button("Create Subclip from Zone").clicked() {
            ui.close_menu();
            cmds.push(AppCommand::CreateSubclipFromZone {
                source_media_id: media_id,
                name: None,
            });
        }
    ui.separator();
    if ui.button("Load in Source").clicked() {
        cmds.push(AppCommand::LoadSourceMedia { media_id });
        ui.close_menu();
    }
    if ui.button("Add to Timeline").clicked() {
        cmds.push(AppCommand::ImportMediaLinked { media_id, on_new_tracks: false });
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
    if ui.button("Open in External App").clicked() {
        cmds.push(AppCommand::OpenInExternalApp { media_id });
        ui.close_menu();
    }
    if ui.button("Reveal in File Manager").clicked() {
        cmds.push(AppCommand::RevealInFileManager {
            path: path.to_path_buf(),
        });
        ui.close_menu();
    }
    // F-160: bin-side audio sync entries. The by-media variants
    // resolve to the first timeline clip using this media at
    // handler time; if no clip uses it yet they surface a toast.
    ui.separator();
    if ui.button("Set as Audio Reference").clicked() {
        cmds.push(AppCommand::SetAudioReferenceByMedia { media_id });
        ui.close_menu();
    }
    if ui.button("Align to Audio Reference").clicked() {
        cmds.push(AppCommand::AlignToAudioReferenceByMedia { media_id });
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Properties\u{2026}").clicked() {
        cmds.push(AppCommand::OpenClipProperties { media_id });
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
    if ui.button("Rename\u{2026}").clicked() {
        ui.close_menu();
        cmds.push(AppCommand::OpenBatchRenameDialog {
            media_ids: selected_ids.iter().copied().collect(),
        });
    }
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
    // Batch colour label — mirrors the single-item "Label colour"
    // submenu but fans the per-item SetMediaColorLabel out across
    // every selected media id. ColorLabel::ALL keeps the swatch
    // list canonical with the single-item path so an added/removed
    // colour propagates to both surfaces in one place.
    ui.menu_button("Set Color Label", |ui| {
        for label in klipp_media::ColorLabel::ALL {
            let mut text = egui::RichText::new(label.label()).size(11.0);
            if let Some((r, g, b)) = label.rgb() {
                text = text.color(egui::Color32::from_rgb(r, g, b));
            }
            if ui.button(text).clicked() {
                for &id in selected_ids {
                    cmds.push(AppCommand::SetMediaColorLabel { media_id: id, label: label.to_u8() });
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

#[cfg(test)]
mod rating_filter_tests {
    use super::passes_rating_filter;

    #[test]
    fn rating_filter_none_passes_all() {
        assert!(passes_rating_filter(0, None));
        assert!(passes_rating_filter(3, None));
        assert!(passes_rating_filter(5, None));
    }

    #[test]
    fn rating_filter_min_3_excludes_below() {
        assert!(!passes_rating_filter(0, Some(3)));
        assert!(!passes_rating_filter(2, Some(3)));
        assert!(passes_rating_filter(3, Some(3)));
        assert!(passes_rating_filter(4, Some(3)));
        assert!(passes_rating_filter(5, Some(3)));
    }

    #[test]
    fn rating_filter_min_5_only_allows_5() {
        assert!(!passes_rating_filter(0, Some(5)));
        assert!(!passes_rating_filter(3, Some(5)));
        assert!(!passes_rating_filter(4, Some(5)));
        assert!(passes_rating_filter(5, Some(5)));
    }

    #[test]
    fn rating_filter_min_1_excludes_unrated() {
        assert!(!passes_rating_filter(0, Some(1)));
        assert!(passes_rating_filter(1, Some(1)));
        assert!(passes_rating_filter(5, Some(1)));
    }
}

#[cfg(test)]
mod color_filter_tests {
    use super::passes_color_filter;
    use klipp_media::ColorLabel;

    #[test]
    fn color_filter_zero_passes_all() {
        assert!(passes_color_filter(ColorLabel::Red, 0));
        assert!(passes_color_filter(ColorLabel::None, 0));
        assert!(passes_color_filter(ColorLabel::Purple, 0));
    }

    #[test]
    fn color_filter_single_bit_filters_correctly() {
        let filter = 1u8 << ColorLabel::Red.to_u8();
        assert!(passes_color_filter(ColorLabel::Red, filter));
        assert!(!passes_color_filter(ColorLabel::Blue, filter));
        assert!(!passes_color_filter(ColorLabel::None, filter));
    }

    #[test]
    fn color_filter_multi_bit_or_semantics() {
        // Red and Green both active — either matches.
        let filter = (1u8 << ColorLabel::Red.to_u8())
            | (1u8 << ColorLabel::Green.to_u8());
        assert!(passes_color_filter(ColorLabel::Red, filter));
        assert!(passes_color_filter(ColorLabel::Green, filter));
        assert!(!passes_color_filter(ColorLabel::Blue, filter));
        assert!(!passes_color_filter(ColorLabel::None, filter));
    }

    #[test]
    fn color_filter_none_label_included_when_bit_set() {
        // Bit 0 = ColorLabel::None — selecting it filters to unlabelled clips.
        let filter = 1u8 << ColorLabel::None.to_u8();
        assert!(passes_color_filter(ColorLabel::None, filter));
        assert!(!passes_color_filter(ColorLabel::Red, filter));
    }

    #[test]
    fn color_filter_toggle_bit() {
        // XOR toggle is how the dot-button UI manipulates the bitmask.
        let mut filter: u8 = 0;
        let bit = 1u8 << ColorLabel::Red.to_u8();
        filter ^= bit; // toggle on
        assert_eq!(filter & bit, bit);
        filter ^= bit; // toggle off
        assert_eq!(filter & bit, 0);
    }
}

#[cfg(test)]
mod smart_bin_filter_tests {
    use super::passes_smart_bin_filter;
    use crate::browser::SmartBinFilter;
    use klipp_core::{Fps, MediaId, RationalTime};
    use klipp_media::MediaItem;

    /// Build a base MediaItem we can mutate per-case. Synthetic-title
    /// constructor gives us every required field at sane defaults so
    /// each test only has to flip the bits that matter.
    fn item() -> MediaItem {
        MediaItem::new_synthetic_title(
            MediaId::new(),
            "test.mp4".into(),
            RationalTime { value: 240, rate: 24 },
            Fps::film_24(),
        )
    }

    #[test]
    fn smart_bin_none_passes_everything() {
        let m = item();
        assert!(passes_smart_bin_filter(&m, SmartBinFilter::None, 0));
        assert!(passes_smart_bin_filter(&m, SmartBinFilter::None, 5));
    }

    #[test]
    fn smart_bin_offline_only_passes_offline() {
        let mut online = item();
        online.offline = false;
        let mut offline = item();
        offline.offline = true;
        assert!(!passes_smart_bin_filter(&online, SmartBinFilter::Offline, 0));
        assert!(passes_smart_bin_filter(&offline, SmartBinFilter::Offline, 0));
    }

    #[test]
    fn smart_bin_offline_ignores_usage_count() {
        let mut m = item();
        m.offline = true;
        // Even when used, an offline item still passes — usage_count is
        // only consulted by Unused. Otherwise the dropdown would never
        // show offline files that were once placed on the timeline.
        assert!(passes_smart_bin_filter(&m, SmartBinFilter::Offline, 5));
    }

    #[test]
    fn smart_bin_unused_uses_usage_count() {
        let m = item();
        assert!(passes_smart_bin_filter(&m, SmartBinFilter::Unused, 0));
        assert!(!passes_smart_bin_filter(&m, SmartBinFilter::Unused, 1));
    }

    #[test]
    fn smart_bin_video_only_excludes_audio_and_stills() {
        let mut video = item();
        video.video_codec = "h264".into();
        video.is_still = false;
        let mut still = item();
        still.video_codec = "mjpeg".into();
        still.is_still = true;
        let mut audio = item();
        audio.video_codec.clear();
        audio.has_audio = true;
        assert!(passes_smart_bin_filter(&video, SmartBinFilter::VideoOnly, 0));
        assert!(!passes_smart_bin_filter(&still, SmartBinFilter::VideoOnly, 0));
        assert!(!passes_smart_bin_filter(&audio, SmartBinFilter::VideoOnly, 0));
    }
}

#[cfg(test)]
mod metadata_filter_tests {
    use super::{enumerate_metadata_filter_values, format_fps_label, passes_metadata_filter};
    use crate::browser::MetadataFilter;
    use klipp_core::{Fps, MediaId, RationalTime};
    use klipp_media::MediaItem;

    fn item() -> MediaItem {
        MediaItem::new_synthetic_title(
            MediaId::new(),
            "test.mp4".into(),
            RationalTime { value: 240, rate: 24 },
            Fps::film_24(),
        )
    }

    #[test]
    fn metadata_any_passes_everything() {
        let m = item();
        assert!(passes_metadata_filter(&m, &MetadataFilter::Any));
    }

    #[test]
    fn metadata_codec_filters_case_insensitively() {
        let mut m = item();
        m.video_codec = "H264".into();
        // Filter set with the lowercase form — should still match.
        assert!(passes_metadata_filter(&m, &MetadataFilter::Codec("h264".into())));
        assert!(!passes_metadata_filter(&m, &MetadataFilter::Codec("hevc".into())));
    }

    #[test]
    fn metadata_fps_requires_exact_match() {
        let m = item();
        // film_24 = 24/1
        assert!(passes_metadata_filter(&m, &MetadataFilter::Fps { num: 24, den: 1 }));
        // ntsc_24 (23.976) shouldn't match film_24
        assert!(!passes_metadata_filter(&m, &MetadataFilter::Fps { num: 24000, den: 1001 }));
    }

    #[test]
    fn metadata_camera_excludes_items_without_camera_make() {
        let mut with_cam = item();
        with_cam.camera_make = Some("Sony A7S".into());
        let no_cam = item();
        assert!(passes_metadata_filter(&with_cam, &MetadataFilter::Camera("Sony A7S".into())));
        assert!(!passes_metadata_filter(&no_cam, &MetadataFilter::Camera("Sony A7S".into())));
    }

    #[test]
    fn enumerate_returns_distinct_sorted_values() {
        let mut a = item(); a.video_codec = "h264".into();
        let mut b = item(); b.video_codec = "hevc".into();
        let mut c = item(); c.video_codec = "h264".into(); // dup
        let mut d = item(); d.video_codec.clear(); // skipped — empty
        let pool = vec![a, b, c, d];
        let (codecs, _, _) = enumerate_metadata_filter_values(&pool);
        assert_eq!(codecs, vec!["h264".to_string(), "hevc".to_string()]);
    }

    #[test]
    fn enumerate_skips_unknown_fps_1_1() {
        let mut a = item(); a.fps = Fps::film_24();
        let mut b = item(); b.fps = Fps { num: 1, den: 1 }; // fallback / unknown
        let pool = vec![a, b];
        let (_, fps_pairs, _) = enumerate_metadata_filter_values(&pool);
        assert_eq!(fps_pairs, vec![(24, 1)]);
    }

    #[test]
    fn format_fps_label_renders_round_and_fractional_correctly() {
        assert_eq!(format_fps_label(24, 1), "24");
        assert_eq!(format_fps_label(30, 1), "30");
        assert_eq!(format_fps_label(24000, 1001), "23.976");
        assert_eq!(format_fps_label(30000, 1001), "29.970");
        assert_eq!(format_fps_label(60, 0), "—");
    }
}
