use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_project::template::ProjectTemplate;
use std::path::{Path, PathBuf};

// The formats, sizes and rates, Klipp's then a person's own formats
// (#3095), are read below the app: the control socket takes a format by
// name too.
pub use klipp_core::project_formats::{formats, fps_presets, resolution_presets, Format, Rate};

/// A rate as people say it: "25", "29.97", "23.976".
pub fn rate_label((num, den): Rate) -> String {
    if den == 1 {
        num.to_string()
    } else {
        let s = format!("{:.3}", num as f64 / den as f64);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The rate a format is made at: `wanted` when it offers that, else its
/// usual one; none for one that takes the first clip's.
pub fn format_rate(format: &Format, wanted: Option<Rate>) -> Option<Rate> {
    wanted.filter(|r| format.rates.contains(r)).or_else(|| format.rates.first().copied())
}

pub struct NewProjectDialog {
    pub project_name: String,
    pub location: PathBuf,
    pub resolution_idx: usize,
    pub custom_width: u32,
    pub custom_height: u32,
    pub fps_idx: usize,
    pub sequence_name: String,
    /// Track whether sequence name was manually edited (else auto-sync from project name)
    name_manually_edited: bool,
    pub template_idx: usize,
    /// The user's project templates (#618), each read or why it isn't.
    pub user_templates: Vec<(PathBuf, Result<ProjectTemplate, String>)>,
    /// The user's template chosen, by its place in `user_templates`.
    pub user_template: Option<usize>,
    /// The chosen template's new name, while it is being renamed.
    pub rename: Option<String>,
    /// What the chosen template needs, a line each, worked out once for
    /// the template it was worked out for.
    needs: Option<(PathBuf, Vec<String>)>,
    /// The brand kits there are, and the one whose look the new sequence
    /// takes (#2722).
    pub brand_kits: Vec<String>,
    pub brand_kit: Option<String>,
    /// The format a project was just made from, by name, for the app to
    /// remember (#3307).
    pub made_from: Option<String>,
}

/// The brand kits there are, by name: the user's, then the starters.
fn brand_kit_names() -> Vec<String> {
    klipp_project::brand_kit::list(klipp_timeline::template_places::data_root().as_deref()).into_iter().map(|f| f.kit.name).collect()
}

impl NewProjectDialog {
    pub fn new(default_location: Option<&PathBuf>) -> Self {
        let location = default_location
            .cloned()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_name: String::new(),
            location,
            resolution_idx: 0, // Automatic (from first clip)
            custom_width: 1920,
            custom_height: 1080,
            fps_idx: 0, // Automatic (from first clip)
            sequence_name: String::new(),
            name_manually_edited: false,
            template_idx: 0,
            user_templates: Vec::new(),
            user_template: None,
            rename: None,
            needs: None,
            brand_kits: brand_kit_names(),
            brand_kit: None,
            made_from: None,
        }
    }

    /// Open on the format named `name` when there is one by that name.
    pub fn pick_format_named(&mut self, name: Option<&str>) {
        if let Some(i) = name.and_then(|n| formats().iter().position(|f| f.name == n)) {
            self.apply_format(i);
        }
    }

    /// What the chosen template needs, worked out the first time it is
    /// shown (the system's fonts are read then).
    fn needs(&mut self) -> Vec<String> {
        let Some((path, t)) = self.chosen_template() else { return Vec::new() };
        if let Some((at, lines)) = &self.needs {
            if at == path {
                return lines.clone();
            }
        }
        let path = path.clone();
        let deps = t.dependencies(&path);
        let fonts = deps.iter().any(|d| matches!(d, klipp_project::template::Dependency::Font(_))).then(klipp_title::font_discovery::discover_system_fonts);
        let installed = |f: &str| fonts.as_ref().is_none_or(|all| klipp_title::font_discovery::font_installed(all, f));
        let lines = dependency_lines(&deps, &installed);
        self.needs = Some((path, lines.clone()));
        lines
    }

    /// Read the user's project templates from `dir` (#618), afresh.
    pub fn load_templates(&mut self, dir: &Path) {
        self.user_templates = klipp_project::template::list_templates(dir);
        self.user_template = None;
        self.rename = None;
        self.needs = None;
    }

    /// The user's template chosen, read.
    fn chosen_template(&self) -> Option<(&PathBuf, &ProjectTemplate)> {
        let (path, t) = self.user_templates.get(self.user_template?)?;
        t.as_ref().ok().map(|t| (path, t))
    }

    /// Reset the dialog fields for a fresh "New Project" invocation.
    pub fn reset(&mut self, default_location: Option<&PathBuf>) {
        self.project_name.clear();
        self.sequence_name.clear();
        self.resolution_idx = 0; // Automatic
        self.custom_width = 1920;
        self.custom_height = 1080;
        self.fps_idx = 0; // Automatic
        self.name_manually_edited = false;
        self.template_idx = 0;
        // Kits made or imported since it last opened are listed.
        self.brand_kits = brand_kit_names();
        self.brand_kit = None;
        self.user_template = None;
        if let Some(loc) = default_location {
            self.location = loc.clone();
        }
    }

    /// Show the new project dialog. Pushes `AppCommand::CreateProject{..}`
    /// when the user clicks Create, and `DeleteProjectTemplate` when a
    /// template of theirs is deleted. Sets `open` to false when closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let mut result = None;
        let mut close = false;

        let screen_width = ctx.screen_rect().width();
        let control_scale = (screen_width / 1920.0).clamp(1.0, 1.5);
        let dialog_width = (screen_width * 0.34).clamp(560.0, 760.0);
        egui::Window::new("New Project")
            .collapsible(false)
            .resizable(false)
            .default_width(dialog_width)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                scale_dialog_style(ui, control_scale);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0) * control_scale;

                // Project Name
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Project Name")
                            .size(12.0 * control_scale)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(16.0 * control_scale);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.project_name)
                            .desired_width(280.0 * control_scale)
                            .hint_text("My Project"),
                    );
                    // Auto-sync sequence name if not manually edited
                    if resp.changed() && !self.name_manually_edited {
                        self.sequence_name = self.project_name.clone();
                    }
                });

                ui.add_space(2.0);

                // Location
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Location")
                            .size(12.0 * control_scale)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(40.0 * control_scale);
                    let loc_str = self.location.to_string_lossy().to_string();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.location.to_string_lossy().to_string())
                            .desired_width(230.0 * control_scale)
                            .interactive(false)
                            .text_color(theme.text_dim),
                    );
                    let _ = loc_str; // consumed above
                    let picker = egui::Id::new("new-project-location");
                    if ui.button("Browse...").clicked() {
                        let start = self.location.clone();
                        crate::file_dialog::pick_path(picker, move || {
                            rfd::FileDialog::new()
                                .set_title("Choose Project Location")
                                .set_directory(&start)
                                .pick_folder()
                        });
                    }
                    if let Some(crate::file_dialog::Picked::Path(dir)) = crate::file_dialog::take_path(picker) {
                        self.location = dir;
                    }
                });

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(2.0);

                // Sequence Settings header
                ui.label(
                    egui::RichText::new("Sequence Settings")
                        .size(11.0 * control_scale)
                        .color(theme.text_dim),
                );

                ui.add_space(2.0);

                // Template
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Template").size(12.0 * control_scale).color(theme.text_secondary));
                    ui.add_space(38.0 * control_scale);
                    let old_idx = self.template_idx;
                    let selected = match self.chosen_template() {
                        Some((_, t)) => t.name.clone(),
                        None => formats()[self.template_idx].name.to_string(),
                    };
                    egui::ComboBox::from_id_salt("project_template")
                        .selected_text(selected)
                        .width(200.0 * control_scale)
                        // Room for the user's templates below the built-in.
                        .height(480.0 * control_scale)
                        .show_ui(ui, |ui| {
                            scale_dialog_style(ui, control_scale);
                            for (i, format) in formats().iter().enumerate() {
                                if ui.selectable_label(self.user_template.is_none() && self.template_idx == i, format.name.as_str()).clicked() {
                                    self.template_idx = i;
                                    self.user_template = None;
                                }
                            }
                            // The user's own (#618); one that doesn't read
                            // is listed, greyed, with why.
                            if !self.user_templates.is_empty() {
                                ui.separator();
                                ui.label(egui::RichText::new("Your templates").size(10.0 * control_scale).color(theme.text_dim));
                            }
                            for (i, (path, t)) in self.user_templates.iter().enumerate() {
                                match t {
                                    Ok(t) => {
                                        if ui.selectable_label(self.user_template == Some(i), &t.name).on_hover_text(t.summary()).clicked() {
                                            self.user_template = Some(i);
                                        }
                                    }
                                    Err(e) => {
                                        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                        ui.add_enabled(false, egui::SelectableLabel::new(false, format!("{name} (unreadable)"))).on_disabled_hover_text(e.as_str());
                                    }
                                }
                            }
                        });
                    // A template from another machine (#618).
                    if ui.small_button("Import...").on_hover_text("Add a project template exported on another machine").clicked() {
                        crate::file_dialog::spawn(move || {
                            rfd::FileDialog::new()
                                .set_title("Import Project Template")
                                .add_filter("Project template", &["zip"])
                                .pick_file()
                                .map(|from| vec![AppCommand::ImportProjectTemplate { from }])
                                .unwrap_or_default()
                        });
                    }
                    // Apply template when changed
                    if self.user_template.is_none() && self.template_idx != old_idx {
                        self.apply_format(self.template_idx);
                    }
                });
                ui.add_space(2.0);

                // A brand kit's look: its colours, title looks and caption
                // look in the new sequence (#2722).
                if !self.brand_kits.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Brand kit").size(12.0 * control_scale).color(theme.text_secondary));
                        ui.add_space(39.0 * control_scale);
                        egui::ComboBox::from_id_salt("project_brand_kit")
                            .selected_text(self.brand_kit.clone().unwrap_or_else(|| "None".into()))
                            .width(200.0 * control_scale)
                            .show_ui(ui, |ui| {
                                scale_dialog_style(ui, control_scale);
                                ui.selectable_value(&mut self.brand_kit, None, "None");
                                for name in &self.brand_kits {
                                    ui.selectable_value(&mut self.brand_kit, Some(name.clone()), name);
                                }
                            })
                            .response
                            .on_hover_text("Its colours, title looks and caption look in the new sequence; Timeline \u{2192} Apply Brand Kit\u{2026} adds its intro, outro and logo");
                    });
                    ui.add_space(2.0);
                }

                // A template of the user's sets the sequences, their formats
                // and tracks, the bins and the colour: what it holds,
                // where the format rows were.
                let chosen = self.chosen_template().map(|(p, t)| (p.clone(), t.summary(), t.description.clone(), t.starter.as_ref().map(|s| s.sequences.join(", "))));
                if let Some((path, summary, description, starter)) = chosen {
                    let needs = self.needs();
                    ui.horizontal(|ui| {
                        ui.add_space(108.0 * control_scale);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(summary).size(11.0 * control_scale).color(theme.text_secondary));
                            if !description.is_empty() {
                                ui.label(egui::RichText::new(description).size(11.0 * control_scale).color(theme.text_dim));
                            }
                            match starter {
                                Some(seqs) => ui.label(egui::RichText::new(format!("Starts with: {seqs}")).size(10.0 * control_scale).color(theme.text_secondary)),
                                None => ui.label(egui::RichText::new("No media or edits: a new project with this structure.").size(10.0 * control_scale).color(theme.text_dim)),
                            };
                            // What it needs, missing things said before the
                            // project is made.
                            for line in &needs {
                                let missing = line.contains("missing") || line.contains("not installed");
                                ui.label(egui::RichText::new(line).size(10.0 * control_scale).color(if missing { theme.accent } else { theme.text_dim }));
                            }
                            ui.horizontal(|ui| {
                                match self.rename.as_mut() {
                                    Some(new_name) => {
                                        ui.add(egui::TextEdit::singleline(new_name).desired_width(140.0 * control_scale));
                                        if ui.small_button("Rename").clicked() {
                                            cmds.push(AppCommand::RenameProjectTemplate { path: path.clone(), name: new_name.trim().to_string() });
                                            self.rename = None;
                                        }
                                    }
                                    None => {
                                        if ui.small_button("Rename...").clicked() {
                                            self.rename = self.chosen_template().map(|(_, t)| t.name.clone());
                                        }
                                    }
                                }
                                if ui.small_button("Export...").clicked() {
                                    let from = path.clone();
                                    let file = format!("{}.zip", path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "template".into()));
                                    crate::file_dialog::spawn(move || {
                                        rfd::FileDialog::new()
                                            .set_title("Export Project Template")
                                            .add_filter("Project template", &["zip"])
                                            .set_file_name(&file)
                                            .save_file()
                                            .map(|to| vec![AppCommand::ExportProjectTemplate { path: from, to }])
                                            .unwrap_or_default()
                                    });
                                }
                                if ui.small_button("Delete Template").clicked() {
                                    cmds.push(AppCommand::DeleteProjectTemplate { path: path.clone() });
                                    if let Some(i) = self.user_template.take() {
                                        let _ = self.user_templates.remove(i);
                                    }
                                }
                            });
                        });
                    });
                } else {

                    // Resolution
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Resolution")
                                .size(12.0 * control_scale)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(30.0 * control_scale);
                        let current_label = resolution_presets()[self.resolution_idx].0.as_str();
                        egui::ComboBox::from_id_salt("res_preset")
                            .selected_text(current_label)
                            .width(200.0 * control_scale)
                            .show_ui(ui, |ui| {
                                scale_dialog_style(ui, control_scale);
                                for (i, (label, _, _)) in resolution_presets().iter().enumerate() {
                                    ui.selectable_value(&mut self.resolution_idx, i, label.as_str());
                                }
                            });
                    });

                    // Custom resolution fields (shown when "Custom..." selected)
                    if self.resolution_idx == resolution_presets().len() - 1 {
                        ui.horizontal(|ui| {
                            ui.add_space(108.0 * control_scale);
                            let field_height = ui.spacing().interact_size.y;
                            ui.label(
                                egui::RichText::new("W")
                                    .size(11.0 * control_scale)
                                    .color(theme.text_dim),
                            );
                            ui.add_sized(
                                egui::vec2(48.0 * control_scale, field_height),
                                egui::DragValue::new(&mut self.custom_width).range(64..=8192).speed(2),
                            );
                            ui.label(
                                egui::RichText::new("H")
                                    .size(11.0 * control_scale)
                                    .color(theme.text_dim),
                            );
                            ui.add_sized(
                                egui::vec2(48.0 * control_scale, field_height),
                                egui::DragValue::new(&mut self.custom_height).range(64..=8192).speed(2),
                            );
                        });
                    }

                    // Frame Rate
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Frame Rate")
                                .size(12.0 * control_scale)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(26.0 * control_scale);
                        let current_fps_label = fps_presets()[self.fps_idx].0.as_str();
                        egui::ComboBox::from_id_salt("fps_preset")
                            .selected_text(current_fps_label)
                            .width(200.0 * control_scale)
                            .show_ui(ui, |ui| {
                                scale_dialog_style(ui, control_scale);
                                for (i, (label, _, _)) in fps_presets().iter().enumerate() {
                                    ui.selectable_value(&mut self.fps_idx, i, label.as_str());
                                }
                            });
                    });

                    // Sequence Name
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Sequence Name")
                                .size(12.0 * control_scale)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(8.0 * control_scale);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.sequence_name)
                                .desired_width(280.0 * control_scale)
                                .hint_text("Main Sequence"),
                        );
                        if resp.changed() {
                            self.name_manually_edited = true;
                        }
                    });
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                // Buttons
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = self.project_name.trim();
                        let can_create = !name.is_empty();

                        // Why it is disabled, said out loud.
                        //
                        // The name field shows "My Project" as a hint,
                        // not a value, so the form looks filled in
                        // while the button refuses to work and nothing
                        // says why. The export dialog already answers
                        // this the same way.
                        let create_btn = ui
                            .add_enabled(
                                can_create,
                                egui::Button::new(
                                    egui::RichText::new("Create Project").size(13.0 * control_scale),
                                ),
                            )
                            .on_disabled_hover_text("Give the project a name first");
                        if create_btn.clicked() && can_create {
                            let (width, height) = self.resolved_resolution();
                            let (fps_num, fps_den) = self.resolved_fps();
                            let seq_name = if self.sequence_name.trim().is_empty() {
                                name.to_string()
                            } else {
                                self.sequence_name.trim().to_string()
                            };

                            result = Some(AppCommand::CreateProject {
                                name: sanitize_filename(name),
                                location: self.location.clone(),
                                fps_num,
                                fps_den,
                                width,
                                height,
                                sequence_name: seq_name,
                                template: self.chosen_template().map(|(p, _)| p.clone()),
                                hdr: self.user_template.is_none() && formats().get(self.template_idx).is_some_and(|f| f.hdr),
                                brand_kit: self.brand_kit.clone(),
                                destination: formats().get(self.template_idx).filter(|_| self.user_template.is_none()).and_then(|f| f.destination.clone()),
                            });
                            self.made_from = formats().get(self.template_idx).filter(|_| self.user_template.is_none()).map(|f| f.name.clone());
                            close = true;
                        }

                        if ui.button(egui::RichText::new("Cancel").size(11.0 * control_scale)).clicked() {
                            close = true;
                        }

                        // And said in the open, because hover text has
                        // to be found before it can help. Only while it
                        // is disabled, so it never nags at a form that
                        // is ready to submit.
                        if !can_create {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("Name the project to continue")
                                    .size(11.0 * control_scale)
                                    .color(theme.text_dim),
                            );
                        }
                    });
                });
            });

        if close {
            *open = false;
        }
        cmds.extend(result);
    }

    /// Set the format rows from built-in format `i`: its picture and
    /// usual rate, or both from the first clip.
    pub fn apply_format(&mut self, i: usize) {
        let Some(format) = formats().get(i) else { return };
        self.template_idx = i;
        self.resolution_idx = resolution_presets().iter().position(|(_, w, h)| (*w, *h) == (format.width, format.height) && *w > 0).unwrap_or(0);
        self.fps_idx = format_rate(format, None)
            .and_then(|r| fps_presets().iter().position(|(_, n, d)| (*n, *d) == r))
            .unwrap_or(0);
    }

    fn resolved_resolution(&self) -> (u32, u32) {
        if self.resolution_idx == 0 {
            // "Automatic (from first clip)" — use 0×0 as sentinel, auto-set when first clip added
            (0, 0)
        } else {
            let (_, w, h) = resolution_presets()[self.resolution_idx];
            if w == 0 && h == 0 {
                // Custom
                (self.custom_width.max(64), self.custom_height.max(64))
            } else {
                (w, h)
            }
        }
    }

    fn resolved_fps(&self) -> (u32, u32) {
        if self.fps_idx == 0 {
            // "Automatic" — use 0/0 as sentinel, auto-set from first clip
            (0, 0)
        } else {
            let (_, num, den) = fps_presets()[self.fps_idx];
            (num, den)
        }
    }
}

