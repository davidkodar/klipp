//! The looks a title may be given by name: the brief's type styles,
//! and the three built in. One table for the title op and the edit
//! plan, so a "heading" is the same heading wherever it is asked for.

use klipp_timeline::{Brief, TypeStyle};

/// The look called `name`: the brief's own, else a built-in one.
pub fn look(brief: Option<&Brief>, name: &str) -> Option<TypeStyle> {
    match brief {
        Some(b) => b.type_style(name),
        None => TypeStyle::builtin(&name.trim().to_lowercase()),
    }
}

/// Every name `look` answers to, the brief's first.
pub fn names(brief: Option<&Brief>) -> Vec<String> {
    match brief {
        Some(b) => b.type_style_names(),
        None => TypeStyle::BUILTIN.iter().map(|s| s.to_string()).collect(),
    }
}

fn motion_kind(name: &str) -> Result<klipp_core::title_motion::MotionKind, String> {
    use klipp_core::title_motion::MotionKind::*;
    Ok(match name.to_lowercase().as_str() {
        "fade" => Fade, "rise" => Rise, "drop" => Drop,
        "slide_left" | "slide" | "slideleft" => SlideLeft,
        "slide_right" | "slideright" => SlideRight,
        "scale_up" | "scaleup" | "zoom_in" | "grow" => ScaleUp,
        "scale_down" | "scaledown" | "settle" => ScaleDown,
        "blur" | "focus" => Blur,
        "mask_rise" => MaskRise, "mask_drop" => MaskDrop,
        "typewriter" | "type" => Typewriter,
        "pop" => Pop, "slam" => Slam,
        "fly_left" | "fly_from_left" => FlyLeft, "fly_right" | "fly_from_right" => FlyRight,
        "fly_top" | "fly_from_top" => FlyTop, "fly_bottom" | "fly_from_bottom" => FlyBottom,
        "bounce" => Bounce, "flicker" => Flicker, "focus_in" => FocusIn, "soft_rise" => SoftRise,
        _ => return Err(format!("{name:?} is not a motion kind; use fade, rise, drop, slide_left, slide_right, scale_up, scale_down, blur, mask_rise, mask_drop, typewriter, pop, slam, fly_left, fly_right, fly_top, fly_bottom, bounce, flicker, focus_in or soft_rise")),
    })
}

fn stagger_unit(name: &str) -> Result<klipp_core::title_motion::StaggerUnit, String> {
    use klipp_core::title_motion::StaggerUnit::*;
    Ok(match name.to_lowercase().as_str() {
        "none" => None,
        "line" => Line,
        "word" => Word,
        "char" | "character" => Char,
        _ => {
            return Err(format!(
                "{name:?} is not a stagger; use none, line, word or char"
            ))
        }
    })
}

fn parse_motion(
    spec: &klipp_timeline::TitleMotionSpec,
) -> Result<Option<klipp_core::title_motion::Motion>, String> {
    use klipp_core::title_motion::{Easing, Motion, MotionKind, StaggerUnit};
    if spec.kind.eq_ignore_ascii_case("none") {
        if spec.duration_secs.is_some()
            || spec.delay_secs.is_some()
            || spec.easing.is_some()
            || spec.stagger.is_some()
            || spec.stagger_secs.is_some()
            || spec.distance.is_some()
        {
            return Err("motion kind none cannot have timing, easing, stagger or distance".into());
        }
        return Ok(None);
    }
    let kind = motion_kind(&spec.kind)?;
    let mut m = Motion::new(kind);
    if kind == MotionKind::Typewriter && spec.stagger.is_none() {
        m = m.stagger(StaggerUnit::Char, 0.04);
    }
    if let Some(v) = spec.duration_secs {
        if !v.is_finite() || v <= 0.0 || v > 5.0 {
            return Err("motion duration_secs must be finite, greater than 0 and at most 5".into());
        }
        m.duration_secs = v;
    }
    for (name, v, max) in [
        ("delay_secs", spec.delay_secs, 3600.0),
        ("stagger_secs", spec.stagger_secs, 60.0),
        ("distance", spec.distance, 100.0),
    ] {
        if v.is_some_and(|v| !v.is_finite() || v < 0.0 || v > max) {
            return Err(format!(
                "motion {name} must be finite and between 0 and {max}"
            ));
        }
    }
    if let Some(v) = spec.delay_secs {
        m.delay_secs = v;
    }
    if let Some(v) = spec.distance {
        if !matches!(
            kind,
            MotionKind::Rise | MotionKind::Drop | MotionKind::SlideLeft | MotionKind::SlideRight
        ) {
            return Err(
                "distance is only supported for rise, drop, slide_left and slide_right".into(),
            );
        }
        m.distance = v;
    }
    if let Some(v) = &spec.stagger {
        m.stagger = stagger_unit(v)?;
    }
    if let Some(v) = spec.stagger_secs {
        if m.stagger == StaggerUnit::None {
            return Err("stagger_secs needs an active stagger unit (line, word or char)".into());
        }
        m.stagger_secs = v;
    }
    if let Some(v) = &spec.easing {
        m.easing = match v.to_lowercase().as_str() {
            "linear" => Easing::Linear, "out_cubic" => Easing::OutCubic,
            "out_quint" => Easing::OutQuint, "out_expo" => Easing::OutExpo,
            "out_back" => Easing::OutBack, "out_bounce" => Easing::OutBounce,
            "in_out_cubic" => Easing::InOutCubic, "in_cubic" => Easing::InCubic,
            _ => return Err(format!("{v:?} is not an easing; use linear, out_cubic, out_quint, out_expo, out_back, out_bounce, in_out_cubic or in_cubic")),
        };
    }
    Ok(Some(m))
}

