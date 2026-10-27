//! The generated parameter rows (M3 step 5) against the old inspector's
//! recorded rows (M3 step 1b's fixture), and the row shell's rules.

use klipp_core::AppCommand;
use klipp_effects::registry::presentation::Widget;
use klipp_effects::registry::{effect_types, EffectType, ParamDesc};
use klipp_graph_format::path::{NodePath, Root};
use klipp_ui::inspector::generated::{self, effect_value, KeyState, NodeRows, RowCtx};
use klipp_ui::widgets::row_recorder::{self, Row};
use serde_json::Value;

fn frame(ctx: &egui::Context, draw: impl FnOnce(&mut egui::Ui)) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 12000.0))), time: Some(0.0), ..Default::default() };
    let mut draw = Some(draw);
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| draw.take().unwrap()(ui));
    });
}

fn addr() -> NodePath {
    NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [uuid::Uuid::from_u128(1)])
}

/// Draw `t`'s rows for the effect saved as `json`, recorded, and the
/// commands drawing pushed.
fn generated(t: &EffectType, json: &Value) -> (Vec<Row>, Vec<AppCommand>) {
    let ctx = egui::Context::default();
    let path = addr();
    let mut cmds = Vec::new();
    let theme = klipp_ui::theme::KlippTheme::default();
    let presentation = |p: &ParamDesc| t.presentation(p);
    let stored = |p: &ParamDesc| effect_value(json, p);
    let keys = |_: &ParamDesc| (None, KeyState::default());
    let rows = NodeRows { params: t.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    let (_, recorded) = row_recorder::capture(|| {
        frame(&ctx, |ui| {
            let mut rc = RowCtx {
                addr: &path,
                clip_time_secs: 0.0,
                timeline_in_secs: 0.0,
                rate: 24,
                encoding: klipp_grade_model::Encoding::Bt1886,
                entry: Some(generated::Entry { payload: json, fields_at: &[], choices_by_index: false }),
                cmds: &mut cmds,
                behaviours: None,
                theme: &theme,
                app: None,
                custom: None,
            };
            generated::node_rows(ui, &mut rc, &rows);
        })
    });
    (recorded, cmds)
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/old_rows.json")).unwrap()
}

/// Differences from today's rows that are meant, by (type, parameter,
/// what).
const MEANT: &[(&str, &str, &str)] = &[
    // IC2: Sharpen and Vibrance reset to their catalog 0, not 0.5.
    ("klipp.fx.sharpen", "amount", "reset"),
    ("klipp.fx.vibrance", "amount", "reset"),
    // IC5: Letterbox's bar colour is a Black/White choice, Noise's
    // monochrome a switch, not 0-to-1 sliders.
    ("klipp.fx.letterbox", "bar_color", "row"),
    ("klipp.fx.noise_generator", "monochrome", "row"),
    // IC12: a Transform card shows its own tilt and perspective rows.
    ("klipp.fx.transform", "rotation_x", "row"),
    ("klipp.fx.transform", "rotation_y", "row"),
    ("klipp.fx.transform", "perspective", "row"),
    // Colour Fade's duration is a scrub row like every other number, not
    // a bare drag value, and gains a reset.
    ("klipp.fx.color_fade", "duration_secs", "widget"),
    ("klipp.fx.color_fade", "duration_secs", "reset"),
    // Frei0r's plug-in editor comes in M3 step 6; until then its row is a
    // read-only placeholder (today it draws nothing without a plug-in).
    ("klipp.fx.frei0r", "type_id", "row"),
    // #2763: Glow's threshold, where its light starts, is a row of its own.
    ("klipp.fx.glow", "threshold", "row"),
    // #2831: Old Film's age sets its look, and each of its marks has a row
    // of its own.
    ("klipp.fx.old_film", "age", "row"),
    ("klipp.fx.old_film", "amount", "row"),
    ("klipp.fx.old_film", "stains", "row"),
    ("klipp.fx.old_film", "frame_lines", "row"),
    ("klipp.fx.old_film", "leaks", "row"),
    ("klipp.fx.old_film", "burn", "row"),
    ("klipp.fx.old_film", "mono", "row"),
    // #2831: its dust is sparse debris, named so.
    ("klipp.fx.old_film", "dust", "label"),
];

fn meant(t: &str, p: &str, what: &str) -> bool {
    MEANT.contains(&(t, p, what))
}

/// What a row is, as compared: everything the old inspector shows, not
/// where egui put it.
fn facts(r: &Row) -> Vec<(&'static str, String)> {
    let num = |v: Option<f64>| v.map(|x| (x as f32).to_string()).unwrap_or_default();
    vec![
        ("label", r.label.clone()),
        ("widget", r.widget.clone()),
        ("value", r.value.to_string()),
        ("range", r.range.map(|[a, b]| format!("{}..{}", a as f32, b as f32)).unwrap_or_default()),
        ("reset", r.reset.as_ref().and_then(Value::as_f64).map(|x| (x as f32).to_string()).unwrap_or_default()),
        ("speed", num(r.speed)),
        ("decimals", r.decimals.map(|d| d.to_string()).unwrap_or_default()),
        ("suffix", r.suffix.trim().to_owned()),
        ("diamond", r.keyframe_state.is_some().to_string()),
    ]
}

/// Every built-in effect, in every state the old inspector was recorded
/// in, draws today's rows: the same parameters in the same order, each
/// with today's label, widget, value, range, reset, drag step, decimals,
/// suffix and diamond, but for the meant differences.
#[test]
fn generated_rows_match_the_old_rows() {
    let fixture = fixture();
    let mut differ = Vec::new();
    let mut compared = 0;
    for case in fixture["cases"].as_array().unwrap().iter().filter(|c| c["kind"].as_str().unwrap().starts_with("klipp.fx.")) {
        let kind = case["kind"].as_str().unwrap();
        let t = effect_types().find(|t| t.desc.id == kind).unwrap();
        let today: Vec<Row> = serde_json::from_value(case["rows"].clone()).unwrap();
        let (ours, cmds) = generated(t, &case["stored"]);
        assert!(cmds.is_empty(), "{kind}: drawing pushed {cmds:?}");
        let key = |r: &Row| r.parameter.clone().unwrap_or_default();
        let today_keys: Vec<String> = today.iter().map(key).filter(|k| !meant(kind, k, "row")).collect();
        let our_keys: Vec<String> = ours.iter().map(key).filter(|k| !meant(kind, k, "row")).collect();
        if today_keys != our_keys {
            differ.push(format!("{kind} [{}]: rows {our_keys:?}, today {today_keys:?}", case["variant"]));
            continue;
        }
        for (o, w) in ours.iter().filter(|r| !meant(kind, &key(r), "row")).zip(today.iter().filter(|r| !meant(kind, &key(r), "row"))) {
            let p = key(o);
            for ((what, a), (_, b)) in facts(o).into_iter().zip(facts(w)) {
                // A bare drag value and a scrub row show the same facts.
                let equal = a == b || (what == "widget" && meant(kind, &p, "widget"));
                if !equal && !meant(kind, &p, what) {
                    differ.push(format!("{kind}/{p} [{}] {what}: {a}, today {b}", case["variant"]));
                }
            }
            compared += 1;
        }
    }
    assert!(differ.is_empty(), "{} differences:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 150, "{compared} rows compared");
}

/// Drawing any effect's rows, frame after frame, pushes nothing and
/// records the same rows (DEC-§1).
#[test]
fn opening_a_panel_emits_nothing() {
    for t in effect_types() {
        let json = serde_json::to_value((t.sample)()).unwrap();
        let (first, cmds) = generated(t, &json);
        assert!(cmds.is_empty(), "{}: {cmds:?}", t.desc.id);
        for _ in 0..9 {
            let (again, cmds) = generated(t, &json);
            assert!(cmds.is_empty(), "{}: {cmds:?}", t.desc.id);
            assert_eq!(again.iter().map(facts).collect::<Vec<_>>(), first.iter().map(facts).collect::<Vec<_>>(), "{}", t.desc.id);
        }
    }
}

/// A fixed parameter (not keyframable: a Transform row, a seed) shows no
/// diamond; a keyframable one does.
#[test]
fn fixed_params_show_no_diamond() {
    for t in effect_types() {
        let json = serde_json::to_value((t.sample)()).unwrap();
        let (rows, _) = generated(t, &json);
        for r in rows.iter().filter(|r| r.widget == "scrubby") {
            let p = t.desc.params.iter().find(|p| Some(p.key) == r.parameter.as_deref()).unwrap();
            assert_eq!(r.keyframe_state.is_some(), p.keyframable && !p.derived, "{}/{}", t.desc.id, p.key);
        }
    }
}

/// Every number resets to its declared value: the presentation's hint,
/// of which no effect has one since Speed Ramp's freeze point went
/// (#1409), else the catalog's default.
#[test]
fn every_row_resets_to_its_declared_value() {
    let mut hints = Vec::new();
    for t in effect_types() {
        for p in t.desc.params {
            if let Some(r) = t.presentation(p).reset {
                hints.push((t.desc.id, p.key, r));
            }
        }
        let json = serde_json::to_value((t.sample)()).unwrap();
        let (rows, _) = generated(t, &json);
        for r in rows.iter().filter(|r| r.widget == "scrubby") {
            let p = t.desc.params.iter().find(|p| Some(p.key) == r.parameter.as_deref()).unwrap();
            let want = t.presentation(p).reset.unwrap_or(p.default);
            assert_eq!(r.reset.as_ref().and_then(Value::as_f64).map(|x| x as f32), Some(want), "{}/{}", t.desc.id, p.key);
        }
    }
    assert!(hints.is_empty(), "{hints:?}");
}

/// A value the row can't read as its kind is shown read-only and nothing
/// is pushed to change it.
#[test]
fn unknown_value_kinds_draw_read_only_and_keep_bytes() {
    let t = effect_types().find(|t| t.desc.id == "klipp.fx.gaussian_blur").unwrap();
    let json = serde_json::json!({ "type": "gaussian_blur", "radius": { "unexpected": [1, 2] } });
    let (rows, cmds) = generated(t, &json);
    assert!(cmds.is_empty());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].widget, "read_only");
    assert_eq!(rows[0].value, json["radius"]);
}

/// Hidden rows aren't drawn, and a sibling's row never touches them: a
/// grain's seed is in no row.
#[test]
fn hidden_values_are_not_drawn() {
    for t in effect_types() {
        let json = serde_json::to_value((t.sample)()).unwrap();
        let (rows, _) = generated(t, &json);
        for p in t.desc.params.iter().filter(|p| t.presentation(p).widget == Widget::Hidden) {
            assert!(rows.iter().all(|r| r.parameter.as_deref() != Some(p.key)), "{}/{}", t.desc.id, p.key);
        }
    }
}
