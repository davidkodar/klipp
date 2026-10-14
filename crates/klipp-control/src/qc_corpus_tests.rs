//! A labelled corpus for the review checks (#1994): frames made in code,
//! each labelled by construction as a fault the check must raise or as
//! content meant as it is, run through the check's own judging, with a
//! precision and recall table for every rule.
//!
//! The colour and title checks hold thresholds that were reasoned, not
//! measured: the backlight gap, the tint a light can give a grey, the
//! smallest figure a title hides, the slack over a file's own floor, the
//! banding a file already holds. This is what to tune them against. To
//! tune one: change its constant, run
//! `cargo test -p klipp-control --lib qc_corpus -- --nocapture`, and
//! compare the tables. The asserts hold only the clear cases (no error on
//! content meant as it is, every fault well clear of its threshold
//! caught); the cases near a threshold are counted in the table, not
//! asserted, so the table shows where a threshold sits.
//!
//! Everything is generated: integer and fixed formulas, no randomness,
//! no files. Families, after the issues that set their thresholds:
//! silhouettes (#1590), sea against casts (#1590), tiny figures (#1590),
//! flat sources against bright scenes (#1896), banding (#1906).

use klipp_color::measure::{self, judge, measure_frame, Lab, LookTarget, SourceReading, Verdict};

const W: u32 = 160;
const H: u32 = 90;

/// A frame of `f(x, y)` in Lab, as 8-bit sRGB RGBA.
fn frame(f: impl Fn(u32, u32) -> Lab) -> Vec<u8> {
    (0..W * H)
        .flat_map(|i| {
            let [r, g, b] = measure::lab_to_srgb8(f(i % W, i / W));
            [r, g, b, 255]
        })
        .collect()
}

/// A gentle, fixed texture: a picture is never one flat colour.
fn grain(x: u32, y: u32) -> f32 {
    ((x * 7 + y * 13 + (x ^ y) * 3) % 5) as f32 - 2.0
}

/// What a case is, by construction.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Truth {
    /// A fault the check must raise.
    Fault,
    /// Content meant as it is: the check must not call it an error.
    Meant,
    /// Near a threshold, where either answer can be argued: counted, not
    /// scored.
    Grey,
}

/// One case's outcome.
struct Outcome {
    family: &'static str,
    case: String,
    truth: Truth,
    verdict: Verdict,
    /// For a fault, whether it sits well clear of the threshold the rule
    /// uses: such a fault must be caught.
    clear: bool,
}

/// The rule's verdict in a judged frame; `Pass` when the rule did not
/// answer.
fn verdict_of(m: &measure::FrameMeasure, t: &LookTarget, rule: &str) -> Verdict {
    judge(m, t).into_iter().find(|f| f.rule == rule).map_or(Verdict::Pass, |f| f.verdict)
}

// ─── Families ──────────────────────────────────────────────────────────

/// A face lit from behind against a bright sky is meant; a dark face in a
/// dark, flat-lit frame is a fault. Rule: skin_exposure.
fn silhouettes(out: &mut Vec<Outcome>) {
    let face = [0.4f32, 0.25, 0.2, 0.45];
    let inside = |x: u32, y: u32| {
        let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
        fx >= face[0] && fx < face[0] + face[2] && fy >= face[1] && fy < face[1] + face[3]
    };
    let t = LookTarget::unbriefed();
    for face_l in [18.0f32, 22.0, 26.0, 30.0, 34.0, 38.0] {
        for sky in [45.0f32, 55.0, 65.0, 75.0, 85.0, 95.0] {
            let rgba = frame(|x, y| {
                if inside(x, y) {
                    Lab { l: face_l + grain(x, y), a: 14.0, b: 17.0 }
                } else if y < H * 2 / 3 {
                    Lab { l: sky + grain(x, y).min(0.0), a: -2.0, b: -12.0 }
                } else {
                    Lab { l: face_l + 5.0 + grain(x, y), a: 2.0, b: 8.0 }
                }
            });
            let m = measure_frame(&rgba, W, H, &[face]);
            let gap = sky - face_l;
            let truth = if gap >= 50.0 { Truth::Meant } else if gap <= 25.0 { Truth::Fault } else { Truth::Grey };
            out.push(Outcome { family: "silhouette (skin_exposure)", case: format!("face L* {face_l}, sky L* {sky}"), truth, verdict: verdict_of(&m, &t, "skin_exposure"), clear: gap <= 20.0 });
        }
    }
}

