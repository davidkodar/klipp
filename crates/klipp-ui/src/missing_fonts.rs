//! Fonts a project's titles name that this computer does not have
//! (#1720): found when a project opens or a title is placed, listed
//! with the titles that use them, and each replaced everywhere it is
//! used, one undo step a sequence. A font left missing keeps its name,
//! so installing it brings the title's look back.

use std::collections::BTreeMap;

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{Sequence, TitleData, TitleObjectContent};

use crate::state::AppState;

/// A title that uses a missing font: its sequence's name, its clip and
/// how the list names it (its words).
#[derive(Debug, Clone, PartialEq)]
pub struct TitleUse {
    pub sequence: String,
    pub clip: ClipId,
    pub label: String,
}

/// A font the project's titles name and this computer does not have,
/// and the titles that use it.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingFont {
    pub family: String,
    pub titles: Vec<TitleUse>,
}

/// The dialog listing them: the fonts still missing, and the
/// replacement chosen for each so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissingFontsDialog {
    pub fonts: Vec<MissingFont>,
    pub chosen: BTreeMap<String, String>,
}

/// Every family a title names: its text layers' and their styled runs'.
pub fn fonts_of(td: &TitleData) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in &td.objects {
        if let TitleObjectContent::TextBox(tb) = &o.content {
            for f in std::iter::once(&tb.font_name).chain(tb.style_runs.iter().filter_map(|r| r.font_name.as_ref())) {
                if !out.contains(f) {
                    out.push(f.clone());
                }
            }
        }
    }
    out
}

