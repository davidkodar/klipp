use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A typed value supplied to an exposed composition-template parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TemplateValue {
    Text(String),
    Color([f32; 4]),
    Number(f64),
    Bool(bool),
    Enum(String),
}

/// Project-local state of one installed template. Defaults remain in the
/// template definition; only user changes and supplied media are persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInstance {
    pub template_id: Uuid,
    #[serde(default)]
    pub overrides: BTreeMap<String, TemplateValue>,
    #[serde(default)]
    pub media_slots: BTreeMap<String, String>,
}
