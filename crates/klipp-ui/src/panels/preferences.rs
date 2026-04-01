use crate::prefs::Preferences;
use klipp_core::{AppCommand, ExportPreset, ExportPresetSettings, ProxyResolution, builtin_presets};
use klipp_core::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportVideoCodec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefsTab {
    General,
    System,
    Appearance,
    Playback,
    Editing,
    Audio,
    AudioHardware,
    Export,
    MediaCache,
    Color,
    Title,
    Timeline,
    Viewer,
    Proxy,
    Advanced,
    Keyboard,
}

pub struct PreferencesPanel {
    active_tab: PrefsTab,
    // New preset dialog state
    new_preset_name: String,
    show_new_preset: bool,
    editing_preset_id: Option<String>,
    /// Action currently being remapped (waiting for key press). (action_name, combo_index)
    remapping: Option<(String, usize)>,
    /// Search filter for keyboard shortcuts
    kb_search: String,
    // Editing preset fields
    edit_format: ExportFormat,
    edit_video_codec: ExportVideoCodec,
    edit_crf: u32,
    edit_preset: EncoderPreset,
    edit_audio_codec: ExportAudioCodec,
    edit_audio_bitrate: u32,
    edit_res_w: u32,
    edit_res_h: u32,
    edit_use_custom_res: bool,
}

impl Default for PreferencesPanel {
    fn default() -> Self {
        Self {
            active_tab: PrefsTab::General,
            new_preset_name: String::new(),
            show_new_preset: false,
            editing_preset_id: None,
            remapping: None,
            kb_search: String::new(),
            edit_format: ExportFormat::Mp4,
            edit_video_codec: ExportVideoCodec::H264,
            edit_crf: 23,
            edit_preset: EncoderPreset::default(),
            edit_audio_codec: ExportAudioCodec::Aac,
            edit_audio_bitrate: 192,
            edit_res_w: 1920,
            edit_res_h: 1080,
            edit_use_custom_res: false,
        }
    }
}

