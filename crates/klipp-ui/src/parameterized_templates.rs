//! User-authored composition templates with typed, exposed parameters.
//!
//! A [`CompositionTemplate`] is a normal Klipp composition (usually a
//! precomposition) plus the small amount of metadata needed to make it
//! reusable from the app.  The renderer does not need a template-specific
//! path: an instance resolves its overrides onto the ordinary composition
//! before rendering.  Keeping this metadata separate from the layer model
//! also means old projects continue to load unchanged.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// File extension for user-authored composition templates.
pub const EXTENSION: &str = "klipptemplate";
pub const FORMAT_VERSION: u32 = 1;

/// Values that can be exposed in the first template version.  These map
/// directly to controls the inspector can render without knowing the template
/// implementation.
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
pub struct NumberRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// A parameter declaration and its default value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateParameter {
    pub label: String,
    pub default: TemplateValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<NumberRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

impl TemplateParameter {
    pub fn validate(&self, value: &TemplateValue) -> Result<(), String> {
        let same_kind = matches!(
            (&self.default, value),
            (TemplateValue::Text(_), TemplateValue::Text(_))
                | (TemplateValue::Color(_), TemplateValue::Color(_))
                | (TemplateValue::Number(_), TemplateValue::Number(_))
                | (TemplateValue::Bool(_), TemplateValue::Bool(_))
                | (TemplateValue::Enum(_), TemplateValue::Enum(_))
        );
        if !same_kind {
            return Err(format!("value for '{}' has the wrong type", self.label));
        }
        if let TemplateValue::Number(number) = value {
            if let Some(range) = &self.range {
                if range.min.is_some_and(|min| *number < min)
                    || range.max.is_some_and(|max| *number > max)
                {
                    return Err(format!("value for '{}' is outside its range", self.label));
                }
            }
        }
        if let TemplateValue::Enum(choice) = value {
            if !self.choices.is_empty() && !self.choices.iter().any(|item| item == choice) {
                return Err(format!("unknown choice '{choice}' for '{}'", self.label));
            }
        }
        Ok(())
    }
}

/// A binding from an exposed parameter to a property in the ordinary
/// composition.  The property path is intentionally opaque to this layer;
/// the document resolver owns its interpretation and can evolve it without
/// changing the on-disk template registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateBinding {
    pub parameter: String,
    pub property: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionTemplate {
    pub format_version: u32,
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// The sequence/precomposition that supplies the actual layers.
    pub composition_id: Uuid,
    #[serde(default)]
    pub parameters: BTreeMap<String, TemplateParameter>,
    #[serde(default)]
    pub bindings: Vec<TemplateBinding>,
    #[serde(default)]
    pub written_by: String,
}

impl CompositionTemplate {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version == 0 || self.format_version > FORMAT_VERSION {
            return Err(format!(
                "unsupported template format {}",
                self.format_version
            ));
        }
        if self.name.trim().is_empty() {
            return Err("a template needs a name".into());
        }
        for (name, parameter) in &self.parameters {
            parameter
                .validate(&parameter.default)
                .map_err(|error| format!("parameter '{name}': {error}"))?;
        }
        for binding in &self.bindings {
            if !self.parameters.contains_key(&binding.parameter) {
                return Err(format!(
                    "binding references unknown parameter '{}'",
                    binding.parameter
                ));
            }
            if binding.property.trim().is_empty() {
                return Err(format!(
                    "binding for '{}' has no property",
                    binding.parameter
                ));
            }
        }
        Ok(())
    }

    pub fn new(name: impl Into<String>, composition_id: Uuid) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            category: String::new(),
            composition_id,
            parameters: BTreeMap::new(),
            bindings: Vec::new(),
            written_by: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// An inserted template instance. Only overrides are stored; default values
/// remain owned by the template so updating a template can be handled
/// explicitly instead of silently duplicating its whole composition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInstance {
    pub template_id: Uuid,
    #[serde(default)]
    pub overrides: BTreeMap<String, TemplateValue>,
}

impl TemplateInstance {
    pub fn set(
        &mut self,
        template: &CompositionTemplate,
        name: &str,
        value: TemplateValue,
    ) -> Result<(), String> {
        let parameter = template
            .parameters
            .get(name)
            .ok_or_else(|| format!("template has no parameter '{name}'"))?;
        parameter.validate(&value)?;
        self.overrides.insert(name.to_string(), value);
        Ok(())
    }

