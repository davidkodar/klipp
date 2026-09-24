//! Agent title ops -> real document edits -> .klipp save/load -> H.264 pixels.
use klipp_core::*;
use klipp_document::{
    apply::DocumentView,
    ops::{self, Op},
};
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Sequence, TitleObjectContent};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

#[test]
fn authored_masked_word_stagger_and_exit_survive_project_export() {
    if !std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipping export fixture: ffmpeg unavailable");
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-title-authoring");
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution {
        width: 320,
        height: 180,
    };
    let ops: Vec<Op> = serde_json::from_value(json!([
        {"op":"generator", "data":{"SolidColor":{"color":[0,0,0,255]}},"at":{"secs":0},"secs":4},
        {"op":"title","text":"MASKED WORDS","at":{"secs":0},"secs":4,"size":144,"font":"DejaVu Sans","color":"#ffffff",
         "enter":{"kind":"mask_rise","duration_secs":0.5,"delay_secs":0.2,"easing":"linear","stagger":"word","stagger_secs":0.45},
         "exit":{"kind":"mask_drop","duration_secs":0.5,"delay_secs":0.2,"easing":"linear","stagger":"word","stagger_secs":0.2}}
    ])).unwrap();
    let mut pool = MediaPool::new();
    let plan = ops::resolve(&seq, &pool, &ops).unwrap();
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView {
        sequences: &mut sequences,
        active: 0,
        media_pool: &mut pool,
        fps: &mut fps,
    };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    let title = doc.sequences[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find_map(|c| c.title_data())
        .unwrap()
        .clone();
    let project = dir.join("motion.klipp");
    klipp_project::save_project(&project, &sequences, &pool).unwrap();
    let (sequences, pool, ..) = klipp_project::load_project(&project).unwrap();
    let restored = sequences[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find_map(|c| c.title_data())
        .unwrap();
    assert_eq!(*restored, title);
    assert!(matches!(restored.objects[0].content, TitleObjectContent::TextBox(_)), "text");
    let motion = restored.objects[0].fx().motion;
    assert_eq!(motion.enter.unwrap().stagger_secs, 0.45);
    assert_eq!(motion.exit.unwrap().delay_secs, 0.2);
    let out = dir.join("masked-words.mkv");
    let settings = ExportSettings {
        output_path: out.clone(),
        video_codec: ExportVideoCodec::H264,
        format: ExportFormat::Mkv,
        audio_codec: ExportAudioCodec::Copy,
        // Text tracks follow the existing burn-in delivery switch.
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
    }
    .run()
    .unwrap();
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let size = 320 * 180 * 3;
    assert_eq!(decoded.stdout.len(), size * frames as usize);
    let ink: Vec<usize> = decoded
        .stdout
        .chunks_exact(size)
        .map(|frame| {
            frame
                .chunks_exact(3)
                .filter(|rgb| rgb.iter().all(|&v| v > 180))
                .count()
        })
        .collect();
    let settled = ink[48];
    assert!(settled > 100, "readable settled text: {settled}");
    assert_eq!(ink[0], 0, "entrance delay starts blank");
    assert!(
        ink[14] > 0 && ink[14] < settled * 3 / 4,
        "only the first word is entering: {} / {settled}",
        ink[14]
    );
    assert!(
        ink[82] > 0 && ink[82] < settled * 3 / 4,
        "independent exit leaves before the end: {} / {settled}",
        ink[82]
    );
    assert_eq!(ink[95], 0, "exit delay leaves a blank tail");
    println!(
        "export {}: ink at frames 0/14/48/82/95 = {:?}",
        out.display(),
        [ink[0], ink[14], settled, ink[82], ink[95]]
    );
}
