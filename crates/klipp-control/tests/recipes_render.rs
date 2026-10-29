//! The recipe harness (#2758): every built-in recipe placed on a test
//! shot through the route the recipe op takes (`recipes::on_clip`, the
//! graph batch one undo step applies), then drawn on the GPU display
//! stack as the viewer draws it, with nothing falling back, within a
//! level of the CPU, and changed by the recipe. Each recipe's own checks
//! are data in its file (`graph.checks`), read here through the
//! viewer's channel view: a new recipe is a new file, never new code.

use std::collections::BTreeMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, TemplateValue, ViewChannel, WorkingColorspace};
use klipp_nodes::side_picture::SideMissing;
use klipp_timeline::composition_template::{builtin_recipes, SlotKind};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 160;
const H: u32 = 90;
const PICTURE: &str = "2f6a9c13-5e8b-4d07-a1c4-8b3e6f9d2a50";
const GREY: f32 = 0.3;
const GREEN: [f32; 3] = [0.02, 0.75, 0.02];

/// The screen tracked on the test shot, still, on its grey left of the
/// patch.
const SCREEN: klipp_timeline::surface::Quad = [[0.05, 0.15], [0.25, 0.2], [0.24, 0.8], [0.06, 0.75]];

/// The test shot's media, which the media reader reads too (a clean
/// frame, #2758).
const SHOT_MEDIA: &str = "9d3f6b28-4a1e-4c75-8e02-6b9a3d7f1c54";

/// The test shot: grey, from half a second on a green patch over its
/// middle (before, its clean frame); a media slot's picture is red.
struct Shot;

/// The test shot at source `secs`.
fn shot(secs: f64, w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let (fx, fy) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            if secs >= 0.5 && (0.3..0.7).contains(&fx) && (0.3..0.7).contains(&fy) { [GREEN[0], GREEN[1], GREEN[2], 1.0] } else { [GREY, GREY, GREY, 1.0] }
        })
        .collect()
}

/// The test shot's clip, carrying the screen as a tracked surface named
/// "Screen".
fn shot_clip() -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId(SHOT_MEDIA.parse().unwrap()) }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let key = |secs| klipp_timeline::SurfaceKey::at_corners(secs, &SCREEN, &SCREEN).unwrap();
    c.surfaces.push(klipp_timeline::Surface {
        id: klipp_core::ClipId::new().0,
        name: "Screen".into(),
        corners: SCREEN,
        reference_secs: 0.0,
        keys: vec![key(0.0), key(10.0)],
        source_size: [W, H],
        media_fps: 0.0,
        track_area: None,
        exclude: Vec::new(),
    });
    c
}

/// Whether (x, y) is inside the screen, `margin` (fractions of the
/// frame) clear of its edges either way: `Some(inside)`, `None` near one.
fn in_screen(x: f32, y: f32, margin: f32) -> Option<bool> {
    let side = |a: [f32; 2], b: [f32; 2]| {
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        (ex * (y - a[1]) - ey * (x - a[0])) / ex.hypot(ey)
    };
    let d = (0..4).map(|i| side(SCREEN[i], SCREEN[(i + 1) % 4])).fold(f32::INFINITY, f32::min);
    (d.abs() > margin).then_some(d > 0.0)
}

/// Whether (x, y), fractions of the frame, is on the test shot's subject:
/// the known mask its subject matte holds.
fn on_subject(x: f32, y: f32) -> bool {
    (0.55..0.85).contains(&x) && (0.2..0.9).contains(&y)
}

/// The test shot's subject matte (#2758): a matte sequence, as the subject
/// matte pass leaves one, of the known mask at every frame of the clip.
fn subject_dir() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use klipp_nodes::matte_sequence as seq;
        let dir = std::env::temp_dir().join(format!("klipp-recipe-subject-{}", std::process::id()));
        let manifest = seq::Manifest { format: seq::FORMAT, fps: 24.0, width: W, height: H, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: false };
        seq::create(&dir, &manifest).unwrap();
        let alpha: Vec<u8> = (0..W * H).map(|i| if on_subject((i % W) as f32 / W as f32, (i / W) as f32 / H as f32) { 255 } else { 0 }).collect();
        for f in 0..=48 {
            seq::put(&dir, &manifest, seq::key_of(f as f64 / 24.0), &alpha).unwrap();
        }
        dir
    })
    .clone()
}

impl FrameSource for Shot {
    fn subject_matte_dir(&mut self, _clip: &Clip, _model: u32) -> Option<std::path::PathBuf> {
        Some(subject_dir())
    }

