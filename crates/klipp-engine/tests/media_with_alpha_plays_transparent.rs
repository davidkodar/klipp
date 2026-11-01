//! #3264: a clip whose file carries alpha (ProRes 4444, FFV1) plays with
//! its transparent parts transparent: over a red track below, red where
//! its alpha is 0, its own picture where it is 1. A bounced track lands
//! on the timeline as such a clip, over the tracks below it.

mod common;

const W: u32 = 320;
const H: u32 = 180;

/// testsrc2 with an alpha ramp from 0 at the top row to 1 at the bottom,
/// encoded as `codec` in `pix_fmt`.
fn source(name: &str, codec: &[&str], pix_fmt: &str, ext: &str) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-alpha-{name}-{}.{ext}", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=s={W}x{H}:r=24:d=1,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*Y/{}'", H - 1))
        .args(codec)
        .args(["-pix_fmt", pix_fmt])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

#[test]
fn a_clip_with_alpha_shows_the_track_below_where_its_alpha_is_zero() {
    for (name, codec, pix_fmt, ext) in [
        ("prores4444", &["-c:v", "prores_ks", "-profile:v", "4"][..], "yuva444p10le", "mov"),
        ("ffv1", &["-c:v", "ffv1", "-level", "3"][..], "gbrap16le", "mkv"),
    ] {
        let Some(src) = source(name, codec, pix_fmt, ext) else {
            eprintln!("skipping {name}: no ffmpeg");
            continue;
        };
        let p = common::Parked::start_ungraded(src, W, H);
        let opaque = p.frame();
        // A solid red track below the clip.
        let mut seq = (*p.sequence.snapshot()).clone();
        let mut red = klipp_timeline::Track::new("V0", klipp_timeline::TrackKind::Video);
        let span = (seq.tracks[0].clips[0].timeline_in, seq.tracks[0].clips[0].timeline_out);
        red.clips = vec![klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 0, 0, 255] }), ..klipp_timeline::Clip::gap(span.0, span.1) }];
        seq.tracks.push(red);
        p.sequence.replace(seq);
        let g = p.generation();
        p.send(klipp_core::AppCommand::SeekTo(common::frame_tc(5, p.fps)));
        p.settle_after(g, std::time::Duration::from_secs(10));
        let over_red = p.frame();
        let px = |f: &[u8], y: u32| {
            let row = &f[(y * W * 4) as usize..((y + 1) * W * 4) as usize];
            let n = W as f32;
            (0..3).map(|c| row.chunks_exact(4).map(|p| f32::from(p[c])).sum::<f32>() / n).collect::<Vec<_>>()
        };
        let (top, bottom) = (px(&over_red, 0), px(&over_red, H - 1));
        eprintln!("MEASURE {name} over red: top row (alpha 0) {top:?}, bottom row (alpha 1) {bottom:?}, bottom row alone {:?}", px(&opaque, H - 1));
        assert!(top[0] > 250.0 && top[1] < 3.0 && top[2] < 3.0, "{name}: red shows through the transparent top row: {top:?}");
        let alone = px(&opaque, H - 1);
        assert!(bottom.iter().zip(&alone).all(|(a, b)| (a - b).abs() < 3.0), "{name}: the opaque bottom row is the clip's own picture: {bottom:?} vs {alone:?}");
    }
}
