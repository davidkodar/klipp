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
    path.trim = Some(klipp_timeline::PathTrim::default());
    path.trim
        .as_mut()
        .unwrap()
        .key(
            1.0,
            klipp_timeline::TrimRange {
                start: 0.2,
                end: 0.7,
                offset: 0.5,
            },
        )
        .unwrap();
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

#[test]
fn a_path_drag_is_one_undo_with_original_and_final_title() {
    use klipp_document::history::{CommandHistory,EditCommand,HistoryEntry};
    let clip_id=klipp_core::ClipId::new();let old=klipp_timeline::TitleData::default();let mut new=old.clone();new.objects.clear();
    let entry=|data:klipp_timeline::TitleData|HistoryEntry{command:EditCommand::EditTitle{clip_id,description:"Edit Vector Path",old_data:data.clone(),new_data:data}};
    let mut h=CommandHistory::new();h.begin_drag("path".into(),entry(old.clone()));h.end_drag(entry(new.clone()));
    assert_eq!(h.undo_count(),1);
    assert!(matches!(&h.undo_entries()[0].command,EditCommand::EditTitle{old_data,new_data,..} if *old_data==old && *new_data==new));
}

/// #67: a ready-made shape is added by name, sized square on screen and
/// placed where asked; asking for both a path and a shape is refused.
#[test]
fn a_ready_made_shape_is_added_by_name() {
    let mut sequences = vec![Sequence::new_default()];
    let mut pool = MediaPool::new();
    let mut fps = sequences[0].fps;
    sequences[0].tracks[0].clips.push(klipp_timeline::Clip::gap(
        klipp_core::Timecode::from_secs_f64(0.0, 24),
        klipp_core::Timecode::from_secs_f64(4.0, 24),
    ));
    let title: Op = serde_json::from_value(json!({"op":"title","text":"Five","at":{"secs":0},"secs":4})).unwrap();
    let planned = ops::resolve(&sequences[0], &pool, &[title]).unwrap();
    let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for s in &planned.steps {
        doc.apply_forward(&s.edit);
    }
    let id = doc.sequences[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.title_data().is_some()).unwrap().id;
    let star: Op = serde_json::from_value(json!({"op":"vector_path","clip_id":id,"shape":{"star":{"points":5,"inner":0.4}},"color":"#ffcc00","x":0.8,"y":0.2})).unwrap();
    let plan = ops::resolve(&doc.sequences[0], doc.media_pool, &[star]).unwrap();
    for s in &plan.steps {
        doc.apply_forward(&s.edit);
    }
    let data = doc.sequences[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == id).unwrap().title_data().unwrap();
    let layer = data.objects.last().unwrap();
    let TitleObjectContent::Shape(shape) = &layer.content else { panic!("a shape layer") };
    let ShapeKind::Path(path) = &shape.kind else { panic!("a path") };
    assert_eq!(path.pose.contours[0].vertices.len(), 10, "five points, five inner corners");
    assert_eq!(path.pose.fill, [1.0, 0.8, 0.0, 1.0]);
    assert_eq!((layer.transform.center.x, layer.transform.center.y), (0.8, 0.2));
    let res = doc.sequences[0].resolution;
    let on_screen = (layer.transform.size.x * res.width as f32, layer.transform.size.y * res.height as f32);
    assert!((on_screen.0 - on_screen.1).abs() < 1.0, "as wide as it is tall on screen: {on_screen:?}");

    let both: Op = serde_json::from_value(json!({"op":"vector_path","clip_id":id,"shape":"ellipse","path":VectorPath::default()})).unwrap();
    assert!(ops::resolve(&doc.sequences[0], doc.media_pool, &[both]).unwrap_err().why.contains("not both"));
    // Nothing to see is refused: a line with no stroke, a box of no width.
    let bare_line: Op = serde_json::from_value(json!({"op":"vector_path","clip_id":id,"shape":"line","stroke_width":0})).unwrap();
    assert!(ops::resolve(&doc.sequences[0], doc.media_pool, &[bare_line]).unwrap_err().why.contains("draws nothing"));
    let flat: Op = serde_json::from_value(json!({"op":"vector_path","clip_id":id,"shape":"ellipse","w":0})).unwrap();
    assert!(ops::resolve(&doc.sequences[0], doc.media_pool, &[flat]).unwrap_err().why.contains("out of range"));
    let wrong: Result<Op, _> = serde_json::from_value(json!({"op":"vector_path","clip_id":id,"shape":"blob"}));
    assert!(wrong.is_err(), "an unknown shape is refused when read");
}
