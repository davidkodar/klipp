//! The Text tool (#1713): a title made where the viewer was clicked, or
//! the size of the box drawn there, its text edited in place at once.

use super::*;

/// The title the Text tool makes: the default title, its text layer
/// centred at `x`, `y` (fractions of the frame) and, for a drawn box,
/// that size. A point title keeps the default layout box, the frame's
/// size, so its text is centred on the click and wraps at the frame.
pub(super) fn text_tool_title(x: f32, y: f32, box_size: Option<(f32, f32)>) -> klipp_timeline::TitleData {
    let mut td = klipp_timeline::TitleData::default();
    if let Some(obj) = td.objects.first_mut() {
        obj.transform.center = klipp_timeline::TitlePosition { x, y };
        if let Some((w, h)) = box_size {
            obj.transform.size = klipp_timeline::TitlePosition { x: w, y: h };
        }
    }
    td
}

/// Makes the Text tool's title on a free text track at the playhead,
/// `duration_secs` long, as one undo step, and puts its text into the
/// viewer's in-place editor with all of it selected, so the first key
/// typed replaces the placeholder. Returns the new clip; the engine sync
/// and the overlay raster are the caller's.
pub(super) fn create_title_in_viewer(
    state: &mut AppState,
    x: f32,
    y: f32,
    box_size: Option<(f32, f32)>,
    duration_secs: f32,
) -> Option<ClipId> {
    if !x.is_finite() || !y.is_finite() || box_size.is_some_and(|(w, h)| !(w > 0.0 && h > 0.0)) {
        return None;
    }
    let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
    let duration_frames = ((duration_secs.max(0.0) as f64 * fps_rate as f64).round() as u32).max(1);
    let timeline_in = state.playhead;
    let track_index = super::titles::free_text_track(state, timeline_in, duration_frames);
    state.pending_title_data = Some(text_tool_title(x, y, box_size));
    let (clip_id, _) = super::titles::add_title_clip(state, track_index, timeline_in, duration_frames);
    let (object_id, text) = state
        .sequence()
        .all_clips()
        .find(|c| c.id == clip_id)
        .and_then(|c| c.title_data())
        .and_then(|td| {
            td.objects.iter().find_map(|o| match &o.content {
                klipp_timeline::TitleObjectContent::TextBox(tb) => Some((o.id, tb.text.clone())),
                _ => None,
            })
        })?;
    // The keys are the viewer's: the inspector's text field does not
    // take them as it does for a title added from the button.
    state.focus_title_text = 0;
    state.selected_title_object = Some((clip_id, object_id));
    state.viewer_text_edit = true;
    state.viewer_text_edit_clip = Some(clip_id);
    state.viewer_text_edit_object = Some(object_id);
    state.viewer_text_edit_request_focus = true;
    state.viewer_text_edit_select_all = true;
    state.viewer_char_positions.clear();
    state.viewer_text_cursor = text.chars().count();
    state.viewer_text_select_anchor = None;
    state.viewer_text_selection = None;
    state.viewer_text_selection_locked = None;
    state.viewer_text_edit_buffer = text;
    Some(clip_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real program viewer in an egui frame, its commands applied to
    /// the state as the app applies them: the Text tool's through
    /// [`create_title_in_viewer`], the in-place editor's text and its end
    /// as the title handlers do.
    struct Viewer {
        ctx: egui::Context,
        panel: ViewerPanel,
        state: AppState,
        theme: KlippTheme,
        clock: f64,
        sent: Vec<AppCommand>,
    }

    impl Viewer {
        fn new() -> Self {
            // Paused on an empty timeline: the viewer shows a frame to
            // draw on.
            let state = AppState { playback: klipp_engine::PlaybackState::Paused, active_tool: Tool::Text, ..Default::default() };
            let mut v = Self { ctx: egui::Context::default(), panel: ViewerPanel::default(), state, theme: KlippTheme::default(), clock: 0.0, sent: Vec::new() };
            v.frame(Vec::new());
            v
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                time: Some(self.clock),
                events,
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

        fn apply(&mut self, cmd: &AppCommand) {
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
                _ => {}
            }
        }

        fn image_rect(&self) -> egui::Rect {
            self.state.viewer_image_rect.expect("the viewer laid the picture out")
        }

        fn press(&mut self, at: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        }

        fn release(&mut self, at: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(at), button(at, false)]);
            self.frame(Vec::new());
            self.frame(Vec::new());
        }

        fn click(&mut self, at: egui::Pos2) {
            self.press(at);
            self.release(at);
        }

        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            self.press(from);
            for i in 1..=4 {
                self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
            }
            self.release(to);
        }

        fn titles(&self) -> Vec<&klipp_timeline::Clip> {
            self.state.sequences[0].all_clips().filter(|c| c.title_data().is_some()).collect()
        }

        fn layer(&self, clip: &klipp_timeline::Clip) -> klipp_timeline::TitleObject {
            clip.title_data().unwrap().objects[0].clone()
        }
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn text(td: &klipp_timeline::TitleData) -> String {
        match &td.objects[0].content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.text.clone(),
            _ => panic!("a text layer"),
        }
    }

    /// A click with the Text tool makes a point title at the playhead,
    /// its text centred where the click was; it is being edited at once
    /// and the tool stays armed.
    #[test]
    fn a_click_makes_a_title_where_it_was_clicked() {
        let mut v = Viewer::new();
        v.state.playhead = klipp_core::RationalTime::from_frames(48, 24);
        let r = v.image_rect();
        let at = r.min + egui::vec2(r.width() * 0.25, r.height() * 0.75);
        v.click(at);
        let titles = v.titles();
        assert_eq!(titles.len(), 1, "one title: {:?}", v.sent);
        let clip = titles[0];
        assert_eq!(clip.timeline_in.to_secs_f64(), 2.0, "at the playhead");
        assert!(((clip.timeline_out - clip.timeline_in).to_secs_f64() - 5.0).abs() < 1e-6, "the default title's length");
        let track = v.state.sequences[0].tracks.iter().find(|t| t.find_clip(clip.id).is_some()).unwrap();
        assert_eq!(track.kind, klipp_timeline::TrackKind::Text, "on a text track");
        let layer = v.layer(clip);
        assert!((layer.transform.center.x - 0.25).abs() < 1e-3 && (layer.transform.center.y - 0.75).abs() < 1e-3, "{:?}", layer.transform.center);
        assert_eq!(layer.transform.size, klipp_timeline::default_text_transform().size, "a point title keeps the default box");
        assert!(v.state.viewer_text_edit && v.state.viewer_text_edit_clip == Some(clip.id), "edited in place at once");
        assert_eq!(v.state.selected_clip, Some(clip.id));
        assert_eq!(v.state.active_tool, Tool::Text, "the tool stays armed");
    }

    /// A drag draws the text box: the title's layer is the drawn box, in
    /// the frame's fractions, with the viewer zoomed and panned, so it
    /// lands where it was drawn. Typing replaces the placeholder; a
    /// click outside ends the editing and the next click makes another
    /// title.
    #[test]
    fn a_drag_draws_the_box_and_typing_goes_into_it() {
        let mut v = Viewer::new();
        v.state.program_view = crate::state::ViewerView { zoom: 0.5, pan: egui::vec2(-60.0, 25.0) };
        v.frame(Vec::new());
        let r = v.image_rect();
        let (from, to) = (r.min + egui::vec2(r.width() * 0.2, r.height() * 0.3), r.min + egui::vec2(r.width() * 0.6, r.height() * 0.5));
        v.drag(from, to);
        let titles = v.titles();
        assert_eq!(titles.len(), 1, "one title: {:?}", v.sent);
        let clip_id = titles[0].id;
        let t = v.layer(titles[0]).transform;
        let drawn = egui::Rect::from_two_pos(from, to);
        let placed = egui::Rect::from_center_size(
            r.min + egui::vec2(t.center.x * r.width(), t.center.y * r.height()),
            egui::vec2(t.size.x * r.width(), t.size.y * r.height()),
        );
        assert!((placed.min - drawn.min).length() < 0.5 && (placed.max - drawn.max).length() < 0.5, "drawn {drawn:?}, placed {placed:?}");
        assert!((t.size.x - 0.4).abs() < 1e-3 && (t.size.y - 0.2).abs() < 1e-3, "{:?}", t.size);

        // The editor takes the keys with the placeholder selected.
        v.frame(Vec::new());
        v.frame(vec![egui::Event::Text("Hello there".into())]);
        v.frame(Vec::new());
        assert_eq!(text(v.titles()[0].title_data().unwrap()), "Hello there", "{:?}", v.sent);

        v.click(r.max - egui::vec2(10.0, 10.0));
        assert!(!v.state.viewer_text_edit, "a click outside ends the editing");
        assert_eq!(v.titles().len(), 1, "and makes nothing");
        assert_eq!(v.state.active_tool, Tool::Text);
        v.click(r.max - egui::vec2(10.0, 10.0));
        assert_eq!(v.titles().len(), 2, "the next click makes another title");
        assert!(v.titles().iter().any(|c| c.id == clip_id));
    }

    /// The title is one undo step: undone, the clip and the text track
    /// made for it are gone.
    #[test]
    fn undo_takes_the_title_away_in_one_step() {
        let mut v = Viewer::new();
        let tracks = v.state.sequences[0].tracks.len();
        let r = v.image_rect();
        v.click(r.center());
        assert_eq!(v.titles().len(), 1);
        assert_eq!(v.state.active_history().undo_count(), 1, "one step");
        let entry = v.state.active_history_mut().undo().expect("a step");
        let mut doc = klipp_document::apply::DocumentView {
            sequences: &mut v.state.sequences,
            active: v.state.active_sequence_idx,
            media_pool: &mut v.state.media_pool,
            fps: &mut v.state.fps,
        };
        doc.apply_inverse(&entry.command);
        assert!(v.titles().is_empty(), "the title is gone");
        assert_eq!(v.state.sequences[0].tracks.len(), tracks, "and the track made for it");
    }

    /// With the tool down the viewer makes no title.
    #[test]
    fn select_makes_no_title() {
        let mut v = Viewer::new();
        v.state.active_tool = Tool::Select;
        let r = v.image_rect();
        v.click(r.center());
        v.drag(r.center(), r.center() + egui::vec2(100.0, 60.0));
        assert!(v.titles().is_empty());
    }

    /// Y arms the Text tool, and the timeline's toolbar names it with
    /// its key.
    #[test]
    fn the_shortcut_is_y() {
        let kb = crate::keybindings::KeyBindings::defaults();
        assert_eq!(kb.display_for("text_tool"), "Y");
        let ctx = egui::Context::default();
        let mut pressed = false;
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Y, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| pressed = ctx.input(|i| kb.is_action_pressed("text_tool", i) && !kb.is_action_pressed("trim_tool", i)));
        assert!(pressed);
        assert!(include_str!("shortcuts.rs").contains("cmds.push(AppCommand::SetActiveTool(Tool::Text))"));
    }
}
