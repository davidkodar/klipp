//! The app's modal dialogs, one after another each frame (save changes,
//! warnings, settings, pickers), the model downloads' dialogs and Quick Look (#826).

use super::*;

impl KlippApp {
    /// Render whichever model-download dialog matches the current
    /// state. Drains worker events first so progress updates land
    /// the same frame they arrive.
    pub(super) fn render_model_download_dialogs(
        &mut self,
        ctx: &egui::Context,
        _all_cmds: &mut Vec<AppCommand>,
    ) {
        use crate::model_download::{
            fmt_bytes, fmt_rate, poll_download, shorten_hash, spawn_download,
            ModelDownloadState,
        };

        // Drain worker events first; transitions take effect this frame.
        poll_download(&mut self.state.model_download);

        // Snapshot the current variant so we can drop the borrow on
        // self.state.model_download before mutating it inside the
        // dialog closures.
        let current = std::mem::take(&mut self.state.model_download);
        match current {
            ModelDownloadState::Idle => {
                // Nothing to render. Restore so we don't re-init.
                self.state.model_download = ModelDownloadState::Idle;
            }
            ModelDownloadState::Consenting { entry } => {
                let mut next = ModelDownloadState::Consenting { entry: entry.clone() };
                let mut close_kind: Option<&'static str> = None;
                egui::Window::new("Download AI Model")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(420.0);
                        ui.heading(&entry.display_name);
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(crate::model_download::consent_text(&entry)).small());
                        ui.add_space(8.0);
                        egui::Grid::new("model_consent_grid")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Size:");
                                ui.label(fmt_bytes(crate::model_download::download_size(&entry)));
                                ui.end_row();
                                ui.label("Source:");
                                ui.label(
                                    entry.sources.first()
                                        .map(|s| s.url.as_str())
                                        .unwrap_or("(none)"),
                                );
                                ui.end_row();
                                ui.label("SHA-256:");
                                ui.label(
                                    egui::RichText::new(shorten_hash(&entry.sha256))
                                        .monospace()
                                        .small(),
                                ).on_hover_text(&entry.sha256);
                                ui.end_row();
                                ui.label("License:");
                                ui.label(&entry.license);
                                ui.end_row();
                                ui.label("Attribution:");
                                ui.label(&entry.attribution);
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Air-gapped install: drop the file directly into \
                                 ~/.local/share/klipp/models/ with the SHA-256 \
                                 above and Klipp will skip the download.",
                            ).small().italics(),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close_kind = Some("cancel");
                            }
                            if ui.button(format!("Download ({})", fmt_bytes(crate::model_download::download_size(&entry)))).clicked() {
                                close_kind = Some("start");
                            }
                        });
                    });
                match close_kind {
                    Some("cancel") => {
                        next = ModelDownloadState::Idle;
                    }
                    Some("start") => {
                        let handle = spawn_download(entry.clone(), ctx.clone());
                        next = ModelDownloadState::Downloading {
                            entry: entry.clone(),
                            total: crate::model_download::download_size(&entry),
                            downloaded: 0,
                            started_at: std::time::Instant::now(),
                            handle,
                        };
                    }
                    _ => {}
                }
                self.state.model_download = next;
            }
            ModelDownloadState::Downloading {
                entry, total, downloaded, started_at, handle,
            } => {
                let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
                let rate = downloaded as f64 / elapsed;
                let mut cancelled = false;
                egui::Window::new("Downloading AI Model")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.label(&entry.display_name);
                        ui.add_space(4.0);
                        let frac = if total > 0 {
                            (downloaded as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        ui.add(egui::ProgressBar::new(frac).show_percentage());
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(format!("{} / {}",
                                fmt_bytes(downloaded), fmt_bytes(total)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(fmt_rate(rate));
                                },
                            );
                        });
                        ui.add_space(8.0);
                        if ui.button("Cancel Download").clicked() {
                            cancelled = true;
                        }
                    });
                if cancelled {
                    handle.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.state.model_download = ModelDownloadState::Downloading {
                    entry, total, downloaded, started_at, handle,
                };
            }
            ModelDownloadState::Failed { entry_label, error } => {
                let mut next = ModelDownloadState::Failed {
                    entry_label: entry_label.clone(),
                    error: error.clone(),
                };
                egui::Window::new("Download Failed")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.heading(&entry_label);
                        ui.add_space(4.0);
                        ui.label(&error);
                        ui.add_space(8.0);
                        if ui.button("Dismiss").clicked() {
                            next = ModelDownloadState::Idle;
                        }
                    });
                self.state.model_download = next;
            }
            ModelDownloadState::Completed { entry_label, path } => {
                self.state.toast.show(format!(
                    "Downloaded {entry_label} ({})", path.display(),
                ));
                // Voice Isolation effects already in a chain pass audio
                // through until their model exists; let them load it.
                klipp_audio::dsp::voice_isolation::notify_model_installed();
                self.state.model_download = ModelDownloadState::Idle;
                // The clips that waited for the speech model (#2942).
                if klipp_analysis::transcript::installed_model().is_some() {
                    for media_id in std::mem::take(&mut self.state.transcribe_after_download) {
                        self.transcribe(media_id);
                    }
                }
            }
        }
    }

    pub(super) fn show_modal_dialogs(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        crate::title_templates::show_dialog(ctx, &mut self.state);
        crate::parameterized_templates::show_save_dialog(ctx, &mut self.state);
        crate::parameterized_templates::show_recipe_save_dialog(ctx, &mut self.state);
        cmds.extend(crate::import_report::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::credits_dialog::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::normalize_dialog::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::shorts_dialog::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::visual_search_dialog::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::music_fit_dialog::show_dialog(ctx, &mut self.state));
        cmds.extend(crate::missing_fonts::show_dialog(ctx, &mut self.state, &self.theme));

        // 4a0. Paste Attributes dialog
        if self.state.show_paste_attributes_dialog {
            let mut open = true;
            egui::Window::new("Paste Attributes")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.label("Select which attributes to paste:");
                    ui.add_space(6.0);

                    // Persistent checkboxes via egui's data store (no unsafe statics)
                    let pa_id = egui::Id::new("paste_attributes_checkboxes");
                    let mut pa_effects = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("fx"), || true));
                    let mut pa_color = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("col"), || true));
                    let mut pa_audio = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("aud"), || true));
                    let mut pa_transform = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("tfm"), || true));
                    let mut pa_speed = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("spd"), || false));

                    ui.checkbox(&mut pa_effects, "Effects (built-in + OFX)");
                    ui.checkbox(&mut pa_color, "Colour Grade (node graph)");
                    ui.checkbox(&mut pa_audio, "Audio (volume, pan, gain, fades, FX)");
                    ui.checkbox(&mut pa_transform, "Transform (position, scale, rotation, opacity)");
                    ui.checkbox(&mut pa_speed, "Speed");

                    // Write back to egui data store
                    ctx.data_mut(|d| { d.insert_temp(pa_id.with("fx"), pa_effects); d.insert_temp(pa_id.with("col"), pa_color); d.insert_temp(pa_id.with("aud"), pa_audio); d.insert_temp(pa_id.with("tfm"), pa_transform); d.insert_temp(pa_id.with("spd"), pa_speed); });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Paste").clicked() {
                            cmds.push(AppCommand::PasteAttributes {
                                effects: pa_effects,
                                color_grade: pa_color,
                                audio: pa_audio,
                                transform: pa_transform,
                                speed: pa_speed,
                            });
                            self.state.show_paste_attributes_dialog = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_paste_attributes_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_paste_attributes_dialog = false;
            }
        }

        // An editing helper's dialog (#988): strobe, set length, copy
        // over every edit.
        if let Some(mut d) = self.state.edit_helper.take() {
            let mut open = true;
            let mut run = false;
            let mut cancel = false;
            egui::Window::new(d.title())
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    match d.kind {
                        0 => {
                            ui.label(egui::RichText::new("Between the In and Out when both are inside the clip, else the whole clip.").small().weak());
                            ui.horizontal(|ui| {
                                ui.label("Pieces of");
                                ui.add(egui::DragValue::new(&mut d.piece_frames).range(1..=240).suffix(" frames"));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Keep");
                                ui.add(egui::DragValue::new(&mut d.on).range(1..=16));
                                ui.label("then remove");
                                ui.add(egui::DragValue::new(&mut d.off).range(1..=16));
                            });
                            ui.checkbox(&mut d.ripple, "Close up the gaps").on_hover_text("Off, the removed pieces leave black");
                        }
                        1 => {
                            ui.label(egui::RichText::new("Every selected clip, or this one.").small().weak());
                            ui.horizontal(|ui| {
                                ui.label("Length");
                                ui.add(egui::DragValue::new(&mut d.frames).range(1..=100_000).suffix(" frames"));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Keep the");
                                ui.selectable_value(&mut d.keep, klipp_document::ops::LengthKeep::Start, "start");
                                ui.selectable_value(&mut d.keep, klipp_document::ops::LengthKeep::Centre, "centre");
                                ui.selectable_value(&mut d.keep, klipp_document::ops::LengthKeep::End, "end");
                            });
                            ui.checkbox(&mut d.ripple, "Move what follows").on_hover_text("Closes up after shorter clips, makes room for longer ones");
                        }
                        _ => {
                            ui.label(egui::RichText::new("On this clip's track, over the cuts of the targeted picture tracks, between the In and Out when set. A cut where a copy would overlap a clip is skipped.").small().weak());
                            ui.horizontal(|ui| {
                                ui.label("Put its");
                                ui.selectable_value(&mut d.align, klipp_document::ops::EditAlign::Centre, "centre");
                                ui.selectable_value(&mut d.align, klipp_document::ops::EditAlign::Start, "start");
                                ui.selectable_value(&mut d.align, klipp_document::ops::EditAlign::End, "end");
                                ui.label("on each");
                            });
                            ui.checkbox(&mut d.every_clip, "Every clip's start, not just the cuts");
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            run = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if open && !cancel {
                self.state.edit_helper = Some(d);
                if run {
                    cmds.push(AppCommand::RunEditHelper);
                }
            }
        }

        // Speed dialog
        if self.state.show_speed_dialog {
            // Modal backdrop
            egui::Area::new(egui::Id::new("speed_dialog_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::PanelResizeLine)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
                    if resp.clicked() { self.state.show_speed_dialog = false; }
                });

            let mut open = true;
            egui::Window::new("Set Clip Speed")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Speed:");
                        ui.add(egui::DragValue::new(&mut self.state.speed_dialog_value)
                            .range(1.0..=1000.0).speed(1.0).suffix("%").fixed_decimals(0));
                    });
                    ui.add_space(4.0);

                    // Preset buttons
                    ui.horizontal(|ui| {
                        for (label, pct) in [("25%", 25.0), ("50%", 50.0), ("100%", 100.0), ("200%", 200.0), ("400%", 400.0)] {
                            if ui.selectable_label((self.state.speed_dialog_value - pct).abs() < 0.5, label).clicked() {
                                self.state.speed_dialog_value = pct;
                            }
                        }
                    });
                    ui.add_space(4.0);

                    // Reverse reads a media file backward (#617): a title or
                    // a compound has none.
                    let can_reverse = self.state.selected_clip
                        .and_then(|id| self.state.sequence().all_clips().find(|c| c.id == id).map(|c| matches!(c.kind, klipp_timeline::ClipKind::Media { .. })))
                        .unwrap_or(false);
                    if !can_reverse {
                        self.state.speed_dialog_reverse = false;
                    }
                    ui.add_enabled(can_reverse, egui::Checkbox::new(&mut self.state.speed_dialog_reverse, "Reverse playback"))
                        .on_hover_text("Play the clip from its last frame to its first. Its sound plays backward with it.")
                        .on_disabled_hover_text("Only a clip of a media file can play in reverse");
                    ui.checkbox(&mut self.state.speed_dialog_ripple, "Move the clips after it")
                        .on_hover_text("A clip made longer pushes what follows on its track (and its sound's) along; one made shorter pulls it up. Off, a clip that would grow into the next one is not changed.");
                    // What a speed change does to the sound (#638, #925).
                    let reverse = self.state.speed_dialog_reverse;
                    ui.add_enabled(!reverse, egui::Checkbox::new(&mut self.state.speed_dialog_keep_pitch, "Keep the pitch"))
                        .on_hover_text("The sound plays at its own pitch, longer or shorter. Off, its pitch moves with the speed, as tape's does.")
                        .on_disabled_hover_text("A reversed clip's sound plays backward, its pitch moving with the speed");
                    let says = if self.state.speed_dialog_keep_pitch && !reverse {
                        "The sound plays at the new speed at its own pitch."
                    } else {
                        "The sound plays at the new speed, its pitch moving with it, as tape does."
                    };
                    ui.label(egui::RichText::new(says).small().weak());
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            if let Some(clip_id) = self.state.selected_clip {
                                let mut speed = self.state.speed_dialog_value / 100.0;
                                if self.state.speed_dialog_reverse { speed = -speed; }
                                cmds.push(AppCommand::SetClipSpeed { clip_id, speed, ripple: self.state.speed_dialog_ripple });
                                cmds.push(AppCommand::SetClipKeepPitch { clip_id, keep_pitch: self.state.speed_dialog_keep_pitch });
                            }
                            self.state.show_speed_dialog = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_speed_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_speed_dialog = false;
            }
        }

        // Save Workspace dialog
        if self.state.show_save_workspace_dialog {
            let mut open = true;
            egui::Window::new("Save Workspace")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.state.save_workspace_name);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // Capture the current panel sizes alongside the
                            // visibility flags so the saved workspace is a
                            // full layout snapshot — not just "which panels
                            // are visible," which would drop the sizing the
                            // user just arranged.
                            let layout = crate::state::SavedWorkspaceLayout {
                                name: self.state.save_workspace_name.clone(),
                                show_browser: self.state.show_browser_panel,
                                show_inspector: self.state.show_inspector_panel,
                                show_history: self.state.show_history_panel,
                                show_markers: self.state.show_markers_panel,
                                show_curve_editor: self.state.show_curve_editor,
                                show_scopes: self.state.show_scopes,
                                show_color_panel: self.state.show_color_panel,
                                browser_width: Some(self.layout.browser_width),
                                inspector_width: Some(self.layout.inspector_width),
                                timeline_height: Some(self.layout.timeline_height),
                                color_panel_height: Some(self.layout.color_panel_height),
                            };
                            // Replace if name exists, otherwise add
                            if let Some(existing) = self.state.custom_workspaces.iter_mut().find(|w| w.name == layout.name) {
                                *existing = layout;
                            } else {
                                self.state.custom_workspaces.push(layout);
                            }
                            self.state.show_save_workspace_dialog = false;
                            self.state.toast.show("Workspace saved".to_string());
                            persist_workspaces(&self.state.custom_workspaces);
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_save_workspace_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_save_workspace_dialog = false;
            }
        }

        // ── Save hyperzoom preset dialog (HZ-NEXT-SAVED-PRESETS) ──
        if self.state.show_save_hyperzoom_preset_dialog {
            let mut open = true;
            egui::Window::new("Save Hyperzoom as Preset")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        let resp = ui.text_edit_singleline(&mut self.state.save_hyperzoom_preset_name);
                        // Clear inline validation error on edit so
                        // the user gets immediate feedback that the
                        // text is being accepted.
                        if resp.changed() {
                            self.state.save_hyperzoom_preset_error = None;
                        }
                    });
                    if let Some(err) = self.state.save_hyperzoom_preset_error.as_ref() {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(format!("\u{26A0} {err}"))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(0xE0, 0x80, 0x40)),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // Read the bound clip's scratch.
                            // Source clip was pinned at dialog-open
                            // time; if it's gone (clip deleted while
                            // dialog open), we close gracefully.
                            let Some(source_clip) = self.state.save_hyperzoom_preset_source_clip else {
                                self.state.show_save_hyperzoom_preset_dialog = false;
                                self.state.toast.show("Preset save cancelled (no source clip)".to_string());
                                return;
                            };
                            let scratch = crate::hyperzoom::read_hyperzoom_scratch(ctx, source_clip);
                            let name = self.state.save_hyperzoom_preset_name.clone();
                            let preset = crate::hyperzoom_presets::HyperzoomPreset::from_scratch(name, &scratch);
                            match self.state.hyperzoom_preset_registry.save(preset) {
                                Ok(()) => {
                                    let saved_name = self.state.save_hyperzoom_preset_name.clone();
                                    self.state.save_hyperzoom_preset_name.clear();
                                    self.state.save_hyperzoom_preset_error = None;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.toast.show(format!("Saved hyperzoom preset '{saved_name}'"));
                                }
                                Err(crate::hyperzoom_presets::SaveError::BadName(e)) => {
                                    // Inline validation — keep the
                                    // dialog open so the user can
                                    // type a fresh name.
                                    self.state.save_hyperzoom_preset_error = Some(e.to_string());
                                }
                                Err(crate::hyperzoom_presets::SaveError::Io(e)) => {
                                    tracing::warn!("hyperzoom preset save IO failure: {e}");
                                    self.state.toast.show("Couldn't save preset (disk error)".to_string());
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                }
                                Err(crate::hyperzoom_presets::SaveError::Serialize(e)) => {
                                    tracing::error!("hyperzoom preset save serialize failure: {e}");
                                    self.state.toast.show("Couldn't save preset".to_string());
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.save_hyperzoom_preset_name.clear();
                            self.state.save_hyperzoom_preset_error = None;
                            self.state.save_hyperzoom_preset_source_clip = None;
                            self.state.show_save_hyperzoom_preset_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.save_hyperzoom_preset_name.clear();
                self.state.save_hyperzoom_preset_error = None;
                self.state.save_hyperzoom_preset_source_clip = None;
                self.state.show_save_hyperzoom_preset_dialog = false;
            }
        }

        // 4a0. The project being opened is locked by someone else.
        self.show_lock_prompt(ctx);

        // 4a1. Auto-save recovery dialog
        if self.state.show_autosave_recovery {
            egui::Window::new("Recover Auto-Save?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("A newer auto-save was found for this project.");
                    ui.label("Would you like to restore it?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restore Auto-Save").clicked() {
                            self.state.autosave_recovery_path = None;
                            self.state.show_autosave_recovery = false;
                            if let Some(original_path) = self.state.autosave_original_path.take() {
                                self.restore_autosave(&original_path);
                            }
                        }
                        if ui.button("Keep Saved Version").clicked() {
                            self.state.autosave_recovery_path = None;
                            self.state.show_autosave_recovery = false;
                            if let Some(original_path) = self.state.autosave_original_path.take() {
                                self.keep_saved_version(&original_path);
                            }
                        }
                    });
                });
        }

        // 4a1b. Confirm remove media from pool
        if let Some(media_id) = self.state.show_remove_media_confirm {
            let usage = self.state.media_usage_cache.get(&media_id).copied().unwrap_or(0);
            let name = self.state.media_pool.get(&media_id)
                .map(|m| m.filename.clone())
                .unwrap_or_else(|| "Unknown".into());
            egui::Window::new("Remove Media?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("\"{}\" is used in {} clip(s).", name, usage));
                    ui.label("Removing it will make those clips offline.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_remove_media_confirm = None;
                        }
                        if ui.button("Remove").clicked() {
                            cmds.push(AppCommand::ConfirmRemoveFromPool { media_id });
                        }
                    });
                });
        }

        // Delete unused media confirmation dialog
        if let Some((count, total_bytes)) = self.state.show_delete_unused_confirm {
            egui::Window::new("Delete Unused Media?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("{count} items are not referenced by any timeline clip."));
                    let size_mb = total_bytes as f64 / (1024.0 * 1024.0);
                    if size_mb >= 1024.0 {
                        ui.label(format!("({count} items referencing {:.1} GB of source media)", size_mb / 1024.0));
                    } else {
                        ui.label(format!("({count} items referencing {:.0} MB of source media)", size_mb));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_delete_unused_confirm = None;
                        }
                        if ui.button(format!("Remove {count} items")).clicked() {
                            cmds.push(AppCommand::ConfirmDeleteUnusedMedia);
                        }
                    });
                });
        }

        // A tracked subject matte short of what the export reads (#239):
        // the export waits for it. Track the rest, or cancel; there is no
        // "anyway", since a missing frame would take its neighbour's matte.
        if let Some(lines) = self.state.export_matte_warning.clone() {
            let mut close = false;
            egui::Window::new("Subject mattes aren't finished")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("These clips are graded by a tracked subject, or cut round one by a transition, that doesn't cover every frame yet:");
                    ui.add_space(4.0);
                    for (_, _, line) in &lines {
                        ui.label(format!("  \u{2022} {line}"));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Track the rest").clicked() {
                            for (clip_id, track, _) in &lines {
                                if !self.state.tracking_subjects.contains_key(clip_id) {
                                    cmds.push(track.clone());
                                }
                            }
                            close = true;
                        }
                    });
                });
            if close {
                self.state.export_matte_warning = None;
            }
        }

        // Export offline media warning dialog
        if self.state.export_offline_warning.is_some() {
            let mut dismiss = false;
            let mut force_export = false;
            if let Some((ref clips, _)) = self.state.export_offline_warning {
                egui::Window::new("Export Contains Offline Media")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("These clips will export as black/silent frames:");
                        ui.add_space(4.0);
                        for (name, tc) in clips {
                            ui.label(format!("  \u{2022} {}  at {}", name, tc));
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                dismiss = true;
                            }
                            if ui.button("Export Anyway").clicked() {
                                force_export = true;
                            }
                        });
                    });
            }
            if dismiss {
                self.state.export_offline_warning = None;
            }
            if force_export {
                if let Some((_, settings)) = self.state.export_offline_warning.take() {
                    cmds.push(AppCommand::ForceExportSequence { settings });
                }
            }
        }

        // Export would overwrite a source media file — warn the user.
        // Three escape hatches: pick a different file (re-opens the
        // export dialog with the conflicting filename pre-filled),
        // auto-rename to the next free numeric suffix, or overwrite
        // anyway (the user knows what they're doing — e.g. they're
        // genuinely re-encoding in place).
        if self.state.export_source_overwrite_warning.is_some() {
            let mut auto_rename = false;
            let mut overwrite_anyway = false;
            let mut pick_different = false;
            let mut rename_target = std::path::PathBuf::new();
            if let Some((ref filename, ref settings)) =
                self.state.export_source_overwrite_warning
            {
                rename_target = Self::auto_rename_export_path(&settings.output_path);
                let rename_label = rename_target
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "renamed copy".into());
                egui::Window::new("Export Would Overwrite Source Media")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("The export target is the same file as a clip in this project:".to_string());
                        ui.add_space(2.0);
                        ui.label(format!("  \u{2022} {}", filename));
                        ui.add_space(6.0);
                        ui.label(
                            "Writing to it will replace the source on disk, \
                             invalidate the filmstrip cache, and on next reload \
                             the timeline will reference the rendered output \
                             instead of the camera-original.",
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Pick Different File").clicked() {
                                pick_different = true;
                            }
                            if ui.button(format!("Auto-Rename to {}", rename_label)).clicked() {
                                auto_rename = true;
                            }
                            if ui.button("Overwrite Anyway").clicked() {
                                overwrite_anyway = true;
                            }
                        });
                    });
            }
            if pick_different {
                self.state.export_source_overwrite_warning = None;
                self.state.show_export_dialog = true;
            }
            if auto_rename {
                if let Some((_, settings)) =
                    self.state.export_source_overwrite_warning.take()
                {
                    let mut new_settings = settings.as_ref().clone();
                    new_settings.output_path = rename_target;
                    cmds.push(AppCommand::ForceExportSequenceOverwrite {
                        settings: Box::new(new_settings),
                    });
                }
            }
            if overwrite_anyway {
                if let Some((_, settings)) =
                    self.state.export_source_overwrite_warning.take()
                {
                    cmds.push(AppCommand::ForceExportSequenceOverwrite { settings });
                }
            }
        }

        // 4a2. Confirm remove track dialog
        if let Some(track_id) = self.state.confirm_remove_track {
            let mut open = true;
            egui::Window::new("Remove Track")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This track contains clips. Remove track and all its clips?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            // One undo step, clips and all (#907).
                            let before = crate::tracks_undo::before_labelled(&self.state, "Remove Track");
                            let _ = self.state.sequences[self.state.active_sequence_idx].remove_track_with_clips(track_id);
                            self.state.sequences[self.state.active_sequence_idx].renumber_tracks();
                            if let Some(before) = before {
                                crate::tracks_undo::record(&mut self.state, before);
                            }
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                            self.state.confirm_remove_track = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.confirm_remove_track = None;
                        }
                    });
                });
            if !open {
                self.state.confirm_remove_track = None;
            }
        }

        // 4a3. Confirm delete sequence (#906): a deleted sequence and its
        // undo history cannot be brought back, so the tab menu asks.
        if let Some(seq_id) = self.state.confirm_delete_sequence {
            let found = self.state.sequences.iter().position(|s| s.id == seq_id);
            match found {
                Some(idx) => {
                    let name = self.state.sequences[idx].name.clone();
                    let mut open = true;
                    egui::Window::new("Delete Sequence")
                        .open(&mut open)
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            ui.label(format!("Delete the sequence \"{name}\" from the project?"));
                            ui.label("Its edits and its undo history go with it. This cannot be undone.");
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui.button("Delete").clicked() {
                                    cmds.push(AppCommand::DeleteSequence { sequence_id: seq_id });
                                    self.state.confirm_delete_sequence = None;
                                }
                                if ui.button("Cancel").clicked() {
                                    self.state.confirm_delete_sequence = None;
                                }
                            });
                        });
                    if !open {
                        self.state.confirm_delete_sequence = None;
                    }
                }
                None => self.state.confirm_delete_sequence = None,
            }
        }

        // 4a4. Auto Align Audio (#615): the two files, the method, and why
        // a method cannot run; Sync moves the recording, Cancel nothing.
        if let Some((pair, mut method)) = self.state.auto_align {
            use crate::auto_align::{self, AlignMethod};
            let name = |id: klipp_core::ClipId| {
                self.state.sequence().find_clip(id)
                    .and_then(|(t, c)| self.state.sequence().tracks[t].clips[c].media_id())
                    .and_then(|m| self.state.media_pool.get(&m))
                    .map(|m| m.filename.clone())
                    .unwrap_or_else(|| "(missing)".into())
            };
            let (camera, recording) = (name(pair.reference), name(pair.recording));
            let waveform_no = auto_align::waveform_unavailable(self.state.sequence(), &self.state.media_pool, pair);
            let timecode_no = auto_align::timecode_position(self.state.sequence(), &self.state.media_pool, pair).err();
            let mut open = true;
            let mut close = false;
            egui::Window::new("Auto Align Audio")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Reference (stays): {camera}"));
                    ui.label(format!("Recording (moves): {recording}"));
                    ui.add_space(6.0);
                    for (m, label, why) in [(AlignMethod::Waveform, "Waveform", &waveform_no), (AlignMethod::Timecode, "Timecode", &timecode_no)] {
                        ui.add_enabled_ui(why.is_none(), |ui| {
                            ui.radio_value(&mut method, m, label);
                        });
                        if let Some(why) = why {
                            ui.label(egui::RichText::new(format!("    {label} cannot run: {why}")).weak().size(11.0));
                        }
                    }
                    let can = match method {
                        AlignMethod::Waveform => waveform_no.is_none(),
                        AlignMethod::Timecode => timecode_no.is_none(),
                    };
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(can, egui::Button::new("Sync")).clicked() {
                            cmds.push(AppCommand::AutoAlignAudio { reference: pair.reference, recording: pair.recording, by_timecode: method == AlignMethod::Timecode });
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            self.state.auto_align = if close || !open { None } else { Some((pair, method)) };
        }

        // 4b. Export dialog
        let export_cmds = self.show_export_dialog(ctx);
        cmds.extend(export_cmds);

        // 4c. Render queue window
        self.show_render_queue_window(ctx);

        // 4c0. AI Tools side panel — only mounted when toggled on.
        if self.state.ai_tools_state.panel_visible {
            self.state.ai_tools_state.can_restore_background = self.state.selected_clip
                .and_then(|id| self.state.sequence().all_clips().find(|c|c.id==id))
                .is_some_and(|c| klipp_timeline::GradeAccess::grade(c).cut_out_matte().is_some());
            // egui's SidePanel default `width_range.max` is
            // `f32::INFINITY`, which means any single-frame
            // content overflow becomes the new stored width on
            // the following frame (PanelState stores
            // inner_response.response.rect). Setting an explicit
            // max_width hard-caps the panel via clamp_to_range
            // regardless of what content reports.
            egui::SidePanel::right("ai_tools_side")
                .resizable(true)
                .default_width(340.0)
                .min_width(300.0)
                .max_width(500.0)
                .show(ctx, |ui| {
                    crate::ai_tools::AiToolsPanel::show(
                        ui,
                        &mut self.state.ai_tools_state,
                        &self.state.ai_tools_registry,
                        &self.theme,
                        &mut self.state.show_ai_models_dialog,
                    );
                });
        }

        // 4c1. New Project dialog
        if self.state.show_new_project_dialog {
            self.new_project_dialog.show(ctx, &mut self.state.show_new_project_dialog, &self.theme, cmds);
        }
        // Save Project as Template (#618): what it will keep, said up front.
        if let Some(mut dlg) = self.state.save_template_dialog.take() {
            let setup = klipp_project::template::ProjectSetup { sequences: &self.state.sequences, pool: &self.state.media_pool, colour: self.template_colour() };
            let choice = klipp_project::template::StarterChoice { sequences: if dlg.include_starter { dlg.starter.clone() } else { Vec::new() }, package_media: dlg.package_media };
            let (template, _) = klipp_project::template::ProjectTemplate::capture_with_starter("-", "", &setup, &choice);
            // What the starter content needs, as the template will hold it
            // (copied media counted as copied).
            let deps: Vec<klipp_project::template::Dependency> = template
                .dependencies(std::path::Path::new(""))
                .into_iter()
                .map(|d| match d {
                    klipp_project::template::Dependency::LinkedMedia { path, .. } if dlg.package_media => {
                        klipp_project::template::Dependency::PackagedMedia(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                    }
                    d => d,
                })
                .collect();
            let view = crate::panels::new_project_dialog::SaveTemplateView {
                summary: template.summary(),
                sequences: self.state.sequences.iter().map(|s| (s.id, s.name.clone())).collect(),
                needs: crate::panels::new_project_dialog::dependency_lines(&deps, &|_| true),
                existing: klipp_project::template::list_templates(&self.project_templates_dir()).into_iter().filter_map(|(_, t)| t.ok().map(|t| t.name)).collect(),
            };
            if dlg.show(ctx, &self.theme, &view, cmds) {
                self.state.save_template_dialog = Some(dlg);
            }
        }

        // 4c1a. Sequence Settings dialog (resolution + frame rate). The
        // original values are sourced from the targeted sequence at render
        // time so the dialog can detect what actually changed and emit
        // commands only for those fields.
        if let Some(dlg) = self.sequence_settings_dialog.as_mut() {
            let (orig_w, orig_h, orig_n, orig_d) = self
                .state
                .sequences
                .get(dlg.sequence_idx)
                .map(|s| (s.resolution.width, s.resolution.height, s.fps.num, s.fps.den))
                .unwrap_or((0, 0, 0, 0));
            let orig_shutter = self
                .state
                .sequences
                .get(dlg.sequence_idx)
                .map(|s| (s.camera.shutter_deg, s.camera.shutter_samples))
                .unwrap_or((0.0, 8));
            let orig_title_nits = self
                .state
                .sequences
                .get(dlg.sequence_idx)
                .map_or(klipp_timeline::DEFAULT_TITLE_NITS, |s| s.title_nits);
            let (still_open, dlg_cmds) = dlg.show(ctx, &self.theme, orig_w, orig_h, orig_n, orig_d, orig_shutter, orig_title_nits);
            cmds.extend(dlg_cmds);
            if !still_open {
                self.sequence_settings_dialog = None;
            }
        }

        // 4c1a-bis. Frame-rate mismatch prompt. When the user drops a clip
        // whose fps differs from the active sequence's (and the sequence
        // already has clips), the append handler defers and opens this
        // modal. Re-dispatch the stashed command on Change / Don't Change
        // with `suppress_fps_prompt` set so the handler does not re-prompt.
        if let Some(dlg) = self.framerate_mismatch_dialog.as_mut() {
            if let Some(choice) = dlg.show(ctx) {
                use crate::panels::framerate_mismatch_dialog::FrameRateMismatchChoice;
                let pending = dlg.pending_cmd.clone();
                let seq_idx = dlg.sequence_idx;
                let media_fps = dlg.media_fps;
                self.framerate_mismatch_dialog = None;
                match choice {
                    FrameRateMismatchChoice::Change => {
                        cmds.push(AppCommand::SetSequenceFps {
                            sequence_idx: seq_idx,
                            fps_num: media_fps.num,
                            fps_den: media_fps.den,
                        });
                        self.suppress_fps_prompt = true;
                        cmds.push(pending);
                    }
                    FrameRateMismatchChoice::DontChange => {
                        self.suppress_fps_prompt = true;
                        cmds.push(pending);
                    }
                    FrameRateMismatchChoice::Cancel => {}
                }
            }
        }

        // 4c1b. Batch Rename dialog. Resolve the selected ids to
        // `(id, current filename)` pairs up-front so the dialog can borrow
        // `state` mutably without a conflicting read of `media_pool`.
        if self.state.show_batch_rename_dialog {
            let items: Vec<(klipp_core::MediaId, String)> = self.state
                .batch_rename_ids
                .iter()
                .filter_map(|id| self.state.media_pool.get(id).map(|m| (*id, m.filename.clone())))
                .collect();
            let dialog_cmds = crate::panels::batch_rename_dialog::show_batch_rename_dialog(
                ctx,
                &mut self.state,
                &items,
            );
            cmds.extend(dialog_cmds);
        }

        // Find and replace in titles (#1717).
        if self.state.show_title_find {
            cmds.extend(crate::panels::title_find_dialog::show_title_find_dialog(ctx, &mut self.state));
        }

        // Clip Properties modal (#47)
        if self.state.show_clip_properties {
            let dialog_cmds = crate::panels::clip_properties::show_clip_properties_dialog(
                ctx,
                &mut self.state,
            );
            cmds.extend(dialog_cmds);
        }

        // Export Preset Manager modal (#45)
        if self.state.show_preset_manager {
            let dialog_cmds = crate::panels::preset_manager::show_preset_manager_dialog(
                ctx,
                &mut self.state,
                &mut self.prefs.export.custom_presets,
            );
            cmds.extend(dialog_cmds);
        }

        // Workspace Layout Manager modal (#39)
        if self.state.show_workspace_manager {
            let dialog_cmds = crate::panels::workspace_manager::show_workspace_manager_dialog(
                ctx,
                &mut self.state,
            );
            cmds.extend(dialog_cmds);
        }

        // Cache Manager modal (#78)
        if self.state.show_cache_manager {
            let dialog_cmds = crate::panels::cache_manager::show_cache_manager_dialog(
                ctx,
                &mut self.state,
                &self.prefs,
            );
            cmds.extend(dialog_cmds);
        }

        // AI Models management modal — list, status badges, install /
        // reinstall actions. Drives the existing model_download
        // consent flow when the user picks Download.
        crate::panels::ai_models::show_ai_models_dialog(ctx, &mut self.state, &self.theme);

        // Set Up AI Assistants (#2649): from the Help menu, the palette,
        // or the button in Preferences, which shares its state.
        if self.preferences_panel.take_open_assistant_setup() {
            self.state.show_assistant_setup = true;
        }
        let mut open = self.state.show_assistant_setup;
        self.preferences_panel.show_assistant_setup_window(ctx, &mut open, &self.prefs, &self.theme);
        self.state.show_assistant_setup = open;

        // Audio Plugins management modal — LADSPA discovery, search
        // paths, Rescan. Pushes commands onto a local Vec so they go
        // through the standard `send_commands` dispatch (giving
        // `handle_playback_commands` first crack at OpenAudioPluginsDialog
        // / RescanAudioPlugins, same path as OpenAiModelsDialog).
        let mut audio_plugin_cmds: Vec<klipp_core::AppCommand> = Vec::new();
        crate::panels::audio_plugins::show_audio_plugins_dialog(
            ctx,
            &mut self.state,
            &self.theme,
            &mut audio_plugin_cmds,
        );
        if !audio_plugin_cmds.is_empty() {
            self.send_commands(audio_plugin_cmds);
        }

        // System Probe — read-only diagnostic snapshot. No commands
        // dispatched from inside, so no Vec needed.
        crate::panels::system_probe::show_system_probe_dialog(
            ctx,
            &mut self.state,
            &self.theme,
        );

        // Project Doctor — detect-and-report scan of the loaded
        // .klipp directory. Pushes commands through the standard
        // dispatch (today no per-finding fix actions exist; the
        // Vec is wired so future Fix buttons hook in cleanly).
        let mut doctor_cmds: Vec<klipp_core::AppCommand> = Vec::new();
        crate::panels::project_doctor::show_project_doctor_dialog(
            ctx,
            &mut self.state,
            &self.theme,
            &mut doctor_cmds,
        );
        if !doctor_cmds.is_empty() {
            self.send_commands(doctor_cmds);
        }

        // 4c2. Consolidate Project dialog
        // Copies the project + all referenced media into a NEW self-contained
        // directory. The source project is never modified.
        if self.state.show_consolidate_dialog {
            let Some(ref project_path) = self.state.project_path.clone() else {
                self.state.show_consolidate_dialog = false;
                return;
            };
            let mut open = true;
            let total_media = self.state.media_pool.all().count();
            let offline_count = self.state.media_pool.all().filter(|m| m.offline).count();
            let online_count = total_media - offline_count;
            let consolidation_active = self.state.consolidate_progress.is_some();

            egui::Window::new("Consolidate Project")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        "Copies media into a new self-contained project folder. \
                         The original is untouched.",
                    );
                    ui.add_space(8.0);

                    // What is copied (#77): everything, to hand the project
                    // on; or only what the timeline uses, to archive it.
                    ui.radio_value(&mut self.state.consolidate_mode, 0, "Everything in the media pool")
                        .on_hover_text("Every file, whole: for handing the project to someone who may want the unused takes");
                    ui.radio_value(&mut self.state.consolidate_mode, 1, "Only media the timelines use")
                        .on_hover_text("Files no sequence uses are left out; the ones used are copied whole");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.state.consolidate_mode, 2, "Only what is used, with handles of")
                            .on_hover_text("Each file cut to the stretches its clips use, plus the handles, without re-encoding (from the keyframe before). A file that cannot be cut cleanly is copied whole");
                        ui.add_enabled(
                            self.state.consolidate_mode == 2,
                            egui::DragValue::new(&mut self.state.consolidate_handle_secs).range(0.0..=30.0).speed(0.1).suffix(" s"),
                        );
                    });
                    let mode = match self.state.consolidate_mode {
                        1 => klipp_project::consolidate::ConsolidateMode::UsedOnly,
                        2 => klipp_project::consolidate::ConsolidateMode::Trimmed { handle_secs: self.state.consolidate_handle_secs },
                        _ => klipp_project::consolidate::ConsolidateMode::Collect,
                    };
                    // Said before anything is written: files, size, and what
                    // is left out.
                    let key = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        (self.state.consolidate_mode, self.state.consolidate_handle_secs.to_bits(), self.state.consolidate_include_proxies, self.state.consolidate_include_skims).hash(&mut h);
                        total_media.hash(&mut h);
                        for c in self.state.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()) {
                            (c.id, c.source_in.value, c.source_out.value).hash(&mut h);
                        }
                        h.finish()
                    };
                    if self.state.consolidate_summary.as_ref().is_none_or(|(k, _)| *k != key) {
                        let options = klipp_project::consolidate::ConsolidateOptions {
                            dest_dir: std::path::PathBuf::new(),
                            include_proxies: self.state.consolidate_include_proxies,
                            include_skims: self.state.consolidate_include_skims,
                            mode,
                            looks: self.state.looks.clone(),
                        };
                        let color = klipp_project::ColorSettings { global_lut_path: self.state.global_lut_path.clone(), ..Default::default() };
                        let summary = klipp_project::consolidate::summarise(&self.state.media_pool, &self.state.sequences, &color, &options);
                        self.state.consolidate_summary = Some((key, summary));
                    }
                    if let Some((_, s)) = &self.state.consolidate_summary {
                        let size = if s.bytes >= 1 << 30 { format!("{:.1} GB", s.bytes as f64 / (1u64 << 30) as f64) } else { format!("{:.1} MB", s.bytes as f64 / (1u64 << 20) as f64) };
                        let mut said = format!("Will write {} file(s), about {size}", s.files);
                        if s.trimmed > 0 {
                            said.push_str(&format!("; {} cut to what is used", s.trimmed));
                        }
                        if !s.left_out.is_empty() {
                            said.push_str(&format!("; {} unused left out", s.left_out.len()));
                        }
                        ui.label(said).on_hover_text(if s.left_out.is_empty() { String::new() } else { format!("Left out:\n{}", s.left_out.join("\n")) });
                    }
                    ui.add_space(4.0);

                    ui.label(format!("Media files:  {online_count} online"));
                    if offline_count > 0 {
                        ui.colored_label(
                            self.theme.marker_warning,
                            format!(
                                "\u{26A0} {offline_count} offline file(s) will be skipped",
                            ),
                        );
                    }
                    ui.add_space(8.0);

                    ui.checkbox(
                        &mut self.state.consolidate_include_proxies,
                        "Include proxy files",
                    );
                    ui.checkbox(
                        &mut self.state.consolidate_include_skims,
                        "Include skim files",
                    );
                    ui.checkbox(
                        &mut self.state.consolidate_create_zip,
                        "Also create .zip archive",
                    );
                    if self.state.consolidate_create_zip {
                        ui.indent("consolidate_zip_opts", |ui| {
                            ui.checkbox(
                                &mut self.state.consolidate_delete_folder_after_zip,
                                "Delete folder after archiving",
                            );
                        });
                    }
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("Destination:");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.state.consolidate_dest_path,
                            )
                            .desired_width(280.0),
                        );
                        let picker = egui::Id::new("consolidate-destination");
                        if ui.button("\u{2026}").clicked() {
                            crate::file_dialog::pick_path(picker, || {
                                rfd::FileDialog::new()
                                    .set_title("Choose Consolidation Destination")
                                    .pick_folder()
                            });
                        }
                        if let Some(crate::file_dialog::Picked::Path(dir)) = crate::file_dialog::take_path(picker) {
                            self.state.consolidate_dest_path =
                                dir.to_string_lossy().to_string();
                        }
                    });
                    ui.add_space(8.0);

                    let can_consolidate =
                        !self.state.consolidate_dest_path.trim().is_empty()
                            && !consolidation_active;

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_consolidate_dialog = false;
                        }
                        if ui
                            .add_enabled(
                                can_consolidate,
                                egui::Button::new("Consolidate"),
                            )
                            .clicked()
                        {
                            let dest = std::path::PathBuf::from(
                                self.state.consolidate_dest_path.trim(),
                            );
                            let include_proxies = self.state.consolidate_include_proxies;
                            let include_skims = self.state.consolidate_include_skims;
                            let create_zip = self.state.consolidate_create_zip;
                            let delete_folder_after_zip =
                                self.state.consolidate_delete_folder_after_zip;
                            self.start_consolidation(
                                dest,
                                include_proxies,
                                include_skims,
                                create_zip,
                                delete_folder_after_zip,
                                mode,
                            );
                        }
                    });
                });
            if !open {
                self.state.show_consolidate_dialog = false;
            }
            let _ = project_path; // silence unused warning in release builds
        }

        // 4c3. Project Info window
        if self.state.show_project_info {
            // Modal backdrop
            egui::Area::new(egui::Id::new("project_info_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(self.state.modal_backdrop_alpha));
                    if resp.clicked() { self.state.show_project_info = false; }
                });

            let mut still_open = true;
            let colour_before = (self.state.project_working_colorspace, self.state.project_grade_encoding);
            crate::panels::project_info::show_project_info(
                ctx,
                &mut self.state,
                &self.theme,
                &mut still_open,
            );
            if !still_open {
                self.state.show_project_info = false;
            }
            // A new working space or grade encoding changes the picture,
            // and a paused viewer redraws nothing on its own.
            if (self.state.project_working_colorspace, self.state.project_grade_encoding) != colour_before {
                self.sync_and_refresh_frame();
            }
        }

        // 4c3b. Colour Management (#466): the project's colour set-up on
        // one page.
        if self.state.show_colour_management {
            let mut open = true;
            let colour_before = (self.state.project_working_colorspace, self.state.project_grade_encoding);
            let calibration = self.prefs.color.viewer_lut_path.clone();
            crate::panels::colour_management::show_window(ctx, &mut self.state, &self.theme, calibration.as_deref(), &mut open, cmds);
            if !open {
                self.state.show_colour_management = false;
            }
            // A new working space or grade encoding changes the picture,
            // and a paused viewer redraws nothing on its own.
            if (self.state.project_working_colorspace, self.state.project_grade_encoding) != colour_before {
                self.sync_and_refresh_frame();
            }
        }

        // 4c4. File Info window — opens from the file browser right-click menu.
        if let Some(path) = self.state.file_info.clone() {
            // Modal backdrop
            egui::Area::new(egui::Id::new("file_info_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(self.state.modal_backdrop_alpha));
                    if resp.clicked() { self.state.file_info = None; }
                });

            // Lazy-load the thumbnail texture from the shared media-pool disk
            // cache. The cache key is path+size+mtime so it's automatically
            // shared with the file browser thumbnails — if the user has seen
            // this file in the browser, the cache hit is instant.
            if self.state.file_info_thumb.is_none() {
                if let Some(key) = klipp_engine::video_decoder::picture_cache_key(&path) {
                    if let Ok((rgba, w, h)) = klipp_media::disk_cache::load_thumbnail_cache(&key, crate::browser::FILE_THUMB_MAX_WIDTH) {
                        let pixels: Vec<egui::Color32> = rgba
                            .chunks_exact(4)
                            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                            .collect();
                        let image = egui::ColorImage {
                            size: [w as usize, h as usize],
                            pixels,
                        };
                        if let Some(old) = self.state.file_info_thumb.take() {
                            self.defer_drop_texture(old);
                        }
                        let tex = alloc_texture_logged(
                            ctx,
                            "file_info_thumb",
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.state.file_info_thumb = Some(tex);
                    }
                }
            }

            let mut still_open = true;
            crate::panels::file_info::show_file_info(
                ctx,
                &path,
                self.state.file_info_thumb.as_ref(),
                &self.theme,
                &mut still_open,
            );
            if !still_open {
                self.state.file_info = None;
                if let Some(tex) = self.state.file_info_thumb.take() {
                    self.defer_drop_texture(tex);
                }
            }
        }

        // 4c5. Quick Look overlay — Spacebar preview from the file browser.
        if let Some(ref ql_path) = self.state.quick_look_path.clone() {
            self.render_quick_look(ctx, ql_path);
        }

        // 4d. Preferences window
        if self.state.show_preferences {
            let (still_open, pref_cmds) = self.preferences_panel.show(
                ctx,
                &mut self.prefs,
                &mut self.prefs_dirty,
                &mut self.key_bindings,
                &self.theme_registry,
                &self.theme,
                self.state.snap,
            );
            cmds.extend(pref_cmds);
            // Sync skim_audio pref → state (checkbox may have changed)
            if self.state.audio_skimming_enabled != self.prefs.playback.skim_audio {
                self.state.audio_skimming_enabled = self.prefs.playback.skim_audio;
                if !self.state.audio_skimming_enabled && self.state.skim_audio_session.is_some() {
                    cmds.push(AppCommand::BrowserSkimAudioEnd);
                }
            }
            // Sync file-browser skim prefs → state. If audio skim was just
            // turned off mid-session, end the active session immediately so
            // the user gets the silence they asked for.
            if self.state.file_browser_video_skim_enabled != self.prefs.file_browser.file_browser_video_skim {
                self.state.file_browser_video_skim_enabled = self.prefs.file_browser.file_browser_video_skim;
            }
            if self.state.file_browser_audio_skim_enabled != self.prefs.file_browser.file_browser_audio_skim {
                self.state.file_browser_audio_skim_enabled = self.prefs.file_browser.file_browser_audio_skim;
                if !self.state.file_browser_audio_skim_enabled && self.state.skim_audio_session.is_some() {
                    cmds.push(AppCommand::BrowserSkimAudioEnd);
                }
            }
            if self.state.show_recent_files != self.prefs.file_browser.show_recent_files {
                self.state.show_recent_files = self.prefs.file_browser.show_recent_files;
            }
            if self.state.mute_reminder_interval_secs != self.prefs.audio_hardware.mute_reminder_interval_secs {
                self.state.mute_reminder_interval_secs = self.prefs.audio_hardware.mute_reminder_interval_secs;
            }
            if self.state.filmstrip_hover_enabled != self.prefs.file_browser.filmstrip_hover {
                self.state.filmstrip_hover_enabled = self.prefs.file_browser.filmstrip_hover;
            }
            if self.state.filmstrip_quality != self.prefs.file_browser.filmstrip_quality {
                self.state.filmstrip_quality = self.prefs.file_browser.filmstrip_quality;
            }
            if self.state.filmstrip_persist != self.prefs.file_browser.filmstrip_persist {
                self.state.filmstrip_persist = self.prefs.file_browser.filmstrip_persist;
            }
            if self.state.file_browser_auto_refresh != self.prefs.file_browser.auto_refresh {
                self.state.file_browser_auto_refresh = self.prefs.file_browser.auto_refresh;
            }
            if !still_open {
                self.state.show_preferences = false;
            }
        }

    }

    /// Render the Quick Look fullscreen overlay. Shows a dark backdrop with
    /// the preview image/waveform centered and aspect-fit to the screen,
    /// plus a small metadata overlay at the bottom. Dismisses on Escape,
    /// Spacebar, or backdrop click. Arrow keys navigate to adjacent files.
    fn render_quick_look(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        let screen = ctx.screen_rect();

        // ── Lazy-load texture and header info on first frame ──
        if self.state.quick_look_texture.is_none() && self.state.quick_look_info.is_none() {
            // Probe header for metadata overlay.
            self.state.quick_look_info = klipp_engine::probe_media_header(path).ok();

            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let is_audio = klipp_core::media_extensions::AUDIO_EXTENSIONS.contains(&ext.as_str());

            if is_audio {
                // Start audio playback session.
                if self.state.quick_look_audio.is_none() {
                    self.state.quick_look_audio =
                        klipp_engine::skim_audio::SkimAudioSession::new_from_source_path(
                            path.to_path_buf(),
                            self.cpal_sample_rate,
                            self.skim_consumer.clone(),
                            if self.state.master_muted { 0.0 } else { self.state.master_volume },
                            0.0,
                        );
                }
            } else {
                // Decode a poster frame (video or image) at up to 1920px wide.
                let max_w = (screen.width() as u32).min(1920);
                if let Ok((rgba, w, h)) =
                    klipp_engine::generate_thumbnail_at_size(path, 0.0, max_w)
                {
                    let pixels: Vec<egui::Color32> = rgba
                        .chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [w as usize, h as usize],
                        pixels,
                    };
                    if let Some(old) = self.state.quick_look_texture.take() {
                        self.defer_drop_texture(old);
                    }
                    let tex = alloc_texture_logged(
                        ctx,
                        "quick_look_preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.state.quick_look_texture = Some(tex);
                }
            }
        }

        // ── Backdrop ──
        egui::Area::new(egui::Id::new("quick_look_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(220),
                );

                // ── Image / video preview ──
                if let Some(ref tex) = self.state.quick_look_texture {
                    let tex_size = tex.size();
                    let img_aspect = tex_size[0] as f32 / tex_size[1].max(1) as f32;
                    // Fit within 90% of screen to leave room for the info bar.
                    let max_w = screen.width() * 0.9;
                    let max_h = screen.height() * 0.85;
                    let (w, h) = if img_aspect > max_w / max_h {
                        (max_w, max_w / img_aspect)
                    } else {
                        (max_h * img_aspect, max_h)
                    };
                    let img_rect = egui::Rect::from_center_size(
                        egui::pos2(screen.center().x, screen.center().y - 16.0),
                        egui::vec2(w, h),
                    );
                    ui.painter().image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else if self.state.quick_look_audio.is_some() {
                    // ── Audio waveform visualization ──
                    // Draw a large waveform in the center of the screen
                    // using the browser's cached audio peaks (if available).
                    let wave_rect = egui::Rect::from_center_size(
                        screen.center(),
                        egui::vec2(screen.width() * 0.7, screen.height() * 0.3),
                    );
                    ui.painter().rect_filled(
                        wave_rect,
                        8.0,
                        egui::Color32::from_rgba_unmultiplied(30, 30, 36, 200),
                    );
                    // Large audio icon in center.
                    ui.painter().text(
                        wave_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "\u{1F3B5}",
                        egui::FontId::proportional(64.0),
                        self.theme.text_accent,
                    );
                }

                // ── Metadata overlay at the bottom ──
                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut info_parts: Vec<String> = vec![filename];
                if let Some(ref info) = self.state.quick_look_info {
                    if let (Some(w), Some(h)) = (info.width, info.height) {
                        info_parts.push(format!("{w}\u{00d7}{h}"));
                    }
                    if let Some(ref codec) = info.codec {
                        info_parts.push(codec.to_string());
                    }
                    if let Some(fps) = info.fps {
                        info_parts.push(format!("{fps:.2} fps"));
                    }
                    if let Some(dur) = info.duration_secs {
                        let m = dur as u32 / 60;
                        let s = dur as u32 % 60;
                        info_parts.push(format!("{m}:{s:02}"));
                    }
                }
                if let Ok(meta) = std::fs::metadata(path) {
                    let size = meta.len();
                    if size > 0 {
                        let size_str = if size < 1024 * 1024 {
                            format!("{:.0} KB", size as f64 / 1024.0)
                        } else if size < 1024 * 1024 * 1024 {
                            format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
                        } else {
                            format!("{:.2} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
                        };
                        info_parts.push(size_str);
                    }
                }
                let info_text = info_parts.join("  \u{00b7}  ");
                let bar_rect = egui::Rect::from_min_max(
                    egui::pos2(screen.min.x, screen.max.y - 40.0),
                    screen.max,
                );
                ui.painter().rect_filled(
                    bar_rect,
                    0.0,
                    egui::Color32::from_black_alpha(180),
                );
                ui.painter().text(
                    egui::pos2(bar_rect.center().x, bar_rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    &info_text,
                    egui::FontId::proportional(12.0),
                    self.theme.text_primary,
                );

                // ── Dismiss: click backdrop, Escape, or Spacebar ──
                let dismiss = resp.clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    || ui.input(|i| i.key_pressed(egui::Key::Space));
                if dismiss {
                    if let Some(ref session) = self.state.quick_look_audio {
                        session.stop();
                    }
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.quick_look_path = None;
                    if let Some(old) = self.state.quick_look_texture.take() {
                        self.defer_drop_texture(old);
                    }
                    self.state.quick_look_info = None;
                    self.state.quick_look_audio = None;
                }

                // ── Arrow keys: navigate to next/previous file ──
                let arrow_up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                let arrow_down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                if (arrow_up || arrow_down) && !dismiss {
                    // Find the current file in the browser's entry list
                    // and move to the adjacent non-directory entry.
                    let entries = &self.browser.files_entries;
                    if let Some(cur_idx) = entries.iter().position(|e| e.path == *path) {
                        let next = if arrow_down {
                            entries[cur_idx + 1..]
                                .iter()
                                .find(|e| !e.is_dir)
                                .map(|e| e.path.clone())
                        } else {
                            entries[..cur_idx]
                                .iter()
                                .rev()
                                .find(|e| !e.is_dir)
                                .map(|e| e.path.clone())
                        };
                        if let Some(next_path) = next {
                            // Stop current audio session if any.
                            if let Some(ref session) = self.state.quick_look_audio {
                                session.stop();
                            }
                            klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                            klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                            // Update selection in the browser too.
                            self.browser.files_selected.clear();
                            self.browser.files_selected.insert(next_path.clone());
                            self.browser.files_selection_anchor = Some(next_path.clone());
                            // Switch to the new file — lazy load on next frame.
                            self.state.quick_look_path = Some(next_path);
                            if let Some(old) = self.state.quick_look_texture.take() {
                                self.defer_drop_texture(old);
                            }
                            self.state.quick_look_info = None;
                            self.state.quick_look_audio = None;
                        }
                    }
                }
            });
    }
}
