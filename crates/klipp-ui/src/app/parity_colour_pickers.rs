//! An effect card's colour rows (a key colour, a glow's or a shadow's
//! colour, a fade's colour with its alpha) on the generated stack, driven
//! through egui's own colour popup: the swatch clicked, a channel's number
//! typed or dragged in the popup, the popup closed. Held to the expected
//! result: the colour stored as typed, one undo step, an exact undo, and
//! the frame the expected colour draws changed, the CPU's the GPU's.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{catalog, CatalogEntry, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::{effect_types, EffectType, ParamDesc};

/// The stack's colour rows: (entry, type, parameter, with alpha).
fn entries() -> Vec<(CatalogEntry, &'static EffectType, &'static ParamDesc, bool)> {
    catalog()
        .into_iter()
        .filter(|e| !e.grade)
        .filter_map(|e| {
            let CatalogValue::Parameter { desc, presentation } = e.value else { return None };
            let alpha = match presentation.widget {
                Widget::ColourRgb => false,
                Widget::ColourRgba => true,
                _ => return None,
            };
            let ty = effect_types().find(|t| t.desc.id == e.type_id)?;
            Some((e, ty, desc, alpha))
        })
        .collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    for (entry, ..) in entries() {
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
    }
}

fn press(p: egui::Pos2, down: bool) -> egui::Event {
    egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE }
}

fn click(run: &mut Run, p: egui::Pos2) {
    run.frame(vec![egui::Event::PointerMoved(p), press(p, true)], egui::Modifiers::NONE);
    run.frame(vec![egui::Event::PointerMoved(p), press(p, false)], egui::Modifiers::NONE);
    run.frame(Vec::new(), egui::Modifiers::NONE);
}

/// A drawn text starting with `prefix` (a popup channel, "R 180").
fn field(run: &mut Run, prefix: &str) -> Option<egui::Pos2> {
    run.frame(Vec::new(), egui::Modifiers::NONE).into_iter().find(|(t, _)| t.starts_with(prefix) && t[prefix.len()..].trim().parse::<f32>().is_ok()).map(|(_, p)| p)
}

/// Open the colour popup of the row labelled `label`: its swatch, the
/// first point right of the label on its line whose click draws the
/// popup's channels, found on a throwaway run of the same view.
fn open(run: &mut Run, label: &str) -> Result<(), String> {
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let at = drawn.iter().find(|(t, _)| t == label).map(|(_, p)| *p).ok_or_else(|| format!("no {label:?} row"))?;
    for i in 0..80 {
        let p = egui::pos2(at.x + 10.0 + 6.0 * i as f32, at.y);
        let mut t = Run::new(run.clip().clone());
        t.frame(Vec::new(), egui::Modifiers::NONE);
        click(&mut t, p);
        if field(&mut t, "R ").is_some() {
            click(run, p);
            return field(run, "R ").map(drop).ok_or_else(|| "the popup didn't open".into());
        }
    }
    Err(format!("no swatch on the {label:?} row"))
}

/// Type `value` into the popup's channel `prefix` ("R ", "A "), Enter.
fn type_channel(run: &mut Run, prefix: &str, value: &str) -> Result<(), String> {
    let at = field(run, prefix).ok_or_else(|| format!("no {prefix:?} in the popup"))?;
    click(run, at);
    run.key(egui::Key::A, egui::Modifiers::COMMAND);
    run.frame(vec![egui::Event::Text(value.into())], egui::Modifiers::NONE);
    run.key(egui::Key::Enter, egui::Modifiers::NONE);
    Ok(())
}

/// Drag the popup's channel `prefix` right by `dx`, in steps, and let go.
fn drag_channel(run: &mut Run, prefix: &str, dx: f32) -> Result<(), String> {
    let at = field(run, prefix).ok_or_else(|| format!("no {prefix:?} in the popup"))?;
    run.frame(vec![egui::Event::PointerMoved(at), press(at, true)], egui::Modifiers::NONE);
    for k in 1..=4 {
        run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx * k as f32 / 4.0, 0.0))], egui::Modifiers::NONE);
    }
    run.frame(vec![press(at + egui::vec2(dx, 0.0), false)], egui::Modifiers::NONE);
    run.frame(vec![egui::Event::PointerGone], egui::Modifiers::NONE);
    Ok(())
}

