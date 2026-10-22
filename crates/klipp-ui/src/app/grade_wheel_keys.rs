//! A colour wheel keyed on the Colour page (#891): its diamond keys its
//! puck and master together at the playhead, a keyed wheel's drag is a
//! key, its arrows step between the wheel's keys, and its key menu sets
//! every part's interpolation or ease; the saturation and brightness under
//! a wheel key on their own. Driven with pointer events on each page that
//! draws the wheels (the per-node layout, the Wheels tab, the bottom
//! tools), each change one undo step. A grade row's key menu eases its key
//! by path the same way.

use super::parity_controls::{placed, with_node, Grade, Page};
use super::parity_grade_numeric::button;
use crate::state::ColorTab;
use klipp_core::{Interpolation, KeyEase, Keyframe};
use klipp_nodes::NodeKind;

/// The time the harness draws the grade at (`Grade::frame`).
const AT: f64 = 0.5;

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
}

/// A Corrector drawn on `page`: on its own per-node layout, or the
/// Colour page's Wheels tab or bottom tools.
fn corrector(page: Page) -> Grade {
    let (state, clip, node) = match page {
        Page::PerNode => placed(NodeKind::Corrector),
        _ => super::grade_inspector_tests::graded(NodeKind::Corrector),
    }
    .expect("a Corrector");
    Grade::new(state, clip, node).on(page)
}

const PAGES: [Page; 3] = [Page::PerNode, Page::Tabbed(ColorTab::Wheels), Page::Bottom(ColorTab::Wheels)];

/// The node's keys for `param`, as stored.
fn keys_of(g: &Grade, param: &str) -> Vec<Keyframe> {
    g.grade().nodes.iter().find(|n| n.id == g.node).and_then(|n| n.keyframes.tracks.iter().find(|t| t.param_name == param)).map(|t| t.keyframes.clone()).unwrap_or_default()
}

/// `start` with its node's parameters and keys set by `edit`.
fn with(start: &Grade, edit: impl FnOnce(&mut klipp_nodes::Node)) -> Grade {
    with_node(start, |n| {
        edit(n);
        Ok(())
    })
    .unwrap()
}

/// The diamond beside the title `title` (a wheel's, or a rail's under a
/// wide wheel): the nearest drawn diamond right of it, on its line.
fn diamond_by(g: &mut Grade, title: &str) -> Result<egui::Pos2, String> {
    diamond_within(g, title, 60.0)
}

/// The nearest diamond right of `title` on its line, within `reach`: a
/// narrow rail's ("S", "B") is at the row's end, past its slider.
fn diamond_within(g: &mut Grade, title: &str, reach: f32) -> Result<egui::Pos2, String> {
    let drawn = g.drawn();
    drawn
        .iter()
        .filter(|(t, _)| t == title)
        .filter_map(|(_, at)| g.diamonds.iter().map(|d| d.center()).filter(|d| (d.y - at.y).abs() < 6.0 && d.x > at.x && d.x - at.x < reach).min_by(|a, b| a.x.total_cmp(&b.x)))
        .next()
        .ok_or_else(|| format!("no diamond beside {title:?}"))
}

/// The arrow toward the previous or next key beside the diamond at `at`.
fn caret(g: &Grade, at: egui::Pos2, next: bool) -> Result<egui::Pos2, String> {
    g.icons.iter().map(|i| i.center()).filter(|c| (c.y - at.y).abs() < 6.0 && (c.x > at.x) == next && (c.x - at.x).abs() < 20.0).min_by(|a, b| (a.x - at.x).abs().total_cmp(&(b.x - at.x).abs())).ok_or_else(|| format!("no {} arrow", if next { "next" } else { "previous" }))
}

/// Pick `item` from the menu a right click at `at` opens.
fn menu_pick(g: &mut Grade, at: egui::Pos2, item: &str) -> Result<(), String> {
    g.right_click_at(at);
    let pos = g.frame(Vec::new()).into_iter().rev().find(|(t, _)| t == item).map(|(_, p)| p).ok_or_else(|| format!("no menu item {item:?}"))?;
    g.click_at(pos);
    Ok(())
}

fn one_step_undone(g: &mut Grade, before: &super::parity_controls::GradePicture) -> Result<(), String> {
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    if g.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", g.state.active_history().undo_count()));
    }
    g.step(false)?;
    if &g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    Ok(())
}