/// Check entrance syntax and legacy values before style inheritance. Nested
/// fields are parsed once, after inheritance, on every authoring path. Exit
/// and legacy entrance may coexist; two entrance syntaxes may not.
pub(crate) fn check_motion_overrides(
    enter: Option<&klipp_timeline::TitleMotionSpec>,
    animate: Option<&str>,
    animate_secs: Option<f64>,
    stagger: Option<&str>,
) -> Result<(), String> {
    if enter.is_some() && (animate.is_some() || animate_secs.is_some() || stagger.is_some()) {
        return Err(
            "enter conflicts with animate/animate_secs/stagger; use one entrance syntax".into(),
        );
    }
    if let Some(a) = animate {
        motion_kind(a)?;
    }
    if let Some(s) = stagger {
        stagger_unit(s)?;
    }
    if animate_secs.is_some_and(|v| !v.is_finite()) {
        return Err("animate_secs must be finite (legacy values clamp to 0.05..5)".into());
    }
    Ok(())
}

/// Only the motion fields needed for inheritance and lowering.
pub(crate) struct MotionFields {
    pub enter: Option<klipp_timeline::TitleMotionSpec>,
    pub exit: Option<klipp_timeline::TitleMotionSpec>,
    pub animate: Option<String>,
    pub animate_secs: Option<f32>,
    pub stagger: Option<String>,
}

impl From<&TypeStyle> for MotionFields {
    fn from(style: &TypeStyle) -> Self {
        Self {
            enter: style.enter.clone(),
            exit: style.exit.clone(),
            animate: style.animate.clone(),
            animate_secs: style.animate_secs,
            stagger: style.stagger.clone(),
        }
    }
}

/// Explicit nested motion replaces a whole side; omitted sides inherit.
/// Legacy timing modifies active inherited motion and is ignored for disabled
/// entrances, matching the legacy no-animation case.
pub(crate) fn inherit_motion(
    style: &TypeStyle,
    enter: Option<&klipp_timeline::TitleMotionSpec>,
    exit: Option<&klipp_timeline::TitleMotionSpec>,
    animate: Option<&str>,
    animate_secs: Option<f32>,
    stagger: Option<&str>,
) -> MotionFields {
    let mut result = MotionFields::from(style);
    if let Some(enter) = enter {
        result.enter = Some(enter.clone());
        result.animate = None;
        result.animate_secs = None;
        result.stagger = None;
    } else if let (Some(inherited), None) = (result.enter.as_mut(), animate) {
        // Legacy timing-only overrides still edit the inherited entrance;
        // they must not silently turn a nested style's motion off.
        if !inherited.kind.eq_ignore_ascii_case("none") {
            inherited.duration_secs = animate_secs.map(|v| v.clamp(0.05, 5.0)).or(inherited.duration_secs);
            inherited.stagger = stagger.map(str::to_string).or(inherited.stagger.take());
            if stagger.is_some_and(|s| s.eq_ignore_ascii_case("none")) {
                inherited.stagger_secs = None;
            }
        }
    } else if animate.is_some() || animate_secs.is_some() || stagger.is_some() {
        result.enter = None;
        result.animate = animate.map(str::to_string).or(result.animate);
        result.animate_secs = animate_secs.or(result.animate_secs);
        result.stagger = stagger.map(str::to_string).or(result.stagger);
    }
    if let Some(exit) = exit {
        result.exit = Some(exit.clone());
    }
    result
}

