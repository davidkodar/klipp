//! Observations of the existing inspector, before the generated view exists.
use klipp_core::{ClipId, RationalTime};
use klipp_nodes::{Node, NodeGraph, NodeId, NodeParams};
use klipp_timeline::Clip;
use klipp_ui::widgets::row_recorder::{self, Row};
use klipp_ui::{panels::node_inspector::NodeInspectorPanel, state::AppState, theme::KlippTheme};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Case {
    kind: String,
    variant: String,
    stored: serde_json::Value,
    rows: Vec<Row>,
}

fn frame(ctx: &egui::Context, draw: impl FnOnce(&mut egui::Ui)) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(520.0, 12000.0),
        )),
        time: Some(0.0),
        ..Default::default()
    };
    let mut draw = Some(draw);
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| draw.take().unwrap()(ui));
    });
}

fn effect_rows(effect: klipp_effects::BuiltinEffect) -> Vec<Row> {
    effect_rows_with_keys(effect, false)
}

fn effect_rows_with_keys(effect: klipp_effects::BuiltinEffect, keyed: bool) -> Vec<Row> {
    let state = AppState::default();
    let mut clip = Clip::gap(
        RationalTime::from_secs_f64(0.0, 24),
        RationalTime::from_secs_f64(2.0, 24),
    );
    clip.id = ClipId::nil();
    let id = clip.push_effect(effect);
    if keyed {
        clip.edit_effect_keyframes_of(id, "radius", |keys| {
            for time_secs in [-1.0, 0.0, 1.0] {
                keys.push(klipp_core::Keyframe {
                    time_secs,
                    value: 2.0,
                    interpolation: klipp_core::Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                });
            }
        })
        .unwrap();
    }
    let before = clip.picture_v2().expect("fixture picture").picture.clone();
    let ctx = egui::Context::default();
    let draw = || {
        let mut commands = Vec::new();
        let (_, rows) = row_recorder::capture(|| {
            frame(&ctx, |ui| {
                klipp_ui::inspector::effects_tab::show(
                    ui,
                    &KlippTheme::default(),
                    &state,
                    &clip,
                    &mut commands,
                    &mut false,
                    &mut false,
                    0.0,
                );
            })
        });
        assert!(
            commands.is_empty(),
            "opening the effect inspector emitted commands"
        );
        assert_eq!(
            clip.picture_v2().expect("fixture picture").picture,
            before,
            "opening changed the picture"
        );
        rows
    };
    let first = draw();
    assert_eq!(first, draw(), "effect rows changed on an idle second frame");
    first
}

fn grade_rows(kind: klipp_nodes::NodeKind, params: NodeParams) -> Vec<Row> {
    grade_rows_on_tab(kind, params, None)
}

fn grade_rows_on_tab(
    kind: klipp_nodes::NodeKind,
    params: NodeParams,
    tab: Option<klipp_ui::state::ColorTab>,
) -> Vec<Row> {
    grade_rows_with_channel(
        kind,
        params,
        tab,
        klipp_ui::widgets::curves_editor::CurveChannel::Master,
    )
}

