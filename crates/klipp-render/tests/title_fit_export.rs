//! A lower third's name box that shrinks to fit, with a backing fitted
//! to it (#1722), exported: the long name on its one line inside the
//! box, the backing round it, where the title renderer the viewer shows
//! draws them.
use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence, Shape, ShapeFit, ShapeKind, TextBox, TextFit, TitleData, TitleObjectContent, TitlePosition};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

const W: u32 = 640;
const H: u32 = 360;

/// A name in a box 40% of the frame wide, shrinking, with a red backing
/// fitted to it.
fn lower_third(name: &str) -> TitleData {
    let mut td = TitleData::single_text(TextBox { text: name.into(), font_size: 72.0, fit: TextFit::Shrink { min_size: 24.0 }, ..Default::default() });
    td.objects[0].transform.size = TitlePosition { x: 0.4, y: 0.15 };
    td.objects[0].transform.center = TitlePosition { x: 0.3, y: 0.8 };
    let target = td.objects[0].id;
    td.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 0.0 },
            fill: Some([255, 0, 0, 255]),
            gradient: None,
            stroke: None,
            fit: Some(ShapeFit { target, padding: TitlePosition { x: 20.0, y: 12.0 } }),
        }),
        klipp_timeline::default_text_transform(),
    );
    // The backing under the text.
    td.objects.swap(0, 1);
    td
}

/// Where a frame is red, as (x0, y0, x1, y1): `px` RGB or RGBA.
fn red_bounds(px: &[u8], channels: usize) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for (i, p) in px.chunks_exact(channels).enumerate() {
        if p[0] > 200 && p[1] < 60 && p[2] < 60 && (channels == 3 || p[3] > 200) {
            let (x, y) = (i % W as usize, i / W as usize);
            b = Some(b.map_or((x, y, x + 1, y + 1), |(x0, y0, x1, y1)| (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1))));
        }
    }
    b
}

fn export(td: TitleData, out: &std::path::Path) -> Vec<u8> {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: W, height: H };
    let tc = |s| Timecode::from_secs_f64(s, 24);
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(0.5)) }];
    let pool = MediaPool::new();
    let settings = ExportSettings {
        output_path: out.to_path_buf(),
        video_codec: ExportVideoCodec::H264,
        format: ExportFormat::Mkv,
        audio_codec: ExportAudioCodec::Copy,
        crf: 1,
        preset: EncoderPreset::Fast,
        ..Default::default()
    };
    let snapshot = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.to_path_buf(),
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
        .arg(out)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    decoded.stdout
}

#[test]
fn a_shrinking_name_and_its_backing_export_as_the_viewer_draws_them() {
    if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping export fixture: ffmpeg unavailable");
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-title-fit");
    let mut viewer = klipp_title::TitleRenderer::new();
    let (short, long) = (lower_third("Ana Ruiz"), lower_third("Dr. Elizabeth Montgomery-Smith"));
    let drawn = red_bounds(&viewer.render(&long, W, H), 4).expect("the viewer draws the backing");
    let layout = viewer.render_layout(&long, W, H);
    let short_layout = viewer.render_layout(&short, W, H);
    // One line, shrunk into the 256 px box.
    let (tw, th) = (layout.text_right - layout.text_left, layout.text_bottom - layout.text_top);
    assert!(tw <= 0.4 * W as f32 + 1.0 && tw > 0.4 * W as f32 * 0.85, "the name is {tw} px in a 256 px box");
    assert!(th <= short_layout.text_bottom - short_layout.text_top + 1.0, "on one line: {th} px tall");

    let exported = red_bounds(&export(long, &dir.join("long.mkv")), 3).expect("the export draws the backing");
    eprintln!("viewer backing {drawn:?}, export {exported:?}");
    for (v, e) in [(drawn.0, exported.0), (drawn.1, exported.1), (drawn.2, exported.2), (drawn.3, exported.3)] {
        assert!((v as i32 - e as i32).abs() <= 2, "the export's backing {exported:?} is not the viewer's {drawn:?}");
    }
    // The backing grows with the name: wider for the long one, the box
    // and its padding at most.
    let short_backing = red_bounds(&export(short, &dir.join("short.mkv")), 3).expect("the short name's backing");
    assert!(exported.2 - exported.0 > short_backing.2 - short_backing.0, "{exported:?} vs {short_backing:?}");
    let pad = (20.0 * W as f32 / 1920.0).ceil() as usize;
    assert!(exported.2 - exported.0 <= (0.4 * W as f32) as usize + 2 * pad + 3, "{exported:?}");
}
