//! A PQ, HLG or camera log clip shown through a composition that only
//! reads its footage renders as the clip without one: Veylvik gets the
//! clip's scene light, and its picture keeps the clip's own display step
//! after the grade (the SDR tone map, or an HDR delivery's display light).
//! Taken for a display-referred picture, it was shown with no tone map at
//! all: log footage flat and grey, PQ highlights clipped.

use std::sync::Arc;

use klipp_color::hdr::{pack_rgb16, HdrTransfer, SceneCodes};
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ColorTransfer, MediaId, RationalTime, WorkingColorspace};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Compositor, Time};

/// The compositor's compositions are the process's: one test at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 96;
const H: u32 = 54;
const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

/// A ramp of 16-bit codes across the code range, as a decoder gives a
/// scene-referred frame.
struct Codes(HdrTransfer);
impl FrameSource for Codes {
    fn frame(&mut self, _: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let codes = (0..h)
            .flat_map(|y| (0..w).map(move |x| pack_rgb16((x * 65535 / w) as u16, (y * 65535 / h) as u16, 20000)))
            .collect();
        Some(SourceFrame {
            pixels: SourcePixels::SceneReferred { codes: Arc::new(SceneCodes::Deep16(codes)), transfer: self.0, source_to_working: klipp_color::primaries::bt2020_to_bt709() },
            width: w,
            height: h,
            pts: Some(secs),
        })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(f64::from(FPS))
    }
}

fn sequence(composition: Option<CompositionId>) -> Sequence {
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(7)), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.composition = composition;
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

fn render(seq: &Sequence, src: &mut dyn FrameSource, gpu: bool, hdr: Option<f32>, working: WorkingColorspace) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r.set_working_colorspace(working);
    let opts = RenderOptions { hdr_peak_nits: hdr, ..Default::default() };
    r.render_inspected(seq, tc(1.0), W, H, src, opts, Inspect::default()).0
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn a_scene_referred_clip_keeps_its_display_step_through_a_composition() {
    let Some(compositor) = Compositor::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let pass = CompositionId(uuid::Uuid::from_u128(1));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: i64::from(FPS), rate: 1 }, 48).unwrap();
    compositor.set_compositions([(pass, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let mut failed = Vec::new();
    for (transfer, working) in [HdrTransfer::Pq, HdrTransfer::Hlg, HdrTransfer::Log(ColorTransfer::SLog3)]
        .into_iter()
        .flat_map(|t| [WorkingColorspace::Rec709Linear, WorkingColorspace::AcesCG, WorkingColorspace::Bt2020Linear].map(|w| (t, w)))
    {
        // One media decoded another way: as the app says when a file's
        // transfer is set, so no frame kept of the other way is used.
        klipp_veylvik::media_changed();
        for hdr in [None, Some(1000.0)] {
            for gpu in [true, false] {
                let plain = render(&sequence(None), &mut Codes(transfer), gpu, hdr, working);
                let mut codes = Codes(transfer);
                let through = render(&sequence(Some(pass)), &mut compositor.source(&mut codes, working), gpu, hdr, working);
                let off = worst(&plain, &through);
                eprintln!("{transfer:?} {working:?} hdr={hdr:?} gpu={gpu}: pass-through differs by {off}");
                // Through Veylvik's Rec.709 and back in f32 outside Rec.709.
                if off > 1e-4 {
                    failed.push(format!("{transfer:?} {working:?} hdr={hdr:?} gpu={gpu}: {off}"));
                }
            }
        }
    }
    compositor.set_compositions(Compositions::new());
    assert!(failed.is_empty(), "{failed:#?}");
}


fn plain_target() -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target { tone_map_op: 0, odt_tag: 0, scene: None, gamut_op: 0, hdr_op: 0, peak_nits: 0.0, luma_coefs: [0.2126, 0.7152, 0.0722], semi_planar: false, bits: 8, sample_shift: 0 }
}

fn pq_target(peak: f32) -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target { hdr_op: 1, peak_nits: peak, luma_coefs: [0.2627, 0.6780, 0.0593], semi_planar: true, bits: 10, sample_shift: 6, ..plain_target() }
}

