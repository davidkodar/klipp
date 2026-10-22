//! Project templates (#618): a project's structure saved to start new
//! projects from. A template keeps each sequence's name and format and its
//! tracks' names, kinds, order and settings; the media pool's bins (empty)
//! and smart bins; and the project's colour setup. It keeps nothing else:
//! no media, clips, transitions, markers, playhead, history or caches.
//!
//! A project made from a template is a new project: every sequence,
//! track, bin and smart bin gets an id of its own, so nothing in it leads
//! back to the template or to the project it was saved from.
//!
//! Templates are JSON files in a folder of their own, one a template,
//! each saying its format. One that does not read is reported by name and
//! the others are still listed.
//!
//! Starter content is opt-in (part 2): chosen sequences kept whole, clips
//! and all, as a project inside the template, and the media they use
//! either linked where it is (relinked on another machine) or copied into
//! the template. Such a template is a folder: `template.json`, the
//! `starter` project and the copied `media`. A template is exported as one
//! zip of that folder and imported from one (part 3).

use std::path::{Path, PathBuf};

use klipp_core::{BinId, Fps, GradeEncoding, Resolution, SequenceId, SmartBin, WorkingColorspace};
use klipp_media::{Bin, MediaPool};
use klipp_timeline::{CaptionStyle, FirstClipAutoSet, Sequence, Track, TrackKind, TrackRole};
use serde::{Deserialize, Serialize};

/// The template format this build writes and reads.
pub const TEMPLATE_FORMAT: u32 = 2;

/// The file a template is kept in ends so.
pub const TEMPLATE_EXTENSION: &str = "klipptemplate";

/// A project's structure, kept to start new projects from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectTemplate {
    pub format: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub sequences: Vec<TemplateSequence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bins: Vec<TemplateBin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_bins: Vec<SmartBin>,
    pub colour: TemplateColour,
    /// What a project starts with beyond its structure, if anything
    /// (format 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<StarterContent>,
}

/// A template's starter content: the sequences kept whole (by name, in
/// the template's `starter` project) and what they need from outside
/// the template.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StarterContent {
    pub sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<StarterMedia>,
    /// The font families its titles are set in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<String>,
    /// The plug-ins its clips run (frei0r and OFX), by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
}

/// A media file the starter content uses: where it was, and, when it was
/// copied into the template, its name in the template's `media` folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StarterMedia {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packaged: Option<String>,
}

/// What starter content to keep: the sequences, by id (any sequence a
/// compound clip in one of them opens is kept with it), and whether the
/// media they use is copied into the template.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StarterChoice {
    pub sequences: Vec<SequenceId>,
    pub package_media: bool,
}

/// Something a template needs from outside itself, and whether it is there.
#[derive(Debug, Clone, PartialEq)]
pub enum Dependency {
    /// A media file copied into the template.
    PackagedMedia(String),
    /// A media file linked where it was: there, or not (to relink).
    LinkedMedia { path: PathBuf, present: bool },
    /// The timeline LUT: there, or not.
    Lut { path: PathBuf, present: bool },
    /// A font family its titles use.
    Font(String),
    /// A plug-in its clips run.
    Plugin(String),
}

/// A sequence's name, format and tracks. A size or rate taken from the
/// first clip stays so in the new project; one set stays as set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateSequence {
    pub name: String,
    pub fps_num: u32,
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub first_clip_autoset: FirstClipAutoSet,
    #[serde(default)]
    pub drop_frame: bool,
    pub tracks: Vec<TemplateTrack>,
    /// Kept whole, clips and all, in the starter content (format 2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starter: bool,
}

/// A track's name, kind and settings, without its clips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateTrack {
    pub name: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub magnetic_role: TrackRole,
    pub opacity: f32,
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub visible: bool,
    pub locked: bool,
    pub sync_lock: bool,
    pub targeted: bool,
    pub source_patch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_height: Option<f32>,
    #[serde(default)]
    pub colour_label: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_style: Option<CaptionStyle>,
    /// A subtitle track's language (#2223).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// An empty bin, under the bin at `parent` in the template's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateBin {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
}

/// The project's colour setup: its working space, grade encoding and the
/// look over the whole timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateColour {
    pub working_colorspace: WorkingColorspace,
    pub grade_encoding: GradeEncoding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_lut_path: Option<PathBuf>,
    #[serde(default = "one")]
    pub global_lut_opacity: f32,
    #[serde(default)]
    pub global_lut_enabled: bool,
}

fn one() -> f32 {
    1.0
}

/// The starter content's sequences and media, as saved inside a template.
pub struct StarterFiles {
    pub sequences: Vec<Sequence>,
    pub pool: MediaPool,
    pub package_media: bool,
}

/// The sequence a compound, a precomposition or a template instance
/// opens.
fn nested_sequence(kind: &klipp_timeline::ClipKind) -> Option<SequenceId> {
    use klipp_timeline::ClipKind;
    match kind {
        ClipKind::Sequence { sequence_id } | ClipKind::Precomposition { sequence_id } | ClipKind::TemplatePrecomposition { sequence_id, .. } => Some(*sequence_id),
        _ => None,
    }
}

