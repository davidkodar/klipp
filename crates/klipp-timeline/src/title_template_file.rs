//! A saved title template as its file holds it (#1139): what the Titles
//! browser writes, one `.klipptitle` file each in the user's template
//! folder, read here too by what can't reach the browser's store (the
//! control socket and the assistant, through it).

use crate::TitleData;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File extension of a saved title template.
pub const TEMPLATE_EXTENSION: &str = "klipptitle";

/// The template file's format (#2757): a file of a newer format was
/// written by a newer Klipp and is left out, not guessed at. A file
/// without one is format 1. What a save writes.
pub const TEMPLATE_FORMAT: u32 = 1;

/// The newest format this Klipp reads: 2 adds a [`TitleDesign`] (#2886).
pub const TEMPLATE_FORMAT_READ: u32 = 2;

/// A file larger than this is not read as a title template.
pub const MAX_TEMPLATE_BYTES: u64 = 16 << 20;

/// The most fields a design names, and the longest name.
pub const MAX_DESIGN_FIELDS: usize = 16;
pub const MAX_FIELD_CHARS: usize = 64;

/// The most layers a designed title has, and the longest preview text.
pub const MAX_DESIGN_LAYERS: usize = 256;
pub const MAX_PREVIEW_CHARS: usize = 128;

/// The most variants a design has, options a variant has, and edits one
/// option or optional field makes.
pub const MAX_VARIANTS: usize = 8;
pub const MAX_OPTIONS: usize = 8;
pub const MAX_EDITS: usize = 64;

fn format_one() -> u32 {
    1
}

/// One saved title template: its name, its category, the title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateFile {
    pub name: String,
    pub category: String,
    pub data: TitleData,
    #[serde(default = "format_one")]
    pub format: u32,
    /// How `data` is laid out and published (#2886): with one, `data` is
    /// the layers as designed on a wide frame, and Klipp makes the title
    /// from them as it makes a built-in (`title_library::TitleTemplate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<TitleDesign>,
}

/// A designed title's layout and fields (#2886): the point of the frame
/// it grows from and is placed by on other frames, whether a centred one
/// grows into the room a tall or square frame has, the names of its
/// texts in the order they stack, the layer whose colour is its accent,
/// and the text a browser card shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleDesign {
    pub anchor: [f32; 2],
    #[serde(default = "yes")]
    pub grow: bool,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<usize>,
    #[serde(default)]
    pub preview: String,
    /// Fields that may be left empty (#2886), each by its name in
    /// `fields`: left empty, its edits are made and its layer goes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<OptionalField>,
    /// Named choices (#2886), each option a set of edits; the first
    /// option is the one taken when none is chosen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
    /// The layer the title is placed by (#2886): put at a point, or
    /// inside a box, it goes there and the others keep their distance
    /// from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_by: Option<usize>,
}

/// A field of a design that may be left empty, and the edits made when
/// it is (#2886).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionalField {
    pub field: String,
    #[serde(default)]
    pub edits: Vec<DesignEdit>,
}

/// A named choice of a design and its options (#2886).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub name: String,
    pub options: Vec<VariantOption>,
}

/// One option of a [`Variant`]: its name and the edits it makes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantOption {
    pub name: String,
    #[serde(default)]
    pub edits: Vec<DesignEdit>,
}

/// One edit of a designed title (#2886): the value at `pointer`, a JSON
/// pointer into the title, replaced by `value`. It reaches the layers
/// (`/objects/...`) and the title's background and readability setting
/// (`/background`, `/protect`), never anything else of the title or the
/// file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignEdit {
    pub pointer: String,
    pub value: serde_json::Value,
}

/// Whether an edit may reach `pointer`.
fn editable(pointer: &str) -> bool {
    pointer.starts_with("/objects/") || pointer == "/background" || pointer.starts_with("/background/") || pointer == "/protect"
}

