//! The font picker (#1719): each family shown in its own face, the
//! title's own words as the sample (its name where the title has none),
//! a search, starred families first and the recent ones after them,
//! and a family's weights a click away.

use std::collections::HashMap;
use std::sync::Mutex;

use klipp_timeline::{TextAlignment, TextBox, TitleData, TitlePosition};

use crate::icons::Icon;
use crate::theme::KlippTheme;

/// A preview's size in pixels.
pub const PREVIEW_W: u32 = 300;
pub const PREVIEW_H: u32 = 30;

/// New previews drawn a frame at most: the rest come on later frames,
/// so opening the picker never stalls the interface.
const PREVIEWS_PER_FRAME: usize = 6;

/// `sample` set in `family` on one line, white on clear, at the
/// preview's size: straight-alpha RGBA.
pub fn render_preview(renderer: &mut klipp_title::TitleRenderer, family: &str, sample: &str) -> Vec<u8> {
    // The renderer sizes type by the frame's short side over 1080.
    let font_size = 20.0 * 1080.0 / PREVIEW_H as f32;
    let tb = TextBox { text: sample.into(), font_name: family.into(), font_size, color: [255, 255, 255, 255], alignment: TextAlignment::Left, ..Default::default() };
    let mut td = TitleData::single_text(tb);
    td.objects[0].transform.center = TitlePosition { x: 0.01, y: 0.5 };
    td.objects[0].transform.anchor_norm = TitlePosition { x: 0.0, y: 0.5 };
    renderer.render(&td, PREVIEW_W, PREVIEW_H)
}

/// The previews made so far, by family and sample, and the renderer
/// that makes them.
#[derive(Default)]
struct Previews {
    renderer: Option<klipp_title::TitleRenderer>,
    textures: HashMap<(String, String), egui::TextureHandle>,
    frame: u64,
    made: usize,
}

/// The picker's previews, kept between frames and shared by every
/// picker; behind a lock, as the inspector reads the state shared.
#[derive(Default)]
pub struct FontPreviews {
    inner: Mutex<Previews>,
}

impl FontPreviews {
    /// `family`'s preview of `sample`: made now if the frame's budget
    /// allows, else later (a repaint is asked for).
    pub fn texture(&self, ctx: &egui::Context, family: &str, sample: &str) -> Option<egui::TextureHandle> {
        let mut p = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let key = (family.to_string(), sample.to_string());
        if let Some(t) = p.textures.get(&key) {
            return Some(t.clone());
        }
        if p.frame != ctx.cumulative_pass_nr() {
            (p.frame, p.made) = (ctx.cumulative_pass_nr(), 0);
        }
        if p.made >= PREVIEWS_PER_FRAME {
            ctx.request_repaint();
            return None;
        }
        p.made += 1;
        let renderer = p.renderer.get_or_insert_with(klipp_title::TitleRenderer::new);
        let rgba = render_preview(renderer, family, sample);
        let image = egui::ColorImage::from_rgba_unmultiplied([PREVIEW_W as usize, PREVIEW_H as usize], &rgba);
        let texture = ctx.load_texture(format!("font_preview:{family}:{sample}"), image, egui::TextureOptions::LINEAR);
        p.textures.insert(key, texture.clone());
        Some(texture)
    }
}

/// What the picker was asked this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Picked {
    /// A family chosen, and the weight with it when one was.
    pub family: Option<String>,
    pub weight: Option<u16>,
    /// A family starred or unstarred.
    pub toggle_favourite: Option<String>,
}

/// The picker's open state and search, kept between frames.
#[derive(Clone, Default)]
struct PickerMemory {
    open: bool,
    search: String,
    /// The family whose weights are shown.
    weights_of: Option<String>,
}

/// A family's weights, upright faces only, lightest first.
fn weights(family: &str) -> Vec<u16> {
    let registry = klipp_title::font_discovery::discover_system_fonts();
    let mut w: Vec<u16> = klipp_title::font_discovery::family_faces(&registry, family).into_iter().filter(|f| !f.italic).map(|f| f.weight).collect();
    w.dedup();
    w
}

