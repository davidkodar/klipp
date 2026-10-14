//! The grade's specialist editors (M3, flip readiness ruling 10-09): the
//! RGB and Hue/Sat curve editors, the camera encoding, the Layer Mixer's
//! inputs, and the External Matte's and Difference Key's image pickers.
//! Each is owed what shows it does the right thing, once, on the page its
//! node is first drawn on (`rules::LEAN_EDITORS`): it opens without a
//! write; one real edit through the drawn widget stores what it must, as
//! one undo step undone and redone exactly; its cancel, where it has one,
//! takes the edit back; and the edit changes the frame alike on the CPU
//! and the GPU.

use super::parity_controls::{placed, Grade, Page};
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, CurveAction, Interaction, Manifest, Report, Surface};
use crate::state::ColorTab;
use klipp_effects::registry::presentation::Widget;
use klipp_nodes::NodeKind;
use klipp_timeline::GradeAccess;

/// The editors this family drives, by their editor's name.
const EDITORS: [&str; 6] = ["curves", "huesat_curves", "camera_encoding", "layer_mixer_inputs", "matte_file", "plate_file"];

/// The catalog entries of the editors driven here, with the page each is
/// owed on.
fn entries() -> Vec<(super::parity_manifest::CatalogEntry, Surface, &'static str)> {
    catalog()
        .into_iter()
        .filter_map(|e| {
            let CatalogValue::Parameter { presentation, .. } = &e.value else { return None };
            let Widget::Custom(id) = presentation.widget else { return None };
            let id = EDITORS.iter().find(|x| **x == id)?;
            let page = super::parity_manifest::owed_page(&e)?;
            Some((e, page, *id))
        })
        .collect()
}

pub(super) fn require(manifest: &mut Manifest) {
    for (entry, page, _) in entries() {
        manifest.require_fixture(&entry, page, "sample");
    }
}

/// `start` with what nothing else writes: its history empty.
fn fresh(start: &Grade) -> Grade {
    start.twin()
}

/// One edit by `drive` from `start`: something changed that `check` holds
/// to what it must store, one step, undone and redone exactly.
fn edit_case(start: &Grade, drive: impl FnOnce(&mut Grade) -> Result<(), String>, check: impl FnOnce(&Grade) -> Result<(), String>) -> Result<Grade, String> {
    let mut g = fresh(start);
    g.drawn();
    let before = g.picture();
    drive(&mut g)?;
    g.drawn();
    if !g.unapplied.is_empty() {
        return Err(format!("unapplied {:?}", g.unapplied));
    }
    check(&g)?;
    let after = g.picture();
    if after == before {
        return Err("the edit changed nothing".into());
    }
    // The sequence's steps and the project's: a camera encoding is the
    // file's, a project step (#1986).
    if g.state.undo_depth() != 1 {
        return Err(format!("{} undo steps, not one", g.state.undo_depth()));
    }
    g.step(false)?;
    if g.picture() != before {
        return Err("undo doesn't give back the grade before".into());
    }
    g.step(true)?;
    if g.picture() != after {
        return Err("redo doesn't give back the edit".into());
    }
    Ok(g)
}

/// Opening the editor's page writes nothing.
fn opened(start: &Grade) -> Result<(), String> {
    let mut g = fresh(start);
    let before = g.picture();
    g.drawn();
    g.drawn();
    if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
        return Err(format!("opening wrote: {:?}", g.unapplied));
    }
    Ok(())
}

/// The canvas of the curve drawn under the label `under` (Hue/Sat's
/// stacked curves), or else the largest panel drawn.
fn canvas_of(g: &mut Grade, under: Option<&str>) -> Result<egui::Rect, String> {
    let Some(label) = under else { return canvas(g) };
    let at = g.drawn().into_iter().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} drawn"))?;
    g.canvases.iter().copied().filter(|r| r.min.y > at.y).min_by(|a, b| a.min.y.total_cmp(&b.min.y)).ok_or_else(|| format!("no canvas under {label:?}"))
}

/// The editor's canvas: the largest panel drawn.
fn canvas(g: &mut Grade) -> Result<egui::Rect, String> {
    g.drawn();
    // The largest panel drawn within the page (the page's own background
    // fills the whole screen).
    let screen = g.canvases.iter().map(|r| r.area()).fold(0.0, f32::max);
    g.canvases.iter().copied().filter(|r| r.area() < screen).max_by(|a, b| a.area().total_cmp(&b.area())).ok_or_else(|| format!("no curve canvas drawn: {:?}", g.canvases))
}

/// A point of the canvas, by its share across and up.
fn at(rect: egui::Rect, x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(rect.min.x + x * rect.width(), rect.max.y - y * rect.height())
}

/// The new point the curve drag below makes, by its share of the canvas.
const POINT: (f32, f32) = (0.3, 0.7);

