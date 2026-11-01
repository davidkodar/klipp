//! Spelling in the text editors (#3236): the words a dictionary does not
//! know underlined as they are typed, and a right-click on one offering
//! what it might have been, or to take it as right in this project or in
//! every one.
//!
//! The checking is `klipp_spelling`'s; this keeps what the editors draw
//! from it. A dictionary is read off the frame the first time its
//! language is asked for, so the first underline comes a moment after
//! the text, never a stall. What a text marks is kept by its language and
//! words, and dropped when the words the project knows change.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use klipp_core::AppCommand;
use klipp_spelling::{Dictionaries, Known};

/// The most texts whose marks are kept.
const KEPT: usize = 512;

/// What the editors draw spelling from. Its caches sit behind a lock, so
/// an editor that only reads the app's state can draw marks.
pub struct SpellCheck {
    /// Off, nothing is underlined.
    pub enabled: bool,
    dicts: &'static Dictionaries,
    inner: std::sync::Mutex<Inner>,
    /// The dictionaries installed, as tags, read once.
    installed: std::sync::OnceLock<Vec<String>>,
}

#[derive(Default)]
struct Inner {
    known: Known,
    /// What `known` was made from, to tell when to make it again.
    known_key: Option<u64>,
    /// Each text's marks, by its language and words.
    marks: HashMap<(String, String), Arc<Vec<(usize, usize)>>>,
}

impl Default for SpellCheck {
    fn default() -> Self {
        Self::with(Dictionaries::shared())
    }
}

impl SpellCheck {
    /// Against `dicts` rather than the system's.
    pub fn with(dicts: &'static Dictionaries) -> Self {
        Self { enabled: true, dicts, inner: Default::default(), installed: Default::default() }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The words the project knows made again when the brief's
    /// vocabulary or cast, the media or the person's list changed.
    pub fn follow_project(&self, brief: Option<&klipp_timeline::Brief>, pool: &klipp_media::MediaPool) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let Some(b) = brief {
            b.vocabulary.hash(&mut h);
            for c in &b.cast {
                c.name.hash(&mut h);
            }
        }
        for m in pool.all() {
            m.filename.hash(&mut h);
        }
        let key = h.finish();
        let mut inner = self.inner();
        if inner.known_key == Some(key) {
            return;
        }
        let personal = klipp_spelling::personal::default_path().map(|p| klipp_spelling::personal::read(&p)).unwrap_or_default();
        let files: Vec<String> = pool.all().map(|m| m.filename.clone()).collect();
        inner.known = Known::new(brief, files.iter().chain(&personal).map(String::as_str));
        inner.known_key = Some(key);
        inner.marks.clear();
    }

    /// The person's word list changed: read it again.
    pub fn personal_changed(&self) {
        self.inner().known_key = None;
    }

    /// The words to underline in `text`, as byte ranges, in `language`.
    /// None while off, while the dictionary is still being read (`ctx`
    /// is asked to draw again soon), or when there is none.
    pub fn marks(&self, ctx: &egui::Context, text: &str, language: &str) -> Arc<Vec<(usize, usize)>> {
        if !self.enabled || text.trim().is_empty() {
            return Arc::default();
        }
        let key = (language.to_string(), text.to_string());
        if let Some(m) = self.inner().marks.get(&key) {
            return m.clone();
        }
        let dict = match self.dicts.ready(language) {
            None => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return Arc::default();
            }
            Some(Err(_)) => None,
            Some(Ok(d)) => Some(d),
        };
        let mut inner = self.inner();
        let found: Vec<(usize, usize)> = dict.map(|d| klipp_spelling::misspelt(text, &d, &inner.known).into_iter().map(|w| (w.start, w.end)).collect()).unwrap_or_default();
        if inner.marks.len() >= KEPT {
            inner.marks.clear();
        }
        let found = Arc::new(found);
        inner.marks.insert(key, found.clone());
        found
    }

    /// The languages there is a dictionary for, as tags (`de-DE`), read
    /// from the system once.
    pub fn installed(&self) -> &[String] {
        self.installed.get_or_init(|| self.dicts.installed().into_iter().map(|stem| stem.replace('_', "-")).collect())
    }

    /// Why `language` is not checked, once its dictionary has been looked
    /// for and none read; `None` while it is read, or when it was.
    pub fn missing(&self, language: &str) -> Option<klipp_spelling::Missing> {
        match self.dicts.ready(language) {
            Some(Err(m)) => Some(m),
            _ => None,
        }
    }

    /// What `word` might have been in `language`, likeliest first.
    pub fn suggestions(&self, word: &str, language: &str) -> Vec<String> {
        match self.dicts.ready(language) {
            Some(Ok(d)) => klipp_spelling::suggestions(word, &d, klipp_spelling::MAX_SUGGESTIONS),
            _ => Vec::new(),
        }
    }
}

