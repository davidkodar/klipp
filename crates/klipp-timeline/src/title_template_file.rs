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
        Ok(())
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
        let design = |anchor: [f32; 2], fields: &[&str], accent: Option<usize>, preview: &str| TitleDesign { anchor, grow: true, fields: fields.iter().map(|f| f.to_string()).collect(), accent, preview: preview.into() };
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
}
