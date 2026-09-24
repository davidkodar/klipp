use klipp_document::{
    apply::DocumentView,
    ops::{self, Op},
};
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, ShapeKind, TitleObjectContent, VectorPath};
use serde_json::json;
#[test]
fn agent_paths_share_the_model_and_undo_without_losing_text() {
    let mut sequences = vec![Sequence::new_default()];
    let mut pool = MediaPool::new();
    let mut fps = sequences[0].fps;
    sequences[0].tracks[0].clips.push(klipp_timeline::Clip::gap(
        klipp_core::Timecode::from_secs_f64(0.0, 24),
        klipp_core::Timecode::from_secs_f64(4.0, 24),
    ));
    let title: Op =
        serde_json::from_value(json!({"op":"title","text":"Keep me","at":{"secs":0},"secs":4}))
            .unwrap();
    let planned = ops::resolve(&sequences[0], &pool, &[title]).unwrap();
    {
        let mut doc = DocumentView {
            sequences: &mut sequences,
            active: 0,
            media_pool: &mut pool,
            fps: &mut fps,
        };
        for s in &planned.steps {
            doc.apply_forward(&s.edit);
        }
    }
    let clip = sequences[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.title_data().is_some())
        .unwrap();
    let id = clip.id;
    let old = clip.title_data().unwrap().clone();
    let mut path = VectorPath::default();
    path.key(0.0, path.pose.clone()).unwrap();
    let mut last = path.pose.clone();
    last.contours[0].vertices[0].incoming = [0.2, 0.7];
    last.fill = [1.0, 0.2, 0.0, 1.0];
    path.key(2.0, last).unwrap();
    let op: Op =
        serde_json::from_value(json!({"op":"vector_path","clip_id":id,"path":path})).unwrap();
    let plan = ops::resolve(&sequences[0], &pool, &[op]).unwrap();
    let mut doc = DocumentView {
        sequences: &mut sequences,
        active: 0,
        media_pool: &mut pool,
        fps: &mut fps,
    };
    for s in &plan.steps {
        doc.apply_forward(&s.edit);
    }
    let data = doc.sequences[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == id)
        .unwrap()
        .title_data()
        .unwrap();
    assert_eq!(data.objects.len(), old.objects.len() + 1);
    assert!(
        matches!(&data.objects.last().unwrap().content,TitleObjectContent::Shape(s) if s.kind==ShapeKind::Path(path.clone()))
    );
    for s in plan.steps.iter().rev() {
        doc.apply_inverse(&s.edit);
    }
    assert_eq!(
        doc.sequences[0]
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
            .unwrap()
            .title_data(),
        Some(&old)
    );
}
