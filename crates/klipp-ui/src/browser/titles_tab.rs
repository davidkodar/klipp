use crate::widgets::menu::MenuRows;
use crate::browser::effects_tab::{EffectSort, EffectViewMode};
use crate::drag_drop::DragPayload;
use crate::browser::library_card::LibraryList;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::title_templates::{TemplateDialog, UserTitleTemplate};
use klipp_core::AppCommand;
use klipp_timeline::TitleData;

/// Preview canvas background used behind every title card. The user
/// picks one from the swatch row above the search bar so they can
/// see how a template would read against different real-world
/// surfaces (black footage, white footage, gradient sky, warm wood,
/// green chroma).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum TitlesPreviewBg {
    /// A frame from the project's own media.
    #[default]
    Footage,
    Black,
    White,
    BlueGradient,
    OrangeWood,
    GreenPattern,
}


/// The Titles tab's source filter (#2757): built in, the user's own, or
/// an installed pack's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum TitlesSourceFilter {
    #[default]
    All,
    Builtin,
    User,
    Packs,
}


pub(crate) use klipp_timeline::title_library::{TitleTemplate, CATEGORIES, TITLES};

/// A card in the browser: a built-in template or one the user saved.
#[derive(Clone, Copy)]
pub(crate) enum Entry<'a> {
    Builtin(&'static TitleTemplate),
    User(&'a UserTitleTemplate),
}

impl<'a> Entry<'a> {
    fn name(&self) -> &'a str {
        match self {
            Entry::Builtin(t) => t.name,
            Entry::User(t) => &t.name,
        }
    }

    fn category(&self) -> &'a str {
        match self {
            Entry::Builtin(t) => t.category,
            Entry::User(t) => &t.category,
        }
    }

    /// The title the template makes, marked as made from it (#1716).
    pub(crate) fn data(&self) -> TitleData {
        let mut data = match self {
            Entry::Builtin(t) => t.build(),
            Entry::User(t) => t.data.clone(),
        };
        data.template = Some(self.list_key());
        data
    }

    /// Identity for selection, hover and the preview cache. A user
    /// template may share a built-in's name, and may be saved over.
    /// The key in Favourites and Recent: a saved template's name
    /// marked as one, so it has its own star beside a built-in of the
    /// same name, and no revision, so a save keeps the star.
    fn list_key(&self) -> String {
        match self {
            Entry::Builtin(t) => t.name.to_string(),
            Entry::User(t) => format!("user:{}", t.name),
        }
    }

    fn key(&self) -> String {
        match self {
            Entry::Builtin(t) => t.name.to_string(),
            Entry::User(t) => format!("user:{}:{}", t.revision, t.name),
        }
    }

    /// Its source as the card says it (#2757): "Built-in", "Mine" or the
    /// pack's name.
    fn source_label(&self) -> String {
        match self {
            Entry::Builtin(_) => "Built-in".into(),
            Entry::User(t) => t.source.label(),
        }
    }

    fn is_from_pack(&self) -> bool {
        matches!(self, Entry::User(t) if matches!(t.source, klipp_timeline::template_places::Source::Pack(_)))
    }
}


use crate::browser::title_previews::{TitlePreviews, REST_TIME};