/// Close the popup with Escape.
fn close(run: &mut Run) {
    run.key(egui::Key::Escape, egui::Modifiers::NONE);
    run.frame(Vec::new(), egui::Modifiers::NONE);
}

/// Where the effect whose colour row this is sits in the clip's list: a
/// pixel fixture puts a shrinking Transform before a glow or a shadow.
fn index_of(clip: &klipp_timeline::Clip, ty: &EffectType) -> usize {
    clip.builtin_effects().iter().position(|e| serde_json::to_value(e).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|t| t == ty.tag)).unwrap_or(false)).unwrap_or(0)
}

/// The effect's stored colour.
fn colour(run: &Run, ty: &EffectType, desc: &ParamDesc) -> serde_json::Value {
    let clip = run.clip();
    serde_json::to_value(&clip.builtin_effects()[index_of(clip, ty)]).ok().and_then(|v| v.get(desc.field).cloned()).unwrap_or_default()
}

/// `start` with its effect's colour set to `value`: the expected clip.
fn with_colour(start: &klipp_timeline::Clip, ty: &EffectType, desc: &ParamDesc, value: serde_json::Value) -> Result<klipp_timeline::Clip, String> {
    let index = index_of(start, ty);
    let mut json = serde_json::to_value(&start.builtin_effects()[index]).map_err(|e| e.to_string())?;
    json[desc.field] = value;
    let effect: klipp_effects::BuiltinEffect = serde_json::from_value(json).map_err(|e| e.to_string())?;
    let mut clip = start.clone();
    let id = clip.effect_id_at(index).ok_or("the effect")?;
    clip.replace_effect(id, effect);
    Ok(clip)
}

/// One edit (`drive`) through the popup on the generated stack from
/// `start`: the stored colour changed (to what `expect` makes of the colour
/// before, when it says), one step, an exact undo. The clip after it.
fn edit(start: &klipp_timeline::Clip, ty: &EffectType, desc: &ParamDesc, label: &str, drive: impl Fn(&mut Run) -> Result<(), String>, expect: Option<&dyn Fn(&serde_json::Value) -> serde_json::Value>, edits: usize) -> Result<klipp_timeline::Clip, String> {
    let mut run = Run::new(start.clone());
    let before = run.clip().picture_v2().map_err(|e| e.to_string())?;
    let was = colour(&run, ty, desc);
    open(&mut run, label)?;
    drive(&mut run)?;
    close(&mut run);
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", run.unapplied));
    }
    let now = colour(&run, ty, desc);
    if now == was {
        return Err(format!("the colour is still {was}"));
    }
    if let Some(expect) = expect {
        let want = expect(&was);
        if now != want {
            return Err(format!("stored {now}, expected {want}"));
        }
        if run.clip().picture_v2().map_err(|e| e.to_string())? != with_colour(start, ty, desc, want)?.picture_v2().map_err(|e| e.to_string())? {
            return Err("not the picture the expected colour makes".into());
        }
    }
    // One step per edit: each typed channel, each drag.
    let steps = run.state.active_history().undo_count();
    if steps != edits {
        return Err(format!("{steps} steps, not {edits}"));
    }
    let after = run.clip().clone();
    for _ in 0..edits {
        run.step(false);
    }
    if run.clip().picture_v2().map_err(|e| e.to_string())? != before {
        return Err("undo doesn't give back the picture before".into());
    }
    Ok(after)
}

/// The colour before with channel `i` set to `v`.
fn channel(i: usize, v: u64) -> impl Fn(&serde_json::Value) -> serde_json::Value {
    move |was| {
        let mut c = was.clone();
        if let Some(slot) = c.get_mut(i) {
            *slot = serde_json::json!(v);
        }
        c
    }
}