/// With nothing neutral in the frame, a coloured subject is meant; grey
/// surfaces under a coloured light are a fault. Rule: cast.
fn casts(out: &mut Vec<Outcome>) {
    let t = LookTarget::unbriefed();
    // Subjects: sea, foliage, painted walls. Nothing grey.
    for a in [-20.0f32, -14.0, -10.0, -6.0, 6.0, 10.0, 14.0, 20.0] {
        for b in [-10.0f32, 0.0, 10.0] {
            let rgba = frame(|x, y| Lab { l: 60.0 + grain(x, y) * 2.0, a: a + grain(y, x) * 0.5, b });
            let m = measure_frame(&rgba, W, H, &[]);
            // A subject no further toward green or magenta than common
            // lamps put a grey (a* within 6) is, alone in the frame, the
            // same picture as a grey under such a lamp: undecidable from
            // the frame, so not scored.
            let truth = if a.abs() <= 6.0 { Truth::Grey } else { Truth::Meant };
            out.push(Outcome { family: "sea and walls (cast)", case: format!("subject a* {a}, b* {b}"), truth, verdict: verdict_of(&m, &t, "cast"), clear: false });
        }
    }
    // Grey surfaces under lamps: warm, cool, green, magenta, beside a red
    // subject.
    for (name, ca, cb) in [("warm", 2.0f32, 12.0f32), ("warmer", 3.0, 18.0), ("cool", -1.0, -12.0), ("green", -6.0, 2.0), ("greener", -10.0, 2.0), ("green tube", -14.0, 4.0), ("magenta", 8.0, 0.0), ("magenta LED", 12.0, -2.0)] {
        let rgba = frame(|x, y| if x < W / 2 { Lab { l: 60.0 + grain(x, y), a: ca, b: cb } } else { Lab { l: 50.0 + grain(x, y), a: 40.0, b: 30.0 } });
        let m = measure_frame(&rgba, W, H, &[]);
        let off = (ca * ca + cb * cb).sqrt();
        out.push(Outcome { family: "lamp casts (cast)", case: format!("{name} light, a* {ca} b* {cb}"), truth: Truth::Fault, verdict: verdict_of(&m, &t, "cast"), clear: off >= 11.0 && ca.abs() <= 6.0 });
    }
}

/// A figure a few pixels tall under a title is part of the shot; a person
/// a fifth of the frame tall is hidden by it. Rule: title_over_person.
fn figures(out: &mut Vec<Outcome>) {
    use klipp_core::{Fps, RationalTime};
    use klipp_document::ops::{SpokenWord, Transcripts};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
    struct Figure(klipp_core::MediaId, f32);
    impl Transcripts for Figure {
        fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            None
        }
        fn people(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
            let h = self.1;
            if item.id == self.0 { vec![(41.0, [0.5 - h * 0.2, 0.5 - h / 2.0, h * 0.4, h])] } else { vec![] }
        }
    }
    let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-1994/beach.mov".into(), None);
    item.duration = tc(60.0);
    item.video_codec = "h264".into();
    let beach = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips.push(Clip::media(beach, tc(0.0), tc(10.0), tc(40.0), tc(50.0)));
    let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "GOLDEN".into(), font_size: 60.0, ..Default::default() });
    td.objects[0].transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.5 };
    let mut text = Track::new("T", TrackKind::Text);
    text.clips.push(Clip { id: klipp_core::ClipId::new(), kind: ClipKind::Title(td), source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(1.0), tc(4.0)) });
    seq.tracks = vec![text, v];
    for h in [0.02f32, 0.04, 0.06, 0.08, 0.1, 0.12, 0.15, 0.2, 0.3, 0.4] {
        let r = klipp_document::qc::check(&seq, &pool, &Figure(beach, h));
        // Raised or not: the rule has one level, counted as the error one.
        let raised = r.findings.iter().any(|f| f.rule == "title_over_person");
        let truth = if h <= 0.06 { Truth::Meant } else if h >= 0.15 { Truth::Fault } else { Truth::Grey };
        out.push(Outcome { family: "figures (title_over_person)", case: format!("figure {:.0}% of the frame tall", h * 100.0), truth, verdict: if raised { Verdict::Fail } else { Verdict::Pass }, clear: h >= 0.2 });
    }
}

