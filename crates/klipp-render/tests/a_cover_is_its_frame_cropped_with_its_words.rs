//! A cover (#3311) is drawn by the export renderer: the sequence at the
//! cover's size, the crop a still camera on the frame, the words on top
//! out of the camera's reach.

use klipp_core::*;
use klipp_render::*;
use klipp_timeline::cover::{cover_sequence, Cover, CoverCrop};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData};

fn bars() -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 1920, height: 1080 };
    let tc = |f| RationalTime::from_frames(f, 25);
    let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    seq.tracks[v].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::ColorBars100), source_in: tc(0), source_out: tc(50), ..Clip::gap(tc(0), tc(50)) }];
    seq
}

fn drawn(seq: &Sequence, cover: &Cover) -> still::Still {
    let (w, h) = (640, 360);
    let drawn = cover_sequence(seq, cover, w, h).unwrap();
    let snap = SequenceSnapshot::from_sequence_with_all(&drawn, std::slice::from_ref(&drawn), &klipp_media::MediaPool::new(), &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    still::timeline_still(&snap, cover.at_secs, w, h).expect("a cover")
}

#[test]
fn a_cover_is_its_frame_cropped_with_its_words() {
    let seq = bars();
    // The seventh bar, blue, filling the cover: zoomed to a seventh,
    // centred on it.
    let mut cover = Cover { id: CoverId::new(), name: String::new(), at_secs: 1.0, size: "youtube".into(), crop: Some(CoverCrop { x: 6.5 / 7.0, y: 0.5, zoom: 7.0 }), text: None, with_titles: false };
    let plain = drawn(&seq, &cover);
    let px = |s: &still::Still, x: u32, y: u32| { let k = ((y * s.width + x) * 4) as usize; [s.rgba[k], s.rgba[k + 1], s.rgba[k + 2]] };
    for (x, y) in [(4, 4), (600, 4), (320, 180), (4, 355), (600, 355)] {
        let [r, g, b] = px(&plain, x, y);
        assert!(r < 30 && g < 30 && b > 220, "the blue bar fills the cover: {:?} at {x},{y}", [r, g, b]);
    }
    let bright = |s: &still::Still| s.rgba.chunks_exact(4).filter(|p| p[0] > 200 && p[1] > 200).count();
    assert_eq!(bright(&plain), 0, "no words, nothing white");

    // The words, white, at their own size: out of the camera's reach, so
    // not seven times as large.
    cover.text = Some(TitleData::single_text(TextBox { text: "Hi".into(), font_size: 120.0, color: [255, 255, 255, 255], ..Default::default() }));
    let worded = drawn(&seq, &cover);
    let white = bright(&worded);
    let total = (worded.width * worded.height) as usize;
    assert!(white > 200 && white < total / 4, "the words show, at their size: {white} of {total} pixels white");
    // Several frames by one renderer are each the frame drawn alone.
    let drawn_seq = cover_sequence(&seq, &cover, 640, 360).unwrap();
    let snap = SequenceSnapshot::from_sequence_with_all(&drawn_seq, std::slice::from_ref(&drawn_seq), &klipp_media::MediaPool::new(), &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    let both = still::timeline_stills(&snap, &[1.0, 0.5], 640, 360);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].as_ref().unwrap(), &worded, "the first of two is the one drawn alone");
    let [r, g, b] = px(&worded, 4, 4);
    assert!(r < 30 && g < 30 && b > 220, "the bar still under them: {:?}", [r, g, b]);
}

