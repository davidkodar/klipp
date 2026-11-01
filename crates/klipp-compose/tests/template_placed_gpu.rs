//! Precompositions on the GPU stack (#2757): a precomposition's scene is
//! composed on the stack and seeds its layer, with no fallback, and draws
//! what the CPU path draws; a composition template placed as one clip
//! draws what its composition draws with the same value set by hand.
//! `precomposition_fps_at_4k` reads the frame rate of a 4K timeline with
//! one precomposition. A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::{RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleBackground, TitleData, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

struct Nothing;

impl FrameSource for Nothing {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

/// A card: a headline on a coloured backing.
fn card(text: &str) -> Sequence {
    let mut td = TitleData::single_text(TextBox { text: text.into(), font_size: 120.0, ..Default::default() });
    td.background = TitleBackground::Color { color: [30, 90, 200, 255], padding: 24.0, lines: None };
    let mut t = Track::new("T1", TrackKind::Text);
    t.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(3.0)) }];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![t];
    seq
}

/// `clip` laid in a sequence of its own, flattened with `composition`.
fn outer(clip: Clip, composition: &Sequence) -> Sequence {
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.tracks = vec![v];
    klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), composition.clone()])
}

fn on_gpu(seq: &Sequence) -> Option<Vec<u8>> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()).map(|(px, _)| px)
}

fn on_cpu(seq: &Sequence) -> Vec<u8> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    klipp_effects::linear_f32_rgba_to_srgb_u8(&r.render(seq, tc(1.0), W, H, &mut Nothing, RenderOptions::default()))
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// A plain precomposition: its scene composed on the stack, no fallback,
/// drawn as the CPU path and the scene on its own draw it.
#[test]
fn a_precomposition_is_composed_on_the_gpu() {
    let scene = card("Launch");
    let flat = outer(Clip::precomposition(scene.id, tc(0.0), tc(3.0)), &scene);
    let Some(gpu) = on_gpu(&flat) else {
        assert!(!required(), "required, but the GPU stack declined the precomposition");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let frame = klipp_effects::residency::last_frame();
    assert_eq!(frame.fallback_count(), 0, "composed on the GPU: {:?}", frame.events);
    let alone = on_gpu(&scene).expect("the scene alone");
    assert!(worst(&gpu, &alone) <= 2, "as the scene draws on its own: {} levels", worst(&gpu, &alone));
    let cpu = on_cpu(&flat);
    assert!(worst(&gpu, &cpu) <= 3, "as the CPU path draws it: {} levels", worst(&gpu, &cpu));
}

/// The frame rate of a 4K timeline with one precomposition on the stack,
/// read over a run of frames after a warm one.
#[test]
fn precomposition_fps_at_4k() {
    let (w, h) = (3840u32, 2160u32);
    let mut scene = card("Launch");
    scene.resolution = klipp_core::Resolution { width: w, height: h };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::precomposition(scene.id, tc(0.0), tc(3.0))];
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: w, height: h };
    seq.tracks = vec![v];
    let flat = klipp_timeline::flatten::flatten_sequence(&seq, &[seq.clone(), scene.clone()]);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    if r.render_display_u8_on_gpu(&flat, tc(0.5), w, h, &mut Nothing, RenderOptions::default()).is_none() {
        assert!(!required(), "required, but the GPU stack declined the 4K precomposition");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    }
    let frames = 24;
    let t = std::time::Instant::now();
    for f in 0..frames {
        let at = tc(0.5 + f as f64 / 24.0);
        assert!(r.render_display_u8_on_gpu(&flat, at, w, h, &mut Nothing, RenderOptions::default()).is_some(), "frame {f} on the stack");
        assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "frame {f} without fallback");
    }
    let fps = frames as f64 / t.elapsed().as_secs_f64();
    eprintln!("4K timeline with one precomposition: {fps:.1} fps (display bytes read back each frame)");
    assert!(fps > 5.0, "{fps:.1} fps");
}

