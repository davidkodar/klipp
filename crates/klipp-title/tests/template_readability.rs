//! Measured sweep of the built-in title templates' readability (#2734):
//! each one, as it rests, in 16:9, 9:16 and 1:1, over a bright, a dark and
//! a busy frame. Measured per text layer: the contrast (WCAG ratio) of its
//! glyphs against what is round them as composited; that every text stays
//! inside title-safe (5 % in from each edge); and the smallest text's size
//! (in pixels of a 1080-line frame). Each is composited with its contrast
//! protection, as the compositor lays it. Report: `target/sweep/templates.txt`.
//! A template set in a catalogue face not fetched here is skipped.
//! `KLIPP_TEMPLATES_SHEET=<dir>` writes contact sheets there, a page of
//! templates each, every one in the three formats over the bright frame.

use klipp_testing::sweep::Sweep;
use klipp_timeline::title_library::{Format, TALL_BAND_TOP, TALL_STRIP_LEFT, TITLES};
use klipp_timeline::{TitleData, TitleObjectContent};
use klipp_title::TitleRenderer;

/// Items known to fail, and the issue that holds each: generated from the
/// sweep's first run, each to come off as #2734's work fixes it.
const KNOWN: &[(&str, u32)] = &[];

/// Readable: a ratio of at least this between a text's glyphs and what
/// is round them (WCAG's large-text minimum).
const CONTRAST: f32 = 3.0;
/// The smallest text, in pixels of a 1080-line frame.
const MIN_SIZE: f32 = klipp_timeline::title_library::MIN_TEXT_SIZE;

/// How far a text's distance from the next may stray from the design's,
/// as a factor, in the title's type size.
const SPACING: f32 = 1.25;

const FORMATS: &[(&str, u32, u32)] = &[("16:9", 960, 540), ("9:16", 540, 960), ("1:1", 540, 540)];

fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn luminance(p: &[u8]) -> f32 {
    0.2126 * srgb_to_linear(p[0]) + 0.7152 * srgb_to_linear(p[1]) + 0.0722 * srgb_to_linear(p[2])
}

/// The three frames, sRGB RGBA: a bright beach under sky and cloud, a dark
/// night with a few lights, and busy detail over the full range.
fn backgrounds(w: u32, h: u32) -> Vec<(&'static str, Vec<u8>)> {
    let make = |f: &dyn Fn(f32, f32, u32, u32) -> [u8; 3]| -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| {
            let c = f(x as f32 / w as f32, y as f32 / h as f32, x, y);
            [c[0], c[1], c[2], 255]
        }).collect()
    };
    let bright = make(&|fx, fy, _, _| {
        let cloud = (-((fx - 0.3).powi(2) * 30.0 + (fy - 0.25).powi(2) * 80.0)).exp() + (-((fx - 0.75).powi(2) * 25.0 + (fy - 0.35).powi(2) * 90.0)).exp();
        if fy < 0.65 {
            let sky = [200.0 + 30.0 * fy, 222.0 + 15.0 * fy, 250.0];
            sky.map(|c| (c + cloud * 60.0).min(255.0) as u8)
        } else {
            [232, 216, 182]
        }
    });
    let dark = make(&|fx, fy, _, _| {
        let lamp = |x: f32, y: f32| (-((fx - x).powi(2) + (fy - y).powi(2)) * 900.0).exp();
        let l = lamp(0.2, 0.45) + lamp(0.6, 0.4) + lamp(0.85, 0.5);
        [(12.0 + 200.0 * l).min(255.0) as u8, (14.0 + 170.0 * l).min(255.0) as u8, (20.0 + 90.0 * l).min(255.0) as u8]
    });
    let busy = make(&|_, _, x, y| {
        let mut v = (x / 3).wrapping_mul(0x9E37_79B1) ^ (y / 3).wrapping_mul(0x85EB_CA6B);
        v ^= v >> 15;
        v = v.wrapping_mul(0x2C1B_3C6D);
        v ^= v >> 12;
        [(v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8]
    });
    vec![("bright", bright), ("dark", dark), ("busy", busy)]
}

/// `fg` (straight alpha) over the opaque `bg`.
fn over(fg: &[u8], bg: &[u8]) -> Vec<u8> {
    fg.chunks(4).zip(bg.chunks(4)).flat_map(|(f, b)| {
        let a = f[3] as f32 / 255.0;
        [0, 1, 2].map(|c| (f[c] as f32 * a + b[c] as f32 * (1.0 - a)).round() as u8).into_iter().chain([255])
    }).collect()
}

