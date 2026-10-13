//! The analysis effects' workflow in the inspector (M3). Deflicker and
//! Video Stabilization measure the clip's media before they do anything,
//! and the stack draws a status widget for it
//! (`effects_tab::deflicker_status`, `stabilization_status`). Each case
//! drives that widget, or the row its measurement is smoothed by, and
//! requires what it must leave: the picture, the undo depth and the
//! analysis commands it sends.
//!
//! A measurement is the media file's analysis (M4 step 9), held by the
//! app by its asset key once found ([`measure`]), never written onto the
//! clip. It lands from a worker while the user may be dragging: it is
//! applied as the app's drain applies it (the open drag closed first,
//! then `proxies::apply_analysis_result`), so the case drives the code
//! the app runs.

use super::parity_harness::{fixture, Run};
use super::parity_manifest::{catalog, CatalogValue, CoverageKey, Interaction, Manifest, Report, Surface};
use klipp_core::{AppEvent, ClipId};
use klipp_effects::registry::{effect_types, EffectType};
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip;

/// The fixture: the type's sample on a two-second clip.
pub(super) const VARIANT: &str = "sample";

/// An analysis effect: its type, the workflow it shows, the buttons its
/// status draws, the row its measurement is smoothed by, and a
/// measurement of the fixture's clip.
pub(super) struct Analysis {
    pub(super) tag: &'static str,
    pub(super) workflow: &'static str,
    reanalyse: &'static str,
    row_key: &'static str,
    pub(super) kind: klipp_analysis::assets::AssetKind,
    /// A measurement of the fixture's media, as the analysis keeps it.
    pub(super) measured: fn() -> serde_json::Value,
    in_progress: fn(&mut crate::state::AppState, ClipId),
    pub(super) complete: fn(ClipId) -> AppEvent,
    /// The undo steps a drag held past a result leaves: the drag closed
    /// before the result as its own step, and the rest as one more.
    result_drag_steps: usize,
}

pub(super) const ANALYSES: &[Analysis] = &[
    Analysis {
        tag: "deflicker",
        workflow: "deflicker",
        reanalyse: "Re-analyse",
        row_key: "window_secs",
        kind: klipp_analysis::assets::AssetKind::Deflicker,
        measured: || {
            let data = klipp_effects::deflicker::DeflickerData { luma: (0..48).map(|i| 0.5 + 0.02 * ((i % 3) as f32)).collect(), fps: 24.0 };
            serde_json::to_value(data).expect("serialises")
        },
        in_progress: |state, clip| {
            state.deflicker_in_progress.insert(clip);
        },
        complete: |clip_id| AppEvent::DeflickerComplete { clip_id },
        result_drag_steps: 2,
    },
    Analysis {
        tag: "video_stabilization",
        workflow: "video_stabilization",
        reanalyse: "Re-analyze",
        row_key: "smoothing",
        kind: klipp_analysis::assets::AssetKind::Stabilisation,
        measured: || {
            use klipp_effects::rolling_shutter::Homography;
            use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, StabilizationData};
            let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 2.0 } else { -1.5 }], [0.0, 1.0, 0.5], [0.0, 0.0, 1.0]] };
            let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
            let accumulated = accumulate_trajectory(&frame_transforms);
            let trajectory = accumulated.iter().map(decompose_homography).collect();
            serde_json::to_value(StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 }).expect("serialises")
        },
        in_progress: |state, clip| {
            state.stabilization_in_progress.insert(clip);
        },
        complete: |clip_id| AppEvent::StabilizationComplete { clip_id },
        result_drag_steps: 2,
    },
];

/// `state` holding `a`'s measurement of `clip`'s media, as the app holds
/// a found analysis (M4 step 9): by the file's asset key. The media is
/// put in the pool as a stand-in file that exists, as a key reads it.
pub(super) fn measure(state: &mut crate::state::AppState, clip: &klipp_timeline::Clip, a: &Analysis) {
    let Some(media) = clip.media_id() else { return };
    if state.media_pool.get(&media).is_none() {
        let Ok(path) = std::env::current_exe() else { return };
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.id = media;
        item.fps = klipp_core::Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        state.media_pool.add(item);
    }
    let Some(key) = state.media_pool.get(&media).and_then(|item| klipp_analysis::assets::asset_key(a.kind, item)) else { return };
    state.analyses.insert(key, std::sync::Arc::new((a.measured)()));
}

/// Whether `state` holds `a`'s measurement of `clip`'s media.
fn holds_measurement(state: &crate::state::AppState, clip: &klipp_timeline::Clip, a: &Analysis) -> bool {
    state.analysis_of(clip, a.kind).is_some()
}