const LIFT: [&str; 3] = ["lift_x", "lift_y", "lift_master"];

/// The Lift wheel's diamond keys its puck and master at the playhead with
/// the values it shows, every part at the one time, as one undo step; no
/// other wheel is keyed. Clicked again, the keys go, one step.
#[test]
fn a_wheel_diamond_keys_its_puck_and_master_together() {
    for page in PAGES {
        let start = with(&corrector(page), |n| {
            n.params.lift_x = Some(0.1);
            n.params.lift_y = Some(-0.05);
            n.params.lift_master = Some(0.02);
        });
        let mut g = start.twin();
        g.drawn();
        let before = g.picture();
        let at = diamond_by(&mut g, "Lift").unwrap_or_else(|e| panic!("{page:?}: {e}"));
        g.click_at(at);
        for (param, want) in LIFT.iter().zip([0.1, -0.05, 0.02]) {
            let keys = keys_of(&g, param);
            assert!(keys.len() == 1 && (keys[0].time_secs - AT).abs() < 1e-9 && (keys[0].value - want).abs() < 1e-6, "{page:?}: {param} keys {keys:?}");
        }
        assert!(keys_of(&g, "gamma_x").is_empty(), "{page:?}: another wheel keyed");
        // The diamond is filled now, and takes the keys away.
        let at = diamond_by(&mut g, "Lift").unwrap();
        g.click_at(at);
        assert!(LIFT.iter().all(|p| keys_of(&g, p).is_empty()), "{page:?}: the second click left keys");
        assert_eq!(g.state.active_history().undo_count(), 2, "{page:?}");
        g.step(false).unwrap();
        g.step(false).unwrap();
        assert_eq!(g.picture(), before, "{page:?}: undone twice");
        let mut once = start.twin();
        once.drawn();
        let at = diamond_by(&mut once, "Lift").unwrap();
        once.click_at(at);
        one_step_undone(&mut once, &before).unwrap_or_else(|e| panic!("{page:?}: {e}"));
    }
}

/// Keyed, the wheel shows its animation at the playhead and a drag of its
/// puck keys the parts there (a key added on each, the rest of the track
/// and the stored values as they were), one undo step: a drag on a keyed
/// wheel isn't lost under its keys.
#[test]
fn a_keyed_wheel_drag_keys_the_playhead() {
    for page in PAGES {
        let start = with(&corrector(page), |n| {
            for (param, a, b) in [("lift_x", 0.0, 0.4), ("lift_y", 0.0, 0.0), ("lift_master", 0.0, 0.2)] {
                n.keyframes.track_mut(param).keyframes = vec![key(0.25, a), key(1.25, b)];
            }
        });
        let mut g = start.twin();
        g.drawn();
        let before = g.picture();
        let (title, centre) = wheel_centre(&mut g, "Lift").unwrap_or_else(|e| panic!("{page:?}: {e}"));
        let _ = title;
        drag(&mut g, centre, egui::vec2(18.0, 0.0));
        let at = |param: &str| keys_of(&g, param).into_iter().find(|k| (k.time_secs - AT).abs() < 1e-9).map(|k| k.value);
        // 0.25 → 1.25 linear: a quarter of the way at 0.5.
        let (x, y, m) = (at("lift_x"), at("lift_y"), at("lift_master"));
        assert!(x.is_some_and(|x| x > 0.1 + 0.05), "{page:?}: lift_x key {x:?} isn't the animated 0.1 moved right");
        assert!(y.is_some_and(|y| y.abs() < 1e-6), "{page:?}: lift_y key {y:?}");
        assert!(m.is_some_and(|m| (m - 0.05).abs() < 1e-5), "{page:?}: lift_master key {m:?}");
        let node = g.grade().nodes.iter().find(|n| n.id == g.node).unwrap().clone();
        assert_eq!((node.params.lift_x, node.params.lift_master), (None, None), "{page:?}: the stored values were written");
        for param in LIFT {
            assert_eq!(keys_of(&g, param).len(), 3, "{page:?}: {param}");
        }
        one_step_undone(&mut g, &before).unwrap_or_else(|e| panic!("{page:?}: {e}"));
    }
}