/// The most two frames' samples are apart, in codes: bytes, or 16-bit
/// samples shifted by `shift` (P010).
fn codes_off(a: &[u8], b: &[u8], target: &klipp_effects::gpu::Yuv420Target) -> u16 {
    assert_eq!(a.len(), b.len());
    if target.bits == 8 {
        return a.iter().zip(b).map(|(x, y)| u16::from(x.abs_diff(*y))).max().unwrap_or(0);
    }
    let samples = |s: &[u8]| s.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c) >> target.sample_shift).collect::<Vec<_>>();
    samples(a).iter().zip(samples(b)).map(|(x, y)| x.abs_diff(y)).max().unwrap_or(0)
}

/// The viewer's GPU stack and the export's GPU route, each the way a scene
/// referred clip takes it: its display step on the GPU there.
#[test]
fn the_gpu_routes_keep_a_scene_referred_clips_display_step_through_a_composition() {
    let Some(compositor) = Compositor::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let pass = CompositionId(uuid::Uuid::from_u128(1));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [W, H], Time { ticks: i64::from(FPS), rate: 1 }, 48).unwrap();
    let w709 = WorkingColorspace::Rec709Linear;
    let mut failed = Vec::new();
    for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg, HdrTransfer::Log(ColorTransfer::SLog3)] {
        compositor.set_compositions([(pass, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
        klipp_veylvik::media_changed();
        let renderer = || {
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(true);
            r.set_working_colorspace(w709);
            r
        };
        // The viewer's stack: display bytes.
        let plain = renderer().render_display_u8_on_gpu(&sequence(None), tc(1.0), W, H, &mut Codes(transfer), RenderOptions::default()).map(|(b, _)| b);
        let mut codes = Codes(transfer);
        let through = renderer().render_display_u8_on_gpu(&sequence(Some(pass)), tc(1.0), W, H, &mut compositor.source(&mut codes, w709), RenderOptions::default()).map(|(b, _)| b);
        match (plain, through) {
            (Some(a), Some(b)) => {
                let off = codes_off(&a, &b, &plain_target());
                eprintln!("{transfer:?} viewer stack: {off} code(s) apart");
                if off > 1 {
                    failed.push(format!("{transfer:?} viewer stack: {off}"));
                }
            }
            (a, b) => failed.push(format!("{transfer:?} viewer stack: drawn {} plain, {} through", a.is_some(), b.is_some())),
        }
        // The export's own GPU route: SDR and a PQ master.
        for (name, target, hdr) in [("SDR", plain_target(), None), ("PQ", pq_target(1000.0), Some(1000.0))] {
            let opts = RenderOptions { hdr_peak_nits: hdr, ..Default::default() };
            let plain = renderer().render_yuv420_on_gpu(&sequence(None), tc(1.0), W, H, &mut Codes(transfer), opts, target).map(|(b, _)| b);
            let mut codes = Codes(transfer);
            let through = renderer().render_yuv420_on_gpu(&sequence(Some(pass)), tc(1.0), W, H, &mut compositor.source(&mut codes, w709), opts, target).map(|(b, _)| b);
            match (plain, through) {
                (Some(a), Some(b)) => {
                    let off = codes_off(&a, &b, &target);
                    eprintln!("{transfer:?} export {name}: {off} code(s) apart");
                    if off > 1 {
                        failed.push(format!("{transfer:?} export {name}: {off}"));
                    }
                }
                (a, b) => eprintln!("{transfer:?} export {name}: drawn {} plain, {} through (declined is the ordinary path)", a.is_some(), b.is_some()),
            }
        }
    }
    compositor.set_compositions(Compositions::new());
    assert!(failed.is_empty(), "{failed:#?}");
}
