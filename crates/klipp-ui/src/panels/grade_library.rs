//! The grade library's row in the Colour page's inspector (#899): saved
//! grades, across projects, each as its thumbnail and name. Click one to
//! pick it; Apply puts its grade on the selected clips, Add puts it after
//! theirs; Save keeps the graded clip's grade under a name; the picked
//! one can be deleted. Labels note the camera and profile a grade was
//! made for, and the filter finds grades by name or label (#614). Purely
//! UI: it emits the library's commands.

use crate::grade_library::LibraryGrade;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};

#[derive(Default)]
pub struct GradeLibraryPanelState {
    /// The picked grade, by name.
    pub picked: Option<String>,
    /// The name being typed for a save, while its field is open.
    pub saving: Option<String>,
    /// Only the grades whose name or labels hold this (#614).
    pub filter: String,
    /// The picked grade's labels being edited, while their fields are open.
    pub labelling: Option<crate::grade_library::GradeLabels>,
}

/// The library's row. `graded` is the clip whose grade a save keeps;
/// `targets` the clips an apply grades.
pub fn show(ui: &mut egui::Ui, state: &mut GradeLibraryPanelState, library: &mut [LibraryGrade], graded: Option<ClipId>, targets: &[ClipId], theme: &KlippTheme) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    if state.picked.as_ref().is_some_and(|p| !library.iter().any(|g| &g.name == p)) {
        state.picked = None;
        state.labelling = None;
    }
    ui.horizontal(|ui| {
        ui.set_min_height(crate::theme::control::COMPACT);
        ui.add_space(12.0);
        ui.add_sized(
            [56.0, crate::theme::control::COMPACT],
            egui::Label::new(egui::RichText::new("Library").size(crate::theme::type_scale::CAPTION).color(theme.text_dim)).selectable(false),
        );
        let saving = state.saving.is_some();
        if ui.add_enabled(graded.is_some(), egui::Button::new(if saving { "Cancel" } else { "Save" }).small()).on_hover_text("Keep this clip's whole grade in the library, for any project").clicked() {
            state.saving = if saving { None } else { Some(String::new()) };
        }
        let picked = state.picked.clone();
        if ui.add_enabled(picked.is_some() && !targets.is_empty(), egui::Button::new("Apply").small()).on_hover_text("Put the picked grade on the selected clips, in place of theirs").clicked() {
            cmds.push(AppCommand::ApplyGradeFromLibrary { name: picked.clone().unwrap_or_default(), clip_ids: targets.to_vec(), append: false });
        }
        if ui.add_enabled(picked.is_some() && !targets.is_empty(), egui::Button::new("Add").small()).on_hover_text("Put the picked grade after the selected clips' own").clicked() {
            cmds.push(AppCommand::ApplyGradeFromLibrary { name: picked.clone().unwrap_or_default(), clip_ids: targets.to_vec(), append: true });
        }
        if ui.add_enabled(picked.is_some(), egui::Button::new("Delete").small()).on_hover_text("Take the picked grade out of the library").clicked() {
            cmds.push(AppCommand::DeleteGradeFromLibrary { name: picked.unwrap_or_default() });
        }
    });
    if let (Some(name), Some(clip_id)) = (state.saving.as_mut(), graded) {
        let mut done = false;
        ui.horizontal(|ui| {
            ui.add_space(72.0);
            let field = ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(140.0));
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Keep").small()).clicked() || entered) && !name.trim().is_empty() {
                cmds.push(AppCommand::SaveGradeToLibrary { clip_id, name: name.trim().to_string() });
                done = true;
            }
        });
        if done {
            state.saving = None;
        }
    }
    if library.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(72.0);
            ui.label(egui::RichText::new("No saved grades yet: Save keeps this clip's grade here, for every project.").size(10.0).color(theme.text_dim));
        });
        return cmds;
    }
    // The filter, and the picked grade's labels (#614).
    ui.horizontal(|ui| {
        ui.add_space(72.0);
        ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Find by name, camera, profile").desired_width(160.0));
        let picked = state.picked.as_ref().and_then(|p| library.iter().find(|g| &g.name == p));
        if let Some(grade) = picked {
            if ui.add_enabled(state.labelling.is_none(), egui::Button::new("Labels").small()).on_hover_text("Note the camera and profile this grade was made for, to find it by. It grades nothing by itself.").clicked() {
                state.labelling = Some(grade.labels.clone());
            }
            if !grade.labels.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(grade.labels.line()).size(10.0).color(theme.text_dim)).truncate());
            }
        }
    });
    if let (Some(labels), Some(name)) = (state.labelling.as_mut(), state.picked.clone()) {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.add_space(72.0);
            ui.add(egui::TextEdit::singleline(&mut labels.camera).hint_text("Camera").desired_width(70.0));
            ui.add(egui::TextEdit::singleline(&mut labels.profile).hint_text("Profile").desired_width(70.0));
            ui.add(egui::TextEdit::singleline(&mut labels.notes).hint_text("Notes").desired_width(110.0));
            if ui.small_button("Keep").clicked() {
                cmds.push(AppCommand::SetGradeLibraryLabels { name, camera: labels.camera.clone(), profile: labels.profile.clone(), notes: labels.notes.clone() });
                close = true;
            }
            if ui.small_button("Cancel").clicked() {
                close = true;
            }
        });
        if close {
            state.labelling = None;
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.add_space(72.0);
        for grade in library.iter_mut().filter(|g| g.matches(&state.filter)) {
            if grade.texture.is_none() {
                if let Some(t) = &grade.thumbnail {
                    let image = egui::ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &t.rgba);
                    grade.texture = Some(ui.ctx().load_texture(format!("grade_library_{}", grade.name), image, egui::TextureOptions::LINEAR));
                }
            }
            let picked = state.picked.as_deref() == Some(grade.name.as_str());
            let size = egui::vec2(64.0, 36.0);
            let resp = ui.vertical(|ui| {
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                match &grade.texture {
                    Some(tex) => {
                        ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                    None => {
                        ui.painter().rect_filled(rect, 2.0, theme.bg_raised);
                    }
                }
                if picked {
                    ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(2.0_f32, theme.accent));
                }
                // The name picks it as the thumbnail does.
                let name = ui.add_sized([size.x, 12.0], egui::Label::new(egui::RichText::new(&grade.name).size(9.0).color(if picked { theme.text_primary } else { theme.text_dim })).truncate().sense(egui::Sense::click()).selectable(false));
                resp | name
            }).inner;
            let hover = if grade.labels.is_empty() { grade.name.clone() } else { format!("{}\n{}", grade.name, grade.labels.line()) };
            if resp.on_hover_text(hover).clicked() {
                state.picked = if picked { None } else { Some(grade.name.clone()) };
                state.labelling = None;
            }
        }
    });
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row {
        ctx: egui::Context,
        state: GradeLibraryPanelState,
        library: Vec<LibraryGrade>,
        clock: f64,
        sent: Vec<AppCommand>,
    }

    impl Row {
        fn frame(&mut self, events: Vec<egui::Event>, graded: Option<ClipId>, targets: &[ClipId]) -> Vec<(String, egui::Pos2)> {
            self.clock += 0.05;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 300.0))), time: Some(self.clock), events, ..Default::default() };
            let theme = KlippTheme::default();
            let Row { ctx, state, library, .. } = self;
            let mut cmds = Vec::new();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| cmds = show(ui, state, library, graded, targets, &theme));
            });
            self.sent.extend(cmds);
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn click(&mut self, text: &str, graded: Option<ClipId>, targets: &[ClipId]) {
            let at = self.frame(Vec::new(), graded, targets).into_iter().find(|(t, _)| t == text).unwrap_or_else(|| panic!("no {text:?}")).1;
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], graded, targets);
            }
        }
    }

    /// Driven with pointer and key events: Save opens a name field and
    /// Enter keeps the graded clip's grade under that name; a grade
    /// clicked is picked, Apply and Add put it on the selected clips,
    /// Delete takes it out.
    #[test]
    fn the_library_row_saves_picks_applies_and_deletes() {
        let library = ["Teal shadows", "Warm dusk"].map(|n| LibraryGrade { name: n.into(), graph: Default::default(), labels: Default::default(), node_labels: Vec::new(), thumbnail: Some(klipp_render::still::Still { rgba: vec![90; 64 * 36 * 4], width: 64, height: 36 }), texture: None }).into_iter().collect();
        let mut row = Row { ctx: egui::Context::default(), state: Default::default(), library, clock: 0.0, sent: Vec::new() };
        let (graded, targets) = (ClipId::new(), [ClipId::new(), ClipId::new()]);

        row.click("Save", Some(graded), &targets);
        let field = row.frame(Vec::new(), Some(graded), &targets).into_iter().find(|(t, _)| t == "Name").expect("the name field").1;
        for pressed in [true, false] {
            row.frame(vec![egui::Event::PointerButton { pos: field, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], Some(graded), &targets);
        }
        row.frame(vec![egui::Event::Text("Night blue".into())], Some(graded), &targets);
        row.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], Some(graded), &targets);
        assert!(row.sent.iter().any(|c| matches!(c, AppCommand::SaveGradeToLibrary { clip_id, name } if *clip_id == graded && name == "Night blue")), "{:?}", row.sent);
        assert!(row.state.saving.is_none(), "the field closes once saved");

        row.sent.clear();
        row.click("Apply", Some(graded), &targets);
        assert!(row.sent.is_empty(), "nothing picked, nothing applied");
        row.click("Warm dusk", Some(graded), &targets);
        assert_eq!(row.state.picked.as_deref(), Some("Warm dusk"));
        row.click("Apply", Some(graded), &targets);
        row.click("Add", Some(graded), &targets);
        row.click("Delete", Some(graded), &targets);
        let want = |append| move |c: &AppCommand| matches!(c, AppCommand::ApplyGradeFromLibrary { name, clip_ids, append: a } if name == "Warm dusk" && *clip_ids == targets.to_vec() && *a == append);
        assert!(row.sent.iter().any(want(false)) && row.sent.iter().any(want(true)), "{:?}", row.sent);
        assert!(row.sent.iter().any(|c| matches!(c, AppCommand::DeleteGradeFromLibrary { name } if name == "Warm dusk")), "{:?}", row.sent);
    }

    /// #614, with pointer and key events: the picked grade's Labels opens
    /// its camera, profile and notes, and Keep sends them; the filter
    /// leaves only the grades whose name or labels hold what is typed.
    #[test]
    fn the_library_row_labels_and_finds_grades() {
        let mut library: Vec<LibraryGrade> = ["Night blue", "Warm dusk"].map(|n| LibraryGrade { name: n.into(), graph: Default::default(), labels: Default::default(), node_labels: Vec::new(), thumbnail: None, texture: None }).into_iter().collect();
        library[0].labels.camera = "FX3".into();
        let mut row = Row { ctx: egui::Context::default(), state: Default::default(), library, clock: 0.0, sent: Vec::new() };
        let (graded, targets) = (ClipId::new(), [ClipId::new()]);
        assert!(row.frame(Vec::new(), Some(graded), &targets).iter().all(|(t, _)| t != "Labels"), "nothing picked, nothing to label");
        row.click("Warm dusk", Some(graded), &targets);
        row.click("Labels", Some(graded), &targets);
        let field = row.frame(Vec::new(), Some(graded), &targets).into_iter().find(|(t, _)| t == "Profile").expect("the profile field").1;
        for pressed in [true, false] {
            row.frame(vec![egui::Event::PointerButton { pos: field, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], Some(graded), &targets);
        }
        row.frame(vec![egui::Event::Text("HLG".into())], Some(graded), &targets);
        row.click("Keep", Some(graded), &targets);
        assert!(row.sent.iter().any(|c| matches!(c, AppCommand::SetGradeLibraryLabels { name, camera, profile, notes } if name == "Warm dusk" && camera.is_empty() && profile == "HLG" && notes.is_empty())), "{:?}", row.sent);
        assert!(row.state.labelling.is_none());

        row.state.filter = "fx3".into();
        let shown = row.frame(Vec::new(), Some(graded), &targets);
        assert!(shown.iter().any(|(t, _)| t == "Night blue") && !shown.iter().any(|(t, _)| t == "Warm dusk"), "found by its camera label");
    }
}