/// A drag from an empty part of the curve's canvas to [`POINT`], released,
/// or taken back by Escape: it adds a point there.
fn drag_new_point(g: &mut Grade, under: Option<&str>, escape: bool) -> Result<(), String> {
    use super::parity_grade_numeric::{button, key};
    let rect = canvas_of(g, under)?;
    let (from, to) = (at(rect, POINT.0, POINT.1 - 0.15), at(rect, POINT.0, POINT.1));
    g.frame(vec![egui::Event::PointerMoved(from)]);
    g.frame(vec![button(from, true)]);
    for i in 1..=4 {
        g.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
    }
    if escape {
        g.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)]);
    }
    g.frame(vec![button(to, false)]);
    g.frame(vec![egui::Event::PointerGone]);
    Ok(())
}

/// Whether `points` holds one near [`POINT`].
fn has_point(points: &[[f32; 2]]) -> bool {
    points.iter().any(|p| (p[0] - POINT.0).abs() < 0.03 && (p[1] - POINT.1).abs() < 0.03)
}

/// The node's stored parameters.
fn params(g: &Grade) -> klipp_nodes::NodeParams {
    g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.clone()).unwrap_or_default()
}

/// The curve editor's cases on `start` (the editor drawn): a point added
/// by a drag, one step; the drag taken back by Escape; the edit drawn.
/// `curve` reads the edited curve's points.
fn curve_cases(report: &mut Report, name: &str, start: &Grade, under: Option<&'static str>, key: impl Fn(Interaction) -> CoverageKey, curve: fn(&klipp_nodes::NodeParams) -> Vec<[f32; 2]>) {
    report.case(&format!("{name} opened"), [key(Interaction::OpenWithoutWrite)], || opened(start));
    report.case(&format!("{name} point added by a drag"), [key(Interaction::CurvePoint(CurveAction::Add)), key(Interaction::UndoRedo)], || {
        edit_case(start, |g| drag_new_point(g, under, false), |g| if has_point(&curve(&params(g))) { Ok(()) } else { Err(format!("no point near {POINT:?}: {:?}", curve(&params(g)))) }).map(drop)
    });
    report.case(&format!("{name} drag cancelled"), [key(Interaction::CancelDrag)], || {
        let mut g = fresh(start);
        g.drawn();
        let before = g.picture();
        drag_new_point(&mut g, under, true)?;
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            return Err("Escape didn't take the drag back".into());
        }
        Ok(())
    });
    report.case_or_blocked(&format!("{name} drawn"), [key(Interaction::Pixels)], || {
        let mut g = fresh(start);
        drag_new_point(&mut g, under, false)?;
        super::parity_pixels::edit_drawn(&start.frame_sequence(), &g.frame_sequence())
    });
}

/// `kind` graded and drawn on `page`'s `tab`, its node selected; for a
/// type that isn't the Corrector, with the Corrector off (the pages edit
/// an enabled Corrector whatever node is selected).
fn on_tab(kind: NodeKind, tab: ColorTab) -> Result<Grade, String> {
    let (mut state, clip, node) = super::grade_inspector_tests::graded(kind).ok_or("the grade won't take one")?;
    if kind != NodeKind::Corrector {
        if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
            c.edit_grade(|g| g.nodes.iter_mut().filter(|n| n.kind == NodeKind::Corrector).for_each(|n| n.enabled = false));
        }
    }
    Ok(Grade::new(state, clip, node).on(Page::Tabbed(tab)).twin())
}

/// The text `label`'s first drawn place, clicked.
fn click_first(g: &mut Grade, label: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} drawn"))?;
    g.click_at(at);
    Ok(())
}

/// The text `label`'s last drawn place, clicked.
fn click_text(g: &mut Grade, label: &str) -> Result<(), String> {
    let at = g.drawn().into_iter().rev().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("no {label:?} drawn"))?;
    g.click_at(at);
    Ok(())
}