#[test]
fn a_placed_template_draws_its_composition_with_the_value_set() {
    // The template: the card, its headline a parameter.
    let made = card("Launch");
    let instance = klipp_core::TemplateInstance {
        template_id: klipp_core::SequenceId::new().0,
        bindings: [("text".to_string(), "object:0.text".to_string())].into_iter().collect(),
        overrides: [("text".to_string(), klipp_core::TemplateValue::Text("Ship it".into()))].into_iter().collect(),
        media_slots: Default::default(),
        links: Vec::new(),
    };
    let mut placed = Clip::precomposition(made.id, tc(0.0), tc(3.0));
    placed.kind = ClipKind::TemplatePrecomposition { sequence_id: made.id, template_instance: instance };
    let Some(placed_px) = on_gpu(&outer(placed, &made)) else {
        assert!(!required(), "required, but the GPU stack declined the placed template");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let frame = klipp_effects::residency::last_frame();
    // The same card with the headline typed in, laid in as it is.
    let typed = card("Ship it");
    let reference = on_gpu(&outer(Clip::precomposition(typed.id, tc(0.0), tc(3.0)), &typed)).expect("the stack drew the card");
    let worst = placed_px.iter().zip(&reference).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(worst <= 2, "the placed template differs from its composition by {worst} levels");
    let blue = placed_px.chunks(4).filter(|p| p[2] > 150 && p[0] < 100).count();
    assert!(blue > 1000, "the card is drawn: {blue} pixels of its backing");
    assert_eq!(frame.fallback_count(), 0, "composed on the GPU: {:?}", frame.events);
}

/// Every media clip a flat orange.
struct Orange;

impl FrameSource for Orange {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let px = [0.8f32, 0.3, 0.05, 1.0].repeat((width * height) as usize);
        Some(SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(px)), width, height, pts: None })
    }
}

/// #2757: a template's media slot, empty, shows its placeholder; filled,
/// the media given; on the GPU stack, no fallback either way.
#[test]
fn a_slot_shows_its_placeholder_until_it_is_filled() {
    let mut scene = Sequence::new_default();
    scene.resolution = klipp_core::Resolution { width: W, height: H };
    scene.tracks[0].clips = vec![Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(3.0), tc(0.0), tc(3.0))];
    let placed = |media: Option<klipp_core::MediaId>| {
        let instance = klipp_core::TemplateInstance {
            template_id: klipp_core::SequenceId::new().0,
            bindings: [("slot_1".to_string(), "clip:0.media".to_string())].into_iter().collect(),
            overrides: Default::default(),
            media_slots: media.map(|m| ("slot_1".to_string(), m.0.to_string())).into_iter().collect(),
            links: Vec::new(),
        };
        let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
        clip.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
        outer(clip, &scene)
    };
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let Some((empty, _)) = r.render_display_u8_on_gpu(&placed(None), tc(1.0), W, H, &mut Orange, RenderOptions::default()) else {
        assert!(!required(), "required, but the GPU stack declined the slot");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "empty: on the GPU");
    let mid = ((H / 2 * W + W / 2) * 4) as usize;
    let p = klipp_timeline::flatten::SLOT_PLACEHOLDER;
    assert!((0..3).all(|c| (empty[mid + c] as i32 - p[c] as i32).abs() <= 2), "the placeholder: {:?}", &empty[mid..mid + 3]);
    let (filled, _) = r.render_display_u8_on_gpu(&placed(Some(klipp_core::MediaId::new())), tc(1.0), W, H, &mut Orange, RenderOptions::default()).expect("the stack drew the filled slot");
    assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "filled: on the GPU");
    assert!(filled[mid] > 200 && filled[mid + 2] < 90, "the media given: {:?}", &filled[mid..mid + 3]);
}