fn scale_dialog_style(ui: &mut egui::Ui, scale: f32) {
    for font in ui.style_mut().text_styles.values_mut() {
        font.size *= scale;
    }
    ui.spacing_mut().interact_size.y *= scale;
}

/// Replace characters that are unsafe in filenames.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Save Project as Template (#618): a name, a description, and what the
/// template will hold and leave out, said before it is saved; starter
/// content opt-in: sequences kept whole, their media copied in or linked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveTemplateDialog {
    pub name: String,
    pub description: String,
    /// Keep chosen sequences whole as starter content.
    pub include_starter: bool,
    /// The sequences kept whole.
    pub starter: Vec<klipp_core::SequenceId>,
    /// Copy their media into the template, rather than link it.
    pub package_media: bool,
}

/// What the Save Project as Template dialog shows of the open project.
pub struct SaveTemplateView {
    /// What the template keeps, in a line.
    pub summary: String,
    /// The project's sequences, to choose starter content from.
    pub sequences: Vec<(klipp_core::SequenceId, String)>,
    /// What the starter content chosen needs from outside, a line each.
    pub needs: Vec<String>,
    /// The names of the templates there already.
    pub existing: Vec<String>,
}

impl SaveTemplateDialog {
    /// The dialog over `view`. Pushes `SaveProjectTemplate` on Save;
    /// `false` once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, theme: &KlippTheme, view: &SaveTemplateView, cmds: &mut Vec<AppCommand>) -> bool {
        let mut keep = true;
        egui::Window::new("Save Project as Template")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("save-template-fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new("Name").color(theme.text_secondary));
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(280.0).hint_text("Weekly Series"));
                    ui.end_row();
                    ui.label(egui::RichText::new("Description").color(theme.text_secondary));
                    ui.add(egui::TextEdit::singleline(&mut self.description).desired_width(280.0).hint_text("Optional"));
                    ui.end_row();
                });
                // Saving under a name there already updates that template:
                // projects made from it before are not touched.
                if view.existing.iter().any(|n| n.eq_ignore_ascii_case(self.name.trim())) {
                    ui.label(egui::RichText::new(format!("Updates the template \"{}\"; projects made from it keep what they have.", self.name.trim())).size(11.0).color(theme.accent));
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!("Keeps: {}", view.summary)).size(11.0).color(theme.text_secondary));
                ui.add_space(4.0);
                ui.checkbox(&mut self.include_starter, "Include starter content");
                if self.include_starter {
                    ui.indent("starter", |ui| {
                        ui.label(egui::RichText::new("Sequences kept whole, clips and all:").size(11.0).color(theme.text_secondary));
                        for (id, name) in &view.sequences {
                            let mut on = self.starter.contains(id);
                            if ui.checkbox(&mut on, name).changed() {
                                self.starter.retain(|s| s != id);
                                if on {
                                    self.starter.push(*id);
                                }
                            }
                        }
                        ui.radio_value(&mut self.package_media, false, "Link their media where it is (relinked on another machine)");
                        ui.radio_value(&mut self.package_media, true, "Copy their media into the template");
                        for line in &view.needs {
                            ui.label(egui::RichText::new(line).size(11.0).color(theme.text_dim));
                        }
                    });
                }
                ui.label(
                    egui::RichText::new(if self.include_starter && !self.starter.is_empty() {
                        "Leaves out: the other sequences' clips, markers, the playhead, caches and history."
                    } else {
                        "Leaves out: media, clips and edits, markers, the playhead, caches and history."
                    })
                    .size(11.0)
                    .color(theme.text_dim),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let named = !self.name.trim().is_empty();
                    if ui.add_enabled(named, egui::Button::new("Save Template")).on_disabled_hover_text("Name the template first").clicked() {
                        let starter = if self.include_starter { self.starter.clone() } else { Vec::new() };
                        cmds.push(AppCommand::SaveProjectTemplate { name: self.name.trim().to_string(), description: self.description.trim().to_string(), starter, package_media: self.package_media });
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        keep
    }
}