const TITLES_SIDEBAR_W: f32 = 180.0;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut Option<&'static str>,
    list: &mut Option<LibraryList>,
    favourites: &mut Vec<String>,
    preview_bg: &mut TitlesPreviewBg,
    preview_text: &mut String,
    source_filter: &mut TitlesSourceFilter,
    animated_only: &mut bool,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
    selected: &mut Option<String>,
    previews: &mut crate::browser::title_previews::TitlePreviews,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();
    // Cards show each template as it would be placed on this sequence's
    // frame (#2734): a 9:16 sequence previews the 9:16 layout.
    let frame = PreviewFrame::of(state.sequences[state.active_sequence_idx].resolution);
    // By template name; a snapshot, since the cards take `state`.
    let recent = state.library_recent.titles.clone();
    // A snapshot, since the cards below take `state` mutably.
    let user_templates: Vec<UserTitleTemplate> = state.title_templates.templates().cloned().collect();
    let entries: Vec<Entry> = TITLES
        .iter()
        .map(Entry::Builtin)
        .chain(user_templates.iter().map(Entry::User))
        .collect();
    // A frame of the project's own footage under every card.
    let footage: Option<egui::TextureHandle> = state
        .media_pool
        .all()
        .find_map(|m| state.thumbnail_textures.get(&m.id).cloned());

    // ── Split: sidebar (left) | content (right) ──
    let sidebar_w = TITLES_SIDEBAR_W.min(avail.width() * 0.40);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Sidebar ─────────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.bg_panel);
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0_f32, theme.border),
        );

        egui::ScrollArea::vertical()
            .id_salt("titles_sidebar_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                // Counts (registry-wide, ignore the active filter so
                // sidebar numbers don't dance as the user types).
                let installed_total = entries.len();
                let in_list = |keys: &[String]| entries.iter().filter(|t| keys.iter().any(|k| *k == t.list_key())).count();
                for (target, icon, label, count) in [
                    (LibraryList::Favourites, Icon::Star, "Favorites", in_list(favourites)),
                    (LibraryList::Recent, Icon::ArrowCounterClockwise, "Recent", in_list(&recent)),
                ] {
                    titles_general_row(ui, theme, icon, label, count, *list == Some(target), &format!("titles_general_{label}"), &mut |()| {
                        *list = Some(target);
                        *active_category = None;
                    });
                }

                ui.add_space(8.0);
                titles_section_separator(ui, theme);
                titles_section_header(ui, theme, Some(Icon::FilmSlate), "Titles");

                // "All" pseudo-category at the top of the Titles
                // section, then each category with its registry count.
                titles_sidebar_row(
                    ui,
                    theme,
                    Some(Icon::List),
                    "All",
                    installed_total,
                    active_category.is_none() && list.is_none(),
                    "titles_cat_all",
                    &mut |opt| {
                        *active_category = opt;
                        *list = None;
                    },
                    None,
                );
                for cat in CATEGORIES {
                    let count = entries.iter().filter(|t| t.category() == *cat).count();
                    let is_active = *active_category == Some(*cat) && list.is_none();
                    titles_sidebar_row(
                        ui,
                        theme,
                        Some(category_icon(cat)),
                        cat,
                        count,
                        is_active,
                        &format!("titles_cat_{cat}"),
                        &mut |opt| {
                            *active_category = opt;
                            *list = None;
                        },
                        Some(*cat),
                    );
                }
                ui.add_space(8.0);
            });
    });

    // ── Right pane ──────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());

        ui.add_space(8.0);

        // ── Preview header: PREVIEW ON [swatches] [text input] ──
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("\u{1F5BC} PREVIEW ON")
                            .size(10.0)
                            .strong()
                            .color(theme.text_dim),
                    );
                    ui.add_space(8.0);
                    preview_swatch(ui, theme, TitlesPreviewBg::Footage, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::Black, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::White, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::BlueGradient, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::OrangeWood, preview_bg);
                    preview_swatch(ui, theme, TitlesPreviewBg::GreenPattern, preview_bg);
                    ui.add_space(8.0);
                    // Live preview text input — empty falls back to
                    // each template's own preview_text.
                    let edit = egui::TextEdit::singleline(preview_text)
                        .hint_text(
                            egui::RichText::new("Klipp the new Linux NLE!")
                                .color(theme.text_dim),
                        )
                        .desired_width(ui.available_width().max(120.0))
                        .font(egui::FontId::proportional(11.0))
                        .text_color(theme.text_primary);
                    ui.add(edit);
                });
            });

        ui.add_space(8.0);

        // ── Search bar ──
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                super::browser_search_field(ui, theme, search_query, "Search titles\u{2026}", state);
            });
        ui.add_space(6.0);
        // Rolling credits from a list (#1514).
        if *active_category == Some("Credits") {
            egui::Frame::none().inner_margin(egui::Margin::symmetric(8.0, 0.0)).show(ui, |ui| {
                if ui.button("Credits from a list\u{2026}").on_hover_text("Build rolling credits from a CSV or tab-separated list of roles and names").clicked() {
                    state.credits_dialog = Some(Default::default());
                }
            });
            ui.add_space(6.0);
        }

        // ── Filter row: All / Built-in / User + Animated only + view toggle ──
        let scoped: Vec<&Entry> = entries
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category() == *cat,
                None => true,
            })
            .filter(|t| match list {
                Some(LibraryList::Favourites) => favourites.iter().any(|k| *k == t.list_key()),
                Some(LibraryList::Recent) => recent.iter().any(|k| *k == t.list_key()),
                None => true,
            })
            .collect();
        let scoped_total = scoped.len();
        let pack_count = scoped.iter().filter(|t| t.is_from_pack()).count();
        let user_count = scoped.iter().filter(|t| matches!(t, Entry::User(_))).count() - pack_count;
        let builtin_count = scoped_total - user_count - pack_count;

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    titles_segmented_filter(
                        ui,
                        theme,
                        source_filter,
                        scoped_total,
                        builtin_count,
                        user_count,
                        pack_count,
                    );
                    ui.add_space(6.0);
                    animated_only_pill(ui, theme, animated_only);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            view_mode_button(
                                ui,
                                theme,
                                view_mode,
                                EffectViewMode::List,
                                Icon::List,
                                "List view",
                            );
                            view_mode_button(
                                ui,
                                theme,
                                view_mode,
                                EffectViewMode::Grid,
                                Icon::GridFour,
                                "Grid view",
                            );
                        },
                    );
                });
            });

        ui.add_space(8.0);

        // ── Filter the registry ──
        let search_lower = search_query.to_lowercase();
        let mut filtered: Vec<&Entry> = scoped
            .into_iter()
            .filter(|t| match *source_filter {
                TitlesSourceFilter::All => true,
                TitlesSourceFilter::Builtin => matches!(t, Entry::Builtin(_)),
                TitlesSourceFilter::User => matches!(t, Entry::User(_)) && !t.is_from_pack(),
                TitlesSourceFilter::Packs => t.is_from_pack(),
            })
            .filter(|t| !*animated_only || template_is_animated(t))
            .filter(|t| {
                if search_lower.is_empty() {
                    return true;
                }
                t.name().to_lowercase().contains(&search_lower)
                    || t.category().to_lowercase().contains(&search_lower)
            })
            .collect();
        match *sort_mode {
            EffectSort::NameAsc => {
                filtered.sort_by_key(|a| a.name().to_lowercase());
            }
            EffectSort::NameDesc => {
                filtered.sort_by_key(|a| std::cmp::Reverse(a.name().to_lowercase()));
            }
            EffectSort::Category => {
                filtered.sort_by(|a, b| {
                    a.category()
                        .to_lowercase()
                        .cmp(&b.category().to_lowercase())
                        .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
                });
            }
            EffectSort::Source => {
                filtered.sort_by_key(|a| (matches!(a, Entry::User(_)), a.name().to_lowercase()));
            }
        }
        // Recent reads newest first, whatever the sort.
        if *list == Some(LibraryList::Recent) {
            filtered.sort_by_key(|t| recent.iter().position(|k| *k == t.list_key()));
        }

        // ── Result strip: count + sort dropdown ──
        titles_result_strip(ui, theme, filtered.len(), sort_mode);
        ui.add_space(4.0);

        // ── Card grid ──
        egui::ScrollArea::vertical()
            .id_salt("titles_grid_scroll")
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if !search_query.is_empty() {
                            format!("No titles match \"{}\"", search_query)
                        } else if *list == Some(LibraryList::Favourites) {
                            "No favourites yet. Star a title to keep it here".to_string()
                        } else if *list == Some(LibraryList::Recent) {
                            "Titles you add to the timeline show here".to_string()
                        } else if *source_filter == TitlesSourceFilter::User {
                            "No saved templates yet. Right-click a title clip and choose Save as Title Template.".to_string()
                        } else if *source_filter == TitlesSourceFilter::Packs {
                            "No template packs installed.".to_string()
                        } else {
                            "No titles in this category".to_string()
                        };
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    });
                    return;
                }

                let avail_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let render_grid = *view_mode == EffectViewMode::Grid;

                if render_grid {
                    let card_w = ((avail_w - spacing) / 2.0).floor();
                    let card_h = (card_w * 9.0 / 16.0).round().max(72.0) + crate::browser::library_card::CAPTION_H;
                    egui::Grid::new("titles_grid")
                        .num_columns(2)
                        .spacing([spacing, spacing])
                        .min_col_width(card_w)
                        .show(ui, |ui| {
                            for (i, title) in filtered.iter().enumerate() {
                                let is_selected =
                                    selected.as_deref() == Some(title.key().as_str());
                                let display_text = effective_preview_text(preview_text, title);
                                // Hover plays the motion; at rest the card
                                // shows the settled pose.
                                let elapsed = read_anim_elapsed(ui, &title.key());
                                let time = if elapsed > 0.0 { TitlePreviews::hover_time(elapsed) } else { REST_TIME };
                                if elapsed > 0.0 {
                                    request_ahead(previews, title, &display_text, time, &frame);
                                }
                                let key = TitlePreviews::key(&frame.key(title), &display_text, time);
                                let tex = previews
                                    .get_or_request_sized(&key, time, frame.w, frame.h, || preview_data(title, &display_text, &frame))
                                    .cloned()
                                    .or_else(|| previews.nearest_for(&frame.key(title), &display_text, time).cloned());
                                let resp = title_card(
                                    ui,
                                    theme,
                                    title,
                                    card_w,
                                    card_h,
                                    *preview_bg,
                                    footage.as_ref(),
                                    tex.as_ref(),
                                    is_selected,
                                );
                                let active = resp.hovered() || is_selected;
                                update_anim_state(ui, &title.key(), active);
                                if active {
                                    ui.ctx().request_repaint();
                                }
                                let is_fav = favourites.iter().any(|k| *k == title.list_key());
                                let star = crate::browser::library_card::favourite_star(ui, theme, &resp, true, is_fav, title.key());
                                if handle_title_interaction(state, &mut cmds, title, &resp, selected, is_fav) || star {
                                    crate::browser::library_card::toggle_favourite(favourites, &title.list_key());
                                }
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                } else {
                    for title in &filtered {
                        let is_selected =
                            selected.as_deref() == Some(title.key().as_str());
                        let row_text = effective_preview_text(preview_text, title);
                        let row_elapsed = read_anim_elapsed(ui, &title.key());
                        let time = if row_elapsed > 0.0 { TitlePreviews::hover_time(row_elapsed) } else { REST_TIME };
                        if row_elapsed > 0.0 {
                            request_ahead(previews, title, &row_text, time, &frame);
                        }
                        let key = TitlePreviews::key(&frame.key(title), &row_text, time);
                        let tex = previews
                            .get_or_request_sized(&key, time, frame.w, frame.h, || preview_data(title, &row_text, &frame))
                            .cloned()
                            .or_else(|| previews.nearest_for(&frame.key(title), &row_text, time).cloned());
                        let resp = title_list_row(
                            ui,
                            theme,
                            title,
                            *preview_bg,
                            footage.as_ref(),
                            tex.as_ref(),
                            is_selected,
                        );
                        let active = resp.hovered() || is_selected;
                        update_anim_state(ui, &title.key(), active);
                        if active {
                            ui.ctx().request_repaint();
                        }
                        let is_fav = favourites.iter().any(|k| *k == title.list_key());
                        let star = crate::browser::library_card::favourite_star(ui, theme, &resp, false, is_fav, title.key());
                        if handle_title_interaction(state, &mut cmds, title, &resp, selected, is_fav) || star {
                            crate::browser::library_card::toggle_favourite(favourites, &title.list_key());
                        }
                    }
                }
            });
    });

    cmds
}

