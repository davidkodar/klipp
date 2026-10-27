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
    /// Its slots (#2757), by name: what the user fills in when the
    /// template is placed or later, each bound like a parameter (a media
    /// slot to `clip:<n>.media`); until filled it shows a placeholder.
    /// The same slots a Composite recipe declares (#2758).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<String, TemplateSlot>,
}

/// One slot of a template or a recipe (#2757, #2758): what the user calls
/// it and what fills it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateSlot {
    pub label: String,
    pub kind: SlotKind,
}

/// What fills a slot. A template fills media slots; the Composite page's
/// recipes add the others (#2758).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// A media item from the pool: a picture or a video.
    Media,
    /// A clip on the timeline.
    Clip,
    /// A title.
    Title,
    /// A subject matte of a clip.
    SubjectMatte,
    /// A colour.
    Colour,
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
            if !self.parameters.contains_key(&binding.parameter) && !self.slots.contains_key(&binding.parameter) {
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
            slots: BTreeMap::new(),
        }
    }
}


impl CompositionTemplate {
    /// The composition's sequence the template carries.
    pub fn composition(&self) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id.0 == self.composition_id)
    }
}

/// What a scene can expose as a template's parameter or slot (#2757): its
/// name in the template, what the user calls it, the parameter (`None`
/// for a media slot) and the property it is bound to.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub label: String,
    pub parameter: Option<TemplateParameter>,
    pub property: String,
}

impl Candidate {
    /// Whether it is offered ticked: a text, and a media slot.
    pub fn by_default(&self) -> bool {
        self.parameter.is_none() || matches!(self.parameter.as_ref().map(|p| &p.default), Some(TemplateValue::Text(_)) if self.property.ends_with(".text"))
    }
}

fn text_param(label: String, default: TemplateValue) -> Option<TemplateParameter> {
    Some(TemplateParameter { label, default, range: None, choices: Vec::new() })
}

/// What `scene` can expose (#2757), in its tracks' order: each title
/// layer's text, colour and font, each image layer's picture, and each
/// media clip as a slot.
pub fn candidates(scene: &Sequence) -> Vec<Candidate> {
    use crate::{ClipKind, TitleObjectContent};
    let mut out = Vec::new();
    let mut object = 0usize;
    let mut texts = 0usize;
    let mut slots = 0usize;
    for (n, clip) in scene.tracks.iter().flat_map(|t| &t.clips).enumerate() {
        match &clip.kind {
            ClipKind::Title(title) => {
                for o in &title.objects {
                    let i = object;
                    object += 1;
                    let named = |fallback: String| if o.name.trim().is_empty() { fallback } else { o.name.trim().to_string() };
                    let colour = |c: [u8; 4]| TemplateValue::Color(c.map(|v| v as f32 / 255.0));
                    match &o.content {
                        TitleObjectContent::TextBox(tb) => {
                            texts += 1;
                            let label = named(format!("Text {texts}"));
                            out.push(Candidate { name: format!("text_{i}"), label: label.clone(), parameter: text_param(label.clone(), TemplateValue::Text(tb.text.clone())), property: format!("object:{i}.text") });
                            out.push(Candidate { name: format!("color_{i}"), label: format!("{label} colour"), parameter: text_param(format!("{label} colour"), colour(tb.color)), property: format!("object:{i}.color") });
                            out.push(Candidate { name: format!("font_{i}"), label: format!("{label} font"), parameter: text_param(format!("{label} font"), TemplateValue::Text(tb.font_name.clone())), property: format!("object:{i}.font") });
                        }
                        TitleObjectContent::Shape(shape) => {
                            if let Some(fill) = shape.fill {
                                let label = named(format!("Shape {}", i + 1));
                                out.push(Candidate { name: format!("color_{i}"), label: format!("{label} colour"), parameter: text_param(format!("{label} colour"), colour(fill)), property: format!("object:{i}.color") });
                            }
                        }
                        TitleObjectContent::Image(img) => {
                            let label = named(img.stem());
                            out.push(Candidate { name: format!("picture_{i}"), label: label.clone(), parameter: text_param(label, TemplateValue::Text(img.path.clone())), property: format!("object:{i}.picture") });
                        }
                    }
                }
            }
            ClipKind::Media { .. } => {
                slots += 1;
                out.push(Candidate { name: format!("slot_{slots}"), label: format!("Media {slots}"), parameter: None, property: format!("clip:{n}.media") });
            }
            _ => {}
        }
    }
    out
}