fn grade_rows_with_channel(
    kind: klipp_nodes::NodeKind,
    params: NodeParams,
    tab: Option<klipp_ui::state::ColorTab>,
    channel: klipp_ui::widgets::curves_editor::CurveChannel,
) -> Vec<Row> {
    let node = NodeId::from_u128(42);
    let mut graph = NodeGraph {
        nodes: vec![Node {
            id: node,
            kind,
            label: String::new(),
            enabled: true,
            params,
            keyframes: Default::default(),
            output_link: None,
            input_links: vec![],
        }],
        output_node_id: node,
        ..Default::default()
    };
    let mut panel = NodeInspectorPanel::default();
    panel.active_curve_channel = channel;
    panel.wheels_presets.clear();
    panel.curves_presets.clear();
    panel.huesat_presets.clear();
    panel.warper_presets.clear();
    panel.color_presets.clear();
    let ctx = egui::Context::default();
    let before = serde_json::to_value(&graph).unwrap();
    let mut draw = || {
        let (_, rows) = row_recorder::capture(|| {
            frame(&ctx, |ui| {
                let commands = if let Some(tab) = tab {
                    panel.record_old_tab(
                        ui,
                        &mut graph,
                        node,
                        ClipId::nil(),
                        &KlippTheme::default(),
                        tab,
                    )
                } else {
                    panel.record_old_inspector(
                        ui,
                        &mut graph,
                        node,
                        ClipId::nil(),
                        &KlippTheme::default(),
                    )
                };
                assert!(commands.is_empty(), "{kind:?}: opening emitted commands");
            })
        });
        assert_eq!(
            serde_json::to_value(&graph).unwrap(),
            before,
            "{kind:?}: opening changed the graph"
        );
        rows
    };
    let first = draw();
    assert_eq!(first, draw(), "{kind:?}: idle rows changed");
    first
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for ty in klipp_effects::registry::effect_types() {
        out.push(Case {
            kind: ty.desc.id.into(),
            variant: "sample".into(),
            stored: serde_json::to_value((ty.sample)()).unwrap(),
            rows: effect_rows((ty.sample)()),
        });
        let mut conditional = (ty.sample)();
        if let klipp_effects::BuiltinEffect::SpeedRamp {
            freeze, reverse, ..
        } = &mut conditional
        {
            *freeze = true;
            *reverse = true;
            out.push(Case {
                kind: ty.desc.id.into(),
                variant: "freeze_reverse".into(),
                stored: serde_json::to_value(&conditional).unwrap(),
                rows: effect_rows(conditional),
            });
        }
        for method in [
            klipp_effects::SpillMethod::None,
            klipp_effects::SpillMethod::Hawk,
        ] {
            let mut conditional = (ty.sample)();
            if let klipp_effects::BuiltinEffect::ChromaKey { spill_method, .. } = &mut conditional {
                *spill_method = method;
                out.push(Case {
                    kind: ty.desc.id.into(),
                    variant: format!("spill/{method:?}"),
                    stored: serde_json::to_value(&conditional).unwrap(),
                    rows: effect_rows(conditional),
                });
            }
        }
    }
    for ty in klipp_nodes::registry::node_types() {
        let params = initialized_params(ty.kind);
        out.push(Case {
            kind: ty.desc.id.into(),
            variant: "initialized_default".into(),
            stored: serde_json::to_value(&params).unwrap(),
            rows: grade_rows(ty.kind, params),
        });
        let mut conditional = initialized_params(ty.kind);
        let variant = match ty.kind {
            klipp_nodes::NodeKind::Qualifier => {
                conditional
                    .qualifier
                    .get_or_insert_with(Default::default)
                    .enabled = true;
                Some("enabled")
            }
            klipp_nodes::NodeKind::LumaKey => {
                conditional
                    .luma_key
                    .get_or_insert_with(Default::default)
                    .enabled = true;
                Some("enabled")
            }
            klipp_nodes::NodeKind::SoftClip => {
                conditional
                    .soft_clip
                    .get_or_insert_with(Default::default)
                    .enabled = true;
                Some("enabled")
            }
            klipp_nodes::NodeKind::FilmGrain => {
                conditional
                    .grain
                    .get_or_insert_with(Default::default)
                    .enabled = true;
                Some("enabled")
            }
            klipp_nodes::NodeKind::ColorWheels => {
                conditional.wheels_log_mode = Some(true);
                Some("log")
            }
            klipp_nodes::NodeKind::DifferenceKey => {
                conditional.difference_key.as_mut().unwrap().enabled = true;
                Some("enabled")
            }
            klipp_nodes::NodeKind::LayerMixer => {
                conditional
                    .layer_mixer
                    .as_mut()
                    .unwrap()
                    .inputs
                    .push(Default::default());
                Some("one_input")
            }
            _ => None,
        };
        if let Some(variant) = variant {
            out.push(Case {
                kind: ty.desc.id.into(),
                variant: variant.into(),
                stored: serde_json::to_value(&conditional).unwrap(),
                rows: grade_rows(ty.kind, conditional),
            });
        }
    }
    use klipp_ui::widgets::curves_editor::CurveChannel;
    for channel in [CurveChannel::Red, CurveChannel::Green, CurveChannel::Blue] {
        let params = initialized_params(klipp_nodes::NodeKind::RGBCurves);
        out.push(Case {
            kind: "klipp.grade.rgb_curves".into(),
            variant: format!("channel/{channel:?}"),
            stored: serde_json::to_value(&params).unwrap(),
            rows: grade_rows_with_channel(klipp_nodes::NodeKind::RGBCurves, params, None, channel),
        });
    }
    use klipp_ui::state::ColorTab;
    for tab in [
        ColorTab::Adjustments,
        ColorTab::Wheels,
        ColorTab::Curves,
        ColorTab::HueSat,
        ColorTab::Warper,
    ] {
        let params = NodeParams {
            curves: Some(Default::default()),
            huesat: Some(Default::default()),
            masks: Some(Default::default()),
            warper_params: Some(Default::default()),
            ..Default::default()
        };
        out.push(Case {
            kind: "klipp.grade.corrector".into(),
            variant: format!("tab/{tab:?}"),
            stored: serde_json::to_value(&params).unwrap(),
            rows: grade_rows_on_tab(klipp_nodes::NodeKind::Corrector, params, Some(tab)),
        });
    }
    let params = NodeParams {
        wheels_log_mode: Some(true),
        ..Default::default()
    };
    out.push(Case {
        kind: "klipp.grade.corrector".into(),
        variant: "tab/Wheels/log".into(),
        stored: serde_json::to_value(&params).unwrap(),
        rows: grade_rows_on_tab(
            klipp_nodes::NodeKind::Corrector,
            params,
            Some(ColorTab::Wheels),
        ),
    });
    out
}