/// A word marked somewhere in the titles and captions, for the Check
/// Spelling dialog (#3236): its sequence, title, text layer, bytes, the
/// word, its language, where the title starts and the line it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub sequence: usize,
    pub clip_id: klipp_core::ClipId,
    pub object: klipp_timeline::TitleObjectId,
    pub start: usize,
    pub end: usize,
    pub word: String,
    pub language: String,
    pub at_secs: f64,
    pub line: String,
}

/// Every word marked in `scope`'s sequences, in timeline order within
/// each, and the languages not checked (said once each). A language
/// still being read adds nothing yet: the next frame has it.
pub fn find_all(spell: &SpellCheck, ctx: &egui::Context, sequences: &[klipp_timeline::Sequence], scope: &[usize]) -> (Vec<Found>, Vec<String>) {
    let fallback = klipp_spelling::system_language();
    let (mut out, mut missing) = (Vec::new(), Vec::new());
    for &si in scope {
        let Some(seq) = sequences.get(si) else { continue };
        let mut here = Vec::new();
        for track in &seq.tracks {
            for clip in &track.clips {
                let Some(td) = clip.title_data() else { continue };
                for o in &td.objects {
                    let klipp_timeline::TitleObjectContent::TextBox(tb) = &o.content else { continue };
                    let language = klipp_spelling::language_of(tb, track, seq.brief.as_ref(), &fallback);
                    if let Some(m) = spell.missing(&language) {
                        let said = m.say();
                        if !missing.contains(&said) {
                            missing.push(said);
                        }
                        continue;
                    }
                    for &(start, end) in spell.marks(ctx, &tb.text, &language).iter() {
                        let line_start = tb.text[..start].rfind('\n').map_or(0, |i| i + 1);
                        let line_end = tb.text[end..].find('\n').map_or(tb.text.len(), |i| end + i);
                        here.push(Found { sequence: si, clip_id: clip.id, object: o.id, start, end, word: tb.text[start..end].to_string(), language: language.clone(), at_secs: clip.timeline_in.to_secs_f64(), line: tb.text[line_start..line_end].to_string() });
                    }
                }
            }
        }
        here.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs).then(a.start.cmp(&b.start)));
        out.extend(here);
    }
    (out, missing)
}

/// The brief with `word` on its vocabulary: what Ignore in This Project
/// makes (#3236). `None` when it is not one word, or already known.
pub fn ignored(brief: Option<&klipp_timeline::Brief>, word: &str) -> Option<klipp_timeline::Brief> {
    let word = word.trim();
    if !klipp_spelling::personal::is_word(word) || Known::new(brief, []).contains(word) {
        return None;
    }
    let mut brief = brief.cloned().unwrap_or_default();
    brief.vocabulary.push(word.to_string());
    Some(brief)
}

/// Every text of `td` set to `language` (#3236), `None` its track's.
pub fn set_language(td: &mut klipp_timeline::TitleData, language: Option<&str>) {
    for o in &mut td.objects {
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
            tb.language = language.map(str::to_string);
        }
    }
}