    fn frame(&mut self, _c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(shot(secs, w, h))), width: w, height: h, pts: None })
    }

    fn media_frame(&mut self, media: &str, secs: f64, _working: WorkingColorspace, w: u32, h: u32) -> Result<Vec<f32>, SideMissing> {
        match media {
            PICTURE => Ok((0..w * h).flat_map(|_| [0.9, 0.03, 0.03, 1.0]).collect()),
            SHOT_MEDIA => Ok(shot(secs, w, h)),
            _ => Err(SideMissing::File(format!("{media}.mov"))),
        }
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

fn sequence(c: Clip) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![c];
    seq.tracks = vec![track];
    seq
}

fn cpu(seq: &Sequence, channel: ViewChannel) -> Vec<u8> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    let (pixels, _) = r.render_inspected(seq, tc(1.0), W, H, &mut Shot, RenderOptions { view_channel: channel, ..Default::default() }, Inspect::default());
    klipp_color::display::display_transform_ws_to(&pixels, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

/// `seq` on the GPU display stack in `channel`, nothing fallen back;
/// `None` when the stack declined and no GPU is required.
fn stack(seq: &Sequence, channel: ViewChannel, name: &str) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(1.0), W, H, &mut Shot, RenderOptions { view_channel: channel, ..Default::default() });
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "{name}: the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: {name}: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "{name}: nothing falls back: {:?}", fell.events);
    Some(bytes)
}

fn worst(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0)
}

/// What the harness fills a slot of `kind` with.
fn filled(kind: SlotKind) -> TemplateValue {
    match kind {
        SlotKind::Media => TemplateValue::Text(PICTURE.into()),
        SlotKind::Colour => {
            let srgb = |v: f32| klipp_color::transfer::linear_to_srgb(v);
            TemplateValue::Color([srgb(GREEN[0]), srgb(GREEN[1]), srgb(GREEN[2]), 1.0])
        }
        // A title the size of the shot, as the title op makes it.
        SlotKind::Title => TemplateValue::Text(serde_json::json!({ "text": "BEHIND", "size": 600 }).to_string()),
        SlotKind::SubjectMatte => TemplateValue::Text("Subject".into()),
        SlotKind::Surface => TemplateValue::Text("Screen".into()),
        other => panic!("the harness has no {other:?} to fill a slot with yet"),
    }
}

#[test]
fn every_built_in_recipe_draws_on_the_gpu_and_passes_its_checks() {
    let recipes = builtin_recipes();
    assert!(!recipes.is_empty());
    for recipe in &recipes {
        let name = recipe.name.as_str();
        let checks = &recipe.graph.as_ref().unwrap().checks;
        assert!(!checks.is_empty(), "{name} says what it does: give it checks");
        let plain = shot_clip();
        let before = sequence(plain.clone());
        let mut values: BTreeMap<String, TemplateValue> = recipe.slots.iter().map(|(n, s)| (n.clone(), filled(s.kind))).collect();
        // The controls its checks are measured with, where its file names
        // them.
        values.extend(recipe.graph.as_ref().unwrap().check_with.iter().map(|(n, v)| (n.clone(), TemplateValue::Number(*v))));
        let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut placed = plain.clone();
        placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{name}: {e}"));
        let seq = sequence(placed);

        let want = cpu(&seq, ViewChannel::Picture);
        assert!(worst(&want, &cpu(&before, ViewChannel::Picture)) > 30, "{name} changes the picture");
        let Some(shown) = stack(&seq, ViewChannel::Picture, name) else { continue };
        assert!(worst(&want, &shown) <= 1, "{name}: the stack draws {} levels from the CPU", worst(&want, &shown));

        for check in checks {
            let Some(frame) = stack(&seq, check.channel, name) else { continue };
            let (x, y) = ((check.at[0] * W as f32) as u32, (check.at[1] * H as f32) as u32);
            let v = frame[((y * W + x) * 4) as usize] as f32 / 255.0;
            assert!((check.min..=check.max).contains(&v), "{name}: {} ({:?} at {:?} reads {v:.3}, not {}..{})", check.says, check.channel, check.at, check.min, check.max);
        }
    }
}

