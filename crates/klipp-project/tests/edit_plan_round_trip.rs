//! The edit plan is part of the project: saved with the sequence,
//! approved or not, and absent from a project saved before there was one.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{EditPlan, PlanMusic, PlanSegment, PlanTitle, Sequence};

#[test]
fn an_edit_plan_comes_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_edit_plan_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let media = klipp_core::MediaId::new();
    let plan = EditPlan {
        segments: vec![
            PlanSegment { feature: "Search".into(), media_id: media, source_in_secs: 12.0, source_out_secs: 18.5, note: "the search screen settles".into(), with_audio: false, title: Some(PlanTitle { text: "Search".into(), secs: Some(2.5), style: "heading".into(), color: Some("#7aa2f7".into()), fill: None }), transition: None, zoom: None },
            PlanSegment { feature: String::new(), media_id: media, source_in_secs: 40.0, source_out_secs: 44.0, note: String::new(), with_audio: false, title: None, transition: Some("cross_dissolve".into()), zoom: None },
        ],
        music: Some(PlanMusic { media_id: klipp_core::MediaId::new(), duck_db: Some(12.0), source_in_secs: Some(0.08), volume_db: Some(-6.0), fade_in_secs: None, fade_out_secs: Some(2.0) }),
        approved: true,
        notes: "second pass".into(),
    };
    seq.edit_plan = Some(plan.clone());
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].edit_plan.as_ref(), Some(&plan));

    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].edit_plan, None);
    let _ = std::fs::remove_dir_all(&dir);
}
