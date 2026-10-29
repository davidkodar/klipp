//! Preferences → Brand Kits (#2722): the kits listed, the starters
//! read-only, one opened to change — its name, colours, font pairing,
//! logo and where it sits, intro, outro, lower third and end screen —
//! saved to the user's kits; a kit imported from a `.klippbrand`, or
//! exported to one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use klipp_project::brand_kit::{self, BrandKit, Bookend, Corner, Found, Logo, Source, TemplateRef};

use super::preferences::PreferencesPanel as P;
use crate::theme::KlippTheme;

/// The page's own state, kept while Preferences is open.
#[derive(Default)]
pub struct BrandKitsPage {
    /// The kits as last read; read again after a save, removal or import.
    kits: Option<Vec<Found>>,
    /// The composition templates' names, for the intro, outro, lower
    /// third and end screen.
    templates: Option<Vec<String>>,
    /// The kit open in the editor.
    draft: Option<Draft>,
    /// What the last action did, or why not.
    said: Option<String>,
}

/// A kit being changed: what it is now, the files picked for it on this
/// machine (copied into its folder on save), each file's licence, and the
/// kit it was opened from.
struct Draft {
    kit: BrandKit,
    from: Option<(String, Source)>,
    dir: Option<PathBuf>,
    picked: Vec<PathBuf>,
    licences: BTreeMap<String, String>,
}

impl Draft {
    fn of(found: &Found) -> Self {
        let licences = found.kit.files().into_iter().filter_map(|f| found.dir.as_ref().and_then(|d| std::fs::read_to_string(d.join(brand_kit::licence_name(f))).ok()).map(|t| (f.to_string(), t))).collect();
        Self { kit: found.kit.clone(), from: Some((found.kit.name.clone(), found.source)), dir: found.dir.clone(), picked: Vec::new(), licences }
    }

    fn builtin(&self) -> bool {
        matches!(self.from, Some((_, Source::Builtin)))
    }

    /// The kit's file `name` on this machine: picked now, or in the
    /// folder it was opened from.
    fn file(&self, name: &str) -> Option<PathBuf> {
        self.picked.iter().rev().find(|p| p.file_name().and_then(|n| n.to_str()) == Some(name)).cloned().or_else(|| self.dir.as_ref().map(|d| d.join(name)).filter(|p| p.is_file()))
    }
}

/// Where the user's kits are kept, under Klipp's data folder.
fn root() -> Option<PathBuf> {
    klipp_timeline::template_places::data_root()
}

/// A picker's id on this page.
fn picker(what: &str) -> egui::Id {
    egui::Id::new(("brand_kits_picker", what))
}

