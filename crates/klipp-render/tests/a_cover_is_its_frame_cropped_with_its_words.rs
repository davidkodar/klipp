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

    // Read on a phone: white words over the blue bar read, over the
    // white bar they don't; at 120 points they are big enough.
    let look = cover::Look { lut: None, working: WorkingColorspace::Rec709Linear, grade_encoding: GradeEncoding::Bt1886 };
    let pool = klipp_media::MediaPool::new();
    let rules = |c: &Cover| {
        let under = cover::under_words(&seq, std::slice::from_ref(&seq), &pool, &look, c, klipp_document::qc::PHONE_FEED_W).unwrap().expect("words to read");
        klipp_document::qc::cover_readability(c, Some((&under.rgba, under.width as usize, under.height as usize))).iter().map(|f| f.rule).collect::<Vec<_>>()
    };
    assert!(rules(&cover).is_empty(), "{:?}", rules(&cover));
    let mut on_white = cover.clone();
    on_white.crop.x = 0.5 / 7.0;
    assert_eq!(rules(&on_white), vec!["cover_hard_to_read"]);
}
