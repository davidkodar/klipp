//! Colour Management (#466): the project's colour settings on one page,
//! in the order the picture goes through them: how it is graded, and
//! what goes out. A page component, so it can sit in a window of its own
//! now and in a Project Settings window later.

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, GradeEncoding, OdtChoice, WorkingColorspace};
use klipp_timeline::GradeAccess;

/// The working spaces, as the page lists them.
const WORKING_SPACES: [WorkingColorspace; 3] = [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear];

pub fn working_colorspace_label(ws: WorkingColorspace) -> &'static str {
    match ws {
        WorkingColorspace::Rec709Linear => "Rec.709 / sRGB (linear)",
        WorkingColorspace::AcesCG => "ACEScg (AP1)",
        WorkingColorspace::Bt2020Linear => "BT.2020 (linear)",
    }
}

pub fn grade_encoding_label(enc: GradeEncoding) -> &'static str {
    match enc {
        GradeEncoding::Bt1886 => "Display gamma (BT.1886)",
        GradeEncoding::AcesCct => "ACEScct",
        GradeEncoding::AcesCc => "ACEScc",
    }
}

/// The project's colour set-up in a line, for Project Info: "ACEScg ·
/// ACEScct · Rec.709 SDR out · LUT".
pub fn summary(state: &AppState) -> String {
    let ws = match state.project_working_colorspace {
        WorkingColorspace::Rec709Linear => "Rec.709 linear",
        WorkingColorspace::AcesCG => "ACEScg",
        WorkingColorspace::Bt2020Linear => "BT.2020 linear",
    };
    let mut parts = vec![ws.to_string()];
    if state.project_working_colorspace == WorkingColorspace::AcesCG {
        parts.push(match state.project_grade_encoding {
            GradeEncoding::Bt1886 => "BT.1886",
            GradeEncoding::AcesCct => "ACEScct",
            GradeEncoding::AcesCc => "ACEScc",
        }.to_string());
    }
    parts.push(format!("{} out", state.project_default_odt.label()));
    if state.global_lut_path.is_some() && state.global_lut_enabled {
        parts.push("output LUT".into());
    }
    parts.join(" \u{00B7} ")
}

/// The window around the page.
pub fn show_window(ctx: &egui::Context, state: &mut AppState, theme: &KlippTheme, open: &mut bool, cmds: &mut Vec<AppCommand>) {
    egui::Window::new("Colour Management")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| show_page(ui, state, theme, cmds));
}

/// A group's heading and the sentence under it.
fn group(ui: &mut egui::Ui, theme: &KlippTheme, title: &str, help: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).size(crate::theme::type_scale::CAPTION).strong().color(theme.text_dim));
    ui.label(egui::RichText::new(help).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
    ui.add_space(2.0);
}

/// A row: its label in the label column, then its control.
fn row<R>(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(110.0, crate::theme::control::REGULAR), egui::Sense::hover());
        ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(crate::theme::type_scale::BODY), theme.text_secondary);
        add(ui)
    })
    .inner
}

