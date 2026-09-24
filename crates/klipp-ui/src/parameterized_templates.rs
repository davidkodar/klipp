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

use klipp_timeline::{
    Easing, LayerFx, Motion, MotionBlur, MotionKind, ObjectTransform, Shape, ShapeKind,
    ShapeStroke, TextAlignment, TextBox, TextGlow, TitleData, TitleLoop, TitleMotion,
    TitleObjectContent, TitlePosition,
};

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

/// The bundled terminal card used by the Lunel-style launch-film workflow.
///
/// This is deliberately ordinary title content: a dark rounded panel, a
/// monospaced command line and a blinking cursor. The card enters and exits
/// with eased motion, drifts a few pixels while held, and carries the same
/// motion-blur path as any hand-built title. The stable object ids are used by
/// the template bindings below.
pub fn terminal_card_composition() -> TitleData {
    let mut title = TitleData::default();
    title.objects.clear();
    title.next_object_id = 0;
    let card_transform = ObjectTransform {
        center: TitlePosition { x: 0.5, y: 0.5 },
        center_x_keyframes: vec![
            key(0.0, 0.44, Easing::OutCubic),
            key(0.16, 0.5, Easing::OutCubic),
            key(0.55, 0.5, Easing::InOutCubic),
            key(0.9, 0.505, Easing::InOutCubic),
            key(1.25, 0.49, Easing::OutCubic),
        ],
        center_y_keyframes: vec![
            key(0.0, 0.52, Easing::OutCubic),
            key(0.16, 0.5, Easing::OutCubic),
            key(0.72, 0.497, Easing::InOutCubic),
            key(1.1, 0.503, Easing::InOutCubic),
            key(1.3, 0.5, Easing::OutCubic),
        ],
        size: TitlePosition { x: 0.42, y: 0.22 },
        rotation: 0.0,
        anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
    };
    let mut card_fx = LayerFx {
        motion: TitleMotion::both(
            Motion::new(MotionKind::SlideRight)
                .duration(0.16)
                .easing(Easing::OutCubic)
                .distance(0.7),
            Motion::new(MotionKind::SlideLeft)
                .duration(0.18)
                .delay(0.05)
                .easing(Easing::InCubic)
                .distance(0.7),
        ),
        motion_blur: Some(MotionBlur {
            enabled: true,
            shutter_angle: 180.0,
            shutter_angle_keyframes: Vec::new(),
            samples: 8,
        }),
        glows: vec![TextGlow {
            enabled: true,
            color: [132, 96, 255, 170],
            radius: 20.0,
            radius_keyframes: Vec::new(),
            intensity: 0.35,
            intensity_keyframes: Vec::new(),
        }],
        ..LayerFx::default()
    };
    card_fx.loops.push(TitleLoop::Pulse {
        rate_hz: 0.55,
        min_opacity: 0.94,
    });
    let card_id = title.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle {
                corner_radius: 24.0,
            },
            fill: Some([18, 17, 29, 244]),
            stroke: Some(ShapeStroke {
                color: [115, 92, 190, 180],
                width: 2.0,
            }),
            fit: None,
        }),
        card_transform,
    );
    if let Some(card) = title.object_mut(card_id) {
        card.fx = Some(card_fx);
    }

    let mut command = TextBox {
        text: "$ find invoice.pdf".into(),
        font_size: 42.0,
        font_name: "JetBrains Mono".into(),
        color: [235, 231, 255, 255],
        alignment: TextAlignment::Left,
        ..TextBox::default()
    };
    command.write_on = 1.0;
    let text_transform = ObjectTransform {
        center: TitlePosition { x: 0.5, y: 0.49 },
        center_x_keyframes: Vec::new(),
        center_y_keyframes: Vec::new(),
        size: TitlePosition { x: 0.34, y: 0.08 },
        rotation: 0.0,
        anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
    };
    let text_fx = LayerFx {
        motion: TitleMotion::both(
            Motion::new(MotionKind::Typewriter)
                .duration(0.34)
                .easing(Easing::Linear),
            Motion::new(MotionKind::Fade)
                .duration(0.12)
                .easing(Easing::InCubic),
        ),
        motion_blur: Some(MotionBlur {
            enabled: true,
            shutter_angle: 140.0,
            shutter_angle_keyframes: Vec::new(),
            samples: 6,
        }),
        glows: vec![TextGlow {
            enabled: true,
            color: [160, 132, 255, 150],
            radius: 9.0,
            radius_keyframes: Vec::new(),
            intensity: 0.4,
            intensity_keyframes: Vec::new(),
        }],
        ..LayerFx::default()
    };
    let command_id = title.add_object(TitleObjectContent::TextBox(command), text_transform);
    if let Some(command_object) = title.object_mut(command_id) {
        command_object.fx = Some(text_fx);
    }

    let cursor_transform = ObjectTransform {
        center: TitlePosition { x: 0.63, y: 0.49 },
        center_x_keyframes: Vec::new(),
        center_y_keyframes: Vec::new(),
        size: TitlePosition { x: 0.008, y: 0.065 },
        rotation: 0.0,
        anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
    };
    let cursor_fx = LayerFx {
        loops: vec![TitleLoop::Blink {
            rate_hz: 2.2,
            duty: 0.5,
        }],
        ..LayerFx::default()
    };
    let cursor_id = title.add_object(
        TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 2.0 },
            fill: Some([184, 157, 255, 255]),
            stroke: None,
            fit: None,
        }),
        cursor_transform,
    );
    if let Some(cursor) = title.object_mut(cursor_id) {
        cursor.fx = Some(cursor_fx);
    }
    title
}

