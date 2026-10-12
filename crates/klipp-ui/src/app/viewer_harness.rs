//! A test driver for the program viewer (#1713, #1714): real pointer and
//! key events through `ViewerPanel::show`, its commands applied to the
//! state.

use super::*;
use super::text_tool::create_title_in_viewer;
use super::title_handles;

/// The real program viewer in an egui frame, its commands applied to
/// the state as the app applies them: the Text tool's through
/// [`create_title_in_viewer`], the title handles' through
/// [`title_handles::apply`], and the selection and the in-place editor's
/// text, start and end as their handlers do.
pub(super) struct Viewer {
    pub(super) ctx: egui::Context,
    pub(super) panel: ViewerPanel,
    pub(super) state: AppState,
    pub(super) theme: KlippTheme,
    pub(super) clock: f64,
    pub(super) sent: Vec<AppCommand>,
    /// The keys held through the frames that follow.
    pub(super) modifiers: egui::Modifiers,
}

impl Viewer {
    pub(super) fn new() -> Self {
        // Paused on an empty timeline: the viewer shows a frame to
        // draw on.
        let state = AppState { playback: klipp_engine::PlaybackState::Paused, active_tool: Tool::Text, ..Default::default() };
        let mut v = Self { ctx: egui::Context::default(), panel: ViewerPanel::default(), state, theme: KlippTheme::default(), clock: 0.0, sent: Vec::new(), modifiers: egui::Modifiers::NONE };
        v.frame(Vec::new());
        v
    }

    pub(super) fn frame(&mut self, events: Vec<egui::Event>) {
        self.clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
            time: Some(self.clock),
            events,
            modifiers: self.modifiers,
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let (panel, state, theme) = (&mut self.panel, &mut self.state, &self.theme);
        let _ = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| cmds = panel.show(ui, state, theme));
        });
        for cmd in cmds {
            self.apply(&cmd);
            self.sent.push(cmd);
        }
    }

    pub(super) fn apply(&mut self, cmd: &AppCommand) {
        let state = &mut self.state;
        match cmd {
            AppCommand::CreateTitleInViewer { x, y, box_size } => {
                create_title_in_viewer(state, *x, *y, *box_size, 5.0).expect("a title");
            }
            AppCommand::SetTitleObjectText { clip_id, object_id, text } => {
                let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(*clip_id)).unwrap();
                let klipp_timeline::ClipKind::Title(td) = &mut clip.kind else { panic!("a title") };
                let Some(klipp_timeline::TitleObject { content: klipp_timeline::TitleObjectContent::TextBox(tb), .. }) =
                    td.object_mut(klipp_timeline::TitleObjectId(*object_id))
                else {
                    panic!("its text layer")
                };
                tb.text = text.clone();
            }
            AppCommand::EndTitleEdit => {
                state.viewer_text_edit = false;
                state.viewer_text_edit_clip = None;
                state.viewer_text_edit_object = None;
            }
            AppCommand::BeginTitleEdit { clip_id } => {
                state.viewer_text_edit_clip = Some(*clip_id);
                state.viewer_text_edit = true;
            }
            AppCommand::SelectClip(id) => state.selected_clip = *id,
            AppCommand::SelectTitleObject { clip_id, object_id } => {
                state.selected_title_object = object_id.map(|id| (*clip_id, klipp_timeline::TitleObjectId(id)));
            }
            _ => {
                title_handles::apply(state, cmd);
            }
        }
    }

    pub(super) fn image_rect(&self) -> egui::Rect {
        self.state.viewer_image_rect.expect("the viewer laid the picture out")
    }

    pub(super) fn press(&mut self, at: egui::Pos2) {
        self.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
    }

    pub(super) fn release(&mut self, at: egui::Pos2) {
        self.frame(vec![egui::Event::PointerMoved(at), button(at, false)]);
        self.frame(Vec::new());
        self.frame(Vec::new());
    }

    pub(super) fn click(&mut self, at: egui::Pos2) {
        self.press(at);
        self.release(at);
    }

    pub(super) fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        self.press(from);
        for i in 1..=4 {
            self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
        }
        self.release(to);
    }

    pub(super) fn titles(&self) -> Vec<&klipp_timeline::Clip> {
        self.state.sequences[0].all_clips().filter(|c| c.title_data().is_some()).collect()
    }

    pub(super) fn layer(&self, clip: &klipp_timeline::Clip) -> klipp_timeline::TitleObject {
        clip.title_data().unwrap().objects[0].clone()
    }
}

pub(super) fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}