/// What a template's starter content needs, a line each: its media
/// (copied in or linked, and any missing), its LUT, fonts and plug-ins.
/// `installed` says whether a font family is on this machine.
pub fn dependency_lines(deps: &[klipp_project::template::Dependency], installed: &dyn Fn(&str) -> bool) -> Vec<String> {
    use klipp_project::template::Dependency as D;
    let copied = deps.iter().filter(|d| matches!(d, D::PackagedMedia(_))).count();
    let linked: Vec<&D> = deps.iter().filter(|d| matches!(d, D::LinkedMedia { .. })).collect();
    let mut out = Vec::new();
    let files = |n: usize| if n == 1 { "1 media file".to_string() } else { format!("{n} media files") };
    if copied > 0 {
        out.push(format!("{}, copied in", files(copied)));
    }
    if !linked.is_empty() {
        let missing = linked.iter().filter(|d| d.missing()).count();
        out.push(if missing > 0 { format!("{}, linked: {missing} missing, to relink", files(linked.len())) } else { format!("{}, linked", files(linked.len())) });
    }
    for d in deps {
        match d {
            D::Lut { path, present } => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(if *present { format!("LUT {name}") } else { format!("LUT {name} (missing)") });
            }
            D::Font(f) => out.push(if installed(f) { format!("Font {f}") } else { format!("Font {f} (not installed)") }),
            D::Plugin(p) => out.push(format!("Plug-in {p}")),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use klipp_project::template::{ProjectSetup, TemplateColour};

    /// A frame of `run` with `events`: the commands pushed and the texts
    /// drawn, each at its middle.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, run: &mut dyn FnMut(&egui::Context, &mut Vec<AppCommand>)) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>) {
        let mut cmds = Vec::new();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), events, ..Default::default() }, |ctx| run(ctx, &mut cmds));
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (cmds, texts)
    }

    fn click(ctx: &egui::Context, at: egui::Pos2, run: &mut dyn FnMut(&egui::Context, &mut Vec<AppCommand>)) -> Vec<AppCommand> {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, vec![egui::Event::PointerMoved(at), button(true)], run).0;
        cmds.extend(frame(ctx, vec![button(false)], run).0);
        cmds.extend(frame(ctx, Vec::new(), run).0);
        cmds
    }

    fn at(texts: &[(String, egui::Pos2)], label: &str) -> egui::Pos2 {
        texts.iter().find(|(t, _)| t == label).map(|(_, p)| *p).unwrap_or_else(|| panic!("{label} is drawn: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>()))
    }

    /// A templates folder holding "Weekly Series" (two sequences).
    fn templates() -> std::path::PathBuf {
        let mut a = klipp_timeline::Sequence::new_default();
        a.name = "Episode".into();
        let b = klipp_timeline::Sequence::new_default();
        let colour = TemplateColour { working_colorspace: Default::default(), grade_encoding: Default::default(), global_lut_path: None, global_lut_opacity: 1.0, global_lut_enabled: false };
        let pool = klipp_media::MediaPool::new();
        let dir = std::env::temp_dir().join(format!("klipp-dialog-templates-{}", uuid::Uuid::new_v4()));
        ProjectTemplate::capture("Weekly Series", "Every episode", &ProjectSetup { sequences: &[a, b], pool: &pool, colour }).save(&dir).unwrap();
        dir
    }

    /// #2887: the picture sizes and frame rates the dialogs offer come
    /// from Klipp's file, as the code listed them, with "Automatic" first
    /// and "Custom..." last for the sizes.
    #[test]
    fn the_sizes_and_rates_read_as_they_were() {
        let sizes: Vec<(&str, u32, u32)> = resolution_presets().iter().map(|(n, w, h)| (n.as_str(), *w, *h)).collect();
        assert_eq!(sizes, [
            ("Automatic (from first clip)", 0, 0), ("720p HD", 1280, 720), ("1080p HD", 1920, 1080), ("1080p 4:3", 1440, 1080), ("2K", 2048, 1080),
            ("4K UHD", 3840, 2160), ("4K DCI", 4096, 2160), ("Vertical 9:16", 1080, 1920), ("Square 1:1", 1080, 1080), ("Custom...", 0, 0),
        ]);
        let rates: Vec<(&str, u32, u32)> = fps_presets().iter().map(|(n, a, b)| (n.as_str(), *a, *b)).collect();
        assert_eq!(rates, [
            ("Automatic (from first clip)", 0, 0), ("23.976 fps", 24000, 1001), ("24 fps", 24, 1), ("25 fps", 25, 1), ("29.97 fps", 30000, 1001),
            ("30 fps", 30, 1), ("48 fps", 48, 1), ("50 fps", 50, 1), ("59.94 fps", 60000, 1001), ("60 fps", 60, 1), ("120 fps", 120, 1),
        ]);
        let f = &formats()[6];
        assert_eq!((f.name.as_str(), f.width, f.height, f.rates.as_slice(), f.hdr), ("HDR UHD (HLG / PQ)", 3840, 2160, &[(25, 1), (30000, 1001)][..], true));
    }

    /// #1864: the built-in formats are the purpose-named list, and no two
    /// are the same format: the same picture, gamut and a rate in common.
    #[test]
    fn the_formats_are_named_for_their_purpose_and_never_repeat() {
        let names: Vec<_> = formats().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Match first clip", "HD 1080p", "UHD 4K", "DCI 4K (cinema)", "Vertical (social)", "Square (social)", "HDR UHD (HLG / PQ)", "YouTube", "YouTube Shorts / Reels / TikTok", "Square post", "Podcast"]);
        // A destination (#3307) is its own format though its picture and
        // rate are another's: it also sets where the piece goes, its
        // loudness and captions ("Square post" beside "Square (social)").
        for (i, a) in formats().iter().enumerate() {
            for b in &formats()[i + 1..] {
                let same_picture = (a.width, a.height, a.hdr) == (b.width, b.height, b.hdr) && a.destination == b.destination;
                let shared_rate = a.rates.iter().any(|r| b.rates.contains(r)) || (a.rates.is_empty() && b.rates.is_empty());
                assert!(!(same_picture && shared_rate), "{} and {} are the same format", a.name, b.name);
            }
        }
        assert!(formats()[1].rates.contains(&(24000, 1001)), "23.976 is offered");
        assert_eq!(rate_label((30000, 1001)), "29.97");
        assert_eq!(rate_label((24000, 1001)), "23.976");
        assert_eq!(rate_label((25, 1)), "25");
    }

    /// #1864: the dialog offers the same formats, and one chosen sets
    /// the project made: its picture, its usual rate, and HDR.
    #[test]
    fn the_dialog_makes_the_chosen_format() {
        let mut dialog = NewProjectDialog::new(None);
        dialog.project_name = "Clip".into();
        for (i, f) in formats().iter().enumerate() {
            dialog.apply_format(i);
            assert_eq!(dialog.resolved_resolution(), (f.width, f.height), "{}", f.name);
            assert_eq!(dialog.resolved_fps(), f.rates.first().copied().unwrap_or((0, 0)), "{}", f.name);
        }
        // And drawn: HDR UHD chosen from the list, Create makes it.
        let ctx = egui::Context::default();
        let mut dialog = NewProjectDialog::new(None);
        dialog.project_name = "Clip".into();
        let mut open = true;
        let theme = KlippTheme::default();
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| dialog.show(ctx, &mut open, &theme, cmds);
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        click(&ctx, at(&texts, "Match first clip"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        click(&ctx, at(&texts, "HDR UHD (HLG / PQ)"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Create Project"), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::CreateProject { width: 3840, height: 2160, fps_num: 25, fps_den: 1, hdr: true, template: None, destination: None, .. }]), "{cmds:?}");
        // #3307: a destination chosen from the same list carries its
        // platform to the new project.
        let mut dialog = NewProjectDialog::new(None);
        dialog.project_name = "Clip".into();
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| dialog.show(ctx, &mut open, &theme, cmds);
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        click(&ctx, at(&texts, "Match first clip"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        click(&ctx, at(&texts, "YouTube Shorts / Reels / TikTok"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Create Project"), &mut run);
        match &cmds[..] {
            [AppCommand::CreateProject { width: 1080, height: 1920, fps_num: 30, destination: Some(d), .. }] => assert!(d.captions && d.platform.contains("TikTok"), "{d:?}"),
            other => panic!("{other:?}"),
        }
        // And remembered (#3307): the format made from is handed over, and
        // a dialog reset and told it opens on it; a name no format has
        // leaves it on Match first clip.
        assert_eq!(dialog.made_from.as_deref(), Some("YouTube Shorts / Reels / TikTok"));
        let mut again = NewProjectDialog::new(None);
        again.reset(None);
        again.pick_format_named(dialog.made_from.as_deref());
        assert_eq!((formats()[again.template_idx].name.as_str(), again.resolved_resolution(), again.resolved_fps()), ("YouTube Shorts / Reels / TikTok", (1080, 1920), (30, 1)));
        again.reset(None);
        again.pick_format_named(Some("Gone"));
        assert_eq!(again.template_idx, 0);
    }

    /// #618: New Project lists the user's templates; chosen, one shows
    /// what it holds in place of the format rows, and Create starts the
    /// project from it. Delete Template deletes it.
    #[test]
    fn new_project_starts_from_a_chosen_template() {
        let dir = templates();
        let ctx = egui::Context::default();
        let mut dialog = NewProjectDialog::new(Some(&dir));
        dialog.load_templates(&dir);
        dialog.project_name = "Episode 12".into();
        let mut open = true;
        let theme = KlippTheme::default();
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| dialog.show(ctx, &mut open, &theme, cmds);
        // A window lays itself out on its first frames.
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(texts.iter().any(|(t, _)| t == "Resolution"), "the format rows show for a built-in choice");
        // The template combo, then the user's template in it.
        click(&ctx, at(&texts, "Match first clip"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(texts.iter().any(|(t, _)| t == "Your templates"));
        click(&ctx, at(&texts, "Weekly Series"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(!texts.iter().any(|(t, _)| t == "Resolution"), "the template sets the format");
        assert!(texts.iter().any(|(t, _)| t.starts_with("2 sequences, 4 tracks")), "{:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
        assert!(texts.iter().any(|(t, _)| t == "Every episode"));
        let cmds = click(&ctx, at(&texts, "Create Project"), &mut run);
        match &cmds[..] {
            [AppCommand::CreateProject { name, template: Some(path), .. }] => {
                assert_eq!(name, "Episode 12");
                assert_eq!(ProjectTemplate::load(path).unwrap().name, "Weekly Series");
            }
            other => panic!("one CreateProject from the template: {other:?}"),
        }
        drop(run);
        // Delete, from the dialog opened again.
        let mut open = true;
        dialog.load_templates(&dir);
        dialog.user_template = Some(0);
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| dialog.show(ctx, &mut open, &theme, cmds);
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Delete Template"), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::DeleteProjectTemplate { path }] if path.parent() == Some(dir.as_path())), "{cmds:?}");
        drop(run);
        assert!(dialog.user_templates.is_empty() && dialog.user_template.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// #618: Save Project as Template says what it keeps and leaves out,
    /// and saves under the name typed.
    #[test]
    fn save_as_template_names_it_and_says_what_it_keeps() {
        let ctx = egui::Context::default();
        let mut dialog = SaveTemplateDialog::default();
        let theme = KlippTheme::default();
        let mut closed = false;
        let view = SaveTemplateView { summary: "1 sequence, 2 tracks".into(), sequences: Vec::new(), needs: Vec::new(), existing: Vec::new() };
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| closed |= !dialog.show(ctx, &theme, &view, cmds);
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(texts.iter().any(|(t, _)| t == "Keeps: 1 sequence, 2 tracks"));
        assert!(texts.iter().any(|(t, _)| t.starts_with("Leaves out: media")));
        assert!(click(&ctx, at(&texts, "Save Template"), &mut run).is_empty(), "no name, no save");
        click(&ctx, at(&texts, "Weekly Series"), &mut run);
        frame(&ctx, vec![egui::Event::Text("Promo".into())], &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Save Template"), &mut run);
        drop(run);
        assert!(matches!(&cmds[..], [AppCommand::SaveProjectTemplate { name, description, starter, .. }] if name == "Promo" && description.is_empty() && starter.is_empty()), "{cmds:?}");
        assert!(closed, "closed once saved");
    }

    /// #618 part 2: starter content is opt-in: ticked, the project's
    /// sequences are offered, one kept whole and its media copied in is
    /// what Save sends; what it needs is listed, and a name there already
    /// says it updates that template.
    #[test]
    fn save_as_template_takes_starter_content_when_asked() {
        let ctx = egui::Context::default();
        let mut dialog = SaveTemplateDialog { name: "Weekly".into(), ..Default::default() };
        let theme = KlippTheme::default();
        let (ep, spare) = (klipp_core::SequenceId::new(), klipp_core::SequenceId::new());
        let view = SaveTemplateView {
            summary: "2 sequences".into(),
            sequences: vec![(ep, "Episode".into()), (spare, "Spare".into())],
            needs: vec!["2 media files, linked".into(), "Font Bebas Neue".into()],
            existing: vec!["Weekly".into()],
        };
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| {
            dialog.show(ctx, &theme, &view, cmds);
        };
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(texts.iter().any(|(t, _)| t.starts_with("Updates the template \"Weekly\"")), "a name there already is an update");
        assert!(!texts.iter().any(|(t, _)| t == "Episode"), "no sequences offered until asked");
        click(&ctx, at(&texts, "Include starter content"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        for line in ["Episode", "Spare", "2 media files, linked", "Font Bebas Neue"] {
            assert!(texts.iter().any(|(t, _)| t == line), "{line}: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
        }
        click(&ctx, at(&texts, "Episode"), &mut run);
        click(&ctx, at(&texts, "Copy their media into the template"), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Save Template"), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::SaveProjectTemplate { name, starter, package_media: true, .. }] if name == "Weekly" && starter == &vec![ep]), "{cmds:?}");
    }

    /// #618 parts 2 and 3: a template with starter content says what it
    /// starts with and what it needs, a linked file that is gone marked
    /// missing; it is renamed in place, and Import is offered.
    #[test]
    fn a_starter_template_says_what_it_needs_and_is_renamed() {
        let root = std::env::temp_dir().join(format!("klipp-dialog-starter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("shot.mov");
        std::fs::write(&file, b"x").unwrap();
        let mut pool = klipp_media::MediaPool::new();
        let id = pool.add(klipp_media::MediaItem::synthetic_for_test(file.clone(), None));
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.name = "Episode".into();
        seq.tracks[0].clips = vec![klipp_timeline::Clip::media(id, tc(0.0), tc(1.0), tc(0.0), tc(1.0))];
        let colour = TemplateColour { working_colorspace: Default::default(), grade_encoding: Default::default(), global_lut_path: None, global_lut_opacity: 1.0, global_lut_enabled: false };
        let sequences = [seq];
        let setup = ProjectSetup { sequences: &sequences, pool: &pool, colour };
        let choice = klipp_project::template::StarterChoice { sequences: vec![sequences[0].id], package_media: false };
        let (t, files) = ProjectTemplate::capture_with_starter("Show", "", &setup, &choice);
        let dir = root.join("templates");
        t.save_with_starter(&dir, files.as_ref()).unwrap();
        std::fs::remove_file(&file).unwrap();

        let ctx = egui::Context::default();
        let mut dialog = NewProjectDialog::new(Some(&root));
        dialog.load_templates(&dir);
        dialog.user_template = Some(0);
        let mut open = true;
        let theme = KlippTheme::default();
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| dialog.show(ctx, &mut open, &theme, cmds);
        frame(&ctx, Vec::new(), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        assert!(texts.iter().any(|(t, _)| t == "Starts with: Episode"), "{:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
        assert!(texts.iter().any(|(t, _)| t == "1 media file, linked: 1 missing, to relink"));
        assert!(texts.iter().any(|(t, _)| t == "Import..."));
        click(&ctx, at(&texts, "Rename..."), &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        // The field holds the name: select it all and type the new one.
        // The field, below the combo that names the template too.
        let field = texts.iter().filter(|(t, _)| t == "Show").map(|(_, p)| *p).max_by(|a, b| a.y.total_cmp(&b.y)).expect("the name field");
        click(&ctx, field, &mut run);
        let select_all = egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        frame(&ctx, vec![select_all, egui::Event::Text("Weekly Show".into())], &mut run);
        let (_, texts) = frame(&ctx, Vec::new(), &mut run);
        let cmds = click(&ctx, at(&texts, "Rename"), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::RenameProjectTemplate { name, .. }] if name == "Weekly Show"), "{cmds:?}");
        drop(run);
        let _ = std::fs::remove_dir_all(root);
    }
}