/// `bg` as the template's contrast protection leaves it under `fg`
/// (#2734), through the compositor's own CPU pass.
fn protected(fg: &[u8], bg: &[u8], w: u32, h: u32, mode: u32) -> Vec<u8> {
    if mode == 0 {
        return bg.to_vec();
    }
    let lin = |px: &[u8]| -> Vec<f32> { px.chunks(4).flat_map(|p| [srgb_to_linear(p[0]), srgb_to_linear(p[1]), srgb_to_linear(p[2]), p[3] as f32 / 255.0]).collect() };
    let mut acc = lin(bg);
    klipp_effects::protect::protect_backdrop(&mut acc, &lin(fg), w, h, 1.0, mode, [0.2126, 0.7152, 0.0722]);
    klipp_effects::linear_f32_rgba_to_srgb_u8(&acc)
}

/// The side of a contact sheet's cell, in pixels:
/// `KLIPP_TEMPLATES_SHEET_CELL`, else 300.
fn cell_side() -> u32 {
    std::env::var("KLIPP_TEMPLATES_SHEET_CELL").ok().and_then(|v| v.parse().ok()).unwrap_or(300)
}

/// Contact sheets of the templates, a row each and a cell per format.
struct Sheet {
    rows: Vec<(String, Vec<Vec<u8>>)>,
}

impl Sheet {
    /// `px` (`w` × `h`) shrunk into a cell, centred on grey.
    fn cell(px: &[u8], w: u32, h: u32) -> Vec<u8> {
        let cell = cell_side();
        let k = (cell as f32 / w.max(h) as f32).min(1.0);
        let (cw, ch) = ((w as f32 * k) as u32, (h as f32 * k) as u32);
        let (ox, oy) = ((cell - cw) / 2, (cell - ch) / 2);
        let mut out = [60u8, 60, 60, 255].repeat((cell * cell) as usize);
        for y in 0..ch {
            for x in 0..cw {
                let (sx, sy) = (((x as f32 + 0.5) / k) as u32, ((y as f32 + 0.5) / k) as u32);
                let i = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
                let o = (((oy + y) * cell + ox + x) * 4) as usize;
                out[o..o + 4].copy_from_slice(&px[i..i + 4]);
            }
        }
        out
    }

    /// Pages of 12 rows into `dir`.
    fn write(&self, dir: &std::path::Path) {
        let cell = cell_side();
        std::fs::create_dir_all(dir).expect("the sheet folder");
        for (page, rows) in self.rows.chunks(12).enumerate() {
            let cols = rows.iter().map(|r| r.1.len()).max().unwrap_or(1) as u32;
            let (w, h) = (cols * (cell + 4), rows.len() as u32 * (cell + 4));
            let mut img = [20u8, 20, 20, 255].repeat((w * h) as usize);
            for (r, (_, cells)) in rows.iter().enumerate() {
                for (c, pixels) in cells.iter().enumerate() {
                    for y in 0..cell {
                        let o = (((r as u32 * (cell + 4) + y) * w + c as u32 * (cell + 4)) * 4) as usize;
                        img[o..o + (cell * 4) as usize].copy_from_slice(&pixels[(y * cell * 4) as usize..((y + 1) * cell * 4) as usize]);
                    }
                }
            }
            let names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
            image::save_buffer(dir.join(format!("templates-{:02}.png", page + 1)), &img, w, h, image::ColorType::Rgba8).expect("the sheet is written");
            std::fs::write(dir.join(format!("templates-{:02}.txt", page + 1)), names.join("\n")).expect("the sheet's names");
        }
    }
}

/// `mask` grown by `r` pixels (a square), separably.
fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let mut row = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            row[y * w + x] = (a..=b).any(|i| mask[y * w + i]);
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        for y in 0..h {
            let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
            out[y * w + x] = (a..=b).any(|j| row[j * w + x]);
        }
    }
    out
}

/// The template with only its text layer `k` left: what that text alone draws.
fn only_text(td: &TitleData, k: usize) -> TitleData {
    let mut t = td.clone();
    t.objects = vec![td.objects[k].clone()];
    t
}