/// A masked merge (#2758) by a window, not a key: the picture laid only
/// inside the window, the clip as it was outside, on the GPU stack with
/// nothing falling back and within a level of the CPU. A recipe made here
/// for the test, placed the way the recipe op places one.
#[test]
fn a_merge_masked_by_a_window_lays_its_picture_inside_it() {
    let recipe: klipp_timeline::composition_template::CompositionTemplate = serde_json::from_value(serde_json::json!({
        "format_version": 2, "id": "7f0e3c2a-6b1d-4e95-8a47-1c9d2b3e4f50", "name": "Windowed picture", "kind": "recipe",
        "composition_id": "00000000-0000-0000-0000-000000000000",
        "slots": { "picture": { "label": "Picture", "kind": "media" } },
        "bindings": [{ "parameter": "picture", "property": "node:1.media" }],
        "graph": { "ops": [
            { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.merge", "params": { "blend_mode": "Normal" } },
            { "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.fx.media_source" },
            { "op": "connect", "from": { "anchor": "source" }, "from_port": "out", "to": { "new": 1 }, "to_port": "in" },
            { "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.grade.mask",
              "params": { "masks": { "masks": [{ "shape": { "Rectangle": { "center_x": 0.25, "center_y": 0.5, "width": 0.3, "height": 0.6, "rotation": 0 } }, "feather": 0, "opacity": 1, "invert": false, "enabled": true }] } } },
            { "op": "connect", "from": { "anchor": "source" }, "from_port": "out", "to": { "new": 3 }, "to_port": "in" },
            { "op": "connect", "from": { "new": 1 }, "from_port": "out", "to": { "new": 0 }, "to_port": "fg" },
            { "op": "connect", "from": { "new": 3 }, "from_port": "out", "to": { "new": 0 }, "to_port": "mask" }
        ] }
    })).unwrap();
    recipe.validate().unwrap();
    let plain = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let before = sequence(plain.clone());
    let values = BTreeMap::from([("picture".to_string(), filled(SlotKind::Media))]);
    let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, &recipe, &values).unwrap_or_else(|e| panic!("{e}"));
    let mut placed = plain.clone();
    placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    let seq = sequence(placed);
    let want = cpu(&seq, ViewChannel::Picture);
    let Some(shown) = stack(&seq, ViewChannel::Picture, "windowed merge") else { return };
    assert!(worst(&want, &shown) <= 1, "the stack draws {} levels from the CPU", worst(&want, &shown));
    let at = |f: &[u8], x: f32, y: f32| { let i = (((y * H as f32) as u32 * W + (x * W as f32) as u32) * 4) as usize; [f[i], f[i + 1], f[i + 2]] };
    let inside = at(&shown, 0.25, 0.5);
    assert!(inside[0] > 200 && inside[1] < 60, "the picture inside the window: {inside:?}");
    let outside = at(&shown, 0.75, 0.15);
    let clip = at(&cpu(&before, ViewChannel::Picture), 0.75, 0.15);
    assert!(outside.iter().zip(clip).all(|(a, b)| a.abs_diff(b) <= 1), "the clip as it was outside: {outside:?} against {clip:?}");
}

/// A window placed by controls (#2886): the recipe's mask rectangle set
/// from numbers inside its parameter, the picture laid where they put it
/// and not where the recipe file drew it, on the GPU stack within a level
/// of the CPU.
#[test]
fn a_window_placed_by_its_controls_lays_its_picture_there() {
    let recipe: klipp_timeline::composition_template::CompositionTemplate = serde_json::from_value(serde_json::json!({
        "format_version": 2, "id": "5c2e8a41-7d3b-4f19-b6e0-9a1c4d7f2e85", "name": "Placed window", "kind": "recipe",
        "composition_id": "00000000-0000-0000-0000-000000000000",
        "parameters": {
            "x": { "label": "Across", "default": { "type": "Number", "value": 0.25 }, "range": { "min": 0.0, "max": 1.0 } },
            "y": { "label": "Down", "default": { "type": "Number", "value": 0.5 }, "range": { "min": 0.0, "max": 1.0 } }
        },
        "slots": { "picture": { "label": "Picture", "kind": "media" } },
        "bindings": [
            { "parameter": "picture", "property": "node:1.media" },
            { "parameter": "x", "property": "node:3.masks/masks/0/shape/Rectangle/center_x" },
            { "parameter": "y", "property": "node:3.masks/masks/0/shape/Rectangle/center_y" }
        ],
        "graph": { "ops": [
            { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.merge", "params": { "blend_mode": "Normal" } },
            { "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.fx.media_source" },
            { "op": "connect", "from": { "anchor": "source" }, "from_port": "out", "to": { "new": 1 }, "to_port": "in" },
            { "op": "add_beside", "at": { "anchor": "source" }, "type": "klipp.grade.mask",
              "params": { "masks": { "masks": [{ "shape": { "Rectangle": { "center_x": 0.25, "center_y": 0.5, "width": 0.3, "height": 0.4, "rotation": 0 } }, "feather": 0, "opacity": 1, "invert": false, "enabled": true }] } } },
            { "op": "connect", "from": { "anchor": "source" }, "from_port": "out", "to": { "new": 3 }, "to_port": "in" },
            { "op": "connect", "from": { "new": 1 }, "from_port": "out", "to": { "new": 0 }, "to_port": "fg" },
            { "op": "connect", "from": { "new": 3 }, "from_port": "out", "to": { "new": 0 }, "to_port": "mask" }
        ] }
    })).unwrap();
    recipe.validate().unwrap();
    let plain = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let before = sequence(plain.clone());
    let values = BTreeMap::from([
        ("picture".to_string(), filled(SlotKind::Media)),
        ("x".to_string(), TemplateValue::Number(0.75)),
        ("y".to_string(), TemplateValue::Number(0.3)),
    ]);
    let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, &recipe, &values).unwrap_or_else(|e| panic!("{e}"));
    let mut placed = plain.clone();
    placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    let seq = sequence(placed);
    let want = cpu(&seq, ViewChannel::Picture);
    let Some(shown) = stack(&seq, ViewChannel::Picture, "window placed by controls") else { return };
    assert!(worst(&want, &shown) <= 1, "the stack draws {} levels from the CPU", worst(&want, &shown));
    let at = |f: &[u8], x: f32, y: f32| { let i = (((y * H as f32) as u32 * W + (x * W as f32) as u32) * 4) as usize; [f[i], f[i + 1], f[i + 2]] };
    let inside = at(&shown, 0.75, 0.3);
    assert!(inside[0] > 200 && inside[1] < 60, "the picture where the controls put the window: {inside:?}");
    let plain_frame = cpu(&before, ViewChannel::Picture);
    let drawn = at(&shown, 0.25, 0.5);
    assert!(drawn.iter().zip(at(&plain_frame, 0.25, 0.5)).all(|(a, b)| a.abs_diff(b) <= 1), "not where the file drew it: {drawn:?}");
}