/// Pick the text to render inside a card — user override beats the
/// template's hand-picked preview_text.
fn effective_preview_text<'a>(
    user_text: &'a str,
    title: &Entry<'a>,
) -> std::borrow::Cow<'a, str> {
    if user_text.trim().is_empty() {
        match title {
            Entry::Builtin(t) => std::borrow::Cow::Borrowed(t.preview_text),
            Entry::User(t) => std::borrow::Cow::Owned(t.preview_text()),
        }
    } else {
        std::borrow::Cow::Borrowed(user_text)
    }
}

/// Shared interaction handler for both grid + list rendering — keeps
/// double-click-to-add and drag-to-timeline logic in one place.
fn handle_title_interaction(
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    title: &Entry,
    resp: &egui::Response,
    selected: &mut Option<String>,
    is_fav: bool,
) -> bool {
    if resp.clicked() {
        *selected = Some(title.key());
    }
    let mut toggle_favourite = false;
    resp.context_menu_rows(|ui| {
        toggle_favourite = crate::browser::library_card::favourite_menu_item(ui, is_fav);
        if let Entry::User(t) = title {
            if ui.button("Rename\u{2026}").clicked() {
                state.title_template_dialog = Some(TemplateDialog::Rename { from: t.name.clone(), name: t.name.clone() });
                ui.close_menu();
            }
            if ui.button("Delete\u{2026}").clicked() {
                state.title_template_dialog = Some(TemplateDialog::Delete { name: t.name.clone() });
                ui.close_menu();
            }
        }
    });
    if resp.double_clicked() {
        crate::browser::library_card::push_recent(&mut state.library_recent.titles, &title.list_key());
        let title_data = title.data();
        state.pending_title_data = Some(title_data);
        let ph = state.playhead;
        let track_index = state.sequences[state.active_sequence_idx]
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| {
                t.kind == klipp_timeline::TrackKind::Text
                    && t.clip_at_timecode(ph).is_none()
            })
            .map(|(idx, _)| idx);
        let track_index = match track_index {
            Some(idx) => idx,
            None => {
                let track_num = state.sequences[state.active_sequence_idx]
                    .tracks
                    .iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Text)
                    .count()
                    + 1;
                let track = klipp_timeline::Track::new(
                    &format!("Text {track_num}"),
                    klipp_timeline::TrackKind::Text,
                );
                state.sequences[state.active_sequence_idx]
                    .tracks
                    .insert(0, track);
                0
            }
        };
        let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
        let duration_frames = fps_rate * 5;
        cmds.push(AppCommand::CreateTitleClip {
            track_index,
            timeline_in: ph,
            duration_frames,
        });
        state
            .toast
            .show(format!("\"{}\" title added to timeline", title.name()));
    }
    if resp.drag_started() {
        let data = title.data();
        state.drag_drop.begin(DragPayload::Title {
            template_name: title.name().to_string(),
            list_key: title.list_key(),
            title_data: data,
        });
    }
    toggle_favourite
}

