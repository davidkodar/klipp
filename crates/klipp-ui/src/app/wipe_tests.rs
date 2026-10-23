//! A wipe against another clip (#899): the viewer's split shows the wiped
//! clip graded on its left, named, the current frame on its right.

use super::*;

/// A grey ramp, 2 s at 24 fps, written once with the ffmpeg CLI.
fn ramp() -> Option<std::path::PathBuf> {
    static RAMP: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("klipp-wipe-{}.mkv", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=24:d=2,geq=lum='60+2*N':cb=128:cr=128", "-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(path)
    })
    .clone()
}

#[test]
fn the_viewer_wipes_against_another_clip_graded() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let mut state = AppState { playback: klipp_engine::PlaybackState::Paused, show_color_panel: true, ..Default::default() };
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    let rt = |f| klipp_core::RationalTime { value: f, rate: 24 };
    item.duration = rt(48);
    item.fps = klipp_core::Fps::new(24, 1);
    item.resolution = klipp_core::Resolution { width: 64, height: 36 };
    item.filename = "ramp.mkv".into();
    let media = state.media_pool.add(item);
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let (a, mut b) = (klipp_timeline::Clip::media(media, rt(0), rt(48), rt(0), rt(48)), klipp_timeline::Clip::media(media, rt(48), rt(96), rt(0), rt(48)));
    b.name = Some("Reverse".into());
    let (ia, ib) = (a.id, b.id);
    state.sequences[0].tracks[v1].clips = vec![a, b];
    state.selected_clip = Some(ia);
    state.wipe_clip = crate::graded_thumbs::wipe_target(&state.sequences[0], state.selected_clip, state.playhead, klipp_core::WipeClip::Next);
    assert_eq!(state.wipe_clip, Some(ib), "the next shot");

    let ctx = egui::Context::default();
    let ws = klipp_core::WorkingColorspace::Rec709Linear;
    let wipe = state.wipe_clip;
    state.graded_thumbs.update(&ctx, 0.0, &state.sequences[0], &state.media_pool, ws, wipe);
    state.graded_thumbs.settle(&ctx);
    let frame = state.graded_thumbs.wipe_frame(ib).expect("the wiped clip's graded frame");
    assert_eq!(frame.size()[0], crate::graded_thumbs::WIPE_WIDTH as usize, "drawn at the viewer's size");
    let frame = frame.id();

    let mut panel = ViewerPanel::default();
    let theme = KlippTheme::default();
    let mut shapes = Vec::new();
    for t in 0..3 {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(t as f64 * 0.05), ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                let _ = panel.show(ui, &mut state, &theme, &crate::keybindings::KeyBindings::default());
            });
        });
        shapes = out.shapes;
    }
    let drew = shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id == frame) || matches!(&s.shape, egui::Shape::Rect(r) if r.fill_texture_id == frame));
    assert!(drew, "the wiped clip's frame isn't on the split's left");
    let texts: Vec<String> = shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect();
    assert!(texts.iter().any(|t| t == "Clip: Reverse") && texts.iter().any(|t| t == "Now"), "the split says what it shows: {texts:?}");

    // On the Edit page, no wipe.
    state.show_color_panel = false;
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(1.0), ..Default::default() };
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            let _ = panel.show(ui, &mut state, &theme, &crate::keybindings::KeyBindings::default());
        });
    });
    assert!(!out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id == frame) || matches!(&s.shape, egui::Shape::Rect(r) if r.fill_texture_id == frame)), "the Edit page's viewer wipes");
}