/// The language a title's text is checked in (#3236): its own, its
/// track's, the brief's, else the system's.
pub fn language_of(seq: &klipp_timeline::Sequence, clip_id: klipp_core::ClipId, object: Option<klipp_timeline::TitleObjectId>) -> String {
    let fallback = klipp_spelling::system_language();
    for track in &seq.tracks {
        let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) else { continue };
        let tb = clip.title_data().and_then(|td| {
            td.objects
                .iter()
                .filter(|o| object.is_none_or(|id| o.id == id))
                .find_map(|o| match &o.content {
                    klipp_timeline::TitleObjectContent::TextBox(tb) => Some(tb),
                    _ => None,
                })
        });
        let default = klipp_timeline::TextBox::default();
        return klipp_spelling::language_of(tb.unwrap_or(&default), track, seq.brief.as_ref(), &fallback);
    }
    seq.brief.as_ref().and_then(|b| b.language.clone()).unwrap_or(fallback)
}

/// A language picker's row (#3236): the text's own language, or `None`
/// for its track's (`inherited`, shown as Auto). What was picked, if
/// anything: `Some(None)` back to Auto.
pub fn language_picker(ui: &mut egui::Ui, id: impl Hash, spell: &SpellCheck, own: Option<&str>, inherited: &str) -> Option<Option<String>> {
    let mut picked = None;
    let auto = format!("Auto ({inherited})");
    egui::ComboBox::from_id_salt(id)
        .selected_text(own.unwrap_or(&auto))
        .show_ui(ui, |ui| {
            if ui.selectable_label(own.is_none(), &auto).on_hover_text("The track's language, else the brief's, else the system's").clicked() {
                picked = Some(None);
            }
            let installed = spell.installed();
            if installed.is_empty() {
                ui.add_enabled(false, egui::Label::new("No dictionaries installed"));
            }
            for tag in installed {
                if ui.selectable_label(own == Some(tag.as_str()), tag).clicked() {
                    picked = Some(Some(tag.clone()));
                }
            }
        });
    picked
}

/// `text` laid out in `font` and `color`, the `marks` underlined in
/// `mark`: a text editor's layouter.
pub fn layout_job(text: &str, wrap_width: f32, font: egui::FontId, color: egui::Color32, marks: &[(usize, usize)], mark: egui::Color32) -> egui::text::LayoutJob {
    let plain = egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let marked = egui::TextFormat { underline: egui::Stroke::new(1.5_f32, mark), ..plain.clone() };
    let mut job = egui::text::LayoutJob { wrap: egui::text::TextWrapping { max_width: wrap_width, ..Default::default() }, ..Default::default() };
    let mut at = 0;
    for &(a, b) in marks.iter().filter(|(a, b)| a < b && *b <= text.len() && text.is_char_boundary(*a) && text.is_char_boundary(*b)) {
        if a < at {
            continue;
        }
        job.append(&text[at..a], 0.0, plain.clone());
        job.append(&text[a..b], 0.0, marked.clone());
        at = b;
    }
    job.append(&text[at..], 0.0, plain);
    job
}

/// `text` with `start..end` put as `with`.
pub fn replaced(text: &str, start: usize, end: usize, with: &str) -> Option<String> {
    (start <= end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)).then(|| format!("{}{with}{}", &text[..start], &text[end..]))
}

/// The marked word a right-click landed on: its bytes, the word and what
/// it might have been. Kept while its menu is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub start: usize,
    pub end: usize,
    pub word: String,
    pub suggestions: Vec<String>,
}

/// The mark holding byte `at` of the text, if any.
pub fn mark_at(marks: &[(usize, usize)], at: usize) -> Option<(usize, usize)> {
    marks.iter().copied().find(|&(a, b)| a <= at && at <= b)
}

/// What a click on the menu asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// The word put as this.
    Replace(String),
    Ignore,
    Learn,
}