/// A frame and the same frame lifted by `lift` L* in its shadows: what a
/// grade's lift does.
fn lifted(src: &impl Fn(u32, u32) -> Lab, lift: f32) -> impl Fn(u32, u32) -> Lab + '_ {
    move |x, y| {
        let c = src(x, y);
        Lab { l: c.l + lift * (1.0 - c.l / 100.0), ..c }
    }
}

/// A bright, saturated scene whose floor is its own is meant; a flat or
/// hazy source with a high floor, and a grade lifting a bright scene
/// further, are faults. Rule: black_point.
fn floors(out: &mut Vec<Outcome>) {
    let t = LookTarget::unbriefed();
    let judge_pair = |src: &dyn Fn(u32, u32) -> Lab, lift: f32| {
        let source = measure_frame(&frame(src), W, H, &[]);
        let mut graded = measure_frame(&frame(lifted(&src, lift)), W, H, &[]);
        graded.source = Some(SourceReading::of(&source));
        verdict_of(&graded, &t, "black_point")
    };
    // Sunlit water and snow: bright, their darkest well above black.
    for floor in [30.0f32, 36.0, 42.0] {
        let sea = move |x: u32, y: u32| Lab { l: floor + (x as f32 / W as f32) * (96.0 - floor) + grain(x, y), a: -20.0, b: -15.0 + (y % 7) as f32 };
        out.push(Outcome { family: "bright scenes (black_point)", case: format!("sea, floor L* {floor}, not lifted"), truth: Truth::Meant, verdict: judge_pair(&sea, 0.0), clear: false });
        for lift in [8.0f32, 14.0] {
            out.push(Outcome { family: "bright scenes (black_point)", case: format!("sea, floor L* {floor}, lifted {lift}"), truth: Truth::Fault, verdict: judge_pair(&sea, lift), clear: lift >= 14.0 });
        }
    }
    // Flat and hazy sources: a high floor, little contrast or colour.
    for (floor, chroma) in [(22.0f32, 4.0f32), (28.0, 6.0), (34.0, 8.0)] {
        let flat = move |x: u32, y: u32| Lab { l: floor + (x as f32 / W as f32) * 40.0 + grain(x, y), a: chroma * 0.3, b: chroma };
        out.push(Outcome { family: "flat sources (black_point)", case: format!("flat, floor L* {floor}, chroma {chroma}, not lifted"), truth: Truth::Fault, verdict: judge_pair(&flat, 0.0), clear: true });
    }
}

