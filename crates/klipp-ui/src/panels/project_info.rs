use crate::state::AppState;
use crate::theme::KlippTheme;

/// Show the Project Info window. Returns true if still open.
pub fn show_project_info(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    open: &mut bool,
) {
    egui::Window::new("Project Info")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .default_pos(ctx.screen_rect().center() - egui::vec2(170.0, 200.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);

            let project_name = state.project_path.as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into());

            // Project name
            info_row(ui, theme, "Project", &project_name);

            // Location
            if let Some(ref path) = state.project_path {
                info_row(ui, theme, "Location", &path.to_string_lossy());
            }

            ui.separator();

            // Created
            if let Some(ref created) = state.created_at {
                info_row(ui, theme, "Created", format_date(created));
            }

            // Sequences
            info_row(ui, theme, "Sequences", &state.sequences.len().to_string());

            // Active sequence info
            let seq = state.sequence();
            info_row(ui, theme, "Active Sequence", &seq.name);

            // Duration
            let dur = state.sequence_duration().to_secs_f64();
            let mins = (dur / 60.0).floor() as u32;
            let secs = (dur % 60.0).floor() as u32;
            let frames = ((dur % 1.0) * seq.fps.rate() as f64).floor() as u32;
            info_row(ui, theme, "Duration", &format!("{mins}:{secs:02}.{frames:02}"));

            // Resolution
            info_row(
                ui, theme, "Resolution",
                &format!("{}x{}", seq.resolution.width, seq.resolution.height),
            );

            // Frame rate
            let fps_str = if seq.fps.den == 1 {
                format!("{} fps", seq.fps.num)
            } else {
                format!("{:.3} fps", seq.fps.num as f64 / seq.fps.den as f64)
            };
            info_row(ui, theme, "Frame Rate", &fps_str);

            ui.separator();

            // Media
            let media_count = state.media_pool.len();
            let total_size: u64 = state.media_pool.all()
                .map(|m| m.file_size_bytes)
                .sum();
            info_row(ui, theme, "Media Items", &media_count.to_string());
            info_row(ui, theme, "Total Media Size", &format_size(total_size));

            // Track count
            let video_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
            let audio_tracks = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            info_row(ui, theme, "Tracks", &format!("{video_tracks}V + {audio_tracks}A"));

            // Clip count
            let clip_count: usize = seq.tracks.iter().map(|t| t.clips.len()).sum();
            info_row(ui, theme, "Clips", &clip_count.to_string());

            // Proxy/skim count
            let proxy_count = state.media_pool.all().filter(|m| m.proxy_ready).count();
            let skim_count = state.media_pool.all().filter(|m| m.skim_file.is_some()).count();
            if proxy_count > 0 {
                info_row(ui, theme, "Proxies", &format!("{proxy_count} files"));
            }
            if skim_count > 0 {
                info_row(ui, theme, "Skim files", &format!("{skim_count} files"));
            }

            ui.separator();
            ui.label(
                egui::RichText::new("Project Settings")
                    .strong()
                    .size(12.0)
                    .color(theme.text_primary),
            );
            ui.add_space(2.0);

            // Filmstrip generation override
            let mut filmstrip_mode = tri_state_index(state.project_auto_filmstrips);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Filmstrip generation:")
                        .size(11.0)
                        .color(theme.text_dim),
                );
                egui::ComboBox::from_id_salt("proj_filmstrip")
                    .width(120.0)
                    .selected_text(tri_state_label(filmstrip_mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut filmstrip_mode, 0, "Use global setting");
                        ui.selectable_value(&mut filmstrip_mode, 1, "Always on");
                        ui.selectable_value(&mut filmstrip_mode, 2, "Always off");
                    });
            });
            let new_filmstrip = tri_state_from_index(filmstrip_mode);
            if new_filmstrip != state.project_auto_filmstrips {
                state.project_auto_filmstrips = new_filmstrip;
                state.dirty = true;
            }

            // Skim transcode override
            let mut skim_mode = tri_state_index(state.project_auto_skim);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Skim transcodes:")
                        .size(11.0)
                        .color(theme.text_dim),
                );
                egui::ComboBox::from_id_salt("proj_skim")
                    .width(120.0)
                    .selected_text(tri_state_label(skim_mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut skim_mode, 0, "Use global setting");
                        ui.selectable_value(&mut skim_mode, 1, "Always on");
                        ui.selectable_value(&mut skim_mode, 2, "Always off");
                    });
            });
            let new_skim = tri_state_from_index(skim_mode);
            if new_skim != state.project_auto_skim {
                state.project_auto_skim = new_skim;
                state.dirty = true;
            }

            // Working colorspace. The grade chain operates in this
            // basis; decoded frames are converted to it; the display
            // path (RRT + ODT) consumes it on the way out. Defaults
            // to Rec.709 for back-compat with any project on disk.
            // Switching it changes the look of the existing grade —
            // there's no in-place migration in this dialog, just the
            // setting.
            let mut ws = state.project_working_colorspace;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Working colorspace:")
                        .size(11.0)
                        .color(theme.text_dim),
                );
                egui::ComboBox::from_id_salt("proj_ws")
                    .width(140.0)
                    .selected_text(working_colorspace_label(ws))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut ws,
                            klipp_core::WorkingColorspace::Rec709Linear,
                            "Rec.709 / sRGB (linear)",
                        );
                        ui.selectable_value(
                            &mut ws,
                            klipp_core::WorkingColorspace::AcesCG,
                            "ACEScg (AP1)",
                        );
                        ui.selectable_value(
                            &mut ws,
                            klipp_core::WorkingColorspace::Bt2020Linear,
                            "BT.2020 (linear)",
                        );
                    });
            });
            // A switch with grades in the project asks first (#902): a naked
            // switch re-bases every grade and changes its look.
            let pending_id = ui.id().with("proj_ws_pending");
            if ws != state.project_working_colorspace {
                let graded = state.sequences.iter().flat_map(|s| s.all_clips()).any(|c| !c.node_graph.is_identity());
                if graded {
                    ui.data_mut(|d| d.insert_temp(pending_id, ws));
                } else {
                    switch_working_space(state, ws, false);
                }
            }
            if let Some(to) = ui.data(|d| d.get_temp::<klipp_core::WorkingColorspace>(pending_id)) {
                ui.label(egui::RichText::new(format!("Switch to {}? The project has grades.", working_colorspace_label(to))).size(11.0));
                ui.horizontal(|ui| {
                    let mut done = false;
                    if ui.button("Convert grades (keep the look)").clicked() {
                        switch_working_space(state, to, true);
                        done = true;
                    }
                    if ui.button("Switch without converting").on_hover_text("Each grade is re-based on the new space and will look different").clicked() {
                        switch_working_space(state, to, false);
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                    if done {
                        ui.data_mut(|d| d.remove::<klipp_core::WorkingColorspace>(pending_id));
                    }
                });
            }
            // What the grade's controls act in (#467). The ACES log
            // encodings are ACEScg's; elsewhere the grade is BT.1886.
            let mut enc = state.project_grade_encoding;
            let aces = state.project_working_colorspace == klipp_core::WorkingColorspace::AcesCG;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Grade in:").size(11.0).color(theme.text_dim));
                ui.add_enabled_ui(aces, |ui| {
                    egui::ComboBox::from_id_salt("proj_grade_encoding")
                        .width(140.0)
                        .selected_text(grade_encoding_label(if aces { enc } else { klipp_core::GradeEncoding::Bt1886 }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut enc, klipp_core::GradeEncoding::Bt1886, grade_encoding_label(klipp_core::GradeEncoding::Bt1886));
                            ui.selectable_value(&mut enc, klipp_core::GradeEncoding::AcesCct, grade_encoding_label(klipp_core::GradeEncoding::AcesCct));
                            ui.selectable_value(&mut enc, klipp_core::GradeEncoding::AcesCc, grade_encoding_label(klipp_core::GradeEncoding::AcesCc));
                        })
                        .response
                        .on_hover_text("What lift, gamma, gain, offset, contrast and the tone controls act in. ACEScct is what most colourists grade ACES in: offset is an exposure and the wheels move evenly across the stops. ACEScc is the same without the toe in the shadows.")
                        .on_disabled_hover_text("ACEScct and ACEScc are ACES encodings: choose ACEScg as the working colorspace to grade in them");
                });
            });
            if enc != state.project_grade_encoding {
                state.project_grade_encoding = enc;
                state.dirty = true;
                // The executors read it with the working space each frame.
                klipp_core::set_grade_encoding(enc);
            }
            ui.label(
                egui::RichText::new(
                    "Switching working colorspace re-bases the entire grade \
                     chain. With grades in the project the dropdown asks \
                     whether to convert them, wrapping each clip's chain \
                     with primaries-conversion nodes so the look is kept; \
                     the button below does that conversion directly.",
                )
                .size(10.0)
                .italics()
                .color(theme.text_dim),
            );

            // Pick the most likely migration target. Rec.709 is where a
            // wide-gamut project goes to come back; a Rec.709 project is
            // most likely heading for ACEScg, the well-tested wide path.
            // Every pair has a matrix now, so the button no longer has a
            // dead end to hide from (it did while BT.2020 was a stub).
            let from = state.project_working_colorspace;
            let target = match from {
                klipp_core::WorkingColorspace::Rec709Linear => klipp_core::WorkingColorspace::AcesCG,
                klipp_core::WorkingColorspace::AcesCG
                | klipp_core::WorkingColorspace::Bt2020Linear => klipp_core::WorkingColorspace::Rec709Linear,
            };
            let label = format!(
                "Convert grades to {} (preserve look)",
                working_colorspace_label(target),
            );
            if ui.button(label).clicked() {
                switch_working_space(state, target, true);
            }
        });
}

