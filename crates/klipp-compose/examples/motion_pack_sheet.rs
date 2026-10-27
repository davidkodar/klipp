//! A contact sheet of the built-in composition templates (#2757): each in
//! 16:9, 9:16 and 1:1, drawn on the GPU stack over a shot, its picture
//! slots and photo fields filled with frames of the clips given. An
//! authoring aid for reviewing the set; nothing in Klipp runs it.
//!
//! `cargo run -p klipp-compose --example motion_pack_sheet -- <clips dir> <out dir>`
//!
//! The clips are named below; use only footage that may be shown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime, Resolution, TemplateValue, Timecode};
use klipp_timeline::composition_template::{builtin_templates, instance_for};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// The shot under every template, then the fills for its slots, in turn.
const UNDER: (&str, f64) = ("12-sunset-sea.mp4", 6.0);
const FILLS: [(&str, f64); 4] = [("06-market-fruit.mp4", 5.0), ("07-promenade-morning-run.mp4", 5.0), ("09-cyclists-street.mp4", 4.0), ("02-harbour-town-calm.mp4", 8.0)];
const PHOTO: (&str, f64) = ("talking-01-promenade-man.mp4", 4.0);

const FORMATS: [(u32, u32); 3] = [(960, 540), (540, 960), (540, 540)];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// One frame of `file` at `secs`, sRGB RGBA at `w` × `h`.
fn grab(file: &Path, secs: f64, w: u32, h: u32) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-ss", &format!("{secs}")])
        .arg("-i")
        .arg(file)
        .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .expect("ffmpeg runs");
    assert_eq!(out.stdout.len(), (w * h * 4) as usize, "a frame of {}", file.display());
    out.stdout
}

