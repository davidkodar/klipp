//! The Find by Picture window (#2212): a description of what is in the
//! picture finds the stretches of footage that look like it, each shown
//! by its best frame, to open in the source monitor or make a subclip of.
use klipp_core::{AppCommand, MediaId};

/// A stretch that looked like what was asked.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub media_id: MediaId,
    pub start_secs: f64,
    pub end_secs: f64,
    pub best_secs: f64,
    pub score: f32,
}

/// The open window.
#[derive(Default)]
pub struct VisualSearch {
    pub query: String,
    /// The query being searched, while it is.
    pub searching: Option<String>,
    pub hits: Vec<Hit>,
    /// The query the hits are for.
    pub found_for: Option<String>,
    pub refused: Option<String>,
    thumbs: std::collections::HashMap<(MediaId, usize), egui::TextureHandle>,
}

/// The commands that open `hit` in the source monitor, its stretch
/// marked in and out, at its best frame.
pub fn open_commands(hit: &Hit) -> Vec<AppCommand> {
    let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s.max(0.0), 1000);
    vec![
        AppCommand::LoadSourceMedia { media_id: hit.media_id },
        AppCommand::SetSourceInPoint(t(hit.start_secs)),
        AppCommand::SetSourceOutPoint(t(hit.end_secs)),
        AppCommand::SeekSource(t(hit.best_secs)),
    ]
}

/// A search's answer for the window, when it is still the one asked.
pub fn take_result(state: &mut crate::state::AppState, query: &str, hits: Result<Vec<(MediaId, f64, f64, f64, f32)>, String>) {
    let Some(w) = state.visual_search.as_mut().filter(|w| w.searching.as_deref() == Some(query)) else { return };
    w.searching = None;
    match hits {
        Ok(hits) => {
            w.hits = hits.into_iter().map(|(media_id, start_secs, end_secs, best_secs, score)| Hit { media_id, start_secs, end_secs, best_secs, score }).collect();
            w.found_for = Some(query.to_string());
            w.refused = None;
        }
        Err(why) => {
            w.hits.clear();
            w.refused = Some(why);
        }
    }
}

/// Draw the open window, if any; the commands it sends.
pub fn show_dialog(ctx: &egui::Context, state: &mut crate::state::AppState) -> Vec<AppCommand> {
    let Some(mut w) = state.visual_search.take() else { return Vec::new() };
    let mut out = Vec::new();
    let mut open = true;
    egui::Window::new("Find by Picture").open(&mut open).default_width(460.0).resizable(true).show(ctx, |ui| {
        ui.label(egui::RichText::new("Describe what is in the shot: \u{201c}a dog on a beach\u{201d}, \u{201c}close-up of hands\u{201d}. Searched on this machine, over footage analysed with the Picture Search model.").small().weak());
        ui.add_space(4.0);
        let mut go = false;
        ui.horizontal(|ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut w.query).hint_text("What is in the picture").desired_width(ui.available_width() - 70.0));
            go |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            go |= ui.add_enabled(w.searching.is_none() && !w.query.trim().is_empty(), egui::Button::new("Search")).clicked();
        });
        if go && w.searching.is_none() && !w.query.trim().is_empty() {
            let query = w.query.trim().to_string();
            w.searching = Some(query.clone());
            out.push(AppCommand::RunVisualSearch { query });
        }
        ui.add_space(6.0);
        if w.searching.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking through the footage\u{2026}");
            });
        } else if let Some(why) = &w.refused {
            ui.colored_label(ui.visuals().warn_fg_color, why);
        } else if w.found_for.is_some() && w.hits.is_empty() {
            ui.label("Nothing looked like that.");
        }
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for hit in w.hits.clone() {
                ui.horizontal(|ui| {
                    if let Some(tex) = thumb(ctx, state, &mut w.thumbs, &hit) {
                        ui.image((tex.id(), egui::vec2(96.0, 96.0 * tex.size()[1] as f32 / tex.size()[0].max(1) as f32)));
                    } else {
                        ui.allocate_exact_size(egui::vec2(96.0, 54.0), egui::Sense::hover());
                    }
                    ui.vertical(|ui| {
                        let name = state.media_pool.get(&hit.media_id).map_or("(removed)".to_string(), |m| m.filename.clone());
                        ui.label(name);
                        ui.label(egui::RichText::new(format!("{:.1}\u{2013}{:.1} s \u{00b7} match {:.0}%", hit.start_secs, hit.end_secs, hit.score * 100.0)).small().weak());
                        ui.horizontal(|ui| {
                            if ui.small_button("Open in Source").on_hover_text("Open this stretch in the source monitor, marked in and out").clicked() {
                                out.extend(open_commands(&hit));
                            }
                            if ui.small_button("Make Subclip").on_hover_text("Add this stretch to the media pool as a subclip, to file in a bin").clicked() {
                                out.extend(open_commands(&hit));
                                let name = w.found_for.as_deref().map(|q| q.chars().take(40).collect::<String>());
                                out.push(AppCommand::CreateSubclipFromZone { source_media_id: hit.media_id, name });
                            }
                        });
                    });
                });
                ui.separator();
            }
        });
    });
    if open {
        state.visual_search = Some(w);
    }
    out
}

