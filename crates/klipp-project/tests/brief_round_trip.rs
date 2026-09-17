//! The brief is part of the project: saved with the sequence, there when
//! it is opened again, and absent from a project saved before there was
//! one without that being an error.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Brief, CastMember, Sequence};

#[test]
fn a_brief_comes_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_brief_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let brief = Brief {
        title: "Why Omarchy is not afraid of AI code".into(),
        about: "DHH on agents".into(),
        platform: "web".into(),
        target_secs: Some(90.0),
        width: Some(1920),
        height: Some(1080),
        loudness_lufs: Some(-14.0),
        captions: true,
        cast: vec![CastMember { name: "David Heinemeier Hansson".into(), role: "Creator of Omarchy".into(), described: "on the right".into() }],
        ..Default::default()
    };
    seq.brief = Some(brief.clone());
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].brief.as_ref(), Some(&brief));

    // Without one, nothing is written for it and nothing comes back.
    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].brief, None);
    let _ = std::fs::remove_dir_all(&dir);
}