/// #2757: a macro control at 0, 0.5 and 1 drives a placed template's
/// headline opacity on an ease-in curve: each frame is the card with that
/// opacity set by hand, its brightest pixel follows the curve (0, a
/// quarter, full, in display light), on the stack with no fallback.
#[test]
fn a_macro_control_drives_a_placed_templates_opacity_on_the_gpu() {
    let mut made = card("Launch");
    let ClipKind::Title(td) = &mut made.tracks[0].clips[0].kind else { unreachable!() };
    td.background = TitleBackground::None;
    let link = klipp_core::TemplateLink {
        parameter: "fade".into(),
        property: "object:0.opacity".into(),
        from: [0.0, 1.0],
        to: [0.0, 1.0],
        curve: klipp_core::LinkCurve::EaseIn,
        offset: 0.0,
        linked: true,
        frozen: 0.0,
    };
    let srgb = |v: f64| if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 } * 255.0;
    for (control, opacity) in [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)] {
        let instance = klipp_core::TemplateInstance {
            template_id: klipp_core::SequenceId::new().0,
            bindings: Default::default(),
            overrides: [("fade".to_string(), klipp_core::TemplateValue::Number(control))].into_iter().collect(),
            media_slots: Default::default(),
            links: vec![link.clone()],
        };
        let mut placed = Clip::precomposition(made.id, tc(0.0), tc(3.0));
        placed.kind = ClipKind::TemplatePrecomposition { sequence_id: made.id, template_instance: instance };
        let Some(px) = on_gpu(&outer(placed, &made)) else {
            assert!(!required(), "required, but the GPU stack declined the placed template");
            eprintln!("UNVERIFIED: no GPU stack");
            return;
        };
        assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "control {control}: composed on the GPU");
        let mut by_hand = made.clone();
        let ClipKind::Title(td) = &mut by_hand.tracks[0].clips[0].kind else { unreachable!() };
        td.objects[0].opacity = opacity as f32;
        let reference = on_gpu(&outer(Clip::precomposition(by_hand.id, tc(0.0), tc(3.0)), &by_hand)).expect("the stack drew the card");
        assert!(worst(&px, &reference) <= 2, "control {control}: {} levels from opacity {opacity} set by hand", worst(&px, &reference));
        let peak = px.chunks(4).map(|p| p[0]).max().unwrap() as f64;
        eprintln!("control {control}: peak {peak}, expected {:.1}", srgb(opacity));
        assert!((peak - srgb(opacity)).abs() <= 4.0, "control {control}: peak {peak}, the curve says {:.1}", srgb(opacity));
    }
}

/// Each media clip by its media: the first slot's red, any other blue;
/// the red one a wide (16:9) source when `.1`, else a tall (9:16) one,
/// and the blue the other.
struct ByMedia(klipp_core::MediaId, bool);

impl FrameSource for ByMedia {
    fn frame(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        let first = clip.media_id() == Some(self.0);
        let c = if first { [0.8f32, 0.05, 0.05, 1.0] } else { [0.05f32, 0.1, 0.8, 1.0] };
        let (w, h) = if first == self.1 { (320u32, 180u32) } else { (180, 320) };
        Some(SourceFrame { pixels: klipp_compose::SourcePixels::Linear(std::sync::Arc::new(c.repeat((w * h) as usize))), width: w, height: h, pts: None })
    }
}