/// The page: the working space and grade encoding, then the output
/// transform new exports start from and the output LUT.
pub fn show_page(ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);

    // ── Working space ──
    group(ui, theme, "WORKING SPACE", "What the grade works in. Switching it re-bases every grade; with grades in the project you are asked whether to keep their look.");
    let mut ws = state.project_working_colorspace;
    row(ui, theme, "Working space", |ui| {
        egui::ComboBox::from_id_salt("cm_working_space")
            .width(200.0)
            .selected_text(working_colorspace_label(ws))
            .show_ui(ui, |ui| {
                for choice in WORKING_SPACES {
                    ui.selectable_value(&mut ws, choice, working_colorspace_label(choice));
                }
            });
    });
    // A switch with grades in the project asks first (#902): a naked
    // switch re-bases every grade and changes its look.
    let pending_id = ui.id().with("cm_ws_pending");
    if ws != state.project_working_colorspace {
        let graded = state.sequences.iter().flat_map(|s| s.all_clips()).any(|c| !c.grade().is_identity());
        if graded {
            ui.data_mut(|d| d.insert_temp(pending_id, ws));
        } else {
            switch_working_space(state, ws, false);
        }
    }
    if let Some(to) = ui.data(|d| d.get_temp::<WorkingColorspace>(pending_id)) {
        ui.label(egui::RichText::new(format!("Switch to {}? The project has grades.", working_colorspace_label(to))).size(crate::theme::type_scale::BODY));
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
                ui.data_mut(|d| d.remove::<WorkingColorspace>(pending_id));
            }
        });
    }
    // What the grade's controls act in (#467). The ACES log encodings are
    // ACEScg's; elsewhere the grade is BT.1886.
    let mut enc = state.project_grade_encoding;
    let aces = state.project_working_colorspace == WorkingColorspace::AcesCG;
    row(ui, theme, "Grade in", |ui| {
        ui.add_enabled_ui(aces, |ui| {
            egui::ComboBox::from_id_salt("cm_grade_encoding")
                .width(200.0)
                .selected_text(grade_encoding_label(if aces { enc } else { GradeEncoding::Bt1886 }))
                .show_ui(ui, |ui| {
                    for choice in [GradeEncoding::Bt1886, GradeEncoding::AcesCct, GradeEncoding::AcesCc] {
                        ui.selectable_value(&mut enc, choice, grade_encoding_label(choice));
                    }
                })
                .response
                .on_hover_text("What lift, gamma, gain, offset, contrast and the tone controls act in. ACEScct is what most colourists grade ACES in: offset is an exposure and the wheels move evenly across the stops. ACEScc is the same without the toe in the shadows.")
                .on_disabled_hover_text("ACEScct and ACEScc are ACES encodings: choose ACEScg as the working space to grade in them");
        });
    });
    if enc != state.project_grade_encoding {
        state.project_grade_encoding = enc;
        state.dirty = true;
        // The executors read it with the working space each frame.
        klipp_core::set_grade_encoding(enc);
    }

    // ── Output ──
    group(ui, theme, "OUTPUT", "What new exports start from, and the LUT applied to the viewer and every export.");
    let mut odt = state.project_default_odt;
    row(ui, theme, "Output transform", |ui| {
        egui::ComboBox::from_id_salt("cm_default_odt")
            .width(200.0)
            .selected_text(odt.label())
            .show_ui(ui, |ui| {
                for choice in OdtChoice::ALL {
                    if choice.suits_working_space(state.project_working_colorspace) {
                        ui.selectable_value(&mut odt, choice, choice.label());
                    }
                }
            });
    });
    if odt != state.project_default_odt {
        state.project_default_odt = odt;
        state.dirty = true;
    }
    row(ui, theme, "Output LUT", |ui| {
        match state.global_lut_path.as_ref() {
            Some(path) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut enabled = state.global_lut_enabled;
                if ui.checkbox(&mut enabled, "").on_hover_text("Apply the output LUT").changed() {
                    cmds.push(AppCommand::SetGlobalLutEnabled(enabled));
                }
                ui.label(egui::RichText::new(name).color(if enabled { theme.text_primary } else { theme.text_dim })).on_hover_text(path.display().to_string());
                if ui.small_button("Clear").clicked() {
                    cmds.push(AppCommand::ClearGlobalLut);
                }
            }
            None => {
                ui.label(egui::RichText::new("None").color(theme.text_dim));
            }
        }
        if ui.small_button("Choose\u{2026}").clicked() {
            crate::file_dialog::spawn(crate::inspector::color_and_luts::pick_global_lut);
        }
    });
}