fn linear(srgb: &[u8]) -> Vec<f32> {
    let lin = |c: u8| {
        let v = c as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    srgb.chunks(4).flat_map(|p| [lin(p[0]), lin(p[1]), lin(p[2]), p[3] as f32 / 255.0]).collect()
}

/// Each media clip's frame, by its media.
struct Frames(HashMap<MediaId, Arc<Vec<f32>>>);

impl FrameSource for Frames {
    fn frame(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        let px = self.0.get(&clip.media_id()?)?.clone();
        Some(SourceFrame { pixels: SourcePixels::Linear(px), width: 1280, height: 720, pts: None })
    }
}

/// When to look at a template: at rest, or mid-move for one that only moves.
fn when(name: &str) -> f64 {
    match name {
        "Countdown" => 0.7,
        "Stinger" => 0.3,
        _ => 3.0,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (clips, out) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    std::fs::create_dir_all(&out).expect("the output folder");
    // The photo field's picture, a still of its own.
    let photo = out.join("photo.png");
    image::save_buffer(&photo, &grab(&clips.join(PHOTO.0), PHOTO.1, 720, 720), 720, 720, image::ColorType::Rgba8).expect("the photo");
    let under = MediaId::new();
    let fills: Vec<MediaId> = FILLS.iter().map(|_| MediaId::new()).collect();
    let mut frames = HashMap::new();
    frames.insert(under, Arc::new(linear(&grab(&clips.join(UNDER.0), UNDER.1, 1280, 720))));
    for (id, (file, secs)) in fills.iter().zip(FILLS) {
        frames.insert(*id, Arc::new(linear(&grab(&clips.join(file), secs, 1280, 720))));
    }
    let mut source = Frames(frames);
    let templates = builtin_templates();
    // Every catalogue face the templates set, fetched, as Klipp fetches one
    // on use.
    let mut families: Vec<String> = templates.iter().flat_map(|t| &t.sequences).flat_map(|s| s.all_clips()).filter_map(|c| c.title_data()).flat_map(|td| td.objects.iter()).filter_map(|o| match &o.content {
        klipp_timeline::TitleObjectContent::TextBox(tb) => Some(tb.font_name.clone()),
        _ => None,
    }).collect();
    families.sort();
    families.dedup();
    for f in families.iter().filter_map(|f| klipp_title::catalogue::family(f)).filter(|f| !klipp_title::catalogue::is_fetched(f)) {
        klipp_title::catalogue::fetch(f).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        println!("fetched {}", f.name);
    }
    // A row a template: the three formats at half size, side by side.
    let (row_h, gap) = (480u32, 8u32);
    let widths: Vec<u32> = FORMATS.iter().map(|(w, _)| w / 2).collect();
    let sheet_w = widths.iter().sum::<u32>() + gap * (FORMATS.len() as u32 + 1);
    let sheet_h = (row_h + gap) * templates.len() as u32 + gap;
    let mut sheet = image::RgbaImage::from_pixel(sheet_w, sheet_h, image::Rgba([24, 24, 28, 255]));
    let mut index = String::from("# Motion pack contact sheet\n\nRows, top to bottom; columns 16:9, 9:16, 1:1.\n\n");
    let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    renderer.set_gpu_enabled(true);
    for (row, t) in templates.iter().enumerate() {
        let mut fallbacks = 0;
        let mut x = gap;
        for (w, h) in FORMATS {
            let layout = t.composition_for(w, h).expect("a layout").clone();
            let mut instance = instance_for(t);
            for (n, slot) in t.slots.keys().enumerate() {
                instance.media_slots.insert(slot.clone(), fills[n % fills.len()].0.to_string());
            }
            for (name, p) in &t.parameters {
                if matches!(p.default, TemplateValue::Text(_)) && p.label == "Photo" {
                    instance.overrides.insert(name.clone(), TemplateValue::Text(photo.to_string_lossy().into_owned()));
                }
            }
            let secs = layout.total_duration().to_secs_f64();
            let mut placed = Clip::precomposition(layout.id, tc(0.0), tc(secs));
            placed.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: instance };
            let mut v1 = Track::new("V1", TrackKind::Video);
            // The shot covering the frame, whatever its shape.
            let mut shot = Clip::media(under, tc(0.0), tc(secs), tc(0.0), tc(secs));
            shot.push_effect(klipp_effects::BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: w as f32 / h as f32, focus_x: 0.5, focus_y: 0.5 });
            v1.clips = vec![shot];
            let mut v2 = Track::new("V2", TrackKind::Video);
            v2.clips = vec![placed];
            let mut seq = Sequence::new_default();
            seq.resolution = Resolution { width: w, height: h };
            seq.tracks = vec![v2, v1];
            let mut all = t.sequences.clone();
            all.push(seq.clone());
            let flat = klipp_timeline::flatten::flatten_sequence(&seq, &all);
            let (px, _) = renderer.render_display_u8_on_gpu(&flat, tc(when(&t.name).min(secs - 0.05)), w, h, &mut source, RenderOptions::default()).expect("the GPU stack drew it");
            fallbacks += klipp_effects::residency::last_frame().fallback_count();
            let img = image::RgbaImage::from_raw(w, h, px).expect("a frame");
            let small = image::imageops::resize(&img, w / 2, h / 2, image::imageops::FilterType::Triangle);
            image::imageops::overlay(&mut sheet, &small, x as i64, (gap + row as u32 * (row_h + gap)) as i64);
            x += w / 2 + gap;
        }
        index.push_str(&format!("{}. {} ({}), {} fallbacks\n", row + 1, t.name, t.category, fallbacks));
        println!("{} ({}): {fallbacks} fallbacks", t.name, t.category);
    }
    sheet.save(out.join("motion-pack-sheet.png")).expect("the sheet");
    index.push_str(&format!(
        "\nFootage, all from the Showcase originals (credits.md: Coverr and Mixkit free licences): under every template {}; slots {}; photo field {}.\n",
        UNDER.0,
        FILLS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", "),
        PHOTO.0
    ));
    std::fs::write(out.join("README.md"), index).expect("the index");
    println!("wrote {}", out.join("motion-pack-sheet.png").display());
}