/// Keys from a recipe (#2886): a blur's radius keyed from none to a
/// control's value across the clip, the frames apart in time drawn apart,
/// the stack within a level of the CPU where they meet.
#[test]
fn a_recipe_keys_a_parameter_over_the_clip() {
    let recipe: klipp_timeline::composition_template::CompositionTemplate = serde_json::from_value(serde_json::json!({
        "format_version": 2, "id": "8e4a1d27-3c6b-4f95-a2d0-7b9e1c5f3a64", "name": "Blur in", "kind": "recipe",
        "composition_id": "00000000-0000-0000-0000-000000000000",
        "parameters": { "most": { "label": "Most", "default": { "type": "Number", "value": 6.0 }, "range": { "min": 0.0, "max": 40.0 } } },
        "bindings": [{ "parameter": "most", "property": "node:0.radius#1.value" }],
        "graph": { "ops": [
            { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.gaussian_blur" },
            { "op": "keys", "path": { "new": 0 }, "key": "radius", "keys": [{ "time_secs": 0.6, "value": 0.0 }, { "time_secs": 1.4, "value": 0.0 }] }
        ] }
    })).unwrap();
    recipe.validate().unwrap();
    let plain = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let before = sequence(plain.clone());
    let values = BTreeMap::from([("most".to_string(), TemplateValue::Number(20.0))]);
    let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, &recipe, &values).unwrap_or_else(|e| panic!("{e}"));
    let mut placed = plain.clone();
    placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    let seq = sequence(placed);
    let Some(early) = stack_at(&seq, 0.6, "keyed blur") else { return };
    let late = stack_at(&seq, 1.5, "keyed blur").expect("drawn once already");
    let sharp = stack_at(&before, 0.6, "plain").expect("drawn once already");
    assert!(worst(&early, &sharp) <= 1, "no blur at the first key: {} levels", worst(&early, &sharp));
    assert!(worst(&late, &early) > 30, "blurred by the last key: {} levels apart", worst(&late, &early));
    let shown = stack(&seq, ViewChannel::Picture, "keyed blur").expect("drawn once already");
    assert!(worst(&cpu(&seq, ViewChannel::Picture), &shown) <= 1, "the stack mid-way, against the CPU");
}