impl PreferencesPanel {
    /// Show the preferences window. Returns (still_open, commands).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        prefs: &mut Preferences,
        prefs_dirty: &mut bool,
        key_bindings: &mut crate::keybindings::KeyBindings,
    ) -> (bool, Vec<AppCommand>) {
        let mut open = true;
        let mut cmds = Vec::new();

        // Modal backdrop — block interaction with background
        egui::Area::new(egui::Id::new("prefs_modal_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::PanelResizeLine)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
                if resp.clicked() {
                    open = false;
                }
            });

        let screen = ctx.screen_rect();
        egui::Window::new("Preferences")
            .open(&mut open)
            .default_width(550.0)
            .default_height(500.0)
            .min_width(400.0)
            .min_height(300.0)
            .resizable(true)
            .collapsible(false)
            .default_pos(egui::pos2(screen.center().x - 275.0, screen.center().y - 250.0))
            .show(ctx, |ui| {
                // Tab bar
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (PrefsTab::General, "General"),
                        (PrefsTab::System, "System"),
                        (PrefsTab::Appearance, "Appearance"),
                        (PrefsTab::Playback, "Playback"),
                        (PrefsTab::Editing, "Editing"),
                        (PrefsTab::Audio, "Audio"),
                        (PrefsTab::AudioHardware, "Audio HW"),
                        (PrefsTab::Export, "Export"),
                        (PrefsTab::MediaCache, "Cache"),
                        (PrefsTab::Color, "Color"),
                        (PrefsTab::Title, "Title"),
                        (PrefsTab::Timeline, "Timeline"),
                        (PrefsTab::Viewer, "Viewer"),
                        (PrefsTab::Proxy, "Proxy"),
                        (PrefsTab::Advanced, "Advanced"),
                        (PrefsTab::Keyboard, "Keyboard"),
                    ] {
                        let style = if self.active_tab == tab {
                            egui::RichText::new(label).strong().size(13.0)
                                .color(egui::Color32::from_rgb(0x0a, 0x0a, 0x0c))
                        } else {
                            egui::RichText::new(label)
                                .size(13.0)
                                .color(egui::Color32::from_rgb(190, 190, 200))
                        };
                        if ui.selectable_label(self.active_tab == tab, style).clicked() {
                            self.active_tab = tab;
                        }
                    }
                });
                ui.separator();
                ui.add_space(8.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    match self.active_tab {
                        PrefsTab::General => self.show_general(ui, prefs, prefs_dirty),
                        PrefsTab::Playback => self.show_playback(ui, prefs, prefs_dirty),
                        PrefsTab::Editing => self.show_editing(ui, prefs, prefs_dirty, &mut cmds),
                        PrefsTab::Audio => self.show_audio(ui, prefs, prefs_dirty),
                        PrefsTab::Export => self.show_export(ui, prefs, prefs_dirty, &mut cmds),
                        PrefsTab::Proxy => self.show_proxy(ui, prefs, prefs_dirty),
                        PrefsTab::System => { ui.heading("System / Performance"); ui.label("GPU preference, decode threads, playback resolution — coming in next update."); }
                        PrefsTab::Appearance => { ui.heading("Appearance"); ui.label("Accent color, UI text size, role colors — coming in next update."); }
                        PrefsTab::AudioHardware => { ui.heading("Audio Hardware"); ui.label("Output device, sample rate, buffer size — coming in next update."); }
                        PrefsTab::MediaCache => { ui.heading("Media Cache"); ui.label("Cache directories, size limits, transcode quality — coming in next update."); }
                        PrefsTab::Color => { ui.heading("Color"); ui.label("Working color space, viewer LUT, scope settings — coming in next update."); }
                        PrefsTab::Title => { ui.heading("Titles"); ui.label("Default font, size, search paths — coming in next update."); }
                        PrefsTab::Timeline => { ui.heading("Timeline"); ui.label("Clip colors, gap spacing, track heights, snap — coming in next update."); }
                        PrefsTab::Viewer => { ui.heading("Viewer"); ui.label("Safe areas, grid, checkerboard, handles — coming in next update."); }
                        PrefsTab::Advanced => { ui.heading("Advanced"); ui.label("Toast duration, backup count, anti-click fade, pre-roll — coming in next update."); }
                        PrefsTab::Keyboard => self.show_keyboard(ui, key_bindings),
                    }
                });
            });

        (open, cmds)
    }

    fn show_general(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
    ) {
        ui.label(egui::RichText::new("Auto-Save").strong().size(12.0));
        ui.add_space(4.0);

        let intervals = [
            (0, "Off"),
            (60, "Every 1 minute"),
            (300, "Every 5 minutes"),
            (600, "Every 10 minutes"),
            (1800, "Every 30 minutes"),
        ];
        let current_label = intervals
            .iter()
            .find(|(v, _)| *v == prefs.general.auto_save_interval_secs)
            .map(|(_, l)| *l)
            .unwrap_or("Custom");

        egui::ComboBox::from_id_salt("autosave_interval")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for (val, label) in &intervals {
                    if ui
                        .selectable_value(
                            &mut prefs.general.auto_save_interval_secs,
                            *val,
                            *label,
                        )
                        .changed()
                    {
                        *dirty = true;
                    }
                }
            });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Default Project Folder").strong().size(12.0));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let dir_text = prefs
                .general
                .default_project_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "(not set)".into());
            ui.label(&dir_text);
            if ui.button("Browse...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Default Project Folder")
                    .pick_folder()
                {
                    prefs.general.default_project_dir = Some(path);
                    *dirty = true;
                }
            }
            if prefs.general.default_project_dir.is_some() && ui.small_button("Clear").clicked() {
                prefs.general.default_project_dir = None;
                *dirty = true;
            }
        });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Recent Projects").strong().size(12.0));
        ui.add_space(4.0);

        if prefs.general.recent_projects.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(80, 80, 90),
                "No recent projects",
            );
        } else {
            ui.label(format!("{} recent project(s)", prefs.general.recent_projects.len()));
            if ui.small_button("Clear List").clicked() {
                prefs.general.recent_projects.clear();
                *dirty = true;
            }
        }

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Inspector").strong().size(12.0));
        ui.add_space(4.0);

        if ui.checkbox(&mut prefs.general.remember_inspector_layout, "Remember collapsed sections")
            .on_hover_text("When enabled, inspector sections you collapse stay collapsed across restarts. When disabled, all sections open fresh each launch.")
            .changed()
        {
            *dirty = true;
        }
    }

    fn show_playback(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
    ) {
        ui.label(egui::RichText::new("Audio").strong().size(12.0));
        ui.add_space(4.0);

        if ui
            .checkbox(&mut prefs.playback.scrub_audio, "Scrub audio during timeline drag")
            .changed()
        {
            *dirty = true;
        }
        if ui
            .checkbox(&mut prefs.playback.skim_audio, "Audio during skimming (Shift+S)")
            .changed()
        {
            *dirty = true;
        }

        ui.add_space(8.0);
        ui.label("Skim resolution");
        ui.add_space(4.0);

        let resolutions: [(u32, &str); 4] = [
            (240, "240p (fastest)"),
            (480, "480p (default)"),
            (720, "720p"),
            (1080, "1080p (sharpest)"),
        ];
        for (res, label) in &resolutions {
            if ui
                .radio(prefs.playback.skim_resolution == *res, *label)
                .clicked()
            {
                prefs.playback.skim_resolution = *res;
                *dirty = true;
            }
        }

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Playback").strong().size(12.0));
        ui.add_space(4.0);
        if ui
            .checkbox(&mut prefs.playback.loop_playback, "Loop playback (Ctrl+L)")
            .changed()
        {
            *dirty = true;
        }

        ui.add_space(8.0);
        ui.label("JKL audio mute threshold");
        ui.add_space(4.0);

        let thresholds = [
            (2.0_f32, "2\u{00d7} speed"),
            (4.0, "4\u{00d7} speed"),
            (8.0, "8\u{00d7} speed"),
            (0.0, "Always mute"),
        ];
        let current_label = thresholds
            .iter()
            .find(|(v, _)| (*v - prefs.playback.jkl_audio_mute_threshold).abs() < 0.01)
            .map(|(_, l)| *l)
            .unwrap_or("Custom");

        egui::ComboBox::from_id_salt("jkl_threshold")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for (val, label) in &thresholds {
                    if ui
                        .selectable_value(
                            &mut prefs.playback.jkl_audio_mute_threshold,
                            *val,
                            *label,
                        )
                        .changed()
                    {
                        *dirty = true;
                    }
                }
            });
    }

    fn show_editing(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        cmds: &mut Vec<AppCommand>,
    ) {
        ui.label(egui::RichText::new("Timeline").strong().size(12.0));
        ui.add_space(4.0);

        if ui
            .checkbox(
                &mut prefs.editing.auto_create_tracks,
                "Automatically create tracks when dropping clips",
            )
            .changed()
        {
            *dirty = true;
            cmds.push(AppCommand::SyncAutoCreateTracks(prefs.editing.auto_create_tracks));
        }
        ui.label(
            egui::RichText::new(
                "When enabled, dragging a clip above the topmost video track or below the \
                 bottommost audio track automatically creates a new track.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Media Import").strong().size(12.0));
        ui.add_space(4.0);

        if ui
            .checkbox(
                &mut prefs.editing.auto_generate_filmstrips,
                "Generate filmstrip thumbnails on import",
            )
            .changed()
        {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new(
                "Generates timeline filmstrip frames when media is added. \
                 Uses GPU decode when available. Disable if import feels slow.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(4.0);
        if ui
            .checkbox(
                &mut prefs.editing.auto_generate_skim,
                "Generate skim transcodes on import",
            )
            .changed()
        {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new(
                "Creates lightweight transcodes for instant hover skimming in the media browser. \
                 Disable to save disk space and import time.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(120, 120, 130)),
        );
        ui.add_space(12.0);
        ui.label(egui::RichText::new("History").strong().size(12.0));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Undo history limit:");
            let mut limit = prefs.editing.undo_limit;
            if ui.add(egui::DragValue::new(&mut limit).range(10..=10000).speed(1.0).suffix(" steps")).changed() {
                prefs.editing.undo_limit = limit;
                *dirty = true;
            }
        });
        ui.label(
            egui::RichText::new(
                "Maximum number of undo steps. Higher values use more memory. \
                 Set to 0 for unlimited.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        let _ = cmds; // suppress unused warning
    }

    fn show_audio(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
    ) {
        ui.label(egui::RichText::new("Timeline Display").strong().size(12.0));
        ui.add_space(4.0);

        // Fade overlay mode
        ui.horizontal(|ui| {
            ui.label("Fade overlay:");
            let current = prefs.audio.fade_overlay_mode;
            egui::ComboBox::from_id_salt("fade_overlay_mode")
                .selected_text(match current {
                    crate::prefs::FadeOverlayMode::AudioOnly => "Audio track only",
                    crate::prefs::FadeOverlayMode::Both => "Both video and audio",
                    crate::prefs::FadeOverlayMode::Hidden => "Hidden",
                })
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut prefs.audio.fade_overlay_mode, crate::prefs::FadeOverlayMode::AudioOnly, "Audio track only").changed() {
                        *dirty = true;
                    }
                    if ui.selectable_value(&mut prefs.audio.fade_overlay_mode, crate::prefs::FadeOverlayMode::Both, "Both video and audio").changed() {
                        *dirty = true;
                    }
                    if ui.selectable_value(&mut prefs.audio.fade_overlay_mode, crate::prefs::FadeOverlayMode::Hidden, "Hidden").changed() {
                        *dirty = true;
                    }
                });
        });
        ui.label(
            egui::RichText::new("Controls where audio fade in/out overlays appear on timeline clips.")
                .size(10.0)
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(8.0);

        // Show rubber band
        if ui.checkbox(&mut prefs.audio.show_rubber_band, "Show automation rubber band").changed() {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new("Display the volume/pan/gain automation line on audio clips.")
                .size(10.0)
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(4.0);

        // Show waveform
        if ui.checkbox(&mut prefs.audio.show_waveform, "Show waveform on audio clips").changed() {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new("Display the audio waveform envelope on audio track clips.")
                .size(10.0)
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Crossfades").strong().size(12.0));
        ui.add_space(4.0);

        if ui.checkbox(&mut prefs.audio.anti_click_fade, "Anti-click crossfade at clip boundaries").changed() {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new("Apply a tiny 2ms fade at clip start/end to prevent click and pop artifacts. \
                Disable for perfectly clean cuts (may produce audible clicks at edit points).")
                .size(10.0)
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );

        ui.add_space(4.0);

        if ui.checkbox(&mut prefs.audio.show_crossfade_indicators, "Show crossfade indicators at clip boundaries").changed() {
            *dirty = true;
        }
        ui.label(
            egui::RichText::new("Display the X icon where two adjacent audio clips meet.")
                .size(10.0)
                .color(egui::Color32::from_rgb(120, 120, 130)),
        );
    }

    fn show_export(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
        _cmds: &mut Vec<AppCommand>,
    ) {
        ui.label(egui::RichText::new("Default Output Folder").strong().size(12.0));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let dir_text = prefs
                .export
                .default_output_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "(not set)".into());
            ui.label(&dir_text);
            if ui.button("Browse...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Default Output Folder")
                    .pick_folder()
                {
                    prefs.export.default_output_dir = Some(path);
                    *dirty = true;
                }
            }
            if prefs.export.default_output_dir.is_some() && ui.small_button("Clear").clicked() {
                prefs.export.default_output_dir = None;
                *dirty = true;
            }
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Presets").strong().size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ New Preset").clicked() {
                    self.show_new_preset = true;
                    self.new_preset_name = "My Preset".into();
                    self.editing_preset_id = None;
                    self.edit_format = ExportFormat::Mp4;
                    self.edit_video_codec = ExportVideoCodec::H264;
                    self.edit_crf = 23;
                    self.edit_preset = EncoderPreset::default();
                    self.edit_audio_codec = ExportAudioCodec::Aac;
                    self.edit_audio_bitrate = 192;
                    self.edit_res_w = 1920;
                    self.edit_res_h = 1080;
                    self.edit_use_custom_res = false;
                }
            });
        });
        ui.separator();
        ui.add_space(4.0);

        // Built-in presets (read-only)
        let builtins = builtin_presets();
        for preset in &builtins {
            ui.horizontal(|ui| {
                ui.label(format!("\u{25CF} {}", preset.name));
                ui.colored_label(
                    egui::Color32::from_rgb(80, 80, 90),
                    "(built-in)",
                );
            });
        }

        // Custom presets
        let mut delete_id = None;
        let mut edit_id = None;
        for preset in &prefs.export.custom_presets {
            ui.horizontal(|ui| {
                ui.label(format!("\u{25CF} {}", preset.name));
                if ui.small_button("Edit").clicked() {
                    edit_id = Some(preset.id.clone());
                }
                if ui.small_button("Delete").clicked() {
                    delete_id = Some(preset.id.clone());
                }
            });
        }

        if let Some(id) = delete_id {
            prefs.export.custom_presets.retain(|p| p.id != id);
            *dirty = true;
        }

        if let Some(id) = edit_id {
            if let Some(preset) = prefs.export.custom_presets.iter().find(|p| p.id == id) {
                self.show_new_preset = true;
                self.editing_preset_id = Some(id);
                self.new_preset_name = preset.name.clone();
                self.edit_format = preset.settings.format;
                self.edit_video_codec = preset.settings.video_codec;
                self.edit_crf = preset.settings.crf;
                self.edit_audio_codec = preset.settings.audio_codec;
                self.edit_preset = preset.settings.preset;
                self.edit_audio_bitrate = preset.settings.audio_bitrate_kbps.unwrap_or(192);
                if let Some(res) = &preset.settings.resolution {
                    self.edit_res_w = res.width;
                    self.edit_res_h = res.height;
                    self.edit_use_custom_res = true;
                } else {
                    self.edit_use_custom_res = false;
                }
            }
        }

        // New/Edit preset form
        if self.show_new_preset {
            ui.add_space(8.0);
            ui.separator();
            let title = if self.editing_preset_id.is_some() {
                "Edit Preset"
            } else {
                "New Preset"
            };
            ui.label(egui::RichText::new(title).strong().size(12.0));
            ui.add_space(4.0);

            egui::Grid::new("preset_edit_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.new_preset_name);
                    ui.end_row();

                    ui.label("Format:");
                    egui::ComboBox::from_id_salt("preset_format")
                        .selected_text(self.edit_format.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.edit_format, ExportFormat::Mp4, "MP4");
                            ui.selectable_value(&mut self.edit_format, ExportFormat::Mkv, "MKV");
                            ui.selectable_value(&mut self.edit_format, ExportFormat::Mov, "MOV");
                        });
                    ui.end_row();

                    ui.label("Video Codec:");
                    egui::ComboBox::from_id_salt("preset_vcodec")
                        .selected_text(self.edit_video_codec.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::H264,
                                "H.264",
                            );
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::H265,
                                "H.265",
                            );
                            ui.separator();
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::ProRes422,
                                "ProRes 422",
                            );
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::ProRes422HQ,
                                "ProRes 422 HQ",
                            );
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::ProRes4444,
                                "ProRes 4444",
                            );
                            ui.separator();
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::DNxHD115,
                                "DNxHD 115",
                            );
                            ui.selectable_value(
                                &mut self.edit_video_codec,
                                ExportVideoCodec::DNxHD145,
                                "DNxHD 145",
                            );
                        });
                    ui.end_row();

                    ui.label("CRF:");
                    ui.add(egui::Slider::new(&mut self.edit_crf, 0..=51));
                    ui.end_row();

                    ui.label("Audio Codec:");
                    egui::ComboBox::from_id_salt("preset_acodec")
                        .selected_text(self.edit_audio_codec.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.edit_audio_codec,
                                ExportAudioCodec::Aac,
                                "AAC",
                            );
                            ui.selectable_value(
                                &mut self.edit_audio_codec,
                                ExportAudioCodec::Mp3,
                                "MP3",
                            );
                            ui.selectable_value(
                                &mut self.edit_audio_codec,
                                ExportAudioCodec::Flac,
                                "FLAC",
                            );
                            ui.selectable_value(
                                &mut self.edit_audio_codec,
                                ExportAudioCodec::Copy,
                                "Copy Source",
                            );
                        });
                    ui.end_row();

                    ui.label("Audio Bitrate:");
                    egui::ComboBox::from_id_salt("preset_abitrate")
                        .selected_text(format!("{} kbps", self.edit_audio_bitrate))
                        .show_ui(ui, |ui| {
                            for rate in [128, 192, 256, 320] {
                                ui.selectable_value(
                                    &mut self.edit_audio_bitrate,
                                    rate,
                                    format!("{rate} kbps"),
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("Resolution:");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.edit_use_custom_res, "Custom");
                        if self.edit_use_custom_res {
                            ui.add(
                                egui::DragValue::new(&mut self.edit_res_w)
                                    .range(320..=7680)
                                    .speed(1),
                            );
                            ui.label("\u{00d7}");
                            ui.add(
                                egui::DragValue::new(&mut self.edit_res_h)
                                    .range(240..=4320)
                                    .speed(1),
                            );
                        } else {
                            ui.label("Match source");
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    let settings = ExportPresetSettings {
                        format: self.edit_format,
                        video_codec: self.edit_video_codec,
                        resolution: if self.edit_use_custom_res {
                            Some(klipp_core::Resolution {
                                width: self.edit_res_w,
                                height: self.edit_res_h,
                            })
                        } else {
                            None
                        },
                        fps: None,
                        crf: self.edit_crf,
                        preset: self.edit_preset,
                        audio_codec: self.edit_audio_codec,
                        audio_bitrate_kbps: Some(self.edit_audio_bitrate),
                        video_bitrate_kbps: None,
                        rate_control: klipp_core::RateControl::Crf,
                    };

                    if let Some(ref edit_id) = self.editing_preset_id {
                        // Update existing preset
                        if let Some(preset) = prefs
                            .export
                            .custom_presets
                            .iter_mut()
                            .find(|p| p.id == *edit_id)
                        {
                            preset.name = self.new_preset_name.clone();
                            preset.settings = settings;
                        }
                    } else {
                        // Create new preset
                        let preset = ExportPreset {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: self.new_preset_name.clone(),
                            built_in: false,
                            settings,
                        };
                        prefs.export.custom_presets.push(preset);
                    }

                    *dirty = true;
                    self.show_new_preset = false;
                    self.editing_preset_id = None;
                }
                if ui.button("Cancel").clicked() {
                    self.show_new_preset = false;
                    self.editing_preset_id = None;
                }
            });
        }
    }

    fn show_proxy(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        dirty: &mut bool,
    ) {
        ui.label(egui::RichText::new("Proxy Media").strong().size(12.0));
        ui.add_space(4.0);

        if ui
            .checkbox(
                &mut prefs.proxy.auto_generate_on_import,
                "Auto-generate proxies on import",
            )
            .changed()
        {
            *dirty = true;
        }

        ui.add_space(8.0);
        ui.label("Proxy resolution");
        ui.add_space(4.0);

        egui::ComboBox::from_id_salt("proxy_resolution")
            .selected_text(prefs.proxy.proxy_resolution.label())
            .show_ui(ui, |ui| {
                for res in [
                    ProxyResolution::Quarter,
                    ProxyResolution::Half,
                    ProxyResolution::W960,
                    ProxyResolution::W1280,
                ] {
                    if ui
                        .selectable_value(&mut prefs.proxy.proxy_resolution, res, res.label())
                        .changed()
                    {
                        *dirty = true;
                    }
                }
            });

        ui.add_space(12.0);
        ui.label(egui::RichText::new("Proxy Cache Folder").strong().size(12.0));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let cache_text = prefs
                .proxy
                .proxy_cache_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| {
                    dirs::cache_dir()
                        .map(|d| d.join("klipp").join("proxies").to_string_lossy().to_string())
                        .unwrap_or_else(|| "~/.cache/klipp/proxies".into())
                });
            ui.label(&cache_text);
            if ui.button("Browse...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Proxy Cache Folder")
                    .pick_folder()
                {
                    prefs.proxy.proxy_cache_dir = Some(path);
                    *dirty = true;
                }
            }
        });

        // Show cache disk usage
        ui.add_space(8.0);
        let cache_dir = prefs
            .proxy
            .proxy_cache_dir
            .clone()
            .unwrap_or_else(|| {
                dirs::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                    .join("klipp")
                    .join("proxies")
            });
        let (file_count, total_bytes) = if cache_dir.exists() {
            std::fs::read_dir(&cache_dir)
                .map(|entries| {
                    let mut count = 0u64;
                    let mut bytes = 0u64;
                    for entry in entries.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_file() {
                                count += 1;
                                bytes += meta.len();
                            }
                        }
                    }
                    (count, bytes)
                })
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let size_label = if total_bytes >= 1_073_741_824 {
            format!("{:.1} GB", total_bytes as f64 / 1_073_741_824.0)
        } else {
            format!("{:.1} MB", total_bytes as f64 / 1_048_576.0)
        };
        ui.label(format!("{} proxy files — {}", file_count, size_label));

        ui.add_space(4.0);
        if ui.button("Clear Proxy Cache").clicked() && cache_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
                tracing::warn!("Failed to clear proxy cache: {e}");
            } else {
                tracing::info!("Proxy cache cleared: {}", cache_dir.display());
            }
        }
    }

    fn show_keyboard(&mut self, ui: &mut egui::Ui, key_bindings: &mut crate::keybindings::KeyBindings) {
        use crate::keybindings::{KeyBindings, KeyCombo, key_event_to_combo};

        ui.heading("Keyboard Shortcuts");
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(egui::TextEdit::singleline(&mut self.kb_search).desired_width(200.0));
            if ui.button("Reset All to Defaults").clicked() {
                *key_bindings = KeyBindings::defaults();
                let _ = key_bindings.save();
            }
        });
        ui.add_space(6.0);

        // Show conflicts
        let conflicts = key_bindings.find_conflicts();
        if !conflicts.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 80), format!(
                "{} conflict(s) detected", conflicts.len()
            ));
            for (a, b, combo) in &conflicts {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 160, 80),
                    format!("  {} and {} both use {}", KeyBindings::action_label(a), KeyBindings::action_label(b), combo.display()),
                );
            }
            ui.add_space(4.0);
        }

        // If remapping, capture key press
        if let Some((ref action_name, combo_idx)) = self.remapping.clone() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 60),
                format!("Press a key combination for \"{}\" (Escape to cancel)...", KeyBindings::action_label(&action_name)),
            );
            ui.add_space(4.0);

            let input = ui.input(|i| i.clone());
            if input.key_pressed(egui::Key::Escape) {
                self.remapping = None;
            } else if let Some(combo) = key_event_to_combo(&input) {
                if let Some(combos) = key_bindings.bindings.get_mut(action_name.as_str()) {
                    if combo_idx < combos.len() {
                        combos[combo_idx] = combo;
                    } else {
                        combos.push(combo);
                    }
                }
                let _ = key_bindings.save();
                self.remapping = None;
            }
        }

        let search = self.kb_search.to_lowercase();

        for (category, actions) in KeyBindings::categories() {
            let filtered: Vec<_> = actions.iter().filter(|a| {
                if search.is_empty() { return true; }
                let label = KeyBindings::action_label(a).to_lowercase();
                let binding_str = key_bindings.display_for(a).to_lowercase();
                label.contains(&search) || binding_str.contains(&search) || a.to_lowercase().contains(&search)
            }).collect();
            if filtered.is_empty() { continue; }

            ui.add_space(4.0);
            ui.strong(*category);
            ui.add_space(2.0);

            egui::Grid::new(format!("kb_grid_{}", category))
                .num_columns(4)
                .spacing([8.0, 3.0])
                .striped(true)
                .min_col_width(0.0)
                .show(ui, |ui| {
                    for action in &filtered {
                        let label = KeyBindings::action_label(action);
                        let combos = key_bindings.bindings.get(**action).cloned().unwrap_or_default();
                        let display = if combos.is_empty() {
                            "—".to_string()
                        } else {
                            combos.iter().map(|c| c.display()).collect::<Vec<_>>().join(" / ")
                        };

                        ui.label(label);
                        ui.monospace(&display);

                        // Remap button
                        let is_remapping = self.remapping.as_ref().is_some_and(|(a, _)| a == **action);
                        let btn_label = if is_remapping { "..." } else { "Set" };
                        if ui.small_button(btn_label).clicked() && !is_remapping {
                            self.remapping = Some((action.to_string(), 0));
                        }

                        // Reset to default
                        let defaults = KeyBindings::defaults();
                        let is_default = defaults.bindings.get(**action) == key_bindings.bindings.get(**action);
                        if ui.add_enabled(!is_default, egui::Button::new("Reset").small()).clicked() {
                            if let Some(default_combos) = defaults.bindings.get(**action) {
                                key_bindings.bindings.insert(action.to_string(), default_combos.clone());
                                let _ = key_bindings.save();
                            }
                        }

                        ui.end_row();
                    }
                });
        }
    }
}
