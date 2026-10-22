use serde::{Deserialize, Serialize};

use crate::keyframe;
use crate::keyframe::Keyframe;
use crate::modifier_math;

/// Identifies a specific parameter on a clip.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParamId {
    Opacity,
    PositionX,
    PositionY,
    Scale,
    Rotation,
    Volume,
    Pan,
    Gain,
    Effect {
        effect_index: usize,
        param_name: String,
    },
    /// A parameter of the node at `addr`, by key (M3 step 2): what an
    /// effect's `Effect { effect_index, .. }` names by place in its list.
    Node {
        addr: klipp_graph_format::path::NodePath,
        key: klipp_graph_format::ParamKey,
    },
    /// The height's own scale lane, keyed apart from the width's (#1471).
    /// Last, so the variants before it keep their places in the render
    /// cache's canonical bytes.
    ScaleY,
}

/// How a modifier combines with the base value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ModifierBlend {
    #[default]
    Add,
    Multiply,
    Replace,
}

/// Waveform shapes for oscillation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Waveform {
    #[default]
    Sine,
    Square,
    Triangle,
    Sawtooth,
}

/// What a lane does outside its keys (#2277), before its first or after
/// its last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Extrapolation {
    /// The end key's value, held: what a lane has always done.
    #[default]
    Hold,
    /// The keyed span repeated.
    Cycle,
    /// The keyed span repeated forwards, then backwards.
    PingPong,
    /// Carried on at the end key's rate of change.
    Continue,
}

impl Extrapolation {
    pub const ALL: [Extrapolation; 4] = [Extrapolation::Hold, Extrapolation::Cycle, Extrapolation::PingPong, Extrapolation::Continue];

    pub fn label(self) -> &'static str {
        match self {
            Extrapolation::Hold => "Hold",
            Extrapolation::Cycle => "Cycle",
            Extrapolation::PingPong => "Ping-Pong",
            Extrapolation::Continue => "Continue",
        }
    }
}