/// #2757: the shipped split screen, placed in a wide, a tall and a square
/// sequence, draws its layout for each: the first picture in the left
/// half (the top one on a tall frame), the second in the other, the line
/// between them, whether each is wide or tall footage, on the stack with
/// no fallback.
#[test]
fn the_split_screen_lays_out_for_each_frame_on_the_gpu() {
    let split = klipp_timeline::composition_template::builtin_templates().into_iter().find(|t| t.name == "Split Screen").expect("shipped");
    let (first, second) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
    for ((w, h), first_wide) in [(1920u32, 1080u32), (1080, 1920), (1080, 1080)].into_iter().flat_map(|f| [(f, true), (f, false)]) {
        let layout = split.composition_for(w, h).unwrap().clone();
        let mut instance = klipp_timeline::composition_template::instance_for(&split);
        instance.media_slots.insert("slot_1".into(), first.0.to_string());
        instance.media_slots.insert("slot_2".into(), second.0.to_string());
        let mut clip = Clip::precomposition(layout.id, tc(0.0), tc(3.0));
        clip.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: instance };
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = vec![clip];
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: w, height: h };
        seq.tracks = vec![v];
        let mut all = split.sequences.clone();
        all.push(seq.clone());
        let flat = klipp_timeline::flatten::flatten_sequence(&seq, &all);
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        let Some((px, _)) = r.render_display_u8_on_gpu(&flat, tc(1.0), w, h, &mut ByMedia(first, first_wide), RenderOptions::default()) else {
            assert!(!required(), "required, but the GPU stack declined the split screen");
            eprintln!("UNVERIFIED: no GPU stack");
            return;
        };
        assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "{w}x{h}: on the GPU");
        let at = |x: u32, y: u32| { let i = ((y * w + x) * 4) as usize; [px[i], px[i + 1], px[i + 2]] };
        let (a, b, mid) = if h > w { (at(w / 2, h / 4), at(w / 2, 3 * h / 4), at(w / 4, h / 2)) } else { (at(w / 4, h / 2), at(3 * w / 4, h / 2), at(w / 2, h / 4)) };
        eprintln!("{w}x{h} (first wide: {first_wide}): first {a:?}, second {b:?}, line {mid:?}");
        assert!(a[0] > 150 && a[2] < 100, "{w}x{h}: the first picture where it goes: {a:?}");
        assert!(b[2] > 150 && b[0] < 100, "{w}x{h}: the second where it goes: {b:?}");
        // Each half covered to its far corners, nothing of the other in it.
        let corners = if h > w { [(4, 4, true), (w - 5, h / 2 - 8, true), (4, h / 2 + 8, false), (w - 5, h - 5, false)] } else { [(4, 4, true), (w / 2 - 8, h - 5, true), (w / 2 + 8, 4, false), (w - 5, h - 5, false)] };
        for (x, y, is_first) in corners {
            let p = at(x, y);
            assert!(if is_first { p[0] > 150 && p[2] < 100 } else { p[2] > 150 && p[0] < 100 }, "{w}x{h} (first wide: {first_wide}): ({x}, {y}) shows {p:?}");
        }
        assert!(mid.iter().all(|c| *c > 200), "{w}x{h}: the line between them: {mid:?}");
    }
}

/// #2757: every built-in template, placed in a wide, a tall and a square
/// sequence with its picture slots filled, draws on the stack with no
/// fallback, and each slot's picture is seen in the frame.
#[test]
fn every_built_in_template_draws_on_the_gpu_in_every_frame() {
    for t in klipp_timeline::composition_template::builtin_templates() {
        for (w, h) in [(960u32, 540u32), (540, 960), (540, 540)] {
            let layout = t.composition_for(w, h).unwrap().clone();
            let mut instance = klipp_timeline::composition_template::instance_for(&t);
            let media = klipp_core::MediaId::new();
            for slot in t.slots.keys() {
                instance.media_slots.insert(slot.clone(), media.0.to_string());
            }
            let secs = layout.total_duration().to_secs_f64();
            let mut clip = Clip::precomposition(layout.id, tc(0.0), tc(secs));
            clip.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: instance };
            let mut v = Track::new("V1", TrackKind::Video);
            v.clips = vec![clip];
            let mut seq = Sequence::new_default();
            seq.resolution = klipp_core::Resolution { width: w, height: h };
            seq.tracks = vec![v];
            let mut all = t.sequences.clone();
            all.push(seq.clone());
            let flat = klipp_timeline::flatten::flatten_sequence(&seq, &all);
            let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(true);
            // At rest, or a stinger mid-sweep.
            let at = if t.name == "Stinger" { 0.2 } else { (secs * 0.6).min(3.0) };
            let Some((px, _)) = r.render_display_u8_on_gpu(&flat, tc(at), w, h, &mut Orange, RenderOptions::default()) else {
                assert!(!required(), "required, but the GPU stack declined {} at {w}x{h}", t.name);
                eprintln!("UNVERIFIED: no GPU stack");
                return;
            };
            assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "{} {w}x{h}: on the GPU", t.name);
            let lit = px.chunks(4).filter(|p| p.iter().take(3).any(|c| *c > 30)).count();
            assert!(lit > 200, "{} {w}x{h}: something drawn ({lit} pixels)", t.name);
            if !t.slots.is_empty() {
                // Its hue, dimmed or not (a hook dims its picture).
                let orange = px.chunks(4).filter(|p| p[0] > 60 && p[0] as u32 > 2 * p[2] as u32 && p[0] > p[1]).count();
                assert!(orange > (w * h / 20) as usize, "{} {w}x{h}: its slots' picture seen ({orange} pixels)", t.name);
            }
        }
    }
}