/// Redact as a recipe (#2886) draws what the redact op drew before it:
/// a mosaic (or a blur) of the box with its margin, from `from` to `to`,
/// against the cropped copy of the clip the op used to lay on a track
/// above, on the GPU stack within a level; outside the stretch, the plain
/// clip. The box crosses the patch's edge, so the blocks mix two colours.
/// Its edges lie on whole pixels.
#[test]
fn redact_draws_what_its_copy_on_a_track_above_drew() {
    use klipp_effects::BuiltinEffect as E;
    // On whole pixels: at an edge part-way through a pixel, the crop
    // rounded it out and the window counts it in when mostly covered.
    let (x0, x1, y0, y1) = (0.25f32, 0.55f32, 0.2f32, 0.5f32);
    for (name, fx) in [("Redact (mosaic)", E::Mosaic { block_size: 36.0 }), ("Redact (blur)", E::GaussianBlur { radius: 35.0 })] {
        let recipe = builtin_recipes().into_iter().find(|r| r.name == name).expect(name);
        let plain = shot_clip();
        let before = sequence(plain.clone());
        let n = |v: f32| TemplateValue::Number(f64::from(v));
        let values = BTreeMap::from([
            ("amount".to_string(), n(0.5)),
            ("centre_x".to_string(), n((x0 + x1) / 2.0)),
            ("centre_y".to_string(), n((y0 + y1) / 2.0)),
            ("width".to_string(), n(x1 - x0)),
            // Along the frame's width, as a window measures its height.
            ("height".to_string(), n((y1 - y0) * H as f32 / W as f32)),
            ("start".to_string(), TemplateValue::Number(0.8 - 0.001)),
            ("from".to_string(), TemplateValue::Number(0.8)),
            ("to".to_string(), TemplateValue::Number(1.2)),
        ]);
        let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, &recipe, &values).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut placed = plain.clone();
        placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{name}: {e}"));
        let now = sequence(placed);
        // As the op made it before: the same footage from 0.8 to 1.2 s,
        // hidden then cut to the box, on a track above.
        let mut cover = Clip { kind: plain.kind.clone(), ..Clip::gap(tc(0.8), tc(1.2)) };
        cover.source_in = tc(0.8);
        cover.source_out = tc(1.2);
        let crop = E::Crop { left: x0, right: 1.0 - x1, top: y0, bottom: 1.0 - y1, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5 };
        cover.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&fx), klipp_timeline::EffectEdit::Add(&crop)]).unwrap().unwrap();
        let mut was = sequence(plain.clone());
        let mut above = Track::new("V2", TrackKind::Video);
        above.clips = vec![cover];
        was.tracks.insert(0, above);
        let Some(drawn) = stack_at(&now, 1.0, name) else { return };
        let old = stack_at(&was, 1.0, name).expect("drawn once already");
        assert!(worst(&drawn, &old) <= 1, "{name}: {} levels from the copy above", worst(&drawn, &old));
        assert!(worst(&drawn, &stack_at(&before, 1.0, name).unwrap()) > 30, "{name}: something hidden");
        assert!(worst(&stack_at(&now, 1.5, name).unwrap(), &stack_at(&before, 1.5, name).unwrap()) <= 1, "{name}: the plain clip after the stretch");
    }
}

/// `recipe` placed on the test shot's clip with the harness's slots and
/// `values` over them: the sequence it makes.
fn placed_with(recipe: &klipp_timeline::composition_template::CompositionTemplate, plain: &Clip) -> Sequence {
    let before = sequence(plain.clone());
    let values: BTreeMap<String, TemplateValue> = recipe.slots.iter().map(|(n, s)| (n.clone(), filled(s.kind))).collect();
    let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).unwrap_or_else(|e| panic!("{e}"));
    let mut placed = plain.clone();
    placed.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
    sequence(placed)
}

/// Text behind a subject (#2758), measured on the test shot with its
/// known subject mask: on the subject the frame is the clip alone, in
/// front of the text; off it, the clip with the same title merged over it
/// unmasked. One clip's graph, one subject matte read (inverted, the title
/// laid where the subject isn't), on the GPU stack with nothing falling
/// back.
#[test]
fn text_behind_a_subject_shows_only_off_the_subject() {
    let recipes = builtin_recipes();
    let recipe = recipes.iter().find(|r| r.name == "Text behind subject").expect("the recipe");
    // The same graph without its matte: the title merged over the whole clip.
    let mut unmasked = recipe.clone();
    let graph = unmasked.graph.as_mut().unwrap();
    graph.ops.truncate(4);
    unmasked.slots.remove("subject");
    unmasked.bindings.retain(|b| b.parameter != "subject");
    let plain = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    let Some(behind) = stack(&placed_with(recipe, &plain), ViewChannel::Picture, "text behind subject") else { return };
    let Some(titled) = stack(&placed_with(&unmasked, &plain), ViewChannel::Picture, "the title over the clip") else { return };
    let Some(alone) = stack(&sequence(plain), ViewChannel::Picture, "the clip alone") else { return };
    let (mut on, mut off) = (0u8, 0u8);
    let (mut text_off, mut text_on) = (0usize, 0usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
            // A pixel's width from the mask's edge either way is left out.
            let near_edge = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)].iter().any(|(dx, dy)| on_subject(fx + dx / W as f32, fy + dy / H as f32) != on_subject(fx, fy));
            if near_edge {
                continue;
            }
            let i = ((y * W + x) * 4) as usize;
            let apart = |f: &[u8], g: &[u8]| (0..3).map(|c| f[i + c].abs_diff(g[i + c])).max().unwrap();
            let has_text = apart(&titled, &alone) > 40;
            if on_subject(fx, fy) {
                on = on.max(apart(&behind, &alone));
                text_on += usize::from(has_text);
            } else {
                off = off.max(apart(&behind, &titled));
                text_off += usize::from(has_text);
            }
        }
    }
    assert!(text_on > 50 && text_off > 50, "the title crosses the subject's edge: {text_on} of its pixels on it, {text_off} off");
    assert!(on <= 1, "on the subject the clip alone shows: {on} levels from it");
    assert!(off <= 1, "off the subject the title shows: {off} levels from it unmasked");
}