pub(super) fn run(report: &mut Report) {
    for (entry, ty, desc, alpha) in entries() {
        let key = |i| CoverageKey::new(entry.type_id, &entry.parameter, Surface::EffectsStack, "sample", i);
        let label = ty.presentation(desc).label.unwrap_or(desc.label);
        let start = fixture(&(ty.sample)());
        let name = |what: &str| format!("{} {} {what}", ty.tag, desc.key);
        report.case(&name("opened"), [key(Interaction::OpenWithoutWrite)], || {
            let mut run = Run::new(start.clone());
            let before = run.clip().picture_v2().map_err(|e| e.to_string())?;
            open(&mut run, label)?;
            close(&mut run);
            if run.clip().picture_v2().map_err(|e| e.to_string())? != before || run.state.active_history().undo_count() != 0 || !run.unapplied.is_empty() {
                return Err("opening the popup wrote".into());
            }
            Ok(())
        });
        report.case(&name("typed"), [key(Interaction::ColourEdit), key(Interaction::UndoRedo)], || {
            edit(&start, ty, desc, label, |r| type_channel(r, "R ", "40"), Some(&channel(0, 40)), 1).map(drop)
        });
        if alpha {
            report.case(&name("alpha typed"), [key(Interaction::AlphaEdit)], || {
                edit(&start, ty, desc, label, |r| type_channel(r, "A ", "128"), Some(&channel(3, 128)), 1).map(drop)
            });
        }
        // A channel dragged in the popup: one step, as typing is.
        report.case(&name("dragged"), [], || {
            // Toward the middle, so the drag can't clamp at an end.
            let g = serde_json::to_value(&start.builtin_effects()[index_of(&start, ty)]).ok().and_then(|v| v[desc.field][1].as_u64()).unwrap_or(0);
            let dx = if g < 128 { 40.0 } else { -40.0 };
            edit(&start, ty, desc, label, |r| drag_channel(r, "G ", dx), None, 1).map(drop)
        });
        // Escape inside the popup, mid-typing: the popup closes, and what
        // it keeps is nothing, or one step undone exactly.
        report.case(&name("escaped"), [], || {
            let mut run = Run::new(start.clone());
            let before = run.clip().picture_v2().map_err(|e| e.to_string())?;
            open(&mut run, label)?;
            let at = field(&mut run, "B ").ok_or("no B in the popup")?;
            click(&mut run, at);
            run.key(egui::Key::A, egui::Modifiers::COMMAND);
            run.frame(vec![egui::Event::Text("17".into())], egui::Modifiers::NONE);
            close(&mut run);
            close(&mut run);
            if !run.unapplied.is_empty() {
                return Err(format!("unapplied {:?}", run.unapplied));
            }
            if field(&mut run, "R ").is_some() {
                return Err("Escape left the popup open".into());
            }
            match run.state.active_history().undo_count() {
                0 if run.clip().picture_v2().map_err(|e| e.to_string())? == before => Ok(()),
                1 if run.step(false) && run.clip().picture_v2().map_err(|e| e.to_string())? == before => Ok(()),
                n => Err(format!("Escape left {n} steps")),
            }
        });
        report.case_or_blocked(&name("drawn"), [key(Interaction::Pixels)], || {
            let (start, _) = super::parity_numeric::pixel_fixture(ty, &(ty.sample)(), Surface::EffectsStack)?;
            // An orange the source holds (its hue ramp), so a key colour
            // keys something and a glow or a shadow shows its change.
            let orange = |was: &serde_json::Value| channel(2, 60)(&channel(1, 120)(&channel(0, 200)(was)));
            let new = edit(&start, ty, desc, label, |r| {
                type_channel(r, "R ", "200")?;
                type_channel(r, "G ", "120")?;
                type_channel(r, "B ", "60")
            }, Some(&orange), 3)?;
            // `edit` has held its picture to the expected colour's; here the
            // frame it draws changes, and the CPU's is the GPU's.
            use super::parity_pixels::{Routes, FULL, SMALL};
            let routes = if super::parity_numeric::CPU_BY_DESIGN.iter().any(|(t, _)| *t == ty.desc.id) { Routes::CpuByDesign } else { Routes::Both };
            let size = if super::parity_numeric::AT_FULL_SIZE.contains(&ty.tag) { FULL } else { SMALL };
            super::parity_pixels::clip_edit_drawn_on(routes, size, &start, &new)
        });
    }
}

/// Every colour row on the stack, as above: every case passes, and every
/// key it's required for is earned, but those excluded with a citation.
#[test]
fn effect_colour_rows_store_the_colour_through_their_popup() {
    let mut report = Report::default();
    report.family("effect colour rows", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| !manifest.excluded_keys.contains_key(*k) && !report.passed.contains(k) && !report.blocked.contains_key(*k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}