pub(super) fn effect_type(tag: &str) -> &'static EffectType {
    effect_types().find(|t| t.tag == tag).unwrap()
}

/// Every specialist entry this family covers, required on the effects
/// stack: the analysis effects' workflows, the frei0r plug-in editor, the
/// 3D LUT effect's file and a media source's item.
pub(super) fn require(manifest: &mut Manifest) {
    require_analyses(manifest);
    require_frei0r(manifest);
    require_lut_file(manifest);
    require_media_item(manifest);
}

fn require_analyses(manifest: &mut Manifest) {
    for entry in catalog() {
        if let CatalogValue::Workflow(id) = entry.value {
            if ANALYSES.iter().any(|a| a.workflow == id) {
                manifest.require_fixture(&entry, Surface::EffectsStack, VARIANT);
            }
        }
    }
}

/// The frei0r effect's plug-in editor: the catalog entries its one custom
/// widget draws (the plug-in it is, and its parameters' values).
pub(super) fn frei0r_entries() -> Vec<super::parity_manifest::CatalogEntry> {
    catalog().into_iter().filter(|e| matches!(e.value, CatalogValue::Parameter { presentation, .. } if presentation.widget == klipp_effects::registry::presentation::Widget::Custom("frei0r_plugin"))).collect()
}

fn require_frei0r(manifest: &mut Manifest) {
    for entry in frei0r_entries() {
        manifest.require_fixture(&entry, Surface::EffectsStack, VARIANT);
    }
}

/// The inspector over a clone of `clip`, `prepare`d (an analysis in
/// progress, say).
pub(super) fn one(clip: &klipp_timeline::Clip, prepare: impl Fn(&mut Run)) -> Run {
    let mut run = Run::new(clip.clone());
    prepare(&mut run);
    run
}

/// What a case's edit must leave besides its picture: nothing unapplied,
/// the undo depth `undo`, and exactly the side commands `side`, in order.
fn holds(new: &Run, undo: usize, side: &[String]) -> Result<(), String> {
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    let depth = new.state.active_history().undo_count();
    if depth != undo {
        return Err(format!("undo depth {depth}, want {undo}"));
    }
    if new.side != side {
        return Err(format!("side commands {:?}, want {side:?}", new.side));
    }
    Ok(())
}

