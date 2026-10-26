//! Pictures and numbers for the colour work, so it can be looked at
//! rather than taken on trust.
//!
//! `cargo run -p klipp-compose --example colour_proof [out_dir]`
//!
//! Renders an HDR test chart — a highlight several times SDR white on a
//! dark ground, colour wedges at three exposures, a grey ramp — through
//! the effects and grade tools that act on brightness or colour, and
//! writes each result twice:
//!
//! * `*-sdr.png`: what an SDR monitor shows, the picture clipped at white,
//!   which is what the viewer shows today.
//! * `*-pq.png`: the same picture PQ-encoded (SMPTE ST 2084, 1000-nit
//!   master, 1.0 = 203 cd/m²) and then displayed as if it were sRGB — the
//!   standard way to look at an HDR master on an SDR screen. Highlights
//!   above white are visible here and flat in the SDR view.
//!
//! It also prints a table of probe values: linear RGB, the light in
//! cd/m², and the 10-bit PQ code a delivery would carry. Those numbers
//! are the check; the pictures show what they mean.

use std::path::{Path, PathBuf};

use klipp_effects::BuiltinEffect;

const W: u32 = 320;
const H: u32 = 180;

/// The chart: a bright disc on a dark ground (a halo shows here), colour
/// wedges at 1x, 4x and 12x SDR white, and a grey ramp along the bottom.
fn chart() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    let hue = |i: usize| match i % 6 {
        0 => [1.0, 0.25, 0.15],
        1 => [0.95, 0.7, 0.2],
        2 => [0.3, 0.9, 0.35],
        3 => [0.2, 0.8, 0.9],
        4 => [0.3, 0.35, 1.0],
        _ => [0.9, 0.4, 0.85],
    };
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let (fx, fy) = (x as f32, y as f32);
            let mut c = [0.02, 0.02, 0.025];
            // Colour wedges, three bands of exposures.
            if fy < H as f32 * 0.66 {
                let band = (fy / (H as f32 * 0.22)) as usize;
                let exposure = [1.0f32, 4.0, 12.0][band.min(2)];
                let wedge = (fx / (W as f32 / 6.0)) as usize;
                if fx > W as f32 * 0.02 {
                    c = hue(wedge).map(|v| v * exposure);
                }
            } else {
                // Grey ramp, 0 to 2x white.
                c = [fx / W as f32 * 2.0; 3];
            }
            // A 12x highlight disc, where a halo or a crush shows.
            let (dx, dy) = (fx - W as f32 * 0.5, fy - H as f32 * 0.28);
            if (dx * dx + dy * dy).sqrt() < H as f32 * 0.12 {
                c = [12.0, 11.0, 9.5];
            }
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    }
    px
}

