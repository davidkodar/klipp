//! A redaction follows the secret it hides (#2212): a screen recording
//! ffmpeg makes, an email sliding across, a password that only comes up
//! while the page scrolls, a token scrolling away off the top, read by the
//! screen-text analyser with its OCR models, and every frame of it
//! checked: wherever each secret's letters are drawn, the box auto_redact
//! holds then covers them. Each secret is drawn again alone, white on
//! black, to measure where its letters are frame by frame.
//!
//! Made from one place, a redaction leaked as the line moved, and a line
//! up only while it moved was never read at all. Skipped without ffmpeg
//! or the OCR models; with `KLIPP_TEST_REQUIRE_OCR=1` it fails instead.

use klipp_analysis::{screentext, AnalysisCtx, Analyser};
use std::io::Read;
use std::path::{Path, PathBuf};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: u32 = 25;
const SECS: f64 = 12.0;

/// Each secret: the text drawn, its x and y as ffmpeg expressions of
/// `t`, and what its masked form starts with.
const SECRETS: [(&str, &str, &str, &str); 3] = [
    // Slides right for six seconds, then stays.
    ("Email\\: alice@example.com", "100+500*min(t/6\\,1)", "120", "ali"),
    // Below the frame until the scroll starts at 6 s, then rises into it.
    ("Password\\: hunter2", "200", "if(lt(t\\,6)\\,800\\,800-110*(t-6))", "hun"),
    // Scrolls up and off the top by about 9 s.
    ("token\\: Zq81xR42kP", "200", "420-55*t", "Zq8"),
];

fn font() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter/Inter-Regular.ttf")
}

fn drawtext(text: &str, x: &str, y: &str, colour: &str) -> String {
    format!("drawtext=fontfile='{}':text='{text}':fontsize=40:fontcolor={colour}:x='{x}':y='{y}'", font().display())
}

fn ffmpeg(args: &[String]) -> bool {
    std::process::Command::new("ffmpeg").args(["-y", "-hide_banner", "-loglevel", "error"]).args(args).status().is_ok_and(|s| s.success())
}

/// The page's header, which what scrolls up passes under, as a page's
/// does: its height in pixels.
const HEADER: u32 = 80;

/// The screen recording: a light page, the three secrets, and the header
/// drawn over them.
fn screencast(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("screencast.mp4");
    let mut vf: Vec<String> = SECRETS.iter().map(|(text, x, y, _)| drawtext(text, x, y, "0x111111")).collect();
    vf.extend([format!("drawbox=x=0:y=0:w={W}:h={HEADER}:color=0x2b3a55:t=fill"), drawtext("Account Settings", "40", "20", "white")]);
    let args: Vec<String> = ["-f", "lavfi", "-i", &format!("color=c=0xf4f4f4:s={W}x{H}:r={FPS}:d={SECS}"), "-vf", &vf.join(","), "-c:v", "libx264", "-pix_fmt", "yuv420p"].iter().map(|s| s.to_string()).chain([out.display().to_string()]).collect();
    ffmpeg(&args).then_some(out)
}

/// Where secret `n`'s letters are in each frame, as fractions of the
/// frame (left, top, right, bottom): drawn alone, white on black, read
/// back frame by frame, under the header hidden. `None` where none of it
/// is on screen.
fn letters(n: usize) -> Vec<Option<[f32; 4]>> {
    let (text, x, y, _) = SECRETS[n];
    let vf = format!("{},format=gray", drawtext(text, x, y, "white"));
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c=black:s={W}x{H}:r={FPS}:d={SECS}"), "-vf", &vf, "-f", "rawvideo", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("ffmpeg draws the secret alone");
    let mut stdout = child.stdout.take().unwrap();
    let mut frame = vec![0u8; (W * H) as usize];
    let mut out = Vec::new();
    while stdout.read_exact(&mut frame).is_ok() {
        let (mut x0, mut y0, mut x1, mut y1) = (W, H, 0, 0);
        for (i, v) in frame.iter().enumerate() {
            if *v > 40 && i as u32 / W >= HEADER {
                let (x, y) = (i as u32 % W, i as u32 / W);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        out.push((x1 > x0).then(|| [x0 as f32 / W as f32, y0 as f32 / H as f32, x1 as f32 / W as f32, y1 as f32 / H as f32]));
    }
    child.wait().ok();
    out
}

#[test]
fn a_redaction_covers_each_secret_in_every_frame_wherever_it_moves() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_OCR").is_some_and(|v| v == "1");
    let skip = |why: &str| {
        assert!(!required, "required, but {why}");
        eprintln!("skipping: {why}");
    };
    if !AnalysisCtx::default().ffmpeg_available() {
        return skip("no ffmpeg");
    }
    let dir = std::env::temp_dir().join(format!("klipp-redaction-follows-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(path) = screencast(&dir) else { return skip("ffmpeg couldn't make the screencast") };
    let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
    item.duration = klipp_core::RationalTime::from_secs_f64(SECS, FPS);
    item.video_codec = "h264".into();
    item.resolution = klipp_core::Resolution { width: W, height: H };
    item.has_audio = false;
    if !screentext::ScreenTextAnalyser.wants(&item) {
        std::fs::remove_dir_all(&dir).ok();
        return skip("the OCR models or their runtime are not here");
    }
    let read: screentext::ScreenText = serde_json::from_value(screentext::ScreenTextAnalyser.run(&item, &AnalysisCtx::default()).expect("the screen is read")).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    for (n, (_, _, _, masked)) in SECRETS.iter().enumerate() {
        // auto_redact hides every finding: one secret found twice is
        // covered by either.
        let findings: Vec<klipp_document::ops::SensitiveText> = read
            .sensitive
            .iter()
            .filter(|s| s.masked.starts_with(masked))
            .map(|found| klipp_document::ops::SensitiveText {
                kind: found.kind.clone(),
                masked: found.masked.clone(),
                start_secs: found.start_secs,
                end_secs: found.end_secs,
                x: found.x,
                y: found.y,
                w: found.w,
                h: found.h,
                track: found.track.iter().map(|b| (b.secs, [b.x, b.y, b.w, b.h])).collect(),
            })
            .collect();
        assert!(!findings.is_empty(), "secret {n} ({masked}…) is found: {:?}", read.sensitive);
        let covers: Vec<(f64, f64, Vec<(f64, [f32; 4])>)> = findings.iter().map(|s| (s.start_secs, s.end_secs, s.cover_keys())).collect();
        let mut drawn = 0;
        for (f, rect) in letters(n).into_iter().enumerate() {
            let Some([x0, y0, x1, y1]) = rect else { continue };
            drawn += 1;
            let t = f as f64 / FPS as f64;
            let held: Vec<[f32; 4]> = covers.iter().filter(|(a, b, _)| t >= *a && t < *b).filter_map(|(_, _, keys)| keys.iter().rev().find(|k| k.0 <= t).map(|k| k.1)).collect();
            assert!(
                held.iter().any(|[bx, by, bw, bh]| *bx <= x0 && *by <= y0 && bx + bw >= x1 && by + bh >= y1),
                "secret {n} at {t:.2} s: its letters {:?} are not inside a box held then {held:?} (findings {:?})",
                [x0, y0, x1, y1],
                covers.iter().map(|(a, b, _)| (a, b)).collect::<Vec<_>>()
            );
        }
        assert!(drawn > 25, "secret {n} is on screen for a while: {drawn} frames");
    }
}
