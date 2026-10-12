use klipp_core::title_motion::{Easing, Motion, MotionKind, StaggerUnit, TitleMotion};
use klipp_document::{
    apply::DocumentView,
    ops::{self, Op},
};
use klipp_media::MediaPool;
use klipp_timeline::{Brief, Sequence, TitleObjectContent, TypeStyle};
use serde_json::{json, Value};

fn motion(seq: &Sequence) -> TitleMotion {
    let title = seq
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find_map(|c| c.title_data())
        .unwrap();
    assert!(matches!(title.objects[0].content, TitleObjectContent::TextBox(_)), "text");
    title.objects[0].fx().motion
}

fn apply(mut seq: Sequence, fields: Value) -> Sequence {
    seq.tracks[0].clips.push(klipp_timeline::Clip::gap(
        klipp_core::Timecode::from_secs_f64(0.0, 24),
        klipp_core::Timecode::from_secs_f64(4.0, 24),
    ));
    let mut value =
        json!({"op":"title", "text":"Words move independently", "at":{"secs":0}, "secs":4});
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let op: Op = serde_json::from_value(value).unwrap();
    let mut pool = MediaPool::new();
    let plan = ops::resolve(&seq, &pool, &[op]).unwrap();
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView {
        sequences: &mut sequences,
        active: 0,
        media_pool: &mut pool,
        fps: &mut fps,
    };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    let saved = serde_json::to_string(&doc.sequences[0]).unwrap();
    for step in plan.steps.iter().rev() {
        doc.apply_inverse(&step.edit);
    }
    assert!(!doc.sequences[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .any(|c| c.title_data().is_some()));
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    assert_eq!(
        motion(&doc.sequences[0]),
        motion(&serde_json::from_str(&saved).unwrap())
    );
    serde_json::from_str(&saved).unwrap()
}

#[test]
fn masked_word_entrance_and_independent_exit_survive_apply_undo_and_serde() {
    let seq = apply(
        Sequence::new_default(),
        json!({
            "enter":{"kind":"mask_rise","duration_secs":0.7,"delay_secs":0.2,"easing":"out_quint","stagger":"word","stagger_secs":0.13},
            "exit":{"kind":"mask_drop","duration_secs":0.4,"delay_secs":0.1,"easing":"in_cubic","stagger":"word","stagger_secs":0.07}
        }),
    );
    let m = motion(&seq);
    assert_eq!(
        m.enter,
        Some(
            Motion::new(MotionKind::MaskRise)
                .duration(0.7)
                .delay(0.2)
                .easing(Easing::OutQuint)
                .stagger(StaggerUnit::Word, 0.13)
        )
    );
    assert_eq!(
        m.exit,
        Some(
            Motion::new(MotionKind::MaskDrop)
                .duration(0.4)
                .delay(0.1)
                .easing(Easing::InCubic)
                .stagger(StaggerUnit::Word, 0.07)
        )
    );
    assert_eq!(m.progress(0.1, Some(4.0), 0), (0.0, 1.0));
    assert_eq!(m.progress(2.0, Some(4.0), 2), (1.0, 1.0));
    assert_eq!(m.progress(3.95, Some(4.0), 0), (1.0, 0.0));
}

#[test]
fn legacy_defaults_and_builtin_styles_keep_their_motion() {
    assert_eq!(
        motion(&apply(Sequence::new_default(), json!({}))),
        TitleMotion::default()
    );
    assert_eq!(
        motion(&apply(
            Sequence::new_default(),
            json!({"animate":"typewriter"})
        ))
        .enter,
        Some(Motion::new(MotionKind::Typewriter).stagger(StaggerUnit::Char, 0.04))
    );
    assert_eq!(
        motion(&apply(Sequence::new_default(), json!({"style":"heading"}))).enter,
        Some(Motion::new(MotionKind::Rise).duration(0.4))
    );
    let seq = apply(
        Sequence::new_default(),
        json!({"style":"heading","enter":{"kind":"mask_drop"},"exit":{"kind":"fade"}}),
    );
    assert_eq!(motion(&seq).enter, Some(Motion::new(MotionKind::MaskDrop)));
}

#[test]
fn style_sides_inherit_replace_or_disable_independently() {
    let style: TypeStyle = serde_json::from_value(json!({"name":"kinetic","enter":{"kind":"mask_rise","delay_secs":0.5},"exit":{"kind":"fade","duration_secs":0.3}})).unwrap();
    let seq = Sequence {
        brief: Some(Brief {
            type_styles: vec![style.clone()],
            ..Default::default()
        }),
        ..Sequence::new_default()
    };
    assert_eq!(
        motion(&apply(seq.clone(), json!({"style":"kinetic"}))),
        klipp_document::title_style::resolve_motion(&style).unwrap()
    );
    let m = motion(&apply(
        seq.clone(),
        json!({"style":"kinetic","enter":{"kind":"none"}}),
    ));
    assert!(m.enter.is_none());
    assert!(m.exit.is_some());
    let timed = motion(&apply(
        seq.clone(),
        json!({"style":"kinetic", "animate_secs":0.8, "stagger":"word"}),
    ));
    assert_eq!(timed.enter.unwrap().kind, MotionKind::MaskRise);
    assert_eq!(timed.enter.unwrap().duration_secs, 0.8);
    assert_eq!(timed.enter.unwrap().delay_secs, 0.5);
    assert_eq!(timed.enter.unwrap().stagger, StaggerUnit::Word);
    let m = motion(&apply(
        seq,
        json!({"style":"kinetic","animate":"rise","exit":{"kind":"none"}}),
    ));
    assert_eq!(m.enter, Some(Motion::new(MotionKind::Rise)));
    assert!(m.exit.is_none());
}

#[test]
fn invalid_motion_refuses_before_editing() {
    for extra in [
        json!({"enter":{"kind":"mask_rise"},"animate":"rise"}),
        json!({"enter":{"kind":"mask_rise"},"stagger":"word"}),
        json!({"exit":{"kind":"unknown"}}),
        json!({"exit":{"kind":"fade","easing":"unknown"}}),
        json!({"exit":{"kind":"fade","duration_secs":0}}),
        json!({"exit":{"kind":"fade","delay_secs":-1}}),
        json!({"enter":{"kind":"fade","distance":-1}}),
        json!({"enter":{"kind":"fade","stagger_secs":-1}}),
        json!({"enter":{"kind":"none","duration_secs":1}}),
    ] {
        let mut v = json!({"op":"title","text":"Test","at":{"secs":0},"secs":3});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let op: Op = serde_json::from_value(v).unwrap();
        assert!(
            ops::resolve(&Sequence::new_default(), &MediaPool::new(), &[op]).is_err(),
            "{extra}"
        );
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for field in ["duration_secs", "delay_secs", "stagger_secs", "distance"] {
            let mut spec = klipp_timeline::TitleMotionSpec {
                kind: "fade".into(),
                ..Default::default()
            };
            match field {
                "duration_secs" => spec.duration_secs = Some(value),
                "delay_secs" => spec.delay_secs = Some(value),
                "stagger_secs" => spec.stagger_secs = Some(value),
                _ => spec.distance = Some(value),
            }
            let style = TypeStyle {
                exit: Some(spec),
                ..Default::default()
            };
            assert!(
                klipp_document::title_style::resolve_motion(&style).is_err(),
                "{field}: {value}"
            );
        }
    }
    assert!(serde_json::from_value::<klipp_document::ops::Op>(
        json!({"op":"title","text":"Hi","at":{"secs":0},"enter":{"kind":"rise","delai_secs":1}})
    )
    .is_err());
}

#[test]
fn edit_plan_lowering_uses_the_same_style_precedence_and_validation() {
    use klipp_document::edit_plan;
    let seq = Sequence::new_default();
    let mut pool = MediaPool::new();
    let mut item =
        klipp_media::MediaItem::synthetic_for_test("/tmp/title-motion-fixture.mov".into(), None);
    item.duration = klipp_core::Timecode::from_secs_f64(4.0, 24);
    item.video_codec = "h264".into();
    let id = pool.add(item);
    let plan: klipp_timeline::EditPlan = serde_json::from_value(json!({"segments":[{
        "media_id":id,"source_in_secs":0,"source_out_secs":4,
        "title":{"text":"Masked words", "enter":{"kind":"mask_rise","stagger":"word","stagger_secs":0.12},"exit":{"kind":"mask_drop","delay_secs":0.2}}
    }]})).unwrap();
    edit_plan::validate(&plan, &pool, None).unwrap();
    let ops = edit_plan::to_ops(&plan, &seq, &pool, seq.brief.as_ref());
    let resolved = ops::resolve(&seq, &pool, &ops).unwrap();
    let mut sequences = vec![seq];
    let mut fps = sequences[0].fps;
    let mut doc = DocumentView {
        sequences: &mut sequences,
        active: 0,
        media_pool: &mut pool,
        fps: &mut fps,
    };
    for step in &resolved.steps {
        doc.apply_forward(&step.edit);
    }
    let m = motion(&doc.sequences[0]);
    assert_eq!(m.enter.unwrap().kind, MotionKind::MaskRise);
    assert_eq!(m.enter.unwrap().stagger_secs, 0.12);
    assert_eq!(m.exit.unwrap().delay_secs, 0.2);
    let mut invalid = plan;
    invalid.segments[0].title.as_mut().unwrap().animate_secs = Some(0.5);
    assert!(edit_plan::validate(&invalid, &pool, None)
        .unwrap_err()
        .contains("conflicts"));
}

#[test]
fn disabled_style_timing_and_overridden_invalid_style_build_like_they_validate() {
    use klipp_document::edit_plan;
    for (style_motion, title_motion) in [
        (
            json!({"enter":{"kind":"none"}}),
            json!({"animate_secs":0.5,"stagger":"word"}),
        ),
        (json!({}), json!({"animate_secs":0.5,"stagger":"word"})),
        (
            json!({"enter":{"kind":"rise","easing":"stale"},"exit":{"kind":"fade","easing":"stale"}}),
            json!({"enter":{"kind":"fade"},"exit":{"kind":"none"}}),
        ),
    ] {
        let mut style = json!({"name":"custom"});
        style
            .as_object_mut()
            .unwrap()
            .extend(style_motion.as_object().unwrap().clone());
        let seq = Sequence {
            brief: Some(Brief {
                type_styles: vec![serde_json::from_value(style).unwrap()],
                ..Default::default()
            }),
            ..Sequence::new_default()
        };
        let mut fields = json!({"style":"custom"});
        fields
            .as_object_mut()
            .unwrap()
            .extend(title_motion.as_object().unwrap().clone());
        let direct = motion(&apply(seq.clone(), fields.clone()));
        let mut pool = MediaPool::new();
        let mut item =
            klipp_media::MediaItem::synthetic_for_test("/tmp/motion-merge.mov".into(), None);
        item.duration = klipp_core::Timecode::from_secs_f64(4.0, 24);
        item.video_codec = "h264".into();
        let id = pool.add(item);
        fields["text"] = json!("Title");
        let plan: klipp_timeline::EditPlan = serde_json::from_value(json!({"segments":[{"media_id":id,"source_in_secs":0,"source_out_secs":4,"title":fields}]})).unwrap();
        edit_plan::validate(&plan, &pool, seq.brief.as_ref()).unwrap();
        let plan_ops = edit_plan::to_ops(&plan, &seq, &pool, seq.brief.as_ref());
        let resolved = ops::resolve(&seq, &pool, &plan_ops).unwrap();
        let mut sequences = vec![seq];
        let mut fps = sequences[0].fps;
        let mut doc = DocumentView {
            sequences: &mut sequences,
            active: 0,
            media_pool: &mut pool,
            fps: &mut fps,
        };
        for step in &resolved.steps {
            doc.apply_forward(&step.edit);
        }
        assert_eq!(motion(&doc.sequences[0]), direct);
    }
}

#[test]
fn merged_motion_disables_inherited_stagger_and_builds_consistently() {
    let seq = Sequence { brief: Some(Brief { type_styles: vec![serde_json::from_value(json!({"name":"custom","enter":{"kind":"rise","stagger":"word","stagger_secs":0.2}})).unwrap()], ..Default::default() }), ..Sequence::new_default() };
    let mut pool = MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/motion-merge.mov".into(), None);
    item.duration = klipp_core::Timecode::from_secs_f64(4.0, 24);
    item.video_codec = "h264".into();
    let id = pool.add(item);
    let plan: klipp_timeline::EditPlan = serde_json::from_value(json!({"segments":[{"media_id":id,"source_in_secs":0,"source_out_secs":4,"title":{"text":"Title","style":"custom","stagger":"none"}}]})).unwrap();
    klipp_document::edit_plan::validate(&plan, &pool, seq.brief.as_ref()).unwrap();
    ops::resolve(
        &seq,
        &pool,
        &klipp_document::edit_plan::to_ops(&plan, &seq, &pool, seq.brief.as_ref()),
    )
    .unwrap();
    let result = motion(&apply(seq, json!({"style":"custom","stagger":"none"})));
    assert_eq!(result.enter.unwrap().stagger, StaggerUnit::None);
}

#[test]
fn stagger_timing_requires_an_active_unit_and_approval_text_exposes_motion() {
    for stagger in [None, Some("none")] {
        let mut enter = json!({"kind":"rise","stagger_secs":0.2});
        if let Some(unit) = stagger {
            enter["stagger"] = json!(unit);
        }
        let style: TypeStyle = serde_json::from_value(json!({"name":"bad","enter":enter})).unwrap();
        assert!(klipp_document::title_style::resolve_motion(&style)
            .unwrap_err()
            .contains("stagger_secs needs"));
    }
    let seq = apply(
        Sequence::new_default(),
        json!({"enter":{"kind":"typewriter","stagger_secs":0.2}}),
    );
    assert_eq!(motion(&seq).enter.unwrap().stagger, StaggerUnit::Char);
    let nested = json!({"enter":{"kind":"mask_rise","stagger":"word","stagger_secs":0.12,"duration_secs":0.5,"easing":"out_quint"},"exit":{"kind":"mask_drop","delay_secs":2}});
    let mut title = nested.clone();
    title["text"] = json!("Title");
    let plan: klipp_timeline::EditPlan =
        serde_json::from_value(json!({"segments":[{"media_id":klipp_core::MediaId::new(),"source_in_secs":0,"source_out_secs":4,"title":title}]})).unwrap();
    let mut style = nested;
    style["name"] = json!("kinetic");
    let brief = Brief {
        type_styles: vec![serde_json::from_value(style).unwrap()],
        ..Default::default()
    };
    for text in [plan.text(None), brief.text()] {
        for expected in [
            "enter: mask_rise",
            "by word",
            "stagger 0.12 s",
            "duration 0.5 s",
            "easing out_quint",
            "exit: mask_drop",
            "exits 2 s early",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }
}

#[test]
fn invisible_timing_is_refused_but_partial_and_overlapping_motion_remain_possible() {
    for fields in [
        json!({"exit":{"kind":"fade","delay_secs":5}}),
        json!({"enter":{"kind":"rise","delay_secs":3}}),
        json!({"enter":{"kind":"fade","delay_secs":2},"exit":{"kind":"fade","delay_secs":1}}),
    ] {
        let mut op = json!({"op":"title","text":"Hi","at":{"secs":0},"secs":3});
        op.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(ops::resolve(
            &Sequence::new_default(),
            &MediaPool::new(),
            &[serde_json::from_value(op).unwrap()]
        )
        .is_err());
        let mut pool = MediaPool::new();
        let mut item =
            klipp_media::MediaItem::synthetic_for_test("/tmp/title-timing.mov".into(), None);
        item.duration = klipp_core::Timecode::from_secs_f64(10.0, 24);
        item.video_codec = "h264".into();
        let id = pool.add(item);
        let mut title = fields;
        title["text"] = json!("Hi");
        title["secs"] = json!(9);
        let plan:klipp_timeline::EditPlan=serde_json::from_value(json!({"segments":[{"media_id":id,"source_in_secs":0,"source_out_secs":3,"title":title}]})).unwrap();
        let err = klipp_document::edit_plan::validate(&plan, &pool, None).unwrap_err();
        assert!(err.contains("title lasts 3 s"), "{err}");
    }
    apply(
        Sequence::new_default(),
        json!({"enter":{"kind":"fade","duration_secs":5},"exit":{"kind":"fade","duration_secs":5}}),
    );
    for kind in [
        "fade",
        "mask_rise",
        "mask_drop",
        "blur",
        "scale_up",
        "typewriter",
    ] {
        let style: TypeStyle =
            serde_json::from_value(json!({"name":"bad","enter":{"kind":kind,"distance":0.2}}))
                .unwrap();
        assert!(klipp_document::title_style::resolve_motion(&style).is_err());
    }
}

#[test]
fn approval_discloses_partial_motion_and_ignored_legacy_overrides() {
    let plan:klipp_timeline::EditPlan=serde_json::from_value(json!({"segments":[{"media_id":klipp_core::MediaId::new(),"source_in_secs":0,"source_out_secs":2,"title":{"text":"Hi","enter":{"kind":"fade","duration_secs":3},"exit":{"kind":"fade","duration_secs":1}}}]})).unwrap();
    let text=plan.text(None);
    assert!(text.contains("entrance continues past"));assert!(text.contains("entrance and exit overlap"));
    let mut plan=plan;let title=plan.segments[0].title.as_mut().unwrap();title.enter=None;title.animate_secs=Some(0.5);
    assert!(plan.text(None).contains("otherwise ignored"));
}

#[test]
fn stored_legacy_timing_keeps_clamping_and_approval_includes_implicit_stagger() {
    for (secs, expected) in [(8.0,8.0),(7200.0,3600.0),(0.0,0.05),(-1.0,0.05),(0.01,0.05)] {
        let style: klipp_timeline::TypeStyle=serde_json::from_value(json!({"name":"stored","animate":"rise","animate_secs":secs})).unwrap();
        assert_eq!(klipp_document::title_style::resolve_motion(&style).unwrap().enter.unwrap().duration_secs,expected);
    }
    let spec:klipp_timeline::TitleMotionSpec=serde_json::from_value(json!({"kind":"TYPEWRITER","future_field":true})).unwrap();
    assert!((spec.span_secs("abcdefghijklmnopqrst")-1.36).abs()<1e-5);
    let spec:klipp_timeline::TitleMotionSpec=serde_json::from_value(json!({"kind":"rise","stagger":"Word"})).unwrap();
    assert!((spec.span_secs("one two three")-0.7).abs()<1e-5);
    let spaced:klipp_timeline::TitleMotionSpec=serde_json::from_value(json!({"kind":"typewriter","stagger_secs":0.2})).unwrap();
    assert!((spaced.span_secs("one two")-1.6).abs()<1e-5);
    assert!(serde_json::from_value::<klipp_document::ops::Op>(json!({"op":"title","text":"Hi","at":{"secs":0},"enter":{"kind":"rise","typo":1}})).is_err());
}

/// #736: an exit named grow grows by the given scale without fading, one
/// named shrink shrinks, an entrance named grow is scale_up as before,
/// and a scale that goes the wrong way, or on a kind that does not
/// scale, is refused.
#[test]
fn grow_and_shrink_exits_scale_by_an_amount_without_fading() {
    let spec = |kind: &str, scale: Option<f32>| klipp_timeline::TitleMotionSpec { kind: kind.into(), scale, ..Default::default() };
    let exit_of = |kind: &str, scale: Option<f32>| {
        let style = TypeStyle { exit: Some(spec(kind, scale)), ..Default::default() };
        klipp_document::title_style::resolve_motion(&style).map(|m| m.exit.unwrap())
    };
    let grow = exit_of("grow", Some(12.0)).unwrap();
    assert_eq!((grow.kind, grow.amount), (MotionKind::Larger, Some(12.0)));
    let (alpha, _, _, scale, _, _) = grow.pose(0.0, 100.0);
    assert_eq!((alpha, scale), (1.0, 12.0), "at the clip's end: fully grown, fully there");
    assert_eq!(exit_of("shrink", None).unwrap().kind, MotionKind::Smaller);
    let entrance = TypeStyle { enter: Some(spec("grow", None)), ..Default::default() };
    assert_eq!(klipp_document::title_style::resolve_motion(&entrance).unwrap().enter.unwrap().kind, MotionKind::ScaleUp);
    assert!(exit_of("grow", Some(0.5)).is_err(), "grow scales up");
    assert!(exit_of("shrink", Some(2.0)).is_err(), "shrink scales down");
    assert!(exit_of("fade", Some(2.0)).is_err(), "fade does not scale");
    assert_eq!(exit_of("scale_up", Some(0.0)).unwrap().amount, Some(0.0), "scale_up from nothing");
}

/// #787: a scaling motion takes an anchor, "solid" or a point of the
/// title's box; a motion that does not scale, a point outside the box
/// and an unknown name are refused.
#[test]
fn a_scaling_motion_takes_an_anchor() {
    use klipp_core::title_motion::MotionAnchor;
    let exit = |v: serde_json::Value| {
        let spec: klipp_timeline::TitleMotionSpec = serde_json::from_value(v).unwrap();
        let style = TypeStyle { exit: Some(spec), ..Default::default() };
        klipp_document::title_style::resolve_motion(&style).map(|m| m.exit.unwrap().anchor)
    };
    assert_eq!(exit(json!({"kind": "grow", "scale": 30, "anchor": "solid"})).unwrap(), Some(MotionAnchor::Solid));
    assert_eq!(exit(json!({"kind": "grow", "anchor": {"x": 0.3, "y": 0.25}})).unwrap(), Some(MotionAnchor::Point { x: 0.3, y: 0.25 }));
    assert_eq!(exit(json!({"kind": "shrink", "anchor": "centre"})).unwrap(), Some(MotionAnchor::Point { x: 0.5, y: 0.5 }));
    assert!(exit(json!({"kind": "fade", "anchor": "solid"})).is_err(), "a fade does not scale");
    assert!(exit(json!({"kind": "grow", "anchor": {"x": 1.5, "y": 0.5}})).is_err(), "outside the box");
    assert!(exit(json!({"kind": "grow", "anchor": "middle"})).is_err(), "not a name it knows");
}
