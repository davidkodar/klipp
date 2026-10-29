//! A small window (#2944): at 1366×768 and 1280×720, Auto makes the
//! interface smaller and says so once; the timeline's toolbar draws
//! nothing past its edge, folding what does not fit into "»"; the timeline
//! keeps V1, A1 and A2 in view; and each side panel scrolls to its end.

use crate::state::AppState;
use crate::theme::KlippTheme;

/// The two windows, in points at the system scale.
const WINDOWS: [(f32, f32); 2] = [(1366.0, 768.0), (1280.0, 720.0)];

/// The editor's rects for a window of `w` by `h` at the size Auto picks.
fn editor(w: f32, h: f32, state: &AppState) -> crate::layout::LayoutRects {
    editor_at(w, h, super::auto_zoom_factor(egui::vec2(w, h)), &crate::layout_state::LayoutState::default(), state)
}

/// The editor's rects for a window of `w` by `h` at interface size `zoom`.
fn editor_at(w: f32, h: f32, zoom: f32, layout: &crate::layout_state::LayoutState, state: &AppState) -> crate::layout::LayoutRects {
    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w / zoom, h / zoom));
    crate::layout::compute_layout(full, layout, state)
}

/// The texts drawn where they show, with where.
fn texts(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    texts_where(out, true)
}

/// The texts drawn, with where: only those their clip shows, or all of
/// them, clipped away or not.
fn texts_where(out: &egui::FullOutput, shown_only: bool) -> Vec<(String, egui::Rect)> {
    fn walk(shape: &egui::Shape, clip: egui::Rect, shown_only: bool, out: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(t) => {
                let r = t.galley.rect.translate(t.pos.to_vec2());
                if !shown_only || r.intersects(clip) {
                    out.push((t.galley.text().to_string(), r));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, shown_only, out)),
            _ => {}
        }
    }
    let mut v = Vec::new();
    for s in &out.shapes {
        walk(&s.shape, s.clip_rect, shown_only, &mut v);
    }
    v
}

fn input(screen: egui::Rect, events: Vec<egui::Event>, time: f64) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(screen), time: Some(time), events, ..Default::default() }
}

/// Auto makes the interface smaller at both windows; the note goes up the
/// first time, with a project open, and never again; its Change offers the
/// sizes, and one picked is asked for.
#[test]
fn auto_makes_it_smaller_and_says_so_once() {
    let mut prefs = crate::prefs::Preferences::default().appearance;
    assert_eq!(prefs.ui_scale_percent, 0, "Auto by default");
    let mut state = AppState::default();
    for (w, h) in WINDOWS {
        assert!(super::auto_zoom_factor(egui::vec2(w, h)) < 1.0, "{w}x{h}");
    }
    let zoom = super::auto_zoom_factor(egui::vec2(1366.0, 768.0));
    assert!(!crate::welcome::small_screen_note_now(&mut state, &mut prefs, zoom), "not on the launch screen");
    state.project_path = Some(crate::project_paths::UNTITLED.into());
    assert!(crate::welcome::small_screen_note_now(&mut state, &mut prefs, zoom) && state.small_screen_note == Some(80), "the first time in the editor");
    state.small_screen_note = None;
    assert!(!crate::welcome::small_screen_note_now(&mut state, &mut prefs, zoom) && state.small_screen_note.is_none(), "once, ever");
    let mut fixed = crate::prefs::Preferences::default().appearance;
    fixed.ui_scale_percent = 100;
    assert!(!crate::welcome::small_screen_note_now(&mut state, &mut fixed, 1.0), "a size the user chose says nothing");

    // Drawn: what it says, Change, and a size picked there.
    state.small_screen_note = Some(80);
    let ctx = egui::Context::default();
    let theme = KlippTheme::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1366.0, 768.0));
    let mut time = 0.0;
    let mut frame = |state: &mut AppState, events| {
        time += 0.1;
        let out = ctx.run(input(screen, events, time), |ctx| crate::welcome::show_small_screen_note(ctx, state, &theme));
        texts(&out)
    };
    frame(&mut state, Vec::new());
    let shown = frame(&mut state, Vec::new());
    assert!(shown.iter().any(|(t, _)| t == "Small screen \u{2014} interface set to 80%"), "{shown:?}");
    let click = |at: egui::Pos2| vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }];
    let change = shown.iter().find(|(t, _)| t == "Change").expect("Change").1.center();
    frame(&mut state, click(change));
    let sizes = frame(&mut state, Vec::new());
    let ninety = sizes.iter().find(|(t, _)| t == "90%").expect("the sizes offered").1.center();
    frame(&mut state, click(ninety));
    assert_eq!(state.ui_scale_request, Some(90));
    assert_eq!(state.small_screen_note, None, "and the note goes");
}

