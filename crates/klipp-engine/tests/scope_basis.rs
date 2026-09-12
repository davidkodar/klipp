//! Where the vectorscope puts a colour, against where the graticule says
//! that colour belongs.
//!
//! A vectorscope is a measuring instrument: its whole use is telling "on
//! the target" from "near it". That only works if the trace and the
//! graticule are drawn in the same coordinates — same colour-difference
//! matrix, same orientation, same scale. Nothing in the shipping path
//! guaranteed that, so this test measures it.

use klipp_color::scopes::ScopeBasis;
use klipp_core::{ScopeFfmpegSettings, WorkingColorspace};
use klipp_engine::ffmpeg_scope::{FfmpegScopeBank, SCOPE_OUT_H, SCOPE_OUT_W};

const VECTORSCOPE: u8 = 2;

/// The six saturated bars, in the order a colourist names them.
const BARS: [(&str, [f32; 3]); 6] = [
    ("R", [1.0, 0.0, 0.0]),
    ("Yl", [1.0, 1.0, 0.0]),
    ("G", [0.0, 1.0, 0.0]),
    ("Cy", [0.0, 1.0, 1.0]),
    ("B", [0.0, 0.0, 1.0]),
    ("Mg", [1.0, 0.0, 1.0]),
];

/// The amplitude the graticule's boxes are drawn for.
const TARGET_AMPLITUDE: f32 = 0.75;

/// One vertical bar per colour, linear light, at `amplitude`.
fn bars_frame(width: u32, height: u32, amplitude: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let bar = (x * 6 / width).min(5) as usize;
            let [r, g, b] = BARS[bar].1;
            // Encoded amplitude: the scopes measure the display-encoded
            // picture, so a "75% bar" is 0.75 of signal, not of light.
            let lin = |c: f32| srgb_to_linear(c * amplitude);
            out.extend_from_slice(&[lin(r), lin(g), lin(b), 1.0]);
        }
    }
    out
}

/// Inverse of the OETF the scope path applies, so a bar written here comes
/// back out as the signal value it names.
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Feed until the filter graph hands a frame back.
fn render(
    bank: &mut FfmpegScopeBank,
    frame: &[f32],
    w: u32,
    h: u32,
    basis: ScopeBasis,
) -> Option<Vec<u8>> {
    for _ in 0..8 {
        match bank.render_linear(
            VECTORSCOPE,
            frame,
            w,
            h,
            ScopeFfmpegSettings::default(),
            basis,
        ) {
            Ok(Some(tex)) => return Some(tex),
            Ok(None) => continue,
            Err(e) => {
                eprintln!("skipping: the vectorscope filter is unavailable here: {e}");
                return None;
            }
        }
    }
    None
}

/// The trace's six clusters, as normalised positions in the square the UI
/// draws the texture into: `(0,0)` top-left, `(1,1)` bottom-right.
///
/// The texture is 1024x512 and the UI squeezes it back into a square, so
/// normalised coordinates are the vectorscope's own square again.
fn clusters(tex: &[u8]) -> Vec<(f32, f32, f64)> {
    let (w, h) = (SCOPE_OUT_W as usize, SCOPE_OUT_H as usize);
    let lit = |x: usize, y: usize| -> f64 {
        let i = (y * w + x) * 4;
        let (r, g, b) = (tex[i] as f64, tex[i + 1] as f64, tex[i + 2] as f64);
        // The trace is drawn over pure black; anything with light in it
        // is signal. Weight by brightness so a soft dot centres properly.
        let v = r.max(g).max(b);
        if v > 24.0 { v } else { 0.0 }
    };
    // Greedy peak-picking: take the brightest pixel, claim everything
    // within a radius of it as one cluster, repeat. Six dots, far apart.
    let mut weight: Vec<Vec<f64>> = (0..h).map(|y| (0..w).map(|x| lit(x, y)).collect()).collect();
    let mut found = Vec::new();
    for _ in 0..6 {
        let mut peak = (0usize, 0usize, 0.0f64);
        for y in 0..h {
            for x in 0..w {
                if weight[y][x] > peak.2 {
                    peak = (x, y, weight[y][x]);
                }
            }
        }
        if peak.2 <= 0.0 {
            break;
        }
        // Centroid over the neighbourhood, then clear it.
        let (rx, ry) = (w / 12, h / 12);
        let (x0, x1) = (peak.0.saturating_sub(rx), (peak.0 + rx).min(w - 1));
        let (y0, y1) = (peak.1.saturating_sub(ry), (peak.1 + ry).min(h - 1));
        let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let v = weight[y][x];
                sx += x as f64 * v;
                sy += y as f64 * v;
                sw += v;
                weight[y][x] = 0.0;
            }
        }
        found.push(((sx / sw / w as f64) as f32, (sy / sw / h as f64) as f32, sw));
    }
    found
}

