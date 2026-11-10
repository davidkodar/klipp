//! The Chroma Key's eyedropper: its card arms the viewer for the key, the
//! viewer, armed, sends the click, and the colour the key receives there
//! becomes its Key Colour, one undo step. (The engine's read of what the
//! key receives is `key_colour_pick_reads_key_input` in klipp-engine.)

use super::colour::{key_colour_of, key_colour_payload};
use super::parity_controls::{fixture, Stack};
use super::*;

fn key() -> BuiltinEffect {
    klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 255, 0], tolerance: 0.2, soft_edge: 0.1, spill_method: klipp_effects::SpillMethod::None, spill_strength: 0.0 }
}

use klipp_effects::BuiltinEffect;

/// The key's node and its path in the clip's picture.
fn key_path(s: &Stack) -> klipp_graph_format::path::NodePath {
    let picture = s.picture();
    let node = picture.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("chroma_key")).expect("the key").id;
    klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: s.clip.0 }, [node])
}

fn key_colour(s: &Stack) -> [u8; 3] {
    let at = key_path(s);
    let picture = s.picture();
    let n = picture.picture.graph.nodes.iter().find(|n| Some(&n.id) == at.nodes.last()).unwrap();
    match serde_json::from_value(klipp_graph_lift::effect_params::entry_of(n).unwrap()).unwrap() {
        BuiltinEffect::ChromaKey { key_color, .. } => key_color,
        e => panic!("not a key: {e:?}"),
    }
}

/// The card's button arms the eyedropper for this key: the clip and the
/// key's own path.
#[test]
fn the_key_card_arms_its_eyedropper() {
    let mut s = Stack::new(fixture(&key()));
    s.click("Pick Key Colour").unwrap();
    let want = format!("{:?}", AppCommand::PickKeyColour { clip_id: s.clip, at: key_path(&s) });
    assert!(s.unapplied.contains(&want), "sent {:?}, wanted {want}", s.unapplied);
    // Armed, the button says what to do.
    s.state.eyedropper_mode = crate::state::EyedropperMode::KeyColour;
    s.state.key_colour_pick = Some((s.clip, key_path(&s)));
    assert!(s.frame(Vec::new()).iter().any(|(t, _)| t == "Click the colour in the viewer"));
}

/// The colour read (linear, as the key receives it) is held as the sRGB
/// bytes the key compares.
#[test]
fn the_pick_is_held_as_the_keys_srgb_bytes() {
    assert_eq!(key_colour_of([0.0, 1.0, 0.0]), [0, 255, 0]);
    assert_eq!(key_colour_of([0.2158605, 0.0, 1.5]), [128, 0, 255], "mid grey's linear value is 128; past white clamps");
    assert_eq!(key_colour_of([-0.1, 0.0, 0.0]), [0, 0, 0]);
}

/// The answer sets the key's colour, and only it, as one undo step; a path
/// that names no key of the clip sets nothing.
#[test]
fn the_pick_sets_the_key_colour_in_one_step() {
    let mut s = Stack::new(fixture(&key()));
    let before = s.picture();
    let at = key_path(&s);
    let payload = key_colour_payload(s.state.sequence(), s.clip, &at, [30, 140, 60]).expect("the key's entry");
    s.apply(&AppCommand::SetPayloadAt { addr: at.clone(), payload });
    assert!(s.unapplied.is_empty(), "{:?}", s.unapplied);
    assert_eq!(key_colour(&s), [30, 140, 60]);
    match serde_json::from_value(klipp_graph_lift::effect_params::entry_of(s.picture().picture.graph.nodes.iter().find(|n| Some(&n.id) == at.nodes.last()).unwrap()).unwrap()).unwrap() {
        BuiltinEffect::ChromaKey { tolerance, soft_edge, .. } => assert_eq!((tolerance, soft_edge), (0.2, 0.1), "the rest of the key is kept"),
        e => panic!("{e:?}"),
    }
    assert_eq!(s.state.active_history().undo_count(), 1);
    s.step(false).unwrap();
    assert_eq!(s.picture(), before);

    let other = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: s.clip.0 }, [uuid::Uuid::new_v4()]);
    assert!(key_colour_payload(s.state.sequence(), s.clip, &other, [1, 2, 3]).is_none());
    let blur = Stack::new(fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 }));
    let blur_node = blur.picture().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n).is_some()).unwrap().id;
    let blur_at = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: blur.clip.0 }, [blur_node]);
    assert!(key_colour_payload(blur.state.sequence(), blur.clip, &blur_at, [1, 2, 3]).is_none(), "not a key");
}

/// Armed for a key, a click on the viewer's picture sends it, in frame
/// fractions, and disarms the viewer; Escape disarms it, the pick called
/// off, nothing sent.
#[test]
fn the_viewer_sends_the_key_colour_click() {
    let mut state = AppState { playback: klipp_engine::PlaybackState::Paused, ..Default::default() };
    let rt = |f| klipp_core::RationalTime { value: f, rate: 24 };
    let mut item = klipp_media::MediaItem::synthetic_for_test(std::env::temp_dir().join("klipp-key-pick.mov"), None);
    item.duration = rt(48);
    item.resolution = klipp_core::Resolution { width: 64, height: 36 };
    let media = state.media_pool.add(item);
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    state.sequences[0].tracks[v1].clips = vec![klipp_timeline::Clip::media(media, rt(0), rt(48), rt(0), rt(48))];
    let pick = (klipp_core::ClipId::new(), klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: uuid::Uuid::new_v4() }, [uuid::Uuid::new_v4()]));
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
    let arm = |state: &mut AppState| {
        state.eyedropper_mode = crate::state::EyedropperMode::KeyColour;
        state.key_colour_pick = Some(pick.clone());
    };
    arm(&mut state);
    frame(&mut state, Vec::new());
    let image = state.viewer_image_rect.expect("the viewer lays out its picture");
    let at = egui::pos2(image.min.x + 0.25 * image.width(), image.min.y + 0.75 * image.height());
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut sent = frame(&mut state, vec![egui::Event::PointerMoved(at), button(true)]);
    sent.extend(frame(&mut state, vec![button(false)]));
    let click = sent.iter().find_map(|c| match c {
        AppCommand::RequestKeyColourSample { nx, ny } => Some((*nx, *ny)),
        _ => None,
    });
    let (nx, ny) = click.unwrap_or_else(|| panic!("no click sent: {sent:?}"));
    assert!((nx - 0.25).abs() < 0.01 && (ny - 0.75).abs() < 0.01, "({nx}, {ny})");
    assert_eq!(state.eyedropper_mode, crate::state::EyedropperMode::None);

    arm(&mut state);
    let esc = frame(&mut state, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert_eq!(state.eyedropper_mode, crate::state::EyedropperMode::None);
    assert!(state.key_colour_pick.is_none(), "the pick called off");
    assert!(!esc.iter().any(|c| matches!(c, AppCommand::RequestKeyColourSample { .. })), "{esc:?}");
}
