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
/// its darkest, and its brightest by its brightest channel.
fn stats(px: &[u8], at: &[usize]) -> ([f32; 3], f32, f32, f32) {
    let luma: Vec<f32> = at.iter().map(|&i| 0.2126 * px[i * 4] as f32 + 0.7152 * px[i * 4 + 1] as f32 + 0.0722 * px[i * 4 + 2] as f32).collect();
    let n = at.len() as f32;
    let mean = [0, 1, 2].map(|c| at.iter().map(|&i| px[i * 4 + c] as f32).sum::<f32>() / n);
    let m = luma.iter().sum::<f32>() / n;
    let spread = (luma.iter().map(|l| (l - m).powi(2)).sum::<f32>() / n).sqrt();
    let mut sorted = luma.clone();
    sorted.sort_by(f32::total_cmp);
    // A highlight by its brightest channel: an orange metal's is red.
    let mut peak: Vec<f32> = at.iter().map(|&i| px[i * 4..i * 4 + 3].iter().copied().max().unwrap_or(0) as f32).collect();
    peak.sort_by(f32::total_cmp);
    // The 2nd and 98th percentiles: a stray pixel is not a highlight.
    (mean, spread, sorted[(n * 0.02) as usize], peak[(n * 0.98) as usize])
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

    // Each metal as it looks over a bright sky and sea, which it reflects;
    // the night above held its letters' shape.
    for preset in MetalPreset::ALL {
        let px = render(Some(TitleMaterial { glints: 0.0, ..TitleMaterial::preset(preset) }), true, None).expect("the stack drew the title");
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
        // Steel and bronze are the dark metals: bright, not white, at their top.
        let top = if matches!(preset, MetalPreset::Steel | MetalPreset::Bronze) { 160.0 } else { 200.0 };
        assert!(hi > top && lo < 140.0, "{preset:?} has highlights and shade: {lo:.0}..{hi:.0}");
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
    let draw = |material: TitleMaterial| {
        let tb = klipp_timeline::TextBox { text: "Klipp".into(), font_size: 320.0, material: Some(material), ..Default::default() };
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
    // A studio and a sky alone, as the first metal was; the defaults; the
    // shot alone over a little studio.
    let gold = TitleMaterial::default();
    let env = |studio, sky, shot| klipp_core::title_material::MetalEnvironment { studio, sky, shot, ..gold.environment };
    let frames = [
        draw(TitleMaterial { environment: env(0.55, 0.5, 0.0), ..gold }),
        draw(gold),
        draw(TitleMaterial { environment: env(0.2, 0.0, 1.0), ..gold }),
    ];
    let row = w as usize * 4;
    let mut all = vec![0u8; row * frames.len() * h as usize];
    for y in 0..h as usize {
        for (k, f) in frames.iter().enumerate() {
            let at = y * row * frames.len() + k * row;
            all[at..at + row].copy_from_slice(&f[y * row..(y + 1) * row]);
        }
    }
    image::save_buffer(&out, &all, w * frames.len() as u32, h, image::ExtendedColorType::Rgba8).unwrap();
}

/// #2706: at its defaults the metal plainly shows the picture behind it:
/// over a bright sky above a sea, gold differs from the same gold lit by
/// a studio and a sky alone by a visible margin across its letters.
#[test]
fn at_its_defaults_the_metal_shows_the_shot() {
    let Some(plain) = render(None, true, None) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let letters: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4] > 200 && plain[i * 4 + 2] > 200 && plain[i * 4 + 1] > 200).collect();
    let gold = TitleMaterial { glints: 0.0, ..TitleMaterial::default() };
    let with = render(Some(gold), true, None).expect("drawn");
    let without = render(Some(TitleMaterial { environment: klipp_core::title_material::MetalEnvironment { shot: 0.0, studio: 0.55, sky: 0.5, ..gold.environment }, ..gold }), true, None).expect("drawn");
    let differs = letters.iter().map(|&i| (0..3).map(|c| with[i * 4 + c].abs_diff(without[i * 4 + c]) as f32).sum::<f32>() / 3.0).sum::<f32>() / letters.len() as f32;
    assert!(differs > 12.0, "the shot changes the gold by {differs:.1} levels on average, too little to see");
}

/// One colour, everywhere.
struct Flat([f32; 3]);

impl FrameSource for Flat {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let px: Vec<f32> = (0..width * height).flat_map(|_| [self.0[0], self.0[1], self.0[2], 1.0]).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width, height, pts: None })
    }
}