/// Screen replacement: the picture on the clip's tracked screen, the clip
/// as it was everywhere else, over every pixel a pixel's width clear of
/// the screen's edges; on the GPU stack with nothing falling back. A
/// surface the clip doesn't carry is refused, naming those it does.
#[test]
fn a_screen_replacement_lies_on_the_tracked_screen_only() {
    let recipes = builtin_recipes();
    let recipe = recipes.iter().find(|r| r.name == "Screen replacement").expect("the recipe");
    let plain = shot_clip();
    let Some(shown) = stack(&placed_with(recipe, &plain), ViewChannel::Picture, "screen replacement") else { return };
    let Some(alone) = stack(&sequence(plain.clone()), ViewChannel::Picture, "the clip alone") else { return };
    let (mut red_in, mut off) = (u8::MAX, 0u8);
    let (mut inside, mut outside) = (0usize, 0usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
            let Some(on) = in_screen(fx, fy, 1.5 / H as f32) else { continue };
            let i = ((y * W + x) * 4) as usize;
            if on {
                red_in = red_in.min(shown[i]);
                inside += 1;
            } else {
                off = off.max((0..3).map(|c| shown[i + c].abs_diff(alone[i + c])).max().unwrap());
                outside += 1;
            }
        }
    }
    assert!(inside > 300 && outside > 300, "{inside} pixels on the screen, {outside} off it");
    assert!(red_in >= 240, "the red picture fills the screen: its reddest-least pixel reads {red_in}");
    assert!(off <= 1, "off the screen the clip alone shows: {off} levels from it");

    let before = sequence(plain.clone());
    let mut values: BTreeMap<String, TemplateValue> = recipe.slots.iter().map(|(n, s)| (n.clone(), filled(s.kind))).collect();
    values.insert("surface".into(), TemplateValue::Text("Billboard".into()));
    let why = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).expect_err("refused");
    assert!(why.contains("\"Billboard\"") && why.contains("\"Screen\""), "says what it has: {why}");
}

/// Replace background (clean plate): keyed against the shot's own clean
/// frame at 0 s, the new background lies wherever the shot is as it was
/// then, and the subject stays in front, every pixel a pixel's width clear
/// of the patch's edges; on the GPU stack with nothing falling back. A
/// clean frame past the clip's end is refused, said.
#[test]
fn a_clean_plate_replaces_the_background_and_keeps_the_subject() {
    let recipes = builtin_recipes();
    let recipe = recipes.iter().find(|r| r.name == "Replace background (clean plate)").expect("the recipe");
    let plain = shot_clip();
    let Some(shown) = stack(&placed_with(recipe, &plain), ViewChannel::Picture, "clean plate") else { return };
    let Some(alone) = stack(&sequence(plain.clone()), ViewChannel::Picture, "the clip alone") else { return };
    let on_patch = |x: f32, y: f32| (0.3..0.7).contains(&x) && (0.3..0.7).contains(&y);
    let (mut subject, mut background) = (0u8, u8::MAX);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
            let near = [(1.5, 0.0), (-1.5, 0.0), (0.0, 1.5), (0.0, -1.5)].iter().any(|(dx, dy)| on_patch(fx + dx / W as f32, fy + dy / H as f32) != on_patch(fx, fy));
            if near {
                continue;
            }
            let i = ((y * W + x) * 4) as usize;
            if on_patch(fx, fy) {
                subject = subject.max((0..3).map(|c| shown[i + c].abs_diff(alone[i + c])).max().unwrap());
            } else {
                background = background.min(shown[i]);
            }
        }
    }
    assert!(subject <= 1, "the subject as the clip shows it: {subject} levels off");
    assert!(background >= 240, "the new background behind it: its least red is {background}");

    let before = sequence(plain.clone());
    let mut values: BTreeMap<String, TemplateValue> = recipe.slots.iter().map(|(n, s)| (n.clone(), filled(s.kind))).collect();
    values.insert("clean_frame".into(), TemplateValue::Number(5.0));
    let why = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).expect_err("refused");
    assert!(why.contains("past the clip's end"), "says why: {why}");
}

