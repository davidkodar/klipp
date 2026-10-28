use crate::widgets::menu::{SubmenuRows, MenuRows};
use crate::widgets::menu::{menu_body, menu_choice, menu_item};
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, MediaId};

/// What the pool is sorted by: any sortable [`Field`].
pub type SortBy = klipp_media::Field;
use klipp_media::{Field, FieldValue};

/// Where a list row's text starts: after the 12 px margin and the 80 px
/// thumbnail. The column header lines up with it.
const LIST_TEXT_X: f32 = 12.0 + 80.0 + 8.0;

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
    filter: &super::SmartBinFilter,
    usage_count: usize,
    smart_bins: &[klipp_core::SmartBin],
) -> bool {
    use klipp_media::SmartMatch;
    match filter {
        super::SmartBinFilter::None => true,
        super::SmartBinFilter::Unused => usage_count == 0,
        super::SmartBinFilter::VideoOnly => !item.video_codec.is_empty() && !item.is_still,
        super::SmartBinFilter::AudioOnly => item.has_audio && item.video_codec.is_empty(),
        super::SmartBinFilter::StillsOnly => item.is_still,
        super::SmartBinFilter::Offline => item.offline,
        super::SmartBinFilter::Favourites => item.has_marker(klipp_core::MediaMarkerKind::Favourite),
        super::SmartBinFilter::Rejected => item.has_marker(klipp_core::MediaMarkerKind::Reject),
        super::SmartBinFilter::Saved(id) => smart_bins.iter().find(|b| &b.id == id).is_some_and(|b| b.matches(item)),
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
        MetadataFilter::Lens(l) => item.shot.lens.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(l)),
        MetadataFilter::Resolution(w, h) => item.resolution.width == *w && item.resolution.height == *h,
        MetadataFilter::Reel(r) => item.reel().is_some_and(|m| m.eq_ignore_ascii_case(r)),
        MetadataFilter::Scene(sc) => item.log.scene.eq_ignore_ascii_case(sc),
        MetadataFilter::Keyword(k) => item.log.keywords.iter().any(|m| m.eq_ignore_ascii_case(k)),
        MetadataFilter::GoodTake => item.log.good_take,
    }
}

/// The distinct values the pool holds for each filterable field, for
/// the metadata dropdown: a project with one camera offers one camera.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MetadataValues {
    pub codecs: Vec<String>,
    pub fps: Vec<(u32, u32)>,
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub resolutions: Vec<(u32, u32)>,
    pub reels: Vec<String>,
    pub scenes: Vec<String>,
    pub keywords: Vec<String>,
    pub any_good_take: bool,
}

/// Walk the media pool for the distinct values of every filterable
/// field, each list sorted. Empty values are skipped: a clip with no
/// recorded camera does not put a blank in the camera list.
pub fn enumerate_metadata_filter_values(items: &[klipp_media::MediaItem]) -> MetadataValues {
    use std::collections::BTreeSet;
    let mut codecs: BTreeSet<String> = BTreeSet::new();
    let mut fps_set: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut cameras: BTreeSet<String> = BTreeSet::new();
    let mut lenses: BTreeSet<String> = BTreeSet::new();
    let mut resolutions: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut reels: BTreeSet<String> = BTreeSet::new();
    let mut scenes: BTreeSet<String> = BTreeSet::new();
    let mut keywords: BTreeSet<String> = BTreeSet::new();
    let mut any_good_take = false;
    for item in items {
        if !item.video_codec.is_empty() {
            codecs.insert(item.video_codec.clone());
        }
        // Skip 1/1 (fallback / unknown) — not a meaningful filter
        // value and would otherwise pollute the dropdown.
        if !(item.fps.num == 1 && item.fps.den == 1) {
            fps_set.insert((item.fps.num, item.fps.den));
        }
        if let Some(make) = item.camera_make.as_deref().filter(|m| !m.is_empty()) {
            cameras.insert(make.to_string());
        }
        if let Some(lens) = item.shot.lens.as_deref().filter(|l| !l.is_empty()) {
            lenses.insert(lens.to_string());
        }
        if item.resolution.width > 0 && item.resolution.height > 0 {
            resolutions.insert((item.resolution.width, item.resolution.height));
        }
        if let Some(reel) = item.reel().filter(|r| !r.is_empty()) {
            reels.insert(reel.to_string());
        }
        if !item.log.scene.is_empty() {
            scenes.insert(item.log.scene.clone());
        }
        for k in &item.log.keywords {
            keywords.insert(k.clone());
        }
        any_good_take |= item.log.good_take;
    }
    let mut resolutions: Vec<(u32, u32)> = resolutions.into_iter().collect();
    resolutions.sort_by_key(|(w, h)| std::cmp::Reverse(w * h));
    MetadataValues {
        codecs: codecs.into_iter().collect(),
        fps: fps_set.into_iter().collect(),
        cameras: cameras.into_iter().collect(),
        lenses: lenses.into_iter().collect(),
        resolutions,
        reels: reels.into_iter().collect(),
        scenes: scenes.into_iter().collect(),
        keywords: keywords.into_iter().collect(),
        any_good_take,
    }
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
    columns: &mut Vec<Field>,
    renaming_bin: &mut Option<(String, String)>,
    scroll_to_media_id: &mut Option<MediaId>,
    smart_filter: &super::SmartBinFilter,
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
    let smart_bins = state.media_pool.smart_bins.clone();
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
            if !passes_smart_bin_filter(m, smart_filter, usage, &smart_bins) {
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
                    || m.tags.iter().any(|t| t.to_lowercase().contains(&search_lower))
                    || m.log.matches(&search_lower)
                    || m.camera_model.as_deref().is_some_and(|c| c.to_lowercase().contains(&search_lower))
                    || m.shot.lens.as_deref().is_some_and(|l| l.to_lowercase().contains(&search_lower))
                    || m.screen_text.to_lowercase().contains(&search_lower);
            }
            true
        })
        .map(|m| MediaRow {
            caption: card_caption(m),
            id: m.id,
            filename: m.filename.clone(),
            duration: m.duration,
            _resolution: m.resolution,
            rating: m.rating,
            _proxy_ready: m.proxy_ready,
            path: m.path.clone(),
            bin: m.bin.clone(),
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
            columns: columns.iter().map(|c| c.text(m)).collect(),
            synced_audio: m.synced_audio.is_some(),
            span_parts: m.span.map(|sp| {
                let mut parts: Vec<(u32, MediaId)> = state.media_pool.all().filter_map(|o| o.span.filter(|os| os.first == sp.first).map(|os| (os.index, o.id))).collect();
                parts.sort_by_key(|(i, _)| *i);
                parts.into_iter().map(|(_, id)| id).collect()
            }).unwrap_or_default(),
            display_name: match m.span {
                Some(sp) => format!("{} \u{00B7} {}", m.filename, sp.label()),
                None => m.filename.clone(),
            },
        })
        .collect();

    // Sort by the chosen field's key; stars descending by default, as a
    // rating sort is read.
    let keys: std::collections::HashMap<MediaId, klipp_media::SortKey> =
        state.media_pool.all().map(|m| (m.id, sort_by.sort_key(m))).collect();
    let missing = klipp_media::SortKey::Text(String::new());
    items.sort_by(|a, b| keys.get(&a.id).unwrap_or(&missing).cmp(keys.get(&b.id).unwrap_or(&missing)));
    if *sort_by == SortBy::Rating {
        items.reverse();
    }
    if !*sort_ascending {
        items.reverse();
    }

    // Bins first, by name, then the clips in none (#2621). An empty bin
    // shows while nothing narrows the pool: it was made to be filled.
    let narrowed = items.len() < state.media_pool.all().count();
    let mut bins_seen: Vec<Option<String>> = bin_names(&state.media_pool)
        .into_iter()
        .filter(|name| !narrowed || items.iter().any(|i| i.bin.as_deref() == Some(name.as_str())))
        .map(Some)
        .collect();
    if items.iter().any(|i| i.bin.is_none()) {
        bins_seen.push(None);
    }

    // Ordered list of all visible IDs (used for Shift+click range selection)
    let all_ordered_ids: Vec<MediaId> = items.iter().map(|i| i.id).collect();

    // Ctrl+A: select all visible items — unless a text field has the
    // keyboard, where Ctrl+A selects its text. The browser draws before
    // the inspector, so consuming the key here would steal it from the
    // title text field.
    if !ui.ctx().wants_keyboard_input()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
    {
        state.selected_media_ids = all_ordered_ids.iter().copied().collect();
    }

    // Render
    egui::ScrollArea::vertical().id_salt("browser_media_scroll").auto_shrink([false, false]).show(ui, |ui| {
        // The pool's own menu wherever a right-click lands on no item: a
        // backdrop under the whole visible list, the items drawn over it.
        // Only the space below the last item had it, and a full pool has
        // none (#2621).
        let backdrop = ui.interact(ui.clip_rect(), ui.id().with("media_pool_backdrop"), egui::Sense::click());
        if backdrop.clicked() {
            state.selected_media_ids.clear();
        }
        backdrop.context_menu_rows(|ui| menu_body(ui, |ui| pool_menu(ui, view_mode, sort_by, sort_ascending, &mut cmds)));
        // The tour's first step points here (#2933).
        crate::welcome::anchor(ui.ctx(), "media_pool", ui.clip_rect());
        if bins_seen.is_empty() {
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
                    render_list_view(ui, state, theme, &bins_seen, &items, &all_ordered_ids, &mut cmds, renaming_bin, scroll_to_media_id, columns, sort_by, sort_ascending);
                }
                BrowserViewMode::Grid => {
                    render_grid_view(ui, state, theme, &bins_seen, &items, &all_ordered_ids, &mut cmds, renaming_bin, scroll_to_media_id);
                }
            }
        }

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