/// What the words of a cover made by the `cover` op change against the same
/// cover without them: how many pixels, of how many, and how near any comes
/// to the frame's sides.
fn words_drawn(fps: Fps, at: f64, size: &str, title: serde_json::Value) -> (usize, usize, u32) {
    let mut seq = bars();
    seq.fps = fps;
    let rate = fps.rate();
    let tc = |secs: f64| RationalTime::from_secs_f64(secs, rate);
    let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    seq.tracks[v].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::ColorBars100), source_in: tc(0.0), source_out: tc(4.0), ..Clip::gap(tc(0.0), tc(4.0)) }];
    let pool = klipp_media::MediaPool::new();
    let op: klipp_document::ops::Op = serde_json::from_value(serde_json::json!({"op": "cover", "name": "Main", "at": {"secs": at}, "size": size, "title": title})).unwrap();
    let plan = klipp_document::ops::resolve(&seq, &pool, &[op]).unwrap_or_else(|r| panic!("{r:?}"));
    let mut seqs = vec![seq];
    let (mut p, mut f) = (pool.clone(), fps);
    let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut f };
    view.apply_forward(&plan.steps[0].edit);
    let seq = seqs.remove(0);
    let cover = seq.covers[0].clone();
    let dims = klipp_core::cover_sizes::by_id(size).unwrap();
    let (w, h) = (dims.width, dims.height);
    let look = cover::Look { lut: None, working: WorkingColorspace::default(), grade_encoding: Default::default() };
    let no_analysis = |_: &std::path::Path| Default::default();
    let worded = cover::draw(&seq, std::slice::from_ref(&seq), &pool, &look, &cover, w, h, &no_analysis).unwrap();
    let bare = cover::draw(&seq, std::slice::from_ref(&seq), &pool, &look, &Cover { text: None, ..cover.clone() }, w, h, &no_analysis).unwrap();
    let mut differ = 0;
    let mut margin = w;
    for (k, (a, b)) in worded.rgba.chunks_exact(4).zip(bare.rgba.chunks_exact(4)).enumerate() {
        if a[0].abs_diff(b[0]) as u32 + a[1].abs_diff(b[1]) as u32 + a[2].abs_diff(b[2]) as u32 > 60 {
            differ += 1;
            let x = k as u32 % w;
            margin = margin.min(x).min(w - 1 - x);
        }
    }
    (differ, (w * h) as usize, margin)
}

/// The words the op makes are drawn in the cover: the title op gives them
/// an entrance, and the one frame drawn was its start, with nothing on screen.
/// At each kind of size, as a title's fields or plain words, at whole and
/// NTSC rates and a time between frames.
#[test]
fn a_covers_words_are_drawn_when_the_op_makes_them() {
    let fields = serde_json::json!({"text": "Seaside mornings"});
    let plain = serde_json::json!("SUNSET");
    for (fps, at, size, title) in [
        (Fps::new(25, 1), 0.75, "youtube", &fields),
        (Fps::new(25, 1), 0.75, "vertical", &fields),
        (Fps::new(25, 1), 0.75, "square", &fields),
        (Fps::new(25, 1), 1.0, "youtube", &plain),
        (Fps::new(30000, 1001), 1.23, "youtube", &fields),
        (Fps::new(50, 1), 0.75, "vertical", &plain),
    ] {
        let (differ, total, margin) = words_drawn(fps, at, size, title.clone());
        println!("WORDS {:.3} fps at {at} {size} {title}: {differ} of {total}, {margin} px from a side", fps.as_f64());
        assert!(differ > 500 && differ < total / 3, "{size} at {:.3} fps, {at} s: {differ} of {total} pixels changed by the words", fps.as_f64());
        assert!(margin > 0, "{size}: the words reach the frame's side");
    }
}

/// A `w` × `h` file of grey with a red patch where a face might be, on its own
/// sequence at 16:9 or as it is, in a pool: and the means to draw its covers.
struct Filmed {
    seq: Sequence,
    pool: klipp_media::MediaPool,
}

fn filmed(name: &str, w: u32, h: u32, patch_x: u32) -> Option<Filmed> {
    let dir = klipp_testing::scratch::scratch_dir("klipp-cover-fill");
    let path = dir.join(name);
    let source = format!("color=c=gray:s={w}x{h}:r=25:d=2,drawbox=x={patch_x}:y={}:w=120:h=120:color=red:t=fill", h / 2 - 60);
    let made = std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &source, "-c:v", "libx264", "-qp", "10", "-pix_fmt", "yuv420p"]).arg(&path).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return None;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    item.fps = Fps::new(25, 1);
    item.resolution = Resolution { width: w, height: h };
    item.video_codec = "h264".into();
    item.duration = RationalTime::from_frames(50, 25);
    let mut pool = klipp_media::MediaPool::new();
    let media_id = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 1280, height: 720 };
    let tc = |f| RationalTime::from_frames(f, 25);
    let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    seq.tracks[v].clips = vec![Clip { kind: ClipKind::Media { media_id }, source_in: tc(0), source_out: tc(50), ..Clip::gap(tc(0), tc(50)) }];
    Some(Filmed { seq, pool })
}