/// Metadata for the bundled terminal card. The composition itself is created
/// in the project that installs the template; these bindings remain stable
/// because `terminal_card_composition` allocates objects in a fixed order.
pub fn terminal_card_template(composition_id: Uuid) -> CompositionTemplate {
    let mut template = CompositionTemplate::new("Terminal Card", composition_id);
    template.category = "Launch cards".into();
    template.parameters = BTreeMap::from([
        (
            "command_text".into(),
            TemplateParameter {
                label: "Command text".into(),
                default: TemplateValue::Text("$ find invoice.pdf".into()),
                range: None,
                choices: Vec::new(),
            },
        ),
        (
            "accent".into(),
            TemplateParameter {
                label: "Accent color".into(),
            default: TemplateValue::Color([160.0 / 255.0, 132.0 / 255.0, 1.0, 1.0]),
                range: None,
                choices: Vec::new(),
            },
        ),
        (
            "typing_speed".into(),
            TemplateParameter {
                label: "Typing speed".into(),
                default: TemplateValue::Number(0.34),
                range: Some(NumberRange {
                    min: Some(0.05),
                    max: Some(2.0),
                }),
                choices: Vec::new(),
            },
        ),
        (
            "entry_direction".into(),
            TemplateParameter {
                label: "Entry direction".into(),
                default: TemplateValue::Enum("right".into()),
                range: None,
                choices: vec!["left".into(), "right".into()],
            },
        ),
        (
            "float_amount".into(),
            TemplateParameter {
                label: "Float amount".into(),
                default: TemplateValue::Number(1.0),
                range: Some(NumberRange {
                    min: Some(0.0),
                    max: Some(4.0),
                }),
                choices: Vec::new(),
            },
        ),
    ]);
    template.bindings = vec![
        TemplateBinding {
            parameter: "command_text".into(),
            property: "object:1.text".into(),
        },
        TemplateBinding {
            parameter: "accent".into(),
            property: "object:0.fill_and_glow".into(),
        },
        TemplateBinding {
            parameter: "typing_speed".into(),
            property: "object:1.motion.enter.duration_secs".into(),
        },
        TemplateBinding {
            parameter: "entry_direction".into(),
            property: "object:0.motion.enter.direction".into(),
        },
        TemplateBinding {
            parameter: "float_amount".into(),
            property: "object:0.float.keyframes".into(),
        },
    ];
    template
}

fn key(time_secs: f64, value: f32, easing: Easing) -> klipp_core::Keyframe {
    klipp_core::Keyframe {
        time_secs,
        value,
        interpolation: match easing {
            Easing::Linear => klipp_core::Interpolation::Linear,
            Easing::InOutCubic | Easing::InCubic | Easing::OutCubic => {
                klipp_core::Interpolation::Bezier
            }
            _ => klipp_core::Interpolation::Linear,
        },
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
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

    #[test]
    fn terminal_card_has_panel_command_cursor_and_motion() {
        let title = terminal_card_composition();
        assert_eq!(title.objects.len(), 3);
        assert!(matches!(
            title.objects[0].content,
            TitleObjectContent::Shape(_)
        ));
        assert!(matches!(
            title.objects[1].content,
            TitleObjectContent::TextBox(_)
        ));
        assert!(matches!(
            title.objects[2].content,
            TitleObjectContent::Shape(_)
        ));
        assert!(title.objects[0].transform.center_x_keyframes.len() >= 4);
        assert!(title.objects[0].fx().motion.is_active());
        assert!(title.objects[1].fx().motion.enter.is_some());
        assert!(title.objects[2]
            .fx()
            .loops
            .iter()
            .any(|loop_| matches!(loop_, TitleLoop::Blink { .. })));
    }
}