/// The media pool's menu, on its backdrop: New Bin, import, and how the
/// pool is shown.
fn pool_menu(ui: &mut egui::Ui, view_mode: &mut BrowserViewMode, sort_by: &mut SortBy, sort_ascending: &mut bool, cmds: &mut Vec<AppCommand>) {
    ui.set_min_width(180.0);
    if menu_item(ui, "New Bin", "").clicked() {
        cmds.push(AppCommand::NewBin { media_ids: Vec::new() });
        ui.close_menu();
    }
    ui.separator();
    if menu_item(ui, "Import Media\u{2026}", "").clicked() {
        crate::file_dialog::spawn(crate::command_palette::import_media_commands);
        ui.close_menu();
    }
    if menu_item(ui, "Import Folder\u{2026}", "").clicked() {
        crate::file_dialog::spawn(move || {
            rfd::FileDialog::new()
                .pick_folder()
                .map(|dir| vec![AppCommand::ImportMediaFolder { directory: dir }])
                .unwrap_or_default()
        });
        ui.close_menu();
    }
    ui.separator();
    ui.menu_rows("Sort By", |ui| menu_body(ui, |ui| {
        for f in Field::SORTABLE {
            ui.selectable_value(sort_by, f, f.label());
        }
        ui.separator();
        // The direction is a choice in here, not an icon row that sat
        // indented among the menu's text (#2621).
        for (ascending, label) in [(true, "Ascending"), (false, "Descending")] {
            if menu_choice(ui, label, "", *sort_ascending == ascending).clicked() {
                *sort_ascending = ascending;
                ui.close_menu();
            }
        }
    }));
    ui.menu_rows("View", |ui| menu_body(ui, |ui| {
        if ui.selectable_value(view_mode, BrowserViewMode::List, "List").clicked() {
            ui.close_menu();
        }
        if ui.selectable_value(view_mode, BrowserViewMode::Grid, "Grid").clicked() {
            ui.close_menu();
        }
    }));
}

/// Whether any selected item is in a bin: Remove from Bin applies.
fn any_in_a_bin(state: &AppState) -> bool {
    state.selected_media_ids.iter().any(|id| state.media_pool.get(id).is_some_and(|m| m.bin.is_some()))
}

