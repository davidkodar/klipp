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

            // Colour: one line saying the set-up, which opens Colour
            // Management, where it is set (#466).
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour:").size(11.0).color(theme.text_dim));
                let summary = crate::panels::colour_management::summary(state);
                if ui.add(egui::Button::new(egui::RichText::new(summary).size(11.0).color(theme.text_secondary)).frame(false))
                    .on_hover_text("Colour Management\u{2026}")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    state.show_colour_management = true;
                }
            });
        });
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
    use crate::panels::colour_management::switch_working_space;
    use crate::state::AppState;
    use klipp_core::WorkingColorspace;
    use klipp_timeline::GradeAccess;

    /// A project with one graded clip, in Rec.709 linear.
    fn graded() -> AppState {
        let mut state = AppState::default();
        let tc = |v| klipp_core::RationalTime { value: v, rate: 24 };
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0), tc(24), tc(0), tc(24));
        let n = clip.edit_grade(|g| g.ensure_corrector());
        clip.edit_grade(|g| g.nodes.iter_mut().find(|x| x.id == n).unwrap().params.exposure = Some(0.5));
        state.sequences[0].tracks[0].clips.push(clip);
        state.project_working_colorspace = WorkingColorspace::Rec709Linear;
        state
    }

    fn grade(state: &AppState) -> klipp_nodes::NodeGraph {
        state.sequences[0].tracks[0].clips[0].grade().clone()
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

    /// #466: Project Info says the colour set-up in a line, set nowhere
    /// but Colour Management, which the line opens.
    #[test]
    fn project_info_shows_the_colour_summary_and_opens_colour_management() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let theme = crate::theme::KlippTheme::default();
        let mut open = true;
        let mut run = |state: &mut AppState, events: Vec<egui::Event>| {
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 1200.0))), events, ..Default::default() }, |ctx| {
                super::show_project_info(ctx, state, &theme, &mut open);
            });
            out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            }).collect::<Vec<_>>()
        };
        run(&mut state, Vec::new());
        let texts = run(&mut state, Vec::new());
        let summary = crate::panels::colour_management::summary(&state);
        assert!(!texts.iter().any(|(t, _)| t.starts_with("Working colorspace")), "not set here any more");
        let at = texts.iter().find(|(t, _)| *t == summary).unwrap_or_else(|| panic!("{summary} in {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>())).1;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        run(&mut state, vec![egui::Event::PointerMoved(at), button(true)]);
        run(&mut state, vec![button(false)]);
        assert!(state.show_colour_management);
    }
}