/// The workflow cases for one analysis effect.
fn run_analysis(report: &mut Report, a: &Analysis) {
    let ty = effect_type(a.tag);
    let id = ty.desc.id;
    let parameter = format!("@workflow/{}", a.workflow);
    let key = |interaction| CoverageKey::new(id, &parameter, Surface::EffectsStack, VARIANT, interaction);
    let clip = fixture(&(ty.sample)());
    let clip_id = clip.id;
    let measured = |r: &mut Run| measure(&mut r.state, &clip, a);
    let desc = ty.desc.params.iter().find(|p| p.key == a.row_key).unwrap();
    let row = ty.presentation(desc).label.unwrap_or(desc.label);

    report.case(&format!("{id} opened"), [key(Interaction::OpenWithoutWrite)], || {
        let mut new = one(&clip, |_| {});
        let before = new.clip().picture_v2()?;
        new.frame(Vec::new(), egui::Modifiers::NONE);
        holds(&new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("opening it wrote the picture".into());
        }
        Ok(())
    });

    // Not measured, it starts by itself: no button. Measured, the button
    // asks for the measurement again and writes nothing.
    report.case(&format!("{id} {}", a.reanalyse), [key(Interaction::StartAnalysis)], || {
        let mut unmeasured = one(&clip, |_| {});
        if unmeasured.click_text("", a.reanalyse) {
            return Err(format!("{:?} before there is a measurement", a.reanalyse));
        }
        holds(&unmeasured, 0, &[])?;
        let mut new = one(&clip, measured);
        if !new.click_text("", a.reanalyse) {
            return Err(format!("no {:?}", a.reanalyse));
        }
        let want = format!("{:?}", if a.tag == "deflicker" { klipp_core::AppCommand::AnalyzeDeflicker { clip_id } } else { klipp_core::AppCommand::AnalyzeStabilization { clip_id } });
        holds(&new, 0, &[want])
    });

    report.case(&format!("{id} cancelled"), [key(Interaction::CancelAnalysis)], || {
        let mut new = one(&clip, |r| (a.in_progress)(&mut r.state, clip_id));
        if !new.click_text("", "Cancel") {
            return Err("no Cancel while it measures".into());
        }
        let want = format!("{:?}", if a.tag == "deflicker" { klipp_core::AppCommand::CancelDeflicker { clip_id } } else { klipp_core::AppCommand::CancelStabilization { clip_id } });
        holds(&new, 0, &[want])
    });

    // On a measured clip, a drag of that row is one step and sends
    // nothing more: the smoothing is applied where the measurement is read
    // (M4 step 9), unlike a measurement landing mid-drag below (#1338).
    report.case(&format!("{id} drag over a measurement"), [key(Interaction::UndoRedo)], || {
        let mut new = one(&clip, measured);
        {
            let run = &mut new;
            let (_, at) = run.row(row).ok_or_else(|| format!("no row {row:?}"))?;
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            run.button(at + egui::vec2(18.0, 0.0), egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
        }
        holds(&new, 1, &[])?;
        // What it must leave: the clip with the row's new value set by the
        // document command, and nothing else changed.
        let effect = test_clip::effects(&clip)[0].clone();
        let to = super::parity_numeric::stored(&test_clip::effects(new.clip())[0], desc);
        if to == super::parity_numeric::stored(&effect, desc) {
            return Err(format!("the drag left {} where it was", desc.key));
        }
        let mut oracle = one(&clip, measured);
        oracle.apply(klipp_core::AppCommand::UpdateBuiltinEffect { clip_id, index: 0, effect: super::parity_numeric::with_field(&effect, desc, to)? });
        if oracle.clip().picture_v2()? != new.clip().picture_v2()? {
            return Err("the drag left another picture than setting its value by the document command does".into());
        }
        Ok(())
    });

    // A measurement arriving mid-drag on the row it is smoothed by: the
    // drag is its own step, closed before the result lands; the result is
    // kept; and an undo takes back the drag alone.
    report.case(&format!("{id} result during a drag"), [key(Interaction::ResultDuringDrag), key(Interaction::UndoRedo)], || {
        let mut new = one(&clip, |r| (a.in_progress)(&mut r.state, clip_id));
        let before = new.clip().picture_v2()?;
        {
            let run = &mut new;
            let (_, at) = run.row(row).ok_or_else(|| format!("no row {row:?}"))?;
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            // The worker's result, as the drain applies it.
            let event = (a.complete)(clip_id);
            if super::event_writes::event_writes(&event) {
                run.state.close_path_drag_before_edit();
            }
            if !super::proxies::apply_analysis_result(&mut run.state, &event) {
                return Err("the result wasn't applied".into());
            }
            // The worker kept it as the file's asset; the app's next look
            // finds it (`ensure_analyses`).
            measure(&mut run.state, &clip, a);
            let at_result = test_clip::effects(run.clip());
            for dx in [18.0, 24.0, 30.0, 36.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            if test_clip::effects(run.clip()) == at_result.as_slice() {
                return Err("the drag stopped at the result".into());
            }
            run.button(at + egui::vec2(36.0, 0.0), egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
        }
        if new.clip().picture_v2()? == before {
            return Err("the drag changed nothing".into());
        }
        if !holds_measurement(&new.state, &clip, a) {
            return Err("the result wasn't kept".into());
        }
        if new.state.deflicker_in_progress.contains(&clip_id) || new.state.stabilization_in_progress.contains(&clip_id) {
            return Err("still measuring after the result".into());
        }
        // The steps the gesture leaves (#1338): the path drag closed as
        // its own step before the result, and the drag going on as one
        // more.
        let depth = new.state.active_history().undo_count();
        if depth != a.result_drag_steps {
            return Err(format!("undo depth {depth}, want {}", a.result_drag_steps));
        }
        if !new.unapplied.is_empty() {
            return Err(format!("unapplied: {:?}", new.unapplied));
        }
        // Undo all the way back: the picture before the drag, and the
        // measurement still on the clip; redo: the picture after.
        let after = new.clip().picture_v2()?;
        for _ in 0..depth {
            if !new.step(false) {
                return Err("an undo step is missing".into());
            }
        }
        if new.clip().picture_v2()? != before {
            return Err("undo doesn't give back the picture before the drag".into());
        }
        if !holds_measurement(&new.state, &clip, a) {
            return Err("undoing the drag took the measurement".into());
        }
        for _ in 0..depth {
            if !new.step(true) {
                return Err("a redo step is missing".into());
            }
        }
        if new.clip().picture_v2()? != after {
            return Err("redo doesn't give back the picture after the drag".into());
        }
        Ok(())
    });
}

/// A plug-in no machine has, declaring one parameter of each kind the
/// editor draws: what the frei0r cases drive, registered once.
fn parity_plugin() -> klipp_frei0r::Frei0rPluginInfo {
    use klipp_frei0r::{ColorModel, ParamInfo, ParamKind, PluginType};
    let param = |name: &str, kind| ParamInfo { name: name.into(), kind, explanation: String::new() };
    let info = klipp_frei0r::Frei0rPluginInfo {
        path: std::path::PathBuf::from("/nonexistent/klipp-parity/parity_fx.so"),
        name: "ParityFx".into(),
        author: "klipp".into(),
        plugin_type: PluginType::Filter,
        color_model: ColorModel::Rgba8888,
        frei0r_version: 1,
        major_version: 0,
        minor_version: 1,
        explanation: String::new(),
        params: vec![param("Amount", ParamKind::Double), param("Active", ParamKind::Bool), param("Centre", ParamKind::Position), param("Caption", ParamKind::String), param("Tint", ParamKind::Color)],
    };
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| klipp_effects::frei0r_effect::_register_for_test(info.clone()));
    info
}

/// Type `text` into the number field at `at` and press Enter, as a user
/// does: a click to focus, select all, the text, Enter.
pub(super) fn type_into(run: &mut Run, at: egui::Pos2, text: &str) {
    for pressed in [true, false, true, false] {
        run.button(at, egui::PointerButton::Primary, pressed);
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    run.key(egui::Key::A, egui::Modifiers::COMMAND);
    run.frame(vec![egui::Event::Text(text.into())], egui::Modifiers::NONE);
    run.key(egui::Key::Enter, egui::Modifiers::NONE);
    run.frame(Vec::new(), egui::Modifiers::NONE);
}

/// The first number drawn right of the text `label` on its line.
pub(super) fn number_right_of(run: &mut Run, label: &str) -> Option<egui::Pos2> {
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let at = drawn.iter().rev().find(|(t, _)| t == label).map(|(_, p)| *p)?;
    drawn.iter().filter(|(t, p)| (p.y - at.y).abs() < 6.0 && p.x > at.x && t.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit())).min_by(|a, b| a.1.x.total_cmp(&b.1.x)).map(|(_, p)| *p)
}

/// Click right of `label` on its line, nearest first, until the clip's
/// effect changes: a control drawn without text of its own (a frei0r
/// switch's box).
fn click_right_of(run: &mut Run, label: &str) -> bool {
    let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
    let Some(at) = drawn.iter().rev().find(|(t, _)| t == label).map(|(_, p)| *p) else { return false };
    let before = format!("{:?}", test_clip::effects(run.clip()));
    let mut x = at.x + 8.0;
    while x < at.x + 160.0 {
        let p = egui::pos2(x, at.y);
        run.button(p, egui::PointerButton::Primary, true);
        run.button(p, egui::PointerButton::Primary, false);
        run.frame(Vec::new(), egui::Modifiers::NONE);
        if format!("{:?}", test_clip::effects(run.clip())) != before {
            return true;
        }
        x += 4.0;
    }
    false
}

/// The frei0r plug-in editor's cases.
fn run_frei0r(report: &mut Report) {
    let entries = frei0r_entries();
    if entries.is_empty() {
        report.case("frei0r editor", [], || Err("no catalog entry draws the frei0r editor".into()));
        return;
    }
    // The editor is one widget over its entries: each case earns its key
    // on all of them.
    let key = |interaction| entries.iter().map(|e| CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, VARIANT, interaction)).collect::<Vec<_>>();
    let plugin = parity_plugin();
    let defaults: Vec<klipp_frei0r::Frei0rValue> = plugin.params.iter().map(|p| klipp_frei0r::Frei0rValue::default_for(p.kind)).collect();
    let effect = BuiltinEffect::Frei0r { type_id: plugin.type_id(), params: defaults.clone() };
    let clip = fixture(&effect);

    report.case("frei0r opened", key(Interaction::OpenWithoutWrite), || {
        let mut new = one(&clip, |_| {});
        let before = new.clip().picture_v2()?;
        {
            let run = &mut new;
            let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
            if !drawn.iter().any(|(t, _)| t == "Amount") {
                return Err("the plug-in's rows aren't drawn".into());
            }
        }
        holds(&new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("opening it wrote the picture".into());
        }
        Ok(())
    });

    // A plug-in this machine doesn't have: said, nothing
    // written, and no rows for values it can't name.
    report.case("frei0r missing", key(Interaction::MissingAsset), || {
        let missing = fixture(&BuiltinEffect::Frei0r { type_id: "frei0r:/nonexistent/gone.so:Gone".into(), params: vec![klipp_frei0r::Frei0rValue::Double(0.25)] });
        let mut new = one(&missing, |_| {});
        {
            let run = &mut new;
            let drawn = run.frame(Vec::new(), egui::Modifiers::NONE);
            if !drawn.iter().any(|(t, _)| t.starts_with("⚠ Plugin not installed")) {
                return Err("no missing-plug-in notice".into());
            }
        }
        holds(&new, 0, &[])
    });

    // Picked from the effects browser, as its double-click and drop do:
    // the effect added, with the plug-in's declared defaults, by path as a
    // legacy effect node (M3 step 19).
    report.case("frei0r picked", key(Interaction::PluginSelect), || {
        let base = fixture(&BuiltinEffect::GaussianBlur { radius: 3.0 });
        let mut new = one(&base, |_| {});
        let source = crate::browser::effect_registry::EffectSource::Frei0r(plugin.clone());
        {
            let run = &mut new;
            let cmds = crate::browser::effects_tab::effect_commands_in(&run.state, &source, run.clip)?;
            for cmd in cmds {
                run.apply(cmd);
            }
        }
        if !new.unapplied.is_empty() {
            return Err(format!("unapplied: {:?}", new.unapplied));
        }
        let want = vec![BuiltinEffect::GaussianBlur { radius: 3.0 }, effect.clone()];
        if test_clip::effects(new.clip()) != want.as_slice() {
            return Err(format!("added {:?}, want {want:?}", test_clip::effects(new.clip())));
        }
        Ok(())
    });

    // Each of its values edited through its own control:
    // the same picture, one step, undone and redone exactly.
    type Drive = fn(&mut Run) -> Result<(), String>;
    // What each edit must leave: the declared defaults with that one value
    // set, every other value as it was.
    type Want = fn(&mut [klipp_frei0r::Frei0rValue]);
    use klipp_frei0r::Frei0rValue as V;
    let edits: [(&str, Drive, Want); 5] = [
        ("Amount", |r| {
            let at = number_right_of(r, "Amount").ok_or("no number for Amount")?;
            type_into(r, at, "0.4");
            Ok(())
        }, |p| p[0] = V::Double(0.4)),
        ("Active", |r| if click_right_of(r, "Active") { Ok(()) } else { Err("no switch for Active".into()) }, |p| p[1] = V::Bool(true)),
        ("Centre X", |r| {
            let at = number_right_of(r, "X").ok_or("no number for X")?;
            type_into(r, at, "0.3");
            Ok(())
        }, |p| p[2] = V::Position([0.3, 0.5])),
        ("Centre Y", |r| {
            let at = number_right_of(r, "Y").ok_or("no number for Y")?;
            type_into(r, at, "0.7");
            Ok(())
        }, |p| p[2] = V::Position([0.5, 0.7])),
        ("Caption", |r| {
            let drawn = r.frame(Vec::new(), egui::Modifiers::NONE);
            let at = drawn.iter().rev().find(|(t, _)| t == "Caption").map(|(_, p)| *p).ok_or("no Caption")?;
            let field = egui::pos2(at.x + 90.0, at.y);
            r.button(field, egui::PointerButton::Primary, true);
            r.button(field, egui::PointerButton::Primary, false);
            r.frame(vec![egui::Event::Text("hi".into())], egui::Modifiers::NONE);
            r.key(egui::Key::Enter, egui::Modifiers::NONE);
            r.frame(Vec::new(), egui::Modifiers::NONE);
            Ok(())
        }, |p| p[3] = V::String("hi".into())),
    ];
    let colour: (&str, Drive, Want) = ("Tint", |r| {
        // Its button opens the picker; its R value typed there, then the
        // picker closed by a click away from it, as a user does.
        let drawn = r.frame(Vec::new(), egui::Modifiers::NONE);
        let at = drawn.iter().rev().find(|(t, _)| t == "Tint").map(|(_, p)| *p).ok_or("no Tint")?;
        let mut x = at.x + 8.0;
        let red = loop {
            if x > at.x + 160.0 {
                return Err("no colour button for Tint".into());
            }
            let p = egui::pos2(x, at.y);
            r.button(p, egui::PointerButton::Primary, true);
            r.button(p, egui::PointerButton::Primary, false);
            let drawn = r.frame(Vec::new(), egui::Modifiers::NONE);
            if let Some((_, red)) = drawn.iter().rev().find(|(t, _)| t.starts_with("R ")) {
                break *red;
            }
            x += 4.0;
        };
        type_into(r, red, "128");
        let away = egui::pos2(500.0, 2900.0);
        r.button(away, egui::PointerButton::Primary, true);
        r.button(away, egui::PointerButton::Primary, false);
        r.frame(Vec::new(), egui::Modifiers::NONE);
        Ok(())
    // The picker's bytes, as the editor stores them: R 128 of 255.
    }, |p| p[4] = V::Color([128.0 / 255.0, 1.0, 1.0]));
    let mut edits = edits.to_vec();
    edits.push(colour);
    for (what, drive, want) in edits {
        report.case(&format!("frei0r {what}"), [key(Interaction::PluginParameter), key(Interaction::UndoRedo)].concat(), || {
            let mut new = one(&clip, |_| {});
            let before = new.clip().picture_v2()?;
            drive(&mut new)?;
            let after = new.clip().picture_v2()?;
            let mut params = defaults.clone();
            want(&mut params);
            let expected = [BuiltinEffect::Frei0r { type_id: plugin.type_id(), params }];
            if test_clip::effects(new.clip()) != expected.as_slice() {
                return Err(format!("{:?}, want {expected:?}", test_clip::effects(new.clip())));
            }
            // One edit, one step.
            let depth = 1;
            holds(&new, depth, &[])?;
            for _ in 0..depth {
                if !new.step(false) {
                    return Err("an undo step is missing".into());
                }
            }
            if new.clip().picture_v2()? != before {
                return Err("undo doesn't give back the picture before".into());
            }
            for _ in 0..depth {
                if !new.step(true) {
                    return Err("a redo step is missing".into());
                }
            }
            if new.clip().picture_v2()? != after {
                return Err("redo doesn't give back the picture after".into());
            }
            Ok(())
        });
    }
}

pub(super) fn run(report: &mut Report) {
    for a in ANALYSES {
        report.family(a.tag, |report| run_analysis(report, a));
    }
    report.family("frei0r", run_frei0r);
    report.family("3D LUT file", run_lut_file);
    report.family("media source item", run_media_item);
}

/// The 3D LUT effect's file editor (#1746): the catalog entry its custom
/// widget draws.
pub(super) fn lut_file_entries() -> Vec<super::parity_manifest::CatalogEntry> {
    catalog().into_iter().filter(|e| matches!(e.value, CatalogValue::Parameter { presentation, .. } if presentation.widget == klipp_effects::registry::presentation::Widget::Custom("effect_lut_file"))).collect()
}

fn require_lut_file(manifest: &mut Manifest) {
    for entry in lut_file_entries() {
        manifest.require_fixture(&entry, Surface::EffectsStack, VARIANT);
    }
}

/// A clip with a 3D LUT effect reading `path`.
pub(super) fn lut_clip(path: &str) -> klipp_timeline::Clip {
    fixture(&BuiltinEffect::Lut3D { path: path.into(), opacity: 1.0, tetrahedral: true, prepared: None })
}

/// The LUT effect's stored file.
fn lut_path_of(run: &Run) -> Option<String> {
    test_clip::effects(run.clip()).iter().find_map(|e| match e {
        BuiltinEffect::Lut3D { path, .. } => Some(path.clone()),
        _ => None,
    })
}

/// One edit of the file from `start`, `drive`n through the editor: the
/// file stored is `want`, in one step, undone and redone exactly.
fn lut_file_edit(start: &klipp_timeline::Clip, want: &str, drive: impl FnOnce(&mut Run) -> Result<(), String>) -> Result<(), String> {
    let mut new = one(start, |_| {});
    let before = new.clip().picture_v2()?;
    drive(&mut new)?;
    if lut_path_of(&new).as_deref() != Some(want) {
        return Err(format!("stored {:?}, want {want:?}", lut_path_of(&new)));
    }
    holds(&new, 1, &[])?;
    let after = new.clip().picture_v2()?;
    if !new.step(false) || new.clip().picture_v2()? != before {
        return Err("undo doesn't give back the picture before".into());
    }
    if !new.step(true) || new.clip().picture_v2()? != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

/// The first bundled look: its category, its name and its file.
pub(super) fn first_bundled_look() -> Option<(String, String, std::path::PathBuf)> {
    let dir = klipp_core::assets::bundled_luts_dir()?;
    crate::panels::node_inspector::bundled_luts(dir).into_iter().find_map(|(category, files)| {
        let file = files.into_iter().next()?;
        Some((category, file.file_stem()?.to_string_lossy().into_owned(), file))
    })
}

/// The 3D LUT effect's file editor's cases: opened, a file missing, Browse
/// dismissed and answered, a bundled look picked and drawn, cleared.
fn run_lut_file(report: &mut Report) {
    let entries = lut_file_entries();
    if entries.is_empty() {
        report.case("3D LUT file", [], || Err("no catalog entry draws the LUT file editor".into()));
        return;
    }
    let key = |interaction| entries.iter().map(|e| CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, VARIANT, interaction)).collect::<Vec<_>>();
    let gone = "/nonexistent/klipp-parity/look.cube";
    let library = klipp_core::assets::bundled_luts_dir();
    let portable = |p: &std::path::Path| klipp_core::assets::portable_lut_path(p, library);

    report.case("3D LUT file opened", key(Interaction::OpenWithoutWrite), || {
        let mut new = one(&lut_clip(gone), |_| {});
        let before = new.clip().picture_v2()?;
        let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
        if !drawn.iter().any(|(t, _)| t == "look") {
            return Err("the file isn't named".into());
        }
        holds(&new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("opening it wrote the picture".into());
        }
        Ok(())
    });
    // A file this machine doesn't have: named, said, nothing written.
    report.case("3D LUT file missing", key(Interaction::MissingAsset), || {
        let mut new = one(&lut_clip(gone), |_| {});
        let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
        if !drawn.iter().any(|(t, _)| t == "\u{26a0} LUT file not found") {
            return Err("the missing file isn't said".into());
        }
        holds(&new, 0, &[])
    });
    report.case("3D LUT file browse cancelled", key(Interaction::FileCancel), || {
        let mut new = one(&lut_clip(gone), |_| {});
        let before = new.clip().picture_v2()?;
        let mut clicked = false;
        let asked = crate::file_dialog::cancelling(|| clicked = new.click_last("Browse\u{2026}"));
        if !clicked || asked != 1 {
            return Err(format!("Browse clicked {clicked}, asked for {asked} pickers"));
        }
        holds(&new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("a dismissed Browse wrote".into());
        }
        Ok(())
    });
    report.case("3D LUT file browsed", [key(Interaction::FileSelect), key(Interaction::UndoRedo)].concat(), || {
        let (_, _, file) = first_bundled_look().ok_or("no bundled look to browse to")?;
        lut_file_edit(&lut_clip(gone), &portable(&file), |r| {
            let mut clicked = false;
            let (asked, cmds) = crate::file_dialog::answering(Some(file.clone()), || clicked = r.click_last("Browse\u{2026}"));
            if !clicked || asked != 1 {
                return Err(format!("Browse clicked {clicked}, asked for {asked} pickers"));
            }
            for cmd in cmds {
                r.apply(cmd);
            }
            Ok(())
        })
    });
    report.case("3D LUT file picked from the bundled looks", [key(Interaction::FileSelect), key(Interaction::UndoRedo)].concat(), || {
        let (category, name, file) = first_bundled_look().ok_or("no bundled look to pick")?;
        lut_file_edit(&lut_clip(""), &portable(&file), |r| {
            for text in ["Bundled", category.as_str(), name.as_str()] {
                if !r.click_last(text) {
                    return Err(format!("{text:?} isn't drawn"));
                }
            }
            Ok(())
        })
    });
    report.case("3D LUT file cleared", [key(Interaction::FileClear), key(Interaction::UndoRedo)].concat(), || {
        lut_file_edit(&lut_clip(gone), "", |r| if r.click_last("Clear") { Ok(()) } else { Err("no Clear".into()) })
    });
}

/// A media source's item editor (#1746): the catalog entry its custom
/// widget draws.
fn media_item_entries() -> Vec<super::parity_manifest::CatalogEntry> {
    catalog().into_iter().filter(|e| matches!(e.value, CatalogValue::Parameter { presentation, .. } if presentation.widget == klipp_effects::registry::presentation::Widget::Custom("effect_media"))).collect()
}

fn require_media_item(manifest: &mut Manifest) {
    for entry in media_item_entries() {
        manifest.require_fixture(&entry, Surface::EffectsStack, VARIANT);
    }
}

/// A clip with a media source reading `media`.
fn media_clip(media: &str) -> klipp_timeline::Clip {
    fixture(&BuiltinEffect::MediaSource { media: media.into(), offset: 0.0 })
}

/// The media source's stored item.
fn media_of(run: &Run) -> Option<String> {
    test_clip::effects(run.clip()).iter().find_map(|e| match e {
        BuiltinEffect::MediaSource { media, .. } => Some(media.clone()),
        _ => None,
    })
}

/// A video item `id` in the pool, named `name`, its file at `path`.
fn pool_item(run: &mut Run, id: klipp_core::MediaId, name: &str, path: &str) {
    let mut item = klipp_media::MediaItem::new_synthetic_title(id, name.into(), klipp_core::Timecode::from_secs_f64(4.0, 24), klipp_core::Fps::new(24, 1));
    item.kind = klipp_media::MediaItemKind::File;
    item.path = path.into();
    item.video_codec = "h264".into();
    run.state.media_pool.add(item);
}

/// A media source's item editor's cases: opened, an item gone from the
/// pool and a file gone from the machine said, an item picked in one step,
/// undone and redone exactly.
fn run_media_item(report: &mut Report) {
    let entries = media_item_entries();
    if entries.is_empty() {
        report.case("media source item", [], || Err("no catalog entry draws the media item editor".into()));
        return;
    }
    let key = |interaction| entries.iter().map(|e| CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, VARIANT, interaction)).collect::<Vec<_>>();
    let gone = "/nonexistent/klipp-parity/plate.mov";
    let item = klipp_core::MediaId::new();
    let with_item = |r: &mut Run| pool_item(r, item, "plate.mov", gone);

    report.case("media source item opened", key(Interaction::OpenWithoutWrite), || {
        let mut new = one(&media_clip(""), with_item);
        let before = new.clip().picture_v2()?;
        let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
        if !drawn.iter().any(|(t, _)| t == "None chosen") {
            return Err("an item not chosen isn't said".into());
        }
        holds(&new, 0, &[])?;
        if new.clip().picture_v2()? != before {
            return Err("opening it wrote the picture".into());
        }
        Ok(())
    });
    // An item the pool hasn't, and one whose file isn't here: named, said,
    // nothing written.
    report.case("media source item missing", key(Interaction::MissingAsset), || {
        let mut new = one(&media_clip(&klipp_core::MediaId::new().0.to_string()), with_item);
        let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
        if !drawn.iter().any(|(t, _)| t == "\u{26a0} Media not in the project") {
            return Err("an item gone from the pool isn't said".into());
        }
        holds(&new, 0, &[])?;
        let mut new = one(&media_clip(&item.0.to_string()), with_item);
        let drawn = new.frame(Vec::new(), egui::Modifiers::NONE);
        if !drawn.iter().any(|(t, _)| t == "plate.mov") || !drawn.iter().any(|(t, _)| t == "\u{26a0} Media file not found") {
            return Err("an item whose file is gone isn't named and said".into());
        }
        holds(&new, 0, &[])
    });
    report.case("media source item picked", [key(Interaction::FileSelect), key(Interaction::UndoRedo)].concat(), || {
        let mut new = one(&media_clip(""), with_item);
        let want = item.0.to_string();
        let before = new.clip().picture_v2()?;
        for text in ["None chosen", "plate.mov"] {
            if !new.click_last(text) {
                return Err(format!("{text:?} isn't drawn"));
            }
        }
        if media_of(&new).as_deref() != Some(want.as_str()) {
            return Err(format!("stored {:?}, want {want:?}", media_of(&new)));
        }
        holds(&new, 1, &[])?;
        let after = new.clip().picture_v2()?;
        if !new.step(false) || new.clip().picture_v2()? != before {
            return Err("undo doesn't give back the picture before".into());
        }
        if !new.step(true) || new.clip().picture_v2()? != after {
            return Err("redo doesn't give back the picture after".into());
        }
        Ok(())
    });
}