/// The project's working colorspace switched to `to`. With `convert`,
/// each clip's grade is wrapped in primaries conversions so it keeps its
/// look, and a toast says how many; without, the grades are re-based as
/// they are. Pushed to the engine at once: the decoder reads it per frame,
/// and without the push the change waits for the next project load.
fn switch_working_space(state: &mut AppState, to: klipp_core::WorkingColorspace, convert: bool) {
    let from = state.project_working_colorspace;
    if from == to {
        return;
    }
    if convert {
        let mut migrated = 0usize;
        for sequence in &mut state.sequences {
            for track in &mut sequence.tracks {
                for clip in &mut track.clips {
                    if clip.node_graph.migrate_working_space(from, to) {
                        migrated += 1;
                    }
                }
            }
        }
        state.toast.show(format!("Migrated {migrated} clip{} to {}", if migrated == 1 { "" } else { "s" }, working_colorspace_label(to)));
    }
    state.project_working_colorspace = to;
    state.dirty = true;
    klipp_engine::video_decoder::set_working_colorspace(to);
}

fn grade_encoding_label(enc: klipp_core::GradeEncoding) -> &'static str {
    match enc {
        klipp_core::GradeEncoding::Bt1886 => "Display gamma (BT.1886)",
        klipp_core::GradeEncoding::AcesCct => "ACEScct",
        klipp_core::GradeEncoding::AcesCc => "ACEScc",
    }
}