/// Every bin's name, by name: the pool's bins and any an item names.
fn bin_names(pool: &klipp_media::MediaPool) -> Vec<String> {
    let mut names: Vec<String> = pool.bins.iter().filter(|b| b.parent_id.is_none()).map(|b| b.name.clone()).collect();
    names.extend(pool.all().filter_map(|m| m.bin.clone()));
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// A grid card's caption, as the list's columns read it: a clip's
/// length and frame rate ("0:13 · 24 fps"), a recording's length and
/// sample rate ("0:13 · 48 kHz"), a still's size ("Still · 1920×1080").
/// No rate is shown that the file does not have.
fn card_caption(item: &klipp_media::MediaItem) -> String {
    use klipp_media::FieldValue;
    if item.is_still {
        return if item.resolution.width > 0 { format!("Still \u{00b7} {}\u{00d7}{}", item.resolution.width, item.resolution.height) } else { "Still".to_string() };
    }
    let length = klipp_media::field::duration_text(item.duration.to_secs_f64());
    if klipp_media::field::has_frame_rate(item) {
        format!("{length} \u{00b7} {} fps", klipp_media::Field::Fps.text(item))
    } else if let Some(rate) = item.sample_rate.filter(|r| *r > 0) {
        let khz = rate as f64 / 1000.0;
        let khz = if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz:.1}") };
        format!("{length} \u{00b7} {khz} kHz")
    } else {
        length
    }
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
            .rounding(4.0)
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
    if klipp_media::field::has_frame_rate(item) {
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
            .map(|t| klipp_core::TimecodeDisplay::plain(item.fps).position(t))
            .unwrap_or_else(|| "\u{2014}".to_string());
        let out_str = item
            .default_source_out
            .map(|t| klipp_core::TimecodeDisplay::plain(item.fps).position(t))
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
        klipp_timeline::TextAlignment::Center | klipp_timeline::TextAlignment::Justify => egui::Align2::CENTER_CENTER,
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
    let font = egui::FontId::proportional(10.0);
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
    let font = egui::FontId::proportional(10.0);
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

/// The column titles over the list, lined up with the rows' columns. A
/// click sorts by that column, a second click flips the order, and a
/// right-click chooses which columns the list shows.
fn render_column_header(ui: &mut egui::Ui, theme: &KlippTheme, columns: &mut Vec<Field>, sort_by: &mut SortBy, sort_ascending: &mut bool) {
    // Two lines, as a row has them: the name, then the columns under it.
    let line_h = crate::theme::control::COMPACT;
    let width = (ui.clip_rect().right() - ui.cursor().min.x).max(1.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, line_h * 2.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme.bg_panel_alt);
    let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut clicked_col: Option<Field> = None;
    let headings = std::iter::once((Field::Name, rect.min.x + LIST_TEXT_X, rect.right() - rect.min.x - LIST_TEXT_X, rect.min.y))
        .chain(columns.iter().scan(rect.min.x + LIST_TEXT_X, |x, col| {
            let at = *x;
            *x += col.column_width();
            Some((*col, at, col.column_width(), rect.min.y + line_h))
        }));
    for (col, x, w, top) in headings {
        // A column shows whole or not at all: one cut to the panel left
        // a heading of a letter or two (#2621). The name line, the
        // width of the panel, is shortened instead.
        let w = if col == Field::Name { w.min(rect.right() - x) } else { w };
        if x + w > rect.right() + 0.5 {
            break;
        }
        let cy = top + line_h / 2.0;
        let col_rect = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(w, line_h));
        let under = pointer.is_some_and(|p| col_rect.contains(p));
        let sorted = *sort_by == col;
        let colour = if sorted { theme.accent } else if under && resp.hovered() { theme.text_primary } else { theme.text_secondary };
        let title = if sorted { format!("{} {}", col.label(), if *sort_ascending { "\u{25B4}" } else { "\u{25BE}" }) } else { col.label().to_string() };
        painter.text(egui::pos2(x + 2.0, cy), egui::Align2::LEFT_CENTER, truncate_to_width(&title, font.clone(), w - 6.0, ui), font.clone(), colour);
        if resp.clicked() && under {
            clicked_col = Some(col);
        }
    }
    if let Some(col) = clicked_col {
        if *sort_by == col {
            *sort_ascending = !*sort_ascending;
        } else {
            *sort_by = col;
            *sort_ascending = true;
        }
    }
    resp.context_menu_rows(|ui| menu_body(ui, |ui| {
        ui.label(egui::RichText::new("Columns").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        for f in Field::COLUMNS {
            let mut on = columns.contains(&f);
            if ui.checkbox(&mut on, f.label()).changed() {
                if on {
                    columns.push(f);
                } else {
                    columns.retain(|c| *c != f);
                }
            }
        }
        ui.separator();
        if menu_item(ui, "Default columns", "").clicked() {
            *columns = Field::DEFAULT_COLUMNS.to_vec();
            ui.close_menu();
        }
    }));
    painter.line_segment([egui::pos2(rect.min.x, rect.max.y), egui::pos2(rect.max.x, rect.max.y)], egui::Stroke::new(1.0_f32, theme.border));
}

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
    scroll_to_media_id: &mut Option<MediaId>,
    columns: &mut Vec<Field>,
    sort_by: &mut SortBy,
    sort_ascending: &mut bool,
) {
    render_column_header(ui, theme, columns, sort_by, sort_ascending);
    let bin_list = bin_names(&state.media_pool);
    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();
        if !bin_section(ui, state, theme, bin, bins_seen, bin_items.len(), renaming_bin, cmds) {
            continue;
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

            // Selected: a 2 px accent bar on the left edge
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
                paint_thumbnail(ui, tex, thumb_rect, crate::theme::radius::CONTROL, theme);
            } else {
                ui.painter().rect_filled(thumb_rect, crate::theme::radius::CONTROL, theme.bg_raised);
            }

            // Offline badge: dim the thumbnail and overlay an OFFLINE pill.
            if item.offline {
                paint_offline_badge(ui.painter(), thumb_rect, theme);
            }

            // Usage indicators: source-loaded border + on-timeline bar.
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
            let text_max_w = row_rect.max.x - text_x - 8.0;
            let name_font = egui::FontId::proportional(crate::theme::type_scale::CONTROL);

            // Filename — truncate to fit. Subclips are prefixed with "↳ "
            // so they read as a child of the item above them.
            let raw_name = if is_subclip_row {
                format!("\u{21B3} {}", item.display_name)
            } else {
                item.display_name.clone()
            };
            let display_name = truncate_to_width(&raw_name, name_font.clone(), text_max_w, ui);
            ui.painter().text(
                egui::pos2(text_x, row_rect.min.y + 11.0),
                egui::Align2::LEFT_TOP,
                &display_name,
                name_font,
                theme.text_primary,
            );

            // The columns, each in its width from where the header put it,
            // clipped to the row.
            let row_painter = ui.painter_at(row_rect);
            let meta_y = row_rect.min.y + 30.0;
            let meta_font = egui::FontId::monospace(crate::theme::type_scale::CAPTION);
            let mut cx = row_rect.min.x + LIST_TEXT_X;
            for (col, text) in columns.iter().zip(&item.columns) {
                // As in the header: a column shows whole or not at all.
                let w = col.column_width();
                if cx + w > row_rect.right() + 0.5 {
                    break;
                }
                if !text.is_empty() {
                    let colour = if *col == Field::Rating { theme.text_secondary } else { theme.text_dim };
                    row_painter.text(
                        egui::pos2(cx + 2.0, meta_y),
                        egui::Align2::LEFT_TOP,
                        truncate_to_width(text, meta_font.clone(), w - 6.0, ui),
                        meta_font.clone(),
                        colour,
                    );
                }
                cx += w;
            }

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
            // A double-click opens the item in the source viewer and
            // nothing more: putting it on the timeline is an Insert,
            // Overwrite or Append the editor asks for (#2663).
            if resp.double_clicked() {
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if resp.dragged() {
                // The item, or the multi-selection it is part of, in
                // browser order: the timeline lays a group end to end.
                state.dragging_media_group = crate::drag_drop::drag_group(all_ordered_ids, &state.selected_media_ids, item.id);
                state.dragging_media = Some(item.id);
            }
            resp.context_menu_rows(|ui| menu_body(ui, |ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, sync_pair_of(state), state.multicam_default_sync, &bin_list, any_in_a_bin(state), cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, item.offline, &item.filename, item.has_zone, item.parent_media_id.is_some(), item.synced_audio, &item.span_parts, &bin_list, item.bin.as_deref(), cmds);
                }
            }));
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
const GRID_INFO_H: f32 = 36.0; // name row + caption row below the thumbnail

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

    let bin_list = bin_names(&state.media_pool);
    for bin in bins_seen {
        let bin_items: Vec<_> = items.iter().filter(|i| &i.bin == bin).collect();
        if !bin_section(ui, state, theme, bin, bins_seen, bin_items.len(), renaming_bin, cmds) {
            continue;
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
            ui.painter().rect_filled(card_rect, crate::theme::radius::PANEL, theme.bg_panel_alt);

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
                paint_thumbnail(ui, tex, thumb_rect, 0.0, theme);
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
            let (border_w, border_color) = if is_selected {
                (1.5_f32, theme.accent)
            } else if hovered {
                (1.0_f32, theme.border_light)
            } else {
                (1.0_f32, theme.border)
            };
            ui.painter().rect_stroke(card_rect, crate::theme::radius::PANEL, egui::Stroke::new(border_w, border_color));

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

            // ── Name row and caption row on the card's own field ──
            let name_font = egui::FontId::proportional(crate::theme::type_scale::BODY);
            let text_max_w = card_w - 12.0;
            let name_y = thumb_rect.max.y + 4.0;
            let display_name = truncate_to_width(&item.filename, name_font.clone(), text_max_w, ui);
            ui.painter().text(
                egui::pos2(card_pos.x + 6.0, name_y),
                egui::Align2::LEFT_TOP,
                &display_name,
                name_font,
                theme.text_primary,
            );

            // Caption: stars first when rated, then what the item is.
            let meta_str = item.caption.clone();
            let meta_y = name_y + crate::theme::type_scale::BODY + 5.0;
            let mut meta_x = card_pos.x + 6.0;
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
            ui.painter().text(
                egui::pos2(meta_x, meta_y),
                egui::Align2::LEFT_TOP,
                &meta_str,
                egui::FontId::monospace(10.0),
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
            // A double-click opens the item in the source viewer and
            // nothing more: putting it on the timeline is an Insert,
            // Overwrite or Append the editor asks for (#2663).
            if card_resp.double_clicked() {
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
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
                        let from_thumb = thumb_rect.contains(start_pos);
                        // A drag that starts on the thumbnail picks a range
                        // only while it stays level with it (running past
                        // either end selects to that end). One that leaves
                        // it above or below is taking the clip somewhere,
                        // the timeline: it drags the clip, and drops the
                        // half-made range so the clip's In is not left
                        // wherever the drag happened to start.
                        let leaving_thumb = from_thumb && !thumb_rect.y_range().contains(current_pos.y);
                        if leaving_thumb
                            && state.dragging_media.is_none()
                            && state.browser_range_selection.as_ref().is_some_and(|r| r.media_id == item.id)
                        {
                            state.browser_range_selection = None;
                        }
                        if dx > 8.0 && from_thumb && !leaving_thumb && state.dragging_media.is_none() {
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
                        } else if dx > 8.0 || leaving_thumb {
                            state.dragging_media_group = crate::drag_drop::drag_group(all_ordered_ids, &state.selected_media_ids, item.id);
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

            card_resp.context_menu_rows(|ui| menu_body(ui, |ui| {
                if state.selected_media_ids.len() > 1 && state.selected_media_ids.contains(&item.id) {
                    show_multi_context_menu(ui, &state.selected_media_ids, sync_pair_of(state), state.multicam_default_sync, &bin_list, any_in_a_bin(state), cmds);
                } else {
                    show_context_menu(ui, item.id, &item.path, item.offline, &item.filename, item.has_zone, item.parent_media_id.is_some(), item.synced_audio, &item.span_parts, &bin_list, item.bin.as_deref(), cmds);
                }
            }));

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

/// A bin's header, its menu and its drop target, or the divider before
/// the clips in no bin: whether the section's items are shown. A bin's
/// header opens and closes it (#2621).
#[allow(clippy::too_many_arguments)]
fn bin_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    bin: &Option<String>,
    bins_seen: &[Option<String>],
    count: usize,
    renaming_bin: &mut Option<(String, String)>,
    cmds: &mut Vec<AppCommand>,
) -> bool {
    let Some(name) = bin else {
        // The clips in no bin, set apart from the bins above them.
        if bins_seen.iter().any(Option::is_some) {
            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.set_min_height(crate::theme::control::COMPACT);
                ui.add_space(10.0);
                ui.label(egui::RichText::new("NOT IN A BIN").size(crate::theme::type_scale::CAPTION).strong().color(theme.text_dim));
            });
        }
        return true;
    };
    let closed_id = egui::Id::new(("media_bin_closed", name.as_str()));
    let closed = ui.data(|d| d.get_temp::<bool>(closed_id)).unwrap_or(false);
    let Some(resp) = bin_header(ui, theme, name, count, closed, renaming_bin, cmds) else {
        return !closed;
    };
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(closed_id, !closed));
    }
    resp.context_menu_rows(|ui| menu_body(ui, |ui| {
        if menu_item(ui, "New Bin", "").clicked() {
            cmds.push(AppCommand::NewBin { media_ids: Vec::new() });
            ui.close_menu();
        }
        ui.separator();
        if menu_item(ui, "Rename Bin", "").clicked() {
            *renaming_bin = Some((name.clone(), name.clone()));
            ui.close_menu();
        }
        if menu_item(ui, "Delete Bin", "").on_hover_text("Its clips stay in the pool, in no bin.").clicked() {
            cmds.push(AppCommand::DeleteBin { name: name.clone() });
            ui.close_menu();
        }
    }));
    // Items dragged onto the header go into the bin.
    if let Some(dragging) = state.dragging_media {
        if resp.contains_pointer() {
            ui.painter().rect_stroke(resp.rect, 2.0, egui::Stroke::new(2.0_f32, theme.accent));
            if !ui.ctx().input(|i| i.pointer.primary_down()) {
                let media_ids = if state.dragging_media_group.is_empty() { vec![dragging] } else { state.dragging_media_group.clone() };
                cmds.push(AppCommand::MoveItemsToBin { media_ids, bin: Some(name.clone()) });
                state.dragging_media = None;
                state.dragging_media_group.clear();
            }
        }
    }
    if !closed && count == 0 {
        ui.horizontal(|ui| {
            ui.add_space(30.0);
            ui.label(egui::RichText::new("Empty: drag clips here, or use Move to Bin").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
        });
    }
    !closed
}

/// A bin's header row: open or closed, its name and how many items it
/// shows. While the bin is being renamed, the name's text field instead,
/// and no response.
fn bin_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    name: &str,
    count: usize,
    closed: bool,
    renaming_bin: &mut Option<(String, String)>,
    cmds: &mut Vec<AppCommand>,
) -> Option<egui::Response> {
    if let Some((old_name, edit_text)) = renaming_bin {
        if old_name == name {
            let old_name = old_name.clone();
            let mut done = false;
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(Icon::Folder.image(12.0).tint(theme.text_dim));
                let id = ui.id().with(("bin_name_field", old_name.as_str()));
                let opening = !ui.memory(|m| m.has_focus(id));
                let text_resp = ui.add(
                    egui::TextEdit::singleline(edit_text)
                        .id(id)
                        .desired_width(140.0)
                        .font(egui::FontId::proportional(crate::theme::type_scale::CONTROL)),
                );
                // The whole name selected as the field opens, so typing
                // replaces it (#2621).
                if opening {
                    let mut field = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                    let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(edit_text.chars().count()));
                    field.cursor.set_char_range(Some(all));
                    field.store(ui.ctx(), id);
                }
                // Commit on lost focus (Enter, click away, Escape)
                if text_resp.lost_focus() {
                    let new_name = edit_text.trim().to_string();
                    if !new_name.is_empty() && new_name != old_name {
                        cmds.push(AppCommand::RenameBin { old_name, new_name });
                    }
                    done = true;
                }
                text_resp.request_focus();
            });
            if done {
                *renaming_bin = None;
            }
            return None;
        }
    }
    ui.add_space(2.0);
    let width = (ui.clip_rect().right() - ui.cursor().min.x).max(1.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, crate::theme::control::COMPACT + 4.0), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect.shrink2(egui::vec2(4.0, 0.0)), 3.0, if resp.hovered() { theme.bg_hover } else { theme.bg_raised });
    let icon = |x: f32| egui::Rect::from_center_size(egui::pos2(rect.left() + x, rect.center().y), egui::vec2(12.0, 12.0));
    (if closed { Icon::CaretRight } else { Icon::CaretDown }).image(12.0).tint(theme.text_secondary).paint_at(ui, icon(16.0));
    Icon::Folder.image(12.0).tint(theme.text_secondary).paint_at(ui, icon(32.0));
    let font = egui::FontId::proportional(crate::theme::type_scale::CONTROL);
    painter.text(egui::pos2(rect.left() + 44.0, rect.center().y), egui::Align2::LEFT_CENTER, name, font.clone(), theme.text_primary);
    painter.text(egui::pos2(rect.right() - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), font, theme.text_dim);
    Some(resp.on_hover_text(if closed { "Show the bin's clips" } else { "Hide the bin's clips" }))
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
    _resolution: klipp_core::Resolution,
    rating: u8,
    _proxy_ready: bool,
    path: std::path::PathBuf,
    bin: Option<String>,
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
    /// The list's columns as text, in the order the list shows them.
    columns: Vec<String>,
    /// A separate recording is paired with the clip.
    synced_audio: bool,
    /// The parts of the split take this clip belongs to, in order, when
    /// it is one.
    span_parts: Vec<MediaId>,
    /// The name with its part, "C0001.MP4 · part 1 of 3", for display.
    display_name: String,
    /// The grid card's caption (`card_caption`).
    caption: String,
}

