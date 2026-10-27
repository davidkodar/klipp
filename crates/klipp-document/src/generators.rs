//! Generator defaults and validation shared by authored backgrounds and ops.

use klipp_core::GeneratorData;
use klipp_timeline::PlanBackground;

fn colour(value: &str) -> Result<[u8; 4], String> {
    let s = value.trim();
    if s.len() != 7 || !s.starts_with('#') || !s[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{value:?} is not a colour; use #rrggbb"));
    }
    let channel =
        |offset| u8::from_str_radix(&s[offset..offset + 2], 16).map_err(|e| e.to_string());
    Ok([channel(1)?, channel(3)?, channel(5)?, 255])
}

/// Turn a plan's named background into the same generator data the UI uses.
/// Explicit colours override the brief's palette; missing colours keep the
/// generator's defaults. The brief may contain a larger palette.
pub fn background_data(
    background: &PlanBackground,
    brand: &[String],
) -> Result<GeneratorData, String> {
    let name = background
        .generator
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_");
    let max = match name.as_str() {
        "gradient" | "living_gradient" => 2,
        "solid" | "solid_color" | "background" | "backgrounds" | "grain" | "circle" | "placeholder" => 1,
        "countdown" | "color_bars" => 0,
        _ => return Err(format!("unknown generator {:?}; use solid, gradient, living_gradient, grain, circle, countdown, placeholder or color_bars", background.generator)),
    };
    if background.colors.len() > max {
        return Err(format!(
            "{} takes at most {max} colours",
            background.generator
        ));
    }
    let palette = if background.colors.is_empty() {
        brand
    } else {
        &background.colors
    };
    let color = |i: usize, default| palette.get(i).map(|s| colour(s)).unwrap_or(Ok(default));
    Ok(match name.as_str() {
        "gradient" => GeneratorData::Gradient {
            top: color(0, [36, 52, 92, 255])?,
            bottom: color(1, [8, 10, 18, 255])?,
            angle_deg: 0.0,
            four_point: None,
            grain: klipp_core::DEFAULT_GENERATOR_GRAIN,
        },
        "living_gradient" => GeneratorData::LivingGradient {
            a: color(0, [22, 34, 78, 255])?,
            b: color(1, [64, 24, 82, 255])?,
            angle_deg: 20.0,
            drift_deg_per_sec: 4.0,
            glow: 0.35,
            grain: klipp_core::DEFAULT_GENERATOR_GRAIN,
        },
        "grain" => GeneratorData::Grain {
            color: color(0, [26, 26, 30, 255])?,
            amount: 0.35,
            size_px: 2.0,
            speed: 24.0,
        },
        "circle" => GeneratorData::Polygon {
            sides: 24,
            color: color(0, [255, 255, 255, 255])?,
            outline_width_px: 0.0,
            fill: true,
        },
        "countdown" => GeneratorData::Countdown,
        "color_bars" => GeneratorData::ColorBars,
        "placeholder" => GeneratorData::Placeholder {
            label: "Missing shot".into(),
            color: color(0, [66, 70, 84, 255])?,
        },
        _ => GeneratorData::SolidColor {
            color: color(0, [0, 0, 0, 255])?,
        },
    })
}

/// Refuse non-finite or nonsensical native parameters before adding a clip.
pub fn validate(data: &GeneratorData) -> Result<(), String> {
    let nonnegative = |v: f32| v.is_finite() && v >= 0.0;
    let grain_ok = |v: f32| (0.0..=klipp_effects::generator_grain::MAX_GENERATOR_GRAIN).contains(&v);
    let valid = match data {
        GeneratorData::SolidColor { .. }
        | GeneratorData::Placeholder { .. }
        | GeneratorData::ColorBars
        | GeneratorData::ColorBars100
        | GeneratorData::ColorBarsSmpte
        | GeneratorData::Countdown
        | GeneratorData::Slate { .. } => true,
        GeneratorData::Gradient { angle_deg, grain, .. } => angle_deg.is_finite() && grain_ok(*grain),
        GeneratorData::LivingGradient {
            angle_deg,
            drift_deg_per_sec,
            glow,
            grain,
            ..
        } => angle_deg.is_finite() && drift_deg_per_sec.is_finite() && (0.0..=1.0).contains(glow) && grain_ok(*grain),
        GeneratorData::Grain {
            amount,
            size_px,
            speed,
            ..
        } => {
            (0.0..=1.0).contains(amount)
                && size_px.is_finite()
                && *size_px > 0.0
                && nonnegative(*speed)
        }
        GeneratorData::Timer {
            initial_seconds, ..
        } => nonnegative(*initial_seconds),
        GeneratorData::Polygon {
            sides,
            outline_width_px,
            ..
        } => (3..=512).contains(sides) && nonnegative(*outline_width_px),
        GeneratorData::Star {
            points,
            inner_radius_pct,
            outline_width_px,
            ..
        } => {
            (3..=256).contains(points)
                && (0.0..=1.0).contains(inner_radius_pct)
                && *inner_radius_pct > 0.0
                && nonnegative(*outline_width_px)
        }
        GeneratorData::StillFrame {
            png_path,
            width,
            height,
        } => png_path.is_absolute() && png_path.is_file() && *width > 0 && *height > 0,
    };
    if valid {
        Ok(())
    } else {
        Err("invalid generator parameters: numbers must be finite; glow/amount are 0..1, a gradient's grain 0..8, grain size positive, speed/outline/timer nonnegative, shapes need valid sides/radii, and stills need an existing absolute path and positive dimensions".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_backgrounds_use_native_generators_and_explicit_colours() {
        for name in [
            "solid",
            "gradient",
            "living_gradient",
            "grain",
            "circle",
            "countdown",
            "placeholder",
            "color_bars",
        ] {
            let data = background_data(
                &PlanBackground {
                    generator: name.into(),
                    colors: Vec::new(),
                },
                &[],
            )
            .unwrap();
            validate(&data).unwrap();
            let wire = serde_json::to_value(&data).unwrap();
            assert_eq!(serde_json::from_value::<GeneratorData>(wire).unwrap(), data);
        }
        let bg = PlanBackground {
            generator: "gradient".into(),
            colors: vec!["#102030".into(), "#405060".into()],
        };
        assert!(matches!(
            background_data(&bg, &["#ffffff".into()]).unwrap(),
            GeneratorData::Gradient {
                top: [16, 32, 48, 255],
                bottom: [64, 80, 96, 255],
                ..
            }
        ));
        assert!(background_data(
            &PlanBackground {
                generator: "unknown".into(),
                colors: vec![]
            },
            &[]
        )
        .is_err());
        assert!(validate(&GeneratorData::Grain {
            color: [0; 4],
            amount: 2.0,
            size_px: 1.0,
            speed: 24.0
        })
        .is_err());
    }
}