/// A banded 8-bit ramp is a fault; the same ramp dithered, and a screen
/// recording's panels the file itself holds, are meant. Rule: banding.
///
/// Banding is a matter of how wide the steps are on screen, so these
/// frames are as wide as a quarter of HD, not the small frames above.
fn banding(out: &mut Vec<Outcome>) {
    const BW: u32 = 480;
    const BH: u32 = 96;
    let camera = LookTarget::unbriefed();
    let screen = LookTarget::preset("screencast").expect("the screencast preset");
    let bayer = [[0u32, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let ramp = |span: u32, dither: bool| -> Vec<u8> {
        (0..BW * BH).flat_map(|i| {
            let (x, y) = (i % BW, i / BW);
            // 16ths of a code value along the ramp, rounded to whole codes,
            // with an ordered dither added before the rounding when asked.
            let sixteenths = 60 * 16 + x * span * 16 / BW;
            let v = if dither { (sixteenths + bayer[(y % 4) as usize][(x % 4) as usize]) / 16 } else { (sixteenths + 8) / 16 };
            let v = v.min(255) as u8;
            [v, v, v, 255]
        }).collect()
    };
    for span in [6u32, 8, 12, 20, 40] {
        // Each code's plateau is this wide, in pixels.
        let plateau = BW / span;
        let banded = measure_frame(&ramp(span, false), BW, BH, &[]);
        out.push(Outcome { family: "gradients (banding)", case: format!("8-bit ramp over {span} codes, steps {plateau} px wide"), truth: Truth::Fault, verdict: verdict_of(&banded, &camera, "banding"), clear: plateau >= 48 });
        let dithered = measure_frame(&ramp(span, true), BW, BH, &[]);
        out.push(Outcome { family: "gradients (banding)", case: format!("dithered ramp over {span} codes"), truth: Truth::Meant, verdict: verdict_of(&dithered, &camera, "banding"), clear: false });
    }
    // An app's flat panels and a graph's lines, read with the recording's
    // own file holding the same.
    for steps in [4u32, 8, 16] {
        let ui = (0..BW * BH).flat_map(|i| {
            let (x, y) = (i % BW, i / BW);
            let v = (24 + (x * steps / BW) * 6 + if y % 30 == 0 { 40 } else { 0 }).min(255) as u8;
            [v, v, v.saturating_add(4), 255]
        }).collect::<Vec<u8>>();
        let mut m = measure_frame(&ui, BW, BH, &[]);
        m.source = Some(SourceReading { range_stated: true, ..SourceReading::of(&m) });
        out.push(Outcome { family: "screen panels (banding)", case: format!("UI with {steps} panels, file the same"), truth: Truth::Meant, verdict: verdict_of(&m, &screen, "banding"), clear: false });
    }
}

// ─── The table ─────────────────────────────────────────────────────────

#[derive(Default)]
struct Tally {
    faults: usize,
    caught: usize,
    caught_as_error: usize,
    meant: usize,
    meant_warned: usize,
    meant_errored: usize,
    grey: usize,
}

fn table(outcomes: &[Outcome]) -> String {
    let mut families: Vec<&str> = Vec::new();
    for o in outcomes {
        if !families.contains(&o.family) {
            families.push(o.family);
        }
    }
    let pct = |a: usize, b: usize| if b == 0 { "   —".to_string() } else { format!("{:>3.0}%", a as f64 / b as f64 * 100.0) };
    let mut s = String::from("rule (family)                       faults caught  error  meant  warned  errored  grey   recall  precision(any)  precision(error)\n");
    for fam in families {
        let mut t = Tally::default();
        for o in outcomes.iter().filter(|o| o.family == fam) {
            match o.truth {
                Truth::Fault => {
                    t.faults += 1;
                    t.caught += usize::from(o.verdict != Verdict::Pass);
                    t.caught_as_error += usize::from(o.verdict == Verdict::Fail);
                }
                Truth::Meant => {
                    t.meant += 1;
                    t.meant_warned += usize::from(o.verdict == Verdict::Warn);
                    t.meant_errored += usize::from(o.verdict == Verdict::Fail);
                }
                Truth::Grey => t.grey += 1,
            }
        }
        let flagged_any = t.caught + t.meant_warned + t.meant_errored;
        let flagged_error = t.caught_as_error + t.meant_errored;
        s.push_str(&format!(
            "{fam:<35} {:>6} {:>6} {:>6} {:>6} {:>7} {:>8} {:>5}   {}     {}           {}\n",
            t.faults, t.caught, t.caught_as_error, t.meant, t.meant_warned, t.meant_errored, t.grey,
            pct(t.caught, t.faults), pct(t.caught, flagged_any), pct(t.caught_as_error, flagged_error),
        ));
    }
    s
}

#[test]
fn qc_corpus_precision_and_recall() {
    let mut out = Vec::new();
    silhouettes(&mut out);
    casts(&mut out);
    figures(&mut out);
    floors(&mut out);
    banding(&mut out);
    println!("{}", table(&out));
    let wrong: Vec<String> = out.iter().filter(|o| match o.truth {
        Truth::Fault => o.verdict == Verdict::Pass,
        Truth::Meant => o.verdict != Verdict::Pass,
        Truth::Grey => false,
    }).map(|o| format!("{:?} {}: {} → {:?}", o.truth, o.family, o.case, o.verdict)).collect();
    println!("Not answered as labelled ({}):\n  {}", wrong.len(), wrong.join("\n  "));
    // The floors: nothing meant as it is is called an error, and every
    // fault well clear of its threshold is caught — apart from the wrong
    // answers known and filed, listed here so a fix must take them off
    // and a new one fails.
    const KNOWN: &[&str] = &[
        // #1996: an ordered dither reads as banding.
        "gradients (banding): dithered ramp over 6 codes",
        "gradients (banding): dithered ramp over 8 codes",
    ];
    let named = |o: &&Outcome| format!("{}: {}", o.family, o.case);
    let mut wrong: Vec<String> = out.iter().filter(|o| (o.truth == Truth::Meant && o.verdict == Verdict::Fail) || (o.truth == Truth::Fault && o.clear && o.verdict == Verdict::Pass)).map(|o| named(&o)).collect();
    wrong.sort();
    let mut known: Vec<String> = KNOWN.iter().map(|k| k.to_string()).collect();
    known.sort();
    assert_eq!(wrong, known, "an error on content meant as it is, or a clear fault missed, beyond the known ones (or a known one fixed: take it off the list)");
    assert!(out.len() >= 60, "the corpus shrank: {} cases", out.len());
}
