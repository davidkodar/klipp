use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{
    Clip, ClipKind, Sequence, Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition,
    VectorPath,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
#[test]
fn a_saved_vector_morph_renders_in_the_export() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-vector-export");
    let mut p = VectorPath::default();
    p.pose.fill = [1.0; 4];
    p.pose.stroke_width = 0.0;
    p.key(0.0, p.pose.clone()).unwrap();
    let mut end = p.pose.clone();
    for v in &mut end.contours[0].vertices {
        for a in [&mut v.point, &mut v.incoming, &mut v.outgoing] {
            a[0] = 0.5 + (a[0] - 0.5) * 0.5;
        }
    }
    p.key(1.0, end).unwrap();
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Path(p),
            fill: None,
            gradient: None,
            stroke: None,
            fit: None,
        }),
        klipp_timeline::ObjectTransform {
            center: TitlePosition { x: 0.5, y: 0.5 },
            size: TitlePosition { x: 1.0, y: 1.0 },
            ..klipp_timeline::default_text_transform()
        },
    );
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution {
        width: 320,
        height: 180,
    };
    let mut clip = Clip::gap(
        Timecode::from_secs_f64(0.0, 24),
        Timecode::from_secs_f64(2.0, 24),
    );
    clip.kind = ClipKind::Title(td);
    seq.tracks[0].clips = vec![clip];
    let project = dir.join("vector.klipp");
    klipp_project::save_project(&project, &[seq], &MediaPool::new()).unwrap();
    let (sequences, pool, ..) = klipp_project::load_project(&project).unwrap();
    let out = dir.join("vector.mkv");
    let settings = ExportSettings {
        output_path: out.clone(),
        video_codec: ExportVideoCodec::H264,
        format: ExportFormat::Mkv,
        audio_codec: ExportAudioCodec::Copy,
        burn_in_subtitles: true,
        crf: 1,
        preset: EncoderPreset::Fast,
        ..Default::default()
    };
    let snapshot = SequenceSnapshot::from_sequence(
        &sequences[0],
        &pool,
        &settings,
        None,
        false,
        WorkingColorspace::Rec709Linear,
    )
    .unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.clone(),
        settings,
        snapshot,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Default::default(),
        total_frames: frames,
        analyser: None,
    }
    .run()
    .unwrap();
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    let size = 320 * 180 * 3;
    assert_eq!(decoded.stdout.len(), size * frames as usize);
    let areas: Vec<_> = decoded
        .stdout
        .chunks_exact(size)
        .map(|f| {
            f.chunks_exact(3)
                .filter(|p| p.iter().all(|v| *v > 180))
                .count()
        })
        .collect();
    assert!((areas[0] as f32 / areas[areas.len() - 1] as f32 - 2.0).abs() < 0.1);
    if let Ok(path) = std::env::var("KLIPP_VECTOR_EXPORT_PROOF") {
        std::fs::copy(out, path).unwrap();
    }
}