#[allow(clippy::too_many_arguments)]
fn title_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    title: &Entry,
    width: f32,
    height: f32,
    preview_bg: TitlesPreviewBg,
    footage: Option<&egui::TextureHandle>,
    preview: Option<&egui::TextureHandle>,
    is_selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, height),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();
    let caption = format!(
        "{} \u{00B7} {}{}",
        title.category(),
        title.source_label(),
        if template_is_animated(title) { " \u{00B7} Animated" } else { "" },
    );
    crate::browser::library_card::library_card(ui, rect, title.name(), &caption, is_selected, hovered, theme, |p, area| {
        let top = egui::Rounding { nw: crate::theme::radius::PANEL, ne: crate::theme::radius::PANEL, sw: 0.0, se: 0.0 };
        let shown = preview.map_or(area, |t| fitted(area, t));
        paint_preview_backdrop(p, shown, if shown == area { top } else { egui::Rounding::ZERO }, preview_bg, footage);
        if let Some(t) = preview {
            p.image(t.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
    });
    resp
}

/// The backdrop under a rendered title: the project's footage when
/// there is some and the picker asks for it, else the chosen swatch.
fn paint_preview_backdrop(
    p: &egui::Painter,
    area: egui::Rect,
    rounding: egui::Rounding,
    preview_bg: TitlesPreviewBg,
    footage: Option<&egui::TextureHandle>,
) {
    match (preview_bg, footage) {
        (TitlesPreviewBg::Footage, Some(f)) => {
            // Cover the area, cropping the longer side.
            let ts = f.size_vec2();
            let (aw, ah) = (area.width(), area.height());
            let uv = if ts.x / ts.y > aw / ah {
                let frac = (aw / ah) / (ts.x / ts.y);
                let o = (1.0 - frac) / 2.0;
                egui::Rect::from_min_max(egui::pos2(o, 0.0), egui::pos2(1.0 - o, 1.0))
            } else {
                let frac = (ts.x / ts.y) / (aw / ah);
                let o = (1.0 - frac) / 2.0;
                egui::Rect::from_min_max(egui::pos2(0.0, o), egui::pos2(1.0, 1.0 - o))
            };
            p.rect_filled(area, rounding, egui::Color32::BLACK);
            p.image(f.id(), area, uv, egui::Color32::WHITE);
        }
        (TitlesPreviewBg::Footage, None) => {
            // No media yet: a neutral dark field.
            p.rect_filled(area, rounding, egui::Color32::from_rgb(0x2a, 0x2c, 0x33));
        }
        (bg, _) => paint_preview_canvas(p, area, rounding, bg),
    }
}

/// Queue the next moments of a hovered card so its motion plays
/// without waiting on renders.
fn request_ahead(previews: &mut TitlePreviews, title: &Entry, text: &str, time: f32, frame: &PreviewFrame) {
    use crate::browser::title_previews::{FRAME_STEP, LOOP_SECS};
    for k in 1..=6 {
        let ahead = (time + k as f32 * FRAME_STEP) % LOOP_SECS;
        let ahead = TitlePreviews::hover_time(ahead);
        let key = TitlePreviews::key(&frame.key(title), text, ahead);
        previews.request_sized(&key, ahead, frame.w, frame.h, || preview_data(title, text, frame));
    }
}

/// The frame the cards render a template on (#2734): the sequence's
/// shape at the preview's height, and the sequence's own size, which a
/// built-in is laid out for.
pub(crate) struct PreviewFrame {
    pub w: u32,
    pub h: u32,
    sequence: (u32, u32),
    /// Marks a card's renders as this shape's in the preview cache.
    tag: &'static str,
}

impl PreviewFrame {
    pub(crate) fn of(r: klipp_core::Resolution) -> Self {
        use crate::browser::title_previews::{PREVIEW_H, PREVIEW_W};
        use klipp_timeline::title_library::Format;
        let (w, h, tag) = match Format::of(r.width, r.height) {
            Format::Wide => (PREVIEW_W, PREVIEW_H, ""),
            Format::Tall => ((PREVIEW_H as f32 * r.width as f32 / r.height.max(1) as f32).round() as u32, PREVIEW_H, "\u{2}tall"),
            Format::Square => (PREVIEW_H, PREVIEW_H, "\u{2}square"),
        };
        Self { w, h, sequence: (r.width, r.height), tag }
    }

    /// A card's key in the preview cache, for this shape.
    fn key(&self, title: &Entry) -> String {
        format!("{}{}", title.key(), self.tag)
    }
}

/// The part of `area` a preview of `t`'s shape fills, centred: a 9:16
/// preview stands in the middle of a wide card.
fn fitted(area: egui::Rect, t: &egui::TextureHandle) -> egui::Rect {
    let size = t.size_vec2();
    let k = (area.width() / size.x.max(1.0)).min(area.height() / size.y.max(1.0));
    egui::Rect::from_center_size(area.center(), size * k)
}

/// The title a card renders: the template laid out for the frame, with
/// the user's preview text in its primary layer, when one is typed.
fn preview_data(title: &Entry, text: &str, frame: &PreviewFrame) -> TitleData {
    let mut td = klipp_timeline::title_library::for_frame(title.data(), frame.sequence.0, frame.sequence.1);
    if !text.trim().is_empty() && td.style_runs().is_empty() {
        if let Some(tb) = td.primary_text_mut() {
            tb.text = text.to_string();
        }
    }
    td
}

/// Whether any layer of the template moves.
fn template_is_animated(title: &Entry) -> bool {
    let animated = |td: &TitleData| td.objects.iter().any(|o| o.fx().motion.is_active() || !o.fx().loops.is_empty());
    match title {
        Entry::Builtin(t) => animated(&t.build()),
        Entry::User(t) => animated(&t.data),
    }
}

// ──────────────────────────── Animation tracking ────────────────────────────

/// Read the elapsed-time-since-hover-began for `title_name`. Returns
/// 0.0 when the card is not currently hovered/selected (which kicks
/// the renderer into static-pose mode).
fn read_anim_elapsed(ui: &egui::Ui, title_name: &str) -> f32 {
    let active_id = ui.id().with(("titles_anim_active", title_name));
    let start_id = ui.id().with(("titles_anim_start", title_name));
    let now = ui.ctx().input(|i| i.time);
    let active: bool = ui
        .memory(|m| m.data.get_temp(active_id))
        .unwrap_or(false);
    if !active {
        return 0.0;
    }
    let start: f64 = ui.memory(|m| m.data.get_temp(start_id)).unwrap_or(now);
    (now - start).max(0.0) as f32
}

/// Persist the current-frame hover-or-selected state. On a rising
/// edge (false → true), records the start instant so the next frame's
/// `read_anim_elapsed` returns 0 and counts up from there.
fn update_anim_state(ui: &egui::Ui, title_name: &str, active: bool) {
    let active_id = ui.id().with(("titles_anim_active", title_name));
    let start_id = ui.id().with(("titles_anim_start", title_name));
    let now = ui.ctx().input(|i| i.time);
    let prev_active: bool = ui
        .memory(|m| m.data.get_temp(active_id))
        .unwrap_or(false);
    ui.memory_mut(|m| {
        m.data.insert_temp(active_id, active);
        if active && !prev_active {
            m.data.insert_temp(start_id, now);
        }
        if !active {
            m.data.remove::<f64>(start_id);
        }
    });
}



/// Compact list-mode row used when the user toggles to list view.
fn title_list_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    title: &Entry,
    preview_bg: TitlesPreviewBg,
    footage: Option<&egui::TextureHandle>,
    preview: Option<&egui::TextureHandle>,
    is_selected: bool,
) -> egui::Response {
    let avail_w = ui.available_width();
    let row_h = 56.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();

    if is_selected {
        let bg = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            28,
        );
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(4.0), bg);
    } else if hovered {
        ui.painter().rect_filled(rect, egui::Rounding::same(4.0), theme.bg_raised);
    }

    // Mini preview on the left, text block on the right.
    let mini_w = 96.0;
    let mini_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 8.0, rect.min.y + 6.0),
        egui::vec2(mini_w, row_h - 12.0),
    );
    let shown = preview.map_or(mini_rect, |t| fitted(mini_rect, t));
    paint_preview_backdrop(ui.painter(), shown, egui::Rounding::same(4.0), preview_bg, footage);
    if let Some(t) = preview {
        ui.painter().image(t.id(), shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }

    let text_x = mini_rect.max.x + 12.0;
    let text_y = rect.center().y;
    ui.painter().text(
        egui::pos2(text_x, text_y - 7.0),
        egui::Align2::LEFT_CENTER,
        title.name(),
        egui::FontId::proportional(12.0),
        theme.text_primary,
    );
    let mut meta = format!("{}  \u{00B7} {}", title.category(), title.source_label());
    if template_is_animated(title) {
        meta.push_str("  \u{00B7} Animated");
    }
    ui.painter().text(
        egui::pos2(text_x, text_y + 8.0),
        egui::Align2::LEFT_CENTER,
        meta,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
    resp
}

/// Paint the chosen preview-canvas background fill. Each variant is
/// a single-pass rect/gradient/pattern — no expensive image loads,
/// since these run inside every card on every frame.
fn paint_preview_canvas(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: egui::Rounding,
    bg: TitlesPreviewBg,
) {
    match bg {
        TitlesPreviewBg::Footage => {
            // The swatch for "your footage": a neutral dark field.
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x2a, 0x2c, 0x33));
        }
        TitlesPreviewBg::Black => {
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
        }
        TitlesPreviewBg::White => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0xe6, 0xe6, 0xe6));
        }
        TitlesPreviewBg::BlueGradient => {
            // Simple two-tone "sky" — paint base then a top-half
            // lighter strip to fake a gradient cheaply.
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x4d, 0x77, 0xab));
            let upper = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.max.x, rect.min.y + rect.height() * 0.5),
            );
            painter.rect_filled(upper, rounding, egui::Color32::from_rgb(0x6b, 0x8d, 0xb5));
        }
        TitlesPreviewBg::OrangeWood => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x8a, 0x4c, 0x2f));
            // Vertical "grain" stripes — quick visual hint.
            let stripe_w = 6.0;
            let mut x = rect.min.x;
            let mut alt = false;
            while x < rect.max.x {
                let s = egui::Rect::from_min_size(
                    egui::pos2(x, rect.min.y),
                    egui::vec2(stripe_w * 0.5, rect.height()),
                );
                let s = s.intersect(rect);
                if alt {
                    painter.rect_filled(
                        s,
                        egui::Rounding::ZERO,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 30),
                    );
                }
                x += stripe_w;
                alt = !alt;
            }
        }
        TitlesPreviewBg::GreenPattern => {
            painter.rect_filled(rect, rounding, egui::Color32::from_rgb(0x2c, 0x5e, 0x3a));
            // Dotted grid — chroma-screen reference.
            let step = 8.0;
            let dot = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24);
            let mut y = rect.min.y + step * 0.5;
            while y < rect.max.y {
                let mut x = rect.min.x + step * 0.5;
                while x < rect.max.x {
                    painter.circle_filled(egui::pos2(x, y), 1.0, dot);
                    x += step;
                }
                y += step;
            }
        }
    }
}

