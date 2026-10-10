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
fn curved_connector_and_device_outline_draw_pause_and_retract() {
    let dir = klipp_testing::scratch::scratch_dir("klipp-vector-export");
    let mut p = VectorPath::default();
    p.pose.fill = [0.0; 4];
    p.pose.stroke = [0.1, 0.8, 1.0, 1.0];
    p.pose.stroke_width = 14.0;
    use klipp_timeline::{PathContour, PathTrim, PathVertex, TrimRange};
    let vertex = |point| PathVertex {
        point,
        incoming: point,
        outgoing: point,
    };
    let mut connector = PathContour {
        closed: false,
        vertices: vec![vertex([0.1, 0.5]), vertex([0.55, 0.5])],
    };
    connector.vertices[0].outgoing = [0.2, 0.05];
    connector.vertices[1].incoming = [0.4, 0.95];
    let mut device = PathContour {
        closed: true,
        vertices: vec![
            vertex([0.69, 0.15]),
            vertex([0.86, 0.15]),
            vertex([0.9, 0.19]),
            vertex([0.9, 0.81]),
            vertex([0.86, 0.85]),
            vertex([0.69, 0.85]),
            vertex([0.65, 0.81]),
            vertex([0.65, 0.19]),
        ],
    };
    let d = 0.04 * 0.5522848;
    device.vertices[1].outgoing = [0.86 + d, 0.15];
    device.vertices[2].incoming = [0.9, 0.19 - d];
    device.vertices[3].outgoing = [0.9, 0.81 + d];
    device.vertices[4].incoming = [0.86 + d, 0.85];
    device.vertices[5].outgoing = [0.69 - d, 0.85];
    device.vertices[6].incoming = [0.65, 0.81 + d];
    device.vertices[7].outgoing = [0.65, 0.19 - d];
    device.vertices[0].incoming = [0.69 - d, 0.15];
    p.pose.contours = vec![connector, device];
    p.pose.stroke_glow=Some(klipp_timeline::StrokeGlow{radius:30.0,strength:0.9});
    let mut trim = PathTrim::default();
    trim.key(
        0.0,
        TrimRange {
            end: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    trim.key(1.0, TrimRange::default()).unwrap();
    trim.key(2.0, TrimRange::default()).unwrap();
    trim.key(
        3.0,
        TrimRange {
            start: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    p.trim = Some(trim);
    let mut td = TitleData::default();
    td.objects.clear();
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Path(p),
            fill: None,
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
        width: 640,
        height: 360,
    };
    let mut clip = Clip::gap(
        Timecode::from_secs_f64(0.0, 24),
        Timecode::from_secs_f64(4.0, 24),
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
        analyser: None,
        total_frames: frames,
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
    let size = 640 * 360 * 3;
    assert_eq!(decoded.stdout.len(), size * frames as usize);
    let areas: Vec<_> = decoded
        .stdout
        .chunks_exact(size)
        .map(|f| f.chunks_exact(3).filter(|p| p[2] > 80).count())
        .collect();
    assert_eq!(areas[0], 0);
    assert!(areas[24] > 500);
    assert!(areas[12] > 0 && areas[12] < areas[24]);
    assert!(areas[60] > 0 && areas[60] < areas[24]);
    assert_eq!(areas[84], 0);
    if let Ok(path) = std::env::var("KLIPP_TRIM_EXPORT_PROOF") {
        std::fs::copy(out, path).unwrap();
    }
}