/// Measures one title, `data_for` laying it out for each format, as
/// `name`: its smallest text, its text inside title-safe and (tall) clear
/// of the buttons, its spacing as designed, and its contrast over the
/// three frames. Pushes its sheet row.
fn measure(sweep: &mut Sweep, renderer: &mut TitleRenderer, sheet: &mut Sheet, sheet_on: bool, name: &str, data_for: &dyn Fn(u32, u32) -> TitleData) {
    let mut cells = Vec::new();
    let mut design: Option<Vec<f32>> = None;
    let td = data_for(1920, 1080);
    // Measured in its own faces: a catalogue face not fetched here
    // would measure the fallback (`KLIPP_FONTS_DIR` names a folder of
    // fetched faces).
    let unfetched: Vec<String> = td.objects.iter().filter_map(|o| match &o.content {
        TitleObjectContent::TextBox(tb) => Some(tb.font_name.clone()),
        _ => None,
    }).filter(|f| klipp_title::catalogue::family(f).is_some() && !klipp_title::catalogue::fetched(f)).collect();
    if !unfetched.is_empty() {
        sweep.skip(name, &format!("{} not fetched", unfetched.join(", ")));
        return;
    }
    let texts: Vec<usize> = td.objects.iter().enumerate().filter(|(_, o)| matches!(o.content, TitleObjectContent::TextBox(_))).map(|(i, _)| i).collect();
    if texts.is_empty() {
        sweep.skip(name, "no text");
        return;
    }
    let smallest = td.objects.iter().filter_map(|o| match &o.content {
        TitleObjectContent::TextBox(tb) if !tb.text.trim().is_empty() => Some(tb.font_size),
        _ => None,
    }).fold(f32::MAX, f32::min);
    sweep.check(&format!("{name} smallest text"), format!("{smallest:.0} px of 1080"), smallest >= MIN_SIZE);
    for &(format, w, h) in FORMATS {
        // Laid out for the frame, as the app and the template op make it.
        let td = data_for(w, h);
        let full = renderer.render(&td, w, h);
        let (wu, hu) = (w as usize, h as usize);
        let r = ((w.min(h) as f32) * 0.012).max(3.0) as usize;
        let layers: Vec<Vec<bool>> = texts.iter().map(|&k| renderer.render(&only_text(&td, k), w, h).chunks(4).map(|p| p[3] > 153).collect()).collect();
        // Inside title-safe: no glyph within 5 % of an edge.
        let (mx, my) = (wu / 20, hu / 20);
        let outside = layers.iter().flat_map(|m| m.iter().enumerate()).filter(|(i, on)| **on && {
            let (x, y) = (i % wu, i / wu);
            x < mx || x >= wu - mx || y < my || y >= hu - my
        }).count();
        sweep.check(&format!("{name} {format} title-safe"), format!("{outside} glyph pixels outside"), outside == 0);
        if Format::of(w, h) == Format::Tall {
            // Clear of what the platforms cover on a phone: the band
            // at the bottom and the buttons down the right.
            let (band, strip) = ((hu as f32 * TALL_BAND_TOP) as usize, (wu as f32 * TALL_STRIP_LEFT) as usize);
            let covered = layers.iter().flat_map(|m| m.iter().enumerate()).filter(|(i, on)| **on && {
                let (x, y) = (i % wu, i / wu);
                y >= band || (y >= hu / 2 && x >= strip)
            }).count();
            sweep.check(&format!("{} {format} clear of buttons", name), format!("{covered} glyph pixels covered"), covered == 0);
        }
        // The layout keeps its shape: each text's distance from the
        // next, in the title's type size, is the design's (16:9, the
        // first format) whatever the frame.
        let centres: Vec<(f32, f32)> = layers.iter().filter_map(|m| {
            let on: Vec<usize> = m.iter().enumerate().filter(|(_, g)| **g).map(|(i, _)| i).collect();
            (!on.is_empty()).then(|| {
                let (x0, x1) = on.iter().fold((usize::MAX, 0), |(lo, hi), i| (lo.min(i % wu), hi.max(i % wu)));
                let (y0, y1) = (on[0] / wu, on[on.len() - 1] / wu);
                ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0)
            })
        }).collect();
        let type_px = texts.first().and_then(|&k| match &td.objects[k].content {
            TitleObjectContent::TextBox(tb) => Some(tb.font_size * w.min(h) as f32 / 1080.0),
            _ => None,
        }).unwrap_or(1.0);
        let gaps: Vec<f32> = centres.windows(2).map(|p| (p[1].0 - p[0].0).hypot(p[1].1 - p[0].1) / type_px).collect();
        match &design {
            None => design = Some(gaps),
            Some(want) if !gaps.is_empty() => {
                let worst = gaps.iter().zip(want).map(|(g, d)| (g / d.max(1e-3)).max(d / g.max(1e-3))).fold(1.0f32, f32::max);
                sweep.check(&format!("{} {format} keeps its spacing", name), format!("{worst:.2}x the design's"), worst <= SPACING);
            }
            _ => {}
        }
        for (bg_name, bg) in backgrounds(w, h) {
            let comp = over(&full, &protected(&full, &bg, w, h, td.protect.code()));
            if sheet_on && bg_name == "bright" {
                cells.push(Sheet::cell(&comp, w, h));
            }
            let mut worst = f32::MAX;
            for glyphs in &layers {
                if !glyphs.iter().any(|g| *g) {
                    continue;
                }
                let near = dilate(glyphs, wu, hu, r);
                let (mut lt, mut nt, mut lb, mut nb) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                for i in 0..wu * hu {
                    let l = luminance(&comp[i * 4..i * 4 + 4]);
                    if glyphs[i] {
                        lt += l;
                        nt += 1.0;
                    } else if near[i] {
                        lb += l;
                        nb += 1.0;
                    }
                }
                let (a, b) = (lt / nt.max(1.0), lb / nb.max(1.0));
                worst = worst.min((a.max(b) + 0.05) / (a.min(b) + 0.05));
            }
            if worst == f32::MAX {
                sweep.skip(&format!("{} {format} {bg_name}", name), "no text drawn");
                continue;
            }
            sweep.check(&format!("{} {format} {bg_name}", name), format!("contrast {worst:.2}:1"), worst >= CONTRAST);
        }
    }
    sheet.rows.push((name.to_string(), cells));
}