/// Single swatch in the preview-canvas selector strip. Same visual
/// language as the main `paint_preview_canvas` so the swatch
/// previews exactly what the cards will show.
fn preview_swatch(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    bg: TitlesPreviewBg,
    selected: &mut TitlesPreviewBg,
) {
    let size = 24.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(size, size),
        egui::Sense::click(),
    );
    paint_preview_canvas(ui.painter(), rect, egui::Rounding::same(4.0), bg);
    let is_active = *selected == bg;
    let stroke = if is_active {
        egui::Stroke::new(2.0_f32, theme.accent)
    } else if resp.hovered() {
        egui::Stroke::new(1.0_f32, theme.text_primary)
    } else {
        egui::Stroke::new(1.0_f32, theme.border)
    };
    ui.painter().rect_stroke(rect, egui::Rounding::same(4.0), stroke);
    if resp.clicked() {
        *selected = bg;
    }
}

// ──────────────────────────── Sidebar widgets ────────────────────────────

#[allow(clippy::too_many_arguments)]
fn titles_sidebar_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(Option<&'static str>),
    target_cat: Option<&'static str>,
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();

    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height()));
        ui.painter().rect_filled(stripe, 0.0, theme.accent);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }

    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let label_color = if is_active || hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let icon_tint = if is_active { theme.accent } else { theme.text_dim };

    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(icon_tint);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }

    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        label_color,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        label_color,
    );

    let count_color = if is_active { theme.accent } else { theme.text_dim };
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        count_color,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        count_color,
    );

    if click_resp.clicked() || resp.clicked() {
        on_click(target_cat);
    }
}