fn show_context_menu(
    ui: &mut egui::Ui,
    media_id: MediaId,
    path: &std::path::Path,
    offline: bool,
    filename: &str,
    has_zone: bool,
    is_subclip: bool,
    synced_audio: bool,
    span_parts: &[MediaId],
    bins: &[String],
    bin: Option<&str>,
    cmds: &mut Vec<AppCommand>,
) {
    if span_parts.len() >= 2 && menu_item(ui, &format!("Add All {} Parts to Timeline", span_parts.len()), "")
        .on_hover_text("The take the camera split into numbered files, laid back to back.")
        .clicked()
    {
        for id in span_parts {
            cmds.push(AppCommand::ImportMediaLinked { media_id: *id, on_new_tracks: false });
        }
        ui.close_menu();
    }
    if synced_audio && menu_item(ui, "Remove Synced Audio", "").on_hover_text("The clip goes back to its own sound.").clicked() {
        cmds.push(AppCommand::ClearSyncedAudio { video_id: media_id });
        ui.close_menu();
    }
    if !offline && menu_item(ui, "Transcribe Clip", "")
        .on_hover_text("Run speech-to-text on this clip now, even when automatic transcription is off.")
        .clicked()
    {
        cmds.push(AppCommand::TranscribeClip { media_id });
        ui.close_menu();
    }
    if !offline && menu_item(ui, "Make Shorts\u{2026}", "")
        .on_hover_text("Find the moments that stand on their own as vertical shorts, review them, and build each as a sequence of its own.")
        .clicked()
    {
        cmds.push(AppCommand::OpenShortsDialog { media_id });
        ui.close_menu();
    }
    // Relink — only shown for offline items, sits at the top so it's
    // the first action a user sees when reaching for a missing clip.
    if offline {
        if menu_item(ui, "Relink\u{2026}", "").clicked() {
            ui.close_menu();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            let title = format!("Locate {}", filename);
            crate::file_dialog::spawn(move || {
                let mut dialog = rfd::FileDialog::new().set_title(title);
                if !ext.is_empty() {
                    dialog = dialog.add_filter(ext.to_uppercase(), &[ext.as_str()]);
                }
                dialog.pick_file()
                    .map(|new_path| vec![AppCommand::RelinkMediaByHand { media_id, new_path }])
                    .unwrap_or_default()
            });
        }
        ui.separator();
    }

    if menu_item(ui, "Rename\u{2026}", "").clicked() {
        ui.close_menu();
        // Single-item rename reuses the batch dialog (count 1 is supported).
        cmds.push(AppCommand::OpenBatchRenameDialog { media_ids: vec![media_id] });
    }
    // Create Subclip from Zone — only for non-subclips with a zone set.
    // The handler rejects subclip-of-subclip defensively; this gate just
    // avoids offering it when it would toast an error.
    if !is_subclip && has_zone
        && menu_item(ui, "Create Subclip from Zone", "").clicked() {
            ui.close_menu();
            cmds.push(AppCommand::CreateSubclipFromZone {
                source_media_id: media_id,
                name: None,
            });
        }
    ui.separator();
    if menu_item(ui, "Load in Source", "").clicked() {
        cmds.push(AppCommand::LoadSourceMedia { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Add at Playhead", "").clicked() {
        cmds.push(AppCommand::PlaceMedia { media_id, drop: klipp_core::events::ClipDrop::AtPlayhead });
        ui.close_menu();
    }
    if menu_item(ui, "Add to End of Timeline", "").clicked() {
        cmds.push(AppCommand::ImportMediaLinked { media_id, on_new_tracks: false });
        ui.close_menu();
    }
    if menu_item(ui, "Generate Proxy", "").clicked() {
        cmds.push(AppCommand::GenerateProxy { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Regenerate Filmstrip", "").clicked() {
        cmds.push(AppCommand::RegenerateFilmstrip { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Regenerate Skim Transcode", "").clicked() {
        cmds.push(AppCommand::RegenerateSkimTranscode { media_id });
        ui.close_menu();
    }
    if ui.add_enabled(!offline, egui::Button::new("Refresh Metadata"))
        .on_hover_text("Read the file again for what the camera recorded: timecode, reel, exposure, a sidecar copied beside it since, every tag.")
        .clicked()
    {
        cmds.push(AppCommand::RefreshMediaMetadata { media_ids: vec![media_id] });
        ui.close_menu();
    }
    ui.separator();
    ui.menu_rows("Set Rating", |ui| menu_body(ui, |ui| {
        for r in 0..=5u8 {
            let label = if r == 0 {
                "None".to_string()
            } else {
                "\u{2605}".repeat(r as usize)
            };
            if menu_item(ui, &label, "").clicked() {
                cmds.push(AppCommand::SetMediaRating { media_id, rating: r });
                ui.close_menu();
            }
        }
    }));
    ui.menu_rows("Label colour", |ui| menu_body(ui, |ui| {
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
    }));
    ui.separator();
    bin_menu_items(ui, bins, &[media_id], bin, bin.is_some(), cmds);
    ui.separator();
    if menu_item(ui, "Remove from Project", "").clicked() {
        cmds.push(AppCommand::RemoveFromPool { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Open in External App", "").clicked() {
        cmds.push(AppCommand::OpenInExternalApp { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Reveal in File Manager", "").clicked() {
        cmds.push(AppCommand::RevealInFileManager {
            path: path.to_path_buf(),
        });
        ui.close_menu();
    }
    if menu_item(ui, "Add to Shared Library", "")
        .on_hover_text("Keep this file in the Library's shared collection, to bring into any project. The file stays where it is.")
        .clicked()
    {
        cmds.push(AppCommand::AddMediaToSharedLibrary { media_ids: vec![media_id], collection: String::new() });
        ui.close_menu();
    }
    // F-160: bin-side audio sync entries. The by-media variants
    // resolve to the first timeline clip using this media at
    // handler time; if no clip uses it yet they surface a toast.
    ui.separator();
    if menu_item(ui, "Set as Audio Reference", "").clicked() {
        cmds.push(AppCommand::SetAudioReferenceByMedia { media_id });
        ui.close_menu();
    }
    if menu_item(ui, "Align to Audio Reference", "").clicked() {
        cmds.push(AppCommand::AlignToAudioReferenceByMedia { media_id });
        ui.close_menu();
    }
    ui.separator();
    if menu_item(ui, "Properties\u{2026}", "").clicked() {
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

/// The video and audio item of a two-item selection that could be
/// synced: one clip with a picture, one recording without.
fn sync_pair_of(state: &AppState) -> Option<(MediaId, MediaId)> {
    if state.selected_media_ids.len() != 2 {
        return None;
    }
    let items: Vec<&klipp_media::MediaItem> = state.selected_media_ids.iter().filter_map(|id| state.media_pool.get(id)).collect();
    let [a, b] = items[..] else { return None };
    let is_video = |m: &klipp_media::MediaItem| !m.video_codec.is_empty() && !m.is_still;
    let is_audio = |m: &klipp_media::MediaItem| m.has_audio && m.video_codec.is_empty();
    if is_video(a) && is_audio(b) {
        Some((a.id, b.id))
    } else if is_video(b) && is_audio(a) {
        Some((b.id, a.id))
    } else {
        None
    }
}

use klipp_core::MulticamSync;

/// Move to Bin, with every bin and a new one, and New Bin from
/// Selection, for the items `ids`: `current` is the bin they are in, and
/// Remove from Bin shows when any is in one.
fn bin_menu_items(ui: &mut egui::Ui, bins: &[String], ids: &[MediaId], current: Option<&str>, in_a_bin: bool, cmds: &mut Vec<AppCommand>) {
    ui.menu_rows("Move to Bin", |ui| menu_body(ui, |ui| {
        for bin in bins {
            if ui.add_enabled_ui(current != Some(bin.as_str()), |ui| menu_item(ui, bin, "")).inner.clicked() {
                cmds.push(AppCommand::MoveItemsToBin { media_ids: ids.to_vec(), bin: Some(bin.clone()) });
                ui.close_menu();
            }
        }
        if !bins.is_empty() {
            ui.separator();
        }
        if menu_item(ui, "New Bin\u{2026}", "").on_hover_text("A new bin with these in it. Its name is ready to type.").clicked() {
            cmds.push(AppCommand::NewBin { media_ids: ids.to_vec() });
            ui.close_menu();
        }
        if in_a_bin && menu_item(ui, "Remove from Bin", "").clicked() {
            cmds.push(AppCommand::MoveItemsToBin { media_ids: ids.to_vec(), bin: None });
            ui.close_menu();
        }
    }));
    if menu_item(ui, "New Bin from Selection", "").on_hover_text("A new bin with the selected clips in it.").clicked() {
        cmds.push(AppCommand::NewBin { media_ids: ids.to_vec() });
        ui.close_menu();
    }
}

fn show_multi_context_menu(
    ui: &mut egui::Ui,
    selected_ids: &std::collections::HashSet<MediaId>,
    sync_pair: Option<(MediaId, MediaId)>,
    default_sync: MulticamSync,
    bins: &[String],
    in_a_bin: bool,
    cmds: &mut Vec<AppCommand>,
) {
    let count = selected_ids.len();
    ui.label(egui::RichText::new(format!("{count} items selected")).strong());
    ui.separator();
    if menu_item(ui, "Batch Rename\u{2026}", "").on_hover_text("Rename these items by one pattern.").clicked() {
        ui.close_menu();
        cmds.push(AppCommand::OpenBatchRenameDialog {
            media_ids: selected_ids.iter().copied().collect(),
        });
    }
    if menu_item(ui, "Refresh Metadata", "").on_hover_text("Read these files again for what the camera recorded.").clicked() {
        ui.close_menu();
        cmds.push(AppCommand::RefreshMediaMetadata { media_ids: selected_ids.iter().copied().collect() });
    }
    if menu_item(ui, "Add to Shared Library", "")
        .on_hover_text("Keep these files in the Library's shared collection, to bring into any project. The files stay where they are.")
        .clicked()
    {
        ui.close_menu();
        cmds.push(AppCommand::AddMediaToSharedLibrary { media_ids: selected_ids.iter().copied().collect(), collection: String::new() });
    }
    let ids = || selected_ids.iter().copied().collect::<Vec<_>>();
    // In one click, by the editing preferences' default sync method;
    // the other methods are one level down.
    if menu_item(ui, "Create Synced Multicam", "")
        .on_hover_text(format!("Put these clips on one clock and lay them out at the playhead as a multicam, one angle per track: {} (the default in Preferences > Editing).", default_sync.label().to_lowercase()))
        .clicked()
    {
        ui.close_menu();
        cmds.push(AppCommand::CreateSyncedMulticam { media_ids: ids(), method: default_sync });
    }
    ui.menu_rows("Create Synced Multicam By", |ui| menu_body(ui, |ui| {
        for (method, hint) in [
            (MulticamSync::TimecodeThenAudio, "Timecode where a clip has it, the sound for the rest. The usual choice."),
            (MulticamSync::Timecode, "Only the start timecode. For cameras jam-synced to one clock."),
            (MulticamSync::Audio, "Only the sound, matched against the longest clip. For cameras with no shared timecode."),
        ] {
            if menu_item(ui, method.label(), "").on_hover_text(hint).clicked() {
                ui.close_menu();
                cmds.push(AppCommand::CreateSyncedMulticam { media_ids: ids(), method });
            }
        }
    }))
    .response
    .on_hover_text("The same, by a method chosen here.");
    if let Some((video_id, audio_id)) = sync_pair {
        if menu_item(ui, "Sync Video and Audio", "")
            .on_hover_text("Pair the clip with this recording: by timecode when both carry one, by the sound otherwise. The recording then goes under the clip when it is added to the timeline.")
            .clicked()
        {
            ui.close_menu();
            cmds.push(AppCommand::SyncMediaPair { video_id, audio_id });
        }
    }
    ui.separator();
    bin_menu_items(ui, bins, &selected_ids.iter().copied().collect::<Vec<_>>(), None, in_a_bin, cmds);
    ui.menu_rows("Set Rating", |ui| menu_body(ui, |ui| {
        for r in 0..=5u8 {
            let label = if r == 0 {
                "None".to_string()
            } else {
                "\u{2605}".repeat(r as usize)
            };
            if menu_item(ui, &label, "").clicked() {
                for &id in selected_ids {
                    cmds.push(AppCommand::SetMediaRating { media_id: id, rating: r });
                }
                ui.close_menu();
            }
        }
    }));
    // Batch colour label — mirrors the single-item "Label colour"
    // submenu but fans the per-item SetMediaColorLabel out across
    // every selected media id. ColorLabel::ALL keeps the swatch
    // list canonical with the single-item path so an added/removed
    // colour propagates to both surfaces in one place.
    ui.menu_rows("Set Colour Label", |ui| menu_body(ui, |ui| {
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
    }));
    ui.separator();
    if menu_item(ui, &format!("Generate Proxies ({count})"), "").clicked() {
        for &id in selected_ids {
            cmds.push(AppCommand::GenerateProxy { media_id: id });
        }
        ui.close_menu();
    }
    if menu_item(ui, &format!("Regenerate Filmstrips ({count})"), "").clicked() {
        for &id in selected_ids {
            cmds.push(AppCommand::RegenerateFilmstrip { media_id: id });
        }
        ui.close_menu();
    }
    ui.separator();
    if menu_item(ui, &format!("Remove {count} items from Project"), "").clicked() {
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
        assert!(passes_smart_bin_filter(&m, &SmartBinFilter::None, 0, &[]));
        assert!(passes_smart_bin_filter(&m, &SmartBinFilter::None, 5, &[]));
    }

    #[test]
    fn smart_bin_offline_only_passes_offline() {
        let mut online = item();
        online.offline = false;
        let mut offline = item();
        offline.offline = true;
        assert!(!passes_smart_bin_filter(&online, &SmartBinFilter::Offline, 0, &[]));
        assert!(passes_smart_bin_filter(&offline, &SmartBinFilter::Offline, 0, &[]));
    }

    #[test]
    fn smart_bin_offline_ignores_usage_count() {
        let mut m = item();
        m.offline = true;
        // Even when used, an offline item still passes — usage_count is
        // only consulted by Unused. Otherwise the dropdown would never
        // show offline files that were once placed on the timeline.
        assert!(passes_smart_bin_filter(&m, &SmartBinFilter::Offline, 5, &[]));
    }

    #[test]
    fn smart_bin_unused_uses_usage_count() {
        let m = item();
        assert!(passes_smart_bin_filter(&m, &SmartBinFilter::Unused, 0, &[]));
        assert!(!passes_smart_bin_filter(&m, &SmartBinFilter::Unused, 1, &[]));
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
        assert!(passes_smart_bin_filter(&video, &SmartBinFilter::VideoOnly, 0, &[]));
        assert!(!passes_smart_bin_filter(&still, &SmartBinFilter::VideoOnly, 0, &[]));
        assert!(!passes_smart_bin_filter(&audio, &SmartBinFilter::VideoOnly, 0, &[]));
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
        let codecs = enumerate_metadata_filter_values(&pool).codecs;
        assert_eq!(codecs, vec!["h264".to_string(), "hevc".to_string()]);
    }

    #[test]
    fn enumerate_skips_unknown_fps_1_1() {
        let mut a = item(); a.fps = Fps::film_24();
        let mut b = item(); b.fps = Fps { num: 1, den: 1 }; // fallback / unknown
        let pool = vec![a, b];
        let fps_pairs = enumerate_metadata_filter_values(&pool).fps;
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

/// A clip's thumbnail in a tile: a wider picture fills it and loses its
/// sides, a taller one (a portrait phone clip) is shown whole between
/// bars, so the pool shows which clips are vertical, as FILES does.
/// Cropped to the tile's width, a 9:16 clip showed only its middle band
/// and read as landscape.
fn paint_thumbnail(ui: &egui::Ui, tex: &egui::TextureHandle, rect: egui::Rect, rounding: f32, theme: &KlippTheme) {
    let size = tex.size_vec2();
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let (dst, uv) = thumbnail_placement(size, rect);
    if dst != rect {
        ui.painter().rect_filled(rect, rounding, theme.bg_void);
    }
    ui.painter().image(tex.id(), dst, uv, egui::Color32::WHITE);
}

/// Where a `size` picture goes in `rect`, and which part of it: see
/// [`paint_thumbnail`].
fn thumbnail_placement(size: egui::Vec2, rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let src = size.x / size.y;
    let dst = rect.width() / rect.height();
    if (src - dst).abs() < 0.01 {
        (rect, full)
    } else if src > dst {
        let offset = (1.0 - dst / src) / 2.0;
        (rect, egui::Rect::from_min_max(egui::pos2(offset, 0.0), egui::pos2(1.0 - offset, 1.0)))
    } else {
        (egui::Rect::from_center_size(rect.center(), egui::vec2(rect.height() * src, rect.height())), full)
    }
}

#[cfg(test)]
mod thumbnail_placement_tests {
    use super::thumbnail_placement;

    #[test]
    fn a_portrait_clip_is_shown_whole_and_a_wide_one_fills_the_tile() {
        let tile = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(160.0, 90.0));
        let (dst, uv) = thumbnail_placement(egui::vec2(480.0, 853.3), tile);
        assert_eq!(uv, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), "all of it");
        assert!((dst.height() - 90.0).abs() < 0.01 && (dst.width() - 90.0 * 480.0 / 853.3).abs() < 0.01, "{dst:?}");
        assert!((dst.center().x - 80.0).abs() < 0.01, "centred");
        let (dst, uv) = thumbnail_placement(egui::vec2(2.39, 1.0), tile);
        assert_eq!(dst, tile);
        assert!(uv.min.x > 0.0 && uv.max.x < 1.0 && uv.min.y == 0.0);
    }
}

#[cfg(test)]
mod default_multicam_sync_tests {
    use super::*;

    /// The command Create Synced Multicam sends, clicked for real, with
    /// `default_sync` the preferences' method.
    fn clicked(default_sync: MulticamSync) -> Vec<AppCommand> {
        let ids: std::collections::HashSet<MediaId> = [MediaId::new(), MediaId::new()].into_iter().collect();
        let ctx = egui::Context::default();
        let run = |events: Vec<egui::Event>| {
            let mut cmds = Vec::new();
            let mut at = None;
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 800.0))), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show_multi_context_menu(ui, &ids, None, default_sync, &[], false, &mut cmds));
            });
            for s in &out.shapes {
                if let egui::Shape::Text(t) = &s.shape {
                    if t.galley.text() == "Create Synced Multicam" {
                        at = Some(t.pos + t.galley.size() / 2.0);
                    }
                }
            }
            (cmds, at)
        };
        let at = run(Vec::new()).1.expect("the item is drawn");
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = run(vec![egui::Event::PointerMoved(at), press(true)]).0;
        cmds.extend(run(vec![press(false)]).0);
        cmds
    }

    /// #1816: Create Synced Multicam syncs by the preferences' default
    /// method: Sound gives Audio, Timecode gives Timecode.
    #[test]
    fn one_click_syncs_by_the_default_method() {
        for method in [MulticamSync::Audio, MulticamSync::Timecode] {
            let cmds = clicked(method);
            assert!(matches!(cmds.as_slice(), [AppCommand::CreateSyncedMulticam { method: m, media_ids }] if *m == method && media_ids.len() == 2), "{cmds:?}");
        }
        assert_eq!(crate::prefs::MulticamSyncMethod::default().sync(), MulticamSync::TimecodeThenAudio, "the default is the usual choice");
    }
}

#[cfg(test)]
mod pool_style_tests {
    use super::*;

    fn item(name: &str, video: &str, audio: &str) -> klipp_media::MediaItem {
        let mut m = klipp_media::MediaItem::synthetic_for_test(format!("/footage/{name}").into(), None);
        m.video_codec = video.into();
        m.audio_codec = audio.into();
        m.has_audio = !audio.is_empty();
        m.fps = klipp_core::Fps::new(24, 1);
        m.duration = klipp_core::RationalTime::from_secs_f64(12.5, 24);
        m
    }

    /// A media item's menu is the shared menu and its items do what they
    /// did: a card right-clicked, then Load in Source clicked for real,
    /// loads that item into the source viewer; the list's rows the same.
    #[test]
    fn a_media_items_menu_loads_it_in_source() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut state = AppState::default();
            let m = item("A001_C002.mov", "h264", "aac");
            let id = state.media_pool.add(m);
            let mut panel = crate::browser::BrowserPanel { browser_view_mode: view, ..Default::default() };
            let ctx = egui::Context::default();
            let theme = KlippTheme::default();
            let mut frame = |events: Vec<egui::Event>| {
                let mut tab = crate::layout_state::BrowserTab::Media;
                let (mut pinned, mut ratio) = (false, 0.5);
                let mut cmds = Vec::new();
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0))), events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| cmds = panel.show(ui, &mut state, &theme, &mut tab, &mut pinned, &mut ratio));
                });
                let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                }).collect();
                (texts, cmds)
            };
            frame(Vec::new());
            let (texts, _) = frame(Vec::new());
            let at = texts.iter().find(|(t, _)| t == "A001_C002.mov").unwrap_or_else(|| panic!("{view:?}: {texts:?}")).1;
            let secondary = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
            frame(vec![egui::Event::PointerMoved(at), secondary(true)]);
            frame(vec![secondary(false)]);
            let (texts, _) = frame(Vec::new());
            let load = texts.iter().find(|(t, _)| t == "Load in Source").unwrap_or_else(|| panic!("{view:?} menu: {texts:?}")).1;
            let primary = |pressed| egui::Event::PointerButton { pos: load, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = frame(vec![egui::Event::PointerMoved(load), primary(true)]).1;
            cmds.extend(frame(vec![primary(false)]).1);
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::LoadSourceMedia { media_id } if *media_id == id)), "{view:?}: {cmds:?}");
        }
    }

    /// A double-click on an item, card or row, opens it in the source
    /// viewer and puts nothing on the timeline (#2663).
    #[test]
    fn a_double_click_only_opens_the_item_in_source() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut state = AppState::default();
            let id = state.media_pool.add(item("A001_C002.mov", "h264", "aac"));
            let mut panel = crate::browser::BrowserPanel { browser_view_mode: view, ..Default::default() };
            let ctx = egui::Context::default();
            let theme = KlippTheme::default();
            let mut time = 0.0;
            let mut frame = |events: Vec<egui::Event>| {
                let mut tab = crate::layout_state::BrowserTab::Media;
                let (mut pinned, mut ratio) = (false, 0.5);
                let mut cmds = Vec::new();
                time += 0.05;
                let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0))), time: Some(time), events, ..Default::default() }, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| cmds = panel.show(ui, &mut state, &theme, &mut tab, &mut pinned, &mut ratio));
                });
                let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                }).collect();
                (texts, cmds)
            };
            frame(Vec::new());
            let (texts, _) = frame(Vec::new());
            let at = texts.iter().find(|(t, _)| t == "A001_C002.mov").unwrap_or_else(|| panic!("{view:?}: {texts:?}")).1;
            let primary = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = frame(vec![egui::Event::PointerMoved(at)]).1;
            for _ in 0..2 {
                cmds.extend(frame(vec![primary(true)]).1);
                cmds.extend(frame(vec![primary(false)]).1);
            }
            cmds.extend(frame(Vec::new()).1);
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::LoadSourceMedia { media_id } if *media_id == id)), "{view:?}: {cmds:?}");
            assert!(
                !cmds.iter().any(|c| matches!(c, AppCommand::ImportMediaLinked { .. } | AppCommand::AppendClipToTrack { .. } | AppCommand::PlaceMedia { .. })),
                "{view:?}: a double-click put it on the timeline: {cmds:?}"
            );
        }
    }

    /// A card's caption reads as the list's columns do, and only rates
    /// the file has: a clip its frame rate, a recording its sample rate
    /// (no "24fps" on a WAV), a still its size.
    #[test]
    fn a_cards_caption_is_what_the_file_is() {
        assert_eq!(card_caption(&item("a.mov", "h264", "aac")), "0:13 \u{00b7} 24 fps");
        let mut wav = item("b.wav", "", "pcm_s24le");
        wav.sample_rate = Some(48_000);
        assert_eq!(card_caption(&wav), "0:13 \u{00b7} 48 kHz");
        wav.sample_rate = Some(44_100);
        assert_eq!(card_caption(&wav), "0:13 \u{00b7} 44.1 kHz");
        wav.sample_rate = None;
        assert_eq!(card_caption(&wav), "0:13");
        let mut still = item("c.png", "png", "");
        still.is_still = true;
        still.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        assert_eq!(card_caption(&still), "Still \u{00b7} 1920\u{00d7}1080");
    }
}