/// The title over one colour, with `fx` on it: the bytes, and how many
/// layers ran effects.
fn over_flat(material: TitleMaterial, colour: [f32; 3], fx: Vec<klipp_effects::BuiltinEffect>) -> Option<(Vec<u8>, u32)> {
    let tb = klipp_timeline::TextBox { text: "Klipp".into(), font_size: 900.0, material: Some(material), ..Default::default() };
    let td = klipp_timeline::TitleData::single_text(tb);
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![klipp_timeline::test_clip::TestClip::of(Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) }).effects(fx).build()];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let (bytes, stats) = r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Flat(colour), RenderOptions::default())?;
    Some((bytes, stats.effects_on_layers))
}

/// #2706: a metal reflecting only the shot behind it takes that shot's
/// colour; its effects run after the reflection, so a glow is still
/// drawn round it; with no shot in the mix the picture behind changes
/// nothing on the metal.
#[test]
fn a_metal_reflects_the_shot_behind_it() {
    let Some(plain) = render(None, false, None) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let letters: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4] > 200).collect();
    let only = |shot: f32| TitleMaterial {
        environment: klipp_core::title_material::MetalEnvironment { studio: 0.0, sky: 0.0, shot, ..Default::default() },
        glints: 0.0,
        ..TitleMaterial::preset(MetalPreset::Silver)
    };
    let (red, _) = over_flat(only(1.0), [0.8, 0.05, 0.02], Vec::new()).expect("drawn");
    let (blue, _) = over_flat(only(1.0), [0.02, 0.1, 0.8], Vec::new()).expect("drawn");
    let (r, b) = (stats(&red, &letters).0, stats(&blue, &letters).0);
    assert!(r[0] > r[2] + 40.0 && b[2] > b[0] + 40.0, "silver over red is red, over blue blue: {r:?} {b:?}");
    // No shot in the mix: what is behind does not show on the metal.
    let none = TitleMaterial { environment: klipp_core::title_material::MetalEnvironment { shot: 0.0, ..Default::default() }, glints: 0.0, ..TitleMaterial::preset(MetalPreset::Silver) };
    let (a, _) = over_flat(none, [0.8, 0.05, 0.02], Vec::new()).expect("drawn");
    let (c, _) = over_flat(none, [0.02, 0.1, 0.8], Vec::new()).expect("drawn");
    let (ma, mc) = (stats(&a, &letters).0, stats(&c, &letters).0);
    // Within a few levels: the letters' soft edges show what is behind.
    assert!(ma.iter().zip(&mc).all(|(x, y)| (x - y).abs() < 6.0), "{ma:?} vs {mc:?}");
    // A glow held for the reflection still runs, and lights round the letters.
    let glow = vec![klipp_effects::BuiltinEffect::Glow { radius: 20.0, intensity: 1.0 }];
    let (lit, ran) = over_flat(only(1.0), [0.01, 0.01, 0.01], glow).expect("drawn");
    let (dark, _) = over_flat(only(1.0), [0.01, 0.01, 0.01], Vec::new()).expect("drawn");
    assert_eq!(ran, 1, "the held effects ran");
    let around = |px: &[u8]| (0..(W * H) as usize).filter(|i| !letters.contains(i)).map(|i| u64::from(px[i * 4])).sum::<u64>();
    assert!(around(&lit) > around(&dark) + 2_000, "the glow lights round the letters");
}

/// The neutral metals are three different metals at their defaults:
/// steel darker than silver, chrome a mirror with near-black darks;
/// copper is orange,
/// not pink, beside rose gold.
#[test]
fn the_metals_are_told_apart() {
    let Some(plain) = render(None, false, None) else { return };
    let letters: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4] > 200).collect();
    let of = |p: MetalPreset| {
        let px = render(Some(TitleMaterial { glints: 0.0, ..TitleMaterial::preset(p) }), false, None).expect("drawn");
        stats(&px, &letters)
    };
    let luma = |m: [f32; 3]| 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
    let (silver, steel, chrome) = (of(MetalPreset::Silver), of(MetalPreset::Steel), of(MetalPreset::Chrome));
    assert!(luma(silver.0) > luma(steel.0) + 15.0, "steel is darker than silver: {:?} {:?}", silver.0, steel.0);
    assert!(steel.0[2] > steel.0[0], "steel is bluish: {:?}", steel.0);
    // A mirror: near-black between bright bands, where silver's shade is soft.
    assert!(chrome.2 + 15.0 < silver.2 && chrome.3 > 200.0, "chrome's darks are darker than silver's, its highs as bright: chrome {:.0}..{:.0}, silver {:.0}..{:.0}", chrome.2, chrome.3, silver.2, silver.3);
    let (copper, rose) = (of(MetalPreset::Copper).0, of(MetalPreset::RoseGold).0);
    // Orange keeps green well above blue; pink has them close.
    assert!(copper[1] - copper[2] > rose[1] - rose[2] + 10.0, "copper is orange, rose gold pink: {copper:?} {rose:?}");
}

