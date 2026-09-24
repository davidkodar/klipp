use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum TemplateValue {
    Text(String),
    Color([f32; 4]),
    Number(f64),
    Bool(bool),
    Enum(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInstance {
    pub template_id: Uuid,
    #[serde(default)]
    pub overrides: BTreeMap<String, TemplateValue>,
    #[serde(default)]
    pub media_slots: BTreeMap<String, String>,
}