/// A recipe that shakes (#2758): a Transform after the chain whose
/// position a wiggle drives, its amplitude a control, put on by the
/// recipe's `modify` op.
fn shaking() -> klipp_timeline::composition_template::CompositionTemplate {
    let recipe: klipp_timeline::composition_template::CompositionTemplate = serde_json::from_value(serde_json::json!({
        "format_version": 2, "id": "3b8e1f2a-9c4d-4a71-b6e5-0d2f7a9c1e84", "name": "Shaking", "kind": "recipe",
        "composition_id": "00000000-0000-0000-0000-000000000000",
        "parameters": { "shake": { "label": "Shake", "default": { "type": "Number", "value": 30.0 }, "range": { "min": 0.0, "max": 200.0 } } },
        "bindings": [{ "parameter": "shake", "property": "node:0.position_x~amplitude" }],
        "graph": { "ops": [
            { "op": "add", "after": { "anchor": "end" }, "type": "klipp.fx.picture_transform" },
            { "op": "modify", "path": { "new": 0 }, "key": "position_x", "modifiers": [{ "type": "wiggle", "amplitude": 0.0, "frequency": 3.0 }] }
        ] }
    })).unwrap();
    recipe.validate().unwrap();
    recipe
}

/// `seq` on the GPU display stack at `secs`, nothing fallen back.
fn stack_at(seq: &Sequence, secs: f64, name: &str) -> Option<Vec<u8>> {
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let frame = klipp_effects::residency::frame("test", 1.0);
    let drawn = gpu.render_display_u8_on_gpu(seq, tc(secs), W, H, &mut Shot, RenderOptions::default());
    drop(frame);
    let Some((bytes, _)) = drawn else {
        assert!(!required(), "{name}: the stack declined: {:?}", gpu.last_decline());
        eprintln!("UNVERIFIED: {name}: the stack declined ({:?})", gpu.last_decline());
        return None;
    };
    let fell = klipp_effects::residency::last_frame();
    assert_eq!(fell.fallback_count(), 0, "{name}: nothing falls back: {:?}", fell.events);
    Some(bytes)
}

/// A behaviour a recipe puts on a parameter (#2758) moves the picture
/// over time, by as much as its control says: the shaken frame differs
/// from one moment to the next, and the CPU draws it as the stack does;
/// with the control at 0 it is the clip, still.
#[test]
fn a_behaviour_a_recipe_puts_on_moves_the_picture_over_time() {
    let recipe = shaking();
    let plain = shot_clip();
    let placed = |shake: f64| {
        let before = sequence(plain.clone());
        let values = BTreeMap::from([("shake".to_string(), TemplateValue::Number(shake))]);
        let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, &recipe, &values).unwrap_or_else(|e| panic!("{e}"));
        let mut c = plain.clone();
        c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
        sequence(c)
    };
    let shaken = placed(30.0);
    let Some(a) = stack_at(&shaken, 1.0, "shaken") else { return };
    let Some(b) = stack_at(&shaken, 1.4, "shaken later") else { return };
    assert!(worst(&a, &b) > 40, "it moves from one moment to the next: {} levels apart", worst(&a, &b));
    assert!(worst(&cpu(&shaken, ViewChannel::Picture), &a) <= 1, "the CPU draws it as the stack does");
    let still = placed(0.0);
    let Some(alone) = stack_at(&sequence(plain.clone()), 1.0, "the clip") else { return };
    for secs in [1.0, 1.4] {
        let Some(s) = stack_at(&still, secs, "no shake") else { return };
        assert!(worst(&s, &alone) <= 1, "no shake at {secs} s: the clip, {} levels off", worst(&s, &alone));
    }
}

/// Colour hold (#2758): the colour picked is kept and the rest goes grey
/// at its own brightness. Holding the test shot's green leaves its patch
/// as the clip shows it; holding red greys the patch to its luminance,
/// never to its brightest channel; the grey background is grey either
/// way. On the GPU stack with nothing falling back.
#[test]
fn colour_hold_keeps_the_colour_picked_and_greys_the_rest() {
    let recipes = builtin_recipes();
    let recipe = recipes.iter().find(|r| r.name == "Colour hold").expect("the recipe");
    let plain = shot_clip();
    let held = |rgb: [f32; 3]| {
        let before = sequence(plain.clone());
        let values = BTreeMap::from([("colour".to_string(), TemplateValue::Color([rgb[0], rgb[1], rgb[2], 1.0]))]);
        let (ops, _) = klipp_control::recipes::on_clip(&before, plain.id, recipe, &values).unwrap_or_else(|e| panic!("{e}"));
        let mut c = plain.clone();
        c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
        sequence(c)
    };
    let srgb = |v: f32| klipp_color::transfer::linear_to_srgb(v);
    let Some(green) = stack(&held([srgb(GREEN[0]), srgb(GREEN[1]), srgb(GREEN[2])]), ViewChannel::Picture, "green held") else { return };
    let Some(red) = stack(&held([0.85, 0.12, 0.1]), ViewChannel::Picture, "red held") else { return };
    let Some(alone) = stack(&sequence(plain.clone()), ViewChannel::Picture, "the clip") else { return };
    let at = |f: &[u8], x: f32, y: f32| { let i = (((y * H as f32) as u32 * W + (x * W as f32) as u32) * 4) as usize; [f[i], f[i + 1], f[i + 2]] };
    let (patch, ground) = ((0.5, 0.5), (0.12, 0.15));
    let apart = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).map(|(x, y)| x.abs_diff(y)).max().unwrap();
    assert!(apart(at(&green, patch.0, patch.1), at(&alone, patch.0, patch.1)) <= 1, "green held: the patch as it was");
    let grey = at(&red, patch.0, patch.1);
    assert!(grey.iter().all(|c| c.abs_diff(grey[0]) <= 1), "red held: the patch grey, {grey:?}");
    // The patch's luminance, sRGB-encoded: what grey it goes, not its
    // brightest channel.
    let luma = srgb(0.2126 * GREEN[0] + 0.7152 * GREEN[1] + 0.0722 * GREEN[2]) * 255.0;
    assert!((f32::from(grey[0]) - luma).abs() <= 2.0, "at its own brightness: {grey:?}, its luminance {luma}");
    for f in [&green, &red] {
        assert!(apart(at(f, ground.0, ground.1), at(&alone, ground.0, ground.1)) <= 1, "the grey ground as it was");
    }
}