/// The project's working space switched to `to`. With `convert`, each
/// clip's grade is wrapped in primaries conversions so it keeps its
/// look, and a toast says how many; without, the grades are re-based as
/// they are. Pushed to the engine at once: the decoder reads it per frame,
/// and without the push the change waits for the next project load.
pub fn switch_working_space(state: &mut AppState, to: WorkingColorspace, convert: bool) {
    let from = state.project_working_colorspace;
    if from == to {
        return;
    }
    if convert {
        let mut migrated = 0usize;
        for sequence in &mut state.sequences {
            for track in &mut sequence.tracks {
                for clip in &mut track.clips {
                    if clip.edit_grade(|g| g.migrate_working_space(from, to)) {
                        migrated += 1;
                    }
                }
            }
        }
        state.toast.show(format!("Migrated {migrated} clip{} to {}", if migrated == 1 { "" } else { "s" }, working_colorspace_label(to)));
    }
    state.project_working_colorspace = to;
    // An output transform the new space cannot use goes back to the default.
    if !state.project_default_odt.suits_working_space(to) {
        state.project_default_odt = OdtChoice::MatchViewer;
    }
    state.dirty = true;
    klipp_engine::video_decoder::set_working_colorspace(to);
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::GradeAccess;
    use std::path::PathBuf;

    type Texts = Vec<(String, egui::Rect)>;

    /// One frame of `run` with `events`: the commands pushed, the texts
    /// drawn, and the filled rectangles.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, run: &mut dyn FnMut(&egui::Context, &mut Vec<AppCommand>)) -> (Vec<AppCommand>, Texts) {
        let mut cmds = Vec::new();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0))), events, ..Default::default() }, |ctx| run(ctx, &mut cmds));
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size()))),
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

    fn at(texts: &Texts, label: &str) -> egui::Pos2 {
        texts.iter().rev().find(|(t, _)| t == label).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("{label} is drawn: {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>()))
    }

    fn page(state: &mut AppState) -> impl FnMut(&egui::Context, &mut Vec<AppCommand>) + '_ {
        let theme = KlippTheme::default();
        move |ctx, cmds| {
            egui::CentralPanel::default().show(ctx, |ui| show_page(ui, state, &theme, cmds));
        }
    }

    /// Pick `choice` from the combo now showing `shown`.
    fn choose(ctx: &egui::Context, run: &mut dyn FnMut(&egui::Context, &mut Vec<AppCommand>), shown: &str, choice: &str) -> Texts {
        let texts = frame(ctx, Vec::new(), run).1;
        click(ctx, at(&texts, shown), run);
        let texts = frame(ctx, Vec::new(), run).1;
        click(ctx, at(&texts, choice), run);
        frame(ctx, Vec::new(), run).1
    }

    /// The engine globals the page sets, put back for other tests.
    fn restore_globals() {
        klipp_engine::video_decoder::set_working_colorspace(WorkingColorspace::Rec709Linear);
        klipp_core::set_grade_encoding(GradeEncoding::Bt1886);
    }

    /// #466: the working space is set here: at once in a project with no
    /// grades; with grades it asks, Cancel keeps it, and Convert switches.
    #[test]
    fn the_working_space_is_set_here_and_asks_when_there_are_grades() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        {
            let mut run = page(&mut state);
            choose(&ctx, &mut run, "Rec.709 / sRGB (linear)", "ACEScg (AP1)");
        }
        assert_eq!(state.project_working_colorspace, WorkingColorspace::AcesCG, "no grades: switched at once");
        assert!(state.dirty);

        // A graded clip in the project.
        let mut clip = klipp_timeline::Clip::gap(klipp_core::Timecode::from_secs_f64(0.0, 24), klipp_core::Timecode::from_secs_f64(1.0, 24));
        let id = clip.edit_grade(|g| g.ensure_corrector());
        assert!(clip.set_grade_param(id, "exposure", 0.5));
        assert!(!clip.grade().is_identity());
        state.sequences[0].tracks[0].clips.push(clip);
        {
            let mut run = page(&mut state);
            let texts = choose(&ctx, &mut run, "ACEScg (AP1)", "BT.2020 (linear)");
            assert!(texts.iter().any(|(t, _)| t == "Switch to BT.2020 (linear)? The project has grades."), "it asks");
            click(&ctx, at(&texts, "Cancel"), &mut run);
        }
        assert_eq!(state.project_working_colorspace, WorkingColorspace::AcesCG, "Cancel keeps it");
        {
            let mut run = page(&mut state);
            let texts = choose(&ctx, &mut run, "ACEScg (AP1)", "BT.2020 (linear)");
            click(&ctx, at(&texts, "Convert grades (keep the look)"), &mut run);
        }
        assert_eq!(state.project_working_colorspace, WorkingColorspace::Bt2020Linear);
        restore_globals();
    }

    /// #466: Grade in is offered in ACEScg only, and sets the encoding.
    #[test]
    fn grade_in_is_set_here_in_acescg() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        {
            let mut run = page(&mut state);
            let texts = frame(&ctx, Vec::new(), &mut run).1;
            click(&ctx, at(&texts, "Display gamma (BT.1886)"), &mut run);
            let texts = frame(&ctx, Vec::new(), &mut run).1;
            assert!(!texts.iter().any(|(t, _)| t == "ACEScct"), "not offered outside ACEScg");
        }
        assert_eq!(state.project_grade_encoding, GradeEncoding::Bt1886);
        state.project_working_colorspace = WorkingColorspace::AcesCG;
        {
            let mut run = page(&mut state);
            choose(&ctx, &mut run, "Display gamma (BT.1886)", "ACEScct");
        }
        assert_eq!(state.project_grade_encoding, GradeEncoding::AcesCct);
        restore_globals();
    }

    /// #466: the output transform new exports start from is set here,
    /// from those the working space can use.
    #[test]
    fn the_default_output_transform_is_set_here() {
        let ctx = egui::Context::default();
        let mut state = AppState { project_working_colorspace: WorkingColorspace::AcesCG, ..Default::default() };
        {
            let mut run = page(&mut state);
            let texts = frame(&ctx, Vec::new(), &mut run).1;
            click(&ctx, at(&texts, "Match Viewer"), &mut run);
            let texts = frame(&ctx, Vec::new(), &mut run).1;
            assert!(!texts.iter().any(|(t, _)| t == OdtChoice::Srgb.label()), "the legacy sRGB curve is Rec.709's only");
            click(&ctx, at(&texts, OdtChoice::Rec2100Pq1000.label()), &mut run);
        }
        assert_eq!(state.project_default_odt, OdtChoice::Rec2100Pq1000);
        assert!(state.dirty);
        assert!(summary(&state).contains("Rec.2100 PQ (1000 nit) out"), "{}", summary(&state));
    }

    /// #466: the output LUT is shown, switched and cleared here, through
    /// the commands the inspector's LUT section uses.
    #[test]
    fn the_output_lut_is_switched_and_cleared_here() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        {
            let mut run = page(&mut state);
            let texts = frame(&ctx, Vec::new(), &mut run).1;
            at(&texts, "None");
            at(&texts, "Choose\u{2026}");
        }
        state.global_lut_path = Some(PathBuf::from("/looks/Show.cube"));
        state.global_lut_enabled = true;
        let mut run = page(&mut state);
        let texts = frame(&ctx, Vec::new(), &mut run).1;
        let name = at(&texts, "Show.cube");
        let cmds = click(&ctx, at(&texts, "Clear"), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::ClearGlobalLut]), "{cmds:?}");
        // The switch sits left of the name.
        let cmds = click(&ctx, egui::pos2(name.x - 40.0, name.y), &mut run);
        assert!(matches!(&cmds[..], [AppCommand::SetGlobalLutEnabled(false)]), "{cmds:?}");
    }

    /// #466: the summary Project Info shows says the set-up in a line.
    #[test]
    fn the_summary_says_the_set_up_in_a_line() {
        let mut state = AppState::default();
        assert_eq!(summary(&state), "Rec.709 linear \u{00B7} Match Viewer out");
        state.project_working_colorspace = WorkingColorspace::AcesCG;
        state.project_grade_encoding = GradeEncoding::AcesCct;
        state.project_default_odt = OdtChoice::Rec709Sdr;
        state.global_lut_path = Some(PathBuf::from("/l.cube"));
        state.global_lut_enabled = true;
        assert_eq!(summary(&state), "ACEScg \u{00B7} ACEScct \u{00B7} Rec.709 SDR out \u{00B7} output LUT");
    }
}