impl BrandKitsPage {
    pub fn show(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) {
        P::page_title(ui, "Brand Kits", theme);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("A brand's colours, fonts, logo and bookends, taken into a sequence with Timeline \u{2192} Apply Brand Kit\u{2026} or picked for a new project.").size(11.0).color(theme.text_dim));
        });
        self.collect_picks();
        let kits = self.kits.get_or_insert_with(|| brand_kit::list(root().as_deref())).clone();

        P::section_header(ui, "KITS", theme);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(8.0);
            for found in &kits {
                let open = self.draft.as_ref().and_then(|d| d.from.as_ref()).is_some_and(|(n, s)| n == &found.kit.name && *s == found.source);
                let label = if found.source == Source::Builtin { format!("{} (starter)", found.kit.name) } else { found.kit.name.clone() };
                if ui.selectable_label(open, label).clicked() {
                    self.draft = Some(Draft::of(found));
                    self.said = None;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("New Kit").clicked() {
                let name = unused_name(&kits, "My Brand");
                self.draft = Some(Draft { kit: BrandKit::new(&name), from: None, dir: None, picked: Vec::new(), licences: BTreeMap::new() });
                self.said = None;
            }
            if ui.button("Import\u{2026}").on_hover_text(format!("Add a kit shared as a .{} file", brand_kit::BRAND_EXTENSION)).clicked() {
                crate::file_dialog::pick_path(picker("import"), || rfd::FileDialog::new().set_title("Import Brand Kit").add_filter("Brand kit", &[brand_kit::BRAND_EXTENSION]).pick_file());
            }
            let exportable = self.draft.as_ref().and_then(|d| d.from.clone());
            let r = ui.add_enabled(exportable.is_some(), egui::Button::new("Export\u{2026}")).on_disabled_hover_text("Open a saved kit to export it");
            if r.on_hover_text("Write the open kit, its files and their licences, to share").clicked() {
                if let Some((name, _)) = exportable {
                    let file = format!("{}.{}", klipp_core::safe_name(&name), brand_kit::BRAND_EXTENSION);
                    crate::file_dialog::pick_path(picker("export"), move || rfd::FileDialog::new().set_title("Export Brand Kit").set_file_name(file).save_file());
                }
            }
        });
        if let Some(said) = &self.said {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(said).size(11.0).color(theme.text_secondary));
            });
        }

        let templates = self.templates.get_or_insert_with(|| crate::parameterized_templates::load_store().templates().map(|t| t.name.clone()).collect()).clone();
        let Some(draft) = self.draft.as_mut() else { return };
        let mut save = false;
        let mut remove = false;
        edit(ui, draft, &templates, theme);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if draft.builtin() {
                if ui.button("Make a Copy").on_hover_text("A starter is read-only: change a copy of it").clicked() {
                    draft.kit.name = unused_name(&kits, &format!("{} Copy", draft.kit.name));
                    draft.from = None;
                }
            } else {
                if ui.button("Save").clicked() {
                    save = true;
                }
                let saved = draft.from.is_some();
                if ui.add_enabled(saved, egui::Button::new("Delete")).clicked() {
                    remove = true;
                }
            }
        });
        if save {
            self.save();
        }
        if remove {
            self.remove();
        }
    }

    /// The answers of this page's pickers: a file for the kit, a kit to
    /// import, a place to export to.
    fn collect_picks(&mut self) {
        use crate::file_dialog::{take_path, Picked};
        for which in ["logo", "intro", "outro"] {
            if let (Some(Picked::Path(path)), Some(d)) = (take_path(picker(which)), self.draft.as_mut()) {
                let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
                match which {
                    "logo" => match &mut d.kit.logo {
                        Some(l) => l.file = name,
                        None => d.kit.logo = Some(Logo { file: name, corner: Corner::default(), margin: 0.03, scale: 0.12, opacity: 0.8 }),
                    },
                    "intro" => d.kit.intro = Some(Bookend { template: None, file: Some(name) }),
                    _ => d.kit.outro = Some(Bookend { template: None, file: Some(name) }),
                }
                d.picked.push(path);
            }
        }
        if let Some(Picked::Path(path)) = take_path(picker("import")) {
            self.said = Some(match root() {
                None => "There is no data folder to keep kits in".into(),
                Some(root) => match brand_kit::import_kit(&root, &path, false) {
                    Ok(found) => {
                        let name = found.kit.name.clone();
                        self.draft = Some(Draft::of(&found));
                        format!("Imported \"{name}\"")
                    }
                    Err(e) => format!("Not imported: {e}"),
                },
            });
            self.kits = None;
        }
        if let Some(Picked::Path(out)) = take_path(picker("export")) {
            let name = self.draft.as_ref().and_then(|d| d.from.as_ref()).map(|(n, _)| n.clone()).unwrap_or_default();
            let licences = self.draft.as_ref().map(|d| d.licences.clone()).unwrap_or_default();
            let out = if out.extension().and_then(|e| e.to_str()) == Some(brand_kit::BRAND_EXTENSION) { out } else { out.with_extension(brand_kit::BRAND_EXTENSION) };
            self.said = Some(match brand_kit::export_kit(root().as_deref(), &name, &out, &licences) {
                Ok(_) => format!("Exported \"{name}\" to {}", out.display()),
                Err(e) => format!("Not exported: {e}"),
            });
        }
    }

    /// Keep the open kit as the user's: its files copied into its folder,
    /// their licences beside them; renamed, the old one removed.
    fn save(&mut self) {
        match root() {
            Some(root) => self.save_in(&root),
            None => self.said = Some("There is no data folder to keep kits in".into()),
        }
    }

    fn save_in(&mut self, root: &std::path::Path) {
        let Some(d) = self.draft.as_mut() else { return };
        let target = brand_kit::kits_dir(&root).join(klipp_core::safe_name(&d.kit.name));
        // Files already in the folder it is saved to stay where they are:
        // copied onto themselves they would be emptied.
        let files: Vec<PathBuf> = d.kit.files().into_iter().filter_map(|f| d.file(f)).filter(|p| p.parent() != Some(target.as_path())).collect();
        self.said = Some(match brand_kit::save(root, &d.kit, &files) {
            Err(e) => format!("Not saved: {e}"),
            Ok(dir) => {
                for f in d.kit.files() {
                    if let Some(text) = d.licences.get(f).filter(|t| !t.trim().is_empty()) {
                        let _ = std::fs::write(dir.join(brand_kit::licence_name(f)), text);
                    }
                }
                let renamed = d.from.as_ref().filter(|(n, s)| *s == Source::Mine && !n.eq_ignore_ascii_case(&d.kit.name)).map(|(n, _)| n.clone());
                if let Some(old) = renamed {
                    let _ = brand_kit::remove(root, &old);
                }
                d.from = Some((d.kit.name.clone(), Source::Mine));
                d.dir = Some(dir);
                d.picked.clear();
                format!("Saved \"{}\"", d.kit.name)
            }
        });
        self.kits = None;
    }

    fn remove(&mut self) {
        let Some((name, _)) = self.draft.as_ref().and_then(|d| d.from.clone()) else { return };
        self.said = Some(match root().map(|r| brand_kit::remove(&r, &name)) {
            Some(Ok(())) => {
                self.draft = None;
                format!("Deleted \"{name}\"")
            }
            Some(Err(e)) => format!("Not deleted: {e}"),
            None => "There is no data folder".into(),
        });
        self.kits = None;
    }
}

