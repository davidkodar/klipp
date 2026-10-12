use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_project::template::ProjectTemplate;
use std::path::{Path, PathBuf};

pub const RESOLUTION_PRESETS: &[(&str, u32, u32)] = &[
    ("Automatic (from first clip)", 0, 0),
    ("720p HD", 1280, 720),
    ("1080p HD", 1920, 1080),
    ("1080p 4:3", 1440, 1080),
    ("2K", 2048, 1080),
    ("4K UHD", 3840, 2160),
    ("4K DCI", 4096, 2160),
    ("Vertical 9:16", 1080, 1920),
    ("Square 1:1", 1080, 1080),
    ("Custom...", 0, 0),
];

pub const FPS_PRESETS: &[(&str, u32, u32)] = &[
    ("Automatic (from first clip)", 0, 0),
    ("23.976 fps", 24000, 1001),
    ("24 fps", 24, 1),
    ("25 fps", 25, 1),
    ("29.97 fps", 30000, 1001),
    ("30 fps", 30, 1),
    ("48 fps", 48, 1),
    ("50 fps", 50, 1),
    ("59.94 fps", 60000, 1001),
    ("60 fps", 60, 1),
    ("120 fps", 120, 1),
];

/// Project templates that pre-fill resolution and frame rate.
pub const TEMPLATES: &[(&str, usize, usize)] = &[
    ("Custom", 0, 0),           // no preset
    ("YouTube 1080p 24fps", 2, 2),   // 1080p, 24fps
    ("YouTube 4K 30fps", 5, 5),      // 4K UHD, 30fps
    ("Instagram Square", 8, 5),      // 1080x1080, 30fps
    ("Instagram Vertical", 7, 5),    // 1080x1920, 30fps
    ("Broadcast PAL", 2, 3),         // 1080p, 25fps
    ("Broadcast NTSC", 2, 4),        // 1080p, 29.97fps
    ("Cinema 24fps", 2, 2),          // 1080p, 24fps
    ("Cinema 4K DCI", 6, 2),         // 4096x2160, 24fps
];

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
}

impl NewProjectDialog {
    pub fn new(default_location: Option<&PathBuf>) -> Self {
        let location = default_location
            .cloned()
            .or_else(|| dirs::home_dir().map(|h| h.join("Projects")))
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
        }
    }

    /// Read the user's project templates from `dir` (#618), afresh.
    pub fn load_templates(&mut self, dir: &Path) {
        self.user_templates = klipp_project::template::list_templates(dir);
        self.user_template = None;
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
                        None => TEMPLATES[self.template_idx].0.to_string(),
                    };
                    egui::ComboBox::from_id_salt("project_template")
                        .selected_text(selected)
                        .width(200.0 * control_scale)
                        // Room for the user's templates below the built-in.
                        .height(480.0 * control_scale)
                        .show_ui(ui, |ui| {
                            scale_dialog_style(ui, control_scale);
                            for (i, (label, _, _)) in TEMPLATES.iter().enumerate() {
                                if ui.selectable_label(self.user_template.is_none() && self.template_idx == i, *label).clicked() {
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
                    // Apply template when changed
                    if self.user_template.is_none() && self.template_idx != old_idx && self.template_idx > 0 {
                        let (_, res_idx, fps_idx) = TEMPLATES[self.template_idx];
                        self.resolution_idx = res_idx;
                        self.fps_idx = fps_idx;
                    }
                });
                ui.add_space(2.0);

                // A template of the user's sets the sequences, their formats
                // and tracks, the bins and the colour: what it holds,
                // where the format rows were.
                let chosen = self.chosen_template().map(|(p, t)| (p.clone(), t.summary(), t.description.clone()));
                if let Some((path, summary, description)) = chosen {
                    ui.horizontal(|ui| {
                        ui.add_space(108.0 * control_scale);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(summary).size(11.0 * control_scale).color(theme.text_secondary));
                            if !description.is_empty() {
                                ui.label(egui::RichText::new(description).size(11.0 * control_scale).color(theme.text_dim));
                            }
                            ui.label(egui::RichText::new("No media or edits: a new project with this structure.").size(10.0 * control_scale).color(theme.text_dim));
                            if ui.small_button("Delete Template").clicked() {
                                cmds.push(AppCommand::DeleteProjectTemplate { path: path.clone() });
                                if let Some(i) = self.user_template.take() {
                                    let _ = self.user_templates.remove(i);
                                }
                            }
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
                        let current_label = RESOLUTION_PRESETS[self.resolution_idx].0;
                        egui::ComboBox::from_id_salt("res_preset")
                            .selected_text(current_label)
                            .width(200.0 * control_scale)
                            .show_ui(ui, |ui| {
                                scale_dialog_style(ui, control_scale);
                                for (i, (label, _, _)) in RESOLUTION_PRESETS.iter().enumerate() {
                                    ui.selectable_value(&mut self.resolution_idx, i, *label);
                                }
                            });
                    });

                    // Custom resolution fields (shown when "Custom..." selected)
                    if self.resolution_idx == RESOLUTION_PRESETS.len() - 1 {
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
                        let current_fps_label = FPS_PRESETS[self.fps_idx].0;
                        egui::ComboBox::from_id_salt("fps_preset")
                            .selected_text(current_fps_label)
                            .width(200.0 * control_scale)
                            .show_ui(ui, |ui| {
                                scale_dialog_style(ui, control_scale);
                                for (i, (label, _, _)) in FPS_PRESETS.iter().enumerate() {
                                    ui.selectable_value(&mut self.fps_idx, i, *label);
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
                            });
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

    fn resolved_resolution(&self) -> (u32, u32) {
        if self.resolution_idx == 0 {
            // "Automatic (from first clip)" — use 0×0 as sentinel, auto-set when first clip added
            (0, 0)
        } else {
            let (_, w, h) = RESOLUTION_PRESETS[self.resolution_idx];
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
            let (_, num, den) = FPS_PRESETS[self.fps_idx];
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
/// template will hold and leave out, said before it is saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveTemplateDialog {
    pub name: String,
    pub description: String,
}

impl SaveTemplateDialog {
    /// The dialog over `summary` (what the template will hold). Pushes
    /// `SaveProjectTemplate` on Save; `false` once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, theme: &KlippTheme, summary: &str, cmds: &mut Vec<AppCommand>) -> bool {
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
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!("Keeps: {summary}")).size(11.0).color(theme.text_secondary));
                ui.label(
                    egui::RichText::new("Leaves out: media, clips and edits, markers, the playhead, caches and history.")
                        .size(11.0)
                        .color(theme.text_dim),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let named = !self.name.trim().is_empty();
                    if ui.add_enabled(named, egui::Button::new("Save Template")).on_disabled_hover_text("Name the template first").clicked() {
                        cmds.push(AppCommand::SaveProjectTemplate { name: self.name.trim().to_string(), description: self.description.trim().to_string() });
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
        click(&ctx, at(&texts, "Custom"), &mut run);
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
        let mut run = |ctx: &egui::Context, cmds: &mut Vec<AppCommand>| closed |= !dialog.show(ctx, &theme, "1 sequence, 2 tracks", cmds);
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
        assert!(matches!(&cmds[..], [AppCommand::SaveProjectTemplate { name, description }] if name == "Promo" && description.is_empty()), "{cmds:?}");
        assert!(closed, "closed once saved");
    }
}