/// The wheel named `title`'s disc: its title, and the centre of its disc,
/// a wheel's diameter below the title (the wheel's master "Y" is 38 px
/// further down, `color_wheel_sized`).
fn wheel_centre(g: &mut Grade, title: &str) -> Result<(egui::Pos2, egui::Pos2), String> {
    let drawn = g.drawn();
    for (_, at) in drawn.iter().filter(|(t, _)| t == title) {
        let master = drawn.iter().filter(|(t, p)| t == "Y" && p.y > at.y + 40.0 && p.y < at.y + 260.0 && p.x < at.x && at.x - p.x < 150.0).map(|(_, p)| *p).min_by(|a, b| a.y.total_cmp(&b.y));
        if let Some(y) = master {
            let diameter = y.y - at.y - 38.0;
            return Ok((*at, egui::pos2(at.x, at.y + 7.0 + diameter / 2.0)));
        }
    }
    Err(format!("no {title:?} wheel"))
}

fn drag(g: &mut Grade, from: egui::Pos2, by: egui::Vec2) {
    g.frame(vec![egui::Event::PointerMoved(from)]);
    g.frame(vec![button(from, true)]);
    g.frame(vec![egui::Event::PointerMoved(from + by)]);
    g.frame(vec![button(from + by, false)]);
    g.frame(vec![egui::Event::PointerGone]);
}

/// The wheel's arrows walk the wheel's keys, whichever part has them: the
/// puck's key before the playhead, the master's after it. Seeking writes
/// nothing.
#[test]
fn wheel_arrows_step_between_the_wheel_keys() {
    for page in PAGES {
        let start = with(&corrector(page), |n| {
            n.keyframes.track_mut("lift_x").keyframes = vec![key(0.25, 0.1)];
            n.keyframes.track_mut("lift_master").keyframes = vec![key(1.25, 0.2)];
        });
        for (next, want) in [(false, 0.25), (true, 1.25)] {
            let mut g = start.twin();
            let at = diamond_by(&mut g, "Lift").unwrap_or_else(|e| panic!("{page:?}: {e}"));
            let arrow = caret(&g, at, next).unwrap_or_else(|e| panic!("{page:?}: {e}"));
            g.click_at(arrow);
            let clip_in = g.clip().timeline_in.to_secs_f64();
            assert!(g.seeks.len() == 1 && (g.seeks[0].to_secs_f64() - clip_in - want).abs() < 0.02, "{page:?}: seeks {:?}, want {want}", g.seeks);
            assert_eq!(g.state.active_history().undo_count(), 0, "{page:?}: a seek wrote");
        }
    }
}

/// A keyed wheel's key menu sets each part's key at the playhead: a hold
/// on every part (the move into the key and out of it, as the keyframe
/// editor sets it), or an ease in, which slows each part into its key;
/// either one undo step. The menu is the app's menu: full-width rows.
#[test]
fn a_wheel_key_menu_holds_or_eases_every_part() {
    for page in PAGES {
        let start = with(&corrector(page), |n| {
            for (i, param) in LIFT.iter().enumerate() {
                let v = 0.1 * (i + 1) as f32;
                n.keyframes.track_mut(param).keyframes = vec![key(0.25, 0.0), key(AT, v), key(1.25, -v)];
            }
        });
        for (item, check) in [("Step (Hold)", None), ("Ease In", Some(KeyEase::In))] {
            let mut g = start.twin();
            g.drawn();
            let before = g.picture();
            let at = diamond_by(&mut g, "Lift").unwrap_or_else(|e| panic!("{page:?}: {e}"));
            menu_pick(&mut g, at, item).unwrap_or_else(|e| panic!("{page:?}: {e}"));
            for (i, param) in LIFT.iter().enumerate() {
                let v = 0.1 * (i + 1) as f32;
                let mut want = vec![key(0.25, 0.0), key(AT, v), key(1.25, -v)];
                match check {
                    None => klipp_core::keyframe::set_interpolation_at(&mut want, 1, Interpolation::Step),
                    Some(side) => klipp_core::keyframe::ease_key(&mut want, 1, side),
                }
                assert_eq!(keys_of(&g, param), want, "{page:?} {item}: {param}");
            }
            one_step_undone(&mut g, &before).unwrap_or_else(|e| panic!("{page:?} {item}: {e}"));
        }
    }
}