/// The hit's best frame from its file's filmstrip, the nearest one made.
fn thumb(ctx: &egui::Context, state: &crate::state::AppState, cache: &mut std::collections::HashMap<(MediaId, usize), egui::TextureHandle>, hit: &Hit) -> Option<egui::TextureHandle> {
    let frames = state.thumbnail_cache.filmstrip(&hit.media_id)?;
    let (i, f) = frames.iter().enumerate().min_by(|a, b| (a.1.time_secs - hit.best_secs).abs().total_cmp(&(b.1.time_secs - hit.best_secs).abs()))?;
    if let Some(t) = cache.get(&(hit.media_id, i)) {
        return Some(t.clone());
    }
    let (w, h) = (f.width as usize, f.height as usize);
    if w == 0 || h == 0 || f.rgba.len() != w * h * 4 {
        return None;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &f.rgba);
    let t = ctx.load_texture(format!("visual-hit-{}-{i}", hit.media_id.0), image, egui::TextureOptions::LINEAR);
    cache.insert((hit.media_id, i), t.clone());
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_opens_marked_at_its_best_frame() {
        let media_id = MediaId::new();
        let cmds = open_commands(&Hit { media_id, start_secs: 4.0, end_secs: 10.0, best_secs: 6.0, score: 0.3 });
        assert!(matches!(cmds[0], AppCommand::LoadSourceMedia { media_id: m } if m == media_id));
        assert!(matches!(cmds[1], AppCommand::SetSourceInPoint(t) if (t.to_secs_f64() - 4.0).abs() < 1e-3));
        assert!(matches!(cmds[2], AppCommand::SetSourceOutPoint(t) if (t.to_secs_f64() - 10.0).abs() < 1e-3));
        assert!(matches!(cmds[3], AppCommand::SeekSource(t) if (t.to_secs_f64() - 6.0).abs() < 1e-3));
    }

    /// A late answer for a query no longer asked is dropped.
    #[test]
    fn only_the_answer_to_the_search_running_is_shown() {
        let mut state = crate::state::AppState {
            visual_search: Some(VisualSearch { searching: Some("sunset".into()), ..Default::default() }),
            ..Default::default()
        };
        take_result(&mut state, "dog", Ok(vec![(MediaId::new(), 0.0, 2.0, 1.0, 0.3)]));
        assert!(state.visual_search.as_ref().unwrap().hits.is_empty());
        take_result(&mut state, "sunset", Ok(vec![(MediaId::new(), 0.0, 2.0, 1.0, 0.3)]));
        let w = state.visual_search.as_ref().unwrap();
        assert_eq!((w.hits.len(), w.searching.is_none(), w.found_for.as_deref()), (1, true, Some("sunset")));
    }
}