/// The clips `ids` of `seq` as a scene of their own (#2757): their tracks
/// kept in order, their times from the first one's start, the timeline
/// untouched. `None` when none is there.
pub fn scene_from_clips(seq: &Sequence, ids: &[klipp_core::ClipId]) -> Option<Sequence> {
    let picked: Vec<(usize, &crate::Clip)> = seq.tracks.iter().enumerate().flat_map(|(t, track)| track.clips.iter().filter(|c| ids.contains(&c.id)).map(move |c| (t, c))).collect();
    let start = picked.iter().map(|(_, c)| c.timeline_in).min()?;
    let mut scene = Sequence::new_default();
    scene.name = "Template".into();
    scene.fps = seq.fps;
    scene.resolution = seq.resolution;
    scene.tracks.clear();
    for (t, track) in seq.tracks.iter().enumerate() {
        let clips: Vec<crate::Clip> = picked.iter().filter(|(pt, _)| *pt == t).map(|(_, c)| {
            let mut c = (*c).clone();
            c.timeline_in -= start;
            c.timeline_out -= start;
            c
        }).collect();
        if !clips.is_empty() {
            let mut new = crate::Track::new(&track.name, track.kind);
            new.clips = clips;
            new.sort_clips();
            scene.tracks.push(new);
        }
    }
    Some(scene)
}