/// A single modifier in a parameter's modifier stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Modifier {
    Oscillate {
        amplitude: f32,
        frequency: f32,
        #[serde(default)]
        phase: f32,
        #[serde(default)]
        waveform: Waveform,
        #[serde(default)]
        blend: ModifierBlend,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Wiggle {
        amplitude: f32,
        frequency: f32,
        #[serde(default)]
        blend: ModifierBlend,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Throw {
        velocity: f32,
        #[serde(default)]
        acceleration: f32,
        #[serde(default)]
        blend: ModifierBlend,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Ramp {
        start_value: f32,
        end_value: f32,
        #[serde(default)]
        start_norm: f32, // 0.0 = clip start
        #[serde(default = "default_one")]
        end_norm: f32, // 1.0 = clip end
        #[serde(default)]
        blend: ModifierBlend,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Loop {
        period_secs: f64,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    PingPong {
        period_secs: f64,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Clamp {
        min: f32,
        max: f32,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// What the lane does before its first key and after its last (#2277).
    Extend {
        #[serde(default)]
        before: Extrapolation,
        #[serde(default)]
        after: Extrapolation,
        #[serde(default = "default_true")]
        enabled: bool,
    },
}

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}

/// A parameter's modifier stack.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModifierStack {
    pub modifiers: Vec<Modifier>,
}

/// Context for modifier evaluation, built once per frame.
#[derive(Debug, Clone)]
pub struct EvalContext {
    pub clip_time_secs: f64,
    pub clip_duration_secs: f64,
    pub timeline_time_secs: f64,
    pub fps: f64,
    pub seed: u64,
}

/// Evaluate a parameter: keyframes first, then modifiers.
pub fn evaluate_param(
    keyframes: &[Keyframe],
    stack: &ModifierStack,
    time_secs: f64,
    default: f32,
    ctx: &EvalContext,
) -> f32 {
    // Phase 1: time remapping modifiers
    let effective_time = apply_time_modifiers(&stack.modifiers, time_secs, ctx);

    // Phase 2: keyframe evaluation at (possibly remapped) time, extended
    // past the keys as the stack says.
    let (before, after) = extension(&stack.modifiers);
    let base = evaluate_extended(keyframes, before, after, effective_time, default);

    // Phase 3: value modifiers
    apply_value_modifiers(&stack.modifiers, base, ctx)
}

/// The stack's extension, before and after its keys: hold both with none.
pub fn extension(modifiers: &[Modifier]) -> (Extrapolation, Extrapolation) {
    modifiers
        .iter()
        .find_map(|m| match m {
            Modifier::Extend { before, after, enabled: true } => Some((*before, *after)),
            _ => None,
        })
        .unwrap_or_default()
}

/// `keyframes` at `time_secs`, carried past the first key as `before`
/// says and past the last as `after` says (#2277). Inside the keys, and
/// with fewer than two, exactly [`keyframe::evaluate`].
pub fn evaluate_extended(keyframes: &[Keyframe], before: Extrapolation, after: Extrapolation, time_secs: f64, default: f32) -> f32 {
    let at = |t: f64| keyframe::evaluate(keyframes, t, default);
    let (Some(first), Some(last)) = (keyframes.first(), keyframes.last()) else { return default };
    let (t0, t1) = (first.time_secs, last.time_secs);
    let span = t1 - t0;
    if keyframes.len() < 2 || span <= 1e-9 || (t0..=t1).contains(&time_secs) {
        return at(time_secs);
    }
    let (mode, end) = if time_secs > t1 { (after, t1) } else { (before, t0) };
    match mode {
        Extrapolation::Hold => at(time_secs),
        Extrapolation::Cycle => at(t0 + (time_secs - t0).rem_euclid(span)),
        Extrapolation::PingPong => {
            let k = ((time_secs - t0) / span).floor() as i64;
            let within = (time_secs - t0).rem_euclid(span);
            at(t0 + if k.rem_euclid(2) == 0 { within } else { span - within })
        }
        Extrapolation::Continue => {
            // The end's rate of change, read just inside it.
            let d = (span * 1e-3).min(1e-3);
            let inside = if end == t1 { end - d } else { end + d };
            let slope = (at(end) - at(inside)) as f64 / (end - inside);
            (at(end) as f64 + slope * (time_secs - end)) as f32
        }
    }
}

fn apply_time_modifiers(modifiers: &[Modifier], time_secs: f64, _ctx: &EvalContext) -> f64 {
    let mut t = time_secs;
    for m in modifiers {
        match m {
            Modifier::Loop {
                period_secs,
                enabled: true,
            } if *period_secs > 0.0 => {
                t = t.rem_euclid(*period_secs);
            }
            Modifier::PingPong {
                period_secs,
                enabled: true,
            } if *period_secs > 0.0 => {
                let cycle = (t / period_secs).floor() as i64;
                let within = t.rem_euclid(*period_secs);
                t = if cycle % 2 == 0 {
                    within
                } else {
                    period_secs - within
                };
            }
            _ => {}
        }
    }
    t
}

fn apply_value_modifiers(modifiers: &[Modifier], base: f32, ctx: &EvalContext) -> f32 {
    let mut value = base;
    let t = ctx.clip_time_secs as f32;

    for m in modifiers {
        match m {
            Modifier::Oscillate {
                amplitude,
                frequency,
                phase,
                waveform,
                blend,
                enabled: true,
            } => {
                let wave = modifier_math::waveform(t, *frequency, *phase, *waveform);
                let delta = amplitude * wave;
                value = apply_blend(value, delta, *blend);
            }
            Modifier::Wiggle {
                amplitude,
                frequency,
                blend,
                enabled: true,
            } => {
                let delta = modifier_math::wiggle_noise(
                    ctx.clip_time_secs,
                    *frequency,
                    *amplitude,
                    ctx.seed,
                );
                value = apply_blend(value, delta, *blend);
            }
            Modifier::Throw {
                velocity,
                acceleration,
                blend,
                enabled: true,
            } => {
                let delta = velocity * t + 0.5 * acceleration * t * t;
                value = apply_blend(value, delta, *blend);
            }
            Modifier::Ramp {
                start_value,
                end_value,
                start_norm,
                end_norm,
                blend,
                enabled: true,
            } => {
                let dur = ctx.clip_duration_secs as f32;
                if dur > 0.0 {
                    let norm_t = t / dur;
                    let range = end_norm - start_norm;
                    let frac = if range.abs() < 1e-9 {
                        0.0
                    } else {
                        ((norm_t - start_norm) / range).clamp(0.0, 1.0)
                    };
                    let ramp_val = start_value + (end_value - start_value) * frac;
                    value = apply_blend(value, ramp_val, *blend);
                }
            }
            Modifier::Clamp {
                min,
                max,
                enabled: true,
            } => {
                value = value.clamp(*min, *max);
            }
            // Time modifiers and disabled modifiers are skipped
            _ => {}
        }
    }
    value
}

fn apply_blend(base: f32, modifier_output: f32, blend: ModifierBlend) -> f32 {
    match blend {
        ModifierBlend::Add => base + modifier_output,
        ModifierBlend::Multiply => base * modifier_output,
        ModifierBlend::Replace => modifier_output,
    }
}

impl Modifier {
    /// Human-readable label for UI display
    pub fn label(&self) -> &'static str {
        match self {
            Modifier::Oscillate { .. } => "Oscillate",
            Modifier::Wiggle { .. } => "Wiggle",
            Modifier::Throw { .. } => "Throw",
            Modifier::Ramp { .. } => "Ramp",
            Modifier::Loop { .. } => "Loop",
            Modifier::PingPong { .. } => "Ping Pong",
            Modifier::Clamp { .. } => "Clamp",
            Modifier::Extend { .. } => "Outside Keys",
        }
    }

    pub fn is_enabled(&self) -> bool {
        match self {
            Modifier::Oscillate { enabled, .. }
            | Modifier::Wiggle { enabled, .. }
            | Modifier::Throw { enabled, .. }
            | Modifier::Ramp { enabled, .. }
            | Modifier::Loop { enabled, .. }
            | Modifier::PingPong { enabled, .. }
            | Modifier::Clamp { enabled, .. }
            | Modifier::Extend { enabled, .. } => *enabled,
        }
    }
}

impl ParamId {
    /// Convert to a string key for JSON serialization.
    pub fn to_key(&self) -> String {
        match self {
            ParamId::Opacity => "opacity".into(),
            ParamId::PositionX => "position_x".into(),
            ParamId::PositionY => "position_y".into(),
            ParamId::Scale => "scale".into(),
            ParamId::ScaleY => "scale_y".into(),
            ParamId::Rotation => "rotation".into(),
            ParamId::Volume => "volume".into(),
            ParamId::Pan => "pan".into(),
            ParamId::Gain => "gain".into(),
            ParamId::Effect { effect_index, param_name } => {
                format!("effect:{}:{}", effect_index, param_name)
            }
            // Never stored: a node's parameter is written under its effect's
            // place (`Effect`) until keys are stored by path. Distinct, so a
            // map built in memory can't merge it with another.
            ParamId::Node { addr, key } => {
                let ids: Vec<String> = addr.nodes.iter().map(|n| n.to_string()).collect();
                format!("node:{}:{}", ids.join("/"), key)
            }
        }
    }

    /// Parse from a string key (inverse of to_key).
    pub fn from_key(s: &str) -> Option<ParamId> {
        match s {
            "opacity" => Some(ParamId::Opacity),
            "position_x" => Some(ParamId::PositionX),
            "position_y" => Some(ParamId::PositionY),
            "scale" => Some(ParamId::Scale),
            "scale_y" => Some(ParamId::ScaleY),
            "rotation" => Some(ParamId::Rotation),
            "volume" => Some(ParamId::Volume),
            "pan" => Some(ParamId::Pan),
            "gain" => Some(ParamId::Gain),
            other => {
                let parts: Vec<&str> = other.splitn(3, ':').collect();
                if parts.len() == 3 && parts[0] == "effect" {
                    let effect_index = parts[1].parse().ok()?;
                    Some(ParamId::Effect {
                        effect_index,
                        param_name: parts[2].to_string(),
                    })
                } else {
                    None
                }
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            ParamId::Opacity => "Opacity".into(),
            ParamId::PositionX => "Position X".into(),
            ParamId::PositionY => "Position Y".into(),
            ParamId::Scale => "Scale".into(),
            ParamId::ScaleY => "Scale Y".into(),
            ParamId::Rotation => "Rotation".into(),
            ParamId::Volume => "Volume".into(),
            ParamId::Pan => "Pan".into(),
            ParamId::Gain => "Gain".into(),
            ParamId::Effect { param_name, .. } => param_name.clone(),
            ParamId::Node { key, .. } => key.to_string(),
        }
    }
}

#[cfg(test)]
mod extend_tests {
    use super::*;

    fn k(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// #2277: a lane keyed 0 at 1 s and 10 at 2 s, outside its keys.
    #[test]
    fn a_lane_holds_cycles_ping_pongs_or_continues_past_its_keys() {
        let keys = [k(1.0, 0.0), k(2.0, 10.0)];
        let e = |b, a, t| evaluate_extended(&keys, b, a, t, 0.0);
        use Extrapolation::*;
        assert_eq!(e(Hold, Hold, 3.5), 10.0, "held, as always");
        assert_eq!(e(Hold, Hold, 0.0), 0.0);
        assert!((e(Hold, Cycle, 2.5) - 5.0).abs() < 1e-4, "the span again from its start");
        assert!((e(Hold, PingPong, 2.25) - 7.5).abs() < 1e-4, "back down from the end");
        assert!((e(Hold, PingPong, 3.25) - 2.5).abs() < 1e-4, "and up again");
        assert!((e(Hold, Continue, 3.0) - 20.0).abs() < 1e-2, "on at 10 a second");
        assert!((e(Continue, Hold, 0.5) + 5.0).abs() < 1e-2, "and before the first key too");
        assert!((e(Cycle, Hold, 0.25) - 2.5).abs() < 1e-4, "cycled backwards");
        assert_eq!(e(Cycle, Cycle, 1.5), 5.0, "inside the keys, untouched");
    }

    /// The stack carries it, so the renderer, which reads the stack, does too.
    #[test]
    fn the_extend_modifier_is_what_evaluate_param_reads() {
        let keys = [k(0.0, 0.0), k(1.0, 1.0)];
        let stack = ModifierStack { modifiers: vec![Modifier::Extend { before: Extrapolation::Hold, after: Extrapolation::Cycle, enabled: true }] };
        let ctx = EvalContext { clip_time_secs: 1.5, clip_duration_secs: 4.0, timeline_time_secs: 1.5, fps: 24.0, seed: 0 };
        assert!((evaluate_param(&keys, &stack, 1.5, 0.0, &ctx) - 0.5).abs() < 1e-4);
        let off = ModifierStack { modifiers: vec![Modifier::Extend { before: Extrapolation::Hold, after: Extrapolation::Cycle, enabled: false }] };
        assert_eq!(evaluate_param(&keys, &off, 1.5, 0.0, &ctx), 1.0, "off, it holds");
        let json = serde_json::to_string(&stack.modifiers[0]).unwrap();
        assert!(json.contains("\"type\":\"extend\"") && json.contains("\"after\":\"cycle\""), "{json}");
    }
}