    pub fn value<'a>(
        &'a self,
        template: &'a CompositionTemplate,
        name: &str,
    ) -> Option<&'a TemplateValue> {
        self.overrides.get(name).or_else(|| {
            template
                .parameters
                .get(name)
                .map(|parameter| &parameter.default)
        })
    }
}

#[derive(Debug, Default)]
pub struct CompositionTemplateStore {
    dir: Option<PathBuf>,
    entries: Vec<(PathBuf, CompositionTemplate)>,
}

impl CompositionTemplateStore {
    pub fn load_default() -> Self {
        Self::load(dirs::config_dir().map(|dir| dir.join("klipp").join("templates")))
    }

    pub fn load(dir: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        if let Some(Ok(read)) = dir.as_deref().map(std::fs::read_dir) {
            for path in read.flatten().map(|entry| entry.path()) {
                if path.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION) {
                    continue;
                }
                match std::fs::read_to_string(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|json| {
                        serde_json::from_str::<CompositionTemplate>(&json)
                            .map_err(|error| error.to_string())
                    }) {
                    Ok(template) if template.validate().is_ok() => entries.push((path, template)),
                    Ok(template) => tracing::warn!(
                        "Skipping invalid template '{}': {}",
                        path.display(),
                        template.validate().unwrap_err()
                    ),
                    Err(error) => tracing::warn!("Skipping template '{}': {error}", path.display()),
                }
            }
        }
        entries.sort_by_key(|(_, template)| template.name.to_lowercase());
        Self { dir, entries }
    }

    pub fn templates(&self) -> impl Iterator<Item = &CompositionTemplate> {
        self.entries.iter().map(|(_, template)| template)
    }

    pub fn save(&mut self, template: CompositionTemplate) -> Result<(), String> {
        template.validate()?;
        let dir = self
            .dir
            .as_ref()
            .ok_or("no template directory configured")?;
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let path = self
            .entries
            .iter()
            .find(|(_, existing)| existing.id == template.id)
            .map(|(path, _)| path.clone())
            .unwrap_or_else(|| dir.join(format!("{}.{}", safe_stem(&template.name), EXTENSION)));
        let json = serde_json::to_string_pretty(&template).map_err(|error| error.to_string())?;
        std::fs::write(&path, json).map_err(|error| error.to_string())?;
        self.entries
            .retain(|(_, existing)| existing.id != template.id);
        self.entries.push((path, template));
        self.entries
            .sort_by_key(|(_, item)| item.name.to_lowercase());
        Ok(())
    }
}

fn safe_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "template".into()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_uses_defaults_and_validates_overrides() {
        let mut template = CompositionTemplate::new("Terminal card", Uuid::new_v4());
        template.parameters.insert(
            "text".into(),
            TemplateParameter {
                label: "Terminal text".into(),
                default: TemplateValue::Text("hello".into()),
                range: None,
                choices: vec![],
            },
        );
        template.parameters.insert(
            "speed".into(),
            TemplateParameter {
                label: "Typing speed".into(),
                default: TemplateValue::Number(0.25),
                range: Some(NumberRange {
                    min: Some(0.05),
                    max: Some(2.0),
                }),
                choices: vec![],
            },
        );
        template.validate().unwrap();
        let mut instance = TemplateInstance {
            template_id: template.id,
            overrides: BTreeMap::new(),
        };
        assert_eq!(
            instance.value(&template, "text"),
            Some(&TemplateValue::Text("hello".into()))
        );
        instance
            .set(&template, "speed", TemplateValue::Number(0.5))
            .unwrap();
        assert!(instance
            .set(&template, "speed", TemplateValue::Number(4.0))
            .is_err());
    }

    #[test]
    fn invalid_binding_is_rejected() {
        let mut template = CompositionTemplate::new("Card", Uuid::new_v4());
        template.bindings.push(TemplateBinding {
            parameter: "missing".into(),
            property: "layer.text".into(),
        });
        assert!(template.validate().is_err());
    }

    #[test]
    fn template_round_trips() {
        let mut template = CompositionTemplate::new("Terminal card", Uuid::new_v4());
        template.parameters.insert(
            "accent".into(),
            TemplateParameter {
                label: "Accent".into(),
                default: TemplateValue::Color([0.6, 0.4, 1.0, 1.0]),
                range: None,
                choices: vec![],
            },
        );
        let json = serde_json::to_string(&template).unwrap();
        let loaded: CompositionTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, template);
    }
}