/// The media source's item editor's required keys, but Pixels: its frame
/// is another media item's, which the harness's source hasn't; it is drawn
/// against its item in klipp-compose's `media_source_render`.
#[test]
fn a_media_sources_item_edits_as_expected() {
    let mut report = Report::default();
    report.family("media source item", run_media_item);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require_media_item(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels) && !report.passed.contains(k)).collect();
    assert!(!manifest.required.is_empty() && missing.is_empty(), "required and not earned: {missing:#?}");
}

/// The 3D LUT file editor's required keys, but Pixels (the picture
/// oracle's).
#[test]
fn the_lut_effects_file_edits_as_expected() {
    let mut report = Report::default();
    report.family("3D LUT file", run_lut_file);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require_lut_file(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels) && !report.passed.contains(k)).collect();
    assert!(!manifest.required.is_empty() && missing.is_empty(), "required and not earned: {missing:#?}");
}

/// The frei0r editor's required keys, but Pixels (the picture oracle's).
#[test]
fn frei0r_plug_ins_edit_as_expected() {
    let mut report = Report::default();
    report.family("frei0r", run_frei0r);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require_frei0r(&mut manifest);
    let wanted: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels)).collect();
    let missing: Vec<_> = wanted.iter().filter(|k| !report.passed.contains(k)).collect();
    assert!(!wanted.is_empty() && missing.is_empty(), "required and not earned: {missing:#?}");
}

#[test]
fn analysis_workflows_edit_as_expected() {
    let mut report = Report::default();
    report.family("analysis workflows", run);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require_analyses(&mut manifest);
    let wanted: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels)).collect();
    let missing: Vec<_> = wanted.iter().filter(|k| !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}

/// The whole family, as the orchestrator runs it: every key it requires
/// (but Pixels) earned.
#[test]
fn every_specialist_key_is_earned() {
    let mut report = Report::default();
    run(&mut report);
    report.family_result().unwrap();
    let mut manifest = Manifest::default();
    require(&mut manifest);
    let missing: Vec<_> = manifest.required.iter().filter(|k| !matches!(k.interaction, Interaction::Pixels) && !report.passed.contains(k)).collect();
    assert!(missing.is_empty(), "required and not earned: {missing:#?}");
}