/// The timeline's toolbar at each window's timeline width: nothing drawn
/// past its right edge, no control over another, the zoom group whole, and
/// what does not fit under "»".
#[test]
fn the_timeline_toolbar_clips_nothing() {
    for (w, h) in WINDOWS.into_iter().chain([(1024.0, 600.0)]) {
        let mut state = AppState::default();
        let width = editor(w, h, &state).timeline_rect.width();
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let kb = crate::keybindings::KeyBindings::defaults();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 40.0));
        let mut shown = Vec::new();
        for frame in 0..4 {
            let out = ctx.run(input(screen, Vec::new(), frame as f64 * 0.1), |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    let mut cmds = Vec::new();
                    crate::timeline::toolbar::show(ui, &mut state, &theme, &mut cmds, &kb);
                });
            });
            shown = texts(&out);
        }
        for (t, r) in &shown {
            assert!(r.right() <= width + 0.5 && r.left() >= -0.5, "{w}x{h} ({width:.0} pt): {t:?} at {r:?} is past the edge");
        }
        for (i, (a, ra)) in shown.iter().enumerate() {
            for (b, rb) in &shown[i + 1..] {
                assert!(!ra.shrink(1.0).intersects(rb.shrink(1.0)), "{w}x{h}: {a:?} and {b:?} overlap");
            }
        }
        assert!(shown.iter().any(|(t, _)| t == "Fit"), "{w}x{h}: the zoom group is there");
        let all_in_row = shown.iter().any(|(t, _)| t == "LINKED");
        assert!(all_in_row || shown.iter().any(|(t, _)| t == "\u{00bb}"), "{w}x{h}: what does not fit is under »");
    }
}

/// The timeline at each window keeps V1, A1 and A2 in view under its
/// tabs, toolbar and ruler: at the size Auto picks, and at 100% with the
/// timeline dragged down to 100 points, its least.
#[test]
fn the_timeline_shows_v1_a1_and_a2() {
    let dragged = crate::layout_state::LayoutState { timeline_height: 100.0, ..Default::default() };
    for (w, h, zoom, layout) in WINDOWS.into_iter().flat_map(|(w, h)| [(w, h, super::auto_zoom_factor(egui::vec2(w, h)), crate::layout_state::LayoutState::default()), (w, h, 1.0, dragged.clone())]) {
        let mut state = AppState::default();
        state.sequences[0].tracks.push(klipp_timeline::Track::new("Audio 2", klipp_timeline::TrackKind::Audio));
        let names: Vec<String> = state.sequence().tracks.iter().map(|t| t.name.clone()).collect();
        let rect = editor_at(w, h, zoom, &layout, &state).timeline_rect;
        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let kb = crate::keybindings::KeyBindings::defaults();
        let mut panel = crate::timeline::NewTimelinePanel::default();
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, rect.size());
        let mut shown = Vec::new();
        for frame in 0..3 {
            let out = ctx.run(input(screen, Vec::new(), frame as f64 * 0.1), |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    let _ = panel.show(ui, &mut state, &theme, &mut old, &kb, false);
                });
            });
            shown = texts(&out);
        }
        for name in &names {
            let at = shown.iter().find(|(t, _)| t == name).unwrap_or_else(|| panic!("{w}x{h}: {name} is not drawn in a timeline {:.0} pt tall: {:?}", rect.height(), shown.iter().map(|t| &t.0).collect::<Vec<_>>()));
            assert!(at.1.bottom() <= screen.bottom() + 0.5, "{w}x{h}: {name} at {:?} is below the timeline's {:.0} pt", at.1, screen.bottom());
        }
    }
}