/// Nodes saved as a recipe (#2757 step 6): the nodes a recipe put on one
/// clip, saved together with a published control, placed on another clip
/// draw that clip as the original was drawn, within a level on the GPU
/// stack with nothing falling back; the control at another value moves its
/// node and the picture. A selection that leaves out a node it is wired to
/// is refused, named.
#[test]
fn nodes_saved_as_a_recipe_place_like_the_original_and_their_control_moves_them() {
    use klipp_timeline::composition_template::{recipe_from_nodes, ControlSpec, DriveSpec};
    let recipes = builtin_recipes();
    let pip = recipes.iter().find(|r| r.name == "Picture in picture").expect("the recipe");
    let first = shot_clip();
    let original = placed_with(pip, &first);
    let clip = original.tracks[0].clips[0].clone();
    let before: Vec<uuid::Uuid> = first.picture_v2().unwrap().picture.graph.nodes.iter().map(|n| n.id).collect();
    let graph = clip.picture_v2().unwrap().picture.graph.clone();
    let made: Vec<uuid::Uuid> = graph.nodes.iter().map(|n| n.id).filter(|id| !before.contains(id)).collect();
    let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
    let control = ControlSpec { label: "Picture opacity".into(), min: Some(0.0), max: Some(1.0), default: None, drives: vec![DriveSpec { target: format!("node:{merge}.opacity"), to: [0.0, 1.0], curve: Default::default() }] };
    let saved = recipe_from_nodes(&clip, &made, "Saved picture in picture", None, std::slice::from_ref(&control)).unwrap_or_else(|e| panic!("{e}"));
    // Through a file and back, as the store keeps it.
    let saved: klipp_timeline::composition_template::CompositionTemplate = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    saved.validate().unwrap();

    let other = shot_clip();
    let place = |values: BTreeMap<String, TemplateValue>| {
        let seq = sequence(other.clone());
        let (ops, _) = klipp_control::recipes::on_clip(&seq, other.id, &saved, &values).unwrap_or_else(|e| panic!("{e}"));
        let mut c = other.clone();
        c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{e}"));
        c
    };
    let copy = place(BTreeMap::new());
    let Some(want) = stack(&original, ViewChannel::Picture, "the original") else { return };
    let Some(got) = stack(&sequence(copy), ViewChannel::Picture, "the saved recipe placed") else { return };
    assert!(worst(&want, &got) <= 1, "placed on another clip, it draws as the original: {} levels apart", worst(&want, &got));
    let Some(plain) = stack(&sequence(other.clone()), ViewChannel::Picture, "the other clip") else { return };
    assert!(worst(&got, &plain) > 100, "and it does something");

    // The control moves its node and the picture.
    let faded = place(BTreeMap::from([("picture_opacity".to_string(), TemplateValue::Number(0.4))]));
    let g = faded.picture_v2().unwrap().picture.graph.clone();
    let m = g.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap();
    let opacity = klipp_graph_lift::effect_params::entry_of(m).unwrap()["opacity"].as_f64().unwrap();
    assert!((opacity - 0.4).abs() < 1e-6, "the merge's opacity is the control's: {opacity}");
    let Some(dim) = stack(&sequence(faded), ViewChannel::Picture, "the control moved") else { return };
    assert!(worst(&dim, &got) > 40, "the picture moves with it");

    // The merge alone leaves out the picture it lays over the clip.
    let why = recipe_from_nodes(&clip, &[merge], "Half", None, &[]).unwrap_err();
    assert!(why.contains("isn't saved"), "{why}");
}
