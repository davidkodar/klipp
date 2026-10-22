//! A review comment's author, status and round come back from a project
//! as they went in (#2214), and a marker that is no review comment still
//! writes no review field.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Marker, ReviewNote, Sequence};

#[test]
fn review_markers_come_back_with_their_author_status_and_round() {
    let dir = std::env::temp_dir().join(format!("klipp_review_markers_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut seq = Sequence::new_default();
    let at = |secs: f64| klipp_core::RationalTime::from_secs_f64(secs, 24);
    seq.markers.push(Marker {
        id: klipp_core::MarkerId::new(), position: at(3.0), label: "Ana".into(), color: klipp_core::MarkerColor::Red, duration: None,
        comment: "Tighten this".into(), kind: klipp_core::MarkerKind::Review,
        review: Some(ReviewNote { author: "Ana".into(), status: klipp_core::ReviewStatus::WontDo, round: 2 }),
    });
    seq.markers.push(Marker {
        id: klipp_core::MarkerId::new(), position: at(5.0), label: "Chapter".into(), color: klipp_core::MarkerColor::Blue, duration: None,
        comment: String::new(), kind: klipp_core::MarkerKind::Chapter, review: None,
    });
    save_project(&dir, &[seq.clone()], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"review\"").count(), 1, "only the review comment writes the field");
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].markers, seq.markers);
    let _ = std::fs::remove_dir_all(&dir);
}