/// The plug-ins a clip's picture runs: frei0r effects by their plug-in id,
/// OFX ones by their name.
fn clip_plugins(clip: &klipp_timeline::Clip) -> Vec<String> {
    let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
    let mut out = Vec::new();
    let mut nodes: Vec<&klipp_graph_format::Node> = lifted.picture.graph.nodes.iter().collect();
    while let Some(n) = nodes.pop() {
        if let Some(group) = &n.group {
            nodes.extend(group.graph.nodes.iter());
        }
        if n.type_id.as_str() == klipp_graph_format::lifted::types::OFX {
            if let Some(klipp_graph_format::ParamValue::Text(name)) = n.params.get(&klipp_graph_format::ParamKey::from("plugin")) {
                out.push(format!("OFX: {name}"));
            }
        } else if klipp_graph_lift::effect_params::tag(n) == Some("frei0r") {
            if let Ok(klipp_effects::BuiltinEffect::Frei0r { type_id, .. }) = klipp_graph_lift::effect_params::effect_of(n) {
                out.push(format!("frei0r: {type_id}"));
            }
        }
    }
    out
}

/// What a project is set up with, as a template takes it.
pub struct ProjectSetup<'a> {
    pub sequences: &'a [Sequence],
    pub pool: &'a MediaPool,
    pub colour: TemplateColour,
}

/// A project made from a template: its sequences, its pool (bins and
/// smart bins, no media) and its colour setup, every id new.
pub struct FromTemplate {
    pub sequences: Vec<Sequence>,
    pub pool: MediaPool,
    pub colour: TemplateColour,
}

impl ProjectTemplate {
    /// `setup`'s structure as a template called `name`.
    pub fn capture(name: &str, description: &str, setup: &ProjectSetup) -> Self {
        let sequences = setup
            .sequences
            .iter()
            .map(|s| TemplateSequence {
                name: s.name.clone(),
                fps_num: s.fps.num,
                fps_den: s.fps.den,
                width: s.resolution.width,
                height: s.resolution.height,
                first_clip_autoset: s.first_clip_autoset,
                drop_frame: s.drop_frame,
                starter: false,
                tracks: s
                    .tracks
                    .iter()
                    .map(|t| TemplateTrack {
                        name: t.name.clone(),
                        kind: t.kind,
                        magnetic_role: t.magnetic_role.clone(),
                        opacity: t.opacity,
                        volume: t.volume,
                        pan: t.pan,
                        muted: t.muted,
                        solo: t.solo,
                        visible: t.visible,
                        locked: t.locked,
                        sync_lock: t.sync_lock,
                        targeted: t.targeted,
                        source_patch: t.source_patch,
                        custom_height: t.custom_height,
                        colour_label: t.colour_label,
                        caption_style: t.caption_style.clone(),
                        language: t.language.clone(),
                    })
                    .collect(),
            })
            .collect();
        // Bins by their place in the list, parents first wherever they
        // stand in the pool's.
        let pool_bins = &setup.pool.bins;
        let under = |parent: Option<BinId>| (0..pool_bins.len()).filter(move |&i| pool_bins[i].parent_id == parent);
        let mut order: Vec<usize> = under(None).collect();
        let mut k = 0;
        while k < order.len() {
            let id = pool_bins[order[k]].id;
            order.extend(under(Some(id)));
            k += 1;
        }
        let bins = order
            .iter()
            .map(|&i| {
                let b = &pool_bins[i];
                TemplateBin { name: b.name.clone(), parent: b.parent_id.and_then(|p| order.iter().position(|&o| pool_bins[o].id == p)) }
            })
            .collect();
        ProjectTemplate {
            format: TEMPLATE_FORMAT,
            name: name.trim().to_string(),
            description: description.trim().to_string(),
            sequences,
            bins,
            smart_bins: setup.pool.smart_bins.clone(),
            colour: setup.colour.clone(),
            starter: None,
        }
    }