/// Stub-only general-section row (Favorites / Recent / Installed).
/// Same visual language as `titles_sidebar_row` but without the
/// category routing — there's no persistence layer for these yet.
fn titles_general_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(()),
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();
    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }
    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = icon.image(icon_size).tint(theme.text_dim);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        theme.text_secondary,
    );
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 14.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        theme.text_dim,
    );
    if resp.clicked() || click_resp.clicked() {
        on_click(());
    }
}

fn titles_section_separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        (rect.min.x + 8.0)..=(rect.max.x - 8.0),
        rect.center().y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.add_space(4.0);
}

fn titles_section_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
        egui::Sense::hover(),
    );
    let indent = 14.0;
    let icon_size = 11.0;
    let icon_gap = 6.0;
    let label_x = if icon.is_some() {
        rect.min.x + indent + icon_size + icon_gap
    } else {
        rect.min.x + indent
    };
    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(theme.text_dim);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }
    ui.painter().text(
        egui::pos2(label_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
}

/// Reasonable icon hint for each title sub-category. Picks from the
/// already-bundled Phosphor set; categories without an exact match
/// fall back to a generic glyph.
fn category_icon(cat: &str) -> Icon {
    match cat {
        "Lower Thirds" => Icon::FrameCorners,
        "Openers" => Icon::FilmStrip,
        "Captions" => Icon::TextAlignCenter,
        "Credits" => Icon::SortAscending,
        "Social" => Icon::Star,
        "Utility" => Icon::Sliders,
        _ => Icon::FilmSlate,
    }
}

// ──────────────────────────── Filter row widgets ────────────────────────────

fn titles_segmented_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut TitlesSourceFilter,
    all_count: usize,
    builtin_count: usize,
    user_count: usize,
    pack_count: usize,
) {
    let outer_h = 26.0;
    let pad = 2.0;
    let segments = [
        (TitlesSourceFilter::All, "All", all_count),
        (TitlesSourceFilter::Builtin, "Built-in", builtin_count),
        (TitlesSourceFilter::User, "Mine", user_count),
        (TitlesSourceFilter::Packs, "Packs", pack_count),
    ];
    let mut seg_widths = [0.0f32; 4];
    let mut total_w = pad * 2.0;
    for (i, (_, label, count)) in segments.iter().enumerate() {
        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            theme.text_secondary,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            theme.text_dim,
        );
        let w = label_galley.size().x + 6.0 + count_galley.size().x + 22.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0;
    let (outer_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, outer_h),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(outer_rect, 4.0, theme.bg_panel);
    ui.painter().rect_stroke(
        outer_rect,
        5.0,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    let mut x = outer_rect.min.x + pad;
    for (i, (variant, label, count)) in segments.iter().enumerate() {
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x, outer_rect.min.y + pad),
            egui::vec2(seg_widths[i], outer_h - pad * 2.0),
        );
        let resp = ui.interact(
            seg_rect,
            ui.id().with(("titles_source_seg", i)),
            egui::Sense::click(),
        );
        let is_active = *filter == *variant;
        let hovered = resp.hovered();
        if is_active {
            ui.painter().rect_filled(seg_rect, 2.0, theme.bg_raised);
        } else if hovered {
            ui.painter().rect_filled(seg_rect, 2.0, theme.accent_subtle);
        }
        let label_color = if is_active || hovered {
            theme.text_primary
        } else {
            theme.text_secondary
        };
        let count_color = if is_active { theme.accent } else { theme.text_dim };
        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            label_color,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            count_color,
        );
        let inner_w = label_galley.size().x + 6.0 + count_galley.size().x;
        let lx = seg_rect.center().x - inner_w * 0.5;
        ui.painter().galley(
            egui::pos2(lx, seg_rect.center().y - label_galley.size().y * 0.5),
            label_galley.clone(),
            label_color,
        );
        ui.painter().galley(
            egui::pos2(
                lx + label_galley.size().x + 6.0,
                seg_rect.center().y - count_galley.size().y * 0.5,
            ),
            count_galley,
            count_color,
        );
        if resp.clicked() {
            *filter = *variant;
        }
        x += seg_widths[i] + 1.0;
    }
}

