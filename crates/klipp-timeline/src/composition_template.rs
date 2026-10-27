//! Composition templates (#225, #2757): a composition (a
//! precomposition and any sequences it nests) with typed parameters bound
//! to its properties, saved to start from in any project. The renderer
//! needs no template path of its own: an instance's values are put on the
//! ordinary composition when the timeline is flattened.
//!
//! The file carries its composition (format 2), so a template inserts
//! into any project: the sequences it needs are copied in when the
//! project lacks them. A file of format 1 only named its composition and
//! is refused, as is one of a newer format than this build writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use klipp_core::TemplateValue;

use crate::Sequence;

/// File extension for composition templates.
pub const EXTENSION: &str = "klipptemplate";
/// The format this build writes and reads: 2 carries the composition.
pub const FORMAT_VERSION: u32 = 2;

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
            // NaN and infinity have no JSON form: serde writes `null`,
            // and the saved template would never load again.
            if !number.is_finite() {
                return Err(format!("value for '{}' is not a number", self.label));
            }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The composition and every sequence it nests (format 2), so the
    /// template inserts into any project.
    #[serde(default)]
    pub sequences: Vec<Sequence>,
}

impl CompositionTemplate {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version > FORMAT_VERSION {
            return Err(format!("written by a newer Klipp (format {}, this one reads {FORMAT_VERSION})", self.format_version));
        }
        if self.format_version < FORMAT_VERSION {
            return Err(format!("format {} only named its composition: open the project it was made in and save it again", self.format_version));
        }
        if self.composition().is_none() {
            return Err("the template does not carry its composition".into());
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
            sequences: Vec::new(),
        }
    }
}


impl CompositionTemplate {
    /// The composition's sequence the template carries.
    pub fn composition(&self) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id.0 == self.composition_id)
    }
}

/// `root` and every sequence it nests, compound or precomposition, at any
/// depth, found among `all`: what a template made of `root` carries.
pub fn composition_sequences(root: &Sequence, all: &[Sequence]) -> Vec<Sequence> {
    let mut out: Vec<Sequence> = vec![root.clone()];
    let mut i = 0;
    while i < out.len() {
        let nested: Vec<klipp_core::SequenceId> = out[i]
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter_map(|c| match &c.kind {
                crate::ClipKind::Sequence { sequence_id } | crate::ClipKind::Precomposition { sequence_id } | crate::ClipKind::TemplatePrecomposition { sequence_id, .. } => Some(*sequence_id),
                _ => None,
            })
            .collect();
        for id in nested {
            if !out.iter().any(|s| s.id == id) {
                if let Some(s) = all.iter().find(|s| s.id == id) {
                    out.push(s.clone());
                }
            }
        }
        i += 1;
    }
    out
}

/// The sequences of `template` that `project` lacks, by id, in the order
/// the template carries them: what inserting it copies in.
pub fn missing_sequences(template: &CompositionTemplate, project: &[Sequence]) -> Vec<Sequence> {
    template.sequences.iter().filter(|s| !project.iter().any(|p| p.id == s.id)).cloned().collect()
}

#[derive(Debug, Default)]
pub struct CompositionTemplateStore {
    dir: Option<PathBuf>,
    entries: Vec<(PathBuf, CompositionTemplate)>,
}