    /// [`Self::capture`] with starter content: the sequences `choice` names
    /// (and any a compound clip in them opens) kept whole, with the media
    /// they use. Saved with [`Self::save_with_starter`]. No starter content
    /// when `choice` names no sequence there.
    pub fn capture_with_starter(name: &str, description: &str, setup: &ProjectSetup, choice: &StarterChoice) -> (Self, Option<StarterFiles>) {
        let mut template = Self::capture(name, description, setup);
        // The chosen sequences and every sequence they open, transitively.
        let mut kept: Vec<SequenceId> = Vec::new();
        let mut queue: Vec<SequenceId> = choice.sequences.clone();
        while let Some(id) = queue.pop() {
            if kept.contains(&id) {
                continue;
            }
            let Some(seq) = setup.sequences.iter().find(|s| s.id == id) else { continue };
            kept.push(id);
            queue.extend(seq.all_clips().filter_map(|c| nested_sequence(&c.kind)));
        }
        if kept.is_empty() {
            return (template, None);
        }
        let sequences: Vec<Sequence> = setup.sequences.iter().filter(|s| kept.contains(&s.id)).cloned().collect();
        for (t, s) in template.sequences.iter_mut().zip(setup.sequences) {
            t.starter = kept.contains(&s.id);
        }
        // The media the kept clips use, out of their bins.
        let mut pool = MediaPool::new();
        for c in sequences.iter().flat_map(|s| s.all_clips()) {
            if let klipp_timeline::ClipKind::Media { media_id } = &c.kind {
                if let Some(item) = setup.pool.get(media_id).filter(|_| pool.get(media_id).is_none()) {
                    let mut item = item.clone();
                    item.bin_id = None;
                    pool.add(item);
                }
            }
        }
        let mut media: Vec<StarterMedia> = pool.all().map(|m| StarterMedia { path: m.path.clone(), packaged: None }).collect();
        media.sort_by(|a, b| a.path.cmp(&b.path));
        let mut fonts: Vec<String> = Vec::new();
        let mut plugins: Vec<String> = Vec::new();
        for c in sequences.iter().flat_map(|s| s.all_clips()) {
            if let klipp_timeline::ClipKind::Title(td) = &c.kind {
                for o in &td.objects {
                    if let klipp_timeline::TitleObjectContent::TextBox(tb) = &o.content {
                        fonts.push(tb.font_name.clone());
                        fonts.extend(tb.style_runs.iter().filter_map(|r| r.font_name.clone()));
                    }
                }
            }
            plugins.extend(clip_plugins(c));
        }
        for v in [&mut fonts, &mut plugins] {
            v.retain(|f| !f.trim().is_empty());
            v.sort();
            v.dedup();
        }
        template.starter = Some(StarterContent { sequences: sequences.iter().map(|s| s.name.clone()).collect(), media, fonts, plugins });
        (template, Some(StarterFiles { sequences, pool, package_media: choice.package_media }))
    }

    /// Why the template can't start a project, if it can't: a format this
    /// build doesn't read, no name, no sequence, a sequence without a
    /// size or rate it can use, or a bin under one that isn't before it.
    pub fn validate(&self) -> Result<(), String> {
        if self.format == 0 || self.format > TEMPLATE_FORMAT {
            return Err(format!("format {} is not one this Klipp reads (it reads up to {TEMPLATE_FORMAT})", self.format));
        }
        if self.name.trim().is_empty() {
            return Err("it has no name".into());
        }
        if self.sequences.is_empty() {
            return Err("it has no sequence".into());
        }
        for s in &self.sequences {
            if !s.first_clip_autoset.fps && (s.fps_num == 0 || s.fps_den == 0) {
                return Err(format!("sequence \"{}\" has no frame rate", s.name));
            }
            if !s.first_clip_autoset.resolution && (s.width == 0 || s.height == 0) {
                return Err(format!("sequence \"{}\" has no size", s.name));
            }
        }
        if let Some((i, _)) = self.bins.iter().enumerate().find(|(i, b)| b.parent.is_some_and(|p| p >= *i)) {
            return Err(format!("bin \"{}\" is under one that isn't before it", self.bins[i].name));
        }
        Ok(())
    }

    /// A new project from the template: each sequence with new tracks,
    /// each bin and smart bin new, the colour setup as kept.
    pub fn instantiate(&self) -> FromTemplate {
        let sequences = self
            .sequences
            .iter()
            .map(|s| {
                let fps = if s.fps_num == 0 || s.fps_den == 0 { Fps::new(24, 1) } else { Fps::new(s.fps_num, s.fps_den) };
                let resolution = if s.width == 0 || s.height == 0 { Resolution { width: 1920, height: 1080 } } else { Resolution { width: s.width, height: s.height } };
                let mut seq = Sequence::new_default();
                seq.id = SequenceId::new();
                seq.name = s.name.clone();
                seq.fps = fps;
                seq.resolution = resolution;
                seq.first_clip_autoset = s.first_clip_autoset;
                seq.drop_frame = s.drop_frame;
                seq.tracks = s
                    .tracks
                    .iter()
                    .map(|t| {
                        let mut track = Track::new(&t.name, t.kind);
                        track.magnetic_role = t.magnetic_role.clone();
                        track.opacity = t.opacity;
                        track.volume = t.volume;
                        track.pan = t.pan;
                        track.muted = t.muted;
                        track.solo = t.solo;
                        track.visible = t.visible;
                        track.locked = t.locked;
                        track.sync_lock = t.sync_lock;
                        track.targeted = t.targeted;
                        track.source_patch = t.source_patch;
                        track.custom_height = t.custom_height;
                        track.colour_label = t.colour_label;
                        track.caption_style = t.caption_style.clone();
                        track.language = t.language.clone();
                        track
                    })
                    .collect();
                seq
            })
            .collect();
        let mut pool = MediaPool::new();
        let ids: Vec<BinId> = self.bins.iter().map(|_| BinId::new()).collect();
        pool.bins = self.bins.iter().enumerate().map(|(i, b)| Bin { id: ids[i], name: b.name.clone(), parent_id: b.parent.and_then(|p| ids.get(p).copied()) }).collect();
        pool.smart_bins = self.smart_bins.iter().map(|s| SmartBin { id: uuid::Uuid::new_v4().to_string(), ..s.clone() }).collect();
        FromTemplate { sequences, pool, colour: self.colour.clone() }
    }