/// The sections the picker lists, each its families: the starred ones,
/// the recent ones not starred, and the rest; every one matching
/// `search` (no case), in the order given.
pub fn sections<'a>(families: &'a [String], favourites: &[String], recents: &[String], search: &str) -> Vec<(&'static str, Vec<&'a str>)> {
    let needle = search.trim().to_lowercase();
    let matches = |f: &str| needle.is_empty() || f.to_lowercase().contains(&needle);
    let find = |name: &str| families.iter().find(|f| *f == name).map(String::as_str);
    let starred: Vec<&str> = favourites.iter().filter_map(|f| find(f)).filter(|f| matches(f)).collect();
    let recent: Vec<&str> = recents.iter().filter_map(|f| find(f)).filter(|f| matches(f) && !starred.contains(f)).collect();
    let rest: Vec<&str> = families.iter().map(String::as_str).filter(|f| matches(f) && !starred.contains(f) && !recent.contains(f)).collect();
    [("Favourites", starred), ("Recent", recent), ("All fonts", rest)].into_iter().filter(|(_, f)| !f.is_empty()).collect()
}

/// The families of Klipp's catalogue (#2732) not among `families`, each
/// whose name or tags have every word of `search`: offered to fetch,
/// in the catalogue's order.
pub fn from_catalogue(families: &[String], search: &str) -> Vec<&'static klipp_title::catalogue::CatalogueFamily> {
    let here = |name: &str| families.iter().any(|f| f.eq_ignore_ascii_case(name));
    klipp_title::catalogue::families().iter().filter(|f| !here(&f.name) && f.matches(search)).collect()
}