/// `base`, or `base 2`, `base 3`… whichever no kit has.
fn unused_name(kits: &[Found], base: &str) -> String {
    let taken = |n: &str| kits.iter().any(|k| k.kit.name.eq_ignore_ascii_case(n));
    (1..).map(|i| if i == 1 { base.to_string() } else { format!("{base} {i}") }).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

/// The editor: every field of the open kit, read-only for a starter.
fn edit(ui: &mut egui::Ui, d: &mut Draft, templates: &[String], theme: &KlippTheme) {
    let enabled = !d.builtin();
    ui.add_enabled_ui(enabled, |ui| {
        P::section_header(ui, "KIT", theme);
        P::setting_row(ui, "Name", theme, |ui| {
            ui.add(egui::TextEdit::singleline(&mut d.kit.name).desired_width(220.0));
        });

        P::section_header(ui, "COLOURS", theme);
        for (label, c) in [("Primary", &mut d.kit.colours.primary), ("Secondary", &mut d.kit.colours.secondary), ("Accent", &mut d.kit.colours.accent)] {
            P::setting_row(ui, label, theme, |ui| colour(ui, c));
        }

        P::section_header(ui, "FONTS", theme);
        P::setting_row_with_subtitle(ui, "Pairing", "A heading face and a body face that sit well together", theme, |ui| {
            let now = match (&d.kit.fonts.heading, &d.kit.fonts.body) {
                (Some(h), Some(b)) => format!("{h} / {b}"),
                _ => "Choose\u{2026}".into(),
            };
            egui::ComboBox::from_id_salt("brand_kit_pairing").selected_text(now).width(220.0).height(360.0).show_ui(ui, |ui| {
                for p in klipp_title::pairings::all() {
                    let r = ui.selectable_label(d.kit.fonts.heading.as_deref() == Some(p.heading.as_str()) && d.kit.fonts.body.as_deref() == Some(p.body.as_str()), format!("{} / {}", p.heading, p.body));
                    if r.on_hover_text(p.mood.join(", ")).clicked() {
                        d.kit.fonts.heading = Some(p.heading.clone());
                        d.kit.fonts.body = Some(p.body.clone());
                    }
                }
            });
        });
        for (label, f) in [("Heading", &mut d.kit.fonts.heading), ("Body", &mut d.kit.fonts.body)] {
            P::setting_row(ui, label, theme, |ui| {
                let mut text = f.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut text).desired_width(220.0).hint_text("A font family")).changed() {
                    *f = Some(text).filter(|t| !t.trim().is_empty());
                }
            });
        }

        P::section_header(ui, "LOGO", theme);
        P::setting_row_with_subtitle(ui, "Picture", "PNG, JPEG or WebP; the watermark on a sequence", theme, |ui| {
            if ui.button("Choose\u{2026}").clicked() {
                crate::file_dialog::pick_path(picker("logo"), || rfd::FileDialog::new().set_title("Brand Kit Logo").add_filter("Picture", brand_kit::LOGO_KINDS).pick_file());
            }
            if let Some(l) = &d.kit.logo {
                ui.label(egui::RichText::new(&l.file).size(11.0).color(theme.text_secondary));
                if ui.small_button("Remove").clicked() {
                    d.kit.logo = None;
                }
            } else {
                ui.label(egui::RichText::new("None").size(11.0).color(theme.text_dim));
            }
        });
        if let Some(l) = &mut d.kit.logo {
            P::setting_row(ui, "Corner", theme, |ui| {
                egui::ComboBox::from_id_salt("brand_kit_corner").selected_text(l.corner.label()).width(160.0).show_ui(ui, |ui| {
                    for c in Corner::ALL {
                        ui.selectable_value(&mut l.corner, c, c.label());
                    }
                });
            });
            P::setting_row(ui, "Width", theme, |ui| {
                ui.add(egui::Slider::new(&mut l.scale, 0.02..=0.5).custom_formatter(|v, _| format!("{:.0}% of the frame", v * 100.0)));
            });
            P::setting_row(ui, "Margin", theme, |ui| {
                ui.add(egui::Slider::new(&mut l.margin, 0.0..=0.25).custom_formatter(|v, _| format!("{:.1}%", v * 100.0)));
            });
            P::setting_row(ui, "Opacity", theme, |ui| {
                ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
            });
            let file = l.file.clone();
            licence(ui, d, &file, theme);
        }

        P::section_header(ui, "INTRO AND OUTRO", theme);
        for which in ["intro", "outro"] {
            bookend(ui, d, which, templates, theme);
        }

        P::section_header(ui, "TEMPLATES", theme);
        for (label, t) in [("Lower third", &mut d.kit.lower_third), ("End screen", &mut d.kit.end_screen)] {
            P::setting_row(ui, label, theme, |ui| {
                let now = t.as_ref().map_or("None".to_string(), |t| t.template.clone());
                egui::ComboBox::from_id_salt(("brand_kit_template", label)).selected_text(now).width(220.0).show_ui(ui, |ui| {
                    if ui.selectable_label(t.is_none(), "None").clicked() {
                        *t = None;
                    }
                    for name in templates {
                        if ui.selectable_label(t.as_ref().is_some_and(|t| &t.template == name), name).clicked() {
                            *t = Some(TemplateRef { template: name.clone() });
                        }
                    }
                });
            });
        }
    });
}