    /// What the template holds, in a line: its sequences, tracks, bins and
    /// smart bins, and its colour space.
    pub fn summary(&self) -> String {
        let tracks: usize = self.sequences.iter().map(|s| s.tracks.len()).sum();
        let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = vec![count(self.sequences.len(), "sequence", "sequences"), count(tracks, "track", "tracks")];
        if !self.bins.is_empty() {
            parts.push(count(self.bins.len(), "bin", "bins"));
        }
        if !self.smart_bins.is_empty() {
            parts.push(count(self.smart_bins.len(), "smart bin", "smart bins"));
        }
        parts.push(format!("{:?} colour", self.colour.working_colorspace));
        parts.join(", ")
    }

    /// Kept in `dir` under its name, without starter content; one of that
    /// name is replaced.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        self.save_with_starter(dir, None)
    }

    /// Kept in `dir` under its name, with `starter`'s sequences and media
    /// when given (a folder then, a single file else); one of that name,
    /// either way, is replaced.
    pub fn save_with_starter(&self, dir: &Path, starter: Option<&StarterFiles>) -> Result<PathBuf, String> {
        self.validate()?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("{}.{TEMPLATE_EXTENSION}", file_stem(&self.name)));
        // Written beside and moved over, so a template is never half there.
        let partial = path.with_extension(format!("{TEMPLATE_EXTENSION}.partial"));
        remove_any(&partial);
        let mut template = self.clone();
        match starter {
            None => {
                template.starter = None;
                template.sequences.iter_mut().for_each(|s| s.starter = false);
                let json = serde_json::to_string_pretty(&template).map_err(|e| e.to_string())?;
                std::fs::write(&partial, json).map_err(|e| format!("{}: {e}", partial.display()))?;
            }
            Some(files) => {
                std::fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
                let mut pool = files.pool.clone();
                let mut content = template.starter.clone().unwrap_or_default();
                if files.package_media {
                    // Each file copied in under a name of its own.
                    let media_dir = partial.join("media");
                    std::fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;
                    let mut taken: Vec<String> = Vec::new();
                    for m in content.media.iter_mut() {
                        let base = m.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "media".into());
                        let mut name = base.clone();
                        let mut k = 2;
                        while taken.contains(&name) {
                            name = format!("{k} {base}");
                            k += 1;
                        }
                        std::fs::copy(&m.path, media_dir.join(&name)).map_err(|e| format!("{}: {e}", m.path.display()))?;
                        taken.push(name.clone());
                        m.packaged = Some(name);
                    }
                }
                let starter_dir = partial.join("starter");
                std::fs::create_dir_all(&starter_dir).map_err(|e| e.to_string())?;
                // Paths kept as they were: the copied files are found by
                // name when a project is made.
                pool.all_mut().for_each(|m| m.proxy_path = None);
                crate::save_project(&starter_dir, &files.sequences, &pool)?;
                template.starter = Some(content);
                let json = serde_json::to_string_pretty(&template).map_err(|e| e.to_string())?;
                std::fs::write(partial.join("template.json"), json).map_err(|e| e.to_string())?;
            }
        }
        remove_any(&path);
        std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// The template kept at `path` (a file, or a folder holding
    /// `template.json`), read and checked.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() { path.join("template.json") } else { path.to_path_buf() };
        let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let template: ProjectTemplate = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        template.validate()?;
        if template.starter.is_some() && !path.join("starter").is_dir() {
            return Err("its starter content is missing".into());
        }
        Ok(template)
    }

    /// A new project from the template kept at `path`, made at
    /// `project_dir`: [`Self::instantiate`], with the starter content's
    /// sequences in place of their empty selves, every id in them new, and
    /// its media: a copied file copied again into the project's
    /// `Template Media` folder, a linked one linked where it was (offline
    /// until relinked when it isn't there).
    pub fn instantiate_at(&self, path: &Path, project_dir: &Path) -> Result<FromTemplate, String> {
        let mut made = self.instantiate();
        let Some(content) = &self.starter else { return Ok(made) };
        let (mut starter, mut pool, ..) = crate::load_project(&path.join("starter"))?;
        // The copied files, into the project.
        let copied: Vec<(&StarterMedia, &String)> = content.media.iter().filter_map(|m| m.packaged.as_ref().map(|p| (m, p))).collect();
        if !copied.is_empty() {
            let into = project_dir.join("Template Media");
            std::fs::create_dir_all(&into).map_err(|e| format!("{}: {e}", into.display()))?;
            for (m, name) in copied {
                let to = into.join(name);
                std::fs::copy(path.join("media").join(name), &to).map_err(|e| format!("{name}: {e}"))?;
                for item in pool.all_mut().filter(|i| i.path == m.path) {
                    item.path = to.clone();
                    item.offline = false;
                }
            }
        }
        for item in pool.all_mut() {
            item.offline = !item.path.exists();
        }
        // Every sequence, track and clip id new, and what names them with it.
        let seq_ids: std::collections::HashMap<SequenceId, SequenceId> = starter.iter().map(|s| (s.id, SequenceId::new())).collect();
        for seq in &mut starter {
            let clips: std::collections::HashMap<klipp_core::ClipId, klipp_core::ClipId> = seq.all_clips().map(|c| (c.id, klipp_core::ClipId::new())).collect();
            klipp_timeline::flatten::reidentify(seq, &clips);
            seq.id = seq_ids[&seq.id];
            for t in &mut seq.tracks {
                t.id = klipp_core::TrackId::new();
                for c in &mut t.clips {
                    use klipp_timeline::ClipKind;
                    if let ClipKind::Sequence { sequence_id } | ClipKind::Precomposition { sequence_id } | ClipKind::TemplatePrecomposition { sequence_id, .. } = &mut c.kind {
                        if let Some(new) = seq_ids.get(sequence_id) {
                            *sequence_id = *new;
                        }
                    }
                }
            }
            seq.last_playhead = None;
        }
        // In the template's order: a kept sequence in place of its empty
        // self, any it opens after them.
        let mut sequences = Vec::new();
        let mut kept = starter.into_iter().map(Some).collect::<Vec<_>>();
        for (t, empty) in self.sequences.iter().zip(made.sequences) {
            let whole = t.starter.then(|| kept.iter_mut().find(|s| s.as_ref().is_some_and(|s| s.name == t.name)).and_then(Option::take)).flatten();
            sequences.push(whole.unwrap_or(empty));
        }
        sequences.extend(kept.into_iter().flatten());
        made.sequences = sequences;
        for item in pool.all() {
            made.pool.add(item.clone());
        }
        Ok(made)
    }

    /// What the template kept at `path` needs from outside itself: its
    /// media (copied in, or linked and there or not), its LUT, its titles'
    /// fonts and its plug-ins.
    pub fn dependencies(&self, path: &Path) -> Vec<Dependency> {
        let mut out = Vec::new();
        if let Some(lut) = &self.colour.global_lut_path {
            out.push(Dependency::Lut { path: lut.clone(), present: lut.exists() });
        }
        if let Some(content) = &self.starter {
            for m in &content.media {
                match &m.packaged {
                    Some(name) => out.push(Dependency::PackagedMedia(name.clone()).present_in(path)),
                    None => out.push(Dependency::LinkedMedia { path: m.path.clone(), present: m.path.exists() }),
                }
            }
            out.extend(content.fonts.iter().cloned().map(Dependency::Font));
            out.extend(content.plugins.iter().cloned().map(Dependency::Plugin));
        }
        out
    }
}