/// The saturation under a wheel keys on its own: its diamond keys it at
/// the playhead with its value, the wheel's parts untouched, one step.
#[test]
fn the_saturation_under_a_wheel_keys_on_its_own() {
    for page in PAGES {
        let start = with(&corrector(page), |n| n.params.lift_sat = Some(0.3));
        let mut g = start.twin();
        g.drawn();
        let before = g.picture();
        let (title, reach) = if matches!(page, Page::Bottom(_)) { ("Saturation", 60.0) } else { ("S", 220.0) };
        let at = diamond_within(&mut g, title, reach).unwrap_or_else(|e| panic!("{page:?}: {e}"));
        g.click_at(at);
        let keys = keys_of(&g, "lift_sat");
        assert!(keys.len() == 1 && (keys[0].time_secs - AT).abs() < 1e-9 && (keys[0].value - 0.3).abs() < 1e-6, "{page:?}: lift_sat keys {keys:?}");
        assert!(LIFT.iter().all(|p| keys_of(&g, p).is_empty()), "{page:?}: the wheel was keyed");
        one_step_undone(&mut g, &before).unwrap_or_else(|e| panic!("{page:?}: {e}"));
    }
}

/// A grade row's key menu eases its key by path: "Ease Out" on the
/// exposure's key at the playhead slows the move out of it, the track as
/// `ease_key` leaves it, one undo step.
#[test]
fn a_grade_row_key_menu_eases_its_key() {
    for page in [Page::Tabbed(ColorTab::Adjustments), Page::Bottom(ColorTab::Adjustments)] {
        let start = with(&corrector(page), |n| n.keyframes.track_mut("exposure").keyframes = vec![key(0.25, 0.0), key(AT, 1.0), key(1.25, 0.0)]);
        let mut g = start.twin();
        let drawn = g.drawn();
        let before = g.picture();
        let label = drawn.iter().find(|(t, _)| t == "Exposure").map(|(_, p)| *p).unwrap_or_else(|| panic!("{page:?}: no Exposure row"));
        let at = g.diamonds.iter().map(|d| d.center()).filter(|d| (d.y - label.y).abs() < 6.0 && d.x > label.x).min_by(|a, b| a.x.total_cmp(&b.x)).unwrap_or_else(|| panic!("{page:?}: no diamond on the Exposure row"));
        menu_pick(&mut g, at, "Ease Out").unwrap_or_else(|e| panic!("{page:?}: {e}"));
        let mut want = vec![key(0.25, 0.0), key(AT, 1.0), key(1.25, 0.0)];
        klipp_core::keyframe::ease_key(&mut want, 1, KeyEase::Out);
        assert_eq!(keys_of(&g, "exposure"), want, "{page:?}");
        one_step_undone(&mut g, &before).unwrap_or_else(|e| panic!("{page:?}: {e}"));
    }
}

/// What the control socket sends for a key (`set_node_param` with `at`):
/// a node gesture holding the key and its ease lands as the key, eased,
/// one undo step, as the Colour page's own key does.
#[test]
fn a_socket_key_is_one_undo_step() {
    use klipp_core::AppCommand as C;
    let mut g = corrector(Page::Tabbed(ColorTab::Wheels));
    g.drawn();
    let before = g.picture();
    let (clip_id, node_id) = (g.clip, g.node);
    for cmd in [
        C::BeginNodeParamEdit { clip_id },
        C::AddKeyframe { clip_id, node_id, param: "exposure".into(), time_secs: 0.25, value: 0.0 },
        C::EndNodeParamEdit { clip_id },
        C::BeginNodeParamEdit { clip_id },
        C::AddKeyframe { clip_id, node_id, param: "exposure".into(), time_secs: 1.0, value: 1.0 },
        C::SetKeyframeEase { clip_id, node_id, param: "exposure".into(), time_secs: 1.0, ease: KeyEase::In },
        C::EndNodeParamEdit { clip_id },
    ] {
        g.apply(cmd);
    }
    let mut want = vec![key(0.25, 0.0), key(1.0, 1.0)];
    klipp_core::keyframe::ease_key(&mut want, 1, KeyEase::In);
    assert_eq!(keys_of(&g, "exposure"), want);
    assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
    assert_eq!(g.state.active_history().undo_count(), 2, "one step per key");
    g.step(false).unwrap();
    g.step(false).unwrap();
    assert_eq!(g.picture(), before);
}
