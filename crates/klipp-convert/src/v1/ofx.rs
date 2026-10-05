// Frozen from crates/klipp-project/src/format.rs at 7d309e9f. Do not follow app type changes.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
fn default_visible() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EffectJson {
    pub plugin_name: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
    #[serde(default)]
    pub param_values: HashMap<String, OfxValueJson>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "t", content = "v")]
pub enum OfxValueJson {
    Double(f64),
    Double2([f64; 2]),
    Double3([f64; 3]),
    Int(i32),
    Int2([i32; 2]),
    Int3([i32; 3]),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba([f64; 4]),
    Rgb([f64; 3]),
}

impl<'de> serde::Deserialize<'de> for OfxValueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        use serde_json::Value;

        let v = Value::deserialize(deserializer)?;

        if let Some(tag) = v.get("t").and_then(|t| t.as_str()) {
            return match tag {
                "Double" => v
                    .get("v")
                    .and_then(|x| x.as_f64())
                    .map(OfxValueJson::Double)
                    .ok_or_else(|| de::Error::custom("invalid Double value")),
                "Double2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double2)
                    .map_err(de::Error::custom),
                "Double3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double3)
                    .map_err(de::Error::custom),
                "Int" => v
                    .get("v")
                    .and_then(|x| x.as_i64())
                    .map(|i| OfxValueJson::Int(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Int value")),
                "Int2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int2)
                    .map_err(de::Error::custom),
                "Int3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int3)
                    .map_err(de::Error::custom),
                "Bool" => v
                    .get("v")
                    .and_then(|x| x.as_bool())
                    .map(OfxValueJson::Bool)
                    .ok_or_else(|| de::Error::custom("invalid Bool value")),
                "Choice" => v
                    .get("v")
                    .and_then(|x| x.as_i64())
                    .map(|i| OfxValueJson::Choice(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Choice value")),
                "String" => v
                    .get("v")
                    .and_then(|x| x.as_str())
                    .map(|s| OfxValueJson::String(s.to_string()))
                    .ok_or_else(|| de::Error::custom("invalid String value")),
                "Rgba" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgba)
                    .map_err(de::Error::custom),
                "Rgb" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgb)
                    .map_err(de::Error::custom),
                _ => Err(de::Error::custom(format!(
                    "unknown OfxValueJson tag: {tag}"
                ))),
            };
        }

        if let Some(b) = v.as_bool() {
            return Ok(OfxValueJson::Bool(b));
        }
        if let Some(s) = v.as_str() {
            return Ok(OfxValueJson::String(s.to_string()));
        }
        if let Some(n) = v.as_i64() {
            return Ok(OfxValueJson::Int(n as i32));
        }
        if let Some(n) = v.as_f64() {
            return Ok(OfxValueJson::Double(n));
        }

        Err(de::Error::custom("cannot parse OfxValueJson"))
    }
}