impl Dependency {
    /// A copied file missing from the template's `media` folder is, for
    /// what it means to a new project, a linked file that isn't there.
    fn present_in(self, template: &Path) -> Self {
        match self {
            Dependency::PackagedMedia(name) if !template.join("media").join(&name).is_file() => Dependency::LinkedMedia { path: template.join("media").join(name), present: false },
            other => other,
        }
    }

    /// Whether it is missing: a file not there.
    pub fn missing(&self) -> bool {
        matches!(self, Dependency::LinkedMedia { present: false, .. } | Dependency::Lut { present: false, .. })
    }
}

/// `name` as a file name: the characters a file system refuses replaced.
fn file_stem(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect()
}

/// `path` gone, a file or a folder.
fn remove_any(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// The template at `path` renamed `name` (#618, part 3): its name inside
/// and its file's. Refused when another template is already called so.
pub fn rename_template(path: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the template a name".into());
    }
    let mut template = ProjectTemplate::load(path)?;
    let dir = path.parent().ok_or("the template has no folder")?;
    let to = dir.join(format!("{}.{TEMPLATE_EXTENSION}", file_stem(name)));
    if to != path && to.exists() {
        return Err(format!("a template called \"{name}\" is there already"));
    }
    template.name = name.to_string();
    let json = serde_json::to_string_pretty(&template).map_err(|e| e.to_string())?;
    let file = if path.is_dir() { path.join("template.json") } else { path.to_path_buf() };
    std::fs::write(&file, json).map_err(|e| e.to_string())?;
    if to != path {
        std::fs::rename(path, &to).map_err(|e| e.to_string())?;
    }
    Ok(to)
}

/// The template at `path` as one zip at `to` (#618, part 3): its
/// `template.json`, starter project and copied media, for another
/// machine. Linked media stays where it is and is relinked there.
pub fn export_template(path: &Path, to: &Path) -> Result<(), String> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    ProjectTemplate::load(path)?;
    let file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(rel) = p.strip_prefix(path) {
                    files.push((p.clone(), rel.to_string_lossy().replace('\\', "/")));
                }
            }
        }
    } else {
        files.push((path.to_path_buf(), "template.json".into()));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    for (abs, rel) in files {
        zip.start_file(rel.as_str(), options).map_err(|e| e.to_string())?;
        let mut src = std::fs::File::open(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
        std::io::copy(&mut src, &mut zip).map_err(|e| e.to_string())?;
    }
    zip.flush().map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// The template in the zip at `from` added to the templates in `dir`
/// (#618, part 3), checked before it is kept: one that does not read is
/// refused and nothing is added. A template of the same name is replaced.
pub fn import_template(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a template archive: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let unpacked = dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            // Only paths inside the template: no absolute or `..` names.
            let Some(rel) = entry.enclosed_name() else { return Err(format!("{} leads outside the template", entry.name())) };
            let to = unpacked.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&to).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
        let template = ProjectTemplate::load(&unpacked)?;
        let path = dir.join(format!("{}.{TEMPLATE_EXTENSION}", file_stem(&template.name)));
        remove_any(&path);
        std::fs::rename(&unpacked, &path).map_err(|e| e.to_string())?;
        Ok(path)
    })();
    remove_any(&unpacked);
    result
}

