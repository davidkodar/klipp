//! The Terminal Card composition as placed in a 16:9, a 9:16 and a 1:1
//! sequence: its command sits on one line inside its card, and its
//! cursor just after the last letter, as each layer alone draws them.

use klipp_timeline::{TitleData, TitleObjectContent};
use klipp_title::TitleRenderer;

const FORMATS: &[(&str, u32, u32)] = &[("16:9", 960, 540), ("9:16", 540, 960), ("1:1", 540, 540)];

/// The Terminal Card's title placed in a `w` × `h` sequence, its controls
/// at their defaults.
fn placed(w: u32, h: u32) -> TitleData {
    use klipp_timeline::{Clip, ClipKind, Sequence};
    let t = klipp_timeline::composition_template::builtin_templates().into_iter().find(|t| t.name == "Terminal Card").expect("the Terminal Card");
    let layout = t.composition_for(w, h).expect("a layout").clone();
    let mut clip = Clip::precomposition(layout.id, klipp_core::RationalTime::from_secs_f64(0.0, 24), layout.total_duration());
    clip.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: klipp_timeline::composition_template::instance_for(&t) };
    let mut outer = Sequence::new_default();
    outer.resolution = klipp_core::Resolution { width: w, height: h };
    outer.tracks[0].clips.push(clip);
    let mut all = t.sequences.clone();
    all.push(outer.clone());
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &all);
    let scene = flat.all_clips().find_map(|c| c.precomp.clone()).expect("the scene");
    let found = scene.all_clips().find_map(|c| c.title_data().cloned());
    found.expect("its title")
}

/// The box (x0, y0, x1, y1, in pixels) of what layer `k` alone draws
/// solidly, its glow left out.
fn ink(renderer: &mut TitleRenderer, td: &TitleData, k: usize, w: u32, h: u32) -> [u32; 4] {
    let mut one = td.clone();
    one.objects = vec![td.objects[k].clone()];
    let px = renderer.render(&one, w, h);
    let mut b = [u32::MAX, u32::MAX, 0, 0];
    for (i, p) in px.chunks(4).enumerate() {
        if p[3] > 200 {
            let (x, y) = (i as u32 % w, i as u32 / w);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
    }
    assert!(b[0] <= b[2], "layer {k} draws nothing");
    b
}

#[test]
fn the_command_sits_in_its_card_on_one_line_with_the_cursor_after_it() {
    let mut renderer = TitleRenderer::new();
    for &(name, w, h) in FORMATS {
        let td = placed(w, h);
        let index = |text: bool| td.objects.iter().enumerate().filter(|(_, o)| matches!(o.content, TitleObjectContent::TextBox(_)) == text).map(|(i, _)| i).collect::<Vec<_>>();
        let (texts, shapes) = (index(true), index(false));
        assert_eq!((texts.len(), shapes.len()), (1, 2), "{name}: a card, a command and a cursor");
        let font_px = match &td.objects[texts[0]].content {
            TitleObjectContent::TextBox(tb) => tb.font_size * w.min(h) as f32 / 1080.0,
            _ => unreachable!(),
        };
        let card = ink(&mut renderer, &td, shapes[0], w, h);
        let command = ink(&mut renderer, &td, texts[0], w, h);
        let cursor = ink(&mut renderer, &td, shapes[1], w, h);
        let margin = (font_px * 0.5) as u32;
        assert!(command[0] >= card[0] + margin && command[2] + margin <= card[2], "{name}: the command {command:?} inside its card {card:?}");
        assert!(((command[3] - command[1]) as f32) < font_px * 1.6, "{name}: the command on one line, {command:?} at {font_px} px");
        let gap = cursor[0] as f32 - command[2] as f32;
        assert!(gap > 0.0 && gap < font_px, "{name}: the cursor {cursor:?} just after the command {command:?}");
        assert!(cursor[2] + margin <= card[2], "{name}: the cursor {cursor:?} inside its card {card:?}");
    }
}
