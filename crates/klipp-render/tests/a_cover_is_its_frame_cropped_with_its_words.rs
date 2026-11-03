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
    let mut cover = Cover { id: CoverId::new(), name: String::new(), at_secs: 1.0, size: "youtube".into(), crop: CoverCrop { x: 6.5 / 7.0, y: 0.5, zoom: 7.0 }, text: None, with_titles: false };
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
    let worded = cover::draw(&seq, std::slice::from_ref(&seq), &pool, &look, &cover, w, h).unwrap();
    let bare = cover::draw(&seq, std::slice::from_ref(&seq), &pool, &look, &Cover { text: None, ..cover.clone() }, w, h).unwrap();
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