/// Where the user's project templates are kept.
pub fn templates_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("klipp").join("project_templates")
}

/// Every template in `dir` by name, each read or the reason it isn't; an
/// unreadable one hides no other. An absent folder holds none.
pub fn list_templates(dir: &Path) -> Vec<(PathBuf, Result<ProjectTemplate, String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(PathBuf, Result<ProjectTemplate, String>)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == TEMPLATE_EXTENSION))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .map(|p| {
            let t = ProjectTemplate::load(&p);
            (p, t)
        })
        .collect();
    let key = |e: &(PathBuf, Result<ProjectTemplate, String>)| match &e.1 {
        Ok(t) => t.name.to_lowercase(),
        Err(_) => e.0.to_string_lossy().to_lowercase(),
    };
    out.sort_by_key(key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Timecode;

    /// A project with two sequences (one with a set format, one taking it
    /// from the first clip), named tracks with settings and clips, nested
    /// bins with media in them, and a smart bin.
    fn project() -> (Vec<Sequence>, MediaPool) {
        let mut a = Sequence::new_default();
        a.name = "Episode".into();
        a.fps = Fps::new(25, 1);
        a.resolution = Resolution { width: 3840, height: 2160 };
        a.first_clip_autoset = FirstClipAutoSet { resolution: false, fps: false };
        a.tracks = vec![Track::new("Camera A", TrackKind::Video), Track::new("B-roll", TrackKind::Video), Track::new("Dialogue", TrackKind::Audio), Track::new("Music", TrackKind::Audio)];
        a.tracks[0].magnetic_role = TrackRole::Primary;
        a.tracks[2].volume = 0.5;
        a.tracks[3].muted = true;
        a.tracks[1].colour_label = 3;
        a.tracks[0].append_clip(klipp_core::MediaId::new(), Timecode::from_secs_f64(4.0, 25));
        let mut b = Sequence::new_default();
        b.name = "Vertical".into();
        b.first_clip_autoset = FirstClipAutoSet { resolution: true, fps: true };
        let mut pool = MediaPool::new();
        let footage = Bin { id: BinId::new(), name: "Footage".into(), parent_id: None };
        let day1 = Bin { id: BinId::new(), name: "Day 1".into(), parent_id: Some(footage.id) };
        let gfx = Bin { id: BinId::new(), name: "Graphics".into(), parent_id: None };
        // A child listed before its parent.
        pool.bins = vec![day1.clone(), footage, gfx];
        pool.smart_bins = vec![SmartBin::new("s1".into(), "Best takes")];
        (vec![a, b], pool)
    }

    fn colour() -> TemplateColour {
        TemplateColour { working_colorspace: WorkingColorspace::default(), grade_encoding: GradeEncoding::default(), global_lut_path: Some("/looks/show.cube".into()), global_lut_opacity: 0.6, global_lut_enabled: true }
    }

    /// The template keeps the structure and nothing else, and a project
    /// made from it has that structure with every id new.
    #[test]
    fn a_template_keeps_the_structure_and_starts_a_new_project() {
        let (sequences, pool) = project();
        let t = ProjectTemplate::capture(" Series ", "Weekly", &ProjectSetup { sequences: &sequences, pool: &pool, colour: colour() });
        assert_eq!((t.name.as_str(), t.description.as_str()), ("Series", "Weekly"));
        t.validate().unwrap();
        // Through a file, as it is kept.
        let dir = std::env::temp_dir().join(format!("klipp-template-{}", uuid::Uuid::new_v4()));
        let path = t.save(&dir).unwrap();
        let back = ProjectTemplate::load(&path).unwrap();
        assert_eq!(back, t);
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("clips") && !json.contains("markers") && !json.contains("media"), "no edit, no media: {json}");

        let made = back.instantiate();
        assert_eq!(made.sequences.len(), 2);
        let (ep, vert) = (&made.sequences[0], &made.sequences[1]);
        assert_eq!((ep.name.as_str(), ep.fps, ep.resolution.width, ep.first_clip_autoset.fps), ("Episode", Fps::new(25, 1), 3840, false), "a set format stays set");
        assert!(vert.first_clip_autoset.fps && vert.first_clip_autoset.resolution, "an automatic format stays automatic");
        let names: Vec<&str> = ep.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Camera A", "B-roll", "Dialogue", "Music"]);
        assert_eq!((ep.tracks[0].magnetic_role.clone(), ep.tracks[2].volume, ep.tracks[3].muted, ep.tracks[1].colour_label), (TrackRole::Primary, 0.5, true, 3));
        assert!(ep.tracks.iter().all(|t| t.clips.is_empty()), "no clips");
        // Every id new.
        assert!(made.sequences.iter().zip(&sequences).all(|(m, s)| m.id != s.id));
        let old_tracks: Vec<_> = sequences.iter().flat_map(|s| s.tracks.iter().map(|t| t.id)).collect();
        assert!(made.sequences.iter().flat_map(|s| &s.tracks).all(|t| !old_tracks.contains(&t.id)));
        // The bins, nested as they were, empty and new.
        let bin = |name: &str| made.pool.bins.iter().find(|b| b.name == name).unwrap().clone();
        assert_eq!(bin("Day 1").parent_id, Some(bin("Footage").id));
        assert_eq!(bin("Graphics").parent_id, None);
        assert!(made.pool.bins.iter().all(|b| !pool.bins.iter().any(|o| o.id == b.id)));
        assert_eq!(made.pool.all().count(), 0, "no media");
        assert_eq!(made.pool.smart_bins.len(), 1);
        assert_eq!((made.pool.smart_bins[0].name.as_str(), made.pool.smart_bins[0].rules.clone()), ("Best takes", pool.smart_bins[0].rules.clone()));
        assert_ne!(made.pool.smart_bins[0].id, "s1");
        assert_eq!(made.colour, colour());
        assert_eq!(back.summary(), format!("2 sequences, 6 tracks, 3 bins, 1 smart bin, {:?} colour", WorkingColorspace::default()));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A template that doesn't read, or reads but can't start a project,
    /// is listed with why, beside the ones that do.
    #[test]
    fn a_bad_template_hides_no_other() {
        let (sequences, pool) = project();
        let dir = std::env::temp_dir().join(format!("klipp-templates-{}", uuid::Uuid::new_v4()));
        ProjectTemplate::capture("Good", "", &ProjectSetup { sequences: &sequences, pool: &pool, colour: colour() }).save(&dir).unwrap();
        std::fs::write(dir.join("broken.klipptemplate"), "{ not json").unwrap();
        let mut future = ProjectTemplate::capture("Future", "", &ProjectSetup { sequences: &sequences, pool: &pool, colour: colour() });
        future.format = TEMPLATE_FORMAT + 1;
        std::fs::write(dir.join("future.klipptemplate"), serde_json::to_string(&future).unwrap()).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a template").unwrap();
        let listed = list_templates(&dir);
        assert_eq!(listed.len(), 3, "{listed:?}");
        assert!(listed.iter().any(|(_, t)| t.as_ref().is_ok_and(|t| t.name == "Good")));
        let errors: Vec<&String> = listed.iter().filter_map(|(_, t)| t.as_ref().err()).collect();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|e| e.contains("format")), "{errors:?}");
        assert!(list_templates(&dir.join("absent")).is_empty());
        // And a template without a sequence, or a size, is refused.
        let mut empty = future.clone();
        empty.format = TEMPLATE_FORMAT;
        empty.sequences.clear();
        assert!(empty.validate().unwrap_err().contains("no sequence"));
        let mut sizeless = future;
        sizeless.format = TEMPLATE_FORMAT;
        sizeless.sequences[0].width = 0;
        assert!(sizeless.validate().unwrap_err().contains("no size"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A project whose "Episode" sequence holds two clips of real files
    /// (one gone from disk later) and a title, opening a "Scene" compound,
    /// beside an empty "Spare" sequence.
    fn with_content(root: &Path) -> (Vec<Sequence>, MediaPool, [PathBuf; 2]) {
        std::fs::create_dir_all(root).unwrap();
        let files = [root.join("a.mov"), root.join("b.mov")];
        for (f, bytes) in files.iter().zip([b"first".as_slice(), b"second".as_slice()]) {
            std::fs::write(f, bytes).unwrap();
        }
        let mut pool = MediaPool::new();
        let ids: Vec<klipp_core::MediaId> = files.iter().map(|f| pool.add(klipp_media::MediaItem::synthetic_for_test(f.clone(), None))).collect();
        let tc = |s: f64| Timecode::from_secs_f64(s, 24);
        let mut scene = Sequence::new_default();
        scene.name = "Scene".into();
        scene.tracks[0].clips = vec![klipp_timeline::Clip::media(ids[1], tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        let mut ep = Sequence::new_default();
        ep.name = "Episode".into();
        let title = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hi".into(), font_name: "Bebas Neue".into(), ..Default::default() });
        ep.tracks[0].clips = vec![
            klipp_timeline::Clip::media(ids[0], tc(0.0), tc(2.0), tc(0.0), tc(2.0)),
            klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Sequence { sequence_id: scene.id }, ..klipp_timeline::Clip::gap(tc(2.0), tc(4.0)) },
            klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(title), ..klipp_timeline::Clip::gap(tc(4.0), tc(5.0)) },
        ];
        let mut spare = Sequence::new_default();
        spare.name = "Spare".into();
        (vec![ep, spare, scene], pool, files)
    }

    /// #618 part 2: chosen sequences are kept whole with what they open,
    /// their media copied in or linked; a new project gets them with every
    /// id new and the copied files in its own folder, a linked file that
    /// is gone shown missing before and offline after. Part 3: the template
    /// goes to another machine as a zip and comes back the same, and can be
    /// renamed.
    #[test]
    fn starter_content_is_kept_and_travels() {
        let root = std::env::temp_dir().join(format!("klipp-starter-{}", uuid::Uuid::new_v4()));
        let (sequences, pool, files) = with_content(&root.join("footage"));
        let setup = ProjectSetup { sequences: &sequences, pool: &pool, colour: colour() };
        let templates = root.join("templates");
        for package in [true, false] {
            let choice = StarterChoice { sequences: vec![sequences[0].id], package_media: package };
            let (t, files_in) = ProjectTemplate::capture_with_starter(if package { "Packed" } else { "Linked" }, "", &setup, &choice);
            let content = t.starter.clone().expect("starter content");
            assert_eq!(content.sequences, ["Episode", "Scene"], "the compound's sequence comes along");
            assert_eq!(content.media.len(), 2);
            assert_eq!(content.fonts, ["Bebas Neue"]);
            assert!(t.sequences.iter().map(|s| (s.name.as_str(), s.starter)).eq([("Episode", true), ("Spare", false), ("Scene", true)]));
            let path = t.save_with_starter(&templates, files_in.as_ref()).unwrap();
            assert!(path.is_dir() && path.join("starter").is_dir());
            assert_eq!(path.join("media").is_dir(), package, "copied only when asked");
        }
        let packed = templates.join("Packed.klipptemplate");
        let linked = templates.join("Linked.klipptemplate");
        // A linked file gone: missing before the project is made.
        std::fs::remove_file(&files[1]).unwrap();
        let deps = ProjectTemplate::load(&linked).unwrap().dependencies(&linked);
        assert!(deps.iter().any(|d| matches!(d, Dependency::LinkedMedia { path, present: false } if *path == files[1])), "{deps:?}");
        assert!(deps.contains(&Dependency::Font("Bebas Neue".into())));
        let deps = ProjectTemplate::load(&packed).unwrap().dependencies(&packed);
        assert!(deps.iter().filter(|d| matches!(d, Dependency::PackagedMedia(_))).count() == 2 && !deps.iter().any(|d| matches!(d, Dependency::LinkedMedia { .. })), "{deps:?}");
        // The template's LUT, not on this machine, is missing.
        assert!(deps.iter().any(|d| d.missing() && matches!(d, Dependency::Lut { .. })));

        // A project from each.
        for (path, packaged) in [(&packed, true), (&linked, false)] {
            let project = root.join(format!("project-{packaged}"));
            let t = ProjectTemplate::load(path).unwrap();
            let made = t.instantiate_at(path, &project).unwrap();
            let names: Vec<&str> = made.sequences.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["Episode", "Spare", "Scene"]);
            let ep = &made.sequences[0];
            assert_eq!(ep.tracks[0].clips.len(), 3, "kept whole");
            assert!(made.sequences[1].tracks.iter().all(|t| t.clips.is_empty()), "the rest is structure");
            // Every id new, the compound opening the new Scene.
            let old_clips: Vec<_> = sequences.iter().flat_map(|s| s.all_clips().map(|c| c.id)).collect();
            assert!(made.sequences.iter().flat_map(|s| s.all_clips()).all(|c| !old_clips.contains(&c.id)));
            assert!(made.sequences.iter().all(|m| sequences.iter().all(|s| s.id != m.id)));
            let klipp_timeline::ClipKind::Sequence { sequence_id } = ep.tracks[0].clips[1].kind else { panic!("the compound") };
            assert_eq!(sequence_id, made.sequences[2].id);
            let item = |name: &str| made.pool.all().find(|m| m.filename == name).unwrap().clone();
            if packaged {
                assert_eq!(item("a.mov").path, project.join("Template Media").join("a.mov"));
                assert_eq!(std::fs::read(item("b.mov").path).unwrap(), b"second", "copied in before the original went");
                assert!(made.pool.all().all(|m| !m.offline));
            } else {
                assert_eq!(item("a.mov").path, files[0]);
                assert!(!item("a.mov").offline && item("b.mov").offline, "the gone file is offline, to relink");
            }
        }

        // Exported and imported elsewhere: the same template, media and all.
        let zip = root.join("Packed.zip");
        export_template(&packed, &zip).unwrap();
        let other = root.join("elsewhere");
        let imported = import_template(&zip, &other).unwrap();
        assert_eq!(ProjectTemplate::load(&imported).unwrap(), ProjectTemplate::load(&packed).unwrap());
        assert_eq!(std::fs::read(imported.join("media").join("b.mov")).unwrap(), b"second");
        // A structure-only one too.
        let plain = ProjectTemplate::capture("Plain", "", &setup).save(&templates).unwrap();
        export_template(&plain, &root.join("Plain.zip")).unwrap();
        let back = import_template(&root.join("Plain.zip"), &other).unwrap();
        assert_eq!(ProjectTemplate::load(&back).unwrap().name, "Plain");
        // Not a template: refused, nothing added.
        std::fs::write(root.join("junk.zip"), b"not a zip").unwrap();
        assert!(import_template(&root.join("junk.zip"), &other).is_err());
        assert_eq!(list_templates(&other).len(), 2);

        // Renamed: in its file and its name; not over another.
        let renamed = rename_template(&imported, "Packed Show").unwrap();
        assert!(!imported.exists() && renamed.ends_with("Packed Show.klipptemplate"));
        assert_eq!(ProjectTemplate::load(&renamed).unwrap().name, "Packed Show");
        assert!(rename_template(&renamed, "Plain").unwrap_err().contains("already"));
        let _ = std::fs::remove_dir_all(root);
    }
}