// Materialized optional custom-editor settings are explicit fixture data.
// The sparse/default-None case is a different opening-behaviour check.
fn initialized_params(kind: klipp_nodes::NodeKind) -> NodeParams {
    use klipp_nodes::NodeKind::*;
    let mut p = NodeParams::default();
    match kind {
        RGBCurves => p.curves = Some(Default::default()),
        HueSatCurves => p.huesat = Some(Default::default()),
        Mask => p.masks = Some(Default::default()),
        DifferenceKey => p.difference_key = Some(Default::default()),
        LayerMixer => p.layer_mixer = Some(Default::default()),
        ColorWarper => p.warper_params = Some(Default::default()),
        _ => {}
    }
    p
}

#[test]
fn old_rows_are_deterministic_and_opening_emits_nothing() {
    let first = cases();
    assert_eq!(first, cases());
    assert!(!first.is_empty());
}

#[test]
fn scrubby_rows_capture_the_actual_diamond_state() {
    let effect = klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 };
    let plain = effect_rows(effect.clone());
    let keyed = effect_rows_with_keys(effect, true);
    let row = |rows: Vec<Row>| {
        rows.into_iter()
            .find(|r| r.parameter.as_deref() == Some("radius"))
            .expect("radius row")
    };
    let (plain, keyed) = (row(plain), row(keyed));
    assert!(!plain.keyframe_state.unwrap().any);
    let state = keyed.keyframe_state.unwrap();
    assert!(state.at_playhead && state.any);
    assert_eq!(state.in_frame, 1);
    assert_eq!((state.previous, state.next), (Some(-1.0), Some(1.0)));
}

