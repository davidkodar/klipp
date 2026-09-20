//! Measure faces and colour in stills or video frames, without the app:
//! the same code the check and `frame_stats` run, over a folder of test
//! material.
//!
//!   cargo run --release -p klipp-ml-ort --features ort --example face_measure -- [--look NAME] [--secs N] [--match] FILE...
//!
//! `--match` adds the matching table: how far every file is from every
//! other by `measure::match_distance`, which is what a colourist is
//! judging when they ask whether two shots cut together.
//!
//! Prints one line of frame readings and one per face, then the
//! findings that are not a pass. The face model is the one in the app's
//! model cache (`yunet_face_2023mar`).

use klipp_color::measure::{self, LookTarget, Verdict};

fn probe(path: &str) -> Option<(u32, u32)> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x", path])
        .output()
        .ok()?;
    // "1920x1080" for a still, "1920x1080x" for a video: take the first
    // two numbers on the line, whatever follows them.
    let s = String::from_utf8_lossy(&out.stdout);
    let mut nums = s.trim().lines().next()?.split('x').filter_map(|p| p.trim().parse::<u32>().ok());
    Some((nums.next()?, nums.next()?))
}

fn frame_rgba(path: &str, secs: f64, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"]);
    if secs > 0.0 {
        cmd.args(["-ss", &format!("{secs:.3}")]);
    }
    cmd.args(["-i", path, "-frames:v", "1", "-vf", &format!("scale={width}:{height}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
    let out = cmd.output().ok()?;
    let want = (width as usize) * (height as usize) * 4;
    (out.stdout.len() >= want).then(|| out.stdout[..want].to_vec())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut look_name = "broadcast_neutral".to_string();
    let mut secs = 0.0f64;
    let mut matching = false;
    let mut false_colour_dir: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--look" => { look_name = args.remove(i + 1); args.remove(i); }
            "--secs" => { secs = args.remove(i + 1).parse().unwrap_or(0.0); args.remove(i); }
            "--match" => { matching = true; args.remove(i); }
            "--false-colour" => { false_colour_dir = Some(args.remove(i + 1)); args.remove(i); }
            _ => i += 1,
        }
    }
    let look = LookTarget::preset(&look_name).unwrap_or_else(|| panic!("no look {look_name}; try {}", LookTarget::PRESETS.join(", ")));
    let model = klipp_analysis_face_model().expect("the face model is not in the model cache: run an analysis with the detector once, or download yunet_face_2023mar");
    let mut detector = klipp_ml_ort::yunet::FaceDetector::open(&model).expect("the face model loads");
    println!("Look \"{look_name}\". Skin band {:.0}-{:.0}°.\n", measure::SKIN_HUE_LOW, measure::SKIN_HUE_HIGH);
    let mut measured: Vec<(String, measure::FrameMeasure)> = Vec::new();
    for path in &args {
        let Some((w0, h0)) = probe(path) else { println!("{path}: not a picture"); continue };
        let w = w0.min(1280);
        let h = ((h0 as f64 * w as f64 / w0 as f64 / 2.0).round() * 2.0) as u32;
        let Some(rgba) = frame_rgba(path, secs, w, h) else { println!("{path}: no frame"); continue };
        let faces = match detector.detect(&rgba, w, h) {
            Ok(f) => f,
            Err(e) => { println!("   detector: {e}"); Vec::new() }
        };
        if std::env::var_os("FACE_DEBUG").is_some() {
            println!("   detections: {:?}", faces.iter().map(|f| (f.score * 100.0).round() as i32).take(8).collect::<Vec<_>>());
        }
        let regions: Vec<[f32; 4]> = faces
            .iter()
            .filter(|f| f.score >= 0.7)
            .map(|f| measure::skin_band(&f.landmarks).unwrap_or([f.x + f.w * 0.12, f.y + f.h * 0.15, f.w * 0.76, f.h * 0.72]))
            .collect();
        let m = measure::measure_frame(&rgba, w, h, &regions);
        let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let cast = if m.cast() < 3.0 { "neutral".to_string() } else { format!("{:.1} toward {}", m.cast(), measure::cast_name(m.cast_a, m.cast_b)) };
        println!("{name}  {w}x{h}");
        println!("   L* {:.0}/{:.0}/{:.0}  contrast {:.0}  chroma {:.0}  cast {cast}  clip {:.1}%/{:.1}%", m.l_p1, m.l_p50, m.l_p99, m.contrast_range, m.chroma_mean, m.clip_hi_pct, m.clip_lo_pct);
        println!("   noise {:.2} L*  banding {:.0}%  gamut-clipped {:.1}%{}", m.noise_l, m.banding_pct, m.gamut_clip_pct, if measure::looks_like_log(&m) { "  LOOKS LIKE LOG (no input transform)" } else { "" });
        for (n, f) in m.faces.iter().enumerate() {
            if f.readable() {
                let off = if f.hue_off_band > 0.0 { format!("{:.0}° OFF the band", f.hue_off_band) } else { "on the band".to_string() };
                println!("   face {}: hue {:5.1}° (wb {:5.1}°) {off:>16}  chroma {:4.1}  L* {:4.1}  {}/{}²  spread {:.0}°{}", n + 1, f.hue, f.hue_wb, f.chroma, f.median.l, f.patches, f.grid, f.hue_iqr, if f.mixed_light { "  two lights" } else { "" });
            } else {
                println!("   face {}: too little skin to read ({} of {}²)", n + 1, f.patches, f.grid);
            }
        }
        if m.faces.is_empty() {
            match m.skin_all {
                Some(s) => println!("   no face found; skin-coloured pixels: hue {:.1}° ({:.0}% of the frame)", s.hue, s.pct),
                None => println!("   no face found"),
            }
        }
        for f in measure::judge(&m, &look).into_iter().filter(|f| f.verdict != Verdict::Pass) {
            println!("   [{}] {} — {}", if f.verdict == Verdict::Fail { "FAIL" } else { "warn" }, f.what, f.fix);
        }
        let step = measure::primaries_toward(&m, &look);
        println!("   toward the look: exposure {:+.2} lift {:+.3} gain {:+.3} temp {:+.3} tint {:+.3} sat {:+.2}\n", step.exposure, step.lift, step.gain, step.temperature, step.tint, step.saturation);
        // The picture repainted by exposure, if asked for: the fastest
        // way to see whether the skin band lands where a face is.
        if let Some(dir) = &false_colour_dir {
            let mut look = look.clone();
            // The look's own skin level, or this face's, so the band is
            // where the picture says a face should be.
            if look.skin_l.is_none() {
                look.skin_l = m.faces.iter().find(|f| f.readable()).map(|f| f.median.l);
            }
            let painted = klipp_color::false_colour::repaint(&rgba, w, h, &look);
            let out = std::path::Path::new(dir).join(format!("{}-false.png", name.trim_end_matches(".jpg").trim_end_matches(".mp4")));
            let _ = std::fs::create_dir_all(dir);
            match write_png(&out, &painted, w, h) {
                Ok(()) => println!("   false colour → {}", out.display()),
                Err(e) => println!("   false colour failed: {e}"),
            }
        }
        measured.push((name, m));
    }
    if matching && measured.len() > 1 {
        // What a colourist means by "do these cut together": not how
        // good each is, but how far apart they are.
        println!("Matching — ΔE between each pair. Under {:.0} the cut is invisible; over {:.0} the viewer sees the join; over {:.0} they are different lighting setups.\n", measure::MATCH_GOOD, measure::MATCH_WARN, measure::SCENE_SPLIT);
        // Numbered columns: the filenames in a test set differ in
        // their last few characters, so a truncated name names nothing.
        let width = measured.iter().map(|(n, _)| n.len()).max().unwrap_or(10).min(34);
        print!("{:width$}   ", "", width = width + 4);
        for i in 0..measured.len() {
            print!("{:>7}", format!("[{}]", i + 1));
        }
        println!();
        for (i, (an, a)) in measured.iter().enumerate() {
            print!("[{:>2}] {:width$}  ", i + 1, &an[..an.len().min(width)], width = width);
            for (_, b) in &measured {
                print!("{:7.1}", measure::match_distance(a, b));
            }
            println!();
        }
    }
}

/// An RGBA frame as a PNG.
fn write_png(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().map_err(|e| e.to_string())?.write_image_data(rgba).map_err(|e| e.to_string())
}

/// The face model in the app's cache.
fn klipp_analysis_face_model() -> Option<std::path::PathBuf> {
    let manifest = klipp_ml::manifest::load_effective().ok()?;
    let cache = klipp_ml::cache::ModelCache::open().ok()?;
    let entry = manifest.get("yunet_face_2023mar")?;
    let p = cache.path_for(entry);
    p.is_file().then_some(p)
}