/// `edits` made on `json`, a title as JSON: each pointer one an edit may
/// reach and there already.
fn edit(json: &mut serde_json::Value, edits: &[DesignEdit]) -> Result<(), String> {
    for e in edits {
        if !editable(&e.pointer) {
            return Err(format!("an edit reaches {}, outside the layers, background and readability", e.pointer));
        }
        *json.pointer_mut(&e.pointer).ok_or_else(|| format!("an edit's {} is not in the title", e.pointer))? = e.value.clone();
    }
    Ok(())
}

fn short_name(n: &str) -> bool {
    !n.trim().is_empty() && n.chars().count() <= MAX_FIELD_CHARS
}

fn yes() -> bool {
    true
}

impl TitleDesign {
    /// Whether the design fits `data`: its anchor in the frame, a name
    /// for no more texts than `data` has, each short and not blank, its
    /// accent one of the layers, and no more layers than a title has.
    pub fn check(&self, data: &TitleData) -> Result<(), String> {
        if !self.anchor.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err(format!("its anchor {:?} is not a point of the frame", self.anchor));
        }
        if data.objects.len() > MAX_DESIGN_LAYERS {
            return Err(format!("it has {} layers, more than {MAX_DESIGN_LAYERS}", data.objects.len()));
        }
        let texts = data.objects.iter().filter(|o| matches!(o.content, crate::TitleObjectContent::TextBox(_))).count();
        if self.fields.len() > MAX_DESIGN_FIELDS || self.fields.len() > texts {
            return Err(format!("it names {} fields for {texts} texts", self.fields.len()));
        }
        if let Some(name) = self.fields.iter().find(|n| n.trim().is_empty() || n.chars().count() > MAX_FIELD_CHARS) {
            return Err(format!("a field's name \"{}\" is blank or longer than {MAX_FIELD_CHARS}", name.chars().take(MAX_FIELD_CHARS).collect::<String>()));
        }
        if let Some(a) = self.accent.filter(|a| *a >= data.objects.len()) {
            return Err(format!("its accent is layer {a} of {}", data.objects.len()));
        }
        if self.preview.chars().count() > MAX_PREVIEW_CHARS {
            return Err(format!("its preview is longer than {MAX_PREVIEW_CHARS}"));
        }
        if let Some(k) = self.place_by.filter(|k| *k >= data.objects.len()) {
            return Err(format!("it is placed by layer {k} of {}", data.objects.len()));
        }
        if self.variants.len() > MAX_VARIANTS || self.optional.len() > MAX_DESIGN_FIELDS {
            return Err(format!("it has {} variants and {} optional fields, more than {MAX_VARIANTS} and {MAX_DESIGN_FIELDS}", self.variants.len(), self.optional.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for v in &self.variants {
            if !short_name(&v.name) || !seen.insert(v.name.to_lowercase()) {
                return Err(format!("a variant's name \"{}\" is blank, long or twice", v.name.chars().take(MAX_FIELD_CHARS).collect::<String>()));
            }
            if v.options.is_empty() || v.options.len() > MAX_OPTIONS {
                return Err(format!("variant \"{}\" has {} options, from 1 to {MAX_OPTIONS}", v.name, v.options.len()));
            }
            let mut named = std::collections::BTreeSet::new();
            if let Some(o) = v.options.iter().find(|o| !short_name(&o.name) || !named.insert(o.name.to_lowercase())) {
                return Err(format!("variant \"{}\" has an option \"{}\" blank, long or twice", v.name, o.name.chars().take(MAX_FIELD_CHARS).collect::<String>()));
            }
        }
        for o in &self.optional {
            if !self.fields.contains(&o.field) || self.optional.iter().filter(|p| p.field == o.field).count() > 1 {
                return Err(format!("optional \"{}\" is not one of its fields, or is named twice", o.field.chars().take(MAX_FIELD_CHARS).collect::<String>()));
            }
        }
        // Each option's and each optional field's edits, alone on the
        // design, leave a title this design fits.
        let edits = self.variants.iter().flat_map(|v| v.options.iter().map(|o| &o.edits)).chain(self.optional.iter().map(|o| &o.edits));
        for e in edits {
            if e.len() > MAX_EDITS {
                return Err(format!("{} edits in one option, more than {MAX_EDITS}", e.len()));
            }
            let mut json = serde_json::to_value(data).map_err(|e| e.to_string())?;
            edit(&mut json, e)?;
            let edited: TitleData = serde_json::from_value(json).map_err(|e| format!("an edit leaves no title: {e}"))?;
            if edited.objects.len() != data.objects.len() {
                return Err("an edit adds or takes layers".into());
            }
        }
        Ok(())
    }

    /// The title `data` made as this design says (#2886): first each
    /// variant's option, in the order the variants are listed (the one
    /// `choose` names by variant, else its first); then each field's text
    /// from `texts` by its name, a field left empty or out that is
    /// optional getting its edits, in the order the optional fields are
    /// listed; last, those fields' layers taken out. Not laid out for a
    /// frame, placed or published. An unknown variant or option is
    /// refused with what there is.
    pub fn make(&self, data: &TitleData, choose: &[(&str, &str)], texts: &[(&str, &str)]) -> Result<TitleData, String> {
        let mut json = serde_json::to_value(data).map_err(|e| e.to_string())?;
        for (name, _) in choose {
            if !self.variants.iter().any(|v| v.name.eq_ignore_ascii_case(name)) {
                return Err(format!("there is no \"{name}\": the choices are {}", self.variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", ")));
            }
        }
        for v in &self.variants {
            let option = match choose.iter().find(|(n, _)| v.name.eq_ignore_ascii_case(n)) {
                Some((_, want)) => v.options.iter().find(|o| o.name.eq_ignore_ascii_case(want.trim())).ok_or_else(|| format!("\"{want}\" is not a {}: use {}", v.name, v.options.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ")))?,
                None => &v.options[0],
            };
            edit(&mut json, &option.edits)?;
        }
        let mut td: TitleData = serde_json::from_value(json).map_err(|e| format!("the choices leave no title: {e}"))?;
        let layer_of: Vec<usize> = td.objects.iter().enumerate().filter(|(_, o)| matches!(o.content, crate::TitleObjectContent::TextBox(_))).map(|(i, _)| i).collect();
        let mut gone = Vec::new();
        for (field, layer) in self.fields.iter().zip(&layer_of) {
            let text = texts.iter().find(|(n, _)| n.eq_ignore_ascii_case(field)).map(|(_, t)| t.trim()).filter(|t| !t.is_empty());
            match text {
                Some(t) => {
                    if let crate::TitleObjectContent::TextBox(tb) = &mut td.objects[*layer].content {
                        tb.text = t.to_string();
                    }
                }
                None => {
                    if self.optional.iter().any(|o| &o.field == field) {
                        gone.push(*layer);
                    }
                }
            }
        }
        // The optional fields' edits in their listed order, on the layers
        // as they stand, before any goes.
        let empty: Vec<&OptionalField> = self.optional.iter().filter(|o| self.fields.iter().zip(&layer_of).any(|(f, l)| f == &o.field && gone.contains(l))).collect();
        if !empty.is_empty() {
            let mut json = serde_json::to_value(&td).map_err(|e| e.to_string())?;
            for o in empty {
                edit(&mut json, &o.edits)?;
            }
            td = serde_json::from_value(json).map_err(|e| format!("the empty fields leave no title: {e}"))?;
        }
        gone.sort_unstable();
        for layer in gone.into_iter().rev() {
            td.objects.remove(layer);
        }
        Ok(td)
    }
}

impl TemplateFile {
    /// The template in `json`, checked (#2886): a format this Klipp
    /// reads, a design that fits its layers, and a design only in a
    /// format that has one. Its title as a saved one keeps it.
    pub fn parse(json: &str) -> Result<TemplateFile, String> {
        let mut t: TemplateFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if t.format == 0 || t.format > TEMPLATE_FORMAT_READ {
            return Err(format!("written by a newer Klipp (format {}, this one reads {TEMPLATE_FORMAT_READ})", t.format));
        }
        t.data.migrate_legacy_animation();
        if let Some(design) = &t.design {
            if t.format < 2 {
                return Err(format!("a design needs format 2, it says {}", t.format));
            }
            design.check(&t.data)?;
        }
        Ok(t)
    }

    /// The template as the file at `path` holds it, checked; a file
    /// larger than [`MAX_TEMPLATE_BYTES`] is not read.
    pub fn read(path: &Path) -> Result<TemplateFile, String> {
        let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size > MAX_TEMPLATE_BYTES {
            return Err(format!("{size} bytes, more than a title template has"));
        }
        Self::parse(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
    }

    /// The template with its title made (#2886): a designed one laid out
    /// for a wide frame and published as a built-in is, a plain format-1
    /// file from then on; any other as it is.
    pub fn made(mut self) -> TemplateFile {
        if let Some(design) = self.design.take() {
            let mut td = crate::title_library::TitleTemplate::designed(&self.name, &self.category, self.data, design).data();
            td.template = None;
            self.data = td;
            self.format = TEMPLATE_FORMAT;
        }
        self
    }
}

/// The user's template folder under `config` (the config directory).
pub fn template_dir(config: &Path) -> PathBuf {
    config.join("klipp").join("title_templates")
}

/// Every template in `dir`, by name; a file that doesn't read is left out.
pub fn read_templates(dir: &Path) -> Vec<TemplateFile> {
    let mut out: Vec<TemplateFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(TEMPLATE_EXTENSION))
        .filter_map(|p| TemplateFile::read(&p).ok().map(TemplateFile::made))
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// Every saved title template from the user's own and the installed
/// packs under `root` (#2757), each with its source: the user's first,
/// then each pack's. The built-ins are Klipp's code (`title_library`).
pub fn read_all_templates(root: &Path) -> Vec<(crate::template_places::Source, TemplateFile)> {
    use crate::template_places::{installed, mine_titles, Source};
    let mut out: Vec<(Source, TemplateFile)> = read_templates(&mine_titles(root)).into_iter().map(|t| (Source::Mine, t)).collect();
    for (pack, folder) in installed(root) {
        out.extend(read_templates(&folder.join("titles")).into_iter().map(|t| (Source::Pack(pack.clone()), t)));
    }
    out
}

/// The title template called `name` among `all`, from `source` when one
/// is given, else the user's own before a pack's (#2757).
pub fn find_template<'a>(all: &'a [(crate::template_places::Source, TemplateFile)], name: &str, source: Option<&crate::template_places::Source>) -> Option<&'a TemplateFile> {
    all.iter()
        .filter(|(s, t)| t.name.eq_ignore_ascii_case(name.trim()) && source.is_none_or(|want| s == want))
        .min_by_key(|(s, _)| s.rank())
        .map(|(_, t)| t)
}

/// `data` saved as the template `name` in `dir` (#2757), the file the
/// Titles browser writes: a template of that name is replaced when
/// `replace`, else refused. Returns the file.
pub fn save_template(dir: &Path, name: &str, category: &str, data: &TitleData, replace: bool) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a template needs a name".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let named = |p: &Path| -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        v.get("name")?.as_str().map(str::to_string)
    };
    let files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|e| e.to_str()) == Some(TEMPLATE_EXTENSION)).collect();
    let path = match files.iter().find(|p| named(p).as_deref() == Some(name)) {
        Some(_) if !replace => return Err(format!("a template called \"{name}\" is saved already")),
        Some(p) => p.clone(),
        None => {
            let stem: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            (1..).map(|n| if n == 1 { dir.join(format!("{stem}.{TEMPLATE_EXTENSION}")) } else { dir.join(format!("{stem}-{n}.{TEMPLATE_EXTENSION}")) }).find(|p| !p.exists()).expect("a free file name")
        }
    };
    let json = serde_json::json!({
        "name": name,
        "category": category,
        "written_by": env!("CARGO_PKG_VERSION"),
        "format": TEMPLATE_FORMAT,
        "data": data,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The title of `clip_secs` saved as a template keeps (#2757): the fields
/// named, by label in any case, from its defaults and the optional ones
/// (fonts, pictures, Duration); none named, its defaults and Duration.
/// An unknown label is refused with the ones there are.
pub fn template_for_save(data: &TitleData, clip_secs: f32, fields: Option<&[String]>) -> Result<TitleData, String> {
    let mut all = data.default_fields();
    all.extend(data.optional_fields(clip_secs));
    let chosen = match fields {
        None => all.iter().filter(|f| !matches!(f.kind, crate::FieldKind::Font { .. } | crate::FieldKind::Picture { .. })).cloned().collect(),
        Some(names) => {
            let mut out = Vec::new();
            for n in names {
                let f = all.iter().find(|f| f.label.eq_ignore_ascii_case(n.trim())).ok_or_else(|| {
                    format!("the title has no field \"{n}\": its fields are {}", all.iter().map(|f| f.label.as_str()).collect::<Vec<_>>().join(", "))
                })?;
                out.push(f.clone());
            }
            out
        }
    };
    let mut td = data.clone();
    td.published = chosen;
    td.template = None;
    Ok(td)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TextBox, TitleData};
    use klipp_core::title_motion::{Motion, MotionKind};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-templates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn rising(text: &str) -> TitleData {
        let mut td = TitleData::single_text(TextBox { text: text.into(), ..Default::default() });
        td.objects[0].fx_mut().motion.enter = Some(Motion::new(MotionKind::Rise).duration(0.6));
        td
    }

    /// #2757: a title saved as a template reads back as it was, its
    /// fields those named (defaults and Duration when none are); a name
    /// taken is refused without replace; a newer format is left out.
    #[test]
    fn a_saved_template_reads_back_as_it_was() {
        let dir = scratch("save");
        let td = rising("Opening night");
        let data = template_for_save(&td, 4.0, None).unwrap();
        let labels: Vec<&str> = data.published.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["Text 1", "Text 1 colour", "Speed", "Duration"]);
        save_template(&dir, "Opener", "My Templates", &data, false).unwrap();
        let read = read_templates(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].name.as_str(), read[0].category.as_str(), read[0].format), ("Opener", "My Templates", TEMPLATE_FORMAT));
        assert_eq!(read[0].data.objects, td.objects, "the title as it was");
        assert!(save_template(&dir, "Opener", "x", &data, false).unwrap_err().contains("saved already"));
        save_template(&dir, "Opener", "Openers", &data, true).unwrap();
        assert_eq!(read_templates(&dir)[0].category, "Openers", "replaced in place");
        assert_eq!(read_templates(&dir).len(), 1);

        let picked = template_for_save(&td, 4.0, Some(&["text 1".into(), "Text 1 font".into()])).unwrap();
        assert_eq!(picked.published.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(), ["Text 1", "Text 1 font"]);
        assert!(template_for_save(&td, 4.0, Some(&["Logo".into()])).unwrap_err().contains("its fields are Text 1"));

        std::fs::write(dir.join(format!("future.{TEMPLATE_EXTENSION}")), r#"{"name":"Future","category":"x","format":99,"data":{"objects":[],"next_object_id":0}}"#).unwrap();
        assert!(read_templates(&dir).iter().all(|t| t.name != "Future"), "a newer format is left out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #2886: a designed template file from anywhere is checked before it
    /// is made: an anchor outside the frame, more names than texts, a
    /// blank or long name, too many names, an accent past the layers, a
    /// long preview, too many layers, a design in format 1, a newer
    /// format, a file too large, or not a template at all, is left out;
    /// a good one is made as a built-in is, a plain format-1 template.
    #[test]
    fn a_hostile_designed_template_is_left_out() {
        let dir = scratch("hostile-design");
        std::fs::create_dir_all(&dir).unwrap();
        let mut two = TitleData::single_text(TextBox { text: "[Name]".into(), ..Default::default() });
        two.add_object(crate::TitleObjectContent::TextBox(TextBox { text: "[Role]".into(), ..Default::default() }), crate::default_text_transform());
        let design = |anchor: [f32; 2], fields: &[&str], accent: Option<usize>, preview: &str| TitleDesign { anchor, grow: true, fields: fields.iter().map(|f| f.to_string()).collect(), accent, preview: preview.into(), optional: Vec::new(), variants: Vec::new(), place_by: None };
        let file = |name: &str, data: &TitleData, format: u32, d: Option<TitleDesign>| {
            let json = serde_json::to_string(&TemplateFile { name: name.into(), category: "Packed".into(), data: data.clone(), format, design: d }).unwrap();
            std::fs::write(dir.join(format!("{name}.{TEMPLATE_EXTENSION}")), json).unwrap();
        };
        file("Good", &two, 2, Some(design([0.08, 0.85], &["Name", "Role"], Some(1), "[Name]")));
        file("Outside", &two, 2, Some(design([1.5, 0.5], &["Name"], None, "")));
        file("Below", &two, 2, Some(design([0.5, -0.1], &["Name"], None, "")));
        file("Names", &two, 2, Some(design([0.5, 0.5], &["A", "B", "C"], None, "")));
        file("Blank", &two, 2, Some(design([0.5, 0.5], &[" "], None, "")));
        file("Long", &two, 2, Some(design([0.5, 0.5], &[&"x".repeat(MAX_FIELD_CHARS + 1)], None, "")));
        file("Accent", &two, 2, Some(design([0.5, 0.5], &["Name"], Some(2), "")));
        file("Preview", &two, 2, Some(design([0.5, 0.5], &["Name"], None, &"p".repeat(MAX_PREVIEW_CHARS + 1))));
        file("Old", &two, 1, Some(design([0.5, 0.5], &["Name"], None, "")));
        file("Newer", &two, TEMPLATE_FORMAT_READ + 1, Some(design([0.5, 0.5], &["Name"], None, "")));
        let mut crowd = TitleData::single_text(TextBox::default());
        for _ in 0..MAX_DESIGN_LAYERS {
            crowd.add_object(crate::TitleObjectContent::TextBox(TextBox::default()), crate::default_text_transform());
        }
        file("Crowd", &crowd, 2, Some(design([0.5, 0.5], &["Name"], None, "")));
        let mut many = TitleData::single_text(TextBox::default());
        for _ in 0..MAX_DESIGN_FIELDS {
            many.add_object(crate::TitleObjectContent::TextBox(TextBox::default()), crate::default_text_transform());
        }
        let seventeen: Vec<String> = (0..=MAX_DESIGN_FIELDS).map(|i| format!("F{i}")).collect();
        file("Many", &many, 2, Some(design([0.5, 0.5], &seventeen.iter().map(String::as_str).collect::<Vec<_>>(), None, "")));
        std::fs::write(dir.join(format!("Huge.{TEMPLATE_EXTENSION}")), vec![b' '; MAX_TEMPLATE_BYTES as usize + 1]).unwrap();
        std::fs::write(dir.join(format!("Junk.{TEMPLATE_EXTENSION}")), "{\"name\": 3").unwrap();

        let read = read_templates(&dir);
        assert_eq!(read.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Good"], "only the good one");
        let good = &read[0];
        assert_eq!((good.format, good.design.is_none(), good.data.template.is_none()), (TEMPLATE_FORMAT, true, true), "made, a plain template");
        let labels: Vec<&str> = good.data.published.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["Name", "Role", "Accent", "Size"], "its fields published");
        assert_eq!(good.data.objects[1].name, "Role");
        for (name, says) in [("Outside", "anchor"), ("Below", "anchor"), ("Names", "3 fields for 2 texts"), ("Blank", "blank"), ("Long", "blank or longer"), ("Accent", "accent is layer 2"), ("Preview", "preview"), ("Old", "needs format 2"), ("Newer", "newer Klipp"), ("Crowd", "layers"), ("Many", "17 fields"), ("Huge", "bytes"), ("Junk", "")] {
            let err = TemplateFile::read(&dir.join(format!("{name}.{TEMPLATE_EXTENSION}"))).expect_err(name);
            assert!(err.contains(says), "{name}: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn three_texts() -> TitleData {
        let mut td = TitleData::single_text(TextBox { text: "[A]".into(), ..Default::default() });
        for t in ["[B]", "[C]"] {
            td.add_object(crate::TitleObjectContent::TextBox(TextBox { text: t.into(), ..Default::default() }), crate::default_text_transform());
        }
        td.protect = crate::ContrastProtect::Auto;
        td
    }

    fn plain_design() -> TitleDesign {
        TitleDesign { anchor: [0.5, 0.5], grow: true, fields: vec!["A".into(), "B".into(), "C".into()], accent: None, preview: String::new(), optional: Vec::new(), variants: Vec::new(), place_by: None }
    }

    fn set(pointer: &str, value: serde_json::Value) -> DesignEdit {
        DesignEdit { pointer: pointer.into(), value }
    }

    fn variant(name: &str, options: Vec<(&str, Vec<DesignEdit>)>) -> Variant {
        Variant { name: name.into(), options: options.into_iter().map(|(n, edits)| VariantOption { name: n.into(), edits }).collect() }
    }

    /// #2886: the order a designed title's edits are made in, as
    /// klipp-format.md says it: the variants in their listed order (a
    /// later one's edit of the same value wins), then the texts, then
    /// each empty optional field's edits in their listed order (after
    /// every variant, so they win), made on the layers as they stand;
    /// last, the empty fields' layers taken out.
    #[test]
    fn a_designs_edits_are_made_in_their_order() {
        let mut d = plain_design();
        d.variants = vec![
            variant("first", vec![("on", vec![set("/objects/0/opacity", 0.5.into())])]),
            variant("second", vec![("on", vec![set("/objects/0/opacity", 0.6.into())]), ("off", Vec::new())]),
        ];
        d.optional = vec![
            OptionalField { field: "B".into(), edits: vec![set("/objects/0/opacity", 0.7.into()), set("/objects/2/opacity", 0.2.into())] },
            OptionalField { field: "C".into(), edits: vec![set("/objects/0/opacity", 0.8.into())] },
        ];
        let data = three_texts();
        d.check(&data).unwrap();
        let text = |td: &TitleData, i: usize| match &td.objects[i].content { crate::TitleObjectContent::TextBox(tb) => tb.text.clone(), _ => unreachable!() };

        // Later variants win; the first option when none is chosen.
        let all = d.make(&data, &[], &[("A", "a"), ("B", "b"), ("C", "c")]).unwrap();
        assert_eq!(all.objects[0].opacity, 0.6);
        assert_eq!((text(&all, 0), text(&all, 1), text(&all, 2)), ("a".into(), "b".into(), "c".into()));
        let off = d.make(&data, &[("SECOND", "Off")], &[("A", "a"), ("B", "b"), ("C", "c")]).unwrap();
        assert_eq!(off.objects[0].opacity, 0.5, "a choice by name, in any case");

        // An empty optional field's edits come after the variants, in
        // their order, on the layers before any goes: B's edit of layer 2
        // lands on C's layer, which is then layer 1.
        let no_b = d.make(&data, &[], &[("A", "a"), ("B", " "), ("C", "c")]).unwrap();
        assert_eq!(no_b.objects.len(), 2);
        assert_eq!((no_b.objects[0].opacity, text(&no_b, 1), no_b.objects[1].opacity), (0.7, "c".into(), 0.2));
        let neither = d.make(&data, &[], &[("A", "a")]).unwrap();
        assert_eq!((neither.objects.len(), neither.objects[0].opacity), (1, 0.8), "C's edit after B's");

        assert!(d.make(&data, &[("third", "on")], &[]).unwrap_err().contains("the choices are first, second"));
        assert!(d.make(&data, &[("second", "sideways")], &[]).unwrap_err().contains("use on, off"));
    }

    /// #2886: a design's variants, optional fields and edits from a file
    /// are checked before anything is made: an edit reaching outside the
    /// layers, background and readability (the id counter, the published
    /// fields, the template's name, the style), or a value not there, or
    /// one that leaves no title; too many edits, variants or options; a
    /// blank, long or twice-given name; an optional field that is not a
    /// field; a placing layer that isn't there. Each is refused.
    #[test]
    fn a_hostile_design_edit_is_refused() {
        let data = three_texts();
        let with = |f: &dyn Fn(&mut TitleDesign)| {
            let mut d = plain_design();
            f(&mut d);
            d.check(&data)
        };
        let one = |e: DesignEdit| move |d: &mut TitleDesign| d.variants = vec![variant("v", vec![("o", vec![e.clone()])])];
        for pointer in ["/next_object_id", "/published", "/template", "/style", "/animation_type", "", "/objects"] {
            let err = with(&one(set(pointer, 3.into()))).unwrap_err();
            assert!(err.contains("outside the layers"), "{pointer}: {err}");
        }
        assert!(with(&one(set("/objects/7/opacity", 0.5.into()))).unwrap_err().contains("not in the title"));
        assert!(with(&one(set("/objects/0/opacity", "lots".into()))).unwrap_err().contains("leaves no title"));
        assert!(with(&one(set("/objects/0", 3.into()))).unwrap_err().contains("leaves no title"));
        assert!(with(&one(set("/protect", "sometimes".into()))).unwrap_err().contains("leaves no title"));
        with(&one(set("/protect", "always".into()))).unwrap();
        with(&one(set("/background", serde_json::json!({ "Color": { "color": [0, 0, 0, 200], "padding": 14.0 } })))).unwrap();
        let many = vec![set("/objects/0/opacity", 0.5.into()); MAX_EDITS + 1];
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", vec![("o", many.clone())])]).unwrap_err().contains("edits in one option"));
        assert!(with(&|d: &mut TitleDesign| d.optional = vec![OptionalField { field: "B".into(), edits: many.clone() }]).unwrap_err().contains("edits in one option"));
        assert!(with(&|d: &mut TitleDesign| d.variants = (0..=MAX_VARIANTS).map(|i| variant(&format!("v{i}"), vec![("o", Vec::new())])).collect()).unwrap_err().contains("variants"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", (0..=MAX_OPTIONS).map(|_| ("o", Vec::new())).collect())]).unwrap_err().contains("options"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", Vec::new())]).unwrap_err().contains("0 options"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant(" ", vec![("o", Vec::new())])]).unwrap_err().contains("blank, long or twice"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", vec![("o", Vec::new())]), variant("V", vec![("o", Vec::new())])]).unwrap_err().contains("blank, long or twice"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", vec![("o", Vec::new()), ("O", Vec::new())])]).unwrap_err().contains("blank, long or twice"));
        assert!(with(&|d: &mut TitleDesign| d.variants = vec![variant("v", vec![(&"o".repeat(MAX_FIELD_CHARS + 1), Vec::new())])]).unwrap_err().contains("blank, long or twice"));
        assert!(with(&|d: &mut TitleDesign| d.optional = vec![OptionalField { field: "D".into(), edits: Vec::new() }]).unwrap_err().contains("not one of its fields"));
        assert!(with(&|d: &mut TitleDesign| d.optional = vec![OptionalField { field: "B".into(), edits: Vec::new() }; 2]).unwrap_err().contains("named twice"));
        assert!(with(&|d: &mut TitleDesign| d.place_by = Some(3)).unwrap_err().contains("placed by layer 3"));

        // Through the reader: a file with such an edit is left out.
        let dir = scratch("hostile-edit");
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = plain_design();
        d.variants = vec![variant("v", vec![("o", vec![set("/template", "user:Someone".into())])])];
        let json = serde_json::to_string(&TemplateFile { name: "Reach".into(), category: "x".into(), data: data.clone(), format: 2, design: Some(d) }).unwrap();
        std::fs::write(dir.join(format!("Reach.{TEMPLATE_EXTENSION}")), json).unwrap();
        assert!(read_templates(&dir).is_empty(), "left out");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