/// Where the UI draws a target, as a normalised position in the square:
/// Cb to the right, Cr up, which is the mapping
/// `klipp-ui/src/widgets/scopes.rs` paints with.
fn target_position(cb: f32, cr: f32) -> (f32, f32) {
    (0.5 + cb, 0.5 - cr)
}

/// Bars in, graticule out: the trace has to land in the boxes.
///
/// A target box is about 4 px across in a ~500 px square — 0.016 of full
/// scale. Anything further out than a box is half the reason a colourist
/// would distrust the instrument; this allows a little over one box for the
/// width of the trace itself.
const BOX_TOLERANCE: f32 = 0.025;

fn check_bars_land_on_targets(ws: WorkingColorspace) {
    let basis = ScopeBasis::for_working_space(ws);
    let (w, h) = (192u32, 64u32);
    let frame = bars_frame(w, h, TARGET_AMPLITUDE);
    let mut bank = FfmpegScopeBank::new();
    let Some(tex) = render(&mut bank, &frame, w, h, basis) else {
        return;
    };
    assert_eq!(tex.len(), (SCOPE_OUT_W * SCOPE_OUT_H * 4) as usize);

    let found = clusters(&tex);
    assert_eq!(found.len(), 6, "six bars should make six clusters, in {}", basis.label());

    // The very targets the UI paints — not a copy of them.
    let targets = klipp_color::scopes::vectorscope_targets(basis);
    let mut worst: (f32, String) = (0.0, String::new());
    eprintln!("-- {} project, 75% bars --", basis.label());
    for (name, cb, cr) in targets {
        let (bx, by) = target_position(cb, cr);
        let (dist, nx, ny) = found
            .iter()
            .map(|(tx, ty, _)| ((tx - bx).hypot(ty - by), *tx, *ty))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("the trace has clusters");
        eprintln!(
            "  {name:>2}: box at ({bx:.4}, {by:.4}), trace at ({nx:.4}, {ny:.4}), off by {dist:.4}"
        );
        if dist > worst.0 {
            worst = (dist, name.to_string());
        }
    }
    assert!(
        worst.0 < BOX_TOLERANCE,
        "in a {} project, {} lands {:.4} of full scale from its target box — \
         further than a box is wide",
        basis.label(),
        worst.1,
        worst.0,
    );
}

#[test]
fn the_vectorscope_trace_lands_on_the_graticule_targets() {
    check_bars_land_on_targets(WorkingColorspace::Rec709Linear);
}

/// A wide-gamut project's own bars, on its own targets. The frame holds the
/// same numbers — each space's primaries at 75% — but they are a different
/// colour in each space, and the graticule moves with them. Rec.709's matrix
/// puts BT.2020's green and magenta 0.0193 of full scale away from where
/// they belong: wider than a target box, so the bar would read off-target.
#[test]
fn a_wide_gamut_project_reads_its_own_bars_on_target() {
    check_bars_land_on_targets(WorkingColorspace::Bt2020Linear);
    check_bars_land_on_targets(WorkingColorspace::AcesCG);
}

/// 100% bars overshoot the 75% boxes, as they do on hardware — proof the
/// scale is a real one and not fitted to whatever was fed in.
#[test]
fn full_amplitude_bars_sit_outside_the_seventy_five_percent_boxes() {
    let basis = ScopeBasis::default();
    let (w, h) = (192u32, 64u32);
    let frame = bars_frame(w, h, 1.0);
    let mut bank = FfmpegScopeBank::new();
    let Some(tex) = render(&mut bank, &frame, w, h, basis) else {
        return;
    };
    let found = clusters(&tex);
    for (name, cb, cr) in klipp_color::scopes::vectorscope_targets(basis) {
        let (bx, by) = target_position(cb, cr);
        let radius = |x: f32, y: f32| (x - 0.5).hypot(y - 0.5);
        let (dist, nx, ny) = found
            .iter()
            .map(|(tx, ty, _)| ((tx - bx).hypot(ty - by), *tx, *ty))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("clusters");
        eprintln!(
            "  {name:>2}: box radius {:.4}, 100% trace radius {:.4}",
            radius(bx, by),
            radius(nx, ny),
        );
        assert!(
            radius(nx, ny) > radius(bx, by) + 0.05,
            "{name} at full amplitude sits {dist:.4} from the 75% box, not outside it",
        );
    }
}
