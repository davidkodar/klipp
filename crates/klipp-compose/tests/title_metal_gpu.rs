//! A title's metal face (#2706) on the GPU stack: every preset reads as
//! its metal with nothing set but the preset, over a bright picture and a
//! dark one. `KLIPP_METAL_SHEET=<path.png>` writes the contact sheet;
//! `KLIPP_METAL_OVER=<frame.png>` with `KLIPP_METAL_BEFORE_AFTER=<path.png>`
//! writes a flat gold title and a Gold one over that frame, side by side.
//! A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_material::{MetalPreset, TitleMaterial};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 480;
const H: u32 = 200;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// A sunny sky over bright water, or a night.
struct Backdrop(Arc<Vec<f32>>);

impl Backdrop {
    fn new(bright: bool) -> Self {
        let mut v = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let t = y as f32 / H as f32;
                let s = x as f32 / W as f32;
                let px = if bright {
                    if t < 0.55 { [0.35 + 0.4 * t, 0.5 + 0.3 * t, 0.85 - 0.2 * t] } else { [0.15 + 0.5 * s * (1.0 - t), 0.3 + 0.3 * (1.0 - t), 0.45] }
                } else {
                    [0.01 + 0.02 * s, 0.012, 0.02 + 0.02 * t]
                };
                v.extend_from_slice(&[px[0], px[1], px[2], 1.0]);
            }
        }
        Self(Arc::new(v))
    }
}

impl Backdrop {
    /// The night as the stack shows it with nothing over it.
    fn night_bytes() -> &'static [u8] {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES.get_or_init(|| {
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: W, height: H };
            let mut v1 = Track::new("V1", TrackKind::Video);
            v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
            seq.tracks = vec![v1];
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(true);
            r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Backdrop::new(false), RenderOptions::default()).expect("the stack drew the night").0
        })
    }
}

impl FrameSource for Backdrop {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width, height, pts: None })
    }
}

/// The mean of each channel over `at`, the spread of its brightness,
/// and its darkest and brightest.
fn stats(px: &[u8], at: &[usize]) -> ([f32; 3], f32, f32, f32) {
    let luma: Vec<f32> = at.iter().map(|&i| 0.2126 * px[i * 4] as f32 + 0.7152 * px[i * 4 + 1] as f32 + 0.0722 * px[i * 4 + 2] as f32).collect();
    let n = at.len() as f32;
    let mean = [0, 1, 2].map(|c| at.iter().map(|&i| px[i * 4 + c] as f32).sum::<f32>() / n);
    let m = luma.iter().sum::<f32>() / n;
    let spread = (luma.iter().map(|l| (l - m).powi(2)).sum::<f32>() / n).sqrt();
    let mut sorted = luma.clone();
    sorted.sort_by(f32::total_cmp);
    // The 2nd and 98th percentiles: a stray pixel is not a highlight.
    (mean, spread, sorted[(n * 0.02) as usize], sorted[(n * 0.98) as usize])
}

/// The title "Klipp" in `material` over the backdrop, as display bytes.
fn render(material: Option<TitleMaterial>, bright: bool, font: Option<&str>) -> Option<Vec<u8>> {
    let mut tb = klipp_timeline::TextBox { text: "Klipp".into(), font_size: 900.0, material, ..Default::default() };
    if let Some(f) = font {
        tb.font_name = f.into();
        tb.font_style = klipp_timeline::FontStyle::Bold;
    }
    let td = klipp_timeline::TitleData::single_text(tb);
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (bytes, _) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Backdrop::new(bright), RenderOptions::default())?;
    Some(bytes)
}

