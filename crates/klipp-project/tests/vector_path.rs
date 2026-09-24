use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_timeline::{
    Clip, ClipKind, Sequence, Shape, ShapeKind, TitleData, TitleObjectContent, VectorPath,
};
#[test]
fn path_titles_round_trip_with_an_explicit_unsupported_reader_tag() {
    let dir = std::env::temp_dir().join(format!("klipp-path-test-{}", klipp_core::ClipId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = VectorPath::default();
    p.key(0.0, p.pose.clone()).unwrap();
    let mut end = p.pose.clone();
    end.fill = [1.0, 0.0, 0.0, 1.0];
    end.contours[0].vertices[0].outgoing = [0.3, 0.7];
    p.key(1.0, end).unwrap();
    p.pose.stroke_glow=Some(klipp_timeline::StrokeGlow{radius:24.0,strength:0.8});
    p.trim = Some(klipp_timeline::PathTrim::default());
    p.trim
        .as_mut()
        .unwrap()
        .key(
            0.5,
            klipp_timeline::TrimRange {
                end: 0.4,
                ..Default::default()
            },
        )
        .unwrap();
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Path(p.clone()),
            fit: None,
            fill: None,
            stroke: None,
        }),
        klipp_timeline::default_text_transform(),
    );
    let mut seq = Sequence::new_default();
    let mut clip = Clip::gap(
        Timecode::from_secs_f64(0.0, 24),
        Timecode::from_secs_f64(2.0, 24),
    );
    clip.kind = ClipKind::Title(td.clone());
    seq.tracks[0].clips = vec![clip];
    klipp_project::save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(text.contains("vector_title"));
    let (loaded, ..) = klipp_project::load_project(&dir).unwrap();
    assert_eq!(loaded[0].tracks[0].clips[0].title_data(), Some(&td));
    if let Ok(output) = std::env::var("KLIPP_PATH_FIXTURE") {
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(std::path::Path::new(&output).join("project.json"), &text).unwrap();
    }
    let broken = text.replacen("\"closed\": true", "\"closed\": false", 1);
    std::fs::write(dir.join("project.json"), &broken).unwrap();
    let error = klipp_project::load_project(&dir).unwrap_err();
    assert!(error.contains("topology mismatch"), "{error}");
    let legacy = broken.replace("\"type\": \"vector_title\"", "\"type\": \"title\", \"text\": \"\"");
    std::fs::write(dir.join("project.json"), legacy).unwrap();
    assert!(klipp_project::load_project(&dir).unwrap_err().contains("topology mismatch"));
    std::fs::remove_dir_all(dir).unwrap();
}