pub(super) fn run(report: &mut Report) {
    for (entry, page, editor) in entries() {
        let Some(ty) = klipp_nodes::registry::node_types().find(|t| t.desc.id == entry.type_id) else { continue };
        let key = |i: Interaction| CoverageKey::new(entry.type_id, &entry.parameter, page, "sample", i);
        let name = format!("{} {editor} on {page:?}", entry.type_id);
        match editor {
            "curves" => match on_tab(ty.kind, ColorTab::Curves) {
                Ok(start) => curve_cases(report, &name, &start, None, key, |p| p.curves.clone().unwrap_or_default().master),
                Err(e) => report.case(&name, [], || Err(e)),
            },
            "huesat_curves" => match on_tab(ty.kind, ColorTab::HueSat) {
                Ok(start) => {
                    curve_cases(report, &name, &start, Some("HUE vs HUE"), key, |p| p.huesat.clone().unwrap_or_default().hue_vs_hue);
                    // The eyedropper asks the app to sample the viewer's frame.
                    report.case(&format!("{name} eyedropper"), [key(Interaction::Eyedropper)], || {
                        // The left of the two icons on HUE vs HUE's label row
                        // (the right one resets the curve).
                        let mut g = fresh(&start);
                        let label = g.drawn().into_iter().find(|(t, _)| t == "HUE vs HUE").map(|(_, p)| p).ok_or("no HUE vs HUE drawn")?;
                        let icon = g.icons.iter().filter(|i| (i.center().y - label.y).abs() < 10.0 && i.center().x > label.x && i.width() <= 14.0).min_by(|a, b| a.min.x.total_cmp(&b.min.x)).copied().ok_or("no eyedropper on HUE vs HUE's row")?;
                        g.click_at(icon.center());
                        if !g.side.iter().any(|s| s.starts_with("PickHueCurve")) {
                            return Err(format!("the eyedropper asked for {:?}", g.side));
                        }
                        Ok(())
                    });
                }
                Err(e) => report.case(&name, [], || Err(e)),
            },
            "camera_encoding" => {
                let start = || -> Result<(Grade, klipp_core::MediaId), String> {
                    let (mut state, clip, node) = super::grade_inspector_tests::graded(ty.kind).ok_or("the grade won't take one")?;
                    let media = state.sequences[0].all_clips().find(|c| c.id == clip).and_then(|c| c.media_id()).ok_or("a clip of a file")?;
                    let fps = state.sequences[0].fps;
                    state.media_pool.add(klipp_media::MediaItem::new_synthetic_title(media, "shot".into(), klipp_core::RationalTime::from_secs_f64(2.0, 25), fps));
                    Ok((Grade::new(state, clip, node).on(Page::Tabbed(ColorTab::Adjustments)), media))
                };
                report.case(&format!("{name} opened"), [key(Interaction::OpenWithoutWrite)], || opened(&start()?.0));
                report.case(&format!("{name} picked"), [key(Interaction::SelectEncoding), key(Interaction::UndoRedo)], || {
                    let (s, media) = start()?;
                    edit_case(&s, |g| {
                        click_text(g, "Rec.709 / sRGB (no conversion)")?;
                        click_text(g, "Sony S-Log3")
                    }, |g| match g.state.media_pool.get(&media).map(|i| i.log_format) {
                        Some(klipp_core::LogFormat::SLog3) => Ok(()),
                        other => Err(format!("the file's encoding is {other:?}")),
                    }).map(drop)
                });
                // Its frame is the decoder's: the file's encoding converts the
                // source as it's read, and the Input Transform node draws
                // nothing itself (`rules::specialist`).
            }
            "layer_mixer_inputs" => {
                let start = || -> Result<Grade, String> {
                    let (mut state, clip, node) = placed(ty.kind).ok_or("the grade won't take one")?;
                    // Two layers: the Corrector, turned up, on top (port 0,
                    // which it already feeds), the untouched Source below
                    // (port 1), so a blend of the top one shows.
                    if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
                        c.edit_grade(|g| {
                            let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id);
                            for n in g.nodes.iter_mut() {
                                if n.kind == NodeKind::Corrector {
                                    n.params.exposure = Some(1.5);
                                }
                                if n.id == node {
                                    n.params.layer_mixer.get_or_insert_with(Default::default).inputs = vec![klipp_core::LayerMixerInput { opacity: 1.0, blend_mode: klipp_core::BlendMode::Normal }; 2];
                                }
                            }
                            if let Some(source) = source {
                                g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: node, to_port: 1 });
                            }
                        });
                    }
                    Ok(Grade::new(state, clip, node))
                };
                let blend = |g: &Grade| params(g).layer_mixer.and_then(|m| m.inputs.first().map(|i| i.blend_mode));
                let pick = |g: &mut Grade| -> Result<(), String> {
                    let shows = klipp_core::BlendMode::Normal.display_name();
                    // The first input's menu, then its item, drawn last.
                    click_first(g, shows)?;
                    click_text(g, klipp_core::BlendMode::Multiply.display_name())
                };
                report.case(&format!("{name} opened"), [key(Interaction::OpenWithoutWrite)], || opened(&start()?));
                report.case(&format!("{name} input's blend picked"), [key(Interaction::EditLayer), key(Interaction::UndoRedo)], || {
                    edit_case(&start()?, pick, |g| if blend(g) == Some(klipp_core::BlendMode::Multiply) { Ok(()) } else { Err(format!("the input's blend is {:?}", blend(g))) }).map(drop)
                });
                report.case_or_blocked(&format!("{name} drawn"), [key(Interaction::Pixels)], || {
                    let s = start()?;
                    let mut g = fresh(&s);
                    pick(&mut g)?;
                    super::parity_pixels::edit_drawn(&s.frame_sequence(), &g.frame_sequence())
                });
            }
            "matte_file" | "plate_file" => {
                let (choose, path_of): (&str, fn(&klipp_nodes::NodeParams) -> Option<std::path::PathBuf>) = if editor == "matte_file" {
                    ("Choose image\u{2026}", |p| p.matte_image_path.clone())
                } else {
                    ("Choose plate\u{2026}", |p| p.difference_key.as_ref().and_then(|d| d.plate_path.clone()))
                };
                let image = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/effect_preview_reference.png");
                let start = || -> Result<Grade, String> {
                    let (mut state, clip, node) = placed(ty.kind).ok_or("the grade won't take one")?;
                    // A Difference Key draws its plate picker once it's on.
                    if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
                        c.edit_grade(|g| g.nodes.iter_mut().filter(|n| n.id == node || n.kind == NodeKind::Corrector).for_each(|n| {
                            if n.id == node && editor == "plate_file" {
                                // On, and a tolerance a plate unlike the frame
                                // stays under: with no plate it keys the whole
                                // frame, with one almost none of it.
                                let dk = n.params.difference_key.get_or_insert_with(Default::default);
                                dk.enabled = true;
                                dk.tolerance = 0.9;
                            }
                            // The correction the matte selects, turned up, so
                            // the matte shows in the frame.
                            if n.kind == NodeKind::Corrector {
                                n.params.exposure = Some(1.5);
                            }
                        }));
                    }
                    Ok(Grade::new(state, clip, node))
                };
                // The picker answered with `chosen`, its commands applied as
                // the app drains them: how many pickers it asked for.
                let pick = |g: &mut Grade, chosen: Option<std::path::PathBuf>| -> Result<usize, String> {
                    let mut clicked = Ok(());
                    let (asked, queued) = crate::file_dialog::answering(chosen, || clicked = click_text(g, choose));
                    clicked?;
                    for cmd in queued {
                        g.apply(cmd);
                    }
                    Ok(asked)
                };
                report.case(&format!("{name} opened"), [key(Interaction::OpenWithoutWrite)], || opened(&start()?));
                // Shown under a condition (the plate's, with the key on): its
                // picker comes and goes with it.
                if let CatalogValue::Parameter { desc, presentation } = &entry.value {
                    if presentation.visible_if.is_some() {
                        report.case(&format!("{name} shown with its condition"), [key(Interaction::ConditionalVisibility)], || super::parity_controls::visibility_case(&start()?, ty, desc, choose));
                    }
                }
                report.case(&format!("{name} picked"), [key(Interaction::FileSelect), key(Interaction::UndoRedo)], || {
                    edit_case(&start()?, |g| pick(g, Some(image.clone())).map(drop), |g| if path_of(&params(g)).as_deref() == Some(image.as_path()) { Ok(()) } else { Err(format!("the node holds {:?}", path_of(&params(g)))) }).map(drop)
                });
                report.case(&format!("{name} picker dismissed"), [key(Interaction::FileCancel)], || {
                    let mut g = fresh(&start()?);
                    g.drawn();
                    let before = (g.picture(), g.state.active_history().undo_count());
                    let asked = pick(&mut g, None)?;
                    if asked != 1 {
                        return Err(format!("{choose} asked for {asked} pickers, not one"));
                    }
                    if (g.picture(), g.state.active_history().undo_count()) != before || !g.unapplied.is_empty() {
                        return Err(format!("a dismissed picker wrote: {:?}", g.unapplied));
                    }
                    Ok(())
                });
                report.case(&format!("{name} cleared"), [key(Interaction::FileClear)], || {
                    let mut with = fresh(&start()?);
                    pick(&mut with, Some(image.clone()))?;
                    edit_case(&with.twin(), |g| click_text(g, "Clear"), |g| if path_of(&params(g)).is_none() { Ok(()) } else { Err(format!("the node still holds {:?}", path_of(&params(g)))) }).map(drop)
                });
                report.case_or_blocked(&format!("{name} drawn"), [key(Interaction::Pixels)], || {
                    let s = start()?;
                    let mut g = fresh(&s);
                    pick(&mut g, Some(image.clone()))?;
                    super::parity_pixels::edit_drawn(&s.frame_sequence(), &g.frame_sequence())
                });
            }
            _ => {}
        }
    }
}

#[test]
fn the_grade_editors_do_what_they_show() {
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let mut report = Report::default();
    report.family("grade editors", run);
    report.family_result().unwrap();
    assert!(report.blocked.is_empty(), "an editor's edit left the native route: {:?}", report.blocked);
    let missing: Vec<_> = manifest.required.difference(&report.passed).collect();
    assert!(missing.is_empty(), "owed and not earned: {missing:#?}");
}