#[test]
fn every_metal_reads_as_its_metal_at_its_defaults() {
    if render(None, false, None).is_none() {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    }
    // The plain white title over the night: where its letters are.
    let plain = render(None, false, None).expect("the stack drew the title");
    let night = render(Some(TitleMaterial { glints: 0.0, ..TitleMaterial::default() }), false, None).expect("the stack drew the title");
    let letters: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4] > 200).collect();
    assert!(letters.len() > 2_000, "the title covers {} pixels", letters.len());
    // Where something is drawn over the night, by the plain and the gold title alike.
    let drawn = |px: &[u8]| (0..(W * H) as usize).filter(|&i| px[i * 4..i * 4 + 3].iter().zip(&Backdrop::night_bytes()[i * 4..i * 4 + 3]).any(|(a, b)| a.abs_diff(*b) > 24)).count();
    let (gold_drawn, plain_drawn) = (drawn(&night), drawn(&plain));
    assert!(gold_drawn.abs_diff(plain_drawn) * 10 < plain_drawn, "the metal keeps the letters' shape: {gold_drawn} drawn vs {plain_drawn}");

    for preset in MetalPreset::ALL {
        let px = render(Some(TitleMaterial { glints: 0.0, ..TitleMaterial::preset(preset) }), false, None).expect("the stack drew the title");
        let (mean, spread, lo, hi) = stats(&px, &letters);
        let warm = matches!(preset, MetalPreset::Gold | MetalPreset::Copper | MetalPreset::Bronze);
        if warm {
            assert!(mean[0] > mean[2] + 30.0, "{preset:?} is warm: {mean:?}");
        } else {
            assert!((mean[0] - mean[2]).abs() < 30.0, "{preset:?} is neutral: {mean:?}");
        }
        if preset == MetalPreset::Gold {
            assert!(mean[0] > mean[1] && mean[1] > mean[2], "gold's red over green over blue: {mean:?}");
        }
        // Metal, not paint: what it reflects runs from shade to highlight.
        assert!(spread > 25.0, "{preset:?} reflects, not a flat fill: spread {spread:.1}");
        assert!(hi > 200.0 && lo < 140.0, "{preset:?} has highlights and shade: {lo:.0}..{hi:.0}");
    }
    // A rougher surface blurs what it reflects.
    let mirror = render(Some(TitleMaterial { roughness: 0.02, glints: 0.0, ..TitleMaterial::preset(MetalPreset::Chrome) }), false, None).unwrap();
    let satin = render(Some(TitleMaterial { roughness: 0.8, glints: 0.0, ..TitleMaterial::preset(MetalPreset::Chrome) }), false, None).unwrap();
    assert!(stats(&mirror, &letters).1 > stats(&satin, &letters).1 + 5.0, "roughness softens the reflection");
    // What it reflects is set: a sky alone is not the studio alone.
    let env = |studio, sky| {
        let m = TitleMaterial { environment: klipp_core::title_material::MetalEnvironment { studio, sky, ..Default::default() }, glints: 0.0, ..TitleMaterial::default() };
        stats(&render(Some(m), false, None).unwrap(), &letters).0
    };
    let (studio, sky) = (env(1.0, 0.0), env(0.0, 1.0));
    assert!(studio.iter().zip(&sky).any(|(a, b)| (a - b).abs() > 8.0), "the environment shows: studio {studio:?}, sky {sky:?}");

    if let Some(path) = std::env::var_os("KLIPP_METAL_SHEET") {
        let font = std::env::var("KLIPP_METAL_FONT").ok();
        let rows: Vec<Option<TitleMaterial>> = std::iter::once(None).chain(MetalPreset::ALL.map(|p| Some(TitleMaterial::preset(p)))).collect();
        let mut sheet = vec![0u8; (W * 2 * H * rows.len() as u32 * 4) as usize];
        for (row, m) in rows.iter().enumerate() {
            for (col, bright) in [true, false].into_iter().enumerate() {
                let tile = render(*m, bright, font.as_deref()).expect("the stack drew the title");
                for y in 0..H as usize {
                    let at = ((row * H as usize + y) * (W as usize * 2) + col * W as usize) * 4;
                    sheet[at..at + W as usize * 4].copy_from_slice(&tile[y * W as usize * 4..(y + 1) * W as usize * 4]);
                }
            }
        }
        image::save_buffer(&path, &sheet, W * 2, H * rows.len() as u32, image::ExtendedColorType::Rgba8).unwrap();
    }
}

/// A picture from a PNG, decoded to linear light.
struct Still(Arc<Vec<f32>>);

impl FrameSource for Still {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width, height, pts: None })
    }
}

#[test]
fn gold_over_a_picture_before_and_after() {
    let (Some(over), Some(out)) = (std::env::var_os("KLIPP_METAL_OVER"), std::env::var_os("KLIPP_METAL_BEFORE_AFTER")) else { return };
    let img = image::open(&over).expect("the frame").to_rgba8();
    let (w, h) = img.dimensions();
    let lin = |v: u8| {
        let x = f32::from(v) / 255.0;
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    let px: Vec<f32> = img.pixels().flat_map(|p| [lin(p[0]), lin(p[1]), lin(p[2]), 1.0]).collect();
    let still = Arc::new(px);
    let draw = |material: Option<TitleMaterial>| {
        let gold = TitleMaterial::default().fallback_color();
        let tb = klipp_timeline::TextBox { text: "Klipp".into(), font_size: 320.0, color: gold, material, ..Default::default() };
        let td = klipp_timeline::TitleData::single_text(tb);
        let mut text = Track::new("T1", TrackKind::Text);
        text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }];
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: w, height: h };
        seq.tracks = vec![text, v1];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        r.render_display_u8_on_gpu(&seq, tc(1.0), w, h, &mut Still(Arc::clone(&still)), RenderOptions::default()).expect("the stack drew the title").0
    };
    let (before, after) = (draw(None), draw(Some(TitleMaterial::default())));
    let mut both = vec![0u8; (w * 2 * h * 4) as usize];
    for y in 0..h as usize {
        let row = w as usize * 4;
        both[y * row * 2..y * row * 2 + row].copy_from_slice(&before[y * row..(y + 1) * row]);
        both[y * row * 2 + row..(y + 1) * row * 2].copy_from_slice(&after[y * row..(y + 1) * row]);
    }
    image::save_buffer(&out, &both, w * 2, h, image::ExtendedColorType::Rgba8).unwrap();
}