#[test]
fn old_effect_rows_cover_the_registry() {
    let cases = cases();
    let mut missing = Vec::new();
    for ty in klipp_effects::registry::effect_types() {
        for param in ty.desc.params {
            let recorded = cases
                .iter()
                .filter(|c| c.kind == ty.desc.id)
                .flat_map(|c| &c.rows)
                .any(|r| r.parameter.as_deref() == Some(param.key));
            // Explicit old-UI omissions, not "fixed means hidden".
            let classified = matches!(
                (ty.tag, param.key),
                ("film_grain" | "old_film" | "vhs", "seed") |
                ("deflicker", "gain") | // populated by analysis
                ("color_fade", "progress") | // supplied by the compositor
                ("transform", "rotation_x" | "rotation_y" | "perspective") | // Video-tab placement controls
                ("transform", "focal_ref") | // derived reference, no old row
                ("focus_blur", "radius" | "vertical" | "band") | // Title roll editor
                ("frei0r", "type_id" | "params") // opaque plugin editor; sample has no installed plugin
            );
            if !recorded && !classified {
                missing.push(format!("{}:{}", ty.tag, param.key));
            }
        }
    }
    assert!(missing.is_empty(), "unrecorded parameters need an actual hook or an explicit old-UI classification: {missing:?}");
}

#[test]
fn binary_sliders_record_their_discrete_step() {
    let cases = cases();
    for (kind, parameter) in [
        ("klipp.fx.noise_generator", "monochrome"),
        ("klipp.fx.letterbox", "bar_color"),
    ] {
        let row = cases
            .iter()
            .find(|c| c.kind == kind)
            .unwrap()
            .rows
            .iter()
            .find(|r| r.parameter.as_deref() == Some(parameter))
            .unwrap();
        assert_eq!(row.widget, "slider");
        assert_eq!(row.range, Some([0.0, 1.0]));
        assert_eq!(row.step, Some(1.0));
    }
}

#[test]
fn old_grade_rows_cover_the_registry() {
    let cases = cases();
    let mut missing = Vec::new();
    for ty in klipp_nodes::registry::node_types() {
        for param in ty.desc.params {
            let recorded = cases
                .iter()
                .filter(|c| c.kind == ty.desc.id)
                .flat_map(|c| &c.rows)
                .any(|r| {
                    let Some(key) = r.parameter.as_deref() else {
                        return false;
                    };
                    key == param.key
                        || key == param.field
                        || match r.widget.as_str() {
                            "colour_wheel" => ["x", "y", "master"]
                                .iter()
                                .any(|suffix| param.key == format!("{key}_{suffix}")),
                            "colour_warper" => param.field.starts_with("warper_params."),
                            "rgb_curve" => param.key == "curves",
                            "huesat_curve" => param.key == "huesat",
                            _ => false,
                        }
                });
            let classified = matches!(
                (ty.kind, param.key),
                (klipp_nodes::NodeKind::Compound, "compound_subgraph") | // edited on the canvas
                (klipp_nodes::NodeKind::PrimariesMatrix, "primaries_from" | "primaries_to") | // read-only inspector labels
                (klipp_nodes::NodeKind::ColorMatch, "color_match.exposure" | "color_match.temperature" | "color_match.tint" | "color_match.saturation") // analysis results, not sliders
            );
            if !recorded && !classified {
                missing.push(format!("{:?}:{}", ty.kind, param.key));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "grade controls need a hook or an explicit custom/hidden classification: {missing:?}"
    );
}

#[test]
fn old_rows_match_base_m3() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/old_rows.json");
    let baseline: serde_json::Value = serde_json::from_slice(
        &std::fs::read(path).expect("record and review the BASE-M3 row fixture explicitly"),
    )
    .unwrap();
    assert_eq!(baseline["cases"], serde_json::to_value(cases()).unwrap());
}

/// Explicit recording command; normal tests only read the reviewed fixture.
#[test]
#[ignore = "explicit fixture recording, followed by review"]
fn record_base_m3_rows() {
    let fixture = serde_json::json!({"format": 2, "base_m3": "1e11f285fd8d3e90e4da799c80d7081bd2bf9e77", "cases": cases()});
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/old_rows.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(
        !path.exists(),
        "remove a baseline only after its replacement is explicitly approved"
    );
    std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
}