/// A template of `scene` called `name` (#2757), carrying it and what it
/// nests among `all`, exposing the candidates labelled in `chosen` (in
/// any case), or those offered ticked when none are named. An unknown
/// label is refused with the ones there are.
pub fn template_from_scene(scene: &Sequence, all: &[Sequence], name: &str, chosen: Option<&[String]>) -> Result<CompositionTemplate, String> {
    let offered = candidates(scene);
    let picked: Vec<&Candidate> = match chosen {
        None => offered.iter().filter(|c| c.by_default()).collect(),
        Some(labels) => labels.iter().map(|l| offered.iter().find(|c| c.label.eq_ignore_ascii_case(l.trim())).ok_or_else(|| {
            format!("the clips have no \"{l}\" to expose: they have {}", offered.iter().map(|c| c.label.as_str()).collect::<Vec<_>>().join(", "))
        })).collect::<Result<_, _>>()?,
    };
    let mut t = CompositionTemplate::new(name.trim(), scene.id.0);
    for c in picked {
        match &c.parameter {
            Some(p) => {
                t.parameters.insert(c.name.clone(), p.clone());
            }
            None => {
                t.slots.insert(c.name.clone(), TemplateSlot { label: c.label.clone(), kind: SlotKind::Media });
            }
        }
        t.bindings.push(TemplateBinding { parameter: c.name.clone(), property: c.property.clone() });
    }
    let mut with_scene: Vec<Sequence> = all.to_vec();
    with_scene.push(scene.clone());
    t.sequences = composition_sequences(scene, &with_scene);
    t.validate()?;
    Ok(t)
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

    /// A scene of a title (a text and a filled shape) and a media clip.
    fn card_and_photo() -> (Sequence, klipp_core::ClipId, klipp_core::ClipId) {
        let mut title = crate::TitleData::single_text(crate::TextBox { text: "Launch".into(), color: [255, 255, 255, 255], ..Default::default() });
        let at = title.objects[0].transform.clone();
        title.add_object(crate::TitleObjectContent::Shape(crate::Shape { kind: crate::ShapeKind::Rectangle { corner_radius: 0.0 }, fit: None, fill: Some([200, 0, 0, 255]), gradient: None, stroke: None }), at);
        let mut seq = Sequence::new_default();
        let text = Clip { kind: ClipKind::Title(title), ..Clip::gap(tc(2.0), tc(5.0)) };
        let photo = Clip::media(klipp_core::MediaId::new(), tc(2.0), tc(5.0), tc(0.0), tc(3.0));
        let (t, p) = (text.id, photo.id);
        seq.tracks[0].clips.push(photo);
        let mut top = crate::Track::new("V2", crate::TrackKind::Video);
        top.clips.push(text);
        seq.tracks.insert(0, top);
        (seq, t, p)
    }

    /// #2757: clips saved together keep their tracks, start at 0 and
    /// leave the timeline as it was; the scene offers each text, colour
    /// and font, the shape's colour and the media clip as a slot, its
    /// texts and slots ticked; an unknown label is refused.
    #[test]
    fn clips_saved_together_offer_their_texts_colours_and_slots() {
        let (seq, t, p) = card_and_photo();
        let before = serde_json::to_value(&seq).unwrap();
        let scene = scene_from_clips(&seq, &[t, p]).expect("a scene");
        assert_eq!(serde_json::to_value(&seq).unwrap(), before, "the timeline untouched");
        assert_eq!(scene.tracks.len(), 2);
        assert!(scene.tracks.iter().flat_map(|t| &t.clips).all(|c| c.timeline_in == tc(0.0)), "from the first one's start");
        let labels: Vec<String> = candidates(&scene).into_iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Text 1", "Text 1 colour", "Text 1 font", "Shape 2 colour", "Media 1"]);
        let t = template_from_scene(&scene, &[], "Card", None).unwrap();
        assert_eq!(t.parameters.values().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["Text 1"], "texts by default");
        assert_eq!(t.slots.values().map(|s| (s.label.as_str(), s.kind)).collect::<Vec<_>>(), [("Media 1", SlotKind::Media)]);
        assert!(template_from_scene(&scene, &[], "Card", Some(&["Logo".into()])).unwrap_err().contains("they have Text 1"));
    }

    /// Placed, an empty slot shows the placeholder and a filled one its
    /// media; a colour and a font set by their parameters land on their
    /// layers.
    #[test]
    fn a_placed_template_fills_its_slots_and_sets_its_properties() {
        let (seq, t, p) = card_and_photo();
        let scene = scene_from_clips(&seq, &[t, p]).unwrap();
        let chosen: Vec<String> = ["Text 1", "Shape 2 colour", "Text 1 font", "Media 1"].map(String::from).to_vec();
        let template = template_from_scene(&scene, &[], "Card", Some(&chosen)).unwrap();
        let place = |overrides: Vec<(&str, klipp_core::TemplateValue)>, media: Option<klipp_core::MediaId>| {
            let instance = klipp_core::TemplateInstance {
                template_id: template.id,
                bindings: template.bindings.iter().map(|b| (b.parameter.clone(), b.property.clone())).collect(),
                overrides: overrides.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                media_slots: media.map(|m| ("slot_1".to_string(), m.0.to_string())).into_iter().collect(),
            };
            let mut clip = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
            clip.kind = ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
            let mut outer = Sequence::new_default();
            outer.tracks[0].clips.push(clip);
            let flat = crate::flatten::flatten_sequence(&outer, &[outer.clone(), scene.clone()]);
            let inner = flat.all_clips().find_map(|c| c.precomp.clone()).expect("the scene");
            (*inner).clone()
        };
        let empty = place(vec![], None);
        assert!(empty.all_clips().any(|c| matches!(c.kind, ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color }) if color == crate::flatten::SLOT_PLACEHOLDER)), "the placeholder where no media was given");
        let media = klipp_core::MediaId::new();
        let filled = place(vec![("color_1", klipp_core::TemplateValue::Color([0.0, 0.0, 1.0, 1.0])), ("font_0", klipp_core::TemplateValue::Text("Anton".into()))], Some(media));
        assert!(filled.all_clips().any(|c| matches!(c.kind, ClipKind::Media { media_id } if media_id == media)), "the slot's media");
        let title = filled.all_clips().find_map(|c| c.title_data().cloned()).unwrap();
        assert!(matches!(&title.objects[0].content, crate::TitleObjectContent::TextBox(tb) if tb.font_name == "Anton"));
        assert!(matches!(&title.objects[1].content, crate::TitleObjectContent::Shape(s) if s.fill == Some([0, 0, 255, 255])));
    }
}