/// A colour of the kit's: a swatch to change it, or none.
fn colour(ui: &mut egui::Ui, c: &mut Option<String>) {
    match c.as_deref().and_then(parse_hex) {
        Some(mut rgb) => {
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                *c = Some(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
            }
            ui.label(c.clone().unwrap_or_default());
            if ui.small_button("Clear").clicked() {
                *c = None;
            }
        }
        None => {
            if ui.small_button("Set").clicked() {
                *c = Some("#ffffff".into());
            }
        }
    }
}

fn parse_hex(c: &str) -> Option<[u8; 3]> {
    let h = c.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The intro or outro: none, a composition template, or a clip.
fn bookend(ui: &mut egui::Ui, d: &mut Draft, which: &str, templates: &[String], theme: &KlippTheme) {
    let label = if which == "intro" { "Intro" } else { "Outro" };
    let mut clip = None;
    P::setting_row(ui, label, theme, |ui| {
        let b = if which == "intro" { &mut d.kit.intro } else { &mut d.kit.outro };
        let now = match b {
            None => "None".to_string(),
            Some(Bookend { template: Some(t), .. }) => t.clone(),
            Some(Bookend { file: Some(f), .. }) => f.clone(),
            Some(_) => "None".to_string(),
        };
        egui::ComboBox::from_id_salt(("brand_kit_bookend", which)).selected_text(now).width(220.0).show_ui(ui, |ui| {
            if ui.selectable_label(b.is_none(), "None").clicked() {
                *b = None;
            }
            for name in templates {
                if ui.selectable_label(b.as_ref().and_then(|b| b.template.as_ref()) == Some(name), name).clicked() {
                    *b = Some(Bookend { template: Some(name.clone()), file: None });
                }
            }
        });
        if ui.button("Clip\u{2026}").on_hover_text("A video file as the bookend").clicked() {
            let title = format!("Brand Kit {label}");
            crate::file_dialog::pick_path(picker(which), move || rfd::FileDialog::new().set_title(title).add_filter("Video", brand_kit::CLIP_KINDS).pick_file());
        }
        clip = b.as_ref().and_then(|b| b.file.clone());
    });
    if let Some(file) = clip {
        licence(ui, d, &file, theme);
    }
}

/// The licence of the kit's file `file`, kept beside it and shared with
/// it: a kit is exported only with every file's.
fn licence(ui: &mut egui::Ui, d: &mut Draft, file: &str, theme: &KlippTheme) {
    P::setting_row_with_subtitle(ui, "Licence", &format!("Who may use {file}, and how: needed to export the kit"), theme, |ui| {
        let text = d.licences.entry(file.to_string()).or_default();
        ui.add(egui::TextEdit::singleline(text).desired_width(220.0).hint_text("e.g. CC BY 4.0, our own"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_kit_takes_a_name_no_kit_has() {
        let kits: Vec<Found> = brand_kit::starters().into_iter().map(|kit| Found { kit, source: Source::Builtin, dir: None }).collect();
        let first = kits[0].kit.name.clone();
        assert_eq!(unused_name(&kits, &first), format!("{first} 2"));
        assert_eq!(unused_name(&kits, "Fresh"), "Fresh");
        assert_eq!(parse_hex("#1a2B3c"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex("1a2b3c"), None);
    }

    /// A kit saved again keeps its logo, and renamed it moves to its new
    /// folder with the logo and its licence, the old folder gone: a file
    /// already in the folder is never copied onto itself (which would
    /// empty it).
    #[test]
    fn saving_again_and_renaming_keep_the_logo() {
        let tmp = std::env::temp_dir().join(format!("klipp-brand-kits-page-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let logo = tmp.join("mark.png");
        let bytes = b"\x89PNG\r\n\x1a\n not really a picture, but 41 bytes";
        std::fs::write(&logo, bytes).unwrap();
        let root = tmp.join("data");
        let mut kit = BrandKit::new("Acme");
        kit.logo = Some(Logo { file: "mark.png".into(), corner: Corner::TopLeft, margin: 0.03, scale: 0.12, opacity: 0.8 });
        let mut page = BrandKitsPage { draft: Some(Draft { kit, from: None, dir: None, picked: vec![logo], licences: BTreeMap::from([("mark.png".to_string(), "Our own".to_string())]) }), ..Default::default() };
        page.save_in(&root);
        assert_eq!(page.said.as_deref(), Some("Saved \"Acme\""));
        let first = brand_kit::get(Some(&root), "Acme").unwrap().dir.unwrap();
        page.save_in(&root);
        assert_eq!(std::fs::read(first.join("mark.png")).unwrap(), bytes, "saved again, the logo is whole");
        page.draft.as_mut().unwrap().kit.name = "Acme Studio".into();
        page.save_in(&root);
        let found = brand_kit::get(Some(&root), "Acme Studio").unwrap();
        let dir = found.dir.unwrap();
        assert_eq!(std::fs::read(dir.join("mark.png")).unwrap(), bytes);
        assert_eq!(std::fs::read_to_string(dir.join(brand_kit::licence_name("mark.png"))).unwrap(), "Our own");
        assert!(brand_kit::get(Some(&root), "Acme").is_err() && !first.exists(), "the old name is gone");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