/// #3304: Glow Caps placed with its colour set and its glow at full or
/// none, drawn at rest on the stack and on the CPU path alike: the words
/// in the colour chosen, and the glow lighting round them only with the
/// glow up. Set in the bundled face: Montserrat is fetched on use, and
/// the colour and glow are what this measures.
#[test]
fn glow_caps_takes_its_colour_and_glow_on_both_routes() {
    let t = klipp_timeline::composition_template::builtin_templates().into_iter().find(|t| t.name == "Glow Caps").expect("shipped");
    let peach = [0xff as f32 / 255.0, 0xe0 as f32 / 255.0, 0xc8 as f32 / 255.0, 1.0];
    let draw = |glow: f64| -> Option<(usize, usize)> {
        let mut layout = t.composition_for(W, H).unwrap().clone();
        for td in layout.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).filter_map(|c| match &mut c.kind { ClipKind::Title(td) => Some(td), _ => None }) {
            for o in &mut td.objects {
                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
                    tb.font_name = String::new();
                }
            }
        }
        let mut instance = klipp_timeline::composition_template::instance_for(&t);
        instance.overrides.insert("colour".into(), klipp_core::TemplateValue::Color(peach));
        instance.overrides.insert("glow".into(), klipp_core::TemplateValue::Number(glow));
        let secs = layout.total_duration().to_secs_f64();
        let mut clip = Clip::precomposition(layout.id, tc(0.0), tc(secs));
        clip.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: instance };
        let seq = outer(clip, &layout);
        let at = tc(3.0);
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(true);
        let (stack, _) = r.render_display_u8_on_gpu(&seq, at, W, H, &mut Nothing, RenderOptions::default())?;
        assert_eq!(klipp_effects::residency::last_frame().fallback_count(), 0, "on the GPU");
        let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        cpu.set_gpu_enabled(false);
        let want = klipp_effects::linear_f32_rgba_to_srgb_u8(&cpu.render(&seq, at, W, H, &mut Nothing, RenderOptions::default()));
        let worst = stack.iter().zip(&want).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
        assert!(worst <= 3, "glow {glow}: the stack differs from the CPU path by {worst} levels");
        // Peach ink: red full, blue well under it; and anything lit.
        let ink = stack.chunks(4).filter(|p| p[0] > 240 && p[2] > 170 && p[2] < 215).count();
        let lit = stack.chunks(4).filter(|p| p[0] > 24).count();
        Some((ink, lit))
    };
    let Some((ink, bare)) = draw(0.0) else {
        assert!(!required(), "required, but the GPU stack declined Glow Caps");
        eprintln!("UNVERIFIED: no GPU stack");
        return;
    };
    let (_, glowing) = draw(1.0).unwrap();
    assert!(ink > 200, "the words in the colour chosen: {ink} pixels");
    assert!(glowing > bare * 3 / 2, "the glow lights round the words: {bare} bare, {glowing} glowing");
}
