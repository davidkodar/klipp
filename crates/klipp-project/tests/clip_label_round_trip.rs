//! A clip's own name and colour on the timeline (#920) are part of the
//! project, and a clip without them writes neither, so a project from
//! before there could be one reads as it did.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_clip_s_name_and_colour_come_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_clip_label_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let solid = klipp_core::GeneratorData::SolidColor { color: [10, 10, 30, 255] };
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut named = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(solid.clone()), ..klipp_timeline::Clip::gap(tc(0.0), tc(2.0)) };
    named.name = Some("Opening card".into());
    named.colour_label = 5;
    let plain = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(solid), ..klipp_timeline::Clip::gap(tc(2.0), tc(4.0)) };
    seq.tracks[0].clips = vec![named, plain];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let labels: Vec<(Option<String>, u8)> = sequences[0].tracks[0].clips.iter().map(|c| (c.name.clone(), c.colour_label)).collect();
    assert_eq!(labels, vec![(Some("Opening card".into()), 5), (None, 0)]);
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"colour_label\"").count(), 1, "only the coloured clip writes a colour");
    assert_eq!(written.matches("\"Opening card\"").count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
