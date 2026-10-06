// Frozen from crates/klipp-core/src/modifier.rs at 7d309e9f. Do not follow app type changes.
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ModifierBlend {
    #[default]
    Add,
    Multiply,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Waveform {
    #[default]
    Sine,
    Square,
    Triangle,
    Sawtooth,
}

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModifierStack {
    pub modifiers: Vec<Modifier>,
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
            ParamId::Effect {
                effect_index,
                param_name,
            } => {
                format!("effect:{}:{}", effect_index, param_name)
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
}