#[cfg(test)]
mod bin_menu_tests {
    use super::*;

    /// The whole media pool, drawn as the app draws it.
    struct Pool {
        ctx: egui::Context,
        state: AppState,
        panel: crate::browser::BrowserPanel,
        /// The window's size.
        size: egui::Vec2,
        /// The last frame's text, with where it was drawn.
        drawn: Vec<(String, egui::Rect)>,
        /// The last frame's filled rectangles.
        fills: Vec<egui::Rect>,
        /// The last frame's shapes.
        shapes: Vec<egui::epaint::ClippedShape>,
    }

    impl Pool {
        fn new(view: BrowserViewMode, names: &[&str]) -> Self {
            let mut state = AppState::default();
            for name in names {
                state.media_pool.add(klipp_media::MediaItem::synthetic_for_test(format!("/footage/{name}").into(), None));
            }
            let panel = crate::browser::BrowserPanel { browser_view_mode: view, ..Default::default() };
            Self { ctx: egui::Context::default(), state, panel, size: egui::vec2(700.0, 1600.0), drawn: Vec::new(), fills: Vec::new(), shapes: Vec::new() }
        }

        /// One frame with `events`: the text drawn, with where, and what
        /// the pool asked the app to do.
        fn frame(&mut self, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
            let theme = KlippTheme::default();
            let mut tab = crate::layout_state::BrowserTab::Media;
            let (mut pinned, mut ratio) = (false, 0.5);
            let mut cmds = Vec::new();
            let (state, panel) = (&mut self.state, &mut self.panel);
            let out = self.ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| cmds = panel.show(ui, state, &theme, &mut tab, &mut pinned, &mut ratio));
            });
            self.shapes = out.shapes.clone();
            self.fills = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.fill.a() > 0 => Some(r.rect),
                _ => None,
            }).collect();
            self.drawn = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size()))),
                _ => None,
            }).collect();
            (self.drawn.iter().map(|(t, r)| (t.clone(), r.center())).collect(), cmds)
        }

        fn texts(&mut self) -> Vec<(String, egui::Pos2)> {
            self.frame(Vec::new()).0
        }

        /// Where `text` is drawn: on top, in an open menu, when it is
        /// drawn twice.
        fn at(&mut self, text: &str) -> egui::Pos2 {
            let texts = self.texts();
            texts.iter().rfind(|(t, _)| t == text).unwrap_or_else(|| panic!("{text:?} is drawn: {texts:?}")).1
        }

        /// Right-click at `at`: the text drawn with the menu open.
        fn right_click(&mut self, at: egui::Pos2) -> Vec<String> {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]);
            self.texts().into_iter().map(|(t, _)| t).collect()
        }

        /// Where the last frame drew `text`, the topmost when twice.
        fn rect(&self, text: &str) -> egui::Rect {
            self.drawn.iter().rfind(|(t, _)| t == text).unwrap_or_else(|| panic!("{text:?} drawn: {:?}", self.drawn)).1
        }

        fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
        }

        /// Click `text` where it is drawn: what the pool asked for.
        fn click(&mut self, text: &str) -> Vec<AppCommand> {
            let at = self.at(text);
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = self.frame(vec![egui::Event::PointerMoved(at), button(true)]).1;
            cmds.extend(self.frame(vec![button(false)]).1);
            cmds
        }
    }

    fn in_bin(pool: &mut Pool, name: &str, bin: &str) -> MediaId {
        let id = pool.state.media_pool.all().find(|m| m.filename == name).expect("the item").id;
        pool.state.media_pool.find_or_create_bin(bin);
        pool.state.media_pool.get_mut(&id).expect("the item").bin = Some(bin.into());
        id
    }

    fn missing<'a>(menu: &[String], items: &[&'a str]) -> Vec<&'a str> {
        items.iter().copied().filter(|item| !menu.iter().any(|t| t == item)).collect()
    }

    /// #2621: a right-click where no item is opens the pool's menu with
    /// New Bin: below the items, and on the divider before the clips in
    /// no bin, in both views.
    #[test]
    fn a_right_click_on_no_item_opens_the_pools_menu() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut pool = Pool::new(view, &["a.mov", "b.mov"]);
            in_bin(&mut pool, "a.mov", "Selects");
            let below = pool.at("b.mov") + egui::vec2(0.0, 300.0);
            let menu = pool.right_click(below);
            assert!(missing(&menu, &["New Bin", "Import Media\u{2026}"]).is_empty(), "{view:?} below: {menu:?}");
            pool.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
            let divider = pool.at("NOT IN A BIN");
            let menu = pool.right_click(divider);
            assert!(menu.iter().any(|t| t == "New Bin"), "{view:?} divider: {menu:?}");
            // A pool too long for its panel, with no space below its items.
            let names: Vec<String> = (0..60).map(|n| format!("clip{n:02}.mov")).collect();
            let mut pool = Pool::new(view, &names.iter().map(String::as_str).collect::<Vec<_>>());
            in_bin(&mut pool, "clip00.mov", "Selects");
            pool.size = egui::vec2(700.0, 700.0);
            let divider = pool.at("NOT IN A BIN");
            let menu = pool.right_click(divider);
            assert!(menu.iter().any(|t| t == "New Bin"), "{view:?} full pool: {menu:?}");
        }
    }

    /// Bins come first, each with a header, the empty ones too, then a
    /// divider and the clips in no bin; a header's click closes the bin
    /// and opens it again.
    #[test]
    fn bins_are_listed_first_and_close() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut pool = Pool::new(view, &["a.mov", "z.mov"]);
            in_bin(&mut pool, "z.mov", "Selects");
            pool.state.media_pool.find_or_create_bin("Empty");
            let texts = pool.texts();
            let y = |t: &str| texts.iter().find(|(s, _)| s == t).unwrap_or_else(|| panic!("{view:?}: {t:?} in {texts:?}")).1.y;
            assert!(y("Empty") < y("Selects") && y("Selects") < y("z.mov") && y("z.mov") < y("NOT IN A BIN") && y("NOT IN A BIN") < y("a.mov"), "{view:?}: {texts:?}");
            let header = pool.at("Selects");
            let button = |pressed| egui::Event::PointerButton { pos: header, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            pool.frame(vec![egui::Event::PointerMoved(header), button(true)]);
            pool.frame(vec![button(false)]);
            assert!(!pool.texts().iter().any(|(t, _)| t == "z.mov"), "{view:?}: closed");
            pool.frame(vec![egui::Event::PointerMoved(header), button(true)]);
            pool.frame(vec![button(false)]);
            assert!(pool.texts().iter().any(|(t, _)| t == "z.mov"), "{view:?}: open again");
        }
    }

    /// A bin header's menu has New Bin, Rename Bin and Delete Bin.
    #[test]
    fn a_bin_headers_menu_has_its_items() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut pool = Pool::new(view, &["a.mov"]);
            in_bin(&mut pool, "a.mov", "Selects");
            let header = pool.at("Selects");
            let menu = pool.right_click(header);
            let lacking = missing(&menu, &["New Bin", "Rename Bin", "Delete Bin"]);
            assert!(lacking.is_empty(), "{view:?}: lacks {lacking:?} in {menu:?}");
        }
    }

    /// A clip's Move to Bin lists every bin, the one it is in greyed, and
    /// New Bin…; New Bin from Selection is beside it.
    #[test]
    fn move_to_bin_lists_every_bin() {
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut pool = Pool::new(view, &["a.mov", "b.mov"]);
            in_bin(&mut pool, "b.mov", "Selects");
            pool.state.media_pool.find_or_create_bin("B-roll");
            let at = pool.at("a.mov");
            let menu = pool.right_click(at);
            assert!(missing(&menu, &["Move to Bin", "New Bin from Selection"]).is_empty(), "{view:?}: {menu:?}");
            let sub = pool.at("Move to Bin");
            pool.frame(vec![egui::Event::PointerMoved(sub)]);
            pool.frame(vec![egui::Event::PointerMoved(sub)]);
            let menu: Vec<String> = pool.texts().into_iter().map(|(t, _)| t).collect();
            let lacking = missing(&menu, &["B-roll", "Selects", "New Bin\u{2026}"]);
            assert!(lacking.is_empty(), "{view:?}: Move to Bin lacks {lacking:?} in {menu:?}");
            let cmds = pool.click("Selects");
            let a = pool.state.media_pool.all().find(|m| m.filename == "a.mov").expect("a").id;
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::MoveItemsToBin { media_ids, bin: Some(b) } if *media_ids == [a] && b == "Selects")), "{view:?}: {cmds:?}");
        }
    }

    /// New Bin from Selection, on a multi-selection's menu, makes a bin
    /// of every selected item.
    #[test]
    fn new_bin_from_selection_takes_every_selected_item() {
        let mut pool = Pool::new(BrowserViewMode::List, &["a.mov", "b.mov", "c.mov"]);
        let ids: Vec<MediaId> = pool.state.media_pool.all().filter(|m| m.filename != "c.mov").map(|m| m.id).collect();
        pool.state.selected_media_ids = ids.iter().copied().collect();
        let at = pool.at("a.mov");
        let menu = pool.right_click(at);
        assert!(menu.iter().any(|t| t == "2 items selected"), "the selection's menu: {menu:?}");
        let cmds = pool.click("New Bin from Selection");
        let made: Vec<_> = cmds.iter().filter_map(|c| match c { AppCommand::NewBin { media_ids } => Some(media_ids.clone()), _ => None }).collect();
        assert_eq!(made.len(), 1, "{cmds:?}");
        let mut got = made[0].clone();
        got.sort_by_key(|id| id.to_string());
        let mut want = ids;
        want.sort_by_key(|id| id.to_string());
        assert_eq!(got, want);
    }

    /// Several items moved to a bin are one undo step; undoing it takes
    /// them all back out.
    #[test]
    fn moving_several_items_is_one_step() {
        let mut state = AppState::default();
        let ids: Vec<MediaId> = ["a.mov", "b.mov"].iter().map(|n| state.media_pool.add(klipp_media::MediaItem::synthetic_for_test(format!("/footage/{n}").into(), None))).collect();
        state.move_media_to_bin(&ids, Some("Selects"));
        assert!(ids.iter().all(|id| state.media_pool.get(id).expect("item").bin.as_deref() == Some("Selects")));
        let step = state.project_history_mut().undo().expect("a step");
        assert!(state.project_history_mut().undo().is_none(), "one step for both");
        let mut pool = state.media_pool.clone();
        let (mut sequences, mut fps) = (state.sequences.clone(), state.fps);
        klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&step.command);
        assert!(ids.iter().all(|id| pool.get(id).expect("item").bin.is_none()), "undone, both are out");
    }

    /// Renaming a bin, as New Bin starts it: the name is selected and
    /// has the keys, so what is typed replaces it.
    #[test]
    fn typing_a_new_bins_name_replaces_it() {
        let mut pool = Pool::new(BrowserViewMode::List, &["a.mov"]);
        in_bin(&mut pool, "a.mov", "New Bin");
        pool.panel.renaming_bin = Some(("New Bin".into(), "New Bin".into()));
        pool.texts();
        pool.texts();
        assert!(pool.ctx.wants_keyboard_input(), "the name has the keys at once");
        pool.frame(vec![egui::Event::Text("Interviews".into())]);
        assert_eq!(pool.panel.renaming_bin.as_ref().map(|(_, t)| t.as_str()), Some("Interviews"));
    }

    /// After Ctrl+A, a click where no item is clears the selection.
    #[test]
    fn a_click_on_no_item_clears_the_selection() {
        let mut pool = Pool::new(BrowserViewMode::Grid, &["a.mov", "b.mov"]);
        pool.texts();
        pool.key(egui::Key::A, egui::Modifiers::COMMAND);
        assert_eq!(pool.state.selected_media_ids.len(), 2);
        let below = pool.at("b.mov") + egui::vec2(0.0, 300.0);
        let button = |pressed| egui::Event::PointerButton { pos: below, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        pool.frame(vec![egui::Event::PointerMoved(below), button(true)]);
        pool.frame(vec![button(false)]);
        assert!(pool.state.selected_media_ids.is_empty());
    }

    /// A selection's Move to Bin offers Remove from Bin only when one of
    /// them is in a bin, and its rename is the batch rename.
    #[test]
    fn a_selections_menu_offers_only_what_applies() {
        for in_one in [false, true] {
            let mut pool = Pool::new(BrowserViewMode::List, &["a.mov", "b.mov"]);
            pool.state.media_pool.find_or_create_bin("Selects");
            if in_one {
                in_bin(&mut pool, "b.mov", "Selects");
            }
            pool.state.selected_media_ids = pool.state.media_pool.all().map(|m| m.id).collect();
            let at = pool.at("a.mov");
            let menu = pool.right_click(at);
            assert!(menu.iter().any(|t| t == "Batch Rename\u{2026}") && !menu.iter().any(|t| t == "Rename\u{2026}"), "{menu:?}");
            let sub = pool.at("Move to Bin");
            pool.frame(vec![egui::Event::PointerMoved(sub)]);
            pool.frame(vec![egui::Event::PointerMoved(sub)]);
            let menu: Vec<String> = pool.texts().into_iter().map(|(t, _)| t).collect();
            assert_eq!(menu.iter().any(|t| t == "Remove from Bin"), in_one, "{menu:?}");
        }
    }

    /// In the pool's menu a row with a submenu starts where a plain row
    /// does.
    #[test]
    fn submenu_rows_line_up_with_plain_rows() {
        let mut pool = Pool::new(BrowserViewMode::Grid, &["a.mov"]);
        let below = pool.at("a.mov") + egui::vec2(0.0, 300.0);
        pool.right_click(below);
        let (plain, sub) = (pool.rect("Import Media\u{2026}").left(), pool.rect("Sort By").left());
        assert!((plain - sub).abs() < 0.5, "Import Media at {plain}, Sort By at {sub}");
    }

    /// The pool's filter row and its buttons are inset from the panel's
    /// edge as its items are.
    #[test]
    fn the_pools_toolbar_rows_are_inset() {
        let mut pool = Pool::new(BrowserViewMode::Grid, &["a.mov"]);
        pool.texts();
        pool.texts();
        for text in ["All media", "New Bin"] {
            assert!(pool.rect(text).left() >= 8.0 + 8.0, "{text} at {}", pool.rect(text).left());
        }
    }

    /// The list's header names the Name line, and a heading at the right
    /// edge fits the panel rather than being cut.
    #[test]
    fn the_lists_header_has_name_and_fits() {
        let mut pool = Pool::new(BrowserViewMode::List, &["a.mov"]);
        pool.size = egui::vec2(480.0, 900.0);
        pool.texts();
        pool.texts();
        assert!(pool.drawn.iter().any(|(t, _)| t.starts_with("Name")), "{:?}", pool.drawn);
        // The list, below the pool's toolbar.
        let top = pool.rect("New Bin").bottom();
        let past: Vec<_> = pool.drawn.iter().filter(|(_, r)| r.top() > top && r.right() > pool.size.x + 0.5).collect();
        assert!(past.is_empty(), "past the panel: {past:?}");
        // At any width a heading is whole, never cut to a letter or two.
        let labels: Vec<&str> = pool.panel.list_columns.iter().map(|c| c.label()).collect();
        for width in [360.0, 420.0, 480.0, 560.0, 640.0, 760.0] {
            pool.size = egui::vec2(width, 900.0);
            pool.texts();
            pool.texts();
            let cut: Vec<_> = pool.drawn.iter().filter(|(t, r)| r.top() > top && t.ends_with('\u{2026}') && labels.iter().any(|l| l.starts_with(t.trim_end_matches('\u{2026}')))).collect();
            assert!(cut.is_empty(), "at {width}: headings cut {cut:?}");
        }
    }

    /// #2621: a clip dragged onto a bin header goes into the bin, and
    /// the drop is the header's: the drag is over when the button comes
    /// up. (The timeline's side is `a_release_above_the_timeline_adds_nothing`.)
    #[test]
    fn a_drop_on_a_bin_header_is_the_headers_alone() {
        for view in [BrowserViewMode::List, BrowserViewMode::Grid] {
            let mut pool = Pool::new(view, &["a.mov", "b.mov"]);
            in_bin(&mut pool, "b.mov", "Selects");
            let a = pool.state.media_pool.all().find(|m| m.filename == "a.mov").expect("a").id;
            let (from, to) = (pool.at("a.mov"), pool.at("Selects"));
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            pool.frame(vec![egui::Event::PointerMoved(from), button(from, true)]);
            let mut sent = Vec::new();
            for k in 1..=8 {
                let at = from + (to - from) * (k as f32 / 8.0);
                sent.extend(pool.frame(vec![egui::Event::PointerMoved(at)]).1);
            }
            assert_eq!(pool.state.dragging_media, Some(a), "{view:?}: dragging");
            sent.extend(pool.frame(vec![button(to, false)]).1);
            sent.extend(pool.frame(Vec::new()).1);
            assert!(sent.iter().any(|c| matches!(c, AppCommand::MoveItemsToBin { media_ids, bin: Some(b) } if *media_ids == [a] && b == "Selects")), "{view:?}: {sent:?}");
            assert!(pool.state.dragging_media.is_none() && pool.state.dragging_media_group.is_empty(), "{view:?}: the drop is taken");
        }
    }

    /// #2621: New Bin, with or without items, is one undo step, and
    /// undoing it takes the bin away as well as the items back out.
    #[test]
    fn new_bin_undoes_in_one_step_bin_and_all() {
        for with_items in [false, true] {
            let mut state = AppState::default();
            let a = state.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/footage/a.mov".into(), None));
            let ids = if with_items { vec![a] } else { Vec::new() };
            state.new_bin("New Bin 2", &ids);
            assert!(state.media_pool.bins.iter().any(|b| b.name == "New Bin 2"));
            let step = state.project_history_mut().undo().expect("a step");
            assert!(state.project_history_mut().undo().is_none(), "one step");
            assert_eq!(step.command.label(), "New Bin");
            let mut pool = state.media_pool.clone();
            let (mut sequences, mut fps) = (state.sequences.clone(), state.fps);
            klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_inverse(&step.command);
            assert!(!pool.bins.iter().any(|b| b.name == "New Bin 2"), "undone, the bin is gone");
            assert!(pool.get(&a).expect("a").bin.is_none());
            klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&step.command);
            assert!(pool.bins.iter().any(|b| b.name == "New Bin 2"), "redone, it is back");
            assert_eq!(pool.get(&a).expect("a").bin.is_some(), with_items);
        }
    }

    /// The sort direction is a choice in Sort By; the pool's menu has no
    /// icon row of its own for it.
    #[test]
    fn the_sort_direction_is_in_sort_by() {
        let mut pool = Pool::new(BrowserViewMode::Grid, &["a.mov"]);
        let below = pool.at("a.mov") + egui::vec2(0.0, 300.0);
        let menu = pool.right_click(below);
        assert!(!menu.iter().any(|t| t == "Ascending" || t == "Descending"), "{menu:?}");
        let sub = pool.at("Sort By");
        pool.frame(vec![egui::Event::PointerMoved(sub)]);
        pool.frame(vec![egui::Event::PointerMoved(sub)]);
        let cmds_before = pool.state.selected_media_ids.len();
        let _ = cmds_before;
        pool.click("Descending");
        assert!(!pool.panel.media_sort_ascending, "descending now");
    }

    /// A pool menu's row shows a band while hovered, as other menus do.
    #[test]
    fn a_hovered_menu_row_gets_a_band() {
        let mut pool = Pool::new(BrowserViewMode::Grid, &["a.mov"]);
        let below = pool.at("a.mov") + egui::vec2(0.0, 300.0);
        pool.right_click(below);
        let row = pool.at("Import Media\u{2026}");
        let under = |pool: &Pool| pool.fills.iter().filter(|r| r.contains(row)).count();
        let resting = under(&pool);
        pool.frame(vec![egui::Event::PointerMoved(row)]);
        pool.frame(vec![egui::Event::PointerMoved(row)]);
        assert!(under(&pool) > resting, "a band under the hovered row");
    }

    /// #2632: the pool's menus, on no item, on a bin header and on a
    /// clip, draw their rows in the one row layout.
    #[test]
    fn the_pools_menus_line_up() {
        let mut problems = Vec::new();
        for view in [BrowserViewMode::Grid, BrowserViewMode::List] {
            let mut pool = Pool::new(view, &["a.mov", "b.mov"]);
            in_bin(&mut pool, "b.mov", "Selects");
            let below = pool.at("a.mov") + egui::vec2(0.0, 300.0);
            for (at, anchor) in [(below, "New Bin"), (pool.at("Selects"), "Rename Bin"), (pool.at("a.mov"), "Load in Source")] {
                pool.key(egui::Key::Escape, egui::Modifiers::NONE);
                pool.right_click(at);
                pool.texts();
                pool.texts();
                problems.extend(crate::widgets::menu::row_problems(&pool.shapes, anchor).into_iter().map(|p| format!("{view:?} {anchor}: {p}")));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