/// The picker: a field naming the `current` family that opens the list
/// under it. `sample` is the title's words for the previews.
#[allow(clippy::too_many_arguments)]
pub fn picker(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    id_salt: impl std::hash::Hash,
    current: &str,
    sample: &str,
    families: &[String],
    favourites: &[String],
    recents: &[String],
    previews: &FontPreviews,
) -> Picked {
    let id = ui.make_persistent_id(("font_picker", id_salt));
    let mut mem: PickerMemory = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    let mut out = Picked::default();
    let field = ui.add(egui::Button::new(egui::RichText::new(current).size(12.5)).min_size(egui::vec2(ui.available_width().min(260.0), 26.0)));
    if field.clicked() {
        mem.open = !mem.open;
    }
    if mem.open {
        let sample = sample.lines().next().map(str::trim).filter(|s| !s.is_empty());
        egui::Frame::none().fill(theme.bg_panel).stroke(egui::Stroke::new(1.0_f32, theme.border)).rounding(4.0).inner_margin(egui::Margin::same(6.0)).show(ui, |ui| {
            let search = ui.add(egui::TextEdit::singleline(&mut mem.search).hint_text("Search fonts\u{2026}").desired_width(f32::INFINITY));
            if field.clicked() {
                search.request_focus();
            }
            egui::ScrollArea::vertical().max_height(320.0).id_salt(("font_picker_list", id)).show(ui, |ui| {
                for (title, list) in sections(families, favourites, recents, &mem.search) {
                    ui.label(egui::RichText::new(title).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    for family in list {
                        let starred = favourites.iter().any(|f| f == family);
                        ui.horizontal(|ui| {
                            // A fixed square, so the row lays out the same
                            // whether the icon has loaded or not.
                            let (star_rect, star) = ui.allocate_exact_size(egui::vec2(18.0, PREVIEW_H as f32), egui::Sense::click());
                            let star = star.on_hover_text(if starred { "Unstar" } else { "Star: shown first" });
                            let icon_rect = egui::Rect::from_center_size(star_rect.center(), egui::vec2(14.0, 14.0));
                            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| {
                                ui.add(Icon::Star.image(14.0).tint(if starred { theme.accent } else { theme.text_dim }));
                            });
                            if star.clicked() {
                                out.toggle_favourite = Some(family.to_string());
                            }
                            let row = ui.allocate_response(egui::vec2((ui.available_width() - 36.0).max(80.0), PREVIEW_H as f32), egui::Sense::click());
                            if row.hovered() {
                                ui.painter().rect_filled(row.rect, 3.0, theme.bg_hover);
                            }
                            if family == current {
                                ui.painter().rect_stroke(row.rect, 3.0, egui::Stroke::new(1.0_f32, theme.accent));
                            }
                            // The family in its own face; its name small
                            // beside it, and alone until the preview comes.
                            let tint = if family == current { theme.text_primary } else { theme.text_secondary };
                            if let Some(tex) = previews.texture(ui.ctx(), family, sample.unwrap_or(family)) {
                                let r = egui::Rect::from_min_size(row.rect.min, egui::vec2(PREVIEW_W as f32, PREVIEW_H as f32).min(row.rect.size()));
                                ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(r.width() / PREVIEW_W as f32, 1.0)), tint);
                            }
                            ui.painter().text(row.rect.right_bottom() - egui::vec2(4.0, 2.0), egui::Align2::RIGHT_BOTTOM, family, egui::FontId::proportional(10.0), theme.text_dim);
                            if row.clicked() {
                                out.family = Some(family.to_string());
                            }
                            let w = weights(family);
                            if w.len() > 1 {
                                let open = mem.weights_of.as_deref() == Some(family);
                                if ui.small_button(if open { "\u{25be}" } else { "\u{25b8}" }).on_hover_text(format!("{} weights", w.len())).clicked() {
                                    mem.weights_of = (!open).then(|| family.to_string());
                                }
                            }
                        });
                        if mem.weights_of.as_deref() == Some(family) {
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(22.0);
                                for weight in weights(family) {
                                    if ui.small_button(klipp_title::font_discovery::weight_name(weight)).clicked() {
                                        (out.family, out.weight) = (Some(family.to_string()), Some(weight));
                                    }
                                }
                            });
                        }
                    }
                }
                // Families Klipp can fetch: named, with what they suit, as
                // their face is not here to show them in (#2734).
                let more = from_catalogue(families, &mem.search);
                if !more.is_empty() {
                    ui.label(egui::RichText::new("From Klipp\u{2019}s catalogue").size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                    for family in more {
                        let row = ui.allocate_response(egui::vec2(ui.available_width(), PREVIEW_H as f32), egui::Sense::click());
                        if row.hovered() {
                            ui.painter().rect_filled(row.rect, 3.0, theme.bg_hover);
                        }
                        let tags: Vec<&str> = family.style.iter().chain(&family.mood).map(String::as_str).take(3).collect();
                        ui.painter().text(row.rect.left_center() + egui::vec2(22.0, 0.0), egui::Align2::LEFT_CENTER, &family.name, egui::FontId::proportional(13.0), theme.text_secondary);
                        ui.painter().text(row.rect.right_center() - egui::vec2(4.0, 0.0), egui::Align2::RIGHT_CENTER, tags.join(" \u{b7} "), egui::FontId::proportional(10.0), theme.text_dim);
                        let row = row.on_hover_text(format!("Fetched when chosen ({}); the title shows it once it is here", family.licence));
                        if row.clicked() {
                            out.family = Some(family.name.clone());
                        }
                    }
                }
            });
        });
    }
    if out.family.is_some() {
        mem = PickerMemory::default();
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, mem));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn families() -> Vec<String> {
        ["Inter", "Bebas Neue", "Libre Baskerville", "Noto Sans"].map(String::from).to_vec()
    }

    struct Frame {
        texts: Vec<(String, egui::Pos2)>,
        picked: Picked,
    }

    fn frame(ctx: &egui::Context, favourites: &[String], previews: &FontPreviews, events: Vec<egui::Event>) -> Frame {
        let mut picked = Picked::default();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 900.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    picked = picker(ui, &KlippTheme::default(), "test", "Inter", "Opening night", &families(), favourites, &[], previews);
                });
            },
        );
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        Frame { texts, picked }
    }

    fn at(f: &Frame, text: &str) -> Option<egui::Pos2> {
        f.texts.iter().find(|(t, _)| t == text).map(|(_, p)| *p)
    }

    fn click(ctx: &egui::Context, favourites: &[String], previews: &FontPreviews, pos: egui::Pos2) -> Picked {
        let press = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(ctx, favourites, previews, vec![egui::Event::PointerMoved(pos), press(true)]);
        frame(ctx, favourites, previews, vec![press(false)]).picked
    }

    /// The done-when: open the picker, search, star a family; next time
    /// it is listed first. A row picks its family.
    #[test]
    fn a_starred_font_is_listed_first_next_time() {
        let ctx = egui::Context::default();
        let previews = FontPreviews::default();
        let f = frame(&ctx, &[], &previews, Vec::new());
        assert!(at(&f, "Bebas Neue").is_none(), "closed at first");
        click(&ctx, &[], &previews, at(&f, "Inter").expect("the field names the font"));
        // Typing goes to the search, which has the keyboard on opening.
        frame(&ctx, &[], &previews, vec![egui::Event::Text("bebas".into())]);
        let f = frame(&ctx, &[], &previews, Vec::new());
        let bebas = at(&f, "Bebas Neue").expect("found by the search");
        assert!(at(&f, "Libre Baskerville").is_none(), "the others are filtered out");
        // The star sits at the row's left, before the preview.
        // The panel's 8 px margin, the list's 7 px frame, the star's 18.
        let star = egui::pos2(8.0 + 7.0 + 9.0, bebas.y - 8.0);
        let picked = click(&ctx, &[], &previews, star);
        assert_eq!(picked.toggle_favourite.as_deref(), Some("Bebas Neue"));
        // Next time, starred, it is the first family, under Favourites.
        let ctx = egui::Context::default();
        let starred = vec!["Bebas Neue".to_string()];
        let f = frame(&ctx, &starred, &previews, Vec::new());
        click(&ctx, &starred, &previews, at(&f, "Inter").unwrap());
        let f = frame(&ctx, &starred, &previews, Vec::new());
        // "Inter" names the field too, so the other three are compared.
        let rows: Vec<(&str, f32)> = ["Bebas Neue", "Libre Baskerville", "Noto Sans"].into_iter().filter_map(|n| at(&f, n).map(|p| (n, p.y))).collect();
        let first = rows.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert_eq!(first.0, "Bebas Neue", "{rows:?}");
        let favourites_header = at(&f, "Favourites").expect("a Favourites section");
        assert!(favourites_header.y < first.1);
        // And a row picks its family.
        let picked = click(&ctx, &starred, &previews, egui::pos2(160.0, first.1 - 6.0));
        assert_eq!(picked.family.as_deref(), Some("Bebas Neue"));
    }

    /// Sections: starred, recent (not starred), the rest; all by search.
    #[test]
    fn the_sections_put_starred_then_recent_first() {
        let f = families();
        let s = sections(&f, &["Noto Sans".into()], &["Bebas Neue".into(), "Noto Sans".into()], "");
        assert_eq!(s, [("Favourites", vec!["Noto Sans"]), ("Recent", vec!["Bebas Neue"]), ("All fonts", vec!["Inter", "Libre Baskerville"])]);
        let s = sections(&f, &[], &[], "BASK");
        assert_eq!(s, [("All fonts", vec!["Libre Baskerville"])]);
        assert!(sections(&f, &["Gone".into()], &[], "").iter().all(|(_, l)| !l.contains(&"Gone")), "a starred font not installed is not listed");
    }

    /// The catalogue's families not here are offered, found by a tag as
    /// well as by name, and one here is not offered again (#2734).
    #[test]
    fn the_catalogue_offers_what_is_not_here_by_name_or_tag() {
        let names = |l: Vec<&klipp_title::catalogue::CatalogueFamily>| l.into_iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        let all = names(from_catalogue(&families(), ""));
        assert!(all.len() >= 30 && all.contains(&"Playfair Display".to_string()), "{all:?}");
        let here = vec!["Playfair Display".to_string()];
        assert!(!names(from_catalogue(&here, "")).contains(&"Playfair Display".to_string()), "a family here is not offered again");
        let script = names(from_catalogue(&families(), "script"));
        assert!(script.contains(&"Great Vibes".to_string()) && !script.contains(&"Oswald".to_string()), "{script:?}");
        assert_eq!(names(from_catalogue(&families(), "great vibes")), ["Great Vibes"]);
    }

    /// The previews are each family in its own face: two faces draw the
    /// same words differently, and both draw something.
    #[test]
    fn previews_render_in_their_own_faces() {
        let mut r = klipp_title::TitleRenderer::new();
        let (inter, bebas) = (render_preview(&mut r, "Inter", "Opening night"), render_preview(&mut r, "Bebas Neue", "Opening night"));
        let ink = |p: &[u8]| p.chunks_exact(4).filter(|p| p[3] > 128).count();
        assert!(ink(&inter) > 200 && ink(&bebas) > 200, "{} {}", ink(&inter), ink(&bebas));
        assert_ne!(inter, bebas, "each in its own face");
        assert_eq!(inter.len(), (PREVIEW_W * PREVIEW_H * 4) as usize);
    }
}