fn working_colorspace_label(ws: klipp_core::WorkingColorspace) -> &'static str {
    match ws {
        klipp_core::WorkingColorspace::Rec709Linear => "Rec.709 / sRGB (linear)",
        klipp_core::WorkingColorspace::AcesCG => "ACEScg (AP1)",
        klipp_core::WorkingColorspace::Bt2020Linear => "BT.2020 (linear)",
    }
}

fn info_row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{label}:"))
                .size(12.0)
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(value)
                .size(12.0)
                .color(theme.text_primary),
        );
    });
}

/// Convert Option<bool> to combo index: 0=None, 1=Some(true), 2=Some(false)
fn tri_state_index(v: Option<bool>) -> usize {
    match v {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

fn tri_state_from_index(i: usize) -> Option<bool> {
    match i {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

fn tri_state_label(i: usize) -> &'static str {
    match i {
        1 => "Always on",
        2 => "Always off",
        _ => "Use global setting",
    }
}

fn format_date(iso: &str) -> &str {
    // Show just the date portion (YYYY-MM-DD) from ISO 8601
    if iso.len() >= 10 { &iso[..10] } else { iso }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod working_space_tests {
    use super::switch_working_space;
    use crate::state::AppState;
    use klipp_core::WorkingColorspace;

    /// A project with one graded clip, in Rec.709 linear.
    fn graded() -> AppState {
        let mut state = AppState::default();
        let tc = |v| klipp_core::RationalTime { value: v, rate: 24 };
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0), tc(24), tc(0), tc(24));
        let n = clip.node_graph.ensure_corrector();
        clip.node_graph.nodes.iter_mut().find(|x| x.id == n).unwrap().params.exposure = Some(0.5);
        state.sequences[0].tracks[0].clips.push(clip);
        state.project_working_colorspace = WorkingColorspace::Rec709Linear;
        state
    }

    fn grade(state: &AppState) -> klipp_nodes::NodeGraph {
        state.sequences[0].tracks[0].clips[0].node_graph.clone()
    }

    #[test]
    fn converting_wraps_the_grades_and_a_plain_switch_leaves_them() {
        let mut converted = graded();
        let before = grade(&converted).nodes.len();
        switch_working_space(&mut converted, WorkingColorspace::AcesCG, true);
        assert_eq!(converted.project_working_colorspace, WorkingColorspace::AcesCG);
        assert!(grade(&converted).nodes.len() > before, "the grade is wrapped in conversions");

        let mut plain = graded();
        let untouched = grade(&plain).nodes.len();
        switch_working_space(&mut plain, WorkingColorspace::AcesCG, false);
        assert_eq!(plain.project_working_colorspace, WorkingColorspace::AcesCG);
        assert_eq!(grade(&plain).nodes.len(), untouched, "a plain switch leaves the grade as it is");
    }
}
