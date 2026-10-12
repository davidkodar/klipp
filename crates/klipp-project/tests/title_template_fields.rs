//! A title applied from a motion-graphics template keeps what stays
//! editable (#1139) through a real save and a strict reload: its published
//! fields, a speed moved off 1.0 among them, and the timing that speed
//! gives.

use klipp_core::TitleFieldValue;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, TrackKind};

#[test]
fn a_template_title_keeps_its_fields_through_a_save() {
    let mut td = TitleData::single_text(TextBox { text: "Jane Doe".into(), ..Default::default() });
    td.objects[0].fx_mut().motion.enter = Some(klipp_core::title_motion::Motion::new(klipp_core::title_motion::MotionKind::Rise).duration(0.6));
    td.published = td.default_fields();
    td.set_field(2, &TitleFieldValue::Speed(2.0)).unwrap();
    let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::gap(t(0.0), t(4.0));
    clip.kind = ClipKind::Title(td.clone());
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(clip);

    let dir = std::env::temp_dir().join(format!("klipp_title_template_fields_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap_or_else(|e| panic!("a strict reload: {e}"));
    let _ = std::fs::remove_dir_all(&dir);

    let back = loaded[0].all_clips().find(|c| c.id == id).and_then(|c| c.title_data()).expect("the title").clone();
    assert_eq!(back.published, td.published, "the fields, the speed's factor and base");
    assert_eq!(back.field_value(2), Some(TitleFieldValue::Speed(2.0)));
    assert_eq!(back.objects[0].fx().motion.enter.unwrap().duration_secs, 0.3, "the timing that speed gives");
}