/// Lower authoring configuration to the existing renderer model. Legacy
/// entrance defaults (including typewriter's char/0.04 stagger) are preserved.
pub fn resolve_motion(style: &TypeStyle) -> Result<klipp_core::title_motion::TitleMotion, String> {
    MotionFields::from(style).resolve()
}

impl MotionFields {
    pub(crate) fn resolve(&self) -> Result<klipp_core::title_motion::TitleMotion, String> {
        let style = self;
        use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit, TitleMotion};
        check_motion_overrides(
            style.enter.as_ref(),
            style.animate.as_deref(),
            style.animate_secs.map(f64::from),
            style.stagger.as_deref(),
        )?;
        let enter = if let Some(m) = &style.enter {
            parse_motion(m).map_err(|e| format!("enter: {e}"))?
        } else if let Some(a) = &style.animate {
            let kind = motion_kind(a)?;
            let mut m =
                Motion::new(kind).duration(style.animate_secs.unwrap_or(0.6).clamp(0.05, 5.0));
            if let Some(s) = &style.stagger {
                m = m.stagger(stagger_unit(s)?, 0.05);
            } else if kind == MotionKind::Typewriter {
                m = m.stagger(StaggerUnit::Char, 0.04);
            }
            Some(m)
        } else {
            None
        };
        let exit = style
            .exit
            .as_ref()
            .map(parse_motion)
            .transpose()
            .map_err(|e| format!("exit: {e}"))?
            .flatten();
        Ok(TitleMotion { enter, exit })
    }
}

/// Resolve the effective authoring motion once, under one numeric and
/// visibility contract shared by title ops and edit-plan validation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn effective_motion(
    style: &TypeStyle,
    enter: Option<&klipp_timeline::TitleMotionSpec>,
    exit: Option<&klipp_timeline::TitleMotionSpec>,
    animate: Option<&str>,
    animate_secs: Option<f64>,
    stagger: Option<&str>,
    secs: f64,
) -> Result<klipp_core::title_motion::TitleMotion, String> {
    check_motion_overrides(enter, animate, animate_secs, stagger)?;
    if let Some(v) = animate_secs {
        if !(0.0 < v && v <= 5.0) {
            return Err(format!("animate_secs must be greater than 0 and at most 5 seconds; got {v}"));
        }
    }
    let motion = inherit_motion(
        style,
        enter,
        exit,
        animate,
        animate_secs.map(|v| v as f32),
        stagger,
    )
    .resolve()?;
    let start = motion.enter.map_or(0.0, |m| m.delay_secs as f64);
    let lead = motion.exit.map_or(0.0, |m| m.delay_secs as f64);
    // Unit zero has the widest possible visible interval. Every later
    // staggered unit starts later and exits earlier; if this is empty,
    // the entire title is guaranteed invisible. Partial motion is legal.
    if start + lead >= secs {
        return Err(format!("title lasts {secs} s but entrance delay {start} s plus exit lead {lead} s leaves no visible interval"));
    }
    Ok(motion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_legacy_timing_outside_range_is_refused() {
        let style = TypeStyle::default();
        let err = effective_motion(&style, None, None, Some("rise"), Some(0.0), None, 2.0).unwrap_err();
        assert!(err.contains("greater than 0"), "{err}");
    }

    #[test]
    fn the_brief_replaces_a_built_in_look_by_name_and_adds_its_own() {
        let brief = Brief {
            type_styles: vec![
                TypeStyle {
                    name: "heading".into(),
                    font: Some("Inter".into()),
                    size: Some(60.0),
                    ..Default::default()
                },
                TypeStyle {
                    name: "kicker".into(),
                    size: Some(28.0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            look(Some(&brief), "Heading ").unwrap().font.as_deref(),
            Some("Inter")
        );
        assert_eq!(look(Some(&brief), "kicker").unwrap().size, Some(28.0));
        assert_eq!(look(Some(&brief), "card").unwrap().full_frame, Some(true));
        assert!(look(Some(&brief), "banner").is_none());
        assert_eq!(
            names(Some(&brief)),
            vec!["heading", "kicker", "callout", "card"]
        );
        assert_eq!(look(None, "callout").unwrap().y, Some(0.85));
    }
}