/// The menu's rows for `target`: its suggestions, then Ignore and Add to
/// Dictionary. What was clicked, if anything.
pub fn menu(ui: &mut egui::Ui, target: &Target) -> Option<Choice> {
    let mut chose = None;
    if target.suggestions.is_empty() {
        ui.add_enabled(false, egui::Button::new("No suggestions"));
    }
    for s in &target.suggestions {
        if ui.button(egui::RichText::new(s).strong()).clicked() {
            chose = Some(Choice::Replace(s.clone()));
        }
    }
    ui.separator();
    if ui.button("Ignore in This Project").on_hover_text("Taken as right in this project: it goes on the brief's vocabulary").clicked() {
        chose = Some(Choice::Ignore);
    }
    if ui.button("Add to Dictionary").on_hover_text("Taken as right in every project").clicked() {
        chose = Some(Choice::Learn);
    }
    if chose.is_some() {
        ui.close_menu();
    }
    chose
}

/// A choice made into what it does: Ignore and Add to Dictionary as
/// their commands; a replacement as the text it makes, for the editor to
/// set as it sets typing.
pub fn act(choice: Choice, target: &Target, text: &str, cmds: &mut Vec<AppCommand>) -> Option<String> {
    match choice {
        Choice::Replace(with) => replaced(text, target.start, target.end, &with),
        Choice::Ignore => {
            cmds.push(AppCommand::SpellingIgnore { word: target.word.clone() });
            None
        }
        Choice::Learn => {
            cmds.push(AppCommand::SpellingLearn { word: target.word.clone() });
            None
        }
    }
}

