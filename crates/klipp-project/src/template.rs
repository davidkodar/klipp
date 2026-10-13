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

use std::path::{Path, PathBuf};

use klipp_core::{BinId, Fps, GradeEncoding, Resolution, SequenceId, SmartBin, WorkingColorspace};
use klipp_media::{Bin, MediaPool};
use klipp_timeline::{CaptionStyle, FirstClipAutoSet, Sequence, Track, TrackKind, TrackRole};
use serde::{Deserialize, Serialize};

/// The template format this build writes and reads.
pub const TEMPLATE_FORMAT: u32 = 1;

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
        }
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

    /// Kept in `dir` under its name; one of that name is replaced.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        self.validate()?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stem: String = self.name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
        let path = dir.join(format!("{stem}.{TEMPLATE_EXTENSION}"));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Written beside and moved over, so a template is never half there.
        let partial = path.with_extension(format!("{TEMPLATE_EXTENSION}.partial"));
        std::fs::write(&partial, json).map_err(|e| format!("{}: {e}", partial.display()))?;
        std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// The template kept at `path`, read and checked.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let template: ProjectTemplate = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        template.validate()?;
        Ok(template)
    }
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
}