/// Gold at its defaults over blue water stays gold: red over green over
/// blue, well apart, not the olive grey the blue alone would make of it.
#[test]
fn gold_over_blue_water_stays_gold() {
    let Some(plain) = render(None, false, None) else {
        assert!(!required(), "required, but the GPU stack declined the title");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let letters: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4] > 200).collect();
    let (px, _) = over_flat(TitleMaterial { glints: 0.0, ..TitleMaterial::default() }, [0.03, 0.18, 0.32], Vec::new()).expect("drawn");
    let m = stats(&px, &letters).0;
    assert!(m[0] > m[1] && m[1] > m[2] && m[0] > m[2] + 45.0, "gold over blue water: {m:?}");
}

/// A bevelled 3D "Klipp" in `material` over one colour, turned a little.
fn solid_over_flat(material: Option<TitleMaterial>, colour: [f32; 3]) -> Option<Vec<u8>> {
    let three_d = klipp_core::text3d::Text3d { rotation: [10.0, -20.0, 0.0], ..klipp_core::text3d::Text3d::default().with_look("bevelled").expect("a look") };
    let tb = klipp_timeline::TextBox { text: "Klipp".into(), font_size: 900.0, material, three_d: Some(three_d), ..Default::default() };
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
    Some(r.render_display_u8_on_gpu(&seq, tc(1.0), W, H, &mut Flat(colour), RenderOptions::default())?.0)
}

/// #2706: a 3D text is metal on every face. Gold keeps its hue and
/// reflects, from shade to highlight, on its bevels and sides; silver
/// with only the shot in its mix takes the colour of what is behind it.
/// `KLIPP_METAL_3D_SHEET=<path.png>` writes every metal in 3D.
#[test]
fn a_3d_text_is_metal_on_every_face() {
    let night = [0.01, 0.01, 0.02];
    let Some(plain) = solid_over_flat(None, night) else {
        assert!(!required(), "required, but the GPU stack declined the solid");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    // The solid, wherever it differs from the night.
    let solid: Vec<usize> = (0..(W * H) as usize).filter(|&i| plain[i * 4..i * 4 + 3].iter().any(|&v| v > 40)).collect();
    assert!(solid.len() > 2_000, "the solid covers {} pixels", solid.len());
    let gold = solid_over_flat(Some(TitleMaterial { glints: 0.0, ..TitleMaterial::default() }), night).expect("drawn");
    let (mean, spread, lo, hi) = stats(&gold, &solid);
    assert!(mean[0] > mean[1] && mean[1] > mean[2] && mean[0] > mean[2] + 30.0, "3D gold is gold: {mean:?}");
    assert!(spread > 20.0 && hi > lo + 80.0, "3D gold reflects: spread {spread:.1}, {lo:.0}..{hi:.0}");
    let only = TitleMaterial {
        environment: klipp_core::title_material::MetalEnvironment { studio: 0.0, sky: 0.0, shot: 1.0, ..Default::default() },
        glints: 0.0,
        ..TitleMaterial::preset(MetalPreset::Silver)
    };
    let red = stats(&solid_over_flat(Some(only), [0.8, 0.05, 0.02]).expect("drawn"), &solid).0;
    let blue = stats(&solid_over_flat(Some(only), [0.02, 0.1, 0.8]).expect("drawn"), &solid).0;
    assert!(red[0] > red[2] + 30.0 && blue[2] > blue[0] + 30.0, "3D silver reflects the shot: {red:?} {blue:?}");

    if let Some(path) = std::env::var_os("KLIPP_METAL_3D_SHEET") {
        let rows: Vec<Option<TitleMaterial>> = std::iter::once(None).chain(MetalPreset::ALL.map(|p| Some(TitleMaterial::preset(p)))).collect();
        let mut sheet = vec![0u8; (W * 2 * H * rows.len() as u32 * 4) as usize];
        for (row, m) in rows.iter().enumerate() {
            for (col, behind) in [[0.35, 0.5, 0.75], night].into_iter().enumerate() {
                let tile = solid_over_flat(*m, behind).expect("drawn");
                for y in 0..H as usize {
                    let at = ((row * H as usize + y) * (W as usize * 2) + col * W as usize) * 4;
                    sheet[at..at + W as usize * 4].copy_from_slice(&tile[y * W as usize * 4..(y + 1) * W as usize * 4]);
                }
            }
        }
        image::save_buffer(&path, &sheet, W * 2, H * rows.len() as u32, image::ExtendedColorType::Rgba8).unwrap();
    }
}