/// A text editor with spelling: a multi-line editor of `text`, set up
/// by `configure`, shown with the marks of its text in `language`
/// underlined, and a right-click on one opening its menu. The editor's
/// output, and the text a chosen suggestion makes, which the caller sets
/// as it sets typing.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    spell: &SpellCheck,
    text: &mut String,
    configure: impl for<'t> FnOnce(egui::TextEdit<'t>) -> egui::TextEdit<'t>,
    language: &str,
    font: egui::FontId,
    color: egui::Color32,
    mark: egui::Color32,
    cmds: &mut Vec<AppCommand>,
) -> (egui::text_edit::TextEditOutput, Option<String>) {
    let ctx = ui.ctx().clone();
    let mut layouter = |ui: &egui::Ui, text: &str, wrap: f32| {
        let marks = spell.marks(&ctx, text, language);
        let job = layout_job(text, wrap, font.clone(), color, &marks, mark);
        ui.fonts(|f| f.layout_job(job))
    };
    let output = configure(egui::TextEdit::multiline(text)).layouter(&mut layouter).show(ui);
    let text = output.galley.text().to_string();
    let id = output.response.id.with("spelling_target");
    if output.response.secondary_clicked() {
        let target = ui.input(|i| i.pointer.interact_pos()).and_then(|pos| {
            let cursor = output.galley.cursor_from_pos(pos - output.galley_pos);
            let at = text.char_indices().nth(cursor.ccursor.index).map_or(text.len(), |(b, _)| b);
            let marks = spell.marks(&ctx, &text, language);
            let (start, end) = mark_at(&marks, at)?;
            let word = text[start..end].to_string();
            Some(Target { start, end, suggestions: spell.suggestions(&word, language), word })
        });
        ui.data_mut(|d| match &target {
            Some(t) => d.insert_temp(id, t.clone()),
            None => d.remove::<Target>(id),
        });
    }
    let mut replacement = None;
    if let Some(target) = ui.data(|d| d.get_temp::<Target>(id)) {
        output.response.context_menu(|ui| {
            if let Some(choice) = menu(ui, &target) {
                replacement = act(choice, &target, &text, cmds);
            }
        });
        // Kept while its menu is open (what `context_menu` gives back is
        // no sign of that).
        if !output.response.context_menu_opened() && !output.response.secondary_clicked() {
            ui.data_mut(|d| d.remove::<Target>(id));
        }
    }
    (output, replacement)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_layout_underlines_only_the_marks() {
        let job = layout_job("Teh dog", 100.0, egui::FontId::proportional(13.0), egui::Color32::WHITE, &[(0, 3)], egui::Color32::RED);
        let sections: Vec<(&str, bool)> = job.sections.iter().map(|s| (&job.text[s.byte_range.clone()], s.format.underline != egui::Stroke::NONE)).collect();
        assert_eq!(sections, [("", false), ("Teh", true), (" dog", false)]);
        // A mark past the text, or inside a letter, is not drawn.
        let job = layout_job("é", 100.0, egui::FontId::proportional(13.0), egui::Color32::WHITE, &[(1, 2), (0, 9)], egui::Color32::RED);
        assert!(job.sections.iter().all(|s| s.format.underline == egui::Stroke::NONE));
    }

    /// The small test dictionaries klipp-spelling keeps, read before the
    /// first frame.
    pub(crate) fn test_spelling() -> SpellCheck {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-spelling/tests/dictionaries");
        let dicts: &'static Dictionaries = Box::leak(Box::new(Dictionaries::in_dirs(vec![dir], None)));
        let start = std::time::Instant::now();
        while dicts.ready("en").is_none() {
            assert!(start.elapsed().as_secs() < 10, "the dictionary was never read");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        SpellCheck::with(dicts)
    }

    /// #3236: in an editor, the misspelt word is underlined and the rest
    /// not; a right-click on it opens its menu, and the suggestion clicked
    /// is put in its place; Ignore and Add to Dictionary send their
    /// commands; a right-click on a word spelt right opens nothing.
    #[test]
    fn a_misspelt_word_is_underlined_and_its_menu_puts_a_suggestion() {
        let spell = test_spelling();
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |text: &mut String, events: Vec<egui::Event>| {
            clock += 0.1;
            let mut cmds = Vec::new();
            let mut got = None;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), time: Some(clock), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (o, respelt) = show(ui, &spell, text, |e| e.desired_width(300.0), "en", egui::FontId::proportional(14.0), egui::Color32::WHITE, egui::Color32::RED, &mut cmds);
                    got = Some((o.galley.clone(), o.galley_pos, respelt));
                });
            });
            let texts: Vec<(String, egui::Rect)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                _ => None,
            }).collect();
            let (galley, at, respelt) = got.unwrap();
            (galley, at, respelt, cmds, texts)
        };
        let mut text = "Teh lazy dog".to_string();
        let (galley, at, ..) = frame(&mut text, Vec::new());
        let underlined: Vec<&str> = galley.job.sections.iter().filter(|s| s.format.underline != egui::Stroke::NONE).map(|s| &galley.job.text[s.byte_range.clone()]).collect();
        assert_eq!(underlined, ["Teh"]);

        // A click as a person makes one: pressed in a frame, let go in the
        // next. What either frame gave back.
        let mut click = |text: &mut String, pos: egui::Pos2, button: egui::PointerButton| {
            let mut respelt = None;
            let mut cmds = Vec::new();
            for pressed in [true, false] {
                let (_, _, r, c, _) = frame(text, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() }]);
                respelt = respelt.or(r);
                cmds.extend(c);
            }
            let (.., texts) = frame(text, Vec::new());
            (respelt, cmds, texts)
        };

        // On "dog", spelt right: no menu.
        let on = |galley: &egui::Galley, at: egui::Pos2, char_index: usize| at + galley.pos_from_cursor(&galley.from_ccursor(egui::text::CCursor::new(char_index))).center().to_vec2() + egui::vec2(2.0, 0.0);
        let (.., texts) = click(&mut text, on(&galley, at, 9), egui::PointerButton::Secondary);
        assert!(!texts.iter().any(|(t, _)| t == "Ignore in This Project"), "no menu over a word spelt right");

        // On "Teh": its menu, with what it might be.
        let (.., texts) = click(&mut text, on(&galley, at, 1), egui::PointerButton::Secondary);
        let the = texts.iter().find(|(t, _)| t == "The").map(|(_, r)| r.center()).expect("the suggestion in the menu");
        assert!(texts.iter().any(|(t, _)| t == "Add to Dictionary"));
        let (respelt, ..) = click(&mut text, the, egui::PointerButton::Primary);
        assert_eq!(respelt.as_deref(), Some("The lazy dog"));

        // Ignore sends its command.
        let (.., texts) = click(&mut text, on(&galley, at, 1), egui::PointerButton::Secondary);
        let ignore = texts.iter().find(|(t, _)| t == "Ignore in This Project").map(|(_, r)| r.center()).expect("Ignore");
        let (respelt, cmds, _) = click(&mut text, ignore, egui::PointerButton::Primary);
        assert_eq!(respelt, None);
        assert!(matches!(&cmds[..], [AppCommand::SpellingIgnore { word }] if word == "Teh"), "{cmds:?}");
    }

    /// #3236: off, nothing is underlined; a word the project knows is not
    /// either, from the frame its brief says so.
    #[test]
    fn nothing_is_underlined_when_off_or_known() {
        let mut spell = test_spelling();
        let ctx = egui::Context::default();
        assert_eq!(*spell.marks(&ctx, "Teh Kestrelholm dog", "en"), [(0, 3), (4, 15)]);
        let brief = klipp_timeline::Brief { vocabulary: vec!["Kestrelholm".into()], ..Default::default() };
        spell.follow_project(Some(&brief), &klipp_media::MediaPool::new());
        assert_eq!(*spell.marks(&ctx, "Teh Kestrelholm dog", "en"), [(0, 3)]);
        spell.enabled = false;
        assert!(spell.marks(&ctx, "Teh Kestrelholm dog", "en").is_empty());
    }

    /// #3236: Ignore puts the word on the brief's vocabulary once, and not
    /// what is not a word; a title's language goes on all its texts.
    #[test]
    fn ignore_goes_on_the_brief_and_a_language_on_every_text() {
        let brief = ignored(None, " Kestrelholm ").expect("a brief made");
        assert_eq!(brief.vocabulary, ["Kestrelholm"]);
        assert_eq!(ignored(Some(&brief), "kestrelholm"), None, "known already, in any case");
        assert_eq!(ignored(Some(&brief), "two words"), None);
        let cast = klipp_timeline::Brief { cast: vec![klipp_timeline::CastMember { name: "Ana Ruiz".into(), ..Default::default() }], ..Default::default() };
        assert_eq!(ignored(Some(&cast), "Ruiz"), None, "a cast name is known");
        assert_eq!(ignored(Some(&cast), "Omarchy").map(|b| (b.vocabulary, b.cast.len())), Some((vec!["Omarchy".to_string()], 1)), "the rest of the brief kept");

        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox::default());
        td.objects.push(td.objects[0].clone());
        set_language(&mut td, Some("sv"));
        assert!(td.objects.iter().all(|o| matches!(&o.content, klipp_timeline::TitleObjectContent::TextBox(tb) if tb.language.as_deref() == Some("sv"))));
        set_language(&mut td, None);
        assert!(td.primary_text().unwrap().language.is_none());
    }

    #[test]
    fn a_choice_does_what_it_says() {
        let target = Target { start: 4, end: 7, word: "teh".into(), suggestions: vec!["the".into()] };
        let mut cmds = Vec::new();
        assert_eq!(act(Choice::Replace("the".into()), &target, "See teh dog", &mut cmds).as_deref(), Some("See the dog"));
        assert!(cmds.is_empty());
        assert_eq!(act(Choice::Ignore, &target, "See teh dog", &mut cmds), None);
        assert_eq!(act(Choice::Learn, &target, "See teh dog", &mut cmds), None);
        assert!(matches!(&cmds[..], [AppCommand::SpellingIgnore { word: a }, AppCommand::SpellingLearn { word: b }] if a == "teh" && b == "teh"));
        assert_eq!(mark_at(&[(4, 7)], 7), Some((4, 7)), "a click just after the word is on it");
        assert_eq!(mark_at(&[(4, 7)], 2), None);
        assert_eq!(replaced("abc", 1, 9, "x"), None);
    }
}
