//! The 3D qualifier's strokes (#895): its page arms a keep or remove
//! stroke and clears them, and the viewer, armed, turns a drag across the
//! picture into the stroke's points.

use super::parity_controls::{placed, with_node, Grade};
use super::*;

fn three_d(samples: Vec<[f32; 3]>) -> Grade {
    let (state, clip, node) = placed(klipp_nodes::NodeKind::Qualifier).expect("a Qualifier");
    with_node(&Grade::new(state, clip, node), |n| {
        n.params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, mode: klipp_core::QualifierMode::ThreeD, keep_samples: samples, ..Default::default() });
        Ok(())
    })
    .unwrap()
}

/// In 3D mode the page has Keep and Remove, which arm the viewer for a
/// stroke of that side, and Clear, which takes every stroke away in one
/// undo step; it says how many samples each side holds.
#[test]
fn the_3d_qualifier_page_arms_strokes_and_clears_them() {
    let mut g = three_d(vec![[0.5, 0.4, 0.3], [0.6, 0.5, 0.4]]);
    let drawn = g.drawn();
    assert!(drawn.iter().any(|(t, _)| t == "2 kept, 0 out"), "{drawn:?}");
    for (label, remove) in [("Keep", false), ("Remove", true)] {
        let at = g.drawn().into_iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("no {label}")).1;
        g.click_at(at);
        assert!(g.side.last().is_some_and(|c| c.contains("PickQualifierStroke") && c.contains(&format!("remove: {remove}"))), "{label}: {:?}", g.side);
    }
    let before = g.picture();
    let at = g.drawn().into_iter().find(|(t, _)| t == "Clear").unwrap().1;
    g.click_at(at);
    let q = g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.qualifier.clone()).unwrap();
    assert!(q.keep_samples.is_empty() && q.remove_samples.is_empty() && q.mode == klipp_core::QualifierMode::ThreeD);
    assert_eq!(g.state.active_history().undo_count(), 1);
    g.step(false).unwrap();
    assert_eq!(g.picture(), before);
}

/// Armed for a stroke, a drag across the viewer's picture gives the
/// stroke's points in frame fractions, left to right as dragged, and
/// disarms the viewer; Escape disarms it with nothing sent.
#[test]
fn the_viewer_turns_a_drag_into_a_stroke() {
    let mut state = AppState { playback: klipp_engine::PlaybackState::Paused, ..Default::default() };
    let rt = |f| klipp_core::RationalTime { value: f, rate: 24 };
    let mut item = klipp_media::MediaItem::synthetic_for_test(std::env::temp_dir().join("klipp-stroke.mov"), None);
    item.duration = rt(48);
    item.resolution = klipp_core::Resolution { width: 64, height: 36 };
    let media = state.media_pool.add(item);
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    state.sequences[0].tracks[v1].clips = vec![klipp_timeline::Clip::media(media, rt(0), rt(48), rt(0), rt(48))];
    state.eyedropper_mode = crate::state::EyedropperMode::QualifierStroke;
    state.qualifier_stroke_pick = Some((klipp_core::ClipId::new(), uuid::Uuid::new_v4(), false));
    let ctx = egui::Context::default();
    let mut panel = ViewerPanel::default();
    let theme = KlippTheme::default();
    let mut clock = 0.0;
    let mut frame = |state: &mut AppState, events: Vec<egui::Event>| {
        clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(clock), events, ..Default::default() };
        let mut cmds = Vec::new();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| cmds = panel.show(ui, state, &theme, &crate::keybindings::KeyBindings::default()));
        });
        cmds
    };
    frame(&mut state, Vec::new());
    let image = state.viewer_image_rect.expect("the viewer lays out its picture");
    let at = |fx: f32| egui::pos2(image.min.x + fx * image.width(), image.center().y);
    let button = |p: egui::Pos2, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut sent = Vec::new();
    sent.extend(frame(&mut state, vec![egui::Event::PointerMoved(at(0.2)), button(at(0.2), true)]));
    for k in 1..=6 {
        sent.extend(frame(&mut state, vec![egui::Event::PointerMoved(at(0.2 + 0.1 * k as f32))]));
    }
    sent.extend(frame(&mut state, vec![button(at(0.8), false)]));
    let points = sent.iter().find_map(|c| match c {
        AppCommand::QualifierStroke { points } => Some(points.clone()),
        _ => None,
    });
    let points = points.unwrap_or_else(|| panic!("no stroke sent: {sent:?}"));
    assert!(points.len() >= 4, "{points:?}");
    assert!(points.windows(2).all(|w| w[1][0] >= w[0][0]), "left to right: {points:?}");
    assert!(points.iter().all(|p| (p[1] - 0.5).abs() < 0.02), "along the middle: {points:?}");
    assert!(points.last().is_some_and(|p| p[0] > 0.7), "{points:?}");
    assert_eq!(state.eyedropper_mode, crate::state::EyedropperMode::None, "the stroke disarms the viewer");

    state.eyedropper_mode = crate::state::EyedropperMode::QualifierStroke;
    let sent = frame(&mut state, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert!(sent.is_empty() && state.eyedropper_mode == crate::state::EyedropperMode::None && state.qualifier_stroke_pick.is_none());
}