impl CompositionTemplateStore {
    /// The template folder under `config` (the config directory).
    pub fn default_dir(config: &Path) -> PathBuf {
        config.join("klipp").join("templates")
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
                    Ok(template) if template.validate().is_ok() => {
                        // One id is one template: a second file carrying
                        // it is a copy, and saving would rewrite the
                        // first while dropping both from the list.
                        if entries
                            .iter()
                            .any(|(_, existing): &(PathBuf, CompositionTemplate)| {
                                existing.id == template.id
                            })
                        {
                            tracing::warn!("Skipping template '{}': its id is already loaded from another file", path.display());
                        } else {
                            entries.push((path, template));
                        }
                    }
                    Ok(template) => tracing::warn!(
                        "Skipping invalid template '{}': {}",
                        path.display(),
                        template.validate().unwrap_err()
                    ),
                    Err(error) => tracing::warn!("Skipping template '{}': {error}", path.display()),
                }
            }
        }
        entries.sort_by_key(|(path, template)| (template.name.to_lowercase(), path.clone()));
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
            .unwrap_or_else(|| {
                // A new template named like another gets a file of its
                // own rather than the other's.
                let stem = safe_stem(&template.name);
                let taken = |p: &PathBuf| self.entries.iter().any(|(existing, _)| existing == p);
                let mut candidate = dir.join(format!("{stem}.{EXTENSION}"));
                let mut n = 2;
                while taken(&candidate) || candidate.exists() {
                    candidate = dir.join(format!("{stem}-{n}.{EXTENSION}"));
                    n += 1;
                }
                candidate
            });
        let json = serde_json::to_string_pretty(&template).map_err(|error| error.to_string())?;
        std::fs::write(&path, json).map_err(|error| error.to_string())?;
        self.entries
            .retain(|(_, existing)| existing.id != template.id);
        self.entries.push((path, template));
        self.entries
            .sort_by_key(|(path, item)| (item.name.to_lowercase(), path.clone()));
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
    use crate::{Clip, ClipKind};
    use klipp_core::RationalTime;

    fn tc(s: f64) -> klipp_core::Timecode {
        RationalTime::from_secs_f64(s, 24)
    }

    /// A composition nesting a precomposition that nests a compound: the
    /// three, and nothing else in the project.
    fn nested() -> (Sequence, Vec<Sequence>) {
        let inner = Sequence::new_default();
        let mut middle = Sequence::new_default();
        middle.tracks[0].clips.push(Clip { kind: ClipKind::Sequence { sequence_id: inner.id }, ..Clip::gap(tc(0.0), tc(2.0)) });
        let mut root = Sequence::new_default();
        root.tracks[0].clips.push(Clip::precomposition(middle.id, tc(0.0), tc(2.0)));
        let unrelated = Sequence::new_default();
        let all = vec![unrelated, inner, middle, root.clone()];
        (root, all)
    }

    /// #2757: a template carries its composition and every sequence it
    /// nests, at any depth; inserting it copies in those a project lacks.
    #[test]
    fn a_template_carries_what_its_composition_nests() {
        let (root, all) = nested();
        let carried = composition_sequences(&root, &all);
        assert_eq!(carried.len(), 3, "the composition, its precomposition and the compound inside that");
        assert_eq!(carried[0].id, root.id);
        assert!(!carried.iter().any(|s| s.id == all[0].id), "nothing it does not nest");
        let mut t = CompositionTemplate::new("Card", root.id.0);
        t.sequences = carried;
        t.validate().unwrap();
        assert_eq!(missing_sequences(&t, &all).len(), 0, "the project it came from has them all");
        assert_eq!(missing_sequences(&t, &[Sequence::new_default()]).len(), 3, "another project has none of them");
    }

    /// A template of format 1 only named its composition, and one of a
    /// newer format was written by a newer Klipp: both refused, as is one
    /// that does not carry its composition.
    #[test]
    fn a_template_without_its_composition_or_of_another_format_is_refused() {
        let (root, all) = nested();
        let mut t = CompositionTemplate::new("Card", root.id.0);
        assert!(t.validate().unwrap_err().contains("carry its composition"));
        t.sequences = composition_sequences(&root, &all);
        t.format_version = 1;
        assert!(t.validate().unwrap_err().contains("save it again"));
        t.format_version = FORMAT_VERSION + 1;
        assert!(t.validate().unwrap_err().contains("newer Klipp"));
    }

    /// Saved, a template reads back carrying its sequences.
    #[test]
    fn a_saved_template_reads_back_with_its_composition() {
        let dir = std::env::temp_dir().join(format!("klipp-composition-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, all) = nested();
        let mut t = CompositionTemplate::new("Card", root.id.0);
        t.sequences = composition_sequences(&root, &all);
        let mut store = CompositionTemplateStore::load(Some(dir.clone()));
        store.save(t.clone()).unwrap();
        let read = CompositionTemplateStore::load(Some(dir.clone()));
        let back = read.templates().next().expect("read back");
        assert_eq!(serde_json::to_value(back).unwrap(), serde_json::to_value(&t).unwrap());
        assert_eq!(back.composition().map(|s| s.id), Some(root.id));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