fn srgb8(v: f32) -> u8 {
    let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
    (e.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The picture as an HDR delivery would carry it, shown on an SDR screen:
/// PQ-encoded light, the code値 displayed directly.
fn pq8(v: f32) -> u8 {
    let nits = (v * klipp_color::hdr::HDR_REFERENCE_WHITE_NITS).clamp(0.0, 1000.0);
    (klipp_color::hdr::pq_encode_nits(nits).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn write_png(path: &PathBuf, px: &[f32], map: impl Fn(f32) -> u8) {
    let mut out = Vec::with_capacity((W * H * 3) as usize);
    for c in px.chunks_exact(4) {
        out.extend_from_slice(&[map(c[0]), map(c[1]), map(c[2])]);
    }
    write_rgb8(path, &out);
}

fn write_rgb8(path: &PathBuf, out: &[u8]) {
    // PNG through a tiny encoder of our own would be another dependency;
    // ffmpeg is already required by the test suite and reads raw frames.
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{W}x{H}"), "-i", "-"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(out)?;
            child.wait()
        });
    if let Err(e) = status {
        eprintln!("could not write {}: {e}", path.display());
    }
}

/// Probe points: inside the highlight disc, in its halo, on a 4x wedge,
/// and on the grey ramp above white.
fn probes() -> [(&'static str, u32, u32); 4] {
    [
        ("highlight", W / 2, (H as f32 * 0.28) as u32),
        ("halo", W / 2 + (H as f32 * 0.16) as u32, (H as f32 * 0.28) as u32),
        ("4x red wedge", W / 12, (H as f32 * 0.3) as u32),
        ("ramp, 1.6x white", (W as f32 * 0.8) as u32, (H as f32 * 0.85) as u32),
    ]
}

/// A saturation ramp per BT.2020 hue: six bands, each running from
/// neutral at the left to the primary or secondary itself at the right.
/// Values are BT.2020 linear light — a wide-gamut project's pixels.
fn gamut_chart() -> Vec<f32> {
    let hues = [
        [1.0f32, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0],
    ];
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        let hue = hues[(y as usize * hues.len() / H as usize).min(hues.len() - 1)];
        for x in 0..W {
            let s = x as f32 / (W - 1) as f32;
            let i = ((y * W + x) * 4) as usize;
            for ch in 0..3 {
                // Neutral 0.5 at s = 0, the full primary at s = 1.
                px[i + ch] = 0.5 * (1.0 - s) + hue[ch] * s;
            }
            px[i + 3] = 1.0;
        }
    }
    px
}

/// What the SDR view did before the gamut map: the matrix into BT.709,
/// negatives clamped away.
fn clip_to_bt709(px: &[f32]) -> Vec<u8> {
    let m = klipp_color::primaries::bt2020_to_bt709();
    let mut out = Vec::with_capacity(px.len());
    for c in px.chunks_exact(4) {
        let rgb = klipp_color::primaries::matrix3_mul_vec(m, [c[0], c[1], c[2]]);
        out.extend_from_slice(&[srgb8(rgb[0]), srgb8(rgb[1]), srgb8(rgb[2]), 255]);
    }
    out
}

/// How much of each saturation ramp is flat — the run of identical pixels
/// at its bright end. A clip flattens colour onto the BT.709 hull, so the
/// end of every ramp becomes one colour and the grade cannot be judged
/// there; the gamut map rolls it off instead and the ramp stays a ramp.
fn plateau(rgba: &[u8], row: u32) -> f32 {
    let at = |x: u32| {
        let i = ((row * W + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    let last = at(W - 1);
    let mut flat = 0;
    for x in (0..W).rev() {
        if at(x) != last {
            break;
        }
        flat += 1;
    }
    flat as f32 / W as f32 * 100.0
}

/// The gamut section: the same wide-gamut chart clipped and mapped, as
/// pictures and as the share of each ramp the clip flattens.
fn gamut_proof(out_dir: &Path) {
    let chart = gamut_chart();
    let clipped = clip_to_bt709(&chart);
    let mapped = klipp_color::display::display_transform_ws(
        &chart,
        klipp_color::display::ToneMapOp::None,
        klipp_core::WorkingColorspace::Bt2020Linear,
    );
    for (name, rgba) in [("20-gamut-clipped", &clipped), ("21-gamut-mapped", &mapped)] {
        let mut rgb = Vec::with_capacity((W * H * 3) as usize);
        for c in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&c[..3]);
        }
        write_rgb8(&out_dir.join(format!("{name}.png")), &rgb);
    }
    println!("\nBT.2020 saturation ramps, share of each ramp the SDR view renders as one flat colour:");
    println!("{:<10} {:>10} {:>10}", "hue", "clipped", "mapped");
    for (i, hue) in ["red", "yellow", "green", "cyan", "blue", "magenta"].iter().enumerate() {
        let row = (i as u32 * H / 6) + H / 12;
        println!("{hue:<10} {:>9.1}% {:>9.1}%", plateau(&clipped, row), plateau(&mapped, row));
    }
    println!("20-gamut-clipped.png is the old SDR view of a BT.2020 project, 21-gamut-mapped.png the new one.");
}

fn main() {
    let out_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/colour-proof"));
    std::fs::create_dir_all(&out_dir).expect("output directory");

    let cases: Vec<(&str, Vec<BuiltinEffect>)> = vec![
        ("00-chart", vec![]),
        ("01-glow", vec![BuiltinEffect::Glow { radius: 12.0, intensity: 0.7, threshold: 0.0 }]),
        ("02-diffusion", vec![BuiltinEffect::Diffusion { amount: 0.6, radius: 12.0, threshold: 0.4, desaturation: 0.2 }]),
        ("03-film-halation", vec![BuiltinEffect::FilmHalation { threshold: 0.6, radius: 12.0, intensity: 0.6, hue: 0.05 }]),
        ("04-bloom", vec![BuiltinEffect::Bloom { threshold: 0.8, radius: 12.0, intensity: 0.6 }]),
        ("05-erode", vec![BuiltinEffect::Erode { radius: 3 }]),
        ("06-dilate", vec![BuiltinEffect::Dilate { radius: 3 }]),
        ("07-posterize", vec![BuiltinEffect::Posterize { levels: 6.0 }]),
        ("08-levels", vec![BuiltinEffect::Levels { in_black: 0.05, in_white: 0.9, gamma: 1.2, out_black: 0.0, out_white: 1.0 }]),
        ("09-equalize", vec![BuiltinEffect::Equalize { strength: 1.0 }]),
        ("10-invert", vec![BuiltinEffect::Invert { mix: 1.0 }]),
        ("11-gamma", vec![BuiltinEffect::Gamma { gamma: 1.5 }]),
        ("12-hsv", vec![BuiltinEffect::HSVAdjust { hue_shift: 25.0, saturation: 1.3, value: 1.0 }]),
    ];

    let mut gpu = klipp_effects::gpu::GpuEffectsPipeline::new();
    println!("{}", if gpu.is_some() { "GPU available: rendering the GPU path" } else { "no GPU: rendering the CPU path" });
    println!("\n{:<16} {:<18} {:>10} {:>9} {:>7}  RGB", "case", "probe", "linear R", "cd/m²", "PQ 10b");
    for (name, fx) in &cases {
        let input = chart();
        let out = match gpu.as_mut() {
            Some(g) if !fx.is_empty() => g.apply_effects(fx, &input, W, H),
            _ if fx.is_empty() => input,
            _ => klipp_effects::apply_effects(fx, input, W, H),
        };
        write_png(&out_dir.join(format!("{name}-sdr.png")), &out, srgb8);
        write_png(&out_dir.join(format!("{name}-pq.png")), &out, pq8);
        for (label, x, y) in probes() {
            let i = ((y * W + x) * 4) as usize;
            let (r, g, b) = (out[i], out[i + 1], out[i + 2]);
            let nits = r * klipp_color::hdr::HDR_REFERENCE_WHITE_NITS;
            let code = (klipp_color::hdr::pq_encode_nits(nits.clamp(0.0, 10_000.0)) * 1023.0).round();
            println!("{name:<16} {label:<18} {r:>10.3} {nits:>9.1} {code:>7.0}  [{r:.2}, {g:.2}, {b:.2}]");
        }
    }
    gamut_proof(&out_dir);
    println!("\nWrote {} pictures to {}", cases.len() * 2 + 2, out_dir.display());
    println!("*-sdr.png is the SDR view (the viewer's picture); *-pq.png is the HDR master, PQ-encoded.");
}