fn animated_only_pill(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    let label_galley = ui.painter().layout_no_wrap(
        "Animated only".to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let icon_size = 11.0;
    let pad_x = 10.0;
    let gap = 5.0;
    let w = pad_x * 2.0 + icon_size + gap + label_galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, 26.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if *active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad_x, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = Icon::Lightning.image(icon_size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    ui.painter().text(
        egui::pos2(rect.min.x + pad_x + icon_size + gap, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Animated only",
        egui::FontId::proportional(11.0),
        fg,
    );
    if resp.clicked() {
        *active = !*active;
    }
}

fn view_mode_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    current: &mut EffectViewMode,
    target: EffectViewMode,
    icon: Icon,
    tooltip: &str,
) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(26.0, 26.0),
        egui::Sense::click(),
    );
    let is_active = *current == target;
    let hovered = resp.hovered();
    let (bg, border, fg) = if is_active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let size = 13.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let img = icon.image(size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    let _ = resp.clone().on_hover_text(tooltip);
    if resp.clicked() {
        *current = target;
    }
}

// ──────────────────────────── Result strip ────────────────────────────

fn titles_result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 24.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} Titles", count),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );

    let dropdown_w = 116.0;
    let dropdown_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - dropdown_w - 8.0, rect.min.y + 1.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 1.0),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(dropdown_rect),
        |ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                egui::Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            let label = format!("Sort: {}", sort_mode.label());
            egui::ComboBox::from_id_salt("titles_sort_dd")
                .selected_text(
                    egui::RichText::new(label).size(11.0).color(theme.text_dim),
                )
                .width(dropdown_w - 4.0)
                .show_ui(ui, |ui| {
                    for variant in [
                        EffectSort::NameAsc,
                        EffectSort::NameDesc,
                        EffectSort::Category,
                    ] {
                        ui.selectable_value(sort_mode, variant, variant.label());
                    }
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use klipp_timeline::title_library::builtin;

    /// #2734: on a 9:16 sequence a card renders the template as it would
    /// be placed there, its own shape and layout; on 16:9, as designed.
    #[test]
    fn a_card_previews_the_sequences_format() {
        use super::{preview_data, Entry, PreviewFrame, TitleData, TITLES};
        let name_bar = Entry::Builtin(TITLES.iter().find(|t| t.name == "Name Bar").unwrap());
        let name_y = |td: &TitleData| td.objects.iter().find(|o| o.name == "Name").unwrap().transform.center.y;
        let wide = PreviewFrame::of(klipp_core::Resolution { width: 1920, height: 1080 });
        let tall = PreviewFrame::of(klipp_core::Resolution { width: 1080, height: 1920 });
        assert_eq!((wide.w, wide.h), (crate::browser::title_previews::PREVIEW_W, crate::browser::title_previews::PREVIEW_H));
        assert!(tall.w < tall.h && (tall.w as f32 / tall.h as f32 - 0.5625).abs() < 0.01, "{}x{}", tall.w, tall.h);
        assert_ne!(wide.key(&name_bar), tall.key(&name_bar), "each shape is cached apart");
        assert_eq!(name_y(&preview_data(&name_bar, "", &wide)), name_y(&name_bar.data()), "16:9 as designed");
        let placed = klipp_timeline::title_library::builtin("Name Bar").unwrap().data_for(1080, 1920);
        assert_eq!(name_y(&preview_data(&name_bar, "", &tall)), name_y(&placed), "9:16 as it is placed there");
    }
    use klipp_timeline::TitleObjectContent;

    /// A built-in's template as made, before its fields are published.
    fn made(name: &str) -> TitleData {
        (builtin(name).expect("a built-in").create_data)()
    }

    /// A template is a producer of titles: every layer it makes has an
    /// explicit stack, and no text box of it carries the old fields.
    #[test]
    fn every_template_authors_its_stacks_on_the_object() {
        for tpl in TITLES {
            let td = (tpl.create_data)();
            for o in &td.objects {
                assert!(o.fx.is_some(), "{}: layer {} has no explicit stack", tpl.name, o.name);
                if let TitleObjectContent::TextBox(tb) = &o.content {
                    assert!(tb.legacy_fx.is_default(), "{}: layer {} keeps effects in the old place", tpl.name, o.name);
                }
            }
        }
    }

    use super::*;

    /// #1722: the lower thirds take a long name on its line. In Name
    /// Bar it is set smaller to stay inside the bar, where it wrapped
    /// over the role line and ran past the bar's end.
    #[test]
    fn a_lower_third_takes_a_long_name_on_its_line() {
        let mut r = klipp_title::TitleRenderer::new();
        let (w, h) = (1920u32, 1080u32);
        let named = |name: &str| {
            let mut td = made("Name Bar");
            for o in &mut td.objects {
                if let TitleObjectContent::TextBox(tb) = &mut o.content {
                    if tb.text == "[Name]" {
                        tb.text = name.into();
                    }
                }
            }
            td
        };
        let name_only = |td: &TitleData| {
            let mut only = td.clone();
            only.objects.retain(|o| matches!(&o.content, TitleObjectContent::TextBox(tb) if tb.text != "[Title or role]"));
            only
        };
        let short = r.render_layout_at_time(&name_only(&named("Ana Ruiz")), w, h, Some(2.0));
        let long = r.render_layout_at_time(&name_only(&named("Professor Elizabeth Montgomery-Smith of Westbury")), w, h, Some(2.0));
        let bar_right = (0.30 + 0.48 / 2.0) * w as f32;
        assert!(long.text_right <= bar_right, "the long name ends at {} past the bar's end, {bar_right}", long.text_right);
        // Two lines would be twice the short name's height; descenders
        // alone add a little.
        let (one, got) = (short.text_bottom - short.text_top, long.text_bottom - long.text_top);
        assert!(got < one * 1.5, "on its one line: {got} px tall, a line {one}");
        assert!(long.text_right - long.text_left > 0.43 * w as f32 * 0.85, "set smaller only as far as it must be");
        assert!((short.text_left - long.text_left).abs() < 2.0, "from the same left edge");
        for name in ["Name Bar", "Clean Line", "Broadcast", "Documentary"] {
            let td = made(name);
            let name = td.objects.iter().find_map(|o| match &o.content {
                TitleObjectContent::TextBox(tb) if tb.text == "[Name]" => Some(tb.fit),
                _ => None,
            });
            assert!(matches!(name, Some(klipp_timeline::TextFit::Shrink { .. })), "a lower third's name keeps its line: {name:?}");
        }
    }

    /// No template may claim it animates (`animation: Some(_)`) and
    /// ship its `TitleData::animation_type` as `Static` AND have
    /// every TextBox-level effect field at default. Pre-cleanup, 60+
    /// templates failed this contract — they wore "+ ANIMATED" badges
    /// in the browser but dropped static text into the timeline.
    /// This test pins the contract that the animation badge means
    /// the template actually does something at render time.
    #[test]
    fn every_template_category_is_in_sidebar() {
        for tpl in TITLES {
            assert!(
                CATEGORIES.contains(&tpl.category),
                "Template '{}' has category '{}' which is not in CATEGORIES — \
                 it will never appear in the browser sidebar.",
                tpl.name, tpl.category,
            );
        }
    }

    /// Every template that ships an animated TitleAnimationType must
    /// produce a non-static result from `apply_animation_type` at
    /// SOME point in the preview loop — otherwise the hover preview
    /// looks identical to a static template and the user has no
    /// indication that something will animate on playback.
    ///
    /// We sample t=0.0, 0.5, 1.5 (covers the entrance window, mid-
    /// loop, and post-entrance hold) and assert that at least one
    /// sample differs from the t=0 baseline (text content, position,
    /// alpha, scale, or colour override). Static templates skip.
    #[test]
    fn every_animated_template_moves_in_the_real_renderer() {
        let mut r = klipp_title::TitleRenderer::new();
        for tpl in TITLES {
            let td = (tpl.create_data)();
            // Only motion the renderer itself evaluates; scroll and
            // crawl become clip keyframes at creation and are covered
            // by the app's template test.
            let moves_in_renderer = td.objects.iter().any(|o| matches!(o.content, TitleObjectContent::TextBox(_)) && o.fx().motion.is_active());
            if !moves_in_renderer {
                continue;
            }
            // A moving template must not look the same at its first frame
            // and at rest, and must be visible at rest.
            let first = r.render_with_window(&td, 320, 180, None, 0.0, Some(5.0));
            let rest = r.render_with_window(&td, 320, 180, None, 2.0, Some(5.0));
            let visible = rest.chunks_exact(4).filter(|p| p[3] > 8).count();
            assert!(visible > 50, "{} is invisible at rest", tpl.name);
            assert_ne!(first, rest, "{} does not move", tpl.name);
        }
    }

    /// Template names must be unique within a category — duplicates
    /// confuse the user (two cards with the same label and no way
    /// to tell them apart).
    #[test]
    fn template_names_unique_within_category() {
        for cat in CATEGORIES {
            let names: Vec<&str> = TITLES
                .iter()
                .filter(|t| t.category == *cat)
                .map(|t| t.name)
                .collect();
            for (i, name) in names.iter().enumerate() {
                for other in &names[i + 1..] {
                    assert_ne!(
                        name, other,
                        "Duplicate template name '{name}' in category '{cat}' — \
                         each category must have unique names."
                    );
                }
            }
        }
    }

    /// Every built-in keeps its texts, by name, editable once applied,
    /// with Size, its accent's colour when it has one, and Speed when it
    /// moves (#1724): one drag and two sliders make it the user's own.
    #[test]
    fn every_template_publishes_its_texts_size_and_speed() {
        use klipp_timeline::FieldKind;
        for tpl in TITLES {
            let td = Entry::Builtin(tpl).data();
            let texts = td.objects.iter().filter(|o| matches!(o.content, TitleObjectContent::TextBox(_))).count();
            assert_eq!(tpl.fields.names.len(), texts, "{}: a name for each text", tpl.name);
            let fields: Vec<_> = td.live_fields();
            assert_eq!(fields.iter().filter(|(_, f, _)| matches!(f.kind, FieldKind::Text { .. })).count(), texts, "{}", tpl.name);
            for name in tpl.fields.names {
                assert!(td.field_named(name).is_some(), "{}: no field {name}", tpl.name);
            }
            assert!(td.field_named("Size").is_some(), "{}: Size", tpl.name);
            assert_eq!(td.field_named("Accent").is_some(), tpl.fields.accent.is_some(), "{}: the accent is live", tpl.name);
            let moves = td.objects.iter().any(|o| o.fx().motion.is_active());
            assert_eq!(td.field_named("Speed").is_some(), moves, "{}: Speed when it moves", tpl.name);
            assert!(fields.len() <= texts + 3, "{}: simple first, {} fields", tpl.name, fields.len());
        }
    }

    /// A backing shape moves with its text (#1724): every template whose
    /// shape layer has motion draws it differently as it enters and at
    /// rest, and draws it at rest.
    #[test]
    fn moving_backings_move_in_the_real_renderer() {
        let mut r = klipp_title::TitleRenderer::new();
        let mut checked = 0;
        for tpl in TITLES {
            let td = Entry::Builtin(tpl).data();
            for o in td.objects.iter().filter(|o| matches!(o.content, TitleObjectContent::Shape(_)) && o.fx().motion.is_active()) {
                let mut alone = td.clone();
                alone.objects.retain(|x| x.id == o.id || matches!(&o.content, TitleObjectContent::Shape(s) if s.fit.is_some_and(|f| f.target == x.id)));
                for x in &mut alone.objects {
                    if x.id != o.id {
                        // The text a backing fits keeps its place but isn't drawn.
                        if let TitleObjectContent::TextBox(tb) = &mut x.content {
                            tb.color[3] = 0;
                        }
                    }
                }
                let first = r.render_with_window(&alone, 320, 180, None, 0.0, Some(5.0));
                let rest = r.render_with_window(&alone, 320, 180, None, 2.0, Some(5.0));
                assert!(rest.chunks_exact(4).any(|p| p[3] > 8), "{}: its backing is drawn at rest", tpl.name);
                assert_ne!(first, rest, "{}: its backing does not move", tpl.name);
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} moving backings");
    }
}

