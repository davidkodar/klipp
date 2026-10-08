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

    // Phase 2: keyframe evaluation at (possibly remapped) time
    let base = keyframe::evaluate(keyframes, effective_time, default);

    // Phase 3: value modifiers
    apply_value_modifiers(&stack.modifiers, base, ctx)
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
            | Modifier::Clamp { enabled, .. } => *enabled,
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
            ParamId::Rotation => "Rotation".into(),
            ParamId::Volume => "Volume".into(),
            ParamId::Pan => "Pan".into(),
            ParamId::Gain => "Gain".into(),
            ParamId::Effect { param_name, .. } => param_name.clone(),
            ParamId::Node { key, .. } => key.to_string(),
        }
    }
}