/// Scrolled far down, a panel drawn in `rect` shows its last text inside
/// `rect`: nothing it holds is left below its foot.
/// True when the panel had more than its height and was scrolled.
fn ends_in_view(name: &str, rect: egui::Rect, draw: &mut dyn FnMut(&mut egui::Ui)) -> bool {
    let ctx = egui::Context::default();
    crate::icons::install(&ctx);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, rect.size());
    let mut shown = Vec::new();
    for frame in 0..12 {
        let events = if frame >= 2 {
            vec![egui::Event::PointerMoved(screen.center()), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -4000.0), modifiers: Default::default() }]
        } else {
            Vec::new()
        };
        let out = ctx.run(input(screen, events, frame as f64 * 0.5), |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| draw(ui));
        });
        // Every text, clipped or not: one left below the foot is a control
        // the panel cannot bring into view.
        shown = texts_where(&out, false);
    }
    let lowest = shown.iter().max_by(|a, b| a.1.bottom().total_cmp(&b.1.bottom())).expect("something drawn");
    let scrolled = shown.iter().any(|(_, r)| r.top() < screen.top() - 1.0);
    assert!(lowest.1.bottom() <= screen.bottom() + 1.0, "{name}: {:?} at {:?} is below the panel's foot at {:.0}", lowest.0, lowest.1, screen.bottom());
    scrolled
}

/// A folder of three empty clips, the same on every machine.
fn files_fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-small-window-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp folder");
    for name in ["a.mov", "b.mov", "c.wav"] {
        std::fs::write(dir.join(name), b"").expect("a fixture file");
    }
    dir
}

/// Each side panel, at each window's height: the browser's tabs and the
/// Library's categories, and the inspector's tabs for a media clip and a
/// title, scroll to their ends.
#[test]
fn every_side_panel_scrolls_to_its_end() {
    use crate::layout_state::{BrowserTab, InspectorTab, LibraryCategory};
    for (w, h) in WINDOWS {
        let mut state = AppState::default();
        let rects = editor(w, h, &state);
        let theme = KlippTheme::default();
        for (tab, category) in [(BrowserTab::Files, None), (BrowserTab::Media, None), (BrowserTab::Library, Some(LibraryCategory::VideoFx)), (BrowserTab::Library, Some(LibraryCategory::AudioFx)), (BrowserTab::Library, Some(LibraryCategory::Titles)), (BrowserTab::Library, Some(LibraryCategory::Transitions)), (BrowserTab::Library, Some(LibraryCategory::Generators))] {
            let mut panel = crate::browser::BrowserPanel::default();
            if let Some(c) = category {
                panel.library_category = c;
            }
            // The Files tab lists a fixed folder of its own, not this
            // machine's Videos, whose listing came in mid-scroll.
            panel.files_current_path = files_fixture();
            let _ = ends_in_view(&format!("{w}x{h} browser {tab:?} {category:?}"), rects.browser_rect, &mut |ui| {
                let (mut tab, mut pinned, mut ratio) = (tab, false, 0.5);
                let _ = panel.show(ui, &mut state, &theme, &mut tab, &mut pinned, &mut ratio);
            });
        }
        // A media clip and a title, each inspector tab.
        let prefs = crate::prefs::Preferences::default();
        let mut media = AppState::default();
        let item = media.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/small-window.mov".into(), None));
        let clip = klipp_timeline::Clip::media(item, klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(96, 24), klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(96, 24));
        media.selected_clip = Some(clip.id);
        media.sequences[0].tracks[0].clips.push(clip);
        let mut title = AppState::default();
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Title".into(), font_name: "Inter".into(), ..Default::default() });
        let tclip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(klipp_core::Timecode::zero(), klipp_core::Timecode::from_frames(48, 24)) };
        title.selected_clip = Some(tclip.id);
        title.sequences[0].tracks[0].clips.push(tclip);
        for (what, state, tabs) in [("media", &media, &[InspectorTab::Video, InspectorTab::Audio, InspectorTab::Color, InspectorTab::Effects, InspectorTab::Info][..]), ("title", &title, &[InspectorTab::Title, InspectorTab::Effects][..])] {
            for tab in tabs {
                let mut panel = crate::inspector::NewInspectorPanel::default();
                let scrolled = ends_in_view(&format!("{w}x{h} inspector {what} {tab:?}"), rects.inspector_rect, &mut |ui| {
                    let mut active = *tab;
                    let _ = panel.show(ui, state, &prefs, &theme, &mut active);
                });
                // The Video tab is longer than the window: the case holds
                // something to scroll.
                if what == "media" && *tab == InspectorTab::Video {
                    assert!(scrolled, "{w}x{h}: the media clip's Video tab overflows and scrolls");
                }
            }
        }
    }
}