/// The fonts `sequences`' titles name that `installed` says are not
/// here, in name order, each with the titles that use it.
pub fn find(sequences: &[Sequence], installed: &dyn Fn(&str) -> bool) -> Vec<MissingFont> {
    let mut by_font: BTreeMap<String, Vec<TitleUse>> = BTreeMap::new();
    for seq in sequences {
        for clip in seq.all_clips() {
            let Some(td) = clip.title_data() else { continue };
            for family in fonts_of(td).into_iter().filter(|f| !installed(f)) {
                let label = td.primary_text().map(|tb| tb.text.lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Title".into());
                by_font.entry(family).or_default().push(TitleUse { sequence: seq.name.clone(), clip: clip.id, label });
            }
        }
    }
    by_font.into_iter().map(|(family, titles)| MissingFont { family, titles }).collect()
}

/// `td` with every use of the family `from` set to `to`: its text
/// layers' and their runs'. Whether anything changed.
pub fn replace_in(td: &mut TitleData, from: &str, to: &str) -> bool {
    let mut changed = false;
    for o in &mut td.objects {
        if let TitleObjectContent::TextBox(tb) = &mut o.content {
            if tb.font_name == from {
                tb.font_name = to.to_string();
                changed = true;
            }
            for r in &mut tb.style_runs {
                if r.font_name.as_deref() == Some(from) {
                    r.font_name = Some(to.to_string());
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Whether a family is here, as the title renderer finds it: installed,
/// or a catalogue family fetched (#2732).
pub fn installed(family: &str) -> bool {
    klipp_title::font_discovery::font_installed(&klipp_title::font_discovery::discover_system_fonts(), family) || klipp_title::catalogue::fetched(family)
}

/// The fonts in `missing` that Klipp's catalogue has, taken out of it and
/// fetched in the background (#2732): a project set in a catalogue family
/// opens with it rather than asking for a replacement. A fetch that fails
/// is logged; the titles keep the fallback until the font is there.
pub fn fetch_from_catalogue(missing: Vec<MissingFont>) -> Vec<MissingFont> {
    let (fetch, rest): (Vec<MissingFont>, Vec<MissingFont>) = missing.into_iter().partition(|m| klipp_title::catalogue::family(&m.family).is_some());
    for m in fetch {
        if let Some(family) = klipp_title::catalogue::family(&m.family) {
            fetch_in_background(family);
        }
    }
    rest
}

/// `family` fetched when Klipp's catalogue has it and it is not here
/// (#2734): a font chosen from the catalogue's section of the picker.
pub fn fetch_family(family: &str) {
    if let Some(f) = klipp_title::catalogue::family(family).filter(|f| !klipp_title::catalogue::is_fetched(f)) {
        if !installed(family) {
            fetch_in_background(f);
        }
    }
}

/// The catalogue families being fetched now, so a second ask waits on
/// the first rather than fetching again.
static FETCHING: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

/// Fetches `family` on a thread of its own, once at a time; the titles
/// are drawn again when it lands (`catalogue::fetches`).
fn fetch_in_background(family: &'static klipp_title::catalogue::CatalogueFamily) {
    {
        let Ok(mut fetching) = FETCHING.lock() else { return };
        if fetching.contains(&family.name.as_str()) {
            return;
        }
        fetching.push(family.name.as_str());
    }
    let _ = std::thread::Builder::new().name("klipp-font-fetch".into()).spawn(move || {
        match klipp_title::catalogue::fetch(family) {
            Ok(dir) => tracing::info!("font {} fetched into {}", family.name, dir.display()),
            Err(e) => tracing::warn!("font {} could not be fetched: {e}", family.name),
        }
        if let Ok(mut fetching) = FETCHING.lock() {
            fetching.retain(|n| *n != family.name.as_str());
        }
    });
}

/// The catalogue families `titles` are set in that are not here yet,
/// fetched (#2734): a title an edit places from a template, from the app
/// or over the socket, is drawn in its own face as soon as it arrives.
pub fn fetch_for<'a>(titles: impl IntoIterator<Item = &'a TitleData>) {
    let mut families: Vec<String> = Vec::new();
    for td in titles {
        for f in fonts_of(td) {
            if !families.contains(&f) {
                families.push(f);
            }
        }
    }
    for name in families {
        if let Some(family) = klipp_title::catalogue::family(&name).filter(|f| !klipp_title::catalogue::is_fetched(f)) {
            if !klipp_title::font_discovery::font_installed(&klipp_title::font_discovery::discover_system_fonts(), &name) {
                fetch_in_background(family);
            }
        }
    }
}

impl AppState {
    /// The fonts this project's titles name that `installed` says are
    /// not here.
    pub fn missing_fonts(&self, installed: &dyn Fn(&str) -> bool) -> Vec<MissingFont> {
        find(&self.sequences, installed)
    }

    /// Every title in every sequence that names `from` set to `to`
    /// instead, recorded as one step in each sequence's history ("Replace
    /// Font"), so undo gives the titles their first font's name back.
    /// The titles changed, by clip.
    pub fn replace_font(&mut self, from: &str, to: &str) -> Vec<ClipId> {
        let mut changed = Vec::new();
        for s in 0..self.sequences.len() {
            let seq_id = self.sequences[s].id;
            let mut edits = Vec::new();
            for track in &mut self.sequences[s].tracks {
                for clip in &mut track.clips {
                    if let klipp_timeline::ClipKind::Title(td) = &mut clip.kind {
                        let old = td.clone();
                        if replace_in(td, from, to) {
                            edits.push(crate::history::EditCommand::EditTitle { clip_id: clip.id, description: "Replace Font", old_data: old, new_data: td.clone() });
                            changed.push(clip.id);
                        }
                    }
                }
            }
            if edits.is_empty() {
                continue;
            }
            let history = self.history_for_mut(seq_id);
            history.begin_batch();
            for command in edits {
                history.push(crate::history::HistoryEntry { command });
            }
            history.end_batch_as("Replace Font");
        }
        if !changed.is_empty() {
            self.dirty = true;
        }
        changed
    }
}

/// The dialog, while it is open: each missing font, the titles using it
/// (on hover), a picker for its replacement, Replace, and Keep (the
/// name kept, drawn in a fallback until the font is installed).
pub fn show_dialog(ctx: &egui::Context, state: &mut AppState, theme: &crate::theme::KlippTheme) -> Vec<AppCommand> {
    let Some(mut dialog) = state.missing_fonts_dialog.take() else { return Vec::new() };
    let mut cmds = Vec::new();
    let mut open = true;
    egui::Window::new("Missing fonts").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
        ui.label("These fonts are not installed. Their titles draw in a fallback until they are, or until you pick a replacement.");
        ui.add_space(6.0);
        let mut done = Vec::new();
        for font in &dialog.fonts {
            ui.separator();
            let uses = font.titles.iter().map(|t| format!("{} ({})", t.label, t.sequence)).collect::<Vec<_>>().join("\n");
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&font.family).strong());
                ui.label(format!("{} title{}", font.titles.len(), if font.titles.len() == 1 { "" } else { "s" })).on_hover_text(uses);
            });
            let chosen = dialog.chosen.get(&font.family).cloned();
            ui.horizontal(|ui| {
                ui.label("Replace with");
                let picked = ui.vertical(|ui| {
                    ui.set_width(220.0);
                    crate::inspector::font_picker::picker(ui, theme, ("missing_font", &font.family), chosen.as_deref().unwrap_or("Choose a font\u{2026}"), &font.family, &state.available_fonts, &state.favourite_fonts, &state.recent_fonts, &state.font_previews)
                }).inner;
                if let Some(f) = picked.family {
                    fetch_family(&f);
                    dialog.chosen.insert(font.family.clone(), f);
                }
                if let Some(f) = picked.toggle_favourite {
                    cmds.push(AppCommand::ToggleFavouriteFont { family: f });
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(chosen.is_some(), egui::Button::new("Replace")).clicked() {
                    if let Some(to) = &chosen {
                        cmds.push(AppCommand::ReplaceFont { from: font.family.clone(), to: to.clone() });
                        done.push(font.family.clone());
                    }
                }
                if ui.button("Keep").on_hover_text("Keep the name: the font is used again once installed").clicked() {
                    done.push(font.family.clone());
                }
            });
        }
        dialog.fonts.retain(|f| !done.contains(&f.family));
    });
    if open && !dialog.fonts.is_empty() {
        state.missing_fonts_dialog = Some(dialog);
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, ClipKind, StyleRun, TextBox};

    fn title_clip(text: &str, font: &str) -> Clip {
        let mut clip = Clip::gap(klipp_core::RationalTime::from_frames(0, 24), klipp_core::RationalTime::from_frames(48, 24));
        clip.kind = ClipKind::Title(TitleData::single_text(TextBox { text: text.into(), font_name: font.into(), ..Default::default() }));
        clip
    }

    fn here(f: &str) -> bool {
        ["Inter", "Bebas Neue", "sans-serif", ""].contains(&f)
    }

    fn font_of(state: &AppState, id: ClipId) -> String {
        state.sequences[0].all_clips().find(|c| c.id == id).unwrap().title_data().unwrap().primary_text().unwrap().font_name.clone()
    }

    /// The done-when: a project naming a font that is not here lists it
    /// with its titles; replacing it sets every title using it, in one
    /// step; undone, the titles name the missing font again.
    #[test]
    fn a_missing_font_is_listed_replaced_everywhere_and_undone_by_name() {
        let mut state = AppState::default();
        let (a, b, c) = (title_clip("Opening", "Gone Grotesk"), title_clip("Credits", "Gone Grotesk"), title_clip("Kept", "Inter"));
        let (ia, ib, ic) = (a.id, b.id, c.id);
        state.sequences[0].tracks[0].clips = vec![a, b, c];
        // A styled run naming another missing font is found too.
        if let ClipKind::Title(td) = &mut state.sequences[0].tracks[0].clips[2].kind {
            if let TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
                tb.style_runs = vec![StyleRun { font_name: Some("Lost Serif".into()), ..StyleRun::default_for_range(0, 2) }];
            }
        }
        // Saved and opened again, as a project the font is missing from.
        let dir = std::env::temp_dir().join(format!("klipp-missing-fonts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        klipp_project::save_project_with_color(&dir, &state.sequences, &klipp_media::MediaPool::new(), &Default::default(), None, None, "").unwrap();
        let (sequences, ..) = klipp_project::load_project_strict(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        state.sequences = sequences;
        let missing = state.missing_fonts(&here);
        assert_eq!(missing.iter().map(|m| m.family.as_str()).collect::<Vec<_>>(), ["Gone Grotesk", "Lost Serif"]);
        assert_eq!(missing[0].titles.iter().map(|t| (t.clip, t.label.as_str())).collect::<Vec<_>>(), [(ia, "Opening"), (ib, "Credits")]);
        assert_eq!(missing[1].titles[0].clip, ic);

        let changed = state.replace_font("Gone Grotesk", "Bebas Neue");
        assert_eq!(changed, [ia, ib]);
        assert_eq!((font_of(&state, ia), font_of(&state, ib), font_of(&state, ic)), ("Bebas Neue".into(), "Bebas Neue".into(), "Inter".into()));
        assert_eq!(state.missing_fonts(&here).len(), 1, "only the run's font is left");
        assert_eq!(state.active_history().undo_count(), 1, "one step");

        // Undone, as the app does: each title back, naming the missing font.
        let entry = state.active_history_mut().undo().expect("a step");
        let crate::history::EditCommand::Named { label, edit } = entry.command else { panic!("a named step") };
        assert_eq!(label, "Replace Font");
        let crate::history::EditCommand::Batch(edits) = *edit else { panic!("a batch") };
        for e in edits {
            let crate::history::EditCommand::EditTitle { clip_id, old_data, .. } = e else { panic!("title edits") };
            let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
            clip.kind = ClipKind::Title(old_data);
        }
        assert_eq!((font_of(&state, ia), font_of(&state, ib)), ("Gone Grotesk".into(), "Gone Grotesk".into()), "the name is kept for when the font comes back");
    }

    /// A title naming only fonts that are here, the default included,
    /// lists nothing; a run's font is replaced as the box's is.
    #[test]
    fn present_fonts_list_nothing_and_runs_are_replaced() {
        let state = AppState::default();
        assert!(state.missing_fonts(&here).is_empty());
        let mut td = TitleData::single_text(TextBox { text: "ab".into(), font_name: "sans-serif".into(), style_runs: vec![StyleRun { font_name: Some("Gone".into()), ..StyleRun::default_for_range(0, 1) }], ..Default::default() });
        assert!(replace_in(&mut td, "Gone", "Inter"));
        assert_eq!(fonts_of(&td), ["sans-serif", "Inter"]);
        assert!(!replace_in(&mut td, "Gone", "Inter"), "nothing left to replace");
    }
}
