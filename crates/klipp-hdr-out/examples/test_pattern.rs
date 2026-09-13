//! Publish a known HDR test pattern, for checking the output path without
//! the rest of Klipp.
//!
//! ```text
//! cargo run --release -p klipp-hdr-out --example test_pattern -- [seconds]
//! ```
//!
//! Then, in another shell, `cargo run --release -p klipp-hdr-out`.
//!
//! The pattern is stepped in *absolute light* rather than in code values,
//! because that is what an HDR reference output is judged on: each band is
//! a level a colourist knows by heart, so what is on the panel can be
//! checked against what it should be without a probe.
//!
//! | Band | cd/m² | What it is |
//! |---|---|---|
//! | 0 | 5 | deep shadow |
//! | 1 | 26 | an 18% grey card (BT.2408) |
//! | 2 | 203 | diffuse white (BT.2408 reference white) |
//! | 3 | 400 | a bright specular |
//! | 4 | 1000 | the usual HDR10 mastering peak |
//! | 5 | 4000 | beyond most panels — should roll off or clip, not wrap |
//!
//! On an SDR path every band from 2 up is white. That is the point: if the
//! output is working, they are visibly different.

/// BT.2408 reference white, cd/m² — what display light 1.0 means.
const REF_WHITE_NITS: f32 = 203.0;

const BANDS: [(f32, &str); 6] = [
    (5.0, "deep shadow"),
    (26.0, "18% grey card"),
    (203.0, "diffuse white"),
    (400.0, "specular"),
    (1000.0, "HDR10 peak"),
    (4000.0, "beyond most panels"),
];

fn main() {
    let secs: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let (w, h) = (1280u32, 720u32);
    let path = klipp_hdr_out::default_path();
    let mut writer = match klipp_hdr_out::FrameWriter::create(&path, w, h) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("could not create {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    writer.set_peak_nits(1000.0);

    // Vertical bands, plus a horizontal ramp through the whole range along
    // the bottom so a roll-off is visible as a gradient rather than a step.
    let mut frame = vec![0.0f32; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let nits = if y > h * 3 / 4 {
                // The ramp: 0 to 4000 cd/m² across the frame.
                (x as f32 / (w - 1) as f32) * 4000.0
            } else {
                let band = (x as usize * BANDS.len() / w as usize).min(BANDS.len() - 1);
                BANDS[band].0
            };
            let light = nits / REF_WHITE_NITS;
            frame[i] = light;
            frame[i + 1] = light;
            frame[i + 2] = light;
            frame[i + 3] = 1.0;
        }
    }

    println!("publishing to {} — {w}x{h}", path.display());
    for (nits, what) in BANDS {
        println!("  {nits:>6.0} cd/m²  (display light {:.3})  {what}", nits / REF_WHITE_NITS);
    }
    println!("for {secs} s; run `klipp-hdr-out` to show it");

    let start = std::time::Instant::now();
    let mut published = 0u64;
    while start.elapsed().as_secs_f64() < secs {
        // Republished every frame even though it is static, so the
        // presenter's "has a new frame arrived" path is the one being
        // exercised rather than its first-frame path.
        if !writer.publish(&frame) {
            eprintln!("publish refused");
            break;
        }
        published += 1;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    println!("published {published} frames");
}