impl Filmed {
    fn drawn(&self, size: &str, crop: Option<CoverCrop>, face_x: Option<f32>) -> still::Still {
        let dims = klipp_core::cover_sizes::by_id(size).unwrap();
        let cover = Cover { id: CoverId::new(), name: String::new(), at_secs: 1.0, size: size.into(), crop, text: None, with_titles: false };
        let look = cover::Look { lut: None, working: WorkingColorspace::default(), grade_encoding: Default::default() };
        let analysis = |_: &std::path::Path| {
            let faces = face_x.map(|x| klipp_analysis::framing::FaceBox { x, y: 0.42, w: 0.09, h: 0.16, score: 0.9, landmarks: Vec::new() });
            let sample = klipp_analysis::framing::FrameSample { secs: 1.0, people: Vec::new(), faces: faces.into_iter().collect() };
            klipp_analysis::report::MediaAnalysis { framing: Some(klipp_analysis::framing::Framing { samples: vec![sample], ..Default::default() }), ..Default::default() }
        };
        cover::draw(&self.seq, std::slice::from_ref(&self.seq), &self.pool, &look, &cover, dims.width, dims.height, &analysis).unwrap()
    }
}

/// The rows and columns of a still with no picture in them, at its edges.
fn black_edges(s: &still::Still) -> usize {
    let px = |x: u32, y: u32| { let k = ((y * s.width + x) * 4) as usize; s.rgba[k] as u32 + s.rgba[k + 1] as u32 + s.rgba[k + 2] as u32 };
    let dark = |mut line: Box<dyn Iterator<Item = u32>>| line.all(|v| v < 12);
    [
        dark(Box::new((0..s.width).map(|x| px(x, 0)))),
        dark(Box::new((0..s.width).map(|x| px(x, s.height - 1)))),
        dark(Box::new((0..s.height).map(|y| px(0, y)))),
        dark(Box::new((0..s.height).map(|y| px(s.width - 1, y)))),
    ].iter().filter(|d| **d).count()
}

fn red(s: &still::Still) -> usize {
    s.rgba.chunks_exact(4).filter(|p| p[0] > 180 && p[1] < 80 && p[2] < 80).count()
}

/// With no crop chosen a cover fills its size: no bars on 16:9 footage made
/// tall or square, on the middle, or on a face to one side when the file's
/// analysis saw one. A crop chosen is kept, the whole frame and its bars too,
/// and a clip already the cover's shape takes none.
#[test]
fn a_cover_fills_its_size_unless_a_crop_was_chosen() {
    let Some(wide) = filmed("wide.mp4", 1280, 720, 100) else { return };
    for size in ["vertical", "square"] {
        let middle = wide.drawn(size, None, None);
        assert_eq!(black_edges(&middle), 0, "{size}: bars with no crop chosen");
        assert_eq!(red(&middle), 0, "{size}: the middle of the frame, away from the patch");
        // A face over the patch: 100-220 of 1280 across.
        let on_face = wide.drawn(size, None, Some(0.08));
        assert_eq!(black_edges(&on_face), 0, "{size}: bars with a face");
        assert!(red(&on_face) > 1000, "{size}: the face is out of the crop: {} red", red(&on_face));
        let whole = wide.drawn(size, Some(CoverCrop::default()), Some(0.08));
        assert!(black_edges(&whole) >= 2, "{size}: the whole frame chosen, with its bars, is kept");
    }
    let chosen = wide.drawn("vertical", Some(CoverCrop { x: 0.5, y: 0.5, zoom: 2.0 }), Some(0.08));
    assert!(black_edges(&chosen) >= 2 && red(&chosen) == 0, "a crop chosen isn't moved to the face or filled");
    let same = wide.drawn("youtube", None, Some(0.08));
    assert_eq!(black_edges(&same), 0);
    let Some(tall) = filmed("tall.mp4", 720, 1280, 300) else { return };
    let shape = tall.drawn("vertical", None, None);
    assert_eq!(black_edges(&shape), 0, "already its shape");
    assert!(red(&shape) > 2000, "and all of it, the patch at the middle: {}", red(&shape));
}
