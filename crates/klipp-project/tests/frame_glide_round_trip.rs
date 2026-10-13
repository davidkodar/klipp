//! A clip's Frame Glide (#611) is part of the project: its framings and
//! easing come back on opening, with the keys it wrote. A clip without
//! one writes nothing for it.

use klipp_core::frame_glide::{FrameGlide, GlideEasing, GlideFrame};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_frame_glide_comes_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_frame_glide_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let solid = klipp_core::GeneratorData::SolidColor { color: [10, 10, 30, 255] };
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let card = |a: f64, b: f64| klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(solid.clone()), ..klipp_timeline::Clip::gap(tc(a), tc(b)) };
    let glide = FrameGlide { start: GlideFrame::FULL, end: GlideFrame { x: 0.1, y: -0.05, width: 0.6 }, easing: GlideEasing::EaseOut };
    let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
    let glided = klipp_timeline::frame_glide::set(&card(0.0, 3.0), Some(glide), false, w, h).unwrap();
    seq.tracks[0].clips = vec![glided.clone(), card(3.0, 5.0)];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let clips = &sequences[0].tracks[0].clips;
    assert_eq!(clips[0].frame_glide, Some(glide));
    let scale = |c: &klipp_timeline::Clip| klipp_timeline::frame_glide::lane(c, "scale");
    assert_eq!(scale(&clips[0]), scale(&glided));
    assert_eq!(scale(&glided).len(), 2, "the keys it wrote");
    assert_eq!(clips[1].frame_glide, None);
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"frame_glide\"").count(), 1, "only the glided clip writes one");
    let _ = std::fs::remove_dir_all(&dir);
}
