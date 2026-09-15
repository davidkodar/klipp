//! Where the vectorscope puts a colour, against where the graticule says
//! that colour belongs.
//!
//! A vectorscope is a measuring instrument: its whole use is telling "on
//! the target" from "near it". That only works if the trace and the
//! graticule are drawn in the same coordinates — same colour-difference
//! matrix, same orientation, same scale — in every working space. The
//! GPU scope measures with the working space's luminance weights, the way
//! the engine drives it; the graticule's targets come from
//! `klipp_color::scopes`. This test measures that they agree. Skips
//! without a GPU.

use klipp_color::scopes::ScopeBasis;
use klipp_core::WorkingColorspace;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_scopes::{ScopeInput, ScopeJob, ScopeTransfer, VectorscopeJob};

const OUT: u32 = 256;
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

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

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The vectorscope of `frame`, measured the way the engine measures a
/// linear frame in working space `ws`.
fn render(gpu: &mut GpuEffectsPipeline, frame: &[f32], w: u32, h: u32, ws: WorkingColorspace) -> Vec<u8> {
    let input = ScopeInput::Linear {
        rgba: frame,
        width: w,
        height: h,
        matrix: IDENTITY,
        luma: klipp_color::primaries::luminance_weights(ws),
        transfer: ScopeTransfer::Srgb,
        scale: 1.0,
    };
    let job = ScopeJob::Vectorscope(VectorscopeJob {
        zoom: 1.0,
        intensity: -1.0,
        colorize: true,
        luma_lo: 0.0,
        luma_hi: 1.0,
        glow: false,
        out_w: OUT,
        out_h: OUT,
    });
    let mut out = gpu.scopes().render(&input, 1, &[job]);
    out.remove(0).expect("a vectorscope")
}

/// The six brightest clusters of the trace, as fractions of the plane,
/// with their weight.
fn clusters(tex: &[u8]) -> Vec<(f32, f32, f64)> {
    let (w, h) = (OUT as usize, OUT as usize);
    let lit = |x: usize, y: usize| -> f64 {
        let i = (y * w + x) * 4;
        let (r, g, b) = (tex[i] as f64, tex[i + 1] as f64, tex[i + 2] as f64);
        let v = r.max(g).max(b);
        if v > 24.0 { v } else { 0.0 }
    };
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

/// Where the graticule draws a target: Cb to the right, Cr up, about the
/// centre — the plane `klipp_ui` draws its boxes on.
fn target_position(cb: f32, cr: f32) -> (f32, f32) {
    (0.5 + cb, 0.5 - cr)
}

/// A box is 0.05 of full scale wide; a trace inside half that is on it.
const BOX_TOLERANCE: f32 = 0.025;

fn check_bars_land_on_targets(gpu: &mut GpuEffectsPipeline, ws: WorkingColorspace) {
    let basis = ScopeBasis::for_working_space(ws);
    let (w, h) = (192u32, 64u32);
    let frame = bars_frame(w, h, TARGET_AMPLITUDE);
    let tex = render(gpu, &frame, w, h, ws);
    assert_eq!(tex.len(), (OUT * OUT * 4) as usize);

    let found = clusters(&tex);
    assert_eq!(found.len(), 6, "six bars should make six clusters, in {}", basis.label());

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
        eprintln!("  {name:>2}: box at ({bx:.4}, {by:.4}), trace at ({nx:.4}, {ny:.4}), off by {dist:.4}");
        if dist > worst.0 {
            worst = (dist, name.to_string());
        }
    }
    assert!(
        worst.0 < BOX_TOLERANCE,
        "in a {} project, {} lands {:.4} of full scale from its target box — further than half a box is wide",
        basis.label(),
        worst.1,
        worst.0,
    );
}

#[test]
fn the_vectorscope_trace_lands_on_the_graticule_targets() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    check_bars_land_on_targets(&mut gpu, WorkingColorspace::Rec709Linear);
}

#[test]
fn a_wide_gamut_project_reads_its_own_bars_on_target() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    check_bars_land_on_targets(&mut gpu, WorkingColorspace::Bt2020Linear);
    check_bars_land_on_targets(&mut gpu, WorkingColorspace::AcesCG);
}

#[test]
fn full_amplitude_bars_sit_outside_the_seventy_five_percent_boxes() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let ws = WorkingColorspace::Rec709Linear;
    let basis = ScopeBasis::for_working_space(ws);
    let (w, h) = (192u32, 64u32);
    let frame = bars_frame(w, h, 1.0);
    let tex = render(&mut gpu, &frame, w, h, ws);
    let found = clusters(&tex);
    for (name, cb, cr) in klipp_color::scopes::vectorscope_targets(basis) {
        let (bx, by) = target_position(cb, cr);
        let radius = |x: f32, y: f32| (x - 0.5).hypot(y - 0.5);
        let (dist, nx, ny) = found
            .iter()
            .map(|(tx, ty, _)| ((tx - bx).hypot(ty - by), *tx, *ty))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("clusters");
        eprintln!("  {name:>2}: box radius {:.4}, 100% trace radius {:.4}", radius(bx, by), radius(nx, ny));
        assert!(
            radius(nx, ny) > radius(bx, by) + 0.05,
            "{name} at full amplitude sits {dist:.4} from the 75% box, not outside it",
        );
    }
}
