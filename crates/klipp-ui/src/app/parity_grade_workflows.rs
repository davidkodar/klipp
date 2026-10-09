//! M3 step 18 parity (and step 11c's `workflow_tools_still_write_their_params`):
//! the grade's sampling tools, Auto White Balance, the qualifier's
//! eyedropper and Colour Match. Each reads the viewer's frame in the app,
//! which a headless test doesn't have, so each is held in two parts:
//! - its trigger, the same request from the same control in both views,
//!   writing nothing;
//! - what it computes, fed a known frame or colour through the same pure
//!   function the app uses, and landed through the same write: the values
//!   asserted, one step, undone exactly.

use super::parity_controls::{placed, Grade, Page};
use klipp_core::AppCommand;
use klipp_nodes::NodeKind;

/// Click the drawn text `text`: its last occurrence.
fn click(g: &mut Grade, text: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == text).map(|(_, p)| p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
    g.click_at(at);
    Ok(())
}

/// Click the icon button by `label` (an eyedropper has no text of its
/// own): right of it on its line, as the old panel puts it, or on a line
/// just above, from the left, as the generated view puts it over the
/// rows. Found on throwaway twins, as the first point whose click sends a
/// request and writes nothing, so a control on the way (the label's own
/// checkbox) is never clicked here.
fn click_icon_by(g: &mut Grade, generated: bool, label: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("{label:?} isn't drawn"))?;
    let right = (0..40).map(|i| egui::pos2(at.x + 4.0 + 4.0 * i as f32, at.y));
    let above = (1..=3).flat_map(|row| (0..50).map(move |i| egui::pos2(4.0 + 4.0 * i as f32, at.y - 20.0 * row as f32)));
    for p in right.chain(above) {
        let mut t = g.twin(generated);
        t.drawn();
        let before = t.picture();
        t.click_at(p);
        if !t.side.is_empty() && t.picture() == before {
            g.click_at(p);
            return Ok(());
        }
    }
    Err(format!("no button by {label:?}"))
}

/// A frame of one colour, `w` by `h`.
fn flat(rgb: [u8; 3], w: u32, h: u32) -> Vec<u8> {
    (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect()
}

/// The trigger: driving `control` in each view sends the same one request,
/// `want`, and writes nothing.
fn trigger(start: &Grade, want: &str, control: impl Fn(&mut Grade, bool) -> Result<(), String>) -> Result<(), String> {
    for generated in [false, true] {
        let mut g = start.twin(generated);
        g.drawn();
        let before = g.picture();
        control(&mut g, generated).map_err(|e| format!("generated {generated}: {e}"))?;
        if !g.unapplied.is_empty() {
            return Err(format!("generated {generated}: unapplied {:?}", g.unapplied));
        }
        if g.side.len() != 1 || !g.side[0].starts_with(want) {
            return Err(format!("generated {generated}: sent {:?}, want one {want}", g.side));
        }
        if g.picture() != before || g.state.active_history().undo_count() != 0 {
            return Err(format!("generated {generated}: the request wrote"));
        }
    }
    Ok(())
}

/// Auto White Balance, the qualifier's pick and Colour Match: each tool's
/// trigger in both views, and each computation on a known input landing
/// its values in one undoable step.
#[test]
fn workflow_tools_still_write_their_params() {
    // ── Auto White Balance ──
    let (state, clip, node) = super::grade_inspector_tests::graded(NodeKind::Corrector).expect("a Corrector");
    let corrector = Grade::new(state, clip, node, true).on(Page::Tabbed(crate::state::ColorTab::Adjustments));
    trigger(&corrector, "AutoWhiteBalance", |g, _| click(g, "Auto WB")).unwrap();
    // A grey card under a warm light: Auto WB cools it, and the Corrector
    // holds exactly that. The cast stays under chroma 8, where the frame
    // is surely grey; a warmer one reads as skin, which is never balanced
    // on (#403).
    let frame = flat([176, 168, 158], 32, 32);
    let (temp, tint) = klipp_color::auto_white_balance(&frame, 32, 32, &[]).expect("a grey to balance on");
    assert!(temp < 0.0, "a warm cast is cooled: {temp}");
    let mut g = corrector.twin(true);
    let balance = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| (n.params.temperature, n.params.tint)).unwrap();
    let before = balance(&g);
    assert_eq!(g.state.apply_white_balance(g.clip, temp, tint), Some(false), "within range");
    let (t, n) = balance(&g);
    assert!(t.is_some_and(|t| (t - temp).abs() < 1e-4) && n.is_some_and(|n| (n - tint).abs() < 1e-4), "the Corrector holds the balance: {:?}", (t, n));
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(balance(&g), before, "undo gives back the balance");

    // ── The qualifier's eyedropper ──
    let (state, clip, node) = placed(NodeKind::Qualifier).expect("a Qualifier");
    let qualifier = Grade::new(state, clip, node, true).on(Page::PerNode);
    trigger(&qualifier, "PickQualifierColour", |g, generated| click_icon_by(g, generated, "Enabled")).unwrap();
    // An orange picked: the key sits on its hue.
    let rgb = [0.8, 0.3, 0.05];
    let hue = klipp_effects::cpu_grade::picture_hsl(rgb).h * 360.0;
    let mut g = qualifier.twin(true);
    let key = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.qualifier.clone());
    let before = key(&g);
    let keyed = super::colour::qualifier_keyed_on(rgb, before.clone().unwrap_or_default());
    g.apply(AppCommand::SetNodeQualifier { clip_id: g.clip, node_id: g.node, qualifier: keyed });
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    let held = key(&g).expect("the qualifier is keyed");
    assert!((held.hue_center - hue).abs() < 1.0, "on the picked hue: {} for {hue}", held.hue_center);
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(key(&g), before, "undo gives back the key");

    // ── Colour Match ──
    let (state, clip, node) = placed(NodeKind::ColorMatch).expect("a Colour Match node");
    let matcher = Grade::new(state, clip, node, true).on(Page::PerNode);
    trigger(&matcher, "CaptureColorMatchReference", |g, _| click(g, "Capture reference")).unwrap();
    trigger(&matcher, "ApplyColorMatch", |g, _| click(g, "Apply match")).unwrap();
    // A frame darker than its reference: the match brightens it.
    let result = klipp_color::color_match::match_colors(&flat([90, 90, 90], 32, 32), &flat([150, 150, 150], 32, 32));
    assert!(result.exposure_adjust > 0.0, "a darker frame is brightened: {}", result.exposure_adjust);
    let settings = klipp_core::ColorMatchSettings { exposure: result.exposure_adjust, temperature: result.temperature_adjust, tint: result.tint_adjust, saturation: result.saturation_adjust };
    let mut g = matcher.twin(true);
    let matched = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.params.color_match);
    let before = matched(&g);
    // As `ApplyColorMatch` lands it: through `SetNodeColorMatch`.
    g.apply(AppCommand::SetNodeColorMatch { clip_id: g.clip, node_id: g.node, settings });
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(matched(&g), Some(settings), "the node holds the match");
    assert_eq!(g.state.active_history().undo_count(), 1, "one step");
    g.step(false).unwrap();
    assert_eq!(matched(&g), before, "undo gives back the node");
}
