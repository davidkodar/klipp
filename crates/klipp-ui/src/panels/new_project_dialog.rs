use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::path::PathBuf;

pub const RESOLUTION_PRESETS: &[(&str, u32, u32)] = &[
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
            resolution_idx: 1, // 1080p HD
            custom_width: 1920,
            custom_height: 1080,
            fps_idx: 1, // 24 fps
            sequence_name: String::new(),
            name_manually_edited: false,
        }
    }

    /// Reset the dialog fields for a fresh "New Project" invocation.
    pub fn reset(&mut self, default_location: Option<&PathBuf>) {
        self.project_name.clear();
        self.sequence_name.clear();
        self.resolution_idx = 1;
        self.custom_width = 1920;
        self.custom_height = 1080;
        self.fps_idx = 1;
        self.name_manually_edited = false;
        if let Some(loc) = default_location {
            self.location = loc.clone();
        }
    }

    /// Show the new project dialog. Returns `Some(AppCommand::CreateProject{..})` when
    /// the user clicks Create, or `None` otherwise. Sets `open` to false when closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        theme: &KlippTheme,
    ) -> Option<AppCommand> {
        let mut result = None;
        let mut close = false;

        egui::Window::new("New Project")
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);

                // Project Name
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Project Name")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(16.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.project_name)
                            .desired_width(280.0)
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
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(40.0);
                    let loc_str = self.location.to_string_lossy().to_string();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.location.to_string_lossy().to_string())
                            .desired_width(230.0)
                            .interactive(false)
                            .text_color(theme.text_dim),
                    );
                    let _ = loc_str; // consumed above
                    if ui.button("Browse...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Choose Project Location")
                            .set_directory(&self.location)
                            .pick_folder()
                        {
                            self.location = dir;
                        }
                    }
                });

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(2.0);

                // Sequence Settings header
                ui.label(
                    egui::RichText::new("Sequence Settings")
                        .size(11.0)
                        .color(theme.text_dim),
                );

                ui.add_space(2.0);

                // Resolution
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Resolution")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(30.0);
                    let current_label = RESOLUTION_PRESETS[self.resolution_idx].0;
                    egui::ComboBox::from_id_salt("res_preset")
                        .selected_text(current_label)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, (label, _, _)) in RESOLUTION_PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut self.resolution_idx, i, *label);
                            }
                        });
                });

                // Custom resolution fields (shown when "Custom..." selected)
                if self.resolution_idx == RESOLUTION_PRESETS.len() - 1 {
                    ui.horizontal(|ui| {
                        ui.add_space(108.0);
                        ui.label(
                            egui::RichText::new("W")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.custom_width)
                                .range(64..=8192)
                                .speed(2),
                        );
                        ui.label(
                            egui::RichText::new("H")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.custom_height)
                                .range(64..=8192)
                                .speed(2),
                        );
                    });
                }

                // Frame Rate
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Frame Rate")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(26.0);
                    let current_fps_label = FPS_PRESETS[self.fps_idx].0;
                    egui::ComboBox::from_id_salt("fps_preset")
                        .selected_text(current_fps_label)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, (label, _, _)) in FPS_PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut self.fps_idx, i, *label);
                            }
                        });
                });

                // Sequence Name
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Sequence Name")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    ui.add_space(8.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.sequence_name)
                            .desired_width(280.0)
                            .hint_text("Main Sequence"),
                    );
                    if resp.changed() {
                        self.name_manually_edited = true;
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(4.0);

                // Buttons
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = self.project_name.trim();
                        let can_create = !name.is_empty();

                        let create_btn = ui.add_enabled(
                            can_create,
                            egui::Button::new(
                                egui::RichText::new("Create Project").size(13.0),
                            ),
                        );
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
                            });
                            close = true;
                        }

                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });

        if close {
            *open = false;
        }
        result
    }

    fn resolved_resolution(&self) -> (u32, u32) {
        let (_, w, h) = RESOLUTION_PRESETS[self.resolution_idx];
        if w == 0 && h == 0 {
            // Custom
            (self.custom_width.max(64), self.custom_height.max(64))
        } else {
            (w, h)
        }
    }

    fn resolved_fps(&self) -> (u32, u32) {
        let (_, num, den) = FPS_PRESETS[self.fps_idx];
        (num, den)
    }
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