#[test]
fn sweep_templates() {
    let mut sweep = Sweep::new("templates", KNOWN);
    let mut renderer = TitleRenderer::new();
    let sheet_dir = std::env::var_os("KLIPP_TEMPLATES_SHEET").map(std::path::PathBuf::from);
    let mut sheet = Sheet { rows: Vec::new() };
    for template in TITLES {
        measure(&mut sweep, &mut renderer, &mut sheet, sheet_dir.is_some(), template.name, &|w, h| template.data_for(w, h));
    }
    // The built-in composition templates' titles (#2757): each as placed,
    // its controls at their defaults, in the layout for each frame.
    for t in klipp_timeline::composition_template::builtin_templates() {
        let titles_in = |w: u32, h: u32| -> Vec<TitleData> { placed_titles(&t, w, h) };
        for k in 0..titles_in(1920, 1080).len() {
            let name = if titles_in(1920, 1080).len() == 1 { t.name.clone() } else { format!("{} title {}", t.name, k + 1) };
            measure(&mut sweep, &mut renderer, &mut sheet, sheet_dir.is_some(), &name, &|w, h| titles_in(w, h).swap_remove(k));
        }
    }
    if let Some(dir) = &sheet_dir {
        sheet.write(dir);
    }
    sweep.finish();
}

/// The titles of `t` placed in a `w` × `h` sequence, as its layout for the
/// frame holds them with its parameters and controls at their defaults.
fn placed_titles(t: &klipp_timeline::composition_template::CompositionTemplate, w: u32, h: u32) -> Vec<TitleData> {
    use klipp_timeline::{Clip, ClipKind, Sequence};
    let layout = t.composition_for(w, h).expect("a layout").clone();
    let secs = layout.total_duration();
    let mut clip = Clip::precomposition(layout.id, klipp_core::RationalTime::from_secs_f64(0.0, 24), secs);
    clip.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: klipp_timeline::composition_template::instance_for(t) };
    let mut outer = Sequence::new_default();
    outer.resolution = klipp_core::Resolution { width: w, height: h };
    outer.tracks[0].clips.push(clip);
    let mut all = t.sequences.clone();
    all.push(outer.clone());
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &all);
    let scene = flat.all_clips().find_map(|c| c.precomp.clone()).expect("the scene");
    scene.all_clips().filter_map(|c| c.title_data().cloned()).collect()
}
